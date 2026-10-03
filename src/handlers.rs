//! Manifestes des gestionnaires (`handlers/<id>.toml`), résolution de
//! l'exécutable et contrat d'exécution : `<exécutable> <verbe> [options]`,
//! variables d'environnement `DL_*`, JSON facultatif sur stdin, JSON sur stdout.

use crate::config::Config;
use crate::identify::IdentResult;
use crate::json::{self, Value};
use crate::naming::{Resolution, Target};
use crate::{jobj, paths, util};
use std::collections::BTreeMap;
use std::io::Write;
use std::process::{Command, Stdio};
use std::time::Duration;

/// Codes de retour du contrat.
pub const RC_OK: i32 = 0;
pub const RC_NOT_APPLICABLE: i32 = 2;
pub const RC_MISSING: i32 = 3;
pub const RC_USER_ERROR: i32 = 4;

#[derive(Clone, Debug)]
pub struct Manifest {
    pub id: String,
    pub raw: Value,
}

impl Manifest {
    pub fn name(&self) -> &str {
        self.raw.path("handler.name").str_or(&self.id)
    }
    pub fn kind(&self) -> &str {
        self.raw.path("handler.kind").str_or("console")
    }
    /// Gestionnaire de lecture seule (média, ou disque de données simple).
    pub fn is_media(&self) -> bool {
        matches!(self.kind(), "media" | "data")
    }
    pub fn is_data(&self) -> bool {
        self.kind() == "data"
    }
    pub fn tags(&self) -> Vec<String> {
        self.raw.path("handler.tags").strings()
    }
    pub fn target_format(&self) -> String {
        self.raw.path("formats.target").str_or("iso").to_string()
    }
    pub fn accepted_formats(&self) -> Vec<String> {
        let mut v = self.raw.path("formats.accepted").strings();
        if v.is_empty() {
            v.push(self.target_format());
        }
        v
    }
    pub fn multidisc(&self) -> bool {
        self.raw.path("formats.multidisc").bool_or(false)
    }
    /// La cible est un dossier (PS3, Wii U) plutôt qu'un fichier.
    pub fn target_is_dir(&self) -> bool {
        self.raw.path("formats.target_is_dir").bool_or(false)
    }
    /// Dossier ES-DE selon la région principale.
    pub fn folder_for(&self, region: Option<&str>, regional: bool) -> String {
        let rf = self.raw.path("handler.regional_folders");
        if regional {
            if let Some(r) = region {
                let main = crate::identity::main_region(r);
                if let Some(f) = rf.get(main).as_str() {
                    return f.to_string();
                }
            }
        }
        rf.get("default").as_str().unwrap_or(&self.id).to_string()
    }
    /// Tous les dossiers possibles (pour le scan de collection).
    pub fn all_folders(&self) -> Vec<String> {
        let mut v = vec![self.id.clone()];
        if let Some(m) = self.raw.path("handler.regional_folders").as_obj() {
            for f in m.values().filter_map(|x| x.as_str()) {
                if !v.iter().any(|y| y == f) {
                    v.push(f.to_string());
                }
            }
        }
        v
    }
    pub fn requires_profile(&self) -> Vec<String> {
        self.raw.path("dump.requires_profile").strings()
    }
    pub fn resolver_chain(&self) -> Vec<String> {
        self.raw.path("resolvers.chain").strings()
    }
    pub fn required_commands(&self) -> Vec<String> {
        self.raw.path("requirements.commands").strings()
    }
    pub fn dumpable(&self) -> bool {
        !self.raw.path("dump.plan").as_arr().is_empty()
    }
    /// Programmes (premier mot de chaque étape) d'un plan de dump.
    pub fn plan_tools(plan: &Value) -> Vec<String> {
        let mut v: Vec<String> = plan["step"].as_arr().iter().filter_map(|s| s["command"].strings().into_iter().next()).collect();
        v.dedup();
        v
    }
    /// Nom d'affichage d'un plan : `name`, sinon son premier outil.
    pub fn plan_name(plan: &Value) -> String {
        plan["name"].as_str().map(|s| s.to_string()).or_else(|| Self::plan_tools(plan).into_iter().next()).unwrap_or_else(|| "plan".into())
    }
    /// État des plans de dump : pour chaque support (cd, dvd…), le premier plan
    /// dont tous les outils sont installés, sinon les outils manquants du plan
    /// préféré (le premier déclaré). Les plans réservés à un profil de lecteur
    /// absent (`profiles`) sont écartés si `profile` est donné.
    pub fn dump_status(&self, profile: Option<&str>) -> DumpStatus {
        let mut st = DumpStatus::default();
        let plans = self.raw.path("dump.plan").as_arr();
        let mut medias: Vec<String> = vec![];
        for p in plans.iter() {
            for m in p["media"].strings() {
                if !medias.contains(&m) {
                    medias.push(m);
                }
            }
        }
        if medias.is_empty() && !plans.is_empty() {
            medias.push("*".into());
        }
        for media in medias {
            let cands: Vec<&Value> = plans
                .iter()
                .filter(|p| {
                    let ms = p["media"].strings();
                    let ps = p["profiles"].strings();
                    (media == "*" || ms.is_empty() || ms.contains(&media)) && (profile.is_none() || ps.is_empty() || ps.iter().any(|x| Some(x.as_str()) == profile))
                })
                .collect();
            let Some(first) = cands.first() else { continue };
            match cands.iter().enumerate().find(|(_, p)| Self::plan_tools(p).iter().all(|t| crate::paths::which(t).is_some())) {
                Some((i, p)) => st.available.push((media.clone(), Self::plan_name(p), i > 0)),
                None => {
                    for t in Self::plan_tools(first) {
                        if crate::paths::which(&t).is_none() && !st.missing.contains(&t) {
                            st.missing.push(t);
                        }
                    }
                }
            }
        }
        st
    }
}

