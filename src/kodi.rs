//! Interface Kodi : l'extension `service.disc-launcher`
//! (https://github.com/flavi1/kodi-disc-launcher) se connecte à la socket de
//! contrôle du démon et s'annonce par `{"cmd":"attach-ui","ui":"kodi"}`. La
//! connexion reste ouverte : le démon y envoie l'état de la fenêtre
//! « Disques et périphériques » et les commandes Kodi des surcharges ;
//! l'extension renvoie les choix de l'utilisateur.
//!
//! Tant que l'extension est connectée (Kodi démarré), elle remplace la fenêtre
//! GTK et les notifications du bureau, en fenêtre comme en plein écran. Sans
//! port réseau ni réglage à activer dans Kodi.
//!
//! Démon → Kodi (une ligne JSON par message, champ `type`) :
//! - `state` : `{visible, raise, title, empty, items}`, comme pour la fenêtre GTK ;
//! - `notify` : `{title, message, icon, error}` ;
//! - `text` : `{title, text}` (explication longue, avec défilement) ;
//! - `play` : `{file, title?}` ; `play-disc` : `{device}` ; `play-album` : `{path}` ;
//! - `stop` : arrêter la lecture ; `stop-device` : `{device, paths, req}`
//!   (seulement si elle utilise ce disque), acquittée par `done` ;
//! - `open` : `{window: videos|music|pictures, path}` ;
//! - `library` : `{kind: video|music, path}` (mise à jour de la bibliothèque) ;
//! - `finished` : `{title, kind, path, folder}` (menu Lire / Ouvrir / Fermer).
//!
//! Kodi → démon : `hello {kodi, addon}`, `action {id, key}`, `hidden`,
//! `show` (comme un clic sur l'icône de la zone de notification), `done {req}`.
//!
//! ```toml
//! [kodi]
//! enabled = true          # false : jamais d'interface Kodi
//! library_update = true   # bibliothèque mise à jour après un encodage
//!
//! [kodi.override]         # surcharges, toutes actives par défaut
//! play-disc = false       # ex. : garder le lecteur de [media] même sous Kodi
//! ```

use crate::config::Config;
use crate::json::{self, Value};
use std::io::Write;
use std::os::unix::net::UnixStream;

/// Surcharges connues, avec leur effet (pour la documentation et `doctor`).
pub const OVERRIDES: &[(&str, &str)] = &[
    ("play-disc", "lecture du disque dans Kodi"),
    ("open-copy", "lecture de la copie encodée dans Kodi"),
    ("open-files", "fenêtre Vidéos, Musique ou Images de Kodi"),
    ("encode-disc", "encbd en arrière-plan, suivi dans Kodi"),
    ("eject", "lecture du disque arrêtée avant l'éjection"),
    ("listen", "écoute du vinyle par la sortie audio de Kodi"),
    ("play-existing", "lecture Kodi arrêtée avant de lancer un jeu"),
    ("notifications", "notifications dans Kodi"),
    ("custom", "actions personnalisées « terminal » en arrière-plan"),
];

pub fn enabled(cfg: &Config) -> bool {
    cfg.raw.path("kodi.enabled").bool_or(true)
}

pub fn library_update(cfg: &Config) -> bool {
    cfg.raw.path("kodi.library_update").bool_or(true)
}

/// Surcharge active pour cette clé d'action (vrai par défaut). Les actions
/// personnalisées suivent `"custom:<nom>"`, sinon `custom`.
pub fn override_on(cfg: &Config, key: &str) -> bool {
    let o = &cfg.raw.get("kodi")["override"];
    let k = match key {
        "open-tray" => "eject",
        "play-game" => "play-existing",
        "stop-listen" => "listen",
        k => k,
    };
    if let Some(b) = o.get(k).as_bool() {
        return b;
    }
    if k.starts_with(crate::custom::PREFIX) {
        return o["custom"].bool_or(true);
    }
    true
}

/// Fenêtre de Kodi où ouvrir les fichiers d'un média.
pub fn window_for(handler: &str, files_kind: Option<&str>) -> &'static str {
    match (handler, files_kind) {
        ("dcim" | "photo-cd", _) | (_, Some("photo")) => "pictures",
        ("cdda" | "dvd-audio", _) | (_, Some("audio")) => "music",
        _ => "videos",
    }
}

/// Médias que Kodi lit directement depuis le lecteur (`PlayDisc`).
pub fn plays_disc(handler: &str) -> bool {
    matches!(handler, "cdda" | "dvd-video" | "bluray-video" | "vcd" | "svcd")
}

