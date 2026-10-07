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
    pub notebooks: NotebookSettings,
    pub spark: SparkSettings,
    /// Keyboard shortcut overrides: command id → "Ctrl+Shift+P" (empty = unbound).
    pub keybindings: std::collections::BTreeMap<String, String>,
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
    /// Append the connection's SPID to tab titles.
    pub spid_in_tab_title: bool,
    /// Show the getting-started pane next to a fresh, empty query tab.
    pub show_welcome: bool,
}
impl Default for Appearance {
    fn default() -> Self {
        Self { theme: ThemeChoice::System, ui_scale: 1.0, editor_font_size: 14.0, grid_font_size: 13.0, ui_font_size: 13.0, spid_in_tab_title: false, show_welcome: true }
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
    /// Format document: keyword case, indent width, blank lines between statements.
    pub format_uppercase_keywords: bool,
    pub format_indent: u8,
    pub format_blank_lines: u8,
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
            format_uppercase_keywords: true,
            format_indent: 4,
            format_blank_lines: 1,
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
    /// Session SET defaults for every new tab (each tab can override in Execution options).
    pub session: crate::ExecOptions,
    /// Keyboard shortcuts that run a procedure on the selected text (SSMS's Alt+F1 = sp_help).
    pub query_shortcuts: Vec<QueryShortcut>,
}
impl Default for ExecutionSettings {
    fn default() -> Self {
        Self { row_cap: 10_000, command_timeout_secs: 0, select_top_n: 1000, stop_on_error: true, arithabort: true, session: crate::ExecOptions::default(), query_shortcuts: QueryShortcut::defaults() }
    }
}

/// A key combination that runs `sql` with `{sel}` replaced by the editor's selection (or the word
/// at the caret), single quotes doubled.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(default)]
pub struct QueryShortcut {
    /// e.g. "Alt+F1", "Ctrl+1".
    pub keys: String,
    pub sql: String,
}
impl QueryShortcut {
    pub fn defaults() -> Vec<QueryShortcut> {
        vec![
            QueryShortcut { keys: "Alt+F1".into(), sql: "EXEC sp_help N'{sel}'".into() },
            QueryShortcut { keys: "Ctrl+1".into(), sql: "EXEC sp_who".into() },
            QueryShortcut { keys: "Ctrl+2".into(), sql: "EXEC sp_lock".into() },
            QueryShortcut { keys: "Ctrl+3".into(), sql: "EXEC sp_helptext N'{sel}'".into() },
        ]
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
pub struct NotebookSettings {
    /// Rows of each result set kept in the notebook file as an Arrow payload (0 = none).
    pub max_output_rows: u64,
    /// `sql` or `pyspark`: what New Notebook creates.
    pub default_language: String,
    /// Height of a result grid under a cell, in rows.
    pub grid_rows: u32,
    /// Rows a bare DataFrame expression or a `%%sql` cell brings back from local Spark
    /// (`display(df)` uses Fabric's 1,000 unless given `limit=`).
    pub spark_row_limit: u64,
}
impl Default for NotebookSettings {
    fn default() -> Self {
        Self { max_output_rows: 1000, default_language: "sql".into(), grid_rows: 12, spark_row_limit: 10_000 }
    }
}

/// The Cobalt-managed local Spark runtime (Settings → Spark runtime).
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct SparkSettings {
    /// Runtime profile: `fabric-2.0` or `fabric-1.3`.
    pub profile: String,
    /// `microsoft` (Microsoft Build of OpenJDK) or `temurin`.
    pub jdk_vendor: String,
    /// A JDK home to use instead of a managed one ("Use what I have").
    pub java_home: Option<String>,
    /// Spark driver memory, e.g. `4g`.
    pub driver_memory: String,
    /// Where the runtime lives; default is the app's local data folder.
    pub runtime_dir: Option<String>,
    /// Python packages for the environment: PyPI requirement specs or wheel/sdist paths.
    pub python_packages: Vec<String>,
    /// Jar files put on the Spark classpath at session start.
    pub jars: Vec<String>,
    /// Maven coordinates (`group:artifact:version`) fetched into the runtime and put on the classpath.
    pub maven: Vec<String>,
    /// When the session ends on its own: `keep` (only when stopped), `idle` (after `idle_minutes`
    /// with no cell or preload), `last_notebook` (when the last Spark notebook closes).
    pub lifecycle: String,
    pub idle_minutes: u32,
    /// When the session starts ahead of the first cell: `notebook_open` (a Spark notebook is
    /// opened or created), `app_start`, or `first_cell`.
    pub early_start: String,
    /// Lakehouse `Files/`: `lazy` (Spark streams from OneLake; Python fetches single files on
    /// first open) or `mirror` (folders are pulled locally with sync_files).
    pub files_mode: String,
}
impl Default for SparkSettings {
    fn default() -> Self {
        Self { profile: "fabric-2.0".into(), jdk_vendor: "microsoft".into(), java_home: None, driver_memory: "4g".into(), runtime_dir: None, python_packages: Vec::new(), jars: Vec::new(), maven: Vec::new(), lifecycle: "idle".into(), idle_minutes: 60, early_start: "notebook_open".into(), files_mode: "lazy".into() }
    }
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct AdvancedSettings {
    /// Bytes of result data held in RAM before spilling to disk.
    pub memory_budget_bytes: u64,
    pub temp_dir: Option<String>,
    pub log_level: String,
    /// `auto` (GPU via wgpu; Mesa OpenGL when there is no GPU), `wgpu-only`, or `opengl`.
    /// Legacy value `wgpu` behaves like `auto`. Takes effect at the next start.
    pub renderer: String,
}
impl Default for AdvancedSettings {
    fn default() -> Self {
        Self { memory_budget_bytes: 1024 * 1024 * 1024, temp_dir: None, log_level: "info".into(), renderer: "auto".into() }
    }
}
