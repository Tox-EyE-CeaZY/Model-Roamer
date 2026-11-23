//! The HTTP server: the UI, the API, live events over a web socket, the agents' tool bridge and artifacts.
//! The desktop app talks to its own copy over loopback; other devices talk to a server the same way, so a laptop,
//! a phone's browser and the windows on the server itself all see the same thing.

use crate::{core::CoreRef, util};
use axum::{
    body::Body,
    extract::{Request, State},
    http::{header, HeaderMap, HeaderValue, Method, StatusCode},
    middleware::Next,
    response::{IntoResponse, Response},
    Json,
};
use rust_embed::RustEmbed;
use serde_json::{json, Value};
use std::net::SocketAddr;

#[derive(RustEmbed)]
#[folder = "ui/"]
pub struct Assets;

pub struct ApiErr(pub StatusCode, pub String);

impl IntoResponse for ApiErr {
    fn into_response(self) -> Response {
        (self.0, Json(json!({ "error": self.1 }))).into_response()
    }
}

impl<E: Into<anyhow::Error>> From<E> for ApiErr {
    fn from(e: E) -> Self {
        let e: anyhow::Error = e.into();
        let msg = format!("{e:#}");
        let code = if msg.starts_with("No ") { StatusCode::NOT_FOUND } else { StatusCode::BAD_REQUEST };
        ApiErr(code, msg)
    }
}

pub type R = Result<Json<Value>, ApiErr>;

pub fn bad(msg: impl Into<String>) -> ApiErr {
    ApiErr(StatusCode::BAD_REQUEST, msg.into())
}

/// Who is calling.
#[derive(Clone, Debug)]
pub enum Who {
    /// This computer's own windows (the local token).
    Local,
    /// A paired device.
    Device(String),
}

impl Who {
    pub fn is_local(&self) -> bool {
        matches!(self, Who::Local)
    }
}

pub const COOKIE: &str = "mr_auth";

fn cookie_token(h: &HeaderMap) -> Option<String> {
    for v in h.get_all(header::COOKIE) {
        for part in v.to_str().ok()?.split(';') {
            if let Some(t) = part.trim().strip_prefix(&format!("{COOKIE}=")) {
                return Some(t.to_string());
            }
        }
    }
    None
}

fn bearer(h: &HeaderMap) -> Option<String> {
    h.get(header::AUTHORIZATION)?.to_str().ok()?.strip_prefix("Bearer ").map(|s| s.trim().to_string())
}

/// Check a token: the local one, or a paired device's.
pub fn identify(core: &CoreRef, token: &str, ip: &str) -> Option<Who> {
    if token.is_empty() {
        return None;
    }
    if util::same(token, &core.local_token) {
        return Some(Who::Local);
    }
    let hash = util::sha256(token);
    let d = core.db.one("SELECT id FROM devices WHERE token_hash = ?1 AND revoked = 0", &[&hash]).ok()??;
    let id = d["id"].as_str()?.to_string();
    let now = util::now_ms();
    let _ = core.db.exec("UPDATE devices SET last_seen = ?1, last_ip = ?2 WHERE id = ?3", &[&now, &ip, &id]);
    Some(Who::Device(id))
}

fn client_ip(req: &Request) -> String {
    req.extensions().get::<axum::extract::ConnectInfo<SocketAddr>>().map(|c| c.0.ip().to_string()).unwrap_or_default()
}

/// Every /api route except the public ones needs a token: a Bearer header, or the cookie plus the X-Roamer header
/// (which a page on another site can't add, so cookies alone can't be used against us).
pub async fn auth(State(core): State<CoreRef>, mut req: Request, next: Next) -> Response {
    let path = req.uri().path().to_string();
    let public = matches!(path.as_str(), "/api/hello" | "/api/pair" | "/api/mcp") || !path.starts_with("/api/");
    if public {
        return next.run(req).await;
    }
    let ip = client_ip(&req);
    let h = req.headers();
    let (tok, via_cookie) = match bearer(h) {
        Some(t) => (t, false),
        None => match cookie_token(h) {
            Some(t) => (t, true),
            None => (String::new(), false),
        },
    };
    let Some(who) = identify(&core, &tok, &ip) else {
        return ApiErr(StatusCode::UNAUTHORIZED, "Sign in to this Model Roamer first (pair this device).".into()).into_response();
    };
    let is_ws = path == "/api/ws";
    if via_cookie && !is_ws && req.method() != Method::GET && req.method() != Method::HEAD && h.get("x-roamer").is_none() {
        return ApiErr(StatusCode::FORBIDDEN, "Missing X-Roamer header".into()).into_response();
    }
    if is_ws {
        // a web socket can be opened from any page; only our own pages may
        let host = h.get(header::HOST).and_then(|v| v.to_str().ok()).unwrap_or("");
        let origin = h.get(header::ORIGIN).and_then(|v| v.to_str().ok()).unwrap_or("");
        if !origin.is_empty() && !origin.ends_with(&format!("//{host}")) {
            return ApiErr(StatusCode::FORBIDDEN, "Cross-site socket refused".into()).into_response();
        }
    }
    req.extensions_mut().insert(who);
    next.run(req).await
}

