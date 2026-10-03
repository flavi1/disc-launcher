//! Lecteur Kodi pour disc-launcher-media-generic : un lecteur parmi d'autres,
//! déclaré dans `[media.players.kodi]` (command = ["disc-launcher-player-kodi"]).
//! Le cœur de disc-launcher ne dépend pas de Kodi : ce programme est facultatif.
//!
//! Périphérique : argument, sinon `DL_DEVICE`.
//!
//! - Kodi tourne : on déclenche l'addon d'import (`RunScript(script.disc.import)`)
//!   par JSON-RPC (`Addons.ExecuteAddon`, TCP 9090 puis HTTP), sinon par
//!   l'EventServer (UDP 9777, action intégrée `RunScript(...)`).
//! - Kodi ne tourne pas : on le démarre. Par défaut (`startup = "addon-detects"`)
//!   l'addon détecte lui-même le disque au démarrage ; avec `startup = "rpc"`,
//!   on attend que JSON-RPC réponde puis on appelle l'addon.

use disclauncher::config::Config;
use disclauncher::json::{self, Value};
use disclauncher::{jobj, paths, util};
use std::io::{Read, Write};
use std::net::{TcpStream, ToSocketAddrs, UdpSocket};
use std::time::{Duration, Instant};

#[derive(Clone, Debug)]
pub struct KodiCfg {
    pub command: Vec<String>,
    pub host: String,
    pub tcp_port: u16,
    pub http_port: u16,
    pub http_user: String,
    pub http_password: String,
    pub eventserver_port: u16,
    pub addon_id: String,
    pub addon_params: Vec<String>,
    /// `addon-detects` | `rpc` | `none`
    pub startup: String,
    pub startup_timeout: u64,
    pub process_names: Vec<String>,
}

impl KodiCfg {
    pub fn from_config(cfg: &Config) -> KodiCfg {
        let h = cfg.raw.path("media.players.kodi");
        let strs = |k: &str, d: &[&str]| {
            let v = h[k].strings();
            if v.is_empty() && h[k].is_null() {
                d.iter().map(|s| s.to_string()).collect()
            } else {
                v
            }
        };
        KodiCfg {
            command: strs("kodi_command", &["kodi"]),
            host: h["host"].str_or("127.0.0.1").to_string(),
            tcp_port: h["tcp_port"].i64_or(9090) as u16,
            http_port: h["http_port"].i64_or(8080) as u16,
            http_user: h["http_user"].str_or("kodi").to_string(),
            http_password: h["http_password"].str_or("").to_string(),
            eventserver_port: h["eventserver_port"].i64_or(9777) as u16,
            addon_id: h["addon_id"].str_or("script.disc.import").to_string(),
            addon_params: strs("addon_params", &[]),
            startup: h["startup"].str_or("addon-detects").to_string(),
            startup_timeout: h["startup_timeout"].i64_or(60).clamp(5, 600) as u64,
            process_names: strs("process_names", &["kodi.bin", "kodi-x11", "kodi-wayland", "kodi-gbm", "kodi"]),
        }
    }
}

fn request_body(method: &str, params: &Value) -> String {
    jobj! {"jsonrpc" => "2.0", "id" => 1, "method" => method, "params" => params.clone()}.to_json()
}

/// Découpe un flux en objets JSON complets (Kodi TCP n'a pas de délimiteur).
fn split_objects(buf: &str) -> (Vec<String>, usize) {
    let b = buf.as_bytes();
    let mut out = vec![];
    let (mut depth, mut in_str, mut esc, mut start, mut consumed) = (0i32, false, false, None, 0);
    for (i, &c) in b.iter().enumerate() {
        if in_str {
            if esc {
                esc = false;
            } else if c == b'\\' {
                esc = true;
            } else if c == b'"' {
                in_str = false;
            }
            continue;
        }
        match c {
            b'"' => in_str = true,
            b'{' => {
                if depth == 0 {
                    start = Some(i);
                }
                depth += 1;
            }
            b'}' => {
                depth -= 1;
                if depth == 0 {
                    if let Some(s) = start.take() {
                        out.push(buf[s..=i].to_string());
                        consumed = i + 1;
                    }
                }
            }
            _ => {}
        }
    }
    (out, consumed)
}

