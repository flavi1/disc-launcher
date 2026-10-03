//! Profils de lecteur (`drive-profiles.toml`) : `standard`, `omnidrive`,
//! `kreon`, `friidump`.
//!
//! Le firmware OmniDrive ne change pas forcément l'identification INQUIRY du
//! lecteur : la table sert surtout à repérer les lecteurs *compatibles*. Le
//! profil effectif vient de la configuration (`[drives."/dev/sr0"] profile`),
//! ou d'une entrée de la table qui précise une révision de firmware.

use crate::device::DriveInfo;
use crate::json::Value;

pub const BUILTIN: &str = include_str!("../../data/drive-profiles.toml");

#[derive(Clone, Debug, PartialEq)]
pub struct Entry {
    pub vendor: String,
    pub model_prefix: String,
    pub firmware_prefix: Option<String>,
    /// Profil appliqué automatiquement (si `firmware_prefix` correspond).
    pub profile: Option<String>,
    /// Profil possible après flash (indication pour `disc-launcher doctor`).
    pub capable: Option<String>,
    pub note: String,
}

pub fn load() -> Vec<Entry> {
    let mut v = parse(BUILTIN);
    for dir in crate::paths::system_data_dirs().iter().rev().chain(std::iter::once(&crate::paths::user_config_dir())) {
        let f = dir.join("drive-profiles.toml");
        if let Ok(t) = std::fs::read_to_string(&f) {
            let extra = parse(&t);
            v.retain(|e| !extra.iter().any(|x| x.vendor == e.vendor && x.model_prefix == e.model_prefix && x.firmware_prefix == e.firmware_prefix));
            v.extend(extra);
        }
    }
    v
}

pub fn parse(src: &str) -> Vec<Entry> {
    let Ok(v) = crate::toml::parse(src) else { return vec![] };
    v["drive"]
        .as_arr()
        .iter()
        .map(|d: &Value| Entry {
            vendor: d["vendor"].str_or("").to_string(),
            model_prefix: d["model"].str_or("").to_string(),
            firmware_prefix: d["firmware"].string(),
            profile: d["profile"].string(),
            capable: d["capable"].string(),
            note: d["note"].str_or("").to_string(),
        })
        .collect()
}

fn matches(e: &Entry, d: &DriveInfo) -> bool {
    d.vendor.eq_ignore_ascii_case(&e.vendor) && d.model.to_ascii_uppercase().starts_with(&e.model_prefix.to_ascii_uppercase()) && e.firmware_prefix.as_ref().map_or(true, |f| d.firmware.starts_with(f.as_str()))
}

/// Profil effectif.
pub fn resolve(forced: &str, drive: Option<&DriveInfo>) -> String {
    if forced != "auto" && !forced.is_empty() {
        return forced.to_string();
    }
    if let Some(d) = drive {
        for e in load() {
            if matches(&e, d) {
                if let Some(p) = &e.profile {
                    return p.clone();
                }
            }
        }
    }
    "standard".into()
}

/// Profils possibles après flash (pour le diagnostic).
pub fn capabilities(drive: &DriveInfo) -> Vec<(String, String)> {
    load().into_iter().filter(|e| matches(e, drive)).filter_map(|e| e.capable.clone().map(|c| (c, e.note.clone()))).collect()
}
