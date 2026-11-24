//! Installation and supervision of Model Roamer's shared, loopback-only llama-server.

use crate::{
    core::CoreRef,
    os,
    server::{bad, R},
};
use anyhow::{anyhow, bail, ensure, Context, Result};
use axum::{
    extract::{Query, State},
    Json,
};
use reqwest::{header, Client, StatusCode};
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use sha2::{Digest, Sha256};
use std::{
    collections::{HashMap, VecDeque},
    fs, io,
    net::TcpListener,
    path::{Component, Path, PathBuf},
    process::Stdio,
    sync::{Arc, Mutex, OnceLock},
    time::{Duration, Instant},
};
use tokio::{
    io::{AsyncReadExt, AsyncWriteExt},
    process::{Child, Command},
    sync::{Mutex as AsyncMutex, OnceCell},
    time::{sleep, timeout},
};

const PIN: &str = "b12686";
const RELEASE_API: &str = "https://api.github.com/repos/ggml-org/llama.cpp/releases";
const RELEASE_DOWNLOAD: &str = "https://github.com/ggml-org/llama.cpp/releases/download";
const MIB: u64 = 1024 * 1024;
const IDLE: Duration = Duration::from_secs(15 * 60);
const START_TIMEOUT: Duration = Duration::from_secs(60);
const CRASH_WINDOW: Duration = Duration::from_secs(5 * 60);
const BACKOFF: [u64; 3] = [1, 3, 8];
const EXE: &str = if cfg!(windows) {
    "llama-server.exe"
} else {
    "llama-server"
};

struct Model {
    id: &'static str,
    name: &'static str,
    repo: &'static str,
    revision: &'static str,
    file: &'static str,
    size: u64,
    sha: &'static str,
    min_vram_mib: Option<u64>,
}

// Revisions and LFS SHA-256 values come from the publishers' Hugging Face trees.
const MODELS: &[Model] = &[
    Model {
        id: "qwen3-4b-instruct-2507",
        name: "Qwen3-4B-Instruct-2507 Q4_K_M",
        repo: "lmstudio-community/Qwen3-4B-Instruct-2507-GGUF",
        revision: "4edb920b6f14e3b9284d4502a6485103d72cde05",
        file: "Qwen3-4B-Instruct-2507-Q4_K_M.gguf",
        size: 2_497_280_448,
        sha: "8cdb57cbb880d313736a9bc4e3d3d2485f145b5e19cf33783746e753e82641fc",
        min_vram_mib: Some(4096),
    },
    Model {
        id: "smollm3-3b",
        name: "SmolLM3-3B Q4_K_M",
        repo: "unsloth/SmolLM3-3B-GGUF",
        revision: "a7bc17204c8a326d6bd6e466e076959eddae2025",
        file: "SmolLM3-3B-Q4_K_M.gguf",
        size: 1_915_306_528,
        sha: "4de907d2d388a5508fb7cb443a06effe14cce3518b0a78d3bdd9e74d9edce989",
        min_vram_mib: None,
    },
    Model {
        id: "gemma-4-e2b-it",
        name: "Gemma 4 E2B-it Q4_K_M",
        repo: "unsloth/gemma-4-E2B-it-GGUF",
        revision: "0314792d7f1f7e229411f620751375812bb9faf2",
        file: "gemma-4-E2B-it-Q4_K_M.gguf",
        size: 3_106_738_272,
        sha: "740185b21d22ceb83a11c3aa62ad5842ef32c70f6096d756bbee85a1e4ec34b8",
        min_vram_mib: Some(4608),
    },
];

fn model(id: &str) -> Result<&'static Model> {
    MODELS
        .iter()
        .find(|m| m.id == id)
        .context("Unknown local model")
}

impl Model {
    fn weights(&self, root: &Path) -> PathBuf {
        root.join("weights").join(self.file)
    }

    fn checksum_file(&self, root: &Path) -> PathBuf {
        self.weights(root).with_extension("gguf.sha256")
    }

    fn verified(&self, root: &Path) -> bool {
        size_on_disk(&self.weights(root)) == Some(self.size)
            && fs::read_to_string(self.checksum_file(root)).is_ok_and(|sha| sha == self.sha)
    }
}

struct PlatformAsset {
    suffix: &'static str,
    size: u64,
    sha: &'static str,
}

// Source: https://api.github.com/repos/ggml-org/llama.cpp/releases/tags/b12686
fn platform_asset(gpu: bool) -> Option<PlatformAsset> {
    let (suffix, size, sha) = match (std::env::consts::OS, std::env::consts::ARCH, gpu) {
        ("windows", "x86_64", false) => (
            "win-cpu-x64.zip",
            25215999,
            "b4ce74b897e4f037736d50e01db5cb813b0195fdc8b502295f73da66d4304b3e",
        ),
        ("windows", "x86_64", true) => (
            "win-vulkan-x64.zip",
            39075952,
            "c6e65326dc4a529a1ba75ec66130bf493311b3c26ce0e07cf7529b927fb75529",
        ),
        ("windows", "aarch64", false) => (
            "win-cpu-arm64.zip",
            13284194,
            "2ca96d0dfb0cc7c4f3c21df8552c7028a840c49403038c9b12276e38fc8bce6e",
        ),
        ("linux", "x86_64", false) => (
            "ubuntu-x64.tar.gz",
            23375907,
            "2325b7c1b54180a91136abaf9ca1d009ac4b2c8a68eb9b4709d45ed4d47ff90d",
        ),
        ("linux", "x86_64", true) => (
            "ubuntu-vulkan-x64.tar.gz",
            37073391,
            "9e34411000a80c49a5e645df9b82d1556fd8869a02275cb3ad267b6a81e34fa8",
        ),
        ("linux", "aarch64", false) => (
            "ubuntu-arm64.tar.gz",
            18928872,
            "414a9148bdc6fff0a9689c33a489e5854858ec6dcceae5c08de5abb324d0f862",
        ),
        ("linux", "aarch64", true) => (
            "ubuntu-vulkan-arm64.tar.gz",
            30059727,
            "f6be23f2fd3dd33dbd780c0664921a269fff7243ab510d983503bdc73e229ba1",
        ),
        ("macos", "x86_64", false) => (
            "macos-x64.tar.gz",
            12538857,
            "80683324dbc5f6dc88f898fcda0066311ae75724e62e856b768c1f55a3b12d03",
        ),
        ("macos", "aarch64", false) => (
            "macos-arm64.tar.gz",
            16818959,
            "845a33b288f1fa1926346a9fd6714bfa3e7ba1c149e78ab95907305b6112a3e8",
        ),
        _ => return None,
    };
    Some(PlatformAsset { suffix, size, sha })
}

#[derive(Clone, Deserialize, Serialize, PartialEq, Eq)]
struct Selection {
    tag: String,
    model: String,
}

#[derive(Default, Deserialize, Serialize)]
struct Current {
    active: Option<Selection>,
    previous: Option<Selection>,
}

fn build_number(tag: &str) -> Option<u32> {
    let number = tag.strip_prefix('b')?.parse::<u32>().ok()?;
    (format!("b{number}") == tag).then_some(number)
}

impl Current {
    fn read(root: &Path) -> Result<Self> {
        let bytes = match fs::read(root.join("current.json")) {
            Ok(bytes) => bytes,
            Err(e) if e.kind() == io::ErrorKind::NotFound => return Ok(Self::default()),
            Err(e) => return Err(e.into()),
        };
        let current: Self =
            serde_json::from_slice(&bytes).context("Invalid local model current.json")?;
        for selection in current.active.iter().chain(current.previous.iter()) {
            ensure!(
                build_number(&selection.tag).is_some(),
                "Invalid llama.cpp build tag"
            );
            model(&selection.model)?;
        }
        Ok(current)
    }

    fn save(&self, root: &Path) -> Result<()> {
        os::write_atomic(
            &root.join("current.json"),
            &serde_json::to_vec_pretty(self)?,
        )
        .context("Could not save local model current.json")
    }

    fn activate(&mut self, selection: Selection) {
        let tag_changed = self
            .active
            .as_ref()
            .is_some_and(|active| active.tag != selection.tag);
        let previous = self.active.replace(selection);
        if tag_changed {
            self.previous = previous;
        }
    }

    fn rollback(&mut self) {
        std::mem::swap(&mut self.active, &mut self.previous);
    }
}

