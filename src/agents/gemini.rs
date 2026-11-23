//! Gemini CLI (`gemini -p --output-format stream-json`), signed in with Google.

use super::*;

pub struct Gemini;

/// Parse Gemini-style stream-json (Antigravity's output looks much the same, so it reuses this).
pub fn parse_gemini(v: &Value, st: &mut Value) -> Vec<AgentEvent> {
    let mut out = vec![];
    match v["type"].as_str().unwrap_or("") {
        "init" => {
            if let Some(id) = v["session_id"].as_str() {
                out.push(AgentEvent::Session(id.into()));
            }
            if let Some(m) = v["model"].as_str() {
                st["model"] = json!(m);
            }
        }
        "message" => {
            if v["role"] == "assistant" {
                let c = s(&v["content"]);
                if v["delta"].as_bool() == Some(true) {
                    out.push(AgentEvent::Delta(c));
                } else {
                    out.push(AgentEvent::Text(c));
                }
            }
        }
        "tool_use" => out.push(AgentEvent::ToolUse { id: s(&v["tool_id"]), name: s(&v["tool_name"]), input: v["parameters"].to_string() }),
        "tool_result" => {
            let err = v["status"].as_str().is_some_and(|x| x != "success");
            let body = if v["output"].is_null() { s(&v["error"]["message"]) } else { s(&v["output"]) };
            out.push(AgentEvent::ToolResult { id: s(&v["tool_id"]), output: body, error: err });
        }
        "error" => {
            let msg = s(&v["message"]);
            if v["severity"] == "warning" {
                out.push(AgentEvent::Raw(msg));
            } else {
                match limit_in(&msg) {
                    Some(until) => out.push(AgentEvent::Limit { until, message: msg }),
                    None => out.push(AgentEvent::Error(msg)),
                }
            }
        }
        "result" => {
            let stt = &v["stats"];
            out.push(AgentEvent::Usage {
                input: stt["input_tokens"].as_i64().or(stt["input"].as_i64()).unwrap_or(0),
                output: stt["output_tokens"].as_i64().unwrap_or(0),
                cached: stt["cached"].as_i64().or(stt["cached_tokens"].as_i64()).unwrap_or(0),
                cost: 0.0,
            });
            if v["status"] == "error" {
                let msg = s(&v["error"]["message"]);
                match limit_in(&msg) {
                    Some(until) => out.push(AgentEvent::Limit { until, message: msg }),
                    None => out.push(AgentEvent::Error(if msg.is_empty() { "The turn ended with an error".into() } else { msg })),
                }
            }
        }
        _ => {}
    }
    out
}

impl AgentAdapter for Gemini {
    fn info(&self) -> Info {
        Info {
            id: "gemini".into(),
            name: "Gemini CLI".into(),
            vendor: "Google".into(),
            hue: 262,
            strengths: vec!["creative work".into(), "UI and visual design".into(), "front-end layout".into(), "design exploration".into(), "large-context research".into()],
            summary: "Creative and visual: UI design, front-end layout, design exploration, reading huge codebases at once.".into(),
            site: "https://github.com/google-gemini/gemini-cli".into(),
            models: vec![
                // used until `gemini models list` answers; these aliases always mean the newest of each kind
                ModelOpt { id: "".into(), label: "CLI default".into(), note: "Auto-routes between models".into(), ..Default::default() },
                ModelOpt { id: "pro".into(), label: "Pro (latest)".into(), note: "Most capable".into(), ..Default::default() },
                ModelOpt { id: "flash".into(), label: "Flash (latest)".into(), note: "Fast".into(), ..Default::default() },
                ModelOpt { id: "flash-lite".into(), label: "Flash-Lite (latest)".into(), note: "Fastest".into(), ..Default::default() },
            ],
            mcp: true,
            can_lead: true,
        }
    }

    fn binaries(&self) -> Vec<String> {
        vec!["gemini".into()]
    }

