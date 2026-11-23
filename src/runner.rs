//! Running agents: threads, turns, wakes and routing. This is the orchestration engine.
//!
//! A message to a thread is stored as *pending*; the thread then runs a turn with its agent, giving it every pending
//! message at once. When a worker's turn ends, its reply wakes the project's lead, the way a colleague's message
//! would. Turns are queued per agent (each has a limit on how many run at once), can be paused, cancelled, retried
//! and moved to another agent, and survive a crash: turns that were running when the app stopped run again on start.

use crate::{
    agents::{AgentEvent, Autonomy, McpLaunch, TurnSpec},
    core::{CoreRef, Running},
    git, github, procs, prompts, tools,
    util::{self, clip},
};
use anyhow::{anyhow, bail, Result};
use serde_json::{json, Value};
use std::{
    collections::HashMap,
    path::PathBuf,
    sync::{Mutex, OnceLock},
    time::Duration,
};
use tokio::sync::watch;

fn final_status() -> &'static Mutex<HashMap<String, (String, String)>> {
    static M: OnceLock<Mutex<HashMap<String, (String, String)>>> = OnceLock::new();
    M.get_or_init(|| Mutex::new(HashMap::new()))
}

/// A tool asked for this status; it's applied when the turn ends.
pub fn set_final_status(_core: &CoreRef, thread: &str, status: &str, note: &str) {
    if let Ok(mut m) = final_status().lock() {
        m.insert(thread.to_string(), (status.to_string(), note.to_string()));
    }
}

// ------------------------------------------------------------------------------------------------ messages

/// Add a message to a thread. `wake`: the thread's agent should read it (it becomes pending and a turn is scheduled).
pub fn post(core: &CoreRef, thread: &str, role: &str, author: &str, body: &str, meta: Value, wake: bool) -> Result<Value> {
    let t = core.thread(thread)?;
    let id = util::id("msg");
    let now = util::now_ms();
    let pending = wake as i64;
    let meta_s = meta.to_string();
    core.db.exec(
        "INSERT INTO messages(id, thread_id, role, author, body, meta, pending, created_at) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8)",
        &[&id, &thread, &role, &author, &body, &meta_s, &pending, &now],
    )?;
    core.db.exec("UPDATE threads SET updated_at = ?1 WHERE id = ?2", &[&now, &thread])?;
    let m = core.db.one("SELECT * FROM messages WHERE id = ?1", &[&id])?.unwrap_or_default();
    core.bus.emit("message", t["project_id"].as_str(), Some(thread), m.clone());
    if wake {
        schedule(core, thread);
    }
    Ok(m)
}

fn note(core: &CoreRef, thread: &str, text: &str) {
    let _ = post(core, thread, "system", "Model Roamer", text, json!({}), false);
}

/// A failure that passes on its own: the CLI's sign-in token was being refreshed by another copy of it (Claude Code
/// running elsewhere on this computer), or the service was briefly overloaded. Worth retrying after a pause.
pub fn transient_error(e: &str) -> bool {
    let l = e.to_lowercase();
    (l.contains("refresh") && l.contains("oauth")) || l.contains("another claude code process") || l.contains("overloaded_error") || l.contains("\"overloaded\"")
}

/// Automatic retries of a transient failure before the turn is reported as failed, and the pause before each.
const TRANSIENT_RETRIES: [u64; 3] = [8, 25, 60];

/// Tell the project's lead something happened (a worker replied, a limit was hit, a decision was answered).
pub fn wake_lead(core: &CoreRef, project: &str, body: &str, meta: Value) {
    match core.lead_thread(project) {
        Ok(l) => {
            let _ = post(core, l["id"].as_str().unwrap_or(""), "wake", "Model Roamer", body, meta, true);
        }
        Err(e) => tracing::warn!("no lead thread for {project}: {e}"),
    }
}

fn wake_tag(reason: &str, t: &Value, body: &str) -> String {
    format!(
        "<wake reason=\"{reason}\" thread=\"{}\" title=\"{}\" agent=\"{}\" status=\"{}\" at=\"{}\">\n{}\n</wake>",
        t["id"].as_str().unwrap_or(""),
        t["title"].as_str().unwrap_or("").replace('"', "'"),
        t["agent"].as_str().unwrap_or(""),
        t["status"].as_str().unwrap_or(""),
        util::now_iso(),
        body.trim()
    )
}

// ------------------------------------------------------------------------------------------------ threads

pub struct NewWorker {
    pub title: String,
    pub instructions: String,
    pub criteria: String,
    pub agent: String,
    pub task_kind: String,
    pub model: String,
    pub workspace: String,
    pub depends_on: Vec<String>,
    pub parent: Option<String>,
    pub kind: String,
}

pub async fn create_worker(core: &CoreRef, project: &str, w: NewWorker) -> Result<Value> {
    let p = core.project(project)?;
    if w.title.trim().is_empty() || w.instructions.trim().is_empty() {
        bail!("A thread needs a title and instructions.");
    }
    let kind = if w.task_kind.is_empty() { "general".to_string() } else { w.task_kind.to_lowercase() };
    let agent = if w.agent.is_empty() || w.agent == "auto" {
        route(core, Some(project), &kind).map(|(a, _)| a).ok_or_else(|| anyhow!("No agent is installed, signed in and within its limits. Open Agents to set one up."))?
    } else {
        let a = w.agent.to_lowercase();
        if core.adapter(&a).is_none() {
            bail!("There's no agent called '{a}'. Use claude, codex, gemini, antigravity or a custom agent's id.");
        }
        a
    };
    let model = if w.model.is_empty() { core.agent_cfg(&agent)["model"].as_str().unwrap_or("").to_string() } else { w.model.clone() };
    let mode = match w.workspace.as_str() {
        "shared" | "scratch" | "worktree" => w.workspace.clone(),
        m if m.starts_with("path:") => m.to_string(),
        _ => "worktree".to_string(),
    };
    let id = util::id("th");
    let now = util::now_ms();
    let deps = w.depends_on.join(",");
    let status = if deps_met(core, &w.depends_on) { "idle" } else { "waiting" };
    let ws = if mode.starts_with("path:") { mode } else { format!("mode:{mode}") };
    let pid_s = p["id"].as_str().unwrap_or("").to_string();
    core.db.exec(
        "INSERT INTO threads(id, project_id, parent_id, kind, title, agent, model, status, task_kind, instructions, criteria, workspace, depends_on, created_at, updated_at)
         VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13, ?14, ?14)",
        &[&id, &pid_s, &w.parent, &w.kind, &w.title.trim(), &agent, &model, &status, &kind, &w.instructions, &w.criteria, &ws, &deps, &now],
    )?;
    let t = core.thread(&id)?;
    core.bus.emit("thread", Some(&pid_s), Some(&id), t.clone());
    let first = if w.kind == "review" { "Start the review." } else { "Start on your task (it's in your instructions under \"The task\")." };
    let pending = status == "idle";
    post(core, &id, "user", "Lead", first, json!({ "kickoff": true }), false)?;
    core.db.exec("UPDATE messages SET pending = 1 WHERE thread_id = ?1", &[&id])?;
    if pending {
        schedule(core, &id);
    }
    core.thread(&id)
}

