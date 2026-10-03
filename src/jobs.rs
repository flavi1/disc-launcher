//! Tâches longues (dump, conversion, vérification) : modèle de fichiers,
//! supervision sans gestionnaire de services, exécution (section 10).
//!
//! Dossier d'une tâche : `$XDG_STATE_HOME/disc-launcher/jobs/<id>/`
//! - `plan.json`  : plan figé au lancement
//! - `state.json` : état, réécrit atomiquement
//! - `job.log`    : journal de la tâche (sortie des outils incluse)
//! - `lock`       : verrou `flock` tenu par la tâche pendant toute sa vie

use crate::collection::{self, Collection};
use crate::config::Config;
use crate::identity::Identity;
use crate::json::{self, Value};
use crate::naming::{self, NameConfidence, Resolution, Target};
use crate::refdb::RefDb;
use crate::{jobj, paths, sys, util};
use std::fs::{File, OpenOptions};
use std::io::{self, BufRead, Read};
use std::os::fd::AsRawFd;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::sync::mpsc;
use std::time::{Duration, Instant};

pub fn job_dir(id: &str) -> PathBuf {
    paths::jobs_dir().join(id)
}

pub fn new_id() -> String {
    use std::sync::atomic::{AtomicU32, Ordering};
    static N: AtomicU32 = AtomicU32::new(0);
    format!("{}-{:x}{}", crate::log::compact_stamp(), sys::pid(), N.fetch_add(1, Ordering::SeqCst))
}

pub fn read_json(p: &Path) -> Option<Value> {
    json::parse(&std::fs::read_to_string(p).ok()?).ok()
}

pub fn read_plan(id: &str) -> Option<Value> {
    read_json(&job_dir(id).join("plan.json"))
}
pub fn read_state(id: &str) -> Option<Value> {
    read_json(&job_dir(id).join("state.json"))
}

pub fn write_state(id: &str, st: &Value) {
    let _ = paths::write_atomic(&job_dir(id).join("state.json"), st.to_pretty().as_bytes());
}

/// Crée le dossier de la tâche et son plan ; renvoie l'identifiant.
pub fn create(mut plan: Value) -> io::Result<String> {
    let id = new_id();
    let dir = job_dir(&id);
    std::fs::create_dir_all(&dir)?;
    plan.set("id", id.clone());
    paths::write_atomic(&dir.join("plan.json"), plan.to_pretty().as_bytes())?;
    std::fs::write(dir.join("lock"), b"")?;
    write_state(&id, &jobj! {"id" => id.clone(), "status" => "pending", "kind" => plan["kind"].clone(), "progress" => 0, "created" => util::now_secs(), "title" => plan["title"].clone(), "device" => plan["device"].clone(), "system" => plan["system"].clone()});
    Ok(id)
}

/// Lance `disc-launcher-job --detach <id>` (double fork : la tâche survit au démon).
pub fn spawn(id: &str) -> io::Result<()> {
    let exe = paths::which("disc-launcher-job").ok_or_else(|| io::Error::new(io::ErrorKind::NotFound, "disc-launcher-job introuvable"))?;
    let st = Command::new(exe).arg("--detach").arg(id).stdin(Stdio::null()).stdout(Stdio::null()).stderr(Stdio::null()).status()?;
    if !st.success() {
        return Err(io::Error::new(io::ErrorKind::Other, "échec du lancement de la tâche"));
    }
    Ok(())
}

/// La tâche tient-elle son verrou ?
pub fn alive(id: &str) -> bool {
    let Ok(f) = File::open(job_dir(id).join("lock")) else { return false };
    match sys::try_lock(f.as_raw_fd(), true) {
        Ok(true) => {
            sys::unlock(f.as_raw_fd());
            false
        }
        Ok(false) => true,
        Err(_) => false,
    }
}

/// Tâches, des plus récentes aux plus anciennes.
pub fn list() -> Vec<(String, Value)> {
    let mut v: Vec<(String, Value)> = std::fs::read_dir(paths::jobs_dir())
        .map(|it| it.flatten().filter_map(|e| {
            let id = e.file_name().to_string_lossy().into_owned();
            read_state(&id).map(|s| (id, s))
        }).collect())
        .unwrap_or_default();
    v.sort_by(|a, b| b.0.cmp(&a.0));
    v
}

pub fn is_active(st: &Value) -> bool {
    matches!(st["status"].as_str(), Some("pending") | Some("running"))
}

/// Annulation : SIGTERM au processus de la tâche, qui arrête son étape.
pub fn cancel(id: &str) -> Result<(), String> {
    let st = read_state(id).ok_or("tâche inconnue")?;
    if !alive(id) {
        return Err("la tâche n'est pas en cours".into());
    }
    let pid = st["pid"].as_i64().ok_or("PID inconnu")? as i32;
    sys::send_signal(pid, sys::SIGTERM).map_err(|e| e.to_string())
}

