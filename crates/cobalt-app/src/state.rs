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
    /// Profiles synthesised by the Fabric explorer (not in the store, not in the tree).
    pub ephemeral: Vec<ConnectionProfile>,
}

impl Library {
    pub fn group(&self, id: GroupId) -> Option<&ServerGroup> {
        self.groups.iter().find(|g| g.id == id)
    }
    pub fn profile(&self, id: ProfileId) -> Option<&ConnectionProfile> {
        self.profiles.iter().chain(self.ephemeral.iter()).find(|p| p.id == id)
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
    /// Nest a folder's objects under one row per schema.
    pub group_by_schema: bool,
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
    /// Row count / size per table, loaded for the Describe hover.
    pub stats: HashMap<i32, Loadable<TableStats>>,
    pub expanded_subfolders: HashSet<(i32, SubFolder)>,
    pub catalog: Loadable<DatabaseCatalog>,
    pub filter: String,
    /// Per-folder name filters (Tables, Views, …) and which folders show their filter box.
    pub folder_filters: HashMap<Folder, String>,
    pub filter_open: HashSet<Folder>,
    /// Expanded schema rows when grouping by schema: (folder, schema).
    pub expanded_schemas: HashSet<(Folder, String)>,
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
#[derive(Default)]
pub enum Loadable<T> {
    #[default]
    NotLoaded,
    Loading,
    Loaded(T),
    Failed(String),
}


impl<T> Loadable<T> {
    pub fn get(&self) -> Option<&T> {
        match self {
            Loadable::Loaded(t) => Some(t),
            _ => None,
        }
    }
    pub fn is_loading(&self) -> bool {
        matches!(self, Loadable::Loading)
    }
    pub fn needs_load(&self) -> bool {
        matches!(self, Loadable::NotLoaded)
    }
}

/// What a pending metadata request will fill in when it completes.
/// What a scratch table is made from.
#[derive(Clone, Debug, PartialEq)]
pub enum ScratchFrom {
    /// A tab's result set.
    Result { tab_index: usize, set: usize },
    /// Any local table, by name.
    Table(String),
}

#[derive(Debug, Clone)]
pub struct PendingMeta {
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
    /// Columns of the target table for Import Data (existing-table mode).
    ImportColumns,
    /// Databases list for a tab's dropdown.
    TabDatabases { tab: TabId },
    /// Columns of the table an Explore tab is over (its field list).
    ExploreColumns { tab: TabId },
    /// Distinct values of a column for an Explore tab's filter picker.
    ExploreDistinct { tab: TabId, table: String, column: String },
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

    /// Engine capabilities of a connected tab; `None` while disconnected.
    pub fn capabilities(&self) -> Option<&cobalt_core::Capabilities> {
        match self {
            ConnState::Connected { engine, .. } => Some(&engine.capabilities),
            _ => None,
        }
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
    /// A preview tab (Files sidebar single click): reused by the next preview, kept once it
    /// is edited, run, pinned, renamed or dragged.
    pub preview: bool,
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
    /// Results beside the editor (`Some`) or the Settings default (`None`).
    pub results_side: Option<bool>,
    pub results_tab: ResultsTab,
    pub editor: EditorState,
    pub catalog: Option<Arc<DatabaseCatalog>>,
    pub catalog_database: Option<String>,
    pub untitled_index: usize,
    /// Run again once the (re)connect completes.
    pub pending_run: Option<RunMode>,
    /// The database the tab was last connected to; a reconnect after a dropped session goes back
    /// there rather than to the profile's default.
    pub last_database: Option<String>,
    /// How the last run was started, so a run that found its connection dead can be repeated once
    /// the tab is connected again.
    pub last_run_mode: Option<RunMode>,
    /// Run-to-export target consumed by the next `execute` (set by the Run to File dialog).
    pub pending_export: Option<Box<crate::ops::ExportJob>>,
    /// Open the Import Data dialog once this tab's connection is up (tree: "Import data from file…").
    pub pending_import: bool,
    /// Run this text instead of the editor's on the next run (Edit Data's save script, then the query again).
    pub script_override: Option<String>,
    /// An Edit Data save is running: the results it came from, to restore on failure.
    pub edit_resume: Option<EditResume>,
    /// Turn Edit Data on for this result set when the next run completes.
    pub edit_reopen: Option<usize>,
    pub snapshot_hash: u64,
    /// `Some` = this tab is a notebook: cells instead of `text`, per-cell runs instead of `run`.
    pub notebook: Option<Box<NotebookState>>,
    /// The Fabric item this notebook came from; Save writes back to it.
    pub fabric_item: Option<FabricItemRef>,
    /// `Some` = a Spark SQL query tab: statements run on the local Spark session in the tab's
    /// own context instead of over a connection (`sparkq.rs`).
    pub spark: Option<SparkTab>,
    /// A dashboard page: tiles with their own hidden tabs (connections) and runs.
    pub dashboard: Option<Box<DashboardState>>,
    /// `Some` = a DAX query tab on a semantic model: runs go through the Arrow query API
    /// (`tabularq.rs`, `session/tabular.rs`) instead of a connection.
    pub tabular: Option<TabularTab>,
    /// A Local query tab: SQL over the local tables through the in-process engine.
    pub local: Option<crate::localq::LocalTab>,
    /// The model's catalog for DAX completion (a copy of `AppState::model_catalogs[key]`).
    pub model_catalog: Option<Arc<cobalt_core::tabular::ModelCatalog>>,
    /// The model's DAX function list for completion (`INFO.FUNCTIONS()`).
    pub dax_functions: Option<Arc<Vec<cobalt_sql::dax::DaxFunctionDoc>>>,
    /// `Some` = an Explore tab: the pivot builder generates the DAX this tab runs (`exploreq.rs`);
    /// `tabular` carries the model, `text` the generated query.
    pub explore: Option<Box<ExploreState>>,
}

/// The Explore builder's state on top of a DAX tab.
pub struct ExploreState {
    pub spec: cobalt_core::explore::ExploreSpec,
    /// The last generated query (`None` until the catalog is loaded or the spec is empty).
    pub dax: Option<cobalt_tabular::explore::DaxQuery>,
    pub dax_error: Option<String>,
    pub show_dax: bool,
    pub field_filter: String,
    /// Tables open in the field list.
    pub field_expanded: std::collections::HashSet<String>,
    /// Distinct values per (table, column) for the "is one of" filter picker.
    pub distinct: std::collections::HashMap<(String, String), Loadable<Vec<String>>>,
    /// The filter being edited in the popup.
    pub filter_editor: Option<FilterEditor>,
    /// The matrix built from the first result set (when the Columns well is used).
    pub pivot: Option<Arc<cobalt_results::pivot::PivotTable>>,
    pub pivot_built_for: Option<(u64, usize)>,
    /// Show the matrix (otherwise the flat grid).
    pub matrix: bool,
    /// The spec changed since the last run.
    pub dirty: bool,
    /// Run again as soon as the spec changes (and the catalog is loaded).
    pub auto_run: bool,
    pub saved_hash: u64,
    /// Run once the catalog arrives.
    pub run_when_loaded: bool,
    /// When the columns of a SQL table were last requested (retried while the tab connects).
    pub catalog_asked: Option<std::time::Instant>,
}

impl ExploreState {
    pub fn new(spec: cobalt_core::explore::ExploreSpec) -> Self {
        let saved_hash = hash_text(&spec.to_json());
        Self { spec, dax: None, dax_error: None, show_dax: false, field_filter: String::new(), field_expanded: Default::default(), distinct: Default::default(), filter_editor: None, pivot: None, pivot_built_for: None, matrix: true, dirty: true, auto_run: true, saved_hash, run_when_loaded: false, catalog_asked: None }
    }
    pub fn spec_hash(&self) -> u64 {
        hash_text(&self.spec.to_json())
    }
}

/// The Servers tree's Analysis Services root: saved endpoints, their catalogs (models), and
/// what is open.
#[derive(Default)]
pub struct TabularRoot {
    pub profiles: Vec<cobalt_core::tabular::TabularProfile>,
    pub loaded: bool,
    pub expanded: bool,
    pub expanded_profiles: std::collections::HashSet<uuid::Uuid>,
    /// Models (catalogs) per profile.
    pub catalogs: std::collections::HashMap<uuid::Uuid, Loadable<Vec<String>>>,
    /// Model rows whose explorer is open (`(profile, model)`).
    pub expanded_models: std::collections::HashSet<(uuid::Uuid, String)>,
}

impl TabularRoot {
    pub fn profile(&self, id: uuid::Uuid) -> Option<&cobalt_core::tabular::TabularProfile> {
        self.profiles.iter().find(|p| p.id == id)
    }
}

/// The filter popup's draft.
#[derive(Clone, Debug)]
pub struct FilterEditor {
    pub table: String,
    pub column: String,
    /// Index into `spec.filters` when editing an existing filter.
    pub index: Option<usize>,
    pub op: cobalt_core::explore::FilterOp,
    pub search: String,
    pub text_a: String,
    pub text_b: String,
    pub last_n: u32,
}

/// A DAX query tab's state: the model it queries and the options of its runs.
pub struct TabularTab {
    pub target: cobalt_core::tabular::TabularTarget,
    /// The saved Analysis Services profile the target came from (basic credentials, display).
    pub profile: Option<uuid::Uuid>,
    /// Power BI targets: use the XMLA transport instead of the Arrow query API (MDX, DMVs).
    pub force_xmla: bool,
    /// The transport the last run used ("Arrow query API" / "XMLA"), for the status bar.
    pub transport: Option<&'static str>,
    /// The row limit the last run was sent with (the capped banner compares against it).
    pub last_limit: Option<u64>,
    /// The next run lifts the row cap ("Run again without the cap").
    pub uncapped_next: bool,
    /// View as: impersonation for RLS testing (workspace admins).
    pub effective_username: Option<String>,
    pub roles: Vec<String>,
}

impl TabularTab {
    pub fn new(target: cobalt_core::tabular::TabularTarget) -> Self {
        Self { target, profile: None, force_xmla: false, transport: None, last_limit: None, uncapped_next: false, effective_username: None, roles: Vec::new() }
    }
    /// Whether a run of `text` goes over XMLA: always for Azure AS, HTTP endpoints and saved
    /// Power BI endpoints (`force_xmla`); for Fabric models per Settings › Semantic models
    /// (`auto` = XMLA only for MDX and DMV text).
    pub fn transport_is_xmla(&self, settings: &cobalt_core::Settings, text: &str) -> bool {
        if self.force_xmla || !matches!(self.target, cobalt_core::tabular::TabularTarget::PowerBi { .. }) {
            return true;
        }
        match settings.semantic.transport.as_str() {
            "xmla" => true,
            "arrow" => false,
            _ => cobalt_tabular::is_mdx_or_dmv(text),
        }
    }
    /// MDX and DMV text can run on this tab (the language label says "DAX / MDX").
    pub fn may_use_xmla(&self, settings: &cobalt_core::Settings) -> bool {
        self.force_xmla || !matches!(self.target, cobalt_core::tabular::TabularTarget::PowerBi { .. }) || settings.semantic.transport != "arrow"
    }
    /// The transport label before the first run.
    pub fn transport_label(&self, settings: &cobalt_core::Settings) -> &'static str {
        if let Some(t) = self.transport {
            return t;
        }
        if self.transport_is_xmla(settings, "") { "XMLA" } else if settings.semantic.transport == "auto" { "Arrow query API · auto" } else { "Arrow query API" }
    }
    pub fn model_key(&self) -> String {
        self.target.key()
    }
}

/// A Spark SQL query tab's state: its lakehouse binding (like a notebook's), the display names
/// (kept so a restored tab reads right before the Fabric lists load) and, during Run to File,
/// the export sink the collected result sets go to.
/// A dashboard tab: the page's settings and its tiles. Each tile owns a hidden `EditorTab`
/// with its own connection and run, so connect, run and the session events serve it unchanged.
pub struct DashboardState {
    pub file: cobalt_core::DashboardFile,
    pub tiles: Vec<Tile>,
    pub dirty: bool,
    /// The server whose home page this is (`pref:dashboard:<profile>` remembers the file).
    pub home_for: Option<ProfileId>,
}

pub struct Tile {
    pub spec: cobalt_core::TileSpec,
    pub tab: EditorTab,
    pub last_refresh: Option<Instant>,
    /// A refresh is wanted as soon as the tile's connection is up.
    pub pending: bool,
    /// The chart strip is open on the tile.
    pub configure: bool,
    /// Why the tile cannot run (no matching connection profile…).
    pub error: Option<String>,
}

impl Tile {
    /// Seconds since the last refresh.
    pub fn age_secs(&self) -> Option<u64> {
        self.last_refresh.map(|t| t.elapsed().as_secs())
    }
    pub fn is_live(&self) -> bool {
        self.tab.run.as_ref().map(|r| r.is_live()).unwrap_or(false)
    }
}

pub struct SparkTab {
    pub binding: Option<NotebookFabric>,
    pub workspace_name: String,
    pub lakehouse_name: Option<String>,
    pub sink: Option<crate::session::RunSink>,
    /// The streamed run in progress (`run_sql` per statement, local-spark-mcp 0.7.0).
    pub sql: Option<SparkSqlRun>,
    /// The next run is a Parse (set by the Parse command around `run`).
    pub parse_next: bool,
    /// The next run lifts the row cap ("Run again without the cap").
    pub uncapped_next: bool,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum SparkSqlMode {
    Run,
    /// `EXPLAIN EXTENDED <statement>`: the plan text goes to the Plan tab, not the grid.
    Plan,
    /// `EXPLAIN <statement>` per statement: only the outcome is reported (Parse).
    Parse,
}

/// Bookkeeping of a Spark tab's streamed run: how many statements, which result set each
/// statement's rows went to, and whether rows go to an export sink (Run to File) rather than
/// beyond the grid's preview.
pub struct SparkSqlRun {
    /// What the run is for: rows, an `EXPLAIN` plan, or a parse check.
    pub mode: SparkSqlMode,
    pub statements: usize,
    pub limit: Option<u64>,
    pub sets: std::collections::HashMap<usize, usize>,
    pub export: bool,
    /// The statement whose set is open on the export sink (SetStart sent, SetEnd pending).
    pub sink_open: Option<usize>,
    /// Plan / Parse: the `EXPLAIN` text per statement (Spark answers an analysis error as text).
    pub texts: std::collections::HashMap<usize, String>,
    /// A Plan / Parse statement whose `EXPLAIN` text was an error.
    pub failed: bool,
}

/// A notebook tab: the document plus per-cell UI state. `nb.cells[i]` and `cells[i]` stay
/// parallel (same length, same order; `CellState.id == Cell.id`).
/// Where a notebook's code cells run.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum NotebookKernel {
    /// SQL cells through the tab's connection (Python cells cannot run).
    Connection,
    /// Every code cell on the local Spark worker: Python via `run_code`, SQL via `spark.sql`.
    Spark,
}

/// A notebook's Fabric binding: which workspace's lakehouses the Spark session should see.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct NotebookFabric {
    pub workspace_id: String,
    pub lakehouse_id: Option<String>,
    /// `sandbox` (default), `readonly` or `writethrough`.
    pub write_mode: String,
    /// Clone the default lakehouse's tables in the background when the session starts.
    pub preload: bool,
}

/// A notebook opened from (and saved back to) a Fabric workspace item.
#[derive(Clone, Debug)]
pub struct FabricItemRef {
    pub item: cobalt_fabric::FabricItem,
    /// The `.platform` definition part, written back unchanged on save.
    pub platform: Option<cobalt_fabric::DefinitionPart>,
    pub saving: bool,
}

/// The Shadows window (sandbox clones of OneLake tables in the running session).
#[derive(Default)]
pub struct ShadowsUi {
    pub open: bool,
    pub status: Option<serde_json::Value>,
    /// The session's `preload_status` reply.
    pub preload: Option<serde_json::Value>,
    pub loading: bool,
    pub error: Option<String>,
    pub note: Option<String>,
}

/// The changelog window (Help → What's new): the copy bundled with this build at once, the
/// latest from GitHub when the fetch comes back.
#[derive(Default)]
pub struct ChangelogUi {
    pub open: bool,
    /// The GitHub copy (`Ok`) or why it could not be fetched (`Err`).
    pub fetched: Option<Result<String, String>>,
    /// A fetch in flight; the task fills the slot and asks for a repaint.
    pub pending: Option<std::sync::Arc<parking_lot::Mutex<Option<Result<String, String>>>>>,
    pub cache: egui_commonmark::CommonMarkCache,
}

/// A notebook tab that is still being fetched (from Fabric).
#[derive(Clone, Debug)]
pub struct NotebookLoading {
    pub item_id: String,
    pub copy: bool,
    pub message: String,
    pub failed: bool,
}

/// A queued run: a cell, optionally only a selected span of it (Ctrl+Shift+Enter).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct QueuedCell {
    pub id: String,
    pub text: Option<String>,
}

