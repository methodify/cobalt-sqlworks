# Semantic models: querying Power BI / Fabric semantic models and Analysis Services from Cobalt

*Design, 2026-10-13. Status: approved by the founder the same day on every decision in §6; slice A
built 2026-10-13 (`cobalt_core::tabular`, the `cobalt-tabular` crate with the Arrow query API
transport, `cobalt_sql::dax`, `session/tabular.rs`, `tabularq.rs`, `ui/model_tree.rs`) and
verified in the app up to the service, which answers a bare 401 until the app registration carries
`Dataset.Read.All`; a Direct Lake test model "Cobalt test model" exists in the Fabric test
workspace for the live check. Scope from the founder: consume
semantic models (query them, get results into the results UX), a visual query builder for people
who don't write DAX, dashboards fed by models, a pure-Rust transport (no .NET sidecar), Analysis
Services endpoints where the same protocol allows. Not in scope: authoring or modifying models.
Product design §5.3 (Fabric-native), §5.6 (dashboards), §5.9 (more engines), §5.1 (local pivot).
Research briefs behind this doc: the XMLA wire protocol (MS-SSAS, MS-SSAS-T, XMLA 1.1), the Fabric
and Power BI REST surface for semantic models, and a survey of non-.NET clients; the codebase seams
were mapped the same day. Where a fact is unconfirmed it is marked so.*

## 1. The idea in one paragraph

A semantic model is the third kind of thing Cobalt's audience lives in, next to the warehouse and
the lakehouse: it is where the measures are. Cobalt should let a person open a model from the
Fabric panel (or a saved profile), see its tables, columns, measures, hierarchies and
relationships, run DAX against it and get the result in the same grid, charts, exports, history
and dashboards everything else lands in, and build a pivot-table-style query by dragging fields
into Rows / Columns / Values / Filters without knowing DAX, with the generated DAX one click away
so the person learns it by example. Two transports, one engine: Microsoft's new Arrow query API
for models on Fabric capacity (ships in days, Microsoft-supported, lossless Arrow straight into our
store), and a pure-Rust XMLA client (SOAP over HTTPS) for Azure Analysis Services, SQL Server
Analysis Services over its HTTP endpoint, MDX and DMVs, and Power BI once one undocumented routing
step is captured. No .NET anywhere.

## 2. What the research changed

**The REST story is not what it was a year ago.** The JSON `executeQueries` API is the one the
founder remembers: 100,000 rows or a million values, 15 MB, one `EVALUATE`, no INFO functions, no
MDX, types lost in JSON. It is too small for Cobalt. But in 2026 Microsoft added
`POST /v1.0/myorg/datasets/{id}/executeDaxQueries` (preview), which returns **Apache Arrow IPC
streams** (LZ4-framed record batches of up to 100k rows, streamed), allows **multiple
`EVALUATE`s per request**, supports the **`INFO.*` functions** (so the model's tables, columns,
measures and relationships come back as plain DAX results), defaults to a one-million-row cap
with **no fixed ceiling** (`resultSetRowCountLimit`), carries `queryTimeout`, `schemaOnly`,
`effectiveUsername` / `roles` / `customData` for RLS testing, and is what Power BI's own DAX query
view uses. It needs a token for the Power BI audience (`https://analysis.windows.net/powerbi/api`,
same PKCE flow we already run), the `Dataset.Read.All` delegated permission, Build on the model,
and the two tenant settings (Execute Queries REST API, XMLA endpoints). It is **capacity-only**
(Premium, Fabric F SKUs including trial, Embedded), rate-limited at 120 requests per minute per
user (40 for Pro/PPU-licensed users), and has no server-side cancel. Cobalt's results store *is*
an Arrow chunk store, so this is a near-zero-impedance fit: decode the stream, append batches.

**Pro workspaces have no XMLA endpoint at all**, so a pure-Rust XMLA client does not reach more
Power BI workspaces than the Arrow API does. What XMLA adds is: Azure Analysis Services, SSAS
over `msmdpump.dll`, MDX (Excel-style pivots over hierarchies), the `$SYSTEM` DMVs (sessions,
commands, locks, storage), sessions with a real `Cancel`, and Power BI models whose live
connection the REST API refuses.

