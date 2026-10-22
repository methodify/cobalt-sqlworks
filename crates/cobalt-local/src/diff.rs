//! Diff: two local tables, a key, and the rows that were added, removed, changed or stayed
//! the same, as one query. `a` is the earlier side ("was"), `b` the later ("now"): a row only
//! in `b` is added, only in `a` removed; a row in both whose compared columns differ (nulls
//! equal, `IS DISTINCT FROM`) is changed. The result carries `__change`, the keys (coalesced),
//! and for every compared column its `b` value plus `<col>__was` from `a`.

use cobalt_core::ColumnInfo;

pub const CHANGE_COLUMN: &str = "__change";
pub const WAS_SUFFIX: &str = "__was";

#[derive(Clone, Debug, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct DiffSpec {
    pub a: String,
    pub b: String,
    pub keys: Vec<String>,
    /// Columns compared for "changed"; empty = every shared non-key column.
    pub compare: Vec<String>,
}

fn q(name: &str) -> String {
    format!("\"{}\"", name.replace('"', "\"\""))
}

/// Shared columns of two tables, in `a`'s order.
pub fn shared_columns(a: &[ColumnInfo], b: &[ColumnInfo]) -> Vec<String> {
    a.iter().filter(|c| b.iter().any(|d| d.name.eq_ignore_ascii_case(&c.name))).map(|c| c.name.clone()).collect()
}

/// A key guess: shared columns named `id`, ending in `_id` / `id` / `key` / `code`, or in the
/// primary key; else the first shared column.
pub fn guess_keys(a: &[ColumnInfo], b: &[ColumnInfo]) -> Vec<String> {
    let shared = shared_columns(a, b);
    let pk: Vec<String> = a.iter().filter(|c| c.in_primary_key && shared.iter().any(|s| s == &c.name)).map(|c| c.name.clone()).collect();
    if !pk.is_empty() {
        return pk;
    }
    let score = |n: &str| -> u8 {
        let l = n.to_ascii_lowercase();
        if l == "id" { 4 } else if l.ends_with("_id") || l.ends_with("id") { 3 } else if l.ends_with("key") || l.ends_with("code") { 2 } else { 0 }
    };
    let best = shared.iter().max_by_key(|n| score(n)).filter(|n| score(n) > 0).cloned();
    match best {
        Some(k) => vec![k],
        None => shared.first().cloned().into_iter().collect(),
    }
}

/// The diff query and its counts query.
pub fn to_sql(spec: &DiffSpec, a_cols: &[ColumnInfo], b_cols: &[ColumnInfo]) -> Result<(String, String), String> {
    if spec.keys.is_empty() {
        return Err("pick at least one key column".into());
    }
    let shared = shared_columns(a_cols, b_cols);
    for k in &spec.keys {
        if !shared.iter().any(|s| s.eq_ignore_ascii_case(k)) {
            return Err(format!("key column {k} is not in both tables"));
        }
    }
    let compare: Vec<String> = if spec.compare.is_empty() {
        shared.iter().filter(|c| !spec.keys.iter().any(|k| k.eq_ignore_ascii_case(c))).cloned().collect()
    } else {
        for c in &spec.compare {
            if !shared.iter().any(|s| s.eq_ignore_ascii_case(c)) {
                return Err(format!("compared column {c} is not in both tables"));
            }
        }
        spec.compare.clone()
    };
    let k0 = q(&spec.keys[0]);
    let changed = if compare.is_empty() { "FALSE".to_string() } else { compare.iter().map(|c| format!("(a.{c} IS DISTINCT FROM b.{c})", c = q(c))).collect::<Vec<_>>().join(" OR ") };
    let mut select = vec![format!("CASE WHEN b.{k0} IS NULL THEN 'removed' WHEN a.{k0} IS NULL THEN 'added' WHEN {changed} THEN 'changed' ELSE 'same' END AS {}", q(CHANGE_COLUMN))];
    for k in &spec.keys {
        select.push(format!("COALESCE(a.{k}, b.{k}) AS {k}", k = q(k)));
    }
    for c in &compare {
        select.push(format!("b.{c} AS {c}", c = q(c)));
        select.push(format!("a.{c} AS {w}", c = q(c), w = q(&format!("{c}{WAS_SUFFIX}"))));
    }
    let on = spec.keys.iter().map(|k| format!("a.{k} = b.{k}", k = q(k))).collect::<Vec<_>>().join(" AND ");
    let order = format!("CASE {ch} WHEN 'added' THEN 0 WHEN 'removed' THEN 1 WHEN 'changed' THEN 2 ELSE 3 END, {keys}", ch = q(CHANGE_COLUMN), keys = spec.keys.iter().map(|k| q(k)).collect::<Vec<_>>().join(", "));
    let text = format!("SELECT {}\nFROM {} a\nFULL OUTER JOIN {} b ON {on}\nORDER BY {order}", select.join(",\n       "), q(&spec.a), q(&spec.b));
    let counts = format!("SELECT {ch}, count(*) AS n FROM (\n{text}\n) d GROUP BY {ch}", ch = q(CHANGE_COLUMN));
    Ok((text, counts))
}

#[cfg(test)]
mod tests {
    use super::*;
    use cobalt_core::SqlType;

    fn cols(names: &[&str]) -> Vec<ColumnInfo> {
        names.iter().enumerate().map(|(i, n)| ColumnInfo::new(*n, SqlType::Int, true, i)).collect()
    }

    #[test]
    fn guesses_keys() {
        assert_eq!(guess_keys(&cols(&["name", "order_id", "amount"]), &cols(&["order_id", "amount"])), vec!["order_id"]);
        assert_eq!(guess_keys(&cols(&["x", "y"]), &cols(&["y", "x"])), vec!["x"]);
        assert!(guess_keys(&cols(&["x"]), &cols(&["y"])).is_empty());
    }

    #[test]
    fn emits_the_join() {
        let spec = DiffSpec { a: "prod".into(), b: "dev".into(), keys: vec!["id".into()], compare: vec![] };
        let (text, counts) = to_sql(&spec, &cols(&["id", "amount", "note"]), &cols(&["id", "amount", "other"])).unwrap();
        assert!(text.starts_with("SELECT CASE WHEN b.\"id\" IS NULL THEN 'removed' WHEN a.\"id\" IS NULL THEN 'added' WHEN (a.\"amount\" IS DISTINCT FROM b.\"amount\") THEN 'changed' ELSE 'same' END AS \"__change\""));
        assert!(text.contains("COALESCE(a.\"id\", b.\"id\") AS \"id\""));
        assert!(text.contains("b.\"amount\" AS \"amount\",\n       a.\"amount\" AS \"amount__was\""));
        assert!(!text.contains("note"));
        assert!(text.contains("FROM \"prod\" a\nFULL OUTER JOIN \"dev\" b ON a.\"id\" = b.\"id\""));
        assert!(counts.contains("GROUP BY \"__change\""));
        assert!(to_sql(&DiffSpec { a: "p".into(), b: "d".into(), keys: vec![], compare: vec![] }, &cols(&["id"]), &cols(&["id"])).is_err());
    }
}
