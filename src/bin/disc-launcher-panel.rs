//! Fenêtre « Disques et périphériques » : une seule boîte pour tous les
//! disques, cartouches, volumes USB et tâches, affichée ou masquée par le démon
//! (insertion, clic sur l'icône de la zone de notification).
//!
//! GTK 3 est chargé à l'exécution (`dlopen`) : aucune dépendance de
//! compilation, et sans GTK ni affichage le programme s'arrête (code 3) et le
//! démon revient aux notifications.
//!
//! Protocole (lignes de texte) :
//! - entrée standard, du démon : un état JSON par ligne
//!   `{"visible":bool,"title":…,"empty":…,"items":[{"id","icon","title","lines":[…],
//!   "usage":{"fraction","text"}|null,"progress":{"fraction","text"}|null,"actions":[[clé,libellé]…]}]}` ;
//! - sortie standard, vers le démon : `ready`, `hidden` (fenêtre fermée par
//!   l'utilisateur), `action\t<id>\t<clé>`.
//!
//! `disc-launcher-panel --check` : vérifie seulement que GTK s'initialise.

use disclauncher::json::{self, Value};
use std::cell::RefCell;
use std::ffi::{c_char, c_int, c_void, CString};
use std::io::{BufRead, Write};
use std::sync::{Arc, Mutex, OnceLock};

type P = *mut c_void;
type GBool = c_int;

extern "C" {
    fn dlopen(file: *const c_char, mode: c_int) -> P;
    fn dlsym(handle: P, name: *const c_char) -> P;
}
const RTLD_NOW: c_int = 2;
const RTLD_GLOBAL: c_int = 0x100;

macro_rules! gtk_api {
    ($($name:ident : fn($($a:ty),*) $(-> $r:ty)?;)*) => {
        #[allow(non_snake_case)]
        struct Gtk { $($name: unsafe extern "C" fn($($a),*) $(-> $r)?,)* }
        impl Gtk {
            fn load() -> Result<Gtk, String> {
                let mut h: P = std::ptr::null_mut();
                for lib in ["libgtk-3.so.0", "libgtk-3.so"] {
                    let c = CString::new(lib).unwrap();
                    h = unsafe { dlopen(c.as_ptr(), RTLD_NOW | RTLD_GLOBAL) };
                    if !h.is_null() { break; }
                }
                if h.is_null() {
                    return Err("GTK 3 introuvable (libgtk-3.so.0)".into());
                }
                Ok(Gtk { $($name: {
                    let c = CString::new(stringify!($name)).unwrap();
                    let p = unsafe { dlsym(h, c.as_ptr()) };
                    if p.is_null() { return Err(format!("symbole GTK absent : {}", stringify!($name))); }
                    unsafe { std::mem::transmute::<P, unsafe extern "C" fn($($a),*) $(-> $r)?>(p) }
                },)* })
            }
        }
    };
}