/// Résultat de [`Manifest::dump_status`].
#[derive(Debug, Default, Clone)]
pub struct DumpStatus {
    /// (support, plan retenu, repli ?) pour chaque support dumpable.
    pub available: Vec<(String, String, bool)>,
    /// Outils manquants des plans préférés des supports non dumpables.
    pub missing: Vec<String>,
}

impl DumpStatus {
    pub fn any(&self) -> bool {
        !self.available.is_empty()
    }
    /// Plans de repli utilisés, ex. « cd : cdrdao ».
    pub fn fallbacks(&self) -> Vec<String> {
        self.available.iter().filter(|a| a.2).map(|a| if a.0 == "*" { a.1.clone() } else { format!("{} : {}", a.0, a.1) }).collect()
    }
}

/// Charge tous les manifestes : intégrés, puis système, puis utilisateur
/// (fusion clé par clé pour un même identifiant).
pub fn load_all() -> BTreeMap<String, Manifest> {
    let mut out: BTreeMap<String, Value> = BTreeMap::new();
    for (name, src) in BUILTIN {
        if let Ok(v) = crate::toml::parse(src) {
            let id = v.path("handler.id").as_str().map(|s| s.to_string()).unwrap_or_else(|| name.trim_end_matches(".toml").to_string());
            out.insert(id, v);
        }
    }
    for dir in paths::data_layers("handlers") {
        // Manifestes à la racine (consoles) et dans `media/`.
        let mut files: Vec<_> = [dir.clone(), dir.join("media"), dir.join("data")]
            .iter()
            .filter_map(|d| std::fs::read_dir(d).ok())
            .flat_map(|it| it.flatten().map(|e| e.path()))
            .filter(|p| p.extension().is_some_and(|e| e == "toml"))
            .collect();
        files.sort();
        for f in files {
            match std::fs::read_to_string(&f).map_err(|e| e.to_string()).and_then(|t| crate::toml::parse(&t).map_err(|e| e.to_string())) {
                Ok(v) => {
                    let id = v.path("handler.id").as_str().map(|s| s.to_string()).unwrap_or_else(|| f.file_stem().unwrap().to_string_lossy().into_owned());
                    match out.get_mut(&id) {
                        Some(base) => json::merge(base, &v),
                        None => {
                            out.insert(id, v);
                        }
                    }
                }
                Err(e) => crate::dl_log!(warn, "handlers", format!("manifeste ignoré : {e}"), "file" => f.display()),
            }
        }
    }
    out.into_iter().map(|(id, raw)| (id.clone(), Manifest { id, raw })).collect()
}

