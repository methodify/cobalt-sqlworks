# D004 — Entra ID interactive auth: own PKCE loopback; project-owned app registration

**Decided:** 2026-09-16 · **By:** founder ("yes")

## Decision
- Cobalt implements OAuth 2.0 authorization-code + PKCE with a `http://localhost:<port>`
  loopback redirect (system browser), plus **device-code** flow as fallback, against
  `login.microsoftonline.com/{tenant}/oauth2/v2.0`. Scope `https://database.windows.net/.default`.
- The founder registers a **multi-tenant public-client app** for the project; its client ID
  ships as the default. Users may override with their own client ID in settings.
- `AzureCliCredential` (`az login`) via `azure_identity` 1.0 is a third path.
- Refresh tokens are stored in the OS keychain (`keyring`).

## Why
`azure_identity` 1.0 GA ships no interactive-browser or device-code credential and no MSAL/WAM
broker exists in Rust. Fabric requires Entra auth, so this is table stakes for the audience and a
differentiator (Tabularis and others still lack it).

## Consequences
- Conditional-access policies requiring a compliant device/broker may reject a plain public
  client; device-code and `az login` cover those tenants.
- Live testing needs a human in a browser; the build agent unit-tests the flow and the founder
  validates at alpha.
