// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Signal handling and process-group forwarding for release steps.

#[cfg(unix)]
mod imp {
    use std::io;
    use std::sync::Arc;
    use std::sync::atomic::{AtomicBool, AtomicI32, AtomicU32, Ordering};

    use crate::release::BranchStop;

    pub const SIGHUP: i32 = 1;
    pub const SIGINT: i32 = 2;
    pub const SIGKILL: i32 = 9;
    pub const SIGTERM: i32 = 15;
    /// Private old-image/new-image signal-mask description. Its value is `v1:<bits>`, where a
    /// set bit says that HUP, INT or TERM respectively was already blocked before the old image
    /// blocked all three for exec.
    pub(crate) const EXEC_MASK_ENV: &str = "ATERM_RELEASE_EXEC_SIGNAL_MASK";
    const EXEC_SIGNALS: [i32; 3] = [SIGHUP, SIGINT, SIGTERM];
    #[cfg(any(test, debug_assertions))]
    pub(crate) const EXEC_PROBE_STAGE_ENV: &str = "ATERM_RELEASE_SIGNAL_EXEC_STAGE";
    #[cfg(any(test, debug_assertions))]
    pub(crate) const EXEC_PROBE_REFUSE_ENV: &str = "ATERM_RELEASE_SIGNAL_EXEC_REFUSE";
    #[cfg(any(test, debug_assertions))]
    pub(crate) const EXEC_PROBE_PID_ENV: &str = "ATERM_RELEASE_SIGNAL_EXEC_PID";

    static SIGNAL: AtomicI32 = AtomicI32::new(0);
    static COUNT: AtomicU32 = AtomicU32::new(0);
    static EXEC_HANDOFF_INSTALLED: AtomicBool = AtomicBool::new(false);

    unsafe extern "C" {
        fn kill(pid: i32, sig: i32) -> i32;
        fn sigismember(set: *const libc::sigset_t, sig: i32) -> i32;
    }

    extern "C" fn on_signal(sig: i32) {
        let _ = SIGNAL.compare_exchange(0, sig, Ordering::SeqCst, Ordering::SeqCst);
        COUNT.fetch_add(1, Ordering::SeqCst);
    }

    fn try_install() -> io::Result<()> {
        for sig in EXEC_SIGNALS {
            // SAFETY: a null new action only reads `sig`'s disposition into the live `old`.
            let mut old: libc::sigaction = unsafe { std::mem::zeroed() };
            if unsafe { libc::sigaction(sig, std::ptr::null(), &raw mut old) } != 0 {
                return Err(io::Error::last_os_error());
            }
            if old.sa_sigaction == libc::SIG_IGN {
                continue;
            }
            // Querying before installing avoids the old signal(SIG_IGN) probe, which could
            // discard a signal held pending across exec before the handler was in place.
            // SAFETY: `on_signal` lives for the process, touches only lock-free atomics, and the
            // native set is initialized before sigaction reads the complete value.
            let installed = unsafe {
                let mut action: libc::sigaction = std::mem::zeroed();
                action.sa_sigaction = on_signal as extern "C" fn(libc::c_int) as libc::sighandler_t;
                action.sa_flags = libc::SA_RESTART;
                if libc::sigemptyset(&raw mut action.sa_mask) != 0 {
                    return Err(io::Error::last_os_error());
                }
                libc::sigaction(sig, &raw const action, std::ptr::null_mut())
            };
            if installed != 0 {
                return Err(io::Error::last_os_error());
            }
        }
        Ok(())
    }

    /// Catches `SIGINT`, `SIGTERM`, and `SIGHUP` unless already ignored.
    pub fn install() {
        let _ = try_install();
    }

    fn signal_set(signals: impl IntoIterator<Item = i32>) -> io::Result<libc::sigset_t> {
        // SAFETY: sigemptyset initializes the whole live set, then sigaddset adds only the three
        // valid signal numbers this module supplies.
        unsafe {
            let mut set: libc::sigset_t = std::mem::zeroed();
            if libc::sigemptyset(&raw mut set) != 0 {
                return Err(io::Error::last_os_error());
            }
            for signal in signals {
                if libc::sigaddset(&raw mut set, signal) != 0 {
                    return Err(io::Error::last_os_error());
                }
            }
            Ok(set)
        }
    }

    fn member(set: &libc::sigset_t, signal: i32) -> io::Result<bool> {
        // SAFETY: sigismember reads the initialized set and a valid signal number.
        match unsafe { sigismember(set, signal) } {
            0 => Ok(false),
            1 => Ok(true),
            _ => Err(io::Error::last_os_error()),
        }
    }

    /// The old image's temporary signal block. A failed exec drops it and restores the exact
    /// calling-thread mask; a successful exec replaces the image, so only the encoded handoff
    /// can authorize the new image's selective restore.
    pub struct ExecBlock {
        previous: libc::sigset_t,
    }

    impl Drop for ExecBlock {
        fn drop(&mut self) {
            // SAFETY: `previous` was written whole by pthread_sigmask for this same thread.
            unsafe {
                libc::pthread_sigmask(
                    libc::SIG_SETMASK,
                    &raw const self.previous,
                    std::ptr::null_mut(),
                );
            }
        }
    }

    /// Arm `command` to inherit the current mask, then block HUP/INT/TERM until it either fails
    /// and this guard drops or succeeds and the new image calls [`install_exec_handoff`].
    ///
    /// # Errors
    /// The native exec cannot preserve a mask, or a signal-set operation fails.
    pub fn block_for_exec(command: &mut aterm_uds::spawnfd::Command) -> Result<ExecBlock, String> {
        command
            .inherit_exec_signal_mask()
            .map_err(|error| format!("arm exec signal mask: {error}"))?;
        let signals = signal_set(EXEC_SIGNALS).map_err(|error| error.to_string())?;
        // SAFETY: both sets are live; pthread_sigmask writes all of `previous` while atomically
        // adding the initialized three-signal set to this thread's mask.
        let mut previous: libc::sigset_t = unsafe { std::mem::zeroed() };
        let rc = unsafe {
            libc::pthread_sigmask(libc::SIG_BLOCK, &raw const signals, &raw mut previous)
        };
        if rc != 0 {
            return Err(io::Error::from_raw_os_error(rc).to_string());
        }
        let block = ExecBlock { previous };
        let mut bits = 0u8;
        for (index, signal) in EXEC_SIGNALS.into_iter().enumerate() {
            if member(&block.previous, signal).map_err(|error| error.to_string())? {
                bits |= 1 << index;
            }
        }
        command.env(EXEC_MASK_ENV, format!("v1:{bits}"));
        Ok(block)
    }

