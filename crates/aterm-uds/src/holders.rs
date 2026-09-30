// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! THE HOLDER SCAN: which live processes of this uid hold a descriptor for one
//! character device, named by its `st_rdev`.
//!
//! The PTY keeper offers a dead window's terminal to the next window only when
//! no live process still holds that terminal's master
//! (`docs/DESIGN-pty-keeper-2026-09-26.md` §4 item 3): the two-reader guarantee
//! then does not rest on a message having been delivered. A master's `st_rdev`
//! survives `dup` and `SCM_RIGHTS`, and a same-uid process cannot mint one
//! (F10), so the rdev IS the terminal's identity across processes.
//!
//! macOS reads it through libproc: `proc_listpids(PROC_UID_ONLY)`, then per
//! process `proc_pidinfo(PROC_PIDLISTFDS)` and, per vnode descriptor,
//! `proc_pidfdinfo(PROC_PIDFDVNODEPATHINFO)`. Measured green (§11 M10): exact,
//! 16–39 µs for a 31-descriptor process, including the notarized,
//! hardened-runtime daily driver. A process whose table cannot be read is
//! reported in [`HolderScan::unreadable`], never silently counted as a
//! non-holder: the caller decides what an unreadable table means (the keeper
//! refuses an offer when the unreadable process is one of its own peers).
//!
//! A process that has EXITED is not unreadable: it holds nothing. The kernel
//! closes every descriptor at the exit, before the parent reaps it, and says
//! so with `ESRCH` — for a zombie the listing still names (measured
//! 2026-09-29: `proc_listpids` lists it, `PROC_PIDLISTFDS` answers `ESRCH`),
//! and for a process that exits between the listing and the read. Read as
//! "unknown", a peer of the keeper that had died but was not yet judged
//! turned a quitting window's death into a handoff: its masters held, then
//! orphaned — the shells kept and a relaunch run.
//!
//! Nor does a DESCRIPTOR that closed between the listing and its read make a
//! table unreadable: its read answers `EBADF` (closed, or its number reused by
//! something that is no vnode), and nothing is held through it. A busy
//! process's table churns, and every closed descriptor used to make it
//! "unknown".
//!
//! Other platforms answer `Unsupported`.

use std::io;

/// What one scan found.
#[derive(Debug, Default, Clone, PartialEq, Eq)]
pub struct HolderScan {
    /// Processes that hold at least one descriptor for the device.
    pub holders: Vec<u32>,
    /// Processes of this uid whose descriptor table could not be read: refused,
    /// or failed for a reason other than having exited or a descriptor having
    /// closed mid-read (a process that has exited holds nothing, and is not
    /// listed here).
    pub unreadable: Vec<u32>,
    /// How many processes were scanned.
    pub scanned: usize,
}

impl HolderScan {
    /// Whether `pid` holds the device.
    #[must_use]
    pub fn holds(&self, pid: u32) -> bool {
        self.holders.contains(&pid)
    }
}

/// Scan every live process of this uid except `skip` for a descriptor whose
/// `st_rdev` is `rdev`. `skip` is the caller's own pid when it holds the device
/// itself (the keeper's custody copy).
///
/// # Errors
/// The process listing itself failing; `Unsupported` off macOS.
#[cfg(target_vendor = "apple")]
// Skip: bottoms out at the libproc FFI calls.
#[cfg_attr(trust_verify, trust::skip)]
pub fn scan_rdev_holders(rdev: u64, skip: &[u32]) -> io::Result<HolderScan> {
    let mut scan = HolderScan::default();
    for pid in uid_pids()? {
        if skip.contains(&pid) {
            continue;
        }
        scan.scanned += 1;
        match process_holds_rdev(pid, rdev) {
            Some(true) => scan.holders.push(pid),
            Some(false) => {}
            None => scan.unreadable.push(pid),
        }
    }
    Ok(scan)
}

/// See the macOS twin.
///
/// # Errors
/// Always `Unsupported`.
#[cfg(not(target_vendor = "apple"))]
pub fn scan_rdev_holders(_rdev: u64, _skip: &[u32]) -> io::Result<HolderScan> {
    Err(io::Error::new(
        io::ErrorKind::Unsupported,
        "the holder scan reads libproc (macOS)",
    ))
}

