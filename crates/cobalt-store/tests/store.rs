use chrono::{Duration, Utc};
use cobalt_core::*;
use cobalt_store::*;

fn mem() -> Store {
    Store::open_in_memory().unwrap()
}

fn sql_profile(name: &str, server: &str) -> ConnectionProfile {
    let mut p = ConnectionProfile::new(
        server,
        AuthMethod::SqlLogin {
            user: "sa".into(),
            password: None,
        },
    );
    p.name = Some(name.into());
    p
}

fn entra_profile(name: &str, server: &str) -> ConnectionProfile {
    let mut p = ConnectionProfile::new(
        server,
        AuthMethod::EntraInteractive {
            tenant: Some("t1".into()),
            account_hint: Some("me@x.com".into()),
        },
    );
    p.name = Some(name.into());
    p
}

// ---- schema ----------------------------------------------------------------------------

#[test]
fn schema_creates_and_reopens() {
    let dir = tempfile::tempdir().unwrap();
    let db = dir.path().join("nested").join("cobalt.db");
    {
        let s = Store::open(&db).unwrap();
        assert_eq!(s.schema_version().unwrap(), SCHEMA_VERSION);
        s.set_kv("probe", &42u32).unwrap();
    }
    let s = Store::open(&db).unwrap();
    assert_eq!(s.schema_version().unwrap(), SCHEMA_VERSION);
    assert_eq!(s.get_kv::<u32>("probe").unwrap(), Some(42));
    assert_eq!(MIGRATIONS.len() as i64, SCHEMA_VERSION);
}

#[test]
fn fts5_is_available() {
    let s = mem();
    let id = s
        .add_history(&NewHistoryEntry::new("srv", "select 1"))
        .unwrap();
    let hits = s.search_history(&HistoryQuery::text("select")).unwrap();
    assert_eq!(hits.len(), 1);
    assert_eq!(hits[0].id, id);
}

#[test]
fn store_is_send_and_sync() {
    fn assert_send_sync<T: Send + Sync>() {}
    assert_send_sync::<Store>();
    let s = std::sync::Arc::new(mem());
    let s2 = s.clone();
    std::thread::spawn(move || s2.set_kv("from-thread", &true).unwrap())
        .join()
        .unwrap();
    assert_eq!(s.get_kv::<bool>("from-thread").unwrap(), Some(true));
}

// ---- groups ----------------------------------------------------------------------------

#[test]
fn group_crud_and_ordering() {
    let s = mem();
    let mut a = ServerGroup::new("Prod", Color::PALETTE[3]);
    a.sort_order = 2;
    let mut b = ServerGroup::new("Dev", Color::PALETTE[1]);
    b.sort_order = 1;
    b.description = Some("local boxes".into());
    s.upsert_group(&a).unwrap();
    s.upsert_group(&b).unwrap();

    let list = s.list_groups().unwrap();
    assert_eq!(
        list.iter().map(|g| g.name.as_str()).collect::<Vec<_>>(),
        ["Dev", "Prod"]
    );
    assert_eq!(list[0], b);

    a.name = "Production".into();
    a.color = Color(1, 2, 3);
    s.upsert_group(&a).unwrap();
    assert_eq!(s.get_group(a.id).unwrap().unwrap(), a);

    s.reorder_groups(&[a.id, b.id]).unwrap();
    let list = s.list_groups().unwrap();
    assert_eq!(
        list.iter().map(|g| g.name.as_str()).collect::<Vec<_>>(),
        ["Production", "Dev"]
    );

    assert!(s.delete_group(b.id, None).unwrap());
    assert!(!s.delete_group(b.id, None).unwrap());
    assert!(s.get_group(b.id).unwrap().is_none());
}

#[test]
fn group_nesting_and_move_cycle_guard() {
    let s = mem();
    let root = ServerGroup::new("Root", Color::PALETTE[0]);
    let mut child = ServerGroup::new("Child", Color::PALETTE[1]);
    child.parent = Some(root.id);
    let mut grandchild = ServerGroup::new("Grandchild", Color::PALETTE[2]);
    grandchild.parent = Some(child.id);
    s.upsert_group(&root).unwrap();
    s.upsert_group(&child).unwrap();
    s.upsert_group(&grandchild).unwrap();

    assert!(matches!(
        s.move_group(root.id, Some(grandchild.id)),
        Err(StoreError::Invalid(_))
    ));
    assert!(matches!(
        s.move_group(root.id, Some(root.id)),
        Err(StoreError::Invalid(_))
    ));
    s.move_group(grandchild.id, None).unwrap();
    assert_eq!(s.get_group(grandchild.id).unwrap().unwrap().parent, None);
    s.move_group(grandchild.id, Some(root.id)).unwrap();
    assert_eq!(
        s.get_group(grandchild.id).unwrap().unwrap().parent,
        Some(root.id)
    );
    assert!(matches!(
        s.move_group(GroupId::new(), None),
        Err(StoreError::NotFound(_))
    ));
}

