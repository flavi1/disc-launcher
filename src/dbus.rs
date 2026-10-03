//! Client D-Bus minimal (protocole filaire, authentification EXTERNAL,
//! passage de descripteurs). Suffisant pour les notifications freedesktop,
//! logind/elogind (session active, inhibition de la veille) et udisks2.

use crate::sys;
use std::collections::VecDeque;
use std::io::{self, Read, Write};
use std::os::fd::{AsRawFd, RawFd};
use std::os::unix::net::UnixStream;
use std::time::{Duration, Instant};

#[derive(Clone, Debug, PartialEq)]
pub enum DV {
    Byte(u8),
    Bool(bool),
    I16(i16),
    U16(u16),
    I32(i32),
    U32(u32),
    I64(i64),
    U64(u64),
    Double(f64),
    Str(String),
    ObjPath(String),
    Sig(String),
    /// Signature des éléments + éléments.
    Array(String, Vec<DV>),
    Struct(Vec<DV>),
    Dict(Box<DV>, Box<DV>),
    Variant(Box<DV>),
    /// Index dans la liste des descripteurs du message.
    Fd(u32),
}

impl DV {
    pub fn signature(&self) -> String {
        match self {
            DV::Byte(_) => "y".into(),
            DV::Bool(_) => "b".into(),
            DV::I16(_) => "n".into(),
            DV::U16(_) => "q".into(),
            DV::I32(_) => "i".into(),
            DV::U32(_) => "u".into(),
            DV::I64(_) => "x".into(),
            DV::U64(_) => "t".into(),
            DV::Double(_) => "d".into(),
            DV::Str(_) => "s".into(),
            DV::ObjPath(_) => "o".into(),
            DV::Sig(_) => "g".into(),
            DV::Array(e, _) => format!("a{e}"),
            DV::Struct(v) => format!("({})", v.iter().map(|x| x.signature()).collect::<String>()),
            DV::Dict(k, v) => format!("{{{}{}}}", k.signature(), v.signature()),
            DV::Variant(_) => "v".into(),
            DV::Fd(_) => "h".into(),
        }
    }
    pub fn str(s: &str) -> DV {
        DV::Str(s.to_string())
    }
    pub fn as_str(&self) -> Option<&str> {
        match self {
            DV::Str(s) | DV::ObjPath(s) | DV::Sig(s) => Some(s),
            DV::Variant(v) => v.as_str(),
            _ => None,
        }
    }
    pub fn as_u32(&self) -> Option<u32> {
        match self {
            DV::U32(x) => Some(*x),
            DV::Variant(v) => v.as_u32(),
            _ => None,
        }
    }
    pub fn as_bool(&self) -> Option<bool> {
        match self {
            DV::Bool(b) => Some(*b),
            DV::Variant(v) => v.as_bool(),
            _ => None,
        }
    }
    /// Dictionnaire `a{sv}` vide.
    pub fn empty_dict() -> DV {
        DV::Array("{sv}".into(), vec![])
    }
    pub fn dict_sv(pairs: Vec<(&str, DV)>) -> DV {
        DV::Array("{sv}".into(), pairs.into_iter().map(|(k, v)| DV::Dict(Box::new(DV::str(k)), Box::new(DV::Variant(Box::new(v))))).collect())
    }
    pub fn strings(v: &[&str]) -> DV {
        DV::Array("s".into(), v.iter().map(|s| DV::str(s)).collect())
    }
}

// ------------------------------------------------------------- sérialisation

fn pad(buf: &mut Vec<u8>, align: usize) {
    while buf.len() % align != 0 {
        buf.push(0);
    }
}

fn align_of(sig: u8) -> usize {
    match sig {
        b'y' | b'g' | b'v' => 1,
        b'n' | b'q' => 2,
        b'b' | b'i' | b'u' | b's' | b'o' | b'a' | b'h' => 4,
        _ => 8, // x t d ( {
    }
}

