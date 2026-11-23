//! Codex CLI (`codex exec --json`), signed in with ChatGPT.

use super::*;

pub struct Codex;

/// A TOML basic string, for `-c key=value` overrides.
pub fn toml_str(s: &str) -> String {
    let mut o = String::from("\"");
    for c in s.chars() {
        match c {
            '\\' => o.push_str("\\\\"),
            '"' => o.push_str("\\\""),
            '\n' => o.push_str("\\n"),
            '\r' => o.push_str("\\r"),
            '\t' => o.push_str("\\t"),
            c if (c as u32) < 0x20 => o.push_str(&format!("\\u{:04x}", c as u32)),
            c => o.push(c),
        }
    }
    o.push('"');
    o
}

impl AgentAdapter for Codex {
    fn info(&self) -> Info {
        Info {
            id: "codex".into(),
            name: "Codex".into(),
            vendor: "OpenAI (ChatGPT)".into(),
            hue: 175,
            strengths: vec!["back end".into(), "APIs".into(), "data layer".into(), "infrastructure".into(), "scripts".into(), "tests".into(), "debugging".into()],
            summary: "Strong, fast engineer for back-end services, APIs, data, infrastructure, scripts and tests.".into(),
            site: "https://developers.openai.com/codex/cli".into(),
            models: vec![
                // `codex debug models` fills in the real list; until then, whatever Codex is set to
                ModelOpt { id: "".into(), label: "CLI default".into(), note: "Whatever Codex is set to".into(), ..Default::default() },
            ],
            mcp: true,
            can_lead: true,
        }
    }

    fn binaries(&self) -> Vec<String> {
        vec!["codex".into()]
    }

    fn launch(&self, spec: &TurnSpec) -> anyhow::Result<Launch> {
        let mut a: Vec<String> = vec!["exec".into(), "--json".into(), "--skip-git-repo-check".into(), "--color".into(), "never".into()];
        match spec.autonomy {
            Autonomy::Full => a.push("--dangerously-bypass-approvals-and-sandbox".into()),
            Autonomy::Edits => a.extend(["--sandbox".into(), "workspace-write".into()]),
            Autonomy::ReadOnly => a.extend(["--sandbox".into(), "read-only".into()]),
        }
        a.extend(["-c".into(), "approval_policy=\"never\"".into()]);
        if !spec.model.is_empty() {
            a.extend(["-m".into(), spec.model.clone()]);
        }
        if !spec.effort.is_empty() {
            a.extend(["-c".into(), format!("model_reasoning_effort={}", toml_str(&spec.effort))]);
        }
        a.extend(["-C".into(), spec.cwd.display().to_string()]);
        for d in &spec.extra_dirs {
            a.extend(["--add-dir".into(), d.display().to_string()]);
        }
        if let Some(m) = &spec.mcp {
            a.extend(["-c".into(), format!("mcp_servers.roamer.command={}", toml_str(&m.command))]);
            let args: Vec<String> = m.args.iter().map(|x| toml_str(x)).collect();
            a.extend(["-c".into(), format!("mcp_servers.roamer.args=[{}]", args.join(", "))]);
            let env: Vec<String> = m.env.iter().map(|(k, v)| format!("{k} = {}", toml_str(v))).collect();
            a.extend(["-c".into(), format!("mcp_servers.roamer.env={{ {} }}", env.join(", "))]);
            a.extend(["-c".into(), "mcp_servers.roamer.tool_timeout_sec=600".into()]);
        }
        a.extend(spec.extra_args.iter().cloned());
        if let Some(sid) = &spec.session {
            a.extend(["resume".into(), sid.clone()]);
        }
        a.push("-".into()); // read the prompt from stdin
        Ok(Launch { args: a, stdin: Some(with_system(spec)), cwd: Some(spec.cwd.clone()), env: vec![], pty: false })
    }

    fn models_probe(&self) -> Option<ModelsProbe> {
        Some(ModelsProbe { args: vec!["debug".into(), "models".into()], stdin: None })
    }

