// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Control-socket endpoint tracking and cleanup on exit.

#[cfg(unix)]
pub(crate) use imp::{
    arm_orderly_quit, die_of_quit_signal, die_of_signal, hold_quit_for_exec, publish,
    quit_signalled, rebound, release,
};

/// How long a terminating signal's orderly quit runs before the process ends anyway
/// ([`arm_orderly_quit`]).
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
        /// Device and inode as `MetadataExt` widens them ([`Owned::follow`]).
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

        /// Record `current` in place of `previous`.
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
            // SAFETY: `stat` is POD, so zeroed is valid, and `lstat` writes it; `self.path` is a
            // live NUL-terminated string; `lstat`/`unlink` are async-signal-safe.
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
            // SAFETY: `buf` is a live, writable stack buffer of the passed length; `readlink`
            // returns the byte count (no NUL), bounds-checked below. `readlink`/`unlink` are
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

    /// Everything the handler removes.
    pub(super) struct Published {
        /// The process that published them.
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

        /// [`Owned::follow`] for the socket.
        pub(super) fn rebound(&self, previous: (u64, u64), current: (u64, u64)) -> bool {
            self.socket
                .as_ref()
                .is_some_and(|socket| socket.follow(previous, current))
        }

        /// Remove every file still this instance's.
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

    /// The write end of the orderly quit's pipe ([`arm_orderly_quit`]).
    static QUIT_PIPE: AtomicI32 = AtomicI32::new(-1);

    /// The process that armed the orderly quit ([`arm_orderly_quit`]).
    static ARMED_PID: AtomicI32 = AtomicI32::new(0);

    /// Terminating signal handed to the orderly quit ([`EXEC_IN_FLIGHT`], [`hold_quit_for_exec`]).
    static QUIT_SIGNAL: AtomicI32 = AtomicI32::new(0);

    /// [`QUIT_SIGNAL`] while a cold `exec` is in flight ([`claim_quit`]).
    const EXEC_IN_FLIGHT: libc::c_int = -1;

    /// The handler's claim on the orderly quit.
    pub(super) fn claim_quit(state: &AtomicI32, sig: libc::c_int) -> bool {
        state
            .compare_exchange(0, sig, Ordering::AcqRel, Ordering::Acquire)
            .is_ok()
    }

    /// A terminating signal holds the orderly quit's claim on `state` ([`claim_quit`]); a cold
    /// `exec`'s hold ([`EXEC_IN_FLIGHT`]) is none.
    pub(super) fn quit_claimed(state: &AtomicI32) -> bool {
        state.load(Ordering::Acquire) > 0
    }

    /// THIS PROCESS IS ENDING OF A TERMINATING SIGNAL: its handler claimed the orderly quit
    /// ([`on_exit_signal`]). The handler sets it as the signal lands, before the quit's thread
    /// (Background QoS, which can wait seconds for a core on a loaded Mac) runs at all.
    pub(crate) fn quit_signalled() -> bool {
        quit_claimed(&QUIT_SIGNAL)
    }

    /// The claim a cold `exec` holds ([`hold_for_exec`]) while it is in flight.
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

    /// THE COLD UPDATE'S `exec` TAKES THE ORDERLY QUIT'S CLAIM FIRST ([`super`]).
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
            // SAFETY: `write(2)` of one byte from a live stack buffer to the pipe's write end,
            // which is
            // never closed; async-signal-safe.
            if unsafe { libc::write(fd, byte.as_ptr().cast(), 1) } == 1 {
                return;
            }
        }
        die_now(sig);
    }

    /// The immediate path.
    fn die_now(sig: libc::c_int) {
        if let Some(published) = PUBLISHED.get() {
            published.remove();
        }
        // SAFETY: a zeroed `sigaction` with `SIG_DFL` restores the default action;
        // `sigaction`/`raise`
        // are async-signal-safe.
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

    /// ARM THE ORDERLY QUIT ([`super`]): `quit` runs on its own thread at the first terminating
    /// signal. A headless instance arms it once its event loop can be asked; a window and the
    /// node job, to give up the account claims before they die of the signal. Once per process:
    /// a later arm does nothing.
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
                    "{name}: quitting in order (a second {name} ends the process at once)"
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
    const fn signal_name(sig: libc::c_int) -> &'static str {
        match sig {
            libc::SIGTERM => "SIGTERM",
            libc::SIGINT => "SIGINT",
            libc::SIGHUP => "SIGHUP",
            _ => "a terminating signal",
        }
    }

    /// At the end of a quit: when a terminating signal asked for it ([`arm_orderly_quit`]), die of
    /// that signal now; else return.
    pub(crate) fn die_of_quit_signal() {
        let sig = QUIT_SIGNAL.load(Ordering::Acquire);
        if sig > 0 {
            crate::crash_signal::release_own_marker();
            die_now(sig);
        }
    }

    /// Die of `sig` now, as the signal's default action ends the process: what [`publish`]
    /// recorded removed, as a signal nothing armed for does ([`on_exit_signal`]), and no clean
    /// exit marked (unlike [`die_of_quit_signal`]), so the next start reads the run as cut short.
    pub(crate) fn die_of_signal(sig: libc::c_int) {
        die_now(sig);
    }

    /// The graceful exit's removal ([`super`]): what [`publish`] recorded, by the rule the
    /// signal handler uses.
    pub(crate) fn release(_plan: &crate::control_auth::SocketPlan) {
        if let Some(published) = PUBLISHED.get() {
            published.remove();
        }
    }

    /// This instance rebound its control socket at the published path.
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

    /// [`on_exit_signal`] for each of [`EXIT_SIGNALS`] whose disposition is still the default.
    fn install_exit_handlers() {
        for sig in EXIT_SIGNALS {
            // SAFETY: both `sigaction` structs are zeroed POD; the first call only reads the
            // disposition, the second installs a valid `extern "C"` handler with an empty mask.
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

/// Nothing to record off Unix.
#[cfg(not(unix))]
pub(crate) fn publish(_plan: &crate::control_auth::SocketPlan) {
    // The endpoint is a named file the next instance's stale sweep reclaims.
}

/// Off Unix no terminating signal quits the instance.
#[cfg(not(unix))]
pub(crate) fn die_of_quit_signal() {}

/// Off Unix no terminating signal quits the instance.
#[cfg(not(unix))]
pub(crate) fn quit_signalled() -> bool {
    false
}

/// The graceful exit's removal off Unix ([`crate::control_auth::cleanup_socket`]).
#[cfg(not(unix))]
pub(crate) fn release(plan: &crate::control_auth::SocketPlan) {
    crate::control_auth::cleanup_socket(plan);
}

#[cfg(all(test, unix))]
#[path = "owned_endpoint_tests.rs"]
mod tests;
