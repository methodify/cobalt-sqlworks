# Cobalt's reply to local-spark-mcp 0.8.1

*From Cobalt SQL Works, 2026-10-10. Pin `v0.8.1`. Adopted the same day (Cobalt 0.9.4, with
0.8.0): the fallback data is read from the manifest, applied at install time and in the status.*

Thank you for taking the note up as data rather than as a special case. What Cobalt does with
`python_packages_fallbacks`:

- **Install.** `Roster::requirements_for(python)` substitutes the fallback requirement for the
  pin where the marker holds for the environment's Python (read from `pyvenv.cfg`, so the
  LakeSail environment gets the same treatment without `local_spark_mcp` in it). The marker
  evaluator covers exactly your shape, `python_version <op> 'X.Y'`; anything else is treated
  as not applying. The log says what was swapped and why.
- **Status.** A package at a version that satisfies the applicable fallback counts as
  installed and is listed under "platform fallback" with Fabric's version and your reason;
  "other version" is kept for real drift; `complete` follows. The agent's `runtime` JSON gains
  `variants`.
- The verbatim `profiles.json` copy moves to 0.8.1 and the pin test with it.

Verified live on Windows, Python 3.11 in both environments:

```
0.8.0 → 0.8.1 in the fabric-2.0 environment     6 s
status before any install (0.8.1 data)          56 of 56 at Fabric's version · complete true
                                                 (scipy 1.17.1 now a platform fallback)
fabric-2.0 roster into the Local Spark env       one resolution: "Audited 56 packages in 591 ms",
                                                 scipy>=1.15,<1.18 instead of scipy==1.18.0
same into the LakeSail env                       "Audited 56 packages in 431 ms"; the recorded
                                                 "not installable here: scipy==1.18.0" from the
                                                 0.8.0 run clears
healthcheck --json  fabric_packages             installed 56, mismatched {}, variants {scipy:
                                                 {have 1.17.1, fabric 1.18.0, requirement, reason}},
                                                 complete true
```

Nothing to ask.
