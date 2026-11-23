//! Memory: per-project and global notes every agent reads and writes through the same interface, that the person
//! can edit and search. One entry per subject; writing a name that exists updates it.

use crate::{core::Core, util};
use anyhow::{bail, Result};
use serde_json::{json, Value};

pub fn list(core: &Core, project: Option<&str>) -> Result<Vec<Value>> {
    core.db.all(
        "SELECT id, project_id, name, description, length(body) AS size, updated_by, updated_at FROM memory
         WHERE deleted_at IS NULL AND (project_id IS NULL OR project_id = ?1) ORDER BY project_id IS NULL, name",
        &[&project.unwrap_or("")],
    )
}

pub fn search(core: &Core, project: Option<&str>, q: &str) -> Result<Vec<Value>> {
    // quote each word so punctuation in the query can't break FTS syntax
    let terms: Vec<String> = q.split_whitespace().map(|w| format!("\"{}\"*", w.replace('"', ""))).collect();
    if terms.is_empty() {
        return list(core, project);
    }
    let fts = terms.join(" ");
    let rows = core.db.all(
        "SELECT m.id, m.project_id, m.name, m.description, snippet(memory_fts, 2, '[', ']', '…', 14) AS snippet, m.updated_at FROM memory_fts
         JOIN memory m ON m.rowid = memory_fts.rowid WHERE memory_fts MATCH ?1 AND m.deleted_at IS NULL AND (m.project_id IS NULL OR m.project_id = ?2)
         ORDER BY rank LIMIT 30",
        &[&fts, &project.unwrap_or("")],
    );
    match rows {
        Ok(r) => Ok(r),
        Err(_) => {
            let like = format!("%{q}%");
            core.db.all(
                "SELECT id, project_id, name, description, substr(body, 1, 160) AS snippet, updated_at FROM memory WHERE deleted_at IS NULL
                 AND (project_id IS NULL OR project_id = ?2) AND (name LIKE ?1 OR description LIKE ?1 OR body LIKE ?1) LIMIT 30",
                &[&like, &project.unwrap_or("")],
            )
        }
    }
}

pub fn get_by_name(core: &Core, project: Option<&str>, name: &str) -> Result<Option<Value>> {
    // a project entry wins over a global one with the same name
    core.db.one(
        "SELECT * FROM memory WHERE deleted_at IS NULL AND name = ?1 AND (project_id IS NULL OR project_id = ?2) ORDER BY project_id IS NULL LIMIT 1",
        &[&name, &project.unwrap_or("")],
    )
}

/// The tool's view: the index, one entry, or search results.
pub fn read(core: &Core, project: Option<&str>, name: &str, query: &str) -> Result<String> {
    if !name.is_empty() {
        return Ok(match get_by_name(core, project, name)? {
            Some(m) => format!("# {}\n{}\n\n{}", m["name"].as_str().unwrap_or(""), m["description"].as_str().unwrap_or(""), m["body"].as_str().unwrap_or("")),
            None => format!("No memory named '{name}'. Call read_memory with no arguments for the index."),
        });
    }
    if !query.is_empty() {
        let r = search(core, project, query)?;
        if r.is_empty() {
            return Ok("Nothing matched.".into());
        }
        return Ok(r.iter().map(|m| format!("- {} ({}): {}", m["name"].as_str().unwrap_or(""), scope(m), m["snippet"].as_str().unwrap_or(""))).collect::<Vec<_>>().join("\n"));
    }
    Ok(index(core, project))
}

fn scope(m: &Value) -> &'static str {
    if m["project_id"].is_null() { "global" } else { "project" }
}

/// Entries named in the index every turn; the rest are found with `read_memory` (which can search).
const INDEX_MAX: usize = 30;

pub fn index(core: &Core, project: Option<&str>) -> String {
    let l = list(core, project).unwrap_or_default();
    if l.is_empty() {
        return "Memory is empty.".into();
    }
    let mut out: Vec<String> = l
        .iter()
        .take(INDEX_MAX)
        .map(|m| format!("- {} ({}): {}", m["name"].as_str().unwrap_or(""), scope(m), util::clip(m["description"].as_str().unwrap_or(""), 110)))
        .collect();
    if l.len() > INDEX_MAX {
        out.push(format!("- ...and {} more entries: call `read_memory` with a search to find them.", l.len() - INDEX_MAX));
    }
    out.join("\n")
}

pub fn write(core: &Core, project: Option<&str>, name: &str, description: &str, body: &str, by: &str) -> Result<Value> {
    let name = util::slug(name, 64);
    if body.trim().is_empty() {
        bail!("A memory entry needs a body.");
    }
    if body.len() > 64_000 {
        bail!("That entry is too long (keep each under 64 KB; split it by subject).");
    }
    let now = util::now_ms();
    let existing = core.db.one(
        "SELECT id FROM memory WHERE name = ?1 AND IFNULL(project_id, '') = ?2",
        &[&name, &project.unwrap_or("")],
    )?;
    let id = match existing {
        Some(r) => {
            let id = r["id"].as_str().unwrap_or("").to_string();
            let desc_keep = if description.is_empty() { None } else { Some(description) };
            core.db.exec(
                "UPDATE memory SET body = ?1, description = COALESCE(?2, description), updated_by = ?3, updated_at = ?4, deleted_at = NULL WHERE id = ?5",
                &[&body, &desc_keep, &by, &now, &id],
            )?;
            id
        }
        None => {
            let id = util::id("mem");
            core.db.exec(
                "INSERT INTO memory(id, project_id, name, description, body, updated_by, updated_at) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7)",
                &[&id, &project, &name, &description, &body, &by, &now],
            )?;
            id
        }
    };
    let m = core.db.one("SELECT * FROM memory WHERE id = ?1", &[&id])?.unwrap_or(json!({}));
    core.bus.emit("memory", project, None, m.clone());
    Ok(m)
}

pub fn delete_by_name(core: &Core, project: Option<&str>, name: &str) -> Result<()> {
    let now = util::now_ms();
    let n = core.db.exec("UPDATE memory SET deleted_at = ?1 WHERE name = ?2 AND IFNULL(project_id, '') = ?3 AND deleted_at IS NULL", &[&now, &name, &project.unwrap_or("")])?;
    if n == 0 {
        bail!("No memory named '{name}'.");
    }
    core.bus.emit("memory", project, None, json!({ "deleted": name }));
    Ok(())
}
