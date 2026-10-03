//! Cartouches lues par une Retrode (adaptateur USB de stockage de masse).
//!
//! La Retrode présente la cartouche insérée comme un fichier ROM à la racine
//! d'un volume (étiquette `RETRODE`, fichier `RETRODE.CFG`). Chaque fichier ROM
//! est traité comme un « lecteur » par le démon : identification par l'en-tête
//! et l'empreinte SHA-1 (base No-Intro), puis lecture par l'émulateur ou copie
//! dans `~/ROMs/<système>` (étape `disc-launcher rom copy` du plan de dump).
//!
//! Systèmes : Super Nintendo, Mega Drive / Genesis, Nintendo 64, Game Boy,
//! Game Boy Color, Game Boy Advance, Master System, Game Gear (selon les
//! adaptateurs branchés sur la Retrode).

use crate::device::{DriveInfo, MediaKind, Physical};
use crate::identify::{Confidence, Feasibility, IdentResult, Match};
use crate::identity::Identity;
use crate::util::Sha1;
use std::io;
use std::path::{Path, PathBuf};

pub const CONFIG_FILE: &str = "RETRODE.CFG";

/// Volumes Retrode montés (racine contenant `RETRODE.CFG`).
pub fn retrode_roots() -> Vec<PathBuf> {
    let mut out = vec![];
    // Dossiers supplémentaires (tests, volume monté ailleurs) : liste séparée par « : ».
    if let Ok(extra) = std::env::var("DISC_LAUNCHER_RETRODE_DIRS") {
        out.extend(extra.split(':').filter(|d| !d.is_empty()).map(PathBuf::from).filter(|p| has_config(p)));
    }
    let Ok(text) = std::fs::read_to_string("/proc/self/mountinfo") else { return out };
    for line in text.lines() {
        let Some(mp) = line.split(' ').nth(4) else { continue };
        let mp = unescape(mp);
        let p = PathBuf::from(&mp);
        if mp.starts_with("/proc") || mp.starts_with("/sys") || mp == "/" {
            continue;
        }
        if has_config(&p) && !out.contains(&p) {
            out.push(p);
        }
    }
    out
}

fn unescape(s: &str) -> String {
    let b = s.as_bytes();
    let mut out = Vec::with_capacity(b.len());
    let mut i = 0;
    while i < b.len() {
        if b[i] == b'\\' && i + 3 < b.len() {
            if let Ok(v) = u8::from_str_radix(&s[i + 1..i + 4], 8) {
                out.push(v);
                i += 4;
                continue;
            }
        }
        out.push(b[i]);
        i += 1;
    }
    String::from_utf8_lossy(&out).into_owned()
}

fn has_config(root: &Path) -> bool {
    std::fs::read_dir(root).map(|it| it.flatten().any(|e| e.file_name().to_string_lossy().eq_ignore_ascii_case(CONFIG_FILE))).unwrap_or(false)
}

/// Périphérique bloc d'une Retrode branchée mais pas montée (étiquette RETRODE).
pub fn unmounted_retrode() -> Option<String> {
    let link = Path::new("/dev/disk/by-label/RETRODE");
    let dev = std::fs::canonicalize(link).ok()?;
    let dev = dev.to_string_lossy().into_owned();
    if crate::device::find_mount_point(&dev).is_some() {
        return None;
    }
    Some(dev)
}

