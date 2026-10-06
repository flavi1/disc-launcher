//! Présentation : notifications freedesktop (avec actions) et repli sur une
//! boîte de dialogue (kdialog, zenity, yad). Textes en français ou en anglais
//! selon `LANG`.

use crate::dbus::{Bus, Connection, DV};
use std::process::Command;
use std::sync::mpsc::{self, Receiver, Sender};
use std::time::Duration;

// ------------------------------------------------------------------ textes

fn english() -> bool {
    let l = std::env::var("LC_ALL").or_else(|_| std::env::var("LC_MESSAGES")).or_else(|_| std::env::var("LANG")).unwrap_or_default();
    l.starts_with("en")
}

/// Texte traduit (français par défaut).
pub fn t(key: &str) -> &'static str {
    let en = english();
    macro_rules! tr {
        ($($k:literal => $fr:literal, $en:literal;)*) => {
            match key { $($k => if en { $en } else { $fr },)* _ => "?" }
        };
    }
    tr! {
        "play-existing" => "Lancer la copie", "Play existing copy";
        "play-disc" => "Jouer le disque", "Play the disc";
        "dump" => "Dumper", "Dump";
        "redump" => "Re-dumper", "Re-dump";
        "dump-then-play" => "Dumper puis jouer", "Dump then play";
        "verify" => "Vérifier la ROM", "Verify ROM";
        "eject" => "Éjecter", "Eject";
        "open-files" => "Ouvrir dans le gestionnaire de fichiers", "Open in file manager";
        "mount" => "Monter", "Mount";
        "unmount" => "Démonter", "Unmount";
        "safe-remove" => "Retirer en toute sécurité", "Safely remove";
        "usb-key" => "Volume USB", "USB volume";
        "sd-card" => "Carte mémoire", "Memory card";
        "safe-to-remove" => "Vous pouvez retirer ce périphérique en toute sécurité.", "This device can be safely removed.";
        "free-of" => "libres sur", "free of";
        "open-tray" => "Ouvrir le plateau", "Open tray";
        "close-tray" => "Fermer le plateau", "Close tray";
        "drive" => "Lecteur optique", "Optical drive";
        "drive-label" => "Lecteur", "Drive";
        "drive-empty" => "vide", "empty";
        "tray-open" => "plateau ouvert", "tray open";
        "drive-reading" => "lecture du disque…", "reading disc…";
        "blank-disc" => "disque vierge", "blank disc";
        "unknown-content" => "contenu non reconnu", "unrecognised content";
        "panel-title" => "Disques et périphériques", "Disks & Devices";
        "job-verify" => "Vérification", "Verification";
        "verify-done" => "ROM vérifiée", "ROM verified";
        "integrity-ok" => "fichier intègre", "file is intact";
        "data-disc" => "Disque de données", "Data disc";
        "retrode" => "Retrode", "Retrode";
        "mounted-on" => "Monté sur", "Mounted on";
        "not-mounted" => "Non monté", "Not mounted";
        "tray-empty" => "Aucun disque ni périphérique", "No disc or device";
        "w-refdb-empty" => "Base de référence vide pour ce système : nom provisoire. Corriger : disc-launcher refdb fetch", "Reference database empty for this system: provisional name. Fix: disc-launcher refdb fetch";
        "convert" => "Convertir", "Convert";
        "complete-game" => "Dumper ce disque", "Dump this disc";
        "play-game" => "Lancer le jeu", "Play the game";
        "restart-dump" => "Recommencer le dump", "Restart dump";
        "delete-partial" => "Supprimer le partiel", "Delete partial dump";
        "play-with" => "Lire avec", "Play with";
        "play-media" => "Lire le disque", "Play the disc";
        "no-player" => "Aucun lecteur multimédia disponible", "No media player available";
        "cancel" => "Annuler", "Cancel";
        "open-folder" => "Ouvrir le dossier", "Open folder";
        "play" => "Lancer", "Play";
        "dump-raw" => "Dumper (identification après lecture)", "Dump (identify after reading)";
        "sit-known" => "Copie existante", "Existing copy";
        "sit-present" => "Copie présente (origine non vérifiée)", "Copy present (unverified)";
        "sit-other-format" => "Copie dans un autre format", "Copy in another format";
        "sit-game-incomplete" => "Jeu incomplet : ce disque manque", "Incomplete game: this disc is missing";
        "sit-partial" => "Dump précédent interrompu", "Previous dump interrupted";
        "sit-new" => "Nouveau disque", "New disc";
        "w-drive-incompatible" => "Lecteur incompatible avec ce disque", "Drive cannot read this disc";
        "w-encrypted-keys-required" => "Disque chiffré : clés requises", "Encrypted disc: keys required";
        "w-gdrom-hd-area-unreadable" => "Zone haute densité du GD-ROM illisible avec ce lecteur", "GD-ROM high-density area unreadable on this drive";
        "w-xbox-video-partition-probable" => "Jeu Xbox ou Xbox 360 probable (partition vidéo seule visible)", "Probable Xbox/Xbox 360 game (only video partition visible)";
        "w-udf-not-mounted" => "Système de fichiers UDF non monté", "UDF filesystem not mounted";
        "w-unreadable-disc" => "Disque illisible par ce lecteur", "Disc unreadable by this drive";
        "w-identify-after-dump" => "Identification après lecture brute", "Will identify after raw read";
        "w-heuristic-identification" => "Identification incertaine", "Uncertain identification";
        "w-probe-budget-exceeded" => "Sondage incomplet", "Incomplete probe";
        "w-collision" => "Nom déjà pris par un autre disque : nom alternatif", "Name taken by another disc: alternative name";
        "conf-exact" => "✓ exact", "✓ exact";
        "conf-probable" => "≈ probable", "≈ probable";
        "conf-repli" => "? nom provisoire", "? provisional name";
        "is-game" => "Ce disque est un jeu", "This disc is a game";
        "is-cart" => "cartouche", "cartridge";
        "cart-dump" => "Dumper", "Dump";
        "cart-dump-then-play" => "Jouer", "Play";
        "cart-play-existing" => "Jouer", "Play";
        "cart-redump" => "Re-dumper", "Re-dump";
        "job-copy" => "Dump de la cartouche", "Cartridge dump";
        "job-copy-done" => "Cartouche dumpée", "Cartridge dumped";
        "job-dump" => "Dump", "Dump";
        "job-done" => "Dump terminé", "Dump complete";
        "job-failed" => "Échec", "Failed";
        "job-cancelled" => "Annulé", "Cancelled";
        "job-interrupted" => "Interrompu", "Interrupted";
        "verified-ok" => "conforme à la base de référence", "matches reference database";
        "verified-mismatch" => "NON CONFORME à l'entrée attendue", "does NOT match expected entry";
        "insert-next" => "Insérez le disque suivant", "Insert the next disc";
        "batch" => "Dumper tous les disques", "Dump all discs";
        "step" => "Étape", "Step";
        "disc" => "Disque", "Disc";
        "unknown-disc" => "Disque non identifié", "Unidentified disc";
    }
}