    fn launch(&self, spec: &TurnSpec) -> anyhow::Result<Launch> {
        let mut a: Vec<String> = vec!["--output-format".into(), "stream-json".into(), "--skip-trust".into()];
        a.push("--approval-mode".into());
        a.push(
            match spec.autonomy {
                Autonomy::Full => "yolo",
                Autonomy::Edits => "auto_edit",
                Autonomy::ReadOnly => "plan",
            }
            .into(),
        );
        match &spec.session {
            Some(sid) => a.extend(["--resume".into(), sid.clone()]),
            None => {
                // pick the session id ourselves so it's known even if the CLI never reports it
                let sid = uuid::Uuid::new_v4().to_string();
                a.extend(["--session-id".into(), sid]);
            }
        }
        if !spec.model.is_empty() {
            a.extend(["-m".into(), spec.model.clone()]);
        }
        for d in &spec.extra_dirs {
            a.extend(["--include-directories".into(), d.display().to_string()]);
        }
        let mut env = vec![];
        if let Some(m) = &spec.mcp {
            let envm: serde_json::Map<String, Value> = m.env.iter().map(|(k, v)| (k.clone(), json!(v))).collect();
            let cfg = json!({ "mcpServers": { "roamer": { "command": m.command, "args": m.args, "env": envm, "trust": true, "timeout": 600000 } } });
            let f = scratch_file(&spec.scratch, "gemini-settings.json", &cfg.to_string())?;
            env.push(("GEMINI_CLI_SYSTEM_SETTINGS_PATH".to_string(), f));
        }
        if spec.headless {
            env.push(("NO_BROWSER".into(), "true".into()));
        }
        a.extend(spec.extra_args.iter().cloned());
        // stdin carries the message; -p switches on headless mode and is appended to it
        a.extend(["-p".into(), " ".into()]);
        Ok(Launch { args: a, stdin: Some(with_system(spec)), cwd: Some(spec.cwd.clone()), env, pty: false })
    }

    fn models_probe(&self) -> Option<ModelsProbe> {
        Some(ModelsProbe { args: vec!["models".into(), "list".into(), "--output-format".into(), "json".into()], stdin: None })
    }

    fn parse(&self, line: &str, st: &mut Value) -> Vec<AgentEvent> {
        match json_line(line) {
            Some(v) if v["type"].is_string() => parse_gemini(&v, st),
            _ if line.trim().is_empty() => vec![],
            _ => match limit_in(line) {
                Some(until) if line.len() < 600 => vec![AgentEvent::Limit { until, message: line.trim().to_string() }],
                _ => vec![AgentEvent::Raw(line.to_string())],
            },
        }
    }

    fn auth_files(&self) -> Vec<PathBuf> {
        let g = home().join(".gemini");
        vec![g.join("oauth_creds.json"), g.join("google_accounts.json")]
    }

    fn install(&self) -> Vec<Step> {
        let mut v = vec![npm_global("Install Gemini CLI with npm", "@google/gemini-cli")];
        if cfg!(target_os = "macos") {
            v.push(step("Or install with Homebrew", &["brew", "install", "gemini-cli"], &["brew"]));
        }
        v
    }

    fn update(&self) -> Vec<Step> {
        vec![npm_global("Update Gemini CLI", "@google/gemini-cli@latest")]
    }

    fn login(&self, _headless: bool) -> Step {
        // Gemini signs in from its own first screen: choose "Login with Google", finish in the browser, then /quit
        step("Sign in to Google (choose Login with Google, then type /quit)", &["gemini"], &["gemini"])
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn parses_stream() {
        let g = Gemini;
        let mut st = json!({});
        let e = g.parse(r#"{"type":"init","session_id":"s1","model":"gemini-3-pro"}"#, &mut st);
        assert!(matches!(&e[0], AgentEvent::Session(s) if s == "s1"));
        let e = g.parse(r#"{"type":"message","role":"assistant","content":"Hel","delta":true}"#, &mut st);
        assert!(matches!(&e[0], AgentEvent::Delta(t) if t == "Hel"));
        let e = g.parse(r#"{"type":"result","status":"error","error":{"message":"Quota exceeded for quota metric"},"stats":{}}"#, &mut st);
        assert!(e.iter().any(|x| matches!(x, AgentEvent::Limit { .. })));
    }
}
