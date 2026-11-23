//! What differs between Windows, macOS and Linux: where files live, how to start a child without a console window,
//! and how to find a program the user installed a moment ago (a fresh install updates PATH in the registry, not in
//! this process).

use std::{
    env,
    path::{Path, PathBuf},
};

pub fn home() -> PathBuf {
    env::var_os("USERPROFILE").or_else(|| env::var_os("HOME")).map(PathBuf::from).unwrap_or_default()
}

/// The app's own data folder. `MODEL_ROAMER_DATA` overrides it (handy for a second server on one machine, or tests).
pub fn data_dir() -> PathBuf {
    if let Some(d) = env::var_os("MODEL_ROAMER_DATA") {
        let d = PathBuf::from(d);
        let _ = std::fs::create_dir_all(&d);
        return d;
    }
    #[cfg(windows)]
    let base = env::var_os("LOCALAPPDATA").map(PathBuf::from);
    #[cfg(target_os = "macos")]
    let base = Some(home().join("Library").join("Application Support"));
    #[cfg(all(unix, not(target_os = "macos")))]
    let base = env::var_os("XDG_DATA_HOME").map(PathBuf::from).or_else(|| Some(home().join(".local").join("share")));
    let d = base.unwrap_or_else(env::temp_dir).join("ModelRoamer");
    let _ = std::fs::create_dir_all(&d);
    d
}

pub fn os_name() -> &'static str {
    if cfg!(windows) {
        "windows"
    } else if cfg!(target_os = "macos") {
        "macos"
    } else {
        "linux"
    }
}

/// Keep a child from flashing a console window (Windows only).
pub fn quiet_std(cmd: &mut std::process::Command) -> &mut std::process::Command {
    #[cfg(windows)]
    {
        use std::os::windows::process::CommandExt;
        cmd.creation_flags(0x0800_0000); // CREATE_NO_WINDOW
    }
    cmd
}

pub fn quiet(cmd: &mut tokio::process::Command) -> &mut tokio::process::Command {
    #[cfg(windows)]
    {
        cmd.creation_flags(0x0800_0000);
    }
    cmd
}

/// Re-read PATH the way a new terminal would, so programs installed after the app started are found.
pub fn refresh_path() {
    #[cfg(windows)]
    {
        use winreg::{enums::*, RegKey};
        let read = |root: RegKey, sub: &str| -> Option<String> { root.open_subkey(sub).ok()?.get_value::<String, _>("Path").ok() };
        let machine = read(RegKey::predef(HKEY_LOCAL_MACHINE), r"SYSTEM\CurrentControlSet\Control\Session Manager\Environment");
        let user = read(RegKey::predef(HKEY_CURRENT_USER), "Environment");
        let mut parts: Vec<String> = Vec::new();
        for p in [machine, user].into_iter().flatten() {
            for seg in p.split(';') {
                let seg = expand_env(seg.trim());
                if !seg.is_empty() && !parts.iter().any(|x| x.eq_ignore_ascii_case(&seg)) {
                    parts.push(seg);
                }
            }
        }
        for extra in extra_dirs() {
            let s = extra.to_string_lossy().to_string();
            if !parts.iter().any(|x| x.eq_ignore_ascii_case(&s)) {
                parts.push(s);
            }
        }
        if !parts.is_empty() {
            env::set_var("PATH", parts.join(";"));
        }
    }
    #[cfg(not(windows))]
    {
        let cur = env::var("PATH").unwrap_or_default();
        let mut parts: Vec<String> = cur.split(':').filter(|s| !s.is_empty()).map(str::to_string).collect();
        for extra in extra_dirs() {
            let s = extra.to_string_lossy().to_string();
            if !parts.contains(&s) {
                parts.push(s);
            }
        }
        env::set_var("PATH", parts.join(":"));
    }
}

/// Folders the agent CLIs' installers use that are often missing from PATH (a GUI app on macOS starts with a bare PATH).
fn extra_dirs() -> Vec<PathBuf> {
    let h = home();
    let mut v = vec![h.join(".local").join("bin"), h.join(".claude").join("local"), h.join(".npm-global").join("bin"), h.join(".bun").join("bin")];
    #[cfg(windows)]
    {
        if let Some(a) = env::var_os("APPDATA") {
            v.push(PathBuf::from(a).join("npm"));
        }
        if let Some(l) = env::var_os("LOCALAPPDATA") {
            let l = PathBuf::from(l);
            v.push(l.join("Microsoft").join("WinGet").join("Links"));
            v.push(l.join("Programs").join("Antigravity").join("bin"));
        }
        v.push(PathBuf::from(r"C:\Program Files\nodejs"));
        v.push(PathBuf::from(r"C:\Program Files\Git\cmd"));
        v.push(PathBuf::from(r"C:\Program Files\GitHub CLI"));
        v.push(PathBuf::from(r"C:\Program Files\Tailscale"));
    }
    #[cfg(not(windows))]
    {
        v.push(PathBuf::from("/usr/local/bin"));
        v.push(PathBuf::from("/opt/homebrew/bin"));
        if let Ok(rd) = std::fs::read_dir(h.join(".nvm").join("versions").join("node")) {
            for e in rd.flatten() {
                v.push(e.path().join("bin"));
            }
        }
    }
    v.into_iter().filter(|p| p.is_dir()).collect()
}

#[cfg(windows)]
fn expand_env(s: &str) -> String {
    let re = regex::Regex::new(r"%([^%]+)%").unwrap();
    re.replace_all(s, |c: &regex::Captures| env::var(&c[1]).unwrap_or_else(|_| c[0].to_string())).into_owned()
}

/// Open a URL or file with the system's default handler.
pub fn open_external(target: &str) -> bool {
    open::that_detached(target).is_ok()
}

/// Show a folder in the system file manager.
pub fn reveal(path: &Path) -> bool {
    path.exists() && open::that_detached(path).is_ok()
}

pub fn machine_name() -> String {
    hostname::get().ok().and_then(|h| h.into_string().ok()).unwrap_or_else(|| "this computer".into())
}

/// Write a file atomically: a crash mid-write never leaves half a file behind.
pub fn write_atomic(path: &Path, data: &[u8]) -> std::io::Result<()> {
    if let Some(p) = path.parent() {
        std::fs::create_dir_all(p)?;
    }
    let tmp = path.with_extension(format!("tmp{}", std::process::id()));
    std::fs::write(&tmp, data)?;
    std::fs::rename(&tmp, path)
}
