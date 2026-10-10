# Cobalt's reply to local-spark-mcp 0.8.0

*From Cobalt SQL Works, 2026-10-09. Pin `v0.8.0`. Adopted the same day, in Cobalt 0.9.4: the
rosters come from your manifest now, both engines offer them, and Cobalt's own 34-package list
is gone.*

Thank you: confirmed it was a gap, and the four asks are all answered. What Cobalt did with it:

- **The manifest is the source.** `cobalt-runtime` embeds a verbatim copy of `profiles.json` at
  the pinned tag (`crates/cobalt-runtime/local-spark-mcp-profiles.json`); a test fails when its
  `version` and Cobalt's pin disagree, so bumping the pin means copying the file again. The
  profiles themselves (pyspark, delta-spark, Python per platform, Java majors and order, Scala,
  hadoop-azure) now come from that file too: the hand-kept mirror in Cobalt's `manifest.json`
  is deleted, which closes item 1 of the original request list for good. `python_packages` →
  `name==version` requirements, `python_packages_source.url` → the "Source" in the tooltip,
  `python_packages_excluded` is kept for the record.
- **Both engines, one list.** Local Spark: a *Fabric packages* checkbox on the runtime page
  installs the profile's roster into the profile's environment (`spark.profile_packages`).
  LakeSail: the picker offers *None* / *fabric-2.0* / *fabric-1.3* (`spark.sail_profile`).
  Install, Reinstall / update and Install Python packages all honour the choice.
- **The install is Cobalt's own copy of your procedure** (one `uv pip install` of the roster,
  then per package, skipped ones named, never fatal) rather than
  `python -m local_spark_mcp.fabric_packages install`, for two reasons: the LakeSail
  environment has no `local_spark_mcp` in it, and Cobalt's log window streams the resolver's
  lines as they come. Same outcome, same reasons.
- **Status reads dist-info**, as yours does: installed at Fabric's version / other version
  (`scipy 1.17.1 (Fabric 1.18.0)`) / missing, plus what the last install skipped. The agent's
  `runtime` JSON carries it as `roster {profile, installed, total, missing, mismatched, failed,
  complete}`.

Verified live on Windows (Python 3.11 for both profiles' environments):

```
0.7.0 → 0.8.0 in the fabric-2.0 environment     7 s (tarball build, pyspark/delta kept)
profiles --json                                 version 0.8.0, protocol 2, 56 packages
fabric-2.0 roster into the Local Spark env      194 s · 55 of 56 at Fabric's version
                                                 (the batch fails on scipy==1.18.0, per-package
                                                 fallback; scipy 1.17.1 arrives as a dependency)
healthcheck --json  fabric_packages             {total 56, installed 55, mismatched {scipy:
                                                 {want 1.18.0, have 1.17.1}}, complete false}
fabric-2.0 roster into the LakeSail env          44 s on top of the old 34 (22 more; sqlparse
                                                 0.6.0 → 0.5.5 and tqdm pinned down to Fabric's)
imports in both envs                            pandas 2.3.3, pyarrow 24.0.0, scikit-learn,
                                                 catboost, prophet, optuna, plotly, statsmodels,
                                                 mlflow-skinny, azure-identity OK; pyspark 4.1.1
                                                 still imports beside them (setuptools 83 has
                                                 no pkg_resources any more; nothing on 2.0 needs it)
```

Nothing to ask. Two notes, no action needed:

- Your `healthcheck` reports the roster only when it finds a JDK first; run without
  `JAVA_HOME` it lists the Java problem and still carries `fabric_packages`, which is what
  Cobalt needs. Fine.
- The pyproject extras carry the `scipy` marker split (`>= 3.12` / `< 3.12`) while
  `ROSTERS` and the manifest pin `1.18.0` flat, so a host that installs from the manifest
  reports scipy as mismatched on 3.11 where the extra would have resolved cleanly. Cobalt's
  status says exactly that, so users understand it; if the manifest ever grows a per-platform
  marker, Cobalt will read it.
