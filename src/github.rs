//! GitHub, handled by Model Roamer itself so every agent can open pull requests and read checks, whether or not its
//! own CLI knows GitHub. The token comes from Settings (a personal access token) or from the GitHub CLI's sign-in.

use crate::{core::Core, procs};
use anyhow::{anyhow, bail, Result};
use serde_json::{json, Value};
use std::time::Duration;

pub fn http() -> reqwest::Client {
    reqwest::Client::builder()
        .user_agent(concat!("ModelRoamer/", env!("CARGO_PKG_VERSION")))
        .connect_timeout(Duration::from_secs(10))
        .timeout(Duration::from_secs(60))
        .build()
        .unwrap_or_default()
}

/// The token to use: the one saved in Settings, else the GitHub CLI's.
pub async fn token(core: &Core) -> Option<String> {
    let t = core.settings()["github_token"].as_str().unwrap_or("").trim().to_string();
    if !t.is_empty() {
        return Some(t);
    }
    let gh = procs::resolve(&["gh"], None)?;
    let o = procs::run(&gh, &procs::Launch { args: vec!["auth".into(), "token".into()], ..Default::default() }, Duration::from_secs(15)).await.ok()?;
    let t = o.stdout.trim().to_string();
    (o.ok() && !t.is_empty()).then_some(t)
}

async fn call(core: &Core, method: reqwest::Method, path: &str, body: Option<Value>) -> Result<Value> {
    let tok = token(core).await.ok_or_else(|| anyhow!("GitHub isn't connected. Add a token or sign in with the GitHub CLI in Settings › GitHub."))?;
    let url = if path.starts_with("http") { path.to_string() } else { format!("https://api.github.com{path}") };
    let mut last = String::new();
    for attempt in 0..3 {
        let mut rq = http().request(method.clone(), &url).bearer_auth(&tok).header("Accept", "application/vnd.github+json").header("X-GitHub-Api-Version", "2022-11-28");
        if let Some(b) = &body {
            rq = rq.json(b);
        }
        match rq.send().await {
            Ok(r) => {
                let status = r.status();
                let v: Value = r.json().await.unwrap_or(Value::Null);
                if status.is_success() {
                    return Ok(v);
                }
                let msg = v["message"].as_str().unwrap_or("").to_string();
                let detail = v["errors"].as_array().map(|e| e.iter().map(|x| x["message"].as_str().unwrap_or("").to_string()).collect::<Vec<_>>().join("; ")).unwrap_or_default();
                if status.is_server_error() && attempt < 2 {
                    last = format!("GitHub {status}: {msg}");
                    tokio::time::sleep(Duration::from_millis(800 * (attempt + 1) as u64)).await;
                    continue;
                }
                bail!("GitHub said {status}: {msg} {detail}");
            }
            Err(e) => {
                last = format!("Could not reach GitHub: {e}");
                tokio::time::sleep(Duration::from_millis(800 * (attempt + 1) as u64)).await;
            }
        }
    }
    bail!(last)
}

pub async fn get(core: &Core, path: &str) -> Result<Value> {
    call(core, reqwest::Method::GET, path, None).await
}

pub async fn post(core: &Core, path: &str, body: Value) -> Result<Value> {
    call(core, reqwest::Method::POST, path, Some(body)).await
}

pub async fn put(core: &Core, path: &str, body: Value) -> Result<Value> {
    call(core, reqwest::Method::PUT, path, Some(body)).await
}

pub async fn whoami(core: &Core) -> Value {
    match get(core, "/user").await {
        Ok(u) => json!({ "connected": true, "login": u["login"], "name": u["name"], "avatar": u["avatar_url"] }),
        Err(e) => json!({ "connected": false, "error": e.to_string(), "gh_cli": procs::resolve(&["gh"], None).is_some() }),
    }
}

pub async fn create_pr(core: &Core, owner: &str, repo: &str, head: &str, base: &str, title: &str, body: &str, draft: bool) -> Result<Value> {
    // reuse an open PR for the same branch instead of failing
    let existing = get(core, &format!("/repos/{owner}/{repo}/pulls?state=open&head={owner}:{head}")).await.unwrap_or(Value::Null);
    if let Some(pr) = existing.as_array().and_then(|a| a.first()) {
        return Ok(json!({ "number": pr["number"], "url": pr["html_url"], "existing": true }));
    }
    let pr = post(core, &format!("/repos/{owner}/{repo}/pulls"), json!({ "title": title, "body": body, "head": head, "base": base, "draft": draft })).await?;
    Ok(json!({ "number": pr["number"], "url": pr["html_url"], "existing": false }))
}

pub async fn list_prs(core: &Core, owner: &str, repo: &str, state: &str) -> Result<Value> {
    let v = get(core, &format!("/repos/{owner}/{repo}/pulls?state={state}&per_page=30")).await?;
    Ok(json!(v.as_array().cloned().unwrap_or_default().iter().map(|p| json!({
        "number": p["number"], "title": p["title"], "url": p["html_url"], "branch": p["head"]["ref"], "base": p["base"]["ref"], "state": p["state"], "merged": !p["merged_at"].is_null(),
        "draft": p["draft"], "user": p["user"]["login"], "updated": p["updated_at"]
    })).collect::<Vec<_>>()))
}

pub async fn checks(core: &Core, owner: &str, repo: &str, number: i64) -> Result<Value> {
    let pr = get(core, &format!("/repos/{owner}/{repo}/pulls/{number}")).await?;
    let sha = pr["head"]["sha"].as_str().unwrap_or("");
    let runs = get(core, &format!("/repos/{owner}/{repo}/commits/{sha}/check-runs?per_page=50")).await.unwrap_or(Value::Null);
    let list: Vec<Value> = runs["check_runs"].as_array().cloned().unwrap_or_default().iter().map(|r| json!({ "name": r["name"], "status": r["status"], "conclusion": r["conclusion"], "url": r["html_url"] })).collect();
    Ok(json!({ "number": number, "state": pr["state"], "merged": pr["merged"], "mergeable": pr["mergeable"], "url": pr["html_url"], "checks": list }))
}

pub async fn merge_pr(core: &Core, owner: &str, repo: &str, number: i64, method: &str) -> Result<Value> {
    put(core, &format!("/repos/{owner}/{repo}/pulls/{number}/merge"), json!({ "merge_method": method })).await
}

pub async fn comment(core: &Core, owner: &str, repo: &str, number: i64, body: &str) -> Result<Value> {
    post(core, &format!("/repos/{owner}/{repo}/issues/{number}/comments"), json!({ "body": body })).await
}

pub async fn my_repos(core: &Core) -> Result<Value> {
    let v = get(core, "/user/repos?per_page=100&sort=pushed").await?;
    Ok(json!(v.as_array().cloned().unwrap_or_default().iter().map(|r| json!({
        "full_name": r["full_name"], "url": r["clone_url"], "private": r["private"], "description": r["description"], "pushed": r["pushed_at"], "default_branch": r["default_branch"]
    })).collect::<Vec<_>>()))
}
