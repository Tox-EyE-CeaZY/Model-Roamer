//! Updating from GitHub releases (the same scheme as Web Roamer): find the newest release, download this system's
//! file, check it against the release's SHA256SUMS.txt, swap it in for the running program and start it again.
//! A running exe can't be overwritten on Windows, but it can be renamed, so the old one steps aside as *.old.

use futures_util::StreamExt;
use serde_json::{json, Value};
use sha2::{Digest, Sha256};
use std::{io::Write, path::PathBuf};

const LATEST: &str = "https://api.github.com/repos/Tox-EyE-CeaZY/Model-Roamer/releases/latest";

pub fn asset_name() -> Option<&'static str> {
    if cfg!(all(windows, target_arch = "x86_64")) {
        Some("model-roamer-windows-x64.exe")
    } else if cfg!(all(windows, target_arch = "aarch64")) {
        Some("model-roamer-windows-arm64.exe")
    } else if cfg!(all(target_os = "linux", target_arch = "x86_64", feature = "desktop")) {
        Some("model-roamer-linux-x64")
    } else if cfg!(all(target_os = "linux", target_arch = "x86_64")) {
        Some("model-roamer-server-linux-x64")
    } else if cfg!(all(target_os = "linux", target_arch = "aarch64")) {
        Some("model-roamer-server-linux-arm64")
    } else {
        None // macOS ships as an app bundle in a zip: that one is opened from the release page instead
    }
}

fn newer(tag: &str) -> bool {
    let parse = |s: &str| s.trim_start_matches('v').split('.').map(|p| p.parse::<u64>().unwrap_or(0)).collect::<Vec<_>>();
    parse(tag) > parse(env!("CARGO_PKG_VERSION"))
}

pub async fn check() -> Value {
    match crate::github::http().get(LATEST).header("Accept", "application/vnd.github+json").send().await {
        Ok(r) if r.status().is_success() => {
            let v: Value = r.json().await.unwrap_or_default();
            let tag = v["tag_name"].as_str().unwrap_or("").to_string();
            json!({ "current": env!("CARGO_PKG_VERSION"), "latest": tag, "newer": newer(&tag), "notes": v["body"], "url": v["html_url"], "can_self_update": asset_name().is_some(), "staged": staged_tag() })
        }
        Ok(r) if r.status().as_u16() == 404 => json!({ "current": env!("CARGO_PKG_VERSION"), "latest": null, "newer": false, "note": "No releases yet." }),
        Ok(r) => json!({ "current": env!("CARGO_PKG_VERSION"), "error": format!("GitHub said {}", r.status()) }),
        Err(e) => json!({ "current": env!("CARGO_PKG_VERSION"), "error": format!("Couldn't reach GitHub: {e}") }),
    }
}

fn staged() -> PathBuf {
    crate::os::data_dir().join("update").join(asset_name().unwrap_or("model-roamer-new"))
}

pub fn staged_tag() -> Option<String> {
    staged().is_file().then(|| std::fs::read_to_string(staged().with_extension("tag")).ok()).flatten().map(|s| s.trim().to_string())
}

pub async fn download(report: impl Fn(Value)) {
    let fail = |e: String| report(json!({ "state": "error", "error": e }));
    let Some(name) = asset_name() else { return fail("This system updates from the release page".into()) };
    let http = crate::github::http();
    let rel: Value = match http.get(LATEST).header("Accept", "application/vnd.github+json").send().await {
        Ok(r) => r.json().await.unwrap_or_default(),
        Err(_) => return fail("Couldn't reach GitHub".into()),
    };
    let tag = rel["tag_name"].as_str().unwrap_or("").to_string();
    let assets = rel["assets"].as_array().cloned().unwrap_or_default();
    let url_of = |n: &str| assets.iter().find(|a| a["name"].as_str() == Some(n)).and_then(|a| a["browser_download_url"].as_str()).map(str::to_string);
    let Some(url) = url_of(name) else { return fail(format!("Release {tag} has no {name}")) };
    let sums = match url_of("SHA256SUMS.txt") {
        Some(u) => http.get(&u).send().await.ok().map(|r| r.text()),
        None => None,
    };
    let sums = match sums {
        Some(f) => f.await.unwrap_or_default(),
        None => String::new(),
    };
    let Some(want) = sums.lines().find(|l| l.trim_end().ends_with(name)).and_then(|l| l.split_whitespace().next()).map(str::to_lowercase) else {
        return fail("The release has no checksum for this file".into());
    };
    let resp = match reqwest::Client::builder().connect_timeout(std::time::Duration::from_secs(15)).build().unwrap_or_default().get(&url).send().await {
        Ok(r) if r.status().is_success() => r,
        Ok(r) => return fail(format!("Download failed: {}", r.status())),
        Err(e) => return fail(format!("Download failed: {e}")),
    };
    let total = resp.content_length().unwrap_or(0);
    let path = staged();
    let _ = std::fs::create_dir_all(path.parent().unwrap());
    let part = path.with_extension("part");
    let Ok(mut out) = std::fs::File::create(&part) else { return fail("Couldn't write the download".into()) };
    let (mut got, mut hash, mut last) = (0u64, Sha256::new(), 0u64);
    let mut body = resp.bytes_stream();
    while let Some(chunk) = body.next().await {
        let Ok(chunk) = chunk else { return fail("The download was cut off".into()) };
        if out.write_all(&chunk).is_err() {
            return fail("Couldn't write the download".into());
        }
        hash.update(&chunk);
        got += chunk.len() as u64;
        if total > 0 && got * 100 / total != last {
            last = got * 100 / total;
            report(json!({ "state": "downloading", "pct": last, "tag": tag }));
        }
    }
    drop(out);
    let have = crate::util::hex(&hash.finalize());
    if have != want {
        let _ = std::fs::remove_file(&part);
        return fail("The download didn't match the release's checksum, so it was thrown away".into());
    }
    if std::fs::rename(&part, &path).is_err() {
        return fail("Couldn't keep the download".into());
    }
    let _ = std::fs::write(path.with_extension("tag"), &tag);
    report(json!({ "state": "ready", "tag": tag }));
}

/// Put the downloaded program in place and start it. Returns false (changing nothing) when it can't.
pub fn apply() -> bool {
    let (Ok(exe), new) = (std::env::current_exe(), staged()) else { return false };
    if !new.is_file() {
        return false;
    }
    let old = exe.with_extension("old");
    let _ = std::fs::remove_file(&old);
    if std::fs::rename(&exe, &old).is_err() {
        return false;
    }
    if std::fs::copy(&new, &exe).is_err() {
        let _ = std::fs::rename(&old, &exe);
        return false;
    }
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let _ = std::fs::set_permissions(&exe, std::fs::Permissions::from_mode(0o755));
    }
    let _ = std::fs::remove_file(&new);
    let _ = std::fs::remove_file(new.with_extension("tag"));
    let mut cmd = std::process::Command::new(&exe);
    cmd.args(std::env::args().skip(1));
    crate::os::quiet_std(&mut cmd).spawn().is_ok()
}

pub fn tidy() {
    if let Ok(exe) = std::env::current_exe() {
        let _ = std::fs::remove_file(exe.with_extension("old"));
    }
}
