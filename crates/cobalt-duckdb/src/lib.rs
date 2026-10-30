//! DuckDB for Cobalt without a build-time dependency: the official `libduckdb` shared library
//! of one pinned version is downloaded on first use (about 20 MB, from DuckDB's GitHub
//! release), loaded with `libloading`, and driven through the C API ([`api`]). Results come
//! out as Arrow through the C data interface, so a DuckDB query feeds the grid the way a SQL
//! Server or SQLite one does.
//!
//! One `Database` per file per process (DuckDB locks the file; connections share the
//! instance), one `Connection` per query tab or metadata session, each used from one thread.
//! Cancel is `duckdb_interrupt` from any thread.

pub mod api;
pub mod types;

pub use types::*;

use api::{Api, SUCCESS};
use arrow::array::{RecordBatch, StructArray};
use arrow::datatypes::{Schema, SchemaRef};
use arrow::ffi::{FFI_ArrowArray, FFI_ArrowSchema};
use arrow::ffi_stream::FFI_ArrowArrayStream;
use arrow::record_batch::RecordBatchReader;
use parking_lot::Mutex;
use std::collections::HashMap;
use std::ffi::CString;
use std::os::raw::{c_char, c_void};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicPtr, Ordering};
use std::sync::{Arc, OnceLock, Weak};

/// The DuckDB release Cobalt downloads and speaks to. Files written by a newer DuckDB may
/// not open; the error names the versions.
pub const VERSION: &str = "1.5.6";

/// The release asset for this platform.
pub fn asset_name() -> Option<&'static str> {
    match (std::env::consts::OS, std::env::consts::ARCH) {
        ("windows", "x86_64") => Some("libduckdb-windows-amd64.zip"),
        ("windows", "aarch64") => Some("libduckdb-windows-arm64.zip"),
        ("linux", "x86_64") => Some("libduckdb-linux-amd64.zip"),
        ("linux", "aarch64") => Some("libduckdb-linux-arm64.zip"),
        ("macos", _) => Some("libduckdb-osx-universal.zip"),
        _ => None,
    }
}

pub fn download_url() -> Option<String> {
    asset_name().map(|a| format!("https://github.com/duckdb/duckdb/releases/download/v{VERSION}/{a}"))
}

pub fn lib_file_name() -> &'static str {
    if cfg!(windows) {
        "duckdb.dll"
    } else if cfg!(target_os = "macos") {
        "libduckdb.dylib"
    } else {
        "libduckdb.so"
    }
}

/// `<root>/duckdb/<version>/`
pub fn install_dir(root: &Path) -> PathBuf {
    root.join("duckdb").join(VERSION)
}

pub fn lib_path(root: &Path) -> PathBuf {
    install_dir(root).join(lib_file_name())
}

pub fn is_installed(root: &Path) -> bool {
    lib_path(root).is_file()
}

/// Bytes the download is about (the progress bar's total before the server says).
pub const DOWNLOAD_BYTES_HINT: u64 = 22_000_000;