**XMLA over HTTPS is small and fully specified** (MS-SSAS §2.1.2 and §2.1.3): plain SOAP 1.1,
`SOAPAction` Discover / Execute, `X-Transport-Caps-Negotiation-Flags: 0,0,0,0,0` keeps every
response in text XML (no binary XML, no XPRESS), DAX results come back as an XMLA rowset with an
inline XSD schema, sessions are `BeginSession` / `Session` / `EndSession` SOAP headers, and
`<Cancel/>` works inside a session. Azure AS is confirmed reachable this way with a Bearer token
(a raw Discover against a resolved `*.asazure.windows.net/webapi/xmla` host is documented working).
**Power BI's hop is the one unknown**: the official client first asks `api.powerbi.com` to
resolve a workspace to its `*.pbidedicated.windows.net` cluster, then posts XMLA to
`/webapi/xmla` there with a few `x-ms-xmla*` routing headers. The URL templates and header names
are visible in the ADOMD client library; the request and response shapes are not public. One
Fiddler capture of SSMS or DAX Studio (the founder's team has both) settles it.

**SSAS over its native TCP port is not HTTP**: DIME framing, an SSPI handshake inside SOAP, sealed
frames afterwards. Two open-source projects reverse-engineered it in 2025 to 2026 (pure Python and
C++, NTLM verified). It is feasible in Rust later; it is a separate, larger project and not in this
slate. The documented on-prem path is `msmdpump.dll` on IIS, which is the same plain SOAP as above.

**Nobody has shipped a non-.NET client for Power BI's XMLA endpoint.** crates.io has a binary-XML
codec and two XMLA *servers* (one validated against Excel and MSOLAP, which documents what the
Microsoft client sends), no client. Cobalt would be first, and the Arrow API means it does not
have to be first to ship value.

## 3. Product shape

### 3.1 Where semantic models appear

1. **Fabric panel.** Workspace listings already return items of type `SemanticModel`; today they
   are discarded. They get a row with the model icon, the storage mode as a hint (Import, Direct
   Lake, DirectQuery, from the Power BI datasets listing when the user has write access), and a
   context menu: **Query (DAX)**, **Explore**, **Open model** (the tree), **Pin**, **Open in
   portal**, **Copy connection string** (the `powerbi://` form for Excel, SSMS, Tabular Editor).
   Default semantic models of lakehouses and warehouses are listed like any other.
2. **Servers library.** A new profile kind, **Analysis Services**, with three endpoint flavours:
   *Power BI workspace* (`powerbi://api.powerbi.com/v1.0/myorg/<workspace>`), *Azure Analysis
   Services* (`asazure://<region>.asazure.windows.net/<server>`), and *HTTP (msmdpump)* (a URL,
   Basic or Windows auth). Under the profile the tree lists the models (catalogs) and under each
   model the model explorer. Power BI profiles use Entra like the Fabric panel; the panel's
   "Save to servers" works for a model. The library kind lands with the XMLA slice; until then
   the panel is the entry point.
3. **A DAX query tab.** Opened from a model row, a tree node, or File > New (when the active
   context is a model). It is a query tab: the editor with DAX syntax, the toolbar, Run / Cancel,
   the results area with the grid, charts, totals, find, export, Run to File, history, Pin to
   dashboard. Several `EVALUATE`s give several result sets. The status bar says `DAX · <model>` and
   the workspace or server. The editor's language is DAX: `'Table'[Column]`, `[Measure]`,
   `EVALUATE` / `DEFINE` / `MEASURE` / `VAR` / `RETURN` / `ORDER BY` / `START AT`, functions,
   strings, comments; completion offers tables, columns, measures (with the format string and the
   expression in the detail), DAX functions (the model's own list via `INFO.FUNCTIONS()`, static
   fallback), and the snippets below.
4. **The model explorer.** A tree per model: Tables (hidden ones dimmed, togglable) → Columns
   (data type, key, hidden, sort-by, format), Measures (format string; expression on hover and in
   the describe panel when the service lets us see it), Hierarchies → Levels; Relationships (from
   → to, cardinality, active, direction); Calculation groups; Roles and Perspectives when
   visible. Drag or double-click inserts `'Table'[Column]` or `[Measure]` at the cursor. Context
   menu quick queries, mirroring DAX query view: *Top 100 rows* (`EVALUATE TOPN(100, 'T')`),
   *Distinct values* (`EVALUATE DISTINCT('T'[C])`), *Evaluate measure* (`SUMMARIZECOLUMNS` with
   the measure), *Evaluate by…* (opens Explore with the measure in Values), *Define measure*
   (pastes `DEFINE MEASURE 'T'[M] = <expr>` so the person can edit it in the query), *Define all
   measures of table*, *Describe* (the data dictionary row).
5. **Explore: the visual query builder.** A tab (or a pane on the model) with a field list on the
   left (tables → columns and measures, searchable, hidden toggle) and four wells: **Rows**,
   **Columns**, **Values**, **Filters**, plus **Sort**, **Top N** and **Subtotals** toggles. Drag
   or click-to-add. A column in Values asks for an aggregation (Sum, Count, Count distinct, Avg,
   Min, Max); a measure is just a measure. A filter is a field with an operator: *is one of*
   (picks from the distinct values, loaded on demand), *between*, *contains*, *is blank / not
   blank*, *relative date* for date columns. The result shows below as the normal grid; when
   **Columns** has fields the result is a **matrix** (Rows down, Columns across, Values in the
   cells, optional subtotals), rendered by a local pivot over the flat result. A **DAX** panel
   shows the generated query, read-only, with *Open in query tab* and *Copy*. Everything the
   results area does applies (chart toggle, export, totals, Pin to dashboard). The Explore spec
   saves as a `.cobalt-explore.json` file and pins to a dashboard tile as its generated DAX plus
   the spec (so the tile can be re-opened in Explore).
6. **Dashboards.** Tiles get a source: SQL on a profile (today) or a semantic model. A number
   tile from a measure (`EVALUATE ROW("v", [Measure])`), a chart tile from an Explore result, a
   grid tile from a DAX query. "Pin to dashboard" on a DAX tab or an Explore tab does the right
   thing. This is the one place the founder's "dashboards never" bends: a page of KPI measures
   from the model is the single most common thing people want from a semantic model.

### 3.2 DAX the builder generates

The builder's spec is engine-agnostic (dimensions, values with aggregations, filters, sort, top
N, subtotals) so a later SQL emitter can offer Explore on a warehouse or lakehouse table. The DAX
emitter produces what Power BI's own visuals produce:

