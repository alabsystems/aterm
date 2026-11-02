// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Signal handling and process-group forwarding for release steps.

#[cfg(unix)]
mod imp {
    use std::sync::Arc;
    use std::sync::atomic::{AtomicBool, AtomicI32, AtomicU32, Ordering};

    pub const SIGHUP: i32 = 1;
    pub const SIGINT: i32 = 2;
    pub const SIGKILL: i32 = 9;
    pub const SIGTERM: i32 = 15;
    const SIG_IGN: usize = 1;

    static SIGNAL: AtomicI32 = AtomicI32::new(0);
    static COUNT: AtomicU32 = AtomicU32::new(0);
    /// The `SIGHUP`s, `SIGINT`s and `SIGTERM`s caught, each counted apart ([`counts`]).
    static EACH: [AtomicU32; 3] = [AtomicU32::new(0), AtomicU32::new(0), AtomicU32::new(0)];
    /// The signals [`EACH`] counts, in its order.
    const CAUGHT: [i32; 3] = [SIGHUP, SIGINT, SIGTERM];

    unsafe extern "C" {
        fn signal(sig: i32, handler: usize) -> usize;
        fn kill(pid: i32, sig: i32) -> i32;
    }

    extern "C" fn on_signal(sig: i32) {
        let _ = SIGNAL.compare_exchange(0, sig, Ordering::SeqCst, Ordering::SeqCst);
        COUNT.fetch_add(1, Ordering::SeqCst);
        if let Some(i) = CAUGHT.iter().position(|&c| c == sig) {
            EACH[i].fetch_add(1, Ordering::SeqCst);
        }
    }

    /// Catches `SIGINT`, `SIGTERM`, and `SIGHUP` unless already ignored.
    pub fn install() {
        let handler = on_signal as extern "C" fn(i32) as usize;
        for sig in [SIGHUP, SIGINT, SIGTERM] {
            // SAFETY: `signal` takes an integer and a handler value: SIG_IGN, or
            // `on_signal`, an `extern "C" fn(i32)` that lives for the whole program and
            // does only async-signal-safe work (lock-free atomics).
            unsafe {
                if signal(sig, SIG_IGN) != SIG_IGN {
                    signal(sig, handler);
                }
            }
        }
    }

    /// The signal that interrupted the release, if one did.
    pub fn caught() -> Option<i32> {
        let sig = SIGNAL.load(Ordering::SeqCst);
        (sig != 0).then_some(sig)
    }

    /// The `SIGHUP`s, `SIGINT`s and `SIGTERM`s this process has caught, in that order.
    pub fn counts() -> [u32; 3] {
        EACH.each_ref().map(|n| n.load(Ordering::SeqCst))
    }

    /// What [`wait_passing_on`] sends for each of [`CAUGHT`], in its order: a `SIGINT` as a
    /// `SIGTERM`. The re-run's `targo` passes a `SIGTERM` or `SIGHUP` on to the cutter it
    /// started and ignores a `SIGINT` sent to it alone while that cutter runs (store targo
    /// 15698, measured), so a `SIGINT` passed on as itself would stop no cutter.
    const PASSED_AS: [i32; 3] = [SIGHUP, SIGTERM, SIGTERM];

