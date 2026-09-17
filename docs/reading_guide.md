# Reading Guide — Cobalt SQL Works design effort

*Written 2026-09-16 at kickoff. This is the map of what exists; update it as the corpus grows.*

## What exists

The repo was empty at kickoff. Everything below was produced on day one from the founder's
brief plus four parallel research passes (web + source-code reading), ~25k words total.

| # | Document | Type | Depth | Read when |
|---|---|---|---|---|
| — | `CLAUDE.md` | Project framing, scope decisions from the founder | Short | First |
| 0 | `docs/feature_inventory.md` | **Synthesis**: every ADS capability, consolidated, with a proposed V1/V2/Later/Out disposition and the open decisions | Medium | **Second — this is the discussion artifact** |
| 1 | `docs/research/01_ads_core_features.md` | ADS connections, Object Explorer, query editor, results grid, Edit Data, settings, shortcuts, admin wizards, shell. Verified against ADS/SqlToolsService source. | Deep | When designing any of those surfaces — it has exact setting names, menu order, dialog fields |
| 2 | `docs/research/02_ads_plans_notebooks_dashboards_extensions.md` | ADS execution-plan viewer, notebooks, dashboards/insight widgets, Table Designer, every first-party extension, chart types, full release timeline, retirement facts, VS Code MSSQL successor feature set | Deep | When designing the plan viewer; when deciding notebooks/dashboards; for "what did the successor keep" |
| 3 | `docs/research/03_rust_egui_stack.md` | Rust ecosystem survey (Sept 2026): TDS drivers, Entra auth, egui crates, editor options, Arrow/Parquet/Delta export stack, packaging; recommended stack + risks | Deep | Before any architecture decision |
| 4 | `docs/research/04_competitors_and_beyond.md` | SSMS 22, VS Code MSSQL, DataGrip, DBeaver, TablePlus, Beekeeper, DbGate, Plan Explorer, SQL Prompt, Fabric portal, DuckDB UI, Hex, TUIs; user complaints; large-result handling ideas; **Top 30 ranked ideas beyond ADS** | Deep | When writing the "beyond ADS" section |
| 5 | `docs/research/05_egui_agent_usage.md` | Founder's `egui_agent` crate usage doc (verbatim copy) | Short | When wiring the app for agent-driven build/debug |

## Where coverage is strong

- **What ADS did** — exhaustively catalogued, much of it from source, down to setting names and context-menu order. We will not be surprised by a forgotten feature.
- **What the market does better** — the competitor survey is opinionated and ranked; it gives the "beyond ADS" section a running start.
- **Whether Rust/egui can carry this** — yes, with named crates, versions, and a proof point (Rerun). Risks are specific, not vague.

## Where coverage is thin

- **The founder's own daily workflow.** We have one screenshot (Fabric warehouse, `select top 100`, views tree, 100-row grid) and a brief. Which ADS features Bryon actually touched weekly vs. never is the single most useful input still missing — it decides V1.
- **Fabric specifics.** Endpoint quirks (no TCL across batches in the portal, session semantics, CU cost visibility, OneLake access from a desktop client) are mentioned but not designed.
- **Entra ID interactive auth.** We know the Rust SDK lacks an interactive credential and that we must build PKCE-loopback ourselves; we have not decided who owns the Entra app registration for an OSS product.
- **Results-grid interaction design.** The research says *what* features exist; the feel (keyboard model, selection model, copy semantics) is unspecified.
- **Visual identity.** Nothing yet beyond "light/dark, and a name that nods to Azure."

## Tensions surfaced

1. **egui_agent pins egui 0.35; the ecosystem moved to 0.36 in Aug–Sep 2026.** Hold at 0.35 (use prior crate versions) or bump egui_agent. Founder owns egui_agent.
2. **Pure Rust vs. ADS-grade IntelliSense.** SqlToolsService (.NET, ~100 MB) would give ADS's exact IntelliSense; pure Rust means our own lexer + catalog-driven completion, which is good-not-great at first.
3. **"Query runner first" vs. "the things people say they'll miss."** The founder's V1 is the query surface. The retirement threads mourn notebooks, dashboards, server groups, charting. Those are V2 candidates but the data model should not preclude them.
4. **Driver churn.** The maintained TDS driver options are all weeks old (tiberius-ng, Microsoft's mssql-tds 0.1.0). A driver trait is mandatory.

## Suggested reading order

1. `CLAUDE.md` (2 min)
2. `docs/feature_inventory.md` (15 min) — then discuss
3. `docs/research/03_rust_egui_stack.md` §8 "Recommended stack" (5 min)
4. `docs/research/04_competitors_and_beyond.md` §5 "Top 30" (10 min)
5. Everything else on demand.
