//! Every user-invocable action, its palette label, category and default shortcut.
//!
//! ADS/SSMS muscle memory is preserved: F5 runs, Ctrl+L estimated plan, Ctrl+M actual plan,
//! Ctrl+Shift+C copy with headers, Ctrl+N new query, F1 / Ctrl+Shift+P palette.

use egui::{Key, KeyboardShortcut, Modifiers};
use serde::{Deserialize, Serialize};
use std::collections::HashMap;

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Command {
    // file
    NewQuery,
    OpenFile,
    SaveFile,
    SaveFileAs,
    CloseTab,
    ReopenClosedTab,
    NextTab,
    PrevTab,
    ImportAdsSettings,
    Quit,
    // connections
    NewConnection,
    ConnectTab,
    DisconnectTab,
    ChangeConnection,
    RefreshTree,
    RefreshIntelliSense,
    // query
    RunQuery,
    RunCurrentStatement,
    CancelQuery,
    EstimatedPlan,
    ToggleActualPlan,
    ParseQuery,
    FormatDocument,
    ExecutionOptions,
    // editor
    Find,
    Replace,
    GoToLine,
    ToggleLineComment,
    ToggleBlockComment,
    TriggerCompletion,
    UppercaseKeywords,
    // results
    ToggleResults,
    FocusEditorOrResults,
    CopySelection,
    CopyWithHeaders,
    CopyHeaders,
    CopyAsMarkdown,
    CopyAsJson,
    CopyAsCsv,
    CopyAsInsert,
    CopyAsInList,
    SelectAllCells,
    FindInResults,
    SaveResultsCsv,
    SaveResultsExcel,
    SaveResultsJson,
    SaveResultsXml,
    SaveResultsMarkdown,
    SaveResultsParquet,
    SaveResultsArrow,
    SaveResultsDelta,
    MaximizeResultSet,
    OpenCellViewer,
    ClearFilters,
    // plan
    OpenPlanFile,
    SavePlanFile,
    ShowPlanXml,
    PlanZoomIn,
    PlanZoomOut,
    PlanZoomFit,
    // view
    CommandPalette,
    ToggleSidebar,
    ShowServers,
    ShowHistory,
    ShowSettings,
    ToggleTheme,
    ZoomIn,
    ZoomOut,
    ZoomReset,
    // help
    About,
    KeyboardShortcuts,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum Category {
    File,
    Connection,
    Query,
    Editor,
    Results,
    Plan,
    View,
    Help,
}

impl Category {
    pub fn label(&self) -> &'static str {
        match self {
            Category::File => "File",
            Category::Connection => "Connection",
            Category::Query => "Query",
            Category::Editor => "Editor",
            Category::Results => "Results",
            Category::Plan => "Plan",
            Category::View => "View",
            Category::Help => "Help",
        }
    }
}

pub struct CommandInfo {
    pub cmd: Command,
    pub id: &'static str,
    pub label: &'static str,
    pub category: Category,
    pub default_key: Option<KeyboardShortcut>,
}

const fn sc(mods: Modifiers, key: Key) -> Option<KeyboardShortcut> {
    Some(KeyboardShortcut::new(mods, key))
}
const CTRL: Modifiers = Modifiers::COMMAND;
const CTRL_SHIFT: Modifiers = Modifiers { alt: false, ctrl: false, shift: true, mac_cmd: false, command: true };
const SHIFT_ALT: Modifiers = Modifiers { alt: true, ctrl: false, shift: true, mac_cmd: false, command: false };
const ALT: Modifiers = Modifiers::ALT;
const NONE: Modifiers = Modifiers::NONE;

