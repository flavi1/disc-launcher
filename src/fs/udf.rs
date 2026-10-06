//! UDF en lecture seule (ECMA-167 / OSTA UDF 1.02 à 2.60), sans montage :
//! de quoi lister des dossiers et lire de petits fichiers (`BDMV/index.bdmv`,
//! `PS3_GAME/PARAM.SFO`, `VIDEO_TS/VIDEO_TS.IFO`).
//!
//! Pris en charge : partitions physiques (type 1), partition de métadonnées
//! de l'UDF 2.50+ (Blu-ray), descripteurs d'allocation courts, longs et
//! intégrés, File Entry et Extended File Entry. Non pris en charge (inutile
//! pour des disques pressés) : table d'allocation virtuelle des disques
//! gravés en multisession, tables de remplacement (sparing), extents de
//! continuation.

use super::{Entry, FileSystem};
use crate::device::DiscSource;
use std::io;

const SECTOR: u32 = 2048;

fn le16(b: &[u8], o: usize) -> u16 {
    u16::from_le_bytes([b[o], b[o + 1]])
}
fn le32(b: &[u8], o: usize) -> u32 {
    u32::from_le_bytes([b[o], b[o + 1], b[o + 2], b[o + 3]])
}
fn le64(b: &[u8], o: usize) -> u64 {
    u64::from_le_bytes(b[o..o + 8].try_into().unwrap())
}

fn bad(msg: &str) -> io::Error {
    io::Error::new(io::ErrorKind::InvalidData, format!("udf : {msg}"))
}

/// Chaîne OSTA (« compressed unicode ») : octet d'identification 8/254
/// (Latin-1) ou 16/255 (UTF-16 BE), puis les caractères.
pub fn osta_string(b: &[u8]) -> String {
    match b.first() {
        Some(8) | Some(254) => b[1..].iter().map(|&c| c as char).collect(),
        Some(16) | Some(255) => {
            let u: Vec<u16> = b[1..].chunks_exact(2).map(|c| u16::from_be_bytes([c[0], c[1]])).collect();
            String::from_utf16_lossy(&u)
        }
        _ => String::new(),
    }
}

/// `dstring` : champ de taille fixe dont le dernier octet donne la longueur utile.
fn dstring(b: &[u8]) -> String {
    let n = *b.last().unwrap_or(&0) as usize;
    if n == 0 || n > b.len() - 1 {
        return String::new();
    }
    osta_string(&b[..n]).trim_end_matches('\0').trim().to_string()
}

/// Correspondance d'une partition logique (table du Logical Volume Descriptor).
#[derive(Clone, Debug)]
enum Map {
    /// Partition physique : premier secteur.
    Physical(u32),
    /// Partition de métadonnées : extents (secteur physique, nombre de blocs).
    Metadata(Vec<(u32, u32)>),
}

#[derive(Clone, Copy, Debug)]
struct Icb {
    lbn: u32,
    part: u16,
}

/// Nœud (fichier ou dossier) décodé depuis son (Extended) File Entry.
struct Node {
    dir: bool,
    size: u64,
    /// Extents (secteur physique, octets) ou données intégrées.
    extents: Vec<(u32, u32)>,
    embedded: Option<Vec<u8>>,
}

pub struct Udf<'a> {
    src: &'a dyn DiscSource,
    base: u32,
    maps: Vec<Map>,
    root: Icb,
    volume_id: String,
    sectors: Option<u64>,
}

impl<'a> Udf<'a> {
    fn read(&self, sector: u32, count: u32) -> io::Result<Vec<u8>> {
        self.src.read_data(self.base + sector, count)
    }

    fn tag(b: &[u8]) -> u16 {
        if b.len() < 16 {
            return 0;
        }
        le16(b, 0)
    }