/// Small security headers on everything we serve.
pub async fn headers(req: Request, next: Next) -> Response {
    let is_artifact = req.uri().path().starts_with("/a/");
    let mut r = next.run(req).await;
    let h = r.headers_mut();
    h.insert("x-content-type-options", HeaderValue::from_static("nosniff"));
    h.insert("referrer-policy", HeaderValue::from_static("no-referrer"));
    if is_artifact {
        // artifacts run in their own sandbox: they can script themselves but can't reach the app
        h.insert("content-security-policy", HeaderValue::from_static("sandbox allow-scripts allow-popups allow-forms allow-modals allow-downloads allow-popups-to-escape-sandbox"));
    } else {
        h.insert("x-frame-options", HeaderValue::from_static("SAMEORIGIN"));
    }
    r
}

pub fn asset(path: &str) -> Response {
    let path = if path.is_empty() || path == "/" { "index.html" } else { path.trim_start_matches('/') };
    match Assets::get(path) {
        Some(f) => {
            let mime = mime_guess::from_path(path).first_or_octet_stream();
            let cache = if path == "index.html" { "no-cache" } else { "public, max-age=300" };
            Response::builder().header(header::CONTENT_TYPE, mime.as_ref()).header(header::CACHE_CONTROL, cache).body(Body::from(f.data.into_owned())).unwrap()
        }
        None => StatusCode::NOT_FOUND.into_response(),
    }
}

/// Open (or reopen) the listener other devices use, from the settings (command-line overrides win). Turning
/// sharing off, or changing its address, takes effect at once: no restart.
pub async fn apply_sharing(core: &CoreRef, over: Option<(&str, u16, Option<(String, String)>)>) -> anyhow::Result<Option<String>> {
    let st = core.settings();
    stop_sharing(core);
    if !core.headless && st["server"]["share"] != true {
        return Ok(None);
    }
    let (bind, port, tls) = match over {
        Some((b, p, t)) => (b.to_string(), p, t),
        None => {
            let cert = st["server"]["tls_cert"].as_str().unwrap_or("").to_string();
            let key = st["server"]["tls_key"].as_str().unwrap_or("").to_string();
            (st["server"]["bind"].as_str().unwrap_or("0.0.0.0").to_string(), st["server"]["port"].as_u64().unwrap_or(4630) as u16, (!cert.is_empty() && !key.is_empty()).then_some((cert, key)))
        }
    };
    let addr: SocketAddr = format!("{bind}:{port}").parse().or_else(|_| format!("[{bind}]:{port}").parse()).map_err(|_| anyhow::anyhow!("'{bind}' isn't an address to listen on"))?;
    let has_tls = tls.is_some();
    let app = crate::api::router(core.clone()).into_make_service_with_connect_info::<SocketAddr>();
    let serve = match tls {
        Some((cert, key)) => {
            let _ = rustls::crypto::ring::default_provider().install_default();
            let cfg = axum_server::tls_rustls::RustlsConfig::from_pem_file(&cert, &key).await.map_err(|e| anyhow::anyhow!("TLS certificate/key: {e}"))?;
            let l = std::net::TcpListener::bind(addr).map_err(|e| anyhow::anyhow!("Can't listen on {addr}: {e}"))?;
            l.set_nonblocking(true)?;
            tracing::info!("serving https on {addr}");
            tokio::spawn(async move {
                if let Err(e) = axum_server::from_tcp_rustls(l, cfg).serve(app).await {
                    tracing::error!("https listener stopped: {e}");
                }
            })
        }
        None => {
            let l = tokio::net::TcpListener::bind(addr).await.map_err(|e| anyhow::anyhow!("Can't listen on {addr}: {e}"))?;
            tracing::info!("serving http on {addr}");
            tokio::spawn(async move {
                if let Err(e) = axum::serve(l, app).await {
                    tracing::error!("listener stopped: {e}");
                }
            })
        }
    };
    let announce = tokio::spawn(crate::net::announce(core.clone(), port, has_tls));
    let at = format!("{}://{addr}", if has_tls { "https" } else { "http" });
    *core.sharing.lock().unwrap_or_else(|p| p.into_inner()) = Some((at.clone(), vec![serve, announce]));
    Ok(Some(at))
}

pub fn stop_sharing(core: &CoreRef) {
    if let Some((at, tasks)) = core.sharing.lock().unwrap_or_else(|p| p.into_inner()).take() {
        tracing::info!("stopped sharing on {at}");
        for t in tasks {
            t.abort();
        }
    }
}

/// Bind a loopback listener (preferring `port`, else any free one) and serve on it in the background.
pub async fn serve_loopback(core: CoreRef, port: u16) -> anyhow::Result<u16> {
    let l = match tokio::net::TcpListener::bind(("127.0.0.1", port)).await {
        Ok(l) => l,
        Err(_) => tokio::net::TcpListener::bind(("127.0.0.1", 0)).await?,
    };
    let actual = l.local_addr()?.port();
    let app = crate::api::router(core.clone());
    tokio::spawn(async move {
        if let Err(e) = axum::serve(l, app.into_make_service_with_connect_info::<SocketAddr>()).await {
            tracing::error!("loopback server stopped: {e}");
        }
    });
    Ok(actual)
}
