//! Index de collection SQLite (`~/.local/state/disc-launcher/collection.db`)
//! et détection de l'existant (section 8.4).
//!
//! Chaque entrée garde le nom canonique prévu, le chemin réel, la taille exacte,
//! une empreinte partielle (taille + premier et dernier Mio) et, quand elle est
//! connue, l'empreinte MD5 complète. Un fichier renommé ou déplacé est retrouvé :
//! même taille, chemin enregistré disparu, même empreinte partielle (puis même
//! MD5 si l'entrée en a un) → chemin mis à jour.
//!
//! Les `.cue` ne sont pas indexés : ce petit fichier texte porte en général le
//! nom de ses pistes, et leurs tailles se ressemblent trop pour servir à
//! l'identification. Un jeu cue/bin est indexé par sa première piste.

use crate::handlers::Manifest;
use crate::json::Value;
use crate::naming::Target;
use crate::sqlite::{Db, Val};
use crate::{jobj, paths, util};
use std::collections::{BTreeMap, HashSet};
use std::path::{Path, PathBuf};

#[derive(Clone, Debug, PartialEq, Default)]
pub struct Entry {
    pub id: i64,
    pub system: String,
    /// Clé d'identité du disque (`psx:SCES-00867:d1:Europe`), None si inconnue.
    pub key: Option<String>,
    /// Chemin réel actuel.
    pub path: String,
    /// Nom canonique prévu lors du dump (sans extension).
    pub canonical_name: Option<String>,
    pub format: String,
    /// Taille exacte en octets (somme des fichiers pour un dossier).
    pub size: u64,
    pub mtime: i64,
    pub md5: Option<String>,
    /// Empreinte partielle (taille + premier et dernier Mio).
    pub quick_hash: Option<String>,
    pub sha1: Option<String>,
    /// `ok`, `mismatch` ou None (non vérifié).
    pub verified: Option<String>,
    pub name_source: Option<String>,
    pub game: Option<String>,
    pub disc: Option<u32>,
    pub added: i64,
    pub updated: i64,
    /// Date à laquelle le fichier a été constaté absent.
    pub missing_since: Option<i64>,
}

const COLS: &str = "id, system, key, path, canonical_name, format, size, mtime, md5, sha1, verified, name_source, game, disc, added, updated, missing_since, quick_hash";

impl Entry {
    fn from_row(r: &[Val]) -> Entry {
        Entry {
            id: r[0].int().unwrap_or(0),
            system: r[1].text().unwrap_or_default(),
            key: r[2].text(),
            path: r[3].text().unwrap_or_default(),
            canonical_name: r[4].text(),
            format: r[5].text().unwrap_or_default(),
            size: r[6].int().unwrap_or(0).max(0) as u64,
            mtime: r[7].int().unwrap_or(0),
            md5: r[8].text(),
            sha1: r[9].text(),
            verified: r[10].text(),
            name_source: r[11].text(),
            game: r[12].text(),
            disc: r[13].int().map(|d| d as u32),
            added: r[14].int().unwrap_or(0),
            updated: r[15].int().unwrap_or(0),
            missing_since: r[16].int(),
            quick_hash: r.get(17).and_then(|v| v.text()),
        }
    }
    pub fn to_value(&self) -> Value {
        jobj! {
            "system" => self.system.clone(), "key" => self.key.clone(), "path" => self.path.clone(),
            "canonical_name" => self.canonical_name.clone(), "format" => self.format.clone(), "size" => self.size,
            "md5" => self.md5.clone(), "sha1" => self.sha1.clone(), "verified" => self.verified.clone(),
            "name_source" => self.name_source.clone(), "game" => self.game.clone(), "disc" => self.disc,
            "missing_since" => self.missing_since,
        }
    }
    pub fn exists(&self) -> bool {
        Path::new(&self.path).exists()
    }
}

pub struct Collection {
    db: Db,
    pub file: PathBuf,
}

/// Niveau d'empreinte d'un scan.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Hashing {
    /// Aucune lecture de contenu (scan d'arrière-plan au démarrage).
    None,
    /// Empreinte partielle (taille + premier et dernier Mio).
    Quick,
    /// MD5 complet.
    Full,
}

pub fn default_path() -> PathBuf {
    paths::user_state_dir().join("collection.db")
}

/// Taille exacte et date de modification (dossier : somme des tailles).
pub fn file_meta(p: &Path) -> (u64, i64) {
    use std::os::unix::fs::MetadataExt;
    match std::fs::metadata(p) {
        Ok(m) if m.is_dir() => (dir_size(p), m.mtime()),
        Ok(m) => (m.len(), m.mtime()),
        Err(_) => (0, 0),
    }
}

