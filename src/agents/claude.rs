//! Claude Code (`claude`), headless with `-p --output-format stream-json`, signed in with the Claude subscription.

use super::*;

pub struct Claude;

impl AgentAdapter for Claude {
    fn info(&self) -> Info {
        Info {
            id: "claude".into(),
            name: "Claude Code".into(),
            vendor: "Anthropic".into(),
            hue: 38,
            strengths: vec!["orchestration".into(), "planning".into(), "core code".into(), "refactoring".into(), "careful reasoning".into(), "code review".into(), "writing".into()],
            summary: "Best lead. Plans, breaks work down, writes and refactors core code, reviews others' work carefully.".into(),
            site: "https://claude.com/claude-code".into(),
            models: vec![
                ModelOpt { id: "".into(), label: "CLI default".into(), note: "Whatever Claude Code is set to".into(), ..Default::default() },
                ModelOpt { id: "opus".into(), label: "Opus (latest)".into(), note: "Most capable; the default lead".into(), ..Default::default() },
                ModelOpt { id: "fable".into(), label: "Fable (latest)".into(), note: "Fast and strong".into(), ..Default::default() },
                ModelOpt { id: "sonnet".into(), label: "Sonnet (latest)".into(), note: "Balanced".into(), ..Default::default() },
                ModelOpt { id: "haiku".into(), label: "Haiku (latest)".into(), note: "Quick small tasks".into(), ..Default::default() },
            ],
            mcp: true,
            can_lead: true,
        }
    }

    fn binaries(&self) -> Vec<String> {
        vec!["claude".into()]
    }

    fn launch(&self, spec: &TurnSpec) -> anyhow::Result<Launch> {
        let mut a: Vec<String> = vec!["-p".into(), "--output-format".into(), "stream-json".into(), "--verbose".into(), "--include-partial-messages".into()];
        if let Some(sid) = &spec.session {
            a.push("--resume".into());
            a.push(sid.clone());
        }
        if !spec.model.is_empty() {
            a.push("--model".into());
            a.push(spec.model.clone());
        }
        if !spec.effort.is_empty() {
            a.push("--effort".into());
            a.push(spec.effort.clone());
        }
        a.push("--permission-mode".into());
        a.push(
            match spec.autonomy {
                Autonomy::Full => "bypassPermissions",
                Autonomy::Edits => "acceptEdits",
                Autonomy::ReadOnly => "plan",
            }
            .into(),
        );
        if !spec.system.trim().is_empty() {
            let f = scratch_file(&spec.scratch, "system.md", &spec.system)?;
            a.push("--append-system-prompt-file".into());
            a.push(f);
        }
        if let Some(m) = &spec.mcp {
            let env: serde_json::Map<String, Value> = m.env.iter().map(|(k, v)| (k.clone(), json!(v))).collect();
            let cfg = json!({ "mcpServers": { "roamer": { "type": "stdio", "command": m.command, "args": m.args, "env": env } } });
            let f = scratch_file(&spec.scratch, "mcp.json", &cfg.to_string())?;
            a.push("--mcp-config".into());
            a.push(f);
            a.push("--allowedTools".into());
            a.push("mcp__roamer".into());
        }
        for d in &spec.extra_dirs {
            a.push("--add-dir".into());
            a.push(d.display().to_string());
        }
        a.extend(spec.extra_args.iter().cloned());
        // the prompt goes in on stdin: no quoting trouble, no length cap
        Ok(Launch { args: a, stdin: Some(spec.prompt.clone()), cwd: Some(spec.cwd.clone()), env: vec![("CLAUDE_CODE_ENTRYPOINT".into(), "model-roamer".into()), ("ENABLE_TOOL_SEARCH".into(), "false".into())], pty: false })
    }

    /// Claude Code has no "models" command, but its SDK handshake answers with the model picker's list.
    fn models_probe(&self) -> Option<ModelsProbe> {
        Some(ModelsProbe {
            args: vec!["-p".into(), "--input-format".into(), "stream-json".into(), "--output-format".into(), "stream-json".into(), "--verbose".into()],
            stdin: Some("{\"type\":\"control_request\",\"request_id\":\"models\",\"request\":{\"subtype\":\"initialize\"}}\n".into()),
        })
    }