pub fn warning_text(w: &str) -> String {
    let k = format!("w-{w}");
    let s = t(&k);
    if s == "?" {
        w.to_string()
    } else {
        s.to_string()
    }
}

// ------------------------------------------------------------------ notifications

#[derive(Clone, Debug, Default)]
pub struct Notification {
    pub replaces: u32,
    pub summary: String,
    pub body: String,
    pub icon: String,
    /// (clé, libellé)
    pub actions: Vec<(String, String)>,
    pub resident: bool,
    pub progress: Option<u8>,
    pub urgency: u8,
    pub timeout_ms: i32,
}

#[derive(Clone, Debug)]
pub enum NotifyEvent {
    Action { id: u32, key: String },
    Closed { id: u32, reason: u32 },
}

#[derive(Clone, Debug, Default)]
pub struct Caps {
    pub actions: bool,
    pub persistence: bool,
    pub body_markup: bool,
}

enum Req {
    Show(Notification, Sender<Option<u32>>),
    Close(u32),
}

/// Fil propriétaire de la connexion de session : envoie les notifications
/// et relaie les signaux ActionInvoked / NotificationClosed.
pub struct Notifier {
    tx: Sender<Req>,
    pub caps: Caps,
}

const DEST: &str = "org.freedesktop.Notifications";
const PATH: &str = "/org/freedesktop/Notifications";

