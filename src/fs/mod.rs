//! Lecture des systèmes de fichiers sans montage : ISO 9660, UDF (dont la
//! partition de métadonnées des Blu-ray) et XDVDFS. Le point de montage
//! (`DirFs`) ne sert plus qu'en dernier recours.

pub mod iso9660;
pub mod udf;
pub mod xdvdfs;

use std::io;
use std::path::{Path, PathBuf};

#[derive(Clone, Debug, PartialEq)]
pub struct Entry {
    pub name: String,
    pub dir: bool,
    pub size: u64,
}

pub trait FileSystem {
    /// Nom du système de fichiers (`iso9660`, `udf`, `xdvdfs`, `mounted`).
    fn kind(&self) -> &'static str;
    /// Liste un dossier (chemin avec `/`, insensible à la casse).
    fn list(&self, path: &str) -> io::Result<Vec<Entry>>;
    /// Lit au plus `max` octets d'un fichier.
    fn read(&self, path: &str, max: usize) -> io::Result<Vec<u8>>;
    fn stat(&self, path: &str) -> Option<Entry> {
        let (dir, name) = match path.trim_matches('/').rsplit_once('/') {
            Some((d, n)) => (d.to_string(), n.to_string()),
            None => (String::new(), path.trim_matches('/').to_string()),
        };
        self.list(&dir).ok()?.into_iter().find(|e| e.name.eq_ignore_ascii_case(&name))
    }
    fn exists(&self, path: &str) -> bool {
        self.stat(path).is_some()
    }
    fn is_dir(&self, path: &str) -> bool {
        self.stat(path).map(|e| e.dir).unwrap_or(false)
    }
    fn volume_id(&self) -> String {
        String::new()
    }
    fn system_id(&self) -> String {
        String::new()
    }
    /// Taille du volume en secteurs de 2048 octets, si connue.
    fn volume_sectors(&self) -> Option<u64> {
        None
    }
}

/// Système de fichiers monté (dossier local), insensible à la casse.
pub struct DirFs {
    pub root: PathBuf,
}

impl DirFs {
    fn resolve(&self, path: &str) -> Option<PathBuf> {
        let mut cur = self.root.clone();
        for comp in path.split('/').filter(|c| !c.is_empty()) {
            let direct = cur.join(comp);
            if direct.exists() {
                cur = direct;
                continue;
            }
            let found = std::fs::read_dir(&cur).ok()?.filter_map(|e| e.ok()).find(|e| e.file_name().to_string_lossy().eq_ignore_ascii_case(comp))?;
            cur = found.path();
        }
        Some(cur)
    }
}

impl FileSystem for DirFs {
    fn kind(&self) -> &'static str {
        "mounted"
    }
    fn list(&self, path: &str) -> io::Result<Vec<Entry>> {
        let p = self.resolve(path).ok_or_else(|| io::Error::from(io::ErrorKind::NotFound))?;
        let mut v = vec![];
        for e in std::fs::read_dir(p)? {
            let e = e?;
            let m = e.metadata()?;
            v.push(Entry { name: e.file_name().to_string_lossy().into_owned(), dir: m.is_dir(), size: m.len() });
        }
        Ok(v)
    }
    fn read(&self, path: &str, max: usize) -> io::Result<Vec<u8>> {
        use std::io::Read;
        let p = self.resolve(path).ok_or_else(|| io::Error::from(io::ErrorKind::NotFound))?;
        let mut buf = vec![];
        std::fs::File::open(p)?.take(max as u64).read_to_end(&mut buf)?;
        Ok(buf)
    }
    fn volume_id(&self) -> String {
        self.root.file_name().map(|s| s.to_string_lossy().into_owned()).unwrap_or_default()
    }
}

/// Le volume porte-t-il une séquence de reconnaissance UDF (NSR02/NSR03) ?
pub fn has_udf(src: &dyn crate::device::DiscSource, base: u32) -> bool {
    for s in 16..24u32 {
        if let Ok(b) = src.read_data(base + s, 1) {
            let id = &b[1..6];
            if id == b"NSR02" || id == b"NSR03" {
                return true;
            }
            if id == b"TEA01" {
                break;
            }
        } else {
            break;
        }
    }
    false
}

pub fn open_mounted(p: &Path) -> Option<DirFs> {
    if p.is_dir() {
        Some(DirFs { root: p.to_path_buf() })
    } else {
        None
    }
}
