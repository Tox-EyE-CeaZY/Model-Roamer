//! Getting a machine ready without a terminal: which tools exist (the agent CLIs plus git, Node.js, the GitHub CLI
//! and Tailscale), and the steps that install them or sign in, run in a terminal panel inside the app.

use crate::{
    agents::{self, shell, step, McpLaunch, Step},
    core::CoreRef,
    procs,
};
use serde_json::{json, Value};

struct Tool {
    id: &'static str,
    name: &'static str,
    why: &'static str,
    bins: &'static [&'static str],
}

const TOOLS: &[Tool] = &[
    Tool { id: "git", name: "Git", why: "Worktrees, commits, merges and pull requests", bins: &["git"] },
    Tool { id: "node", name: "Node.js", why: "Needed to install Codex and Gemini CLI with npm", bins: &["node", "npm"] },
    Tool { id: "gh", name: "GitHub CLI", why: "Optional: sign in to GitHub without making a token", bins: &["gh"] },
    Tool { id: "tailscale", name: "Tailscale", why: "Optional: reach your server from anywhere, privately", bins: &["tailscale"] },
];

pub fn tool_steps(id: &str) -> Vec<Step> {
    let wg = |label: &str, pkg: &str| step(label, &["winget", "install", "-e", "--id", pkg, "--accept-source-agreements", "--accept-package-agreements"], &["winget"]);
    let brew = |label: &str, pkg: &str| step(label, &["brew", "install", pkg], &["brew"]);
    let apt = |label: &str, pkgs: &str| shell(label, "", &format!("sudo apt-get update && sudo apt-get install -y {pkgs}"), &[]);
    match (id, crate::os::os_name()) {
        ("git", "windows") => vec![wg("Install Git", "Git.Git")],
        ("git", "macos") => vec![step("Install Git (Xcode command line tools)", &["xcode-select", "--install"], &[]), brew("Or with Homebrew", "git")],
        ("git", _) => vec![apt("Install Git", "git")],
        ("node", "windows") => vec![wg("Install Node.js LTS", "OpenJS.NodeJS.LTS")],
        ("node", "macos") => vec![brew("Install Node.js", "node")],
        ("node", _) => vec![shell("Install Node.js LTS", "", "curl -fsSL https://deb.nodesource.com/setup_lts.x | sudo -E bash - && sudo apt-get install -y nodejs", &[]), apt("Or the distribution's Node.js", "nodejs npm")],
        ("gh", "windows") => vec![wg("Install the GitHub CLI", "GitHub.cli")],
        ("gh", "macos") => vec![brew("Install the GitHub CLI", "gh")],
        ("gh", _) => vec![apt("Install the GitHub CLI", "gh")],
        ("tailscale", "windows") => vec![wg("Install Tailscale", "Tailscale.Tailscale")],
        ("tailscale", "macos") => vec![brew("Install Tailscale", "tailscale")],
        ("tailscale", _) => vec![shell("Install Tailscale", "", "curl -fsSL https://tailscale.com/install.sh | sh", &[])],
        _ => vec![],
    }
}

pub fn tool_login(id: &str) -> Option<Step> {
    match id {
        "gh" => Some(step("Sign in to GitHub", &["gh", "auth", "login", "--web", "--git-protocol", "https"], &["gh"])),
        "tailscale" => Some(if cfg!(windows) || cfg!(target_os = "macos") { step("Connect Tailscale", &["tailscale", "up"], &["tailscale"]) } else { shell("Connect Tailscale", "", "sudo tailscale up", &[]) }),
        _ => None,
    }
}

pub fn hint_for(need: &str) -> String {
    match need {
        "npm" | "node" => "Install Node.js first (Agents › Tools › Node.js), then try again.".into(),
        "winget" => "winget comes with Windows 10/11 as \"App Installer\" from the Microsoft Store; update it there.".into(),
        "brew" => "Install Homebrew from https://brew.sh first.".into(),
        other => format!("Install {other} first."),
    }
}

/// The step to run for a request from the UI.
pub fn step_for(core: &CoreRef, what: &str, agent: &str, idx: usize) -> Option<Step> {
    match what {
        "tool" => tool_steps(agent).into_iter().nth(idx),
        "tool-login" => tool_login(agent),
        _ => {
            let a = core.adapter(agent)?;
            match what {
                "install" => a.install().into_iter().nth(idx),
                "update" => a.update().into_iter().nth(idx),
                "login" => Some(a.login(core.headless)),
                "logout" => a.logout(),
                "connect" => a.connect_tools(&McpLaunch { command: core.exe.display().to_string(), args: vec!["mcp".into()], env: vec![] }),
                _ => None,
            }
        }
    }
    .map(|mut s| {
        // run the CLI the user configured, if they pointed Settings at a specific one
        if let Some(r) = (what != "tool" && what != "tool-login").then(|| core.locate(agent)).flatten() {
            let names = core.adapter(agent).map(|a| a.binaries()).unwrap_or_default();
            if names.iter().any(|n| n == &s.cmd[0]) {
                s.cmd[0] = r.shown.clone();
                s.needs.clear();
            }
        }
        s
    })
}

pub async fn status(core: &CoreRef) -> Value {
    crate::os::refresh_path();
    let mut tools = vec![];
    for t in TOOLS {
        let found = t.bins.iter().all(|b| procs::resolve(&[b], None).is_some());
        let version = match procs::resolve(&[t.bins[0]], None) {
            Some(r) if found => agents::version(&r).await,
            _ => None,
        };
        tools.push(json!({ "id": t.id, "name": t.name, "why": t.why, "installed": found, "version": version, "install": tool_steps(t.id), "login": tool_login(t.id) }));
    }
    json!({ "tools": tools, "os": crate::os::os_name(), "headless": core.headless, "exe": core.exe.display().to_string() })
}
