//! Pages served on the machine Model Roamer runs on (a dev server or a preview an agent started on this box) that
//! another device can't reach as "localhost": the server fetches them and serves them back under /fwd/<sig>/<port>/,
//! so the app's in-app browser (or any browser) on that device can open them. The link carries a signature, like an
//! artifact's, and the pages run sandboxed (see server::headers) so they can't touch the app.

use crate::{core::CoreRef, server::bad, util};
use axum::{
    body::Body,
    extract::{Path, Request, State},
    http::{header, HeaderMap, Method, StatusCode},
    response::{IntoResponse, Redirect, Response},
};
use std::{collections::HashMap, sync::OnceLock};

pub fn sig(core: &CoreRef, port: u16) -> String {
    util::sha256(&format!("{}:fwd:{port}", core.local_token))[..24].to_string()
}

/// Ports that are Model Roamer's own, or privileged: never forwarded.
fn blocked(core: &CoreRef, port: u16) -> bool {
    let own = core.settings()["server"]["port"].as_u64().unwrap_or(4630) as u16;
    port < 1024 || port == own || matches!(port, 4625 | 4630 | 4631) || core.base_url().rsplit(':').next().and_then(|p| p.trim_end_matches('/').parse::<u16>().ok()) == Some(port)
}

/// "http://localhost:4173/x?y" → ("/fwd/<sig>/4173/x?y", 4173): None when the address isn't one of this machine's own.
pub fn rewrite(core: &CoreRef, url: &str) -> Result<(String, u16), crate::server::ApiErr> {
    let u = reqwest::Url::parse(url.trim()).map_err(|_| bad("That isn't a web address."))?;
    if !matches!(u.scheme(), "http" | "https") || !matches!(u.host_str(), Some("localhost" | "127.0.0.1" | "[::1]" | "::1" | "0.0.0.0")) {
        return Err(bad("Only pages on this machine (localhost) are routed."));
    }
    let port = u.port_or_known_default().ok_or_else(|| bad("No port in that address."))?;
    if blocked(core, port) {
        return Err(bad("That port is Model Roamer's own, so it isn't forwarded."));
    }
    let mut p = format!("/fwd/{}/{port}{}", sig(core, port), u.path());
    if let Some(q) = u.query() {
        p.push('?');
        p.push_str(q);
    }
    Ok((p, port))
}

fn client() -> &'static reqwest::Client {
    static C: OnceLock<reqwest::Client> = OnceLock::new();
    C.get_or_init(|| reqwest::Client::builder().redirect(reqwest::redirect::Policy::none()).timeout(std::time::Duration::from_secs(60)).build().expect("http client"))
}

pub async fn serve(State(core): State<CoreRef>, Path(p): Path<HashMap<String, String>>, req: Request) -> Response {
    let port: u16 = p.get("port").and_then(|x| x.parse().ok()).unwrap_or(0);
    if port == 0 || blocked(&core, port) || !util::same(p.get("sig").map(String::as_str).unwrap_or(""), &sig(&core, port)) {
        return StatusCode::NOT_FOUND.into_response();
    }
    let prefix = format!("/fwd/{}/{port}", sig(&core, port));
    let rest = p.get("rest").cloned().unwrap_or_default();
    if !req.uri().path().ends_with('/') && rest.is_empty() {
        // /fwd/sig/port → /fwd/sig/port/ so relative links resolve
        let q = req.uri().query().map(|q| format!("?{q}")).unwrap_or_default();
        return Redirect::temporary(&format!("{prefix}/{q}")).into_response();
    }
    let target = format!("http://127.0.0.1:{port}/{rest}{}", req.uri().query().map(|q| format!("?{q}")).unwrap_or_default());
    let method = req.method().clone();
    let mut out = client().request(method.clone(), &target);
    for (k, v) in req.headers() {
        if matches!(k.as_str(), "accept" | "accept-language" | "content-type" | "range" | "user-agent" | "if-none-match" | "if-modified-since") {
            out = out.header(k, v);
        }
    }
    out = out.header(header::HOST, format!("127.0.0.1:{port}")).header(header::ACCEPT_ENCODING, "identity");
    if method != Method::GET && method != Method::HEAD {
        match axum::body::to_bytes(req.into_body(), 20_000_000).await {
            Ok(b) => out = out.body(b),
            Err(_) => return StatusCode::PAYLOAD_TOO_LARGE.into_response(),
        }
    }
    let r = match out.send().await {
        Ok(r) => r,
        Err(e) => return (StatusCode::BAD_GATEWAY, format!("Nothing answered on port {port} of this machine ({e}). Is the page still being served?")).into_response(),
    };
    let status = StatusCode::from_u16(r.status().as_u16()).unwrap_or(StatusCode::BAD_GATEWAY);
    let mut headers = HeaderMap::new();
    for (k, v) in r.headers() {
        match k.as_str() {
            "content-type" | "cache-control" | "etag" | "last-modified" | "content-range" | "accept-ranges" | "content-disposition" => {
                headers.insert(k.clone(), v.clone());
            }
            "location" => {
                // a redirect to another page of the same server stays inside the prefix
                let loc = v.to_str().unwrap_or("");
                let loc = loc.strip_prefix(&format!("http://127.0.0.1:{port}")).or_else(|| loc.strip_prefix(&format!("http://localhost:{port}"))).unwrap_or(loc);
                let loc = if loc.starts_with('/') && !loc.starts_with("//") { format!("{prefix}{loc}") } else { loc.to_string() };
                if let Ok(v) = loc.parse() {
                    headers.insert(header::LOCATION, v);
                }
            }
            _ => {}
        }
    }
    let ct = r.headers().get(header::CONTENT_TYPE).and_then(|v| v.to_str().ok()).unwrap_or("").to_lowercase();
    let is_html = ct.starts_with("text/html");
    let is_css = ct.starts_with("text/css");
    if is_html || is_css {
        let bytes = match r.bytes().await {
            Ok(b) => b,
            Err(e) => return (StatusCode::BAD_GATEWAY, e.to_string()).into_response(),
        };
        let text = String::from_utf8_lossy(&bytes).into_owned();
        let body = if is_html { rewrite_html(&text, &prefix) } else { rewrite_css(&text, &prefix) };
        headers.remove(header::CONTENT_LENGTH);
        headers.remove(header::CONTENT_RANGE);
        return (status, headers, Body::from(body)).into_response();
    }
    (status, headers, Body::from_stream(r.bytes_stream())).into_response()
}

