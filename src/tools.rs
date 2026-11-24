//! Model Roamer's own tools, the same for every agent: memory, threads, messages, artifacts, git and GitHub.
//! Agents reach them through the MCP server (`model-roamer mcp`) or, for CLIs without MCP, a text protocol.
//! Which tools an agent sees depends on its role: the lead gets the orchestration tools.

use crate::{
    core::CoreRef,
    git, github, runner,
    util::{self, clip},
};
use anyhow::{anyhow, bail, Result};
use serde_json::{json, Value};
use std::path::PathBuf;

struct Def {
    name: &'static str,
    roles: &'static [&'static str],
    desc: &'static str,
    schema: fn() -> Value,
}

const ALL: &[&str] = &["lead", "worker", "review", "chat"];
const LEAD: &[&str] = &["lead"];
const WORK: &[&str] = &["lead", "worker", "review"];

fn obj(props: Value, req: &[&str]) -> Value {
    json!({ "type": "object", "properties": props, "required": req, "additionalProperties": false })
}

const DEFS: &[Def] = &[
    Def { name: "who_is_working_on", roles: ALL, desc: "Read a compact live summary of other collaborators' threads, branches, status and claims. Pass a title or file paths to check a proposed task for overlaps.", schema: || obj(json!({
        "title": {"type":"string"}, "paths": {"type":"array", "items":{"type":"string"}}
    }), &[]) },
    // ---------------------------------------------------------------- orchestration (lead)
    Def { name: "spawn_thread", roles: LEAD, desc: "Open a worker thread for one task and hand it to an agent. It starts right away (or after the threads in depends_on finish). Give clear instructions and acceptance criteria. Returns the thread id.", schema: || obj(json!({
        "title": {"type": "string", "description": "Short title, verb first, 3-6 words"},
        "instructions": {"type": "string", "description": "The task: what to do, context, files/areas it owns, what not to touch"},
        "criteria": {"type": "string", "description": "Acceptance criteria: what done looks like and how to verify it"},
        "agent": {"type": "string", "description": "claude | codex | antigravity | a custom agent id | auto (route by task_kind and headroom)"},
        "task_kind": {"type": "string", "description": "plan | core | refactor | review | docs | design | frontend | creative | research | backend | api | data | infra | scripts | tests | general"},
        "model": {"type": "string", "description": "Optional model for that agent"},
        "effort": {"type": "string", "description": "Optional thinking strength for this thread (low | medium | high | xhigh | max, as the model allows). Use lower for simple edits and higher for hard design or debugging; leave empty for the agent's default."},
        "workspace": {"type": "string", "enum": ["worktree", "shared", "scratch"], "description": "worktree (default when the project has a git repo): its own branch and folder; shared: the project folder itself (read-only work, research); scratch: an empty folder"},
        "depends_on": {"type": "array", "items": {"type": "string"}, "description": "Thread ids that must finish (review/done) first"},
        "repo": {"type": "string", "description": "Which of the project's repositories it works on (an id from your instructions, or its name). Default: the primary."},
        "allow_limited": {"type": "boolean", "description": "Open the thread even though that agent is out of usage right now (it waits for the reset). Default false: you're told who has room instead."}
    }), &["title", "instructions"]) },
    Def { name: "agent_status", roles: LEAD, desc: "Each agent and model tier's state right now: ready, out of usage (and when it resets), signed out, how much of its 5-hour and weekly allowance is used, and how many threads it is running. Check it before routing work, and when you get an agent-limit wake.", schema: || obj(json!({}), &[]) },
    Def { name: "send_message", roles: LEAD, desc: "Send a message to a thread (answers, review notes, new instructions). If the thread is mid-turn it is interrupted so it reads the message at once, then carries on with its session (pass interrupt=false to let it finish its turn first).", schema: || obj(json!({
        "thread_id": {"type": "string"}, "text": {"type": "string"}, "interrupt": {"type": "boolean", "description": "Default true. false queues the message until the thread's current turn ends."}
    }), &["thread_id", "text"]) },
    Def { name: "list_threads", roles: LEAD, desc: "List this project's threads with agent, status, branch and last activity.", schema: || obj(json!({
        "status": {"type": "string", "description": "Optional filter: working | waiting | blocked | review | done | failed | paused | limited | open (anything unresolved)"}
    }), &[]) },
    Def { name: "read_thread", roles: LEAD, desc: "Read a thread's recent messages (newest last).", schema: || obj(json!({
        "thread_id": {"type": "string"}, "limit": {"type": "integer", "description": "Messages to return (default 12)"}
    }), &["thread_id"]) },
    Def { name: "check_status", roles: LEAD, desc: "Overview: every open thread's state, what each agent is doing, which agents are signed in or out of usage, and pending decisions.", schema: || obj(json!({}), &[]) },
    Def { name: "route_task", roles: LEAD, desc: "Which agent and model should take a task, based on routing rules and available usage.", schema: || obj(json!({
        "task_kind": {"type": "string"},
        "model": {"type": "string", "description": "Optional model to check for this task"}
    }), &["task_kind"]) },
    Def { name: "reassign_thread", roles: LEAD, desc: "Move a thread to a different agent (e.g. when one hits its usage limit). The new agent gets the thread's history.", schema: || obj(json!({
        "thread_id": {"type": "string"}, "agent": {"type": "string"}, "model": {"type": "string"}, "note": {"type": "string", "description": "Optional message for the new agent"}
    }), &["thread_id", "agent"]) },
    Def { name: "cancel_thread", roles: LEAD, desc: "Stop a thread's running turn and mark it cancelled.", schema: || obj(json!({
        "thread_id": {"type": "string"}, "reason": {"type": "string"}
    }), &["thread_id"]) },
    Def { name: "pause_thread", roles: LEAD, desc: "Pause a thread: its current turn stops and new messages wait until it's resumed.", schema: || obj(json!({
        "thread_id": {"type": "string"}
    }), &["thread_id"]) },
    Def { name: "resume_thread", roles: LEAD, desc: "Resume a paused, failed, limited or cancelled thread (runs any waiting messages).", schema: || obj(json!({
        "thread_id": {"type": "string"}, "note": {"type": "string", "description": "Optional message to send with it"}
    }), &["thread_id"]) },
    Def { name: "resolve_thread", roles: LEAD, desc: "Mark a thread finished (hidden from the open list; it can be reopened). Removes its worktree once merged.", schema: || obj(json!({
        "thread_id": {"type": "string"}
    }), &["thread_id"]) },
    Def { name: "park_thread", roles: LEAD, desc: "Keep a finished-for-now thread open and idle because its context is worth having for follow-ups (the author of a subsystem, a reviewer, a designer). It costs nothing while idle and its file claims are released. Model Roamer closes parked threads on its own after a quiet spell; reopen_thread brings one back.", schema: || obj(json!({
        "thread_id": {"type": "string"}, "why": {"type": "string", "description": "What it is kept for"}
    }), &["thread_id"]) },
    Def { name: "reopen_thread", roles: LEAD, desc: "Bring a resolved or auto-closed thread back, with its session and context, optionally with a message for it to act on.", schema: || obj(json!({
        "thread_id": {"type": "string"}, "message": {"type": "string"}
    }), &["thread_id"]) },
    Def { name: "review_diff", roles: LEAD, desc: "The thread's changes against the base branch (committed and uncommitted), as a unified diff.", schema: || obj(json!({
        "thread_id": {"type": "string"}, "max_chars": {"type": "integer"}
    }), &["thread_id"]) },
    Def { name: "merge_result", roles: LEAD, desc: "Commit anything left in the thread's worktree and merge its branch into the project's base branch. Stops cleanly and lists files on conflicts.", schema: || obj(json!({
        "thread_id": {"type": "string"}, "message": {"type": "string"}, "squash": {"type": "boolean"}
    }), &["thread_id"]) },
    Def { name: "request_review", roles: LEAD, desc: "Ask a different agent to cross-review a thread's work. Opens a review thread; you get its verdict as a wake.", schema: || obj(json!({
        "thread_id": {"type": "string"}, "agent": {"type": "string", "description": "Reviewer agent (default: a different strong agent than the author)"}, "focus": {"type": "string"}
    }), &["thread_id"]) },
    Def { name: "ask_user", roles: LEAD, desc: "Put one decision in front of the person: a question, 2-4 options and the one you recommend. Their choice comes back as a wake. Keep working on the recommended option meanwhile unless it can't be undone.", schema: || obj(json!({
        "question": {"type": "string"},
        "options": {"type": "array", "items": {"type": "object", "properties": {"label": {"type": "string"}, "consequence": {"type": "string"}}, "required": ["label"]}},
        "recommended": {"type": "integer", "description": "Index of the recommended option, from 0"}
    }), &["question", "options"]) },
    // ---------------------------------------------------------------- for everyone
    Def { name: "read_memory", roles: ALL, desc: "Read project and global memory. No arguments: the index (name and description of every entry). name: one entry in full. query: full-text search.", schema: || obj(json!({
        "name": {"type": "string"}, "query": {"type": "string"}
    }), &[]) },
    Def { name: "write_memory", roles: ALL, desc: "Create or update a memory entry (one subject per entry; update rather than duplicate). Visible to every agent and the person.", schema: || obj(json!({
        "name": {"type": "string", "description": "short-kebab-case"}, "description": {"type": "string", "description": "One line: what this entry is for"},
        "body": {"type": "string"}, "scope": {"type": "string", "enum": ["project", "global"], "description": "project (default) or global (shared by all projects)"}
    }), &["name", "body"]) },
    Def { name: "delete_memory", roles: ALL, desc: "Delete a memory entry that turned out to be wrong or obsolete.", schema: || obj(json!({
        "name": {"type": "string"}, "scope": {"type": "string", "enum": ["project", "global"]}
    }), &["name"]) },
    Def { name: "publish_artifact", roles: ALL, desc: "Publish something to look at (HTML page, markdown, SVG, image, PDF, text) so the person can open it in Model Roamer on any device. Give a file path in your workspace, or the content itself. Same title updates it. It is filed under this project and thread automatically; pass folder to put it somewhere else.", schema: || obj(json!({
        "title": {"type": "string"}, "folder": {"type": "string", "description": "Optional folder path, made if missing, e.g. 'Designs/Mockups' (inside the project's folder; a leading / starts from the top of the library)"}, "path": {"type": "string", "description": "A file (or a folder with index.html) in your workspace"},
        "content": {"type": "string", "description": "Or the content itself"}, "kind": {"type": "string", "enum": ["html", "markdown", "svg", "image", "pdf", "text"]},
        "description": {"type": "string"}
    }), &["title"]) },
    Def { name: "list_artifacts", roles: ALL, desc: "The artifact folders for this project (or your chat) and what is in each, to see how files are organized before publishing or moving.", schema: || obj(json!({}), &[]) },
    Def { name: "move_artifact", roles: ALL, desc: "Move an artifact (by title or id) into a folder path, made if missing, e.g. 'Reports/2026'. Use it to keep the project's files organized.", schema: || obj(json!({
        "artifact": {"type": "string"}, "folder": {"type": "string"}
    }), &["artifact", "folder"]) },
    Def { name: "post_update", roles: ALL, desc: "Show a short progress line on your thread without ending your turn (e.g. 'Tests pass, writing docs').", schema: || obj(json!({
        "text": {"type": "string"}
    }), &["text"]) },
    Def { name: "message_lead", roles: &["worker", "review"], desc: "Write to the project's lead without ending your turn: ask for more work or a decision, report something it should act on (a blocker, a finding, a need for another agent), or ask for help. The lead is woken, can answer you with send_message, and may open more threads. Keep working on what you can meanwhile.", schema: || obj(json!({
        "text": {"type": "string"}, "kind": {"type": "string", "enum": ["question", "report", "request"]}
    }), &["text"]) },
    Def { name: "set_title", roles: ALL, desc: "Name your chat or thread so the person can find it in a list: lead with the verb of their goal, 3 to 6 words, sentence case, in their words (e.g. 'Fix Safari export button'). Call it after the first exchange and again when the goal changes. Lead: pass thread_id to name a worker's thread.", schema: || obj(json!({
        "title": {"type": "string"}, "thread_id": {"type": "string"}
    }), &["title"]) },
    Def { name: "set_status", roles: WORK, desc: "Set your thread's status: review (ready for the lead to check), done, blocked (say why), working.", schema: || obj(json!({
        "status": {"type": "string", "enum": ["working", "review", "done", "blocked"]}, "note": {"type": "string"}
    }), &["status"]) },
    Def { name: "claim_paths", roles: WORK, desc: "Claim files or folders you are changing so agents working at the same time stay clear. Reports any conflicting claims.", schema: || obj(json!({
        "paths": {"type": "array", "items": {"type": "string"}}, "note": {"type": "string"}
    }), &["paths"]) },
    Def { name: "release_paths", roles: WORK, desc: "Release your claims (all of them, or the given paths).", schema: || obj(json!({
        "paths": {"type": "array", "items": {"type": "string"}}
    }), &[]) },
    Def { name: "list_claims", roles: WORK, desc: "Who has claimed what in this project.", schema: || obj(json!({}), &[]) },
    Def { name: "git_status", roles: ALL, desc: "Your workspace's branch, changed files and recent commits.", schema: || obj(json!({}), &[]) },
    Def { name: "commit_work", roles: WORK, desc: "Commit everything in your workspace with a message.", schema: || obj(json!({
        "message": {"type": "string"}
    }), &["message"]) },
    Def { name: "open_pull_request", roles: WORK, desc: "Commit, push this thread's branch to GitHub (using Model Roamer's GitHub sign-in) and open a pull request against the base branch. Lead: pass thread_id to do it for a worker's branch.", schema: || obj(json!({
        "title": {"type": "string"}, "body": {"type": "string"}, "draft": {"type": "boolean"}, "thread_id": {"type": "string"}
    }), &["title"]) },
    Def { name: "list_pull_requests", roles: WORK, desc: "The project repository's pull requests on GitHub.", schema: || obj(json!({
        "state": {"type": "string", "enum": ["open", "closed", "all"]}
    }), &[]) },
    Def { name: "pull_request_checks", roles: WORK, desc: "A pull request's state and CI check results.", schema: || obj(json!({
        "number": {"type": "integer"}
    }), &["number"]) },
    Def { name: "comment_on_pull_request", roles: WORK, desc: "Post a comment on a pull request.", schema: || obj(json!({
        "number": {"type": "integer"}, "body": {"type": "string"}
    }), &["number", "body"]) },
];