pub struct NotebookState {
    pub nb: cobalt_notebook::Notebook,
    pub cells: Vec<CellState>,
    pub kernel: NotebookKernel,
    pub fabric: Option<NotebookFabric>,
    /// Set while the document is on its way; the tab shows a spinner instead of cells.
    pub loading: Option<NotebookLoading>,
    /// The current cell (keyboard target, highlighted).
    pub selected: usize,
    /// Cells waiting to run, in order; one runs at a time per tab.
    pub queue: std::collections::VecDeque<QueuedCell>,
    /// Set by edits, runs and structure changes; cleared by save.
    pub dirty: bool,
    /// Next `execution_count`.
    pub counter: u64,
    /// Warnings from the Git `.py` reader, shown once.
    pub warnings: Vec<String>,
    pub md_cache: egui_commonmark::CommonMarkCache,
    /// The last deleted cell (Z restores it).
    pub undo_delete: Option<(usize, cobalt_notebook::Cell)>,
}

pub struct CellState {
    pub id: String,
    pub editor: EditorState,
    /// The cell's SQL run (live, or rebuilt from the file's cached outputs).
    pub run: Option<RunView>,
    /// Outputs from the file that are not tabular (text, errors, images), shown under the grids.
    pub extra_outputs: Vec<cobalt_notebook::Output>,
    /// `run` came from the file, not from this session.
    pub cached: bool,
    pub md_editing: bool,
    pub outputs_collapsed: bool,
    /// `%pip install` packages the last run of this cell carried: reported, not installed from
    /// the notebook; "Add to runtime" puts them in the runtime's Python packages.
    pub pip_packages: Vec<String>,
}

impl CellState {
    pub fn new(id: String) -> Self {
        Self { id, editor: EditorState::default(), run: None, extra_outputs: Vec::new(), cached: false, md_editing: false, outputs_collapsed: false, pip_packages: Vec::new() }
    }
}

