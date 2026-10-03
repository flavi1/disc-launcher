//! Commande de contrôle `disc-launcher`.

use disclauncher::collection::Collection;
use disclauncher::config::Config;
use disclauncher::json::Value;
use disclauncher::log::parse_line;
use disclauncher::{control, daemon, device, handlers, identify, jobj, jobs, paths, refdb};
use std::io::{BufRead, Seek, SeekFrom};
use std::path::PathBuf;
use std::time::Duration;

const USAGE: &str = "usage : disc-launcher <commande> [options]

  status [--json]                    lecteurs, disques identifiés, tâches
  identify [/dev/srN | --image f]    identifie et prédit le fichier (sans le démon)
  refresh [/dev/srN]                 le démon réidentifie le disque et repropose
  run <action> [/dev/srN|URI] [--target <id>]
                                     exécute une action sans notification
                                     (kodi, play-existing, play-disc, dump, redump,
                                      dump-then-play, verify, convert, complete-game)
  jobs                               tâches récentes
  cancel <id>                        annule une tâche
  log [-f] [--job <id>] [--level <niveau>] [--drive <dev>] [--json]
  watch [id]                         suit une tâche (la dernière par défaut) jusqu'à sa fin
  rom info <fichier>                 cartouche (Retrode) : système, en-tête, empreinte, nom
  rom copy <source> <destination>    copie normalisée d'une ROM (N64 en z64…)
  collection scan [--full-hash]      réindexe ~/ROMs, retrouve les fichiers renommés
  collection list | prune [--days N]  liste l'index | supprime les entrées absentes
  collection rename [--dry-run]      donne leur nom canonique aux fichiers au nom provisoire
  refdb import <fichier.dat|dossier>… [--system <id>]
  refdb [status]                     contenu de la base de référence, par système
  refdb fetch [système… | all]       télécharge noms et numéros de série (libretro) ; tous par défaut
  handlers [id]                      exécutable retenu pour chaque verbe
  reload                             relit la configuration du démon
  doctor                             capacités, lecteurs, gestionnaires, outils
";

fn die(msg: &str) -> ! {
    eprintln!("disc-launcher : {msg}");
    std::process::exit(1);
}

fn req(v: Value) -> Value {
    match control::request(&v, Duration::from_secs(30)) {
        Ok(r) => {
            if !r["ok"].bool_or(false) {
                die(r["error"].str_or("erreur"));
            }
            r
        }
        Err(e) => die(&e),
    }
}

fn first_drive() -> String {
    device::list_drives().into_iter().next().unwrap_or_else(|| "/dev/sr0".into())
}