fn marshal(buf: &mut Vec<u8>, v: &DV) {
    match v {
        DV::Byte(b) => buf.push(*b),
        DV::Bool(b) => {
            pad(buf, 4);
            buf.extend_from_slice(&(*b as u32).to_le_bytes());
        }
        DV::I16(x) => {
            pad(buf, 2);
            buf.extend_from_slice(&x.to_le_bytes());
        }
        DV::U16(x) => {
            pad(buf, 2);
            buf.extend_from_slice(&x.to_le_bytes());
        }
        DV::I32(x) => {
            pad(buf, 4);
            buf.extend_from_slice(&x.to_le_bytes());
        }
        DV::U32(x) | DV::Fd(x) => {
            pad(buf, 4);
            buf.extend_from_slice(&x.to_le_bytes());
        }
        DV::I64(x) => {
            pad(buf, 8);
            buf.extend_from_slice(&x.to_le_bytes());
        }
        DV::U64(x) => {
            pad(buf, 8);
            buf.extend_from_slice(&x.to_le_bytes());
        }
        DV::Double(x) => {
            pad(buf, 8);
            buf.extend_from_slice(&x.to_le_bytes());
        }
        DV::Str(s) | DV::ObjPath(s) => {
            pad(buf, 4);
            buf.extend_from_slice(&(s.len() as u32).to_le_bytes());
            buf.extend_from_slice(s.as_bytes());
            buf.push(0);
        }
        DV::Sig(s) => {
            buf.push(s.len() as u8);
            buf.extend_from_slice(s.as_bytes());
            buf.push(0);
        }
        DV::Array(esig, items) => {
            pad(buf, 4);
            let len_pos = buf.len();
            buf.extend_from_slice(&[0; 4]);
            pad(buf, align_of(esig.as_bytes()[0]));
            let start = buf.len();
            for it in items {
                marshal(buf, it);
            }
            let len = (buf.len() - start) as u32;
            buf[len_pos..len_pos + 4].copy_from_slice(&len.to_le_bytes());
        }
        DV::Struct(items) => {
            pad(buf, 8);
            for it in items {
                marshal(buf, it);
            }
        }
        DV::Dict(k, v) => {
            pad(buf, 8);
            marshal(buf, k);
            marshal(buf, v);
        }
        DV::Variant(inner) => {
            marshal(buf, &DV::Sig(inner.signature()));
            marshal(buf, inner);
        }
    }
}

/// Découpe une signature en types complets.
fn split_sig(sig: &str) -> Vec<String> {
    let b = sig.as_bytes();
    let mut out = vec![];
    let mut i = 0;
    while i < b.len() {
        let e = end_of_type(b, i);
        out.push(sig[i..e].to_string());
        i = e;
    }
    out
}

fn end_of_type(b: &[u8], i: usize) -> usize {
    match b[i] {
        b'a' => end_of_type(b, i + 1),
        b'(' | b'{' => {
            let (open, close) = if b[i] == b'(' { (b'(', b')') } else { (b'{', b'}') };
            let mut depth = 0;
            let mut j = i;
            while j < b.len() {
                if b[j] == open {
                    depth += 1;
                } else if b[j] == close {
                    depth -= 1;
                    if depth == 0 {
                        return j + 1;
                    }
                }
                j += 1;
            }
            b.len()
        }
        _ => i + 1,
    }
}

struct Reader<'a> {
    b: &'a [u8],
    pos: usize,
}