pub fn get(id: &str) -> Option<Manifest> {
    load_all().remove(id)
}

/// Gestionnaire d'une étiquette (`console:psx` → psx, `video:dvd` → dvd-video).
pub fn for_tag<'a>(all: &'a BTreeMap<String, Manifest>, tag: &str) -> Option<&'a Manifest> {
    all.values().find(|m| m.tags().iter().any(|t| t == tag || (t.ends_with(":*") && tag.starts_with(&t[..t.len() - 1])))).or_else(|| crate::identify::target_of(tag).and_then(|t| all.get(&t)))
}

/// Gestionnaires génériques fournis.
pub const GENERIC_CONSOLE: &str = "disc-launcher-generic";
pub const GENERIC_MEDIA: &str = "disc-launcher-media-generic";
pub const GENERIC_DATA: &str = "disc-launcher-data-generic";

/// Exécutable choisi dans une valeur de configuration : chaîne (tous les
/// verbes) ou table `{ play = "…", "*" = "…" }`.
fn pick(v: &Value, verb: &str) -> Option<String> {
    match v {
        Value::Str(s) if !s.is_empty() => Some(s.clone()),
        Value::Obj(_) => v.get(verb).as_str().or_else(|| v.get("*").as_str()).map(|s| s.to_string()),
        _ => None,
    }
}

/// Exécutable qui traite `verb` pour ce gestionnaire, dans l'ordre :
/// 1. `[handlers.<id>] executable` de la configuration ;
/// 2. `[handler] executable` du manifeste ;
/// 3. un exécutable spécifique dans le PATH : `disc-launcher-<id>`
///    (console) ou `disc-launcher-media-<id>` (média) ;
/// 4. `[handlers.defaults] console | media` de la configuration ;
/// 5. le gestionnaire générique fourni.
pub fn executable_for(m: &Manifest, cfg: &Config, verb: &str) -> String {
    if let Some(e) = pick(&cfg.handler(&m.id)["executable"], verb) {
        return paths::expand(&e).to_string_lossy().into_owned();
    }
    if let Some(e) = pick(m.raw.path("handler.executable"), verb) {
        return e;
    }
    let kind = if m.is_data() { "data" } else if m.is_media() { "media" } else { "console" };
    let specific = if kind == "console" { format!("disc-launcher-{}", m.id) } else { format!("disc-launcher-{kind}-{}", m.id) };
    if paths::which(&specific).is_some() {
        return specific;
    }
    if let Some(e) = pick(&cfg.raw.path("handlers.defaults").get(kind).clone(), verb) {
        return paths::expand(&e).to_string_lossy().into_owned();
    }
    match kind {
        "data" => GENERIC_DATA,
        "media" => GENERIC_MEDIA,
        _ => GENERIC_CONSOLE,
    }
    .to_string()
}

pub fn is_generic(exe: &str) -> bool {
    let n = std::path::Path::new(exe).file_name().map(|s| s.to_string_lossy().into_owned()).unwrap_or_default();
    n == GENERIC_CONSOLE || n == GENERIC_MEDIA || n == GENERIC_DATA
}

/// Contexte d'un appel de gestionnaire, transmis en variables `DL_*`.
#[derive(Default)]
pub struct CallCtx<'a> {
    pub action: &'a str,
    pub device: Option<&'a str>,
    pub ident: Option<&'a IdentResult>,
    pub tag: Option<&'a str>,
    pub resolution: Option<&'a Resolution>,
    pub target: Option<&'a Target>,
    pub existing: Option<&'a str>,
    pub checked: bool,
}

