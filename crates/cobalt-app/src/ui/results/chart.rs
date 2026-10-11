//! A chart in place of a result set's grid: the strip (kind, X, Y, aggregation, top-N, stacked,
//! horizontal, image export) and the egui_plot rendering of `cobalt_results::chart::ChartData`.
//! Bars, lines, areas and scatters go through egui_plot (hover labels, legend, zoom, pan); a pie
//! is painted as sectors. A click on a bar or point selects its rows in the grid.

use crate::state::{ChartView, ResultSetView, Selection};
use crate::ui::theme::Theme;
use cobalt_core::{Agg, ChartKind, ChartSpec};
use cobalt_results::chart::{self, ChartData, XKind};
use cobalt_results::CellFormatter;
use egui::{Color32, RichText, Ui, Vec2};
use egui_phosphor::regular as icons;
use egui_plot::{Bar, BarChart, Corner, Legend, Line, Plot, PlotPoint, PlotPoints, Points};
use std::sync::Arc;

/// Eight categorical colours that read on both themes (Vega's category10 minus the greys).
pub const PALETTE: [Color32; 8] = [
    Color32::from_rgb(0x4C, 0x78, 0xA8),
    Color32::from_rgb(0xF5, 0x85, 0x18),
    Color32::from_rgb(0x54, 0xA2, 0x4B),
    Color32::from_rgb(0xE4, 0x57, 0x56),
    Color32::from_rgb(0x72, 0xB7, 0xB2),
    Color32::from_rgb(0xEE, 0xCA, 0x3B),
    Color32::from_rgb(0xB2, 0x79, 0xA2),
    Color32::from_rgb(0xFF, 0x9D, 0xA6),
];

pub fn series_color(i: usize) -> Color32 {
    let base = PALETTE[i % PALETTE.len()];
    if i < PALETTE.len() {
        base
    } else {
        // later series: lighter tints of the same eight
        let t = 0.35;
        Color32::from_rgb((base.r() as f32 + (255.0 - base.r() as f32) * t) as u8, (base.g() as f32 + (255.0 - base.g() as f32) * t) as u8, (base.b() as f32 + (255.0 - base.b() as f32) * t) as u8)
    }
}

pub enum ChartAction {
    /// Read the plot's pixels on the next frame: to the clipboard, or to a PNG file.
    Capture { save: bool },
    /// Notebook cells: read the pixels into the cell's outputs (saved with the notebook).
    Keep,
    Close,
}

/// Make sure the chart's data matches its spec and the view; rebuilds when either moved.
pub fn ensure_data(view: &mut ResultSetView, fmt: &CellFormatter) {
    let rs = view.rs.clone();
    let Some(c) = view.chart.as_mut() else { return };
    let gen = rs.generation();
    if c.built_for.as_ref().map(|(s, g)| *s == c.spec && *g == gen).unwrap_or(false) {
        return;
    }
    let t0 = std::time::Instant::now();
    c.data = Some(Arc::new(chart::chart_data(&rs, &c.spec, fmt)));
    c.build_ms = t0.elapsed().as_millis() as u64;
    c.built_for = Some((c.spec.clone(), gen));
}