    /// Ouvre le volume UDF dont la piste commence à `base`.
    pub fn open(src: &'a dyn DiscSource, base: u32) -> io::Result<Udf<'a>> {
        let mut u = Udf { src, base, maps: vec![], root: Icb { lbn: 0, part: 0 }, volume_id: String::new(), sectors: None };
        // Ancre (AVDP) au secteur 256, sinon au dernier secteur - 256.
        let mut avdp = u.read(256, 1)?;
        if Self::tag(&avdp) != 2 {
            let last = src.physical().tracks.iter().find(|t| t.start == base).map(|t| t.length).unwrap_or(0);
            if last > 512 {
                avdp = u.read(last - 1 - 256, 1).unwrap_or_default();
            }
            if Self::tag(&avdp) != 2 {
                return Err(bad("ancre (AVDP) introuvable"));
            }
        }
        let vds_len = le32(&avdp, 16);
        let vds_loc = le32(&avdp, 20);
        let n = (vds_len / SECTOR).clamp(1, 64);
        let vds = u.read(vds_loc, n)?;
        // Partitions physiques : numéro → premier secteur.
        let mut parts: Vec<(u16, u32, u32)> = vec![];
        let mut lvd: Option<Vec<u8>> = None;
        for i in 0..n as usize {
            let d = &vds[i * SECTOR as usize..(i + 1) * SECTOR as usize];
            match Self::tag(d) {
                1 => {
                    if u.volume_id.is_empty() {
                        u.volume_id = dstring(&d[24..56]);
                    }
                }
                5 => parts.push((le16(d, 22), le32(d, 188), le32(d, 192))),
                6 => lvd = Some(d.to_vec()),
                8 => break,
                _ => {}
            }
        }
        let lvd = lvd.ok_or_else(|| bad("descripteur de volume logique absent"))?;
        if le32(&lvd, 212) != SECTOR {
            return Err(bad("taille de bloc logique non gérée"));
        }
        let lv_name = dstring(&lvd[84..212]);
        if !lv_name.is_empty() {
            u.volume_id = lv_name;
        }
        u.sectors = parts.iter().map(|p| (p.1 as u64) + (p.2 as u64)).max();
        let part_start = |num: u16| parts.iter().find(|p| p.0 == num).map(|p| p.1);
        // Table des partitions logiques.
        let nmaps = le32(&lvd, 268) as usize;
        let mut off = 440usize;
        let mut pending_meta: Vec<(usize, u16, u32)> = vec![];
        for _ in 0..nmaps.min(8) {
            if off + 2 > lvd.len() {
                break;
            }
            let (ty, len) = (lvd[off], lvd[off + 1] as usize);
            if len == 0 || off + len > lvd.len() {
                break;
            }
            let m = &lvd[off..off + len];
            match ty {
                1 => u.maps.push(Map::Physical(part_start(le16(m, 4)).ok_or_else(|| bad("partition physique inconnue"))?)),
                2 => {
                    let ident = String::from_utf8_lossy(&m[5..28]).to_string();
                    let num = le16(m, 38);
                    if ident.starts_with("*UDF Metadata Partition") {
                        pending_meta.push((u.maps.len(), num, le32(m, 40)));
                        u.maps.push(Map::Metadata(vec![]));
                    } else {
                        // Partition « sparable » (et autres) : lue comme une partition physique.
                        u.maps.push(Map::Physical(part_start(num).ok_or_else(|| bad("partition inconnue"))?));
                    }
                }
                _ => {}
            }
            off += len;
        }
        // Fichier de métadonnées : ses extents forment la partition de métadonnées.
        for (idx, num, file_lbn) in pending_meta {
            let start = part_start(num).ok_or_else(|| bad("partition des métadonnées inconnue"))?;
            let node = u.node_at(start + file_lbn, 0)?;
            let ext: Vec<(u32, u32)> = node.extents.iter().map(|(s, bytes)| (*s, bytes.div_ceil(SECTOR))).collect();
            if ext.is_empty() {
                return Err(bad("fichier de métadonnées vide"));
            }
            u.maps[idx] = Map::Metadata(ext);
        }
        // File Set Descriptor (long_ad dans le contenu du volume logique).
        let fsd_icb = Icb { lbn: le32(&lvd, 252), part: le16(&lvd, 256) };
        let fsd = u.read(u.phys(fsd_icb)?, 1)?;
        if Self::tag(&fsd) != 256 {
            return Err(bad("File Set Descriptor introuvable"));
        }
        u.root = Icb { lbn: le32(&fsd, 404), part: le16(&fsd, 408) };
        // Vérification : la racine doit être lisible.
        u.node(u.root)?;
        Ok(u)
    }

    /// Bloc logique → secteur physique (relatif à `base`).
    fn phys(&self, icb: Icb) -> io::Result<u32> {
        match self.maps.get(icb.part as usize) {
            Some(Map::Physical(start)) => Ok(start + icb.lbn),
            Some(Map::Metadata(ext)) => {
                let mut rest = icb.lbn;
                for (s, n) in ext {
                    if rest < *n {
                        return Ok(s + rest);
                    }
                    rest -= n;
                }
                Err(bad("bloc hors de la partition de métadonnées"))
            }
            None => Err(bad("référence de partition inconnue")),
        }
    }

    fn node(&self, icb: Icb) -> io::Result<Node> {
        let s = self.phys(icb)?;
        self.node_at(s, icb.part)
    }

    /// Décode un File Entry (261) ou Extended File Entry (266) au secteur `s`.
    /// `part` : partition des descripteurs courts (même partition que le nœud).
    fn node_at(&self, s: u32, part: u16) -> io::Result<Node> {
        let b = self.read(s, 1)?;
        let (ea_off, ad_base) = match Self::tag(&b) {
            261 => (168usize, 176usize),
            266 => (208usize, 216usize),
            t => return Err(bad(&format!("entrée de fichier attendue (étiquette {t})"))),
        };
        let file_type = b[16 + 11];
        let flags = le16(&b, 16 + 18);
        let size = le64(&b, 56);
        let l_ea = le32(&b, ea_off) as usize;
        let l_ad = le32(&b, ea_off + 4) as usize;
        let start = ad_base + l_ea;
        if start + l_ad > b.len() {
            return Err(bad("descripteurs d'allocation hors du secteur"));
        }
        let ads = &b[start..start + l_ad];
        let mut node = Node { dir: file_type == 4, size, extents: vec![], embedded: None };
        match flags & 7 {
            0 => {
                // short_ad : 8 octets (longueur, position) dans la même partition.
                for c in ads.chunks_exact(8) {
                    let len = le32(c, 0);
                    if len & 0x3FFF_FFFF == 0 {
                        break;
                    }
                    if len >> 30 == 0 {
                        node.extents.push((self.phys(Icb { lbn: le32(c, 4), part })?, len & 0x3FFF_FFFF));
                    }
                }
            }
            1 => {
                // long_ad : 16 octets (longueur, bloc, partition).
                for c in ads.chunks_exact(16) {
                    let len = le32(c, 0);
                    if len & 0x3FFF_FFFF == 0 {
                        break;
                    }
                    if len >> 30 == 0 {
                        node.extents.push((self.phys(Icb { lbn: le32(c, 4), part: le16(c, 8) })?, len & 0x3FFF_FFFF));
                    }
                }
            }
            3 => node.embedded = Some(ads.to_vec()),
            _ => return Err(bad("type de descripteur d'allocation non géré")),
        }
        Ok(node)
    }

    /// Contenu d'un nœud, au plus `max` octets.
    fn data(&self, n: &Node, max: usize) -> io::Result<Vec<u8>> {
        let want = (n.size as usize).min(max);
        if let Some(e) = &n.embedded {
            return Ok(e[..want.min(e.len())].to_vec());
        }
        let mut out = Vec::with_capacity(want);
        for (s, bytes) in &n.extents {
            if out.len() >= want {
                break;
            }
            let take = (*bytes as usize).min(want - out.len());
            let d = self.read(*s, (take as u32).div_ceil(SECTOR).max(1))?;
            out.extend_from_slice(&d[..take.min(d.len())]);
        }
        Ok(out)
    }

    /// Entrées d'un dossier : (entrée, ICB).
    fn entries(&self, dir: &Node) -> io::Result<Vec<(Entry, Icb)>> {
        let d = self.data(dir, 4 << 20)?;
        let mut out = vec![];
        let mut p = 0usize;
        while p + 38 <= d.len() {
            if le16(&d, p) != 257 {
                break;
            }
            let chars = d[p + 18];
            let l_fi = d[p + 19] as usize;
            let icb = Icb { lbn: le32(&d, p + 20 + 4), part: le16(&d, p + 20 + 8) };
            let l_iu = le16(&d, p + 36) as usize;
            let name_at = p + 38 + l_iu;
            let len = (38 + l_iu + l_fi + 3) & !3;
            if name_at + l_fi > d.len() {
                break;
            }
            // Ni parent (bit 3) ni fichier effacé (bit 2).
            if chars & 0x0C == 0 && l_fi > 0 {
                let name = osta_string(&d[name_at..name_at + l_fi]);
                let is_dir = chars & 0x02 != 0;
                let size = if is_dir { 0 } else { self.node(icb).map(|n| n.size).unwrap_or(0) };
                out.push((Entry { name, dir: is_dir, size }, icb));
            }
            p += len;
        }
        Ok(out)
    }

    /// Nœud d'un chemin (`/`, insensible à la casse).
    fn walk(&self, path: &str) -> io::Result<Node> {
        let mut node = self.node(self.root)?;
        for comp in path.split('/').filter(|c| !c.is_empty()) {
            if !node.dir {
                return Err(io::Error::from(io::ErrorKind::NotFound));
            }
            let (_, icb) = self.entries(&node)?.into_iter().find(|(e, _)| e.name.eq_ignore_ascii_case(comp)).ok_or_else(|| io::Error::from(io::ErrorKind::NotFound))?;
            node = self.node(icb)?;
        }
        Ok(node)
    }
}

impl FileSystem for Udf<'_> {
    fn kind(&self) -> &'static str {
        "udf"
    }
    fn list(&self, path: &str) -> io::Result<Vec<Entry>> {
        let n = self.walk(path)?;
        if !n.dir {
            return Err(io::Error::new(io::ErrorKind::Other, "pas un dossier"));
        }
        Ok(self.entries(&n)?.into_iter().map(|(e, _)| e).collect())
    }
    fn read(&self, path: &str, max: usize) -> io::Result<Vec<u8>> {
        let n = self.walk(path)?;
        if n.dir {
            return Err(io::Error::new(io::ErrorKind::Other, "est un dossier"));
        }
        self.data(&n, max)
    }
    fn volume_id(&self) -> String {
        self.volume_id.clone()
    }
    fn volume_sectors(&self) -> Option<u64> {
        self.sectors
    }
}

