//! Ouverture d'un dossier dans le gestionnaire de fichiers de l'utilisateur.
//!
//! Ordre : réglage `[general] file_manager` ; `xdg-open` (qui suit l'association
//! XDG `inode/directory` du bureau) ; interface D-Bus
//! `org.freedesktop.FileManager1` (Dolphin, Nautilus, Nemo, Caja, Thunar…) ;
//! enfin le premier gestionnaire connu présent dans le PATH.

use crate::config::Config;
use crate::dbus::{Bus, Connection, DV};
use crate::paths;
use std::path::Path;
use std::time::Duration;

const KNOWN: &[&str] = &["dolphin", "nautilus", "nemo", "caja", "thunar", "pcmanfm-qt", "pcmanfm", "spacefm", "konqueror"];

/// `file:///chemin` avec encodage des caractères réservés.
pub fn file_uri(p: &Path) -> String {
    let mut out = String::from("file://");
    for b in p.to_string_lossy().bytes() {
        if b.is_ascii_alphanumeric() || b"/-_.~".contains(&b) {
            out.push(b as char);
        } else {
            out.push_str(&format!("%{b:02X}"));
        }
    }
    out
}

fn spawn(argv: &[String]) -> Result<(), String> {
    crate::generic::spawn_detached(argv, &paths::log_dir().join("file-manager.log")).map(|_| ()).map_err(|e| e.to_string())
}

/// Ouvre `dir` ; renvoie le moyen utilisé.
pub fn open(cfg: &Config, dir: &Path) -> Result<String, String> {
    let d = dir.to_string_lossy().into_owned();
    let custom = cfg.raw.path("general.file_manager").strings();
    if !custom.is_empty() {
        let mut argv = custom.clone();
        argv.push(d);
        spawn(&argv)?;
        return Ok(custom[0].clone());
    }
    if paths::which("xdg-open").is_some() {
        spawn(&["xdg-open".into(), d.clone()])?;
        return Ok("xdg-open".into());
    }
    if let Ok(mut c) = Connection::open(Bus::Session) {
        let r = c.call(
            "org.freedesktop.FileManager1",
            "/org/freedesktop/FileManager1",
            "org.freedesktop.FileManager1",
            "ShowFolders",
            vec![DV::Array("s".into(), vec![DV::str(&file_uri(dir))]), DV::str("")],
            Duration::from_secs(10),
        );
        if r.is_ok() {
            return Ok("org.freedesktop.FileManager1".into());
        }
    }
    for fm in KNOWN {
        if paths::which(fm).is_some() {
            spawn(&[fm.to_string(), d.clone()])?;
            return Ok(fm.to_string());
        }
    }
    Err("aucun gestionnaire de fichiers trouvé (réglez [general] file_manager)".into())
}

#[cfg(test)]
mod tests {
    #[test]
    fn uri() {
        assert_eq!(super::file_uri(std::path::Path::new("/run/media/u/MA CLÉ")), "file:///run/media/u/MA%20CL%C3%89");
    }
}
