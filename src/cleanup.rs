//! Clears Rust build folders in the app's own workspaces, so they don't fill the disk.
//!
//! Every worker gets its own worktree, and an agent that runs `cargo build` there leaves a `target` folder of
//! several gigabytes behind. A build folder goes when its thread is resolved or deleted, when nothing has used it for
//! a while, or (longest unused first) when all of them together pass a size limit. The next build makes it again.
//!
//! Only folders inside the app's data folder are looked at, never a person's own project folders, and only ones Cargo
//! made (they carry its CACHEDIR.TAG) that hold nothing git tracks. One that a thread, a build or a running program
//! is using stays.

use crate::{
    core::{Core, CoreRef, Running},
    git, runner, util,
};
use std::{
    collections::HashMap,
    fs,
    io::Read,
    path::{Path, PathBuf},
    time::{Duration, UNIX_EPOCH},
};

const EVERY: Duration = Duration::from_secs(600);
/// Over the size limit, a folder still has to have been left alone this long before it goes.
const GRACE_MS: i64 = 30 * 60_000;
/// How far into a workspace to look: `target`, `src-tauri/target`, `apps/x/src-tauri/target`.
const DEPTH: usize = 4;
const CARGO_TAG: &[u8] = b"Signature: 8a477f597d28d172789f06886806bc55";
const GB: u64 = 1 << 30;

#[derive(Clone, Copy)]
struct Policy {
    /// Unused this long and a build folder goes. 0: age alone never removes one.
    idle_ms: i64,
    /// What all build folders may take together. 0: no limit.
    max_bytes: u64,
}

fn policy(core: &Core) -> Option<Policy> {
    let s = core.settings()["build_cleanup"].clone();
    if s["enabled"] == false {
        return None;
    }
    Some(Policy {
        idle_ms: s["idle_hours"].as_u64().unwrap_or(24) as i64 * 3_600_000,
        max_bytes: s["max_gb"].as_u64().unwrap_or(20).saturating_mul(GB),
    })
}

/// A folder of the app's that threads work in, and who it belongs to (to know when it is in use).
struct Place {
    root: PathBuf,
    thread: Option<String>,
    project: Option<String>,
}

struct Build {
    dir: PathBuf,
    place: usize,
    bytes: u64,
    used: i64,
}

/// The folders directly inside `dir`. Links and junctions are not followed.
fn subdirs(dir: &Path) -> Vec<(String, PathBuf)> {
    let Ok(rd) = fs::read_dir(dir) else { return vec![] };
    rd.flatten()
        .filter(|e| e.file_type().is_ok_and(|t| t.is_dir()))
        .map(|e| (e.file_name().to_string_lossy().into_owned(), e.path()))
        .collect()
}

fn places(data: &Path) -> Vec<Place> {
    let mut out = vec![];
    let of_thread = |id: String, root: PathBuf| Place { root, thread: Some(id), project: None };
    for (_, project) in subdirs(&data.join("worktrees")) {
        out.extend(subdirs(&project).into_iter().map(|(id, root)| of_thread(id, root)));
    }
    for kind in ["scratch", "chats"] {
        out.extend(subdirs(&data.join(kind)).into_iter().map(|(id, root)| of_thread(id, root)));
    }
    // repositories the app cloned itself, and the folders of projects that have none
    for (pid, dir) in subdirs(&data.join("projects")) {
        let mut roots = vec![dir.join("repo"), dir.join("files")];
        roots.extend(subdirs(&dir.join("repos")).into_iter().map(|x| x.1));
        out.extend(roots.into_iter().filter(|r| r.is_dir()).map(|root| Place { root, thread: None, project: Some(pid.clone()) }));
    }
    out
}

fn made_by_cargo(dir: &Path) -> bool {
    let mut head = [0u8; CARGO_TAG.len()];
    fs::File::open(dir.join("CACHEDIR.TAG")).and_then(|mut f| f.read_exact(&mut head)).is_ok() && head == CARGO_TAG
}