fn dir_size(p: &Path) -> u64 {
    std::fs::read_dir(p).map(|it| it.flatten().map(|e| e.metadata().map(|m| if m.is_dir() { dir_size(&e.path()) } else { m.len() }).unwrap_or(0)).sum()).unwrap_or(0)
}

const SCHEMA: &str = "
CREATE TABLE IF NOT EXISTS entries (
  id             INTEGER PRIMARY KEY,
  system         TEXT NOT NULL,
  key            TEXT,
  path           TEXT NOT NULL UNIQUE,
  canonical_name TEXT,
  format         TEXT,
  size           INTEGER NOT NULL DEFAULT 0,
  mtime          INTEGER NOT NULL DEFAULT 0,
  md5            TEXT,
  sha1           TEXT,
  verified       TEXT,
  name_source    TEXT,
  game           TEXT,
  disc           INTEGER,
  added          INTEGER NOT NULL,
  updated        INTEGER NOT NULL,
  missing_since  INTEGER,
  quick_hash     TEXT
);
CREATE INDEX IF NOT EXISTS entries_key  ON entries(key);
CREATE INDEX IF NOT EXISTS entries_size ON entries(size);
CREATE INDEX IF NOT EXISTS entries_md5  ON entries(md5);
";

/// Compte rendu d'un scan.
#[derive(Default, Debug)]
pub struct ScanReport {
    pub added: usize,
    pub updated: usize,
    /// (ancien chemin, nouveau chemin)
    pub relocated: Vec<(String, String)>,
    pub missing: usize,
}

impl Collection {
    pub fn open() -> Result<Collection, String> {
        Self::open_at(&default_path())
    }

    pub fn open_at(p: &Path) -> Result<Collection, String> {
        let db = Db::open(p).map_err(|e| e.to_string())?;
        db.exec("PRAGMA journal_mode = WAL; PRAGMA synchronous = NORMAL;").map_err(|e| e.to_string())?;
        db.exec(SCHEMA).map_err(|e| e.to_string())?;
        // Migration 1 → 2 : empreinte partielle.
        let has_quick = db.query("PRAGMA table_info(entries)", &[]).map(|rows| rows.iter().any(|r| r.get(1).and_then(|v| v.text()).as_deref() == Some("quick_hash"))).unwrap_or(false);
        if !has_quick {
            db.exec("ALTER TABLE entries ADD COLUMN quick_hash TEXT").map_err(|e| e.to_string())?;
        }
        db.exec("CREATE INDEX IF NOT EXISTS entries_quick ON entries(quick_hash); PRAGMA user_version = 2;").map_err(|e| e.to_string())?;
        Ok(Collection { db, file: p.to_path_buf() })
    }

    fn select(&self, cond: &str, params: &[Val]) -> Vec<Entry> {
        self.db.query(&format!("SELECT {COLS} FROM entries {cond}"), params).map(|rows| rows.iter().map(|r| Entry::from_row(r)).collect()).unwrap_or_default()
    }

    pub fn entries(&self) -> Vec<Entry> {
        self.select("ORDER BY system, path", &[])
    }

    pub fn find_path(&self, p: &Path) -> Option<Entry> {
        self.select("WHERE path = ?", &[p.to_string_lossy().as_ref().into()]).into_iter().next()
    }

    /// Entrée d'une clé d'identité ; si son fichier a disparu, tente de le
    /// retrouver sous `roms` (renommage ou déplacement par l'utilisateur).
    pub fn find_key(&self, key: &str, roms: Option<&Path>) -> Option<Entry> {
        let list = self.select("WHERE key = ? ORDER BY updated DESC", &[key.into()]);
        if let Some(e) = list.iter().find(|e| e.exists()) {
            return Some(e.clone());
        }
        let roms = roms?;
        for e in list {
            if let Some(newp) = self.relocate(&e, roms) {
                return self.find_path(&newp);
            }
        }
        None
    }

    /// Enregistre (ou met à jour) un fichier produit ou trouvé. La taille, la
    /// date et l'empreinte partielle sont relues ; le MD5 complet est calculé
    /// s'il n'est pas fourni et que `full_hash` est vrai.
    pub fn record(&self, e: Entry, full_hash: bool) -> Result<(), String> {
        self.record_with(e, full_hash, true)
    }

