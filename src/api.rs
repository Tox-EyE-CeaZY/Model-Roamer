//! The API routes. Everything the UI does goes through here; everything here is also what a remote device uses.

use crate::{
    agents, artifacts,
    core::{CoreRef, TermEntry},
    forward, git, github, memory, net, procs, runner,
    server::{self, bad, ApiErr, Who, R},
    setup, util,
};
use axum::{
    body::{Body, Bytes},
    extract::{ws::Message, ConnectInfo, Path, Query, State, WebSocketUpgrade},
    http::{header, HeaderMap, StatusCode},
    middleware,
    response::{IntoResponse, Redirect, Response},
    routing::{any, delete, get, post},
    Extension, Json, Router,
};
use base64::Engine;
use serde_json::{json, Value};
use std::{collections::HashMap, net::SocketAddr, path::PathBuf, time::Duration};

type Q = Query<HashMap<String, String>>;

pub fn router(core: CoreRef) -> Router {
    Router::new()
        .route("/", get(|| async { server::asset("index.html") }))
        .route("/ui/{*path}", get(|Path(p): Path<String>| async move { server::asset(&p) }))
        .route("/favicon.ico", get(|| async { server::asset("favicon.svg") }))
        .route("/manifest.webmanifest", get(|| async { server::asset("manifest.webmanifest") }))
        .route("/sw.js", get(|| async { server::asset("sw.js") }))
        .route("/auth", get(auth_cookie))
        .route("/a/{id}/{sig}", get(artifact_file))
        .route("/a/{id}/{sig}/", get(artifact_file))
        .route("/a/{id}/{sig}/{*rest}", get(artifact_file))
        .route("/fwd/{sig}/{port}", any(forward::serve))
        .route("/fwd/{sig}/{port}/", any(forward::serve))
        .route("/fwd/{sig}/{port}/{*rest}", any(forward::serve))
        .route("/api/forward", post(forward_url))
        .route("/api/hello", get(hello))
        .route("/api/pair", post(pair))
        .route("/api/mcp", post(mcp))
        .route("/api/ws", any(ws))
        .route("/api/state", get(state))
        .route("/api/settings", get(get_settings).patch(patch_settings))
        // projects
        .route("/api/projects", get(list_projects).post(create_project))
        .route("/api/projects/{id}", get(get_project).patch(patch_project).delete(delete_project))
        .route("/api/projects/{id}/restore", post(restore_project))
        .route("/api/projects/{id}/threads", get(project_threads))
        .route("/api/projects/{id}/git", get(project_git))
        .route("/api/projects/{id}/files", get(crate::files::list))
        .route("/api/projects/{id}/file", get(crate::files::read))
        .route("/api/projects/{id}/prs", get(project_prs))
        .route("/api/projects/{id}/prs/{number}/merge", post(project_pr_merge))
        .route("/api/projects/{id}/workers", post(new_worker))
        // threads
        .route("/api/threads", get(list_threads).post(create_chat))
        .route("/api/threads/{id}", get(get_thread).patch(patch_thread).delete(delete_thread))
        .route("/api/threads/{id}/restore", post(restore_thread))
        .route("/api/threads/{id}/messages", get(thread_messages).post(send_message))
        .route("/api/threads/{id}/activity", get(thread_activity))
        .route("/api/threads/{id}/action", post(thread_action))
        .route("/api/threads/{id}/diff", get(thread_diff))
        .route("/api/threads/{id}/merge", post(thread_merge))
        .route("/api/threads/{id}/pr", post(thread_pr))
        .route("/api/messages/{id}", axum::routing::patch(patch_message).delete(delete_message))
        .route("/api/messages/{id}/now", post(message_now))
        .route("/api/decisions/{id}", post(answer_decision))
        .route("/api/search", get(search))
        // memory
        .route("/api/memory", get(list_memory).post(save_memory))
        .route("/api/memory/{id}", get(get_memory).delete(delete_memory))
        .route("/api/memory/{id}/restore", post(restore_memory))
        // artifacts
        .route("/api/artifacts", get(list_artifacts).post(create_artifact))
        .route("/api/artifacts/{id}", get(get_artifact).patch(patch_artifact).delete(delete_artifact))
        .route("/api/folders", get(list_folders).post(create_folder))
        .route("/api/folders/{id}", axum::routing::patch(patch_folder).delete(delete_folder))
        // agents and tools
        .route("/api/agents", get(list_agents).post(add_custom_agent))
        .route("/api/agents/{id}", get(agent_status).patch(patch_agent).delete(remove_custom_agent))
        .route("/api/agents/{id}/models", post(refresh_agent_models))
        .route("/api/agents/{id}/test", post(test_agent))
        .route("/api/agents/{id}/limit", delete(clear_limit))
        .route("/api/setup", get(setup_status))
        .route("/api/activity", get(activity))
        .route("/api/terms", get(list_terms).post(start_term))
        .route("/api/terms/{id}", get(term_buffer).delete(stop_term))
        .route("/api/terms/{id}/input", post(term_input))
        .route("/api/terms/{id}/resize", post(term_resize))
        // usage, GitHub, files
        .route("/api/usage", get(usage))
        .route("/api/github", get(github_status).post(github_token))
        .route("/api/github/repos", get(github_repos))
        .route("/api/fs", get(fs_list))
        .route("/api/fs/mkdir", post(fs_mkdir))
        .route("/api/fs/reveal", post(fs_reveal))
        .route("/api/upload", post(upload))
        // remote access
        .route("/api/network", get(network))
        .route("/api/network/tailscale-serve", post(tailscale_serve))
        .route("/api/pairing", post(new_pairing))
        .route("/api/devices", get(list_devices))
        .route("/api/devices/{id}", delete(revoke_device))
        .route("/api/connections", get(list_connections).post(add_connection))
        .route("/api/connections/{id}", delete(remove_connection))
        .route("/api/connections/{id}/open", post(open_connection))
        .route("/api/connections/test", post(test_connection))
        .route("/api/discover", get(discover))
        // the app itself
        .route("/api/desktop", post(desktop_cmd))
        .route("/api/suite", get(suite))
        .route("/api/update", get(update_check).post(update_do))
        .route("/api/logs", get(logs))
        .layer(axum::extract::DefaultBodyLimit::max(80 * 1024 * 1024))
        .layer(middleware::from_fn_with_state(core.clone(), server::auth))
        .layer(middleware::from_fn(server::headers))
        .with_state(core)
}

fn s(v: &Value, k: &str) -> String {
    v[k].as_str().unwrap_or("").trim().to_string()
}

// ------------------------------------------------------------------------------------------------ auth and pairing

async fn auth_cookie(State(core): State<CoreRef>, ConnectInfo(addr): ConnectInfo<SocketAddr>, headers: HeaderMap, q: Q) -> Response {
    let tok = q.get("token").cloned().unwrap_or_default();
    let next = q.get("next").filter(|n| n.starts_with('/') && !n.starts_with("//")).cloned().unwrap_or_else(|| "/".into());
    if server::identify(&core, &tok, &addr.ip().to_string()).is_none() {
        return Redirect::to("/#/pair?expired=1").into_response();
    }
    let secure = headers.get("x-forwarded-proto").and_then(|v| v.to_str().ok()) == Some("https") || core.settings()["server"]["tls_cert"].as_str().is_some_and(|c| !c.is_empty()) && !addr.ip().is_loopback();
    let cookie = format!("{}={tok}; Path=/; HttpOnly; SameSite=Lax; Max-Age=31536000{}", server::COOKIE, if secure { "; Secure" } else { "" });
    let mut r = Redirect::to(&next).into_response();
    r.headers_mut().insert(header::SET_COOKIE, cookie.parse().unwrap());
    r
}

async fn hello(State(core): State<CoreRef>) -> Json<Value> {
    Json(json!({ "app": "model-roamer", "version": env!("CARGO_PKG_VERSION"), "name": core.display_name(), "id": core.instance_id, "headless": core.headless, "os": crate::os::os_name() }))
}

/// Exchange a pairing code for a device token.
async fn pair(State(core): State<CoreRef>, ConnectInfo(addr): ConnectInfo<SocketAddr>, Json(b): Json<Value>) -> R {
    let now = util::now_ms();
    {
        let mut f = core.pair_failures.lock().unwrap_or_else(|p| p.into_inner());
        f.retain(|t| *t > now - 10 * 60_000);
        if f.len() >= 12 {
            return Err(ApiErr(StatusCode::TOO_MANY_REQUESTS, "Too many wrong codes. Wait a few minutes, then make a new code on the server.".into()));
        }
    }
    let code = s(&b, "code").to_uppercase().replace([' ', '-'], "");
    let ok = {
        let mut p = core.pairing.lock().unwrap_or_else(|p| p.into_inner());
        match p.as_ref() {
            Some((c, exp)) if *exp > now && util::same(&c.replace('-', ""), &code) => {
                *p = None; // single use
                true
            }
            _ => false,
        }
    };
    if !ok {
        core.pair_failures.lock().unwrap_or_else(|p| p.into_inner()).push(now);
        return Err(ApiErr(StatusCode::UNAUTHORIZED, "That code didn't match, or it expired. Make a new one on the server (Settings › Remote access).".into()));
    }
    let token = util::secret();
    let id = util::id("dev");
    let name = if s(&b, "name").is_empty() { format!("Device at {}", addr.ip()) } else { s(&b, "name") };
    let hash = util::sha256(&token);
    let ip = addr.ip().to_string();
    core.db.exec("INSERT INTO devices(id, name, token_hash, created_at, last_seen, last_ip) VALUES (?1, ?2, ?3, ?4, ?4, ?5)", &[&id, &name, &hash, &now, &ip])?;
    core.bus.emit("devices", None, None, json!({}));
    tracing::info!("paired device {name} from {ip}");
    Ok(Json(json!({ "token": token, "device": id, "server": core.display_name(), "server_id": core.instance_id })))
}

pub fn make_pairing(core: &CoreRef) -> String {
    let raw = util::rand_str(8).to_uppercase();
    let code = format!("{}-{}", &raw[..4], &raw[4..]);
    *core.pairing.lock().unwrap_or_else(|p| p.into_inner()) = Some((code.clone(), util::now_ms() + 10 * 60_000));
    code
}

async fn new_pairing(State(core): State<CoreRef>, Extension(who): Extension<Who>) -> R {
    let _ = who;
    let code = make_pairing(&core);
    let port = core.settings()["server"]["port"].as_u64().unwrap_or(4630) as u16;
    let addrs = net::addresses(&core, port, false).await;
    let first = addrs["urls"].as_array().and_then(|a| a.first()).and_then(|u| u["url"].as_str()).unwrap_or("").to_string();
    let link = format!("{first}/#/pair?code={code}");
    Ok(Json(json!({ "code": code, "expires_in": 600, "link": link, "qr": net::qr_svg(&link), "addresses": addrs })))
}

async fn list_devices(State(core): State<CoreRef>, Extension(who): Extension<Who>) -> R {
    let me = match who {
        Who::Device(d) => d,
        _ => String::new(),
    };
    let rows = core.db.all("SELECT id, name, created_at, last_seen, last_ip FROM devices WHERE revoked = 0 ORDER BY last_seen DESC", &[])?;
    Ok(Json(json!({ "devices": rows, "me": me })))
}

async fn revoke_device(State(core): State<CoreRef>, Path(id): Path<String>) -> R {
    core.db.exec("UPDATE devices SET revoked = 1 WHERE id = ?1", &[&id])?;
    core.bus.emit("devices", None, None, json!({}));
    Ok(Json(json!({ "ok": true })))
}

