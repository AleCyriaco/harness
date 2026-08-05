use anyhow::{Context, Result, bail};
use std::io::{BufRead, BufReader, Read};
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::time::{Duration, Instant};

use super::safe_join;

const MAX_LIST_ENTRIES: usize = 400;
const MAX_READ_BYTES: usize = 256 * 1024;
const MAX_SEARCH_HITS: usize = 48;
const MAX_CMD_OUT: usize = 48_000;
const MAX_GLOB: usize = 200;
const MAX_TREE_NODES: usize = 300;

pub fn list_dir(root: &Path, rel: &str) -> Result<String> {
    let path = safe_join(root, rel)?;
    let entries =
        std::fs::read_dir(&path).with_context(|| format!("list_dir {}", path.display()))?;
    let mut lines = Vec::new();
    for entry in entries.flatten() {
        let meta = entry.metadata().ok();
        let kind = if meta.as_ref().map(|m| m.is_dir()).unwrap_or(false) {
            "dir"
        } else {
            "file"
        };
        let size = meta.map(|m| m.len()).unwrap_or(0);
        lines.push(format!(
            "{kind:>4} {size:>10}  {}",
            entry.file_name().to_string_lossy()
        ));
        if lines.len() >= MAX_LIST_ENTRIES {
            lines.push("…[list truncated]".into());
            break;
        }
    }
    lines.sort();
    if lines.is_empty() {
        Ok("(empty)".into())
    } else {
        Ok(lines.join("\n"))
    }
}

pub fn workspace_tree(root: &Path, rel: &str, max_depth: usize) -> Result<String> {
    let path = safe_join(root, rel)?;
    let mut lines = Vec::new();
    let mut count = 0usize;
    fn walk(
        dir: &Path,
        prefix: &str,
        depth: usize,
        max_depth: usize,
        lines: &mut Vec<String>,
        count: &mut usize,
    ) -> Result<()> {
        if *count >= MAX_TREE_NODES || depth > max_depth {
            return Ok(());
        }
        let mut entries: Vec<_> = std::fs::read_dir(dir)?.flatten().collect();
        entries.sort_by_key(|e| e.file_name());
        for entry in entries {
            if *count >= MAX_TREE_NODES {
                lines.push(format!("{prefix}…"));
                break;
            }
            let name = entry.file_name().to_string_lossy().into_owned();
            let p = entry.path();
            if p.is_dir() {
                if skip_dir(&name) {
                    continue;
                }
                lines.push(format!("{prefix}{name}/"));
                *count += 1;
                walk(
                    &p,
                    &format!("{prefix}  "),
                    depth + 1,
                    max_depth,
                    lines,
                    count,
                )?;
            } else {
                lines.push(format!("{prefix}{name}"));
                *count += 1;
            }
        }
        Ok(())
    }
    let label = if rel == "." || rel.is_empty() {
        ".".into()
    } else {
        rel.to_string()
    };
    lines.push(format!("{label}/"));
    count += 1;
    walk(
        &path,
        "  ",
        1,
        max_depth.max(1).min(6),
        &mut lines,
        &mut count,
    )?;
    Ok(lines.join("\n"))
}

pub fn glob_files(root: &Path, pattern: &str) -> Result<String> {
    if pattern.is_empty() {
        bail!("pattern is empty");
    }
    let pat = pattern.replace('\\', "/");
    let mut hits = Vec::new();
    let mut stack = vec![root.to_path_buf()];
    while let Some(dir) = stack.pop() {
        let Ok(rd) = std::fs::read_dir(&dir) else {
            continue;
        };
        for entry in rd.flatten() {
            let path = entry.path();
            if path.is_dir() {
                if let Some(name) = path.file_name().and_then(|s| s.to_str()) {
                    if skip_dir(name) {
                        continue;
                    }
                }
                stack.push(path);
                continue;
            }
            let rel = path
                .strip_prefix(root)
                .unwrap_or(&path)
                .to_string_lossy()
                .replace('\\', "/");
            if match_glob(&pat, &rel) {
                hits.push(rel);
                if hits.len() >= MAX_GLOB {
                    hits.push("…[glob cap]".into());
                    hits.sort();
                    return Ok(hits.join("\n"));
                }
            }
        }
    }
    hits.sort();
    if hits.is_empty() {
        Ok("(no matches)".into())
    } else {
        Ok(hits.join("\n"))
    }
}

fn match_glob(pattern: &str, path: &str) -> bool {
    if pattern.contains('*') {
        if let Some((pre, rest)) = pattern.split_once('*') {
            if let Some((mid, suf)) = rest.split_once('*') {
                return path.starts_with(pre) && path.contains(mid) && path.ends_with(suf);
            }
            return path.starts_with(pre) && path.ends_with(rest);
        }
    }
    path.contains(pattern)
}

