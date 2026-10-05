//! Démon de session `disc-launcherd` (section 6) : événements disque, machine
//! à états par lecteur, préparation des propositions, politique, notifications,
//! suivi des tâches, socket de contrôle.

use crate::collection::{self, Check, Collection, Situation};
use crate::config::{Config, Mode, Policy};
use crate::device::cdrom::{self, DriveStatus};
use crate::handlers;
use crate::identify::{self, Confidence, IdentResult};
use crate::json::{self, Value};
use crate::naming::{self, Resolution, Target};
use crate::notify::{self, t, Notification, Notifier, NotifyEvent};
use crate::refdb::RefDb;
use crate::{jobj, jobs, paths, sys};
use std::collections::{BTreeMap, HashMap};
use std::fs::File;
use std::io::{BufRead, BufReader, Write};
use std::os::fd::AsRawFd;
use std::os::unix::net::{UnixListener, UnixStream};
use std::path::{Path, PathBuf};
use std::sync::mpsc::{self, Receiver, Sender};
use std::time::{Duration, Instant};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum DState {
    Empty,
    Settling,
    Identifying,
    Resolving,
    Offering,
    Job,
    Idle,
    Failed,
}

impl DState {
    fn name(self) -> &'static str {
        match self {
            DState::Empty => "vide",
            DState::Settling => "stabilisation",
            DState::Identifying => "identification",
            DState::Resolving => "résolution",
            DState::Offering => "proposition",
            DState::Job => "tâche",
            DState::Idle => "inactif",
            DState::Failed => "échec",
        }
    }
}

/// Proposition préparée pour un disque.
#[derive(Clone, Debug)]
pub struct Offer {
    pub handler: String,
    /// Gestionnaire multimédia (CD audio, DVD vidéo…) plutôt que console.
    pub media: bool,
    pub handler_name: String,
    pub tag: String,
    pub confidence: Confidence,
    pub resolution: Option<Resolution>,
    pub target: Option<Target>,
    pub situation: Option<Situation>,
    pub collision: bool,
    pub describe: Value,
    pub actions: Vec<(String, String)>,
    pub warnings: Vec<String>,
}

struct Drive {
    dev: String,
    state: DState,
    gen: u64,
    due: Option<Instant>,
    settle_until: Option<Instant>,
    ident: Option<IdentResult>,
    offer: Option<Offer>,
    notif: Option<u32>,
    job: Option<String>,
    silent: bool,
    /// Lot multi-disques : système et jeu à dumper automatiquement.
    batch: Option<(String, String)>,
    /// Clé USB ou autre support amovible de données.
    usb: Option<crate::usb::Volume>,
}

enum Event {
    Media(String),
    /// Cartouche (fichier ROM d'une Retrode) apparue ou disparue ; vrai au premier sondage.
    Cart(String, bool),
    Identified(String, u64, Box<Result<IdentResult, String>>),
    Prepared(String, u64, Box<Result<Offer, String>>),
    /// Clé USB apparue, modifiée (montage) ou retirée (None) ; vrai au premier sondage.
    Usb(String, Option<crate::usb::Volume>, bool),
    Notify(NotifyEvent),
    /// Réponse de la boîte de dialogue n° gen : (périphérique, action) choisie.
    Dialog(u64, Option<(String, String)>),
    Tray(crate::tray::TrayEvent),
    /// Erreur survenue dans un fil d'action, à signaler.
    Error(String, String),
    Control(Value, Sender<Value>),
    JobUpdate(String),
}

pub struct Daemon {
    cfg: Config,
    drives: BTreeMap<String, Drive>,
    tx: Sender<Event>,
    notifier: Option<Notifier>,
    /// id de notification → (lecteur, tâche éventuelle)
    notif_owner: HashMap<u32, (String, Option<String>)>,
    job_notif: HashMap<String, u32>,
    fp_cache: HashMap<String, IdentResult>,
    active_jobs: Vec<String>,
    /// Dernier état affiché par tâche (évite de renvoyer une notification identique).
    job_shown: HashMap<String, String>,
    use_dialog: bool,
    /// Boîte de dialogue unique ouverte : (génération, PID partagé, choix proposés).
    dialog_gen: u64,
    dialog_pid: std::sync::Arc<std::sync::Mutex<Option<u32>>>,
    dialog_map: Vec<(String, String)>,
    tray: Option<Sender<crate::tray::Model>>,
    tray_model: Option<crate::tray::Model>,
}

/// Lance le démon (bloquant). Renvoie un code de sortie.
pub fn run(foreground: bool) -> i32 {
    let cfg = Config::load();
    crate::log::init(crate::log::options_from_config(&cfg, Some(&crate::log::default_daemon_log()), foreground));
    for e in &cfg.errors {
        dl_log!(error, "config", e.clone());
    }
    if cfg.mode() == Mode::NativeOnly {
        dl_log!(info, "daemon", "mode natif-seul : le démon ne fait rien");
        return 0;
    }
    // Instance unique : verrou sur le dossier d'exécution.
    let rt = paths::runtime_dir();
    let lock = match File::create(rt.join("instance.lock")) {
        Ok(f) => f,
        Err(e) => {
            dl_log!(error, "daemon", format!("verrou d'instance : {e}"));
            return 1;
        }
    };
    if !sys::try_lock(lock.as_raw_fd(), true).unwrap_or(false) {
        dl_log!(info, "daemon", "une autre instance tourne déjà");
        return 0;
    }
    sys::install_signal_flags();
    let (tx, rx) = mpsc::channel::<Event>();

    // Capacités
    let (ntx, nrx) = mpsc::channel::<NotifyEvent>();
    let notifier = Notifier::start(ntx);
    {
        let tx2 = tx.clone();
        std::thread::spawn(move || {
            while let Ok(e) = nrx.recv() {
                if tx2.send(Event::Notify(e)).is_err() {
                    break;
                }
            }
        });
    }
    let use_dialog = notifier.as_ref().map_or(true, |n| !n.caps.actions) && notify::dialog_tool().is_some();
    let logind = crate::dbus::session_active();
    dl_log!(info, "daemon", "démarrage",
        "version" => crate::VERSION,
        "mode" => cfg.mode().name(),
        "roms" => cfg.roms_dir().display(),
        "notifications" => notifier.as_ref().map(|n| if n.caps.actions { "actions" } else { "sans-actions" }).unwrap_or("absentes"),
        "dialog" => notify::dialog_tool().unwrap_or("aucun"),
        "logind" => match logind { Some(_) => "oui", None => "non" },
        "pkexec" => paths::which("pkexec").is_some(),
    );
    dl_log!(info, "daemon", "capabilities",
        "uevent" => sys::uevent_socket().map(|fd| { sys::close_fd(fd); "oui" }).unwrap_or("non"),
        "inhibit" => if logind.is_some() { "logind" } else { "powermanagement (si présent)" },
        "syslog" => cfg.raw.path("log.syslog").bool_or(false),
    );

    // Reprise des tâches
    for id in jobs::reconcile() {
        dl_log!(warn, "daemon", "tâche interrompue détectée", "job" => id);
    }
    jobs::prune(30, 100);

    if cfg.scan_on_start() {
        let roms = cfg.roms_dir();
        std::thread::Builder::new()
            .name("scan".into())
            .spawn(move || {
                let start = Instant::now();
                match Collection::open() {
                    Ok(col) => {
                        let r = col.scan_with(&roms, &handlers::load_all(), collection::Hashing::None);
                        dl_log!(info, "collection", "scan de démarrage terminé", "added" => r.added, "updated" => r.updated, "relocated" => r.relocated.len(), "missing" => r.missing, "ms" => start.elapsed().as_millis());
                    }
                    Err(e) => dl_log!(warn, "collection", format!("scan de démarrage impossible : {e}")),
                }
            })
            .ok();
    }
    spawn_event_sources(tx.clone());
    spawn_control_server(tx.clone());
    let tray = if cfg.raw.path("tray.enabled").bool_or(true) {
        let (mtx, mrx) = mpsc::channel::<crate::tray::Model>();
        let (etx, erx) = mpsc::channel::<crate::tray::TrayEvent>();
        crate::tray::start(mrx, etx);
        let tx2 = tx.clone();
        std::thread::spawn(move || {
            while let Ok(e) = erx.recv() {
                if tx2.send(Event::Tray(e)).is_err() {
                    break;
                }
            }
        });
        Some(mtx)
    } else {
        None
    };

    let mut d = Daemon {
        cfg,
        drives: BTreeMap::new(),
        tx: tx.clone(),
        notifier,
        notif_owner: HashMap::new(),
        job_notif: HashMap::new(),
        fp_cache: HashMap::new(),
        active_jobs: vec![],
        job_shown: HashMap::new(),
        use_dialog,
        dialog_gen: 0,
        dialog_pid: Default::default(),
        dialog_map: vec![],
        tray,
        tray_model: None,
    };
    // Tâches encore vivantes (démon relancé pendant un dump)
    for (id, st) in jobs::list() {
        if jobs::is_active(&st) && jobs::alive(&id) {
            d.active_jobs.push(id.clone());
            if let Some(dev) = st["device"].as_str() {
                d.drive(dev).job = Some(id.clone());
                d.drive(dev).state = DState::Job;
            }
        }
    }
    // Disques déjà présents
    for dev in crate::device::list_drives() {
        let silent = !d.cfg.notify_on_startup();
        let dr = d.drive(&dev);
        dr.silent = silent;
        if dr.state != DState::Job {
            dr.state = DState::Settling;
            dr.due = Some(Instant::now());
        }
    }
    d.main_loop(rx);
    dl_log!(info, "daemon", "arrêt");
    drop(lock);
    0
}

