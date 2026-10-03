//! XDVDFS (disques Xbox / Xbox 360) : repérage de la partition de jeu et
//! lecture des fichiers de la racine (`default.xbe`, `default.xex`).

use super::{Entry, FileSystem};
use crate::device::DiscSource;
use std::io;

pub const MAGIC: &[u8] = b"MICROSOFT*XBOX*MEDIA";

/// Décalages (en secteurs) de la partition de jeu selon la génération du
/// disque, plus 0 pour une image déjà extraite (XISO) ou un lecteur qui
/// expose directement la partition.
pub const PARTITION_OFFSETS: &[(u32, &str)] = &[(0, "xiso"), (198_144, "xgd1"), (129_824, "xgd2"), (16_640, "xgd3")];

pub struct Xdvdfs<'a> {
    src: &'a dyn DiscSource,
    pub base: u32,
    pub generation: &'static str,
    root_sector: u32,
    root_size: u32,
}

fn le32(b: &[u8]) -> u32 {
    u32::from_le_bytes([b[0], b[1], b[2], b[3]])
}

impl<'a> Xdvdfs<'a> {
    pub fn find(src: &'a dyn DiscSource) -> Option<Xdvdfs<'a>> {
        for &(base, gen_name) in PARTITION_OFFSETS {
            if let Ok(b) = src.read_data(base + 32, 1) {
                if &b[..20] == MAGIC && &b[0x7EC..0x7EC + 20] == MAGIC {
                    return Some(Xdvdfs { src, base, generation: gen_name, root_sector: le32(&b[20..24]), root_size: le32(&b[24..28]) });
                }
            }
        }
        None
    }

    fn root_entries(&self) -> io::Result<Vec<(Entry, u32)>> {
        let size = self.root_size.min(64 * 1024);
        let sectors = size.div_ceil(2048).max(1);
        let d = self.src.read_data(self.base + self.root_sector, sectors)?;
        let mut out = vec![];
        let mut p = 0usize;
        let end = (size as usize).min(d.len());
        while p + 14 <= end {
            if d[p] == 0xFF && d[p + 1] == 0xFF {
                p = (p / 2048 + 1) * 2048;
                continue;
            }
            let sector = le32(&d[p + 4..p + 8]);
            let fsize = le32(&d[p + 8..p + 12]);
            let attr = d[p + 12];
            let nlen = d[p + 13] as usize;
            if nlen == 0 || p + 14 + nlen > end {
                break;
            }
            let name = String::from_utf8_lossy(&d[p + 14..p + 14 + nlen]).into_owned();
            out.push((Entry { name, dir: attr & 0x10 != 0, size: fsize as u64 }, sector));
            p = (p + 14 + nlen + 3) & !3;
        }
        Ok(out)
    }
}

impl FileSystem for Xdvdfs<'_> {
    fn kind(&self) -> &'static str {
        "xdvdfs"
    }
    fn list(&self, path: &str) -> io::Result<Vec<Entry>> {
        if !path.trim_matches('/').is_empty() {
            return Err(io::Error::new(io::ErrorKind::Unsupported, "seule la racine est lue"));
        }
        Ok(self.root_entries()?.into_iter().map(|(e, _)| e).collect())
    }
    fn read(&self, path: &str, max: usize) -> io::Result<Vec<u8>> {
        let name = path.trim_matches('/');
        let (e, sector) = self.root_entries()?.into_iter().find(|(e, _)| e.name.eq_ignore_ascii_case(name)).ok_or_else(|| io::Error::from(io::ErrorKind::NotFound))?;
        let n = (e.size as usize).min(max);
        let mut d = self.src.read_data(self.base + sector, (n as u32).div_ceil(2048).max(1))?;
        d.truncate(n);
        Ok(d)
    }
}

pub struct XbeInfo {
    pub title_id: u32,
    pub title: String,
    pub region_flags: u32,
}

/// Certificat d'un `default.xbe` : identifiant et nom du titre.
pub fn parse_xbe(b: &[u8]) -> Option<XbeInfo> {
    if b.len() < 0x180 || &b[..4] != b"XBEH" {
        return None;
    }
    let base = le32(&b[0x104..]);
    let cert = le32(&b[0x118..]).checked_sub(base)? as usize;
    if cert + 0xA4 > b.len() {
        return None;
    }
    let title_id = le32(&b[cert + 8..]);
    let mut units = vec![];
    for k in 0..40 {
        let o = cert + 0xC + k * 2;
        let u = u16::from_le_bytes([b[o], b[o + 1]]);
        if u == 0 {
            break;
        }
        units.push(u);
    }
    Some(XbeInfo { title_id, title: String::from_utf16_lossy(&units).trim().to_string(), region_flags: le32(&b[cert + 0xA0..]) })
}

/// `0x4D530004` → `MS-004` (préfixe éditeur + numéro).
pub fn title_id_serial(id: u32) -> String {
    let a = ((id >> 24) & 0xff) as u8;
    let b = ((id >> 16) & 0xff) as u8;
    if a.is_ascii_alphanumeric() && b.is_ascii_alphanumeric() {
        format!("{}{}-{:03}", a as char, b as char, id & 0xffff)
    } else {
        format!("{id:08X}")
    }
}
