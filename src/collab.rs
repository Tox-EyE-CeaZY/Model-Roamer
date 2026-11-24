//! Collaboration: other people's Model Roamers joining this machine's projects, and this machine joining someone
//! else's. The owner hosts (see docs/collaboration.md):
//!
//! - **Owner** routes (`/api/collab/projects/..`, `/invites/..`, `/members/..`): invite people, set the rules, revoke.
//!   The owner is whoever holds the local token or a paired device, as everywhere else.
//! - **Join** (`POST /api/collab/join`, public, rate limited): a guest's app trades a single-use invite for a member
//!   token scoped to one project.
//! - **Host** routes (`/api/collab/host/..`): what a member's app may read and do, every call permission-checked.
//!   Member tokens work here and nowhere else (server::auth).
//! - **Guest** routes (`/api/collab/links/..`): this machine's joined projects, and a narrow proxy that adds the
//!   member token, so the guest's windows and agents never hold it.
//!
//! Nothing here runs unless someone is invited: with no members, every other route behaves as before.

use crate::{
    artifacts,
    core::CoreRef,
    memory, runner,
    server::{self, bad, ApiErr, Who, R},
    util,
};
use axum::{
    body::{Body, Bytes},
    extract::{ConnectInfo, Path, Query, State},
    http::{header, Method, StatusCode},
    middleware,
    response::{IntoResponse, Response},
    routing::{any, delete, get, patch, post},
    Extension, Json, Router,
};
use serde_json::{json, Map, Value};
use std::{
    collections::HashMap,
    net::SocketAddr,
    sync::{Mutex, OnceLock},
    time::Duration,
};

type Q = Query<HashMap<String, String>>;

#[path = "collab_presence.rs"]
pub(crate) mod presence;
#[path = "collab_notes.rs"]
pub(crate) mod notes;

pub fn router(core: CoreRef) -> Router {
    Router::new()
        .merge(coordination_routes())
        // owner
        .route("/api/collab", get(overview))
        .route("/api/collab/projects/{pid}", get(project_admin))
        .route("/api/collab/projects/{pid}/invites", post(create_invite))
        .route("/api/collab/projects/{pid}/rules", patch(patch_rules))
        .route("/api/collab/invites/{id}", delete(revoke_invite))
        .route("/api/collab/members/{id}", patch(patch_member).delete(revoke_member))
        // a guest app joining
        .route("/api/collab/join", post(join))
        // what members may reach
        .route("/api/collab/host/me", get(h_me))
        .route("/api/collab/host/project", get(h_project))
        .route("/api/collab/host/changes", get(h_changes))
        .route("/api/collab/host/threads", post(h_create_thread))
        .route("/api/collab/host/threads/{id}/messages", get(h_messages).post(h_post_message))
        .route("/api/collab/host/threads/{id}/merge", post(h_merge))
        .route("/api/collab/host/threads/{id}/transfer", post(h_transfer))
        .route("/api/collab/host/memory", get(h_memory).post(h_save_memory))
        .route("/api/collab/host/memory/{name}", get(h_memory_entry))
        .route("/api/collab/host/artifacts", get(h_artifacts).post(h_publish))
        .route("/api/collab/host/mcp", post(h_mcp))
        .route("/api/collab/host/leave", post(h_leave))
        .route("/api/collab/host/request-role", post(h_request_role))
        // this machine as a guest
        .route("/api/collab/links", get(list_links).post(add_link))
        .route("/api/collab/links/{id}", delete(remove_link))
        .route("/api/collab/links/{id}/host/{*path}", any(proxy))
        .layer(axum::extract::DefaultBodyLimit::max(4 * 1024 * 1024))
        .layer(middleware::from_fn_with_state(core.clone(), server::auth))
        .layer(middleware::from_fn(server::headers))
        .with_state(core)
}

fn coordination_routes() -> Router<CoreRef> {
    Router::new()
        .route("/api/collab/projects/{pid}/presence", get(presence::owner_get))
        .route("/api/collab/projects/{pid}/overlaps", post(presence::owner_check))
        .route("/api/collab/projects/{pid}/coordination", get(presence::coordination))
        .route("/api/collab/projects/{pid}/notes", get(notes::owner_inbox).post(notes::owner_send))
        .route("/api/collab/projects/{pid}/notes/{id}", post(notes::owner_decide))
        .route("/api/collab/notes-attention", get(notes::local_count))
        .route("/api/collab/host/presence", get(presence::member_get).post(presence::heartbeat))
        .route("/api/collab/host/overlaps", post(presence::member_check))
        .route("/api/collab/host/notes", get(notes::member_list).post(notes::member_send))
        .route("/api/collab/host/notes/{id}", post(notes::member_decide))
        .route("/api/collab/host/notes-delivery", get(notes::member_delivery).post(notes::member_receipts))
}

fn s(v: &Value, k: &str) -> String {
    v[k].as_str().unwrap_or("").trim().to_string()
}

fn forbid(msg: impl Into<String>) -> ApiErr {
    ApiErr(StatusCode::FORBIDDEN, msg.into())
}

// ------------------------------------------------------------------------------------------------ permissions

/// What a member may do: (key, what it allows, defaults for [collaborator, operator, viewer]). Unknown keys in saved
/// rules are ignored, so later stages add permissions here without a migration.
pub const PERMS: &[(&str, &str, [bool; 3])] = &[
    ("view_presence", "see who is working on what", [true, true, true]),
    ("share_presence", "share their active threads and path claims", [true, true, false]),
    ("send_conflict_notes", "send a note for the receiving thread's owner to review", [true, true, false]),
    ("review_conflict_notes", "review conflict notes for other people's threads as an operator", [false, true, false]),
    ("view_threads", "see the project's shared threads and their history", [true, true, true]),
    ("view_others_chats", "see other people's own project chats", [false, false, false]),
    ("create_threads", "open threads of their own (they run on their own machine)", [true, true, false]),
    ("message_threads", "post into threads run by your agents (uses your plan)", [false, false, false]),
    ("message_lead", "talk to your project lead (wakes it, uses your plan)", [false, false, false]),
    ("edit_memory", "write project memory", [true, true, false]),
    ("read_global_memory", "read your global memory (it spans all your projects)", [false, false, false]),
    ("publish_artifacts", "publish artifacts into the project", [true, true, false]),
    ("merge", "merge threads into the base branch", [false, false, false]),
    // Stage 2: an operator takes a thread over without asking each time; everyone else asks per transfer
    ("transfer_threads", "take threads over from you or others without asking each time", [false, true, false]),
];

/// Roles, in the order the defaults above use. `operator` is a collaborator the owner trusts with transfers: they ask
/// once (`POST host/request-role`), the owner approves once.
pub const ROLES: &[&str] = &["collaborator", "operator", "viewer"];

fn role_defaults(role: &str) -> Map<String, Value> {
    let i = ROLES.iter().position(|r| *r == role).unwrap_or(0);
    PERMS.iter().map(|(k, _, d)| (k.to_string(), json!(d[i]))).collect()
}

fn rules_of(core: &CoreRef, project: &str) -> Value {
    core.db.one("SELECT rules FROM collab_rules WHERE project_id = ?1", &[&project]).ok().flatten().map(|r| r["rules"].clone()).filter(Value::is_object).unwrap_or(json!({}))
}

/// Only known permission keys with boolean values survive.
fn clean_perms(v: &Value) -> Map<String, Value> {
    let mut out = Map::new();
    if let Some(o) = v.as_object() {
        for (k, x) in o {
            if PERMS.iter().any(|(p, ..)| p == k) {
                if let Some(b) = x.as_bool() {
                    out.insert(k.clone(), json!(b));
                }
            }
        }
    }
    out
}