fn spawn_event_sources(tx: Sender<Event>) {
    // Uevents du noyau (aucune dépendance à udev/udisks).
    let tx1 = tx.clone();
    std::thread::Builder::new()
        .name("uevent".into())
        .spawn(move || {
            let fd = match sys::uevent_socket() {
                Ok(fd) => fd,
                Err(e) => {
                    dl_log!(warn, "events", format!("uevents indisponibles ({e}) : sondage seul"));
                    return;
                }
            };
            let mut buf = vec![0u8; 16384];
            loop {
                let Ok(n) = sys::recv_bytes(fd, &mut buf) else { continue };
                let msg = &buf[..n];
                let fields: Vec<&[u8]> = msg.split(|&b| b == 0).collect();
                let get = |k: &str| fields.iter().find_map(|f| f.strip_prefix(format!("{k}=").as_bytes())).map(|v| String::from_utf8_lossy(v).into_owned());
                if get("SUBSYSTEM").as_deref() != Some("block") {
                    continue;
                }
                let Some(name) = get("DEVNAME") else { continue };
                if !name.starts_with("sr") {
                    continue;
                }
                let media = get("DISK_MEDIA_CHANGE").is_some() || get("DISK_EJECT_REQUEST").is_some() || matches!(get("ACTION").as_deref(), Some("add") | Some("remove"));
                if media && tx1.send(Event::Media(format!("/dev/{name}"))).is_err() {
                    return;
                }
            }
        })
        .ok();
    // Cartouches : volumes Retrode (RETRODE.CFG à la racine), sondés toutes les 2 s.
    let txc = tx.clone();
    std::thread::Builder::new()
        .name("carts".into())
        .spawn(move || {
            let mut known: std::collections::BTreeSet<String> = Default::default();
            let mut mount_tried: Option<String> = None;
            let mut first = true;
            loop {
                let roots = crate::cart::retrode_roots();
                // Retrode branchée mais non montée (pas de montage automatique) : udisks2.
                match crate::cart::unmounted_retrode() {
                    Some(dev) if roots.is_empty() && mount_tried.as_deref() != Some(dev.as_str()) => {
                        mount_tried = Some(dev.clone());
                        match crate::dbus::udisks_mount(&dev) {
                            Ok(mp) => dl_log!(info, "cart", "Retrode montée par udisks2", "device" => dev, "mount" => mp),
                            Err(e) => dl_log!(warn, "cart", format!("montage de la Retrode impossible : {e}"), "device" => dev),
                        }
                    }
                    None => mount_tried = None,
                    _ => {}
                }
                let now: std::collections::BTreeSet<String> = roots.iter().flat_map(|r| crate::cart::rom_files(r)).map(|p| p.to_string_lossy().into_owned()).collect();
                for p in now.difference(&known) {
                    dl_log!(info, "cart", "cartouche détectée", "rom" => p);
                    if txc.send(Event::Cart(p.clone(), first)).is_err() {
                        return;
                    }
                }
                for p in known.difference(&now) {
                    dl_log!(info, "cart", "cartouche retirée", "rom" => p);
                    if txc.send(Event::Cart(p.clone(), false)).is_err() {
                        return;
                    }
                }
                known = now;
                first = false;
                std::thread::sleep(Duration::from_secs(2));
            }
        })
        .ok();
    // Clés USB et supports amovibles de données, sondés toutes les 2 s.
    let txu = tx.clone();
    std::thread::Builder::new()
        .name("usb".into())
        .spawn(move || {
            let mut known: BTreeMap<String, crate::usb::Volume> = BTreeMap::new();
            let mut first = true;
            loop {
                let now: BTreeMap<String, crate::usb::Volume> = crate::usb::list().into_iter().map(|v| (v.dev.clone(), v)).collect();
                for (dev, v) in &now {
                    if known.get(dev) != Some(v) {
                        if !known.contains_key(dev) {
                            dl_log!(info, "usb", "support amovible détecté", "device" => dev, "label" => v.label, "fs" => v.fstype);
                        }
                        if txu.send(Event::Usb(dev.clone(), Some(v.clone()), first)).is_err() {
                            return;
                        }
                    }
                }
                for dev in known.keys().filter(|d| !now.contains_key(*d)) {
                    dl_log!(info, "usb", "support amovible retiré", "device" => dev);
                    if txu.send(Event::Usb(dev.clone(), None, false)).is_err() {
                        return;
                    }
                }
                known = now;
                first = false;
                std::thread::sleep(Duration::from_secs(2));
            }
        })
        .ok();
    // Sondage de secours (toutes les 5 s) : état du tiroir/disque.
    std::thread::Builder::new()
        .name("poll".into())
        .spawn(move || {
            let mut last: HashMap<String, Option<DriveStatus>> = HashMap::new();
            loop {
                for dev in crate::device::list_drives() {
                    let st = cdrom::drive_status(&dev).ok();
                    let prev = last.insert(dev.clone(), st);
                    let changed = match (&prev, &st) {
                        (Some(a), b) => a.map(|x| x == DriveStatus::DiscOk) != b.map(|x| x == DriveStatus::DiscOk),
                        (None, _) => false,
                    };
                    if changed && tx.send(Event::Media(dev)).is_err() {
                        return;
                    }
                }
                std::thread::sleep(Duration::from_secs(5));
            }
        })
        .ok();
}

fn spawn_control_server(tx: Sender<Event>) {
    let path = paths::control_socket();
    let _ = std::fs::remove_file(&path);
    let listener = match UnixListener::bind(&path) {
        Ok(l) => l,
        Err(e) => {
            dl_log!(error, "control", format!("socket de contrôle : {e}"));
            return;
        }
    };
    {
        use std::os::unix::fs::PermissionsExt;
        let _ = std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o600));
    }
    std::thread::Builder::new()
        .name("control".into())
        .spawn(move || {
            for s in listener.incoming().flatten() {
                let tx = tx.clone();
                std::thread::spawn(move || handle_client(s, tx));
            }
        })
        .ok();
}

fn handle_client(s: UnixStream, tx: Sender<Event>) {
    match sys::peer_cred(s.as_raw_fd()) {
        Ok(c) if c.uid == sys::uid() => {}
        _ => return,
    }
    let _ = s.set_read_timeout(Some(Duration::from_secs(10)));
    let mut r = BufReader::new(match s.try_clone() {
        Ok(x) => x,
        Err(_) => return,
    });
    let mut w = s;
    let mut line = String::new();
    while r.read_line(&mut line).map(|n| n > 0).unwrap_or(false) {
        let resp = match json::parse(line.trim()) {
            Ok(req) => {
                let (rtx, rrx) = mpsc::channel();
                if tx.send(Event::Control(req, rtx)).is_err() {
                    return;
                }
                rrx.recv_timeout(Duration::from_secs(120)).unwrap_or_else(|_| jobj! {"ok" => false, "error" => "délai dépassé"})
            }
            Err(e) => jobj! {"ok" => false, "error" => e.to_string()},
        };
        let mut out = resp.to_json();
        out.push('\n');
        if w.write_all(out.as_bytes()).is_err() {
            return;
        }
        line.clear();
    }
}

impl Daemon {
    fn drive(&mut self, dev: &str) -> &mut Drive {
        self.drives.entry(dev.to_string()).or_insert_with(|| Drive { dev: dev.to_string(), state: DState::Empty, gen: 0, due: None, settle_until: None, ident: None, offer: None, notif: None, job: None, silent: false, batch: None, usb: None })
    }

    fn main_loop(&mut self, rx: Receiver<Event>) {
        let mut last_upgrade_check = Instant::now();
        loop {
            if sys::term_requested() {
                return;
            }
            if last_upgrade_check.elapsed() >= Duration::from_secs(10) {
                last_upgrade_check = Instant::now();
                self.restart_if_upgraded();
            }
            if sys::take_hup() {
                self.reload();
            }
            match rx.recv_timeout(Duration::from_millis(500)) {
                Ok(ev) => self.handle(ev),
                Err(mpsc::RecvTimeoutError::Timeout) => {}
                Err(_) => return,
            }
            self.tick();
            self.update_tray();
        }
    }

    /// Après une mise à jour (`make install` remplace l'exécutable), le démon
    /// en cours exécute encore l'ancienne version. Dès qu'il est au repos (aucune
    /// tâche suivie, aucun disque en cours d'identification ni de proposition),
    /// il se relance sur le nouvel exécutable. Les dumps détachés continuent et
    /// sont repris par la nouvelle instance.
    fn restart_if_upgraded(&mut self) {
        let Ok(link) = std::fs::read_link("/proc/self/exe") else { return };
        let s = link.to_string_lossy();
        let Some(path) = s.strip_suffix(" (deleted)") else { return };
        let path = PathBuf::from(path);
        if !path.is_file() {
            return;
        }
        let busy = !self.active_jobs.is_empty() || self.drives.values().any(|d| !matches!(d.state, DState::Empty | DState::Idle | DState::Failed));
        if busy {
            return;
        }
        dl_log!(info, "daemon", "exécutable mis à jour : relance", "path" => path.display());
        if let Some(nt) = &self.notifier {
            for d in self.drives.values() {
                if let Some(n) = d.notif {
                    nt.close(n);
                }
            }
        }
        use std::os::unix::process::CommandExt;
        let err = std::process::Command::new(&path).args(std::env::args_os().skip(1)).exec();
        dl_log!(error, "daemon", format!("relance impossible : {err}"));
    }

    fn reload(&mut self) {
        self.cfg = Config::load();
        for e in &self.cfg.errors {
            dl_log!(error, "config", e.clone());
        }
        dl_log!(info, "daemon", "configuration rechargée");
    }

    /// Seule la session active agit.
    fn session_ok(&self, dev: &str) -> bool {
        match crate::dbus::session_active() {
            Some(a) => a,
            None => sys::can_access(Path::new(dev), sys::R_OK),
        }
    }

    fn tick(&mut self) {
        let now = Instant::now();
        let due: Vec<String> = self.drives.values().filter(|d| d.due.map_or(false, |t| t <= now)).map(|d| d.dev.clone()).collect();
        for dev in due {
            self.check_drive(&dev);
        }
        // Suivi des tâches (source de vérité : state.json).
        if !self.active_jobs.is_empty() {
            for id in self.active_jobs.clone() {
                self.job_update(&id);
            }
        }
    }

