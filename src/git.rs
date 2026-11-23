//! Git, through the git program: worktrees (one per worker so agents never step on each other), diffs, commits,
//! merges and pushes. Pushes and clones to GitHub carry the app's token, so they work even when the agent's own CLI
//! has no GitHub sign-in.

use crate::procs::{self, Launch, Output, Resolved};
use anyhow::{anyhow, bail, Result};
use base64::Engine;
use std::{
    path::{Path, PathBuf},
    time::Duration,
};

pub fn git() -> Option<Resolved> {
    procs::resolve(&["git"], None)
}

/// Run git in `dir`. `token` is added as an auth header for github.com remotes.
pub async fn run_in(dir: &Path, args: &[&str], token: Option<&str>, timeout_s: u64) -> Result<Output> {
    let g = git().ok_or_else(|| anyhow!("Git isn't installed. Install it from Agents › Tools."))?;
    let mut a: Vec<String> = vec!["-c".into(), "core.quotepath=off".into(), "-c".into(), "color.ui=never".into()];
    if let Some(t) = token.filter(|t| !t.is_empty()) {
        let basic = base64::engine::general_purpose::STANDARD.encode(format!("x-access-token:{t}"));
        a.push("-c".into());
        a.push(format!("http.https://github.com/.extraheader=AUTHORIZATION: basic {basic}"));
    }
    a.extend(args.iter().map(|s| s.to_string()));
    let env = vec![("GIT_TERMINAL_PROMPT".to_string(), "0".to_string()), ("GCM_INTERACTIVE".into(), "never".into())];
    procs::run(&g, &Launch { args: a, env, cwd: Some(dir.to_path_buf()), ..Default::default() }, Duration::from_secs(timeout_s)).await
}

async fn ok(dir: &Path, args: &[&str]) -> Result<String> {
    let o = run_in(dir, args, None, 120).await?;
    if !o.ok() {
        bail!("git {} failed: {}", args.join(" "), o.text().trim());
    }
    Ok(o.stdout.trim_end().to_string())
}

pub async fn is_repo(dir: &Path) -> bool {
    dir.is_dir() && run_in(dir, &["rev-parse", "--is-inside-work-tree"], None, 20).await.map(|o| o.ok() && o.stdout.trim() == "true").unwrap_or(false)
}

pub async fn current_branch(dir: &Path) -> Result<String> {
    ok(dir, &["rev-parse", "--abbrev-ref", "HEAD"]).await
}

pub async fn remote_url(dir: &Path) -> String {
    ok(dir, &["remote", "get-url", "origin"]).await.unwrap_or_default()
}

pub async fn is_clean(dir: &Path) -> bool {
    ok(dir, &["status", "--porcelain"]).await.map(|s| s.trim().is_empty()).unwrap_or(false)
}

/// A worktree on a new branch from `base`, for one worker.
pub async fn add_worktree(repo: &Path, path: &Path, branch: &str, base: &str) -> Result<()> {
    if path.join(".git").exists() {
        return Ok(()); // already there (a retry or a restart)
    }
    if let Some(p) = path.parent() {
        std::fs::create_dir_all(p)?;
    }
    let ps = path.display().to_string();
    let exists = run_in(repo, &["rev-parse", "--verify", "--quiet", &format!("refs/heads/{branch}")], None, 20).await.map(|o| o.ok()).unwrap_or(false);
    let _ = ok(repo, &["worktree", "prune"]).await;
    if exists {
        ok(repo, &["worktree", "add", &ps, branch]).await?;
    } else {
        ok(repo, &["worktree", "add", "-b", branch, &ps, base]).await?;
    }
    Ok(())
}

pub async fn remove_worktree(repo: &Path, path: &Path) -> Result<()> {
    let ps = path.display().to_string();
    let _ = ok(repo, &["worktree", "remove", "--force", &ps]).await;
    let _ = std::fs::remove_dir_all(path);
    let _ = ok(repo, &["worktree", "prune"]).await;
    Ok(())
}

/// Everything the branch changed since it left `base`, including uncommitted work, as a unified diff.
pub async fn diff(worktree: &Path, base: &str) -> Result<String> {
    let mb = ok(worktree, &["merge-base", base, "HEAD"]).await.unwrap_or_else(|_| base.to_string());
    // stage intent-to-add for new files so they show up, without committing anything
    let _ = ok(worktree, &["add", "-N", "."]).await;
    let d = ok(worktree, &["diff", "--no-ext-diff", "--stat", "--patch", &mb]).await?;
    Ok(d)
}

pub async fn changed_files(worktree: &Path, base: &str) -> Vec<String> {
    let mb = ok(worktree, &["merge-base", base, "HEAD"]).await.unwrap_or_else(|_| base.to_string());
    let _ = ok(worktree, &["add", "-N", "."]).await;
    ok(worktree, &["diff", "--name-status", &mb]).await.map(|s| s.lines().map(str::to_string).collect()).unwrap_or_default()
}

pub async fn ensure_identity(dir: &Path, name: &str, email: &str) {
    let have = ok(dir, &["config", "user.email"]).await.unwrap_or_default();
    if have.is_empty() {
        let n = if name.is_empty() { "Model Roamer" } else { name };
        let e = if email.is_empty() { "model-roamer@localhost" } else { email };
        let _ = ok(dir, &["config", "user.name", n]).await;
        let _ = ok(dir, &["config", "user.email", e]).await;
    }
}