/// Whether the one process `pid` holds a descriptor for `rdev`: `Some(true)`,
/// `Some(false)` (also when it has exited: it holds nothing), or `None` when
/// its table could not be read.
#[cfg(target_vendor = "apple")]
#[must_use]
pub fn process_holds_rdev(pid: u32, rdev: u64) -> Option<bool> {
    match read_table(pid, rdev) {
        TableRead::Holds => Some(true),
        TableRead::Clear | TableRead::Exited => Some(false),
        TableRead::Unreadable => None,
    }
}

/// What reading one process's descriptor table for one device found.
#[cfg(target_vendor = "apple")]
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum TableRead {
    /// A descriptor for the device.
    Holds,
    /// No descriptor for it.
    Clear,
    /// The process has exited (`ESRCH`), before the listing's read or during
    /// the descriptors' reads: every descriptor it had is closed.
    Exited,
    /// Unknown: the table was refused, or a read failed otherwise.
    Unreadable,
}

/// Why one libproc read failed, by its `errno`.
#[cfg(target_vendor = "apple")]
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Failed {
    /// `ESRCH`: no such process — it has exited (a zombie's descriptors are
    /// closed at its exit) or been reaped.
    Exited,
    /// `EBADF`, reading one descriptor: it is gone — closed since the
    /// listing, or its number reused by something that is no vnode.
    Closed,
    /// Anything else (`EPERM`, or a failure that set no `errno`).
    Other,
}

#[cfg(target_vendor = "apple")]
impl Failed {
    const ESRCH: i32 = 3;
    const EBADF: i32 = 9;

    fn of(errno: i32) -> Self {
        match errno {
            Self::ESRCH => Self::Exited,
            Self::EBADF => Self::Closed,
            _ => Self::Other,
        }
    }

    /// The failure the last libproc call on this thread reported (its
    /// `errno`, cleared before the call by [`clear_errno`]).
    fn last() -> Self {
        Self::of(io::Error::last_os_error().raw_os_error().unwrap_or(0))
    }
}

// Skip: bottoms out at the libproc FFI calls.
#[cfg(target_vendor = "apple")]
#[cfg_attr(trust_verify, trust::skip)]
fn read_table(pid: u32, rdev: u64) -> TableRead {
    let Ok(pid) = i32::try_from(pid) else {
        return TableRead::Unreadable;
    };
    let fds = match list_fds(pid) {
        Ok(fds) => fds,
        Err(Failed::Exited) => return TableRead::Exited,
        Err(Failed::Closed | Failed::Other) => return TableRead::Unreadable,
    };
    #[cfg(test)]
    hook::after_listing(pid, &fds);
    let mut unreadable = false;
    for (fd, kind) in fds {
        if kind != ffi::PROX_FDTYPE_VNODE {
            continue;
        }
        match vnode_rdev(pid, fd) {
            // `dev_t` is 32 bits on Darwin; the caller's u64 is `st_rdev` widened.
            Ok(r) if u64::from(r) == rdev => return TableRead::Holds,
            Ok(_) => {}
            // It exited after the listing: none of its descriptors is open.
            Err(Failed::Exited) => return TableRead::Exited,
            // A descriptor closed between the listing and this read is gone:
            // nothing is held through it.
            Err(Failed::Closed) => {}
            // Any other failure makes the verdict unknown.
            Err(Failed::Other) => unreadable = true,
        }
    }
    if unreadable {
        TableRead::Unreadable
    } else {
        TableRead::Clear
    }
}

/// See the macOS twin.
#[cfg(not(target_vendor = "apple"))]
#[must_use]
pub fn process_holds_rdev(_pid: u32, _rdev: u64) -> Option<bool> {
    None
}

#[cfg(target_vendor = "apple")]
mod ffi {
    // libproc.h / sys/proc_info.h, measured with the SDK 2026-09-28.
    pub const PROC_UID_ONLY: u32 = 4;
    pub const PROC_PIDLISTFDS: i32 = 1;
    pub const PROC_PIDFDVNODEPATHINFO: i32 = 2;
    pub const PROX_FDTYPE_VNODE: u32 = 1;
    /// `sizeof(struct proc_fdinfo)`: `int32_t proc_fd; uint32_t proc_fdtype;`.
    pub const FDINFO_SIZE: usize = 8;
    /// `sizeof(struct vnode_fdinfowithpath)`.
    pub const VNODE_PATH_SIZE: usize = 1200;
    /// `offsetof(struct vnode_fdinfowithpath, pvip.vip_vi.vi_stat.vst_rdev)`.
    pub const VST_RDEV: usize = 140;