// ------------------------------------------------------------------------------------------------ MCP bridge

async fn mcp(State(core): State<CoreRef>, headers: HeaderMap, Json(req): Json<Value>) -> Response {
    let tok = headers.get(header::AUTHORIZATION).and_then(|v| v.to_str().ok()).and_then(|v| v.strip_prefix("Bearer ")).unwrap_or("");
    let thread = core.turn_tokens.lock().ok().and_then(|m| m.get(tok).cloned());
    let Some(thread) = thread else {
        return ApiErr(StatusCode::UNAUTHORIZED, "This tool token has expired (the turn ended).".into()).into_response();
    };
    match crate::mcp::handle(&core, &thread, &req).await {
        Some(v) => Json(v).into_response(),
        None => StatusCode::ACCEPTED.into_response(),
    }
}

// ------------------------------------------------------------------------------------------------ live events

async fn ws(State(core): State<CoreRef>, up: WebSocketUpgrade) -> Response {
    up.on_upgrade(move |mut sock| async move {
        let mut rx = core.bus.subscribe();
        let _ = sock.send(Message::Text(json!({ "kind": "hello", "data": { "version": env!("CARGO_PKG_VERSION") } }).to_string().into())).await;
        let mut ping = tokio::time::interval(Duration::from_secs(25));
        loop {
            tokio::select! {
                ev = rx.recv() => match ev {
                    Ok(ev) => {
                        let txt = serde_json::to_string(&ev).unwrap_or_default();
                        if sock.send(Message::Text(txt.into())).await.is_err() { break; }
                    }
                    Err(tokio::sync::broadcast::error::RecvError::Lagged(n)) => {
                        // the client fell behind: tell it to reload what it shows
                        let _ = sock.send(Message::Text(json!({ "kind": "resync", "data": { "missed": n } }).to_string().into())).await;
                    }
                    Err(_) => break,
                },
                m = sock.recv() => match m {
                    Some(Ok(Message::Close(_))) | None | Some(Err(_)) => break,
                    _ => {}
                },
                _ = ping.tick() => { if sock.send(Message::Ping(Vec::new().into())).await.is_err() { break; } }
            }
        }
    })
}

// ------------------------------------------------------------------------------------------------ state and settings

async fn state(State(core): State<CoreRef>, Extension(who): Extension<Who>) -> R {
    let settings = crate::config::redact(&core.settings());
    let projects = core.db.all(
        "SELECT p.*, (SELECT COUNT(*) FROM threads t WHERE t.project_id = p.id AND t.kind != 'lead' AND t.resolved = 0 AND t.deleted_at IS NULL) AS open_threads,
                (SELECT COUNT(*) FROM threads t WHERE t.project_id = p.id AND t.status IN ('working','queued') AND t.deleted_at IS NULL) AS working,
                (SELECT COUNT(*) FROM threads t WHERE t.project_id = p.id AND t.unread = 1 AND t.deleted_at IS NULL) AS unread,
                (SELECT COUNT(*) FROM decisions d WHERE d.project_id = p.id AND d.answer IS NULL) AS decisions
         FROM projects p WHERE p.deleted_at IS NULL ORDER BY p.updated_at DESC",
        &[],
    )?;
    let running: Vec<Value> = core.running.lock().map(|r| r.iter().map(|(k, v)| json!({ "thread": k, "turn": v.turn_id, "started": v.started })).collect()).unwrap_or_default();
    Ok(Json(json!({
        "version": env!("CARGO_PKG_VERSION"),
        "server": { "name": core.display_name(), "id": core.instance_id, "headless": core.headless, "os": crate::os::os_name() },
        "me": { "local": who.is_local(), "device": match &who { Who::Device(d) => d.clone(), _ => String::new() } },
        "settings": settings,
        "projects": projects,
        "agents": core.cached_status(),
        "running": running,
        "task_kinds": agents::TASK_KINDS.iter().map(|(k, d)| json!({ "id": k, "label": d })).collect::<Vec<_>>(),
        "data_dir": core.data.display().to_string(),
    })))
}

/// Everything going on right now, for the status bar: turns running or queued, installs and sign-ins in their
/// terminals, agent checks, questions waiting on the person, recent failures and agents that need attention.
async fn activity(State(core): State<CoreRef>) -> R {
    let running: std::collections::HashMap<String, i64> = core.running.lock().map(|r| r.iter().map(|(k, v)| (k.clone(), v.started)).collect()).unwrap_or_default();
    let recent = util::now_ms() - 30 * 60 * 1000;
    let mut threads = core.db.all(
        "SELECT t.id, t.kind, t.title, t.agent, t.model, t.status, t.status_note, t.project_id, p.name AS project, t.updated_at
         FROM threads t LEFT JOIN projects p ON p.id = t.project_id
         WHERE t.deleted_at IS NULL AND t.resolved = 0 AND (t.status IN ('working','queued','blocked','limited') OR (t.status = 'failed' AND t.updated_at > ?1))
         ORDER BY t.updated_at DESC LIMIT 40",
        &[&recent],
    )?;
    for t in threads.iter_mut() {
        if let Some(st) = t["id"].as_str().and_then(|id| running.get(id)) {
            t["started"] = json!(st);
            t["status"] = json!("working");
        }
    }
    let decisions = core.db.all(
        "SELECT d.id, d.thread_id, d.project_id, d.question, t.title, t.kind, d.created_at FROM decisions d JOIN threads t ON t.id = d.thread_id
         WHERE d.answer IS NULL AND t.deleted_at IS NULL ORDER BY d.created_at DESC LIMIT 20",
        &[],
    )?;
    let terms = core.terms.lock().map(|m| m.iter().filter(|(_, v)| v.exited.is_none()).map(|(k, v)| json!({ "id": k, "label": v.label, "agent": v.agent, "started": v.started })).collect::<Vec<_>>()).unwrap_or_default();
    let agents: Vec<Value> = core
        .cached_status()
        .into_iter()
        .filter(|a| a["config"]["enabled"] != false && a["installed"] != false)
        .filter_map(|a| {
            let state = if a["pending"] == true {
                "checking"
            } else if a["limit"]["active"] == true {
                "limit"
            } else if a["auth"]["signed_in"] == "no" {
                "signin"
            } else {
                return None;
            };
            Some(json!({ "id": a["id"], "name": a["info"]["name"], "state": state, "note": a["limit"]["note"] }))
        })
        .collect();
    Ok(Json(json!({ "threads": threads, "decisions": decisions, "terms": terms, "agents": agents, "update": crate::update::staged_tag(), "quotas": core.quotas() })))
}

async fn get_settings(State(core): State<CoreRef>) -> R {
    Ok(Json(crate::config::redact(&core.settings())))
}

async fn patch_settings(State(core): State<CoreRef>, Json(b): Json<Value>) -> R {
    let before = core.settings()["server"].clone();
    let full = core.patch_settings(&b)?;
    // sharing on/off, or a new address: the listener follows at once
    let s = &full["server"];
    let listen_changed = ["share", "bind", "port", "tls_cert", "tls_key"].iter().any(|k| before[k] != s[k]);
    if listen_changed && !core.headless {
        if let Err(e) = server::apply_sharing(&core, None).await {
            // put it back as it was, so the switch shows the truth
            core.patch_settings(&json!({ "server": { "share": false } }))?;
            return Err(bad(format!("{e:#}")));
        }
    }
    Ok(Json(crate::config::redact(&full)))
}

// ------------------------------------------------------------------------------------------------ projects

async fn list_projects(State(core): State<CoreRef>, q: Q) -> R {
    let deleted = q.get("deleted").is_some();
    let rows = core.db.all(if deleted { "SELECT * FROM projects WHERE deleted_at IS NOT NULL ORDER BY deleted_at DESC" } else { "SELECT * FROM projects WHERE deleted_at IS NULL ORDER BY updated_at DESC" }, &[])?;
    Ok(Json(json!(rows)))
}

async fn create_project(State(core): State<CoreRef>, Json(b): Json<Value>) -> R {
    let name = s(&b, "name");
    if name.is_empty() {
        return Err(bad("Give the project a name."));
    }
    let folder = s(&b, "folder");
    if !folder.is_empty() && !PathBuf::from(&folder).is_dir() {
        if b["create_folder"].as_bool() == Some(true) {
            std::fs::create_dir_all(&folder)?;
        } else {
            return Err(bad(format!("The folder {folder} doesn't exist.")));
        }
    }
    let st = core.settings();
    let lead = if s(&b, "lead_agent").is_empty() { st["default_lead"].as_str().unwrap_or("claude").to_string() } else { s(&b, "lead_agent") };
    let lead_model = if b.get("lead_model").is_some() { s(&b, "lead_model") } else { st["default_lead_model"].as_str().unwrap_or("").to_string() };
    let id = util::id("prj");
    let now = util::now_ms();
    let mut base = s(&b, "base_branch");
    if base.is_empty() && !folder.is_empty() && git::is_repo(&PathBuf::from(&folder)).await {
        base = git::current_branch(&PathBuf::from(&folder)).await.unwrap_or_default();
    }
    let mut repo = s(&b, "repo_url");
    if repo.is_empty() && !folder.is_empty() {
        repo = git::remote_url(&PathBuf::from(&folder)).await;
    }
    let hue = b["color"].as_i64().unwrap_or_else(|| [128, 38, 175, 262, 318, 205, 85][(now as usize / 7) % 7]);
    core.db.exec(
        "INSERT INTO projects(id, name, instructions, lead_agent, lead_model, folder, repo_url, base_branch, color, created_at, updated_at) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?10)",
        &[&id, &name, &s(&b, "instructions"), &lead, &lead_model, &folder, &repo, &base, &hue, &now],
    )?;
    let p = core.project(&id)?;
    core.lead_thread(&id)?;
    core.bus.emit("project", Some(&id), None, p.clone());
    // a repository without a folder gets cloned now, in the background
    if folder.is_empty() && !repo.is_empty() {
        let c = core.clone();
        let p2 = p.clone();
        tokio::spawn(async move {
            if let Err(e) = runner::ensure_project_folder(&c, &p2).await {
                c.bus.emit("toast", p2["id"].as_str(), None, json!({ "err": true, "text": format!("Couldn't clone the repository: {e}") }));
            }
        });
    }
    Ok(Json(p))
}

async fn get_project(State(core): State<CoreRef>, Path(id): Path<String>) -> R {
    let mut p = core.project(&id)?;
    p["lead"] = core.lead_thread(&id)?;
    p["home"] = json!(core.project_home(&p).display().to_string());
    p["decisions"] = json!(core.db.all("SELECT * FROM decisions WHERE project_id = ?1 AND answer IS NULL ORDER BY created_at", &[&id])?);
    p["claims"] = json!(core.db.all("SELECT c.*, t.title, t.agent FROM claims c JOIN threads t ON t.id = c.thread_id WHERE c.project_id = ?1", &[&id])?);
    Ok(Json(p))
}