gtk_api! {
    gtk_init_check: fn(*mut c_int, *mut *mut *mut c_char) -> GBool;
    gtk_main: fn();
    gtk_main_quit: fn();
    gtk_window_new: fn(c_int) -> P;
    gtk_window_set_title: fn(P, *const c_char);
    gtk_window_set_default_size: fn(P, c_int, c_int);
    gtk_window_set_icon_name: fn(P, *const c_char);
    gtk_window_set_position: fn(P, c_int);
    gtk_window_present: fn(P);
    gtk_container_add: fn(P, P);
    gtk_container_set_border_width: fn(P, u32);
    gtk_box_new: fn(c_int, c_int) -> P;
    gtk_box_pack_start: fn(P, P, GBool, GBool, u32);
    gtk_label_new: fn(*const c_char) -> P;
    gtk_label_set_markup: fn(P, *const c_char);
    gtk_label_set_text: fn(P, *const c_char);
    gtk_label_set_xalign: fn(P, f32);
    gtk_label_set_line_wrap: fn(P, GBool);
    gtk_label_set_max_width_chars: fn(P, c_int);
    gtk_button_new_with_label: fn(*const c_char) -> P;
    gtk_image_new_from_icon_name: fn(*const c_char, c_int) -> P;
    gtk_progress_bar_new: fn() -> P;
    gtk_progress_bar_set_fraction: fn(P, f64);
    gtk_progress_bar_set_text: fn(P, *const c_char);
    gtk_progress_bar_set_show_text: fn(P, GBool);
    gtk_separator_new: fn(c_int) -> P;
    gtk_scrolled_window_new: fn(P, P) -> P;
    gtk_scrolled_window_set_policy: fn(P, c_int, c_int);
    gtk_scrolled_window_set_propagate_natural_height: fn(P, GBool);
    gtk_scrolled_window_set_max_content_height: fn(P, c_int);
    gtk_button_new_from_icon_name: fn(*const c_char, c_int) -> P;
    gtk_button_set_relief: fn(P, c_int);
    gtk_widget_set_tooltip_text: fn(P, *const c_char);
    gtk_widget_show_all: fn(P);
    gtk_widget_hide: fn(P);
    gtk_widget_destroy: fn(P);
    gtk_widget_set_valign: fn(P, c_int);
    gtk_widget_set_halign: fn(P, c_int);
    gtk_widget_set_margin_top: fn(P, c_int);
    gtk_widget_set_margin_bottom: fn(P, c_int);
    gtk_widget_set_margin_start: fn(P, c_int);
    gtk_widget_set_margin_end: fn(P, c_int);
    gtk_widget_set_can_focus: fn(P, GBool);
    gtk_widget_get_style_context: fn(P) -> P;
    gtk_style_context_add_class: fn(P, *const c_char);
    g_signal_connect_data: fn(P, *const c_char, P, P, P, c_int) -> u64;
    g_timeout_add: fn(u32, P, P) -> u32;
}

const VERTICAL: c_int = 1;
const HORIZONTAL: c_int = 0;
const ICON_SIZE_DND: c_int = 5;
const ICON_SIZE_BUTTON: c_int = 4;
const RELIEF_NONE: c_int = 2;
/// Actions affichées en icône à droite du titre (comme le notificateur de Plasma).
const ICON_ACTIONS: &[(&str, &str)] = &[("eject", "media-eject"), ("safe-remove", "media-eject")];
const ALIGN_START: c_int = 1;
const POLICY_AUTOMATIC: c_int = 1;
const POLICY_NEVER: c_int = 2;
const WIN_POS_CENTER: c_int = 1;

static GTK: OnceLock<Gtk> = OnceLock::new();
fn g() -> &'static Gtk {
    GTK.get().expect("GTK chargé")
}

fn cs(s: &str) -> CString {
    CString::new(s.replace('\0', "")).unwrap()
}

fn escape(s: &str) -> String {
    s.replace('&', "&amp;").replace('<', "&lt;").replace('>', "&gt;")
}

fn emit(line: &str) {
    let mut o = std::io::stdout().lock();
    let _ = writeln!(o, "{line}");
    let _ = o.flush();
}

/// Widgets mis à jour sans reconstruire la fenêtre (progression, textes).
struct Live {
    title: P,
    lines: Option<P>,
    usage: Option<P>,
    progress: Option<P>,
}

struct Ui {
    window: P,
    holder: P,
    list: P,
    /// Forme de la liste (identifiants, actions, barres) : la reconstruire
    /// seulement si elle change.
    shape: String,
    live: Vec<Live>,
    visible: bool,
}

thread_local! {
    static UI: RefCell<Option<Ui>> = const { RefCell::new(None) };
}

unsafe extern "C" fn on_clicked(_w: P, data: P) {
    let (id, key) = &*(data as *const (String, String));
    emit(&format!("action\t{id}\t{key}"));
}

unsafe extern "C" fn free_data(data: P, _closure: P) {
    drop(Box::from_raw(data as *mut (String, String)));
}

unsafe extern "C" fn on_delete(w: P, _ev: P, _d: P) -> GBool {
    (g().gtk_widget_hide)(w);
    UI.with(|u| {
        if let Some(ui) = u.borrow_mut().as_mut() {
            ui.visible = false;
        }
    });
    emit("hidden");
    1
}