pub fn create_chat(core: &CoreRef, agent: &str, model: &str, title: &str, folder: &str) -> Result<Value> {
    if core.adapter(agent).is_none() {
        bail!("Unknown agent {agent}");
    }
    let id = util::id("th");
    let now = util::now_ms();
    let title = if title.trim().is_empty() { "New chat" } else { title.trim() };
    let ws = if folder.trim().is_empty() { String::new() } else { format!("path:{}", folder.trim()) };
    core.db.exec(
        "INSERT INTO threads(id, project_id, kind, title, agent, model, status, workspace, created_at, updated_at) VALUES (?1, NULL, 'chat', ?2, ?3, ?4, 'idle', ?5, ?6, ?6)",
        &[&id, &title, &agent, &model, &ws, &now],
    )?;
    let t = core.thread(&id)?;
    core.bus.emit("thread", None, Some(&id), t.clone());
    Ok(t)
}

fn deps_met(core: &CoreRef, deps: &[String]) -> bool {
    deps.iter().filter(|d| !d.trim().is_empty()).all(|d| {
        core.thread(d.trim()).map(|t| matches!(t["status"].as_str(), Some("review" | "done")) || t["resolved"] == 1 || t["merged"] == 1).unwrap_or(true)
    })
}

/// Start threads whose dependencies just finished.
fn release_dependents(core: &CoreRef, project: &str) {
    let waiting = core.db.all("SELECT id, depends_on FROM threads WHERE project_id = ?1 AND status = 'waiting' AND deleted_at IS NULL", &[&project]).unwrap_or_default();
    for w in waiting {
        let deps: Vec<String> = w["depends_on"].as_str().unwrap_or("").split(',').map(str::to_string).collect();
        if deps_met(core, &deps) {
            let id = w["id"].as_str().unwrap_or("");
            let _ = core.set_status(id, "idle", "");
            schedule(core, id);
        }
    }
}

/// Which agent should take a kind of task now: the person's routing rule if that agent is usable, else the first
/// usable one in the fallback order.
pub fn route(core: &CoreRef, project: Option<&str>, kind: &str) -> Option<(String, String)> {
    let s = core.settings();
    let mut routing = s["routing"].clone();
    if let Some(p) = project.and_then(|p| core.project(p).ok()) {
        crate::config::merge(&mut routing, &p["routing"]);
    }
    let want = routing[kind].as_str().or(routing["general"].as_str()).unwrap_or("claude").to_string();
    if usable(core, &want) {
        return Some((want, format!("routing rule for {kind}")));
    }
    let mut order: Vec<String> = s["lead_fallback"].as_array().map(|a| a.iter().filter_map(|x| x.as_str().map(str::to_string)).collect()).unwrap_or_default();
    order.extend(core.adapters().iter().map(|a| a.id()));
    for a in order {
        if a != want && usable(core, &a) {
            return Some((a.clone(), format!("{want} isn't available right now ({}), so {a}", why_not(core, &want))));
        }
    }
    None
}

pub fn usable(core: &CoreRef, agent: &str) -> bool {
    if core.adapter(agent).is_none() || core.agent_cfg(agent)["enabled"] == false || core.is_limited(agent) {
        return false;
    }
    let st = core.status_cache.lock().ok().and_then(|c| c.get(agent).cloned());
    match st {
        Some(st) => st["installed"] != false && st["auth"]["signed_in"] != "no",
        None => true, // not checked yet: give it a go
    }
}

fn why_not(core: &CoreRef, agent: &str) -> String {
    if core.adapter(agent).is_none() {
        return "unknown agent".into();
    }
    if core.agent_cfg(agent)["enabled"] == false {
        return "turned off".into();
    }
    if core.is_limited(agent) {
        return "out of usage".into();
    }
    let st = core.status_cache.lock().ok().and_then(|c| c.get(agent).cloned()).unwrap_or_default();
    if st["installed"] == false {
        return "not installed".into();
    }
    "not signed in".into()
}

// ------------------------------------------------------------------------------------------------ scheduling

/// Make sure a turn runs for this thread if it has anything waiting. Safe to call any time, as often as you like.
pub fn schedule(core: &CoreRef, thread: &str) {
    let (tx, rx) = watch::channel(false);
    {
        let mut r = core.running.lock().unwrap_or_else(|p| p.into_inner());
        if r.contains_key(thread) {
            return; // the running loop picks up new messages when its turn ends
        }
        r.insert(thread.to_string(), Running { turn_id: String::new(), cancel: tx, started: util::now_ms() });
    }
    let core = core.clone();
    let thread = thread.to_string();
    tokio::spawn(async move {
        let mut rx = rx;
        loop {
            match runnable(&core, &thread) {
                Ok(true) => {}
                _ => break,
            }
            if *rx.borrow() {
                break;
            }
            if let Err(e) = run_turn(&core, &thread, &mut rx).await {
                tracing::error!(thread = %thread, "turn failed: {e:#}");
                note(&core, &thread, &format!("This turn couldn't run: {e}"));
                let _ = core.set_status(&thread, "failed", &clip(&e.to_string(), 200));
                break;
            }
            if *rx.borrow() {
                break;
            }
        }
        if let Ok(mut r) = core.running.lock() {
            r.remove(&thread);
        }
        core.bus.emit("running", None, Some(&thread), json!({ "running": false }));
        // something may have arrived between the last check and removing ourselves
        if runnable(&core, &thread).unwrap_or(false) {
            schedule(&core, &thread);
        }
    });
}

fn runnable(core: &CoreRef, thread: &str) -> Result<bool> {
    let t = core.thread(thread)?;
    if matches!(t["status"].as_str(), Some("paused" | "cancelled" | "limited" | "waiting")) {
        return Ok(false);
    }
    if t["project_id"].is_string() {
        if let Ok(p) = core.project(t["project_id"].as_str().unwrap()) {
            if p["deleted_at"].is_i64() {
                return Ok(false);
            }
        }
    }
    Ok(core.db.scalar_i64("SELECT COUNT(*) FROM messages WHERE thread_id = ?1 AND pending = 1", &[&thread])? > 0)
}

// ------------------------------------------------------------------------------------------------ workspaces

/// The folder a thread works in, once it has one.
pub fn workspace_of(core: &CoreRef, t: &Value) -> Option<PathBuf> {
    let ws = t["workspace"].as_str().unwrap_or("");
    if let Some(p) = ws.strip_prefix("path:") {
        return Some(PathBuf::from(p));
    }
    if !ws.is_empty() && !ws.starts_with("mode:") {
        return Some(PathBuf::from(ws));
    }
    match t["kind"].as_str() {
        Some("lead") => t["project_id"].as_str().and_then(|p| core.project(p).ok()).map(|p| core.project_home(&p)),
        Some("chat") => Some(core.data.join("chats").join(t["id"].as_str().unwrap_or("x"))),
        _ => None,
    }
}

async fn prepare_workspace(core: &CoreRef, t: &Value, project: Option<&Value>) -> Result<PathBuf> {
    let id = t["id"].as_str().unwrap_or("");
    if let Some(p) = project {
        ensure_project_folder(core, p).await?;
    }
    let project = match project {
        Some(p) => Some(core.project(p["id"].as_str().unwrap_or(""))?),
        None => None,
    };
    let ws = t["workspace"].as_str().unwrap_or("").to_string();
    if let Some(mode) = ws.strip_prefix("mode:") {
        let p = project.as_ref().ok_or_else(|| anyhow!("no project"))?;
        let home = core.project_home(p);
        let path = match mode {
            "worktree" if core.settings()["worktrees"] != false && git::is_repo(&home).await => {
                let base = match p["base_branch"].as_str().filter(|s| !s.is_empty()) {
                    Some(b) => b.to_string(),
                    None => git::current_branch(&home).await.unwrap_or_else(|_| "main".into()),
                };
                let branch = format!("roamer/{}-{}", util::slug(t["title"].as_str().unwrap_or("task"), 36), &id[id.len().saturating_sub(5)..]);
                let path = git::worktree_path(&core.data, p["id"].as_str().unwrap_or(""), id);
                git::add_worktree(&home, &path, &branch, &base).await?;
                let s = core.settings();
                git::ensure_identity(&path, s["git_name"].as_str().unwrap_or(""), s["git_email"].as_str().unwrap_or("")).await;
                core.set_thread(id, &json!({ "branch": branch, "base_branch": base }))?;
                path
            }
            "scratch" => {
                let d = core.data.join("scratch").join(id);
                std::fs::create_dir_all(&d)?;
                d
            }
            _ => home, // shared, or a project without git
        };
        core.set_thread(id, &json!({ "workspace": path.display().to_string() }))?;
        return Ok(path);
    }
    let path = workspace_of(core, t).ok_or_else(|| anyhow!("This thread has no workspace"))?;
    std::fs::create_dir_all(&path).map_err(|e| anyhow!("Can't use {}: {e}", path.display()))?;
    Ok(path)
}

