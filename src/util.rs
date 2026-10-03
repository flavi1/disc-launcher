//! Utilitaires : SHA-1, hexadécimal, base64, découpage de commandes, attente
//! de processus avec délai.

use std::io::{self, Read};
use std::path::Path;
use std::process::{Child, ExitStatus};
use std::time::{Duration, Instant};

/// SHA-1 incrémental (FIPS 180-4).
#[derive(Clone)]
pub struct Sha1 {
    h: [u32; 5],
    buf: Vec<u8>,
    len: u64,
}

impl Default for Sha1 {
    fn default() -> Self {
        Self::new()
    }
}

impl Sha1 {
    pub fn new() -> Sha1 {
        Sha1 { h: [0x67452301, 0xEFCDAB89, 0x98BADCFE, 0x10325476, 0xC3D2E1F0], buf: Vec::with_capacity(64), len: 0 }
    }
    pub fn update(&mut self, mut data: &[u8]) {
        self.len += data.len() as u64;
        if !self.buf.is_empty() {
            let need = 64 - self.buf.len();
            let take = need.min(data.len());
            self.buf.extend_from_slice(&data[..take]);
            data = &data[take..];
            if self.buf.len() == 64 {
                let b: [u8; 64] = self.buf[..].try_into().unwrap();
                self.block(&b);
                self.buf.clear();
            }
        }
        while data.len() >= 64 {
            let b: [u8; 64] = data[..64].try_into().unwrap();
            self.block(&b);
            data = &data[64..];
        }
        self.buf.extend_from_slice(data);
    }
    fn block(&mut self, b: &[u8; 64]) {
        let mut w = [0u32; 80];
        for i in 0..16 {
            w[i] = u32::from_be_bytes([b[i * 4], b[i * 4 + 1], b[i * 4 + 2], b[i * 4 + 3]]);
        }
        for i in 16..80 {
            w[i] = (w[i - 3] ^ w[i - 8] ^ w[i - 14] ^ w[i - 16]).rotate_left(1);
        }
        let [mut a, mut bb, mut c, mut d, mut e] = self.h;
        for (i, wi) in w.iter().enumerate() {
            let (f, k) = match i {
                0..=19 => ((bb & c) | (!bb & d), 0x5A827999),
                20..=39 => (bb ^ c ^ d, 0x6ED9EBA1),
                40..=59 => ((bb & c) | (bb & d) | (c & d), 0x8F1BBCDC),
                _ => (bb ^ c ^ d, 0xCA62C1D6),
            };
            let t = a.rotate_left(5).wrapping_add(f).wrapping_add(e).wrapping_add(k).wrapping_add(*wi);
            e = d;
            d = c;
            c = bb.rotate_left(30);
            bb = a;
            a = t;
        }
        self.h[0] = self.h[0].wrapping_add(a);
        self.h[1] = self.h[1].wrapping_add(bb);
        self.h[2] = self.h[2].wrapping_add(c);
        self.h[3] = self.h[3].wrapping_add(d);
        self.h[4] = self.h[4].wrapping_add(e);
    }
    pub fn finish(mut self) -> [u8; 20] {
        let bits = self.len.wrapping_mul(8);
        let mut pad = vec![0x80u8];
        while (self.buf.len() + pad.len()) % 64 != 56 {
            pad.push(0);
        }
        pad.extend_from_slice(&bits.to_be_bytes());
        let saved = self.len;
        self.update(&pad);
        self.len = saved;
        let mut out = [0u8; 20];
        for (i, v) in self.h.iter().enumerate() {
            out[i * 4..i * 4 + 4].copy_from_slice(&v.to_be_bytes());
        }
        out
    }
    pub fn hex(self) -> String {
        hex(&self.finish())
    }
}

pub fn sha1_hex(data: &[u8]) -> String {
    let mut s = Sha1::new();
    s.update(data);
    s.hex()
}

/// SHA-1 d'un fichier, avec rappel de progression (octets lus).
pub fn sha1_file(path: &Path, mut progress: impl FnMut(u64)) -> io::Result<String> {
    let mut f = std::fs::File::open(path)?;
    let mut s = Sha1::new();
    let mut buf = vec![0u8; 1 << 20];
    let mut total = 0u64;
    loop {
        let n = f.read(&mut buf)?;
        if n == 0 {
            break;
        }
        s.update(&buf[..n]);
        total += n as u64;
        progress(total);
    }
    Ok(s.hex())
}

