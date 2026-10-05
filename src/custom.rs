//! Actions personnalisées, déclarées dans la configuration et ajoutées aux
//! propositions des disques ou volumes choisis :
//!
//! ```toml
//! [actions.rip-cd]
//! label = "Ripper le CD audio"
//! handlers = ["cdda"]                 # gestionnaires concernés ; "*" = tous
//! command = "encbd \"$DL_DEVICE\" ~/Musique"
//! terminal = true                     # suivre la commande dans un terminal
//! ```
//!
//! `command` est soit une chaîne, exécutée par `sh -c` avec les variables
//! `DL_*` (comme pour un gestionnaire), soit une liste d'arguments avec les
//! gabarits `{device}`, `{mount}`, `{existing}`, `{label}`, `{name}`,
//! `{handler}`, `{roms}` (et `~` en début d'argument).

use crate::config::Config;
use crate::handlers;
use crate::identify::IdentResult;
use crate::json::Value;
use crate::naming::Resolution;
use crate::paths;

pub const PREFIX: &str = "custom:";

/// Une action déclarée.
#[derive(Clone, Debug, PartialEq)]
pub struct Action {
    pub name: String,
    pub label: String,
    pub handlers: Vec<String>,
    pub command: Value,
    pub terminal: bool,
}

pub fn all(cfg: &Config) -> Vec<Action> {
    let mut out = vec![];
    if let Some(obj) = cfg.raw.get("actions").as_obj() {
        for (name, a) in obj {
            if a["command"].is_null() {
                continue;
            }
            let handlers = match a["handlers"].as_str() {
                Some(s) => vec![s.to_string()],
                None => a["handlers"].strings(),
            };
            out.push(Action { name: name.clone(), label: a["label"].str_or(name).to_string(), handlers, command: a["command"].clone(), terminal: a["terminal"].bool_or(false) });
        }
    }
    out
}

pub fn get(cfg: &Config, key: &str) -> Option<Action> {
    let name = key.strip_prefix(PREFIX)?;
    all(cfg).into_iter().find(|a| a.name == name)
}

/// Actions (clé, libellé) applicables au gestionnaire `handler`.
pub fn actions_for(cfg: &Config, handler: &str) -> Vec<(String, String)> {
    all(cfg).into_iter().filter(|a| a.handlers.iter().any(|h| h == "*" || h == handler)).map(|a| (format!("{PREFIX}{}", a.name), a.label)).collect()
}

/// Ajoute les actions personnalisées à une liste, avant « Éjecter ».
pub fn extend(cfg: &Config, handler: &str, actions: &mut Vec<(String, String)>) {
    let extra = actions_for(cfg, handler);
    if extra.is_empty() {
        return;
    }
    let at = actions.iter().position(|(k, _)| k == "eject").unwrap_or(actions.len());
    for (i, a) in extra.into_iter().enumerate() {
        if !actions.iter().any(|(k, _)| *k == a.0) {
            actions.insert(at + i, a);
        }
    }
}

/// Contexte d'exécution.
pub struct Ctx<'a> {
    pub handler: &'a str,
    pub device: Option<&'a str>,
    pub ident: Option<&'a IdentResult>,
    pub tag: Option<&'a str>,
    pub resolution: Option<&'a Resolution>,
    pub existing: Option<&'a str>,
    pub label: Option<&'a str>,
}

fn variables(cfg: &Config, c: &Ctx) -> Vec<(String, String)> {
    let mut env: Vec<(String, String)> = match handlers::get(c.handler) {
        Some(m) => handlers::env_for(&m, cfg, &handlers::CallCtx { action: "custom", device: c.device, ident: c.ident, tag: c.tag, resolution: c.resolution, existing: c.existing, checked: true, ..Default::default() }),
        None => vec![("DL_HANDLER".into(), c.handler.into()), ("DL_ROMS_DIR".into(), cfg.roms_dir().to_string_lossy().into_owned())],
    };
    let mut set = |k: &str, v: Option<String>| {
        if let Some(v) = v.filter(|v| !v.is_empty()) {
            if !env.iter().any(|(x, _)| x == k) {
                env.push((k.into(), v));
            }
        }
    };
    if let Some(d) = c.device.filter(|d| d.starts_with("/dev/")) {
        set("DL_DEVICE", Some(d.into()));
        set("DL_DEVNAME", Some(d.trim_start_matches("/dev/").into()));
        set("DL_MOUNT", crate::device::find_mount_point(d).map(|p| p.to_string_lossy().into_owned()));
    }
    set("DL_LABEL", c.label.map(|s| s.to_string()));
    set("DL_EXISTING", c.existing.map(|s| s.to_string()));
    env
}

