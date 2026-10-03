//! Disques de données « simples » : uniquement de la musique (FLAC, MP3…), ou
//! uniquement de la vidéo (MKV, DivX…). Ils sont reconnus à l'identification
//! (étiquettes `data:audio`, `data:video`) et lus par
//! `disc-launcher-data-generic`, qui établit la liste des fichiers en liste de
//! lecture et la confie au lecteur choisi dans `[media]` (comme un média).

use crate::config::Config;
use crate::fs::FileSystem;
use crate::json::Value;
use crate::{jobj, paths};
use std::path::{Path, PathBuf};

pub const AUDIO_EXT: &[&str] = &["flac", "mp3", "ogg", "oga", "opus", "m4a", "aac", "wav", "wma", "ape", "wv", "mpc", "alac", "aiff", "aif", "dsf", "dff", "mka"];
pub const VIDEO_EXT: &[&str] = &["mkv", "avi", "divx", "xvid", "mp4", "m4v", "mov", "wmv", "mpg", "mpeg", "ts", "m2ts", "webm", "ogm", "ogv", "flv", "3gp"];
/// Fichiers d'accompagnement sans incidence : pochettes, sous-titres, listes,
/// fichiers d'exécution automatique ou d'informations.
pub const IGNORED_EXT: &[&str] = &[
    "jpg", "jpeg", "png", "gif", "bmp", "webp", "txt", "nfo", "inf", "ini", "log", "cue", "m3u", "m3u8", "pls", "sfv", "md5", "sha1", "ico", "db", "url", "srt", "sub", "idx", "ass", "ssa", "vtt", "sup", "pdf", "accurip", "ffp", "st5", "xml",
];

const MAX_ENTRIES: usize = 5000;
const MAX_DEPTH: usize = 6;

fn ext_of(name: &str) -> String {
    // ISO 9660 sans Joliet : « PISTE01.MP3;1 »
    let n = name.split(';').next().unwrap_or(name);
    n.rsplit_once('.').map(|(_, e)| e.to_ascii_lowercase()).unwrap_or_default()
}

/// Nature d'un fichier : `Some("audio")`, `Some("video")`, `Some("")` (ignoré),
/// `None` (autre : programme, document… le disque n'est pas « simple »).
pub fn file_kind(name: &str) -> Option<&'static str> {
    let e = ext_of(name);
    if AUDIO_EXT.contains(&e.as_str()) {
        Some("audio")
    } else if VIDEO_EXT.contains(&e.as_str()) {
        Some("video")
    } else if IGNORED_EXT.contains(&e.as_str()) || name.starts_with('.') {
        Some("")
    } else {
        None
    }
}

/// Classe un système de fichiers : `("audio" | "video", nombre de fichiers)`.
/// Un disque mêlant audio et vidéo, ou contenant d'autres fichiers, n'est pas classé.
pub fn classify(fs: &dyn FileSystem) -> Option<(&'static str, usize)> {
    let (mut audio, mut video, mut seen) = (0usize, 0usize, 0usize);
    let mut stack: Vec<(String, usize)> = vec![(String::new(), 0)];
    while let Some((dir, depth)) = stack.pop() {
        let entries = fs.list(&dir).ok()?;
        for e in entries {
            seen += 1;
            if seen > MAX_ENTRIES {
                return None;
            }
            let path = if dir.is_empty() { e.name.clone() } else { format!("{dir}/{}", e.name) };
            if e.dir {
                // Dossiers système sans intérêt (corbeilles, vignettes)
                let lower = e.name.to_ascii_lowercase();
                if lower.starts_with('.') || lower == "$recycle.bin" || lower == "system volume information" {
                    continue;
                }
                if depth + 1 > MAX_DEPTH {
                    return None;
                }
                stack.push((path, depth + 1));
                continue;
            }
            match file_kind(&e.name) {
                Some("audio") => audio += 1,
                Some("video") => video += 1,
                Some(_) => {}
                None => return None,
            }
        }
    }
    match (audio, video) {
        (a, 0) if a > 0 => Some(("audio", a)),
        (0, v) if v > 0 => Some(("video", v)),
        _ => None,
    }
}

/// Monte le disque par udisks2 s'il ne l'est pas. Renvoie le point de montage.
pub fn ensure_mounted(device: &str) -> Option<PathBuf> {
    if let Some(mp) = crate::device::find_mount_point(device) {
        return Some(mp);
    }
    match crate::dbus::udisks_mount(device) {
        Ok(mp) if !mp.is_empty() => Some(PathBuf::from(mp)),
        Ok(_) => crate::device::find_mount_point(device),
        Err(e) => {
            crate::dl_log!(warn, "data", format!("montage impossible : {e}"), "drive" => device);
            None
        }
    }
}

