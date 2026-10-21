//! Encodage des CD audio, DVD et Blu-ray vidéo avec encbd
//! (https://github.com/flavi1/encbd), s'il est installé.
//!
//! Dès qu'un de ces disques est proposé, `encbd --identify --json` donne le nom
//! prévu (TheDiscDB, métadonnées du disque, label, TMDb, Wikidata ; MusicBrainz
//! pour les CD) et signale une copie déjà présente, sans tenir compte des
//! mentions entre crochets. encbd ne lit aucune base de disc-launcher : c'est
//! disc-launcher qui lui demande son avis, puis lui impose le nom retenu
//! (`--title`/`--year`, ou `--release` pour un CD) au moment d'encoder. Le
//! résultat est enregistré dans la collection de disc-launcher, rattaché à
//! l'empreinte du disque : la copie est retrouvée au prochain passage, même
//! renommée ou hors ligne.
//!
//! ```toml
//! [encbd]
//! enabled = true                      # défaut : vrai si encbd est trouvé
//! command = ["encbd"]                 # défaut : encbd, puis encbd.appimage
//! music_dir = "~/Musique"             # défaut : dossier Musique (xdg-user-dirs)
//! video_dir = "~/Vidéos"              # défaut : dossier Vidéos (xdg-user-dirs)
//! online = false                      # --no-online (défaut : réglage d'encbd)
//! ```

use crate::config::Config;
use crate::json::Value;
use crate::paths;
use std::io::Read;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::time::{Duration, Instant};

/// Clé d'action « Encoder ».
pub const ACTION: &str = "encode-disc";
/// Clé d'action « Lire la copie ».
pub const OPEN_COPY: &str = "open-copy";

/// Nature du contenu encodé : `music` (CD audio) ou `video` (DVD, Blu-ray).
pub fn kind_for(handler: &str) -> Option<&'static str> {
    match handler {
        "cdda" => Some("music"),
        "dvd-video" | "bluray-video" => Some("video"),
        _ => None,
    }
}

/// Commande encbd : `[encbd] command`, sinon `encbd` ou `encbd.appimage` du PATH.
pub fn command(cfg: &Config) -> Option<Vec<String>> {
    let sect = cfg.raw.get("encbd");
    if !sect["enabled"].bool_or(true) {
        return None;
    }
    let custom = match sect["command"].as_str() {
        Some(s) => s.split_whitespace().map(|x| x.to_string()).collect(),
        None => sect["command"].strings(),
    };
    let mut c = if !custom.is_empty() {
        let mut c = custom;
        if c[0].starts_with('~') {
            c[0] = paths::expand(&c[0]).to_string_lossy().into_owned();
        }
        c
    } else {
        vec![["encbd", "encbd.appimage"].iter().find_map(|n| paths::which(n))?.to_string_lossy().into_owned()]
    };
    // Bases en ligne : réglage d'encbd (ONLINE_LOOKUP), sauf `online = false` ici.
    if !sect["online"].bool_or(true) {
        c.push("--no-online".into());
    }
    Some(c)
}

/// Dossier XDG de l'utilisateur (`MUSIC`, `VIDEOS`) d'après `user-dirs.dirs`.
pub fn xdg_user_dir(name: &str) -> Option<PathBuf> {
    let text = std::fs::read_to_string(paths::config_home().join("user-dirs.dirs")).ok()?;
    parse_user_dir(&text, name, &paths::home())
}

pub fn parse_user_dir(text: &str, name: &str, home: &Path) -> Option<PathBuf> {
    let key = format!("XDG_{name}_DIR=");
    let line = text.lines().map(str::trim).find(|l| l.starts_with(&key))?;
    let v = line[key.len()..].trim().trim_matches('"');
    let p = match v.strip_prefix("$HOME") {
        Some(rest) => home.join(rest.trim_start_matches('/')),
        None if v.starts_with('/') => PathBuf::from(v),
        None => return None,
    };
    // « $HOME/ » seul : dossier désactivé.
    (p != home).then_some(p)
}

/// Dossier de destination pour `kind` (`music` ou `video`).
pub fn dest_dir(cfg: &Config, kind: &str) -> PathBuf {
    let (key, xdg, fallback) = if kind == "music" { ("music_dir", "MUSIC", "Music") } else { ("video_dir", "VIDEOS", "Videos") };
    if let Some(s) = cfg.raw.get("encbd")[key].as_str().filter(|s| !s.is_empty()) {
        return paths::expand(s);
    }
    xdg_user_dir(xdg).unwrap_or_else(|| paths::home().join(fallback))
}

