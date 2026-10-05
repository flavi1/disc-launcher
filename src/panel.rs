//! Côté démon de la fenêtre « Disques et périphériques »
//! (`disc-launcher-panel`) : lancement, envoi de l'état, lecture des clics.
//! Le protocole est décrit dans `src/bin/disc-launcher-panel.rs`.

use crate::config::Config;
use crate::json::Value;
use crate::paths;
use std::io::{BufRead, BufReader, Write};
use std::process::{Child, ChildStdin, Command, Stdio};
use std::sync::mpsc::Sender;

#[derive(Clone, Debug, PartialEq)]
pub enum PanelEvent {
    Ready,
    /// Fenêtre fermée par l'utilisateur.
    Hidden,
    Action(String, String),
    /// Le programme s'est arrêté (sans GTK ni affichage : code 3).
    Exited(Option<i32>),
}

/// Ligne de sortie du panneau → événement.
pub fn parse_line(l: &str) -> Option<PanelEvent> {
    let l = l.trim_end();
    match l {
        "ready" => Some(PanelEvent::Ready),
        "hidden" => Some(PanelEvent::Hidden),
        _ => {
            let mut it = l.splitn(3, '\t');
            match (it.next(), it.next(), it.next()) {
                (Some("action"), Some(id), Some(key)) => Some(PanelEvent::Action(id.to_string(), key.to_string())),
                _ => None,
            }
        }
    }
}

/// Commande du panneau : `[ui] panel_command`, sinon `disc-launcher-panel`.
/// None si `[ui] panel = "notifications"`.
pub fn command(cfg: &Config) -> Option<Vec<String>> {
    if cfg.raw.path("ui.panel").str_or("auto") == "notifications" {
        return None;
    }
    let custom = cfg.raw.path("ui.panel_command").strings();
    if !custom.is_empty() {
        return Some(custom);
    }
    paths::which("disc-launcher-panel").map(|p| vec![p.to_string_lossy().into_owned()])
}

pub struct Panel {
    child: Child,
    stdin: ChildStdin,
    last: String,
}

impl Panel {
    pub fn start(cmd: &[String], events: Sender<PanelEvent>) -> std::io::Result<Panel> {
        let mut child = Command::new(&cmd[0]).args(&cmd[1..]).stdin(Stdio::piped()).stdout(Stdio::piped()).stderr(Stdio::inherit()).spawn()?;
        let stdin = child.stdin.take().ok_or_else(|| std::io::Error::other("stdin"))?;
        let stdout = child.stdout.take().ok_or_else(|| std::io::Error::other("stdout"))?;
        let pid = child.id();
        std::thread::Builder::new()
            .name("panel".into())
            .spawn(move || {
                for line in BufReader::new(stdout).lines().map_while(Result::ok) {
                    if let Some(e) = parse_line(&line) {
                        if events.send(e).is_err() {
                            return;
                        }
                    }
                }
                // Fin de la sortie : le programme s'est arrêté (code lu par le démon).
                let _ = events.send(PanelEvent::Exited(None));
                crate::dl_log!(debug, "panel", "sortie du panneau fermée", "pid" => pid);
            })?;
        Ok(Panel { child, stdin, last: String::new() })
    }

    /// Envoie l'état s'il a changé.
    pub fn send(&mut self, state: &Value) -> std::io::Result<()> {
        let line = state.to_json();
        if line == self.last {
            return Ok(());
        }
        self.stdin.write_all(line.as_bytes())?;
        self.stdin.write_all(b"\n")?;
        self.stdin.flush()?;
        self.last = line;
        Ok(())
    }

    /// Code de sortie, si le programme s'est arrêté.
    pub fn exit_code(&mut self) -> Option<Option<i32>> {
        self.child.try_wait().ok().flatten().map(|s| s.code())
    }
}

impl Drop for Panel {
    fn drop(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn lines() {
        assert_eq!(parse_line("ready"), Some(PanelEvent::Ready));
        assert_eq!(parse_line("action\t/dev/sr0\teject\n"), Some(PanelEvent::Action("/dev/sr0".into(), "eject".into())));
        assert_eq!(parse_line("action\tjob:1"), None);
        assert_eq!(parse_line("bruit"), None);
    }
}