    fn models_read(&self, out: &str) -> Vec<ModelOpt> {
        out.lines()
            .filter_map(json_line)
            .find(|v| v["type"] == "control_response")
            .map(|v| {
                let r = if v["response"]["response"].is_object() { &v["response"]["response"] } else { &v["response"] };
                parse_model_list(&r["models"].to_string())
            })
            .unwrap_or_default()
    }

    fn parse(&self, line: &str, st: &mut Value) -> Vec<AgentEvent> {
        let Some(v) = json_line(line) else {
            return if line.trim().is_empty() { vec![] } else { vec![AgentEvent::Raw(line.to_string())] };
        };
        let mut out = vec![];
        match v["type"].as_str().unwrap_or("") {
            "system" => {
                if let Some(id) = v["session_id"].as_str() {
                    if st["sid"].as_str() != Some(id) {
                        st["sid"] = json!(id);
                        out.push(AgentEvent::Session(id.into()));
                    }
                }
            }
            "stream_event" => {
                let e = &v["event"];
                if e["type"] == "content_block_delta" && e["delta"]["type"] == "text_delta" && v["parent_tool_use_id"].is_null() {
                    out.push(AgentEvent::Delta(s(&e["delta"]["text"])));
                }
            }
            "assistant" => {
                if !v["parent_tool_use_id"].is_null() {
                    return out; // a subagent's inner messages: the tool result says what came of them
                }
                for c in v["message"]["content"].as_array().cloned().unwrap_or_default() {
                    match c["type"].as_str().unwrap_or("") {
                        "text" => out.push(AgentEvent::Text(s(&c["text"]))),
                        "thinking" => {
                            let t = s(&c["thinking"]);
                            if !t.trim().is_empty() {
                                out.push(AgentEvent::Thinking(t));
                            }
                        }
                        "tool_use" => out.push(AgentEvent::ToolUse { id: s(&c["id"]), name: s(&c["name"]), input: c["input"].to_string() }),
                        _ => {}
                    }
                }
                if let Some(err) = v["error"].as_str() {
                    if err.contains("rate_limit") || err.contains("billing") {
                        out.push(AgentEvent::Limit { until: None, message: err.into() });
                    }
                }
            }
            "user" => {
                for c in v["message"]["content"].as_array().cloned().unwrap_or_default() {
                    if c["type"] == "tool_result" {
                        let body = match &c["content"] {
                            Value::Array(parts) => parts.iter().map(|p| s(&p["text"])).collect::<Vec<_>>().join("\n"),
                            other => s(other),
                        };
                        out.push(AgentEvent::ToolResult { id: s(&c["tool_use_id"]), output: body, error: c["is_error"].as_bool().unwrap_or(false) });
                    }
                }
            }
            "rate_limit_event" => {
                let info = &v["rate_limit_info"];
                let mut q = json!({});
                for (k, name) in [("five_hour", "five_hour"), ("seven_day", "weekly")] {
                    let w = &info["unifiedWindows"][k];
                    if let Some(u) = w["utilization"].as_f64() {
                        q[name] = json!({ "used": u.clamp(0.0, 1.0), "resets_at": w["resetsAt"].as_i64().map(epoch_ms) });
                    }
                }
                if q.as_object().is_some_and(|o| !o.is_empty()) {
                    out.push(AgentEvent::Quota(q));
                }
                if info["status"] == "rejected" {
                    let until = info["resetsAt"].as_i64().map(|t| if t < 10_000_000_000 { t * 1000 } else { t });
                    out.push(AgentEvent::Limit { until, message: format!("Claude {} limit reached", s(&info["rateLimitType"]).replace('_', " ")) });
                }
            }
            "result" => {
                let u = &v["usage"];
                out.push(AgentEvent::Usage {
                    input: u["input_tokens"].as_i64().unwrap_or(0) + u["cache_creation_input_tokens"].as_i64().unwrap_or(0),
                    output: u["output_tokens"].as_i64().unwrap_or(0),
                    cached: u["cache_read_input_tokens"].as_i64().unwrap_or(0),
                    cost: v["total_cost_usd"].as_f64().unwrap_or(0.0),
                });
                if let Some(id) = v["session_id"].as_str() {
                    if st["sid"].as_str() != Some(id) {
                        out.push(AgentEvent::Session(id.into()));
                    }
                }
                let text = s(&v["result"]);
                if v["is_error"].as_bool().unwrap_or(false) || v["subtype"].as_str().is_some_and(|x| x.starts_with("error")) {
                    let msg = if text.is_empty() { s(&v["subtype"]) } else { text };
                    match limit_in(&msg) {
                        Some(until) => out.push(AgentEvent::Limit { until, message: msg }),
                        None => out.push(AgentEvent::Error(msg)),
                    }
                } else {
                    out.push(AgentEvent::Final(text));
                }
            }
            _ => {}
        }
        out
    }

