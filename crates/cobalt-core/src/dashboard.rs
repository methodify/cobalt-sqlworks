//! A dashboard: a page of tiles, each a query on a connection shown as a number, a chart or a
//! small grid, refreshed on an interval. Saved as `<name>.cobalt-dash.json`, shareable: the
//! connection is named by profile id, name and server so another machine resolves it from its
//! own library.

use crate::ChartSpec;
use serde::{Deserialize, Serialize};

pub const DASHBOARD_EXT: &str = ".cobalt-dash.json";

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct DashboardFile {
    pub version: u32,
    pub title: String,
    /// Tile columns across the page (tiles of size L span two).
    pub columns: u32,
    pub tiles: Vec<TileSpec>,
}

impl Default for DashboardFile {
    fn default() -> Self {
        Self { version: 1, title: "Dashboard".into(), columns: 3, tiles: Vec::new() }
    }
}

/// The connection a tile runs on: resolved by id first, then by name, then by server.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(default)]
pub struct ProfileRef {
    pub id: Option<String>,
    pub name: String,
    pub server: String,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(rename_all = "snake_case")]
pub enum TileKind {
    /// The first cell of the first result set, large.
    Number,
    #[default]
    Chart,
    /// The first rows of the first result set.
    Grid,
}

impl TileKind {
    pub const ALL: [TileKind; 3] = [TileKind::Number, TileKind::Chart, TileKind::Grid];
    pub fn label(self) -> &'static str {
        match self {
            TileKind::Number => "Number",
            TileKind::Chart => "Chart",
            TileKind::Grid => "Grid",
        }
    }
    pub fn key(self) -> &'static str {
        match self {
            TileKind::Number => "number",
            TileKind::Chart => "chart",
            TileKind::Grid => "grid",
        }
    }
    pub fn parse(s: &str) -> Option<Self> {
        Self::ALL.into_iter().find(|k| k.key().eq_ignore_ascii_case(s.trim()))
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(rename_all = "snake_case")]
pub enum TileSize {
    Small,
    #[default]
    Medium,
    Large,
}

impl TileSize {
    pub const ALL: [TileSize; 3] = [TileSize::Small, TileSize::Medium, TileSize::Large];
    pub fn label(self) -> &'static str {
        match self {
            TileSize::Small => "Small",
            TileSize::Medium => "Medium",
            TileSize::Large => "Large",
        }
    }
    pub fn key(self) -> &'static str {
        match self {
            TileSize::Small => "small",
            TileSize::Medium => "medium",
            TileSize::Large => "large",
        }
    }
    pub fn parse(s: &str) -> Option<Self> {
        Self::ALL.into_iter().find(|k| k.key().eq_ignore_ascii_case(s.trim()))
    }
    /// Columns a tile spans and its height in pixels.
    pub fn span(self) -> u32 {
        match self {
            TileSize::Large => 2,
            _ => 1,
        }
    }
    pub fn height(self) -> f32 {
        match self {
            TileSize::Small => 160.0,
            TileSize::Medium => 260.0,
            TileSize::Large => 360.0,
        }
    }
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct TileSpec {
    pub id: String,
    pub title: String,
    pub profile: ProfileRef,
    pub database: Option<String>,
    pub sql: String,
    pub kind: TileKind,
    pub chart: Option<ChartSpec>,
    /// Seconds between refreshes; 0 = only by hand.
    pub interval_secs: u64,
    pub size: TileSize,
}

impl Default for TileSpec {
    fn default() -> Self {
        Self { id: new_tile_id(), title: "New tile".into(), profile: ProfileRef::default(), database: None, sql: String::new(), kind: TileKind::Chart, chart: None, interval_secs: 300, size: TileSize::Medium }
    }
}

pub fn new_tile_id() -> String {
    uuid::Uuid::new_v4().simple().to_string()[..8].to_string()
}

/// The refresh intervals offered in the UI (seconds; 0 = by hand).
pub const INTERVALS: [(u64, &str); 7] = [(0, "by hand"), (30, "30 s"), (60, "1 min"), (300, "5 min"), (900, "15 min"), (1800, "30 min"), (3600, "1 hour")];

pub fn interval_label(secs: u64) -> String {
    INTERVALS.iter().find(|(s, _)| *s == secs).map(|(_, l)| l.to_string()).unwrap_or_else(|| format!("{secs} s"))
}

impl DashboardFile {
    pub fn parse(text: &str) -> Result<Self, String> {
        let f: DashboardFile = serde_json::from_str(text).map_err(|e| e.to_string())?;
        if f.version > 1 {
            return Err(format!("dashboard file version {} is newer than this Cobalt understands", f.version));
        }
        Ok(f)
    }
    pub fn to_json(&self) -> String {
        serde_json::to_string_pretty(self).unwrap_or_default()
    }
    /// A file name is a dashboard when it ends with the dashboard extension.
    pub fn is_dashboard_path(path: &std::path::Path) -> bool {
        path.file_name().map(|n| n.to_string_lossy().to_ascii_lowercase().ends_with(DASHBOARD_EXT)).unwrap_or(false)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn round_trip_and_defaults() {
        let mut f = DashboardFile::default();
        f.tiles.push(TileSpec { title: "Orders today".into(), sql: "SELECT COUNT(*) FROM orders".into(), kind: TileKind::Number, interval_secs: 60, ..Default::default() });
        let text = f.to_json();
        let back = DashboardFile::parse(&text).unwrap();
        assert_eq!(back, f);
        assert_eq!(back.tiles[0].kind, TileKind::Number);
        // older files with missing fields still open
        let old = DashboardFile::parse(r#"{"title": "x", "tiles": [{"title": "t", "sql": "select 1"}]}"#).unwrap();
        assert_eq!(old.columns, 3);
        assert_eq!(old.tiles[0].kind, TileKind::Chart);
        assert_eq!(old.tiles[0].interval_secs, 300);
        assert!(DashboardFile::parse(r#"{"version": 9}"#).is_err());
        assert!(DashboardFile::is_dashboard_path(std::path::Path::new("C:/x/Sales.Cobalt-Dash.JSON")));
        assert!(!DashboardFile::is_dashboard_path(std::path::Path::new("C:/x/sales.json")));
        assert_eq!(TileSize::Large.span(), 2);
        assert_eq!(interval_label(300), "5 min");
        assert_eq!(TileKind::parse("grid"), Some(TileKind::Grid));
    }
}
