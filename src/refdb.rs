//! Base de référence locale construite à partir des fichiers DAT : Redump
//! (format Logiqx XML) et métadonnées libretro (format clrmamepro, avec les
//! numéros de série). Permet de prédire le nom canonique **avant** le dump
//! (tailles des pistes) et de le vérifier **après** (SHA-1).
//!
//! Stockage : `~/.local/share/disc-launcher/reference.tsv`, une ligne par jeu :
//! `système \t nom \t série \t taille totale \t tailles des pistes (,) \t sha1 (,) \t noms des fichiers (|)`.

use crate::device::{MediaKind, Physical};
use crate::identity::Identity;
use crate::xml::{self, Token};
use std::path::{Path, PathBuf};

#[derive(Clone, Debug, PartialEq)]
pub struct RefGame {
    pub system: String,
    pub name: String,
    pub serial: Option<String>,
    pub total: u64,
    pub sizes: Vec<u64>,
    pub sha1: Vec<String>,
    pub files: Vec<String>,
}

#[derive(Default)]
pub struct RefDb {
    pub games: Vec<RefGame>,
}

pub fn db_path() -> PathBuf {
    crate::paths::user_data_dir().join("reference.tsv")
}

/// Nom de système d'un en-tête DAT → identifiant ES-DE (famille).
pub fn system_from_dat_name(name: &str) -> Option<&'static str> {
    let n = name.to_ascii_lowercase();
    let table: &[(&str, &str)] = &[
        ("sony - playstation 3", "ps3"),
        ("sony - playstation 2", "ps2"),
        ("sony - playstation portable", "psp"),
        ("sony - playstation", "psx"),
        ("sega - saturn", "saturn"),
        ("sega - mega cd", "megacd"),
        ("sega - mega-cd", "megacd"),
        ("sega - sega cd", "megacd"),
        ("sega - dreamcast", "dreamcast"),
        ("sega - naomi", "naomigd"),
        ("nec - pc engine cd", "pcenginecd"),
        ("nec - turbografx cd", "pcenginecd"),
        ("nec - pc-fx", "pcfx"),
        ("nec - pc-98", "pc98"),
        ("snk - neo geo cd", "neogeocd"),
        ("panasonic - 3do", "3do"),
        ("philips - cd-i", "cdimono1"),
        ("commodore - amiga cd32", "amigacd32"),
        ("commodore - amiga cdtv", "cdtv"),
        ("commodore - cd32", "amigacd32"),
        ("commodore - cdtv", "cdtv"),
        ("the 3do company", "3do"),
        ("atari - jaguar cd", "atarijaguarcd"),
        ("nintendo - gamecube", "gc"),
        ("nintendo - wii u", "wiiu"),
        ("nintendo - wii", "wii"),
        ("microsoft - xbox 360", "xbox360"),
        ("microsoft - xbox", "xbox"),
        ("fujitsu - fm-towns", "fmtowns"),
        ("fujitsu - fm towns", "fmtowns"),
        ("ibm - pc compatible", "windows"),
        ("nintendo - nintendo 64dd", "n64dd"),
        ("nintendo - nintendo 64", "n64"),
        ("nintendo - super nintendo", "snes"),
        ("sega - mega drive", "megadrive"),
        ("sega - genesis", "megadrive"),
        ("nintendo - game boy advance", "gba"),
        ("nintendo - game boy color", "gbc"),
        ("nintendo - game boy", "gb"),
        ("sega - master system", "mastersystem"),
        ("sega - game gear", "gamegear"),
    ];
    table.iter().find(|(k, _)| n.starts_with(k)).map(|(_, v)| *v)
}

/// Fichiers de métadonnées libretro (`metadat/<source>/<nom>.dat`) par système :
/// noms Redump (disques : numéros de série et piste de données de chaque jeu)
/// ou No-Intro (cartouches : empreintes de la ROM).
pub const LIBRETRO_DATS: &[(&str, &str)] = &[
    // cartouches (No-Intro)
    ("n64", "no-intro/Nintendo - Nintendo 64"),
    ("snes", "no-intro/Nintendo - Super Nintendo Entertainment System"),
    ("megadrive", "no-intro/Sega - Mega Drive - Genesis"),
    ("gb", "no-intro/Nintendo - Game Boy"),
    ("gbc", "no-intro/Nintendo - Game Boy Color"),
    ("gba", "no-intro/Nintendo - Game Boy Advance"),
    ("mastersystem", "no-intro/Sega - Master System - Mark III"),
    ("gamegear", "no-intro/Sega - Game Gear"),
    // disques (Redump)
    ("psx", "redump/Sony - PlayStation"),
    ("ps2", "redump/Sony - PlayStation 2"),
    ("ps3", "redump/Sony - PlayStation 3"),
    ("saturn", "redump/Sega - Saturn"),
    ("megacd", "redump/Sega - Mega-CD - Sega CD"),
    ("dreamcast", "redump/Sega - Dreamcast"),
    ("naomigd", "redump/Sega - Naomi"),
    ("pcenginecd", "redump/NEC - PC Engine CD - TurboGrafx-CD"),
    ("pcfx", "redump/NEC - PC-FX"),
    ("pc98", "redump/NEC - PC-98"),
    ("neogeocd", "redump/SNK - Neo Geo CD"),
    ("3do", "redump/The 3DO Company - 3DO"),
    ("cdimono1", "redump/Philips - CD-i"),
    ("amigacd32", "redump/Commodore - CD32"),
    ("cdtv", "redump/Commodore - CDTV"),
    ("atarijaguarcd", "redump/Atari - Jaguar CD"),
    ("gc", "redump/Nintendo - GameCube"),
    ("wii", "redump/Nintendo - Wii"),
    ("xbox", "redump/Microsoft - Xbox"),
    ("xbox360", "redump/Microsoft - Xbox 360"),
];

