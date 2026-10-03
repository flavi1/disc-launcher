//! Implémentation générique des verbes d'un gestionnaire console, pilotée par
//! son manifeste : `describe`, `play`, `dump-plan`, `convert-plan`.
//! Utilisée par l'exécutable `disc-launcher-generic` (installé sous les noms
//! `disc-launcher-<id>`).

use crate::config::Config;
use crate::handlers::{Manifest, RC_MISSING, RC_NOT_APPLICABLE, RC_USER_ERROR};
use crate::json::Value;
use crate::{jobj, paths, util};
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};

pub struct HandlerError {
    pub code: i32,
    pub message: String,
}

fn err(code: i32, m: impl Into<String>) -> HandlerError {
    HandlerError { code, message: m.into() }
}

/// Numéro de lecteur pour RetroArch (`/dev/sr0` → 1).
fn drive_number(dev: &str) -> String {
    dev.trim_start_matches("/dev/sr").parse::<u32>().map(|n| (n + 1).to_string()).unwrap_or_else(|_| "1".into())
}

pub fn cores_dir(cfg: &Config, id: &str) -> String {
    cfg.handler(id)["cores_dir"].as_str().or_else(|| cfg.raw.path("handlers.retroarch.cores_dir").as_str()).map(|s| paths::expand(s).to_string_lossy().into_owned()).unwrap_or_else(|| paths::config_home().join("retroarch/cores").to_string_lossy().into_owned())
}

/// Émulateur choisi et ses commandes.
pub fn emulator<'a>(m: &'a Manifest, cfg: &Config) -> Option<(String, &'a Value)> {
    let name = cfg.handler(&m.id)["emulator"].as_str().map(|s| s.to_string()).or_else(|| m.raw.path("emulator.default").string())?;
    let e = m.raw.get("emulators").get(&name);
    if e.is_null() {
        return None;
    }
    Some((name, e))
}

/// Commande `key` (« existing » ou « disc ») de l'émulateur, avec le
/// programme résolu en tête. Ordre : `[handlers.<id>] program` de la
/// configuration (chemin ou nom, `~` accepté), puis le premier nom trouvé de
/// `program` dans le manifeste (chaîne ou liste), puis le premier mot de la
/// commande telle qu'écrite.
pub fn emulator_command(m: &Manifest, cfg: &Config, e: &Value, key: &str) -> Vec<String> {
    let mut cmd = e[key].strings();
    if cmd.is_empty() {
        return cmd;
    }
    let configured = cfg.handler(&m.id)["program"].as_str().map(|p| paths::expand(p).to_string_lossy().into_owned());
    let program = configured.or_else(|| {
        let cands: Vec<String> = match e["program"].as_str() {
            Some(p) => vec![p.to_string()],
            None => e["program"].strings(),
        };
        cands.into_iter().find_map(|c| paths::which(&c).map(|p| if c.contains('/') { p.to_string_lossy().into_owned() } else { c }))
    });
    if let Some(p) = program {
        cmd[0] = p;
    }
    cmd
}

fn command_available(cmd: &[String]) -> bool {
    cmd.first().map_or(false, |c| paths::which(c).is_some())
}

pub fn render_cmd(tpl: &[String], vars: &[(&str, String)]) -> Vec<String> {
    tpl.iter().map(|a| util::render(a, vars)).collect()
}

