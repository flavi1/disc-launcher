//! Résolution du nom canonique et prédiction du chemin final (section 8).

use crate::config::{Config, MultidiscLayout};
use crate::device::Physical;
use crate::handlers::{self, Manifest};
use crate::identity::Identity;
use crate::json::{self, Value};
use crate::refdb::{self, RefDb};
use crate::{jobj, paths, util};
use std::path::{Path, PathBuf};
use std::time::Duration;

#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub enum NameConfidence {
    Fallback = 0,
    Probable = 1,
    Exact = 2,
}

impl NameConfidence {
    pub fn name(self) -> &'static str {
        match self {
            NameConfidence::Exact => "exact",
            NameConfidence::Probable => "probable",
            NameConfidence::Fallback => "repli",
        }
    }
    pub fn parse(s: &str) -> NameConfidence {
        match s {
            "exact" => NameConfidence::Exact,
            "probable" => NameConfidence::Probable,
            _ => NameConfidence::Fallback,
        }
    }
}

#[derive(Clone, Debug, PartialEq)]
pub struct Resolution {
    /// Nom canonique complet : « Final Fantasy VII (France) (Disc 1) ».
    pub name: String,
    /// Nom du jeu sans le disque : « Final Fantasy VII (France) ».
    pub game: String,
    pub disc: Option<u32>,
    pub discs: Option<u32>,
    pub confidence: NameConfidence,
    pub source: String,
}

impl Resolution {
    pub fn multidisc(&self) -> bool {
        self.discs.map_or(false, |d| d > 1) || refdb::disc_of(&self.name).is_some()
    }
    pub fn to_value(&self) -> Value {
        jobj! {"name" => self.name.clone(), "game" => self.game.clone(), "disc" => self.disc, "discs" => self.discs,
               "confidence" => self.confidence.name(), "source" => self.source.clone()}
    }
    pub fn from_value(v: &Value) -> Option<Resolution> {
        Some(Resolution {
            name: v["name"].string()?,
            game: v["game"].string()?,
            disc: v["disc"].as_i64().map(|x| x as u32),
            discs: v["discs"].as_i64().map(|x| x as u32),
            confidence: NameConfidence::parse(v["confidence"].str_or("")),
            source: v["source"].str_or("").into(),
        })
    }
}

/// Remplace les caractères interdits ; tronque à 255 octets en gardant
/// l'extension et le suffixe de disque.
pub fn sanitize(name: &str, portable: bool) -> String {
    let mut s: String = name
        .chars()
        .map(|c| match c {
            '/' | '\0' => '-',
            '<' | '>' | ':' | '"' | '\\' | '|' | '?' | '*' if portable => match c {
                ':' => '-',
                '"' => '\'',
                _ => '_',
            },
            c if c.is_control() => ' ',
            c => c,
        })
        .collect();
    s = s.split_whitespace().collect::<Vec<_>>().join(" ");
    if portable {
        s = s.trim_end_matches(['.', ' ']).to_string();
    }
    if s.is_empty() || s == "." || s == ".." {
        s = "disc".into();
    }
    s
}

/// Tronque `stem` pour que `stem + suffix` tienne dans `max` octets.
pub fn fit(stem: &str, suffix: &str, max: usize) -> String {
    if stem.len() + suffix.len() <= max {
        return format!("{stem}{suffix}");
    }
    // Conserve le suffixe « (Disc N) » s'il est présent.
    let (core, disc) = match stem.rfind(" (Disc ") {
        Some(i) => (&stem[..i], &stem[i..]),
        None => (stem, ""),
    };
    let mut budget = max.saturating_sub(suffix.len() + disc.len());
    while budget > 0 && !core.is_char_boundary(budget.min(core.len())) {
        budget -= 1;
    }
    format!("{}{disc}{suffix}", core[..budget.min(core.len())].trim_end())
}