impl Reader<'_> {
    fn align(&mut self, a: usize) {
        self.pos = (self.pos + a - 1) / a * a;
    }
    fn take(&mut self, n: usize) -> io::Result<&[u8]> {
        if self.pos + n > self.b.len() {
            return Err(io::Error::new(io::ErrorKind::InvalidData, "message D-Bus tronqué"));
        }
        let s = &self.b[self.pos..self.pos + n];
        self.pos += n;
        Ok(s)
    }
    fn u32(&mut self) -> io::Result<u32> {
        self.align(4);
        let s = self.take(4)?;
        Ok(u32::from_le_bytes([s[0], s[1], s[2], s[3]]))
    }
    fn read(&mut self, sig: &str, depth: usize) -> io::Result<DV> {
        if depth > 32 {
            return Err(io::Error::new(io::ErrorKind::InvalidData, "imbrication excessive"));
        }
        let c = sig.as_bytes()[0];
        Ok(match c {
            b'y' => DV::Byte(self.take(1)?[0]),
            b'b' => DV::Bool(self.u32()? != 0),
            b'n' => {
                self.align(2);
                let s = self.take(2)?;
                DV::I16(i16::from_le_bytes([s[0], s[1]]))
            }
            b'q' => {
                self.align(2);
                let s = self.take(2)?;
                DV::U16(u16::from_le_bytes([s[0], s[1]]))
            }
            b'i' => DV::I32(self.u32()? as i32),
            b'u' => DV::U32(self.u32()?),
            b'h' => DV::Fd(self.u32()?),
            b'x' | b't' | b'd' => {
                self.align(8);
                let s = self.take(8)?;
                let a: [u8; 8] = s.try_into().unwrap();
                match c {
                    b'x' => DV::I64(i64::from_le_bytes(a)),
                    b't' => DV::U64(u64::from_le_bytes(a)),
                    _ => DV::Double(f64::from_le_bytes(a)),
                }
            }
            b's' | b'o' => {
                let n = self.u32()? as usize;
                let s = String::from_utf8_lossy(self.take(n)?).into_owned();
                self.take(1)?;
                if c == b's' {
                    DV::Str(s)
                } else {
                    DV::ObjPath(s)
                }
            }
            b'g' => {
                let n = self.take(1)?[0] as usize;
                let s = String::from_utf8_lossy(self.take(n)?).into_owned();
                self.take(1)?;
                DV::Sig(s)
            }
            b'v' => {
                let DV::Sig(s) = self.read("g", depth + 1)? else { unreachable!() };
                if s.is_empty() {
                    return Err(io::Error::new(io::ErrorKind::InvalidData, "variant vide"));
                }
                DV::Variant(Box::new(self.read(&s, depth + 1)?))
            }
            b'a' => {
                let n = self.u32()? as usize;
                let esig = &sig[1..end_of_type(sig.as_bytes(), 1)];
                self.align(align_of(esig.as_bytes()[0]));
                let end = self.pos + n;
                if end > self.b.len() {
                    return Err(io::Error::new(io::ErrorKind::InvalidData, "tableau tronqué"));
                }
                let mut items = vec![];
                while self.pos < end {
                    items.push(self.read(esig, depth + 1)?);
                }
                DV::Array(esig.to_string(), items)
            }
            b'(' => {
                self.align(8);
                let inner = &sig[1..end_of_type(sig.as_bytes(), 0) - 1];
                let mut v = vec![];
                for t in split_sig(inner) {
                    v.push(self.read(&t, depth + 1)?);
                }
                DV::Struct(v)
            }
            b'{' => {
                self.align(8);
                let inner = &sig[1..end_of_type(sig.as_bytes(), 0) - 1];
                let parts = split_sig(inner);
                let k = self.read(&parts[0], depth + 1)?;
                let v = self.read(parts.get(1).map(|s| s.as_str()).unwrap_or("v"), depth + 1)?;
                DV::Dict(Box::new(k), Box::new(v))
            }
            _ => return Err(io::Error::new(io::ErrorKind::InvalidData, format!("type D-Bus inconnu {}", c as char))),
        })
    }
}

// ------------------------------------------------------------- messages

#[derive(Clone, Debug, Default)]
pub struct Message {
    pub mtype: u8,
    pub serial: u32,
    pub reply_serial: Option<u32>,
    pub path: Option<String>,
    pub interface: Option<String>,
    pub member: Option<String>,
    pub error_name: Option<String>,
    pub destination: Option<String>,
    pub sender: Option<String>,
    pub body: Vec<DV>,
    pub fds: Vec<RawFd>,
}

pub const METHOD_CALL: u8 = 1;
pub const METHOD_RETURN: u8 = 2;
pub const ERROR: u8 = 3;
pub const SIGNAL: u8 = 4;