/// Variables d'environnement `DL_*` (toutes facultatives pour le gestionnaire).
pub fn env_for(m: &Manifest, cfg: &Config, c: &CallCtx) -> Vec<(String, String)> {
    fn put(v: &mut Vec<(String, String)>, k: &str, val: Option<String>) {
        if let Some(x) = val.filter(|x| !x.is_empty()) {
            v.push((format!("DL_{k}"), x));
        }
    }
    let mut v: Vec<(String, String)> = vec![];
    put(&mut v, "ACTION", Some(c.action.to_string()));
    put(&mut v, "HANDLER", Some(m.id.clone()));
    put(&mut v, "KIND", Some(m.kind().to_string()));
    put(&mut v, "SYSTEM", Some(m.id.clone()));
    if m.is_media() {
        put(&mut v, "MEDIA", Some(m.id.clone()));
    }
    put(&mut v, "ROMS_DIR", Some(cfg.roms_dir().to_string_lossy().into_owned()));
    if let Some(d) = c.device {
        put(&mut v, "DEVICE", Some(d.to_string()));
        put(&mut v, "DEVNAME", Some(d.trim_start_matches("/dev/").to_string()));
        put(&mut v, "MOUNT", crate::device::find_mount_point(d).map(|p| p.to_string_lossy().into_owned()));
    }
    if let Some(i) = c.ident {
        let p = &i.physical;
        put(&mut v, "MEDIA_TYPE", Some(p.media.name().to_string()));
        put(&mut v, "TRACKS", Some(p.tracks.len().to_string()));
        put(&mut v, "AUDIO_TRACKS", Some(p.audio_tracks().to_string()));
        put(&mut v, "DATA_TRACKS", Some(p.data_tracks().to_string()));
        put(&mut v, "SESSIONS", Some(p.sessions.to_string()));
        put(&mut v, "FINGERPRINT", Some(i.fingerprint.clone()));
        put(&mut v, "DRIVE_PROFILE", Some(i.profile.clone()));
        put(&mut v, "LABEL", i.volume_id.clone());
        if let Some(pm) = i.primary() {
            put(&mut v, "TAG", Some(pm.tag.clone()));
            put(&mut v, "CONFIDENCE", Some(pm.confidence.name().to_string()));
            let id = &pm.identity;
            put(&mut v, "SERIAL", id.serial.clone());
            put(&mut v, "GAME_ID", id.game_id.clone());
            put(&mut v, "REGION", id.region.clone());
            put(&mut v, "DISC", id.disc.map(|d| d.to_string()));
            put(&mut v, "DISCS", id.discs_total.map(|d| d.to_string()));
            put(&mut v, "TITLE", id.title.clone());
            put(&mut v, "KEY", Some(id.key()));
        }
    }
    if let Some(t) = c.tag {
        if !v.iter().any(|(k, _)| k == "DL_TAG") {
            put(&mut v, "TAG", Some(t.to_string()));
        }
    }
    if let Some(r) = c.resolution {
        put(&mut v, "NAME", Some(r.name.clone()));
        put(&mut v, "GAME", Some(r.game.clone()));
        put(&mut v, "NAME_CONFIDENCE", Some(r.confidence.name().to_string()));
        if !v.iter().any(|(k, _)| k == "DL_TITLE") {
            put(&mut v, "TITLE", Some(r.game.clone()));
        }
    }
    if let Some(t) = c.target {
        put(&mut v, "TARGET", Some(t.path.to_string_lossy().into_owned()));
        put(&mut v, "FOLDER", Some(t.folder.clone()));
    }
    if let Some(e) = c.existing {
        put(&mut v, "EXISTING", Some(e.to_string()));
        put(&mut v, "EXT", std::path::Path::new(e).extension().map(|x| x.to_string_lossy().to_ascii_lowercase()));
    }
    if c.checked {
        put(&mut v, "CHECKED", Some("1".into()));
    }
    // Lecture surchargée : le générique doit la considérer comme disponible.
    let play = executable_for(m, cfg, "play");
    if !is_generic(&play) {
        put(&mut v, "PLAY_HANDLER", Some(play));
    }
    // Identification complète en JSON.
    let info = jobj! {
        "handler" => m.id.clone(), "action" => c.action, "device" => c.device,
        "identification" => c.ident.map(|i| i.to_value()),
        "resolution" => c.resolution.map(|r| r.to_value()),
        "target" => c.target.map(|t| t.to_value()),
        "existing" => c.existing,
    };
    let name = c.device.map(|d| d.trim_start_matches("/dev/").to_string()).unwrap_or_else(|| m.id.clone());
    let path = paths::runtime_dir().join(format!("info-{name}.json"));
    if paths::write_atomic(&path, info.to_pretty().as_bytes()).is_ok() {
        put(&mut v, "INFO", Some(path.to_string_lossy().into_owned()));
    }
    v
}

