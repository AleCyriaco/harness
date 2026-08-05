//! Self-dev: build harness from source and reload binary (jcode-inspired).

use anyhow::{Context, Result, bail};
use std::path::{Path, PathBuf};
use std::process::Command;

pub fn repo_root() -> PathBuf {
    // Prefer CARGO_MANIFEST_DIR at compile time when running from dev tree
    let manifest = PathBuf::from(env!("CARGO_MANIFEST_DIR"));
    if manifest.join("Cargo.toml").exists() {
        return manifest;
    }
    std::env::current_dir().unwrap_or_else(|_| PathBuf::from("."))
}

pub fn status() -> String {
    let root = repo_root();
    let exe = std::env::current_exe()
        .map(|p| p.display().to_string())
        .unwrap_or_else(|_| "?".into());
    format!(
        "version={} exe={} source={} release_bin={}",
        env!("CARGO_PKG_VERSION"),
        exe,
        root.display(),
        root.join("target/release/harness").display()
    )
}

pub fn build_release() -> Result<String> {
    let root = repo_root();
    if !root.join("Cargo.toml").exists() {
        bail!("no Cargo.toml at {}", root.display());
    }
    let out = Command::new("cargo")
        .args(["build", "--release", "-q"])
        .current_dir(&root)
        .output()
        .context("cargo build")?;
    if !out.status.success() {
        let err = String::from_utf8_lossy(&out.stderr);
        bail!("build failed:\n{err}");
    }
    let bin = root.join("target/release/harness");
    #[cfg(windows)]
    let bin = root.join("target/release/harness.exe");
    if !bin.exists() {
        bail!("binary missing after build: {}", bin.display());
    }
    Ok(format!("built {}", bin.display()))
}

/// Replace current process with freshly built binary (sessions should use daemon).
pub fn reload() -> Result<()> {
    let root = repo_root();
    let _ = build_release()?;
    #[cfg(windows)]
    let bin = root.join("target/release/harness.exe");
    #[cfg(not(windows))]
    let bin = root.join("target/release/harness");
    let err = Command::new(&bin).args(std::env::args().skip(1)).exec_replace();
    Err(err.into())
}

trait ExecReplace {
    fn exec_replace(&mut self) -> std::io::Error;
}

impl ExecReplace for Command {
    #[cfg(unix)]
    fn exec_replace(&mut self) -> std::io::Error {
        use std::os::unix::process::CommandExt;
        self.exec()
    }
    #[cfg(not(unix))]
    fn exec_replace(&mut self) -> std::io::Error {
        match self.spawn() {
            Ok(_) => std::process::exit(0),
            Err(e) => e,
        }
    }
}

pub fn tool_selfdev(action: &str, workspace_hint: &Path) -> Result<String> {
    let _ = workspace_hint;
    match action {
        "status" => Ok(status()),
        "build" => build_release(),
        "reload" => {
            // Don't reload mid-tool from agent without care — return instructions
            let msg = build_release()?;
            Ok(format!(
                "{msg}\nTo reload: run `harness self-dev reload` or restart the app/daemon."
            ))
        }
        _ => bail!("selfdev action: status|build|reload"),
    }
}
