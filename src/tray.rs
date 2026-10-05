//! Icône de la zone de notification (StatusNotifierItem), sur le bus de
//! session, sans bibliothèque graphique. Un clic (gauche ou droit) affiche ou
//! masque la fenêtre « Disques et périphériques » (ou, à défaut, les
//! propositions en notifications). Pas de menu : la fenêtre fait déjà tout.
//!
//! Pris en charge par KDE Plasma, LXQt, Xfce (greffon « Status Notifier »),
//! Cinnamon, MATE (applet Ayatana), Budgie et GNOME avec l'extension
//! AppIndicator. Sans hôte compatible (LXDE/lxpanel, GNOME sans extension),
//! l'icône n'apparaît pas.

use crate::dbus::{Bus, Connection, Message, DV, METHOD_CALL};
use std::sync::mpsc::{Receiver, Sender, TryRecvError};
use std::time::{Duration, Instant};

const ITEM_PATH: &str = "/StatusNotifierItem";
const ITEM_IFACE: &str = "org.kde.StatusNotifierItem";
const WATCHER: &str = "org.kde.StatusNotifierWatcher";
pub const ICON: &str = "media-eject";

#[derive(Clone, Debug, Default, PartialEq)]
pub struct Model {
    /// Un disque, une cartouche, un volume ou une tâche est présent.
    pub active: bool,
    pub tooltip: String,
}

/// Événements renvoyés au démon.
#[derive(Clone, Debug, PartialEq)]
pub enum TrayEvent {
    /// Clic sur l'icône.
    Activate,
}

/// Démarre l'icône dans son propre fil. Les mises à jour arrivent par
/// `models` ; les clics repartent par `events`.
pub fn start(models: Receiver<Model>, events: Sender<TrayEvent>) {
    std::thread::Builder::new()
        .name("tray".into())
        .spawn(move || {
            let mut backoff = Duration::from_secs(5);
            loop {
                match run(&models, &events) {
                    Ok(()) => return,
                    Err(e) => {
                        crate::dl_log!(warn, "tray", format!("icône indisponible : {e} ; nouvel essai dans {} s", backoff.as_secs()));
                        std::thread::sleep(backoff);
                        backoff = (backoff * 2).min(Duration::from_secs(300));
                    }
                }
            }
        })
        .ok();
}

fn status(m: &Model) -> &'static str {
    if m.active {
        "Active"
    } else {
        "Passive"
    }
}

fn props(m: &Model) -> Vec<(&'static str, DV)> {
    let tooltip = DV::Struct(vec![DV::str(ICON), DV::Array("(iiay)".into(), vec![]), DV::str("disc-launcher"), DV::str(&m.tooltip)]);
    vec![
        ("Category", DV::str("Hardware")),
        ("Id", DV::str("disc-launcher")),
        ("Title", DV::str("disc-launcher")),
        ("Status", DV::str(status(m))),
        ("WindowId", DV::I32(0)),
        ("IconName", DV::str(ICON)),
        ("IconThemePath", DV::str("")),
        ("OverlayIconName", DV::str("")),
        ("AttentionIconName", DV::str(ICON)),
        ("ToolTip", tooltip),
        // Sans menu : le clic droit appelle ContextMenu, traité comme un clic.
        ("ItemIsMenu", DV::Bool(false)),
        ("Menu", DV::ObjPath("/NO_DBUSMENU".into())),
    ]
}

const INTROSPECT: &str = r#"<!DOCTYPE node PUBLIC "-//freedesktop//DTD D-BUS Object Introspection 1.0//EN" "http://www.freedesktop.org/standards/dbus/1.0/introspect.dtd"><node><interface name="org.freedesktop.DBus.Properties"><method name="Get"><arg type="s" direction="in"/><arg type="s" direction="in"/><arg type="v" direction="out"/></method><method name="GetAll"><arg type="s" direction="in"/><arg type="a{sv}" direction="out"/></method></interface><interface name="org.kde.StatusNotifierItem"><method name="Activate"><arg type="i" direction="in"/><arg type="i" direction="in"/></method><method name="SecondaryActivate"><arg type="i" direction="in"/><arg type="i" direction="in"/></method><method name="ContextMenu"><arg type="i" direction="in"/><arg type="i" direction="in"/></method><method name="Scroll"><arg type="i" direction="in"/><arg type="s" direction="in"/></method><signal name="NewStatus"><arg type="s"/></signal><signal name="NewToolTip"/><property name="Status" type="s" access="read"/><property name="IconName" type="s" access="read"/><property name="ItemIsMenu" type="b" access="read"/></interface></node>"#;

