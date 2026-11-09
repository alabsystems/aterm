// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! A BUILD'S CLAIM KEPT BY A HOLDER WHILE THE BUILD RUNS IN ITS FRONT'S PID (BUILD-ADMIT, the shim
//! hop). The front ([`super::front_exec`]) claims, spawns `<aterm> __build_hold <pid> <serial>
//! <claim> <key> <book>` detached ([`aterm_uds::spawnfd::detached_with_two_fds`]: its own session,
//! stdio on `/dev/null`, a copy of the claim's descriptor at 3 and the arming pipe's write end at
//! 4), waits for the arming byte, hands its own copy off and execs the build. That call's first
//! act ([`entry`]) forks once, while it has one thread, and the parent exits, so the holder is
//! launchd's: no job's signal, no `TaskStop` of the caller's tree and no wait of the build's
//! reaches it.
//!
//! The holder ([`hold`]) writes its pid, adopts the claim (`Claim::adopt`'s three proofs),
//! watches the front's pid for its exit and its status (`EVFILT_PROC` `NOTE_EXIT |
//! NOTE_EXITSTATUS`, which a non-parent of the same user may register and which survives the
//! front's exec: measured on macOS 27.0.1, 37 of 37), checks after registering that the pid is
//! still the front's process by its serial, and only then writes the arming byte and starts
//! metering the front's tree. A front that gave up on it closed the pipe, so that write fails and
//! the holder leaves without letting the claim go (every way it leaves unarmed hands the claim
//! off, never drops it): the front keeps it.
//!
//! While the build runs the watch wakes every [`aterm_capacity::tree_meter::READ_EVERY`] too, and
//! reads the front's pid itself. A pid that names no live process of its serial is an exit
//! begun, not read yet: macOS stops reading a dying process's pid before it queues the exit's
//! event, so the watch waits on for that event and its status, and takes the end for one with no
//! status only after a minute of such reads in a row (`EXIT_EVENT_WAKES`). A kevent that fails
//! (any error but `EINTR`) is no exit, so the holder then waits by those reads alone until the
//! pid names no live process of its serial.
//!
//! At the build's end: an exit lets the claim go at once ([`super::Held::let_go`], before the
//! meter's last read), its peak then kept as the front's would have been ([`super::Ended::keep`]:
//! the meter's peak, with no floor of the build's own peak, which only the process that reaps it
//! reads: `wait4`'s accounting). A death by a signal keeps it while the build's SURVIVORS still work
//! (the compilers a killed `cargo` leaves running, `TreeMeter::survivors_using`), crediting them
//! with their use (`Claim::credit`), and lets it go ([`Survival`]) once none is left, once they
//! hold less than [`SURVIVORS_IDLE_UNDER`] together (a shell, a sleeper, a server waiting: no
//! compile works in so little), once their footprint has not grown for [`SURVIVORS_STALL_WITHIN`]
//! (a daemon holding what it took), at [`SURVIVORS_HELD_WITHIN`] at the latest, or at once when
//! the front's pid is seen live as another process. The holder signals nothing. A holder that
//! dies lets the claim go with it, so every failure of it fails open.

/// The longest a holder keeps a claim for what a build killed by a signal left running: a
/// production bound past which the claim ends anyway, so survivors that keep growing (a test
/// that never exits) cannot pin it, a claim admitted alone included.
pub const SURVIVORS_HELD_WITHIN: std::time::Duration = std::time::Duration::from_mins(30);

/// The footprint a killed build's survivors must hold together to keep its claim, bytes: below
/// it they are idle (a shell, a sleeper, a server waiting on its socket; a working `rustc` holds
/// hundreds of MiB), and the claim, which would reserve its whole need less that little for them,
/// goes at once.
pub const SURVIVORS_IDLE_UNDER: u64 = 64 << 20;