/// Variables de gabarit (`{device}`, `{existing}`, `{system}`…) tirées des
/// variables `DL_*` de l'environnement courant.
pub fn template_vars_from_env() -> Vec<(String, String)> {
    std::env::vars().filter_map(|(k, v)| k.strip_prefix("DL_").map(|n| (n.to_ascii_lowercase(), v))).collect()
}

#[derive(Debug)]
pub struct CallResult {
    pub code: i32,
    pub out: Value,
    pub stderr: String,
}

/// Appelle `exe verbe args…` avec les variables `env` et `input` en JSON sur stdin.
pub fn call(exe: &str, verb: &str, args: &[String], input: &Value, env: &[(String, String)], timeout: Duration) -> Result<CallResult, String> {
    let path = paths::which(exe).ok_or_else(|| format!("exécutable introuvable : {exe}"))?;
    let mut child = Command::new(&path)
        .arg(verb)
        .args(args)
        .envs(env.iter().map(|(k, v)| (k.as_str(), v.as_str())))
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .map_err(|e| format!("{exe} : {e}"))?;
    if let Some(mut si) = child.stdin.take() {
        let _ = si.write_all(input.to_json().as_bytes());
    }
    // Lecture des sorties dans des fils pour éviter les blocages de tube.
    let mut so = child.stdout.take().unwrap();
    let mut se = child.stderr.take().unwrap();
    let t1 = std::thread::spawn(move || {
        let mut s = String::new();
        let _ = std::io::Read::read_to_string(&mut so, &mut s);
        s
    });
    let t2 = std::thread::spawn(move || {
        let mut s = String::new();
        let _ = std::io::Read::read_to_string(&mut se, &mut s);
        s
    });
    let status = util::wait_timeout(&mut child, timeout).map_err(|e| e.to_string())?;
    let stdout = t1.join().unwrap_or_default();
    let stderr = t2.join().unwrap_or_default();
    let Some(st) = status else { return Err(format!("{exe} {verb} : délai dépassé ({} s)", timeout.as_secs())) };
    let code = st.code().unwrap_or(-1);
    let out = if stdout.trim().is_empty() { Value::Null } else { json::parse(stdout.trim()).unwrap_or_else(|_| jobj! {"raw" => stdout.trim()}) };
    Ok(CallResult { code, out, stderr })
}

/// Appel d'un verbe d'un gestionnaire, exécutable résolu et variables `DL_*` fournies.
pub fn invoke(m: &Manifest, cfg: &Config, verb: &str, args: &[String], input: &Value, ctx: &CallCtx, timeout: Duration) -> Result<CallResult, String> {
    let exe = executable_for(m, cfg, verb);
    let mut env = env_for(m, cfg, ctx);
    let mut args = args.to_vec();
    if is_generic(&exe) {
        args.insert(0, m.id.clone());
        args.insert(0, "--id".into());
    }
    env.push(("DL_VERB".into(), verb.into()));
    call(&exe, verb, &args, input, &env, timeout)
}

