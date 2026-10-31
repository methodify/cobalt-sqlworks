# The Files sidebar: every row does what a drop does

*Status: designed and built, shipped in 0.15.3 (2026-10-30), from the founder's note
that the Files tab "is in-actionable": a folder opens, its items show, and nothing can be done
with them. Companion to `docs/design/local_query.md` (file tables) and `docs/design/file_databases.md`
(SQLite / DuckDB connections).*

## 1. What was wrong

The app already knows what to do with a file. Dropping one on the window runs it through
`drop::classify` (folder, Delta folder, dashboard, Explore spec, SQLite or DuckDB by header,
plan, notebook, Fabric `.py`, data files, Excel workbooks, a query in one of four dialects,
connection exports) and `drop::drop_files` opens it the right way: a Local tab over a CSV, a
connection on a database file, the notebook editor, the plan viewer, the import dialog for a
workbook. The Files sidebar used none of it. A click read the file as text into a SQL tab
(so a CSV became a wall of commas and a Parquet file a "could not open" toast), the listing hid
database files, workbooks and most Arrow spellings, and the context menu offered Open, Reveal
and Copy path. Only a drag out of the sidebar onto an editor group reached the classifier.

## 2. The principle

**One classifier, one set of verbs.** A row in the Files tab is the same thing as a dropped
file: it shows what Cobalt sees it as, and opening it does what a drop does. The sidebar adds
nothing the drop path lacks; when a new kind of file becomes openable, both learn it at once.

## 3. The shape

**Listing.** Every file whose kind Cobalt recognises is listed, with the kind as the row's
detail text: *SQL*, *Spark SQL*, *DAX*, *notebook*, *Python*, *plan*, *dashboard*, *Explore*,
*CSV* / *TSV* / *Parquet* / *Arrow* / *JSON lines* / *JSON*, *Excel*, *SQLite database*, *DuckDB
database*, *text* (`.md`, `.toml`, `.xml`). Folders holding a `_delta_log` show as *Delta table*.
Kinds come from the name, the extension and, for database files, a 16-byte header; nothing is
read in full to draw the list. An eye toggle in the header lists every other file too (greyed;
they open as text). Hidden names (a leading dot) stay hidden.

**Opening.** Click opens as a preview tab, double-click keeps the tab, exactly as before, but
the opening is `drop::drop_files` for everything that is not text: a data file becomes a local
table in a Local tab, a database file a connection, a notebook the notebook editor, a plan the
plan viewer, a workbook the import dialog on the active connection. Query files keep the preview
tab behaviour (SQL in a query tab; Spark SQL and DAX files open in their own tab kinds). *Open
with Ctrl+O* goes through the same door, so a CSV picked there is a local table too.

**The context menu** says what the row is and offers its verbs:

| Kind | Verbs |
|---|---|
| SQL / Spark SQL / DAX / text | Open · Open in a new tab (no preview) · Open as text |
| CSV, TSV, Parquet, Arrow, JSON lines, Delta table | Query as a local table · Import into a table on the active connection… |
| Excel workbook | Import into a table on the active connection… |
| SQLite / DuckDB database | Open as a connection |
| Notebook / Python | Open the notebook (Python: as a one-cell PySpark notebook) · Open as text |
| Plan / dashboard / Explore | Open · Open as text |
| Folder | Open as the root · New .sql file here… · New notebook here… |

Every row also has *Reveal in file manager* and *Copy path*. The header gains an *Up* button
(the parent folder becomes the root) next to Open folder, Refresh, New notebook and New .sql.

**Not in scope.** File management (rename, delete, move) stays with the OS: Cobalt opens
files, it does not curate them. Keyboard navigation of the tree and a recent-files list are
later. A folder watcher (automatic refresh) is later; Refresh stays.

## 4. Where it lives

- `drop::classify_shallow(path)`: the classifier without content reads (the list), returning
  the same `DropAction`; `drop::classify` (with sniffing) decides on open.
- `ops::open_file_as`: text kinds take the preview path, everything else `drop_files`.
- `drop::import_into_active(state, cx, path)`: the import dialog on the active connected tab
  (a new query tab on the active profile when there is none), shared by drops and the menu.
- `ui/files.rs`: the kind per row (`FileKind::of`), icons, details, the menus; actions
  `Open`, `OpenText`, `Query`, `Import`, `SetRoot`, `NewFile`, `NewNotebook`, `Reveal`.
- `FilesState.show_all` (not persisted).
- Agent: `files_root {path}` as before; the rows' labels stay `file: name` / `folder: name`,
  the detail text carries the kind.
