//! The slice of DuckDB's C API Cobalt uses, resolved by name from the shared library at run
//! time (no link-time dependency, no C++ build). Signatures follow `duckdb.h` of the pinned
//! version; handles are opaque pointers, `duckdb_result` is the six-word struct the header
//! declares, passed by value where the header does.

#![allow(non_camel_case_types, clippy::upper_case_acronyms)]

use std::os::raw::{c_char, c_void};

pub type idx_t = u64;
/// `duckdb_state`: 0 = success, 1 = error.
pub type State = i32;
pub const SUCCESS: State = 0;

pub type Database = *mut c_void;
pub type Connection = *mut c_void;
pub type Config = *mut c_void;
pub type Prepared = *mut c_void;
pub type Pending = *mut c_void;
pub type Extracted = *mut c_void;
pub type Chunk = *mut c_void;
pub type LogicalType = *mut c_void;
pub type ErrorData = *mut c_void;
pub type ArrowOptions = *mut c_void;

/// `duckdb_result`: deprecated counters and pointers plus the internal data; allocated by the
/// caller, filled by the library, destroyed with `duckdb_destroy_result`.
#[repr(C)]
#[derive(Clone, Copy)]
pub struct Result {
    pub deprecated_column_count: idx_t,
    pub deprecated_row_count: idx_t,
    pub deprecated_rows_changed: idx_t,
    pub deprecated_columns: *mut c_void,
    pub deprecated_error_message: *mut c_char,
    pub internal_data: *mut c_void,
}

impl Result {
    pub fn zeroed() -> Self {
        Self { deprecated_column_count: 0, deprecated_row_count: 0, deprecated_rows_changed: 0, deprecated_columns: std::ptr::null_mut(), deprecated_error_message: std::ptr::null_mut(), internal_data: std::ptr::null_mut() }
    }
}

/// `duckdb_result_type`
pub const RESULT_CHANGED_ROWS: i32 = 1;
pub const RESULT_NOTHING: i32 = 2;
pub const RESULT_QUERY_RESULT: i32 = 3;
/// `duckdb_error_type::DUCKDB_ERROR_INTERRUPT`
pub const ERROR_INTERRUPT: i32 = 29;

macro_rules! api {
    ($( $name:ident : $sym:literal => fn($($arg:ty),*) $(-> $ret:ty)? ; )*) => {
        /// Function pointers resolved from the library.
        #[derive(Clone, Copy)]
        pub struct Api {
            $( pub $name: unsafe extern "C" fn($($arg),*) $(-> $ret)?, )*
        }
        impl Api {
            /// Resolve every symbol; a missing one names itself (the library is the wrong version).
            pub fn load(lib: &libloading::Library) -> std::result::Result<Self, String> {
                unsafe {
                    Ok(Self {
                        $( $name: *lib.get::<unsafe extern "C" fn($($arg),*) $(-> $ret)?>(concat!($sym, "\0").as_bytes()).map_err(|e| format!("{}: {e}", $sym))?, )*
                    })
                }
            }
        }
    };
}

api! {
    library_version: "duckdb_library_version" => fn() -> *const c_char;
    free: "duckdb_free" => fn(*mut c_void);
    create_config: "duckdb_create_config" => fn(*mut Config) -> State;
    set_config: "duckdb_set_config" => fn(Config, *const c_char, *const c_char) -> State;
    destroy_config: "duckdb_destroy_config" => fn(*mut Config);
    open_ext: "duckdb_open_ext" => fn(*const c_char, *mut Database, Config, *mut *mut c_char) -> State;
    close: "duckdb_close" => fn(*mut Database);
    connect: "duckdb_connect" => fn(Database, *mut Connection) -> State;
    disconnect: "duckdb_disconnect" => fn(*mut Connection);
    interrupt: "duckdb_interrupt" => fn(Connection);
    extract_statements: "duckdb_extract_statements" => fn(Connection, *const c_char, *mut Extracted) -> idx_t;
    extract_statements_error: "duckdb_extract_statements_error" => fn(Extracted) -> *const c_char;
    prepare_extracted_statement: "duckdb_prepare_extracted_statement" => fn(Connection, Extracted, idx_t, *mut Prepared) -> State;
    destroy_extracted: "duckdb_destroy_extracted" => fn(*mut Extracted);
    prepare_error: "duckdb_prepare_error" => fn(Prepared) -> *const c_char;
    destroy_prepare: "duckdb_destroy_prepare" => fn(*mut Prepared);
    pending_prepared_streaming: "duckdb_pending_prepared_streaming" => fn(Prepared, *mut Pending) -> State;
    pending_error: "duckdb_pending_error" => fn(Pending) -> *const c_char;
    execute_pending: "duckdb_execute_pending" => fn(Pending, *mut Result) -> State;
    destroy_pending: "duckdb_destroy_pending" => fn(*mut Pending);
    query: "duckdb_query" => fn(Connection, *const c_char, *mut Result) -> State;
    destroy_result: "duckdb_destroy_result" => fn(*mut Result);
    result_error: "duckdb_result_error" => fn(*mut Result) -> *const c_char;
    result_error_type: "duckdb_result_error_type" => fn(*mut Result) -> i32;
    result_return_type: "duckdb_result_return_type" => fn(Result) -> i32;
    column_count: "duckdb_column_count" => fn(*mut Result) -> idx_t;
    column_name: "duckdb_column_name" => fn(*mut Result, idx_t) -> *const c_char;
    column_logical_type: "duckdb_column_logical_type" => fn(*mut Result, idx_t) -> LogicalType;
    destroy_logical_type: "duckdb_destroy_logical_type" => fn(*mut LogicalType);
    rows_changed: "duckdb_rows_changed" => fn(*mut Result) -> idx_t;
    fetch_chunk: "duckdb_fetch_chunk" => fn(Result) -> Chunk;
    data_chunk_get_size: "duckdb_data_chunk_get_size" => fn(Chunk) -> idx_t;
    destroy_data_chunk: "duckdb_destroy_data_chunk" => fn(*mut Chunk);
    connection_get_arrow_options: "duckdb_connection_get_arrow_options" => fn(Connection, *mut ArrowOptions);
    destroy_arrow_options: "duckdb_destroy_arrow_options" => fn(*mut ArrowOptions);
    to_arrow_schema: "duckdb_to_arrow_schema" => fn(ArrowOptions, *mut LogicalType, *mut *const c_char, idx_t, *mut c_void) -> ErrorData;
    data_chunk_to_arrow: "duckdb_data_chunk_to_arrow" => fn(ArrowOptions, Chunk, *mut c_void) -> ErrorData;
    error_data_has_error: "duckdb_error_data_has_error" => fn(ErrorData) -> bool;
    error_data_message: "duckdb_error_data_message" => fn(ErrorData) -> *const c_char;
    destroy_error_data: "duckdb_destroy_error_data" => fn(*mut ErrorData);
    arrow_scan: "duckdb_arrow_scan" => fn(Connection, *const c_char, *mut c_void) -> State;
}

/// A C string the library owns, copied out (empty for null).
///
/// # Safety
/// `p` is null or points at a NUL-terminated string that stays valid for the call.
pub unsafe fn text(p: *const c_char) -> String {
    if p.is_null() {
        String::new()
    } else {
        std::ffi::CStr::from_ptr(p).to_string_lossy().into_owned()
    }
}
