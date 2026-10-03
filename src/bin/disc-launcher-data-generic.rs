//! Gestionnaire générique des disques de données simples : musique seule
//! (`data-audio`) ou vidéo seule (`data-video`).
//!
//!   disc-launcher-data-generic describe [--id data-audio|data-video] [--disc /dev/srN]
//!   disc-launcher-data-generic play     [--id …] [--disc /dev/srN] [--no-check]
//!
//! Monte le disque au besoin (udisks2), écrit la liste des fichiers en M3U
//! et la confie au lecteur choisi dans `[media]` (clé `data-audio` ou
//! `data-video` de chaque lecteur). Gabarits : `{playlist}`, `{first}`,
//! `{count}`, `{mount}`, `{device}` et les variables `DL_*`.

use disclauncher::config::Config;
use disclauncher::handlers::{RC_NOT_APPLICABLE, RC_OK, RC_USER_ERROR};
use disclauncher::json::Value;
use disclauncher::{data, identify, jobj, paths};

fn out(v: &Value, code: i32) -> ! {
    println!("{}", v.to_json());
    std::process::exit(code);
}

fn main() {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let opt = |f: &str| args.iter().position(|a| a == f).and_then(|i| args.get(i + 1)).cloned();
    let verb = args.iter().find(|a| matches!(a.as_str(), "describe" | "play" | "dump-plan" | "convert-plan")).cloned().unwrap_or_else(|| "play".into());
    let no_check = args.iter().any(|a| a == "--no-check") || std::env::var("DL_CHECKED").is_ok_and(|v| v == "1");
    let cfg = Config::load();
    disclauncher::log::init(disclauncher::log::options_from_config(&cfg, Some(&paths::log_dir().join("data.log")), false));
    let dev = opt("--disc").or_else(|| std::env::var("DL_DEVICE").ok()).or_else(|| args.iter().find(|a| a.starts_with("/dev/")).cloned());
    let dev = dev.unwrap_or_else(|| disclauncher::device::list_drives().into_iter().next().unwrap_or_else(|| "/dev/sr0".into()));
    let mut id = opt("--id").or_else(|| std::env::var("DL_MEDIA").ok());

    if !matches!(verb.as_str(), "describe" | "play") {
        out(&jobj! {"error" => format!("{verb} : non pris en charge pour les disques de données")}, RC_NOT_APPLICABLE);
    }
    if verb == "play" && (!no_check || id.is_none()) {
        match identify::identify_device(&dev, &identify::Options { profile: cfg.drive_profile(&dev), ..Default::default() }) {
            Ok(r) => {
                let tag = r.primary().map(|m| m.tag.clone()).unwrap_or_else(|| "data".into());
                match identify::media_id(&tag).filter(|m| m.starts_with("data-")) {
                    Some(m) => id = Some(m.to_string()),
                    None => out(&jobj! {"error" => format!("ce disque n'est pas un disque de musique ou de vidéo en fichiers ({tag})"), "tag" => tag}, RC_NOT_APPLICABLE),
                }
            }
            Err(e) => out(&jobj! {"error" => format!("{dev} : {e}")}, RC_USER_ERROR),
        }
    }
    let Some(id) = id else { out(&jobj! {"error" => "type inconnu : --id data-audio|data-video"}, RC_USER_ERROR) };
    if verb == "describe" {
        out(&data::describe(&cfg, &id, &dev), RC_OK);
    }
    match data::play(&cfg, &id, &dev) {
        Ok(v) => {
            disclauncher::dl_log!(info, "data", "lecteur lancé", "drive" => dev, "media" => id, "player" => v["player"].str_or(""));
            out(&v, RC_OK)
        }
        Err(e) => {
            disclauncher::dl_log!(error, "data", e.clone(), "drive" => dev);
            out(&jobj! {"error" => e}, RC_USER_ERROR)
        }
    }
}
