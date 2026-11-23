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
                // `agy models` fills in the real list (it needs a sign-in)
                ModelOpt { id: "".into(), label: "CLI default".into(), note: "Whatever agy is set to".into(), ..Default::default() },
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
        // a JSON event the terminal broke over two lines: keep the start and try again with the next line
        let joined;
        let line = if let Some(p) = st["partial"].as_str().filter(|p| !p.is_empty()) {
            joined = format!("{p}{}", line.trim());
            st["partial"] = json!("");
            joined.as_str()
        } else {
            line
        };
        if line.trim_start().starts_with('{') && json_line(line).is_none() && line.len() < 1_000_000 {
            st["partial"] = json!(line.trim());
            return vec![];
        }
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
        // stream-json: {"event":"step_update","step_update":{...}} while it works, {"event":"result","result":{...}} at the end
        if v["event"] == "step_update" {
            return step_update(&v["step_update"], st);
        }
        let v = if v["event"] == "result" { v["result"].clone() } else { v };
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
                cached: u["cached_tokens"].as_i64().or(u["cache_read_tokens"].as_i64()).unwrap_or(0),
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

    fn models_probe(&self) -> Option<ModelsProbe> {
        Some(ModelsProbe { args: vec!["models".into()], stdin: None })
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

/// One step of agy's work: the reply streaming in, its thinking, or a tool it used. Your own message comes back as a
/// `user_input` step and is skipped.
/// The reply text inside a message that was stored as raw stream-json (by builds before the stream was parsed), or
/// None when the body isn't that.
pub fn repair_body(body: &str) -> Option<String> {
    if !body.trim_start().starts_with("{\"event\"") {
        return None;
    }
    let mut st = json!({});
    let (mut text, mut fin) = (String::new(), None);
    for v in serde_json::Deserializer::from_str(body).into_iter::<Value>().map_while(Result::ok) {
        for ev in Antigravity.parse(&v.to_string(), &mut st) {
            match ev {
                AgentEvent::Delta(d) => text.push_str(&d),
                AgentEvent::Final(f) => fin = Some(f),
                _ => {}
            }
        }
    }
    let out = fin.unwrap_or(text);
    (!out.trim().is_empty()).then(|| out.trim().to_string())
}

fn step_update(u: &Value, st: &mut Value) -> Vec<AgentEvent> {
    let mut out = vec![];
    if let Some(id) = u["conversation_id"].as_str().filter(|id| st["session"].as_str() != Some(*id)) {
        st["session"] = json!(id);
        out.push(AgentEvent::Session(id.into()));
    }
    let ty = u["step_type"].as_str().unwrap_or("");
    let idx = u["step_index"].to_string();
    let done = u["state"].as_str().is_some_and(|x| x.eq_ignore_ascii_case("done"));
    let delta = u["text_delta"].as_str().unwrap_or("");
    match ty {
        // the person's own message and the CLI's internal bookkeeping steps aren't the agent's work
        "user_input" | "system_message" | "ephemeral_message" | "checkpoint" | "" => {}
        "agent_response" | "response" | "model_response" => {
            if !delta.is_empty() {
                out.push(AgentEvent::Delta(delta.into()));
            }
        }
        t if t.contains("think") || t.contains("plan") || t.contains("reason") => {
            let acc = format!("{}{}", st["think"][&idx].as_str().unwrap_or(""), delta);
            st["think"][&idx] = json!(acc);
            if done && !acc.trim().is_empty() {
                out.push(AgentEvent::Thinking(acc));
            }
        }
        t => {
            // a tool or action step: show it once when it starts, and its outcome when it's done
            let what = ["command", "command_line", "tool_name", "title", "description", "path", "query", "url"]
                .iter()
                .find_map(|k| u[*k].as_str().filter(|x| !x.is_empty()))
                .map(str::to_string)
                .unwrap_or_else(|| delta.to_string());
            if st["seen"][&idx].is_null() {
                st["seen"][&idx] = json!(true);
                out.push(AgentEvent::ToolUse { id: idx.clone(), name: t.replace('_', " "), input: what });
            }
            if done {
                let result = ["output", "result", "stdout", "error"].iter().find_map(|k| u[*k].as_str()).unwrap_or("").to_string();
                let failed = u["status"].as_str().is_some_and(|x| x.eq_ignore_ascii_case("error") || x.eq_ignore_ascii_case("failed")) || !u["error"].is_null();
                out.push(AgentEvent::ToolResult { id: idx, output: result, error: failed });
            }
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn reads_step_updates_and_result() {
        let a = Antigravity;
        let mut st = json!({});
        let lines = [
            r#"{"event":"step_update","step_update":{"conversation_id":"c1","step_index":0,"state":"DONE","step_type":"user_input"}}"#,
            r#"{"event":"step_update","step_update":{"conversation_id":"c1","step_index":1,"state":"ACTIVE","step_type":"agent_response","text_delta":"Hello! What "}}"#,
            r#"{"event":"step_update","step_update":{"conversation_id":"c1","step_index":1,"state":"ACTIVE","step_type":"agent_response","text_delta":"can I help you with today?"}}"#,
            r#"{"event":"step_update","step_update":{"conversation_id":"c1","step_index":1,"state":"DONE","step_type":"agent_response","text_delta":"\n","usage":{"input_tokens":12809,"output_tokens":44}}}"#,
        ];
        let mut text = String::new();
        let mut sessions = 0;
        for l in lines {
            for e in a.parse(l, &mut st) {
                match e {
                    AgentEvent::Delta(d) => text.push_str(&d),
                    AgentEvent::Session(_) => sessions += 1,
                    AgentEvent::Raw(r) => panic!("raw line leaked: {r}"),
                    _ => {}
                }
            }
        }
        assert_eq!(text, "Hello! What can I help you with today?\n");
        assert_eq!(sessions, 1);
        // the result, broken over two lines by the terminal
        let r1 = r#"{"event":"result","result":{"conversation_id":"c1","status":"SUCCESS","response":"Hello! What can I help you with today?\n","usage":{"input_tokens":12809,"#;
        let r2 = r#""output_tokens":44,"thinking_tokens":30,"cache_read_tokens":0,"total_tokens":12853}}}"#;
        assert!(a.parse(r1, &mut st).is_empty());
        let ev = a.parse(r2, &mut st);
        assert!(ev.iter().any(|e| matches!(e, AgentEvent::Usage { input: 12809, output: 44, .. })));
        assert!(ev.iter().any(|e| matches!(e, AgentEvent::Final(t) if t.starts_with("Hello!"))));
    }

    #[test]
    fn repairs_raw_bodies() {
        let raw = r#"{"event":"step_update","step_update":{"conversation_id":"c","step_index":0,"state":"DONE","step_type":"user_input","text_delta":"hi"}} {"event":"result","result":{"conversation_id":"c","status":"success","response":"pong"}}"#;
        assert_eq!(repair_body(raw).as_deref(), Some("pong"));
        assert_eq!(repair_body("plain text"), None);
    }
    #[test]
    fn shows_tool_steps() {
        let a = Antigravity;
        let mut st = json!({});
        let start = a.parse(r#"{"event":"step_update","step_update":{"conversation_id":"c1","step_index":2,"state":"ACTIVE","step_type":"run_command","command":"ls"}}"#, &mut st);
        assert!(start.iter().any(|e| matches!(e, AgentEvent::ToolUse { name, input, .. } if name == "run command" && input == "ls")));
        let end = a.parse(r#"{"event":"step_update","step_update":{"conversation_id":"c1","step_index":2,"state":"DONE","step_type":"run_command","command":"ls","output":"a.txt"}}"#, &mut st);
        assert!(end.iter().any(|e| matches!(e, AgentEvent::ToolResult { output, .. } if output == "a.txt")));
        assert!(!end.iter().any(|e| matches!(e, AgentEvent::ToolUse { .. })));
    }
}
