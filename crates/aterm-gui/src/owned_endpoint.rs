// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! What this instance published on its control socket, and the one rule by which
//! any exit removes it.
//!
//! Once the socket is bound, [`publish`] records the socket's and token's file
//! identities (device, inode) and the `latest` alias with its target. Every exit
//! removes each file only while it is still the one this instance published, and
//! only in this process: a successor that rebound the same path owns a new inode, a
//! newer instance may have repointed `latest`, and a child between `fork` and `exec`
//! is not the publisher.
//!
//! When this instance rebinds its own socket at the same path (the listener
//! watchdog replacing a wedged one, the cold update lane reopening after an `exec`
//! that did not happen), [`rebound`] moves the record to the fresh socket, only from
//! the one it replaced.
//!
//! * The loop's graceful exit calls [`release`].
//! * `SIGTERM`, `SIGINT` and `SIGHUP` end the process by default action, running no
//!   loop-exit code, and an explicit `$ATERM_CONTROL_SOCK` is not swept by anyone
//!   else. [`publish`] installs a handler for each of the three whose disposition is
//!   still the default (an ignored one, like `nohup`'s `SIGHUP`, stays ignored); it
//!   removes by the same rule, restores the default and re-raises.
//!
//! On Windows no file identity is read: the graceful exit unlinks by path
//! (`control_auth::cleanup_socket`) and the stale sweep reclaims names.
//!
//! Async-signal safety (the discipline [`crate::crash_signal`] states): every path is
//! resolved and encoded at publish time and parked, never freed, in a `OnceLock` set
//! before any handler is installed; the socket inode a rebind moves is an atomic. The
//! removal calls only `getpid(2)`, `lstat(2)`, `readlink(2)` and `unlink(2)` (the
//! handler adds `sigaction(2)` and `raise(3)`), allocates nothing and takes no lock.

#[cfg(unix)]
pub(crate) use imp::{publish, rebound, release};

#[cfg(unix)]
mod imp {
    use std::ffi::CString;
    use std::os::unix::ffi::OsStrExt;
    use std::path::{Path, PathBuf};
    use std::sync::OnceLock;
    use std::sync::atomic::{AtomicU64, Ordering};

    /// The signals whose default action ends the process without cleanup.
    const EXIT_SIGNALS: [libc::c_int; 3] = [libc::SIGTERM, libc::SIGINT, libc::SIGHUP];

    /// One file this instance published, by path and identity.
    pub(super) struct Owned {
        path: CString,
        /// Device and inode as `MetadataExt` widens them. The inode moves when this
        /// instance renames another of its own files over the path
        /// ([`Owned::follow`]); the device cannot, since a rename never crosses one.
        dev: u64,
        ino: AtomicU64,
    }

    impl Owned {
        /// The file at `path` now, or `None` when it is gone or unnamed.
        pub(super) fn capture(path: &Path) -> Option<Self> {
            use std::os::unix::fs::MetadataExt;
            let meta = std::fs::symlink_metadata(path).ok()?;
            Some(Self {
                path: CString::new(path.as_os_str().as_bytes()).ok()?,
                dev: meta.dev(),
                ino: AtomicU64::new(meta.ino()),
            })
        }

        /// Record `current` in place of `previous` (both `(device, inode)`) after this
        /// instance renamed its own file over the published one; only while the record
        /// still names `previous`.
        pub(super) fn follow(&self, previous: (u64, u64), current: (u64, u64)) -> bool {
            previous.0 == self.dev
                && current.0 == self.dev
                && self
                    .ino
                    .compare_exchange(previous.1, current.1, Ordering::AcqRel, Ordering::Acquire)
                    .is_ok()
        }