/// Built-in role defaults, then the project's rules for that role, then this person's overrides.
fn role_rules(core: &CoreRef, project: &str, role: &str) -> Map<String, Value> {
    let mut eff = role_defaults(role);
    eff.extend(clean_perms(&rules_of(core, project)[role]));
    eff
}

fn effective(core: &CoreRef, project: &str, role: &str, overrides: &Value) -> Value {
    let mut eff = role_rules(core, project, role);
    eff.extend(clean_perms(overrides));
    Value::Object(eff)
}

fn catalog() -> Value {
    json!(PERMS.iter().map(|(k, label, d)| json!({ "id": k, "label": label, "collaborator": d[0], "operator": d[1], "viewer": d[2] })).collect::<Vec<_>>())
}

/// A member of one of this machine's projects, as the auth layer found them from their token.
#[derive(Clone, Debug)]
pub struct Member {
    pub id: String,
    pub user_id: String,
    pub name: String,
    pub project_id: String,
    pub role: String,
    pub perms: Value,
    /// a role they asked the owner for and haven't been answered on yet ('' when none)
    pub requested_role: String,
}

impl Member {
    pub fn can(&self, perm: &str) -> bool {
        self.perms[perm] == true
    }

    fn need(&self, perm: &str) -> Result<(), ApiErr> {
        if self.can(perm) {
            return Ok(());
        }
        let what = PERMS.iter().find(|(k, ..)| *k == perm).map(|(_, l, ..)| *l).unwrap_or(perm);
        Err(forbid(format!("The project's owner hasn't allowed you to {what} (permission '{perm}').")))
    }

    fn author(&self) -> String {
        format!("{} (collaborator)", self.name)
    }
}

/// Who holds this member token? Only live memberships of live people in live projects count; revoking any of them
/// shuts the token out on its next request.
pub fn identify_member(core: &CoreRef, token: &str, ip: &str) -> Option<Member> {
    if token.len() < 32 {
        return None;
    }
    let hash = util::sha256(token);
    let r = core
        .db
        .one(
            "SELECT m.id, m.user_id, m.project_id, m.role, m.perms, m.requested_role, u.name FROM collab_members m
             JOIN collab_users u ON u.id = m.user_id JOIN projects p ON p.id = m.project_id
             WHERE m.token_hash = ?1 AND m.revoked_at IS NULL AND u.revoked_at IS NULL AND p.deleted_at IS NULL",
            &[&hash],
        )
        .ok()??;
    let now = util::now_ms();
    let id = s(&r, "id");
    let _ = core.db.exec("UPDATE collab_members SET last_seen = ?1, last_ip = ?2 WHERE id = ?3", &[&now, &ip, &id]);
    let project = s(&r, "project_id");
    let role = s(&r, "role");
    Some(Member { perms: effective(core, &project, &role, &r["perms"]), id, user_id: s(&r, "user_id"), name: s(&r, "name"), project_id: project, role, requested_role: s(&r, "requested_role") })
}

// ------------------------------------------------------------------------------------------------ extension points

/// The host's runner must not run a thread that belongs to a collaborator: its agents run on their machine, under
/// their accounts (terms of service: an agent only runs on its own user's account, on prompts that user wrote or
/// approved). `runner::runnable` checks it.
pub fn runs_here(thread: &Value) -> bool {
    thread["user_id"].as_str().unwrap_or("").is_empty()
}

/// Stage 1b, collaborator memory: the memory scope a member's agents write to with `scope: "me"` (a `user_id`
/// column on `memory`, private to that person). Until then members write project memory only.
#[allow(dead_code)]
pub fn memory_scope(m: &Member) -> (Option<String>, Option<String>) {
    (Some(m.project_id.clone()), None)
}

/// Stage 1c, per-person project chats: find or make the member's own `chat` thread in the project (`user_id` set).
/// `view_others_chats` already gates seeing other people's (see `can_see`).
#[allow(dead_code)]
pub fn personal_chat(_core: &CoreRef, _m: &Member) -> Option<String> {
    None
}

fn not_yet(stage: &str, what: &str) -> ApiErr {
    ApiErr(StatusCode::NOT_IMPLEMENTED, format!("{what} arrives in {stage} of collaboration; this host doesn't offer it yet."))
}

/// Stage 1b, the agent tool bridge: a member's local agents call the host's tools through here, scoped to them.
async fn h_mcp(Extension(_m): Extension<Member>) -> Result<Json<Value>, ApiErr> {
    Err(not_yet("Stage 1b", "The agent tool bridge"))
}

/// Stage 2, thread transfer through the shared git repository. Members with `transfer_threads` (operators) take a
/// thread at once; others' requests become a decision for the thread's current holder.
async fn h_transfer(Extension(_m): Extension<Member>, Path(_id): Path<String>) -> Result<Json<Value>, ApiErr> {
    Err(not_yet("Stage 2", "Thread transfer"))
}

// ------------------------------------------------------------------------------------------------ owner

fn owner_only(who: &Who) -> Result<(), ApiErr> {
    // every Who is the owner (local windows or the owner's paired devices); members never get one
    let _ = who;
    Ok(())
}

async fn overview(State(core): State<CoreRef>, Extension(who): Extension<Who>) -> R {
    owner_only(&who)?;
    let rows = core.db.all(
        "SELECT p.id, p.name,
                (SELECT COUNT(*) FROM collab_members m WHERE m.project_id = p.id AND m.revoked_at IS NULL) AS members,
                (SELECT COUNT(*) FROM collab_invites i WHERE i.project_id = p.id AND i.used_at IS NULL AND i.revoked_at IS NULL AND i.expires_at > ?1) AS invites
         FROM projects p WHERE p.deleted_at IS NULL ORDER BY p.updated_at DESC",
        &[&util::now_ms()],
    )?;
    Ok(Json(json!({ "projects": rows, "perms": catalog(), "roles": ROLES })))
}

async fn project_admin(State(core): State<CoreRef>, Extension(who): Extension<Who>, Path(pid): Path<String>) -> R {
    owner_only(&who)?;
    let p = core.project(&pid)?;
    let members = core.db.all(
        "SELECT m.id, m.user_id, m.role, m.perms, m.requested_role, m.requested_at, m.created_at, m.last_seen, m.last_ip, u.name FROM collab_members m JOIN collab_users u ON u.id = m.user_id
         WHERE m.project_id = ?1 AND m.revoked_at IS NULL AND u.revoked_at IS NULL ORDER BY m.created_at",
        &[&pid],
    )?;
    let members: Vec<Value> = members
        .into_iter()
        .map(|mut m| {
            m["effective"] = effective(&core, &pid, &s(&m, "role"), &m["perms"]);
            m
        })
        .collect();
    let invites = core.db.all(
        "SELECT id, name, role, created_at, expires_at, used_at, revoked_at FROM collab_invites WHERE project_id = ?1 AND used_at IS NULL AND revoked_at IS NULL AND expires_at > ?2 ORDER BY created_at DESC",
        &[&pid, &util::now_ms()],
    )?;
    let rules: Map<String, Value> = ROLES.iter().map(|r| (r.to_string(), Value::Object(role_rules(&core, &pid, r)))).collect();
    let listening = core.sharing.lock().map(|s| s.is_some()).unwrap_or(false) || core.headless;
    Ok(Json(json!({ "project": { "id": pid, "name": p["name"] }, "members": members, "invites": invites, "rules": rules, "perms": catalog(), "roles": ROLES, "listening": listening })))
}