fn encode(m: &Message) -> Vec<u8> {
    let mut body = vec![];
    for v in &m.body {
        marshal(&mut body, v);
    }
    let sig: String = m.body.iter().map(|v| v.signature()).collect();
    let mut fields = vec![];
    let mut f = |code: u8, v: DV| fields.push(DV::Struct(vec![DV::Byte(code), DV::Variant(Box::new(v))]));
    if let Some(p) = &m.path {
        f(1, DV::ObjPath(p.clone()));
    }
    if let Some(i) = &m.interface {
        f(2, DV::Str(i.clone()));
    }
    if let Some(x) = &m.member {
        f(3, DV::Str(x.clone()));
    }
    if let Some(d) = &m.destination {
        f(6, DV::Str(d.clone()));
    }
    if !sig.is_empty() {
        f(8, DV::Sig(sig));
    }
    let mut buf = vec![b'l', m.mtype, 0, 1];
    buf.extend_from_slice(&(body.len() as u32).to_le_bytes());
    buf.extend_from_slice(&m.serial.to_le_bytes());
    marshal(&mut buf, &DV::Array("(yv)".into(), fields));
    pad(&mut buf, 8);
    buf.extend_from_slice(&body);
    buf
}

fn decode(b: &[u8], fds: Vec<RawFd>) -> io::Result<Message> {
    if b[0] != b'l' {
        return Err(io::Error::new(io::ErrorKind::InvalidData, "boutisme D-Bus non pris en charge"));
    }
    let mut r = Reader { b, pos: 4 };
    let body_len = r.u32()? as usize;
    let serial = r.u32()?;
    let DV::Array(_, fields) = r.read("a(yv)", 0)? else { unreachable!() };
    r.align(8);
    let mut m = Message { mtype: b[1], serial, fds, ..Default::default() };
    let mut sig = String::new();
    for f in fields {
        if let DV::Struct(v) = f {
            if let (Some(DV::Byte(code)), Some(DV::Variant(val))) = (v.first(), v.get(1)) {
                match code {
                    1 => m.path = val.as_str().map(|s| s.to_string()),
                    2 => m.interface = val.as_str().map(|s| s.to_string()),
                    3 => m.member = val.as_str().map(|s| s.to_string()),
                    4 => m.error_name = val.as_str().map(|s| s.to_string()),
                    5 => m.reply_serial = val.as_u32(),
                    6 => m.destination = val.as_str().map(|s| s.to_string()),
                    7 => m.sender = val.as_str().map(|s| s.to_string()),
                    8 => sig = val.as_str().unwrap_or("").to_string(),
                    _ => {}
                }
            }
        }
    }
    let body_start = r.pos;
    let mut br = Reader { b: &b[body_start..body_start + body_len.min(b.len() - body_start)], pos: 0 };
    for t in split_sig(&sig) {
        m.body.push(br.read(&t, 0)?);
    }
    Ok(m)
}

// ------------------------------------------------------------- connexion

pub struct Connection {
    stream: UnixStream,
    serial: u32,
    pub unique_name: String,
    queue: VecDeque<Message>,
    inbuf: Vec<u8>,
    infds: Vec<RawFd>,
}

#[derive(Clone, Copy)]
pub enum Bus {
    Session,
    System,
}

fn connect_address(addr: &str) -> io::Result<UnixStream> {
    for part in addr.split(';') {
        let Some(rest) = part.strip_prefix("unix:") else { continue };
        for kv in rest.split(',') {
            if let Some(p) = kv.strip_prefix("path=") {
                if let Ok(s) = UnixStream::connect(unescape(p)) {
                    return Ok(s);
                }
            } else if let Some(n) = kv.strip_prefix("abstract=") {
                use std::os::linux::net::SocketAddrExt;
                let a = std::os::unix::net::SocketAddr::from_abstract_name(unescape(n).as_bytes())?;
                if let Ok(s) = UnixStream::connect_addr(&a) {
                    return Ok(s);
                }
            }
        }
    }
    Err(io::Error::new(io::ErrorKind::NotFound, format!("adresse D-Bus inutilisable : {addr}")))
}

fn unescape(s: &str) -> String {
    let b = s.as_bytes();
    let mut out = vec![];
    let mut i = 0;
    while i < b.len() {
        if b[i] == b'%' && i + 2 < b.len() {
            if let Ok(v) = u8::from_str_radix(&s[i + 1..i + 3], 16) {
                out.push(v);
                i += 3;
                continue;
            }
        }
        out.push(b[i]);
        i += 1;
    }
    String::from_utf8_lossy(&out).into_owned()
}