#[derive(Clone, Serialize)]
struct Gpu {
    name: String,
    vram_mib: u64,
    device: Option<String>,
}

#[derive(Clone, Default, Serialize)]
struct Hardware {
    ram_mib: Option<u64>,
    gpu: Option<Gpu>,
    metal: bool,
}

#[derive(Clone, Copy, Serialize, PartialEq, Eq, Debug)]
#[serde(rename_all = "lowercase")]
enum Backend {
    Cpu,
    Vulkan,
    Metal,
}

#[derive(Serialize)]
struct Profile {
    backend: Backend,
    context: u32,
}

fn profile(hardware: &Hardware, model: &Model, cpu: bool) -> Profile {
    let vram = hardware.gpu.as_ref().map_or(0, |gpu| gpu.vram_mib);
    if !cpu && model.min_vram_mib.is_some_and(|min| vram >= min) {
        let backend = if hardware.metal {
            Backend::Metal
        } else {
            Backend::Vulkan
        };
        let context = match (model.id, vram) {
            ("gemma-4-e2b-it", _) => 4096,
            (_, 6144..) => 12288,
            _ => 8192,
        };
        return Profile { backend, context };
    }
    Profile {
        backend: Backend::Cpu,
        context: if hardware.ram_mib.is_some_and(|ram| ram >= 8192) {
            8192
        } else {
            4096
        },
    }
}

fn recommended(hardware: &Hardware) -> &'static Model {
    if profile(hardware, &MODELS[0], false).backend == Backend::Cpu {
        &MODELS[1]
    } else {
        &MODELS[0]
    }
}

async fn probe_hardware() -> Hardware {
    #[cfg(target_os = "macos")]
    let ram_mib = command_output(Command::new("sysctl").args(["-n", "hw.memsize"]), 5)
        .await
        .and_then(|out| out.trim().parse::<u64>().ok())
        .map(|bytes| bytes / MIB);
    #[cfg(not(target_os = "macos"))]
    let ram_mib = total_ram_mib();
    let metal = cfg!(all(target_os = "macos", target_arch = "aarch64"));
    let gpu = if metal {
        ram_mib.map(|ram| Gpu {
            name: "Apple Silicon (shared memory)".into(),
            vram_mib: ram / 2,
            device: None,
        })
    } else if platform_asset(true).is_some() {
        let mut nvidia = Command::new("nvidia-smi");
        nvidia.args([
            "--query-gpu=name,memory.total",
            "--format=csv,noheader,nounits",
        ]);
        let nvidia = command_output(&mut nvidia, 5).await;
        nvidia
            .as_deref()
            .unwrap_or_default()
            .lines()
            .filter_map(|line| {
                let (name, memory) = line.rsplit_once(',')?;
                Some(Gpu {
                    name: name.trim().to_string(),
                    vram_mib: memory.trim().parse().ok()?,
                    device: None,
                })
            })
            .chain(linux_vram())
            .max_by_key(|gpu| gpu.vram_mib)
    } else {
        None
    };
    Hardware {
        ram_mib,
        gpu,
        metal,
    }
}

#[cfg(target_os = "linux")]
fn total_ram_mib() -> Option<u64> {
    let meminfo = fs::read_to_string("/proc/meminfo").ok()?;
    meminfo.lines().find_map(|line| {
        line.strip_prefix("MemTotal:")?
            .split_whitespace()
            .next()?
            .parse::<u64>()
            .ok()
            .map(|kb| kb / 1024)
    })
}

#[cfg(not(any(windows, target_os = "linux", target_os = "macos")))]
fn total_ram_mib() -> Option<u64> {
    None
}

#[cfg(target_os = "linux")]
fn linux_vram() -> Option<Gpu> {
    fs::read_dir("/sys/class/drm")
        .ok()?
        .flatten()
        .filter_map(|entry| {
            let device = entry.path().join("device");
            let bytes = fs::read_to_string(device.join("mem_info_vram_total"))
                .ok()?
                .trim()
                .parse::<u64>()
                .ok()?;
            let name = fs::read_to_string(device.join("product_name"))
                .unwrap_or_else(|_| entry.file_name().to_string_lossy().into_owned());
            Some(Gpu {
                name: name.trim().into(),
                vram_mib: bytes / MIB,
                device: None,
            })
        })
        .max_by_key(|gpu| gpu.vram_mib)
}

#[cfg(not(target_os = "linux"))]
fn linux_vram() -> Option<Gpu> {
    None
}

fn vulkan_candidate(hardware: &Hardware) -> bool {
    if platform_asset(true).is_none() {
        return false;
    }
    #[cfg(windows)]
    let detectable = std::env::var_os("SystemRoot")
        .is_some_and(|dir| PathBuf::from(dir).join("System32/vulkan-1.dll").is_file());
    #[cfg(target_os = "linux")]
    let detectable = Path::new("/dev/dri").is_dir();
    #[cfg(not(any(windows, target_os = "linux")))]
    let detectable = false;
    detectable || hardware.gpu.is_some()
}

async fn command_output(cmd: &mut Command, seconds: u64) -> Option<String> {
    os::quiet(cmd).kill_on_drop(true).stdin(Stdio::null());
    let out = timeout(Duration::from_secs(seconds), cmd.output())
        .await
        .ok()?
        .ok()?;
    out.status
        .success()
        .then(|| String::from_utf8_lossy(&out.stdout).into_owned())
}

fn vulkan_devices(output: &str, preferred: Option<&Gpu>) -> Option<Gpu> {
    output
        .lines()
        .filter_map(|line| {
            let (device, description) = line.trim().split_once(':')?;
            device.strip_prefix("Vulkan")?.parse::<u32>().ok()?;
            let (name, memory) = description.rsplit_once(" (")?;
            let vram_mib = memory.split_once(" MiB")?.0.parse().ok()?;
            Some(Gpu {
                name: name.trim().into(),
                vram_mib,
                device: Some(device.into()),
            })
        })
        .max_by_key(|gpu| {
            let identified =
                preferred.is_some_and(|known| known.name.eq_ignore_ascii_case(&gpu.name));
            (identified, gpu.vram_mib)
        })
}

async fn probe_vulkan(exe: &Path, preferred: Option<&Gpu>) -> Option<Gpu> {
    let mut command = engine_command(exe);
    let out = command_output(command.arg("--list-devices"), 8).await?;
    vulkan_devices(&out, preferred)
}

#[cfg(windows)]
fn total_ram_mib() -> Option<u64> {
    use windows_sys::Win32::System::SystemInformation::{GlobalMemoryStatusEx, MEMORYSTATUSEX};
    let mut status: MEMORYSTATUSEX = unsafe { std::mem::zeroed() };
    status.dwLength = std::mem::size_of_val(&status) as u32;
    (unsafe { GlobalMemoryStatusEx(&mut status) } != 0).then_some(status.ullTotalPhys / MIB)
}

#[cfg(windows)]
fn free_disk_bytes(path: &Path) -> io::Result<u64> {
    use std::os::windows::ffi::OsStrExt;
    use windows_sys::Win32::Storage::FileSystem::GetDiskFreeSpaceExW;
    let path: Vec<u16> = path.as_os_str().encode_wide().chain(Some(0)).collect();
    let mut free = 0;
    let ok = unsafe {
        GetDiskFreeSpaceExW(
            path.as_ptr(),
            &mut free,
            std::ptr::null_mut(),
            std::ptr::null_mut(),
        )
    };
    if ok == 0 {
        Err(io::Error::last_os_error())
    } else {
        Ok(free)
    }
}

#[cfg(unix)]
fn free_disk_bytes(path: &Path) -> io::Result<u64> {
    use std::os::unix::ffi::OsStrExt;
    let path = std::ffi::CString::new(path.as_os_str().as_bytes())?;
    let mut stats = std::mem::MaybeUninit::<libc::statvfs>::uninit();
    if unsafe { libc::statvfs(path.as_ptr(), stats.as_mut_ptr()) } != 0 {
        return Err(io::Error::last_os_error());
    }
    let stats = unsafe { stats.assume_init() };
    Ok((stats.f_bavail as u64).saturating_mul(stats.f_frsize as u64))
}

#[cfg(not(any(unix, windows)))]
fn free_disk_bytes(_: &Path) -> io::Result<u64> {
    Err(io::Error::new(
        io::ErrorKind::Unsupported,
        "Platform has no disk space probe",
    ))
}

struct Download {
    url: String,
    size: u64,
    sha: String,
}

