//! Each harness's plan usage (5-hour and weekly windows), read the way its own CLI reads it, so the status bar is
//! right at start-up and after every turn rather than only when a CLI happens to mention it mid-turn.
//!
//! - Claude Code: the same usage endpoint its /usage screen calls, with the sign-in it already saved. The token is
//!   only read, never refreshed here (refreshing would race the CLI's own refresh).
//! - Codex: the rate limits it writes into its session logs after every turn (~/.codex/sessions).
//! - Antigravity doesn't expose its plan usage, so it has turn counts only.

use crate::agents::{codex_quota_at, epoch_ms};
use serde_json::{json, Value};
use std::path::PathBuf;

pub async fn probe(agent: &str) -> Option<Value> {
    match agent {
        "claude" => claude().await,
        "codex" => tokio::task::spawn_blocking(codex).await.ok().flatten(),
        _ => None,
    }
}

fn claude_dir() -> PathBuf {
    std::env::var_os("CLAUDE_CONFIG_DIR").map(PathBuf::from).unwrap_or_else(|| crate::os::home().join(".claude"))
}

async fn claude() -> Option<Value> {
    let creds: Value = serde_json::from_str(&tokio::fs::read_to_string(claude_dir().join(".credentials.json")).await.ok()?).ok()?;
    let o = &creds["claudeAiOauth"];
    let token = o["accessToken"].as_str()?;
    if o["expiresAt"].as_i64().is_some_and(|e| e < crate::util::now_ms()) {
        return None; // the CLI refreshes it on its next run; the turn's own report fills in meanwhile
    }
    let r = crate::github::http()
        .get("https://api.anthropic.com/api/oauth/usage")
        .bearer_auth(token)
        .header("anthropic-beta", "oauth-2025-04-20")
        .header("User-Agent", "model-roamer")
        .timeout(std::time::Duration::from_secs(12))
        .send()
        .await
        .ok()?;
    if !r.status().is_success() {
        tracing::debug!(status = %r.status(), "claude usage read failed");
        return None;
    }
    claude_parse(&r.json::<Value>().await.ok()?)
}

/// {"five_hour": {"utilization": 38.0, "resets_at": "2026-11-23T07:30:00Z"}, "seven_day": {...}}; utilization is a
/// percentage.
pub fn claude_parse(v: &Value) -> Option<Value> {
    let mut q = json!({});
    for (k, name) in [("five_hour", "five_hour"), ("seven_day", "weekly")] {
        let w = &v[k];
        let Some(u) = w["utilization"].as_f64() else { continue };
        let resets = match &w["resets_at"] {
            Value::String(s) => chrono::DateTime::parse_from_rfc3339(s).ok().map(|d| d.timestamp_millis()),
            Value::Number(n) => n.as_i64().map(epoch_ms),
            _ => None,
        };
        q[name] = json!({ "used": (u / 100.0).clamp(0.0, 1.0), "resets_at": resets });
    }
    q.as_object().is_some_and(|o| !o.is_empty()).then_some(q)
}

fn codex() -> Option<Value> {
    let home = std::env::var_os("CODEX_HOME").map(PathBuf::from).unwrap_or_else(|| crate::os::home().join(".codex"));
    // sessions/YYYY/MM/DD/rollout-*.jsonl: the newest few files, newest first
    let mut files = vec![];
    let mut dirs = vec![home.join("sessions")];
    for _ in 0..3 {
        let mut next = vec![];
        for d in &dirs {
            let mut kids: Vec<PathBuf> = std::fs::read_dir(d).ok()?.flatten().map(|e| e.path()).filter(|p| p.is_dir()).collect();
            kids.sort();
            next.extend(kids.into_iter().rev().take(2));
        }
        dirs = next;
    }
    for d in dirs {
        if let Ok(rd) = std::fs::read_dir(&d) {
            files.extend(rd.flatten().map(|e| e.path()).filter(|p| p.extension().is_some_and(|x| x == "jsonl")));
        }
    }
    files.sort_by_key(|p| std::fs::metadata(p).and_then(|m| m.modified()).ok());
    for f in files.iter().rev().take(4) {
        if let Some(q) = codex_file(&std::fs::read_to_string(f).ok()?) {
            return Some(q);
        }
    }
    None
}

