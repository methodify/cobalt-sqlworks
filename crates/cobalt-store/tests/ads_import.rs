use cobalt_core::*;
use cobalt_store::*;

/// A realistic ADS `settings.json`: comments, trailing commas, ROOT group, nested groups,
/// three MSSQL connections (SQL login, Windows, Entra MFA to Fabric) and a PostgreSQL one.
const FIXTURE: &str = r##"{
    // Azure Data Studio user settings
    "workbench.colorTheme": "Default Dark Azure Data Studio",
    "editor.fontSize": 14,
    "datasource.connectionGroups": [
        {
            "name": "ROOT",
            "id": "C777F06B-202E-4480-B475-FA416154D458"
        },
        {
            "name": "Production",
            "id": "6b8a4d1e-9c1f-4d63-a8bd-6a2e2b0c5c11",
            "parentId": "C777F06B-202E-4480-B475-FA416154D458",
            "color": "#a1634d",
            "description": "Live systems",
        },
        {
            "name": "Fabric",
            "id": "1d3e2f4a-5b6c-4d7e-8f90-a1b2c3d4e5f6",
            "parentId": "6b8a4d1e-9c1f-4d63-a8bd-6a2e2b0c5c11",
            "color": "teal"
        },
        {
            "name": "Broken parent",
            "id": "7e7e7e7e-0000-4000-8000-000000000001",
            "parentId": "does-not-exist"
        },
    ],
    "datasource.connections": [
        {
            "options": {
                "connectionName": "Orders DB",
                "server": "sql-prod-01.contoso.local,1433",
                "database": "Orders",
                "authenticationType": "SqlLogin",
                "user": "orders_app",
                "password": "",
                "applicationName": "azdata",
                "groupId": "6b8a4d1e-9c1f-4d63-a8bd-6a2e2b0c5c11",
                "databaseDisplayName": "Orders",
                "encrypt": "Mandatory",
                "trustServerCertificate": true,
                "connectTimeout": 15,
                "commandTimeout": "60",
                "applicationIntent": "ReadOnly", /* replica */
            },
            "groupId": "6b8a4d1e-9c1f-4d63-a8bd-6a2e2b0c5c11",
            "providerName": "MSSQL",
            "savePassword": true,
            "id": "2f0c5d4a-1111-4a2b-9c3d-000000000001"
        },
        {
            "options": {
                "connectionName": "",
                "server": "localhost\\SQLEXPRESS",
                "database": "",
                "authenticationType": "Integrated",
                "user": "",
                "applicationName": "azdata",
                "groupId": "C777F06B-202E-4480-B475-FA416154D458",
                "encrypt": "false",
                "trustServerCertificate": "true"
            },
            "groupId": "C777F06B-202E-4480-B475-FA416154D458",
            "providerName": "MSSQL",
            "savePassword": false,
            "id": "2f0c5d4a-1111-4a2b-9c3d-000000000002"
        },
        {
            "options": {
                "connectionName": "Fabric DW",
                "server": "abcd1234-wxyz.datawarehouse.fabric.microsoft.com",
                "database": "Lakehouse_DW",
                "authenticationType": "AzureMFA",
                "user": "bryon@contoso.com",
                "azureAccount": "bryon@contoso.com",
                "azureTenantId": "72f988bf-86f1-41af-91ab-2d7cd011db47",
                "applicationName": "azdata",
                "groupId": "1d3e2f4a-5b6c-4d7e-8f90-a1b2c3d4e5f6",
                "encrypt": "Strict",
                "trustServerCertificate": false
            },
            "groupId": "1d3e2f4a-5b6c-4d7e-8f90-a1b2c3d4e5f6",
            "providerName": "MSSQL",
            "savePassword": false,
            "id": "2f0c5d4a-1111-4a2b-9c3d-000000000003"
        },
        {
            "options": {
                "connectionName": "Postgres analytics",
                "host": "pg.contoso.local",
                "server": "pg.contoso.local",
                "dbname": "analytics",
                "authenticationType": "SqlLogin",
                "user": "postgres"
            },
            "groupId": "C777F06B-202E-4480-B475-FA416154D458",
            "providerName": "PGSQL",
            "savePassword": true,
            "id": "2f0c5d4a-1111-4a2b-9c3d-000000000004"
        }
    ],
    "sql.showConnectionInfoInTitle": true
}"##;

