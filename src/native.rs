//! Coquilles natives (mode « hybride ») : entrée « Lire le disque
//! (disc-launcher) » dans le notificateur de Plasma (action Solid) et dans les
//! choix d'application de GNOME et apparentés (fichier .desktop GIO).
//!
//! Installées avec disc-launcher mais **inactives** : les modèles sont dans
//! `…/share/disc-launcher/native/`. `disc-launcher native enable` les copie
//! dans le profil de l'utilisateur, `disable` les retire. Elles ne sont utiles
//! qu'en mode hybride : en mode `daemon` (défaut), la fenêtre de
//! disc-launcher propose déjà la lecture, et le disque serait proposé deux fois.

use crate::paths;
use std::path::PathBuf;

const SOLID: &str = "disc-launcher-play-solid.desktop";
const GIO: &str = "disc-launcher-play-gio.desktop";

/// (modèle, destination dans le profil de l'utilisateur)
fn targets() -> Vec<(&'static str, PathBuf)> {
    let data = paths::data_home();
    vec![(SOLID, data.join("solid/actions/disc-launcher-play.desktop")), (GIO, data.join("applications/disc-launcher-play.desktop"))]
}

fn template(name: &str) -> Option<PathBuf> {
    let mut dirs: Vec<PathBuf> = paths::data_layers("native").into_iter().rev().collect();
    // Arborescence source (DISC_LAUNCHER_DATA_DIR=./data) : data/desktop/.
    dirs.extend(paths::system_data_dirs().into_iter().map(|d| d.join("desktop")));
    dirs.into_iter().map(|d| d.join(name)).find(|p| p.is_file())
}

/// Actives pour cet utilisateur ? (les deux fichiers présents)
pub fn enabled() -> bool {
    targets().iter().all(|(_, dst)| dst.is_file())
}

/// Installées pour tout le système (`make install-native`) ?
pub fn system_wide() -> bool {
    paths::system_data_dirs().iter().filter_map(|d| d.parent()).any(|share| share.join("solid/actions/disc-launcher-play.desktop").is_file() || share.join("applications/disc-launcher-play.desktop").is_file())
}

pub fn enable() -> Result<Vec<PathBuf>, String> {
    let mut done = vec![];
    for (name, dst) in targets() {
        let src = template(name).ok_or_else(|| format!("modèle {name} introuvable (…/share/disc-launcher/native/) : réinstallez disc-launcher"))?;
        if let Some(d) = dst.parent() {
            std::fs::create_dir_all(d).map_err(|e| format!("{} : {e}", d.display()))?;
        }
        std::fs::copy(&src, &dst).map_err(|e| format!("{} : {e}", dst.display()))?;
        done.push(dst);
    }
    refresh_caches();
    Ok(done)
}

pub fn disable() -> Vec<PathBuf> {
    let mut removed = vec![];
    for (_, dst) in targets() {
        if std::fs::remove_file(&dst).is_ok() {
            removed.push(dst);
        }
    }
    refresh_caches();
    removed
}

fn refresh_caches() {
    let apps = paths::data_home().join("applications");
    if paths::which("update-desktop-database").is_some() {
        let _ = std::process::Command::new("update-desktop-database").arg(&apps).stdout(std::process::Stdio::null()).stderr(std::process::Stdio::null()).status();
    }
}
