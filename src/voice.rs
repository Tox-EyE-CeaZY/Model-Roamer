//! Local, free offline speech-to-text fallback using whisper.cpp (`whisper-cli`).
//! Used when browser Web Speech isn't available (e.g. Windows desktop WebView2 without a Google key).
//! Both the whisper.cpp binary and the speech model are optional, downloaded on first use with
//! progress updates over the event bus, and cached under `<data_dir>/voice/`.

use crate::{
    core::CoreRef,
    server::{bad, R},
};
use axum::{
    body::Bytes,
    extract::{Query, State},
    Json,
};
use futures_util::StreamExt;
use serde_json::{json, Value};
use std::{
    collections::HashMap,
    io::Write,
    path::{Path, PathBuf},
    sync::{Mutex, OnceLock},
    time::Duration,
};

const MODEL_FILE: &str = "ggml-base.bin";
const MODEL_MB: u64 = 141;
const MODEL_URL: &str = "https://huggingface.co/ggerganov/whisper.cpp/resolve/main/ggml-base.bin";
const WHISPER_RELEASES_API: &str = "https://api.github.com/repos/ggml-org/whisper.cpp/releases?per_page=10";
const FALLBACK_RELEASE_TAG: &str = "b5907";
const MAX_WAV_BYTES: usize = 25 * 1024 * 1024;
const MIN_MODEL_BYTES: u64 = 10 * 1024 * 1024;

#[derive(Clone, Debug, Default)]
struct SetupProgress {
    running: bool,
    state: String, // "idle" | "downloading" | "extracting" | "ready" | "error"
    stage: String, // "" | "engine" | "model"
    pct: u64,
    detail: String,
    error: String,
}

fn progress_lock() -> &'static Mutex<SetupProgress> {
    static CELL: OnceLock<Mutex<SetupProgress>> = OnceLock::new();
    CELL.get_or_init(|| Mutex::new(SetupProgress { state: "idle".into(), ..Default::default() }))
}

fn voice_dir(data: &Path) -> PathBuf {
    data.join("voice")
}

fn bin_dir(data: &Path) -> PathBuf {
    voice_dir(data).join("bin")
}

fn models_dir(data: &Path) -> PathBuf {
    voice_dir(data).join("models")
}

fn engine_asset_name() -> Option<&'static str> {
    if cfg!(all(windows, target_arch = "x86_64")) {
        Some("whisper-bin-x64.zip")
    } else if cfg!(all(windows, target_arch = "aarch64")) {
        Some("whisper-bin-win-cpu-arm64.zip")
    } else if cfg!(all(target_os = "linux", target_arch = "x86_64")) {
        Some("whisper-bin-ubuntu-x64.tar.gz")
    } else if cfg!(all(target_os = "linux", target_arch = "aarch64")) {
        Some("whisper-bin-ubuntu-arm64.tar.gz")
    } else {
        None
    }
}

fn engine_exe_name() -> &'static str {
    if cfg!(windows) { "whisper-cli.exe" } else { "whisper-cli" }
}

/// Locate a cached or PATH-installed `whisper-cli` binary.
pub fn find_engine(data: &Path) -> Option<PathBuf> {
    let local = bin_dir(data).join(engine_exe_name());
    if local.is_file() {
        return Some(local);
    }
    for name in ["whisper-cli", "whisper-cpp"] {
        if let Ok(p) = which::which(name) {
            return Some(p);
        }
    }
    None
}

/// Locate a cached GGML Whisper model (preferring `ggml-base.bin`, then any valid `ggml-*.bin`).
pub fn find_model(data: &Path) -> Option<PathBuf> {
    let dir = models_dir(data);
    let preferred = dir.join(MODEL_FILE);
    if is_valid_model(&preferred) {
        return Some(preferred);
    }
    let rd = std::fs::read_dir(&dir).ok()?;
    for entry in rd.flatten() {
        let p = entry.path();
        let name = p.file_name().and_then(|s| s.to_str()).unwrap_or("");
        if name.starts_with("ggml-") && name.ends_with(".bin") && is_valid_model(&p) {
            return Some(p);
        }
    }
    None
}