/// `encbd --identify --json <périphérique> <destination>`, limité à `timeout`.
pub fn identify(cmd: &[String], dev: &str, dest: &Path, timeout: Duration) -> Result<Value, String> {
    let log = paths::log_dir().join("encbd-identify.log");
    if let Some(d) = log.parent() {
        let _ = std::fs::create_dir_all(d);
    }
    let err = std::fs::OpenOptions::new().create(true).append(true).open(&log).map(Stdio::from).unwrap_or_else(|_| Stdio::null());
    let mut child = Command::new(&cmd[0])
        .args(&cmd[1..])
        .args(["--identify", "--json", dev])
        .arg(dest)
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(err)
        .spawn()
        .map_err(|e| format!("{} : {e}", cmd[0]))?;
    let mut out = child.stdout.take().ok_or("stdout")?;
    let reader = std::thread::spawn(move || {
        let mut s = String::new();
        let _ = out.read_to_string(&mut s);
        s
    });
    let start = Instant::now();
    let status = loop {
        if let Some(st) = child.try_wait().map_err(|e| e.to_string())? {
            break st;
        }
        if start.elapsed() > timeout {
            let _ = child.kill();
            let _ = child.wait();
            return Err(format!("encbd --identify : délai de {} s dépassé", timeout.as_secs()));
        }
        std::thread::sleep(Duration::from_millis(100));
    };
    let text = reader.join().unwrap_or_default();
    if !status.success() {
        return Err(format!("encbd --identify : code {} (voir {})", status.code().unwrap_or(-1), log.display()));
    }
    parse(&text)
}

/// Dernière ligne JSON de la sortie.
pub fn parse(text: &str) -> Result<Value, String> {
    let line = text.lines().rev().find(|l| l.trim_start().starts_with('{')).ok_or("encbd --identify : aucune réponse JSON")?;
    crate::json::parse(line).map_err(|e| format!("encbd --identify : {e}"))
}

/// Nom fiable (autre chose que « Sans titre » ou un album inconnu).
pub fn name_known(p: &Value) -> bool {
    let t = p["title"].str_or("");
    !t.is_empty() && p["origin"].str_or("") != "défaut" && t != "Sans titre"
}

/// Copie déjà présente signalée par encbd.
pub fn existing(p: &Value) -> Option<String> {
    p["existing"].as_arr().first().and_then(|v| v.as_str()).map(|s| s.to_string())
}

/// Ligne de commande d'encodage : le nom retenu est imposé à encbd.
pub fn encode_argv(cmd: &[String], p: Option<&Value>, dev: &str, dest: &Path) -> Vec<String> {
    let mut v = cmd.to_vec();
    if let Some(p) = p {
        if p["kind"].str_or("") == "cd" {
            if let Some(r) = p["release"].as_str().filter(|s| !s.is_empty()) {
                v.extend(["--release".into(), r.to_string()]);
            }
        } else if name_known(p) {
            v.extend(["--title".into(), p["title"].str_or("").to_string()]);
            if let Some(y) = p["year"].as_str().filter(|s| !s.is_empty()) {
                v.extend(["--year".into(), y.to_string()]);
            }
        }
    }
    v.push(dev.to_string());
    v.push(dest.to_string_lossy().into_owned());
    v
}

fn tilde(p: &str) -> String {
    let home = paths::home();
    match Path::new(p).strip_prefix(&home) {
        Ok(rest) => format!("~/{}", rest.display()),
        Err(_) => p.to_string(),
    }
}

/// Lignes affichées dans la proposition.
pub fn lines(p: &Value, known_copy: Option<&str>) -> Vec<String> {
    let mut out = vec![];
    let en = crate::notify::t("play") == "Play";
    if p["kind"].str_or("") == "cd" {
        if !p["album"].str_or("").is_empty() {
            let year = p["year"].as_str().filter(|y| !y.is_empty()).map(|y| format!(" ({y})")).unwrap_or_default();
            out.push(format!("{} — {}{year}", p["artist"].str_or("?"), p["album"].str_or("")));
        } else {
            out.push(if en { "Album not found in MusicBrainz" } else { "Album absent de MusicBrainz" }.to_string());
        }
    } else if name_known(p) {
        let year = p["year"].as_str().filter(|y| !y.is_empty()).map(|y| format!(" ({y})")).unwrap_or_default();
        out.push(format!("{}{year}  [{}]", p["title"].str_or(""), p["origin"].str_or("")));
    } else {
        out.push(if en { "Title not found: encbd will ask for it" } else { "Titre introuvable : encbd le demandera" }.to_string());
    }
    match existing(p).as_deref().or(known_copy) {
        Some(e) => out.push(format!("{} : {}", crate::notify::t("sit-known"), tilde(e))),
        None => {
            let f = p["final_path"].str_or("");
            if !f.is_empty() {
                out.push(format!("→ {}", tilde(f)));
            }
        }
    }
    out
}