    /// `quick` : calculer l'empreinte partielle si elle manque (faux pour un scan paresseux).
    pub fn record_with(&self, mut e: Entry, full_hash: bool, quick: bool) -> Result<(), String> {
        let p = index_path_for(Path::new(&e.path));
        e.path = p.to_string_lossy().into_owned();
        if let Some(x) = p.extension() {
            e.format = x.to_string_lossy().to_ascii_lowercase();
        }
        let (size, mtime) = file_meta(&p);
        e.size = size;
        e.mtime = mtime;
        if quick && e.quick_hash.is_none() && p.exists() {
            e.quick_hash = util::quick_hash(&p).ok();
        }
        if e.md5.is_none() && full_hash && p.exists() {
            e.md5 = util::md5_path(&p).ok();
        }
        let now = util::now_secs();
        // Même disque déjà connu à un chemin disparu : on remplace cette entrée.
        if let Some(k) = &e.key {
            for old in self.select("WHERE key = ? AND path <> ?", &[k.as_str().into(), e.path.as_str().into()]) {
                if !old.exists() {
                    let _ = self.db.execute("DELETE FROM entries WHERE id = ?", &[Val::Int(old.id)]);
                }
            }
        }
        let existing = self.find_path(&p);
        let added = existing.as_ref().map(|x| x.added).filter(|a| *a > 0).unwrap_or(now);
        let key = e.key.clone().or_else(|| existing.as_ref().and_then(|x| x.key.clone()));
        let canonical = e.canonical_name.clone().or_else(|| existing.as_ref().and_then(|x| x.canonical_name.clone()));
        self.db
            .execute(
                "INSERT INTO entries (system, key, path, canonical_name, format, size, mtime, md5, sha1, verified, name_source, game, disc, added, updated, missing_since, quick_hash)
                 VALUES (?,?,?,?,?,?,?,?,?,?,?,?,?,?,?,NULL,?)
                 ON CONFLICT(path) DO UPDATE SET system=excluded.system, key=excluded.key, canonical_name=excluded.canonical_name,
                   format=excluded.format, size=excluded.size, mtime=excluded.mtime, md5=excluded.md5, sha1=excluded.sha1,
                   verified=excluded.verified, name_source=excluded.name_source, game=excluded.game, disc=excluded.disc,
                   updated=excluded.updated, missing_since=NULL, quick_hash=excluded.quick_hash",
                &[
                    e.system.as_str().into(),
                    key.into(),
                    e.path.as_str().into(),
                    canonical.into(),
                    e.format.as_str().into(),
                    Val::Int(e.size as i64),
                    Val::Int(e.mtime),
                    e.md5.clone().into(),
                    e.sha1.clone().into(),
                    e.verified.clone().into(),
                    e.name_source.clone().into(),
                    e.game.clone().into(),
                    e.disc.map(|d| d as i64).into(),
                    Val::Int(added),
                    Val::Int(now),
                    e.quick_hash.clone().into(),
                ],
            )
            .map(|_| ())
            .map_err(|e| e.to_string())
    }

    /// Après renommage d'un fichier vers son nom canonique.
    pub fn set_canonical(&self, id: i64, newp: &Path, canonical: &str, source: &str) {
        self.set_path(id, newp);
        let _ = self.db.execute("UPDATE entries SET canonical_name = ?, name_source = ? WHERE id = ?", &[canonical.into(), source.into(), Val::Int(id)]);
    }

    fn set_path(&self, id: i64, newp: &Path) {
        let (size, mtime) = file_meta(newp);
        let fmt = newp.extension().map(|x| x.to_string_lossy().to_ascii_lowercase()).unwrap_or_default();
        let _ = self.db.execute(
            "UPDATE entries SET path = ?, format = ?, size = ?, mtime = ?, updated = ?, missing_since = NULL WHERE id = ?",
            &[newp.to_string_lossy().as_ref().into(), fmt.into(), Val::Int(size as i64), Val::Int(mtime), Val::Int(util::now_secs()), Val::Int(id)],
        );
    }

    /// Retrouve le fichier d'une entrée disparue : même taille, puis même
    /// empreinte partielle, puis même MD5 complet si l'entrée en possède un.
    pub fn relocate(&self, e: &Entry, roms: &Path) -> Option<PathBuf> {
        if e.exists() || e.size == 0 {
            return None;
        }
        let known: HashSet<String> = self.entries().into_iter().map(|x| x.path).collect();
        for c in walk(roms) {
            if known.contains(c.to_string_lossy().as_ref()) || file_meta(&c).0 != e.size {
                continue;
            }
            if same_content(e, &c) {
                crate::dl_log!(info, "collection", "fichier renommé ou déplacé retrouvé", "from" => e.path, "to" => c.display());
                self.set_path(e.id, &c);
                return Some(c);
            }
        }
        None
    }

