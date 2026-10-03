//! Images disque comme source : `.iso` (2048 octets/secteur) et `.cue` + `.bin`
//! (2352 octets/secteur, une ou plusieurs pistes, multisession Redump).
//! Sert à `disc-identify --image`, aux tests et à l'identification après dump.

use super::{DiscSource, MediaKind, Physical, Track};
use std::fs::File;
use std::io::{self, Read, Seek, SeekFrom};
use std::path::{Path, PathBuf};
use std::sync::Mutex;

struct Segment {
    file: Mutex<File>,
    /// Premier LBA couvert par ce fichier.
    lba: u32,
    sectors: u32,
    sector_size: u32,
    /// Décalage des données utilisateur dans un secteur brut (16 mode 1, 24 mode 2).
    user_offset: u32,
}

pub struct Image {
    phys: Physical,
    segments: Vec<Segment>,
}

impl Image {
    pub fn open(path: &Path) -> io::Result<Image> {
        let ext = path.extension().and_then(|e| e.to_str()).unwrap_or("").to_ascii_lowercase();
        match ext.as_str() {
            "cue" => Self::open_cue(path),
            _ => Self::open_iso(path),
        }
    }

    pub fn open_iso(path: &Path) -> io::Result<Image> {
        let f = File::open(path)?;
        let size = f.metadata()?.len();
        let sectors = (size / 2048) as u32;
        let media = if size > 900 << 20 {
            if size > 9_000_000_000 {
                MediaKind::Bd
            } else {
                MediaKind::Dvd
            }
        } else {
            MediaKind::Cd
        };
        let phys = Physical {
            media,
            profile: 0,
            recordable: false,
            blank: false,
            sessions: 1,
            tracks: vec![Track { number: 1, session: 1, data: true, start: 0, length: sectors, mode: Some(1) }],
            leadout: sectors,
            disc_type: None,
            capacity: sectors as u64,
        };
        Ok(Image { phys, segments: vec![Segment { file: Mutex::new(f), lba: 0, sectors, sector_size: 2048, user_offset: 0 }] })
    }

    pub fn open_cue(path: &Path) -> io::Result<Image> {
        let text = std::fs::read_to_string(path)?;
        let dir = path.parent().unwrap_or(Path::new("."));
        let mut segments: Vec<Segment> = vec![];
        let mut tracks: Vec<Track> = vec![];
        let mut session = 1u8;
        let mut cur_file: Option<(PathBuf, u32)> = None; // (chemin, LBA de début)
        let mut next_lba: u32 = 0;
        let mut pending: Option<(u8, bool, u32, u8)> = None; // numéro, données, taille, mode
        // Redump insère un trou de 11 400 secteurs entre deux sessions (lead-out + lead-in + pregap).
        const SESSION_GAP: u32 = 11400;
        for line in text.lines() {
            let l = line.trim();
            let up = l.to_ascii_uppercase();
            if up.starts_with("REM SESSION") {
                let n: u8 = up[11..].trim().parse().unwrap_or(1);
                if n > session {
                    next_lba += SESSION_GAP;
                    session = n;
                }
            } else if up.starts_with("FILE") {
                let name = parse_quoted(&l[4..]);
                let p = dir.join(&name);
                cur_file = Some((p, next_lba));
            } else if up.starts_with("TRACK") {
                let parts: Vec<&str> = up.split_whitespace().collect();
                let num: u8 = parts.get(1).and_then(|s| s.parse().ok()).unwrap_or(1);
                let kind = parts.get(2).copied().unwrap_or("MODE1/2352");
                let (data, size, mode) = match kind {
                    "AUDIO" => (false, 2352, 0),
                    "MODE1/2048" => (true, 2048, 1),
                    "MODE1/2352" => (true, 2352, 1),
                    "MODE2/2336" => (true, 2336, 2),
                    "MODE2/2352" | "CDI/2352" => (true, 2352, 2),
                    _ => (true, 2352, 1),
                };
                pending = Some((num, data, size, mode));
            } else if up.starts_with("INDEX 01") {
                let (num, data, size, mode) = pending.take().unwrap_or((1, true, 2352, 1));
                let (fpath, flba) = cur_file.clone().ok_or_else(|| io::Error::new(io::ErrorKind::InvalidData, "INDEX sans FILE"))?;
                let off = parse_msf(&l[8..]).unwrap_or(0);
                if !segments.iter().any(|s| s.lba == flba) {
                    let f = File::open(&fpath)?;
                    let sectors = (f.metadata()?.len() / size as u64) as u32;
                    let user_offset = match (size, mode) {
                        (2048, _) => 0,
                        (2336, _) => 8,
                        (_, 2) => 24,
                        _ => 16,
                    };
                    next_lba = flba + sectors;
                    segments.push(Segment { file: Mutex::new(f), lba: flba, sectors, sector_size: size, user_offset });
                }
                tracks.push(Track { number: num, session, data, start: flba + off, length: 0, mode: if data { Some(mode) } else { None } });
            }
        }
        if tracks.is_empty() {
            return Err(io::Error::new(io::ErrorKind::InvalidData, "aucune piste dans le .cue"));
        }
        let leadout = segments.iter().map(|s| s.lba + s.sectors).max().unwrap_or(0);
        // Fin de chaque session : fin du dernier fichier de la session.
        let mut leadouts: Vec<(u8, u32)> = vec![];
        for t in &tracks {
            let end = segments.iter().filter(|s| s.lba <= t.start).max_by_key(|s| s.lba).map(|s| s.lba + s.sectors).unwrap_or(leadout);
            match leadouts.iter_mut().find(|l| l.0 == t.session) {
                Some(l) => l.1 = l.1.max(end),
                None => leadouts.push((t.session, end)),
            }
        }
        super::cdrom::compute_lengths(&mut tracks, &leadouts);
        let disc_type = if tracks.iter().any(|t| t.mode == Some(2)) { Some(0x20) } else { Some(0x00) };
        let phys = Physical {
            media: MediaKind::Cd,
            profile: 0,
            recordable: false,
            blank: false,
            sessions: session,
            tracks,
            leadout,
            disc_type,
            capacity: leadout as u64,
        };
        Ok(Image { phys, segments })
    }

