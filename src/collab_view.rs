//! Host and guest view routes that let a joined project behave as a real project in the UI,
//! with every permission enforced server-side.

use crate::{
    artifacts,
    collab::{self, Member},
    collab_bridge, collab_chat,
    core::{Core, CoreRef},
    files, git, github, repos,
    server::{self, bad, ApiErr, Who, R},
    util,
};
use axum::{
    body::Body,
    extract::{Path, Query, State},
    http::{header, HeaderValue, StatusCode},
    middleware,
    response::{IntoResponse, Response},
    routing::{get, post},
    Extension, Json, Router,
};
use serde_json::{json, Value};
use std::{collections::HashMap, path::PathBuf, time::Duration};

type Q = Query<HashMap<String, String>>;

pub fn router(core: CoreRef) -> Router {
    Router::new()
        // Host-side member routes (authenticated via member Bearer token in server::auth)
        .route("/api/collab/host/view/threads", get(h_view_threads))
        .route("/api/collab/host/folders", get(h_folders))
        .route("/api/collab/host/artifacts-list", get(h_artifacts_list))
        .route("/api/collab/host/artifacts/{aid}", get(h_artifact_get))
        .route("/api/collab/host/view/artifacts", post(h_view_publish_artifact))
        // Guest-side view & proxy routes (authenticated via local/device token)
        .route("/api/collab/links/{id}/view/state", get(g_state))
        .route("/api/collab/links/{id}/view/ensure", post(g_ensure))
        .route("/api/collab/links/{id}/view/refresh", post(g_refresh))
        .route("/api/collab/links/{id}/view/files", get(g_files_list))
        .route("/api/collab/links/{id}/view/file", get(g_files_read))
        .route("/api/collab/links/{id}/view/git", get(g_git))
        .route("/api/collab/links/{id}/view/prs", get(g_prs))
        .route("/api/collab/links/{id}/view/prs/{number}/merge", post(g_pr_merge))
        .route("/api/collab/links/{id}/artifact-file/{aid}", get(g_artifact_file_root))
        .route("/api/collab/links/{id}/artifact-file/{aid}/", get(g_artifact_file_root))
        .route("/api/collab/links/{id}/artifact-file/{aid}/{*rest}", get(g_artifact_file_rest))
        .layer(axum::extract::DefaultBodyLimit::max(4 * 1024 * 1024))
        .layer(middleware::from_fn_with_state(core.clone(), server::auth))
        .layer(middleware::from_fn(server::headers))
        .with_state(core)
}

fn s(v: &Value, k: &str) -> String {
    v[k].as_str().unwrap_or("").trim().to_string()
}

fn forbid(msg: impl Into<String>) -> ApiErr {
    ApiErr(StatusCode::FORBIDDEN, msg.into())
}

// ================================================================================================ host side

async fn h_view_threads(State(core): State<CoreRef>, Extension(m): Extension<Member>) -> R {
    m.need("view_threads")?;
    Ok(Json(json!({
        "threads": collab::threads_of(&core, &m, 0)?,
        "people": collab::people(&core, &m.project_id),
    })))
}

async fn h_folders(State(core): State<CoreRef>, Extension(m): Extension<Member>) -> R {
    let root = artifacts::root_folder(&core, Some(&m.project_id))?;
    let keep: std::collections::HashSet<String> = artifacts::subtree(&core, &root).into_iter().collect();
    let all = core.db.all(
        "SELECT f.id, f.parent_id, f.project_id, f.name, f.auto_key, f.updated_at,
                (SELECT COUNT(*) FROM artifacts a WHERE a.folder_id = f.id AND a.deleted_at IS NULL) AS count
         FROM folders f WHERE f.deleted_at IS NULL ORDER BY f.auto_key IS NULL, lower(f.name)",
        &[],
    )?;
    let mut folders: Vec<Value> = all
        .into_iter()
        .filter(|f| f["id"].as_str().is_some_and(|i| keep.contains(i)))
        .filter(|f| {
            // Do not expose folder names derived from threads the member cannot see.
            if let Some(tid) = f["auto_key"].as_str().and_then(|k| k.strip_prefix("thread:")) {
                if let Ok(t) = core.thread(tid) {
                    return collab::can_see(&m, &t);
                }
            }
            true
        })
        .collect();
    if !folders.iter().any(|f| f["id"] == root.as_str()) {
        folders.extend(core.db.one(
            "SELECT id, parent_id, project_id, name, auto_key, updated_at, 0 AS count FROM folders WHERE id = ?1",
            &[&root],
        )?);
    }
    Ok(Json(json!({ "folders": folders, "root": root })))
}

