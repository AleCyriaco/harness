//! Auto-update: check GitHub releases, download, stage, apply on restart.

use anyhow::{Context, Result, bail};
use directories::ProjectDirs;
use serde::Deserialize;
use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command;

use crate::llm::http_client;

pub const CURRENT_VERSION: &str = env!("CARGO_PKG_VERSION");

#[derive(Debug, Clone, Default)]
pub struct UpdateStatus {
    pub current: String,
    pub latest: Option<String>,
    pub notes: String,
    pub download_url: Option<String>,
    pub staged_path: Option<PathBuf>,
    pub message: String,
}

#[derive(Deserialize)]
struct GhRelease {
    tag_name: String,
    body: Option<String>,
    assets: Vec<GhAsset>,
}

#[derive(Deserialize)]
struct GhAsset {
    name: String,
    browser_download_url: String,
}

pub fn default_repo() -> String {
    std::env::var("HARNESS_UPDATE_REPO").unwrap_or_else(|_| "AleCyriaco/harness".into())
}

pub fn check_for_update(repo: &str) -> Result<UpdateStatus> {
    let mut st = UpdateStatus {
        current: CURRENT_VERSION.to_string(),
        message: "checking…".into(),
        ..Default::default()
    };
    let url = format!("https://api.github.com/repos/{repo}/releases/latest");
    let resp = http_client()
        .get(&url)
        .header("User-Agent", format!("harness/{CURRENT_VERSION}"))
        .header("Accept", "application/vnd.github+json")
        .send()
        .context("update check request")?;

    if resp.status().as_u16() == 404 {
        st.message = format!("no releases on {repo} (publish a GitHub release to enable updates)");
        return Ok(st);
    }
    if !resp.status().is_success() {
        let code = resp.status();
        let t = resp.text().unwrap_or_default();
        bail!("GitHub API {code}: {t}");
    }

    let rel: GhRelease = resp.json().context("parse release")?;
    let latest = rel.tag_name.trim_start_matches('v').to_string();
    st.latest = Some(latest.clone());
    st.notes = rel.body.unwrap_or_default().chars().take(800).collect();

    if version_gt(&latest, CURRENT_VERSION) {
        let asset = pick_asset(&rel.assets);
        st.download_url = asset.map(|a| a.browser_download_url.clone());
        st.message = format!("update available: {CURRENT_VERSION} → {latest}");
    } else {
        st.message = format!("up to date ({CURRENT_VERSION})");
    }
    Ok(st)
}

fn pick_asset(assets: &[GhAsset]) -> Option<&GhAsset> {
    let os = std::env::consts::OS;
    let arch = std::env::consts::ARCH;
    let candidates: &[&str] = match (os, arch) {
        ("macos", "aarch64") => &["darwin-aarch64", "macos-arm64", "aarch64-apple", "macos"],
        ("macos", _) => &["darwin-x86_64", "macos-x64", "x86_64-apple", "macos"],
        ("linux", "aarch64") => &["linux-aarch64", "linux-arm64", "aarch64-unknown-linux"],
        ("linux", _) => &["linux-x86_64", "linux-amd64", "x86_64-unknown-linux"],
        ("windows", _) => &["windows-x86_64", "windows-amd64", "pc-windows", "windows"],
        _ => &["harness"],
    };
    for c in candidates {
        if let Some(a) = assets.iter().find(|a| {
            let n = a.name.to_ascii_lowercase();
            n.contains(c) && (n.contains("harness") || n.ends_with(".tar.gz") || n.ends_with(".zip") || !n.contains('.'))
        }) {
            return Some(a);
        }
    }
    assets.iter().find(|a| a.name.to_ascii_lowercase().contains("harness"))
}

fn version_gt(a: &str, b: &str) -> bool {
    let pa = parse_ver(a);
    let pb = parse_ver(b);
    pa > pb
}

fn parse_ver(s: &str) -> (u64, u64, u64) {
    let mut parts = s.split(|c| c == '.' || c == '-');
    let major = parts.next().and_then(|x| x.parse().ok()).unwrap_or(0);
    let minor = parts.next().and_then(|x| x.parse().ok()).unwrap_or(0);
    let patch = parts.next().and_then(|x| x.parse().ok()).unwrap_or(0);
    (major, minor, patch)
}

pub fn download_update(url: &str) -> Result<PathBuf> {
    let dirs = ProjectDirs::from("sh", "harness", "harness").context("dirs")?;
    let dir = dirs.data_dir().join("updates");
    fs::create_dir_all(&dir)?;
    let resp = http_client()
        .get(url)
        .header("User-Agent", format!("harness/{CURRENT_VERSION}"))
        .send()
        .context("download")?;
    if !resp.status().is_success() {
        bail!("download HTTP {}", resp.status());
    }
    let bytes = resp.bytes()?.to_vec();
    if bytes.len() < 1000 {
        bail!("download too small ({} bytes)", bytes.len());
    }
    let dest = dir.join(asset_name_from_url(url));
    fs::write(&dest, &bytes)?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        // If looks like raw binary, mark executable
        if !dest.extension().is_some_and(|e| {
            matches!(e.to_str(), Some("zip" | "gz" | "tgz"))
        }) {
            let mut perms = fs::metadata(&dest)?.permissions();
            perms.set_mode(0o755);
            fs::set_permissions(&dest, perms)?;
        }
    }
    Ok(dest)
}

fn asset_name_from_url(url: &str) -> String {
    url.rsplit('/')
        .next()
        .unwrap_or("harness-update.bin")
        .to_string()
}

/// Replace current executable with staged binary (best-effort).
pub fn apply_update(staged: &Path) -> Result<String> {
    let current = std::env::current_exe().context("current_exe")?;
    if !staged.exists() {
        bail!("staged file missing");
    }
    // Archives need manual extract — only raw binaries auto-apply.
    if staged
        .extension()
        .and_then(|e| e.to_str())
        .is_some_and(|e| matches!(e, "zip" | "gz" | "tgz"))
    {
        return Ok(format!(
            "downloaded archive to {} — extract and replace {} manually",
            staged.display(),
            current.display()
        ));
    }

    let bak = current.with_extension("bak");
    let _ = fs::remove_file(&bak);
    fs::rename(&current, &bak).with_context(|| format!("backup {}", current.display()))?;
    match fs::copy(staged, &current) {
        Ok(_) => {
            #[cfg(unix)]
            {
                use std::os::unix::fs::PermissionsExt;
                let mut perms = fs::metadata(&current)?.permissions();
                perms.set_mode(0o755);
                fs::set_permissions(&current, perms)?;
            }
            Ok(format!(
                "updated binary at {} (backup {}). Restart harness.",
                current.display(),
                bak.display()
            ))
        }
        Err(e) => {
            let _ = fs::rename(&bak, &current);
            Err(e).context("copy new binary")
        }
    }
}

/// Optional: relaunch after apply.
pub fn relaunch() -> Result<()> {
    let exe = std::env::current_exe()?;
    Command::new(exe).spawn()?;
    std::process::exit(0);
}

#[allow(dead_code)]
pub fn pending_update_path() -> Option<PathBuf> {
    let dirs = ProjectDirs::from("sh", "harness", "harness")?;
    let dir = dirs.data_dir().join("updates");
    let rd = fs::read_dir(dir).ok()?;
    rd.flatten()
        .map(|e| e.path())
        .filter(|p| p.is_file())
        .max_by_key(|p| p.metadata().and_then(|m| m.modified()).ok())
}