fn connect(host: &str, port: u16, timeout: Duration) -> std::io::Result<TcpStream> {
    let addr = (host, port).to_socket_addrs()?.next().ok_or_else(|| std::io::Error::new(std::io::ErrorKind::NotFound, "adresse"))?;
    let s = TcpStream::connect_timeout(&addr, timeout)?;
    s.set_read_timeout(Some(timeout))?;
    s.set_write_timeout(Some(timeout))?;
    Ok(s)
}

/// JSON-RPC sur le port TCP brut (pas d'authentification, local).
pub fn rpc_tcp(k: &KodiCfg, method: &str, params: &Value) -> Result<Value, String> {
    let mut s = connect(&k.host, k.tcp_port, Duration::from_secs(3)).map_err(|e| e.to_string())?;
    s.write_all(request_body(method, params).as_bytes()).map_err(|e| e.to_string())?;
    let end = Instant::now() + Duration::from_secs(5);
    let mut buf = String::new();
    let mut chunk = [0u8; 8192];
    while Instant::now() < end {
        let n = s.read(&mut chunk).map_err(|e| e.to_string())?;
        if n == 0 {
            break;
        }
        buf.push_str(&String::from_utf8_lossy(&chunk[..n]));
        let (objs, used) = split_objects(&buf);
        for o in objs {
            if let Ok(v) = json::parse(&o) {
                if v["id"].as_i64() == Some(1) {
                    return reply(v);
                }
            }
        }
        buf.drain(..used);
    }
    Err("pas de réponse JSON-RPC".into())
}

fn reply(v: Value) -> Result<Value, String> {
    if !v["error"].is_null() {
        return Err(format!("erreur JSON-RPC : {}", v["error"]["message"].str_or("?")));
    }
    Ok(v["result"].clone())
}

/// JSON-RPC sur HTTP (réglage « Autoriser le contrôle à distance via HTTP »).
pub fn rpc_http(k: &KodiCfg, method: &str, params: &Value) -> Result<Value, String> {
    let mut s = connect(&k.host, k.http_port, Duration::from_secs(3)).map_err(|e| e.to_string())?;
    let body = request_body(method, params);
    let mut req = format!("POST /jsonrpc HTTP/1.0\r\nHost: {}:{}\r\nContent-Type: application/json\r\nContent-Length: {}\r\n", k.host, k.http_port, body.len());
    if !k.http_password.is_empty() {
        req.push_str(&format!("Authorization: Basic {}\r\n", util::base64(format!("{}:{}", k.http_user, k.http_password).as_bytes())));
    }
    req.push_str("Connection: close\r\n\r\n");
    req.push_str(&body);
    s.write_all(req.as_bytes()).map_err(|e| e.to_string())?;
    let mut resp = vec![];
    let _ = s.read_to_end(&mut resp);
    let text = String::from_utf8_lossy(&resp);
    let (head, payload) = text.split_once("\r\n\r\n").ok_or("réponse HTTP invalide")?;
    let status = head.split_whitespace().nth(1).unwrap_or("");
    if status != "200" {
        return Err(format!("HTTP {status}"));
    }
    reply(json::parse(payload.trim()).map_err(|e| e.to_string())?)
}

pub fn rpc(k: &KodiCfg, method: &str, params: &Value) -> Result<Value, String> {
    match rpc_tcp(k, method, params) {
        Ok(v) => Ok(v),
        Err(e1) => rpc_http(k, method, params).map_err(|e2| format!("TCP : {e1} ; HTTP : {e2}")),
    }
}

pub fn ping(k: &KodiCfg) -> bool {
    rpc(k, "JSONRPC.Ping", &Value::obj()).is_ok()
}

/// Kodi tourne-t-il (processus) ?
pub fn process_running(k: &KodiCfg) -> bool {
    let Ok(it) = std::fs::read_dir("/proc") else { return false };
    let uid = disclauncher::sys::uid();
    for e in it.flatten() {
        let name = e.file_name();
        let Some(pid) = name.to_str().filter(|s| s.chars().all(|c| c.is_ascii_digit())) else { continue };
        let Ok(comm) = std::fs::read_to_string(format!("/proc/{pid}/comm")) else { continue };
        let comm = comm.trim();
        if k.process_names.iter().any(|n| n.starts_with(comm) || comm == n) {
            use std::os::unix::fs::MetadataExt;
            if std::fs::metadata(format!("/proc/{pid}")).map(|m| m.uid() == uid).unwrap_or(false) {
                return true;
            }
        }
    }
    false
}