/// The last rate-limit report in one Codex session log.
pub fn codex_file(text: &str) -> Option<Value> {
    for line in text.lines().rev() {
        if !line.contains("rate_limits") {
            continue;
        }
        let Ok(v) = serde_json::from_str::<Value>(line) else { continue };
        let at = v["timestamp"].as_str().and_then(|t| chrono::DateTime::parse_from_rfc3339(t).ok()).map(|d| d.timestamp_millis()).unwrap_or_else(crate::util::now_ms);
        if let Some(q) = codex_quota_at(&v, at) {
            return Some(q);
        }
    }
    None
}

/// How full a Codex session's context is, and how often it compacted: read from its session log. Returns
/// {"used", "window", "compactions"}.
pub fn codex_context(session: &str) -> Option<Value> {
    if session.len() < 8 {
        return None;
    }
    let home = std::env::var_os("CODEX_HOME").map(PathBuf::from).unwrap_or_else(|| crate::os::home().join(".codex"));
    // newest day folders first: sessions/YYYY/MM/DD
    let mut days = vec![home.join("sessions")];
    for _ in 0..3 {
        let mut next = vec![];
        for d in &days {
            let mut kids: Vec<PathBuf> = std::fs::read_dir(d).ok()?.flatten().map(|e| e.path()).filter(|p| p.is_dir()).collect();
            kids.sort();
            next.extend(kids.into_iter().rev().take(14));
        }
        days = next;
    }
    for d in days {
        for e in std::fs::read_dir(&d).ok()?.flatten() {
            let p = e.path();
            if p.file_name().is_some_and(|n| n.to_string_lossy().contains(session)) {
                return codex_context_file(&std::fs::read_to_string(&p).ok()?);
            }
        }
    }
    None
}

pub fn codex_context_file(text: &str) -> Option<Value> {
    let (mut used, mut window, mut compactions) = (0i64, 0i64, 0i64);
    for line in text.lines() {
        if line.contains("compacted") {
            if let Ok(v) = serde_json::from_str::<Value>(line) {
                if v["type"] == "compacted" || v["payload"]["type"] == "compacted" || v["payload"]["type"] == "context_compacted" {
                    compactions += 1;
                }
            }
        }
        if line.contains("token_count") && line.contains("model_context_window") {
            if let Ok(v) = serde_json::from_str::<Value>(line) {
                let info = &v["payload"]["info"];
                if let Some(u) = info["last_token_usage"]["total_tokens"].as_i64() {
                    used = u;
                }
                if let Some(w) = info["model_context_window"].as_i64() {
                    window = w;
                }
            }
        }
    }
    (window > 0).then(|| json!({ "used": used, "window": window, "compactions": compactions }))
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn reads_codex_context() {
        let log = concat!(
            r#"{"type":"event_msg","payload":{"type":"token_count","info":{"last_token_usage":{"total_tokens":1000},"model_context_window":258000}}}"#, "\n",
            r#"{"type":"compacted","payload":{}}"#, "\n",
            r#"{"type":"event_msg","payload":{"type":"token_count","info":{"last_token_usage":{"total_tokens":42000},"model_context_window":258000}}}"#, "\n"
        );
        let c = codex_context_file(log).unwrap();
        assert_eq!((c["used"].as_i64(), c["window"].as_i64(), c["compactions"].as_i64()), (Some(42000), Some(258000), Some(1)));
    }
    #[test]
    fn reads_usage() {
        let q = claude_parse(&json!({ "five_hour": { "utilization": 38.0, "resets_at": "2026-11-23T07:30:00+00:00" }, "seven_day": { "utilization": 8, "resets_at": null } })).unwrap();
        assert!((q["five_hour"]["used"].as_f64().unwrap() - 0.38).abs() < 1e-9);
        assert!(q["five_hour"]["resets_at"].as_i64().is_some());
        assert!((q["weekly"]["used"].as_f64().unwrap() - 0.08).abs() < 1e-9);
        let log = r#"{"timestamp":"2026-11-23T05:00:00Z","type":"event_msg","payload":{"type":"token_count","info":null,"rate_limits":{"primary":{"used_percent":12.0,"window_minutes":300,"resets_in_seconds":3600},"secondary":{"used_percent":40.0,"window_minutes":10080,"resets_in_seconds":86400}}}}"#;
        let c = codex_file(&format!("{{}}\n{log}\n")).unwrap();
        assert!((c["five_hour"]["used"].as_f64().unwrap() - 0.12).abs() < 1e-9);
        assert!((c["weekly"]["used"].as_f64().unwrap() - 0.40).abs() < 1e-9);
        assert_eq!(c["five_hour"]["resets_at"].as_i64().unwrap(), chrono::DateTime::parse_from_rfc3339("2026-11-23T06:00:00Z").unwrap().timestamp_millis());
    }
}