    fn parse(&self, line: &str, st: &mut Value) -> Vec<AgentEvent> {
        let Some(v) = json_line(line) else {
            return if line.trim().is_empty() { vec![] } else { vec![AgentEvent::Raw(line.to_string())] };
        };
        let mut out = vec![];
        let ty = v["type"].as_str().unwrap_or("");
        match ty {
            "thread.started" | "session.created" | "session_configured" => {
                let id = v["thread_id"].as_str().or(v["session_id"].as_str()).unwrap_or("");
                if !id.is_empty() {
                    out.push(AgentEvent::Session(id.into()));
                }
            }
            "item.started" | "item.updated" | "item.completed" => {
                let it = &v["item"];
                let id = s(&it["id"]);
                let done = ty == "item.completed";
                match it["type"].as_str().or(it["item_type"].as_str()).unwrap_or("") {
                    "agent_message" | "assistant_message" if done => out.push(AgentEvent::Text(s(&it["text"]))),
                    // partial text, when this Codex sends it: pass on only what is new since the last update
                    "agent_message" | "assistant_message" => {
                        let text = s(&it["text"]);
                        let seen = st["typed"][&id].as_u64().unwrap_or(0) as usize;
                        if text.len() > seen && text.is_char_boundary(seen) {
                            out.push(AgentEvent::Delta(text[seen..].to_string()));
                            st["typed"][&id] = json!(text.len());
                        }
                    }
                    "reasoning" if done => out.push(AgentEvent::Thinking(s(&it["text"]))),
                    "command_execution" => {
                        if ty == "item.started" {
                            out.push(AgentEvent::ToolUse { id: id.clone(), name: "shell".into(), input: s(&it["command"]) });
                        } else if done {
                            if st["seen"][&id].is_null() {
                                out.push(AgentEvent::ToolUse { id: id.clone(), name: "shell".into(), input: s(&it["command"]) });
                            }
                            let code = it["exit_code"].as_i64().unwrap_or(0);
                            out.push(AgentEvent::ToolResult { id: id.clone(), output: s(&it["aggregated_output"]), error: code != 0 });
                        }
                        st["seen"][&id] = json!(true);
                    }
                    "file_change" if done => {
                        let files: Vec<String> = it["changes"].as_array().cloned().unwrap_or_default().iter().map(|c| format!("{} {}", s(&c["kind"]), s(&c["path"]))).collect();
                        out.push(AgentEvent::ToolUse { id: id.clone(), name: "edit files".into(), input: files.join("\n") });
                        out.push(AgentEvent::ToolResult { id, output: s(&it["status"]), error: it["status"] == "failed" });
                    }
                    "mcp_tool_call" => {
                        if ty == "item.started" || (done && st["seen"][&id].is_null()) {
                            out.push(AgentEvent::ToolUse { id: id.clone(), name: format!("{}:{}", s(&it["server"]), s(&it["tool"])), input: it["arguments"].to_string() });
                        }
                        if done {
                            let res = if it["result"].is_null() { s(&it["error"]) } else { it["result"].to_string() };
                            out.push(AgentEvent::ToolResult { id: id.clone(), output: res, error: !it["error"].is_null() });
                        }
                        st["seen"][&id] = json!(true);
                    }
                    "web_search" if done => out.push(AgentEvent::ToolUse { id, name: "web search".into(), input: s(&it["query"]) }),
                    "todo_list" if done => {
                        let items: Vec<String> = it["items"].as_array().cloned().unwrap_or_default().iter().map(|t| format!("[{}] {}", if t["completed"].as_bool() == Some(true) { "x" } else { " " }, s(&t["text"]))).collect();
                        out.push(AgentEvent::Thinking(items.join("\n")));
                    }
                    "error" if done => out.push(AgentEvent::Error(s(&it["message"]))),
                    _ => {}
                }
            }
            "turn.completed" => {
                let u = &v["usage"];
                out.push(AgentEvent::Usage {
                    input: u["input_tokens"].as_i64().unwrap_or(0),
                    output: u["output_tokens"].as_i64().unwrap_or(0),
                    cached: u["cached_input_tokens"].as_i64().unwrap_or(0),
                    cost: 0.0,
                });
            }
            "turn.failed" | "error" => {
                let msg = if v["error"].is_object() { s(&v["error"]["message"]) } else { s(&v["message"]) };
                let msg = if msg.is_empty() { v.to_string() } else { msg };
                match limit_in(&msg) {
                    Some(until) => out.push(AgentEvent::Limit { until, message: msg }),
                    None if msg.contains("Reconnecting") || msg.contains("retrying") => out.push(AgentEvent::Raw(msg)),
                    None => out.push(AgentEvent::Error(msg)),
                }
            }
            _ => {}
        }
        out
    }