/// MD5 incrémental (RFC 1321) : empreinte rapide servant à reconnaître un
/// fichier déplacé ou renommé (pas un usage cryptographique).
#[derive(Clone)]
pub struct Md5 {
    s: [u32; 4],
    buf: Vec<u8>,
    len: u64,
}

impl Default for Md5 {
    fn default() -> Self {
        Self::new()
    }
}

const MD5_K: [u32; 64] = {
    let mut k = [0u32; 64];
    // floor(abs(sin(i + 1)) * 2^32), précalculé
    let t: [u32; 64] = [
        0xd76aa478, 0xe8c7b756, 0x242070db, 0xc1bdceee, 0xf57c0faf, 0x4787c62a, 0xa8304613, 0xfd469501, 0x698098d8, 0x8b44f7af, 0xffff5bb1, 0x895cd7be, 0x6b901122, 0xfd987193, 0xa679438e, 0x49b40821,
        0xf61e2562, 0xc040b340, 0x265e5a51, 0xe9b6c7aa, 0xd62f105d, 0x02441453, 0xd8a1e681, 0xe7d3fbc8, 0x21e1cde6, 0xc33707d6, 0xf4d50d87, 0x455a14ed, 0xa9e3e905, 0xfcefa3f8, 0x676f02d9, 0x8d2a4c8a,
        0xfffa3942, 0x8771f681, 0x6d9d6122, 0xfde5380c, 0xa4beea44, 0x4bdecfa9, 0xf6bb4b60, 0xbebfbc70, 0x289b7ec6, 0xeaa127fa, 0xd4ef3085, 0x04881d05, 0xd9d4d039, 0xe6db99e5, 0x1fa27cf8, 0xc4ac5665,
        0xf4292244, 0x432aff97, 0xab9423a7, 0xfc93a039, 0x655b59c3, 0x8f0ccc92, 0xffeff47d, 0x85845dd1, 0x6fa87e4f, 0xfe2ce6e0, 0xa3014314, 0x4e0811a1, 0xf7537e82, 0xbd3af235, 0x2ad7d2bb, 0xeb86d391,
    ];
    let mut i = 0;
    while i < 64 {
        k[i] = t[i];
        i += 1;
    }
    k
};
const MD5_R: [u32; 64] = [
    7, 12, 17, 22, 7, 12, 17, 22, 7, 12, 17, 22, 7, 12, 17, 22, 5, 9, 14, 20, 5, 9, 14, 20, 5, 9, 14, 20, 5, 9, 14, 20,
    4, 11, 16, 23, 4, 11, 16, 23, 4, 11, 16, 23, 4, 11, 16, 23, 6, 10, 15, 21, 6, 10, 15, 21, 6, 10, 15, 21, 6, 10, 15, 21,
];

impl Md5 {
    pub fn new() -> Md5 {
        Md5 { s: [0x67452301, 0xefcdab89, 0x98badcfe, 0x10325476], buf: Vec::with_capacity(64), len: 0 }
    }
    pub fn update(&mut self, mut data: &[u8]) {
        self.len += data.len() as u64;
        if !self.buf.is_empty() {
            let take = (64 - self.buf.len()).min(data.len());
            self.buf.extend_from_slice(&data[..take]);
            data = &data[take..];
            if self.buf.len() == 64 {
                let b: [u8; 64] = self.buf[..].try_into().unwrap();
                self.block(&b);
                self.buf.clear();
            }
        }
        while data.len() >= 64 {
            let b: [u8; 64] = data[..64].try_into().unwrap();
            self.block(&b);
            data = &data[64..];
        }
        self.buf.extend_from_slice(data);
    }
    fn block(&mut self, b: &[u8; 64]) {
        let mut m = [0u32; 16];
        for (i, w) in m.iter_mut().enumerate() {
            *w = u32::from_le_bytes([b[i * 4], b[i * 4 + 1], b[i * 4 + 2], b[i * 4 + 3]]);
        }
        let [mut a, mut bb, mut c, mut d] = self.s;
        for i in 0..64 {
            let (f, g) = match i / 16 {
                0 => ((bb & c) | (!bb & d), i),
                1 => ((d & bb) | (!d & c), (5 * i + 1) % 16),
                2 => (bb ^ c ^ d, (3 * i + 5) % 16),
                _ => (c ^ (bb | !d), (7 * i) % 16),
            };
            let t = d;
            d = c;
            c = bb;
            bb = bb.wrapping_add(a.wrapping_add(f).wrapping_add(MD5_K[i]).wrapping_add(m[g]).rotate_left(MD5_R[i]));
            a = t;
        }
        self.s[0] = self.s[0].wrapping_add(a);
        self.s[1] = self.s[1].wrapping_add(bb);
        self.s[2] = self.s[2].wrapping_add(c);
        self.s[3] = self.s[3].wrapping_add(d);
    }
    pub fn hex(mut self) -> String {
        let bits = self.len.wrapping_mul(8);
        let mut pad = vec![0x80u8];
        while (self.buf.len() + pad.len()) % 64 != 56 {
            pad.push(0);
        }
        pad.extend_from_slice(&bits.to_le_bytes());
        self.update(&pad);
        let mut out = [0u8; 16];
        for (i, v) in self.s.iter().enumerate() {
            out[i * 4..i * 4 + 4].copy_from_slice(&v.to_le_bytes());
        }
        hex(&out)
    }
}