/// Paquet EventServer « ACTION » (exécution d'une commande intégrée).
pub fn eventserver_packet(builtin: &str) -> Vec<u8> {
    let mut payload = vec![0x01u8]; // ACTION_EXECBUILTIN
    payload.extend_from_slice(builtin.as_bytes());
    payload.push(0);
    let mut p = Vec::with_capacity(32 + payload.len());
    p.extend_from_slice(b"XBMC");
    p.push(2); // version majeure
    p.push(0); // version mineure
    p.extend_from_slice(&0x000Au16.to_be_bytes()); // PT_ACTION
    p.extend_from_slice(&1u32.to_be_bytes()); // séquence
    p.extend_from_slice(&1u32.to_be_bytes()); // nombre de paquets
    p.extend_from_slice(&(payload.len() as u16).to_be_bytes());
    p.extend_from_slice(&(disclauncher::util::now_secs() as u32).to_be_bytes()); // identifiant client
    p.extend_from_slice(&[0u8; 10]);
    p.extend_from_slice(&payload);
    p
}

pub fn eventserver_action(k: &KodiCfg, builtin: &str) -> Result<(), String> {
    let s = UdpSocket::bind("0.0.0.0:0").map_err(|e| e.to_string())?;
    s.send_to(&eventserver_packet(builtin), (k.host.as_str(), k.eventserver_port)).map_err(|e| e.to_string())?;
    Ok(())
}

fn render_params(k: &KodiCfg, device: &str) -> Vec<String> {
    k.addon_params.iter().map(|p| util::render(p, &[("device", device.to_string())])).collect()
}

/// Commande intégrée équivalente : `RunScript(script.disc.import,arg1,arg2)`.
pub fn builtin(k: &KodiCfg, device: &str) -> String {
    let params = render_params(k, device);
    if params.is_empty() {
        format!("RunScript({})", k.addon_id)
    } else {
        format!("RunScript({},{})", k.addon_id, params.join(","))
    }
}

/// Déclenche l'addon dans un Kodi déjà lancé.
pub fn trigger_addon(k: &KodiCfg, device: &str) -> Result<String, String> {
    let params: Vec<Value> = render_params(k, device).into_iter().map(Value::from).collect();
    let p = jobj! {"addonid" => k.addon_id.clone(), "params" => params, "wait" => false};
    match rpc(k, "Addons.ExecuteAddon", &p) {
        Ok(_) => Ok("json-rpc".into()),
        Err(e) => {
            disclauncher::dl_log!(warn, "kodi", format!("JSON-RPC indisponible ({e}), essai par l'EventServer"));
            eventserver_action(k, &builtin(k, device)).map(|_| "eventserver".into())
        }
    }
}

/// Démarre Kodi, détaché.
pub fn launch(k: &KodiCfg, device: &str) -> Result<u32, String> {
    let cmd: Vec<String> = k.command.iter().map(|a| util::render(a, &[("device", device.to_string())])).collect();
    if cmd.is_empty() || paths::which(&cmd[0]).is_none() {
        return Err(format!("Kodi introuvable : {}", cmd.first().cloned().unwrap_or_default()));
    }
    disclauncher::generic::spawn_detached(&cmd, &paths::log_dir().join("kodi.log"))
}

/// Ouvre le disque dans Kodi. Renvoie une description de ce qui a été fait.
pub fn open_disc(k: &KodiCfg, device: &str) -> Result<String, String> {
    if ping(k) {
        return trigger_addon(k, device).map(|via| format!("addon {} déclenché ({via})", k.addon_id));
    }
    if process_running(k) {
        // Kodi tourne mais JSON-RPC est fermé : EventServer.
        return eventserver_action(k, &builtin(k, device)).map(|_| format!("addon {} déclenché (eventserver)", k.addon_id));
    }
    let pid = launch(k, device)?;
    match k.startup.as_str() {
        "rpc" => {
            let end = Instant::now() + Duration::from_secs(k.startup_timeout);
            while Instant::now() < end {
                std::thread::sleep(Duration::from_secs(1));
                if ping(k) {
                    std::thread::sleep(Duration::from_secs(2));
                    return trigger_addon(k, device).map(|via| format!("Kodi démarré (pid {pid}), addon déclenché ({via})"));
                }
            }
            Err(format!("Kodi démarré (pid {pid}) mais JSON-RPC n'a pas répondu en {} s", k.startup_timeout))
        }
        _ => Ok(format!("Kodi démarré (pid {pid}) ; l'addon {} détecte le disque", k.addon_id)),
    }
}

