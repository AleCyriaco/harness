//! Internal WebView window (WKWebView / WebView2 / WebKitGTK) — same harness binary.
//! Opened via `harness --webview <url>` so web apps render inside the app, not Safari/Chrome.

use anyhow::{Context, Result, bail};
use std::sync::atomic::{AtomicBool, Ordering};

static RUNNING: AtomicBool = AtomicBool::new(false);

/// Block the process and show a native webview owned by harness.
pub fn run_blocking(url: &str) -> Result<()> {
    let url = url.trim();
    if url.is_empty() {
        bail!("empty url");
    }
    if RUNNING.swap(true, Ordering::SeqCst) {
        // Another webview in this process — rare.
    }

    use tao::{
        dpi::LogicalSize,
        event::{Event, WindowEvent},
        event_loop::{ControlFlow, EventLoop},
        window::WindowBuilder,
    };
    use wry::WebViewBuilder;

    let event_loop = EventLoop::new();
    let window = WindowBuilder::new()
        .with_title(format!("harness · Web — {url}"))
        .with_inner_size(LogicalSize::new(1200.0, 860.0))
        .with_min_inner_size(LogicalSize::new(640.0, 480.0))
        .build(&event_loop)
        .context("create webview window")?;

    let builder = WebViewBuilder::new()
        .with_url(url)
        .with_devtools(cfg!(debug_assertions));

    // Platform build API differs slightly across wry versions.
    #[cfg(any(
        target_os = "windows",
        target_os = "macos",
        target_os = "ios",
        target_os = "android"
    ))]
    let _webview = builder.build(&window).context("build webview")?;

    #[cfg(not(any(
        target_os = "windows",
        target_os = "macos",
        target_os = "ios",
        target_os = "android"
    )))]
    let _webview = {
        use tao::platform::unix::WindowExtUnix;
        use wry::WebViewBuilderExtUnix;
        let vbox = window.default_vbox().context("gtk vbox")?;
        builder.build_gtk(vbox).context("build gtk webview")?
    };

    let _ = window;
    event_loop.run(move |event, _, control_flow| {
        *control_flow = ControlFlow::Wait;
        if let Event::WindowEvent {
            event: WindowEvent::CloseRequested,
            ..
        } = event
        {
            *control_flow = ControlFlow::Exit;
            RUNNING.store(false, Ordering::SeqCst);
        }
    });
}

/// Launch (or re-launch) the internal webview as a child process of this binary.
pub fn open_in_app(url: &str) -> Result<()> {
    let url = url.trim();
    if url.is_empty() {
        bail!("empty url");
    }
    stop_previous();

    let exe = std::env::current_exe().context("current_exe")?;
    let child = std::process::Command::new(exe)
        .arg("--webview")
        .arg(url)
        .stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .spawn()
        .context("spawn harness webview")?;

    if let Ok(mut g) = WEBVIEW_PID.lock() {
        *g = Some(child.id());
    }
    // Detach: don't wait; child owns the window.
    std::mem::forget(child);

    if let Ok(mut g) = crate::browser::BROWSER.lock() {
        g.url = url.to_string();
        g.last_error.clear();
        g.title = format!("harness · {url}");
    }
    Ok(())
}

static WEBVIEW_PID: std::sync::Mutex<Option<u32>> = std::sync::Mutex::new(None);

fn stop_previous() {
    let pid = WEBVIEW_PID.lock().ok().and_then(|mut g| g.take());
    if let Some(pid) = pid {
        #[cfg(unix)]
        {
            unsafe {
                libc::kill(pid as i32, libc::SIGTERM);
            }
        }
        #[cfg(windows)]
        {
            let _ = std::process::Command::new("taskkill")
                .args(["/PID", &pid.to_string(), "/F"])
                .stdout(std::process::Stdio::null())
                .stderr(std::process::Stdio::null())
                .status();
        }
        std::thread::sleep(std::time::Duration::from_millis(80));
    }
}