fn is_valid_model(p: &Path) -> bool {
    p.is_file() && p.metadata().map(|m| m.len() >= MIN_MODEL_BYTES).unwrap_or(false)
}

fn status_json(data: &Path) -> Value {
    let engine = find_engine(data);
    let model = find_model(data);
    let engine_ready = engine.is_some();
    let model_ready = model.is_some();
    let ready = engine_ready && model_ready;
    let can_download_engine = engine_asset_name().is_some();
    let can_download = can_download_engine || engine_ready;
    let prog = progress_lock().lock().unwrap().clone();
    let state = if prog.running {
        prog.state
    } else if ready {
        "ready".into()
    } else if prog.state == "error" {
        "error".into()
    } else {
        "idle".into()
    };
    let install_hint = if !engine_ready && !can_download_engine {
        if cfg!(target_os = "macos") {
            "Install whisper.cpp first with Homebrew (`brew install whisper-cpp`), then click Download to fetch the speech model."
        } else {
            "Install `whisper-cli` (whisper.cpp) on PATH, then try again."
        }
    } else {
        ""
    };
    json!({
        "engine_ready": engine_ready,
        "model_ready": model_ready,
        "ready": ready,
        "can_download": can_download,
        "model_name": model.as_ref().and_then(|p| p.file_name()).and_then(|s| s.to_str()).unwrap_or(MODEL_FILE),
        "model_mb": MODEL_MB,
        "state": state,
        "stage": prog.stage,
        "pct": prog.pct,
        "detail": if !prog.detail.is_empty() { prog.detail } else { install_hint.into() },
        "error": prog.error,
    })
}

fn emit_progress(core: &CoreRef, state: &str, stage: &str, pct: u64, detail: impl Into<String>, error: impl Into<String>) {
    let detail = detail.into();
    let error = error.into();
    {
        let mut p = progress_lock().lock().unwrap();
        p.running = matches!(state, "downloading" | "extracting");
        p.state = state.into();
        p.stage = stage.into();
        p.pct = pct;
        p.detail = detail.clone();
        p.error = error.clone();
    }
    let payload = status_json(&core.data);
    core.bus.emit("voice", None, None, payload);
}

fn http_client() -> reqwest::Client {
    reqwest::Client::builder()
        .user_agent(format!("model-roamer/{}", env!("CARGO_PKG_VERSION")))
        .connect_timeout(Duration::from_secs(15))
        .build()
        .unwrap_or_default()
}

async fn resolve_engine_url(asset: &str) -> String {
    let fallback = format!("https://github.com/ggml-org/whisper.cpp/releases/download/{FALLBACK_RELEASE_TAG}/{asset}");
    let client = http_client();
    let Ok(resp) = client
        .get(WHISPER_RELEASES_API)
        .header("Accept", "application/vnd.github+json")
        .timeout(Duration::from_secs(15))
        .send()
        .await
    else {
        return fallback;
    };
    if !resp.status().is_success() {
        return fallback;
    }
    let Ok(Value::Array(releases)) = resp.json::<Value>().await else {
        return fallback;
    };
    for rel in releases {
        if let Some(assets) = rel["assets"].as_array() {
            for a in assets {
                if a["name"].as_str() == Some(asset) {
                    if let Some(url) = a["browser_download_url"].as_str() {
                        return url.to_string();
                    }
                }
            }
        }
    }
    fallback
}