    /// Install this image's handlers while all handoff signals remain blocked, then unblock only
    /// those the old image newly blocked. A malformed/incomplete handoff restores nothing.
    ///
    /// # Errors
    /// The private description is absent/malformed, the inherited mask is not fully blocked, or
    /// handler/mask installation fails.
    pub fn install_exec_handoff() -> Result<(), String> {
        let encoded = std::env::var_os(EXEC_MASK_ENV);
        // SAFETY: the frozen cutter calls this at single-threaded startup, before callbacks or
        // worker threads, and the private value is consumed exactly once.
        unsafe { std::env::remove_var(EXEC_MASK_ENV) };
        let encoded = encoded
            .as_deref()
            .and_then(std::ffi::OsStr::to_str)
            .ok_or("the exec signal-mask handoff is absent or non-UTF-8")?;
        let raw = encoded
            .strip_prefix("v1:")
            .ok_or("the exec signal-mask handoff has an unknown schema")?;
        if raw.is_empty()
            || (raw.len() > 1 && raw.starts_with('0'))
            || !raw.bytes().all(|byte| byte.is_ascii_digit())
        {
            return Err("the exec signal-mask handoff has a non-canonical bit set".into());
        }
        let bits: u8 = raw
            .parse()
            .ok()
            .filter(|bits| bits & !0b111 == 0)
            .ok_or("the exec signal-mask handoff names unknown signals")?;

        // SAFETY: a null set reads the whole current mask into `current` without changing it.
        let mut current: libc::sigset_t = unsafe { std::mem::zeroed() };
        let rc =
            unsafe { libc::pthread_sigmask(libc::SIG_BLOCK, std::ptr::null(), &raw mut current) };
        if rc != 0 {
            return Err(format!(
                "read inherited signal mask: {}",
                io::Error::from_raw_os_error(rc)
            ));
        }
        for signal in EXEC_SIGNALS {
            if !member(&current, signal).map_err(|error| error.to_string())? {
                return Err("the exec signal-mask handoff arrived with a signal unblocked".into());
            }
        }

        try_install().map_err(|error| format!("install exec signal handlers: {error}"))?;
        let newly_blocked = signal_set(
            EXEC_SIGNALS
                .into_iter()
                .enumerate()
                .filter_map(|(index, signal)| (bits & (1 << index) == 0).then_some(signal)),
        )
        .map_err(|error| error.to_string())?;
        // SAFETY: `newly_blocked` is initialized; this restores only the handoff's additions.
        let rc = unsafe {
            libc::pthread_sigmask(
                libc::SIG_UNBLOCK,
                &raw const newly_blocked,
                std::ptr::null_mut(),
            )
        };
        if rc != 0 {
            return Err(format!(
                "restore exec signal mask: {}",
                io::Error::from_raw_os_error(rc)
            ));
        }
        EXEC_HANDOFF_INSTALLED.store(true, Ordering::SeqCst);
        Ok(())
    }

    /// Whether this image installed handlers and restored a valid blocked exec handoff.
    #[must_use]
    pub fn exec_handoff_installed() -> bool {
        EXEC_HANDOFF_INSTALLED.load(Ordering::SeqCst)
    }

    /// Debug-only single-threaded executable probe for the real blocked-signal exec boundary.
    /// The integration test starts the binary in `actor`; it becomes `observer` in place.
    #[cfg(any(test, debug_assertions))]
    pub fn exec_probe_if_asked() -> Option<Result<(), String>> {
        let stage = std::env::var(EXEC_PROBE_STAGE_ENV).ok()?;
        let refused = std::env::var_os(EXEC_PROBE_REFUSE_ENV).is_some();
        let blocked = |signal| -> Result<bool, String> {
            // SAFETY: a null set reads the current thread's complete mask into `mask`.
            let mut mask: libc::sigset_t = unsafe { std::mem::zeroed() };
            let rc =
                unsafe { libc::pthread_sigmask(libc::SIG_BLOCK, std::ptr::null(), &raw mut mask) };
            if rc != 0 {
                return Err(io::Error::from_raw_os_error(rc).to_string());
            }
            member(&mask, signal).map_err(|error| error.to_string())
        };
        let result = (|| -> Result<(), String> {
            match stage.as_str() {
                "observer" => {
                    let same_pid = std::env::var(EXEC_PROBE_PID_ENV)
                        .map_err(|error| error.to_string())
                        .and_then(|pid| {
                            (pid == std::process::id().to_string())
                                .then_some(())
                                .ok_or("native exec changed the pid".into())
                        });
                    same_pid?;
                    let restored = install_exec_handoff();
                    if refused {
                        if restored.is_ok() {
                            return Err("a malformed signal handoff was accepted".into());
                        }
                        if !blocked(SIGTERM).unwrap_or(false) {
                            return Err("a refused signal handoff restored TERM".into());
                        }
                        println!("signal-exec-refused-malformed-handoff");
                        Ok(())
                    } else {
                        restored?;
                        if !blocked(SIGHUP)? || blocked(SIGINT)? || blocked(SIGTERM)? {
                            return Err(
                                "exec signal restore did not preserve only the old HUP block"
                                    .into(),
                            );
                        }
                        let bound = std::time::Instant::now() + std::time::Duration::from_secs(60);
                        while caught().is_none() {
                            if std::time::Instant::now() >= bound {
                                return Err("queued TERM was lost across exec".into());
                            }
                            std::thread::sleep(std::time::Duration::from_millis(10));
                        }
                        if caught() != Some(SIGTERM) {
                            return Err("the exec handoff caught a signal other than TERM".into());
                        }
                        println!("signal-exec-caught-queued-term-after-handler-install");
                        Ok(())
                    }
                }
                "actor" => {
                    // HUP, INT and TERM start at their default actions, set here, not inherited
                    // from the test's runner: a probe started ignoring TERM (a `nohup` or `&`
                    // runner's choice, which every exec keeps) discards the TERM it queues, and
                    // the observer's `install` keeps an ignored one ignored.
                    for sig in EXEC_SIGNALS {
                        // SAFETY: SIG_DFL is a disposition, no handler of ours.
                        unsafe { libc::signal(sig, libc::SIG_DFL) };
                    }
                    let hup = signal_set([SIGHUP]).map_err(|error| error.to_string())?;
                    // SAFETY: `hup` is initialized and changes only this single-threaded startup.
                    let rc = unsafe {
                        libc::pthread_sigmask(libc::SIG_BLOCK, &raw const hup, std::ptr::null_mut())
                    };
                    if rc != 0 {
                        return Err(io::Error::from_raw_os_error(rc).to_string());
                    }
                    let mut observer = aterm_uds::spawnfd::command(
                        std::env::current_exe().map_err(|e| e.to_string())?,
                    );
                    observer
                        .env(EXEC_PROBE_STAGE_ENV, "observer")
                        .env(EXEC_PROBE_PID_ENV, std::process::id().to_string());
                    if refused {
                        observer.env(EXEC_PROBE_REFUSE_ENV, "1");
                    }
                    let _handoff = block_for_exec(&mut observer)?;
                    if refused {
                        observer.env(EXEC_MASK_ENV, "broken");
                    }
                    // SAFETY: this is the probe's own live process and TERM is blocked in its only
                    // thread, so the signal becomes kernel-pending until the observer restores it.
                    if unsafe { libc::kill(std::process::id().cast_signed(), SIGTERM) } != 0 {
                        return Err(io::Error::last_os_error().to_string());
                    }
                    if caught().is_some() {
                        return Err("TERM ran before the exec boundary".into());
                    }
                    Err(format!("signal-handoff exec returned: {}", observer.exec()))
                }
                other => Err(format!("unknown signal exec probe stage {other}")),
            }
        })();
        Some(result)
    }