fn main() {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let flag = |f: &str| args.iter().any(|a| a == f);
    let opt = |f: &str| args.iter().position(|a| a == f).and_then(|i| args.get(i + 1)).cloned();
    let positional: Vec<String> = {
        let mut v = vec![];
        let mut skip = false;
        for a in args.iter().skip(1) {
            if skip {
                skip = false;
                continue;
            }
            if matches!(a.as_str(), "--target" | "--job" | "--level" | "--drive" | "--image" | "--system" | "--profile" | "--days") {
                skip = true;
                continue;
            }
            if !a.starts_with('-') {
                v.push(a.clone());
            }
        }
        v
    };
    let cmd = args.first().map(|s| s.as_str()).unwrap_or("help");
    match cmd {
        "status" => {
            let r = req(jobj! {"cmd" => "status"});
            if flag("--json") {
                println!("{}", r.to_pretty());
                return;
            }
            for d in r["drives"].as_arr() {
                println!("{}  [{}]", d["device"].str_or("?"), d["state"].str_or("?"));
                if let Some(m) = d.path("identification.matches").as_arr().iter().find(|m| m["role"].as_str() == Some("primary")) {
                    println!("  disque : {} ({})  {}", m["tag"].str_or("?"), m["confidence"].str_or("?"), m.path("identity.key").str_or(""));
                }
                let o = &d["offer"];
                if !o.is_null() {
                    if let Some(p) = o.path("target.path").as_str() {
                        println!("  cible  : {p}  [{}]", o.path("resolution.confidence").str_or("?"));
                    }
                    if let Some(s) = o.path("situation.kind").as_str() {
                        println!("  état   : {s}");
                    }
                    println!("  actions: {}", o["actions"].strings().join(", "));
                    for w in o["warnings"].strings() {
                        println!("  ⚠ {w}");
                    }
                }
                if let Some(j) = d["job"].as_str() {
                    println!("  tâche  : {j}");
                }
            }
            if r["drives"].as_arr().is_empty() {
                println!("aucun lecteur connu du démon");
            }
        }
        "identify" | "predict" => {
            let cfg = Config::load();
            let (r, dev) = if let Some(img) = opt("--image") {
                let opts = identify::Options { profile: opt("--profile").unwrap_or_else(|| "omnidrive".into()), ..Default::default() };
                (identify::identify_image(std::path::Path::new(&img), &opts), img)
            } else {
                let dev = positional.first().cloned().map(|d| device::device_from_uri(&d).unwrap_or(d)).unwrap_or_else(first_drive);
                let opts = identify::Options { profile: opt("--profile").unwrap_or_else(|| cfg.drive_profile(&dev)), ..Default::default() };
                (identify::identify_device(&dev, &opts), dev)
            };
            let r = r.unwrap_or_else(|e| die(&format!("{dev} : {e}")));
            let offer = daemon::prepare(&cfg, &dev, &r).ok();
            if flag("--json") {
                println!("{}", jobj! {"identification" => r.to_value(), "offer" => offer.as_ref().map(daemon::offer_value)}.to_pretty());
                return;
            }
            for m in &r.matches {
                println!("{} {:<26} {:<9} {}", if m.primary { "*" } else { " " }, m.tag, m.confidence.name(), m.identity.key());
            }
            for w in &r.warnings {
                println!("⚠ {w}");
            }
            if let Some(o) = offer {
                let (summary, body) = daemon::offer_text(&cfg, &o, Some(&r));
                println!("\n{summary}\n{body}");
                println!("actions : {}", o.actions.iter().map(|a| a.0.as_str()).collect::<Vec<_>>().join(", "));
            }
        }
        "refresh" => {
            let dev = positional.first().cloned().unwrap_or_else(first_drive);
            req(jobj! {"cmd" => "identify", "device" => dev});
            println!("réidentification demandée");
        }
        "run" => {
            let action = positional.first().cloned().unwrap_or_else(|| die("action attendue"));
            let dev = positional.get(1).cloned().map(|d| device::device_from_uri(&d).unwrap_or(d)).unwrap_or_else(first_drive);
            let target = opt("--target");
            if control::daemon_running() {
                let mut q = jobj! {"cmd" => "run", "action" => action.clone(), "device" => dev.clone()};
                if let Some(t) = &target {
                    q.set("target", t.clone());
                }
                match control::request(&q, Duration::from_secs(30)) {
                    Ok(r) if r["ok"].bool_or(false) => return,
                    Ok(r) if r["error"].as_str().is_some_and(|e| e.contains("pas encore identifié")) && action == "play-disc" => {}
                    Ok(r) => die(r["error"].str_or("erreur")),
                    Err(_) => {}
                }
            }
            // Sans démon (coquilles natives) : lecture d'un média par le gestionnaire
            // multimédia, qui identifie le disque et refuse s'il s'agit d'un jeu.
            if action == "play-disc" {
                match handlers::call(handlers::GENERIC_MEDIA, "play", &["--disc".into(), dev], &Value::obj(), &[], Duration::from_secs(120)) {
                    Ok(r) if r.code == 0 => println!("{}", r.out["player_name"].str_or(r.out["message"].str_or("ok"))),
                    Ok(r) => die(r.out["error"].str_or(r.stderr.trim())),
                    Err(e) => die(&e),
                }
            } else {
                die("le démon ne tourne pas (disc-launcherd)");
            }
        }
        "jobs" => {
            for (id, st) in jobs::list().into_iter().take(20) {
                println!("{id}  {:<11} {:>3}%  {:<6} {}  {}", st["status"].str_or("?"), st["progress"].i64_or(0), st["kind"].str_or(""), st["title"].str_or(""), st["message"].str_or(""));
            }
        }
        "cancel" => {
            let id = positional.first().cloned().unwrap_or_else(|| die("identifiant attendu"));
            jobs::cancel(&id).unwrap_or_else(|e| die(&e));
            println!("annulation demandée");
        }
        "watch" => watch(positional.first().cloned()),
        "rom" => match positional.first().map(|s| s.as_str()) {
            Some("copy") if positional.len() == 3 => match disclauncher::cart::copy(std::path::Path::new(&positional[1]), std::path::Path::new(&positional[2])) {
                Ok(h) => println!("copié : {} (sha1 {h})", positional[2]),
                Err(e) => die(&format!("{} : {e}", positional[1])),
            },
            Some("info") if positional.len() == 2 => {
                let (r, _) = disclauncher::cart::inspect(std::path::Path::new(&positional[1])).unwrap_or_else(|e| die(&e.to_string()));
                let db = refdb::RefDb::load();
                let name = db.by_sha1(&r.system, std::slice::from_ref(&r.sha1)).map(|g| g.name.clone());
                println!("système : {}\ntitre   : {}\ncode    : {}\nrégion  : {}\ntaille  : {}\nsha1    : {}\nnom     : {}",
                    r.system, r.title.unwrap_or_default(), r.code.unwrap_or_default(), r.region.unwrap_or_default(), r.size, r.sha1,
                    name.unwrap_or_else(|| "inconnu de la base (disc-launcher refdb fetch)".into()));
            }
            _ => die("usage : disc-launcher rom info <fichier> | rom copy <source> <destination>"),
        },
        "log" => show_log(opt("--job"), opt("--level"), opt("--drive"), flag("-f"), flag("--json")),
        "collection" => {
            let cfg = Config::load();
            let col = Collection::open().unwrap_or_else(|e| die(&e));
            match positional.first().map(|s| s.as_str()) {
                Some("scan") => {
                    let r = col.scan(&cfg.roms_dir(), &handlers::load_all(), flag("--full-hash"));
                    for (a, b) in &r.relocated {
                        println!("renommé : {a}\n      → {b}");
                    }
                    println!("{} entrées : {} ajoutées, {} mises à jour, {} renommages retrouvés, {} absentes ({})", col.entries().len(), r.added, r.updated, r.relocated.len(), r.missing, cfg.roms_dir().display());
                }
                Some("prune") => {
                    let days = opt("--days").and_then(|d| d.parse().ok()).unwrap_or(30);
                    println!("{} entrées absentes supprimées", col.prune_missing(days));
                }
                Some("rename") => collection_rename(&cfg, &col, flag("--dry-run")),
                _ => {
                    for e in col.entries() {
                        let state = if e.missing_since.is_some() { "absent" } else { e.verified.as_deref().unwrap_or("-") };
                        println!("{:<10} {:<8} {:>12}  {}  {}", e.system, state, e.size, e.path, e.canonical_name.as_deref().unwrap_or(""));
                    }
                }
            }
        }
        "refdb" => {
            if positional.first().map(|s| s.as_str()) == Some("fetch") {
                refdb_fetch(&positional[1..]);
                return;
            }
            if matches!(positional.first().map(|s| s.as_str()), None | Some("status")) {
                let db = refdb::RefDb::load();
                let counts = db.counts();
                if counts.is_empty() {
                    println!("base de référence vide : disc-launcher refdb fetch");
                }
                for (sys, (n, with_serial)) in counts {
                    println!("{sys:<14} {n:>6} jeux  {with_serial:>6} avec numéro de série");
                }
                println!("({})", refdb::db_path().display());
                return;
            }
            if positional.first().map(|s| s.as_str()) != Some("import") {
                die("usage : disc-launcher refdb import <fichier.dat|dossier>… [--system <id>]\n        disc-launcher refdb fetch [système…]");
            }
            let mut files: Vec<PathBuf> = positional.iter().skip(1).map(PathBuf::from).collect();
            if files.is_empty() {
                files.push(Config::load().reference_dats_dir());
            }
            let sys_opt = opt("--system");
            match refdb::import(&files, sys_opt.as_deref()) {
                Ok((n, msgs)) => {
                    for m in msgs {
                        println!("{m}");
                    }
                    println!("{n} jeux importés dans {}", refdb::db_path().display());
                }
                Err(e) => die(&e),
            }
        }
        "handlers" => {
            let cfg = Config::load();
            let all = handlers::load_all();
            let only = positional.first().cloned();
            for m in all.values().filter(|m| only.as_ref().map_or(true, |o| *o == m.id)) {
                let ex = |v: &str| {
                    let e = handlers::executable_for(m, &cfg, v);
                    let found = paths::which(&e).is_some();
                    format!("{e}{}", if found { "" } else { " (introuvable)" })
                };
                println!("{:<14} {:<7} play: {}  describe: {}  dump-plan: {}", m.id, m.kind(), ex("play"), ex("describe"), ex("dump-plan"));
            }
        }
        "reload" => {
            req(jobj! {"cmd" => "reload"});
            println!("configuration rechargée");
        }
        "doctor" => doctor(),
        "help" | "-h" | "--help" => print!("{USAGE}"),
        "--version" => println!("disc-launcher {}", disclauncher::VERSION),
        c => die(&format!("commande inconnue : {c}\n{USAGE}")),
    }
}