/// The address an invite link points at: the one the owner chose, or the best one this machine answers on.
async fn base_url(core: &CoreRef, chosen: &str) -> String {
    let chosen = chosen.trim().trim_end_matches('/');
    if chosen.starts_with("http://") || chosen.starts_with("https://") {
        return chosen.to_string();
    }
    let st = core.settings();
    let port = st["server"]["port"].as_u64().unwrap_or(4630) as u16;
    let tls = !st["server"]["tls_cert"].as_str().unwrap_or("").is_empty();
    let a = crate::net::addresses(core, port, tls).await;
    a["urls"].as_array().and_then(|u| u.first()).and_then(|u| u["url"].as_str()).unwrap_or("").to_string()
}

async fn create_invite(State(core): State<CoreRef>, Extension(who): Extension<Who>, Path(pid): Path<String>, Json(b): Json<Value>) -> R {
    owner_only(&who)?;
    core.project(&pid)?;
    let name = s(&b, "name");
    if name.is_empty() {
        return Err(bad("Who is this invite for? Give their name."));
    }
    let role = if s(&b, "role").is_empty() { "collaborator".to_string() } else { s(&b, "role") };
    if !ROLES.contains(&role.as_str()) {
        return Err(bad("role must be collaborator, operator or viewer"));
    }
    let hours = b["expires_hours"].as_i64().unwrap_or(168).clamp(1, 24 * 30);
    let id = util::id("inv");
    let secret = util::secret();
    let now = util::now_ms();
    let perms = Value::Object(clean_perms(&b["perms"])).to_string();
    core.db.exec(
        "INSERT INTO collab_invites(id, project_id, name, role, perms, secret_hash, created_at, expires_at) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8)",
        &[&id, &pid, &name, &role, &perms, &util::sha256(&secret), &now, &(now + hours * 3_600_000)],
    )?;
    let code = format!("{id}.{secret}");
    let base = base_url(&core, &s(&b, "base_url")).await;
    let link = format!("{base}/#/join?invite={code}");
    core.bus.emit("collab", Some(&pid), None, json!({ "invite": id }));
    tracing::info!("collab: invite {id} for {name} ({role}) on project {pid}");
    Ok(Json(json!({ "id": id, "code": code, "link": link, "qr": crate::net::qr_svg(&link), "expires_at": now + hours * 3_600_000 })))
}

async fn revoke_invite(State(core): State<CoreRef>, Extension(who): Extension<Who>, Path(id): Path<String>) -> R {
    owner_only(&who)?;
    let now = util::now_ms();
    let inv = core.db.one("SELECT project_id FROM collab_invites WHERE id = ?1", &[&id])?.ok_or_else(|| ApiErr(StatusCode::NOT_FOUND, "No such invite".into()))?;
    core.db.exec("UPDATE collab_invites SET revoked_at = ?1 WHERE id = ?2 AND revoked_at IS NULL", &[&now, &id])?;
    core.bus.emit("collab", inv["project_id"].as_str(), None, json!({ "invite": id }));
    Ok(Json(json!({ "ok": true })))
}

async fn patch_member(State(core): State<CoreRef>, Extension(who): Extension<Who>, Path(id): Path<String>, Json(b): Json<Value>) -> R {
    owner_only(&who)?;
    let m = core.db.one("SELECT * FROM collab_members WHERE id = ?1 AND revoked_at IS NULL", &[&id])?.ok_or_else(|| ApiErr(StatusCode::NOT_FOUND, "No such member".into()))?;
    if let Some(role) = b["role"].as_str() {
        if !ROLES.contains(&role) {
            return Err(bad("role must be collaborator, operator or viewer"));
        }
        core.db.exec("UPDATE collab_members SET role = ?1, requested_role = '' WHERE id = ?2", &[&role, &id])?;
    }
    // the owner answers a role request once: approve gives the role, decline clears the request
    if let Some(yes) = b["approve_request"].as_bool() {
        let asked = s(&m, "requested_role");
        if asked.is_empty() {
            return Err(bad("They haven't asked for anything."));
        }
        if yes {
            core.db.exec("UPDATE collab_members SET role = ?1, requested_role = '' WHERE id = ?2", &[&asked, &id])?;
        } else {
            core.db.exec("UPDATE collab_members SET requested_role = '' WHERE id = ?1", &[&id])?;
        }
    }
    if b.get("perms").is_some() {
        let perms = Value::Object(clean_perms(&b["perms"])).to_string();
        core.db.exec("UPDATE collab_members SET perms = ?1 WHERE id = ?2", &[&perms, &id])?;
    }
    core.bus.emit("collab", m["project_id"].as_str(), None, json!({ "member": id }));
    Ok(Json(json!({ "ok": true })))
}

async fn revoke_member(State(core): State<CoreRef>, Extension(who): Extension<Who>, Path(id): Path<String>) -> R {
    owner_only(&who)?;
    let m = core.db.one("SELECT project_id FROM collab_members WHERE id = ?1", &[&id])?.ok_or_else(|| ApiErr(StatusCode::NOT_FOUND, "No such member".into()))?;
    let now = util::now_ms();
    core.db.exec("UPDATE collab_members SET revoked_at = ?1 WHERE id = ?2 AND revoked_at IS NULL", &[&now, &id])?;
    core.bus.emit("collab", m["project_id"].as_str(), None, json!({ "member": id }));
    tracing::info!("collab: revoked member {id}");
    Ok(Json(json!({ "ok": true })))
}

async fn patch_rules(State(core): State<CoreRef>, Extension(who): Extension<Who>, Path(pid): Path<String>, Json(b): Json<Value>) -> R {
    owner_only(&who)?;
    core.project(&pid)?;
    let mut rules = rules_of(&core, &pid);
    for role in ROLES {
        if let Some(over) = b.get(*role) {
            let mut cur = clean_perms(&rules[*role]);
            cur.extend(clean_perms(over));
            rules[*role] = Value::Object(cur);
        }
    }
    let now = util::now_ms();
    core.db.exec(
        "INSERT INTO collab_rules(project_id, rules, updated_at) VALUES (?1, ?2, ?3) ON CONFLICT(project_id) DO UPDATE SET rules = excluded.rules, updated_at = excluded.updated_at",
        &[&pid, &rules.to_string(), &now],
    )?;
    core.bus.emit("collab", Some(&pid), None, json!({ "rules": true }));
    let out: Map<String, Value> = ROLES.iter().map(|r| (r.to_string(), Value::Object(role_rules(&core, &pid, r)))).collect();
    Ok(Json(Value::Object(out)))
}

// ------------------------------------------------------------------------------------------------ join (public)

static JOIN_FAILS: Mutex<Vec<i64>> = Mutex::new(Vec::new());

