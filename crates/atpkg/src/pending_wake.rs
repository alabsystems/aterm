// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0

//! Directory-watch notification for commands waiting on pending tool shims or prefix passes.

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
#[path = "pending_wake_tests.rs"]
mod tests;
