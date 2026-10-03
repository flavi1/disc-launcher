//! Sondes en code : structure de la TOC, Jaguar CD, Xbox, PS3, partition
//! vidéo Xbox, disques illisibles, heuristique PC ; analyseurs d'en-têtes
//! appelés par les signatures (`parser = "…"`).

use super::{Confidence, Ctx, Match};
use crate::device::{MediaKind, Track};
use crate::fs::xdvdfs::{self, Xdvdfs};
use crate::fs::FileSystem;
use crate::identity::{region_from_nintendo_id, region_from_sega_area, region_from_sony_serial};
use crate::util::find_bytes;
use std::collections::BTreeMap;

/// Structure audio/données : CD audio, CD-Extra, Mixed Mode, audio en piste 1.
pub fn toc_structure(ctx: &Ctx, out: &mut Vec<Match>) {
    let p = ctx.phys;
    if p.media != MediaKind::Cd || p.tracks.is_empty() {
        return;
    }
    let audio = p.audio_tracks();
    let data = p.data_tracks();
    if audio > 0 && data == 0 {
        out.push(Match::new("audio:cdda", Confidence::Certain, "toc-audio-only"));
        return;
    }
    if audio == 0 {
        return;
    }
    let first = &p.tracks[0];
    if first.data {
        // Mixed Mode : données en piste 1 puis pistes audio (jeux, CD-ROM PC).
        out.push(Match::new("data", Confidence::Strong, "toc-mixed-mode"));
        out.push(Match::new("audio:cdda-mixed", Confidence::Strong, "toc-mixed-mode"));
        return;
    }
    let s1_all_audio = p.tracks.iter().filter(|t| t.session == 1).all(|t| !t.data);
    let last_is_data_later_session = p.tracks.last().map_or(false, |t| t.data && t.session > 1);
    if p.sessions >= 2 && s1_all_audio && last_is_data_later_session {
        // Enhanced CD / CD-Extra / CD-Plus.
        out.push(Match::new("audio:cdda", Confidence::Certain, "toc-cd-extra"));
        out.push(Match::new("data", Confidence::Strong, "toc-cd-extra"));
    } else {
        // Audio en piste 1 puis données dans la même session (PC Engine CD…).
        out.push(Match::new("audio:cdda", Confidence::Strong, "toc-audio-first"));
        out.push(Match::new("data", Confidence::Strong, "toc-audio-first"));
    }
}

fn swap16(b: &[u8]) -> Vec<u8> {
    let mut v = b.to_vec();
    for c in v.chunks_mut(2) {
        if c.len() == 2 {
            c.swap(0, 1);
        }
    }
    v
}

/// Atari Jaguar CD : données cachées dans les pistes audio de la 2e session.
pub fn jaguar(ctx: &Ctx, out: &mut Vec<Match>) {
    let p = ctx.phys;
    if p.media != MediaKind::Cd || p.sessions < 2 {
        return;
    }
    let Some(t) = p.tracks.iter().find(|t| t.session == 2 && !t.data) else { return };
    let needle = b"ATARI APPROVED DATA HEADER ATRI";
    let swapped = swap16(needle);
    for s in 0..32u32.min(t.length.max(1)) {
        if let Some(raw) = ctx.sector(t.start + s, true) {
            if find_bytes(&raw, needle).is_some() || find_bytes(&raw, &swapped).is_some() || find_bytes(&raw, &swapped[1..swapped.len() - 1]).is_some() {
                let mut m = Match::new("console:atarijaguarcd", Confidence::Certain, "jaguar-atri");
                m.identity.discs_total = None;
                out.push(m);
                return;
            }
        }
    }
}