pub static COMMANDS: &[CommandInfo] = &[
    CommandInfo { cmd: Command::NewQuery, id: "file.new_query", label: "New Query", category: Category::File, default_key: sc(CTRL, Key::N) },
    CommandInfo { cmd: Command::OpenFile, id: "file.open", label: "Open File…", category: Category::File, default_key: sc(CTRL, Key::O) },
    CommandInfo { cmd: Command::SaveFile, id: "file.save", label: "Save", category: Category::File, default_key: sc(CTRL, Key::S) },
    CommandInfo { cmd: Command::SaveFileAs, id: "file.save_as", label: "Save As…", category: Category::File, default_key: sc(CTRL_SHIFT, Key::S) },
    CommandInfo { cmd: Command::CloseTab, id: "file.close_tab", label: "Close Tab", category: Category::File, default_key: sc(CTRL, Key::W) },
    CommandInfo { cmd: Command::ReopenClosedTab, id: "file.reopen_closed", label: "Reopen Closed Tab", category: Category::File, default_key: sc(CTRL_SHIFT, Key::T) },
    CommandInfo { cmd: Command::NextTab, id: "file.next_tab", label: "Next Tab", category: Category::File, default_key: sc(CTRL, Key::Tab) },
    CommandInfo { cmd: Command::PrevTab, id: "file.prev_tab", label: "Previous Tab", category: Category::File, default_key: sc(CTRL_SHIFT, Key::Tab) },
    CommandInfo { cmd: Command::ImportAdsSettings, id: "file.import_ads", label: "Import Azure Data Studio Connections…", category: Category::File, default_key: None },
    CommandInfo { cmd: Command::Quit, id: "file.quit", label: "Quit", category: Category::File, default_key: sc(CTRL, Key::Q) },
    CommandInfo { cmd: Command::NewConnection, id: "connection.new", label: "New Connection…", category: Category::Connection, default_key: sc(CTRL_SHIFT, Key::N) },
    CommandInfo { cmd: Command::ConnectTab, id: "connection.connect", label: "Connect", category: Category::Connection, default_key: None },
    CommandInfo { cmd: Command::DisconnectTab, id: "connection.disconnect", label: "Disconnect", category: Category::Connection, default_key: None },
    CommandInfo { cmd: Command::ChangeConnection, id: "connection.change", label: "Change Connection…", category: Category::Connection, default_key: None },
    CommandInfo { cmd: Command::RefreshTree, id: "connection.refresh_tree", label: "Refresh Object Explorer", category: Category::Connection, default_key: None },
    CommandInfo { cmd: Command::RefreshIntelliSense, id: "connection.refresh_intellisense", label: "Refresh IntelliSense Cache", category: Category::Connection, default_key: sc(NONE, Key::F7) },
    CommandInfo { cmd: Command::RunQuery, id: "query.run", label: "Run Query", category: Category::Query, default_key: sc(NONE, Key::F5) },
    CommandInfo { cmd: Command::RunCurrentStatement, id: "query.run_current", label: "Run Current Statement", category: Category::Query, default_key: sc(CTRL, Key::Enter) },
    CommandInfo { cmd: Command::CancelQuery, id: "query.cancel", label: "Cancel Query", category: Category::Query, default_key: sc(ALT, Key::Pause) },
    CommandInfo { cmd: Command::EstimatedPlan, id: "query.estimated_plan", label: "Display Estimated Execution Plan", category: Category::Query, default_key: sc(CTRL, Key::L) },
    CommandInfo { cmd: Command::ToggleActualPlan, id: "query.actual_plan", label: "Include Actual Execution Plan", category: Category::Query, default_key: sc(CTRL, Key::M) },
    CommandInfo { cmd: Command::ParseQuery, id: "query.parse", label: "Parse Query", category: Category::Query, default_key: sc(SHIFT_ALT, Key::P) },
    CommandInfo { cmd: Command::FormatDocument, id: "query.format", label: "Format Document", category: Category::Query, default_key: sc(SHIFT_ALT, Key::F) },
    CommandInfo { cmd: Command::ExecutionOptions, id: "query.options", label: "Execution Options…", category: Category::Query, default_key: None },
    CommandInfo { cmd: Command::Find, id: "editor.find", label: "Find", category: Category::Editor, default_key: sc(CTRL, Key::F) },
    CommandInfo { cmd: Command::Replace, id: "editor.replace", label: "Replace", category: Category::Editor, default_key: sc(CTRL, Key::H) },
    CommandInfo { cmd: Command::GoToLine, id: "editor.goto_line", label: "Go to Line…", category: Category::Editor, default_key: sc(CTRL, Key::G) },
    CommandInfo { cmd: Command::ToggleLineComment, id: "editor.toggle_comment", label: "Toggle Line Comment", category: Category::Editor, default_key: sc(CTRL, Key::Slash) },
    CommandInfo { cmd: Command::ToggleBlockComment, id: "editor.toggle_block_comment", label: "Toggle Block Comment", category: Category::Editor, default_key: sc(SHIFT_ALT, Key::A) },
    CommandInfo { cmd: Command::TriggerCompletion, id: "editor.complete", label: "Trigger Suggest", category: Category::Editor, default_key: sc(CTRL, Key::Space) },
    CommandInfo { cmd: Command::UppercaseKeywords, id: "editor.uppercase_keywords", label: "Uppercase Keywords", category: Category::Editor, default_key: None },
    CommandInfo { cmd: Command::ToggleResults, id: "results.toggle", label: "Toggle Query Results", category: Category::Results, default_key: sc(CTRL_SHIFT, Key::R) },
    CommandInfo { cmd: Command::FocusEditorOrResults, id: "results.focus_toggle", label: "Toggle Focus Between Query and Results", category: Category::Results, default_key: sc(CTRL_SHIFT, Key::F) },
    CommandInfo { cmd: Command::CopySelection, id: "results.copy", label: "Copy", category: Category::Results, default_key: None },
    CommandInfo { cmd: Command::CopyWithHeaders, id: "results.copy_with_headers", label: "Copy With Headers", category: Category::Results, default_key: sc(CTRL_SHIFT, Key::C) },
    CommandInfo { cmd: Command::CopyHeaders, id: "results.copy_headers", label: "Copy Headers", category: Category::Results, default_key: sc(CTRL_SHIFT, Key::H) },
    CommandInfo { cmd: Command::CopyAsMarkdown, id: "results.copy_markdown", label: "Copy as Markdown Table", category: Category::Results, default_key: None },
    CommandInfo { cmd: Command::CopyAsJson, id: "results.copy_json", label: "Copy as JSON", category: Category::Results, default_key: None },
    CommandInfo { cmd: Command::CopyAsCsv, id: "results.copy_csv", label: "Copy as CSV", category: Category::Results, default_key: None },
    CommandInfo { cmd: Command::CopyAsInsert, id: "results.copy_insert", label: "Copy as INSERT Statements", category: Category::Results, default_key: None },
    CommandInfo { cmd: Command::CopyAsInList, id: "results.copy_in_list", label: "Copy as IN List", category: Category::Results, default_key: None },
    CommandInfo { cmd: Command::SelectAllCells, id: "results.select_all", label: "Select All Cells", category: Category::Results, default_key: None },
    CommandInfo { cmd: Command::FindInResults, id: "results.find", label: "Find in Results", category: Category::Results, default_key: None },
    CommandInfo { cmd: Command::SaveResultsCsv, id: "results.save_csv", label: "Save Results as CSV…", category: Category::Results, default_key: None },
    CommandInfo { cmd: Command::SaveResultsExcel, id: "results.save_excel", label: "Save Results as Excel…", category: Category::Results, default_key: None },
    CommandInfo { cmd: Command::SaveResultsJson, id: "results.save_json", label: "Save Results as JSON…", category: Category::Results, default_key: None },
    CommandInfo { cmd: Command::SaveResultsXml, id: "results.save_xml", label: "Save Results as XML…", category: Category::Results, default_key: None },
    CommandInfo { cmd: Command::SaveResultsMarkdown, id: "results.save_markdown", label: "Save Results as Markdown…", category: Category::Results, default_key: None },
    CommandInfo { cmd: Command::SaveResultsParquet, id: "results.save_parquet", label: "Save Results as Parquet…", category: Category::Results, default_key: None },
    CommandInfo { cmd: Command::SaveResultsArrow, id: "results.save_arrow", label: "Save Results as Arrow…", category: Category::Results, default_key: None },
    CommandInfo { cmd: Command::SaveResultsDelta, id: "results.save_delta", label: "Save Results as Delta Table…", category: Category::Results, default_key: None },
    CommandInfo { cmd: Command::MaximizeResultSet, id: "results.maximize", label: "Maximize / Restore Result Set", category: Category::Results, default_key: None },
    CommandInfo { cmd: Command::OpenCellViewer, id: "results.cell_viewer", label: "Open Cell in Viewer", category: Category::Results, default_key: None },
    CommandInfo { cmd: Command::ClearFilters, id: "results.clear_filters", label: "Clear All Filters and Sorts", category: Category::Results, default_key: None },
    CommandInfo { cmd: Command::OpenPlanFile, id: "plan.open", label: "Open Execution Plan File…", category: Category::Plan, default_key: None },
    CommandInfo { cmd: Command::SavePlanFile, id: "plan.save", label: "Save Execution Plan As…", category: Category::Plan, default_key: None },
    CommandInfo { cmd: Command::ShowPlanXml, id: "plan.show_xml", label: "Show Plan XML", category: Category::Plan, default_key: None },
    CommandInfo { cmd: Command::PlanZoomIn, id: "plan.zoom_in", label: "Plan: Zoom In", category: Category::Plan, default_key: None },
    CommandInfo { cmd: Command::PlanZoomOut, id: "plan.zoom_out", label: "Plan: Zoom Out", category: Category::Plan, default_key: None },
    CommandInfo { cmd: Command::PlanZoomFit, id: "plan.zoom_fit", label: "Plan: Zoom to Fit", category: Category::Plan, default_key: None },
    CommandInfo { cmd: Command::CommandPalette, id: "view.palette", label: "Command Palette…", category: Category::View, default_key: sc(CTRL_SHIFT, Key::P) },
    CommandInfo { cmd: Command::ToggleSidebar, id: "view.toggle_sidebar", label: "Toggle Sidebar", category: Category::View, default_key: sc(CTRL, Key::B) },
    CommandInfo { cmd: Command::ShowServers, id: "view.servers", label: "Show Servers", category: Category::View, default_key: sc(CTRL_SHIFT, Key::E) },
    CommandInfo { cmd: Command::ShowHistory, id: "view.history", label: "Show Query History", category: Category::View, default_key: sc(CTRL_SHIFT, Key::Y) },
    CommandInfo { cmd: Command::ShowSettings, id: "view.settings", label: "Settings", category: Category::View, default_key: sc(CTRL, Key::Comma) },
    CommandInfo { cmd: Command::ToggleTheme, id: "view.toggle_theme", label: "Toggle Light / Dark Theme", category: Category::View, default_key: None },
    CommandInfo { cmd: Command::ZoomIn, id: "view.zoom_in", label: "Zoom In", category: Category::View, default_key: sc(CTRL, Key::Equals) },
    CommandInfo { cmd: Command::ZoomOut, id: "view.zoom_out", label: "Zoom Out", category: Category::View, default_key: sc(CTRL, Key::Minus) },
    CommandInfo { cmd: Command::ZoomReset, id: "view.zoom_reset", label: "Reset Zoom", category: Category::View, default_key: sc(CTRL, Key::Num0) },
    CommandInfo { cmd: Command::About, id: "help.about", label: "About Cobalt SQL Works", category: Category::Help, default_key: None },
    CommandInfo { cmd: Command::KeyboardShortcuts, id: "help.shortcuts", label: "Keyboard Shortcuts", category: Category::Help, default_key: None },
];