    /// The signal that interrupted the release, if one did.
    pub fn caught() -> Option<i32> {
        let sig = SIGNAL.load(Ordering::SeqCst);
        (sig != 0).then_some(sig)
    }

    /// Sends `sig` to `pid`.
    pub fn send(pid: i32, sig: i32) {
        // SAFETY: `kill` takes two integers and touches no memory of ours.
        unsafe {
            kill(pid, sig);
        }
    }

    /// How long after the send of the first signal to a running step's group ended a later caught
    /// signal must be found to kill that group ([`Stop`]). One stop reaches the release along
    /// several paths within milliseconds: a closed terminal's `SIGHUP` or a `kill -TERM -<group>`
    /// reaches the cutter directly and again from the `targo` that started it, which passes a
    /// `SIGHUP` or `SIGTERM` on to its cutter within a millisecond (store targo 15698, measured).
    /// The first send lists every process before it sends ([`super::listing`]), and a copy can
    /// come while that runs: a signal caught before that send ended, or found sooner than this
    /// after it, is taken for such a copy; a person's second stop comes later.
    pub const ESCALATE_AFTER: std::time::Duration = std::time::Duration::from_secs(1);

    /// What a running step's group is sent ([`Stop::next`]).
    #[derive(Debug, Clone, Copy, PartialEq, Eq)]
    pub enum Pass {
        /// The signal that interrupted the release.
        First,
        /// `SIGKILL`.
        Kill,
    }

    /// The signals a running step's group was sent for those this process caught, counted while
    /// that step runs.
    #[derive(Debug, Default)]
    pub struct Stop {
        /// When the group was sent the first signal: when that send ended, once [`Stop::sent`]
        /// said so, and until then when it was answered.
        first: Option<std::time::Instant>,
        /// The signals this process had caught when last counted ([`Stop::next`],
        /// [`Stop::sent`]).
        counted: u32,
    }

    impl Stop {
        /// `caught`, the signals this process has caught, as found at `now`: [`Pass::First`] for
        /// the first found, and [`Pass::Kill`] for more found at least [`ESCALATE_AFTER`] after
        /// the first send ended ([`Stop::sent`]). None when nothing was caught since they were
        /// last counted, or what was is found sooner: copies of the first stop, counted, so a
        /// later signal is a stop of its own.
        pub fn next(&mut self, caught: u32, now: std::time::Instant) -> Option<Pass> {
            if caught <= self.counted {
                return None;
            }
            self.counted = caught;
            match self.first {
                None => {
                    self.first = Some(now);
                    Some(Pass::First)
                }
                Some(at) if now.saturating_duration_since(at) >= ESCALATE_AFTER => Some(Pass::Kill),
                Some(_) => None,
            }
        }

        /// The send that [`Pass::First`] answered ended at `at`, by when this process had caught
        /// `caught` signals: each of those is the first stop (a copy caught while that send ran
        /// counts with it), and [`ESCALATE_AFTER`] runs from `at`.
        pub fn sent(&mut self, caught: u32, at: std::time::Instant) {
            self.first = Some(at);
            self.counted = self.counted.max(caught);
        }
    }

    /// Guard that forwards caught signals to a running step's process group.
    pub struct Running {
        done: Arc<AtomicBool>,
        thread: Option<std::thread::JoinHandle<()>>,
        _credentials: crate::credential_wait::Watch,
    }

    impl Running {
        /// Starts forwarding caught signals for the process group led by `leader`.
        pub fn enter(leader: u32) -> Self {
            Self::forwarding(leader, None)
        }

        /// [`Running::enter`] for a qualify branch's child, which its branch's stop ends too:
        /// once `stop` fires the group is sent `sig`, and [`BranchStop::GRACE`] later `SIGKILL`.
        pub fn enter_stoppable(leader: u32, stop: BranchStop, sig: i32) -> Self {
            Self::forwarding(leader, Some((stop, sig)))
        }

        fn forwarding(leader: u32, stop: Option<(BranchStop, i32)>) -> Self {
            let done = Arc::new(AtomicBool::new(false));
            let thread = i32::try_from(leader).ok().map(|group| {
                let done = Arc::clone(&done);
                std::thread::spawn(move || forward(group, &done, stop.as_ref()))
            });
            Self {
                done,
                thread,
                _credentials: crate::credential_wait::Watch::enter(leader),
            }
        }
    }

    impl Drop for Running {
        fn drop(&mut self) {
            self.done.store(true, Ordering::SeqCst);
            if let Some(thread) = self.thread.take() {
                let _ = thread.join();
            }
        }
    }