/// Au démarrage du démon : les tâches « running » dont le verrou est libre
/// sont mortes → `interrupted`, dossier temporaire supprimé.
pub fn reconcile() -> Vec<String> {
    let mut out = vec![];
    for (id, mut st) in list() {
        if !is_active(&st) || alive(&id) {
            continue;
        }
        let age = util::now_secs() - st["created"].as_i64().unwrap_or(0);
        if st["status"].as_str() == Some("pending") && age < 60 {
            continue;
        }
        if let Some(plan) = read_plan(&id) {
            if let Some(t) = plan["tmp"].as_str() {
                let _ = std::fs::remove_dir_all(t);
            }
        }
        st.set("status", "interrupted");
        st.set("finished", util::now_secs());
        st.set("message", "tâche interrompue (processus disparu)");
        write_state(&id, &st);
        out.push(id);
    }
    out
}

/// Conservation : 30 jours et 100 tâches terminées au plus.
pub fn prune(days: i64, max: usize) {
    let now = util::now_secs();
    let mut kept = 0;
    for (id, st) in list() {
        if is_active(&st) {
            continue;
        }
        let t = st["finished"].as_i64().or(st["created"].as_i64()).unwrap_or(now);
        kept += 1;
        if kept > max || now - t > days * 86400 {
            let _ = std::fs::remove_dir_all(job_dir(&id));
        }
    }
}

/// Tâche interrompue la plus récente pour cette clé d'identité.
pub fn interrupted_for_key(key: &str) -> Option<String> {
    list().into_iter().find(|(id, st)| st["status"].as_str() == Some("interrupted") && read_plan(id).map_or(false, |p| p["key"].as_str() == Some(key))).map(|(id, _)| id)
}

// ======================================================================
// Exécution d'une tâche (processus `disc-launcher-job`)
// ======================================================================

struct Runner {
    id: String,
    plan: Value,
    state: Value,
    last_write: Instant,
    last_notify: Instant,
}

impl Runner {
    fn save(&mut self, force: bool) {
        if force || self.last_write.elapsed() > Duration::from_millis(900) {
            self.state.set("heartbeat", util::now_secs());
            write_state(&self.id, &self.state);
            self.last_write = Instant::now();
        }
        if force || self.last_notify.elapsed() > Duration::from_secs(2) {
            crate::control::notify(&jobj! {"cmd" => "job-update", "id" => self.id.clone()});
            self.last_notify = Instant::now();
        }
    }
    fn set_progress(&mut self, step_index: usize, steps: usize, pct: f64) {
        let overall = ((step_index as f64 + pct / 100.0) / steps.max(1) as f64 * 100.0).clamp(0.0, 100.0);
        self.state.set("progress", overall.round() as i64);
        self.save(false);
    }
}

/// Glob simple : `*` dans le dernier composant uniquement.
pub fn glob(dir: &Path, pattern: &str) -> Vec<PathBuf> {
    let (sub, pat) = match pattern.rsplit_once('/') {
        Some((s, p)) => (dir.join(s), p.to_string()),
        None => (dir.to_path_buf(), pattern.to_string()),
    };
    if !pat.contains('*') {
        let p = sub.join(&pat);
        return if p.exists() { vec![p] } else { vec![] };
    }
    let parts: Vec<&str> = pat.split('*').collect();
    let mut out: Vec<PathBuf> = std::fs::read_dir(&sub)
        .map(|it| {
            it.flatten()
                .map(|e| e.path())
                .filter(|p| {
                    let n = p.file_name().map(|s| s.to_string_lossy().into_owned()).unwrap_or_default();
                    wildcard(&n, &parts)
                })
                .collect()
        })
        .unwrap_or_default();
    out.sort();
    out
}

fn wildcard(name: &str, parts: &[&str]) -> bool {
    let mut rest = name;
    for (i, p) in parts.iter().enumerate() {
        if i == 0 {
            if !rest.starts_with(p) {
                return false;
            }
            rest = &rest[p.len()..];
        } else if i == parts.len() - 1 {
            return rest.ends_with(p);
        } else {
            match rest.find(p) {
                Some(j) => rest = &rest[j + p.len()..],
                None => return false,
            }
        }
    }
    rest.is_empty() || parts.len() > 1
}

fn helper_path() -> Option<PathBuf> {
    for p in ["/usr/libexec/disc-launcher/disc-launcher-helper", "/usr/local/libexec/disc-launcher/disc-launcher-helper", "/usr/lib/disc-launcher/disc-launcher-helper"] {
        if Path::new(p).is_file() {
            return Some(PathBuf::from(p));
        }
    }
    paths::which("disc-launcher-helper")
}

