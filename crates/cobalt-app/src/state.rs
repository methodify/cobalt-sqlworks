//! UI-side application state. Plain data mutated by the UI thread and by drained session events.

use crate::session::{Event, MetadataRequest};
use cobalt_core::*;
use cobalt_results::{CellFormatter, ResultSet, ViewSpec};
use std::collections::{HashMap, HashSet};
use std::path::PathBuf;
use std::sync::Arc;
use std::time::{Duration, Instant};

// ---------------------------------------------------------------------------------------------
// Connection library + object explorer model
// ---------------------------------------------------------------------------------------------

#[derive(Default)]
pub struct Library {
    pub groups: Vec<ServerGroup>,
    pub profiles: Vec<ConnectionProfile>,
    /// Per-profile live tree data.
    pub servers: HashMap<ProfileId, ServerNode>,
    pub expanded_groups: HashSet<GroupId>,
    pub filter: String,
}

impl Library {
    pub fn group(&self, id: GroupId) -> Option<&ServerGroup> {
        self.groups.iter().find(|g| g.id == id)
    }
    pub fn profile(&self, id: ProfileId) -> Option<&ConnectionProfile> {
        self.profiles.iter().find(|p| p.id == id)
    }
    pub fn profile_mut(&mut self, id: ProfileId) -> Option<&mut ConnectionProfile> {
        self.profiles.iter_mut().find(|p| p.id == id)
    }
    pub fn color_for(&self, p: &ConnectionProfile) -> Option<Color> {
        p.color.or_else(|| p.group.and_then(|g| self.group(g)).map(|g| g.color))
    }
    pub fn server(&mut self, id: ProfileId) -> &mut ServerNode {
        self.servers.entry(id).or_default()
    }
}

#[derive(Default)]
pub struct ServerNode {
    pub expanded: bool,
    pub engine: Option<EngineInfo>,
    pub databases: Loadable<Vec<DatabaseInfo>>,
    pub db_nodes: HashMap<String, DbNode>,
    /// Resolved credentials for the metadata connection (kept so we can re-issue requests
    /// without prompting again). Cleared on disconnect.
    pub creds: Option<ResolvedCredentials>,
    pub show_system_dbs: bool,
}

