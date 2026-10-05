//! disc-launcher : détection et identification des disques optiques, prédiction
//! du fichier produit, proposition d'actions (lecteur multimédia, émulateurs, dump vers
//! `~/ROMs/<système>` façon ES-DE).
//!
//! Aucune dépendance externe : seulement la bibliothèque standard et des appels
//! système Linux déclarés dans `sys`.

pub mod json;
#[macro_use]
pub mod log;
pub mod config;
pub mod paths;
pub mod sys;
pub mod toml;
pub mod util;
pub mod xml;

pub mod device;
pub mod fs;
pub mod identify;
pub mod identity;

pub mod cart;
pub mod collection;
pub mod data;
pub mod filemanager;
pub mod generic;
pub mod handlers;
pub mod helper_profiles;
pub mod jobs;
pub mod media;
pub mod naming;
pub mod refdb;
pub mod retroarch;
pub mod sqlite;
pub mod usb;

pub mod control;
pub mod daemon;
pub mod dbus;
pub mod notify;
pub mod terminal;
pub mod tray;

pub const VERSION: &str = env!("CARGO_PKG_VERSION");