pub fn read_file(
    root: &Path,
    rel: &str,
    start_line: Option<usize>,
    end_line: Option<usize>,
) -> Result<String> {
    let path = safe_join(root, rel)?;
    let file = std::fs::File::open(&path).with_context(|| format!("read {}", path.display()))?;
    let meta = file.metadata()?;
    if meta.len() > 8 * 1024 * 1024 && start_line.is_none() {
        bail!(
            "file is {} bytes — pass start_line/end_line to read a window",
            meta.len()
        );
    }

    let reader = BufReader::new(file);
    let start = start_line.unwrap_or(1).max(1);
    let end = end_line.unwrap_or(usize::MAX);

    let mut out = String::new();
    let mut bytes = 0usize;
    let mut line_no = 0usize;
    for line in reader.lines() {
        let line = line?;
        line_no += 1;
        if line_no < start {
            continue;
        }
        if line_no > end {
            break;
        }
        let add = line.len() + 1;
        if bytes + add > MAX_READ_BYTES {
            out.push_str(&format!(
                "\n…[read capped at {MAX_READ_BYTES} bytes; use a smaller line range]\n"
            ));
            break;
        }
        out.push_str(&format!("{line_no:>6}|{line}\n"));
        bytes += add;
    }
    if out.is_empty() {
        Ok(format!("(no lines in range {start}..{end})"))
    } else {
        Ok(out)
    }
}

pub fn write_file(root: &Path, rel: &str, content: &str) -> Result<String> {
    let path = safe_join(root, rel)?;
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)?;
    }
    std::fs::write(&path, content).with_context(|| format!("write {}", path.display()))?;
    Ok(format!("wrote {} ({} bytes)", path.display(), content.len()))
}

pub fn replace_in_file(root: &Path, rel: &str, old: &str, new: &str) -> Result<String> {
    if old.is_empty() {
        bail!("old_string is empty");
    }
    let path = safe_join(root, rel)?;
    let text = std::fs::read_to_string(&path).with_context(|| format!("read {}", path.display()))?;
    let count = text.matches(old).count();
    if count == 0 {
        bail!("old_string not found in {}", path.display());
    }
    if count > 1 {
        bail!("old_string found {count} times — make it unique");
    }
    let updated = text.replacen(old, new, 1);
    std::fs::write(&path, &updated)?;
    let diff = mini_diff(old, new);
    Ok(format!(
        "replaced 1 occurrence in {} ({} → {} bytes)\n{diff}",
        path.display(),
        text.len(),
        updated.len()
    ))
}

pub fn apply_patch(root: &Path, rel: &str, edits: &[(String, String)]) -> Result<String> {
    if edits.is_empty() {
        bail!("edits is empty");
    }
    let path = safe_join(root, rel)?;
    let mut text =
        std::fs::read_to_string(&path).with_context(|| format!("read {}", path.display()))?;
    let mut report = Vec::new();
    for (i, (old, new)) in edits.iter().enumerate() {
        if old.is_empty() {
            bail!("edit[{i}]: old_string empty");
        }
        let count = text.matches(old.as_str()).count();
        if count == 0 {
            bail!("edit[{i}]: old_string not found");
        }
        if count > 1 {
            bail!("edit[{i}]: old_string found {count} times — make unique");
        }
        text = text.replacen(old, new, 1);
        report.push(format!("edit[{i}] ok\n{}", mini_diff(old, new)));
    }
    std::fs::write(&path, &text)?;
    Ok(format!(
        "apply_patch {} ({} edits, {} bytes)\n{}",
        path.display(),
        edits.len(),
        text.len(),
        report.join("\n")
    ))
}

fn mini_diff(old: &str, new: &str) -> String {
    let old_l: Vec<&str> = old.lines().take(8).collect();
    let new_l: Vec<&str> = new.lines().take(8).collect();
    let mut s = String::from("  ---");
    for l in old_l {
        s.push_str(&format!("\n  - {l}"));
    }
    s.push_str("\n  +++");
    for l in new_l {
        s.push_str(&format!("\n  + {l}"));
    }
    s
}

