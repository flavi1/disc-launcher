//! Gestionnaire RetroArch : un seul exécutable pour toutes les consoles, avec
//! détection du cœur libretro.
//!
//! Choix du cœur, dans l'ordre :
//! 1. `[retroarch.cores] <système> = "nom_du_cœur"` de la configuration ;
//! 2. le premier cœur installé de la liste de préférence intégrée ;
//! 3. tout cœur installé dont le fichier `.info` déclare la base de données
//!    libretro du système et accepte l'extension du fichier.
//!
//! ```toml
//! [retroarch]
//! command = ["retroarch"]          # Flatpak : ["flatpak", "run", "org.libretro.RetroArch"]
//! cores_dir = "~/.config/retroarch/cores"
//! [retroarch.cores]
//! psx = "mednafen_psx_hw"
//! ```

use crate::config::Config;
use crate::paths;
use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

/// Base de données libretro (champ `database` des `.info`) par système.
pub fn database_of(system: &str) -> Option<&'static str> {
    Some(match system {
        "psx" => "Sony - PlayStation",
        "ps2" => "Sony - PlayStation 2",
        "saturn" => "Sega - Saturn",
        "megacd" => "Sega - Mega-CD - Sega CD",
        "dreamcast" => "Sega - Dreamcast",
        "naomigd" => "Sega - Naomi",
        "pcenginecd" => "NEC - PC Engine CD - TurboGrafx-CD",
        "pcfx" => "NEC - PC-FX",
        "pc98" => "NEC - PC-98",
        "neogeocd" => "SNK - Neo Geo CD",
        "3do" => "The 3DO Company - 3DO",
        "cdimono1" => "Philips - CD-i",
        "amigacd32" => "Commodore - CD32",
        "cdtv" => "Commodore - CDTV",
        "atarijaguarcd" => "Atari - Jaguar",
        "gc" => "Nintendo - GameCube",
        "wii" => "Nintendo - Wii",
        "fmtowns" => "FM Towns",
        "dos" | "windows" => "DOS",
        "n64" => "Nintendo - Nintendo 64",
        "snes" => "Nintendo - Super Nintendo Entertainment System",
        "megadrive" => "Sega - Mega Drive - Genesis",
        "gb" => "Nintendo - Game Boy",
        "gbc" => "Nintendo - Game Boy Color",
        "gba" => "Nintendo - Game Boy Advance",
        "mastersystem" => "Sega - Master System - Mark III",
        "gamegear" => "Sega - Game Gear",
        _ => return None,
    })
}

/// Cœurs préférés par système (premier installé retenu).
pub fn preferred(system: &str) -> &'static [&'static str] {
    match system {
        "psx" => &["swanstation", "mednafen_psx_hw", "mednafen_psx", "pcsx_rearmed"],
        "ps2" => &["pcsx2"],
        "saturn" => &["mednafen_saturn", "yabasanshiro", "kronos", "yabause"],
        "megacd" => &["genesis_plus_gx", "picodrive"],
        "dreamcast" | "naomigd" => &["flycast"],
        "pcenginecd" => &["mednafen_pce", "mednafen_pce_fast"],
        "pcfx" => &["mednafen_pcfx"],
        "pc98" => &["np2kai"],
        "neogeocd" => &["neocd"],
        "3do" => &["opera"],
        "cdimono1" => &["same_cdi"],
        "amigacd32" | "cdtv" => &["puae"],
        "gc" | "wii" => &["dolphin"],
        "dos" | "windows" => &["dosbox_pure"],
        "n64" => &["mupen64plus_next", "parallel_n64"],
        "snes" => &["snes9x", "bsnes", "mesen-s"],
        "megadrive" | "mastersystem" | "gamegear" => &["genesis_plus_gx", "picodrive"],
        "gb" | "gbc" => &["gambatte", "sameboy", "gearboy"],
        "gba" => &["mgba", "vba_next"],
        _ => &[],
    }
}

#[derive(Clone, Debug, Default, PartialEq)]
pub struct CoreInfo {
    pub name: String,
    pub path: Option<PathBuf>,
    pub extensions: Vec<String>,
    pub databases: Vec<String>,
}

/// Lecture d'un fichier `.info` (lignes `clé = "valeur"`).
pub fn parse_info(text: &str) -> BTreeMap<String, String> {
    text.lines()
        .filter_map(|l| {
            let (k, v) = l.split_once('=')?;
            Some((k.trim().to_string(), v.trim().trim_matches('"').to_string()))
        })
        .collect()
}

pub fn cores_dirs(cfg: &Config) -> Vec<PathBuf> {
    let mut v = vec![];
    if let Some(d) = cfg.raw.path("retroarch.cores_dir").as_str().or_else(|| cfg.raw.path("handlers.retroarch.cores_dir").as_str()) {
        v.push(paths::expand(d));
    }
    let h = paths::home();
    v.push(paths::config_home().join("retroarch/cores"));
    v.push(h.join(".var/app/org.libretro.RetroArch/config/retroarch/cores"));
    v.push(PathBuf::from("/usr/lib/libretro"));
    v.push(PathBuf::from("/usr/lib/x86_64-linux-gnu/libretro"));
    v.push(PathBuf::from("/usr/lib64/libretro"));
    v
}

pub fn info_dirs(cfg: &Config) -> Vec<PathBuf> {
    let mut v = cores_dirs(cfg);
    if let Some(d) = cfg.raw.path("retroarch.info_dir").as_str() {
        v.insert(0, paths::expand(d));
    }
    v.push(paths::config_home().join("retroarch/info"));
    v.push(paths::home().join(".var/app/org.libretro.RetroArch/config/retroarch/info"));
    v.push(PathBuf::from("/usr/share/libretro/info"));
    v
}