fn connect() -> std::io::Result<(Connection, Caps)> {
    let mut c = Connection::open(Bus::Session)?;
    let r = c.call(DEST, PATH, DEST, "GetCapabilities", vec![], Duration::from_secs(5))?;
    let list: Vec<String> = match r.body.first() {
        Some(DV::Array(_, v)) => v.iter().filter_map(|x| x.as_str().map(|s| s.to_string())).collect(),
        _ => vec![],
    };
    let caps = Caps { actions: list.iter().any(|x| x == "actions"), persistence: list.iter().any(|x| x == "persistence"), body_markup: list.iter().any(|x| x == "body-markup") };
    c.add_match(&format!("type='signal',interface='{DEST}',member='ActionInvoked'"))?;
    c.add_match(&format!("type='signal',interface='{DEST}',member='NotificationClosed'"))?;
    Ok((c, caps))
}

fn escape_markup(s: &str) -> String {
    s.replace('&', "&amp;").replace('<', "&lt;").replace('>', "&gt;")
}

fn send(c: &mut Connection, n: &Notification, caps: &Caps) -> std::io::Result<u32> {
    let mut acts = vec![];
    for (k, l) in &n.actions {
        acts.push(DV::str(k));
        acts.push(DV::str(l));
    }
    let mut hints = vec![("urgency", DV::Byte(n.urgency)), ("desktop-entry", DV::str("disc-launcher"))];
    if n.resident {
        hints.push(("resident", DV::Bool(true)));
    }
    if let Some(p) = n.progress {
        hints.push(("value", DV::I32(p as i32)));
    }
    let body = if caps.body_markup { escape_markup(&n.body) } else { n.body.clone() };
    let r = c.call(
        DEST,
        PATH,
        DEST,
        "Notify",
        vec![
            DV::str("disc-launcher"),
            DV::U32(n.replaces),
            DV::str(if n.icon.is_empty() { "media-optical" } else { &n.icon }),
            DV::str(&n.summary),
            DV::str(&body),
            DV::Array("s".into(), acts),
            DV::dict_sv(hints),
            DV::I32(n.timeout_ms),
        ],
        Duration::from_secs(5),
    )?;
    Ok(r.body.first().and_then(|v| v.as_u32()).unwrap_or(0))
}