fn show_log(job: Option<String>, level: Option<String>, drive: Option<String>, follow: bool, as_json: bool) {
    let path = match &job {
        Some(id) => jobs::job_dir(id).join("job.log"),
        None => disclauncher::log::default_daemon_log(),
    };
    let max = level.as_deref().map(disclauncher::log::Level::parse).unwrap_or(disclauncher::log::Level::Trace);
    let print = |line: &str| {
        let f = parse_line(line);
        let get = |k: &str| f.iter().find(|(a, _)| a == k).map(|(_, v)| v.as_str());
        if disclauncher::log::Level::parse(get("level").unwrap_or("info")) > max {
            return;
        }
        if let Some(d) = &drive {
            if get("drive").map_or(true, |x| !x.ends_with(d.trim_start_matches("/dev/"))) {
                return;
            }
        }
        if as_json {
            let mut o = Value::obj();
            for (k, v) in &f {
                o.set(k, v.clone());
            }
            println!("{}", o.to_json());
        } else {
            println!("{line}");
        }
    };
    let file = std::fs::File::open(&path).unwrap_or_else(|e| die(&format!("{} : {e}", path.display())));
    let mut r = std::io::BufReader::new(file);
    let mut line = String::new();
    loop {
        line.clear();
        let n = r.read_line(&mut line).unwrap_or(0);
        if n == 0 {
            if !follow {
                break;
            }
            std::thread::sleep(Duration::from_millis(500));
            // rotation : rouvrir si le fichier a rétréci
            let pos = r.stream_position().unwrap_or(0);
            if std::fs::metadata(&path).map(|m| m.len() < pos).unwrap_or(false) {
                if let Ok(f) = std::fs::File::open(&path) {
                    r = std::io::BufReader::new(f);
                    let _ = r.seek(SeekFrom::Start(0));
                }
            }
            continue;
        }
        print(line.trim_end());
    }
}