/// Clé de la collection pour un disque encodé par encbd.
pub fn collection_key(fingerprint: &str) -> String {
    format!("encbd:{fingerprint}")
}

/// Enregistre une copie (existante ou à venir) dans la collection.
pub fn record(handler: &str, fingerprint: &str, path: &str, name: &str) {
    if fingerprint.is_empty() || path.is_empty() {
        return;
    }
    if let Ok(col) = crate::collection::Collection::open() {
        let e = crate::collection::Entry {
            system: handler.to_string(),
            key: Some(collection_key(fingerprint)),
            path: path.to_string(),
            canonical_name: Some(name.to_string()).filter(|s| !s.is_empty()),
            name_source: Some("encbd".into()),
            ..Default::default()
        };
        if let Err(err) = col.record_with(e, false, false) {
            crate::dl_log!(warn, "encbd", format!("collection : {err}"), "path" => path);
        }
    }
}

/// Copie connue de la collection pour ce disque (fichier toujours présent).
pub fn known_copy(fingerprint: &str) -> Option<String> {
    if fingerprint.is_empty() {
        return None;
    }
    let col = crate::collection::Collection::open().ok()?;
    col.find_key(&collection_key(fingerprint), None).map(|e| e.path)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn user_dirs() {
        let text = "# xdg\nXDG_MUSIC_DIR=\"$HOME/Musique\"\nXDG_VIDEOS_DIR=\"$HOME/\"\nXDG_PICTURES_DIR=\"/srv/photos\"\n";
        let home = Path::new("/home/u");
        assert_eq!(parse_user_dir(text, "MUSIC", home), Some(PathBuf::from("/home/u/Musique")));
        assert_eq!(parse_user_dir(text, "VIDEOS", home), None);
        assert_eq!(parse_user_dir(text, "PICTURES", home), Some(PathBuf::from("/srv/photos")));
        assert_eq!(parse_user_dir(text, "DOCUMENTS", home), None);
    }

    #[test]
    fn argv_and_lines() {
        let cmd = vec!["encbd".to_string()];
        let v = parse("ℹ️ bruit\n{\"kind\":\"bluray\",\"title\":\"Avatar\",\"year\":\"2009\",\"origin\":\"TheDiscDB\",\"final_path\":\"/v/Avatar (2009).mkv\",\"exists\":false,\"existing\":[]}\n").unwrap();
        assert_eq!(encode_argv(&cmd, Some(&v), "/dev/sr0", Path::new("/v")), ["encbd", "--title", "Avatar", "--year", "2009", "/dev/sr0", "/v"]);
        assert_eq!(lines(&v, None)[1], "→ /v/Avatar (2009).mkv");
        let unknown = parse("{\"kind\":\"dvd\",\"title\":\"Sans titre\",\"origin\":\"défaut\",\"existing\":[\"/v/x [VFF].mkv\"]}").unwrap();
        assert_eq!(encode_argv(&cmd, Some(&unknown), "/dev/sr0", Path::new("/v")), ["encbd", "/dev/sr0", "/v"]);
        assert_eq!(existing(&unknown).as_deref(), Some("/v/x [VFF].mkv"));
        let cd = parse("{\"kind\":\"cd\",\"release\":\"mb-1\",\"artist\":\"A\",\"album\":\"B\",\"year\":\"1999\",\"existing\":[]}").unwrap();
        assert_eq!(encode_argv(&cmd, Some(&cd), "/dev/sr0", Path::new("/m")), ["encbd", "--release", "mb-1", "/dev/sr0", "/m"]);
        assert_eq!(lines(&cd, Some("/m/A/B"))[0], "A — B (1999)");
        assert!(lines(&cd, Some("/m/A/B"))[1].ends_with(": /m/A/B"));
        assert_eq!(encode_argv(&cmd, None, "/dev/sr0", Path::new("/m")), ["encbd", "/dev/sr0", "/m"]);
        assert_eq!(kind_for("cdda"), Some("music"));
        assert_eq!(kind_for("psx"), None);
    }
}
