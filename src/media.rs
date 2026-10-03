//! Gestionnaire multimédia générique : choisit un « lecteur » (application)
//! dans la configuration et lance sa commande pour le média inséré.
//! Aucune application n'est privilégiée dans le code : Kodi, mpv, VLC…
//! ne sont que des entrées de `[media.players.*]`.
//!
//! ```toml
//! [media]
//! player = "auto"                   # ou le nom d'un lecteur
//! auto_order = ["kodi", "mpv", "vlc"]
//!
//! [media.players.vlc]
//! name = "VLC"
//! dvd-video = ["vlc", "dvd://{device}"]   # commande pour un média précis
//! default = ["vlc", "{mount}"]            # sinon
//! # command = [...]                       # sinon (tous médias)
//! # requires = "vlc"                      # programme dont la présence est testée
//!
//! [handlers.cdda]
//! player = "mpv"                    # choix par média
//! ```
//!
//! Une commande dont un gabarit `{…}` reste vide (ex. `{mount}` sans montage)
//! est considérée inapplicable : on passe au lecteur suivant.

use crate::config::Config;
use crate::json::Value;
use crate::{jobj, paths, util};

#[derive(Clone, Debug, PartialEq)]
pub struct Choice {
    pub player: String,
    pub name: String,
    pub command: Vec<String>,
}

fn template_for(p: &Value, media: &str) -> Vec<String> {
    for k in [media, "default", "command"] {
        let v = p.get(k).strings();
        if !v.is_empty() {
            return v;
        }
    }
    vec![]
}

/// Rendu d'un gabarit ; None si une variable utilisée est vide ou inconnue.
pub fn render_strict(tpl: &[String], vars: &[(String, String)]) -> Option<Vec<String>> {
    let mut out = vec![];
    for a in tpl {
        let mut s = String::new();
        let mut rest = a.as_str();
        while let Some(i) = rest.find('{') {
            s.push_str(&rest[..i]);
            let after = &rest[i + 1..];
            let j = after.find('}')?;
            let key = &after[..j];
            let val = vars.iter().find(|(k, _)| k == key).map(|(_, v)| v.clone()).filter(|v| !v.is_empty())?;
            s.push_str(&val);
            rest = &after[j + 1..];
        }
        s.push_str(rest);
        out.push(s);
    }
    Some(out)
}

fn installed(p: &Value, cmd: &[String]) -> bool {
    let probe = p["requires"].as_str().map(|s| s.to_string()).or_else(|| cmd.first().cloned());
    probe.is_some_and(|c| paths::which(&c).is_some())
}

/// Lecteur et commande pour ce média.
pub fn choose(cfg: &Config, media: &str, vars: &[(String, String)]) -> Option<Choice> {
    let players = cfg.raw.path("media.players");
    let wanted = cfg.handler(media)["player"].as_str().or_else(|| cfg.raw.path("media.player").as_str()).unwrap_or("auto").to_string();
    let mut order: Vec<String> = vec![];
    if wanted != "auto" {
        order.push(wanted.clone());
    }
    for p in cfg.raw.path("media.auto_order").strings() {
        if !order.contains(&p) {
            order.push(p);
        }
    }
    for name in order {
        let p = players.get(&name);
        if p.is_null() {
            continue;
        }
        let tpl = template_for(p, media);
        if tpl.is_empty() {
            continue;
        }
        let Some(cmd) = render_strict(&tpl, vars) else { continue };
        if !installed(p, &cmd) {
            continue;
        }
        if name != wanted && wanted != "auto" {
            crate::dl_log!(info, "media", format!("lecteur « {wanted} » indisponible pour {media}, repli sur « {name} »"));
        }
        return Some(Choice { name: p["name"].str_or(&name).to_string(), player: name, command: cmd });
    }
    None
}

/// Variables de gabarit : `DL_*` de l'environnement + périphérique et montage.
pub fn vars(device: &str) -> Vec<(String, String)> {
    let mut v = crate::handlers::template_vars_from_env();
    let set = |v: &mut Vec<(String, String)>, k: &str, val: String| {
        if !v.iter().any(|(x, _)| x == k) {
            v.push((k.to_string(), val));
        }
    };
    set(&mut v, "device", device.to_string());
    set(&mut v, "devname", device.trim_start_matches("/dev/").to_string());
    if let Some(mp) = crate::device::find_mount_point(device) {
        set(&mut v, "mount", mp.to_string_lossy().into_owned());
    }
    v
}

