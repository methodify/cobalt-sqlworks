# Fabric Explorer — design story

*Status: proposal, 2026-09-17. Targets a 0.2 release. Builds on the Fabric connectivity that shipped
in 0.1.0 (Warehouse, SQL analytics endpoint, SQL database in Fabric, Entra sign-in).*

## 1. The story

> Maya is a data engineer on a Fabric tenant with eleven workspaces. Every week she needs to look at a
> table in some warehouse she does not have a saved connection for. In Azure Data Studio that meant:
> open the Fabric portal, find the item, open its settings, copy the SQL connection string, paste it
> into a New Connection dialog, pick Entra auth, type a name, connect. In Cobalt she clicks the
> **Fabric** icon on the left rail, sees her workspaces, expands *Finance*, and double-clicks the
> *Sales_DW* warehouse. A query tab opens, connected. She pins it. Next week it is one click.

The Servers panel is where you keep the things you *own*: named, grouped, colored connections that
you built by hand. The Fabric panel is where you *discover* what your identity can reach right now.
They are complementary, and the second one can feed the first ("Save to Servers").

Nothing like this exists in ADS. SSMS 22 and the VS Code MSSQL extension added Fabric browsing in
2025/26, so for the target audience this is the difference between "a nice ADS replacement" and
"the tool that understands Fabric".

## 2. Goals and non-goals

Goals

1. Zero-config discovery: sign in once with Entra, see every workspace and every SQL-capable item
   you can access, connect with one action.
2. Favourites: pin items (or whole workspaces) so the panel opens to what you use daily.
3. Feed the connection library: "Save to Servers" turns a discovered item into a normal profile.
4. Same identity everywhere: the Entra account already cached for SQL sign-in is reused for the
   Fabric REST API; no second login unless consent is missing.
5. Fast and cheap: the tree loads lazily, caches locally, and stays well under the API throttle
   (200 calls/min per principal).

Non-goals (this story)

- Creating, renaming or deleting Fabric items. Read-only.
- OneLake file browsing, Spark, notebooks, pipelines. (OneLake *export* is a separate backlog item
  that this panel makes discoverable, see §9.)
- Power BI semantic models, KQL databases, Eventhouses. They are listed only if a later story adds
  the engine for them.

## 3. What Fabric exposes (verified against the REST reference, 2026-09)

All calls go to `https://api.fabric.microsoft.com/v1` with a bearer token whose audience is
`https://api.fabric.microsoft.com`.

| Need | Call | Scope | Notes |
|---|---|---|---|
| Workspaces I can access | `GET /workspaces` (paged, `roles=` filter, `continuationToken`) | `Workspace.Read.All` | Returns id, displayName, type (Personal/Workspace/AdminWorkspace), capacityId, capacityRegion, domainId, tags |
| Items in a workspace | `GET /workspaces/{ws}/items?type=…` (paged) | `Workspace.Read.All` | One call per SQL-capable type, or one unfiltered call and filter locally (fewer calls) |
| Warehouse connection | `GET /workspaces/{ws}/warehouses/{id}` | `Item.Read.All` (or `Warehouse.Read.All`) | `properties.connectionString` = host (`…datawarehouse.fabric.microsoft.com`), `collationType`, created/updated |
| Lakehouse SQL endpoint | `GET /workspaces/{ws}/lakehouses/{id}` | `Item.Read.All` | `properties.sqlEndpointProperties{connectionString,id,provisioningStatus}`, `oneLakeTablesPath`, `defaultSchema` (schema-enabled lakehouses) |
| SQL database in Fabric | `GET /workspaces/{ws}/sqlDatabases/{id}` | `Item.Read.All` | `properties.serverFqdn` (`…database.fabric.microsoft.com,1433`), `databaseName`, full `connectionString`, collation, restore points |
| Mirrored database | `GET /workspaces/{ws}/mirroredDatabases/{id}` | `Item.Read.All` | `properties.sqlEndpointProperties`, `oneLakeTablesPath`, `defaultSchema` |
| SQL endpoint item | `GET /workspaces/{ws}/sqlEndpoints/{id}` (item type `SQLEndpoint`) | `Item.Read.All` | The endpoint as its own item; same host as the parent lakehouse/mirror. Listed under its parent, not twice |

SQL-capable item types: `Warehouse`, `Lakehouse` (via its SQL endpoint), `SQLDatabase`,
`MirroredDatabase`, `MirroredWarehouse`, `MirroredAzureDatabricksCatalog` / `MirroredCatalog` (SQL
endpoint), `WarehouseSnapshot`, `Datamart` (legacy, has a SQL endpoint; low priority). Everything
else in the 50-value `ItemType` enum is ignored.

