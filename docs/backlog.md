# Backlog

Items the founder has parked for after the current epoch, with what is known about each. Design
docs under `docs/design/` carry the larger arcs; this list is for defects and smaller asks.

## Defects

- **Results find stops after ~200,000 cells (2026-10-09) — fixed 2026-10-10.** The scan now
  runs on a thread over the whole visible set in blocks of about 50,000 cells, matches arrive as
  they are found (the first one selects and scrolls at once), the status says "n of m · scanning
  k%" until every cell was looked at, and "No matches" only then; a new text, option or view
  stops the running scan and starts another (`spawn_find_scan` in `ui/results/mod.rs`).

## Improvements

- **The Fabric table API for the JVM engine's listings** — done on Cobalt's side 2026-10-10
  (`cobalt_fabric::TableApiClient`; the pane and completion read it with the DFS crawl as the
  fallback; see `docs/design/lakesail_runtime.md` §5). Open: the JVM worker's own listing, asked
  of local-spark-mcp in `docs/requests/local-spark-mcp-0.8.2-request.md`.