/// Download the library for this platform into `root` (a `.part` file renamed at the end) and
/// unpack the shared library out of the zip. `progress(done, total)` runs as bytes arrive.
pub fn install(root: &Path, progress: &mut dyn FnMut(u64, Option<u64>)) -> std::result::Result<PathBuf, String> {
    let url = download_url().ok_or_else(|| format!("DuckDB ships no library for {} {}", std::env::consts::OS, std::env::consts::ARCH))?;
    let dir = install_dir(root);
    std::fs::create_dir_all(&dir).map_err(|e| format!("{}: {e}", dir.display()))?;
    let zip_path = dir.join("libduckdb.zip.part");
    {
        let client = reqwest::blocking::Client::builder().user_agent(concat!("cobalt-sqlworks/", env!("CARGO_PKG_VERSION"))).timeout(std::time::Duration::from_secs(600)).build().map_err(|e| e.to_string())?;
        let mut resp = client.get(&url).send().map_err(|e| format!("{url}: {e}"))?;
        if !resp.status().is_success() {
            return Err(format!("{url}: HTTP {}", resp.status()));
        }
        let total = resp.content_length();
        let mut file = std::fs::File::create(&zip_path).map_err(|e| format!("{}: {e}", zip_path.display()))?;
        let mut buf = vec![0u8; 1 << 16];
        let mut done = 0u64;
        progress(0, total);
        loop {
            let n = std::io::Read::read(&mut resp, &mut buf).map_err(|e| format!("{url}: {e}"))?;
            if n == 0 {
                break;
            }
            std::io::Write::write_all(&mut file, &buf[..n]).map_err(|e| format!("{}: {e}", zip_path.display()))?;
            done += n as u64;
            progress(done, total);
        }
    }
    let out = lib_path(root);
    {
        let f = std::fs::File::open(&zip_path).map_err(|e| e.to_string())?;
        let mut z = zip::ZipArchive::new(f).map_err(|e| format!("{}: {e}", zip_path.display()))?;
        let want = lib_file_name();
        let idx = (0..z.len()).find(|&i| z.by_index(i).map(|e| e.name().rsplit('/').next().unwrap_or("") == want).unwrap_or(false)).ok_or_else(|| format!("{want} is not in {}", url))?;
        let mut entry = z.by_index(idx).map_err(|e| e.to_string())?;
        let tmp = out.with_extension("part");
        let mut w = std::fs::File::create(&tmp).map_err(|e| format!("{}: {e}", tmp.display()))?;
        std::io::copy(&mut entry, &mut w).map_err(|e| e.to_string())?;
        drop(w);
        if out.exists() {
            let _ = std::fs::remove_file(&out);
        }
        std::fs::rename(&tmp, &out).map_err(|e| format!("{}: {e}", out.display()))?;
    }
    let _ = std::fs::remove_file(&zip_path);
    tracing::info!(path = %out.display(), "DuckDB library installed");
    Ok(out)
}

/// Delete the installed library (and forget the loaded one for new opens; databases already
/// open keep working until closed).
pub fn remove(root: &Path) -> std::result::Result<(), String> {
    let dir = root.join("duckdb");
    if dir.exists() {
        std::fs::remove_dir_all(&dir).map_err(|e| format!("{}: {e}", dir.display()))?;
    }
    Ok(())
}

/// The loaded library: its function table and the handle that keeps it mapped.
pub struct Loaded {
    pub api: Api,
    pub version: String,
    _lib: libloading::Library,
}

static LOADED: OnceLock<Mutex<Option<Arc<Loaded>>>> = OnceLock::new();

/// Load the installed library (once per process; later calls share it).
pub fn load(root: &Path) -> std::result::Result<Arc<Loaded>, String> {
    let slot = LOADED.get_or_init(|| Mutex::new(None));
    let mut g = slot.lock();
    if let Some(l) = g.as_ref() {
        return Ok(Arc::clone(l));
    }
    let path = lib_path(root);
    if !path.is_file() {
        return Err(format!("the DuckDB {VERSION} library is not installed ({})", path.display()));
    }
    // SAFETY: loading the DuckDB shared library runs its initialisers; it is the official build.
    let lib = unsafe { libloading::Library::new(&path) }.map_err(|e| format!("{}: {e}", path.display()))?;
    let api = Api::load(&lib).map_err(|e| format!("{} is not a DuckDB {VERSION} library: {e}", path.display()))?;
    let version = unsafe { api::text((api.library_version)()) };
    let loaded = Arc::new(Loaded { api, version, _lib: lib });
    *g = Some(Arc::clone(&loaded));
    Ok(loaded)
}

/// `true` when the first 12 bytes carry DuckDB's magic (`DUCK` at offset 8).
pub fn is_duckdb_file(path: &Path) -> bool {
    use std::io::Read;
    let Ok(mut f) = std::fs::File::open(path) else { return false };
    let mut head = [0u8; 12];
    f.read_exact(&mut head).map(|_| &head[8..12] == b"DUCK").unwrap_or(false)
}

// ---------------------------------------------------------------------------------------------
// Database (one per file per process) and Connection (one per thread of use)
// ---------------------------------------------------------------------------------------------

pub struct Database {
    loaded: Arc<Loaded>,
    handle: api::Database,
    pub path: PathBuf,
    pub read_only: bool,
}

impl std::fmt::Debug for Database {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "Database({}{})", self.path.display(), if self.read_only { ", read-only" } else { "" })
    }
}

// SAFETY: a duckdb_database is thread-safe (connections are made and used from any thread).
unsafe impl Send for Database {}
unsafe impl Sync for Database {}