/// Nom de repli construit depuis les métadonnées du disque.
pub fn fallback_name(id: &Identity) -> String {
    let norm = |s: &str| s.chars().filter(|c| c.is_ascii_alphanumeric()).collect::<String>().to_ascii_uppercase();
    let serial = id.serial.as_ref().or(id.game_id.as_ref());
    // Libellé de volume qui n'est que le numéro de série (« SLES_029.05 ») : pas un titre.
    let label = id.label.clone().filter(|l| serial.map_or(true, |s| norm(l) != norm(s)));
    let title = id.title.clone().filter(|t| !t.trim().is_empty()).or(label).map(|t| title_case_if_upper(t.trim()));
    let mut s = match (title, serial) {
        (Some(t), Some(ser)) => format!("{t} [{ser}]"),
        (Some(t), None) => t,
        (None, Some(ser)) => ser.clone(),
        (None, None) => "Disque inconnu".into(),
    };
    if let Some(r) = &id.region {
        s.push_str(&format!(" ({r})"));
    }
    if let Some(rev) = &id.revision {
        s.push_str(&format!(" ({})", if rev.starts_with("Rev") { rev.clone() } else { format!("v{rev}") }));
    }
    if id.discs_total.map_or(false, |t| t > 1) || id.disc.map_or(false, |d| d > 1) {
        s.push_str(&format!(" (Disc {})", id.disc.unwrap_or(1)));
    }
    s
}

fn title_case_if_upper(s: &str) -> String {
    if s.chars().any(|c| c.is_lowercase()) || !s.chars().any(|c| c.is_alphabetic()) {
        return s.to_string();
    }
    s.split(' ')
        .map(|w| {
            let mut c = w.chars();
            match c.next() {
                Some(f) => f.to_string() + &c.as_str().to_lowercase(),
                None => String::new(),
            }
        })
        .collect::<Vec<_>>()
        .join(" ")
}

// ---------------------------------------------------------------- cache

fn cache_path() -> PathBuf {
    paths::user_cache_dir().join("resolve-cache.json")
}

fn cache_get(key: &str, max_days: i64) -> Option<Resolution> {
    let v = json::parse(&std::fs::read_to_string(cache_path()).ok()?).ok()?;
    let e = v.get(key);
    let t = e["time"].as_i64()?;
    if util::now_secs() - t > max_days * 86400 {
        return None;
    }
    // Base de référence mise à jour depuis : l'entrée est périmée.
    let db_mtime = std::fs::metadata(refdb::db_path()).ok().and_then(|m| m.modified().ok()).and_then(|m| m.duration_since(std::time::UNIX_EPOCH).ok()).map(|d| d.as_secs() as i64);
    if db_mtime.is_some_and(|m| m >= t) {
        return None;
    }
    Resolution::from_value(&e["resolution"])
}

fn cache_put(key: &str, r: &Resolution) {
    let p = cache_path();
    let mut v = std::fs::read_to_string(&p).ok().and_then(|t| json::parse(&t).ok()).unwrap_or_else(Value::obj);
    v.set(key, jobj! {"time" => util::now_secs(), "resolution" => r.to_value()});
    let _ = paths::write_atomic(&p, v.to_json().as_bytes());
}

// ---------------------------------------------------------------- chaîne

pub struct ResolveCtx<'a> {
    pub cfg: &'a Config,
    pub manifest: &'a Manifest,
    pub refdb: &'a RefDb,
    pub collection: Option<&'a crate::collection::Collection>,
    pub use_cache: bool,
}

pub fn resolution_from_name(name: &str, confidence: NameConfidence, source: &str, id: &Identity, refdb: Option<&RefDb>) -> Resolution {
    let game = refdb::game_name(name);
    let disc = refdb::disc_of(name).or(id.disc);
    let discs = refdb.and_then(|r| r.disc_count(&id.system, &game)).or(id.discs_total);
    Resolution { name: name.to_string(), game, disc, discs, confidence, source: source.into() }
}