pub fn search(root: &Path, query: &str, path_glob: Option<&str>) -> Result<String> {
    if query.is_empty() {
        bail!("query is empty");
    }
    let query_l = query.to_ascii_lowercase();
    let mut hits = Vec::new();
    let mut stack = vec![root.to_path_buf()];

    while let Some(dir) = stack.pop() {
        let Ok(rd) = std::fs::read_dir(&dir) else {
            continue;
        };
        for entry in rd.flatten() {
            let path = entry.path();
            if path.is_dir() {
                if let Some(name) = path.file_name().and_then(|s| s.to_str()) {
                    if skip_dir(name) {
                        continue;
                    }
                }
                stack.push(path);
                continue;
            }
            if !is_textish(&path) {
                continue;
            }
            if let Some(g) = path_glob {
                let s = path.to_string_lossy().replace('\\', "/");
                if !match_glob(g, &s) && !s.contains(g) {
                    continue;
                }
            }
            if let Ok(file) = std::fs::File::open(&path) {
                let mut reader = BufReader::new(file.take(512 * 1024));
                let mut line_no = 0u32;
                let mut symbols: Vec<(u32, String)> = Vec::new();
                let mut file_hits: Vec<(u32, String)> = Vec::new();
                let mut buf = String::new();
                loop {
                    buf.clear();
                    let n = reader.read_line(&mut buf)?;
                    if n == 0 {
                        break;
                    }
                    line_no += 1;
                    let trimmed = buf.trim_end_matches(['\n', '\r']);
                    if let Some(sym) = detect_symbol(trimmed) {
                        if symbols.len() < 80 {
                            symbols.push((line_no, sym));
                        }
                    }
                    if trimmed.to_ascii_lowercase().contains(&query_l) {
                        file_hits.push((line_no, trimmed.chars().take(200).collect()));
                        if file_hits.len() >= 8 {
                            break;
                        }
                    }
                }
                if !file_hits.is_empty() {
                    let rel = path.strip_prefix(root).unwrap_or(&path);
                    for (ln, text) in &file_hits {
                        let outline = nearby_symbols(&symbols, *ln);
                        hits.push(format!(
                            "{}:{}: {}\n  symbols: {}",
                            rel.display(),
                            ln,
                            text,
                            outline
                        ));
                        if hits.len() >= MAX_SEARCH_HITS {
                            hits.push("…[search hit cap]".into());
                            return Ok(hits.join("\n\n"));
                        }
                    }
                }
            }
        }
    }

    if hits.is_empty() {
        Ok("(no matches)".into())
    } else {
        Ok(hits.join("\n\n"))
    }
}

pub fn git_status(root: &Path) -> Result<String> {
    run_git(root, &["status", "-sb", "--untracked-files=normal"])
}

pub fn git_diff(root: &Path, path: Option<&str>, staged: bool) -> Result<String> {
    let mut args: Vec<String> = vec![
        "diff".into(),
        "--stat".into(),
        "--patch".into(),
        "--no-color".into(),
    ];
    if staged {
        args.push("--cached".into());
    }
    if let Some(p) = path {
        args.push("--".into());
        args.push(p.to_string());
    }
    let refs: Vec<&str> = args.iter().map(|s| s.as_str()).collect();
    let out = run_git(root, &refs)?;
    if out.len() > 40_000 {
        Ok(format!(
            "{}…\n[diff truncated {} chars]",
            &out[..40_000],
            out.len() - 40_000
        ))
    } else {
        Ok(out)
    }
}

pub fn git_log(root: &Path, n: usize) -> Result<String> {
    let n = n.clamp(1, 30).to_string();
    run_git(
        root,
        &["log", "-n", &n, "--oneline", "--decorate", "--no-color"],
    )
}

fn run_git(root: &Path, args: &[&str]) -> Result<String> {
    let output = Command::new("git")
        .args(args)
        .current_dir(root)
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .output()
        .context("git not available")?;
    let mut out = String::from_utf8_lossy(&output.stdout).into_owned();
    if !output.stderr.is_empty() {
        let err = String::from_utf8_lossy(&output.stderr);
        if out.is_empty() {
            out = err.into_owned();
        } else if !output.status.success() {
            out.push_str("\n--- stderr ---\n");
            out.push_str(&err);
        }
    }
    if out.is_empty() {
        out = if output.status.success() {
            "(clean / empty)".into()
        } else {
            format!("(git exit {})", output.status.code().unwrap_or(-1))
        };
    }
    Ok(out)
}