/// A project linked to a repository but without a local folder gets cloned into the app's data folder.
pub async fn ensure_project_folder(core: &CoreRef, p: &Value) -> Result<()> {
    let folder = p["folder"].as_str().unwrap_or("");
    let repo = p["repo_url"].as_str().unwrap_or("");
    let pid = p["id"].as_str().unwrap_or("");
    if !folder.is_empty() || repo.is_empty() {
        return Ok(());
    }
    let into = core.data.join("projects").join(pid).join("repo");
    if !into.join(".git").exists() {
        core.bus.emit("progress", Some(pid), None, json!({ "text": format!("Cloning {repo}…") }));
        let tok = github::token(core).await;
        git::clone(repo, &into, tok.as_deref()).await?;
    }
    let base = git::default_branch(&into).await;
    let f = into.display().to_string();
    let now = util::now_ms();
    core.db.exec("UPDATE projects SET folder = ?1, base_branch = CASE WHEN base_branch = '' THEN ?2 ELSE base_branch END, updated_at = ?3 WHERE id = ?4", &[&f, &base, &now, &pid])?;
    core.bus.emit("project", Some(pid), None, core.project(pid)?);
    Ok(())
}

// ------------------------------------------------------------------------------------------------ one turn

fn format_inputs(msgs: &[Value]) -> String {
    let mut out = String::new();
    for m in msgs {
        let body = m["body"].as_str().unwrap_or("");
        match m["role"].as_str().unwrap_or("") {
            "wake" => out.push_str(body),
            _ => out.push_str(&format!(
                "<message from=\"{}\" at=\"{}\">\n{}\n</message>",
                m["author"].as_str().unwrap_or("someone").replace('"', "'"),
                chrono::DateTime::from_timestamp_millis(m["created_at"].as_i64().unwrap_or(0)).map(|d| d.with_timezone(&chrono::Local).format("%Y-%m-%d %H:%M").to_string()).unwrap_or_default(),
                body
            )),
        }
        if let Some(files) = m["meta"]["files"].as_array() {
            for f in files {
                out.push_str(&format!("\n(Attached file: {})", f.as_str().unwrap_or("")));
            }
        }
        out.push_str("\n\n");
    }
    out.trim().to_string()
}

/// The conversation so far, for an agent that starts fresh on an old thread (reassigned, or its session was lost).
fn recap(core: &CoreRef, thread: &str, exclude: &[String]) -> String {
    let rows = core
        .db
        .all("SELECT * FROM (SELECT id, role, author, body, created_at FROM messages WHERE thread_id = ?1 AND role != 'system' ORDER BY created_at DESC LIMIT 30) ORDER BY created_at", &[&thread])
        .unwrap_or_default();
    let rows: Vec<&Value> = rows.iter().filter(|r| !exclude.iter().any(|e| Some(e.as_str()) == r["id"].as_str())).collect();
    if rows.is_empty() {
        return String::new();
    }
    let mut s = String::from("<conversation-so-far note=\"This thread was started earlier, possibly by another agent. Here is what was said, oldest first.\">\n");
    for r in rows {
        s.push_str(&format!("[{} — {}]\n{}\n\n", r["role"].as_str().unwrap_or(""), r["author"].as_str().unwrap_or(""), clip(r["body"].as_str().unwrap_or(""), 2500)));
    }
    s.push_str("</conversation-so-far>\n\n");
    s
}

fn add_activity(core: &CoreRef, t: &Value, turn: &str, kind: &str, title: &str, body: &str) {
    let now = util::now_ms();
    let tid = t["id"].as_str().unwrap_or("");
    let body = clip(body, 20_000);
    let _ = core.db.exec("INSERT INTO activity(thread_id, turn_id, kind, title, body, created_at) VALUES (?1, ?2, ?3, ?4, ?5, ?6)", &[&tid, &turn, &kind, &title, &body, &now]);
    core.bus.emit("activity", t["project_id"].as_str(), Some(tid), json!({ "turn_id": turn, "kind": kind, "title": title, "body": clip(&body, 4000), "created_at": now }));
}

enum Outcome {
    Done,
    Cancelled,
    Limited,
    Failed,
    RetryFresh,
    /// A transient failure (see `transient_error`): try again after a pause, this many seconds.
    RetryLater(u64),
}

