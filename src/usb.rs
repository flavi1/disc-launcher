//! Clés USB et autres supports amovibles de données (cartes mémoire…).
//!
//! Détection sans udev ni udisks : disques `sd*` de `/sys/block` amovibles ou
//! reliés au bus USB, avec leurs partitions. Le type et l'étiquette du système
//! de fichiers viennent de la base d'udev (`/run/udev/data`), sinon d'udisks2.
//! La Retrode, qui se présente aussi comme une clé USB, est exclue : elle est
//! gérée comme lecteur de cartouches (voir `cart`).

use std::path::{Path, PathBuf};

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Volume {
    /// `/dev/sdb1` (ou `/dev/sdb` sans table de partitions).
    pub dev: String,
    pub label: String,
    pub fstype: String,
    pub size: u64,
    /// Fabricant et modèle (sysfs).
    pub model: String,
    pub mount: Option<PathBuf>,
}

impl Volume {
    /// Nom affiché : étiquette, sinon modèle, sinon périphérique.
    pub fn name(&self) -> String {
        if !self.label.is_empty() {
            self.label.clone()
        } else if !self.model.is_empty() {
            self.model.clone()
        } else {
            self.dev.trim_start_matches("/dev/").to_string()
        }
    }
}

fn read(p: &Path) -> String {
    std::fs::read_to_string(p).map(|s| s.trim().to_string()).unwrap_or_default()
}

/// Décodage des « \x20 » d'udev (ID_FS_LABEL_ENC).
fn udev_unescape(s: &str) -> String {
    let b = s.as_bytes();
    let mut out = vec![];
    let mut i = 0;
    while i < b.len() {
        if b[i] == b'\\' && i + 3 < b.len() && b[i + 1] == b'x' {
            if let Ok(v) = u8::from_str_radix(&s[i + 2..i + 4], 16) {
                out.push(v);
                i += 4;
                continue;
            }
        }
        out.push(b[i]);
        i += 1;
    }
    String::from_utf8_lossy(&out).into_owned()
}

/// (type, étiquette) d'après la base d'udev ; None si udev ne connaît pas le périphérique.
fn udev_fs(udev: &Path, sys: &Path) -> Option<(String, String)> {
    let devnum = read(&sys.join("dev"));
    let text = std::fs::read_to_string(udev.join(format!("b{devnum}"))).ok()?;
    let get = |k: &str| text.lines().find_map(|l| l.strip_prefix("E:").and_then(|x| x.strip_prefix(k)).and_then(|x| x.strip_prefix('='))).map(|s| s.to_string());
    let label = get("ID_FS_LABEL_ENC").map(|s| udev_unescape(&s)).or_else(|| get("ID_FS_LABEL")).unwrap_or_default();
    Some((get("ID_FS_TYPE").unwrap_or_default(), label))
}

/// Volumes amovibles présents (hors Retrode), triés.
pub fn list() -> Vec<Volume> {
    let root = std::env::var("DISC_LAUNCHER_SYSFS_BLOCK").unwrap_or_else(|_| "/sys/block".into());
    let mut v = list_in(Path::new(&root), Path::new("/run/udev/data"), true);
    // Volumes simulés (tests) : DISC_LAUNCHER_USB_DIRS="ÉTIQUETTE=/dossier:…"
    if let Ok(extra) = std::env::var("DISC_LAUNCHER_USB_DIRS") {
        for item in extra.split(':').filter(|s| !s.is_empty()) {
            let (label, dir) = item.split_once('=').unwrap_or(("USB", item));
            if Path::new(dir).is_dir() {
                v.push(Volume { dev: format!("usb:{dir}"), label: label.into(), fstype: "vfat".into(), size: 0, model: String::new(), mount: Some(PathBuf::from(dir)) });
            }
        }
    }
    v
}

