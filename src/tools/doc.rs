use anyhow::{Context, Result};
use docx_rs::*;
use std::path::Path;

pub fn create_docx(path: &Path, title: &str, paragraphs: &[String]) -> Result<String> {
    let mut doc = Docx::new().add_paragraph(
        Paragraph::new().add_run(
            Run::new()
                .add_text(title)
                .bold()
                .size(32), // half-points → 16pt
        ),
    );

    for p in paragraphs {
        if p.trim().is_empty() {
            doc = doc.add_paragraph(Paragraph::new());
        } else {
            doc = doc.add_paragraph(Paragraph::new().add_run(Run::new().add_text(p).size(22)));
        }
    }

    let file = std::fs::File::create(path)
        .with_context(|| format!("create {}", path.display()))?;
    doc.build().pack(file).context("write docx")?;
    Ok(format!("created docx: {}", path.display()))
}
