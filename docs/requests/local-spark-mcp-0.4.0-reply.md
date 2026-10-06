# Cobalt's reply to local-spark-mcp 0.4.0

*From Cobalt SQL Works, 2026-10-06. Follows `local-spark-mcp.md` (the nine requests) and
`local-spark-mcp-0.3.5-reply.md`. Three parts: what Cobalt adopted from 0.4.0, what we found
while adopting it, and the asks for 0.4.1 and after.*

## Adopted (Cobalt 0.7.3)

- **Pinned `v0.4.0`.** The runtime manager upgrades an existing environment in place
  (`uv pip install` of the new pin, then `warm`).
- **Interrupt.** Cobalt spawns with `--port N --control-port M` when the installed package is
  0.4.0 or newer and keeps the control connection on a handle any thread can use. Stop on a
  running cell sends `interrupt`; the in-flight `run_code` comes back `interrupted: true`, the
  cell ends as cancelled with its stdout so far and "Interrupted (Spark jobs cancelled)", and the
  session survives. A second Stop while the first is pending kills the worker — the escape hatch
  for a cell blocked in pure Python on Windows, exactly the limit you documented.
- **Streaming.** `run_code` goes out with `stream: true`; `stdout`/`stderr` event frames land on
  the cell as they arrive and the final reply's complete `stdout` replaces them (our Arrow
  marker lines are filtered out of the live view).
- **Id check.** Both sockets reject a reply whose id is not the outstanding request's: data
  socket → fatal → respawn; control socket → a *later* id is fatal, an *earlier* one is treated
  as a late duplicate and skipped (see finding 1 for why that case exists).
- **Blobs.** A reply's `"binary": [sizes]` is read and kept in step with the socket. We do not
  ask for `arrow: true` or use the pre-bound `display()` yet (see ask 3).
- `init`'s `profile_warnings` go to the session log.

Verified on Windows (Python 3.11, fabric-2.0, Spark 4.1.1): a six-second print loop streamed one
line per second; a `spark.range(10**13)` aggregate was cancelled about five seconds after Stop
with the session intact and the next cell running normally; a `time.sleep(8)` cell returned
"Interrupted" when the sleep ended.

## Found while adopting

1. **The interrupt acknowledgement lags the interrupt by 10 s or more, and the cell's stderr
   fills with the cancel's own errors.** `SparkEngine.interrupt()` calls
   `self.spark.sparkContext.cancelAllJobs()` from the control thread while the cell's thread is
   blocked inside a py4j call; the result on our box is
   `ERROR:root:Exception while sending command. … RuntimeError: reentrant call inside
   <_io.BufferedReader …>` followed by `Py4JNetworkError` (twice), all of it on the cell's
   stderr, and the `interrupt` reply arriving well after the jobs were in fact cancelled (our
   10 s wait expired both times; the late reply then sat on the socket for the next request —
   hence the duplicate handling above). The interrupt itself works. Ask: issue the cancel from a
   connection the cell cannot be holding — a second `GatewayClient`/`JavaGateway` to the same
   gateway port and auth token, or a JVM-side cancel reached over the HTTP token/control
   endpoint — and reply as soon as `cancelAllJobs` returns (the watchdog thread can start
   after). Also: errors raised *by the cancellation* should not be written to the cell's
   stderr. Until then Cobalt waits 30 s for the acknowledgement and hides the stderr of an
   interrupted cell.
2. **`healthcheck --json` has no `protocol_version`** although `init`, `info` and
   `profiles.json` do; Cobalt would show it on the runtime page before any session starts.
3. **Arrow for bare expressions.** Cobalt still installs its own `display()` and an IPython
   pretty-printer hook so that a bare `df` (or pandas frame) as the cell's last expression opens
   in the grid, Fabric-style; both write Arrow IPC files and print a marker. The pre-bound
   `display(df)` with blobs covers only the explicit call. To retire the bootstrap we would need
   `run_code` to capture the cell's last-expression value when it is a Spark or pandas DataFrame
   (opt-in, e.g. `capture_result: true`) as a `displays` entry under the same row cap — then
   `displays` + blobs would be the only path and the output directory goes away.

## For 0.4.1 and after

4. **Schema-enabled lakehouses**: the V2 catalog with the Fabric spelling
   (`test.dbo.publicholidays`) is the one we want; please keep tables at `Tables/<t>` of the same
   lakehouse reachable as `test.<t>` (our test lakehouse has both layouts). `preload` with
   explicit table lists and the host-token route for `FabricAPIClient` as planned; Cobalt keeps
   its own OneLake-listing preload until then and will switch back when 0.4.1 is pinned.
5. **PyPI**: the trusted-publisher step is on Cobalt's side (Bryon); we will move the pin from
   the git tag to `local-spark-mcp[fabric-2.0]==x.y.z` once the first wheel is up.
6. A **`status`** call on the control socket returning the running cell's elapsed time and the
   count of active Spark jobs would let the Stop button say what it is stopping.
