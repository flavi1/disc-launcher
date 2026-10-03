//! JSON minimal : valeur, analyseur, sérialiseur.
//! Sert aussi de modèle de données pour le TOML (voir `toml.rs`).

use std::collections::BTreeMap;
use std::fmt::Write as _;

pub type Map = BTreeMap<String, Value>;

#[derive(Clone, Debug, PartialEq, Default)]
pub enum Value {
    #[default]
    Null,
    Bool(bool),
    Int(i64),
    Num(f64),
    Str(String),
    Arr(Vec<Value>),
    Obj(Map),
}

static NULL: Value = Value::Null;

impl Value {
    pub fn obj() -> Value {
        Value::Obj(Map::new())
    }
    pub fn get(&self, key: &str) -> &Value {
        match self {
            Value::Obj(m) => m.get(key).unwrap_or(&NULL),
            _ => &NULL,
        }
    }
    /// Chemin pointé : `a.b.c`.
    pub fn path(&self, dotted: &str) -> &Value {
        let mut v = self;
        for k in dotted.split('.') {
            v = v.get(k);
        }
        v
    }
    pub fn get_mut(&mut self, key: &str) -> Option<&mut Value> {
        match self {
            Value::Obj(m) => m.get_mut(key),
            _ => None,
        }
    }
    pub fn set(&mut self, key: &str, v: impl Into<Value>) {
        if !matches!(self, Value::Obj(_)) {
            *self = Value::obj();
        }
        if let Value::Obj(m) = self {
            m.insert(key.to_string(), v.into());
        }
    }
    pub fn push(&mut self, v: impl Into<Value>) {
        if !matches!(self, Value::Arr(_)) {
            *self = Value::Arr(vec![]);
        }
        if let Value::Arr(a) = self {
            a.push(v.into());
        }
    }
    pub fn is_null(&self) -> bool {
        matches!(self, Value::Null)
    }
    pub fn as_str(&self) -> Option<&str> {
        match self {
            Value::Str(s) => Some(s),
            _ => None,
        }
    }
    pub fn str_or<'a>(&'a self, d: &'a str) -> &'a str {
        self.as_str().unwrap_or(d)
    }
    pub fn string(&self) -> Option<String> {
        self.as_str().map(|s| s.to_string())
    }
    pub fn as_i64(&self) -> Option<i64> {
        match self {
            Value::Int(i) => Some(*i),
            Value::Num(f) if f.fract() == 0.0 => Some(*f as i64),
            _ => None,
        }
    }
    pub fn as_f64(&self) -> Option<f64> {
        match self {
            Value::Int(i) => Some(*i as f64),
            Value::Num(f) => Some(*f),
            _ => None,
        }
    }
    pub fn as_bool(&self) -> Option<bool> {
        match self {
            Value::Bool(b) => Some(*b),
            _ => None,
        }
    }
    pub fn bool_or(&self, d: bool) -> bool {
        self.as_bool().unwrap_or(d)
    }
    pub fn i64_or(&self, d: i64) -> i64 {
        self.as_i64().unwrap_or(d)
    }
    pub fn as_arr(&self) -> &[Value] {
        match self {
            Value::Arr(a) => a,
            _ => &[],
        }
    }
    pub fn as_obj(&self) -> Option<&Map> {
        match self {
            Value::Obj(m) => Some(m),
            _ => None,
        }
    }
    /// Tableau de chaînes (ignore les éléments non textuels).
    pub fn strings(&self) -> Vec<String> {
        self.as_arr().iter().filter_map(|v| v.string()).collect()
    }

    pub fn to_json(&self) -> String {
        let mut s = String::new();
        write_value(&mut s, self, None, 0);
        s
    }
    pub fn to_pretty(&self) -> String {
        let mut s = String::new();
        write_value(&mut s, self, Some(2), 0);
        s
    }
}