async fn join(State(core): State<CoreRef>, ConnectInfo(addr): ConnectInfo<SocketAddr>, Json(b): Json<Value>) -> R {
    let now = util::now_ms();
    {
        let mut f = JOIN_FAILS.lock().unwrap_or_else(|p| p.into_inner());
        f.retain(|t| *t > now - 10 * 60_000);
        if f.len() >= 12 {
            return Err(ApiErr(StatusCode::TOO_MANY_REQUESTS, "Too many wrong invites. Wait a few minutes and try again.".into()));
        }
    }
    let fail = |msg: &str| {
        JOIN_FAILS.lock().unwrap_or_else(|p| p.into_inner()).push(now);
        ApiErr(StatusCode::UNAUTHORIZED, msg.to_string())
    };
    let code = s(&b, "invite");
    let (id, secret) = code.split_once('.').ok_or_else(|| fail("That isn't an invite code."))?;
    let inv = core.db.one("SELECT * FROM collab_invites WHERE id = ?1", &[&id])?.ok_or_else(|| fail("That invite isn't valid on this Model Roamer."))?;
    if !util::same(&util::sha256(secret), &s(&inv, "secret_hash")) {
        return Err(fail("That invite isn't valid on this Model Roamer."));
    }
    if !inv["used_at"].is_null() {
        return Err(fail("That invite was already used. Ask the owner for a new one."));
    }
    if !inv["revoked_at"].is_null() {
        return Err(fail("That invite was withdrawn by the owner."));
    }
    if inv["expires_at"].as_i64().unwrap_or(0) < now {
        return Err(fail("That invite has expired. Ask the owner for a new one."));
    }
    let pid = s(&inv, "project_id");
    let p = core.project(&pid).map_err(|_| fail("The project this invite was for is gone."))?;
    let instance = s(&b, "instance_id");
    if instance == core.instance_id {
        return Err(bad("That invite is for this same Model Roamer. Open it on the other person's app."));
    }
    let name = if s(&b, "name").is_empty() { s(&inv, "name") } else { util::clip(&s(&b, "name"), 60) };
    // the same app joining again is the same person
    let existing = if instance.is_empty() { None } else { core.db.one("SELECT id FROM collab_users WHERE instance_id = ?1 AND revoked_at IS NULL", &[&instance])? };
    let user_id = match existing {
        Some(u) => {
            let uid = s(&u, "id");
            core.db.exec("UPDATE collab_users SET name = ?1 WHERE id = ?2", &[&name, &uid])?;
            uid
        }
        None => {
            let uid = util::id("usr");
            core.db.exec("INSERT INTO collab_users(id, name, instance_id, created_at) VALUES (?1, ?2, ?3, ?4)", &[&uid, &name, &instance, &now])?;
            uid
        }
    };
    // one live membership per person and project: joining again replaces the old token
    core.db.exec("UPDATE collab_members SET revoked_at = ?1 WHERE user_id = ?2 AND project_id = ?3 AND revoked_at IS NULL", &[&now, &user_id, &pid])?;
    let token = util::secret();
    let mid = util::id("mbr");
    let role = s(&inv, "role");
    let perms = inv["perms"].to_string();
    let ip = addr.ip().to_string();
    core.db.exec(
        "INSERT INTO collab_members(id, user_id, project_id, role, perms, token_hash, created_at, last_seen, last_ip) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?7, ?8)",
        &[&mid, &user_id, &pid, &role, &perms, &util::sha256(&token), &now, &ip],
    )?;
    core.db.exec("UPDATE collab_invites SET used_at = ?1, used_by = ?2 WHERE id = ?3", &[&now, &user_id, &id])?;
    core.bus.emit("collab", Some(&pid), None, json!({ "joined": mid }));
    core.bus.emit("toast", None, None, json!({ "text": format!("{name} joined {} as a {role}", s(&p, "name")) }));
    // the lead reads it with its next turn; a join doesn't cost a turn of its own
    if let Ok(l) = core.lead_thread(&pid) {
        let _ = runner::post(&core, l["id"].as_str().unwrap_or(""), "system", "Model Roamer", &format!("{name} joined this project as a {role} (from their own Model Roamer)."), json!({ "collab": "joined", "user_id": user_id }), false);
    }
    tracing::info!("collab: {name} ({user_id}) joined project {pid} from {ip}");
    Ok(Json(json!({
        "token": token,
        "member_id": mid,
        "user_id": user_id,
        "name": name,
        "role": role,
        "perms": effective(&core, &pid, &role, &inv["perms"]),
        "project": { "id": pid, "name": p["name"] },
        "host": { "id": core.instance_id, "name": core.display_name() },
    })))
}

// ------------------------------------------------------------------------------------------------ host (members)

/// Thread fields a member may see: no local paths, sessions or the owner's settings.
const THREAD_FIELDS: &[&str] = &[
    "id", "project_id", "parent_id", "kind", "title", "agent", "model", "status", "status_note", "task_kind", "instructions", "criteria", "branch",
    "base_branch", "merged", "resolved", "created_at", "updated_at", "user_id", "repo_id", "last_body", "message_count", "deleted_at",
];

fn pick(v: &Value, keys: &[&str]) -> Value {
    Value::Object(keys.iter().filter_map(|k| v.get(*k).map(|x| (k.to_string(), x.clone()))).collect())
}

fn clean_message(m: &Value) -> Value {
    let mut out = pick(m, &["id", "thread_id", "role", "author", "body", "created_at", "pending"]);
    // meta can carry local file paths: only what's safe goes out
    let files = m["meta"]["files"].as_array().map(|a| a.len()).unwrap_or(0);
    out["meta"] = json!({ "user_id": m["meta"]["user_id"], "files": files });
    out
}

/// May this member see this thread? Their own always; the owner's shared ones with view_threads; other people's own
/// chats only with view_others_chats.
fn can_see(m: &Member, t: &Value) -> bool {
    if t["project_id"].as_str() != Some(m.project_id.as_str()) {
        return false;
    }
    let owner = t["user_id"].as_str().unwrap_or("");
    if owner == m.user_id {
        return true;
    }
    if !owner.is_empty() && t["kind"] == "chat" {
        return m.can("view_others_chats");
    }
    m.can("view_threads")
}

fn visible_thread(core: &CoreRef, m: &Member, id: &str) -> Result<Value, ApiErr> {
    let t = core.thread(id).map_err(|_| ApiErr(StatusCode::NOT_FOUND, "No such thread".into()))?;
    if !can_see(m, &t) {
        // the same answer as a missing thread: members can't probe for ids
        return Err(ApiErr(StatusCode::NOT_FOUND, "No such thread".into()));
    }
    Ok(t)
}

fn project_view(core: &CoreRef, pid: &str) -> Result<Value, ApiErr> {
    let p = core.project(pid)?;
    let mut out = pick(&p, &["id", "name", "instructions", "lead_agent", "repo_url", "base_branch", "color", "created_at", "updated_at"]);
    let repos = core.db.all("SELECT id, name, kind, url, base_branch FROM repos WHERE project_id = ?1 AND deleted_at IS NULL ORDER BY created_at", &[&pid]).unwrap_or_default();
    // folder repos are paths on the owner's disk: members see that they exist, not where
    out["repos"] = json!(repos.into_iter().map(|mut r| {
        if r["kind"] != "github" {
            r["url"] = json!("");
        }
        r
    }).collect::<Vec<_>>());
    Ok(out)
}

fn people(core: &CoreRef, pid: &str) -> Vec<Value> {
    let mut v = vec![json!({ "user_id": "", "name": core.display_name(), "role": "owner" })];
    v.extend(
        core.db
            .all(
                "SELECT m.user_id, u.name, m.role, m.last_seen FROM collab_members m JOIN collab_users u ON u.id = m.user_id
                 WHERE m.project_id = ?1 AND m.revoked_at IS NULL AND u.revoked_at IS NULL ORDER BY m.created_at",
                &[&pid],
            )
            .unwrap_or_default(),
    );
    v
}

fn me_json(core: &CoreRef, m: &Member) -> Value {
    json!({
        "member": { "id": m.id, "user_id": m.user_id, "name": m.name, "role": m.role, "requested_role": m.requested_role },
        "perms": m.perms,
        "catalog": catalog(),
        "project": project_view(core, &m.project_id).unwrap_or(Value::Null),
        "host": { "id": core.instance_id, "name": core.display_name() },
    })
}

