//! Per-person project chats (collaboration Stage 1c, see docs/collaboration.md §10) and the link between a guest's
//! local threads and the host's rows for them.
//!
//! **One id, two machines.** On a guest, each joined project (a `collab_links` row) gets a *local* project whose id
//! is the link's id (`lnk_…`). Every thread in it has the same id as its row on the host:
//!
//! - the guest's own project chat is a local `lead` thread; on the host it is a `chat` thread with `user_id` = that
//!   member (`personal_chat`). The owner keeps the project's real lead thread as their own chat.
//! - threads the guest's lead (or the guest) opens are local `worker`/`review` threads, mirrored on the host with
//!   `user_id` = the member and `parent_id` = their chat. `collab::runs_here` keeps the host from ever running them.
//!
//! So `link_of(project_id)` says "this local thread belongs to that host", and the host thread id is the thread's id.
//!
//! **Sync, guest → host** (`run`, `push`): the guest's app pushes its threads' state and new messages to
//! `POST host/sync` after local changes (debounced, backed off while offline). The host inserts them as they are
//! (by id, never twice) and never wakes an agent for them: another person's activity must not run the owner's plan.
//! **Context, host → guest** (`before_turn`): before each turn of a guest's lead, a compact snapshot of the host
//! project (`GET host/context`: people, threads and who owns them, memory index, artifacts) is fetched and
//! cached; the prompt gets the snapshot and a short "what changed since your last turn" digest, never whole histories.
//!
//! **Private by default.** A person's chat messages leave their machine only while someone else may read them: the
//! owner, if the project's rules say so (`rules.chats.owner`), or a member with `view_others_chats`. Otherwise the
//! host keeps only the chat's row (title, status) and drops any copy it had.

use crate::{
    collab::{self, Member},
    core::{Core, CoreRef},
    server::{bad, ApiErr, Who, R},
    util,
};
use anyhow::{anyhow, bail, Result};
use axum::{
    extract::{Path, State},
    http::StatusCode,
    routing::{get, post},
    Extension, Json, Router,
};
use serde_json::{json, Map, Value};
use std::{
    collections::{HashMap, HashSet},
    time::Duration,
};

pub fn routes() -> Router<CoreRef> {
    Router::new()
        // owner
        .route("/api/collab/projects/{pid}/chats", get(owner_chats).patch(patch_owner_chats))
        // members
        .route("/api/collab/host/chat", get(h_chat))
        .route("/api/collab/host/sync", post(h_sync))
        .route("/api/collab/host/context", get(h_context))
        // this machine as a guest
        .route("/api/collab/links/{id}/chat", post(open_chat))
}

fn s(v: &Value, k: &str) -> String {
    v[k].as_str().unwrap_or("").trim().to_string()
}

// ================================================================================================ host side

/// Find or make a member's own project chat on the host: a `chat` thread in the project with their `user_id`. It is
/// never run here (`collab::runs_here`); its agent runs on their machine.
pub fn personal_chat(core: &CoreRef, m: &Member) -> Option<String> {
    let found = core
        .db
        .one("SELECT id FROM threads WHERE project_id = ?1 AND kind = 'chat' AND user_id = ?2 AND deleted_at IS NULL ORDER BY created_at LIMIT 1", &[&m.project_id, &m.user_id])
        .ok()
        .flatten();
    if let Some(r) = found {
        return r["id"].as_str().map(str::to_string);
    }
    let id = util::id("th");
    let now = util::now_ms();
    let title = format!("{}'s project chat", m.name);
    let note = format!("Runs on {}'s machine", m.name);
    core.db
        .exec(
            "INSERT INTO threads(id, project_id, kind, title, agent, status, status_note, created_at, updated_at, title_by, user_id)
             VALUES (?1, ?2, 'chat', ?3, 'guest', 'idle', ?4, ?5, ?5, 'person', ?6)",
            &[&id, &m.project_id, &title, &note, &now, &m.user_id],
        )
        .map_err(|e| tracing::warn!("collab: couldn't make {}'s chat: {e}", m.name))
        .ok()?;
    if let Ok(t) = core.thread(&id) {
        core.bus.emit("thread", Some(&m.project_id), Some(&id), t);
    }
    Some(id)
}

/// Is this a collaborator's own project chat (on the host)?
pub fn is_personal_chat(t: &Value) -> bool {
    t["kind"] == "chat" && t["project_id"].is_string() && !t["user_id"].as_str().unwrap_or("").is_empty()
}

/// May the owner read collaborators' own chats in this project? Off unless they turned it on (Settings ›
/// Collaborators › Project chats).
pub fn owner_sees_chats(core: &Core, pid: &str) -> bool {
    collab::rules_of(core, pid)["chats"]["owner"] == true
}

/// Hidden from the owner's own windows and agents: a collaborator's chat the owner's rules don't let them read.
pub fn hidden_from_owner(core: &CoreRef, t: &Value) -> bool {
    is_personal_chat(t) && !owner_sees_chats(core, t["project_id"].as_str().unwrap_or(""))
}

/// Who besides its owner may read `user`'s chat: "owner" and/or the names of members with `view_others_chats`.
fn chat_readers(core: &CoreRef, pid: &str, user: &str) -> Vec<String> {
    let mut out = vec![];
    if owner_sees_chats(core, pid) {
        out.push("owner".to_string());
    }
    let members = core
        .db
        .all(
            "SELECT m.user_id, m.role, m.perms, u.name FROM collab_members m JOIN collab_users u ON u.id = m.user_id
             WHERE m.project_id = ?1 AND m.user_id != ?2 AND m.revoked_at IS NULL AND u.revoked_at IS NULL",
            &[&pid, &user],
        )
        .unwrap_or_default();
    for m in members {
        if collab::effective(core, pid, &s(&m, "role"), &m["perms"])["view_others_chats"] == true {
            out.push(s(&m, "name"));
        }
    }
    out
}

async fn h_chat(State(core): State<CoreRef>, Extension(m): Extension<Member>) -> R {
    let id = collab::personal_chat(&core, &m).ok_or_else(|| ApiErr(StatusCode::INTERNAL_SERVER_ERROR, "Couldn't make your project chat".into()))?;
    let t = core.thread(&id)?;
    let readers = chat_readers(&core, &m.project_id, &m.user_id);
    Ok(Json(json!({
        "thread": collab::pick(&t, collab::THREAD_FIELDS),
        "project": collab::project_view(&core, &m.project_id)?,
        "member": { "id": m.id, "user_id": m.user_id, "name": m.name, "role": m.role },
        "perms": m.perms,
        "share_chat": !readers.is_empty(),
        "readers": readers,
        "host": { "id": core.instance_id, "name": core.display_name() },
    })))
}