    fn handle(&mut self, ev: Event) {
        match ev {
            Event::Media(dev) => {
                dl_log!(debug, "events", "changement de média", "drive" => dev);
                let d = self.drive(&dev);
                if d.state == DState::Job {
                    // le dump éjecte lui-même le disque ; rien à faire
                    return;
                }
                d.gen += 1;
                d.state = DState::Settling;
                d.silent = false;
                d.due = Some(Instant::now() + Duration::from_millis(1500));
                d.settle_until = Some(Instant::now() + Duration::from_secs(10));
                // Nouveau disque (ou le même, réinséré) : l'ancienne proposition
                // est fermée ; la nouvelle s'affichera comme une nouvelle
                // notification (remplacer une notification déjà rangée dans
                // l'historique ne la réaffiche pas sous Plasma).
                self.close_offer(&dev);
            }
            Event::Cart(path, initial) => {
                let silent = initial && !self.cfg.notify_on_startup();
                let d = self.drive(&path);
                if d.state == DState::Job {
                    return;
                }
                d.gen += 1;
                d.state = DState::Settling;
                d.silent = silent;
                d.due = Some(Instant::now() + Duration::from_millis(500));
                d.settle_until = None;
                self.close_offer(&path);
            }
            Event::Usb(dev, Some(vol), initial) => {
                let silent = initial && !self.cfg.notify_on_startup();
                let fresh = self.drives.get(&dev).map_or(true, |d| d.usb.is_none());
                let d = self.drive(&dev);
                d.usb = Some(vol.clone());
                if fresh {
                    d.gen += 1;
                    d.state = DState::Settling;
                    d.silent = silent;
                    d.due = Some(Instant::now());
                } else if d.offer.is_some() {
                    // Montage ou démontage : actions mises à jour, sans nouvelle notification.
                    d.offer = Some(usb_offer(&vol));
                }
            }
            Event::Usb(dev, None, _) => {
                self.close_offer(&dev);
                self.drives.remove(&dev);
                if self.use_dialog && !self.dialog_map.is_empty() {
                    self.refresh_dialog();
                }
            }
            Event::Tray(e) => self.on_tray(e),
            Event::Error(dev, msg) => self.error_notif(&dev, &msg),
            Event::Identified(dev, gen, res) => self.on_identified(&dev, gen, *res),
            Event::Prepared(dev, gen, res) => self.on_prepared(&dev, gen, *res),
            Event::Notify(NotifyEvent::Action { id, key }) => self.on_action(id, &key),
            Event::Notify(NotifyEvent::Closed { id, .. }) => {
                if let Some((dev, None)) = self.notif_owner.get(&id).cloned() {
                    let d = self.drive(&dev);
                    if d.notif == Some(id) {
                        d.notif = None;
                        if d.state == DState::Offering {
                            d.state = DState::Idle;
                        }
                    }
                }
            }
            Event::Dialog(gen, choice) => {
                if gen != self.dialog_gen {
                    return; // boîte remplacée ou fermée par le démon
                }
                *self.dialog_pid.lock().unwrap() = None;
                self.dialog_map.clear();
                match choice {
                    Some((dev, k)) => self.act(&dev, &k),
                    None => {
                        for d in self.drives.values_mut() {
                            if d.state == DState::Offering {
                                d.state = DState::Idle;
                            }
                        }
                    }
                }
            }
            Event::Control(req, reply) => {
                let r = self.control(&req);
                let _ = reply.send(r);
            }
            Event::JobUpdate(id) => {
                if !self.active_jobs.contains(&id) {
                    self.active_jobs.push(id.clone());
                }
                self.job_update(&id);
            }
        }
    }

    /// Cartouche : le « lecteur » est le fichier ROM de la Retrode.
    fn check_cart(&mut self, path: &str) {
        let gen = self.drive(path).gen;
        if !Path::new(path).exists() {
            let notif = self.drives.get(path).and_then(|d| d.notif);
            if let (Some(n), Some(nt)) = (notif, &self.notifier) {
                nt.close(n);
            }
            if self.drives.get(path).map_or(false, |d| d.state != DState::Job) {
                self.drives.remove(path);
            }
            return;
        }
        let d = self.drive(path);
        d.due = None;
        d.state = DState::Identifying;
        d.offer = None;
        let tx = self.tx.clone();
        let p = path.to_string();
        std::thread::spawn(move || {
            let r = crate::cart::identify(&p).map_err(|e| e.to_string());
            let _ = tx.send(Event::Identified(p, gen, Box::new(r)));
        });
    }

    fn check_drive(&mut self, dev: &str) {
        if let Some(v) = self.drives.get(dev).and_then(|d| d.usb.clone()) {
            let gen = self.drive(dev).gen;
            self.drive(dev).due = None;
            return self.on_prepared(dev, gen, Ok(usb_offer(&v)));
        }
        if !dev.starts_with("/dev/") {
            return self.check_cart(dev);
        }
        let status = cdrom::drive_status(dev);
        let gen = self.drive(dev).gen;
        match status {
            Ok(DriveStatus::DiscOk) => {
                let d = self.drive(dev);
                d.due = None;
                d.state = DState::Identifying;
                d.offer = None;
                let profile = self.cfg.drive_profile(dev);
                let tx = self.tx.clone();
                let dev2 = dev.to_string();
                std::thread::spawn(move || {
                    let opts = identify::Options { profile, ..Default::default() };
                    let mut r = identify::identify_device(&dev2, &opts).map_err(|e| e.to_string());
                    // UDF seul (Blu-ray vidéo…) non monté : demander le montage à udisks2, puis réessayer.
                    if r.as_ref().is_ok_and(|x| x.warnings.iter().any(|w| w == "udf-not-mounted")) {
                        match crate::dbus::udisks_mount(&dev2) {
                            Ok(mp) => {
                                dl_log!(info, "identify", "système de fichiers UDF monté par udisks2", "drive" => dev2, "mount" => mp);
                                r = identify::identify_device(&dev2, &opts).map_err(|e| e.to_string());
                            }
                            Err(e) => dl_log!(warn, "identify", format!("montage UDF impossible : {e}"), "drive" => dev2),
                        }
                    }
                    let _ = tx.send(Event::Identified(dev2, gen, Box::new(r)));
                });
            }
            Ok(DriveStatus::NotReady) => {
                let d = self.drive(dev);
                if d.settle_until.map_or(false, |t| Instant::now() < t) {
                    d.due = Some(Instant::now() + Duration::from_secs(1));
                } else {
                    d.due = None;
                    d.state = DState::Empty;
                }
            }
            _ => {
                let notif = {
                    let d = self.drive(dev);
                    d.due = None;
                    d.state = DState::Empty;
                    d.ident = None;
                    d.offer = None;
                    d.notif.take()
                };
                if let (Some(n), Some(nt)) = (notif, &self.notifier) {
                    nt.close(n);
                }
            }
        }
    }

    fn on_identified(&mut self, dev: &str, gen: u64, res: Result<IdentResult, String>) {
        if self.drive(dev).gen != gen {
            return;
        }
        let r = match res {
            Ok(r) => r,
            Err(e) => {
                dl_log!(error, "identify", e, "drive" => dev);
                self.drive(dev).state = DState::Failed;
                return;
            }
        };
        let p = r.primary().cloned();
        dl_log!(info, "identify", "disque identifié",
            "drive" => dev,
            "tag" => p.as_ref().map(|m| m.tag.as_str()).unwrap_or("-"),
            "confidence" => p.as_ref().map(|m| m.confidence.name()).unwrap_or("-"),
            "disc" => p.as_ref().map(|m| m.identity.key()).unwrap_or_default(),
            "warnings" => r.warnings.join(","),
            "media" => r.physical.media.name(),
            "profile" => format!("0x{:04x}", r.physical.profile),
            "tracks" => r.physical.tracks.iter().map(|t| format!("{}{}@{}+{}{}", if t.data { 'D' } else { 'A' }, t.number, t.start, t.length, t.mode.map(|m| format!("/m{m}")).unwrap_or_default())).collect::<Vec<_>>().join(" "),
            "fs" => r.filesystem.clone().unwrap_or_else(|| "-".into()),
            "label" => r.volume_id.clone().unwrap_or_default(),
            "matches" => r.matches.iter().map(|m| m.tag.as_str()).collect::<Vec<_>>().join(","),
        );
        self.fp_cache.insert(r.fingerprint.clone(), r.clone());
        self.drive(dev).ident = Some(r.clone());
        // Disque vierge, ou sans rien de lisible : rien à proposer.
        if p.as_ref().map_or(r.filesystem.is_none(), |m| m.tag == "blank") {
            self.drive(dev).state = DState::Idle;
            return;
        }
        self.drive(dev).state = DState::Resolving;
        let tx = self.tx.clone();
        let cfg = self.cfg.clone();
        let dev2 = dev.to_string();
        std::thread::spawn(move || {
            let o = prepare(&cfg, &dev2, &r);
            let _ = tx.send(Event::Prepared(dev2, gen, Box::new(o)));
        });
    }

    fn on_prepared(&mut self, dev: &str, gen: u64, res: Result<Offer, String>) {
        if self.drive(dev).gen != gen {
            return;
        }
        let offer = match res {
            Ok(o) => o,
            Err(e) => {
                dl_log!(warn, "resolve", e, "drive" => dev);
                self.drive(dev).state = DState::Idle;
                return;
            }
        };
        if let Some(t) = &offer.target {
            dl_log!(info, "resolve", "cible prédite", "drive" => dev, "target" => t.path.display(),
                "confidence" => offer.resolution.as_ref().map(|r| r.confidence.name()).unwrap_or("-"),
                "source" => offer.resolution.as_ref().map(|r| r.source.as_str()).unwrap_or("-"),
                "situation" => offer.situation.as_ref().map(|s| s.name()).unwrap_or("-"));
        }
        self.drive(dev).offer = Some(offer.clone());
        self.drive(dev).state = DState::Offering;
        if self.drive(dev).silent {
            self.drive(dev).state = DState::Idle;
            return;
        }
        if !self.session_ok(dev) {
            dl_log!(debug, "daemon", "session inactive : rien n'est proposé", "drive" => dev);
            self.drive(dev).state = DState::Idle;
            return;
        }
        // Mode hybride : la coquille native propose la lecture des médias ;
        // le démon ne parle qu'en cas de contradiction (jeu déguisé en média).
        if self.cfg.mode() == Mode::Hybrid && offer.media {
            self.drive(dev).state = DState::Idle;
            return;
        }
        // Lot multi-disques en cours ?
        if let Some((sys_id, game)) = self.drive(dev).batch.clone() {
            if offer.handler == sys_id && offer.resolution.as_ref().map_or(false, |r| r.game == game) && matches!(offer.situation, Some(Situation::New) | Some(Situation::GameIncomplete { .. })) {
                self.perform(dev, "dump");
                return;
            }
        }
        // Heuristique non autorisée : on se tait.
        if offer.confidence == Confidence::Heuristic && !self.cfg.handler(&offer.handler)["allow_heuristic"].bool_or(false) && offer.handler != "xbox" {
            self.drive(dev).state = DState::Idle;
            return;
        }
        let policy = self.cfg.policy_for(&offer.handler, offer.media);
        let has = |k: &str| offer.actions.iter().any(|(a, _)| a == k);
        let auto = match policy {
            Policy::Ignore => {
                self.drive(dev).state = DState::Idle;
                return;
            }
            Policy::Ask => None,
            Policy::AutoPlay => ["play-existing", "play-disc"].into_iter().find(|k| has(k)),
            Policy::AutoDump => ["dump", "complete-game"].into_iter().find(|k| has(k)),
            Policy::DumpThenPlay => {
                if has("dump-then-play") {
                    Some("dump-then-play")
                } else {
                    ["play-existing", "complete-game"].into_iter().find(|k| has(k))
                }
            }
        };
        if let Some(k) = auto {
            dl_log!(info, "policy", "action automatique", "drive" => dev, "policy" => policy.name(), "action" => k);
            self.perform(dev, k);
            return;
        }
        self.show_offer(dev);
    }

