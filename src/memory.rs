//! Scoped memory with one entry per subject. Shared rows have no user_id.
use crate::{core::Core, util};
use anyhow::{bail, Context, Result};
use serde_json::{json, Value};

pub const LOCAL_USER: &str = "local";
const SCOPE: &str = "CASE WHEN user_id IS NOT NULL THEN 'collaborator' WHEN project_id IS NULL THEN 'global' ELSE 'project' END";
const VISIBLE: &str =
    "((user_id IS NULL AND ((?2 AND project_id = ?1) OR (?3 AND project_id IS NULL)))
    OR (user_id = ?4 AND project_id IS ?5))";

#[derive(Clone, Copy, PartialEq, Eq)]
pub enum Scope {
    Collaborator,
    Project,
    Global,
}

impl Scope {
    pub fn parse(s: &str) -> Result<Option<Self>> {
        Ok(Some(match s {
            "" => return Ok(None),
            "collaborator" => Self::Collaborator,
            "project" => Self::Project,
            "global" => Self::Global,
            _ => bail!("Memory scope must be collaborator, project or global."),
        }))
    }
}

#[derive(Clone, Copy)]
pub struct View<'a> {
    pub project: Option<&'a str>,
    pub shared: bool,
    pub global: bool,
    pub user: Option<&'a str>,
    pub personal_project: Option<&'a str>,
}

impl<'a> View<'a> {
    pub fn local(project: Option<&'a str>) -> Self {
        Self {
            project,
            shared: true,
            global: true,
            user: Some(LOCAL_USER),
            personal_project: None,
        }
    }

    pub fn only(mut self, scope: Option<Scope>) -> Self {
        if let Some(scope) = scope {
            self.shared &= scope == Scope::Project;
            self.global &= scope == Scope::Global;
            self.user = self.user.filter(|_| scope == Scope::Collaborator);
        }
        self
    }

