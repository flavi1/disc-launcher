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
    /// État du tiroir (lecteurs optiques).
    tray: Option<DriveStatus>,
    /// Fabricant et modèle du lecteur optique.
    model: Option<String>,
    /// Lecteur à plateau motorisé.
    can_close: bool,
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
    Panel(crate::panel::PanelEvent),
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
    /// Fenêtre « Disques et périphériques » (disc-launcher-panel).
    panel: Option<crate::panel::Panel>,
    panel_cmd: Option<Vec<String>>,
    /// Faux si le panneau ne peut pas s'afficher (pas de GTK 3, pas
    /// d'affichage) : repli sur les notifications.
    panel_ok: bool,
    panel_ready: bool,
    panel_visible: bool,
    /// Ramener la fenêtre au premier plan au prochain envoi.
    panel_raise: bool,
    panel_starts: u32,
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
    // Instance unique par utilisateur, quel que soit XDG_RUNTIME_DIR (un démon
    // lancé depuis un terminal et celui de la session ne doivent pas coexister).
    let lock = match sys::instance_lock("disc-launcherd") {
        Ok(Some(l)) => l,
        Ok(None) => {
            dl_log!(info, "daemon", "une autre instance tourne déjà");
            return 0;
        }
        Err(e) => {
            dl_log!(error, "daemon", format!("verrou d'instance : {e}"));
            return 1;
        }
    };
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
        panel: None,
        panel_cmd: None,
        panel_ok: false,
        panel_ready: false,
        panel_visible: false,
        panel_raise: false,
        panel_starts: 0,
    };
    d.panel_cmd = crate::panel::command(&d.cfg);
    d.panel_ok = d.panel_cmd.is_some();
    d.start_panel();
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
    // Sondage de secours (toutes les 2 s) : état du tiroir/disque.
    std::thread::Builder::new()
        .name("poll".into())
        .spawn(move || {
            let mut last: HashMap<String, Option<DriveStatus>> = HashMap::new();
            loop {
                for dev in crate::device::list_drives() {
                    let st = cdrom::drive_status(&dev).ok();
                    let prev = last.insert(dev.clone(), st);
                    // Tout changement (disque, tiroir ouvert ou fermé), y compris
                    // une éjection faite hors de disc-launcher.
                    let changed = matches!(&prev, Some(a) if *a != st);
                    if changed && tx.send(Event::Media(dev)).is_err() {
                        return;
                    }
                }
                std::thread::sleep(Duration::from_secs(2));
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
        self.drives.entry(dev.to_string()).or_insert_with(|| Drive { dev: dev.to_string(), state: DState::Empty, gen: 0, due: None, settle_until: None, ident: None, offer: None, notif: None, job: None, silent: false, batch: None, usb: None, tray: None, model: None, can_close: false })
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
            self.update_ui();
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
        // La fenêtre appartient à cette instance : la fermer (et la réclamer)
        // avant de se remplacer, sinon elle reste orpheline.
        self.panel = None;
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
            Event::Panel(e) => self.on_panel(e),
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
        self.drive(dev).tray = status.as_ref().ok().copied();
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
                    let r = identify_mounting(&dev2, &opts);
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
        let mut offer = match res {
            Ok(o) => o,
            Err(e) => {
                dl_log!(warn, "resolve", e, "drive" => dev);
                self.drive(dev).state = DState::Idle;
                return;
            }
        };
        // Actions personnalisées de la configuration ([actions.<nom>]).
        crate::custom::extend(&self.cfg, &offer.handler, &mut offer.actions);
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
        if self.use_panel() {
            self.panel_visible = true;
            self.panel_raise = true;
            return;
        }
        if self.use_dialog {
            self.refresh_dialog();
            return;
        }
        let replaces = self.drive(dev).notif.unwrap_or(0);
        let Some(nt) = &self.notifier else { return };
        let icon = offer_icon(&o, ident.as_ref());
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

    /// La fenêtre « Disques et périphériques » remplace les notifications.
    fn use_panel(&self) -> bool {
        self.panel_ok && self.panel_cmd.is_some()
    }

    fn start_panel(&mut self) {
        let Some(cmd) = self.panel_cmd.clone() else { return };
        if self.panel.is_some() || !self.panel_ok || self.panel_starts >= 5 {
            return;
        }
        self.panel_starts += 1;
        let (ptx, prx) = mpsc::channel::<crate::panel::PanelEvent>();
        match crate::panel::Panel::start(&cmd, ptx) {
            Ok(p) => {
                self.panel = Some(p);
                self.panel_ready = false;
                let tx = self.tx.clone();
                std::thread::spawn(move || {
                    while let Ok(e) = prx.recv() {
                        if tx.send(Event::Panel(e)).is_err() {
                            break;
                        }
                    }
                });
            }
            Err(e) => {
                dl_log!(warn, "panel", format!("fenêtre indisponible ({e}) : notifications"), "cmd" => cmd.join(" "));
                self.panel_ok = false;
            }
        }
    }

    fn on_panel(&mut self, e: crate::panel::PanelEvent) {
        use crate::panel::PanelEvent;
        match e {
            PanelEvent::Ready => {
                self.panel_ready = true;
                dl_log!(info, "panel", "fenêtre « Disques et périphériques » prête");
            }
            PanelEvent::Hidden => self.hide_all(),
            PanelEvent::Action(id, key) => {
                dl_log!(info, "panel", "action choisie", "drive" => id, "action" => key);
                if let Some(j) = id.strip_prefix("job:") {
                    if key == "cancel" {
                        if let Err(e) = jobs::cancel(j) {
                            dl_log!(warn, "job", e, "job" => j);
                        }
                    }
                    return;
                }
                self.act(&id, &key);
            }
            PanelEvent::Exited(_) => {
                let code = self.panel.as_mut().and_then(|p| {
                    for _ in 0..20 {
                        if let Some(c) = p.exit_code() {
                            return Some(c);
                        }
                        std::thread::sleep(Duration::from_millis(50));
                    }
                    None
                });
                self.panel = None;
                if !self.panel_ready || code == Some(Some(3)) {
                    // Pas de GTK 3 ou pas d'affichage : notifications.
                    dl_log!(warn, "panel", "fenêtre indisponible (GTK 3 ou affichage absent) : propositions en notifications");
                    self.panel_ok = false;
                    if self.panel_visible {
                        self.panel_visible = false;
                        self.show_all();
                    }
                } else {
                    dl_log!(warn, "panel", "la fenêtre s'est arrêtée ; elle sera relancée", "code" => format!("{code:?}"));
                }
                self.panel_ready = false;
            }
        }
    }

    /// Des propositions sont-elles affichées ?
    fn offers_visible(&self) -> bool {
        if self.use_panel() {
            return self.panel_visible;
        }
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
        self.panel_visible = false;
    }

    /// Réaffiche les propositions de tous les périphériques présents.
    fn show_all(&mut self) {
        if self.use_panel() {
            self.panel_visible = true;
            self.panel_raise = true;
            self.start_panel();
            return;
        }
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
        // Lecteur à plateau : après l'ouverture, la fenêtre reste affichée
        // jusqu'à « Fermer le plateau » (on y pose le disque entre-temps).
        let keep = matches!(key, "eject" | "open-tray") && dev.starts_with("/dev/sr") && self.drives.get(dev).is_some_and(|d| d.can_close) && self.use_panel() && self.panel_visible;
        if !keep {
            self.hide_all();
        }
        self.perform(dev, key);
    }

    fn on_tray(&mut self, e: crate::tray::TrayEvent) {
        match e {
            crate::tray::TrayEvent::Activate => {
                if self.offers_visible() {
                    self.hide_all();
                } else {
                    self.show_all();
                }
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

    /// Élément « tâche » de la fenêtre : titre, étape, progression, Annuler.
    fn job_item(&self, id: &str) -> Option<Value> {
        let (st, plan) = (jobs::read_state(id)?, jobs::read_plan(id)?);
        if !jobs::is_active(&st) {
            return None;
        }
        let label = match plan["kind"].as_str() {
            Some("verify") => t("job-verify"),
            Some("convert") => t("convert"),
            _ if plan["cart"].bool_or(false) => t("job-copy"),
            _ => t("job-dump"),
        };
        let pct = st["progress"].i64_or(0).clamp(0, 100);
        let step = format!("{} {}/{} — {}", t("step"), st["step_index"].i64_or(1), st["steps_total"].i64_or(1), st["step"].str_or("…"));
        Some(jobj! {
            "id" => format!("job:{id}"), "icon" => crate::tray::ICON,
            "title" => format!("{label} : {}", plan["title"].str_or("")),
            "lines" => vec![Value::from(step)],
            "progress" => jobj! {"fraction" => pct as f64 / 100.0, "text" => format!("{pct} %")},
            "actions" => vec![Value::from(vec![Value::from("cancel"), Value::from(t("cancel"))])],
        })
    }

    /// Éléments de la fenêtre : lecteurs optiques (même vides), cartouches,
    /// volumes USB, puis tâches sans lecteur (vérification, conversion).
    fn panel_items(&mut self) -> Vec<(u8, Value)> {
        let pairs = |a: &[(String, String)]| -> Value { a.iter().map(|(k, l)| Value::from(vec![Value::from(k.clone()), Value::from(l.clone())])).collect::<Vec<_>>().into() };
        let mut out: Vec<(u8, Value)> = vec![];
        let mut shown_jobs: Vec<String> = vec![];
        let devs: Vec<String> = self.drives.keys().cloned().collect();
        for dev in devs {
            if dev.starts_with("/dev/sr") && self.drives[&dev].model.is_none() {
                self.drive(&dev).model = Some(cdrom::drive_model(&dev));
                self.drive(&dev).can_close = cdrom::can_close_tray(&dev);
            }
            let d = &self.drives[&dev];
            if d.state == DState::Job {
                if let Some(it) = d.job.as_deref().and_then(|j| self.job_item(j)) {
                    shown_jobs.push(d.job.clone().unwrap_or_default());
                    out.push((0, it));
                    continue;
                }
            }
            let optical = dev.starts_with("/dev/sr");
            let short = dev.trim_start_matches("/dev/").to_string();
            let drive_line = d.model.as_deref().filter(|m| !m.is_empty()).map(|m| format!("{} : {m} ({short})", t("drive-label"))).unwrap_or_else(|| format!("{} ({short})", t("drive")));
            let busy = matches!(d.state, DState::Settling | DState::Identifying | DState::Resolving);
            if let (Some(o), false) = (&d.offer, busy && optical) {
                let (summary, body) = offer_text(&self.cfg, o, d.ident.as_ref());
                let mut lines: Vec<Value> = vec![];
                if optical {
                    lines.push(drive_line.clone().into());
                }
                lines.extend(body.lines().filter(|l| !l.is_empty()).map(|l| Value::from(l.to_string())));
                let mut usage = Value::Null;
                if let Some(v) = &d.usb {
                    if let Some(mp) = v.mount.clone().or_else(|| crate::device::find_mount_point(&v.dev)) {
                        if let Ok((total, free)) = sys::fs_usage(&mp) {
                            if total > 0 {
                                usage = jobj! {"fraction" => (total - free.min(total)) as f64 / total as f64,
                                    "text" => format!("{} {} {}", crate::usb::human_size(free), t("free-of"), crate::usb::human_size(total))};
                            }
                        }
                    }
                }
                let kind = if optical { 0 } else if d.usb.is_some() { 2 } else { 1 };
                out.push((kind, jobj! {"id" => dev.clone(), "icon" => offer_icons(o, d.ident.as_ref()).join(","), "title" => summary, "lines" => lines, "usage" => usage, "progress" => Value::Null, "actions" => pairs(&o.actions)}));
                continue;
            }
            if !optical {
                continue;
            }
            // Lecteur sans proposition : vide, tiroir ouvert, lecture en cours…
            let (state_txt, actions): (&str, Vec<(String, String)>) = if busy {
                (t("drive-reading"), vec![("eject".into(), t("eject").into())])
            } else {
                match d.tray {
                    Some(DriveStatus::TrayOpen) => (t("tray-open"), vec![("close-tray".into(), t("close-tray").into())]),
                    Some(DriveStatus::DiscOk) => {
                        let blank = d.ident.as_ref().and_then(|i| i.primary()).is_some_and(|m| m.tag == "blank");
                        (if blank { t("blank-disc") } else { t("unknown-content") }, vec![("eject".into(), t("eject").into())])
                    }
                    _ => (t("drive-empty"), vec![("open-tray".into(), t("open-tray").into())]),
                }
            };
            let title = format!("{} ({short}) — {state_txt}", t("drive"));
            let lines: Vec<Value> = d.model.iter().filter(|m| !m.is_empty()).map(|m| Value::from(m.clone())).collect();
            out.push((0, jobj! {"id" => dev.clone(), "icon" => "drive-optical,media-optical", "title" => title, "lines" => lines, "usage" => Value::Null, "progress" => Value::Null, "actions" => pairs(&actions), "idle" => true}));
        }
        for id in self.active_jobs.clone() {
            if !shown_jobs.contains(&id) {
                if let Some(it) = self.job_item(&id) {
                    out.push((3, it));
                }
            }
        }
        out.sort_by_key(|(k, _)| *k);
        out
    }

    /// Fenêtre et icône : état à jour (envoyé seulement s'il a changé).
    fn update_ui(&mut self) {
        let items: Vec<Value> = self.panel_items().into_iter().map(|(_, v)| v).collect();
        let present: Vec<&Value> = items.iter().filter(|v| !v["idle"].bool_or(false)).collect();
        if let Some(tx) = &self.tray {
            let tooltip = if present.is_empty() { t("tray-empty").to_string() } else { present.iter().map(|v| v["title"].str_or("").to_string()).collect::<Vec<_>>().join("\n") };
            let m = crate::tray::Model { active: !present.is_empty(), tooltip };
            if self.tray_model.as_ref() != Some(&m) {
                let _ = tx.send(m.clone());
                self.tray_model = Some(m);
            }
        }
        if !self.use_panel() {
            return;
        }
        if self.panel.is_none() && self.panel_visible {
            self.start_panel();
        }
        let raise = std::mem::take(&mut self.panel_raise);
        let state = jobj! {"visible" => self.panel_visible, "raise" => raise, "title" => t("panel-title"), "empty" => t("tray-empty"), "items" => items};
        if let Some(p) = self.panel.as_mut() {
            if let Err(e) = p.send(&state) {
                dl_log!(debug, "panel", format!("envoi impossible : {e}"));
            }
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

    /// Actions sur le périphérique lui-même (sans proposition nécessaire) :
    /// tiroir, montage, gestionnaire de fichiers. Vrai si `key` en est une.
    fn perform_device(&mut self, dev: &str, key: &str) -> bool {
        if key.starts_with(crate::custom::PREFIX) {
            let Some(a) = crate::custom::get(&self.cfg, key) else {
                self.error_notif(dev, &format!("action inconnue : {key} (voir [actions] dans la configuration)"));
                return true;
            };
            let d = self.drives.get(dev);
            let o = d.and_then(|d| d.offer.clone());
            let ident = d.and_then(|d| d.ident.clone());
            let usb_label = d.and_then(|d| d.usb.as_ref().map(|v| v.label.clone()));
            let label = usb_label.or_else(|| ident.as_ref().and_then(|i| i.volume_id.clone()));
            let existing = o.as_ref().and_then(|o| o.situation.as_ref()).and_then(|s| s.existing()).map(|p| p.to_string_lossy().into_owned());
            let real_dev = d.and_then(|d| d.usb.as_ref().map(|v| v.dev.clone())).unwrap_or_else(|| dev.to_string());
            let handler = o.as_ref().map(|o| o.handler.clone()).unwrap_or_default();
            let ctx = crate::custom::Ctx {
                handler: &handler,
                device: Some(&real_dev),
                ident: ident.as_ref(),
                tag: o.as_ref().map(|o| o.tag.as_str()),
                resolution: o.as_ref().and_then(|o| o.resolution.as_ref()),
                existing: existing.as_deref(),
                label: label.as_deref(),
            };
            match crate::custom::run(&self.cfg, &a, &ctx) {
                Ok(cmd) => dl_log!(info, "action", "action personnalisée lancée", "drive" => dev, "action" => a.name, "cmd" => cmd.join(" ")),
                Err(e) => self.error_notif(dev, &e),
            }
            return true;
        }
        match key {
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
            "eject" | "open-tray" | "close-tray" => {
                if !dev.starts_with("/dev/sr") {
                    return false;
                }
                let tx = self.tx.clone();
                let d = dev.to_string();
                let key = key.to_string();
                std::thread::spawn(move || {
                    let r = if key == "close-tray" {
                        cdrom::close_tray(&d)
                    } else {
                        if crate::device::find_mount_point(&d).is_some() {
                            if let Err(e) = crate::dbus::udisks_unmount(&d) {
                                dl_log!(warn, "eject", format!("démontage impossible : {e}"), "drive" => d);
                            }
                        }
                        cdrom::eject(&d)
                    };
                    match r {
                        Ok(()) => dl_log!(info, "eject", "tiroir actionné", "drive" => d, "action" => key),
                        Err(e) => {
                            let _ = tx.send(Event::Error(d.clone(), format!("{} : {e}", t(&key))));
                        }
                    }
                    // État du tiroir à jour sans attendre le sondage.
                    let _ = tx.send(Event::Media(d));
                });
            }
            "mount" | "unmount" | "safe-remove" => {
                let Some(v) = self.drives.get(dev).and_then(|d| d.usb.clone()) else { return false };
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
            _ => return false,
        }
        true
    }

    /// Exécute une action sur le disque d'un lecteur.
    fn perform(&mut self, dev: &str, key: &str) {
        if self.perform_device(dev, key) {
            return;
        }
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

    /// Fin d'une tâche liée à un lecteur : le lecteur redevient libre.
    fn job_released(&mut self, dev: &str, id: &str) {
        if dev.is_empty() || self.drives.get(dev).map_or(true, |d| d.job.as_deref() != Some(id)) {
            return;
        }
        let d = self.drive(dev);
        d.job = None;
        d.state = DState::Idle;
        // Le disque a pu être éjecté pendant la tâche (les changements de
        // média sont ignorés pendant un dump) : relire l'état du lecteur, sans
        // nouvelle proposition.
        if dev.starts_with("/dev/sr") {
            d.gen += 1;
            d.state = DState::Settling;
            d.silent = true;
            d.due = Some(Instant::now());
            d.settle_until = Some(Instant::now() + Duration::from_secs(5));
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
                self.job_released(&dev, id);
            }
            return;
        }
        // Fenêtre : la progression y est affichée ; seule la fin est notifiée.
        if !finished && self.use_panel() {
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
            self.job_released(&dev, id);
            match status.as_str() {
                "done" => {
                    let path = st["result"]["path"].str_or("").to_string();
                    let ver = match st["result"]["verified"].as_str() {
                        // Vérification d'un fichier : intégrité seulement (chdman verify…).
                        Some("ok") if plan["kind"].as_str() == Some("verify") => format!("\n✓ {}", t("integrity-ok")),
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
                    let done = match plan["kind"].as_str() {
                        Some("verify") => t("verify-done"),
                        _ if plan["cart"].bool_or(false) => t("job-copy-done"),
                        _ => t("job-done"),
                    };
                    Notification { replaces, summary: format!("{done} : {title}"), body: format!("{shown}{ver}{extra}"), icon: "media-optical".into(), actions, resident: false, progress: None, urgency: 1, timeout_ms: -1 }
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

/// Identification d'un lecteur ; un disque UDF seul (Blu-ray vidéo…) non
/// monté est monté par udisks2, puis réidentifié. Si le montage échoue parce
/// qu'une autre identification (ou le bureau) l'a monté entre-temps, le
/// disque est tout de même relu par son point de montage.
pub fn identify_mounting(dev: &str, opts: &identify::Options) -> Result<IdentResult, String> {
    let r = identify::identify_device(dev, opts).map_err(|e| e.to_string())?;
    if !r.warnings.iter().any(|w| w == "udf-not-mounted") {
        return Ok(r);
    }
    // Juste après l'insertion, udisks2 n'a pas toujours fini d'examiner le
    // disque (« No such interface …Filesystem ») : réessayer quelques secondes.
    let mut res = crate::dbus::udisks_mount(dev);
    for _ in 0..15 {
        match &res {
            Err(e) if e.to_string().contains("UDisks2.Filesystem") => {
                std::thread::sleep(Duration::from_secs(1));
                res = crate::dbus::udisks_mount(dev);
            }
            _ => break,
        }
    }
    match res {
        Ok(mp) => dl_log!(info, "identify", "système de fichiers UDF monté par udisks2", "drive" => dev, "mount" => mp),
        Err(e) => {
            // Déjà monté (course avec une autre identification) : on attend le point de montage.
            let mut found = false;
            for _ in 0..20 {
                if crate::device::find_mount_point(dev).is_some() {
                    found = true;
                    break;
                }
                std::thread::sleep(Duration::from_millis(250));
            }
            if !found {
                dl_log!(warn, "identify", format!("montage UDF impossible : {e}"), "drive" => dev);
                return Ok(r);
            }
        }
    }
    identify::identify_device(dev, opts).map_err(|e| e.to_string())
}

/// Icônes candidates d'une proposition, de la plus précise à la plus
/// générique (noms freedesktop ; la fenêtre prend la première que le thème
/// possède, Breeze et Adwaita n'ayant pas les mêmes).
pub fn offer_icons(o: &Offer, ident: Option<&IdentResult>) -> Vec<String> {
    let own = format!("disc-launcher-{}", o.handler);
    let mut v: Vec<&str> = vec![];
    let cart = ident.is_some_and(|i| i.physical.media == crate::device::MediaKind::Cart);
    if cart {
        // Types MIME des ROM (fournis par la plupart des thèmes).
        v.extend(match o.handler.as_str() {
            "n64" => &["application-x-n64-rom"][..],
            "snes" => &["application-x-snes-rom"][..],
            "megadrive" => &["application-x-genesis-rom", "application-x-sega-genesis-rom"][..],
            "gb" => &["application-x-gameboy-rom"][..],
            "gbc" => &["application-x-gameboy-color-rom", "application-x-gameboy-rom"][..],
            "gba" => &["application-x-gba-rom", "application-x-gameboy-advance-rom"][..],
            "mastersystem" => &["application-x-sms-rom"][..],
            "gamegear" => &["application-x-gamegear-rom"][..],
            _ => &[][..],
        });
        v.extend(["input-gaming", "media-flash"]);
    } else {
        match o.handler.as_str() {
            "usb" if o.describe["sd"].bool_or(false) => v.extend(["media-flash-sd-mmc", "media-flash", "drive-removable-media"]),
            "usb" => v.extend(["drive-removable-media-usb", "drive-removable-media"]),
            "cdda" => v.extend(["media-optical-audio", "audio-x-generic"]),
            "dcim" | "photo-cd" => v.extend(["camera-photo", "folder-pictures", "image-x-generic"]),
            "dvd-video" | "dvd-vr" | "vcd" | "svcd" => v.extend(["media-optical-dvd-video", "media-optical-dvd", "media-optical-video", "video-x-generic"]),
            "bluray-video" | "bdav" | "avchd" | "hddvd-video" => v.extend(["media-optical-blu-ray", "media-optical-video", "video-x-generic"]),
            "dvd-audio" | "data-audio" => v.extend(["media-optical-audio", "audio-x-generic"]),
            "data-video" => v.extend(["media-optical-video", "video-x-generic"]),
            "data" => v.extend(["media-optical-data"]),
            _ if !o.media => v.extend(["input-gaming"]),
            _ => {}
        }
    }
    v.push(if o.handler == "usb" { "drive-removable-media" } else { "media-optical" });
    let mut out = vec![];
    if icon_exists(&own) {
        out.push(own);
    }
    out.extend(v.into_iter().map(|s| s.to_string()));
    out
}

/// Icône unique (notifications) : l'icône propre si installée, sinon la plus générique.
fn offer_icon(o: &Offer, ident: Option<&IdentResult>) -> String {
    let v = offer_icons(o, ident);
    if v.first().is_some_and(|f| f.starts_with("disc-launcher-")) {
        return v[0].clone();
    }
    v.last().cloned().unwrap_or_else(|| "media-optical".into())
}

/// Proposition pour une clé USB : ouvrir, monter ou démonter, retirer.
pub fn usb_offer(v: &crate::usb::Volume) -> Offer {
    let mounted = v.mount.is_some() || crate::device::find_mount_point(&v.dev).is_some();
    let mut actions = vec![("open-files".to_string(), t("open-files").to_string())];
    if !v.dev.starts_with("usb:") {
        // Démonté : rien à faire avant de le retirer (indication, pas d'action).
        if mounted {
            actions.push(("unmount".into(), t("unmount").into()));
            actions.push(("safe-remove".into(), t("safe-remove").into()));
        } else {
            actions.push(("mount".into(), t("mount").into()));
        }
    }
    let size = if v.size > 0 { format!(" ({})", crate::usb::human_size(v.size)) } else { String::new() };
    let kind = if v.sd { t("sd-card") } else { t("usb-key") };
    let model = if v.model.is_empty() || v.label.is_empty() { String::new() } else { format!("{} · ", v.model) };
    let where_ = match &v.mount {
        Some(m) => format!("{model}{} {}", t("mounted-on"), m.display()),
        None => format!("{model}{} {}", t("not-mounted"), t("safe-to-remove")),
    };
    Offer {
        handler: "usb".into(),
        media: true,
        handler_name: format!("{kind} — {}{size}", v.name()),
        tag: "usb".into(),
        confidence: Confidence::Strong,
        resolution: None,
        target: None,
        situation: None,
        collision: false,
        describe: jobj! {"device" => v.dev.clone(), "fs" => v.fstype.clone(), "label" => v.label.clone(), "model" => v.model.clone(), "status" => where_, "sd" => v.sd},
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