async fn h_me(State(core): State<CoreRef>, Extension(m): Extension<Member>) -> R {
    Ok(Json(me_json(&core, &m)))
}

fn threads_of(core: &CoreRef, m: &Member, since: i64) -> Result<Vec<Value>, ApiErr> {
    let rows = core.db.all(
        "SELECT t.*, (SELECT body FROM messages x WHERE x.thread_id = t.id AND x.role IN ('agent','user') ORDER BY created_at DESC LIMIT 1) AS last_body,
                (SELECT COUNT(*) FROM messages x WHERE x.thread_id = t.id) AS message_count
         FROM threads t WHERE t.project_id = ?1 AND (t.updated_at > ?2 OR t.deleted_at > ?2) AND (t.deleted_at IS NULL OR ?2 > 0)
         ORDER BY t.kind = 'lead' DESC, t.created_at LIMIT 500",
        &[&m.project_id, &since],
    )?;
    Ok(rows
        .into_iter()
        .filter(|t| can_see(m, t))
        .map(|mut t| {
            t["last_body"] = json!(util::clip(t["last_body"].as_str().unwrap_or(""), 240));
            pick(&t, THREAD_FIELDS)
        })
        .collect())
}

async fn h_project(State(core): State<CoreRef>, Extension(m): Extension<Member>) -> R {
    let cursor = util::now_ms() - 1;
    let mut me = me_json(&core, &m);
    me["threads"] = json!(threads_of(&core, &m, 0)?);
    me["people"] = json!(people(&core, &m.project_id));
    me["presence"] = json!(presence::snapshot(&core, &m.project_id, Some(&m)));
    me["notes"] = json!(notes::member_inbox(&core, &m)?);
    me["cursor"] = json!(cursor);
    Ok(Json(me))
}

const CHANGES_MAX_MESSAGES: i64 = 200;

/// Everything this member may see that changed after `since` (a host timestamp in ms), and the cursor to ask from
/// next. Rows sharing the cursor's millisecond can come twice; clients de-duplicate by id.
fn changes_since(core: &CoreRef, m: &Member, since: i64) -> Result<Value, ApiErr> {
    let started = util::now_ms();
    let threads = threads_of(core, m, since)?;
    let visible: std::collections::HashSet<String> = core
        .db
        .all("SELECT * FROM threads WHERE project_id = ?1 AND deleted_at IS NULL", &[&m.project_id])?
        .into_iter()
        .filter(|t| can_see(m, t))
        .filter_map(|t| t["id"].as_str().map(str::to_string))
        .collect();
    let raw = core.db.all(
        "SELECT x.* FROM messages x JOIN threads t ON t.id = x.thread_id WHERE t.project_id = ?1 AND x.created_at > ?2 ORDER BY x.created_at LIMIT ?3",
        &[&m.project_id, &since, &CHANGES_MAX_MESSAGES],
    )?;
    let more = raw.len() as i64 >= CHANGES_MAX_MESSAGES;
    let last_msg = raw.last().and_then(|x| x["created_at"].as_i64());
    let messages: Vec<Value> = raw.iter().filter(|x| visible.contains(x["thread_id"].as_str().unwrap_or(""))).map(|x| {
        let mut c = clean_message(x);
        c["body"] = json!(util::clip(c["body"].as_str().unwrap_or(""), 20_000));
        c
    }).collect();
    let global = m.can("read_global_memory");
    let mem = core.db.all(
        "SELECT id, project_id, name, description, length(body) AS size, updated_by, updated_at, deleted_at FROM memory
         WHERE (project_id = ?1 OR (?3 = 1 AND project_id IS NULL)) AND (updated_at > ?2 OR deleted_at > ?2)",
        &[&m.project_id, &since, &(global as i64)],
    )?;
    let arts = core.db.all(
        "SELECT id, project_id, thread_id, title, kind, description, version, created_at, updated_at, deleted_at FROM artifacts WHERE project_id = ?1 AND (updated_at > ?2 OR deleted_at > ?2)",
        &[&m.project_id, &since],
    )?;
    let arts: Vec<Value> = arts.into_iter().map(|a| with_url(core, a)).collect();
    let mut newest = since;
    for t in threads.iter().chain(&messages).chain(&mem).chain(&arts) {
        for k in ["updated_at", "created_at", "deleted_at"] {
            newest = newest.max(t[k].as_i64().unwrap_or(0));
        }
    }
    let mut cursor = newest.min(started - 1).max(since);
    if more {
        cursor = cursor.min(last_msg.unwrap_or(cursor));
    }
    Ok(json!({
        "threads": threads, "messages": messages, "memory": mem, "artifacts": arts, "cursor": cursor, "more": more,
        "people": people(core, &m.project_id), "presence": presence::snapshot(core, &m.project_id, Some(m)),
        "notes": notes::member_inbox(core, m)?, "perms": m.perms,
    }))
}

fn has_changes(v: &Value) -> bool {
    ["threads", "messages", "memory", "artifacts"].iter().any(|k| v[*k].as_array().is_some_and(|a| !a.is_empty()))
}

/// At most this many parked long-polls per member; more return at once.
const MAX_WAITERS: usize = 2;

fn waiters() -> &'static Mutex<HashMap<String, usize>> {
    static W: OnceLock<Mutex<HashMap<String, usize>>> = OnceLock::new();
    W.get_or_init(|| Mutex::new(HashMap::new()))
}

struct Waiter(String);
impl Drop for Waiter {
    fn drop(&mut self) {
        let mut w = waiters().lock().unwrap_or_else(|p| p.into_inner());
        if let Some(n) = w.get_mut(&self.0) {
            *n = n.saturating_sub(1);
            if *n == 0 {
                w.remove(&self.0);
            }
        }
    }
}

/// Incremental sync with a long poll: answers at once when something changed after `since`, otherwise waits (up to
/// `wait` seconds, max 25) for the project's next event. One parked request per guest instead of a polling loop.
async fn h_changes(State(core): State<CoreRef>, Extension(m): Extension<Member>, q: Q) -> R {
    let since: i64 = q.get("since").and_then(|x| x.parse().ok()).unwrap_or(0);
    let wait = q.get("wait").and_then(|x| x.parse::<u64>().ok()).unwrap_or(0).min(25);
    // subscribe before reading, so an event between the read and the wait isn't missed
    let mut rx = core.bus.subscribe();
    let first = changes_since(&core, &m, since)?;
    if wait == 0 || has_changes(&first) {
        return Ok(Json(first));
    }
    let _guard = {
        let mut w = waiters().lock().unwrap_or_else(|p| p.into_inner());
        let n = w.entry(m.id.clone()).or_insert(0);
        if *n >= MAX_WAITERS {
            return Ok(Json(first));
        }
        *n += 1;
        Waiter(m.id.clone())
    };
    let end = tokio::time::Instant::now() + Duration::from_secs(wait);
    loop {
        match tokio::time::timeout_at(end, rx.recv()).await {
            Err(_) => break, // time's up
            Ok(Ok(ev)) => {
                let ours = ev.project.as_deref() == Some(m.project_id.as_str()) || (ev.kind == "memory" && ev.project.is_none() && m.can("read_global_memory"));
                if ours && matches!(ev.kind.as_str(), "message" | "thread" | "memory" | "artifact" | "collab" | "project" | "presence" | "conflict-notes") {
                    // let a burst (a message and its thread update) land together
                    tokio::time::sleep(Duration::from_millis(150)).await;
                    break;
                }
            }
            Ok(Err(tokio::sync::broadcast::error::RecvError::Lagged(_))) => break,
            Ok(Err(_)) => break,
        }
    }
    let row = core.db.one(
        "SELECT m.role, m.perms, u.name FROM collab_members m JOIN collab_users u ON u.id = m.user_id
         JOIN projects p ON p.id = m.project_id AND p.deleted_at IS NULL
         WHERE m.id = ?1 AND m.revoked_at IS NULL AND u.revoked_at IS NULL",
        &[&m.id],
    )?.ok_or_else(|| ApiErr(StatusCode::UNAUTHORIZED, "The owner removed this membership.".into()))?;
    let mut m = m;
    m.role = s(&row, "role");
    m.name = s(&row, "name");
    m.perms = effective(&core, &m.project_id, &m.role, &row["perms"]);
    Ok(Json(changes_since(&core, &m, since)?))
}