/// The longest a killed build's survivors keep its claim once their footprint stopped growing:
/// `supervise`'s grace for what a killed build leaves, before it is killed
/// ([`super::TREE_ENDS_WITHIN`]). Memory they
/// hold still is counted by every reading of the machine; what the claim held beyond it was room
/// for them to grow.
#[cfg(unix)]
pub const SURVIVORS_STALL_WITHIN: std::time::Duration = super::TREE_ENDS_WITHIN;

/// How many of a watch's wakes in a row ([`aterm_capacity::tree_meter::READ_EVERY`] apart, each
/// with no event: a minute's) read the front's pid gone before its end is taken for one with no
/// status. macOS stops reading a killed process's pid before it queues the exit's event
/// (`NOTE_EXIT`): 110 of 110 kills on macOS 27.2.0, the event 1 µs to 1.2 ms later under load. A
/// watch that took the first such read for the end read a death by a signal as no status, and
/// so let a killed build's claim go while its survivors worked. The event comes, so this bounds
/// only one the kernel holds back (it reports a traced process's exit to its debugger first) or
/// never queues.
#[cfg(target_os = "macos")]
const EXIT_EVENT_WAKES: u32 = 120;

#[cfg(target_os = "macos")]
const _: () = assert!(
    EXIT_EVENT_WAKES as u128 * aterm_capacity::tree_meter::READ_EVERY.as_millis()
        == std::time::Duration::from_mins(1).as_millis()
);

/// TEST SEAM: set in a debug build's environment to a file, a holder appends at each wake of its
/// watch one line naming what its meter's last read found descended from the front (`reads
/// <pid>…`), so a test kills the build only once a child it started was seen in its tree.
#[cfg(any(test, debug_assertions))]
pub const READS_ENV: &str = "ATERM_BUILD_HOLD_READS";

/// TEST SEAM: a private JSON file replaced with the last 256 diagnostic events. Meter samples
/// include PID, start, footprint and eligibility; exit and survival events retain the holder's
/// actual decision. This observation never changes admission or the survival policy.
#[cfg(any(test, debug_assertions))]
pub const TRACE_ENV: &str = "ATERM_BUILD_HOLD_TRACE";

/// The holder's verb ([`aterm_uds::build_front::HOLD_VERB`]).
pub use aterm_uds::build_front::HOLD_VERB;

/// TEST SEAM: set in a debug build's environment, a holder adopts its claim and then leaves
/// unarmed, handing it off, as one whose watch the kernel refused: what a test of the front's
/// fallback (the build run under the front, its claim kept) starts. Set to [`UNARMED_PANICS`], it
/// panics there instead, as a fault between its adoption and its arming would.
#[cfg(any(test, debug_assertions))]
pub const UNARMED_ENV: &str = "ATERM_BUILD_HOLD_UNARMED";

/// [`UNARMED_ENV`]'s value that makes the holder panic once it adopted its claim.
#[cfg(any(test, debug_assertions))]
pub const UNARMED_PANICS: &str = "panic";

/// What a holder's command line names: the front's pid and serial, the claim's file, its key, and
/// the book its peak is kept in (`-` for none).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Words {
    /// The front's pid: the build's, once it execs.
    pub pid: u32,
    /// Its serial (`aterm_uds::process::process_serial`), which tells it from a later process
    /// given the same pid.
    pub serial: u64,
    /// The claim's file.
    pub claim: std::path::PathBuf,
    /// The key its peak is kept under.
    pub key: aterm_capacity::book::Key,
    /// The book its peak is kept in.
    pub own: Option<std::path::PathBuf>,
}

impl Words {
    /// The words after [`HOLD_VERB`].
    #[must_use]
    pub fn render(&self) -> Vec<std::ffi::OsString> {
        vec![
            self.pid.to_string().into(),
            self.serial.to_string().into(),
            self.claim.clone().into_os_string(),
            self.key.to_json().into(),
            self.own
                .clone()
                .map_or_else(|| "-".into(), std::path::PathBuf::into_os_string),
        ]
    }