impl std::ops::Index<&str> for Value {
    type Output = Value;
    fn index(&self, k: &str) -> &Value {
        self.get(k)
    }
}

impl From<&str> for Value {
    fn from(s: &str) -> Self {
        Value::Str(s.to_string())
    }
}
impl From<String> for Value {
    fn from(s: String) -> Self {
        Value::Str(s)
    }
}
impl From<&String> for Value {
    fn from(s: &String) -> Self {
        Value::Str(s.clone())
    }
}
impl From<bool> for Value {
    fn from(b: bool) -> Self {
        Value::Bool(b)
    }
}
macro_rules! from_int {
    ($($t:ty),*) => {$(impl From<$t> for Value { fn from(i: $t) -> Self { Value::Int(i as i64) } })*};
}
from_int!(i32, i64, u8, u16, u32, u64, usize);
impl From<f64> for Value {
    fn from(f: f64) -> Self {
        Value::Num(f)
    }
}
impl From<Vec<Value>> for Value {
    fn from(v: Vec<Value>) -> Self {
        Value::Arr(v)
    }
}
impl From<Vec<String>> for Value {
    fn from(v: Vec<String>) -> Self {
        Value::Arr(v.into_iter().map(Value::Str).collect())
    }
}
impl From<Map> for Value {
    fn from(m: Map) -> Self {
        Value::Obj(m)
    }
}
impl<T: Into<Value>> From<Option<T>> for Value {
    fn from(o: Option<T>) -> Self {
        match o {
            Some(v) => v.into(),
            None => Value::Null,
        }
    }
}

/// Construit un objet : `jobj! { "a" => 1, "b" => "x" }`.
#[macro_export]
macro_rules! jobj {
    () => { $crate::json::Value::obj() };
    ($($k:expr => $v:expr),+ $(,)?) => {{
        let mut o = $crate::json::Value::obj();
        $( o.set($k, $v); )+
        o
    }};
}

pub fn escape_into(out: &mut String, s: &str) {
    out.push('"');
    for c in s.chars() {
        match c {
            '"' => out.push_str("\\\""),
            '\\' => out.push_str("\\\\"),
            '\n' => out.push_str("\\n"),
            '\r' => out.push_str("\\r"),
            '\t' => out.push_str("\\t"),
            c if (c as u32) < 0x20 => {
                let _ = write!(out, "\\u{:04x}", c as u32);
            }
            c => out.push(c),
        }
    }
    out.push('"');
}

fn write_value(out: &mut String, v: &Value, indent: Option<usize>, depth: usize) {
    let nl = |out: &mut String, d: usize| {
        if let Some(n) = indent {
            out.push('\n');
            for _ in 0..n * d {
                out.push(' ');
            }
        }
    };
    match v {
        Value::Null => out.push_str("null"),
        Value::Bool(b) => out.push_str(if *b { "true" } else { "false" }),
        Value::Int(i) => {
            let _ = write!(out, "{i}");
        }
        Value::Num(f) => {
            if f.is_finite() {
                let _ = write!(out, "{f}");
            } else {
                out.push_str("null");
            }
        }
        Value::Str(s) => escape_into(out, s),
        Value::Arr(a) => {
            if a.is_empty() {
                out.push_str("[]");
                return;
            }
            out.push('[');
            for (i, x) in a.iter().enumerate() {
                if i > 0 {
                    out.push(',');
                }
                nl(out, depth + 1);
                write_value(out, x, indent, depth + 1);
            }
            nl(out, depth);
            out.push(']');
        }
        Value::Obj(m) => {
            if m.is_empty() {
                out.push_str("{}");
                return;
            }
            out.push('{');
            for (i, (k, x)) in m.iter().enumerate() {
                if i > 0 {
                    out.push(',');
                }
                nl(out, depth + 1);
                escape_into(out, k);
                out.push(':');
                if indent.is_some() {
                    out.push(' ');
                }
                write_value(out, x, indent, depth + 1);
            }
            nl(out, depth);
            out.push('}');
        }
    }
}