Database name rules, learned in 0.1.0 testing: for a Warehouse the database is the item's display
name; for a Lakehouse/mirror SQL endpoint it is the item's display name too; for a SQL database it is
`properties.databaseName` (`<name>-<guid>`), which the user never has to see.

Throttle: 200 calls/min per principal for platform APIs. A 30-workspace tenant with one unfiltered
`items` call per workspace plus one detail call per SQL item is well inside that, and we cache.

### 3.1 Auth: one identity, two audiences

The SQL token has audience `https://database.windows.net/`. The Fabric API needs a second token
for `https://api.fabric.microsoft.com`. Both come from the same refresh token **if the user has
consented to the Fabric permissions**. Tested 2026-09-17 against the founder's tenant: exchanging the
cached refresh token for `https://api.fabric.microsoft.com/.default` fails with `AADSTS65001
consent_required` until the app registration declares the permissions.

Registration change (one-time, founder): App registrations → Cobalt Sql Works → API permissions →
Add → **Power BI Service** → Delegated → `Workspace.Read.All`, `Item.Read.All`. No admin consent is
needed for these; the user consents once. Because our sign-in requests `.default`, the next
interactive sign-in (or the Fabric panel's first use) shows one consent screen covering SQL and Fabric,
and after that the refresh token silently yields both tokens.

Design: `cobalt-auth` grows an `audience` parameter on token acquisition (`Sql`, `FabricApi`,
later `OneLake` = `https://storage.azure.com/`), keyed in the in-memory cache per (profile/account,
audience). The Fabric panel is tied to an **account**, not a connection profile: it uses the most
recently signed-in Entra account, or asks the user to sign in (same dialog as today).

## 4. UX

### 4.1 Left rail

A third icon under Servers and History: **Fabric** (the Fabric diamond glyph, or our spark).
Ctrl+Shift+B. The panel has three states:

1. **Signed out** — "Browse your Fabric workspaces" + *Sign in with Microsoft*. If an Entra account
   is already cached from a SQL profile, the button reads *Continue as maya@contoso.com* and only
   prompts if consent is missing.
2. **Loading / error** — inline; 401/403 offers *Sign in again*; 429 shows "Fabric asked us to slow
   down, retrying in 12 s" and honours `Retry-After`.
3. **Tree**.

### 4.2 Tree

```
★ Pinned
    Sales_DW                      warehouse · Finance
    lh_bronze (SQL endpoint)      lakehouse · Data Platform
Workspaces                                            [search] [↻]
  ▸ My workspace                 personal
  ▾ Finance                      F64 · East US
      Sales_DW                   warehouse
      Sales_LH                   lakehouse → SQL endpoint
      crm_mirror                 mirrored database
      appdb                      SQL database
  ▸ Data Platform                F64 · East US
  ▸ Marketing                    (no SQL items)
```

- Workspaces sorted by name, personal first; capacity and region as muted subtitle when known
  (`capacityId` → name needs `GET /capacities`, one call, cached).
- Items show a type icon and label: warehouse, lakehouse, SQL database, mirrored database. A
  lakehouse whose SQL endpoint is still `InProgress` shows a spinner and is not connectable yet;
  `Failed` shows a warning.
- Workspaces with zero SQL-capable items collapse to a muted "(no SQL items)".
- Search box filters workspaces and items by name as you type (local, over the cache).
- Row actions (double-click / Enter / context menu):
  - **Open query** — new tab connected to the item (default).
  - **Expand** — chevron on an item expands its object explorer *inline*, reusing the Servers tree
    code (databases → tables → columns). Same node types, same a11y labels.
  - **Pin / Unpin**.
  - **Save to Servers…** — opens the connection editor prefilled (server, database, Entra auth
    with the account hint, name = "Finance / Sales_DW", color by workspace).
  - **Copy connection string**, **Open in Fabric portal** (`https://app.fabric.microsoft.com/groups/{ws}/…`).
- Keyboard: arrows, Enter opens, Ctrl+P pins, F5 refreshes the node.

### 4.3 Connecting

An opened item becomes an **ephemeral profile**: a `ConnectionProfile` with `origin: Fabric{workspace_id,
item_id, item_type}` that is not stored in the library unless "Save to Servers" is used. Tabs show
`Sales_DW · Finance` in the title. Ephemeral profiles reuse everything (metadata actor, completion
catalog, history). History entries record the origin so *Restore closed tab* and *Recent* still work
next session, re-resolving the item by id (renames follow).

Pins are stored (SQLite `fabric_pins`: workspace_id, item_id, item_type, display cache, order).
The pinned list is shown even before the tree loads, from the display cache; a stale pin (item deleted
or access lost) greys out with "not found — unpin?".

### 4.4 Discovery → library

"Save to Servers" is the bridge. Suggested defaults: group = workspace name (created on demand),
color = derived from workspace id, name = item name. This means the Servers panel can become a
curated subset of what Fabric shows, which is what people do in practice.

## 5. Architecture

New crate **`cobalt-fabric`** (pure Rust, no egui): REST client over `reqwest` with the bearer
token supplied by `cobalt-auth`, typed models for the subset above, pagination, `Retry-After`
handling, and a `FabricCatalog` in-memory model with a SQLite-backed cache (`fabric_cache` table:
workspace + items JSON, `fetched_at`). Unit tests against recorded JSON fixtures from the founder's
tenant (ids scrubbed).

App side: a `fabric` metadata actor next to the existing per-profile metadata actors: requests
`ListWorkspaces`, `ListItems{ws}`, `ItemDetail{ws,id,type}`; events fill `AppState.fabric`. The UI
module `ui/fabric.rs` renders the panel; expansion of an item's object tree hands off to the existing
servers-tree renderer with an ephemeral `ServerNode`.

Token flow: panel asks `CredentialResolver::token_for(account, Audience::FabricApi)`; on
`InteractionRequired` it shows the sign-in state with the account hint, and the interactive flow
requests `.default` for the Fabric resource (consent for everything declared).

Cache policy: workspaces and items cached 15 minutes, item detail (connection strings) 24 hours,
refresh button forces. Everything shown instantly from cache on open, refreshed in the background
("last refreshed 3 min ago" in the footer).

## 6. Edge cases

- **Personal accounts / tenants without Fabric**: `GET /workspaces` returns 401/403 or an empty
  list; show "No Fabric workspaces for this account" with a switch-account action.
- **Multiple tenants**: the account's home tenant is used; a tenant picker (from the id token's
  `tid` plus a manual entry) is a follow-on. Guest (B2B) access works via the tenant override.