fn shape_of(state: &Value) -> String {
    let mut s = String::new();
    for it in state["items"].as_arr() {
        s.push_str(it["id"].str_or(""));
        s.push('|');
        s.push_str(it["icon"].str_or(""));
        for a in it["actions"].as_arr() {
            s.push_str(&a.as_arr().iter().map(|x| x.str_or("")).collect::<Vec<_>>().join("="));
            s.push(',');
        }
        s.push_str(&format!("|{}{}{}\n", !it["usage"].is_null(), !it["progress"].is_null(), !it["lines"].as_arr().is_empty()));
    }
    s
}

fn lines_text(it: &Value) -> String {
    it["lines"].as_arr().iter().map(|l| l.str_or("").to_string()).collect::<Vec<_>>().join("\n")
}

fn set_bar(bar: P, v: &Value) {
    let g = g();
    unsafe {
        (g.gtk_progress_bar_set_fraction)(bar, v["fraction"].as_f64().unwrap_or(0.0).clamp(0.0, 1.0));
        (g.gtk_progress_bar_set_text)(bar, cs(v["text"].str_or("")).as_ptr());
    }
}

fn label(text: &str, markup: bool, dim: bool) -> P {
    let g = g();
    unsafe {
        let l = (g.gtk_label_new)(std::ptr::null());
        if markup {
            (g.gtk_label_set_markup)(l, cs(text).as_ptr());
        } else {
            (g.gtk_label_set_text)(l, cs(text).as_ptr());
        }
        (g.gtk_label_set_xalign)(l, 0.0);
        (g.gtk_label_set_line_wrap)(l, 1);
        (g.gtk_label_set_max_width_chars)(l, 52);
        if dim {
            (g.gtk_style_context_add_class)((g.gtk_widget_get_style_context)(l), cs("dim-label").as_ptr());
        }
        l
    }
}

fn bar(v: &Value) -> P {
    let g = g();
    unsafe {
        let b = (g.gtk_progress_bar_new)();
        (g.gtk_progress_bar_set_show_text)(b, 1);
        set_bar(b, v);
        b
    }
}

fn title_markup(it: &Value) -> String {
    format!("<b>{}</b>", escape(it["title"].str_or("")))
}

