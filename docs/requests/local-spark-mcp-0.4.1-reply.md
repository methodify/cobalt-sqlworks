# Cobalt's reply to local-spark-mcp 0.4.1

*From Cobalt SQL Works, 2026-10-06. Follows `local-spark-mcp.md`, `local-spark-mcp-0.3.5-reply.md`
and `local-spark-mcp-0.4.0-reply.md`. Everything in 0.4.1 is adopted in Cobalt 0.7.3. One
correction to our 0.4.0 reply comes first, because it changes what you should spend time on.*

## Correction: the interrupt acknowledgement lag was ours

Finding 1 of our 0.4.0 reply said the `interrupt` reply arrived 10 s or more after the jobs were
cancelled. It did not. Cobalt binds its listeners non-blocking for the accept loop and, on
Windows, an accepted socket inherits that mode; our control-socket read therefore returned
`WouldBlock` at once and we logged it as a timeout. The data socket had the same inheritance and
was busy-polling. Both accepted sockets are now switched back to blocking, and measured on the
same box with 0.4.1:

```
cobalt: interrupt → interrupting after 0.0 s (spark jobs cancelled in 0.02s)
acknowledgement after 0.031 s: {"interrupted": true, "state": "interrupting",
  "detail": "spark jobs cancelled in 0.02s", "method": "run_code", "elapsed_s": 8.6}
```

So there is no Windows-side acknowledgement problem to chase. The second half of that finding
stood — the py4j `reentrant call` errors on the interrupted cell's stderr came from pyspark's
SIGINT handler, as you diagnosed — and 0.4.1's handler replacement fixed it: an interrupted
cell's output here is now just what it printed plus "Interrupted (Spark jobs cancelled)". Sorry
for the detour.

## Adopted

- **Pinned `v0.4.1`.** Protocol 2 as before; the runtime manager upgrades in place (26 s here).
- **Host tokens (item 5b).** Cobalt's loopback endpoint answers `GET /token?scope=<scope>` with
  the same `X-Token-Secret`: `https://storage.azure.com/.default` (and no scope) → the OneLake
  storage token, `https://api.fabric.microsoft.com/.default` → the Fabric REST token, both minted
  silently from the signed-in account's refresh token. `https://vault.azure.net/.default` gets a
  404 with a one-line explanation (Cobalt holds no Key Vault consent), which surfaces in the call
  that needed it, as you intended.
- **Preload is yours again (items 5a, 6).** `init` carries `preload: [<default lakehouse>]` (or
  `["all"]`); the app-side OneLake listing and chunked `mount_tables` from our 0.3.5 reply are
  deleted. The Shadows window polls `preload_status` on the control socket every 3 s while it is
  open, so the bar moves while a cell runs.
- **`capture_result` (finding 3).** `run_code` goes out with `capture_result: true`; each
  `displays` entry maps onto the blob that follows and becomes a grid; for a `source: "result"`
  entry Cobalt drops the trailing `Out[n]:` repr from the cell's text. Our bootstrap display
  hook, the IPC files under the runtime folder and the marker lines are gone; the only helper
  Cobalt still installs is the `%%sql` runner, which splits statements and hands the last frame
  to your `display(df, limit)`. `init`'s `default_sql_limit` is the user's "Rows a Spark
  DataFrame brings back" setting. The legacy hook remains only for an environment not yet updated
  past 0.4.0, and the session log says so.
- **`healthcheck.protocol_version`** shows on the Spark runtime page.
- **`status` (ask 6)** is not used by the UI yet; it will feed the Stop button's tooltip.

Verified on Windows (Python 3.11, fabric-2.0, Spark 4.1.1), plain session: a bare
`spark.range(7).selectExpr(...)` → grid of 7 rows, `display(df, limit=3)` → 3 rows with the text
after it intact, a bare pandas frame → grid, a `%%sql` cell with two statements → grid from the
second, a bare `42` → `Out[n]: 42` kept as text; streamed output still arrives per line; a
`spark.range(10**13)` aggregate interrupted cleanly with the session intact and the next cell
running normally.

## Live output from the schema-enabled test lakehouse

Pending. The dev box's Fabric sign-in lapsed while the sockets above were being chased, and a
fresh sign-in needs the founder at the browser prompt; the `test` lakehouse here has both
layouts (`Tables/sales_import`, `Tables/r2e_stream`, `Tables/cobalt_nb_writethrough` at the top
level and `Tables/dbo/{cobalt_export_schema, cobalt_export_test, publicholidays, r2e_dialog,
r2e_stream2}`), so it is the right fixture for `test.dbo.publicholidays`, `SHOW TABLES IN
test.dbo`, `SHOW NAMESPACES IN test`, `test.sales_import` and the worker-side preload over host
tokens. The script is ready; the output will be appended here as soon as the sign-in is done.

## Asks

1. **`status` for the Stop button.** Already shipped as asked; no change needed. If `cell` could
   also carry the active Spark job's description (`spark.jobGroup`/`callSite.short`), the
   tooltip could say *what* is running, not only for how long.
2. **Blob-free `displays` for small frames** is not needed; the blob path is fine. No ask.
3. **PyPI**: trusted publisher on the PyPI side is still the founder's step; nothing for you.
