//! Background jobs — long-running shell without blocking the agent turn.

use anyhow::{Result, bail};
use std::collections::HashMap;
use std::process::{Child, Command, Stdio};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Mutex;
use std::time::Instant;

#[derive(Debug, Clone)]
pub struct JobInfo {
    pub id: u64,
    pub command: String,
    pub status: String,
    pub started: String,
    pub output_preview: String,
}

struct Job {
    info: JobInfo,
    child: Option<Child>,
    started_at: Instant,
}

static NEXT: AtomicU64 = AtomicU64::new(1);
static JOBS: Mutex<Option<HashMap<u64, Job>>> = Mutex::new(None);

fn with_jobs<T>(f: impl FnOnce(&mut HashMap<u64, Job>) -> T) -> T {
    let mut g = JOBS.lock().expect("jobs lock");
    if g.is_none() {
        *g = Some(HashMap::new());
    }
    f(g.as_mut().unwrap())
}

pub fn start(cwd: &std::path::Path, command: &str) -> Result<JobInfo> {
    if command.trim().is_empty() {
        bail!("empty command");
    }
    let id = NEXT.fetch_add(1, Ordering::Relaxed);
    #[cfg(windows)]
    let child = Command::new("cmd")
        .args(["/C", command])
        .current_dir(cwd)
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()?;
    #[cfg(not(windows))]
    let child = Command::new("sh")
        .args(["-lc", command])
        .current_dir(cwd)
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()?;

    let info = JobInfo {
        id,
        command: command.to_string(),
        status: "running".into(),
        started: chrono::Local::now().to_rfc3339(),
        output_preview: String::new(),
    };
    with_jobs(|g| {
        if g.len() > 20 {
            g.retain(|_, j| j.info.status == "running");
        }
        g.insert(
            id,
            Job {
                info: info.clone(),
                child: Some(child),
                started_at: Instant::now(),
            },
        );
    });
    Ok(info)
}

pub fn poll(id: u64) -> Result<JobInfo> {
    with_jobs(|g| {
        let job = g
            .get_mut(&id)
            .ok_or_else(|| anyhow::anyhow!("unknown job {id}"))?;
        if let Some(child) = job.child.as_mut() {
            match child.try_wait()? {
                Some(status) => {
                    let mut out = String::new();
                    if let Some(mut stdout) = child.stdout.take() {
                        use std::io::Read;
                        let mut buf = Vec::new();
                        let _ = stdout.read_to_end(&mut buf);
                        out.push_str(&String::from_utf8_lossy(&buf));
                    }
                    if let Some(mut stderr) = child.stderr.take() {
                        use std::io::Read;
                        let mut buf = Vec::new();
                        let _ = stderr.read_to_end(&mut buf);
                        if !buf.is_empty() {
                            out.push_str("\n--- stderr ---\n");
                            out.push_str(&String::from_utf8_lossy(&buf));
                        }
                    }
                    if out.len() > 12_000 {
                        out.truncate(12_000);
                        out.push_str("\n…[truncated]");
                    }
                    job.info.status = format!("exit {}", status.code().unwrap_or(-1));
                    job.info.output_preview = out;
                    job.child = None;
                }
                None => {
                    job.info.status =
                        format!("running {}s", job.started_at.elapsed().as_secs());
                }
            }
        }
        Ok(job.info.clone())
    })
}

pub fn kill(id: u64) -> Result<String> {
    with_jobs(|g| {
        let job = g
            .get_mut(&id)
            .ok_or_else(|| anyhow::anyhow!("unknown job {id}"))?;
        if let Some(child) = job.child.as_mut() {
            let _ = child.kill();
            let _ = child.wait();
            job.child = None;
            job.info.status = "killed".into();
            Ok(format!("killed job {id}"))
        } else {
            Ok(format!("job {id} already finished ({})", job.info.status))
        }
    })
}

pub fn list() -> Vec<JobInfo> {
    let ids: Vec<u64> = with_jobs(|g| g.keys().copied().collect());
    for id in ids {
        let _ = poll(id);
    }
    with_jobs(|g| g.values().map(|j| j.info.clone()).collect())
}

pub fn format_list() -> String {
    let jobs = list();
    if jobs.is_empty() {
        return "(no background jobs)".into();
    }
    jobs.iter()
        .map(|j| {
            format!(
                "#{} [{}] {} — {}",
                j.id,
                j.status,
                j.command.chars().take(80).collect::<String>(),
                j.output_preview.chars().take(120).collect::<String>()
            )
        })
        .collect::<Vec<_>>()
        .join("\n")
}
