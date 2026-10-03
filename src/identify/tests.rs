use super::*;
use crate::device::{MediaKind, MemSource, Physical, Track};
use crate::fs::iso9660::testutil::{build, mem_from_iso};

fn opts() -> Options {
    Options { signatures: Some(signatures::BUILTIN.iter().flat_map(|(_, s)| signatures::parse_file(s).unwrap()).collect()), ..Default::default() }
}

fn cd(tracks: Vec<Track>, sessions: u8) -> Physical {
    let leadout = tracks.iter().map(|t| t.start + t.length).max().unwrap_or(0);
    Physical { media: MediaKind::Cd, profile: 8, recordable: false, blank: false, sessions, tracks, leadout, disc_type: Some(0), capacity: leadout as u64 }
}

fn t(n: u8, session: u8, data: bool, start: u32, length: u32) -> Track {
    Track { number: n, session, data, start, length, mode: if data { Some(2) } else { None } }
}

fn primary_tag(r: &IdentResult) -> Option<String> {
    r.primary().map(|m| m.tag.clone())
}

#[test]
fn ps1_systemcnf() {
    let img = build("PLAYSTATION", "FF7_D1", &[("SYSTEM.CNF", b"BOOT = cdrom:\\SCES_008.67;1\r\nTCB = 4\r\nVMODE = PAL\r\n")]);
    let r = identify(&mem_from_iso(&img, MediaKind::Cd), &opts());
    let m = r.primary().unwrap();
    assert_eq!(m.tag, "console:psx");
    assert_eq!(m.identity.serial.as_deref(), Some("SCES-00867"));
    assert_eq!(m.identity.region.as_deref(), Some("Europe"));
    assert_eq!(m.identity.disc, Some(1));
    assert_eq!(r.target().as_deref(), Some("psx"));
}

#[test]
fn ps2_dvd() {
    let img = build("PLAYSTATION", "SLUS_200", &[("SYSTEM.CNF", b"BOOT2 = cdrom0:\\SLUS_200.62;1\r\nVER = 1.01\r\n")]);
    let r = identify(&mem_from_iso(&img, MediaKind::Dvd), &opts());
    let m = r.primary().unwrap();
    assert_eq!(m.tag, "console:ps2");
    assert_eq!(m.identity.serial.as_deref(), Some("SLUS-20062"));
    assert_eq!(m.identity.region.as_deref(), Some("USA"));
    assert_eq!(m.identity.revision.as_deref(), Some("1.01"));
}

#[test]
fn ps1_mixed_mode_game_is_console_not_audio() {
    let img = build("PLAYSTATION", "WIPEOUT", &[("SYSTEM.CNF", b"BOOT = cdrom:\\SLES_000.10;1\r\n")]);
    let mut m = mem_from_iso(&img, MediaKind::Cd);
    m.phys = cd(vec![t(1, 1, true, 0, 1000), t(2, 1, false, 1000, 20000), t(3, 1, false, 21000, 20000)], 1);
    let r = identify(&m, &opts());
    assert_eq!(primary_tag(&r).as_deref(), Some("console:psx"));
    assert!(r.matches.iter().any(|x| x.tag == "audio:cdda-mixed" && !x.primary));
}

#[test]
fn audio_cd_and_cd_extra() {
    let m = MemSource::new(cd(vec![t(1, 1, false, 0, 20000), t(2, 1, false, 20000, 20000)], 1));
    let r = identify(&m, &opts());
    assert_eq!(primary_tag(&r).as_deref(), Some("audio:cdda"));
    assert_eq!(r.target().as_deref(), Some("cdda"));

    let img = build("", "EXTRA", &[("README.TXT", b"bonus")]);
    let mut m = MemSource::new(cd(vec![t(1, 1, false, 0, 20000), t(2, 1, false, 20000, 20000), t(3, 2, true, 51400, 64)], 2));
    m.put(51400, 0, &img);
    let r = identify(&m, &opts());
    assert_eq!(primary_tag(&r).as_deref(), Some("audio:cdda"));
    assert_eq!(r.primary().unwrap().source, "toc-cd-extra");
}

#[test]
fn pc_mixed_mode_without_signature_has_no_action() {
    let img = build("", "SOMEGAME", &[("GAME.EXE", b"MZ")]);
    let mut m = mem_from_iso(&img, MediaKind::Cd);
    m.phys = cd(vec![t(1, 1, true, 0, 1000), t(2, 1, false, 1000, 20000)], 1);
    let r = identify(&m, &opts());
    assert_eq!(r.primary(), None);
    assert_eq!(r.target(), None);
}

