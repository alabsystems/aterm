// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0

//! Wake a pending tool's waiting command when a shim lands in `bin/`, or when the pass it
//! waits on moves in the prefix.
//!
//! The installer atomically renames a staged shim over the pending stub, and lands the
//! pass's `progress.json` and retires its `pass-owed.json` promise in the prefix, each by a
//! rename or a removal. A watch on both directories makes those edges promptly visible —
//! the shim the moment it lands, and the pass's end or the promise's retirement (an offline
//! first launch's whole answer) the moment it is written rather than up to a poll later:
//! [`crate::dirwatch::Watcher`] on macOS (kqueue) and Linux (inotify), and this module's
//! own change notification on Windows, where that watcher has no arm. The old 250 ms poll
//! remains the fallback on every other platform and after watch failure. A notification is
//! only a hint: the caller always re-reads the pass first, then the shim, before deciding
//! whether to run or fail.

use std::time::Duration;

use crate::store::Layout;

pub(crate) struct PendingShimWake {
    #[cfg(any(target_os = "macos", target_os = "linux", windows))]
    dirs: [std::path::PathBuf; 2],
    #[cfg(any(target_os = "macos", target_os = "linux", windows))]
    watch: Option<DirWatch>,
}

impl PendingShimWake {
    pub(crate) fn new(layout: &Layout) -> Self {
        #[cfg(any(target_os = "macos", target_os = "linux", windows))]
        {
            let dirs = [layout.bin_dir(), layout.prefix.clone()];
            let watch = DirWatch::new(&dirs).ok();
            Self { dirs, watch }
        }
        #[cfg(not(any(target_os = "macos", target_os = "linux", windows)))]
        {
            let _ = layout;
            Self {}
        }
    }

    /// Whether a directory change woke us before the fallback elapsed.
    pub(crate) fn wait(&mut self, fallback: Duration) -> bool {
        #[cfg(any(target_os = "macos", target_os = "linux", windows))]
        if let Some(watch) = self.watch.as_ref() {
            let started = std::time::Instant::now();
            match watch.wait(fallback) {
                Ok(true) => {
                    // A rename of a directory itself invalidates its old
                    // vnode (inode, handle). Re-register before the next
                    // pass/shim read so a subsequent replacement still has a
                    // live listener; dropping the old watch drops its queue.
                    // The new one is made BEFORE the old one closes: on Linux
                    // a close that drops the last watch on a directory waits
                    // out an SRCU grace period (measured 3.9-4.8 ms on the
                    // lane), one that leaves the successor's watch 0.15 ms.
                    self.watch = DirWatch::new(&self.dirs).ok();
                    return true;
                }
                Ok(false) => return false,
                Err(_) => {
                    self.watch = None;
                    std::thread::sleep(fallback.saturating_sub(started.elapsed()));
                    return false;
                }
            }
        }
        std::thread::sleep(fallback);
        false
    }

    /// The wait is over: this process is about to exec the tool or exit. On Linux the
    /// watches come off now, so the descriptor's close at that exec or exit is no
    /// longer the one that drops the last watch on the two directories, which waits out
    /// an SRCU grace period: measured on the lane, a close 0.3 ms or more after the
    /// removal takes 0.15 ms, against 3.9 ms for a close that removes them itself. The
    /// descriptor stays open until then, since closing it here would pay that wait now.
    pub(crate) fn retire(&mut self) {
        #[cfg(target_os = "linux")]
        if let Some(watch) = self.watch.as_ref() {
            watch.unwatch();
        }
    }
}

/// Both directories under one [`crate::dirwatch::Watcher`]: an entry renamed into,
/// out of or removed from either, or either moved or removed, wakes [`Self::wait`].
#[cfg(any(target_os = "macos", target_os = "linux"))]
struct DirWatch {
    watcher: crate::dirwatch::Watcher,
    #[cfg_attr(
        not(target_os = "linux"),
        expect(dead_code, reason = "only Linux retires")
    )]
    ids: Vec<crate::dirwatch::WatchId>,
}

#[cfg(any(target_os = "macos", target_os = "linux"))]
impl DirWatch {
    fn new(dirs: &[std::path::PathBuf]) -> std::io::Result<Self> {
        let watcher = crate::dirwatch::Watcher::new()?;
        let ids = dirs
            .iter()
            .map(|dir| watcher.add_dir(dir))
            .collect::<std::io::Result<_>>()?;
        Ok(Self { watcher, ids })
    }

