//! Acquisition of the `intellij-server` backend, in resolution order:
//! 1. user-configured `binary.path`;
//! 2. an existing JetBrains install of the pinned version (shared with VS Code);
//! 3. download from `download.jetbrains.com` (pinned version + SHA-256).
//!
//! Layout inside the extension working directory:
//!   servers/<version>/            extracted backend (bin/, jbr/, EULA.txt, …)
//!   servers/.install-<ver>.lock/  install lock (mkdir-based, stale after 10 min)
//!   downloads/                    download + extraction staging
//!   system-path/<hash>/           per-worktree `--system-path`

use crate::eula;
use std::path::{Path, PathBuf};
use std::time::{Duration, SystemTime};
use zed_extension_api::{
    self as zed, current_platform, set_language_server_installation_status, Architecture,
    LanguageServerId, LanguageServerInstallationStatus, Os,
};

const BUNDLES_JSON: &str = include_str!("../server-bundles.json");
const PROXY_BUNDLES_JSON: &str = include_str!("../proxy-bundles.json");
const LOCK_STALE_AFTER: Duration = Duration::from_secs(600);
const LOCK_RETRY_DELAY: Duration = Duration::from_millis(200);
const LOCK_WAIT_TIMEOUT: Duration = Duration::from_secs(600);

/// Resolves the `ij-zed-proxy` binary, in order:
/// 1. user-configured `proxy_path` setting (trusted as-is: the WASM sandbox
///    cannot stat paths outside the extension working dir, but Zed spawns the
///    command on the host where the path is valid);
/// 2. a previously downloaded proxy in the extension working dir;
/// 3. download from the pinned GitHub Release (when published for this platform);
/// 4. `None` — the server runs without the proxy (direct mode).
pub fn resolve_proxy_path(
    proxy_path: Option<&str>,
    language_server_id: Option<&LanguageServerId>,
) -> Result<Option<PathBuf>, String> {
    if let Some(path) = proxy_path {
        return Ok(Some(PathBuf::from(path)));
    }

    let json: zed::serde_json::Value = zed::serde_json::from_str(PROXY_BUNDLES_JSON)
        .map_err(|e| format!("Corrupt embedded proxy-bundles.json: {e}"))?;
    let key = platform_key()?;
    let entry = &json["platforms"][key];
    let (Some(version), Some(sha256)) = (json["version"].as_str(), entry["sha256"].as_str())
    else {
        return Ok(None); // no proxy published for this platform
    };
    let proxy_name = match current_platform() {
        (Os::Windows, _) => "ij-zed-proxy.exe",
        _ => "ij-zed-proxy",
    };
    let workdir = working_dir()?;
    let proxy_dir = workdir.join("proxy").join(version);
    let proxy_bin = proxy_dir.join(proxy_name);
    if proxy_bin.exists() {
        return Ok(Some(proxy_bin));
    }

    if let Some(id) = language_server_id {
        set_language_server_installation_status(id, &LanguageServerInstallationStatus::Downloading);
    }
    let result = (|| -> Result<PathBuf, String> {
        std::fs::create_dir_all(&proxy_dir).map_err(|e| e.to_string())?;
        let base = json["releaseBaseUrl"]
            .as_str()
            .ok_or("proxy-bundles.json: missing releaseBaseUrl")?;
        let archive = format!("ij-zed-proxy-{key}");
        let url = format!("{base}/{archive}");
        let download_path = proxy_dir.join(&archive);
        zed::download_file(
            &url,
            download_path.to_str().ok_or("Non-UTF-8 path")?,
            zed::DownloadedFileType::Uncompressed,
        )?;
        let bytes = std::fs::read(&download_path).map_err(|e| e.to_string())?;
        let actual = eula::sha256_hex(&bytes);
        if actual != sha256 {
            let _ = std::fs::remove_file(&download_path);
            return Err(format!(
                "SHA-256 mismatch for {archive}: expected {sha256}, got {actual}"
            ));
        }
        if current_platform().0 != Os::Windows {
            zed::make_file_executable(
                download_path.to_str().ok_or("Non-UTF-8 path")?,
            )?;
        }
        std::fs::rename(&download_path, &proxy_bin).map_err(|e| e.to_string())?;
        Ok(proxy_bin)
    })();
    if let Some(id) = language_server_id {
        set_language_server_installation_status(id, &LanguageServerInstallationStatus::None);
    }
    // A proxy that cannot be fetched degrades to direct mode, not to a failure.
    Ok(result.ok())
}

pub struct Bundle {
    pub version: String,
    pub url: String,
    pub archive_name: String,
    pub sha256: String,
}

pub fn launcher_name() -> &'static str {
    match current_platform() {
        (Os::Windows, _) => "intellij-server.exe",
        _ => "intellij-server",
    }
}

fn launcher_path(server_dir: &Path) -> PathBuf {
    server_dir.join("bin").join(launcher_name())
}

