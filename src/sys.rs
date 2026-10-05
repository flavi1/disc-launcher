//! Appels système Linux déclarés à la main (aucune dépendance externe).
//!
//! Cible : Linux 64 bits (x86_64, aarch64, riscv64). Les structures sont
//! déclarées avec la disposition de l'ABI 64 bits de glibc et musl.

#![allow(non_camel_case_types, dead_code)]

use std::ffi::{c_char, c_int, c_long, c_ulong, c_void, CString};
use std::io;
use std::os::fd::RawFd;
use std::path::Path;

pub type pid_t = i32;
pub type uid_t = u32;
pub type gid_t = u32;

pub const O_NONBLOCK: i32 = 0o4000;
pub const LOCK_SH: c_int = 1;
pub const LOCK_EX: c_int = 2;
pub const LOCK_NB: c_int = 4;
pub const LOCK_UN: c_int = 8;

pub const SIGHUP: c_int = 1;
pub const SIGKILL: c_int = 9;
pub const SIGTERM: c_int = 15;

pub const AF_UNIX: c_int = 1;
pub const AF_NETLINK: c_int = 16;
pub const SOCK_DGRAM: c_int = 2;
pub const SOCK_CLOEXEC: c_int = 0o2000000;
pub const NETLINK_KOBJECT_UEVENT: c_int = 15;
pub const SOL_SOCKET: c_int = 1;
pub const SO_PEERCRED: c_int = 17;
pub const SCM_RIGHTS: c_int = 1;

pub const PR_SET_KEEPCAPS: c_int = 8;
pub const PR_CAP_AMBIENT: c_int = 47;
pub const PR_CAP_AMBIENT_RAISE: c_ulong = 2;
pub const CAP_SYS_RAWIO: u32 = 17;

pub const W_OK: c_int = 2;
pub const R_OK: c_int = 4;

// Ioctl CD-ROM (linux/cdrom.h)
pub const CDROMEJECT: c_ulong = 0x5309;
pub const CDROMCLOSETRAY: c_ulong = 0x5319;
pub const CDROM_MEDIA_CHANGED: c_ulong = 0x5325;
pub const CDROM_DRIVE_STATUS: c_ulong = 0x5326;
pub const CDROM_DISC_STATUS: c_ulong = 0x5327;
pub const CDROM_LOCKDOOR: c_ulong = 0x5329;
pub const CDSL_CURRENT: c_ulong = i32::MAX as c_ulong;
pub const CDS_NO_INFO: c_int = 0;
pub const CDS_NO_DISC: c_int = 1;
pub const CDS_TRAY_OPEN: c_int = 2;
pub const CDS_DRIVE_NOT_READY: c_int = 3;
pub const CDS_DISC_OK: c_int = 4;
// SG_IO (scsi/sg.h)
pub const SG_IO: c_ulong = 0x2285;
pub const SG_DXFER_NONE: c_int = -1;
pub const SG_DXFER_FROM_DEV: c_int = -3;

#[cfg(target_arch = "x86_64")]
pub const SYS_CAPSET: c_long = 126;
#[cfg(any(target_arch = "aarch64", target_arch = "riscv64", target_arch = "loongarch64"))]
pub const SYS_CAPSET: c_long = 91;

#[repr(C)]
pub struct SgIoHdr {
    pub interface_id: c_int,
    pub dxfer_direction: c_int,
    pub cmd_len: u8,
    pub mx_sb_len: u8,
    pub iovec_count: u16,
    pub dxfer_len: u32,
    pub dxferp: *mut c_void,
    pub cmdp: *const u8,
    pub sbp: *mut u8,
    pub timeout: u32,
    pub flags: u32,
    pub pack_id: c_int,
    pub usr_ptr: *mut c_void,
    pub status: u8,
    pub masked_status: u8,
    pub msg_status: u8,
    pub sb_len_wr: u8,
    pub host_status: u16,
    pub driver_status: u16,
    pub resid: c_int,
    pub duration: u32,
    pub info: u32,
}

#[repr(C)]
pub struct PollFd {
    pub fd: c_int,
    pub events: i16,
    pub revents: i16,
}

