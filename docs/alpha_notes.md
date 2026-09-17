# Cobalt SQL Works — V1 alpha notes for the founder

*Written during the build, 2026-09-17. Update as the alpha evolves.*

## Run it

```
cargo run -p cobalt-app --release            # or: target\release\cobalt.exe
COBALT_AGENT=1 cargo run -p cobalt-app       # dev build with the agent channel on (pipe: cobalt.agent)
```

Data lives in `%APPDATA%\Cobalt\Cobalt SQL Works\` (Windows): `config\settings.toml`,
`data\cobalt.db` (library, history, tab snapshots). Passwords go to Windows Credential Manager
under service `cobalt-sqlworks`. Spill files go to the temp dir and are cleaned on startup.

## What to try first

1. **Servers → +** — add your SQL Server (SQL login or Windows auth). "Test connection", then
   "Save & Connect". Expand the server in the tree: databases → Tables → columns/keys/indexes.
2. **F5** runs the tab (selection if any); **Ctrl+Enter** runs the statement under the cursor;
   **Ctrl+L** estimated plan; **Ctrl+M** toggles actual plan for subsequent runs.
3. Big result: `SELECT * FROM <big table>` stops at 10,000 rows with a **Fetch more / Fetch all**
   bar. 2M rows fetch in ~4 s locally; sort/filter work on the full set.
4. Right-click the grid: Copy / with headers / Markdown / JSON / INSERT / IN list;
   **Save results as…** → CSV, Excel, JSON, XML, Markdown, **Parquet, Arrow, Delta**.
5. Double-click a cell (or Enter) → cell viewer (pretty JSON / XML / hex).
6. **History** (left strip) — every run, searchable; double-click reopens.
7. **Ctrl+Shift+P** — command palette. **File → Import Azure Data Studio connections**.

## Entra ID / Fabric — ready for you to complete the first sign-in

Your app registration (`ecec63e7-…`) ships as Cobalt's default client ID (Settings → Connections
overrides it). A profile named **fabric** pointing at your warehouse endpoint, tenant preset, is
already in your library. The interactive flow was exercised end to end up to the human step: the
app opens the loopback listener, launches Chrome to the Microsoft sign-in page, and waits (5 min
timeout, clean failure message). To finish the test: expand **fabric** in Servers or open a query
tab on it, complete the login in the browser, then run `SELECT TOP 100 * FROM <your table>`.
If your tenant's conditional access rejects a public client, switch the profile's auth to
**device code** or **Azure CLI**. What the registration must have: mobile/desktop platform with
redirect URI `http://localhost`, "Allow public client flows" = Yes, delegated
`Azure SQL Database / user_impersonation` (admin consent may be required).

## Known gaps in this alpha

- Windows Integrated auth is implemented (SSPI via the driver) but untested here (Docker SQL Server = SQL auth only).
- Object-explorer filter dialog, group-by-schema, freeze columns, transposed view: V1.x.
- Multi-cursor / folding in the editor: V2.
- Plan comparison and Plan-Explorer-class analysis: V2.
- The agent's `snapshot` sometimes returns only the root node between interactions (egui_agent/AccessKit incremental tree); clicks by label still work. Worth a look in egui_agent.
- macOS: untested. Linux: built in Docker and runs under WSLg (X11); no Secret Service there, so passwords fall back to a plain file with a warning.
- Release binary is 46 MB and cold-starts in ≈1.7 s (target was <1 s / <40 MB); arrow + delta-rs + wgpu are the bulk. Worth a size pass later.
- The `all_types` seed row 'long nvarchar max ✓ text' shows `?` because the seed literal lacked an `N` prefix — a seed bug, not a driver bug.

## V1 acceptance checklist — status at alpha (from `product_design.md` §8)

| Item | Status |
|---|---|
| Cold start < 1 s, binary < 40 MB | ~1.7 s to agent-responsive, 46 MB — close, not met; size pass later |
| Group/profile with SQL auth, connect, lazy tree | ✅ verified via agent |
| Entra interactive login to Fabric | ⏳ flow verified to the browser; needs you to complete the sign-in |
| `az login` credential | implemented; no `az` on the build box |
| New Query, highlighting, completion, F5, Ctrl+Enter, cancel | ✅ (completion + cancel verified; keys need a human) |
| 5M-row streaming with cap, fetch-all, spill, sort/filter | ✅ 2M rows in ~4 s; spill covered by unit tests |
| Multiple result sets, PRINT, clickable errors, rows affected | ✅ |
| Copy / with headers / Markdown / JSON / INSERT | ✅ Markdown verified in clipboard; others share the builder |
| Save as CSV/Excel/JSON/XML/Markdown/Parquet/Arrow/Delta | ✅ all produced; round-trips in tests; Delta needs your Fabric read check |
| JSON/XML cell viewer, 1 MB values in full | ✅ |
| Estimated/actual plan graph, properties, top ops, .sqlplan | ✅ |
| History records, search, restore closed tab | ✅ |
| Light/dark complete, follows OS | ✅ |
| Palette with shortcuts; ADS keys | ✅ palette; keys need a human |
| Hot exit | ✅ |
| Agent verbs via egui-agent-cli | ✅ (this is how everything above was tested) |
| Linux build runs the checklist | binary builds and runs under WSLg; checklist not exercised there |
