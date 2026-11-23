//! Agent harnesses. Every CLI (Claude Code, Codex, Gemini, Antigravity, and any custom one) sits behind the same
//! `AgentAdapter` trait: it says how to start a turn, how to read the CLI's output into `AgentEvent`s, how to check the
//! sign-in, and how to install, sign in and update. The runner does the rest the same way for all of them, so adding
//! an agent means writing one adapter.

pub mod antigravity;
pub mod claude;
pub mod codex;
pub mod custom;
pub mod gemini;

use crate::procs::{self, Launch, Resolved};
use serde::Serialize;
use serde_json::{json, Value};
use std::{
    path::{Path, PathBuf},
    sync::Arc,
    time::Duration,
};

/// What a CLI reported during a turn, in one shape for every agent.
#[derive(Debug, Clone)]
pub enum AgentEvent {
    /// The CLI's own session/conversation id, used to resume next turn.
    Session(String),
    /// Streaming text (appended to the current text block).
    Delta(String),
    /// A complete text block (replaces any streamed deltas for it).
    Text(String),
    Thinking(String),
    ToolUse { id: String, name: String, input: String },
    ToolResult { id: String, output: String, error: bool },
    Usage { input: i64, output: i64, cached: i64, cost: f64 },
    /// The CLI says its final answer is this.
    Final(String),
    /// A subscription or rate limit was hit; `until` is when it resets (ms), when known.
    Limit { until: Option<i64>, message: String },
    Error(String),
    /// An output line the adapter didn't recognize (kept for the log).
    Raw(String),
}

/// How much an agent may do on its own.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "lowercase")]
pub enum Autonomy {
    /// Edit files and run commands in its workspace without asking (workers in their own worktree).
    Full,
    /// Edit files; ask before commands where the CLI supports that.
    Edits,
    /// Read and plan only.
    ReadOnly,
}

impl Autonomy {
    pub fn parse(s: &str) -> Autonomy {
        match s {
            "edits" => Autonomy::Edits,
            "readonly" | "read-only" | "plan" => Autonomy::ReadOnly,
            _ => Autonomy::Full,
        }
    }
}

/// How to reach Model Roamer's own tools (an MCP server over stdio, run by this same program).
#[derive(Debug, Clone)]
pub struct McpLaunch {
    pub command: String,
    pub args: Vec<String>,
    pub env: Vec<(String, String)>,
}

/// Everything an adapter needs to start one turn.
pub struct TurnSpec {
    /// The message(s) for this turn, already composed.
    pub prompt: String,
    /// Standing instructions (role, tools, project). Sent as a system prompt where the CLI has one, else up front.
    pub system: String,
    /// Resume this CLI session (None: start a new one).
    pub session: Option<String>,
    pub cwd: PathBuf,
    pub model: String,
    pub effort: String,
    pub autonomy: Autonomy,
    pub mcp: Option<McpLaunch>,
    pub extra_dirs: Vec<PathBuf>,
    /// A private folder for this turn's files (system prompt, MCP config).
    pub scratch: PathBuf,
    pub extra_args: Vec<String>,
    /// Running on a server with no one at its screen (sign-in pages can't open there).
    pub headless: bool,
}

#[derive(Debug, Clone, Serialize)]
pub struct AuthState {
    /// "yes", "no" or "unknown"
    pub signed_in: String,
    pub detail: String,
}

#[derive(Debug, Clone, Serialize)]
pub struct Step {
    pub label: String,
    /// The program and its arguments, run in a terminal the user can watch and type into.
    pub cmd: Vec<String>,
    /// Programs that must exist first (e.g. "npm").
    pub needs: Vec<String>,
}

#[derive(Debug, Clone, Serialize)]
pub struct ModelOpt {
    pub id: String,
    pub label: String,
    pub note: String,
}

/// Static facts about an agent, shown in the UI and given to the lead so it can route well.
#[derive(Debug, Clone, Serialize)]
pub struct Info {
    pub id: String,
    pub name: String,
    pub vendor: String,
    pub hue: i32,
    pub strengths: Vec<String>,
    pub summary: String,
    pub site: String,
    pub models: Vec<ModelOpt>,
    pub mcp: bool,
    pub can_lead: bool,
}

