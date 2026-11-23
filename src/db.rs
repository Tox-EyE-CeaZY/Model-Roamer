//! SQLite storage: projects, threads, messages, activity, memory, artifacts, usage, devices, settings.
//! Migrations run in order from day one (PRAGMA user_version). Rows come back as JSON objects so the API layer and
//! the tools share one shape.

use anyhow::{Context, Result};
use rusqlite::{params_from_iter, types::ValueRef, Connection, ToSql};
use serde_json::{json, Map, Value};
use std::{
    path::Path,
    sync::{Arc, Mutex},
};

const MIGRATIONS: &[&str] = &[
    // 1: everything the first version needs
    r#"
    CREATE TABLE settings (key TEXT PRIMARY KEY, value TEXT NOT NULL);

    CREATE TABLE projects (
        id TEXT PRIMARY KEY,
        name TEXT NOT NULL,
        instructions TEXT NOT NULL DEFAULT '',
        lead_agent TEXT NOT NULL DEFAULT 'claude',
        lead_model TEXT NOT NULL DEFAULT '',
        folder TEXT NOT NULL DEFAULT '',
        repo_url TEXT NOT NULL DEFAULT '',
        base_branch TEXT NOT NULL DEFAULT '',
        color INTEGER NOT NULL DEFAULT 128,
        routing TEXT NOT NULL DEFAULT '{}',
        created_at INTEGER NOT NULL,
        updated_at INTEGER NOT NULL,
        deleted_at INTEGER
    );

    CREATE TABLE threads (
        id TEXT PRIMARY KEY,
        project_id TEXT,
        parent_id TEXT,
        kind TEXT NOT NULL,                 -- lead | worker | chat | review
        title TEXT NOT NULL,
        agent TEXT NOT NULL,
        model TEXT NOT NULL DEFAULT '',
        status TEXT NOT NULL DEFAULT 'idle',-- idle | queued | working | waiting | blocked | review | done | failed | paused | cancelled | limited
        status_note TEXT NOT NULL DEFAULT '',
        task_kind TEXT NOT NULL DEFAULT '',
        instructions TEXT NOT NULL DEFAULT '',
        criteria TEXT NOT NULL DEFAULT '',
        workspace TEXT NOT NULL DEFAULT '',
        branch TEXT NOT NULL DEFAULT '',
        base_branch TEXT NOT NULL DEFAULT '',
        depends_on TEXT NOT NULL DEFAULT '', -- comma-separated thread ids that must finish first
        merged INTEGER NOT NULL DEFAULT 0,
        session_id TEXT NOT NULL DEFAULT '',
        session_agent TEXT NOT NULL DEFAULT '',
        resolved INTEGER NOT NULL DEFAULT 0,
        pinned INTEGER NOT NULL DEFAULT 0,
        unread INTEGER NOT NULL DEFAULT 0,
        created_at INTEGER NOT NULL,
        updated_at INTEGER NOT NULL,
        deleted_at INTEGER
    );
    CREATE INDEX threads_project ON threads(project_id, updated_at);

    CREATE TABLE messages (
        id TEXT PRIMARY KEY,
        thread_id TEXT NOT NULL,
        role TEXT NOT NULL,                 -- user | agent | system | wake
        author TEXT NOT NULL DEFAULT '',
        body TEXT NOT NULL,
        meta TEXT NOT NULL DEFAULT '{}',
        pending INTEGER NOT NULL DEFAULT 0, -- 1 until an agent turn has read it
        created_at INTEGER NOT NULL
    );
    CREATE INDEX messages_thread ON messages(thread_id, created_at);

    CREATE TABLE turns (
        id TEXT PRIMARY KEY,
        thread_id TEXT NOT NULL,
        project_id TEXT,
        agent TEXT NOT NULL,
        model TEXT NOT NULL DEFAULT '',
        status TEXT NOT NULL,               -- running | done | failed | cancelled | limited
        error TEXT NOT NULL DEFAULT '',
        inputs TEXT NOT NULL DEFAULT '',    -- comma-separated message ids this turn answered (for retry)
        input_tokens INTEGER NOT NULL DEFAULT 0,
        output_tokens INTEGER NOT NULL DEFAULT 0,
        cached_tokens INTEGER NOT NULL DEFAULT 0,
        cost_usd REAL NOT NULL DEFAULT 0,
        started_at INTEGER NOT NULL,
        ended_at INTEGER
    );
    CREATE INDEX turns_thread ON turns(thread_id, started_at);
    CREATE INDEX turns_time ON turns(started_at);

    CREATE TABLE activity (
        id INTEGER PRIMARY KEY AUTOINCREMENT,
        thread_id TEXT NOT NULL,
        turn_id TEXT NOT NULL,
        kind TEXT NOT NULL,                 -- tool | result | thinking | text | error | note
        title TEXT NOT NULL DEFAULT '',
        body TEXT NOT NULL DEFAULT '',
        created_at INTEGER NOT NULL
    );
    CREATE INDEX activity_thread ON activity(thread_id, id);

    CREATE TABLE memory (
        id TEXT PRIMARY KEY,
        project_id TEXT,                    -- NULL: global memory, shared by every project
        name TEXT NOT NULL,
        description TEXT NOT NULL DEFAULT '',
        body TEXT NOT NULL DEFAULT '',
        updated_by TEXT NOT NULL DEFAULT '',
        updated_at INTEGER NOT NULL,
        deleted_at INTEGER
    );
    CREATE UNIQUE INDEX memory_name ON memory(IFNULL(project_id, ''), name);
    CREATE VIRTUAL TABLE memory_fts USING fts5(name, description, body, content='memory', content_rowid='rowid');
    CREATE TRIGGER memory_ai AFTER INSERT ON memory BEGIN
        INSERT INTO memory_fts(rowid, name, description, body) VALUES (new.rowid, new.name, new.description, new.body);
    END;
    CREATE TRIGGER memory_ad AFTER DELETE ON memory BEGIN
        INSERT INTO memory_fts(memory_fts, rowid, name, description, body) VALUES ('delete', old.rowid, old.name, old.description, old.body);
    END;
    CREATE TRIGGER memory_au AFTER UPDATE ON memory BEGIN
        INSERT INTO memory_fts(memory_fts, rowid, name, description, body) VALUES ('delete', old.rowid, old.name, old.description, old.body);
        INSERT INTO memory_fts(rowid, name, description, body) VALUES (new.rowid, new.name, new.description, new.body);
    END;

    CREATE TABLE artifacts (
        id TEXT PRIMARY KEY,
        project_id TEXT,
        thread_id TEXT,
        title TEXT NOT NULL,
        kind TEXT NOT NULL,                 -- html | markdown | image | svg | text | pdf | file
        entry TEXT NOT NULL,                -- file name inside the artifact's folder
        description TEXT NOT NULL DEFAULT '',
        version INTEGER NOT NULL DEFAULT 1,
        created_at INTEGER NOT NULL,
        updated_at INTEGER NOT NULL,
        deleted_at INTEGER
    );

    CREATE TABLE decisions (
        id TEXT PRIMARY KEY,
        thread_id TEXT NOT NULL,
        project_id TEXT,
        question TEXT NOT NULL,
        options TEXT NOT NULL,              -- JSON [{label, consequence}]
        recommended INTEGER NOT NULL DEFAULT 0,
        answer INTEGER,
        answered_at INTEGER,
        created_at INTEGER NOT NULL
    );

    CREATE TABLE claims (
        id TEXT PRIMARY KEY,
        project_id TEXT NOT NULL,
        thread_id TEXT NOT NULL,
        path TEXT NOT NULL,
        note TEXT NOT NULL DEFAULT '',
        created_at INTEGER NOT NULL
    );

    CREATE TABLE limits (
        agent TEXT PRIMARY KEY,
        limited_until INTEGER,
        note TEXT NOT NULL DEFAULT '',
        hit_at INTEGER NOT NULL
    );

    CREATE TABLE devices (
        id TEXT PRIMARY KEY,
        name TEXT NOT NULL,
        token_hash TEXT NOT NULL,
        created_at INTEGER NOT NULL,
        last_seen INTEGER,
        last_ip TEXT NOT NULL DEFAULT '',
        revoked INTEGER NOT NULL DEFAULT 0
    );

    CREATE TABLE connections (
        id TEXT PRIMARY KEY,
        name TEXT NOT NULL,
        url TEXT NOT NULL,
        token TEXT NOT NULL DEFAULT '',
        via TEXT NOT NULL DEFAULT '',       -- lan | tailscale | wireguard | domain | host
        created_at INTEGER NOT NULL,
        last_used INTEGER
    );
    "#,
    // who named a thread: auto (a default), agent (set_title) or person (renamed in the app, which agents must keep)
    "ALTER TABLE threads ADD COLUMN title_by TEXT NOT NULL DEFAULT 'auto';",
    // artifact folders: nested, filed automatically by project and thread (auto_key) or made by the person or an agent
    r#"
    CREATE TABLE folders (
        id TEXT PRIMARY KEY,
        parent_id TEXT,
        project_id TEXT,
        name TEXT NOT NULL,
        auto_key TEXT,                      -- project:<id> | thread:<id> | chats, for the folders the app files into
        named_by TEXT NOT NULL DEFAULT 'auto',
        created_at INTEGER NOT NULL,
        updated_at INTEGER NOT NULL,
        deleted_at INTEGER
    );
    CREATE INDEX folders_parent ON folders(parent_id);
    ALTER TABLE artifacts ADD COLUMN folder_id TEXT;
    "#,
    // which usage window ran out (five_hour | weekly), and decisions Model Roamer acts on itself when answered
    "ALTER TABLE limits ADD COLUMN window TEXT NOT NULL DEFAULT ''; ALTER TABLE decisions ADD COLUMN action TEXT;",
    // how this thread shows its steps: '' follows the setting, or collapsed | expanded | hidden
    "ALTER TABLE threads ADD COLUMN steps TEXT NOT NULL DEFAULT '';",
    // how full the agent's context is and when it compacted: {used, window, compactions, compacted_at, trigger, at}
    "ALTER TABLE threads ADD COLUMN context TEXT;",
    // a thread run by buddies (two agents taking turns): {id, name, side, turns, started, sessions: {a, b}}
    "ALTER TABLE threads ADD COLUMN buddy TEXT;",
    "ALTER TABLE threads ADD COLUMN parked_at INTEGER;",
];

