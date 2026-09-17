//! Excel `.xlsx` via rust_xlsxwriter in constant-memory mode (rows stream to a temp file).

use crate::{Ctx, ExportError, Progress, Result, EXCEL_MAX_ROWS};
use arrow::array::Array;
use chrono::{Datelike, Timelike};
use cobalt_results::CellValue;
use rust_xlsxwriter::{ExcelDateTime, Format, Workbook, Worksheet};
use std::path::Path;

/// Excel's per-cell string limit.
pub const EXCEL_MAX_CHARS: usize = 32_767;
const WIDTH_SAMPLE_ROWS: usize = 200;
const MAX_COL_WIDTH: f64 = 60.0;

/// Sheet names: ≤31 chars, none of `[]:*?/\`, not empty, no leading/trailing apostrophe.
pub fn sanitize_sheet_name(name: &str) -> String {
    let cleaned: String = name.chars().filter(|c| !matches!(c, '[' | ']' | ':' | '*' | '?' | '/' | '\\')).collect();
    let cleaned = cleaned.trim().trim_matches('\'').to_string();
    let cleaned: String = cleaned.chars().take(31).collect();
    if cleaned.trim().is_empty() {
        "Sheet1".into()
    } else {
        cleaned
    }
}

struct Formats {
    header: Format,
    date: Format,
    datetime: Format,
    time: Format,
    decimals: Vec<Format>,
}

impl Formats {
    fn new() -> Self {
        Self {
            header: Format::new().set_bold(),
            date: Format::new().set_num_format("yyyy-mm-dd"),
            datetime: Format::new().set_num_format("yyyy-mm-dd hh:mm:ss"),
            time: Format::new().set_num_format("hh:mm:ss"),
            decimals: (0..=38u8).map(|scale| Format::new().set_num_format(if scale == 0 { "0".to_string() } else { format!("0.{}", "0".repeat(scale as usize)) })).collect(),
        }
    }
}

