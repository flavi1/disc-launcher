//! Analyseur TOML (sous-ensemble utile : tables, tableaux de tables, clés
//! pointées, chaînes simples/littérales/multilignes, entiers, flottants,
//! booléens, tableaux, tables en ligne). Produit un `json::Value`.
//! Les dates sont conservées sous forme de chaîne.

use crate::json::{Map, Value};

#[derive(Debug)]
pub struct TomlError {
    pub line: usize,
    pub msg: String,
}
impl std::fmt::Display for TomlError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "TOML invalide, ligne {} : {}", self.line, self.msg)
    }
}
impl std::error::Error for TomlError {}

pub fn parse(src: &str) -> Result<Value, TomlError> {
    let mut p = P { c: src.chars().collect(), i: 0, line: 1 };
    let mut root = Value::obj();
    let mut current: Vec<String> = vec![];
    loop {
        p.skip_ws_nl_comments();
        if p.eof() {
            break;
        }
        if p.peek() == '[' {
            let array = p.peek_at(1) == '[';
            p.i += if array { 2 } else { 1 };
            p.skip_ws();
            let path = p.key_path()?;
            p.skip_ws();
            if !p.eat(']') || (array && !p.eat(']')) {
                return Err(p.err("']' attendu"));
            }
            p.end_of_line()?;
            if array {
                let parent = navigate(&mut root, &path[..path.len() - 1], &p)?;
                let last = path.last().unwrap().clone();
                let slot = obj_mut(parent).entry(last).or_insert_with(|| Value::Arr(vec![]));
                match slot {
                    Value::Arr(a) => a.push(Value::obj()),
                    _ => return Err(p.err("la clé n'est pas un tableau de tables")),
                }
            } else {
                navigate(&mut root, &path, &p)?;
            }
            current = path;
        } else {
            let path = p.key_path()?;
            p.skip_ws();
            if !p.eat('=') {
                return Err(p.err("'=' attendu"));
            }
            p.skip_ws();
            let v = p.value()?;
            p.end_of_line()?;
            let mut full = current.clone();
            full.extend_from_slice(&path[..path.len() - 1]);
            let tbl = navigate(&mut root, &full, &p)?;
            let k = path.last().unwrap().clone();
            let m = obj_mut(tbl);
            if m.contains_key(&k) {
                return Err(p.err(&format!("clé « {k} » définie deux fois")));
            }
            m.insert(k, v);
        }
    }
    Ok(root)
}

fn obj_mut(v: &mut Value) -> &mut Map {
    if !matches!(v, Value::Obj(_)) {
        *v = Value::obj();
    }
    match v {
        Value::Obj(m) => m,
        _ => unreachable!(),
    }
}

/// Descend dans `path`, en créant les tables manquantes ; dans un tableau de
/// tables, prend le dernier élément.
fn navigate<'a>(root: &'a mut Value, path: &[String], p: &P) -> Result<&'a mut Value, TomlError> {
    let mut v = root;
    for k in path {
        let m = obj_mut(v);
        let next = m.entry(k.clone()).or_insert_with(Value::obj);
        v = match next {
            Value::Arr(a) => match a.last_mut() {
                Some(x @ Value::Obj(_)) => x,
                _ => return Err(p.err("tableau inattendu dans le chemin")),
            },
            Value::Obj(_) => next,
            _ => return Err(p.err(&format!("« {k} » n'est pas une table"))),
        };
    }
    Ok(v)
}

struct P {
    c: Vec<char>,
    i: usize,
    line: usize,
}