impl NotebookState {
    pub fn new(nb: cobalt_notebook::Notebook) -> Self {
        let cells = nb.cells.iter().map(|c| CellState::new(c.id.clone())).collect();
        let counter = nb.cells.iter().filter_map(|c| c.execution_count).max().unwrap_or(0);
        let kernel = if nb.default_language() == cobalt_notebook::CellLanguage::Python { NotebookKernel::Spark } else { NotebookKernel::Connection };
        let fabric = nb.default_lakehouse().and_then(|lh| lh.workspace_id.map(|ws| NotebookFabric { workspace_id: ws, lakehouse_id: Some(lh.id), write_mode: "sandbox".into(), preload: false }));
        Self { nb, cells, kernel, fabric, loading: None, selected: 0, queue: Default::default(), dirty: false, counter, warnings: Vec::new(), md_cache: Default::default(), undo_delete: None }
    }
    pub fn is_running(&self) -> bool {
        !self.queue.is_empty() || self.cells.iter().any(|c| c.run.as_ref().map(|r| r.is_live()).unwrap_or(false))
    }
    pub fn running_cell(&self) -> Option<usize> {
        self.cells.iter().position(|c| c.run.as_ref().map(|r| r.is_live()).unwrap_or(false))
    }
    pub fn cell_index(&self, id: &str) -> Option<usize> {
        self.cells.iter().position(|c| c.id == id)
    }
    pub fn is_queued(&self, id: &str) -> bool {
        self.queue.iter().any(|q| q.id == id)
    }
    /// Take a cell out of the queue (a run that has not started yet).
    pub fn dequeue(&mut self, id: &str) {
        self.queue.retain(|q| q.id != id);
    }
    pub fn language_of(&self, i: usize) -> cobalt_notebook::CellLanguage {
        self.nb.cell_language(&self.nb.cells[i])
    }
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
            preview: false,
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
            results_side: None,
            results_tab: ResultsTab::Results,
            editor: EditorState::default(),
            catalog: None,
            catalog_database: None,
            untitled_index,
            pending_run: None,
            last_database: None,
            last_run_mode: None,
            pending_export: None,
            pending_import: false,
            script_override: None,
            edit_resume: None,
            edit_reopen: None,
            snapshot_hash: hash_text(""),
            notebook: None,
            fabric_item: None,
            spark: None,
            dashboard: None,
            tabular: None,
            local: None,
            model_catalog: None,
            dax_functions: None,
            explore: None,
        }
    }

    pub fn is_notebook(&self) -> bool {
        self.notebook.is_some()
    }

    /// The editor a command should act on: the selected notebook cell, or the query editor.
    pub fn active_host(&mut self) -> crate::ui::editor::EditorHost<'_> {
        let tab_id = self.id;
        let EditorTab { notebook, catalog, databases, text, editor, spark, tabular, model_catalog, dax_functions, local, profile, .. } = self;
        let tab_syntax = if spark.is_some() {
            crate::ui::editor::Syntax::SparkSql
        } else if local.is_some() {
            crate::ui::editor::Syntax::Local
        } else if tabular.is_some() {
            crate::ui::editor::Syntax::Dax
        } else if profile.as_ref().map(|p| p.engine == cobalt_core::EngineFamily::Postgres).unwrap_or(false) {
            crate::ui::editor::Syntax::Postgres
        } else if profile.as_ref().map(|p| p.engine == cobalt_core::EngineFamily::DuckDb).unwrap_or(false) {
            crate::ui::editor::Syntax::DuckDb
        } else if profile.as_ref().map(|p| p.is_file()).unwrap_or(false) {
            crate::ui::editor::Syntax::Sqlite
        } else {
            crate::ui::editor::Syntax::Sql
        };
        match notebook.as_deref_mut() {
            Some(nb) if !nb.cells.is_empty() => {
                let i = nb.selected.min(nb.cells.len() - 1);
                let cell = &mut nb.cells[i];
                let syntax = match (nb.nb.cells[i].kind, nb.nb.cell_language(&nb.nb.cells[i])) {
                    (cobalt_notebook::CellKind::Code, cobalt_notebook::CellLanguage::Sql) => crate::ui::editor::Syntax::Sql,
                    (cobalt_notebook::CellKind::Code, cobalt_notebook::CellLanguage::Python) => crate::ui::editor::Syntax::Python,
                    _ => crate::ui::editor::Syntax::Plain,
                };
                crate::ui::editor::EditorHost { id: egui::Id::unique(("cobalt-cell", tab_id, cell.id.as_str())), text: &mut nb.nb.cells[i].source, editor: &mut cell.editor, catalog: catalog.as_deref(), databases, syntax , model: None, dax_functions: &[], local_functions: &[] }
            }
            _ => crate::ui::editor::EditorHost { id: egui::Id::unique(("cobalt-editor", tab_id)), text, editor, catalog: catalog.as_deref(), databases, syntax: tab_syntax, model: model_catalog.as_deref(), dax_functions: dax_functions.as_deref().map(|v| v.as_slice()).unwrap_or(&[]), local_functions: local.as_ref().map(|l| l.functions.as_slice()).unwrap_or(&[]) },
        }
    }

    /// The database a reconnect should land in: the live one, else the one the tab last had.
    pub fn reconnect_database(&self) -> Option<String> {
        self.conn.database().map(str::to_string).or_else(|| self.last_database.clone())
    }

    pub fn is_dirty(&self) -> bool {
        if let Some(d) = self.dashboard.as_deref() {
            return d.dirty;
        }
        if self.explore.is_some() {
            return crate::exploreq::is_dirty(self);
        }
        match &self.notebook {
            Some(nb) => nb.dirty,
            None => hash_text(&self.text) != self.saved_text_hash,
        }
    }
    pub fn is_tabular(&self) -> bool {
        self.tabular.is_some()
    }
    pub fn is_explore(&self) -> bool {
        self.explore.is_some()
    }
    pub fn is_dashboard(&self) -> bool {
        self.dashboard.is_some()
    }
    pub fn mark_saved(&mut self) {
        self.saved_text_hash = hash_text(&self.text);
        if let Some(nb) = self.notebook.as_mut() {
            nb.dirty = false;
        }
    }
    pub fn is_running(&self) -> bool {
        if let Some(nb) = &self.notebook {
            return nb.is_running() || nb.loading.as_ref().map(|l| !l.failed).unwrap_or(false);
        }
        self.run.as_ref().map(|r| r.state == RunViewState::Running || r.state == RunViewState::Paused || r.state == RunViewState::Cancelling).unwrap_or(false)
    }
    pub fn display_title(&self) -> String {
        let mut t = self.title.clone();
        if self.notebook.is_some() {
            t = format!("{} {t}", egui_phosphor::regular::NOTEBOOK);
        } else if self.spark.is_some() {
            t = format!("{} {t}", egui_phosphor::regular::FIRE);
        } else if self.explore.is_some() {
            t = format!("{} {t}", egui_phosphor::regular::TABLE);
        } else if self.tabular.is_some() {
            t = format!("{} {t}", egui_phosphor::regular::CUBE);
        }
        if self.profile.as_ref().map(|p| p.read_only_guard).unwrap_or(false) {
            t = format!("{} {t}", egui_phosphor::regular::LOCK_SIMPLE);
        }
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
    /// Char index of the primary cursor as of the last frame (mirror of `cursors.primary()`).
    pub cursor: usize,
    /// The primary selection as of the last frame (sorted char range), if any.
    pub selection: Option<(usize, usize)>,
    /// The full cursor set (multi-cursor) and the undo stack; owned by the editor widget.
    pub cursors: crate::ui::editor::core::Cursors,
    pub undo: crate::ui::editor::core::UndoStack,
    /// An expanded snippet whose placeholders Tab cycles through.
    pub snippet: Option<crate::ui::editor::core::SnippetSession>,
    /// Tab stops (absolute byte ranges in the text after the pending Replace) of a snippet just accepted.
    pub pending_snippet: Option<Vec<(usize, usize)>>,
    pub scroll_to_cursor: bool,
    pub find_open: bool,
    pub find_text: String,
    pub replace_text: String,
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
    /// Run-to-export: where the rows went (the grids hold previews only).
    pub export_target: Option<String>,
    /// Per batch: (editor line the batch starts on, elapsed once done, ended in error). Drawn in
    /// the editor gutter while the editor text still matches `script_hash`.
    pub batch_times: Vec<(u32, Option<Duration>, bool)>,
    pub script_hash: u64,
    /// A Spark `EXPLAIN` plan: `(section title, text)` per `== … ==` block.
    pub text_plan: Option<Vec<(String, String)>>,
    /// A Spark result hit the row cap (a one-click uncapped re-run is offered).
    pub capped: bool,
    /// The script this run executed (Edit Data reads the table from it).
    pub script: String,
}

impl RunView {
    pub fn new(id: RunId) -> Self {
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
            export_target: None,
            batch_times: Vec::new(),
            script_hash: 0,
            text_plan: None,
            capped: false,
            script: String::new(),
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
    /// Column profile (nulls, distinct, min/max, top values, distribution), computed on demand.
    pub profile: Option<crate::ui::results::profile::ColumnProfiles>,
    /// The chart shown instead of the grid (the Chart toggle); follows the view.
    pub chart: Option<ChartView>,
    /// Edit Data: staged changes (Some while Edit Data is on).
    pub edits: Option<GridEdits>,
}

/// A result set's chart: the spec the strip edits and the data built from the current view.
pub struct ChartView {
    pub spec: cobalt_core::ChartSpec,
    pub data: Option<Arc<cobalt_results::chart::ChartData>>,
    /// The spec the data was built for (rebuilt when it or the view generation changes).
    pub built_for: Option<(cobalt_core::ChartSpec, u64)>,
    /// Milliseconds the last build took.
    pub build_ms: u64,
    /// The plot's rect on screen in the last frame (for the image capture).
    pub rect: Option<egui::Rect>,
    /// A notebook cell's chart kept as a picture for the saved outputs (Keep in notebook).
    pub png: Option<Arc<Vec<u8>>>,
}

impl ChartView {
    pub fn new(spec: cobalt_core::ChartSpec) -> Self {
        Self { spec, data: None, built_for: None, build_ms: 0, rect: None, png: None }
    }
}

/// A chart image capture in flight: the next frame is read back and the rect cropped.
#[derive(Clone, Debug)]
pub struct PendingCapture {
    pub tab: TabId,
    pub set: usize,
    pub rect: egui::Rect,
    pub purpose: CapturePurpose,
    /// The screenshot was requested; the frame after this one carries it.
    pub requested: bool,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum CapturePurpose {
    Clipboard,
    SaveFile,
    /// Into the notebook cell's chart as the picture saved with the outputs.
    Cell { cell_id: String },
}

#[derive(Clone, Debug)]
pub struct MessageLine {
    pub text: String,
    pub is_error: bool,
    pub is_batch_header: bool,
    /// Editor line to jump to when clicked.
    pub line: Option<u32>,
    /// A file this message is about (an export target): offers "Open folder".
    pub path: Option<PathBuf>,
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
    /// What colours the nodes: 0 cost %, 1 est. rows, 2 actual rows, 3 elapsed ms, 4 logical reads.
    pub metric: usize,
    /// Operator tree as indented text instead of the graph.
    pub text_view: bool,
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
            zoom: 0.0, // 0 = fit on first show
            pan: egui::Vec2::ZERO,
            show_properties: true,
            show_top_ops: true,
            top_ops_sort: (2, true),
            find: String::new(),
            metric: 0,
            text_view: false,
        }
    }
}

