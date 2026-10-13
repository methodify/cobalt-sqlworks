//! Dev helper (not shipped): run DAX against a semantic model through the Arrow query API.
//! Usage: cargo run -p cobalt-tabular --example dax -- <token-file> <workspace-id> <dataset-id> "<DAX>"
//!        cargo run -p cobalt-tabular --example dax -- <token-file> <workspace-id> <dataset-id> --catalog
//!        (also --functions and --probe)
//! The token file holds a bearer token for https://analysis.windows.net/powerbi/api.
use cobalt_core::tabular::TabularTarget;
use cobalt_tabular::{DaxItem, DaxOptions, PowerBiClient, TabularError};

#[tokio::main]
async fn main() {
    let args: Vec<String> = std::env::args().skip(1).collect();
    if args.len() < 4 {
        eprintln!("usage: dax <token-file> <workspace-id> <dataset-id> (\"<DAX>\" | --catalog | --functions | --probe)");
        std::process::exit(2);
    }
    let token = std::fs::read_to_string(&args[0]).expect("token file").trim().to_string();
    let target = TabularTarget::PowerBi { workspace_id: args[1].clone(), workspace_name: args[1].clone(), dataset_id: args[2].clone(), model_name: args[2].clone() };
    let client = PowerBiClient::new(token);
    let t0 = std::time::Instant::now();
    let outcome = match args[3].as_str() {
        "--catalog" => catalog(&client, &target).await,
        "--functions" => functions(&client, &target).await,
        "--probe" => cobalt_tabular::probe(&client, &target).await.map(|_| println!("probe ok")),
        dax => query(&client, &target, dax, t0).await,
    };
    if let Err(e) = outcome {
        println!("ERROR: {e}");
        if let Some(h) = e.hint() {
            println!("  hint: {h}");
        }
    }
    println!("elapsed {:?}", t0.elapsed());
}

async fn query(client: &PowerBiClient, target: &TabularTarget, dax: &str, t0: std::time::Instant) -> Result<(), TabularError> {
    let mut stream = client.execute(target, dax, &DaxOptions::default()).await?;
    println!("first byte after {:?}", t0.elapsed());
    let mut shown = 0usize;
    let mut set = 0usize;
    while let Some(item) = stream.next().await? {
        match item {
            DaxItem::ResultSetStart { columns } => {
                set += 1;
                shown = 0;
                println!("-- result set {set} at {:?}", t0.elapsed());
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

async fn catalog(client: &PowerBiClient, target: &TabularTarget) -> Result<(), TabularError> {
    let c = cobalt_tabular::load_catalog(client, target).await?;
    println!("model {}: {} tables, {} columns, {} measures, {} relationships{}", c.name, c.tables.len(), c.column_count(), c.measure_count(), c.relationships.len(), if c.expressions_hidden { " (expressions hidden)" } else { "" });
    for t in &c.tables {
        println!("{}{} [{}]", t.name, if t.hidden { " (hidden)" } else { "" }, t.storage_mode.as_deref().unwrap_or("?"));
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

async fn functions(client: &PowerBiClient, target: &TabularTarget) -> Result<(), TabularError> {
    let f = cobalt_tabular::load_functions(client, target).await?;
    println!("{} functions", f.len());
    for x in f.iter().take(20) {
        println!("  {} {} [{}]", x.name, x.parameters.as_deref().unwrap_or(""), x.category.as_deref().unwrap_or(""));
    }
    Ok(())
}