fn register(c: &mut Connection, name: &str) -> std::io::Result<()> {
    c.call(WATCHER, "/StatusNotifierWatcher", WATCHER, "RegisterStatusNotifierItem", vec![DV::str(name)], Duration::from_secs(5)).map(|_| ())
}

fn run(models: &Receiver<Model>, events: &Sender<TrayEvent>) -> std::io::Result<()> {
    let mut c = Connection::open(Bus::Session)?;
    let name = format!("org.kde.StatusNotifierItem-{}-1", std::process::id());
    c.request_name(&name)?;
    c.add_match(&format!("type='signal',sender='org.freedesktop.DBus',member='NameOwnerChanged',arg0='{WATCHER}'"))?;
    let mut model = Model::default();
    let mut registered = register(&mut c, &name).is_ok();
    if registered {
        crate::dl_log!(info, "tray", "icône enregistrée", "name" => name);
    } else {
        crate::dl_log!(info, "tray", "aucun hôte d'icônes (StatusNotifierWatcher) pour l'instant");
    }
    let mut last_try = Instant::now();
    loop {
        let mut changed = None;
        loop {
            match models.try_recv() {
                Ok(m) => changed = Some(m),
                Err(TryRecvError::Empty) => break,
                Err(TryRecvError::Disconnected) => return Ok(()),
            }
        }
        if let Some(m) = changed {
            if m != model {
                let old = status(&model);
                model = m;
                let _ = c.emit(ITEM_PATH, ITEM_IFACE, "NewToolTip", vec![]);
                if status(&model) != old {
                    let _ = c.emit(ITEM_PATH, ITEM_IFACE, "NewStatus", vec![DV::str(status(&model))]);
                }
            }
        }
        if !registered && last_try.elapsed() > Duration::from_secs(30) {
            last_try = Instant::now();
            registered = register(&mut c, &name).is_ok();
        }
        let Some(m) = c.next_message(Duration::from_millis(200))? else { continue };
        if m.mtype != METHOD_CALL {
            // Hôte d'icônes (re)démarré : se réenregistrer.
            if m.member.as_deref() == Some("NameOwnerChanged") && m.body.get(2).and_then(|v| v.as_str()).is_some_and(|s| !s.is_empty()) {
                registered = register(&mut c, &name).is_ok();
            }
            continue;
        }
        handle_call(&mut c, &model, &m, events)?;
    }
}

fn handle_call(c: &mut Connection, model: &Model, m: &Message, events: &Sender<TrayEvent>) -> std::io::Result<()> {
    let iface = m.interface.as_deref().unwrap_or("");
    let member = m.member.as_deref().unwrap_or("");
    match (iface, member) {
        ("org.freedesktop.DBus.Introspectable", "Introspect") => c.reply(m, vec![DV::str(INTROSPECT)]),
        ("org.freedesktop.DBus.Peer", "Ping") => c.reply(m, vec![]),
        ("org.freedesktop.DBus.Properties", "GetAll") => c.reply(m, vec![DV::dict_sv(props(model))]),
        ("org.freedesktop.DBus.Properties", "Get") => {
            let want = m.body.get(1).and_then(|v| v.as_str()).unwrap_or("").to_string();
            match props(model).into_iter().find(|(k, _)| *k == want) {
                Some((_, v)) => c.reply(m, vec![DV::Variant(Box::new(v))]),
                None => c.reply_error(m, "org.freedesktop.DBus.Error.UnknownProperty", &want),
            }
        }
        (ITEM_IFACE, "Activate") | (ITEM_IFACE, "SecondaryActivate") | (ITEM_IFACE, "ContextMenu") => {
            let _ = events.send(TrayEvent::Activate);
            c.reply(m, vec![])
        }
        (ITEM_IFACE, _) => c.reply(m, vec![]),
        _ => c.reply_error(m, "org.freedesktop.DBus.Error.UnknownMethod", &format!("{iface}.{member}")),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn properties() {
        let p = props(&Model { active: true, tooltip: "Rayman 2".into() });
        let dict = DV::dict_sv(p);
        assert_eq!(dict.signature(), "a{sv}");
        assert!(format!("{dict:?}").contains("media-eject"));
        assert_eq!(status(&Model::default()), "Passive");
    }
}
