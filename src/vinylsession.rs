//! Encodage d'un vinyle sans terminal : disc-launcher pilote
//! `vinylrip --json session` (commandes JSON sur l'entrée, événements sur la
//! sortie) et l'affiche comme les autres éléments de la fenêtre, avec ses
//! boutons : démarrer la face, l'arrêter, écouter, face suivante, terminer,
//! choisir l'édition. Après un échec, l'explication de vinylrip (ce qui est
//! conservé, comment reprendre ou terminer à la main) s'affiche en entier.

use crate::json::{self, Value};
use crate::jobj;
use crate::notify::t;
use std::io::{BufRead, BufReader, Write};
use std::process::{Child, ChildStdin, Command, Stdio};

pub const PREFIX: &str = "vs:";

#[derive(Clone, Debug, PartialEq)]
pub enum Phase {
    Starting,
    Ready,
    Recording(String),
    Processing(String),
    Choosing(Vec<Value>),
    Done(Value),
    Failed { message: String, help: String },
    Cancelled,
}

/// État de la session, mis à jour par les événements de vinylrip.
#[derive(Clone, Debug)]
pub struct State {
    pub phase: Phase,
    pub faces: Vec<String>,
    pub listening: bool,
    pub listen_url: Option<String>,
    /// Écoute servie en flux HTTP (Kodi) plutôt que par aplay.
    pub http: bool,
    pub job: Option<String>,
    pub warnings: Vec<String>,
}

const LETTERS: &str = "ABCDEFGH";

impl State {
    pub fn new(http: bool) -> State {
        State { phase: Phase::Starting, faces: vec![], listening: false, listen_url: None, http, job: None, warnings: vec![] }
    }

    pub fn next_face(&self) -> String {
        LETTERS.chars().nth(self.faces.len()).map(|c| c.to_string()).unwrap_or_else(|| "?".into())
    }

    /// Applique un événement ; renvoie vrai si la session est terminée.
    pub fn apply(&mut self, ev: &Value) -> bool {
        match ev["event"].str_or("") {
            "ready" => {
                self.job = ev["job"].string();
                self.phase = Phase::Ready;
            }
            "recording" => self.phase = Phase::Recording(ev["face"].str_or("?").to_string()),
            "face_done" => {
                self.faces.push(ev["face"].str_or("?").to_string());
                self.listening = false;
                self.listen_url = None;
                if ev["overruns"].i64_or(0) > 0 {
                    self.warnings.push(format!("{} {} : {}", t("vinyl-face"), ev["face"].str_or("?"), t("vinyl-overruns")));
                }
                self.phase = Phase::Ready;
            }
            "listening" => {
                self.listening = ev["on"].bool_or(false);
                self.listen_url = if self.listening { ev["url"].string() } else { None };
            }
            "stage" => self.phase = Phase::Processing(stage_text(ev)),
            "choose_release" => self.phase = Phase::Choosing(ev["candidates"].as_arr().to_vec()),
            "done" => {
                self.phase = Phase::Done(ev.clone());
                return true;
            }
            "cancelled" => {
                self.phase = Phase::Cancelled;
                return true;
            }
            "error" => {
                let message = ev["message"].str_or("").to_string();
                match ev["help"].as_str() {
                    // Échec d'une étape : explication complète.
                    Some(h) => {
                        self.phase = Phase::Failed { message, help: h.to_string() };
                        return true;
                    }
                    // Commande refusée : la session continue.
                    None => self.warnings.push(message),
                }
            }
            _ => {}
        }
        false
    }

    /// Actions proposées (clé, libellé).
    pub fn actions(&self) -> Vec<(String, String)> {
        let a = |k: &str, l: String| (format!("{PREFIX}{k}"), l);
        match &self.phase {
            Phase::Starting | Phase::Processing(_) | Phase::Cancelled => vec![],
            Phase::Ready => {
                let mut v = vec![a("start", format!("{} {}", t("vinyl-start-face"), self.next_face()))];
                if !self.faces.is_empty() {
                    v.push(a("finish", t("vinyl-finish").into()));
                }
                v.push(a("cancel", t("cancel").into()));
                v
            }
            Phase::Recording(face) => vec![
                a("stop", format!("{} {face}", t("vinyl-stop-face"))),
                if self.listening { a("unlisten", t("stop-listen").into()) } else { a("listen", t("listen").into()) },
                a("cancel", t("cancel").into()),
            ],
            Phase::Choosing(c) => {
                let mut v: Vec<(String, String)> = c.iter().enumerate().map(|(i, r)| a(&format!("choose:{i}"), release_label(r))).collect();
                v.push(a("choose:none", t("vinyl-no-release").into()));
                v
            }
            Phase::Done(_) | Phase::Failed { .. } => {
                let mut v = vec![];
                if self.details().is_some() {
                    v.push(a("details", t("vinyl-details").into()));
                }
                v.push(a("close", t("close").into()));
                v
            }
        }
    }

