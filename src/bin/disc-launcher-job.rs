//! Exécute une tâche : `disc-launcher-job [--detach] <id>`.
//! `--detach` : double fork + setsid, la tâche survit au démon.

use disclauncher::{jobs, sys};

fn main() {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let detach = args.iter().any(|a| a == "--detach");
    let Some(id) = args.iter().find(|a| !a.starts_with("--")).cloned() else {
        eprintln!("usage : disc-launcher-job [--detach] <id>");
        std::process::exit(2);
    };
    if detach {
        match sys::daemonize() {
            Ok(true) => {
                // petit-fils : stdio vers /dev/null
                if let Ok(null) = std::fs::OpenOptions::new().read(true).write(true).open("/dev/null") {
                    use std::os::fd::AsRawFd;
                    unsafe {
                        sys::dup2(null.as_raw_fd(), 0);
                        sys::dup2(null.as_raw_fd(), 1);
                        sys::dup2(null.as_raw_fd(), 2);
                    }
                }
            }
            Ok(false) => std::process::exit(0),
            Err(e) => {
                eprintln!("détachement impossible : {e}");
                std::process::exit(1);
            }
        }
    }
    std::process::exit(jobs::run(&id));
}
