//! Environnement de la session graphique pour un démon lancé hors session.
//!
//! Lancé par XDG Autostart, le démon hérite de l'environnement de la session
//! (DISPLAY, WAYLAND_DISPLAY…). Lancé par un gestionnaire de services (OpenRC,
//! runit, s6, parfois systemd), il ne l'a pas. Dans ce cas, la session exécute
//! `disc-launcher session-env` à l'ouverture : ces variables sont écrites dans
//! `$XDG_RUNTIME_DIR/disc-launcher/session.env`, puis le démon les relit
//! (au démarrage et à chaque rechargement).

use crate::paths;
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, Ordering};

/// Variables transmises ; aucune autre n'est lue depuis le fichier.
pub const VARS: &[&str] = &[
    "DISPLAY",
    "WAYLAND_DISPLAY",
    "XAUTHORITY",
    "DBUS_SESSION_BUS_ADDRESS",
    "XDG_CURRENT_DESKTOP",
    "XDG_SESSION_TYPE",
    "XDG_SESSION_ID",
    "XDG_SESSION_DESKTOP",
    "DESKTOP_SESSION",
    "KDE_FULL_SESSION",
    "KDE_SESSION_VERSION",
];

static ADOPTED: AtomicBool = AtomicBool::new(false);

pub fn env_file() -> PathBuf {
    paths::runtime_dir().join("session.env")
}

fn has_display() -> bool {
    ["DISPLAY", "WAYLAND_DISPLAY"].iter().any(|k| std::env::var(k).map_or(false, |v| !v.is_empty()))
}

/// Sans XDG_RUNTIME_DIR (fréquent sous runit ou s6), adopte /run/user/<uid>
/// s'il existe et appartient à l'utilisateur : le démon et la session
/// partagent alors le même dossier d'exécution (socket de contrôle, session.env).
pub fn adopt_runtime_dir() {
    if std::env::var_os("XDG_RUNTIME_DIR").map_or(false, |v| !v.is_empty()) {
        return;
    }
    use std::os::unix::fs::MetadataExt;
    let d = PathBuf::from(format!("/run/user/{}", crate::sys::uid()));
    if let Ok(m) = std::fs::metadata(&d) {
        if m.is_dir() && m.uid() == crate::sys::uid() {
            std::env::set_var("XDG_RUNTIME_DIR", &d);
        }
    }
}

/// À appeler au démarrage du démon, avant tout autre fil d'exécution : si
/// l'environnement n'a pas d'affichage, le démon suivra désormais session.env.
pub fn init_daemon() -> Vec<String> {
    adopt_runtime_dir();
    if !has_display() {
        ADOPTED.store(true, Ordering::SeqCst);
    }
    apply()
}

/// Applique session.env si le démon a été lancé hors session. Retourne les
/// variables modifiées.
pub fn apply() -> Vec<String> {
    if !ADOPTED.load(Ordering::SeqCst) {
        return vec![];
    }
    let Ok(text) = std::fs::read_to_string(env_file()) else { return vec![] };
    let mut changed = vec![];
    for line in text.lines() {
        let Some((k, v)) = line.split_once('=') else { continue };
        if !VARS.contains(&k) || v.is_empty() || v.contains('\0') {
            continue;
        }
        if std::env::var(k).ok().as_deref() != Some(v) {
            std::env::set_var(k, v);
            changed.push(k.to_string());
        }
    }
    changed
}

/// Écrit les variables de la session courante dans session.env.
pub fn save() -> std::io::Result<(PathBuf, usize)> {
    let mut out = String::from("# Écrit par « disc-launcher session-env » à l'ouverture de la session.\n");
    let mut n = 0;
    for k in VARS {
        if let Ok(v) = std::env::var(k) {
            if !v.is_empty() && !v.contains('\n') {
                out.push_str(&format!("{k}={v}\n"));
                n += 1;
            }
        }
    }
    let path = env_file();
    paths::write_atomic(&path, out.as_bytes())?;
    use std::os::unix::fs::PermissionsExt;
    let _ = std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o600));
    Ok((path, n))
}
