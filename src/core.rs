//! The core: storage, the event bus, the agents and everything running right now. One `Core` serves the desktop
//! windows, remote devices and the agents' own tool calls alike.

use crate::{
    agents::{self, AgentAdapter},
    bus::Bus,
    config,
    db::Db,
    procs::Resolved,
    util,
};
use anyhow::{anyhow, Result};
use serde_json::{json, Value};
use std::{
    collections::HashMap,
    path::PathBuf,
    sync::{
        atomic::{AtomicU16, Ordering},
        Arc, Mutex, RwLock,
    },
};
use tokio::sync::{watch, Semaphore};

pub struct Running {
    pub turn_id: String,
    pub cancel: watch::Sender<bool>,
    pub started: i64,
}

pub struct TermEntry {
    pub term: Arc<crate::procs::Term>,
    pub label: String,
    pub agent: String,
    pub buffer: Vec<u8>,
    pub exited: Option<Option<i32>>,
    pub started: i64,
}

pub struct Core {
    pub db: Db,
    pub bus: Bus,
    pub data: PathBuf,
    /// Running as a server with nobody at its screen.
    pub headless: bool,
    pub port: AtomicU16,
    pub local_token: String,
    pub instance_id: String,
    pub agents: RwLock<Vec<Arc<dyn AgentAdapter>>>,
    pub running: Mutex<HashMap<String, Running>>,
    /// Turn tokens handed to agents for the tool server: token -> thread id.
    pub turn_tokens: Mutex<HashMap<String, String>>,
    pub terms: Mutex<HashMap<String, TermEntry>>,
    pub status_cache: Mutex<HashMap<String, Value>>,
    pub slots: Mutex<HashMap<String, Arc<Semaphore>>>,
    pub pairing: Mutex<Option<(String, i64)>>,
    pub pair_failures: Mutex<Vec<i64>>,
    pub exe: PathBuf,
    /// Lets the desktop shell react to things only it can do (open a window, quit).
    pub desktop: Mutex<Option<tokio::sync::mpsc::UnboundedSender<Value>>>,
    pub settings_cache: RwLock<Value>,
    /// The listener other devices reach (and its announcer), while sharing is on: where it listens, and its tasks.
    pub sharing: Mutex<Option<(String, Vec<tokio::task::JoinHandle<()>>)>>,
}

pub type CoreRef = Arc<Core>;

impl Core {
    pub fn open(data: PathBuf, headless: bool) -> Result<CoreRef> {
        std::fs::create_dir_all(&data)?;
        let db = Db::open(&data.join("model-roamer.db"))?;
        let tok_path = data.join("local.token");
        let local_token = match std::fs::read_to_string(&tok_path) {
            Ok(t) if t.trim().len() >= 32 => t.trim().to_string(),
            _ => {
                let t = util::secret();
                crate::os::write_atomic(&tok_path, t.as_bytes())?;
                t
            }
        };
        let instance_id = match db.setting("instance_id") {
            Value::String(s) => s,
            _ => {
                let s = util::id("srv");
                db.set_setting("instance_id", &json!(s))?;
                s
            }
        };
        let mut settings = config::defaults();
        config::merge(&mut settings, &db.setting("config"));
        let core = Arc::new(Core {
            db,
            bus: Bus::new(),
            data,
            headless,
            port: AtomicU16::new(0),
            local_token,
            instance_id,
            agents: RwLock::new(agents::registry(&settings["custom_agents"])),
            running: Mutex::new(HashMap::new()),
            turn_tokens: Mutex::new(HashMap::new()),
            terms: Mutex::new(HashMap::new()),
            status_cache: Mutex::new(HashMap::new()),
            slots: Mutex::new(HashMap::new()),
            pairing: Mutex::new(None),
            pair_failures: Mutex::new(vec![]),
            exe: std::env::current_exe().unwrap_or_else(|_| PathBuf::from("model-roamer")),
            desktop: Mutex::new(None),
            settings_cache: RwLock::new(settings),
            sharing: Mutex::new(None),
        });
        Ok(core)
    }

    // ------------------------------------------------------------------ settings
    pub fn settings(&self) -> Value {
        self.settings_cache.read().map(|s| s.clone()).unwrap_or_else(|_| config::defaults())
    }

