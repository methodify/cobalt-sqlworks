# How ADOMD.NET talks to a Power BI Premium / Fabric XMLA endpoint

Research note, 2026-10-14. Source: decompilation of the NuGet packages
`Microsoft.AnalysisServices.AdomdClient.NetCore.retail.amd64` 19.84.1 and
`Microsoft.AnalysisServices.NetCore.retail.amd64` 19.84.1 (ilspycmd 9.1.0.7988 on the .NET 9
runtime; no SDK installed, so the tool's `net8.0` build was run with `DOTNET_ROLL_FORWARD=Major`).
Decompiled sources live in the session scratchpad under `adomd/decomp/`. Class and method names
below are from `Microsoft.AnalysisServices.AdomdClient.dll` unless noted; the AMO transport
(`Microsoft.AnalysisServices.Core.dll`, `HttpStream.cs`) is byte-for-byte the same apart from
exception type names (106 diff lines, all `AdomdConnectionException` vs `ConnectionException`).

## 1. The answer in one paragraph

The official client does **not** send the Entra bearer token to the dedicated cluster. For a
`powerbi://` data source it exchanges the Entra token for a **MWC token** at
`POST https://api.powerbi.com/metadata/v201606/generateastoken` and sends
`Authorization: MwcToken <token>` to the XMLA endpoint. That token carries the workspace, capacity
and dataset, which is why the default client sends **no** `x-ms-xmlaworkspaceobjectid` and **no**
`x-ms-xmladatabase` header at all: `x-ms-xmlaworkspaceobjectid` is only emitted in a debug mode
(`AS_USE_AAD_TOKEN_PUBLIC_XMLA_EP` set, bearer token used), and `x-ms-xmladatabase` is only emitted
for the *internal* `pbidedicated://` instance type that Power BI's own services use. Cobalt's current
request (bearer token + workspace/database headers) is therefore the undocumented AAD-mode shape,
and the "internal error" is consistent with the cluster failing to resolve the identity/scope it
expects from the MWC token. Two secondary differences: the client resolves the cluster first with an
**unauthenticated** `POST https://<rollout fqdn>/webapi/clusterResolve` whose answer gives both the
host to post to and the value of `x-ms-xmlaserver` (the `coreServerName`, which may differ from
the capacity object id), and `<Catalog>` in the SOAP body is the **friendly model name**, not the
dataset guid.

## 2. Call graph

```
AdomdConnection.Open()
  -> XmlaClientProvider.ConnectXmla() -> XmlaClient.Connect(info, beginSession:false)
       -> ConnectionInfo.HandlePaaSInfrastructureConnection  (instance type from the URI scheme)
       -> XmlaClient.OpenConnection -> OpenHttpConnection -> HttpStream.Create
            -> ConnectionInfo.AcquireAADTokenAndResolveHTTPConnectionPropertiesForPaaSInfrastructure
                 -> ResolveHTTPConnectionPropertiesForPaaSInfrastructure            (ConnectionInfo.cs)
                      -> AcquireToken (MSAL, resource https://analysis.windows.net/powerbi/api)
                      -> PbiPremiumAuthenticationHandle.TryResolvePbiWorkspace     (GET workspaces)
                      -> PbiPremiumAuthenticationHandle.TryGetDatabaseName         (POST getDatabaseName)
                      -> new PbiPremiumAuthenticationHandle(...)  => GetMwcToken   (POST generateastoken)
                      -> ASAzureUtility.ResolvePaaSConnectionEndpointDetail        (POST clusterResolve)
            -> HttpChannelManager.GetControllerAndUserContext => PaasInfraController + AuthenticatedUserContext
  -> AdomdConnection.ConnectToXMLA(createSession:true, isHTTP:true)
       -> XmlaClientProviderEx.CreateSession(sendNamespaceCompatibility:true)
            -> XmlaClient.CreateSession => first XMLA request: Execute with <BeginSession>
```

### 2.1 Data source parsing

`ConnectivityHelper.GetAsInstanceType` (Runtime.Core):

| scheme | AsInstanceType | used by |
|---|---|---|
| `asazure://` | `AsAzure` | Azure Analysis Services |
| `pbidedicated://` | `PbiPremiumInternal` | Power BI's own services (needs `x-ms-xmladatabase`, bypass headers) |
| `powerbi://` | `PbiPremiumXmlaEp` | **the public XMLA endpoint, our case** |
| `pbiazure://` or `https://analysis.windows.net/powerbi/api` | `PbiDataset` | Analyze-in-Excel / shared capacity |

`ASAzureUtility.ExtractPbiPublicXmlaTenantAndWorkspace("powerbi://api.powerbi.com/v1.0/myorg/Fabric test")`:
segment 1 must be `v1.0`; segment 2 is the tenant (`myorg` -> null); the rest joined with `/` is the
workspace name. `Initial Catalog` (or `Catalog`/`Database`) sets `ConnectionInfo.Catalog`, which is
also written into the XMLA `<PropertyList>` as `<Catalog>` (`MDXMLAPropInfo`: `Initial Catalog` ->
`Catalog`). An optional `?readonly` / `?readwrite` query on the data source sets `PbipAccessMode`.

### 2.2 Entra token

`AuthenticationInformation.FindMatchingAuthenticationInformation` matches the data-source host
against the embedded `ASAzureSecurityConfig.xml`. For `api.powerbi.com`:

```
Authority     https://login.microsoftonline.com/common   (v2: .../organizations)
ApplicationId cf710c6e-dfcc-4fa8-a093-d47294e44c66        (Microsoft's "Power BI / AS client" app)
ResourceId    https://analysis.windows.net/powerbi/api
```

MSAL scope `https://analysis.windows.net/powerbi/api/.default`, redirect URI
`https://login.microsoftonline.com/common/oauth2/nativeclient`. This is the same audience Cobalt uses.
For `*.pbidedicated.windows.net` the table has no ResourceId, so `AuthenticationManager.GetAuthenticationInformation`
would default to `https://<host>`; that path is only taken for `pbidedicated://` internal connections.

### 2.3 Workspace resolution (`PbiPremiumAuthenticationHandle`)

All api.powerbi.com calls go through `ExecuteJsonBasedHttpRequestWithPreferClientRoutingImpl`:
`Authorization: Bearer <entra>`, `RequestId: <activity guid>`, `User-Agent: ASClient/.NET-Core`,
`Content-Type: application/json`, query string `?PreferClientRouting=true`; a `307` with `Location`
is followed up to 3 times and the redirected host is cached per user ("home tenant uri").

Default (non-EmbedToken) path, `TryResolveWorkspaceWithWorkspaceResolver`:

```
GET https://api.powerbi.com/powerbi/databases/v201606/workspaces?PreferClientRouting=true
```

returns the list of all workspaces; the client lower-cases names and matches
`Uri.UnescapeDataString(workspaceName).ToLower()`; a `type == "User"` entry is the personal
workspace (used when the name is empty); duplicate names are disambiguated as `<name>-<id>`.
The single-workspace form `GET .../workspaces/{name}` (which Cobalt confirmed live) is only used when
the auth scheme is `EmbedToken`; both return the same `Workspace201606` record:

```json
{"id","name","type","capacitySku","capacityUri"}
```

`TryResolvePbiWorkspace` then, when `capacitySku == "Premium"`:

```csharp
Uri uri = new Uri(workspace.CapacityUri);     // pbidedicated://westus.pbidedicated.windows.net/<cap guid>
capacityObjectId = uri.Segments[1];           // "ba8d4c98-e720-4344-91d4-fb18bdc13a17"
pbiDedicatedRolloutFqdn = uri.DnsSafeHost;    // "westus.pbidedicated.windows.net"
```

### 2.4 Database name (`TryGetDatabaseName`)

```
POST https://api.powerbi.com/powerbi/databases/v201606/workspaces/<ws guid>/getDatabaseName?PreferClientRouting=true
{"datasetName":"RetailDemo","workspaceType":1}          // workspaceType: DataContractJsonSerializer writes the enum as int; Group = 1
-> [{"databaseName":"<dataset guid>","datasetName":..,"versionEtag":..,"virtualServerName":"sobe_wowvirtualserver","creatorUserPrincipalName":..,"isAvailableOnPremiumCapacity":true}]
```

Important: on the Premium path (`ResolveHTTPConnectionPropertiesForPaaSInfrastructure`,
ConnectionInfo.cs ~2412) the result is used **only** to read `isAvailableOnPremiumCapacity`. The
`databaseName` guid is discarded. `SetCatalog("sobe_wowvirtualserver-" + guid)` and the switch to
`AsInstanceType.PbiDataset` (plain https to api.powerbi.com with `?vs=sobe_wowvirtualserver&db=...`)
only happen for **Shared** capacity. For Premium, `Catalog` stays `RetailDemo`.

### 2.5 MWC token (`PbiPremiumAuthenticationHandle.GetMwcToken`)

```csharp
public override string AuthenticationScheme => "MwcToken";
...
MWCASTokenRequest requestBody = new MWCASTokenRequest {
    CapacityObjectId = targetCapacityObjectId, WorkspaceObjectId = targetWorkspaceObjectId,
    DatasetName = targetDatabaseName,            // the Catalog (friendly name), may be null
    ApplyAuxiliaryPermission = ..., AuxiliaryPermissionOwner = ..., BypassBuildPermission = ...,
    IntendedUsage = permissionInfo.IntendedUsage, SourceCapacityObjectId = ... };
item = ExecuteJsonBasedHttpPostRequestWithPreferClientRouting<MWCASTokenRequest, MWCToken>(
    new Uri(string.Format("https://{0}/metadata/v201606/generateastoken", pbiApiBaseUri)), ...);
```

Request (members have no `Order`, so DataContractJsonSerializer emits them alphabetically; nulls are
emitted because `EmitDefaultValue` is left at its default):

```
POST https://api.powerbi.com/metadata/v201606/generateastoken?PreferClientRouting=true
Authorization: Bearer <entra token for https://analysis.windows.net/powerbi/api>
Content-Type: application/json
User-Agent: ASClient/.NET-Core

{"applyAuxiliaryPermission":false,"auxiliaryPermissionOwner":null,"bypassBuildPermission":false,
 "capacityObjectId":"ba8d4c98-e720-4344-91d4-fb18bdc13a17","datasetName":"RetailDemo",
 "intendedUsage":0,"sourceCapacityObjectId":null,"workspaceObjectId":"eb24c8db-6189-4c38-94d3-390fabe47782"}
```

Response: `{"Token":"<opaque>"}` (`[DataContract] MWCToken { [DataMember] string Token }`, so the
JSON property is `Token`, capital T). Cached 5 minutes keyed on
`mwctoken|<api>|<ws>|<cap>|<entra token>|<catalog>|<profile id>`; refreshed when the underlying Entra
handle's refresh-by time passes (`GetAccessToken()`). Optional headers on this call:
`x-ms-xls2stoken` (service-to-service token) and `X-PowerBI-profile-id` (service principal profile).
`IntendedUsage` enum: `Default=0, ScheduledProcessing=1, InteractiveProcessing=2, BackgroundQuery=3`.

The handle is then used as the connection's auth handle:

```csharp
authHandle = (UseAadTokenInPublicXmlaEP ? authenticationHandle
             : new PbiPremiumAuthenticationHandle(authenticationHandle, dataSourceUri.Authority,
                   workspaceObjectId, capacityObjectId, Catalog, permissionInfo));
PaaSInfrastructureServerName = capacityObjectId;
PbiPremiumWorkspaceObjectId = workspaceObjectId;
```

### 2.6 Cluster resolution (`ASAzureUtility.ResolvePaaSConnectionEndpointDetail`)

```csharp
AsPaasEndpointInfo asPaasEndpointInfo2 = ASAzureUtility.ResolvePaaSConnectionEndpointDetail(
    AsInstanceType, new Uri(ASAzureUtility.ConstructPbiPremiumServerConnUri(pbiDedicatedRolloutFqdn)),
    PaaSInfrastructureServerName /* cap guid */, null, premiumPublicXmlaEndpoint: true, ref timeLeft, ...);
dataSourceUri = asPaasEndpointInfo2.Cluster;     // https://<clusterFQDN>/webapi/xmla
paasCoreServerName = asPaasEndpointInfo2.Server; // -> x-ms-xmlaserver
```

`ConstructPbiPremiumServerConnUri` = `https://{fqdn}/webapi/xmla`; the resolver swaps the path:

```
POST https://westus.pbidedicated.windows.net/webapi/clusterResolve
Content-Type: application/json
User-Agent: ASClient/.NET-Core
x-ms-parent-activity-id: <activity guid>          (only if the connection has one)
(no Authorization header)

{"databaseName":null,"premiumPublicXmlaEndpoint":true,"serverName":"ba8d4c98-e720-4344-91d4-fb18bdc13a17"}

-> {"clusterFQDN":"<host>","coreServerName":"<server>","tenantId":"<tenant guid>"}
```

Then `Cluster = new UriBuilder(dataSourceUri){ Host = clusterFQDN }.Uri` (path `/webapi/xmla` kept)
and `Server = coreServerName`. Result cached 60 min sliding in a process-shared cache keyed on
`"{dataSourceUri} + {serverName}"`; a 3xx from the XMLA endpoint clears it and raises
"Analysis Services instance was moved" (`PaasInfraController.HandleResponseStatusCode`).

Azure AS takes the identical shape with `premiumPublicXmlaEndpoint:false`, `serverName` = the
instance name from `asazure://<region>.asazure.windows.net/<server>` and the POST going to
`https://<region>.asazure.windows.net/webapi/clusterResolve` (`databaseName` is only set for
`pbidedicated://` internal connections, where it is the Catalog). The Azure AS XMLA request then
carries `Authorization: Bearer <entra for https://<region>.asazure.windows.net>` — bearer is the
scheme for AsAzure and for PbiPremiumInternal; MwcToken is specific to `powerbi://`.

Before any of this, an `asazure://` source is checked for redirection to a Power BI workspace
(`ConnectionInfo.HandleAsAzureRedirection` -> `ASAzureUtility.IsAsAzureRedirectedToPowerBIWorkspace`):
`POST https://api.powerbi.com/AASRedirect/public/mappings/exists {"aasInstance":"asazure://..."}`
(no auth, `RequestId` header); 200 `{tenantId}` means redirected, 404 means not; when redirected,
`POST https://api.powerbi.com/AASRedirect/mappings/lookup` (bearer) returns `{"pbiWorkspace":"powerbi://..."}`
and the connection proceeds as `powerbi://` with `PbipAccessMode` from the query.

### 2.7 The HTTP channel (`HttpStream.PaasInfraController`)

`HttpChannelManager.ChannelOptions`: `info.IsPaaSInfrastructure` selects `HttpChannelMode.PaasInfra`
and `context = new AuthenticatedUserContext(info.AuthHandle)`, whose `UpdateHttpRequestImpl` adds
`Authorization: {handle.AuthenticationScheme} {handle.GetAccessToken()}` on every request.
`PaasInfraController : HttpChannelController` (not `LegacyController`), so the legacy per-request
headers — `SOAPAction`, `X-Transport-Caps-Negotiation-Flags`, `X-AS-SessionID`,
`X-AS-GetSessionToken`, `X-AS-ActivityID`, `X-AS-RequestID`, `X-AS-CurrentActivityID`, `X-AS-Routing`
— are **never sent** to a PaaS endpoint. There is no `X-AS-GetSessionToken` handshake
(`PrepareWebRequestImpl` removes it in PaasInfra mode; `isSessionTokenNeeded` is only true for client
certificates or `IntegratedSecurity=Federated`).

Transport: `HttpClient` over `SocketsHttpHandler`, `POST`, chunked transfer encoding,
`Connection: Keep-Alive`, gzip auto-decompression only if `AllowAutoRedirect`-style option set
(`options.AllowGzipCompression`), TCP keep-alive.

## 3. The exact first request to the dedicated cluster (our test case)

Inputs: workspace "Fabric test", ws `eb24c8db-6189-4c38-94d3-390fabe47782`, capacity
`ba8d4c98-e720-4344-91d4-fb18bdc13a17`, rollout `westus.pbidedicated.windows.net`, model
`RetailDemo`. Connection string equivalent:
`Data Source=powerbi://api.powerbi.com/v1.0/myorg/Fabric test;Initial Catalog=RetailDemo;User ID=...;Password=...`
(or an external access token).

Preconditions, in order: Entra token -> `GET .../v201606/workspaces` (or `/workspaces/Fabric%20test`)
-> `POST .../getDatabaseName` (result only used to confirm the dataset is on capacity) ->
`POST /metadata/v201606/generateastoken` -> `POST https://westus.pbidedicated.windows.net/webapi/clusterResolve`
giving `clusterFQDN` + `coreServerName`.

```
POST https://<clusterFQDN>/webapi/xmla
```

| header | value | source |
|---|---|---|
| `Authorization` | `MwcToken <token from generateastoken>` | `AuthenticatedUserContext.UpdateHttpRequestImpl`; scheme `PbiPremiumAuthenticationHandle.AuthenticationScheme` |
| `User-Agent` | `ASClient/.NET-Core` | `HttpChannelController.GetDefaultStreamHeaders` |
| `X-AS-AcquireTokenStats` | `AppName=` | same |
| `x-ms-xmlaserver` | `<coreServerName from clusterResolve>` (for the capacity id `ba8d4c98-…`; may equal it, not guaranteed) | `PaasInfraController.GetDefaultStreamHeaders` |
| `x-ms-xmlaapp-general-info` | `AppName=<proc>.exe,AppVer=<ver>,AppBinaryType=6,HostRuntime='.NET 9.0.1',ManagedVer=Microsoft.AnalysisServices.AdomdClient:19.84.1.0 [.NET Core],ClientLibVer=Microsoft.AnalysisServices.AdomdClient:19.84.1.0 [.NET Core],AadClient=MSAL,CacheUsed=0,ConStrId=1[,sspropInitApp=<Application Name>]` | `AsPaasHelper.BuildGeneralInfoHeader` |
| `x-ms-xmlaclienttraits` | `1` | `info.IsBinarySupported` is true for PbiPremiumXmlaEp/Internal |
| `x-ms-xmladedicatedconnection` | `0` | `info.DedicatedAdminConnection` |
| `x-ms-xmlareadonly` | **absent** unless the data source had `?readonly` (`1`), `?readwrite` (`0`) or an AIXL conversion (`2`); `ConnectionAccessMode.Default` writes nothing | `PaasInfraController.GetDefaultStreamHeaders` switch |
| `x-ms-xmlaworkspaceobjectid` | **absent** (only when `AS_USE_AAD_TOKEN_PUBLIC_XMLA_EP` is set) | same |
| `x-ms-xmladatabase` | **absent** (only for `pbidedicated://` PbiPremiumInternal) | same |
| `x-ms-xmlaintendedusage` | **absent** for `powerbi://` (sent for PbiPremiumInternal and PbiDataset only) | same |
| `x-ms-routing-scenario` | **absent** unless IntendedUsage is a processing mode (`Processing`) | same |
| `x-ms-xmlacontextualidentity` | **absent** unless `ContextualIdentity` set | same |
| `Content-Type` | `text/xml` on the first request (not yet negotiated) | `HttpChannelController.GetPerRequestHeaders`; `TransportCapabilitiesAwareXmlaStream.GetRequestDataType` |
| `x-ms-xmlacaps-negotiation-flags` | `0,0,0,1,1` on Windows x64 (bits: ContentTypeNegotiated, RequestBinary, RequestCompression, ResponseBinary, ResponseCompression; response binary+compression are *desired* because `msasxpress.dll` ships in the package and binary XML is enabled by default on .NET >= 8). `0,0,0,0,0` is legal and yields text/xml responses | `PaasInfraController.GetPerRequestHeaders`, `TransportCapabilities.GetString` |
| `x-ms-request-registration-id` | new guid per operation | `XmlaOperationContext` |
| `x-ms-accepts-continuations` | `1` | same |
| `x-ms-round-trip-id` | `0` (incremented under the long-running-operation protocol) | same |
| `x-ms-parent-activity-id` | the connection's `RequestID` or a fresh guid | same |
| `x-ms-xmlasession-id` | **absent** on the first request; `<SessionId>` afterwards | same |
| `x-ms-root-activity-id` | absent on the first request; echoed once the server returns one | same |
| `x-ms-workload-resource-moniker`, `x-ms-routing-hint` | absent on the first request; echoed from the previous `200` response's headers of the same name (cleared on non-200) | `ProcessWebResponse` |
| `Transfer-Encoding` / `Connection` | `chunked` / `Keep-Alive` | `InitializeHttpClient` |

Not sent: `SOAPAction`, `X-Transport-Caps-Negotiation-Flags`, `X-AS-*` (other than AcquireTokenStats),
`x-ms-xmlaerror-extended` (that is a *response* header the client reads for the error message),
`x-ms-src-capacity-id` (PbiPremiumInternal only).

Body — `AdomdConnection.ConnectToXMLA(createSession:true)` -> `XmlaClient.CreateSession(props, sendNamespaceCompatibility:true, "")`:

```xml
<Envelope xmlns="http://schemas.xmlsoap.org/soap/envelope/">
  <Header>
    <BeginSession soap:mustUnderstand="1" xmlns="urn:schemas-microsoft-com:xml-analysis" xmlns:soap="http://schemas.xmlsoap.org/soap/envelope/" />
    <Version Sequence="922" xmlns="http://schemas.microsoft.com/analysisservices/2003/engine/2" />
    <NamespaceCompatibility xmlns="http://schemas.microsoft.com/analysisservices/2003/xmla" mustUnderstand="0"/>
  </Header>
  <Body>
    <Execute xmlns="urn:schemas-microsoft-com:xml-analysis">
      <Command>
        <Statement></Statement>
      </Command>
      <Properties>
        <PropertyList>
          <Catalog>RetailDemo</Catalog>
          <LocaleIdentifier>1033</LocaleIdentifier>
          <!-- SspropInitAppName and ClientProcessID only when "Application Name" is in the connection string -->
          <DbpropMsmdActivityID>{guid}</DbpropMsmdActivityID>
          <DbpropMsmdCurrentActivityID>{guid}</DbpropMsmdCurrentActivityID>
        </PropertyList>
      </Properties>
    </Execute>
  </Body>
</Envelope>
```

Notes on the body:

- `WriteBeginSession` / `WriteVersionHeader` / the raw `NamespaceCompatibility` string are in
  `XmlaClient.StartMessage`. `Version Sequence="922"` is constant.
- `PropertyList` = `ConnectionInfo.ExtendedProperties` (every connection-string key that is not a
  known setting lands there under its XMLA name via `MDXMLAPropInfo`) plus the activity ids.
  `LocaleIdentifier` is always added (`CultureInfo.CurrentCulture.LCID`); `ClientProcessID` only when
  `SspropInitAppName` is present. `DbpropMsmdActivityID`/`DbpropMsmdCurrentActivityID` are added
  because `supportsActivityIDAndRequestID`/`supportsCurrentActivityID`/`supportsApplicationContext`
  are pre-set true for any PaaS connection (`XmlaClient.Connect`). `ApplicationContext` is only
  written if the caller set it. No `DataSourceInfo`, no `Timeout`, no `Format`/`Content` on this
  request (those appear on Discover: `DiscoverProperties` adds `Content=SchemaData`, `Format=Tabular`).
- The response's `<Session SessionId="…">` header is stored and sent back both as the SOAP
  `<Session soap:mustUnderstand="1" SessionId="…"/>` header and as `x-ms-xmlasession-id`.
- The **second** request is `Discover DISCOVER_PROPERTIES` with restriction `PropertyName=Catalog`
  (`GetProperty("Catalog", sendNSCompatibility:true)` is what the non-session branch does;
  with a session the connection goes straight to `DISCOVER_SCHEMA_ROWSETS` on first schema use).
  The first request type is therefore an **Execute with BeginSession**, not a Discover.

Response handling specific to the dedicated cluster (`PaasInfraController`):

- `ProcessWebResponse` requires `x-ms-xmlacaps-negotiation-flags` in the response
  (`InvalidDataException` otherwise) and stores it on the operation context; the stream's
  `TransportCapabilities` are then updated so later requests may switch to `application/sx+xpress`.
- `ClassifyWebError`: a `400` with `x-ms-wrong-node: 1` is `Ignore` (the response is read as if it
  were a success; the routing hint from the next 200 corrects the node); under the LRO protocol a
  `500`/`503` is retried with back-off `100,200,400,…,10000 ms`; anything else becomes
  `AdomdConnectionException` built by `ASAzureUtility.GetConnectionException`, which prefers the
  `x-ms-xmlaerror-extended` header, else the JSON body `{"code","message","subCode","httpStatusCode"}`
  (codes `ServerPausedOrScaling`, `ServerNotReady`, `CapacityNotActive`, `XmlaEndpointDisabled`,
  `ServerNotFoundForConnectionMode`), appending `x-ms-root-activity-id` / `x-ms-current-utc-date`.
- `x-ms-core-service-exhausted` does not appear anywhere in the client.
- Any 3xx: invalidate the clusterResolve cache and fail with "instance was moved".

## 4. What Cobalt does differently (from `crates/cobalt-tabular/src/xmla/resolve.rs`, not modified)

| | ADOMD default | Cobalt today |
|---|---|---|
| Authorization | `MwcToken <generateastoken result>` | `Bearer <entra>` |
| cluster host | `clusterFQDN` from `clusterResolve` on the rollout fqdn | rollout fqdn directly (`westus.pbidedicated.windows.net`) |
| `x-ms-xmlaserver` | `coreServerName` from `clusterResolve` | capacity object id |
| `x-ms-xmlaworkspaceobjectid` | not sent (AAD-mode only) | sent |
| `x-ms-xmladatabase` | not sent (internal mode only) | sent (dataset guid) |
| `<Catalog>` | friendly model name | dataset guid |
| `X-Transport-Caps-Negotiation-Flags` | not sent to PaaS | sent |
| `x-ms-xmlacaps-negotiation-flags` | `0,0,0,1,1` | `0,0,0,0,0` (fine) |
| extra ids | `x-ms-request-registration-id`, `x-ms-accepts-continuations: 1`, `x-ms-round-trip-id: 0`, `x-ms-parent-activity-id` | none |

The fact that the gateway answers `404 Server '…' is not found` for anything but the capacity guid
but `200 {"code":null,"message":"An internal error occurred."}` for the right guid says the front
door does accept the capacity id as the server name (so `coreServerName` is plausibly the capacity
id, or the front door resolves either); the failure is after routing, i.e. in authorisation/scope
resolution — exactly where the MWC token differs from a bearer token.

Recommended experiment order: (1) keep everything else, add the `generateastoken` call and send
`Authorization: MwcToken …` with `<Catalog>RetailDemo</Catalog>` and *without*
`x-ms-xmlaworkspaceobjectid`/`x-ms-xmladatabase`; (2) if still failing, add the `clusterResolve`
step and use its `clusterFQDN`/`coreServerName`; (3) as a fallback test the AAD mode faithfully:
bearer + `x-ms-xmlaworkspaceobjectid` only (no `x-ms-xmladatabase`), friendly-name Catalog.

## 5. Headers the client reads but Cobalt should know about

- `x-ms-routing-hint`, `x-ms-workload-resource-moniker`: echo back on subsequent requests (reset
  when the Catalog property changes: `IXmlaClientProviderEx.SetXmlaProperty`).
- `x-ms-root-activity-id`: echo back once seen; also used in error text.
- `x-ms-wrong-node: 1` on a 400: treat the body as a normal response and retry nothing.
- `x-ms-xmlaerror-extended`: human-readable error for non-2xx.
- `x-ms-xmlacaps-negotiation-flags`: mandatory in every response; drives content-type upgrade.

## 6. Could not determine from the code

- The literal `clusterFQDN`/`coreServerName` the service returns for capacity
  `ba8d4c98-…` (needs a live `clusterResolve` call; it is unauthenticated so it is cheap to try).
- The MWC token format (opaque to the client; only `Token` is read).
- Whether the dedicated cluster accepts a bearer token at all when the workspace header is present
  (the AAD mode exists behind an env var, so the server presumably supports it, but with what
  additional requirements is not visible client-side).
- `BinaryXmlSupport`/xpress availability on Linux (`msasxpress.dll` is Windows-only in this package;
  on Linux the flags would be `0,0,0,1,0` or `0,0,0,0,0`).