- **Conditional access blocking the public client**: same message and device-code fallback as
  today's SQL sign-in.
- **Capacity paused**: item detail still lists, connecting fails with the engine's message; surface
  it in the row after a failed connect.
- **SQL endpoint provisioning**: `InProgress` items are visible but disabled.
- **Renames**: the item id is the key; names refresh on reload; pins update their display cache.
- **Private link / workspace-specific endpoints**: `preferWorkspaceSpecificEndpoints=True` gives an
  `apiEndpoint` per workspace; honour it when present.

## 7. Beyond the basics (ideas to grow into)

- **Recent Fabric items** across sessions, sorted by last use, at the top of the panel.
- **Workspace scope for Global object search**: search tables across every warehouse you can reach.
- **OneLake export target**: the Delta export dialog offers "…to OneLake" with a picker rooted at
  the lakehouse `oneLakeTablesPath` (needs the `https://storage.azure.com/.default` audience). This
  turns Cobalt into the missing "query a warehouse, land a Delta table in a lakehouse" tool.
- **Query Insights**: a per-item "Recent queries" view over `queryinsights.exec_requests_history`
  showing duration, CU seconds, rows, and a one-click "open this query".
- **Capacity awareness**: show the capacity SKU and region next to a workspace; warn before running
  against a paused capacity.
- **Cross-item queries**: since a warehouse can three-part-name other items in the same workspace,
  offer "Insert cross-database reference" from the tree.
- **Item health**: SQL endpoint sync status for lakehouses (`GET .../sqlEndpoints/{id}/refreshMetadata`
  is a write; reading the last sync time is enough), mirroring status for mirrored databases.
- **Tenant switcher** and multiple accounts side by side.

## 8. Rollout

1. **0.2.0 — Explorer MVP**: registration permissions; second audience in cobalt-auth; `cobalt-fabric`
   with workspaces + items + details; panel with tree, search, open query, pin, save to servers,
   copy connection string, open in portal; ephemeral profiles; cache. Read-only.
2. **0.2.x**: inline object explorer expansion, recent items, capacity names, workspace-specific
   endpoints, tenant picker.
3. **0.3**: OneLake export target, Query Insights view.

Acceptance for the MVP (agent-verifiable): sign in once; panel lists every workspace the test
account can see; the founder's `test`/`warehouse` warehouses and the `sqldb-…` SQL database appear
with correct types; opening each yields a connected tab whose engine matches; pin survives restart;
"Save to Servers" creates a working profile; 429 handling exercised with a mocked server.

## 9. Open questions for the founder

- Panel name: "Fabric" (clear) vs "Workspaces" (room for Azure later)? Recommendation: **Fabric**.
- Should opening an item always create a new tab, or reuse an existing tab on the same item?
  Recommendation: new tab, like Servers.
- Should Fabric items appear inside the Servers tree as a virtual group instead of a separate panel?
  Recommendation: separate panel; the mental models differ (curated vs discovered), and it keeps the
  Servers tree fast for people without Fabric.
- Pin storage: local only (SQLite) for now; roaming later if ever.