    /// The words [`Self::render`] wrote; `None` for any others.
    #[must_use]
    pub fn parse(words: &[std::ffi::OsString]) -> Option<Words> {
        let [pid, serial, claim, key, own] = words else {
            return None;
        };
        let claim = std::path::PathBuf::from(claim);
        let own = match own.to_str() {
            Some("-") => None,
            _ => Some(std::path::PathBuf::from(own))
                .filter(|p| p.is_absolute())?
                .into(),
        };
        let pid = pid.to_str()?;
        let serial = serial.to_str()?;
        if pid.is_empty()
            || !pid.bytes().all(|b| b.is_ascii_digit())
            || serial.is_empty()
            || !serial.bytes().all(|b| b.is_ascii_digit())
        {
            return None;
        }
        Some(Words {
            pid: pid.parse().ok().filter(|p| *p > 1)?,
            serial: serial.parse().ok()?,
            claim: claim.is_absolute().then_some(claim)?,
            key: aterm_capacity::book::Key::from_json(key.to_str()?)?,
            own,
        })
    }
}

/// An executable's first call in `main`, while it has one thread: asked for [`HOLD_VERB`], it
/// becomes the holder (forking once so launchd adopts it, the parent exiting at once) and answers
/// the exit code it ends with; otherwise `None`.
#[must_use = "return a handled admission command's exit status to the caller"]
pub fn entry() -> Option<std::process::ExitCode> {
    let mut args = std::env::args_os().skip(1);
    if args.next()? != HOLD_VERB {
        return None;
    }
    let words: Vec<std::ffi::OsString> = args.collect();
    Some(serve(&words))
}

/// Serve [`HOLD_VERB`] on `words`: 2 for words it does not take, 1 where it cannot hold (off
/// macOS, and a fork refused), else the holder's end.
#[cfg(target_os = "macos")]
fn serve(words: &[std::ffi::OsString]) -> std::process::ExitCode {
    let Some(words) = Words::parse(words) else {
        return std::process::ExitCode::from(2);
    };
    // SAFETY: `fork` takes nothing. This process has run nothing but `main`'s first call, so it
    // has one thread, and the child goes on as an ordinary single-threaded process.
    match unsafe { libc::fork() } {
        0 => std::process::ExitCode::from(hold(&words)),
        // SAFETY: the parent leaves at once with no exit handler run: its child is the holder.
        pid if pid > 0 => unsafe { libc::_exit(0) },
        _ => std::process::ExitCode::from(1),
    }
}

/// Off macOS no holder runs: the front supervises its build instead.
#[cfg(not(target_os = "macos"))]
fn serve(words: &[std::ffi::OsString]) -> std::process::ExitCode {
    let _ = words;
    std::process::ExitCode::from(1)
}

/// How the front's process ended, as its watch read it.
#[cfg(target_os = "macos")]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum End {
    /// Its `wait(2)` status.
    Status(i32),
    /// Its exit, with no status (the knote gave none).
    Unknown,
}