async fn h_messages(State(core): State<CoreRef>, Extension(m): Extension<Member>, Path(id): Path<String>, q: Q) -> R {
    let t = visible_thread(&core, &m, &id)?;
    let before: i64 = q.get("before").and_then(|b| b.parse().ok()).unwrap_or(i64::MAX);
    let limit: i64 = q.get("limit").and_then(|b| b.parse().ok()).unwrap_or(80).clamp(1, 300);
    let rows = core.db.all("SELECT * FROM (SELECT * FROM messages WHERE thread_id = ?1 AND created_at < ?2 ORDER BY created_at DESC LIMIT ?3) ORDER BY created_at", &[&id, &before, &limit])?;
    Ok(Json(json!({ "thread": pick(&t, THREAD_FIELDS), "messages": rows.iter().map(clean_message).collect::<Vec<_>>() })))
}

async fn h_post_message(State(core): State<CoreRef>, Extension(m): Extension<Member>, Path(id): Path<String>, Json(b): Json<Value>) -> R {
    let t = visible_thread(&core, &m, &id)?;
    let text = s(&b, "text");
    if text.is_empty() {
        return Err(bad("Write a message first."));
    }
    if text.len() > 100_000 {
        return Err(bad("That message is too long."));
    }
    let own = t["user_id"].as_str() == Some(m.user_id.as_str());
    // a message to the owner's threads runs the owner's agents, on the owner's plan: its own permission
    if !own {
        m.need(if t["kind"] == "lead" { "message_lead" } else { "message_threads" })?;
    }
    if !own && matches!(t["status"].as_str(), Some("paused" | "failed" | "cancelled")) {
        core.set_status(&id, "idle", "")?;
    }
    // a member's own thread runs on their machine (Stage 1b), so it never wakes an agent here
    // only threads this machine runs get woken; a member's own thread runs on their machine
    let msg = runner::post(&core, &id, "user", &m.author(), &text, json!({ "user_id": m.user_id }), runs_here(&t))?;
    Ok(Json(clean_message(&msg)))
}

async fn h_create_thread(State(core): State<CoreRef>, Extension(m): Extension<Member>, Json(b): Json<Value>) -> R {
    m.need("create_threads")?;
    let title = util::clip(&s(&b, "title"), 120);
    if title.is_empty() {
        return Err(bad("A thread needs a title."));
    }
    let lead = core.lead_thread(&m.project_id)?;
    let id = util::id("th");
    let now = util::now_ms();
    let agent = if s(&b, "agent").is_empty() { "guest".to_string() } else { util::slug(&s(&b, "agent"), 40) };
    let note = format!("Runs on {}'s machine", m.name);
    let instructions = s(&b, "instructions");
    let lead_id = s(&lead, "id");
    // paused, with no waiting message: this machine's runner never picks it up (see runs_here)
    core.db.exec(
        "INSERT INTO threads(id, project_id, parent_id, kind, title, agent, status, status_note, instructions, created_at, updated_at, title_by, user_id)
         VALUES (?1, ?2, ?3, 'worker', ?4, ?5, 'paused', ?6, ?7, ?8, ?8, 'person', ?9)",
        &[&id, &m.project_id, &lead_id, &title, &agent, &note, &instructions, &now, &m.user_id],
    )?;
    let t = core.thread(&id)?;
    core.bus.emit("thread", Some(&m.project_id), Some(&id), t.clone());
    if !instructions.is_empty() {
        runner::post(&core, &id, "user", &m.author(), &instructions, json!({ "user_id": m.user_id }), false)?;
    }
    Ok(Json(pick(&core.thread(&id)?, THREAD_FIELDS)))
}

async fn h_merge(State(core): State<CoreRef>, Extension(m): Extension<Member>, Path(id): Path<String>, Json(b): Json<Value>) -> R {
    m.need("merge")?;
    let t = visible_thread(&core, &m, &id)?;
    let msg = if s(&b, "message").is_empty() { format!("{} (merged by {})", s(&t, "title"), m.name) } else { s(&b, "message") };
    let sha = runner::merge(&core, &t, &msg, b["squash"].as_bool().unwrap_or(false)).await?;
    runner::wake_lead(&core, &m.project_id, &format!("<wake reason=\"collaborator-merged\" thread=\"{id}\">{} merged \"{}\" into the base branch ({sha}).</wake>", m.name, s(&t, "title")), json!({}));
    Ok(Json(json!({ "sha": sha })))
}

async fn h_memory(State(core): State<CoreRef>, Extension(m): Extension<Member>) -> R {
    let rows = memory::list(&core, Some(m.project_id.as_str()))?;
    let global = m.can("read_global_memory");
    Ok(Json(json!(rows.into_iter().filter(|r| global || !r["project_id"].is_null()).collect::<Vec<_>>())))
}

async fn h_memory_entry(State(core): State<CoreRef>, Extension(m): Extension<Member>, Path(name): Path<String>) -> R {
    let e = memory::get_by_name(&core, Some(m.project_id.as_str()), &name)?.filter(|e| !e["project_id"].is_null() || m.can("read_global_memory"));
    let e = e.ok_or_else(|| ApiErr(StatusCode::NOT_FOUND, format!("No memory named '{name}'")))?;
    Ok(Json(pick(&e, &["id", "project_id", "name", "description", "body", "updated_by", "updated_at"])))
}

async fn h_save_memory(State(core): State<CoreRef>, Extension(m): Extension<Member>, Json(b): Json<Value>) -> R {
    m.need("edit_memory")?;
    // members write project memory only; global memory is the owner's (collaborator memory: memory_scope, Stage 1b)
    let e = memory::write(&core, Some(m.project_id.as_str()), &s(&b, "name"), &s(&b, "description"), b["body"].as_str().unwrap_or(""), &m.author())?;
    Ok(Json(pick(&e, &["id", "project_id", "name", "description", "updated_by", "updated_at"])))
}

fn with_url(core: &CoreRef, mut a: Value) -> Value {
    let id = s(&a, "id");
    a["url"] = json!(format!("/a/{id}/{}/", crate::api::artifact_sig(core, &id)));
    a
}

async fn h_artifacts(State(core): State<CoreRef>, Extension(m): Extension<Member>) -> R {
    let rows = core.db.all(
        "SELECT id, project_id, thread_id, title, kind, description, version, created_at, updated_at FROM artifacts WHERE project_id = ?1 AND deleted_at IS NULL ORDER BY updated_at DESC LIMIT 300",
        &[&m.project_id],
    )?;
    Ok(Json(json!(rows.into_iter().map(|a| with_url(&core, a)).collect::<Vec<_>>())))
}

