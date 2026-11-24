//! A project's repositories. The primary one is the project's own folder / repo_url / base_branch (as it was before
//! projects could have several, so older projects open unchanged); the others live in the `repos` table. Each is a
//! GitHub repository Model Roamer clones, or a folder on the machine it runs on (a git repository or a plain folder).
//! A thread names the one it works on in `threads.repo_id` ('' is the primary).
//!
//! `view` gives back the project as one repository sees it - its folder, repo_url and base_branch swapped in - so the
//! workspace, merge and pull-request code works on any of them without knowing there are several.

use crate::{
    core::{Core, CoreRef},
    git, github,
    server::{bad, R},
    util,
};
use anyhow::{anyhow, bail, Result};
use axum::{
    extract::{Path, Query, State},
    Json,
};
use serde_json::{json, Value};
use std::{collections::HashMap, path::PathBuf, sync::OnceLock};

/// The id the API and the agents use for the primary repository.
pub const PRIMARY: &str = "primary";

fn s(v: &Value, k: &str) -> String {
    v[k].as_str().unwrap_or("").trim().to_string()
}

/// "primary" and "" both mean the project's own repository.
pub fn norm(rid: &str) -> &str {
    let r = rid.trim();
    if r == PRIMARY { "" } else { r }
}

/// The repositories added beside the primary one.
pub fn extras(core: &Core, pid: &str) -> Result<Vec<Value>> {
    core.db.all("SELECT * FROM repos WHERE project_id = ?1 AND deleted_at IS NULL ORDER BY created_at", &[&pid])
}

/// One added repository, removed or not (a thread that worked on it can still merge and clean up).
fn row(core: &Core, pid: &str, rid: &str) -> Result<Value> {
    core.db.one("SELECT * FROM repos WHERE id = ?1 AND project_id = ?2", &[&rid, &pid])?.ok_or_else(|| anyhow!("No repository {rid} in this project."))
}

fn clone_dir(core: &Core, pid: &str, rid: &str) -> PathBuf {
    core.data.join("projects").join(pid).join("repos").join(rid)
}

/// "https://github.com/a/b.git" -> "a/b"; a folder -> its last part.
fn short_name(url: &str, folder: &str) -> String {
    if let Some((o, r)) = git::github_repo(url) {
        return format!("{o}/{r}");
    }
    if !folder.is_empty() {
        if let Some(n) = PathBuf::from(folder).file_name() {
            return n.to_string_lossy().into_owned();
        }
        return folder.to_string();
    }
    let u = url.trim_end_matches('/').trim_end_matches(".git");
    u.rsplit(['/', ':']).next().unwrap_or(u).to_string()
}

/// The project as repository `rid` sees it.
pub fn view(core: &Core, p: &Value, rid: &str) -> Result<Value> {
    let rid = norm(rid);
    let mut v = p.clone();
    if rid.is_empty() {
        v["repo_id"] = json!("");
        v["repo_name"] = json!(short_name(p["repo_url"].as_str().unwrap_or(""), p["folder"].as_str().unwrap_or("")));
        return Ok(v);
    }
    let pid = p["id"].as_str().unwrap_or("");
    let r = row(core, pid, rid)?;
    // not cloned yet: it will be, there, before anything works in it (see ensure_folder)
    let folder = match r["folder"].as_str().unwrap_or("") {
        "" => clone_dir(core, pid, rid).display().to_string(),
        f => f.to_string(),
    };
    v["folder"] = json!(folder);
    v["repo_url"] = r["url"].clone();
    v["base_branch"] = r["base_branch"].clone();
    v["repo_id"] = json!(rid);
    v["repo_name"] = r["name"].clone();
    Ok(v)
}

/// The project as the repository a thread works on sees it.
pub fn for_thread(core: &Core, t: &Value) -> Result<Value> {
    let p = core.project(t["project_id"].as_str().unwrap_or(""))?;
    view(core, &p, t["repo_id"].as_str().unwrap_or(""))
}