fn redact_artifact_row(core: &CoreRef, m: &Member, mut a: Value) -> Value {
    let tid = s(&a, "thread_id");
    if !tid.is_empty() {
        let visible = core.thread(&tid).ok().is_some_and(|t| collab::can_see(m, &t));
        if !visible {
            a["thread_id"] = Value::Null;
            a["thread_title"] = Value::Null;
            a["agent"] = Value::Null;
        }
    }
    let id = s(&a, "id");
    a["url"] = json!(collab_bridge::artifact_link(core, m, &id));
    a
}

async fn h_artifacts_list(State(core): State<CoreRef>, Extension(m): Extension<Member>) -> R {
    let rows = core.db.all(
        "SELECT a.id, a.project_id, a.thread_id, a.folder_id, a.title, a.kind, a.entry, a.description, a.version, a.created_at, a.updated_at,
                t.title AS thread_title, t.agent
         FROM artifacts a LEFT JOIN threads t ON t.id = a.thread_id
         WHERE a.project_id = ?1 AND a.deleted_at IS NULL
         ORDER BY a.updated_at DESC LIMIT 300",
        &[&m.project_id],
    )?;
    let out: Vec<Value> = rows.into_iter().map(|a| redact_artifact_row(&core, &m, a)).collect();
    Ok(Json(json!(out)))
}

async fn h_artifact_get(State(core): State<CoreRef>, Extension(m): Extension<Member>, Path(aid): Path<String>) -> R {
    let row = core
        .db
        .one(
            "SELECT a.id, a.project_id, a.thread_id, a.folder_id, a.title, a.kind, a.entry, a.description, a.version, a.created_at, a.updated_at,
                    t.title AS thread_title, t.agent
             FROM artifacts a LEFT JOIN threads t ON t.id = a.thread_id
             WHERE a.id = ?1 AND a.project_id = ?2 AND a.deleted_at IS NULL",
            &[&aid, &m.project_id],
        )?
        .ok_or_else(|| ApiErr(StatusCode::NOT_FOUND, "No such artifact".into()))?;
    Ok(Json(redact_artifact_row(&core, &m, row)))
}

async fn h_view_publish_artifact(State(core): State<CoreRef>, Extension(m): Extension<Member>, Json(b): Json<Value>) -> R {
    m.need("publish_artifacts")?;
    let kind = if s(&b, "kind").is_empty() { "markdown".to_string() } else { s(&b, "kind") };
    if !matches!(kind.as_str(), "markdown" | "html" | "text" | "svg") {
        return Err(bad("Members publish markdown, html, text or svg content."));
    }
    let content = b["content"].as_str().ok_or_else(|| bad("content is required"))?;
    if b.get("path").is_some() {
        return Err(bad("Publish shared artifacts with inline content."));
    }
    let folder = b["folder"].as_str().filter(|p| !p.trim().is_empty());
    if folder.is_some_and(|p| !collab_bridge::relative_path(p.trim())) {
        return Err(bad("Use a folder inside the shared project."));
    }
    let folder_id = b["folder_id"].as_str().filter(|f| !f.trim().is_empty());
    if let Some(fid) = folder_id {
        let root = artifacts::root_folder(&core, Some(&m.project_id))?;
        let keep = artifacts::subtree(&core, &root);
        if !keep.iter().any(|x| x == fid) {
            return Err(forbid("That folder isn't inside this shared project."));
        }
    }
    let mut a = artifacts::publish(
        &core,
        Some(m.project_id.as_str()),
        None,
        &s(&b, "title"),
        &s(&b, "description"),
        &kind,
        None,
        Some(content),
        None,
        folder,
    )?;
    if let Some(fid) = folder_id {
        let aid = s(&a, "id");
        core.db.exec("UPDATE artifacts SET folder_id = ?1 WHERE id = ?2", &[&fid, &aid])?;
        a["folder_id"] = json!(fid);
    }
    let picked = collab::pick(
        &a,
        &["id", "project_id", "thread_id", "folder_id", "title", "kind", "entry", "description", "version", "created_at", "updated_at"],
    );
    Ok(Json(redact_artifact_row(&core, &m, picked)))
}