/// Transforme une étape `helper = true` en appel `pkexec disc-launcher-helper`.
fn wrap_helper(cmd: &[String], plan: &Value, mode: &str) -> Result<Vec<String>, String> {
    if mode == "none" || sys::euid() == 0 {
        return Ok(cmd.to_vec());
    }
    let pk = paths::which("pkexec");
    let helper = helper_path();
    match (pk, helper) {
        (Some(pk), Some(h)) => {
            // Contrôle préalable (le fichier est lisible par tous) : un outil absent
            // de la liste ou introuvable à ses chemins autorisés ferait échouer
            // l'assistant après la demande de mot de passe.
            let conf = paths::sysconf_dir().join("helper-tools.toml");
            if let Ok(text) = std::fs::read_to_string(&conf) {
                if let Ok(v) = crate::toml::parse(&text) {
                    let t = v.get("tools").get(&cmd[0]);
                    if t.is_null() {
                        return Err(format!("{} n'est pas autorisé dans {}", cmd[0], conf.display()));
                    }
                    let cands: Vec<String> = match t["path"].as_str() {
                        Some(p) => vec![p.to_string()],
                        None => t["path"].strings(),
                    };
                    if !cands.iter().any(|p| Path::new(p).exists()) {
                        let found = paths::which(&cmd[0]).map(|p| format!(" ; il est installé ici : {}", p.display())).unwrap_or_default();
                        return Err(format!("{} introuvable aux chemins autorisés par {} ({}){found}", cmd[0], conf.display(), cands.join(", ")));
                    }
                }
            }
            let mut v = vec![pk.to_string_lossy().into_owned(), h.to_string_lossy().into_owned(), "run".into(), "--tool".into(), cmd[0].clone(), "--device".into(), plan["device"].str_or("").into(), "--out".into(), plan["tmp"].str_or("").into(), "--".into()];
            v.extend_from_slice(&cmd[1..]);
            Ok(v)
        }
        _ if mode == "pkexec" => Err("pkexec ou disc-launcher-helper introuvable".into()),
        _ => {
            dl_log!(warn, "job", "assistant privilégié indisponible : exécution sans privilège (les commandes constructeur peuvent échouer)");
            Ok(cmd.to_vec())
        }
    }
}

/// Exécute une commande, journalise sa sortie et suit sa progression.
fn run_command(r: &mut Runner, cmd: &[String], cwd: &Path, step_index: usize, steps: usize, progress: &str) -> Result<(), String> {
    use std::os::unix::process::CommandExt;
    dl_log!(info, "job", "commande", "job" => r.id, "step" => step_index + 1, "cmd" => cmd.join(" "));
    let mut c = Command::new(&cmd[0]);
    c.args(&cmd[1..]).current_dir(cwd).stdin(Stdio::null()).stdout(Stdio::piped()).stderr(Stdio::piped());
    unsafe {
        c.pre_exec(|| {
            sys::setpgid(0, 0);
            Ok(())
        });
    }
    let mut child = c.spawn().map_err(|e| format!("{} : {e}", cmd[0]))?;
    let pgid = child.id() as i32;
    let (tx, rx) = mpsc::channel::<String>();
    for stream in [child.stdout.take().map(|s| Box::new(s) as Box<dyn Read + Send>), child.stderr.take().map(|s| Box::new(s) as Box<dyn Read + Send>)].into_iter().flatten() {
        let tx = tx.clone();
        std::thread::spawn(move || {
            let mut rd = io::BufReader::new(stream);
            let mut buf = vec![];
            loop {
                buf.clear();
                // découpe sur \n et \r (barres de progression)
                let mut byte = [0u8; 1];
                loop {
                    match rd.read(&mut byte) {
                        Ok(0) => {
                            if !buf.is_empty() {
                                let _ = tx.send(String::from_utf8_lossy(&buf).into_owned());
                            }
                            return;
                        }
                        Ok(_) => {
                            if byte[0] == b'\n' || byte[0] == b'\r' {
                                break;
                            }
                            buf.push(byte[0]);
                        }
                        Err(_) => return,
                    }
                }
                if !buf.is_empty() && tx.send(String::from_utf8_lossy(&buf).into_owned()).is_err() {
                    return;
                }
            }
        });
    }
    drop(tx);
    let tool = Path::new(&cmd[0]).file_name().map(|s| s.to_string_lossy().into_owned()).unwrap_or_default();
    // Outil réel quand l'étape passe par pkexec et l'assistant.
    let via_helper = tool == "pkexec";
    let real_tool = if via_helper { cmd.iter().position(|a| a == "--tool").and_then(|i| cmd.get(i + 1)).cloned().unwrap_or(tool.clone()) } else { tool.clone() };
    let mut last_error: Option<String> = None;
    let mut unknown_drive = false;
    let mut last_logged_pct = -1.0;
    let mut term_sent: Option<Instant> = None;
    loop {
        match rx.recv_timeout(Duration::from_millis(200)) {
            Ok(line) => {
                let pct = if progress == "none" { None } else { util::last_percent(&line) };
                if let Some(p) = pct {
                    r.set_progress(step_index, steps, p);
                    if (p - last_logged_pct).abs() >= 10.0 || crate::log::enabled(crate::log::Level::Trace) {
                        last_logged_pct = p;
                        dl_log!(info, "job", line.trim(), "tool" => tool, "progress" => p.round());
                    }
                } else if !line.trim().is_empty() {
                    dl_log!(info, "job", line.trim(), "tool" => tool);
                }
                let l = line.trim();
                if let Some(e) = l.strip_prefix("error:").or_else(|| l.strip_prefix("disc-launcher-helper :")) {
                    last_error = Some(e.trim().to_string());
                }
                if l.contains("drive not found in the database") {
                    unknown_drive = true;
                }
            }
            Err(mpsc::RecvTimeoutError::Timeout) => {}
            Err(mpsc::RecvTimeoutError::Disconnected) => {}
        }
        if sys::term_requested() {
            match term_sent {
                None => {
                    dl_log!(warn, "job", "annulation demandée", "job" => r.id);
                    unsafe {
                        sys::killpg(pgid, sys::SIGTERM);
                    }
                    term_sent = Some(Instant::now());
                }
                Some(t) if t.elapsed() > Duration::from_secs(10) => unsafe {
                    sys::killpg(pgid, sys::SIGKILL);
                },
                _ => {}
            }
        }
        if let Some(st) = child.try_wait().map_err(|e| e.to_string())? {
            // vider les dernières lignes
            while let Ok(line) = rx.try_recv() {
                let l = line.trim();
                if !l.is_empty() {
                    dl_log!(info, "job", l, "tool" => tool);
                }
                if let Some(e) = l.strip_prefix("error:").or_else(|| l.strip_prefix("disc-launcher-helper :")) {
                    last_error = Some(e.trim().to_string());
                }
            }
            if term_sent.is_some() || sys::term_requested() {
                return Err("annulée".into());
            }
            if !st.success() {
                let code = st.code();
                // pkexec : 126 = autorisation refusée ou fenêtre fermée, 127 = authentification impossible.
                if via_helper && last_error.is_none() && matches!(code, Some(126) | Some(127)) {
                    return Err(format!("autorisation refusée pour {real_tool} (pkexec, code {})", code.unwrap_or(0)));
                }
                let mut msg = format!("{real_tool} a échoué (code {})", code.map(|c| c.to_string()).unwrap_or_else(|| "signal".into()));
                if let Some(e) = last_error {
                    msg.push_str(&format!(" : {e}"));
                }
                if unknown_drive {
                    msg.push_str(&format!(" — lecteur inconnu de {real_tool} : voir « Lecteur reconnu par redumper » dans docs/dependances.md"));
                }
                return Err(msg);
            }
            return Ok(());
        }
    }
}