async fn run_turn(core: &CoreRef, thread: &str, cancel: &mut watch::Receiver<bool>) -> Result<()> {
    let mut t = core.thread(thread)?;
    let project = match t["project_id"].as_str() {
        Some(p) => Some(core.project(p)?),
        None => None,
    };
    let pid = project.as_ref().and_then(|p| p["id"].as_str()).map(str::to_string);
    let mut agent = t["agent"].as_str().unwrap_or("claude").to_string();

    // out of usage: move to another agent, or wait for the reset
    if core.is_limited(&agent) {
        let lim = core.limit_of(&agent);
        let alt = if core.settings()["auto_fallback"] != false { route(core, pid.as_deref(), t["task_kind"].as_str().filter(|s| !s.is_empty()).unwrap_or("general")).map(|r| r.0).filter(|a| a != &agent) } else { None };
        match alt {
            Some(a) => {
                note(core, thread, &format!("{agent} is out of usage ({}), so this thread moved to {a}.", lim["note"].as_str().unwrap_or("limit reached")));
                core.set_thread(thread, &json!({ "agent": a, "model": core.agent_cfg(&a)["model"], "session_id": "", "session_agent": "" }))?;
                agent = a;
                t = core.thread(thread)?;
            }
            None => {
                core.set_status(thread, "limited", &format!("Waiting for {agent}'s usage to reset"))?;
                return Ok(());
            }
        }
    }
    let adapter = core.adapter(&agent).ok_or_else(|| anyhow!("Unknown agent {agent}"))?;
    if core.agent_cfg(&agent)["enabled"] == false {
        bail!("{} is turned off in Agents. Turn it on or reassign this thread.", adapter.info().name);
    }
    let Some(program) = core.locate(&agent) else {
        bail!("{} isn't installed on {}. Open Agents to install it, or reassign this thread.", adapter.info().name, core.display_name());
    };

    // wait for a free slot for this agent
    let slot = core.slot(&agent);
    let permit = match slot.clone().try_acquire_owned() {
        Ok(p) => p,
        Err(_) => {
            core.set_status(thread, "queued", &format!("Waiting for a free {agent} slot"))?;
            tokio::select! {
                p = slot.acquire_owned() => p?,
                _ = cancel.changed() => return Ok(()),
            }
        }
    };

    // the messages this turn answers
    let inputs = core.db.all("SELECT * FROM messages WHERE thread_id = ?1 AND pending = 1 ORDER BY created_at", &[&thread])?;
    if inputs.is_empty() {
        return Ok(());
    }
    let input_ids: Vec<String> = inputs.iter().filter_map(|m| m["id"].as_str().map(str::to_string)).collect();
    for id in &input_ids {
        core.db.exec("UPDATE messages SET pending = 0 WHERE id = ?1", &[id])?;
    }

    core.set_status(thread, "working", "")?;
    let workspace = match prepare_workspace(core, &t, project.as_ref()).await {
        Ok(w) => w,
        Err(e) => {
            requeue(core, &input_ids);
            core.set_status(thread, "failed", "Workspace problem")?;
            bail!("Couldn't set up the workspace: {e}");
        }
    };
    t = core.thread(thread)?;

    let turn_id = util::id("turn");
    let scratch = core.data.join("turns").join(&turn_id);
    std::fs::create_dir_all(&scratch)?;
    let token = util::secret();
    core.turn_tokens.lock().unwrap_or_else(|p| p.into_inner()).insert(token.clone(), thread.to_string());
    let mcp = McpLaunch {
        command: core.exe.display().to_string(),
        args: vec!["mcp".into()],
        env: vec![("MR_MCP_URL".into(), format!("{}/api/mcp", core.base_url())), ("MR_MCP_TOKEN".into(), token.clone())],
    };
    let session = (t["session_agent"].as_str() == Some(agent.as_str()) && !t["session_id"].as_str().unwrap_or("").is_empty()).then(|| t["session_id"].as_str().unwrap_or("").to_string());
    let target: Option<Value> = None; // a review's instructions already name the thread it reviews
    let system = prompts::system(core, &prompts::Ctx { thread: &t, project: project.as_ref(), workspace: workspace.display().to_string(), mcp: adapter.info().mcp, target: target.as_ref() });
    let mut prompt = String::new();
    if session.is_none() {
        prompt.push_str(&recap(core, thread, &input_ids));
    }
    prompt.push_str(&format_inputs(&inputs));
    let cfg = core.agent_cfg(&agent);
    let model = t["model"].as_str().filter(|s| !s.is_empty()).map(str::to_string).unwrap_or_else(|| cfg["model"].as_str().unwrap_or("").to_string());
    let mut extra_dirs = vec![];
    if t["kind"] == "worker" || t["kind"] == "review" {
        if let Some(p) = &project {
            let home = core.project_home(p);
            if home != workspace {
                extra_dirs.push(home);
            }
        }
    }
    let spec = TurnSpec {
        prompt,
        system,
        session: session.clone(),
        cwd: workspace.clone(),
        model: model.clone(),
        effort: cfg["effort"].as_str().unwrap_or("").to_string(),
        autonomy: Autonomy::parse(cfg["autonomy"].as_str().unwrap_or("full")),
        mcp: Some(mcp.clone()),
        extra_dirs,
        scratch: scratch.clone(),
        extra_args: cfg["extra_args"].as_array().map(|a| a.iter().filter_map(|x| x.as_str().map(str::to_string)).collect()).unwrap_or_default(),
        headless: core.headless,
    };
    let mut launch = adapter.launch(&spec)?;
    launch.pty = launch.pty || adapter.needs_pty();
    let now = util::now_ms();
    let inputs_s = input_ids.join(",");
    core.db.exec(
        "INSERT INTO turns(id, thread_id, project_id, agent, model, status, inputs, started_at) VALUES (?1, ?2, ?3, ?4, ?5, 'running', ?6, ?7)",
        &[&turn_id, &thread, &pid, &agent, &model, &inputs_s, &now],
    )?;
    if let Ok(mut r) = core.running.lock() {
        if let Some(x) = r.get_mut(thread) {
            x.turn_id = turn_id.clone();
            x.started = now;
        }
    }
    core.bus.emit("turn", pid.as_deref(), Some(thread), json!({ "turn_id": turn_id, "state": "start", "agent": agent, "model": model }));
    let log_path = core.data.join("logs").join("turns").join(format!("{turn_id}.log"));
    let _ = std::fs::create_dir_all(log_path.parent().unwrap());
    let mut log = std::fs::File::create(&log_path).ok();
    let mut log_line = |s: &str| {
        use std::io::Write;
        if let Some(f) = log.as_mut() {
            let _ = writeln!(f, "{s}");
        }
    };
    log_line(&format!("$ {} {} {}", program.shown, program.prefix.join(" "), launch.args.iter().map(|a| clip(a, 200)).collect::<Vec<_>>().join(" ")));

    let mut stream = procs::stream(&program, &launch)?;
    let s = core.settings();
    let idle = Duration::from_secs(s["idle_timeout_min"].as_u64().unwrap_or(25).max(2) * 60);
    let max = Duration::from_secs(s["max_turn_min"].as_u64().unwrap_or(180).max(5) * 60);
    let deadline = tokio::time::Instant::now() + max;
    let mut st = json!({});
    let mut segments: Vec<String> = vec![];
    let mut cur = String::new();
    let mut last_tool_seg = 0usize;
    let mut final_text: Option<String> = None;
    let mut raw: Vec<String> = vec![];
    let mut errs: Vec<String> = vec![];
    let mut stderr_tail: Vec<String> = vec![];
    let mut usage = (0i64, 0i64, 0i64, 0f64);
    let mut limit: Option<(Option<i64>, String)> = None;
    let mut new_session: Option<String> = None;
    let mut exit: Option<i32> = None;
    let mut cancelled = false;
    let mut timed_out = None;
    loop {
        let line = tokio::select! {
            l = stream.rx.recv() => l,
            _ = cancel.changed() => { cancelled = true; stream.kill().await; break; }
            _ = tokio::time::sleep(idle) => { timed_out = Some(format!("no output for {} minutes", idle.as_secs() / 60)); stream.kill().await; break; }
            _ = tokio::time::sleep_until(deadline) => { timed_out = Some(format!("the turn ran longer than {} minutes", max.as_secs() / 60)); stream.kill().await; break; }
        };
        let Some(line) = line else { break };
        match line {
            procs::Line::Exit(c) => {
                exit = c;
                break;
            }
            procs::Line::Err(e) => {
                log_line(&format!("[stderr] {e}"));
                if !e.trim().is_empty() {
                    stderr_tail.push(e.clone());
                    if stderr_tail.len() > 40 {
                        stderr_tail.remove(0);
                    }
                    if let Some(until) = crate::agents::limit_in(&e).filter(|_| e.len() < 500) {
                        limit = Some((until, e.trim().to_string()));
                    }
                }
            }
            procs::Line::Out(l) => {
                log_line(&l);
                for ev in adapter.parse(&l, &mut st) {
                    match ev {
                        AgentEvent::Session(id) => new_session = Some(id),
                        AgentEvent::Delta(d) => {
                            cur.push_str(&d);
                            core.bus.emit("delta", pid.as_deref(), Some(thread), json!({ "turn_id": turn_id, "text": d }));
                        }
                        AgentEvent::Text(x) => {
                            cur.clear();
                            if !x.trim().is_empty() {
                                segments.push(x.clone());
                                core.bus.emit("delta", pid.as_deref(), Some(thread), json!({ "turn_id": turn_id, "block": x }));
                            }
                        }
                        AgentEvent::Thinking(x) => add_activity(core, &t, &turn_id, "thinking", "Thinking", &x),
                        AgentEvent::ToolUse { id: _, name, input } => {
                            if !cur.trim().is_empty() {
                                segments.push(std::mem::take(&mut cur));
                            }
                            last_tool_seg = segments.len();
                            add_activity(core, &t, &turn_id, "tool", &name, &input);
                        }
                        AgentEvent::ToolResult { id: _, output, error } => add_activity(core, &t, &turn_id, if error { "error" } else { "result" }, if error { "Tool error" } else { "Result" }, &output),
                        AgentEvent::Usage { input, output, cached, cost } => {
                            usage.0 += input;
                            usage.1 += output;
                            usage.2 += cached;
                            usage.3 += cost;
                        }
                        AgentEvent::Final(x) => final_text = Some(x),
                        AgentEvent::Limit { until, message } => limit = Some((until, message)),
                        AgentEvent::Error(e) => {
                            add_activity(core, &t, &turn_id, "error", "Error", &e);
                            errs.push(e);
                        }
                        AgentEvent::Raw(r) => raw.push(r),
                    }
                }
            }
        }
    }
    drop(permit);
    core.turn_tokens.lock().unwrap_or_else(|p| p.into_inner()).remove(&token);
    if !cur.trim().is_empty() {
        segments.push(std::mem::take(&mut cur));
    }
    // the reply: what the CLI called final, else the text after its last tool call, else all of it
    let mut reply = final_text.clone().filter(|f| !f.trim().is_empty()).unwrap_or_else(|| {
        let after: Vec<String> = segments[last_tool_seg.min(segments.len())..].to_vec();
        if after.iter().any(|s| !s.trim().is_empty()) { after.join("\n\n") } else { segments.join("\n\n") }
    });
    if reply.trim().is_empty() && adapter.raw_is_text() {
        reply = raw.join("\n").trim().to_string();
    }
    if limit.is_none() && reply.trim().is_empty() {
        for e in errs.iter().chain(stderr_tail.iter()) {
            if let Some(until) = crate::agents::limit_in(e).filter(|_| e.len() < 800) {
                limit = Some((until, e.trim().to_string()));
                break;
            }
        }
    }
    let session_failed = session.is_some()
        && reply.trim().is_empty()
        && errs.iter().chain(stderr_tail.iter()).any(|e| {
            let l = e.to_lowercase();
            (l.contains("session") || l.contains("conversation") || l.contains("thread")) && (l.contains("not found") || l.contains("no conversation") || l.contains("invalid") || l.contains("does not exist") || l.contains("unable to resume") || l.contains("could not resume"))
        });

    // a transient failure, unless the agent got real work done anyway (an error text alone doesn't count as a reply)
    let transient = !cancelled
        && limit.is_none()
        && (errs.iter().chain(stderr_tail.iter()).any(|e| transient_error(e)) || (reply.len() < 600 && transient_error(&reply)))
        && (reply.trim().is_empty() || transient_error(&reply));
    let earlier_tries = if transient {
        core.db.scalar_i64("SELECT COUNT(*) FROM turns WHERE thread_id = ?1 AND error LIKE 'Retrying:%' AND started_at > ?2", &[&thread, &(util::now_ms() - 30 * 60_000)])?
    } else {
        0
    };
    let outcome = if cancelled {
        Outcome::Cancelled
    } else if let Some((until, msg)) = &limit {
        let now = util::now_ms();
        let _ = core.db.exec(
            "INSERT INTO limits(agent, limited_until, note, hit_at) VALUES (?1, ?2, ?3, ?4) ON CONFLICT(agent) DO UPDATE SET limited_until = excluded.limited_until, note = excluded.note, hit_at = excluded.hit_at",
            &[&agent, until, &clip(msg, 300), &now],
        );
        core.bus.emit("limits", None, None, json!({ "agent": agent, "limit": core.limit_of(&agent) }));
        Outcome::Limited
    } else if transient && (earlier_tries as usize) < TRANSIENT_RETRIES.len() {
        Outcome::RetryLater(TRANSIENT_RETRIES[earlier_tries as usize])
    } else if transient {
        Outcome::Failed
    } else if session_failed {
        Outcome::RetryFresh
    } else if timed_out.is_some() || (reply.trim().is_empty() && (exit.unwrap_or(1) != 0 || !errs.is_empty())) {
        Outcome::Failed
    } else {
        Outcome::Done
    };

    let status_s = match outcome {
        Outcome::Done => "done",
        Outcome::Cancelled => "cancelled",
        Outcome::Limited => "limited",
        Outcome::Failed | Outcome::RetryFresh | Outcome::RetryLater(_) => "failed",
    };
    let ended = util::now_ms();
    let err_s = match outcome {
        Outcome::Failed if transient => errs.iter().chain(stderr_tail.iter()).find(|e| transient_error(e)).cloned().unwrap_or_else(|| reply.trim().to_string()),
        Outcome::Failed => timed_out.clone().or_else(|| errs.last().cloned()).or_else(|| stderr_tail.last().cloned()).unwrap_or_else(|| format!("exited with {exit:?}")),
        Outcome::RetryLater(_) => format!("Retrying: {}", errs.iter().chain(stderr_tail.iter()).find(|e| transient_error(e)).cloned().unwrap_or_else(|| reply.trim().to_string())),
        Outcome::Limited => limit.as_ref().map(|l| l.1.clone()).unwrap_or_default(),
        _ => String::new(),
    };
    core.db.exec(
        "UPDATE turns SET status = ?1, error = ?2, input_tokens = ?3, output_tokens = ?4, cached_tokens = ?5, cost_usd = ?6, ended_at = ?7 WHERE id = ?8",
        &[&status_s, &clip(&err_s, 2000), &usage.0, &usage.1, &usage.2, &usage.3, &ended, &turn_id],
    )?;
    if let Some(sid) = &new_session {
        core.set_thread(thread, &json!({ "session_id": sid, "session_agent": agent }))?;
    }
    core.bus.emit("turn", pid.as_deref(), Some(thread), json!({ "turn_id": turn_id, "state": "end", "status": status_s, "usage": { "input": usage.0, "output": usage.1, "cached": usage.2, "cost": usage.3 } }));
    let _ = std::fs::remove_dir_all(&scratch);

    let is_worker = matches!(t["kind"].as_str(), Some("worker" | "review"));
    match outcome {
        Outcome::Cancelled => Ok(()),
        Outcome::RetryLater(secs) => {
            let name = adapter.info().name;
            note(core, thread, &format!("{name} was busy (another copy of it was refreshing its sign-in, or the service was overloaded). Trying again in {secs} seconds."));
            requeue(core, &input_ids);
            core.set_status(thread, "idle", &format!("Retrying in {secs}s"))?;
            let (core2, th) = (core.clone(), thread.to_string());
            tokio::spawn(async move {
                tokio::time::sleep(Duration::from_secs(secs)).await;
                // only if nothing else picked the thread up in the meantime (the person retried, moved or stopped it)
                if core2.thread(&th).map(|t| t["status"] == "idle").unwrap_or(false) {
                    schedule(&core2, &th);
                }
            });
            Ok(())
        }
        Outcome::RetryFresh => {
            note(core, thread, &format!("{agent} couldn't resume its earlier session, so it starts fresh with the thread's history."));
            core.set_thread(thread, &json!({ "session_id": "", "session_agent": "" }))?;
            requeue(core, &input_ids);
            core.set_status(thread, "idle", "")?;
            Ok(())
        }
        Outcome::Limited => {
            let (until, msg) = limit.unwrap_or((None, String::new()));
            let when = until.and_then(chrono::DateTime::from_timestamp_millis).map(|d| d.with_timezone(&chrono::Local).format("%a %H:%M").to_string());
            requeue(core, &input_ids);
            let alt = if core.settings()["auto_fallback"] != false { route(core, pid.as_deref(), t["task_kind"].as_str().filter(|s| !s.is_empty()).unwrap_or("general")).map(|r| r.0).filter(|a| a != &agent) } else { None };
            let head = format!("{} hit its usage limit{}.", adapter.info().name, when.map(|w| format!(" (resets around {w})")).unwrap_or_default());
            match alt {
                Some(a) => {
                    note(core, thread, &format!("{head} Moving this thread to {a} so the work keeps going. ({})", clip(&msg, 200)));
                    core.set_thread(thread, &json!({ "agent": a, "model": core.agent_cfg(&a)["model"], "session_id": "", "session_agent": "", "status": "idle", "status_note": "" }))?;
                }
                None => {
                    note(core, thread, &format!("{head} The thread will continue on its own when the limit resets, or you can move it to another agent. ({})", clip(&msg, 200)));
                    core.set_status(thread, "limited", &head)?;
                }
            }
            if is_worker {
                if let Some(p) = &pid {
                    let t2 = core.thread(thread)?;
                    wake_lead(core, p, &wake_tag("agent-limit", &t2, &format!("{head} {}", if t2["status"] == "limited" { "It's queued until the reset; reassign it if it's urgent." } else { "It was moved automatically." })), json!({ "thread": thread }));
                }
            }
            Ok(())
        }
        Outcome::Failed => {
            let detail = clip(&err_s, 1500);
            let hint = if transient {
                format!(" It kept failing after {} tries. If Claude Code is open somewhere else on this computer, close it; otherwise sign {} in again.", TRANSIENT_RETRIES.len() + 1, adapter.info().name)
            } else if stderr_tail.iter().chain(errs.iter()).any(|e| { let l = e.to_lowercase(); l.contains("login") || l.contains("log in") || l.contains("sign in") || l.contains("unauthorized") || l.contains("authenticat") }) {
                format!(" It looks like {} needs to sign in again: open Agents and choose Sign in.", adapter.info().name)
            } else {
                String::new()
            };
            let text = format!("The turn failed: {detail}.{hint} Choose Retry to run it again, or move the thread to another agent.");
            let meta = if transient || !hint.is_empty() { json!({ "action": "signin", "agent": agent }) } else { json!({}) };
            if !hint.is_empty() {
                // the remembered "signed in" may be out of date now
                let (c2, a2) = (core.clone(), agent.clone());
                tokio::spawn(async move {
                    let _ = c2.agent_status(&a2).await;
                });
            }
            let _ = post(core, thread, "system", "Model Roamer", &text, meta, false);
            core.set_status(thread, "failed", &clip(&detail, 160))?;
            if is_worker {
                if let Some(p) = &pid {
                    let t2 = core.thread(thread)?;
                    wake_lead(core, p, &wake_tag("thread-failed", &t2, &format!("The turn failed: {detail}"))
                        , json!({ "thread": thread }));
                }
            }
            Ok(())
        }
        Outcome::Done => {
            // tools called through the text protocol run now, and their results start the next round
            let (clean, calls) = tools::text_calls(&reply);
            if !clean.trim().is_empty() {
                post(core, thread, "agent", &adapter.info().name, &clean, json!({ "agent": agent, "model": model, "turn_id": turn_id }), false)?;
                core.db.exec("UPDATE threads SET unread = 1 WHERE id = ?1", &[&thread])?;
            } else if calls.is_empty() {
                note(core, thread, &format!("{} finished without a reply.", adapter.info().name));
            }
            if !calls.is_empty() {
                let rounds = core.db.scalar_i64("SELECT COUNT(*) FROM messages WHERE thread_id = ?1 AND author = 'Tool results' AND created_at > ?2", &[&thread, &(now - 3_600_000)])?;
                if rounds >= core.settings()["max_tool_rounds"].as_i64().unwrap_or(8) {
                    note(core, thread, "Stopped running text-protocol tool calls: too many rounds in a row.");
                } else {
                    let mut out = String::from("<tool-results>\n");
                    for (name, args) in calls {
                        let res = match tools::call(core, &core.thread(thread)?, &name, &args).await {
                            Ok(r) => r,
                            Err(e) => format!("Error: {e}"),
                        };
                        add_activity(core, &t, &turn_id, "tool", &format!("roamer:{name}"), &args.to_string());
                        out.push_str(&format!("<result tool=\"{name}\">\n{}\n</result>\n", clip(&res, 30_000)));
                    }
                    out.push_str("</tool-results>");
                    post(core, thread, "wake", "Tool results", &out, json!({}), true)?;
                }
            }
            let fs = final_status().lock().ok().and_then(|mut m| m.remove(thread));
            let (st_new, note_new) = match fs {
                Some((s, n)) => (s, n),
                None => ("idle".to_string(), String::new()),
            };
            core.set_status(thread, &st_new, &note_new)?;
            if is_worker && !clean.trim().is_empty() {
                if let Some(p) = &pid {
                    let t2 = core.thread(thread)?;
                    wake_lead(core, p, &wake_tag(if t["kind"] == "review" { "review-done" } else { "thread-reply" }, &t2, &clean), json!({ "thread": thread }));
                    release_dependents(core, p);
                }
            }
            Ok(())
        }
    }
}