    /// Waits for `child`, sending its pid alone each signal `caught` counts (as [`counts`]
    /// does), one for one, until it ends: every one counted since this process started, so the
    /// caller starts the child only when it caught none. Each is sent as [`PASSED_AS`] names
    /// (a `SIGINT` as a `SIGTERM`), and only after a `try_wait` on this thread found the child
    /// unreaped, so its pid is still its own; each look sends the `SIGHUP`s counted since the
    /// last, then the `SIGINT`s, then the `SIGTERM`s. For the re-run as R's own cutter: its
    /// `targo` shares this process's group, so a terminal's signals reach it and its cutter
    /// already, but one sent to this pid alone (`kill -TERM`, what a second run's refusal names)
    /// reaches nothing else. That `targo` (store targo 15698, measured) ends on any of the three
    /// while it builds; while the cutter it started runs, it passes a `SIGTERM` or `SIGHUP` on
    /// to it.
    /// # Errors
    /// The child's status cannot be read.
    pub fn wait_passing_on(
        child: &mut aterm_uds::spawnfd::Child,
        caught: &dyn Fn() -> [u32; 3],
    ) -> std::io::Result<std::process::ExitStatus> {
        let pid = i32::try_from(child.id()).ok();
        let mut passed = [0u32; 3];
        loop {
            if let Some(status) = child.try_wait()? {
                return Ok(status);
            }
            let now = caught();
            for (i, sig) in PASSED_AS.into_iter().enumerate() {
                while passed[i] < now[i] {
                    if let Some(pid) = pid {
                        send(pid, sig);
                    }
                    passed[i] += 1;
                }
            }
            std::thread::sleep(std::time::Duration::from_millis(50));
        }
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
    /// `SIGHUP` or `SIGTERM` on to its cutter within a millisecond (store targo 15698, measured),
    /// and a re-run's cutter a third time through the run waiting on it, which passes each signal
    /// on at its next 50 ms look ([`wait_passing_on`]). The first send lists every process
    /// before it sends ([`super::listing`]), and a copy can come while that runs: a signal caught
    /// before that send ended, or found sooner than this after it, is taken for such a copy; a
    /// person's second stop comes later.
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
            let done = Arc::new(AtomicBool::new(false));
            let thread = i32::try_from(leader).ok().map(|group| {
                let done = Arc::clone(&done);
                std::thread::spawn(move || forward(group, &done))
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
    fn forward(group: i32, done: &AtomicBool) {
        let active = || !done.load(Ordering::SeqCst);
        forward_with(&Forwarding {
            active: &active,
            caught: &|| COUNT.load(Ordering::SeqCst),
            signal: &|| SIGNAL.load(Ordering::SeqCst),
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
        let home = i32::try_from(std::process::id()).unwrap_or(0);
        let passes = if sig == SIGKILL { 2 } else { 1 };
        for _ in 0..passes {
            if !active() {
                return;
            }
            let members = super::listing()
                .map_or_else(|| vec![group], |rows| super::signalled(&rows, group, home));
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
    pub fn install() {}
    pub fn caught() -> Option<i32> {
        None
    }
    pub struct Running;
    impl Running {
        pub fn enter(_leader: u32) -> Self {
            Self
        }
    }
    pub fn counts() -> [u32; 3] {
        [0, 0, 0]
    }
    pub fn wait_passing_on(
        child: &mut aterm_uds::spawnfd::Child,
        _caught: &dyn Fn() -> [u32; 3],
    ) -> std::io::Result<std::process::ExitStatus> {
        child.wait()
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
            aterm_uds::deadline::KeptUntilRan::new(aterm_tempfile::tempdir().unwrap().keep());
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
        let group = i32::try_from(leader.id()).unwrap();
        let from_group = lines(leader.stdout.take().unwrap());
        let up = next(&from_group, "the grandchild").expect("the grandchild's first line");
        let grandchild = up
            .strip_prefix("up ")
            .expect("the grandchild says it is up");
        let mut standin = command(&trustd)
            .args(["-c", "echo up; /bin/cat; echo \"cat $?\""])
            .process_group(group)
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::null())
            .spawn()
            .unwrap();
        let from_standin = lines(standin.stdout.take().unwrap());
        let mut to_standin = standin.stdin.take().unwrap();
        assert_eq!(next(&from_standin, "the stand-in").as_deref(), Some("up"));
        to_standin.write_all(b"before\n").unwrap();
        assert_eq!(
            next(&from_standin, "its cat").as_deref(),
            Some("before"),
            "the trustd's cat runs in the group"
        );

        imp::signal_group(group, SIGTERM, &|| true);

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