    /// Scan de tous les fichiers sous `roms` :
    /// - fichier connu : taille/date mises à jour si elles ont changé ;
    /// - fichier inconnu de même taille qu'une entrée disparue, même MD5 :
    ///   renommage → chemin mis à jour ;
    /// - fichier inconnu d'un format accepté dans un dossier système : ajouté ;
    /// - entrée dont le fichier a disparu : marquée absente.
    ///
    /// Les fichiers ajoutés reçoivent toujours l'empreinte partielle (rapide) ;
    /// `full_hash` calcule en plus leur MD5 complet (lent sur une grosse collection).
    pub fn scan(&self, roms: &Path, manifests: &BTreeMap<String, Manifest>, full_hash: bool) -> ScanReport {
        self.scan_with(roms, manifests, if full_hash { Hashing::Full } else { Hashing::Quick })
    }

    /// Scan avec le niveau d'empreinte choisi. `Hashing::None` (paresseux) ne lit
    /// aucun contenu, sauf pour confirmer un renommage (fichiers de même taille
    /// qu'une entrée disparue).
    pub fn scan_with(&self, roms: &Path, manifests: &BTreeMap<String, Manifest>, hashing: Hashing) -> ScanReport {
        let full_hash = hashing == Hashing::Full;
        let mut rep = ScanReport::default();
        let all = self.entries();
        let by_path: BTreeMap<String, Entry> = all.iter().map(|e| (e.path.clone(), e.clone())).collect();
        let mut orphans: Vec<Entry> = all.iter().filter(|e| !e.exists()).cloned().collect();
        // dossier système → (identifiant, formats acceptés)
        let mut folders: BTreeMap<String, (String, Vec<String>)> = BTreeMap::new();
        for m in manifests.values().filter(|m| m.kind() == "console") {
            for f in m.all_folders() {
                folders.insert(f, (m.id.clone(), m.accepted_formats()));
            }
        }
        for f in walk(roms) {
            let fs = f.to_string_lossy().into_owned();
            let (size, mtime) = file_meta(&f);
            if let Some(e) = by_path.get(&fs) {
                if e.size != size || e.mtime != mtime {
                    let md5 = if e.size != size { None } else { e.md5.clone() };
                    let quick = if hashing == Hashing::None { None } else { util::quick_hash(&f).ok() };
                    let _ = self.db.execute(
                        "UPDATE entries SET size = ?, mtime = ?, md5 = ?, quick_hash = ?, verified = CASE WHEN ? IS NULL THEN NULL ELSE verified END, updated = ? WHERE id = ?",
                        &[Val::Int(size as i64), Val::Int(mtime), md5.clone().into(), quick.into(), md5.into(), Val::Int(util::now_secs()), Val::Int(e.id)],
                    );
                    rep.updated += 1;
                }
                continue;
            }
            // Renommage ?
            let cands: Vec<usize> = orphans.iter().enumerate().filter(|(_, o)| o.size == size && size > 0).map(|(i, _)| i).collect();
            if !cands.is_empty() {
                if let Some(i) = cands.into_iter().find(|i| same_content(&orphans[*i], &f)) {
                    let o = orphans.remove(i);
                    dl_log!(info, "collection", "renommage détecté", "from" => o.path, "to" => fs);
                    self.set_path(o.id, &f);
                    rep.relocated.push((o.path, fs));
                    continue;
                }
            }
            // Nouveau fichier dans un dossier système ?
            let rel = f.strip_prefix(roms).unwrap_or(&f);
            let folder = rel.components().next().map(|c| c.as_os_str().to_string_lossy().into_owned()).unwrap_or_default();
            let Some((sys_id, exts)) = folders.get(&folder) else { continue };
            let ext = f.extension().map(|x| x.to_string_lossy().to_ascii_lowercase()).unwrap_or_default();
            if !indexable(&f, exts) {
                continue;
            }
            let stem = f.file_stem().map(|s| s.to_string_lossy().into_owned()).unwrap_or_default();
            let _ = self.record_with(
                Entry {
                system: sys_id.clone(),
                path: fs,
                format: ext,
                name_source: Some("scan".into()),
                game: Some(crate::refdb::game_name(&stem.replace(" (Track 1)", "").replace(" (Track 01)", ""))),
                disc: crate::refdb::disc_of(&stem),
                ..Default::default()
            },
                full_hash,
                hashing != Hashing::None,
            );
            rep.added += 1;
        }
        let now = util::now_secs();
        for o in orphans {
            if o.missing_since.is_none() {
                let _ = self.db.execute("UPDATE entries SET missing_since = ? WHERE id = ?", &[Val::Int(now), Val::Int(o.id)]);
            }
            rep.missing += 1;
        }
        rep
    }