impl Connection {
    pub fn open(bus: Bus) -> io::Result<Connection> {
        let addr = match bus {
            Bus::Session => std::env::var("DBUS_SESSION_BUS_ADDRESS").ok().filter(|s| !s.is_empty()).unwrap_or_else(|| {
                std::env::var("XDG_RUNTIME_DIR").map(|d| format!("unix:path={d}/bus")).unwrap_or_default()
            }),
            Bus::System => std::env::var("DBUS_SYSTEM_BUS_ADDRESS").unwrap_or_else(|_| "unix:path=/run/dbus/system_bus_socket".into()),
        };
        let mut stream = connect_address(&addr)?;
        stream.set_read_timeout(Some(Duration::from_secs(5)))?;
        // Authentification EXTERNAL
        let uid = sys::euid().to_string();
        let hexuid: String = uid.bytes().map(|b| format!("{b:02x}")).collect();
        stream.write_all(format!("\0AUTH EXTERNAL {hexuid}\r\n").as_bytes())?;
        let line = read_line(&mut stream)?;
        if !line.starts_with("OK") {
            return Err(io::Error::new(io::ErrorKind::PermissionDenied, format!("authentification D-Bus refusée : {line}")));
        }
        stream.write_all(b"NEGOTIATE_UNIX_FD\r\n")?;
        let _ = read_line(&mut stream)?;
        stream.write_all(b"BEGIN\r\n")?;
        let mut c = Connection { stream, serial: 0, unique_name: String::new(), queue: VecDeque::new(), inbuf: vec![], infds: vec![] };
        let r = c.call("org.freedesktop.DBus", "/org/freedesktop/DBus", "org.freedesktop.DBus", "Hello", vec![], Duration::from_secs(5))?;
        c.unique_name = r.body.first().and_then(|v| v.as_str()).unwrap_or("").to_string();
        Ok(c)
    }

    pub fn send(&mut self, mut m: Message) -> io::Result<u32> {
        self.serial += 1;
        m.serial = self.serial;
        self.stream.write_all(&encode(&m))?;
        Ok(self.serial)
    }

    /// Appel de méthode synchrone ; les signaux reçus entre-temps sont mis en file.
    pub fn call(&mut self, dest: &str, path: &str, iface: &str, member: &str, body: Vec<DV>, timeout: Duration) -> io::Result<Message> {
        let serial = self.send(Message {
            mtype: METHOD_CALL,
            path: Some(path.into()),
            interface: Some(iface.into()),
            member: Some(member.into()),
            destination: Some(dest.into()),
            body,
            ..Default::default()
        })?;
        let end = Instant::now() + timeout;
        loop {
            let left = end.saturating_duration_since(Instant::now());
            if left.is_zero() {
                return Err(io::Error::new(io::ErrorKind::TimedOut, format!("{iface}.{member} : pas de réponse")));
            }
            let Some(m) = self.read_message(left)? else { continue };
            if m.reply_serial == Some(serial) && (m.mtype == METHOD_RETURN || m.mtype == ERROR) {
                if m.mtype == ERROR {
                    let detail = m.body.first().and_then(|v| v.as_str()).unwrap_or("").to_string();
                    for fd in &m.fds {
                        sys::close_fd(*fd);
                    }
                    return Err(io::Error::new(io::ErrorKind::Other, format!("{} : {detail}", m.error_name.unwrap_or_default())));
                }
                return Ok(m);
            }
            if m.mtype == SIGNAL {
                self.queue.push_back(m);
            } else {
                for fd in &m.fds {
                    sys::close_fd(*fd);
                }
            }
        }
    }

    pub fn add_match(&mut self, rule: &str) -> io::Result<()> {
        self.call("org.freedesktop.DBus", "/org/freedesktop/DBus", "org.freedesktop.DBus", "AddMatch", vec![DV::str(rule)], Duration::from_secs(5)).map(|_| ())
    }

    /// Prochain signal (file d'attente ou socket), ou None après `timeout`.
    pub fn next_signal(&mut self, timeout: Duration) -> io::Result<Option<Message>> {
        if let Some(m) = self.queue.pop_front() {
            return Ok(Some(m));
        }
        let end = Instant::now() + timeout;
        loop {
            let left = end.saturating_duration_since(Instant::now());
            if left.is_zero() {
                return Ok(None);
            }
            match self.read_message(left)? {
                Some(m) if m.mtype == SIGNAL => return Ok(Some(m)),
                Some(_) => continue,
                None => return Ok(None),
            }
        }
    }

