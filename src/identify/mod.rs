//! Classificateur : lit le disque sans le monter et produit un résultat
//! multi-étiquettes (section 7 de la spécification).

pub mod probes;
pub mod profiles;
pub mod signatures;

use crate::device::{Budget, DiscSource, DriveInfo, MediaKind, Physical, Track};
use crate::fs::{self, iso9660::Iso9660, FileSystem};
use crate::identity::Identity;
use crate::jobj;
use crate::json::Value;
use std::cell::RefCell;
use std::collections::HashMap;
use std::rc::Rc;
use std::time::Duration;

#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub enum Confidence {
    Heuristic = 0,
    Strong = 1,
    Certain = 2,
}

impl Confidence {
    pub fn parse(s: &str) -> Confidence {
        match s {
            "certain" => Confidence::Certain,
            "heuristic" | "heuristique" => Confidence::Heuristic,
            _ => Confidence::Strong,
        }
    }
    pub fn name(self) -> &'static str {
        match self {
            Confidence::Certain => "certain",
            Confidence::Strong => "strong",
            Confidence::Heuristic => "heuristic",
        }
    }
}

#[derive(Clone, Debug, PartialEq)]
pub struct Match {
    pub tag: String,
    pub confidence: Confidence,
    pub primary: bool,
    pub identity: Identity,
    pub source: String,
}

impl Match {
    pub fn new(tag: &str, confidence: Confidence, source: &str) -> Match {
        Match { tag: tag.into(), confidence, primary: false, identity: Identity { system: target_of(tag).unwrap_or_default(), ..Default::default() }, source: source.into() }
    }
    pub fn to_value(&self) -> Value {
        jobj! {
            "tag" => self.tag.clone(), "confidence" => self.confidence.name(),
            "role" => if self.primary { "primary" } else { "secondary" },
            "identity" => self.identity.to_value(), "source" => self.source.clone(),
        }
    }
    pub fn from_value(v: &Value) -> Match {
        Match {
            tag: v["tag"].str_or("").into(),
            confidence: Confidence::parse(v["confidence"].str_or("strong")),
            primary: v["role"].as_str() == Some("primary"),
            identity: Identity::from_value(&v["identity"]),
            source: v["source"].str_or("").into(),
        }
    }
}

/// Gestionnaire d'une étiquette : `console:psx` → `psx`, `video:dvd` →
/// `dvd-video`, `audio:cdda` → `cdda`…
pub fn target_of(tag: &str) -> Option<String> {
    let (kind, rest) = tag.split_once(':')?;
    match kind {
        "console" => Some(rest.to_string()),
        "audio" | "video" | "photo" | "data" => media_id(tag).map(|s| s.to_string()),
        _ => None,
    }
}

/// Identifiant de média d'une étiquette audio/vidéo.
pub fn media_id(tag: &str) -> Option<&'static str> {
    Some(match tag {
        "audio:cdda" | "audio:cdda-mixed" => "cdda",
        "audio:dvd-audio" => "dvd-audio",
        "video:dvd" => "dvd-video",
        "video:bd" => "bluray-video",
        "video:vcd" => "vcd",
        "video:svcd" => "svcd",
        "video:hddvd" => "hddvd-video",
        "video:avchd" => "avchd",
        "video:dvd-vr" => "dvd-vr",
        "video:bdav" => "bdav",
        "photo:cd" => "photo-cd",
        "photo:dcim" => "dcim",
        "data:audio" => "data-audio",
        "data:video" => "data-video",
        _ => return None,
    })
}

/// L'étiquette relève-t-elle d'un gestionnaire multimédia ?
pub fn is_media(tag: &str) -> bool {
    media_id(tag).is_some()
}

#[derive(Clone, Debug, Default, PartialEq)]
pub struct Feasibility {
    pub read: bool,
    pub dump: bool,
    pub dump_usable: bool,
}

#[derive(Clone, Debug, PartialEq)]
pub struct IdentResult {
    pub device: Option<String>,
    pub drive: Option<DriveInfo>,
    pub profile: String,
    pub physical: Physical,
    pub fingerprint: String,
    pub filesystem: Option<String>,
    pub volume_id: Option<String>,
    pub matches: Vec<Match>,
    pub warnings: Vec<String>,
    pub feasibility: Feasibility,
}

