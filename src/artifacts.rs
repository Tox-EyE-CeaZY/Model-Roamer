//! Artifacts: pages, mockups, charts, documents and images an agent makes for the person to look at. They are copied
//! into the app's data folder and served by the same server as the UI, so they show up on whatever device is
//! connected, including a laptop talking to a server in a closet. They're served sandboxed (no access to the app).

use crate::{core::Core, util};
use anyhow::{anyhow, bail, Result};
use serde_json::{json, Value};
use std::path::{Path, PathBuf};

const MAX_BYTES: u64 = 60 * 1024 * 1024;

pub fn kind_of(path: &str) -> &'static str {
    match path.rsplit('.').next().unwrap_or("").to_ascii_lowercase().as_str() {
        "html" | "htm" => "html",
        "md" | "markdown" => "markdown",
        "svg" => "svg",
        "png" | "jpg" | "jpeg" | "gif" | "webp" | "avif" | "bmp" => "image",
        "pdf" => "pdf",
        "mp4" | "webm" | "mov" => "video",
        _ => "text",
    }
}

fn ext_for(kind: &str) -> &'static str {
    match kind {
        "html" => "html",
        "markdown" => "md",
        "svg" => "svg",
        _ => "txt",
    }
}

fn copy_dir(from: &Path, to: &Path, budget: &mut u64) -> Result<()> {
    std::fs::create_dir_all(to)?;
    for e in std::fs::read_dir(from)? {
        let e = e?;
        let name = e.file_name();
        let n = name.to_string_lossy();
        if n == ".git" || n == "node_modules" || n == "target" {
            continue;
        }
        let ft = e.file_type()?;
        if ft.is_dir() {
            copy_dir(&e.path(), &to.join(&name), budget)?;
        } else if ft.is_file() {
            let len = e.metadata()?.len();
            if len > *budget {
                bail!("That artifact is over {} MB.", MAX_BYTES / 1024 / 1024);
            }
            *budget -= len;
            std::fs::copy(e.path(), to.join(&name))?;
        }
    }
    Ok(())
}