async fn download_with_progress(
    url: &str,
    dest: &Path,
    mut on_pct: impl FnMut(u64, u64, u64),
) -> anyhow::Result<()> {
    let resp = http_client()
        .get(url)
        .send()
        .await
        .map_err(|e| anyhow::anyhow!("Couldn't connect to download server: {e}"))?;
    if !resp.status().is_success() {
        anyhow::bail!("Download server returned HTTP {}", resp.status().as_u16());
    }
    let total = resp.content_length().unwrap_or(0);
    if let Some(parent) = dest.parent() {
        std::fs::create_dir_all(parent)?;
    }
    let part = dest.with_extension("part");
    let _ = std::fs::remove_file(&part);
    let mut file = std::fs::File::create(&part)
        .map_err(|e| anyhow::anyhow!("Couldn't create cache file: {e}"))?;
    let mut body = resp.bytes_stream();
    let mut got = 0u64;
    let mut last_pct = u64::MAX;

    loop {
        let next = tokio::time::timeout(Duration::from_secs(60), body.next())
            .await
            .map_err(|_| anyhow::anyhow!("Download timed out — check your internet connection and try again."))?;
        let Some(chunk) = next else { break };
        let chunk = chunk.map_err(|e| anyhow::anyhow!("Download was interrupted: {e}"))?;
        if let Err(e) = file.write_all(&chunk) {
            let _ = std::fs::remove_file(&part);
            return Err(anyhow::anyhow!("Couldn't write downloaded file: {e}"));
        }
        got += chunk.len() as u64;
        let pct = if total > 0 { (got * 100 / total).min(100) } else { 0 };
        if pct != last_pct {
            last_pct = pct;
            on_pct(pct, got, total);
        }
    }
    drop(file);
    if got == 0 {
        let _ = std::fs::remove_file(&part);
        anyhow::bail!("Downloaded file was empty.");
    }
    let _ = std::fs::remove_file(dest);
    std::fs::rename(&part, dest)
        .map_err(|e| anyhow::anyhow!("Couldn't save downloaded file to cache: {e}"))?;
    Ok(())
}

fn find_file_recursive(dir: &Path, filename: &str) -> Option<PathBuf> {
    let rd = std::fs::read_dir(dir).ok()?;
    for entry in rd.flatten() {
        let p = entry.path();
        if p.is_dir() {
            if let Some(found) = find_file_recursive(&p, filename) {
                return Some(found);
            }
        } else if p.file_name().and_then(|s| s.to_str()).map(|s| s.eq_ignore_ascii_case(filename)).unwrap_or(false) {
            return Some(p);
        }
    }
    None
}