impl IdentResult {
    pub fn primary(&self) -> Option<&Match> {
        self.matches.iter().find(|m| m.primary)
    }
    pub fn primaries(&self) -> Vec<&Match> {
        self.matches.iter().filter(|m| m.primary).collect()
    }
    pub fn has_tag(&self, tag: &str) -> bool {
        self.matches.iter().any(|m| m.tag == tag)
    }
    /// Gestionnaire de l'action principale (`psx`, `dvd-video`…), sinon None (données, vierge).
    pub fn target(&self) -> Option<String> {
        self.primary().and_then(|m| target_of(&m.tag))
    }
    pub fn to_value(&self) -> Value {
        let drive = self.drive.as_ref().map(|d| jobj! {"vendor" => d.vendor.clone(), "model" => d.model.clone(), "firmware" => d.firmware.clone(), "profile" => self.profile.clone()});
        jobj! {
            "device" => self.device.clone(),
            "drive" => drive,
            "profile" => self.profile.clone(),
            "physical" => self.physical.to_value(),
            "fingerprint" => self.fingerprint.clone(),
            "filesystem" => self.filesystem.clone(),
            "volume_id" => self.volume_id.clone(),
            "matches" => self.matches.iter().map(|m| m.to_value()).collect::<Vec<_>>(),
            "warnings" => self.warnings.clone(),
            "feasibility" => jobj!{"read" => self.feasibility.read, "dump" => self.feasibility.dump, "dump_usable" => self.feasibility.dump_usable},
        }
    }
    /// Reconstruction partielle (le détail physique n'est pas relu).
    pub fn matches_from_value(v: &Value) -> Vec<Match> {
        v["matches"].as_arr().iter().map(Match::from_value).collect()
    }
}

pub struct Options {
    pub max_bytes: u64,
    pub max_time: Duration,
    /// Profil forcé (`auto` = détection).
    pub profile: String,
    pub signatures: Option<Vec<signatures::Signature>>,
}

impl Default for Options {
    fn default() -> Self {
        Options { max_bytes: 4 << 20, max_time: Duration::from_secs(20), profile: "auto".into(), signatures: None }
    }
}

/// Contexte de sondage partagé par les signatures et les sondes.
pub struct Ctx<'a> {
    pub src: &'a dyn DiscSource,
    pub phys: &'a Physical,
    sectors: RefCell<HashMap<(u32, bool), Option<Rc<Vec<u8>>>>>,
    fss: RefCell<HashMap<u32, Option<Rc<dyn FileSystem + 'a>>>>,
    pub warnings: RefCell<Vec<String>>,
}