pub fn md5_hex(data: &[u8]) -> String {
    let mut m = Md5::new();
    m.update(data);
    m.hex()
}

/// Empreinte partielle, rapide quelle que soit la taille : MD5 de la taille,
/// du premier et du dernier Mio. Pour un dossier : identique à `md5_path`.
pub fn quick_hash(path: &Path) -> io::Result<String> {
    use std::io::{Seek, SeekFrom};
    if path.is_dir() {
        return md5_path(path);
    }
    const CHUNK: u64 = 1 << 20;
    let mut f = std::fs::File::open(path)?;
    let size = f.metadata()?.len();
    let mut m = Md5::new();
    m.update(&size.to_le_bytes());
    let mut buf = vec![0u8; CHUNK as usize];
    let n = f.read(&mut buf)?;
    m.update(&buf[..n]);
    if size > 2 * CHUNK {
        f.seek(SeekFrom::Start(size - CHUNK))?;
        let mut tail = vec![];
        f.take(CHUNK).read_to_end(&mut tail)?;
        m.update(&tail);
    } else if size > CHUNK {
        let mut rest = vec![];
        f.read_to_end(&mut rest)?;
        m.update(&rest);
    }
    Ok(m.hex())
}

/// MD5 d'un fichier ; pour un dossier (PS3, Wii U), MD5 de la liste
/// triée « chemin relatif, taille » (rapide, sans lire le contenu).
pub fn md5_path(path: &Path) -> io::Result<String> {
    if path.is_dir() {
        let mut list = vec![];
        fn walk(base: &Path, d: &Path, out: &mut Vec<String>) -> io::Result<()> {
            for e in std::fs::read_dir(d)? {
                let e = e?;
                let p = e.path();
                let m = e.metadata()?;
                if m.is_dir() {
                    walk(base, &p, out)?;
                } else {
                    out.push(format!("{}\t{}", p.strip_prefix(base).unwrap_or(&p).display(), m.len()));
                }
            }
            Ok(())
        }
        walk(path, path, &mut list)?;
        list.sort();
        return Ok(md5_hex(list.join("\n").as_bytes()));
    }
    let mut f = std::fs::File::open(path)?;
    let mut m = Md5::new();
    let mut buf = vec![0u8; 1 << 20];
    loop {
        let n = f.read(&mut buf)?;
        if n == 0 {
            break;
        }
        m.update(&buf[..n]);
    }
    Ok(m.hex())
}

pub fn hex(b: &[u8]) -> String {
    b.iter().map(|x| format!("{x:02x}")).collect()
}

pub fn unhex(s: &str) -> Option<Vec<u8>> {
    let s: String = s.chars().filter(|c| !c.is_whitespace()).collect();
    if s.len() % 2 != 0 {
        return None;
    }
    (0..s.len()).step_by(2).map(|i| u8::from_str_radix(&s[i..i + 2], 16).ok()).collect()
}

pub fn base64(data: &[u8]) -> String {
    const T: &[u8; 64] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";
    let mut out = String::new();
    for c in data.chunks(3) {
        let n = (c[0] as u32) << 16 | (*c.get(1).unwrap_or(&0) as u32) << 8 | *c.get(2).unwrap_or(&0) as u32;
        out.push(T[(n >> 18) as usize & 63] as char);
        out.push(T[(n >> 12) as usize & 63] as char);
        out.push(if c.len() > 1 { T[(n >> 6) as usize & 63] as char } else { '=' });
        out.push(if c.len() > 2 { T[n as usize & 63] as char } else { '=' });
    }
    out
}

/// Chaîne ASCII nettoyée d'un champ d'en-tête (espaces, NUL, non imprimables).
pub fn ascii_field(b: &[u8]) -> String {
    let s: String = b.iter().map(|&c| if (0x20..0x7f).contains(&c) { c as char } else { ' ' }).collect();
    s.split_whitespace().collect::<Vec<_>>().join(" ")
}