fn build(ui: &mut Ui, state: &Value) {
    let g = g();
    unsafe {
        (g.gtk_widget_destroy)(ui.list);
        let list = (g.gtk_box_new)(VERTICAL, 0);
        (g.gtk_box_pack_start)(ui.holder, list, 1, 1, 0);
        ui.list = list;
        ui.live.clear();
        let items = state["items"].as_arr();
        if items.is_empty() {
            let l = label(state["empty"].str_or(""), false, true);
            (g.gtk_widget_set_margin_top)(l, 24);
            (g.gtk_widget_set_margin_bottom)(l, 24);
            (g.gtk_widget_set_halign)(l, 3);
            (g.gtk_box_pack_start)(list, l, 0, 0, 0);
        }
        for (i, it) in items.iter().enumerate() {
            if i > 0 {
                (g.gtk_box_pack_start)(list, (g.gtk_separator_new)(HORIZONTAL), 0, 0, 0);
            }
            let row = (g.gtk_box_new)(HORIZONTAL, 12);
            (g.gtk_widget_set_margin_top)(row, 10);
            (g.gtk_widget_set_margin_bottom)(row, 10);
            (g.gtk_widget_set_margin_start)(row, 12);
            (g.gtk_widget_set_margin_end)(row, 12);
            let icon = (g.gtk_image_new_from_icon_name)(cs(it["icon"].str_or("media-optical")).as_ptr(), ICON_SIZE_DND);
            (g.gtk_widget_set_valign)(icon, ALIGN_START);
            (g.gtk_box_pack_start)(row, icon, 0, 0, 0);
            let col = (g.gtk_box_new)(VERTICAL, 4);
            (g.gtk_box_pack_start)(row, col, 1, 1, 0);
            let head = (g.gtk_box_new)(HORIZONTAL, 6);
            (g.gtk_box_pack_start)(col, head, 0, 0, 0);
            let title = label(&title_markup(it), true, false);
            (g.gtk_box_pack_start)(head, title, 1, 1, 0);
            let connect = |b: P, key: &str| {
                let data = Box::into_raw(Box::new((it["id"].str_or("").to_string(), key.to_string()))) as P;
                (g.g_signal_connect_data)(b, cs("clicked").as_ptr(), on_clicked as *const () as P, data, free_data as *const () as P, 0);
            };
            for a in it["actions"].as_arr() {
                let pair = a.as_arr();
                let key = pair.first().map(|x| x.str_or("")).unwrap_or("");
                if let Some((_, icon_name)) = ICON_ACTIONS.iter().find(|(k, _)| *k == key) {
                    let b = (g.gtk_button_new_from_icon_name)(cs(icon_name).as_ptr(), ICON_SIZE_BUTTON);
                    (g.gtk_button_set_relief)(b, RELIEF_NONE);
                    (g.gtk_widget_set_tooltip_text)(b, cs(pair.get(1).map(|x| x.str_or("")).unwrap_or("")).as_ptr());
                    (g.gtk_widget_set_valign)(b, ALIGN_START);
                    connect(b, key);
                    (g.gtk_box_pack_start)(head, b, 0, 0, 0);
                }
            }
            let mut live = Live { title, lines: None, usage: None, progress: None };
            if !it["lines"].as_arr().is_empty() {
                let l = label(&lines_text(it), false, true);
                (g.gtk_box_pack_start)(col, l, 0, 0, 0);
                live.lines = Some(l);
            }
            for (k, slot) in [("usage", &mut live.usage), ("progress", &mut live.progress)] {
                if !it[k].is_null() {
                    let b = bar(&it[k]);
                    (g.gtk_widget_set_margin_top)(b, 2);
                    (g.gtk_box_pack_start)(col, b, 0, 0, 0);
                    *slot = Some(b);
                }
            }
            let acts: Vec<&Value> = it["actions"].as_arr().iter().filter(|a| {
                let k = a.as_arr().first().map(|x| x.str_or("")).unwrap_or("");
                !ICON_ACTIONS.iter().any(|(i, _)| *i == k)
            }).collect();
            if !acts.is_empty() {
                let buttons = (g.gtk_box_new)(HORIZONTAL, 6);
                (g.gtk_widget_set_margin_top)(buttons, 4);
                for a in acts {
                    let pair = a.as_arr();
                    let key = pair.first().map(|x| x.str_or("")).unwrap_or("");
                    let b = (g.gtk_button_new_with_label)(cs(pair.get(1).map(|x| x.str_or("")).unwrap_or("")).as_ptr());
                    connect(b, key);
                    (g.gtk_box_pack_start)(buttons, b, 0, 0, 0);
                }
                (g.gtk_box_pack_start)(col, buttons, 0, 0, 0);
            }
            (g.gtk_box_pack_start)(list, row, 0, 0, 0);
            ui.live.push(live);
        }
        (g.gtk_widget_show_all)(list);
    }
}

fn refresh(ui: &mut Ui, state: &Value) {
    let g = g();
    unsafe {
        (g.gtk_window_set_title)(ui.window, cs(state["title"].str_or("disc-launcher")).as_ptr());
    }
    let shape = shape_of(state);
    if shape != ui.shape {
        build(ui, state);
        ui.shape = shape;
    } else {
        for (it, live) in state["items"].as_arr().iter().zip(&ui.live) {
            unsafe {
                (g.gtk_label_set_markup)(live.title, cs(&title_markup(it)).as_ptr());
                if let Some(l) = live.lines {
                    (g.gtk_label_set_text)(l, cs(&lines_text(it)).as_ptr());
                }
            }
            if let Some(b) = live.usage {
                set_bar(b, &it["usage"]);
            }
            if let Some(b) = live.progress {
                set_bar(b, &it["progress"]);
            }
        }
    }
    let want = state["visible"].bool_or(false);
    unsafe {
        if want && !ui.visible {
            (g.gtk_widget_show_all)(ui.window);
            (g.gtk_window_present)(ui.window);
        } else if !want && ui.visible {
            (g.gtk_widget_hide)(ui.window);
        } else if want && state["raise"].bool_or(false) {
            (g.gtk_window_present)(ui.window);
        }
    }
    ui.visible = want;
}