// ================================================================================================ guest side

fn load_link(core: &Core, id: &str) -> Result<Value, ApiErr> {
    core.db
        .one("SELECT * FROM collab_links WHERE id = ?1", &[&id])?
        .ok_or_else(|| ApiErr(StatusCode::NOT_FOUND, "No such joined project".into()))
}

fn require_active_link(core: &Core, id: &str) -> Result<Value, ApiErr> {
    let l = load_link(core, id)?;
    if l["status"] == "revoked" {
        return Err(forbid("The owner removed you from this project (or you left it). Ask them for a new invite."));
    }
    Ok(l)
}

fn sanitize_link(l: &Value) -> Value {
    let mut v = collab::pick(
        l,
        &[
            "id", "host_url", "host_id", "host_name", "project_id", "project_name", "member_id",
            "user_id", "name", "role", "perms", "status", "error", "created_at", "last_sync",
        ],
    );
    v["insecure"] = json!(l["host_url"].as_str().is_some_and(|u| u.starts_with("http://") && !is_private_http(u)));
    v
}

fn is_private_http(url: &str) -> bool {
    let Some(rest) = url.strip_prefix("http://") else { return true };
    let host = rest.split('/').next().unwrap_or("");
    let host = host.rsplit_once(':').map(|(h, _)| h).unwrap_or(host).trim_matches(['[', ']']);
    if host == "localhost" || host.ends_with(".local") || host.ends_with(".lan") || host.ends_with(".ts.net") || !host.contains('.') && host.parse::<std::net::IpAddr>().is_err() {
        return true;
    }
    match host.parse::<std::net::IpAddr>() {
        Ok(std::net::IpAddr::V4(v)) => v.is_private() || v.is_loopback() || (v.octets()[0] == 100 && (64..128).contains(&v.octets()[1])),
        Ok(std::net::IpAddr::V6(v)) => v.is_loopback() || (v.segments()[0] & 0xfe00) == 0xfc00,
        Err(_) => false,
    }
}

/// Whether this joined project has a real local working tree or a permitted remote git source on the guest.
async fn source_caps(core: &Core, id: &str) -> (bool, bool, bool) {
    let Ok(p) = core.project(id) else {
        return (false, false, false);
    };
    let folder = s(&p, "folder");
    let repo_url = s(&p, "repo_url");
    let home = core.project_home(&p);
    let extra_present = repos::extras(core, id)
        .unwrap_or_default()
        .into_iter()
        .any(|r| {
            let rf = s(&r, "folder");
            !rf.is_empty() && PathBuf::from(&rf).is_dir()
        });
    let dir_non_empty = home.is_dir() && std::fs::read_dir(&home).ok().is_some_and(|mut d| d.next().is_some());
    let has_local_source = (!folder.is_empty() && PathBuf::from(&folder).is_dir())
        || (!repo_url.is_empty() && dir_non_empty)
        || extra_present;
    let is_git = has_local_source && git::is_repo(&home).await;
    let has_git_source = is_git || !repo_url.is_empty();
    (true, has_local_source, has_git_source)
}

fn perm_bool(link: &Value, key: &str) -> bool {
    link["perms"][key] == true
}

async fn g_state(State(core): State<CoreRef>, Extension(_who): Extension<Who>, Path(id): Path<String>) -> R {
    let link = load_link(&core, &id)?;
    let (local_exists, has_local_source, has_git_source) = source_caps(&core, &id).await;
    Ok(Json(json!({
        "link": sanitize_link(&link),
        "local_project_exists": local_exists,
        "has_local_source": has_local_source,
        "has_git_source": has_git_source,
    })))
}