/// The strip and the chart. Returns what the caller must do (capture, close). `in_notebook`
/// adds "Keep in notebook" (the picture saved with the cell's outputs).
pub fn show(ui: &mut Ui, view: &mut ResultSetView, theme: &Theme, fmt: &CellFormatter, set: usize, in_notebook: bool) -> Vec<ChartAction> {
    let mut actions = Vec::new();
    ensure_data(view, fmt);
    let rs = view.rs.clone();
    let columns: Vec<(usize, String, bool)> = rs.columns.iter().enumerate().map(|(i, c)| (i, c.name.clone(), chart::is_numeric(&c.sql_type))).collect();
    let Some(c) = view.chart.as_mut() else { return actions };
    let a11y = |what: &str| format!("chart {what}");
    // ---- the strip ----
    egui::Frame::new().fill(theme.bg_sidebar).inner_margin(egui::Margin::symmetric(8, 4)).show(ui, |ui| {
        ui.horizontal_wrapped(|ui| {
            ui.spacing_mut().item_spacing.x = 6.0;
            for k in ChartKind::ALL {
                let r = ui.add(egui::Button::selectable(c.spec.kind == k, RichText::new(k.label()).size(12.0)).small());
                r.widget_info(|| egui::WidgetInfo::labeled(egui::WidgetType::Button, true, a11y(&format!("kind {}", k.key()))));
                if r.clicked() {
                    c.spec.kind = k;
                    if k == ChartKind::Scatter && c.spec.agg != Agg::None {
                        c.spec.agg = Agg::None;
                    }
                    if k == ChartKind::Pie && c.spec.agg == Agg::None {
                        c.spec.agg = Agg::Sum;
                    }
                    if k == ChartKind::Histogram && c.spec.agg == Agg::None {
                        c.spec.agg = if c.spec.y.is_empty() { Agg::Count } else { Agg::Sum };
                    }
                }
            }
            ui.separator();
            ui.label(RichText::new(if c.spec.kind == ChartKind::Histogram { "Bin" } else { "X" }).size(12.0).color(theme.text_muted));
            let x_label = c.spec.x.clone().unwrap_or_else(|| "row number".to_string());
            let r = egui::ComboBox::from_id_salt(("chart-x", set)).width(150.0).selected_text(RichText::new(x_label).size(12.0)).show_ui(ui, |ui| {
                if ui.selectable_label(c.spec.x.is_none(), "row number").clicked() {
                    c.spec.x = None;
                }
                for (_, name, _) in &columns {
                    if ui.selectable_label(c.spec.x.as_deref() == Some(name.as_str()), name).clicked() {
                        c.spec.x = Some(name.clone());
                    }
                }
            });
            r.response.widget_info(|| egui::WidgetInfo::labeled(egui::WidgetType::ComboBox, true, a11y("x")));
            ui.label(RichText::new("Y").size(12.0).color(theme.text_muted));
            let y_label = if c.spec.y.is_empty() { "count of rows".to_string() } else { c.spec.y.join(", ") };
            let r = ui.menu_button(RichText::new(y_label).size(12.0), |ui| {
                ui.set_min_width(180.0);
                ui.label(RichText::new("Numeric columns (one series each)").size(11.0).color(theme.text_faint));
                for (_, name, numeric) in &columns {
                    if !numeric {
                        continue;
                    }
                    let mut on = c.spec.y.iter().any(|y| y == name);
                    let r = ui.checkbox(&mut on, name.as_str());
                    r.widget_info(|| egui::WidgetInfo::labeled(egui::WidgetType::Checkbox, true, a11y(&format!("y {name}"))));
                    if r.changed() {
                        if on {
                            if c.spec.y.len() < 12 {
                                c.spec.y.push(name.clone());
                            }
                        } else {
                            c.spec.y.retain(|y| y != name);
                        }
                        if c.spec.y.is_empty() {
                            c.spec.agg = Agg::Count;
                        } else if c.spec.agg == Agg::Count {
                            c.spec.agg = Agg::Sum;
                        }
                    }
                }
                if columns.iter().all(|(_, _, n)| !n) {
                    ui.label(RichText::new("no numeric columns: the chart counts rows per X").size(11.0).color(theme.text_muted));
                }
                if c.spec.y.len() >= 12 {
                    ui.label(RichText::new("12 series at most").size(11.0).color(theme.warning));
                }
            });
            r.response.widget_info(|| egui::WidgetInfo::labeled(egui::WidgetType::Button, true, a11y("y")));
            if c.spec.kind != ChartKind::Scatter {
                let r = egui::ComboBox::from_id_salt(("chart-agg", set)).width(130.0).selected_text(RichText::new(c.spec.agg.label()).size(12.0)).show_ui(ui, |ui| {
                    for a in Agg::ALL {
                        if a == Agg::Count && !c.spec.y.is_empty() {
                            // count with Y columns picked counts their non-null values; fine
                        }
                        if ui.selectable_label(c.spec.agg == a, a.label()).on_hover_text(match a {
                            Agg::None => "Every row is a point; repeated X values stay separate.",
                            _ => "Rows with the same X fold into one point.",
                        }).clicked() {
                            c.spec.agg = a;
                        }
                    }
                });
                r.response.widget_info(|| egui::WidgetInfo::labeled(egui::WidgetType::ComboBox, true, a11y("agg")));
            }
            if c.spec.kind == ChartKind::Histogram {
                let mut auto = c.spec.bins.is_none();
                let r = ui.checkbox(&mut auto, RichText::new("Auto bins").size(12.0)).on_hover_text("Bin count from the data (Freedman–Diaconis, else Sturges), 5 to 200.");
                r.widget_info(|| egui::WidgetInfo::labeled(egui::WidgetType::Checkbox, true, a11y("auto bins")));
                if r.changed() {
                    c.spec.bins = if auto { None } else { Some(c.data.as_ref().map(|d| d.points()).filter(|n| *n > 0).unwrap_or(20)) };
                }
                if let Some(b) = c.spec.bins.as_mut() {
                    let r = ui.add(egui::DragValue::new(b).range(1..=10_000).speed(1.0));
                    r.widget_info(|| egui::WidgetInfo::labeled(egui::WidgetType::DragValue, true, a11y("bins")));
                    ui.label(RichText::new("bins").size(11.0).color(theme.text_faint));
                } else if let Some(d) = &c.data {
                    ui.label(RichText::new(format!("{} bins", d.points())).size(11.0).color(theme.text_faint));
                }
            }
            let is_category = c.data.as_ref().map(|d| d.x_kind == XKind::Category).unwrap_or(false);
            if is_category {
                let mut on = c.spec.top_n.is_some();
                let r = ui.checkbox(&mut on, RichText::new("Top").size(12.0));
                r.widget_info(|| egui::WidgetInfo::labeled(egui::WidgetType::Checkbox, true, a11y("top")));
                if r.changed() {
                    c.spec.top_n = if on { Some(if c.spec.kind == ChartKind::Pie { 12 } else { 50 }) } else { None };
                }
                if let Some(n) = c.spec.top_n.as_mut() {
                    let r = ui.add(egui::DragValue::new(n).range(1..=10_000).speed(1.0));
                    r.widget_info(|| egui::WidgetInfo::labeled(egui::WidgetType::DragValue, true, a11y("top n")));
                    ui.label(RichText::new("+ other").size(11.0).color(theme.text_faint)).on_hover_text("The N largest X values by the first series; the rest fold into one \"other\" point (sum and count only).");
                }
            }
            if matches!(c.spec.kind, ChartKind::Bar | ChartKind::Area) && c.spec.y.len() > 1 {
                let r = ui.checkbox(&mut c.spec.stacked, RichText::new("Stacked").size(12.0));
                r.widget_info(|| egui::WidgetInfo::labeled(egui::WidgetType::Checkbox, true, a11y("stacked")));
            }
            if c.spec.kind == ChartKind::Bar {
                let r = ui.checkbox(&mut c.spec.horizontal, RichText::new("Horizontal").size(12.0));
                r.widget_info(|| egui::WidgetInfo::labeled(egui::WidgetType::Checkbox, true, a11y("horizontal")));
            }
        });
        // second row: the status on the left, the image buttons and close on the right
        ui.horizontal(|ui| {
            ui.spacing_mut().item_spacing.x = 6.0;
            ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                let r = ui.small_button(icons::X).on_hover_text("Back to the grid");
                r.widget_info(|| egui::WidgetInfo::labeled(egui::WidgetType::Button, true, a11y("close")));
                if r.clicked() {
                    actions.push(ChartAction::Close);
                }
                let r = ui.small_button(format!("{} Save PNG…", icons::FLOPPY_DISK)).on_hover_text("Write the chart as it is on screen to a PNG file.");
                r.widget_info(|| egui::WidgetInfo::labeled(egui::WidgetType::Button, true, a11y("save png")));
                if r.clicked() {
                    actions.push(ChartAction::Capture { save: true });
                }
                let r = ui.small_button(format!("{} Copy image", icons::COPY)).on_hover_text("Copy the chart as it is on screen to the clipboard (paste into Teams, a document, a mail).");
                r.widget_info(|| egui::WidgetInfo::labeled(egui::WidgetType::Button, true, a11y("copy image")));
                if r.clicked() {
                    actions.push(ChartAction::Capture { save: false });
                }
                if in_notebook {
                    let kept = c.png.is_some();
                    let r = ui.small_button(format!("{} {}", if kept { icons::CHECK } else { icons::PUSH_PIN }, if kept { "Kept" } else { "Keep in notebook" })).on_hover_text("Save the chart as a picture with the cell's outputs, so the .ipynb shows it in Fabric, VS Code and GitHub. The chart itself is saved as its settings and comes back live when the notebook opens.");
                    r.widget_info(|| egui::WidgetInfo::labeled(egui::WidgetType::Button, true, a11y("keep")));
                    if r.clicked() {
                        actions.push(ChartAction::Keep);
                    }
                }
                if let Some(d) = &c.data {
                    let status = match &d.error {
                        Some(e) => e.clone(),
                        None => {
                            let mut s = format!("{} point{} · {} rows", crate::state::fmt_count(d.points() as u64), if d.points() == 1 { "" } else { "s" }, crate::state::fmt_count(d.total_rows as u64));
                            if d.has_other {
                                s.push_str(&format!(" · top {} of {} + other", d.points() - 1, d.distinct_x));
                            }
                            if d.x_kind == XKind::Bin {
                                s.push_str(&format!(" · {} bins{}", d.points(), if d.bins_auto { " (auto)" } else { "" }));
                            }
                            if c.build_ms >= 100 {
                                s.push_str(&format!(" · built in {:.1} s", c.build_ms as f64 / 1000.0));
                            }
                            s
                        }
                    };
                    let l = ui.label(RichText::new(status).size(11.0).color(if d.error.is_some() { theme.error } else { theme.text_faint }));
                    l.widget_info(|| egui::WidgetInfo::labeled(egui::WidgetType::Label, true, a11y("status")));
                }
            });
        });
    });
    // ---- the chart ----
    let Some(data) = c.data.clone() else { return actions };
    if let Some(e) = &data.error {
        ui.vertical_centered(|ui| {
            ui.add_space(24.0);
            ui.label(RichText::new(e).color(theme.text_muted));
        });
        return actions;
    }
    if data.points() == 0 {
        ui.vertical_centered(|ui| {
            ui.add_space(24.0);
            ui.label(RichText::new("No rows to chart.").color(theme.text_muted));
        });
        return actions;
    }
    let spec = c.spec.clone();
    let clicked_point = if spec.kind == ChartKind::Pie { pie(ui, c, &data, theme) } else { plot(ui, c, &data, &spec, set, theme) };
    if let Some(p) = clicked_point {
        if let Some(rows) = data.rows.get(p) {
            if let Some(&first) = rows.first() {
                let contiguous = rows.windows(2).all(|w| w[1] == w[0] + 1);
                let last = if contiguous { *rows.last().unwrap_or(&first) } else { first };
                view.grid.selection = Selection::Rows { r0: first as usize, r1: last as usize };
                view.grid.anchor = Some((first as usize, 0));
                view.grid.scroll_to = Some((first as usize, 0));
            }
        }
    }
    actions
}