fn network() -> &'static Client {
    static CLIENT: OnceLock<Client> = OnceLock::new();
    CLIENT.get_or_init(|| {
        Client::builder()
            .user_agent(concat!("Model-Roamer/", env!("CARGO_PKG_VERSION")))
            .connect_timeout(Duration::from_secs(10))
            .read_timeout(Duration::from_secs(60))
            .timeout(Duration::from_secs(4 * 60 * 60))
            .build()
            .expect("download HTTP client")
    })
}

async fn sha256(path: &Path) -> Result<String> {
    let mut file = tokio::fs::File::open(path).await?;
    let mut buffer = vec![0; 256 * 1024];
    let mut digest = Sha256::new();
    loop {
        let read = file.read(&mut buffer).await?;
        if read == 0 {
            return Ok(format!("{:x}", digest.finalize()));
        }
        digest.update(&buffer[..read]);
    }
}

fn size_on_disk(path: &Path) -> Option<u64> {
    let metadata = fs::metadata(path).ok()?;
    metadata.is_file().then_some(metadata.len())
}

fn check_space(dir: &Path, remaining: u64, extraction_space: u64) -> Result<()> {
    let free = free_disk_bytes(dir)
        .with_context(|| format!("Could not check free space in {}", dir.display()))?;
    let needed = remaining
        .saturating_add(remaining / 4)
        .saturating_add(500 * MIB)
        .saturating_add(extraction_space);
    ensure!(
        free >= needed,
        "Local model needs {:.2} GiB free, but only {:.2} GiB is available",
        needed as f64 / (1024 * MIB) as f64,
        free as f64 / (1024 * MIB) as f64
    );
    Ok(())
}

fn valid_content_range(range: &str, offset: u64, size: u64) -> bool {
    let Some((bounds, total)) = range
        .strip_prefix("bytes ")
        .and_then(|range| range.split_once('/'))
    else {
        return false;
    };
    let Some((start, end)) = bounds.split_once('-') else {
        return false;
    };
    start.parse::<u64>() == Ok(offset)
        && end.parse::<u64>().ok().and_then(|end| end.checked_add(1)) == Some(size)
        && total.parse::<u64>() == Ok(size)
}

async fn transfer(
    asset: &Download,
    part: &Path,
    extraction_space: u64,
    progress: &mut impl FnMut(u64),
) -> Result<()> {
    let offset = size_on_disk(part).unwrap_or(0);
    if offset == asset.size {
        return Ok(());
    }
    let dir = part.parent().context("Missing download directory")?;
    check_space(dir, asset.size - offset, extraction_space)?;
    progress(offset);
    let mut request = network()
        .get(&asset.url)
        .header(header::ACCEPT_ENCODING, "identity");
    if offset != 0 {
        request = request.header(header::RANGE, format!("bytes={offset}-"));
    }
    let mut response = request.send().await.map_err(reqwest::Error::without_url)?;
    let start = match response.status() {
        StatusCode::OK => {
            if offset != 0 {
                // Truncating the partial file frees its existing bytes.
                check_space(dir, asset.size - offset, extraction_space + offset / 4)?;
            }
            0
        }
        StatusCode::PARTIAL_CONTENT if offset > 0 => {
            let range = response
                .headers()
                .get(header::CONTENT_RANGE)
                .and_then(|v| v.to_str().ok())
                .unwrap_or_default();
            if !valid_content_range(range, offset, asset.size) {
                tokio::fs::remove_file(part).await?;
                bail!("Invalid resume response: {range}. Retrying from the beginning");
            }
            offset
        }
        StatusCode::RANGE_NOT_SATISFIABLE if offset > 0 => {
            tokio::fs::remove_file(part).await?;
            bail!("Server rejected the partial download. Retrying from the beginning");
        }
        status => bail!("Unexpected download response: HTTP {status}"),
    };
    if let Some(bytes) = response.content_length() {
        ensure!(
            bytes == asset.size - start,
            "Unexpected download size (received {bytes} response bytes)"
        );
    }
    let mut file = tokio::fs::OpenOptions::new()
        .write(true)
        .create(true)
        .truncate(start == 0)
        .append(start != 0)
        .open(part)
        .await?;
    let mut got = start;
    progress(got);
    while let Some(chunk) = response
        .chunk()
        .await
        .map_err(reqwest::Error::without_url)?
    {
        got += chunk.len() as u64;
        ensure!(got <= asset.size, "Download exceeded expected length");
        file.write_all(&chunk).await?;
        progress(got);
    }
    ensure!(
        got == asset.size,
        "Download ended after {got} of {} bytes",
        asset.size
    );
    file.sync_all().await?;
    Ok(())
}

async fn download(
    asset: &Download,
    dest: &Path,
    extraction_space: u64,
    mut progress: impl FnMut(u64),
) -> Result<()> {
    let dir = dest.parent().context("Missing destination directory")?;
    fs::create_dir_all(dir)?;
    let part = dest.with_file_name(format!(
        "{}.part",
        dest.file_name()
            .context("Missing filename")?
            .to_string_lossy()
    ));
    if size_on_disk(&part).is_some_and(|bytes| bytes > asset.size) {
        fs::remove_file(&part)?;
    }
    let mut last_error = None;
    for delay in [0, 1, 3] {
        sleep(Duration::from_secs(delay)).await;
        match transfer(asset, &part, extraction_space, &mut progress).await {
            Ok(()) if sha256(&part).await? == asset.sha => {
                tokio::fs::rename(&part, dest)
                    .await
                    .context("Could not publish verified download")?;
                progress(asset.size);
                return Ok(());
            }
            Ok(()) => {
                fs::remove_file(&part)?;
                last_error = Some(anyhow!("SHA-256 mismatch for {}", dest.display()));
            }
            Err(error) => last_error = Some(error),
        }
    }
    Err(last_error.unwrap()).with_context(|| format!("Could not download {}", dest.display()))
}

#[derive(Deserialize)]
struct Release {
    tag_name: String,
    assets: Vec<ReleaseAsset>,
}

#[derive(Deserialize)]
struct ReleaseAsset {
    name: String,
    size: u64,
    digest: Option<String>,
}

async fn latest_release() -> Result<Release> {
    let platform =
        platform_asset(false).context("No prebuilt llama.cpp package for this platform")?;
    // GitHub's /latest points to v1.0.0. The prebuilt binaries are published as numbered builds.
    let releases: Vec<Release> = network()
        .get(format!("{RELEASE_API}?per_page=10"))
        .timeout(Duration::from_secs(15))
        .send()
        .await?
        .error_for_status()?
        .json()
        .await?;
    releases
        .into_iter()
        .filter_map(|release| {
            let build = build_number(&release.tag_name)?;
            let name = format!("llama-{}-bin-{}", release.tag_name, platform.suffix);
            release
                .assets
                .iter()
                .any(|asset| asset.name == name)
                .then_some(())?;
            engine_download(&release.tag_name, &platform, Some(&release)).ok()?;
            Some((build, release))
        })
        .max_by_key(|(build, _)| *build)
        .map(|(_, release)| release)
        .context("No recent numbered llama.cpp release has a verified package for this platform")
}

fn engine_download(
    tag: &str,
    platform: &PlatformAsset,
    release: Option<&Release>,
) -> Result<Download> {
    let name = format!("llama-{tag}-bin-{}", platform.suffix);
    let (size, sha) = if tag == PIN {
        (platform.size, platform.sha)
    } else {
        let asset = release
            .filter(|release| release.tag_name == tag)
            .and_then(|release| release.assets.iter().find(|a| a.name == name))
            .with_context(|| {
                format!(
                    "llama.cpp {tag} has no published {} package",
                    platform.suffix
                )
            })?;
        let sha = asset
            .digest
            .as_deref()
            .and_then(|digest| digest.strip_prefix("sha256:"))
            .filter(|sha| sha.len() == 64 && sha.bytes().all(|ch| ch.is_ascii_hexdigit()))
            .context("The release package is missing its SHA-256 digest")?;
        ensure!(asset.size > 0, "The release package is empty");
        (asset.size, sha)
    };
    Ok(Download {
        url: format!("{RELEASE_DOWNLOAD}/{tag}/{name}"),
        size,
        sha: sha.to_ascii_lowercase(),
    })
}

fn package_dir(root: &Path, tag: &str, platform: &PlatformAsset) -> PathBuf {
    root.join("bin").join(tag).join(platform.suffix)
}

