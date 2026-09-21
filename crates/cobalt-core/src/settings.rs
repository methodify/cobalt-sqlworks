use serde::{Deserialize, Serialize};

/// User settings, persisted as TOML. Every field has a default so partial files load.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, Default)]
#[serde(default)]
pub struct Settings {
    pub appearance: Appearance,
    pub editor: EditorSettings,
    pub execution: ExecutionSettings,
    pub results: ResultsSettings,
    pub export: ExportSettings,
    pub connections: ConnectionSettings,
    pub history: HistorySettings,
    pub updates: UpdateSettings,
    pub advanced: AdvancedSettings,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(rename_all = "snake_case")]
pub enum ThemeChoice {
    #[default]
    System,
    Light,
    Dark,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct Appearance {
    pub theme: ThemeChoice,
    pub ui_scale: f32,
    pub editor_font_size: f32,
    pub grid_font_size: f32,
    pub ui_font_size: f32,
}
impl Default for Appearance {
    fn default() -> Self {
        Self { theme: ThemeChoice::System, ui_scale: 1.0, editor_font_size: 14.0, grid_font_size: 13.0, ui_font_size: 13.0 }
    }
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct EditorSettings {
    pub tab_size: u8,
    pub insert_spaces: bool,
    pub word_wrap: bool,
    pub auto_close_brackets: bool,
    pub highlight_current_statement: bool,
    pub completion_enabled: bool,
    pub completion_on_type: bool,
    pub uppercase_keywords_on_complete: bool,
    pub auto_save: bool,
}
impl Default for EditorSettings {
    fn default() -> Self {
        Self {
            tab_size: 4,
            insert_spaces: true,
            word_wrap: false,
            auto_close_brackets: true,
            highlight_current_statement: true,
            completion_enabled: true,
            completion_on_type: true,
            uppercase_keywords_on_complete: true,
            auto_save: false,
        }
    }
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct ExecutionSettings {
    pub row_cap: u64,
    pub command_timeout_secs: u32,
    pub select_top_n: u32,
    pub stop_on_error: bool,
    pub arithabort: bool,
}
impl Default for ExecutionSettings {
    fn default() -> Self {
        Self { row_cap: 10_000, command_timeout_secs: 0, select_top_n: 1000, stop_on_error: true, arithabort: true }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(rename_all = "snake_case")]
pub enum ResultLayout {
    #[default]
    Stacked,
    Tabs,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct ResultsSettings {
    pub layout: ResultLayout,
    pub null_text: String,
    pub bit_as_number: bool,
    pub max_column_width: f32,
    pub auto_size_sample_rows: usize,
    pub datetime_format: String,
    pub show_row_numbers: bool,
    pub copy_include_headers: bool,
    pub copy_null_as: String,
}
impl Default for ResultsSettings {
    fn default() -> Self {
        Self {
            layout: ResultLayout::Stacked,
            null_text: "NULL".into(),
            bit_as_number: true,
            max_column_width: 400.0,
            auto_size_sample_rows: 200,
            datetime_format: "%Y-%m-%d %H:%M:%S%.f".into(),
            show_row_numbers: true,
            copy_include_headers: false,
            copy_null_as: "NULL".into(),
        }
    }
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct ExportSettings {
    pub csv_delimiter: String,
    pub csv_include_headers: bool,
    pub csv_quote_all: bool,
    pub csv_line_ending: String,
    pub csv_bom: bool,
    pub csv_null_as: String,
    pub json_lines: bool,
    pub json_pretty: bool,
    pub xml_row_element: String,
    pub xml_attribute_style: bool,
    pub parquet_compression: String,
    pub parquet_row_group_rows: usize,
    pub excel_freeze_header: bool,
    pub excel_autofilter: bool,
    pub excel_bold_header: bool,
    pub delta_mode: String,
    pub open_after_save: bool,
    pub last_dir: Option<String>,
    /// Extension of the format chosen last time (`csv`, `parquet`, `delta`…).
    #[serde(default)]
    pub last_format: Option<String>,
}
impl Default for ExportSettings {
    fn default() -> Self {
        Self {
            csv_delimiter: ",".into(),
            csv_include_headers: true,
            csv_quote_all: false,
            csv_line_ending: "\r\n".into(),
            csv_bom: false,
            csv_null_as: String::new(),
            json_lines: false,
            json_pretty: true,
            xml_row_element: "row".into(),
            xml_attribute_style: false,
            parquet_compression: "zstd".into(),
            parquet_row_group_rows: 131_072,
            excel_freeze_header: true,
            excel_autofilter: true,
            excel_bold_header: true,
            delta_mode: "create".into(),
            open_after_save: false,
            last_dir: None,
            last_format: None,
        }
    }
}

/// Cobalt SQL Works public-client app registration (multi-tenant, personal accounts allowed).
pub const DEFAULT_ENTRA_CLIENT_ID: &str = "ecec63e7-6f92-470c-be3e-0fccff2e8c34";

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct ConnectionSettings {
    /// Entra public-client app id used for interactive/device-code auth. Overridable per user.
    pub entra_client_id: String,
    pub entra_default_tenant: Option<String>,
    pub entra_redirect_port: Option<u16>,
    pub default_auth: String,
    pub metadata_connection: bool,
    pub reconnect_on_run: bool,
}
impl ConnectionSettings {
    /// The configured client id, or Cobalt's default when the setting is blank.
    pub fn effective_entra_client_id(&self) -> &str {
        let t = self.entra_client_id.trim();
        if t.is_empty() { DEFAULT_ENTRA_CLIENT_ID } else { t }
    }
}

impl Default for ConnectionSettings {
    fn default() -> Self {
        Self {
            // Cobalt SQL Works public-client registration (multi-tenant). Users can override.
            entra_client_id: DEFAULT_ENTRA_CLIENT_ID.into(),
            entra_default_tenant: None,
            entra_redirect_port: None,
            default_auth: "sql_login".into(),
            metadata_connection: true,
            reconnect_on_run: true,
        }
    }
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct HistorySettings {
    pub capture: bool,
    pub retention_days: u32,
    pub max_entries: u32,
}
impl Default for HistorySettings {
    fn default() -> Self {
        Self { capture: true, retention_days: 90, max_entries: 10_000 }
    }
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct UpdateSettings {
    /// Ask GitHub for the latest release shortly after start-up (one small anonymous request).
    pub check_on_startup: bool,
    /// A version the user chose to skip; the start-up check stays quiet about it.
    pub skipped_version: Option<String>,
}
impl Default for UpdateSettings {
    fn default() -> Self {
        Self { check_on_startup: true, skipped_version: None }
    }
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct AdvancedSettings {
    /// Bytes of result data held in RAM before spilling to disk.
    pub memory_budget_bytes: u64,
    pub temp_dir: Option<String>,
    pub log_level: String,
    pub renderer: String,
}
impl Default for AdvancedSettings {
    fn default() -> Self {
        Self { memory_budget_bytes: 1024 * 1024 * 1024, temp_dir: None, log_level: "info".into(), renderer: "wgpu".into() }
    }
}
