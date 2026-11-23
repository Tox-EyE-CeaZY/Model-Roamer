//! Gemini-style stream-json: the event format Google's CLIs print. Antigravity's output looks much the same, and a
//! custom agent can choose it. (Gemini CLI itself isn't a built-in agent; Antigravity covers the same models.)

use super::*;

/// Parse one Gemini-style stream-json event.
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

/// One line of Gemini-style output: an event, a usage-limit notice, or plain text.
pub fn parse_line(line: &str, st: &mut Value) -> Vec<AgentEvent> {
    match json_line(line) {
        Some(v) if v["type"].is_string() => parse_gemini(&v, st),
        _ if line.trim().is_empty() => vec![],
        _ => match limit_in(line) {
            Some(until) if line.len() < 600 => vec![AgentEvent::Limit { until, message: line.trim().to_string() }],
            _ => vec![AgentEvent::Raw(line.to_string())],
        },
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn parses_stream() {
        let mut st = json!({});
        let e = parse_line(r#"{"type":"init","session_id":"s1","model":"gemini-3-pro"}"#, &mut st);
        assert!(matches!(&e[0], AgentEvent::Session(s) if s == "s1"));
        let e = parse_line(r#"{"type":"message","role":"assistant","content":"Hel","delta":true}"#, &mut st);
        assert!(matches!(&e[0], AgentEvent::Delta(t) if t == "Hel"));
        let e = parse_line(r#"{"type":"result","status":"error","error":{"message":"Quota exceeded for quota metric"},"stats":{}}"#, &mut st);
        assert!(e.iter().any(|x| matches!(x, AgentEvent::Limit { .. })));
    }
}