    /// Takes every watch off, leaving the descriptor open ([`PendingShimWake::retire`]).
    #[cfg(target_os = "linux")]
    fn unwatch(&self) {
        for &id in &self.ids {
            self.watcher.remove(id);
        }
    }

    /// Whether either directory moved within `timeout`. Nothing else wakes this watcher:
    /// a wake replaces the watch ([`PendingShimWake::wait`]).
    fn wait(&self, timeout: Duration) -> std::io::Result<bool> {
        Ok(!self.watcher.wait(Some(timeout))?.is_empty())
    }
}

#[cfg(windows)]
unsafe extern "system" {
    fn FindFirstChangeNotificationW(
        lp_path_name: *const u16,
        b_watch_subtree: i32,
        dw_notify_filter: u32,
    ) -> *mut core::ffi::c_void;
    fn FindCloseChangeNotification(h_change_handle: *mut core::ffi::c_void) -> i32;
    fn WaitForMultipleObjects(
        n_count: u32,
        lp_handles: *const *mut core::ffi::c_void,
        b_wait_all: i32,
        dw_milliseconds: u32,
    ) -> u32;
}

/// One change-notification handle per directory, each closed with the watch: this
/// module's own Windows arm, since [`crate::dirwatch`] has none there.
#[cfg(windows)]
struct DirWatch {
    handles: Vec<*mut core::ffi::c_void>,
}

#[cfg(windows)]
impl DirWatch {
    /// A change notification on every directory of `dirs`: a file or directory name
    /// created in, renamed in or removed from any of them signals its handle and wakes
    /// [`Self::wait`]. A reparse point (a symlink or junction) is refused, as the macOS
    /// arm's `O_NOFOLLOW` open refuses a symlink.
    fn new(dirs: &[std::path::PathBuf]) -> std::io::Result<Self> {
        use std::os::windows::ffi::OsStrExt as _;

        const FILE_NOTIFY_CHANGE_FILE_NAME: u32 = 0x1;
        const FILE_NOTIFY_CHANGE_DIR_NAME: u32 = 0x2;
        const INVALID_HANDLE_VALUE: isize = -1;
        let mut watch = Self {
            handles: Vec::with_capacity(dirs.len()),
        };
        for dir in dirs {
            if !std::fs::symlink_metadata(dir)?.is_dir() {
                return Err(std::io::Error::from(std::io::ErrorKind::NotADirectory));
            }
            let mut wide: Vec<u16> = dir.as_os_str().encode_wide().collect();
            wide.push(0);
            // SAFETY: `wide` is a NUL-terminated UTF-16 buffer that outlives the call; a
            // handle, or INVALID_HANDLE_VALUE with the reason in the last error, out.
            let handle = unsafe {
                FindFirstChangeNotificationW(
                    wide.as_ptr(),
                    0,
                    FILE_NOTIFY_CHANGE_FILE_NAME | FILE_NOTIFY_CHANGE_DIR_NAME,
                )
            };
            if handle as isize == INVALID_HANDLE_VALUE {
                return Err(std::io::Error::last_os_error());
            }
            watch.handles.push(handle);
        }
        Ok(watch)
    }

    fn wait(&self, timeout: Duration) -> std::io::Result<bool> {
        const WAIT_OBJECT_0: u32 = 0;
        const WAIT_TIMEOUT: u32 = 0x102;
        const INFINITE: u32 = u32::MAX;
        // Rounded up, and kept below INFINITE: a long fallback still ends.
        let ms = u32::try_from(timeout.as_nanos().div_ceil(1_000_000))
            .unwrap_or(INFINITE)
            .min(INFINITE - 1);
        let count = u32::try_from(self.handles.len()).unwrap_or(u32::MAX);
        // SAFETY: `count` live change handles, owned by this watch, for the call's length.
        let rc = unsafe { WaitForMultipleObjects(count, self.handles.as_ptr(), 0, ms) };
        if rc == WAIT_TIMEOUT {
            return Ok(false);
        }
        if (WAIT_OBJECT_0..WAIT_OBJECT_0 + count).contains(&rc) {
            return Ok(true);
        }
        Err(std::io::Error::last_os_error())
    }
}

#[cfg(windows)]
impl Drop for DirWatch {
    fn drop(&mut self) {
        for &handle in &self.handles {
            // SAFETY: each handle came from FindFirstChangeNotificationW and is closed
            // once, here.
            unsafe { FindCloseChangeNotification(handle) };
        }
    }
}

#[cfg(all(test, any(target_os = "macos", target_os = "linux", windows)))]
mod tests {
    use super::*;