/// Longest category label on an axis; the hover shows the whole text.
const AXIS_LABEL_CHARS: usize = 22;

fn x_axis_label(data: &ChartData, v: f64) -> String {
    match data.x_kind {
        XKind::Category | XKind::Bin => {
            let i = v.round();
            if (v - i).abs() > 0.01 || i < 0.0 {
                return String::new();
            }
            let l = data.x_labels.get(i as usize).cloned().unwrap_or_default();
            // a bin's axis label is its lower edge; the hover carries the whole range
            let l = if data.x_kind == XKind::Bin { l.split(" – ").next().unwrap_or("").to_string() } else { l };
            if l.chars().count() > AXIS_LABEL_CHARS {
                format!("{}…", l.chars().take(AXIS_LABEL_CHARS - 1).collect::<String>())
            } else {
                l
            }
        }
        XKind::Time => chart::fmt_time(v),
        XKind::RowNumber => chart::fmt_num(v + 1.0),
        XKind::Number => chart::fmt_num(v),
    }
}

/// Bars, lines, areas and scatters through egui_plot. Returns the point index a click landed on.
fn plot(ui: &mut Ui, c: &mut ChartView, data: &Arc<ChartData>, spec: &ChartSpec, set: usize, theme: &Theme) -> Option<usize> {
    let n = data.points();
    let binned = data.x_kind == XKind::Bin;
    let category = data.x_kind == XKind::Category || binned;
    let horizontal = spec.kind == ChartKind::Bar && spec.horizontal;
    let x_of = |i: usize| -> f64 { if category { i as f64 } else { data.x_values[i] } };
    let d1 = data.clone();
    let d2 = data.clone();
    let x_name = data.x_name.clone();
    let mut plot = Plot::new(("chart", set))
        .legend(Legend::default().position(Corner::RightTop))
        .show_background(false)
        .allow_drag(true)
        .allow_zoom(true)
        .allow_scroll(false)
        .label_formatter(move |pos: &egui_plot::HoverPosition<'_>| {
            let (name, p): (&str, PlotPoint) = match pos {
                egui_plot::HoverPosition::NearDataPoint { plot_name, position, .. } => (plot_name, *position),
                egui_plot::HoverPosition::Elsewhere { position } => ("", *position),
            };
            let (xv, yv) = if horizontal { (p.y, p.x) } else { (p.x, p.y) };
            let xl = x_axis_label(&d1, xv);
            let xl = if xl.is_empty() { chart::fmt_num(xv) } else { xl };
            Some(if name.is_empty() { format!("{}: {xl}\n{}", d1.x_name, chart::fmt_num(yv)) } else { format!("{name}\n{}: {xl}\n{}", d1.x_name, chart::fmt_num(yv)) })
        });
    let value_fmt = |mark: egui_plot::GridMark, _range: &std::ops::RangeInclusive<f64>| chart::fmt_num(mark.value);
    let cat_fmt = move |mark: egui_plot::GridMark, range: &std::ops::RangeInclusive<f64>| {
        if d2.x_kind == XKind::Time {
            // dates alone when the visible span is days or more
            let span = range.end() - range.start();
            if span > 2.0 * 86_400_000.0 {
                let s = chart::fmt_time(mark.value);
                return s.split(' ').next().unwrap_or("").to_string();
            }
        }
        x_axis_label(&d2, mark.value)
    };
    if horizontal {
        plot = plot.y_axis_formatter(cat_fmt).x_axis_formatter(value_fmt).y_axis_label(x_name.clone());
    } else {
        plot = plot.x_axis_formatter(cat_fmt).y_axis_formatter(value_fmt).x_axis_label(x_name.clone());
    }
    if category {
        // one grid mark per category, labels thinned to what fits: side by side on the X axis
        // (about 7 px per character of the longest label, capped), one text row each on the Y
        // axis; a big step size keeps the labels in the tier egui_plot draws text for
        let longest = data.x_labels.iter().map(|l| l.chars().count()).max().unwrap_or(4).min(AXIS_LABEL_CHARS);
        let (axis_px, per_label) = if horizontal { (ui.available_height(), 18.0) } else { (ui.available_width(), longest as f32 * 7.0 + 24.0) };
        let max_labels = ((axis_px / per_label).floor() as usize).max(2);
        let step = n.div_ceil(max_labels).max(1);
        let marks = move |_input: egui_plot::GridInput| -> Vec<egui_plot::GridMark> { (0..n).step_by(step).map(|i| egui_plot::GridMark { value: i as f64, step_size: 1e12 }).collect() };
        plot = if horizontal { plot.y_grid_spacer(marks) } else { plot.x_grid_spacer(marks) };
    }
    let mut clicked: Option<usize> = None;
    let response = plot.show(ui, |plot_ui| {
        match spec.kind {
            ChartKind::Bar | ChartKind::Histogram => {
                let n_series = data.series.len().max(1);
                let width = if binned { 1.0 } else if category || data.x_kind == XKind::RowNumber { 0.8 } else { bar_width(&data.x_values) };
                let mut charts: Vec<BarChart> = Vec::new();
                for (s, series) in data.series.iter().enumerate() {
                    let (w, offset) = if spec.stacked { (width, 0.0) } else { (width / n_series as f64, (s as f64 - (n_series as f64 - 1.0) / 2.0) * width / n_series as f64) };
                    let bars: Vec<Bar> = (0..n).filter(|i| !series.values[*i].is_nan()).map(|i| Bar::new(x_of(i) + offset, series.values[i]).width(w).fill(series_color(s)).name(&series.name)).collect();
                    let mut chart = BarChart::new(series.name.clone(), bars).color(series_color(s));
                    if horizontal {
                        chart = chart.horizontal();
                    }
                    if spec.stacked && !charts.is_empty() {
                        let refs: Vec<&BarChart> = charts.iter().collect();
                        chart = chart.stack_on(&refs);
                    }
                    charts.push(chart);
                }
                for chart in charts {
                    plot_ui.bar_chart(chart);
                }
            }
            ChartKind::Line | ChartKind::Area => {
                let stacked = spec.kind == ChartKind::Area && spec.stacked;
                // stacked: each series sits on the ones before it; drawn top band first so every
                // band stays visible under the later fills
                let mut tops: Vec<Vec<f64>> = Vec::with_capacity(data.series.len());
                let mut base: Vec<f64> = vec![0.0; n];
                for series in &data.series {
                    let top: Vec<f64> = (0..n).map(|i| if stacked { if series.values[i].is_nan() { base[i] } else { base[i] + series.values[i] } } else { series.values[i] }).collect();
                    if stacked {
                        base.clone_from(&top);
                    }
                    tops.push(top);
                }
                let order: Vec<usize> = if stacked { (0..data.series.len()).rev().collect() } else { (0..data.series.len()).collect() };
                for s in order {
                    let series = &data.series[s];
                    let pts: Vec<[f64; 2]> = (0..n).filter(|i| !series.values[*i].is_nan()).map(|i| [x_of(i), tops[s][i]]).collect();
                    let mut line = Line::new(series.name.clone(), PlotPoints::from(pts)).color(series_color(s)).width(1.8);
                    if spec.kind == ChartKind::Area {
                        line = line.fill(0.0).fill_alpha(if stacked { 0.9 } else { 0.35 });
                    }
                    plot_ui.line(line);
                }
            }
            ChartKind::Scatter => {
                for (s, series) in data.series.iter().enumerate() {
                    let pts: Vec<[f64; 2]> = (0..n).filter(|i| !series.values[*i].is_nan()).map(|i| [x_of(i), series.values[i]]).collect();
                    plot_ui.points(Points::new(series.name.clone(), PlotPoints::from(pts)).color(series_color(s)).radius(if n > 20_000 { 1.5 } else { 2.5 }));
                }
            }
            ChartKind::Pie => {}
        }
        if plot_ui.response().clicked() {
            if let Some(p) = plot_ui.pointer_coordinate() {
                let xv = if horizontal { p.y } else { p.x };
                clicked = nearest_point(data, xv, category);
            }
        }
    });
    c.rect = Some(response.response.rect);
    let _ = theme;
    clicked
}