/// Put messages back in the queue so the next turn (or a retry) answers them.
fn requeue(core: &CoreRef, ids: &[String]) {
    for id in ids {
        let _ = core.db.exec("UPDATE messages SET pending = 1 WHERE id = ?1", &[id]);
    }
}

// ------------------------------------------------------------------------------------------------ controls

pub async fn cancel(core: &CoreRef, thread: &str, status: &str, reason: &str) {
    let _ = core.set_status(thread, status, reason);
    let tx = core.running.lock().ok().and_then(|r| r.get(thread).map(|x| x.cancel.clone()));
    if let Some(tx) = tx {
        let _ = tx.send(true);
    }
    let verb = if status == "paused" { "Paused" } else { "Stopped" };
    note(core, thread, &if reason.is_empty() { format!("{verb}.") } else { format!("{verb}: {reason}") });
}

/// Run the last turn again (after a failure, a limit or a cancel).
pub fn retry(core: &CoreRef, thread: &str) -> Result<()> {
    let pending = core.db.scalar_i64("SELECT COUNT(*) FROM messages WHERE thread_id = ?1 AND pending = 1", &[&thread])?;
    if pending == 0 {
        if let Some(last) = core.db.one("SELECT inputs FROM turns WHERE thread_id = ?1 ORDER BY started_at DESC LIMIT 1", &[&thread])? {
            let ids: Vec<String> = last["inputs"].as_str().unwrap_or("").split(',').filter(|s| !s.is_empty()).map(str::to_string).collect();
            requeue(core, &ids);
        }
        if core.db.scalar_i64("SELECT COUNT(*) FROM messages WHERE thread_id = ?1 AND pending = 1", &[&thread])? == 0 {
            post(core, thread, "user", "Model Roamer", "Continue where you left off.", json!({}), false)?;
            core.db.exec("UPDATE messages SET pending = 1 WHERE id = (SELECT id FROM messages WHERE thread_id = ?1 ORDER BY created_at DESC LIMIT 1)", &[&thread])?;
        }
    }
    core.set_status(thread, "idle", "")?;
    schedule(core, thread);
    Ok(())
}