pub fn describe(m: &Manifest, cfg: &Config, input: &Value) -> Value {
    let profile = input["profile"].as_str().map(|s| s.to_string()).or_else(|| std::env::var("DL_DRIVE_PROFILE").ok()).unwrap_or_else(|| "standard".into());
    let dstat = m.dump_status(Some(&profile));
    let mut missing: Vec<String> = m.required_commands().into_iter().filter(|c| paths::which(c).is_none()).collect();
    missing.extend(dstat.missing.iter().cloned());
    let emu = emulator(m, cfg);
    let (emu_name, play_existing, play_disc) = match &emu {
        Some((n, e)) => {
            let ex = emulator_command(m, cfg, e, "existing");
            let di = emulator_command(m, cfg, e, "disc");
            if !ex.is_empty() && !command_available(&ex) {
                missing.push(ex[0].clone());
            }
            (n.clone(), !ex.is_empty() && command_available(&ex), !di.is_empty() && command_available(&di))
        }
        None => (String::new(), false, false),
    };
    // Lecture confiée à un autre exécutable (DL_PLAY_HANDLER) : considérée disponible.
    let (play_existing, play_disc) = if std::env::var("DL_PLAY_HANDLER").is_ok() { (true, cfg.handler(&m.id)["play_disc"].bool_or(false) || play_disc) } else { (play_existing, play_disc) };
    let profile = profile.as_str();
    let profile_ok = m.requires_profile().is_empty() || m.requires_profile().iter().any(|p| p == profile);
    let dump = m.dumpable() && profile_ok && dstat.any() && m.required_commands().iter().all(|c| paths::which(c).is_some());
    missing.sort();
    missing.dedup();
    jobj! {
        "id" => m.id.clone(), "name" => m.name(), "emulator" => emu_name,
        "actions" => jobj!{"play-existing" => play_existing, "play-disc" => play_disc, "dump" => dump},
        "profile_ok" => profile_ok, "missing" => missing, "dump_fallbacks" => dstat.fallbacks(),
    }
}

/// Lance l'émulateur (copie existante ou disque), détaché ; rend la main.
pub fn play(m: &Manifest, cfg: &Config, existing: Option<&str>, disc: Option<&str>) -> Result<Value, HandlerError> {
    let (name, e) = emulator(m, cfg).ok_or_else(|| err(RC_MISSING, format!("aucun émulateur configuré pour {}", m.id)))?;
    let (tpl, what) = match (existing, disc) {
        (Some(p), _) => {
            // Commande propre à l'extension (ex. « existing_wad » pour un WAD de Wii
            // contenant un jeu N64), sinon « existing ».
            let ext = Path::new(p).extension().map(|x| x.to_string_lossy().to_ascii_lowercase()).unwrap_or_default();
            let key = format!("existing_{ext}");
            if !e[key.as_str()].strings().is_empty() {
                (emulator_command(m, cfg, e, &key), "existing")
            } else {
                (emulator_command(m, cfg, e, "existing"), "existing")
            }
        }
        (None, Some(_)) => (emulator_command(m, cfg, e, "disc"), "disc"),
        _ => return Err(err(RC_USER_ERROR, "--existing ou --disc attendu")),
    };
    if tpl.is_empty() {
        return Err(err(RC_NOT_APPLICABLE, format!("{name} ne sait pas lancer ce type de source ({what})")));
    }
    let dev = disc.unwrap_or("");
    // Variables de gabarit : DL_* de l'environnement ({system}, {name}, {serial}…)
    // plus {path}/{existing}, {device}, {drive_number}, {cores_dir}.
    let mut owned: Vec<(String, String)> = crate::handlers::template_vars_from_env();
    owned.push(("path".into(), existing.unwrap_or("").to_string()));
    owned.push(("existing".into(), existing.unwrap_or("").to_string()));
    owned.push(("device".into(), dev.to_string()));
    owned.push(("drive_number".into(), drive_number(dev)));
    owned.push(("cores_dir".into(), cores_dir(cfg, &m.id)));
    owned.push(("system".into(), m.id.clone()));
    let vars: Vec<(&str, String)> = owned.iter().rev().map(|(k, v)| (k.as_str(), v.clone())).collect();
    let cmd = render_cmd(&tpl, &vars);
    if paths::which(&cmd[0]).is_none() {
        return Err(err(RC_MISSING, format!("émulateur introuvable : {}", cmd[0])));
    }
    let pid = spawn_detached(&cmd, &paths::log_dir().join(format!("{}-{name}.log", m.id))).map_err(|e| err(RC_USER_ERROR, e))?;
    Ok(jobj! {"pid" => pid, "command" => cmd})
}

