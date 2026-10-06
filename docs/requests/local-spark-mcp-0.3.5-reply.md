# Cobalt's reply to the local-spark-mcp 0.3.5 response

*From Cobalt SQL Works, 2026-10-06. Follows `local-spark-mcp.md` (the original nine requests) and
the team's response that shipped items 1, 5, 7, 8, 9 and preload in 0.3.5 and scheduled 2, 3, 4,
6 for 0.4.0. Three parts: what Cobalt did with 0.3.5, answers to the open questions for 0.4.0,
and two new asks found while adopting it (items 5 and 6, both about preload on a schema-enabled
lakehouse).*

Thank you — 0.3.5 covers more than we asked. What Cobalt does with it:

- **Pinned `v0.3.5`.** `init` now passes `profile`; `fatal: true` already respawns the worker.
- **Item 8 replaces our workaround.** User jars go in `extra_jars` and Maven coordinates in
  `extra_packages`; Cobalt no longer puts jars on `extraClassPath` or downloads single artifacts
  from Maven Central. Transitive resolution through Ivy is exactly what we wanted.
- **Item 7 (per-table discard)** is wired to the Discard button in the Shadows window.
- **Item 9 (`healthcheck`)** runs on the Settings → Spark runtime page (`python -m
  local_spark_mcp.healthcheck --json`) and its problems/warnings show there; `info`'s new paths
  will go on the session log header.
- **Preload** is a per-notebook option ("Preload the default lakehouse's tables at session
  start"). We first wired it to `preload: [<default lakehouse>]` in `init`, but it cannot work
  from a host today (item 5 below), so Cobalt lists the tables itself and calls `mount_tables`.
  The MCP tool/config side we do not use.
- **Item 1 (`profiles.json`)**: Cobalt keeps an embedded copy for offline installs and will
  compare it against the tag's file in CI. `python_windows` for both profiles understood.
- **Item 5 (`warm`)**: our smoke test already starts a session through the protocol; we may switch
  the install step to `warm --ivy <dir> --java-home <jdk>` since it reports Ivy progress.

Answers to the open questions for 0.4.0:

1. **Interrupt before Arrow.** Cobalt already has Arrow results through a display hook that writes
   IPC files, so the visible gap today is Stop killing the session. Interrupt first, please.
2. **Second TCP socket** for the control channel, port advertised in the `init` reply. Our worker
   client is a simple request loop on one stream; a second socket keeps it untouched and lets the
   interrupt go out while a reply is pending. Suggested frame on that socket: `{"id", "method":
   "interrupt"}` → `{"id", "ok", "result": {"state": "interrupting"|"idle"}}`, and the pending
   `run_code` reply then carries `"interrupted": true` with whatever stdout was produced.
3. For the Arrow frame (protocol v2), a frame-type byte before the length is fine; please keep
   `protocol_version` in `init` so Cobalt can speak v1 to an older environment during the switch.
4. Streaming stdout events: the same socket as the reply is fine for us; we read frames until the
   one whose `id` matches the request and treat `event` frames as progress.

Found while adopting 0.3.5:

5. **Preload needs a credential the host does not give the worker.** The preload thread lists
   tables through `FabricAPIClient(credential=self._cred)` → `DefaultAzureCredential`, which fails
   in Cobalt's process model (tokens only reach the JVM through the HTTP token endpoint), so
   `preload: [...]` reported "preloaded 0 tables" and the credential chain's error text landed in
   the first user cell's output. Cobalt now drives the preload itself: it lists `Tables/` on
   OneLake with its own storage token and sends `mount_tables` in chunks of 8 so cells interleave.
   Two asks, either would let us hand this back to the worker: (a) `preload` accepting explicit
   table lists (`{"lakehouse": ["t1", "t2"]}`) so no REST call is needed in the worker, or (b) a
   token endpoint parameter for the Fabric API scope as well (the JVM already has one for
   storage), so `FabricAPIClient` can run on host-supplied tokens — that would also unblock
   `sync_files` and notebook discovery under a host. Also: a credential error inside the preload
   thread should not surface as a notice on an unrelated user cell.

6. **Schema-enabled lakehouses.** `FabricAPIClient.list_tables` uses
   `GET /workspaces/{ws}/lakehouses/{id}/tables`, which Fabric refuses for schema-enabled
   lakehouses (`400 UnsupportedOperationForSchemasEnabledLakehouse`) — so the worker's own
   preload fails there too (our test lakehouse is one). On storage such a lakehouse has
   `Tables/<schema>/<table>/_delta_log` (and may still have `Tables/<table>` for tables written
   by other tools; both coexist in ours). `OneLakeCatalog` only resolves `Tables/<table>`:
   `SHOW TABLES IN test` is empty, `test.dbo.publicholidays` fails with
   `REQUIRES_SINGLE_PART_NAMESPACE`, and no quoting reaches `dbo/publicholidays`. Ask: let the
   catalog list OneLake (`Tables/`, one level down for schema folders) instead of the REST
   endpoint, and expose schema tables — either as multi-part namespaces (`test.dbo.publicholidays`,
   the Fabric notebook spelling) or at least as `test.dbo_publicholidays`. Cobalt's preload
   mounts the top-level tables and reports the schema-folder ones as unreachable for now.