/// `dat_path` : « redump/Sony - PlayStation » ou « no-intro/Nintendo - Nintendo 64 ».
pub fn libretro_url(dat_path: &str) -> String {
    format!("https://raw.githubusercontent.com/libretro/libretro-database/master/metadat/{}.dat", dat_path.replace(' ', "%20"))
}

/// Jetons du format clrmamepro : mots, chaînes entre guillemets, parenthèses.
fn cmp_tokens(text: &str) -> Vec<String> {
    let mut out = vec![];
    let mut it = text.chars().peekable();
    while let Some(&c) = it.peek() {
        if c.is_whitespace() {
            it.next();
        } else if c == '(' || c == ')' {
            out.push(c.to_string());
            it.next();
        } else if c == '"' {
            it.next();
            let mut s = String::new();
            while let Some(ch) = it.next() {
                match ch {
                    '"' => break,
                    '\\' => {
                        if let Some(n) = it.next() {
                            s.push(n);
                        }
                    }
                    _ => s.push(ch),
                }
            }
            out.push(format!("\u{0}{s}"));
        } else {
            let mut s = String::new();
            while let Some(&ch) = it.peek() {
                if ch.is_whitespace() || ch == '(' || ch == ')' {
                    break;
                }
                s.push(ch);
                it.next();
            }
            out.push(s);
        }
    }
    out
}

/// Format clrmamepro (`clrmamepro ( … ) game ( name "…" serial "…" rom ( … ) )`).
fn parse_clrmamepro(text: &str, system: Option<&str>) -> Result<Vec<RefGame>, String> {
    let toks = cmp_tokens(text);
    let unq = |t: &str| t.strip_prefix('\u{0}').unwrap_or(t).to_string();
    let mut sys: Option<String> = system.map(|s| s.to_string());
    let mut out = vec![];
    let mut i = 0;
    // Lit un bloc « ( clé valeur … ) » à partir de i (sur la parenthèse ouvrante).
    fn block(toks: &[String], mut i: usize) -> (Vec<(String, Option<usize>, String)>, usize) {
        // (clé, début d'un sous-bloc, valeur simple)
        let mut items = vec![];
        if toks.get(i).map(|t| t.as_str()) != Some("(") {
            return (items, i);
        }
        i += 1;
        while i < toks.len() && toks[i] != ")" {
            let key = toks[i].clone();
            i += 1;
            if toks.get(i).map(|t| t.as_str()) == Some("(") {
                let start = i;
                let mut depth = 0;
                while i < toks.len() {
                    if toks[i] == "(" {
                        depth += 1;
                    } else if toks[i] == ")" {
                        depth -= 1;
                        if depth == 0 {
                            i += 1;
                            break;
                        }
                    }
                    i += 1;
                }
                items.push((key, Some(start), String::new()));
            } else if i < toks.len() {
                items.push((key, None, toks[i].clone()));
                i += 1;
            }
        }
        (items, i + 1)
    }
    while i < toks.len() {
        let head = toks[i].clone();
        i += 1;
        let (items, next) = block(&toks, i);
        i = next;
        match head.as_str() {
            "clrmamepro" => {
                if sys.is_none() {
                    if let Some((_, _, v)) = items.iter().find(|x| x.0 == "name") {
                        sys = system_from_dat_name(&unq(v)).map(|s| s.to_string());
                    }
                }
            }
            "game" | "machine" => {
                let mut g = RefGame { system: String::new(), name: String::new(), serial: None, total: 0, sizes: vec![], sha1: vec![], files: vec![] };
                for (k, sub, v) in &items {
                    match (k.as_str(), sub) {
                        ("name", None) => g.name = unq(v),
                        ("serial", None) => g.serial = Some(unq(v).split(',').next().unwrap_or("").trim().to_string()).filter(|s| !s.is_empty()),
                        ("rom", Some(start)) => {
                            let (rom, _) = block(&toks, *start);
                            let get = |key: &str| rom.iter().find(|x| x.0 == key).map(|x| unq(&x.2));
                            let fname = get("name").unwrap_or_default();
                            let lower = fname.to_ascii_lowercase();
                            if lower.ends_with(".cue") || lower.ends_with(".gdi") {
                                continue;
                            }
                            let size: u64 = get("size").and_then(|x| x.parse().ok()).unwrap_or(0);
                            g.total += size;
                            g.sizes.push(size);
                            g.sha1.push(get("sha1").unwrap_or_default().to_ascii_lowercase());
                            g.files.push(fname);
                            if g.serial.is_none() {
                                g.serial = get("serial").filter(|s| !s.is_empty());
                            }
                        }
                        _ => {}
                    }
                }
                if !g.name.is_empty() {
                    out.push(g);
                }
            }
            _ => {}
        }
    }
    let Some(s) = sys else { return Err("système inconnu : utilisez --system".into()) };
    for g in out.iter_mut() {
        g.system = s.clone();
    }
    Ok(out)
}