/// Lance une commande détachée (nouvelle session), sorties vers `log`.
pub fn spawn_detached(cmd: &[String], log: &Path) -> Result<u32, String> {
    use std::os::unix::process::CommandExt;
    if let Some(d) = log.parent() {
        let _ = std::fs::create_dir_all(d);
    }
    let out = std::fs::OpenOptions::new().create(true).append(true).open(log).map_err(|e| e.to_string())?;
    let err2 = out.try_clone().map_err(|e| e.to_string())?;
    // Chemin complet : le programme peut être hors du PATH (~/.local/bin…).
    let prog = paths::which(&cmd[0]).unwrap_or_else(|| PathBuf::from(&cmd[0]));
    let mut c = Command::new(&prog);
    c.args(&cmd[1..]).stdin(Stdio::null()).stdout(out).stderr(err2);
    unsafe {
        c.pre_exec(|| {
            crate::sys::setsid();
            Ok(())
        });
    }
    let child = c.spawn().map_err(|e| format!("{} : {e}", cmd[0]))?;
    let pid = child.id();
    // Le processus est réclamé par un fil pour éviter un zombie.
    std::thread::spawn(move || {
        let mut ch = child;
        let _ = ch.wait();
    });
    Ok(pid)
}

/// Cherche la clé d'un disque chiffré (`<série>.dkey` ou `<nom>.dkey`).
fn find_key(m: &Manifest, cfg: &Config, input: &Value) -> Option<String> {
    let dir = cfg.handler(&m.id)["keys_dir"].as_str().or_else(|| m.raw.path("dump.keys_dir").as_str()).map(paths::expand)?;
    let mut names = vec![];
    if let Some(s) = input.path("identity.serial").as_str() {
        names.push(s.to_string());
        names.push(s.replace('-', ""));
    }
    if let Some(s) = input["stem"].as_str() {
        names.push(s.to_string());
    }
    for n in names {
        for ext in ["dkey", "key"] {
            let p = dir.join(format!("{n}.{ext}"));
            if let Ok(t) = std::fs::read_to_string(&p) {
                let k: String = t.chars().filter(|c| c.is_ascii_hexdigit()).collect();
                if k.len() == 32 {
                    return Some(k);
                }
            }
        }
    }
    None
}