fn doctor() {
    let cfg = Config::load();
    println!("disc-launcher {}", disclauncher::VERSION);
    for e in &cfg.errors {
        println!("✗ configuration : {e}");
    }
    println!("mode           : {}", cfg.mode().name());
    println!("dossier ROMs   : {}{}", cfg.roms_dir().display(), if disclauncher::config::esde_rom_directory().is_some() && cfg.raw.path("general.roms_dir").str_or("").is_empty() { " (réglage ES-DE)" } else { "" });
    println!("démon          : {}", if control::daemon_running() { "en marche" } else { "arrêté" });
    println!("journal        : {}", disclauncher::log::default_daemon_log().display());
    let logind = disclauncher::dbus::session_active();
    println!("logind         : {}", match logind { Some(true) => "session active", Some(false) => "session inactive", None => "indisponible (repli : droits sur le lecteur)" });
    println!("uevents noyau  : {}", if disclauncher::sys::uevent_socket().map(disclauncher::sys::close_fd).is_ok() { "oui" } else { "non (sondage seul)" });
    println!("notifications  : {}", match disclauncher::dbus::Connection::open(disclauncher::dbus::Bus::Session) { Ok(_) => "bus de session joignable", Err(_) => "bus de session injoignable" });
    println!("dialogue       : {}", disclauncher::notify::dialog_tool().unwrap_or("aucun"));
    println!("pkexec         : {}", if paths::which("pkexec").is_some() { "oui" } else { "non" });
    {
        // Groupe dispensant du mot de passe pour les lectures privilégiées (règle polkit).
        let user = std::env::var("USER").unwrap_or_default();
        let groups = std::fs::read_to_string("/etc/group").unwrap_or_default();
        let line = groups.lines().find(|l| l.starts_with("disc-launcher:"));
        let member = line.is_some_and(|l| l.rsplit(':').next().unwrap_or("").split(',').any(|u| u == user));
        println!("groupe         : {}", match (line, member) {
            (None, _) => "disc-launcher absent (sudo make install le crée)".to_string(),
            (Some(_), true) => "membre de disc-launcher (dumps sans mot de passe)".to_string(),
            (Some(_), false) => format!("non membre de disc-launcher : sudo usermod -aG disc-launcher {user}, puis reconnexion"),
        });
    }
    let db = refdb::RefDb::load();
    println!("base Redump    : {} jeux ({})", db.games.len(), refdb::db_path().display());
    println!("\nlecteurs :");
    for d in device::list_drives() {
        let info = device::cdrom::open_dev(&d).ok().and_then(|f| device::cdrom::inquiry(&f).ok());
        let status = device::cdrom::drive_status(&d).map(|s| format!("{s:?}")).unwrap_or_else(|e| e.to_string());
        match info {
            Some(i) => {
                let prof = identify::profiles::resolve(&cfg.drive_profile(&d), Some(&i));
                println!("  {d}  {} {} ({})  profil : {prof}  état : {status}", i.vendor, i.model, i.firmware);
                for (c, note) in identify::profiles::capabilities(&i) {
                    println!("      compatible {c} : {note}");
                }
            }
            None => println!("  {d}  (illisible)  état : {status}"),
        }
    }
    println!("\ngestionnaires :");
    let all = handlers::load_all();
    for m in all.values() {
        let dstat = m.dump_status(None);
        let mut missing: Vec<String> = m.required_commands().into_iter().filter(|c| paths::which(c).is_none()).collect();
        missing.extend(dstat.missing.iter().filter(|t| !missing.contains(t)).cloned().collect::<Vec<_>>());
        let play = handlers::executable_for(m, &cfg, "play");
        if paths::which(&play).is_none() {
            missing.insert(0, play.clone());
        }
        let runner = if m.is_media() {
            disclauncher::media::choose(&cfg, &m.id, &disclauncher::media::vars("/dev/sr0")).map(|c| format!("lecteur {}", c.player)).unwrap_or_else(|| "aucun lecteur".into())
        } else if handlers::is_generic(&play) {
            disclauncher::generic::emulator(m, &cfg)
                .map(|(n, e)| {
                    let cmd = disclauncher::generic::emulator_command(m, &cfg, e, "existing");
                    match cmd.first().and_then(|c| paths::which(c)) {
                        Some(p) => format!("{n} ({})", p.file_name().map(|f| f.to_string_lossy().into_owned()).unwrap_or_default()),
                        None => format!("{n} (absent)"),
                    }
                })
                .unwrap_or_else(|| "-".into())
        } else {
            std::path::Path::new(&play).file_name().map(|s| s.to_string_lossy().into_owned()).unwrap_or(play.clone())
        };
        println!("  {:<14} {:<30} {:<6} {:<28} {}", m.id, m.name(), m.kind(), runner, {
            let mut st = if missing.is_empty() { "✓".to_string() } else { format!("manque : {}", missing.join(", ")) };
            let fb = dstat.fallbacks();
            if !fb.is_empty() {
                st.push_str(&format!("  (dump de repli : {})", fb.join(", ")));
            }
            st
        });
    }
}