impl Drop for Database {
    fn drop(&mut self) {
        let mut h = self.handle;
        unsafe { (self.loaded.api.close)(&mut h) };
        OPEN.get_or_init(Default::default).lock().remove(&self.path);
    }
}

static OPEN: OnceLock<Mutex<HashMap<PathBuf, Weak<Database>>>> = OnceLock::new();

/// Open (or share) the database at `path`: one instance per file per process (DuckDB locks the
/// file). A read-only instance refuses a writable open until its handles are gone; a writable
/// instance serves read-only opens.
pub fn open(root: &Path, path: &Path, read_only: bool) -> std::result::Result<Arc<Database>, String> {
    let loaded = load(root)?;
    let key = registry_key(path);
    let reg = OPEN.get_or_init(Default::default);
    if let Some(db) = reg.lock().get(&key).and_then(Weak::upgrade) {
        if db.read_only && !read_only {
            return Err(format!("{} is open read-only in this session (a connection opened it so); disconnect those tabs first to write to it", path.display()));
        }
        return Ok(db);
    }
    let api = &loaded.api;
    let cpath = CString::new(path.to_string_lossy().as_bytes()).map_err(|e| e.to_string())?;
    let mut config: api::Config = std::ptr::null_mut();
    unsafe {
        if (api.create_config)(&mut config) != SUCCESS {
            return Err("could not create a DuckDB configuration".into());
        }
        if read_only {
            let k = CString::new("access_mode").unwrap();
            let v = CString::new("READ_ONLY").unwrap();
            (api.set_config)(config, k.as_ptr(), v.as_ptr());
        }
        let mut handle: api::Database = std::ptr::null_mut();
        let mut err: *mut c_char = std::ptr::null_mut();
        let st = (api.open_ext)(cpath.as_ptr(), &mut handle, config, &mut err);
        (api.destroy_config)(&mut config);
        if st != SUCCESS {
            let msg = api::text(err);
            if !err.is_null() {
                (api.free)(err as *mut c_void);
            }
            return Err(if msg.is_empty() { format!("could not open {}", path.display()) } else { msg });
        }
        let db = Arc::new(Database { loaded: Arc::clone(&loaded), handle, path: key.clone(), read_only });
        reg.lock().insert(key, Arc::downgrade(&db));
        Ok(db)
    }
}