    pub fn list(self, core: &Core, deleted: bool) -> Result<Vec<Value>> {
        let (condition, order) = if deleted {
            ("IS NOT NULL", "deleted_at DESC LIMIT 50")
        } else {
            ("IS NULL", "user_id IS NULL, project_id IS NULL, name")
        };
        core.db.all(
            &format!("SELECT id, project_id, {SCOPE} AS scope, name, description, length(body) AS size, updated_by, updated_at, deleted_at
                      FROM memory WHERE deleted_at {condition} AND {VISIBLE} ORDER BY {order}"),
            &[&self.project, &self.shared, &self.global, &self.user, &self.personal_project],
        )
    }

    pub fn search(self, core: &Core, q: &str) -> Result<Vec<Value>> {
        let terms: Vec<_> = q
            .split_whitespace()
            .map(|w| w.replace('"', ""))
            .filter(|w| !w.is_empty())
            .map(|w| format!("\"{w}\"*"))
            .collect();
        if terms.is_empty() {
            return self.list(core, false);
        }
        core.db.all(
            &format!(
                "SELECT m.id, m.project_id, {SCOPE} AS scope, m.name, m.description,
                             snippet(memory_fts, 2, '[', ']', '…', 14) AS snippet, m.updated_at
                      FROM memory_fts JOIN memory m ON m.rowid = memory_fts.rowid
                      WHERE memory_fts MATCH ?6 AND deleted_at IS NULL AND {VISIBLE}
                      ORDER BY rank, m.name LIMIT 30"
            ),
            &[
                &self.project,
                &self.shared,
                &self.global,
                &self.user,
                &self.personal_project,
                &terms.join(" "),
            ],
        )
    }

    pub fn get(self, core: &Core, name: &str) -> Result<Option<Value>> {
        core.db.one(
            &format!("SELECT id, project_id, {SCOPE} AS scope, name, description, body, updated_by, updated_at FROM memory
                      WHERE deleted_at IS NULL AND {VISIBLE} AND name = ?6
                      ORDER BY user_id IS NULL, project_id IS NULL LIMIT 1"),
            &[&self.project, &self.shared, &self.global, &self.user, &self.personal_project, &name],
        )
    }

    pub fn rows(self, core: &Core, name: &str, query: &str) -> Result<Vec<Value>> {
        if !name.is_empty() {
            return Ok(self.get(core, name)?.into_iter().collect());
        }
        if !query.is_empty() {
            return self.search(core, query);
        }
        self.list(core, false)
    }

    pub fn changes(self, core: &Core, since: i64) -> Result<Vec<Value>> {
        core.db.all(
            &format!("SELECT id, project_id, {SCOPE} AS scope, name, description, length(body) AS size, updated_by, updated_at, deleted_at
                      FROM memory WHERE {VISIBLE} AND (updated_at > ?6 OR deleted_at > ?6)"),
            &[&self.project, &self.shared, &self.global, &self.user, &self.personal_project, &since],
        )
    }
}

pub fn render(rows: &[Value], name: &str, query: &str) -> String {
    if !name.is_empty() {
        return match rows.first() {
            Some(m) => format!(
                "# {} ({})\n{}\n\n{}",
                field(m, "name"),
                field(m, "scope"),
                field(m, "description"),
                field(m, "body")
            ),
            None => format!(
                "No memory named '{name}'. Call read_memory with no arguments for the index."
            ),
        };
    }
    if rows.is_empty() {
        return if query.is_empty() {
            "Memory is empty."
        } else {
            "Nothing matched."
        }
        .into();
    }
    rows.iter()
        .map(|m| {
            let text = if query.is_empty() {
                field(m, "description")
            } else {
                field(m, "snippet")
            };
            format!(
                "- {} ({}): {}",
                field(m, "name"),
                field(m, "scope"),
                util::clip(text, 300)
            )
        })
        .collect::<Vec<_>>()
        .join("\n")
}

fn field<'a>(row: &'a Value, key: &str) -> &'a str {
    row[key].as_str().unwrap_or("")
}

pub fn index(core: &Core, project: Option<&str>) -> String {
    let rows = View::local(project).list(core, false).unwrap_or_default();
    index_rows(&rows)
}

pub fn index_rows(rows: &[Value]) -> String {
    const MAX: usize = 100;
    if rows.is_empty() {
        return "Memory is empty.".into();
    }
    let mut names: Vec<_> = rows
        .iter()
        .take(MAX)
        .map(|m| format!("`{}` ({})", field(m, "name"), field(m, "scope")))
        .collect();
    if rows.len() > MAX {
        names.push(format!("...and {} more", rows.len() - MAX));
    }
    format!("Entries: {}.\nCall `read_memory` with a `name` for an entry, a `query` to search, or no arguments for the descriptions. Use `scope` to select a specific scope.", names.join(", "))
}

pub fn write(
    core: &Core,
    project: Option<&str>,
    user: Option<&str>,
    name: &str,
    description: &str,
    body: &str,
    by: &str,
) -> Result<Value> {
    let name = util::slug(name, 64);
    if body.trim().is_empty() {
        bail!("A memory entry needs a body.");
    }
    if body.len() > 64_000 {
        bail!("That entry is too long (keep each under 64 KB; split it by subject).");
    }
    let m = core.db.one(
        &format!("INSERT INTO memory(id, project_id, user_id, name, description, body, updated_by, updated_at)
                  VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8)
                  ON CONFLICT(IFNULL(project_id, ''), IFNULL(user_id, ''), name) DO UPDATE SET
                      body = excluded.body, description = COALESCE(NULLIF(excluded.description, ''), memory.description),
                      updated_by = excluded.updated_by, updated_at = excluded.updated_at, deleted_at = NULL
                  RETURNING *, {SCOPE} AS scope"),
        &[&util::id("mem"), &project, &user, &name, &description, &body, &by, &util::now_ms()],
    )?.context("The memory write did not return an entry.")?;
    notify(core, project, user, json!({ "id": m["id"] }));
    Ok(m)
}

pub fn notify(core: &Core, project: Option<&str>, user: Option<&str>, payload: Value) {
    // The local event bus is for the owner and their paired devices.
    if user.is_none() || user == Some(LOCAL_USER) {
        core.bus.emit("memory", project, None, payload);
    }
}

pub fn local_entry(core: &Core, id: &str) -> Result<Option<Value>> {
    core.db.one(
        &format!("SELECT *, {SCOPE} AS scope FROM memory WHERE id = ?1 AND (user_id IS NULL OR user_id = ?2)"),
        &[&id, &LOCAL_USER],
    )
}

pub fn delete_by_name(
    core: &Core,
    project: Option<&str>,
    user: Option<&str>,
    name: &str,
) -> Result<()> {
    let n = core.db.exec("UPDATE memory SET deleted_at = ?1 WHERE name = ?2 AND project_id IS ?3 AND user_id IS ?4 AND deleted_at IS NULL", &[&util::now_ms(), &name, &project, &user])?;
    if n == 0 {
        bail!("No memory named '{name}'.");
    }
    notify(core, project, user, json!({ "deleted": name }));
    Ok(())
}
