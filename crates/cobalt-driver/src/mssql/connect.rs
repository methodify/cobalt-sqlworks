//! TCP (+ SQL Browser) → TLS → login → engine probe.

use super::{cell_i64, cell_string, config, map_error, ErrorPhase, MssqlConnection, TdsClient};
use crate::{DriverError, Result};
use cobalt_core::{ConnectionProfile, ConnectionRole, EngineInfo, EngineKind, ResolvedCredentials};
use std::time::Duration;
use tiberius::{Config, SqlBrowser};
use tokio::net::TcpStream;
use tokio_util::compat::TokioAsyncWriteCompatExt;

/// TCP keepalive, as SqlClient sets it (30 s idle, then a probe every second). Keeps NAT and
/// gateway mappings alive across long idle stretches, and makes a vanished peer surface as an
/// I/O error within seconds instead of after the retransmission budget.
const KEEPALIVE_IDLE: Duration = Duration::from_secs(30);
const KEEPALIVE_INTERVAL: Duration = Duration::from_secs(1);

fn tune_socket(s: &TcpStream) {
    let _ = s.set_nodelay(true);
    let ka = socket2::TcpKeepalive::new().with_time(KEEPALIVE_IDLE).with_interval(KEEPALIVE_INTERVAL);
    if let Err(e) = socket2::SockRef::from(s).set_tcp_keepalive(&ka) {
        tracing::debug!("could not set TCP keepalive: {e}");
    }
}

const PROBE_SQL: &str = "SELECT CAST(@@SPID AS int) AS spid, \
    CAST(SERVERPROPERTY('ProductVersion') AS nvarchar(128)) AS product_version, \
    CAST(SERVERPROPERTY('ProductLevel') AS nvarchar(128)) AS product_level, \
    CAST(SERVERPROPERTY('Edition') AS nvarchar(256)) AS edition, \
    CAST(SERVERPROPERTY('EngineEdition') AS int) AS engine_edition, \
    CAST(SERVERPROPERTY('ServerName') AS nvarchar(256)) AS server_name, \
    @@VERSION AS version_text, \
    DB_NAME() AS db_name";

pub(crate) async fn connect(profile: &ConnectionProfile, creds: &ResolvedCredentials, role: ConnectionRole) -> Result<MssqlConnection> {
    if !config::credentials_match(profile, creds) {
        return Err(DriverError::Other(format!(
            "resolved credentials do not match the profile's authentication method ({})",
            profile.auth.label()
        )));
    }
    let (config, spec) = config::build_config(profile, creds)?;
    let secs = profile.options.connect_timeout_secs.max(1);
    tracing::debug!(host = %spec.host, port = ?spec.port, instance = ?spec.instance, "connecting");

    let client = tokio::time::timeout(Duration::from_secs(secs as u64), open(config))
        .await
        .map_err(|_| DriverError::Timeout(secs))??;

    let mut conn = MssqlConnection::new(client, placeholder_engine(), None, profile.database.clone().unwrap_or_default(), profile, role);
    probe(&mut conn, profile).await?;
    tracing::info!(spid = ?conn.spid, engine = ?conn.engine.kind, version = %conn.engine.version, db = %conn.database, "connected");
    Ok(conn)
}

/// TCP connect (SQL Browser for named instances, multi-subnet failover honoured) + TDS login,
/// following at most one Azure routing redirect.
async fn open(mut config: Config) -> Result<TdsClient> {
    // After a routing redirect (Azure SQL, Synapse, Fabric) the target may look like
    // `gateway.host\warehouse-id-dw` — connect TCP/TLS to the gateway host and keep the full
    // name for the LOGIN7 server-name field, as SqlClient does. Never treat it as a named instance.
    let mut routed: Option<(String, u16)> = None;
    for hop in 0..2 {
        let tcp = match &routed {
            Some((gateway, port)) => {
                let s = TcpStream::connect((gateway.as_str(), *port)).await.map_err(|e| DriverError::Connect(format!("{gateway}:{port}: {e}")))?;
                tune_socket(&s);
                s
            }
            None => {
                let s = TcpStream::connect_named(&config).await.map_err(|e| map_error(e, ErrorPhase::Connect))?;
                tune_socket(&s);
                s
            }
        };
        match tiberius::Client::connect(config.clone(), tcp.compat_write()).await {
            Ok(client) => return Ok(client),
            Err(tiberius::error::Error::Routing { host, port }) if hop == 0 => {
                tracing::info!("server redirected the connection to {host}:{port}");
                let gateway = host.split('\\').next().unwrap_or(&host).to_string();
                config.hostname_in_certificate(gateway.clone());
                config.host(gateway.clone());
                config.port(port);
                // SqlClient sends the routed name verbatim plus the port in LOGIN7; Fabric's
                // gateway picks the target warehouse out of it.
                config.login_server_name(format!("{host},{port}"));
                routed = Some((gateway, port));
            }
            Err(e) => return Err(map_error(e, ErrorPhase::Connect)),
        }
    }
    Err(DriverError::Connect("too many routing redirects".into()))
}

async fn probe(conn: &mut MssqlConnection, profile: &ConnectionProfile) -> Result<()> {
    let rows = conn.query_rows(PROBE_SQL).await.map_err(|e| match e {
        DriverError::Server(m) => DriverError::Connect(format!("server probe failed: {}", m.message)),
        other => other,
    })?;
    let row = rows.first().ok_or_else(|| DriverError::Connect("server probe returned no row".into()))?;

    let s = |i: usize| cell_string(row, i).unwrap_or_default();
    let engine_edition = cell_i64(row, 4).unwrap_or(0) as i32;
    let edition = s(3);
    let kind = EngineKind::from_edition(engine_edition, profile.looks_like_fabric(), &edition);
    conn.spid = cell_i64(row, 0).map(|v| v as i32);
    conn.engine = EngineInfo {
        kind,
        version: s(1),
        product_level: s(2),
        edition,
        server_name: s(5),
        version_text: s(6),
        capabilities: kind.capabilities(),
    };
    let db = s(7);
    if !db.is_empty() {
        conn.database = db;
    }
    Ok(())
}

fn placeholder_engine() -> EngineInfo {
    EngineInfo {
        kind: EngineKind::Unknown,
        version: String::new(),
        product_level: String::new(),
        edition: String::new(),
        server_name: String::new(),
        version_text: String::new(),
        capabilities: EngineKind::Unknown.capabilities(),
    }
}
