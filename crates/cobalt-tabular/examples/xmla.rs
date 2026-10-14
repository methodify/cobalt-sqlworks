//! Dev helper (not shipped): talk XMLA to a model.
//! Usage: cargo run -p cobalt-tabular --example xmla -- <kind: http|asazure|powerbi> <url-or-region> <server-or-workspace> <model>
//!            [--token-file F | --basic user:pass] (--catalogs | --catalog | --functions | "<query>")
//!   http:    <url> is the XMLA URL (msmdpump.dll); <server-or-workspace> is ignored (pass -)
//!   asazure: <region> (e.g. westeurope) and <server>; the token is for https://*.asazure.windows.net
//!   powerbi: <region> is ignored (pass -); <workspace> is the workspace id (GUID) or name; the token is for
//!            https://analysis.windows.net/powerbi/api
//! RUST_LOG=debug prints the resolution steps and every response's status.
use cobalt_core::tabular::TabularTarget;
use cobalt_tabular::{Credential, DaxItem, DaxOptions, TabularError, XmlaClient};

#[tokio::main]
async fn main() {
    tracing_subscriber::fmt().with_env_filter(tracing_subscriber::EnvFilter::from_default_env()).with_writer(std::io::stderr).init();
    let args: Vec<String> = std::env::args().skip(1).collect();
    if args.len() < 5 {
        eprintln!("usage: xmla <http|asazure|powerbi> <url-or-region> <server-or-workspace> <model> [--token-file F | --basic user:pass] (--catalogs | --catalog | --functions | \"<query>\")");
        std::process::exit(2);
    }
    let (kind, a, b, model) = (args[0].as_str(), args[1].clone(), args[2].clone(), args[3].clone());
    let target = match kind {
        "http" => TabularTarget::Http { url: a, model },
        "asazure" => TabularTarget::AzureAs { region: a, server: b, model },
        "powerbi" => {
            let is_guid = b.len() == 36 && b.bytes().filter(|c| *c == b'-').count() == 4;
            TabularTarget::PowerBi { workspace_id: if is_guid { b.clone() } else { String::new() }, workspace_name: if is_guid { String::new() } else { b.clone() }, dataset_id: String::new(), model_name: model }
        }
        other => {
            eprintln!("unknown kind {other}; use http, asazure or powerbi");
            std::process::exit(2);
        }
    };
    let mut rest = &args[4..];
    let mut cred = Credential::Anonymous;
    while let Some(flag) = rest.first() {
        match flag.as_str() {
            "--token-file" => {
                cred = Credential::Bearer(std::fs::read_to_string(&rest[1]).expect("token file").trim().to_string());
                rest = &rest[2..];
            }
            "--basic" => {
                let (u, p) = rest[1].split_once(':').expect("user:pass");
                cred = Credential::Basic { user: u.to_string(), password: p.to_string() };
                rest = &rest[2..];
            }
            _ => break,
        }
    }
    let action = rest.first().map(String::as_str).unwrap_or("--catalogs");
    let t0 = std::time::Instant::now();
    let outcome = run(&target, cred, action, t0).await;
    if let Err(e) = outcome {
        println!("ERROR: {e}");
        if let Some(h) = e.hint() {
            println!("  hint: {h}");
        }
    }
    println!("elapsed {:?}", t0.elapsed());
}

async fn run(target: &TabularTarget, cred: Credential, action: &str, t0: std::time::Instant) -> Result<(), TabularError> {
    let client = XmlaClient::connect(target, cred).await?;
    println!("connected to {} (server {} version {}) in {:?}; session {}", client.url(), client.server_name().unwrap_or_default(), client.server_version().unwrap_or_default(), t0.elapsed(), client.session_id().unwrap_or_default());
    let result = match action {
        "--catalogs" => {
            for c in client.list_catalogs().await? {
                println!("  {c}");
            }
            Ok(())
        }
        "--catalog" => catalog(&client, target).await,
        "--functions" => {
            let f = client.load_functions().await?;
            println!("{} functions", f.len());
            for x in f.iter().take(20) {
                println!("  {} {} [{}]", x.name, x.parameters.as_deref().unwrap_or(""), x.category.as_deref().unwrap_or(""));
            }
            Ok(())
        }
        query => run_query(&client, query, t0).await,
    };
    client.end_session().await;
    result
}

async fn run_query(client: &XmlaClient, text: &str, t0: std::time::Instant) -> Result<(), TabularError> {
    println!("{} query", if cobalt_tabular::is_mdx_or_dmv(text) { "MDX/DMV" } else { "DAX" });
    let mut stream = client.execute(text, &DaxOptions::default()).await?;
    println!("response after {:?}", t0.elapsed());
    let mut shown = 0usize;
    let mut set = 0usize;
    while let Some(item) = stream.next().await? {
        match item {
            DaxItem::ResultSetStart { columns } => {
                set += 1;
                shown = 0;
                println!("-- result set {set}");
                for c in &columns {
                    println!("   {} {}", c.name, c.sql_type);
                }
            }
            DaxItem::Batch(b) => {
                if shown < 5 {
                    let take = (5 - shown).min(b.num_rows());
                    let head = b.slice(0, take);
                    print!("{}", arrow::util::pretty::pretty_format_batches(&[head]).map(|t| t.to_string()).unwrap_or_default());
                    shown += take;
                }
            }
            DaxItem::ResultSetEnd { rows } => println!("   {rows} rows at {:?}", t0.elapsed()),
            DaxItem::Error { message, code } => println!("-- error {}: {message}", code.unwrap_or_default()),
        }
    }
    Ok(())
}

async fn catalog(client: &XmlaClient, target: &TabularTarget) -> Result<(), TabularError> {
    let c = client.load_catalog(target.model()).await?;
    println!("model {}: {} tables, {} columns, {} measures, {} relationships{}", c.name, c.tables.len(), c.column_count(), c.measure_count(), c.relationships.len(), if c.expressions_hidden { " (expressions hidden)" } else { "" });
    for t in &c.tables {
        println!("{}{}", t.name, if t.hidden { " (hidden)" } else { "" });
        for col in &t.columns {
            println!("  {} : {}{}{}", col.name, col.data_type.label(), if col.hidden { " (hidden)" } else { "" }, if col.calculated { " (calculated)" } else { "" });
        }
        for m in &t.measures {
            let expr = m.expression.as_deref().map(|e| format!(" = {}", e.lines().next().unwrap_or(""))).unwrap_or_default();
            println!("  [{}] : {}{}{expr}", m.name, m.data_type.label(), if m.hidden { " (hidden)" } else { "" });
        }
        for h in &t.hierarchies {
            println!("  hierarchy {}: {}", h.name, h.levels.iter().map(|l| l.name.as_str()).collect::<Vec<_>>().join(" > "));
        }
    }
    for r in &c.relationships {
        println!("{}[{}] {} -> {}[{}] {}{}", r.from_table, r.from_column, r.from_cardinality, r.to_table, r.to_column, r.to_cardinality, if r.active { "" } else { " (inactive)" });
    }
    Ok(())
}