pub fn role_of(thread: &Value) -> &'static str {
    match thread["kind"].as_str().unwrap_or("") {
        "lead" => "lead",
        "worker" => "worker",
        "review" => "review",
        _ => "chat",
    }
}

/// The MCP tool list for a thread's role.
pub fn list(role: &str) -> Vec<Value> {
    DEFS.iter().filter(|d| d.roles.contains(&role)).map(|d| json!({ "name": d.name, "description": d.desc, "inputSchema": (d.schema)() })).collect()
}

/// The tools as text, for the instructions (and for agents using the text protocol).
pub fn describe(role: &str) -> String {
    let mut out = String::new();
    for d in DEFS.iter().filter(|d| d.roles.contains(&role)) {
        let sch = (d.schema)();
        let props = sch["properties"].as_object().cloned().unwrap_or_default();
        let req: Vec<String> = sch["required"].as_array().cloned().unwrap_or_default().iter().filter_map(|x| x.as_str().map(str::to_string)).collect();
        let args: Vec<String> = props.keys().map(|k| if req.contains(k) { k.clone() } else { format!("{k}?") }).collect();
        out.push_str(&format!("- `{}({})`: {}\n", d.name, args.join(", "), d.desc));
    }
    out
}

/// Run a tool for the agent working in `caller`. The result is text for the agent.
pub async fn call(core: &CoreRef, caller: &Value, name: &str, args: &Value) -> Result<String> {
    let role = role_of(caller);
    let def = DEFS.iter().find(|d| d.name == name).ok_or_else(|| anyhow!("Unknown tool {name}"))?;
    if !def.roles.contains(&role) {
        bail!("{name} is for the lead only. Ask the lead in your reply instead.");
    }
    let tid = caller["id"].as_str().unwrap_or("").to_string();
    let project = caller["project_id"].as_str().map(str::to_string);
    let a = |k: &str| args[k].as_str().unwrap_or("").trim().to_string();
    let need_project = || project.clone().ok_or_else(|| anyhow!("This thread isn't part of a project."));
    let same_project = |t: &Value| -> Result<()> {
        if t["project_id"].as_str() != project.as_deref() {
            bail!("That thread belongs to another project.");
        }
        Ok(())
    };
    tracing::info!(thread = %tid, tool = name, "tool call");
    match name {
        "who_is_working_on" => {
            let activity = crate::collab::presence::digest(core, caller);
            let work = crate::collab::presence::Work {
                title: a("title"), kind: "worker".into(), paths: proposed_claims(args), ..Default::default()
            };
            let overlaps = crate::collab::presence::caller_overlaps(core, caller, &work);
            if !overlaps.is_empty() {
                return Ok(format!("{activity}\nProposed work overlaps: {}", json!(overlaps)));
            }
            Ok(if activity.is_empty() { "No other active collaboration work is visible.".into() } else { activity })
        }
        "agent_status" => Ok(runner::fleet_now(core)),
        "spawn_thread" => {
            let p = need_project()?;
            let proposed = crate::collab::presence::Work { title: a("title"), kind: "worker".into(), ..Default::default() };
            if !crate::collab::presence::preflight(core, caller, proposed).is_empty() {
                bail!("This task overlaps another collaborator's work. The project chat shows the overlap and lets the person send a note. Tell the person before continuing with this task.");
            }
            let deps: Vec<String> = args["depends_on"].as_array().map(|d| d.iter().filter_map(|x| x.as_str().map(str::to_string)).collect()).unwrap_or_default();
            let t = runner::create_worker(
                core,
                &p,
                runner::NewWorker {
                    title: a("title"),
                    instructions: a("instructions"),
                    criteria: a("criteria"),
                    agent: a("agent"),
                    task_kind: a("task_kind"),
                    model: a("model"),
                    allow_limited: args["allow_limited"] == true,
                    effort: a("effort"),
                    workspace: a("workspace"),
                    repo: a("repo"),
                    depends_on: deps,
                    parent: Some(tid.clone()),
                    kind: "worker".into(),
                },
            )
            .await?;
            Ok(format!(
                "Opened thread {} \"{}\" with {} ({}){}. You'll get a wake when it replies.",
                t["id"].as_str().unwrap_or(""),
                t["title"].as_str().unwrap_or(""),
                t["agent"].as_str().unwrap_or(""),
                runner::model_label(t["model"].as_str().unwrap_or("")),
                if t["status"] == "waiting" { " (waiting for its dependencies)" } else { "" }
            ))
        }
        "send_message" => {
            let t = core.thread(&a("thread_id"))?;
            same_project(&t)?;
            runner::post(core, t["id"].as_str().unwrap(), "user", &format!("Lead ({})", caller["agent"].as_str().unwrap_or("")), &a("text"), json!({ "from_thread": tid }), true)?;
            let id = t["id"].as_str().unwrap();
            if args["interrupt"] != false && runner::turn_active(core, id) {
                runner::cancel(core, id, "idle", "so it can read the lead's message").await;
                return Ok("Sent. The thread was mid-turn, so it was interrupted to read it; it picks the message up with its session intact.".into());
            }
            Ok("Sent. The thread is working on it.".into())
        }
        "list_threads" => {
            let p = need_project()?;
            let filter = a("status");
            let rows = core.db.all("SELECT id, kind, title, agent, model, status, status_note, branch, repo_id, resolved, parked_at, merged, task_kind, updated_at, context FROM threads WHERE project_id = ?1 AND deleted_at IS NULL AND kind != 'lead' ORDER BY created_at", &[&p])?;
            let rows: Vec<Value> = rows
                .into_iter()
                .filter(|r| match filter.as_str() {
                    "" => true,
                    "open" => r["resolved"] == 0,
                    "parked" => r["resolved"] == 0 && r["parked_at"].as_i64().is_some(),
                    f => r["status"] == f,
                })
                .collect();
            if rows.is_empty() {
                return Ok("No threads yet.".into());
            }
            Ok(rows.iter().map(thread_line).collect::<Vec<_>>().join("\n"))
        }
        "read_thread" => {
            let t = core.thread(&a("thread_id"))?;
            same_project(&t)?;
            let n = args["limit"].as_i64().unwrap_or(12).clamp(1, 60);
            let msgs = core.db.all(
                "SELECT * FROM (SELECT role, author, body, created_at FROM messages WHERE thread_id = ?1 ORDER BY created_at DESC LIMIT ?2) ORDER BY created_at",
                &[&t["id"].as_str().unwrap(), &n],
            )?;
            let mut out = format!("{}\n\n", thread_line(&t));
            for m in msgs {
                out.push_str(&format!("[{}] {}: {}\n\n", m["role"].as_str().unwrap_or(""), m["author"].as_str().unwrap_or(""), clip(m["body"].as_str().unwrap_or(""), 6000)));
            }
            Ok(out)
        }
        "check_status" => {
            let p = need_project()?;
            Ok(runner::overview(core, &p))
        }
        "route_task" => Ok(runner::route(core, project.as_deref(), &a("task_kind"), &a("model")).map(|(agent, model, why)| format!("{agent} ({}): {why}\n\n{}", runner::model_label(&model), runner::fleet_now(core))).unwrap_or_else(|| format!("No agent is available right now.\n\n{}", runner::fleet_now(core)))),
        "reassign_thread" => {
            let t = core.thread(&a("thread_id"))?;
            same_project(&t)?;
            runner::reassign(core, t["id"].as_str().unwrap(), &a("agent"), &a("model"), &a("note")).await?;
            Ok(format!("Moved to {}. It picks up with the thread's history.", a("agent")))
        }
        "cancel_thread" => {
            let t = core.thread(&a("thread_id"))?;
            same_project(&t)?;
            runner::cancel(core, t["id"].as_str().unwrap(), "cancelled", &a("reason")).await;
            Ok("Cancelled.".into())
        }
        "pause_thread" => {
            let t = core.thread(&a("thread_id"))?;
            same_project(&t)?;
            runner::cancel(core, t["id"].as_str().unwrap(), "paused", "Paused by the lead").await;
            Ok("Paused.".into())
        }
        "resume_thread" => {
            let t = core.thread(&a("thread_id"))?;
            same_project(&t)?;
            let id = t["id"].as_str().unwrap();
            core.set_status(id, "idle", "")?;
            let note = a("note");
            if !note.is_empty() {
                runner::post(core, id, "user", "Lead", &note, json!({ "from_thread": tid }), true)?;
                if runner::turn_active(core, id) {
                    runner::cancel(core, id, "idle", "so it can read the lead's note").await;
                }
            } else {
                runner::schedule(core, id);
            }
            Ok("Resumed.".into())
        }
        "resolve_thread" => {
            let t = core.thread(&a("thread_id"))?;
            same_project(&t)?;
            runner::resolve(core, t["id"].as_str().unwrap(), true).await?;
            Ok("Resolved.".into())
        }
        "park_thread" => {
            let t = core.thread(&a("thread_id"))?;
            same_project(&t)?;
            let id = t["id"].as_str().unwrap();
            if t["resolved"] == 1 {
                runner::resolve(core, id, false).await?;
                core.set_status(id, "idle", "")?;
            }
            core.set_thread(id, &json!({ "parked_at": util::now_ms() }))?;
            core.db.exec("DELETE FROM claims WHERE thread_id = ?1", &[&id])?;
            let why = a("why");
            runner::post(core, id, "system", "Model Roamer", &if why.is_empty() { "Parked by the lead: kept open and idle.".to_string() } else { format!("Parked by the lead: kept open and idle for {why}.") }, json!({}), false)?;
            Ok(format!("Parked. It stays open and idle, and closes by itself after {} hours unused (reopen_thread brings it back).", core.settings()["idle_close_hours"].as_i64().unwrap_or(24)))
        }
        "reopen_thread" => {
            let t = core.thread(&a("thread_id"))?;
            same_project(&t)?;
            let id = t["id"].as_str().unwrap();
            runner::resolve(core, id, false).await?;
            core.set_thread(id, &json!({ "parked_at": Value::Null }))?;
            core.set_status(id, "idle", "")?;
            let m = a("message");
            if m.is_empty() {
                Ok("Reopened and idle. Send it a message to give it work.".into())
            } else {
                runner::post(core, id, "user", &format!("Lead ({})", caller["agent"].as_str().unwrap_or("")), &m, json!({ "from_thread": tid }), true)?;
                Ok("Reopened; it's working on your message.".into())
            }
        }
        "review_diff" => {
            let t = core.thread(&a("thread_id"))?;
            same_project(&t)?;
            let max = args["max_chars"].as_u64().unwrap_or(60_000) as usize;
            let d = runner::diff_of(core, &t).await?;
            Ok(if d.trim().is_empty() { "No changes yet.".into() } else { clip(&d, max) })
        }
        "merge_result" => {
            let t = core.thread(&a("thread_id"))?;
            same_project(&t)?;
            let msg = if a("message").is_empty() { format!("{} ({})", t["title"].as_str().unwrap_or(""), t["agent"].as_str().unwrap_or("")) } else { a("message") };
            let sha = runner::merge(core, &t, &msg, args["squash"].as_bool().unwrap_or(false)).await?;
            Ok(format!("Merged into the base branch at {sha}. Run the build and tests to confirm."))
        }
        "request_review" => {
            let t = core.thread(&a("thread_id"))?;
            same_project(&t)?;
            let r = runner::request_review(core, &t, &a("agent"), &a("focus"), &tid).await?;
            Ok(format!("Review thread {} opened with {}.", r["id"].as_str().unwrap_or(""), r["agent"].as_str().unwrap_or("")))
        }
        "ask_user" => {
            let opts = args["options"].as_array().cloned().unwrap_or_default();
            if opts.len() < 2 || opts.len() > 4 {
                bail!("Give 2 to 4 options.");
            }
            let id = util::id("dc");
            let now = util::now_ms();
            let rec = args["recommended"].as_i64().unwrap_or(0).clamp(0, opts.len() as i64 - 1);
            let opts_s = Value::Array(opts).to_string();
            let q = a("question");
            core.db.exec(
                "INSERT INTO decisions(id, thread_id, project_id, question, options, recommended, created_at) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7)",
                &[&id, &tid, &project, &q, &opts_s, &rec, &now],
            )?;
            let d = core.db.one("SELECT * FROM decisions WHERE id = ?1", &[&id])?.unwrap_or_default();
            runner::post(core, &tid, "system", "Decision", &q, json!({ "decision": d }), false)?;
            core.bus.emit("decision", project.as_deref(), Some(&tid), d);
            Ok("The question is in front of the person. Their answer arrives as a wake; keep going on the recommended option meanwhile.".into())
        }
        "read_memory" => crate::memory::read(core, project.as_deref(), &a("name"), &a("query")),
        "write_memory" => {
            let scope_project = if a("scope") == "global" { None } else { project.clone() };
            crate::memory::write(core, scope_project.as_deref(), &a("name"), &a("description"), &a("body"), &format!("{} ({})", caller["agent"].as_str().unwrap_or(""), caller["title"].as_str().unwrap_or("")))?;
            Ok(format!("Saved memory '{}'.", a("name")))
        }
        "delete_memory" => {
            let scope_project = if a("scope") == "global" { None } else { project.clone() };
            crate::memory::delete_by_name(core, scope_project.as_deref(), &a("name"))?;
            Ok("Deleted.".into())
        }
        "publish_artifact" => {
            let ws = runner::workspace_of(core, caller);
            let art = crate::artifacts::publish(core, project.as_deref(), Some(&tid), &a("title"), &a("description"), &a("kind"), args["path"].as_str(), args["content"].as_str(), ws.as_deref(), args["folder"].as_str())?;
            runner::post(core, &tid, "system", "Artifact", &format!("Published **{}**", art["title"].as_str().unwrap_or("")), json!({ "artifact": art }), false)?;
            let path = art["folder_id"].as_str().map(|f| crate::artifacts::folder_path(core, f)).unwrap_or_default();
            Ok(format!("Published to {path}. The person can open it from Artifacts (id {}).", art["id"].as_str().unwrap_or("")))
        }
        "list_artifacts" => {
            let root = if project.is_some() { crate::artifacts::root_folder(core, project.as_deref())? } else { crate::artifacts::auto_folder(core, None, Some(&tid))? };
            let mut out = String::new();
            for f in crate::artifacts::subtree(core, &root) {
                if core.db.one("SELECT id FROM folders WHERE id = ?1 AND deleted_at IS NULL", &[&f])?.is_none() {
                    continue;
                }
                let items = core.db.all("SELECT id, title, kind FROM artifacts WHERE folder_id = ?1 AND deleted_at IS NULL ORDER BY lower(title)", &[&f])?;
                out.push_str(&format!("{}/\n", crate::artifacts::folder_path(core, &f)));
                for a in items {
                    out.push_str(&format!("  - {} ({}, {})\n", a["title"].as_str().unwrap_or(""), a["kind"].as_str().unwrap_or(""), a["id"].as_str().unwrap_or("")));
                }
            }
            Ok(if out.is_empty() { "No folders yet.".into() } else { out })
        }
        "move_artifact" => {
            let key = a("artifact");
            let art = core
                .db
                .one("SELECT id, title FROM artifacts WHERE (id = ?1 OR lower(title) = lower(?1)) AND IFNULL(project_id, '') = ?2 AND deleted_at IS NULL ORDER BY id = ?1 DESC LIMIT 1", &[&key, &project.clone().unwrap_or_default()])?
                .ok_or_else(|| anyhow!("No artifact called {key} here. list_artifacts shows them."))?;
            let f = crate::artifacts::folder_for_path(core, project.as_deref(), Some(&tid), &a("folder"))?;
            core.db.exec("UPDATE artifacts SET folder_id = ?1 WHERE id = ?2", &[&f, &art["id"].as_str().unwrap_or("")])?;
            core.bus.emit("artifact", project.as_deref(), None, json!({ "folder": f }));
            Ok(format!("Moved \"{}\" to {}.", art["title"].as_str().unwrap_or(""), crate::artifacts::folder_path(core, &f)))
        }
        "post_update" => {
            core.set_thread(&tid, &json!({ "status_note": clip(&a("text"), 200) }))?;
            core.bus.emit("progress", project.as_deref(), Some(&tid), json!({ "text": a("text") }));
            Ok("Shown.".into())
        }
        "message_lead" => {
            let p = need_project()?;
            let lead = core.lead_thread(&p)?;
            let text = a("text");
            if text.trim().is_empty() {
                bail!("Say what you want the lead to know or do.");
            }
            let kind = if a("kind").is_empty() { "report".to_string() } else { a("kind") };
            let body = runner::wake_tag("worker-message", &caller, &format!("This thread wrote to you ({kind}):\n{}", clip(&text, 4000)));
            runner::post(core, lead["id"].as_str().unwrap_or(""), "wake", "Model Roamer", &body, json!({ "from_thread": tid }), true)?;
            Ok("Sent to the lead. Carry on with what you can; its answer arrives as a new message here.".into())
        }
        "set_title" => {
            let target = if a("thread_id").is_empty() || a("thread_id") == tid {
                caller.clone()
            } else {
                if role != "lead" {
                    bail!("You can only name your own thread.");
                }
                let t = core.thread(&a("thread_id"))?;
                same_project(&t)?;
                t
            };
            if target["kind"] == "lead" {
                bail!("The project chat is named after the project. Name the threads instead.");
            }
            if target["title_by"] == "person" {
                bail!("The person named this thread \"{}\" themselves. Keep their title.", target["title"].as_str().unwrap_or(""));
            }
            let title = clean_title(&a("title"));
            if title.is_empty() {
                bail!("Give a title of a few words.");
            }
            core.set_thread(target["id"].as_str().unwrap_or(""), &json!({ "title": title, "title_by": "agent" }))?;
            Ok(format!("Named \"{title}\"."))
        }
        "set_status" => {
            let st = a("status");
            if !["working", "review", "done", "blocked"].contains(&st.as_str()) {
                bail!("status must be working, review, done or blocked");
            }
            core.db.exec("UPDATE threads SET status_note = ?1 WHERE id = ?2", &[&a("note"), &tid])?;
            // the runner applies it when the turn ends, so the turn itself doesn't look finished early
            runner::set_final_status(core, &tid, &st, &a("note"));
            Ok(format!("Status will be '{st}' when this turn ends."))
        }
        "claim_paths" => {
            let p = need_project()?;
            let proposed = crate::collab::presence::Work { paths: proposed_claims(args), ..Default::default() };
            if !crate::collab::presence::preflight(core, caller, proposed).is_empty() {
                bail!("These paths overlap another collaborator's work. No paths were claimed. The person's thread shows the overlap and offers to send a note.");
            }
            let paths: Vec<String> = args["paths"].as_array().map(|d| d.iter().filter_map(|x| x.as_str().map(|s| s.trim().trim_start_matches("./").to_string())).collect()).unwrap_or_default();
            let mut conflicts = vec![];
            let others = core.db.all("SELECT c.path, c.thread_id, t.title, t.agent FROM claims c JOIN threads t ON t.id = c.thread_id WHERE c.project_id = ?1 AND c.thread_id != ?2 AND t.resolved = 0 AND t.status NOT IN ('done','cancelled','failed')", &[&p, &tid])?;
            for path in &paths {
                for o in &others {
                    let op = o["path"].as_str().unwrap_or("");
                    if overlaps(path, op) {
                        conflicts.push(format!("{path} overlaps {op} (thread {} \"{}\", {})", o["thread_id"].as_str().unwrap_or(""), o["title"].as_str().unwrap_or(""), o["agent"].as_str().unwrap_or("")));
                    }
                }
                let now = util::now_ms();
                let cid = util::id("cl");
                core.db.exec("INSERT INTO claims(id, project_id, thread_id, path, note, created_at) VALUES (?1, ?2, ?3, ?4, ?5, ?6)", &[&cid, &p, &tid, path, &a("note"), &now])?;
            }
            core.bus.emit("claims", Some(&p), Some(&tid), json!({}));
            if conflicts.is_empty() {
                Ok(format!("Claimed {} path(s). No conflicts.", paths.len()))
            } else {
                Ok(format!("Claimed, but these overlap other threads' claims. Don't edit them; tell the lead:\n{}", conflicts.join("\n")))
            }
        }
        "release_paths" => {
            let paths: Vec<String> = args["paths"].as_array().map(|d| d.iter().filter_map(|x| x.as_str().map(str::to_string)).collect()).unwrap_or_default();
            if paths.is_empty() {
                core.db.exec("DELETE FROM claims WHERE thread_id = ?1", &[&tid])?;
            } else {
                for p in paths {
                    core.db.exec("DELETE FROM claims WHERE thread_id = ?1 AND path = ?2", &[&tid, &p])?;
                }
            }
            Ok("Released.".into())
        }
        "list_claims" => {
            let p = need_project()?;
            let rows = core.db.all("SELECT c.path, c.note, t.id, t.title, t.agent FROM claims c JOIN threads t ON t.id = c.thread_id WHERE c.project_id = ?1 AND t.resolved = 0 ORDER BY c.created_at", &[&p])?;
            if rows.is_empty() {
                return Ok("No claims.".into());
            }
            Ok(rows.iter().map(|r| format!("{} — {} \"{}\" ({}) {}", r["path"].as_str().unwrap_or(""), r["id"].as_str().unwrap_or(""), r["title"].as_str().unwrap_or(""), r["agent"].as_str().unwrap_or(""), r["note"].as_str().unwrap_or(""))).collect::<Vec<_>>().join("\n"))
        }
        "git_status" => {
            let ws = runner::workspace_of(core, caller).ok_or_else(|| anyhow!("No workspace yet."))?;
            if !git::is_repo(&ws).await {
                return Ok(format!("{} is not a git repository.", ws.display()));
            }
            Ok(format!("{}\n\nRecent commits:\n{}", git::status(&ws).await, git::log(&ws, 8).await.iter().map(|l| l.replace('\x1f', " · ")).collect::<Vec<_>>().join("\n")))
        }
        "commit_work" => {
            let ws = runner::workspace_of(core, caller).ok_or_else(|| anyhow!("No workspace yet."))?;
            let s = core.settings();
            git::ensure_identity(&ws, s["git_name"].as_str().unwrap_or(""), s["git_email"].as_str().unwrap_or("")).await;
            Ok(match git::commit_all(&ws, &a("message")).await? {
                Some(sha) => format!("Committed {sha}."),
                None => "Nothing to commit.".into(),
            })
        }
        "open_pull_request" => {
            let target = if !a("thread_id").is_empty() && role == "lead" {
                let t = core.thread(&a("thread_id"))?;
                same_project(&t)?;
                t
            } else {
                caller.clone()
            };
            let r = runner::open_pr(core, &target, &a("title"), &a("body"), args["draft"].as_bool().unwrap_or(false)).await?;
            Ok(format!("Pull request #{}: {}", r["number"], r["url"].as_str().unwrap_or("")))
        }
        "list_pull_requests" => {
            let (o, r) = repo_of(core, &caller).await?;
            let st = if a("state").is_empty() { "open".to_string() } else { a("state") };
            Ok(github::list_prs(core, &o, &r, &st).await?.to_string())
        }
        "pull_request_checks" => {
            let (o, r) = repo_of(core, &caller).await?;
            Ok(github::checks(core, &o, &r, args["number"].as_i64().unwrap_or(0)).await?.to_string())
        }
        "comment_on_pull_request" => {
            let (o, r) = repo_of(core, &caller).await?;
            let body = format!("{}\n\n— {} via Model Roamer", a("body"), caller["agent"].as_str().unwrap_or("agent"));
            github::comment(core, &o, &r, args["number"].as_i64().unwrap_or(0), &body).await?;
            Ok("Commented.".into())
        }
        _ => bail!("Unknown tool {name}"),
    }
}

