// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0

//! Wake a pending tool's waiting command when a shim lands in `bin/`, or when the pass it
//! waits on moves in the prefix.
//!
//! The installer atomically renames a staged shim over the pending stub, and lands the
//! pass's `progress.json` and retires its `pass-owed.json` promise in the prefix, each by a
//! rename or a removal. On macOS a vnode watch on both directories makes those edges promptly
//! visible — the shim the moment it lands, and the pass's end or the promise's retirement
//! (an offline first launch's whole answer) the moment it is written rather than up to a
//! poll later; the old 250 ms poll remains the fallback on every platform and after watch
//! failure. A notification is only a hint: the caller always re-reads the pass first, then
//! the shim, before deciding whether to run or fail.

use std::time::Duration;

use crate::store::Layout;

pub(crate) struct PendingShimWake {
    #[cfg(target_os = "macos")]
    dirs: [std::path::PathBuf; 2],
    #[cfg(target_os = "macos")]
    watch: Option<DirWatch>,
}

impl PendingShimWake {
    pub(crate) fn new(layout: &Layout) -> Self {
        #[cfg(target_os = "macos")]
        {
            let dirs = [layout.bin_dir(), layout.prefix.clone()];
            let watch = DirWatch::new(&dirs).ok();
            Self { dirs, watch }
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
                    // A rename of a directory itself invalidates its old
                    // vnode. Re-register before the next pass/shim read so
                    // a subsequent replacement still has a live listener.
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
}

#[cfg(target_os = "macos")]
struct DirWatch {
    _dirs: Vec<std::fs::File>,
    kq: std::os::fd::OwnedFd,
}

#[cfg(target_os = "macos")]
impl DirWatch {
    /// One kqueue over every directory of `dirs`: an entry renamed into, out of or removed
    /// from any of them wakes [`Self::wait`].
    fn new(dirs: &[std::path::PathBuf]) -> std::io::Result<Self> {
        use std::os::fd::{AsRawFd as _, FromRawFd as _};
        use std::os::unix::fs::OpenOptionsExt as _;

        // SAFETY: kqueue creates a new descriptor owned by this process.
        let raw = unsafe { libc::kqueue() };
        if raw < 0 {
            return Err(std::io::Error::last_os_error());
        }
        // SAFETY: `raw` is fresh and exclusively ours.
        let kq = unsafe { std::os::fd::OwnedFd::from_raw_fd(raw) };
        // SAFETY: F_SETFD changes only this descriptor's close-on-exec flag.
        unsafe { libc::fcntl(kq.as_raw_fd(), libc::F_SETFD, libc::FD_CLOEXEC) };
        let mut files = Vec::with_capacity(dirs.len());
        for dir in dirs {
            let file = std::fs::OpenOptions::new()
                .read(true)
                .custom_flags(libc::O_NOFOLLOW | libc::O_CLOEXEC)
                .open(dir)?;
            if !file.metadata()?.is_dir() {
                return Err(std::io::Error::from(std::io::ErrorKind::NotADirectory));
            }
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
            files.push(file);
        }
        Ok(Self { _dirs: files, kq })
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
        assert!(wake.watch.is_some(), "the test requires a live vnode watch");
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
        assert!(wake.watch.is_some(), "the test requires a live vnode watch");
        std::fs::remove_file(layout.pass_owed_file()).unwrap();
        assert!(
            wake.wait(aterm_spec::wait::MUST_HAPPEN),
            "the promise retired"
        );
        let staged = dir.join("progress.json.tmp");
        std::fs::write(&staged, b"{}").unwrap();
        std::fs::rename(&staged, layout.progress_file()).unwrap();
        assert!(
            wake.wait(aterm_spec::wait::MUST_HAPPEN),
            "the pass's file landed"
        );
        std::fs::remove_dir_all(&dir).unwrap();
    }
}