fn estimated_bytes(plan: &Value) -> u64 {
    plan["estimated_bytes"].as_i64().unwrap_or(0).max(0) as u64
}

/// Point d'entrée de `disc-launcher-job <id>`.
pub fn run(id: &str) -> i32 {
    let dir = job_dir(id);
    let lock = match OpenOptions::new().read(true).write(true).create(true).truncate(false).open(dir.join("lock")) {
        Ok(f) => f,
        Err(e) => {
            eprintln!("tâche {id} : {e}");
            return 1;
        }
    };
    if !sys::try_lock(lock.as_raw_fd(), true).unwrap_or(false) {
        eprintln!("tâche {id} déjà en cours");
        return 1;
    }
    let cfg = Config::load();
    crate::log::init(crate::log::Options { file: Some(dir.join("job.log")), max_size: 0, ..crate::log::options_from_config(&cfg, None, false) });
    sys::install_signal_flags();
    let Some(plan) = read_plan(id) else {
        dl_log!(error, "job", "plan illisible", "job" => id);
        return 1;
    };
    let mut state = read_state(id).unwrap_or_else(|| jobj! {"id" => id});
    state.set("status", "running");
    state.set("pid", sys::pid());
    state.set("started", util::now_secs());
    let mut r = Runner { id: id.to_string(), plan, state, last_write: Instant::now(), last_notify: Instant::now() - Duration::from_secs(10) };
    r.save(true);
    dl_log!(info, "job", "début de la tâche", "job" => id, "kind" => r.plan["kind"].str_or("?"), "system" => r.plan["system"].str_or("?"), "plan" => r.plan["dump_plan"].str_or("-"));
    if !r.plan["exact"].bool_or(true) {
        dl_log!(warn, "job", "plan de repli : image utilisable mais non conforme Redump (installez redumper pour un dump exact)", "job" => id, "plan" => r.plan["dump_plan"].str_or("-"));
    }

    let result = execute(&mut r, &cfg);
    let device = r.plan["device"].as_str().map(|s| s.to_string());
    if let Some(d) = &device {
        let _ = crate::device::cdrom::lock_door(d, false);
    }
    if let Some(t) = r.plan["tmp"].as_str() {
        let _ = std::fs::remove_dir_all(t);
        if let Some(parent) = Path::new(t).parent() {
            let _ = std::fs::remove_dir(parent); // `.disc-launcher-tmp` s'il est vide
        }
    }
    match result {
        Ok(res) => {
            r.state.set("status", "done");
            r.state.set("progress", 100);
            r.state.set("result", res);
            r.state.set("message", "terminé");
            dl_log!(info, "job", "tâche terminée", "job" => id);
        }
        Err(e) => {
            let cancelled = sys::term_requested();
            r.state.set("status", if cancelled { "cancelled" } else { "failed" });
            r.state.set("message", e.clone());
            dl_log!(error, "job", format!("échec : {e}"), "job" => id);
        }
    }
    r.state.set("finished", util::now_secs());
    r.save(true);
    drop(lock);
    0
}