#[test]
fn delete_group_reassigns_profiles_and_children() {
    let s = mem();
    let g1 = ServerGroup::new("G1", Color::PALETTE[0]);
    let g2 = ServerGroup::new("G2", Color::PALETTE[1]);
    let mut sub = ServerGroup::new("Sub", Color::PALETTE[2]);
    sub.parent = Some(g1.id);
    for g in [&g1, &g2, &sub] {
        s.upsert_group(g).unwrap();
    }
    let mut p1 = sql_profile("p1", "a");
    p1.group = Some(g1.id);
    let mut p2 = sql_profile("p2", "b");
    p2.group = Some(g1.id);
    s.upsert_profile(&p1).unwrap();
    s.upsert_profile(&p2).unwrap();

    s.delete_group(g1.id, Some(g2.id)).unwrap();
    assert_eq!(s.get_profile(p1.id).unwrap().unwrap().group, Some(g2.id));
    assert_eq!(s.get_profile(p2.id).unwrap().unwrap().group, Some(g2.id));
    assert_eq!(s.get_group(sub.id).unwrap().unwrap().parent, Some(g2.id));

    s.delete_group(g2.id, None).unwrap();
    assert_eq!(s.get_profile(p1.id).unwrap().unwrap().group, None);
    assert_eq!(s.get_group(sub.id).unwrap().unwrap().parent, None);
    assert!(matches!(
        s.delete_group(sub.id, Some(sub.id)),
        Err(StoreError::Invalid(_))
    ));
}

// ---- profiles --------------------------------------------------------------------------

#[test]
fn profile_roundtrip_all_fields() {
    let s = mem();
    let g = ServerGroup::new("G", Color::PALETTE[5]);
    s.upsert_group(&g).unwrap();
    let mut p = ConnectionProfile::new(
        "myhost\\inst",
        AuthMethod::SqlLogin {
            user: "app".into(),
            password: Some(SecretRef {
                key: "k:password".into(),
            }),
        },
    );
    p.name = Some("Warehouse".into());
    p.port = Some(1444);
    p.database = Some("dw".into());
    p.group = Some(g.id);
    p.color = Some(Color(9, 8, 7));
    p.read_only_guard = true;
    p.options.encrypt = Encrypt::Strict;
    p.options.trust_server_certificate = true;
    p.options.application_intent = ApplicationIntent::ReadOnly;
    p.options.packet_size = Some(8000);
    p.options.command_timeout_secs = 120;
    s.upsert_profile(&p).unwrap();

    let got = s.get_profile(p.id).unwrap().unwrap();
    assert_eq!(got, p);
    assert!(s.get_profile(ProfileId::new()).unwrap().is_none());

    for auth in [
        AuthMethod::EntraInteractive {
            tenant: None,
            account_hint: None,
        },
        AuthMethod::EntraDeviceCode {
            tenant: Some("t".into()),
        },
        AuthMethod::AzureCli { tenant: None },
        AuthMethod::WindowsIntegrated,
        AuthMethod::EntraServicePrincipal {
            tenant: "t".into(),
            client_id: "c".into(),
            secret: None,
        },
    ] {
        let q = ConnectionProfile::new("x", auth.clone());
        s.upsert_profile(&q).unwrap();
        assert_eq!(s.get_profile(q.id).unwrap().unwrap().auth, auth);
    }
}