/// Plan de dump : étapes rendues pour ce disque, ce lecteur et cette cible.
pub fn dump_plan(m: &Manifest, cfg: &Config, input: &Value) -> Result<Value, HandlerError> {
    let media = input.path("physical.media").str_or("cd").to_string();
    let profile = input["profile"].str_or("standard").to_string();
    let req = m.requires_profile();
    if !req.is_empty() && !req.iter().any(|p| *p == profile) {
        return Err(err(RC_NOT_APPLICABLE, format!("lecteur incompatible (profil {profile}, requis : {})", req.join(", "))));
    }
    // Plans applicables (support, profil), dans l'ordre de préférence ; le
    // premier dont tous les outils sont installés est retenu.
    let plans = m.raw.path("dump.plan").as_arr();
    let cands: Vec<&Value> = plans
        .iter()
        .filter(|p| {
            let ms = p["media"].strings();
            let ps = p["profiles"].strings();
            (ms.is_empty() || ms.contains(&media)) && (ps.is_empty() || ps.contains(&profile))
        })
        .collect();
    if cands.is_empty() {
        return Err(err(RC_NOT_APPLICABLE, format!("aucun plan de dump pour {} sur {media}", m.id)));
    }
    let chosen = cands.iter().position(|p| Manifest::plan_tools(p).iter().all(|t| paths::which(t).is_some()));
    let plan = cands[chosen.unwrap_or(0)];
    let fallback = chosen.is_some_and(|i| i > 0);
    let device = input["device"].str_or("").to_string();
    let tmp = input["tmp"].str_or("").to_string();
    let stem = input["stem"].str_or("disc").to_string();
    let sgdevice = input["sgdevice"].as_str().map(|s| s.to_string()).or_else(|| crate::device::sg_of(&device)).unwrap_or_else(|| device.clone());
    let mut vars = vec![("device", device.clone()), ("sgdevice", sgdevice), ("tmp", tmp), ("stem", stem.clone()), ("drive_number", drive_number(&device))];
    if m.raw.path("dump.requires_key").bool_or(false) {
        match find_key(m, cfg, input) {
            Some(k) => vars.push(("key", k)),
            None => return Err(err(RC_MISSING, "clé du disque introuvable : ajoutez <série>.dkey dans le dossier des clés")),
        }
    }
    let mut missing = vec![];
    let steps: Vec<Value> = plan["step"]
        .as_arr()
        .iter()
        .map(|s| {
            let mut cmd = render_cmd(&s["command"].strings(), &vars);
            // Réglages propres au lecteur (ex. redumper_args pour un modèle inconnu de redumper).
            if let Some(tool) = cmd.first().map(|c| Path::new(c).file_name().map(|f| f.to_string_lossy().into_owned()).unwrap_or_default()) {
                cmd.extend(cfg.drive_tool_args(&device, &tool));
            }
            if !cmd.is_empty() && paths::which(&cmd[0]).is_none() {
                missing.push(cmd[0].clone());
            }
            jobj! {
                "name" => s["name"].str_or("step"), "command" => cmd,
                "helper" => s["helper"].bool_or(false), "progress" => s["progress"].str_or("generic"),
            }
        })
        .collect();
    if !missing.is_empty() {
        missing.dedup();
        return Err(err(RC_MISSING, format!("outils manquants : {}", missing.join(", "))));
    }
    let r = |list: &Value| -> Vec<String> { list.strings().iter().map(|x| util::render(x, &vars)).collect() };
    // `outputs` et `verify` propres au plan, sinon ceux de [dump].
    let pick = |k: &str| if plan[k].is_null() { m.raw.path(&format!("dump.{k}")).clone() } else { plan[k].clone() };
    Ok(jobj! {
        "steps" => steps,
        "outputs" => r(&pick("outputs")),
        "verify" => r(&pick("verify")),
        "plan" => Manifest::plan_name(plan),
        "fallback" => fallback,
        "exact" => plan["exact"].bool_or(true),
        "note" => plan["note"].clone(),
    })
}

/// Étapes de conversion d'un fichier existant vers le format cible.
pub fn convert_plan(m: &Manifest, input_path: &str, output_path: &str) -> Result<Value, HandlerError> {
    let ext = Path::new(input_path).extension().map(|e| e.to_string_lossy().to_ascii_lowercase()).unwrap_or_default();
    let from = m.raw.path("convert.from").strings();
    if !from.contains(&ext) {
        return Err(err(RC_NOT_APPLICABLE, format!("conversion depuis .{ext} non prévue pour {}", m.id)));
    }
    let tpl = m.raw.path("convert.command").strings();
    if tpl.is_empty() {
        return Err(err(RC_NOT_APPLICABLE, "aucune commande de conversion"));
    }
    let cmd = render_cmd(&tpl, &[("input", input_path.to_string()), ("output", output_path.to_string())]);
    if paths::which(&cmd[0]).is_none() {
        return Err(err(RC_MISSING, format!("outil manquant : {}", cmd[0])));
    }
    Ok(jobj! {"steps" => vec![jobj!{"name" => "convert", "command" => cmd, "helper" => false, "progress" => "chdman"}]})
}

/// Commande de vérification d'une copie existante (selon son extension).
pub fn verify_command(m: &Manifest, path: &str) -> Option<Vec<String>> {
    let ext = Path::new(path).extension()?.to_string_lossy().to_ascii_lowercase();
    let tpl = m.raw.path("verify_cmd").get(&ext).strings();
    if tpl.is_empty() {
        return None;
    }
    Some(render_cmd(&tpl, &[("path", path.to_string())]))
}

pub fn emulator_log(m: &Manifest) -> PathBuf {
    paths::log_dir().join(format!("{}-emulator.log", m.id))
}