fn installed_engine(root: &Path, tag: &str, platform: &PlatformAsset) -> Option<PathBuf> {
    let dir = package_dir(root, tag, platform);
    let entry = fs::read_to_string(dir.join("executable")).ok()?;
    let rel = Path::new(&entry);
    if rel.file_name()? != EXE
        || !rel
            .components()
            .all(|part| matches!(part, Component::Normal(_)))
    {
        return None;
    }
    let exe = dir.join(rel);
    exe.is_file().then_some(exe)
}

fn find_engine(dir: &Path) -> Option<PathBuf> {
    for entry in fs::read_dir(dir).ok()?.flatten() {
        let path = entry.path();
        let kind = entry.file_type().ok()?;
        if kind.is_file() && entry.file_name() == EXE {
            return Some(path);
        }
        if kind.is_dir() {
            if let Some(exe) = find_engine(&path) {
                return Some(exe);
            }
        }
    }
    None
}

fn engine_command(exe: &Path) -> Command {
    let mut command = Command::new(exe);
    // Prevent shell or service environment settings from overriding this managed instance.
    for (key, _) in std::env::vars_os() {
        if key
            .to_string_lossy()
            .to_ascii_uppercase()
            .starts_with("LLAMA_")
        {
            command.env_remove(key);
        }
    }
    if let Some(dir) = exe.parent() {
        command.current_dir(dir);
    }
    command
}

fn tar_command() -> Command {
    // Git's tar on PATH does not read zip archives. Windows ships bsdtar.
    #[cfg(windows)]
    if let Some(windows) = std::env::var_os("SystemRoot") {
        let tar = PathBuf::from(windows).join("System32/tar.exe");
        if tar.is_file() {
            return Command::new(tar);
        }
    }
    Command::new("tar")
}

async fn extract(archive: &Path, stage: &Path) -> Result<()> {
    let mut command = tar_command();
    command.arg("-xf").arg(archive).arg("-C").arg(stage);
    os::quiet(&mut command).kill_on_drop(true);
    let result = timeout(Duration::from_secs(90), command.output())
        .await
        .context("Timed out extracting llama.cpp")??;
    ensure!(
        result.status.success(),
        "Could not extract llama.cpp: {}",
        String::from_utf8_lossy(&result.stderr).trim()
    );
    Ok(())
}

async fn install_engine(
    core: &CoreRef,
    manager: &Manager,
    tag: &str,
    platform: &PlatformAsset,
    release: Option<&Release>,
) -> Result<PathBuf> {
    let root = &manager.root;
    if let Some(exe) = installed_engine(root, tag, platform) {
        return Ok(exe);
    }
    let asset = engine_download(tag, platform, release)?;
    let package = package_dir(root, tag, platform);
    let archive = root
        .join("downloads")
        .join(format!("{tag}-{}", platform.suffix));
    let extraction_space = asset.size.saturating_mul(4);
    if size_on_disk(&archive) != Some(asset.size) || sha256(&archive).await? != asset.sha {
        manager
            .download(core, "engine", &asset, &archive, extraction_space)
            .await?;
    }
    manager.progress(core, Progress::stage("extracting", "engine"));
    check_space(archive.parent().unwrap(), 0, extraction_space)?;
    let stage = package.with_extension("stage");
    remove_dir(&stage)?;
    fs::create_dir_all(&stage)?;
    extract(&archive, &stage).await?;
    let exe = find_engine(&stage).context("The llama.cpp package did not contain llama-server")?;
    command_output(engine_command(&exe).arg("--version"), 10).await
        .context("The downloaded llama-server did not pass its --version check. Check OS and driver compatibility")?;
    let rel = exe
        .strip_prefix(&stage)?
        .to_str()
        .context("Invalid llama-server archive path")?;
    fs::write(stage.join("executable"), rel)?;
    remove_dir(&package)?;
    fs::rename(&stage, &package)?;
    if let Err(error) = fs::remove_file(&archive) {
        tracing::warn!(%error, "Could not remove verified llama.cpp archive");
    }
    Ok(package.join(rel))
}

fn remove_dir(path: &Path) -> io::Result<()> {
    match fs::remove_dir_all(path) {
        Err(error) if error.kind() == io::ErrorKind::NotFound => Ok(()),
        result => result,
    }
}

/// Hold the endpoint for the duration of a turn (including all streamed requests).
/// The base URL ends at the port. Append an OpenAI route such as /v1/chat/completions.
#[derive(Clone)]
pub struct Endpoint {
    pub base_url: String,
    pub api_key: String,
    lease: Arc<()>,
}

impl Endpoint {
    fn in_use(&self) -> bool {
        Arc::strong_count(&self.lease) > 1
    }

    fn idle(&self, last_active: &mut Instant, now: Instant) -> bool {
        if self.in_use() {
            *last_active = now;
        }
        now.duration_since(*last_active) >= IDLE
    }
}

struct Running {
    child: Child,
    #[cfg(windows)]
    _job: std::os::windows::io::OwnedHandle,
    endpoint: Endpoint,
    profile: Profile,
    last_active: Instant,
}

#[cfg(windows)]
fn kill_on_close_job() -> io::Result<std::os::windows::io::OwnedHandle> {
    use std::os::windows::io::FromRawHandle;
    use windows_sys::Win32::System::JobObjects::*;
    let handle = unsafe { CreateJobObjectW(std::ptr::null(), std::ptr::null()) };
    if handle.is_null() {
        return Err(io::Error::last_os_error());
    }
    let job = unsafe { std::os::windows::io::OwnedHandle::from_raw_handle(handle) };
    let mut limits: JOBOBJECT_EXTENDED_LIMIT_INFORMATION = unsafe { std::mem::zeroed() };
    limits.BasicLimitInformation.LimitFlags = JOB_OBJECT_LIMIT_KILL_ON_JOB_CLOSE;
    let ok = unsafe {
        SetInformationJobObject(
            handle,
            JobObjectExtendedLimitInformation,
            (&limits as *const JOBOBJECT_EXTENDED_LIMIT_INFORMATION).cast(),
            std::mem::size_of_val(&limits) as u32,
        )
    };
    if ok == 0 {
        return Err(io::Error::last_os_error());
    }
    Ok(job)
}

fn loopback() -> &'static Client {
    static CLIENT: OnceLock<Client> = OnceLock::new();
    CLIENT.get_or_init(|| {
        Client::builder()
            .no_proxy()
            .timeout(Duration::from_secs(1))
            .build()
            .expect("loopback HTTP client")
    })
}

async fn healthy(endpoint: &Endpoint, route: &str) -> bool {
    loopback()
        .get(format!("{}{route}", endpoint.base_url))
        .bearer_auth(&endpoint.api_key)
        .send()
        .await
        .is_ok_and(|response| response.status() == StatusCode::OK)
}