/// Which repository a request means: "", "primary", an id or a name (any case). Gives back the id ('' = primary).
pub fn resolve(core: &Core, pid: &str, want: &str) -> Result<String> {
    let w = norm(want);
    if w.is_empty() {
        return Ok(String::new());
    }
    let list = extras(core, pid)?;
    if let Some(r) = list.iter().find(|r| r["id"] == w || r["name"].as_str().is_some_and(|n| n.eq_ignore_ascii_case(w))) {
        return Ok(r["id"].as_str().unwrap_or("").to_string());
    }
    let p = core.project(pid)?;
    if short_name(p["repo_url"].as_str().unwrap_or(""), p["folder"].as_str().unwrap_or("")).eq_ignore_ascii_case(w) {
        return Ok(String::new());
    }
    let names: Vec<String> = std::iter::once(format!("{PRIMARY} (the project's own)")).chain(list.iter().map(|r| format!("{} ({})", r["id"].as_str().unwrap_or(""), r["name"].as_str().unwrap_or("")))).collect();
    bail!("This project has no repository '{w}'. It has: {}.", names.join(", "))
}

/// Every repository of a project, the primary first, as the UI and the lead see them.
pub fn list(core: &Core, p: &Value) -> Result<Vec<Value>> {
    let pid = p["id"].as_str().unwrap_or("");
    let open = |rid: &str| core.db.scalar_i64("SELECT COUNT(*) FROM threads WHERE project_id = ?1 AND repo_id = ?2 AND resolved = 0 AND deleted_at IS NULL AND kind != 'lead'", &[&pid, &rid]).unwrap_or(0);
    let on_disk = |f: &str| !f.is_empty() && PathBuf::from(f).is_dir();
    let is_git = |f: &str| !f.is_empty() && PathBuf::from(f).join(".git").exists();
    let pf = p["folder"].as_str().unwrap_or("");
    let pu = p["repo_url"].as_str().unwrap_or("");
    let mut out = vec![json!({
        "id": PRIMARY, "primary": true, "name": if pf.is_empty() && pu.is_empty() { "Project files".to_string() } else { short_name(pu, pf) },
        // a GitHub repository Model Roamer cloned into its data, or a folder the person chose
        "kind": if !pu.is_empty() && (pf.is_empty() || PathBuf::from(pf).starts_with(&core.data)) { "github" } else { "folder" },
        "url": pu, "folder": pf, "base_branch": p["base_branch"], "home": core.project_home(p).display().to_string(),
        "present": pf.is_empty() || on_disk(pf), "git": is_git(&core.project_home(p).display().to_string()), "threads": open(""),
    })];
    for r in extras(core, pid)? {
        let rid = r["id"].as_str().unwrap_or("");
        let f = r["folder"].as_str().unwrap_or("");
        let home = if f.is_empty() { clone_dir(core, pid, rid).display().to_string() } else { f.to_string() };
        out.push(json!({
            "id": rid, "primary": false, "name": r["name"], "kind": r["kind"], "url": r["url"], "folder": f, "base_branch": r["base_branch"], "home": home,
            // a GitHub repository with no folder yet is still being cloned (or the clone failed: see `cloning`)
            "present": on_disk(f), "cloning": f.is_empty() && !r["url"].as_str().unwrap_or("").is_empty(), "git": is_git(&home), "threads": open(rid),
            "created_at": r["created_at"],
        }));
    }
    Ok(out)
}

/// One line per repository, for the lead's instructions.
pub fn describe(core: &Core, p: &Value) -> String {
    let Ok(l) = list(core, p) else { return String::new() };
    if l.len() < 2 {
        return String::new();
    }
    let mut s = String::from("This project has several repositories. Pass `repo` (the id) to spawn_thread to choose which one a thread works on; it defaults to the primary.\n");
    for r in &l {
        s.push_str(&format!(
            "- `{}`: {}{} ({}{})\n",
            r["id"].as_str().unwrap_or(""),
            r["name"].as_str().unwrap_or(""),
            if r["primary"] == true { " [primary]" } else { "" },
            if r["kind"] == "folder" { format!("folder {}", r["home"].as_str().unwrap_or("")) } else { r["url"].as_str().unwrap_or("").to_string() },
            r["base_branch"].as_str().filter(|b| !b.is_empty()).map(|b| format!(", base `{b}`")).unwrap_or_default(),
        ));
    }
    s
}

