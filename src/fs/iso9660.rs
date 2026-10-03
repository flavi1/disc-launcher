//! Lecteur ISO 9660 en lecture seule, à lectures bornées.

use super::{Entry, FileSystem};
use crate::device::DiscSource;
use std::io;

const MAX_DIR_BYTES: u32 = 256 * 1024;

pub struct Iso9660<'a> {
    src: &'a dyn DiscSource,
    /// LBA de début de la piste (le secteur 16 du volume est à `base + 16`).
    base: u32,
    root_extent: u32,
    root_size: u32,
    pub system_id: String,
    pub volume_id: String,
    pub volume_space: u32,
}

fn le32(b: &[u8]) -> u32 {
    u32::from_le_bytes([b[0], b[1], b[2], b[3]])
}

fn clean(b: &[u8]) -> String {
    String::from_utf8_lossy(b).trim_matches(|c: char| c == ' ' || c == '\0').to_string()
}

impl<'a> Iso9660<'a> {
    pub fn open(src: &'a dyn DiscSource, base: u32) -> io::Result<Iso9660<'a>> {
        for s in 16..32u32 {
            let b = src.read_data(base + s, 1)?;
            if &b[1..6] != b"CD001" {
                return Err(io::Error::new(io::ErrorKind::InvalidData, "pas de descripteur ISO 9660"));
            }
            match b[0] {
                1 => {
                    let root = &b[156..190];
                    return Ok(Iso9660 {
                        src,
                        base,
                        root_extent: le32(&root[2..6]),
                        root_size: le32(&root[10..14]),
                        system_id: clean(&b[8..40]),
                        volume_id: clean(&b[40..72]),
                        volume_space: le32(&b[80..84]),
                    });
                }
                255 => break,
                _ => {}
            }
        }
        Err(io::Error::new(io::ErrorKind::InvalidData, "descripteur primaire absent"))
    }

    fn read_dir(&self, extent: u32, size: u32) -> io::Result<Vec<(Entry, u32)>> {
        let size = size.min(MAX_DIR_BYTES);
        let sectors = size.div_ceil(2048).max(1);
        let data = self.src.read_data(self.base + extent, sectors)?;
        let mut out = vec![];
        let mut sector_start = 0usize;
        while sector_start < data.len() {
            let mut p = sector_start;
            let end = sector_start + 2048;
            while p < end {
                let len = data[p] as usize;
                if len == 0 || p + len > end || len < 34 {
                    break;
                }
                let rec = &data[p..p + len];
                let nlen = rec[32] as usize;
                if 33 + nlen <= len {
                    let raw = &rec[33..33 + nlen];
                    if !(nlen == 1 && (raw[0] == 0 || raw[0] == 1)) {
                        let mut name = String::from_utf8_lossy(raw).into_owned();
                        if let Some(i) = name.find(';') {
                            name.truncate(i);
                        }
                        let name = name.trim_end_matches('.').to_string();
                        let dir = rec[25] & 0x02 != 0;
                        out.push((Entry { name, dir, size: le32(&rec[10..14]) as u64 }, le32(&rec[2..6])));
                    }
                }
                p += len;
            }
            sector_start = end;
        }
        Ok(out)
    }

    fn lookup(&self, path: &str) -> io::Result<(u32, u32, bool)> {
        let mut extent = self.root_extent;
        let mut size = self.root_size;
        let mut is_dir = true;
        for comp in path.split('/').filter(|c| !c.is_empty()) {
            if !is_dir {
                return Err(io::ErrorKind::NotFound.into());
            }
            let entries = self.read_dir(extent, size)?;
            let (e, ext) = entries.into_iter().find(|(e, _)| e.name.eq_ignore_ascii_case(comp)).ok_or_else(|| io::Error::from(io::ErrorKind::NotFound))?;
            extent = ext;
            size = e.size as u32;
            is_dir = e.dir;
        }
        Ok((extent, size, is_dir))
    }
}

impl FileSystem for Iso9660<'_> {
    fn kind(&self) -> &'static str {
        "iso9660"
    }
    fn list(&self, path: &str) -> io::Result<Vec<Entry>> {
        let (ext, size, dir) = self.lookup(path)?;
        if !dir {
            return Err(io::Error::new(io::ErrorKind::InvalidInput, "pas un dossier"));
        }
        Ok(self.read_dir(ext, size)?.into_iter().map(|(e, _)| e).collect())
    }
    fn read(&self, path: &str, max: usize) -> io::Result<Vec<u8>> {
        let (ext, size, dir) = self.lookup(path)?;
        if dir {
            return Err(io::Error::new(io::ErrorKind::InvalidInput, "dossier"));
        }
        let n = (size as usize).min(max);
        let sectors = (n as u32).div_ceil(2048).max(1);
        let mut d = self.src.read_data(self.base + ext, sectors)?;
        d.truncate(n);
        Ok(d)
    }
    fn volume_id(&self) -> String {
        self.volume_id.clone()
    }
    fn system_id(&self) -> String {
        self.system_id.clone()
    }
    fn volume_sectors(&self) -> Option<u64> {
        Some(self.volume_space as u64)
    }
}

/// Construit une petite image ISO 9660 en mémoire (tests).
#[cfg(test)]
pub mod testutil {
    use crate::device::{MediaKind, MemSource, Physical, Track};