async fn launch(
    exe: &Path,
    weights: &Path,
    profile: Profile,
    hardware: &Hardware,
) -> Result<Running> {
    let listener = TcpListener::bind(("127.0.0.1", 0))
        .context("Could not reserve a local llama-server port")?;
    let port = listener.local_addr()?.port();
    let endpoint = Endpoint {
        base_url: format!("http://127.0.0.1:{port}"),
        api_key: crate::util::secret(),
        lease: Arc::new(()),
    };
    let mut command = engine_command(exe);
    os::quiet(&mut command)
        .kill_on_drop(true)
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::piped())
        .env("LLAMA_API_KEY", &endpoint.api_key)
        .args([
            "--host",
            "127.0.0.1",
            "--port",
            &port.to_string(),
            "--model",
        ])
        .arg(weights)
        .args([
            "--jinja",
            "--parallel",
            "1",
            "--ctx-size",
            &profile.context.to_string(),
            "--no-webui",
            "--no-slots",
            "--cache-ram",
            "0",
            "--no-cache-prompt",
            "--reasoning",
            "off",
        ]);
    if profile.backend == Backend::Cpu {
        command.args(["--device", "none", "--n-gpu-layers", "0"]);
    } else {
        command.args([
            "--n-gpu-layers",
            "99",
            "--flash-attn",
            "on",
            "-ctk",
            "q8_0",
            "-ctv",
            "q8_0",
        ]);
        if let Some(device) = hardware.gpu.as_ref().and_then(|gpu| gpu.device.as_deref()) {
            command.args(["--device", device]);
        }
    }

    #[cfg(target_os = "linux")]
    {
        let parent = unsafe { libc::getpid() };
        // Check the parent after arming the signal to cover an exit between fork and prctl.
        unsafe {
            command.pre_exec(move || {
                if libc::prctl(libc::PR_SET_PDEATHSIG, libc::SIGKILL) == -1 {
                    return Err(io::Error::last_os_error());
                }
                if libc::getppid() != parent {
                    return Err(io::Error::from_raw_os_error(libc::ECHILD));
                }
                Ok(())
            });
        }
    }
    #[cfg(windows)]
    let job = kill_on_close_job().context("Could not create the llama-server Job Object")?;
    drop(listener);
    let mut child = command
        .spawn()
        .with_context(|| format!("Could not launch {}", exe.display()))?;
    #[cfg(windows)]
    {
        use std::os::windows::io::AsRawHandle;
        use windows_sys::Win32::System::JobObjects::AssignProcessToJobObject;
        let process = child
            .raw_handle()
            .context("Missing llama-server process handle")?;
        if unsafe { AssignProcessToJobObject(job.as_raw_handle(), process) } == 0 {
            let error = io::Error::last_os_error();
            return Err(error).context("Could not contain llama-server in its Job Object");
        }
    }

    // Keep just enough stderr to diagnose a failed launch. Discard all stderr after startup.
    let mut stderr = child.stderr.take().context("Missing llama-server stderr")?;
    let tail = Arc::new(Mutex::new(Vec::new()));
    let capture = Arc::downgrade(&tail);
    let drain = tokio::spawn(async move {
        let mut buffer = [0; 2048];
        while let Ok(len @ 1..) = stderr.read(&mut buffer).await {
            if let Some(tail) = capture.upgrade() {
                let mut tail = tail.lock().unwrap();
                tail.extend_from_slice(&buffer[..len]);
                let excess = tail.len().saturating_sub(2048);
                tail.drain(..excess);
            }
        }
    });
    let ready = timeout(START_TIMEOUT, async {
        loop {
            if let Some(status) = child.try_wait()? {
                bail!("llama-server exited with {status}");
            }
            if healthy(&endpoint, "/health").await && healthy(&endpoint, "/v1/models").await {
                return Ok(());
            }
            sleep(Duration::from_millis(250)).await;
        }
    })
    .await;
    if !matches!(ready, Ok(Ok(()))) {
        let reason = match ready {
            Ok(Err(error)) => error.to_string(),
            _ => "llama-server did not become healthy within 60 seconds".into(),
        };
        let _ = stop_child(&mut child).await;
        let _ = timeout(Duration::from_secs(1), drain).await;
        let diagnostics =
            String::from_utf8_lossy(&tail.lock().unwrap()).replace(&endpoint.api_key, "[redacted]");
        bail!("{reason}. {}", diagnostics.trim());
    }
    Ok(Running {
        child,
        #[cfg(windows)]
        _job: job,
        endpoint,
        profile,
        last_active: Instant::now(),
    })
}

async fn stop_child(child: &mut Child) -> Result<()> {
    timeout(Duration::from_secs(5), child.kill())
        .await
        .context("Timed out stopping llama-server")??;
    Ok(())
}

#[derive(Clone, Serialize)]
struct Progress {
    state: &'static str,
    stage: &'static str,
    running: bool,
    bytes: u64,
    total: u64,
    pct: u64,
    error: Option<String>,
}

impl Progress {
    fn stage(state: &'static str, stage: &'static str) -> Self {
        Self {
            state,
            stage,
            running: matches!(
                state,
                "starting" | "downloading" | "extracting" | "checking"
            ),
            bytes: 0,
            total: 0,
            pct: if state == "ready" { 100 } else { 0 },
            error: None,
        }
    }
}

#[derive(Clone, Serialize)]
struct RuntimeStatus {
    state: &'static str,
    backend: Option<Backend>,
    context: Option<u32>,
    warning: Option<String>,
    error: Option<String>,
}

impl RuntimeStatus {
    fn new(state: &'static str) -> Self {
        Self {
            state,
            backend: None,
            context: None,
            warning: None,
            error: None,
        }
    }
}

#[derive(Default)]
struct Runtime {
    process: Option<Running>,
    force_cpu: Option<String>,
    crashes: VecDeque<Instant>,
    retry_at: Option<Instant>,
    monitor_active: bool,
}

impl Runtime {
    fn recent_crashes(&mut self) -> usize {
        self.crashes.retain(|when| when.elapsed() < CRASH_WINDOW);
        self.crashes.len()
    }

    async fn stop(&mut self) -> Result<()> {
        self.retry_at = None;
        self.crashes.clear();
        self.force_cpu = None;
        if let Some(mut process) = self.process.take() {
            stop_child(&mut process.child).await?;
        }
        Ok(())
    }

    async fn stop_for_install(&mut self) -> Result<()> {
        ensure!(
            !self
                .process
                .as_ref()
                .is_some_and(|process| process.endpoint.in_use()),
            "Wait for the active local model request to finish before changing the installation"
        );
        self.stop().await
    }
}

struct Manager {
    root: PathBuf,
    install: Arc<AsyncMutex<()>>,
    runtime: AsyncMutex<Runtime>,
    hardware: OnceCell<Hardware>,
    vulkan: AsyncMutex<HashMap<String, Option<Gpu>>>,
    progress_status: Mutex<Progress>,
    runtime_status: Mutex<RuntimeStatus>,
}

fn manager(core: &CoreRef) -> Arc<Manager> {
    static HOSTS: OnceLock<Mutex<HashMap<PathBuf, Arc<Manager>>>> = OnceLock::new();
    let path = core.data.join("models");
    let root = std::path::absolute(&path).unwrap_or(path);
    HOSTS
        .get_or_init(Mutex::default)
        .lock()
        .unwrap()
        .entry(root.clone())
        .or_insert_with(|| {
            Arc::new(Manager {
                root,
                install: Arc::default(),
                runtime: AsyncMutex::default(),
                hardware: OnceCell::new(),
                vulkan: AsyncMutex::default(),
                progress_status: Mutex::new(Progress::stage("idle", "")),
                runtime_status: Mutex::new(RuntimeStatus::new("stopped")),
            })
        })
        .clone()
}

impl Manager {
    fn progress(&self, core: &CoreRef, progress: Progress) {
        *self.progress_status.lock().unwrap() = progress.clone();
        core.bus
            .emit("local_llm", None, None, json!({ "progress": progress }));
    }

    fn runtime_status(&self, core: &CoreRef, state: RuntimeStatus) {
        *self.runtime_status.lock().unwrap() = state.clone();
        core.bus
            .emit("local_llm", None, None, json!({ "runtime": state }));
    }

    async fn download(
        &self,
        core: &CoreRef,
        stage: &'static str,
        asset: &Download,
        dest: &Path,
        extraction_space: u64,
    ) -> Result<()> {
        let mut last = None;
        download(asset, dest, extraction_space, |bytes| {
            let pct = bytes * 100 / asset.size;
            if last == Some(pct) {
                return;
            }
            last = Some(pct);
            self.progress(
                core,
                Progress {
                    bytes,
                    total: asset.size,
                    pct,
                    ..Progress::stage("downloading", stage)
                },
            );
        })
        .await
    }

    async fn detected(&self) -> &Hardware {
        self.hardware.get_or_init(probe_hardware).await
    }

    async fn hardware_for(&self, tag: &str) -> Hardware {
        let mut hardware = self.detected().await.clone();
        if !hardware.metal {
            let exe =
                platform_asset(true).and_then(|asset| installed_engine(&self.root, tag, &asset));
            hardware.gpu = if let Some(exe) = exe {
                let mut probes = self.vulkan.lock().await;
                if let Some(found) = probes.get(tag) {
                    found.clone()
                } else {
                    let found = probe_vulkan(&exe, hardware.gpu.as_ref()).await;
                    probes.insert(tag.into(), found.clone());
                    found
                }
            } else {
                None
            };
        }
        hardware
    }

    fn installed(&self, selection: &Selection) -> Result<(PathBuf, &'static Model)> {
        let model = model(&selection.model)?;
        let cpu =
            platform_asset(false).context("No prebuilt llama.cpp package for this platform")?;
        let executable = installed_engine(&self.root, &selection.tag, &cpu)
            .context("Run local model setup to install llama-server")?;
        ensure!(
            model.verified(&self.root),
            "Run local model setup to download or verify the weights"
        );
        Ok((executable, model))
    }