/// Thread fields a guest may set on its own threads' host rows.
const SYNC_FIELDS: &[&str] = &["title", "agent", "model", "status", "status_note", "task_kind", "instructions", "criteria", "branch", "base_branch", "resolved", "merged"];
const SYNC_MAX_THREADS: usize = 100;
const SYNC_MAX_MESSAGES: usize = 300;

fn valid_id(id: &str) -> bool {
    (4..=64).contains(&id.len()) && id.chars().all(|c| c.is_ascii_alphanumeric() || c == '_' || c == '-')
}

/// A guest's app mirrors its own threads here: their state, and messages the host doesn't have yet (by id). Only the
/// member's own threads; new ones need `create_threads`. Nothing here wakes an agent on this machine.
async fn h_sync(State(core): State<CoreRef>, Extension(m): Extension<Member>, Json(b): Json<Value>) -> R {
    let chat = collab::personal_chat(&core, &m).ok_or_else(|| ApiErr(StatusCode::INTERNAL_SERVER_ERROR, "Couldn't make your project chat".into()))?;
    let shared = !chat_readers(&core, &m.project_id, &m.user_id).is_empty();
    let now = util::now_ms();
    let mut errors: Vec<Value> = vec![];
    let mut changed: Vec<String> = vec![];
    for th in b["threads"].as_array().map(|a| a.iter().take(SYNC_MAX_THREADS).collect::<Vec<_>>()).unwrap_or_default() {
        let id = s(th, "id");
        if !valid_id(&id) {
            errors.push(json!({ "id": id, "error": "bad id" }));
            continue;
        }
        let existing = core.db.one("SELECT * FROM threads WHERE id = ?1", &[&id])?;
        match &existing {
            Some(t) if t["user_id"].as_str() != Some(m.user_id.as_str()) || t["project_id"].as_str() != Some(m.project_id.as_str()) => {
                errors.push(json!({ "id": id, "error": "That thread isn't yours." }));
                continue;
            }
            None if id == chat => {}
            None => {
                if !m.can("create_threads") {
                    errors.push(json!({ "id": id, "error": "The project's owner hasn't allowed you to open threads (permission 'create_threads')." }));
                    continue;
                }
                let kind = if th["kind"] == "review" { "review" } else { "worker" };
                let title = util::clip(&s(th, "title"), 120);
                let title = if title.is_empty() { "Untitled thread".to_string() } else { title };
                let created = th["created_at"].as_i64().filter(|c| *c > 0 && *c <= now).unwrap_or(now);
                core.db.exec(
                    "INSERT INTO threads(id, project_id, parent_id, kind, title, agent, status, created_at, updated_at, title_by, user_id) VALUES (?1, ?2, ?3, ?4, ?5, 'guest', 'idle', ?6, ?7, 'agent', ?8)",
                    &[&id, &m.project_id, &chat, &kind, &title, &created, &now, &m.user_id],
                )?;
            }
            _ => {}
        }
        // a thread the guest deleted goes away here too; the chat itself stays
        if th["deleted"] == true && id != chat {
            core.db.exec("UPDATE threads SET deleted_at = ?1, updated_at = ?1 WHERE id = ?2", &[&now, &id])?;
            changed.push(id);
            continue;
        }
        for k in SYNC_FIELDS {
            let Some(v) = th.get(*k) else { continue };
            // the chat's title is the host's ("Ann's project chat"), so the owner's lists stay readable
            if id == chat && *k == "title" {
                continue;
            }
            let sql = format!("UPDATE threads SET {k} = ?1, updated_at = ?2 WHERE id = ?3");
            match v {
                Value::Bool(x) => core.db.exec(&sql, &[&(*x as i64), &now, &id])?,
                Value::Number(n) => core.db.exec(&sql, &[&n.as_i64().unwrap_or(0), &now, &id])?,
                Value::String(x) => core.db.exec(&sql, &[&util::clip(x, if *k == "instructions" || *k == "criteria" { 8000 } else { 300 }), &now, &id])?,
                _ => continue,
            };
        }
        changed.push(id);
    }
    // whose threads these are: only the member's own in this project take messages
    let mine: HashSet<String> = core
        .db
        .all("SELECT id FROM threads WHERE project_id = ?1 AND user_id = ?2 AND deleted_at IS NULL", &[&m.project_id, &m.user_id])?
        .into_iter()
        .filter_map(|r| r["id"].as_str().map(str::to_string))
        .collect();
    let mut added = 0;
    for x in b["messages"].as_array().map(|a| a.iter().take(SYNC_MAX_MESSAGES).collect::<Vec<_>>()).unwrap_or_default() {
        let (id, tid) = (s(x, "id"), s(x, "thread_id"));
        if !valid_id(&id) || !mine.contains(&tid) || (tid == chat && !shared) {
            continue;
        }
        let role = match x["role"].as_str() {
            Some(r @ ("user" | "agent" | "system")) => r,
            _ => continue,
        };
        let created = x["created_at"].as_i64().filter(|c| *c > 0 && *c <= now + 60_000).unwrap_or(now);
        let meta = json!({ "user_id": m.user_id, "mirror": true, "files": x["files"].as_u64().unwrap_or(0) }).to_string();
        let n = core.db.exec(
            "INSERT OR IGNORE INTO messages(id, thread_id, role, author, body, meta, pending, created_at) VALUES (?1, ?2, ?3, ?4, ?5, ?6, 0, ?7)",
            &[&id, &tid, &role, &util::clip(&s(x, "author"), 80), &util::clip(x["body"].as_str().unwrap_or(""), 20_000), &meta, &created],
        )?;
        if n > 0 {
            added += 1;
            if let Ok(Some(msg)) = core.db.one("SELECT * FROM messages WHERE id = ?1", &[&id]) {
                core.bus.emit("message", Some(&m.project_id), Some(&tid), msg);
            }
            if !changed.contains(&tid) {
                changed.push(tid.clone());
            }
        }
    }
    // not shared (any more): the chat's words stay on their machine only
    if !shared {
        let gone = core.db.exec("DELETE FROM messages WHERE thread_id = ?1", &[&chat])?;
        if gone > 0 {
            changed.push(chat.clone());
        }
    }
    for id in &changed {
        let _ = core.db.exec("UPDATE threads SET updated_at = ?1 WHERE id = ?2", &[&now, id]);
        if let Ok(t) = core.thread(id) {
            core.bus.emit("thread", Some(&m.project_id), Some(id), t);
        }
    }
    let upto: Map<String, Value> = core
        .db
        .all(
            "SELECT t.id, (SELECT MAX(x.created_at) FROM messages x WHERE x.thread_id = t.id) AS upto FROM threads t WHERE t.project_id = ?1 AND t.user_id = ?2 AND t.deleted_at IS NULL",
            &[&m.project_id, &m.user_id],
        )?
        .into_iter()
        .map(|r| (s(&r, "id"), json!(r["upto"].as_i64().unwrap_or(0))))
        .collect();
    if added > 0 || !changed.is_empty() {
        tracing::debug!("collab: {} synced {} thread(s), {added} message(s)", m.name, changed.len());
    }
    Ok(Json(json!({ "chat_id": chat, "share_chat": shared, "upto": upto, "errors": errors, "perms": m.perms, "role": m.role })))
}