#[test]
fn saturn_header() {
    let mut m = MemSource::new(cd(vec![t(1, 1, true, 0, 1000), t(2, 1, false, 1000, 5000)], 1));
    let mut hdr = vec![b' '; 256];
    hdr[..16].copy_from_slice(b"SEGA SEGASATURN ");
    hdr[0x20..0x2A].copy_from_slice(b"MK-81019  ");
    hdr[0x2A..0x30].copy_from_slice(b"V1.003");
    hdr[0x38..0x40].copy_from_slice(b"CD-1/2  ");
    hdr[0x40..0x4A].copy_from_slice(b"JTUBKAEL  ");
    hdr[0x60..0x6F].copy_from_slice(b"PANZER DRAGOON ");
    m.put(0, 0, &hdr);
    let r = identify(&m, &opts());
    let p = r.primary().unwrap();
    assert_eq!(p.tag, "console:saturn");
    assert_eq!(p.identity.serial.as_deref(), Some("MK-81019"));
    assert_eq!(p.identity.disc, Some(1));
    assert_eq!(p.identity.discs_total, Some(2));
    assert_eq!(p.identity.region.as_deref(), Some("World"));
    assert_eq!(p.identity.title.as_deref(), Some("PANZER DRAGOON"));
    assert_eq!(p.identity.revision.as_deref(), Some("1.003"));
}

#[test]
fn megacd_header() {
    let mut m = MemSource::new(cd(vec![t(1, 1, true, 0, 1000)], 1));
    let mut hdr = vec![b' '; 0x200];
    hdr[..14].copy_from_slice(b"SEGADISCSYSTEM");
    hdr[0x150..0x15A].copy_from_slice(b"SONIC CD  ");
    hdr[0x180..0x18E].copy_from_slice(b"GM MK-4407 -00");
    hdr[0x1F0..0x1F3].copy_from_slice(b"J  ");
    m.put(0, 0, &hdr);
    let r = identify(&m, &opts());
    let p = r.primary().unwrap();
    assert_eq!(p.tag, "console:megacd");
    assert_eq!(p.identity.serial.as_deref(), Some("MK-4407"));
    assert_eq!(p.identity.region.as_deref(), Some("Japan"));
}

fn dvd(sectors: u32) -> Physical {
    Physical { media: MediaKind::Dvd, profile: 0x10, recordable: false, blank: false, sessions: 1, tracks: vec![t(1, 1, true, 0, sectors)], leadout: sectors, disc_type: None, capacity: sectors as u64 }
}

#[test]
fn gamecube_and_wii() {
    let mut m = MemSource::new(dvd(712_880));
    let mut h = vec![0u8; 0x60];
    h[..6].copy_from_slice(b"GALP01");
    h[6] = 0;
    h[7] = 2;
    h[0x1C..0x20].copy_from_slice(&[0xC2, 0x33, 0x9F, 0x3D]);
    h[0x20..0x36].copy_from_slice(b"Super Smash Bros Melee");
    m.put(0, 0, &h);
    let r = identify(&m, &opts());
    let p = r.primary().unwrap();
    assert_eq!(p.tag, "console:gc");
    assert_eq!(p.identity.game_id.as_deref(), Some("GALP01"));
    assert_eq!(p.identity.region.as_deref(), Some("Europe"));
    assert_eq!(p.identity.revision.as_deref(), Some("Rev 2"));
    // Lecteur standard : lecture et dump impossibles annoncés.
    assert!(!r.feasibility.dump);
    assert!(r.warnings.iter().any(|w| w == "drive-incompatible"));

    let mut w = MemSource::new(dvd(2_294_912));
    let mut h = vec![0u8; 0x60];
    h[..6].copy_from_slice(b"RMGE01");
    h[0x18..0x1C].copy_from_slice(&[0x5D, 0x1C, 0x9E, 0xA3]);
    w.put(0, 0, &h);
    let r = identify(&w, &Options { profile: "omnidrive".into(), ..opts() });
    assert_eq!(primary_tag(&r).as_deref(), Some("console:wii"));
    assert!(r.feasibility.dump);
}

