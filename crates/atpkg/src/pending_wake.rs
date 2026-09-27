// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0

//! Wake a pending tool's waiting command when a shim lands in `bin/`.
//!
//! The installer atomically renames a staged shim over the pending stub. On
//! macOS a directory vnode watch makes that edge promptly visible; the old
//! 250 ms poll remains the fallback on every platform and after watch failure.
//! A notification is only a hint: the caller always re-reads the pass first,
//! then the shim, before deciding whether to run or fail.

use std::time::Duration;

use crate::store::Layout;

pub(crate) struct PendingShimWake {
    #[cfg(target_os = "macos")]
    dir: std::path::PathBuf,
    #[cfg(target_os = "macos")]
    watch: Option<DirWatch>,
}

impl PendingShimWake {
    pub(crate) fn new(layout: &Layout) -> Self {
        #[cfg(target_os = "macos")]
        {
            let dir = layout.bin_dir();
            let watch = DirWatch::new(&dir).ok();
            Self { dir, watch }
        }
        #[cfg(not(target_os = "macos"))]
        {
            let _ = layout;
            Self {}
        }
    }

    /// Whether a directory change woke us before the fallback elapsed.
    pub(crate) fn wait(&mut self, fallback: Duration) -> bool {
        #[cfg(target_os = "macos")]
        if let Some(watch) = self.watch.as_ref() {
            let started = std::time::Instant::now();
            match watch.wait(fallback) {
                Ok(true) => {
                    // A rename of the directory itself invalidates its old
                    // vnode. Re-register before the next pass/shim read so
                    // a subsequent replacement still has a live listener.
                    self.watch = DirWatch::new(&self.dir).ok();
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
}

#[cfg(target_os = "macos")]
struct DirWatch {
    _dir: std::fs::File,
    kq: std::os::fd::OwnedFd,
}

#[cfg(target_os = "macos")]
impl DirWatch {
    fn new(dir: &std::path::Path) -> std::io::Result<Self> {
        use std::os::fd::{AsRawFd as _, FromRawFd as _};
        use std::os::unix::fs::OpenOptionsExt as _;

        let file = std::fs::OpenOptions::new()
            .read(true)
            .custom_flags(libc::O_NOFOLLOW | libc::O_CLOEXEC)
            .open(dir)?;
        if !file.metadata()?.is_dir() {
            return Err(std::io::Error::from(std::io::ErrorKind::NotADirectory));
        }
        // SAFETY: kqueue creates a new descriptor owned by this process.
        let raw = unsafe { libc::kqueue() };
        if raw < 0 {
            return Err(std::io::Error::last_os_error());
        }
        // SAFETY: `raw` is fresh and exclusively ours.
        let kq = unsafe { std::os::fd::OwnedFd::from_raw_fd(raw) };
        // SAFETY: F_SETFD changes only this descriptor's close-on-exec flag.
        unsafe { libc::fcntl(kq.as_raw_fd(), libc::F_SETFD, libc::FD_CLOEXEC) };
        let change = libc::kevent {
            ident: file.as_raw_fd() as libc::uintptr_t,
            filter: libc::EVFILT_VNODE,
            flags: libc::EV_ADD | libc::EV_ENABLE | libc::EV_CLEAR,
            fflags: libc::NOTE_WRITE | libc::NOTE_DELETE | libc::NOTE_RENAME,
            data: 0,
            udata: std::ptr::null_mut(),
        };
        // SAFETY: one initialized change entry, and no output slot.
        let rc = unsafe {
            libc::kevent(
                kq.as_raw_fd(),
                &change,
                1,
                std::ptr::null_mut(),
                0,
                std::ptr::null(),
            )
        };
        if rc < 0 {
            return Err(std::io::Error::last_os_error());
        }
        Ok(Self { _dir: file, kq })
    }

    fn wait(&self, timeout: Duration) -> std::io::Result<bool> {
        use std::os::fd::AsRawFd as _;

        // SAFETY: kevent initializes this output slot before it is read.
        let mut event: libc::kevent = unsafe { std::mem::zeroed() };
        let ts = libc::timespec {
            tv_sec: libc::time_t::try_from(timeout.as_secs()).unwrap_or(libc::time_t::MAX),
            tv_nsec: libc::c_long::from(timeout.subsec_nanos()),
        };
        // SAFETY: the initialized timeout and output slot live for this call.
        let rc =
            unsafe { libc::kevent(self.kq.as_raw_fd(), std::ptr::null(), 0, &mut event, 1, &ts) };
        if rc < 0 {
            return Err(std::io::Error::last_os_error());
        }
        if rc == 0 {
            return Ok(false);
        }
        if event.flags & libc::EV_ERROR != 0 {
            return Err(std::io::Error::from_raw_os_error(event.data as i32));
        }
        Ok(true)
    }
}

#[cfg(all(test, target_os = "macos"))]
mod tests {
    use super::*;

    #[test]
    fn pending_shim_rename_wakes_waiter_and_exposes_tool() {
        use std::os::unix::fs::symlink;

        let dir = std::env::temp_dir().join(format!(
            "atpkg-pending-wake-{}-{:?}",
            std::process::id(),
            std::thread::current().id()
        ));
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
        assert!(wake.watch.is_some(), "the test requires a live vnode watch");
        assert!(crate::which(&layout, "ty").is_none());
        // Stage outside the watched directory: only the final rename can
        // account for the event this assertion receives.
        let staged = dir.join("stage-ty");
        symlink(&target, &staged).unwrap();
        std::fs::rename(staged, &shim).unwrap();

        // The rename can finish BEFORE the wait starts. The registered watch
        // must retain that event rather than sleeping out the fallback.
        assert!(wake.wait(Duration::from_secs(1)));
        assert_eq!(crate::which(&layout, "ty"), Some(target));
        std::fs::remove_dir_all(dir).unwrap();
    }
}
