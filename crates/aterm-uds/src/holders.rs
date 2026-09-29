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
//! Other platforms answer `Unsupported`.

use std::io;

/// What one scan found.
#[derive(Debug, Default, Clone, PartialEq, Eq)]
pub struct HolderScan {
    /// Processes that hold at least one descriptor for the device.
    pub holders: Vec<u32>,
    /// Processes of this uid whose descriptor table could not be read (they may
    /// have exited mid-scan, or refused).
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
/// `Some(false)`, or `None` when its table could not be read.
#[cfg(target_vendor = "apple")]
#[must_use]
// Skip: bottoms out at the libproc FFI calls.
#[cfg_attr(trust_verify, trust::skip)]
pub fn process_holds_rdev(pid: u32, rdev: u64) -> Option<bool> {
    let pid = i32::try_from(pid).ok()?;
    let fds = list_fds(pid)?;
    let mut unreadable = false;
    for (fd, kind) in fds {
        if kind != ffi::PROX_FDTYPE_VNODE {
            continue;
        }
        match vnode_rdev(pid, fd) {
            // `dev_t` is 32 bits on Darwin; the caller's u64 is `st_rdev` widened.
            Some(r) if u64::from(r) == rdev => return Some(true),
            Some(_) => {}
            // A descriptor closed between the listing and this read is gone;
            // any other failure makes the verdict unknown.
            None => unreadable = true,
        }
    }
    if unreadable { None } else { Some(false) }
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
    }
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
fn list_fds(pid: i32) -> Option<Vec<(i32, u32)>> {
    // SAFETY: a NULL buffer asks for the size in bytes.
    let need = unsafe { ffi::proc_pidinfo(pid, ffi::PROC_PIDLISTFDS, 0, std::ptr::null_mut(), 0) };
    if need <= 0 {
        return None;
    }
    // Headroom for descriptors opened between the two calls.
    let slots = usize::try_from(need).ok()? / ffi::FDINFO_SIZE + 32;
    let mut buf = vec![0u8; slots * ffi::FDINFO_SIZE];
    let size = i32::try_from(buf.len()).ok()?;
    // SAFETY: `buf` is `size` writable bytes.
    let got =
        unsafe { ffi::proc_pidinfo(pid, ffi::PROC_PIDLISTFDS, 0, buf.as_mut_ptr().cast(), size) };
    if got <= 0 {
        return None;
    }
    let got = usize::try_from(got).ok()?.min(buf.len());
    Some(
        buf.get(..got)?
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
            .collect(),
    )
}

#[cfg(target_vendor = "apple")]
fn vnode_rdev(pid: i32, fd: i32) -> Option<u32> {
    let mut buf = vec![0u8; ffi::VNODE_PATH_SIZE];
    let size = i32::try_from(buf.len()).ok()?;
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
    if got != size {
        return None;
    }
    Some(u32::from_ne_bytes(
        buf.get(ffi::VST_RDEV..ffi::VST_RDEV + 4)?.try_into().ok()?,
    ))
}

#[cfg(all(test, target_vendor = "apple"))]
mod tests {
    use std::os::unix::fs::MetadataExt as _;

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
            None,
            "gone: unreadable"
        );
    }

    /// An rdev nobody holds is held by nobody.
    #[test]
    fn an_unheld_rdev_has_no_holders() {
        let scan = super::scan_rdev_holders(0xfeed_0001, &[]).expect("scan");
        assert!(scan.holders.is_empty(), "{scan:?}");
    }
}