/// Analyse un DAT (Logiqx XML ou clrmamepro). `system` force le système si
/// l'en-tête n'est pas reconnu.
pub fn parse_dat(text: &str, system: Option<&str>) -> Result<Vec<RefGame>, String> {
    if !text.trim_start().starts_with('<') {
        return parse_clrmamepro(text, system);
    }
    let toks = xml::tokenize(text);
    let mut sys: Option<String> = system.map(|s| s.to_string());
    let mut out = vec![];
    let mut cur: Option<RefGame> = None;
    let mut path: Vec<String> = vec![];
    for t in toks {
        match t {
            Token::Open { name, attrs, self_closing } => {
                match name.as_str() {
                    "game" | "machine" => {
                        cur = Some(RefGame { system: String::new(), name: xml::attr(&attrs, "name").unwrap_or("").to_string(), serial: xml::attr(&attrs, "serial").map(|s| s.to_string()), total: 0, sizes: vec![], sha1: vec![], files: vec![] });
                    }
                    "rom" => {
                        if let Some(g) = cur.as_mut() {
                            let fname = xml::attr(&attrs, "name").unwrap_or("").to_string();
                            if fname.to_ascii_lowercase().ends_with(".cue") || fname.to_ascii_lowercase().ends_with(".gdi") {
                                // les fichiers de description ne comptent pas dans les tailles
                            } else {
                                let size: u64 = xml::attr(&attrs, "size").and_then(|s| s.parse().ok()).unwrap_or(0);
                                g.total += size;
                                g.sizes.push(size);
                                g.sha1.push(xml::attr(&attrs, "sha1").unwrap_or("").to_ascii_lowercase());
                                g.files.push(fname);
                            }
                            if g.serial.is_none() {
                                g.serial = xml::attr(&attrs, "serial").map(|s| s.to_string());
                            }
                        }
                    }
                    _ => {}
                }
                if !self_closing {
                    path.push(name);
                }
            }
            Token::Close(name) => {
                if (name == "game" || name == "machine") && cur.is_some() {
                    let mut g = cur.take().unwrap();
                    g.system = sys.clone().unwrap_or_default();
                    if !g.name.is_empty() && !g.sizes.is_empty() {
                        out.push(g);
                    }
                }
                if let Some(i) = path.iter().rposition(|p| *p == name) {
                    path.truncate(i);
                }
            }
            Token::Text(t) => {
                let last = path.last().map(|s| s.as_str()).unwrap_or("");
                if last == "name" && path.iter().any(|p| p == "header") && sys.is_none() {
                    sys = system_from_dat_name(&t).map(|s| s.to_string());
                }
                if last == "serial" {
                    if let Some(g) = cur.as_mut() {
                        g.serial = Some(t.trim().split(',').next().unwrap_or("").trim().to_string());
                    }
                }
            }
        }
    }
    if sys.is_none() {
        return Err("système inconnu : utilisez --system".into());
    }
    let s = sys.unwrap();
    for g in out.iter_mut() {
        g.system = s.clone();
    }
    Ok(out)
}

impl RefDb {
    pub fn load() -> RefDb {
        Self::load_from(&db_path())
    }

    pub fn load_from(p: &Path) -> RefDb {
        let mut db = RefDb::default();
        let Ok(text) = std::fs::read_to_string(p) else { return db };
        for line in text.lines() {
            let f: Vec<&str> = line.split('\t').collect();
            if f.len() < 7 {
                continue;
            }
            db.games.push(RefGame {
                system: f[0].into(),
                name: f[1].into(),
                serial: Some(f[2].to_string()).filter(|s| !s.is_empty()),
                total: f[3].parse().unwrap_or(0),
                sizes: f[4].split(',').filter_map(|x| x.parse().ok()).collect(),
                sha1: f[5].split(',').map(|s| s.to_string()).collect(),
                files: f[6].split('|').map(|s| s.to_string()).collect(),
            });
        }
        db
    }

    pub fn save_to(&self, p: &Path) -> std::io::Result<()> {
        let mut s = String::new();
        for g in &self.games {
            let clean = |x: &str| x.replace(['\t', '\n'], " ");
            s.push_str(&format!(
                "{}\t{}\t{}\t{}\t{}\t{}\t{}\n",
                g.system,
                clean(&g.name),
                clean(g.serial.as_deref().unwrap_or("")),
                g.total,
                g.sizes.iter().map(|x| x.to_string()).collect::<Vec<_>>().join(","),
                g.sha1.join(","),
                g.files.iter().map(|f| clean(f)).collect::<Vec<_>>().join("|")
            ));
        }
        crate::paths::write_atomic(p, s.as_bytes())
    }