#[test]
fn profile_ordering_by_group_then_sort_then_name() {
    let s = mem();
    let mut g_a = ServerGroup::new("Alpha", Color::PALETTE[0]);
    g_a.sort_order = 1;
    let mut g_b = ServerGroup::new("Beta", Color::PALETTE[1]);
    g_b.sort_order = 0;
    s.upsert_group(&g_a).unwrap();
    s.upsert_group(&g_b).unwrap();

    let mut ungrouped = sql_profile("zzz", "z");
    ungrouped.group = None;
    let mut a1 = sql_profile("a1", "a");
    a1.group = Some(g_a.id);
    let mut a2 = sql_profile("a2", "a");
    a2.group = Some(g_a.id);
    let mut b1 = sql_profile("b1", "b");
    b1.group = Some(g_b.id);
    for p in [&a2, &a1, &ungrouped, &b1] {
        s.upsert_profile(p).unwrap();
    }
    // Insert order gives sort_order; a2 (0) before a1 (1) within Alpha.
    let names =
        |v: Vec<ConnectionProfile>| v.into_iter().map(|p| p.name.unwrap()).collect::<Vec<_>>();
    assert_eq!(names(s.list_profiles().unwrap()), ["zzz", "b1", "a2", "a1"]);

    s.reorder_profiles(Some(g_a.id), &[a1.id, a2.id]).unwrap();
    assert_eq!(names(s.list_profiles().unwrap()), ["zzz", "b1", "a1", "a2"]);
    assert_eq!(
        names(s.list_profiles_in_group(Some(g_a.id)).unwrap()),
        ["a1", "a2"]
    );

    // Reorder can also move a profile between groups.
    s.reorder_profiles(Some(g_b.id), &[a2.id, b1.id]).unwrap();
    assert_eq!(
        names(s.list_profiles_in_group(Some(g_b.id)).unwrap()),
        ["a2", "b1"]
    );
    assert_eq!(
        names(s.list_profiles_in_group(Some(g_a.id)).unwrap()),
        ["a1"]
    );

    // Updating a profile keeps its position; changing its group appends it there.
    let mut a1_renamed = a1.clone();
    a1_renamed.name = Some("a1-renamed".into());
    s.upsert_profile(&a1_renamed).unwrap();
    assert_eq!(
        names(s.list_profiles_in_group(Some(g_a.id)).unwrap()),
        ["a1-renamed"]
    );
    assert_eq!(s.profile_count().unwrap(), 4);
}

#[test]
fn profile_delete_touch_and_recent() {
    let s = mem();
    let p1 = sql_profile("p1", "a");
    let p2 = sql_profile("p2", "b");
    let p3 = sql_profile("p3", "c");
    for p in [&p1, &p2, &p3] {
        s.upsert_profile(p).unwrap();
    }
    assert!(s.recent_profiles(10).unwrap().is_empty());
    s.touch_profile(p1.id).unwrap();
    std::thread::sleep(std::time::Duration::from_millis(2));
    s.touch_profile(p2.id).unwrap();
    std::thread::sleep(std::time::Duration::from_millis(2));
    s.touch_profile(p1.id).unwrap();

    let recent = s.recent_profiles(10).unwrap();
    assert_eq!(
        recent.iter().map(|p| p.id).collect::<Vec<_>>(),
        [p1.id, p2.id]
    );
    assert!(recent[0].last_used.is_some());
    assert_eq!(s.recent_profiles(1).unwrap().len(), 1);
    assert!(matches!(
        s.touch_profile(ProfileId::new()),
        Err(StoreError::NotFound(_))
    ));

    // Re-upserting a profile without last_used does not lose it.
    let stale = p1.clone();
    assert!(stale.last_used.is_none());
    s.upsert_profile(&stale).unwrap();
    assert!(s.get_profile(p1.id).unwrap().unwrap().last_used.is_some());

    assert!(s.delete_profile(p1.id).unwrap());
    assert!(!s.delete_profile(p1.id).unwrap());
    assert_eq!(
        s.recent_profiles(10)
            .unwrap()
            .iter()
            .map(|p| p.id)
            .collect::<Vec<_>>(),
        [p2.id]
    );
    assert_eq!(s.profile_count().unwrap(), 2);
}

// ---- history ---------------------------------------------------------------------------

fn seed_history(s: &Store) -> Vec<i64> {
    let sqls = [
        "SELECT * FROM dbo.Orders WHERE id = 1",
        "select top 10 * from bigtable",
        "UPDATE dbo.Customers SET name = 'x' WHERE id = 2",
        "exec sp_who2",
        "SELECT name FROM sys.tables t JOIN bigger_table b ON b.id = t.object_id",
    ];
    let mut ids = Vec::new();
    for (i, sql) in sqls.iter().enumerate() {
        let mut e = NewHistoryEntry::new("srv1", *sql);
        e.started_at = Utc::now() - Duration::minutes((sqls.len() - i) as i64);
        e.database = Some(if i % 2 == 0 {
            "db1".into()
        } else {
            "db2".into()
        });
        ids.push(s.add_history(&e).unwrap());
    }
    ids
}

