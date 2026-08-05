use anyhow::{Context, Result};
use printpdf::*;
use std::fs::File;
use std::io::BufWriter;
use std::path::Path;

pub fn create_pdf(path: &Path, title: &str, paragraphs: &[String]) -> Result<String> {
    let (doc, page1, layer1) =
        PdfDocument::new(title, Mm(210.0), Mm(297.0), "Layer 1");
    let font = doc.add_builtin_font(BuiltinFont::Helvetica)?;
    let font_bold = doc.add_builtin_font(BuiltinFont::HelveticaBold)?;

    let mut page = page1;
    let mut layer = layer1;
    let mut y = 280.0_f32;

    {
        let layer_ref = doc.get_page(page).get_layer(layer);
        layer_ref.use_text(title, 18.0, Mm(20.0), Mm(y), &font_bold);
    }
    y -= 12.0;

    for para in paragraphs {
        for line in wrap_text(para, 90) {
            if y < 20.0 {
                let (p, l) = doc.add_page(Mm(210.0), Mm(297.0), "Layer 1");
                page = p;
                layer = l;
                y = 280.0;
            }
            let layer_ref = doc.get_page(page).get_layer(layer);
            layer_ref.use_text(line, 11.0, Mm(20.0), Mm(y), &font);
            y -= 6.0;
        }
        y -= 4.0;
    }

    let file = File::create(path).with_context(|| format!("create {}", path.display()))?;
    doc.save(&mut BufWriter::new(file))
        .context("write pdf")?;
    Ok(format!("created pdf: {}", path.display()))
}

fn wrap_text(text: &str, width: usize) -> Vec<String> {
    let mut lines = Vec::new();
    for paragraph in text.split('\n') {
        if paragraph.is_empty() {
            lines.push(String::new());
            continue;
        }
        let mut current = String::new();
        for word in paragraph.split_whitespace() {
            if current.is_empty() {
                current = word.to_string();
            } else if current.len() + 1 + word.len() <= width {
                current.push(' ');
                current.push_str(word);
            } else {
                lines.push(current);
                current = word.to_string();
            }
        }
        if !current.is_empty() {
            lines.push(current);
        }
    }
    lines
}
