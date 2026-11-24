//! The MCP bridge. Agents start `model-roamer mcp` as a stdio MCP server; it forwards each JSON-RPC message to the
//! running Model Roamer (`/api/mcp`, authorized with the turn's token), which answers with the tools for that thread.
//! The protocol is handled in one place, the server; this side only carries lines.

use crate::{core::CoreRef, tools};
use serde_json::{json, Value};
use std::io::{BufRead, Write};

/// `model-roamer mcp`: read JSON-RPC lines on stdin, forward them, write answers on stdout.
pub fn run_stdio() -> i32 {
    let url = std::env::var("MR_MCP_URL").unwrap_or_default();
    let token = std::env::var("MR_MCP_TOKEN").unwrap_or_default();
    let rt = match tokio::runtime::Builder::new_current_thread().enable_all().build() {
        Ok(r) => r,
        Err(e) => {
            eprintln!("model-roamer mcp: {e}");
            return 1;
        }
    };
    let client = reqwest::Client::builder().timeout(std::time::Duration::from_secs(900)).build().unwrap_or_default();
    let stdin = std::io::stdin();
    let mut out = std::io::stdout();
    for line in stdin.lock().lines() {
        let Ok(line) = line else { break };
        if line.trim().is_empty() {
            continue;
        }
        let req: Value = match serde_json::from_str(&line) {
            Ok(v) => v,
            Err(e) => {
                let _ = writeln!(out, "{}", json!({ "jsonrpc": "2.0", "id": null, "error": { "code": -32700, "message": format!("parse error: {e}") } }));
                let _ = out.flush();
                continue;
            }
        };
        let id = req.get("id").cloned();
        let resp: Option<Value> = if url.is_empty() || token.is_empty() {
            id.map(|id| json!({ "jsonrpc": "2.0", "id": id, "error": { "code": -32000, "message": "Model Roamer's tool bridge was started without MR_MCP_URL / MR_MCP_TOKEN, so it can't reach the app." } }))
        } else {
            let r = rt.block_on(async {
                let mut last = String::new();
                for attempt in 0..3 {
                    match client.post(&url).bearer_auth(&token).json(&req).send().await {
                        Ok(r) if r.status().as_u16() == 202 || r.status().as_u16() == 204 => return Ok(None),
                        Ok(r) if r.status().is_success() => return r.json::<Value>().await.map(Some).map_err(|e| e.to_string()),
                        Ok(r) => {
                            let st = r.status();
                            let body = r.text().await.unwrap_or_default();
                            return Err(format!("Model Roamer answered {st}: {body}"));
                        }
                        Err(e) => {
                            last = e.to_string();
                            std::thread::sleep(std::time::Duration::from_millis(400 * (attempt + 1)));
                        }
                    }
                }
                Err(format!("Can't reach Model Roamer: {last}"))
            });
            match r {
                Ok(v) => v,
                Err(e) => id.map(|id| json!({ "jsonrpc": "2.0", "id": id, "error": { "code": -32000, "message": e } })),
            }
        };
        if let Some(r) = resp {
            let _ = writeln!(out, "{r}");
            let _ = out.flush();
        }
    }
    0
}

/// Answer one MCP JSON-RPC message for the thread that owns the token. None: a notification (no reply).
pub async fn handle(core: &CoreRef, thread_id: &str, req: &Value) -> Option<Value> {
    let id = req.get("id").cloned()?;
    let method = req["method"].as_str().unwrap_or("");
    let ok = |result: Value| json!({ "jsonrpc": "2.0", "id": id.clone(), "result": result });
    let thread = match core.thread(thread_id) {
        Ok(t) => t,
        Err(e) => return Some(json!({ "jsonrpc": "2.0", "id": id, "error": { "code": -32000, "message": e.to_string() } })),
    };
    Some(match method {
        "initialize" => ok(json!({
            "protocolVersion": req["params"]["protocolVersion"].as_str().unwrap_or("2025-06-18"),
            "capabilities": { "tools": { "listChanged": false } },
            "serverInfo": { "name": "roamer", "title": "Model Roamer", "version": env!("CARGO_PKG_VERSION") },
            "instructions": "Model Roamer's tools: shared memory, threads, messages, artifacts, git and GitHub for this project. Your instructions explain when to use each."
        })),
        "ping" => ok(json!({})),
        "tools/list" => {
            let bridged = crate::collab_bridge::bind_row(core, &thread).ok().flatten().is_some();
            ok(json!({ "tools": tools::list(tools::role_of(&thread), bridged) }))
        }
        "tools/call" => {
            let name = req["params"]["name"].as_str().unwrap_or("");
            let args = req["params"]["arguments"].clone();
            let args = if args.is_null() { json!({}) } else { args };
            match tools::call(core, &thread, name, &args).await {
                Ok(text) => ok(json!({ "content": [{ "type": "text", "text": text }], "isError": false })),
                Err(e) => ok(json!({ "content": [{ "type": "text", "text": format!("Error: {e}") }], "isError": true })),
            }
        }
        "resources/list" => ok(json!({ "resources": [] })),
        "prompts/list" => ok(json!({ "prompts": [] })),
        _ => json!({ "jsonrpc": "2.0", "id": id, "error": { "code": -32601, "message": format!("Unknown method {method}") } }),
    })
}