/// Unresolved threads in the snapshot, most recent first, and the few most recently finished.
const CONTEXT_OPEN: usize = 60;
const CONTEXT_DONE: usize = 8;

/// The project as a member's lead needs it, compactly: people, threads (whose, doing what), the memory index,
/// and recent artifacts (claims and presence come from 1d's filtered digest). Bodies never: those are read on demand.
async fn h_context(State(core): State<CoreRef>, Extension(m): Extension<Member>) -> R {
    let now = util::now_ms();
    let people = collab::people(&core, &m.project_id);
    let name_of = |uid: &str| people.iter().find(|p| p["user_id"].as_str() == Some(uid)).map(|p| s(p, "name")).unwrap_or_else(|| "someone".into());
    let rows = core.db.all(
        "SELECT id, project_id, parent_id, kind, title, agent, status, status_note, user_id, branch, resolved, merged, updated_at FROM threads
         WHERE project_id = ?1 AND deleted_at IS NULL ORDER BY updated_at DESC LIMIT 400",
        &[&m.project_id],
    )?;
    let (mut open, mut done) = (vec![], vec![]);
    for t in rows.into_iter().filter(|t| collab::can_see(&m, t)) {
        let owner = s(&t, "user_id");
        let item = json!({
            "id": t["id"], "kind": t["kind"], "title": t["title"], "agent": t["agent"], "status": t["status"],
            "note": util::clip(t["status_note"].as_str().unwrap_or(""), 140), "branch": t["branch"],
            "owner": owner, "owner_name": name_of(&owner), "mine": owner == m.user_id, "resolved": t["resolved"] == 1, "merged": t["merged"] == 1,
            "updated_at": t["updated_at"],
        });
        if t["resolved"] == 1 || t["merged"] == 1 {
            if done.len() < CONTEXT_DONE {
                done.push(item);
            }
        } else if open.len() < CONTEXT_OPEN {
            open.push(item);
        }
    }
    // the same entries `GET host/memory` gives this member: shared project memory, global with read_global_memory,
    // never anyone's private collaborator notes (the member's own are on their machine or behind an explicit scope)
    let mut view = crate::collab::memory_scope(&m);
    view.user = None;
    let memory: Vec<Value> = view
        .list(&core, false)?
        .into_iter()
        .map(|r| {
            // "You" is the owner, as their own windows say it
            let by = s(&r, "updated_by");
            let by = if by == "You" || by.starts_with("You (") { format!("{} (owner)", core.display_name()) } else { by };
            json!({ "name": r["name"], "description": util::clip(r["description"].as_str().unwrap_or(""), 140), "global": r["project_id"].is_null(), "updated_by": by, "updated_at": r["updated_at"] })
        })
        .collect();
    let artifacts = core.db.all("SELECT id, title, kind, updated_at FROM artifacts WHERE project_id = ?1 AND deleted_at IS NULL ORDER BY updated_at DESC LIMIT 20", &[&m.project_id])?;
    let readers = chat_readers(&core, &m.project_id, &m.user_id);
    Ok(Json(json!({
        "at": now,
        "host": { "id": core.instance_id, "name": core.display_name() },
        "project": collab::project_view(&core, &m.project_id)?,
        "me": { "user_id": m.user_id, "name": m.name, "role": m.role },
        "perms": m.perms,
        "people": people.iter().map(|p| json!({ "user_id": p["user_id"], "name": p["name"], "role": p["role"] })).collect::<Vec<_>>(),
        "threads": open,
        "finished": done,
        "memory": memory,
        "artifacts": artifacts,
        "share_chat": !readers.is_empty(),
        "readers": readers,
    })))
}

// ------------------------------------------------------------------------------------------------ owner

/// Each collaborator's chat in a project and who may read it; the owner's own switch.
async fn owner_chats(State(core): State<CoreRef>, Extension(_who): Extension<Who>, Path(pid): Path<String>) -> R {
    core.project(&pid)?;
    let owner = owner_sees_chats(&core, &pid);
    let rows = core.db.all(
        "SELECT t.id, t.user_id, t.status, t.updated_at, u.name, (SELECT COUNT(*) FROM messages x WHERE x.thread_id = t.id) AS mirrored
         FROM threads t LEFT JOIN collab_users u ON u.id = t.user_id WHERE t.project_id = ?1 AND t.kind = 'chat' AND t.user_id != '' AND t.deleted_at IS NULL ORDER BY t.created_at",
        &[&pid],
    )?;
    let chats: Vec<Value> = rows
        .into_iter()
        .map(|r| {
            let readers = chat_readers(&core, &pid, r["user_id"].as_str().unwrap_or(""));
            json!({ "id": if owner { r["id"].clone() } else { Value::Null }, "user_id": r["user_id"], "name": r["name"], "status": r["status"], "updated_at": r["updated_at"], "readers": readers, "mirrored": r["mirrored"] })
        })
        .collect();
    Ok(Json(json!({ "owner": owner, "chats": chats })))
}

async fn patch_owner_chats(State(core): State<CoreRef>, Extension(_who): Extension<Who>, Path(pid): Path<String>, Json(b): Json<Value>) -> R {
    core.project(&pid)?;
    let Some(on) = b["owner"].as_bool() else { return Err(bad("Send {owner: true|false}.")) };
    let mut rules = collab::rules_of(&core, &pid);
    rules["chats"] = json!({ "owner": on });
    core.db.exec(
        "INSERT INTO collab_rules(project_id, rules, updated_at) VALUES (?1, ?2, ?3) ON CONFLICT(project_id) DO UPDATE SET rules = excluded.rules, updated_at = excluded.updated_at",
        &[&pid, &rules.to_string(), &util::now_ms()],
    )?;
    core.bus.emit("collab", Some(&pid), None, json!({ "rules": true }));
    owner_chats(State(core), Extension(_who), Path(pid)).await
}

// ================================================================================================ guest side

/// The joined-project link a local project stands for, if it is one (its id is the link's id).
pub fn link_of(core: &Core, project_id: &str) -> Option<Value> {
    if !project_id.starts_with("lnk_") {
        return None;
    }
    core.db.one("SELECT * FROM collab_links WHERE id = ?1", &[&project_id]).ok().flatten()
}