#[repr(C)]
pub struct SockaddrNl {
    pub nl_family: u16,
    pub nl_pad: u16,
    pub nl_pid: u32,
    pub nl_groups: u32,
}

#[repr(C)]
#[derive(Default, Clone, Copy)]
pub struct Ucred {
    pub pid: pid_t,
    pub uid: uid_t,
    pub gid: gid_t,
}

#[repr(C)]
pub struct Iovec {
    pub iov_base: *mut c_void,
    pub iov_len: usize,
}

#[repr(C)]
pub struct Msghdr {
    pub msg_name: *mut c_void,
    pub msg_namelen: u32,
    pub msg_iov: *mut Iovec,
    pub msg_iovlen: usize,
    pub msg_control: *mut c_void,
    pub msg_controllen: usize,
    pub msg_flags: c_int,
}

#[repr(C)]
pub struct Cmsghdr {
    pub cmsg_len: usize,
    pub cmsg_level: c_int,
    pub cmsg_type: c_int,
}

#[repr(C)]
#[derive(Default)]
pub struct Tm {
    pub tm_sec: c_int,
    pub tm_min: c_int,
    pub tm_hour: c_int,
    pub tm_mday: c_int,
    pub tm_mon: c_int,
    pub tm_year: c_int,
    pub tm_wday: c_int,
    pub tm_yday: c_int,
    pub tm_isdst: c_int,
    pub tm_gmtoff: c_long,
    pub tm_zone: usize,
}

#[repr(C)]
pub struct CapHeader {
    pub version: u32,
    pub pid: c_int,
}
#[repr(C)]
#[derive(Default, Clone, Copy)]
pub struct CapData {
    pub effective: u32,
    pub permitted: u32,
    pub inheritable: u32,
}

extern "C" {
    pub fn ioctl(fd: c_int, req: c_ulong, ...) -> c_int;
    pub fn flock(fd: c_int, op: c_int) -> c_int;
    pub fn fork() -> pid_t;
    pub fn setsid() -> pid_t;
    pub fn getuid() -> uid_t;
    pub fn geteuid() -> uid_t;
    pub fn getgid() -> gid_t;
    pub fn getpid() -> pid_t;
    pub fn kill(pid: pid_t, sig: c_int) -> c_int;
    pub fn statvfs(path: *const c_char, buf: *mut c_void) -> c_int;
    pub fn getsockopt(fd: c_int, level: c_int, name: c_int, val: *mut c_void, len: *mut u32) -> c_int;
    pub fn socket(domain: c_int, ty: c_int, proto: c_int) -> c_int;
    pub fn bind(fd: c_int, addr: *const c_void, len: u32) -> c_int;
    pub fn recv(fd: c_int, buf: *mut c_void, len: usize, flags: c_int) -> isize;
    pub fn recvmsg(fd: c_int, msg: *mut Msghdr, flags: c_int) -> isize;
    pub fn close(fd: c_int) -> c_int;
    pub fn prctl(opt: c_int, a2: c_ulong, a3: c_ulong, a4: c_ulong, a5: c_ulong) -> c_int;
    pub fn setresuid(r: uid_t, e: uid_t, s: uid_t) -> c_int;
    pub fn setresgid(r: gid_t, e: gid_t, s: gid_t) -> c_int;
    pub fn setgroups(n: usize, list: *const gid_t) -> c_int;
    pub fn initgroups(user: *const std::ffi::c_char, group: gid_t) -> c_int;
    pub fn syscall(num: c_long, ...) -> c_long;
    pub fn localtime_r(t: *const i64, tm: *mut Tm) -> *mut Tm;
    pub fn access(path: *const c_char, mode: c_int) -> c_int;
    pub fn setpgid(pid: pid_t, pgid: pid_t) -> c_int;
    pub fn waitpid(pid: pid_t, status: *mut c_int, opts: c_int) -> pid_t;
    pub fn _exit(code: c_int) -> !;
    pub fn dup2(old: c_int, new: c_int) -> c_int;
    pub fn umask(mask: u32) -> u32;
    pub fn killpg(pgrp: pid_t, sig: c_int) -> c_int;
    pub fn isatty(fd: c_int) -> c_int;
    pub fn poll(fds: *mut PollFd, n: c_ulong, timeout: c_int) -> c_int;
    fn read(fd: c_int, buf: *mut std::ffi::c_void, n: usize) -> isize;
    fn tcgetattr(fd: c_int, t: *mut Termios) -> c_int;
    fn tcsetattr(fd: c_int, act: c_int, t: *const Termios) -> c_int;
    pub fn signal(sig: c_int, handler: extern "C" fn(c_int)) -> usize;
}