    pub fn lines(&self) -> Vec<String> {
        let mut out = vec![match &self.phase {
            Phase::Starting => t("vinyl-starting").to_string(),
            Phase::Ready if self.faces.is_empty() => t("vinyl-place-face").to_string(),
            Phase::Ready => format!("{} {} — {}", t("vinyl-faces-done"), self.faces.join(", "), t("vinyl-next-or-finish")),
            Phase::Recording(f) => format!("{} {f}{}", t("vinyl-recording"), if self.listening { format!(" — {}", t("vinyl-listening")) } else { String::new() }),
            Phase::Processing(s) => s.clone(),
            Phase::Choosing(_) => t("vinyl-choose").to_string(),
            Phase::Done(r) => format!("{} : {}", t("vinyl-done"), r["album_dir"].str_or("")),
            Phase::Failed { message, .. } => format!("{} : {message}", t("job-failed")),
            Phase::Cancelled => t("job-cancelled").to_string(),
        }];
        out.extend(self.warnings.iter().map(|w| format!("⚠ {w}")));
        out
    }

    /// Explication longue (échec, ou faces à vérifier après le traitement).
    pub fn details(&self) -> Option<String> {
        match &self.phase {
            Phase::Failed { help, .. } => Some(help.clone()),
            Phase::Done(r) => {
                let mut parts: Vec<String> = r["warnings"].as_arr().iter().filter_map(|w| w.as_str()).map(|w| format!("• {w}")).collect();
                if let Some(h) = r["help"].as_str().filter(|h| !h.is_empty()) {
                    parts.insert(0, h.to_string());
                }
                (!parts.is_empty()).then(|| parts.join("\n"))
            }
            _ => None,
        }
    }

    /// Commande JSON pour une action ; None si l'action n'en envoie pas.
    pub fn command(&self, key: &str) -> Option<Value> {
        let k = key.strip_prefix(PREFIX)?;
        Some(match k {
            "start" => jobj! {"cmd" => "start_face"},
            "stop" => jobj! {"cmd" => "stop_face"},
            "listen" => jobj! {"cmd" => "listen", "on" => true, "http" => self.http},
            "unlisten" => jobj! {"cmd" => "listen", "on" => false},
            "finish" => jobj! {"cmd" => "finish"},
            "cancel" => jobj! {"cmd" => "cancel", "delete" => false},
            "choose:none" => jobj! {"cmd" => "choose", "index" => Value::Null},
            c if c.starts_with("choose:") => jobj! {"cmd" => "choose", "index" => c["choose:".len()..].parse::<i64>().ok()?},
            _ => return None,
        })
    }
}

fn release_label(r: &Value) -> String {
    let year = r["year"].as_i64().map(|y| format!(" ({y})")).unwrap_or_default();
    format!("{} — {}{year}, {} {}", r["artist"].str_or("?"), r["album"].str_or("?"), r["tracks"].i64_or(0), t("vinyl-tracks"))
}

fn stage_text(ev: &Value) -> String {
    let n = |k: &str| ev[k].as_i64().map(|x| x.to_string()).unwrap_or_default();
    match ev["stage"].str_or("") {
        "split" => format!("{} {}", t("vinyl-st-split"), ev["face"].str_or("")),
        "identify" => t("vinyl-st-identify").into(),
        "release" => t("vinyl-st-release").into(),
        "encode" => format!("{} {}/{}", t("vinyl-st-encode"), n("track"), n("of")),
        "convert" => format!("{} {}/{}", t("vinyl-st-convert"), n("track"), n("of")),
        "filed" => t("vinyl-st-filed").into(),
        s => s.to_string(),
    }
}

/// Processus `vinylrip --json session`.
pub struct Session {
    pub dev: String,
    pub state: State,
    child: Child,
    stdin: Option<ChildStdin>,
}