/// Call the host as this member. A 401 marks the link revoked (as the proxy does); a network error marks it offline.
async fn host(core: &CoreRef, link: &Value, method: &str, path: &str, body: Option<&Value>, timeout: Duration) -> Result<Value> {
    let url = format!("{}/api/collab/host/{path}", s(link, "host_url"));
    let rq = if method == "POST" { collab::client().post(&url).json(body.unwrap_or(&json!({}))) } else { collab::client().get(&url) };
    let r = match rq.bearer_auth(s(link, "token")).timeout(timeout).send().await {
        Ok(r) => r,
        Err(e) => {
            let msg = format!("Can't reach {} right now ({}). It will retry.", s(link, "host_name"), if e.is_timeout() { "the host took too long to answer".to_string() } else { e.to_string() });
            collab::set_link_status(core, link, "offline", &msg);
            bail!(msg);
        }
    };
    let st = r.status();
    let v: Value = r.json().await.unwrap_or_default();
    if st == StatusCode::UNAUTHORIZED {
        let msg = "The owner removed you from this project (or you left it). Ask them for a new invite.";
        collab::set_link_status(core, link, "revoked", msg);
        bail!(msg);
    }
    if !st.is_success() {
        bail!(v["error"].as_str().map(str::to_string).unwrap_or_else(|| format!("The host said {st}")));
    }
    collab::set_link_status(core, link, "ok", "");
    Ok(v)
}

fn state_key(link: &str) -> String {
    format!("collab.sync.{link}")
}

/// Make sure this machine has the joined project's local side: the local project (id = the link's) and the person's
/// own chat as its lead thread (id = the host's chat). Safe to call often; it refreshes names and the project's
/// instructions from the host.
pub async fn ensure_local(core: &CoreRef, link_id: &str) -> Result<Value> {
    let link = core.db.one("SELECT * FROM collab_links WHERE id = ?1", &[&link_id])?.ok_or_else(|| anyhow!("No such project link"))?;
    if link["status"] == "revoked" {
        bail!("The owner removed you from this project (or your invite was withdrawn). Ask them for a new invite.");
    }
    let r = host(core, &link, "GET", "chat", None, Duration::from_secs(15)).await?;
    let chat = s(&r["thread"], "id");
    if !valid_id(&chat) {
        bail!("The host didn't send a chat id back.");
    }
    let hp = &r["project"];
    let now = util::now_ms();
    let st = core.settings();
    // threads of an older link to the same host project (joined again) move to this one
    if let Ok(Some(old)) = core.db.one("SELECT project_id FROM threads WHERE id = ?1", &[&chat]) {
        let old = s(&old, "project_id");
        if !old.is_empty() && old != link_id {
            core.db.exec("UPDATE threads SET project_id = ?1 WHERE project_id = ?2", &[&link_id, &old])?;
            core.db.exec("UPDATE projects SET deleted_at = ?1 WHERE id = ?2", &[&now, &old])?;
            core.db.exec("DELETE FROM settings WHERE key = ?1", &[&state_key(&old)])?;
        }
    }
    let name = { let n = s(hp, "name"); if n.is_empty() { s(&link, "project_name") } else { n } };
    let instructions = s(hp, "instructions");
    if core.db.one("SELECT id FROM projects WHERE id = ?1", &[&link_id])?.is_none() {
        let lead = st["default_lead"].as_str().unwrap_or("claude").to_string();
        let lead_model = st["default_lead_model"].as_str().unwrap_or("").to_string();
        core.db.exec(
            "INSERT INTO projects(id, name, instructions, lead_agent, lead_model, repo_url, base_branch, color, created_at, updated_at) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?9)",
            &[&link_id, &name, &instructions, &lead, &lead_model, &s(hp, "repo_url"), &s(hp, "base_branch"), &hp["color"].as_i64().unwrap_or(128), &now],
        )?;
        tracing::info!("collab: made the local side of {name} ({link_id})");
    } else {
        core.db.exec("UPDATE projects SET name = ?1, instructions = ?2, deleted_at = NULL WHERE id = ?3", &[&name, &instructions, &link_id])?;
        // the repository only while nothing was cloned yet
        core.db.exec("UPDATE projects SET repo_url = ?1, base_branch = ?2 WHERE id = ?3 AND folder = ''", &[&s(hp, "repo_url"), &s(hp, "base_branch"), &link_id])?;
    }
    let p = core.project(link_id)?;
    // one lead per project: the person's chat. An empty one made before (by core.lead_thread) gives way.
    for other in core.db.all("SELECT id, (SELECT COUNT(*) FROM messages x WHERE x.thread_id = t.id) AS n FROM threads t WHERE project_id = ?1 AND kind = 'lead' AND id != ?2 AND deleted_at IS NULL", &[&link_id, &chat])? {
        let oid = s(&other, "id");
        if other["n"].as_i64().unwrap_or(0) == 0 {
            core.db.exec("DELETE FROM threads WHERE id = ?1", &[&oid])?;
        } else {
            core.db.exec("UPDATE threads SET kind = 'worker', resolved = 1 WHERE id = ?1", &[&oid])?;
        }
    }
    if core.db.one("SELECT id FROM threads WHERE id = ?1", &[&chat])?.is_none() {
        core.db.exec(
            "INSERT INTO threads(id, project_id, kind, title, agent, model, status, created_at, updated_at, title_by) VALUES (?1, ?2, 'lead', 'Your project chat', ?3, ?4, 'idle', ?5, ?5, 'person')",
            &[&chat, &link_id, &p["lead_agent"].as_str().unwrap_or("claude"), &p["lead_model"].as_str().unwrap_or(""), &now],
        )?;
    } else {
        core.db.exec("UPDATE threads SET project_id = ?1, kind = 'lead', deleted_at = NULL WHERE id = ?2", &[&link_id, &chat])?;
    }
    // what the owner allows, kept on the link for the local checks (opening threads)
    core.db.exec("UPDATE collab_links SET perms = ?1, role = ?2, project_name = ?3 WHERE id = ?4", &[&r["perms"].to_string(), &s(&r["member"], "role"), &name, &link_id])?;
    let mut sync = core.db.setting(&state_key(link_id));
    if !sync.is_object() {
        sync = json!({});
    }
    sync["share_chat"] = r["share_chat"].clone();
    sync["readers"] = r["readers"].clone();
    core.db.set_setting(&state_key(link_id), &sync)?;
    core.bus.emit("project", Some(link_id), None, p);
    let t = core.thread(&chat)?;
    if let Err(e) = bind_bridge(core, &chat, link_id) {
        tracing::warn!("collab: couldn't bind {chat} to the tool bridge: {e}");
    }
    core.bus.emit("thread", Some(link_id), Some(&chat), t.clone());
    mark_dirty(link_id);
    Ok(json!({ "thread": t, "project_id": link_id, "share_chat": r["share_chat"], "readers": r["readers"], "host": r["host"], "member": r["member"] }))
}

