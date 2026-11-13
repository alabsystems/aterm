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
    /// The `SIGTERM`s among [`COUNT`].
    static TERMS: AtomicU32 = AtomicU32::new(0);
    /// The `SIGTERM`s this process sent itself on its own account ([`stop_self`]).
    static OWN: AtomicU32 = AtomicU32::new(0);
    static EXEC_HANDOFF_INSTALLED: AtomicBool = AtomicBool::new(false);

    unsafe extern "C" {
        fn kill(pid: i32, sig: i32) -> i32;
        fn sigismember(set: *const libc::sigset_t, sig: i32) -> i32;
    }

    extern "C" fn on_signal(sig: i32, info: *mut libc::siginfo_t, _: *mut libc::c_void) {
        // SAFETY: installed with SA_SIGINFO; the packet lives throughout this call.
        unsafe { aterm_uds::signal_origin::capture(sig, info) };
        let _ = SIGNAL.compare_exchange(0, sig, Ordering::SeqCst, Ordering::SeqCst);
        // TERMS before COUNT: a reader that loads COUNT first ([`foreign`]) then finds every
        // TERM it counted there in TERMS too, so this process's own stop never reads as a
        // signal from elsewhere, not even while its handler runs.
        if sig == SIGTERM {
            TERMS.fetch_add(1, Ordering::SeqCst);
        }
        COUNT.fetch_add(1, Ordering::SeqCst);
    }

    fn try_install() -> io::Result<()> {
        aterm_uds::signal_origin::start().map_err(io::Error::other)?;
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
            // SAFETY: the SA_SIGINFO handler lives for the process, uses only lock-free
            // atomics and a nonblocking pipe write; its native mask is initialized here.
            let installed = unsafe {
                let mut action: libc::sigaction = std::mem::zeroed();
                action.sa_sigaction = on_signal as *const () as libc::sighandler_t;
                action.sa_flags = libc::SA_RESTART | libc::SA_SIGINFO;
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
        if sig != 0 {
            let _ = aterm_uds::signal_origin::records();
            Some(sig)
        } else {
            None
        }
    }

    /// Sends `sig` to `pid`.
    pub fn send(pid: i32, sig: i32) {
        // SAFETY: `kill` takes two integers and touches no memory of ours.
        unsafe {
            kill(pid, sig);
        }
    }

    /// Sends this process `SIGTERM` on its own account: the release stopping itself (its fleet
    /// line lost to another holder, `line_claim`'s `fail`). It is caught ([`caught`]) and
    /// forwarded to a running step's group ([`Running::enter`]) as a person's would be, so that
    /// step ends and no other step child starts; a child that settles this stop still starts,
    /// unless a signal from elsewhere was caught too ([`caught_foreign`], [`Running::settling`]).
    pub fn stop_self() {
        OWN.fetch_add(1, Ordering::SeqCst);
        if let Ok(pid) = i32::try_from(std::process::id()) {
            send(pid, SIGTERM);
        }
    }

    /// The signals caught from elsewhere than [`stop_self`]: every one caught but the
    /// `SIGTERM`s this process sent itself, each of which it catches once (none where it
    /// inherited `SIGTERM` ignored, which [`install`] keeps). Only a `SIGTERM` from elsewhere can
    /// read as the release's own, in two ways: while the own one is in flight ([`stop_self`]
    /// counted it and the handler has not caught it yet), one from elsewhere already caught,
    /// before [`stop_self`] or after it, does until the own one is caught; and one that merges
    /// with the own while that is pending (the kernel keeps one pending signal of a kind) does
    /// for good.
    fn foreign() -> u32 {
        // COUNT first: each own SIGTERM it holds was counted in OWN before it was sent and in
        // TERMS before COUNT ([`on_signal`]), so the later loads hold it too.
        let caught = COUNT.load(Ordering::SeqCst);
        let own = OWN.load(Ordering::SeqCst).min(TERMS.load(Ordering::SeqCst));
        caught.saturating_sub(own)
    }

    /// Whether a signal from elsewhere than this process's own stop ([`stop_self`]) was caught:
    /// a person's ^C, a closed terminal's `SIGHUP`, a `SIGTERM` another process sent (but for
    /// one that reads as the own, [`foreign`]). It refuses a settlement's child as [`caught`]
    /// refuses every other.
    pub fn caught_foreign() -> bool {
        foreign() > 0
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
            Self::forwarding(leader, None, all_caught)
        }

        /// [`Running::enter`] for a qualify branch's child, which its branch's stop ends too:
        /// once `stop` fires the group is sent `sig`, and [`BranchStop::GRACE`] later `SIGKILL`.
        pub fn enter_stoppable(leader: u32, stop: BranchStop, sig: i32) -> Self {
            Self::forwarding(leader, Some((stop, sig)), all_caught)
        }

        /// [`Running::enter`] for a child that settles this process's own stop ([`stop_self`]):
        /// that stop, which the release already answered by starting it, is never forwarded to
        /// it; a signal from elsewhere is ([`caught_foreign`]), as the signal that interrupted
        /// the release ([`caught`]: the own `SIGTERM` when that came first).
        pub fn settling(leader: u32) -> Self {
            Self::forwarding(leader, None, foreign)
        }

        fn forwarding(leader: u32, stop: Option<(BranchStop, i32)>, counted: fn() -> u32) -> Self {
            let done = Arc::new(AtomicBool::new(false));
            let thread = i32::try_from(leader).ok().map(|group| {
                let done = Arc::clone(&done);
                std::thread::spawn(move || forward(group, &done, stop.as_ref(), counted))
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

    /// Every signal this process caught ([`COUNT`]).
    fn all_caught() -> u32 {
        COUNT.load(Ordering::SeqCst)
    }

    /// Every 50 ms until `done`, sends `group` what [`Stop`] answers for the signals `counted`
    /// so far ([`all_caught`], or for a settlement [`foreign`]), read then: the signal
    /// that interrupted the release, or `SIGKILL` ([`forward_with`]).
    /// A branch's fired `stop` counts as one more signal caught, and as a second once
    /// [`BranchStop::GRACE`] passed, so it is answered as a person's stop and then a second one;
    /// with no signal caught, the group is sent the branch's.
    fn forward(
        group: i32,
        done: &AtomicBool,
        stop: Option<&(BranchStop, i32)>,
        counted: fn() -> u32,
    ) {
        let active = || !done.load(Ordering::SeqCst);
        let stopped = || {
            stop.and_then(|(stop, _)| stop.fired_for())
                .map_or(0, |since| if since >= BranchStop::GRACE { 2 } else { 1 })
        };
        forward_with(&Forwarding {
            active: &active,
            caught: &|| counted() + stopped(),
            signal: &|| caught().unwrap_or_else(|| stop.map_or(SIGTERM, |(_, sig)| *sig)),
            clock: &std::time::Instant::now,
            send: &|sig| {
                #[cfg(test)]
                SENDS.fetch_add(1, Ordering::SeqCst);
                signal_group(group, sig, &active);
            },
            pause: &|| {
                #[cfg(test)]
                LOOKS.fetch_add(1, Ordering::SeqCst);
                std::thread::sleep(std::time::Duration::from_millis(50));
                !done.load(Ordering::SeqCst)
            },
        });
    }

    /// The looks every [`forward`] of this process ended, each counted as its pause begins,
    /// after whatever that look sent.
    #[cfg(test)]
    static LOOKS: AtomicU32 = AtomicU32::new(0);
    /// The signals every [`forward`] of this process sent its group, each counted as its send
    /// begins.
    #[cfg(test)]
    static SENDS: AtomicU32 = AtomicU32::new(0);

    /// What this process caught and its forwarders did so far ([`counts_for_test`]).
    #[cfg(test)]
    #[derive(Debug, Clone, Copy)]
    pub struct Counts {
        /// Every signal caught ([`COUNT`]): the release's own stops and those from elsewhere.
        pub caught: u32,
        /// The looks every forwarder ended ([`LOOKS`]): a look whose answer was a send ended
        /// after that send.
        pub looks: u32,
        /// The signals every forwarder sent its group ([`SENDS`]).
        pub sends: u32,
    }

    /// The signals this process caught and what its forwarders did, so a test can wait for a
    /// signal it sent to be caught, or for a forwarder to have looked, instead of for a time.
    /// The counts are process-global and never cleared, so only a test's re-executed child
    /// reads them; only this crate's test binaries call it, so the seam is `cfg(test)`.
    #[cfg(test)]
    pub fn counts_for_test() -> Counts {
        Counts {
            caught: COUNT.load(Ordering::SeqCst),
            looks: LOOKS.load(Ordering::SeqCst),
            sends: SENDS.load(Ordering::SeqCst),
        }
    }

    /// What [`forward_with`] reads, sends and waits by.
    pub(super) struct Forwarding<'a> {
        /// The step still owns forwarding; false once its guard starts closing.
        pub(super) active: &'a dyn Fn() -> bool,
        /// The signals counted so far: every one this process caught ([`COUNT`]), or for a
        /// settlement those from elsewhere ([`foreign`]), and a branch's fired stop.
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
    pub fn stop_self() {}
    pub fn caught_foreign() -> bool {
        false
    }
    pub struct Running;
    impl Running {
        pub fn enter_stoppable(_leader: u32, _stop: BranchStop, _sig: i32) -> Self {
            Self
        }
        pub fn enter(_leader: u32) -> Self {
            Self
        }
        pub fn settling(_leader: u32) -> Self {
            Self
        }
    }
}

pub use imp::*;

/// A child whose dedicated group was established by its actual spawn, not a later pid query.
pub(crate) struct GroupChild {
    child: aterm_uds::spawnfd::Child,
    drained: bool,
}

impl GroupChild {
    pub(crate) fn spawn(command: &mut aterm_uds::spawnfd::Command) -> std::io::Result<Self> {
        #[cfg(unix)]
        let child = command.spawn_in_dedicated_process_group()?;
        #[cfg(not(unix))]
        let child = command.spawn()?;
        Ok(Self {
            child,
            drained: false,
        })
    }

    pub(crate) fn id(&self) -> u32 {
        self.child.id()
    }

    #[cfg(test)]
    pub(crate) fn kill(&mut self) -> std::io::Result<()> {
        self.child.kill()
    }

    pub(crate) fn drain(&mut self) -> std::io::Result<()> {
        if self.drained {
            return Ok(());
        }
        #[cfg(unix)]
        drain_child_group(&mut self.child)?;
        self.drained = true;
        Ok(())
    }

    pub(crate) fn wait(&mut self) -> std::io::Result<std::process::ExitStatus> {
        self.drain()?;
        self.child.wait()
    }
}

impl Drop for GroupChild {
    fn drop(&mut self) {
        #[cfg(unix)]
        if self.child.owns_unreaped() {
            let _ = self.child.kill();
            let _ = self.drain();
            let _ = self.child.wait();
        }
    }
}

/// End a completed child's remaining group before returning its budget or compiler tokens.
/// The unreaped leader pins the group identity; Trust's daemon and its children remain spared.
#[cfg(unix)]
fn drain_child_group(child: &mut aterm_uds::spawnfd::Child) -> std::io::Result<()> {
    let group = i32::try_from(child.id()).map_err(std::io::Error::other)?;
    if !child.owns_unreaped() {
        return Err(std::io::Error::other(
            "slice group leader was already reaped",
        ));
    }
    child.wait_exited()?;
    loop {
        if let Some(table) = listing() {
            let work = table.iter().any(|row| {
                if row.pgid != group
                    || row.pid == group
                    || matches!(
                        aterm_uds::process::process_presence(row.pid),
                        aterm_uds::process::ProcessPresence::Exited
                            | aterm_uds::process::ProcessPresence::Absent
                    )
                {
                    return false;
                }
                // Only a positively identified daemon ancestry is exempt. A missing
                // ancestor is not safe to signal, but it is not evidence of drain either.
                let mut at = row;
                for _ in 0..=table.len() {
                    if is_trustd(&at.name) {
                        return false;
                    }
                    let Some(parent) = table.iter().find(|p| p.pid == at.parent) else {
                        break;
                    };
                    at = parent;
                }
                true
            });
            if !work {
                return Ok(());
            }
            imp::signal_group(group, SIGKILL, &|| true);
        }
        std::thread::sleep(std::time::Duration::from_millis(20));
    }
}

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
/// name is not recognised: its `argv[0]` is not read, because reading a program's arguments can
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
#[path = "interrupt_tests.rs"]
mod tests;