/// The index of the point nearest to an X coordinate.
fn nearest_point(data: &ChartData, xv: f64, category: bool) -> Option<usize> {
    let n = data.points();
    if n == 0 {
        return None;
    }
    if category {
        let i = xv.round();
        return (i >= -0.5 && (i as usize) < n && (xv - i).abs() <= 0.5).then_some(i as usize);
    }
    let mut best = None;
    let mut best_d = f64::INFINITY;
    for (i, x) in data.x_values.iter().enumerate() {
        let d = (x - xv).abs();
        if d < best_d {
            best_d = d;
            best = Some(i);
        }
    }
    best
}

/// A bar width for numeric X: a fraction of the smallest gap between neighbours.
fn bar_width(xs: &[f64]) -> f64 {
    let mut sorted: Vec<f64> = xs.iter().copied().filter(|v| !v.is_nan()).collect();
    sorted.sort_by(|a, b| a.partial_cmp(b).unwrap_or(std::cmp::Ordering::Equal));
    let mut gap = f64::INFINITY;
    for w in sorted.windows(2) {
        let d = w[1] - w[0];
        if d > 0.0 && d < gap {
            gap = d;
        }
    }
    if gap.is_finite() { gap * 0.8 } else { 1.0 }
}

/// A pie (donut) of the first series: sectors painted directly, labels with share on the
/// right. Returns the clicked sector.
fn pie(ui: &mut Ui, c: &mut ChartView, data: &Arc<ChartData>, theme: &Theme) -> Option<usize> {
    let series = data.series.first()?;
    let total: f64 = series.values.iter().filter(|v| v.is_finite() && **v > 0.0).sum();
    let avail = ui.available_size();
    let (rect, response) = ui.allocate_exact_size(avail, egui::Sense::click());
    c.rect = Some(rect);
    if total <= 0.0 {
        ui.painter().text(rect.center(), egui::Align2::CENTER_CENTER, "No positive values to draw.", egui::FontId::proportional(13.0), theme.text_muted);
        return None;
    }
    let painter = ui.painter_at(rect);
    let legend_w = 220.0_f32.min(rect.width() * 0.4);
    let radius = ((rect.width() - legend_w) * 0.5).min(rect.height() * 0.5) - 12.0;
    let center = egui::pos2(rect.left() + (rect.width() - legend_w) * 0.5, rect.center().y);
    let inner = radius * 0.45;
    let pointer = response.hover_pos();
    let mut hovered: Option<usize> = None;
    let mut angle = -std::f32::consts::FRAC_PI_2;
    let n = series.values.len();
    for i in 0..n {
        let v = series.values[i];
        if !(v.is_finite() && v > 0.0) {
            continue;
        }
        let sweep = (v / total) as f32 * std::f32::consts::TAU;
        let steps = ((sweep / 0.05).ceil() as usize).max(2);
        let mut pts: Vec<egui::Pos2> = Vec::with_capacity(steps * 2 + 2);
        for k in 0..=steps {
            let a = angle + sweep * k as f32 / steps as f32;
            pts.push(egui::pos2(center.x + radius * a.cos(), center.y + radius * a.sin()));
        }
        for k in (0..=steps).rev() {
            let a = angle + sweep * k as f32 / steps as f32;
            pts.push(egui::pos2(center.x + inner * a.cos(), center.y + inner * a.sin()));
        }
        if let Some(p) = pointer {
            let d = p - center;
            let r = d.length();
            if r >= inner && r <= radius {
                let mut a = d.y.atan2(d.x);
                while a < angle {
                    a += std::f32::consts::TAU;
                }
                if a >= angle && a <= angle + sweep {
                    hovered = Some(i);
                }
            }
        }
        let color = if hovered == Some(i) { theme.tint(series_color(i), 0.85) } else { series_color(i) };
        painter.add(egui::Shape::convex_polygon(pts, color, egui::Stroke::new(1.0, theme.bg_panel)));
        angle += sweep;
    }
    // legend with shares
    let mut y = rect.top() + 8.0;
    let x0 = rect.right() - legend_w + 8.0;
    let shown = n.min(40);
    for i in 0..shown {
        let v = series.values[i];
        if !(v.is_finite() && v > 0.0) {
            continue;
        }
        let share = v / total * 100.0;
        painter.rect_filled(egui::Rect::from_min_size(egui::pos2(x0, y + 2.0), Vec2::new(10.0, 10.0)), 2.0, series_color(i));
        let label = format!("{}  {} ({:.1}%)", data.x_label(i), chart::fmt_num(v), share);
        painter.text(egui::pos2(x0 + 16.0, y), egui::Align2::LEFT_TOP, label, egui::FontId::proportional(12.0), if hovered == Some(i) { theme.text } else { theme.text_muted });
        y += 16.0;
        if y > rect.bottom() - 12.0 {
            break;
        }
    }
    if n > shown {
        painter.text(egui::pos2(x0, y), egui::Align2::LEFT_TOP, format!("… {} more (use Top)", n - shown), egui::FontId::proportional(11.0), theme.text_faint);
    }
    if let Some(h) = hovered {
        let v = series.values[h];
        response.clone().on_hover_text(format!("{}\n{}: {} ({:.1}%)", data.x_label(h), series.name, chart::fmt_num(v), v / total * 100.0));
    }
    if response.clicked() { hovered } else { None }
}
