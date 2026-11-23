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
pub fn publish(core: &Core, project: Option<&str>, thread: Option<&str>, title: &str, description: &str, kind: &str, path: Option<&str>, content: Option<&str>, workspace: Option<&Path>, folder: Option<&str>) -> Result<Value> {
    let title = title.trim();
    if title.is_empty() {
        bail!("Give the artifact a title.");
    }
    // where it goes: the folder asked for (made if missing), else where it already is, else its thread's folder
    let asked = match folder.map(str::trim).filter(|f| !f.is_empty()) {
        Some(f) => Some(folder_for_path(core, project, thread, f)?),
        None => None,
    };
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
            "UPDATE artifacts SET kind = ?1, entry = ?2, description = CASE WHEN ?3 = '' THEN description ELSE ?3 END, version = version + 1, updated_at = ?4, thread_id = COALESCE(?5, thread_id), folder_id = COALESCE(?6, folder_id) WHERE id = ?7",
            &[&kind, &entry, &description, &now, &thread, &asked, &id],
        )?;
    } else {
        let folder_id = match asked {
            Some(f) => f,
            None => auto_folder(core, project, thread)?,
        };
        core.db.exec(
            "INSERT INTO artifacts(id, project_id, thread_id, title, kind, entry, description, created_at, updated_at, folder_id) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?8, ?9)",
            &[&id, &project, &thread, &title, &kind, &entry, &description, &now, &folder_id],
        )?;
    }
    let a = core.db.one("SELECT * FROM artifacts WHERE id = ?1", &[&id])?.unwrap_or(json!({}));
    core.bus.emit("artifact", project, thread, a.clone());
    Ok(a)
}

// ------------------------------------------------------------ folders

/// A folder the app keeps filing into (a project's, a thread's, the chats'), made the first time it's needed. Its name
/// follows the project or thread until the person renames it. A folder the person deleted is made again.
fn ensure_auto(core: &Core, key: &str, name: &str, parent: Option<&str>, project: Option<&str>) -> Result<String> {
    let now = util::now_ms();
    if let Some(f) = core.db.one("SELECT id, name, named_by FROM folders WHERE auto_key = ?1 AND deleted_at IS NULL", &[&key])? {
        let id = f["id"].as_str().unwrap_or("").to_string();
        if f["named_by"] == "auto" && f["name"].as_str() != Some(name) && !name.is_empty() {
            core.db.exec("UPDATE folders SET name = ?1, updated_at = ?2 WHERE id = ?3", &[&name, &now, &id])?;
        }
        return Ok(id);
    }
    let id = util::id("fld");
    core.db.exec(
        "INSERT INTO folders(id, parent_id, project_id, name, auto_key, created_at, updated_at) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?6)",
        &[&id, &parent, &project, &name, &key, &now],
    )?;
    Ok(id)
}

/// The top folder for a project's files, or the one that holds every chat's.
pub fn root_folder(core: &Core, project: Option<&str>) -> Result<String> {
    match project {
        Some(p) => {
            let name = core.db.one("SELECT name FROM projects WHERE id = ?1", &[&p])?.and_then(|r| r["name"].as_str().map(str::to_string)).unwrap_or_else(|| "Project".into());
            ensure_auto(core, &format!("project:{p}"), &name, None, Some(p))
        }
        None => ensure_auto(core, "chats", "Chats", None, None),
    }
}

/// Where a new artifact goes when nobody says: its project's folder, then its thread's (the lead's go straight in the
/// project folder), or its chat's folder under Chats.
pub fn auto_folder(core: &Core, project: Option<&str>, thread: Option<&str>) -> Result<String> {
    let root = root_folder(core, project)?;
    let Some(t) = thread.and_then(|t| core.db.one("SELECT id, kind, title FROM threads WHERE id = ?1", &[&t]).ok().flatten()) else { return Ok(root) };
    if t["kind"] == "lead" {
        return Ok(root);
    }
    let tid = t["id"].as_str().unwrap_or("");
    ensure_auto(core, &format!("thread:{tid}"), t["title"].as_str().unwrap_or("Thread"), Some(&root), project)
}

/// A folder by path, made where missing. "Designs/Mockups" is inside the project's folder (a chat's own folder for a
/// chat); "/Designs" starts from the top of the library.
pub fn folder_for_path(core: &Core, project: Option<&str>, thread: Option<&str>, path: &str) -> Result<String> {
    let mut parent: Option<String> = if path.trim_start().starts_with('/') {
        None
    } else if project.is_some() {
        Some(root_folder(core, project)?)
    } else {
        Some(auto_folder(core, None, thread)?)
    };
    let now = util::now_ms();
    for seg in path.split(['/', '\\']).map(str::trim).filter(|s| !s.is_empty() && *s != ".") {
        if seg == ".." {
            parent = parent.and_then(|p| core.db.one("SELECT parent_id FROM folders WHERE id = ?1", &[&p]).ok().flatten()).and_then(|r| r["parent_id"].as_str().map(str::to_string));
            continue;
        }
        let name: String = seg.chars().take(80).collect();
        let found = core.db.one(
            "SELECT id FROM folders WHERE IFNULL(parent_id, '') = ?1 AND lower(name) = lower(?2) AND deleted_at IS NULL",
            &[&parent.clone().unwrap_or_default(), &name],
        )?;
        parent = Some(match found {
            Some(f) => f["id"].as_str().unwrap_or("").to_string(),
            None => {
                let id = util::id("fld");
                core.db.exec(
                    "INSERT INTO folders(id, parent_id, project_id, name, named_by, created_at, updated_at) VALUES (?1, ?2, ?3, ?4, 'agent', ?5, ?5)",
                    &[&id, &parent, &project, &name, &now],
                )?;
                id
            }
        });
    }
    parent.ok_or_else(|| anyhow!("Name a folder."))
}