pub fn describe(cfg: &Config, media: &str, device: &str) -> Value {
    let mut v = vars(device);
    if media == "dcim" {
        // Le montage n'a lieu qu'à la lecture : valeurs fictives pour le choix du lecteur.
        for k in ["mount", "dcim"] {
            if !v.iter().any(|(x, _)| x == k) {
                v.push((k.to_string(), "-".to_string()));
            }
        }
    }
    let c = choose(cfg, media, &v);
    jobj! {
        "id" => media,
        "player" => c.as_ref().map(|c| c.player.clone()),
        "player_name" => c.as_ref().map(|c| c.name.clone()),
        "actions" => jobj!{"play-disc" => c.is_some(), "play-existing" => false, "dump" => false},
        "missing" => if c.is_some() { Vec::<String>::new() } else { vec!["lecteur multimédia".to_string()] },
    }
}

/// Variables d'un disque photo : `{dcim}`, dossier DCIM du disque monté
/// (monté par udisks2 au besoin).
pub fn dcim_vars(device: &str) -> Vec<(String, String)> {
    let mut v = vars(device);
    if let Some(mount) = crate::data::ensure_mounted(device) {
        if !v.iter().any(|(k, _)| k == "mount") {
            v.push(("mount".into(), mount.to_string_lossy().into_owned()));
        }
        if let Ok(it) = std::fs::read_dir(&mount) {
            if let Some(d) = it.flatten().find(|e| e.file_name().to_string_lossy().eq_ignore_ascii_case("DCIM")) {
                v.push(("dcim".into(), d.path().to_string_lossy().into_owned()));
            }
        }
    }
    v
}

/// Lance le lecteur, détaché, avec l'environnement courant (variables DL_*).
pub fn play(cfg: &Config, media: &str, device: &str) -> Result<Value, String> {
    let v = if media == "dcim" { dcim_vars(device) } else { vars(device) };
    play_with(cfg, media, &v)
}

/// Lance le lecteur choisi pour `media` avec ces variables de gabarit.
pub fn play_with(cfg: &Config, media: &str, v: &[(String, String)]) -> Result<Value, String> {
    let c = choose(cfg, media, v).ok_or_else(|| format!("aucun lecteur multimédia disponible pour {media}"))?;
    let mut cmd = c.command.clone();
    cmd[0] = paths::which(&cmd[0]).map(|p| p.to_string_lossy().into_owned()).unwrap_or(cmd[0].clone());
    let pid = crate::generic::spawn_detached(&cmd, &paths::log_dir().join(format!("player-{}.log", c.player)))?;
    let _ = util::now_secs();
    Ok(jobj! {"player" => c.player, "player_name" => c.name, "pid" => pid, "command" => c.command})
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn choose_players() {
        let cfg = Config::from_value(
            crate::toml::parse(
                r#"
[media]
player = "nope"
auto_order = ["ghost", "sh1", "sh2"]
[media.players.ghost]
command = ["/nonexistent/ghost"]
[media.players.sh1]
name = "Shell 1"
default = ["sh", "{mount}"]
[media.players.sh2]
name = "Shell 2"
cdda = ["sh", "-c", "cdda {device}"]
"#,
            )
            .unwrap(),
        );
        let v = vec![("device".to_string(), "/dev/sr0".to_string())];
        // sh1 : {mount} vide → inapplicable ; sh2 : commande spécifique cdda
        let c = choose(&cfg, "cdda", &v).unwrap();
        assert_eq!(c.player, "sh2");
        assert_eq!(c.command, vec!["sh", "-c", "cdda /dev/sr0"]);
        assert!(choose(&cfg, "dvd-video", &v).is_none());
        let mut v2 = v.clone();
        v2.push(("mount".into(), "/run/media/x/DVD".into()));
        assert_eq!(choose(&cfg, "dvd-video", &v2).unwrap().command, vec!["sh", "/run/media/x/DVD"]);
    }
}