/// Construction d'images UDF pour les tests (UDF 2.50 avec partition de
/// métadonnées, ou UDF 1.02 simple).
#[cfg(test)]
pub mod build {
    use crate::device::MemSource;

    fn tag(buf: &mut [u8], id: u16, loc: u32) {
        buf[0..2].copy_from_slice(&id.to_le_bytes());
        buf[2..4].copy_from_slice(&3u16.to_le_bytes());
        buf[12..16].copy_from_slice(&loc.to_le_bytes());
        let sum: u32 = buf[0..16].iter().enumerate().filter(|(i, _)| *i != 4).map(|(_, b)| *b as u32).sum();
        buf[4] = (sum & 0xFF) as u8;
    }
    fn dstr(buf: &mut [u8], s: &str) {
        let n = buf.len();
        buf[0] = 8;
        buf[1..1 + s.len()].copy_from_slice(s.as_bytes());
        buf[n - 1] = (s.len() + 1) as u8;
    }

    pub struct Img {
        pub src: MemSource,
        part_start: u32,
        meta: bool,
        meta_start: u32,
        next: u32,
    }

    impl Img {
        /// `meta` : UDF 2.50 (Blu-ray) avec partition de métadonnées.
        pub fn new(phys: crate::device::Physical, label: &str, meta: bool) -> Img {
            let mut src = MemSource::new(phys);
            // Séquence de reconnaissance (BEA01, NSR03, TEA01).
            for (i, id) in [b"BEA01", b"NSR03", b"TEA01"].iter().enumerate() {
                let mut s = vec![0u8; 2048];
                s[1..6].copy_from_slice(*id);
                s[6] = 1;
                src.put(16 + i as u32, 0, &s);
            }
            let part_start = 300u32;
            let part_len = 10_000u32;
            // AVDP → VDS au secteur 32 (4 secteurs).
            let mut avdp = vec![0u8; 2048];
            avdp[16..20].copy_from_slice(&(4 * 2048u32).to_le_bytes());
            avdp[20..24].copy_from_slice(&32u32.to_le_bytes());
            tag(&mut avdp, 2, 256);
            src.put(256, 0, &avdp);
            let mut pvd = vec![0u8; 2048];
            dstr(&mut pvd[24..56], "PVD_LABEL");
            tag(&mut pvd, 1, 32);
            src.put(32, 0, &pvd);
            let mut pd = vec![0u8; 2048];
            pd[22..24].copy_from_slice(&0u16.to_le_bytes());
            pd[188..192].copy_from_slice(&part_start.to_le_bytes());
            pd[192..196].copy_from_slice(&part_len.to_le_bytes());
            tag(&mut pd, 5, 33);
            src.put(33, 0, &pd);
            let mut lvd = vec![0u8; 2048];
            dstr(&mut lvd[84..212], label);
            lvd[212..216].copy_from_slice(&2048u32.to_le_bytes());
            // FSD : bloc 0 de la partition 1 (métadonnées) ou 0.
            lvd[248..252].copy_from_slice(&2048u32.to_le_bytes());
            lvd[252..256].copy_from_slice(&0u32.to_le_bytes());
            lvd[256..258].copy_from_slice(&(if meta { 1u16 } else { 0 }).to_le_bytes());
            let mut maps = vec![1u8, 6, 1, 0, 0, 0];
            if meta {
                let mut m = vec![0u8; 64];
                m[0] = 2;
                m[1] = 64;
                m[5..28].copy_from_slice(b"*UDF Metadata Partition");
                m[38..40].copy_from_slice(&0u16.to_le_bytes());
                m[40..44].copy_from_slice(&0u32.to_le_bytes()); // fichier de métadonnées : bloc 0 de la partition physique
                maps.extend(m);
            }
            lvd[264..268].copy_from_slice(&(maps.len() as u32).to_le_bytes());
            lvd[268..272].copy_from_slice(&(if meta { 2u32 } else { 1 }).to_le_bytes());
            lvd[440..440 + maps.len()].copy_from_slice(&maps);
            tag(&mut lvd, 6, 34);
            src.put(34, 0, &lvd);
            let mut td = vec![0u8; 2048];
            tag(&mut td, 8, 35);
            src.put(35, 0, &td);
            let mut img = Img { src, part_start, meta, meta_start: part_start + 1, next: 0 };
            if meta {
                // Fichier de métadonnées (EFE au bloc 0) : un extent de 100 blocs au bloc 1.
                let mut efe = vec![0u8; 2048];
                efe[16 + 11] = 250;
                efe[16 + 18..16 + 20].copy_from_slice(&0u16.to_le_bytes()); // short_ad
                efe[56..64].copy_from_slice(&(100 * 2048u64).to_le_bytes());
                efe[212..216].copy_from_slice(&8u32.to_le_bytes());
                efe[216..220].copy_from_slice(&(100 * 2048u32).to_le_bytes());
                efe[220..224].copy_from_slice(&1u32.to_le_bytes());
                tag(&mut efe, 266, 0);
                img.src.put(part_start, 0, &efe);
            }
            img
        }