```dax
DEFINE
    VAR __F1 = TREATAS({"Accessories", "Clothing"}, 'Product'[Category])
    VAR __F2 = FILTER(KEEPFILTERS(VALUES('Date'[Year])), 'Date'[Year] >= 2024)
EVALUATE
    SUMMARIZECOLUMNS(
        'Product'[Category], 'Date'[Year],
        __F1, __F2,
        "Sales Amount", [Sales Amount],
        "Quantity", CALCULATE(SUM('Sales'[Quantity]))
    )
ORDER BY [Sales Amount] DESC
```

Subtotals wrap group-by columns in `ROLLUPADDISSUBTOTAL(..., "IsSubtotal")`; Top N wraps the
whole thing in `TOPN(n, ..., [measure], 0)`; the matrix is the same query with the Columns fields
added to the group-by and pivoted locally, which keeps one query per refresh and makes the pivot
reusable for product design §5.1 (a local pivot over any result set). Rules the emitter follows:
fully qualified columns, one `DEFINE` block, filters as tables not booleans, `IGNORE()` is not
used (blank rows drop, as in Power BI), and `ORDER BY` names a selected column or value.

### 3.3 Two transports, one engine

| | Arrow query API (`executeDaxQueries`) | XMLA over HTTPS (pure Rust SOAP) |
|---|---|---|
| Targets | Power BI / Fabric models on capacity | Azure AS; SSAS via msmdpump; Power BI after the routing capture |
| Languages | DAX, INFO functions | DAX, MDX (`Format=Tabular`), DMVs (`$SYSTEM.*`) |
| Results | Arrow IPC (LZ4), lossless types, streamed | XMLA rowset XML with inline XSD, parsed to Arrow |
| Model tree | `INFO.VIEW.TABLES/COLUMNS/MEASURES/RELATIONSHIPS` in one request | `TMSCHEMA_*` DMVs or the same INFO functions |
| Cancel | none (query timeout, drop the connection) | `<Cancel/>` in a session |
| Limits | 120 requests/min/user, 1M rows default cap (raisable) | capacity workload settings (default 1 h timeout, no row cap) |
| Risk | preview API; permissions of INFO.VIEW for Build-only users to verify live | Power BI hop unconfirmed; everything else specified |