#[test]
fn history_add_finish_get() {
    let s = mem();
    let pid = ProfileId::new();
    let tab = TabId::new();
    let mut e = NewHistoryEntry::new("srv", "select 1");
    e.profile_id = Some(pid);
    e.tab_id = Some(tab);
    e.database = Some("master".into());
    let id = s.add_history(&e).unwrap();

    let running = s.get_history(id).unwrap().unwrap();
    assert_eq!(running.status, HistoryStatus::Running);
    assert_eq!(running.profile_id, Some(pid));
    assert_eq!(running.tab_id, Some(tab));
    assert!(running.ended_at.is_none());

    let ended = e.started_at + Duration::milliseconds(1234);
    s.finish_history(id, ended, Some(1234), Some(7), HistoryStatus::Success, None)
        .unwrap();
    let done = s.get_history(id).unwrap().unwrap();
    assert_eq!(done.status, HistoryStatus::Success);
    assert_eq!(done.duration_ms, Some(1234));
    assert_eq!(done.rows, Some(7));
    assert_eq!(
        done.ended_at.unwrap().timestamp_micros(),
        ended.timestamp_micros()
    );
    assert_eq!(
        done.started_at.timestamp_micros(),
        e.started_at.timestamp_micros()
    );

    s.finish_history(id, ended, None, None, HistoryStatus::Error, Some("boom"))
        .unwrap();
    let failed = s.get_history(id).unwrap().unwrap();
    assert_eq!(failed.status, HistoryStatus::Error);
    assert_eq!(failed.error.as_deref(), Some("boom"));
    assert!(matches!(
        s.finish_history(9999, ended, None, None, HistoryStatus::Cancelled, None),
        Err(StoreError::NotFound(_))
    ));
    assert_eq!(s.history_count().unwrap(), 1);
}

#[test]
fn history_search_newest_first_and_filters() {
    let s = mem();
    let ids = seed_history(&s);
    let all = s.search_history(&HistoryQuery::default()).unwrap();
    assert_eq!(
        all.iter().map(|h| h.id).collect::<Vec<_>>(),
        ids.iter().rev().copied().collect::<Vec<_>>()
    );

    let db1 = s
        .search_history(&HistoryQuery {
            database: Some("db1".into()),
            ..Default::default()
        })
        .unwrap();
    assert_eq!(db1.len(), 3);
    assert!(db1.iter().all(|h| h.database.as_deref() == Some("db1")));

    let none = s
        .search_history(&HistoryQuery {
            server: Some("other".into()),
            ..Default::default()
        })
        .unwrap();
    assert!(none.is_empty());

    s.finish_history(
        ids[0],
        Utc::now(),
        None,
        None,
        HistoryStatus::Error,
        Some("x"),
    )
    .unwrap();
    let errs = s
        .search_history(&HistoryQuery {
            status: Some(HistoryStatus::Error),
            ..Default::default()
        })
        .unwrap();
    assert_eq!(errs.iter().map(|h| h.id).collect::<Vec<_>>(), [ids[0]]);

    let paged = s
        .search_history(&HistoryQuery {
            limit: 2,
            offset: 1,
            ..Default::default()
        })
        .unwrap();
    assert_eq!(
        paged.iter().map(|h| h.id).collect::<Vec<_>>(),
        [ids[3], ids[2]]
    );

    let recent = s
        .search_history(&HistoryQuery {
            since: Some(Utc::now() - Duration::minutes(2)),
            ..Default::default()
        })
        .unwrap();
    assert_eq!(recent.iter().map(|h| h.id).collect::<Vec<_>>(), [ids[4]]);
    let old = s
        .search_history(&HistoryQuery {
            until: Some(Utc::now() - Duration::minutes(4)),
            ..Default::default()
        })
        .unwrap();
    assert_eq!(
        old.iter().map(|h| h.id).collect::<Vec<_>>(),
        [ids[1], ids[0]]
    );
}