/// Commit everything in the worktree. Returns the new commit, or None when there was nothing to commit.
pub async fn commit_all(dir: &Path, message: &str) -> Result<Option<String>> {
    ok(dir, &["add", "-A"]).await?;
    if ok(dir, &["diff", "--cached", "--quiet"]).await.is_ok() {
        return Ok(None);
    }
    ok(dir, &["commit", "-m", message, "--no-verify"]).await?;
    Ok(Some(ok(dir, &["rev-parse", "--short", "HEAD"]).await?))
}

/// Merge `branch` into the branch checked out in `repo` (the project folder). Aborts cleanly on conflicts.
pub async fn merge(repo: &Path, branch: &str, message: &str, squash: bool) -> Result<String> {
    if !is_clean(repo).await {
        bail!("The project folder has uncommitted changes, so nothing was merged. Commit or stash them first.");
    }
    let o = if squash {
        run_in(repo, &["merge", "--squash", branch], None, 120).await?
    } else {
        run_in(repo, &["merge", "--no-ff", "-m", message, branch], None, 120).await?
    };
    if !o.ok() {
        let conflicts = ok(repo, &["diff", "--name-only", "--diff-filter=U"]).await.unwrap_or_default();
        let _ = run_in(repo, &["merge", "--abort"], None, 60).await;
        let _ = run_in(repo, &["reset", "--merge"], None, 60).await;
        if !conflicts.is_empty() {
            bail!("Merging {branch} conflicts in:\n{conflicts}\nNothing was changed. Ask the thread to rebase onto the base branch and resolve them.");
        }
        bail!("Merge failed: {}", o.text().trim());
    }
    if squash {
        ok(repo, &["commit", "-m", message, "--no-verify"]).await?;
    }
    ok(repo, &["rev-parse", "--short", "HEAD"]).await
}

pub async fn push(dir: &Path, branch: &str, token: Option<&str>) -> Result<()> {
    let o = run_in(dir, &["push", "-u", "origin", branch], token, 300).await?;
    if !o.ok() {
        bail!("Push failed: {}", o.text().trim());
    }
    Ok(())
}

pub async fn clone(url: &str, into: &Path, token: Option<&str>) -> Result<()> {
    if let Some(p) = into.parent() {
        std::fs::create_dir_all(p)?;
    }
    let parent = into.parent().unwrap_or(Path::new("."));
    let o = run_in(parent, &["clone", url, &into.display().to_string()], token, 1800).await?;
    if !o.ok() {
        bail!("Clone failed: {}", o.text().trim());
    }
    Ok(())
}

pub async fn default_branch(dir: &Path) -> String {
    if let Ok(s) = ok(dir, &["symbolic-ref", "--short", "refs/remotes/origin/HEAD"]).await {
        if let Some(b) = s.strip_prefix("origin/") {
            return b.to_string();
        }
    }
    current_branch(dir).await.unwrap_or_else(|_| "main".into())
}

pub async fn log(dir: &Path, n: usize) -> Vec<String> {
    ok(dir, &["log", &format!("-{n}"), "--pretty=format:%h\x1f%an\x1f%ar\x1f%s"]).await.map(|s| s.lines().map(str::to_string).collect()).unwrap_or_default()
}

pub async fn status(dir: &Path) -> String {
    ok(dir, &["status", "--short", "--branch"]).await.unwrap_or_default()
}

/// "https://github.com/a/b.git" or "git@github.com:a/b.git" -> ("a", "b")
pub fn github_repo(url: &str) -> Option<(String, String)> {
    let re = regex::Regex::new(r"github\.com[:/]([^/\s]+)/([^/\s]+?)(?:\.git)?/?$").unwrap();
    let c = re.captures(url.trim())?;
    Some((c[1].to_string(), c[2].to_string()))
}

pub fn worktree_path(data: &Path, project: &str, thread: &str) -> PathBuf {
    data.join("worktrees").join(project).join(thread)
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn parses_github() {
        assert_eq!(github_repo("https://github.com/Tox-EyE-CeaZY/Model-Roamer.git"), Some(("Tox-EyE-CeaZY".into(), "Model-Roamer".into())));
        assert_eq!(github_repo("git@github.com:a/b"), Some(("a".into(), "b".into())));
        assert_eq!(github_repo("https://gitlab.com/a/b"), None);
    }

    #[tokio::test]
    async fn worktree_flow() {
        let dir = std::env::temp_dir().join(format!("mr-git-{}", crate::util::rand_str(6)));
        std::fs::create_dir_all(&dir).unwrap();
        ok(&dir, &["init", "-b", "main"]).await.unwrap();
        ensure_identity(&dir, "T", "t@t").await;
        std::fs::write(dir.join("a.txt"), "one\n").unwrap();
        commit_all(&dir, "first").await.unwrap();
        let wt = dir.join("wt");
        add_worktree(&dir, &wt, "mr/task", "main").await.unwrap();
        std::fs::write(wt.join("b.txt"), "two\n").unwrap();
        let d = diff(&wt, "main").await.unwrap();
        assert!(d.contains("b.txt"), "{d}");
        ensure_identity(&wt, "T", "t@t").await;
        commit_all(&wt, "add b").await.unwrap();
        std::fs::write(dir.join(".gitignore"), "wt\n").unwrap();
        commit_all(&dir, "ignore").await.unwrap();
        merge(&dir, "mr/task", "merge task", false).await.unwrap();
        assert!(dir.join("b.txt").exists());
        remove_worktree(&dir, &wt).await.unwrap();
        let _ = std::fs::remove_dir_all(&dir);
    }
}