impl<'a> Ctx<'a> {
    pub fn new(src: &'a dyn DiscSource) -> Ctx<'a> {
        Ctx { src, phys: src.physical(), sectors: Default::default(), fss: Default::default(), warnings: Default::default() }
    }
    pub fn warn(&self, w: &str) {
        let mut ws = self.warnings.borrow_mut();
        if !ws.iter().any(|x| x == w) {
            ws.push(w.to_string());
        }
    }
    /// Secteur de données (2048) ou brut (2352), avec cache.
    pub fn sector(&self, lba: u32, raw: bool) -> Option<Rc<Vec<u8>>> {
        if let Some(v) = self.sectors.borrow().get(&(lba, raw)) {
            return v.clone();
        }
        let r = if raw { self.src.read_raw(lba) } else { self.src.read_data(lba, 1) };
        if let Err(e) = &r {
            // Première erreur seulement : elle suffit au diagnostic.
            if !self.warnings.borrow().iter().any(|w| w.starts_with("read-error")) {
                self.warn(&format!("read-error: {e}"));
            }
        }
        let v = r.ok().map(Rc::new);
        self.sectors.borrow_mut().insert((lba, raw), v.clone());
        v
    }
    /// Système de fichiers d'une piste de données (ISO 9660, sinon montage).
    pub fn fs(&self, track: &Track) -> Option<Rc<dyn FileSystem + 'a>> {
        if let Some(v) = self.fss.borrow().get(&track.start) {
            return v.clone();
        }
        let mut out: Option<Rc<dyn FileSystem + 'a>> = None;
        let iso = match Iso9660::open(self.src, track.start) {
            Ok(i) => Some(i),
            Err(e) => {
                if self.phys.media == MediaKind::Cd {
                    self.warn(&format!("iso9660: {e}"));
                }
                None
            }
        };
        let udf = matches!(self.phys.media, MediaKind::Dvd | MediaKind::Bd | MediaKind::HdDvd) && fs::has_udf(self.src, track.start);
        if let Some(i) = iso {
            out = Some(Rc::new(i));
        }
        // UDF seul, ou pont ISO sans les dossiers UDF (BD-Video) : lecture UDF
        // directe, sinon le point de montage.
        if udf {
            let useful = |f: &dyn FileSystem| f.exists("VIDEO_TS") || f.exists("BDMV") || f.exists("AUDIO_TS") || f.exists("HVDVD_TS") || f.exists("PS3_GAME");
            let mut need_mount = out.as_ref().map_or(true, |f| !useful(f.as_ref()));
            if need_mount {
                match fs::udf::Udf::open(self.src, track.start) {
                    Ok(u) => {
                        if out.is_none() || useful(&u) {
                            out = Some(Rc::new(u));
                            need_mount = false;
                        }
                    }
                    Err(e) => self.warn(&format!("{e}")),
                }
            }
            if need_mount {
                match self.src.mount_point().and_then(|p| fs::open_mounted(&p)) {
                    Some(d) => out = Some(Rc::new(d)),
                    None => {
                        if out.is_none() || self.phys.media == MediaKind::Bd {
                            self.warn("udf-not-mounted");
                        }
                    }
                }
            }
        }
        if out.is_none() && self.phys.media == MediaKind::Unknown {
            if let Some(d) = self.src.mount_point().and_then(|p| fs::open_mounted(&p)) {
                out = Some(Rc::new(d));
            }
        }
        self.fss.borrow_mut().insert(track.start, out.clone());
        out
    }
    pub fn data_tracks(&self) -> Vec<&'a Track> {
        self.phys.tracks.iter().filter(|t| t.data).collect()
    }
}

/// Empreinte rapide : TOC + SHA-1 de quelques secteurs.
pub fn fingerprint(src: &dyn DiscSource) -> String {
    let p = src.physical();
    let mut s = crate::util::Sha1::new();
    s.update(p.media.name().as_bytes());
    for t in &p.tracks {
        s.update(format!("{}:{}:{}:{}:{};", t.number, t.session, t.data, t.start, t.length).as_bytes());
    }
    s.update(&p.leadout.to_le_bytes());
    if let Some(t) = p.tracks.iter().find(|t| t.data) {
        for l in [0u32, 16] {
            if let Ok(b) = src.read_data(t.start + l, 1) {
                s.update(&b);
            }
        }
    }
    s.hex()[..16].to_string()
}

/// Empreinte de la seule table des matières (identité des systèmes sans série).
pub fn toc_fingerprint(p: &Physical) -> String {
    let desc: String = p.tracks.iter().map(|t| format!("{}{}:{};", if t.data { 'D' } else { 'A' }, t.session, t.length)).collect();
    crate::util::sha1_hex(format!("{desc}{}", p.leadout).as_bytes())[..12].to_string()
}

