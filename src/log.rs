//! Journalisation interne : logfmt, rotation par taille, copie stderr et syslog.
//! Aucune dépendance à journald : la copie syslog (`/dev/log`) est reçue par
//! journald, rsyslog, syslog-ng ou le syslogd de BusyBox.

use std::fmt::Write as _;
use std::fs::{File, OpenOptions};
use std::io::Write;
use std::os::unix::net::UnixDatagram;
use std::path::{Path, PathBuf};
use std::sync::{Mutex, OnceLock};
use std::time::{SystemTime, UNIX_EPOCH};

#[derive(Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Debug)]
pub enum Level {
    Error = 0,
    Warn = 1,
    Info = 2,
    Debug = 3,
    Trace = 4,
}

impl Level {
    pub fn parse(s: &str) -> Level {
        match s {
            "error" => Level::Error,
            "warn" | "warning" => Level::Warn,
            "debug" => Level::Debug,
            "trace" => Level::Trace,
            _ => Level::Info,
        }
    }
    pub fn name(self) -> &'static str {
        ["error", "warn", "info", "debug", "trace"][self as usize]
    }
    fn syslog_sev(self) -> u8 {
        [3, 4, 6, 7, 7][self as usize]
    }
}

pub struct Options {
    pub file: Option<PathBuf>,
    pub level: Level,
    pub max_size: u64,
    pub max_files: u32,
    pub stderr: bool,
    pub syslog: bool,
    pub ident: String,
}

impl Default for Options {
    fn default() -> Self {
        Options { file: None, level: Level::Info, max_size: 5 << 20, max_files: 5, stderr: false, syslog: false, ident: "disc-launcher".into() }
    }
}

struct Logger {
    opts: Options,
    file: Option<File>,
    size: u64,
    syslog: Option<UnixDatagram>,
}

static LOGGER: OnceLock<Mutex<Logger>> = OnceLock::new();

/// Initialise le journal global. Sans appel, les messages vont sur stderr.
pub fn init(opts: Options) {
    let mut l = Logger { file: None, size: 0, syslog: None, opts };
    l.open();
    if l.opts.syslog {
        if let Ok(s) = UnixDatagram::unbound() {
            if s.connect("/dev/log").is_ok() {
                l.syslog = Some(s);
            }
        }
    }
    if LOGGER.set(Mutex::new(l)).is_err() {
        // Déjà initialisé : on ignore (cas des tests).
    }
}

impl Logger {
    fn open(&mut self) {
        if let Some(p) = &self.opts.file {
            if let Some(d) = p.parent() {
                let _ = std::fs::create_dir_all(d);
            }
            if let Ok(f) = OpenOptions::new().create(true).append(true).open(p) {
                self.size = f.metadata().map(|m| m.len()).unwrap_or(0);
                self.file = Some(f);
            }
        }
    }
    fn rotate(&mut self) {
        let Some(p) = self.opts.file.clone() else { return };
        self.file = None;
        let n = self.opts.max_files.max(1);
        let name = |i: u32| -> PathBuf {
            if i == 0 {
                p.clone()
            } else {
                PathBuf::from(format!("{}.{i}", p.display()))
            }
        };
        let _ = std::fs::remove_file(name(n - 1));
        for i in (0..n - 1).rev() {
            let _ = std::fs::rename(name(i), name(i + 1));
        }
        self.open();
    }
    fn write(&mut self, level: Level, line: &str, plain: &str) {
        if let Some(f) = &mut self.file {
            let _ = f.write_all(line.as_bytes());
            self.size += line.len() as u64;
            if self.opts.max_size > 0 && self.size > self.opts.max_size {
                self.rotate();
            }
        }
        if self.opts.stderr || self.file.is_none() && self.syslog.is_none() {
            let _ = std::io::stderr().write_all(line.as_bytes());
        }
        if let Some(s) = &self.syslog {
            let pri = 8 /* LOG_USER */ + level.syslog_sev();
            let msg = format!("<{pri}>{}[{}]: {plain}", self.opts.ident, crate::sys::pid());
            let _ = s.send(msg.as_bytes());
        }
    }
}

/// Horodatage local ISO 8601 avec millisecondes et décalage.
pub fn timestamp() -> String {
    let now = SystemTime::now().duration_since(UNIX_EPOCH).unwrap_or_default();
    let secs = now.as_secs() as i64;
    let (tm, off) = crate::sys::local_time(secs);
    let sign = if off < 0 { '-' } else { '+' };
    let off = off.abs();
    format!(
        "{:04}-{:02}-{:02}T{:02}:{:02}:{:02}.{:03}{}{:02}:{:02}",
        tm.tm_year + 1900,
        tm.tm_mon + 1,
        tm.tm_mday,
        tm.tm_hour,
        tm.tm_min,
        tm.tm_sec,
        now.subsec_millis(),
        sign,
        off / 3600,
        (off % 3600) / 60
    )
}

/// Date compacte pour les noms de fichiers : `20261002-143912`.
pub fn compact_stamp() -> String {
    let secs = SystemTime::now().duration_since(UNIX_EPOCH).unwrap_or_default().as_secs() as i64;
    let (tm, _) = crate::sys::local_time(secs);
    format!("{:04}{:02}{:02}-{:02}{:02}{:02}", tm.tm_year + 1900, tm.tm_mon + 1, tm.tm_mday, tm.tm_hour, tm.tm_min, tm.tm_sec)
}