fn execute(r: &mut Runner, cfg: &Config) -> Result<Value, String> {
    let plan = r.plan.clone();
    let kind = plan["kind"].str_or("dump").to_string();
    let tmp = PathBuf::from(plan["tmp"].as_str().ok_or("plan sans dossier temporaire")?);
    let device = plan["device"].as_str().map(|s| s.to_string());
    let mut target = Target::from_value(&plan["target"]).ok_or("plan sans cible")?;
    let mut identity = Identity::from_value(&plan["identity"]);
    let mut system = plan["system"].str_or("").to_string();
    let mut resolution = Resolution::from_value(&plan["resolution"]);
    let steps: Vec<Value> = plan["steps"].as_arr().to_vec();

    // 1. Contrôles
    for s in &steps {
        let c = s["command"].strings();
        if let Some(c0) = c.first() {
            if paths::which(c0).is_none() {
                return Err(format!("outil manquant : {c0}"));
            }
        }
    }
    std::fs::create_dir_all(&tmp).map_err(|e| format!("{} : {e}", tmp.display()))?;
    std::fs::create_dir_all(&target.system_dir).map_err(|e| format!("{} : {e}", target.system_dir.display()))?;
    let need = estimated_bytes(&plan) * 2;
    if need > 0 {
        let free = sys::free_space(&tmp).unwrap_or(u64::MAX);
        if free < need {
            return Err(format!("espace libre insuffisant : {} Mio disponibles, {} Mio nécessaires", free >> 20, need >> 20));
        }
    }

    // 2. Préparation
    let reads = steps.iter().filter(|s| s["helper"].bool_or(false) || s["name"].as_str() == Some("read")).count();
    if reads > 0 {
        if let Some(d) = &device {
            if let Err(e) = crate::device::cdrom::lock_door(d, true) {
                dl_log!(warn, "job", format!("verrouillage du tiroir impossible : {e}"));
            }
        }
    }
    let _inhibit = crate::dbus::inhibit_sleep("Dump de disque en cours");
    if _inhibit.is_none() {
        dl_log!(warn, "job", "blocage de la veille indisponible (ni logind ni PowerManagement)");
    }

    // 3-4. Étapes du gestionnaire
    let helper_mode = cfg.helper_mode();
    let total = steps.len() + 2;
    let mut reads_left = reads;
    for (i, s) in steps.iter().enumerate() {
        let name = s["name"].str_or("step").to_string();
        r.state.set("step", name.clone());
        r.state.set("step_index", i + 1);
        r.state.set("steps_total", total);
        r.set_progress(i, total, 0.0);
        r.save(true);
        let mut cmd = s["command"].strings();
        if cmd.is_empty() {
            continue;
        }
        if s["helper"].bool_or(false) || name == "read" {
            // Le lecteur doit être libre : démonter le montage automatique du bureau.
            if let Some(d) = &device {
                if let Some(mp) = crate::device::find_mount_point(d) {
                    match crate::dbus::udisks_unmount(d) {
                        Ok(()) => dl_log!(info, "job", "disque démonté avant lecture", "device" => d, "mount" => mp.display()),
                        Err(e) => dl_log!(warn, "job", format!("démontage impossible ({e}) : la lecture risque d'échouer"), "device" => d, "mount" => mp.display()),
                    }
                }
            }
        }
        if s["helper"].bool_or(false) {
            cmd = wrap_helper(&cmd, &plan, &helper_mode)?;
        }
        run_command(r, &cmd, &tmp, i, total, s["progress"].str_or("generic"))?;
        let is_read = s["helper"].bool_or(false) || name == "read";
        if is_read {
            reads_left -= 1;
            if reads_left == 0 {
                if let Some(d) = &device {
                    let _ = crate::device::cdrom::lock_door(d, false);
                    if plan["options"]["eject_after_read"].bool_or(true) && kind != "convert" && kind != "verify" {
                        let _ = crate::device::cdrom::eject(d);
                    }
                }
            }
        }
    }

    if kind == "verify" {
        return Ok(jobj! {"verified" => "ok", "path" => plan["input"].clone()});
    }

    let mut outputs: Vec<String> = plan["outputs"].strings();
    let mut verify_globs: Vec<String> = plan["verify"].strings();

    // Identification après lecture (disque illisible en lecture standard).
    if plan["late_identify"].bool_or(false) {
        let (sys_id, id2, res2, tgt2, outs, ver) = late_identify(r, cfg, &tmp)?;
        system = sys_id;
        identity = id2;
        resolution = Some(res2);
        target = tgt2;
        outputs = outs;
        verify_globs = ver;
    }

    // 5. Vérification
    r.state.set("step", "verify");
    r.save(true);
    let refdb = RefDb::load();
    let mut files: Vec<PathBuf> = verify_globs.iter().flat_map(|g| glob(&tmp, g)).collect();
    files.sort();
    files.dedup();
    let mut hashes = vec![];
    for f in &files {
        let h = util::sha1_file(f, |_| {}).map_err(|e| format!("{} : {e}", f.display()))?;
        dl_log!(info, "job", "empreinte", "file" => f.file_name().unwrap_or_default().to_string_lossy(), "sha1" => h);
        hashes.push(h);
    }
    let expected_exact = resolution.as_ref().map_or(false, |x| x.confidence == NameConfidence::Exact && x.source != "collection");
    let mut verified: Option<&str> = None;
    if let Some(g) = refdb.by_sha1(&system, &hashes) {
        verified = Some("ok");
        dl_log!(info, "job", "dump conforme à la base de référence", "name" => g.name);
        let canonical = g.name.clone();
        if cfg.rename_after_verify() && resolution.as_ref().map_or(true, |x| x.name != canonical) {
            if let Some(m) = crate::handlers::get(&system) {
                let res = naming::resolution_from_name(&canonical, NameConfidence::Exact, "redump-verify", &identity, Some(&refdb));
                let nt = naming::build_target(cfg, &m, &identity, &res);
                dl_log!(info, "job", "renommage vers le nom canonique", "from" => target.stem, "to" => nt.stem);
                resolution = Some(res);
                target = nt;
            }
        }
    } else if !hashes.is_empty() && expected_exact {
        verified = Some("mismatch");
        dl_log!(warn, "job", "le dump ne correspond pas à l'entrée attendue de la base de référence");
    }

    // 6. Finalisation par le gestionnaire (facultative) : non nécessaire pour les gestionnaires génériques.

    // 7. Placement
    r.state.set("step", "place");
    r.save(true);
    let old_stem = plan["stem"].str_or("").to_string();
    let produced: Vec<PathBuf> = outputs.iter().flat_map(|o| glob(&tmp, o)).collect();
    if produced.is_empty() {
        return Err(format!("aucun fichier produit ({})", outputs.join(", ")));
    }
    let placed = place(&produced, &target, &old_stem, kind == "redump" || kind == "convert", cfg.redump_keep_previous())?;
    if let (Some(m3u), Some(res)) = (&target.m3u, &resolution) {
        let discs_dir = target.game_dir.clone().unwrap_or_else(|| target.system_dir.clone());
        let base = m3u.parent().unwrap_or(&target.system_dir).to_path_buf();
        if let Err(e) = collection::write_m3u_for_game(m3u, &discs_dir, &base, &res.game) {
            dl_log!(warn, "job", format!("liste m3u non écrite : {e}"));
        }
    }

    // Index de collection
    let col = Collection::open()?;
    col.record(collection::Entry {
        system: system.clone(),
        key: Some(identity.key()),
        path: placed.to_string_lossy().into_owned(),
        canonical_name: resolution.as_ref().map(|x| x.name.clone()),
        format: target.ext.clone(),
        sha1: hashes.first().cloned(),
        verified: verified.map(|s| s.to_string()),
        name_source: resolution.as_ref().map(|x| x.source.clone()),
        game: resolution.as_ref().map(|x| x.game.clone()),
        disc: resolution.as_ref().and_then(|x| x.disc),
        ..Default::default()
    }, true)?;
    if let Some(old) = plan["input"].as_str() {
        if kind == "convert" && Path::new(old) != placed && plan["options"]["remove_input"].bool_or(false) {
            let _ = std::fs::remove_file(old);
        }
    }
    // Crochet on_dumped (ES-DE, métadonnées, sauvegarde…) : un échec n'annule pas la tâche.
    r.state.set("step", "on_dumped");
    r.save(true);
    run_on_dumped(cfg, &r.id, &kind, &system, &identity, resolution.as_ref(), &target, &placed, verified);
    Ok(jobj! {"path" => placed.to_string_lossy().into_owned(), "verified" => verified, "name" => resolution.map(|x| x.name), "system" => system})
}