The engine code above the transport (tab, tree, completion, Explore, tiles) does not know which
transport runs a query. A `TabularClient` trait has `execute(dax) -> stream of result sets`,
`catalog() -> ModelCatalog`, `cancel()`, and each transport implements it; the Power BI target
prefers Arrow and falls back to XMLA when that slice exists and the Arrow API refuses (Pro
workspace: neither works, and the tab says so in one sentence with the tenant setting or license
it needs).

### 3.4 What this points to beyond the ask

- **Explore on everything.** Once the pivot builder exists with an abstract spec, a SQL emitter
  (`GROUP BY` with the same wells) gives the same UX on warehouse, lakehouse and SQL Server tables.
  The founder's "I don't want to pre-group before seeing the distribution" from the charts work is
  the same instinct: see the shape first. Design the spec engine-agnostic now; ship the SQL emitter
  when asked.
- **The data dictionary.** The model tree is a complete dictionary (tables, columns, measures with
  expressions, descriptions, display folders, relationships). *Export model documentation* to
  Markdown or Excel is a half-day on top of the tree and is something every BI team asks for.
- **Lineage back to the lake.** Direct Lake tables name their source lakehouse or warehouse table.
  *Open source table* on a model table opens the Spark SQL or SQL endpoint tab on it: "what does
  the model say vs what the warehouse says" in one app.
- **View as.** `effectiveUsername` and `roles` on the query tab (the Arrow API and XMLA both carry
  them) let the founder's team test RLS without leaving Cobalt. A small connection-options dialog.
- **DAX in notebooks.** A DAX cell kind that runs through the same client (semantic link's
  `evaluate_dax` parity without the .NET under it), results in the cell with charts. After the
  slate, if notebooks keep growing.
- **Measure dependencies.** `INFO.CALCDEPENDENCY()` gives the measure graph; *Where used* on a
  column or measure. Later.
- **Refresh status** (last refresh, failures) on the model row from the Power BI datasets API when
  the user has write access. Cheap, later.
