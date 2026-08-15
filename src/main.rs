mod agent;
mod app;
mod app_text;
mod bg;
mod browser;
mod tokenless;
mod cli;
mod compact;
mod config;
mod daemon;
mod daemon_client;
mod diagnostics;
mod file_watch;
mod gauntlet;
mod guard;
mod graph;
mod hooks;
mod icon;
mod llm;
mod llm_pool;
mod llm_responses;
mod mcp;
mod md;
mod mem_stats;
mod metrics;
mod memory;
mod memory_graph;
mod mermaid_lite;
mod modes;
mod plan;
mod preview;
mod protocol;
mod provider_doctor;
mod resume_import;
mod selfdev;
mod session;
mod session_search;
mod side_panel;
mod skills;
mod slash;
mod stuck;
mod swarm;
mod swarm_plan;
mod theme;
mod toolcall;
mod tools;
mod ui;
mod update;
mod web_extract;
mod webserver;
mod webview_win;

use app::HarnessApp;

fn main() -> eframe::Result<()> {
    let argv: Vec<String> = std::env::args().skip(1).collect();
    if let Some(flag) = argv.first() {
        if flag == "--webview" || flag == "webview" {
            let url = argv
                .get(1)
                .cloned()
                .unwrap_or_else(|| "http://127.0.0.1:8765/".into());
            if let Err(e) = webview_win::run_blocking(&url) {
                eprintln!("harness webview error: {e:#}");
                std::process::exit(1);
            }
            return Ok(());
        }
        match cli::dispatch(&argv) {
            Ok(true) => return Ok(()),
            Ok(false) => {}
            Err(e) => {
                eprintln!("error: {e:#}");
                std::process::exit(1);
            }
        }
    }

    // Ambient memory consolidation on GUI start
    crate::memory_graph::ambient_start();

    let options = eframe::NativeOptions {
        viewport: egui::ViewportBuilder::default()
            .with_inner_size([1360.0, 840.0])
            .with_min_inner_size([900.0, 580.0])
            .with_title("harness")
            .with_icon(icon::window_icon()),
        ..Default::default()
    };

    eframe::run_native(
        "harness",
        options,
        Box::new(|cc| Ok(Box::new(HarnessApp::new(cc)))),
    )
}
