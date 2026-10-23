// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! What this instance published on its control socket, and the one rule by which any exit removes it.

#[cfg(unix)]
pub(crate) use imp::{
    arm_orderly_quit, die_of_quit_signal, hold_quit_for_exec, publish, rebound, release,
};

/// How long a headless instance's terminating signal waits for the orderly quit before the immediate path ends the process ([`arm_orderly_quit`]'s backstop): as generous as a cut lifeline's (`lifeline::SHUTDOWN_GRACE`), for a main thread that is not coming back rather than a slow one.
#[cfg(unix)]
pub(crate) const QUIT_GRACE: std::time::Duration = std::time::Duration::from_secs(30);

#[cfg(unix)]
mod imp {
    use std::ffi::CString;
    use std::os::unix::ffi::OsStrExt;
    use std::path::{Path, PathBuf};
    use std::sync::OnceLock;
    use std::sync::atomic::{AtomicI32, AtomicU64, Ordering};

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

        /// Record `current` in place of `previous` (both `(device, inode)`) after this instance renamed its own file over the published one; only while the record still names `previous`.
        pub(super) fn follow(&self, previous: (u64, u64), current: (u64, u64)) -> bool {
            previous.0 == self.dev
                && current.0 == self.dev
                && self
                    .ino
                    .compare_exchange(previous.1, current.1, Ordering::AcqRel, Ordering::Acquire)
                    .is_ok()
        }