async fn g_ensure(State(core): State<CoreRef>, Extension(_who): Extension<Who>, Path(id): Path<String>) -> R {
    let link = load_link(&core, &id)?;
    if link["status"] == "revoked" {
        return Ok(Json(json!({
            "ok": false,
            "revoked": true,
            "error": if s(&link, "error").is_empty() { "The owner removed you from this project (or you left it). Ask them for a new invite.".to_string() } else { s(&link, "error") },
            "link": sanitize_link(&link),
        })));
    }
    match collab_chat::ensure_local(&core, &id).await {
        Ok(mut v) => {
            let (_, has_local_source, has_git_source) = source_caps(&core, &id).await;
            let fresh = load_link(&core, &id).unwrap_or(link);
            v["ok"] = json!(true);
            v["link"] = sanitize_link(&fresh);
            v["has_local_source"] = json!(has_local_source);
            v["has_git_source"] = json!(has_git_source);
            Ok(Json(v))
        }
        Err(e) => {
            let fresh = load_link(&core, &id).unwrap_or(link);
            if fresh["status"] == "revoked" {
                return Ok(Json(json!({
                    "ok": false,
                    "revoked": true,
                    "error": e.to_string(),
                    "link": sanitize_link(&fresh),
                })));
            }
            // Offline fallback: make sure the local project row exists so the project page can still render cleanly.
            if core.db.one("SELECT id FROM projects WHERE id = ?1", &[&id])?.is_none() {
                let now = util::now_ms();
                let st = core.settings();
                let lead = st["default_lead"].as_str().unwrap_or("claude").to_string();
                let lead_model = st["default_lead_model"].as_str().unwrap_or("").to_string();
                let name = s(&fresh, "project_name");
                let _ = core.db.exec(
                    "INSERT INTO projects(id, name, instructions, lead_agent, lead_model, color, created_at, updated_at) VALUES (?1, ?2, '', ?3, ?4, 128, ?5, ?5)",
                    &[&id, &name, &lead, &lead_model, &now],
                );
                if let Ok(p) = core.project(&id) {
                    core.bus.emit("project", Some(&id), None, p);
                }
            }
            let chat = core.db.one("SELECT * FROM threads WHERE project_id = ?1 AND kind = 'lead' AND deleted_at IS NULL", &[&id])?;
            let (_, has_local_source, has_git_source) = source_caps(&core, &id).await;
            let sync = core.db.setting(&format!("collab.sync.{id}"));
            Ok(Json(json!({
                "ok": true,
                "offline": e.to_string(),
                "project_id": id,
                "thread": chat,
                "share_chat": sync["share_chat"],
                "readers": sync["readers"],
                "link": sanitize_link(&fresh),
                "has_local_source": has_local_source,
                "has_git_source": has_git_source,
            })))
        }
    }
}

