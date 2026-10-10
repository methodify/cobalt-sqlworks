# Backlog

Items the founder has parked for after the current epoch, with what is known about each. Design
docs under `docs/design/` carry the larger arcs; this list is for defects and smaller asks.

## Defects

- **Results find stops after ~200,000 cells (2026-10-09).** `ui/results/mod.rs` scans the
  result set on the UI thread with a hard budget of 200,000 cells and stops silently when it runs
  out: on a 29-column, 1,342,769-row result a search for `.com` reported matches only up to row
  ~6,964, with no sign that the rest was never searched. Fix shape: run the scan on a background
  thread over the chunk store (cancellable, generation-checked like the sort/filter views),
  deliver matches incrementally with a status of "n matches so far, scanning…", and keep the
  first result instant by scanning forward from the current row for Next / Previous; the UI
  must never claim a count it has not finished. Until then the status should at least say
  "first 200,000 cells searched".

## Improvements

- **The Fabric table API for the JVM engine's listings** — done on Cobalt's side 2026-10-10
  (`cobalt_fabric::TableApiClient`; the pane and completion read it with the DFS crawl as the
  fallback; see `docs/design/lakesail_runtime.md` §5). Open: the JVM worker's own listing, asked
  of local-spark-mcp in `docs/requests/local-spark-mcp-0.8.2-request.md`.