#[test]
fn dvd_video_and_xbox_partition() {
    // Vrai DVD-Video : volume de plusieurs Go (simulé par la taille du volume).
    let mut img = build("", "MOVIE", &[("VIDEO_TS/VIDEO_TS.IFO", b"DVDVIDEO-VMG")]);
    // volume_space = 2 000 000 secteurs
    img[16 * 2048 + 80..16 * 2048 + 84].copy_from_slice(&2_000_000u32.to_le_bytes());
    let mut m = mem_from_iso(&img, MediaKind::Dvd);
    m.phys.capacity = 2_000_000;
    let r = identify(&m, &opts());
    assert_eq!(primary_tag(&r).as_deref(), Some("video:dvd"));

    // Partition vidéo Xbox vue par un lecteur standard : minuscule.
    let img = build("", "XBOX", &[("VIDEO_TS/VIDEO_TS.IFO", b"DVDVIDEO-VMG")]);
    let m = mem_from_iso(&img, MediaKind::Dvd);
    let r = identify(&m, &Options { profile: "standard".into(), ..opts() });
    assert_eq!(primary_tag(&r).as_deref(), Some("console:xbox"));
    assert_eq!(r.target().as_deref(), Some("xbox"));
    assert!(r.warnings.iter().any(|w| w == "xbox-video-partition-probable"));
}

#[test]
fn xbox_xdvdfs() {
    let mut m = MemSource::new(dvd(3_820_880));
    let base = 198_144u32;
    let mut vd = vec![0u8; 2048];
    vd[..20].copy_from_slice(b"MICROSOFT*XBOX*MEDIA");
    vd[20..24].copy_from_slice(&40u32.to_le_bytes()); // secteur du dossier racine
    vd[24..28].copy_from_slice(&2048u32.to_le_bytes());
    vd[0x7EC..0x7EC + 20].copy_from_slice(b"MICROSOFT*XBOX*MEDIA");
    m.put(base + 32, 0, &vd);
    // dossier racine : une entrée default.xbe au secteur 50
    let mut dir = vec![0xFFu8; 2048];
    let name = b"default.xbe";
    let mut e = vec![0u8; 14];
    e[4..8].copy_from_slice(&50u32.to_le_bytes());
    e[8..12].copy_from_slice(&4096u32.to_le_bytes());
    e[12] = 0x20;
    e[13] = name.len() as u8;
    e.extend_from_slice(name);
    dir[..e.len()].copy_from_slice(&e);
    m.put(base + 40, 0, &dir);
    // XBE : base 0x10000, certificat à 0x10000 + 0x200
    let mut xbe = vec![0u8; 4096];
    xbe[..4].copy_from_slice(b"XBEH");
    xbe[0x104..0x108].copy_from_slice(&0x10000u32.to_le_bytes());
    xbe[0x118..0x11C].copy_from_slice(&0x10200u32.to_le_bytes());
    let cert = 0x200;
    xbe[cert + 8..cert + 12].copy_from_slice(&0x4D530004u32.to_le_bytes());
    for (k, u) in "Halo".encode_utf16().enumerate() {
        xbe[cert + 0xC + k * 2..cert + 0xC + k * 2 + 2].copy_from_slice(&u.to_le_bytes());
    }
    xbe[cert + 0xA0..cert + 0xA4].copy_from_slice(&1u32.to_le_bytes());
    m.put(base + 50, 0, &xbe);
    let r = identify(&m, &Options { profile: "omnidrive".into(), ..opts() });
    let p = r.primary().unwrap();
    assert_eq!(p.tag, "console:xbox");
    assert_eq!(p.identity.serial.as_deref(), Some("MS-004"));
    assert_eq!(p.identity.title.as_deref(), Some("Halo"));
    assert_eq!(p.identity.region.as_deref(), Some("USA"));
}

#[test]
fn jaguar_cd() {
    let mut m = MemSource::new(cd(vec![t(1, 1, false, 0, 5000), t(2, 2, false, 16400, 30000), t(3, 2, false, 46400, 30000)], 2));
    // en-tête dans les données audio, octets permutés
    let hdr: Vec<u8> = super::probes::tests_swap(b"ATARI APPROVED DATA HEADER ATRI ");
    m.put_raw(16400 + 3, 100, &hdr);
    let r = identify(&m, &opts());
    assert_eq!(primary_tag(&r).as_deref(), Some("console:atarijaguarcd"));
}

#[test]
fn vcd_is_video_not_cdi() {
    let mut img = build("CD-RTOS CD-BRIDGE", "VIDEOCD", &[("MPEGAV/AVSEQ01.DAT", b"RIFF"), ("VCD/INFO.VCD", b"VIDEO_CD")]);
    img[16 * 2048 + 8..16 * 2048 + 15].copy_from_slice(b"CD-RTOS");
    let r = identify(&mem_from_iso(&img, MediaKind::Cd), &opts());
    assert_eq!(primary_tag(&r).as_deref(), Some("video:vcd"));
}

#[test]
fn pce_audio_first() {
    let mut m = MemSource::new(cd(vec![t(1, 1, false, 0, 3000), t(2, 1, true, 3000, 50000), t(3, 1, false, 53000, 1000)], 1));
    let mut boot = vec![0u8; 0x40];
    boot[0x20..0x37].copy_from_slice(b"PC Engine CD-ROM SYSTEM");
    m.put(3001, 0, &boot);
    let r = identify(&m, &opts());
    assert_eq!(primary_tag(&r).as_deref(), Some("console:pcenginecd"));
    assert!(r.primary().unwrap().identity.toc_fp.is_some());
}