#[test]
fn ads_fixture_groups_hierarchy() {
    let imp = parse_ads_settings_detailed(FIXTURE).unwrap();
    let groups = &imp.library.groups;
    assert_eq!(groups.len(), 3, "ROOT is skipped: {groups:?}");
    let prod = groups.iter().find(|g| g.name == "Production").unwrap();
    let fabric = groups.iter().find(|g| g.name == "Fabric").unwrap();
    let broken = groups.iter().find(|g| g.name == "Broken parent").unwrap();
    assert_eq!(prod.parent, None, "child of ROOT is top-level");
    assert_eq!(prod.color, Color(0xA1, 0x63, 0x4D));
    assert_eq!(prod.description.as_deref(), Some("Live systems"));
    assert_eq!(
        prod.id,
        GroupId::parse("6b8a4d1e-9c1f-4d63-a8bd-6a2e2b0c5c11").unwrap(),
        "ADS GUID ids are preserved"
    );
    assert_eq!(fabric.parent, Some(prod.id));
    assert_eq!(fabric.color, Color(0x16, 0x9C, 0xA8), "palette name mapped");
    assert_eq!(broken.parent, None);
    assert!(imp.warnings.iter().any(|w| w.contains("does-not-exist")));
    assert!(prod.sort_order < fabric.sort_order);
}

#[test]
fn ads_fixture_connections() {
    let imp = parse_ads_settings_detailed(FIXTURE).unwrap();
    let profiles = &imp.library.profiles;
    assert_eq!(profiles.len(), 3);
    assert_eq!(imp.skipped_count(), 1);
    assert_eq!(
        imp.skipped[0],
        ("Postgres analytics".to_string(), "PGSQL".to_string())
    );

    let prod = imp
        .library
        .groups
        .iter()
        .find(|g| g.name == "Production")
        .unwrap();
    let fabric_g = imp
        .library
        .groups
        .iter()
        .find(|g| g.name == "Fabric")
        .unwrap();

    let orders = profiles
        .iter()
        .find(|p| p.name.as_deref() == Some("Orders DB"))
        .unwrap();
    assert_eq!(orders.server, "sql-prod-01.contoso.local,1433");
    assert_eq!(orders.database.as_deref(), Some("Orders"));
    assert_eq!(
        orders.auth,
        AuthMethod::SqlLogin {
            user: "orders_app".into(),
            password: None
        }
    );
    assert_eq!(orders.group, Some(prod.id));
    assert_eq!(orders.options.encrypt, Encrypt::Mandatory);
    assert!(orders.options.trust_server_certificate);
    assert_eq!(orders.options.connect_timeout_secs, 15);
    assert_eq!(orders.options.command_timeout_secs, 60);
    assert_eq!(
        orders.options.application_intent,
        ApplicationIntent::ReadOnly
    );
    assert_eq!(
        orders.options.application_name,
        ConnectionOptions::default().application_name,
        "azdata not carried over"
    );
    assert_eq!(
        orders.id,
        ProfileId::parse("2f0c5d4a-1111-4a2b-9c3d-000000000001").unwrap()
    );

    let local = profiles
        .iter()
        .find(|p| p.server == "localhost\\SQLEXPRESS")
        .unwrap();
    assert_eq!(local.name, None, "empty connectionName becomes None");
    assert_eq!(local.database, None);
    assert_eq!(local.auth, AuthMethod::WindowsIntegrated);
    assert_eq!(local.group, None, "ROOT group becomes ungrouped");
    assert_eq!(local.options.encrypt, Encrypt::Optional);
    assert!(
        local.options.trust_server_certificate,
        "string \"true\" accepted"
    );
    assert_eq!(local.display_name(), "localhost\\SQLEXPRESS");

    let fabric = profiles
        .iter()
        .find(|p| p.name.as_deref() == Some("Fabric DW"))
        .unwrap();
    assert!(fabric.looks_like_fabric());
    assert_eq!(
        fabric.auth,
        AuthMethod::EntraInteractive {
            tenant: Some("72f988bf-86f1-41af-91ab-2d7cd011db47".into()),
            account_hint: Some("bryon@contoso.com".into())
        }
    );
    assert_eq!(fabric.group, Some(fabric_g.id));
    assert_eq!(fabric.options.encrypt, Encrypt::Strict);
    assert!(!fabric.options.trust_server_certificate);
    assert_eq!(fabric.database.as_deref(), Some("Lakehouse_DW"));
}