async fn extract_engine_archive(archive: &Path, dest_bin: &Path) -> anyhow::Result<()> {
    let stage = dest_bin.with_extension("stage");
    let _ = std::fs::remove_dir_all(&stage);
    std::fs::create_dir_all(&stage)?;

    let mut extracted = false;
    // Built-in `tar` works on Windows 10/11 (including .zip) and Linux (.tar.gz)
    let mut tar_cmd = tokio::process::Command::new("tar");
    tar_cmd.arg("-xf").arg(archive).arg("-C").arg(&stage);
    crate::os::quiet(&mut tar_cmd);
    if let Ok(out) = tokio::time::timeout(Duration::from_secs(90), tar_cmd.output()).await {
        if out.map(|o| o.status.success()).unwrap_or(false) {
            extracted = true;
        }
    }

    #[cfg(windows)]
    if !extracted {
        let mut ps = tokio::process::Command::new("powershell");
        ps.args([
            "-NoProfile",
            "-NonInteractive",
            "-Command",
            "Expand-Archive -LiteralPath $args[0] -DestinationPath $args[1] -Force",
        ])
        .arg(archive)
        .arg(&stage);
        crate::os::quiet(&mut ps);
        let out = tokio::time::timeout(Duration::from_secs(90), ps.output())
            .await
            .map_err(|_| anyhow::anyhow!("Timed out extracting whisper.cpp archive."))??;
        if out.status.success() {
            extracted = true;
        } else {
            let err = String::from_utf8_lossy(&out.stderr);
            let _ = std::fs::remove_dir_all(&stage);
            anyhow::bail!("Couldn't extract whisper.cpp archive: {}", err.trim());
        }
    }

    if !extracted {
        let _ = std::fs::remove_dir_all(&stage);
        anyhow::bail!("Couldn't extract whisper.cpp archive (tar failed).");
    }

    let exe_name = engine_exe_name();
    let Some(found_exe) = find_file_recursive(&stage, exe_name) else {
        let _ = std::fs::remove_dir_all(&stage);
        anyhow::bail!("Downloaded whisper.cpp archive did not contain {exe_name}.");
    };
    let src_dir = found_exe.parent().unwrap_or(&stage);
    std::fs::create_dir_all(dest_bin)?;

    // Copy whisper-cli plus any companion shared libraries (.dll / .so / .dylib) in the same folder
    for entry in std::fs::read_dir(src_dir)?.flatten() {
        let p = entry.path();
        if !p.is_file() {
            continue;
        }
        let name = p.file_name().and_then(|s| s.to_str()).unwrap_or("");
        let lower = name.to_ascii_lowercase();
        let keep = lower == exe_name.to_ascii_lowercase()
            || lower.ends_with(".dll")
            || lower.ends_with(".dylib")
            || lower.contains(".so");
        if keep {
            let target = dest_bin.join(name);
            std::fs::copy(&p, &target)?;
            #[cfg(unix)]
            if lower == exe_name {
                use std::os::unix::fs::PermissionsExt;
                let _ = std::fs::set_permissions(&target, std::fs::Permissions::from_mode(0o755));
            }
        }
    }

    let _ = std::fs::remove_dir_all(&stage);
    Ok(())
}

async fn run_setup(core: CoreRef) {
    let data = core.data.clone();
    // 1. Engine (whisper-cli)
    if find_engine(&data).is_none() {
        let Some(asset) = engine_asset_name() else {
            let msg = if cfg!(target_os = "macos") {
                "Automatic whisper.cpp download isn't available on macOS. Install it via `brew install whisper-cpp`, then try again."
            } else {
                "No prebuilt whisper.cpp binary is available for this platform. Install `whisper-cli` on PATH, then try again."
            };
            emit_progress(&core, "error", "engine", 0, "", msg);
            return;
        };
        emit_progress(&core, "downloading", "engine", 0, "Fetching whisper.cpp…", "");
        let url = resolve_engine_url(asset).await;
        let archive = voice_dir(&data).join(asset);
        let res = download_with_progress(&url, &archive, |pct, _, _| {
            emit_progress(&core, "downloading", "engine", pct, format!("Downloading whisper.cpp… {pct}%"), "");
        })
        .await;
        if let Err(e) = res {
            let _ = std::fs::remove_file(&archive);
            emit_progress(&core, "error", "engine", 0, "", format!("Failed to download whisper.cpp: {e}"));
            return;
        }
        emit_progress(&core, "extracting", "engine", 100, "Extracting whisper.cpp…", "");
        let ex = extract_engine_archive(&archive, &bin_dir(&data)).await;
        let _ = std::fs::remove_file(&archive);
        if let Err(e) = ex {
            emit_progress(&core, "error", "engine", 0, "", format!("Failed to install whisper.cpp: {e}"));
            return;
        }
    }

    // 2. Speech model (ggml-base.bin, ~141 MB)
    if find_model(&data).is_none() {
        let dest = models_dir(&data).join(MODEL_FILE);
        emit_progress(&core, "downloading", "model", 0, format!("Downloading offline speech model (~{MODEL_MB} MB)…"), "");
        let res = download_with_progress(MODEL_URL, &dest, |pct, got, total| {
            let mb = got / (1024 * 1024);
            let tot = if total > 0 { total / (1024 * 1024) } else { MODEL_MB };
            emit_progress(
                &core,
                "downloading",
                "model",
                pct,
                format!("Downloading speech model… {pct}% ({mb}/{tot} MB)"),
                "",
            );
        })
        .await;
        if let Err(e) = res {
            let _ = std::fs::remove_file(&dest);
            emit_progress(&core, "error", "model", 0, "", format!("Failed to download speech model: {e}"));
            return;
        }
    }

    emit_progress(&core, "ready", "", 100, "Offline voice is ready", "");
}