fn find(dir: &Path, depth: usize, out: &mut Vec<PathBuf>) {
    for (name, path) in subdirs(dir) {
        if name == ".git" || name == "node_modules" {
            continue;
        }
        if name == "target" && made_by_cargo(&path) {
            out.push(path);
        } else if depth > 1 {
            find(&path, depth - 1, out);
        }
    }
}

fn size(dir: &Path) -> u64 {
    let Ok(rd) = fs::read_dir(dir) else { return 0 };
    rd.flatten()
        .map(|e| match e.file_type() {
            Ok(t) if t.is_dir() => size(&e.path()),
            Ok(t) if t.is_file() => e.metadata().map(|m| m.len()).unwrap_or(0),
            _ => 0,
        })
        .sum()
}

/// When a build last wrote near the top of its folder (a profile folder changes with every build).
fn touched(dir: &Path, depth: usize) -> i64 {
    let own = fs::metadata(dir)
        .and_then(|m| m.modified())
        .ok()
        .and_then(|t| t.duration_since(UNIX_EPOCH).ok())
        .map(|d| d.as_millis() as i64)
        .unwrap_or(0);
    if depth == 0 {
        return own;
    }
    subdirs(dir).iter().map(|(_, p)| touched(p, depth - 1)).fold(own, i64::max)
}

fn executable(_path: &Path, _meta: &fs::Metadata) -> bool {
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        _meta.permissions().mode() & 0o111 != 0
    }
    #[cfg(not(unix))]
    {
        _path.extension().is_some_and(|x| x.eq_ignore_ascii_case("exe"))
    }
}

/// A build holds Cargo's lock, or a program built here is running (the system won't open it for writing).
fn in_use(dir: &Path, depth: usize) -> bool {
    let Ok(rd) = fs::read_dir(dir) else { return false };
    rd.flatten().any(|e| {
        let name = e.file_name().to_string_lossy().into_owned();
        let path = e.path();
        let Ok(meta) = e.metadata() else { return false };
        if meta.is_dir() {
            return depth > 0 && name != "incremental" && name != ".fingerprint" && in_use(&path, depth - 1);
        }
        let lock = name.starts_with(".cargo") && name.ends_with("lock");
        if !meta.is_file() || !(lock || executable(&path, &meta)) {
            return false;
        }
        match fs::OpenOptions::new().read(true).write(true).open(&path) {
            Ok(f) => lock && matches!(f.try_lock(), Err(fs::TryLockError::WouldBlock)),
            Err(e) => e.kind() != std::io::ErrorKind::NotFound,
        }
    })
}

/// Whether a thread that is running (or about to) works in this place.
fn busy(core: &CoreRef, running: &HashMap<String, Running>, place: &Place) -> bool {
    running.keys().any(|id| {
        if place.thread.as_deref() == Some(id.as_str()) {
            return true;
        }
        let Ok(t) = core.thread(id) else { return false };
        // the app's clone of a repository is shared by the whole project (the lead, read-only workers, merges)
        if place.project.is_some() && t["project_id"].as_str() == place.project.as_deref() {
            return true;
        }
        runner::workspace_of(core, &t).is_some_and(|w| w.starts_with(&place.root) || place.root.starts_with(&w))
    })
}

/// (the thread is resolved or deleted, when it last changed)
fn thread_state(core: &Core, id: &str) -> (bool, i64) {
    match core.db.one("SELECT resolved, deleted_at, updated_at FROM threads WHERE id = ?1", &[&id]) {
        Ok(Some(t)) => (t["resolved"] == 1 || t["deleted_at"].is_i64(), t["updated_at"].as_i64().unwrap_or(0)),
        _ => (false, 0),
    }
}

/// Whether git tracks anything in the folder. When git can't say, it counts as tracked.
fn tracked(rt: &tokio::runtime::Handle, dir: &Path) -> bool {
    let Some(parent) = dir.parent() else { return true };
    match rt.block_on(git::run_in(parent, &["ls-files", "--", "target"], None, 60)) {
        Ok(o) if o.ok() => !o.stdout.trim().is_empty(),
        // not a repository at all: nothing to track it
        Ok(o) => !o.text().contains("not a git repository"),
        Err(_) => true,
    }
}