    fn auth_probe(&self) -> Option<Vec<String>> {
        Some(vec!["login".into(), "status".into()])
    }

    fn auth_read(&self, ok: bool, out: &str) -> AuthState {
        let t = out.to_lowercase();
        if t.contains("not logged in") {
            AuthState { signed_in: "no".into(), detail: "Not signed in".into() }
        } else if ok && t.contains("logged in") {
            let how = if t.contains("chatgpt") { "Signed in with ChatGPT" } else if t.contains("api key") { "Using an API key (billed per token)" } else { "Signed in" };
            AuthState { signed_in: "yes".into(), detail: how.into() }
        } else {
            AuthState { signed_in: "unknown".into(), detail: String::new() }
        }
    }

    fn auth_files(&self) -> Vec<PathBuf> {
        vec![home().join(".codex").join("auth.json")]
    }

    fn install(&self) -> Vec<Step> {
        let mut v = vec![npm_global("Install Codex with npm", "@openai/codex")];
        if cfg!(target_os = "macos") {
            v.push(step("Or install with Homebrew", &["brew", "install", "codex"], &["brew"]));
        }
        if cfg!(windows) {
            v.push(step("Or install with winget", &["winget", "install", "-e", "--id", "OpenAI.Codex", "--accept-source-agreements", "--accept-package-agreements"], &["winget"]));
        }
        v
    }

    fn update(&self) -> Vec<Step> {
        vec![npm_global("Update Codex", "@openai/codex@latest")]
    }

    fn login(&self, headless: bool) -> Step {
        if headless {
            // the device-code flow: the page opens on whatever device you're using, not on the server
            step("Sign in to ChatGPT", &["codex", "login", "--device-auth"], &["codex"])
        } else {
            step("Sign in to ChatGPT", &["codex", "login"], &["codex"])
        }
    }

    fn logout(&self) -> Option<Step> {
        Some(step("Sign out of ChatGPT", &["codex", "logout"], &["codex"]))
    }
}

#[cfg(test)]
mod tests {
    #[test]
    fn streams_message_updates() {
        let c = Codex;
        let mut st = serde_json::json!({});
        let a = c.parse(r#"{"type":"item.updated","item":{"id":"m1","type":"agent_message","text":"Hel"}}"#, &mut st);
        let b = c.parse(r#"{"type":"item.updated","item":{"id":"m1","type":"agent_message","text":"Hello there"}}"#, &mut st);
        let d = c.parse(r#"{"type":"item.completed","item":{"id":"m1","type":"agent_message","text":"Hello there"}}"#, &mut st);
        assert!(matches!(&a[0], AgentEvent::Delta(t) if t == "Hel"));
        assert!(matches!(&b[0], AgentEvent::Delta(t) if t == "lo there"));
        assert!(matches!(&d[0], AgentEvent::Text(t) if t == "Hello there"));
    }

    use super::*;
    #[test]
    fn parses_jsonl() {
        let c = Codex;
        let mut st = json!({});
        let e = c.parse(r#"{"type":"thread.started","thread_id":"t-1"}"#, &mut st);
        assert!(matches!(&e[0], AgentEvent::Session(s) if s == "t-1"));
        let e = c.parse(r#"{"type":"item.completed","item":{"id":"i1","type":"agent_message","text":"All set"}}"#, &mut st);
        assert!(matches!(&e[0], AgentEvent::Text(t) if t == "All set"));
        let e = c.parse(r#"{"type":"item.started","item":{"id":"i2","type":"command_execution","command":"ls","status":"in_progress"}}"#, &mut st);
        assert_eq!(e.len(), 1);
        let e = c.parse(r#"{"type":"item.completed","item":{"id":"i2","type":"command_execution","command":"ls","aggregated_output":"a\n","exit_code":0}}"#, &mut st);
        assert_eq!(e.len(), 1);
        let e = c.parse(r#"{"type":"turn.failed","error":{"message":"You've hit your usage limit. Try again in 3 hours."}}"#, &mut st);
        assert!(matches!(&e[0], AgentEvent::Limit { until: Some(_), .. }));
        assert_eq!(toml_str("a\"b\\c\nd"), "\"a\\\"b\\\\c\\nd\"");
    }
}