/// Per-grid interactive state.
/// Edit Data: the staged changes to a result set that is one table (see `editq`).
#[derive(Clone, Debug, Default)]
pub struct GridEdits {
    pub table: String,
    /// The table's schema when the SELECT named one (PostgreSQL: `"schema"."table"`).
    pub schema: Option<String>,
    /// PostgreSQL: booleans are TRUE / FALSE, no rowid.
    pub pg: bool,
    /// Key columns (name, index in the result): `rowid` or the primary key.
    pub key: Vec<(String, usize)>,
    /// Result columns that are columns of the table (writable); `rowid` is not.
    pub editable: Vec<usize>,
    /// (visible row, column) → the staged value (`None` = NULL).
    pub cells: std::collections::BTreeMap<(usize, usize), Option<String>>,
    pub deleted: std::collections::BTreeSet<usize>,
    /// New rows, shown after the data; one value per result column.
    pub inserted: Vec<Vec<Option<String>>>,
    pub editing: Option<CellEdit>,
    pub error: Option<String>,
    pub saving: bool,
    pub show_sql: bool,
}

/// The cell being typed into.
#[derive(Clone, Debug, Default)]
pub struct CellEdit {
    pub row: usize,
    pub col: usize,
    pub text: String,
    pub focused: bool,
    /// The editor lost focus to a click elsewhere: commit on the next frame.
    pub blur: bool,
}

impl GridEdits {
    /// Rows with staged cell changes (not deleted) + deleted rows + inserted rows.
    pub fn pending_count(&self) -> usize {
        let rows: std::collections::BTreeSet<usize> = self.cells.keys().filter(|(r, _)| !self.deleted.contains(r)).map(|(r, _)| *r).collect();
        rows.len() + self.deleted.len() + self.inserted.len()
    }
    /// The cell's text as the grid shows it now (`None` = NULL); rows past the data are inserted rows.
    pub fn current(&self, rs: &cobalt_results::ResultSet, plain: &cobalt_results::CellFormatter, row: usize, col: usize) -> Option<String> {
        let data_rows = rs.visible_count();
        if row >= data_rows {
            return self.inserted.get(row - data_rows).and_then(|r| r.get(col).cloned()).flatten();
        }
        if let Some(v) = self.cells.get(&(row, col)) {
            return v.clone();
        }
        if rs.is_null(row, col) { None } else { Some(rs.cell_text(row, col, plain).to_string()) }
    }
    /// Stage a value (`None` = NULL); a value equal to the original un-stages the cell.
    pub fn set_cell(&mut self, rs: &cobalt_results::ResultSet, plain: &cobalt_results::CellFormatter, row: usize, col: usize, value: Option<String>) {
        let data_rows = rs.visible_count();
        if row >= data_rows {
            if let Some(slot) = self.inserted.get_mut(row - data_rows).and_then(|r| r.get_mut(col)) {
                *slot = value;
            }
            return;
        }
        if !self.editable.contains(&col) {
            return;
        }
        let original = if rs.is_null(row, col) { None } else { Some(rs.cell_text(row, col, plain).to_string()) };
        if original == value {
            self.cells.remove(&(row, col));
        } else {
            self.cells.insert((row, col), value);
        }
    }
    /// Begin typing into a cell: `text` replaces the current value as the starting text.
    pub fn start_edit(&mut self, rs: &cobalt_results::ResultSet, plain: &cobalt_results::CellFormatter, row: usize, col: usize, text: Option<String>) -> bool {
        let data_rows = rs.visible_count();
        if self.saving || !self.editable.contains(&col) || row >= data_rows + self.inserted.len() || self.deleted.contains(&row) {
            return false;
        }
        let current = text.or_else(|| self.current(rs, plain, row, col)).unwrap_or_default();
        self.editing = Some(CellEdit { row, col, text: current, focused: false, blur: false });
        true
    }
    /// The next editable cell after (row, col): to the right, then the next row's first.
    pub fn next_cell(&self, total_rows: usize, row: usize, col: usize, back: bool) -> Option<(usize, usize)> {
        if back {
            if let Some(c) = self.editable.iter().rev().find(|&&c| c < col) {
                return Some((row, *c));
            }
            return if row > 0 { self.editable.last().map(|&c| (row - 1, c)) } else { None };
        }
        if let Some(c) = self.editable.iter().find(|&&c| c > col) {
            return Some((row, *c));
        }
        if row + 1 < total_rows { self.editable.first().map(|&c| (row + 1, c)) } else { None }
    }
}

/// A save in flight: the results (with their staged edits) kept aside, the query to run again.
pub struct EditResume {
    pub set: usize,
    pub script: String,
    pub run: Box<RunView>,
    pub count: usize,
}

#[derive(Default)]
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
    /// A primary-button press started on a cell and the button is still down: pointer movement
    /// over other cells extends the selection from the anchor.
    pub drag_select: bool,
    /// Open the viewer in record mode next time it is created (set by "View row as record").
    pub viewer_record: bool,
    /// Status-bar aggregates of the selection, cached by (selection, view generation, row count).
    pub summary: Option<(Selection, u64, usize, cobalt_results::summary::Summary)>,
    /// Totals row (context menu → Totals row).
    pub totals: Option<TotalsRow>,
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
}

pub struct FilterPopup {
    pub column: usize,
    /// Screen position to open at (under the column's funnel); applied on the first frame only.
    pub pos: egui::Pos2,
    pub placed: bool,
    pub search: String,
    pub values: Vec<(Arc<str>, usize)>,
    pub truncated: bool,
    pub checked: HashSet<Arc<str>>,
    pub condition_op: usize,
    pub condition_value: String,
    pub condition_value2: String,
}

#[derive(Default)]
pub struct GridFind {
    pub text: String,
    /// Matches found so far, in scan order (row, then column).
    pub matches: Vec<(usize, usize)>,
    /// The same matches for the grid's per-cell lookup (shared with the frame that paints).
    pub match_set: Arc<std::collections::HashSet<(usize, usize)>>,
    pub current: usize,
    pub generation: u64,
    pub case_sensitive: bool,
    pub use_regex: bool,
    pub whole_word: bool,
    /// A regex that did not compile.
    pub error: Option<String>,
    /// The scan over the whole result set runs on a thread and reports in blocks; None once
    /// every cell was looked at (or there is nothing to scan).
    pub scan: Option<FindScan>,
    pub scanned_rows: usize,
    pub total_rows: usize,
    /// Set by code that changes the text or options outside the bar (the agent): scan again.
    pub dirty: bool,
}

impl GridFind {
    /// Every cell of the current view has been looked at.
    pub fn complete(&self) -> bool {
        self.scan.is_none()
    }
}

/// A running find scan: blocks of matches arrive on `rx`; dropping it stops the thread.
pub struct FindScan {
    pub rx: crossbeam_channel::Receiver<FindMsg>,
    pub cancel: Arc<std::sync::atomic::AtomicBool>,
}

impl Drop for FindScan {
    fn drop(&mut self) {
        self.cancel.store(true, std::sync::atomic::Ordering::Relaxed);
    }
}

pub enum FindMsg {
    /// Matches of one block of rows, and how many rows have been scanned so far.
    Block { matches: Vec<(usize, usize)>, scanned_rows: usize },
    Done,
}

/// The aggregate shown in the grid's totals row.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum TotalKind {
    Sum,
    Avg,
    Min,
    Max,
    Count,
    Distinct,
}

impl TotalKind {
    pub const ALL: [TotalKind; 6] = [TotalKind::Sum, TotalKind::Avg, TotalKind::Min, TotalKind::Max, TotalKind::Count, TotalKind::Distinct];
    pub fn label(&self) -> &'static str {
        match self {
            TotalKind::Sum => "Sum",
            TotalKind::Avg => "Avg",
            TotalKind::Min => "Min",
            TotalKind::Max => "Max",
            TotalKind::Count => "Count",
            TotalKind::Distinct => "Distinct",
        }
    }
}

/// A sticky row under the headers with one aggregate per column.
#[derive(Clone, Debug)]
pub struct TotalsRow {
    pub kind: TotalKind,
    pub generation: u64,
    pub values: Vec<String>,
}

// ---------------------------------------------------------------------------------------------
// Whole-app state
// ---------------------------------------------------------------------------------------------

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum SidebarView {
    Servers,
    /// The local engine's tables (results, files, scratch, OneLake).
    Local,
    History,
    Fabric,
    Files,
    /// Tables and Files of the active Spark notebook's lakehouse.
    Lakehouse,
}

/// One entry of a lakehouse `Files/` folder, listed from OneLake.
#[derive(Clone, Debug)]
pub struct FileEntry {
    pub name: String,
    pub is_dir: bool,
    pub size: u64,
}