    /// Fusionne `games` dans la base, jeu par jeu (même système, même nom) :
    /// un DAT Redump complet (toutes les pistes) et les métadonnées libretro
    /// (numéros de série, piste de données seule) se complètent. La liste de
    /// pistes la plus longue est gardée ; le numéro de série est ajouté s'il manquait.
    pub fn merge(&mut self, games: Vec<RefGame>) {
        let mut index: std::collections::HashMap<(String, String), Vec<usize>> = std::collections::HashMap::new();
        for (i, g) in self.games.iter().enumerate() {
            index.entry((g.system.clone(), g.name.clone())).or_default().push(i);
        }
        for g in games {
            let key = (g.system.clone(), g.name.clone());
            // Même jeu : même nom, et même numéro de série (ou l'un des deux inconnu).
            let same = index.get(&key).and_then(|v| v.iter().copied().find(|&i| self.games[i].serial.is_none() || g.serial.is_none() || self.games[i].serial == g.serial));
            match same {
                Some(i) => {
                    let cur = &mut self.games[i];
                    if g.sizes.len() >= cur.sizes.len() {
                        let serial = cur.serial.take();
                        *cur = RefGame { serial: g.serial.clone().or(serial), ..g };
                    } else if cur.serial.is_none() {
                        cur.serial = g.serial;
                    }
                }
                None => {
                    index.entry(key).or_default().push(self.games.len());
                    self.games.push(g);
                }
            }
        }
    }

    /// Jeux d'un système portant ce numéro de série (comparaison sans tirets ni casse).
    pub fn by_serial(&self, system: &str, serial: &str) -> Vec<&RefGame> {
        let norm = |s: &str| s.chars().filter(|c| c.is_ascii_alphanumeric()).collect::<String>().to_ascii_uppercase();
        let want = norm(serial);
        // GameCube et Wii : l'identifiant lu sur le disque (« GALE01 ») commence par
        // le code de 4 caractères qui figure dans la référence (« DL-DOL-GALE-USA »).
        let nintendo = matches!(system, "gc" | "wii" | "n64" | "gba") && want.len() >= 4;
        let code = if nintendo { want[..4].to_string() } else { String::new() };
        self.games
            .iter()
            .filter(|g| g.system == system)
            .filter(|g| {
                g.serial.as_deref().map_or(false, |s| {
                    // Plusieurs numéros possibles : « SLUS-00001, SLUS-00002 ».
                    s.split(',').any(|one| norm(one) == want || (nintendo && one.split('-').any(|seg| seg.trim().eq_ignore_ascii_case(&code))))
                })
            })
            .collect()
    }

    /// Jeux dont la seule piste connue (bases partielles comme libretro) a la
    /// taille de la première piste du disque, à 3 s près.
    fn by_first_track(&self, system: &str, phys: &Physical) -> Vec<&RefGame> {
        let Some(t) = phys.tracks.first() else { return vec![] };
        if phys.media != MediaKind::Cd || phys.tracks.len() < 2 {
            return vec![];
        }
        let len = t.length as i64 * 2352;
        self.games.iter().filter(|g| g.system == system && g.sizes.len() == 1 && (g.sizes[0] as i64 - len).unsigned_abs() <= 225 * 2352).collect()
    }

    /// Prédiction avant dump. Renvoie (nom, exact ?) ou None.
    pub fn predict(&self, id: &Identity, phys: &Physical) -> Option<(String, bool)> {
        // Contenu déjà haché (cartouche) : correspondance exacte.
        if let Some(h) = &id.sha1 {
            if let Some(g) = self.by_sha1(&id.system, std::slice::from_ref(h)) {
                return Some((g.name.clone(), true));
            }
        }
        // Cartouche non dumpée : seul le titre (nom de fichier de la Retrode) est connu.
        if phys.media == MediaKind::Cart {
            return self.name_for_title(&id.system, id.title.as_deref()?, phys.capacity * 2048).map(|n| (n, false));
        }
        let cands: Vec<&RefGame> = self.games.iter().filter(|g| g.system == id.system).collect();
        if cands.is_empty() {
            return None;
        }
        let mut hits: Vec<&RefGame> = match phys.media {
            MediaKind::Cd => {
                let tracks = phys.tracks.len();
                let total = phys.leadout as u64 * 2352;
                // Multisession : les écarts entre sessions ne sont pas dans les fichiers.
                let gap = (phys.sessions.saturating_sub(1)) as u64 * 11400 * 2352;
                cands
                    .into_iter()
                    .filter(|g| g.sizes.len() == tracks && (g.total == total || g.total + gap == total))
                    .filter(|g| {
                        // tolérance de 3 s (225 secteurs) par piste pour les prégaps
                        g.sizes.iter().zip(phys.tracks.iter()).all(|(s, t)| (*s as i64 - t.length as i64 * 2352).unsigned_abs() <= 225 * 2352 + gap)
                    })
                    .collect()
            }
            _ => {
                let size = phys.capacity * 2048;
                cands.into_iter().filter(|g| g.sizes.len() == 1 && g.total == size).collect()
            }
        };
        if let Some(serial) = &id.serial {
            let with: Vec<&RefGame> = hits.iter().copied().filter(|g| g.serial.as_ref().map_or(false, |s| s.eq_ignore_ascii_case(serial))).collect();
            if !with.is_empty() {
                hits = with;
            }
        }
        if hits.len() > 1 {
            if let Some(r) = &id.region {
                let with: Vec<&RefGame> = hits.iter().copied().filter(|g| g.name.contains(&format!("({r}")) || g.name.contains(&format!(", {r}"))).collect();
                if !with.is_empty() {
                    hits = with;
                }
            }
            if let Some(d) = id.disc {
                let with: Vec<&RefGame> = hits.iter().copied().filter(|g| disc_of(&g.name) == Some(d) || (d == 1 && disc_of(&g.name).is_none())).collect();
                if !with.is_empty() {
                    hits = with;
                }
            }
        }
        // Doublons d'un même jeu (plusieurs numéros de série pour un nom).
        hits.dedup_by(|a, b| a.name == b.name);
        match hits.len() {
            0 => {}
            1 => return Some((hits[0].name.clone(), true)),
            _ => return Some((hits[0].name.clone(), false)),
        }
        // Pas de correspondance par tailles (base sans toutes les pistes, comme
        // les métadonnées libretro) : numéro de série, sinon taille de la première
        // piste si elle désigne un seul jeu. Nom probable seulement : la
        // vérification après dump le confirmera.
        if let Some(n) = self.name_for_serial(id) {
            return Some((n, false));
        }
        let mut by = self.by_first_track(&id.system, phys);
        by.dedup_by(|a, b| a.name == b.name);
        if by.len() == 1 {
            return Some((by[0].name.clone(), false));
        }
        None
    }