    fn auth_probe(&self) -> Option<Vec<String>> {
        Some(vec!["auth".into(), "status".into()])
    }

    fn auth_read(&self, _ok: bool, out: &str) -> AuthState {
        match json_line(out) {
            Some(v) if v["loggedIn"].as_bool() == Some(true) => AuthState {
                signed_in: "yes".into(),
                detail: match v["authMethod"].as_str().unwrap_or("") {
                    "oauth_token" | "claude.ai" => "Signed in with your Claude subscription".into(),
                    "api_key" => "Using an API key (billed per token)".into(),
                    m => format!("Signed in ({m})"),
                },
            },
            Some(v) if v["loggedIn"].as_bool() == Some(false) => AuthState { signed_in: "no".into(), detail: "Not signed in".into() },
            _ => AuthState { signed_in: "unknown".into(), detail: String::new() },
        }
    }

    fn auth_files(&self) -> Vec<PathBuf> {
        vec![home().join(".claude").join(".credentials.json")]
    }

    fn install(&self) -> Vec<Step> {
        vec![
            shell("Install Claude Code (official installer)", "irm https://claude.ai/install.ps1 | iex", "curl -fsSL https://claude.ai/install.sh | bash", &[]),
            npm_global("Or install with npm", "@anthropic-ai/claude-code"),
        ]
    }

    fn update(&self) -> Vec<Step> {
        vec![step("Update Claude Code", &["claude", "update"], &["claude"])]
    }

    fn login(&self, _headless: bool) -> Step {
        // prints a sign-in link and waits for the code to be pasted, which works from any device
        step("Sign in to Claude", &["claude", "auth", "login", "--claudeai"], &["claude"])
    }

    fn logout(&self) -> Option<Step> {
        Some(step("Sign out of Claude", &["claude", "auth", "logout"], &["claude"]))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn parses_stream() {
        let c = Claude;
        let mut st = json!({});
        let e = c.parse(r#"{"type":"system","subtype":"init","session_id":"abc"}"#, &mut st);
        assert!(matches!(&e[0], AgentEvent::Session(s) if s == "abc"));
        let e = c.parse(r#"{"type":"assistant","message":{"content":[{"type":"text","text":"Hi"},{"type":"tool_use","id":"t1","name":"Bash","input":{"command":"ls"}}]},"parent_tool_use_id":null}"#, &mut st);
        assert_eq!(e.len(), 2);
        let e = c.parse(r#"{"type":"result","subtype":"success","result":"Done","session_id":"abc","total_cost_usd":0.01,"usage":{"input_tokens":5,"output_tokens":7}}"#, &mut st);
        assert!(e.iter().any(|x| matches!(x, AgentEvent::Final(t) if t == "Done")));
        let e = c.parse(r#"{"type":"result","subtype":"success","is_error":true,"result":"Claude AI usage limit reached|1735689600"}"#, &mut st);
        assert!(e.iter().any(|x| matches!(x, AgentEvent::Limit { .. })));
    }
}
