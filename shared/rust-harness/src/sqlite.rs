//! Minimal binding to the system libsqlite3 (always present on macOS).
//! Only what the job store needs: open, exec, prepared statements, typed columns.
use std::ffi::{c_char, c_int, c_void, CStr, CString};
use std::path::Path;
use std::ptr;

#[repr(C)]
struct RawDb {
    _p: [u8; 0],
}
#[repr(C)]
struct RawStmt {
    _p: [u8; 0],
}

const SQLITE_OK: c_int = 0;
const SQLITE_ROW: c_int = 100;
const SQLITE_DONE: c_int = 101;
const OPEN_READWRITE: c_int = 0x02;
const OPEN_CREATE: c_int = 0x04;
const OPEN_FULLMUTEX: c_int = 0x10000;
const TRANSIENT: isize = -1;

#[link(name = "sqlite3")]
extern "C" {
    fn sqlite3_open_v2(
        f: *const c_char,
        db: *mut *mut RawDb,
        flags: c_int,
        vfs: *const c_char,
    ) -> c_int;
    fn sqlite3_close_v2(db: *mut RawDb) -> c_int;
    fn sqlite3_busy_timeout(db: *mut RawDb, ms: c_int) -> c_int;
    fn sqlite3_exec(
        db: *mut RawDb,
        sql: *const c_char,
        cb: *const c_void,
        arg: *mut c_void,
        err: *mut *mut c_char,
    ) -> c_int;
    fn sqlite3_free(p: *mut c_void);
    fn sqlite3_errmsg(db: *mut RawDb) -> *const c_char;
    fn sqlite3_prepare_v2(
        db: *mut RawDb,
        sql: *const c_char,
        n: c_int,
        st: *mut *mut RawStmt,
        tail: *mut *const c_char,
    ) -> c_int;
    fn sqlite3_bind_text(st: *mut RawStmt, i: c_int, v: *const c_char, n: c_int, d: isize)
        -> c_int;
    fn sqlite3_bind_double(st: *mut RawStmt, i: c_int, v: f64) -> c_int;
    fn sqlite3_bind_int64(st: *mut RawStmt, i: c_int, v: i64) -> c_int;
    fn sqlite3_bind_null(st: *mut RawStmt, i: c_int) -> c_int;
    fn sqlite3_step(st: *mut RawStmt) -> c_int;
    fn sqlite3_column_count(st: *mut RawStmt) -> c_int;
    fn sqlite3_column_name(st: *mut RawStmt, i: c_int) -> *const c_char;
    fn sqlite3_column_type(st: *mut RawStmt, i: c_int) -> c_int;
    fn sqlite3_column_text(st: *mut RawStmt, i: c_int) -> *const u8;
    fn sqlite3_column_bytes(st: *mut RawStmt, i: c_int) -> c_int;
    fn sqlite3_column_double(st: *mut RawStmt, i: c_int) -> f64;
    fn sqlite3_column_int64(st: *mut RawStmt, i: c_int) -> i64;
    fn sqlite3_finalize(st: *mut RawStmt) -> c_int;
}

#[derive(Debug, Clone, PartialEq)]
pub enum Value {
    Null,
    Int(i64),
    Real(f64),
    Text(String),
}

impl Value {
    pub fn as_str(&self) -> Option<&str> {
        if let Value::Text(s) = self {
            Some(s)
        } else {
            None
        }
    }
    pub fn as_f64(&self) -> Option<f64> {
        match self {
            Value::Real(v) => Some(*v),
            Value::Int(v) => Some(*v as f64),
            _ => None,
        }
    }
    pub fn as_i64(&self) -> Option<i64> {
        match self {
            Value::Int(v) => Some(*v),
            Value::Real(v) => Some(*v as i64),
            _ => None,
        }
    }
}

pub type Row = Vec<(String, Value)>;

pub fn col<'a>(row: &'a Row, name: &str) -> &'a Value {
    row.iter()
        .find(|(k, _)| k == name)
        .map(|(_, v)| v)
        .unwrap_or(&Value::Null)
}

pub struct Db {
    raw: *mut RawDb,
}

unsafe impl Send for Db {}

impl Db {
    pub fn open(path: &Path, busy_ms: i32) -> Result<Db, String> {
        let c = CString::new(path.to_string_lossy().as_bytes()).map_err(|e| e.to_string())?;
        let mut raw = ptr::null_mut();
        let rc = unsafe {
            sqlite3_open_v2(
                c.as_ptr(),
                &mut raw,
                OPEN_READWRITE | OPEN_CREATE | OPEN_FULLMUTEX,
                ptr::null(),
            )
        };
        let db = Db { raw };
        if rc != SQLITE_OK {
            return Err(format!("sqlite open failed: {}", db.err()));
        }
        unsafe { sqlite3_busy_timeout(raw, busy_ms) };
        Ok(db)
    }

