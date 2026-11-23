//! The project's working folder as the person sees it: what is in it, and which files agents are working on.
//!
//! A file counts as being worked on when a thread has *claimed* it, when a running thread has *changed* it in its
//! own worktree (git status), or - for threads that share the project folder itself - when it was modified in the
//! last few minutes while one of them is running.

use crate::{
    core::{CoreRef, Running},
    git, runner,
    server::{bad, R},
};
use axum::{
    extract::{Path, Query, State},
    Json,
};
use serde_json::{json, Value};
use std::{
    collections::{HashMap, HashSet},
    path::{Component, PathBuf},
};

const MAX_ENTRIES: usize = 2000;
const MAX_FILE: u64 = 256 * 1024;
const RECENT_MS: i64 = 5 * 60_000;

/// A project-relative path, with no way out of the folder: no `..`, no root, no drive.
fn clean(rel: &str) -> Option<String> {
    let mut parts = vec![];
    for c in PathBuf::from(rel.replace('\\', "/")).components() {
        match c {
            Component::Normal(s) => parts.push(s.to_string_lossy().into_owned()),
            Component::CurDir => {}
            _ => return None,
        }
    }
    Some(parts.join("/"))
}

/// The folder for a relative path, checked to stay inside the project's folder even through links.
fn resolve(home: &std::path::Path, rel: &str) -> Result<PathBuf, crate::server::ApiErr> {
    let rel = clean(rel).ok_or_else(|| bad("That path leaves the project folder."))?;
    let full = if rel.is_empty() { home.to_path_buf() } else { home.join(&rel) };
    let (Ok(h), Ok(f)) = (home.canonicalize(), full.canonicalize()) else { return Err(bad("That isn't in the project folder.")) };
    if !f.starts_with(&h) {
        return Err(bad("That path leaves the project folder."));
    }
    Ok(f)
}

fn overlaps(entry: &str, other: &str) -> bool {
    other == entry || other.starts_with(&format!("{entry}/")) || entry.starts_with(&format!("{other}/"))
}

/// `git status --porcelain` lines to project-relative paths ("R  a -> b" gives b).
fn porcelain_paths(out: &str) -> Vec<String> {
    out.lines()
        .filter(|l| l.len() > 3)
        .map(|l| {
            let p = &l[3..];
            let p = p.rsplit(" -> ").next().unwrap_or(p);
            p.trim_matches('"').trim_end_matches('/').to_string()
        })
        .collect()
}

struct Who {
    id: String,
    title: String,
    agent: String,
}