async fn open_chat(State(core): State<CoreRef>, Extension(_who): Extension<Who>, Path(id): Path<String>) -> R {
    match ensure_local(&core, &id).await {
        Ok(v) => Ok(Json(v)),
        Err(e) => {
            // offline: the local chat still works; it syncs when the host is back
            let chat = core.db.one("SELECT * FROM threads WHERE project_id = ?1 AND kind = 'lead' AND deleted_at IS NULL", &[&id])?;
            match chat {
                Some(t) => {
                    let sync = core.db.setting(&state_key(&id));
                    Ok(Json(json!({ "thread": t, "project_id": id, "share_chat": sync["share_chat"], "readers": sync["readers"], "offline": e.to_string() })))
                }
                None => Err(ApiErr(StatusCode::BAD_GATEWAY, e.to_string())),
            }
        }
    }
}

/// A local thread of a joined project is the host thread with the same id. Register that with 1b's agent tool bridge
/// (`collab_bridge::bind`, table collab_agent_threads), so the thread's agents reach the host's tools with no extra
/// step. Called for the person's chat once the host made it, and for every thread the host accepted on sync (so
/// ownership is verified).
fn bind_bridge(core: &Core, thread_id: &str, link_id: &str) -> Result<()> {
    crate::collab_bridge::bind(core, thread_id, link_id, Some(thread_id))
}

/// Before a thread opens in a joined project: may this person open threads there?
pub fn may_create(core: &CoreRef, project_id: &str) -> Result<()> {
    let Some(l) = link_of(core, project_id) else { return Ok(()) };
    if l["status"] == "revoked" {
        bail!("The owner removed you from this project, so new threads can't be opened in it.");
    }
    if l["perms"]["create_threads"] == false {
        bail!("The project's owner hasn't allowed you to open threads in it (permission 'create_threads'). Ask them, or do this outside the project.");
    }
    Ok(())
}

// ------------------------------------------------------------------------------------------------ push loop

fn dirty() -> &'static std::sync::Mutex<HashSet<String>> {
    static D: std::sync::OnceLock<std::sync::Mutex<HashSet<String>>> = std::sync::OnceLock::new();
    D.get_or_init(|| std::sync::Mutex::new(HashSet::new()))
}

fn mark_dirty(link: &str) {
    dirty().lock().unwrap_or_else(|p| p.into_inner()).insert(link.to_string());
}

const PUSH_MAX_MESSAGES: usize = 200;

/// Push one joined project's changes to the host. Returns whether more is waiting (a capped batch).
pub async fn push(core: &CoreRef, link_id: &str) -> Result<bool> {
    let Some(link) = link_of(core, link_id) else { return Ok(false) };
    if link["status"] == "revoked" || core.db.one("SELECT id FROM projects WHERE id = ?1", &[&link_id])?.is_none() {
        return Ok(false);
    }
    let key = state_key(link_id);
    let mut st = core.db.setting(&key);
    if !st.is_object() {
        st = json!({});
    }
    let since = st["pushed_at"].as_i64().unwrap_or(0);
    let started = util::now_ms();
    let all = core.db.all("SELECT * FROM threads WHERE project_id = ?1", &[&link_id])?;
    let chat = all.iter().find(|t| t["kind"] == "lead" && t["deleted_at"].is_null()).map(|t| s(t, "id")).unwrap_or_default();
    let threads: Vec<Value> = all
        .iter()
        .filter(|t| t["updated_at"].as_i64().unwrap_or(0) >= since || t["deleted_at"].as_i64().unwrap_or(0) >= since)
        .map(|t| {
            let mut v = collab::pick(t, &["id", "title", "agent", "model", "status", "status_note", "task_kind", "instructions", "criteria", "branch", "base_branch", "created_at"]);
            v["kind"] = json!(if t["kind"] == "lead" { "chat" } else { t["kind"].as_str().unwrap_or("worker") });
            v["resolved"] = json!(t["resolved"] == 1);
            v["merged"] = json!(t["merged"] == 1);
            if !t["deleted_at"].is_null() {
                v["deleted"] = json!(true);
            }
            v
        })
        .collect();
    let share = st["share_chat"] == true;
    let upto = st["upto"].clone();
    let me = s(&link, "name");
    let mut messages = vec![];
    for t in all.iter().filter(|t| t["deleted_at"].is_null()) {
        let tid = s(t, "id");
        if tid == chat && !share {
            continue;
        }
        let from = upto[&tid].as_i64().unwrap_or(0);
        let left = PUSH_MAX_MESSAGES.saturating_sub(messages.len());
        if left == 0 {
            break;
        }
        let rows = core.db.all(
            "SELECT id, thread_id, role, author, body, meta, created_at FROM messages WHERE thread_id = ?1 AND created_at >= ?2 AND role IN ('user','agent','system') ORDER BY created_at LIMIT ?3",
            &[&tid, &from, &(left as i64)],
        )?;
        for mut r in rows {
            // "You" here is this person; on the host it reads as their name
            if r["author"] == "You" || r["author"] == "" && r["role"] == "user" {
                r["author"] = json!(me);
            }
            r["files"] = json!(r["meta"]["files"].as_array().map(|a| a.len()).unwrap_or(0));
            r.as_object_mut().map(|o| o.remove("meta"));
            messages.push(r);
        }
    }
    let more = messages.len() >= PUSH_MAX_MESSAGES;
    let r = host(core, &link, "POST", "sync", Some(&json!({ "threads": threads, "messages": messages })), Duration::from_secs(30)).await?;
    let refused: HashSet<String> = r["errors"].as_array().into_iter().flatten().map(|e| {
        tracing::warn!("collab: host refused thread {}: {}", e["id"], e["error"]);
        s(e, "id")
    }).collect();
    // the host took these as this person's: their agents may use the host's tools for them
    for t in threads.iter().filter(|t| t["deleted"] != true && !refused.contains(&s(t, "id"))) {
        if let Err(e) = bind_bridge(core, &s(t, "id"), link_id) {
            tracing::warn!("collab: couldn't bind {} to the tool bridge: {e}", s(t, "id"));
        }
    }
    st["pushed_at"] = json!(started - 1);
    st["upto"] = r["upto"].clone();
    let was_shared = st["share_chat"] == true;
    st["share_chat"] = r["share_chat"].clone();
    core.db.set_setting(&key, &st)?;
    core.db.exec("UPDATE collab_links SET perms = ?1, role = ?2 WHERE id = ?3", &[&r["perms"].to_string(), &s(&r, "role"), &link_id])?;
    if was_shared != (r["share_chat"] == true) {
        core.bus.emit("collab-links", None, None, json!({ "id": link_id, "share_chat": r["share_chat"] }));
    }
    // the chat just became shared: its earlier words go up next round
    Ok(more || (!was_shared && r["share_chat"] == true))
}