    unsafe extern "C" {
        pub fn proc_listpids(kind: u32, typeinfo: u32, buffer: *mut i32, size: i32) -> i32;
        pub fn proc_pidinfo(
            pid: i32,
            flavor: i32,
            arg: u64,
            buffer: *mut core::ffi::c_void,
            size: i32,
        ) -> i32;
        pub fn proc_pidfdinfo(
            pid: i32,
            fd: i32,
            flavor: i32,
            buffer: *mut core::ffi::c_void,
            size: i32,
        ) -> i32;
        pub fn geteuid() -> u32;
        /// Darwin's `errno` location.
        pub fn __error() -> *mut i32;
    }
}

/// Clear `errno`, so that a libproc call's failure reads as its own.
#[cfg(target_vendor = "apple")]
fn clear_errno() {
    // SAFETY: `__error` returns this thread's live `errno` slot.
    unsafe { *ffi::__error() = 0 };
}

#[cfg(target_vendor = "apple")]
fn uid_pids() -> io::Result<Vec<u32>> {
    // SAFETY: a side-effect-free getter.
    let uid = unsafe { ffi::geteuid() };
    // Size, then fill with headroom: processes start between the two calls, and
    // a full buffer is re-asked with more room rather than trusted.
    let mut room = 1024usize;
    loop {
        let mut pids = vec![0i32; room];
        let bytes = i32::try_from(room * 4).map_err(|_| io::Error::other("pid list too large"))?;
        // SAFETY: `pids` is `bytes` writable bytes of i32 slots.
        let got = unsafe { ffi::proc_listpids(ffi::PROC_UID_ONLY, uid, pids.as_mut_ptr(), bytes) };
        if got <= 0 {
            return Err(io::Error::last_os_error());
        }
        let n = usize::try_from(got).unwrap_or(0) / 4;
        if n >= room && room < (1 << 20) {
            room *= 4;
            continue;
        }
        return Ok(pids
            .into_iter()
            .take(n)
            .filter_map(|p| u32::try_from(p).ok())
            .filter(|p| *p > 0)
            .collect());
    }
}

#[cfg(target_vendor = "apple")]
fn list_fds(pid: i32) -> Result<Vec<(i32, u32)>, Failed> {
    clear_errno();
    // SAFETY: a NULL buffer asks for the size in bytes.
    let need = unsafe { ffi::proc_pidinfo(pid, ffi::PROC_PIDLISTFDS, 0, std::ptr::null_mut(), 0) };
    if need <= 0 {
        return Err(Failed::last());
    }
    // Headroom for descriptors opened between the two calls.
    let slots = usize::try_from(need).map_err(|_| Failed::Other)? / ffi::FDINFO_SIZE + 32;
    let mut buf = vec![0u8; slots * ffi::FDINFO_SIZE];
    let size = i32::try_from(buf.len()).map_err(|_| Failed::Other)?;
    clear_errno();
    // SAFETY: `buf` is `size` writable bytes.
    let got =
        unsafe { ffi::proc_pidinfo(pid, ffi::PROC_PIDLISTFDS, 0, buf.as_mut_ptr().cast(), size) };
    if got <= 0 {
        return Err(Failed::last());
    }
    let got = usize::try_from(got)
        .map_err(|_| Failed::Other)?
        .min(buf.len());
    Ok(buf
        .get(..got)
        .ok_or(Failed::Other)?
        .as_chunks::<{ ffi::FDINFO_SIZE }>()
        .0
        .iter()
        .map(|c| {
            let [a, b, c0, d, e, f, g, h] = *c;
            (
                i32::from_ne_bytes([a, b, c0, d]),
                u32::from_ne_bytes([e, f, g, h]),
            )
        })
        .collect())
}