#[test]
fn history_fts_word_phrase_prefix() {
    let s = mem();
    let ids = seed_history(&s);
    let word = s.search_history(&HistoryQuery::text("select")).unwrap();
    assert_eq!(
        word.iter().map(|h| h.id).collect::<Vec<_>>(),
        [ids[4], ids[1], ids[0]]
    );

    let phrase = s
        .search_history(&HistoryQuery::text("\"from bigtable\""))
        .unwrap();
    assert_eq!(phrase.iter().map(|h| h.id).collect::<Vec<_>>(), [ids[1]]);

    let prefix = s.search_history(&HistoryQuery::text("big*")).unwrap();
    assert_eq!(
        prefix.iter().map(|h| h.id).collect::<Vec<_>>(),
        [ids[4], ids[1]]
    );

    let boolean = s
        .search_history(&HistoryQuery::text("select NOT bigtable"))
        .unwrap();
    assert_eq!(
        boolean.iter().map(|h| h.id).collect::<Vec<_>>(),
        [ids[4], ids[0]]
    );

    // Combined with a filter.
    let q = HistoryQuery {
        text: Some("select".into()),
        database: Some("db2".into()),
        ..Default::default()
    };
    assert_eq!(
        s.search_history(&q)
            .unwrap()
            .iter()
            .map(|h| h.id)
            .collect::<Vec<_>>(),
        [ids[1]]
    );
}

#[test]
fn history_search_falls_back_to_like_on_fts_syntax_error() {
    let s = mem();
    let ids = seed_history(&s);
    // A lone "*" and a leading "-"/"(" are FTS5 syntax errors; LIKE should still find text.
    let star = s.search_history(&HistoryQuery::text("* from big")).unwrap();
    assert_eq!(star.iter().map(|h| h.id).collect::<Vec<_>>(), [ids[1]]);
    let paren = s.search_history(&HistoryQuery::text("(sp_who")).unwrap();
    assert!(paren.is_empty());
    let punct = s.search_history(&HistoryQuery::text("sp_who2")).unwrap();
    assert_eq!(punct.iter().map(|h| h.id).collect::<Vec<_>>(), [ids[3]]);
    let pct = s
        .search_history(&HistoryQuery::text("100% of -nothing"))
        .unwrap();
    assert!(pct.is_empty());
}

#[test]
fn history_star_delete_clear() {
    let s = mem();
    let ids = seed_history(&s);
    s.set_starred(ids[2], true).unwrap();
    assert!(matches!(
        s.set_starred(12345, true),
        Err(StoreError::NotFound(_))
    ));
    let starred = s
        .search_history(&HistoryQuery {
            starred_only: true,
            ..Default::default()
        })
        .unwrap();
    assert_eq!(starred.iter().map(|h| h.id).collect::<Vec<_>>(), [ids[2]]);
    assert!(starred[0].starred);

    assert!(s.delete_history(ids[0]).unwrap());
    assert!(!s.delete_history(ids[0]).unwrap());
    // Deleted rows leave the FTS index too.
    assert!(s
        .search_history(&HistoryQuery::text("Orders"))
        .unwrap()
        .is_empty());

    assert_eq!(s.clear_history(true).unwrap(), 3);
    assert_eq!(s.history_count().unwrap(), 1);
    assert_eq!(
        s.get_history(ids[2]).unwrap().unwrap().sql,
        "UPDATE dbo.Customers SET name = 'x' WHERE id = 2"
    );
    assert_eq!(s.clear_history(false).unwrap(), 1);
    assert_eq!(s.history_count().unwrap(), 0);
}

#[test]
fn history_prune_by_age_and_count_keeps_starred() {
    let s = mem();
    let mut ids = Vec::new();
    for i in 0..10 {
        let mut e = NewHistoryEntry::new("srv", format!("select {i}"));
        e.started_at = Utc::now() - Duration::days(i * 20); // 0, 20, ... 180 days old
        ids.push(s.add_history(&e).unwrap());
    }
    s.set_starred(ids[9], true).unwrap(); // oldest, starred
    s.set_starred(ids[8], true).unwrap();

    // Age: >90 days removes ids[5..=9] minus starred 8, 9 -> removes 5, 6, 7.
    let removed = s.prune_history(90, 0).unwrap();
    assert_eq!(removed, 3);
    let left: Vec<i64> = s
        .search_history(&HistoryQuery::default())
        .unwrap()
        .iter()
        .map(|h| h.id)
        .collect();
    assert_eq!(
        left,
        [ids[0], ids[1], ids[2], ids[3], ids[4], ids[8], ids[9]]
    );

    // Count: keep newest 3 -> unstarred beyond them (ids 3, 4) go; starred 8, 9 stay.
    let removed = s.prune_history(0, 3).unwrap();
    assert_eq!(removed, 2);
    let left: Vec<i64> = s
        .search_history(&HistoryQuery::default())
        .unwrap()
        .iter()
        .map(|h| h.id)
        .collect();
    assert_eq!(left, [ids[0], ids[1], ids[2], ids[8], ids[9]]);

    assert_eq!(s.prune_history(0, 0).unwrap(), 0);
    s.rebuild_history_index().unwrap();
    assert_eq!(
        s.search_history(&HistoryQuery::text("select"))
            .unwrap()
            .len(),
        5
    );
}