/// Exécute `[handlers.<système>] on_dumped`, sinon `[hooks] on_dumped`, après
/// un dump, un re-dump ou une conversion réussis. Variables : DL_* habituelles
/// plus DL_DUMPED_PATH, DL_VERIFIED, DL_JOB ; gabarits {path} {system} {name}
/// {folder} {verified} {roms_dir}.
#[allow(clippy::too_many_arguments)]
fn run_on_dumped(cfg: &Config, job: &str, kind: &str, system: &str, id: &Identity, res: Option<&Resolution>, target: &Target, placed: &Path, verified: Option<&str>) {
    let per = cfg.handler(system)["on_dumped"].strings();
    let tpl = if per.is_empty() { cfg.raw.path("hooks.on_dumped").strings() } else { per };
    if tpl.is_empty() {
        return;
    }
    let path = placed.to_string_lossy().into_owned();
    let name = res.map(|x| x.name.clone()).unwrap_or_default();
    let roms = cfg.roms_dir().to_string_lossy().into_owned();
    let vars = [
        ("path", path.clone()),
        ("system", system.to_string()),
        ("name", name.clone()),
        ("folder", target.folder.clone()),
        ("verified", verified.unwrap_or("").to_string()),
        ("roms_dir", roms.clone()),
    ];
    let cmd: Vec<String> = tpl.iter().map(|a| util::render(&paths::expand(a).to_string_lossy(), &vars)).collect();
    let env = [
        ("DL_ACTION", "on_dumped".to_string()),
        ("DL_JOB", job.to_string()),
        ("DL_KIND", kind.to_string()),
        ("DL_SYSTEM", system.to_string()),
        ("DL_KEY", id.key()),
        ("DL_SERIAL", id.serial.clone().unwrap_or_default()),
        ("DL_REGION", id.region.clone().unwrap_or_default()),
        ("DL_NAME", name),
        ("DL_FOLDER", target.folder.clone()),
        ("DL_ROMS_DIR", roms),
        ("DL_DUMPED_PATH", path),
        ("DL_VERIFIED", verified.unwrap_or("").to_string()),
    ];
    dl_log!(info, "job", "crochet on_dumped", "cmd" => cmd.join(" "));
    let timeout = Duration::from_secs(cfg.raw.path("hooks.timeout").i64_or(600).clamp(1, 86400) as u64);
    let child = Command::new(&cmd[0]).args(&cmd[1..]).envs(env.iter().filter(|(_, v)| !v.is_empty()).map(|(k, v)| (*k, v.as_str()))).stdin(Stdio::null()).stdout(Stdio::piped()).stderr(Stdio::piped()).spawn();
    let mut child = match child {
        Ok(c) => c,
        Err(e) => {
            dl_log!(warn, "job", format!("on_dumped : {} : {e}", cmd[0]));
            return;
        }
    };
    let mut so = child.stdout.take().unwrap();
    let mut se = child.stderr.take().unwrap();
    let t1 = std::thread::spawn(move || {
        let mut s = String::new();
        let _ = so.read_to_string(&mut s);
        s
    });
    let t2 = std::thread::spawn(move || {
        let mut s = String::new();
        let _ = se.read_to_string(&mut s);
        s
    });
    let st = util::wait_timeout(&mut child, timeout);
    for line in t1.join().unwrap_or_default().lines().chain(t2.join().unwrap_or_default().lines()) {
        if !line.trim().is_empty() {
            dl_log!(info, "job", line.trim(), "tool" => "on_dumped");
        }
    }
    match st {
        Ok(Some(s)) if s.success() => {}
        Ok(Some(s)) => dl_log!(warn, "job", format!("on_dumped a échoué (code {})", s.code().unwrap_or(-1))),
        Ok(None) => dl_log!(warn, "job", format!("on_dumped interrompu après {} s", timeout.as_secs())),
        Err(e) => dl_log!(warn, "job", format!("on_dumped : {e}")),
    }
}