/// Normalize a BCP-47 language tag (`"en-US"` -> `"en"`, `""` -> `"auto"`).
pub fn normalize_lang(raw: Option<&str>) -> String {
    let s = raw.unwrap_or("").trim();
    if s.is_empty() || s.eq_ignore_ascii_case("auto") {
        return "auto".into();
    }
    let primary = s.split(['-', '_']).next().unwrap_or("");
    let clean: String = primary.chars().filter(|c| c.is_ascii_alphabetic()).take(4).collect();
    if clean.is_empty() { "auto".into() } else { clean.to_ascii_lowercase() }
}

/// Validate a 16-bit PCM WAV payload before handing it to whisper.cpp.
pub fn validate_wav(bytes: &[u8]) -> anyhow::Result<()> {
    if bytes.len() < 44 {
        anyhow::bail!("Recording was too short to transcribe.");
    }
    if bytes.len() > MAX_WAV_BYTES {
        anyhow::bail!("Recording is too long (max 25 MB).");
    }
    if &bytes[0..4] != b"RIFF" || &bytes[8..12] != b"WAVE" {
        anyhow::bail!("Audio must be a WAV recording.");
    }
    Ok(())
}

/// Strip whisper.cpp blank-audio / silence placeholders and tidy whitespace.
pub fn clean_transcript(raw: &str) -> String {
    let mut parts = Vec::new();
    for line in raw.lines() {
        let mut s = line.trim();
        // Strip optional timestamp prefix like `[00:00:00.000 --> 00:00:03.000]`
        if s.starts_with('[') {
            if let Some(end) = s.find("-->") {
                if let Some(close) = s[end..].find(']') {
                    s = s[end + close + 1..].trim();
                }
            }
        }
        let lower = s.to_ascii_lowercase();
        if s.is_empty()
            || matches!(
                lower.as_str(),
                "[blank_audio]" | "(blank audio)" | "[silence]" | "(silence)" | "[no speech]" | "(inaudible)" | "[inaudible]"
            )
        {
            continue;
        }
        parts.push(s);
    }
    parts.join(" ").split_whitespace().collect::<Vec<_>>().join(" ")
}

/// Run `whisper-cli` on a WAV file and return the cleaned transcript text.
pub async fn transcribe_file(engine: &Path, model: &Path, wav: &Path, lang: &str) -> anyhow::Result<String> {
    let threads = std::thread::available_parallelism()
        .map(|n| n.get().clamp(2, 6))
        .unwrap_or(4);
    let mut cmd = tokio::process::Command::new(engine);
    if let Some(dir) = engine.parent() {
        cmd.current_dir(dir);
    }
    cmd.args(["-m"])
        .arg(model)
        .args(["-f"])
        .arg(wav)
        .args(["-nt", "-np", "-l", lang, "-t", &threads.to_string()]);
    crate::os::quiet(&mut cmd);

    let out = tokio::time::timeout(Duration::from_secs(60), cmd.output())
        .await
        .map_err(|_| anyhow::anyhow!("Local transcription timed out after 60 seconds."))?
        .map_err(|e| anyhow::anyhow!("Couldn't start whisper.cpp ({}): {e}", engine.display()))?;

    if !out.status.success() {
        let stderr = String::from_utf8_lossy(&out.stderr);
        let detail = stderr.lines().rev().find(|l| !l.trim().is_empty()).unwrap_or("whisper-cli exited with an error");
        anyhow::bail!("Local transcription failed: {}", detail.trim());
    }
    Ok(clean_transcript(&String::from_utf8_lossy(&out.stdout)))
}