/// Ouvre un fichier, une liste de lecture ou un dossier (photos : diaporama)
/// dans Kodi par JSON-RPC `Player.Open`, en démarrant Kodi au besoin.
pub fn open_path(k: &KodiCfg, path: &str) -> Result<String, String> {
    let item = if std::path::Path::new(path).is_dir() { jobj! {"directory" => path} } else { jobj! {"file" => path} };
    let p = jobj! {"item" => item};
    if !ping(k) {
        if process_running(k) {
            let b = if std::path::Path::new(path).is_dir() { format!("SlideShow({path})") } else { format!("PlayMedia({path})") };
            return eventserver_action(k, &b).map(|_| format!("{path} ouvert (eventserver)"));
        }
        let pid = launch(k, "")?;
        let end = Instant::now() + Duration::from_secs(k.startup_timeout);
        loop {
            if Instant::now() > end {
                return Err(format!("Kodi démarré (pid {pid}) mais JSON-RPC n'a pas répondu en {} s (activez le contrôle à distance)", k.startup_timeout));
            }
            std::thread::sleep(Duration::from_secs(1));
            if ping(k) {
                std::thread::sleep(Duration::from_secs(2));
                break;
            }
        }
    }
    rpc(k, "Player.Open", &p).map(|_| format!("{path} ouvert (json-rpc)"))
}

fn main() {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let cfg = Config::load();
    disclauncher::log::init(disclauncher::log::options_from_config(&cfg, Some(&paths::log_dir().join("player-kodi.log")), false));
    let k = KodiCfg::from_config(&cfg);
    if let Some(i) = args.iter().position(|a| a == "--open") {
        let Some(path) = args.get(i + 1) else {
            eprintln!("disc-launcher-player-kodi : --open <fichier|dossier>");
            std::process::exit(2);
        };
        match open_path(&k, path) {
            Ok(msg) => {
                disclauncher::dl_log!(info, "kodi", msg.clone());
                println!("{}", jobj! {"message" => msg}.to_json());
                return;
            }
            Err(e) => {
                disclauncher::dl_log!(error, "kodi", e.clone());
                eprintln!("disc-launcher-player-kodi : {e}");
                std::process::exit(1);
            }
        }
    }
    let dev = args.iter().find(|a| !a.starts_with('-')).cloned().or_else(|| std::env::var("DL_DEVICE").ok()).unwrap_or_else(|| "/dev/sr0".into());
    match open_disc(&k, &dev) {
        Ok(msg) => {
            disclauncher::dl_log!(info, "kodi", msg.clone(), "drive" => dev);
            println!("{}", jobj! {"message" => msg}.to_json());
        }
        Err(e) => {
            disclauncher::dl_log!(error, "kodi", e.clone(), "drive" => dev);
            eprintln!("disc-launcher-player-kodi : {e}");
            std::process::exit(1);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn packet_and_split() {
        let p = eventserver_packet("RunScript(script.disc.import)");
        assert_eq!(&p[..4], b"XBMC");
        assert_eq!(p.len(), 32 + 1 + "RunScript(script.disc.import)".len() + 1);
        assert_eq!(u16::from_be_bytes([p[16], p[17]]) as usize, p.len() - 32);
        let (o, used) = split_objects(r#"{"a":"}"}{"id":1,"result":"pong"}{"x""#);
        assert_eq!(o.len(), 2);
        assert_eq!(used, r#"{"a":"}"}{"id":1,"result":"pong"}"#.len());
        let cfg = Config::from_value(disclauncher::toml::parse("[media.players.kodi]\naddon_params = [\"device={device}\"]\n").unwrap());
        let k = KodiCfg::from_config(&cfg);
        assert_eq!(builtin(&k, "/dev/sr0"), "RunScript(script.disc.import,device=/dev/sr0)");
        assert_eq!(k.tcp_port, 9090);
    }
}