- **Out, deliberately:** authoring (TMSL, TOM), refresh, Profiler and traces, DAX performance
  tooling (DAX Studio's server timings), native TCP SSAS (see §2), Excel-style MDX pivoting over
  hierarchies (the local pivot over DAX covers the pivot need; MDX stays a text-query feature).

## 4. Architecture

- **`crates/cobalt-tabular`** (new; depends on core, arrow, reqwest, quick-xml, no egui, no
  tokio types in its public API beyond `Stream`): `ModelCatalog { tables: [Table { name, hidden,
  storage_mode, columns: [Column { name, data_type, hidden, key, format, sort_by, description }],
  measures: [Measure { name, format, expression: Option, display_folder, description }],
  hierarchies }], relationships, calculation_groups, roles }`; `TabularTarget::{PowerBi {
  workspace_id, workspace_name, dataset_id, model_name }, AzureAs { region, server, model }, Http {
  url, model }}`; the `TabularClient` trait; `rest.rs` (the Arrow API: request body, concatenated
  IPC streams via `arrow_ipc::reader::StreamReader` with the `lz4` feature, error rowsets detected
  by schema metadata `IsError`, dictionary-encoded strings cast to utf8, `Variant` dense unions
  cast to string, `date64` to the grid's datetime); `xmla/` (slice C: `soap.rs` envelopes and
  headers, `rowset.rs` streaming parser of `root/row` with the XSD schema, `_xHHHH_` name
  decoding and `sql:field`, `session.rs`, `discover.rs`, target resolution per flavour); `catalog.rs`
  builds `ModelCatalog` from INFO.VIEW results or TMSCHEMA rowsets; `explore.rs` the DAX emitter
  for `ExploreSpec`. Types map to `SqlType` the way `sparkq::spark_type_to_sql` does.
- **`cobalt-core`**: `ExploreSpec { source, rows, columns, values: [{ field, agg }], filters,
  sort, top_n, subtotals }` (serde; the file format and the tile payload); `TileSource::{Sql,
  SemanticModel { target }}` on `TileSpec` (default `Sql`, file version unchanged);
  `ProfileKind::AnalysisServices { target }` on the profile when the library kind lands (slice C).
- **`cobalt-sql::dax`**: the DAX lexer (for highlighting) and completer (tables, columns,
  measures, functions, keywords, snippets) over a `ModelCatalog` view; `Syntax::Dax` in the
  editor. Independent of IO, unit-tested like the T-SQL side.
- **`cobalt-auth`**: `POWERBI_RESOURCE = "https://analysis.windows.net/powerbi/api"` and
  `Dataset.Read.All` in the required scopes (the app registration gains the delegated Power BI
  Service permission; `Grant permissions` already handles the consent round). One refresh token,
  one more audience, same token cache.
- **`cobalt-results`**: `pivot(view, rows, columns, values) -> PivotTable` (local matrix over a
  flat result set; also the §5.1 "local pivot" later) and a matrix grid view in
  `ui/results/matrix.rs`.
- **Session runtime**: `Command::RunTabular { tab, run, target, token, text, opts }` handled by
  a spawned task per run with an abort handle per tab (`Command::Cancel` aborts it; the XMLA
  transport also sends `<Cancel/>`). The task emits the existing `RunStarted`,
  `ResultSetStarted { rs }` (built with the shared memory budget and spill dir), `ResultSetDone`,
  `Message`, `RunDone`, so `apply_event`, the grid, Messages, history, Run to File, charts and
  dashboard tiles need no new code. This is the seam the codebase survey recommended over a
  `Driver` impl (ten T-SQL catalog methods and a SQL-audience token to fake) or a Spark-style actor
  (a long-lived process that does not exist here).
- **App**: `EditorTab.tabular: Option<TabularTab { target, catalog: Loadable<Arc<ModelCatalog>>,
  options: { effective_user, roles } }>`; `tabularq.rs` (new tab, run, cancel, snapshot with a
  `dax:` prefix, catalog load); Fabric panel rows and actions (`FabricAction::{DaxQuery, Explore,
  OpenModel}`), the model explorer under the panel row and under library profiles; `ui/explore.rs`;
  `ui/editor` `Syntax::Dax`; status bar and tab-kind branches (the survey lists every
  `t.spark.is_some()` site: run, cancel, snapshot, title, syntax, toolbar, status bar, agent).
- **Agent verbs**: `dax_query { workspace, model, text? }`, `model { action: tree | describe |
  refresh }`, `explore { action: new | set | state | dax | run | save | open, rows, columns,
  values, filters, ... }`, `dashboard add_tile` with a `source`; tab kind `dax`; a11y labels
  `well rows`, `field 'Table'[Column]`, `explore dax`, `model table T`, `measure M`.

## 5. Slices

- **Slice A: DAX query tab on Fabric semantic models (Arrow API).** Semantic model rows in the
  Fabric panel; the Power BI token audience and scope; `cobalt-tabular` with the REST transport
  and `ModelCatalog` from INFO.VIEW; `Command::RunTabular`; the DAX tab with syntax, completion,
  multiple result sets, cancel-by-abort, history; the model explorer under the panel row with the
  quick queries and drag-to-insert; agent verbs; alpha notes. Verified live on the Fabric test
  workspace (needs a model there, see §7).
- **Slice B: Explore.** `ExploreSpec`, the DAX emitter with tests, the field list and wells, filters
  with distinct-value pickers, sort, top N, subtotals, the local pivot and the matrix view, the DAX
  panel with Open in query tab, `.cobalt-explore.json`, Pin to dashboard with `TileSource`, charts
  on Explore results, agent verbs.
- **Slice C: the XMLA transport.** SOAP Discover / Execute, the rowset parser, sessions and
  Cancel, DMVs and MDX in the DAX tab (the tab becomes "DAX / MDX"), the Analysis Services profile
  kind with the three flavours, Azure AS resolution, msmdpump with Basic / Negotiate, and Power
  BI over XMLA once the routing capture is in hand. Unit-tested against recorded envelopes;
  live-tested on Azure AS or an msmdpump endpoint only if the founder can provide one, otherwise
  Power BI after the capture.
- **Slice D (pick list):** model dictionary export; Open source table lineage; View as (RLS);
  refresh status; DAX cells in notebooks; the SQL emitter for Explore on tables.

## 6. Decisions for the founder (recommendations in bold)

1. Transport order: **Arrow API first (slice A), XMLA second (slice C)**, vs XMLA first. The
   Arrow API is pure Rust too, Microsoft-supported, and lossless; it ships the tab, the tree,
   Explore and tiles for the Fabric audience without a single undocumented call. XMLA then adds
   Azure AS, SSAS, MDX, DMVs and cancel on a known-good base. If you want XMLA first on principle,
   slice A becomes the XMLA client against Azure AS (documented) plus the Power BI capture, and the
   Fabric audience waits on that capture.
2. Explore scope in slice B: **Rows, Columns (matrix via local pivot), Values, Filters, sort, top
   N, subtotals all in B**, vs a flat-only first cut. The pivot is local and cheap, and a pivot
   builder without Columns is not a pivot table.
3. The Explore spec: **engine-agnostic in `cobalt-core` from day one, DAX emitter now, SQL emitter
   in slice D or later**, vs DAX-only.
4. Where models live: **Fabric panel first (A), Analysis Services profile kind with XMLA (C)**, vs
   a library kind from the start.
5. SSAS reach in this slate: **HTTP via msmdpump only**; native TCP (DIME + SSPI sealing + NTLM /
   Kerberos) is a later project if a customer needs it.
6. Naming: **"Semantic model" everywhere Fabric is the context, "Analysis Services" for the
   profile kind** (it covers Azure AS, SSAS and Power BI's XMLA endpoint, which is what Microsoft
   calls it).
7. Dashboards: **tiles gain a source; a measure becomes a number tile in one click** (B), vs
   leaving models out of dashboards.
8. RLS "View as": **slice D**, vs in A. It is a small dialog but needs admin rights to test
   meaningfully.

## 7. Before execution (the founder's side)

- **App registration**: add the delegated Power BI Service permission `Dataset.Read.All` (and
  `Workspace.Read.All` if not present) to Cobalt's registration; users consent through the existing
  Grant permissions round.
- **A semantic model to test on** in the "Fabric test" workspace (the trial or F capacity has the
  XMLA endpoint and the Arrow API): a Direct Lake model over the `test` lakehouse is enough, with
  two or three measures and one relationship. Fabric no longer creates default models for new
  lakehouses automatically, so one may need to be created in the portal.
- **Tenant settings** on the test tenant: *Dataset Execute Queries REST API* and *Allow XMLA
  endpoints and Analyze in Excel with on-premises semantic models* (both on by default).
- **For slice C on Power BI**: one Fiddler or mitmproxy capture of SSMS or DAX Studio connecting to
  a workspace (the `api.powerbi.com/powerbi/databases/v201606/...` resolution call and the first
  `/webapi/xmla` POST with its headers). The work tenant is fine for the capture; nothing is written.
- **For slice C on Azure AS / SSAS**: an endpoint to test against, if one exists outside the work
  environment; otherwise slice C ships unit-tested on recorded envelopes and Power BI only.
- The stop-and-coordinate rule holds: no live Entra or Fabric calls against the work tenant
  without the founder's word for each round.

## 8. Risks

- The Arrow API is in preview; the request and response shapes may move. The transport is one
  file and the engine above it does not care.
- INFO.VIEW permissions: the docs disagree on whether Build-only users get the tree or only
  write users. Verify live on day one of slice A; the fallback is the Fabric `getDefinition` TMDL
  (write permission) and, failing that, a tab without a tree.
- Rate limits: 120 requests per minute per user is plenty for a person, thin for a dashboard with
  many tiles on short intervals. Tiles on a model default to 5 minutes and the page says so.
- No server-side cancel over REST: a runaway query runs until `queryTimeout` (we set it from the
  tab's timeout) even after the user hits Stop. The status says "stopped locally; the service
  finishes within N s".
- Power BI over XMLA depends on one capture; if the front door requires something we cannot
  reproduce, Power BI stays on the Arrow API and XMLA serves Azure AS and SSAS only.
- Direct Lake quirks: user hierarchies and session-scoped MDX are unsupported there; DAX and the
  INFO functions are fine. The tree shows hierarchies but Explore does not depend on them.
- DAX completion without a parser: the first completer is lexical (bracket context decides
  table / column / measure). Good enough for `'Table'[Column]`; expression-aware completion is a
  later refinement, as it was for T-SQL.
- Type edges: `Currency` arrives as decimal128(19,4), `DateTime` as date64 (milliseconds), Variant
  as a dense union; each gets an explicit cast to a grid type and a test.