#[test]
fn ads_import_lands_in_store() {
    let lib = parse_ads_settings(FIXTURE).unwrap();
    let s = Store::open_in_memory().unwrap();
    let summary = s.import_library(&lib, false).unwrap();
    assert_eq!(
        (summary.groups, summary.profiles, summary.orphaned_profiles),
        (3, 3, 0)
    );
    let stored = s.list_profiles().unwrap();
    assert_eq!(stored.len(), 3);
    let fabric = stored
        .iter()
        .find(|p| p.name.as_deref() == Some("Fabric DW"))
        .unwrap();
    assert!(fabric.auth.is_entra());
    let groups = s.list_groups().unwrap();
    let fabric_g = groups.iter().find(|g| g.name == "Fabric").unwrap();
    let prod = groups.iter().find(|g| g.name == "Production").unwrap();
    assert_eq!(fabric_g.parent, Some(prod.id));
}

#[test]
fn ads_edge_cases() {
    // Missing keys: empty library plus a warning, not an error.
    let imp = parse_ads_settings_detailed("{ \"editor.fontSize\": 12 }").unwrap();
    assert!(imp.library.groups.is_empty() && imp.library.profiles.is_empty());
    assert_eq!(imp.warnings.len(), 1);

    // Not JSON at all.
    assert!(matches!(
        parse_ads_settings("not json"),
        Err(StoreError::Ads(_))
    ));
    assert!(matches!(
        parse_ads_settings("[1,2]"),
        Err(StoreError::Ads(_))
    ));

    // AzureMFAAndUser without azureAccount falls back to user; non-GUID ids get fresh ones;
    // no server is skipped with a warning; unknown auth type is a warning.
    let imp = parse_ads_settings_detailed(
        r#"{
        "datasource.connections": [
            { "id": "ROOTISH", "providerName": "MSSQL", "options": {
                "server": "a.database.windows.net", "authenticationType": "AzureMFAAndUser",
                "user": "u@x.com", "encrypt": true, "port": 1433 } },
            { "id": "x", "providerName": "MSSQL", "options": { "authenticationType": "SqlLogin" } },
            { "id": "y", "providerName": "MSSQL", "options": { "server": "s", "authenticationType": "dstsAuth", "user": "z" } },
            { "id": "z", "options": { "server": "s2" } }
        ]}"#,
    )
    .unwrap();
    assert_eq!(imp.library.profiles.len(), 3);
    let a = &imp.library.profiles[0];
    assert_eq!(
        a.auth,
        AuthMethod::EntraInteractive {
            tenant: None,
            account_hint: Some("u@x.com".into())
        }
    );
    assert_eq!(a.options.encrypt, Encrypt::Mandatory);
    assert_eq!(a.port, Some(1433));
    assert!(imp.warnings.iter().any(|w| w.contains("no server")));
    assert!(imp.warnings.iter().any(|w| w.contains("dstsAuth")));
    assert_eq!(
        imp.library.profiles[1].auth,
        AuthMethod::SqlLogin {
            user: "z".into(),
            password: None
        }
    );
    assert_eq!(
        imp.library.profiles[2].auth,
        AuthMethod::SqlLogin {
            user: String::new(),
            password: None
        },
        "missing provider/auth = MSSQL SQL login"
    );
}

#[test]
fn ads_default_path_candidates() {
    let cands = cobalt_store::ads_import::ads_settings_candidates();
    assert!(cands
        .iter()
        .all(|p| p.ends_with(std::path::Path::new("azuredatastudio/User/settings.json"))));
    // Whatever exists (or not) on this box, the function must not panic.
    let _ = default_ads_settings_path();
}