// ---- tab snapshots ---------------------------------------------------------------------

#[test]
fn tab_snapshot_lifecycle() {
    let s = mem();
    let t1 = TabId::new();
    let t2 = TabId::new();
    let mut snap1 = TabSnapshot::new(t1, "Query 1", "select 1");
    snap1.profile_id = Some(ProfileId::new());
    snap1.database = Some("db".into());
    snap1.cursor = 5;
    snap1.file_path = Some(std::path::PathBuf::from("C:\\work\\q.sql"));
    snap1.updated_at = Utc::now() - Duration::seconds(10);
    let snap2 = TabSnapshot::new(t2, "Query 2", "select 2");
    s.save_tab(&snap1).unwrap();
    s.save_tab(&snap2).unwrap();

    let open = s.load_open_tabs().unwrap();
    assert_eq!(open.len(), 2);
    assert_eq!(open[0].tab_id, t1, "oldest first");
    let got = s.get_tab(t1).unwrap().unwrap();
    assert_eq!(got.title, "Query 1");
    assert_eq!(got.cursor, 5);
    assert_eq!(got.file_path, snap1.file_path);
    assert_eq!(got.profile_id, snap1.profile_id);
    assert_eq!(
        got.updated_at.timestamp_micros(),
        snap1.updated_at.timestamp_micros()
    );

    // Update text in place.
    snap1.text = "select 1 -- edited".into();
    s.save_tab(&snap1).unwrap();
    assert_eq!(s.get_tab(t1).unwrap().unwrap().text, "select 1 -- edited");

    assert!(s.close_tab(t1).unwrap());
    assert!(!s.close_tab(t1).unwrap(), "already closed");
    assert_eq!(s.load_open_tabs().unwrap().len(), 1);
    let closed = s.recently_closed(10).unwrap();
    assert_eq!(closed.len(), 1);
    assert_eq!(closed[0].text, "select 1 -- edited");
    assert!(closed[0].closed_at.is_some());

    let restored = s.restore_tab(t1).unwrap().unwrap();
    assert!(restored.closed_at.is_none());
    assert_eq!(s.load_open_tabs().unwrap().len(), 2);
    assert!(s.recently_closed(10).unwrap().is_empty());
    assert!(s.restore_tab(TabId::new()).unwrap().is_none());

    assert!(s.delete_tab(t2).unwrap());
    assert!(!s.delete_tab(t2).unwrap());
    assert_eq!(s.load_open_tabs().unwrap().len(), 1);
}

#[test]
fn prune_closed_tabs_keeps_most_recent() {
    let s = mem();
    let mut ids = Vec::new();
    for i in 0..5 {
        let mut t = TabSnapshot::new(TabId::new(), format!("T{i}"), "");
        t.closed_at = Some(Utc::now() - Duration::minutes(10 - i));
        s.save_tab(&t).unwrap();
        ids.push(t.tab_id);
    }
    let open = TabSnapshot::new(TabId::new(), "open", "");
    s.save_tab(&open).unwrap();

    let closed = s.recently_closed(2).unwrap();
    assert_eq!(
        closed.iter().map(|t| t.tab_id).collect::<Vec<_>>(),
        [ids[4], ids[3]]
    );
    assert_eq!(s.prune_closed_tabs(2).unwrap(), 3);
    let closed = s.recently_closed(10).unwrap();
    assert_eq!(
        closed.iter().map(|t| t.tab_id).collect::<Vec<_>>(),
        [ids[4], ids[3]]
    );
    assert_eq!(s.load_open_tabs().unwrap().len(), 1, "open tabs untouched");
}