// ---------------------------------------------------------------- HTTP handlers

pub async fn api_status(State(core): State<CoreRef>) -> R {
    Ok(Json(status_json(&core.data)))
}

pub async fn api_setup(State(core): State<CoreRef>) -> R {
    let already_ready = find_engine(&core.data).is_some() && find_model(&core.data).is_some();
    if already_ready {
        return Ok(Json(status_json(&core.data)));
    }
    {
        let mut p = progress_lock().lock().unwrap();
        if p.running {
            return Ok(Json(status_json(&core.data)));
        }
        p.running = true;
        p.state = "downloading".into();
        p.stage = if find_engine(&core.data).is_none() { "engine".into() } else { "model".into() };
        p.pct = 0;
        p.detail = "Starting download…".into();
        p.error.clear();
    }
    let c = core.clone();
    tokio::spawn(async move {
        run_setup(c).await;
    });
    Ok(Json(status_json(&core.data)))
}

pub async fn api_transcribe(
    State(core): State<CoreRef>,
    Query(q): Query<HashMap<String, String>>,
    body: Bytes,
) -> R {
    validate_wav(&body).map_err(|e| bad(e.to_string()))?;
    let Some(engine) = find_engine(&core.data) else {
        return Err(bad("Offline speech engine (whisper.cpp) isn't installed yet. Download it from the mic prompt first."));
    };
    let Some(model) = find_model(&core.data) else {
        return Err(bad("Offline speech model isn't downloaded yet. Download it from the mic prompt first."));
    };

    let tmp_dir = voice_dir(&core.data).join("tmp");
    std::fs::create_dir_all(&tmp_dir)?;
    let wav_path = tmp_dir.join(format!("{}.wav", crate::util::id("mic")));
    tokio::fs::write(&wav_path, &body).await?;

    let lang = normalize_lang(q.get("lang").map(String::as_str));
    let res = transcribe_file(&engine, &model, &wav_path, &lang).await;
    let _ = tokio::fs::remove_file(&wav_path).await;

    let text = res.map_err(|e| bad(e.to_string()))?;
    Ok(Json(json!({ "text": text })))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn normalizes_language_tag() {
        assert_eq!(normalize_lang(None), "auto");
        assert_eq!(normalize_lang(Some("")), "auto");
        assert_eq!(normalize_lang(Some("en-US")), "en");
        assert_eq!(normalize_lang(Some("pt_BR")), "pt");
        assert_eq!(normalize_lang(Some("ja-JP")), "ja");
        assert_eq!(normalize_lang(Some("123")), "auto");
    }

    #[test]
    fn validates_wav_header() {
        assert!(validate_wav(&[]).is_err());
        assert!(validate_wav(&[0u8; 44]).is_err());
        let mut wav = vec![0u8; 64];
        wav[0..4].copy_from_slice(b"RIFF");
        wav[8..12].copy_from_slice(b"WAVE");
        assert!(validate_wav(&wav).is_ok());
    }

    #[test]
    fn cleans_transcript_output() {
        assert_eq!(clean_transcript("  [BLANK_AUDIO]\n"), "");
        assert_eq!(clean_transcript("[00:00:00.000 --> 00:00:02.100]   Hello world.\n[00:00:02.100 --> 00:00:04.000]  How are you? "), "Hello world. How are you?");
        assert_eq!(clean_transcript("(silence)\n  Ship it  \n[BLANK_AUDIO]"), "Ship it");
    }

    #[test]
    fn ignores_tiny_corrupt_model_file() {
        let dir = std::env::temp_dir().join(format!("mr-voice-test-{}", crate::util::id("t")));
        let mdir = models_dir(&dir);
        std::fs::create_dir_all(&mdir).unwrap();
        std::fs::write(mdir.join("ggml-base.bin"), b"corrupt").unwrap();
        assert!(find_model(&dir).is_none());
        let _ = std::fs::remove_dir_all(&dir);
    }
}
