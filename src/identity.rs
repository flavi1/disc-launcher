//! Identité d'un disque : champs extraits, régions, clé stable.

use crate::jobj;
use crate::json::Value;

#[derive(Clone, Debug, Default, PartialEq)]
pub struct Identity {
    /// Identifiant système ES-DE de la famille (`psx`, `megacd`, `saturn`…) ou de média (`dvd-video`…).
    pub system: String,
    /// Numéro de série normalisé (`SCES-00867`, `T-12345`).
    pub serial: Option<String>,
    /// Identifiant de jeu Nintendo (6 caractères) ou identifiant de titre.
    pub game_id: Option<String>,
    /// Numéro du disque (1 par défaut).
    pub disc: Option<u32>,
    /// Nombre total de disques si l'en-tête l'indique.
    pub discs_total: Option<u32>,
    pub revision: Option<String>,
    /// Région normalisée : `Japan`, `USA`, `Europe`, `World`, `Korea`, `Asia`…
    pub region: Option<String>,
    /// Titre interne lu sur le disque.
    pub title: Option<String>,
    /// Libellé du volume.
    pub label: Option<String>,
    /// Empreinte de la table des matières (pour les systèmes sans série).
    pub toc_fp: Option<String>,
    /// SHA-1 du contenu, quand il est connu dès l'identification (cartouches).
    pub sha1: Option<String>,
}

impl Identity {
    /// Clé stable : `psx:SCES-00867:d1:Europe`, `pcenginecd:toc-1a2b…:d1:-`.
    pub fn key(&self) -> String {
        let id = self.serial.clone().or_else(|| self.game_id.clone()).or_else(|| self.toc_fp.clone().map(|f| format!("toc-{f}"))).unwrap_or_else(|| "unknown".into());
        format!("{}:{}:d{}:{}", self.system, id.replace(':', "_"), self.disc.unwrap_or(1), self.region.as_deref().unwrap_or("-"))
    }

    pub fn to_value(&self) -> Value {
        jobj! {
            "system" => self.system.clone(), "serial" => self.serial.clone(), "game_id" => self.game_id.clone(),
            "disc" => self.disc, "discs_total" => self.discs_total, "revision" => self.revision.clone(),
            "region" => self.region.clone(), "title" => self.title.clone(), "label" => self.label.clone(),
            "toc_fp" => self.toc_fp.clone(), "sha1" => self.sha1.clone(), "key" => self.key(),
        }
    }

    pub fn from_value(v: &Value) -> Identity {
        Identity {
            system: v["system"].str_or("").to_string(),
            serial: v["serial"].string(),
            game_id: v["game_id"].string(),
            disc: v["disc"].as_i64().map(|x| x as u32),
            discs_total: v["discs_total"].as_i64().map(|x| x as u32),
            revision: v["revision"].string(),
            region: v["region"].string(),
            title: v["title"].string(),
            label: v["label"].string(),
            toc_fp: v["toc_fp"].string(),
            sha1: v["sha1"].string(),
        }
    }
}

/// Région d'après une série Sony (`SCES`, `SLUS`, `SLPM`…).
pub fn region_from_sony_serial(serial: &str) -> Option<String> {
    let c = serial.chars().nth(2)?;
    Some(
        match c {
            'E' => "Europe",
            'U' => "USA",
            'P' | 'J' | 'M' => "Japan",
            'K' => "Korea",
            'A' | 'C' | 'H' => "Asia",
            _ => return None,
        }
        .into(),
    )
}

/// Région d'après le 4e caractère d'un identifiant Nintendo (`GALE01` → USA).
pub fn region_from_nintendo_id(id: &str) -> Option<String> {
    let c = id.chars().nth(3)?;
    Some(
        match c {
            'E' | 'N' => "USA",
            'J' => "Japan",
            'K' | 'Q' | 'T' => "Korea",
            'W' => "Taiwan",
            'P' | 'D' | 'F' | 'S' | 'I' | 'U' | 'X' | 'Y' | 'Z' | 'H' | 'V' | 'L' | 'M' | 'R' => "Europe",
            _ => return None,
        }
        .into(),
    )
}

/// Symboles de zone Sega (Saturn `JTUBKAEL`, Mega CD `JUE`, Dreamcast `JUE`).
pub fn region_from_sega_area(area: &str) -> Option<String> {
    let a: String = area.chars().filter(|c| c.is_ascii_alphabetic()).collect::<String>().to_ascii_uppercase();
    let j = a.contains('J');
    let u = a.contains('U') || a.contains('T');
    let e = a.contains('E');
    Some(
        match (j, u, e) {
            (true, false, false) => "Japan",
            (false, true, false) => "USA",
            (false, false, true) => "Europe",
            (false, true, true) => "USA, Europe",
            (true, true, false) => "Japan, USA",
            (true, false, true) => "Japan, Europe",
            (true, true, true) => "World",
            _ => return None,
        }
        .into(),
    )
}

/// Région « principale » pour le choix d'un dossier régional.
pub fn main_region(region: &str) -> &'static str {
    let r = region.to_ascii_lowercase();
    if r == "world" || r.contains(',') {
        return if r.contains("usa") { "USA" } else if r.contains("europe") { "Europe" } else { "Japan" };
    }
    if r.contains("japan") {
        "Japan"
    } else if r.contains("usa") {
        "USA"
    } else if r.contains("europe") || ["france", "germany", "spain", "italy", "uk", "australia"].iter().any(|x| r.contains(x)) {
        "Europe"
    } else {
        "Other"
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn regions() {
        assert_eq!(region_from_sony_serial("SCES-00867").as_deref(), Some("Europe"));
        assert_eq!(region_from_sony_serial("SLPM-86000").as_deref(), Some("Japan"));
        assert_eq!(region_from_nintendo_id("GALE01").as_deref(), Some("USA"));
        assert_eq!(region_from_nintendo_id("GALP01").as_deref(), Some("Europe"));
        assert_eq!(region_from_sega_area("JTUBKAEL").as_deref(), Some("World"));
        assert_eq!(region_from_sega_area("J").as_deref(), Some("Japan"));
        assert_eq!(main_region("USA, Europe"), "USA");
        let i = Identity { system: "psx".into(), serial: Some("SCES-00867".into()), disc: Some(1), region: Some("Europe".into()), ..Default::default() };
        assert_eq!(i.key(), "psx:SCES-00867:d1:Europe");
        assert_eq!(Identity::from_value(&i.to_value()), i);
    }
}
