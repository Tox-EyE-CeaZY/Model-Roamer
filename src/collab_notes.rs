//! Conflict notes are queued for human review. Delivery is idempotent and never schedules an agent turn.

use super::{presence, *};

const UNDO_MS: i64 = 10_000;
const RATE_WINDOW_MS: i64 = 60_000;
const RATE_MAX: i64 = 6;
const MAX_UNREAD: i64 = 20;

pub(crate) fn initialize(db: &crate::db::Db) -> anyhow::Result<()> {
    db.exec(
        "CREATE TABLE IF NOT EXISTS collab_conflict_notes (
            id TEXT PRIMARY KEY,
            project_id TEXT NOT NULL,
            sender TEXT NOT NULL,
            sender_name TEXT NOT NULL,
            recipient TEXT NOT NULL,
            thread_id TEXT NOT NULL,
            title TEXT NOT NULL,
            body TEXT NOT NULL,
            created_at INTEGER NOT NULL,
            decision TEXT NOT NULL DEFAULT '',
            decided_by TEXT,
            deliver_at INTEGER,
            received_at INTEGER
         )",
        &[],
    )?;
    db.exec("CREATE INDEX IF NOT EXISTS collab_notes_project ON collab_conflict_notes(project_id, recipient, decision, created_at)", &[])?;
    db.exec("CREATE INDEX IF NOT EXISTS collab_notes_rate ON collab_conflict_notes(project_id, sender, created_at)", &[])?;
    Ok(())
}

// An empty user ID is reserved for the host owner.
fn live_user(core: &CoreRef, project: &str, user: &str) -> bool {
    user.is_empty() || core.db.scalar_i64(
        "SELECT count(*) FROM collab_members m JOIN collab_users u ON u.id = m.user_id
         WHERE m.project_id = ?1 AND m.user_id = ?2 AND m.revoked_at IS NULL AND u.revoked_at IS NULL",
        &[&project, &user],
    ).unwrap_or(0) > 0
}

fn missing() -> ApiErr {
    ApiErr(
        StatusCode::NOT_FOUND,
        "This note or thread is no longer available.".into(),
    )
}

fn send(core: &CoreRef, project: &str, member: Option<&Member>, body: &Value) -> R {
    if let Some(m) = member {
        m.need("send_conflict_notes")?;
        m.need("view_threads")?;
        m.need("view_presence")?;
    }
    let sender = member.map_or("", |m| m.user_id.as_str());
    let target = body["recipient"]
        .as_str()
        .ok_or_else(|| bad("Choose a recipient."))?;
    let thread = s(body, "thread");
    let text = s(body, "text");
    if text.is_empty() || text.chars().count() > 1600 {
        return Err(bad("Write a note between 1 and 1,600 characters."));
    }
    if target == sender || !live_user(core, project, sender) || !live_user(core, project, target) {
        return Err(missing());
    }
    let view = presence::snapshot(core, project, member);
    let work = view
        .people
        .iter()
        .find(|p| p.user_id == target && p.online)
        .and_then(|p| p.work.iter().find(|w| w.thread == thread))
        .ok_or_else(missing)?;
    let name = member.map_or_else(|| core.display_name(), |m| m.name.clone());
    let id = util::id("cn");
    let now = util::now_ms();
    let changed = core.db.exec(
        "INSERT INTO collab_conflict_notes(id, project_id, sender, sender_name, recipient, thread_id, title, body, created_at)
         SELECT ?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9
         WHERE (SELECT count(*) FROM collab_conflict_notes WHERE project_id = ?2 AND sender = ?3 AND created_at > ?10) < ?11
           AND (SELECT count(*) FROM collab_conflict_notes WHERE project_id = ?2 AND recipient = ?5 AND sender = ?3 AND decision = '') < ?12",
        &[&id, &project, &sender, &name, &target, &thread, &work.title, &text, &now, &(now - RATE_WINDOW_MS), &RATE_MAX, &MAX_UNREAD],
    )?;
    if changed == 0 {
        return Err(ApiErr(
            StatusCode::TOO_MANY_REQUESTS,
            "Too many recent or unanswered notes. Wait before sending another.".into(),
        ));
    }
    changed_event(core, project);
    Ok(Json(json!({ "id": id })))
}

fn changed_event(core: &CoreRef, project: &str) {
    // The bus deliberately carries no note contents.
    core.bus
        .emit("conflict-notes", Some(project), None, json!({}));
}

fn operator(member: Option<&Member>) -> bool {
    member.is_none_or(|m| m.role == "operator" && m.can("review_conflict_notes"))
}