pub trait AgentAdapter: Send + Sync {
    fn info(&self) -> Info;
    fn id(&self) -> String {
        self.info().id
    }
    /// Program names to look for, in order.
    fn binaries(&self) -> Vec<String>;
    /// Arguments, environment and stdin for one turn.
    fn launch(&self, spec: &TurnSpec) -> anyhow::Result<Launch>;
    /// Turn one output line into events. `st` is per-turn scratch state.
    fn parse(&self, line: &str, st: &mut Value) -> Vec<AgentEvent>;
    /// How to check the sign-in: a command, then a reading of its output.
    fn auth_probe(&self) -> Option<Vec<String>> {
        None
    }
    fn auth_read(&self, _ok: bool, _out: &str) -> AuthState {
        AuthState { signed_in: "unknown".into(), detail: String::new() }
    }
    /// Files whose presence means a sign-in happened (used when there is no probe, or it fails).
    fn auth_files(&self) -> Vec<PathBuf> {
        vec![]
    }
    fn install(&self) -> Vec<Step>;
    fn update(&self) -> Vec<Step> {
        self.install()
    }
    /// The interactive sign-in, run in a terminal panel. `headless`: the sign-in page must open on another device.
    fn login(&self, headless: bool) -> Step;
    fn logout(&self) -> Option<Step> {
        None
    }
    /// Whether turns must run inside a pseudo-terminal.
    fn needs_pty(&self) -> bool {
        false
    }
    /// Whether lines that aren't events are the answer itself (plain-text CLIs).
    fn raw_is_text(&self) -> bool {
        false
    }
    /// A one-time step that makes Model Roamer's tools available to this CLI (when they can't be passed per turn).
    fn connect_tools(&self, _mcp: &McpLaunch) -> Option<Step> {
        None
    }
}

/// Every built-in adapter plus the user's custom ones.
pub fn registry(custom: &Value) -> Vec<Arc<dyn AgentAdapter>> {
    let mut v: Vec<Arc<dyn AgentAdapter>> = vec![
        Arc::new(claude::Claude),
        Arc::new(codex::Codex),
        Arc::new(gemini::Gemini),
        Arc::new(antigravity::Antigravity),
    ];
    if let Some(list) = custom.as_array() {
        for c in list {
            if let Some(a) = custom::Custom::from_json(c) {
                v.push(Arc::new(a));
            }
        }
    }
    v
}

/// Which task kinds go to which agent unless the user or the lead says otherwise.
pub fn default_routing() -> Value {
    json!({
        "plan": "claude", "orchestrate": "claude", "core": "claude", "refactor": "claude", "review": "claude", "docs": "claude",
        "design": "gemini", "frontend": "gemini", "creative": "gemini", "research": "gemini",
        "backend": "codex", "api": "codex", "data": "codex", "infra": "codex", "scripts": "codex", "tests": "codex",
        "general": "claude"
    })
}

pub const TASK_KINDS: &[(&str, &str)] = &[
    ("plan", "Planning, breaking work down"),
    ("core", "Core code, careful logic"),
    ("refactor", "Refactoring and cleanup"),
    ("review", "Code review"),
    ("docs", "Writing and docs"),
    ("design", "Visual and UI design"),
    ("frontend", "Front-end layout and styling"),
    ("creative", "Creative work, naming, copy"),
    ("research", "Research and exploration"),
    ("backend", "Back-end services"),
    ("api", "APIs and integrations"),
    ("data", "Data layer, schemas, queries"),
    ("infra", "Infrastructure, CI, deploys"),
    ("scripts", "Scripts and tooling"),
    ("tests", "Tests"),
    ("general", "Anything else"),
];

/// Find an agent's program, honoring a path the user set in Settings.
pub fn locate(a: &dyn AgentAdapter, explicit: Option<&str>) -> Option<Resolved> {
    let names = a.binaries();
    let refs: Vec<&str> = names.iter().map(String::as_str).collect();
    procs::resolve(&refs, explicit)
}

pub async fn version(r: &Resolved) -> Option<String> {
    let out = procs::run(r, &Launch { args: vec!["--version".into()], ..Default::default() }, Duration::from_secs(25)).await.ok()?;
    let t = out.text();
    let re = regex::Regex::new(r"\d+\.\d+(\.\d+)?([-.][0-9A-Za-z.]+)?").unwrap();
    re.find(&t).map(|m| m.as_str().to_string()).or_else(|| t.lines().next().map(|s| s.trim().to_string()).filter(|s| !s.is_empty()))
}

pub async fn auth(a: &dyn AgentAdapter, r: &Resolved) -> AuthState {
    if let Some(args) = a.auth_probe() {
        if let Ok(out) = procs::run(r, &Launch { args, ..Default::default() }, Duration::from_secs(25)).await {
            if !out.timed_out {
                let st = a.auth_read(out.ok(), &out.text());
                if st.signed_in != "unknown" {
                    return st;
                }
            }
        }
    }
    let files = a.auth_files();
    if files.iter().any(|f| f.exists()) {
        return AuthState { signed_in: "yes".into(), detail: "Saved sign-in found".into() };
    }
    if !files.is_empty() {
        return AuthState { signed_in: "no".into(), detail: "No saved sign-in found".into() };
    }
    AuthState { signed_in: "unknown".into(), detail: "This CLI has no way to check; run a test to be sure".into() }
}

// ------------------------------------------------------------------------------------------------ shared helpers