async fn patch_project(State(core): State<CoreRef>, Path(id): Path<String>, Json(b): Json<Value>) -> R {
    core.project(&id)?;
    let now = util::now_ms();
    for k in ["name", "instructions", "lead_agent", "lead_model", "folder", "repo_url", "base_branch"] {
        if let Some(v) = b.get(k).and_then(|v| v.as_str()) {
            if k == "name" && v.trim().is_empty() {
                return Err(bad("A project needs a name."));
            }
            if k == "folder" && !v.is_empty() && !PathBuf::from(v).is_dir() {
                return Err(bad(format!("The folder {v} doesn't exist.")));
            }
            core.db.exec(&format!("UPDATE projects SET {k} = ?1, updated_at = ?2 WHERE id = ?3"), &[&v.trim(), &now, &id])?;
        }
    }
    if let Some(c) = b["color"].as_i64() {
        core.db.exec("UPDATE projects SET color = ?1 WHERE id = ?2", &[&c, &id])?;
    }
    if let Some(r) = b.get("routing").filter(|r| r.is_object()) {
        let rs = r.to_string();
        core.db.exec("UPDATE projects SET routing = ?1 WHERE id = ?2", &[&rs, &id])?;
    }
    // switching the lead: the lead thread follows on its next turn, with the history
    if b.get("lead_agent").is_some() || b.get("lead_model").is_some() {
        let p = core.project(&id)?;
        let l = core.lead_thread(&id)?;
        if l["agent"] != p["lead_agent"] || l["model"] != p["lead_model"] {
            core.set_thread(l["id"].as_str().unwrap_or(""), &json!({ "agent": p["lead_agent"], "model": p["lead_model"], "session_id": "", "session_agent": "" }))?;
        }
    }
    let p = core.project(&id)?;
    core.bus.emit("project", Some(&id), None, p.clone());
    Ok(Json(p))
}

async fn delete_project(State(core): State<CoreRef>, Path(id): Path<String>) -> R {
    let threads = core.db.all("SELECT id FROM threads WHERE project_id = ?1", &[&id])?;
    for t in &threads {
        runner::cancel(&core, t["id"].as_str().unwrap_or(""), "paused", "Project deleted").await;
    }
    let now = util::now_ms();
    core.db.exec("UPDATE projects SET deleted_at = ?1 WHERE id = ?2", &[&now, &id])?;
    core.bus.emit("project", Some(&id), None, json!({ "id": id, "deleted": true }));
    Ok(Json(json!({ "ok": true })))
}

async fn restore_project(State(core): State<CoreRef>, Path(id): Path<String>) -> R {
    core.db.exec("UPDATE projects SET deleted_at = NULL WHERE id = ?1", &[&id])?;
    let p = core.project(&id)?;
    core.bus.emit("project", Some(&id), None, p.clone());
    Ok(Json(p))
}

async fn project_threads(State(core): State<CoreRef>, Path(id): Path<String>) -> R {
    let rows = core.db.all(
        "SELECT t.*, (SELECT body FROM messages m WHERE m.thread_id = t.id AND m.role IN ('agent','user') ORDER BY created_at DESC LIMIT 1) AS last_body,
                (SELECT COUNT(*) FROM messages m WHERE m.thread_id = t.id) AS message_count
         FROM threads t WHERE t.project_id = ?1 AND t.deleted_at IS NULL ORDER BY t.kind = 'lead' DESC, t.created_at",
        &[&id],
    )?;
    let running = core.running.lock().map(|r| r.keys().cloned().collect::<Vec<_>>()).unwrap_or_default();
    let rows: Vec<Value> = rows
        .into_iter()
        .map(|mut r| {
            r["last_body"] = json!(util::clip(r["last_body"].as_str().unwrap_or(""), 240));
            r["running"] = json!(running.iter().any(|x| Some(x.as_str()) == r["id"].as_str()));
            r
        })
        .collect();
    Ok(Json(json!(rows)))
}

async fn project_git(State(core): State<CoreRef>, Path(id): Path<String>) -> R {
    let p = core.project(&id)?;
    let home = core.project_home(&p);
    if !git::is_repo(&home).await {
        return Ok(Json(json!({ "repo": false, "folder": home.display().to_string(), "git": git::git().is_some() })));
    }
    Ok(Json(json!({
        "repo": true,
        "folder": home.display().to_string(),
        "branch": git::current_branch(&home).await.unwrap_or_default(),
        "remote": git::remote_url(&home).await,
        "status": git::status(&home).await,
        "log": git::log(&home, 15).await.iter().map(|l| { let p: Vec<&str> = l.split('\x1f').collect(); json!({ "sha": p.first(), "author": p.get(1), "when": p.get(2), "subject": p.get(3) }) }).collect::<Vec<_>>(),
    })))
}

/// The GitHub owner and repository a project pushes to: its repo URL, else its folder's remote.
async fn project_github(core: &CoreRef, id: &str) -> Result<Option<(String, String)>, ApiErr> {
    let p = core.project(id)?;
    let mut url = s(&p, "repo_url");
    if url.is_empty() {
        url = git::remote_url(&core.project_home(&p)).await;
    }
    Ok(git::github_repo(&url))
}

async fn project_prs(State(core): State<CoreRef>, Path(id): Path<String>) -> R {
    let Some((o, r)) = project_github(&core, &id).await? else { return Ok(Json(json!({ "github": false }))) };
    match github::list_prs(&core, &o, &r, "all").await {
        Ok(list) => Ok(Json(json!({ "github": true, "repo": format!("{o}/{r}"), "prs": list }))),
        Err(e) => Ok(Json(json!({ "github": true, "repo": format!("{o}/{r}"), "error": e.to_string() }))),
    }
}

async fn project_pr_merge(State(core): State<CoreRef>, Path((id, number)): Path<(String, i64)>, Json(b): Json<Value>) -> R {
    let Some((o, r)) = project_github(&core, &id).await? else { return Err(bad("This project's remote isn't on GitHub.")) };
    let method = match s(&b, "method").as_str() {
        m @ ("merge" | "squash" | "rebase") => m.to_string(),
        _ => "merge".to_string(),
    };
    Ok(Json(github::merge_pr(&core, &o, &r, number, &method).await?))
}

/// The person opens a worker thread by hand (the lead hears about it).
async fn new_worker(State(core): State<CoreRef>, Path(id): Path<String>, Json(b): Json<Value>) -> R {
    let lead = core.lead_thread(&id)?;
    let t = runner::create_worker(
        &core,
        &id,
        runner::NewWorker {
            title: s(&b, "title"),
            instructions: s(&b, "instructions"),
            criteria: s(&b, "criteria"),
            agent: s(&b, "agent"),
            task_kind: s(&b, "task_kind"),
            model: s(&b, "model"),
            effort: s(&b, "effort"),
            workspace: s(&b, "workspace"),
            depends_on: vec![],
            parent: lead["id"].as_str().map(str::to_string),
            kind: "worker".into(),
        },
    )
    .await?;
    runner::post(&core, lead["id"].as_str().unwrap_or(""), "wake", "Model Roamer", &format!(
        "<wake reason=\"person-opened-thread\" thread=\"{}\" agent=\"{}\">The person opened a thread themselves: \"{}\". Keep it in mind for planning; you don't need to reply unless it affects other work.</wake>",
        t["id"].as_str().unwrap_or(""), t["agent"].as_str().unwrap_or(""), t["title"].as_str().unwrap_or("")
    ), json!({}), false)?;
    Ok(Json(t))
}

// ------------------------------------------------------------------------------------------------ threads

async fn list_threads(State(core): State<CoreRef>, q: Q) -> R {
    let kind = q.get("kind").cloned().unwrap_or_else(|| "chat".into());
    let rows = core.db.all(
        "SELECT t.*, (SELECT body FROM messages m WHERE m.thread_id = t.id AND m.role IN ('agent','user') ORDER BY created_at DESC LIMIT 1) AS last_body
         FROM threads t WHERE t.kind = ?1 AND t.deleted_at IS NULL ORDER BY t.pinned DESC, t.updated_at DESC LIMIT 300",
        &[&kind],
    )?;
    Ok(Json(json!(rows.into_iter().map(|mut r| { r["last_body"] = json!(util::clip(r["last_body"].as_str().unwrap_or(""), 200)); r }).collect::<Vec<_>>())))
}

async fn create_chat(State(core): State<CoreRef>, Json(b): Json<Value>) -> R {
    let agent = if s(&b, "agent").is_empty() { core.settings()["default_lead"].as_str().unwrap_or("claude").to_string() } else { s(&b, "agent") };
    let model = if b.get("model").is_some() { s(&b, "model") } else { core.agent_cfg(&agent)["model"].as_str().unwrap_or("").to_string() };
    let mut t = runner::create_chat(&core, &agent, &model, &s(&b, "title"), &s(&b, "folder"))?;
    if !s(&b, "effort").is_empty() && !agent.starts_with("buddy:") {
        t = core.set_thread(t["id"].as_str().unwrap_or(""), &json!({ "effort": s(&b, "effort") }))?;
    }
    let text = s(&b, "text");
    if !text.is_empty() {
        runner::post(&core, t["id"].as_str().unwrap_or(""), "user", "You", &text, json!({ "files": b["files"] }), true)?;
    }
    Ok(Json(t))
}

async fn get_thread(State(core): State<CoreRef>, Path(id): Path<String>) -> R {
    let mut t = core.thread(&id)?;
    if let Some(p) = t["project_id"].as_str() {
        t["project"] = core.project(p).unwrap_or(Value::Null);
    }
    t["running"] = json!(core.running.lock().map(|r| r.get(&id).map(|x| json!({ "turn": x.turn_id, "started": x.started }))).ok().flatten());
    t["workspace_path"] = json!(runner::workspace_of(&core, &t).map(|p| p.display().to_string()));
    t["decisions"] = json!(core.db.all("SELECT * FROM decisions WHERE thread_id = ?1 ORDER BY created_at", &[&id])?);
    t["artifacts"] = json!(core.db.all("SELECT * FROM artifacts WHERE thread_id = ?1 AND deleted_at IS NULL ORDER BY updated_at DESC", &[&id])?);
    t["usage"] = core.db.one("SELECT COUNT(*) AS turns, SUM(input_tokens) AS input, SUM(output_tokens) AS output, SUM(cost_usd) AS cost FROM turns WHERE thread_id = ?1", &[&id])?.unwrap_or_default();
    t["children"] = json!(core.db.all("SELECT id, title, agent, status FROM threads WHERE parent_id = ?1 AND deleted_at IS NULL", &[&id])?);
    if t["unread"] == 1 {
        core.db.exec("UPDATE threads SET unread = 0 WHERE id = ?1", &[&id])?;
    }
    Ok(Json(t))
}