pub async fn list(State(core): State<CoreRef>, Path(id): Path<String>, Query(q): Query<HashMap<String, String>>) -> R {
    let p = core.project(&id)?;
    let home = core.project_home(&p);
    let rel = clean(q.get("path").map(String::as_str).unwrap_or("")).ok_or_else(|| bad("That path leaves the project folder."))?;
    let dir = resolve(&home, &rel)?;
    if !dir.is_dir() {
        return Err(bad("That isn't a folder."));
    }

    // who is working on what
    let threads = core
        .db
        .all("SELECT id, title, agent, status, workspace, kind FROM threads WHERE project_id = ?1 AND resolved = 0 AND deleted_at IS NULL AND kind != 'lead'", &[&id])?;
    let running: HashSet<String> = core.running.lock().map(|r| r.iter().filter(|(_, x): &(&String, &Running)| !x.turn_id.is_empty()).map(|(k, _)| k.clone()).collect()).unwrap_or_default();
    let claims = core.db.all("SELECT c.path, c.note, c.thread_id, t.title, t.agent FROM claims c JOIN threads t ON t.id = c.thread_id WHERE c.project_id = ?1 AND t.resolved = 0", &[&id])?;
    let claimed: Vec<(String, Who, String)> = claims
        .iter()
        .filter_map(|c| {
            let path = clean(c["path"].as_str()?.trim_end_matches(['/', '*']))?;
            Some((path, Who { id: c["thread_id"].as_str()?.into(), title: c["title"].as_str().unwrap_or("").into(), agent: c["agent"].as_str().unwrap_or("").into() }, c["note"].as_str().unwrap_or("").into()))
        })
        .collect();

    let worktrees = core.data.join("worktrees");
    let mut changed: Vec<(Who, Vec<String>)> = vec![];
    let mut shared: Vec<Who> = vec![];
    for t in &threads {
        let tid = t["id"].as_str().unwrap_or("");
        let live = running.contains(tid) || t["status"] == "working";
        if !live {
            continue;
        }
        let who = || Who { id: tid.into(), title: t["title"].as_str().unwrap_or("").into(), agent: t["agent"].as_str().unwrap_or("").into() };
        match runner::workspace_of(&core, t).or_else(|| Some(git::worktree_path(&core.data, &id, tid))) {
            Some(ws) if ws.starts_with(&worktrees) && ws.is_dir() => {
                if let Ok(o) = git::run_in(&ws, &["status", "--porcelain", "-uall"], None, 15).await {
                    changed.push((who(), porcelain_paths(&o.stdout)));
                }
            }
            _ => shared.push(who()),
        }
    }

    let now = crate::util::now_ms();
    let mut entries = vec![];
    let mut truncated = false;
    if let Ok(rd) = std::fs::read_dir(&dir) {
        for e in rd.flatten() {
            let name = e.file_name().to_string_lossy().into_owned();
            if name == ".git" {
                continue;
            }
            if entries.len() >= MAX_ENTRIES {
                truncated = true;
                break;
            }
            let md = e.metadata().ok();
            let is_dir = md.as_ref().is_some_and(|m| m.is_dir());
            let mtime = md.as_ref().and_then(|m| m.modified().ok()).and_then(|t| t.duration_since(std::time::UNIX_EPOCH).ok()).map(|d| d.as_millis() as i64).unwrap_or(0);
            let path = if rel.is_empty() { name.clone() } else { format!("{rel}/{name}") };
            let mut marks: Vec<Value> = vec![];
            let mut seen = HashSet::new();
            let mut mark = |w: &Who, how: &str, note: &str| {
                if seen.insert((w.id.clone(), how.to_string())) {
                    marks.push(json!({ "thread_id": w.id, "title": w.title, "agent": w.agent, "how": how, "note": note }));
                }
            };
            for (cp, w, note) in &claimed {
                if overlaps(&path, cp) {
                    mark(w, "claimed", note);
                }
            }
            for (w, files) in &changed {
                if files.iter().any(|f| f == &path || f.starts_with(&format!("{path}/"))) {
                    mark(w, "changed", "");
                }
            }
            if !is_dir && now - mtime < RECENT_MS {
                for w in &shared {
                    mark(w, "recent", "");
                }
            }
            entries.push(json!({ "name": name, "path": path, "dir": is_dir, "size": md.as_ref().map(|m| m.len()).unwrap_or(0), "mtime": mtime, "marks": marks }));
        }
    }
    entries.sort_by(|a, b| {
        b["dir"].as_bool().cmp(&a["dir"].as_bool()).then_with(|| a["name"].as_str().unwrap_or("").to_lowercase().cmp(&b["name"].as_str().unwrap_or("").to_lowercase()))
    });

    // everyone working somewhere in this project, for the strip above the list
    let mut active: Vec<Value> = vec![];
    let mut add = |w: &Who, files: usize, how: &str| active.push(json!({ "thread_id": w.id, "title": w.title, "agent": w.agent, "files": files, "how": how }));
    for (w, files) in &changed {
        add(w, files.len(), "worktree");
    }
    for w in &shared {
        add(w, 0, "shared");
    }
    let parent = if rel.is_empty() { Value::Null } else { json!(rel.rsplit_once('/').map(|x| x.0).unwrap_or("")) };
    Ok(Json(json!({
        "root": home.display().to_string(), "path": rel, "parent": parent,
        "entries": entries, "truncated": truncated, "active": active,
        "claims": claimed.len(),
    })))
}

pub async fn read(State(core): State<CoreRef>, Path(id): Path<String>, Query(q): Query<HashMap<String, String>>) -> R {
    let p = core.project(&id)?;
    let home = core.project_home(&p);
    let rel = q.get("path").cloned().unwrap_or_default();
    let full = resolve(&home, &rel)?;
    if !full.is_file() {
        return Err(bad("That isn't a file."));
    }
    let md = std::fs::metadata(&full).map_err(|e| bad(e.to_string()))?;
    let mut buf = Vec::new();
    {
        use std::io::Read;
        let f = std::fs::File::open(&full).map_err(|e| bad(e.to_string()))?;
        f.take(MAX_FILE).read_to_end(&mut buf).map_err(|e| bad(e.to_string()))?;
    }
    let binary = buf.contains(&0);
    Ok(Json(json!({
        "path": clean(&rel).unwrap_or_default(), "size": md.len(), "binary": binary, "truncated": md.len() > MAX_FILE,
        "text": if binary { String::new() } else { String::from_utf8_lossy(&buf).into_owned() },
    })))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn paths_cannot_leave_the_folder() {
        assert_eq!(clean("src/./a.rs").as_deref(), Some("src/a.rs"));
        assert_eq!(clean("").as_deref(), Some(""));
        assert!(clean("../x").is_none());
        assert!(clean("a/../../x").is_none());
        assert!(clean("/etc/passwd").is_none());
    }

    #[test]
    fn claims_mark_files_and_the_folders_around_them() {
        assert!(overlaps("src", "src/a.rs"));
        assert!(overlaps("src/a.rs", "src"));
        assert!(overlaps("src/a.rs", "src/a.rs"));
        assert!(!overlaps("src", "srcs/a.rs"));
        assert!(!overlaps("docs", "src/a.rs"));
    }

    #[test]
    fn reads_git_status_paths() {
        let out = " M src/a.rs\n?? new/b.rs\nR  old.rs -> moved.rs\n";
        assert_eq!(porcelain_paths(out), vec!["src/a.rs", "new/b.rs", "moved.rs"]);
    }
}