/// A local change in a joined project marks it for pushing; leaving one takes its local side away.
fn on_event(core: &CoreRef, ev: &crate::bus::Event) {
    if matches!(ev.kind.as_str(), "message" | "thread") {
        if let Some(p) = ev.project.as_deref().filter(|p| p.starts_with("lnk_")) {
            mark_dirty(p);
        }
    }
    if ev.kind == "collab-links" {
        if let Some(id) = ev.data["id"].as_str() {
            if core.db.one("SELECT id FROM collab_links WHERE id = ?1", &[&id]).ok().flatten().is_none() {
                if core.db.exec("UPDATE projects SET deleted_at = ?1 WHERE id = ?2 AND deleted_at IS NULL", &[&util::now_ms(), &id]).unwrap_or(0) > 0 {
                    core.bus.emit("project", Some(id), None, json!({ "id": id, "deleted": true }));
                }
            }
        }
    }
}

/// The guest's sync loop: local changes in joined projects go up to their hosts a moment later (batched), and every
/// few minutes anyway (so a rule the owner changed is noticed). Offline hosts are retried with a growing pause.
pub async fn run(core: CoreRef) {
    let mut rx = core.bus.subscribe();
    for l in core.db.all("SELECT id FROM collab_links", &[]).unwrap_or_default() {
        mark_dirty(&s(&l, "id"));
    }
    let mut wait: HashMap<String, (tokio::time::Instant, u64)> = HashMap::new();
    let mut sweep = tokio::time::Instant::now() + Duration::from_secs(300);
    loop {
        let has = !dirty().lock().unwrap_or_else(|p| p.into_inner()).is_empty();
        let ev = tokio::time::timeout(Duration::from_secs(if has { 2 } else { 30 }), rx.recv()).await;
        match ev {
            Ok(Ok(ev)) => {
                on_event(&core, &ev);
                // let a burst land before pushing
                tokio::time::sleep(Duration::from_millis(1200)).await;
                while let Ok(ev) = rx.try_recv() {
                    on_event(&core, &ev);
                }
            }
            Ok(Err(tokio::sync::broadcast::error::RecvError::Lagged(_))) => {
                for l in core.db.all("SELECT id FROM collab_links", &[]).unwrap_or_default() {
                    mark_dirty(&s(&l, "id"));
                }
            }
            Ok(Err(_)) => return,
            Err(_) => {} // quiet: try what's waiting
        }
        let now = tokio::time::Instant::now();
        if now >= sweep {
            sweep = now + Duration::from_secs(300);
            for l in core.db.all("SELECT id FROM collab_links WHERE status != 'revoked'", &[]).unwrap_or_default() {
                mark_dirty(&s(&l, "id"));
            }
        }
        let due: Vec<String> = dirty().lock().unwrap_or_else(|p| p.into_inner()).iter().filter(|l| wait.get(*l).is_none_or(|(at, _)| *at <= now)).cloned().collect();
        for l in due {
            dirty().lock().unwrap_or_else(|p| p.into_inner()).remove(&l);
            match push(&core, &l).await {
                Ok(more) => {
                    wait.remove(&l);
                    if more {
                        mark_dirty(&l);
                    }
                }
                Err(e) => {
                    let secs = wait.get(&l).map(|(_, s)| (*s * 2).min(120)).unwrap_or(5);
                    tracing::debug!("collab: sync of {l} failed, retrying in {secs}s: {e}");
                    wait.insert(l.clone(), (tokio::time::Instant::now() + Duration::from_secs(secs), secs));
                    mark_dirty(&l);
                }
            }
        }
    }
}

// ------------------------------------------------------------------------------------------------ context for prompts

fn ctx_key(link: &str) -> String {
    format!("collab.ctx.{link}")
}

fn seen_key(thread: &str) -> String {
    format!("collab.seen.{thread}")
}

/// Before a turn in a joined project: refresh the host's project picture (a few seconds at most; the last one is
/// used when the host can't be reached) and, for the lead, say what changed since its last turn. Empty elsewhere.
pub async fn before_turn(core: &CoreRef, t: &Value) -> String {
    let Some(link) = link_of(core, t["project_id"].as_str().unwrap_or("")) else { return String::new() };
    let lid = s(&link, "id");
    let fresh = if link["status"] == "revoked" { None } else { host(core, &link, "GET", "context", None, Duration::from_secs(8)).await.ok() };
    let offline = fresh.is_none();
    let ctx = match fresh {
        Some(c) => {
            let _ = core.db.set_setting(&ctx_key(&lid), &c);
            c
        }
        None => core.db.setting(&ctx_key(&lid)),
    };
    if t["kind"] != "lead" || !ctx.is_object() {
        return String::new();
    }
    let tid = s(t, "id");
    let prev = core.db.setting(&seen_key(&tid));
    let now = seen_of(&ctx);
    let _ = core.db.set_setting(&seen_key(&tid), &now);
    let mut out = digest(&prev, &now, &ctx);
    if offline {
        out.push_str(&format!("- {} can't be reached right now; the project picture is from {}.\n", s(&link, "host_name"), when(ctx["at"].as_i64().unwrap_or(0))));
    }
    if out.is_empty() {
        return String::new();
    }
    format!("<project-update from=\"{}\" note=\"Changes in the shared project since your last turn.\">\n{out}</project-update>\n\n", s(&link, "host_name").replace('"', "'"))
}

/// What a lead has seen of the project: each thread's status, each memory entry's time, each artifact's time.
fn seen_of(ctx: &Value) -> Value {
    let threads: Map<String, Value> = ctx["threads"].as_array().into_iter().flatten().chain(ctx["finished"].as_array().into_iter().flatten()).map(|t| (s(t, "id"), json!(format!("{}|{}", s(t, "status"), t["resolved"] == true || t["merged"] == true)))).collect();
    let memory: Map<String, Value> = ctx["memory"].as_array().into_iter().flatten().map(|m| (s(m, "name"), m["updated_at"].clone())).collect();
    let arts: Map<String, Value> = ctx["artifacts"].as_array().into_iter().flatten().map(|a| (s(a, "id"), a["updated_at"].clone())).collect();
    let people: Vec<Value> = ctx["people"].as_array().into_iter().flatten().map(|p| json!(s(p, "user_id"))).collect();
    json!({ "threads": threads, "memory": memory, "artifacts": arts, "people": people })
}