pub async fn reassign(core: &CoreRef, thread: &str, agent: &str, model: &str, msg: &str) -> Result<()> {
    if core.adapter(agent).is_none() {
        bail!("Unknown agent {agent}");
    }
    let t = core.thread(thread)?;
    let was = t["agent"].as_str().unwrap_or("").to_string();
    if core.running.lock().map(|r| r.get(thread).map(|x| !x.turn_id.is_empty()).unwrap_or(false)).unwrap_or(false) {
        cancel(core, thread, "idle", &format!("Moving to {agent}")).await;
        tokio::time::sleep(Duration::from_millis(300)).await;
    }
    let model = if model.is_empty() { core.agent_cfg(agent)["model"].as_str().unwrap_or("").to_string() } else { model.to_string() };
    core.set_thread(thread, &json!({ "agent": agent, "model": model, "session_id": "", "session_agent": "", "status": "idle", "status_note": "" }))?;
    note(core, thread, &format!("Moved from {was} to {agent}."));
    if !msg.trim().is_empty() {
        post(core, thread, "user", "Lead", msg, json!({}), true)?;
    } else if matches!(t["status"].as_str(), Some("failed" | "limited" | "cancelled")) || core.db.scalar_i64("SELECT COUNT(*) FROM messages WHERE thread_id = ?1 AND pending = 1", &[&thread])? > 0 {
        retry(core, thread)?;
    }
    Ok(())
}