    /// Every 50 ms until `done`, sends `group` what [`Stop`] answers for the signals caught so
    /// far, read then: the signal that interrupted the release, or `SIGKILL` ([`forward_with`]).
    /// A branch's fired `stop` counts as one more signal caught, and as a second once
    /// [`BranchStop::GRACE`] passed, so it is answered as a person's stop and then a second one;
    /// with no signal caught, the group is sent the branch's.
    fn forward(group: i32, done: &AtomicBool, stop: Option<&(BranchStop, i32)>) {
        let active = || !done.load(Ordering::SeqCst);
        let stopped = || {
            stop.and_then(|(stop, _)| stop.fired_for())
                .map_or(0, |since| if since >= BranchStop::GRACE { 2 } else { 1 })
        };
        forward_with(&Forwarding {
            active: &active,
            caught: &|| COUNT.load(Ordering::SeqCst) + stopped(),
            signal: &|| match SIGNAL.load(Ordering::SeqCst) {
                0 => stop.map_or(SIGTERM, |(_, sig)| *sig),
                caught => caught,
            },
            clock: &std::time::Instant::now,
            send: &|sig| signal_group(group, sig, &active),
            pause: &|| {
                std::thread::sleep(std::time::Duration::from_millis(50));
                !done.load(Ordering::SeqCst)
            },
        });
    }

    /// What [`forward_with`] reads, sends and waits by.
    pub(super) struct Forwarding<'a> {
        /// The step still owns forwarding; false once its guard starts closing.
        pub(super) active: &'a dyn Fn() -> bool,
        /// The signals this process has caught ([`COUNT`]).
        pub(super) caught: &'a dyn Fn() -> u32,
        /// The signal that interrupted the release ([`SIGNAL`]).
        pub(super) signal: &'a dyn Fn() -> i32,
        /// The time now.
        pub(super) clock: &'a dyn Fn() -> std::time::Instant,
        /// Sends the step's group a signal, returning once it is sent.
        pub(super) send: &'a dyn Fn(i32),
        /// Waits until the next look; false once the step ended.
        pub(super) pause: &'a dyn Fn() -> bool,
    }

    /// While active, looks at the signals caught, then pauses, until a pause answers false.
    /// Activity is checked again before dispatch, so cancellation during a look sends nothing.
    /// A look sends the
    /// group what [`Stop::next`] answers for them at the time then: for [`Pass::First`] the
    /// signal that interrupted the release, after which it tells [`Stop::sent`] the signals
    /// caught and the time once that send returned; for [`Pass::Kill`] `SIGKILL`.
    pub(super) fn forward_with(f: &Forwarding<'_>) {
        let mut stop = Stop::default();
        while (f.active)() {
            match stop.next((f.caught)(), (f.clock)()) {
                Some(Pass::First) => {
                    let sig = (f.signal)();
                    if !(f.active)() {
                        return;
                    }
                    (f.send)(sig);
                    stop.sent((f.caught)(), (f.clock)());
                }
                Some(Pass::Kill) => {
                    if !(f.active)() {
                        return;
                    }
                    (f.send)(SIGKILL);
                }
                None => {}
            }
            if !(f.pause)() {
                return;
            }
        }
    }

    /// Sends `sig` to each member of `group` that [`super::signalled`] names in one listing of
    /// every process, read in this process with no program run ([`super::listing`]), this
    /// process as the home each member's climb ends at: every member but a trustd and what
    /// descends from one, and but a member whose parent below this process the listing lacks
    /// (Linux: a `stat` that ended or did not read, `hidepid`), which could be a trustd. SIGKILL
    /// is sent twice, every process listed again between, for what forked meanwhile. A member
    /// the listing leaves out (Linux under `hidepid`) is sent nothing, the leader too: on macOS,
    /// which lists a zombie, a leader the listing lacks was reaped, and its pid may be another
    /// process's. With no listing the group's leader alone is sent it, the step's program this
    /// process started.
    /// Cancellation is checked before listing and before every actual send, including after a
    /// listing that outlived the forwarding guard.
    pub(super) fn signal_group(group: i32, sig: i32, active: &dyn Fn() -> bool) {
        signal_group_with(group, sig, active, &super::listing);
    }

    /// [`signal_group`] with the process-table read supplied by its caller. The seam keeps the
    /// real signal-delivery test independent of a legitimately unavailable bounded table read.
    pub(super) fn signal_group_with(
        group: i32,
        sig: i32,
        active: &dyn Fn() -> bool,
        listing: &dyn Fn() -> Option<Vec<super::Proc>>,
    ) {
        let home = i32::try_from(std::process::id()).unwrap_or(0);
        let passes = if sig == SIGKILL { 2 } else { 1 };
        for _ in 0..passes {
            if !active() {
                return;
            }
            let members =
                listing().map_or_else(|| vec![group], |rows| super::signalled(&rows, group, home));
            for pid in members {
                if !active() {
                    return;
                }
                send(pid, sig);
            }
        }
    }
}

#[cfg(not(unix))]
mod imp {
    use crate::release::BranchStop;
    pub(crate) const EXEC_MASK_ENV: &str = "ATERM_RELEASE_EXEC_SIGNAL_MASK";
    pub const SIGINT: i32 = 2;
    pub const SIGTERM: i32 = 15;
    pub fn install() {}
    #[cfg(any(test, debug_assertions))]
    pub fn exec_probe_if_asked() -> Option<Result<(), String>> {
        None
    }
    pub fn install_exec_handoff() -> Result<(), String> {
        Err("native exec signal handoff requires Unix".into())
    }
    pub fn exec_handoff_installed() -> bool {
        false
    }
    pub fn caught() -> Option<i32> {
        None
    }
    pub struct Running;
    impl Running {
        pub fn enter_stoppable(_leader: u32, _stop: BranchStop, _sig: i32) -> Self {
            Self
        }
        pub fn enter(_leader: u32) -> Self {
            Self
        }
    }
}

pub use imp::*;

/// One process as a listing of every process reads it ([`listing`]), for [`signalled`].
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Proc {
    /// Its pid.
    pub pid: i32,
    /// Its parent's pid: 0 for the kernel's own, 1 once launchd or init adopted it.
    pub parent: i32,
    /// Its process group.
    pub pgid: i32,
    /// The kernel's name for it: on macOS `p_comm`, the name of the file it last exec'd (a
    /// symlink's target's), at most 16 bytes; on Linux `comm`, the name it was exec'd by, at most
    /// 15 bytes, which the process may change.
    pub name: String,
}

/// Whether `name`, a [`Proc::name`], is a trustd's: its last `/`-separated part is `trustd`.
/// On macOS that name is the exec'd file's, so a trustd started through a symlink of another
/// name is not recognised: its argv[0] is not read, because reading a program's arguments can
/// hang while macOS holds it at its start, the hang this listing avoids.
#[must_use]
pub fn is_trustd(name: &str) -> bool {
    name.rsplit('/').next() == Some("trustd")
}