/// Pull fresh role + effective permissions from the host server-to-server and update the local link cache.
async fn g_refresh(State(core): State<CoreRef>, Extension(_who): Extension<Who>, Path(id): Path<String>) -> R {
    let link = require_active_link(&core, &id)?;
    let url = format!("{}/api/collab/host/me", s(&link, "host_url"));
    let r = collab::client()
        .get(&url)
        .bearer_auth(s(&link, "token"))
        .timeout(Duration::from_secs(12))
        .send()
        .await
        .map_err(|e| {
            let msg = format!("Can't reach {} right now ({e}).", s(&link, "host_name"));
            collab::set_link_status(&core, &link, "offline", &msg);
            ApiErr(StatusCode::BAD_GATEWAY, msg)
        })?;
    let st = r.status();
    let v: Value = r.json().await.unwrap_or_default();
    if st == StatusCode::UNAUTHORIZED {
        let msg = "The owner removed you from this project (or you left it). Ask them for a new invite.";
        collab::set_link_status(&core, &link, "revoked", msg);
        return Err(forbid(msg));
    }
    if !st.is_success() {
        return Err(ApiErr(st, v["error"].as_str().unwrap_or("Host error").into()));
    }
    collab::set_link_status(&core, &link, "ok", "");
    let role = s(&v["member"], "role");
    let perms_str = v["perms"].to_string();
    let pname = {
        let n = s(&v["project"], "name");
        if n.is_empty() { s(&link, "project_name") } else { n }
    };
    core.db.exec(
        "UPDATE collab_links SET perms = ?1, role = ?2, project_name = ?3 WHERE id = ?4",
        &[&perms_str, &role, &pname, &id],
    )?;
    if core.db.one("SELECT id FROM projects WHERE id = ?1", &[&id])?.is_some() {
        let _ = core.db.exec("UPDATE projects SET name = ?1 WHERE id = ?2", &[&pname, &id]);
        let repo_url = s(&v["project"], "repo_url");
        let base_branch = s(&v["project"], "base_branch");
        let _ = core.db.exec("UPDATE projects SET repo_url = ?1, base_branch = ?2 WHERE id = ?3 AND folder = ''", &[&repo_url, &base_branch, &id]);
    }
    core.bus.emit("collab-links", None, None, json!({ "id": id, "role": role, "perms": v["perms"] }));
    let (_, has_local_source, has_git_source) = source_caps(&core, &id).await;
    let fresh = load_link(&core, &id)?;
    Ok(Json(json!({
        "member": v["member"],
        "perms": v["perms"],
        "catalog": v["catalog"],
        "project": v["project"],
        "host": v["host"],
        "link": sanitize_link(&fresh),
        "has_local_source": has_local_source,
        "has_git_source": has_git_source,
    })))
}

async fn check_files_access(core: &Core, id: &str) -> Result<Value, ApiErr> {
    let link = require_active_link(core, id)?;
    let (_, has_local_source, _) = source_caps(core, id).await;
    if !has_local_source {
        return Err(forbid("Files aren't available for this joined project because there is no local repository folder here."));
    }
    if !perm_bool(&link, "view_threads") && !perm_bool(&link, "create_threads") {
        return Err(forbid("The project's owner hasn't allowed you to view project files."));
    }
    Ok(link)
}

async fn check_git_access(core: &Core, id: &str) -> Result<Value, ApiErr> {
    let link = require_active_link(core, id)?;
    let (_, _, has_git_source) = source_caps(core, id).await;
    if !has_git_source {
        return Err(forbid("Git isn't available for this joined project because it has no shared git repository."));
    }
    if !perm_bool(&link, "view_threads") && !perm_bool(&link, "create_threads") && !perm_bool(&link, "merge") {
        return Err(forbid("The project's owner hasn't allowed you to view Git for this project."));
    }
    Ok(link)
}

async fn g_files_list(State(core): State<CoreRef>, Extension(_who): Extension<Who>, Path(id): Path<String>, q: Q) -> R {
    check_files_access(&core, &id).await?;
    files::list(State(core), Path(id), q).await
}

async fn g_files_read(State(core): State<CoreRef>, Extension(_who): Extension<Who>, Path(id): Path<String>, q: Q) -> R {
    check_files_access(&core, &id).await?;
    files::read(State(core), Path(id), q).await
}

async fn g_git(State(core): State<CoreRef>, Extension(_who): Extension<Who>, Path(id): Path<String>, q: Q) -> R {
    check_git_access(&core, &id).await?;
    let p = repos::view(&core, &core.project(&id)?, q.get("repo").map(String::as_str).unwrap_or(""))?;
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
        "log": git::log(&home, 15).await.iter().map(|l| {
            let parts: Vec<&str> = l.split('\x1f').collect();
            json!({ "sha": parts.first(), "author": parts.get(1), "when": parts.get(2), "subject": parts.get(3) })
        }).collect::<Vec<_>>(),
    })))
}

async fn g_github_repo(core: &CoreRef, id: &str, repo: &str) -> Result<Option<(String, String)>, ApiErr> {
    let p = repos::view(core, &core.project(id)?, repo)?;
    let mut url = s(&p, "repo_url");
    if url.is_empty() {
        url = git::remote_url(&core.project_home(&p)).await;
    }
    Ok(git::github_repo(&url))
}