static TERM_FLAG: std::sync::atomic::AtomicBool = std::sync::atomic::AtomicBool::new(false);
static HUP_FLAG: std::sync::atomic::AtomicBool = std::sync::atomic::AtomicBool::new(false);

extern "C" fn on_signal(sig: c_int) {
    if sig == SIGHUP {
        HUP_FLAG.store(true, std::sync::atomic::Ordering::SeqCst);
    } else {
        TERM_FLAG.store(true, std::sync::atomic::Ordering::SeqCst);
    }
}

/// Installe des gestionnaires SIGTERM/SIGINT/SIGHUP qui lèvent des drapeaux.
pub fn install_signal_flags() {
    unsafe {
        signal(SIGTERM, on_signal);
        signal(2 /* SIGINT */, on_signal);
        signal(SIGHUP, on_signal);
    }
}
pub fn term_requested() -> bool {
    TERM_FLAG.load(std::sync::atomic::Ordering::SeqCst)
}
/// Renvoie vrai une fois par SIGHUP reçu.
pub fn take_hup() -> bool {
    HUP_FLAG.swap(false, std::sync::atomic::Ordering::SeqCst)
}

pub fn last_err() -> io::Error {
    io::Error::last_os_error()
}

fn cpath(p: &Path) -> io::Result<CString> {
    use std::os::unix::ffi::OsStrExt;
    CString::new(p.as_os_str().as_bytes()).map_err(|_| io::Error::new(io::ErrorKind::InvalidInput, "NUL dans le chemin"))
}

/// Verrou `flock` non bloquant. Renvoie Ok(false) si déjà tenu ailleurs.
pub fn try_lock(fd: RawFd, exclusive: bool) -> io::Result<bool> {
    let op = if exclusive { LOCK_EX } else { LOCK_SH } | LOCK_NB;
    let r = unsafe { flock(fd, op) };
    if r == 0 {
        Ok(true)
    } else {
        let e = last_err();
        if e.raw_os_error() == Some(11) {
            Ok(false)
        } else {
            Err(e)
        }
    }
}

pub fn unlock(fd: RawFd) {
    unsafe {
        flock(fd, LOCK_UN);
    }
}

pub fn uid() -> u32 {
    unsafe { getuid() }
}
pub fn euid() -> u32 {
    unsafe { geteuid() }
}
pub fn pid() -> i32 {
    unsafe { getpid() }
}

/// Le processus `pid` existe-t-il ?
pub fn pid_alive(pid: i32) -> bool {
    pid > 0 && (unsafe { kill(pid, 0) } == 0 || last_err().raw_os_error() == Some(1))
}

pub fn send_signal(pid: i32, sig: c_int) -> io::Result<()> {
    if unsafe { kill(pid, sig) } == 0 {
        Ok(())
    } else {
        Err(last_err())
    }
}

/// Espace libre (octets) disponible pour un utilisateur non privilégié.
pub fn free_space(path: &Path) -> io::Result<u64> {
    let c = cpath(path)?;
    // struct statvfs 64 bits : f_bsize, f_frsize, f_blocks, f_bfree, f_bavail, ...
    let mut buf = [0u64; 16];
    if unsafe { statvfs(c.as_ptr(), buf.as_mut_ptr() as *mut c_void) } != 0 {
        return Err(last_err());
    }
    let frsize = if buf[1] != 0 { buf[1] } else { buf[0] };
    Ok(buf[4].saturating_mul(frsize))
}

/// (taille totale, espace disponible) du système de fichiers de `path`.
pub fn fs_usage(path: &Path) -> io::Result<(u64, u64)> {
    let c = cpath(path)?;
    let mut buf = [0u64; 16];
    if unsafe { statvfs(c.as_ptr(), buf.as_mut_ptr() as *mut c_void) } != 0 {
        return Err(last_err());
    }
    let frsize = if buf[1] != 0 { buf[1] } else { buf[0] };
    Ok((buf[2].saturating_mul(frsize), buf[4].saturating_mul(frsize)))
}