/// Range les fichiers produits à leur place définitive (renommage atomique).
fn place(produced: &[PathBuf], target: &Target, old_stem: &str, replacing: bool, keep_previous: bool) -> Result<PathBuf, String> {
    let dest_dir = target.path.parent().ok_or("cible invalide")?.to_path_buf();
    std::fs::create_dir_all(&dest_dir).map_err(|e| e.to_string())?;
    let backup = |p: &Path| -> Result<(), String> {
        if !p.exists() {
            return Ok(());
        }
        if !replacing {
            return Err(format!("{} existe déjà", p.display()));
        }
        if keep_previous {
            let bak = PathBuf::from(format!("{}.bak-{}", p.display(), crate::log::compact_stamp()));
            std::fs::rename(p, &bak).map_err(|e| e.to_string())
        } else if p.is_dir() {
            std::fs::remove_dir_all(p).map_err(|e| e.to_string())
        } else {
            std::fs::remove_file(p).map_err(|e| e.to_string())
        }
    };
    if produced.len() == 1 {
        backup(&target.path)?;
        std::fs::rename(&produced[0], &target.path).map_err(|e| format!("{} → {} : {e}", produced[0].display(), target.path.display()))?;
        return Ok(target.path.clone());
    }
    // Plusieurs fichiers (cue + bin) : renommage de la base, réécriture du .cue.
    let mut main = None;
    for p in produced {
        let name = p.file_name().unwrap().to_string_lossy().into_owned();
        let new_name = if !old_stem.is_empty() && name.starts_with(old_stem) { format!("{}{}", target.stem, &name[old_stem.len()..]) } else { name.clone() };
        let dest = dest_dir.join(&new_name);
        backup(&dest)?;
        if new_name.to_ascii_lowercase().ends_with(".cue") && old_stem != target.stem {
            let text = std::fs::read_to_string(p).map_err(|e| e.to_string())?;
            let fixed = text.replace(&format!("\"{old_stem}"), &format!("\"{}", target.stem));
            paths::write_atomic(&dest, fixed.as_bytes()).map_err(|e| e.to_string())?;
            let _ = std::fs::remove_file(p);
        } else {
            std::fs::rename(p, &dest).map_err(|e| e.to_string())?;
        }
        if dest.extension().map(|e| e.to_string_lossy().to_ascii_lowercase()) == Some(target.ext.clone()) {
            main = Some(dest.clone());
        }
    }
    Ok(main.unwrap_or_else(|| target.path.clone()))
}