    async fn launch_selection(
        &self,
        selection: &Selection,
        runtime: &mut Runtime,
    ) -> Result<Running> {
        let (cpu, model) = self.installed(selection)?;
        let hardware = self.hardware_for(&selection.tag).await;
        let weights = model.weights(&self.root);
        let preferred = profile(&hardware, model, runtime.force_cpu.is_some());
        if preferred.backend != Backend::Cpu {
            let exe = platform_asset(true)
                .and_then(|asset| installed_engine(&self.root, &selection.tag, &asset))
                .unwrap_or_else(|| cpu.clone());
            match launch(&exe, &weights, preferred, &hardware).await {
                Ok(process) => return Ok(process),
                Err(error) => {
                    runtime.force_cpu = Some(format!("GPU launch failed. Using CPU. {error}"));
                }
            }
        }
        launch(&cpu, &weights, profile(&hardware, model, true), &hardware).await
    }

    async fn install_packages(
        &self,
        core: &CoreRef,
        tag: &str,
        release: Option<&Release>,
    ) -> Result<()> {
        let cpu =
            platform_asset(false).context("No prebuilt llama.cpp package for this platform")?;
        install_engine(core, self, tag, &cpu, release).await?;
        if vulkan_candidate(self.detected().await) {
            let gpu = platform_asset(true).unwrap();
            if let Err(error) = install_engine(core, self, tag, &gpu, release).await {
                tracing::warn!(%error, "Could not install Vulkan llama.cpp. CPU engine is available");
            }
        }
        Ok(())
    }

    async fn setup(&self, core: &CoreRef, requested: Option<String>) -> Result<()> {
        let mut current = Current::read(&self.root)?;
        let tag = current
            .active
            .as_ref()
            .map_or(PIN, |active| &active.tag)
            .to_string();
        let hardware = self.detected().await;
        let missing_package = [false, true]
            .into_iter()
            .filter(|gpu| !gpu || vulkan_candidate(hardware))
            .filter_map(platform_asset)
            .any(|asset| installed_engine(&self.root, &tag, &asset).is_none());
        let release = if tag != PIN && missing_package {
            Some(
                network()
                    .get(format!("{RELEASE_API}/tags/{tag}"))
                    .timeout(Duration::from_secs(15))
                    .send()
                    .await?
                    .error_for_status()?
                    .json()
                    .await?,
            )
        } else {
            None
        };
        self.install_packages(core, &tag, release.as_ref()).await?;
        self.vulkan.lock().await.remove(&tag);
        let hardware = self.hardware_for(&tag).await;
        let existing = current.active.as_ref().map(|active| active.model.as_str());
        let selected = requested
            .as_deref()
            .or(existing)
            .map(model)
            .transpose()?
            .unwrap_or_else(|| recommended(&hardware));
        if !selected.verified(&self.root) {
            let weights = selected.weights(&self.root);
            self.progress(core, Progress::stage("checking", "model"));
            if size_on_disk(&weights) != Some(selected.size)
                || sha256(&weights).await? != selected.sha
            {
                let asset = Download {
                    url: format!(
                        "https://huggingface.co/{}/resolve/{}/{}",
                        selected.repo, selected.revision, selected.file
                    ),
                    size: selected.size,
                    sha: selected.sha.into(),
                };
                self.download(core, "model", &asset, &weights, 0).await?;
            }
            os::write_atomic(&selected.checksum_file(&self.root), selected.sha.as_bytes())?;
        }
        let selection = Selection {
            tag,
            model: selected.id.into(),
        };
        if current.active.as_ref() != Some(&selection) {
            let mut runtime = self.runtime.lock().await;
            runtime.stop_for_install().await?;
            self.runtime_status(core, RuntimeStatus::new("stopped"));
            current.activate(selection);
            current.save(&self.root)?;
        }
        Ok(())
    }

    async fn update(&self, core: &CoreRef) -> Result<()> {
        let mut current = Current::read(&self.root)?;
        let active = current
            .active
            .clone()
            .context("Set up a local model before updating llama.cpp")?;
        let release = latest_release().await?;
        if !newer(&release.tag_name, &active.tag) {
            return Ok(());
        }
        self.install_packages(core, &release.tag_name, Some(&release))
            .await?;
        let selection = Selection {
            tag: release.tag_name,
            model: active.model,
        };
        self.progress(core, Progress::stage("checking", "engine"));
        let mut runtime = self.runtime.lock().await;
        runtime.stop_for_install().await?;
        self.runtime_status(core, RuntimeStatus::new("stopped"));
        let mut candidate_runtime = Runtime::default();
        let mut candidate = self
            .launch_selection(&selection, &mut candidate_runtime)
            .await?;
        stop_child(&mut candidate.child).await?;
        current.activate(selection);
        current.save(&self.root)?;
        runtime.force_cpu = candidate_runtime.force_cpu;
        Ok(())
    }

    async fn rollback(&self, core: &CoreRef) -> Result<()> {
        let mut current = Current::read(&self.root)?;
        let previous = current
            .previous
            .as_ref()
            .context("There is no previous local model installation")?;
        self.installed(previous)?;
        let mut runtime = self.runtime.lock().await;
        runtime.stop_for_install().await?;
        self.runtime_status(core, RuntimeStatus::new("stopped"));
        current.rollback();
        current.save(&self.root)
    }

    fn crashed(&self, core: &CoreRef, runtime: &mut Runtime, error: String) {
        runtime.recent_crashes();
        runtime.crashes.push_back(Instant::now());
        runtime.retry_at = BACKOFF
            .get(runtime.crashes.len() - 1)
            .map(|seconds| Instant::now() + Duration::from_secs(*seconds));
        let state = if runtime.retry_at.is_some() {
            "backoff"
        } else {
            "error"
        };
        self.runtime_status(
            core,
            RuntimeStatus {
                warning: runtime.force_cpu.clone(),
                error: Some(error),
                ..RuntimeStatus::new(state)
            },
        );
    }

    async fn failed_process(&self, core: &CoreRef, runtime: &mut Runtime, reason: String) {
        if let Some(mut process) = runtime.process.take() {
            if process.profile.backend != Backend::Cpu {
                runtime.force_cpu = Some(format!("{reason}. Switching to CPU."));
            }
            if let Err(error) = stop_child(&mut process.child).await {
                tracing::warn!(%error, "Could not reap llama-server");
            }
        }
        self.crashed(core, runtime, reason);
    }

    async fn start_locked(
        &self,
        core: &CoreRef,
        runtime: &mut Runtime,
        selection: &Selection,
    ) -> Result<Endpoint> {
        runtime.retry_at = None;
        self.runtime_status(core, RuntimeStatus::new("starting"));
        match self.launch_selection(selection, runtime).await {
            Ok(process) => {
                let state = RuntimeStatus {
                    backend: Some(process.profile.backend),
                    context: Some(process.profile.context),
                    warning: runtime.force_cpu.clone(),
                    ..RuntimeStatus::new("running")
                };
                let endpoint = process.endpoint.clone();
                runtime.process = Some(process);
                self.runtime_status(core, state);
                Ok(endpoint)
            }
            Err(error) => {
                self.crashed(core, runtime, format!("{error:#}"));
                Err(error)
            }
        }
    }

    async fn ensure(self: &Arc<Self>, core: &CoreRef) -> Result<Endpoint> {
        let mut runtime = self.runtime.lock().await;
        let selection = Current::read(&self.root)?
            .active
            .context("Set up the local model before starting it")?;
        self.installed(&selection)?;
        if let Some(process) = runtime.process.as_mut() {
            if process.child.try_wait()?.is_none() && healthy(&process.endpoint, "/health").await {
                process.last_active = Instant::now();
                return Ok(process.endpoint.clone());
            }
            self.failed_process(core, &mut runtime, "llama-server stopped responding".into())
                .await;
        }
        ensure!(
            runtime.recent_crashes() <= BACKOFF.len(),
            "llama-server stopped after repeated crashes. Stop and start it to retry"
        );
        if let Some(when) = runtime.retry_at {
            sleep(when.saturating_duration_since(Instant::now())).await;
        }
        let result = self.start_locked(core, &mut runtime, &selection).await;
        if !runtime.monitor_active && (result.is_ok() || runtime.retry_at.is_some()) {
            runtime.monitor_active = true;
            tokio::spawn(self.clone().monitor(core.clone()));
        }
        result
    }

