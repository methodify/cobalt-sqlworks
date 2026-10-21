//! The matrix view of an Explore result: row keys down the left, column keys across the top,
//! values in the cells (`cobalt_results::pivot::PivotTable`). Virtualized: only the visible
//! rows are painted. Design: `docs/design/semantic_models.md` §3.1 (5).

use crate::ui::theme::Theme;
use cobalt_results::pivot::PivotTable;
use egui::{Align2, Color32, FontId, Rect, RichText, Sense, Ui, Vec2};
use egui_phosphor::regular as icons;

const ROW_H: f32 = 22.0;
const MIN_W: f32 = 56.0;
const MAX_W: f32 = 220.0;
const PAD: f32 = 8.0;

pub enum MatrixAction {
    CopyTsv(String),
}

/// Draw the matrix into the available space.
pub fn show(ui: &mut Ui, theme: &Theme, pt: &PivotTable, a11y: &str) -> Vec<MatrixAction> {
    let mut actions = Vec::new();
    if let Some(e) = &pt.error {
        ui.colored_label(theme.warning, format!("{} {e}", icons::WARNING));
        return actions;
    }
    if pt.is_empty() {
        ui.centered_and_justified(|ui| {
            ui.label(RichText::new("No rows").color(theme.text_faint));
        });
        return actions;
    }
    let font = FontId::monospace(12.5);
    let hfont = FontId::proportional(12.0);
    let nrk = pt.row_key_names.len().max(1);
    let nv = pt.value_names.len().max(1);
    let ncol_levels = pt.col_keys.first().map(|k| k.labels.len()).unwrap_or(0);
    let header_rows = ncol_levels + 1; // column key levels + the value names row
    // widths: row-key columns from labels, value columns from cells (sampled)
    let measure = |ui: &Ui, s: &str, f: &FontId| ui.painter().layout_no_wrap(s.to_string(), f.clone(), Color32::WHITE).size().x;
    let mut rk_w: Vec<f32> = (0..nrk).map(|i| measure(ui, pt.row_key_names.get(i).map(String::as_str).unwrap_or(""), &hfont) + 2.0 * PAD).collect();
    for k in pt.row_keys.iter().take(400) {
        for (i, l) in k.labels.iter().enumerate().take(nrk) {
            let w = measure(ui, l, &font) + 2.0 * PAD;
            if w > rk_w[i] {
                rk_w[i] = w;
            }
        }
    }
    for w in rk_w.iter_mut() {
        *w = w.clamp(MIN_W, MAX_W);
    }
    let ncols = pt.column_count();
    let mut col_w: Vec<f32> = (0..ncols).map(|c| measure(ui, &pt.value_names[c % nv], &hfont) + 2.0 * PAD).collect();
    for r in 0..pt.row_keys.len().min(200) {
        for (c, w_c) in col_w.iter_mut().enumerate() {
            if let Some(cell) = pt.cell(r, c / nv, c % nv) {
                let w = measure(ui, &cell.text, &font) + 2.0 * PAD;
                if w > *w_c {
                    *w_c = w;
                }
            }
        }
    }
    for w in col_w.iter_mut() {
        *w = w.clamp(MIN_W, MAX_W);
    }
    let rk_total: f32 = rk_w.iter().sum();
    let total_w = rk_total + col_w.iter().sum::<f32>();
    let total_h = (header_rows + pt.row_keys.len()) as f32 * ROW_H;
    let subtotal_bg = theme.bg_hover;
    let resp = egui::ScrollArea::both().id_salt(("matrix", a11y)).auto_shrink([false, false]).show_viewport(ui, |ui, viewport| {
        let (rect, resp) = ui.allocate_exact_size(Vec2::new(total_w.max(ui.available_width()), total_h), Sense::click());
        resp.widget_info(|| egui::WidgetInfo::labeled(egui::WidgetType::Other, true, a11y.to_string()));
        let p = ui.painter().with_clip_rect(rect.intersect(ui.clip_rect()));
        let origin = rect.min;
        let first_row = ((viewport.min.y / ROW_H) as usize).saturating_sub(header_rows + 1);
        let last_row = ((viewport.max.y / ROW_H) as usize + 2).min(pt.row_keys.len());
        // column x offsets
        let mut xs: Vec<f32> = Vec::with_capacity(ncols + 1);
        let mut x = origin.x + rk_total;
        for w in &col_w {
            xs.push(x);
            x += *w;
        }
        xs.push(x);
        // header: column key levels (merged across the values of a key), then value names
        let header_bottom = origin.y + header_rows as f32 * ROW_H;
        p.rect_filled(Rect::from_min_max(egui::pos2(origin.x, origin.y), egui::pos2(rect.max.x, header_bottom)), 0.0, theme.bg_sidebar);
        for level in 0..ncol_levels {
            let y = origin.y + level as f32 * ROW_H;
            let mut c = 0;
            while c < pt.col_keys.len() {
                let label = pt.col_keys[c].labels.get(level).map(|s| s.to_string()).unwrap_or_default();
                let mut end = c + 1;
                while end < pt.col_keys.len() && pt.col_keys[end].labels.get(..=level) == pt.col_keys[c].labels.get(..=level) {
                    end += 1;
                }
                let x0 = xs[c * nv];
                let x1 = xs[end * nv];
                let cell = Rect::from_min_max(egui::pos2(x0, y), egui::pos2(x1, y + ROW_H));
                if cell.intersects(viewport.translate(origin.to_vec2())) {
                    p.line_segment([egui::pos2(x1, y), egui::pos2(x1, y + ROW_H)], egui::Stroke::new(1.0, theme.border));
                    let shown = if pt.col_keys[c].subtotal_level > 0 && label == cobalt_results::pivot::TOTAL_LABEL { "Total".to_string() } else { label };
                    crate::fonts::note(p.ctx(), &shown);
                    p.text(egui::pos2(x0 + PAD, y + ROW_H / 2.0), Align2::LEFT_CENTER, clip_text(&shown, (x1 - x0 - 2.0 * PAD).max(10.0), 6.2), hfont.clone(), theme.text);
                }
                c = end;
            }
        }
        let yv = origin.y + ncol_levels as f32 * ROW_H;
        for (i, name) in pt.row_key_names.iter().enumerate().take(nrk) {
            let x0 = origin.x + rk_w[..i].iter().sum::<f32>();
            p.text(egui::pos2(x0 + PAD, yv + ROW_H / 2.0), Align2::LEFT_CENTER, clip_text(name, rk_w[i] - 2.0 * PAD, 6.2), hfont.clone(), theme.text_muted);
        }
        for c in 0..ncols {
            let name = &pt.value_names[c % nv];
            p.text(egui::pos2(xs[c + 1] - PAD, yv + ROW_H / 2.0), Align2::RIGHT_CENTER, clip_text(name, col_w[c] - 2.0 * PAD, 6.2), hfont.clone(), theme.text_muted);
        }
        p.line_segment([egui::pos2(origin.x, header_bottom), egui::pos2(rect.max.x, header_bottom)], egui::Stroke::new(1.0, theme.border));
        // rows
        for r in first_row..last_row {
            let y = header_bottom + r as f32 * ROW_H;
            let key = &pt.row_keys[r];
            let row_rect = Rect::from_min_max(egui::pos2(origin.x, y), egui::pos2(rect.max.x, y + ROW_H));
            if key.is_subtotal() {
                p.rect_filled(row_rect, 0.0, subtotal_bg);
            } else if r % 2 == 1 {
                p.rect_filled(row_rect, 0.0, theme.bg_panel.linear_multiply(0.97));
            }
            let strong = key.is_subtotal();
            for (i, l) in key.labels.iter().enumerate().take(nrk) {
                let x0 = origin.x + rk_w[..i].iter().sum::<f32>();
                // a subtotal row shows "Total" at its level and blanks the inner levels
                let txt = l.to_string();
                let f = if strong { FontId::proportional(12.5) } else { font.clone() };
                crate::fonts::note(p.ctx(), &txt);
                p.text(egui::pos2(x0 + PAD, y + ROW_H / 2.0), Align2::LEFT_CENTER, clip_text(&txt, rk_w[i] - 2.0 * PAD, 7.5), f, theme.text);
            }
            for c in 0..ncols {
                if let Some(cell) = pt.cell(r, c / nv, c % nv) {
                    let align = if cell.num.is_some() { Align2::RIGHT_CENTER } else { Align2::LEFT_CENTER };
                    let x = if cell.num.is_some() { xs[c + 1] - PAD } else { xs[c] + PAD };
                    let f = if strong { FontId::proportional(12.5) } else { font.clone() };
                    crate::fonts::note(p.ctx(), &cell.text);
                    p.text(egui::pos2(x, y + ROW_H / 2.0), align, clip_text(&cell.text, col_w[c] - 2.0 * PAD, 7.5), f, theme.text);
                }
            }
        }
        // vertical separators between row keys and values
        p.line_segment([egui::pos2(origin.x + rk_total, origin.y), egui::pos2(origin.x + rk_total, rect.max.y)], egui::Stroke::new(1.0, theme.border));
        resp
    });
    resp.inner.context_menu(|ui| {
        if ui.button("Copy matrix (tab-separated)").clicked() {
            actions.push(MatrixAction::CopyTsv(to_tsv(pt)));
            ui.close();
        }
    });
    actions
}

fn clip_text(s: &str, width: f32, char_w: f32) -> String {
    let max = ((width / char_w) as usize).max(3);
    if s.chars().count() > max {
        let mut t: String = s.chars().take(max - 1).collect();
        t.push('…');
        t
    } else {
        s.to_string()
    }
}

/// The matrix as tab-separated text, headers included.
pub fn to_tsv(pt: &PivotTable) -> String {
    let nv = pt.value_names.len().max(1);
    let mut out = String::new();
    for name in &pt.row_key_names {
        out.push_str(name);
        out.push('\t');
    }
    for c in 0..pt.column_count() {
        out.push_str(&pt.column_name(c / nv, c % nv));
        out.push('\t');
    }
    out.push('\n');
    for (r, key) in pt.row_keys.iter().enumerate() {
        for l in &key.labels {
            out.push_str(l);
            out.push('\t');
        }
        for c in 0..pt.column_count() {
            if let Some(cell) = pt.cell(r, c / nv, c % nv) {
                out.push_str(&cell.text);
            }
            out.push('\t');
        }
        out.push('\n');
    }
    out
}