/// Extensions → système, d'après `RETRODE.CFG` (`[snesRomExt] sfc`…), avec les
/// valeurs d'usine par défaut.
pub fn ext_map(root: &Path) -> Vec<(String, &'static str)> {
    let mut m: Vec<(String, &'static str)> = vec![
        ("sfc".into(), "snes"),
        ("smc".into(), "snes"),
        ("bin".into(), "megadrive"),
        ("n64".into(), "n64"),
        ("z64".into(), "n64"),
        ("v64".into(), "n64"),
        ("gb".into(), "gb"),
        ("gbc".into(), "gbc"),
        ("gba".into(), "gba"),
        ("sms".into(), "mastersystem"),
        ("gg".into(), "gamegear"),
    ];
    let cfg = std::fs::read_dir(root).ok().and_then(|it| it.flatten().find(|e| e.file_name().to_string_lossy().eq_ignore_ascii_case(CONFIG_FILE))).and_then(|e| std::fs::read_to_string(e.path()).ok());
    if let Some(text) = cfg {
        for line in text.lines() {
            let line = line.split(';').next().unwrap_or("").trim();
            let Some(rest) = line.strip_prefix('[') else { continue };
            let Some((key, val)) = rest.split_once(']') else { continue };
            let sys = match key.trim() {
                "snesRomExt" => "snes",
                "segaRomExt" => "megadrive",
                "n64RomExt" => "n64",
                "gbRomExt" => "gb",
                "gbaRomExt" => "gba",
                "smsRomExt" => "mastersystem",
                "ggRomExt" => "gamegear",
                _ => continue,
            };
            let ext = val.trim().trim_start_matches('.').to_ascii_lowercase();
            if !ext.is_empty() {
                m.retain(|(e, _)| *e != ext);
                m.insert(0, (ext, sys));
            }
        }
    }
    m
}

/// Fichiers ROM à la racine d'un volume Retrode (ni configuration ni sauvegarde).
pub fn rom_files(root: &Path) -> Vec<PathBuf> {
    let map = ext_map(root);
    let mut v: Vec<PathBuf> = std::fs::read_dir(root)
        .map(|it| {
            it.flatten()
                .map(|e| e.path())
                .filter(|p| p.is_file())
                .filter(|p| {
                    let ext = p.extension().map(|e| e.to_string_lossy().to_ascii_lowercase()).unwrap_or_default();
                    map.iter().any(|(e, _)| *e == ext)
                })
                .collect()
        })
        .unwrap_or_default();
    v.sort();
    v
}

/// La ROM vient-elle d'une Retrode (volume avec `RETRODE.CFG`) ?
pub fn is_cart_path(p: &str) -> bool {
    !p.starts_with("/dev/") && Path::new(p).parent().is_some_and(has_config)
}

// ------------------------------------------------------------------ en-têtes

#[derive(Clone, Debug, Default, PartialEq)]
pub struct Rom {
    pub system: String,
    pub title: Option<String>,
    /// Code de jeu (N64 « NKTP », GBA « AMKP »).
    pub code: Option<String>,
    pub region: Option<String>,
    pub revision: Option<String>,
    /// SHA-1 du contenu normalisé (celui des bases No-Intro).
    pub sha1: String,
    pub size: u64,
}

/// Extension de la copie, comme les bases No-Intro et ES-DE.
pub fn target_ext(system: &str) -> &'static str {
    match system {
        "n64" => "z64",
        "snes" => "sfc",
        "megadrive" => "md",
        "gb" => "gb",
        "gbc" => "gbc",
        "gba" => "gba",
        "mastersystem" => "sms",
        "gamegear" => "gg",
        _ => "bin",
    }
}

fn text(b: &[u8]) -> Option<String> {
    let s: String = b.iter().map(|&c| if (0x20..0x7f).contains(&c) { c as char } else { ' ' }).collect();
    let s = s.split_whitespace().collect::<Vec<_>>().join(" ");
    Some(s).filter(|s| !s.is_empty())
}

fn n64_region(c: u8) -> Option<&'static str> {
    Some(match c {
        b'E' => "USA",
        b'J' => "Japan",
        b'P' | b'X' | b'Y' => "Europe",
        b'D' => "Germany",
        b'F' => "France",
        b'I' => "Italy",
        b'S' => "Spain",
        b'U' => "Australia",
        b'A' => "Asia",
        _ => return None,
    })
}