    fn segment(&self, lba: u32) -> io::Result<&Segment> {
        self.segments.iter().find(|s| lba >= s.lba && lba < s.lba + s.sectors).ok_or_else(|| io::Error::new(io::ErrorKind::UnexpectedEof, format!("LBA {lba} hors image")))
    }
}

fn parse_quoted(s: &str) -> String {
    let s = s.trim();
    if let Some(r) = s.strip_prefix('"') {
        r.split('"').next().unwrap_or("").to_string()
    } else {
        s.split_whitespace().next().unwrap_or("").to_string()
    }
}

fn parse_msf(s: &str) -> Option<u32> {
    let p: Vec<u32> = s.trim().split(':').filter_map(|x| x.parse().ok()).collect();
    if p.len() == 3 {
        Some((p[0] * 60 + p[1]) * 75 + p[2])
    } else {
        None
    }
}

impl DiscSource for Image {
    fn physical(&self) -> &Physical {
        &self.phys
    }
    fn read_data(&self, lba: u32, count: u32) -> io::Result<Vec<u8>> {
        let mut out = Vec::with_capacity(count as usize * 2048);
        for l in lba..lba + count {
            let s = self.segment(l)?;
            let mut f = s.file.lock().unwrap();
            f.seek(SeekFrom::Start((l - s.lba) as u64 * s.sector_size as u64 + s.user_offset as u64))?;
            let mut buf = [0u8; 2048];
            f.read_exact(&mut buf)?;
            out.extend_from_slice(&buf);
        }
        Ok(out)
    }
    fn read_raw(&self, lba: u32) -> io::Result<Vec<u8>> {
        let s = self.segment(lba)?;
        let mut f = s.file.lock().unwrap();
        let mut buf = vec![0u8; 2352];
        if s.sector_size == 2352 {
            f.seek(SeekFrom::Start((lba - s.lba) as u64 * 2352))?;
            f.read_exact(&mut buf)?;
        } else {
            f.seek(SeekFrom::Start((lba - s.lba) as u64 * s.sector_size as u64))?;
            let n = s.sector_size as usize;
            f.read_exact(&mut buf[16..16 + n.min(2336)])?;
        }
        Ok(buf)
    }
}