/// One key per file however the path was spelt: the canonical path (the parent's when the
/// file does not exist yet), without the `\?\` prefix, lower-cased on Windows.
pub fn registry_key(path: &Path) -> PathBuf {
    let canon = std::fs::canonicalize(path).or_else(|_| {
        let parent = path.parent().filter(|p| !p.as_os_str().is_empty()).map(Path::to_path_buf).unwrap_or_else(|| PathBuf::from("."));
        let name = path.file_name().map(PathBuf::from).unwrap_or_default();
        std::fs::canonicalize(&parent).map(|p| p.join(name))
    });
    let p = canon.unwrap_or_else(|_| path.to_path_buf());
    let text = p.to_string_lossy().trim_start_matches(r"\?\").replace('/', std::path::MAIN_SEPARATOR_STR);
    if cfg!(windows) { PathBuf::from(text.to_lowercase()) } else { PathBuf::from(text) }
}

/// A cancel handle: `duckdb_interrupt` on the connection, safe from any thread; a no-op once
/// the connection is gone.
#[derive(Clone)]
pub struct Interrupter {
    api: Api,
    conn: Arc<AtomicPtr<c_void>>,
}

// SAFETY: duckdb_interrupt only flags the running query.
unsafe impl Send for Interrupter {}
unsafe impl Sync for Interrupter {}

impl Interrupter {
    pub fn interrupt(&self) {
        let c = self.conn.load(Ordering::SeqCst);
        if !c.is_null() {
            unsafe { (self.api.interrupt)(c) };
        }
    }
}

/// What a statement produces, in order.
#[derive(Debug)]
pub enum Item {
    /// A result set begins with its Arrow schema (DuckDB's own Arrow types).
    Start { schema: SchemaRef },
    Batch(RecordBatch),
    End { rows: u64 },
    /// An INSERT / UPDATE / DELETE.
    Changed(u64),
    /// A statement with nothing to show (DDL, SET, …).
    Nothing,
    /// The statement failed (or was interrupted); the script stops here.
    Error { message: String, interrupted: bool },
}

pub struct Connection {
    db: Arc<Database>,
    conn: Arc<AtomicPtr<c_void>>,
    arrow_options: api::ArrowOptions,
    /// Arrow streams registered as views; alive until the view is dropped.
    streams: Vec<(String, Box<FFI_ArrowArrayStream>)>,
}

// SAFETY: a duckdb_connection moves between threads; Cobalt uses it from one at a time.
unsafe impl Send for Connection {}

impl Drop for Connection {
    fn drop(&mut self) {
        let api = &self.db.loaded.api;
        let c = self.conn.swap(std::ptr::null_mut(), Ordering::SeqCst);
        unsafe {
            if !self.arrow_options.is_null() {
                (api.destroy_arrow_options)(&mut self.arrow_options);
            }
            if !c.is_null() {
                let mut c = c;
                (api.disconnect)(&mut c);
            }
        }
    }
}

impl Connection {
    pub fn new(db: &Arc<Database>) -> std::result::Result<Self, String> {
        let api = &db.loaded.api;
        let mut c: api::Connection = std::ptr::null_mut();
        unsafe {
            if (api.connect)(db.handle, &mut c) != SUCCESS {
                return Err(format!("could not connect to {}", db.path.display()));
            }
            let mut opts: api::ArrowOptions = std::ptr::null_mut();
            (api.connection_get_arrow_options)(c, &mut opts);
            Ok(Self { db: Arc::clone(db), conn: Arc::new(AtomicPtr::new(c)), arrow_options: opts, streams: Vec::new() })
        }
    }

    pub fn database(&self) -> &Arc<Database> {
        &self.db
    }

    pub fn version(&self) -> &str {
        &self.db.loaded.version
    }

    pub fn interrupter(&self) -> Interrupter {
        Interrupter { api: self.db.loaded.api, conn: Arc::clone(&self.conn) }
    }

    fn raw(&self) -> api::Connection {
        self.conn.load(Ordering::SeqCst)
    }

    /// Run a script statement by statement, streaming each result set's batches to `sink`
    /// (returning `false` stops: the query is interrupted and the script ends). An error ends
    /// the script after its `Item::Error`.
    pub fn run(&self, sql: &str, sink: &mut dyn FnMut(Item) -> bool) {
        let api = &self.db.loaded.api;
        let conn = self.raw();
        let Ok(csql) = CString::new(sql) else {
            sink(Item::Error { message: "the script contains a NUL character".into(), interrupted: false });
            return;
        };
        unsafe {
            let mut ext: api::Extracted = std::ptr::null_mut();
            let n = (api.extract_statements)(conn, csql.as_ptr(), &mut ext);
            if n == 0 {
                let e = api::text((api.extract_statements_error)(ext));
                (api.destroy_extracted)(&mut ext);
                if !e.is_empty() {
                    sink(Item::Error { message: e, interrupted: false });
                }
                return;
            }
            for i in 0..n {
                let mut st: api::Prepared = std::ptr::null_mut();
                if (api.prepare_extracted_statement)(conn, ext, i, &mut st) != SUCCESS {
                    let e = api::text((api.prepare_error)(st));
                    (api.destroy_prepare)(&mut st);
                    sink(Item::Error { message: e, interrupted: false });
                    break;
                }
                let mut pend: api::Pending = std::ptr::null_mut();
                if (api.pending_prepared_streaming)(st, &mut pend) != SUCCESS {
                    let e = api::text((api.pending_error)(pend));
                    (api.destroy_pending)(&mut pend);
                    (api.destroy_prepare)(&mut st);
                    sink(Item::Error { message: e, interrupted: false });
                    break;
                }
                let mut res = api::Result::zeroed();
                if (api.execute_pending)(pend, &mut res) != SUCCESS {
                    let e = api::text((api.result_error)(&mut res));
                    let interrupted = (api.result_error_type)(&mut res) == api::ERROR_INTERRUPT || e.contains("INTERRUPT");
                    (api.destroy_result)(&mut res);
                    (api.destroy_pending)(&mut pend);
                    (api.destroy_prepare)(&mut st);
                    sink(Item::Error { message: e, interrupted });
                    break;
                }
                let stop = match (api.result_return_type)(res) {
                    api::RESULT_CHANGED_ROWS => !sink(Item::Changed((api.rows_changed)(&mut res))),
                    api::RESULT_QUERY_RESULT => self.stream(&mut res, sink),
                    _ => !sink(Item::Nothing),
                };
                (api.destroy_result)(&mut res);
                (api.destroy_pending)(&mut pend);
                (api.destroy_prepare)(&mut st);
                if stop {
                    break;
                }
            }
            (api.destroy_extracted)(&mut ext);
        }
    }

    /// Stream one result: schema, chunks as batches, end. Returns true when the script should stop.
    unsafe fn stream(&self, res: &mut api::Result, sink: &mut dyn FnMut(Item) -> bool) -> bool {
        let api = &self.db.loaded.api;
        let ncol = (api.column_count)(res);
        let mut types: Vec<api::LogicalType> = (0..ncol).map(|c| (api.column_logical_type)(res, c)).collect();
        let names: Vec<CString> = (0..ncol).map(|c| CString::new(api::text((api.column_name)(res, c))).unwrap_or_default()).collect();
        let mut name_ptrs: Vec<*const c_char> = names.iter().map(|n| n.as_ptr()).collect();
        let mut ffi_schema = FFI_ArrowSchema::empty();
        let err = (api.to_arrow_schema)(self.arrow_options, types.as_mut_ptr(), name_ptrs.as_mut_ptr(), ncol, &mut ffi_schema as *mut FFI_ArrowSchema as *mut c_void);
        let schema_err = self.take_error(err);
        for t in types.iter_mut() {
            (api.destroy_logical_type)(t);
        }
        if let Some(e) = schema_err {
            sink(Item::Error { message: format!("Arrow schema: {e}"), interrupted: false });
            return true;
        }
        let schema = match Schema::try_from(&ffi_schema) {
            Ok(s) => Arc::new(s),
            Err(e) => {
                sink(Item::Error { message: format!("Arrow schema: {e}"), interrupted: false });
                return true;
            }
        };
        if !sink(Item::Start { schema }) {
            return true;
        }
        let mut rows = 0u64;
        let mut stop = false;
        loop {
            let mut chunk = (api.fetch_chunk)(*res);
            if chunk.is_null() {
                break;
            }
            let size = (api.data_chunk_get_size)(chunk);
            if size == 0 {
                (api.destroy_data_chunk)(&mut chunk);
                break;
            }
            let mut arr = FFI_ArrowArray::empty();
            let err = (api.data_chunk_to_arrow)(self.arrow_options, chunk, &mut arr as *mut FFI_ArrowArray as *mut c_void);
            (api.destroy_data_chunk)(&mut chunk);
            if let Some(e) = self.take_error(err) {
                sink(Item::Error { message: format!("Arrow conversion: {e}"), interrupted: false });
                return true;
            }
            let batch = match arrow::ffi::from_ffi(arr, &ffi_schema) {
                Ok(data) => RecordBatch::from(StructArray::from(data)),
                Err(e) => {
                    sink(Item::Error { message: format!("Arrow import: {e}"), interrupted: false });
                    return true;
                }
            };
            rows += batch.num_rows() as u64;
            if !sink(Item::Batch(batch)) {
                self.interrupter().interrupt();
                stop = true;
                break;
            }
        }
        // a streaming result fails after it started (an interrupt, a cast on a late row): the
        // chunks stop and the result carries the error
        let e = api::text((api.result_error)(res));
        if !e.is_empty() && !stop {
            let interrupted = (api.result_error_type)(res) == api::ERROR_INTERRUPT || e.contains("INTERRUPT");
            sink(Item::Error { message: e, interrupted });
            return true;
        }
        sink(Item::End { rows });
        stop
    }

    unsafe fn take_error(&self, err: api::ErrorData) -> Option<String> {
        let api = &self.db.loaded.api;
        if err.is_null() {
            return None;
        }
        let has = (api.error_data_has_error)(err);
        let msg = if has { api::text((api.error_data_message)(err)) } else { String::new() };
        let mut e = err;
        (api.destroy_error_data)(&mut e);
        has.then_some(msg)
    }

    /// Run a query and collect every batch of its (last) result set.
    pub fn query(&self, sql: &str) -> std::result::Result<(SchemaRef, Vec<RecordBatch>), String> {
        let mut schema: Option<SchemaRef> = None;
        let mut batches = Vec::new();
        let mut error: Option<String> = None;
        self.run(sql, &mut |item| {
            match item {
                Item::Start { schema: s } => {
                    schema = Some(s);
                    batches.clear();
                }
                Item::Batch(b) => batches.push(b),
                Item::Error { message, .. } => error = Some(message),
                _ => {}
            }
            true
        });
        if let Some(e) = error {
            return Err(e);
        }
        let schema = schema.ok_or_else(|| "the statement returned no result set".to_string())?;
        Ok((schema, batches))
    }

    /// Run statements for their effect; the rows changed by the last one that changed any.
    pub fn execute(&self, sql: &str) -> std::result::Result<u64, String> {
        let mut changed = 0u64;
        let mut error: Option<String> = None;
        self.run(sql, &mut |item| {
            match item {
                Item::Changed(n) => changed = n,
                Item::Error { message, .. } => error = Some(message),
                _ => {}
            }
            true
        });
        match error {
            Some(e) => Err(e),
            None => Ok(changed),
        }
    }

    /// Register a stream of Arrow batches as a view named `name` (consumed by one scan); the
    /// stream lives until [`Connection::drop_arrow_view`] or the connection ends.
    pub fn register_arrow_view(&mut self, name: &str, reader: Box<dyn RecordBatchReader + Send>) -> std::result::Result<(), String> {
        let api = &self.db.loaded.api;
        let mut stream = Box::new(FFI_ArrowArrayStream::new(reader));
        let cname = CString::new(name).map_err(|e| e.to_string())?;
        let st = unsafe { (api.arrow_scan)(self.raw(), cname.as_ptr(), &mut *stream as *mut FFI_ArrowArrayStream as *mut c_void) };
        if st != SUCCESS {
            return Err(format!("could not register the Arrow stream as {name}"));
        }
        self.streams.push((name.to_string(), stream));
        Ok(())
    }

    pub fn drop_arrow_view(&mut self, name: &str) {
        let _ = self.execute(&format!("DROP VIEW IF EXISTS {}", quote(name)));
        self.streams.retain(|(n, _)| n != name);
    }
}

/// `"name"` with inner quotes doubled.
pub fn quote(name: &str) -> String {
    format!("\"{}\"", name.replace('"', "\"\""))
}

/// `'text'` with inner quotes doubled.
pub fn lit(s: &str) -> String {
    format!("'{}'", s.replace('\'', "''"))
}

#[cfg(test)]
mod tests {
    use super::*;

    pub(crate) fn test_root() -> Option<PathBuf> {
        root()
    }

    /// Needs the network once (`COBALT_DUCKDB_TEST=1`): the library is cached under the temp dir.
    fn root() -> Option<PathBuf> {
        if std::env::var("COBALT_DUCKDB_TEST").is_err() {
            return None;
        }
        static INSTALL: std::sync::Mutex<()> = std::sync::Mutex::new(());
        let _g = INSTALL.lock().unwrap();
        let root = std::env::temp_dir().join("cobalt-duckdb-test");
        if !is_installed(&root) {
            install(&root, &mut |_, _| {}).expect("download libduckdb");
        }
        Some(root)
    }

    #[test]
    fn types_statements_errors_and_interrupt() {
        let Some(root) = root() else { return };
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("t.duckdb");
        let db = open(&root, &path, false).unwrap();
        let conn = Connection::new(&db).unwrap();
        assert!(conn.version().contains(VERSION), "{}", conn.version());
        assert_eq!(conn.execute("CREATE TABLE t (id INTEGER, amount DECIMAL(10,2), name VARCHAR, day DATE, \"at\" TIMESTAMP, tags VARCHAR[]); INSERT INTO t VALUES (1, 12.34, 'a', DATE '2024-01-01', TIMESTAMP '2024-01-01 10:00:00', ['x','y']), (2, NULL, NULL, NULL, NULL, NULL)").unwrap(), 2);
        let (schema, batches) = conn.query("SELECT * FROM t ORDER BY id").unwrap();
        assert_eq!(schema.fields().len(), 6);
        assert_eq!(batches.iter().map(|b| b.num_rows()).sum::<usize>(), 2);
        assert!(matches!(schema.field(1).data_type(), arrow::datatypes::DataType::Decimal128(10, 2)));
        assert!(matches!(schema.field(5).data_type(), arrow::datatypes::DataType::List(_)));
        drop(conn);
        drop(db);
        // the file is a DuckDB file; a second open shares the instance
        assert!(is_duckdb_file(&path));
        let db = open(&root, &path, true).unwrap();
        assert!(open(&root, &path, false).unwrap_err().contains("read-only"), "a writable open of a read-only instance is refused");
        let db2 = open(&root, &path, true).unwrap();
        assert!(Arc::ptr_eq(&db, &db2));
        let conn = Connection::new(&db).unwrap();
        // items in order over a script, stopping at the error
        let mut items = Vec::new();
        conn.run("SELECT 1 AS one; SELECT * FROM nope; SELECT 2", &mut |i| {
            items.push(format!("{i:?}").chars().take(40).collect::<String>());
            true
        });
        assert!(items[0].starts_with("Start"), "{items:?}");
        assert!(items[1].starts_with("Batch"), "{items:?}");
        assert!(items[2].starts_with("End"), "{items:?}");
        assert!(items[3].starts_with("Error"), "{items:?}");
        assert_eq!(items.len(), 4, "{items:?}");
        // an interrupt from another thread ends a long query
        let int = conn.interrupter();
        let t = std::thread::spawn(move || {
            std::thread::sleep(std::time::Duration::from_millis(300));
            int.interrupt();
        });
        let started = std::time::Instant::now();
        let mut err = None;
        conn.run("SELECT count(*) FROM range(5000000000)", &mut |i| {
            if let Item::Error { message, interrupted } = i {
                err = Some((message, interrupted));
            }
            true
        });
        t.join().unwrap();
        let (m, interrupted) = err.expect("the query was interrupted");
        assert!(interrupted, "{m}");
        assert!(started.elapsed() < std::time::Duration::from_secs(20));
        // large results stream in chunks
        let mut n = 0usize;
        let mut rows = 0u64;
        conn.run("SELECT range AS i, 'x' || range AS s FROM range(100000)", &mut |i| {
            match i {
                Item::Batch(b) => n += usize::from(b.num_rows() > 0),
                Item::End { rows: r } => rows = r,
                _ => {}
            }
            true
        });
        assert_eq!(rows, 100000);
        assert!(n > 1);
    }

    #[test]
    fn registry_keys_agree_before_and_after_the_file_exists() {
        let dir = tempfile::tempdir().unwrap();
        let p = dir.path().join("k.duckdb");
        let before = registry_key(&p);
        let slashes = PathBuf::from(p.to_string_lossy().replace('\\', "/"));
        assert_eq!(before, registry_key(&slashes));
        std::fs::write(&p, b"x").unwrap();
        assert_eq!(before, registry_key(&p));
        assert_eq!(before, registry_key(&slashes));
    }

    #[test]
    fn arrow_view_feeds_an_insert() {
        let Some(root) = root() else { return };
        use arrow::array::{Int64Array, StringArray};
        use arrow::datatypes::{DataType, Field};
        let dir = tempfile::tempdir().unwrap();
        let db = open(&root, &dir.path().join("v.duckdb"), false).unwrap();
        let mut conn = Connection::new(&db).unwrap();
        let schema = Arc::new(Schema::new(vec![Field::new("id", DataType::Int64, false), Field::new("name", DataType::Utf8, true)]));
        let batch = RecordBatch::try_new(schema.clone(), vec![Arc::new(Int64Array::from(vec![1, 2, 3])), Arc::new(StringArray::from(vec![Some("a"), None, Some("c")]))]).unwrap();
        let reader = arrow::array::RecordBatchIterator::new(vec![Ok(batch)], schema);
        conn.register_arrow_view("__in", Box::new(reader)).unwrap();
        conn.execute("CREATE TABLE t AS SELECT * FROM \"__in\"").unwrap();
        conn.drop_arrow_view("__in");
        let (_, b) = conn.query("SELECT count(*) AS n, count(name) AS names FROM t").unwrap();
        let n = b[0].column(0).as_any().downcast_ref::<Int64Array>().unwrap().value(0);
        assert_eq!(n, 3);
    }
}


