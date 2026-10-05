//! Lecteur optique réel : ioctl CD-ROM et commandes MMC via SG_IO.
//! Seules des commandes autorisées aux utilisateurs non privilégiés sont
//! utilisées ici (INQUIRY, GET CONFIGURATION, READ TOC, READ CAPACITY,
//! READ DISC INFORMATION, READ CD) ; les commandes constructeur passent par
//! l'assistant privilégié.

use super::{msf_to_lba, DiscSource, DriveInfo, MediaKind, Physical, Track};
use crate::sys;
use std::ffi::c_void;
use std::fs::{File, OpenOptions};
use std::io::{self, Read, Seek, SeekFrom};
use std::os::fd::AsRawFd;
use std::os::unix::fs::OpenOptionsExt;
use std::path::PathBuf;
use std::sync::Mutex;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DriveStatus {
    NoInfo,
    NoDisc,
    TrayOpen,
    NotReady,
    DiscOk,
}

pub struct Cdrom {
    pub path: String,
    file: Mutex<File>,
    phys: Physical,
    drive: DriveInfo,
}

pub fn open_dev(path: &str) -> io::Result<File> {
    OpenOptions::new().read(true).custom_flags(sys::O_NONBLOCK).open(path)
}

pub fn drive_status(path: &str) -> io::Result<DriveStatus> {
    let f = open_dev(path)?;
    let r = unsafe { sys::ioctl(f.as_raw_fd(), sys::CDROM_DRIVE_STATUS, sys::CDSL_CURRENT) };
    if r < 0 {
        return Err(sys::last_err());
    }
    Ok(match r {
        sys::CDS_NO_DISC => DriveStatus::NoDisc,
        sys::CDS_TRAY_OPEN => DriveStatus::TrayOpen,
        sys::CDS_DRIVE_NOT_READY => DriveStatus::NotReady,
        sys::CDS_DISC_OK => DriveStatus::DiscOk,
        _ => DriveStatus::NoInfo,
    })
}

pub fn lock_door(path: &str, lock: bool) -> io::Result<()> {
    let f = open_dev(path)?;
    if unsafe { sys::ioctl(f.as_raw_fd(), sys::CDROM_LOCKDOOR, lock as std::ffi::c_ulong) } < 0 {
        return Err(sys::last_err());
    }
    Ok(())
}

pub fn eject(path: &str) -> io::Result<()> {
    let _ = lock_door(path, false);
    let f = open_dev(path)?;
    if unsafe { sys::ioctl(f.as_raw_fd(), sys::CDROMEJECT) } < 0 {
        return Err(sys::last_err());
    }
    Ok(())
}

pub fn close_tray(path: &str) -> io::Result<()> {
    let f = open_dev(path)?;
    if unsafe { sys::ioctl(f.as_raw_fd(), sys::CDROMCLOSETRAY) } < 0 {
        return Err(sys::last_err());
    }
    Ok(())
}

/// Fabricant et modèle du lecteur (sysfs), ex. « ASUS DRW-24F1ST ».
pub fn drive_model(path: &str) -> String {
    let name = path.trim_start_matches("/dev/");
    let rd = |f: &str| std::fs::read_to_string(format!("/sys/block/{name}/device/{f}")).map(|s| s.trim().to_string()).unwrap_or_default();
    format!("{} {}", rd("vendor"), rd("model")).trim().to_string()
}

/// Erreur SCSI avec données de sense.
fn scsi_err(sense: &[u8], status: u8) -> io::Error {
    let (key, asc, ascq) = if sense.len() >= 14 && (sense[0] & 0x7f) >= 0x70 {
        (sense[2] & 0x0f, sense[12], sense[13])
    } else {
        (0, 0, 0)
    };
    io::Error::new(io::ErrorKind::Other, format!("erreur SCSI status=0x{status:02x} sense={key:x}/{asc:02x}/{ascq:02x}"))
}

