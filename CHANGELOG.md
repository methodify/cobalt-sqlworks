# Changelog

## 0.3.0 — 2026-09-21

- **Run to File** (Query menu, toolbar, Ctrl+Shift+F5): run a query straight into a file or a
  OneLake lakehouse without filling the grid. Every result set streams from the wire to the
  target as it arrives (CSV, TSV, JSON, JSON Lines, XML, Markdown, Excel, Parquet, Arrow, Delta;
  local file or lakehouse Delta table / Files upload), with back-pressure so memory stays flat.
  The grid keeps a 1,000-row preview of each set, the status bar shows rows written, the outcome
  lands in Messages, and cancelling the query discards the partial output. A second result set
  gets a `_2` suffix, and so on.
- Results grid: **View row as record** (context menu) opens the viewer in Record mode — every
  column of the row as name/value lines with previous/next row, copy the row as JSON, and click a
  value to open it. The cell viewer has a Record toggle too. Made for wide Fabric tables.
- File menu: **Export Connections…** / **Import Connections…** — the connection library as JSON
  (groups and connections; never passwords or secrets), merged on import.
- macOS: the app bundle is now ad-hoc code-signed and the dmg built with `hdiutil`. Apple Silicon
  refused the unsigned 0.2.2 bundle as "damaged"; it now shows the standard unidentified-developer
  prompt (right-click → Open) until the app is notarized. The fixed dmg was re-uploaded to v0.2.2.
- Dev: `run_to_export` and `library` agent verbs.

## 0.2.2 — 2026-09-18

- Results grid: **click-and-drag selects a range** again. Pressing a cell selects it at once and
  dragging extends from it; previously a press flashed a range from the old cell, then collapsed to
  a single cell and ignored the drag. Shift+click extends as before.
- Results grid: the column **filter popup opens beside its column header** instead of the window's
  top-left corner (and stays where you drag it afterwards).
- Results: **Maximize** now gives the result set the whole tab by hiding the editor; the same button,
  the grid context menu, or Escape in the grid restores it. Before, with one result set it did nothing.
- Results grid: new context-menu action **Filter to selected values** — every column the selection
  spans gets an "in (…)" filter of the selected cells' values (other filters and sorts are kept).
- Results: a filtered result set's header reads "n of m rows".
- Fabric explorer: expanding an item now shows **that item's database** (Tables, Views,
  Programmability, Schemas…) directly. Previously the inline explorer treated the item like a
  server and listed every database on the workspace's shared SQL endpoint, duplicating the
  workspace's own item list one level down. "Refresh objects" added to the item context menu.
- Fabric explorer: a SQL database's analytics-endpoint child row appears when the database is
  expanded, rather than always.
- Fabric explorer: pinned and recent items resolve on sign-in (their workspaces' items load
  eagerly) instead of reading "not found" until you expand the workspace.
- Export to OneLake: the dialog **remembers the last lakehouse** you exported to.
- Dev: `pointer` agent verb (click / right-click / double-click / drag in screenshot pixels) fed
  through eframe's raw-input hook, so agent runs can exercise real mouse interaction.

## 0.2.1 — 2026-09-18

- Fabric explorer: chevron on an item opens an **inline object explorer** (databases → tables →
  columns/keys/indexes, same tree as Servers) without leaving the panel.
- Fabric explorer: **Recent** section (last six items you opened) above Pinned.
- Fabric explorer: a SQL database's **SQL analytics endpoint** appears as a child row and opens
  against the workspace's warehouse host.
- Fabric explorer: capacity SKU next to the region when the registration grants
  `Capacity.Read.All` (silently skipped otherwise).
- Lakehouse context menu: **Export results here…** opens Save-results with that lakehouse
  preselected (Delta, schema pre-filled).

## 0.2.0 — 2026-09-18

- **Fabric explorer**: a Fabric icon on the left rail (Ctrl+Shift+B). Sign in once with your Entra
  account (an existing Entra connection is adopted silently), browse every workspace you can reach
  and its warehouses, lakehouse SQL endpoints, SQL databases and mirrored databases. Double-click
  opens a connected query tab, no saved connection needed. Pin favourites, search, *Save to
  Servers*, copy the connection string, open in the Fabric portal.
- **Export to OneLake**: the Save-results dialog can target a lakehouse. Delta tables land in
  `Tables/` (queryable from the lakehouse SQL endpoint right away); Parquet, CSV, Excel and the other
  formats land in `Files/`. Schema-enabled lakehouses are detected (`defaultSchema`) and Delta
  tables go under `Tables/<schema>/`, defaulting to the lakehouse's schema.
- Entra: one refresh token now serves SQL, the Fabric REST API and OneLake once the app
  registration's permissions are consented to (Power BI Service → Workspace.Read.All,
  Item.Read.All, OneLake.ReadWrite.All; optionally Azure Storage → user_impersonation).
- Agent verbs: `fabric_state`, `fabric {action}`, `export {lakehouse, name}`.

## 0.1.1 — 2026-09-18

- Check for updates: Help → Check for Updates…, plus a once-per-start check (Settings → Updates to turn
  it off or stop skipping a version).
- Database switcher: the toolbar combo is always a switcher when connected. A failed list keeps the
  current database and offers *Refresh list* (with the error on hover); engines without `USE` switch
  by reconnecting.
- The tab follows a `USE` run from the editor (title, status bar, completion catalog).
- Fabric Warehouse's per-statement "Statement ID / Query hash" info lines are shown muted, without
  the Msg/Level header.
- Release builds no longer contain the dev-only egui_agent channel.
- Build: faster release workflow (prebuilt packager), Linux-only CI, macOS dmg named without spaces.

## 0.1.0 — 2026-09-17

First release. Connection library, object explorer, editor with completion, streaming results grid,
exports (CSV/Excel/JSON/XML/Markdown/Parquet/Arrow/Delta), estimated and actual plans, history,
light/dark, Entra ID sign-in to Fabric Warehouse, SQL analytics endpoints and SQL database in Fabric,
Windows integrated auth. Installers for Windows and Linux; experimental macOS dmg.