// ------------------------------------------------------------------------------------------------ folders

/// "C:\\x\\" and "C:\\x" are the same folder; so are "/a/b/" and "/a/b".
fn tidy(p: &str) -> String {
    let t = p.trim();
    let trimmed = t.trim_end_matches(['/', '\\']);
    if trimmed.is_empty() || trimmed.ends_with(':') { t.to_string() } else { trimmed.to_string() }
}

/// For comparing folders: the real path, without Windows' `\\?\` prefix, in one case on Windows.
fn same_key(p: &str) -> String {
    let c = std::fs::canonicalize(p).map(|c| c.display().to_string()).unwrap_or_else(|_| tidy(p));
    let c = c.strip_prefix(r"\\?\").unwrap_or(&c).to_string();
    if cfg!(windows) { c.to_lowercase() } else { c }
}

/// Check a folder on this machine before a project uses it: a full path, there (or made, when asked), a folder,
/// readable, and not inside Model Roamer's own data. Says whether it is a git repository and what it tracks.
pub async fn check_folder(core: &Core, raw: &str, create: bool) -> Result<Value> {
    let path = tidy(raw);
    if path.is_empty() {
        bail!("Give a folder path.");
    }
    let pb = PathBuf::from(&path);
    if !pb.is_absolute() {
        bail!("Give the full path to the folder (starting with / or a drive letter), not {path}.");
    }
    if !pb.exists() {
        if !create {
            bail!("{path} doesn't exist on {}.", core.display_name());
        }
        std::fs::create_dir_all(&pb).map_err(|e| anyhow!("Couldn't make {path}: {e}"))?;
    }
    if !pb.is_dir() {
        bail!("{path} is a file, not a folder.");
    }
    std::fs::read_dir(&pb).map_err(|e| anyhow!("Model Roamer can't read {path}: {e}"))?;
    let data = same_key(&core.data.display().to_string());
    let key = same_key(&path);
    if key == data || key.starts_with(&format!("{data}{}", std::path::MAIN_SEPARATOR)) {
        bail!("{path} is inside Model Roamer's own data folder. Choose a folder of your own.");
    }
    let is_git = git::is_repo(&pb).await;
    let (branch, remote) = if is_git { (git::current_branch(&pb).await.unwrap_or_default(), git::remote_url(&pb).await) } else { (String::new(), String::new()) };
    Ok(json!({ "path": path, "name": short_name("", &path), "git": is_git, "branch": branch, "remote": remote }))
}

fn same_url(a: &str, b: &str) -> bool {
    let n = |u: &str| match git::github_repo(u) {
        Some((o, r)) => format!("github:{o}/{r}").to_lowercase(),
        None => u.trim().trim_end_matches('/').trim_end_matches(".git").to_lowercase(),
    };
    !a.trim().is_empty() && n(a) == n(b)
}

/// A repository about to be added, checked: {name, kind, url, folder, base_branch}. `taken` are the project's
/// current repositories (from `list`), so the same one isn't added twice.
pub async fn check_new(core: &Core, b: &Value, taken: &[Value]) -> Result<Value> {
    let mut url = s(b, "url");
    let mut base = s(b, "base_branch");
    let raw_folder = s(b, "folder");
    if url.is_empty() && raw_folder.is_empty() {
        bail!("Give a repository address or a folder.");
    }
    if url.chars().any(char::is_whitespace) {
        bail!("That repository address has spaces in it.");
    }
    if !url.is_empty() && !(url.starts_with("https://") || url.starts_with("http://") || url.starts_with("git@") || url.starts_with("ssh://") || url.starts_with("file://")) {
        // "owner/repo" is a GitHub repository
        if url.split('/').count() == 2 && !url.starts_with('/') {
            url = format!("https://github.com/{url}.git");
        } else {
            bail!("{url} isn't a repository address. Use https://github.com/owner/repo or owner/repo.");
        }
    }
    let mut folder = String::new();
    if !raw_folder.is_empty() {
        let f = check_folder(core, &raw_folder, b["create_folder"] == true).await?;
        folder = f["path"].as_str().unwrap_or("").to_string();
        if url.is_empty() {
            url = f["remote"].as_str().unwrap_or("").to_string();
        }
        if base.is_empty() {
            base = f["branch"].as_str().unwrap_or("").to_string();
        }
    }
    for t in taken {
        let tf = t["folder"].as_str().unwrap_or("");
        if !folder.is_empty() && !tf.is_empty() && same_key(tf) == same_key(&folder) {
            bail!("{folder} is already one of this project's repositories ({}).", t["name"].as_str().unwrap_or(""));
        }
        if folder.is_empty() && same_url(&url, t["url"].as_str().unwrap_or("")) {
            bail!("{url} is already one of this project's repositories ({}).", t["name"].as_str().unwrap_or(""));
        }
    }
    let name = match s(b, "name") {
        n if n.is_empty() => short_name(&url, &folder),
        n => n,
    };
    Ok(json!({ "name": name, "kind": if folder.is_empty() { "github" } else { "folder" }, "url": url, "folder": folder, "base_branch": base }))
}

/// Add a checked repository (from `check_new`). A GitHub one starts cloning in the background.
pub fn insert(core: &CoreRef, pid: &str, r: &Value) -> Result<Value> {
    let id = util::id("repo");
    let now = util::now_ms();
    core.db.exec(
        "INSERT INTO repos(id, project_id, name, kind, url, folder, base_branch, created_at, updated_at) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?8)",
        &[&id, &pid, &s(r, "name"), &s(r, "kind"), &s(r, "url"), &s(r, "folder"), &s(r, "base_branch"), &now],
    )?;
    changed(core, pid);
    if s(r, "folder").is_empty() {
        let (c, pid, rid) = (core.clone(), pid.to_string(), id.clone());
        tokio::spawn(async move {
            if let Err(e) = ensure_folder(&c, &pid, &rid).await {
                c.bus.emit("toast", Some(&pid), None, json!({ "err": true, "text": format!("Couldn't clone the repository: {e:#}") }));
                changed(&c, &pid);
            }
        });
    }
    Ok(row(core, pid, &id)?)
}

fn changed(core: &Core, pid: &str) {
    core.bus.emit("repos", Some(pid), None, json!({ "project": pid }));
}

/// An added GitHub repository gets cloned into the app's data folder the first time anything needs it.
pub async fn ensure_folder(core: &Core, pid: &str, rid: &str) -> Result<()> {
    // one clone at a time: two threads starting together on the same repository mustn't both clone it
    static GATE: OnceLock<tokio::sync::Mutex<()>> = OnceLock::new();
    let _g = GATE.get_or_init(|| tokio::sync::Mutex::new(())).lock().await;
    let r = row(core, pid, rid)?;
    let url = s(&r, "url");
    if !s(&r, "folder").is_empty() || url.is_empty() {
        return Ok(());
    }
    let into = clone_dir(core, pid, rid);
    if !into.join(".git").exists() {
        if into.exists() {
            // a clone that died halfway leaves a folder git won't clone into
            let _ = std::fs::remove_dir_all(&into);
        }
        core.bus.emit("progress", Some(pid), None, json!({ "text": format!("Cloning {url}…") }));
        let tok = github::token(core).await;
        git::clone(&url, &into, tok.as_deref()).await?;
    }
    let base = git::default_branch(&into).await;
    let f = into.display().to_string();
    let now = util::now_ms();
    core.db.exec("UPDATE repos SET folder = ?1, base_branch = CASE WHEN base_branch = '' THEN ?2 ELSE base_branch END, updated_at = ?3 WHERE id = ?4", &[&f, &base, &now, &rid])?;
    changed(core, pid);
    Ok(())
}

// ------------------------------------------------------------------------------------------------ API

type Q = Query<HashMap<String, String>>;

pub async fn list_api(State(core): State<CoreRef>, Path(id): Path<String>) -> R {
    let p = core.project(&id)?;
    Ok(Json(json!(list(&core, &p)?)))
}

pub async fn add_api(State(core): State<CoreRef>, Path(id): Path<String>, Json(b): Json<Value>) -> R {
    let p = core.project(&id)?;
    let r = check_new(&core, &b, &list(&core, &p)?).await.map_err(|e| bad(format!("{e:#}")))?;
    Ok(Json(insert(&core, &id, &r)?))
}

pub async fn patch_api(State(core): State<CoreRef>, Path((id, rid)): Path<(String, String)>, Json(b): Json<Value>) -> R {
    let rid = norm(&rid).to_string();
    if rid.is_empty() {
        return Err(bad("Change the primary repository under Project above."));
    }
    row(&core, &id, &rid)?;
    let now = util::now_ms();
    if let Some(n) = b["name"].as_str() {
        if n.trim().is_empty() {
            return Err(bad("A repository needs a name."));
        }
        core.db.exec("UPDATE repos SET name = ?1, updated_at = ?2 WHERE id = ?3", &[&n.trim(), &now, &rid])?;
    }
    if let Some(v) = b["base_branch"].as_str() {
        core.db.exec("UPDATE repos SET base_branch = ?1, updated_at = ?2 WHERE id = ?3", &[&v.trim(), &now, &rid])?;
    }
    changed(&core, &id);
    Ok(Json(row(&core, &id, &rid)?))
}

/// Take a repository off the project. Nothing on disk is deleted, so Undo (restore) brings it back as it was.
pub async fn delete_api(State(core): State<CoreRef>, Path((id, rid)): Path<(String, String)>) -> R {
    let rid = norm(&rid).to_string();
    if rid.is_empty() {
        return Err(bad("The primary repository can't be removed. Change it under Project above, or remove the others."));
    }
    row(&core, &id, &rid)?;
    let open = core.db.all("SELECT title FROM threads WHERE project_id = ?1 AND repo_id = ?2 AND resolved = 0 AND deleted_at IS NULL AND kind != 'lead'", &[&id, &rid])?;
    if !open.is_empty() {
        let titles: Vec<&str> = open.iter().filter_map(|t| t["title"].as_str()).collect();
        return Err(bad(format!("{} open thread{} still work{} on it ({}). Resolve or delete {} first.", open.len(), if open.len() == 1 { "" } else { "s" }, if open.len() == 1 { "s" } else { "" }, titles.join(", "), if open.len() == 1 { "it" } else { "them" })));
    }
    let now = util::now_ms();
    core.db.exec("UPDATE repos SET deleted_at = ?1 WHERE id = ?2", &[&now, &rid])?;
    changed(&core, &id);
    Ok(Json(json!({ "ok": true })))
}

pub async fn restore_api(State(core): State<CoreRef>, Path((id, rid)): Path<(String, String)>) -> R {
    row(&core, &id, &rid)?;
    core.db.exec("UPDATE repos SET deleted_at = NULL WHERE id = ?1", &[&rid])?;
    changed(&core, &id);
    Ok(Json(row(&core, &id, &rid)?))
}

/// Is this a folder a project can use? For the path box, before anything is saved.
pub async fn check_api(State(core): State<CoreRef>, q: Q) -> R {
    let path = q.get("path").cloned().unwrap_or_default();
    match check_folder(&core, &path, false).await {
        Ok(v) => Ok(Json(json!({ "ok": true, "folder": v }))),
        Err(e) => Ok(Json(json!({ "ok": false, "error": format!("{e:#}") }))),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn names_and_paths() {
        assert_eq!(short_name("https://github.com/a/b.git", ""), "a/b");
        assert_eq!(short_name("", "/srv/code/thing"), "thing");
        assert_eq!(short_name("https://gitlab.com/x/y.git", ""), "y");
        assert_eq!(tidy("/a/b/"), "/a/b");
        assert_eq!(tidy("/"), "/");
        assert_eq!(tidy(r"C:\"), r"C:\");
        assert_eq!(norm("primary"), "");
        assert!(same_url("https://github.com/A/B.git", "git@github.com:a/b"));
        assert!(!same_url("", ""));
    }
}