#[test]
fn blank_disc() {
    let mut p = cd(vec![], 1);
    p.blank = true;
    let r = identify(&MemSource::new(p), &opts());
    assert_eq!(primary_tag(&r).as_deref(), Some("blank"));
    assert_eq!(r.target(), None);
}

#[test]
fn result_roundtrip() {
    let img = build("PLAYSTATION", "X", &[("SYSTEM.CNF", b"BOOT = cdrom:\\SLUS_005.94;1\r\n")]);
    let r = identify(&mem_from_iso(&img, MediaKind::Cd), &opts());
    let v = crate::json::parse(&r.to_value().to_json()).unwrap();
    let ms = IdentResult::matches_from_value(&v);
    assert_eq!(ms[0].identity.serial.as_deref(), Some("SLUS-00594"));
    assert!(ms[0].primary);
}

#[test]
fn cue_bin_image_redump_style() {
    // Image Redump : une piste de données Mode 2 + une piste audio, un .bin par piste.
    let d = std::env::temp_dir().join(format!("dl-cue-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&d);
    std::fs::create_dir_all(&d).unwrap();
    let iso = build("PLAYSTATION", "GAME", &[("SYSTEM.CNF", b"BOOT = cdrom:\\SLUS_005.94;1\r\n")]);
    let mut t1 = vec![];
    for s in iso.chunks(2048) {
        let mut raw = vec![0u8; 2352];
        raw[1..11].fill(0xff);
        raw[15] = 2;
        raw[24..24 + 2048].copy_from_slice(s);
        t1.extend_from_slice(&raw);
    }
    std::fs::write(d.join("Game (Track 1).bin"), &t1).unwrap();
    std::fs::write(d.join("Game (Track 2).bin"), vec![0u8; 2352 * 300]).unwrap();
    std::fs::write(
        d.join("Game.cue"),
        "FILE \"Game (Track 1).bin\" BINARY\n  TRACK 01 MODE2/2352\n    INDEX 01 00:00:00\nFILE \"Game (Track 2).bin\" BINARY\n  TRACK 02 AUDIO\n    INDEX 00 00:00:00\n    INDEX 01 00:02:00\n",
    )
    .unwrap();
    let r = identify_image(&d.join("Game.cue"), &opts()).unwrap();
    assert_eq!(r.physical.tracks.len(), 2);
    assert_eq!(r.physical.tracks[1].start, (iso.len() / 2048) as u32 + 150);
    assert_eq!(primary_tag(&r).as_deref(), Some("console:psx"));
    assert_eq!(r.primary().unwrap().identity.serial.as_deref(), Some("SLUS-00594"));
    let _ = std::fs::remove_dir_all(&d);
}

#[test]
fn data_discs_music_video_dcim() {
    let img = build("", "MUSIQUE", &[("ALBUM/01.FLAC", b"x"), ("ALBUM/02.FLAC", b"x"), ("ALBUM/COVER.JPG", b"x"), ("AUTORUN.INF", b"[autorun]")]);
    let r = identify(&mem_from_iso(&img, MediaKind::Cd), &opts());
    assert_eq!(primary_tag(&r).as_deref(), Some("data:audio"));
    assert_eq!(r.target().as_deref(), Some("data-audio"));

    let img = build("", "FILM", &[("FILM.MKV", b"x"), ("FILM.SRT", b"x")]);
    let r = identify(&mem_from_iso(&img, MediaKind::Dvd), &opts());
    assert_eq!(primary_tag(&r).as_deref(), Some("data:video"));

    let img = build("", "PHOTOS", &[("DCIM/IMG_0001.JPG", b"x")]);
    let r = identify(&mem_from_iso(&img, MediaKind::Cd), &opts());
    assert_eq!(primary_tag(&r).as_deref(), Some("photo:dcim"));
    assert_eq!(r.target().as_deref(), Some("dcim"));

    // Programme d'installation et vidéo : ni « vidéo seule » ni action par défaut.
    let img = build("", "JEU", &[("SETUP.EXE", b"MZ"), ("INTRO.AVI", b"x"), ("AUTORUN.INF", b"x")]);
    let r = identify(&mem_from_iso(&img, MediaKind::Cd), &opts());
    assert!(!r.matches.iter().any(|m| m.tag.starts_with("data:")));
    assert!(r.matches.iter().any(|m| m.tag == "console:windows"));
}