    async fn monitor(self: Arc<Self>, core: CoreRef) {
        loop {
            sleep(Duration::from_secs(1)).await;
            let mut runtime = self.runtime.lock().await;
            if let Some(process) = runtime.process.as_mut() {
                let failure = match process.child.try_wait() {
                    Ok(Some(status)) => Some(format!("llama-server exited with {status}")),
                    Err(error) => Some(format!("Could not check llama-server: {error}")),
                    Ok(None) => None,
                };
                if let Some(reason) = failure {
                    self.failed_process(&core, &mut runtime, reason).await;
                } else if process
                    .endpoint
                    .idle(&mut process.last_active, Instant::now())
                {
                    let result = runtime.stop().await;
                    self.runtime_status(&core, stopped_status(result));
                }
            } else if let Some(when) = runtime.retry_at {
                if Instant::now() >= when {
                    let selection = Current::read(&self.root).and_then(|current| {
                        current
                            .active
                            .context("Local model installation is missing")
                    });
                    match selection {
                        Ok(selection) => {
                            let _ = self.start_locked(&core, &mut runtime, &selection).await;
                        }
                        Err(error) => {
                            runtime.retry_at = None;
                            self.runtime_status(
                                &core,
                                RuntimeStatus {
                                    error: Some(format!("{error:#}")),
                                    ..RuntimeStatus::new("error")
                                },
                            );
                        }
                    }
                }
            }
            if runtime.process.is_none() && runtime.retry_at.is_none() {
                runtime.monitor_active = false;
                return;
            }
        }
    }
}

fn stopped_status(result: Result<()>) -> RuntimeStatus {
    match result {
        Ok(()) => RuntimeStatus::new("stopped"),
        Err(error) => RuntimeStatus {
            error: Some(format!("{error:#}")),
            ..RuntimeStatus::new("error")
        },
    }
}

fn newer(candidate: &str, active: &str) -> bool {
    matches!((build_number(candidate), build_number(active)), (Some(a), Some(b)) if a > b)
}

/// Start on demand or reuse the single healthy server for this data directory.
pub async fn ensure_running(core: &CoreRef) -> Result<Endpoint> {
    let manager = manager(core);
    let core = core.clone();
    // A disconnected HTTP caller must not interrupt startup and strand the runtime state.
    tokio::spawn(async move { manager.ensure(&core).await }).await?
}

pub async fn status(core: &CoreRef) -> Value {
    let manager = manager(core);
    let detected = manager.detected().await;
    let (current, manifest_error) = match Current::read(&manager.root) {
        Ok(current) => (current, None),
        Err(error) => (Current::default(), Some(format!("{error:#}"))),
    };
    let active = current.active.as_ref();
    let hardware = match active {
        Some(selection) => manager.hardware_for(&selection.tag).await,
        None => detected.clone(),
    };
    let recommended = recommended(&hardware);
    let installed = active.and_then(|selection| manager.installed(selection).ok());
    let chosen = installed.as_ref().map_or(recommended, |(_, model)| *model);
    let selected_profile = profile(&hardware, chosen, false);
    let warning = if selected_profile.backend == Backend::Cpu
        && hardware.ram_mib.is_some_and(|ram| ram < 6144)
    {
        Some("CPU inference may run out of memory on hosts with less than 6 GiB of RAM.")
    } else {
        None
    };
    json!({
        "supported": platform_asset(false).is_some(),
        "ready": installed.is_some(),
        "pinned_tag": PIN,
        "active": current.active,
        "previous": current.previous,
        "hardware": { "ram_mib": detected.ram_mib, "gpu": hardware.gpu.as_ref().or(detected.gpu.as_ref()), "metal": detected.metal },
        "recommended_model": recommended.id,
        "profile": selected_profile,
        "models": MODELS.iter().map(|model| json!({ "id": model.id, "name": model.name, "bytes": model.size })).collect::<Vec<_>>(),
        "progress": manager.progress_status.lock().unwrap().clone(),
        "runtime": manager.runtime_status.lock().unwrap().clone(),
        "idle_seconds": IDLE.as_secs(),
        "warning": warning,
        "error": manifest_error,
    })
}

enum Install {
    Setup(Option<String>),
    Update,
    Rollback,
}

fn enqueue(core: CoreRef, operation: Install) -> R {
    let manager = manager(&core);
    let lock = manager
        .install
        .clone()
        .try_lock_owned()
        .map_err(|_| bad("Another local model installation operation is already running"))?;
    // A second Model Roamer process pointed at the same data directory shares its partial files.
    let lock_file = (|| -> Result<fs::File> {
        fs::create_dir_all(&manager.root)?;
        let file = fs::OpenOptions::new()
            .read(true)
            .write(true)
            .create(true)
            .truncate(false)
            .open(manager.root.join("install.lock"))?;
        file.try_lock()
            .context("Could not lock the local model installation")?;
        Ok(file)
    })()
    .map_err(|error| bad(format!("{error:#}")))?;
    let stage = match &operation {
        Install::Setup(_) => "setup",
        Install::Update => "update",
        Install::Rollback => "rollback",
    };
    manager.progress(&core, Progress::stage("starting", stage));
    tokio::spawn(async move {
        let _lock = lock;
        let _lock_file = lock_file;
        let result = match operation {
            Install::Setup(model) => manager.setup(&core, model).await,
            Install::Update => manager.update(&core).await,
            Install::Rollback => manager.rollback(&core).await,
        };
        let progress = match result {
            Ok(()) => Progress::stage("ready", stage),
            Err(error) => Progress {
                error: Some(format!("{error:#}")),
                ..Progress::stage("error", stage)
            },
        };
        manager.progress(&core, progress);
    });
    Ok(Json(json!({ "started": true })))
}

pub async fn api_status(State(core): State<CoreRef>) -> R {
    Ok(Json(status(&core).await))
}

#[derive(Deserialize)]
pub struct SetupQuery {
    model: Option<String>,
}

pub async fn api_setup(State(core): State<CoreRef>, Query(query): Query<SetupQuery>) -> R {
    if let Some(id) = &query.model {
        model(id).map_err(|error| bad(error.to_string()))?;
    }
    enqueue(core, Install::Setup(query.model))
}

pub async fn api_start(State(core): State<CoreRef>) -> R {
    // The bearer token is only exposed to the in-process callers of ensure_running.
    ensure_running(&core)
        .await
        .map_err(|error| bad(format!("{error:#}")))?;
    Ok(Json(status(&core).await))
}

pub async fn api_stop(State(core): State<CoreRef>) -> R {
    let manager = manager(&core);
    let mut runtime = manager.runtime.lock().await;
    let state = stopped_status(runtime.stop().await);
    manager.runtime_status(&core, state.clone());
    drop(runtime);
    if let Some(error) = state.error {
        return Err(bad(error));
    }
    Ok(Json(status(&core).await))
}

pub async fn api_check_update(State(core): State<CoreRef>) -> R {
    let manager = manager(&core);
    let current = Current::read(&manager.root).map_err(|error| bad(format!("{error:#}")))?;
    let active = current
        .active
        .as_ref()
        .map(|selection| selection.tag.as_str());
    let release = latest_release()
        .await
        .map_err(|error| bad(format!("Could not check llama.cpp updates: {error:#}")))?;
    let available = active.is_some_and(|tag| newer(&release.tag_name, tag))
        && platform_asset(false).is_some_and(|asset| {
            engine_download(&release.tag_name, &asset, Some(&release)).is_ok()
        });
    Ok(Json(
        json!({ "active_tag": active, "latest_tag": release.tag_name, "available": available }),
    ))
}

pub async fn api_update(State(core): State<CoreRef>) -> R {
    enqueue(core, Install::Update)
}

pub async fn api_rollback(State(core): State<CoreRef>) -> R {
    enqueue(core, Install::Rollback)
}

#[cfg(test)]
mod tests {
    use super::*;
    use axum::{http::HeaderMap, response::IntoResponse, routing::get, Router};
    use std::sync::atomic::{AtomicUsize, Ordering};

    struct Scratch(PathBuf);

    impl Scratch {
        fn new() -> Self {
            let dir =
                std::env::temp_dir().join(format!("roamer-local-llm-{}", uuid::Uuid::new_v4()));
            fs::create_dir(&dir).unwrap();
            Self(dir)
        }
    }

