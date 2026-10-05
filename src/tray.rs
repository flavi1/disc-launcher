//! Icône de la zone de notification (StatusNotifierItem) et son menu
//! (com.canonical.dbusmenu), sur le bus de session, sans bibliothèque
//! graphique.
//!
//! Pris en charge par KDE Plasma, LXQt, Xfce (greffon « Status Notifier »),
//! Cinnamon, MATE (applet Ayatana) et GNOME avec l'extension AppIndicator
//! (installée d'office par Ubuntu). Sans hôte compatible (LXDE/lxpanel, GNOME
//! sans extension), l'icône n'apparaît pas ; les notifications restent.
//!
//! - **Clic gauche** : affiche ou masque les propositions en cours
//!   (notifications ou boîtes de dialogue).
//! - **Clic droit** : un menu unique pour tous les disques, cartouches et clés
//!   USB présents, chacun suivi de ses actions, puis les tâches en cours.

use crate::dbus::{Bus, Connection, Message, DV, METHOD_CALL};
use std::sync::mpsc::{Receiver, Sender, TryRecvError};
use std::time::{Duration, Instant};

const ITEM_PATH: &str = "/StatusNotifierItem";
const MENU_PATH: &str = "/MenuBar";
const ITEM_IFACE: &str = "org.kde.StatusNotifierItem";
const MENU_IFACE: &str = "com.canonical.dbusmenu";
const WATCHER: &str = "org.kde.StatusNotifierWatcher";

/// Un périphérique et ses actions, tel qu'affiché dans le menu.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct Entry {
    /// Clé du périphérique côté démon (`/dev/sr0`, fichier ROM, `/dev/sdb1`).
    pub dev: String,
    pub title: String,
    pub icon: String,
    /// (clé d'action, libellé)
    pub actions: Vec<(String, String)>,
}

#[derive(Clone, Debug, Default, PartialEq)]
pub struct Model {
    pub entries: Vec<Entry>,
    /// Actions générales en fin de menu : (clé, libellé).
    pub general: Vec<(String, String)>,
    pub tooltip: String,
}

/// Événements renvoyés au démon.
#[derive(Clone, Debug, PartialEq)]
pub enum TrayEvent {
    /// Clic gauche sur l'icône.
    Activate,
    /// Élément de menu : (périphérique, ou vide pour une action générale ; clé d'action).
    Action(String, String),
}

/// Démarre l'icône dans son propre fil. Les mises à jour du menu arrivent par
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

struct State {
    model: Model,
    revision: u32,
    /// id de menu → (périphérique, action)
    ids: Vec<(i32, String, String)>,
}

fn escape_label(s: &str) -> String {
    // « _ » introduit un raccourci clavier dans dbusmenu.
    s.replace('_', "__")
}

fn item(id: i32, props: Vec<(&str, DV)>, children: Vec<DV>) -> DV {
    DV::Struct(vec![DV::I32(id), DV::dict_sv(props), DV::Array("v".into(), children)])
}

impl State {
    /// Arbre du menu : racine 0, puis éléments à plat.
    fn layout(&mut self) -> DV {
        self.ids.clear();
        let mut kids: Vec<DV> = vec![];
        let mut next = 1;
        let mut push = |kids: &mut Vec<DV>, props: Vec<(&str, DV)>| {
            kids.push(DV::Variant(Box::new(item(next, props, vec![]))));
            next += 1;
            next - 1
        };
        let empty = self.model.entries.is_empty();
        if empty {
            push(&mut kids, vec![("label", DV::str(crate::notify::t("tray-empty"))), ("enabled", DV::Bool(false))]);
        }
        for (i, e) in self.model.entries.iter().enumerate() {
            if i > 0 {
                push(&mut kids, vec![("type", DV::str("separator"))]);
            }
            let mut p = vec![("label", DV::str(&escape_label(&e.title))), ("enabled", DV::Bool(false))];
            if !e.icon.is_empty() {
                p.push(("icon-name", DV::str(&e.icon)));
            }
            push(&mut kids, p);
            for (k, l) in &e.actions {
                let id = push(&mut kids, vec![("label", DV::str(&format!("    {}", escape_label(l))))]);
                self.ids.push((id, e.dev.clone(), k.clone()));
            }
        }
        if !self.model.general.is_empty() {
            push(&mut kids, vec![("type", DV::str("separator"))]);
            for (k, l) in &self.model.general {
                let id = push(&mut kids, vec![("label", DV::str(&escape_label(l)))]);
                self.ids.push((id, String::new(), k.clone()));
            }
        }
        item(0, vec![("children-display", DV::str("submenu"))], kids)
    }