pub fn list_in(sys_block: &Path, udev: &Path, use_udisks: bool) -> Vec<Volume> {
    let mut out = vec![];
    let Ok(it) = std::fs::read_dir(sys_block) else { return out };
    let mut disks: Vec<String> = it.flatten().map(|e| e.file_name().to_string_lossy().into_owned()).filter(|n| n.starts_with("sd")).collect();
    disks.sort();
    for disk in disks {
        let d = sys_block.join(&disk);
        let real = std::fs::canonicalize(&d).unwrap_or_else(|_| d.clone());
        let usb = real.to_string_lossy().contains("/usb");
        if read(&d.join("removable")) != "1" && !usb {
            continue;
        }
        // Lecteur de cartes vide : taille nulle.
        if read(&d.join("size")).parse::<u64>().unwrap_or(0) == 0 {
            continue;
        }
        let model = format!("{} {}", read(&d.join("device/vendor")), read(&d.join("device/model"))).trim().to_string();
        if model.to_ascii_lowercase().contains("retrode") {
            continue;
        }
        let mut parts: Vec<String> = std::fs::read_dir(&d)
            .map(|it| it.flatten().map(|e| e.file_name().to_string_lossy().into_owned()).filter(|n| n.starts_with(&disk) && d.join(n).join("partition").exists()).collect())
            .unwrap_or_default();
        parts.sort();
        let nodes: Vec<(String, PathBuf)> = if parts.is_empty() { vec![(disk.clone(), d.clone())] } else { parts.iter().map(|p| (p.clone(), d.join(p))).collect() };
        for (name, sys) in nodes {
            let dev = format!("/dev/{name}");
            let (mut fstype, mut label) = match udev_fs(udev, &sys) {
                Some(x) => x,
                None if use_udisks => (crate::dbus::udisks_block_prop(&dev, "IdType").unwrap_or_default(), crate::dbus::udisks_block_prop(&dev, "IdLabel").unwrap_or_default()),
                None => (String::new(), String::new()),
            };
            let mount = crate::device::find_mount_point(&dev);
            if fstype.is_empty() && mount.is_none() {
                // Partition étendue, espace non formaté…
                continue;
            }
            if fstype.is_empty() {
                fstype = "?".into();
            }
            if label.eq_ignore_ascii_case("RETRODE") || mount.as_deref().is_some_and(crate::cart::is_retrode_root) {
                continue;
            }
            label = label.trim().to_string();
            let size = read(&sys.join("size")).parse::<u64>().unwrap_or(0) * 512;
            out.push(Volume { dev, label, fstype, size, model: model.clone(), mount });
        }
    }
    out
}

/// Taille lisible : « 15,6 Go ».
pub fn human_size(bytes: u64) -> String {
    let units = ["o", "Ko", "Mo", "Go", "To"];
    let mut v = bytes as f64;
    let mut i = 0;
    while v >= 1000.0 && i + 1 < units.len() {
        v /= 1000.0;
        i += 1;
    }
    if i == 0 {
        format!("{bytes} o")
    } else {
        format!("{v:.1} {}", units[i]).replace('.', ",")
    }
}

/// Monte le volume si besoin (udisks2) ; renvoie le point de montage.
pub fn ensure_mounted(v: &Volume) -> Option<PathBuf> {
    if let Some(m) = &v.mount {
        return Some(m.clone());
    }
    crate::data::ensure_mounted(&v.dev)
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn detect_usb_partitions() {
        let base = std::env::temp_dir().join(format!("dl-usb-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&base);
        let sb = base.join("sys/block");
        let udev = base.join("udev");
        std::fs::create_dir_all(&udev).unwrap();
        let mk = |p: &Path, files: &[(&str, &str)]| {
            std::fs::create_dir_all(p).unwrap();
            for (k, v) in files {
                let f = p.join(k);
                std::fs::create_dir_all(f.parent().unwrap()).unwrap();
                std::fs::write(f, v).unwrap();
            }
        };
        // Clé amovible avec une partition FAT étiquetée.
        mk(&sb.join("sdb"), &[("removable", "1\n"), ("size", "30310400\n"), ("device/vendor", "SanDisk \n"), ("device/model", "Ultra\n")]);
        mk(&sb.join("sdb/sdb1"), &[("partition", "1"), ("size", "30308352"), ("dev", "8:17")]);
        std::fs::write(udev.join("b8:17"), "E:ID_FS_TYPE=vfat\nE:ID_FS_LABEL=MA_CLE\nE:ID_FS_LABEL_ENC=MA\\x20CLE\n").unwrap();
        // Disque interne : ignoré.
        mk(&sb.join("sda"), &[("removable", "0"), ("size", "1000")]);
        // Retrode : ignorée.
        mk(&sb.join("sdc"), &[("removable", "1"), ("size", "100"), ("device/vendor", "Retrode"), ("device/model", "2")]);
        // Lecteur de cartes vide : ignoré.
        mk(&sb.join("sdd"), &[("removable", "1"), ("size", "0")]);
        let v = list_in(&sb, &udev, false);
        assert_eq!(v.len(), 1, "{v:?}");
        assert_eq!(v[0].dev, "/dev/sdb1");
        assert_eq!(v[0].label, "MA CLE");
        assert_eq!(v[0].fstype, "vfat");
        assert_eq!(v[0].model, "SanDisk Ultra");
        assert_eq!(human_size(v[0].size), "15,5 Go");
        let _ = std::fs::remove_dir_all(&base);
    }
}