#[derive(Default)]
pub struct DbNode {
    pub expanded: bool,
    pub objects: Loadable<Vec<ObjectRef>>,
    pub expanded_folders: HashSet<Folder>,
    pub expanded_objects: HashSet<i32>,
    pub columns: HashMap<i32, Loadable<Vec<ColumnInfo>>>,
    pub parameters: HashMap<i32, Loadable<Vec<ParameterInfo>>>,
    pub indexes: HashMap<i32, Loadable<Vec<IndexInfo>>>,
    pub keys: HashMap<i32, Loadable<Vec<KeyInfo>>>,
    pub expanded_subfolders: HashSet<(i32, SubFolder)>,
    pub catalog: Loadable<DatabaseCatalog>,
    pub filter: String,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum Folder {
    Tables,
    Views,
    Programmability,
    Procedures,
    Functions,
    TableFunctions,
    ScalarFunctions,
    Schemas,
    Synonyms,
    Sequences,
    TableTypes,
}

impl Folder {
    pub fn label(&self) -> &'static str {
        match self {
            Folder::Tables => "Tables",
            Folder::Views => "Views",
            Folder::Programmability => "Programmability",
            Folder::Procedures => "Stored Procedures",
            Folder::Functions => "Functions",
            Folder::TableFunctions => "Table-valued Functions",
            Folder::ScalarFunctions => "Scalar-valued Functions",
            Folder::Schemas => "Schemas",
            Folder::Synonyms => "Synonyms",
            Folder::Sequences => "Sequences",
            Folder::TableTypes => "User-defined Table Types",
        }
    }
    pub fn kinds(&self) -> &'static [ObjectKind] {
        match self {
            Folder::Tables => &[ObjectKind::Table],
            Folder::Views => &[ObjectKind::View],
            Folder::Procedures => &[ObjectKind::Procedure],
            Folder::TableFunctions => &[ObjectKind::TableFunction],
            Folder::ScalarFunctions => &[ObjectKind::ScalarFunction, ObjectKind::AggregateFunction],
            Folder::Schemas => &[ObjectKind::Schema],
            Folder::Synonyms => &[ObjectKind::Synonym],
            Folder::Sequences => &[ObjectKind::Sequence],
            Folder::TableTypes => &[ObjectKind::TableType],
            Folder::Programmability | Folder::Functions => &[],
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum SubFolder {
    Columns,
    Keys,
    Indexes,
    Parameters,
}

#[derive(Debug)]
pub enum Loadable<T> {
    NotLoaded,
    Loading(RequestId),
    Loaded(T),
    Failed(String),
}

impl<T> Default for Loadable<T> {
    fn default() -> Self {
        Loadable::NotLoaded
    }
}

impl<T> Loadable<T> {
    pub fn get(&self) -> Option<&T> {
        match self {
            Loadable::Loaded(t) => Some(t),
            _ => None,
        }
    }
    pub fn is_loading(&self) -> bool {
        matches!(self, Loadable::Loading(_))
    }
    pub fn needs_load(&self) -> bool {
        matches!(self, Loadable::NotLoaded)
    }
}

/// What a pending metadata request will fill in when it completes.
#[derive(Debug, Clone)]
pub struct PendingMeta {
    pub profile: ProfileId,
    pub kind: MetadataRequest,
    pub purpose: MetaPurpose,
}

#[derive(Debug, Clone)]
pub enum MetaPurpose {
    Tree,
    /// Completion catalog for an editor tab.
    Catalog { tab: TabId, database: String },
    /// Script as … → open a new tab with the text.
    ScriptToTab { title: String, profile: ProfileId, database: String, run: bool },
    /// Databases list for a tab's dropdown.
    TabDatabases { tab: TabId },
}

// ---------------------------------------------------------------------------------------------
// Editor tabs
// ---------------------------------------------------------------------------------------------

#[derive(Clone, Debug, PartialEq)]
pub enum ConnState {
    Disconnected,
    Connecting,
    Connected { engine: EngineInfo, spid: Option<i32>, database: String },
    Failed { error: String, hint: Option<String> },
}

impl ConnState {
    pub fn is_connected(&self) -> bool {
        matches!(self, ConnState::Connected { .. })
    }
    pub fn database(&self) -> Option<&str> {
        match self {
            ConnState::Connected { database, .. } => Some(database),
            _ => None,
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ResultsTab {
    Results,
    Messages,
    Plan,
}

pub struct EditorTab {
    pub id: TabId,
    pub title: String,
    pub custom_title: bool,
    pub pinned: bool,
    pub text: String,
    pub file_path: Option<PathBuf>,
    pub saved_text_hash: u64,
    pub profile: Option<ConnectionProfile>,
    pub conn: ConnState,
    pub creds: Option<ResolvedCredentials>,
    pub databases: Loadable<Vec<DatabaseInfo>>,
    pub actual_plan: bool,
    pub exec: ExecOptions,
    pub run: Option<RunView>,
    pub results_visible: bool,
    pub results_fraction: f32,
    pub results_tab: ResultsTab,
    pub editor: EditorState,
    pub catalog: Option<Arc<DatabaseCatalog>>,
    pub catalog_database: Option<String>,
    pub last_snapshot: Instant,
    pub untitled_index: usize,
    /// Run again once the (re)connect completes.
    pub pending_run: Option<RunMode>,
    pub snapshot_hash: u64,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum RunMode {
    All,
    Selection,
    Current,
    EstimatedPlan,
}

impl EditorTab {
    pub fn new(untitled_index: usize) -> Self {
        Self {
            id: TabId::new(),
            title: format!("SQLQuery_{untitled_index}"),
            custom_title: false,
            pinned: false,
            text: String::new(),
            file_path: None,
            saved_text_hash: hash_text(""),
            profile: None,
            conn: ConnState::Disconnected,
            creds: None,
            databases: Loadable::NotLoaded,
            actual_plan: false,
            exec: ExecOptions::default(),
            run: None,
            results_visible: true,
            results_fraction: 0.45,
            results_tab: ResultsTab::Results,
            editor: EditorState::default(),
            catalog: None,
            catalog_database: None,
            last_snapshot: Instant::now(),
            untitled_index,
            pending_run: None,
            snapshot_hash: hash_text(""),
        }
    }

    pub fn is_dirty(&self) -> bool {
        hash_text(&self.text) != self.saved_text_hash
    }
    pub fn mark_saved(&mut self) {
        self.saved_text_hash = hash_text(&self.text);
    }
    pub fn is_running(&self) -> bool {
        self.run.as_ref().map(|r| r.state == RunViewState::Running || r.state == RunViewState::Paused || r.state == RunViewState::Cancelling).unwrap_or(false)
    }
    pub fn display_title(&self) -> String {
        let mut t = self.title.clone();
        if self.is_dirty() {
            t.push_str(" •");
        }
        t
    }
}

pub fn hash_text(s: &str) -> u64 {
    use std::hash::{Hash, Hasher};
    let mut h = std::collections::hash_map::DefaultHasher::new();
    s.hash(&mut h);
    h.finish()
}

#[derive(Default)]
pub struct EditorState {
    /// Char index of the primary cursor as of the last frame.
    pub cursor: usize,
    pub selection: Option<(usize, usize)>,
    pub scroll_to_cursor: bool,
    pub find_open: bool,
    pub find_text: String,
    pub replace_text: String,
    pub find_regex: bool,
    pub find_case: bool,
    pub goto_line_open: bool,
    pub goto_line_text: String,
    pub completion: Option<CompletionPopup>,
    /// Set by commands to move the cursor / replace text on the next frame.
    pub pending_edit: Option<PendingEdit>,
    pub request_focus: bool,
    pub line_count: usize,
    pub col: usize,
    pub line: usize,
    pub statement_range: Option<(usize, usize)>,
}

pub struct CompletionPopup {
    pub items: Vec<CompletionEntry>,
    pub selected: usize,
    pub replace_start: usize,
    pub replace_end: usize,
    pub anchor: egui::Pos2,
}

#[derive(Clone, Debug)]
pub struct CompletionEntry {
    pub label: String,
    pub insert: String,
    pub detail: Option<String>,
    pub icon: &'static str,
}

#[derive(Clone, Debug)]
pub enum PendingEdit {
    SetCursor(usize),
    Select(usize, usize),
    Replace { start: usize, end: usize, text: String, cursor_after: Option<usize> },
    SetText { text: String, cursor: usize },
}

// ---------------------------------------------------------------------------------------------
// Runs and results
// ---------------------------------------------------------------------------------------------

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum RunViewState {
    Running,
    Paused,
    Cancelling,
    Done,
    Failed,
    Cancelled,
}

pub struct RunView {
    pub id: RunId,
    pub started: Instant,
    pub elapsed: Duration,
    pub state: RunViewState,
    pub batches: usize,
    pub current_batch: usize,
    pub result_sets: Vec<ResultSetView>,
    pub messages: Vec<MessageLine>,
    pub total_rows: u64,
    pub rows_affected: Vec<u64>,
    pub paused_set: Option<usize>,
    pub plans: Vec<PlanView>,
    pub maximized: Option<usize>,
    pub history_id: Option<i64>,
    pub plan_mode: PlanMode,
}

impl RunView {
    pub fn new(id: RunId, plan_mode: PlanMode) -> Self {
        Self {
            id,
            started: Instant::now(),
            elapsed: Duration::ZERO,
            state: RunViewState::Running,
            batches: 0,
            current_batch: 0,
            result_sets: Vec::new(),
            messages: Vec::new(),
            total_rows: 0,
            rows_affected: Vec::new(),
            paused_set: None,
            plans: Vec::new(),
            maximized: None,
            history_id: None,
            plan_mode,
        }
    }
    pub fn is_live(&self) -> bool {
        matches!(self.state, RunViewState::Running | RunViewState::Paused | RunViewState::Cancelling)
    }
    pub fn has_error(&self) -> bool {
        self.messages.iter().any(|m| m.is_error)
    }
}

pub struct ResultSetView {
    pub rs: Arc<ResultSet>,
    pub grid: GridState,
    /// Showplan result sets are captured into `plans` instead of being shown as grids.
    pub is_plan: bool,
}

#[derive(Clone, Debug)]
pub struct MessageLine {
    pub text: String,
    pub is_error: bool,
    pub is_batch_header: bool,
    /// Editor line to jump to when clicked.
    pub line: Option<u32>,
    pub at: Instant,
}

pub struct PlanView {
    pub xml: String,
    pub statement_index: usize,
    pub error: Option<String>,
    pub parsed: Option<Arc<cobalt_plan::Plan>>,
    /// Which statement inside `parsed` is displayed.
    pub shown_statement: usize,
    pub layout: Option<Arc<cobalt_plan::Layout>>,
    pub selected_node: Option<usize>,
    pub zoom: f32,
    pub pan: egui::Vec2,
    pub show_properties: bool,
    pub show_top_ops: bool,
    pub top_ops_sort: (usize, bool),
    pub find: String,
}

impl PlanView {
    pub fn new(xml: String) -> Self {
        Self {
            xml,
            statement_index: 0,
            error: None,
            parsed: None,
            shown_statement: 0,
            layout: None,
            selected_node: None,
            zoom: 1.0,
            pan: egui::Vec2::ZERO,
            show_properties: true,
            show_top_ops: true,
            top_ops_sort: (2, true),
            find: String::new(),
        }
    }
}

/// Per-grid interactive state.
pub struct GridState {
    pub selection: Selection,
    pub anchor: Option<(usize, usize)>,
    pub view: ViewSpec,
    pub view_generation: u64,
    pub applying_view: bool,
    pub col_widths: Vec<f32>,
    pub widths_initialized: bool,
    pub filter_popup: Option<FilterPopup>,
    pub find: Option<GridFind>,
    pub viewer: Option<(usize, usize)>,
    pub scroll_to: Option<(usize, usize)>,
    pub focused: bool,
    pub frozen_cols: usize,
    pub transposed: bool,
}

impl Default for GridState {
    fn default() -> Self {
        Self {
            selection: Selection::default(),
            anchor: None,
            view: ViewSpec::default(),
            view_generation: 0,
            applying_view: false,
            col_widths: Vec::new(),
            widths_initialized: false,
            filter_popup: None,
            find: None,
            viewer: None,
            scroll_to: None,
            focused: false,
            frozen_cols: 0,
            transposed: false,
        }
    }
}

#[derive(Clone, Debug, Default, PartialEq)]
pub enum Selection {
    #[default]
    None,
    /// Inclusive rectangle in visible-row / column coordinates.
    Cells { r0: usize, c0: usize, r1: usize, c1: usize },
    Rows { r0: usize, r1: usize },
    Cols { c0: usize, c1: usize },
    All,
}

impl Selection {
    pub fn contains(&self, row: usize, col: usize) -> bool {
        match self {
            Selection::None => false,
            Selection::Cells { r0, c0, r1, c1 } => row >= *r0 && row <= *r1 && col >= *c0 && col <= *c1,
            Selection::Rows { r0, r1 } => row >= *r0 && row <= *r1,
            Selection::Cols { c0, c1 } => col >= *c0 && col <= *c1,
            Selection::All => true,
        }
    }
    /// Resolve to concrete (rows, cols) ranges given the grid dimensions.
    pub fn resolve(&self, rows: usize, cols: usize) -> Option<(std::ops::RangeInclusive<usize>, std::ops::RangeInclusive<usize>)> {
        if rows == 0 || cols == 0 {
            return None;
        }
        match self {
            Selection::None => None,
            Selection::Cells { r0, c0, r1, c1 } => Some((*r0..=(*r1).min(rows - 1), *c0..=(*c1).min(cols - 1))),
            Selection::Rows { r0, r1 } => Some((*r0..=(*r1).min(rows - 1), 0..=cols - 1)),
            Selection::Cols { c0, c1 } => Some((0..=rows - 1, *c0..=(*c1).min(cols - 1))),
            Selection::All => Some((0..=rows - 1, 0..=cols - 1)),
        }
    }
    pub fn cell_count(&self, rows: usize, cols: usize) -> usize {
        self.resolve(rows, cols).map(|(r, c)| (r.end() - r.start() + 1) * (c.end() - c.start() + 1)).unwrap_or(0)
    }
    pub fn single_cell(&self) -> Option<(usize, usize)> {
        match self {
            Selection::Cells { r0, c0, r1, c1 } if r0 == r1 && c0 == c1 => Some((*r0, *c0)),
            _ => None,
        }
    }
}

pub struct FilterPopup {
    pub column: usize,
    pub search: String,
    pub values: Vec<(Arc<str>, usize)>,
    pub truncated: bool,
    pub checked: HashSet<Arc<str>>,
    pub condition_op: usize,
    pub condition_value: String,
    pub condition_value2: String,
}

pub struct GridFind {
    pub text: String,
    pub matches: Vec<(usize, usize)>,
    pub current: usize,
    pub generation: u64,
}

// ---------------------------------------------------------------------------------------------
// Whole-app state
// ---------------------------------------------------------------------------------------------

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum SidebarView {
    Servers,
    History,
}

pub struct AppState {
    pub library: Library,
    pub tabs: Vec<EditorTab>,
    pub active_tab: Option<usize>,
    pub next_untitled: usize,
    pub sidebar_visible: bool,
    pub sidebar_view: SidebarView,
    pub sidebar_width: f32,
    pub pending_meta: HashMap<RequestId, PendingMeta>,
    pub formatter: CellFormatter,
    pub dialog: Dialog,
    pub palette_open: bool,
    pub palette_query: String,
    pub palette_selected: usize,
    pub history: HistoryView,
    pub settings_open: bool,
    pub about_open: bool,
    pub shortcuts_open: bool,
    pub ui_zoom: f32,
    pub focus: Focus,
    pub status_flash: Option<(String, Instant)>,
    pub last_hot_exit_save: Instant,
    pub recently_closed_count: usize,
    pub export_progress: Option<Arc<parking_lot::Mutex<(usize, usize)>>>,
    /// Settings changes requested by ops/UI; applied by the app (which owns `Settings`).
    pub settings_patch: Vec<SettingsPatch>,
    pub settings_draft: Option<Settings>,
    pub theme_override: Option<ThemeChoice>,
}

#[derive(Debug, Clone)]
pub enum SettingsPatch {
    LastExportDir(String),
    Theme(ThemeChoice),
    UiScale(f32),
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Default)]
pub enum Focus {
    #[default]
    Editor,
    Results,
    Tree,
    Other,
}

#[derive(Default)]
pub struct HistoryView {
    pub query: String,
    pub entries: Vec<HistoryRow>,
    pub loaded: bool,
    pub starred_only: bool,
    pub selected: Option<i64>,
}

#[derive(Clone, Debug)]
pub struct HistoryRow {
    pub id: i64,
    pub sql: String,
    pub server: String,
    pub database: Option<String>,
    pub started: chrono::DateTime<chrono::Utc>,
    pub duration_ms: Option<i64>,
    pub rows: Option<i64>,
    pub status: String,
    pub error: Option<String>,
    pub starred: bool,
    pub profile_id: Option<ProfileId>,
}

/// One modal at a time.
#[derive(Default)]
pub enum Dialog {
    #[default]
    None,
    Connection(Box<ConnectionDialog>),
    Password { profile: ConnectionProfile, password: String, remember: bool, purpose: ConnectPurpose, error: Option<String> },
    AuthWaiting { profile: ConnectionProfile, purpose: ConnectPurpose, message: String, device: Arc<parking_lot::Mutex<Option<(String, String)>>>, cancel: Arc<std::sync::atomic::AtomicBool> },
    Group { group: ServerGroup, is_new: bool },
    ConfirmClose { tab_index: usize },
    ConfirmDeleteProfile { profile: ProfileId },
    ConfirmDeleteGroup { group: GroupId },
    ConfirmWrite { tab_index: usize, statement_preview: String, script: String, opts: ExecOptions, start_line: u32 },
    Export(Box<ExportDialog>),
    ChangeConnection { tab_index: usize },
    ExecOptions { tab_index: usize, opts: ExecOptions },
    Rename { tab_index: usize, title: String },
    Error { title: String, message: String },
    AdsImport { path: String, summary: Option<String>, error: Option<String> },
}

impl Dialog {
    pub fn is_open(&self) -> bool {
        !matches!(self, Dialog::None)
    }
}

/// Why we're resolving credentials — what to do once we have them.
#[derive(Clone, Debug)]
pub enum ConnectPurpose {
    Tab { tab: TabId, database: Option<String> },
    Tree { profile: ProfileId },
    TestOnly,
}

pub struct ConnectionDialog {
    pub profile: ConnectionProfile,
    pub is_new: bool,
    pub password: String,
    pub remember_password: bool,
    pub auth_index: usize,
    pub tenant: String,
    pub account_hint: String,
    pub sp_client_id: String,
    pub sp_secret: String,
    pub port_text: String,
    pub show_advanced: bool,
    pub error: Option<(String, Option<String>)>,
    pub testing: bool,
    pub test_result: Option<Result<String, String>>,
    pub connect_after_save: Option<ConnectPurpose>,
    pub recent: Vec<ConnectionProfile>,
    pub group_index: usize,
    pub color_index: Option<usize>,
}

pub struct ExportDialog {
    pub tab_index: usize,
    pub set_index: usize,
    pub format_index: usize,
    pub path: String,
    pub selection_only: bool,
    pub delta_mode: usize,
    pub delta_partition: String,
    pub csv_delimiter: String,
    pub csv_headers: bool,
    pub json_lines: bool,
    pub running: bool,
    pub progress: Option<(usize, usize)>,
    pub result: Option<Result<String, String>>,
    pub cancel: Arc<std::sync::atomic::AtomicBool>,
}

impl AppState {
    pub fn new() -> Self {
        Self {
            library: Library::default(),
            tabs: Vec::new(),
            active_tab: None,
            next_untitled: 1,
            sidebar_visible: true,
            sidebar_view: SidebarView::Servers,
            sidebar_width: 280.0,
            pending_meta: HashMap::new(),
            formatter: CellFormatter::default(),
            dialog: Dialog::None,
            palette_open: false,
            palette_query: String::new(),
            palette_selected: 0,
            history: HistoryView::default(),
            settings_open: false,
            about_open: false,
            shortcuts_open: false,
            ui_zoom: 1.0,
            focus: Focus::Editor,
            status_flash: None,
            last_hot_exit_save: Instant::now(),
            recently_closed_count: 0,
            export_progress: None,
            settings_patch: Vec::new(),
            settings_draft: None,
            theme_override: None,
        }
    }

    pub fn active(&self) -> Option<&EditorTab> {
        self.active_tab.and_then(|i| self.tabs.get(i))
    }
    pub fn active_mut(&mut self) -> Option<&mut EditorTab> {
        self.active_tab.and_then(move |i| self.tabs.get_mut(i))
    }
    pub fn tab_index(&self, id: TabId) -> Option<usize> {
        self.tabs.iter().position(|t| t.id == id)
    }
    pub fn tab_mut(&mut self, id: TabId) -> Option<&mut EditorTab> {
        self.tabs.iter_mut().find(|t| t.id == id)
    }

    pub fn new_tab(&mut self) -> usize {
        let tab = EditorTab::new(self.next_untitled);
        self.next_untitled += 1;
        self.tabs.push(tab);
        self.active_tab = Some(self.tabs.len() - 1);
        self.tabs.len() - 1
    }

    pub fn flash(&mut self, msg: impl Into<String>) {
        self.status_flash = Some((msg.into(), Instant::now()));
    }

    /// Apply one session event to the state. Returns things the app layer must do afterwards
    /// (history bookkeeping etc.).
    pub fn apply_event(&mut self, ev: Event) -> Vec<Followup> {
        let mut out = Vec::new();
        match ev {
            Event::Connected { tab, engine, spid, database } => {
                if let Some(t) = self.tab_mut(tab) {
                    t.conn = ConnState::Connected { engine, spid, database: database.clone() };
                    out.push(Followup::LoadTabDatabases(tab));
                    out.push(Followup::LoadCatalog(tab, database));
                }
            }
            Event::ConnectFailed { tab, error, hint } => {
                if let Some(t) = self.tab_mut(tab) {
                    t.conn = ConnState::Failed { error: error.clone(), hint: hint.clone() };
                    out.push(Followup::Toast(ToastKind::Error, format!("Connection failed: {error}{}", hint.map(|h| format!("\n{h}")).unwrap_or_default())));
                }
            }
            Event::Disconnected { tab } => {
                if let Some(t) = self.tab_mut(tab) {
                    t.conn = ConnState::Disconnected;
                    if let Some(r) = &mut t.run {
                        if r.is_live() {
                            r.state = RunViewState::Failed;
                            r.messages.push(MessageLine { text: "Connection closed.".into(), is_error: true, is_batch_header: false, line: None, at: Instant::now() });
                        }
                    }
                }
            }
            Event::NotConnected { tab } => {
                if let Some(t) = self.tab_mut(tab) {
                    if let Some(r) = &mut t.run {
                        r.state = RunViewState::Failed;
                        r.messages.push(MessageLine { text: "Not connected.".into(), is_error: true, is_batch_header: false, line: None, at: Instant::now() });
                    }
                    t.conn = ConnState::Disconnected;
                }
            }
            Event::RunStarted { tab, run, batches } => {
                if let Some(t) = self.tab_mut(tab) {
                    if let Some(r) = t.run.as_mut().filter(|r| r.id == run) {
                        r.batches = batches;
                    }
                }
            }
            Event::BatchStarted { tab, run, batch, start_line } => {
                if let Some(r) = self.run_mut(tab, run) {
                    r.current_batch = batch;
                    if r.batches > 1 || batch > 0 {
                        r.messages.push(MessageLine { text: format!("Started executing batch {} at line {}", batch + 1, start_line), is_error: false, is_batch_header: true, line: Some(start_line), at: Instant::now() });
                    }
                }
            }
            Event::ResultSetStarted { tab, run, rs } => {
                if let Some(r) = self.run_mut(tab, run) {
                    let is_plan = cobalt_driver::is_showplan_result(&rs.columns);
                    r.result_sets.push(ResultSetView { rs, grid: GridState::default(), is_plan });
                }
            }
            Event::ResultSetDone { tab, run, index, rows } => {
                if let Some(r) = self.run_mut(tab, run) {
                    if let Some(v) = r.result_sets.get(index) {
                        if v.is_plan {
                            // one row, one column: the plan XML
                            if v.rs.row_count() > 0 {
                                if let cobalt_results::CellValue::Text(xml) = v.rs.cell_value_global(0, 0) {
                                    let mut pv = PlanView::new(xml);
                                    pv.statement_index = r.plans.len();
                                    r.plans.push(pv);
                                }
                            }
                        } else {
                            r.messages.push(MessageLine { text: format!("({} row{} returned)", fmt_count(rows), if rows == 1 { "" } else { "s" }), is_error: false, is_batch_header: false, line: None, at: Instant::now() });
                        }
                    }
                    if r.paused_set == Some(index) {
                        r.paused_set = None;
                        if r.state == RunViewState::Paused {
                            r.state = RunViewState::Running;
                        }
                    }
                }
            }
            Event::Paused { tab, run, index, rows: _ } => {
                if let Some(r) = self.run_mut(tab, run) {
                    r.paused_set = Some(index);
                    r.state = RunViewState::Paused;
                }
            }
            Event::Message { tab, run, message, batch: _, batch_start_line } => {
                if let Some(r) = self.run_mut(tab, run) {
                    let line = if message.line > 0 { Some(batch_start_line + message.line - 1) } else { None };
                    let mut text = String::new();
                    if let Some(h) = message.headline() {
                        text.push_str(&h);
                        text.push('\n');
                    }
                    text.push_str(&message.message);
                    r.messages.push(MessageLine { text, is_error: message.is_error, is_batch_header: false, line, at: Instant::now() });
                }
            }
            Event::RowsAffected { tab, run, rows } => {
                if let Some(r) = self.run_mut(tab, run) {
                    r.rows_affected.push(rows);
                    r.messages.push(MessageLine { text: format!("({} row{} affected)", fmt_count(rows), if rows == 1 { "" } else { "s" }), is_error: false, is_batch_header: false, line: None, at: Instant::now() });
                }
            }
            Event::BatchDone { tab, run, batch: _, error: _, elapsed: _ } => {
                let _ = self.run_mut(tab, run);
            }
            Event::RunDone { tab, run, cancelled, failed, elapsed, total_rows } => {
                if let Some(r) = self.run_mut(tab, run) {
                    r.elapsed = elapsed;
                    r.total_rows = total_rows;
                    r.paused_set = None;
                    r.state = if cancelled {
                        RunViewState::Cancelled
                    } else if failed {
                        RunViewState::Failed
                    } else {
                        RunViewState::Done
                    };
                    let text = if cancelled {
                        "Query was cancelled by user.".to_string()
                    } else {
                        format!("Total execution time: {}", fmt_duration(elapsed))
                    };
                    r.messages.push(MessageLine { text, is_error: false, is_batch_header: false, line: None, at: Instant::now() });
                    let history_id = r.history_id;
                    let has_error = r.has_error();
                    out.push(Followup::FinishHistory { history_id, elapsed, rows: total_rows, cancelled, failed: failed || has_error, error: r.messages.iter().find(|m| m.is_error).map(|m| m.text.clone()) });
                    if has_error && !cancelled {
                        if let Some(t) = self.tab_mut(tab) {
                            t.results_tab = ResultsTab::Messages;
                        }
                    }
                }
            }
            Event::DatabaseChanged { tab, database } => {
                if let Some(t) = self.tab_mut(tab) {
                    if let ConnState::Connected { database: d, .. } = &mut t.conn {
                        *d = database.clone();
                    }
                    out.push(Followup::LoadCatalog(tab, database));
                }
            }
            Event::DatabaseChangeFailed { tab: _, error } => out.push(Followup::Toast(ToastKind::Error, error)),
            Event::Pong { tab, ok } => {
                if !ok {
                    if let Some(t) = self.tab_mut(tab) {
                        t.conn = ConnState::Disconnected;
                    }
                }
            }
            Event::Metadata { req, profile, result } => {
                if let Some(pending) = self.pending_meta.remove(&req) {
                    out.extend(self.apply_metadata(pending, profile, result));
                }
            }
        }
        out
    }

    fn run_mut(&mut self, tab: TabId, run: RunId) -> Option<&mut RunView> {
        self.tab_mut(tab).and_then(|t| t.run.as_mut()).filter(|r| r.id == run)
    }

    fn apply_metadata(&mut self, pending: PendingMeta, profile: ProfileId, result: Result<crate::session::MetadataResponse, String>) -> Vec<Followup> {
        use crate::session::MetadataResponse as R;
        let mut out = Vec::new();
        match pending.purpose {
            MetaPurpose::Tree => {
                let node = self.library.server(profile);
                match (pending.kind, result) {
                    (MetadataRequest::Probe, Ok(R::Probe(e))) => node.engine = Some(e),
                    (MetadataRequest::ListDatabases, Ok(R::Databases(d))) => node.databases = Loadable::Loaded(d),
                    (MetadataRequest::ListDatabases, Err(e)) => {
                        node.databases = Loadable::Failed(e.clone());
                        out.push(Followup::Toast(ToastKind::Error, e));
                    }
                    (MetadataRequest::ListObjects { database }, Ok(R::Objects(o))) => node.db_nodes.entry(database).or_default().objects = Loadable::Loaded(o),
                    (MetadataRequest::ListObjects { database }, Err(e)) => {
                        node.db_nodes.entry(database).or_default().objects = Loadable::Failed(e.clone());
                        out.push(Followup::Toast(ToastKind::Error, e));
                    }
                    (MetadataRequest::ListColumns { obj }, res) => {
                        let db = node.db_nodes.entry(obj.database.clone()).or_default();
                        db.columns.insert(obj.object_id.unwrap_or(0), match res { Ok(R::Columns(c)) => Loadable::Loaded(c), Err(e) => Loadable::Failed(e), _ => Loadable::Failed("unexpected".into()) });
                    }
                    (MetadataRequest::ListParameters { obj }, res) => {
                        let db = node.db_nodes.entry(obj.database.clone()).or_default();
                        db.parameters.insert(obj.object_id.unwrap_or(0), match res { Ok(R::Parameters(c)) => Loadable::Loaded(c), Err(e) => Loadable::Failed(e), _ => Loadable::Failed("unexpected".into()) });
                    }
                    (MetadataRequest::ListIndexes { obj }, res) => {
                        let db = node.db_nodes.entry(obj.database.clone()).or_default();
                        db.indexes.insert(obj.object_id.unwrap_or(0), match res { Ok(R::Indexes(c)) => Loadable::Loaded(c), Err(e) => Loadable::Failed(e), _ => Loadable::Failed("unexpected".into()) });
                    }
                    (MetadataRequest::ListKeys { obj }, res) => {
                        let db = node.db_nodes.entry(obj.database.clone()).or_default();
                        db.keys.insert(obj.object_id.unwrap_or(0), match res { Ok(R::Keys(c)) => Loadable::Loaded(c), Err(e) => Loadable::Failed(e), _ => Loadable::Failed("unexpected".into()) });
                    }
                    (_, Err(e)) => out.push(Followup::Toast(ToastKind::Error, e)),
                    _ => {}
                }
            }
            MetaPurpose::Catalog { tab, database } => match result {
                Ok(R::Catalog(c)) => {
                    let cat = Arc::new(c);
                    if let Some(t) = self.tab_mut(tab) {
                        t.catalog = Some(cat.clone());
                        t.catalog_database = Some(database.clone());
                    }
                    self.library.server(profile).db_nodes.entry(database.clone()).or_default().catalog = Loadable::Loaded((*cat).clone());
                    out.push(Followup::CacheCatalog { profile, database, catalog: cat });
                }
                Err(e) => tracing::warn!(error = %e, "catalog load failed"),
                _ => {}
            },
            MetaPurpose::ScriptToTab { title, profile: pid, database, run } => match result {
                Ok(R::Script(sql)) => out.push(Followup::OpenScriptTab { title, sql, profile: pid, database, run }),
                Err(e) => out.push(Followup::Toast(ToastKind::Error, e)),
                _ => {}
            },
            MetaPurpose::TabDatabases { tab } => {
                if let Some(t) = self.tab_mut(tab) {
                    t.databases = match result {
                        Ok(R::Databases(d)) => Loadable::Loaded(d),
                        Err(e) => Loadable::Failed(e),
                        _ => Loadable::Failed("unexpected".into()),
                    };
                }
            }
        }
        out
    }
}

/// Side effects the app performs after applying events (they need the store/session).
#[derive(Debug)]
pub enum Followup {
    LoadTabDatabases(TabId),
    LoadCatalog(TabId, String),
    FinishHistory { history_id: Option<i64>, elapsed: Duration, rows: u64, cancelled: bool, failed: bool, error: Option<String> },
    CacheCatalog { profile: ProfileId, database: String, catalog: Arc<DatabaseCatalog> },
    OpenScriptTab { title: String, sql: String, profile: ProfileId, database: String, run: bool },
    Toast(ToastKind, String),
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ToastKind {
    Info,
    Success,
    Warning,
    Error,
}

pub fn fmt_count(n: u64) -> String {
    let s = n.to_string();
    let mut out = String::with_capacity(s.len() + s.len() / 3);
    for (i, ch) in s.chars().enumerate() {
        if i > 0 && (s.len() - i) % 3 == 0 {
            out.push(',');
        }
        out.push(ch);
    }
    out
}

pub fn fmt_duration(d: Duration) -> String {
    let ms = d.as_millis();
    let h = ms / 3_600_000;
    let m = (ms / 60_000) % 60;
    let s = (ms / 1000) % 60;
    let frac = ms % 1000;
    format!("{h:02}:{m:02}:{s:02}.{frac:03}")
}
