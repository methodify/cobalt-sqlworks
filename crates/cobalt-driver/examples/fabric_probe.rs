//! Dev helper: probe routing behaviour against an Azure SQL / Fabric endpoint with a bearer
//! token. Usage: TOKFILE=token.txt cargo run -p cobalt-driver --example fabric_probe -- <host> [database]
use tiberius::{AuthMethod, Config, EncryptionLevel, SqlBrowser};
use tokio::net::TcpStream;
use tokio_util::compat::TokioAsyncWriteCompatExt;

#[tokio::main]
async fn main() {
    tracing_subscriber::fmt().with_env_filter(tracing_subscriber::EnvFilter::from_default_env()).with_writer(std::io::stderr).init();
    let mut args = std::env::args().skip(1);
    let host = args.next().expect("host");
    let database = args.next();
    let token = std::fs::read_to_string(std::env::var("TOKFILE").expect("TOKFILE")).unwrap().trim().to_string();
    let modes: Vec<&str> = if std::env::var("ONE").is_ok() { vec!["full"] } else { vec!["full", "port", "orig", "gateway", "instance_field"] };
    for mode in modes {
        let mut config = Config::new();
        let (h, p) = host.split_once(':').map(|(h, p)| (h.to_string(), p.parse::<u16>().unwrap())).unwrap_or((host.clone(), 1433));
        config.host(&h);
        config.port(p);
        config.encryption(EncryptionLevel::Required);
        if std::env::var("TRUST").is_ok() { config.trust_cert(); }
        if std::env::var("PSIZE").is_ok() { config.packet_size(8000); }
        config.authentication(AuthMethod::aad_token(token.clone()));
        config.application_name("cobalt-probe");
        if std::env::var("NOTRACE").is_err() { config.activity_id(tiberius::ActivityId::with_connection_id(uuid::Uuid::new_v4(), uuid::Uuid::new_v4(), 1)); }
        if let Some(db) = &database {
            config.database(db);
        }
        let r = open(config, mode, &host).await;
        match r {
            Ok(mut c) => {
                let row = c.simple_query("SELECT DB_NAME(), @@VERSION").await.unwrap().into_row().await.unwrap().unwrap();
                println!("[{mode}] OK db={:?} ver={:?}", row.get::<&str, _>(0), row.get::<&str, _>(1).map(|s| &s[..40]));
                if let Ok(sqls) = std::env::var("SQL") {
                    for sql in sqls.split("||") {
                        match c.simple_query(sql).await {
                            Ok(s) => match s.into_results().await { Ok(r) => println!("  SQL OK  {sql} -> {} result sets", r.len()), Err(e) => println!("  SQL ERR {sql} -> {e}") },
                            Err(e) => println!("  SQL ERR {sql} -> {e}"),
                        }
                    }
                }
            }
            Err(e) => println!("[{mode}] ERR {e}"),
        }
    }
}

async fn open(mut config: Config, mode: &str, orig: &str) -> Result<tiberius::Client<tokio_util::compat::Compat<TcpStream>>, String> {
    let mut routed: Option<(String, u16)> = None;
    for hop in 0..2 {
        let tcp = match &routed {
            Some((g, p)) => TcpStream::connect((g.as_str(), *p)).await.map_err(|e| e.to_string())?,
            None => TcpStream::connect_named(&config).await.map_err(|e| e.to_string())?,
        };
        match tiberius::Client::connect(config.clone(), tcp.compat_write()).await {
            Ok(c) => return Ok(c),
            Err(tiberius::error::Error::Routing { host, port }) if hop == 0 => {
                let gateway = host.split('\\').next().unwrap_or(&host).to_string();
                let instance = host.split('\\').nth(1).map(str::to_string);
                config.hostname_in_certificate(gateway.clone());
                config.host(gateway.clone());
                config.port(port);
                match mode {
                    "full" => config.login_server_name(host.clone()),
                    "port" => config.login_server_name(format!("{host},{port}")),
                    "orig" => config.login_server_name(orig.to_string()),
                    "gateway" => {}
                    "instance_field" => {
                        if let Some(i) = instance { config.instance_name(i); }
                        config.login_server_name(host.clone());
                    }
                    _ => unreachable!(),
                }
                routed = Some((gateway, port));
            }
            Err(tiberius::error::Error::Server(t)) => {
                return Err(format!("server error code={} state={} class={} msg={} server={:?} proc={:?} line={}", t.code(), t.state(), t.class(), t.message(), t.server(), t.procedure(), t.line()));
            }
            Err(e) => return Err(format!("{e:?}")),
        }
    }
    Err("too many redirects".into())
}
