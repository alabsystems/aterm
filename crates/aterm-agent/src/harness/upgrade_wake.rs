// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0

//! Park the Claude upgrade host until atpkg changes its live Claude twin.
//!
//! atpkg atomically replaces one marker in a dedicated directory after a mutating pass
//! leaves `agents/claude` pointing elsewhere. On macOS a directory kqueue
//! watches that replacement without a polling thread. Other directory writes
//! are coalesced by comparing the marker's file identity; the marker is only a
//! wake hint, and the host always re-reads the actual twin before acting.
//! Every wait is capped by the host's existing minute fallback.

#[cfg(target_os = "macos")]
use std::path::PathBuf;
use std::time::Duration;

/// One host's current marker and its last observed identity.
pub(super) struct ActivationWake {
    #[cfg(target_os = "macos")]
    marker: Option<PathBuf>,
    #[cfg(target_os = "macos")]
    seen: Option<Stamp>,
    #[cfg(target_os = "macos")]
    watch: Option<DirWatch>,
}

impl ActivationWake {
    pub(super) fn new() -> Self {
        #[cfg(target_os = "macos")]
        let marker = configured_marker();
        Self {
            #[cfg(target_os = "macos")]
            seen: marker.as_deref().and_then(stamp),
            #[cfg(target_os = "macos")]
            marker,
            #[cfg(target_os = "macos")]
            watch: None,
        }
    }

    /// Return as soon as the marker changes, else after `fallback`. A failed
    /// watch uses that same bounded fallback; no failure can stop upgrades.
    pub(super) fn wait(&mut self, fallback: Duration) {
        #[cfg(target_os = "macos")]
        self.wait_macos(fallback);
        #[cfg(not(target_os = "macos"))]
        {
            std::thread::sleep(fallback);
        }
    }

    #[cfg(target_os = "macos")]
    fn wait_macos(&mut self, fallback: Duration) {
        self.wait_macos_at(fallback, configured_marker);
    }

    #[cfg(target_os = "macos")]
    fn wait_macos_at(
        &mut self,
        fallback: Duration,
        mut configured_marker: impl FnMut() -> Option<PathBuf>,
    ) {
        let deadline = std::time::Instant::now() + fallback;
        loop {
            // A Settings edit may have moved the package prefix. The next
            // sweep must use the new one, even without a marker write.
            let configured = configured_marker();
            if configured != self.marker {
                self.marker = configured;
                self.seen = self.marker.as_deref().and_then(stamp);
                self.watch = None;
                return;
            }
            if self.changed() {
                return;
            }
            self.ensure_watch();
            // Snapshot BEFORE registration, register, then snapshot AGAIN:
            // a replacement in the gap is either seen here or queued by
            // kqueue. A notification never depends on timing that gap.
            if self.changed() {
                return;
            }
            let remaining = deadline.saturating_duration_since(std::time::Instant::now());
            if remaining.is_zero() {
                return;
            }
            match self.watch.as_ref().map(|w| w.wait(remaining)) {
                Some(Ok(true)) => {
                    // A directory entry moved. Compare the marker before
                    // deciding to sweep; unrelated atpkg writes cost no ps.
                    self.watch_if_directory_moved();
                }
                Some(Ok(false)) => return,
                Some(Err(_)) | None => {
                    self.watch = None;
                    std::thread::sleep(remaining);
                    return;
                }
            }
        }
    }

    #[cfg(target_os = "macos")]
    fn changed(&mut self) -> bool {
        let next = self.marker.as_deref().and_then(stamp);
        if next == self.seen {
            return false;
        }
        self.seen = next;
        true
    }

    #[cfg(target_os = "macos")]
    fn ensure_watch(&mut self) {
        let desired = self.marker.as_deref().and_then(watch_dir);
        if self.watch.as_ref().map(|w| &w.dir) == desired.as_ref() {
            return;
        }
        self.watch = desired.and_then(|dir| DirWatch::new(dir).ok());
    }

    #[cfg(target_os = "macos")]
    fn watch_if_directory_moved(&mut self) {
        // A first install can create the prefix while we watch its parent.
        // Move the watch inward before another change lands in the new dir.
        self.ensure_watch();
    }
}

#[cfg(target_os = "macos")]
fn configured_marker() -> Option<PathBuf> {
    atpkg::store::resolve_configured().map(|layout| atpkg::activation_notice::marker_path(&layout))
}

#[cfg(target_os = "macos")]
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct Stamp {
    dev: u64,
    ino: u64,
    len: u64,
    ctime: i64,
    ctime_nsec: i64,
}