pub async fn resolve(core: &CoreRef, thread: &str, on: bool) -> Result<()> {
    let t = core.thread(thread)?;
    core.set_thread(thread, &json!({ "resolved": on as i64 }))?;
    if on {
        if matches!(t["status"].as_str(), Some("working" | "queued")) {
            cancel(core, thread, "done", "Resolved").await;
        } else if !matches!(t["status"].as_str(), Some("done" | "cancelled")) {
            core.set_status(thread, "done", "")?;
        }
        core.db.exec("DELETE FROM claims WHERE thread_id = ?1", &[&thread])?;
        // a merged worktree has nothing left to keep
        if t["merged"] == 1 {
            cleanup_worktree(core, &t).await;
        }
        if let Some(p) = t["project_id"].as_str() {
            release_dependents(core, p);
        }
    }
    Ok(())
}

pub async fn cleanup_worktree(core: &CoreRef, t: &Value) {
    let (Some(p), Some(ws)) = (t["project_id"].as_str(), workspace_of(core, t)) else { return };
    if t["branch"].as_str().unwrap_or("").is_empty() || !ws.starts_with(core.data.join("worktrees")) {
        return;
    }
    if let Ok(proj) = core.project(p) {
        let _ = git::remove_worktree(&core.project_home(&proj), &ws).await;
    }
}

pub async fn diff_of(core: &CoreRef, t: &Value) -> Result<String> {
    let ws = workspace_of(core, t).ok_or_else(|| anyhow!("This thread hasn't started working yet."))?;
    if !git::is_repo(&ws).await {
        bail!("This thread's workspace isn't a git repository, so there's no diff.");
    }
    let base = t["base_branch"].as_str().filter(|s| !s.is_empty()).map(str::to_string).unwrap_or_else(|| "HEAD".into());
    if base == "HEAD" {
        return git::run_in(&ws, &["diff", "--stat", "--patch", "HEAD"], None, 60).await.map(|o| o.stdout);
    }
    git::diff(&ws, &base).await
}

pub async fn merge(core: &CoreRef, t: &Value, message: &str, squash: bool) -> Result<String> {
    let branch = t["branch"].as_str().unwrap_or("");
    if branch.is_empty() {
        bail!("This thread worked directly in the project folder, so there's nothing to merge.");
    }
    let ws = workspace_of(core, t).ok_or_else(|| anyhow!("No workspace"))?;
    let p = core.project(t["project_id"].as_str().unwrap_or(""))?;
    if matches!(t["status"].as_str(), Some("working" | "queued")) {
        bail!("The thread is still working. Wait for it to finish (or pause it) before merging.");
    }
    git::commit_all(&ws, &format!("{} (work in progress)", t["title"].as_str().unwrap_or(""))).await?;
    let home = core.project_home(&p);
    let cur = git::current_branch(&home).await.unwrap_or_default();
    let base = t["base_branch"].as_str().unwrap_or("");
    if !base.is_empty() && cur != base {
        bail!("The project folder is on '{cur}', not '{base}'. Switch it back to '{base}' to merge.");
    }
    let sha = git::merge(&home, branch, message, squash).await?;
    let id = t["id"].as_str().unwrap_or("");
    core.set_thread(id, &json!({ "merged": 1 }))?;
    note(core, id, &format!("Merged `{branch}` into `{}` ({sha}).", if base.is_empty() { cur.as_str() } else { base }));
    core.db.exec("DELETE FROM claims WHERE thread_id = ?1", &[&id])?;
    if let Some(pid) = t["project_id"].as_str() {
        release_dependents(core, pid);
    }
    Ok(sha)
}

pub async fn open_pr(core: &CoreRef, t: &Value, title: &str, body: &str, draft: bool) -> Result<Value> {
    let branch = t["branch"].as_str().unwrap_or("").to_string();
    if branch.is_empty() {
        bail!("This thread has no branch of its own. Pull requests need a worker in a worktree.");
    }
    let ws = workspace_of(core, t).ok_or_else(|| anyhow!("No workspace"))?;
    let p = core.project(t["project_id"].as_str().unwrap_or(""))?;
    let mut url = p["repo_url"].as_str().unwrap_or("").to_string();
    if url.is_empty() {
        url = git::remote_url(&core.project_home(&p)).await;
    }
    let (owner, repo) = git::github_repo(&url).ok_or_else(|| anyhow!("The project's repository isn't on GitHub."))?;
    let title = if title.trim().is_empty() { t["title"].as_str().unwrap_or("Changes").to_string() } else { title.to_string() };
    git::commit_all(&ws, &title).await?;
    let tok = github::token(core).await;
    git::push(&ws, &branch, tok.as_deref()).await?;
    let base = t["base_branch"].as_str().filter(|s| !s.is_empty()).map(str::to_string).unwrap_or_else(|| p["base_branch"].as_str().unwrap_or("main").to_string());
    let body = format!("{body}\n\n---\n_Made by {} in Model Roamer (thread \"{}\")._", t["agent"].as_str().unwrap_or("an agent"), t["title"].as_str().unwrap_or(""));
    let pr = github::create_pr(core, &owner, &repo, &branch, &base, &title, &body, draft).await?;
    let id = t["id"].as_str().unwrap_or("");
    note(core, id, &format!("Pull request #{}: {}", pr["number"], pr["url"].as_str().unwrap_or("")));
    Ok(pr)
}

pub async fn request_review(core: &CoreRef, target: &Value, agent: &str, focus: &str, lead_thread: &str) -> Result<Value> {
    let author = target["agent"].as_str().unwrap_or("");
    let reviewer = if !agent.is_empty() {
        agent.to_string()
    } else {
        ["claude", "codex", "gemini", "antigravity"].iter().find(|a| **a != author && usable(core, a)).map(|s| s.to_string()).ok_or_else(|| anyhow!("No other agent is available to review."))?
    };
    let files = match workspace_of(core, target) {
        Some(ws) if git::is_repo(&ws).await => git::changed_files(&ws, target["base_branch"].as_str().filter(|s| !s.is_empty()).unwrap_or("HEAD")).await.join("\n"),
        _ => String::new(),
    };
    let last = core.db.one("SELECT body FROM messages WHERE thread_id = ?1 AND role = 'agent' ORDER BY created_at DESC LIMIT 1", &[&target["id"].as_str().unwrap_or("")])?.map(|m| m["body"].as_str().unwrap_or("").to_string()).unwrap_or_default();
    let instructions = format!(
        "Review the work in thread {} \"{}\" by {author}.\n\n{}### What the author was asked to do\n\n{}\n\n### Acceptance criteria\n\n{}\n\n### The author's report\n\n{}\n\n### Changed files\n\n{}\n\nThe workspace you're in is the author's worktree (branch `{}`, based on `{}`): read the code there and run its build/tests. Use `git diff {}...HEAD` to see the changes. Don't edit files.",
        target["id"].as_str().unwrap_or(""),
        target["title"].as_str().unwrap_or(""),
        if focus.is_empty() { String::new() } else { format!("Focus: {focus}\n\n") },
        target["instructions"].as_str().unwrap_or(""),
        target["criteria"].as_str().unwrap_or("(none given)"),
        clip(&last, 6000),
        if files.is_empty() { "(none listed)".into() } else { files },
        target["branch"].as_str().unwrap_or(""),
        target["base_branch"].as_str().unwrap_or(""),
        target["base_branch"].as_str().unwrap_or("HEAD"),
    );
    let ws = workspace_of(core, target).map(|p| format!("path:{}", p.display())).unwrap_or_else(|| "shared".into());
    create_worker(
        core,
        target["project_id"].as_str().unwrap_or(""),
        NewWorker {
            title: format!("Review: {}", target["title"].as_str().unwrap_or("")),
            instructions,
            criteria: "A verdict (approve or changes needed) and concrete findings, most severe first.".into(),
            agent: reviewer,
            task_kind: "review".into(),
            model: String::new(),
            workspace: ws,
            depends_on: vec![],
            parent: Some(lead_thread.to_string()),
            kind: "review".into(),
        },
    )
    .await
}

