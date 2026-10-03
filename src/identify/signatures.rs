//! Signatures déclaratives (`signatures/*.toml`).
//!
//! ```toml
//! [[signature]]
//! id = "saturn"
//! tag = "console:saturn"
//! confidence = "certain"
//! media = ["cd"]
//! track = "first-data"      # first-data | last-data | any-data | first
//! parser = "saturn"         # post-traitement en code (facultatif)
//! [[signature.match]]       # toutes les conditions doivent être vraies
//! sector = 0
//! offset = 0
//! ascii = "SEGA SEGASATURN"
//! [[signature.not]]         # aucune ne doit être vraie
//! file = "SYSTEM.CNF"
//! [signature.fields]
//! serial = { sector = 0, offset = 0x20, len = 10 }
//! ```
//!
//! Conditions : `ascii`/`hex` à (`sector`, `offset`) ; `contains` (texte) ou
//! `contains_hex` dans les secteurs `sectors = [début, fin]` ; `raw = true`
//! pour lire des secteurs bruts de 2352 octets ; `file`, `dir`,
//! `file_contains` (avec `file`) ; `system_id`, `volume_id` (préfixe du PVD) ;
//! `disc_type` (octet de la TOC complète) ; `min_audio_tracks`.

use super::{probes, Confidence, Ctx, Match};
use crate::device::{MediaKind, Track};
use crate::json::Value;
use crate::util;

#[derive(Clone, Debug)]
pub enum Cond {
    Bytes { sector: u32, offset: usize, bytes: Vec<u8>, raw: bool },
    Contains { from: u32, to: u32, bytes: Vec<u8>, raw: bool, ignore_case: bool },
    File(String),
    Dir(String),
    FileContains { file: String, text: String },
    SystemId(String),
    VolumeId(String),
    DiscType(u8),
    MinAudio(usize),
}

#[derive(Clone, Debug)]
pub struct FieldSpec {
    pub name: String,
    pub sector: u32,
    pub offset: usize,
    pub len: usize,
    pub enc: String,
}

#[derive(Clone, Copy, Debug, PartialEq)]
pub enum TrackSel {
    FirstData,
    LastData,
    AnyData,
    First,
}

#[derive(Clone, Debug)]
pub struct Signature {
    pub id: String,
    pub tag: String,
    pub confidence: Confidence,
    pub media: Vec<MediaKind>,
    pub track: TrackSel,
    pub conds: Vec<Cond>,
    pub nots: Vec<Cond>,
    pub fields: Vec<FieldSpec>,
    pub parser: Option<String>,
    pub secondary: bool,
}

/// Signatures intégrées au binaire (utilisées si aucun fichier n'est installé).
pub const BUILTIN: &[(&str, &str)] = &[("consoles.toml", include_str!("../../data/signatures/consoles.toml")), ("media.toml", include_str!("../../data/signatures/media.toml"))];

fn parse_cond(v: &Value) -> Result<Cond, String> {
    let sector = v["sector"].as_i64().unwrap_or(0) as u32;
    let offset = v["offset"].as_i64().unwrap_or(0) as usize;
    let raw = v["raw"].bool_or(false);
    if let Some(s) = v["ascii"].as_str() {
        return Ok(Cond::Bytes { sector, offset, bytes: s.as_bytes().to_vec(), raw });
    }
    if let Some(h) = v["hex"].as_str() {
        return Ok(Cond::Bytes { sector, offset, bytes: util::unhex(h).ok_or("hex invalide")?, raw });
    }
    let range = |v: &Value| {
        let r = v["sectors"].as_arr();
        let from = r.first().and_then(|x| x.as_i64()).unwrap_or(sector as i64) as u32;
        let to = r.get(1).and_then(|x| x.as_i64()).unwrap_or(from as i64) as u32;
        (from, to.max(from).min(from + 63))
    };
    if let Some(s) = v["contains"].as_str() {
        let (from, to) = range(v);
        return Ok(Cond::Contains { from, to, bytes: s.as_bytes().to_vec(), raw, ignore_case: v["ignore_case"].bool_or(false) });
    }
    if let Some(h) = v["contains_hex"].as_str() {
        let (from, to) = range(v);
        return Ok(Cond::Contains { from, to, bytes: util::unhex(h).ok_or("hex invalide")?, raw, ignore_case: false });
    }
    if let Some(f) = v["file"].as_str() {
        if let Some(t) = v["file_contains"].as_str() {
            return Ok(Cond::FileContains { file: f.into(), text: t.into() });
        }
        return Ok(Cond::File(f.into()));
    }
    if let Some(d) = v["dir"].as_str() {
        return Ok(Cond::Dir(d.into()));
    }
    if let Some(s) = v["system_id"].as_str() {
        return Ok(Cond::SystemId(s.into()));
    }
    if let Some(s) = v["volume_id"].as_str() {
        return Ok(Cond::VolumeId(s.into()));
    }
    if let Some(d) = v["disc_type"].as_i64() {
        return Ok(Cond::DiscType(d as u8));
    }
    if let Some(n) = v["min_audio_tracks"].as_i64() {
        return Ok(Cond::MinAudio(n as usize));
    }
    Err("condition inconnue".into())
}

