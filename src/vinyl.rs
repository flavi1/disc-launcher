//! Platine vinyle pilotée par vinylrip (https://github.com/flavi1/vinylrip).
//!
//! Si `vinylrip` est installé et que sa configuration
//! (`~/.config/vinylrip/config.toml`) désigne une platine (`device = …`), la
//! carte son correspondante devient un « disque » de type vinyle dès qu'elle
//! est branchée. La correspondance suit celle de vinylrip : identifiant de
//! carte ALSA, `vendeur:produit` USB, ou partie du nom.
//!
//! - Écouter : `vinylrip --json listen --device …` (arecord | aplay), comme
//!   l'extension vinylrip-kodi ; arrêté en fermant son entrée standard.
//! - Encoder : `vinylrip record --device …` dans un terminal (interactif :
//!   Entrée pour démarrer et terminer chaque face, puis traitement).

use crate::paths;
use std::io;
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Turntable {
    /// Valeur `device` de la configuration de vinylrip.
    pub wanted: String,
    /// Identifiant ALSA de la carte (`Device`).
    pub card_id: String,
    pub name: String,
    pub index: u32,
}

/// Fichier de configuration de vinylrip.
pub fn config_path() -> PathBuf {
    let base = std::env::var("XDG_CONFIG_HOME").ok().filter(|s| !s.is_empty()).map(PathBuf::from).unwrap_or_else(|| paths::home().join(".config"));
    base.join("vinylrip").join("config.toml")
}

/// Platine désignée par la configuration de vinylrip (clé `device`, à la
/// racine ou dans une section : vinylrip aplatit ses sections), si vinylrip
/// est installé.
pub fn configured_device() -> Option<String> {
    paths::which("vinylrip")?;
    let text = std::fs::read_to_string(config_path()).ok()?;
    device_from_config(&text)
}

pub fn device_from_config(text: &str) -> Option<String> {
    let v = crate::toml::parse(text).ok()?;
    let mut found = v["device"].as_str().map(|s| s.to_string());
    if found.is_none() {
        if let Some(obj) = v.as_obj() {
            found = obj.values().find_map(|t| t["device"].as_str().map(|s| s.to_string()));
        }
    }
    found.map(|s| s.trim().to_string()).filter(|s| !s.is_empty())
}

/// Cartes de `/proc/asound/cards` : (index, identifiant, nom).
pub fn parse_cards(text: &str) -> Vec<(u32, String, String)> {
    let mut out = vec![];
    for line in text.lines() {
        // " 1 [Device         ]: USB-Audio - USB Audio CODEC"
        let l = line.trim_start();
        let Some((idx, rest)) = l.split_once(' ') else { continue };
        let Ok(index) = idx.parse::<u32>() else { continue };
        let Some(rest) = rest.trim_start().strip_prefix('[') else { continue };
        let Some((id, rest)) = rest.split_once(']') else { continue };
        let name = rest.split_once(" - ").map(|(_, n)| n.trim()).unwrap_or("").to_string();
        out.push((index, id.trim().to_string(), name));
    }
    out
}

/// Carte correspondant à `wanted`, comme vinylrip : identifiant ou
/// `vendeur:produit` exacts d'abord, puis partie du nom.
pub fn find(wanted: &str, cards: &[(u32, String, String)], usb_id: impl Fn(u32) -> String) -> Option<Turntable> {
    let w = wanted.trim().to_lowercase();
    if w.is_empty() {
        return None;
    }
    let mk = |c: &(u32, String, String)| Turntable { wanted: wanted.to_string(), card_id: c.1.clone(), name: c.2.clone(), index: c.0 };
    if let Some(c) = cards.iter().find(|c| c.1.to_lowercase() == w || usb_id(c.0).to_lowercase() == w) {
        return Some(mk(c));
    }
    cards.iter().find(|c| c.2.to_lowercase().contains(&w)).map(mk)
}

fn read_usb_id(index: u32) -> String {
    std::fs::read_to_string(format!("/proc/asound/card{index}/usbid")).map(|s| s.trim().to_string()).unwrap_or_default()
}