    fn read_message(&mut self, timeout: Duration) -> io::Result<Option<Message>> {
        let end = Instant::now() + timeout;
        loop {
            if let Some(total) = message_len(&self.inbuf) {
                if self.inbuf.len() >= total {
                    let data: Vec<u8> = self.inbuf.drain(..total).collect();
                    let fds = std::mem::take(&mut self.infds);
                    return decode(&data, fds).map(Some);
                }
            }
            let left = end.saturating_duration_since(Instant::now());
            if left.is_zero() {
                return Ok(None);
            }
            self.stream.set_read_timeout(Some(left.max(Duration::from_millis(1))))?;
            let mut buf = vec![0u8; 65536];
            match sys::recv_with_fds(self.stream.as_raw_fd(), &mut buf, &mut self.infds) {
                Ok(0) => return Err(io::Error::new(io::ErrorKind::ConnectionAborted, "bus D-Bus fermé")),
                Ok(n) => self.inbuf.extend_from_slice(&buf[..n]),
                Err(e) if e.kind() == io::ErrorKind::WouldBlock || e.kind() == io::ErrorKind::TimedOut => return Ok(None),
                Err(e) if e.kind() == io::ErrorKind::Interrupted => continue,
                Err(e) => return Err(e),
            }
        }
    }

    /// Propriété via org.freedesktop.DBus.Properties.Get.
    pub fn get_property(&mut self, dest: &str, path: &str, iface: &str, prop: &str) -> io::Result<DV> {
        let r = self.call(dest, path, "org.freedesktop.DBus.Properties", "Get", vec![DV::str(iface), DV::str(prop)], Duration::from_secs(5))?;
        r.body.into_iter().next().ok_or_else(|| io::Error::new(io::ErrorKind::InvalidData, "propriété vide"))
    }
}

fn message_len(b: &[u8]) -> Option<usize> {
    if b.len() < 16 {
        return None;
    }
    let body = u32::from_le_bytes([b[4], b[5], b[6], b[7]]) as usize;
    let fields = u32::from_le_bytes([b[12], b[13], b[14], b[15]]) as usize;
    let hdr = (16 + fields + 7) / 8 * 8;
    Some(hdr + body)
}

fn read_line(s: &mut UnixStream) -> io::Result<String> {
    let mut out = vec![];
    let mut b = [0u8; 1];
    loop {
        s.read_exact(&mut b)?;
        if b[0] == b'\n' {
            break;
        }
        out.push(b[0]);
        if out.len() > 4096 {
            break;
        }
    }
    Ok(String::from_utf8_lossy(&out).trim_end().to_string())
}

// ------------------------------------------------------------- services

/// Inhibiteur de veille : garde ouvert le descripteur logind, ou le cookie
/// PowerManagement avec sa connexion.
pub enum Inhibitor {
    Logind(RawFd),
    PowerManagement(Connection, u32),
}

impl Drop for Inhibitor {
    fn drop(&mut self) {
        match self {
            Inhibitor::Logind(fd) => sys::close_fd(*fd),
            Inhibitor::PowerManagement(c, cookie) => {
                let _ = c.call("org.freedesktop.PowerManagement", "/org/freedesktop/PowerManagement/Inhibit", "org.freedesktop.PowerManagement.Inhibit", "UnInhibit", vec![DV::U32(*cookie)], Duration::from_secs(2));
            }
        }
    }
}

/// Bloque la mise en veille : logind/elogind, sinon PowerManagement.
pub fn inhibit_sleep(why: &str) -> Option<Inhibitor> {
    if let Ok(mut c) = Connection::open(Bus::System) {
        if let Ok(r) = c.call(
            "org.freedesktop.login1",
            "/org/freedesktop/login1",
            "org.freedesktop.login1.Manager",
            "Inhibit",
            vec![DV::str("sleep:idle"), DV::str("disc-launcher"), DV::str(why), DV::str("block")],
            Duration::from_secs(5),
        ) {
            if let Some(DV::Fd(i)) = r.body.first() {
                if let Some(fd) = r.fds.get(*i as usize) {
                    return Some(Inhibitor::Logind(*fd));
                }
            }
        }
    }
    if let Ok(mut c) = Connection::open(Bus::Session) {
        if let Ok(r) = c.call("org.freedesktop.PowerManagement", "/org/freedesktop/PowerManagement/Inhibit", "org.freedesktop.PowerManagement.Inhibit", "Inhibit", vec![DV::str("disc-launcher"), DV::str(why)], Duration::from_secs(5)) {
            if let Some(cookie) = r.body.first().and_then(|v| v.as_u32()) {
                return Some(Inhibitor::PowerManagement(c, cookie));
            }
        }
    }
    None
}

