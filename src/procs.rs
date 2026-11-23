//! Running other programs reliably: finding them (npm's .cmd shims on Windows are unwrapped to `node script.js` so
//! long, multi-line prompts never pass through cmd.exe), running them with timeouts, streaming their output line by
//! line, running them inside a pseudo-terminal (ConPTY on Windows) when they need one, and killing the whole process
//! tree on cancel.

use anyhow::{anyhow, Context, Result};
use std::{
    io::{Read, Write},
    path::{Path, PathBuf},
    process::Stdio,
    sync::{Arc, Mutex},
    time::Duration,
};
use tokio::{
    io::{AsyncBufReadExt, AsyncWriteExt, BufReader},
    sync::mpsc,
};

/// A program ready to start: `program prefix... args...`.
#[derive(Clone, Debug, serde::Serialize)]
pub struct Resolved {
    pub program: PathBuf,
    pub prefix: Vec<String>,
    /// What the user would recognize (the path they installed).
    pub shown: String,
}

#[derive(Clone, Debug, Default)]
pub struct Launch {
    pub args: Vec<String>,
    pub env: Vec<(String, String)>,
    pub stdin: Option<String>,
    pub cwd: Option<PathBuf>,
    pub pty: bool,
}

/// Find a program by any of its names (or an explicit path the user set).
pub fn resolve(names: &[&str], explicit: Option<&str>) -> Option<Resolved> {
    if let Some(p) = explicit.map(str::trim).filter(|s| !s.is_empty()) {
        let pb = PathBuf::from(p);
        if pb.is_file() {
            return Some(unwrap_shim(pb));
        }
        if let Ok(found) = which::which(p) {
            return Some(unwrap_shim(found));
        }
    }
    for n in names {
        if let Ok(found) = which::which(n) {
            return Some(unwrap_shim(found));
        }
    }
    None
}