/// Ligne de commande finale (avant un éventuel terminal).
pub fn argv(cfg: &Config, a: &Action, c: &Ctx) -> Result<Vec<String>, String> {
    let env = variables(cfg, c);
    let get = |k: &str| env.iter().find(|(x, _)| x == k).map(|(_, v)| v.clone()).unwrap_or_default();
    match &a.command {
        Value::Str(s) => {
            // `env` transmet les variables, y compris à travers un terminal.
            let mut v = vec!["env".to_string()];
            v.extend(env.iter().map(|(k, val)| format!("{k}={val}")));
            v.extend(["sh".to_string(), "-c".to_string(), s.clone()]);
            Ok(v)
        }
        Value::Arr(_) => {
            let vars = [
                ("device", get("DL_DEVICE")),
                ("mount", get("DL_MOUNT")),
                ("existing", get("DL_EXISTING")),
                ("label", get("DL_LABEL")),
                ("name", get("DL_NAME")),
                ("handler", c.handler.to_string()),
                ("roms", get("DL_ROMS_DIR")),
            ];
            let v: Vec<String> = a.command.strings().iter().map(|t| crate::util::render(t, &vars)).map(|x| if x.starts_with('~') { paths::expand(&x).to_string_lossy().into_owned() } else { x }).collect();
            if v.is_empty() || v[0].is_empty() {
                return Err(format!("action {} : commande vide", a.name));
            }
            Ok(v)
        }
        _ => Err(format!("action {} : « command » doit être une chaîne ou une liste", a.name)),
    }
}

/// Lance l'action, détachée (dans un terminal si `terminal = true`).
pub fn run(cfg: &Config, a: &Action, c: &Ctx) -> Result<Vec<String>, String> {
    let cmd = argv(cfg, a, c)?;
    let log = paths::log_dir().join(format!("action-{}.log", a.name));
    let full = if a.terminal { crate::terminal::command(cfg, &cmd).unwrap_or_else(|| cmd.clone()) } else { cmd };
    crate::generic::spawn_detached(&full, &log)?;
    Ok(full)
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn declared_actions() {
        let cfg = Config { errors: vec![], raw: crate::toml::parse("[actions.rip-cd]\nlabel = \"Ripper le CD audio\"\nhandlers = [\"cdda\"]\ncommand = \"encbd \\\"$DL_DEVICE\\\" ~/Musique\"\nterminal = true\n[actions.copie]\nhandlers = \"*\"\ncommand = [\"cp\", \"-r\", \"{mount}\", \"~/Copies/{label}\"]\n").unwrap() };
        let mut acts = vec![("play-disc".to_string(), "Lire".to_string()), ("eject".to_string(), "Éjecter".to_string())];
        extend(&cfg, "cdda", &mut acts);
        let keys: Vec<&str> = acts.iter().map(|(k, _)| k.as_str()).collect();
        assert_eq!(keys, ["play-disc", "custom:copie", "custom:rip-cd", "eject"]);
        assert_eq!(actions_for(&cfg, "psx").len(), 1);
        let a = get(&cfg, "custom:rip-cd").unwrap();
        assert!(a.terminal);
        let ctx = Ctx { handler: "cdda", device: Some("/dev/sr0"), ident: None, tag: None, resolution: None, existing: None, label: Some("ALBUM") };
        let v = argv(&cfg, &a, &ctx).unwrap();
        assert_eq!(v[0], "env");
        assert!(v.contains(&"DL_DEVICE=/dev/sr0".to_string()));
        assert_eq!(&v[v.len() - 3..], ["sh", "-c", "encbd \"$DL_DEVICE\" ~/Musique"]);
        let b = get(&cfg, "custom:copie").unwrap();
        let w = argv(&cfg, &b, &ctx).unwrap();
        assert_eq!(w[0], "cp");
        assert!(w[3].ends_with("/Copies/ALBUM") && !w[3].starts_with('~'));
    }
}

#[cfg(test)]
mod toml_tests {
    #[test]
    fn literal_string_command() {
        let v = crate::toml::parse("[actions.rip-cd]\ncommand = 'encbd \"$DL_DEVICE\" ~/Musique'\n").unwrap();
        assert_eq!(v["actions"]["rip-cd"]["command"].as_str(), Some("encbd \"$DL_DEVICE\" ~/Musique"));
    }
}