impl Session {
    /// Lance la session ; chaque événement est remis à `on_event`.
    pub fn start(dev: &str, device: &str, http: bool, on_event: impl Fn(Value) + Send + 'static) -> std::io::Result<Session> {
        let log = crate::paths::log_dir().join("vinylrip-session.log");
        let err = std::fs::OpenOptions::new().create(true).append(true).open(log).map(Stdio::from).unwrap_or_else(|_| Stdio::null());
        let mut child = Command::new("vinylrip").args(["--json", "session", "--device", device]).stdin(Stdio::piped()).stdout(Stdio::piped()).stderr(err).spawn()?;
        let out = child.stdout.take().ok_or_else(|| std::io::Error::other("stdout"))?;
        let stdin = child.stdin.take();
        std::thread::spawn(move || {
            for line in BufReader::new(out).lines().map_while(Result::ok) {
                if let Ok(v) = json::parse(line.trim()) {
                    on_event(v);
                }
            }
            on_event(jobj! {"event" => "exited"});
        });
        Ok(Session { dev: dev.to_string(), state: State::new(http), child, stdin })
    }

    pub fn send(&mut self, cmd: &Value) -> std::io::Result<()> {
        let Some(w) = self.stdin.as_mut() else { return Err(std::io::Error::other("session fermée")) };
        let mut line = cmd.to_json();
        line.push('\n');
        w.write_all(line.as_bytes())?;
        w.flush()
    }

    /// Fermeture : vinylrip garde toujours ce qui était en cours d'enregistrement.
    pub fn close(&mut self) {
        drop(self.stdin.take());
        for _ in 0..50 {
            if self.child.try_wait().ok().flatten().is_some() {
                return;
            }
            std::thread::sleep(std::time::Duration::from_millis(100));
        }
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}

impl Drop for Session {
    fn drop(&mut self) {
        if self.child.try_wait().ok().flatten().is_none() {
            self.close();
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn ev(s: &str) -> Value {
        json::parse(s).unwrap()
    }

    fn keys(st: &State) -> Vec<String> {
        st.actions().into_iter().map(|(k, _)| k).collect()
    }

    #[test]
    fn full_flow() {
        let mut st = State::new(true);
        assert!(st.actions().is_empty());
        st.apply(&ev(r#"{"event":"ready","job":"/j"}"#));
        assert_eq!(keys(&st), ["vs:start", "vs:cancel"]);
        assert!(st.actions()[0].1.ends_with(" A"));
        st.apply(&ev(r#"{"event":"recording","face":"A"}"#));
        assert_eq!(keys(&st), ["vs:stop", "vs:listen", "vs:cancel"]);
        assert_eq!(st.command("vs:listen").unwrap()["http"].as_bool(), Some(true));
        st.apply(&ev(r#"{"event":"listening","on":true,"url":"http://127.0.0.1:1/vinyl.wav"}"#));
        assert_eq!(st.listen_url.as_deref(), Some("http://127.0.0.1:1/vinyl.wav"));
        assert_eq!(keys(&st)[1], "vs:unlisten");
        st.apply(&ev(r#"{"event":"face_done","face":"A","overruns":2}"#));
        assert_eq!(keys(&st), ["vs:start", "vs:finish", "vs:cancel"]);
        assert!(st.actions()[0].1.ends_with(" B"));
        assert!(st.lines().iter().any(|l| l.starts_with('⚠')));
        st.apply(&ev(r#"{"event":"stage","stage":"encode","track":3,"of":9}"#));
        assert!(st.actions().is_empty() && st.lines()[0].ends_with("3/9"));
        st.apply(&ev(r#"{"event":"choose_release","candidates":[{"artist":"A","album":"B","year":1979,"tracks":9}]}"#));
        assert_eq!(keys(&st), ["vs:choose:0", "vs:choose:none"]);
        assert_eq!(st.command("vs:choose:0").unwrap()["index"].as_i64(), Some(0));
        assert!(st.command("vs:choose:none").unwrap()["index"].is_null());
        assert!(st.apply(&ev(r#"{"event":"done","album_dir":"/m/A/B","warnings":["face B: à vérifier"],"help":"Écoutez les coupures."}"#)));
        assert_eq!(keys(&st), ["vs:details", "vs:close"]);
        assert!(st.details().unwrap().starts_with("Écoutez"));
    }

    #[test]
    fn failure_keeps_the_explanation() {
        let mut st = State::new(false);
        st.apply(&ev(r#"{"event":"ready","job":"/j"}"#));
        // Commande refusée : simple avertissement.
        assert!(!st.apply(&ev(r#"{"event":"error","message":"nothing was recorded","stage":null}"#)));
        assert_eq!(st.phase, Phase::Ready);
        assert!(st.apply(&ev(r#"{"event":"error","message":"flac","stage":"encode","help":"Reprendre :\n  vinylrip process /j"}"#)));
        assert_eq!(st.details().unwrap(), "Reprendre :\n  vinylrip process /j");
        assert_eq!(keys(&st), ["vs:details", "vs:close"]);
    }
}