impl P {
    fn err(&self, m: &str) -> TomlError {
        TomlError { line: self.line, msg: m.to_string() }
    }
    fn eof(&self) -> bool {
        self.i >= self.c.len()
    }
    fn peek(&self) -> char {
        self.peek_at(0)
    }
    fn peek_at(&self, n: usize) -> char {
        *self.c.get(self.i + n).unwrap_or(&'\0')
    }
    fn eat(&mut self, ch: char) -> bool {
        if self.peek() == ch {
            self.i += 1;
            true
        } else {
            false
        }
    }
    fn starts(&self, s: &str) -> bool {
        s.chars().enumerate().all(|(k, ch)| self.peek_at(k) == ch)
    }
    fn skip_ws(&mut self) {
        while matches!(self.peek(), ' ' | '\t') {
            self.i += 1;
        }
    }
    fn skip_comment(&mut self) {
        if self.peek() == '#' {
            while !self.eof() && self.peek() != '\n' {
                self.i += 1;
            }
        }
    }
    fn skip_ws_nl_comments(&mut self) {
        loop {
            self.skip_ws();
            self.skip_comment();
            match self.peek() {
                '\n' => {
                    self.line += 1;
                    self.i += 1;
                }
                '\r' => self.i += 1,
                _ => break,
            }
        }
    }
    fn end_of_line(&mut self) -> Result<(), TomlError> {
        self.skip_ws();
        self.skip_comment();
        if self.eof() {
            return Ok(());
        }
        self.eat('\r');
        if self.eat('\n') {
            self.line += 1;
            Ok(())
        } else {
            Err(self.err("fin de ligne attendue"))
        }
    }
    fn key_path(&mut self) -> Result<Vec<String>, TomlError> {
        let mut out = vec![];
        loop {
            self.skip_ws();
            let k = match self.peek() {
                '"' => self.basic_string()?,
                '\'' => self.literal_string()?,
                _ => {
                    let s = self.i;
                    while self.peek().is_ascii_alphanumeric() || matches!(self.peek(), '_' | '-') {
                        self.i += 1;
                    }
                    if s == self.i {
                        return Err(self.err("clé attendue"));
                    }
                    self.c[s..self.i].iter().collect()
                }
            };
            out.push(k);
            self.skip_ws();
            if !self.eat('.') {
                break;
            }
        }
        Ok(out)
    }
    fn value(&mut self) -> Result<Value, TomlError> {
        match self.peek() {
            '"' => {
                if self.starts("\"\"\"") {
                    Ok(Value::Str(self.ml_basic()?))
                } else {
                    Ok(Value::Str(self.basic_string()?))
                }
            }
            '\'' => {
                if self.starts("'''") {
                    Ok(Value::Str(self.ml_literal()?))
                } else {
                    Ok(Value::Str(self.literal_string()?))
                }
            }
            '[' => {
                self.i += 1;
                let mut a = vec![];
                loop {
                    self.skip_ws_nl_comments();
                    if self.eat(']') {
                        break;
                    }
                    a.push(self.value()?);
                    self.skip_ws_nl_comments();
                    if self.eat(',') {
                        continue;
                    }
                    if self.eat(']') {
                        break;
                    }
                    return Err(self.err("',' ou ']' attendu"));
                }
                Ok(Value::Arr(a))
            }
            '{' => {
                self.i += 1;
                let mut o = Value::obj();
                self.skip_ws();
                if self.eat('}') {
                    return Ok(o);
                }
                loop {
                    let path = self.key_path()?;
                    self.skip_ws();
                    if !self.eat('=') {
                        return Err(self.err("'=' attendu"));
                    }
                    self.skip_ws();
                    let v = self.value()?;
                    let tbl = navigate(&mut o, &path[..path.len() - 1], self)?;
                    obj_mut(tbl).insert(path.last().unwrap().clone(), v);
                    self.skip_ws();
                    if self.eat(',') {
                        self.skip_ws();
                        continue;
                    }
                    if self.eat('}') {
                        break;
                    }
                    return Err(self.err("',' ou '}' attendu"));
                }
                Ok(o)
            }
            _ => self.scalar(),
        }
    }
    fn scalar(&mut self) -> Result<Value, TomlError> {
        let s = self.i;
        while !self.eof() && !matches!(self.peek(), ',' | ']' | '}' | '\n' | '\r' | '#') {
            self.i += 1;
        }
        let raw: String = self.c[s..self.i].iter().collect::<String>().trim().to_string();
        match raw.as_str() {
            "true" => return Ok(Value::Bool(true)),
            "false" => return Ok(Value::Bool(false)),
            "" => return Err(self.err("valeur attendue")),
            _ => {}
        }
        let clean = raw.replace('_', "");
        if let Some(h) = clean.strip_prefix("0x") {
            return i64::from_str_radix(h, 16).map(Value::Int).map_err(|_| self.err("entier hexadécimal invalide"));
        }
        if let Some(o) = clean.strip_prefix("0o") {
            return i64::from_str_radix(o, 8).map(Value::Int).map_err(|_| self.err("entier octal invalide"));
        }
        if let Some(b) = clean.strip_prefix("0b") {
            return i64::from_str_radix(b, 2).map(Value::Int).map_err(|_| self.err("entier binaire invalide"));
        }
        if let Ok(i) = clean.parse::<i64>() {
            return Ok(Value::Int(i));
        }
        if let Ok(f) = clean.parse::<f64>() {
            return Ok(Value::Num(f));
        }
        // Date/heure ou autre : conservée telle quelle.
        if raw.chars().next().map_or(false, |c| c.is_ascii_digit()) {
            return Ok(Value::Str(raw));
        }
        Err(self.err(&format!("valeur inconnue « {raw} »")))
    }
    fn escape(&mut self, out: &mut String) -> Result<(), TomlError> {
        let e = self.peek();
        self.i += 1;
        match e {
            'n' => out.push('\n'),
            't' => out.push('\t'),
            'r' => out.push('\r'),
            'b' => out.push('\u{8}'),
            'f' => out.push('\u{c}'),
            '"' => out.push('"'),
            '\\' => out.push('\\'),
            'u' | 'U' => {
                let n = if e == 'u' { 4 } else { 8 };
                let h: String = self.c[self.i..(self.i + n).min(self.c.len())].iter().collect();
                self.i += n;
                let cp = u32::from_str_radix(&h, 16).map_err(|_| self.err("\\u invalide"))?;
                out.push(char::from_u32(cp).unwrap_or('\u{FFFD}'));
            }
            _ => return Err(self.err("échappement inconnu")),
        }
        Ok(())
    }
    fn basic_string(&mut self) -> Result<String, TomlError> {
        self.i += 1;
        let mut out = String::new();
        loop {
            if self.eof() || self.peek() == '\n' {
                return Err(self.err("chaîne non terminée"));
            }
            let ch = self.peek();
            self.i += 1;
            match ch {
                '"' => break,
                '\\' => self.escape(&mut out)?,
                c => out.push(c),
            }
        }
        Ok(out)
    }
    fn literal_string(&mut self) -> Result<String, TomlError> {
        self.i += 1;
        let s = self.i;
        while !self.eof() && self.peek() != '\'' {
            if self.peek() == '\n' {
                return Err(self.err("chaîne non terminée"));
            }
            self.i += 1;
        }
        let out = self.c[s..self.i].iter().collect();
        self.i += 1;
        Ok(out)
    }
    fn ml_basic(&mut self) -> Result<String, TomlError> {
        self.i += 3;
        self.eat('\r');
        if self.eat('\n') {
            self.line += 1;
        }
        let mut out = String::new();
        loop {
            if self.eof() {
                return Err(self.err("chaîne multiligne non terminée"));
            }
            if self.starts("\"\"\"") {
                self.i += 3;
                break;
            }
            let ch = self.peek();
            self.i += 1;
            match ch {
                '\\' => {
                    if matches!(self.peek(), '\n' | ' ' | '\t' | '\r') {
                        while matches!(self.peek(), '\n' | ' ' | '\t' | '\r') {
                            if self.peek() == '\n' {
                                self.line += 1;
                            }
                            self.i += 1;
                        }
                    } else {
                        self.escape(&mut out)?;
                    }
                }
                '\n' => {
                    self.line += 1;
                    out.push('\n');
                }
                c => out.push(c),
            }
        }
        Ok(out)
    }
    fn ml_literal(&mut self) -> Result<String, TomlError> {
        self.i += 3;
        self.eat('\r');
        if self.eat('\n') {
            self.line += 1;
        }
        let s = self.i;
        while !self.eof() && !self.starts("'''") {
            if self.peek() == '\n' {
                self.line += 1;
            }
            self.i += 1;
        }
        let out = self.c[s..self.i].iter().collect();
        self.i += 3;
        Ok(out)
    }
}