/// Ordre des octets d'une ROM N64 ramené au format « z64 » (gros-boutiste).
pub fn n64_to_z64(b: &mut [u8]) {
    if b.len() < 4 {
        return;
    }
    match b[..4] {
        [0x37, 0x80, 0x40, 0x12] => {
            // v64 : octets échangés deux à deux
            for c in b.chunks_exact_mut(2) {
                c.swap(0, 1);
            }
        }
        [0x40, 0x12, 0x37, 0x80] => {
            // n64 : mots de 32 bits petit-boutistes
            for c in b.chunks_exact_mut(4) {
                c.reverse();
            }
        }
        _ => {}
    }
}

fn snes_header(b: &[u8]) -> Option<usize> {
    let mut best = None;
    for base in [0x7FC0usize, 0xFFC0, 0x40FFC0] {
        if b.len() < base + 0x40 {
            continue;
        }
        let comp = u16::from_le_bytes([b[base + 0x1C], b[base + 0x1D]]);
        let sum = u16::from_le_bytes([b[base + 0x1E], b[base + 0x1F]]);
        if comp ^ sum == 0xFFFF {
            return Some(base);
        }
        if best.is_none() && text(&b[base..base + 21]).is_some() {
            best = Some(base);
        }
    }
    best
}

/// Système d'après le contenu, quand l'extension ne suffit pas.
pub fn sniff(b: &[u8]) -> Option<&'static str> {
    if b.len() >= 4 && matches!(b[..4], [0x80, 0x37, 0x12, 0x40] | [0x37, 0x80, 0x40, 0x12] | [0x40, 0x12, 0x37, 0x80]) {
        return Some("n64");
    }
    if b.len() > 0xBD && b[0xB2] == 0x96 && b[4..8] == [0x24, 0xFF, 0xAE, 0x51] {
        return Some("gba");
    }
    if b.len() > 0x150 && b[0x104..0x108] == [0xCE, 0xED, 0x66, 0x66] {
        return Some(if b[0x143] & 0x80 != 0 { "gbc" } else { "gb" });
    }
    if b.len() > 0x200 && &b[0x100..0x104] == b"SEGA" {
        return Some("megadrive");
    }
    for off in [0x7FF0usize, 0x3FF0, 0x1FF0] {
        if b.len() > off + 16 && &b[off..off + 8] == b"TMR SEGA" {
            return Some(if matches!(b[off + 15] >> 4, 5..=7) { "gamegear" } else { "mastersystem" });
        }
    }
    None
}

/// Lit, normalise et décrit une ROM. `hint` : système d'après l'extension.
pub fn inspect_bytes(mut b: Vec<u8>, hint: Option<&str>) -> (Rom, Vec<u8>) {
    let size = b.len() as u64;
    let mut rom = parse_header(&mut b, size, hint);
    let mut h = Sha1::new();
    h.update(&b);
    rom.sha1 = h.hex();
    rom.size = b.len() as u64;
    (rom, b)
}

/// Octets à lire pour l'en-tête, selon le système présumé (extension).
/// Lire toute la ROM sur une Retrode revient à la dumper : on s'en tient au début.
pub fn head_len(hint: Option<&str>) -> usize {
    match hint {
        Some("n64") => 0x1000,
        Some("gb") | Some("gbc") => 0x200,
        Some("gba") => 0x200,
        Some("megadrive") => 0x200,
        Some("mastersystem") | Some("gamegear") => 0x8000,
        _ => 0x10200, // SNES (en-tête HiROM à 0xFFC0, plus un éventuel en-tête de copieur)
    }
}