/// Manifestes intégrés au binaire (utilisés si rien n'est installé).
pub const BUILTIN: &[(&str, &str)] = &[
    // consoles
    ("psx.toml", include_str!("../data/handlers/psx.toml")),
    ("ps2.toml", include_str!("../data/handlers/ps2.toml")),
    ("ps3.toml", include_str!("../data/handlers/ps3.toml")),
    ("gc.toml", include_str!("../data/handlers/gc.toml")),
    ("wii.toml", include_str!("../data/handlers/wii.toml")),
    ("wiiu.toml", include_str!("../data/handlers/wiiu.toml")),
    ("xbox.toml", include_str!("../data/handlers/xbox.toml")),
    ("xbox360.toml", include_str!("../data/handlers/xbox360.toml")),
    ("dreamcast.toml", include_str!("../data/handlers/dreamcast.toml")),
    ("saturn.toml", include_str!("../data/handlers/saturn.toml")),
    ("megacd.toml", include_str!("../data/handlers/megacd.toml")),
    ("pcenginecd.toml", include_str!("../data/handlers/pcenginecd.toml")),
    ("pcfx.toml", include_str!("../data/handlers/pcfx.toml")),
    ("neogeocd.toml", include_str!("../data/handlers/neogeocd.toml")),
    ("3do.toml", include_str!("../data/handlers/3do.toml")),
    ("cdimono1.toml", include_str!("../data/handlers/cdimono1.toml")),
    ("amigacd32.toml", include_str!("../data/handlers/amigacd32.toml")),
    ("cdtv.toml", include_str!("../data/handlers/cdtv.toml")),
    ("atarijaguarcd.toml", include_str!("../data/handlers/atarijaguarcd.toml")),
    ("fmtowns.toml", include_str!("../data/handlers/fmtowns.toml")),
    ("pc98.toml", include_str!("../data/handlers/pc98.toml")),
    ("naomigd.toml", include_str!("../data/handlers/naomigd.toml")),
    ("windows.toml", include_str!("../data/handlers/windows.toml")),
    // cartouches (Retrode)
    ("n64.toml", include_str!("../data/handlers/n64.toml")),
    ("snes.toml", include_str!("../data/handlers/snes.toml")),
    ("megadrive.toml", include_str!("../data/handlers/megadrive.toml")),
    ("gb.toml", include_str!("../data/handlers/gb.toml")),
    ("gbc.toml", include_str!("../data/handlers/gbc.toml")),
    ("gba.toml", include_str!("../data/handlers/gba.toml")),
    ("mastersystem.toml", include_str!("../data/handlers/mastersystem.toml")),
    ("gamegear.toml", include_str!("../data/handlers/gamegear.toml")),
    // médias
    ("cdda.toml", include_str!("../data/handlers/media/cdda.toml")),
    ("dvd-video.toml", include_str!("../data/handlers/media/dvd-video.toml")),
    ("bluray-video.toml", include_str!("../data/handlers/media/bluray-video.toml")),
    ("dvd-audio.toml", include_str!("../data/handlers/media/dvd-audio.toml")),
    ("vcd.toml", include_str!("../data/handlers/media/vcd.toml")),
    ("svcd.toml", include_str!("../data/handlers/media/svcd.toml")),
    ("hddvd-video.toml", include_str!("../data/handlers/media/hddvd-video.toml")),
    ("avchd.toml", include_str!("../data/handlers/media/avchd.toml")),
    ("dvd-vr.toml", include_str!("../data/handlers/media/dvd-vr.toml")),
    ("bdav.toml", include_str!("../data/handlers/media/bdav.toml")),
    ("photo-cd.toml", include_str!("../data/handlers/media/photo-cd.toml")),
    ("dcim.toml", include_str!("../data/handlers/media/dcim.toml")),
    // disques de données simples
    ("data-audio.toml", include_str!("../data/handlers/data/data-audio.toml")),
    ("data-video.toml", include_str!("../data/handlers/data/data-video.toml")),
];