/// Resolves the backend server directory (the one containing `bin/` and `EULA.txt`).
pub fn resolve_server_dir(
    binary_path: Option<&str>,
    shell_env: &[(String, String)],
    language_server_id: &LanguageServerId,
) -> Result<PathBuf, String> {
    if let Some(path) = binary_path {
        // Trusted as-is: the sandbox cannot stat paths outside the extension
        // working dir, but Zed spawns the command on the host.
        let launcher = PathBuf::from(path);
        return launcher
            .parent()
            .and_then(Path::parent)
            .map(Path::to_path_buf)
            .ok_or_else(|| {
                format!(
                    "Expected the intellij-server binary at <server>/bin/{}, got {}",
                    launcher_name(),
                    launcher.display()
                )
            });
    }

    let bundle = pinned_bundle()?;
    let workdir = working_dir()?;
    let local = workdir.join("servers").join(&bundle.version);
    if launcher_path(&local).exists() {
        return Ok(local);
    }
    if let Some(existing) = find_jetbrains_install(&bundle.version, shell_env) {
        return Ok(existing);
    }
    download_and_install(&bundle, &workdir, language_server_id)
}

fn pinned_bundle() -> Result<Bundle, String> {
    let json: zed::serde_json::Value = zed::serde_json::from_str(BUNDLES_JSON)
        .map_err(|e| format!("Corrupt embedded server-bundles.json: {e}"))?;
    let key = platform_key()?;
    let version = json["version"]
        .as_str()
        .ok_or("server-bundles.json: missing version")?;
    let entry = &json["platforms"][key];
    let get = |field: &str| {
        entry[field]
            .as_str()
            .map(str::to_owned)
            .ok_or_else(|| format!("server-bundles.json: missing {field} for {key}"))
    };
    Ok(Bundle {
        version: version.to_owned(),
        url: get("url")?,
        archive_name: get("archiveName")?,
        sha256: get("sha256")?,
    })
}

fn platform_key() -> Result<&'static str, String> {
    let (os, arch) = current_platform();
    let os_key = match os {
        Os::Mac => "darwin",
        Os::Linux => "linux",
        Os::Windows => "windows",
    };
    let arch_key = match arch {
        Architecture::Aarch64 => "aarch64",
        Architecture::X8664 => "x86_64",
        _ => return Err("Unsupported architecture for intellij-server".to_string()),
    };
    Ok(match (os_key, arch_key) {
        ("darwin", "aarch64") => "darwin-aarch64",
        ("darwin", "x86_64") => "darwin-x86_64",
        ("linux", "aarch64") => "linux-aarch64",
        ("linux", "x86_64") => "linux-x86_64",
        ("windows", "aarch64") => "windows-aarch64",
        ("windows", "x86_64") => "windows-x86_64",
        _ => unreachable!(),
    })
}

pub fn working_dir() -> Result<PathBuf, String> {
    std::env::current_dir().map_err(|e| format!("Cannot resolve extension working dir: {e}"))
}

/// Looks for a backend of the pinned version already installed by another
/// JetBrains client, in the editor-independent install locations used by the
/// official VS Code extension (serverBundleDownload.ts).
fn find_jetbrains_install(version: &str, shell_env: &[(String, String)]) -> Option<PathBuf> {
    let env = |key: &str| {
        shell_env
            .iter()
            .find(|(k, _)| k == key)
            .map(|(_, v)| v.as_str())
    };
    let data_home = match current_platform() {
        (Os::Mac, _) => env("HOME").map(|h| format!("{h}/Library/Application Support")),
        (Os::Windows, _) => env("LOCALAPPDATA")
            .map(str::to_owned)
            .or_else(|| env("HOME").map(|h| format!("{h}/AppData/Local"))),
        _ => env("XDG_DATA_HOME")
            .map(str::to_owned)
            .or_else(|| env("HOME").map(|h| format!("{h}/.local/share"))),
    }?;
    let servers_root = Path::new(&data_home).join("JetBrains/IntelliJServer/servers");
    // servers/<package-name>/<version>/bin/intellij-server
    for package_dir in std::fs::read_dir(&servers_root).ok()?.flatten() {
        let candidate = package_dir.path().join(version);
        if launcher_path(&candidate).exists() {
            return Some(candidate);
        }
    }
    None
}