    fn status(&self) -> &'static str {
        if self.model.entries.is_empty() {
            "Passive"
        } else {
            "Active"
        }
    }

    fn item_props(&self) -> Vec<(&'static str, DV)> {
        let tooltip = DV::Struct(vec![
            DV::str("media-optical"),
            DV::Array("(iiay)".into(), vec![]),
            DV::str("disc-launcher"),
            DV::str(&self.model.tooltip),
        ]);
        vec![
            ("Category", DV::str("Hardware")),
            ("Id", DV::str("disc-launcher")),
            ("Title", DV::str("disc-launcher")),
            ("Status", DV::str(self.status())),
            ("WindowId", DV::I32(0)),
            ("IconName", DV::str("media-optical")),
            ("IconThemePath", DV::str("")),
            ("OverlayIconName", DV::str("")),
            ("AttentionIconName", DV::str("media-optical")),
            ("ToolTip", tooltip),
            ("ItemIsMenu", DV::Bool(false)),
            ("Menu", DV::ObjPath(MENU_PATH.into())),
        ]
    }

    fn menu_props(&self) -> Vec<(&'static str, DV)> {
        vec![("Version", DV::U32(3)), ("TextDirection", DV::str("ltr")), ("Status", DV::str("normal")), ("IconThemePath", DV::Array("s".into(), vec![]))]
    }
}

fn introspect(path: &str) -> String {
    let body = match path {
        ITEM_PATH => r#"<interface name="org.kde.StatusNotifierItem"><method name="Activate"><arg type="i" direction="in"/><arg type="i" direction="in"/></method><method name="SecondaryActivate"><arg type="i" direction="in"/><arg type="i" direction="in"/></method><method name="ContextMenu"><arg type="i" direction="in"/><arg type="i" direction="in"/></method><method name="Scroll"><arg type="i" direction="in"/><arg type="s" direction="in"/></method><signal name="NewStatus"><arg type="s"/></signal><signal name="NewToolTip"/><property name="Status" type="s" access="read"/><property name="IconName" type="s" access="read"/><property name="Menu" type="o" access="read"/><property name="ItemIsMenu" type="b" access="read"/></interface>"#,
        MENU_PATH => r#"<interface name="com.canonical.dbusmenu"><method name="GetLayout"><arg type="i" direction="in"/><arg type="i" direction="in"/><arg type="as" direction="in"/><arg type="u" direction="out"/><arg type="(ia{sv}av)" direction="out"/></method><method name="Event"><arg type="i" direction="in"/><arg type="s" direction="in"/><arg type="v" direction="in"/><arg type="u" direction="in"/></method><method name="AboutToShow"><arg type="i" direction="in"/><arg type="b" direction="out"/></method><signal name="LayoutUpdated"><arg type="u"/><arg type="i"/></signal><property name="Version" type="u" access="read"/></interface>"#,
        _ => "",
    };
    format!(
        "<!DOCTYPE node PUBLIC \"-//freedesktop//DTD D-BUS Object Introspection 1.0//EN\" \"http://www.freedesktop.org/standards/dbus/1.0/introspect.dtd\"><node><interface name=\"org.freedesktop.DBus.Properties\"><method name=\"Get\"><arg type=\"s\" direction=\"in\"/><arg type=\"s\" direction=\"in\"/><arg type=\"v\" direction=\"out\"/></method><method name=\"GetAll\"><arg type=\"s\" direction=\"in\"/><arg type=\"a{{sv}}\" direction=\"out\"/></method></interface>{body}</node>"
    )
}

fn find_node(layout: &DV, id: i32) -> Option<DV> {
    let DV::Struct(v) = layout else { return None };
    if v.first() == Some(&DV::I32(id)) {
        return Some(layout.clone());
    }
    if let Some(DV::Array(_, kids)) = v.get(2) {
        for k in kids {
            if let DV::Variant(inner) = k {
                if let Some(n) = find_node(inner, id) {
                    return Some(n);
                }
            }
        }
    }
    None
}