pub fn info(cmd: Command) -> &'static CommandInfo {
    COMMANDS.iter().find(|c| c.cmd == cmd).expect("every Command has a CommandInfo")
}

/// Active key bindings (defaults + user overrides).
pub struct Keymap {
    bindings: HashMap<Command, KeyboardShortcut>,
}

impl Default for Keymap {
    fn default() -> Self {
        let mut bindings = HashMap::new();
        for c in COMMANDS {
            if let Some(k) = c.default_key {
                bindings.insert(c.cmd, k);
            }
        }
        // F1 also opens the palette; Ctrl+E runs (SSMS habit); Ctrl+F5 runs current statement (ADS habit).
        Self { bindings }
    }
}

impl Keymap {
    pub fn shortcut(&self, cmd: Command) -> Option<KeyboardShortcut> {
        self.bindings.get(&cmd).copied()
    }

    pub fn shortcut_text(&self, ctx: &egui::Context, cmd: Command) -> String {
        self.shortcut(cmd).map(|s| ctx.format_shortcut(&s)).unwrap_or_default()
    }

    /// Extra aliases that map to the same command as a primary binding.
    fn aliases() -> &'static [(KeyboardShortcut, Command)] {
        static ALIASES: &[(KeyboardShortcut, Command)] = &[
            (KeyboardShortcut::new(NONE, Key::F1), Command::CommandPalette),
            (KeyboardShortcut::new(CTRL, Key::E), Command::RunQuery),
            (KeyboardShortcut::new(CTRL, Key::F5), Command::RunCurrentStatement),
            (KeyboardShortcut::new(CTRL, Key::P), Command::CommandPalette),
            (KeyboardShortcut::new(CTRL, Key::Plus), Command::ZoomIn),
        ];
        ALIASES
    }

    /// Consume pressed shortcuts this frame. `editor_focused` suppresses bindings that would fight
    /// plain typing (none of ours do, but Ctrl+Space etc. are only meaningful there).
    pub fn consume(&self, ctx: &egui::Context) -> Vec<Command> {
        let mut out = Vec::new();
        ctx.input_mut(|i| {
            for (cmd, sc) in &self.bindings {
                if i.consume_shortcut(sc) {
                    out.push(*cmd);
                }
            }
            for (sc, cmd) in Self::aliases() {
                if i.consume_shortcut(sc) {
                    out.push(*cmd);
                }
            }
        });
        out
    }
}