/// THE HOLDER: from [`HOLD_VERB`]'s fork, with the claim's descriptor at 3 and the arming pipe at
/// 4. Its exit code: 0 once it let the claim go at the build's end, 1 where it never armed.
#[cfg(target_os = "macos")]
fn hold(words: &Words) -> u8 {
    use std::io::Write as _;
    use std::os::fd::FromRawFd as _;
    let (claim_fd, arming_fd) = (
        aterm_uds::spawnfd::BRIDGE_VERB_FD,
        aterm_uds::spawnfd::BRIDGE_PUSH_FD,
    );
    // SAFETY: F_GETFD reads one descriptor's flags; no memory is touched.
    if [claim_fd, arming_fd]
        .iter()
        .any(|fd| unsafe { libc::fcntl(*fd, libc::F_GETFD) } < 0)
    {
        return 1;
    }
    // SAFETY: the spawn placed the arming pipe's write end at this descriptor, which nothing else
    // in this process owns.
    let mut arming = unsafe { std::fs::File::from_raw_fd(arming_fd) };
    // SAFETY: as above, the claim's descriptor.
    let claim_fd = unsafe { std::os::fd::OwnedFd::from_raw_fd(claim_fd) };
    // SAFETY: `getpid` takes nothing and cannot fail.
    let me = unsafe { libc::getpid() }.unsigned_abs();
    if arming.write_all(&me.to_le_bytes()).is_err() {
        return 1;
    }
    let Ok(claim) = aterm_capacity::Claim::adopt(claim_fd, &words.claim) else {
        return 1;
    };
    let claim = Unarmed(Some(claim));
    #[cfg(any(test, debug_assertions))]
    if let Some(seam) = std::env::var_os(UNARMED_ENV) {
        assert!(seam != UNARMED_PANICS, "{UNARMED_ENV}={UNARMED_PANICS}");
        return 1;
    }
    let Some(watch) = Watch::on(words.pid) else {
        return 1;
    };
    let Ok(pid) = i32::try_from(words.pid) else {
        return 1;
    };
    // The watch and the meter's first read are on the front's process only while its pid still
    // names it: a front gone before this read (its pid given anew, or a zombie, which reads none)
    // is never armed for.
    let Some(meter) = meter_for_front(
        || aterm_capacity::tree_meter::TreeMeter::start(words.pid),
        || {
            aterm_uds::process::process_serial(pid) == Some(words.serial)
                && arming.write_all(&[1]).is_ok()
        },
    ) else {
        return 1;
    };
    let Some(mut claim) = claim.armed() else {
        return 1;
    };
    drop(arming);
    #[cfg(any(test, debug_assertions))]
    trace_meter(&meter, words);
    #[cfg(any(test, debug_assertions))]
    let reads = std::env::var_os(READS_ENV).map(std::path::PathBuf::from);
    let mut phases = aterm_capacity::phase::Phases::new();
    let end = watch.wait(words.serial, &mut || {
        #[cfg(any(test, debug_assertions))]
        if let Some(reads) = &reads {
            note_reads(reads, &meter);
        }
        #[cfg(any(test, debug_assertions))]
        trace_meter(&meter, words);
        let stepped = step_phase(&mut phases, &mut claim, &meter);
        #[cfg(any(test, debug_assertions))]
        if let Some(step) = stepped {
            trace_record(|| aterm_json::json!({"kind": "phase-step", "step": format!("{step:?}")}));
        }
        #[cfg(not(any(test, debug_assertions)))]
        let _ = stepped;
    });
    #[cfg(any(test, debug_assertions))]
    trace_record(|| {
        aterm_json::json!({
            "kind": "front-ended", "front_pid": words.pid, "front_serial": words.serial,
            "end": format!("{end:?}"),
        })
    });
    let held = super::Held {
        claim,
        key: words.key.clone(),
        own: words.own.clone(),
        cohort: false,
    };
    let held = match end {
        End::Status(status) if libc::WIFSIGNALED(status) => survive(held, &meter, words),
        End::Status(_) | End::Unknown => held,
    };
    let built = match end {
        End::Status(status) => {
            super::BuildEnd::Status(std::os::unix::process::ExitStatusExt::from_raw(status))
        }
        End::Unknown => super::BuildEnd::Unknown,
    };
    let ended = held.let_go();
    #[cfg(any(test, debug_assertions))]
    trace_record(|| aterm_json::json!({"kind": "claim-released", "succeeded": built.succeeded()}));
    let peak = meter.stop(0);
    #[cfg(any(test, debug_assertions))]
    trace_record(|| aterm_json::json!({"kind": "meter-stopped", "peak": peak}));
    ended.keep_run(&built, phases.run_peak(), &mut |_| {});
    ended.keep(built, peak, &mut |_| {});
    #[cfg(any(test, debug_assertions))]
    trace_record(|| {
        aterm_json::json!({
            "kind": "complete", "front_pid": words.pid, "front_serial": words.serial,
        })
    });
    0
}