pub(super) fn rows(
    core: &CoreRef,
    project: &str,
    member: Option<&Member>,
) -> Result<Vec<Value>, ApiErr> {
    let user = member.map_or("", |m| m.user_id.as_str());
    let may_review = operator(member) as i64;
    let rows = core.db.all(
        "SELECT id, sender_name, sender, recipient, thread_id, title, body, created_at, decision, deliver_at
         FROM collab_conflict_notes
         WHERE project_id = ?1 AND decision = '' AND (recipient = ?2 OR (?3 = 1 AND sender != ?2))
         ORDER BY created_at DESC LIMIT 200",
        &[&project, &user, &may_review],
    )?;
    Ok(rows
        .into_iter()
        .filter_map(|mut n| {
            let recipient = s(&n, "recipient");
            if !live_user(core, project, &recipient) || !live_user(core, project, &s(&n, "sender"))
            {
                return None;
            }
            n["needs_you"] = json!(recipient == user);
            Some(n)
        })
        .collect())
}

pub(super) fn member_inbox(core: &CoreRef, m: &Member) -> Result<Vec<Value>, ApiErr> {
    rows(core, &m.project_id, Some(m))
}

fn decide(core: &CoreRef, project: &str, member: Option<&Member>, id: &str, body: &Value) -> R {
    let user = member.map_or("", |m| m.user_id.as_str());
    let n = core
        .db
        .one(
            "SELECT * FROM collab_conflict_notes WHERE id = ?1 AND project_id = ?2",
            &[&id, &project],
        )?
        .ok_or_else(missing)?;
    let recipient = s(&n, "recipient");
    if n["sender"] == user
        || (recipient != user && !operator(member))
        || !live_user(core, project, &recipient)
    {
        return Err(missing());
    }
    let now = util::now_ms();
    let action = s(body, "action");
    let changed = if action == "undo" {
        core.db.exec(
            "UPDATE collab_conflict_notes SET decision = '', decided_by = NULL, deliver_at = NULL
                      WHERE id = ?1 AND decided_by = ?2 AND deliver_at > ?3",
            &[&id, &user, &now],
        )?
    } else {
        if !matches!(action.as_str(), "approve" | "decline") {
            return Err(bad("Choose Approve, Decline or Undo."));
        }
        if !live_user(core, project, &s(&n, "sender")) {
            return Err(missing());
        }
        let deliver_at = now + UNDO_MS;
        core.db.exec(
            "UPDATE collab_conflict_notes SET decision = ?2, decided_by = ?3, deliver_at = ?4
                      WHERE id = ?1 AND decision = ''",
            &[&id, &action, &user, &deliver_at],
        )?
    };
    if changed == 0 {
        return Err(ApiErr(
            StatusCode::CONFLICT,
            "This note was already reviewed or its Undo window closed.".into(),
        ));
    }
    changed_event(core, project);
    Ok(Json(json!({ "ok": true, "undo_ms": UNDO_MS })))
}

pub(super) async fn owner_inbox(
    State(core): State<CoreRef>,
    Extension(who): Extension<Who>,
    Path(project): Path<String>,
) -> R {
    owner_only(&who)?;
    core.project(&project)?;
    Ok(Json(json!(rows(&core, &project, None)?)))
}

pub(super) async fn owner_send(
    State(core): State<CoreRef>,
    Extension(who): Extension<Who>,
    Path(project): Path<String>,
    Json(body): Json<Value>,
) -> R {
    owner_only(&who)?;
    core.project(&project)?;
    send(&core, &project, None, &body)
}

pub(super) async fn owner_decide(
    State(core): State<CoreRef>,
    Extension(who): Extension<Who>,
    Path((project, note)): Path<(String, String)>,
    Json(body): Json<Value>,
) -> R {
    owner_only(&who)?;
    core.project(&project)?;
    decide(&core, &project, None, &note, &body)
}

pub(super) async fn member_list(State(core): State<CoreRef>, Extension(m): Extension<Member>) -> R {
    Ok(Json(json!(member_inbox(&core, &m)?)))
}

pub(super) async fn member_send(
    State(core): State<CoreRef>,
    Extension(m): Extension<Member>,
    Json(body): Json<Value>,
) -> R {
    send(&core, &m.project_id, Some(&m), &body)
}

pub(super) async fn member_decide(
    State(core): State<CoreRef>,
    Extension(m): Extension<Member>,
    Path(id): Path<String>,
    Json(body): Json<Value>,
) -> R {
    decide(&core, &m.project_id, Some(&m), &id, &body)
}

