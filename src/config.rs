//! Settings, stored in the database as one JSON document and merged over defaults (new settings get sensible
//! defaults without a migration; unknown ones are kept).

use serde_json::{json, Value};

pub fn defaults() -> Value {
    json!({
        "theme": "night",
        "hue": 128,
        "topo": true,
        "grain": true,
        "motion": true,
        "density": "cozy",

        // windows
        "popout_threads": false,
        "restore_windows": true,
        "close_to_tray": true,

        // agents: per-agent config; any missing key falls back to these
        "agents": {
            "claude": { "enabled": true, "model": "opus", "effort": "", "autonomy": "full", "max_parallel": 3, "path": "", "extra_args": [] },
            "codex": { "enabled": true, "model": "", "effort": "", "autonomy": "full", "max_parallel": 3, "path": "", "extra_args": [] },
            "gemini": { "enabled": true, "model": "", "effort": "", "autonomy": "full", "max_parallel": 3, "path": "", "extra_args": [] },
            "antigravity": { "enabled": true, "model": "", "effort": "", "autonomy": "full", "max_parallel": 2, "path": "", "extra_args": [] }
        },
        "custom_agents": [],
        "routing": crate::agents::default_routing(),
        "default_lead": "claude",
        "default_lead_model": "opus",
        "lead_fallback": ["claude", "gemini", "codex", "antigravity"],
        "auto_fallback": true,
        "cross_review": true,

        // turns
        "idle_timeout_min": 25,
        "max_turn_min": 180,
        "max_tool_rounds": 8,

        // git and GitHub
        "github_token": "",
        "git_name": "",
        "git_email": "",
        "worktrees": true,
        "auto_push": false,

        // this machine as a server for other devices
        "server": {
            "share": false,
            "bind": "0.0.0.0",
            "port": 4630,
            "name": "",
            "public_url": "",
            "tls_cert": "",
            "tls_key": "",
            "discoverable": true
        },

        "updates": { "check": true }
    })
}

/// Merge `over` onto `base` (objects merge key by key; everything else replaces).
pub fn merge(base: &mut Value, over: &Value) {
    match (base, over) {
        (Value::Object(b), Value::Object(o)) => {
            for (k, v) in o {
                match b.get_mut(k) {
                    Some(bv) if bv.is_object() && v.is_object() => merge(bv, v),
                    _ => {
                        b.insert(k.clone(), v.clone());
                    }
                }
            }
        }
        // nothing stored yet (a fresh install) leaves the defaults as they are
        (_, Value::Null) => {}
        (b, o) => *b = o.clone(),
    }
}

/// Settings that are never sent to a client (they're shown as "set" / "not set" instead).
pub const SECRET_KEYS: &[&str] = &["github_token"];

pub fn redact(v: &Value) -> Value {
    let mut v = v.clone();
    for k in SECRET_KEYS {
        if let Some(s) = v.get(*k).and_then(|x| x.as_str()) {
            let set = !s.is_empty();
            v[*k] = json!(if set { "••••••••" } else { "" });
        }
    }
    v
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn merges() {
        let mut d = defaults();
        merge(&mut d, &json!({"agents": {"claude": {"model": "sonnet"}}, "theme": "paper"}));
        assert_eq!(d["agents"]["claude"]["model"], "sonnet");
        assert_eq!(d["agents"]["claude"]["max_parallel"], 3);
        assert_eq!(d["theme"], "paper");
        assert_eq!(redact(&json!({"github_token": "x"}))["github_token"], "••••••••");
        let mut fresh = defaults();
        merge(&mut fresh, &Value::Null);
        assert_eq!(fresh["theme"], "night");
    }
}