fn register(c: &mut Connection, name: &str) -> std::io::Result<()> {
    c.call(WATCHER, "/StatusNotifierWatcher", WATCHER, "RegisterStatusNotifierItem", vec![DV::str(name)], Duration::from_secs(5)).map(|_| ())
}

fn run(models: &Receiver<Model>, events: &Sender<TrayEvent>) -> std::io::Result<()> {
    let mut c = Connection::open(Bus::Session)?;
    let name = format!("org.kde.StatusNotifierItem-{}-1", std::process::id());
    c.request_name(&name)?;
    c.add_match(&format!("type='signal',sender='org.freedesktop.DBus',member='NameOwnerChanged',arg0='{WATCHER}'"))?;
    let mut st = State { model: Model::default(), revision: 1, ids: vec![] };
    let mut registered = register(&mut c, &name).is_ok();
    if registered {
        crate::dl_log!(info, "tray", "icône enregistrée", "name" => name);
    } else {
        crate::dl_log!(info, "tray", "aucun hôte d'icônes (StatusNotifierWatcher) pour l'instant");
    }
    let mut last_try = Instant::now();
    loop {
        // Mises à jour du modèle (on ne garde que la dernière).
        let mut changed = None;
        loop {
            match models.try_recv() {
                Ok(m) => changed = Some(m),
                Err(TryRecvError::Empty) => break,
                Err(TryRecvError::Disconnected) => return Ok(()),
            }
        }
        if let Some(m) = changed {
            if m != st.model {
                let old_status = st.status();
                st.model = m;
                st.revision += 1;
                st.layout(); // identifiants des éléments à jour avant tout clic
                let _ = c.emit(MENU_PATH, MENU_IFACE, "LayoutUpdated", vec![DV::U32(st.revision), DV::I32(0)]);
                let _ = c.emit(ITEM_PATH, ITEM_IFACE, "NewToolTip", vec![]);
                if st.status() != old_status {
                    let _ = c.emit(ITEM_PATH, ITEM_IFACE, "NewStatus", vec![DV::str(st.status())]);
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
        handle_call(&mut c, &mut st, &m, events)?;
    }
}

fn handle_call(c: &mut Connection, st: &mut State, m: &Message, events: &Sender<TrayEvent>) -> std::io::Result<()> {
    let path = m.path.as_deref().unwrap_or("");
    let iface = m.interface.as_deref().unwrap_or("");
    let member = m.member.as_deref().unwrap_or("");
    let arg_str = |i: usize| m.body.get(i).and_then(|v| v.as_str()).unwrap_or("").to_string();
    let arg_i32 = |i: usize| match m.body.get(i) {
        Some(DV::I32(x)) => *x,
        _ => 0,
    };
    match (iface, member) {
        ("org.freedesktop.DBus.Introspectable", "Introspect") => c.reply(m, vec![DV::str(&introspect(path))]),
        ("org.freedesktop.DBus.Peer", "Ping") => c.reply(m, vec![]),
        ("org.freedesktop.DBus.Properties", "Get") | ("org.freedesktop.DBus.Properties", "GetAll") => {
            let props = if path == MENU_PATH { st.menu_props() } else { st.item_props() };
            if member == "GetAll" {
                c.reply(m, vec![DV::dict_sv(props)])
            } else {
                let want = arg_str(1);
                match props.into_iter().find(|(k, _)| *k == want) {
                    Some((_, v)) => c.reply(m, vec![DV::Variant(Box::new(v))]),
                    None => c.reply_error(m, "org.freedesktop.DBus.Error.UnknownProperty", &want),
                }
            }
        }
        (ITEM_IFACE, "Activate") | (ITEM_IFACE, "SecondaryActivate") => {
            let _ = events.send(TrayEvent::Activate);
            c.reply(m, vec![])
        }
        (ITEM_IFACE, _) => c.reply(m, vec![]),
        (MENU_IFACE, "GetLayout") => {
            let root = st.layout();
            let node = find_node(&root, arg_i32(0)).unwrap_or(root);
            c.reply(m, vec![DV::U32(st.revision), node])
        }
        (MENU_IFACE, "GetGroupProperties") => {
            let root = st.layout();
            let ids: Vec<i32> = match m.body.first() {
                Some(DV::Array(_, v)) => v.iter().filter_map(|x| if let DV::I32(i) = x { Some(*i) } else { None }).collect(),
                _ => vec![],
            };
            let mut out = vec![];
            for id in ids {
                if let Some(DV::Struct(v)) = find_node(&root, id) {
                    out.push(DV::Struct(vec![DV::I32(id), v.get(1).cloned().unwrap_or_else(DV::empty_dict)]));
                }
            }
            c.reply(m, vec![DV::Array("(ia{sv})".into(), out)])
        }
        (MENU_IFACE, "GetProperty") => {
            let root = st.layout();
            let (id, name) = (arg_i32(0), arg_str(1));
            let val = find_node(&root, id).and_then(|n| match n {
                DV::Struct(v) => match v.get(1) {
                    Some(DV::Array(_, entries)) => entries.iter().find_map(|e| match e {
                        DV::Dict(k, val) if k.as_str() == Some(name.as_str()) => Some((**val).clone()),
                        _ => None,
                    }),
                    _ => None,
                },
                _ => None,
            });
            match val {
                Some(v) => c.reply(m, vec![v]),
                None => c.reply_error(m, "org.freedesktop.DBus.Error.InvalidArgs", "propriété inconnue"),
            }
        }
        (MENU_IFACE, "Event") => {
            if arg_str(1) == "clicked" {
                let id = arg_i32(0);
                if let Some((_, dev, key)) = st.ids.iter().find(|(i, _, _)| *i == id) {
                    let _ = events.send(TrayEvent::Action(dev.clone(), key.clone()));
                }
            }
            c.reply(m, vec![])
        }
        (MENU_IFACE, "EventGroup") => {
            if let Some(DV::Array(_, evs)) = m.body.first() {
                for e in evs {
                    if let DV::Struct(v) = e {
                        if let (Some(DV::I32(id)), Some(DV::Str(kind))) = (v.first(), v.get(1)) {
                            if kind == "clicked" {
                                if let Some((_, dev, key)) = st.ids.iter().find(|(i, _, _)| i == id) {
                                    let _ = events.send(TrayEvent::Action(dev.clone(), key.clone()));
                                }
                            }
                        }
                    }
                }
            }
            c.reply(m, vec![DV::Array("i".into(), vec![])])
        }
        (MENU_IFACE, "AboutToShow") => c.reply(m, vec![DV::Bool(false)]),
        (MENU_IFACE, "AboutToShowGroup") => c.reply(m, vec![DV::Array("i".into(), vec![]), DV::Array("i".into(), vec![])]),
        _ => c.reply_error(m, "org.freedesktop.DBus.Error.UnknownMethod", &format!("{iface}.{member}")),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn layout_and_ids() {
        let mut st = State {
            model: Model {
                entries: vec![
                    Entry { dev: "/dev/sr0".into(), title: "Sony PlayStation — Rayman 2".into(), icon: "media-optical".into(), actions: vec![("play-existing".into(), "Lancer la copie".into()), ("eject".into(), "Éjecter".into())] },
                    Entry { dev: "/dev/sdb1".into(), title: "Clé USB — MA_CLE".into(), icon: String::new(), actions: vec![("open-files".into(), "Ouvrir".into())] },
                ],
                general: vec![("show".into(), "Afficher les propositions".into())],
                tooltip: String::new(),
            },
            revision: 1,
            ids: vec![],
        };
        let root = st.layout();
        // titre, 2 actions, séparateur, titre, 1 action, séparateur, 1 action générale
        let DV::Struct(v) = &root else { panic!() };
        let DV::Array(sig, kids) = &v[2] else { panic!() };
        assert_eq!(sig, "v");
        assert_eq!(kids.len(), 8);
        assert_eq!(st.ids.len(), 4);
        assert_eq!(st.ids[0], (2, "/dev/sr0".into(), "play-existing".into()));
        assert_eq!(st.ids[3].1, "");
        assert_eq!(root.signature(), "(ia{sv}av)");
        assert!(find_node(&root, 6).is_some());
        assert!(find_node(&root, 9).is_none());
        // Le titre de la clé est échappé (« _ » = raccourci clavier dans dbusmenu).
        assert!(format!("{:?}", find_node(&root, 5).unwrap()).contains("MA__CLE"));
    }
}