/// Représentation TOML d'une valeur simple (pour l'écriture de configuration).
pub fn literal(v: &Value) -> String {
    match v {
        Value::Str(s) => crate::json::Value::Str(s.clone()).to_json(),
        Value::Bool(b) => b.to_string(),
        Value::Int(i) => i.to_string(),
        Value::Num(f) => f.to_string(),
        Value::Arr(a) => format!("[{}]", a.iter().map(literal).collect::<Vec<_>>().join(", ")),
        _ => "\"\"".into(),
    }
}

/// Modifie (ou ajoute) `key = value` dans la section `[section]` d'un texte
/// TOML, en préservant le reste du fichier et ses commentaires.
pub fn set_key_in_text(text: &str, section: &str, key: &str, value: &Value) -> String {
    let lit = format!("{key} = {}", literal(value));
    let header = format!("[{section}]");
    let lines: Vec<&str> = text.lines().collect();
    let mut out: Vec<String> = Vec::with_capacity(lines.len() + 3);
    let mut in_sec = false;
    let mut done = false;
    let mut sec_found = false;
    for (idx, l) in lines.iter().enumerate() {
        let t = l.trim();
        if t.starts_with('[') {
            if in_sec && !done {
                // insérer avant la section suivante
                while out.last().map_or(false, |x| x.trim().is_empty()) {
                    out.pop();
                }
                out.push(lit.clone());
                out.push(String::new());
                done = true;
            }
            in_sec = t == header;
            if in_sec {
                sec_found = true;
            }
        } else if in_sec && !done {
            let k = t.split('=').next().unwrap_or("").trim().trim_matches('"');
            if !t.starts_with('#') && t.contains('=') && k == key {
                out.push(lit.clone());
                done = true;
                continue;
            }
        }
        out.push(l.to_string());
        let _ = idx;
    }
    if !done {
        if !sec_found {
            if out.last().map_or(false, |x| !x.trim().is_empty()) {
                out.push(String::new());
            }
            out.push(header);
        }
        out.push(lit);
    }
    let mut s = out.join("\n");
    s.push('\n');
    s
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn parse_basic() {
        let src = r#"
# commentaire
title = "x" # fin
[general]
mode = "daemon"
n = 1_000
f = 0.5
ok = true
list = [ "a", 'b',
  "c", ]   # multi
[drives."/dev/sr0"]
profile = "auto"
[[handler.step]]
name = "read"
cmd = ["redumper", "--drive={device}"]
[[handler.step]]
name = "convert"
inline = { a = 1, b.c = "z" }
hex = 0x1C
s = '''
lit\n'''
m = """a \
   b"""
"#;
        let v = parse(src).unwrap();
        assert_eq!(v["title"].as_str(), Some("x"));
        assert_eq!(v.path("general.n").as_i64(), Some(1000));
        assert_eq!(v.path("general.list").as_arr().len(), 3);
        assert_eq!(v["drives"]["/dev/sr0"]["profile"].as_str(), Some("auto"));
        let steps = v.path("handler.step").as_arr();
        assert_eq!(steps.len(), 2);
        assert_eq!(steps[1].path("inline.b.c").as_str(), Some("z"));
        assert_eq!(steps[1]["hex"].as_i64(), Some(0x1c));
        assert_eq!(steps[1]["s"].as_str(), Some("lit\\n"));
        assert_eq!(steps[1]["m"].as_str(), Some("a b"));
    }
    #[test]
    fn duplicate_key() {
        assert!(parse("a = 1\na = 2\n").is_err());
    }
    #[test]
    fn set_key() {
        let t = "# c\n[general]\nmode = \"daemon\"\n\n[policy]\ndefault = \"ask\"\npsx = \"ask\"\n";
        let r = set_key_in_text(t, "policy", "psx", &Value::from("auto-play"));
        assert!(r.contains("psx = \"auto-play\""));
        assert!(!r.contains("psx = \"ask\""));
        let r2 = set_key_in_text(t, "policy", "gc", &Value::from("ignore"));
        assert!(parse(&r2).unwrap().path("policy.gc").as_str() == Some("ignore"));
        let r3 = set_key_in_text(t, "general", "roms_dir", &Value::from("/x"));
        let p = parse(&r3).unwrap();
        assert_eq!(p.path("general.roms_dir").as_str(), Some("/x"));
        assert_eq!(p.path("policy.psx").as_str(), Some("ask"));
        let r4 = set_key_in_text("", "policy", "kodi", &Value::from("auto-play"));
        assert_eq!(parse(&r4).unwrap().path("policy.kodi").as_str(), Some("auto-play"));
    }
}