/// ONE READ OF THE BUILD'S PHASE (CAP3, [`aterm_capacity::phase`]): the processes `meter`'s latest
/// read found in its tree, each with its footprint and its lifetime peak
/// ([`aterm_capacity::tree_meter::TreeMeter::members`], no read of its own), each one's program
/// read from the kernel; `claim` stepped as `phases` says, and the step it took answered.
#[cfg(target_os = "macos")]
fn step_phase(
    phases: &mut aterm_capacity::phase::Phases,
    claim: &mut aterm_capacity::Claim,
    meter: &aterm_capacity::tree_meter::TreeMeter,
) -> Option<aterm_capacity::phase::Step> {
    phases.read(claim, &meter.members(), &aterm_capacity::phase::image)
}

/// The front's meter, started (its first read made) before `still_front` reads whether the
/// front's pid still names it, so that read vouches for the meter's first one as it does for the
/// watch armed before it; `None`, the meter dropped, where it does not. A meter started after the
/// read could take its root from a later process given the pid in between.
#[cfg(target_os = "macos")]
fn meter_for_front<M>(start: impl FnOnce() -> M, still_front: impl FnOnce() -> bool) -> Option<M> {
    let meter = start();
    still_front().then_some(meter)
}

/// A CLAIM ADOPTED AND NOT YET ARMED FOR: every way the holder leaves before its arming byte (a
/// return, a panic's unwind) hands it off ([`aterm_capacity::Claim::hand_off`]), never lets it
/// go, so the front, which still holds its copy, keeps it.
#[cfg(target_os = "macos")]
struct Unarmed(Option<aterm_capacity::Claim>);

#[cfg(target_os = "macos")]
impl Unarmed {
    /// Armed: the claim is the holder's to let go.
    fn armed(mut self) -> Option<aterm_capacity::Claim> {
        self.0.take()
    }
}

#[cfg(target_os = "macos")]
impl Drop for Unarmed {
    fn drop(&mut self) {
        if let Some(claim) = self.0.take() {
            claim.hand_off();
        }
    }
}

/// [`READS_ENV`]'s line: what `meter`'s last read found descended from the front, appended to
/// `reads`.
#[cfg(all(target_os = "macos", any(test, debug_assertions)))]
fn note_reads(reads: &std::path::Path, meter: &aterm_capacity::tree_meter::TreeMeter) {
    use std::io::Write as _;
    use std::os::unix::fs::OpenOptionsExt as _;
    let pids: Vec<String> = meter.survivors().iter().map(u32::to_string).collect();
    if let Ok(mut file) = std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .custom_flags(aterm_uds::atomic_file::LOCK_OPEN_FLAGS)
        .open(reads)
        && file.metadata().is_ok_and(|m| m.is_file())
    {
        let _ = writeln!(file, "reads {}", pids.join(" "));
    }
}

/// The trace is opt-in, process-local, and bounded independently of the holder's lifetime.
#[cfg(all(target_os = "macos", any(test, debug_assertions)))]
struct Trace {
    path: std::path::PathBuf,
    since: std::time::Instant,
    sequence: u64,
    events: std::collections::VecDeque<aterm_json::Value>,
}

#[cfg(all(target_os = "macos", any(test, debug_assertions)))]
fn trace_record(event: impl FnOnce() -> aterm_json::Value) {
    thread_local! {
        static TRACE: std::cell::RefCell<Option<Trace>> = std::cell::RefCell::new(
            std::env::var_os(TRACE_ENV).map(|path| Trace {
                path: path.into(), since: std::time::Instant::now(), sequence: 0,
                events: std::collections::VecDeque::new(),
            })
        );
    }
    TRACE.with_borrow_mut(|trace| {
        let Some(trace) = trace else { return };
        let mut event = event();
        trace.sequence = trace.sequence.saturating_add(1);
        let Some(fields) = event.as_object_mut() else {
            return;
        };
        fields.insert("sequence".into(), trace.sequence.into());
        fields.insert(
            "elapsed_ms".into(),
            u64::try_from(trace.since.elapsed().as_millis())
                .unwrap_or(u64::MAX)
                .into(),
        );
        if trace.events.len() == 256 {
            trace.events.pop_front();
        }
        trace.events.push_back(event);
        let body = aterm_json::json!({
            "v": 1, "holder_pid": std::process::id(),
            "events": trace.events.iter().collect::<Vec<_>>(),
        });
        let Ok(bytes) = aterm_json::to_vec(&body) else {
            return;
        };
        let _ = aterm_uds::atomic_file::write(&trace.path, &bytes, 0o600);
    });
}