type Inbox = Arc<Mutex<(Option<String>, bool)>>;
static INBOX: OnceLock<Inbox> = OnceLock::new();

unsafe extern "C" fn on_tick(_d: P) -> GBool {
    let (msg, closed) = {
        let mut b = INBOX.get().unwrap().lock().unwrap();
        (b.0.take(), b.1)
    };
    if let Some(m) = msg {
        if let Ok(state) = json::parse(&m) {
            UI.with(|u| {
                if let Some(ui) = u.borrow_mut().as_mut() {
                    refresh(ui, &state);
                }
            });
        }
    }
    if closed {
        (g().gtk_main_quit)();
        return 0;
    }
    1
}

fn main() {
    let check = std::env::args().any(|a| a == "--check");
    // Mourir avec le démon qui nous a lancé (arrêt brutal compris).
    disclauncher::sys::set_parent_death_signal(disclauncher::sys::SIGTERM);
    let gtk = match Gtk::load() {
        Ok(x) => x,
        Err(e) => {
            eprintln!("disc-launcher-panel : {e}");
            std::process::exit(3);
        }
    };
    let _ = GTK.set(gtk);
    let g = g();
    if unsafe { (g.gtk_init_check)(std::ptr::null_mut(), std::ptr::null_mut()) } == 0 {
        eprintln!("disc-launcher-panel : affichage indisponible");
        std::process::exit(3);
    }
    if check {
        return;
    }
    unsafe {
        let win = (g.gtk_window_new)(0);
        (g.gtk_window_set_title)(win, cs("disc-launcher").as_ptr());
        (g.gtk_window_set_icon_name)(win, cs("media-eject").as_ptr());
        (g.gtk_window_set_default_size)(win, 480, -1);
        (g.gtk_window_set_position)(win, WIN_POS_CENTER);
        (g.g_signal_connect_data)(win, cs("delete-event").as_ptr(), on_delete as *const () as P, std::ptr::null_mut(), std::ptr::null_mut(), 0);
        let sw = (g.gtk_scrolled_window_new)(std::ptr::null_mut(), std::ptr::null_mut());
        (g.gtk_scrolled_window_set_policy)(sw, POLICY_NEVER, POLICY_AUTOMATIC);
        (g.gtk_scrolled_window_set_propagate_natural_height)(sw, 1);
        (g.gtk_scrolled_window_set_max_content_height)(sw, 640);
        (g.gtk_container_add)(win, sw);
        let holder = (g.gtk_box_new)(VERTICAL, 0);
        (g.gtk_container_set_border_width)(holder, 4);
        (g.gtk_container_add)(sw, holder);
        let list = (g.gtk_box_new)(VERTICAL, 0);
        (g.gtk_box_pack_start)(holder, list, 1, 1, 0);
        (g.gtk_widget_set_can_focus)(holder, 0);
        UI.with(|u| *u.borrow_mut() = Some(Ui { window: win, holder, list, shape: "\u{0}".into(), live: vec![], visible: false }));
    }
    let inbox: Inbox = Arc::new(Mutex::new((None, false)));
    let _ = INBOX.set(inbox.clone());
    std::thread::spawn(move || {
        let stdin = std::io::stdin();
        for line in stdin.lock().lines() {
            match line {
                Ok(l) if !l.trim().is_empty() => inbox.lock().unwrap().0 = Some(l),
                Ok(_) => {}
                Err(_) => break,
            }
        }
        inbox.lock().unwrap().1 = true;
    });
    unsafe {
        (g.g_timeout_add)(100, on_tick as *const () as P, std::ptr::null_mut());
    }
    emit("ready");
    unsafe { (g.gtk_main)() };
}
