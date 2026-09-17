# Cobalt patch on top of tiberius-ng 0.13.1

Pristine source: crates.io `tiberius-ng` 0.13.1 (MIT OR Apache-2.0). Only `src/` is vendored.
Every change is additive; existing public APIs behave the same except where noted.

1. `src/lib.rs` — re-export `ReceivedToken`, `TokenColMetaData`, `TokenDone`, `DoneStatus`,
   `TokenInfo`, `TokenEnvChange` so an application can consume the raw token stream.
2. `src/client.rs` — new `Client::simple_query_send`, `Client::next_token`, and
   `Client::is_response_complete`; `Client::cancel_query` made resilient (packet-level resync
   when the attention acknowledgement cannot be parsed at a token boundary).
3. `src/client/connection.rs` — new `Connection::resync_after_attention` (packet-level drain).
4. `src/tds/codec/token/token_info.rs` — public getters (`number`, `state`, `class`, `message`,
   `server`, `procedure`, `line`).
5. `src/tds/codec/token/token_done.rs` — `rows`, `is_final`, `is_attention` made public; new
   `has_count`, `is_more`, `has_error`, `status`.
6. `src/tds/codec/column_data/money.rs` + `src/from_sql.rs` — `money`/`smallmoney` decode to
   `ColumnData::Numeric` (scale 4) instead of a lossy `f64` (money's full range does not fit an
   f64 mantissa); `f64::from_sql` additionally accepts `Numeric`, so existing callers keep working.

Reason: 0.13.1's `QueryStream` discards INFO/ERROR/DONE tokens, so PRINT/RAISERROR messages,
rows-affected counts and error ordering are unreachable from the public API, and the token stream
type is crate-private. Upstream issue to be filed; drop this vendor copy once upstream exposes
an equivalent API.

## login_server_name (routing redirects)

`Config::login_server_name(name)` overrides the server name written into the LOGIN7 record
without changing the TCP/TLS host. Azure SQL / Fabric routing redirects hand out
`gateway\instance` targets; the gateway routes on the login record's server name while the
TCP connection must go to the bare gateway host (SqlClient sends `gateway\instance,port`).
Additive: a new `Option<String>` field, defaulting to `host` when unset.

## PRELOGIN TRACEID (36 bytes) and `Config::activity_id`

Upstream encodes the TRACEID option as 20 bytes (activity GUID + sequence) and never sends
it from the client path. MS-TDS 2.2.6.5 defines it as GUID_CONNID (16) + ACTIVITYID GUID (16) +
sequence (4) = 36 bytes, which is what SqlClient sends. Microsoft Fabric's routed warehouse
gateway rejects logins whose PRELOGIN lacks a TRACEID with 18456 "Couldn't complete the
operation due to a system update", so Cobalt sends one on every connection.
`ActivityId::with_connection_id` and `Config::activity_id` are additive; decoding accepts
both the 36-byte and legacy 20-byte forms. Candidate for an upstream PR.