async fn g_prs(State(core): State<CoreRef>, Extension(_who): Extension<Who>, Path(id): Path<String>, q: Q) -> R {
    check_git_access(&core, &id).await?;
    let Some((o, r)) = g_github_repo(&core, &id, q.get("repo").map(String::as_str).unwrap_or("")).await? else {
        return Ok(Json(json!({ "github": false })));
    };
    match github::list_prs(&core, &o, &r, "all").await {
        Ok(list) => Ok(Json(json!({ "github": true, "repo": format!("{o}/{r}"), "prs": list }))),
        Err(e) => Ok(Json(json!({ "github": true, "repo": format!("{o}/{r}"), "error": e.to_string() }))),
    }
}

async fn g_pr_merge(
    State(core): State<CoreRef>,
    Extension(_who): Extension<Who>,
    Path((id, number)): Path<(String, i64)>,
    Json(b): Json<Value>,
) -> R {
    let link = check_git_access(&core, &id).await?;
    if !perm_bool(&link, "merge") {
        return Err(forbid("The project's owner hasn't allowed you to merge (permission 'merge')."));
    }
    let Some((o, r)) = g_github_repo(&core, &id, &s(&b, "repo")).await? else {
        return Err(bad("This repository's remote isn't on GitHub."));
    };
    let method = match s(&b, "method").as_str() {
        m @ ("merge" | "squash" | "rebase") => m.to_string(),
        _ => "merge".to_string(),
    };
    Ok(Json(github::merge_pr(&core, &o, &r, number, &method).await?))
}

async fn g_artifact_file_root(
    State(core): State<CoreRef>,
    Extension(_who): Extension<Who>,
    Path((id, aid)): Path<(String, String)>,
    q: Q,
) -> Response {
    proxy_artifact_file(&core, &id, &aid, "", &q.0).await
}

async fn g_artifact_file_rest(
    State(core): State<CoreRef>,
    Extension(_who): Extension<Who>,
    Path((id, aid, rest)): Path<(String, String, String)>,
    q: Q,
) -> Response {
    proxy_artifact_file(&core, &id, &aid, &rest, &q.0).await
}