        /// Unlink the file iff the path still names the same file.
        /// Async-signal-safe: `lstat` and `unlink` only.
        #[allow(
            clippy::unnecessary_cast,
            reason = "dev_t and ino_t are narrower than u64 on some targets; widened as std widens them"
        )]
        pub(super) fn remove_if_still_ours(&self) -> bool {
            // SAFETY: `stat` is POD, so zeroed is valid, and `lstat` writes it;
            // `self.path` is a live NUL-terminated string. `lstat`/`unlink` are
            // async-signal-safe.
            unsafe {
                let mut now: libc::stat = std::mem::zeroed();
                if libc::lstat(self.path.as_ptr(), &mut now) != 0
                    || now.st_dev as u64 != self.dev
                    || now.st_ino as u64 != self.ino.load(Ordering::Acquire)
                {
                    return false;
                }
                libc::unlink(self.path.as_ptr()) == 0
            }
        }
    }

    /// The `latest` alias and the target this instance pointed it at.
    pub(super) struct Alias {
        link: CString,
        target: Vec<u8>,
    }

    impl Alias {
        /// Unlink the alias iff it still points where this instance pointed
        /// it. Async-signal-safe: `readlink` into a stack buffer, `unlink`.
        pub(super) fn remove_if_still_ours(&self) -> bool {
            let mut buf = [0u8; 1024];
            // SAFETY: `buf` is a live, writable stack buffer of the passed length;
            // `readlink` returns the byte count (no NUL), bounds-checked below.
            // `readlink`/`unlink` are async-signal-safe.
            unsafe {
                let n = libc::readlink(self.link.as_ptr(), buf.as_mut_ptr().cast(), buf.len());
                let Ok(n) = usize::try_from(n) else {
                    return false;
                };
                if buf.get(..n) != Some(self.target.as_slice()) {
                    return false;
                }
                libc::unlink(self.link.as_ptr()) == 0
            }
        }
    }

    /// Everything the handler removes. Set once, before the first handler is
    /// installed; never freed.
    pub(super) struct Published {
        /// The process that published them: a child forked between `fork` and
        /// `exec` inherits the handler and must not take its parent's socket.
        pub(super) pid: libc::pid_t,
        pub(super) socket: Option<Owned>,
        pub(super) token: Option<Owned>,
        pub(super) alias: Option<Alias>,
    }

    impl Published {
        /// What `plan` published, read now. A relative socket name is anchored
        /// to this process's working directory, which is where it was bound.
        pub(super) fn capture(plan: &crate::control_auth::SocketPlan) -> Self {
            let sock = Path::new(&plan.sock_path);
            let sock: PathBuf = if sock.is_absolute() {
                sock.to_path_buf()
            } else {
                std::env::current_dir().map_or_else(|_| sock.to_path_buf(), |cwd| cwd.join(sock))
            };
            let alias = plan.latest_link.as_deref().and_then(|link| {
                Some(Alias {
                    link: CString::new(link.as_os_str().as_bytes()).ok()?,
                    target: sock.file_name()?.as_bytes().to_vec(),
                })
            });
            Self {
                pid: std::process::id().cast_signed(),
                socket: Owned::capture(&sock),
                token: Owned::capture(&plan.token_path),
                alias,
            }
        }

        /// [`Owned::follow`] for the socket: a rebind at its path.
        pub(super) fn rebound(&self, previous: (u64, u64), current: (u64, u64)) -> bool {
            self.socket
                .as_ref()
                .is_some_and(|socket| socket.follow(previous, current))
        }

        /// Remove every file still this instance's, when called in the process
        /// that published them. Async-signal-safe: `getpid` and the removals.
        pub(super) fn remove(&self) {
            // SAFETY: `getpid` takes nothing and is async-signal-safe.
            if unsafe { libc::getpid() } != self.pid {
                return;
            }
            if let Some(alias) = &self.alias {
                alias.remove_if_still_ours();
            }
            for file in [&self.socket, &self.token].into_iter().flatten() {
                file.remove_if_still_ours();
            }
        }
    }

    static PUBLISHED: OnceLock<Published> = OnceLock::new();

    extern "C" fn on_exit_signal(sig: libc::c_int) {
        if let Some(published) = PUBLISHED.get() {
            published.remove();
        }
        // SAFETY: a zeroed `sigaction` with `SIG_DFL` restores the default action;
        // `sigaction`/`raise` are async-signal-safe. The signal is masked while this
        // handler runs, so the re-raise is delivered on return.
        unsafe {
            let mut act: libc::sigaction = std::mem::zeroed();
            act.sa_sigaction = libc::SIG_DFL;
            libc::sigaction(sig, &act, std::ptr::null_mut());
            #[expect(
                clippy::disallowed_methods,
                reason = "the termination signal that arrived, re-raised with its default action after cleanup"
            )]
            libc::raise(sig);
        }
    }

    /// The graceful exit's removal ([`super`]): what [`publish`] recorded, by
    /// the rule the signal handler uses. Nothing before [`publish`] ran.
    pub(crate) fn release(_plan: &crate::control_auth::SocketPlan) {
        if let Some(published) = PUBLISHED.get() {
            published.remove();
        }
    }

    /// This instance rebound its control socket at the published path: `previous` was
    /// replaced by `current`, a socket it bound itself (`control_listener::rebind_socket`),
    /// both `(device, inode)`. Exits then remove `current`, still only while the path
    /// names it. `false` when the record did not name `previous`.
    pub(crate) fn rebound(previous: (u64, u64), current: (u64, u64)) -> bool {
        PUBLISHED
            .get()
            .is_some_and(|published| published.rebound(previous, current))
    }

    /// Record what `plan` published and arm the handlers. Once per process: a
    /// second call (a rolled-back handoff republishing) keeps the first record.
    pub(crate) fn publish(plan: &crate::control_auth::SocketPlan) {
        if PUBLISHED.set(Published::capture(plan)).is_err() {
            return;
        }
        for sig in EXIT_SIGNALS {
            // SAFETY: both `sigaction` structs are zeroed POD; the first call only
            // reads the disposition, the second installs a valid `extern "C"` handler
            // with an empty mask. Runs at arm time, outside signal context.
            unsafe {
                let mut old: libc::sigaction = std::mem::zeroed();
                if libc::sigaction(sig, std::ptr::null(), &mut old) != 0
                    || old.sa_sigaction != libc::SIG_DFL
                {
                    continue;
                }
                let mut act: libc::sigaction = std::mem::zeroed();
                act.sa_sigaction = on_exit_signal as *const () as usize;
                act.sa_flags = libc::SA_RESTART;
                libc::sigemptyset(&mut act.sa_mask);
                libc::sigaction(sig, &act, std::ptr::null_mut());
            }
        }
    }
}