    #[test]
    #[cfg(unix)]
    fn pending_shim_rename_wakes_waiter_and_exposes_tool() {
        use std::os::unix::fs::symlink;

        let dir = crate::testdir::dir("atpkg-pending-wake-");
        std::fs::create_dir_all(dir.join("bin")).unwrap();
        let layout = Layout {
            prefix: dir.clone(),
        };
        let shim = layout.bin_dir().join("ty");
        std::fs::write(&shim, b"#!/bin/sh\n# atpkg pending\n").unwrap();
        let target = dir.join("store/ty/44/bin/ty");
        std::fs::create_dir_all(target.parent().unwrap()).unwrap();
        std::fs::write(&target, b"#!/bin/sh\nexit 0\n").unwrap();

        let mut wake = PendingShimWake::new(&layout);
        assert!(
            wake.watch.is_some(),
            "the test requires a live directory watch"
        );
        assert!(crate::which(&layout, "ty").is_none());
        // Stage outside the watched directories (bin/ and the prefix): only the
        // final rename can account for the event this assertion receives.
        let stage = crate::testdir::dir("atpkg-pending-wake-stage-");
        let staged = stage.join("stage-ty");
        symlink(&target, &staged).unwrap();
        std::fs::rename(staged, &shim).unwrap();
        std::fs::remove_dir_all(&stage).unwrap();

        // The rename can finish BEFORE the wait starts. The registered watch
        // must retain that event rather than sleeping out the fallback.
        assert!(wake.wait(aterm_spec::wait::MUST_HAPPEN));
        assert_eq!(crate::which(&layout, "ty"), Some(target));
        std::fs::remove_dir_all(&dir).unwrap();
    }

    /// The pass a stub waits on moves in the prefix, not in `bin/`: the promise retired, the
    /// progress file landed. Either wakes the waiter at once — an offline first launch's
    /// pass ends with no shim ever landing, and the stub's answer is that end.
    #[test]
    fn a_retired_promise_or_a_landed_progress_file_wakes_the_waiter() {
        let dir = crate::testdir::dir("atpkg-pending-wake-pass-");
        std::fs::create_dir_all(dir.join("bin")).unwrap();
        let layout = Layout {
            prefix: dir.clone(),
        };
        std::fs::write(layout.pass_owed_file(), b"{}").unwrap();
        let mut wake = PendingShimWake::new(&layout);
        assert!(
            wake.watch.is_some(),
            "the test requires a live directory watch"
        );
        std::fs::remove_file(layout.pass_owed_file()).unwrap();
        assert!(
            wake.wait(aterm_spec::wait::MUST_HAPPEN),
            "the promise retired"
        );
        // Staged outside the watched directories, so only the landing rename can wake it.
        let stage = crate::testdir::dir("atpkg-pending-wake-pass-stage-");
        let staged = stage.join("progress.json.tmp");
        std::fs::write(&staged, b"{}").unwrap();
        std::fs::rename(&staged, layout.progress_file()).unwrap();
        std::fs::remove_dir_all(&stage).unwrap();
        assert!(
            wake.wait(aterm_spec::wait::MUST_HAPPEN),
            "the pass's file landed"
        );
        std::fs::remove_dir_all(&dir).unwrap();
    }

    /// The wait's end takes both watches off and keeps the descriptor open
    /// ([`PendingShimWake::retire`]), so the close at the tool's exec or this process's
    /// exit is not the one that drops the last watch on the two directories. Read off the
    /// kernel's own account of the instance, one `inotify wd:` line per watch.
    #[test]
    #[cfg(target_os = "linux")]
    fn retiring_takes_the_watches_off_and_keeps_the_descriptor() {
        let dir = crate::testdir::dir("atpkg-pending-wake-retire-");
        std::fs::create_dir_all(dir.join("bin")).unwrap();
        let layout = Layout {
            prefix: dir.clone(),
        };
        let mut wake = PendingShimWake::new(&layout);
        let fd = wake
            .watch
            .as_ref()
            .expect("the test requires a live directory watch")
            .watcher
            .inotify_fd();
        let watches = || {
            std::fs::read_to_string(format!("/proc/self/fdinfo/{fd}"))
                .expect("the descriptor is open")
                .lines()
                .filter(|line| line.starts_with("inotify wd:"))
                .count()
        };
        assert_eq!(watches(), 2, "one watch on bin/, one on the prefix");
        wake.retire();
        assert_eq!(watches(), 0, "no watch is left for the close to drop");
        std::fs::remove_dir_all(&dir).unwrap();
    }
}
