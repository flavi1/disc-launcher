//! Démon de session. Démarré par XDG Autostart (`/etc/xdg/autostart/disc-launcherd.desktop`)
//! ou par un gestionnaire de services (exemples dans `contrib/`).
//!
//! Options : `--foreground` (journal copié sur stderr, pour runit/s6/systemd).

fn main() {
    let args: Vec<String> = std::env::args().skip(1).collect();
    if args.iter().any(|a| a == "--version") {
        println!("disc-launcherd {}", disclauncher::VERSION);
        return;
    }
    let fg = args.iter().any(|a| a == "--foreground" || a == "-f");
    std::process::exit(disclauncher::daemon::run(fg));
}
