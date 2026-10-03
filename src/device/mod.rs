//! Sources de disque : lecteur réel (`cdrom`), images (`image`), mémoire (tests).

pub mod cdrom;
pub mod image;

use crate::json::Value;
use crate::jobj;
use std::cell::Cell;
use std::io;
use std::path::PathBuf;
use std::time::{Duration, Instant};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum MediaKind {
    Cd,
    Dvd,
    Bd,
    HdDvd,
    /// Cartouche lue par un adaptateur USB (Retrode) : un fichier ROM.
    Cart,
    Unknown,
}

impl MediaKind {
    pub fn name(self) -> &'static str {
        match self {
            MediaKind::Cd => "cd",
            MediaKind::Dvd => "dvd",
            MediaKind::Bd => "bd",
            MediaKind::HdDvd => "hddvd",
            MediaKind::Cart => "cart",
            MediaKind::Unknown => "unknown",
        }
    }
    /// Profil MMC (GET CONFIGURATION) → famille de média, gravable ?
    pub fn from_profile(p: u16) -> (MediaKind, bool) {
        match p {
            0x08 => (MediaKind::Cd, false),
            0x09 | 0x0A => (MediaKind::Cd, true),
            0x10 => (MediaKind::Dvd, false),
            0x11..=0x2B => (MediaKind::Dvd, true),
            0x40 => (MediaKind::Bd, false),
            0x41..=0x43 => (MediaKind::Bd, true),
            0x50 => (MediaKind::HdDvd, false),
            0x51..=0x5A => (MediaKind::HdDvd, true),
            _ => (MediaKind::Unknown, false),
        }
    }
}

#[derive(Clone, Debug, PartialEq)]
pub struct Track {
    pub number: u8,
    pub session: u8,
    pub data: bool,
    /// LBA de l'index 1.
    pub start: u32,
    /// Longueur en secteurs jusqu'au début de la piste suivante (ou lead-out).
    pub length: u32,
    /// Mode du secteur de données (1 ou 2) si connu.
    pub mode: Option<u8>,
}

#[derive(Clone, Debug, PartialEq)]
pub struct Physical {
    pub media: MediaKind,
    pub profile: u16,
    pub recordable: bool,
    pub blank: bool,
    pub sessions: u8,
    pub tracks: Vec<Track>,
    /// LBA du lead-out (fin de la dernière session).
    pub leadout: u32,
    /// Octet « disc type » de la TOC complète : 0x00 CD-DA/ROM, 0x10 CD-i, 0x20 CD-ROM XA.
    pub disc_type: Option<u8>,
    /// Capacité lisible en secteurs de 2048 octets.
    pub capacity: u64,
}

impl Physical {
    pub fn audio_tracks(&self) -> usize {
        self.tracks.iter().filter(|t| !t.data).count()
    }
    pub fn data_tracks(&self) -> usize {
        self.tracks.iter().filter(|t| t.data).count()
    }
    pub fn first_data(&self) -> Option<&Track> {
        self.tracks.iter().find(|t| t.data)
    }
    pub fn last_data(&self) -> Option<&Track> {
        self.tracks.iter().rev().find(|t| t.data)
    }
    pub fn to_value(&self) -> Value {
        let tracks: Vec<Value> = self
            .tracks
            .iter()
            .map(|t| {
                jobj! {
                    "n" => t.number, "session" => t.session,
                    "type" => if t.data { "data" } else { "audio" },
                    "mode" => t.mode.map(|m| m.to_string()),
                    "start" => t.start, "sectors" => t.length,
                }
            })
            .collect();
        jobj! {
            "media" => self.media.name(), "profile" => format!("0x{:04x}", self.profile),
            "pressed" => !self.recordable, "blank" => self.blank, "sessions" => self.sessions,
            "tracks" => tracks, "leadout" => self.leadout, "capacity_sectors" => self.capacity,
            "disc_type" => self.disc_type.map(|d| format!("0x{d:02x}")),
        }
    }
}

#[derive(Clone, Debug, Default, PartialEq)]
pub struct DriveInfo {
    pub vendor: String,
    pub model: String,
    pub firmware: String,
}

/// Une source de lecture : lecteur, image ou mémoire.
pub trait DiscSource {
    fn physical(&self) -> &Physical;
    /// Lit `count` secteurs de données utilisateur (2048 octets) à partir de `lba`.
    fn read_data(&self, lba: u32, count: u32) -> io::Result<Vec<u8>>;
    /// Lit un secteur brut de 2352 octets (audio ou données avec en-tête).
    fn read_raw(&self, lba: u32) -> io::Result<Vec<u8>>;
    fn device(&self) -> Option<String> {
        None
    }
    fn drive(&self) -> Option<DriveInfo> {
        None
    }
    /// Point de montage du système de fichiers, s'il est monté.
    fn mount_point(&self) -> Option<PathBuf> {
        None
    }
}

