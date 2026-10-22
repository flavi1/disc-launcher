//! Tâches externes en arrière-plan : commande lancée sans terminal (encbd,
//! action personnalisée sous Kodi), suivie dans la fenêtre avec sa durée, une
//! progression lue dans son journal quand elle en affiche une, et « Annuler ».
//! La commande tourne dans son propre groupe de processus : l'annulation
//! arrête aussi ses enfants (makemkvcon, ffmpeg…).

use crate::json::Value;
use crate::jobj;
use std::io::{Read, Seek, SeekFrom};
use std::os::unix::process::CommandExt;
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::time::Instant;

pub const PREFIX: &str = "task:";

pub struct Task {
    pub id: u32,
    pub title: String,
    /// Périphérique d'origine (`/dev/sr0`…), vide si aucun.
    pub dev: String,
    pub log: PathBuf,
    pub started: Instant,
    /// Données propres à l'appelant (nom prévu, dossier, nature…).
    pub meta: Value,
    child: Child,
    pub cancelled: bool,
}

impl Task {
    pub fn spawn(id: u32, title: &str, dev: &str, argv: &[String], log: &Path, meta: Value) -> std::io::Result<Task> {
        if let Some(d) = log.parent() {
            let _ = std::fs::create_dir_all(d);
        }
        let out = std::fs::File::create(log)?;
        let err = out.try_clone()?;
        let prog = crate::paths::which(&argv[0]).unwrap_or_else(|| PathBuf::from(&argv[0]));
        let child = Command::new(prog).args(&argv[1..]).stdin(Stdio::null()).stdout(out).stderr(err).process_group(0).spawn()?;
        Ok(Task { id, title: title.to_string(), dev: dev.to_string(), log: log.to_path_buf(), started: Instant::now(), meta, child, cancelled: false })
    }

    /// Code de sortie si la commande est terminée (-signal si tuée par un signal).
    pub fn poll(&mut self) -> Option<i32> {
        use std::os::unix::process::ExitStatusExt;
        self.child.try_wait().ok().flatten().map(|s| s.code().unwrap_or_else(|| -s.signal().unwrap_or(0)))
    }

    pub fn cancel(&mut self) {
        self.cancelled = true;
        let pg = self.child.id() as i32;
        unsafe {
            crate::sys::killpg(pg, crate::sys::SIGTERM);
        }
        for _ in 0..30 {
            if self.poll().is_some() {
                return;
            }
            std::thread::sleep(std::time::Duration::from_millis(100));
        }
        unsafe {
            crate::sys::killpg(pg, crate::sys::SIGKILL);
        }
    }

    pub fn tail(&self, bytes: u64) -> String {
        tail(&self.log, bytes)
    }

    /// Élément de la fenêtre.
    pub fn item(&self, cancel_label: &str) -> Value {
        let secs = self.started.elapsed().as_secs();
        let elapsed = if secs >= 3600 { format!("{} h {:02} min", secs / 3600, secs / 60 % 60) } else { format!("{} min {:02} s", secs / 60, secs % 60) };
        let last = self.tail(4096);
        let pct = progress(&last);
        let mut lines = vec![Value::from(elapsed)];
        if let Some(l) = last_line(&last) {
            lines.push(l.into());
        }
        jobj! {
            "id" => format!("{PREFIX}{}", self.id), "icon" => self.meta["icon"].str_or(crate::tray::ICON),
            "title" => self.title.clone(), "lines" => lines,
            "progress" => match pct { Some(p) => jobj! {"fraction" => p / 100.0, "text" => format!("{p:.0} %")}, None => Value::Null },
            "actions" => vec![Value::from(vec![Value::from("cancel"), Value::from(cancel_label)])],
        }
    }
}

pub fn tail(p: &Path, bytes: u64) -> String {
    let Ok(mut f) = std::fs::File::open(p) else { return String::new() };
    let len = f.metadata().map(|m| m.len()).unwrap_or(0);
    let _ = f.seek(SeekFrom::Start(len.saturating_sub(bytes)));
    let mut buf = vec![];
    let _ = f.read_to_end(&mut buf);
    String::from_utf8_lossy(&buf).into_owned()
}

/// Dernier pourcentage affiché par la commande (« 42.5% », « 42 % »).
pub fn progress(text: &str) -> Option<f64> {
    let t = text.replace('\r', "\n");
    let b = t.as_bytes();
    let mut i = b.len();
    while i > 0 {
        i -= 1;
        if b[i] != b'%' {
            continue;
        }
        let mut j = i;
        while j > 0 && b[j - 1] == b' ' {
            j -= 1;
        }
        let end = j;
        while j > 0 && (b[j - 1].is_ascii_digit() || b[j - 1] == b'.' || b[j - 1] == b',') {
            j -= 1;
        }
        if j < end {
            if let Ok(v) = t[j..end].replace(',', ".").parse::<f64>() {
                if (0.0..=100.0).contains(&v) {
                    return Some(v);
                }
            }
        }
    }
    None
}

/// Dernière ligne lisible du journal (sans les séquences de terminal).
pub fn last_line(text: &str) -> Option<String> {
    let t = text.replace('\r', "\n");
    t.lines().rev().map(strip_ansi).map(|l| l.trim().to_string()).find(|l| !l.is_empty()).map(|l| if l.chars().count() > 90 { l.chars().take(90).collect::<String>() + "…" } else { l })
}

fn strip_ansi(s: &str) -> String {
    let mut out = String::new();
    let mut it = s.chars().peekable();
    while let Some(c) = it.next() {
        if c == '\x1b' {
            if it.peek() == Some(&'[') {
                it.next();
                for d in it.by_ref() {
                    if d.is_ascii_alphabetic() {
                        break;
                    }
                }
            }
            continue;
        }
        out.push(c);
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn progress_and_lines() {
        assert_eq!(progress("Current progress - 12%\rCurrent progress - 42.5%  \n"), Some(42.5));
        assert_eq!(progress("frame=  1 fps  Encodage 7 %"), Some(7.0));
        assert_eq!(progress("aucun"), None);
        assert_eq!(progress("200%"), None);
        assert_eq!(last_line("a\n\x1b[1mÉtape 2\x1b[0m\n\n").as_deref(), Some("Étape 2"));
    }

    #[test]
    fn run_and_cancel() {
        let dir = std::env::temp_dir().join(format!("dl-task-{}", std::process::id()));
        let mut t = Task::spawn(1, "test", "", &["sh".into(), "-c".into(), "echo 'Rip 30 %'; sleep 30".into()], &dir.join("t.log"), Value::Null).unwrap();
        std::thread::sleep(std::time::Duration::from_millis(300));
        assert_eq!(t.poll(), None);
        assert_eq!(t.item("Annuler")["progress"]["text"].str_or(""), "30 %");
        t.cancel();
        assert!(t.poll().is_some());
        let mut ok = Task::spawn(2, "ok", "", &["true".into()], &dir.join("o.log"), Value::Null).unwrap();
        std::thread::sleep(std::time::Duration::from_millis(300));
        assert_eq!(ok.poll(), Some(0));
        let _ = std::fs::remove_dir_all(dir);
    }
}