/// Publish (or update, by title within the project) an artifact from a file/folder in the workspace or from content.
#[allow(clippy::too_many_arguments)]
pub fn publish(core: &Core, project: Option<&str>, thread: Option<&str>, title: &str, description: &str, kind: &str, path: Option<&str>, content: Option<&str>, workspace: Option<&Path>) -> Result<Value> {
    let title = title.trim();
    if title.is_empty() {
        bail!("Give the artifact a title.");
    }
    let existing = core.db.one("SELECT * FROM artifacts WHERE title = ?1 AND IFNULL(project_id, '') = ?2 AND deleted_at IS NULL", &[&title, &project.unwrap_or("")])?;
    let id = existing.as_ref().and_then(|e| e["id"].as_str().map(str::to_string)).unwrap_or_else(|| util::id("art"));
    let dir = core.artifacts_dir().join(&id);
    let staging = core.artifacts_dir().join(format!("{id}.new"));
    let _ = std::fs::remove_dir_all(&staging);
    std::fs::create_dir_all(&staging)?;
    let (entry, kind) = if let Some(p) = path.filter(|p| !p.trim().is_empty()) {
        let mut src = PathBuf::from(p.trim());
        if src.is_relative() {
            if let Some(ws) = workspace {
                src = ws.join(src);
            }
        }
        if !src.exists() {
            bail!("{} doesn't exist.", src.display());
        }
        let mut budget = MAX_BYTES;
        if src.is_dir() {
            copy_dir(&src, &staging, &mut budget)?;
            let entry = ["index.html", "index.htm", "README.md"].iter().find(|n| staging.join(n).exists()).map(|s| s.to_string()).ok_or_else(|| anyhow!("That folder has no index.html."))?;
            let k = kind_of(&entry);
            (entry, k.to_string())
        } else {
            if src.metadata()?.len() > MAX_BYTES {
                bail!("That file is over {} MB.", MAX_BYTES / 1024 / 1024);
            }
            let name = src.file_name().map(|n| n.to_string_lossy().to_string()).unwrap_or_else(|| "file".into());
            std::fs::copy(&src, staging.join(&name))?;
            // an HTML page usually pulls in files next to it (css, js, images): bring siblings along, within reason
            if kind_of(&name) == "html" {
                if let Some(parent) = src.parent() {
                    for e in std::fs::read_dir(parent)?.flatten() {
                        let p = e.path();
                        let n = e.file_name();
                        if p.is_file() && p != src && e.metadata().map(|m| m.len() < 8 * 1024 * 1024).unwrap_or(false) {
                            let ext = p.extension().and_then(|x| x.to_str()).unwrap_or("").to_ascii_lowercase();
                            if ["css", "js", "mjs", "json", "png", "jpg", "jpeg", "svg", "gif", "webp", "woff", "woff2", "ttf"].contains(&ext.as_str()) {
                                let _ = std::fs::copy(&p, staging.join(n));
                            }
                        } else if p.is_dir() && ["assets", "css", "js", "img", "images", "fonts", "static"].contains(&n.to_string_lossy().as_ref()) {
                            let mut b = 20 * 1024 * 1024;
                            let _ = copy_dir(&p, &staging.join(&n), &mut b);
                        }
                    }
                }
            }
            let k = if kind.is_empty() { kind_of(&name).to_string() } else { kind.to_string() };
            (name, k)
        }
    } else if let Some(c) = content {
        let k = if kind.is_empty() { if c.trim_start().starts_with('<') { if c.contains("<svg") && !c.contains("<html") { "svg" } else { "html" } } else { "markdown" } } else { kind };
        let name = format!("index.{}", ext_for(k));
        std::fs::write(staging.join(&name), c)?;
        (name, k.to_string())
    } else {
        bail!("Give a path or the content.");
    };
    // swap in atomically so a viewer never sees half an update
    let old = core.artifacts_dir().join(format!("{id}.old"));
    let _ = std::fs::remove_dir_all(&old);
    if dir.exists() {
        std::fs::rename(&dir, &old)?;
    }
    std::fs::rename(&staging, &dir)?;
    let _ = std::fs::remove_dir_all(&old);
    let now = util::now_ms();
    if existing.is_some() {
        core.db.exec(
            "UPDATE artifacts SET kind = ?1, entry = ?2, description = CASE WHEN ?3 = '' THEN description ELSE ?3 END, version = version + 1, updated_at = ?4, thread_id = COALESCE(?5, thread_id) WHERE id = ?6",
            &[&kind, &entry, &description, &now, &thread, &id],
        )?;
    } else {
        core.db.exec(
            "INSERT INTO artifacts(id, project_id, thread_id, title, kind, entry, description, created_at, updated_at) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?8)",
            &[&id, &project, &thread, &title, &kind, &entry, &description, &now],
        )?;
    }
    let a = core.db.one("SELECT * FROM artifacts WHERE id = ?1", &[&id])?.unwrap_or(json!({}));
    core.bus.emit("artifact", project, thread, a.clone());
    Ok(a)
}

/// The file to serve for /a/{id}/{path}, refusing anything outside the artifact's folder.
pub fn file(core: &Core, id: &str, rel: &str) -> Option<PathBuf> {
    if id.contains(['/', '\\', '.']) {
        return None;
    }
    let root = core.artifacts_dir().join(id);
    let rel = rel.trim_start_matches('/');
    let p = if rel.is_empty() {
        let a = core.db.one("SELECT entry FROM artifacts WHERE id = ?1 AND deleted_at IS NULL", &[&id]).ok()??;
        root.join(a["entry"].as_str()?)
    } else {
        if rel.split(['/', '\\']).any(|seg| seg == ".." || seg.contains(':')) {
            return None;
        }
        root.join(rel)
    };
    let canon = p.canonicalize().ok()?;
    let root = root.canonicalize().ok()?;
    (canon.starts_with(&root) && canon.is_file()).then_some(canon)
}