fn remove(core: &CoreRef, rt: &tokio::runtime::Handle, place: &Place, build: &Build) -> bool {
    let own = |p: &Path| p.canonicalize().unwrap_or_else(|_| p.to_path_buf());
    if own(&core.exe).starts_with(own(&build.dir)) || tracked(rt, &build.dir) || in_use(&build.dir, 3) {
        return false;
    }
    let trash = core.data.join("build-trash");
    if fs::create_dir_all(&trash).is_err() {
        return false;
    }
    let gone = trash.join(util::id("build"));
    {
        // no turn can start while the folder is moved out of the way; deleting it can then take its time
        let running = core.running.lock().unwrap_or_else(|p| p.into_inner());
        if busy(core, &running, place) || fs::rename(&build.dir, &gone).is_err() {
            return false;
        }
    }
    if let Err(e) = fs::remove_dir_all(&gone) {
        tracing::warn!("couldn't finish deleting {}: {e} (it is tried again later)", gone.display());
    }
    tracing::info!("cleared build folder {} ({:.1} GB)", build.dir.display(), build.bytes as f64 / GB as f64);
    true
}

/// One pass. Returns how many folders went and how many bytes they held.
fn pass(core: &CoreRef, rt: &tokio::runtime::Handle, policy: Policy, now: i64) -> (usize, u64) {
    let places = places(&core.data);
    let (mut count, mut freed) = (0, 0);
    let mut kept = vec![];
    for (i, place) in places.iter().enumerate() {
        let (ended, changed) = place.thread.as_deref().map(|id| thread_state(core, id)).unwrap_or((false, 0));
        let mut dirs = vec![];
        find(&place.root, DEPTH, &mut dirs);
        for dir in dirs {
            let build = Build { bytes: size(&dir), used: changed.max(touched(&dir, 2)), place: i, dir };
            let idle = policy.idle_ms > 0 && now - build.used >= policy.idle_ms;
            if (ended || idle) && remove(core, rt, place, &build) {
                count += 1;
                freed += build.bytes;
            } else {
                kept.push(build);
            }
        }
    }
    if policy.max_bytes > 0 {
        let mut total: u64 = kept.iter().map(|b| b.bytes).sum();
        kept.sort_by_key(|b| b.used);
        for build in &kept {
            if total <= policy.max_bytes {
                break;
            }
            if now - build.used >= GRACE_MS && remove(core, rt, &places[build.place], build) {
                total -= build.bytes;
                count += 1;
                freed += build.bytes;
            }
        }
    }
    (count, freed)
}

pub async fn sweep(core: &CoreRef) -> (usize, u64) {
    static ONE: tokio::sync::Mutex<()> = tokio::sync::Mutex::const_new(());
    let _one = ONE.lock().await;
    let core = core.clone();
    let rt = tokio::runtime::Handle::current();
    tokio::task::spawn_blocking(move || {
        // anything a stopped app or a locked file left half deleted
        let _ = fs::remove_dir_all(core.data.join("build-trash"));
        let Some(p) = policy(&core) else { return (0, 0) };
        pass(&core, &rt, p, util::now_ms())
    })
    .await
    .unwrap_or_default()
}

/// Sweep shortly, once a thread that was just resolved or deleted has stopped.
pub fn soon(core: &CoreRef) {
    let core = core.clone();
    tokio::spawn(async move {
        tokio::time::sleep(Duration::from_secs(5)).await;
        sweep(&core).await;
    });
}