pub fn can_access(path: &Path, mode: c_int) -> bool {
    match cpath(path) {
        Ok(c) => unsafe { access(c.as_ptr(), mode) == 0 },
        Err(_) => false,
    }
}

pub fn peer_cred(fd: RawFd) -> io::Result<Ucred> {
    let mut cred = Ucred::default();
    let mut len = std::mem::size_of::<Ucred>() as u32;
    let r = unsafe { getsockopt(fd, SOL_SOCKET, SO_PEERCRED, &mut cred as *mut Ucred as *mut c_void, &mut len) };
    if r == 0 {
        Ok(cred)
    } else {
        Err(last_err())
    }
}

/// Heure locale décomposée + décalage UTC en secondes.
pub fn local_time(secs: i64) -> (Tm, i64) {
    let mut tm = Tm::default();
    unsafe {
        localtime_r(&secs, &mut tm);
    }
    let off = tm.tm_gmtoff as i64;
    (tm, off)
}

/// Détache le processus courant : double fork + setsid.
/// Renvoie `true` dans le petit-fils (qui continue), `false` dans le parent d'origine.
pub fn daemonize() -> io::Result<bool> {
    unsafe {
        let p = fork();
        if p < 0 {
            return Err(last_err());
        }
        if p > 0 {
            let mut st = 0;
            waitpid(p, &mut st, 0);
            return Ok(false);
        }
        setsid();
        let p2 = fork();
        if p2 < 0 {
            _exit(1);
        }
        if p2 > 0 {
            _exit(0);
        }
        Ok(true)
    }
}

/// Socket netlink abonné aux uevents du noyau (groupe 1).
pub fn uevent_socket() -> io::Result<RawFd> {
    unsafe {
        let fd = socket(AF_NETLINK, SOCK_DGRAM | SOCK_CLOEXEC, NETLINK_KOBJECT_UEVENT);
        if fd < 0 {
            return Err(last_err());
        }
        let addr = SockaddrNl { nl_family: AF_NETLINK as u16, nl_pad: 0, nl_pid: 0, nl_groups: 1 };
        if bind(fd, &addr as *const SockaddrNl as *const c_void, std::mem::size_of::<SockaddrNl>() as u32) != 0 {
            let e = last_err();
            close(fd);
            return Err(e);
        }
        Ok(fd)
    }
}

pub fn recv_bytes(fd: RawFd, buf: &mut [u8]) -> io::Result<usize> {
    let n = unsafe { recv(fd, buf.as_mut_ptr() as *mut c_void, buf.len(), 0) };
    if n < 0 {
        Err(last_err())
    } else {
        Ok(n as usize)
    }
}

/// recvmsg avec récupération des descripteurs passés en SCM_RIGHTS.
pub fn recv_with_fds(fd: RawFd, buf: &mut [u8], fds: &mut Vec<RawFd>) -> io::Result<usize> {
    let mut iov = Iovec { iov_base: buf.as_mut_ptr() as *mut c_void, iov_len: buf.len() };
    let mut ctrl = [0u64; 32];
    let mut msg = Msghdr {
        msg_name: std::ptr::null_mut(),
        msg_namelen: 0,
        msg_iov: &mut iov,
        msg_iovlen: 1,
        msg_control: ctrl.as_mut_ptr() as *mut c_void,
        msg_controllen: std::mem::size_of_val(&ctrl),
        msg_flags: 0,
    };
    let n = unsafe { recvmsg(fd, &mut msg, 0x40000000 /* MSG_CMSG_CLOEXEC */) };
    if n < 0 {
        return Err(last_err());
    }
    // Parcours des cmsg
    let base = ctrl.as_ptr() as *const u8;
    let total = msg.msg_controllen;
    let hdr = std::mem::size_of::<Cmsghdr>();
    let mut off = 0usize;
    while off + hdr <= total {
        let c = unsafe { &*(base.add(off) as *const Cmsghdr) };
        if c.cmsg_len < hdr {
            break;
        }
        if c.cmsg_level == SOL_SOCKET && c.cmsg_type == SCM_RIGHTS {
            let data = unsafe { base.add(off + hdr) } as *const i32;
            let count = (c.cmsg_len - hdr) / 4;
            for i in 0..count {
                fds.push(unsafe { *data.add(i) });
            }
        }
        off += (c.cmsg_len + 7) & !7;
    }
    Ok(n as usize)
}

