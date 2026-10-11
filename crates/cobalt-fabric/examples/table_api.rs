//! Dev helper (not shipped): list a lakehouse through Fabric's table API with a storage token.
//! Usage: cargo run -p cobalt-fabric --example table_api -- <token-file> <workspace-id> <lakehouse-id> [table]
use cobalt_fabric::TableApiClient;

#[tokio::main]
async fn main() {
    let mut args = std::env::args().skip(1);
    let token = std::fs::read_to_string(args.next().expect("token file")).expect("token").trim().to_string();
    let ws = args.next().expect("workspace id");
    let lh = args.next().expect("lakehouse id");
    let table = args.next();
    let api = TableApiClient::new(token);
    let t0 = std::time::Instant::now();
    match api.list_schemas(&ws, &lh).await {
        Ok(s) => println!("schemas ({:?}): {:?}", t0.elapsed(), s),
        Err(e) => println!("schemas: ERROR {e}"),
    }
    let t0 = std::time::Instant::now();
    match api.list_tables(&ws, &lh).await {
        Ok(t) => {
            println!("list_tables ({:?}): {} tables", t0.elapsed(), t.len());
            for x in t.iter().take(12) {
                println!("  {:?} / {}", x.schema, x.name);
            }
            if let Some(first) = t.first() {
                let name = table.clone().unwrap_or_else(|| first.name.clone());
                let schema = first.schema.clone().unwrap_or_else(|| "dbo".into());
                let t0 = std::time::Instant::now();
                match api.get_table(&ws, &lh, &schema, &name).await {
                    Ok(Some(d)) => println!("get_table {schema}.{name} ({:?}): rel={} schema={:?} columns={:?}", t0.elapsed(), d.rel_path, d.schema, d.columns.iter().map(|c| format!("{}:{}", c.name, c.type_name)).collect::<Vec<_>>()),
                    Ok(None) => println!("get_table {schema}.{name}: not found"),
                    Err(e) => println!("get_table: ERROR {e}"),
                }
            }
        }
        Err(e) => println!("list_tables: ERROR {e}"),
    }
}