#[cfg(target_vendor = "apple")]
fn vnode_rdev(pid: i32, fd: i32) -> Result<u32, Failed> {
    let mut buf = vec![0u8; ffi::VNODE_PATH_SIZE];
    let size = i32::try_from(buf.len()).map_err(|_| Failed::Other)?;
    clear_errno();
    // SAFETY: `buf` is exactly the structure this flavor fills.
    let got = unsafe {
        ffi::proc_pidfdinfo(
            pid,
            fd,
            ffi::PROC_PIDFDVNODEPATHINFO,
            buf.as_mut_ptr().cast(),
            size,
        )
    };
    if got <= 0 {
        return Err(Failed::last());
    }
    if got != size {
        return Err(Failed::Other);
    }
    let bytes = buf
        .get(ffi::VST_RDEV..ffi::VST_RDEV + 4)
        .and_then(|b| <[u8; 4]>::try_from(b).ok())
        .ok_or(Failed::Other)?;
    Ok(u32::from_ne_bytes(bytes))
}

/// A TEST SEAM (unit tests only): run a closure between one table's listing
/// and the reads of its descriptors, to make a race certain.
#[cfg(all(test, target_vendor = "apple"))]
mod hook {
    use std::cell::RefCell;

    type Hook = Box<dyn FnOnce(i32, &[(i32, u32)])>;

    thread_local! {
        static AFTER_LISTING: RefCell<Option<Hook>> = const { RefCell::new(None) };
    }

    /// Run `f` once, after the next listing this thread makes, with the pid
    /// listed and its `(fd, type)` pairs.
    pub fn after_next_listing(f: impl FnOnce(i32, &[(i32, u32)]) + 'static) {
        AFTER_LISTING.with(|h| *h.borrow_mut() = Some(Box::new(f)));
    }

    pub(super) fn after_listing(pid: i32, fds: &[(i32, u32)]) {
        if let Some(f) = AFTER_LISTING.with(|h| h.borrow_mut().take()) {
            f(pid, fds);
        }
    }
}

#[cfg(all(test, target_vendor = "apple"))]
mod tests {
    use std::os::unix::fs::MetadataExt as _;
    use std::time::{Duration, Instant};

    use super::{ffi, hook};
    use crate::exitwatch::ExitWatch;

    const HANG: Duration = Duration::from_secs(60);

    unsafe extern "C" {
        fn kill(pid: i32, sig: i32) -> i32;
        fn posix_openpt(flags: i32) -> i32;
        fn fcntl(fd: i32, cmd: i32, ...) -> i32;
    }

    /// The floors the descriptors below are duplicated to. Both sit under 256,
    /// macOS's default soft `RLIMIT_NOFILE` (`launchctl limit maxfiles`):
    /// `F_DUPFD_CLOEXEC` at or above the soft limit fails `EINVAL`, so a floor
    /// of 900 failed in every shell that had not raised it. Both sit above
    /// `pspawn`'s 200, so no sibling test's duplicate lands on either.
    const LOW: i32 = 210;
    const HIGH: i32 = 240;

    /// `fd` duplicated, close-on-exec, to the lowest free number at or above
    /// `at` (the allocator hands every other open the lowest free number, so
    /// no other thread's descriptor takes that one once it closes).
    fn dup_at(fd: &std::os::fd::OwnedFd, at: i32) -> std::os::fd::OwnedFd {
        use std::os::fd::{AsRawFd as _, FromRawFd as _};
        const F_DUPFD_CLOEXEC: i32 = 67;
        // SAFETY: F_DUPFD_CLOEXEC on a descriptor we own; the result is ours.
        let high = unsafe { fcntl(fd.as_raw_fd(), F_DUPFD_CLOEXEC, at) };
        assert!(
            high >= at,
            "F_DUPFD_CLOEXEC at {at}: {}",
            std::io::Error::last_os_error()
        );
        // SAFETY: the duplicate, owned by nothing else.
        unsafe { std::os::fd::OwnedFd::from_raw_fd(high) }
    }

    /// A fresh PTY master — a device no other process holds — at the lowest
    /// free number at or above `at` (the low original is closed), and its rdev.
    fn a_master_at(at: i32) -> (std::os::fd::OwnedFd, u64) {
        use std::os::fd::FromRawFd as _;
        const O_RDWR: i32 = 2;
        const O_NOCTTY: i32 = 0x20000;
        // SAFETY: a fresh descriptor this test owns.
        let raw = unsafe { posix_openpt(O_RDWR | O_NOCTTY) };
        assert!(raw >= 0, "posix_openpt");
        // SAFETY: as above.
        let low = unsafe { std::os::fd::OwnedFd::from_raw_fd(raw) };
        let fd = dup_at(&low, at);
        drop(low);
        let rdev = std::fs::File::from(fd.try_clone().expect("dup"))
            .metadata()
            .expect("fstat")
            .rdev();
        (fd, rdev)
    }