async fn patch_thread(State(core): State<CoreRef>, Path(id): Path<String>, Json(b): Json<Value>) -> R {
    let mut f = serde_json::Map::new();
    for k in ["title", "pinned", "unread", "instructions", "criteria", "model", "effort"] {
        if let Some(v) = b.get(k) {
            f.insert(k.into(), v.clone());
        }
    }
    // a title the person typed is theirs: agents' set_title leaves it alone from now on
    if f.contains_key("title") {
        f.insert("title_by".into(), json!("person"));
    }
    let t = core.thread(&id)?;
    // how this thread shows its steps: a view choice, so it doesn't move the thread up the lists
    if let Some(v) = b.get("steps") {
        let v = v.as_str().unwrap_or("");
        if !["", "collapsed", "expanded", "hidden"].contains(&v) {
            return Err(bad("Steps can be collapsed, expanded or hidden."));
        }
        core.db.exec("UPDATE threads SET steps = ?1 WHERE id = ?2", &[&v, &id])?;
        if f.is_empty() && b.get("agent").is_none() && b.get("folder").is_none() {
            let t = core.thread(&id)?;
            core.bus.emit("thread", t["project_id"].as_str(), Some(&id), t.clone());
            return Ok(Json(t));
        }
    }
    // another agent (a chat's picker): its CLI can't resume the old one's session, so the next turn starts fresh
    if let Some(a0) = b["agent"].as_str().filter(|a| !a.is_empty() && (a.starts_with("buddy:") || Some(*a) != t["agent"].as_str() || t["buddy"].is_object())) {
        // a buddy pair (two agents taking turns) or a single agent
        let (a_s, m_s, pair) = core.pick_agent(a0, b["model"].as_str().unwrap_or("")).map_err(|e| bad(e.to_string()))?;
        let a = a_s.as_str();
        core.db.exec("UPDATE threads SET buddy = ?1 WHERE id = ?2", &[&pair.as_ref().map(|p| p.to_string()), &id])?;
        if pair.is_some() {
            f.insert("model".into(), json!(m_s));
        }
        if core.adapter(a).is_none() {
            return Err(bad(format!("There's no agent called {a}.")));
        }
        if core.running.lock().map(|r| r.contains_key(&id)).unwrap_or(false) {
            return Err(bad("Wait for this turn to finish (or stop it) before switching agents."));
        }
        f.insert("agent".into(), json!(a));
        // another agent has other effort levels: back to its default unless the person chose one
        f.entry("effort").or_insert(json!(""));
        f.insert("session_id".into(), json!(""));
        f.insert("session_agent".into(), json!(""));
        if !f.contains_key("model") {
            f.insert("model".into(), core.agent_cfg(a)["model"].clone());
        }
    }
    // a chat's folder: a path, or "" for a fresh one of its own
    if let Some(folder) = b["folder"].as_str() {
        if t["kind"] != "chat" {
            return Err(bad("Only a chat's folder can be changed here."));
        }
        let folder = folder.trim();
        if !folder.is_empty() && !std::path::Path::new(folder).is_dir() {
            return Err(bad(format!("{folder} isn't a folder on this machine.")));
        }
        f.insert("workspace".into(), json!(if folder.is_empty() { String::new() } else { format!("path:{folder}") }));
        f.insert("session_id".into(), json!(""));
        f.insert("session_agent".into(), json!(""));
    }
    Ok(Json(core.set_thread(&id, &Value::Object(f))?))
}

async fn delete_thread(State(core): State<CoreRef>, Path(id): Path<String>) -> R {
    let t = core.thread(&id)?;
    if t["kind"] == "lead" {
        return Err(bad("The project chat can't be deleted. Delete the project instead."));
    }
    runner::cancel(&core, &id, "cancelled", "Deleted").await;
    let now = util::now_ms();
    core.db.exec("UPDATE threads SET deleted_at = ?1 WHERE id = ?2", &[&now, &id])?;
    core.db.exec("DELETE FROM claims WHERE thread_id = ?1", &[&id])?;
    core.bus.emit("thread", t["project_id"].as_str(), Some(&id), json!({ "id": id, "deleted": true }));
    Ok(Json(json!({ "ok": true })))
}

async fn restore_thread(State(core): State<CoreRef>, Path(id): Path<String>) -> R {
    core.db.exec("UPDATE threads SET deleted_at = NULL, status = 'idle' WHERE id = ?1", &[&id])?;
    let t = core.thread(&id)?;
    core.bus.emit("thread", t["project_id"].as_str(), Some(&id), t.clone());
    Ok(Json(t))
}

async fn thread_messages(State(core): State<CoreRef>, Path(id): Path<String>, q: Q) -> R {
    let before: i64 = q.get("before").and_then(|b| b.parse().ok()).unwrap_or(i64::MAX);
    let limit: i64 = q.get("limit").and_then(|b| b.parse().ok()).unwrap_or(80).clamp(1, 500);
    let rows = core.db.all("SELECT * FROM (SELECT * FROM messages WHERE thread_id = ?1 AND created_at < ?2 ORDER BY created_at DESC LIMIT ?3) ORDER BY created_at", &[&id, &before, &limit])?;
    let turns = core.db.all("SELECT id, agent, model, status, error, input_tokens, output_tokens, cost_usd, started_at, ended_at FROM turns WHERE thread_id = ?1 ORDER BY started_at DESC LIMIT 100", &[&id])?;
    Ok(Json(json!({ "messages": rows, "turns": turns })))
}

async fn send_message(State(core): State<CoreRef>, Path(id): Path<String>, Extension(who): Extension<Who>, Json(b): Json<Value>) -> R {
    let t = core.thread(&id)?;
    let text = s(&b, "text");
    if text.is_empty() && b["files"].as_array().map(|a| a.is_empty()).unwrap_or(true) {
        return Err(bad("Write a message first."));
    }
    // a message to a paused/failed thread means "carry on"
    if matches!(t["status"].as_str(), Some("paused" | "failed" | "cancelled")) {
        core.set_status(&id, "idle", "")?;
    }
    let author = match &who {
        Who::Local => "You".to_string(),
        Who::Device(d) => core.db.one("SELECT name FROM devices WHERE id = ?1", &[d]).ok().flatten().and_then(|r| r["name"].as_str().map(|n| format!("You ({n})"))).unwrap_or_else(|| "You".into()),
    };
    runner::buddy_reset(&core, &id);
    let m = runner::post(&core, &id, "user", &author, &text, json!({ "files": b["files"] }), true)?;
    // the person spoke to a worker directly: the lead should know
    if matches!(t["kind"].as_str(), Some("worker" | "review")) {
        if let Some(p) = t["project_id"].as_str() {
            if let Ok(l) = core.lead_thread(p) {
                let body = format!("<wake reason=\"person-messaged-thread\" thread=\"{}\" title=\"{}\">The person wrote to this thread directly:\n{}</wake>", id, t["title"].as_str().unwrap_or(""), util::clip(&text, 2000));
                runner::post(&core, l["id"].as_str().unwrap_or(""), "wake", "Model Roamer", &body, json!({}), false)?;
            }
        }
    }
    Ok(Json(m))
}

/// A message the person wrote that the agent hasn't read yet: only those can still be changed or taken back.
fn waiting_message(core: &CoreRef, id: &str) -> Result<Value, server::ApiErr> {
    let m = core.db.one("SELECT * FROM messages WHERE id = ?1", &[&id]).ok().flatten().ok_or_else(|| bad("That message isn't there."))?;
    if m["pending"] != 1 || m["role"] != "user" || m["meta"]["kickoff"] == true {
        return Err(bad("The agent has already read that message."));
    }
    Ok(m)
}

async fn patch_message(State(core): State<CoreRef>, Path(id): Path<String>, Json(b): Json<Value>) -> R {
    let m = waiting_message(&core, &id)?;
    let text = s(&b, "text");
    if text.is_empty() {
        return Err(bad("A message can't be empty. Delete it instead."));
    }
    // the agent may pick it up between the check and the write: only change it while it's still waiting
    if core.db.exec("UPDATE messages SET body = ?1 WHERE id = ?2 AND pending = 1", &[&text, &id])? == 0 {
        return Err(bad("The agent has already read that message."));
    }
    let m = core.db.one("SELECT * FROM messages WHERE id = ?1", &[&id])?.unwrap_or(m);
    let t = core.thread(m["thread_id"].as_str().unwrap_or(""))?;
    core.bus.emit("message", t["project_id"].as_str(), t["id"].as_str(), m.clone());
    Ok(Json(m))
}

async fn delete_message(State(core): State<CoreRef>, Path(id): Path<String>) -> R {
    let m = waiting_message(&core, &id)?;
    if core.db.exec("DELETE FROM messages WHERE id = ?1 AND pending = 1", &[&id])? == 0 {
        return Err(bad("The agent has already read that message."));
    }
    let tid = m["thread_id"].as_str().unwrap_or("").to_string();
    let t = core.thread(&tid)?;
    core.bus.emit("message", t["project_id"].as_str(), Some(&tid), json!({ "id": id, "deleted": true }));
    Ok(Json(json!({ "ok": true })))
}

/// "Send now": stop what the agent is doing so it reads this message right away (it keeps its session and carries on
/// from there). Agents can't take a message in the middle of a turn, so this is the closest there is.
async fn message_now(State(core): State<CoreRef>, Path(id): Path<String>) -> R {
    let m = waiting_message(&core, &id)?;
    let tid = m["thread_id"].as_str().unwrap_or("").to_string();
    runner::cancel(&core, &tid, "idle", "so it can read your message").await;
    Ok(Json(core.thread(&tid)?))
}

async fn thread_activity(State(core): State<CoreRef>, Path(id): Path<String>, q: Q) -> R {
    let rows = match q.get("turn") {
        Some(turn) => core.db.all("SELECT * FROM activity WHERE thread_id = ?1 AND turn_id = ?2 ORDER BY id", &[&id, turn])?,
        None => core.db.all("SELECT * FROM (SELECT * FROM activity WHERE thread_id = ?1 ORDER BY id DESC LIMIT 400) ORDER BY id", &[&id])?,
    };
    Ok(Json(json!(rows)))
}

async fn thread_action(State(core): State<CoreRef>, Path(id): Path<String>, Json(b): Json<Value>) -> R {
    let t = core.thread(&id)?;
    match s(&b, "action").as_str() {
        "pause" => runner::cancel(&core, &id, "paused", "Paused by you").await,
        "stop" | "cancel" => runner::cancel(&core, &id, "cancelled", "Stopped by you").await,
        "resume" => {
            core.set_status(&id, "idle", "")?;
            if core.db.scalar_i64("SELECT COUNT(*) FROM messages WHERE thread_id = ?1 AND pending = 1", &[&id])? > 0 {
                runner::schedule(&core, &id);
            } else {
                runner::retry(&core, &id)?;
            }
        }
        "retry" => runner::retry(&core, &id)?,
        "resolve" => runner::resolve(&core, &id, true).await?,
        "reopen" => {
            runner::resolve(&core, &id, false).await?;
            core.set_thread(&id, &json!({ "parked_at": serde_json::Value::Null }))?;
            core.set_status(&id, "idle", "")?;
        }
        "reassign" => runner::reassign(&core, &id, &s(&b, "agent"), &s(&b, "model"), &s(&b, "note")).await?,
        "new_session" => {
            core.set_thread(&id, &json!({ "session_id": "", "session_agent": "" }))?;
        }
        "review" => {
            let lead = core.lead_thread(t["project_id"].as_str().unwrap_or(""))?;
            let r = runner::request_review(&core, &t, &s(&b, "agent"), &s(&b, "focus"), lead["id"].as_str().unwrap_or("")).await?;
            return Ok(Json(r));
        }
        "reveal" => {
            if let Some(w) = runner::workspace_of(&core, &t) {
                crate::os::reveal(&w);
            }
        }
        a => return Err(bad(format!("Unknown action {a}"))),
    }
    Ok(Json(core.thread(&id)?))
}

async fn thread_diff(State(core): State<CoreRef>, Path(id): Path<String>) -> R {
    let t = core.thread(&id)?;
    match runner::diff_of(&core, &t).await {
        Ok(d) => Ok(Json(json!({ "diff": d, "branch": t["branch"], "base": t["base_branch"] }))),
        Err(e) => Ok(Json(json!({ "diff": "", "note": e.to_string() }))),
    }
}

async fn thread_merge(State(core): State<CoreRef>, Path(id): Path<String>, Json(b): Json<Value>) -> R {
    let t = core.thread(&id)?;
    let msg = if s(&b, "message").is_empty() { format!("{} ({})", s(&t, "title"), s(&t, "agent")) } else { s(&b, "message") };
    let sha = runner::merge(&core, &t, &msg, b["squash"].as_bool().unwrap_or(false)).await?;
    if let Some(p) = t["project_id"].as_str() {
        runner::wake_lead(&core, p, &format!("<wake reason=\"person-merged\" thread=\"{id}\">The person merged \"{}\" into the base branch ({sha}).</wake>", s(&t, "title")), json!({}));
    }
    Ok(Json(json!({ "sha": sha })))
}