    pub fn patch_settings(&self, patch: &Value) -> Result<Value> {
        let mut stored = self.db.setting("config");
        if !stored.is_object() {
            stored = json!({});
        }
        let mut patch = patch.clone();
        // a redacted secret coming back unchanged means "keep it"
        for k in config::SECRET_KEYS {
            if patch.get(*k).and_then(|v| v.as_str()) == Some("••••••••") {
                patch.as_object_mut().map(|o| o.remove(*k));
            }
        }
        config::merge(&mut stored, &patch);
        // lists replace rather than merge
        for k in ["custom_agents", "lead_fallback"] {
            if let Some(v) = patch.get(k) {
                stored[k] = v.clone();
            }
        }
        self.db.set_setting("config", &stored)?;
        let mut full = config::defaults();
        config::merge(&mut full, &stored);
        if let Ok(mut c) = self.settings_cache.write() {
            *c = full.clone();
        }
        if patch.get("custom_agents").is_some() {
            if let Ok(mut a) = self.agents.write() {
                *a = agents::registry(&full["custom_agents"]);
            }
        }
        self.bus.emit("settings", None, None, config::redact(&full));
        Ok(full)
    }

    pub fn agent_cfg(&self, id: &str) -> Value {
        let s = self.settings();
        let mut base = json!({ "enabled": true, "model": "", "effort": "", "autonomy": "full", "max_parallel": 2, "path": "", "extra_args": [] });
        config::merge(&mut base, &s["agents"][id]);
        base
    }

    // ------------------------------------------------------------------ agents
    pub fn adapter(&self, id: &str) -> Option<Arc<dyn AgentAdapter>> {
        self.agents.read().ok()?.iter().find(|a| a.id() == id).cloned()
    }

    pub fn adapters(&self) -> Vec<Arc<dyn AgentAdapter>> {
        self.agents.read().map(|a| a.clone()).unwrap_or_default()
    }

    pub fn locate(&self, id: &str) -> Option<Resolved> {
        let a = self.adapter(id)?;
        let cfg = self.agent_cfg(id);
        agents::locate(a.as_ref(), cfg["path"].as_str())
    }

    /// One agent's install, version and sign-in state (slow: runs the CLI). Cached for the UI.
    pub async fn agent_status(&self, id: &str) -> Value {
        crate::os::refresh_path();
        let Some(a) = self.adapter(id) else { return json!({ "id": id, "error": "unknown agent" }) };
        let info = a.info();
        let cfg = self.agent_cfg(id);
        let limit = self.limit_of(id);
        let mut v = json!({ "id": id, "info": info, "config": cfg, "limit": limit, "checked_at": util::now_ms(), "pty": a.needs_pty() });
        match agents::locate(a.as_ref(), cfg["path"].as_str()) {
            None => {
                v["installed"] = json!(false);
                v["auth"] = json!({ "signed_in": "no", "detail": "Not installed" });
            }
            Some(r) => {
                v["installed"] = json!(true);
                v["path"] = json!(r.shown);
                let (ver, auth) = tokio::join!(agents::version(&r), agents::auth(a.as_ref(), &r));
                v["version"] = json!(ver);
                v["auth"] = json!(auth);
            }
        }
        v["install"] = json!(a.install());
        v["login"] = json!(a.login(self.headless));
        v["logout"] = json!(a.logout());
        if let Ok(mut c) = self.status_cache.lock() {
            c.insert(id.to_string(), v.clone());
        }
        self.bus.emit("agent", None, None, v.clone());
        v
    }

    pub fn cached_status(&self) -> Vec<Value> {
        let cache = self.status_cache.lock().map(|c| c.clone()).unwrap_or_default();
        self.adapters()
            .iter()
            .map(|a| {
                let id = a.id();
                cache.get(&id).cloned().map(|mut v| {
                    v["limit"] = self.limit_of(&id);
                    v["config"] = self.agent_cfg(&id);
                    v
                }).unwrap_or_else(|| json!({ "id": id, "info": a.info(), "config": self.agent_cfg(&id), "limit": self.limit_of(&id), "pending": true }))
            })
            .collect()
    }

    pub fn limit_of(&self, agent: &str) -> Value {
        match self.db.one("SELECT limited_until, note, hit_at FROM limits WHERE agent = ?1", &[&agent]) {
            Ok(Some(r)) => {
                let until = r["limited_until"].as_i64();
                let active = until.map(|u| u > util::now_ms()).unwrap_or(r["hit_at"].as_i64().unwrap_or(0) > util::now_ms() - 30 * 60_000);
                json!({ "active": active, "until": until, "note": r["note"], "hit_at": r["hit_at"] })
            }
            _ => json!({ "active": false }),
        }
    }