    fn err(&self) -> String {
        if self.raw.is_null() {
            return "out of memory".into();
        }
        unsafe {
            CStr::from_ptr(sqlite3_errmsg(self.raw))
                .to_string_lossy()
                .into_owned()
        }
    }

    pub fn exec(&self, sql: &str) -> Result<(), String> {
        let c = CString::new(sql).map_err(|e| e.to_string())?;
        let mut msg: *mut c_char = ptr::null_mut();
        let rc =
            unsafe { sqlite3_exec(self.raw, c.as_ptr(), ptr::null(), ptr::null_mut(), &mut msg) };
        if rc != SQLITE_OK {
            let text = if msg.is_null() {
                self.err()
            } else {
                let t = unsafe { CStr::from_ptr(msg).to_string_lossy().into_owned() };
                unsafe { sqlite3_free(msg as *mut c_void) };
                t
            };
            return Err(format!("sqlite: {text}"));
        }
        Ok(())
    }

    /// Run one statement; returns all rows (empty for writes).
    pub fn query(&self, sql: &str, params: &[Value]) -> Result<Vec<Row>, String> {
        let c = CString::new(sql).map_err(|e| e.to_string())?;
        let mut st = ptr::null_mut();
        if unsafe { sqlite3_prepare_v2(self.raw, c.as_ptr(), -1, &mut st, ptr::null_mut()) }
            != SQLITE_OK
        {
            return Err(format!("sqlite prepare: {}", self.err()));
        }
        let result = self.run(st, params);
        unsafe { sqlite3_finalize(st) };
        result
    }

    fn run(&self, st: *mut RawStmt, params: &[Value]) -> Result<Vec<Row>, String> {
        let mut keep = Vec::new();
        for (i, p) in params.iter().enumerate() {
            let idx = (i + 1) as c_int;
            let rc = unsafe {
                match p {
                    Value::Null => sqlite3_bind_null(st, idx),
                    Value::Int(v) => sqlite3_bind_int64(st, idx, *v),
                    Value::Real(v) => sqlite3_bind_double(st, idx, *v),
                    Value::Text(s) => {
                        let c = CString::new(s.as_str()).map_err(|e| e.to_string())?;
                        let rc = sqlite3_bind_text(st, idx, c.as_ptr(), -1, TRANSIENT);
                        keep.push(c);
                        rc
                    }
                }
            };
            if rc != SQLITE_OK {
                return Err(format!("sqlite bind: {}", self.err()));
            }
        }
        let mut rows = Vec::new();
        loop {
            match unsafe { sqlite3_step(st) } {
                SQLITE_ROW => {
                    let n = unsafe { sqlite3_column_count(st) };
                    let mut row = Vec::with_capacity(n as usize);
                    for i in 0..n {
                        let name = unsafe {
                            CStr::from_ptr(sqlite3_column_name(st, i))
                                .to_string_lossy()
                                .into_owned()
                        };
                        let v = unsafe {
                            match sqlite3_column_type(st, i) {
                                1 => Value::Int(sqlite3_column_int64(st, i)),
                                2 => Value::Real(sqlite3_column_double(st, i)),
                                5 => Value::Null,
                                _ => {
                                    let p = sqlite3_column_text(st, i);
                                    let len = sqlite3_column_bytes(st, i) as usize;
                                    if p.is_null() {
                                        Value::Null
                                    } else {
                                        Value::Text(
                                            String::from_utf8_lossy(std::slice::from_raw_parts(
                                                p, len,
                                            ))
                                            .into_owned(),
                                        )
                                    }
                                }
                            }
                        };
                        row.push((name, v));
                    }
                    rows.push(row);
                }
                SQLITE_DONE => return Ok(rows),
                _ => return Err(format!("sqlite step: {}", self.err())),
            }
        }
    }

    /// BEGIN IMMEDIATE ... COMMIT, rolling back on error.
    pub fn immediate<T>(&self, f: impl FnOnce(&Db) -> Result<T, String>) -> Result<T, String> {
        self.exec("BEGIN IMMEDIATE")?;
        match f(self) {
            Ok(v) => {
                self.exec("COMMIT")?;
                Ok(v)
            }
            Err(e) => {
                let _ = self.exec("ROLLBACK");
                Err(e)
            }
        }
    }
}

impl Drop for Db {
    fn drop(&mut self) {
        unsafe { sqlite3_close_v2(self.raw) };
    }
}