pub fn parse_file(src: &str) -> Result<Vec<Signature>, String> {
    let v = crate::toml::parse(src).map_err(|e| e.to_string())?;
    let mut out = vec![];
    for s in v["signature"].as_arr() {
        let id = s["id"].as_str().ok_or("signature sans id")?.to_string();
        let ctx = |e: String| format!("signature {id} : {e}");
        let media = s["media"]
            .strings()
            .iter()
            .map(|m| match m.as_str() {
                "cd" => MediaKind::Cd,
                "dvd" => MediaKind::Dvd,
                "bd" => MediaKind::Bd,
                "hddvd" => MediaKind::HdDvd,
                _ => MediaKind::Unknown,
            })
            .collect();
        let track = match s["track"].str_or("first-data") {
            "last-data" => TrackSel::LastData,
            "any-data" => TrackSel::AnyData,
            "first" => TrackSel::First,
            _ => TrackSel::FirstData,
        };
        let conds = s["match"].as_arr().iter().map(parse_cond).collect::<Result<Vec<_>, _>>().map_err(ctx)?;
        let nots = s["not"].as_arr().iter().map(parse_cond).collect::<Result<Vec<_>, _>>().map_err(ctx)?;
        let mut fields = vec![];
        if let Some(m) = s["fields"].as_obj() {
            for (name, f) in m {
                fields.push(FieldSpec {
                    name: name.clone(),
                    sector: f["sector"].as_i64().unwrap_or(0) as u32,
                    offset: f["offset"].as_i64().unwrap_or(0) as usize,
                    len: f["len"].as_i64().unwrap_or(1).clamp(1, 2048) as usize,
                    enc: f["enc"].str_or("ascii").to_string(),
                });
            }
        }
        out.push(Signature {
            tag: s["tag"].as_str().ok_or_else(|| ctx("tag manquant".into()))?.to_string(),
            confidence: Confidence::parse(s["confidence"].str_or("strong")),
            media,
            track,
            conds,
            nots,
            fields,
            parser: s["parser"].string(),
            secondary: s["role"].as_str() == Some("secondary"),
            id,
        });
    }
    Ok(out)
}

/// Charge les signatures intégrées puis celles des répertoires de données
/// (système puis utilisateur) ; une signature de même `id` remplace la précédente.
pub fn load_all() -> Vec<Signature> {
    let mut all: Vec<Signature> = vec![];
    let mut add = |sigs: Vec<Signature>| {
        for s in sigs {
            if let Some(i) = all.iter().position(|x| x.id == s.id) {
                all[i] = s;
            } else {
                all.push(s);
            }
        }
    };
    for (_, src) in BUILTIN {
        add(parse_file(src).expect("signatures intégrées valides"));
    }
    for dir in crate::paths::data_layers("signatures") {
        let mut files: Vec<_> = std::fs::read_dir(&dir).map(|it| it.filter_map(|e| e.ok()).map(|e| e.path()).filter(|p| p.extension().map_or(false, |e| e == "toml")).collect()).unwrap_or_default();
        files.sort();
        for f in files {
            match std::fs::read_to_string(&f).map_err(|e| e.to_string()).and_then(|t| parse_file(&t)) {
                Ok(s) => add(s),
                Err(e) => crate::dl_log!(warn, "identify", format!("signatures ignorées : {e}"), "file" => f.display()),
            }
        }
    }
    all
}