fn proposed_claims(args: &Value) -> Vec<String> {
    args["paths"].as_array().into_iter().flatten().filter_map(Value::as_str).filter_map(crate::collab::presence::normal_path).collect()
}

fn thread_line(t: &Value) -> String {
    format!(
        "{} · \"{}\" · {} · {}{}{}{}{}",
        t["id"].as_str().unwrap_or(""),
        t["title"].as_str().unwrap_or(""),
        t["agent"].as_str().unwrap_or(""),
        t["status"].as_str().unwrap_or(""),
        t["status_note"].as_str().filter(|s| !s.is_empty()).map(|s| format!(" ({s})")).unwrap_or_default(),
        t["branch"].as_str().filter(|s| !s.is_empty()).map(|s| format!(" · branch {s}")).unwrap_or_default()
            + &t["repo_id"].as_str().filter(|s| !s.is_empty()).map(|s| format!(" · repo {s}")).unwrap_or_default(),
        if t["merged"] == 1 { " · merged" } else if t["resolved"] == 1 { " · resolved" } else if t["parked_at"].as_i64().is_some() { " · parked (kept idle)" } else { "" },
        match (t["context"]["used"].as_i64(), t["context"]["window"].as_i64()) {
            (Some(u), Some(w)) if w > 0 => format!(" · context {}%{}", u * 100 / w, if u * 100 / w >= 80 { " (near compacting)" } else { "" }),
            _ => String::new(),
        }
    )
}

