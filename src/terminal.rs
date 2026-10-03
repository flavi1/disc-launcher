//! Choix de l'émulateur de terminal pour afficher une tâche en cours.
//!
//! Ordre :
//! 1. `[general] terminal_command` de la configuration (liste, ex. `["konsole", "-e"]`) ;
//! 2. `xdg-terminal-exec` (proposition XDG « Default Terminal Execution ») ;
//! 3. le terminal par défaut du bureau : KDE (`kdeglobals`), Xfce (`helpers.rc`),
//!    GNOME/Cinnamon/MATE (gsettings) ;
//! 4. la variable `$TERMINAL` ;
//! 5. `x-terminal-emulator` (Debian, Ubuntu) ;
//! 6. le premier terminal connu présent, ceux du bureau courant d'abord.

use crate::config::Config;
use crate::paths;
use std::path::Path;

/// Arguments qui précèdent la commande à exécuter, selon le terminal.
/// `None` : terminal inconnu (on tente `-e`). Le booléen indique un terminal
/// qui attend la commande en une seule chaîne.
fn exec_args(name: &str) -> (Vec<&'static str>, bool) {
    match name {
        "xdg-terminal-exec" | "kitty" | "foot" | "footclient" => (vec![], false),
        "gnome-terminal" | "kgx" | "gnome-console" | "ptyxis" | "kgx-terminal" => (vec!["--"], false),
        "wezterm" => (vec!["start", "--"], false),
        "xfce4-terminal" | "mate-terminal" | "terminator" => (vec!["-x"], false),
        "lxterminal" | "tilix" | "qterminal" | "deepin-terminal" => (vec!["-e"], true),
        // konsole, xterm, alacritty, urxvt, st, cool-retro-term, x-terminal-emulator, terminology…
        _ => (vec!["-e"], false),
    }
}

fn quote(a: &str) -> String {
    if !a.is_empty() && a.chars().all(|c| c.is_ascii_alphanumeric() || "-_./=:@%+,".contains(c)) {
        a.to_string()
    } else {
        format!("'{}'", a.replace('\'', "'\\''"))
    }
}

/// Commande complète : terminal + commande `argv`.
fn wrap(term: &[String], argv: &[String]) -> Vec<String> {
    let name = Path::new(&term[0]).file_name().map(|s| s.to_string_lossy().into_owned()).unwrap_or_default();
    let mut out: Vec<String> = term.to_vec();
    if term.len() > 1 {
        // Arguments fournis par l'utilisateur ou le bureau : on ne rajoute que la commande.
        out.extend(argv.iter().cloned());
        return out;
    }
    let (args, single) = exec_args(&name);
    out.extend(args.iter().map(|s| s.to_string()));
    if single {
        out.push(argv.iter().map(|a| quote(a)).collect::<Vec<_>>().join(" "));
    } else {
        out.extend(argv.iter().cloned());
    }
    out
}

/// Valeur `clé=valeur` d'une section d'un fichier INI simple.
fn ini_value(path: &Path, section: &str, key: &str) -> Option<String> {
    let text = std::fs::read_to_string(path).ok()?;
    let mut in_sec = false;
    for l in text.lines() {
        let l = l.trim();
        if l.starts_with('[') {
            in_sec = l == format!("[{section}]");
            continue;
        }
        if in_sec {
            if let Some((k, v)) = l.split_once('=') {
                if k.trim() == key && !v.trim().is_empty() {
                    return Some(v.trim().to_string());
                }
            }
        }
    }
    None
}

fn desktops() -> Vec<String> {
    std::env::var("XDG_CURRENT_DESKTOP").unwrap_or_default().split(':').map(|s| s.to_ascii_uppercase()).filter(|s| !s.is_empty()).collect()
}