/// Chaîne : cache → collection → base locale → résolveurs externes → repli.
pub fn resolve(id: &Identity, phys: &Physical, rc: &ResolveCtx) -> Resolution {
    let key = id.key();
    if let Some(col) = rc.collection {
        if let Some(e) = col.find_key(&key, Some(&rc.cfg.roms_dir())) {
            // Nom canonique enregistré au dump (le fichier a pu être renommé depuis).
            let name = e.canonical_name.clone().or_else(|| Path::new(&e.path).file_stem().and_then(|s| s.to_str()).map(|s| s.to_string()));
            // Un nom provisoire enregistré au dump ne fait pas autorité : on
            // cherche mieux (base de référence importée depuis, résolveurs).
            let provisional = e.name_source.as_deref() == Some("fallback") || name.as_deref() == Some(fallback_name(id).as_str());
            if let (Some(name), false) = (name, provisional) {
                let conf = if e.verified.as_deref() == Some("ok") { NameConfidence::Exact } else { NameConfidence::Probable };
                return resolution_from_name(&name, conf, "collection", id, Some(rc.refdb));
            }
        }
    }
    let mut best: Option<Resolution> = None;
    if let Some((name, exact)) = rc.refdb.predict(id, phys) {
        let r = resolution_from_name(&name, if exact { NameConfidence::Exact } else { NameConfidence::Probable }, "redump-local", id, Some(rc.refdb));
        if exact {
            cache_put(&key, &r);
            return r;
        }
        best = Some(r);
    }
    // Cache des résolveurs (en ligne notamment), seulement si la base locale
    // n'a rien donné : la base, mise à jour, l'emporte toujours.
    if best.is_none() && rc.use_cache {
        if let Some(r) = cache_get(&key, rc.cfg.cache_days()) {
            return r;
        }
    }
    for name in rc.manifest.resolver_chain() {
        if name == "redump-local" {
            continue;
        }
        let input = jobj! {
            "identity" => id.to_value(), "physical" => phys.to_value(),
            "online" => rc.cfg.online_source_enabled(&name),
            "timeout" => rc.cfg.online_timeout(),
        };
        let exe = format!("disc-launcher-resolve-{name}");
        let Ok(res) = handlers::call(&exe, "resolve", &[], &input, &[], Duration::from_secs(rc.cfg.online_timeout() + 2)) else { continue };
        if res.code != 0 {
            continue;
        }
        for c in res.out["candidates"].as_arr() {
            let Some(n) = c["name"].as_str() else { continue };
            let conf = NameConfidence::parse(c["confidence"].str_or("probable"));
            if best.as_ref().map_or(true, |b| conf > b.confidence) {
                let mut r = resolution_from_name(n, conf, &format!("resolve-{name}"), id, Some(rc.refdb));
                if let Some(d) = c["discs"].as_i64() {
                    r.discs = Some(d as u32);
                }
                best = Some(r);
            }
        }
        if best.as_ref().map_or(false, |b| b.confidence == NameConfidence::Exact) {
            break;
        }
    }
    let r = best.unwrap_or_else(|| resolution_from_name(&fallback_name(id), NameConfidence::Fallback, "fallback", id, None));
    if r.confidence > NameConfidence::Fallback {
        cache_put(&key, &r);
    }
    r
}

// ---------------------------------------------------------------- cible

#[derive(Clone, Debug, PartialEq)]
pub struct Target {
    /// Dossier système : `~/ROMs/psx`.
    pub system_dir: PathBuf,
    pub folder: String,
    /// Fichier (ou dossier) final.
    pub path: PathBuf,
    /// Nom sans extension (sert de base aux fichiers produits).
    pub stem: String,
    pub ext: String,
    /// Dossier de jeu `Jeu.m3u/` (disposition m3u-dir).
    pub game_dir: Option<PathBuf>,
    /// Fichier `.m3u` à maintenir.
    pub m3u: Option<PathBuf>,
}

impl Target {
    pub fn to_value(&self) -> Value {
        jobj! {
            "system_dir" => self.system_dir.to_string_lossy().into_owned(), "folder" => self.folder.clone(),
            "path" => self.path.to_string_lossy().into_owned(), "stem" => self.stem.clone(), "ext" => self.ext.clone(),
            "game_dir" => self.game_dir.as_ref().map(|p| p.to_string_lossy().into_owned()),
            "m3u" => self.m3u.as_ref().map(|p| p.to_string_lossy().into_owned()),
        }
    }
    pub fn from_value(v: &Value) -> Option<Target> {
        Some(Target {
            system_dir: PathBuf::from(v["system_dir"].as_str()?),
            folder: v["folder"].string()?,
            path: PathBuf::from(v["path"].as_str()?),
            stem: v["stem"].string()?,
            ext: v["ext"].string()?,
            game_dir: v["game_dir"].as_str().map(PathBuf::from),
            m3u: v["m3u"].as_str().map(PathBuf::from),
        })
    }
    /// Chemin relatif à la racine des ROMs, pour l'affichage.
    pub fn display(&self, roms: &Path) -> String {
        self.path.strip_prefix(roms).unwrap_or(&self.path).to_string_lossy().into_owned()
    }
}