    fn show_offer(&mut self, dev: &str) {
        let Some(o) = self.drive(dev).offer.clone() else { return };
        let ident = self.drive(dev).ident.clone();
        let (summary, body) = offer_text(&self.cfg, &o, ident.as_ref());
        let actions: Vec<(String, String)> = o.actions.clone();
        if self.use_dialog {
            self.refresh_dialog();
            return;
        }
        let replaces = self.drive(dev).notif.unwrap_or(0);
        let Some(nt) = &self.notifier else { return };
        let icon = offer_icon(&o);
        let n = Notification { replaces, summary, body, icon, actions, resident: true, progress: None, urgency: 1, timeout_ms: 0 };
        if let Some(id) = nt.show(n) {
            self.notif_owner.insert(id, (dev.to_string(), None));
            self.drive(dev).notif = Some(id);
        }
    }

    fn on_action(&mut self, id: u32, key: &str) {
        let Some((dev, job)) = self.notif_owner.get(&id).cloned() else { return };
        dl_log!(info, "notify", "action choisie", "drive" => dev, "action" => key);
        if let Some(j) = job {
            match key {
                "cancel" => {
                    if let Err(e) = jobs::cancel(&j) {
                        dl_log!(warn, "job", e, "job" => j);
                    }
                }
                "play" => {
                    if let (Some(st), Some(plan)) = (jobs::read_state(&j), jobs::read_plan(&j)) {
                        let sys_id = st["result"]["system"].as_str().or(plan["system"].as_str()).unwrap_or("").to_string();
                        if let Some(p) = st["result"]["path"].as_str() {
                            play_existing(&self.cfg, &sys_id, &play_path(p), None, None);
                        }
                    }
                }
                "open-folder" => {
                    if let Some(p) = jobs::read_state(&j).and_then(|s| s["result"]["path"].string()) {
                        let dir = Path::new(&p).parent().map(|x| x.to_path_buf()).unwrap_or_default();
                        let _ = crate::generic::spawn_detached(&["xdg-open".into(), dir.to_string_lossy().into_owned()], &paths::log_dir().join("xdg-open.log"));
                    }
                }
                "batch" => {
                    if let Some(plan) = jobs::read_plan(&j) {
                        let game = plan["resolution"]["game"].str_or("").to_string();
                        let sys_id = plan["system"].str_or("").to_string();
                        self.drive(&dev).batch = Some((sys_id, game));
                    }
                }
                _ => {}
            }
            return;
        }
        self.act(&dev, key);
    }

    /// Ferme la proposition affichée pour un périphérique (notification).
    fn close_offer(&mut self, dev: &str) {
        let Some(n) = self.drives.get_mut(dev).and_then(|d| d.notif.take()) else { return };
        self.notif_owner.remove(&n);
        if let Some(nt) = &self.notifier {
            nt.close(n);
        }
    }

    /// Ferme la boîte de dialogue ouverte, s'il y en a une.
    fn close_dialog(&mut self) {
        self.dialog_gen += 1;
        self.dialog_map.clear();
        if let Some(pid) = self.dialog_pid.lock().unwrap().take() {
            unsafe {
                sys::kill(pid as i32, sys::SIGTERM);
            }
        }
    }

    /// Des propositions sont-elles affichées ?
    fn offers_visible(&self) -> bool {
        self.drives.values().any(|d| d.state == DState::Offering)
    }

    /// Ferme toutes les propositions (notifications et boîte de dialogue).
    /// Elles restent accessibles par l'icône de la zone de notification.
    fn hide_all(&mut self) {
        let devs: Vec<String> = self.drives.keys().cloned().collect();
        for dev in devs {
            self.close_offer(&dev);
            let d = self.drive(&dev);
            if d.state == DState::Offering {
                d.state = DState::Idle;
            }
        }
        self.close_dialog();
    }

    /// Réaffiche les propositions de tous les périphériques présents.
    fn show_all(&mut self) {
        let devs: Vec<String> = self.drives.values().filter(|d| matches!(d.state, DState::Idle | DState::Offering) && d.offer.as_ref().is_some_and(|o| !o.actions.is_empty())).map(|d| d.dev.clone()).collect();
        for dev in &devs {
            self.drive(dev).state = DState::Offering;
        }
        if self.use_dialog {
            if !devs.is_empty() {
                self.refresh_dialog();
            }
            return;
        }
        for dev in &devs {
            self.show_offer(dev);
        }
    }

    /// Action choisie (notification, boîte, menu de l'icône) : toutes les
    /// propositions se ferment, puis l'action s'exécute.
    fn act(&mut self, dev: &str, key: &str) {
        self.hide_all();
        self.perform(dev, key);
    }

    fn on_tray(&mut self, e: crate::tray::TrayEvent) {
        use crate::tray::TrayEvent;
        match e {
            TrayEvent::Activate => {
                if self.offers_visible() {
                    self.hide_all();
                } else {
                    self.show_all();
                }
            }
            TrayEvent::Action(dev, key) if dev.is_empty() => match key.as_str() {
                "show" => self.show_all(),
                "hide" => self.hide_all(),
                _ => {}
            },
            TrayEvent::Action(dev, key) => {
                if let Some(id) = dev.strip_prefix("job:") {
                    if key == "cancel" {
                        if let Err(e) = jobs::cancel(id) {
                            dl_log!(warn, "job", e, "job" => id);
                        }
                    }
                    return;
                }
                dl_log!(info, "tray", "action choisie", "drive" => dev, "action" => key);
                self.act(&dev, &key);
            }
        }
    }

    /// Boîte de dialogue unique (repli sans notifications à actions) : toutes
    /// les propositions affichées, regroupées. Remplace la boîte précédente.
    fn refresh_dialog(&mut self) {
        self.close_dialog();
        let shown: Vec<(String, Offer)> = self.drives.values().filter(|d| d.state == DState::Offering).filter_map(|d| d.offer.clone().map(|o| (d.dev.clone(), o))).collect();
        if shown.is_empty() {
            return;
        }
        let mut items: Vec<(String, String)> = vec![];
        let mut map = vec![];
        let mut texts = vec![];
        let single = shown.len() == 1;
        let mut title = String::new();
        for (dev, o) in &shown {
            let ident = self.drives.get(dev).and_then(|d| d.ident.clone());
            let (summary, body) = offer_text(&self.cfg, o, ident.as_ref());
            if single {
                title = summary.clone();
                texts.push(body);
            } else {
                texts.push(if body.is_empty() { summary.clone() } else { format!("{summary}\n{body}") });
            }
            for (k, l) in &o.actions {
                items.push((map.len().to_string(), if single { l.clone() } else { format!("{summary} : {l}") }));
                map.push((dev.clone(), k.clone()));
            }
        }
        if !single {
            title = "disc-launcher".into();
        }
        self.dialog_map = map.clone();
        let gen = self.dialog_gen;
        let pid = self.dialog_pid.clone();
        let tx = self.tx.clone();
        let text = texts.join("\n\n");
        std::thread::spawn(move || {
            let c = notify::dialog_choose(&title, &text, &items, |p| *pid.lock().unwrap() = Some(p));
            let choice = c.and_then(|i| i.parse::<usize>().ok()).and_then(|i| map.get(i).cloned());
            let _ = tx.send(Event::Dialog(gen, choice));
        });
    }

    /// Menu de l'icône : tous les périphériques et leurs actions, puis les tâches.
    fn update_tray(&mut self) {
        let Some(tx) = &self.tray else { return };
        let mut m = crate::tray::Model::default();
        for d in self.drives.values() {
            if d.state == DState::Job {
                continue;
            }
            let Some(o) = &d.offer else { continue };
            let (summary, _) = offer_text(&self.cfg, o, d.ident.as_ref());
            m.entries.push(crate::tray::Entry { dev: d.dev.clone(), title: summary, icon: offer_icon(o), actions: o.actions.clone() });
        }
        for id in &self.active_jobs {
            let (Some(st), Some(plan)) = (jobs::read_state(id), jobs::read_plan(id)) else { continue };
            if !jobs::is_active(&st) {
                continue;
            }
            let label = match plan["kind"].as_str() {
                Some("verify") => t("verify"),
                Some("convert") => t("convert"),
                _ if plan["cart"].bool_or(false) => t("job-copy"),
                _ => t("job-dump"),
            };
            let title = format!("{label} : {} ({} %)", plan["title"].str_or(""), st["progress"].i64_or(0).clamp(0, 100));
            m.entries.push(crate::tray::Entry { dev: format!("job:{id}"), title, icon: "media-optical".into(), actions: vec![("cancel".into(), t("cancel").into())] });
        }
        if m.entries.iter().any(|e| !e.dev.starts_with("job:")) {
            m.general = if self.offers_visible() { vec![("hide".into(), t("tray-hide").into())] } else { vec![("show".into(), t("tray-show").into())] };
        }
        m.tooltip = m.entries.iter().map(|e| e.title.clone()).collect::<Vec<_>>().join("\n");
        if self.tray_model.as_ref() != Some(&m) {
            let _ = tx.send(m.clone());
            self.tray_model = Some(m);
        }
    }