#[cfg(all(target_os = "macos", any(test, debug_assertions)))]
fn trace_meter(meter: &aterm_capacity::tree_meter::TreeMeter, words: &Words) {
    trace_record(|| {
        let sample = meter.observation();
        let members: Vec<_> = sample
            .members
            .iter()
            .map(|(m, eligible)| {
                aterm_json::json!({
                    "pid": m.pid, "start": m.start, "footprint": m.footprint,
                    "peak": m.peak, "eligible": eligible,
                })
            })
            .collect();
        aterm_json::json!({
            "kind": "meter", "front_pid": words.pid, "front_serial": words.serial,
            "sample_sequence": sample.sequence, "available": sample.available,
            "root_lived": sample.root_lived, "group": sample.group, "members": members,
        })
    });
}

/// Why a holder lets a killed build's claim go at one look at its survivors ([`Survival::look`]).
#[cfg(target_os = "macos")]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Gone {
    /// None is left.
    None,
    /// They hold less than [`SURVIVORS_IDLE_UNDER`] together.
    Idle,
    /// Their footprint has not grown for [`SURVIVORS_STALL_WITHIN`].
    Stalled,
    /// [`SURVIVORS_HELD_WITHIN`] passed.
    Spent,
}

/// HOW A HOLDER JUDGES A KILLED BUILD'S SURVIVORS, read by read: the most they held together so
/// far, and when they first held it.
#[cfg(target_os = "macos")]
#[derive(Debug, Default)]
pub(crate) struct Survival {
    most: u64,
    grew: std::time::Duration,
}

#[cfg(target_os = "macos")]
impl Survival {
    /// One look, `at` after the build's death, at survivors holding `footprint` together (`any`:
    /// whether one is left): `None` to keep the claim, else why it goes ([`Gone`], in that order).
    pub(crate) fn look(
        &mut self,
        at: std::time::Duration,
        any: bool,
        footprint: u64,
    ) -> Option<Gone> {
        if !any {
            return Some(Gone::None);
        }
        if at >= SURVIVORS_HELD_WITHIN {
            return Some(Gone::Spent);
        }
        if footprint < SURVIVORS_IDLE_UNDER {
            return Some(Gone::Idle);
        }
        if footprint > self.most {
            self.most = footprint;
            self.grew = at;
        }
        (at.saturating_sub(self.grew) >= SURVIVORS_STALL_WITHIN).then_some(Gone::Stalled)
    }
}