/// Enveloppe qui impose le budget de sondage (octets et durée).
pub struct Budget<'a> {
    pub inner: &'a dyn DiscSource,
    pub max_bytes: u64,
    pub deadline: Instant,
    pub used: Cell<u64>,
    pub exceeded: Cell<bool>,
}

impl<'a> Budget<'a> {
    pub fn new(inner: &'a dyn DiscSource, max_bytes: u64, max_time: Duration) -> Self {
        Budget { inner, max_bytes, deadline: Instant::now() + max_time, used: Cell::new(0), exceeded: Cell::new(false) }
    }
    fn charge(&self, n: u64) -> io::Result<()> {
        if self.used.get() + n > self.max_bytes || Instant::now() > self.deadline {
            self.exceeded.set(true);
            return Err(io::Error::new(io::ErrorKind::Other, "budget de sondage dépassé"));
        }
        self.used.set(self.used.get() + n);
        Ok(())
    }
}

impl DiscSource for Budget<'_> {
    fn physical(&self) -> &Physical {
        self.inner.physical()
    }
    fn read_data(&self, lba: u32, count: u32) -> io::Result<Vec<u8>> {
        self.charge(count as u64 * 2048)?;
        self.inner.read_data(lba, count)
    }
    fn read_raw(&self, lba: u32) -> io::Result<Vec<u8>> {
        self.charge(2352)?;
        self.inner.read_raw(lba)
    }
    fn device(&self) -> Option<String> {
        self.inner.device()
    }
    fn drive(&self) -> Option<DriveInfo> {
        self.inner.drive()
    }
    fn mount_point(&self) -> Option<PathBuf> {
        self.inner.mount_point()
    }
}

/// Source en mémoire pour les tests : secteurs de 2048 et bruts de 2352.
pub struct MemSource {
    pub phys: Physical,
    pub data: std::collections::HashMap<u32, Vec<u8>>,
    pub raw: std::collections::HashMap<u32, Vec<u8>>,
}

impl MemSource {
    pub fn new(phys: Physical) -> Self {
        MemSource { phys, data: Default::default(), raw: Default::default() }
    }
    /// Écrit des octets à une position (en octets) relative au LBA `base`.
    pub fn put(&mut self, base: u32, offset: usize, bytes: &[u8]) {
        for (k, b) in bytes.iter().enumerate() {
            let pos = offset + k;
            let lba = base + (pos / 2048) as u32;
            let s = self.data.entry(lba).or_insert_with(|| vec![0; 2048]);
            s[pos % 2048] = *b;
        }
    }
    pub fn put_raw(&mut self, lba: u32, offset: usize, bytes: &[u8]) {
        let s = self.raw.entry(lba).or_insert_with(|| vec![0; 2352]);
        s[offset..offset + bytes.len()].copy_from_slice(bytes);
    }
}

impl DiscSource for MemSource {
    fn physical(&self) -> &Physical {
        &self.phys
    }
    fn read_data(&self, lba: u32, count: u32) -> io::Result<Vec<u8>> {
        let mut out = Vec::with_capacity(count as usize * 2048);
        for l in lba..lba + count {
            if l as u64 >= self.phys.capacity.max(self.phys.leadout as u64) {
                return Err(io::Error::new(io::ErrorKind::UnexpectedEof, "hors du disque"));
            }
            out.extend_from_slice(self.data.get(&l).map(|v| v.as_slice()).unwrap_or(&[0u8; 2048]));
        }
        Ok(out)
    }
    fn read_raw(&self, lba: u32) -> io::Result<Vec<u8>> {
        Ok(self.raw.get(&lba).cloned().unwrap_or_else(|| vec![0; 2352]))
    }
}

pub fn msf_to_lba(m: u8, s: u8, f: u8) -> i64 {
    (m as i64 * 60 + s as i64) * 75 + f as i64 - 150
}

/// Liste des lecteurs optiques (`/dev/sr*`).
pub fn list_drives() -> Vec<String> {
    let mut v: Vec<String> = std::fs::read_dir("/sys/class/block")
        .map(|it| it.filter_map(|e| e.ok()).map(|e| e.file_name().to_string_lossy().into_owned()).filter(|n| n.starts_with("sr")).map(|n| format!("/dev/{n}")).collect())
        .unwrap_or_default();
    v.sort();
    v
}

