# Charts: on a result set, in a notebook cell, on a dashboard tile

*Design, 2026-10-11. Status: approved by the founder the same day with one change (no row
budget: the chart takes whatever the result set holds, decision 3 withdrawn); slice A built
2026-10-11 (`cobalt_core::ChartSpec`, `cobalt_results::chart`, `ui/results/chart.rs`), with
a histogram kind added the same day; slice B built 2026-10-11 (cell header toggle, spec in
`metadata.cobalt.charts["<set>"]`, Keep in notebook → `image/png` output carrying the spec,
`image/png` outputs drawn through egui's image loaders). Slice C below, only on the founder's
word. Product design §4.5
("Charts (V2): quick bar/line/pie/scatter from the selection") and §5.6 (lightweight
dashboards); roadmap "Charts in cells (egui_plot)". The founder's usage signal (D002): charts
rarely, dashboards never — so the shape is a quick chart over what is already on screen, not a
charting product.*

## 1. The idea in one paragraph

A chart is another way to look at a result set that is already in Cobalt: the grid's rows, the
grid's current sort and filter, the columns the user points at. One click on a result set (or
a cell's result) turns it into a bar, line, area, scatter or pie chart; the chart follows the
grid (re-run, sort, filter, totals) and the user picks the X column, one or more Y columns and
an aggregation when X repeats. Nothing new is fetched, nothing is sent anywhere, and the chart
can be copied as an image for the Teams message or saved in the notebook as a picture other
tools can show. Dashboard tiles are the same chart spec pinned to a saved query with a refresh
interval, a later slice.

## 2. What exists to build on

- `cobalt_results::ResultSet`: the chunk store with a view (sort + filter), `view_to_single_batch`
  ("small sets only — copy, charts"), `cell_value` typed access, totals and the column
  profiler (`ui/results/profile.rs`: per-column summaries, numeric detection). A chart reads
  the view, so what the user sorted and filtered is what gets charted.
- `egui_plot` 0.37 tracks egui 0.36 (checked on crates.io): `BarChart`, `Line`, `Points`,
  `Polygon`, legends, hover labels, zoom/pan, `Plot::show` with `PlotPoints`. No pie: a pie is
  a handful of `Polygon` sectors or a painter arc, 40 lines.
- Result-set tools already have a home: the per-set toolbar (Maximize, Profile columns,
  Totals row, Copy, Export, Pop out) and `ResultsAction`.
- Notebook outputs are MIME bundles (`cobalt_notebook::Output::DisplayData`), with Cobalt's own
  `application/vnd.cobalt.*` entries alongside `text/html` / `image/png` for other tools
  (`PNG_MIME` exists; cells do not render images yet).
- egui can hand back the frame as pixels (`ViewportCommand::Screenshot`; the agent's
  `screenshot` verb uses it), which is how a chart becomes a PNG without a second renderer.
- Theme colours (`Theme`) for a categorical palette that reads in light and dark.

## 3. Product shape

**Where a chart appears.**

1. *A result set* (slice A). Each result set gets a **Chart** toggle next to Maximize: the grid
   area shows the chart instead of the grid (the grid is one click away; the chart keeps its
   spec per set for the life of the run and across re-runs of the same query text). A thin
   configuration strip above the chart: kind, X, Y (multi-select), aggregation, top-N, stacked,
   and "Copy image". Selection in the grid before clicking Chart seeds X/Y: the selected
   columns become the series; a single selected column with a text column to its left becomes
   Y with that X.
2. *A notebook cell* (slice B). A cell's result grid has the same Chart toggle; the chart spec is
   saved in the cell's metadata (`cobalt.chart`) and the rendered picture goes into the cell's
   outputs as `image/png` next to the table, so the `.ipynb` shows the chart in Fabric, VS Code
   and GitHub. Re-running the cell re-renders from the spec.
3. *A dashboard tile* (slice C, after the founder's call). A saved query plus a chart spec (or
   "number" for a one-cell result) pinned on a server or database home page with a refresh
   interval; a page of tiles is a file. §5.6 as written; nothing here constrains it.

**Chart kinds and the data they need.**

| kind | X | Y | notes |
|---|---|---|---|
| bar (vertical, horizontal) | category or number | one or more numeric series | grouped side by side, or stacked |
| histogram (added on the founder's ask, 2026-10-11) | number or date/time, binned | count of rows per bin, or a sum / other aggregate of a numeric column | bins from the data (Freedman–Diaconis, Sturges when the IQR is 0; 5–200) or a typed count; touching bars; a bin's range in the hover; the profiler's 12-bin sketch can point here later |
| line | number, date/time, or category in row order | one or more numeric series | gaps for NULL |
| area | as line | as line | stacked by default |
| scatter | number or date/time | one numeric series | a third numeric column sizes the points (optional) |
| pie / donut | category | one numeric series | top-N slices + "other" |
| number | — | the first cell | the dashboard "number" tile; also fine on a 1×1 result |

**Aggregation.** When X has repeated values the series are aggregated per X: sum (default for
numbers), count, average, min, max, count distinct; "none" plots every row (scatter's default).
Aggregation runs in Rust over the view's rows (one pass, a hash map keyed by X), capped at a
row budget (default 200,000 visible rows: above it the strip says "first 200,000 rows" and
offers to chart the whole set anyway, since a GROUP BY in the query is the better tool there).

**X ordering.** Numbers and dates in value order; categories in first-seen order (the view's
order, so a sorted grid sorts the bars) or by value (top-N). Dates on a line chart use the
value axis with date labels.

**Interaction.** Hover shows the series name, X and Y value (egui_plot's label formatter);
legend entries toggle series; drag to zoom, double-click to reset (egui_plot defaults);
clicking a bar or point selects the matching rows in the grid when the chart has no
aggregation, else selects the first row of that X — enough to answer "which rows is that?".

**Copy image / Save as PNG.** The chart rect is read from the next frame's screenshot and
written as PNG to the clipboard (`arboard` already carries images? to check; else a temp file
and the file path on the clipboard) or to a file via Export. The same path feeds the notebook's
`image/png` output. A chart rendered off-screen (`cobalt run notebook` later) is out of scope
until a headless frame exists; the spec stays in the cell metadata so it renders when opened.

**Colours and type.** A fixed categorical palette of eight colours derived per theme (accent
first), series beyond eight wrap with lighter tints; NULL X is a "(null)" category; axis labels
use the grid's number formatting (`CellFormatter`), with thousands separators on the value
axis and the column's display name as the axis title.

## 4. Architecture

- `crates/cobalt-core`: `ChartSpec { kind, x: Option<usize>, y: Vec<usize>, agg: Agg, top_n:
  Option<usize>, stacked: bool, horizontal: bool, size: Option<usize> }` (serde, stored in cell
  metadata and later in tile files; columns by name for persistence with index resolution at
  load) and `Agg`.
- `crates/cobalt-results`: `chart_data(rs, &spec, fmt, budget) -> ChartData` — the one pass
  over the view: X values (as `XKey`: number, timestamp, category text), per-series `Vec<f64>`
  (NaN for NULL), aggregation, top-N with "other", and the row ids behind each X (for click →
  select). Pure data, unit-tested, no egui.
- `crates/cobalt-app/src/ui/results/chart.rs`: the strip and the `egui_plot` rendering of a
  `ChartData` (bar / line / area / scatter via egui_plot, pie via painter), hover formatting,
  legend, the click → `ResultsAction::SelectRows`. `GridState` (or `ResultSetView`) gains
  `chart: Option<ChartView { spec, data: Option<Arc<ChartData>>, generation }>`, recomputed
  when the view generation changes (same rule as totals and the profiler; on a thread when the
  set is over ~50k rows, like the find scan).
- `ResultsAction::{Chart { set, on }, SetChart { set, spec }, CopyChartImage { set }}`;
  `ops.rs` applies them; the toolbar toggle next to Maximize; "Chart…" in the result-set menu.
- Notebook: the cell's `RunView` is the same type, so the toggle and strip are shared; saving
  writes `cobalt.chart` into the cell metadata and the PNG into outputs (`notebook::document`
  already rebuilds outputs from the run; it adds the picture when `chart.png` is set).
- Image capture: `state.pending_capture: Option<(TabId, set, Rect, Purpose)>`; the frame after
  the request sends `ViewportCommand::Screenshot`, the `Event::Screenshot` handler crops the
  rect at the pixel ratio, encodes PNG (`image` crate, already a dependency), and routes to the
  clipboard, a file dialog, or the cell's run.
- Agent: `chart {set?, kind, x, y: [..], agg?, top_n?, stacked?}` and `chart_state {set?}`
  (spec + the data's series lengths and the first few points), `copy {kind: "chart_png"}`.
  a11y labels: `chart toggle`, `chart kind <k>`, `chart x`, `chart y <col>`, `chart agg <a>`.

## 5. Slices

- **Slice A — a chart on a result set.** `ChartSpec` + `chart_data` with tests; the toggle,
  strip and rendering for bar / line / area / scatter / pie; seeding from the selection; hover,
  legend, zoom; click → rows; Copy image / Save as PNG through the screenshot path; follows
  sort/filter/re-run; agent verbs; alpha notes. Query tabs and Spark SQL tabs alike (both are
  `RunView`s).
- **Slice B — charts in notebook cells.** The same toggle on a cell's result; spec in cell
  metadata; `image/png` in the saved outputs; cells render `image/png` outputs they already
  carry (from Fabric notebooks too — matplotlib pictures start showing, a bonus).
- **Slice C — dashboard tiles (§5.6).** Only on the founder's word: tile = saved query +
  spec + interval; a tiles page per server/database home; a `.cobalt-dash.json` file.

## 6. Decisions for the founder (recommendations in bold)

1. Where the chart lives: **a per-result-set toggle (grid ⇄ chart) next to Maximize** vs. a
   fourth Results / Messages / Plan tab. The toggle keeps one chart per set and no new tab
   plumbing; Maximize + Chart gives a full-area chart.
2. Pie: **include (simple pie/donut with top-N + other)**, as the product design lists it; it is
   small and the one chart non-analysts ask for.
3. Row budget for charting: **200,000 visible rows, with "chart anyway"**; above that the right
   answer is a GROUP BY in the query, and the strip says so.
4. Image export: **the screenshot path (no second renderer)**; a headless renderer only if
   `cobalt run notebook` lands and needs pictures without a window.
5. Slice C timing: **after A and B ship, and only if you want dashboards at all** — your own
   usage says never.

## 7. Risks

- egui_plot's API moves with egui minors (the rest of the stack tracks 0.36 the same way; one
  more crate on the bump list).
- Wide results: 1,000 series makes no chart; the strip limits Y to 12 series and says so.
- Categories with thousands of distinct values: top-N (default 50 for bars, 12 for pies) with
  "other", never a 10,000-bar chart.
- The screenshot path captures what is on screen: a chart partly scrolled out of view would be
  clipped — the capture scrolls it into view first, or temporarily maximizes the set for the
  frame.
