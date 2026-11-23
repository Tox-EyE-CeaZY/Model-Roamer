//! Agent harnesses. Every CLI (Claude Code, Codex, Antigravity, and any custom one) sits behind the same
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
    /// `id` pairs a result with its call; the timeline shows them in order, so it is parsed but not read yet.
    ToolUse { #[allow(dead_code)] id: String, name: String, input: String },
    ToolResult { #[allow(dead_code)] id: String, output: String, error: bool },
    Usage { input: i64, output: i64, cached: i64, cost: f64 },
    /// The CLI says its final answer is this.
    Final(String),
    /// How much of the plan's windows is used: {"five_hour": {"used": 0..1, "resets_at": ms}, "weekly": {...}}.
    Quota(Value),
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

#[derive(Debug, Clone, Default, Serialize, serde::Deserialize, PartialEq)]
pub struct ModelOpt {
    pub id: String,
    pub label: String,
    pub note: String,
    /// Reasoning effort levels this model takes, when the CLI says.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub efforts: Vec<String>,
}

/// How to ask a CLI which models it offers: arguments, and what to write to its stdin.
pub struct ModelsProbe {
    pub args: Vec<String>,
    pub stdin: Option<String>,
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
    /// How to ask the installed CLI for the models it offers (None: it can't say, so `info().models` is used).
    fn models_probe(&self) -> Option<ModelsProbe> {
        None
    }
    /// Read the probe's output. The default understands JSON lists and plain "model-id  description" lines.
    fn models_read(&self, out: &str) -> Vec<ModelOpt> {
        parse_model_list(out)
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
        "design": "antigravity", "frontend": "antigravity", "creative": "antigravity", "research": "antigravity",
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

/// Ask the CLI which models it offers. Empty when it can't say (not installed, no probe, an error).
pub async fn discover_models(a: &dyn AgentAdapter, r: &Resolved) -> Vec<ModelOpt> {
    let Some(p) = a.models_probe() else { return vec![] };
    let Ok(out) = procs::run(r, &Launch { args: p.args, stdin: p.stdin, ..Default::default() }, Duration::from_secs(45)).await else { return vec![] };
    if out.timed_out {
        return vec![];
    }
    let mut seen = std::collections::HashSet::new();
    a.models_read(&out.stdout).into_iter().filter(|m| !m.id.is_empty() && seen.insert(m.id.clone())).collect()
}

/// The models to offer: "CLI default" first, then what the CLI reported, or the built-in list when it reported nothing.
pub fn merge_models(builtin: &[ModelOpt], found: &[ModelOpt]) -> Vec<ModelOpt> {
    if found.is_empty() {
        return builtin.to_vec();
    }
    let mut v: Vec<ModelOpt> = builtin.iter().filter(|m| m.id.is_empty()).cloned().collect();
    // "default"/"auto" mean the same as our "CLI default" entry
    v.extend(found.iter().filter(|m| !m.id.is_empty() && m.id != "default" && m.id != "auto").cloned());
    v
}

/// A model list in whatever shape a CLI prints it: a JSON array (or an object holding one) of strings or of objects
/// with an id and a name, or text lines that start with a model id ("gemini-3-pro  Most capable", "- claude-opus").
pub fn parse_model_list(out: &str) -> Vec<ModelOpt> {
    let t = out.trim();
    if let Some(start) = t.find(|c| c == '[' || c == '{') {
        if let Ok(v) = serde_json::from_str::<Value>(&t[start..]) {
            let list = if v.is_array() {
                v
            } else {
                ["models", "data", "items", "available_models"].iter().map(|k| v[*k].clone()).find(|x| x.is_array()).unwrap_or(Value::Null)
            };
            if let Some(arr) = list.as_array() {
                return arr.iter().filter_map(model_from_json).collect();
            }
        }
    }
    static ID: std::sync::OnceLock<regex::Regex> = std::sync::OnceLock::new();
    let re = ID.get_or_init(|| regex::Regex::new(r"^[\s*•>\-]*\(?([A-Za-z][A-Za-z0-9]*(?:[-._/:@][A-Za-z0-9]+)+)\)?(?:\s+\(([^)]*)\))?\s*(?:[-–—:|]\s*)?(.*)$").unwrap());
    let mut v = vec![];
    for line in t.lines() {
        let l = procs::strip_ansi(line);
        if l.trim().is_empty() || l.trim_end().ends_with(':') {
            continue;
        }
        if let Some(c) = re.captures(&l) {
            let id = c[1].to_string();
            // a model id has a digit or a known family word; this keeps out URLs and prose
            let lid = id.to_lowercase();
            if id.contains("://") || !(lid.chars().any(|ch| ch.is_ascii_digit()) || ["opus", "sonnet", "haiku", "fable", "gemini", "gpt", "flash", "pro", "claude"].iter().any(|w| lid.contains(w))) {
                continue;
            }
            let label = c.get(2).map(|m| m.as_str().trim().to_string()).filter(|s| !s.is_empty() && *s != id).unwrap_or_else(|| pretty_model(&id));
            v.push(ModelOpt { id, label, note: c.get(3).map(|m| m.as_str().trim().to_string()).unwrap_or_default(), ..Default::default() });
        }
    }
    v
}

/// A readable name for a bare model id: "gemini-3.5-flash" → "Gemini 3.5 Flash".
pub fn pretty_model(id: &str) -> String {
    id.split(['-', '_'])
        .filter(|w| !w.is_empty())
        .map(|w| {
            let mut c = w.chars();
            match c.next() {
                Some(f) if f.is_ascii_alphabetic() && w.len() <= 3 && !["pro", "max", "mini", "lite"].contains(&w) => w.to_uppercase(),
                Some(f) => f.to_uppercase().collect::<String>() + c.as_str(),
                None => String::new(),
            }
        })
        .collect::<Vec<_>>()
        .join(" ")
}

fn model_from_json(m: &Value) -> Option<ModelOpt> {
    if let Some(id) = m.as_str() {
        return Some(ModelOpt { id: id.into(), label: id.into(), ..Default::default() });
    }
    if m["visibility"].as_str().is_some_and(|x| x == "hide" || x == "hidden") || m["hidden"] == true {
        return None;
    }
    let id = ["value", "slug", "id", "modelId", "model", "name"].iter().find_map(|k| m[*k].as_str().filter(|s| !s.is_empty()))?.to_string();
    let label = ["displayName", "display_name", "label", "name"].iter().find_map(|k| m[*k].as_str().filter(|s| !s.is_empty() && *s != id)).map(str::to_string).unwrap_or_else(|| pretty_model(&id));
    let note = ["description", "note"].iter().find_map(|k| m[*k].as_str()).unwrap_or("").to_string();
    let efforts = m["supportedEffortLevels"]
        .as_array()
        .or(m["supported_reasoning_levels"].as_array())
        .map(|a| a.iter().filter_map(|e| e.as_str().or(e["effort"].as_str()).map(str::to_string)).collect())
        .unwrap_or_default();
    Some(ModelOpt { id, label, note, efforts })
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

/// Seconds or milliseconds since the epoch, as milliseconds.
pub fn epoch_ms(n: i64) -> i64 {
    if n < 10_000_000_000 { n * 1000 } else { n }
}

/// Codex-style rate limits anywhere in an event: {primary|secondary: {used_percent, window_minutes, resets_in_seconds |
/// resets_at}}. The window decides 5-hour (up to a day) or weekly.
pub fn codex_quota(v: &Value) -> Option<Value> {
    codex_quota_at(v, crate::util::now_ms())
}

/// The same, for an event that happened at `base` (ms): "resets in N seconds" counts from then.
pub fn codex_quota_at(v: &Value, base: i64) -> Option<Value> {
    let rl = [&v["rate_limits"], &v["msg"]["rate_limits"], &v["info"]["rate_limits"], &v["payload"]["rate_limits"]].into_iter().find(|x| x.is_object())?;
    let mut q = json!({});
    for k in ["primary", "secondary"] {
        let w = &rl[k];
        let Some(pct) = w["used_percent"].as_f64() else { continue };
        let mins = w["window_minutes"].as_i64().unwrap_or(if k == "primary" { 300 } else { 10080 });
        let resets = w["resets_at"].as_i64().map(epoch_ms).or_else(|| w["resets_in_seconds"].as_i64().map(|s| base + s * 1000));
        q[if mins <= 1440 { "five_hour" } else { "weekly" }] = json!({ "used": (pct / 100.0).clamp(0.0, 1.0), "resets_at": resets });
    }
    (q.as_object().is_some_and(|o| !o.is_empty())).then_some(q)
}

/// Which plan window a limit message is about: "weekly", "five_hour", or "" when it doesn't say.
pub fn limit_window(text: &str) -> &'static str {
    let t = text.to_lowercase();
    if t.contains("weekly") || t.contains("week") || t.contains("7-day") || t.contains("7 day") {
        "weekly"
    } else if t.contains("5-hour") || t.contains("5 hour") || t.contains("five-hour") || t.contains("5h") || t.contains("session limit") || t.contains("hourly") {
        "five_hour"
    } else {
        ""
    }
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
    fn reads_model_lists() {
        // Codex: JSON catalog, hidden models left out, effort levels kept
        let c = parse_model_list(r#"{"models":[{"slug":"gpt-6.2-sol","display_name":"GPT-6.2-Sol","description":"Fast","visibility":"list","supported_reasoning_levels":[{"effort":"low"},{"effort":"high"}]},{"slug":"old","visibility":"hide"}]}"#);
        assert_eq!(c.len(), 1);
        assert_eq!((c[0].id.as_str(), c[0].label.as_str(), c[0].efforts.len()), ("gpt-6.2-sol", "GPT-6.2-Sol", 2));
        // Gemini: JSON array
        let g = parse_model_list(r#"[{"modelId":"auto","id":"auto","displayName":"Auto"},{"id":"gemini-4-argon","displayName":"Gemini 4 Argon","description":""}]"#);
        assert_eq!(g[1].label, "Gemini 4 Argon");
        // plain text, as a CLI might print it
        let t = parse_model_list("Available models:\n  gemini-4-argon (Gemini 4 Argon)  Most capable\n  - claude-opus-5-5: Opus 5.5\nSee https://example.com/models for more\n");
        assert_eq!(t.iter().map(|m| m.id.as_str()).collect::<Vec<_>>(), ["gemini-4-argon", "claude-opus-5-5"]);
        assert_eq!(t[0].label, "Gemini 4 Argon");
        assert_eq!(pretty_model("gemini-3.5-flash"), "Gemini 3.5 Flash");
        assert_eq!(pretty_model("gpt-6-mini"), "GPT 6 Mini");
        // "CLI default" stays first; the CLI's own "default"/"auto" entries are dropped
        let merged = merge_models(&[ModelOpt { id: "".into(), label: "CLI default".into(), ..Default::default() }], &g);
        assert_eq!(merged.iter().map(|m| m.id.as_str()).collect::<Vec<_>>(), ["", "gemini-4-argon"]);
    }

    #[test]
    fn limits_and_resets() {
        assert!(limit_in("all good").is_none());
        let r = limit_in("You've hit your usage limit. Try again in 2 hours 5 minutes").unwrap().unwrap();
        let d = r - crate::util::now_ms();
        assert!(d > 7_400_000 && d < 7_600_000, "{d}");
        assert_eq!(limit_in("429 Too Many Requests, resets at 1735689600").unwrap(), Some(1_735_689_600_000));
    }
}