        /// Unlink the file iff the path still names the same file.
        #[allow(
            clippy::unnecessary_cast,
            reason = "dev_t and ino_t are narrower than u64 on some targets; widened as std widens them"
        )]
        pub(super) fn remove_if_still_ours(&self) -> bool {
            // SAFETY: `stat` is POD, so zeroed is valid, and `lstat` writes it; `self.path` is a live NUL-terminated string.
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
        /// Unlink the alias iff it still points where this instance pointed it.
        pub(super) fn remove_if_still_ours(&self) -> bool {
            let mut buf = [0u8; 1024];
            // SAFETY: `buf` is a live, writable stack buffer of the passed length; `readlink` returns the byte count (no NUL), bounds-checked below.
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

    /// Everything the handler removes.
    pub(super) struct Published {
        /// The process that published them: a child forked between `fork` and `exec` inherits the handler and must not take its parent's socket.
        pub(super) pid: libc::pid_t,
        pub(super) socket: Option<Owned>,
        pub(super) token: Option<Owned>,
        pub(super) alias: Option<Alias>,
    }

    impl Published {
        /// What `plan` published, read now.
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

        /// Remove every file still this instance's, when called in the process that published them.
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

    /// The write end of the orderly quit's pipe ([`arm_orderly_quit`]); `-1` while
    /// unarmed.
    static QUIT_PIPE: AtomicI32 = AtomicI32::new(-1);

    /// The process that armed the orderly quit ([`arm_orderly_quit`]); `0` while
    /// unarmed.
    static ARMED_PID: AtomicI32 = AtomicI32::new(0);

    /// The terminating signal handed to the orderly quit; `0` until one is, and
    /// [`EXEC_IN_FLIGHT`] while a cold `exec` holds the claim ([`hold_quit_for_exec`]).
    static QUIT_SIGNAL: AtomicI32 = AtomicI32::new(0);

    /// [`QUIT_SIGNAL`] while a cold `exec` is in flight: no signal's number, so the
    /// handler's claim ([`claim_quit`]) fails and it takes the immediate path.
    const EXEC_IN_FLIGHT: libc::c_int = -1;

    /// The handler's claim on the orderly quit: `sig` goes into `state` only while nothing is there — not after a first signal, not while a cold `exec` holds it.
    pub(super) fn claim_quit(state: &AtomicI32, sig: libc::c_int) -> bool {
        state
            .compare_exchange(0, sig, Ordering::AcqRel, Ordering::Acquire)
            .is_ok()
    }

    /// The claim a cold `exec` holds ([`hold_for_exec`]) while it is in flight; given back when dropped, which is when the `exec` returned (it failed) and this process carries on.
    pub(crate) struct ExecHold<'a>(&'a AtomicI32);

    impl Drop for ExecHold<'_> {
        fn drop(&mut self) {
            let _ = self
                .0
                .compare_exchange(EXEC_IN_FLIGHT, 0, Ordering::AcqRel, Ordering::Acquire);
        }
    }

    /// The claim on `state` for an `exec`, or `None` when a signal's quit has it.
    pub(super) fn hold_for_exec(state: &AtomicI32) -> Option<ExecHold<'_>> {
        state
            .compare_exchange(0, EXEC_IN_FLIGHT, Ordering::AcqRel, Ordering::Acquire)
            .ok()
            .map(|_| ExecHold(state))
    }

    /// THE COLD UPDATE'S `exec` TAKES THE ORDERLY QUIT'S CLAIM FIRST ([`super`]): `None` when a terminating signal already asked for the quit, and the caller then does not `exec` (the event loop runs that quit next); else the hold, kept until the `exec` returns, under which a signal ends the process at once.
    pub(crate) fn hold_quit_for_exec() -> Option<ExecHold<'static>> {
        hold_for_exec(&QUIT_SIGNAL)
    }

    extern "C" fn on_exit_signal(sig: libc::c_int) {
        let fd = QUIT_PIPE.load(Ordering::Acquire);
        // SAFETY: `getpid` takes nothing and is async-signal-safe.
        let ours = unsafe { libc::getpid() } == ARMED_PID.load(Ordering::Acquire);
        if fd >= 0 && ours && claim_quit(&QUIT_SIGNAL, sig) {
            #[allow(
                clippy::cast_possible_truncation,
                clippy::cast_sign_loss,
                reason = "a signal number is under 64"
            )]
            let byte = [sig as u8];
            // SAFETY: `write(2)` of one byte from a live stack buffer to the pipe's write end, which is never closed; async-signal-safe.
            if unsafe { libc::write(fd, byte.as_ptr().cast(), 1) } == 1 {
                return;
            }
        }
        die_now(sig);
    }

    /// The immediate path: remove what is still ours, restore `sig`'s default
    /// action and re-raise it.
    fn die_now(sig: libc::c_int) {
        if let Some(published) = PUBLISHED.get() {
            published.remove();
        }
        // SAFETY: a zeroed `sigaction` with `SIG_DFL` restores the default action; `sigaction`/`raise` are async-signal-safe.
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

    /// ARM THE ORDERLY QUIT ([`super`]), in a headless instance once its event loop can be asked: a thread waits on the pipe the handler writes, logs the signal, and calls `quit` with its number (the caller posts the quit to the loop), and the handlers are installed now, whether or not a control socket is ever published.
    pub(crate) fn arm_orderly_quit(
        quit: impl FnOnce(libc::c_int) + Send + 'static,
    ) -> std::io::Result<()> {
        use std::io::Read as _;
        use std::os::fd::IntoRawFd as _;
        static ARMED: std::sync::atomic::AtomicBool = std::sync::atomic::AtomicBool::new(false);
        if ARMED.swap(true, Ordering::AcqRel) {
            return Ok(());
        }
        let (mut reader, writer) = std::io::pipe()?;
        let fd = writer.into_raw_fd();
        std::thread::Builder::new()
            .name("aterm-quit-signal".to_string())
            .spawn(move || {
                crate::qos::set_self(crate::qos::Role::Background);
                let mut byte = [0u8; 1];
                if reader.read_exact(&mut byte).is_err() {
                    return;
                }
                let sig = libc::c_int::from(byte[0]);
                let name = signal_name(sig);
                aterm_log::info!(
                    "{name}: quitting as the Quit verb does (a second {name} ends the process at once)"
                );
                crate::logging::stderr_line!("aterm-gui: {name}: quitting");
                quit(sig);
                std::thread::sleep(super::QUIT_GRACE);
                aterm_log::error!(
                    "{name} {} s ago and the quit has not finished; ending the process now",
                    super::QUIT_GRACE.as_secs()
                );
                die_now(sig);
            })?;
        // SAFETY: `getpid` takes nothing.
        ARMED_PID.store(unsafe { libc::getpid() }, Ordering::Release);
        QUIT_PIPE.store(fd, Ordering::Release);
        install_exit_handlers();
        Ok(())
    }

    /// The word for a terminating signal, as a log line names it.
    fn signal_name(sig: libc::c_int) -> &'static str {
        match sig {
            libc::SIGTERM => "SIGTERM",
            libc::SIGINT => "SIGINT",
            libc::SIGHUP => "SIGHUP",
            _ => "a terminating signal",
        }
    }

    /// At the end of a quit: when a terminating signal asked for it ([`arm_orderly_quit`]), die of that signal now, its default action restored, so the parent reads the status a signal gives — the crash marker removed first, as the `atexit` handler a signal death skips would have; else return, and the caller exits as it would.
    pub(crate) fn die_of_quit_signal() {
        let sig = QUIT_SIGNAL.load(Ordering::Acquire);
        if sig > 0 {
            crate::crash_signal::release_own_marker();
            die_now(sig);
        }
    }

    /// The graceful exit's removal ([`super`]): what [`publish`] recorded, by the rule the signal handler uses.
    pub(crate) fn release(_plan: &crate::control_auth::SocketPlan) {
        if let Some(published) = PUBLISHED.get() {
            published.remove();
        }
    }

    /// This instance rebound its control socket at the published path: `previous` was replaced by `current`, a socket it bound itself (`control_listener::rebind_socket`), both `(device, inode)`.
    pub(crate) fn rebound(previous: (u64, u64), current: (u64, u64)) -> bool {
        PUBLISHED
            .get()
            .is_some_and(|published| published.rebound(previous, current))
    }

    /// Record what `plan` published and arm the handlers.
    pub(crate) fn publish(plan: &crate::control_auth::SocketPlan) {
        if PUBLISHED.set(Published::capture(plan)).is_err() {
            return;
        }
        install_exit_handlers();
    }

    /// [`on_exit_signal`] for each of [`EXIT_SIGNALS`] whose disposition is still the
    /// default: an ignored one stays ignored, and one already this handler (the
    /// orderly quit's arm ran first) is left as it is.
    fn install_exit_handlers() {
        for sig in EXIT_SIGNALS {
            // SAFETY: both `sigaction` structs are zeroed POD; the first call only reads the disposition, the second installs a valid `extern "C"` handler with an empty mask.
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

/// Nothing to record off Unix: no terminating signal to hook, and no file identity read.
#[cfg(not(unix))]
pub(crate) fn publish(_plan: &crate::control_auth::SocketPlan) {
    // The endpoint is a named file the next instance's stale sweep reclaims.
}

/// Off Unix no terminating signal quits the instance: nothing to die of.
#[cfg(not(unix))]
pub(crate) fn die_of_quit_signal() {}

/// The graceful exit's removal off Unix: by path ([`crate::control_auth::cleanup_socket`]).
#[cfg(not(unix))]
pub(crate) fn release(plan: &crate::control_auth::SocketPlan) {
    crate::control_auth::cleanup_socket(plan);
}

#[cfg(all(test, unix))]
mod tests {
    use std::path::PathBuf;

    use super::imp::{Owned, Published, claim_quit, hold_for_exec};

    /// The handler's removal removes the published socket, token and alias. Negative
    /// controls: a socket another listener rebound, a token replaced by rename, and an
    /// alias a newer instance repointed all stay.
    #[test]
    fn a_terminating_signal_removes_only_what_this_instance_still_owns() {
        let scratch = crate::test_scratch::short_dir("the own test's socket");
        let dir = scratch.path().to_path_buf();
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
    }

    /// A rebind at the same path renames a fresh socket over the published one, and
    /// the exit removes the fresh one.
    #[test]
    fn the_exit_removes_the_socket_a_rebind_put_at_the_same_path() {
        use crate::control_listener::{rebind_socket, socket_file_id};
        let scratch = crate::test_scratch::short_dir("the rebind test's socket");
        let dir = scratch.path().to_path_buf();
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
    }

    /// With no record (nothing published by this process), the loop's exit
    /// ([`super::release`]) deletes nothing by path: another instance's socket and
    /// token at the same explicit path stay.
    #[test]
    fn the_graceful_exit_removes_nothing_it_did_not_record() {
        let scratch = crate::test_scratch::short_dir("the grace test's socket");
        let dir = scratch.path().to_path_buf();
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
        Published::capture(&plan).remove();
        assert!(!sock.exists() && !token.exists());
    }

    /// A COLD `exec` AND A TERMINATING SIGNAL NEVER BOTH WIN THE ONE CLAIM: a quit a
    /// signal claimed refuses the `exec` (the new image would drop that quit, and the
    /// instance would carry on under the new build), and a signal while an `exec` holds
    /// the claim is refused the orderly path (its handler ends the process at once).
    #[test]
    fn a_cold_exec_and_a_quit_signal_never_both_take_the_claim() {
        use std::sync::atomic::{AtomicI32, Ordering};
        let state = AtomicI32::new(0);
        {
            let held = hold_for_exec(&state);
            assert!(held.is_some(), "nothing claimed: the exec may run");
            assert!(
                !claim_quit(&state, libc::SIGTERM),
                "a signal during the exec takes the immediate path"
            );
            assert!(hold_for_exec(&state).is_none(), "one exec holds it");
        }
        assert!(
            claim_quit(&state, libc::SIGTERM),
            "the exec returned: the next signal's quit is orderly"
        );
        assert!(
            hold_for_exec(&state).is_none(),
            "a claimed quit refuses the exec"
        );
        assert_eq!(
            state.load(Ordering::Acquire),
            libc::SIGTERM,
            "and keeps its signal"
        );
        assert!(
            !claim_quit(&state, libc::SIGINT),
            "a second signal is not claimed"
        );
    }
}