/// Analyse l'en-tête (début de ROM, ou ROM entière) et normalise `b` en place
/// (ordre d'octets N64, en-tête de copieur SNES). `size` : taille du fichier.
/// Le SHA-1 n'est pas calculé ici.
pub fn parse_header(b: &mut Vec<u8>, size: u64, hint: Option<&str>) -> Rom {
    let mut system = sniff(&b).map(|s| s.to_string()).or_else(|| hint.map(|s| s.to_string())).unwrap_or_else(|| "snes".into());
    // Game Boy : une cartouche compatible Color va dans « gbc » (classement No-Intro).
    if system == "gb" && b.len() > 0x143 && b[0x143] & 0x80 != 0 {
        system = "gbc".into();
    }
    let mut rom = Rom { system: system.clone(), ..Default::default() };
    match system.as_str() {
        "n64" => {
            n64_to_z64(b);
            if b.len() >= 0x40 {
                rom.title = text(&b[0x20..0x34]);
                rom.code = text(&b[0x3B..0x3F]).filter(|c| c.len() == 4);
                rom.region = n64_region(b[0x3E]).map(|s| s.to_string());
                if b[0x3F] > 0 {
                    rom.revision = Some(format!("Rev {}", b[0x3F]));
                }
            }
        }
        "snes" => {
            // En-tête de copieur de 512 octets : absent des bases No-Intro.
            if size % 1024 == 512 && b.len() >= 512 {
                b.drain(..512);
            }
            if let Some(h) = snes_header(&b) {
                rom.title = text(&b[h..h + 21]);
                rom.region = Some(
                    match b[h + 0x19] {
                        0 => "Japan",
                        1 | 0x0F => "USA",
                        2 | 3 | 4 | 5 | 7 | 0x11 => "Europe",
                        6 => "France",
                        8 => "Spain",
                        9 => "Germany",
                        10 => "Italy",
                        13 => "Korea",
                        16 => "Brazil",
                        _ => "",
                    }
                    .to_string(),
                )
                .filter(|s| !s.is_empty());
                if b[h + 0x1B] > 0 {
                    rom.revision = Some(format!("Rev {}", b[h + 0x1B]));
                }
            }
        }
        "megadrive" => {
            if b.len() > 0x200 {
                rom.title = text(&b[0x150..0x180]).or_else(|| text(&b[0x120..0x150]));
                let r = String::from_utf8_lossy(&b[0x1F0..0x1F3]).to_string();
                rom.region = if r.contains('E') && !r.contains('U') && !r.contains('J') {
                    Some("Europe".into())
                } else if r.contains('U') && !r.contains('E') && !r.contains('J') {
                    Some("USA".into())
                } else if r.starts_with('J') && r.trim() == "J" {
                    Some("Japan".into())
                } else {
                    None
                };
            }
        }
        "gb" | "gbc" => {
            if b.len() > 0x14C {
                let end = if b[0x143] & 0x80 != 0 { 0x13F } else { 0x144 };
                rom.title = text(&b[0x134..end]);
                rom.region = if b[0x14A] == 0 { Some("Japan".into()) } else { None };
                if b[0x14C] > 0 {
                    rom.revision = Some(format!("Rev {}", b[0x14C]));
                }
            }
        }
        "gba" => {
            if b.len() > 0xBD {
                rom.title = text(&b[0xA0..0xAC]);
                rom.code = text(&b[0xAC..0xB0]).filter(|c| c.len() == 4);
                rom.region = rom.code.as_deref().and_then(|c| n64_region(c.as_bytes()[3])).map(|s| s.to_string());
                if b[0xBC] > 0 {
                    rom.revision = Some(format!("Rev {}", b[0xBC]));
                }
            }
        }
        "mastersystem" | "gamegear" => {
            for off in [0x7FF0usize, 0x3FF0, 0x1FF0] {
                if b.len() > off + 16 && &b[off..off + 8] == b"TMR SEGA" {
                    rom.region = match b[off + 15] >> 4 {
                        3 | 5 => Some("Japan".into()),
                        _ => None,
                    };
                    break;
                }
            }
        }
        _ => {}
    }
    rom.size = if system == "snes" && size % 1024 == 512 { size - 512 } else { size };
    rom
}