/// "src/ui" and "src/ui/app.js" overlap; "src/ui" and "src/core" don't.
fn overlaps(a: &str, b: &str) -> bool {
    let n = |s: &str| s.trim().trim_start_matches("./").trim_matches('/').replace('\\', "/").trim_end_matches("/**").trim_end_matches("/*").to_lowercase();
    let (a, b) = (n(a), n(b));
    a.is_empty() || b.is_empty() || a == b || a.starts_with(&format!("{b}/")) || b.starts_with(&format!("{a}/"))
}

/// The GitHub repository a thread works on (one of its project's).
async fn repo_of(core: &CoreRef, caller: &Value) -> Result<(String, String)> {
    caller["project_id"].as_str().ok_or_else(|| anyhow!("This thread isn't part of a project."))?;
    let p = crate::repos::for_thread(core, caller)?;
    let mut url = p["repo_url"].as_str().unwrap_or("").to_string();
    if url.is_empty() {
        url = git::remote_url(&PathBuf::from(core.project_home(&p))).await;
    }
    git::github_repo(&url).ok_or_else(|| anyhow!("This project isn't linked to a GitHub repository."))
}

// ------------------------------------------------------------------------------------------------ text protocol
/// For agents without MCP: `<roamer-tool name="spawn_thread">{"title": "..."}</roamer-tool>` blocks in a reply.
pub fn text_calls(reply: &str) -> (String, Vec<(String, Value)>) {
    // Be forgiving: single or double quotes, spaces around `=`, a `roamer:` / `mcp__roamer__` prefix on the name,
    // and a self-closing block with no arguments.
    let re = regex::Regex::new(r#"(?s)<roamer-tool\s+name\s*=\s*["']([A-Za-z0-9_:.\-]+)["']\s*(?:/>|>(.*?)</roamer-tool\s*>)"#).unwrap();
    let mut calls = vec![];
    for c in re.captures_iter(reply) {
        let name = c[1].rsplit(|ch| ch == ':' || ch == '.').next().unwrap_or("").trim_start_matches("mcp__roamer__").trim_start_matches("roamer__").to_lowercase().replace('-', "_");
        let body = c.get(2).map(|m| m.as_str()).unwrap_or("").trim().trim_start_matches("```json").trim_start_matches("```").trim_end_matches("```").trim();
        let args = if body.is_empty() {
            json!({})
        } else {
            serde_json::from_str(body)
                .or_else(|_| serde_json::from_str(&strip_trailing_commas(body)))
                .unwrap_or_else(|_| json!({ "_raw": body }))
        };
        calls.push((name, args));
    }
    (re.replace_all(reply, "").trim().to_string(), calls)
}

/// `{"a": 1,}` and `[1, 2,]` are common slips in hand-written JSON.
fn strip_trailing_commas(s: &str) -> String {
    regex::Regex::new(r",\s*([}\]])").unwrap().replace_all(s, "$1").into_owned()
}

pub const TEXT_PROTOCOL: &str = r#"## Calling Model Roamer tools without native tool calls

If the Model Roamer tools are not available to you as tool calls, call them by writing blocks like this in your
reply (JSON arguments, one block per call):

<roamer-tool name="read_memory">{}</roamer-tool>
<roamer-tool name="spawn_thread">{"title": "Build the settings page", "instructions": "...", "agent": "antigravity"}</roamer-tool>

Model Roamer runs them when your turn ends (not while you are still writing) and sends the results back to you in a
new turn. Text outside the blocks is your reply. Rules that keep calls from failing: the tag is exactly
`<roamer-tool name="tool_name">`, the body is one valid JSON object (double quotes, no comments), and the name is
the bare tool name from the list above. If a call's arguments can't be read you get an error back and can try
again. Only use this when the native tools are missing."#;

/// A title as the person will see it in a list: one line, no wrapping quotes or trailing period, at most 60 characters.
fn clean_title(raw: &str) -> String {
    let line = raw.lines().map(str::trim).find(|l| !l.is_empty()).unwrap_or("");
    let t = line.trim_matches(|c: char| matches!(c, '"' | '\'' | '`' | '*' | '#' | '\u{201c}' | '\u{201d}') || c.is_whitespace());
    let t = t.trim_end_matches('.').trim();
    if t.chars().count() <= 60 {
        return t.to_string();
    }
    // cut at a word boundary rather than mid-word
    let cut: String = t.chars().take(60).collect();
    cut.rsplit_once(' ').map(|(a, _)| a.trim_end_matches([',', ':', ';', '-']).trim().to_string()).unwrap_or(cut)
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn parses_text_calls() {
        let (rest, calls) = text_calls("Hi\n<roamer-tool name=\"read_memory\">{\"name\":\"plan\"}</roamer-tool>\nbye");
        assert_eq!(rest, "Hi\n\nbye");
        assert_eq!(calls[0].0, "read_memory");
        assert_eq!(calls[0].1["name"], "plan");
    }
    #[test]
    fn text_calls_are_forgiving() {
        let (rest, calls) = text_calls("<roamer-tool name='mcp__roamer__message_lead'>{\"text\": \"hi\",}</roamer-tool> <roamer-tool name = \"roamer:read_memory\" />");
        assert!(rest.trim().is_empty());
        assert_eq!(calls[0].0, "message_lead");
        assert_eq!(calls[0].1["text"], "hi");
        assert_eq!(calls[1].0, "read_memory");
    }
    #[test]
    fn overlap_rules() {
        assert!(overlaps("src/ui", "src/ui/app.js"));
        assert!(overlaps("./src/ui/", "src/ui"));
        assert!(!overlaps("src/ui", "src/core"));
        assert!(!overlaps("src/ui2", "src/ui"));
    }
    #[test]
    fn cleans_titles() {
        assert_eq!(clean_title("  \"Fix Safari export button.\"\nextra"), "Fix Safari export button");
        assert_eq!(clean_title("**Add dark mode**"), "Add dark mode");
        let long = clean_title(&"word ".repeat(30));
        assert!(long.chars().count() <= 60 && !long.ends_with(' '));
        assert!(list("chat").iter().any(|t| t["name"] == "set_title"));
    }
    #[test]
    fn lists_by_role() {
        assert!(list("lead").iter().any(|t| t["name"] == "spawn_thread"));
        assert!(!list("worker").iter().any(|t| t["name"] == "spawn_thread"));
        assert!(list("chat").iter().any(|t| t["name"] == "write_memory"));
    }
}
