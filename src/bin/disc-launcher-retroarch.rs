//! Gestionnaire console RetroArch, utilisable pour un système précis ou pour
//! tous les systèmes :
//!
//!   [handlers.defaults]
//!   console = { play = "disc-launcher-retroarch", "*" = "disc-launcher-generic" }
//!
//! Il ne traite que `describe` et `play` ; les autres verbes (plan de dump,
//! conversion) sont délégués à l'implémentation générique, ce qui permet aussi
//! de le déclarer pour tous les verbes.
//!
//! Système : `DL_SYSTEM` ou `--id`. Copie : `DL_EXISTING` ou `--existing`.
//! Disque : `DL_DEVICE` ou `--disc`.

use disclauncher::config::Config;
use disclauncher::handlers::{self, RC_MISSING, RC_OK, RC_USER_ERROR};
use disclauncher::json::{self, Value};
use disclauncher::{generic, jobj, paths, retroarch, sys};
use std::io::Read;
use std::path::Path;

fn out(v: &Value, code: i32) -> ! {
    println!("{}", v.to_json());
    std::process::exit(code);
}

fn main() {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let opt = |f: &str| args.iter().position(|a| a == f).and_then(|i| args.get(i + 1)).cloned();
    let verb = args.first().cloned().unwrap_or_else(|| "describe".into());
    let cfg = Config::load();
    let Some(system) = opt("--id").or_else(|| std::env::var("DL_SYSTEM").ok()) else { out(&jobj! {"error" => "système inconnu (DL_SYSTEM ou --id)"}, RC_USER_ERROR) };
    let Some(m) = handlers::get(&system) else { out(&jobj! {"error" => format!("aucun manifeste pour {system}")}, RC_USER_ERROR) };
    let input = if sys::stdin_ready(300) {
        let mut s = String::new();
        let _ = std::io::stdin().read_to_string(&mut s);
        json::parse(s.trim()).unwrap_or_else(|_| Value::obj())
    } else {
        Value::obj()
    };
    let existing = opt("--existing").or_else(|| std::env::var("DL_EXISTING").ok()).or_else(|| input["existing"].string());
    let device = opt("--disc").or_else(|| std::env::var("DL_DEVICE").ok()).or_else(|| input["device"].string());
    match verb.as_str() {
        "describe" => {
            let mut d = generic::describe(&m, &cfg, &input);
            let ra = retroarch::command(&cfg);
            let has_ra = paths::which(&ra[0]).is_some();
            let core = retroarch::choose_core(&cfg, &system, "");
            let ok = has_ra && core.is_some();
            if let Some(a) = d.get_mut("actions") {
                a.set("play-existing", ok);
                a.set("play-disc", ok);
            }
            d.set("emulator", format!("retroarch/{}", core.map(|c| c.name).unwrap_or_else(|| "?".into())));
            out(&d, RC_OK)
        }
        "play" => {
            let ex = if device.is_some() && existing.is_none() { None } else { existing.as_deref().map(Path::new) };
            match retroarch::launch_command(&cfg, &system, ex, device.as_deref()) {
                Ok(cmd) => {
                    if paths::which(&cmd[0]).is_none() {
                        out(&jobj! {"error" => format!("introuvable : {}", cmd[0])}, RC_MISSING);
                    }
                    match generic::spawn_detached(&cmd, &paths::log_dir().join(format!("{system}-retroarch.log"))) {
                        Ok(pid) => out(&jobj! {"pid" => pid, "command" => cmd}, RC_OK),
                        Err(e) => out(&jobj! {"error" => e}, RC_USER_ERROR),
                    }
                }
                Err(e) => out(&jobj! {"error" => e}, RC_MISSING),
            }
        }
        "dump-plan" => match generic::dump_plan(&m, &cfg, &input) {
            Ok(v) => out(&v, RC_OK),
            Err(e) => out(&jobj! {"error" => e.message}, e.code),
        },
        "convert-plan" => {
            let i = opt("--input").or_else(|| input["input"].string()).unwrap_or_default();
            let o = opt("--output").or_else(|| input["output"].string()).unwrap_or_default();
            match generic::convert_plan(&m, &i, &o) {
                Ok(v) => out(&v, RC_OK),
                Err(e) => out(&jobj! {"error" => e.message}, e.code),
            }
        }
        v => out(&jobj! {"error" => format!("verbe non pris en charge : {v}")}, handlers::RC_NOT_APPLICABLE),
    }
}