/// Point d'entrée : identifie le disque de `src`.
pub fn identify(src: &dyn DiscSource, opts: &Options) -> IdentResult {
    let budget = Budget::new(src, opts.max_bytes, opts.max_time);
    let ctx = Ctx::new(&budget);
    let phys = ctx.phys.clone();
    let drive = src.drive();
    let profile = profiles::resolve(&opts.profile, drive.as_ref());
    let mut matches: Vec<Match> = vec![];

    let fingerprint = fingerprint(&budget);

    if phys.blank {
        let mut m = Match::new("blank", Confidence::Certain, "physical");
        m.primary = true;
        return IdentResult {
            device: src.device(),
            drive,
            profile,
            physical: phys,
            fingerprint,
            filesystem: None,
            volume_id: None,
            matches: vec![m],
            warnings: vec![],
            feasibility: Feasibility::default(),
        };
    }

    // 1-2. Structure audio/données (TOC).
    probes::toc_structure(&ctx, &mut matches);

    // 3-4. Signatures déclaratives (secteurs bruts, fichiers, PVD).
    let loaded;
    let sigs: &[signatures::Signature] = match &opts.signatures {
        Some(s) => s,
        None => {
            loaded = signatures::load_all();
            &loaded
        }
    };
    for sig in sigs {
        if let Some(m) = signatures::evaluate(sig, &ctx) {
            matches.push(m);
        }
    }

    // Sondes complexes.
    probes::jaguar(&ctx, &mut matches);
    probes::xbox(&ctx, &mut matches, &profile);
    probes::ps3(&ctx, &mut matches);
    probes::xbox_video_partition(&ctx, &mut matches, &profile);
    probes::nintendo_unreadable(&ctx, &mut matches, &profile);

    // 5. Disques de données simples (musique seule, vidéo seule), puis
    //    heuristiques en dernier recours.
    let concluded = matches.iter().any(|m| m.tag != "data" && m.tag != "audio:cdda-mixed");
    if !concluded {
        probes::data_content(&ctx, &mut matches);
    }
    let concluded = matches.iter().any(|m| m.tag != "data" && m.tag != "audio:cdda-mixed");
    if !concluded {
        probes::pc_heuristic(&ctx, &mut matches);
    }

    let fs_info = phys.tracks.iter().rev().find(|t| t.data).and_then(|t| ctx.fs(t)).map(|f| (f.kind().to_string(), f.volume_id()));

    // Complète l'identité : empreinte TOC, libellé, numéro de disque par défaut.
    let toc_fp = toc_fingerprint(&phys);
    for m in matches.iter_mut() {
        if m.identity.toc_fp.is_none() && phys.media == MediaKind::Cd {
            m.identity.toc_fp = Some(toc_fp.clone());
        }
        if m.identity.label.is_none() {
            m.identity.label = fs_info.as_ref().map(|f| f.1.clone()).filter(|s| !s.is_empty());
        }
        if m.identity.disc.is_none() && m.tag.starts_with("console:") {
            m.identity.disc = Some(1);
        }
    }

    resolve_priority(&mut matches);
    let mut warnings = ctx.warnings.borrow().clone();
    if budget.exceeded.get() {
        warnings.push("probe-budget-exceeded".into());
    }
    let feasibility = feasibility(&matches, &phys, &profile, &mut warnings);

    IdentResult {
        device: src.device(),
        drive,
        profile,
        physical: phys,
        fingerprint,
        filesystem: fs_info.as_ref().map(|f| f.0.clone()),
        volume_id: fs_info.map(|f| f.1).filter(|s| !s.is_empty()),
        matches,
        warnings,
        feasibility,
    }
}

/// Règles de priorité (section 7.4).
pub fn resolve_priority(matches: &mut Vec<Match>) {
    // Doublons : on garde la meilleure confiance par étiquette, en fusionnant l'identité.
    let mut out: Vec<Match> = vec![];
    for m in matches.drain(..) {
        if let Some(e) = out.iter_mut().find(|e| e.tag == m.tag) {
            if m.confidence > e.confidence {
                let old = std::mem::replace(e, m);
                fill_identity(&mut e.identity, &old.identity);
            } else {
                fill_identity(&mut e.identity, &m.identity);
            }
        } else {
            out.push(m);
        }
    }
    for m in out.iter_mut() {
        m.primary = false;
    }
    let certain_console = out.iter().any(|m| m.tag.starts_with("console:") && m.confidence == Confidence::Certain);
    let xbox_partition = out.iter().any(|m| m.tag == "console:xbox" || m.tag == "console:xbox360");
    if certain_console {
        for m in out.iter_mut() {
            m.primary = m.tag.starts_with("console:") && m.confidence == Confidence::Certain;
        }
    } else if xbox_partition && out.iter().any(|m| m.tag.starts_with("video:")) {
        // Partition vidéo d'un jeu Xbox : jamais le lecteur multimédia.
        for m in out.iter_mut() {
            m.primary = m.tag.starts_with("console:xbox");
        }
    } else if let Some(i) = out.iter().position(|m| m.tag.starts_with("video:") && m.tag != "video:avchd") {
        out[i].primary = true;
    } else if let Some(i) = out.iter().position(|m| m.tag.starts_with("audio:") && m.tag != "audio:cdda-mixed") {
        out[i].primary = true;
    } else if let Some(i) = out.iter().position(|m| m.tag.starts_with("video:")) {
        out[i].primary = true;
    } else if let Some(i) = out.iter().position(|m| m.tag == "data:audio" || m.tag == "data:video") {
        // Disque de fichiers musicaux ou vidéo seuls.
        out[i].primary = true;
    } else if let Some(i) = out.iter().position(|m| m.tag.starts_with("console:")) {
        // Console de confiance forte ou heuristique (affichage soumis à la configuration).
        out[i].primary = true;
    } else if let Some(i) = out.iter().position(|m| m.tag.starts_with("photo:") || m.tag == "unknown:needs-raw-read") {
        out[i].primary = true;
    }
    if !out.iter().any(|m| m.primary) {
        if !out.iter().any(|m| m.tag == "data") {
            out.push(Match::new("data", Confidence::Strong, "fallback"));
        }
        // `data` n'est jamais une action ; aucun principal.
    }
    out.sort_by(|a, b| b.primary.cmp(&a.primary).then(b.confidence.cmp(&a.confidence)));
    *matches = out;
}