/// Xbox / Xbox 360 : système de fichiers XDVDFS (lecteur flashé ou image).
pub fn xbox(ctx: &Ctx, out: &mut Vec<Match>, _profile: &str) {
    if !matches!(ctx.phys.media, MediaKind::Dvd | MediaKind::Unknown) {
        return;
    }
    let Some(x) = Xdvdfs::find(ctx.src) else { return };
    let entries = x.list("").unwrap_or_default();
    let has = |n: &str| entries.iter().any(|e| e.name.eq_ignore_ascii_case(n));
    if has("default.xbe") && !has("default.xex") && x.generation != "xgd2" && x.generation != "xgd3" {
        let mut m = Match::new("console:xbox", Confidence::Certain, "xdvdfs-xbe");
        if let Some(info) = x.read("default.xbe", 64 * 1024).ok().and_then(|b| xdvdfs::parse_xbe(&b)) {
            m.identity.serial = Some(xdvdfs::title_id_serial(info.title_id));
            m.identity.game_id = Some(format!("{:08X}", info.title_id));
            if !info.title.is_empty() {
                m.identity.title = Some(info.title);
            }
            m.identity.region = Some(
                match info.region_flags & 7 {
                    1 => "USA",
                    2 => "Japan",
                    4 => "Europe",
                    7 => "World",
                    5 => "USA, Europe",
                    3 => "Japan, USA",
                    _ => "World",
                }
                .into(),
            );
        }
        out.push(m);
    } else {
        out.push(Match::new("console:xbox360", Confidence::Certain, "xdvdfs-xex"));
    }
}

fn le16(b: &[u8]) -> u16 {
    u16::from_le_bytes([b[0], b[1]])
}
fn le32(b: &[u8]) -> u32 {
    u32::from_le_bytes([b[0], b[1], b[2], b[3]])
}

/// Lecture d'un PARAM.SFO (PS3, PSP) : paires clé → valeur texte.
pub fn parse_sfo(b: &[u8]) -> BTreeMap<String, String> {
    let mut out = BTreeMap::new();
    if b.len() < 20 || &b[..4] != b"\0PSF" {
        return out;
    }
    let kt = le32(&b[8..]) as usize;
    let dt = le32(&b[12..]) as usize;
    let n = le32(&b[16..]).min(256) as usize;
    for i in 0..n {
        let e = 20 + i * 16;
        if e + 16 > b.len() {
            break;
        }
        let ko = kt + le16(&b[e..]) as usize;
        let fmt = le16(&b[e + 2..]);
        let len = le32(&b[e + 4..]) as usize;
        let doff = dt + le32(&b[e + 12..]) as usize;
        let key_end = b[ko.min(b.len())..].iter().position(|&c| c == 0).map(|p| ko + p).unwrap_or(b.len());
        let key = String::from_utf8_lossy(&b[ko.min(b.len())..key_end]).into_owned();
        if doff + len > b.len() {
            continue;
        }
        let val = if fmt == 0x0404 && len >= 4 {
            le32(&b[doff..]).to_string()
        } else {
            String::from_utf8_lossy(&b[doff..doff + len]).trim_end_matches('\0').to_string()
        };
        out.insert(key, val);
    }
    out
}

/// PlayStation 3 : `PS3_DISC.SFB` / `PS3_GAME/PARAM.SFO`.
pub fn ps3(ctx: &Ctx, out: &mut Vec<Match>) {
    if !matches!(ctx.phys.media, MediaKind::Bd | MediaKind::Unknown) {
        return;
    }
    for t in ctx.data_tracks() {
        let Some(fs) = ctx.fs(t) else { continue };
        if !(fs.exists("PS3_DISC.SFB") || fs.exists("PS3_GAME/PARAM.SFO")) {
            continue;
        }
        let mut m = Match::new("console:ps3", Confidence::Certain, "ps3-sfo");
        if let Ok(b) = fs.read("PS3_GAME/PARAM.SFO", 64 * 1024) {
            let sfo = parse_sfo(&b);
            if let Some(id) = sfo.get("TITLE_ID") {
                let s = if id.len() == 9 { format!("{}-{}", &id[..4], &id[4..]) } else { id.clone() };
                m.identity.region = region_from_sony_serial(&s);
                m.identity.serial = Some(s);
            }
            m.identity.title = sfo.get("TITLE").cloned();
            m.identity.revision = sfo.get("VERSION").or(sfo.get("APP_VER")).cloned();
        }
        out.push(m);
        return;
    }
}

