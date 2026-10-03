//! Gestionnaire générique, piloté par le manifeste du système.
//! Installé sous les noms `disc-launcher-<id>` (liens symboliques) : l'identifiant
//! vient du nom d'appel, ou de `--id <id>`.
//!
//! Verbes : describe | play [--existing <chemin> | --disc <périphérique>]
//!          | dump-plan | convert-plan --input <f> --output <f> | post-dump
//! Entrée : document JSON sur stdin (facultatif hors démon). Sortie : JSON.

use disclauncher::config::Config;
use disclauncher::handlers::{self, RC_NOT_APPLICABLE, RC_OK, RC_USER_ERROR};
use disclauncher::json::{self, Value};
use disclauncher::{generic, jobj, sys};
use std::io::Read;

fn out(v: &Value, code: i32) -> ! {
    println!("{}", v.to_json());
    std::process::exit(code);
}

fn fail(code: i32, msg: &str) -> ! {
    eprintln!("{msg}");
    out(&jobj! {"error" => msg}, code);
}

fn main() {
    let args: Vec<String> = std::env::args().collect();
    let argv0 = std::path::Path::new(&args[0]).file_name().map(|s| s.to_string_lossy().into_owned()).unwrap_or_default();
    let mut id = argv0.strip_prefix("disc-launcher-").filter(|s| *s != "generic").map(|s| s.to_string());
    let mut verb = None;
    let mut existing = None;
    let mut disc = None;
    let mut input_path = None;
    let mut output_path = None;
    let mut i = 1;
    while i < args.len() {
        let next = || args.get(i + 1).cloned();
        match args[i].as_str() {
            "--id" => {
                id = next();
                i += 1;
            }
            "--existing" => {
                existing = next();
                i += 1;
            }
            "--disc" => {
                disc = next();
                i += 1;
            }
            "--input" => {
                input_path = next();
                i += 1;
            }
            "--output" => {
                output_path = next();
                i += 1;
            }
            v if verb.is_none() && !v.starts_with("--") => verb = Some(v.to_string()),
            _ => {}
        }
        i += 1;
    }
    let id = id.or_else(|| std::env::var("DL_SYSTEM").ok());
    let Some(id) = id else { fail(RC_USER_ERROR, "identifiant du gestionnaire inconnu : utilisez --id ou DL_SYSTEM") };
    let Some(m) = handlers::get(&id) else { fail(RC_NOT_APPLICABLE, &format!("aucun manifeste pour « {id} »")) };
    let cfg = Config::load();
    let input = if !sys::stdin_ready(300) {
        Value::obj()
    } else {
        let mut s = String::new();
        let _ = std::io::stdin().read_to_string(&mut s);
        if s.trim().is_empty() {
            Value::obj()
        } else {
            json::parse(&s).unwrap_or_else(|e| fail(RC_USER_ERROR, &e.to_string()))
        }
    };
    match verb.as_deref().unwrap_or("describe") {
        "describe" => out(&generic::describe(&m, &cfg, &input), RC_OK),
        "play" => {
            let existing = existing.or_else(|| input["existing"].string()).or_else(|| std::env::var("DL_EXISTING").ok());
            let disc = disc.or_else(|| input["device"].string()).or_else(|| std::env::var("DL_DEVICE").ok());
            match generic::play(&m, &cfg, existing.as_deref(), if existing.is_some() { None } else { disc.as_deref() }) {
                Ok(v) => out(&v, RC_OK),
                Err(e) => fail(e.code, &e.message),
            }
        }
        "dump-plan" => match generic::dump_plan(&m, &cfg, &input) {
            Ok(v) => out(&v, RC_OK),
            Err(e) => fail(e.code, &e.message),
        },
        "convert-plan" => {
            let inp = input_path.or_else(|| input["input"].string()).unwrap_or_else(|| fail(RC_USER_ERROR, "--input attendu"));
            let outp = output_path.or_else(|| input["output"].string()).unwrap_or_else(|| fail(RC_USER_ERROR, "--output attendu"));
            match generic::convert_plan(&m, &inp, &outp) {
                Ok(v) => out(&v, RC_OK),
                Err(e) => fail(e.code, &e.message),
            }
        }
        "post-dump" => out(&jobj! {"files" => Vec::<Value>::new()}, RC_OK),
        v => fail(RC_USER_ERROR, &format!("verbe inconnu : {v}")),
    }
}
