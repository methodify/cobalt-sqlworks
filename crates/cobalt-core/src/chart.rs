//! A chart over a result set: what to draw, by column name (so a spec survives a re-run and a
//! notebook save). The data behind it is built by `cobalt_results::chart`.

use serde::{Deserialize, Serialize};

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(rename_all = "snake_case")]
pub enum ChartKind {
    #[default]
    Bar,
    Line,
    Area,
    Scatter,
    Pie,
}

impl ChartKind {
    pub const ALL: [ChartKind; 5] = [ChartKind::Bar, ChartKind::Line, ChartKind::Area, ChartKind::Scatter, ChartKind::Pie];
    pub fn label(self) -> &'static str {
        match self {
            ChartKind::Bar => "Bar",
            ChartKind::Line => "Line",
            ChartKind::Area => "Area",
            ChartKind::Scatter => "Scatter",
            ChartKind::Pie => "Pie",
        }
    }
    pub fn key(self) -> &'static str {
        match self {
            ChartKind::Bar => "bar",
            ChartKind::Line => "line",
            ChartKind::Area => "area",
            ChartKind::Scatter => "scatter",
            ChartKind::Pie => "pie",
        }
    }
    pub fn parse(s: &str) -> Option<Self> {
        Self::ALL.into_iter().find(|k| k.key().eq_ignore_ascii_case(s.trim()))
    }
}

/// How repeated X values fold their Y values. `None` plots every row.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(rename_all = "snake_case")]
pub enum Agg {
    None,
    #[default]
    Sum,
    Count,
    Avg,
    Min,
    Max,
    CountDistinct,
}

impl Agg {
    pub const ALL: [Agg; 7] = [Agg::None, Agg::Sum, Agg::Count, Agg::Avg, Agg::Min, Agg::Max, Agg::CountDistinct];
    pub fn label(self) -> &'static str {
        match self {
            Agg::None => "none (every row)",
            Agg::Sum => "sum",
            Agg::Count => "count",
            Agg::Avg => "average",
            Agg::Min => "min",
            Agg::Max => "max",
            Agg::CountDistinct => "count distinct",
        }
    }
    pub fn key(self) -> &'static str {
        match self {
            Agg::None => "none",
            Agg::Sum => "sum",
            Agg::Count => "count",
            Agg::Avg => "avg",
            Agg::Min => "min",
            Agg::Max => "max",
            Agg::CountDistinct => "count_distinct",
        }
    }
    pub fn parse(s: &str) -> Option<Self> {
        let s = s.trim().to_ascii_lowercase().replace([' ', '-'], "_");
        Self::ALL.into_iter().find(|a| a.key() == s || a.label().replace(' ', "_") == s)
    }
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, Default)]
#[serde(default)]
pub struct ChartSpec {
    pub kind: ChartKind,
    /// The X column; None = row number (line / scatter) or the first text column (bar / pie).
    pub x: Option<String>,
    /// The Y columns, one series each (count needs none).
    pub y: Vec<String>,
    pub agg: Agg,
    /// Keep the N largest X categories (by the first series) and fold the rest into "other".
    pub top_n: Option<usize>,
    pub stacked: bool,
    pub horizontal: bool,
}

impl ChartSpec {
    pub fn is_empty(&self) -> bool {
        self.y.is_empty() && self.agg != Agg::Count
    }
}