pub fn answer_decision(core: &CoreRef, id: &str, choice: i64) -> Result<Value> {
    let d = core.db.one("SELECT * FROM decisions WHERE id = ?1", &[&id])?.ok_or_else(|| anyhow!("No such decision"))?;
    if !d["answer"].is_null() {
        return Ok(d);
    }
    let opts = d["options"].as_array().cloned().unwrap_or_default();
    let opt = opts.get(choice as usize).ok_or_else(|| anyhow!("No such option"))?;
    let now = util::now_ms();
    core.db.exec("UPDATE decisions SET answer = ?1, answered_at = ?2 WHERE id = ?3", &[&choice, &now, &id])?;
    let d = core.db.one("SELECT * FROM decisions WHERE id = ?1", &[&id])?.unwrap_or_default();
    let thread = d["thread_id"].as_str().unwrap_or("");
    core.bus.emit("decision", d["project_id"].as_str(), Some(thread), d.clone());
    let body = format!(
        "<wake reason=\"decision-answered\" at=\"{}\">\nThe person answered \"{}\": **{}**{}\n</wake>",
        util::now_iso(),
        d["question"].as_str().unwrap_or(""),
        opt["label"].as_str().unwrap_or(""),
        opt["consequence"].as_str().map(|c| format!(" ({c})")).unwrap_or_default()
    );
    post(core, thread, "wake", "Decision", &body, json!({ "decision": id }), true)?;
    Ok(d)
}

/// A plain-text overview of the project for the lead.
pub fn overview(core: &CoreRef, project: &str) -> String {
    let mut s = String::from("## Threads\n");
    let rows = core.db.all("SELECT id, title, agent, status, status_note, branch, merged, resolved FROM threads WHERE project_id = ?1 AND kind != 'lead' AND deleted_at IS NULL AND resolved = 0 ORDER BY created_at", &[&project]).unwrap_or_default();
    if rows.is_empty() {
        s.push_str("No open threads.\n");
    }
    for r in rows {
        let running = core.running.lock().ok().and_then(|m| m.get(r["id"].as_str().unwrap_or("")).map(|x| (util::now_ms() - x.started) / 60_000));
        s.push_str(&format!(
            "- {} \"{}\" · {} · {}{}{}{}\n",
            r["id"].as_str().unwrap_or(""),
            r["title"].as_str().unwrap_or(""),
            r["agent"].as_str().unwrap_or(""),
            r["status"].as_str().unwrap_or(""),
            r["status_note"].as_str().filter(|x| !x.is_empty()).map(|x| format!(" ({x})")).unwrap_or_default(),
            running.map(|m| format!(" · running {m} min")).unwrap_or_default(),
            if r["merged"] == 1 { " · merged" } else { "" }
        ));
    }
    let decisions = core.db.all("SELECT question FROM decisions WHERE project_id = ?1 AND answer IS NULL", &[&project]).unwrap_or_default();
    if !decisions.is_empty() {
        s.push_str("\n## Waiting on the person\n");
        for d in decisions {
            s.push_str(&format!("- {}\n", d["question"].as_str().unwrap_or("")));
        }
    }
    let claims = core.db.scalar_i64("SELECT COUNT(*) FROM claims WHERE project_id = ?1", &[&project]).unwrap_or(0);
    s.push_str(&format!("\n{claims} path claim(s) active (list_claims shows them).\n\n"));
    s.push_str(&prompts::fleet(core));
    s
}

// ------------------------------------------------------------------------------------------------ startup and watchers

/// After a crash or restart: turns that were running run again, and anything queued gets going.
pub fn recover(core: &CoreRef) {
    let now = util::now_ms();
    let interrupted = core.db.all("SELECT id, thread_id, inputs FROM turns WHERE status = 'running'", &[]).unwrap_or_default();
    for t in &interrupted {
        let ids: Vec<String> = t["inputs"].as_str().unwrap_or("").split(',').filter(|s| !s.is_empty()).map(str::to_string).collect();
        requeue(core, &ids);
        let _ = core.db.exec("UPDATE turns SET status = 'failed', error = 'Interrupted: Model Roamer stopped during this turn', ended_at = ?1 WHERE id = ?2", &[&now, &t["id"].as_str().unwrap_or("")]);
        let tid = t["thread_id"].as_str().unwrap_or("");
        let _ = post(core, tid, "wake", "Model Roamer", &format!("<wake reason=\"restarted\" at=\"{}\">Model Roamer restarted while you were working. Check the state of your workspace, then carry on.</wake>", util::now_iso()), json!({}), false);
        let _ = core.db.exec("UPDATE messages SET pending = 1 WHERE id = (SELECT id FROM messages WHERE thread_id = ?1 ORDER BY created_at DESC LIMIT 1)", &[&tid]);
    }
    let _ = core.db.exec("UPDATE threads SET status = 'idle', status_note = '' WHERE status IN ('working', 'queued')", &[]);
    let pending = core.db.all("SELECT DISTINCT m.thread_id FROM messages m JOIN threads t ON t.id = m.thread_id WHERE m.pending = 1 AND t.deleted_at IS NULL", &[]).unwrap_or_default();
    for p in pending {
        schedule(core, p["thread_id"].as_str().unwrap_or(""));
    }
    if !interrupted.is_empty() {
        tracing::info!("resumed {} interrupted turn(s)", interrupted.len());
    }
}

/// Every minute: threads waiting on a usage limit start again once it resets.
pub async fn watch_limits(core: CoreRef) {
    loop {
        tokio::time::sleep(Duration::from_secs(60)).await;
        let limited = core.db.all("SELECT id, agent FROM threads WHERE status = 'limited' AND deleted_at IS NULL", &[]).unwrap_or_default();
        for t in limited {
            let agent = t["agent"].as_str().unwrap_or("");
            if !core.is_limited(agent) {
                let id = t["id"].as_str().unwrap_or("");
                let _ = core.set_status(id, "idle", "");
                note(&core, id, &format!("{agent}'s usage limit has reset. Picking up again."));
                schedule(&core, id);
            }
        }
    }
}

#[cfg(test)]
mod tests {
    #[test]
    fn spots_transient_failures() {
        assert!(super::transient_error("Failed to refresh OAuth token: another Claude Code process is refreshing it or exited mid-refresh."));
        assert!(super::transient_error(r#"API Error: 529 {"type":"error","error":{"type":"overloaded_error"}}"#));
        assert!(!super::transient_error("Invalid API key · Please run /login"));
    }
}