// ---- catalog cache ---------------------------------------------------------------------

fn sample_catalog(db: &str) -> DatabaseCatalog {
    let mut c = DatabaseCatalog {
        database: db.into(),
        schemas: vec!["dbo".into(), "sales".into()],
        ..Default::default()
    };
    c.objects.push(ObjectRef {
        database: db.into(),
        schema: "dbo".into(),
        name: "Orders".into(),
        kind: ObjectKind::Table,
        object_id: Some(10),
    });
    c.columns
        .insert(10, vec![ColumnInfo::new("id", SqlType::Int, false, 0)]);
    c.refreshed_at = Some(Utc::now());
    c
}

#[test]
fn catalog_cache_roundtrip_and_invalidate() {
    let s = mem();
    let p = ProfileId::new();
    assert!(s.get_catalog(p, "db1").unwrap().is_none());
    let c1 = sample_catalog("db1");
    let c2 = sample_catalog("db2");
    s.put_catalog(p, "db1", &c1).unwrap();
    s.put_catalog(p, "db2", &c2).unwrap();
    s.put_catalog(ProfileId::new(), "db1", &c1).unwrap();

    let (got, at) = s.get_catalog(p, "db1").unwrap().unwrap();
    assert_eq!(got, c1);
    assert!((Utc::now() - at).num_seconds() < 5);

    // Overwrite.
    let mut c1b = c1.clone();
    c1b.schemas.push("extra".into());
    s.put_catalog(p, "db1", &c1b).unwrap();
    assert_eq!(s.get_catalog(p, "db1").unwrap().unwrap().0.schemas.len(), 3);

    assert_eq!(s.invalidate_catalog(p, Some("db1")).unwrap(), 1);
    assert!(s.get_catalog(p, "db1").unwrap().is_none());
    assert!(s.get_catalog(p, "db2").unwrap().is_some());
    assert_eq!(s.invalidate_catalog(p, None).unwrap(), 1);
    assert!(s.get_catalog(p, "db2").unwrap().is_none());
}

#[test]
fn deleting_profile_drops_its_catalog_cache() {
    let s = mem();
    let p = sql_profile("p", "srv");
    s.upsert_profile(&p).unwrap();
    s.put_catalog(p.id, "db", &sample_catalog("db")).unwrap();
    s.delete_profile(p.id).unwrap();
    assert!(s.get_catalog(p.id, "db").unwrap().is_none());
}

// ---- kv --------------------------------------------------------------------------------

#[test]
fn kv_roundtrip_typed() {
    let s = mem();
    #[derive(serde::Serialize, serde::Deserialize, PartialEq, Debug)]
    struct Geometry {
        x: i32,
        y: i32,
        maximized: bool,
    }
    assert_eq!(s.get_kv::<Geometry>("window").unwrap(), None);
    let g = Geometry {
        x: 10,
        y: -5,
        maximized: true,
    };
    s.set_kv("window", &g).unwrap();
    assert_eq!(s.get_kv::<Geometry>("window").unwrap(), Some(g));
    s.set_kv(
        "window",
        &Geometry {
            x: 1,
            y: 1,
            maximized: false,
        },
    )
    .unwrap();
    assert_eq!(s.get_kv::<Geometry>("window").unwrap().unwrap().x, 1);
    s.set_kv("name", "str value").unwrap();
    assert_eq!(
        s.get_kv::<String>("name").unwrap().as_deref(),
        Some("str value")
    );
    // Type mismatch is a miss, not an error.
    assert_eq!(s.get_kv::<Geometry>("name").unwrap(), None);
    assert!(s.delete_kv("name").unwrap());
    assert!(!s.delete_kv("name").unwrap());
}

// ---- library export / import -----------------------------------------------------------