        /// Secteur physique du bloc `lbn` de la partition des métadonnées (ou physique).
        fn sector(&self, lbn: u32) -> u32 {
            if self.meta {
                self.meta_start + lbn
            } else {
                self.part_start + lbn
            }
        }

        fn alloc(&mut self) -> u32 {
            let b = self.next;
            self.next += 1;
            b
        }

        /// Écrit un nœud (EFE) avec données intégrées ; renvoie son bloc.
        fn efe(&mut self, dir: bool, data: &[u8]) -> u32 {
            let lbn = self.alloc();
            let mut e = vec![0u8; 2048];
            e[16 + 11] = if dir { 4 } else { 5 };
            e[16 + 18..16 + 20].copy_from_slice(&3u16.to_le_bytes()); // données intégrées
            e[56..64].copy_from_slice(&(data.len() as u64).to_le_bytes());
            e[212..216].copy_from_slice(&(data.len() as u32).to_le_bytes());
            e[216..216 + data.len()].copy_from_slice(data);
            tag(&mut e, 266, lbn);
            let s = self.sector(lbn);
            self.src.put(s, 0, &e);
            lbn
        }

        fn fid(name: &str, dir: bool, lbn: u32, part: u16) -> Vec<u8> {
            let mut f = vec![0u8; 38];
            f[0..2].copy_from_slice(&257u16.to_le_bytes());
            f[18] = if dir { 2 } else { 0 };
            f[19] = (name.len() + 1) as u8;
            f[24..28].copy_from_slice(&lbn.to_le_bytes());
            f[28..30].copy_from_slice(&part.to_le_bytes());
            f.push(8);
            f.extend_from_slice(name.as_bytes());
            while f.len() % 4 != 0 {
                f.push(0);
            }
            f
        }

