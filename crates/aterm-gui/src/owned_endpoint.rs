// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! What this instance published on its control socket, and the ONE rule by
//! which any exit takes it away.
//!
//! Once the socket is bound, [`publish`] records what this instance published
//! — the socket's and the token's file identities (device and inode) and the
//! `latest` alias with the target it was pointed at. Every exit removes each
//! file only while it is still the one this instance published, and only in
//! that process: a successor that rebound the same explicit path in a
//! self-update overlap owns a new inode, a newer instance may have repointed
//! `latest`, and a child forked between `fork` and `exec` is not the publisher.
//!
//! * The loop's graceful exit calls [`release`].
//! * `SIGTERM` (a `kill`, a process manager, a test harness tearing down its
//!   headless instance), `SIGINT` (Ctrl-C on a headless instance started from
//!   a shell) and `SIGHUP` end the process by their default action, which runs
//!   no loop-exit code at all: the socket, its token and the alias stayed
//!   behind, and every client dialled a dead endpoint (the 2026-09-24 live
//!   probe, D10; the next spawn's stale sweep clears a per-instance name, but
//!   an explicit `$ATERM_CONTROL_SOCK` is nobody else's to sweep). [`publish`]
//!   therefore also installs a handler for each of the three whose disposition
//!   is still the default (one a parent set to ignore, `nohup`'s `SIGHUP`,
//!   stays ignored), which removes by the same rule, restores the default
//!   disposition and re-raises, so the process dies exactly as it would have.
//!
//! Until 2026-09-24 the graceful exit unlinked by PATH (`control_auth::
//! cleanup_socket`), so it could delete a successor's rebound socket that the
//! signal path refused to touch; that is now the Windows rule alone, where no
//! file identity is read and the stale sweep reclaims the names.
//!
//! ASYNC-SIGNAL-SAFETY, the discipline [`crate::crash_signal`] states: every
//! path is resolved and encoded at publish time and parked, never freed, in a
//! `OnceLock` set before any handler is installed; the removal itself calls
//! only `getpid(2)`, `lstat(2)`, `readlink(2)` and `unlink(2)` (and the
//! handler `sigaction(2)` and `raise(3)`), allocates nothing and takes no lock.

#[cfg(unix)]
pub(crate) use imp::{publish, release};

#[cfg(unix)]
mod imp {
    use std::ffi::CString;
    use std::os::unix::ffi::OsStrExt;
    use std::path::{Path, PathBuf};
    use std::sync::OnceLock;

    /// The signals whose default action ends the process without cleanup.
    const EXIT_SIGNALS: [libc::c_int; 3] = [libc::SIGTERM, libc::SIGINT, libc::SIGHUP];

    /// One file this instance published, by path and identity.
    pub(super) struct Owned {
        path: CString,
        /// Device and inode as `std`'s `MetadataExt` widens them.
        dev: u64,
        ino: u64,
    }

    impl Owned {
        /// The file at `path` now, or `None` when it is gone or unnamed.
        pub(super) fn capture(path: &Path) -> Option<Self> {
            use std::os::unix::fs::MetadataExt;
            let meta = std::fs::symlink_metadata(path).ok()?;
            Some(Self {
                path: CString::new(path.as_os_str().as_bytes()).ok()?,
                dev: meta.dev(),
                ino: meta.ino(),
            })
        }

        /// Unlink the file iff the path still names the same file.
        /// Async-signal-safe: `lstat` and `unlink` only.
        #[allow(
            clippy::unnecessary_cast,
            reason = "dev_t and ino_t are narrower than u64 on some targets; widened as std widens them"
        )]
        pub(super) fn remove_if_still_ours(&self) -> bool {
            // SAFETY: `stat` is plain old data, so a zeroed value is valid, and
            // `lstat` writes it; `self.path` is a live NUL-terminated string.
            // `lstat`/`unlink` are on the POSIX async-signal-safe list.
            unsafe {
                let mut now: libc::stat = std::mem::zeroed();
                if libc::lstat(self.path.as_ptr(), &mut now) != 0
                    || now.st_dev as u64 != self.dev
                    || now.st_ino as u64 != self.ino
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
            // SAFETY: `buf` is a live, writable stack buffer of the length
            // passed; `readlink` does not NUL-terminate and returns the byte
            // count, which is bounds-checked below. `readlink`/`unlink` are
            // async-signal-safe.
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
        // SAFETY: a zeroed `sigaction` with `SIG_DFL` restores the default
        // action; `sigaction`/`raise` are async-signal-safe. The signal is
        // masked while this handler runs, so the re-raise is delivered on
        // return and ends the process the conventional way.
        unsafe {
            let mut act: libc::sigaction = std::mem::zeroed();
            act.sa_sigaction = libc::SIG_DFL;
            libc::sigaction(sig, &act, std::ptr::null_mut());
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

    /// Record what `plan` published and arm the handlers. Once per process: a
    /// second call (a rolled-back handoff republishing) keeps the first record.
    pub(crate) fn publish(plan: &crate::control_auth::SocketPlan) {
        if PUBLISHED.set(Published::capture(plan)).is_err() {
            return;
        }
        for sig in EXIT_SIGNALS {
            // SAFETY: both `sigaction` structs are zeroed POD; the first call
            // only reads the current disposition, the second installs a valid
            // `extern "C"` handler with an empty mask. Called at arm time, on
            // no signal context.
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

    /// The handler's removal, run directly: the socket, the token and the
    /// alias this instance published go. NEGATIVE CONTROLS: a socket path
    /// another listener rebound (a new inode), a token rewritten by
    /// replacement, and an alias a newer instance repointed all stay.
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

    /// The loop's exit removes by the same record ([`super::release`]), so
    /// with none — nothing this process published — a path alone deletes
    /// nothing: the socket and token another instance bound at the same
    /// explicit path stay. (Until 2026-09-24 the graceful exit unlinked them by
    /// path.) No test in this binary publishes, so the record is empty here.
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
        // NEGATIVE CONTROL: the record of those same files removes them.
        Published::capture(&plan).remove();
        assert!(!sock.exists() && !token.exists());
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// Nothing to remove is not an error: an already-gone file stays gone.
    #[test]
    fn an_absent_file_is_left_absent() {
        let dir = scratch("gone");
        let path = dir.join("f");
        std::fs::write(&path, "x").unwrap();
        let owned = Owned::capture(&path).unwrap();
        std::fs::remove_file(&path).unwrap();
        assert!(!owned.remove_if_still_ours());
        assert!(Owned::capture(&path).is_none());
        let _ = std::fs::remove_dir_all(&dir);
    }
}