/// Suit une tâche en clair (ouvert dans un terminal par le démon) : sortie des
/// outils, étapes, puis le résultat. Fermer ne l'interrompt pas.
fn watch(id: Option<String>) {
    let id = id.or_else(|| jobs::list().into_iter().next().map(|(i, _)| i)).unwrap_or_else(|| die("aucune tâche"));
    let plan = jobs::read_plan(&id).unwrap_or_else(|| die(&format!("tâche inconnue : {id}")));
    println!("disc-launcher — {} : {}", plan["kind"].str_or("tâche"), plan["title"].str_or(""));
    if let Some(p) = plan["dump_plan"].as_str() {
        println!("méthode : {p}{}", if plan["exact"].bool_or(true) { "" } else { " (repli, image non conforme Redump)" });
    }
    println!("Fermer cette fenêtre n'interrompt pas la tâche ; pour l'annuler : disc-launcher cancel {id}\n");
    let path = jobs::job_dir(&id).join("job.log");
    let start = std::time::Instant::now();
    while !path.exists() && start.elapsed() < Duration::from_secs(15) {
        std::thread::sleep(Duration::from_millis(200));
    }
    let print = |line: &str| {
        let f = parse_line(line);
        let get = |k: &str| f.iter().find(|(a, _)| a == k).map(|(_, v)| v.clone());
        let msg = get("msg").unwrap_or_default();
        if get("tool").is_some() {
            println!("{msg}");
            return;
        }
        let time = get("ts").and_then(|t| t.get(11..19).map(|s| s.to_string())).unwrap_or_default();
        let mark = match get("level").as_deref() {
            Some("error") => "✗ ",
            Some("warn") => "⚠ ",
            _ => "",
        };
        let extra = get("cmd").map(|c| format!("\n    $ {c}")).unwrap_or_default();
        println!("[{time}] {mark}{msg}{extra}");
    };
    let mut reader = std::fs::File::open(&path).ok().map(std::io::BufReader::new);
    let mut line = String::new();
    loop {
        let mut got = false;
        if let Some(r) = reader.as_mut() {
            loop {
                line.clear();
                if r.read_line(&mut line).unwrap_or(0) == 0 {
                    break;
                }
                got = true;
                print(line.trim_end());
            }
        } else if path.exists() {
            reader = std::fs::File::open(&path).ok().map(std::io::BufReader::new);
            continue;
        }
        let st = jobs::read_state(&id).unwrap_or(Value::Null);
        let active = jobs::is_active(&st) && (jobs::alive(&id) || st["status"].as_str() == Some("pending") && start.elapsed() < Duration::from_secs(30));
        if !active && !got {
            println!();
            match st["status"].as_str() {
                Some("done") => println!("✓ Terminé : {}", st["result"]["path"].str_or("")),
                Some("cancelled") => println!("Annulé."),
                Some(s) => println!("✗ Échec ({s}) : {}", st["message"].str_or("?")),
                None => println!("✗ État de la tâche illisible."),
            }
            break;
        }
        if !got {
            std::thread::sleep(Duration::from_millis(300));
        }
    }
    if disclauncher::sys::stdin_is_tty() {
        use std::io::Write;
        println!();
        let mut kept = false;
        for left in (1..=10).rev() {
            print!("\rLe terminal se fermera dans {left:>2} secondes. Appuyez sur une touche pour le préserver. ");
            let _ = std::io::stdout().flush();
            if disclauncher::sys::wait_key(1000) {
                kept = true;
                break;
            }
        }
        if kept {
            println!("\nTerminal préservé. Appuyez sur une touche pour le fermer.");
            disclauncher::sys::wait_key(-1);
        } else {
            println!();
        }
    }
}