/// Construit la cible à partir de la résolution (section 8.3).
pub fn build_target(cfg: &Config, m: &Manifest, id: &Identity, r: &Resolution) -> Target {
    let roms = cfg.roms_dir();
    let folder = m.folder_for(id.region.as_deref(), cfg.regional_dirs());
    let system_dir = roms.join(&folder);
    let portable = cfg.portable_names();
    let ext = m.target_format();
    let tpl = cfg.handler(&m.id)["name_template"].as_str().unwrap_or("{name}").to_string();
    let vars = [
        ("name", r.name.clone()),
        ("title", id.title.clone().unwrap_or_else(|| r.game.clone())),
        ("region", id.region.clone().unwrap_or_default()),
        ("disc", r.disc.or(id.disc).unwrap_or(1).to_string()),
        ("serial", id.serial.clone().or(id.game_id.clone()).unwrap_or_default()),
        ("revision", id.revision.clone().unwrap_or_default()),
    ];
    let suffix = format!(".{ext}");
    let stem_full = fit(&sanitize(&util::render(&tpl, &vars), portable), &suffix, 255);
    let stem = stem_full.trim_end_matches(&suffix).to_string();
    let file_name = format!("{stem}.{ext}");
    let multidisc = m.multidisc() && r.multidisc();
    let (path, game_dir, m3u) = if multidisc {
        let game = fit(&sanitize(&r.game, portable), ".m3u", 255);
        match cfg.multidisc_layout() {
            MultidiscLayout::M3uDir => {
                let gd = system_dir.join(&game);
                let m3u = gd.join(&game);
                (gd.join(&file_name), Some(gd), Some(m3u))
            }
            MultidiscLayout::FlatM3u => (system_dir.join(&file_name), None, Some(system_dir.join(&game))),
            MultidiscLayout::Flat => (system_dir.join(&file_name), None, None),
        }
    } else {
        (system_dir.join(&file_name), None, None)
    };
    Target { system_dir, folder, path, stem, ext, game_dir, m3u }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::json::Value;

    fn manifest(id: &str, src: &str) -> Manifest {
        Manifest { id: id.into(), raw: crate::toml::parse(src).unwrap() }
    }

    #[test]
    fn sanitize_and_fit() {
        assert_eq!(sanitize("Halo: Combat Evolved / Edition?", true), "Halo- Combat Evolved - Edition_");
        assert_eq!(sanitize("A: B", false), "A: B");
        assert_eq!(sanitize("trailing.. ", true), "trailing");
        let long = "x".repeat(300) + " (Disc 2)";
        let f = fit(&long, ".chd", 255);
        assert!(f.len() <= 255 && f.ends_with(" (Disc 2).chd"));
    }

    #[test]
    fn fallback_label_is_serial() {
        let id = Identity { system: "psx".into(), serial: Some("SLES-02905".into()), label: Some("SLES_029.05".into()), region: Some("Europe".into()), disc: Some(1), ..Default::default() };
        assert_eq!(fallback_name(&id), "SLES-02905 (Europe)");
    }

    #[test]
    fn fallback() {
        let id = Identity { system: "saturn".into(), serial: Some("MK-81019".into()), title: Some("PANZER DRAGOON".into()), region: Some("Europe".into()), disc: Some(2), discs_total: Some(2), ..Default::default() };
        assert_eq!(fallback_name(&id), "Panzer Dragoon [MK-81019] (Europe) (Disc 2)");
    }

    #[test]
    fn targets() {
        let cfg = Config::from_value(crate::toml::parse("[general]\nroms_dir = \"/r\"\n").unwrap());
        let m = manifest("megacd", "[handler]\nid=\"megacd\"\nregional_folders = { Japan = \"megacdjp\", USA = \"segacd\", default = \"megacd\" }\n[formats]\ntarget=\"chd\"\nmultidisc=true\n");
        let id = Identity { system: "megacd".into(), region: Some("Japan".into()), ..Default::default() };
        let r = resolution_from_name("Sonic CD (Japan)", NameConfidence::Exact, "t", &id, None);
        let t = build_target(&cfg, &m, &id, &r);
        assert_eq!(t.path, PathBuf::from("/r/megacdjp/Sonic CD (Japan).chd"));

        let id = Identity { system: "psx".into(), region: Some("France".into()), ..Default::default() };
        let mp = manifest("psx", "[handler]\nid=\"psx\"\n[formats]\ntarget=\"chd\"\nmultidisc=true\n");
        let r = resolution_from_name("Final Fantasy VII (France) (Disc 1)", NameConfidence::Exact, "t", &id, None);
        let t = build_target(&cfg, &mp, &id, &r);
        assert_eq!(t.path, PathBuf::from("/r/psx/Final Fantasy VII (France).m3u/Final Fantasy VII (France) (Disc 1).chd"));
        assert_eq!(t.m3u, Some(PathBuf::from("/r/psx/Final Fantasy VII (France).m3u/Final Fantasy VII (France).m3u")));
        assert_eq!(Target::from_value(&t.to_value()).unwrap(), t);
        let _ = Value::Null;
    }
}