    /// Nombre de jeux par système.
    pub fn counts(&self) -> std::collections::BTreeMap<String, (usize, usize)> {
        let mut m: std::collections::BTreeMap<String, (usize, usize)> = Default::default();
        for g in &self.games {
            let e = m.entry(g.system.clone()).or_default();
            e.0 += 1;
            if g.serial.is_some() {
                e.1 += 1;
            }
        }
        m
    }

    /// Nom d'après le numéro de série (ou l'identifiant de jeu), départagé par
    /// la région puis le numéro de disque.
    pub fn name_for_serial(&self, id: &Identity) -> Option<String> {
        let serial = id.serial.as_deref().or(id.game_id.as_deref())?;
        let mut by: Vec<&RefGame> = self.by_serial(&id.system, serial);
        if by.len() > 1 {
            if let Some(r) = &id.region {
                let with: Vec<&RefGame> = by.iter().copied().filter(|g| g.name.contains(&format!("({r}")) || g.name.contains(&format!(", {r}"))).collect();
                if !with.is_empty() {
                    by = with;
                }
            }
            if let Some(d) = id.disc {
                let with: Vec<&RefGame> = by.iter().copied().filter(|g| disc_of(&g.name) == Some(d) || (d == 1 && disc_of(&g.name).is_none())).collect();
                if !with.is_empty() {
                    by = with;
                }
            }
            // Révision lue dans l'en-tête (cartouches : « Rev 1 ») ; sans révision,
            // la version d'origine (nom sans « (Rev … ) »).
            let rev = id.revision.as_deref().filter(|r| r.starts_with("Rev "));
            let with: Vec<&RefGame> = match rev {
                Some(r) => by.iter().copied().filter(|g| g.name.contains(&format!("({r})"))).collect(),
                None => by.iter().copied().filter(|g| !g.name.contains("(Rev ")).collect(),
            };
            if !with.is_empty() {
                by = with;
            }
        }
        by.first().map(|g| g.name.clone())
    }

    /// Nom d'après le titre seul (lettres et chiffres comparés, mentions entre
    /// parenthèses ignorées). Départage : même taille, version d'origine, puis
    /// région dans l'ordre Europe, World, USA, Japan.
    pub fn name_for_title(&self, system: &str, title: &str, size: u64) -> Option<String> {
        let want = title_key(title);
        if want.len() < 3 {
            return None;
        }
        let mut c: Vec<&RefGame> = match_title(self.games.iter().filter(|g| g.system == system), &want);
        let same_size: Vec<&RefGame> = c.iter().copied().filter(|g| g.total == size).collect();
        if !same_size.is_empty() {
            c = same_size;
        }
        let rank = |g: &RefGame| -> (usize, usize) {
            let n = &g.name;
            let rev = usize::from(n.contains("(Rev ") || n.contains("(Beta") || n.contains("(Proto"));
            let region = ["(Europe", "(World", "(USA", "(Japan"].iter().position(|r| n.contains(r)).unwrap_or(4);
            (rev, region)
        };
        c.sort_by_key(|g| rank(g));
        c.first().map(|g| g.name.clone())
    }

    /// Tous les noms connus pour ce titre (toutes versions et régions).
    pub fn names_for_title(&self, system: &str, title: &str) -> Vec<String> {
        let mut v: Vec<String> = match_title(self.games.iter().filter(|g| g.system == system), &title_key(title)).into_iter().map(|g| g.name.clone()).collect();
        v.sort();
        v.dedup();
        v
    }