/// Cœurs installés (`*_libretro.so`) avec leurs informations si disponibles.
pub fn installed_cores(cfg: &Config) -> Vec<CoreInfo> {
    let mut cores: BTreeMap<String, CoreInfo> = BTreeMap::new();
    for d in cores_dirs(cfg) {
        let Ok(it) = std::fs::read_dir(&d) else { continue };
        for e in it.flatten() {
            let n = e.file_name().to_string_lossy().into_owned();
            if let Some(core) = n.strip_suffix("_libretro.so") {
                cores.entry(core.to_string()).or_insert_with(|| CoreInfo { name: core.to_string(), path: Some(e.path()), ..Default::default() });
            }
        }
    }
    for d in info_dirs(cfg) {
        for c in cores.values_mut() {
            if !c.extensions.is_empty() {
                continue;
            }
            if let Ok(t) = std::fs::read_to_string(d.join(format!("{}_libretro.info", c.name))) {
                let i = parse_info(&t);
                c.extensions = i.get("supported_extensions").map(|s| s.split('|').map(|x| x.to_ascii_lowercase()).collect()).unwrap_or_default();
                c.databases = i.get("database").map(|s| s.split('|').map(|x| x.to_string()).collect()).unwrap_or_default();
            }
        }
    }
    cores.into_values().collect()
}

/// Cœur pour ce système et cette extension (vide = lecture du disque).
pub fn choose_core(cfg: &Config, system: &str, ext: &str) -> Option<CoreInfo> {
    let cores = installed_cores(cfg);
    let accepts = |c: &CoreInfo| ext.is_empty() || c.extensions.is_empty() || c.extensions.iter().any(|e| e == ext);
    if let Some(forced) = cfg.raw.path("retroarch.cores").get(system).as_str() {
        if let Some(c) = cores.iter().find(|c| c.name == forced) {
            return Some(c.clone());
        }
    }
    for p in preferred(system) {
        if let Some(c) = cores.iter().find(|c| c.name == *p && accepts(c)) {
            return Some(c.clone());
        }
    }
    let db = database_of(system)?;
    cores.into_iter().find(|c| c.databases.iter().any(|d| d.eq_ignore_ascii_case(db)) && accepts(c) && !c.extensions.is_empty())
}

pub fn command(cfg: &Config) -> Vec<String> {
    let c = cfg.raw.path("retroarch.command").strings();
    if c.is_empty() {
        vec!["retroarch".into()]
    } else {
        c
    }
}

/// Ligne de commande complète pour une copie existante ou le disque.
pub fn launch_command(cfg: &Config, system: &str, existing: Option<&Path>, device: Option<&str>) -> Result<Vec<String>, String> {
    let ext = existing.and_then(|p| p.extension()).map(|e| e.to_string_lossy().to_ascii_lowercase()).unwrap_or_default();
    let core = choose_core(cfg, system, &ext).ok_or_else(|| format!("aucun cœur libretro installé pour {system}{}", if ext.is_empty() { String::new() } else { format!(" (.{ext})") }))?;
    let mut cmd = command(cfg);
    cmd.push("-L".into());
    cmd.push(core.path.map(|p| p.to_string_lossy().into_owned()).unwrap_or(core.name));
    match (existing, device) {
        (Some(p), _) => cmd.push(p.to_string_lossy().into_owned()),
        (None, Some(d)) => {
            // Lecture du disque physique par RetroArch sous Linux.
            let n = d.trim_start_matches("/dev/sr").parse::<u32>().unwrap_or(0) + 1;
            cmd.push(format!("cdrom://drive{n}.cue"));
        }
        _ => return Err("--existing ou --disc attendu".into()),
    }
    Ok(cmd)
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn core_detection() {
        let d = std::env::temp_dir().join(format!("dl-ra-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&d);
        std::fs::create_dir_all(&d).unwrap();
        for c in ["pcsx_rearmed", "mednafen_saturn", "my_psx_core"] {
            std::fs::write(d.join(format!("{c}_libretro.so")), b"").unwrap();
        }
        std::fs::write(d.join("my_psx_core_libretro.info"), "display_name = \"X\"\nsupported_extensions = \"cue|chd\"\ndatabase = \"Sony - PlayStation\"\n").unwrap();
        std::fs::write(d.join("pcsx_rearmed_libretro.info"), "supported_extensions = \"bin|cue|img|pbp|chd\"\ndatabase = \"Sony - PlayStation\"\n").unwrap();
        let cfg = Config::from_value(crate::toml::parse(&format!("[retroarch]\ncores_dir = \"{}\"\n", d.display())).unwrap());
        assert_eq!(choose_core(&cfg, "psx", "chd").unwrap().name, "pcsx_rearmed");
        assert_eq!(choose_core(&cfg, "saturn", "chd").unwrap().name, "mednafen_saturn");
        assert!(choose_core(&cfg, "3do", "chd").is_none());
        let cfg2 = Config::from_value(crate::toml::parse(&format!("[retroarch]\ncores_dir = \"{}\"\n[retroarch.cores]\npsx = \"my_psx_core\"\n", d.display())).unwrap());
        let cmd = launch_command(&cfg2, "psx", Some(Path::new("/r/psx/A.chd")), None).unwrap();
        assert_eq!(cmd[0], "retroarch");
        assert!(cmd[2].ends_with("my_psx_core_libretro.so"));
        assert_eq!(cmd[3], "/r/psx/A.chd");
        let cmd = launch_command(&cfg2, "psx", None, Some("/dev/sr1")).unwrap();
        assert_eq!(cmd[3], "cdrom://drive2.cue");
        let _ = std::fs::remove_dir_all(&d);
    }
}