    /// Dossier à ouvrir dans le gestionnaire de fichiers (monté au besoin).
    fn files_dir(&self, dev: &str) -> Option<Result<PathBuf, String>> {
        let d = self.drives.get(dev)?;
        if let Some(v) = &d.usb {
            let v = v.clone();
            return Some(crate::usb::ensure_mounted(&v).ok_or_else(|| format!("{} : montage impossible", v.dev)));
        }
        if !dev.starts_with("/dev/") {
            // Cartouche : dossier de la Retrode.
            return Path::new(dev).parent().map(|p| Ok(p.to_path_buf()));
        }
        None
    }

    /// Exécute une action sur le disque d'un lecteur.
    fn perform(&mut self, dev: &str, key: &str) {
        let Some(o) = self.drive(dev).offer.clone() else { return };
        let ident = self.drive(dev).ident.clone();
        self.drive(dev).state = DState::Idle;
        match key {
            "play-existing" | "play-game" => {
                if let Some(p) = o.situation.as_ref().and_then(|s| match s {
                    Situation::GameIncomplete { game_dir, .. } => o.target.as_ref().and_then(|t| t.m3u.clone()).filter(|m| m.exists()).or(Some(game_dir.clone())),
                    other => other.existing().map(|p| p.to_path_buf()),
                }) {
                    play_existing(&self.cfg, &o.handler, &play_path(&p.to_string_lossy()), ident.as_ref(), o.resolution.as_ref());
                }
            }
            "play-disc" => {
                let cfg = self.cfg.clone();
                let d = dev.to_string();
                let o2 = o.clone();
                std::thread::spawn(move || {
                    let Some(m) = handlers::get(&o2.handler) else { return };
                    let ctx = handlers::CallCtx { action: "play", device: Some(&d), ident: ident.as_ref(), tag: Some(&o2.tag), resolution: o2.resolution.as_ref(), target: o2.target.as_ref(), checked: true, ..Default::default() };
                    match handlers::invoke(&m, &cfg, "play", &["--disc".into(), d.clone()], &jobj! {"device" => d.clone()}, &ctx, Duration::from_secs(120)) {
                        Ok(r) if r.code == 0 => dl_log!(info, "play", "lecture lancée", "drive" => d, "handler" => m.id, "player" => r.out["player"].str_or(r.out["message"].str_or(""))),
                        Ok(r) => dl_log!(warn, "play", format!("{} {}", r.out["error"].str_or(""), r.stderr.trim()), "drive" => d, "handler" => m.id),
                        Err(e) => dl_log!(error, "play", e, "drive" => d),
                    }
                });
            }
            "dump" | "redump" | "dump-then-play" | "complete-game" | "restart-dump" | "dump-raw" => {
                if let Some(Situation::Partial(j)) = &o.situation {
                    let _ = std::fs::remove_dir_all(jobs::job_dir(j));
                }
                let kind = if key == "redump" { "redump" } else { "dump" };
                match start_dump(&self.cfg, dev, &o, ident.as_ref(), kind, key == "dump-then-play") {
                    Ok(id) => {
                        dl_log!(info, "job", "tâche lancée", "job" => id, "drive" => dev);
                        self.drive(dev).job = Some(id.clone());
                        self.drive(dev).state = DState::Job;
                        self.active_jobs.push(id.clone());
                        if let Some(n) = self.drive(dev).notif.take() {
                            self.notif_owner.remove(&n);
                            self.job_notif.insert(id.clone(), n);
                            self.notif_owner.insert(n, (dev.to_string(), Some(id.clone())));
                        }
                        self.job_update(&id);
                    }
                    Err(e) => self.error_notif(dev, &e),
                }
            }
            "convert" | "verify" => {
                let r = if key == "convert" { start_convert(&self.cfg, dev, &o, ident.as_ref()) } else { start_verify(dev, &o) };
                match r {
                    Ok(id) => {
                        self.active_jobs.push(id.clone());
                        self.job_update(&id);
                    }
                    Err(e) => self.error_notif(dev, &e),
                }
            }
            "delete-partial" => {
                if let Some(Situation::Partial(j)) = &o.situation {
                    let _ = std::fs::remove_dir_all(jobs::job_dir(j));
                }
            }
            "open-files" => {
                let cfg = self.cfg.clone();
                let tx = self.tx.clone();
                let d = dev.to_string();
                let known = self.files_dir(dev);
                std::thread::spawn(move || {
                    let dir = match known {
                        Some(r) => r,
                        None => crate::data::ensure_mounted(&d).ok_or_else(|| format!("{d} : montage impossible")),
                    };
                    let r = dir.and_then(|p| crate::filemanager::open(&cfg, &p).map(|how| (p, how)));
                    match r {
                        Ok((p, how)) => dl_log!(info, "files", "dossier ouvert", "drive" => d, "path" => p.display(), "with" => how),
                        Err(e) => {
                            let _ = tx.send(Event::Error(d, e));
                        }
                    }
                });
            }
            "eject" => {
                let tx = self.tx.clone();
                let d = dev.to_string();
                std::thread::spawn(move || {
                    if crate::device::find_mount_point(&d).is_some() {
                        if let Err(e) = crate::dbus::udisks_unmount(&d) {
                            dl_log!(warn, "eject", format!("démontage impossible : {e}"), "drive" => d);
                        }
                    }
                    match cdrom::eject(&d) {
                        Ok(()) => dl_log!(info, "eject", "disque éjecté", "drive" => d),
                        Err(e) => {
                            let _ = tx.send(Event::Error(d, format!("{} : {e}", t("eject"))));
                        }
                    }
                });
            }
            "mount" | "unmount" | "safe-remove" => {
                let Some(v) = self.drive(dev).usb.clone() else { return };
                let tx = self.tx.clone();
                let key = key.to_string();
                std::thread::spawn(move || {
                    let mounted = crate::device::find_mount_point(&v.dev).is_some();
                    let r = match key.as_str() {
                        "mount" => crate::dbus::udisks_mount(&v.dev).map(|_| ()),
                        "unmount" => crate::dbus::udisks_unmount(&v.dev),
                        _ => (if mounted { crate::dbus::udisks_unmount(&v.dev) } else { Ok(()) }).and_then(|_| crate::dbus::udisks_power_off(&v.dev)),
                    };
                    match r {
                        Ok(()) => dl_log!(info, "usb", "action effectuée", "device" => v.dev, "action" => key),
                        Err(e) => {
                            let _ = tx.send(Event::Error(v.dev.clone(), format!("{} : {e}", t(&key))));
                        }
                    }
                });
            }
            _ => {}
        }
    }

    fn error_notif(&mut self, dev: &str, msg: &str) {
        dl_log!(error, "daemon", msg.to_string(), "drive" => dev);
        let replaces = self.drives.get(dev).and_then(|d| d.notif).unwrap_or(0);
        if let Some(nt) = &self.notifier {
            let n = Notification { replaces, summary: t("job-failed").into(), body: msg.into(), icon: "dialog-error".into(), urgency: 1, timeout_ms: -1, ..Default::default() };
            nt.show(n);
        }
    }

    fn job_update(&mut self, id: &str) {
        let Some(st) = jobs::read_state(id) else { return };
        let Some(plan) = jobs::read_plan(id) else { return };
        let status = st["status"].str_or("pending").to_string();
        let finished = !jobs::is_active(&st) || (status == "running" && !jobs::alive(id) && st["pid"].as_i64().is_some() && !sys::pid_alive(st["pid"].as_i64().unwrap() as i32));
        let dev = plan["device"].str_or("").to_string();
        let title = plan["title"].str_or("").to_string();
        if finished && self.active_jobs.iter().any(|x| x == id) {
            let msg = st["message"].str_or("").to_string();
            match status.as_str() {
                "done" => dl_log!(info, "job", "tâche terminée", "job" => id, "drive" => dev, "path" => st["result"]["path"].str_or("")),
                "cancelled" => dl_log!(info, "job", "tâche annulée", "job" => id, "drive" => dev),
                _ => dl_log!(error, "job", format!("tâche en échec : {msg}"), "job" => id, "drive" => dev, "status" => status, "detail" => format!("disc-launcher log --job {id}")),
            }
        }
        if self.notifier.is_none() {
            if finished {
                self.active_jobs.retain(|x| x != id);
                if !dev.is_empty() && self.drive(&dev).job.as_deref() == Some(id) {
                    self.drive(&dev).job = None;
                    self.drive(&dev).state = DState::Idle;
                }
            }
            return;
        }
        let replaces = self.job_notif.get(id).copied().unwrap_or(0);
        let sig = format!("{status}|{}|{}|{finished}", st["progress"].i64_or(0), st["step"].str_or(""));
        if self.job_shown.get(id) == Some(&sig) {
            return;
        }
        self.job_shown.insert(id.to_string(), sig);
        let n = if !finished {
            let pct = st["progress"].i64_or(0).clamp(0, 100) as u8;
            let label = if plan["kind"].as_str() == Some("verify") {
                t("verify")
            } else if plan["kind"].as_str() == Some("convert") {
                t("convert")
            } else if plan["cart"].bool_or(false) {
                t("job-copy")
            } else {
                t("job-dump")
            };
            Notification {
                replaces,
                summary: format!("{label} : {title}"),
                body: format!("{} {}/{} — {} ({pct} %)", t("step"), st["step_index"].i64_or(1), st["steps_total"].i64_or(1), st["step"].str_or("…")),
                icon: "media-optical".into(),
                actions: vec![("cancel".into(), t("cancel").into())],
                resident: true,
                progress: Some(pct),
                urgency: 0,
                timeout_ms: 0,
            }
        } else {
            self.active_jobs.retain(|x| x != id);
            if !dev.is_empty() {
                let d = self.drive(&dev);
                if d.job.as_deref() == Some(id) {
                    d.job = None;
                    d.state = DState::Idle;
                }
            }
            match status.as_str() {
                "done" => {
                    let path = st["result"]["path"].str_or("").to_string();
                    let ver = match st["result"]["verified"].as_str() {
                        Some("ok") => format!("\n✓ {}", t("verified-ok")),
                        Some("mismatch") => format!("\n⚠ {}", t("verified-mismatch")),
                        _ => String::new(),
                    };
                    let mut actions = vec![("play".to_string(), t("play").to_string()), ("open-folder".to_string(), t("open-folder").to_string())];
                    let multi = plan["resolution"]["discs"].as_i64().map_or(false, |n| n > 1);
                    let mut extra = String::new();
                    if multi && plan["kind"].as_str() == Some("dump") {
                        extra = format!("\n{}", t("insert-next"));
                        actions.push(("batch".into(), t("batch").into()));
                    }
                    if plan["play_after"].bool_or(false) {
                        play_existing(&self.cfg, st["result"]["system"].str_or(plan["system"].str_or("")), &play_path(&path), None, naming::Resolution::from_value(&plan["resolution"]).as_ref());
                    }
                    let shown = Path::new(&path).strip_prefix(self.cfg.roms_dir()).map(|p| p.display().to_string()).unwrap_or(path.clone());
                    Notification { replaces, summary: format!("{} : {title}", if plan["cart"].bool_or(false) { t("job-copy-done") } else { t("job-done") }), body: format!("{shown}{ver}{extra}"), icon: "media-optical".into(), actions, resident: false, progress: None, urgency: 1, timeout_ms: -1 }
                }
                "cancelled" => Notification { replaces, summary: format!("{} : {title}", t("job-cancelled")), body: String::new(), icon: "media-optical".into(), timeout_ms: 5000, ..Default::default() },
                "interrupted" => Notification { replaces, summary: format!("{} : {title}", t("job-interrupted")), body: st["message"].str_or("").into(), icon: "dialog-warning".into(), timeout_ms: -1, ..Default::default() },
                _ => Notification { replaces, summary: format!("{} : {title}", t("job-failed")), body: st["message"].str_or("").into(), icon: "dialog-error".into(), urgency: 2, timeout_ms: -1, ..Default::default() },
            }
        };
        let shown = self.notifier.as_ref().and_then(|nt| nt.show(n));
        if let Some(nid) = shown {
            self.job_notif.insert(id.to_string(), nid);
            self.notif_owner.insert(nid, (dev.clone(), Some(id.to_string())));
        }
        if finished {
            self.job_notif.remove(id);
            self.job_shown.remove(id);
        }
    }