    /// Supprime les entrées absentes depuis plus de `days` jours.
    pub fn prune_missing(&self, days: i64) -> usize {
        self.db.execute("DELETE FROM entries WHERE missing_since IS NOT NULL AND missing_since < ?", &[Val::Int(util::now_secs() - days * 86400)]).unwrap_or(0)
    }
}

/// Même contenu ? Empreinte partielle d'abord, MD5 complet ensuite si
/// l'entrée en possède un (confirmation).
fn same_content(e: &Entry, p: &Path) -> bool {
    match &e.quick_hash {
        Some(q) => {
            if util::quick_hash(p).ok().as_ref() != Some(q) {
                return false;
            }
            match &e.md5 {
                Some(m) => util::md5_path(p).ok().as_ref() == Some(m),
                None => true,
            }
        }
        None => e.md5.is_some() && util::md5_path(p).ok() == e.md5,
    }
}

/// `.bin` sans numéro de piste, ou première piste d'un jeu cue/bin.
fn first_track(p: &Path) -> bool {
    let n = p.file_name().map(|s| s.to_string_lossy().into_owned()).unwrap_or_default();
    !n.contains("(Track ") || n.contains("(Track 1)") || n.contains("(Track 01)")
}

/// Fichier pris en compte par l'index pour un système qui accepte `exts`.
fn indexable(p: &Path, exts: &[String]) -> bool {
    let ext = p.extension().map(|x| x.to_string_lossy().to_ascii_lowercase()).unwrap_or_default();
    match ext.as_str() {
        "cue" | "m3u" => false,
        "bin" => (exts.iter().any(|e| e == "cue" || e == "bin")) && first_track(p),
        _ => exts.contains(&ext),
    }
}

/// Chemin indexé pour un produit : un `.cue` est remplacé par sa première piste.
pub fn index_path_for(p: &Path) -> PathBuf {
    if p.extension().is_some_and(|x| x.eq_ignore_ascii_case("cue")) {
        if let Ok(t) = std::fs::read_to_string(p) {
            for l in t.lines() {
                let l = l.trim();
                if l.to_ascii_uppercase().starts_with("FILE") {
                    if let Some(name) = l.split('"').nth(1) {
                        let bin = p.with_file_name(name);
                        if bin.exists() {
                            return bin;
                        }
                    }
                }
            }
        }
    }
    p.to_path_buf()
}

/// Chemin à lancer pour une entrée : une piste `.bin` est lancée par le `.cue`
/// du même dossier qui la référence.
pub fn launch_path_for(p: &Path) -> PathBuf {
    if !p.extension().is_some_and(|x| x.eq_ignore_ascii_case("bin")) {
        return p.to_path_buf();
    }
    let name = p.file_name().map(|s| s.to_string_lossy().into_owned()).unwrap_or_default();
    if let Some(dir) = p.parent() {
        if let Ok(it) = std::fs::read_dir(dir) {
            for e in it.flatten() {
                let c = e.path();
                if c.extension().is_some_and(|x| x.eq_ignore_ascii_case("cue")) && std::fs::read_to_string(&c).is_ok_and(|t| t.contains(&format!("\"{name}\""))) {
                    return c;
                }
            }
        }
    }
    p.to_path_buf()
}

/// Tous les fichiers (et dossiers-jeux `.ps3`, `.wua`…) sous `root`, hors
/// fichiers cachés, temporaires et sauvegardes.
pub fn walk(root: &Path) -> Vec<PathBuf> {
    fn rec(d: &Path, depth: usize, out: &mut Vec<PathBuf>) {
        if depth > 5 {
            return;
        }
        let Ok(it) = std::fs::read_dir(d) else { return };
        for e in it.flatten() {
            let name = e.file_name().to_string_lossy().into_owned();
            if name.starts_with('.') || name.contains(".bak-") {
                continue;
            }
            let p = e.path();
            let Ok(ft) = e.file_type() else { continue };
            if ft.is_dir() {
                let ext = p.extension().map(|x| x.to_string_lossy().to_ascii_lowercase()).unwrap_or_default();
                if ["ps3", "wua", "wud"].contains(&ext.as_str()) {
                    out.push(p);
                } else {
                    rec(&p, depth + 1, out);
                }
            } else if ft.is_file() {
                let ext = p.extension().map(|x| x.to_string_lossy().to_ascii_lowercase()).unwrap_or_default();
                if ext != "cue" && !(ext == "bin" && !first_track(&p)) {
                    out.push(p);
                }
            }
        }
    }
    let mut v = vec![];
    rec(root, 0, &mut v);
    v.sort();
    v
}