/// npm installs `foo.cmd` on Windows; running it means going through cmd.exe, which mangles quotes and newlines.
/// The shim names the script it runs, so run that with node directly instead.
fn unwrap_shim(path: PathBuf) -> Resolved {
    let shown = path.display().to_string();
    let ext = path.extension().and_then(|e| e.to_str()).unwrap_or("").to_ascii_lowercase();
    if ext == "cmd" || ext == "bat" {
        if let Ok(text) = std::fs::read_to_string(&path) {
            let re = regex::Regex::new(r#"%(?:~?dp0|_prog_dir)%\\([^"%]+?\.(?:js|mjs|cjs))"#).unwrap();
            if let Some(c) = re.captures(&text) {
                let dir = path.parent().unwrap_or(Path::new("."));
                let script = dir.join(c[1].replace('\\', std::path::MAIN_SEPARATOR_STR));
                if script.is_file() {
                    let local_node = dir.join("node.exe");
                    let node = if local_node.is_file() { Some(local_node) } else { which::which("node").ok() };
                    if let Some(node) = node {
                        return Resolved { program: node, prefix: vec![script.display().to_string()], shown };
                    }
                }
            }
            // a native exe next to the shim (some packages ship one)
            let re2 = regex::Regex::new(r#"%(?:~?dp0)%\\([^"%]+?\.exe)"#).unwrap();
            if let Some(c) = re2.captures(&text) {
                let exe = path.parent().unwrap_or(Path::new(".")).join(&c[1]);
                if exe.is_file() {
                    return Resolved { program: exe, prefix: vec![], shown };
                }
            }
        }
        return Resolved { program: PathBuf::from("cmd"), prefix: vec!["/D".into(), "/C".into(), path.display().to_string()], shown };
    }
    if ext == "ps1" {
        return Resolved {
            program: PathBuf::from("powershell"),
            prefix: vec!["-NoProfile".into(), "-ExecutionPolicy".into(), "Bypass".into(), "-File".into(), path.display().to_string()],
            shown,
        };
    }
    Resolved { program: path, prefix: vec![], shown }
}

pub struct Output {
    pub code: Option<i32>,
    pub stdout: String,
    pub stderr: String,
    pub timed_out: bool,
}

impl Output {
    pub fn ok(&self) -> bool {
        self.code == Some(0) && !self.timed_out
    }
    pub fn text(&self) -> String {
        format!("{}{}", self.stdout, self.stderr)
    }
}

fn command(r: &Resolved, l: &Launch) -> tokio::process::Command {
    let mut c = tokio::process::Command::new(&r.program);
    c.args(&r.prefix).args(&l.args);
    for (k, v) in &l.env {
        c.env(k, v);
    }
    if let Some(d) = &l.cwd {
        c.current_dir(d);
    }
    crate::os::quiet(&mut c);
    #[cfg(unix)]
    c.process_group(0);
    c.kill_on_drop(true);
    c
}

/// Run to completion and capture the output, giving up after `timeout`.
pub async fn run(r: &Resolved, l: &Launch, timeout: Duration) -> Result<Output> {
    let mut c = command(r, l);
    c.stdin(if l.stdin.is_some() { Stdio::piped() } else { Stdio::null() }).stdout(Stdio::piped()).stderr(Stdio::piped());
    let mut child = c.spawn().with_context(|| format!("starting {}", r.shown))?;
    if let (Some(s), Some(mut si)) = (l.stdin.clone(), child.stdin.take()) {
        tokio::spawn(async move {
            let _ = si.write_all(s.as_bytes()).await;
        });
    }
    let pid = child.id();
    let mut so = child.stdout.take().unwrap();
    let mut se = child.stderr.take().unwrap();
    let read = async {
        use tokio::io::AsyncReadExt;
        let (mut a, mut b) = (Vec::new(), Vec::new());
        let _ = tokio::join!(so.read_to_end(&mut a), se.read_to_end(&mut b));
        let st = child.wait().await;
        (a, b, st)
    };
    match tokio::time::timeout(timeout, read).await {
        Ok((a, b, st)) => Ok(Output {
            code: st.ok().and_then(|s| s.code()),
            stdout: String::from_utf8_lossy(&a).into_owned(),
            stderr: String::from_utf8_lossy(&b).into_owned(),
            timed_out: false,
        }),
        Err(_) => {
            if let Some(p) = pid {
                kill_tree(p).await;
            }
            Ok(Output { code: None, stdout: String::new(), stderr: format!("{} did not finish within {}s", r.shown, timeout.as_secs()), timed_out: true })
        }
    }
}

/// Kill a process and everything it started.
pub async fn kill_tree(pid: u32) {
    #[cfg(windows)]
    {
        let mut c = tokio::process::Command::new("taskkill");
        c.args(["/T", "/F", "/PID", &pid.to_string()]).stdout(Stdio::null()).stderr(Stdio::null());
        crate::os::quiet(&mut c);
        let _ = c.status().await;
    }
    #[cfg(unix)]
    {
        // Children start in their own process group (see `command`), whose id is the child's pid. Signal the group
        // directly: the `kill` program parses "-<pid>" differently between systems, and once signalled the wrong group.
        let Ok(pg) = libc::pid_t::try_from(pid) else { return };
        if pg <= 1 {
            return;
        }
        // SAFETY: killpg only sends a signal; a group that has already exited just returns ESRCH.
        if unsafe { libc::killpg(pg, libc::SIGTERM) } != 0 {
            return; // the group is gone already
        }
        tokio::time::sleep(Duration::from_millis(1500)).await;
        unsafe { libc::killpg(pg, libc::SIGKILL) };
    }
}

/// One line of output from a streaming child.
#[derive(Debug)]
pub enum Line {
    Out(String),
    Err(String),
    Exit(Option<i32>),
}

/// A running child whose output arrives line by line.
pub struct Stream {
    pub rx: mpsc::Receiver<Line>,
    pid: Option<u32>,
    pty_killer: Option<Arc<Mutex<Box<dyn portable_pty::ChildKiller + Send + Sync>>>>,
}

impl Stream {
    pub async fn kill(&mut self) {
        if let Some(k) = &self.pty_killer {
            let _ = k.lock().map(|mut k| k.kill());
        }
        if let Some(p) = self.pid {
            kill_tree(p).await;
        }
    }
}

/// Start a child and stream its output. With `l.pty` it runs inside a pseudo-terminal (ConPTY on Windows): some CLIs
/// print nothing, or hang, without a terminal.
pub fn stream(r: &Resolved, l: &Launch) -> Result<Stream> {
    if l.pty {
        return stream_pty(r, l);
    }
    let mut c = command(r, l);
    c.stdin(if l.stdin.is_some() { Stdio::piped() } else { Stdio::null() }).stdout(Stdio::piped()).stderr(Stdio::piped());
    let mut child = c.spawn().with_context(|| format!("starting {}", r.shown))?;
    let pid = child.id();
    if let (Some(s), Some(mut si)) = (l.stdin.clone(), child.stdin.take()) {
        tokio::spawn(async move {
            let _ = si.write_all(s.as_bytes()).await;
            let _ = si.shutdown().await;
        });
    }
    let (tx, rx) = mpsc::channel(1024);
    let so = child.stdout.take().unwrap();
    let se = child.stderr.take().unwrap();
    let t1 = tx.clone();
    let a = tokio::spawn(async move {
        let mut rd = BufReader::new(so);
        let mut buf = Vec::new();
        loop {
            buf.clear();
            match rd.read_until(b'\n', &mut buf).await {
                Ok(0) | Err(_) => break,
                Ok(_) => {
                    let s = String::from_utf8_lossy(&buf).trim_end_matches(['\r', '\n']).to_string();
                    if t1.send(Line::Out(s)).await.is_err() {
                        break;
                    }
                }
            }
        }
    });
    let t2 = tx.clone();
    let b = tokio::spawn(async move {
        let mut rd = BufReader::new(se).lines();
        while let Ok(Some(l)) = rd.next_line().await {
            if t2.send(Line::Err(l)).await.is_err() {
                break;
            }
        }
    });
    tokio::spawn(async move {
        let _ = tokio::join!(a, b);
        let st = child.wait().await.ok().and_then(|s| s.code());
        let _ = tx.send(Line::Exit(st)).await;
    });
    Ok(Stream { rx, pid, pty_killer: None })
}

/// Remove terminal escape sequences (colors, cursor moves, titles) from PTY output.
pub fn strip_ansi(s: &str) -> String {
    static RE: std::sync::OnceLock<regex::Regex> = std::sync::OnceLock::new();
    let re = RE.get_or_init(|| regex::Regex::new(r"\x1b\[[0-?]*[ -/]*[@-~]|\x1b\][^\x07\x1b]*(\x07|\x1b\\)|\x1b[@-Z\\-_]|[\x00-\x08\x0b\x0c\x0e-\x1f]").unwrap());
    re.replace_all(s, "").into_owned()
}

fn pty_command(r: &Resolved, l: &Launch) -> portable_pty::CommandBuilder {
    let mut cb = portable_pty::CommandBuilder::new(&r.program);
    for a in r.prefix.iter().chain(l.args.iter()) {
        cb.arg(a);
    }
    for (k, v) in std::env::vars() {
        cb.env(k, v);
    }
    for (k, v) in &l.env {
        cb.env(k, v);
    }
    cb.env("TERM", "xterm-256color");
    if let Some(d) = &l.cwd {
        cb.cwd(d);
    }
    cb
}

fn stream_pty(r: &Resolved, l: &Launch) -> Result<Stream> {
    use portable_pty::{native_pty_system, PtySize};
    let pair = native_pty_system()
        .openpty(PtySize { rows: 50, cols: 400, pixel_width: 0, pixel_height: 0 })
        .map_err(|e| anyhow!("could not open a pseudo-terminal: {e}"))?;
    let mut child = pair.slave.spawn_command(pty_command(r, l)).map_err(|e| anyhow!("starting {}: {e}", r.shown))?;
    drop(pair.slave);
    let pid = child.process_id();
    let killer = Arc::new(Mutex::new(child.clone_killer()));
    let mut reader = pair.master.try_clone_reader().map_err(|e| anyhow!("{e}"))?;
    let mut writer = pair.master.take_writer().map_err(|e| anyhow!("{e}"))?;
    if let Some(s) = l.stdin.clone() {
        std::thread::spawn(move || {
            let _ = writer.write_all(s.as_bytes());
            let _ = writer.write_all(b"\r\x04");
        });
    }
    let (tx, rx) = mpsc::channel(1024);
    let t1 = tx.clone();
    std::thread::spawn(move || {
        let mut buf = [0u8; 8192];
        let mut pending = String::new();
        loop {
            match reader.read(&mut buf) {
                Ok(0) | Err(_) => break,
                Ok(n) => {
                    pending.push_str(&String::from_utf8_lossy(&buf[..n]));
                    while let Some(i) = pending.find('\n') {
                        let line = strip_ansi(pending[..i].trim_end_matches('\r'));
                        pending.drain(..=i);
                        if t1.blocking_send(Line::Out(line)).is_err() {
                            return;
                        }
                    }
                }
            }
        }
        if !pending.trim().is_empty() {
            let _ = t1.blocking_send(Line::Out(strip_ansi(&pending)));
        }
    });
    let master = pair.master;
    std::thread::spawn(move || {
        let st = child.wait().ok().map(|s| s.exit_code() as i32);
        drop(master); // closing the terminal ends the reader
        let _ = tx.blocking_send(Line::Exit(st));
    });
    Ok(Stream { rx, pid, pty_killer: Some(killer) })
}

/// An interactive terminal (installs, sign-ins): raw bytes out, keystrokes in, resizable.
pub struct Term {
    pub writer: Mutex<Box<dyn Write + Send>>,
    pub master: Mutex<Box<dyn portable_pty::MasterPty + Send>>,
    killer: Mutex<Box<dyn portable_pty::ChildKiller + Send + Sync>>,
}

impl Term {
    pub fn write(&self, data: &[u8]) {
        if let Ok(mut w) = self.writer.lock() {
            let _ = w.write_all(data);
            let _ = w.flush();
        }
    }
    pub fn resize(&self, cols: u16, rows: u16) {
        if let Ok(m) = self.master.lock() {
            let _ = m.resize(portable_pty::PtySize { rows: rows.max(5), cols: cols.max(20), pixel_width: 0, pixel_height: 0 });
        }
    }
    pub fn kill(&self) {
        if let Ok(mut k) = self.killer.lock() {
            let _ = k.kill();
        }
    }
}

/// Start an interactive terminal. `on_data` gets raw output; `on_exit` the exit code.
pub fn terminal(
    r: &Resolved,
    l: &Launch,
    cols: u16,
    rows: u16,
    on_data: impl Fn(Vec<u8>) + Send + 'static,
    on_exit: impl FnOnce(Option<i32>) + Send + 'static,
) -> Result<Arc<Term>> {
    use portable_pty::{native_pty_system, PtySize};
    let pair = native_pty_system()
        .openpty(PtySize { rows: rows.max(5), cols: cols.max(20), pixel_width: 0, pixel_height: 0 })
        .map_err(|e| anyhow!("could not open a pseudo-terminal: {e}"))?;
    let mut child = pair.slave.spawn_command(pty_command(r, l)).map_err(|e| anyhow!("starting {}: {e}", r.shown))?;
    drop(pair.slave);
    let mut reader = pair.master.try_clone_reader().map_err(|e| anyhow!("{e}"))?;
    let writer = pair.master.take_writer().map_err(|e| anyhow!("{e}"))?;
    let term = Arc::new(Term { writer: Mutex::new(writer), killer: Mutex::new(child.clone_killer()), master: Mutex::new(pair.master) });
    std::thread::spawn(move || {
        let mut buf = [0u8; 8192];
        loop {
            match reader.read(&mut buf) {
                Ok(0) | Err(_) => break,
                Ok(n) => on_data(buf[..n].to_vec()),
            }
        }
    });
    std::thread::spawn(move || {
        let st = child.wait().ok().map(|s| s.exit_code() as i32);
        on_exit(st);
    });
    Ok(term)
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn strips_escapes() {
        assert_eq!(strip_ansi("\x1b[1;32mhello\x1b[0m world\x1b]0;title\x07"), "hello world");
    }
    #[cfg(unix)]
    #[tokio::test]
    async fn runs_with_timeout() {
        let sh = resolve(&["sh"], None).unwrap();
        let out = run(&sh, &Launch { args: vec!["-c".into(), "echo hi".into()], ..Default::default() }, Duration::from_secs(5)).await.unwrap();
        assert!(out.ok() && out.stdout.trim() == "hi");
        let slow = run(&sh, &Launch { args: vec!["-c".into(), "sleep 5".into()], ..Default::default() }, Duration::from_millis(300)).await.unwrap();
        assert!(slow.timed_out);
        // the test process itself is still here (an earlier kill signalled the wrong group) and its own group is untouched
        assert_eq!(unsafe { libc::kill(std::process::id() as libc::pid_t, 0) }, 0);
    }
    #[tokio::test]
    async fn streams_lines_and_stdin() {
        let sh = resolve(&["sh"], None).unwrap();
        let mut s = stream(&sh, &Launch { args: vec!["-c".into(), "cat; echo done".into()], stdin: Some("a\nb\n".into()), ..Default::default() }).unwrap();
        let mut got = vec![];
        while let Some(l) = s.rx.recv().await {
            match l {
                Line::Out(x) => got.push(x),
                Line::Exit(_) => break,
                _ => {}
            }
        }
        assert_eq!(got, vec!["a", "b", "done"]);
    }
}