/// La session du processus courant est-elle active (logind/elogind) ?
/// None si logind est indisponible.
pub fn session_active() -> Option<bool> {
    let mut c = Connection::open(Bus::System).ok()?;
    // Le démon n'appartient pas toujours à une session (lancé par le
    // gestionnaire systemd utilisateur, ex. XDG Autostart sous Plasma 6) :
    // « session/auto » désigne alors la session graphique de l'utilisateur.
    let path = c
        .call("org.freedesktop.login1", "/org/freedesktop/login1", "org.freedesktop.login1.Manager", "GetSessionByPID", vec![DV::U32(sys::pid() as u32)], Duration::from_secs(3))
        .ok()
        .and_then(|r| r.body.first().and_then(|v| v.as_str()).map(|s| s.to_string()))
        .unwrap_or_else(|| "/org/freedesktop/login1/session/auto".to_string());
    c.get_property("org.freedesktop.login1", &path, "org.freedesktop.login1.Session", "Active").ok()?.as_bool()
}

/// Demande à udisks2 de démonter le système de fichiers de `dev` (montage
/// automatique du bureau), avant un dump : redumper exige le lecteur libre.
pub fn udisks_unmount(dev: &str) -> io::Result<()> {
    let mut c = Connection::open(Bus::System)?;
    let name = dev.trim_start_matches("/dev/");
    let path = format!("/org/freedesktop/UDisks2/block_devices/{}", name.replace(|ch: char| !ch.is_ascii_alphanumeric(), "_"));
    c.call("org.freedesktop.UDisks2", &path, "org.freedesktop.UDisks2.Filesystem", "Unmount", vec![DV::empty_dict()], Duration::from_secs(30))?;
    Ok(())
}

/// Demande à udisks2 de monter le système de fichiers de `dev` (BD-Video en UDF).
pub fn udisks_mount(dev: &str) -> io::Result<String> {
    let mut c = Connection::open(Bus::System)?;
    let name = dev.trim_start_matches("/dev/");
    let path = format!("/org/freedesktop/UDisks2/block_devices/{}", name.replace(|ch: char| !ch.is_ascii_alphanumeric(), "_"));
    let r = c.call("org.freedesktop.UDisks2", &path, "org.freedesktop.UDisks2.Filesystem", "Mount", vec![DV::empty_dict()], Duration::from_secs(30))?;
    Ok(r.body.first().and_then(|v| v.as_str()).unwrap_or("").to_string())
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn marshal_roundtrip() {
        let m = Message {
            mtype: METHOD_CALL,
            serial: 7,
            path: Some("/org/freedesktop/Notifications".into()),
            interface: Some("org.freedesktop.Notifications".into()),
            member: Some("Notify".into()),
            destination: Some("org.freedesktop.Notifications".into()),
            body: vec![
                DV::str("disc-launcher"),
                DV::U32(0),
                DV::str("media-optical"),
                DV::str("Titre"),
                DV::str("Corps"),
                DV::strings(&["play", "Jouer", "dump", "Dumper"]),
                DV::dict_sv(vec![("resident", DV::Bool(true)), ("urgency", DV::Byte(1)), ("value", DV::I32(42))]),
                DV::I32(-1),
            ],
            ..Default::default()
        };
        let b = encode(&m);
        assert_eq!(message_len(&b), Some(b.len()));
        let d = decode(&b, vec![]).unwrap();
        assert_eq!(d.member.as_deref(), Some("Notify"));
        assert_eq!(d.body, m.body);
        assert_eq!(d.body[6].signature(), "a{sv}");
    }
}