fn candidates<'a>(ctx: &Ctx<'a>, sel: TrackSel) -> Vec<&'a Track> {
    let data = ctx.data_tracks();
    match sel {
        TrackSel::FirstData => data.first().copied().into_iter().collect(),
        TrackSel::LastData => data.last().copied().into_iter().collect(),
        TrackSel::AnyData => data,
        TrackSel::First => ctx.phys.tracks.first().into_iter().collect(),
    }
}

fn eq_bytes(a: &[u8], b: &[u8], ignore_case: bool) -> bool {
    if ignore_case {
        a.eq_ignore_ascii_case(b)
    } else {
        a == b
    }
}

fn check(c: &Cond, ctx: &Ctx, t: &Track) -> bool {
    match c {
        Cond::Bytes { sector, offset, bytes, raw } => {
            let size = if *raw { 2352 } else { 2048 };
            let lba = t.start + sector + (*offset / size) as u32;
            let off = offset % size;
            match ctx.sector(lba, *raw) {
                Some(s) if off + bytes.len() <= s.len() => &s[off..off + bytes.len()] == bytes.as_slice(),
                _ => false,
            }
        }
        Cond::Contains { from, to, bytes, raw, ignore_case } => {
            for s in *from..=*to {
                if let Some(d) = ctx.sector(t.start + s, *raw) {
                    if d.windows(bytes.len()).any(|w| eq_bytes(w, bytes, *ignore_case)) {
                        return true;
                    }
                }
            }
            false
        }
        Cond::File(p) => ctx.fs(t).map_or(false, |f| f.stat(p).map_or(false, |e| !e.dir)),
        Cond::Dir(p) => ctx.fs(t).map_or(false, |f| f.is_dir(p)),
        Cond::FileContains { file, text } => ctx.fs(t).and_then(|f| f.read(file, 64 * 1024).ok()).map_or(false, |d| d.windows(text.len()).any(|w| w.eq_ignore_ascii_case(text.as_bytes()))),
        Cond::SystemId(p) => ctx.fs(t).map_or(false, |f| f.system_id().to_ascii_uppercase().starts_with(&p.to_ascii_uppercase())),
        Cond::VolumeId(p) => ctx.fs(t).map_or(false, |f| f.volume_id().to_ascii_uppercase().starts_with(&p.to_ascii_uppercase())),
        Cond::DiscType(d) => ctx.phys.disc_type == Some(*d),
        Cond::MinAudio(n) => ctx.phys.audio_tracks() >= *n,
    }
}

fn extract(f: &FieldSpec, ctx: &Ctx, t: &Track) -> Option<String> {
    let lba = t.start + f.sector + (f.offset / 2048) as u32;
    let off = f.offset % 2048;
    let s = ctx.sector(lba, false)?;
    let end = (off + f.len).min(s.len());
    let b = &s[off..end];
    let v = match f.enc.as_str() {
        "hex" => util::hex(b),
        "u8" => b.first()?.to_string(),
        _ => util::ascii_field(b),
    };
    if v.is_empty() {
        None
    } else {
        Some(v)
    }
}

pub fn evaluate(sig: &Signature, ctx: &Ctx) -> Option<Match> {
    if !sig.media.is_empty() && !sig.media.contains(&ctx.phys.media) && ctx.phys.media != MediaKind::Unknown {
        return None;
    }
    for t in candidates(ctx, sig.track) {
        if !sig.conds.iter().all(|c| check(c, ctx, t)) {
            continue;
        }
        if sig.nots.iter().any(|c| check(c, ctx, t)) {
            continue;
        }
        let mut m = Match::new(&sig.tag, sig.confidence, &sig.id);
        let mut raw = std::collections::BTreeMap::new();
        for f in &sig.fields {
            if let Some(v) = extract(f, ctx, t) {
                raw.insert(f.name.clone(), v);
            }
        }
        let id = &mut m.identity;
        id.serial = raw.get("serial").cloned();
        id.game_id = raw.get("game_id").cloned();
        id.title = raw.get("title").cloned();
        id.revision = raw.get("revision").cloned();
        id.label = raw.get("label").cloned();
        if let Some(p) = &sig.parser {
            if !probes::apply_parser(p, ctx, t, &mut m, &raw) {
                continue;
            }
        }
        return Some(m);
    }
    None
}

#[cfg(test)]
mod tests {
    #[test]
    fn builtin_parse() {
        for (name, src) in super::BUILTIN {
            let s = super::parse_file(src).unwrap_or_else(|e| panic!("{name}: {e}"));
            assert!(!s.is_empty());
        }
    }
}