/// The members of process group `group` to signal, in `rows`' order, `rows` being one listing of
/// every process ([`listing`]): each row in `group` whose climb through `rows`' parents to pid 1
/// or below, that pid judged too, meets no trustd ([`is_trustd`]), itself included. Below
/// `home` (the process that started the step), a parent `rows` lacks spares the member, and so
/// does a climb longer than `rows` (pids reused into a loop): a process that was not read could
/// be a trustd. From `home` up, either ends the climb as no trustd: a listing can lack `home`'s
/// own parents (on Linux `hidepid` hides another user's, such as the `sshd` above a session),
/// and taking one for a trustd would spare every member `home` started.
#[must_use]
pub fn signalled(rows: &[Proc], group: i32, home: i32) -> Vec<i32> {
    /// Where a climb from one process ends.
    #[derive(PartialEq)]
    enum End {
        /// At a trustd.
        Trustd,
        /// At `home`, not judged.
        Home,
        /// At pid 1 or below.
        Top,
        /// At a parent `rows` lacks, or after more steps than `rows` has rows.
        Unread,
    }
    let by_pid: std::collections::HashMap<i32, &Proc> = rows.iter().map(|p| (p.pid, p)).collect();
    let climb = |from: &Proc| {
        let mut at = from;
        for _ in 0..=rows.len() {
            if is_trustd(&at.name) {
                return End::Trustd;
            }
            if at.parent <= 1 {
                // pid 1 (or the kernel's 0) is judged too: a trustd there spares what it runs.
                return if by_pid
                    .get(&at.parent)
                    .is_some_and(|top| is_trustd(&top.name))
                {
                    End::Trustd
                } else {
                    End::Top
                };
            }
            if at.parent == home {
                return End::Home;
            }
            match by_pid.get(&at.parent) {
                Some(up) => at = up,
                None => return End::Unread,
            }
        }
        End::Unread
    };
    let home_clear = by_pid.get(&home).is_none_or(|h| climb(h) != End::Trustd);
    rows.iter()
        .filter(|p| {
            p.pgid == group
                && match climb(p) {
                    End::Top => true,
                    End::Home => home_clear,
                    End::Trustd | End::Unread => false,
                }
        })
        .map(|p| p.pid)
        .collect()
}

/// Every process, read in this process (no program runs): macOS's process table in one
/// `KERN_PROC_ALL` read ([`aterm_sysprobe::kproc::records`]), every user's, an unreaped zombie
/// among them. A process its kill is still tearing down stays listed, its children under it,
/// until it is a zombie and they are launchd's (measured: a SIGKILLed 4 GB `perl`, which
/// `proc_pidinfo` could no longer read, stayed so for its 60 ms teardown). `None` when the
/// table cannot be read.
#[cfg(target_os = "macos")]
#[must_use]
pub fn listing() -> Option<Vec<Proc>> {
    aterm_sysprobe::kproc::records()?
        .into_iter()
        .map(|r| {
            Some(Proc {
                pid: i32::try_from(r.pid).ok()?,
                parent: i32::try_from(r.ppid).ok()?,
                pgid: r.pgid,
                name: r.comm,
            })
        })
        .collect()
}

/// See the macOS twin: each `/proc/<pid>/stat` ([`stat_proc`]), read one after another, so a
/// process that ends mid-listing, or whose `stat` does not read, is left out, and so, under a
/// `/proc` mounted with `hidepid`, are another user's processes and this user's that it may not
/// trace. `None` when `/proc` cannot be listed.
#[cfg(any(target_os = "linux", target_os = "android"))]
#[must_use]
pub fn listing() -> Option<Vec<Proc>> {
    let rows = std::fs::read_dir("/proc").ok()?.filter_map(|entry| {
        let entry = entry.ok()?;
        let pid = entry
            .file_name()
            .to_str()
            .filter(|s| !s.is_empty() && s.bytes().all(|b| b.is_ascii_digit()))?
            .parse::<i32>()
            .ok()?;
        let stat = aterm_uds::process::read_proc_text(&entry.path().join("stat")).ok()?;
        stat_proc(&stat, pid)
    });
    Some(rows.collect())
}

/// See the macOS twin: no reader is wired here.
#[cfg(not(any(target_os = "macos", target_os = "linux", target_os = "android")))]
#[must_use]
pub fn listing() -> Option<Vec<Proc>> {
    None
}