/// Télécharge les métadonnées libretro (noms Redump + numéros de série) et les importe.
fn refdb_fetch(systems: &[String]) {
    let all = refdb::LIBRETRO_DATS;
    let every = systems.is_empty() || systems.iter().any(|s| s == "all" || s == "*");
    let unknown: Vec<&String> = systems.iter().filter(|s| *s != "all" && *s != "*" && !all.iter().any(|(id, _)| id == s)).collect();
    if !every && !unknown.is_empty() {
        die(&format!(
            "système inconnu : {} ; disponibles : {} (ou rien, « all », « '*' » pour tous ; un * sans guillemets est remplacé par le shell par les fichiers du dossier courant)",
            unknown.iter().map(|s| s.as_str()).collect::<Vec<_>>().join(", "),
            all.iter().map(|x| x.0).collect::<Vec<_>>().join(", ")
        ));
    }
    let wanted: Vec<&(&str, &str)> = if every { all.iter().collect() } else { all.iter().filter(|(id, _)| systems.iter().any(|s| s == id)).collect() };
    if paths::which("curl").is_none() {
        die("curl est nécessaire pour télécharger (ou utilisez refdb import sur un fichier téléchargé)");
    }
    let dir = paths::user_cache_dir().join("dat");
    let _ = std::fs::create_dir_all(&dir);
    let mut files = vec![];
    for (id, name) in wanted {
        let url = refdb::libretro_url(name);
        let file = name.rsplit('/').next().unwrap_or(name);
        let out = dir.join(format!("{file}.dat"));
        print!("{id:<14} {file} … ");
        let _ = std::io::Write::flush(&mut std::io::stdout());
        let st = std::process::Command::new("curl").args(["-fsSL", "--retry", "2", "-o"]).arg(&out).arg(&url).status();
        match st {
            Ok(s) if s.success() => {
                println!("ok");
                files.push(out);
            }
            _ => println!("échec ({url})"),
        }
    }
    if files.is_empty() {
        die("aucun fichier téléchargé");
    }
    match refdb::import(&files, None) {
        Ok((n, _)) => println!("{n} jeux importés dans {}", refdb::db_path().display()),
        Err(e) => die(&e),
    }
    println!("Fichiers déjà dumpés sous un nom provisoire : disc-launcher collection rename");
}

