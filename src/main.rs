#![cfg_attr(all(not(debug_assertions), target_os = "windows", feature = "desktop"), windows_subsystem = "windows")]
//! Model Roamer: an AI orchestration studio in the Roamer suite.
//!
//!   model-roamer                  the desktop app (windows), with its own core on this computer
//!   model-roamer server           a headless server for an always-on machine; other devices connect to it
//!   model-roamer pair             make a pairing code on the running server (for a new device)
//!   model-roamer --browser        the app in your web browser instead of its own windows
//!   model-roamer mcp              (used by agents) the bridge to Model Roamer's tools

mod agents;
mod api;
mod artifacts;
mod bus;
mod config;
mod core;
mod db;
#[cfg(feature = "desktop")]
mod desktop;
mod git;
mod github;
mod mcp;
mod memory;
mod net;
mod os;
mod procs;
mod prompts;
mod runner;
mod server;
mod setup;
mod tools;
mod update;
mod util;

use serde_json::json;
use std::{sync::atomic::Ordering, time::Duration};

const LOCAL_PORT: u16 = 4625;

struct Opts {
    cmd: String,
    bind: Option<String>,
    port: Option<u16>,
    name: Option<String>,
    tls_cert: Option<String>,
    tls_key: Option<String>,
    browser: bool,
    no_open: bool,
    path: Option<String>,
}

fn parse_args() -> Opts {
    let mut o = Opts { cmd: String::new(), bind: None, port: None, name: None, tls_cert: None, tls_key: None, browser: false, no_open: false, path: None };
    let mut it = std::env::args().skip(1);
    while let Some(a) = it.next() {
        match a.as_str() {
            "server" | "serve" | "mcp" | "pair" | "help" | "version" => o.cmd = a.replace("serve", "server").replace("serverr", "server"),
            "--help" | "-h" => o.cmd = "help".into(),
            "--version" | "-V" => o.cmd = "version".into(),
            "--bind" => o.bind = it.next(),
            "--port" => o.port = it.next().and_then(|p| p.parse().ok()),
            "--name" => o.name = it.next(),
            "--tls-cert" => o.tls_cert = it.next(),
            "--tls-key" => o.tls_key = it.next(),
            "--data" => {
                if let Some(d) = it.next() {
                    std::env::set_var("MODEL_ROAMER_DATA", d);
                }
            }
            "--browser" => o.browser = true,
            "--no-open" => o.no_open = true,
            "--open" => o.path = it.next(),
            _ => {}
        }
    }
    o
}

const HELP: &str = "Model Roamer — an AI orchestration studio (Roamer suite)