/// Write the workbook straight to `path` (rust_xlsxwriter needs a seekable target). Returns
/// (rows, warnings); the byte count is taken from the file afterwards.
pub(crate) fn write_file(ctx: &Ctx<'_>, path: &Path, progress: &mut dyn FnMut(Progress) -> bool) -> Result<(usize, Vec<String>)> {
    let o = &ctx.opts.excel;
    let rows_total = ctx.rs.visible_count();
    if rows_total + 1 > EXCEL_MAX_ROWS {
        return Err(ExportError::TooManyRows { rows: rows_total, limit: EXCEL_MAX_ROWS - 1 });
    }
    let ncols = ctx.rs.columns.len();
    let formats = Formats::new();
    let mut workbook = Workbook::new();
    let sheet: &mut Worksheet = workbook.add_worksheet_with_constant_memory();
    sheet.set_name(sanitize_sheet_name(&o.sheet_name))?;

    // Header row.
    for (c, col) in ctx.rs.columns.iter().enumerate() {
        if o.bold_header {
            sheet.write_string_with_format(0, c as u16, &col.name, &formats.header)?;
        } else {
            sheet.write_string(0, c as u16, &col.name)?;
        }
    }
    if o.freeze_header {
        sheet.set_freeze_panes(1, 0)?;
    }
    if o.autofilter && ncols > 0 {
        sheet.autofilter(0, 0, rows_total as u32, (ncols - 1) as u16)?;
    }

    // Column widths from the header and a sample of rows (autofit() needs stored cells, which
    // constant-memory mode does not keep).
    if o.autofit && ncols > 0 {
        let mut widths: Vec<f64> = ctx.rs.columns.iter().map(|c| c.name.chars().count().max(4) as f64).collect();
        for r in 0..rows_total.min(WIDTH_SAMPLE_ROWS) {
            for (c, w) in widths.iter_mut().enumerate() {
                let n = ctx.rs.cell_text(r, c, &ctx.fmt).chars().count() as f64;
                if n > *w {
                    *w = n;
                }
            }
        }
        for (c, w) in widths.iter().enumerate() {
            sheet.set_column_width(c as u16, (w * 1.1 + 1.0).min(MAX_COL_WIDTH))?;
        }
    }

    let mut truncated = 0usize;
    let mut out_of_range_dates = 0usize;
    let mut row_no: u32 = 1;
    let rows = ctx.for_each_batch(progress, |batch| {
        let text = if o.native_types { None } else { Some(ctx.format_batch(batch, &ctx.fmt)) };
        for r in 0..batch.num_rows() {
            for c in 0..ncols {
                let col = c as u16;
                let arr = batch.column(c);
                if arr.is_null(r) {
                    continue;
                }
                if let Some(text) = &text {
                    write_str(sheet, row_no, col, &text[c][r], &mut truncated)?;
                    continue;
                }
                match CellValue::from_array(arr, r) {
                    CellValue::Null => {}
                    CellValue::Bool(b) => {
                        sheet.write_boolean(row_no, col, b)?;
                    }
                    CellValue::Int(i) => {
                        sheet.write_number(row_no, col, i as f64)?;
                    }
                    CellValue::Float(f) => {
                        if f.is_finite() {
                            sheet.write_number(row_no, col, f)?;
                        } else {
                            sheet.write_string(row_no, col, f.to_string())?;
                        }
                    }
                    CellValue::Decimal(v, s) => {
                        let scale = s.clamp(0, 38) as usize;
                        let f = v as f64 / 10f64.powi(s as i32);
                        sheet.write_number_with_format(row_no, col, f, &formats.decimals[scale])?;
                    }
                    CellValue::Text(t) => write_str(sheet, row_no, col, &t, &mut truncated)?,
                    CellValue::Bytes(b) => write_str(sheet, row_no, col, &format!("0x{}", cobalt_results::value::hex(&b)), &mut truncated)?,
                    CellValue::Date(d) => match excel_date(d.year(), d.month(), d.day()) {
                        Some(dt) => {
                            sheet.write_datetime_with_format(row_no, col, &dt, &formats.date)?;
                        }
                        None => {
                            out_of_range_dates += 1;
                            sheet.write_string(row_no, col, d.to_string())?;
                        }
                    },
                    CellValue::Time(t) => {
                        let dt = ExcelDateTime::from_hms_milli(t.hour() as u16, t.minute() as u8, t.second() as u8, (t.nanosecond() / 1_000_000) as u16)?;
                        sheet.write_datetime_with_format(row_no, col, &dt, &formats.time)?;
                    }
                    CellValue::DateTime(dt) => write_datetime(sheet, row_no, col, dt, &formats, &mut out_of_range_dates)?,
                    CellValue::DateTimeTz(dt) => write_datetime(sheet, row_no, col, dt.naive_utc(), &formats, &mut out_of_range_dates)?,
                }
            }
            row_no += 1;
        }
        Ok(())
    })?;

    workbook.save(path)?;
    let mut warnings = Vec::new();
    if truncated > 0 {
        warnings.push(format!("{truncated} text cell(s) were truncated to Excel's limit of {EXCEL_MAX_CHARS} characters"));
    }
    if out_of_range_dates > 0 {
        warnings.push(format!("{out_of_range_dates} date(s) before 1900 were written as text"));
    }
    Ok((rows, warnings))
}

fn write_str(sheet: &mut Worksheet, row: u32, col: u16, s: &str, truncated: &mut usize) -> Result<()> {
    if s.chars().count() > EXCEL_MAX_CHARS {
        *truncated += 1;
        let cut: String = s.chars().take(EXCEL_MAX_CHARS).collect();
        sheet.write_string(row, col, cut)?;
    } else {
        sheet.write_string(row, col, s)?;
    }
    Ok(())
}

fn excel_date(year: i32, month: u32, day: u32) -> Option<ExcelDateTime> {
    if !(1900..=9999).contains(&year) {
        return None;
    }
    ExcelDateTime::from_ymd(year as u16, month as u8, day as u8).ok()
}

fn write_datetime(sheet: &mut Worksheet, row: u32, col: u16, dt: chrono::NaiveDateTime, formats: &Formats, out_of_range: &mut usize) -> Result<()> {
    match excel_date(dt.year(), dt.month(), dt.day()) {
        Some(d) => {
            let d = d.and_hms_milli(dt.hour() as u16, dt.minute() as u8, dt.second() as u8, (dt.nanosecond() / 1_000_000) as u16)?;
            sheet.write_datetime_with_format(row, col, &d, &formats.datetime)?;
        }
        None => {
            *out_of_range += 1;
            sheet.write_string(row, col, dt.format("%Y-%m-%d %H:%M:%S%.f").to_string())?;
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::sanitize_sheet_name;
    #[test]
    fn sheet_names() {
        assert_eq!(sanitize_sheet_name("Results"), "Results");
        assert_eq!(sanitize_sheet_name("a/b:c*d?e[f]\\g"), "abcdefg");
        assert_eq!(sanitize_sheet_name(""), "Sheet1");
        assert_eq!(sanitize_sheet_name(&"x".repeat(40)).len(), 31);
        assert_eq!(sanitize_sheet_name("'quoted'"), "quoted");
    }
}