/// Nothing to record off Unix: no terminating signal to hook, and no file
/// identity read.
#[cfg(not(unix))]
pub(crate) fn publish(_plan: &crate::control_auth::SocketPlan) {
    // Windows ends a process without a POSIX terminating signal, and its
    // endpoint is a named file the next instance's stale sweep reclaims.
}

/// The graceful exit's removal off Unix: by path ([`crate::control_auth::cleanup_socket`]).
#[cfg(not(unix))]
pub(crate) fn release(plan: &crate::control_auth::SocketPlan) {
    crate::control_auth::cleanup_socket(plan);
}

#[cfg(all(test, unix))]
mod tests {
    use std::path::PathBuf;

    use super::imp::{Owned, Published};

    fn scratch(tag: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!("atxs-{tag}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        dir
    }

    /// The handler's removal removes the published socket, token and alias. Negative
    /// controls: a socket another listener rebound, a token replaced by rename, and an
    /// alias a newer instance repointed all stay.
    #[test]
    fn a_terminating_signal_removes_only_what_this_instance_still_owns() {
        let dir = scratch("own");
        let sock = dir.join("aterm-1.sock");
        let token = dir.join("aterm-1.token");
        let link = dir.join("latest");
        let _listener = std::os::unix::net::UnixListener::bind(&sock).unwrap();
        std::fs::write(&token, "t").unwrap();
        std::os::unix::fs::symlink("aterm-1.sock", &link).unwrap();
        let plan = crate::control_auth::SocketPlan {
            sock_path: sock.to_string_lossy().into_owned(),
            token_path: token.clone(),
            latest_link: Some(link.clone()),
        };
        Published::capture(&plan).remove();
        assert!(!sock.exists() && !token.exists());
        assert!(
            std::fs::symlink_metadata(&link).is_err(),
            "the alias went too"
        );

        // A successor rebound the path, replaced the token and repointed
        // `latest` after this instance captured them.
        let _old = std::os::unix::net::UnixListener::bind(&sock).unwrap();
        std::fs::write(&token, "t").unwrap();
        std::os::unix::fs::symlink("aterm-1.sock", &link).unwrap();
        let published = Published::capture(&plan);
        std::fs::remove_file(&sock).unwrap();
        let _successor = std::os::unix::net::UnixListener::bind(&sock).unwrap();
        let replacement = dir.join("token.new");
        std::fs::write(&replacement, "u").unwrap();
        std::fs::rename(&replacement, &token).unwrap();
        std::fs::remove_file(&link).unwrap();
        std::os::unix::fs::symlink("aterm-2.sock", &link).unwrap();
        // A forked child that inherited the record removes nothing.
        let foreign = Published {
            pid: published.pid + 1,
            socket: Owned::capture(&sock),
            token: None,
            alias: None,
        };
        foreign.remove();
        assert!(sock.exists(), "another process's record removes nothing");
        published.remove();
        assert!(sock.exists(), "the successor's socket is not ours");
        assert_eq!(std::fs::read_to_string(&token).unwrap(), "u");
        assert_eq!(
            std::fs::read_link(&link).unwrap(),
            PathBuf::from("aterm-2.sock")
        );
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// A rebind at the same path renames a fresh socket over the published one, and
    /// the exit removes the fresh one. Negative control: a record that no longer names
    /// the replaced socket does not move, and another instance's socket stays.
    #[test]
    fn the_exit_removes_the_socket_a_rebind_put_at_the_same_path() {
        use crate::control_listener::{rebind_socket, socket_file_id};
        let dir = scratch("rebind");
        let sock = dir.join("aterm-1.sock");
        let _first = aterm_uds::CtlListener::bind(&sock).unwrap();
        let plan = crate::control_auth::SocketPlan {
            sock_path: sock.to_string_lossy().into_owned(),
            token_path: dir.join("aterm-1.token"),
            latest_link: None,
        };
        let published = Published::capture(&plan);
        let previous = socket_file_id(&plan.sock_path).unwrap();
        let mut expected = Some(previous);
        let _fresh = rebind_socket(&plan.sock_path, &mut expected).expect("rebind");
        let current = expected.unwrap();
        assert_ne!(previous, current, "the rebind put a new socket at the path");
        published.rebound(previous, current);
        published.remove();
        assert!(!sock.exists(), "the exit removes the rebound socket");

        let _other = std::os::unix::net::UnixListener::bind(&sock).unwrap();
        let other = socket_file_id(&plan.sock_path).unwrap();
        assert!(!published.rebound(previous, other));
        published.remove();
        assert!(sock.exists(), "another instance's socket at the path stays");
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// With no record (nothing published by this process), the loop's exit
    /// ([`super::release`]) deletes nothing by path: another instance's socket and
    /// token at the same explicit path stay.
    #[test]
    fn the_graceful_exit_removes_nothing_it_did_not_record() {
        let dir = scratch("grace");
        let sock = dir.join("aterm.sock");
        let token = dir.join("aterm.token");
        let _other = std::os::unix::net::UnixListener::bind(&sock).unwrap();
        std::fs::write(&token, "t").unwrap();
        let plan = crate::control_auth::SocketPlan {
            sock_path: sock.to_string_lossy().into_owned(),
            token_path: token.clone(),
            latest_link: None,
        };
        super::release(&plan);
        assert!(sock.exists() && token.exists());
        // Negative control: the record of those same files removes them.
        Published::capture(&plan).remove();
        assert!(!sock.exists() && !token.exists());
        let _ = std::fs::remove_dir_all(&dir);
    }
}