USAGE
  model-roamer                         Open the desktop app
  model-roamer --browser [--no-open]   Run the app in your web browser (http://127.0.0.1:4625)
  model-roamer server [options]        Run headless, as a server other devices connect to
  model-roamer pair                    Print a pairing code from the running server
  model-roamer --version

SERVER OPTIONS
  --bind ADDR        Address to listen on (default 0.0.0.0, every network: LAN, Tailscale, WireGuard)
  --port N           Port (default 4630)
  --name NAME        The name devices see (default: this machine's name)
  --tls-cert FILE    Serve HTTPS with this certificate (PEM) ...
  --tls-key FILE     ... and this key (PEM)
  --data DIR         Keep data here instead of the default folder

Pair a device: open http://<server>:4630 in a browser (or Connect in the desktop app) and enter the code
printed below, or run `model-roamer pair` on the server for a new one.";

fn logging(to_stderr: bool) -> Option<tracing_appender::non_blocking::WorkerGuard> {
    use tracing_subscriber::{fmt, prelude::*, EnvFilter};
    let dir = os::data_dir().join("logs");
    let _ = std::fs::create_dir_all(&dir);
    let file = tracing_appender::rolling::daily(&dir, "model-roamer.log");
    let (nb, guard) = tracing_appender::non_blocking(file);
    let filter = EnvFilter::try_from_env("MODEL_ROAMER_LOG").unwrap_or_else(|_| EnvFilter::new("info,hyper=warn,reqwest=warn,tower=warn"));
    let reg = tracing_subscriber::registry().with(filter).with(fmt::layer().with_writer(nb).with_ansi(false));
    if to_stderr {
        let _ = reg.with(fmt::layer().with_writer(std::io::stderr).with_target(false)).try_init();
    } else {
        let _ = reg.try_init();
    }
    // old turn logs and scratch files don't need to live forever
    let cutoff = std::time::SystemTime::now() - Duration::from_secs(30 * 86_400);
    for sub in ["logs/turns", "turns"] {
        for e in std::fs::read_dir(os::data_dir().join(sub)).into_iter().flatten().flatten() {
            if e.metadata().and_then(|m| m.modified()).map(|m| m < cutoff).unwrap_or(false) {
                let _ = if e.path().is_dir() { std::fs::remove_dir_all(e.path()) } else { std::fs::remove_file(e.path()) };
            }
        }
    }
    Some(guard)
}

/// Is a Model Roamer already running on this computer (same data folder)? Returns its port.
async fn running_instance() -> Option<u16> {
    let port: u16 = std::fs::read_to_string(os::data_dir().join("port")).ok()?.trim().parse().ok()?;
    let id = std::fs::read_to_string(os::data_dir().join("instance")).ok()?;
    let c = reqwest::Client::builder().timeout(Duration::from_millis(1500)).build().ok()?;
    let v: serde_json::Value = c.get(format!("http://127.0.0.1:{port}/api/hello")).send().await.ok()?.json().await.ok()?;
    (v["id"].as_str() == Some(id.trim())).then_some(port)
}

async fn ask_running(port: u16, body: serde_json::Value, path: &str) -> Option<serde_json::Value> {
    let tok = std::fs::read_to_string(os::data_dir().join("local.token")).ok()?;
    let c = reqwest::Client::builder().timeout(Duration::from_secs(5)).build().ok()?;
    c.post(format!("http://127.0.0.1:{port}{path}")).bearer_auth(tok.trim()).json(&body).send().await.ok()?.json().await.ok()
}

/// Open the store, start serving, resume interrupted work.
async fn start(headless: bool, o: &Opts) -> anyhow::Result<core::CoreRef> {
    os::refresh_path();
    update::tidy();
    let core = core::Core::open(os::data_dir(), headless)?;
    if let Some(n) = &o.name {
        core.patch_settings(&json!({ "server": { "name": n } }))?;
    }
    // the suite's design language, shared by every agent, the first time round
    if memory::get_by_name(&core, None, "design-language")?.is_none() {
        let _ = memory::write(&core, None, "design-language", "The Roamer suite's look and feel: follow it for any UI", prompts::DESIGN_NOTE, "Model Roamer");
    }
    let port = server::serve_loopback(core.clone(), if headless { 0 } else { o.port.filter(|_| o.browser).unwrap_or(LOCAL_PORT) }).await?;
    core.port.store(port, Ordering::Relaxed);
    let _ = os::write_atomic(&os::data_dir().join("port"), port.to_string().as_bytes());
    let _ = os::write_atomic(&os::data_dir().join("instance"), core.instance_id.as_bytes());

    let st = core.settings();
    if headless || st["server"]["share"] == true {
        // command-line choices (server mode) are remembered, so the settings page shows what is really in use
        if headless && (o.bind.is_some() || o.port.is_some()) {
            let mut p = json!({ "server": {} });
            if let Some(b) = &o.bind {
                p["server"]["bind"] = json!(b);
            }
            if let Some(port) = o.port {
                p["server"]["port"] = json!(port);
            }
            core.patch_settings(&p)?;
        }
        let tls = match (&o.tls_cert, &o.tls_key) {
            (Some(c), Some(k)) => Some((c.clone(), k.clone())),
            _ => None,
        };
        let over = tls.map(|t| {
            let st = core.settings();
            (st["server"]["bind"].as_str().unwrap_or("0.0.0.0").to_string(), st["server"]["port"].as_u64().unwrap_or(4630) as u16, Some(t))
        });
        if let Err(e) = server::apply_sharing(&core, over.as_ref().map(|(b, p, t)| (b.as_str(), *p, t.clone()))).await {
            tracing::error!("{e:#}");
            eprintln!("Model Roamer can't share: {e:#}");
            if headless {
                return Err(e);
            }
            core.bus.emit("toast", None, None, json!({ "err": true, "text": format!("Sharing is on, but {e}") }));
        }
    }
    runner::recover(&core);
    tokio::spawn(runner::watch_limits(core.clone()));
    // what the last run found about each agent shows at once; only agents whose facts may have changed are checked
    // again, in the background and all at once (so one slow CLI doesn't hold up the others)
    core.load_remembered_status();
    core.repair_raw_messages();
    crate::artifacts::file_unfiled(&core);
    let c = core.clone();
    tokio::spawn(async move {
        tokio::time::sleep(Duration::from_millis(600)).await;
        let ids: Vec<String> = c.adapters().iter().map(|a| a.id()).filter(|id| c.needs_check(id)).collect();
        futures_util::future::join_all(ids.iter().map(|id| c.agent_status(id))).await;
    });
    Ok(core)
}

fn main() {
    let o = parse_args();
    match o.cmd.as_str() {
        "mcp" => std::process::exit(mcp::run_stdio()),
        "help" => {
            println!("{HELP}");
            return;
        }
        "version" => {
            println!("model-roamer {}", env!("CARGO_PKG_VERSION"));
            return;
        }
        _ => {}
    }
    let rt = tokio::runtime::Builder::new_multi_thread().enable_all().thread_name("roamer").build().expect("tokio runtime");

    if o.cmd == "pair" {
        let code = rt.block_on(async {
            let port = running_instance().await?;
            ask_running(port, json!({}), "/api/pairing").await
        });
        match code {
            Some(v) => {
                println!("Pairing code: {}  (valid for 10 minutes, one use)", v["code"].as_str().unwrap_or("?"));
                for u in v["addresses"]["urls"].as_array().cloned().unwrap_or_default().iter().take(6) {
                    println!("  {:<28} {}", u["label"].as_str().unwrap_or(""), u["url"].as_str().unwrap_or(""));
                }
            }
            None => {
                eprintln!("No Model Roamer is running here. Start it with `model-roamer server` first.");
                std::process::exit(1);
            }
        }
        return;
    }

    let headless = o.cmd == "server" || !cfg!(feature = "desktop");
    let _guard = logging(headless || o.browser);
    tracing::info!("Model Roamer {} starting ({})", env!("CARGO_PKG_VERSION"), if headless { "server" } else if o.browser { "browser" } else { "desktop" });

    // a second launch of the desktop app just brings the first one forward
    if !headless {
        if let Some(port) = rt.block_on(running_instance()) {
            if o.browser {
                let _ = os::open_external(&format!("http://127.0.0.1:{port}/"));
            } else {
                rt.block_on(ask_running(port, json!({ "cmd": "open", "path": o.path }), "/api/desktop"));
            }
            return;
        }
    }

    let core = match rt.block_on(start(headless, &o)) {
        Ok(c) => c,
        Err(e) => {
            tracing::error!("couldn't start: {e:#}");
            eprintln!("Model Roamer couldn't start: {e:#}");
            #[cfg(all(windows, feature = "desktop"))]
            fatal_box(&format!("Model Roamer couldn't start:\n\n{e:#}\n\nLogs: {}", os::data_dir().join("logs").display()));
            std::process::exit(1);
        }
    };

    if headless {
        let port = core.settings()["server"]["port"].as_u64().unwrap_or(4630);
        rt.block_on(async {
            let devices = core.db.scalar_i64("SELECT COUNT(*) FROM devices WHERE revoked = 0", &[]).unwrap_or(0);
            let addrs = net::addresses(&core, port as u16, o.tls_cert.is_some()).await;
            println!("\nModel Roamer {} server \"{}\" is running.", env!("CARGO_PKG_VERSION"), core.display_name());
            println!("Data: {}", core.data.display());
            println!("Reach it at:");
            for u in addrs["urls"].as_array().cloned().unwrap_or_default().iter().take(8) {
                println!("  {:<30} {}", u["label"].as_str().unwrap_or(""), u["url"].as_str().unwrap_or(""));
            }
            let code = api::make_pairing(&core);
            println!("\nPairing code: {code}  (valid 10 minutes; `model-roamer pair` makes another)");
            if devices == 0 {
                println!("Open one of the addresses above on your laptop or phone, or use Connect in the desktop app, and enter the code.");
            }
            println!("Sign the agents in from any connected device: Agents › Sign in. Press Ctrl+C to stop.\n");
            let _ = tokio::signal::ctrl_c().await;
            println!("Stopping. Running agents will pick up where they left off next start.");
        });
        return;
    }

    if o.browser || !cfg!(feature = "desktop") {
        let url = format!("{}/auth?token={}&next=/", core.base_url(), core.local_token);
        println!("Model Roamer is running at {}", core.base_url());
        if !o.no_open {
            os::open_external(&url);
        } else {
            println!("Open: {url}");
        }
        rt.block_on(async {
            let _ = tokio::signal::ctrl_c().await;
        });
        return;
    }

    #[cfg(feature = "desktop")]
    {
        let handle = rt.handle().clone();
        desktop::run(core, handle, o.path.clone());
    }
}

#[cfg(all(windows, feature = "desktop"))]
fn fatal_box(msg: &str) {
    // a GUI app has no console: show the reason in a plain message box
    let mut cmd = std::process::Command::new("powershell");
    cmd.args(["-NoProfile", "-Command", &format!("Add-Type -AssemblyName PresentationFramework; [System.Windows.MessageBox]::Show('{}', 'Model Roamer')", msg.replace('\'', "''"))]);
    let _ = os::quiet_std(&mut cmd).status();
}