#[test]
fn library_export_import_replace() {
    let a = mem();
    let g1 = ServerGroup::new("G1", Color::PALETTE[0]);
    let mut g2 = ServerGroup::new("G2", Color::PALETTE[1]);
    g2.parent = Some(g1.id);
    g2.sort_order = 1;
    a.upsert_group(&g1).unwrap();
    a.upsert_group(&g2).unwrap();
    let mut p1 = entra_profile("p1", "s1.database.windows.net");
    p1.group = Some(g2.id);
    let p2 = sql_profile("p2", "s2");
    let mut p2_with_secret = p2.clone();
    p2_with_secret.auth = AuthMethod::SqlLogin {
        user: "sa".into(),
        password: Some(SecretRef::for_profile(&p2.id, "password")),
    };
    a.upsert_profile(&p1).unwrap();
    a.upsert_profile(&p2_with_secret).unwrap();

    let lib = a.export_library().unwrap();
    assert_eq!(lib.version, LibraryExport::VERSION);
    assert_eq!(lib.groups.len(), 2);
    assert_eq!(lib.profiles.len(), 2);
    let exported_p2 = lib.profiles.iter().find(|p| p.id == p2.id).unwrap();
    assert_eq!(
        exported_p2.auth,
        AuthMethod::SqlLogin {
            user: "sa".into(),
            password: None
        },
        "secret refs stripped"
    );

    let json = lib.to_json().unwrap();
    assert!(json.contains("\"version\": 1"));
    let parsed = LibraryExport::from_json(&json).unwrap();
    assert_eq!(parsed, lib);

    let b = mem();
    b.upsert_profile(&sql_profile("pre-existing", "old"))
        .unwrap();
    let summary = b.import_library(&parsed, false).unwrap();
    assert_eq!(
        (summary.groups, summary.profiles, summary.orphaned_profiles),
        (2, 2, 0)
    );
    assert_eq!(b.list_groups().unwrap(), a.list_groups().unwrap());
    assert_eq!(b.get_profile(p1.id).unwrap().unwrap(), p1);
    assert_eq!(
        b.profile_count().unwrap(),
        2,
        "replace mode dropped the pre-existing profile"
    );

    let mut newer = parsed.clone();
    newer.version = 99;
    assert!(matches!(
        LibraryExport::from_json(&newer.to_json().unwrap()),
        Err(StoreError::Invalid(_))
    ));
}

#[test]
fn library_import_merge_keeps_existing_and_handles_orphans() {
    let s = mem();
    let keep_group = ServerGroup::new("Keep", Color::PALETTE[0]);
    s.upsert_group(&keep_group).unwrap();
    let mut keep = sql_profile("keep", "k");
    keep.group = Some(keep_group.id);
    s.upsert_profile(&keep).unwrap();
    let mut existing = sql_profile("existing", "e");
    existing.group = Some(keep_group.id);
    s.upsert_profile(&existing).unwrap();

    let new_group = ServerGroup::new("New", Color::PALETTE[2]);
    let mut updated = existing.clone();
    updated.name = Some("existing-updated".into());
    updated.group = Some(new_group.id);
    let mut orphan = sql_profile("orphan", "o");
    orphan.group = Some(GroupId::new()); // not in the import, not in the store
    let mut child_of_kept = ServerGroup::new("Child", Color::PALETTE[3]);
    child_of_kept.parent = Some(keep_group.id); // parent exists in store only
    let lib = LibraryExport::new(
        vec![new_group.clone(), child_of_kept.clone()],
        vec![updated.clone(), orphan.clone()],
    );

    let summary = s.import_library(&lib, true).unwrap();
    assert_eq!(
        (summary.groups, summary.profiles, summary.orphaned_profiles),
        (2, 2, 1)
    );
    assert_eq!(s.list_groups().unwrap().len(), 3);
    assert_eq!(
        s.get_group(child_of_kept.id).unwrap().unwrap().parent,
        Some(keep_group.id)
    );
    assert_eq!(s.get_profile(keep.id).unwrap().unwrap(), keep);
    let e = s.get_profile(existing.id).unwrap().unwrap();
    assert_eq!(e.name.as_deref(), Some("existing-updated"));
    assert_eq!(e.group, Some(new_group.id));
    assert_eq!(s.get_profile(orphan.id).unwrap().unwrap().group, None);
    assert_eq!(s.profile_count().unwrap(), 3);
}

#[test]
fn library_import_group_order_independent() {
    // Child listed before parent must still link up.
    let s = mem();
    let parent = ServerGroup::new("Parent", Color::PALETTE[0]);
    let mut child = ServerGroup::new("Child", Color::PALETTE[1]);
    child.parent = Some(parent.id);
    let lib = LibraryExport::new(vec![child.clone(), parent.clone()], vec![]);
    s.import_library(&lib, false).unwrap();
    assert_eq!(
        s.get_group(child.id).unwrap().unwrap().parent,
        Some(parent.id)
    );
}