/// "Project / Thread / Designs"
pub fn folder_path(core: &Core, id: &str) -> String {
    let mut names = vec![];
    let mut cur = Some(id.to_string());
    while let Some(c) = cur.take() {
        let Ok(Some(f)) = core.db.one("SELECT name, parent_id FROM folders WHERE id = ?1", &[&c]) else { break };
        names.push(f["name"].as_str().unwrap_or("").to_string());
        cur = f["parent_id"].as_str().map(str::to_string);
        if names.len() > 32 {
            break;
        }
    }
    names.reverse();
    names.join(" / ")
}

/// True when `id` is `ancestor` or sits somewhere inside it (moving a folder there would make a loop).
pub fn is_within(core: &Core, id: &str, ancestor: &str) -> bool {
    let mut cur = Some(id.to_string());
    for _ in 0..64 {
        let Some(c) = cur.take() else { return false };
        if c == ancestor {
            return true;
        }
        cur = core.db.one("SELECT parent_id FROM folders WHERE id = ?1", &[&c]).ok().flatten().and_then(|r| r["parent_id"].as_str().map(str::to_string));
    }
    false
}

/// A folder and everything under it, for deleting (and undoing) as one.
pub fn subtree(core: &Core, id: &str) -> Vec<String> {
    let mut out = vec![id.to_string()];
    let mut i = 0;
    while i < out.len() && out.len() < 5000 {
        let kids = core.db.all("SELECT id FROM folders WHERE parent_id = ?1", &[&out[i]]).unwrap_or_default();
        out.extend(kids.iter().filter_map(|k| k["id"].as_str().map(str::to_string)));
        i += 1;
    }
    out
}

/// Artifacts from before folders existed go where a new one would.
pub fn file_unfiled(core: &Core) {
    let rows = core.db.all("SELECT id, project_id, thread_id FROM artifacts WHERE folder_id IS NULL AND deleted_at IS NULL", &[]).unwrap_or_default();
    for a in rows {
        if let Ok(f) = auto_folder(core, a["project_id"].as_str(), a["thread_id"].as_str()) {
            let _ = core.db.exec("UPDATE artifacts SET folder_id = ?1 WHERE id = ?2", &[&f, &a["id"].as_str().unwrap_or("")]);
        }
    }
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

/// Added to the end of every HTML artifact when it is served. The page runs sandboxed, so the app can't see what the
/// person selects in it; this tells the app (via postMessage to the window that framed it) so the person can comment on
/// that part. Alt+click does the same for a single element. It sends nothing anywhere else.
pub const COMMENT_SNIPPET: &str = r#"<script data-roamer-comment>(function(){
var mode='text',last=null;
function d(e){var o=[];for(var i=0;e&&e.nodeType===1&&i<4&&e!==document.body;i++,e=e.parentElement){var s=e.tagName.toLowerCase();if(e.id)s+='#'+e.id;else if(e.className&&typeof e.className==='string')s+='.'+e.className.trim().split(/\s+/).slice(0,2).join('.');o.unshift(s)}return o.join(' > ')}
function send(){if(mode!=='text')return;var s=getSelection(),t=s&&s.toString().trim();if(!t){parent.postMessage({mr:'sel',text:''},'*');return}
var r=s.getRangeAt(0).getBoundingClientRect(),n=s.anchorNode,e=n&&(n.nodeType===1?n:n.parentElement);
parent.postMessage({mr:'sel',text:t.slice(0,1500),x:r.left,y:r.bottom,el:d(e)},'*')}
function mark(e){if(last){last.style.outline=last.__o||'';last=null}if(e&&e!==document.body&&e!==document.documentElement){last=e;e.__o=e.style.outline;e.style.outline='2px solid #8f0'}}
function pin(e){var t=e.target,de=document.documentElement,w=Math.max(de.scrollWidth,1),h=Math.max(de.scrollHeight,1);
parent.postMessage({mr:'pin',mode:mode,el:d(t),text:(t.innerText||t.alt||'').trim().slice(0,300),x:e.clientX,y:e.clientY,px:Math.round(e.pageX/w*100),py:Math.round(e.pageY/h*100)},'*')}
addEventListener('message',function(e){if(e.source!==parent||!e.data||!e.data.mr)return;if(e.data.mr==='mode'){mode=e.data.mode;mark(null);document.documentElement.style.cursor=mode==='text'?'':'crosshair'}});
document.addEventListener('mouseover',function(e){if(mode==='element')mark(e.target)});
document.addEventListener('mouseup',function(){setTimeout(send,0)});
document.addEventListener('keyup',function(e){if(e.key==='Shift'||e.key.indexOf('Arrow')===0)setTimeout(send,0)});
document.addEventListener('click',function(e){if(mode==='text'&&!e.altKey)return;e.preventDefault();e.stopPropagation();pin(e)},true);
parent.postMessage({mr:'ready'},'*');
})();</script>"#;