/// Exécute une commande SCSI en lecture.
pub fn sg_read(f: &File, cdb: &[u8], buf: &mut [u8], timeout_ms: u32) -> io::Result<usize> {
    let mut sense = [0u8; 32];
    let mut hdr = sys::SgIoHdr {
        interface_id: b'S' as i32,
        dxfer_direction: if buf.is_empty() { sys::SG_DXFER_NONE } else { sys::SG_DXFER_FROM_DEV },
        cmd_len: cdb.len() as u8,
        mx_sb_len: sense.len() as u8,
        iovec_count: 0,
        dxfer_len: buf.len() as u32,
        dxferp: buf.as_mut_ptr() as *mut c_void,
        cmdp: cdb.as_ptr(),
        sbp: sense.as_mut_ptr(),
        timeout: timeout_ms,
        flags: 0,
        pack_id: 0,
        usr_ptr: std::ptr::null_mut(),
        status: 0,
        masked_status: 0,
        msg_status: 0,
        sb_len_wr: 0,
        host_status: 0,
        driver_status: 0,
        resid: 0,
        duration: 0,
        info: 0,
    };
    if unsafe { sys::ioctl(f.as_raw_fd(), sys::SG_IO, &mut hdr as *mut sys::SgIoHdr) } < 0 {
        return Err(sys::last_err());
    }
    if hdr.status != 0 || hdr.host_status != 0 || (hdr.driver_status & !0x08) != 0 || (hdr.info & 1) != 0 {
        return Err(scsi_err(&sense[..hdr.sb_len_wr as usize], hdr.status));
    }
    Ok(buf.len().saturating_sub(hdr.resid.max(0) as usize))
}

fn be16(b: &[u8]) -> u16 {
    u16::from_be_bytes([b[0], b[1]])
}
fn be32(b: &[u8]) -> u32 {
    u32::from_be_bytes([b[0], b[1], b[2], b[3]])
}

pub fn inquiry(f: &File) -> io::Result<DriveInfo> {
    let mut buf = [0u8; 96];
    sg_read(f, &[0x12, 0, 0, 0, 96, 0], &mut buf, 5000)?;
    let s = |r: std::ops::Range<usize>| String::from_utf8_lossy(&buf[r]).trim().to_string();
    Ok(DriveInfo { vendor: s(8..16), model: s(16..32), firmware: s(32..36) })
}

pub fn current_profile(f: &File) -> io::Result<u16> {
    let mut buf = [0u8; 8];
    sg_read(f, &[0x46, 0x01, 0, 0, 0, 0, 0, 0, 8, 0], &mut buf, 5000)?;
    Ok(be16(&buf[6..8]))
}

pub fn read_capacity(f: &File) -> io::Result<u64> {
    let mut buf = [0u8; 8];
    sg_read(f, &[0x25, 0, 0, 0, 0, 0, 0, 0, 0, 0], &mut buf, 10000)?;
    Ok(be32(&buf[0..4]) as u64 + 1)
}

/// READ DISC INFORMATION : (vierge, nombre de sessions).
pub fn disc_info(f: &File) -> io::Result<(bool, u8)> {
    let mut buf = [0u8; 34];
    sg_read(f, &[0x51, 0, 0, 0, 0, 0, 0, 0, 34, 0], &mut buf, 5000)?;
    let status = buf[2] & 0x03;
    Ok((status == 0, buf[4]))
}

/// TOC complète (format 2) : pistes, sessions, type de disque, lead-out.
pub fn read_full_toc(f: &File) -> io::Result<(Vec<Track>, u32, Option<u8>, u8)> {
    let mut buf = vec![0u8; 4096];
    let n = sg_read(f, &[0x43, 0x02, 0x02, 0, 0, 0, 1, 0x10, 0x00, 0], &mut buf, 10000)?;
    if n < 4 {
        return Err(io::Error::new(io::ErrorKind::InvalidData, "TOC trop courte"));
    }
    let len = (be16(&buf[0..2]) as usize + 2).min(n);
    let mut tracks: Vec<Track> = vec![];
    let mut disc_type = None;
    let mut leadouts: Vec<(u8, u32)> = vec![];
    let mut sessions = 1u8;
    let mut off = 4;
    while off + 11 <= len {
        let d = &buf[off..off + 11];
        off += 11;
        let session = d[0];
        let control = d[1] & 0x0f;
        let adr = d[1] >> 4;
        let point = d[3];
        sessions = sessions.max(session);
        if adr != 1 {
            continue;
        }
        match point {
            0x01..=0x63 => {
                let lba = msf_to_lba(d[8], d[9], d[10]).max(0) as u32;
                tracks.push(Track { number: point, session, data: control & 0x04 != 0, start: lba, length: 0, mode: None });
            }
            0xA0 => {
                if session == 1 {
                    disc_type = Some(d[9]);
                }
            }
            0xA2 => leadouts.push((session, msf_to_lba(d[8], d[9], d[10]).max(0) as u32)),
            _ => {}
        }
    }
    tracks.sort_by_key(|t| t.number);
    let leadout = leadouts.iter().map(|l| l.1).max().unwrap_or(0);
    compute_lengths(&mut tracks, &leadouts);
    Ok((tracks, leadout, disc_type, sessions))
}