fn fill_identity(dst: &mut Identity, src: &Identity) {
    macro_rules! fill {
        ($($f:ident),*) => {$( if dst.$f.is_none() { dst.$f = src.$f.clone(); } )*};
    }
    fill!(serial, game_id, disc, discs_total, revision, region, title, label, toc_fp);
}

/// Faisabilité de la lecture et du dump selon l'étiquette et le profil du lecteur.
pub fn feasibility(matches: &[Match], phys: &Physical, profile: &str, warnings: &mut Vec<String>) -> Feasibility {
    let Some(p) = matches.iter().find(|m| m.primary) else {
        return Feasibility { read: true, dump: false, dump_usable: false };
    };
    let sys = target_of(&p.tag).unwrap_or_default();
    let mut f = Feasibility { read: true, dump: true, dump_usable: true };
    let mut need = |profiles: &[&str], w: &str, f: &mut Feasibility| {
        if !profiles.contains(&profile) {
            f.read = false;
            f.dump = false;
            f.dump_usable = false;
            warnings.push(w.into());
        }
    };
    match sys.as_str() {
        _ if is_media(&p.tag) => f.dump = false,
        "gc" | "wii" => need(&["omnidrive", "friidump"], "drive-incompatible", &mut f),
        "xbox" | "xbox360" => need(&["omnidrive", "kreon"], "drive-incompatible", &mut f),
        "wiiu" => {
            need(&["omnidrive"], "drive-incompatible", &mut f);
            f.dump_usable = false;
            warnings.push("encrypted-keys-required".into());
        }
        "ps3" => {
            f.dump_usable = false;
            warnings.push("encrypted-keys-required".into());
        }
        "dreamcast" | "naomigd" => {
            // GD-ROM : la zone haute densité n'est pas exposée par un lecteur standard.
            let hd_visible = phys.tracks.iter().any(|t| t.start >= 45000);
            if phys.media == MediaKind::Cd && !hd_visible && p.source.contains("gdrom") {
                f.dump = false;
                f.dump_usable = false;
                warnings.push("gdrom-hd-area-unreadable".into());
            }
        }
        _ => {}
    }
    if p.confidence == Confidence::Heuristic {
        warnings.push("heuristic-identification".into());
    }
    f
}

/// Identifie un lecteur réel.
pub fn identify_device(dev: &str, opts: &Options) -> std::io::Result<IdentResult> {
    let d = crate::device::cdrom::Cdrom::open(dev)?;
    Ok(identify(&d, opts))
}

/// Identifie une image (`.iso`, `.cue`).
pub fn identify_image(path: &std::path::Path, opts: &Options) -> std::io::Result<IdentResult> {
    let i = crate::device::image::Image::open(path)?;
    Ok(identify(&i, opts))
}

#[cfg(test)]
mod tests;