    pub fn mem_from_iso(img: &[u8], media: MediaKind) -> MemSource {
        let sectors = (img.len() / 2048) as u32;
        let mut m = MemSource::new(Physical {
            media,
            profile: 0,
            recordable: false,
            blank: false,
            sessions: 1,
            tracks: vec![Track { number: 1, session: 1, data: true, start: 0, length: sectors, mode: Some(1) }],
            leadout: sectors,
            disc_type: None,
            capacity: sectors as u64,
        });
        m.put(0, 0, img);
        m
    }

    /// Fichiers : (chemin "DIR/NAME.EXT", contenu). Un seul niveau de dossier.
    pub fn build(system_id: &str, volume_id: &str, files: &[(&str, &[u8])]) -> Vec<u8> {
        let mut img = vec![0u8; 2048 * 64];
        let mut next = 24u32; // premier secteur libre
        let mut alloc = |img: &mut Vec<u8>, data: &[u8]| -> u32 {
            let lba = next;
            let n = (data.len() as u32).div_ceil(2048).max(1);
            next += n;
            let need = (next as usize) * 2048;
            if img.len() < need {
                img.resize(need, 0);
            }
            img[lba as usize * 2048..lba as usize * 2048 + data.len()].copy_from_slice(data);
            lba
        };
        fn rec(name: &[u8], extent: u32, size: u32, dir: bool) -> Vec<u8> {
            let len = 33 + name.len() + (name.len() + 1) % 2;
            let mut r = vec![0u8; len];
            r[0] = len as u8;
            r[2..6].copy_from_slice(&extent.to_le_bytes());
            r[6..10].copy_from_slice(&extent.to_be_bytes());
            r[10..14].copy_from_slice(&size.to_le_bytes());
            r[25] = if dir { 2 } else { 0 };
            r[32] = name.len() as u8;
            r[33..33 + name.len()].copy_from_slice(name);
            r
        }
        // dossiers
        let mut dirs: Vec<String> = vec![];
        for (p, _) in files {
            if let Some((d, _)) = p.split_once('/') {
                if !dirs.contains(&d.to_string()) {
                    dirs.push(d.to_string());
                }
            }
        }
        let mut root_entries: Vec<Vec<u8>> = vec![];
        for d in &dirs {
            let mut ents = vec![rec(&[0], 0, 2048, true), rec(&[1], 0, 2048, true)];
            for (p, data) in files.iter().filter(|(p, _)| p.starts_with(&format!("{d}/"))) {
                let name = format!("{};1", &p[d.len() + 1..]);
                let lba = alloc(&mut img, data);
                ents.push(rec(name.as_bytes(), lba, data.len() as u32, false));
            }
            let blob: Vec<u8> = ents.concat();
            let lba = alloc(&mut img, &blob);
            root_entries.push(rec(d.as_bytes(), lba, 2048, true));
        }
        for (p, data) in files.iter().filter(|(p, _)| !p.contains('/')) {
            let name = format!("{p};1");
            let lba = alloc(&mut img, data);
            root_entries.push(rec(name.as_bytes(), lba, data.len() as u32, false));
        }
        let mut root = vec![rec(&[0], 0, 2048, true), rec(&[1], 0, 2048, true)];
        root.extend(root_entries);
        let root_blob = root.concat();
        let root_lba = alloc(&mut img, &root_blob);
        let total = next;
        let pvd = &mut img[16 * 2048..17 * 2048];
        pvd[0] = 1;
        pvd[1..6].copy_from_slice(b"CD001");
        pvd[6] = 1;
        let pad = |s: &str, n: usize| {
            let mut v = s.as_bytes().to_vec();
            v.resize(n, b' ');
            v
        };
        pvd[8..40].copy_from_slice(&pad(system_id, 32));
        pvd[40..72].copy_from_slice(&pad(volume_id, 32));
        pvd[80..84].copy_from_slice(&total.to_le_bytes());
        pvd[128..130].copy_from_slice(&2048u16.to_le_bytes());
        let r = rec(&[0], root_lba, 2048, true);
        pvd[156..156 + 34].copy_from_slice(&r[..34]);
        let term = &mut img[17 * 2048..18 * 2048];
        term[0] = 255;
        term[1..6].copy_from_slice(b"CD001");
        img
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use super::testutil::mem_from_iso;
    use crate::device::MediaKind;

    #[test]
    fn read_tree() {
        let img = testutil::build("PLAYSTATION", "TEST", &[("SYSTEM.CNF", b"BOOT = cdrom:\\SCES_008.67;1\r\n"), ("VIDEO_TS/VIDEO_TS.IFO", b"DVDVIDEO")]);
        let m = mem_from_iso(&img, MediaKind::Cd);
        let fs = Iso9660::open(&m, 0).unwrap();
        assert_eq!(fs.system_id, "PLAYSTATION");
        assert!(fs.exists("system.cnf"));
        assert!(fs.is_dir("VIDEO_TS"));
        assert_eq!(fs.read("VIDEO_TS/VIDEO_TS.IFO", 100).unwrap(), b"DVDVIDEO");
        assert!(!fs.exists("BDMV"));
    }
}