/// Comparaison « naturelle » : « Piste 2 » avant « Piste 10 ».
fn natural_key(s: &str) -> Vec<(String, u64)> {
    let mut out = vec![];
    let mut text = String::new();
    let mut num = String::new();
    for c in s.chars() {
        if c.is_ascii_digit() {
            num.push(c);
        } else {
            if !num.is_empty() {
                out.push((std::mem::take(&mut text), num.parse().unwrap_or(0)));
                num.clear();
            }
            text.extend(c.to_lowercase());
        }
    }
    out.push((text, num.parse().unwrap_or(0)));
    out
}

/// Fichiers audio ou vidéo du disque monté, dans l'ordre naturel des chemins.
pub fn files(mount: &Path, kind: &str) -> Vec<PathBuf> {
    let mut out = vec![];
    let mut stack = vec![(mount.to_path_buf(), 0usize)];
    while let Some((d, depth)) = stack.pop() {
        let Ok(it) = std::fs::read_dir(&d) else { continue };
        for e in it.flatten() {
            let p = e.path();
            if p.is_dir() {
                if depth < MAX_DEPTH {
                    stack.push((p, depth + 1));
                }
            } else if file_kind(&e.file_name().to_string_lossy()) == Some(if kind == "audio" { "audio" } else { "video" }) {
                out.push(p);
            }
        }
    }
    out.sort_by_key(|p| natural_key(&p.to_string_lossy()));
    out
}

/// `data-audio` → `audio`, `data-video` → `video`.
pub fn kind_of(id: &str) -> &'static str {
    if id.ends_with("video") {
        "video"
    } else {
        "audio"
    }
}

/// Variables de gabarit : celles d'un média, plus `{playlist}` (liste M3U
/// écrite dans le dossier d'exécution), `{first}` (premier fichier), `{count}`.
pub fn vars(id: &str, device: &str) -> Vec<(String, String)> {
    let mut v = crate::media::vars(device);
    let Some(mount) = ensure_mounted(device) else { return v };
    if !v.iter().any(|(k, _)| k == "mount") {
        v.push(("mount".into(), mount.to_string_lossy().into_owned()));
    }
    let list = files(&mount, kind_of(id));
    if list.is_empty() {
        return v;
    }
    let pl = paths::runtime_dir().join(format!("playlist-{}.m3u", device.trim_start_matches("/dev/")));
    let mut text = String::from("#EXTM3U\n");
    for f in &list {
        text.push_str(&f.to_string_lossy());
        text.push('\n');
    }
    if paths::write_atomic(&pl, text.as_bytes()).is_ok() {
        v.push(("playlist".into(), pl.to_string_lossy().into_owned()));
    }
    v.push(("first".into(), list[0].to_string_lossy().into_owned()));
    v.push(("count".into(), list.len().to_string()));
    v
}

pub fn describe(cfg: &Config, id: &str, device: &str) -> Value {
    // Pas de montage ni de liste ici : seul le choix du lecteur compte, avec
    // des valeurs fictives pour que les gabarits soient applicables.
    let mut v = crate::media::vars(device);
    for k in ["mount", "playlist", "first", "count"] {
        if !v.iter().any(|(x, _)| x == k) {
            v.push((k.to_string(), "-".to_string()));
        }
    }
    let c = crate::media::choose(cfg, id, &v);
    jobj! {
        "id" => id,
        "player" => c.as_ref().map(|c| c.player.clone()),
        "player_name" => c.as_ref().map(|c| c.name.clone()),
        "actions" => jobj!{"play-disc" => c.is_some(), "play-existing" => false, "dump" => false},
        "missing" => if c.is_some() { Vec::<String>::new() } else { vec!["lecteur multimédia".to_string()] },
    }
}

pub fn play(cfg: &Config, id: &str, device: &str) -> Result<Value, String> {
    let v = vars(id, device);
    if !v.iter().any(|(k, _)| k == "playlist") {
        return Err(format!("aucun fichier {} trouvé sur le disque (monté ?)", kind_of(id)));
    }
    crate::media::play_with(cfg, id, &v)
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn kinds_and_order() {
        assert_eq!(file_kind("PISTE01.FLA"), None);
        assert_eq!(file_kind("PISTE01.MP3;1"), Some("audio"));
        assert_eq!(file_kind("film.MKV"), Some("video"));
        assert_eq!(file_kind("folder.jpg"), Some(""));
        assert_eq!(file_kind("setup.exe"), None);
        let mut v = vec!["d/10 - b.flac", "d/2 - a.flac", "d/1 - z.flac"];
        v.sort_by_key(|s| natural_key(s));
        assert_eq!(v, vec!["d/1 - z.flac", "d/2 - a.flac", "d/10 - b.flac"]);
    }
}
