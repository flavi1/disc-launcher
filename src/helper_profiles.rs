//! Profils d'invocation de l'assistant privilégié (`disc-launcher-helper`).
//!
//! L'assistant tourne avec CAP_SYS_RAWIO : il ne doit pas exécuter une ligne
//! de commande arbitraire. La tâche de dump ne lui transmet donc qu'un **nom
//! de profil** et des valeurs typées ; l'assistant construit lui-même la
//! commande à partir du profil :
//!
//! ```toml
//! [profiles.redumper-disc]
//! path = ["/usr/local/bin/redumper", "/usr/bin/redumper"]
//! command = ["disc", "--drive={sgdevice}", "--image-path={out}", "--image-name={name}"]
//! # Options supplémentaires acceptées (réglages par lecteur) : exactes, ou
//! # préfixe terminé par « = » suivi d'une valeur simple.
//! options = ["--speed=", "--drive-pregap-start="]
//! ```
//!
//! Gabarits : `{device}` (/dev/srN), `{sgdevice}` (/dev/sgN du même lecteur),
//! `{out}` (dossier de sortie de l'utilisateur), `{name}` (nom de fichier
//! contrôlé). Les profils intégrés ci-dessous peuvent être complétés ou
//! remplacés, profil par profil, dans `/etc/disc-launcher/helper-tools.toml`
//! (fichier appartenant à root).

use crate::json::Value;

pub const BUILTIN: &str = r#"
[profiles.redumper-disc]
path = ["/usr/local/bin/redumper", "/usr/bin/redumper"]
command = ["disc", "--drive={sgdevice}", "--image-path={out}", "--image-name={name}"]
options = [
  "--speed=", "--retries=", "--verbose", "--auto-eject", "--disable-cdtext",
  "--drive-type=", "--drive-read-offset=", "--drive-c2-shift=", "--drive-pregap-start=",
  "--drive-read-method=", "--drive-sector-order=", "--plextor-skip-leadin",
  "--asus-skip-leadout", "--correct-offset-shift",
]

[profiles.friidump]
path = ["/usr/local/bin/friidump", "/usr/bin/friidump"]
command = ["-d", "{device}", "-i", "{out}/{name}.iso"]
options = ["-a", "-s", "-r"]
"#;

/// Profils intégrés, complétés par `extra` (contenu de helper-tools.toml).
pub fn load(extra: Option<&str>) -> Result<Value, String> {
    let mut v = crate::toml::parse(BUILTIN).map_err(|e| e.to_string())?;
    if let Some(t) = extra {
        let e = crate::toml::parse(t).map_err(|e| format!("helper-tools.toml : {e}"))?;
        if let Some(obj) = e.get("profiles").as_obj() {
            for (k, p) in obj {
                // Un profil redéfini remplace entièrement celui d'origine.
                if let Some(all) = v.get_mut("profiles") {
                    all.set(k, p.clone());
                }
            }
        }
    }
    Ok(v)
}

/// Chemins candidats du programme d'un profil.
pub fn paths(p: &Value) -> Vec<String> {
    match p["path"].as_str() {
        Some(s) => vec![s.to_string()],
        None => p["path"].strings(),
    }
}

/// Nom de fichier acceptable pour `{name}` : pas de chemin, pas d'option.
pub fn valid_name(n: &str) -> bool {
    !n.is_empty() && n.len() <= 200 && !n.starts_with('-') && !n.starts_with('.') && !n.contains('/') && !n.chars().any(|c| c.is_control())
}

fn valid_value(v: &str) -> bool {
    !v.is_empty() && v.len() <= 32 && v.chars().all(|c| c.is_ascii_alphanumeric() || "_.+-".contains(c))
}

/// Option supplémentaire autorisée par le profil ?
pub fn option_allowed(p: &Value, opt: &str) -> bool {
    p["options"].strings().iter().any(|o| match o.strip_suffix('=') {
        Some(prefix) => opt.strip_prefix(&format!("{prefix}=")).is_some_and(valid_value),
        None => opt == o,
    })
}

/// Arguments du programme : gabarit du profil rendu, puis options vérifiées.
pub fn build(p: &Value, device: &str, sgdevice: &str, out: &str, name: &str, options: &[String]) -> Result<Vec<String>, String> {
    if !valid_name(name) {
        return Err(format!("nom de fichier refusé : {name:?}"));
    }
    let tpl = p["command"].strings();
    if tpl.is_empty() {
        return Err("profil sans commande".into());
    }
    let vars = [("device", device.to_string()), ("sgdevice", sgdevice.to_string()), ("out", out.to_string()), ("name", name.to_string())];
    let mut args: Vec<String> = tpl.iter().map(|a| crate::util::render(a, &vars)).collect();
    for o in options {
        if !option_allowed(p, o) {
            return Err(format!("option non autorisée par le profil : {o}"));
        }
        args.push(o.clone());
    }
    Ok(args)
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn redumper_profile() {
        let v = load(Some("[profiles.friidump]\npath = \"/opt/friidump\"\ncommand = [\"-d\", \"{device}\"]\n")).unwrap();
        let p = &v["profiles"]["redumper-disc"];
        let a = build(p, "/dev/sr0", "/dev/sg1", "/home/u/tmp", "Jeu (Europe)", &["--drive-pregap-start=-135".into()]).unwrap();
        assert_eq!(a, vec!["disc", "--drive=/dev/sg1", "--image-path=/home/u/tmp", "--image-name=Jeu (Europe)", "--drive-pregap-start=-135"]);
        assert!(build(p, "/dev/sr0", "/dev/sg1", "/t", "Jeu", &["--image-path=/etc".into()]).is_err());
        assert!(build(p, "/dev/sr0", "/dev/sg1", "/t", "Jeu", &["--speed=8;rm".into()]).is_err());
        assert!(build(p, "/dev/sr0", "/dev/sg1", "/t", "../x", &[]).is_err());
        assert!(build(p, "/dev/sr0", "/dev/sg1", "/t", "-x", &[]).is_err());
        assert_eq!(paths(&v["profiles"]["friidump"]), vec!["/opt/friidump"]);
    }
}