pub(super) async fn local_count(State(core): State<CoreRef>, Extension(who): Extension<Who>) -> R {
    owner_only(&who)?;
    let rows = core.db.all(
        "SELECT DISTINCT n.project_id FROM collab_conflict_notes n JOIN projects p ON p.id = n.project_id
         WHERE n.recipient = '' AND n.decision = '' AND p.deleted_at IS NULL", &[])?;
    let mut waiting = Vec::new();
    for p in rows {
        let project_id = s(&p, "project_id");
        let project = core.project(&project_id)?;
        waiting.extend(self::rows(&core, &project_id, None)?.iter().filter(|n| n["needs_you"] == true).map(|n| json!({
            "id": n["id"], "project_id": project_id, "project": project["name"], "title": n["title"],
        })));
    }
    waiting.extend(presence::joined_attention(&core));
    Ok(Json(json!(waiting)))
}

pub(super) async fn member_delivery(
    State(core): State<CoreRef>,
    Extension(m): Extension<Member>,
) -> R {
    let notes = core.db.all(
        "SELECT id, sender_name, thread_id, title, body, deliver_at FROM collab_conflict_notes
         WHERE project_id = ?1 AND recipient = ?2 AND decision = 'approve' AND deliver_at <= ?3
           AND received_at IS NULL ORDER BY deliver_at, id LIMIT 100",
        &[&m.project_id, &m.user_id, &util::now_ms()],
    )?;
    Ok(Json(json!(notes)))
}

pub(super) async fn member_receipts(
    State(core): State<CoreRef>,
    Extension(m): Extension<Member>,
    Json(ids): Json<Vec<String>>,
) -> R {
    if ids.len() > 100 {
        return Err(bad("Too many note receipts."));
    }
    let now = util::now_ms();
    for id in ids {
        core.db.exec(
            "UPDATE collab_conflict_notes SET received_at = ?4 WHERE id = ?1 AND project_id = ?2 AND recipient = ?3
               AND decision = 'approve' AND deliver_at <= ?4 AND received_at IS NULL",
            &[&id, &m.project_id, &m.user_id, &now],
        )?;
    }
    Ok(Json(json!({ "ok": true })))
}

/// Only the machine which owns the session writes the visible message. The note ID doubles as a receipt ID.
pub(crate) fn deliver(core: &CoreRef, project: &str, note: &Value) -> anyhow::Result<bool> {
    let thread = s(note, "thread_id");
    let id = s(note, "id");
    let name: String = s(note, "sender_name")
        .chars()
        .filter(|c| !c.is_control() && !matches!(c, '<' | '>' | '"'))
        .take(80)
        .collect();
    let author = format!("from {name}");
    let body = s(note, "body");
    let now = util::now_ms();
    // pending = 2 is queued for the next turn explicitly started on this machine. It never schedules a turn.
    let changed = core.db.exec(
        "INSERT OR IGNORE INTO messages(id, thread_id, role, author, body, meta, pending, created_at)
         SELECT ?3, id, 'user', ?4, ?5, '{\"conflict_note\":true}', 2, ?6 FROM threads
         WHERE id = ?1 AND project_id = ?2 AND user_id = '' AND deleted_at IS NULL",
        &[&thread, &project, &id, &author, &body, &now],
    )?;
    if changed != 0 {
        let m = core
            .db
            .one("SELECT * FROM messages WHERE id = ?1", &[&id])?
            .unwrap_or_default();
        core.bus.emit("message", Some(project), Some(&thread), m);
    }
    Ok(changed != 0
        || core.db.scalar_i64(
            "SELECT count(*) FROM messages WHERE id = ?1 AND thread_id = ?2",
            &[&id, &thread],
        )? != 0)
}

pub(crate) fn deliver_owner(core: &CoreRef) -> anyhow::Result<()> {
    let now = util::now_ms();
    let notes = core.db.all(
        "SELECT n.* FROM collab_conflict_notes n JOIN projects p ON p.id = n.project_id
         JOIN threads t ON t.id = n.thread_id AND t.project_id = n.project_id AND t.user_id = '' AND t.deleted_at IS NULL
         WHERE n.recipient = '' AND n.decision = 'approve' AND n.deliver_at <= ?1 AND p.deleted_at IS NULL
           AND NOT EXISTS (SELECT 1 FROM messages m WHERE m.id = n.id) ORDER BY n.deliver_at LIMIT 100",
        &[&now],
    )?;
    for note in notes {
        deliver(core, &s(&note, "project_id"), &note)?;
    }
    Ok(())
}