/// Point de montage d'un périphérique d'après `/proc/self/mountinfo`.
/// Périphérique SCSI générique (/dev/sgN) d'un lecteur /dev/srN, via sysfs.
/// redumper l'exige sous Linux : il ouvre le lecteur en lecture-écriture et
/// exclusif, ce que /dev/srN refuse pour un disque pressé.
pub fn sg_of(dev: &str) -> Option<String> {
    let real = std::fs::canonicalize(dev).ok()?;
    let name = real.file_name()?.to_str()?.to_string();
    let dir = std::fs::read_dir(format!("/sys/block/{name}/device/scsi_generic")).ok()?;
    for e in dir.flatten() {
        let n = e.file_name().to_string_lossy().into_owned();
        if n.starts_with("sg") {
            return Some(format!("/dev/{n}"));
        }
    }
    None
}

pub fn find_mount_point(dev: &str) -> Option<PathBuf> {
    let real = std::fs::canonicalize(dev).ok()?;
    let text = std::fs::read_to_string("/proc/self/mountinfo").ok()?;
    for line in text.lines() {
        let (pre, post) = line.split_once(" - ")?;
        let src = post.split_whitespace().nth(1)?;
        if std::fs::canonicalize(src).ok().as_deref() == Some(real.as_path()) {
            let mp = pre.split_whitespace().nth(4)?;
            return Some(PathBuf::from(unescape_mount(mp)));
        }
    }
    None
}

fn unescape_mount(s: &str) -> String {
    let b = s.as_bytes();
    let mut out = Vec::with_capacity(b.len());
    let mut i = 0;
    while i < b.len() {
        if b[i] == b'\\' && i + 3 < b.len() {
            if let Ok(v) = u8::from_str_radix(&s[i + 1..i + 4], 8) {
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

/// Périphérique désigné par une URI ou un point de montage (coquilles natives) :
/// `/dev/sr0`, `sr0`, `cdda://sr0`, `cdda://local/dev/sr0`, `file:///run/media/u/DVD`.
pub fn device_from_uri(s: &str) -> Option<String> {
    let s = s.trim();
    if s.starts_with("/dev/") {
        return Some(s.to_string());
    }
    if s.starts_with("sr") && s[2..].chars().all(|c| c.is_ascii_digit()) && s.len() > 2 {
        return Some(format!("/dev/{s}"));
    }
    if let Some((scheme, rest)) = s.split_once("://") {
        if scheme != "file" {
            let rest = rest.trim_end_matches('/');
            if let Some(i) = rest.find("sr") {
                let num: String = rest[i + 2..].chars().take_while(|c| c.is_ascii_digit()).collect();
                if !num.is_empty() {
                    return Some(format!("/dev/sr{num}"));
                }
            }
            return None;
        }
        return device_for_mount(&percent_decode(rest));
    }
    if s.starts_with('/') {
        return device_for_mount(s);
    }
    None
}

fn percent_decode(s: &str) -> String {
    let b = s.as_bytes();
    let mut out = vec![];
    let mut i = 0;
    while i < b.len() {
        if b[i] == b'%' && i + 2 < b.len() {
            if let Ok(v) = u8::from_str_radix(&s[i + 1..i + 3], 16) {
                out.push(v);
                i += 3;
                continue;
            }
        }
        out.push(b[i]);
        i += 1;
    }
    String::from_utf8_lossy(&out).into_owned()
}

/// Périphérique monté sur `mp` (d'après `/proc/self/mountinfo`).
pub fn device_for_mount(mp: &str) -> Option<String> {
    let want = std::fs::canonicalize(mp).ok()?;
    let text = std::fs::read_to_string("/proc/self/mountinfo").ok()?;
    for line in text.lines() {
        let (pre, post) = line.split_once(" - ")?;
        let point = unescape_mount(pre.split_whitespace().nth(4)?);
        if std::path::Path::new(&point) == want {
            return post.split_whitespace().nth(1).map(|s| s.to_string());
        }
    }
    None
}

#[cfg(test)]
mod uri_tests {
    #[test]
    fn uris() {
        assert_eq!(super::device_from_uri("cdda://sr1/").as_deref(), Some("/dev/sr1"));
        assert_eq!(super::device_from_uri("cdda://local/dev/sr0").as_deref(), Some("/dev/sr0"));
        assert_eq!(super::device_from_uri("sr0").as_deref(), Some("/dev/sr0"));
        assert_eq!(super::device_from_uri("/dev/sr2").as_deref(), Some("/dev/sr2"));
    }
}