#[derive(Debug)]
pub struct ParseError(pub String);
impl std::fmt::Display for ParseError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "JSON invalide : {}", self.0)
    }
}
impl std::error::Error for ParseError {}

pub fn parse(s: &str) -> Result<Value, ParseError> {
    let mut p = Parser { b: s.as_bytes(), i: 0, depth: 0 };
    p.ws();
    let v = p.value()?;
    p.ws();
    if p.i != p.b.len() {
        return Err(p.err("caractères en trop"));
    }
    Ok(v)
}

struct Parser<'a> {
    b: &'a [u8],
    i: usize,
    depth: usize,
}

impl Parser<'_> {
    fn err(&self, m: &str) -> ParseError {
        ParseError(format!("{m} (position {})", self.i))
    }
    fn ws(&mut self) {
        while self.i < self.b.len() && matches!(self.b[self.i], b' ' | b'\t' | b'\n' | b'\r') {
            self.i += 1;
        }
    }
    fn eat(&mut self, lit: &str) -> bool {
        if self.b[self.i..].starts_with(lit.as_bytes()) {
            self.i += lit.len();
            true
        } else {
            false
        }
    }
    fn value(&mut self) -> Result<Value, ParseError> {
        if self.i >= self.b.len() {
            return Err(self.err("fin inattendue"));
        }
        if self.eat("null") {
            return Ok(Value::Null);
        }
        if self.eat("true") {
            return Ok(Value::Bool(true));
        }
        if self.eat("false") {
            return Ok(Value::Bool(false));
        }
        match self.b[self.i] {
            b'"' => Ok(Value::Str(self.string()?)),
            b'[' => {
                self.depth += 1;
                if self.depth > 128 {
                    return Err(self.err("imbrication trop profonde"));
                }
                self.i += 1;
                let mut a = vec![];
                self.ws();
                if self.i < self.b.len() && self.b[self.i] == b']' {
                    self.i += 1;
                    self.depth -= 1;
                    return Ok(Value::Arr(a));
                }
                loop {
                    self.ws();
                    a.push(self.value()?);
                    self.ws();
                    match self.b.get(self.i) {
                        Some(b',') => self.i += 1,
                        Some(b']') => {
                            self.i += 1;
                            break;
                        }
                        _ => return Err(self.err("',' ou ']' attendu")),
                    }
                }
                self.depth -= 1;
                Ok(Value::Arr(a))
            }
            b'{' => {
                self.depth += 1;
                if self.depth > 128 {
                    return Err(self.err("imbrication trop profonde"));
                }
                self.i += 1;
                let mut m = Map::new();
                self.ws();
                if self.i < self.b.len() && self.b[self.i] == b'}' {
                    self.i += 1;
                    self.depth -= 1;
                    return Ok(Value::Obj(m));
                }
                loop {
                    self.ws();
                    if self.b.get(self.i) != Some(&b'"') {
                        return Err(self.err("clé attendue"));
                    }
                    let k = self.string()?;
                    self.ws();
                    if self.b.get(self.i) != Some(&b':') {
                        return Err(self.err("':' attendu"));
                    }
                    self.i += 1;
                    self.ws();
                    let v = self.value()?;
                    m.insert(k, v);
                    self.ws();
                    match self.b.get(self.i) {
                        Some(b',') => self.i += 1,
                        Some(b'}') => {
                            self.i += 1;
                            break;
                        }
                        _ => return Err(self.err("',' ou '}' attendu")),
                    }
                }
                self.depth -= 1;
                Ok(Value::Obj(m))
            }
            b'-' | b'0'..=b'9' => self.number(),
            _ => Err(self.err("valeur attendue")),
        }
    }
    fn number(&mut self) -> Result<Value, ParseError> {
        let start = self.i;
        let mut float = false;
        while self.i < self.b.len() {
            match self.b[self.i] {
                b'0'..=b'9' | b'-' | b'+' => {}
                b'.' | b'e' | b'E' => float = true,
                _ => break,
            }
            self.i += 1;
        }
        let s = std::str::from_utf8(&self.b[start..self.i]).unwrap();
        if !float {
            if let Ok(i) = s.parse::<i64>() {
                return Ok(Value::Int(i));
            }
        }
        s.parse::<f64>().map(Value::Num).map_err(|_| self.err("nombre invalide"))
    }
    fn hex4(&mut self) -> Result<u32, ParseError> {
        if self.i + 4 > self.b.len() {
            return Err(self.err("\\u incomplet"));
        }
        let s = std::str::from_utf8(&self.b[self.i..self.i + 4]).map_err(|_| self.err("\\u invalide"))?;
        self.i += 4;
        u32::from_str_radix(s, 16).map_err(|_| self.err("\\u invalide"))
    }
    fn string(&mut self) -> Result<String, ParseError> {
        self.i += 1;
        let mut out = Vec::new();
        loop {
            let c = *self.b.get(self.i).ok_or_else(|| self.err("chaîne non terminée"))?;
            self.i += 1;
            match c {
                b'"' => break,
                b'\\' => {
                    let e = *self.b.get(self.i).ok_or_else(|| self.err("échappement incomplet"))?;
                    self.i += 1;
                    match e {
                        b'"' => out.push(b'"'),
                        b'\\' => out.push(b'\\'),
                        b'/' => out.push(b'/'),
                        b'b' => out.push(8),
                        b'f' => out.push(12),
                        b'n' => out.push(b'\n'),
                        b'r' => out.push(b'\r'),
                        b't' => out.push(b'\t'),
                        b'u' => {
                            let mut cp = self.hex4()?;
                            if (0xD800..0xDC00).contains(&cp) && self.eat("\\u") {
                                let lo = self.hex4()?;
                                cp = 0x10000 + ((cp - 0xD800) << 10) + (lo.wrapping_sub(0xDC00) & 0x3FF);
                            }
                            let ch = char::from_u32(cp).unwrap_or('\u{FFFD}');
                            let mut b = [0u8; 4];
                            out.extend_from_slice(ch.encode_utf8(&mut b).as_bytes());
                        }
                        _ => return Err(self.err("échappement inconnu")),
                    }
                }
                c => out.push(c),
            }
        }
        String::from_utf8(out).map_err(|_| self.err("UTF-8 invalide"))
    }
}

