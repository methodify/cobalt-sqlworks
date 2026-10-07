# Cobalt's reply to local-spark-mcp 0.6.3

*From Cobalt SQL Works, 2026-10-07. Pin `v0.6.3`; Cobalt 0.8.0 ships on it.*

- **`shadow_status` is back.** Confirmed on the box that found it (state path with spaces,
  `Cobalt SQL Works`), sandbox, `test` lakehouse:

  ```
  spark.table("test.sales_import").count()            → shadow_status: [test.sales_import read 2026-10-07T21:57]
  mount_table("test", "dbo/publicholidays")           → "Cloned dbo/publicholidays"
                                                        shadow_status: + test__dbo.publicholidays read
  preload {"lakehouses": {"test": ["r2e_stream"]}}    → shadow_status: + test.r2e_stream read
  ```

  The pane's cloned/written markers and the Shadows window are live again. The stray
  `Cobalt%20SQL%20Works` folder (235 KB of test clones) is deleted here; the alpha notes tell
  testers on 0.6.0–0.6.2 to do the same.
- **A detour worth knowing about, not yours.** While chasing the empty listing on 0.6.3 I first
  saw every first touch fail with `TABLE_OR_VIEW_NOT_FOUND` and your log full of
  `AccessDeniedException: Unauthorized 401` from `listOneLakeTables`. That was Cobalt's token
  endpoint: after an evening re-sign-in the silent Azure-Storage-scope refresh had stopped and the
  endpoint fell back to the Fabric API token, which OneLake's DFS plane refuses
  (`Audience validation failed for audience 'https://api.fabric.microsoft.com'`). The endpoint
  now serves the storage audience only and says "sign in again" when it cannot. If a host ever
  reports your catalog "not resolving", the 401 in that log line is the tell.
- **`mount_table("test", "dbo/publicholidays")`** works and is what the pane's *Clone now* calls
  again (it answers when the clone exists, which suits a button better than the background
  preload we had switched to).

Nothing open from Cobalt's side. Thanks for the quick turn.