/// A minute after starting, then every ten minutes.
pub async fn watch(core: CoreRef) {
    tokio::time::sleep(Duration::from_secs(60)).await;
    loop {
        sweep(&core).await;
        tokio::time::sleep(EVERY).await;
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;
    use tokio::sync::watch as channel;

    const HOUR: i64 = 3_600_000;
    const DAY_ON: Policy = Policy { idle_ms: 24 * HOUR, max_bytes: 0 };

    struct App {
        core: CoreRef,
        data: PathBuf,
    }

    impl Drop for App {
        fn drop(&mut self) {
            let _ = fs::remove_dir_all(&self.data);
        }
    }

    fn app() -> App {
        let data = std::env::temp_dir().join(util::id("roamer-cleanup-test"));
        App { core: Core::open(data.clone(), true).unwrap(), data }
    }

    impl App {
        fn thread(&self, id: &str, resolved: i64, updated: i64) {
            self.core
                .db
                .exec(
                    "INSERT INTO threads(id, kind, title, agent, status, resolved, created_at, updated_at) VALUES (?1, 'worker', 'Test', 'claude', 'idle', ?2, 0, ?3)",
                    &[&id, &resolved, &updated],
                )
                .unwrap();
        }
        /// A worktree with a source file and a build folder as Cargo leaves it.
        fn worktree(&self, id: &str) -> PathBuf {
            let ws = self.data.join("worktrees").join("prj").join(id);
            fs::create_dir_all(ws.join("src")).unwrap();
            fs::write(ws.join("src").join("main.rs"), "fn main() {}").unwrap();
            build(&ws.join("target"));
            ws
        }
        async fn pass(&self, policy: Policy, now: i64) -> usize {
            let (core, rt) = (self.core.clone(), tokio::runtime::Handle::current());
            tokio::task::spawn_blocking(move || pass(&core, &rt, policy, now).0).await.unwrap()
        }
    }

    fn build(target: &Path) {
        fs::create_dir_all(target.join("debug").join("deps")).unwrap();
        fs::write(target.join("CACHEDIR.TAG"), [CARGO_TAG, b"\n# created by cargo\n"].concat()).unwrap();
        fs::write(target.join("debug").join("deps").join("lib.rlib"), vec![0u8; 4096]).unwrap();
        fs::write(target.join("debug").join(".cargo-lock"), "").unwrap();
    }

    #[tokio::test]
    async fn clears_idle_builds_and_nothing_else() {
        let a = app();
        let now = util::now_ms();
        a.thread("t1", 0, now);
        let ws = a.worktree("t1");
        // a nested one, as a Tauri app has
        build(&ws.join("app").join("src-tauri").join("target"));
        // a folder that happens to be called target
        fs::create_dir_all(ws.join("docs").join("target")).unwrap();
        fs::write(ws.join("docs").join("target").join("notes.md"), "keep").unwrap();

        assert_eq!(a.pass(DAY_ON, now + HOUR).await, 0);
        assert!(ws.join("target").exists());
        assert_eq!(a.pass(DAY_ON, now + 25 * HOUR).await, 2);
        assert!(!ws.join("target").exists());
        assert!(!ws.join("app").join("src-tauri").join("target").exists());
        assert!(ws.join("src").join("main.rs").exists());
        assert!(ws.join("docs").join("target").join("notes.md").exists());
    }

    #[tokio::test]
    async fn a_resolved_thread_loses_its_build_at_once() {
        let a = app();
        let now = util::now_ms();
        a.thread("done", 1, now);
        a.thread("open", 0, now);
        let (done, open) = (a.worktree("done"), a.worktree("open"));
        assert_eq!(a.pass(DAY_ON, now).await, 1);
        assert!(!done.join("target").exists());
        assert!(open.join("target").exists());
    }

    #[tokio::test]
    async fn leaves_alone_what_a_running_thread_uses() {
        let a = app();
        a.thread("author", 1, 0);
        a.thread("reviewer", 0, 0);
        let ws = a.worktree("author");
        // a review runs in the author's worktree
        a.core.db.exec("UPDATE threads SET workspace = ?1 WHERE id = 'reviewer'", &[&format!("path:{}", ws.display())]).unwrap();
        for id in ["author", "reviewer"] {
            let (cancel, _) = channel::channel(false);
            a.core.running.lock().unwrap().insert(id.into(), Running { turn_id: String::new(), cancel, started: 0 });
            assert_eq!(a.pass(DAY_ON, util::now_ms() + 48 * HOUR).await, 0, "{id} is running");
            a.core.running.lock().unwrap().clear();
        }
        assert_eq!(a.pass(DAY_ON, util::now_ms()).await, 1);
    }

    #[tokio::test]
    async fn leaves_alone_a_build_in_progress() {
        let a = app();
        a.thread("t1", 1, 0);
        let ws = a.worktree("t1");
        let lock = fs::OpenOptions::new().read(true).write(true).open(ws.join("target").join("debug").join(".cargo-lock")).unwrap();
        lock.lock().unwrap();
        assert_eq!(a.pass(DAY_ON, util::now_ms()).await, 0);
        lock.unlock().unwrap();
        drop(lock);
        assert_eq!(a.pass(DAY_ON, util::now_ms()).await, 1);
    }

    #[cfg(any(windows, target_os = "linux"))]
    #[tokio::test]
    async fn leaves_alone_a_program_that_is_running() {
        let a = app();
        a.thread("t1", 1, 0);
        let ws = a.worktree("t1");
        #[cfg(windows)]
        let (from, name, args) = (PathBuf::from(std::env::var("SystemRoot").unwrap()).join("System32").join("PING.EXE"), "app.exe", ["-n", "60", "127.0.0.1"]);
        #[cfg(not(windows))]
        let (from, name, args) = (PathBuf::from("/bin/sleep"), "app", ["60"]);
        let exe = ws.join("target").join("debug").join(name);
        fs::copy(from, &exe).unwrap();
        // another test forking at the same moment can hold the fresh copy open for an instant (Linux: "text file busy")
        let mut child = loop {
            match std::process::Command::new(&exe).args(args).stdout(std::process::Stdio::null()).spawn() {
                Ok(c) => break c,
                Err(e) if e.kind() == std::io::ErrorKind::ExecutableFileBusy => std::thread::sleep(Duration::from_millis(50)),
                Err(e) => panic!("{e}"),
            }
        };
        let while_running = a.pass(DAY_ON, util::now_ms()).await;
        child.kill().unwrap();
        child.wait().unwrap();
        assert_eq!(while_running, 0);
        assert!(exe.exists());
    }

    #[tokio::test]
    async fn over_the_limit_the_longest_unused_go_first() {
        let a = app();
        let now = util::now_ms();
        for (id, hours) in [("old", 1), ("middle", 2), ("new", 3)] {
            a.thread(id, 0, now + hours * HOUR);
            a.worktree(id);
        }
        let one = size(&a.data.join("worktrees").join("prj").join("new").join("target"));
        let limit = Policy { idle_ms: 0, max_bytes: one * 2 };
        assert_eq!(a.pass(limit, now + 10 * HOUR).await, 1);
        let has = |id: &str| a.data.join("worktrees").join("prj").join(id).join("target").exists();
        assert!(!has("old") && has("middle") && has("new"));
        // one used within the grace period stays, even over the limit
        assert_eq!(a.pass(Policy { idle_ms: 0, max_bytes: 1 }, now + 3 * HOUR + GRACE_MS - 1).await, 1);
        assert!(!has("middle") && has("new"));
    }

    #[tokio::test]
    async fn keeps_a_target_folder_git_tracks() {
        let a = app();
        a.thread("t1", 1, 0);
        let ws = a.worktree("t1");
        for args in [&["init", "-q"][..], &["add", "-f", "target/CACHEDIR.TAG"]] {
            assert!(git::run_in(&ws, args, None, 30).await.unwrap().ok());
        }
        assert_eq!(a.pass(DAY_ON, util::now_ms()).await, 0);
        assert!(ws.join("target").exists());
    }

    #[tokio::test]
    async fn the_switch_turns_it_off() {
        let a = app();
        a.thread("t1", 1, 0);
        let ws = a.worktree("t1");
        a.core.patch_settings(&json!({ "build_cleanup": { "enabled": false } })).unwrap();
        assert_eq!(sweep(&a.core).await.0, 0);
        assert!(ws.join("target").exists());
        a.core.patch_settings(&json!({ "build_cleanup": { "enabled": true } })).unwrap();
        assert_eq!(sweep(&a.core).await.0, 1);
        assert!(!ws.join("target").exists());
    }
}