/// Situation d'un disque vis-à-vis de la collection (section 8.4).
#[derive(Clone, Debug, PartialEq)]
pub enum Situation {
    /// 1. Copie connue par sa clé (même renommée ou déplacée).
    Known(PathBuf),
    /// 2. Le chemin cible existe.
    Present(PathBuf),
    /// 3. Même nom dans un autre format accepté.
    OtherFormat(PathBuf),
    /// 4. Jeu multi-disques présent sans ce disque.
    GameIncomplete { game_dir: PathBuf, present: Vec<PathBuf> },
    /// 5. Dump interrompu pour ce disque.
    Partial(String),
    /// 6. Nouveau disque.
    New,
}

impl Situation {
    pub fn name(&self) -> &'static str {
        match self {
            Situation::Known(_) => "known",
            Situation::Present(_) => "present",
            Situation::OtherFormat(_) => "other-format",
            Situation::GameIncomplete { .. } => "game-incomplete",
            Situation::Partial(_) => "partial",
            Situation::New => "new",
        }
    }
    pub fn existing(&self) -> Option<&Path> {
        match self {
            Situation::Known(p) | Situation::Present(p) | Situation::OtherFormat(p) => Some(p),
            _ => None,
        }
    }
    pub fn to_value(&self) -> Value {
        let mut v = jobj! {"kind" => self.name()};
        match self {
            Situation::Known(p) | Situation::Present(p) | Situation::OtherFormat(p) => v.set("path", p.to_string_lossy().into_owned()),
            Situation::GameIncomplete { game_dir, present } => {
                v.set("game_dir", game_dir.to_string_lossy().into_owned());
                v.set("present", present.iter().map(|p| Value::from(p.to_string_lossy().into_owned())).collect::<Vec<_>>());
            }
            Situation::Partial(j) => v.set("job", j.clone()),
            Situation::New => {}
        }
        v
    }
}

/// Résultat de la vérification, plus un éventuel conflit de nom.
#[derive(Clone, Debug)]
pub struct Check {
    pub situation: Situation,
    /// Cible alternative si le chemin prévu est pris par un autre disque.
    pub collision_alt: Option<Target>,
}

pub fn check_existing(col: &Collection, key: &str, target: &Target, accepted: &[String], partial_job: Option<String>, roms: Option<&Path>) -> Check {
    if let Some(e) = col.find_key(key, roms) {
        return Check { situation: Situation::Known(PathBuf::from(&e.path)), collision_alt: None };
    }
    if target.path.exists() {
        // Collision : le fichier appartient à une autre identité connue.
        if let Some(e) = col.find_path(&target.path) {
            if e.key.as_deref().map_or(false, |k| k != key) {
                return Check { situation: Situation::New, collision_alt: Some(alternative(target)) };
            }
        }
        return Check { situation: Situation::Present(target.path.clone()), collision_alt: None };
    }
    let dir = target.path.parent().unwrap_or(Path::new("."));
    for ext in accepted.iter().filter(|e| **e != target.ext && *e != "m3u" && *e != "bin") {
        let p = dir.join(format!("{}.{ext}", target.stem));
        if p.exists() {
            return Check { situation: Situation::OtherFormat(p), collision_alt: None };
        }
        // Copie à plat quand la cible est dans un dossier .m3u (et inversement).
        let flat = target.system_dir.join(format!("{}.{ext}", target.stem));
        if flat.exists() {
            return Check { situation: Situation::OtherFormat(flat), collision_alt: None };
        }
    }
    let flat_target = target.system_dir.join(format!("{}.{}", target.stem, target.ext));
    if flat_target != target.path && flat_target.exists() {
        return Check { situation: Situation::Present(flat_target), collision_alt: None };
    }
    if let Some(gd) = &target.game_dir {
        if gd.is_dir() {
            let present: Vec<PathBuf> = std::fs::read_dir(gd).map(|it| it.flatten().map(|e| e.path()).filter(|p| p.extension().map_or(false, |x| x != "m3u")).collect()).unwrap_or_default();
            if !present.is_empty() {
                return Check { situation: Situation::GameIncomplete { game_dir: gd.clone(), present }, collision_alt: None };
            }
        }
    }
    if let Some(j) = partial_job {
        return Check { situation: Situation::Partial(j), collision_alt: None };
    }
    Check { situation: Situation::New, collision_alt: None }
}

/// « Nom.chd » → « Nom (2).chd » (premier nom libre).
pub fn alternative(t: &Target) -> Target {
    let dir = t.path.parent().unwrap_or(Path::new(".")).to_path_buf();
    for n in 2..100 {
        let stem = format!("{} ({n})", t.stem);
        let p = dir.join(format!("{stem}.{}", t.ext));
        if !p.exists() {
            let mut a = t.clone();
            a.stem = stem;
            a.path = p;
            return a;
        }
    }
    t.clone()
}