#[cfg(target_os = "macos")]
fn stamp(path: &std::path::Path) -> Option<Stamp> {
    use std::os::unix::fs::MetadataExt as _;
    let m = std::fs::symlink_metadata(path).ok()?;
    if !m.file_type().is_file() {
        return None;
    }
    Some(Stamp {
        dev: m.dev(),
        ino: m.ino(),
        len: m.len(),
        ctime: m.ctime(),
        ctime_nsec: m.ctime_nsec(),
    })
}

#[cfg(target_os = "macos")]
fn watch_dir(marker: &std::path::Path) -> Option<PathBuf> {
    let notices = marker.parent()?;
    let prefix = notices.parent()?;
    // A normal host watches only `activation-notices/`: progress.json's
    // atomic writes in the package prefix never wake it. Before the first
    // install, watch the nearest existing parent and move inward as atpkg
    // creates each directory.
    [notices, prefix, prefix.parent()?]
        .into_iter()
        .find(|p| p.is_dir())
        .map(PathBuf::from)
}

#[cfg(target_os = "macos")]
struct DirWatch {
    dir: PathBuf,
    _file: std::fs::File,
    kq: std::os::fd::OwnedFd,
}

#[cfg(target_os = "macos")]
impl DirWatch {
    fn new(dir: PathBuf) -> std::io::Result<Self> {
        use std::os::fd::{AsRawFd as _, FromRawFd as _};
        use std::os::unix::fs::OpenOptionsExt as _;

        let file = std::fs::OpenOptions::new()
            .read(true)
            .custom_flags(libc::O_NOFOLLOW | libc::O_CLOEXEC)
            .open(&dir)?;
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
        Ok(Self {
            dir,
            _file: file,
            kq,
        })
    }

    fn wait(&self, timeout: Duration) -> std::io::Result<bool> {
        use std::os::fd::AsRawFd as _;
        // SAFETY: `kevent` initializes this out-parameter before it is read.
        let mut event: libc::kevent = unsafe { std::mem::zeroed() };
        let ts = libc::timespec {
            tv_sec: libc::time_t::try_from(timeout.as_secs()).unwrap_or(libc::time_t::MAX),
            tv_nsec: libc::c_long::from(timeout.subsec_nanos()),
        };
        // SAFETY: one event slot and an initialized timeout, both live for the call.
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
    fn marker_wakes_a_parked_host_and_unrelated_writes_do_not() {
        let root =
            std::env::temp_dir().join(format!("aterm-claude-upgrade-wake-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        std::fs::create_dir_all(&root).unwrap();
        let marker = root
            .join("pkg")
            .join(atpkg::activation_notice::NOTICE_DIR)
            .join(atpkg::activation_notice::CLAUDE_MARKER);
        let mut wake = ActivationWake {
            marker: Some(marker.clone()),
            seen: None,
            // Arm the parent watch before the writer starts. This makes the
            // unrelated-write assertion exercise a real parked watcher even
            // on a heavily loaded test machine.
            watch: Some(DirWatch::new(root.clone()).unwrap()),
        };
        let (tx, rx) = std::sync::mpsc::channel();
        let waited_marker = marker.clone();
        let waiter = std::thread::spawn(move || {
            wake.wait_macos_at(Duration::from_secs(5), || Some(waited_marker.clone()));
            tx.send(()).unwrap();
        });
        std::thread::sleep(Duration::from_millis(50));
        std::fs::write(root.join("unrelated"), b"x").unwrap();
        assert!(
            rx.recv_timeout(Duration::from_millis(150)).is_err(),
            "an unrelated parent write is not a Claude upgrade"
        );
        std::fs::create_dir(root.join("pkg")).unwrap();
        std::thread::sleep(Duration::from_millis(50));
        std::fs::create_dir(root.join("pkg").join(atpkg::activation_notice::NOTICE_DIR)).unwrap();
        std::thread::sleep(Duration::from_millis(50));
        assert_eq!(
            watch_dir(&marker),
            Some(root.join("pkg").join(atpkg::activation_notice::NOTICE_DIR))
        );
        std::fs::write(root.join("pkg/progress.json"), b"busy").unwrap();
        assert!(
            rx.recv_timeout(Duration::from_millis(150)).is_err(),
            "an active download's progress writes do not trigger a sweep"
        );
        let tmp = root
            .join("pkg")
            .join(atpkg::activation_notice::NOTICE_DIR)
            .join(".marker.tmp");
        std::fs::write(&tmp, b"1\n").unwrap();
        std::fs::rename(
            &tmp,
            root.join("pkg")
                .join(atpkg::activation_notice::NOTICE_DIR)
                .join(atpkg::activation_notice::CLAUDE_MARKER),
        )
        .unwrap();
        assert!(
            rx.recv_timeout(Duration::from_secs(2)).is_ok(),
            "the marker replacement wakes the parked host before the 5 s fallback"
        );
        waiter.join().unwrap();
        let _ = std::fs::remove_dir_all(root);
    }
}
