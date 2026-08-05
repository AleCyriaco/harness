use anyhow::{Context, Result};
use rust_xlsxwriter::{Format, Workbook};
use std::path::Path;

pub fn create_xlsx(
    path: &Path,
    sheet_name: &str,
    headers: &[String],
    rows: &[Vec<String>],
) -> Result<String> {
    let mut workbook = Workbook::new();
    let worksheet = workbook.add_worksheet();
    let name = if sheet_name.is_empty() {
        "Sheet1"
    } else {
        sheet_name
    };
    worksheet
        .set_name(name)
        .context("set sheet name")?;

    let header_fmt = Format::new().set_bold();
    for (col, h) in headers.iter().enumerate() {
        worksheet.write_string_with_format(0, col as u16, h, &header_fmt)?;
    }
    for (r, row) in rows.iter().enumerate() {
        for (c, cell) in row.iter().enumerate() {
            worksheet.write_string((r as u32) + 1, c as u16, cell)?;
        }
    }

    workbook
        .save(path)
        .with_context(|| format!("save {}", path.display()))?;
    Ok(format!(
        "created xlsx: {} ({} headers, {} rows)",
        path.display(),
        headers.len(),
        rows.len()
    ))
}