    /// A DESCRIPTOR CLOSED BETWEEN THE LISTING AND ITS READ is not held, and
    /// does not make the table unknown: its read answers `EBADF`. This
    /// process holds a fresh PTY master through one descriptor, and the hook
    /// closes it after the listing named it: held by nothing now.
    /// NEGATIVE CONTROLS: a closed descriptor ends nothing but its own read —
    /// the master's descriptor at `LOW` closes while a second one for it
    /// stays open ABOVE it, so the scan (ascending) meets the `EBADF` first
    /// and must go on to find the device held; and a failure that is neither
    /// a closed descriptor nor an exit still reads as unknown.
    #[test]
    fn a_descriptor_closed_between_the_listing_and_its_read_is_not_held() {
        use std::os::fd::AsRawFd as _;
        let me = std::process::id();
        let (fd, rdev) = a_master_at(LOW);
        let n = fd.as_raw_fd();
        assert_eq!(super::process_holds_rdev(me, rdev), Some(true), "held");
        let named = std::rc::Rc::new(std::cell::Cell::new(false));
        let seen = std::rc::Rc::clone(&named);
        hook::after_next_listing(move |_, fds| {
            seen.set(fds.contains(&(n, ffi::PROX_FDTYPE_VNODE)));
            drop(fd);
        });
        assert_eq!(
            super::process_holds_rdev(me, rdev),
            Some(false),
            "closed after the listing: not held, and not unknown"
        );
        assert!(named.get(), "the listing named the descriptor it closed");

        // Control: the LOW descriptor closes after the listing; a second
        // descriptor for the same master, ABOVE it, stays open.
        let (low, rdev) = a_master_at(LOW);
        let kept = dup_at(&low, HIGH);
        let (n, k) = (low.as_raw_fd(), kept.as_raw_fd());
        let order = std::rc::Rc::new(std::cell::Cell::new(None));
        let seen = std::rc::Rc::clone(&order);
        hook::after_next_listing(move |_, fds| {
            let at = |fd| fds.iter().position(|e| *e == (fd, ffi::PROX_FDTYPE_VNODE));
            seen.set(Some((at(n), at(k))));
            drop(low);
        });
        assert_eq!(
            super::process_holds_rdev(me, rdev),
            Some(true),
            "the closed descriptor's EBADF ends only its own read"
        );
        match order.get() {
            Some((Some(closed), Some(open))) => assert!(
                closed < open,
                "the scan meets the closed descriptor before the open one"
            ),
            other => panic!("the listing named both descriptors: {other:?}"),
        }
        drop(kept);

        // Control: only an exit or a closed descriptor is a known answer.
        use super::Failed;
        assert_eq!(Failed::of(3), Failed::Exited, "ESRCH");
        assert_eq!(Failed::of(9), Failed::Closed, "EBADF");
        for other in [0, 1, 12, 22] {
            assert_eq!(Failed::of(other), Failed::Other, "errno {other}");
        }
    }

    /// A `sleep` whose stdin is `/dev/null`, once the kernel shows it holding
    /// the device; and that device's rdev.
    fn a_holder_of_dev_null() -> (std::process::Child, u64) {
        let rdev = std::fs::metadata("/dev/null").expect("stat").rdev();
        let child = std::process::Command::new("/bin/sleep")
            .arg("600")
            .stdin(std::process::Stdio::null())
            .spawn()
            .expect("spawn");
        let started = Instant::now();
        while super::process_holds_rdev(child.id(), rdev) != Some(true) {
            assert!(
                started.elapsed() < HANG,
                "the child never showed the device"
            );
            std::thread::yield_now();
        }
        (child, rdev)
    }

    /// SIGKILL `pid` and return once the kernel reports its exit: it is a
    /// zombie then, until its parent (this test) reaps it.
    fn kill_unreaped(pid: u32) {
        let watch = ExitWatch::watch(pid).expect("watch the child");
        // SAFETY: SIGKILL to a child this test started and has not reaped.
        unsafe { kill(pid as i32, 9) };
        let started = Instant::now();
        while watch.exit_status().is_none() {
            assert!(started.elapsed() < HANG, "the child never exited");
            std::thread::yield_now();
        }
    }