    fn control(&mut self, req: &Value) -> Value {
        match req["cmd"].str_or("") {
            "ping" => jobj! {"ok" => true, "version" => crate::VERSION},
            "status" => {
                let drives: Vec<Value> = self
                    .drives
                    .values()
                    .map(|d| {
                        jobj! {
                            "device" => d.dev.clone(), "state" => d.state.name(), "job" => d.job.clone(),
                            "identification" => d.ident.as_ref().map(|i| i.to_value()),
                            "offer" => d.offer.as_ref().map(offer_value),
                        }
                    })
                    .collect();
                jobj! {"ok" => true, "drives" => drives, "active_jobs" => self.active_jobs.clone()}
            }
            "identify" => {
                let dev = req["device"].as_str().map(|s| s.to_string()).or_else(|| self.drives.keys().next().cloned()).unwrap_or_else(|| "/dev/sr0".into());
                let d = self.drive(&dev);
                d.gen += 1;
                d.silent = req["silent"].bool_or(false);
                d.state = DState::Settling;
                d.due = Some(Instant::now());
                jobj! {"ok" => true, "device" => dev}
            }
            "offer" => {
                let dev = req["device"].str_or("/dev/sr0").to_string();
                match self.drives.get(&dev).and_then(|d| d.offer.as_ref()) {
                    Some(o) => jobj! {"ok" => true, "offer" => offer_value(o)},
                    None => jobj! {"ok" => false, "error" => "aucune proposition pour ce lecteur"},
                }
            }
            "run" => {
                let dev = req["device"].str_or("/dev/sr0").to_string();
                let action = req["action"].str_or("").to_string();
                let has_offer = self.drives.get(&dev).and_then(|d| d.offer.as_ref()).is_some();
                if !has_offer {
                    return jobj! {"ok" => false, "error" => "disque pas encore identifié (voir disc-launcher status)"};
                }
                if let Some(target) = req["target"].as_str() {
                    let h = self.drives.get(&dev).and_then(|d| d.offer.as_ref()).map(|o| o.handler.clone()).unwrap_or_default();
                    if target != h {
                        return jobj! {"ok" => false, "error" => format!("ce disque relève de « {h} », pas de « {target} »"), "handler" => h};
                    }
                }
                self.perform(&dev, &action);
                jobj! {"ok" => true}
            }
            "jobs" => jobj! {"ok" => true, "jobs" => jobs::list().into_iter().take(req["limit"].as_i64().unwrap_or(20) as usize).map(|(_, s)| s).collect::<Vec<_>>()},
            "cancel" => match jobs::cancel(req["id"].str_or("")) {
                Ok(()) => jobj! {"ok" => true},
                Err(e) => jobj! {"ok" => false, "error" => e},
            },
            "reload" => {
                self.reload();
                jobj! {"ok" => true}
            }
            "job-update" => {
                let id = req["id"].str_or("").to_string();
                if !id.is_empty() {
                    self.handle(Event::JobUpdate(id));
                }
                jobj! {"ok" => true}
            }
            c => jobj! {"ok" => false, "error" => format!("commande inconnue : {c}")},
        }
    }
}

/// Icône d'une proposition : celle du système si elle est installée.
fn offer_icon(o: &Offer) -> String {
    let icon = format!("disc-launcher-{}", o.handler);
    if icon_exists(&icon) {
        return icon;
    }
    match o.handler.as_str() {
        "usb" => "drive-removable-media-usb".into(),
        _ => "media-optical".into(),
    }
}

/// Proposition pour une clé USB : ouvrir, monter ou démonter, retirer.
pub fn usb_offer(v: &crate::usb::Volume) -> Offer {
    let mounted = v.mount.is_some() || crate::device::find_mount_point(&v.dev).is_some();
    let mut actions = vec![("open-files".to_string(), t("open-files").to_string())];
    if !v.dev.starts_with("usb:") {
        actions.push(if mounted { ("unmount".into(), t("unmount").into()) } else { ("mount".into(), t("mount").into()) });
        actions.push(("safe-remove".into(), t("safe-remove").into()));
    }
    let size = if v.size > 0 { format!(" ({})", crate::usb::human_size(v.size)) } else { String::new() };
    let where_ = match &v.mount {
        Some(m) => format!("{} {}", t("mounted-on"), m.display()),
        None => t("not-mounted").to_string(),
    };
    Offer {
        handler: "usb".into(),
        media: true,
        handler_name: format!("{} — {}{size}", t("usb-key"), v.name()),
        tag: "usb".into(),
        confidence: Confidence::Strong,
        resolution: None,
        target: None,
        situation: None,
        collision: false,
        describe: jobj! {"device" => v.dev.clone(), "fs" => v.fstype.clone(), "label" => v.label.clone(), "model" => v.model.clone(), "status" => where_},
        actions,
        warnings: vec![],
    }
}

fn icon_exists(name: &str) -> bool {
    let dirs = std::env::var("XDG_DATA_DIRS").unwrap_or_else(|_| "/usr/local/share:/usr/share".into());
    std::iter::once(paths::data_home()).chain(dirs.split(':').map(PathBuf::from)).any(|d| d.join("icons/hicolor/scalable/apps").join(format!("{name}.svg")).exists())
}

/// Un dossier `.m3u` d'ES-DE se lance par le fichier `.m3u` qu'il contient.
fn play_path(p: &str) -> String {
    // Une piste .bin indexée se lance par son .cue.
    let launch = collection::launch_path_for(Path::new(p));
    let p = launch.to_string_lossy();
    let p = p.as_ref();
    let path = Path::new(p);
    if path.is_dir() && path.extension().map_or(false, |e| e == "m3u") {
        if let Some(n) = path.file_name() {
            let inner = path.join(n);
            if inner.exists() {
                return inner.to_string_lossy().into_owned();
            }
        }
    }
    p.to_string()
}

fn play_existing(cfg: &Config, system: &str, path: &str, ident: Option<&IdentResult>, res: Option<&Resolution>) {
    let cfg = cfg.clone();
    let sys_id = system.to_string();
    let p = path.to_string();
    let ident = ident.cloned();
    let res = res.cloned();
    std::thread::spawn(move || {
        let Some(m) = handlers::get(&sys_id) else { return };
        let ctx = handlers::CallCtx { action: "play", existing: Some(&p), ident: ident.as_ref(), resolution: res.as_ref(), checked: true, ..Default::default() };
        match handlers::invoke(&m, &cfg, "play", &["--existing".into(), p.clone()], &jobj! {"existing" => p.clone()}, &ctx, Duration::from_secs(30)) {
            Ok(r) if r.code == 0 => dl_log!(info, "play", "émulateur lancé", "system" => sys_id, "path" => p),
            Ok(r) => dl_log!(warn, "play", format!("{} {}", r.out["error"].str_or(""), r.stderr.trim()), "system" => sys_id),
            Err(e) => dl_log!(error, "play", e, "system" => sys_id),
        }
    });
}

pub fn offer_value(o: &Offer) -> Value {
    jobj! {
        "handler" => o.handler.clone(), "handler_name" => o.handler_name.clone(), "tag" => o.tag.clone(),
        "confidence" => o.confidence.name(),
        "resolution" => o.resolution.as_ref().map(|r| r.to_value()),
        "target" => o.target.as_ref().map(|t| t.to_value()),
        "situation" => o.situation.as_ref().map(|s| s.to_value()),
        "actions" => o.actions.iter().map(|(k, _)| Value::from(k.clone())).collect::<Vec<_>>(),
        "warnings" => o.warnings.clone(), "describe" => o.describe.clone(),
    }
}

