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
        // agents' steps (thinking, tools, commands) in chats: collapsed | expanded | hidden
        "steps": "collapsed",
        // parked threads (kept idle by the lead for follow-ups) close themselves after this many hours unused; 0 = never
        "idle_close_hours": 24,
        // automatic events (a thread replied, finished, hit a limit) wait this many seconds so the lead reads several in
        // one turn instead of paying for a full turn each; 0 = wake at once. The person's own messages never wait.
        "wake_batch_secs": 45,

        // windows
        "popout_threads": false,
        "restore_windows": true,
        "close_to_tray": true,

        // agents: per-agent config; any missing key falls back to these
        "agents": {
            "claude": { "enabled": true, "model": "opus", "effort": "", "autonomy": "full", "max_parallel": 3, "path": "", "extra_args": [] },
            "codex": { "enabled": true, "model": "", "effort": "", "autonomy": "full", "max_parallel": 3, "path": "", "extra_args": [] },
            "antigravity": { "enabled": true, "model": "", "effort": "", "autonomy": "full", "max_parallel": 2, "path": "", "extra_args": [] }
        },
        "custom_agents": [],
        // buddies: two agents that take turns on one thread, each with a role. An agent id with the prefix buddy:
        // picks one anywhere an agent can be picked.
        "buddies": [
            {
                "id": "builder-reviewer", "name": "Builder + reviewer", "max_turns": 4, "start": "a",
                "instructions": "One of you builds, the other checks the work and sends it back until it is right. Keep each hand-over short and specific.",
                "a": { "agent": "claude", "model": "", "role": "Build it: plan, write the code and run it." },
                "b": { "agent": "codex", "model": "", "role": "Review it: read the changes, run the tests, find bugs and gaps, and say exactly what to fix. Say you are done when it is right." }
            },
            {
                "id": "designer-builder", "name": "Designer + builder", "max_turns": 4, "start": "a",
                "instructions": "One of you designs, the other builds it. The builder tells the designer what doesn't fit; the designer adjusts until it looks and works right.",
                "a": { "agent": "antigravity", "model": "", "role": "Design it: the look, layout, copy and any images, with clear specifics for the builder." },
                "b": { "agent": "claude", "model": "", "role": "Build it faithfully from the design, run it, and send back anything that can't work as designed." }
            }
        ],
        "routing": crate::agents::default_routing(),
        "default_lead": "claude",
        "default_lead_model": "opus",
        "lead_fallback": ["claude", "codex", "antigravity"],
        "auto_fallback": false, // move work off a used-up agent without asking: only when the person turns it on
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
/// Gemini CLI stopped being a built-in agent (Antigravity runs the same Gemini models). Saved settings, project
/// routing and open threads that named it move to Antigravity, once.
pub fn retire_gemini(db: &crate::db::Db) {
    if db.setting("migrated.gemini") == json!(true) {
        return;
    }
    let swap = |v: &mut Value| {
        if let Some(o) = v.as_object_mut() {
            for x in o.values_mut() {
                if x == "gemini" {
                    *x = json!("antigravity");
                }
            }
        }
    };
    let mut cfg = db.setting("config");
    if cfg.is_object() {
        // get_mut, not cfg["x"]: indexing a missing key would add it as null and wipe out its default
        if let Some(o) = cfg.get_mut("agents").and_then(Value::as_object_mut) {
            o.remove("gemini");
        }
        if let Some(r) = cfg.get_mut("routing") {
            swap(r);
        }
        if cfg["default_lead"] == "gemini" {
            cfg["default_lead"] = json!("antigravity");
            cfg["default_lead_model"] = json!("");
        }
        if let Some(list) = cfg.get_mut("lead_fallback").and_then(Value::as_array_mut) {
            list.retain(|x| x != "gemini");
        }
        let _ = db.set_setting("config", &cfg);
    }
    for p in db.all("SELECT id, routing FROM projects", &[]).unwrap_or_default() {
        let mut r = match &p["routing"] {
            Value::String(s) => serde_json::from_str(s).unwrap_or(json!({})),
            v => v.clone(),
        };
        if r.as_object().is_some_and(|o| o.values().any(|x| x == "gemini")) {
            swap(&mut r);
            let _ = db.exec("UPDATE projects SET routing = ?1 WHERE id = ?2", &[&r.to_string(), &p["id"].as_str().unwrap_or("")]);
        }
    }
    // their CLI sessions can't carry over, so the next turn starts fresh with the thread's history
    let _ = db.exec("UPDATE threads SET agent = 'antigravity', model = '', session_id = NULL WHERE agent = 'gemini' AND deleted_at IS NULL", &[]);
    let _ = db.set_setting("migrated.gemini", &json!(true));
}

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
    fn retires_gemini() {
        let db = crate::db::Db::memory().unwrap();
        db.set_setting("config", &json!({ "routing": { "design": "gemini", "core": "claude" }, "default_lead": "gemini", "lead_fallback": ["gemini", "codex"], "agents": { "gemini": {} } })).unwrap();
        retire_gemini(&db);
        let c = db.setting("config");
        assert_eq!(c["routing"]["design"], "antigravity");
        assert_eq!(c["routing"]["core"], "claude");
        assert_eq!(c["default_lead"], "antigravity");
        assert_eq!(c["lead_fallback"], json!(["codex"]));
        assert!(c["agents"].get("gemini").is_none());
        // keys it didn't have stay missing, so their defaults still apply
        db.set_setting("config", &json!({ "theme": "paper" })).unwrap();
        db.set_setting("migrated.gemini", &json!(false)).unwrap();
        retire_gemini(&db);
        assert_eq!(db.setting("config"), json!({ "theme": "paper" }));
        assert_eq!(defaults()["routing"]["frontend"], "antigravity");
    }
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