/// Fusion profonde : les objets sont fusionnés clé par clé, le reste est remplacé.
pub fn merge(base: &mut Value, over: &Value) {
    match (base, over) {
        (Value::Obj(b), Value::Obj(o)) => {
            for (k, v) in o {
                match b.get_mut(k) {
                    Some(bv) => merge(bv, v),
                    None => {
                        b.insert(k.clone(), v.clone());
                    }
                }
            }
        }
        (b, o) => *b = o.clone(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn roundtrip() {
        let s = r#"{"a":[1,2.5,"x\né"],"b":{"c":null,"d":true},"e":-3}"#;
        let v = parse(s).unwrap();
        assert_eq!(v["a"].as_arr()[2].as_str(), Some("x\né"));
        assert_eq!(v["e"].as_i64(), Some(-3));
        assert_eq!(parse(&v.to_json()).unwrap(), v);
        assert_eq!(parse(&v.to_pretty()).unwrap(), v);
    }
    #[test]
    fn merge_deep() {
        let mut a = parse(r#"{"x":{"y":1,"z":2},"l":[1]}"#).unwrap();
        merge(&mut a, &parse(r#"{"x":{"y":5},"l":[2,3]}"#).unwrap());
        assert_eq!(a.path("x.y").as_i64(), Some(5));
        assert_eq!(a.path("x.z").as_i64(), Some(2));
        assert_eq!(a["l"].as_arr().len(), 2);
    }
}