pub fn logfmt_value(out: &mut String, v: &str) {
    let needs = v.is_empty() || v.chars().any(|c| c == ' ' || c == '"' || c == '=' || c == '\\' || c.is_control());
    if !needs {
        out.push_str(v);
        return;
    }
    out.push('"');
    for c in v.chars() {
        match c {
            '"' => out.push_str("\\\""),
            '\\' => out.push_str("\\\\"),
            '\n' => out.push_str("\\n"),
            '\r' => out.push_str("\\r"),
            '\t' => out.push_str("\\t"),
            c if c.is_control() => {
                let _ = write!(out, "\\u{:04x}", c as u32);
            }
            c => out.push(c),
        }
    }
    out.push('"');
}

pub fn enabled(level: Level) -> bool {
    match LOGGER.get() {
        Some(l) => l.lock().map(|g| level <= g.opts.level).unwrap_or(true),
        None => level <= Level::Info,
    }
}

pub fn log(level: Level, comp: &str, fields: &[(&str, &dyn std::fmt::Display)], msg: &str) {
    if !enabled(level) {
        return;
    }
    let mut body = String::new();
    let _ = write!(body, "level={} comp={}", level.name(), comp);
    for (k, v) in fields {
        body.push(' ');
        body.push_str(k);
        body.push('=');
        logfmt_value(&mut body, &v.to_string());
    }
    body.push_str(" msg=");
    logfmt_value(&mut body, msg);
    let line = format!("{} {}\n", timestamp(), body);
    match LOGGER.get() {
        Some(l) => {
            if let Ok(mut g) = l.lock() {
                g.write(level, &line, &body);
            }
        }
        None => {
            let _ = std::io::stderr().write_all(line.as_bytes());
        }
    }
}

/// `dl_log!(info, "comp", "message", "clé" => valeur, ...)`
#[macro_export]
macro_rules! dl_log {
    ($lvl:ident, $comp:expr, $msg:expr $(, $k:expr => $v:expr)* $(,)?) => {{
        let lvl = $crate::log::Level::from_ident(stringify!($lvl));
        if $crate::log::enabled(lvl) {
            $crate::log::log(lvl, $comp, &[$(($k, &$v as &dyn std::fmt::Display)),*], &$msg);
        }
    }};
}

impl Level {
    pub fn from_ident(s: &str) -> Level {
        Level::parse(s)
    }
}

/// Lecture et filtrage d'un journal logfmt (commande `disc-launcher log`).
pub fn parse_line(line: &str) -> Vec<(String, String)> {
    let mut out = vec![];
    let mut it = line.char_indices().peekable();
    // premier champ : horodatage
    let ts_end = line.find(' ').unwrap_or(line.len());
    out.push(("ts".to_string(), line[..ts_end].to_string()));
    while let Some(&(i, _)) = it.peek() {
        if i <= ts_end {
            it.next();
        } else {
            break;
        }
    }
    let rest: Vec<char> = line[ts_end.min(line.len())..].chars().collect();
    let mut i = 0;
    while i < rest.len() {
        while i < rest.len() && rest[i] == ' ' {
            i += 1;
        }
        let ks = i;
        while i < rest.len() && rest[i] != '=' && rest[i] != ' ' {
            i += 1;
        }
        let key: String = rest[ks..i].iter().collect();
        if i >= rest.len() || rest[i] != '=' {
            continue;
        }
        i += 1;
        let mut val = String::new();
        if i < rest.len() && rest[i] == '"' {
            i += 1;
            while i < rest.len() && rest[i] != '"' {
                if rest[i] == '\\' && i + 1 < rest.len() {
                    i += 1;
                    val.push(match rest[i] {
                        'n' => '\n',
                        't' => '\t',
                        'r' => '\r',
                        c => c,
                    });
                } else {
                    val.push(rest[i]);
                }
                i += 1;
            }
            i += 1;
        } else {
            while i < rest.len() && rest[i] != ' ' {
                val.push(rest[i]);
                i += 1;
            }
        }
        if !key.is_empty() {
            out.push((key, val));
        }
    }
    out
}

pub fn default_daemon_log() -> PathBuf {
    crate::paths::log_dir().join("daemon.log")
}

pub fn options_from_config(cfg: &crate::config::Config, file: Option<&Path>, stderr: bool) -> Options {
    Options {
        file: file.map(|p| p.to_path_buf()),
        level: Level::parse(cfg.raw.path("log.level").str_or("info")),
        max_size: (cfg.raw.path("log.max_size_mb").i64_or(5).max(0) as u64) << 20,
        max_files: cfg.raw.path("log.max_files").i64_or(5).clamp(1, 100) as u32,
        stderr,
        syslog: cfg.raw.path("log.syslog").bool_or(false),
        ident: "disc-launcher".into(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn logfmt_roundtrip() {
        let mut s = String::from("2026-01-01T00:00:00.000+00:00 level=info comp=x msg=");
        logfmt_value(&mut s, "a \"b\" c");
        let f = parse_line(&s);
        assert_eq!(f.iter().find(|(k, _)| k == "msg").unwrap().1, "a \"b\" c");
        assert_eq!(f.iter().find(|(k, _)| k == "comp").unwrap().1, "x");
    }
}