/// KEEP THE CLAIM FOR THE BUILD'S SURVIVORS while they work ([`Survival`]): credit them with their
/// use, read again every [`aterm_capacity::tree_meter::READ_EVERY`], and come back once a look lets
/// it go, or the front's pid is another process's. A read of the known group that fails
/// ([`aterm_capacity::tree_meter::Survivors::Unread`]) lets nothing go: it is no reading of
/// none left, so the claim is kept to the next read, [`SURVIVORS_HELD_WITHIN`] from the
/// build's death at the latest. Where no read ever found the group
/// ([`aterm_capacity::tree_meter::Survivors::Unobserved`]) no survivor was ever observed to
/// keep the claim for, and it goes at once, as a read of none did.
#[cfg(target_os = "macos")]
fn survive(
    mut held: super::Held,
    meter: &aterm_capacity::tree_meter::TreeMeter,
    words: &Words,
) -> super::Held {
    use aterm_capacity::tree_meter::Survivors;
    let from = std::time::Instant::now();
    let mut judged = Survival::default();
    let mut credited: Vec<u32> = Vec::new();
    let pid = i32::try_from(words.pid).unwrap_or(0);
    loop {
        let (mut now, footprint) = match meter.survivors_using_observed() {
            Survivors::Read(now, footprint) => (now, footprint),
            Survivors::Unobserved => {
                #[cfg(any(test, debug_assertions))]
                trace_record(
                    || aterm_json::json!({"kind": "survival-ended", "reason": "survivors-unobserved"}),
                );
                return held;
            }
            Survivors::Unread => {
                if from.elapsed() >= SURVIVORS_HELD_WITHIN {
                    #[cfg(any(test, debug_assertions))]
                    trace_record(|| {
                        aterm_json::json!({
                            "kind": "survival-ended", "reason": format!("{:?}", Gone::Spent),
                        })
                    });
                    return held;
                }
                std::thread::sleep(aterm_capacity::tree_meter::READ_EVERY);
                meter.read();
                continue;
            }
        };
        #[cfg(any(test, debug_assertions))]
        {
            let observed = meter.survivors_observed();
            trace_record(|| {
                let members: Vec<_> = observed
                    .members
                    .iter()
                    .map(|(m, bytes)| {
                        aterm_json::json!({
                            "pid": m.pid, "start": m.start, "ppid": m.ppid, "footprint": bytes,
                            "eligible": now.contains(&m.pid),
                        })
                    })
                    .collect();
                aterm_json::json!({
                    "kind": "survival-read", "front_pid": words.pid, "front_serial": words.serial,
                    "meter_sequence_at_trace": meter.observation().sequence,
                    "members": members, "footprint": footprint,
                })
            });
        }
        now.sort_unstable();
        if let Some(gone) = judged.look(from.elapsed(), !now.is_empty(), footprint) {
            #[cfg(any(test, debug_assertions))]
            trace_record(
                || aterm_json::json!({"kind": "survival-ended", "reason": format!("{gone:?}")}),
            );
            #[cfg(not(any(test, debug_assertions)))]
            let _ = gone;
            return held;
        }
        if aterm_uds::process::process_serial(pid).is_some_and(|s| s != words.serial) {
            #[cfg(any(test, debug_assertions))]
            trace_record(
                || aterm_json::json!({"kind": "survival-ended", "reason": "front-pid-reused"}),
            );
            return held;
        }
        if now != credited {
            let result = held.claim.credit(&now);
            #[cfg(any(test, debug_assertions))]
            trace_record(|| {
                aterm_json::json!({
                    "kind": "credit", "pids": now, "ok": result.is_ok(),
                    "error": result.as_ref().err().map(ToString::to_string),
                })
            });
            if result.is_ok() {
                credited = now;
            }
        }
        std::thread::sleep(aterm_capacity::tree_meter::READ_EVERY);
        meter.read();
    }
}

/// A kqueue watch of one process's exit and status.
#[cfg(target_os = "macos")]
struct Watch {
    kq: std::os::fd::OwnedFd,
    pid: u32,
}

#[cfg(target_os = "macos")]
impl Watch {
    /// Watch `pid` for its exit and status; `None` where the kernel refused it (`ESRCH` for a
    /// process gone or a zombie, a refusal of the watch itself).
    fn on(pid: u32) -> Option<Watch> {
        use std::os::fd::{AsRawFd as _, FromRawFd as _};
        // SAFETY: `kqueue` takes nothing and returns a new descriptor or -1.
        let raw = unsafe { libc::kqueue() };
        if raw < 0 {
            return None;
        }
        // SAFETY: `raw` is a fresh descriptor this process alone owns.
        let kq = unsafe { std::os::fd::OwnedFd::from_raw_fd(raw) };
        // SAFETY: F_SETFD touches only this descriptor's flags.
        unsafe { libc::fcntl(kq.as_raw_fd(), libc::F_SETFD, libc::FD_CLOEXEC) };
        let change = libc::kevent {
            ident: pid as usize,
            filter: libc::EVFILT_PROC,
            flags: libc::EV_ADD | libc::EV_RECEIPT,
            fflags: libc::NOTE_EXIT | libc::NOTE_EXITSTATUS,
            data: 0,
            udata: std::ptr::null_mut(),
        };
        let mut receipt = libc::kevent {
            ident: 0,
            filter: 0,
            flags: 0,
            fflags: 0,
            data: 0,
            udata: std::ptr::null_mut(),
        };
        // SAFETY: one change and one receipt slot, both live for the call; no timeout is read
        // for a receipt.
        let rc = unsafe {
            libc::kevent(
                kq.as_raw_fd(),
                &raw const change,
                1,
                &raw mut receipt,
                1,
                std::ptr::null(),
            )
        };
        (rc == 1 && !(receipt.flags & libc::EV_ERROR != 0 && receipt.data != 0))
            .then_some(Watch { kq, pid })
    }