/// Renomme les fichiers dont le nom n'a pas été établi par la base de
/// référence (nom provisoire, ou probable non vérifié) quand la base, enrichie
/// depuis, connaît leur numéro de série.
fn collection_rename(cfg: &Config, col: &Collection, dry: bool) {
    let db = refdb::RefDb::load();
    let all = handlers::load_all();
    let mut n = 0;
    for e in col.entries() {
        if e.missing_since.is_some() || e.verified.as_deref() == Some("ok") {
            continue;
        }
        let Some(key) = &e.key else { continue };
        // Clé : système:série:dN:région
        let parts: Vec<&str> = key.split(':').collect();
        if parts.len() < 4 || parts[1].starts_with("toc-") || parts[1] == "unknown" {
            continue;
        }
        let id = disclauncher::identity::Identity {
            system: parts[0].to_string(),
            serial: Some(parts[1].to_string()),
            disc: parts[2].trim_start_matches('d').parse().ok(),
            region: Some(parts[3].to_string()).filter(|r| r != "-"),
            ..Default::default()
        };
        let Some(name) = db.name_for_serial(&id) else { continue };
        if e.canonical_name.as_deref() == Some(name.as_str()) {
            continue;
        }
        let Some(m) = all.get(&id.system) else { continue };
        let res = disclauncher::naming::resolution_from_name(&name, disclauncher::naming::NameConfidence::Probable, "redump-serial", &id, Some(&db));
        let target = disclauncher::naming::build_target(cfg, m, &id, &res);
        let cur = PathBuf::from(&e.path);
        let ext = cur.extension().map(|x| x.to_string_lossy().into_owned()).unwrap_or_default();
        let newp = target.path.with_extension(&ext);
        if cur.parent() != newp.parent() {
            println!("ignoré (rangement différent, renommez à la main) : {} → {}", cur.display(), newp.display());
            continue;
        }
        if newp == cur {
            continue;
        }
        if newp.exists() {
            println!("ignoré (existe déjà) : {}", newp.display());
            continue;
        }
        println!("{} → {}", cur.file_name().unwrap_or_default().to_string_lossy(), newp.file_name().unwrap_or_default().to_string_lossy());
        if !dry {
            if let Err(err) = std::fs::rename(&cur, &newp) {
                println!("  échec : {err}");
                continue;
            }
            col.set_canonical(e.id, &newp, &name, "redump-serial");
        }
        n += 1;
    }
    println!("{n} fichier(s) {}", if dry { "à renommer (--dry-run : rien n'a été modifié)" } else { "renommé(s)" });
}