/// Terminal configuré par le bureau courant (programme, et ses arguments éventuels).
fn desktop_terminal() -> Option<Vec<String>> {
    let de = desktops();
    let has = |d: &str| de.iter().any(|x| x == d);
    if has("KDE") {
        if let Some(v) = ini_value(&paths::config_home().join("kdeglobals"), "General", "TerminalApplication") {
            let parts: Vec<String> = v.split_whitespace().map(|s| s.to_string()).collect();
            if !parts.is_empty() && paths::which(&parts[0]).is_some() {
                return Some(parts);
            }
        }
        return paths::which("konsole").map(|_| vec!["konsole".into()]);
    }
    if has("XFCE") {
        // helpers.rc n'a pas d'en-tête de section : « TerminalEmulator=xfce4-terminal ».
        let v = std::fs::read_to_string(paths::config_home().join("xfce4/helpers.rc")).ok().and_then(|t| t.lines().find_map(|l| l.strip_prefix("TerminalEmulator=").map(|s| s.trim().to_string())));
        if let Some(v) = v.filter(|v| paths::which(v).is_some()) {
            return Some(vec![v]);
        }
    }
    let schema = if has("CINNAMON") || has("X-CINNAMON") {
        Some("org.cinnamon.desktop.default-applications.terminal")
    } else if has("MATE") {
        Some("org.mate.applications-terminal")
    } else if has("GNOME") || has("UNITY") || has("BUDGIE") {
        Some("org.gnome.desktop.default-applications.terminal")
    } else {
        None
    };
    if let (Some(schema), Some(_)) = (schema, paths::which("gsettings")) {
        if let Ok(o) = std::process::Command::new("gsettings").args(["get", schema, "exec"]).output() {
            let v = String::from_utf8_lossy(&o.stdout).trim().trim_matches('\'').to_string();
            if !v.is_empty() && paths::which(&v).is_some() {
                return Some(vec![v]);
            }
        }
    }
    None
}

/// Terminal retenu (sans la commande), ou `None` si aucun n'est trouvé.
pub fn find(cfg: &Config) -> Option<Vec<String>> {
    let configured = cfg.raw.path("general.terminal_command").strings();
    if !configured.is_empty() {
        return Some(configured);
    }
    if paths::which("xdg-terminal-exec").is_some() {
        return Some(vec!["xdg-terminal-exec".into()]);
    }
    if let Some(t) = desktop_terminal() {
        return Some(t);
    }
    if let Ok(t) = std::env::var("TERMINAL") {
        let parts: Vec<String> = t.split_whitespace().map(|s| s.to_string()).collect();
        if !parts.is_empty() && paths::which(&parts[0]).is_some() {
            return Some(parts);
        }
    }
    if paths::which("x-terminal-emulator").is_some() {
        return Some(vec!["x-terminal-emulator".into()]);
    }
    let de = desktops();
    let mut known: Vec<&str> = vec![];
    if de.iter().any(|d| d == "KDE") {
        known.push("konsole");
    }
    if de.iter().any(|d| d == "GNOME") {
        known.extend(["ptyxis", "kgx", "gnome-terminal"]);
    }
    known.extend([
        "konsole", "gnome-terminal", "ptyxis", "kgx", "xfce4-terminal", "mate-terminal", "lxterminal", "qterminal", "tilix", "terminator", "alacritty", "kitty", "foot", "wezterm",
        "xterm",
    ]);
    known.into_iter().find(|t| paths::which(t).is_some()).map(|t| vec![t.to_string()])
}

/// Commande qui ouvre `argv` dans un terminal.
pub fn command(cfg: &Config, argv: &[String]) -> Option<Vec<String>> {
    find(cfg).map(|t| wrap(&t, argv))
}

#[cfg(test)]
mod tests {
    use super::*;
    fn v(s: &[&str]) -> Vec<String> {
        s.iter().map(|x| x.to_string()).collect()
    }
    #[test]
    fn wrapping() {
        let cmd = v(&["/usr/bin/disc-launcher", "watch", "J1"]);
        assert_eq!(wrap(&v(&["konsole"]), &cmd), v(&["konsole", "-e", "/usr/bin/disc-launcher", "watch", "J1"]));
        assert_eq!(wrap(&v(&["gnome-terminal"]), &cmd), v(&["gnome-terminal", "--", "/usr/bin/disc-launcher", "watch", "J1"]));
        assert_eq!(wrap(&v(&["xdg-terminal-exec"]), &cmd), v(&["xdg-terminal-exec", "/usr/bin/disc-launcher", "watch", "J1"]));
        assert_eq!(wrap(&v(&["lxterminal"]), &cmd), v(&["lxterminal", "-e", "/usr/bin/disc-launcher watch J1"]));
        assert_eq!(wrap(&v(&["konsole", "--hold", "-e"]), &cmd), v(&["konsole", "--hold", "-e", "/usr/bin/disc-launcher", "watch", "J1"]));
        assert_eq!(quote("a b'c"), "'a b'\\''c'");
    }
}
