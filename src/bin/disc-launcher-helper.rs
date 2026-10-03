//! Assistant privilégié (section 11).
//!
//! Lancé par `pkexec` (action polkit `io.github.flavi1.disclauncher.dump`), il
//! vérifie la demande puis exécute un outil de dump **sous l'identité de
//! l'utilisateur**, avec la seule capacité ambiante CAP_SYS_RAWIO (commandes
//! SCSI constructeur : OmniDrive, Kreon, Friidump).
//!
//! Usage : disc-launcher-helper run --tool <nom> --device /dev/srN --out <dossier> -- <arguments…>
//!
//! Configuration (root uniquement) : /etc/disc-launcher/helper-tools.toml

use disclauncher::{sys, toml};
use std::os::unix::fs::{FileTypeExt, MetadataExt, PermissionsExt};
use std::os::unix::process::CommandExt;
use std::path::{Path, PathBuf};
use std::process::Command;

const CONFIG: &str = "/etc/disc-launcher/helper-tools.toml";

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
        die("usage : disc-launcher-helper run --tool <nom> --device /dev/srN --out <dossier> -- <arguments…>");
    }
    let mut tool = None;
    let mut device = None;
    let mut out = None;
    let mut rest: Vec<String> = vec![];
    let mut i = 2;
    while i < args.len() {
        match args[i].as_str() {
            "--tool" => {
                tool = args.get(i + 1).cloned();
                i += 2;
            }
            "--device" => {
                device = args.get(i + 1).cloned();
                i += 2;
            }
            "--out" => {
                out = args.get(i + 1).cloned();
                i += 2;
            }
            "--" => {
                rest = args[i + 1..].to_vec();
                break;
            }
            a => die(&format!("argument inattendu : {a}")),
        }
    }
    let (Some(tool), Some(device), Some(out)) = (tool, device, out) else { die("--tool, --device et --out sont obligatoires") };

    if sys::euid() != 0 {
        die("doit être lancé par pkexec (root)");
    }
    let uid: u32 = std::env::var("PKEXEC_UID").ok().and_then(|s| s.parse().ok()).unwrap_or_else(|| die("PKEXEC_UID absent : lancement hors pkexec refusé"));
    if uid == 0 {
        die("l'utilisateur appelant ne peut pas être root");
    }

    // Périphérique : /dev/srN, périphérique bloc.
    let dname = device.strip_prefix("/dev/sr").unwrap_or("");
    if dname.is_empty() || !dname.chars().all(|c| c.is_ascii_digit()) {
        die("périphérique refusé (attendu /dev/srN)");
    }
    match std::fs::metadata(&device) {
        Ok(m) if m.file_type().is_block_device() => {}
        _ => die("le périphérique n'est pas un périphérique bloc"),
    }

    // Dossier de sortie : dossier réel appartenant à l'utilisateur.
    // Le périphérique SCSI générique du même lecteur est accepté aussi (redumper).
    let sg = disclauncher::device::sg_of(&device);
    let same_drive = |v: &str| v == device || sg.as_deref() == Some(v);

    let outp = PathBuf::from(&out);
    match std::fs::symlink_metadata(&outp) {
        Ok(m) if m.is_dir() && m.uid() == uid => {}
        _ => die("dossier de sortie refusé (doit être un dossier appartenant à l'utilisateur)"),
    }
    let outp = std::fs::canonicalize(&outp).unwrap_or_else(|_| die("dossier de sortie introuvable"));

    // Outil autorisé.
    if !root_owned_safe(Path::new(CONFIG)) {
        die("configuration absente ou non protégée : /etc/disc-launcher/helper-tools.toml");
    }
    let cfg = toml::parse(&std::fs::read_to_string(CONFIG).unwrap_or_default()).unwrap_or_else(|e| die(&e.to_string()));
    let t = cfg.get("tools").get(&tool);
    if t.is_null() {
        die(&format!("outil non autorisé : {tool}"));
    }
    // « path » : un chemin, ou une liste dont le premier existant est retenu.
    let candidates: Vec<String> = match t["path"].as_str() {
        Some(p) => vec![p.to_string()],
        None => t["path"].strings(),
    };
    if candidates.is_empty() {
        die("chemin de l'outil manquant");
    }
    let Some(found) = candidates.iter().find(|p| Path::new(p).exists()) else {
        die(&format!("{tool} introuvable aux chemins autorisés ({}) : corrigez {CONFIG}", candidates.join(", ")));
    };
    let path = PathBuf::from(found);
    if !path.is_absolute() || !root_owned_safe(&path) {
        die("l'outil doit être un chemin absolu appartenant à root, non modifiable par d'autres");
    }
    let allowed = t["allowed_args"].strings();
    let path_opts = t["path_args"].strings();
    let device_opts = t["device_args"].strings();
    // Options suivies d'une valeur séparée : { "-d" = "device", "-i" = "path" }
    let value_args = t["value_args"].clone();
    let check_path = |v: &str, a: &str| {
        let vp = PathBuf::from(v);
        let canon = std::fs::canonicalize(&vp).unwrap_or_else(|_| vp.parent().and_then(|p| std::fs::canonicalize(p).ok()).map(|p| p.join(vp.file_name().unwrap_or_default())).unwrap_or(vp.clone()));
        if !canon.starts_with(&outp) {
            die(&format!("chemin hors du dossier de sortie : {a}"));
        }
    };
    let mut k = 0;
    while k < rest.len() {
        let a = &rest[k];
        if let Some(kind) = value_args.get(a).as_str() {
            let v = rest.get(k + 1).unwrap_or_else(|| die(&format!("valeur manquante après {a}")));
            match kind {
                "device" if !same_drive(v) => die(&format!("périphérique différent de --device : {v}")),
                "path" => check_path(v, v),
                "device" => {}
                _ => die("value_args : type inconnu"),
            }
            k += 2;
            continue;
        }
        let ok_token = allowed.iter().any(|x| if x.ends_with('=') { a.starts_with(x.as_str()) } else { a == x });
        if !ok_token {
            die(&format!("argument non autorisé : {a}"));
        }
        for p in &path_opts {
            if let Some(v) = a.strip_prefix(p.as_str()) {
                check_path(v, a);
            }
        }
        for d in &device_opts {
            if let Some(v) = a.strip_prefix(d.as_str()) {
                if !same_drive(v) {
                    die(&format!("périphérique différent de --device : {a}"));
                }
            }
        }
        k += 1;
    }

    let (gid, home, user) = user_gid_home(uid);
    if let Err(e) = sys::drop_to_user_keep_rawio(uid, gid, user.as_deref()) {
        die(&format!("abandon des privilèges impossible : {e}"));
    }
    let err = Command::new(&path).args(&rest).current_dir(&outp).env_clear().env("PATH", "/usr/local/bin:/usr/bin:/bin").env("HOME", home).env("LANG", "C.UTF-8").exec();
    die(&format!("exécution de {} : {err}", path.display()));
}