    impl Drop for Scratch {
        fn drop(&mut self) {
            let _ = fs::remove_dir_all(&self.0);
        }
    }

    #[test]
    fn selects_model_and_context_for_detected_memory() {
        let mut hardware = Hardware {
            ram_mib: Some(8192),
            ..Hardware::default()
        };
        let qwen = model("qwen3-4b-instruct-2507").unwrap();
        let gemma = model("gemma-4-e2b-it").unwrap();
        assert_eq!(recommended(&hardware).id, "smollm3-3b");
        assert_eq!(profile(&hardware, qwen, false).backend, Backend::Cpu);
        assert_eq!(profile(&Hardware::default(), qwen, false).context, 4096);

        for (memory, context) in [(4096, 8192), (6144, 12288)] {
            hardware.gpu = Some(Gpu {
                name: "GPU".into(),
                vram_mib: memory,
                device: None,
            });
            assert_eq!(recommended(&hardware).id, qwen.id);
            let selected = profile(&hardware, qwen, false);
            assert_eq!(selected.backend, Backend::Vulkan);
            assert_eq!(selected.context, context);
            assert_eq!(
                profile(&hardware, gemma, false).backend,
                if memory >= 4608 {
                    Backend::Vulkan
                } else {
                    Backend::Cpu
                }
            );
        }

        hardware.metal = true;
        assert_eq!(profile(&hardware, qwen, false).backend, Backend::Metal);
        assert_eq!(profile(&hardware, qwen, true).backend, Backend::Cpu);
    }

    #[test]
    fn prefers_the_detected_dedicated_gpu_over_shared_memory() {
        let gpu = Gpu {
            name: "NVIDIA GeForce RTX 4050 Laptop GPU".into(),
            vram_mib: 6141,
            device: None,
        };
        let output = "Available devices:\n  Vulkan0: Intel(R) UHD Graphics (8043 MiB, 7385 MiB free)\n  Vulkan1: NVIDIA GeForce RTX 4050 Laptop GPU (5920 MiB, 5152 MiB free)";
        let selected = vulkan_devices(output, Some(&gpu)).unwrap();
        assert_eq!(selected.device.as_deref(), Some("Vulkan1"));
        assert_eq!(selected.vram_mib, 5920);
    }

    #[test]
    fn uses_pinned_package_and_preserves_the_previous_installation() {
        let dir = Scratch::new();
        let cpu = platform_asset(false).unwrap();
        let poison = Release {
            tag_name: PIN.into(),
            assets: vec![ReleaseAsset {
                name: format!("llama-{PIN}-bin-{}", cpu.suffix),
                size: 1,
                digest: Some("sha256:bad".into()),
            }],
        };
        let pin = engine_download(PIN, &cpu, Some(&poison)).unwrap();
        assert_eq!(pin.size, cpu.size);
        assert_eq!(pin.sha, cpu.sha);
        assert!(pin.url.contains(&format!("/{PIN}/")));
        let next = format!("b{}", build_number(PIN).unwrap() + 1);
        assert!(newer(&next, PIN));
        assert!(!newer(PIN, &next));
        assert!(!newer("../b99999", PIN));
        assert!(engine_download(&next, &cpu, Some(&poison)).is_err());

        let mut current = Current::read(&dir.0).unwrap();
        let qwen = "qwen3-4b-instruct-2507".to_string();
        current.activate(Selection {
            tag: PIN.into(),
            model: qwen.clone(),
        });
        current.save(&dir.0).unwrap();
        let mut current = Current::read(&dir.0).unwrap();
        current.activate(Selection {
            tag: next.clone(),
            model: qwen,
        });
        current.activate(current.active.clone().unwrap());
        current.activate(Selection {
            tag: next.clone(),
            model: "smollm3-3b".into(),
        });
        current.save(&dir.0).unwrap();
        let mut current = Current::read(&dir.0).unwrap();
        assert_eq!(current.active.as_ref().unwrap().tag, next);
        assert_eq!(current.previous.as_ref().unwrap().tag, PIN);
        current.rollback();
        current.save(&dir.0).unwrap();
        let rolled_back = Current::read(&dir.0).unwrap();
        assert_eq!(rolled_back.active.unwrap().tag, PIN);
        assert_eq!(rolled_back.previous.unwrap().tag, next);
    }

    #[test]
    fn rejects_a_download_that_cannot_fit_on_disk() {
        let dir = Scratch::new();
        let error = check_space(&dir.0, u64::MAX, 0).unwrap_err();
        assert!(error.to_string().contains("free, but only"));
    }

    #[test]
    fn idle_timer_waits_for_the_last_endpoint_lease() {
        let endpoint = Endpoint {
            base_url: String::new(),
            api_key: String::new(),
            lease: Arc::new(()),
        };
        let mut last_active = Instant::now();
        let overdue = last_active + IDLE;
        let held_for_turn = endpoint.clone();
        assert!(!endpoint.idle(&mut last_active, overdue));
        drop(held_for_turn);
        assert!(!endpoint.idle(&mut last_active, overdue + IDLE - Duration::from_secs(1)));
        assert!(endpoint.idle(&mut last_active, overdue + IDLE));
    }

    async fn mock_download(
        respond: impl Fn(HeaderMap) -> axum::response::Response + Clone + Send + Sync + 'static,
    ) -> (Download, tokio::task::JoinHandle<io::Result<()>>) {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let url = format!("http://{}", listener.local_addr().unwrap());
        let router = Router::new().route(
            "/",
            get(move |headers: HeaderMap| async move { respond(headers) }),
        );
        let task = tokio::spawn(async move { axum::serve(listener, router).await });
        (
            Download {
                url,
                size: 6,
                sha: format!("{:x}", Sha256::digest(b"abcdef")),
            },
            task,
        )
    }

    #[tokio::test]
    async fn resumes_a_partial_download_and_publishes_only_verified_bytes() {
        let dir = Scratch::new();
        let dest = dir.0.join("asset.gguf");
        let part = dir.0.join("asset.gguf.part");
        fs::write(&part, b"abc").unwrap();
        let (asset, task) = mock_download(|headers| {
            if headers
                .get(header::RANGE)
                .is_some_and(|range| range == "bytes=3-")
            {
                (
                    StatusCode::PARTIAL_CONTENT,
                    [(header::CONTENT_RANGE, "bytes 3-5/6")],
                    "def",
                )
                    .into_response()
            } else {
                StatusCode::BAD_REQUEST.into_response()
            }
        })
        .await;
        let mut offsets = Vec::new();
        download(&asset, &dest, 0, |bytes| offsets.push(bytes))
            .await
            .unwrap();
        task.abort();
        assert_eq!(offsets.first(), Some(&3));
        assert_eq!(fs::read(&dest).unwrap(), b"abcdef");
        assert!(!part.exists());
    }

    #[tokio::test]
    async fn replaces_a_partial_file_when_server_ignores_range() {
        let dir = Scratch::new();
        let dest = dir.0.join("asset.gguf");
        fs::write(dir.0.join("asset.gguf.part"), b"xyz").unwrap();
        let (asset, task) = mock_download(|_| "abcdef".into_response()).await;
        download(&asset, &dest, 0, |_| {}).await.unwrap();
        task.abort();
        assert_eq!(fs::read(dest).unwrap(), b"abcdef");
    }

    #[tokio::test]
    async fn retries_bad_resume_and_sha_before_publishing() {
        let dir = Scratch::new();
        let dest = dir.0.join("asset.gguf");
        let part = dir.0.join("asset.gguf.part");
        fs::write(&part, b"abc").unwrap();
        let attempts = Arc::new(AtomicUsize::new(0));
        let request_count = attempts.clone();
        let (asset, task) =
            mock_download(
                move |headers| match request_count.fetch_add(1, Ordering::Relaxed) {
                    0 if headers.contains_key(header::RANGE) => (
                        StatusCode::PARTIAL_CONTENT,
                        [(header::CONTENT_RANGE, "bytes 2-5/6")],
                        "cdef",
                    )
                        .into_response(),
                    _ => "badsha".into_response(),
                },
            )
            .await;
        let error = download(&asset, &dest, 0, |_| {}).await.unwrap_err();
        task.abort();
        assert!(format!("{error:#}").contains("SHA-256 mismatch"));
        assert_eq!(attempts.load(Ordering::Relaxed), 3);
        assert!(!dest.exists());
        assert!(!part.exists());
    }
}