pub fn run_command(root: &Path, command: &str) -> Result<String> {
    if is_destructive(command) {
        bail!("blocked potentially destructive command");
    }
    let _ = std::fs::create_dir_all(root);

    #[cfg(windows)]
    let mut child = Command::new("cmd")
        .args(["/C", command])
        .current_dir(root)
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .context("spawn command")?;

    #[cfg(not(windows))]
    let mut child = Command::new("sh")
        .args(["-lc", command])
        .current_dir(root)
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .context("spawn command")?;

    let mut stdout = child.stdout.take().context("stdout")?;
    let mut stderr = child.stderr.take().context("stderr")?;
    let start = Instant::now();

    loop {
        if start.elapsed() > Duration::from_secs(90) {
            let _ = child.kill();
            let _ = child.wait();
            bail!("command timed out after 90s");
        }
        match child.try_wait()? {
            Some(status) => {
                let mut out_buf = Vec::new();
                let mut err_buf = Vec::new();
                let _ = stdout.read_to_end(&mut out_buf);
                let _ = stderr.read_to_end(&mut err_buf);
                if out_buf.len() > MAX_CMD_OUT {
                    out_buf.truncate(MAX_CMD_OUT);
                }
                if err_buf.len() > MAX_CMD_OUT / 2 {
                    err_buf.truncate(MAX_CMD_OUT / 2);
                }
                let mut out = String::from_utf8_lossy(&out_buf).into_owned();
                if !err_buf.is_empty() {
                    if !out.is_empty() {
                        out.push('\n');
                    }
                    out.push_str("--- stderr ---\n");
                    out.push_str(&String::from_utf8_lossy(&err_buf));
                }
                if out.is_empty() {
                    out = format!("(exit {})", status.code().unwrap_or(-1));
                } else if !status.success() {
                    out.push_str(&format!("\n(exit {})", status.code().unwrap_or(-1)));
                }
                return Ok(out);
            }
            None => std::thread::sleep(Duration::from_millis(40)),
        }
    }
}

fn skip_dir(name: &str) -> bool {
    matches!(
        name,
        "target"
            | ".git"
            | "node_modules"
            | "dist"
            | "build"
            | ".venv"
            | "venv"
            | "__pycache__"
            | ".idea"
            | ".vscode"
            | "vendor"
    )
}

fn is_textish(path: &Path) -> bool {
    match path.extension().and_then(|s| s.to_str()).unwrap_or("") {
        "rs" | "py" | "js" | "ts" | "tsx" | "jsx" | "go" | "java" | "kt" | "c" | "h" | "cpp"
        | "hpp" | "cs" | "rb" | "php" | "swift" | "md" | "toml" | "yaml" | "yml" | "json"
        | "txt" | "css" | "html" | "sh" | "zsh" | "bash" | "sql" | "xml" | "gradle" | "cmake"
        | "lock" | "scss" | "vue" | "svelte" => true,
        "" => path
            .file_name()
            .and_then(|s| s.to_str())
            .is_some_and(|n| {
                matches!(
                    n,
                    "Makefile" | "Dockerfile" | "Cargo.toml" | "package.json" | "README"
                        | "LICENSE" | "Gemfile"
                )
            }),
        _ => false,
    }
}

fn detect_symbol(line: &str) -> Option<String> {
    let t = line.trim();
    if t.starts_with("//") || t.starts_with('#') || t.starts_with("/*") {
        return None;
    }
    let patterns = [
        "fn ",
        "pub fn ",
        "async fn ",
        "pub async fn ",
        "def ",
        "class ",
        "struct ",
        "pub struct ",
        "enum ",
        "pub enum ",
        "impl ",
        "trait ",
        "pub trait ",
        "function ",
        "export function ",
        "export const ",
        "const ",
        "type ",
        "interface ",
        "func ",
    ];
    for p in patterns {
        if let Some(rest) = t.strip_prefix(p) {
            let name: String = rest
                .chars()
                .take_while(|c| c.is_alphanumeric() || *c == '_' || *c == ':')
                .collect();
            if !name.is_empty() {
                return Some(format!("{p}{name}"));
            }
        }
    }
    None
}

fn nearby_symbols(symbols: &[(u32, String)], line: u32) -> String {
    let mut best: Vec<&str> = Vec::new();
    for (ln, name) in symbols.iter().rev() {
        if *ln <= line {
            best.push(name.as_str());
            if best.len() >= 3 {
                break;
            }
        }
    }
    if best.is_empty() {
        "—".into()
    } else {
        best.join(" · ")
    }
}

fn is_destructive(cmd: &str) -> bool {
    let c = cmd.to_lowercase();
    const BAD: &[&str] = &[
        "rm -rf /",
        "rm -rf /*",
        "mkfs",
        "diskutil erase",
        ":(){",
        "shutdown",
        "reboot",
        "format c:",
        "del /s /q c:\\",
        "rd /s /q c:",
    ];
    BAD.iter().any(|b| c.contains(b))
}

#[allow(dead_code)]
pub fn path_buf(root: &Path, rel: &str) -> Result<PathBuf> {
    safe_join(root, rel)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn glob_simple() {
        assert!(match_glob("*.rs", "src/main.rs"));
        assert!(match_glob("src/*", "src/main.rs"));
        assert!(!match_glob("*.rs", "src/main.py"));
    }
}