    /// Wait for the exit of the process, `serial` its serial: its status where the kernel gave
    /// one. `tick` runs at each wake, every [`aterm_capacity::tree_meter::READ_EVERY`], when the
    /// pid is read too. A pid that names no live process of `serial` is an exit begun, whose
    /// event the next kevent takes once the kernel queues it ([`EXIT_EVENT_WAKES`]); only that
    /// many such wakes in a row with no event are an end with no status (`End::Unknown`). A
    /// kevent that fails (any error but `EINTR`) is waited out by those reads alone, never
    /// taken for an exit, and there the first read of the pid gone is the end.
    fn wait(&self, serial: u64, tick: &mut dyn FnMut()) -> End {
        let pid = i32::try_from(self.pid).ok();
        let mut gone = || pid.and_then(aterm_uds::process::process_serial) != Some(serial);
        self.wait_reading(&mut gone, EXIT_EVENT_WAKES, tick)
    }

    /// [`Self::wait`], its process's pid read by `gone` (true where the pid names no live process
    /// of the watched one's serial), its end taken for one with no status once `gone` said so at
    /// `wakes` wakes in a row with no exit event.
    fn wait_reading(
        &self,
        gone: &mut dyn FnMut() -> bool,
        wakes: u32,
        tick: &mut dyn FnMut(),
    ) -> End {
        use std::os::fd::AsRawFd as _;
        let every = aterm_capacity::tree_meter::READ_EVERY;
        let nap = libc::timespec {
            tv_sec: 0,
            tv_nsec: libc::c_long::try_from(every.as_nanos()).unwrap_or(500_000_000),
        };
        let mut failed = false;
        let mut gone_for = 0u32;
        loop {
            if failed {
                std::thread::sleep(every);
                tick();
                if gone() {
                    return End::Unknown;
                }
                continue;
            }
            let mut event = libc::kevent {
                ident: 0,
                filter: 0,
                flags: 0,
                fflags: 0,
                data: 0,
                udata: std::ptr::null_mut(),
            };
            // SAFETY: one event slot and the timeout, both live for the call.
            let rc = unsafe {
                libc::kevent(
                    self.kq.as_raw_fd(),
                    std::ptr::null(),
                    0,
                    &raw mut event,
                    1,
                    &raw const nap,
                )
            };
            if rc < 0 {
                failed = std::io::Error::last_os_error().kind() != std::io::ErrorKind::Interrupted;
                continue;
            }
            if rc == 0 {
                tick();
                // The kernel stops reading a dying process's pid before it queues the exit's
                // event: a read of it gone is no end yet, and the next kevent takes the event,
                // and its status, the moment it is queued. A read of it live counts anew.
                gone_for = if gone() {
                    gone_for.saturating_add(1)
                } else {
                    0
                };
                if gone_for >= wakes {
                    return End::Unknown;
                }
                continue;
            }
            if event.ident != self.pid as usize || event.fflags & libc::NOTE_EXIT == 0 {
                continue;
            }
            return Self::status(&event);
        }
    }

    /// The end an exit event names.
    fn status(event: &libc::kevent) -> End {
        if event.fflags & libc::NOTE_EXITSTATUS == 0 {
            End::Unknown
        } else {
            i32::try_from(event.data & 0xffff_ffff).map_or(End::Unknown, End::Status)
        }
    }
}

#[cfg(all(test, target_os = "macos"))]
#[path = "build_admit_hold_tests.rs"]
mod tests;