/// Système d'un fichier d'après son extension (carte de la Retrode).
pub fn hint_for(path: &Path) -> Option<&'static str> {
    let ext = path.extension().map(|e| e.to_string_lossy().to_ascii_lowercase()).unwrap_or_default();
    let root = path.parent().unwrap_or(Path::new("/"));
    ext_map(root).into_iter().find(|(e, _)| *e == ext).map(|(_, s)| s)
}

pub fn inspect(path: &Path) -> io::Result<(Rom, Vec<u8>)> {
    let b = std::fs::read(path)?;
    if b.is_empty() {
        return Err(io::Error::new(io::ErrorKind::InvalidData, "fichier vide (cartouche mal insérée ?)"));
    }
    Ok(inspect_bytes(b, hint_for(path)))
}

/// Début d'un fichier ROM, sans lire le reste.
pub fn read_head(path: &Path, n: usize) -> io::Result<Vec<u8>> {
    use std::io::Read;
    let mut f = std::fs::File::open(path)?;
    let mut b = Vec::with_capacity(n);
    f.by_ref().take(n as u64).read_to_end(&mut b)?;
    Ok(b)
}

fn cache_path() -> PathBuf {
    crate::paths::user_cache_dir().join("carts.tsv")
}

fn hex_sha1(b: &[u8]) -> String {
    let mut h = Sha1::new();
    h.update(b);
    h.hex()
}

/// Empreinte complète d'une cartouche déjà dumpée, retrouvée sans la relire :
/// clé = (nom du fichier présenté par la Retrode, taille). Si plusieurs
/// empreintes partagent cette clé (révisions de même titre et même taille),
/// aucune n'est retenue.
pub fn cached_sha1(file_name: &str, size: u64) -> Option<String> {
    let text = std::fs::read_to_string(cache_path()).ok()?;
    let mut found: Vec<String> = vec![];
    for l in text.lines() {
        let f: Vec<&str> = l.split('\t').collect();
        if f.len() >= 3 && f[0] == file_name && f[1] == size.to_string() && !found.iter().any(|x| x == f[2]) {
            found.push(f[2].to_string());
        }
    }
    if found.len() == 1 {
        found.pop()
    } else {
        None
    }
}

fn cache_put(file_name: &str, size: u64, full: &str) {
    let p = cache_path();
    if let Ok(text) = std::fs::read_to_string(&p) {
        if text.lines().any(|l| l == format!("{file_name}\t{size}\t{full}")) {
            return;
        }
    }
    if let Some(d) = p.parent() {
        let _ = std::fs::create_dir_all(d);
    }
    use std::io::Write;
    if let Ok(mut f) = std::fs::OpenOptions::new().create(true).append(true).open(&p) {
        let _ = writeln!(f, "{}\t{size}\t{full}", file_name.replace(['\t', '\n'], " "));
    }
}

/// Titre d'après le nom de fichier de la Retrode, tiré de l'en-tête de la
/// cartouche sans espaces : « MortalKombat » → « Mortal Kombat »,
/// « Mariokart64 » → « Mariokart 64 ». Un éventuel suffixe de somme de
/// contrôle (`[filenameChksum] 1`) est retiré.
pub fn title_from_file_name(stem: &str) -> String {
    let mut base = stem.replace('_', " ");
    // « Nom ABCD » / « Nom-1A2B3C4D » : somme hexadécimale en fin de nom.
    if let Some((head, tail)) = base.rsplit_once([' ', '-']) {
        if (4..=8).contains(&tail.len()) && tail.chars().all(|c| c.is_ascii_hexdigit()) && tail.chars().any(|c| c.is_ascii_digit()) && !head.is_empty() {
            base = head.to_string();
        }
    }
    let chars: Vec<char> = base.chars().collect();
    let mut out = String::new();
    for (i, &c) in chars.iter().enumerate() {
        if i > 0 {
            let p = chars[i - 1];
            let next_lower = chars.get(i + 1).is_some_and(|n| n.is_lowercase());
            let boundary = (p.is_lowercase() && c.is_uppercase())
                || (p.is_alphabetic() && c.is_ascii_digit())
                || (p.is_ascii_digit() && c.is_alphabetic())
                // « SNESGame » → « SNES Game »
                || (p.is_uppercase() && c.is_uppercase() && next_lower);
            if boundary && !out.ends_with(' ') {
                out.push(' ');
            }
        }
        out.push(c);
    }
    out.split_whitespace().collect::<Vec<_>>().join(" ")
}