/// `pid`'s row from `stat`, its `/proc/<pid>/stat` text
/// ([`aterm_sysprobe::jobs::linux::parse_stat`]); `None` when the text is malformed or names
/// another pid.
#[cfg(any(target_os = "linux", target_os = "android", test))]
fn stat_proc(stat: &str, pid: i32) -> Option<Proc> {
    let named = stat.split_once(" (")?.0.trim();
    if named.is_empty()
        || !named.bytes().all(|b| b.is_ascii_digit())
        || named.parse::<i32>().ok() != Some(pid)
    {
        return None;
    }
    let line = aterm_sysprobe::jobs::linux::parse_stat(stat)?;
    Some(Proc {
        pid,
        parent: i32::try_from(line.ppid).ok()?,
        pgid: i32::try_from(line.pgrp).ok()?,
        name: line.name,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A TERM queued after the old image blocks the handoff set stays pending across a real
    /// native exec, then reaches the new image's handler only after it is installed. HUP, which
    /// was already blocked, stays blocked. NEGATIVE CONTROL: a corrupted private description is
    /// refused and restores nothing, leaving the queued TERM blocked until that probe exits.
    #[cfg(target_vendor = "apple")]
    #[test]
    fn a_queued_signal_survives_exec_and_a_malformed_handoff_restores_nothing() {
        use std::time::Duration;

        for refused in [true, false] {
            let mut actor = aterm_uds::spawnfd::command(aterm_tempfile::bin_exe("aterm-release"));
            actor.env(EXEC_PROBE_STAGE_ENV, "actor");
            if refused {
                actor.env(EXEC_PROBE_REFUSE_ENV, "1");
            }
            let output =
                aterm_uds::deadline::output_within(&mut actor, Duration::from_secs(60)).unwrap();
            assert!(
                output.status.success(),
                "signal exec actor failed ({:?}): stdout={} stderr={}",
                output.status,
                String::from_utf8_lossy(&output.stdout),
                String::from_utf8_lossy(&output.stderr),
            );
            let output = String::from_utf8_lossy(&output.stdout);
            let expected = if refused {
                "signal-exec-refused-malformed-handoff"
            } else {
                "signal-exec-caught-queued-term-after-handler-install"
            };
            assert!(output.contains(expected), "missing {expected}: {output}");
        }
    }

    /// A guard that closed before its worker was scheduled must not read stale signals,
    /// wait, or dispatch to the completed step's former numeric process group.
    #[cfg(unix)]
    #[test]
    fn an_already_closed_forwarder_never_looks_or_sends() {
        forward_with(&Forwarding {
            active: &|| false,
            caught: &|| panic!("a closed step's signal count was read"),
            signal: &|| panic!("a closed step's signal was read"),
            clock: &|| panic!("a closed step's clock was read"),
            send: &|_| panic!("a completed step's old group was signalled"),
            pause: &|| panic!("a closed step waited before checking cancellation"),
        });
    }

    /// Closing while a signal/clock observation is in progress fences either dispatch,
    /// including escalation after a previously sent first stop. No native signal is sent.
    #[cfg(unix)]
    #[test]
    fn cancellation_during_a_look_fences_first_and_escalated_sends() {
        use std::cell::{Cell, RefCell};
        use std::time::{Duration, Instant};
        for cancel_look in [1, 2] {
            let active = Cell::new(true);
            let look = Cell::new(1);
            let sends = RefCell::new(Vec::new());
            let t0 = Instant::now();
            forward_with(&Forwarding {
                active: &|| active.get(),
                caught: &|| {
                    if look.get() == cancel_look {
                        active.set(false);
                    }
                    look.get()
                },
                signal: &|| SIGHUP,
                clock: &|| t0 + Duration::from_secs(u64::from(look.get())),
                send: &|sig| sends.borrow_mut().push(sig),
                pause: &|| {
                    look.set(look.get() + 1);
                    active.get()
                },
            });
            assert_eq!(
                *sends.borrow(),
                if cancel_look == 1 {
                    vec![]
                } else {
                    vec![SIGHUP]
                },
                "the cancelled look must not send its first stop or escalation"
            );
        }
    }

    /// One stop that reaches the release along several paths sends the step's group one signal;
    /// a stop found [`ESCALATE_AFTER`] or more after the first send ended ([`Stop::sent`]) kills
    /// the group, and so does each after it. The clock is the test's: each answer is asked at a
    /// moment it names. NEGATIVE CONTROLS: kill on any signal found after the first, and the
    /// burst of copies kills the group; take only the time from [`Stop::sent`], and a copy caught
    /// while the first send ran, found the interval after it ended, kills the group.
    #[cfg(unix)]
    #[test]
    fn copies_of_one_stop_count_once_and_a_later_stop_kills_the_group() {
        use std::time::{Duration, Instant};
        let t0 = Instant::now();
        let at = |ms: u64| t0 + Duration::from_millis(ms);
        let mut stop = Stop::default();
        assert_eq!(stop.next(0, at(0)), None, "nothing caught");
        assert_eq!(
            stop.next(2, at(50)),
            Some(Pass::First),
            "the first stop, and a copy"
        );
        stop.sent(3, at(400));
        assert_eq!(
            stop.next(5, at(450)),
            None,
            "a burst of copies found after it ended"
        );
        assert_eq!(
            stop.next(6, at(1399)),
            None,
            "a copy found just inside the interval after the send ended"
        );
        assert_eq!(stop.next(6, at(9000)), None, "nothing caught since");
        assert_eq!(
            stop.next(7, at(9000)),
            Some(Pass::Kill),
            "a person's second stop"
        );
        assert_eq!(
            stop.next(9, at(9050)),
            Some(Pass::Kill),
            "and each after it"
        );

        let mut stop = Stop::default();
        assert_eq!(stop.next(1, at(0)), Some(Pass::First));
        stop.sent(2, at(2000));
        let after = at(2000) + ESCALATE_AFTER;
        assert_eq!(
            stop.next(2, after),
            None,
            "a copy caught while the first send ran, found the interval after it ended"
        );
        assert_eq!(
            stop.next(3, after),
            Some(Pass::Kill),
            "a stop found the interval after the first send ended"
        );
    }

    /// The signals [`forward_with`] sends for a run of looks on the test's clock: the first send
    /// takes `first_send_ms` and `copies_in_send` copies are caught while it runs; each pause
    /// lasts its milliseconds and catches its count of signals. Each send is listed with the
    /// milliseconds from the start at which it began.
    #[cfg(unix)]
    fn forwarded(
        first_send_ms: u64,
        copies_in_send: u32,
        pauses: &[(u64, u32)],
    ) -> Vec<(i32, u64)> {
        use std::cell::{Cell, RefCell};
        use std::time::{Duration, Instant};
        let t0 = Instant::now();
        let clock = Cell::new(t0);
        let caught = Cell::new(0u32);
        let sends = RefCell::new(Vec::new());
        let next = Cell::new(0usize);
        let since = |at: Instant| u64::try_from(at.duration_since(t0).as_millis()).unwrap();
        forward_with(&Forwarding {
            active: &|| true,
            caught: &|| caught.get(),
            signal: &|| SIGHUP,
            clock: &|| clock.get(),
            send: &|sig| {
                let first = sends.borrow().is_empty();
                sends.borrow_mut().push((sig, since(clock.get())));
                if first {
                    caught.set(caught.get() + copies_in_send);
                    clock.set(clock.get() + Duration::from_millis(first_send_ms));
                }
            },
            pause: &|| {
                let Some(&(ms, signals)) = pauses.get(next.get()) else {
                    return false;
                };
                next.set(next.get() + 1);
                clock.set(clock.get() + Duration::from_millis(ms));
                caught.set(caught.get() + signals);
                true
            },
        });
        sends.into_inner()
    }

    /// The forwarding loop sends the signal that interrupted the release once for the first stop,
    /// however long that send takes: a copy caught while it runs, or found within
    /// [`ESCALATE_AFTER`] after it ended, sends nothing, and a stop found that long after it
    /// ended sends `SIGKILL`. NEGATIVE CONTROLS: time the interval from the look that found the
    /// first signal (drop [`Stop::sent`]), and the copy found 50 ms after a 2 s send kills the
    /// group; take only the time from [`Stop::sent`], not the copies caught by then, and the copy
    /// caught during the send but found by a thread that looked again 1.5 s after it kills the
    /// group; send nothing for [`Pass::Kill`], and the person's second stop sends nothing; send
    /// `SIGTERM` for the first stop, and the `SIGHUP` that interrupted the release is not sent.
    #[cfg(unix)]
    #[test]
    fn a_copy_caught_during_a_slow_first_send_counts_once_and_a_later_stop_kills() {
        assert_eq!(
            forwarded(2000, 1, &[(50, 1), (50, 1), (1000, 1)]),
            [(SIGHUP, 50), (SIGKILL, 3100)],
            "the stop at 50 ms, a copy caught during its 2 s send, one found 50 ms after it, \
             and a second stop found 1050 ms after it"
        );
        assert_eq!(
            forwarded(2000, 1, &[(50, 1), (1500, 0)]),
            [(SIGHUP, 50)],
            "a copy caught during the send, found 1.5 s after it ended"
        );
        assert_eq!(
            forwarded(0, 0, &[(50, 1), (50, 1), (949, 1), (1, 1)]),
            [(SIGHUP, 50), (SIGKILL, 1050)],
            "a copy 50 ms and one 999 ms after an instant send, then a stop 1000 ms after it"
        );
    }

    /// The rows of a `pid ppid pgid name` table, one process a line; a line that does not read
    /// is skipped.
    fn rows(table: &str) -> Vec<Proc> {
        table
            .lines()
            .filter_map(|line| {
                let mut rest = line.trim_start();
                let mut num = || -> Option<i32> {
                    let end = rest.find(char::is_whitespace)?;
                    let n = rest[..end].parse().ok()?;
                    rest = rest[end..].trim_start();
                    Some(n)
                };
                let (pid, parent, pgid) = (num()?, num()?, num()?);
                Some(Proc {
                    pid,
                    parent,
                    pgid,
                    name: rest.trim_end().to_string(),
                })
            })
            .collect()
    }

    /// The release (400) started the step's group 500; its member 502 is a trustd, and 503 and
    /// 504 its solvers.
    #[test]
    fn the_interrupt_spares_a_trustd_and_its_solvers() {
        let listing = rows(
            "\
            1     0     1 /sbin/launchd\n\
          500   400   500 /bin/sh\n\
          501   500   500 /Users//x/Library/Application Support/aterm/pkg/trust/bin/targo\n\
          502   501   500 /Users//x/Library/Application Support/aterm/pkg/trust/bin/trustd\n\
          503   502   500 z3\n\
          504   503   500 z3-worker\n\
          505   500   500 sleep\n\
          600   500   600 /usr/libexec/trustd\n\
          601     1   777 /bin/sh\n\
          garbage line\n",
        );
        assert_eq!(signalled(&listing, 500, 400), [500, 501, 505]);
        assert_eq!(signalled(&listing, 777, 400), [601]);
        assert!(signalled(&listing, 999, 400).is_empty());
    }

    /// Below the process that started the step (`home`, 10), a parent the listing lacks (20) or
    /// a loop of reused pids (31, 32) spares a member, since what was not read could be a
    /// trustd; from `home` up, one the listing lacks (7, above the shell 8) is no trustd, and a
    /// trustd there spares every member `home` started, as `ps`'s listing did. NEGATIVE
    /// CONTROLS: take a parent the listing lacks below `home` for no trustd, and 21 is
    /// signalled; answer a loop's end with a signal, and 31 and 32 are; take one lacking above
    /// `home` for a trustd, and only the orphan 13 is; end the climb at `home` unjudged, and the
    /// trustd above it spares nothing.
    #[test]
    fn a_climb_spares_what_it_could_not_read_below_home_and_judges_homes_own_parents() {
        let table = |shell: &str| {
            rows(&format!(
                "\
                 8     7     8 {shell}\n\
                10     8    10 aterm-release\n\
                11    10    11 targo\n\
                12    11    11 rustc\n\
                13     1    11 orphan\n\
                21    20    11 cc\n\
                31    32    11 a\n\
                32    31    11 b\n"
            ))
        };
        assert_eq!(
            signalled(&table("zsh"), 11, 10),
            [11, 12, 13],
            "what home started, and an orphan of it"
        );
        assert_eq!(
            signalled(&table("trustd"), 11, 10),
            [13],
            "under a trustd, only the orphan launchd took"
        );
        let mut under_one = table("zsh");
        under_one.push(Proc {
            pid: 1,
            parent: 0,
            pgid: 1,
            name: "trustd".to_string(),
        });
        assert_eq!(
            signalled(&under_one, 11, 10),
            [11, 12],
            "a trustd at pid 1 spares the orphan it took"
        );
    }

    /// `/proc/<pid>/stat` reads as the row of the pid it names, its name between the first `(`
    /// and the last `)`. NEGATIVE CONTROL: drop the pid check, and the text of another pid reads
    /// as this one's.
    #[test]
    fn a_proc_stat_line_reads_as_the_row_of_its_own_pid() {
        let stat = "4242 (a) b (trustd)) S 4200 4100 4100 0 -1 4194560 1 0 0 0 0 0 0 0 20 0 1 0 \
                    123 0 0 18446744073709551615 0 0 0 0 0 0 0 0 0 0 0 0 17 0 0 0 0 0 0";
        assert_eq!(
            stat_proc(stat, 4242),
            Some(Proc {
                pid: 4242,
                parent: 4200,
                pgid: 4100,
                name: "a) b (trustd)".to_string(),
            })
        );
        assert_eq!(stat_proc(stat, 4243), None, "the text of another pid");
        assert_eq!(stat_proc("4242 (sh", 4242), None, "a cut line");
    }

    /// The lines `from` carries, read on a thread: each `Some`, then `None` at its end.
    #[cfg(unix)]
    fn lines(
        from: impl std::io::Read + Send + 'static,
    ) -> std::sync::mpsc::Receiver<Option<String>> {
        use std::io::BufRead as _;
        let (tx, rx) = std::sync::mpsc::channel();
        std::thread::spawn(move || {
            for line in std::io::BufReader::new(from).lines() {
                let Ok(line) = line else { break };
                if tx.send(Some(line)).is_err() {
                    return;
                }
            }
            let _ = tx.send(None);
        });
        rx
    }

    /// The next of `lines`, within the one-minute hang detector.
    #[cfg(unix)]
    #[track_caller]
    fn next(lines: &std::sync::mpsc::Receiver<Option<String>>, what: &str) -> Option<String> {
        lines
            .recv_timeout(std::time::Duration::from_secs(60))
            .unwrap_or_else(|e| panic!("{what}: nothing within the one-minute hang detector: {e}"))
    }

    /// `child`'s exit status, within the one-minute hang detector.
    #[cfg(unix)]
    #[track_caller]
    fn ended(child: &mut aterm_uds::spawnfd::Child, what: &str) -> std::process::ExitStatus {
        let bound = std::time::Instant::now() + std::time::Duration::from_secs(60);
        loop {
            if let Some(status) = child.try_wait().unwrap() {
                return status;
            }
            assert!(
                std::time::Instant::now() < bound,
                "{what} did not end within the one-minute hang detector"
            );
            std::thread::sleep(std::time::Duration::from_millis(20));
        }
    }

    /// A real process group of this test's own: a leader shell, a grandchild shell it started,
    /// and a stand-in named `trustd` (a copy of a shell, joined to the group) running `cat`, its
    /// child. One SIGTERM sent to the group ends the leader by that signal (its wait status) and
    /// the grandchild by its own handler (the line it prints), and their output pipe ends. The
    /// stand-in's `cat` still echoes a line written after the send returned (a SIGTERM sent
    /// before that write would have ended it first), then ends at its input's end with status 0,
    /// and the stand-in exits 0. Each process here ends by itself once this test's pipes close.
    /// NEGATIVE CONTROLS: send the leader alone, and the grandchild never prints `TERM`; judge a
    /// member by its own name only, not climbing, and the trustd's `cat` ends by the signal
    /// (`cat 143`) instead of echoing; take no member for a trustd, and nothing echoes either;
    /// spare what descends from a trustd but not the trustd, and it dies by SIGTERM before it
    /// prints `cat 0`.
    #[cfg(unix)]
    #[test]
    fn a_real_group_is_signalled_but_for_a_trustd_and_what_it_runs() {
        use aterm_uds::spawnfd::{Stdio, command};
        use std::io::Write as _;
        use std::os::unix::process::ExitStatusExt as _;
        let mut kept =
            aterm_uds::deadline::KeptUntilRan::new(crate::test_scratch_dir("interrupt").keep());
        let trustd = kept.path().join("trustd");
        let shell = if cfg!(target_vendor = "apple") {
            "/bin/dash"
        } else {
            "/bin/sh"
        };
        aterm_uds::deadline::ad_hoc_copy(std::path::Path::new(shell), &trustd);
        // The copy shows it runs from its file (macOS has assessed its first exec) by a line it
        // writes, before anything may remove it; one macOS still holds at the hang detector
        // ends the test COULD NOT RUN.
        let mut proof = command(&trustd)
            .args(["-c", "echo ran"])
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::null())
            .spawn()
            .unwrap();
        aterm_uds::deadline::prints(&mut proof, "ran", aterm_uds::deadline::ECHO_HANG);
        let _ = proof.wait();
        kept.ran();
        // The group starts with the signals that end a process at their defaults, set here, not
        // this test binary's: a shell started ignoring SIGTERM (a `nohup` or `&` runner's
        // choice, which every exec keeps) can trap none, and neither ends nor says `TERM`.
        let start = aterm_spec::start::at_defaults(&aterm_spec::start::TERMINATING);
        let mut leader = command("/bin/sh")
            .args([
                "-c",
                "/bin/sh -c 'trap \"echo TERM \\$\\$; exit 0\" TERM; echo up $$; \
                 while kill -0 $PPID 2>/dev/null; do /bin/sleep 1; done' & read -r _",
            ])
            .process_group(0)
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::null())
            .spawn()
            .unwrap();
        drop(start);
        let group = i32::try_from(leader.id()).unwrap();
        let from_group = lines(leader.stdout.take().unwrap());
        let up = next(&from_group, "the grandchild").expect("the grandchild's first line");
        let grandchild = up
            .strip_prefix("up ")
            .expect("the grandchild says it is up");
        let grandchild_pid = grandchild
            .parse::<i32>()
            .expect("the grandchild says its pid");
        let start = aterm_spec::start::at_defaults(&aterm_spec::start::TERMINATING);
        let mut standin = command(&trustd)
            .args(["-c", "echo up; /bin/cat; echo \"cat $?\""])
            .process_group(group)
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::null())
            .spawn()
            .unwrap();
        drop(start);
        let from_standin = lines(standin.stdout.take().unwrap());
        let mut to_standin = standin.stdin.take().unwrap();
        assert_eq!(next(&from_standin, "the stand-in").as_deref(), Some("up"));
        to_standin.write_all(b"before\n").unwrap();
        assert_eq!(
            next(&from_standin, "its cat").as_deref(),
            Some("before"),
            "the trustd's cat runs in the group"
        );

        let home = i32::try_from(std::process::id()).unwrap();
        let standin_pid = i32::try_from(standin.id()).unwrap();
        imp::signal_group_with(group, SIGTERM, &|| true, &|| {
            Some(vec![
                Proc {
                    pid: group,
                    parent: home,
                    pgid: group,
                    name: "sh".to_string(),
                },
                Proc {
                    pid: grandchild_pid,
                    parent: group,
                    pgid: group,
                    name: "sh".to_string(),
                },
                Proc {
                    pid: standin_pid,
                    parent: home,
                    pgid: group,
                    name: "trustd".to_string(),
                },
            ])
        });

        let status = ended(&mut leader, "the leader");
        assert_eq!(
            status.signal(),
            Some(SIGTERM),
            "the leader ends by the signal: {status}"
        );
        assert_eq!(
            next(&from_group, "the grandchild"),
            Some(format!("TERM {grandchild}")),
            "the grandchild ends by its own handler"
        );
        assert_eq!(
            next(&from_group, "the group's pipe"),
            None,
            "and nothing holds the pipe"
        );
        let wrote = to_standin.write_all(b"after\n").map_err(|e| e.kind());
        let echoed = next(&from_standin, "its cat");
        assert_eq!(
            (wrote, echoed.as_deref()),
            (Ok(()), Some("after")),
            "the trustd's cat echoes after the send"
        );
        drop(to_standin);
        assert_eq!(
            next(&from_standin, "its cat's end").as_deref(),
            Some("cat 0"),
            "its cat ends at its input's end, and the stand-in reports it"
        );
        let status = ended(&mut standin, "the stand-in");
        assert_eq!(
            status.code(),
            Some(0),
            "the trustd stand-in exits by itself: {status}"
        );
    }
}
