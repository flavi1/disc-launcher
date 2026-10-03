//! Socket de contrôle du démon : `$XDG_RUNTIME_DIR/disc-launcher/control.sock`,
//! protocole JSON ligne par ligne (une requête, une réponse).

use crate::json::{self, Value};
use crate::paths;
use std::io::{BufRead, BufReader, Write};
use std::os::unix::net::UnixStream;
use std::time::Duration;

/// Envoie une requête et attend la réponse.
pub fn request(req: &Value, timeout: Duration) -> Result<Value, String> {
    let path = paths::control_socket();
    let mut s = UnixStream::connect(&path).map_err(|e| format!("démon injoignable ({}) : {e}", path.display()))?;
    s.set_read_timeout(Some(timeout)).ok();
    s.set_write_timeout(Some(Duration::from_secs(5))).ok();
    let mut line = req.to_json();
    line.push('\n');
    s.write_all(line.as_bytes()).map_err(|e| e.to_string())?;
    let mut r = BufReader::new(s);
    let mut resp = String::new();
    r.read_line(&mut resp).map_err(|e| e.to_string())?;
    if resp.trim().is_empty() {
        return Err("réponse vide du démon".into());
    }
    json::parse(resp.trim()).map_err(|e| e.to_string())
}

/// Envoi sans attente de réponse utile (notifications des tâches).
pub fn notify(req: &Value) {
    let _ = request(req, Duration::from_secs(2));
}

pub fn daemon_running() -> bool {
    UnixStream::connect(paths::control_socket()).is_ok()
}