/// DVD-Video minuscule sur lecteur standard : probable partition vidéo d'un
/// jeu Xbox ou Xbox 360 (le reste du disque est invisible).
pub fn xbox_video_partition(ctx: &Ctx, out: &mut Vec<Match>, profile: &str) {
    if ctx.phys.media != MediaKind::Dvd || out.iter().any(|m| m.tag.starts_with("console:")) {
        return;
    }
    let Some(t) = ctx.data_tracks().first().copied() else { return };
    let Some(fs) = ctx.fs(t) else { return };
    if !fs.exists("VIDEO_TS/VIDEO_TS.IFO") {
        return;
    }
    let vol = fs.volume_sectors().unwrap_or(u64::MAX);
    // Un vrai DVD-Video fait des centaines de Mo ; la partition Xbox quelques Mo.
    if vol < 51_200 && ctx.phys.capacity < 51_200 * 2 {
        let mut m = Match::new("console:xbox", Confidence::Heuristic, "xbox-video-partition");
        m.identity.label = Some(fs.volume_id());
        out.push(m);
        ctx.warn("xbox-video-partition-probable");
        if profile == "standard" {
            ctx.warn("drive-incompatible");
        }
    }
}

/// DVD dont aucun secteur n'est lisible : disque Nintendo (ou autre format
/// propriétaire) sur un lecteur qui n'expose pas la lecture brute.
pub fn nintendo_unreadable(ctx: &Ctx, out: &mut Vec<Match>, profile: &str) {
    if ctx.phys.media != MediaKind::Dvd || !out.iter().all(|m| m.tag == "data") {
        return;
    }
    let base = ctx.data_tracks().first().map(|t| t.start).unwrap_or(0);
    if ctx.sector(base, false).is_some() || ctx.sector(base + 16, false).is_some() {
        return;
    }
    if matches!(profile, "omnidrive" | "friidump") {
        out.push(Match::new("unknown:needs-raw-read", Confidence::Strong, "unreadable-standard-read"));
        ctx.warn("identify-after-dump");
    } else {
        ctx.warn("unreadable-disc");
    }
}

/// Jeux PC : `AUTORUN.INF` / `SETUP.EXE` à la racine (désactivé par défaut).
/// Disque de données ne contenant que de la musique, ou que de la vidéo
/// (pochettes, sous-titres et fichiers d'information mis à part).
pub fn data_content(ctx: &Ctx, out: &mut Vec<Match>) {
    if !matches!(ctx.phys.media, MediaKind::Cd | MediaKind::Dvd | MediaKind::Bd | MediaKind::Unknown) {
        return;
    }
    let tracks = ctx.data_tracks();
    let Some(t) = tracks.last() else { return };
    let Some(fs) = ctx.fs(t) else { return };
    if let Some((kind, n)) = crate::data::classify(fs.as_ref()) {
        let mut m = Match::new(&format!("data:{kind}"), Confidence::Strong, "data-files");
        m.identity.title = Some(fs.volume_id()).filter(|s| !s.is_empty());
        m.identity.label = m.identity.title.clone();
        crate::dl_log!(debug, "identify", "disque de données simple", "kind" => kind, "files" => n);
        out.push(m);
    }
}

pub fn pc_heuristic(ctx: &Ctx, out: &mut Vec<Match>) {
    for t in ctx.data_tracks() {
        if let Some(fs) = ctx.fs(t) {
            if fs.exists("AUTORUN.INF") || fs.exists("SETUP.EXE") {
                let mut m = Match::new("console:windows", Confidence::Heuristic, "pc-autorun");
                m.identity.title = Some(fs.volume_id()).filter(|s| !s.is_empty());
                out.push(m);
                return;
            }
        }
    }
}

/// Série Sony depuis une valeur `BOOT` : `cdrom:\SCES_008.67;1` → `SCES-00867`.
pub fn sony_serial_from_boot(v: &str) -> Option<String> {
    let v = v.trim();
    let base = v.rsplit(['\\', ':', '/']).next()?.split(';').next()?.trim();
    let alnum: String = base.chars().filter(|c| c.is_ascii_alphanumeric()).collect::<String>().to_ascii_uppercase();
    let letters: String = alnum.chars().take_while(|c| c.is_ascii_alphabetic()).collect();
    let digits = &alnum[letters.len()..];
    if letters.len() == 4 && digits.len() >= 5 && digits.chars().all(|c| c.is_ascii_digit()) {
        Some(format!("{letters}-{digits}"))
    } else {
        None
    }
}

fn parse_disc_fraction(s: &str) -> (Option<u32>, Option<u32>) {
    // « CD-1/2 », « GD-ROM1/2 »
    let Some(slash) = s.find('/') else { return (None, None) };
    let before: String = s[..slash].chars().rev().take_while(|c| c.is_ascii_digit()).collect::<Vec<_>>().into_iter().rev().collect();
    let after: String = s[slash + 1..].chars().take_while(|c| c.is_ascii_digit()).collect();
    (before.parse().ok(), after.parse().ok())
}

