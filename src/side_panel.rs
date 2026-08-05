//! Live side panel — file view, diff, notes (jcode-inspired).

use std::path::PathBuf;
use std::sync::Mutex;

#[derive(Debug, Clone, Default)]
pub struct SidePanelState {
    pub title: String,
    pub kind: PanelKind,
    pub body: String,
    pub path: Option<PathBuf>,
}

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub enum PanelKind {
    #[default]
    Empty,
    File,
    Diff,
    Note,
    Plan,
}

pub static SIDE_PANEL: Mutex<SidePanelState> = Mutex::new(SidePanelState {
    title: String::new(),
    kind: PanelKind::Empty,
    body: String::new(),
    path: None,
});

pub fn get() -> SidePanelState {
    SIDE_PANEL.lock().map(|g| g.clone()).unwrap_or_default()
}

pub fn set_file(path: PathBuf, content: String) {
    if let Ok(mut g) = SIDE_PANEL.lock() {
        g.title = path
            .file_name()
            .map(|s| s.to_string_lossy().into_owned())
            .unwrap_or_else(|| path.display().to_string());
        g.kind = PanelKind::File;
        g.body = content;
        g.path = Some(path);
    }
}

pub fn set_diff(title: &str, diff: String) {
    if let Ok(mut g) = SIDE_PANEL.lock() {
        g.title = title.into();
        g.kind = PanelKind::Diff;
        g.body = diff;
        g.path = None;
    }
}

pub fn set_note(title: &str, body: String) {
    if let Ok(mut g) = SIDE_PANEL.lock() {
        g.title = title.into();
        g.kind = PanelKind::Note;
        g.body = body;
        g.path = None;
    }
}

pub fn set_plan(body: String) {
    if let Ok(mut g) = SIDE_PANEL.lock() {
        g.title = "Plan".into();
        g.kind = PanelKind::Plan;
        g.body = body;
        g.path = None;
    }
}

pub fn clear() {
    if let Ok(mut g) = SIDE_PANEL.lock() {
        *g = SidePanelState::default();
    }
}