/// Écrit (ou met à jour) la liste `.m3u` d'un jeu multi-disques : les disques
/// de `discs_dir` dont le nom commence par `game`.
pub fn write_m3u_for_game(m3u: &Path, discs_dir: &Path, base_dir_for_relative: &Path, game: &str) -> std::io::Result<()> {
    let mut files: Vec<PathBuf> = std::fs::read_dir(discs_dir)?
        .flatten()
        .map(|e| e.path())
        .filter(|p| p.is_file() && p.extension().map_or(false, |x| x != "m3u" && x != "bin" && x != "sav"))
        .filter(|p| p.file_name().map_or(false, |n| n.to_string_lossy().starts_with(game)))
        .filter(|p| !p.to_string_lossy().contains(".bak-"))
        .collect();
    files.sort_by_key(|p| (crate::refdb::disc_of(&p.file_stem().unwrap_or_default().to_string_lossy()).unwrap_or(0), p.clone()));
    let mut s = String::new();
    for f in files {
        let rel = f.strip_prefix(base_dir_for_relative).unwrap_or(&f);
        s.push_str(&rel.to_string_lossy());
        s.push('\n');
    }
    paths::write_atomic(m3u, s.as_bytes())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::Config;
    use crate::identity::Identity;
    use crate::naming::{build_target, resolution_from_name, NameConfidence};

    fn tmpdir(name: &str) -> PathBuf {
        let d = std::env::temp_dir().join(format!("dl-test-{name}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&d);
        std::fs::create_dir_all(&d).unwrap();
        d
    }

    fn psx() -> Manifest {
        Manifest { id: "psx".into(), raw: crate::toml::parse("[handler]\nid=\"psx\"\nkind=\"console\"\n[formats]\ntarget=\"chd\"\naccepted=[\"chd\",\"cue\",\"m3u\"]\nmultidisc=true\n").unwrap() }
    }

    #[test]
    fn situations() {
        let root = tmpdir("col");
        let cfg = Config::from_value(crate::toml::parse(&format!("[general]\nroms_dir = \"{}\"\n", root.display())).unwrap());
        let m = psx();
        let id = Identity { system: "psx".into(), serial: Some("SCES-00867".into()), disc: Some(2), region: Some("France".into()), ..Default::default() };
        let r = resolution_from_name("Final Fantasy VII (France) (Disc 2)", NameConfidence::Exact, "t", &id, None);
        let t = build_target(&cfg, &m, &id, &r);
        let col = Collection::open_at(&root.join("c.db")).unwrap();
        let check = |col: &Collection| check_existing(col, &id.key(), &t, &m.accepted_formats(), None, Some(&root)).situation;
        assert_eq!(check(&col), Situation::New);
        std::fs::create_dir_all(t.game_dir.as_ref().unwrap()).unwrap();
        let d1 = t.game_dir.as_ref().unwrap().join("Final Fantasy VII (France) (Disc 1).chd");
        std::fs::write(&d1, b"x").unwrap();
        assert!(matches!(check(&col), Situation::GameIncomplete { .. }));
        std::fs::write(root.join("psx/Final Fantasy VII (France) (Disc 2).cue"), b"x").unwrap();
        assert!(matches!(check(&col), Situation::OtherFormat(_)));
        std::fs::write(&t.path, b"disc two content").unwrap();
        assert_eq!(check(&col), Situation::Present(t.path.clone()));
        col.record(Entry { system: "psx".into(), key: Some(id.key()), path: t.path.to_string_lossy().into(), canonical_name: Some(r.name.clone()), format: "chd".into(), ..Default::default() }, true).unwrap();
        let e = col.find_path(&t.path).unwrap();
        assert_eq!(e.size, 16);
        assert_eq!(e.md5.as_deref(), Some(util::md5_hex(b"disc two content").as_str()));
        // Renommé par l'utilisateur : retrouvé par taille + MD5 lors de la recherche par clé.
        let moved = root.join("psx/FF7 disque 2.chd");
        std::fs::rename(&t.path, &moved).unwrap();
        assert_eq!(check(&col), Situation::Known(moved.clone()));
        assert_eq!(col.find_path(&moved).unwrap().canonical_name.as_deref(), Some("Final Fantasy VII (France) (Disc 2)"));
        write_m3u_for_game(t.m3u.as_ref().unwrap(), t.game_dir.as_ref().unwrap(), t.game_dir.as_ref().unwrap(), "Final Fantasy VII (France)").unwrap();
        assert_eq!(std::fs::read_to_string(t.m3u.as_ref().unwrap()).unwrap(), "Final Fantasy VII (France) (Disc 1).chd\n");
        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn scan_detects_renames() {
        let root = tmpdir("scan");
        std::fs::create_dir_all(root.join("psx")).unwrap();
        let mut all = BTreeMap::new();
        all.insert("psx".to_string(), psx());
        let col = Collection::open_at(&root.join("c.db")).unwrap();
        std::fs::write(root.join("psx/A (Europe).chd"), b"AAAA").unwrap();
        std::fs::write(root.join("psx/B (Europe).chd"), b"BBBB").unwrap(); // même taille, contenu différent
        std::fs::write(root.join("psx/notes.txt"), b"ignored").unwrap();
        let r = col.scan(&root, &all, true);
        assert_eq!((r.added, r.relocated.len()), (2, 0));
        // L'utilisateur renomme A et le range dans un sous-dossier ; B disparaît.
        std::fs::create_dir_all(root.join("psx/favoris")).unwrap();
        std::fs::rename(root.join("psx/A (Europe).chd"), root.join("psx/favoris/Mon jeu A.chd")).unwrap();
        std::fs::remove_file(root.join("psx/B (Europe).chd")).unwrap();
        let r = col.scan(&root, &all, true);
        assert_eq!(r.relocated.len(), 1);
        assert!(r.relocated[0].1.ends_with("favoris/Mon jeu A.chd"));
        assert_eq!(r.missing, 1);
        let e = col.find_path(&root.join("psx/favoris/Mon jeu A.chd")).unwrap();
        assert_eq!(e.game.as_deref(), Some("A (Europe)"));
        assert_eq!(col.entries().len(), 2);
        assert_eq!(col.prune_missing(-1), 1);
        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn cue_bin_and_quick_hash() {
        let root = tmpdir("cue");
        std::fs::create_dir_all(root.join("atarijaguarcd")).unwrap();
        let mut all = BTreeMap::new();
        all.insert("atarijaguarcd".to_string(), Manifest { id: "atarijaguarcd".into(), raw: crate::toml::parse("[handler]\nid=\"atarijaguarcd\"\nkind=\"console\"\n[formats]\ntarget=\"cue\"\naccepted=[\"cue\"]\n").unwrap() });
        let d = root.join("atarijaguarcd");
        std::fs::write(d.join("Jeu.cue"), "FILE \"Jeu (Track 1).bin\" BINARY\nFILE \"Jeu (Track 2).bin\" BINARY\n").unwrap();
        std::fs::write(d.join("Jeu (Track 1).bin"), vec![1u8; 3 << 20]).unwrap();
        std::fs::write(d.join("Jeu (Track 2).bin"), vec![2u8; 1000]).unwrap();
        let col = Collection::open_at(&root.join("c.db")).unwrap();
        let r = col.scan(&root, &all, false);
        assert_eq!(r.added, 1, "seule la première piste est indexée, pas le .cue");
        let e = &col.entries()[0];
        assert!(e.path.ends_with("Jeu (Track 1).bin") && e.quick_hash.is_some() && e.md5.is_none());
        assert_eq!(launch_path_for(Path::new(&e.path)), d.join("Jeu.cue"));
        assert_eq!(index_path_for(&d.join("Jeu.cue")), d.join("Jeu (Track 1).bin"));
        // Renommage du jeu entier : retrouvé par taille + empreinte partielle.
        for (a, b) in [("Jeu.cue", "Autre.cue"), ("Jeu (Track 1).bin", "Autre (Track 1).bin"), ("Jeu (Track 2).bin", "Autre (Track 2).bin")] {
            std::fs::rename(d.join(a), d.join(b)).unwrap();
        }
        let r = col.scan(&root, &all, false);
        assert_eq!(r.relocated.len(), 1);
        // Même taille, milieu différent : même empreinte partielle (par construction),
        // mais un MD5 enregistré l'emporte.
        let mut big = vec![0u8; 5 << 20];
        let a = root.join("a.bin");
        std::fs::write(&a, &big).unwrap();
        big[3 << 20] = 9;
        let b = root.join("b.bin");
        std::fs::write(&b, &big).unwrap();
        assert_eq!(util::quick_hash(&a).unwrap(), util::quick_hash(&b).unwrap());
        let e = Entry { size: 5 << 20, quick_hash: util::quick_hash(&a).ok(), md5: util::md5_path(&a).ok(), ..Default::default() };
        assert!(same_content(&e, &a) && !same_content(&e, &b));
        let _ = std::fs::remove_dir_all(&root);
    }
}
