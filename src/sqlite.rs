//! Liaison minimale avec la bibliothèque SQLite du système (libsqlite3),
//! déclarée à la main : pas de dépendance Rust externe.
//! Compilation : paquet `libsqlite3-dev` (ou équivalent) ; exécution : libsqlite3.so.0.

#![allow(non_camel_case_types)]

use std::ffi::{c_char, c_int, c_void, CStr, CString};
use std::path::Path;

#[repr(C)]
pub struct sqlite3 {
    _p: [u8; 0],
}
#[repr(C)]
pub struct sqlite3_stmt {
    _p: [u8; 0],
}

const SQLITE_OK: c_int = 0;
const SQLITE_ROW: c_int = 100;
const SQLITE_DONE: c_int = 101;
const SQLITE_NULL: c_int = 5;
const SQLITE_OPEN_READWRITE: c_int = 0x02;
const SQLITE_OPEN_CREATE: c_int = 0x04;
const SQLITE_OPEN_FULLMUTEX: c_int = 0x10000;
/// SQLITE_TRANSIENT : SQLite copie la valeur liée.
fn transient() -> isize {
    -1
}

#[link(name = "sqlite3")]
extern "C" {
    fn sqlite3_open_v2(filename: *const c_char, db: *mut *mut sqlite3, flags: c_int, vfs: *const c_char) -> c_int;
    fn sqlite3_close_v2(db: *mut sqlite3) -> c_int;
    fn sqlite3_errmsg(db: *mut sqlite3) -> *const c_char;
    fn sqlite3_exec(db: *mut sqlite3, sql: *const c_char, cb: *const c_void, arg: *mut c_void, err: *mut *mut c_char) -> c_int;
    fn sqlite3_free(p: *mut c_void);
    fn sqlite3_busy_timeout(db: *mut sqlite3, ms: c_int) -> c_int;
    fn sqlite3_prepare_v2(db: *mut sqlite3, sql: *const c_char, n: c_int, stmt: *mut *mut sqlite3_stmt, tail: *mut *const c_char) -> c_int;
    fn sqlite3_finalize(stmt: *mut sqlite3_stmt) -> c_int;
    fn sqlite3_step(stmt: *mut sqlite3_stmt) -> c_int;
    fn sqlite3_bind_text(stmt: *mut sqlite3_stmt, i: c_int, v: *const c_char, n: c_int, d: isize) -> c_int;
    fn sqlite3_bind_int64(stmt: *mut sqlite3_stmt, i: c_int, v: i64) -> c_int;
    fn sqlite3_bind_null(stmt: *mut sqlite3_stmt, i: c_int) -> c_int;
    fn sqlite3_column_count(stmt: *mut sqlite3_stmt) -> c_int;
    fn sqlite3_column_type(stmt: *mut sqlite3_stmt, i: c_int) -> c_int;
    fn sqlite3_column_int64(stmt: *mut sqlite3_stmt, i: c_int) -> i64;
    fn sqlite3_column_text(stmt: *mut sqlite3_stmt, i: c_int) -> *const u8;
    fn sqlite3_column_bytes(stmt: *mut sqlite3_stmt, i: c_int) -> c_int;
    fn sqlite3_changes(db: *mut sqlite3) -> c_int;
    fn sqlite3_last_insert_rowid(db: *mut sqlite3) -> i64;
}

#[derive(Debug)]
pub struct Error(pub String);
impl std::fmt::Display for Error {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "SQLite : {}", self.0)
    }
}
impl std::error::Error for Error {}
pub type Result<T> = std::result::Result<T, Error>;

/// Valeur liée ou lue.
#[derive(Clone, Debug, PartialEq)]
pub enum Val {
    Null,
    Int(i64),
    Text(String),
}

impl From<&str> for Val {
    fn from(s: &str) -> Self {
        Val::Text(s.to_string())
    }
}
impl From<String> for Val {
    fn from(s: String) -> Self {
        Val::Text(s)
    }
}
impl From<i64> for Val {
    fn from(i: i64) -> Self {
        Val::Int(i)
    }
}
impl<T: Into<Val>> From<Option<T>> for Val {
    fn from(o: Option<T>) -> Self {
        o.map(Into::into).unwrap_or(Val::Null)
    }
}

impl Val {
    pub fn text(&self) -> Option<String> {
        match self {
            Val::Text(s) => Some(s.clone()),
            Val::Int(i) => Some(i.to_string()),
            Val::Null => None,
        }
    }
    pub fn int(&self) -> Option<i64> {
        match self {
            Val::Int(i) => Some(*i),
            Val::Text(s) => s.parse().ok(),
            Val::Null => None,
        }
    }
}

pub struct Db {
    db: *mut sqlite3,
}

// La connexion est ouverte en mode FULLMUTEX.
unsafe impl Send for Db {}

impl Db {
    pub fn open(path: &Path) -> Result<Db> {
        if let Some(d) = path.parent() {
            let _ = std::fs::create_dir_all(d);
        }
        let c = CString::new(path.to_string_lossy().as_bytes()).map_err(|_| Error("chemin invalide".into()))?;
        let mut db = std::ptr::null_mut();
        let rc = unsafe { sqlite3_open_v2(c.as_ptr(), &mut db, SQLITE_OPEN_READWRITE | SQLITE_OPEN_CREATE | SQLITE_OPEN_FULLMUTEX, std::ptr::null()) };
        let d = Db { db };
        if rc != SQLITE_OK {
            return Err(Error(d.errmsg()));
        }
        unsafe {
            sqlite3_busy_timeout(db, 5000);
        }
        Ok(d)
    }