/// Root-absolute addresses ("/app.js") mean the page's own server, not ours: point them back through the prefix.
fn rewrite_css(css: &str, prefix: &str) -> String {
    static RE: OnceLock<regex::Regex> = OnceLock::new();
    let re = RE.get_or_init(|| regex::Regex::new(r#"(url\(\s*["']?|@import\s+["'])/([^/])"#).unwrap());
    re.replace_all(css, |c: &regex::Captures| format!("{}{prefix}/{}", &c[1], &c[2])).into_owned()
}

fn rewrite_html(html: &str, prefix: &str) -> String {
    static RE: OnceLock<regex::Regex> = OnceLock::new();
    let re = RE.get_or_init(|| regex::Regex::new(r#"(?i)(\s(?:src|href|action|poster|data-src)\s*=\s*["'])/([^/])"#).unwrap());
    let mut out = re.replace_all(html, |c: &regex::Captures| format!("{}{prefix}/{}", &c[1], &c[2])).into_owned();
    out = rewrite_css(&out, prefix);
    // scripts that fetch "/api/x" go through the prefix too
    let shim = format!(
        "<script>(function(){{var p='{prefix}';function f(u){{return typeof u==='string'&&u[0]==='/'&&u[1]!=='/'&&u.indexOf(p)!==0?p+u:u}}var F=window.fetch;if(F)window.fetch=function(u,o){{return F.call(this,f(u),o)}};var X=XMLHttpRequest.prototype.open;XMLHttpRequest.prototype.open=function(m,u){{arguments[1]=f(u);return X.apply(this,arguments)}}}})();</script>"
    );
    match out.to_lowercase().find("<head") {
        Some(i) => match out[i..].find('>') {
            Some(j) => {
                out.insert_str(i + j + 1, &shim);
                out
            }
            None => format!("{shim}{out}"),
        },
        None => format!("{shim}{out}"),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn keeps_root_links_inside_the_prefix() {
        let h = rewrite_html(r#"<head><link href="/s.css"></head><img src="/a/b.png"><a href="//cdn.x/y"><a href="rel.html">"#, "/fwd/s/4173");
        assert!(h.contains(r#"href="/fwd/s/4173/s.css""#) && h.contains(r#"src="/fwd/s/4173/a/b.png""#));
        assert!(h.contains(r#"href="//cdn.x/y""#) && h.contains(r#"href="rel.html""#));
        assert!(h.starts_with("<head><script>"));
        assert_eq!(rewrite_css("a{background:url(/i.png)} @import \"/x.css\";", "/fwd/s/1"), "a{background:url(/fwd/s/1/i.png)} @import \"/fwd/s/1/x.css\";");
    }
}