pub fn find_bytes(hay: &[u8], needle: &[u8]) -> Option<usize> {
    if needle.is_empty() || needle.len() > hay.len() {
        return None;
    }
    hay.windows(needle.len()).position(|w| w == needle)
}

/// Attend la fin d'un processus avec délai ; le tue au-delà.
pub fn wait_timeout(child: &mut Child, timeout: Duration) -> io::Result<Option<ExitStatus>> {
    let end = Instant::now() + timeout;
    loop {
        if let Some(st) = child.try_wait()? {
            return Ok(Some(st));
        }
        if Instant::now() >= end {
            let _ = child.kill();
            let _ = child.wait();
            return Ok(None);
        }
        std::thread::sleep(Duration::from_millis(20));
    }
}

/// Remplace les `{clé}` d'un gabarit. Les clés inconnues restent telles quelles.
pub fn render(template: &str, vars: &[(&str, String)]) -> String {
    let mut out = String::with_capacity(template.len());
    let mut rest = template;
    while let Some(i) = rest.find('{') {
        out.push_str(&rest[..i]);
        let after = &rest[i + 1..];
        match after.find('}') {
            Some(j) => {
                let key = &after[..j];
                match vars.iter().find(|(k, _)| *k == key) {
                    Some((_, v)) => out.push_str(v),
                    None => {
                        out.push('{');
                        out.push_str(key);
                        out.push('}');
                    }
                }
                rest = &after[j + 1..];
            }
            None => {
                out.push_str(&rest[i..]);
                rest = "";
            }
        }
    }
    out.push_str(rest);
    out
}

pub fn now_secs() -> i64 {
    std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map(|d| d.as_secs() as i64).unwrap_or(0)
}

/// Premier pourcentage présent dans une ligne (« 37% », « 37.5% ») : c'est la
/// progression chez redumper (« [ 37%] ») comme chez chdman (« 37.5% complete…
/// (ratio=40.0%) »).
pub fn last_percent(line: &str) -> Option<f64> {
    let b = line.as_bytes();
    let mut best = None;
    for (i, &c) in b.iter().enumerate() {
        if c == b'%' {
            let mut s = i;
            while s > 0 && (b[s - 1].is_ascii_digit() || b[s - 1] == b'.') {
                s -= 1;
            }
            if s < i {
                if let Ok(v) = line[s..i].parse::<f64>() {
                    if (0.0..=100.0).contains(&v) {
                        best = Some(v);
                        break;
                    }
                }
            }
        }
    }
    best
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn sha1_vectors() {
        assert_eq!(sha1_hex(b""), "da39a3ee5e6b4b0d3255bfef95601890afd80709");
        assert_eq!(sha1_hex(b"abc"), "a9993e364706816aba3e25717850c26c9cd0d89d");
        let mut s = Sha1::new();
        for _ in 0..1000 {
            s.update(b"a");
        }
        let long: Vec<u8> = vec![b'a'; 1000];
        assert_eq!(s.hex(), sha1_hex(&long));
        assert_eq!(sha1_hex(b"abcdbcdecdefdefgefghfghighijhijkijkljklmklmnlmnomnopnopq"), "84983e441c3bd26ebaae4aa1f95129e5e54670f1");
    }
    #[test]
    fn md5_vectors() {
        assert_eq!(md5_hex(b""), "d41d8cd98f00b204e9800998ecf8427e");
        assert_eq!(md5_hex(b"abc"), "900150983cd24fb0d6963f7d28e17f72");
        assert_eq!(md5_hex(b"12345678901234567890123456789012345678901234567890123456789012345678901234567890"), "57edf4a22be3c955ac49da2e2107b67a");
        let mut m = Md5::new();
        for c in b"message digest".chunks(3) {
            m.update(c);
        }
        assert_eq!(m.hex(), "f96b697d7cb7938d525a2f31aaf161d0");
    }
    #[test]
    fn misc() {
        assert_eq!(base64(b"kodi:pass"), "a29kaTpwYXNz");
        assert_eq!(render("a{x}b{y}{z}", &[("x", "1".into()), ("y", "2".into())]), "a1b2{z}");
        assert_eq!(last_percent("Compressing, 37.5% complete... (ratio=40.0%)"), Some(37.5));
        assert_eq!(last_percent("[ 12%] LBA: 1000"), Some(12.0));
        assert_eq!(ascii_field(b"  SEGA\0\0 SATURN  "), "SEGA SATURN");
    }
}