impl Notifier {
    /// Démarre le fil. Renvoie None si aucun serveur de notifications n'est joignable.
    pub fn start(events: Sender<NotifyEvent>) -> Option<Notifier> {
        let (conn, caps) = match connect() {
            Ok(x) => x,
            Err(e) => {
                crate::dl_log!(warn, "notify", format!("serveur de notifications injoignable : {e}"));
                return None;
            }
        };
        let (tx, rx): (Sender<Req>, Receiver<Req>) = mpsc::channel();
        let caps2 = caps.clone();
        std::thread::Builder::new()
            .name("notifier".into())
            .spawn(move || {
                let mut conn = Some(conn);
                let mut caps = caps2;
                loop {
                    if conn.is_none() {
                        std::thread::sleep(Duration::from_secs(2));
                        match connect() {
                            Ok((c, k)) => {
                                conn = Some(c);
                                caps = k;
                            }
                            Err(_) => {
                                // vider les demandes pour ne pas bloquer les appelants
                                while let Ok(r) = rx.try_recv() {
                                    if let Req::Show(_, reply) = r {
                                        let _ = reply.send(None);
                                    }
                                }
                                continue;
                            }
                        }
                    }
                    let c = conn.as_mut().unwrap();
                    let mut broken = false;
                    while let Ok(r) = rx.try_recv() {
                        match r {
                            Req::Show(n, reply) => {
                                let res = send(c, &n, &caps);
                                if res.is_err() {
                                    broken = true;
                                }
                                let _ = reply.send(res.ok());
                            }
                            Req::Close(id) => {
                                let _ = c.call(DEST, PATH, DEST, "CloseNotification", vec![DV::U32(id)], Duration::from_secs(3));
                            }
                        }
                    }
                    match c.next_signal(Duration::from_millis(150)) {
                        Ok(Some(m)) => {
                            let id = m.body.first().and_then(|v| v.as_u32()).unwrap_or(0);
                            match m.member.as_deref() {
                                Some("ActionInvoked") => {
                                    let key = m.body.get(1).and_then(|v| v.as_str()).unwrap_or("").to_string();
                                    let _ = events.send(NotifyEvent::Action { id, key });
                                }
                                Some("NotificationClosed") => {
                                    let reason = m.body.get(1).and_then(|v| v.as_u32()).unwrap_or(0);
                                    let _ = events.send(NotifyEvent::Closed { id, reason });
                                }
                                _ => {}
                            }
                        }
                        Ok(None) => {}
                        Err(_) => broken = true,
                    }
                    if broken {
                        conn = None;
                    }
                }
            })
            .ok()?;
        Some(Notifier { tx, caps })
    }

    pub fn show(&self, n: Notification) -> Option<u32> {
        let (rtx, rrx) = mpsc::channel();
        self.tx.send(Req::Show(n, rtx)).ok()?;
        rrx.recv_timeout(Duration::from_secs(8)).ok().flatten()
    }

    pub fn close(&self, id: u32) {
        let _ = self.tx.send(Req::Close(id));
    }
}

// ------------------------------------------------------------------ boîte de dialogue

/// Outil de dialogue disponible, selon le bureau.
pub fn dialog_tool() -> Option<&'static str> {
    let desk = std::env::var("XDG_CURRENT_DESKTOP").unwrap_or_default().to_ascii_uppercase();
    let order: &[&str] = if desk.contains("KDE") || desk.contains("LXQT") { &["kdialog", "zenity", "yad"] } else { &["zenity", "yad", "kdialog"] };
    order.iter().copied().find(|t| crate::paths::which(t).is_some())
}

/// Boîte de choix bloquante ; renvoie la clé choisie. `on_spawn` reçoit le
/// PID de la boîte, pour pouvoir la fermer de l'extérieur.
pub fn dialog_choose(title: &str, text: &str, actions: &[(String, String)], on_spawn: impl FnOnce(u32)) -> Option<String> {
    let tool = dialog_tool()?;
    let mut c = Command::new(tool);
    match tool {
        "kdialog" => {
            c.arg("--title").arg(title).arg("--radiolist").arg(text);
            for (i, (k, l)) in actions.iter().enumerate() {
                c.arg(k).arg(l).arg(if i == 0 { "on" } else { "off" });
            }
        }
        _ => {
            c.arg("--list").arg("--radiolist").arg(format!("--title={title}")).arg(format!("--text={text}")).arg("--column=").arg("--column=key").arg("--column=Action").arg("--hide-column=2").arg("--print-column=2");
            if tool == "yad" {
                c.arg("--width=420").arg("--height=260");
            }
            for (i, (k, l)) in actions.iter().enumerate() {
                c.arg(if i == 0 { "TRUE" } else { "FALSE" }).arg(k).arg(l);
            }
        }
    }
    c.stdout(std::process::Stdio::piped()).stderr(std::process::Stdio::null());
    let child = c.spawn().ok()?;
    on_spawn(child.id());
    let out = child.wait_with_output().ok()?;
    if !out.status.success() {
        return None;
    }
    let s = String::from_utf8_lossy(&out.stdout).trim().trim_end_matches('|').to_string();
    if s.is_empty() {
        None
    } else {
        Some(s)
    }
}