/// Fenêtre de Kodi pour un dossier : d'après le média, sinon d'après son
/// contenu (dossier DCIM ou images → Images, sons → Musique, sinon Vidéos).
pub fn guess_window(handler: &str, dir: &std::path::Path) -> &'static str {
    let w = window_for(handler, None);
    if w != "videos" || !matches!(handler, "usb" | "data" | "") && !handler.starts_with("data") {
        return w;
    }
    if dir.join("DCIM").is_dir() || dir.join("dcim").is_dir() {
        return "pictures";
    }
    let (mut audio, mut video, mut photo, mut seen) = (0, 0, 0, 0);
    let mut stack = vec![(dir.to_path_buf(), 0)];
    while let Some((d, depth)) = stack.pop() {
        let Ok(it) = std::fs::read_dir(&d) else { continue };
        for e in it.flatten() {
            seen += 1;
            if seen > 2000 {
                break;
            }
            let name = e.file_name().to_string_lossy().into_owned();
            if e.file_type().is_ok_and(|t| t.is_dir()) {
                if depth < 2 && !name.starts_with('.') {
                    stack.push((e.path(), depth + 1));
                }
                continue;
            }
            let ext = name.rsplit_once('.').map(|(_, x)| x.to_ascii_lowercase()).unwrap_or_default();
            if ["jpg", "jpeg", "png", "heic", "heif", "webp", "gif", "tif", "tiff", "cr2", "nef", "arw", "dng"].contains(&ext.as_str()) {
                photo += 1;
            } else {
                match crate::data::file_kind(&name) {
                    Some("audio") => audio += 1,
                    Some("video") => video += 1,
                    _ => {}
                }
            }
        }
    }
    if photo > audio && photo > video {
        "pictures"
    } else if audio > video {
        "music"
    } else {
        "videos"
    }
}

/// Message reçu de l'extension.
#[derive(Clone, Debug, PartialEq)]
pub enum Msg {
    Hello(Value),
    Action(String, String),
    Hidden,
    /// Afficher la fenêtre (entrée « Disques et périphériques » de Kodi).
    Show,
    Done(u64),
    /// Connexion fermée (Kodi arrêté).
    Gone,
}

pub fn parse(line: &str) -> Option<Msg> {
    let v = json::parse(line.trim()).ok()?;
    match v["type"].as_str()? {
        "hello" => Some(Msg::Hello(v)),
        "action" => Some(Msg::Action(v["id"].as_str()?.to_string(), v["key"].as_str()?.to_string())),
        "hidden" => Some(Msg::Hidden),
        "show" => Some(Msg::Show),
        "done" => Some(Msg::Done(v["req"].i64_or(-1).max(0) as u64)),
        _ => None,
    }
}

/// Connexion de l'extension, côté démon (écriture).
pub struct Conn {
    pub id: u64,
    w: UnixStream,
    last_state: String,
    pub kodi: String,
    pub addon: String,
}

impl Conn {
    pub fn new(id: u64, w: UnixStream) -> Conn {
        let _ = w.set_write_timeout(Some(std::time::Duration::from_secs(5)));
        Conn { id, w, last_state: String::new(), kodi: String::new(), addon: String::new() }
    }

    pub fn send(&mut self, v: &Value) -> std::io::Result<()> {
        let mut line = v.to_json();
        line.push('\n');
        self.w.write_all(line.as_bytes())
    }

    /// État de la fenêtre, envoyé seulement s'il a changé.
    pub fn send_state(&mut self, state: &Value) -> std::io::Result<()> {
        let mut v = state.clone();
        v.set("type", "state");
        let line = v.to_json();
        if line == self.last_state {
            return Ok(());
        }
        self.send(&v)?;
        self.last_state = line;
        Ok(())
    }
}

pub fn msg(kind: &str) -> Value {
    let mut v = Value::obj();
    v.set("type", kind);
    v
}

#[cfg(test)]
mod tests {
    use super::*;

    fn cfg(text: &str) -> Config {
        Config { errors: vec![], raw: crate::toml::parse(text).unwrap() }
    }

    #[test]
    fn overrides_default_on() {
        let c = cfg("[kodi.override]\nplay-disc = false\ncustom = false\n\"custom:copie\" = true\n");
        assert!(!override_on(&c, "play-disc"));
        assert!(override_on(&c, "open-files"));
        assert!(override_on(&c, "open-tray"));
        assert!(!override_on(&c, "custom:autre"));
        assert!(override_on(&c, "custom:copie"));
        assert!(enabled(&cfg("")) && !enabled(&cfg("[kodi]\nenabled = false\n")));
    }

    #[test]
    fn messages() {
        assert_eq!(parse("{\"type\":\"action\",\"id\":\"/dev/sr0\",\"key\":\"eject\"}"), Some(Msg::Action("/dev/sr0".into(), "eject".into())));
        assert_eq!(parse("{\"type\":\"done\",\"req\":4}"), Some(Msg::Done(4)));
        assert_eq!(parse("{\"type\":\"hidden\"}"), Some(Msg::Hidden));
        assert_eq!(parse("bruit"), None);
        assert_eq!(window_for("dcim", None), "pictures");
        assert_eq!(window_for("usb", Some("audio")), "music");
        assert_eq!(window_for("bluray-video", None), "videos");
        let d = std::env::temp_dir().join(format!("dl-kodi-{}", std::process::id()));
        std::fs::create_dir_all(d.join("Album")).unwrap();
        for f in ["Album/01.flac", "Album/02.flac", "x.mkv"] {
            std::fs::write(d.join(f), b"").unwrap();
        }
        assert_eq!(guess_window("usb", &d), "music");
        std::fs::create_dir_all(d.join("DCIM")).unwrap();
        assert_eq!(guess_window("usb", &d), "pictures");
        assert_eq!(guess_window("bluray-video", &d), "videos");
        let _ = std::fs::remove_dir_all(d);
        assert!(plays_disc("cdda") && !plays_disc("psx"));
    }
}