/// Titre et corps de la notification de proposition (section 12.1).
pub fn offer_text(cfg: &Config, o: &Offer, ident: Option<&IdentResult>) -> (String, String) {
    let name = o.resolution.as_ref().map(|r| r.name.clone()).or_else(|| ident.and_then(|i| i.primary()).and_then(|m| m.identity.title.clone().or(m.identity.label.clone()))).unwrap_or_else(|| t("unknown-disc").into());
    let mut summary = if o.media {
        match ident.and_then(|i| i.volume_id.clone()).filter(|v| !v.is_empty()) {
            Some(label) => format!("{} — {label}", o.handler_name),
            None => o.handler_name.clone(),
        }
    } else {
        format!("{} — {name}", o.handler_name)
    };
    if ident.is_some_and(|i| i.physical.media == crate::device::MediaKind::Cart) {
        summary = format!("{} ({})", summary, t("is-cart"));
    } else if !o.media && ident.is_some_and(|i| i.matches.iter().any(|m| crate::identify::is_media(&m.tag) && !m.primary)) {
        summary = format!("{} ({})", summary, t("is-game"));
    }
    let mut lines = vec![];
    if let (Some(tg), Some(r)) = (&o.target, &o.resolution) {
        lines.push(format!("→ {}  {}", tg.display(&cfg.roms_dir()), t(&format!("conf-{}", r.confidence.name()))));
    }
    if let Some(s) = &o.situation {
        lines.push(t(&format!("sit-{}", s.name())).to_string());
    }
    if o.collision {
        lines.push(t("w-collision").into());
    }
    if o.handler == "usb" {
        lines.push(o.describe["status"].str_or("").to_string());
    }
    for w in &o.warnings {
        lines.push(format!("⚠ {}", notify::warning_text(w)));
    }
    (summary, lines.join("\n"))
}

/// Préparation (fil de travail) : résolution, cible, collection, actions.
pub fn prepare(cfg: &Config, dev: &str, r: &IdentResult) -> Result<Offer, String> {
    let optical = dev.starts_with("/dev/");
    // Disque de données ordinaire (ni jeu ni média reconnu) : l'ouvrir.
    if r.primary().map_or(r.filesystem.is_some(), |m| m.tag == "data") {
        let mut actions = vec![];
        if r.filesystem.is_some() {
            actions.push(("open-files".to_string(), t("open-files").to_string()));
        }
        if optical {
            actions.push(("eject".to_string(), t("eject").to_string()));
        }
        return Ok(Offer {
            handler: "data".into(),
            media: true,
            handler_name: t("data-disc").into(),
            tag: "data".into(),
            confidence: Confidence::Strong,
            resolution: None,
            target: None,
            situation: None,
            collision: false,
            describe: Value::Null,
            actions,
            warnings: vec![],
        });
    }
    let m = r.primary().ok_or("aucune étiquette principale")?.clone();
    let all = handlers::load_all();
    if m.tag == "unknown:needs-raw-read" {
        return Ok(Offer {
            handler: "unknown".into(),
            media: false,
            handler_name: t("unknown-disc").into(),
            tag: m.tag.clone(),
            confidence: m.confidence,
            resolution: None,
            target: None,
            situation: None,
            collision: false,
            describe: Value::Null,
            actions: {
                let mut a: Vec<(String, String)> = if paths::which("redumper").is_some() { vec![("dump-raw".into(), t("dump-raw").into())] } else { vec![] };
                a.push(("eject".into(), t("eject").into()));
                a
            },
            warnings: r.warnings.clone(),
        });
    }
    let manifest = handlers::for_tag(&all, &m.tag).cloned().ok_or_else(|| format!("aucun gestionnaire pour {}", m.tag))?;
    let dctx = handlers::CallCtx { action: "describe", device: Some(dev), ident: Some(r), tag: Some(&m.tag), checked: true, ..Default::default() };
    let describe = handlers::invoke(&manifest, cfg, "describe", &[], &jobj! {"profile" => r.profile.clone(), "device" => dev, "tag" => m.tag.clone()}, &dctx, Duration::from_secs(10)).map(|c| c.out).unwrap_or(Value::Null);
    if manifest.is_media() {
        let mut actions = vec![];
        if describe["actions"]["play-disc"].bool_or(false) {
            let label = match describe["player_name"].as_str() {
                Some(p) => format!("{} {p}", t("play-with")),
                None => t("play-media").to_string(),
            };
            actions.push(("play-disc".to_string(), label));
        }
        let mut warnings = r.warnings.clone();
        if actions.is_empty() {
            warnings.push(t("no-player").into());
        }
        // Média avec système de fichiers (photos, Blu-ray, DVD, disque de
        // fichiers audio/vidéo…) : il peut aussi s'ouvrir comme un dossier.
        if r.filesystem.is_some() {
            actions.push(("open-files".to_string(), t("open-files").to_string()));
        }
        if optical {
            actions.push(("eject".to_string(), t("eject").to_string()));
        }
        return Ok(Offer { handler: manifest.id.clone(), media: true, handler_name: manifest.name().to_string(), tag: m.tag.clone(), confidence: m.confidence, resolution: None, target: None, situation: None, collision: false, describe, actions, warnings });
    }
    let refdb = RefDb::load();
    let col = Collection::open().or_else(|_| Collection::open_at(Path::new(":memory:")))?;
    let res = naming::resolve(&m.identity, &r.physical, &naming::ResolveCtx { cfg, manifest: &manifest, refdb: &refdb, collection: Some(&col), use_cache: true });
    let mut target = naming::build_target(cfg, &manifest, &m.identity, &res);
    let key = m.identity.key();
    let partial = jobs::interrupted_for_key(&key);
    let cart = r.physical.media == crate::device::MediaKind::Cart;
    let Check { situation, collision_alt } = if cart {
        // Cartouche : n'importe quelle version déjà présente du jeu (révision,
        // région, WAD de console virtuelle…), retrouvée par son titre.
        let found = cart_existing(cfg, &manifest, &col, &key, &m.identity, &res, &refdb);
        let situation = match (found, partial) {
            (Some(p), _) => Situation::Present(p),
            (None, Some(j)) => Situation::Partial(j),
            (None, None) => Situation::New,
        };
        Check { situation, collision_alt: None }
    } else {
        collection::check_existing(&col, &key, &target, &manifest.accepted_formats(), partial, Some(&cfg.roms_dir()))
    };
    let collision = collision_alt.is_some();
    if let Some(alt) = collision_alt {
        target = alt;
    }
    let acts = &describe["actions"];
    let can_play = acts["play-existing"].bool_or(false);
    let can_disc = acts["play-disc"].bool_or(false) && r.feasibility.read;
    let can_dump = acts["dump"].bool_or(false) && r.feasibility.dump;
    let mut actions: Vec<&str> = match &situation {
        // Cartouche : lire la ROM sur la Retrode revient à la dumper ; « Jouer »
        // lance la copie existante, ou copie puis lance.
        _ if cart => match &situation {
            Situation::Partial(_) => vec!["restart-dump", "delete-partial"],
            s if s.existing().is_some() => vec!["play-existing", "redump"],
            _ => vec!["dump-then-play", "dump"],
        },
        Situation::Known(_) => vec!["play-existing", "redump", "verify"],
        Situation::Present(_) => vec!["play-existing", "verify", "redump"],
        Situation::OtherFormat(_) => vec!["play-existing", "convert", "redump"],
        Situation::GameIncomplete { .. } => vec!["complete-game", "play-game"],
        Situation::Partial(_) => vec!["restart-dump", "delete-partial"],
        Situation::New => vec!["dump", "dump-then-play", "play-disc"],
    };
    actions.retain(|a| match *a {
        "play-existing" | "play-game" => can_play,
        "play-disc" => can_disc,
        "dump" | "redump" | "dump-then-play" | "complete-game" | "restart-dump" => can_dump && !(*a == "dump-then-play" && !can_play),
        "verify" => situation.existing().map_or(false, |p| crate::generic::verify_command(&manifest, &p.to_string_lossy()).is_some()),
        "convert" => situation.existing().map_or(false, |p| crate::generic::convert_plan(&manifest, &p.to_string_lossy(), "/dev/null").is_ok()),
        _ => true,
    });
    actions.truncate(3);
    if cart {
        actions.push("open-files");
    } else if optical {
        actions.push("eject");
    }
    // Cartouche : deux actions, « Jouer » et « (Re-)dumper ».
    let list: Vec<(String, String)> = actions
        .iter()
        .map(|a| {
            let k = if cart && matches!(*a, "dump" | "dump-then-play" | "play-existing" | "redump") { format!("cart-{a}") } else { a.to_string() };
            (a.to_string(), t(&k).to_string())
        })
        .collect();
    let mut warnings = r.warnings.clone();
    if let Some(miss) = describe["missing"].as_arr().first() {
        warnings.push(format!("outil manquant : {}", miss.str_or("?")));
    }
    // Nom provisoire faute de base de référence pour ce système.
    if res.confidence == naming::NameConfidence::Fallback && refdb.counts().get(&m.identity.system).map_or(true, |c| c.0 == 0) {
        warnings.push(format!("{} {}", t("w-refdb-empty"), m.identity.system));
    }
    Ok(Offer {
        handler: manifest.id.clone(),
        media: false,
        handler_name: manifest.name().to_string(),
        tag: m.tag.clone(),
        confidence: m.confidence,
        resolution: Some(res),
        target: Some(target),
        situation: Some(situation),
        collision,
        describe,
        actions: list,
        warnings,
    })
}

use crate::refdb::title_key;