    /// AN EXITED PROCESS HOLDS NOTHING. A killed child its parent has not
    /// reaped is still in the listing the scan walks (a zombie), and the
    /// kernel closed its descriptors at the exit: it is a non-holder, never
    /// "unreadable" (the keeper read an unreadable peer as a possible holder).
    /// NEGATIVE CONTROL: a table the kernel refuses (launchd's, root's:
    /// `EPERM`) is still unreadable.
    #[test]
    fn an_exited_process_still_listed_holds_nothing() {
        let (mut child, rdev) = a_holder_of_dev_null();
        let pid = child.id();
        kill_unreaped(pid);
        assert!(
            super::uid_pids().expect("listing").contains(&pid),
            "the zombie is still listed: the scan meets it"
        );
        assert_eq!(super::process_holds_rdev(pid, rdev), Some(false));
        let scan = super::scan_rdev_holders(rdev, &[]).expect("scan");
        assert!(!scan.holds(pid), "{scan:?}");
        assert!(!scan.unreadable.contains(&pid), "{scan:?}");
        let _ = child.wait();
        assert_eq!(
            super::process_holds_rdev(1, rdev),
            None,
            "a refused table is unknown"
        );
    }

    /// A process that exits BETWEEN THE LISTING AND THE READS of its
    /// descriptors holds nothing either: its descriptor reads answer `ESRCH`.
    #[test]
    fn a_process_that_exits_after_its_listing_holds_nothing() {
        let (mut child, rdev) = a_holder_of_dev_null();
        let pid = child.id();
        let listed = std::rc::Rc::new(std::cell::Cell::new(0usize));
        let seen = std::rc::Rc::clone(&listed);
        hook::after_next_listing(move |at, fds| {
            assert_eq!(at, pid as i32);
            seen.set(
                fds.iter()
                    .filter(|(_, kind)| *kind == ffi::PROX_FDTYPE_VNODE)
                    .count(),
            );
            kill_unreaped(pid);
        });
        assert_eq!(super::process_holds_rdev(pid, rdev), Some(false));
        assert!(
            listed.get() > 0,
            "the listing named its vnodes, read after its exit"
        );
        let _ = child.wait();
    }

    /// A child holding a file of ours is found by its rdev... A regular file has
    /// rdev 0, so the probe device here is `/dev/null` (a character device with
    /// a real rdev): a `sleep` whose stdin is `/dev/null` holds it, and a scan
    /// that SKIPS that child does not report it.
    #[test]
    fn the_scan_finds_a_child_holding_a_character_device() {
        let rdev = std::fs::metadata("/dev/null").expect("stat").rdev();
        assert_ne!(rdev, 0, "/dev/null is a character device");
        let mut child = std::process::Command::new("/bin/sleep")
            .arg("600")
            .stdin(std::process::Stdio::null())
            .spawn()
            .expect("spawn");
        // The child's stdin is /dev/null from its first instruction; wait for
        // the exec to have happened by asking the kernel for the child's table
        // until it names the device (a hang detector bounds it).
        let started = std::time::Instant::now();
        loop {
            if super::process_holds_rdev(child.id(), rdev) == Some(true) {
                break;
            }
            assert!(
                started.elapsed().as_secs() < 60,
                "the child never showed the device"
            );
            std::thread::yield_now();
        }
        let scan = super::scan_rdev_holders(rdev, &[]).expect("scan");
        assert!(scan.holds(child.id()), "{scan:?}");
        assert!(scan.scanned > 0);
        let skipped = super::scan_rdev_holders(rdev, &[child.id()]).expect("scan");
        assert!(!skipped.holds(child.id()), "skip is honoured");
        let _ = child.kill();
        let _ = child.wait();
        assert_eq!(
            super::process_holds_rdev(child.id(), rdev),
            Some(false),
            "gone: holds nothing"
        );
    }

    /// An rdev nobody holds is held by nobody.
    #[test]
    fn an_unheld_rdev_has_no_holders() {
        let scan = super::scan_rdev_holders(0xfeed_0001, &[]).expect("scan");
        assert!(scan.holders.is_empty(), "{scan:?}");
    }
}