async fn proxy_artifact_file(core: &CoreRef, id: &str, aid: &str, rest: &str, q: &HashMap<String, String>) -> Response {
    let link = match require_active_link(core, id) {
        Ok(l) => l,
        Err(e) => return e.into_response(),
    };
    if aid.contains(['/', '\\', '.']) || rest.split('/').any(|seg| seg == "..") {
        return bad("Bad artifact path").into_response();
    }
    let host_url = s(&link, "host_url");
    let token = s(&link, "token");
    // Ask the host for this artifact's live signed member URL (checks membership + project scope).
    let meta_url = format!("{host_url}/api/collab/host/artifacts/{aid}");
    let meta_res = match collab::client().get(&meta_url).bearer_auth(&token).timeout(Duration::from_secs(12)).send().await {
        Ok(r) => r,
        Err(e) => {
            let msg = format!("Can't reach {} right now ({e}).", s(&link, "host_name"));
            collab::set_link_status(core, &link, "offline", &msg);
            return ApiErr(StatusCode::BAD_GATEWAY, msg).into_response();
        }
    };
    if meta_res.status() == StatusCode::UNAUTHORIZED {
        let msg = "The owner removed you from this project (or you left it). Ask them for a new invite.";
        collab::set_link_status(core, &link, "revoked", msg);
        return forbid(msg).into_response();
    }
    if !meta_res.status().is_success() {
        return ApiErr(meta_res.status(), "Artifact unavailable".into()).into_response();
    }
    let meta: Value = meta_res.json().await.unwrap_or_default();
    let signed_path = s(&meta, "url");
    if !signed_path.starts_with("/a/") {
        return ApiErr(StatusCode::BAD_GATEWAY, "Invalid artifact URL from host".into()).into_response();
    }
    let rest_clean = rest.trim_start_matches('/');
    let mut target = format!("{host_url}{signed_path}{rest_clean}");
    if q.get("download").is_some() {
        target.push_str("?download=1");
    }
    let file_res = match collab::client().get(&target).timeout(Duration::from_secs(25)).send().await {
        Ok(r) => r,
        Err(e) => return ApiErr(StatusCode::BAD_GATEWAY, format!("Couldn't fetch artifact file: {e}")).into_response(),
    };
    let st = file_res.status();
    let ct = file_res
        .headers()
        .get(header::CONTENT_TYPE)
        .and_then(|v| v.to_str().ok())
        .unwrap_or("application/octet-stream")
        .to_string();
    let cd = file_res.headers().get(header::CONTENT_DISPOSITION).cloned();
    let bytes = match file_res.bytes().await {
        Ok(b) => b,
        Err(e) => return ApiErr(StatusCode::BAD_GATEWAY, format!("Couldn't read artifact file: {e}")).into_response(),
    };
    let mut resp = Response::builder()
        .status(st)
        .header(header::CONTENT_TYPE, HeaderValue::from_str(&ct).unwrap_or_else(|_| HeaderValue::from_static("application/octet-stream")));
    if let Some(cd) = cd {
        resp = resp.header(header::CONTENT_DISPOSITION, cd);
    }
    resp.body(Body::from(bytes)).unwrap_or_else(|_| StatusCode::BAD_GATEWAY.into_response())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn private_http_check_matches_collab_rules() {
        assert!(is_private_http("http://127.0.0.1:4630"));
        assert!(is_private_http("http://localhost:4630"));
        assert!(is_private_http("http://192.168.1.9:4630"));
        assert!(is_private_http("http://box.tailnet.ts.net:4630"));
        assert!(is_private_http("https://example.com"));
        assert!(!is_private_http("http://example.com"));
    }

    #[tokio::test]
    async fn files_and_git_access_denied_without_source_or_permission() {
        let data = std::env::temp_dir().join(util::id("roamer-collab-view-test"));
        let core = Core::open(data.clone(), true).unwrap();
        core.db
            .exec(
                "INSERT INTO collab_links(id, host_url, host_id, host_name, project_id, project_name, member_id, user_id, name, role, perms, token, status, created_at, last_sync)
                 VALUES ('lnk_t1', 'http://127.0.0.1:1', 'h1', 'Host', 'prj_1', 'Demo', 'mbr_1', 'usr_1', 'Guest', 'viewer', '{\"view_threads\":true,\"create_threads\":false,\"merge\":false}', 'tok', 'ok', 0, 0)",
                &[],
            )
            .unwrap();
        core.db
            .exec(
                "INSERT INTO projects(id, name, created_at, updated_at) VALUES ('lnk_t1', 'Demo', 0, 0)",
                &[],
            )
            .unwrap();
        // No local folder and no repo_url => 403 for both Files and Git.
        assert_eq!(check_files_access(&core, "lnk_t1").await.unwrap_err().0, StatusCode::FORBIDDEN);
        assert_eq!(check_git_access(&core, "lnk_t1").await.unwrap_err().0, StatusCode::FORBIDDEN);

        // Give it a remote repo_url: Git is permitted when view_threads is true, but denied when view_threads is false.
        core.db
            .exec("UPDATE projects SET repo_url = 'https://github.com/example/demo' WHERE id = 'lnk_t1'", &[])
            .unwrap();
        assert!(check_git_access(&core, "lnk_t1").await.is_ok());
        core.db
            .exec("UPDATE collab_links SET perms = '{\"view_threads\":false,\"create_threads\":false,\"merge\":false}' WHERE id = 'lnk_t1'", &[])
            .unwrap();
        assert_eq!(check_git_access(&core, "lnk_t1").await.unwrap_err().0, StatusCode::FORBIDDEN);

        // Revoked link always gets 403.
        core.db
            .exec("UPDATE collab_links SET status = 'revoked', perms = '{\"view_threads\":true}' WHERE id = 'lnk_t1'", &[])
            .unwrap();
        assert_eq!(check_git_access(&core, "lnk_t1").await.unwrap_err().0, StatusCode::FORBIDDEN);
        let _ = std::fs::remove_dir_all(&data);
    }
}
