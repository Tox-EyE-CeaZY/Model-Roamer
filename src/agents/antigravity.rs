//! Google Antigravity CLI (`agy`), headless with `-p`, signed in with Google. agy can hang or print nothing without a
//! terminal (especially on Windows), so its turns run inside a pseudo-terminal (ConPTY).

use super::*;

pub struct Antigravity;

impl AgentAdapter for Antigravity {
    fn info(&self) -> Info {
        Info {
            id: "antigravity".into(),
            name: "Antigravity".into(),
            vendor: "Google".into(),
            hue: 318,
            strengths: vec!["UI and visual design".into(), "front-end builds".into(), "browser testing".into(), "agentic app building".into(), "creative work".into()],
            summary: "Google's agent-first harness: front-end and full app builds, visual design, checking work in a browser.".into(),
            site: "https://antigravity.google".into(),
            models: vec![
                ModelOpt { id: "".into(), label: "CLI default".into(), note: "Whatever agy is set to (see `agy models`)".into() },
                ModelOpt { id: "gemini-3-pro".into(), label: "Gemini Pro".into(), note: "".into() },
                ModelOpt { id: "gemini-3-flash".into(), label: "Gemini Flash".into(), note: "".into() },
                ModelOpt { id: "claude-opus".into(), label: "Claude Opus (via agy)".into(), note: "If your plan includes it".into() },
            ],
            mcp: true,
            can_lead: true,
        }
    }

    fn binaries(&self) -> Vec<String> {
        vec!["agy".into(), "antigravity-cli".into()]
    }

    fn needs_pty(&self) -> bool {
        true
    }

    fn raw_is_text(&self) -> bool {
        true
    }

    fn launch(&self, spec: &TurnSpec) -> anyhow::Result<Launch> {
        let mut a: Vec<String> = vec!["--output-format".into(), "stream-json".into()];
        match spec.autonomy {
            Autonomy::Full => a.push("--dangerously-skip-permissions".into()),
            Autonomy::Edits => a.extend(["--mode".into(), "accept-edits".into()]),
            Autonomy::ReadOnly => a.extend(["--mode".into(), "plan".into()]),
        }
        if let Some(sid) = &spec.session {
            a.extend(["--conversation".into(), sid.clone()]);
        }
        if !spec.model.is_empty() {
            a.extend(["--model".into(), spec.model.clone()]);
        }
        if !spec.effort.is_empty() {
            a.extend(["--effort".into(), spec.effort.clone()]);
        }
        for d in &spec.extra_dirs {
            a.extend(["--add-dir".into(), d.display().to_string()]);
        }
        a.extend(spec.extra_args.iter().cloned());
        a.extend(["-p".into(), arg_prompt(spec, &with_system(spec))]);
        let mut env = vec![("AGY_CLI_DISABLE_AUTO_UPDATE".to_string(), "1".to_string()), ("NO_COLOR".into(), "1".into())];
        // the roamer tools are registered once (Connect tools); they find this thread through these variables
        if let Some(m) = &spec.mcp {
            env.extend(m.env.iter().cloned());
        }
        if spec.headless && cfg!(target_os = "linux") {
            env.push(("GEMINI_FORCE_FILE_STORAGE".into(), "true".into()));
        }
        Ok(Launch { args: a, stdin: None, cwd: Some(spec.cwd.clone()), env, pty: true })
    }

    fn parse(&self, line: &str, st: &mut Value) -> Vec<AgentEvent> {
        let Some(v) = json_line(line) else {
            let t = line.trim();
            if t.is_empty() {
                return vec![];
            }
            if let Some(until) = limit_in(t).filter(|_| t.len() < 600) {
                return vec![AgentEvent::Limit { until, message: t.to_string() }];
            }
            return vec![AgentEvent::Raw(line.to_string())];
        };
        // the json envelope: {conversation_id, status, response, usage}
        if v.get("response").is_some() || v.get("conversation_id").is_some() {
            let mut out = vec![];
            if let Some(id) = v["conversation_id"].as_str() {
                out.push(AgentEvent::Session(id.into()));
            }
            let u = &v["usage"];
            out.push(AgentEvent::Usage {
                input: u["input_tokens"].as_i64().or(u["prompt_tokens"].as_i64()).unwrap_or(0),
                output: u["output_tokens"].as_i64().or(u["completion_tokens"].as_i64()).unwrap_or(0),
                cached: u["cached_tokens"].as_i64().unwrap_or(0),
                cost: 0.0,
            });
            let status = s(&v["status"]).to_lowercase();
            let resp = s(&v["response"]);
            if !status.is_empty() && status != "success" {
                let msg = if resp.is_empty() { v.to_string() } else { resp };
                match limit_in(&msg) {
                    Some(until) => out.push(AgentEvent::Limit { until, message: msg }),
                    None => out.push(AgentEvent::Error(msg)),
                }
            } else if !resp.is_empty() {
                out.push(AgentEvent::Final(resp));
            }
            return out;
        }
        // stream-json: Gemini-style events first, then Claude-style ones
        let ev = super::gemini::parse_gemini(&v, st);
        if !ev.is_empty() {
            return ev;
        }
        let ev = super::claude::Claude.parse(line, st);
        if ev.iter().any(|e| !matches!(e, AgentEvent::Raw(_))) {
            return ev;
        }
        if let Some(id) = v["conversation_id"].as_str().or(v["session_id"].as_str()) {
            return vec![AgentEvent::Session(id.into())];
        }
        if let Some(t) = v["text"].as_str().or(v["content"].as_str()) {
            return vec![AgentEvent::Delta(t.into())];
        }
        vec![AgentEvent::Raw(line.to_string())]
    }

    fn auth_probe(&self) -> Option<Vec<String>> {
        // listing models needs a signed-in session
        Some(vec!["models".into()])
    }

    fn auth_read(&self, ok: bool, out: &str) -> AuthState {
        let t = out.to_lowercase();
        if t.contains("sign in") || t.contains("log in") || t.contains("not authenticated") || t.contains("unauthenticated") {
            AuthState { signed_in: "no".into(), detail: "Not signed in".into() }
        } else if ok {
            AuthState { signed_in: "yes".into(), detail: "Signed in with Google".into() }
        } else {
            AuthState { signed_in: "unknown".into(), detail: String::new() }
        }
    }

    fn install(&self) -> Vec<Step> {
        vec![shell(
            "Install the Antigravity CLI (official installer)",
            "irm https://antigravity.google/cli/install.ps1 | iex",
            "curl -fsSL https://antigravity.google/cli/install.sh | bash",
            &[],
        )]
    }

    fn update(&self) -> Vec<Step> {
        vec![step("Update agy", &["agy", "update"], &["agy"])]
    }

    fn login(&self, _headless: bool) -> Step {
        // agy signs in on its first screen; on a server it prints a link to open elsewhere and asks for the code
        step("Sign in to Google (finish the sign-in, then type /quit)", &["agy"], &["agy"])
    }

    fn connect_tools(&self, m: &McpLaunch) -> Option<Step> {
        let mut cmd: Vec<String> = vec!["agy".into(), "mcp".into(), "add".into(), "-t".into(), "stdio".into(), "roamer".into(), m.command.clone()];
        cmd.extend(m.args.iter().cloned());
        Some(Step { label: "Give agy Model Roamer's tools".into(), cmd, needs: vec!["agy".into()] })
    }
}