pub fn close_fd(fd: RawFd) {
    unsafe {
        close(fd);
    }
}

/// Abandonne root pour `uid`/`gid` en conservant uniquement CAP_SYS_RAWIO en
/// capacité ambiante (héritée par execve).
/// `user` : nom de l'utilisateur, pour reprendre ses groupes supplémentaires
/// (cdrom, optical… : accès au lecteur sans ACL de session).
pub fn drop_to_user_keep_rawio(uid: u32, gid: u32, user: Option<&str>) -> io::Result<()> {
    let cuser = user.and_then(|u| std::ffi::CString::new(u).ok());
    unsafe {
        if prctl(PR_SET_KEEPCAPS, 1, 0, 0, 0) != 0 {
            return Err(last_err());
        }
        let r = match &cuser {
            Some(c) => initgroups(c.as_ptr(), gid),
            None => setgroups(0, std::ptr::null()),
        };
        if r != 0 {
            return Err(last_err());
        }
        if setresgid(gid, gid, gid) != 0 {
            return Err(last_err());
        }
        if setresuid(uid, uid, uid) != 0 {
            return Err(last_err());
        }
        let bit = 1u32 << CAP_SYS_RAWIO;
        let mut hdr = CapHeader { version: 0x20080522, pid: 0 };
        let data = [CapData { effective: bit, permitted: bit, inheritable: bit }, CapData::default()];
        if syscall(SYS_CAPSET, &mut hdr as *mut CapHeader, data.as_ptr()) != 0 {
            return Err(last_err());
        }
        if prctl(PR_CAP_AMBIENT, PR_CAP_AMBIENT_RAISE, CAP_SYS_RAWIO as c_ulong, 0, 0) != 0 {
            return Err(last_err());
        }
    }
    Ok(())
}

pub fn stdin_is_tty() -> bool {
    unsafe { isatty(0) == 1 }
}

/// L'entrée standard a-t-elle des données (ou une fin de fichier) dans le délai ?
/// Évite de bloquer quand un lanceur laisse un tube ouvert sans y écrire.
pub fn stdin_ready(timeout_ms: i32) -> bool {
    if stdin_is_tty() {
        return false;
    }
    let mut p = PollFd { fd: 0, events: 1 /* POLLIN */, revents: 0 };
    unsafe { poll(&mut p, 1, timeout_ms) > 0 }
}

/// `struct termios` de la glibc (Linux).
#[repr(C)]
#[derive(Clone, Copy)]
pub struct Termios {
    c_iflag: u32,
    c_oflag: u32,
    c_cflag: u32,
    c_lflag: u32,
    c_line: u8,
    c_cc: [u8; 32],
    c_ispeed: u32,
    c_ospeed: u32,
}

/// Attend une touche sur l'entrée standard (terminal), sans Entrée ni écho,
/// pendant `timeout_ms` (-1 : sans limite). Renvoie vrai si une touche a été pressée.
pub fn wait_key(timeout_ms: i32) -> bool {
    const ICANON: u32 = 0o000002;
    const ECHO: u32 = 0o000010;
    const VMIN: usize = 6;
    const VTIME: usize = 5;
    unsafe {
        let mut old: Termios = std::mem::zeroed();
        let raw_ok = tcgetattr(0, &mut old) == 0;
        if raw_ok {
            let mut t = old;
            t.c_lflag &= !(ICANON | ECHO);
            t.c_cc[VMIN] = 1;
            t.c_cc[VTIME] = 0;
            tcsetattr(0, 0, &t);
        }
        let mut p = PollFd { fd: 0, events: 1, revents: 0 };
        let got = poll(&mut p, 1, timeout_ms) > 0;
        if got {
            let mut b = [0u8; 16];
            let _ = read(0, b.as_mut_ptr() as *mut std::ffi::c_void, b.len());
        }
        if raw_ok {
            tcsetattr(0, 0, &old);
        }
        got
    }
}
