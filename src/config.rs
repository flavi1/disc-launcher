//! Configuration : `/etc/disc-launcher/config.toml` puis
//! `~/.config/disc-launcher/config.toml`, fusionnés clé par clé.

use crate::json::{self, Value};
use crate::paths;
use std::path::{Path, PathBuf};

#[derive(Clone, Debug)]
pub struct Config {
    pub raw: Value,
    pub errors: Vec<String>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Mode {
    Daemon,
    Hybrid,
    NativeOnly,
}

impl Mode {
    pub fn name(self) -> &'static str {
        match self {
            Mode::Daemon => "daemon",
            Mode::Hybrid => "hybride",
            Mode::NativeOnly => "natif-seul",
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Policy {
    Ask,
    AutoPlay,
    AutoDump,
    DumpThenPlay,
    Ignore,
}

impl Policy {
    pub fn parse(s: &str) -> Option<Policy> {
        Some(match s {
            "ask" => Policy::Ask,
            "auto-play" => Policy::AutoPlay,
            "auto-dump" => Policy::AutoDump,
            "dump-then-play" => Policy::DumpThenPlay,
            "ignore" => Policy::Ignore,
            _ => return None,
        })
    }
    pub fn name(self) -> &'static str {
        match self {
            Policy::Ask => "ask",
            Policy::AutoPlay => "auto-play",
            Policy::AutoDump => "auto-dump",
            Policy::DumpThenPlay => "dump-then-play",
            Policy::Ignore => "ignore",
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum MultidiscLayout {
    M3uDir,
    FlatM3u,
    Flat,
}

/// Valeurs par défaut (doivent rester cohérentes avec `data/config.toml`).
const DEFAULTS: &str = r#"
[general]
mode = "daemon"
roms_dir = ""
regional_dirs = true
portable_names = true
multidisc_layout = "m3u-dir"
notify_on_startup = false
eject_after_read = true
max_conversions = 1
helper = "auto"
terminal = true
terminal_command = []

[naming]
rename_after_verify = true
redump_keep_previous = true
reference_dats = "~/.local/share/disc-launcher/dat/"

[online]
enabled = false
sources = []
timeout = 5
cache_days = 90

[policy]
default = "ask"
dos = "ignore"
windows = "ignore"
photo-cd = "ignore"

[handlers.defaults]
console = "disc-launcher-generic"
media = "disc-launcher-media-generic"
data = "disc-launcher-data-generic"

[handlers.dcim]
player = "images"

[media]
player = "auto"
auto_order = ["kodi", "mpv", "vlc", "images"]

[media.players.kodi]
name = "Kodi"
command = ["disc-launcher-player-kodi"]
data-audio = ["disc-launcher-player-kodi", "--open", "{playlist}"]
data-video = ["disc-launcher-player-kodi", "--open", "{playlist}"]
dcim = ["disc-launcher-player-kodi", "--open", "{dcim}"]
requires = "kodi"

[media.players.mpv]
name = "mpv"
cdda = ["mpv", "--force-window", "cdda://", "--cdrom-device={device}"]
dvd-video = ["mpv", "dvd://", "--dvd-device={device}"]
bluray-video = ["mpv", "bd://", "--bluray-device={device}"]
data-audio = ["mpv", "--force-window", "--playlist={playlist}"]
data-video = ["mpv", "--fullscreen", "--playlist={playlist}"]
default = ["mpv", "{mount}"]

[media.players.vlc]
name = "VLC"
cdda = ["vlc", "cdda://{device}"]
dvd-video = ["vlc", "dvd://{device}"]
bluray-video = ["vlc", "bluray://{device}"]
vcd = ["vlc", "vcd://{device}"]
svcd = ["vlc", "vcd://{device}"]
data-audio = ["vlc", "{playlist}"]
data-video = ["vlc", "--fullscreen", "{playlist}"]
default = ["vlc", "{mount}"]

[media.players.images]
name = "Visionneuse d'images"
dcim = ["xdg-open", "{dcim}"]
requires = "xdg-open"

[retroarch]
command = ["retroarch"]

[hooks]
on_dumped = []
timeout = 600

[log]
level = "info"
syslog = false
max_size_mb = 5
max_files = 5
"#;

pub fn system_config_path() -> PathBuf {
    paths::sysconf_dir().join("config.toml")
}
pub fn user_config_path() -> PathBuf {
    paths::user_config_dir().join("config.toml")
}

impl Config {
    pub fn load() -> Config {
        Self::load_from(&[system_config_path(), user_config_path()])
    }

    pub fn load_from(files: &[PathBuf]) -> Config {
        let mut raw = crate::toml::parse(DEFAULTS).expect("défauts TOML valides");
        let mut errors = vec![];
        for f in files {
            if !f.is_file() {
                continue;
            }
            match std::fs::read_to_string(f) {
                Ok(t) => match crate::toml::parse(&t) {
                    Ok(v) => json::merge(&mut raw, &v),
                    Err(e) => errors.push(format!("{} : {e}", f.display())),
                },
                Err(e) => errors.push(format!("{} : {e}", f.display())),
            }
        }
        Config { raw, errors }
    }

    pub fn from_value(v: Value) -> Config {
        let mut raw = crate::toml::parse(DEFAULTS).unwrap();
        json::merge(&mut raw, &v);
        Config { raw, errors: vec![] }
    }

    pub fn mode(&self) -> Mode {
        match self.raw.path("general.mode").str_or("daemon") {
            "hybride" | "hybrid" => Mode::Hybrid,
            "natif-seul" | "native-only" => Mode::NativeOnly,
            _ => Mode::Daemon,
        }
    }

    /// Racine des ROMs : configuration, sinon réglage ES-DE, sinon `~/ROMs`.
    pub fn roms_dir(&self) -> PathBuf {
        let s = self.raw.path("general.roms_dir").str_or("");
        if !s.is_empty() {
            return paths::expand(s);
        }
        esde_rom_directory().unwrap_or_else(|| paths::home().join("ROMs"))
    }

    pub fn regional_dirs(&self) -> bool {
        self.raw.path("general.regional_dirs").bool_or(true)
    }
    pub fn portable_names(&self) -> bool {
        self.raw.path("general.portable_names").bool_or(true)
    }
    pub fn multidisc_layout(&self) -> MultidiscLayout {
        match self.raw.path("general.multidisc_layout").str_or("m3u-dir") {
            "flat-m3u" => MultidiscLayout::FlatM3u,
            "flat" => MultidiscLayout::Flat,
            _ => MultidiscLayout::M3uDir,
        }
    }
    pub fn notify_on_startup(&self) -> bool {
        self.raw.path("general.notify_on_startup").bool_or(false)
    }
    pub fn eject_after_read(&self) -> bool {
        self.raw.path("general.eject_after_read").bool_or(true)
    }
    pub fn max_conversions(&self) -> usize {
        self.raw.path("general.max_conversions").i64_or(1).clamp(1, 16) as usize
    }
    /// `auto` | `pkexec` | `none`
    /// Ouvrir un terminal qui suit chaque tâche de dump.
    pub fn job_terminal(&self) -> bool {
        self.raw.path("general.terminal").bool_or(true)
    }
    pub fn helper_mode(&self) -> String {
        self.raw.path("general.helper").str_or("auto").to_string()
    }
    pub fn rename_after_verify(&self) -> bool {
        self.raw.path("naming.rename_after_verify").bool_or(true)
    }
    pub fn redump_keep_previous(&self) -> bool {
        self.raw.path("naming.redump_keep_previous").bool_or(true)
    }
    pub fn reference_dats_dir(&self) -> PathBuf {
        paths::expand(self.raw.path("naming.reference_dats").str_or("~/.local/share/disc-launcher/dat/"))
    }
    pub fn online_enabled(&self) -> bool {
        self.raw.path("online.enabled").bool_or(false)
    }
    pub fn online_source_enabled(&self, name: &str) -> bool {
        self.online_enabled() && self.raw.path("online.sources").strings().iter().any(|s| s == name)
    }
    pub fn online_timeout(&self) -> u64 {
        self.raw.path("online.timeout").i64_or(5).clamp(1, 60) as u64
    }
    pub fn cache_days(&self) -> i64 {
        self.raw.path("online.cache_days").i64_or(90).max(0)
    }

    /// Politique d'un système ou d'un média ; repli sur `media` pour les médias, puis `default`.
    pub fn policy(&self, system: &str) -> Policy {
        self.policy_for(system, false)
    }

    pub fn policy_for(&self, id: &str, media: bool) -> Policy {
        let p = self.raw.get("policy");
        p.get(id)
            .as_str()
            .and_then(Policy::parse)
            .or_else(|| if media { p.get("media").as_str().and_then(Policy::parse) } else { None })
            .or_else(|| p.get("default").as_str().and_then(Policy::parse))
            .unwrap_or(Policy::Ask)
    }

    /// Profil forcé d'un lecteur : `auto` par défaut.
    pub fn drive_profile(&self, dev: &str) -> String {
        let d = self.raw.get("drives");
        let short = dev.trim_start_matches("/dev/");
        d.get(dev).get("profile").as_str().or_else(|| d.get(short).get("profile").as_str()).unwrap_or("auto").to_string()
    }

    /// Arguments supplémentaires d'un outil pour un lecteur :
    /// `[drives."/dev/sr0"] redumper_args = [...]`.
    pub fn drive_tool_args(&self, dev: &str, tool: &str) -> Vec<String> {
        let d = self.raw.get("drives");
        let short = dev.trim_start_matches("/dev/");
        let key = format!("{tool}_args");
        let v = d.get(dev).get(&key).strings();
        if v.is_empty() {
            d.get(short).get(&key).strings()
        } else {
            v
        }
    }

    /// Section `[handlers.<id>]`.
    pub fn handler(&self, id: &str) -> &Value {
        self.raw.get("handlers").get(id)
    }
}

/// Cherche `ROMDirectory` dans la configuration d'ES-DE (ou d'EmulationStation).
pub fn esde_rom_directory() -> Option<PathBuf> {
    let home = paths::home();
    let mut candidates = vec![];
    if let Some(d) = std::env::var_os("ESDE_APPDATA_DIR") {
        candidates.push(PathBuf::from(d).join("settings/es_settings.xml"));
    }
    candidates.push(home.join("ES-DE/settings/es_settings.xml"));
    candidates.push(home.join(".emulationstation/es_settings.xml"));
    candidates.push(home.join(".emulationstation/es_settings.cfg"));
    for c in candidates {
        if let Some(p) = rom_dir_from_settings(&c) {
            return Some(p);
        }
    }
    None
}

fn rom_dir_from_settings(file: &Path) -> Option<PathBuf> {
    let text = std::fs::read_to_string(file).ok()?;
    for line in text.lines() {
        if line.contains("name=\"ROMDirectory\"") {
            let i = line.find("value=\"")? + 7;
            let j = line[i..].find('"')? + i;
            let v = crate::xml::decode_entities(&line[i..j]);
            if v.trim().is_empty() {
                return None;
            }
            let v = v.replace("%ESPATH%", &paths::home().to_string_lossy());
            return Some(paths::expand(&v));
        }
    }
    None
}

/// Écrit `key = value` dans `[section]` du fichier de configuration
/// utilisateur.
pub fn set_user_value(section: &str, key: &str, value: &Value) -> std::io::Result<()> {
    let path = user_config_path();
    let text = std::fs::read_to_string(&path).unwrap_or_default();
    let new = crate::toml::set_key_in_text(&text, section, key, value);
    crate::toml::parse(&new).map_err(|e| std::io::Error::new(std::io::ErrorKind::InvalidData, e.to_string()))?;
    paths::write_atomic(&path, new.as_bytes())
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn defaults_and_policy() {
        let c = Config::from_value(crate::toml::parse("[policy]\npsx = \"auto-play\"\n[drives.sr0]\nprofile = \"omnidrive\"\n").unwrap());
        assert_eq!(c.policy("psx"), Policy::AutoPlay);
        assert_eq!(c.policy("gc"), Policy::Ask);
        assert_eq!(c.policy("dos"), Policy::Ignore);
        assert_eq!(c.drive_profile("/dev/sr0"), "omnidrive");
        assert_eq!(c.mode(), Mode::Daemon);
        assert_eq!(c.multidisc_layout(), MultidiscLayout::M3uDir);
    }
}

#[cfg(test)]
mod shipped {
    #[test]
    fn shipped_config_parses() {
        let v = crate::toml::parse(include_str!("../data/config.toml")).unwrap();
        assert_eq!(v.path("media.players.vlc.name").as_str(), Some("VLC"));
        assert_eq!(v.path("handlers.defaults.media").as_str(), Some("disc-launcher-media-generic"));
    }
}