#[derive(Clone)]
pub struct Db {
    conn: Arc<Mutex<Connection>>,
}

pub type P<'a> = &'a [&'a dyn ToSql];

impl Db {
    pub fn open(path: &Path) -> Result<Db> {
        let conn = Connection::open(path).with_context(|| format!("opening {}", path.display()))?;
        conn.busy_timeout(std::time::Duration::from_secs(8))?;
        conn.execute_batch("PRAGMA journal_mode=WAL; PRAGMA synchronous=NORMAL; PRAGMA foreign_keys=ON;")?;
        let db = Db { conn: Arc::new(Mutex::new(conn)) };
        db.migrate()?;
        Ok(db)
    }

    #[cfg(test)]
    pub fn memory() -> Result<Db> {
        let conn = Connection::open_in_memory()?;
        let db = Db { conn: Arc::new(Mutex::new(conn)) };
        db.migrate()?;
        Ok(db)
    }

    fn lock(&self) -> std::sync::MutexGuard<'_, Connection> {
        // a panic while holding the lock must not take the whole app down with it
        self.conn.lock().unwrap_or_else(|p| p.into_inner())
    }

    fn migrate(&self) -> Result<()> {
        let mut c = self.lock();
        let have: usize = c.query_row("PRAGMA user_version", [], |r| r.get::<_, i64>(0))? as usize;
        for (i, sql) in MIGRATIONS.iter().enumerate().skip(have) {
            let tx = c.transaction()?;
            tx.execute_batch(sql).with_context(|| format!("migration {}", i + 1))?;
            tx.execute_batch(&format!("PRAGMA user_version = {}", i + 1))?;
            tx.commit()?;
            tracing::info!("database migrated to version {}", i + 1);
        }
        Ok(())
    }

    pub fn exec(&self, sql: &str, p: P) -> Result<usize> {
        let c = self.lock();
        Ok(c.execute(sql, params_from_iter(p.iter()))?)
    }

    pub fn all(&self, sql: &str, p: P) -> Result<Vec<Value>> {
        let c = self.lock();
        let mut st = c.prepare_cached(sql)?;
        let names: Vec<String> = st.column_names().iter().map(|s| s.to_string()).collect();
        let mut rows = st.query(params_from_iter(p.iter()))?;
        let mut out = Vec::new();
        while let Some(r) = rows.next()? {
            let mut m = Map::new();
            for (i, n) in names.iter().enumerate() {
                let v = match r.get_ref(i)? {
                    ValueRef::Null => Value::Null,
                    ValueRef::Integer(x) => json!(x),
                    ValueRef::Real(x) => json!(x),
                    ValueRef::Text(t) => {
                        let s = String::from_utf8_lossy(t).into_owned();
                        // JSON columns come back parsed
                        if (n == "meta" || n == "options" || n == "routing" || n == "context" || n == "buddy") && (s.starts_with('{') || s.starts_with('[')) {
                            serde_json::from_str(&s).unwrap_or(Value::String(s))
                        } else {
                            Value::String(s)
                        }
                    }
                    ValueRef::Blob(b) => json!(b.len()),
                };
                m.insert(n.clone(), v);
            }
            out.push(Value::Object(m));
        }
        Ok(out)
    }

    pub fn one(&self, sql: &str, p: P) -> Result<Option<Value>> {
        Ok(self.all(sql, p)?.into_iter().next())
    }

    pub fn scalar_i64(&self, sql: &str, p: P) -> Result<i64> {
        let c = self.lock();
        Ok(c.query_row(sql, params_from_iter(p.iter()), |r| r.get::<_, Option<i64>>(0))?.unwrap_or(0))
    }

    // ---------------------------------------------------------------- settings (JSON values)
    pub fn setting(&self, key: &str) -> Value {
        self.one("SELECT value FROM settings WHERE key = ?1", &[&key])
            .ok()
            .flatten()
            .and_then(|r| r["value"].as_str().and_then(|s| serde_json::from_str(s).ok()))
            .unwrap_or(Value::Null)
    }

    pub fn set_setting(&self, key: &str, v: &Value) -> Result<()> {
        let s = v.to_string();
        self.exec("INSERT INTO settings(key, value) VALUES (?1, ?2) ON CONFLICT(key) DO UPDATE SET value = excluded.value", &[&key, &s])?;
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn migrates_and_round_trips() {
        let db = Db::memory().unwrap();
        db.set_setting("x", &json!({"a": 1})).unwrap();
        assert_eq!(db.setting("x")["a"], 1);
        let now = 1i64;
        db.exec("INSERT INTO memory(id, project_id, name, body, updated_at) VALUES ('m1', NULL, 'design', 'chamfered corners and lime', ?1)", &[&now]).unwrap();
        let hits = db.all("SELECT m.name FROM memory_fts f JOIN memory m ON m.rowid = f.rowid WHERE memory_fts MATCH 'chamfered'", &[]).unwrap();
        assert_eq!(hits.len(), 1);
    }
}