/// The Lakehouse sidebar's state (see `ui/lakehouse.rs`).
#[derive(Default)]
pub struct LakehousePane {
    /// `(workspace id, lakehouse name, lakehouse id)` shown; follows the active notebook's
    /// binding unless another lakehouse of the same workspace was picked.
    pub selected: Option<(String, String, String)>,
    pub tables: Option<Result<Vec<cobalt_fabric::OneLakeTable>, String>>,
    pub tables_loading: bool,
    /// Where the listing came from: "table API" (Fabric's, one call per schema) or "OneLake
    /// listing" (the DFS crawl, when the API is not available for the lakehouse).
    pub tables_source: &'static str,
    /// Listed folders by relative path (`""` = `Files/`).
    pub files: std::collections::HashMap<String, Result<Vec<FileEntry>, String>>,
    pub files_loading: std::collections::HashSet<String>,
    pub expanded: std::collections::HashSet<String>,
    /// Collapsed schema groups (`schema:<name>`); groups are open by default.
    pub collapsed: std::collections::HashSet<String>,
    /// This lakehouse's entry of the worker's `mirror_status`.
    pub mirror: Option<serde_json::Value>,
    pub mirror_pending: bool,
    pub filter: String,
    pub note: Option<String>,
    /// Re-read shadows and the mirror at this time (a clone or pull runs in the background).
    pub refresh_at: Option<std::time::Instant>,
}

impl LakehousePane {
    pub fn reset_data(&mut self) {
        self.tables = None;
        self.tables_loading = false;
        self.files.clear();
        self.files_loading.clear();
        self.mirror = None;
        self.mirror_pending = false;
        self.note = None;
    }
}

/// The Files sidebar: a remembered folder of query files.
#[derive(Default)]
pub struct FilesState {
    pub root: Option<PathBuf>,
    pub expanded: HashSet<PathBuf>,
    /// Directory listings, cleared by Refresh.
    pub cache: HashMap<PathBuf, Vec<(PathBuf, bool)>>,
    pub filter: String,
}

/// Downloads, suggestions and pending actions of the font manager (Settings › Fonts).
#[derive(Default)]
pub struct FontsState {
    pub downloads: crate::fonts::Downloads,
    pub actions: Vec<FontAction>,
    /// A pack to offer: text used scripts no font on this computer draws.
    pub suggest: Option<FontSuggest>,
    /// Entry ids the user said "not now" to this session.
    pub declined: HashSet<String>,
    pub registry_fetch: Option<Arc<parking_lot::Mutex<Option<Result<usize, String>>>>>,
    pub registry_note: Option<String>,
}

#[derive(Clone, Debug)]
pub enum FontAction {
    Download(String),
    Remove(String),
    Refresh,
}

#[derive(Clone, Debug)]
pub struct FontSuggest {
    pub packs: Vec<crate::fonts::Pack>,
    pub entry_id: String,
}

pub struct AppState {
    /// Paths the agent asked to show as hovering over the window (the drop overlay), until cleared.
    pub simulated_hover: Vec<PathBuf>,
    pub fonts: FontsState,
    pub library: Library,
    pub fabric: crate::fabric::FabricState,
    pub files: FilesState,
    pub lakehouse_pane: LakehousePane,
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
    /// The palette was opened by a click this frame: that click must not count as "outside".
    pub palette_just_opened: bool,
    pub palette_query: String,
    pub palette_selected: usize,
    pub history: HistoryView,
    pub settings_open: bool,
    pub about_open: bool,
    /// Help → What's new: the changelog, rendered in a window.
    pub changelog: ChangelogUi,
    pub shortcuts_open: bool,
    /// Help → Check for Updates… was chosen; the app fires the request.
    pub update_check_requested: bool,
    /// The update dialog asked to skip this version; the app persists it in settings.
    pub skip_version_request: Option<String>,
    /// An installer download in flight or done (Help › Check for updates › Download and install).
    pub update_download: Option<crate::update::UpdateDownload>,
    /// A verified installer to start when Cobalt exits ("Install when I close Cobalt").
    pub pending_installer: Option<std::path::PathBuf>,
    /// Set by the update dialog: close the app now (the installer is started on exit).
    pub close_for_update: bool,
    pub focus: Focus,
    pub status_flash: Option<(String, Instant)>,
    pub last_hot_exit_save: Instant,
    pub recently_closed_count: usize,
    /// The Servers tree's Local Spark root is open.
    pub spark_root_expanded: bool,
    /// The Local root of the Servers tree (local tables), and the tables whose columns are open.
    pub local_root_expanded: bool,
    pub local_expanded: HashSet<String>,
    /// The Local sidebar's filter text.
    pub local_filter: String,
    /// The editor groups (`layout.rs`): which tabs are in which group, how the area is split.
    pub layout: crate::layout::Layout,
    /// Where each group was drawn last frame (for "move to the group on the left").
    pub group_rects: std::collections::HashMap<crate::layout::GroupId, egui::Rect>,
    /// The layout generation the snapshot store holds.
    pub layout_saved_generation: u64,
    /// The engine's function names, for completion on Local tabs.
    pub local_functions: Arc<Vec<String>>,
    /// The registry generation the tabs' catalogs were built from.
    pub local_generation: u64,
    /// Completion catalogs of lakehouses (tables, schemas, columns from the Delta logs), by
    /// lakehouse id; Spark SQL tabs bound to the lakehouse share the Arc.
    pub spark_catalogs: std::collections::HashMap<String, Arc<DatabaseCatalog>>,
    /// Semantic model catalogs by target key (`TabularTarget::key`), shared by the model
    /// explorer and the DAX tabs' completion.
    pub model_catalogs: std::collections::HashMap<String, Loadable<Arc<cobalt_core::tabular::ModelCatalog>>>,
    /// The DAX functions a model reports (`INFO.FUNCTIONS()`), by target key, in the
    /// completer's shape.
    pub dax_functions: std::collections::HashMap<String, Arc<Vec<cobalt_sql::dax::DaxFunctionDoc>>>,
    /// Open nodes of the model explorer trees (`<key>/t/<table>` …).
    pub model_tree_expanded: std::collections::HashSet<String>,
    /// Model explorer: show hidden tables, columns and measures.
    pub model_show_hidden: bool,
    /// Saved Analysis Services endpoints (the Servers tree's "Analysis Services" root).
    pub tabular: TabularRoot,
    pub spark_catalog_loading: std::collections::HashSet<String>,
    /// Open the Spark tab's lakehouse menu on the next frame (Connect / Change connection on a Spark tab).
    pub open_spark_chip: Option<TabId>,
    /// A session start asked for (Spark menu, tree) for this tab's binding, retried while its
    /// workspace items load.
    pub spark_start_tab: Option<TabId>,
    pub export_progress: Option<Arc<parking_lot::Mutex<(usize, usize)>>>,
    /// Settings changes requested by ops/UI; applied by the app (which owns `Settings`).
    pub settings_patch: Vec<SettingsPatch>,
    /// A notebook asked for its `%pip` packages to be installed: start the Libraries job once
    /// the settings patch that added them is applied.
    pub install_libraries_requested: bool,
    /// A chart image capture waiting for the next frame's screenshot.
    pub pending_capture: Option<PendingCapture>,
    pub settings_draft: Option<Settings>,
    /// Scroll the settings window to this section when it opens.
    pub settings_scroll_to: Option<&'static str>,
    /// Settings → Spark runtime: status snapshot and the running install job.
    pub runtime: crate::runtime::RuntimeUi,
    /// The local Spark kernel notebooks run PySpark (and Spark SQL) cells on.
    pub kernel: crate::kernel::KernelUi,
    /// Restart requested: start again once the old session has stopped.
    pub kernel_restart_pending: bool,
    pub shadows: ShadowsUi,
    /// The last toasts shown (agent diagnostics).
    pub recent_toasts: std::collections::VecDeque<String>,
    /// Commands raised by widgets drawn outside `dispatch`'s reach (status bar); run next frame.
    pub pending_commands: Vec<crate::commands::Command>,
    /// Synthetic input from the agent (`press` / `type_text` verbs), injected next frame.
    pub injected_events: Vec<egui::Event>,
    /// Agent-injected pointer input, one frame per entry. Fed through eframe's `raw_input_hook`
    /// so egui sees it before `begin_pass` (hover, press, drag all work like a real mouse).
    pub injected_pointer: std::collections::VecDeque<Vec<egui::Event>>,
}