/// Platine configurée et branchée, s'il y en a une.
pub fn detect() -> Option<Turntable> {
    let wanted = configured_device()?;
    // DISC_LAUNCHER_ASOUND_CARDS : autre fichier de cartes (tests).
    let path = std::env::var("DISC_LAUNCHER_ASOUND_CARDS").unwrap_or_else(|_| "/proc/asound/cards".into());
    let cards = std::fs::read_to_string(path).ok()?;
    find(&wanted, &parse_cards(&cards), read_usb_id)
}

/// Lance l'écoute ; l'arrêter avec `stop_listen`.
pub fn start_listen(t: &Turntable, log: &Path) -> io::Result<Child> {
    if let Some(d) = log.parent() {
        let _ = std::fs::create_dir_all(d);
    }
    let out = std::fs::OpenOptions::new().create(true).append(true).open(log)?;
    let err = out.try_clone()?;
    Command::new("vinylrip").args(["--json", "listen", "--device", &t.wanted]).stdin(Stdio::piped()).stdout(out).stderr(err).spawn()
}

/// Écoute servie en flux WAV sur 127.0.0.1 (`vinylrip --json listen --http`),
/// jouée par Kodi avec sa propre sortie audio. `on_url` reçoit l'adresse du flux.
pub fn start_listen_http(t: &Turntable, log: &Path, on_url: impl FnOnce(String) + Send + 'static) -> io::Result<Child> {
    if let Some(d) = log.parent() {
        let _ = std::fs::create_dir_all(d);
    }
    let err = std::fs::OpenOptions::new().create(true).append(true).open(log)?;
    let mut child = Command::new("vinylrip").args(["--json", "listen", "--http", "--device", &t.wanted]).stdin(Stdio::piped()).stdout(Stdio::piped()).stderr(err).spawn()?;
    let out = child.stdout.take().ok_or_else(|| io::Error::other("stdout"))?;
    std::thread::spawn(move || {
        use std::io::BufRead;
        let mut on_url = Some(on_url);
        for line in std::io::BufReader::new(out).lines().map_while(Result::ok) {
            if let Ok(v) = crate::json::parse(line.trim()) {
                if let Some(u) = v["url"].as_str() {
                    if let Some(f) = on_url.take() {
                        f(u.to_string());
                    }
                }
            }
        }
    });
    Ok(child)
}

/// Arrête l'écoute : fermeture de l'entrée (vinylrip coupe arecord et aplay),
/// puis arrêt forcé après 3 s.
pub fn stop_listen(mut child: Child) {
    drop(child.stdin.take());
    for _ in 0..30 {
        if child.try_wait().ok().flatten().is_some() {
            return;
        }
        std::thread::sleep(std::time::Duration::from_millis(100));
    }
    let _ = child.kill();
    let _ = child.wait();
}

/// Commande d'encodage (à lancer dans un terminal).
pub fn record_argv(t: &Turntable) -> Vec<String> {
    vec!["vinylrip".into(), "record".into(), "--device".into(), t.wanted.clone()]
}

#[cfg(test)]
mod tests {
    use super::*;
    const CARDS: &str = " 0 [PCH            ]: HDA-Intel - HDA Intel PCH\n                      HDA Intel PCH at 0xf7f10000 irq 32\n 1 [Device         ]: USB-Audio - USB Audio CODEC\n                      BURR-BROWN USB Audio CODEC at usb-0000:00:14.0-2, full speed\n";

    #[test]
    fn config_and_match() {
        assert_eq!(device_from_config("device = \"Device\"\n[output]\ndelete_raw = true\n").as_deref(), Some("Device"));
        assert_eq!(device_from_config("[capture]\ndevice = \"08bb:2902\"\n").as_deref(), Some("08bb:2902"));
        assert_eq!(device_from_config("device = \"\"\n"), None);
        let cards = parse_cards(CARDS);
        assert_eq!(cards, vec![(0, "PCH".into(), "HDA Intel PCH".into()), (1, "Device".into(), "USB Audio CODEC".into())]);
        let usb = |i: u32| if i == 1 { "08bb:2902".to_string() } else { String::new() };
        assert_eq!(find("Device", &cards, usb).unwrap().index, 1);
        assert_eq!(find("08BB:2902", &cards, usb).unwrap().card_id, "Device");
        assert_eq!(find("codec", &cards, usb).unwrap().index, 1);
        assert_eq!(find("Behringer", &cards, usb), None);
        assert_eq!(record_argv(&find("Device", &cards, usb).unwrap()), ["vinylrip", "record", "--device", "Device"]);
    }
}