    pub fn open_memory() -> Result<Db> {
        Self::open(Path::new(":memory:"))
    }

    fn errmsg(&self) -> String {
        if self.db.is_null() {
            return "ouverture impossible".into();
        }
        unsafe { CStr::from_ptr(sqlite3_errmsg(self.db)).to_string_lossy().into_owned() }
    }

    /// Exécute une ou plusieurs instructions sans paramètres.
    pub fn exec(&self, sql: &str) -> Result<()> {
        let c = CString::new(sql).map_err(|_| Error("NUL dans la requête".into()))?;
        let mut err: *mut c_char = std::ptr::null_mut();
        let rc = unsafe { sqlite3_exec(self.db, c.as_ptr(), std::ptr::null(), std::ptr::null_mut(), &mut err) };
        if rc != SQLITE_OK {
            let m = if err.is_null() { self.errmsg() } else { unsafe { CStr::from_ptr(err).to_string_lossy().into_owned() } };
            unsafe { sqlite3_free(err as *mut c_void) };
            return Err(Error(m));
        }
        Ok(())
    }

    fn prepare(&self, sql: &str, params: &[Val]) -> Result<Stmt<'_>> {
        let c = CString::new(sql).map_err(|_| Error("NUL dans la requête".into()))?;
        let mut st = std::ptr::null_mut();
        let rc = unsafe { sqlite3_prepare_v2(self.db, c.as_ptr(), -1, &mut st, std::ptr::null_mut()) };
        if rc != SQLITE_OK {
            return Err(Error(format!("{} ({sql})", self.errmsg())));
        }
        let s = Stmt { db: self, st };
        for (i, p) in params.iter().enumerate() {
            let idx = (i + 1) as c_int;
            let rc = unsafe {
                match p {
                    Val::Null => sqlite3_bind_null(st, idx),
                    Val::Int(v) => sqlite3_bind_int64(st, idx, *v),
                    Val::Text(t) => sqlite3_bind_text(st, idx, t.as_ptr() as *const c_char, t.len() as c_int, transient()),
                }
            };
            if rc != SQLITE_OK {
                return Err(Error(self.errmsg()));
            }
        }
        Ok(s)
    }

    /// Instruction de modification ; renvoie le nombre de lignes touchées.
    pub fn execute(&self, sql: &str, params: &[Val]) -> Result<usize> {
        let s = self.prepare(sql, params)?;
        match unsafe { sqlite3_step(s.st) } {
            SQLITE_DONE | SQLITE_ROW => Ok(unsafe { sqlite3_changes(self.db) } as usize),
            _ => Err(Error(self.errmsg())),
        }
    }

    /// Requête : toutes les lignes, colonnes dans l'ordre du SELECT.
    pub fn query(&self, sql: &str, params: &[Val]) -> Result<Vec<Vec<Val>>> {
        let s = self.prepare(sql, params)?;
        let mut rows = vec![];
        loop {
            match unsafe { sqlite3_step(s.st) } {
                SQLITE_ROW => {
                    let n = unsafe { sqlite3_column_count(s.st) };
                    let mut row = Vec::with_capacity(n as usize);
                    for i in 0..n {
                        row.push(unsafe {
                            match sqlite3_column_type(s.st, i) {
                                SQLITE_NULL => Val::Null,
                                1 => Val::Int(sqlite3_column_int64(s.st, i)),
                                _ => {
                                    let p = sqlite3_column_text(s.st, i);
                                    let len = sqlite3_column_bytes(s.st, i) as usize;
                                    if p.is_null() {
                                        Val::Null
                                    } else {
                                        Val::Text(String::from_utf8_lossy(std::slice::from_raw_parts(p, len)).into_owned())
                                    }
                                }
                            }
                        });
                    }
                    rows.push(row);
                }
                SQLITE_DONE => return Ok(rows),
                _ => return Err(Error(self.errmsg())),
            }
        }
    }

    pub fn last_insert_rowid(&self) -> i64 {
        unsafe { sqlite3_last_insert_rowid(self.db) }
    }

    pub fn user_version(&self) -> i64 {
        self.query("PRAGMA user_version", &[]).ok().and_then(|r| r.first().and_then(|x| x[0].int())).unwrap_or(0)
    }
}

impl Drop for Db {
    fn drop(&mut self) {
        if !self.db.is_null() {
            unsafe {
                sqlite3_close_v2(self.db);
            }
        }
    }
}

struct Stmt<'a> {
    #[allow(dead_code)]
    db: &'a Db,
    st: *mut sqlite3_stmt,
}

impl Drop for Stmt<'_> {
    fn drop(&mut self) {
        unsafe {
            sqlite3_finalize(self.st);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn roundtrip() {
        let db = Db::open_memory().unwrap();
        db.exec("CREATE TABLE t(a INTEGER, b TEXT); PRAGMA user_version = 3;").unwrap();
        assert_eq!(db.execute("INSERT INTO t VALUES (?, ?)", &[Val::Int(1), "é'\"x".into()]).unwrap(), 1);
        db.execute("INSERT INTO t VALUES (?, ?)", &[Val::Int(2), Val::Null]).unwrap();
        let r = db.query("SELECT a, b FROM t ORDER BY a", &[]).unwrap();
        assert_eq!(r[0], vec![Val::Int(1), Val::Text("é'\"x".into())]);
        assert_eq!(r[1][1], Val::Null);
        assert_eq!(db.user_version(), 3);
        assert!(db.exec("SELEC nope").is_err());
    }
}
