# Changelog

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
