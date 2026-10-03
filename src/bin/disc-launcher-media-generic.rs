//! Gestionnaire multimédia générique (CD audio, DVD/Blu-ray vidéo…).
//!
//!   disc-launcher-media-generic describe [--id <média>] [--disc /dev/srN]
//!   disc-launcher-media-generic play     [--id <média>] [--disc /dev/srN] [--no-check]
//!
//! Média : `--id`, sinon `DL_MEDIA`, sinon déterminé en identifiant le disque.
//! Avant de lancer quoi que ce soit, revérifie que le disque est bien un média
//! (sauf `DL_CHECKED=1`, posé par le démon) : refus (code 2) si c'est un jeu.

use disclauncher::config::Config;
use disclauncher::handlers::{RC_NOT_APPLICABLE, RC_OK, RC_USER_ERROR};
use disclauncher::json::Value;
use disclauncher::{handlers, identify, jobj, media, paths};

fn out(v: &Value, code: i32) -> ! {
    println!("{}", v.to_json());
    std::process::exit(code);
}

fn main() {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let opt = |f: &str| args.iter().position(|a| a == f).and_then(|i| args.get(i + 1)).cloned();
    let verb = args.iter().find(|a| matches!(a.as_str(), "describe" | "play" | "dump-plan" | "convert-plan" | "post-dump")).cloned().unwrap_or_else(|| "play".into());
    let no_check = args.iter().any(|a| a == "--no-check") || std::env::var("DL_CHECKED").is_ok_and(|v| v == "1");
    let cfg = Config::load();
    disclauncher::log::init(disclauncher::log::options_from_config(&cfg, Some(&paths::log_dir().join("media.log")), false));
    let dev = opt("--disc").or_else(|| std::env::var("DL_DEVICE").ok()).or_else(|| args.iter().find(|a| a.starts_with("/dev/") || a.contains("://")).cloned());
    let dev = dev.map(|d| disclauncher::device::device_from_uri(&d).unwrap_or(d)).unwrap_or_else(|| disclauncher::device::list_drives().into_iter().next().unwrap_or_else(|| "/dev/sr0".into()));
    let mut media_id = opt("--id").or_else(|| std::env::var("DL_MEDIA").ok());

    if !matches!(verb.as_str(), "describe" | "play") {
        out(&jobj! {"error" => format!("{verb} : non pris en charge pour les médias")}, RC_NOT_APPLICABLE);
    }

    // Revérification (et détermination du média si inconnu).
    if verb == "play" && (!no_check || media_id.is_none()) {
        let opts = identify::Options { profile: cfg.drive_profile(&dev), ..Default::default() };
        match identify::identify_device(&dev, &opts) {
            Ok(r) => {
                let tag = r.primary().map(|m| m.tag.clone()).unwrap_or_else(|| "data".into());
                if !identify::is_media(&tag) {
                    disclauncher::dl_log!(warn, "media", "refus : le disque n'est pas un média audio/vidéo", "drive" => dev, "tag" => tag);
                    out(&jobj! {"error" => format!("ce disque n'est pas un média audio/vidéo ({tag})"), "tag" => tag}, RC_NOT_APPLICABLE);
                }
                let found = identify::media_id(&tag).unwrap().to_string();
                if media_id.as_deref().is_some_and(|m| m != found) {
                    disclauncher::dl_log!(info, "media", "média corrigé par la revérification", "from" => media_id.clone().unwrap_or_default(), "to" => found);
                }
                // Variables DL_* pour le lecteur si on n'a pas été appelé par le démon.
                if std::env::var("DL_CHECKED").is_err() {
                    if let Some(m) = handlers::get(&found) {
                        for (k, v) in handlers::env_for(&m, &cfg, &handlers::CallCtx { action: "play", device: Some(&dev), ident: Some(&r), checked: true, ..Default::default() }) {
                            std::env::set_var(k, v);
                        }
                    }
                }
                media_id = Some(found);
            }
            Err(e) => out(&jobj! {"error" => format!("{dev} : {e}")}, RC_USER_ERROR),
        }
    }
    let Some(media_id) = media_id else { out(&jobj! {"error" => "média inconnu : --id <média>"}, RC_USER_ERROR) };

    if verb == "describe" {
        out(&media::describe(&cfg, &media_id, &dev), RC_OK);
    }
    match media::play(&cfg, &media_id, &dev) {
        Ok(v) => {
            disclauncher::dl_log!(info, "media", "lecteur lancé", "drive" => dev, "media" => media_id, "player" => v["player"].str_or(""));
            out(&v, RC_OK)
        }
        Err(e) => {
            disclauncher::dl_log!(error, "media", e.clone(), "drive" => dev);
            out(&jobj! {"error" => e}, RC_USER_ERROR)
        }
    }
}
