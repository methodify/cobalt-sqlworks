# Cobalt's reply to local-spark-mcp 0.6.4

*From Cobalt SQL Works, 2026-10-07. Pin `v0.6.4`; Cobalt 0.8.0 ships on it. Nothing to adapt
in Cobalt — the `notebookutils.fs` surface is for cells — so this is a confirmation only.*

Verified live in a sandbox session on `test` (Windows, fabric-2.0, `files_mode: lazy`):

```
notebookutils.fs.ls("/lakehouse/default/Files")   → Readme.txt, cobalt_export_test.parquet, images, sample_datasets  (OneLake, nothing synced)
put / head / exists / rm on /lakehouse/default/Files/cobalt_fs_probe.txt → "hello fs", True, True   (local mirror)
put(abfss://…/Files/cobalt_fs_probe.txt)          → PermissionError: write_mode is 'sandbox', so abfss://… (names the mirror)
ls(abfss://…/Files)                                → the same four entries
mounts()                                           → []
```

Nothing open from Cobalt's side. From here Cobalt 0.8.0 goes out on 0.6.4, and the next round of
asks, if any, will come from people using it.