    pub fn is_limited(&self, agent: &str) -> bool {
        self.limit_of(agent)["active"].as_bool().unwrap_or(false)
    }

    /// How many turns of this agent may run at once.
    pub fn slot(&self, agent: &str) -> Arc<Semaphore> {
        let n = self.agent_cfg(agent)["max_parallel"].as_u64().unwrap_or(2).clamp(1, 16) as usize;
        let mut m = self.slots.lock().unwrap_or_else(|p| p.into_inner());
        m.entry(agent.to_string()).or_insert_with(|| Arc::new(Semaphore::new(n))).clone()
    }

    // ------------------------------------------------------------------ entities
    pub fn project(&self, id: &str) -> Result<Value> {
        self.db.one("SELECT * FROM projects WHERE id = ?1 AND deleted_at IS NULL", &[&id])?.ok_or_else(|| anyhow!("No project {id}"))
    }

    pub fn thread(&self, id: &str) -> Result<Value> {
        self.db.one("SELECT * FROM threads WHERE id = ?1 AND deleted_at IS NULL", &[&id])?.ok_or_else(|| anyhow!("No thread {id}"))
    }

    pub fn lead_thread(&self, project: &str) -> Result<Value> {
        if let Some(t) = self.db.one("SELECT * FROM threads WHERE project_id = ?1 AND kind = 'lead' AND deleted_at IS NULL", &[&project])? {
            return Ok(t);
        }
        let p = self.project(project)?;
        let id = util::id("th");
        let now = util::now_ms();
        let agent = p["lead_agent"].as_str().unwrap_or("claude").to_string();
        let model = p["lead_model"].as_str().unwrap_or("").to_string();
        self.db.exec(
            "INSERT INTO threads(id, project_id, kind, title, agent, model, status, created_at, updated_at) VALUES (?1, ?2, 'lead', 'Project chat', ?3, ?4, 'idle', ?5, ?5)",
            &[&id, &project, &agent, &model, &now],
        )?;
        self.thread(&id)
    }

    pub fn set_thread(&self, id: &str, fields: &Value) -> Result<Value> {
        let allowed = [
            "title", "agent", "model", "status", "status_note", "task_kind", "instructions", "criteria", "workspace", "branch", "session_id", "session_agent",
            "resolved", "pinned", "unread", "project_id", "parent_id", "base_branch", "depends_on", "merged",
        ];
        let obj = fields.as_object().ok_or_else(|| anyhow!("fields must be an object"))?;
        for (k, v) in obj {
            if !allowed.contains(&k.as_str()) {
                continue;
            }
            let sql = format!("UPDATE threads SET {k} = ?1, updated_at = ?2 WHERE id = ?3");
            let now = util::now_ms();
            match v {
                Value::Number(n) => self.db.exec(&sql, &[&n.as_i64().unwrap_or(0), &now, &id])?,
                Value::Bool(b) => self.db.exec(&sql, &[&(*b as i64), &now, &id])?,
                Value::Null => self.db.exec(&sql, &[&rusqlite::types::Null, &now, &id])?,
                other => self.db.exec(&sql, &[&crate::agents::s(other), &now, &id])?,
            };
        }
        let t = self.thread(id)?;
        self.bus.emit("thread", t["project_id"].as_str(), Some(id), t.clone());
        Ok(t)
    }

    pub fn set_status(&self, id: &str, status: &str, note: &str) -> Result<Value> {
        self.set_thread(id, &json!({ "status": status, "status_note": note }))
    }

    /// Where a project's own files live when it has no folder of its own.
    pub fn project_home(&self, project: &Value) -> PathBuf {
        let folder = project["folder"].as_str().unwrap_or("");
        if !folder.is_empty() {
            return PathBuf::from(folder);
        }
        let d = self.data.join("projects").join(project["id"].as_str().unwrap_or("x")).join("files");
        let _ = std::fs::create_dir_all(&d);
        d
    }

    pub fn artifacts_dir(&self) -> PathBuf {
        self.data.join("artifacts")
    }

    pub fn base_url(&self) -> String {
        format!("http://127.0.0.1:{}", self.port.load(Ordering::Relaxed))
    }

    pub fn tell_desktop(&self, msg: Value) -> bool {
        self.desktop.lock().ok().and_then(|d| d.as_ref().map(|tx| tx.send(msg).is_ok())).unwrap_or(false)
    }

    pub fn display_name(&self) -> String {
        let n = self.settings()["server"]["name"].as_str().unwrap_or("").trim().to_string();
        if n.is_empty() { crate::os::machine_name() } else { n }
    }
}
