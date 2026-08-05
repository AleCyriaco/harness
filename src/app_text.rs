use crate::modes::AppMode;
use std::path::Path;

pub fn welcome(mode: AppMode) -> String {
    match mode {
        AppMode::Code => {
            "harness · Code — files go under your default folder (code/). Streaming on.".into()
        }
        AppMode::Office => {
            "harness · Office — docs/sheets/pdfs under your default folder (docs/, sheets/, pdfs/).".into()
        }
    }
}

pub fn welcome_with_workspace(mode: AppMode, workspace: &Path) -> String {
    format!(
        "{} · workspace: {}",
        welcome(mode),
        workspace.display()
    )
}
