# D009 — Notebooks with Python/PySpark cells on a Cobalt-managed local Spark

**Status:** proposed, 2026-10-05 · **Supersedes part of:** D002 ("Out: Jupyter/Python kernels")

## Context

D002 scoped V2 notebooks as SQL-only, with no Python and no Jupyter kernels. Two things changed:
every V1.x item has shipped (v0.6.1), and the founder's `local-spark-mcp` showed that a local
Spark matching a Fabric runtime, reading OneLake directly and sandboxing writes as Delta shallow
clones, is practical on Windows and Linux. The Fabric development loop (portal, session start,
wait, write to real tables) is the pain this addresses. See `docs/design/notebooks_roadmap.md`.

## Decision (proposed)

- Notebooks are `.ipynb` (nbformat 4) with Fabric's metadata, so they move between Cobalt, the
  Fabric portal and Git unchanged. Markdown, SQL and PySpark cells.
- SQL cells run against a Cobalt connection through the existing session actor; PySpark and
  `%%sql` cells run in a **worker process** reused from local-spark-mcp over its socket
  protocol. This is **not** a Jupyter kernel: no ZeroMQ, no kernelspecs, no Jupyter server.
  The "no Jupyter kernels" part of D002 stands; the "no Python" part is reversed.
- Cobalt **owns the runtime**: uv, Python 3.11, the pinned Fabric runtime environment, and a
  non-Oracle JDK 17 are installed into Cobalt's app-data folder on request, hash-checked and
  removable; existing installs that meet the pins can be adopted instead.
- OneLake access uses the signed-in user's Entra token, served to the JVM from a loopback
  endpoint inside Cobalt. Writes default to **sandbox** (shallow clones); write-through is an
  explicit switch.
- Delivery in three releases: 0.7 SQL notebooks + runtime manager, 0.8 PySpark cells with Arrow
  results, 0.9 OneLake/Fabric notebooks and shadows.

## Consequences

- local-spark-mcp becomes a dependency with a published version and two protocol additions
  (Arrow results, interrupt) that Cobalt contributes upstream.
- The download footprint of the optional runtime (~600 MB) is the largest thing Cobalt will ever
  fetch; it never happens without the user asking.
- `docs/product_design.md` §5.5 ("No Jupyter, no Python") needs rewording once this is accepted.

## Open

JDK vendor (Microsoft Build of OpenJDK vs Temurin), notebooks root (= Files sidebar root?), and
whether the runtime manager ships in 0.7 or 0.8.