async fn h_publish(State(core): State<CoreRef>, Extension(m): Extension<Member>, Json(b): Json<Value>) -> R {
    m.need("publish_artifacts")?;
    let kind = if s(&b, "kind").is_empty() { "markdown".to_string() } else { s(&b, "kind") };
    if !matches!(kind.as_str(), "markdown" | "html" | "text" | "svg") {
        return Err(bad("Members publish markdown, html, text or svg content."));
    }
    let content = b["content"].as_str().ok_or_else(|| bad("content is required"))?;
    // content only, never a path: a path would read the owner's disk
    let a = artifacts::publish(&core, Some(m.project_id.as_str()), None, &s(&b, "title"), &s(&b, "description"), &kind, None, Some(content), None, None)?;
    Ok(Json(with_url(&core, a)))
}

/// A member asks the owner for a role (today: operator). One open request at a time; the owner answers it once in
/// Settings › Collaborators and it never needs asking again per thread.
async fn h_request_role(State(core): State<CoreRef>, Extension(m): Extension<Member>, Json(b): Json<Value>) -> R {
    let role = if s(&b, "role").is_empty() { "operator".to_string() } else { s(&b, "role") };
    if role != "operator" {
        return Err(bad("You can ask to become an operator."));
    }
    if m.role == role {
        return Err(bad("You're already an operator."));
    }
    if !m.requested_role.is_empty() {
        return Err(ApiErr(StatusCode::CONFLICT, "You've already asked; the owner hasn't answered yet.".into()));
    }
    let now = util::now_ms();
    core.db.exec("UPDATE collab_members SET requested_role = ?1, requested_at = ?2 WHERE id = ?3", &[&role, &now, &m.id])?;
    core.bus.emit("collab", Some(&m.project_id), None, json!({ "request": m.id }));
    core.bus.emit("toast", None, None, json!({ "text": format!("{} asks to become an operator (Settings › Collaborators)", m.name) }));
    Ok(Json(json!({ "ok": true, "requested_role": role })))
}

async fn h_leave(State(core): State<CoreRef>, Extension(m): Extension<Member>) -> R {
    let now = util::now_ms();
    core.db.exec("UPDATE collab_members SET revoked_at = ?1 WHERE id = ?2", &[&now, &m.id])?;
    core.bus.emit("collab", Some(&m.project_id), None, json!({ "left": m.id }));
    if let Ok(l) = core.lead_thread(&m.project_id) {
        let _ = runner::post(&core, l["id"].as_str().unwrap_or(""), "system", "Model Roamer", &format!("{} left this project.", m.name), json!({ "collab": "left", "user_id": m.user_id }), false);
    }
    Ok(Json(json!({ "ok": true })))
}

// ------------------------------------------------------------------------------------------------ guest (this machine joined a host)

fn client() -> &'static reqwest::Client {
    static C: OnceLock<reqwest::Client> = OnceLock::new();
    C.get_or_init(|| reqwest::Client::builder().connect_timeout(Duration::from_secs(6)).build().unwrap_or_default())
}

/// `https://host:4630/#/join?invite=inv_x.SECRET` → ("https://host:4630", "inv_x.SECRET"). A bare host gets
/// http:// and the default port, like Connect to a server.
pub fn parse_invite(link: &str) -> Option<(String, String)> {
    let link = link.trim();
    let invite = link.split(['?', '&', '#']).find_map(|p| p.strip_prefix("invite="))?.trim().to_string();
    if invite.is_empty() || !invite.contains('.') {
        return None;
    }
    let with_scheme = if link.starts_with("http://") || link.starts_with("https://") { link.to_string() } else { format!("http://{link}") };
    let (scheme, rest) = with_scheme.split_once("://")?;
    let host = rest.split(['/', '?', '#']).next()?.to_string();
    if host.is_empty() {
        return None;
    }
    let has_port = host.rsplit_once(':').is_some_and(|(_, p)| !p.is_empty() && p.chars().all(|c| c.is_ascii_digit())) && !host.ends_with(']');
    let host = if !has_port && scheme == "http" { format!("{host}:4630") } else { host };
    Some((format!("{scheme}://{host}"), invite))
}

/// Plain http is fine on a private network or a tailnet (WireGuard underneath); elsewhere it would carry the token
/// in the clear.
fn insecure(url: &str) -> bool {
    let Some(rest) = url.strip_prefix("http://") else { return false };
    let host = rest.split('/').next().unwrap_or("");
    let host = host.rsplit_once(':').map(|(h, _)| h).unwrap_or(host).trim_matches(['[', ']']);
    if host == "localhost" || host.ends_with(".local") || host.ends_with(".lan") || host.ends_with(".ts.net") || !host.contains('.') && host.parse::<std::net::IpAddr>().is_err() {
        return false;
    }
    match host.parse::<std::net::IpAddr>() {
        Ok(std::net::IpAddr::V4(v)) => !(v.is_private() || v.is_loopback() || v.octets()[0] == 100 && (64..128).contains(&v.octets()[1])),
        Ok(std::net::IpAddr::V6(v)) => !(v.is_loopback() || (v.segments()[0] & 0xfe00) == 0xfc00),
        Err(_) => true, // a domain over plain http
    }
}

fn link_view(l: &Value) -> Value {
    let mut v = pick(l, &["id", "host_url", "host_id", "host_name", "project_id", "project_name", "member_id", "user_id", "name", "role", "perms", "status", "error", "created_at", "last_sync"]);
    v["insecure"] = json!(insecure(l["host_url"].as_str().unwrap_or("")));
    v
}

async fn list_links(State(core): State<CoreRef>, Extension(_who): Extension<Who>) -> R {
    let rows = core.db.all("SELECT * FROM collab_links ORDER BY created_at DESC", &[])?;
    Ok(Json(json!(rows.iter().map(link_view).collect::<Vec<_>>())))
}

async fn add_link(State(core): State<CoreRef>, Extension(_who): Extension<Who>, Json(b): Json<Value>) -> R {
    let (url, invite) = parse_invite(&s(&b, "link")).ok_or_else(|| bad("Paste the whole invite link the owner sent you (it has “invite=” in it)."))?;
    let hello: Value = async {
        let r = client().get(format!("{url}/api/hello")).timeout(Duration::from_secs(10)).send().await?;
        r.error_for_status()?.json::<Value>().await
    }
    .await
    .map_err(|e: reqwest::Error| e)
    .map_err(|e| bad(format!("Couldn't reach {url}: {e}. Check the owner's sharing is on and you're on the same network or tailnet.")))?;
    if hello["app"] != "model-roamer" {
        return Err(bad(format!("Something answered at {url}, but it isn't Model Roamer.")));
    }
    if hello["id"] == json!(core.instance_id) {
        return Err(bad("That invite is for this Model Roamer itself. Open it on the other person's app."));
    }
    let name = if s(&b, "name").is_empty() { core.display_name() } else { s(&b, "name") };
    let r = client()
        .post(format!("{url}/api/collab/join"))
        .timeout(Duration::from_secs(20))
        .json(&json!({ "invite": invite, "name": name, "instance_id": core.instance_id, "version": env!("CARGO_PKG_VERSION") }))
        .send()
        .await
        .map_err(|e| bad(format!("Couldn't reach {url}: {e}")))?;
    let status = r.status();
    let v: Value = r.json().await.unwrap_or_default();
    if !status.is_success() {
        return Err(bad(v["error"].as_str().map(str::to_string).unwrap_or_else(|| format!("The host said {status}"))));
    }
    let token = s(&v, "token");
    if token.is_empty() {
        return Err(bad("The host didn't send a token back."));
    }
    let id = util::id("lnk");
    let now = util::now_ms();
    let pid = v["project"]["id"].as_str().unwrap_or("").to_string();
    // joining the same host project again replaces the old link
    core.db.exec("DELETE FROM collab_links WHERE host_id = ?1 AND project_id = ?2", &[&v["host"]["id"].as_str().unwrap_or(""), &pid])?;
    core.db.exec(
        "INSERT INTO collab_links(id, host_url, host_id, host_name, project_id, project_name, member_id, user_id, name, role, perms, token, created_at, last_sync)
         VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13, ?13)",
        &[
            &id, &url, &v["host"]["id"].as_str().unwrap_or(""), &v["host"]["name"].as_str().unwrap_or(""), &pid, &v["project"]["name"].as_str().unwrap_or(""),
            &s(&v, "member_id"), &s(&v, "user_id"), &s(&v, "name"), &s(&v, "role"), &v["perms"].to_string(), &token, &now,
        ],
    )?;
    core.bus.emit("collab-links", None, None, json!({ "id": id }));
    tracing::info!("collab: joined {} on {url}", v["project"]["name"]);
    let l = core.db.one("SELECT * FROM collab_links WHERE id = ?1", &[&id])?.unwrap_or_default();
    Ok(Json(link_view(&l)))
}