    /// Tous les noms connus pour ce numéro de série (toutes versions et régions).
    pub fn names_for_serial(&self, system: &str, serial: &str) -> Vec<String> {
        let mut v: Vec<String> = self.by_serial(system, serial).into_iter().map(|g| g.name.clone()).collect();
        v.dedup();
        v
    }

    /// Vérification après dump : jeu dont tous les SHA-1 correspondent.
    pub fn by_sha1(&self, system: &str, hashes: &[String]) -> Option<&RefGame> {
        if hashes.is_empty() {
            return None;
        }
        let hashes: Vec<String> = hashes.iter().map(|h| h.to_ascii_lowercase()).collect();
        let ok = |g: &&RefGame| (system.is_empty() || g.system == system) && !g.sha1.is_empty() && g.sha1.iter().all(|x| !x.is_empty());
        // Toutes les pistes connues…
        self.games
            .iter()
            .filter(ok)
            .find(|g| g.sha1.len() == hashes.len() && hashes.iter().all(|h| g.sha1.contains(h)))
            // …ou une base partielle (piste de données seule) dont chaque empreinte est présente.
            .or_else(|| self.games.iter().filter(ok).find(|g| g.sha1.len() < hashes.len() && g.sha1.iter().all(|x| hashes.contains(x))))
    }

    /// Nombre de disques d'un jeu (d'après les noms « (Disc N) » de même base).
    pub fn disc_count(&self, system: &str, game_base: &str) -> Option<u32> {
        let n = self.games.iter().filter(|g| g.system == system && game_name(&g.name) == game_base && disc_of(&g.name).is_some()).count();
        if n > 0 {
            Some(n as u32)
        } else {
            None
        }
    }
}

/// Titre réduit à ses lettres et chiffres, sans les mentions entre
/// parenthèses ou crochets : « Mario Kart 64 (Europe) (Rev 1) » → « mariokart64 ».
/// L'article placé en fin de titre par No-Intro et Redump revient en tête :
/// « Legend of Zelda, The - Ocarina of Time » → « thelegendofzeldaocarinaoftime ».
pub fn title_key(name: &str) -> String {
    let mut plain = String::new();
    let mut depth = 0i32;
    for c in name.chars() {
        match c {
            '(' | '[' => depth += 1,
            ')' | ']' => depth -= 1,
            _ if depth <= 0 => plain.push(c),
            _ => {}
        }
    }
    let (first, rest) = match plain.split_once(" - ") {
        Some((a, b)) => (a.to_string(), format!(" {b}")),
        None => (plain.clone(), String::new()),
    };
    let mut first = first.trim().to_string();
    for art in ["The", "A", "An", "Les", "Le", "La", "L'", "Der", "Die", "Das", "El", "Il"] {
        if let Some(base) = first.strip_suffix(&format!(", {art}")) {
            first = format!("{art} {base}");
            break;
        }
    }
    format!("{first}{rest}").chars().filter(|c| c.is_alphanumeric()).flat_map(|c| c.to_lowercase()).collect()
}

/// Jeux dont le titre correspond : identique, sinon commençant par `want` (titre
/// tronqué d'un en-tête de cartouche) si un seul titre distinct correspond.
pub fn match_title<'a>(games: impl Iterator<Item = &'a RefGame>, want: &str) -> Vec<&'a RefGame> {
    let all: Vec<&RefGame> = games.collect();
    let exact: Vec<&RefGame> = all.iter().copied().filter(|g| title_key(&g.name) == want).collect();
    if !exact.is_empty() || want.len() < 8 {
        return exact;
    }
    let pre: Vec<&RefGame> = all.iter().copied().filter(|g| title_key(&g.name).starts_with(want)).collect();
    let mut keys: Vec<String> = pre.iter().map(|g| title_key(&g.name)).collect();
    keys.sort();
    keys.dedup();
    if keys.len() == 1 {
        pre
    } else {
        vec![]
    }
}

/// Numéro de disque d'un nom Redump : « … (Disc 2) » → 2.
pub fn disc_of(name: &str) -> Option<u32> {
    let i = name.find("(Disc ")?;
    let rest = &name[i + 6..];
    let num: String = rest.chars().take_while(|c| c.is_ascii_digit()).collect();
    num.parse().ok()
}

/// Nom du jeu sans le suffixe de disque : « Final Fantasy VII (France) (Disc 1) » → « Final Fantasy VII (France) ».
pub fn game_name(name: &str) -> String {
    match name.find(" (Disc ") {
        Some(i) => {
            let after = &name[i + 1..];
            let end = after.find(')').map(|j| i + 1 + j + 1).unwrap_or(name.len());
            format!("{}{}", &name[..i], &name[end..]).trim().to_string()
        }
        None => name.to_string(),
    }
}