/// TOC simple (format 0), pour DVD/BD ou si la TOC complète échoue.
pub fn read_simple_toc(f: &File) -> io::Result<(Vec<Track>, u32)> {
    let mut buf = vec![0u8; 2048];
    let n = sg_read(f, &[0x43, 0, 0, 0, 0, 0, 1, 0x08, 0x00, 0], &mut buf, 10000)?;
    let len = (be16(&buf[0..2]) as usize + 2).min(n);
    let mut tracks = vec![];
    let mut leadout = 0;
    let mut off = 4;
    while off + 8 <= len {
        let d = &buf[off..off + 8];
        off += 8;
        let lba = be32(&d[4..8]);
        if d[2] == 0xAA {
            leadout = lba;
        } else {
            tracks.push(Track { number: d[2], session: 1, data: d[1] & 0x04 != 0, start: lba, length: 0, mode: None });
        }
    }
    compute_lengths(&mut tracks, &[(1, leadout)]);
    Ok((tracks, leadout))
}

/// Longueur de chaque piste = début de la suivante (même session) ou lead-out
/// de sa session.
pub fn compute_lengths(tracks: &mut [Track], leadouts: &[(u8, u32)]) {
    for i in 0..tracks.len() {
        let end = match tracks.get(i + 1) {
            Some(n) if n.session == tracks[i].session => n.start,
            _ => leadouts.iter().find(|l| l.0 == tracks[i].session).map(|l| l.1).or_else(|| leadouts.iter().map(|l| l.1).max()).unwrap_or(tracks[i].start),
        };
        tracks[i].length = end.saturating_sub(tracks[i].start);
    }
}

/// READ CD brut (2352 octets : synchro + en-tête + données + EDC/ECC, ou audio).
pub fn read_cd_raw(f: &File, lba: u32) -> io::Result<Vec<u8>> {
    let mut buf = vec![0u8; 2352];
    let l = lba.to_be_bytes();
    sg_read(f, &[0xBE, 0, l[0], l[1], l[2], l[3], 0, 0, 1, 0xF8, 0, 0], &mut buf, 10000)?;
    Ok(buf)
}

/// READ CD, données utilisateur seulement (2048 octets par secteur en mode 1
/// et mode 2 forme 1), quel que soit le type de secteur.
pub fn read_cd_user(f: &File, lba: u32, count: u32) -> io::Result<Vec<u8>> {
    let mut buf = vec![0u8; count as usize * 2048];
    let l = lba.to_be_bytes();
    let c = count.to_be_bytes();
    sg_read(f, &[0xBE, 0, l[0], l[1], l[2], l[3], c[1], c[2], c[3], 0x10, 0, 0], &mut buf, 20000)?;
    Ok(buf)
}

/// READ(12) par SG_IO (DVD, BD) : ne dépend pas de la taille du périphérique
/// bloc connue du noyau.
pub fn read12(f: &File, lba: u32, count: u32) -> io::Result<Vec<u8>> {
    let mut buf = vec![0u8; count as usize * 2048];
    let l = lba.to_be_bytes();
    let c = count.to_be_bytes();
    sg_read(f, &[0xA8, 0, l[0], l[1], l[2], l[3], c[0], c[1], c[2], c[3], 0, 0], &mut buf, 20000)?;
    Ok(buf)
}