/// Copie déjà présente d'un jeu de cartouche, quelle qu'en soit la version :
/// copie connue par sa clé, ou fichier des dossiers du système dont le titre
/// correspond (nom de la base pour ce code de jeu, toutes versions, ou titre
/// de l'en-tête). Le format préféré du manifeste (`formats.prefer`, ex. un WAD
/// avant la ROM) l'emporte, puis le nom exact.
fn cart_existing(cfg: &Config, m: &handlers::Manifest, col: &Collection, key: &str, id: &crate::identity::Identity, res: &Resolution, refdb: &RefDb) -> Option<PathBuf> {
    let mut titles: Vec<String> = vec![title_key(&res.name)];
    if let Some(code) = id.game_id.as_deref().or(id.serial.as_deref()) {
        titles.extend(refdb.names_for_serial(&id.system, code).iter().map(|n| title_key(n)));
    }
    if let Some(t) = &id.title {
        titles.push(title_key(t));
        // Titre de l'en-tête (tronqué) : tous les noms de la base qui lui correspondent.
        titles.extend(refdb.names_for_title(&id.system, t).iter().map(|n| title_key(n)));
    }
    titles.retain(|t| t.len() >= 3);
    titles.sort();
    titles.dedup();
    let accepted = m.accepted_formats();
    let prefer = m.raw.path("formats.prefer").strings();
    let rank = |p: &Path| -> (usize, usize) {
        let ext = p.extension().map(|e| e.to_string_lossy().to_ascii_lowercase()).unwrap_or_default();
        let pref = prefer.iter().position(|x| *x == ext).unwrap_or(prefer.len());
        let exact = p.file_stem().map_or(false, |s| s.to_string_lossy() == res.name);
        (pref, if exact { 0 } else { 1 })
    };
    let mut found: Vec<PathBuf> = vec![];
    if let Some(e) = col.find_key(key, Some(&cfg.roms_dir())) {
        let p = PathBuf::from(&e.path);
        if p.exists() {
            found.push(p);
        }
    }
    let roms = cfg.roms_dir();
    for folder in m.all_folders() {
        let dir = roms.join(&folder);
        let mut stack = vec![(dir, 0usize)];
        while let Some((d, depth)) = stack.pop() {
            let Ok(it) = std::fs::read_dir(&d) else { continue };
            for e in it.flatten() {
                let p = e.path();
                let name = e.file_name().to_string_lossy().into_owned();
                if name.starts_with('.') {
                    continue;
                }
                if p.is_dir() {
                    if depth < 1 {
                        stack.push((p, depth + 1));
                    }
                    continue;
                }
                let ext = p.extension().map(|x| x.to_string_lossy().to_ascii_lowercase()).unwrap_or_default();
                if !accepted.contains(&ext) {
                    continue;
                }
                let stem = p.file_stem().map(|s| s.to_string_lossy().into_owned()).unwrap_or_default();
                if titles.contains(&title_key(&stem)) && !found.contains(&p) {
                    found.push(p);
                }
            }
        }
    }
    found.sort_by_key(|p| rank(p));
    if let Some(p) = found.first() {
        dl_log!(info, "cart", "copie existante retrouvée", "system" => id.system, "path" => p.display(), "candidates" => found.len());
    }
    found.into_iter().next()
}

fn tmp_dir_for(target: &Target, id_hint: &str) -> PathBuf {
    target.system_dir.join(".disc-launcher-tmp").join(id_hint)
}

/// Construit et lance une tâche de dump.
pub fn start_dump(cfg: &Config, dev: &str, o: &Offer, ident: Option<&IdentResult>, kind: &str, play_after: bool) -> Result<String, String> {
    let ident = ident.ok_or("disque non identifié")?;
    let late = o.tag == "unknown:needs-raw-read";
    let (manifest, target, res) = if late {
        // Cible provisoire dans un dossier commun ; le système est déterminé après lecture.
        let t = Target { system_dir: cfg.roms_dir().join(".unidentified"), folder: ".unidentified".into(), path: cfg.roms_dir().join(".unidentified/disc.iso"), stem: format!("disc-{}", crate::log::compact_stamp()), ext: "iso".into(), game_dir: None, m3u: None };
        (None, t, None)
    } else {
        (Some(handlers::get(&o.handler).ok_or("gestionnaire introuvable")?), o.target.clone().ok_or("cible inconnue")?, o.resolution.clone())
    };
    let hint = format!("{}-{}", crate::log::compact_stamp(), sys::pid());
    let tmp = tmp_dir_for(&target, &hint);
    let m = ident.primary().ok_or("aucune étiquette")?;
    let steps_out = if late {
        jobj! {"steps" => vec![jobj!{"name" => "read", "helper" => true, "progress" => "redumper",
            "helper_profile" => "redumper-disc", "helper_name" => target.stem.clone(), "helper_options" => cfg.drive_tool_args(dev, "redumper"),
            "command" => vec![Value::from("redumper"), Value::from("disc"), Value::from(format!("--drive={}", crate::device::sg_of(dev).unwrap_or_else(|| dev.to_string()))), Value::from(format!("--image-path={}", tmp.display())), Value::from(format!("--image-name={}", target.stem))]}],
            "outputs" => Vec::<Value>::new(), "verify" => Vec::<Value>::new()}
    } else {
        let man = manifest.as_ref().unwrap();
        let input = jobj! {
            "identity" => m.identity.to_value(), "physical" => ident.physical.to_value(), "profile" => ident.profile.clone(),
            "device" => dev, "tmp" => tmp.to_string_lossy().into_owned(), "stem" => target.stem.clone(), "target" => target.to_value(),
        };
        let ctx = handlers::CallCtx { action: "dump-plan", device: Some(dev), ident: Some(ident), tag: Some(&m.tag), resolution: res.as_ref(), target: Some(&target), checked: true, ..Default::default() };
        let c = handlers::invoke(man, cfg, "dump-plan", &[], &input, &ctx, Duration::from_secs(10))?;
        if c.code != 0 {
            return Err(c.out["error"].as_str().map(|s| s.to_string()).unwrap_or_else(|| c.stderr.trim().to_string()));
        }
        c.out
    };
    let est = match ident.physical.media {
        crate::device::MediaKind::Cd => ident.physical.leadout as i64 * 2352,
        _ => ident.physical.capacity as i64 * 2048,
    };
    let plan = jobj! {
        "kind" => kind, "device" => dev, "system" => o.handler.clone(),
        "title" => res.as_ref().map(|r| r.name.clone()).unwrap_or_else(|| t("unknown-disc").into()),
        "key" => m.identity.key(), "identity" => m.identity.to_value(),
        "resolution" => res.as_ref().map(|r| r.to_value()), "target" => target.to_value(),
        "tmp" => tmp.to_string_lossy().into_owned(), "stem" => target.stem.clone(),
        "steps" => steps_out["steps"].clone(), "outputs" => steps_out["outputs"].clone(), "verify" => steps_out["verify"].clone(),
        "dump_plan" => steps_out["plan"].clone(), "exact" => steps_out["exact"].bool_or(true),
        "cart" => ident.physical.media == crate::device::MediaKind::Cart,
        "estimated_bytes" => est, "late_identify" => late, "play_after" => play_after,
        "options" => jobj!{"eject_after_read" => cfg.eject_after_read()},
    };
    let id = jobs::create(plan).map_err(|e| e.to_string())?;
    jobs::spawn(&id).map_err(|e| e.to_string())?;
    // Une copie de cartouche dure quelques secondes : pas de terminal.
    if ident.physical.media != crate::device::MediaKind::Cart {
        open_job_terminal(cfg, &id);
    }
    Ok(id)
}

/// Ouvre un terminal qui suit la tâche (`disc-launcher watch <id>`), si demandé.
fn open_job_terminal(cfg: &Config, id: &str) {
    if !cfg.job_terminal() {
        return;
    }
    let exe = paths::which("disc-launcher").map(|p| p.to_string_lossy().into_owned()).unwrap_or_else(|| "disc-launcher".into());
    let argv = vec![exe, "watch".to_string(), id.to_string()];
    match crate::terminal::command(cfg, &argv) {
        Some(cmd) => match crate::generic::spawn_detached(&cmd, &paths::log_dir().join("terminal.log")) {
            Ok(_) => dl_log!(info, "job", "terminal ouvert", "job" => id, "cmd" => cmd.join(" ")),
            Err(e) => dl_log!(warn, "job", format!("terminal impossible à ouvrir : {e}"), "job" => id),
        },
        None => dl_log!(warn, "job", "aucun terminal trouvé (réglez [general] terminal_command)", "job" => id),
    }
}

pub fn start_convert(_cfg: &Config, dev: &str, o: &Offer, ident: Option<&IdentResult>) -> Result<String, String> {
    let input = o.situation.as_ref().and_then(|s| s.existing()).ok_or("aucune copie à convertir")?.to_path_buf();
    let target = o.target.clone().ok_or("cible inconnue")?;
    let manifest = handlers::get(&o.handler).ok_or("gestionnaire introuvable")?;
    let tmp = tmp_dir_for(&target, &format!("conv-{}", crate::log::compact_stamp()));
    let out = tmp.join(format!("{}.{}", target.stem, target.ext));
    let p = crate::generic::convert_plan(&manifest, &input.to_string_lossy(), &out.to_string_lossy()).map_err(|e| e.message)?;
    let m = ident.and_then(|i| i.primary()).ok_or("disque non identifié")?;
    let plan = jobj! {
        "kind" => "convert", "device" => dev, "system" => o.handler.clone(),
        "title" => o.resolution.as_ref().map(|r| r.name.clone()).unwrap_or_default(),
        "key" => m.identity.key(), "identity" => m.identity.to_value(),
        "resolution" => o.resolution.as_ref().map(|r| r.to_value()), "target" => target.to_value(),
        "tmp" => tmp.to_string_lossy().into_owned(), "stem" => target.stem.clone(), "input" => input.to_string_lossy().into_owned(),
        "steps" => p["steps"].clone(), "outputs" => vec![Value::from(format!("{}.{}", target.stem, target.ext))], "verify" => Vec::<Value>::new(),
        "options" => jobj!{"eject_after_read" => false, "remove_input" => false},
    };
    let id = jobs::create(plan).map_err(|e| e.to_string())?;
    jobs::spawn(&id).map_err(|e| e.to_string())?;
    Ok(id)
}

pub fn start_verify(dev: &str, o: &Offer) -> Result<String, String> {
    let path = o.situation.as_ref().and_then(|s| s.existing()).ok_or("aucune copie à vérifier")?.to_string_lossy().into_owned();
    let manifest = handlers::get(&o.handler).ok_or("gestionnaire introuvable")?;
    let cmd = crate::generic::verify_command(&manifest, &path).ok_or("aucune commande de vérification pour ce format")?;
    let target = o.target.clone().ok_or("cible inconnue")?;
    let tmp = tmp_dir_for(&target, &format!("verify-{}", crate::log::compact_stamp()));
    let plan = jobj! {
        "kind" => "verify", "device" => dev, "system" => o.handler.clone(), "title" => Path::new(&path).file_name().map(|s| s.to_string_lossy().into_owned()),
        "input" => path, "target" => target.to_value(), "tmp" => tmp.to_string_lossy().into_owned(),
        "steps" => vec![jobj!{"name" => "verify", "command" => cmd, "helper" => false, "progress" => "generic"}],
        "options" => jobj!{"eject_after_read" => false},
    };
    let id = jobs::create(plan).map_err(|e| e.to_string())?;
    jobs::spawn(&id).map_err(|e| e.to_string())?;
    Ok(id)
}