#[derive(Debug, Clone)]
pub enum SettingsPatch {
    LastExportDir(String),
    /// Extension of the last export format chosen (`csv`, `parquet`, `delta`…).
    LastExportFormat(String),
    Theme(ThemeChoice),
    UiScale(f32),
    ShowWelcome(bool),
    /// `spark.engine`: `pyspark` or `sail` (the next session runs on it).
    SparkEngine(String),
    /// `appearance.new_tab_kind`: what the + on the tab strip opens.
    NewTabKind(String),
    /// `fonts.suggest_downloads`.
    SuggestFonts(bool),
    /// `fonts.ui_font` / `fonts.mono_font`: a registry entry id, a file path, or empty for the default.
    UiFont(String),
    MonoFont(String),
    /// `spark.python_packages`: add these specs (a notebook's `%pip install` lines) when absent.
    AddPythonPackages(Vec<String>),
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Default)]
pub enum Focus {
    #[default]
    Editor,
    Results,
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
    /// Add or edit a saved Analysis Services endpoint (`TabularRoot`).
    TabularProfile { draft: Box<cobalt_core::tabular::TabularProfile>, password: String, editing: bool, test: Option<Loadable<String>> },
    /// Save a notebook back to its Fabric item (overwrites the item's definition).
    ConfirmFabricSave { tab_index: usize },
    Connection(Box<ConnectionDialog>),
    Password { profile: ConnectionProfile, password: String, remember: bool, purpose: ConnectPurpose, error: Option<String> },
    AuthWaiting { profile: ConnectionProfile, purpose: ConnectPurpose, message: String, device: Arc<parking_lot::Mutex<Option<(String, String)>>>, url: Arc<parking_lot::Mutex<Option<String>>>, cancel: Arc<std::sync::atomic::AtomicBool>, started: Instant },
    Group { group: ServerGroup, is_new: bool },
    ConfirmClose { tab_index: usize },
    /// Close several tabs at once (Close others / Close all); the dirty ones are listed.
    ConfirmCloseMany { tab_ids: Vec<TabId>, dirty: Vec<String> },
    /// Run a notebook with the parameters cell's assignments overridden (`params` = name, value).
    RunWithParameters { tab_index: usize, params: Vec<(String, String)> },
    /// Add or edit a dashboard tile (`tile` = index when editing).
    TileEditor { tab_index: usize, tile: Option<usize>, draft: cobalt_core::TileSpec, profile: Option<ProfileId> },
    /// Pin a result set of a query tab onto a dashboard (`target` = an open dashboard's tab index, None = a new one).
    PinTile { tab_index: usize, set: usize, title: String, target: Option<usize>, interval_secs: u64, kind: cobalt_core::TileKind },
    ConfirmDeleteProfile { profile: ProfileId },
    /// Empty the built-in scratch database (its file is deleted and made again).
    ConfirmEmptyScratch,
    ConfirmDeleteGroup { group: GroupId },
    ConfirmWrite { tab_index: usize, statement_preview: String, script: String, opts: ExecOptions, start_line: u32 },
    Export(Box<ExportDialog>),
    Import(Box<ImportDialog>),
    ChangeConnection { tab_index: usize },
    ExecOptions { tab_index: usize, opts: ExecOptions },
    Rename { tab_index: usize, title: String },
    /// Pick a workspace and lakehouse for a Spark SQL tab (`tab_index` None = a new tab).
    SparkLakehouse { tab_index: Option<usize>, workspace: Option<String> },
    /// Pick a semantic model (a Fabric workspace's, or one of a saved Analysis Services
    /// endpoint) for a DAX or Explore tab; `tab_index` None = a new tab (`explore` = an Explore tab).
    ChooseModel { tab_index: Option<usize>, explore: bool, source: Option<ModelSource>, search: String, file: Option<PathBuf> },
    /// Rename a local table (the engine's registry).
    RenameLocalTable { name: String, to: String },
    /// Diff two local tables: A (was), B (now), the key columns, the compared columns (empty = all shared).
    DiffSetup { a: String, b: String, keys: Vec<String>, compare: Vec<String> },
    /// Save a result set or a local table as `scratch.<name>`.
    SaveScratch { from: ScratchFrom, name: String },
    /// Forget / delete a local table (a scratch table's file is deleted).
    ConfirmForgetLocal { name: String },
    AdsImport { path: String, summary: Option<String>, error: Option<String> },
    UpdateAvailable { info: crate::update::UpdateInfo },
}

/// Where the model chooser lists models from.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum ModelSource {
    Recent,
    Workspace(String),
    Profile(uuid::Uuid),
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
    /// Service principal certificate (.pem) path; empty = use the secret.
    pub sp_cert: String,
    pub port_text: String,
    pub show_advanced: bool,
    /// "From connection string" box: pasted text, applied on demand.
    pub conn_string: String,
    pub conn_string_note: Option<String>,
    /// 0 = SQL Server (TDS), 1 = SQLite file.
    pub engine_index: usize,
    /// The Server field took focus once when the dialog opened.
    pub focus_done: bool,
    pub error: Option<(String, Option<String>)>,
    pub testing: bool,
    pub test_result: Option<Result<String, String>>,
    pub connect_after_save: Option<ConnectPurpose>,
    pub recent: Vec<ConnectionProfile>,
    pub group_index: usize,
    pub color_index: Option<usize>,
}

/// One column of a file being imported, as the user may edit it.
#[derive(Clone, Debug)]
pub struct ImportColumnEdit {
    pub name: String,
    pub sql_type: String,
    pub nullable: bool,
    pub include: bool,
    /// The file's own type, for the hint column.
    pub source: String,
}

/// Where the rows of an Import dialog come from.
#[derive(Clone)]
pub enum ImportSource {
    /// `ImportDialog::path` (CSV/Parquet/Arrow).
    File,
    /// A result set (or a selection of it) from a query tab: "Save results as table".
    Results { rs: Arc<ResultSet>, label: String },
}

pub struct ImportDialog {
    /// The target tab (its connection does the bulk insert); the file path for `ImportSource::File`.
    pub tab_index: usize,
    pub source: ImportSource,
    pub path: String,
    /// 0 = a table in this tab's database (bulk insert); 1 = any export target (file, OneLake).
    pub destination: usize,
    /// CSV options (ignored for Parquet / Arrow).
    pub delimiter: String,
    pub has_header: bool,
    pub inspection: Option<cobalt_import::Inspection>,
    pub inspect_error: Option<String>,
    pub columns: Vec<ImportColumnEdit>,
    pub schema_name: String,
    pub table_name: String,
    /// Append to an existing table (types come from the table) instead of creating one.
    pub existing: bool,
    pub existing_columns: Loadable<Vec<ColumnInfo>>,
    pub want_existing_columns: bool,
    pub running: bool,
    pub rows_done: u64,
    pub started: Option<Instant>,
    pub result: Option<Result<String, String>>,
    pub cancel: Arc<std::sync::atomic::AtomicBool>,
}

pub struct ExportDialog {
    pub tab_index: usize,
    pub set_index: usize,
    pub format_index: usize,
    pub path: String,
    pub selection_only: bool,
    pub delta_mode: usize,
    pub delta_partition: String,
    /// 0 = local file/folder, 1 = OneLake lakehouse (Delta table or Files/ upload).
    pub destination: usize,
    /// Selected lakehouse item id for OneLake.
    pub onelake_item: Option<String>,
    /// Table name (Delta) or file name (other formats) inside the lakehouse.
    pub onelake_name: String,
    /// Schema folder for schema-enabled lakehouses (`Tables/<schema>/<table>`); empty = none.
    pub onelake_schema: String,
    pub csv_delimiter: String,
    pub csv_headers: bool,
    pub json_lines: bool,
    pub running: bool,
    pub progress: Option<(usize, usize)>,
    pub result: Option<Result<String, String>>,
    pub cancel: Arc<std::sync::atomic::AtomicBool>,
    /// `Some` = "Run to file": the query runs and streams straight to the target.
    pub run_mode: Option<RunMode>,
    /// `Some` = "Import to file/lakehouse": rows come from a file (the Import dialog's spec), not a query.
    pub import: Option<crate::ops::ImportSpec>,
    /// SQLite format: the table inside the file (empty = the file's stem) and what to do when it exists
    /// (0 = create, 1 = replace, 2 = append).
    pub sqlite_table: String,
    pub sqlite_mode: usize,
}

