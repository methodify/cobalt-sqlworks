use cobalt_core::*;
use cobalt_store::*;
use std::fs;

#[test]
fn settings_roundtrip() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("deep").join("settings.toml");
    let mut s = Settings::default();
    s.appearance.theme = ThemeChoice::Dark;
    s.appearance.ui_scale = 1.25;
    s.editor.tab_size = 2;
    s.editor.word_wrap = true;
    s.execution.row_cap = 500;
    s.results.null_text = "<null>".into();
    s.results.layout = ResultLayout::Tabs;
    s.export.last_dir = Some("C:\\exports".into());
    s.export.csv_line_ending = "\n".into();
    s.connections.entra_default_tenant = Some("tenant".into());
    s.history.retention_days = 7;
    s.advanced.temp_dir = Some("D:\\tmp".into());
    save_settings(&path, &s).unwrap();

    let text = fs::read_to_string(&path).unwrap();
    assert!(text.starts_with("# Cobalt SQL Works settings"));
    assert!(text.contains("[editor]"));
    assert!(text.contains("tab_size = 2"));
    assert!(!text.contains(".tmp"));
    assert_eq!(fs::read_dir(path.parent().unwrap()).unwrap().count(), 1, "no temp file left behind");

    let loaded = load_settings(&path);
    assert_eq!(loaded, s);
}

#[test]
fn settings_missing_file_is_default() {
    let dir = tempfile::tempdir().unwrap();
    assert_eq!(load_settings(&dir.path().join("nope.toml")), Settings::default());
}

#[test]
fn settings_partial_file_fills_defaults() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("settings.toml");
    fs::write(&path, "[editor]\ntab_size = 2\n").unwrap();
    let s = load_settings(&path);
    assert_eq!(s.editor.tab_size, 2);
    assert_eq!(s.editor.insert_spaces, EditorSettings::default().insert_spaces);
    assert_eq!(s.appearance, Appearance::default());
    assert_eq!(s.history, HistorySettings::default());

    // Unknown keys are tolerated (forward compatibility).
    fs::write(&path, "[editor]\ntab_size = 3\nfuture_knob = true\n[future_section]\nx = 1\n").unwrap();
    assert_eq!(load_settings(&path).editor.tab_size, 3);
}

#[test]
fn settings_bad_file_defaults_and_backs_up() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("settings.toml");
    fs::write(&path, "[editor\ntab_size = = 2\n").unwrap();
    assert_eq!(load_settings(&path), Settings::default());
    let bad = cobalt_store::settings::bad_path(&path);
    assert!(bad.ends_with("settings.toml.bad"));
    assert_eq!(fs::read_to_string(bad).unwrap(), "[editor\ntab_size = = 2\n");
    // Wrong type is also a parse failure.
    fs::write(&path, "[editor]\ntab_size = \"two\"\n").unwrap();
    assert_eq!(load_settings(&path), Settings::default());
}

#[test]
fn settings_save_overwrites_existing() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("settings.toml");
    save_settings(&path, &Settings::default()).unwrap();
    let mut s = Settings::default();
    s.editor.tab_size = 8;
    save_settings(&path, &s).unwrap();
    assert_eq!(load_settings(&path).editor.tab_size, 8);
}

#[test]
fn app_paths_for_test_and_spill_cleanup() {
    let dir = tempfile::tempdir().unwrap();
    let paths = AppPaths::for_test(dir.path());
    for d in [&paths.config_dir, &paths.data_dir, &paths.cache_dir, &paths.log_dir, &paths.temp_dir] {
        assert!(d.is_dir(), "{}", d.display());
    }
    assert_eq!(paths.settings_file(), dir.path().join("config").join("settings.toml"));
    assert_eq!(paths.db_file(), dir.path().join("data").join("cobalt.db"));
    assert_eq!(paths.spill_dir(), dir.path().join("temp").join("spill"));

    assert_eq!(paths.clean_spill_dir().unwrap(), 0);
    let spill = paths.spill_dir();
    assert!(spill.is_dir());
    fs::write(spill.join(format!("{SPILL_PREFIX}1.bin")), b"x").unwrap();
    fs::write(spill.join(format!("{SPILL_PREFIX}2.bin")), b"y").unwrap();
    fs::create_dir(spill.join(format!("{SPILL_PREFIX}dir"))).unwrap();
    fs::write(spill.join(format!("{SPILL_PREFIX}dir")).join("part"), b"z").unwrap();
    fs::write(spill.join("keep-me.txt"), b"k").unwrap();
    assert_eq!(paths.clean_spill_dir().unwrap(), 3);
    let left: Vec<_> = fs::read_dir(&spill).unwrap().map(|e| e.unwrap().file_name().into_string().unwrap()).collect();
    assert_eq!(left, ["keep-me.txt"]);

    let moved = paths.clone().with_temp_dir(dir.path().join("other-temp")).unwrap();
    assert!(moved.temp_dir.is_dir());
    assert_eq!(moved.spill_dir(), dir.path().join("other-temp").join("spill"));

    // Store opens fine at the resolved db path.
    let store = Store::open(&paths.db_file()).unwrap();
    assert_eq!(store.schema_version().unwrap(), SCHEMA_VERSION);
}

#[test]
fn app_paths_platform_dirs_resolve() {
    let paths = AppPaths::new().unwrap();
    assert!(paths.config_dir.is_dir());
    assert!(paths.data_dir.is_dir());
    assert!(paths.settings_file().ends_with("settings.toml"));
    assert!(paths.db_file().ends_with("cobalt.db"));
}