impl Cdrom {
    pub fn open(path: &str) -> io::Result<Cdrom> {
        let file = open_dev(path)?;
        let drive = inquiry(&file).unwrap_or_default();
        let profile = current_profile(&file).unwrap_or(0);
        let (mut media, recordable) = MediaKind::from_profile(profile);
        let (blank, _) = disc_info(&file).unwrap_or((false, 1));
        let capacity = read_capacity(&file).unwrap_or(0);
        let (mut tracks, mut leadout, mut disc_type, mut sessions) = (vec![], 0, None, 1);
        if media == MediaKind::Cd || media == MediaKind::Unknown {
            if let Ok(t) = read_full_toc(&file) {
                (tracks, leadout, disc_type, sessions) = t;
                if media == MediaKind::Unknown && !tracks.is_empty() {
                    media = MediaKind::Cd;
                }
            }
        }
        if tracks.is_empty() && !blank {
            if let Ok((t, l)) = read_simple_toc(&file) {
                tracks = t;
                leadout = l;
            }
        }
        if tracks.is_empty() && capacity > 0 && !blank {
            tracks.push(Track { number: 1, session: 1, data: true, start: 0, length: capacity as u32, mode: None });
            leadout = capacity as u32;
        }
        // Mode des pistes de données sur CD (octet 15 de l'en-tête brut).
        if media == MediaKind::Cd {
            for t in tracks.iter_mut().filter(|t| t.data) {
                if let Ok(raw) = read_cd_raw(&file, t.start) {
                    if raw[0] == 0 && raw[1..11].iter().all(|&b| b == 0xff) && raw[11] == 0 {
                        t.mode = Some(raw[15]);
                    }
                }
            }
        }
        let phys = Physical { media, profile, recordable, blank, sessions, tracks, leadout, disc_type, capacity };
        Ok(Cdrom { path: path.to_string(), file: Mutex::new(file), phys, drive })
    }
}

impl DiscSource for Cdrom {
    fn physical(&self) -> &Physical {
        &self.phys
    }
    fn read_data(&self, lba: u32, count: u32) -> io::Result<Vec<u8>> {
        let mut f = self.file.lock().unwrap();
        // Lecture par le périphérique bloc ; elle peut échouer juste après
        // l'insertion (taille encore inconnue du noyau, ouverture O_NONBLOCK)
        // ou sur certains disques en mode 2 : repli sur une commande MMC.
        let block = (|| {
            f.seek(SeekFrom::Start(lba as u64 * 2048))?;
            let mut buf = vec![0u8; count as usize * 2048];
            f.read_exact(&mut buf)?;
            Ok::<_, io::Error>(buf)
        })();
        match block {
            Ok(b) => Ok(b),
            Err(e1) => {
                let r = if self.phys.media == MediaKind::Cd { read_cd_user(&f, lba, count) } else { read12(&f, lba, count) };
                r.map_err(|e2| io::Error::new(e2.kind(), format!("secteur {lba} : lecture bloc : {e1} ; MMC : {e2}")))
            }
        }
    }
    fn read_raw(&self, lba: u32) -> io::Result<Vec<u8>> {
        let f = self.file.lock().unwrap();
        read_cd_raw(&f, lba)
    }
    fn device(&self) -> Option<String> {
        Some(self.path.clone())
    }
    fn drive(&self) -> Option<DriveInfo> {
        Some(self.drive.clone())
    }
    fn mount_point(&self) -> Option<PathBuf> {
        super::find_mount_point(&self.path)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn lengths_by_session() {
        let mut t = vec![
            Track { number: 1, session: 1, data: false, start: 0, length: 0, mode: None },
            Track { number: 2, session: 1, data: false, start: 1000, length: 0, mode: None },
            Track { number: 3, session: 2, data: true, start: 20000, length: 0, mode: None },
        ];
        compute_lengths(&mut t, &[(1, 5000), (2, 30000)]);
        assert_eq!(t.iter().map(|x| x.length).collect::<Vec<_>>(), vec![1000, 4000, 10000]);
    }
}