async fn thread_pr(State(core): State<CoreRef>, Path(id): Path<String>, Json(b): Json<Value>) -> R {
    let t = core.thread(&id)?;
    let pr = runner::open_pr(&core, &t, &s(&b, "title"), &s(&b, "body"), b["draft"].as_bool().unwrap_or(false)).await?;
    Ok(Json(pr))
}

async fn answer_decision(State(core): State<CoreRef>, Path(id): Path<String>, Json(b): Json<Value>) -> R {
    Ok(Json(runner::answer_decision(&core, &id, b["choice"].as_i64().ok_or_else(|| bad("choice is required"))?)?))
}

async fn search(State(core): State<CoreRef>, q: Q) -> R {
    let text = q.get("q").cloned().unwrap_or_default();
    if text.trim().len() < 2 {
        return Ok(Json(json!({ "threads": [], "messages": [], "memory": [], "projects": [], "artifacts": [] })));
    }
    let like = format!("%{}%", text.trim());
    let snip = |col: &str| format!("substr({col}, max(1, instr(lower({col}), lower(?2)) - 60), 200)");
    // a thread matches by its title or by the brief it was given
    let threads = core.db.all(
        &format!("SELECT id, title, agent, status, project_id, kind, CASE WHEN title LIKE ?1 THEN '' ELSE {} END AS snippet FROM threads WHERE deleted_at IS NULL AND (title LIKE ?1 OR instructions LIKE ?1 OR criteria LIKE ?1) ORDER BY (title LIKE ?1) DESC, updated_at DESC LIMIT 20", snip("instructions")),
        &[&like, &text.trim()],
    )?;
    // messages in chats, the project chat and every thread of a project
    let messages = core.db.all(
        &format!("SELECT m.id, m.thread_id, m.author, m.role, {} AS snippet, m.created_at, t.title, t.project_id, t.kind FROM messages m JOIN threads t ON t.id = m.thread_id
         WHERE t.deleted_at IS NULL AND m.role != 'wake' AND m.body LIKE ?1 ORDER BY m.created_at DESC LIMIT 40", snip("m.body")),
        &[&like, &text.trim()],
    )?;
    let projects = core.db.all(
        &format!("SELECT id, name, CASE WHEN name LIKE ?1 THEN '' ELSE {} END AS snippet FROM projects WHERE deleted_at IS NULL AND (name LIKE ?1 OR instructions LIKE ?1 OR folder LIKE ?1 OR repo_url LIKE ?1) ORDER BY updated_at DESC LIMIT 8", snip("instructions")),
        &[&like, &text.trim()],
    )?;
    let artifacts = core.db.all("SELECT id, title, kind, project_id, description FROM artifacts WHERE deleted_at IS NULL AND (title LIKE ?1 OR description LIKE ?1) ORDER BY updated_at DESC LIMIT 10", &[&like])?;
    let mem = memory::search(&core, q.get("project").map(String::as_str), &text).unwrap_or_default();
    Ok(Json(json!({ "threads": threads, "messages": messages, "memory": mem, "projects": projects, "artifacts": artifacts })))
}

// ------------------------------------------------------------------------------------------------ memory

async fn list_memory(State(core): State<CoreRef>, q: Q) -> R {
    let project = q.get("project").filter(|p| !p.is_empty()).map(String::as_str);
    if q.get("deleted").is_some() {
        return Ok(Json(json!(core.db.all("SELECT id, project_id, name, description, deleted_at FROM memory WHERE deleted_at IS NOT NULL AND (project_id IS NULL OR project_id = ?1) ORDER BY deleted_at DESC LIMIT 50", &[&project.unwrap_or("")])?)));
    }
    let rows = match q.get("q").filter(|x| !x.trim().is_empty()) {
        Some(text) => memory::search(&core, project, text)?,
        None => memory::list(&core, project)?,
    };
    Ok(Json(json!(rows)))
}

async fn get_memory(State(core): State<CoreRef>, Path(id): Path<String>) -> R {
    Ok(Json(core.db.one("SELECT * FROM memory WHERE id = ?1", &[&id])?.ok_or_else(|| ApiErr(StatusCode::NOT_FOUND, "No such memory".into()))?))
}

async fn save_memory(State(core): State<CoreRef>, Json(b): Json<Value>) -> R {
    let project = b["project_id"].as_str().filter(|p| !p.is_empty());
    // renaming: the old entry goes away
    if let Some(old) = b["id"].as_str() {
        if let Some(prev) = core.db.one("SELECT name, project_id FROM memory WHERE id = ?1", &[&old])? {
            let newname = util::slug(&s(&b, "name"), 64);
            if prev["name"].as_str() != Some(newname.as_str()) || prev["project_id"].as_str() != project {
                let now = util::now_ms();
                core.db.exec("UPDATE memory SET deleted_at = ?1 WHERE id = ?2", &[&now, &old])?;
            }
        }
    }
    Ok(Json(memory::write(&core, project, &s(&b, "name"), &s(&b, "description"), b["body"].as_str().unwrap_or(""), "You")?))
}

async fn delete_memory(State(core): State<CoreRef>, Path(id): Path<String>) -> R {
    let now = util::now_ms();
    core.db.exec("UPDATE memory SET deleted_at = ?1 WHERE id = ?2", &[&now, &id])?;
    core.bus.emit("memory", None, None, json!({ "deleted": id }));
    Ok(Json(json!({ "ok": true })))
}

async fn restore_memory(State(core): State<CoreRef>, Path(id): Path<String>) -> R {
    core.db.exec("UPDATE memory SET deleted_at = NULL WHERE id = ?1", &[&id])?;
    core.bus.emit("memory", None, None, json!({ "restored": id }));
    Ok(Json(json!({ "ok": true })))
}

// ------------------------------------------------------------------------------------------------ artifacts

pub fn artifact_sig(core: &CoreRef, id: &str) -> String {
    util::sha256(&format!("{}:{id}", core.local_token))[..24].to_string()
}

fn with_urls(core: &CoreRef, mut a: Value) -> Value {
    let id = a["id"].as_str().unwrap_or("").to_string();
    a["url"] = json!(format!("/a/{id}/{}/", artifact_sig(core, &id)));
    a
}

async fn list_artifacts(State(core): State<CoreRef>, q: Q) -> R {
    let rows = match q.get("project").filter(|p| !p.is_empty()) {
        Some(p) => core.db.all("SELECT a.*, t.title AS thread_title, t.agent FROM artifacts a LEFT JOIN threads t ON t.id = a.thread_id WHERE a.project_id = ?1 AND a.deleted_at IS NULL ORDER BY a.updated_at DESC", &[p])?,
        None => core.db.all("SELECT a.*, t.title AS thread_title, t.agent, p.name AS project_name FROM artifacts a LEFT JOIN threads t ON t.id = a.thread_id LEFT JOIN projects p ON p.id = a.project_id WHERE a.deleted_at IS NULL ORDER BY a.updated_at DESC LIMIT 300", &[])?,
    };
    Ok(Json(json!(rows.into_iter().map(|a| with_urls(&core, a)).collect::<Vec<_>>())))
}

async fn get_artifact(State(core): State<CoreRef>, Path(id): Path<String>) -> R {
    let a = core.db.one("SELECT a.*, t.title AS thread_title, t.agent FROM artifacts a LEFT JOIN threads t ON t.id = a.thread_id WHERE a.id = ?1", &[&id])?.ok_or_else(|| ApiErr(StatusCode::NOT_FOUND, "No such artifact".into()))?;
    Ok(Json(with_urls(&core, a)))
}

async fn create_artifact(State(core): State<CoreRef>, Json(b): Json<Value>) -> R {
    let mut a = artifacts::publish(&core, b["project_id"].as_str().filter(|p| !p.is_empty()), None, &s(&b, "title"), &s(&b, "description"), &s(&b, "kind"), b["path"].as_str(), b["content"].as_str(), None, None)?;
    if let Some(f) = b["folder_id"].as_str().filter(|f| !f.is_empty()) {
        core.db.exec("UPDATE artifacts SET folder_id = ?1 WHERE id = ?2", &[&f, &a["id"].as_str().unwrap_or("")])?;
        a["folder_id"] = json!(f);
    }
    Ok(Json(with_urls(&core, a)))
}

async fn patch_artifact(State(core): State<CoreRef>, Path(id): Path<String>, Json(b): Json<Value>) -> R {
    if let Some(t) = b["title"].as_str().filter(|t| !t.trim().is_empty()) {
        core.db.exec("UPDATE artifacts SET title = ?1 WHERE id = ?2", &[&t.trim(), &id])?;
    }
    if let Some(d) = b["description"].as_str() {
        core.db.exec("UPDATE artifacts SET description = ?1 WHERE id = ?2", &[&d, &id])?;
    }
    if b["restore"].as_bool() == Some(true) {
        core.db.exec("UPDATE artifacts SET deleted_at = NULL WHERE id = ?1", &[&id])?;
    }
    if let Some(f) = b.get("folder_id") {
        let f = f.as_str().filter(|f| !f.is_empty());
        if let Some(f) = f {
            core.db.one("SELECT id FROM folders WHERE id = ?1 AND deleted_at IS NULL", &[&f])?.ok_or_else(|| bad("That folder is gone."))?;
        }
        core.db.exec("UPDATE artifacts SET folder_id = ?1 WHERE id = ?2", &[&f, &id])?;
    }
    let a = core.db.one("SELECT * FROM artifacts WHERE id = ?1", &[&id])?.unwrap_or_default();
    core.bus.emit("artifact", a["project_id"].as_str(), None, a.clone());
    Ok(Json(with_urls(&core, a)))
}

/// The folder tree. With ?project=, that project's own folder (made if needed) and everything under it.
async fn list_folders(State(core): State<CoreRef>, q: Q) -> R {
    let all = core.db.all(
        "SELECT f.id, f.parent_id, f.project_id, f.name, f.auto_key, f.named_by, f.updated_at,
                (SELECT COUNT(*) FROM artifacts a WHERE a.folder_id = f.id AND a.deleted_at IS NULL) AS count
         FROM folders f WHERE f.deleted_at IS NULL ORDER BY f.auto_key IS NULL, lower(f.name)",
        &[],
    )?;
    let Some(p) = q.get("project").filter(|p| !p.is_empty()) else { return Ok(Json(json!({ "folders": all, "root": null }))) };
    let root = artifacts::root_folder(&core, Some(p))?;
    let keep: std::collections::HashSet<String> = artifacts::subtree(&core, &root).into_iter().collect();
    let mut folders: Vec<Value> = all.into_iter().filter(|f| f["id"].as_str().is_some_and(|i| keep.contains(i))).collect();
    if !folders.iter().any(|f| f["id"] == root.as_str()) {
        folders.extend(core.db.one("SELECT id, parent_id, project_id, name, auto_key, named_by, updated_at, 0 AS count FROM folders WHERE id = ?1", &[&root])?);
    }
    Ok(Json(json!({ "folders": folders, "root": root })))
}

async fn create_folder(State(core): State<CoreRef>, Json(b): Json<Value>) -> R {
    let name = s(&b, "name");
    if name.trim().is_empty() {
        return Err(bad("Give the folder a name."));
    }
    let parent = b["parent_id"].as_str().filter(|p| !p.is_empty());
    let project = match parent {
        Some(p) => core.db.one("SELECT project_id FROM folders WHERE id = ?1 AND deleted_at IS NULL", &[&p])?.ok_or_else(|| bad("That folder is gone."))?["project_id"].as_str().map(str::to_string),
        None => None,
    };
    let name: String = name.trim().chars().take(80).collect();
    if core.db.one("SELECT id FROM folders WHERE IFNULL(parent_id, '') = ?1 AND lower(name) = lower(?2) AND deleted_at IS NULL", &[&parent.unwrap_or(""), &name])?.is_some() {
        return Err(bad(format!("There's already a folder called {name} here.")));
    }
    let id = util::id("fld");
    let now = util::now_ms();
    core.db.exec(
        "INSERT INTO folders(id, parent_id, project_id, name, named_by, created_at, updated_at) VALUES (?1, ?2, ?3, ?4, 'person', ?5, ?5)",
        &[&id, &parent, &project, &name, &now],
    )?;
    core.bus.emit("artifact", project.as_deref(), None, json!({ "folder": id }));
    Ok(Json(core.db.one("SELECT *, 0 AS count FROM folders WHERE id = ?1", &[&id])?.unwrap_or_default()))
}

async fn patch_folder(State(core): State<CoreRef>, Path(id): Path<String>, Json(b): Json<Value>) -> R {
    let f = core.db.one("SELECT * FROM folders WHERE id = ?1", &[&id])?.ok_or_else(|| ApiErr(StatusCode::NOT_FOUND, "No such folder".into()))?;
    let now = util::now_ms();
    if b["restore"].as_bool() == Some(true) {
        // everything deleted with it comes back with it
        if let Some(at) = f["deleted_at"].as_i64() {
            for x in artifacts::subtree(&core, &id) {
                core.db.exec("UPDATE folders SET deleted_at = NULL WHERE id = ?1 AND deleted_at = ?2", &[&x, &at])?;
                core.db.exec("UPDATE artifacts SET deleted_at = NULL WHERE folder_id = ?1 AND deleted_at = ?2", &[&x, &at])?;
            }
        }
    }
    if let Some(n) = b["name"].as_str().map(str::trim).filter(|n| !n.is_empty()) {
        let n: String = n.chars().take(80).collect();
        core.db.exec("UPDATE folders SET name = ?1, named_by = 'person', updated_at = ?2 WHERE id = ?3", &[&n, &now, &id])?;
    }
    if let Some(p) = b.get("parent_id") {
        let p = p.as_str().filter(|p| !p.is_empty());
        if let Some(p) = p {
            if artifacts::is_within(&core, p, &id) {
                return Err(bad("A folder can't go inside itself."));
            }
        }
        core.db.exec("UPDATE folders SET parent_id = ?1, updated_at = ?2 WHERE id = ?3", &[&p, &now, &id])?;
    }
    core.bus.emit("artifact", f["project_id"].as_str(), None, json!({ "folder": id }));
    Ok(Json(core.db.one("SELECT * FROM folders WHERE id = ?1", &[&id])?.unwrap_or_default()))
}

/// Deletes a folder with everything in it; PATCH {restore: true} brings it all back.
async fn delete_folder(State(core): State<CoreRef>, Path(id): Path<String>) -> R {
    let now = util::now_ms();
    let mut n = 0;
    for x in artifacts::subtree(&core, &id) {
        core.db.exec("UPDATE folders SET deleted_at = ?1 WHERE id = ?2 AND deleted_at IS NULL", &[&now, &x])?;
        n += core.db.scalar_i64("SELECT COUNT(*) FROM artifacts WHERE folder_id = ?1 AND deleted_at IS NULL", &[&x])?;
        core.db.exec("UPDATE artifacts SET deleted_at = ?1 WHERE folder_id = ?2 AND deleted_at IS NULL", &[&now, &x])?;
    }
    core.bus.emit("artifact", None, None, json!({ "folder": id, "deleted": true }));
    Ok(Json(json!({ "ok": true, "artifacts": n })))
}

async fn delete_artifact(State(core): State<CoreRef>, Path(id): Path<String>) -> R {
    let now = util::now_ms();
    core.db.exec("UPDATE artifacts SET deleted_at = ?1 WHERE id = ?2", &[&now, &id])?;
    core.bus.emit("artifact", None, None, json!({ "id": id, "deleted": true }));
    Ok(Json(json!({ "ok": true })))
}

/// A page on this machine's localhost, as a link another device can open (served by the server under /fwd/).
async fn forward_url(State(core): State<CoreRef>, Json(b): Json<Value>) -> R {
    let (path, port) = forward::rewrite(&core, &s(&b, "url"))?;
    Ok(Json(json!({ "path": path, "port": port })))
}

async fn artifact_file(State(core): State<CoreRef>, Path(p): Path<HashMap<String, String>>, q: Q) -> Response {
    let id = p.get("id").cloned().unwrap_or_default();
    let sig = p.get("sig").cloned().unwrap_or_default();
    if !util::same(&sig, &artifact_sig(&core, &id)) {
        return StatusCode::NOT_FOUND.into_response();
    }
    let rest = p.get("rest").cloned().unwrap_or_default();
    let Some(path) = artifacts::file(&core, &id, &rest) else { return StatusCode::NOT_FOUND.into_response() };
    let Ok(bytes) = tokio::fs::read(&path).await else { return StatusCode::NOT_FOUND.into_response() };
    let mime = mime_guess::from_path(&path).first_or_octet_stream();
    // the viewer asks for the comment picker: a few lines that let the person point at a part of the page
    let bytes = if q.get("annotate").is_some() && mime.essence_str() == "text/html" { artifacts::with_picker(bytes) } else { bytes };
    let mut r = Response::builder().header(header::CONTENT_TYPE, mime.as_ref()).header(header::CACHE_CONTROL, "no-cache");
    if q.get("download").is_some() {
        let name = path.file_name().map(|n| n.to_string_lossy().to_string()).unwrap_or_else(|| "artifact".into());
        r = r.header(header::CONTENT_DISPOSITION, format!("attachment; filename=\"{}\"", name.replace('"', "")));
    }
    r.body(Body::from(bytes)).unwrap()
}

// ------------------------------------------------------------------------------------------------ agents

async fn list_agents(State(core): State<CoreRef>, q: Q) -> R {
    if q.get("refresh").is_some() {
        let ids: Vec<String> = core.adapters().iter().map(|a| a.id()).collect();
        let futs = ids.iter().map(|id| core.agent_status(id));
        let all = futures_util::future::join_all(futs).await;
        return Ok(Json(json!(all)));
    }
    Ok(Json(json!(core.cached_status())))
}

async fn agent_status(State(core): State<CoreRef>, Path(id): Path<String>) -> R {
    Ok(Json(core.agent_status(&id).await))
}

/// Ask the agent's CLI for its models now.
async fn refresh_agent_models(State(core): State<CoreRef>, Path(id): Path<String>) -> R {
    if core.adapter(&id).is_none() {
        return Err(bad("Unknown agent"));
    }
    Ok(Json(core.agent_status_with(&id, true).await))
}

async fn patch_agent(State(core): State<CoreRef>, Path(id): Path<String>, Json(b): Json<Value>) -> R {
    if core.adapter(&id).is_none() {
        return Err(bad("Unknown agent"));
    }
    let mut patch = json!({ "agents": {} });
    patch["agents"][&id] = b.clone();
    core.patch_settings(&patch)?;
    if b.get("max_parallel").is_some() {
        if let Ok(mut s) = core.slots.lock() {
            s.remove(&id); // takes effect for new turns
        }
    }
    if b.get("path").is_some() {
        return Ok(Json(core.agent_status(&id).await));
    }
    Ok(Json(json!({ "ok": true, "config": core.agent_cfg(&id) })))
}

async fn add_custom_agent(State(core): State<CoreRef>, Json(b): Json<Value>) -> R {
    let mut list = core.settings()["custom_agents"].as_array().cloned().unwrap_or_default();
    let id = util::slug(&s(&b, "id").replace(' ', "-"), 24);
    if ["claude", "codex", "antigravity"].contains(&id.as_str()) {
        return Err(bad("That id belongs to a built-in agent."));
    }
    if s(&b, "command").is_empty() {
        return Err(bad("Give the command that runs the agent."));
    }
    let mut a = b.clone();
    a["id"] = json!(id);
    list.retain(|x| x["id"].as_str() != Some(id.as_str()));
    list.push(a);
    core.patch_settings(&json!({ "custom_agents": list }))?;
    Ok(Json(core.agent_status(&id).await))
}

async fn remove_custom_agent(State(core): State<CoreRef>, Path(id): Path<String>) -> R {
    let mut list = core.settings()["custom_agents"].as_array().cloned().unwrap_or_default();
    list.retain(|x| x["id"].as_str() != Some(id.as_str()));
    core.patch_settings(&json!({ "custom_agents": list }))?;
    Ok(Json(json!({ "ok": true })))
}

/// A tiny real turn: proves the CLI runs, is signed in and can talk back.
async fn test_agent(State(core): State<CoreRef>, Path(id): Path<String>) -> R {
    let a = core.adapter(&id).ok_or_else(|| bad("Unknown agent"))?;
    let prog = core.locate(&id).ok_or_else(|| bad(format!("{} isn't installed.", a.info().name)))?;
    let dir = core.data.join("scratch").join(format!("test-{id}"));
    std::fs::create_dir_all(&dir)?;
    let cfg = core.agent_cfg(&id);
    let spec = agents::TurnSpec {
        prompt: "Reply with exactly the words: ROAMER READY".into(),
        system: String::new(),
        session: None,
        cwd: dir.clone(),
        model: cfg["model"].as_str().unwrap_or("").into(),
        effort: String::new(),
        autonomy: agents::Autonomy::ReadOnly,
        mcp: None,
        extra_dirs: vec![],
        scratch: dir.clone(),
        extra_args: vec![],
        headless: core.headless,
        lead: false,
    };
    let mut launch = a.launch(&spec)?;
    launch.pty = launch.pty || a.needs_pty();
    let started = std::time::Instant::now();
    let mut stream = procs::stream(&prog, &launch)?;
    let mut st = json!({});
    let (mut text, mut errs, mut raw, mut limit) = (String::new(), vec![], vec![], None);
    let deadline = tokio::time::Instant::now() + Duration::from_secs(150);
    loop {
        let l = tokio::select! { l = stream.rx.recv() => l, _ = tokio::time::sleep_until(deadline) => { stream.kill().await; errs.push("No answer within 150 seconds".to_string()); break; } };
        match l {
            None | Some(procs::Line::Exit(_)) => break,
            Some(procs::Line::Err(e)) => {
                if !e.trim().is_empty() {
                    errs.push(e)
                }
            }
            Some(procs::Line::Out(o)) => {
                for ev in a.parse(&o, &mut st) {
                    match ev {
                        agents::AgentEvent::Delta(d) => text.push_str(&d),
                        agents::AgentEvent::Text(t) | agents::AgentEvent::Final(t) => text = t,
                        agents::AgentEvent::Error(e) => errs.push(e),
                        agents::AgentEvent::Limit { message, .. } => limit = Some(message),
                        agents::AgentEvent::Raw(r) => raw.push(r),
                        _ => {}
                    }
                }
            }
        }
    }
    if text.trim().is_empty() && a.raw_is_text() {
        text = raw.join("\n");
    }
    let ok = text.to_uppercase().contains("ROAMER READY");
    let v = json!({
        "ok": ok,
        "reply": util::clip(text.trim(), 400),
        "limit": limit,
        "error": if ok { Value::Null } else { json!(util::clip(&errs.iter().rev().take(6).rev().cloned().collect::<Vec<_>>().join("\n"), 1500)) },
        "seconds": started.elapsed().as_secs(),
    });
    let _ = core.agent_status(&id).await;
    Ok(Json(v))
}

async fn clear_limit(State(core): State<CoreRef>, Path(id): Path<String>) -> R {
    core.db.exec("DELETE FROM limits WHERE agent = ?1", &[&id])?;
    core.bus.emit("limits", None, None, json!({ "agent": id }));
    let limited = core.db.all("SELECT id FROM threads WHERE status = 'limited' AND agent = ?1", &[&id])?;
    for t in limited {
        let tid = t["id"].as_str().unwrap_or("");
        core.set_status(tid, "idle", "")?;
        runner::schedule(&core, tid);
    }
    Ok(Json(json!({ "ok": true })))
}

async fn setup_status(State(core): State<CoreRef>) -> R {
    Ok(Json(setup::status(&core).await))
}

// ------------------------------------------------------------------------------------------------ terminals

async fn list_terms(State(core): State<CoreRef>) -> R {
    let t = core.terms.lock().map(|m| m.iter().map(|(k, v)| json!({ "id": k, "label": v.label, "agent": v.agent, "exited": v.exited, "started": v.started })).collect::<Vec<_>>()).unwrap_or_default();
    Ok(Json(json!(t)))
}

/// Start an install, sign-in, update or tool step in a terminal the UI shows. No terminal window needed.
async fn start_term(State(core): State<CoreRef>, Json(b): Json<Value>) -> R {
    crate::os::refresh_path();
    let what = s(&b, "what");
    let agent = s(&b, "agent");
    let idx = b["step"].as_u64().unwrap_or(0) as usize;
    let step = setup::step_for(&core, &what, &agent, idx).ok_or_else(|| bad("Nothing to run for that."))?;
    for need in &step.needs {
        if procs::resolve(&[need.as_str(), &format!("{need}.cmd"), &format!("{need}.exe")], None).is_none() {
            let hint = setup::hint_for(need);
            return Err(bad(format!("This needs {need}, which isn't installed yet. {hint}")));
        }
    }
    let prog = procs::resolve(&[step.cmd[0].as_str()], None).ok_or_else(|| bad(format!("Can't find {} on this machine.", step.cmd[0])))?;
    let mut env = vec![];
    if core.headless {
        env.push(("NO_BROWSER".to_string(), "true".to_string()));
        env.push(("BROWSER".into(), "echo".into()));
    }
    let launch = procs::Launch { args: step.cmd[1..].to_vec(), env, cwd: Some(crate::os::home()), ..Default::default() };
    let id = util::id("term");
    let cols = b["cols"].as_u64().unwrap_or(110) as u16;
    let rows = b["rows"].as_u64().unwrap_or(30) as u16;
    let (c1, c2) = (core.clone(), core.clone());
    let (i1, i2) = (id.clone(), id.clone());
    let agent2 = agent.clone();
    let what2 = what.clone();
    let term = procs::terminal(
        &prog,
        &launch,
        cols,
        rows,
        move |data| {
            if let Ok(mut m) = c1.terms.lock() {
                if let Some(e) = m.get_mut(&i1) {
                    e.buffer.extend_from_slice(&data);
                    let over = e.buffer.len().saturating_sub(512 * 1024);
                    if over > 0 {
                        e.buffer.drain(..over);
                    }
                }
            }
            c1.bus.emit("term", None, None, json!({ "id": i1, "data": base64::engine::general_purpose::STANDARD.encode(&data) }));
        },
        move |code| {
            if let Ok(mut m) = c2.terms.lock() {
                if let Some(e) = m.get_mut(&i2) {
                    e.exited = Some(code);
                }
            }
            c2.bus.emit("term", None, None, json!({ "id": i2, "exit": code }));
            // whatever was installed or signed in, show its new state
            let c3 = c2.clone();
            tokio::runtime::Handle::try_current().ok().map(|h| {
                h.spawn(async move {
                    crate::os::refresh_path();
                    if !agent2.is_empty() && what2 != "tool" {
                        let _ = c3.agent_status(&agent2).await;
                    }
                    c3.bus.emit("setup", None, None, json!({}));
                })
            });
        },
    )?;
    core.terms.lock().unwrap_or_else(|p| p.into_inner()).insert(
        id.clone(),
        TermEntry { term, label: step.label.clone(), agent: agent.clone(), buffer: vec![], exited: None, started: util::now_ms() },
    );
    Ok(Json(json!({ "id": id, "label": step.label, "cmd": step.cmd })))
}

async fn term_buffer(State(core): State<CoreRef>, Path(id): Path<String>) -> R {
    let m = core.terms.lock().unwrap_or_else(|p| p.into_inner());
    let e = m.get(&id).ok_or_else(|| ApiErr(StatusCode::NOT_FOUND, "No such terminal".into()))?;
    Ok(Json(json!({ "id": id, "label": e.label, "data": base64::engine::general_purpose::STANDARD.encode(&e.buffer), "exited": e.exited })))
}

async fn term_input(State(core): State<CoreRef>, Path(id): Path<String>, Json(b): Json<Value>) -> R {
    let t = core.terms.lock().ok().and_then(|m| m.get(&id).map(|e| e.term.clone())).ok_or_else(|| ApiErr(StatusCode::NOT_FOUND, "No such terminal".into()))?;
    // keystrokes go through exactly as typed (no trimming: spaces and Enter matter)
    t.write(b["data"].as_str().unwrap_or("").as_bytes());
    Ok(Json(json!({ "ok": true })))
}

async fn term_resize(State(core): State<CoreRef>, Path(id): Path<String>, Json(b): Json<Value>) -> R {
    if let Some(t) = core.terms.lock().ok().and_then(|m| m.get(&id).map(|e| e.term.clone())) {
        t.resize(b["cols"].as_u64().unwrap_or(100) as u16, b["rows"].as_u64().unwrap_or(30) as u16);
    }
    Ok(Json(json!({ "ok": true })))
}

async fn stop_term(State(core): State<CoreRef>, Path(id): Path<String>) -> R {
    if let Some(e) = core.terms.lock().ok().and_then(|mut m| m.remove(&id)) {
        e.term.kill();
    }
    Ok(Json(json!({ "ok": true })))
}

// ------------------------------------------------------------------------------------------------ usage

async fn usage(State(core): State<CoreRef>, q: Q) -> R {
    let days: i64 = q.get("days").and_then(|d| d.parse().ok()).unwrap_or(14).clamp(1, 120);
    let since = util::now_ms() - days * 86_400_000;
    let tz = chrono::Local::now().offset().local_minus_utc() as i64 * 1000;
    let per_day = core.db.all(
        "SELECT agent, ((started_at + ?2) / 86400000) AS day, COUNT(*) AS turns, SUM(input_tokens) AS input, SUM(output_tokens) AS output, SUM(cached_tokens) AS cached,
                SUM(cost_usd) AS cost, SUM(status = 'failed') AS failed, SUM(status = 'limited') AS limited, SUM(COALESCE(ended_at, started_at) - started_at) AS ms
         FROM turns WHERE started_at >= ?1 GROUP BY agent, day ORDER BY day",
        &[&since, &tz],
    )?;
    let totals = core.db.all(
        "SELECT agent, COUNT(*) AS turns, SUM(input_tokens) AS input, SUM(output_tokens) AS output, SUM(cost_usd) AS cost, SUM(status = 'failed') AS failed, SUM(status = 'limited') AS limited,
                MAX(started_at) AS last FROM turns WHERE started_at >= ?1 GROUP BY agent",
        &[&since],
    )?;
    let limits = core.db.all("SELECT * FROM limits ORDER BY hit_at DESC", &[])?;
    let recent = core.db.all(
        "SELECT tu.id, tu.agent, tu.model, tu.status, tu.error, tu.input_tokens, tu.output_tokens, tu.cost_usd, tu.started_at, tu.ended_at, t.title, t.id AS thread_id, t.project_id
         FROM turns tu LEFT JOIN threads t ON t.id = tu.thread_id ORDER BY tu.started_at DESC LIMIT 40",
        &[],
    )?;
    Ok(Json(json!({ "days": days, "per_day": per_day, "totals": totals, "limits": limits, "recent": recent, "agents": core.cached_status(), "tz_offset_ms": tz })))
}

// ------------------------------------------------------------------------------------------------ GitHub

async fn github_status(State(core): State<CoreRef>) -> R {
    Ok(Json(github::whoami(&core).await))
}

async fn github_token(State(core): State<CoreRef>, Json(b): Json<Value>) -> R {
    core.patch_settings(&json!({ "github_token": s(&b, "token") }))?;
    Ok(Json(github::whoami(&core).await))
}

async fn github_repos(State(core): State<CoreRef>) -> R {
    Ok(Json(github::my_repos(&core).await?))
}

// ------------------------------------------------------------------------------------------------ files

async fn fs_list(State(_core): State<CoreRef>, q: Q) -> R {
    let path = q.get("path").filter(|p| !p.trim().is_empty()).map(PathBuf::from);
    let Some(dir) = path else {
        // the starting points
        let home = crate::os::home();
        let mut roots = vec![json!({ "name": "Home", "path": home.display().to_string() })];
        for sub in ["Documents", "Desktop", "Projects", "Code", "src", "dev", "repos", "GitHub", "Github"] {
            let p = home.join(sub);
            if p.is_dir() {
                roots.push(json!({ "name": sub, "path": p.display().to_string() }));
            }
        }
        if cfg!(windows) {
            for l in b'A'..=b'Z' {
                let d = format!("{}:\\", l as char);
                if PathBuf::from(&d).is_dir() {
                    roots.push(json!({ "name": d.clone(), "path": d }));
                }
            }
        } else {
            roots.push(json!({ "name": "/", "path": "/" }));
        }
        return Ok(Json(json!({ "path": "", "roots": roots, "entries": [] })));
    };
    let mut entries = vec![];
    let rd = std::fs::read_dir(&dir).map_err(|e| bad(format!("Can't open {}: {e}", dir.display())))?;
    for e in rd.flatten().take(3000) {
        let name = e.file_name().to_string_lossy().to_string();
        let is_dir = e.file_type().map(|t| t.is_dir()).unwrap_or(false);
        let hidden = name.starts_with('.');
        entries.push(json!({ "name": name, "dir": is_dir, "hidden": hidden, "git": is_dir && e.path().join(".git").exists() }));
    }
    entries.sort_by(|a, b| b["dir"].as_bool().cmp(&a["dir"].as_bool()).then(a["name"].as_str().unwrap_or("").to_lowercase().cmp(&b["name"].as_str().unwrap_or("").to_lowercase())));
    Ok(Json(json!({
        "path": dir.display().to_string(),
        "parent": dir.parent().map(|p| p.display().to_string()),
        "sep": std::path::MAIN_SEPARATOR.to_string(),
        "git": dir.join(".git").exists(),
        "entries": entries
    })))
}

async fn fs_mkdir(Json(b): Json<Value>) -> R {
    let p = PathBuf::from(s(&b, "path"));
    if !p.is_absolute() {
        return Err(bad("Give a full path."));
    }
    std::fs::create_dir_all(&p)?;
    Ok(Json(json!({ "path": p.display().to_string() })))
}

async fn fs_reveal(Extension(who): Extension<Who>, Json(b): Json<Value>) -> R {
    if !who.is_local() {
        return Err(bad("Folders open on the server's own screen; that only works from the server itself."));
    }
    Ok(Json(json!({ "ok": crate::os::reveal(&PathBuf::from(s(&b, "path"))) })))
}

async fn upload(State(core): State<CoreRef>, q: Q, body: Bytes) -> R {
    let name = q.get("name").cloned().unwrap_or_else(|| "file".into());
    let clean: String = name.chars().map(|c| if c.is_control() || r#"<>:"/\|?*"#.contains(c) { '_' } else { c }).collect();
    let dir = core.data.join("uploads").join(util::id("up"));
    std::fs::create_dir_all(&dir)?;
    let p = dir.join(if clean.trim().is_empty() { "file".to_string() } else { clean });
    tokio::fs::write(&p, &body).await?;
    Ok(Json(json!({ "path": p.display().to_string(), "size": body.len() })))
}

// ------------------------------------------------------------------------------------------------ remote access

async fn network(State(core): State<CoreRef>) -> R {
    let st = core.settings();
    let port = st["server"]["port"].as_u64().unwrap_or(4630) as u16;
    let tls = !st["server"]["tls_cert"].as_str().unwrap_or("").is_empty();
    let mut v = net::addresses(&core, port, tls).await;
    let at = core.sharing.lock().ok().and_then(|s| s.as_ref().map(|(a, _)| a.clone()));
    v["listening"] = json!(at.is_some());
    v["listening_at"] = json!(at);
    v["port"] = json!(port);
    v["tls"] = json!(tls);
    v["local_port"] = json!(core.port.load(std::sync::atomic::Ordering::Relaxed));
    Ok(Json(v))
}

async fn tailscale_serve(State(core): State<CoreRef>, Json(b): Json<Value>) -> R {
    let port = core.settings()["server"]["port"].as_u64().unwrap_or(4630) as u16;
    let out = net::tailscale_serve(port, b["on"].as_bool().unwrap_or(true)).await?;
    Ok(Json(json!({ "ok": true, "output": out, "tailscale": net::tailscale().await })))
}

async fn list_connections(State(core): State<CoreRef>, Extension(who): Extension<Who>) -> R {
    if !who.is_local() {
        return Ok(Json(json!([])));
    }
    Ok(Json(json!(core.db.all("SELECT id, name, url, via, created_at, last_used FROM connections ORDER BY last_used DESC", &[])?)))
}

fn normalize_url(u: &str) -> String {
    let u = u.trim().trim_end_matches('/');
    let with_scheme = if u.starts_with("http://") || u.starts_with("https://") { u.to_string() } else { format!("http://{u}") };
    // a bare host gets the default port
    let after = with_scheme.split("://").nth(1).unwrap_or("");
    let host_part = after.split('/').next().unwrap_or("");
    let has_port = host_part.rsplit_once(':').map(|(_, p)| p.chars().all(|c| c.is_ascii_digit())).unwrap_or(false) && !host_part.ends_with(']');
    if !has_port && with_scheme.starts_with("http://") {
        format!("{with_scheme}:4630")
    } else {
        with_scheme
    }
}

async fn probe(url: &str) -> anyhow::Result<Value> {
    let c = reqwest::Client::builder().connect_timeout(Duration::from_secs(6)).timeout(Duration::from_secs(10)).build()?;
    let v: Value = c.get(format!("{url}/api/hello")).send().await?.error_for_status()?.json().await?;
    if v["app"] != "model-roamer" {
        anyhow::bail!("Something answered at {url}, but it isn't Model Roamer.");
    }
    Ok(v)
}

async fn test_connection(Json(b): Json<Value>) -> R {
    let url = normalize_url(&s(&b, "url"));
    match probe(&url).await {
        Ok(v) => Ok(Json(json!({ "ok": true, "url": url, "server": v }))),
        Err(e) => Ok(Json(json!({ "ok": false, "url": url, "error": format!("Couldn't reach {url}: {e}") }))),
    }
}

/// Pair this computer with a server and remember it.
async fn add_connection(State(core): State<CoreRef>, Extension(who): Extension<Who>, Json(b): Json<Value>) -> R {
    if !who.is_local() {
        return Err(bad("Connections are kept per computer; add them from the computer itself."));
    }
    let url = normalize_url(&s(&b, "url"));
    let hello = probe(&url).await.map_err(|e| bad(format!("Couldn't reach {url}: {e}")))?;
    if hello["id"] == json!(core.instance_id) {
        return Err(bad("That's this computer's own Model Roamer."));
    }
    let c = reqwest::Client::builder().timeout(Duration::from_secs(15)).build()?;
    let r = c.post(format!("{url}/api/pair")).json(&json!({ "code": s(&b, "code"), "name": format!("{} (desktop)", crate::os::machine_name()) })).send().await?;
    let st = r.status();
    let v: Value = r.json().await.unwrap_or_default();
    if !st.is_success() {
        return Err(bad(v["error"].as_str().unwrap_or("Pairing failed").to_string()));
    }
    let id = util::id("con");
    let now = util::now_ms();
    let name = if s(&b, "name").is_empty() { hello["name"].as_str().unwrap_or("Server").to_string() } else { s(&b, "name") };
    let via = if s(&b, "via").is_empty() { guess_via(&url) } else { s(&b, "via") };
    core.db.exec("DELETE FROM connections WHERE url = ?1", &[&url])?;
    core.db.exec("INSERT INTO connections(id, name, url, token, via, created_at, last_used) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?6)", &[&id, &name, &url, &s(&v, "token"), &via, &now])?;
    Ok(Json(json!({ "id": id, "name": name, "url": url, "via": via })))
}

fn guess_via(url: &str) -> String {
    let host = url.split("://").nth(1).unwrap_or("").split(['/', ':']).next().unwrap_or("");
    if host.ends_with(".ts.net") || host.starts_with("100.") && host.split('.').nth(1).and_then(|o| o.parse::<u8>().ok()).map(|o| (64..128).contains(&o)).unwrap_or(false) {
        "tailscale".into()
    } else if host.starts_with("10.") || host.starts_with("192.168.") || host.starts_with("172.") || host.ends_with(".local") || host.ends_with(".lan") {
        "lan".into()
    } else if host.contains('.') && host.chars().any(|c| c.is_ascii_alphabetic()) {
        "domain".into()
    } else {
        "host".into()
    }
}

async fn remove_connection(State(core): State<CoreRef>, Path(id): Path<String>) -> R {
    core.db.exec("DELETE FROM connections WHERE id = ?1", &[&id])?;
    Ok(Json(json!({ "ok": true })))
}

/// The address a window should load to switch to this server (signs the window in on the way).
async fn open_connection(State(core): State<CoreRef>, Extension(who): Extension<Who>, Path(id): Path<String>) -> R {
    if !who.is_local() {
        return Err(bad("Only this computer's own windows can switch servers."));
    }
    let c = core.db.one("SELECT * FROM connections WHERE id = ?1", &[&id])?.ok_or_else(|| bad("No such connection"))?;
    let url = s(&c, "url");
    probe(&url).await.map_err(|e| bad(format!("{} isn't answering right now: {e}", s(&c, "name"))))?;
    let now = util::now_ms();
    core.db.exec("UPDATE connections SET last_used = ?1 WHERE id = ?2", &[&now, &id])?;
    let enc = percent_encoding::utf8_percent_encode(&s(&c, "token"), percent_encoding::NON_ALPHANUMERIC).to_string();
    Ok(Json(json!({ "url": format!("{url}/auth?token={enc}&next=/"), "origin": url, "name": s(&c, "name") })))
}

async fn discover(State(core): State<CoreRef>) -> R {
    let mut found = net::discover(Duration::from_millis(3500)).await;
    found.retain(|f| f["id"] != json!(core.instance_id));
    Ok(Json(json!(found)))
}

// ------------------------------------------------------------------------------------------------ app

/// Things only the desktop shell can do (open a window, the local URL, quit), asked for over the local API.
async fn desktop_cmd(State(core): State<CoreRef>, Extension(who): Extension<Who>, Json(b): Json<Value>) -> R {
    if !who.is_local() {
        return Err(bad("Only for this computer's own windows."));
    }
    let ok = core.tell_desktop(b);
    Ok(Json(json!({ "ok": ok })))
}

async fn suite() -> R {
    let c = reqwest::Client::builder().timeout(Duration::from_millis(1200)).build()?;
    let board = c.get("http://127.0.0.1:4621/").send().await.map(|r| r.status().is_success()).unwrap_or(false);
    Ok(Json(json!({
        "apps": [
            { "id": "board-roamer", "name": "Board Roamer", "about": "A stylish multi-booru board viewer.", "repo": "https://github.com/Tox-EyE-CeaZY/Board-Roamer", "running": board, "open": if board { json!("http://127.0.0.1:4621/") } else { Value::Null } },
            { "id": "web-roamer", "name": "Web Roamer", "about": "A stylish little web browser.", "repo": "https://github.com/Tox-EyE-CeaZY/Web-Roamer", "running": Value::Null, "open": Value::Null },
            { "id": "model-roamer", "name": "Model Roamer", "about": "This app: an AI orchestration studio.", "repo": "https://github.com/Tox-EyE-CeaZY/Model-Roamer", "running": true, "open": Value::Null }
        ]
    })))
}

async fn update_check() -> R {
    Ok(Json(crate::update::check().await))
}

async fn update_do(State(core): State<CoreRef>, Extension(who): Extension<Who>, Json(b): Json<Value>) -> R {
    match s(&b, "action").as_str() {
        "download" => {
            let c = core.clone();
            tokio::spawn(async move {
                crate::update::download(|v| c.bus.emit("update", None, None, v)).await;
            });
            Ok(Json(json!({ "ok": true })))
        }
        "apply" => {
            if !who.is_local() && !core.headless {
                return Err(bad("Restart from the computer itself."));
            }
            let ok = crate::update::apply();
            if ok {
                core.tell_desktop(json!({ "cmd": "quit" }));
                if core.headless {
                    tokio::spawn(async {
                        tokio::time::sleep(Duration::from_millis(500)).await;
                        std::process::exit(0);
                    });
                }
            }
            Ok(Json(json!({ "ok": ok })))
        }
        _ => Err(bad("action must be download or apply")),
    }
}

async fn logs(State(core): State<CoreRef>, q: Q) -> R {
    if let Some(turn) = q.get("turn") {
        if turn.chars().all(|c| c.is_ascii_alphanumeric() || c == '_') {
            let p = core.data.join("logs").join("turns").join(format!("{turn}.log"));
            let text = std::fs::read_to_string(p).unwrap_or_else(|_| "No log for that turn.".into());
            return Ok(Json(json!({ "text": util::clip(&text, 400_000) })));
        }
    }
    let dir = core.data.join("logs");
    let mut newest: Option<(std::time::SystemTime, PathBuf)> = None;
    for e in std::fs::read_dir(&dir).into_iter().flatten().flatten() {
        if e.file_name().to_string_lossy().starts_with("model-roamer.log") {
            if let Ok(m) = e.metadata().and_then(|m| m.modified()) {
                if newest.as_ref().map(|n| m > n.0).unwrap_or(true) {
                    newest = Some((m, e.path()));
                }
            }
        }
    }
    let text = newest.and_then(|(_, p)| std::fs::read_to_string(p).ok()).unwrap_or_default();
    let tail: Vec<&str> = text.lines().rev().take(600).collect::<Vec<_>>().into_iter().rev().collect();
    Ok(Json(json!({ "text": tail.join("\n"), "dir": dir.display().to_string() })))
}