/// Post-traitement nommé d'une signature. Renvoie `false` pour rejeter.
pub fn apply_parser(name: &str, ctx: &Ctx, t: &Track, m: &mut Match, raw: &BTreeMap<String, String>) -> bool {
    let id = &mut m.identity;
    match name {
        "sony" => {
            let Some(fs) = ctx.fs(t) else { return false };
            let Ok(cnf) = fs.read("SYSTEM.CNF", 8192) else { return false };
            let text = String::from_utf8_lossy(&cnf);
            let mut found = false;
            for line in text.lines() {
                let Some((k, v)) = line.split_once('=') else { continue };
                match k.trim().to_ascii_uppercase().as_str() {
                    "BOOT2" => {
                        m.tag = "console:ps2".into();
                        id.system = "ps2".into();
                        id.serial = sony_serial_from_boot(v);
                        found = true;
                    }
                    "BOOT" if !found => {
                        m.tag = "console:psx".into();
                        id.system = "psx".into();
                        id.serial = sony_serial_from_boot(v);
                        found = true;
                    }
                    "VER" => id.revision = Some(v.trim().to_string()).filter(|s| !s.is_empty() && s != "1.00"),
                    _ => {}
                }
            }
            if !found {
                return false;
            }
            id.region = id.serial.as_deref().and_then(region_from_sony_serial);
            id.label = Some(fs.volume_id()).filter(|s| !s.is_empty());
            true
        }
        "saturn" | "dreamcast" => {
            id.region = raw.get("region_raw").and_then(|r| region_from_sega_area(r));
            if let Some(d) = raw.get("disc_raw") {
                let (n, total) = parse_disc_fraction(d);
                id.disc = n;
                id.discs_total = total;
            }
            if let Some(v) = id.revision.take() {
                let v = v.trim_start_matches('V').trim_start_matches('v');
                id.revision = Some(v.to_string()).filter(|s| !s.is_empty() && s != "1.000" && s != "1.00");
            }
            true
        }
        "megacd" => {
            id.region = raw.get("region_raw").and_then(|r| region_from_sega_area(&r.chars().take(3).collect::<String>()));
            if let Some(s) = id.serial.take() {
                // « GM T-12345 -00 » → série T-12345, révision 00
                let s = s.trim_start_matches("GM").trim();
                let (ser, rev) = match s.rsplit_once(" -").or_else(|| s.rsplit_once(' ')) {
                    Some((a, b)) => (a.trim().to_string(), Some(b.trim().trim_start_matches('-').to_string())),
                    None => (s.to_string(), None),
                };
                id.serial = Some(ser).filter(|x| !x.is_empty());
                id.revision = rev.filter(|r| !r.is_empty() && r != "00");
            }
            true
        }
        "nintendo" => {
            let Some(gid) = id.game_id.clone() else { return false };
            if gid.len() != 6 || !gid.chars().all(|c| c.is_ascii_alphanumeric()) {
                return false;
            }
            id.region = region_from_nintendo_id(&gid);
            id.disc = raw.get("disc_raw").and_then(|d| d.parse::<u32>().ok()).map(|d| d + 1);
            id.revision = raw.get("rev_raw").and_then(|d| d.parse::<u32>().ok()).filter(|&r| r > 0).map(|r| format!("Rev {r}"));
            true
        }
        _ => true,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn sony_boot() {
        assert_eq!(sony_serial_from_boot(" cdrom:\\SCES_008.67;1").as_deref(), Some("SCES-00867"));
        assert_eq!(sony_serial_from_boot("cdrom0:\\SLUS_200.62;1").as_deref(), Some("SLUS-20062"));
        assert_eq!(sony_serial_from_boot("cdrom:\\PSX.EXE;1"), None);
        assert_eq!(parse_disc_fraction("CD-2/3"), (Some(2), Some(3)));
        assert_eq!(parse_disc_fraction("GD-ROM1/1"), (Some(1), Some(1)));
    }
}

#[cfg(test)]
pub fn tests_swap(b: &[u8]) -> Vec<u8> {
    swap16(b)
}
