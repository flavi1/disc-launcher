//! Emplacements XDG et répertoires du projet.

use std::env;
use std::path::{Path, PathBuf};

pub const APP: &str = "disc-launcher";

pub fn home() -> PathBuf {
    env::var_os("HOME").map(PathBuf::from).unwrap_or_else(|| PathBuf::from("/"))
}

fn xdg(var: &str, default_rel: &str) -> PathBuf {
    match env::var_os(var) {
        Some(v) if !v.is_empty() && Path::new(&v).is_absolute() => PathBuf::from(v),
        _ => home().join(default_rel),
    }
}

pub fn config_home() -> PathBuf {
    xdg("XDG_CONFIG_HOME", ".config")
}
pub fn data_home() -> PathBuf {
    xdg("XDG_DATA_HOME", ".local/share")
}
pub fn cache_home() -> PathBuf {
    xdg("XDG_CACHE_HOME", ".cache")
}
pub fn state_home() -> PathBuf {
    xdg("XDG_STATE_HOME", ".local/state")
}

/// `$XDG_RUNTIME_DIR/disc-launcher`, sinon `/tmp/disc-launcher-<uid>` (0700).
pub fn runtime_dir() -> PathBuf {
    let d = match env::var_os("XDG_RUNTIME_DIR") {
        Some(v) if !v.is_empty() => PathBuf::from(v).join(APP),
        _ => PathBuf::from(format!("/tmp/{APP}-{}", crate::sys::uid())),
    };
    ensure_private_dir(&d);
    d
}

pub fn ensure_private_dir(d: &Path) {
    use std::os::unix::fs::{DirBuilderExt, PermissionsExt};
    if !d.exists() {
        let _ = std::fs::DirBuilder::new().recursive(true).mode(0o700).create(d);
    } else if d.starts_with("/tmp") {
        let _ = std::fs::set_permissions(d, std::fs::Permissions::from_mode(0o700));
    }
}

pub fn user_config_dir() -> PathBuf {
    config_home().join(APP)
}
pub fn user_data_dir() -> PathBuf {
    data_home().join(APP)
}
pub fn user_cache_dir() -> PathBuf {
    cache_home().join(APP)
}
pub fn user_state_dir() -> PathBuf {
    state_home().join(APP)
}
pub fn jobs_dir() -> PathBuf {
    user_state_dir().join("jobs")
}
pub fn log_dir() -> PathBuf {
    user_state_dir().join("log")
}
pub fn control_socket() -> PathBuf {
    runtime_dir().join("control.sock")
}

/// Répertoire de configuration système (`/etc/disc-launcher`).
pub fn sysconf_dir() -> PathBuf {
    env::var_os("DISC_LAUNCHER_SYSCONF_DIR").map(PathBuf::from).unwrap_or_else(|| PathBuf::from("/etc").join(APP))
}

/// Répertoires de données système, du plus prioritaire au moins prioritaire.
/// `DISC_LAUNCHER_DATA_DIR` permet d'utiliser l'arborescence source (`./data`).
pub fn system_data_dirs() -> Vec<PathBuf> {
    let mut v = vec![];
    if let Some(d) = env::var_os("DISC_LAUNCHER_DATA_DIR") {
        v.push(PathBuf::from(d));
    }
    let dirs = env::var("XDG_DATA_DIRS").ok().filter(|s| !s.is_empty()).unwrap_or_else(|| "/usr/local/share:/usr/share".into());
    for d in dirs.split(':').filter(|s| !s.is_empty()) {
        v.push(PathBuf::from(d).join(APP));
    }
    v
}

/// Toutes les occurrences d'un sous-répertoire de données, du moins
/// prioritaire au plus prioritaire (l'ordre de fusion) :
/// `/usr/share`, `/usr/local/share`, `~/.local/share/disc-launcher`,
/// `~/.config/disc-launcher`.
pub fn data_layers(sub: &str) -> Vec<PathBuf> {
    let mut v: Vec<PathBuf> = system_data_dirs().into_iter().rev().map(|d| d.join(sub)).collect();
    v.push(user_data_dir().join(sub));
    v.push(user_config_dir().join(sub));
    let mut out: Vec<PathBuf> = vec![];
    for d in v {
        // Un même dossier peut figurer deux fois (XDG_DATA_DIRS contenant ~/.local/share).
        if d.is_dir() && !out.iter().any(|o| o == &d) {
            out.push(d);
        }
    }
    out
}

/// Remplace `~/` par le dossier personnel et développe `$HOME`.
pub fn expand(p: &str) -> PathBuf {
    if p == "~" {
        return home();
    }
    if let Some(rest) = p.strip_prefix("~/") {
        return home().join(rest);
    }
    if let Some(rest) = p.strip_prefix("$HOME/") {
        return home().join(rest);
    }
    PathBuf::from(p)
}

/// Cherche un exécutable dans le PATH (ou chemin absolu).
pub fn which(cmd: &str) -> Option<PathBuf> {
    use std::os::unix::fs::PermissionsExt;
    let is_exec = |p: &Path| p.is_file() && p.metadata().map(|m| m.permissions().mode() & 0o111 != 0).unwrap_or(false);
    if cmd.contains('/') {
        let p = expand(cmd);
        return if is_exec(&p) { Some(p) } else { None };
    }
    let path = env::var("PATH").unwrap_or_else(|_| "/usr/local/bin:/usr/bin:/bin".into());
    for d in path.split(':') {
        let p = Path::new(d).join(cmd);
        if is_exec(&p) {
            return Some(p);
        }
    }
    // Dossiers personnels courants, même absents du PATH du démon (lancé par la
    // session, il n'hérite pas toujours de ~/.local/bin) : AppImages, scripts.
    for d in [".local/bin", "bin", "Applications", ".local/share/applications/bin"] {
        let p = home().join(d).join(cmd);
        if is_exec(&p) {
            return Some(p);
        }
    }
    // Emplacement des binaires du projet (installation ou cible cargo)
    if let Ok(me) = env::current_exe() {
        if let Some(dir) = me.parent() {
            let p = dir.join(cmd);
            if is_exec(&p) {
                return Some(p);
            }
        }
    }
    None
}

/// Écriture atomique : fichier temporaire dans le même dossier puis renommage.
pub fn write_atomic(path: &Path, data: &[u8]) -> std::io::Result<()> {
    use std::io::Write;
    if let Some(d) = path.parent() {
        std::fs::create_dir_all(d)?;
    }
    let tmp = path.with_file_name(format!(".{}.tmp{}", path.file_name().and_then(|s| s.to_str()).unwrap_or("f"), crate::sys::pid()));
    {
        let mut f = std::fs::File::create(&tmp)?;
        f.write_all(data)?;
        f.sync_all()?;
    }
    std::fs::rename(&tmp, path)
}
