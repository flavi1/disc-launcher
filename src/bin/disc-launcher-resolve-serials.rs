//! Résolveur hors ligne : numéro de série / identifiant de jeu → titre.
//!
//! Lit `serials/<système>.txt` dans `~/.local/share/disc-launcher/` puis dans
//! les répertoires de données système. Formats acceptés, une entrée par ligne :
//!   GALE01 = Super Smash Bros. Melee        (format GameTDB, wiitdb.txt…)
//!   SCES-00867<TAB>Final Fantasy VII        (TSV)
//!
//! Contrat : `disc-launcher-resolve-serials resolve`, identité en JSON sur
//! stdin, `{"candidates":[{"name":…,"confidence":…}]}` sur stdout.

use disclauncher::json::{self, Value};
use disclauncher::naming::sanitize;
use disclauncher::{jobj, paths};
use std::io::Read;

fn norm(s: &str) -> String {
    s.chars().filter(|c| c.is_ascii_alphanumeric()).collect::<String>().to_ascii_uppercase()
}

fn lookup(system: &str, keys: &[String]) -> Option<String> {
    let mut files = vec![paths::user_data_dir().join("serials").join(format!("{system}.txt"))];
    for d in paths::system_data_dirs() {
        files.push(d.join("serials").join(format!("{system}.txt")));
    }
    for f in files {
        let Ok(text) = std::fs::read_to_string(&f) else { continue };
        for line in text.lines() {
            let line = line.trim();
            if line.is_empty() || line.starts_with('#') {
                continue;
            }
            let (k, v) = match line.split_once('\t').or_else(|| line.split_once(" = ")).or_else(|| line.split_once('=')) {
                Some(x) => x,
                None => continue,
            };
            let nk = norm(k);
            if keys.contains(&nk) {
                let v = v.trim();
                if !v.is_empty() {
                    return Some(v.to_string());
                }
            }
        }
    }
    None
}

fn main() {
    let mut s = String::new();
    if disclauncher::sys::stdin_ready(2000) {
        let _ = std::io::stdin().read_to_string(&mut s);
    }
    let input = json::parse(&s).unwrap_or_else(|_| Value::obj());
    let id = &input["identity"];
    let system = id["system"].str_or("");
    let mut keys = vec![];
    for k in ["serial", "game_id"] {
        if let Some(v) = id[k].as_str() {
            keys.push(norm(v));
        }
    }
    let mut cands = vec![];
    if let Some(title) = lookup(system, &keys) {
        let mut name = sanitize(&title, false);
        if let Some(r) = id["region"].as_str() {
            name.push_str(&format!(" ({r})"));
        }
        if let Some(rev) = id["revision"].as_str() {
            if rev.starts_with("Rev") {
                name.push_str(&format!(" ({rev})"));
            }
        }
        let multi = id["discs_total"].as_i64().is_some_and(|t| t > 1) || id["disc"].as_i64().is_some_and(|d| d > 1);
        if multi {
            name.push_str(&format!(" (Disc {})", id["disc"].as_i64().unwrap_or(1)));
        }
        cands.push(jobj! {"name" => name, "confidence" => "probable", "discs" => id["discs_total"].clone()});
    }
    println!("{}", jobj! {"candidates" => cands}.to_json());
}