/// Après lecture brute d'un disque non identifié : identifie l'image, résout
/// le nom, convertit si besoin. Renvoie (système, identité, résolution, cible,
/// sorties, fichiers à vérifier).
#[allow(clippy::type_complexity)]
fn late_identify(r: &mut Runner, cfg: &Config, tmp: &Path) -> Result<(String, Identity, Resolution, Target, Vec<String>, Vec<String>), String> {
    let image = glob(tmp, "*.cue").into_iter().chain(glob(tmp, "*.iso")).next().ok_or("aucune image produite par la lecture")?;
    let res = crate::identify::identify_image(&image, &crate::identify::Options::default()).map_err(|e| e.to_string())?;
    let m = res.primary().ok_or("image non identifiée")?.clone();
    let system = crate::identify::target_of(&m.tag).filter(|_| !crate::identify::is_media(&m.tag)).ok_or("image non identifiée comme jeu")?;
    let manifest = crate::handlers::get(&system).ok_or_else(|| format!("aucun gestionnaire pour {system}"))?;
    let refdb = RefDb::load();
    let resolution = naming::resolve(&m.identity, &res.physical, &naming::ResolveCtx { cfg, manifest: &manifest, refdb: &refdb, collection: None, use_cache: true });
    let target = naming::build_target(cfg, &manifest, &m.identity, &resolution);
    dl_log!(info, "job", "image identifiée après lecture", "system" => system, "name" => resolution.name);
    let ext = image.extension().map(|e| e.to_string_lossy().to_ascii_lowercase()).unwrap_or_default();
    let img_stem = image.file_stem().unwrap().to_string_lossy().into_owned();
    let verify = if ext == "cue" { vec![format!("{img_stem}*.bin")] } else { vec![image.file_name().unwrap().to_string_lossy().into_owned()] };
    let outputs = if ext == target.ext {
        vec![image.file_name().unwrap().to_string_lossy().into_owned()]
    } else {
        let out = tmp.join(format!("{img_stem}.{}", target.ext));
        let plan = crate::generic::convert_plan(&manifest, &image.to_string_lossy(), &out.to_string_lossy()).map_err(|e| e.message)?;
        for s in plan["steps"].as_arr() {
            let cmd = s["command"].strings();
            r.state.set("step", "convert");
            run_command(r, &cmd, tmp, 1, 3, "generic")?;
        }
        vec![out.file_name().unwrap().to_string_lossy().into_owned()]
    };
    r.plan.set("stem", img_stem);
    Ok((system, m.identity.clone(), resolution, target, outputs, verify))
}

/// Lecture des lignes d'un journal de tâche (pour `disc-launcher log --job`).
pub fn log_lines(id: &str) -> Vec<String> {
    File::open(job_dir(id).join("job.log")).map(|f| io::BufReader::new(f).lines().map_while(Result::ok).collect()).unwrap_or_default()
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn globbing() {
        let d = std::env::temp_dir().join(format!("dl-glob-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&d);
        std::fs::create_dir_all(d.join("xiso")).unwrap();
        for f in ["Game (Track 1).bin", "Game (Track 2).bin", "Game.cue", "Game.log", "xiso/Game.iso"] {
            std::fs::write(d.join(f), b"x").unwrap();
        }
        assert_eq!(glob(&d, "Game*.bin").len(), 2);
        assert_eq!(glob(&d, "Game.cue").len(), 1);
        assert_eq!(glob(&d, "xiso/Game.iso").len(), 1);
        assert_eq!(glob(&d, "*.iso").len(), 0);
        assert!(wildcard("Game (Track 1).bin", &["Game", ".bin"]));
        assert!(!wildcard("Other.bin", &["Game", ".bin"]));

        // placement multi-fichiers avec renommage de base et réécriture du cue
        let roms = d.join("roms/psx");
        std::fs::write(d.join("Game.cue"), "FILE \"Game (Track 1).bin\" BINARY\n").unwrap();
        let t = Target { system_dir: roms.clone(), folder: "psx".into(), path: roms.join("Nouveau.cue"), stem: "Nouveau".into(), ext: "cue".into(), game_dir: None, m3u: None };
        let produced = vec![d.join("Game.cue"), d.join("Game (Track 1).bin"), d.join("Game (Track 2).bin")];
        let p = place(&produced, &t, "Game", false, true).unwrap();
        assert_eq!(p, roms.join("Nouveau.cue"));
        assert!(std::fs::read_to_string(&p).unwrap().contains("\"Nouveau (Track 1).bin\""));
        assert!(roms.join("Nouveau (Track 2).bin").exists());
        let _ = std::fs::remove_dir_all(&d);
    }
}