fn download_and_install(
    bundle: &Bundle,
    workdir: &Path,
    language_server_id: &LanguageServerId,
) -> Result<PathBuf, String> {
    let servers_dir = workdir.join("servers");
    let final_dir = servers_dir.join(&bundle.version);
    let _lock = InstallLock::acquire(&servers_dir, &bundle.version)?;
    if launcher_path(&final_dir).exists() {
        return Ok(final_dir); // another process won the race
    }

    let downloads_dir = workdir.join("downloads");
    let archive_path = downloads_dir.join(&bundle.archive_name);
    let extract_dir = downloads_dir.join(format!("extract-{}", bundle.version));
    std::fs::create_dir_all(&downloads_dir)
        .and_then(|_| std::fs::create_dir_all(&extract_dir))
        .map_err(|e| format!("Cannot create download staging dirs: {e}"))?;

    set_language_server_installation_status(
        language_server_id,
        &LanguageServerInstallationStatus::Downloading,
    );
    let result = (|| -> Result<PathBuf, String> {
        // Downloaded uncompressed on every platform so the SHA-256 can be
        // verified before extraction, like the official client does.
        // The host HTTP client has no resume support, so retry whole attempts.
        let mut download_err = String::new();
        let mut downloaded = false;
        for attempt in 1..=3 {
            match zed::download_file(
                &bundle.url,
                archive_path.to_str().ok_or("Non-UTF-8 download path")?,
                zed::DownloadedFileType::Uncompressed,
            ) {
                Ok(()) => {
                    downloaded = true;
                    break;
                }
                Err(e) => {
                    download_err = e;
                    let _ = std::fs::remove_file(&archive_path);
                    if attempt < 3 {
                        std::thread::sleep(std::time::Duration::from_secs(2 * attempt as u64));
                    }
                }
            }
        }
        if !downloaded {
            return Err(format!(
                "Failed to download {} after 3 attempts: {download_err}",
                bundle.url
            ));
        }
        let bytes = std::fs::read(&archive_path)
            .map_err(|e| format!("Cannot read downloaded archive: {e}"))?;
        let actual = eula::sha256_hex(&bytes);
        if actual != bundle.sha256 {
            let _ = std::fs::remove_file(&archive_path);
            return Err(format!(
                "SHA-256 mismatch for {}: expected {}, got {}",
                bundle.archive_name, bundle.sha256, actual
            ));
        }

        let output = zed::process::Command::new("tar")
            .arg("-xf")
            .arg(archive_path.to_str().ok_or("Non-UTF-8 archive path")?)
            .arg("-C")
            .arg(extract_dir.to_str().ok_or("Non-UTF-8 extract path")?)
            .output()
            .map_err(|e| format!("Failed to run tar: {e}"))?;
        if output.status != Some(0) {
            return Err(format!(
                "tar extraction failed: {}",
                String::from_utf8_lossy(&output.stderr)
            ));
        }

        let candidate = find_server_root(&extract_dir).ok_or_else(|| {
            format!("No bin/{} found in the extracted archive", launcher_name())
        })?;
        if let Err(e) = std::fs::rename(&candidate, &final_dir) {
            if !launcher_path(&final_dir).exists() {
                return Err(format!("Cannot install server to {}: {e}", final_dir.display()));
            }
        }
        if current_platform().0 != Os::Windows {
            zed::make_file_executable(
                launcher_path(&final_dir)
                    .to_str()
                    .ok_or("Non-UTF-8 launcher path")?,
            )?;
        }
        Ok(final_dir)
    })();

    let _ = std::fs::remove_file(&archive_path);
    let _ = std::fs::remove_dir_all(&extract_dir);
    set_language_server_installation_status(
        language_server_id,
        &LanguageServerInstallationStatus::None,
    );
    result
}

/// The archive contains a single top-level directory; be tolerant about its name.
fn find_server_root(extract_dir: &Path) -> Option<PathBuf> {
    if launcher_path(extract_dir).exists() {
        return Some(extract_dir.to_path_buf());
    }
    for entry in std::fs::read_dir(extract_dir).ok()?.flatten() {
        let path = entry.path();
        if path.is_dir() && launcher_path(&path).exists() {
            return Some(path);
        }
    }
    None
}

/// Per-worktree `--system-path`, stable across restarts so the index persists.
pub fn system_path_for(worktree_root: &str) -> Result<PathBuf, String> {
    let hash = &eula::sha256_hex(worktree_root.as_bytes())[..16];
    let dir = working_dir()?.join("system-path").join(hash);
    std::fs::create_dir_all(&dir)
        .map_err(|e| format!("Cannot create {}: {e}", dir.display()))?;
    Ok(dir)
}

struct InstallLock {
    lock_dir: PathBuf,
}

impl InstallLock {
    fn acquire(servers_dir: &Path, version: &str) -> Result<Self, String> {
        std::fs::create_dir_all(servers_dir)
            .map_err(|e| format!("Cannot create {}: {e}", servers_dir.display()))?;
        let lock_dir = servers_dir.join(format!(".install-{version}.lock"));
        let deadline = SystemTime::now() + LOCK_WAIT_TIMEOUT;
        loop {
            match std::fs::create_dir(&lock_dir) {
                Ok(()) => return Ok(Self { lock_dir }),
                Err(_) => {
                    if is_stale(&lock_dir) {
                        let _ = std::fs::remove_dir_all(&lock_dir);
                        continue;
                    }
                    if SystemTime::now() > deadline {
                        return Err("Timed out waiting for the intellij-server install lock"
                            .to_string());
                    }
                    std::thread::sleep(LOCK_RETRY_DELAY);
                }
            }
        }
    }
}

impl Drop for InstallLock {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir(&self.lock_dir);
    }
}

fn is_stale(lock_dir: &Path) -> bool {
    std::fs::metadata(lock_dir)
        .and_then(|m| m.modified())
        .map(|t| t.elapsed().unwrap_or_default() > LOCK_STALE_AFTER)
        .unwrap_or(false)
}