/// Importe un ou plusieurs DAT (fichiers ou dossier) dans la base.
pub fn import(paths: &[PathBuf], system: Option<&str>) -> Result<(usize, Vec<String>), String> {
    let mut files = vec![];
    for p in paths {
        if p.is_dir() {
            if let Ok(it) = std::fs::read_dir(p) {
                for e in it.flatten() {
                    let ep = e.path();
                    if matches!(ep.extension().and_then(|x| x.to_str()).map(|x| x.to_ascii_lowercase()).as_deref(), Some("dat") | Some("xml")) {
                        files.push(ep);
                    }
                }
            }
        } else {
            files.push(p.clone());
        }
    }
    files.sort();
    let mut db = RefDb::load();
    let mut total = 0;
    let mut msgs = vec![];
    for f in files {
        let text = std::fs::read_to_string(&f).map_err(|e| format!("{} : {e}", f.display()))?;
        match parse_dat(&text, system) {
            Ok(g) => {
                msgs.push(format!("{} : {} jeux ({})", f.display(), g.len(), g.first().map(|x| x.system.clone()).unwrap_or_default()));
                total += g.len();
                db.merge(g);
            }
            Err(e) => msgs.push(format!("{} ignoré : {e}", f.display())),
        }
    }
    db.save_to(&db_path()).map_err(|e| e.to_string())?;
    Ok((total, msgs))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::device::Track;

    const DAT: &str = r#"<?xml version="1.0"?>
<datafile><header><name>Sony - PlayStation</name></header>
<game name="Final Fantasy VII (France) (Disc 1)"><category>Games</category>
 <rom name="Final Fantasy VII (France) (Disc 1).cue" size="99" sha1="00"/>
 <rom name="Final Fantasy VII (France) (Disc 1).bin" size="696714144" sha1="AAAA"/>
</game>
<game name="Final Fantasy VII (France) (Disc 2)">
 <rom name="Final Fantasy VII (France) (Disc 2).bin" size="700000000" sha1="BBBB"/>
</game>
<game name="Wipeout (Europe)">
 <rom name="Wipeout (Europe) (Track 1).bin" size="23520000" sha1="c1"/>
 <rom name="Wipeout (Europe) (Track 2).bin" size="47040000" sha1="c2"/>
</game>
</datafile>"#;

    #[test]
    fn dat_and_predict() {
        let games = parse_dat(DAT, None).unwrap();
        assert_eq!(games.len(), 3);
        assert_eq!(games[0].system, "psx");
        let db = RefDb { games };
        let phys = |tracks: Vec<Track>| {
            let leadout = tracks.iter().map(|t| t.start + t.length).max().unwrap();
            Physical { media: MediaKind::Cd, profile: 8, recordable: false, blank: false, sessions: 1, tracks, leadout, disc_type: None, capacity: leadout as u64 }
        };
        let id = Identity { system: "psx".into(), serial: Some("SCES-00867".into()), disc: Some(1), ..Default::default() };
        let p = phys(vec![Track { number: 1, session: 1, data: true, start: 0, length: 696714144 / 2352, mode: Some(2) }]);
        assert_eq!(db.predict(&id, &p), Some(("Final Fantasy VII (France) (Disc 1)".into(), true)));
        // Pistes : la 2e commence après un prégap de 150 secteurs compté dans la piste 1 de la TOC.
        let p2 = phys(vec![
            Track { number: 1, session: 1, data: true, start: 0, length: 10000 + 150, mode: Some(2) },
            Track { number: 2, session: 1, data: false, start: 10150, length: 20000 - 150, mode: None },
        ]);
        assert_eq!(db.predict(&Identity { system: "psx".into(), ..Default::default() }, &p2), Some(("Wipeout (Europe)".into(), true)));
        assert_eq!(db.by_sha1("psx", &["c2".into(), "C1".into()]).map(|g| g.name.as_str()), Some("Wipeout (Europe)"));
        assert_eq!(db.disc_count("psx", "Final Fantasy VII (France)"), Some(2));
    }

    #[test]
    fn names() {
        assert_eq!(disc_of("X (USA) (Disc 2)"), Some(2));
        assert_eq!(game_name("X (USA) (Disc 2)"), "X (USA)");
        assert_eq!(game_name("X (USA) (Disc 2) (Rev 1)"), "X (USA) (Rev 1)");
        assert_eq!(system_from_dat_name("Sony - PlayStation 2 - Datfile (12)"), Some("ps2"));
    }

    const CMP: &str = r#"clrmamepro (
	name "Sony - PlayStation"
	version "2026.08.01"
)

