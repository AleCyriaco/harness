//! Simple hooks — scripts under .harness/hooks/ (pre_turn / post_turn / post_tool).

use std::path::Path;
use std::process::Command;

pub fn run_hook(workspace: &Path, name: &str, payload: &str) -> Option<String> {
    let path = workspace.join(".harness").join("hooks").join(name);
    #[cfg(windows)]
    let script = if path.with_extension("bat").exists() {
        path.with_extension("bat")
    } else if path.with_extension("cmd").exists() {
        path.with_extension("cmd")
    } else {
        path.clone()
    };
    #[cfg(not(windows))]
    let script = if path.exists() {
        path.clone()
    } else if path.with_extension("sh").exists() {
        path.with_extension("sh")
    } else {
        return None;
    };
    if !script.exists() {
        return None;
    }
    #[cfg(windows)]
    let output = Command::new("cmd")
        .args(["/C", &script.display().to_string(), payload])
        .current_dir(workspace)
        .output()
        .ok()?;
    #[cfg(not(windows))]
    let output = Command::new("sh")
        .arg(&script)
        .arg(payload)
        .current_dir(workspace)
        .output()
        .ok()?;
    let mut s = String::from_utf8_lossy(&output.stdout).into_owned();
    if !output.stderr.is_empty() {
        s.push_str(&String::from_utf8_lossy(&output.stderr));
    }
    if s.len() > 4000 {
        s.truncate(4000);
        s.push_str("…");
    }
    if s.trim().is_empty() {
        None
    } else {
        Some(s)
    }
}
