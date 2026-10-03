//! Classificateur en ligne de commande. Ne modifie rien, n'exécute rien du disque.
//!
//!   disc-identify [/dev/srN]            lecteur (défaut : premier lecteur)
//!   disc-identify --image <f.iso|f.cue> image disque
//!   options : --profile <p>  --json (compact)

use disclauncher::config::Config;
use disclauncher::identify::{self, Options};

fn main() {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let mut image = None;
    let mut dev = None;
    let mut profile = None;
    let mut compact = false;
    let mut i = 0;
    while i < args.len() {
        match args[i].as_str() {
            "--image" => {
                image = args.get(i + 1).cloned();
                i += 1;
            }
            "--profile" => {
                profile = args.get(i + 1).cloned();
                i += 1;
            }
            "--json" => compact = true,
            "-h" | "--help" => {
                println!("usage : disc-identify [/dev/srN | --image <fichier>] [--profile <profil>] [--json]");
                return;
            }
            a => dev = Some(a.to_string()),
        }
        i += 1;
    }
    let cfg = Config::load();
    let res = if let Some(img) = image {
        let opts = Options { profile: profile.unwrap_or_else(|| "omnidrive".into()), ..Default::default() };
        identify::identify_image(std::path::Path::new(&img), &opts)
    } else {
        let dev = dev.or_else(|| disclauncher::device::list_drives().into_iter().next()).unwrap_or_else(|| "/dev/sr0".into());
        let opts = Options { profile: profile.unwrap_or_else(|| cfg.drive_profile(&dev)), ..Default::default() };
        identify::identify_device(&dev, &opts)
    };
    match res {
        Ok(r) => {
            let v = r.to_value();
            println!("{}", if compact { v.to_json() } else { v.to_pretty() });
        }
        Err(e) => {
            eprintln!("disc-identify : {e}");
            std::process::exit(1);
        }
    }
}
