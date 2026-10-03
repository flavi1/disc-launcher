//! Lecteur XML minimal, suffisant pour les fichiers DAT (Redump, Logiqx)
//! et les réglages ES-DE : balises, attributs, texte, entités.

#[derive(Debug, Clone, PartialEq)]
pub enum Token {
    Open { name: String, attrs: Vec<(String, String)>, self_closing: bool },
    Close(String),
    Text(String),
}

pub fn decode_entities(s: &str) -> String {
    if !s.contains('&') {
        return s.to_string();
    }
    let mut out = String::with_capacity(s.len());
    let mut rest = s;
    while let Some(i) = rest.find('&') {
        out.push_str(&rest[..i]);
        rest = &rest[i..];
        let Some(j) = rest.find(';') else {
            out.push_str(rest);
            return out;
        };
        let ent = &rest[1..j];
        let rep = match ent {
            "amp" => Some('&'),
            "lt" => Some('<'),
            "gt" => Some('>'),
            "quot" => Some('"'),
            "apos" => Some('\''),
            e if e.starts_with("#x") || e.starts_with("#X") => u32::from_str_radix(&e[2..], 16).ok().and_then(char::from_u32),
            e if e.starts_with('#') => e[1..].parse::<u32>().ok().and_then(char::from_u32),
            _ => None,
        };
        match rep {
            Some(c) => {
                out.push(c);
                rest = &rest[j + 1..];
            }
            None => {
                out.push('&');
                rest = &rest[1..];
            }
        }
    }
    out.push_str(rest);
    out
}

/// Découpe un document en jetons (ignore commentaires, déclarations, CDATA → texte).
pub fn tokenize(src: &str) -> Vec<Token> {
    let mut out = vec![];
    let b = src.as_bytes();
    let mut i = 0;
    while i < b.len() {
        if b[i] == b'<' {
            if src[i..].starts_with("<!--") {
                i = src[i..].find("-->").map(|j| i + j + 3).unwrap_or(b.len());
                continue;
            }
            if src[i..].starts_with("<![CDATA[") {
                let s = i + 9;
                let e = src[s..].find("]]>").map(|j| s + j).unwrap_or(b.len());
                out.push(Token::Text(src[s..e].to_string()));
                i = (e + 3).min(b.len());
                continue;
            }
            if src[i..].starts_with("<?") || src[i..].starts_with("<!") {
                i = src[i..].find('>').map(|j| i + j + 1).unwrap_or(b.len());
                continue;
            }
            // fin de balise en tenant compte des guillemets
            let mut j = i + 1;
            let mut q: Option<u8> = None;
            while j < b.len() {
                match (q, b[j]) {
                    (None, b'"') | (None, b'\'') => q = Some(b[j]),
                    (Some(c), x) if c == x => q = None,
                    (None, b'>') => break,
                    _ => {}
                }
                j += 1;
            }
            let inner = &src[i + 1..j.min(b.len())];
            i = j + 1;
            if let Some(name) = inner.strip_prefix('/') {
                out.push(Token::Close(name.trim().to_string()));
                continue;
            }
            let self_closing = inner.ends_with('/');
            let inner = inner.trim_end_matches('/');
            let mut chars = inner.char_indices();
            let name_end = chars.find(|(_, c)| c.is_whitespace()).map(|(k, _)| k).unwrap_or(inner.len());
            let name = inner[..name_end].to_string();
            let attrs = parse_attrs(&inner[name_end..]);
            out.push(Token::Open { name, attrs, self_closing });
        } else {
            let e = src[i..].find('<').map(|j| i + j).unwrap_or(b.len());
            let t = &src[i..e];
            if !t.trim().is_empty() {
                out.push(Token::Text(decode_entities(t)));
            }
            i = e;
        }
    }
    out
}

fn parse_attrs(s: &str) -> Vec<(String, String)> {
    let mut v = vec![];
    let c: Vec<char> = s.chars().collect();
    let mut i = 0;
    while i < c.len() {
        while i < c.len() && c[i].is_whitespace() {
            i += 1;
        }
        let ks = i;
        while i < c.len() && c[i] != '=' && !c[i].is_whitespace() {
            i += 1;
        }
        let key: String = c[ks..i].iter().collect();
        while i < c.len() && c[i].is_whitespace() {
            i += 1;
        }
        if i >= c.len() || c[i] != '=' {
            if !key.is_empty() {
                v.push((key, String::new()));
            }
            continue;
        }
        i += 1;
        while i < c.len() && c[i].is_whitespace() {
            i += 1;
        }
        if i < c.len() && (c[i] == '"' || c[i] == '\'') {
            let q = c[i];
            i += 1;
            let vs = i;
            while i < c.len() && c[i] != q {
                i += 1;
            }
            let val: String = c[vs..i].iter().collect();
            i += 1;
            v.push((key, decode_entities(&val)));
        } else {
            let vs = i;
            while i < c.len() && !c[i].is_whitespace() {
                i += 1;
            }
            v.push((key, c[vs..i].iter().collect()));
        }
    }
    v
}

pub fn attr<'a>(attrs: &'a [(String, String)], k: &str) -> Option<&'a str> {
    attrs.iter().find(|(a, _)| a == k).map(|(_, v)| v.as_str())
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn tokens() {
        let t = tokenize(r#"<?xml version="1.0"?><!-- c --><datafile><game name="A &amp; B (Disc 1)"><rom name="x.bin" size="12"/><serial>SLUS-1</serial></game></datafile>"#);
        assert!(matches!(&t[1], Token::Open{name, attrs, ..} if name == "game" && attr(attrs, "name") == Some("A & B (Disc 1)")));
        assert!(matches!(&t[2], Token::Open{self_closing: true, ..}));
        assert_eq!(t[4], Token::Text("SLUS-1".into()));
    }
}