/// The few lines that changed between two looks at the project. Nothing on the first look (the system prompt has
/// the whole picture then).
fn digest(prev: &Value, now: &Value, ctx: &Value) -> String {
    if !prev.is_object() {
        return String::new();
    }
    let mut out = String::new();
    let all: Vec<&Value> = ctx["threads"].as_array().into_iter().flatten().chain(ctx["finished"].as_array().into_iter().flatten()).collect();
    let mut n = 0;
    for t in &all {
        let id = s(t, "id");
        if t["mine"] == true {
            continue; // the lead hears about its own threads from them
        }
        let before = prev["threads"][&id].as_str();
        let after = now["threads"][&id].as_str();
        if before == after {
            continue;
        }
        n += 1;
        if n > 15 {
            continue;
        }
        let who = s(t, "owner_name");
        let note = s(t, "note");
        out.push_str(&match before {
            None => format!("- New thread by {who}: \"{}\" ({}, {}{}) `{id}`\n", s(t, "title"), s(t, "agent"), s(t, "status"), if note.is_empty() { String::new() } else { format!(": {note}") }),
            Some(_) => format!("- {who}'s thread \"{}\" is now {}{}{} `{id}`\n", s(t, "title"), s(t, "status"), if t["merged"] == true { " (merged)" } else if t["resolved"] == true { " (resolved)" } else { "" }, if note.is_empty() { String::new() } else { format!(": {note}") }),
        });
    }
    if n > 15 {
        out.push_str(&format!("- ...and {} more thread changes (see the project picture).\n", n - 15));
    }
    for m in ctx["memory"].as_array().into_iter().flatten() {
        let name = s(m, "name");
        if prev["memory"][&name] != now["memory"][&name] {
            out.push_str(&format!("- Memory `{name}` {} by {}\n", if prev["memory"][&name].is_null() { "added" } else { "updated" }, s(m, "updated_by")));
        }
    }
    for a in ctx["artifacts"].as_array().into_iter().flatten() {
        let id = s(a, "id");
        if prev["artifacts"][&id] != now["artifacts"][&id] {
            out.push_str(&format!("- Artifact \"{}\" ({}) {}\n", s(a, "title"), s(a, "kind"), if prev["artifacts"][&id].is_null() { "published" } else { "updated" }));
        }
    }
    for p in ctx["people"].as_array().into_iter().flatten() {
        let uid = json!(s(p, "user_id"));
        if !prev["people"].as_array().is_some_and(|a| a.contains(&uid)) {
            out.push_str(&format!("- {} joined the project as a {}\n", s(p, "name"), s(p, "role")));
        }
    }
    out
}

fn when(ms: i64) -> String {
    chrono::DateTime::from_timestamp_millis(ms).map(|d| d.with_timezone(&chrono::Local).format("%Y-%m-%d %H:%M").to_string()).unwrap_or_else(|| "earlier".into())
}

const COLLAB_PROMPT: &str = include_str!("../prompts/collab.md");

fn prompt_part(key: &str) -> String {
    let text = COLLAB_PROMPT.replace("\r\n", "\n");
    let marker = format!("## @{key}\n");
    let Some(start) = text.find(&marker) else { return String::new() };
    let rest = &text[start + marker.len()..];
    let end = rest.find("\n## @").map(|i| i + 1).unwrap_or(rest.len());
    rest[..end].trim().to_string()
}

fn role_label(r: &str) -> &str {
    match r {
        "owner" => "owner",
        "operator" => "operator",
        "viewer" => "viewer",
        _ => "collaborator",
    }
}

/// The collaborative setup for a thread's instructions: on a guest, the host project's picture as last fetched; on
/// the host, who the collaborators are and which threads are theirs. Empty for projects nobody joined.
pub fn prompt_section(core: &Core, t: &Value, project: Option<&Value>) -> String {
    let Some(pid) = project.and_then(|p| p["id"].as_str()) else { return String::new() };
    if let Some(link) = link_of(core, pid) {
        return guest_section(core, t, &link);
    }
    host_section(core, t, pid)
}

fn host_section(core: &Core, t: &Value, pid: &str) -> String {
    let people = collab::people(core, pid);
    if people.len() < 2 {
        return String::new();
    }
    let lead = t["kind"] == "lead";
    let list: Vec<String> = people
        .iter()
        .map(|p| {
            if p["role"] == "owner" {
                format!("- **{}** (owner): the person in this chat. Their agents run on this machine.", s(p, "name"))
            } else {
                format!("- **{}** ({}): joined from their own Model Roamer; their chat and their threads run on their machine.", s(p, "name"), role_label(p["role"].as_str().unwrap_or("")))
            }
        })
        .collect();
    let names: HashMap<String, String> = people.iter().map(|p| (s(p, "user_id"), s(p, "name"))).collect();
    let rows = core
        .db
        .all("SELECT id, title, status, user_id, kind FROM threads WHERE project_id = ?1 AND user_id != '' AND kind != 'chat' AND resolved = 0 AND deleted_at IS NULL ORDER BY updated_at DESC LIMIT 40", &[&pid])
        .unwrap_or_default();
    let threads = if rows.is_empty() {
        "No collaborator has an open thread right now.".to_string()
    } else {
        format!(
            "Collaborators' open threads:\n\n{}",
            rows.iter().map(|r| format!("- \"{}\" ({}): {} `{}`", s(r, "title"), names.get(&s(r, "user_id")).cloned().unwrap_or_else(|| "a collaborator".into()), s(r, "status"), s(r, "id"))).collect::<Vec<_>>().join("\n")
        )
    };
    let mut text = prompt_part("host").replace("{{PEOPLE}}", &list.join("\n")).replace("{{THREADS}}", &threads);
    if !lead {
        // workers need the rule, not the whole picture
        text = format!("## A collaborative project\n\nOther people work on this project from their own machines; their threads are theirs. Don't redo what their threads do (the lead knows who has what), and keep to your task.\n\n{threads}");
    }
    format!("\n\n{text}\n")
}