game (
	name "Rayman 2 - The Great Escape (Europe) (Fr,De)"
	region "Europe"
	serial "SLES-02905"
	rom ( name "Rayman 2 - The Great Escape (Europe) (Fr,De) (Track 1).bin" size 699734112 crc A6F35937 sha1 BF9899DBB39513954B1B916F3FD7CB6FB73E60CD serial "SLES-02905" )
)
game (
	name "Rayman 2 - The Great Escape (USA) (En,Fr,Es)"
	serial "SLUS-01235"
	rom ( name "Rayman 2 (USA) (Track 1).bin" size 699734112 sha1 4E10783D593C2C54F59DD03027253222D16AE2F1 )
)
"#;

    #[test]
    fn clrmamepro_serials() {
        let g = parse_dat(CMP, None).unwrap();
        assert_eq!(g.len(), 2);
        assert_eq!(g[0].system, "psx");
        assert_eq!(g[0].serial.as_deref(), Some("SLES-02905"));
        assert_eq!(g[0].sizes, vec![699734112]);
        let mut db = RefDb::default();
        db.merge(g);
        // Redump XML (toutes les pistes, sans série) puis libretro : fusion.
        let xml = r#"<datafile><header><name>Sony - PlayStation</name></header>
<game name="Rayman 2 - The Great Escape (Europe) (Fr,De)"><rom name="a (Track 1).bin" size="699734112" sha1="bf9899dbb39513954b1b916f3fd7cb6fb73e60cd"/><rom name="a (Track 2).bin" size="32340000" sha1="22"/></game></datafile>"#;
        db.merge(parse_dat(xml, None).unwrap());
        assert_eq!(db.games.len(), 2);
        assert_eq!(db.games[0].sizes.len(), 2);
        assert_eq!(db.games[0].serial.as_deref(), Some("SLES-02905"));
        let id = Identity { system: "psx".into(), serial: Some("SLES-02905".into()), region: Some("Europe".into()), disc: Some(1), ..Default::default() };
        assert_eq!(db.name_for_serial(&id).as_deref(), Some("Rayman 2 - The Great Escape (Europe) (Fr,De)"));
        // Vérification partielle : la piste de données seule suffit à une base libretro.
        let mut lib = RefDb::default();
        lib.merge(parse_dat(CMP, None).unwrap());
        assert!(lib.by_sha1("psx", &["BF9899DBB39513954B1B916F3FD7CB6FB73E60CD".into(), "ffff".into()]).is_some());
        assert!(lib.by_sha1("psx", &["0000".into(), "ffff".into()]).is_none());
    }

    #[test]
    fn nintendo_codes_and_first_track() {
        let cmp = r#"clrmamepro ( name "Nintendo - GameCube" )
game ( name "Super Smash Bros. Melee (Europe) (En,De,Fr,It,Es)" serial "DL-DOL-GALP-EUR" rom ( name "a.iso" size 1459978240 sha1 AA ) )
game ( name "Super Smash Bros. Melee (USA) (En,Ja)" serial "DL-DOL-GALE-USA" rom ( name "b.iso" size 1459978240 sha1 BB ) )"#;
        let mut db = RefDb::default();
        db.merge(parse_dat(cmp, None).unwrap());
        let id = Identity { system: "gc".into(), game_id: Some("GALE01".into()), ..Default::default() };
        assert_eq!(db.name_for_serial(&id).as_deref(), Some("Super Smash Bros. Melee (USA) (En,Ja)"));

        let pce = r#"clrmamepro ( name "NEC - PC Engine CD - TurboGrafx-CD" )
game ( name "Ys I & II (Japan)" rom ( name "Ys (Track 01).bin" size 3528000 sha1 C1 ) )
game ( name "Autre (Japan)" rom ( name "x (Track 01).bin" size 7056000 sha1 C2 ) )"#;
        let mut db = RefDb::default();
        db.merge(parse_dat(pce, None).unwrap());
        let phys = Physical {
            media: MediaKind::Cd, profile: 8, recordable: false, blank: false, sessions: 1,
            tracks: vec![Track { number: 1, session: 1, data: false, start: 0, length: 1500, mode: None }, Track { number: 2, session: 1, data: true, start: 1500, length: 100000, mode: Some(1) }],
            leadout: 101500, disc_type: None, capacity: 0,
        };
        let id = Identity { system: "pcenginecd".into(), ..Default::default() };
        assert_eq!(db.predict(&id, &phys), Some(("Ys I & II (Japan)".into(), false)));
    }

    #[test]
    fn titles_and_articles() {
        assert_eq!(title_key("Legend of Zelda, The - Ocarina of Time (Europe) (Rev 1)"), "thelegendofzeldaocarinaoftime");
        assert_eq!(title_key("Mario Kart 64 (Europe)"), "mariokart64");
        let cmp = r#"clrmamepro ( name "Nintendo - Nintendo 64" )
game ( name "Legend of Zelda, The - Ocarina of Time (Europe)" rom ( name "a.z64" size 33554432 sha1 A1 ) )
game ( name "Legend of Zelda, The - Majora's Mask (Europe)" rom ( name "b.z64" size 33554432 sha1 A2 ) )
game ( name "Mario Kart 64 (USA)" rom ( name "c.z64" size 12582912 sha1 A3 ) )
game ( name "Mario Kart 64 (Europe) (Rev 1)" rom ( name "d.z64" size 12582912 sha1 A4 ) )
game ( name "Mario Kart 64 (Europe)" rom ( name "e.z64" size 12582912 sha1 A5 ) )
game ( name "Mortal Kombat Trilogy (Europe)" rom ( name "f.z64" size 8388608 sha1 A6 ) )"#;
        let mut db = RefDb::default();
        db.merge(parse_dat(cmp, None).unwrap());
        assert_eq!(db.name_for_title("n64", "Mariokart 64", 12582912).as_deref(), Some("Mario Kart 64 (Europe)"));
        // Titre tronqué ambigu (deux Zelda) : pas de nom ; unique : trouvé.
        assert_eq!(db.name_for_title("n64", "The Legend Of Zelda", 33554432), None);
        assert_eq!(db.name_for_title("n64", "Mortal Kombat Tril", 8388608).as_deref(), Some("Mortal Kombat Trilogy (Europe)"));
    }
}