        /// Écrit l'arborescence `(chemin, contenu)` (dossiers déduits) puis le FSD.
        pub fn files(mut self, files: &[(&str, &[u8])]) -> MemSource {
            let part = if self.meta { 1 } else { 0 };
            // Arborescence en mémoire.
            fn write(img: &mut Img, prefix: &str, files: &[(&str, &[u8])], part: u16) -> u32 {
                // Entrée « parent » (sans nom), alignée sur 4 octets.
                let mut dir = vec![0u8; 40];
                dir[0..2].copy_from_slice(&257u16.to_le_bytes());
                dir[18] = 0x0A;
                let mut names: Vec<String> = vec![];
                for (p, _) in files {
                    if let Some(rest) = p.strip_prefix(prefix) {
                        let first = rest.split('/').next().unwrap().to_string();
                        if !names.contains(&first) {
                            names.push(first);
                        }
                    }
                }
                for n in names {
                    let full = format!("{prefix}{n}");
                    let lbn = match files.iter().find(|(p, _)| *p == full) {
                        Some((_, data)) => {
                            let l = img.efe(false, data);
                            dir.extend(Img::fid(&n, false, l, part));
                            continue;
                        }
                        None => write(img, &format!("{full}/"), files, part),
                    };
                    dir.extend(Img::fid(&n, true, lbn, part));
                }
                img.efe(true, &dir)
            }
            let fsd_lbn = self.alloc();
            let root = write(&mut self, "", files, part);
            let mut fsd = vec![0u8; 2048];
            fsd[400..404].copy_from_slice(&2048u32.to_le_bytes());
            fsd[404..408].copy_from_slice(&root.to_le_bytes());
            fsd[408..410].copy_from_slice(&part.to_le_bytes());
            tag(&mut fsd, 256, fsd_lbn);
            let s = self.sector(fsd_lbn);
            self.src.put(s, 0, &fsd);
            self.src
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::device::{MediaKind, Physical, Track};

    fn bd() -> Physical {
        Physical { media: MediaKind::Bd, profile: 0x40, recordable: false, blank: false, sessions: 1, tracks: vec![Track { number: 1, session: 1, data: true, start: 0, length: 20000, mode: None }], leadout: 20000, disc_type: None, capacity: 20000 }
    }

    #[test]
    fn udf250_metadata_partition() {
        let src = build::Img::new(bd(), "RED_BIRD_3D", true).files(&[("BDMV/index.bdmv", b"INDX0200..."), ("BDMV/STREAM/00001.m2ts", b"x"), ("CERTIFICATE/id.bdmv", b"y")]);
        let u = Udf::open(&src, 0).unwrap();
        assert_eq!(u.volume_id(), "RED_BIRD_3D");
        let root: Vec<String> = u.list("").unwrap().into_iter().map(|e| e.name).collect();
        assert_eq!(root, ["BDMV", "CERTIFICATE"]);
        assert!(u.is_dir("bdmv"));
        assert!(u.exists("BDMV/STREAM/00001.m2ts"));
        assert_eq!(FileSystem::read(&u, "BDMV/index.bdmv", 4).unwrap(), b"INDX");
        assert_eq!(u.stat("BDMV/index.bdmv").unwrap().size, 11);
    }

    #[test]
    fn udf102_plain() {
        let src = build::Img::new(bd(), "DVD_LABEL", false).files(&[("VIDEO_TS/VIDEO_TS.IFO", b"DVDVIDEO-VMG")]);
        let u = Udf::open(&src, 0).unwrap();
        assert_eq!(u.volume_id(), "DVD_LABEL");
        assert_eq!(FileSystem::read(&u, "video_ts/video_ts.ifo", 64).unwrap(), b"DVDVIDEO-VMG");
        assert!(u.list("NOPE").is_err());
    }

    #[test]
    fn strings() {
        assert_eq!(osta_string(&[8, b'A', b'b']), "Ab");
        assert_eq!(osta_string(&[16, 0, b'E', 0, 0xE9]), "Eé");
    }
}