async fn remove_link(State(core): State<CoreRef>, Extension(_who): Extension<Who>, Path(id): Path<String>) -> R {
    let l = core.db.one("SELECT * FROM collab_links WHERE id = ?1", &[&id])?.ok_or_else(|| ApiErr(StatusCode::NOT_FOUND, "No such project link".into()))?;
    // tell the host, so the owner sees it and the token dies there too; leaving works offline as well
    let told = client().post(format!("{}/api/collab/host/leave", s(&l, "host_url"))).bearer_auth(s(&l, "token")).timeout(Duration::from_secs(6)).send().await.map(|r| r.status().is_success()).unwrap_or(false);
    core.db.exec("DELETE FROM collab_links WHERE id = ?1", &[&id])?;
    core.bus.emit("collab-links", None, None, json!({ "id": id }));
    Ok(Json(json!({ "ok": true, "host_told": told })))
}

fn set_link_status(core: &CoreRef, l: &Value, status: &str, error: &str) {
    let id = s(l, "id");
    let changed = l["status"].as_str() != Some(status) || l["error"].as_str() != Some(error);
    let now = util::now_ms();
    if status == "ok" {
        let _ = core.db.exec("UPDATE collab_links SET status = ?1, error = ?2, last_sync = ?3 WHERE id = ?4", &[&status, &error, &now, &id]);
    } else {
        let _ = core.db.exec("UPDATE collab_links SET status = ?1, error = ?2 WHERE id = ?3", &[&status, &error, &id]);
    }
    if changed {
        core.bus.emit("collab-links", None, None, json!({ "id": id, "status": status }));
    }
}

/// The guest's windows reach the host only through here: the member token is added on this machine and never sent
/// to a browser. GETs are retried twice on connection errors; nothing that changes state is retried.
async fn proxy(State(core): State<CoreRef>, Extension(_who): Extension<Who>, method: Method, Path((id, path)): Path<(String, String)>, q: Q, body: Bytes) -> Response {
    let Some(l) = core.db.one("SELECT * FROM collab_links WHERE id = ?1", &[&id]).ok().flatten() else {
        return ApiErr(StatusCode::NOT_FOUND, "No such project link".into()).into_response();
    };
    if l["status"] == "revoked" {
        return ApiErr(StatusCode::FORBIDDEN, "The owner removed you from this project (or your invite was withdrawn). Ask them for a new invite.".into()).into_response();
    }
    if path.split('/').any(|seg| seg == ".." || seg.is_empty()) || !path.chars().all(|c| c.is_ascii_alphanumeric() || "_-./".contains(c)) {
        return bad("Bad path").into_response();
    }
    if method != Method::GET && method != Method::POST {
        return bad("Only GET and POST reach the host").into_response();
    }
    let wait = q.get("wait").and_then(|w| w.parse::<u64>().ok()).unwrap_or(0).min(25);
    let url = format!("{}/api/collab/host/{path}", s(&l, "host_url"));
    let timeout = Duration::from_secs(20 + wait);
    let tries = if method == Method::GET { 3 } else { 1 };
    let mut last = String::new();
    for attempt in 0..tries {
        let mut rq = client().request(method.clone(), &url).bearer_auth(s(&l, "token")).query(&q.0).timeout(timeout);
        if method == Method::POST {
            rq = rq.header(header::CONTENT_TYPE, "application/json").body(body.clone());
        }
        match rq.send().await {
            Ok(r) => {
                let st = r.status();
                let bytes = r.bytes().await.unwrap_or_default();
                if st == StatusCode::UNAUTHORIZED {
                    // not this window's own sign-in: a 401 here would send the guest's UI to the pairing page
                    let msg = "The owner removed you from this project (or you left it). Ask them for a new invite.";
                    set_link_status(&core, &l, "revoked", msg);
                    return ApiErr(StatusCode::FORBIDDEN, msg.into()).into_response();
                }
                set_link_status(&core, &l, "ok", "");
                return Response::builder().status(st).header(header::CONTENT_TYPE, "application/json").body(Body::from(bytes)).unwrap_or_else(|_| StatusCode::BAD_GATEWAY.into_response());
            }
            Err(e) => {
                last = if e.is_timeout() { "the host took too long to answer".into() } else { e.to_string() };
                if !(e.is_connect() || e.is_timeout()) || attempt + 1 == tries {
                    break;
                }
                tokio::time::sleep(Duration::from_millis(500 * 3u64.pow(attempt))).await;
            }
        }
    }
    let msg = format!("Can't reach {} right now ({last}). It will retry.", s(&l, "host_name"));
    set_link_status(&core, &l, "offline", &msg);
    ApiErr(StatusCode::BAD_GATEWAY, msg).into_response()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_invite_links() {
        assert_eq!(parse_invite("https://box.tail1.ts.net/#/join?invite=inv_1.abc"), Some(("https://box.tail1.ts.net".into(), "inv_1.abc".into())));
        assert_eq!(parse_invite("http://192.168.1.5:4630/#/join?invite=inv_1.abc&x=1"), Some(("http://192.168.1.5:4630".into(), "inv_1.abc".into())));
        assert_eq!(parse_invite("roamer-box/#/join?invite=inv_1.abc"), Some(("http://roamer-box:4630".into(), "inv_1.abc".into())));
        assert_eq!(parse_invite("http://x:4630/#/join"), None);
    }

    #[test]
    fn flags_plain_http_off_private_networks() {
        assert!(!insecure("http://192.168.1.5:4630"));
        assert!(!insecure("http://100.101.2.3:4630"));
        assert!(!insecure("http://box.tail1.ts.net:4630"));
        assert!(!insecure("http://roamer-box:4630"));
        assert!(!insecure("https://roamer.example.com"));
        assert!(insecure("http://roamer.example.com"));
        assert!(insecure("http://8.8.8.8:4630"));
    }

    #[test]
    fn permissions_layer() {
        let o = role_defaults("operator");
        assert_eq!(o["transfer_threads"], true);
        assert_eq!(role_defaults("collaborator")["transfer_threads"], false);
        let d = role_defaults("viewer");
        assert_eq!(d["view_threads"], true);
        assert_eq!(d["edit_memory"], false);
        let c = clean_perms(&json!({ "merge": true, "nonsense": true, "edit_memory": "yes" }));
        assert_eq!(c.len(), 1);
        assert_eq!(c["merge"], true);
    }
}