impl AppState {
    pub fn new() -> Self {
        Self {
            simulated_hover: Vec::new(),
            fonts: FontsState::default(),
            library: Library::default(),
            tabs: Vec::new(),
            active_tab: None,
            next_untitled: 1,
            sidebar_visible: true,
            sidebar_view: SidebarView::Servers,
            fabric: Default::default(),
            files: FilesState::default(),
            lakehouse_pane: LakehousePane::default(),
            sidebar_width: 280.0,
            pending_meta: HashMap::new(),
            formatter: CellFormatter::default(),
            dialog: Dialog::None,
            palette_open: false,
            palette_just_opened: false,
            palette_query: String::new(),
            palette_selected: 0,
            history: HistoryView::default(),
            settings_open: false,
            about_open: false,
            changelog: ChangelogUi::default(),
            update_check_requested: false,
            skip_version_request: None,
            update_download: None,
            pending_installer: None,
            close_for_update: false,
            shortcuts_open: false,
            focus: Focus::Editor,
            status_flash: None,
            last_hot_exit_save: Instant::now(),
            recently_closed_count: 0,
            spark_root_expanded: true,
            local_root_expanded: true,
            local_expanded: HashSet::new(),
            local_filter: String::new(),
            layout: crate::layout::Layout::new(),
            group_rects: Default::default(),
            layout_saved_generation: 0,
            local_functions: Arc::new(Vec::new()),
            local_generation: 0,
            spark_catalogs: Default::default(),
            model_catalogs: Default::default(),
            dax_functions: Default::default(),
            model_tree_expanded: Default::default(),
            model_show_hidden: false,
            tabular: Default::default(),
            spark_catalog_loading: Default::default(),
            open_spark_chip: None,
            spark_start_tab: None,
            export_progress: None,
            settings_patch: Vec::new(),
            install_libraries_requested: false,
            pending_capture: None,
            settings_draft: None,
            settings_scroll_to: None,
            runtime: Default::default(),
            kernel: Default::default(),
            kernel_restart_pending: false,
            shadows: ShadowsUi::default(),
            recent_toasts: Default::default(),
            pending_commands: Vec::new(),
            injected_events: Vec::new(),
            injected_pointer: std::collections::VecDeque::new(),
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
    /// A tab by id: an editor tab, or a dashboard tile's own hidden tab (tiles connect and run
    /// through the same session paths, keyed by their tab id).
    pub fn tab_mut(&mut self, id: TabId) -> Option<&mut EditorTab> {
        if let Some(i) = self.tabs.iter().position(|t| t.id == id) {
            return self.tabs.get_mut(i);
        }
        self.tabs.iter_mut().find_map(|t| t.dashboard.as_deref_mut().and_then(|d| d.tiles.iter_mut().map(|x| &mut x.tab).find(|x| x.id == id)))
    }


    /// The Import dialog, when it targets tab `id` (import events carry the tab they ran on).
    pub fn import_dialog_for(&mut self, id: TabId) -> Option<&mut ImportDialog> {
        let target = match &self.dialog {
            Dialog::Import(d) => self.tabs.get(d.tab_index).map(|t| t.id),
            _ => None,
        };
        match &mut self.dialog {
            Dialog::Import(d) if target == Some(id) => Some(d),
            _ => None,
        }
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
                    t.last_database = Some(database.clone());
                    out.push(Followup::LoadTabDatabases(tab));
                    out.push(Followup::LoadCatalog(tab, database));
                }
            }
            Event::ConnectFailed { tab, error, hint, database } => {
                if let Some(t) = self.tab_mut(tab) {
                    let missing = is_database_missing(&error);
                    let default_db = t.profile.as_ref().and_then(|p| p.database.clone());
                    if missing {
                        // never steer a reconnect back to a database that is gone
                        t.last_database = None;
                    }
                    if missing && database.is_some() && database != default_db {
                        // the remembered database vanished (a dropped lakehouse endpoint, a
                        // dropped database): fall back to the profile's default once, and do
                        // not replay a run that was meant for the old database
                        t.pending_run = None;
                        t.pending_import = false;
                        t.conn = ConnState::Connecting;
                        let db = database.unwrap_or_default();
                        let target = default_db.clone().unwrap_or_else(|| "the server default".into());
                        out.push(Followup::Toast(ToastKind::Warning, format!("Database {db} is no longer available on this connection; connecting to {target} instead.\n{error}")));
                        out.push(Followup::ReconnectDefault(tab));
                    } else {
                        t.conn = ConnState::Failed { error: error.clone(), hint: hint.clone() };
                        out.push(Followup::Toast(ToastKind::Error, format!("Connection failed: {error}{}", hint.map(|h| format!("\n{h}")).unwrap_or_default())));
                    }
                }
            }
            Event::Disconnected { tab } => {
                if let Some(t) = self.tab_mut(tab) {
                    if let Some(db) = t.conn.database() {
                        t.last_database = Some(db.to_string());
                    }
                    t.conn = ConnState::Disconnected;
                    if let Some(r) = &mut t.run {
                        if r.is_live() {
                            r.state = RunViewState::Failed;
                            r.messages.push(MessageLine { text: "Connection closed.".into(), is_error: true, is_batch_header: false, line: None, path: None });
                        }
                    }
                }
            }
            Event::Reconnected { tab, engine, spid, database, idle } => {
                if let Some(t) = self.tab_mut(tab) {
                    t.conn = ConnState::Connected { engine, spid, database: database.clone() };
                    t.last_database = Some(database.clone());
                    let text = format!(
                        "The connection had been closed while idle ({}); reconnected to {} as SPID {}.",
                        fmt_idle(idle),
                        database,
                        spid.map(|s| s.to_string()).unwrap_or_else(|| "?".into())
                    );
                    match t.run.as_mut().filter(|r| r.is_live()) {
                        Some(r) => r.messages.push(MessageLine { text, is_error: false, is_batch_header: false, line: None, path: None }),
                        None => out.push(Followup::Toast(ToastKind::Info, text)),
                    }
                    if t.catalog_database.as_deref() != Some(database.as_str()) {
                        out.push(Followup::LoadCatalog(tab, database));
                    }
                }
            }
            Event::ConnectionLost { tab, error, hint, idle, rerun } => {
                let text = format!(
                    "The connection had been closed while idle ({}) and could not be reopened: {error}{}",
                    fmt_idle(idle),
                    hint.map(|h| format!(" ({h})")).unwrap_or_default()
                );
                if let Some(t) = self.tab_mut(tab) {
                    if let Some(db) = t.conn.database() {
                        t.last_database = Some(db.to_string());
                    }
                    t.conn = ConnState::Disconnected;
                    let mut rerun_ok = false;
                    if let Some(r) = t.run.as_mut().filter(|r| r.is_live()) {
                        r.state = RunViewState::Failed;
                        r.elapsed = r.started.elapsed();
                        r.messages.push(MessageLine { text: text.clone(), is_error: true, is_batch_header: false, line: None, path: None });
                        out.push(Followup::FinishHistory { history_id: r.history_id, elapsed: r.elapsed, rows: 0, cancelled: false, failed: true, error: Some(text.clone()) });
                        // a Run to File already handed its job to the writer; only a plain run repeats
                        rerun_ok = r.export_target.is_none();
                    }
                    if rerun && rerun_ok {
                        t.pending_run = t.last_run_mode;
                    }
                    out.push(Followup::Toast(ToastKind::Error, text));
                    // back through the normal connect path (interactive sign-in when needed); a
                    // pending run goes out once the tab is connected again
                    out.push(Followup::Reconnect(tab));
                }
            }
            Event::NotConnected { tab } => {
                if let Some(t) = self.tab_mut(tab) {
                    if let Some(r) = &mut t.run {
                        r.state = RunViewState::Failed;
                        r.messages.push(MessageLine { text: "Not connected.".into(), is_error: true, is_batch_header: false, line: None, path: None });
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
                    if r.batch_times.len() <= batch {
                        r.batch_times.resize(batch + 1, (start_line, None, false));
                    }
                    r.batch_times[batch] = (start_line, None, false);
                    if r.batches > 1 || batch > 0 {
                        r.messages.push(MessageLine { text: format!("Started executing batch {} at line {}", batch + 1, start_line), is_error: false, is_batch_header: true, line: Some(start_line), path: None });
                    }
                }
            }
            Event::ResultSetStarted { tab, run, rs } => {
                if let Some(r) = self.run_mut(tab, run) {
                    let is_plan = cobalt_driver::is_showplan_result(&rs.columns);
                    r.result_sets.push(ResultSetView { rs, grid: GridState::default(), is_plan, profile: None, chart: None, edits: None });
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
                            r.messages.push(MessageLine { text: format!("({} row{} returned)", fmt_count(rows), if rows == 1 { "" } else { "s" }), is_error: false, is_batch_header: false, line: None, path: None });
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
            Event::Paused { tab, run, index } => {
                if let Some(r) = self.run_mut(tab, run) {
                    r.paused_set = Some(index);
                    r.state = RunViewState::Paused;
                    // freeze the timer while we wait for the user
                    r.elapsed = r.started.elapsed();
                }
            }
            Event::Message { tab, run, message, batch_start_line } => {
                if let Some(r) = self.run_mut(tab, run) {
                    let line = if message.line > 0 { Some(batch_start_line + message.line - 1) } else { None };
                    // Fabric Warehouse tags every statement with an informational "Statement ID / Query
                    // hash / Distributed request ID" line (15806, 24528). Keep it, but muted and without
                    // the Msg/Level header so real messages stand out.
                    let is_fabric_trace = !message.is_error && matches!(message.number, 15806 | 24528);
                    let mut text = String::new();
                    if !is_fabric_trace {
                        if let Some(h) = message.headline() {
                            text.push_str(&h);
                            text.push('\n');
                        }
                    }
                    text.push_str(&message.message);
                    // a file engine's EXPLAIN text (marked by the driver) goes to the Plan pane
                    if message.procedure.as_deref() == Some("plan") && !message.is_error {
                        let (key, body) = message.message.split_once('\n').map(|(k, b)| (k.trim().to_string(), b.to_string())).unwrap_or_else(|| ("plan".to_string(), message.message.clone()));
                        r.text_plan.get_or_insert_with(Vec::new).push((key, body));
                    } else {
                        r.messages.push(MessageLine { text, is_error: message.is_error, is_batch_header: is_fabric_trace, line, path: None });
                    }
                }
            }
            Event::RowsAffected { tab, run, rows } => {
                if let Some(r) = self.run_mut(tab, run) {
                    r.rows_affected.push(rows);
                    r.messages.push(MessageLine { text: format!("({} row{} affected)", fmt_count(rows), if rows == 1 { "" } else { "s" }), is_error: false, is_batch_header: false, line: None, path: None });
                }
            }
            Event::BatchDone { tab, run, batch, error, elapsed } => {
                if let Some(r) = self.run_mut(tab, run) {
                    if let Some(t) = r.batch_times.get_mut(batch) {
                        t.1 = Some(elapsed);
                        t.2 = error.is_some();
                    }
                }
            }
            Event::RunDone { tab, run, cancelled, failed, elapsed, total_rows } => {
                if let Some(t) = self.tab_mut(tab) {
                    if t.tabular.is_some() && t.run.as_ref().map(|r| r.id == run).unwrap_or(false) {
                        crate::tabularq::note_capped(t);
                    }
                }
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
                    r.messages.push(MessageLine { text, is_error: false, is_batch_header: false, line: None, path: None });
                    let history_id = r.history_id;
                    let has_error = r.has_error();
                    out.push(Followup::FinishHistory { history_id, elapsed, rows: total_rows, cancelled, failed: failed || has_error, error: r.messages.iter().find(|m| m.is_error).map(|m| m.text.clone()) });
                    if has_error && !cancelled {
                        if let Some(t) = self.tab_mut(tab) {
                            t.results_tab = ResultsTab::Messages;
                        }
                    }
                }
                if self.tab_mut(tab).map(|t| t.is_notebook()).unwrap_or(false) {
                    out.push(Followup::NotebookPump(tab, failed || cancelled));
                }
                // a file database's catalog follows a run with DDL (the completion and the tree)
                let ddl = self.run_mut(tab, run).map(|r| { let u = r.script.to_ascii_uppercase(); ["CREATE ", "DROP ", "ALTER ", "ATTACH ", "DETACH "].iter().any(|k| u.contains(k)) }).unwrap_or(false);
                if ddl {
                    if let Some(t) = self.tab_mut(tab) {
                        if let Some(pid) = t.profile.as_ref().filter(|p| p.is_file()).map(|p| p.id) {
                            if let Some(db) = t.conn.database().map(str::to_string) {
                                out.push(Followup::ReloadCatalog(tab, db.clone()));
                                out.push(Followup::RefreshDatabase { profile: pid, database: db });
                            }
                        }
                    }
                }
                // Edit Data: a save script finished, or the query ran again after one
                let run_ok = !failed && !cancelled && !self.run_mut(tab, run).map(|r| r.has_error()).unwrap_or(true);
                if let Some(t) = self.tab_mut(tab) {
                    if t.edit_resume.is_some() {
                        out.push(Followup::EditSaved { tab, ok: run_ok });
                    } else if let Some(set) = t.edit_reopen.take() {
                        if run_ok {
                            out.push(Followup::EditReopen { tab, set });
                        }
                    }
                }
            }
            Event::DatabaseChanged { tab, database } => {
                if let Some(t) = self.tab_mut(tab) {
                    if let ConnState::Connected { database: d, .. } = &mut t.conn {
                        *d = database.clone();
                    }
                    t.last_database = Some(database.clone());
                    if !t.custom_title {
                        if t.explore.is_some() {
                            crate::exploreq::retitle(t);
                        } else if let Some(p) = t.profile.as_ref().filter(|p| p.is_file()) {
                            t.title = format!("SQLQuery_{} · {}", t.untitled_index, p.display_name());
                        } else {
                            t.title = format!("SQLQuery_{} · {}", t.untitled_index, database);
                        }
                    }
                    out.push(Followup::LoadCatalog(tab, database));
                }
            }
            Event::DatabaseChangeFailed { tab, error } => {
                let title = self.tab_mut(tab).map(|t| t.title.clone()).unwrap_or_default();
                out.push(Followup::Toast(ToastKind::Error, format!("{title}: {error}")));
            }
            Event::ImportStarted { tab } => {
                if let Some(d) = self.import_dialog_for(tab) {
                    d.running = true;
                    d.rows_done = 0;
                    d.started = Some(Instant::now());
                }
            }
            Event::ImportProgress { tab, rows } => {
                if let Some(d) = self.import_dialog_for(tab) {
                    d.rows_done = rows;
                }
            }
            Event::ImportDone { tab, result } => {
                let (file, pg) = self.tabs.iter().find(|t| t.id == tab).and_then(|t| t.profile.as_ref()).map(|p| (p.is_file(), p.engine == cobalt_core::EngineFamily::Postgres)).unwrap_or((false, false));
                let target = if let Dialog::Import(d) = &self.dialog { Some(if file { format!("\"{}\"", d.table_name) } else if pg { format!("\"{}\".\"{}\"", if d.schema_name.trim().is_empty() { "public" } else { d.schema_name.trim() }, d.table_name) } else { format!("[{}].[{}]", d.schema_name, d.table_name) }) } else { None };
                let msg = match &result {
                    Ok((rows, el)) => Ok(format!("Imported {} rows into {} in {:.1}s", fmt_count(*rows), target.clone().unwrap_or_default(), el.as_secs_f32())),
                    Err(e) if e == "cancelled" => Err("Import cancelled; the transaction was rolled back.".to_string()),
                    Err(e) => Err(format!("Import failed (rolled back): {e}")),
                };
                if let Dialog::Import(d) = &mut self.dialog {
                    d.running = false;
                    d.result = Some(msg.clone());
                    if let Ok((rows, _)) = &result {
                        d.rows_done = *rows;
                    }
                }
                match &msg {
                    Ok(m) => out.push(Followup::Toast(ToastKind::Success, m.clone())),
                    Err(e) => out.push(Followup::Toast(ToastKind::Error, e.clone())),
                }
                if result.is_ok() {
                    if let Some(t) = self.tab_mut(tab) {
                        if let (Some(p), Some(db)) = (t.profile.as_ref().map(|p| p.id), t.conn.database().map(str::to_string)) {
                            out.push(Followup::RefreshDatabase { profile: p, database: db });
                        }
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
        let t = self.tab_mut(tab)?;
        if let Some(nb) = t.notebook.as_deref_mut() {
            return nb.cells.iter_mut().find_map(|c| c.run.as_mut().filter(|r| r.id == run));
        }
        t.run.as_mut().filter(|r| r.id == run)
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
                    (MetadataRequest::TableStats { obj }, res) => {
                        let db = node.db_nodes.entry(obj.database.clone()).or_default();
                        db.stats.insert(obj.object_id.unwrap_or(0), match res { Ok(R::TableStats(s)) => Loadable::Loaded(s), Err(e) => Loadable::Failed(e), _ => Loadable::Failed("unexpected".into()) });
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
            MetaPurpose::ExploreColumns { tab } => {
                if let Some(t) = self.tab_mut(tab) {
                    let name = t.explore.as_ref().and_then(|e| e.spec.source.as_ref()).and_then(|s| s.sql_field_table()).unwrap_or_default();
                    match result {
                        Ok(R::Columns(cols)) => {
                            t.model_catalog = Some(std::sync::Arc::new(crate::exploreq::catalog_from_columns(&name, &cols)));
                            if let Some(ex) = t.explore.as_deref_mut() {
                                ex.dax_error = None;
                            }
                        }
                        Err(e) => {
                            if let Some(ex) = t.explore.as_deref_mut() {
                                ex.dax_error = Some(format!("columns of {name}: {e}"));
                            }
                        }
                        _ => {}
                    }
                }
            }
            MetaPurpose::ExploreDistinct { tab, table, column } => {
                if let Some(ex) = self.tab_mut(tab).and_then(|t| t.explore.as_deref_mut()) {
                    let v = match result {
                        Ok(R::Values(v)) => Loadable::Loaded(v),
                        Err(e) => Loadable::Failed(e),
                        _ => Loadable::Failed("unexpected".into()),
                    };
                    ex.distinct.insert((table, column), v);
                }
            }
            MetaPurpose::ImportColumns => {
                if let Dialog::Import(d) = &mut self.dialog {
                    match result {
                        Ok(R::Columns(cols)) => {
                            d.apply_existing_columns(&cols);
                            d.existing_columns = Loadable::Loaded(cols);
                        }
                        Err(e) => d.existing_columns = Loadable::Failed(e),
                        _ => d.existing_columns = Loadable::Failed("unexpected".into()),
                    }
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
    /// The tab's catalog again, past the cache (a run with DDL on a file database).
    ReloadCatalog(TabId, String),
    FinishHistory { history_id: Option<i64>, elapsed: Duration, rows: u64, cancelled: bool, failed: bool, error: Option<String> },
    CacheCatalog { profile: ProfileId, database: String, catalog: Arc<DatabaseCatalog> },
    OpenScriptTab { title: String, sql: String, profile: ProfileId, database: String, run: bool },
    Toast(ToastKind, String),
    /// Reload a database's objects in the Servers tree (after an import created a table).
    RefreshDatabase { profile: ProfileId, database: String },
    /// Reconnect the tab to its profile and last database (after a session was lost for good).
    Reconnect(TabId),
    /// Reconnect the tab to its profile's default database (the remembered one is gone).
    ReconnectDefault(TabId),
    /// A notebook cell finished: start the next queued cell (or drop the queue after a failure).
    NotebookPump(TabId, bool),
    /// Edit Data's save script finished (ok = committed): run the query again, or restore the grid with the error.
    EditSaved { tab: TabId, ok: bool },
    /// The query ran again after a save (or the tree's Edit Data): Edit Data back on for the set.
    EditReopen { tab: TabId, set: usize },
}

/// A login rejected because the requested database does not exist (or is not visible): SQL
/// Server 4060 "Cannot open database", Azure/Fabric "the database was not found or you have
/// insufficient permissions", Fabric endpoints that were deleted.
pub fn is_database_missing(error: &str) -> bool {
    let e = error.to_ascii_lowercase();
    e.contains("cannot open database")
        || e.contains("database was not found")
        || e.contains("database does not exist")
        || (e.contains("database") && e.contains("not found"))
        || e.contains("cannot find the database")
}

impl ImportDialog {
    /// Existing-table mode: take names, types and nullability from the table; file columns that
    /// the table does not have are switched off, table columns missing from the file are noted.
    pub fn apply_existing_columns(&mut self, cols: &[ColumnInfo]) {
        for c in &mut self.columns {
            match cols.iter().find(|t| t.name.eq_ignore_ascii_case(&c.name)) {
                Some(t) => {
                    c.name = t.name.clone();
                    c.sql_type = t.sql_type.to_string();
                    c.nullable = t.nullable;
                    c.include = !t.is_computed && !t.is_identity;
                }
                None => c.include = false,
            }
        }
    }
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

/// An idle stretch for people: "6 h 12 min", "3 min", "45 s".
pub fn fmt_idle(d: Duration) -> String {
    let s = d.as_secs();
    if s >= 3600 {
        format!("{} h {} min", s / 3600, (s % 3600) / 60)
    } else if s >= 60 {
        format!("{} min", s / 60)
    } else {
        format!("{s} s")
    }
}

#[cfg(test)]
mod db_missing_tests {
    use super::is_database_missing;
    #[test]
    fn recognises_missing_database_logins() {
        assert!(is_database_missing("login failed: Login failed for user '<token-identified principal>'.Reason: Authentication was successful, but the database was not found or you have insufficient permissions to connect to it."));
        assert!(is_database_missing("login failed: Cannot open database \"tmpdb\" requested by the login. The login failed."));
        assert!(!is_database_missing("login failed: Login failed for user 'sa'."));
        assert!(!is_database_missing("connection failed: timed out after 30 s"));
    }
}

pub fn fmt_duration(d: Duration) -> String {
    let ms = d.as_millis();
    let h = ms / 3_600_000;
    let m = (ms / 60_000) % 60;
    let s = (ms / 1000) % 60;
    let frac = ms % 1000;
    format!("{h:02}:{m:02}:{s:02}.{frac:03}")
}