fn guest_section(core: &Core, t: &Value, link: &Value) -> String {
    let ctx = core.db.setting(&ctx_key(&s(link, "id")));
    let sync = core.db.setting(&state_key(&s(link, "id")));
    let me = s(link, "name");
    let host_name = s(link, "host_name");
    let people = ctx["people"].as_array().cloned().unwrap_or_default();
    let owner = people.iter().find(|p| p["role"] == "owner").map(|p| s(p, "name")).unwrap_or_else(|| host_name.clone());
    let list: Vec<String> = people
        .iter()
        .map(|p| {
            let n = s(p, "name");
            if s(p, "user_id") == s(link, "user_id") {
                format!("- **{n}** ({}): the person in this chat (you work for them).", role_label(p["role"].as_str().unwrap_or("")))
            } else if p["role"] == "owner" {
                format!("- **{n}** (owner): hosts the project; has their own lead and agents.")
            } else {
                format!("- **{n}** ({}): works from their own machine with their own lead.", role_label(p["role"].as_str().unwrap_or("")))
            }
        })
        .collect();
    let line = |x: &Value| {
        let note = s(x, "note");
        format!("- \"{}\" by {}{} ({}, {}{}) `{}`", s(x, "title"), s(x, "owner_name"), if x["mine"] == true { " (yours)" } else { "" }, s(x, "agent"), s(x, "status"), if note.is_empty() { String::new() } else { format!(": {note}") }, s(x, "id"))
    };
    let open: Vec<String> = ctx["threads"].as_array().into_iter().flatten().filter(|x| x["kind"] != "lead" && x["kind"] != "chat").map(line).collect();
    let done: Vec<String> = ctx["finished"].as_array().into_iter().flatten().filter(|x| x["kind"] != "chat").map(line).collect();
    let mut threads = if open.is_empty() { "No open threads in the project right now.".to_string() } else { format!("Open threads in the project:\n\n{}", open.join("\n")) };
    if !done.is_empty() {
        threads.push_str(&format!("\n\nRecently finished:\n\n{}", done.join("\n")));
    }
    let perms = ctx["perms"].as_object().or(link["perms"].as_object()).cloned().unwrap_or_default();
    let allowed: Vec<&str> = collab::PERMS.iter().filter(|(k, ..)| perms.get(*k) == Some(&json!(true))).map(|(k, ..)| *k).collect();
    let perms_text = if allowed.is_empty() { "read only".to_string() } else { allowed.iter().map(|k| format!("`{k}`")).collect::<Vec<_>>().join(", ") };
    let memory: Vec<String> = ctx["memory"].as_array().into_iter().flatten().take(80).map(|m| format!("`{}`{}", s(m, "name"), if m["global"] == true { " (owner's global)" } else { "" })).collect();
    let arts: Vec<String> = ctx["artifacts"].as_array().into_iter().flatten().take(12).map(|a| format!("\"{}\" ({})", s(a, "title"), s(a, "kind"))).collect();
    let readers: Vec<String> = sync["readers"].as_array().into_iter().flatten().filter_map(|x| x.as_str()).map(|x| if x == "owner" { format!("{owner} (the owner)") } else { x.to_string() }).collect();
    let shared = if readers.is_empty() { ", and nobody else can read it".to_string() } else { format!("; the owner's rules let {} read it too", readers.join(" and ")) };
    let stale = if ctx.is_object() { String::new() } else { format!("\n\n(The project picture hasn't been fetched from {host_name} yet; it arrives with the next turn.)") };
    let lead = t["kind"] == "lead";
    if !lead {
        return format!(
            "\n\n## A collaborative project\n\nThis project belongs to {owner} and is shared; you work for {me}, on {me}'s machine and accounts. Other people's threads are theirs: don't redo their work or edit files they claimed.\n\n{threads}\n"
        );
    }
    let text = prompt_part("guest")
        .replace("{{HOST_PERSON}}", &owner)
        .replace("{{HOST}}", &host_name)
        .replace("{{ME}}", if me.is_empty() { "the person" } else { &me })
        .replace("{{ROLE}}", role_label(link["role"].as_str().unwrap_or("")))
        .replace("{{PERMS}}", &perms_text)
        .replace("{{PEOPLE}}", &if list.is_empty() { format!("- **{me}** (you work for them) and **{owner}** (owner)") } else { list.join("\n") })
        .replace("{{THREADS}}", &threads)
        .replace("{{SHARED}}", &shared)
        .replace("{{MEMORY}}", &if memory.is_empty() { "empty.".to_string() } else { memory.join(", ") })
        .replace("{{ARTIFACTS}}", &if arts.is_empty() { String::new() } else { format!("**Recent artifacts:** {}.\n\n", arts.join(", ")) })
        .replace("{{AT}}", &when(ctx["at"].as_i64().unwrap_or(0)));
    format!("\n\n{text}{stale}\n")
}

/// Owner-side tools that act on a thread can't act on a collaborator's: it runs on their machine, on their account.
/// Reading is fine, except a person's chat the owner's rules keep private.
pub fn guard_tool(core: &CoreRef, name: &str, args: &Value) -> Result<()> {
    let acts = matches!(
        name,
        "send_message" | "reassign_thread" | "cancel_thread" | "pause_thread" | "resume_thread" | "resolve_thread" | "park_thread" | "reopen_thread" | "merge_result" | "request_review" | "open_pull_request"
    );
    let reads = matches!(name, "read_thread" | "review_diff");
    if !acts && !reads {
        return Ok(());
    }
    let id = args["thread_id"].as_str().unwrap_or("").trim();
    if id.is_empty() {
        return Ok(());
    }
    let Ok(t) = core.thread(id) else { return Ok(()) };
    let uid = s(&t, "user_id");
    if uid.is_empty() {
        return Ok(());
    }
    let who = core.db.one("SELECT name FROM collab_users WHERE id = ?1", &[&uid]).ok().flatten().map(|r| s(&r, "name")).unwrap_or_else(|| "a collaborator".into());
    if reads && hidden_from_owner(core, &t) {
        bail!("That is {who}'s own project chat, and it's private to them.");
    }
    if acts {
        bail!("That thread belongs to {who} and runs on their machine, on their account, so only they (and their lead) can steer it. You can read it with read_thread; tell the person if something needs {who}'s attention.");
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn prompt_parts_split() {
        assert!(prompt_part("host").contains("{{PEOPLE}}"));
        assert!(!prompt_part("host").contains("{{HOST_PERSON}}"));
        assert!(prompt_part("guest").contains("{{HOST_PERSON}}"));
    }

    #[test]
    fn digest_lists_only_changes() {
        let ctx1 = json!({ "threads": [{ "id": "th_a", "title": "Fix login", "status": "working", "owner_name": "Ann", "agent": "codex" }], "memory": [{ "name": "plan", "updated_at": 1 }], "artifacts": [], "people": [{ "user_id": "", "name": "Owner", "role": "owner" }] });
        let s1 = seen_of(&ctx1);
        assert_eq!(digest(&Value::Null, &s1, &ctx1), "");
        assert_eq!(digest(&s1, &s1, &ctx1), "");
        let ctx2 = json!({ "threads": [{ "id": "th_a", "title": "Fix login", "status": "review", "owner_name": "Ann", "agent": "codex" }, { "id": "th_b", "title": "Mine", "status": "working", "mine": true }], "memory": [{ "name": "plan", "updated_at": 2, "updated_by": "Ann (collaborator)" }], "artifacts": [], "people": [{ "user_id": "", "name": "Owner", "role": "owner" }, { "user_id": "usr_b", "name": "Bo", "role": "viewer" }] });
        let d = digest(&s1, &seen_of(&ctx2), &ctx2);
        assert!(d.contains("Ann's thread \"Fix login\" is now review"), "{d}");
        assert!(!d.contains("Mine"));
        assert!(d.contains("Memory `plan` updated"));
        assert!(d.contains("Bo joined"));
    }

    #[test]
    fn ids_are_checked() {
        assert!(valid_id("th_1a2b3c"));
        assert!(!valid_id("th_x'; DROP"));
        assert!(!valid_id(""));
    }
}