/// Copie normalisée (`z64` pour la N64, sans en-tête de copieur pour la SNES).
/// Renvoie le SHA-1 du fichier écrit, et le mémorise pour les insertions
/// suivantes de la même cartouche.
pub fn copy(src: &Path, dst: &Path) -> io::Result<String> {
    let raw = std::fs::read(src)?;
    if raw.is_empty() {
        return Err(io::Error::new(io::ErrorKind::InvalidData, "fichier vide (cartouche mal insérée ?)"));
    }
    let hint = hint_for(src);
    let size = raw.len() as u64;
    let (rom, bytes) = inspect_bytes(raw, hint);
    cache_put(&src.file_name().unwrap_or_default().to_string_lossy(), size, &rom.sha1);
    if let Some(d) = dst.parent() {
        std::fs::create_dir_all(d)?;
    }
    let tmp = dst.with_extension("part");
    std::fs::write(&tmp, &bytes)?;
    std::fs::rename(&tmp, dst)?;
    Ok(rom.sha1)
}

/// Résultat d'identification d'une cartouche, au format des disques.
///
/// **Aucun octet du fichier n'est lu** : sur la Retrode, lire ne serait-ce que
/// le début du fichier déclenche le dump de toute la cartouche. Seuls comptent
/// le nom du fichier (titre tiré de l'en-tête par la Retrode), son extension
/// (système) et sa taille. L'empreinte complète vient du cache si la
/// cartouche a déjà été dumpée.
pub fn identify(path: &str) -> io::Result<IdentResult> {
    let p = Path::new(path);
    let size = std::fs::metadata(p)?.len();
    if size == 0 {
        return Err(io::Error::new(io::ErrorKind::InvalidData, "fichier vide (cartouche mal insérée ?)"));
    }
    let file_name = p.file_name().map(|s| s.to_string_lossy().into_owned()).unwrap_or_default();
    let stem = p.file_stem().map(|s| s.to_string_lossy().into_owned()).unwrap_or_default();
    let system = hint_for(p).unwrap_or("snes").to_string();
    let title = Some(title_from_file_name(&stem)).filter(|t| !t.is_empty());
    let full = cached_sha1(&file_name, size);
    let fp = hex_sha1(format!("{system}|{file_name}|{size}").as_bytes());
    let mut m = Match::new(&format!("console:{system}"), Confidence::Certain, "cart-file-name");
    m.primary = true;
    m.identity = Identity {
        system: system.clone(),
        title: title.clone(),
        label: Some(stem.clone()),
        // Clé stable d'une insertion à l'autre : nom de fichier et taille.
        toc_fp: Some(fp[..12].to_string()),
        sha1: full,
        disc: Some(1),
        ..Default::default()
    };
    Ok(IdentResult {
        device: Some(path.to_string()),
        drive: Some(DriveInfo { vendor: "Retrode".into(), model: "cartouche".into(), firmware: String::new() }),
        profile: "standard".into(),
        physical: Physical { media: MediaKind::Cart, profile: 0, recordable: false, blank: false, sessions: 0, tracks: vec![], leadout: 0, disc_type: None, capacity: size.div_ceil(2048) },
        fingerprint: fp[..16].to_string(),
        filesystem: None,
        volume_id: title,
        matches: vec![m],
        warnings: vec![],
        feasibility: Feasibility { read: true, dump: true, dump_usable: true },
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn n64_rom() -> Vec<u8> {
        let mut b = vec![0u8; 0x1000];
        b[..4].copy_from_slice(&[0x80, 0x37, 0x12, 0x40]);
        b[0x20..0x2B].copy_from_slice(b"MARIOKART64");
        b[0x3B..0x3F].copy_from_slice(b"NKTP");
        b
    }

    #[test]
    fn n64_byte_orders_give_same_hash() {
        let z = n64_rom();
        let (rz, _) = inspect_bytes(z.clone(), Some("n64"));
        assert_eq!(rz.system, "n64");
        assert_eq!(rz.title.as_deref(), Some("MARIOKART64"));
        assert_eq!(rz.code.as_deref(), Some("NKTP"));
        assert_eq!(rz.region.as_deref(), Some("Europe"));
        let mut n = z.clone();
        for c in n.chunks_exact_mut(4) {
            c.reverse();
        }
        let mut v = z.clone();
        for c in v.chunks_exact_mut(2) {
            c.swap(0, 1);
        }
        assert_eq!(inspect_bytes(n, Some("n64")).0.sha1, rz.sha1);
        assert_eq!(inspect_bytes(v, None).0.sha1, rz.sha1);
    }

    #[test]
    fn titles_from_retrode_names() {
        assert_eq!(title_from_file_name("Mariokart64"), "Mariokart 64");
        assert_eq!(title_from_file_name("MortalKombat"), "Mortal Kombat");
        assert_eq!(title_from_file_name("SuperMarioWorld"), "Super Mario World");
        assert_eq!(title_from_file_name("NHLHockey94"), "NHL Hockey 94");
        assert_eq!(title_from_file_name("Sonic2_A1B2"), "Sonic 2");
    }

    #[test]
    fn gameboy_color_and_config() {
        let mut b = vec![0u8; 0x8000];
        b[0x104..0x108].copy_from_slice(&[0xCE, 0xED, 0x66, 0x66]);
        b[0x134..0x13B].copy_from_slice(b"POKEMON");
        b[0x143] = 0x80;
        b[0x14A] = 1;
        let (r, _) = inspect_bytes(b, Some("gb"));
        assert_eq!(r.system, "gbc");
        assert_eq!(r.title.as_deref(), Some("POKEMON"));

        let dir = std::env::temp_dir().join(format!("dl-retrode-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(dir.join("RETRODE.CFG"), "[snesRomExt] smc ; ext\n[n64RomExt] n64\n").unwrap();
        // Contenu propre à cette exécution : le cache des empreintes est partagé.
        let mut rom = n64_rom();
        let stamp = format!("{:?}", std::time::SystemTime::now());
        rom[0x100..0x100 + stamp.len().min(64)].copy_from_slice(&stamp.as_bytes()[..stamp.len().min(64)]);
        // Nom propre à cette exécution : le cache des empreintes est partagé.
        let name = format!("Mariokart{}.n64", std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).unwrap().as_nanos());
        let rp = dir.join(&name);
        std::fs::write(&rp, rom).unwrap();
        std::fs::write(dir.join("Game.srm"), b"save").unwrap();
        assert!(ext_map(&dir).iter().any(|(e, s)| e == "smc" && *s == "snes"));
        assert_eq!(rom_files(&dir), vec![rp.clone()]);
        assert!(is_cart_path(&rp.to_string_lossy()));
        let r = identify(&rp.to_string_lossy()).unwrap();
        assert_eq!(r.primary().unwrap().tag, "console:n64");
        assert_eq!(r.primary().unwrap().identity.sha1, None);
        let out = dir.join("out/M.z64");
        let h = copy(&rp, &out).unwrap();
        assert_eq!(std::fs::read(&out).unwrap()[..4], [0x80, 0x37, 0x12, 0x40]);
        // Insertion suivante : empreinte complète retrouvée sans relire la ROM.
        let r2 = identify(&rp.to_string_lossy()).unwrap();
        assert_eq!(r2.primary().unwrap().identity.sha1.as_deref(), Some(h.as_str()));
        let _ = std::fs::remove_dir_all(&dir);
    }
}