/// Read JSON from a line that may carry a prefix (PTY noise, a log tag).
pub fn json_line(line: &str) -> Option<Value> {
    let t = line.trim();
    let start = t.find('{')?;
    serde_json::from_str(&t[start..]).ok()
}

pub fn s(v: &Value) -> String {
    match v {
        Value::String(s) => s.clone(),
        Value::Null => String::new(),
        other => other.to_string(),
    }
}

/// Does this text say a usage or rate limit was hit? Returns the reset time (ms) when it can tell.
pub fn limit_in(text: &str) -> Option<Option<i64>> {
    static RE: std::sync::OnceLock<regex::Regex> = std::sync::OnceLock::new();
    let re = RE.get_or_init(|| {
        regex::Regex::new(r"(?i)(usage limit|rate.?limit|quota (exceeded|exhausted)|resource.?exhausted|too many requests|limit (reached|exceeded)|hit your limit|out of (credits|usage)|\b429\b|weekly limit|session limit)").unwrap()
    });
    if !re.is_match(text) {
        return None;
    }
    Some(reset_time(text))
}

/// "try again in 2 hours 13 minutes", "resets in 45m", "resets at 1735689600"
pub fn reset_time(text: &str) -> Option<i64> {
    let now = crate::util::now_ms();
    let t = text.to_lowercase();
    if let Some(c) = regex::Regex::new(r"(?:resets?|reset_at|resetsat|until|\|)\D{0,6}(\d{10,13})").unwrap().captures(&t) {
        let n: i64 = c[1].parse().ok()?;
        return Some(if n < 10_000_000_000 { n * 1000 } else { n });
    }
    let re = regex::Regex::new(r"(?:in|after)\s+((?:\d+\s*(?:days?|d|hours?|hrs?|h|minutes?|mins?|m|seconds?|secs?|s)\b[\s,and]*)+)").unwrap();
    let c = re.captures(&t)?;
    let mut ms = 0i64;
    for p in regex::Regex::new(r"(\d+)\s*([a-z]+)").unwrap().captures_iter(&c[1]) {
        let n: i64 = p[1].parse().unwrap_or(0);
        ms += n * match &p[2][..1] {
            "d" => 86_400_000,
            "h" => 3_600_000,
            "m" => 60_000,
            _ => 1000,
        };
    }
    (ms > 0).then_some(now + ms)
}

/// Long prompts can't go on a command line (Windows caps it near 32k): park them in a file and point at it.
pub fn arg_prompt(spec: &TurnSpec, text: &str) -> String {
    if text.len() < 20_000 {
        return text.to_string();
    }
    let p = spec.scratch.join("prompt.md");
    let _ = std::fs::write(&p, text);
    format!("Your full message is in the file {} . Read the whole file first, then act on it.", p.display())
}

/// The instructions plus the message, for CLIs without a separate system prompt.
pub fn with_system(spec: &TurnSpec) -> String {
    if spec.session.is_some() || spec.system.trim().is_empty() {
        spec.prompt.clone()
    } else {
        format!("<model-roamer-instructions>\n{}\n</model-roamer-instructions>\n\n{}", spec.system, spec.prompt)
    }
}

pub fn home() -> PathBuf {
    crate::os::home()
}

pub fn step(label: &str, cmd: &[&str], needs: &[&str]) -> Step {
    Step { label: label.into(), cmd: cmd.iter().map(|s| s.to_string()).collect(), needs: needs.iter().map(|s| s.to_string()).collect() }
}

/// Run a shell line in the platform's shell (installers are usually one-liners).
pub fn shell(label: &str, windows_ps: &str, unix_sh: &str, needs: &[&str]) -> Step {
    if cfg!(windows) {
        step(label, &["powershell", "-NoProfile", "-ExecutionPolicy", "Bypass", "-Command", windows_ps], needs)
    } else {
        step(label, &["bash", "-lc", unix_sh], needs)
    }
}

pub fn npm_global(label: &str, pkg: &str) -> Step {
    if cfg!(windows) {
        step(label, &["npm.cmd", "install", "-g", pkg], &["npm"])
    } else {
        step(label, &["npm", "install", "-g", pkg], &["npm"])
    }
}

/// Write a file for the CLI to read, returning its path as text.
pub fn scratch_file(dir: &Path, name: &str, body: &str) -> anyhow::Result<String> {
    std::fs::create_dir_all(dir)?;
    let p = dir.join(name);
    std::fs::write(&p, body)?;
    Ok(p.display().to_string())
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn limits_and_resets() {
        assert!(limit_in("all good").is_none());
        let r = limit_in("You've hit your usage limit. Try again in 2 hours 5 minutes").unwrap().unwrap();
        let d = r - crate::util::now_ms();
        assert!(d > 7_400_000 && d < 7_600_000, "{d}");
        assert_eq!(limit_in("429 Too Many Requests, resets at 1735689600").unwrap(), Some(1_735_689_600_000));
    }
}
