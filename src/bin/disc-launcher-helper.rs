//! Assistant privilégié.
//!
//! Lancé par `pkexec` (action polkit `io.github.flavi1.disclauncher.dump`), il
//! construit lui-même la commande d'un **profil d'invocation** (redumper,
//! friidump…) et l'exécute **sous l'identité de l'utilisateur**, avec la seule
//! capacité ambiante CAP_SYS_RAWIO (commandes SCSI constructeur : OmniDrive,
//! Kreon, Friidump). La tâche ne transmet qu'un nom de profil, le lecteur, le
//! dossier de sortie, un nom de fichier et des options prévues par le profil.
//!
//! Usage : disc-launcher-helper run --profile <nom> --device /dev/srN --out <dossier> --name <fichier> [-- <options…>]
//!
//! Profils : intégrés (voir `helper_profiles.rs`), complétés ou remplacés par
//! /etc/disc-launcher/helper-tools.toml (appartenant à root).

use disclauncher::{helper_profiles, sys};
use std::os::unix::fs::{FileTypeExt, MetadataExt, PermissionsExt};
use std::os::unix::process::CommandExt;
use std::path::{Path, PathBuf};
use std::process::Command;

const CONFIG: &str = "/etc/disc-launcher/helper-tools.toml";
const USAGE: &str = "usage : disc-launcher-helper run --profile <nom> --device /dev/srN --out <dossier> --name <fichier> [-- <options…>]";

fn die(msg: &str) -> ! {
    eprintln!("disc-launcher-helper : {msg}");
    std::process::exit(126);
}

/// Fichier ou dossier appartenant à root et non modifiable par d'autres.
fn root_owned_safe(p: &Path) -> bool {
    let mut cur = Some(p);
    while let Some(c) = cur {
        match std::fs::metadata(c) {
            Ok(m) if m.uid() == 0 && m.permissions().mode() & 0o022 == 0 => {}
            _ => return false,
        }
        cur = c.parent().filter(|x| !x.as_os_str().is_empty());
    }
    true
}

fn user_gid_home(uid: u32) -> (u32, String, Option<String>) {
    let text = std::fs::read_to_string("/etc/passwd").unwrap_or_default();
    for l in text.lines() {
        let f: Vec<&str> = l.split(':').collect();
        if f.len() >= 6 && f[2].parse::<u32>().ok() == Some(uid) {
            return (f[3].parse().unwrap_or(uid), f[5].to_string(), Some(f[0].to_string()));
        }
    }
    (uid, "/".into(), None)
}

fn main() {
    let args: Vec<String> = std::env::args().collect();
    if args.get(1).map(|s| s.as_str()) != Some("run") {
        die(USAGE);
    }
    let (mut profile, mut device, mut out, mut name) = (None, None, None, None);
    let mut options: Vec<String> = vec![];
    let mut i = 2;
    while i < args.len() {
        let val = || args.get(i + 1).cloned();
        match args[i].as_str() {
            "--profile" => profile = val(),
            "--device" => device = val(),
            "--out" => out = val(),
            "--name" => name = val(),
            "--" => {
                options = args[i + 1..].to_vec();
                break;
            }
            a => die(&format!("argument inattendu : {a}\n{USAGE}")),
        }
        i += 2;
    }
    let (Some(profile), Some(device), Some(out), Some(name)) = (profile, device, out, name) else { die(USAGE) };

    if sys::euid() != 0 {
        die("doit être lancé par pkexec (root)");
    }
    let uid: u32 = std::env::var("PKEXEC_UID").ok().and_then(|s| s.parse().ok()).unwrap_or_else(|| die("PKEXEC_UID absent : lancement hors pkexec refusé"));
    if uid == 0 {
        die("l'utilisateur appelant ne peut pas être root");
    }

    // Lecteur : /dev/srN, périphérique bloc ; /dev/sgN déduit par le noyau (sysfs).
    let dname = device.strip_prefix("/dev/sr").unwrap_or("");
    if dname.is_empty() || !dname.chars().all(|c| c.is_ascii_digit()) {
        die("périphérique refusé (attendu /dev/srN)");
    }
    match std::fs::metadata(&device) {
        Ok(m) if m.file_type().is_block_device() => {}
        _ => die("le périphérique n'est pas un périphérique bloc"),
    }
    let sgdevice = disclauncher::device::sg_of(&device).unwrap_or_else(|| device.clone());

    // Dossier de sortie : dossier réel appartenant à l'utilisateur.
    let outp = PathBuf::from(&out);
    match std::fs::symlink_metadata(&outp) {
        Ok(m) if m.is_dir() && m.uid() == uid => {}
        _ => die("dossier de sortie refusé (doit être un dossier appartenant à l'utilisateur)"),
    }
    let outp = std::fs::canonicalize(&outp).unwrap_or_else(|_| die("dossier de sortie introuvable"));

    // Profil : intégré, ou défini par root dans helper-tools.toml.
    let extra = if Path::new(CONFIG).exists() {
        if !root_owned_safe(Path::new(CONFIG)) {
            die(&format!("{CONFIG} doit appartenir à root et n'être modifiable par personne d'autre"));
        }
        Some(std::fs::read_to_string(CONFIG).unwrap_or_else(|e| die(&format!("{CONFIG} : {e}"))))
    } else {
        None
    };
    let all = helper_profiles::load(extra.as_deref()).unwrap_or_else(|e| die(&e));
    let p = all["profiles"].get(&profile);
    if p.is_null() {
        die(&format!("profil inconnu : {profile}"));
    }
    let candidates = helper_profiles::paths(p);
    let Some(found) = candidates.iter().find(|c| Path::new(c).exists()) else {
        die(&format!("programme du profil {profile} introuvable ({}) : installez-le ou corrigez {CONFIG}", candidates.join(", ")));
    };
    let path = PathBuf::from(found);
    if !path.is_absolute() || !root_owned_safe(&path) {
        die(&format!("{} doit être un chemin absolu appartenant à root, non modifiable par d'autres", path.display()));
    }
    let argv = helper_profiles::build(p, &device, &sgdevice, &outp.to_string_lossy(), &name, &options).unwrap_or_else(|e| die(&e));

    let (gid, home, user) = user_gid_home(uid);
    if let Err(e) = sys::drop_to_user_keep_rawio(uid, gid, user.as_deref()) {
        die(&format!("abandon des privilèges impossible : {e}"));
    }
    let err = Command::new(&path).args(&argv).current_dir(&outp).env_clear().env("PATH", "/usr/local/bin:/usr/bin:/bin").env("HOME", home).env("LANG", "C.UTF-8").exec();
    die(&format!("exécution de {} : {err}", path.display()));
}
