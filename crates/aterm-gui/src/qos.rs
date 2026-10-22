// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Thread scheduling role, declared once per worker.
//!
//! macOS schedules by Quality of Service, and on Apple Silicon QoS also steers
//! P-core versus E-core placement. A thread that declares none inherits
//! `QOS_CLASS_DEFAULT`, just below `USER_INITIATED`, and so competes with the UI
//! thread's work whenever the machine is saturated. Nearly every aterm worker (font
//! warmers, encoders, update checkers, package probes, log flushers) does work no
//! human is waiting on; left at the default it keeps its share under load while
//! keystroke handling waits, which is felt as typing lag.
//!
//! The scale is deliberately coarse, four roles chosen by asking "who is waiting for
//! this?", so the priority order stays explicit and reviewable. Call sites stay
//! platform-neutral: on macOS a role is a QoS class, on Linux (x86_64/aarch64)
//! `Interactive` and `Responsive` are an EEVDF slice request (see the Linux section
//! below), and elsewhere a role is a no-op.
//!
//! A worker that initialises or holds a lock the UI thread contends (a `Mutex`, or a
//! `OnceLock` the UI thread's own first use would block on) is never demoted below
//! [`Role::Responsive`]: a descheduled lock holder at a lower class is a priority
//! inversion.
//!
//! NO WORK INTERVAL (LAG-12, 2026-10-19; LAG-13, 2026-10-20). No role here, and
//! no `os_workgroup` interval either, lifts a spawn out of the idle slow clock.
//! An audio interval speeds the threads inside it but cannot hold the shell,
//! another process, whose first prompt came no sooner with it in aterm.app's
//! own band (28.60 against 28.54 ms) and 3.7-5.3 ms later inside an agent's.
//! Real-time spawn threads and a `UI_FOCAL` shell did not lift it either. A
//! member thread with no open interval runs slower than a plain one, and
//! control lanes inside one doubled a spawn's reply under load. The
//! measurements are in `docs/DESIGN-warm-spare-2026-10-16.md` §16 D6.

/// What a thread's work is worth relative to the human at the keyboard.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Role {
    /// The human is watching this land right now: input egress, and anything else on
    /// the keystroke→glass path. Ranks at or above the UI thread.
    Interactive,
    /// The human asked for it and is waiting, but not frame-by-frame: PTY drain and
    /// parse, reflow. Below the UI thread, above everything cosmetic.
    ///
    /// Also the floor for any worker that holds or initialises a lock the UI thread
    /// contends: it shortens the window in which a lock holder can sit descheduled
    /// with the UI thread queued behind it.
    Responsive,
    /// Useful work nobody is blocked on: font warming, encoding, log flushing,
    /// package/update probes. Must never delay a keystroke.
    Background,
    /// Work with no deadline at all, which may run arbitrarily late: opportunistic
    /// disk sweeps and prefetch. Yields to everything.
    ///
    /// "Arbitrarily late" is the whole contract, and it is stricter than it
    /// sounds. A `QOS_CLASS_BACKGROUND` thread on a saturated machine can be
    /// starved for many seconds, so anything holding a lock, enforcing a
    /// timeout, or owing cleanup that something else waits on does NOT belong
    /// here. Process REAPERS in particular look like housekeeping and are not:
    /// they enforce a kill-and-reap contract on a real deadline, and a starved
    /// reaper leaks the very processes whose CPU use degrades the machine. They
    /// take [`Role::Background`]. (Pinned by experience — putting the
    /// artifact-reply quarantine reaper here, whose guards hold capture
    /// capacity, failed its reap by 60 s, 2026-09-30.)
    Housekeeping,
}

#[cfg(target_os = "macos")]
const fn qos_class(role: Role) -> libc::qos_class_t {
    match role {
        Role::Interactive => libc::qos_class_t::QOS_CLASS_USER_INTERACTIVE,
        Role::Responsive => libc::qos_class_t::QOS_CLASS_USER_INITIATED,
        Role::Background => libc::qos_class_t::QOS_CLASS_UTILITY,
        Role::Housekeeping => libc::qos_class_t::QOS_CLASS_BACKGROUND,
    }
}

/// Declare the calling thread's role. Call it as the first statement of a worker's
/// closure, so the whole body runs at the declared class. The result of setting
/// one's own QoS (`_self_np`) is discarded: it cannot fail in a way worth branching on.
///
/// On macOS it first installs `fork_at_default`, so no thread is ever demoted while
/// a fork from it could run at the demoted class.
#[inline]
pub(crate) fn set_self(role: Role) {
    #[cfg(target_os = "macos")]
    {
        fork_at_default::install();
        // SAFETY: sets this thread's own QoS class. Takes no pointers and
        // mutates no shared state, so there is nothing for another thread to
        // observe.
        unsafe {
            libc::pthread_set_qos_class_self_np(qos_class(role), 0);
        }
    }
    #[cfg(all(
        target_os = "linux",
        any(target_arch = "x86_64", target_arch = "aarch64")
    ))]
    if let Some(slice) = linux_slice_ns(role) {
        let _ = set_self_slice(slice);
    }
    #[cfg(not(any(
        target_os = "macos",
        all(
            target_os = "linux",
            any(target_arch = "x86_64", target_arch = "aarch64")
        )
    )))]
    let _ = role;
}

/// Spawn the named worker thread `name` whose WHOLE body runs at `role`: the
/// role is declared as the first thing the new thread does, on that thread.
///
/// The one way a lane thread is made, so none can be spawned undeclared or
/// declared on the wrong side. A role does not reach a thread from the one that
/// creates it (measured 2026-09-15: a thread created from a `QOS_CLASS_UTILITY`
/// thread reads `QOS_CLASS_DEFAULT`), so a `set_self` on the spawning side would
/// read as a declaration and change nothing; an undeclared lane ran at the
/// inherited DEFAULT band — the typing band — while it held the terminal mutex
/// or parsed a package pass's output.
pub(crate) fn spawn_lane_thread<F, T>(
    name: impl Into<String>,
    role: Role,
    f: F,
) -> std::io::Result<std::thread::JoinHandle<T>>
where
    F: FnOnce() -> T + Send + 'static,
    T: Send + 'static,
{
    std::thread::Builder::new()
        .name(name.into())
        .spawn(move || {
            set_self(role);
            f()
        })
}

// A fork from a demoted thread runs at the default class.
//
// A fork-path spawn — any `Command` with a `pre_exec`, a raw `fork()`, a
// `forkpty` — makes a child that holds a copy of every descriptor aterm has
// open (each tab's pty master, the control listener, locks, pipe ends) from its
// copy inside `fork(2)` until the child's exec closes them. `fork(2)` runs on the
// FORKING thread: aterm's address space is copied there, and the child cannot
// run before that copy is done. So that window is paid at the forking thread's
// class, and a `Background` thread paid it at UTILITY. Every fork-path spawn
// found today runs on an undeclared or a raised thread (the update handoff
// worker, the shell spawns, the CLI's own verbs), and the guard below holds for
// whichever demoted thread forks next.
//
// MEASURED 2026-10-01 at load 128-140 on 18 cores (6 P, 12 E): from the
// instant `fork` is called to the child's last `pre_exec` step, `/usr/bin/true`
// from a parent of 2 GB in 6000 resident mappings (the live window had 6643),
// n=300 per row, the rows interleaved, in ms:
//
//   forking thread                   p50     p90     p99     max
//   DEFAULT                          7.2     11.9    20      32
//   UTILITY                          26      133     309     431
//   UTILITY, lifted by this guard    7.3     11.2    26      50
//
// The child's own thread is NOT the slow party: it starts with no class at all
// (base priority 31) whichever class forked it — measured, and read back by the
// kernel as `THREAD_QOS_UNSPECIFIED` in its `pre_exec`. The address-space copy,
// on the forking thread, is what a demoted class starves.
//
// A fork-path spawn (any `Command` with a `pre_exec`, a raw `fork()`, a `forkpty`)
// makes a child that holds a copy of every descriptor aterm has open (pty masters,
// the control listener, locks, pipe ends) until its exec closes them, and `fork(2)`
// copies the address space on the forking thread, so that window is paid at the
// forking thread's class. Measured at load ~130 on 18 cores, a 2 GB parent, fork to
// the child's last `pre_exec` step: p99 20 ms from DEFAULT, 309 ms from UTILITY,
// 26 ms from UTILITY lifted by this guard. The child's own thread is not the slow
// party: it starts with no class (`THREAD_QOS_UNSPECIFIED`) whichever class forked it.
//
// The guard is `pthread_atfork`, so no spawn site can forget it: libSystem runs the
// prepare handler on the forking thread before `fork(2)` and the parent handler after
// it returns, for every fork in the process (std's `Command`, aterm-pty's raw `fork`,
// any library's) and for no `posix_spawn`, which copies nothing of aterm's. A thread
// below the default is lifted to it and put back as soon as `fork(2)` returns; a
// thread at or above the default, or opted out of QoS, is left as it is. `set_self`
// installs it before it sets a class, the one way this crate demotes a thread.
#[cfg(target_os = "macos")]
mod fork_at_default {
    use libc::qos_class_t::{QOS_CLASS_BACKGROUND, QOS_CLASS_DEFAULT, QOS_CLASS_UTILITY};
    use std::cell::Cell;

    unsafe extern "C" {
        fn pthread_atfork(
            prepare: Option<extern "C" fn()>,
            parent: Option<extern "C" fn()>,
            child: Option<extern "C" fn()>,
        ) -> libc::c_int;
    }

    thread_local! {
        /// The class and relative priority [`lift`] raised this thread from, for
        /// the fork in flight on it; `None` when it raised nothing.
        static LIFTED_FROM: Cell<Option<(libc::qos_class_t, libc::c_int)>> =
            const { Cell::new(None) };
    }

    /// Bracket every fork in the process with [`lift`] and [`restore`]. Once.
    pub(super) fn install() {
        static INSTALLED: std::sync::Once = std::sync::Once::new();
        INSTALLED.call_once(|| {
            // SAFETY: registers two `extern "C"` functions that live as long as
            // the process; neither unwinds (they cannot panic).
            unsafe { pthread_atfork(Some(lift), Some(restore), None) };
        });
    }

    /// The prepare handler: on the forking thread, before `fork(2)`.
    extern "C" fn lift() {
        let lifted = super::requested_class_of_self().and_then(|(class, relative)| {
            let from = [QOS_CLASS_UTILITY, QOS_CLASS_BACKGROUND]
                .into_iter()
                .find(|below| *below as libc::c_uint == class)?;
            // SAFETY: sets this thread's own class; no pointers, no shared state.
            let rc = unsafe { libc::pthread_set_qos_class_self_np(QOS_CLASS_DEFAULT, 0) };
            (rc == 0).then_some((from, relative))
        });
        // Written on every fork, `None` included: the child copies this slot,
        // and a child that forks again must not "restore" its parent's class.
        LIFTED_FROM.set(lifted);
    }

    /// The parent handler: on the forking thread, once `fork(2)` has returned.
    extern "C" fn restore() {
        if let Some((class, relative)) = LIFTED_FROM.take() {
            // SAFETY: as in `lift`; `class` and `relative` are the pair this
            // thread had requested, read back off it before the fork.
            unsafe { libc::pthread_set_qos_class_self_np(class, relative) };
        }
    }
}

// Linux: the same roles, spoken to EEVDF.
//
// Linux has no QoS class an unprivileged process may raise itself into, but since
// 6.12 EEVDF honours a per-task slice request (`sched_setattr` on a fair policy with
// `sched_runtime` set, allowed unprivileged, range 0.1-100 ms). A shorter slice is a
// latency hint, not a bigger share: the task keeps its weight (its nice, which the
// request carries over) but gets an earlier virtual deadline, so it is picked sooner
// when it wakes onto a busy CPU. Measured at load 65-75 on 20 cores (kernel 6.17), a
// 1 ms sleeper's wake overshoot p99 was 3.1-3.7 ms at the 2.8 ms default slice,
// 1.4-2.5 ms at 1 ms and 1.4-1.6 ms at 0.1 ms; the UI thread is woken several times
// per keystroke (key event, PTY output, present), so the tail is paid more than once.
//
// `SCHED_FLAG_RESET_ON_FORK` stops the hint at this thread: a thread or child process
// it creates (a shell spawned from the UI thread, so every compiler run in it) starts
// back at the default slice. Only `Interactive` and `Responsive` take a slice;
// `Background`/`Housekeeping` are left alone because a Linux nice value, unlike a
// macOS QoS class, is inherited by child processes, and a demoted thread that
// launches an aterm would starve it. Older kernels accept the call and ignore
// `sched_runtime` for a fair policy, so the hint degrades to a no-op.

/// The EEVDF slice request for `role`, in ns, or `None` for the kernel default.
#[cfg(any(
    all(
        target_os = "linux",
        any(target_arch = "x86_64", target_arch = "aarch64")
    ),
    test
))]
const fn linux_slice_ns(role: Role) -> Option<u64> {
    match role {
        Role::Interactive => Some(500_000),
        Role::Responsive => Some(1_000_000),
        Role::Background | Role::Housekeeping => None,
    }
}

/// `struct sched_attr` (`include/uapi/linux/sched/types.h`, VER1 — 56 bytes).
/// Not in `libc`, which is generated from a pinned reference.
#[cfg(any(
    all(
        target_os = "linux",
        any(target_arch = "x86_64", target_arch = "aarch64")
    ),
    test
))]
#[repr(C)]
#[derive(Clone, Debug, Default, PartialEq, Eq)]
struct SchedAttr {
    size: u32,
    sched_policy: u32,
    sched_flags: u64,
    sched_nice: i32,
    sched_priority: u32,
    sched_runtime: u64,
    sched_deadline: u64,
    sched_period: u64,
    sched_util_min: u32,
    sched_util_max: u32,
}

#[cfg(all(target_os = "linux", target_arch = "x86_64"))]
const SYS_SCHED_SETATTR: libc::c_long = 314;
#[cfg(all(target_os = "linux", target_arch = "aarch64"))]
const SYS_SCHED_SETATTR: libc::c_long = 274;
#[cfg(all(target_os = "linux", target_arch = "x86_64"))]
const SYS_SCHED_GETATTR: libc::c_long = 315;
#[cfg(all(target_os = "linux", target_arch = "aarch64"))]
const SYS_SCHED_GETATTR: libc::c_long = 275;
/// `SCHED_FLAG_RESET_ON_FORK`.
#[cfg(any(
    all(
        target_os = "linux",
        any(target_arch = "x86_64", target_arch = "aarch64")
    ),
    test
))]
const SCHED_FLAG_RESET_ON_FORK: u64 = 0x01;

/// The fair-class policies (`SCHED_OTHER`, `SCHED_BATCH`, `SCHED_IDLE`) — the
/// only ones whose `sched_runtime` is an EEVDF slice request. A real-time or
/// deadline thread (only reachable if someone privileged set it) is left
/// exactly as it is.
#[cfg(any(
    all(
        target_os = "linux",
        any(target_arch = "x86_64", target_arch = "aarch64")
    ),
    test
))]
const FAIR_POLICIES: [u32; 3] = [0, 3, 5];

/// The `sched_attr` that asks for `slice_ns` while changing nothing else about
/// `current`: its policy, nice and priority are carried over. A fresh
/// `SCHED_OTHER`/nice-0 struct would reset a user-chosen `SCHED_BATCH`, reset a
/// privileged negative nice, and for an aterm started under `nice` fail outright
/// (lowering nice unprivileged is `EPERM`). `None` for a non-fair policy.
///
/// `None`, too, for a negative nice. The reset-on-fork this request carries also
/// resets a negative nice to 0 in every thread and process the thread creates
/// afterwards (`sched_fork`), and the thread asking is the UI thread, which spawns
/// the shells: an aterm started at nice -5 would hand every shell nice 0. Such a
/// thread already out-ranks its peers, so it keeps the default slice and its
/// inheritance.
#[cfg(any(
    all(
        target_os = "linux",
        any(target_arch = "x86_64", target_arch = "aarch64")
    ),
    test
))]
fn slice_request(current: &SchedAttr, slice_ns: u64) -> Option<SchedAttr> {
    (FAIR_POLICIES.contains(&current.sched_policy) && current.sched_nice >= 0).then(|| SchedAttr {
        size: std::mem::size_of::<SchedAttr>() as u32,
        sched_policy: current.sched_policy,
        // Only the reset-on-fork flag: no UTIL_CLAMP bits, so the clamp
        // fields below (zero) are ignored rather than written.
        sched_flags: SCHED_FLAG_RESET_ON_FORK,
        sched_nice: current.sched_nice,
        sched_priority: current.sched_priority,
        sched_runtime: slice_ns,
        ..SchedAttr::default()
    })
}

/// Request `slice_ns` for the calling thread (tid 0): read its attributes, change
/// only the slice ([`slice_request`]), write them back. The error is for the tests;
/// `set_self` discards it, since a scheduling hint must never make a worker fail.
#[cfg(all(
    target_os = "linux",
    any(target_arch = "x86_64", target_arch = "aarch64")
))]
fn set_self_slice(slice_ns: u64) -> std::io::Result<()> {
    let current = sched_attr_of_self()?;
    let Some(attr) = slice_request(&current, slice_ns) else {
        return Ok(());
    };
    // SAFETY: `sched_setattr(0, &attr, 0)` reads one correctly sized, live
    // `sched_attr` and changes only the calling thread's scheduling hint.
    let rc = unsafe {
        libc::syscall(
            SYS_SCHED_SETATTR,
            0 as libc::c_long,
            &raw const attr,
            0 as libc::c_long,
        )
    };
    if rc == 0 {
        Ok(())
    } else {
        Err(std::io::Error::last_os_error())
    }
}

/// The calling thread's `sched_attr`, read back from the kernel.
#[cfg(all(
    target_os = "linux",
    any(target_arch = "x86_64", target_arch = "aarch64")
))]
fn sched_attr_of_self() -> std::io::Result<SchedAttr> {
    let mut attr = SchedAttr::default();
    // SAFETY: `sched_getattr(0, &mut attr, size, 0)` writes at most `size`
    // bytes into the live, correctly sized local.
    let rc = unsafe {
        libc::syscall(
            SYS_SCHED_GETATTR,
            0 as libc::c_long,
            &raw mut attr,
            std::mem::size_of::<SchedAttr>() as libc::c_long,
            0 as libc::c_long,
        )
    };
    if rc == 0 {
        Ok(attr)
    } else {
        Err(std::io::Error::last_os_error())
    }
}

/// The slice (ns) and flags the calling thread runs with: the Linux twin of
/// `class_of_self`.
#[cfg(all(
    target_os = "linux",
    any(target_arch = "x86_64", target_arch = "aarch64"),
    test
))]
pub(crate) fn slice_of_self() -> Option<(u64, u64)> {
    sched_attr_of_self()
        .ok()
        .map(|attr| (attr.sched_runtime, attr.sched_flags))
}

// CHILD PROCESSES. A role is a THREAD attribute, and it stops at the spawn.
//
// Measured 2026-09-15 on macOS: a thread at UTILITY (`ps` pri 20) or BACKGROUND
// (pri 4) still starts its child at pri 31, the default band. That is the band
// the programs the user's shell starts run in (the shell itself runs above it,
// in the UI tier: `aterm_pty`'s `SHELL_ROLE`). So the `Background`
// workers that launch `atpkg update`, a package verb or a login-shell PATH probe
// handed that work straight back to the scheduler
// as an equal of the foreground TUI: the thread yielded, and the process doing
// the actual CPU work did not. Nothing in aterm attributes the lost time either,
// because the starved party is a sibling process aterm launched.

/// macOS's QoS launcher — the one `aterm-verify` wraps compile-only children in.
/// Absent elsewhere, where a [`command`] child runs as is.
const TASKPOLICY: &str = "/usr/sbin/taskpolicy";

/// The `taskpolicy -c` clamp for a child spawned on behalf of `role`, or `None`
/// when the child keeps the tier it inherits.
///
/// Mirrors [`qos_class`]: a `Background` worker's child runs at UTILITY, the class
/// the worker's own thread runs at, and a `Housekeeping` worker's at BACKGROUND.
/// `Interactive` and `Responsive` rank above the inherited default, and a clamp can
/// only lower a child, so their children are spawned bare.
const fn child_clamp(role: Role) -> Option<&'static str> {
    match role {
        Role::Interactive | Role::Responsive => None,
        Role::Background => Some("utility"),
        Role::Housekeeping => Some("background"),
    }
}

/// A `Command` for `program`, run on behalf of `role`'s work: behind
/// `taskpolicy -c <clamp>` whenever that work ranks below the inherited default, so
/// the child and everything it launches yield to the program being typed into
/// (under `taskpolicy -c utility` a child and its own child both read pri 20,
/// against 31 bare).
///
/// `taskpolicy` replaces itself with the program (`POSIX_SPAWN_SETEXEC`), so
/// `Child::id` is the program's own pid and its parent is aterm: pid-keyed tailers,
/// group kills and `SPAWNER_PID_ENV` checks see what they would bare. Both arms build
/// through [`aterm_uds::spawnfd::command`], so neither inherits a descriptor of
/// aterm's above stdio, on Darwin the spawn is one `posix_spawn` (no fork), and on
/// Windows a console child (an `atpkg.exe` pass) gets a windowless console rather
/// than a visible window.
///
/// Wrapped only when `taskpolicy` exists and `program` is an absolute path to an
/// executable file: a missing program under `taskpolicy` exits 66 rather than
/// failing the spawn, so such a program is spawned bare and fails as its caller
/// expects.
///
/// Not for a child whose pid is attested the moment `spawn` returns: until
/// `taskpolicy` execs the program, that pid runs taskpolicy's image. Nor for a
/// child that launches an aterm: the clamp is inherited, and docs/RELEASING.md
/// measured an aterm under UTILITY starving its paint probe.
pub(crate) fn command(
    role: Role,
    program: impl AsRef<std::path::Path>,
) -> aterm_uds::spawnfd::Command {
    command_with(role, program.as_ref(), std::path::Path::new(TASKPOLICY))
}

/// THE way the GUI starts the co-located `atpkg`: a [`Role::Background`]
/// [`command`], so a package pass — parsing, verifying, unpacking — never runs
/// in the band of the program being typed into. A bare `Command::new(atpkg)`
/// compiles, runs and passes every functional test; it shows up only as echo
/// lag while an update pass verifies a snapshot.
pub(crate) fn atpkg_command(atpkg: impl AsRef<std::path::Path>) -> aterm_uds::spawnfd::Command {
    command(Role::Background, atpkg)
}

/// The package lane's pass child: [`atpkg_command`], except for the FIRST RUN a person
/// waits on (`waited`: the store's first install of the ALab toolset still ahead,
/// `crate::pass_is_waited_on`): told to wait for the toolset, with stubs on PATH blocking
/// until it lands. That pass starts at the default band ([`Role::Responsive`], a bare
/// spawn), an equal of the program being typed into, and so of whatever else keeps a fresh
/// Mac busy. Measured on a contended
/// first launch (14 spinners and a disk copy at the default band, 2026-10-17, n=4 each):
/// clamped, the Trust toolchain landed at 276 s and the pass ended at 357 s; unclamped at
/// 25 s and 31 s. A shell's typing echo during it rose 0.06 ms at p95 (0.16 to 0.22 ms)
/// and fell at p99 (1.60 to 0.62 ms). Every routine pass stays clamped.
pub(crate) fn atpkg_pass_command(
    atpkg: impl AsRef<std::path::Path>,
    waited: bool,
) -> aterm_uds::spawnfd::Command {
    command(
        if waited {
            Role::Responsive
        } else {
            Role::Background
        },
        atpkg,
    )
}

/// [`command`] over an explicit launcher, so the wrapping decision is testable on
/// every Unix against a stand-in.
fn command_with(
    role: Role,
    program: &std::path::Path,
    taskpolicy: &std::path::Path,
) -> aterm_uds::spawnfd::Command {
    match child_clamp(role) {
        Some(clamp)
            if program.is_absolute()
                && is_executable_file(program)
                && is_executable_file(taskpolicy) =>
        {
            let mut wrapped = aterm_uds::spawnfd::command(taskpolicy);
            wrapped.arg("-c").arg(clamp).arg(program);
            wrapped
        }
        _ => aterm_uds::spawnfd::command(program),
    }
}

fn is_executable_file(path: &std::path::Path) -> bool {
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt as _;
        std::fs::metadata(path)
            .is_ok_and(|meta| meta.is_file() && meta.permissions().mode() & 0o111 != 0)
    }
    #[cfg(not(unix))]
    {
        let _ = path;
        false
    }
}

// Lending a waiter's urgency to the worker it waits on.
//
// A channel donates no priority: a UI-thread caller that waits (bounded) for a
// `Background` worker to finish what is queued waits on a UTILITY thread the
// scheduler may run late on a busy Mac. A QoS override (`<pthread/qos.h>`) raises the
// worker for as long as the dependency stands, leaves the class it requested alone,
// and may be ended from any thread, so the worker ends it once the awaited job is
// done. libSystem exports, declared here for the reason the next section gives.
#[cfg(target_os = "macos")]
unsafe extern "C" {
    fn pthread_override_qos_class_start_np(
        thread: libc::pthread_t,
        class: libc::qos_class_t,
        relative_priority: libc::c_int,
    ) -> *mut std::ffi::c_void;
    fn pthread_override_qos_class_end_np(over: *mut std::ffi::c_void) -> libc::c_int;
}

/// A worker raised to [`Role::Responsive`] until this drops: it travels with the job
/// the caller waits on, and the worker drops it once that job is done. Inert where
/// the platform has no override or declined one.
pub(crate) struct Boost {
    #[cfg(target_os = "macos")]
    over: Option<std::ptr::NonNull<std::ffi::c_void>>,
}

// SAFETY: the override is an opaque libSystem object that any thread may end
// (`pthread_override_qos_class_end_np`'s contract), and only the one owner of
// this value ever ends it.
#[cfg(target_os = "macos")]
unsafe impl Send for Boost {}

impl Boost {
    /// Raise `thread`, a worker not yet joined, while the caller waits on it.
    pub(crate) fn start(thread: Option<&std::thread::JoinHandle<()>>) -> Boost {
        #[cfg(target_os = "macos")]
        {
            use std::os::unix::thread::JoinHandleExt as _;
            let over = thread.and_then(|thread| {
                let pthread = thread.as_pthread_t() as libc::pthread_t;
                // SAFETY: the borrowed handle is not joined, so `pthread`
                // names a thread whose handle stays valid across the call.
                std::ptr::NonNull::new(unsafe {
                    pthread_override_qos_class_start_np(pthread, qos_class(Role::Responsive), 0)
                })
            });
            Boost { over }
        }
        #[cfg(not(target_os = "macos"))]
        {
            let _ = thread;
            Boost {}
        }
    }

    /// Whether an override was started.
    #[cfg(all(test, target_os = "macos"))]
    pub(crate) fn armed(&self) -> bool {
        self.over.is_some()
    }
}

#[cfg(target_os = "macos")]
impl Drop for Boost {
    fn drop(&mut self) {
        if let Some(over) = self.over.take() {
            // SAFETY: `over` came from a successful start and is ended once.
            unsafe { pthread_override_qos_class_end_np(over.as_ptr()) };
        }
    }
}

// Reading a class back off a running thread, the only thing the scheduler reads:
// the fork guard reads the class it lifts from, and the tests read where a role landed.
//
// None of these symbols is in `libc` (aterm-libc, generated from a pinned reference:
// regenerate rather than edit); all are ordinary libSystem exports (`<sys/qos.h>`,
// `<pthread/qos.h>`, `<pthread.h>`), declared here. Declared returning `c_uint`, not
// `libc::qos_class_t`: a `repr(u32)` enum return carrying a value outside its six
// discriminants would be undefined behaviour.
#[cfg(target_os = "macos")]
unsafe extern "C" {
    /// Spelled as `main_thread_probe` declares it: one symbol, one signature.
    fn pthread_self() -> *mut std::ffi::c_void;
    fn pthread_get_qos_class_np(
        thread: libc::pthread_t,
        class: *mut libc::c_uint,
        relative_priority: *mut libc::c_int,
    ) -> libc::c_int;
}

#[cfg(all(test, target_os = "macos"))]
unsafe extern "C" {
    fn qos_class_self() -> libc::c_uint;
}

/// The class and relative priority the calling thread has requested (its own word,
/// which no override another thread pushes onto it changes), or `None` if the
/// platform declined to answer.
#[cfg(target_os = "macos")]
fn requested_class_of_self() -> Option<(libc::c_uint, libc::c_int)> {
    let mut class: libc::c_uint = 0;
    let mut relative: libc::c_int = 0;
    // SAFETY: both out-parameters are live, correctly typed locals, and
    // `pthread_self` names the calling thread, alive across the call.
    let rc = unsafe { pthread_get_qos_class_np(pthread_self().addr(), &mut class, &mut relative) };
    (rc == 0).then_some((class, relative))
}

/// The class the calling thread is actually running at.
#[cfg(all(test, target_os = "macos"))]
pub(crate) fn class_of_self() -> u32 {
    // SAFETY: reads the calling thread's own class. Takes no pointers and
    // mutates no shared state.
    unsafe { qos_class_self() }
}

/// The class another thread is actually running at, named by its pthread handle
/// (`std::os::unix::thread::JoinHandleExt::as_pthread_t`), or `None` if the
/// platform declined to report one.
#[cfg(all(test, target_os = "macos"))]
pub(crate) fn class_of(thread: libc::pthread_t) -> Option<u32> {
    let mut class: libc::c_uint = 0;
    let mut relative: libc::c_int = 0;
    // SAFETY: both out-parameters are live, correctly typed locals, and the
    // caller's `JoinHandle` keeps `thread` alive across the call.
    let rc = unsafe { pthread_get_qos_class_np(thread, &mut class, &mut relative) };
    (rc == 0).then_some(class)
}

/// The class [`set_self`] puts a thread at, to compare against one read back off
/// the running thread.
#[cfg(all(test, target_os = "macos"))]
pub(crate) fn class_for(role: Role) -> u32 {
    qos_class(role) as u32
}

/// The longest timer [`sleep_until`] asks for within [`COALESCE_MARGIN`] of its
/// deadline.
pub(crate) const PRECISE_SLICE: std::time::Duration = std::time::Duration::from_millis(2);

/// How far before its deadline [`sleep_until`] stops asking for one long timer
/// and takes [`PRECISE_SLICE`]s: more than the coalescing measured below.
pub(crate) const COALESCE_MARGIN: std::time::Duration = std::time::Duration::from_millis(150);

/// Sleep until `deadline`, never asking for a timer that ends within
/// [`COALESCE_MARGIN`] of it longer than [`PRECISE_SLICE`].
///
/// macOS coalesces a low-class thread's timers. Measured 2026-10-20 on an M4
/// Max under macOS 27: a 20 or 100 ms `nanosleep` woke 61-75 ms late at UTILITY
/// (what [`Role::Background`] runs at) and up to 100 ms late at BACKGROUND,
/// against 5 ms at DEFAULT; the same waits taken in 2 ms slices woke 1.1-2.4 ms
/// late at UTILITY. Use it where someone waits behind a low-class thread's
/// deadline: the warm spare's verification sleeps out its scan window at its
/// hold, and the spare is not ready, so not adoptable, until it wakes. Measured
/// with an empty `.zshrc`/`.bash_profile`, n=20 each: a spare came back 343-345 ms
/// after its adoption with one plain sleep, 283-289 ms with this one, and
/// 180-188 ms before that wait existed (the rest is the 100 ms window itself). A
/// wait nobody stands behind keeps a plain sleep.
pub(crate) fn sleep_until(deadline: std::time::Instant) {
    sleep_until_with(deadline, std::time::Instant::now, std::thread::sleep);
}

/// [`sleep_until`] over an injected clock and sleeper.
fn sleep_until_with(
    deadline: std::time::Instant,
    mut now: impl FnMut() -> std::time::Instant,
    mut sleep: impl FnMut(std::time::Duration),
) {
    loop {
        let left = deadline.saturating_duration_since(now());
        if left.is_zero() {
            return;
        }
        sleep(if left > COALESCE_MARGIN {
            left - COALESCE_MARGIN
        } else {
            left.min(PRECISE_SLICE)
        });
    }
}

/// The class a worker that declares no role runs at — what every silent `spawn`
/// in this crate gets, and what a deleted `set_self` line leaves behind.
#[cfg(all(test, target_os = "macos"))]
pub(crate) const UNDECLARED_CLASS: u32 = libc::qos_class_t::QOS_CLASS_DEFAULT as u32;

#[cfg(test)]
mod tests {
    use super::*;

    /// [`sleep_until`] takes a long wait as one timer that ends a
    /// [`COALESCE_MARGIN`] early and the rest in [`PRECISE_SLICE`]s, and returns
    /// once the deadline has passed, never before. Over a fake clock: no real
    /// time is slept. NEGATIVE CONTROL: one `sleep(left)` (the plain sleep a
    /// low-class thread's timer coalescing makes up to 75 ms late) asks for a
    /// timer that ends at the deadline itself and fails the slice assertion.
    #[test]
    fn a_precise_sleep_asks_for_no_long_timer_near_its_deadline() {
        use std::cell::Cell;
        use std::time::{Duration, Instant};
        let start = Instant::now();
        for wait_ms in [0u64, 1, 3, 100, 149, 151, 2_000] {
            let deadline = start + Duration::from_millis(wait_ms);
            let clock = Cell::new(start);
            let asked = std::cell::RefCell::new(Vec::new());
            sleep_until_with(
                deadline,
                || clock.get(),
                |d| {
                    asked.borrow_mut().push(d);
                    clock.set(clock.get() + d);
                },
            );
            assert!(clock.get() >= deadline, "{wait_ms} ms: returned early");
            let mut at = start;
            for d in asked.borrow().iter() {
                let ends = at + *d;
                assert!(
                    ends + COALESCE_MARGIN <= deadline || *d <= PRECISE_SLICE,
                    "{wait_ms} ms: a {d:?} timer ending {:?} before the deadline",
                    deadline.saturating_duration_since(ends)
                );
                at = ends;
            }
        }
    }

    /// A [`Boost`] raises the worker's base priority while it stands and puts
    /// it back when it drops, read off the kernel's own record of the thread
    /// (an override is invisible to the class a thread reads of itself).
    #[cfg(target_os = "macos")]
    #[test]
    fn a_boost_raises_the_worker_while_it_stands_and_no_longer() {
        unsafe extern "C" {
            fn pthread_threadid_np(thread: libc::pthread_t, id: *mut u64) -> libc::c_int;
        }
        /// `<sys/proc_info.h>`'s by-thread-id flavor of `PROC_PIDTHREADINFO`.
        const PROC_PIDTHREADID64INFO: libc::c_int = 15;
        /// `struct proc_threadinfo` (`<sys/proc_info.h>`, not in the generated
        /// `libc`): 112 bytes, the base priority `pth_priority` at offset 40.
        #[repr(C)]
        struct ProcThreadInfo {
            _user_and_system_time: [u64; 2],
            _usage_policy_state_flags_sleep_curpri: [i32; 6],
            priority: i32,
            _max_priority: i32,
            _name: [u8; 64],
        }
        const _: () = assert!(size_of::<ProcThreadInfo>() == 112);
        let priority_of = |thread: libc::pthread_t| {
            let mut id = 0u64;
            // SAFETY: `thread` is a live, unjoined thread; `id` a writable slot.
            assert_eq!(unsafe { pthread_threadid_np(thread, &mut id) }, 0);
            let mut info = std::mem::MaybeUninit::<ProcThreadInfo>::uninit();
            let size = size_of::<ProcThreadInfo>() as libc::c_int;
            // SAFETY: `info` is `size` writable bytes laid out as the record
            // this flavor fills (its size pinned above).
            let read = unsafe {
                libc::proc_pidinfo(
                    libc::getpid(),
                    PROC_PIDTHREADID64INFO,
                    id,
                    info.as_mut_ptr().cast(),
                    size,
                )
            };
            assert_eq!(read, size, "the thread's record reads");
            // SAFETY: the exact-size read initialized every (integer) field.
            unsafe { info.assume_init() }.priority
        };
        use std::os::unix::thread::JoinHandleExt as _;
        let (declared_tx, declared) = std::sync::mpsc::channel::<()>();
        let (release, released) = std::sync::mpsc::channel::<()>();
        let worker = std::thread::spawn(move || {
            set_self(Role::Background);
            let _ = declared_tx.send(());
            let _ = released.recv();
        });
        declared.recv().expect("the worker declared its role");
        let thread = worker.as_pthread_t() as libc::pthread_t;
        let parked = priority_of(thread);
        let boost = Boost::start(Some(&worker));
        assert!(boost.armed(), "an unjoined thread takes an override");
        let raised = priority_of(thread);
        assert!(raised > parked, "boosted {raised}, parked {parked}");
        drop(boost);
        assert_eq!(priority_of(thread), parked, "the override ended with it");
        let _ = release.send(());
        worker.join().expect("the worker");
    }

    /// Each role, declared on a real thread, is the class the kernel reports that
    /// thread running at.
    #[cfg(target_os = "macos")]
    #[test]
    fn a_declared_role_really_lands_on_the_running_thread() {
        for role in [
            Role::Interactive,
            Role::Responsive,
            Role::Background,
            Role::Housekeeping,
        ] {
            let observed = std::thread::spawn(move || {
                set_self(role);
                class_of_self()
            })
            .join()
            .expect("the probe thread");
            assert_eq!(
                observed,
                class_for(role),
                "{role:?} was declared, but the thread runs at 0x{observed:x}"
            );
        }
    }

    /// The class a fork from the calling thread ran at, as the forked child reads it
    /// before its exec: its copy of this thread's QoS word, taken when `fork(2)` copied
    /// the address space ([`fork_at_default`]). Not the child's base priority, which
    /// starts with no class whichever class forked it.
    #[cfg(target_os = "macos")]
    fn class_a_fork_ran_at() -> u32 {
        use std::io::Read as _;
        use std::os::fd::AsRawFd as _;
        use std::os::unix::process::CommandExt as _;

        let (mut probe, probe_end) = std::io::pipe().unwrap();
        // std's Command on purpose: the probe is a `pre_exec` in a forked child.
        let mut command = std::process::Command::new("/usr/bin/true");
        // SAFETY: the probe loads this thread's QoS word (a thread-specific
        // slot) and makes one 4-byte `write`; it allocates nothing and takes
        // no lock. The closure owns the write end.
        unsafe {
            command.pre_exec(move || {
                let class = qos_class_self();
                let _ = libc::write(
                    probe_end.as_raw_fd(),
                    (&raw const class).cast(),
                    size_of::<libc::c_uint>(),
                );
                Ok(())
            });
        }
        let status = command.status().unwrap();
        assert!(status.success(), "{status}");
        // The probe wrote before the exec, and `status` returned after it: the
        // bytes are in the pipe, and no EOF is needed to read them.
        let mut class = [0; 4];
        probe
            .read_exact(&mut class)
            .expect("the pre_exec probe wrote nothing before the exec");
        u32::from_ne_bytes(class)
    }

    /// A fork from a demoted thread runs at the default class, and the thread has its
    /// own class back once `spawn` returns ([`fork_at_default`]). Read off the fork,
    /// not timed. A thread at or above the default forks at its own class (the guard
    /// never lowers), which also shows the probe reads the forking thread's class.
    /// Negative control: without the guard a `Background` thread forks at UTILITY
    /// (0x11).
    #[cfg(target_os = "macos")]
    #[test]
    fn a_fork_runs_at_the_default_class_or_above_and_the_role_comes_back() {
        for role in [
            Role::Interactive,
            Role::Responsive,
            Role::Background,
            Role::Housekeeping,
        ] {
            let (at_fork, after) = std::thread::spawn(move || {
                set_self(role);
                (class_a_fork_ran_at(), class_of_self())
            })
            .join()
            .expect("the forking thread");
            let declared = class_for(role);
            assert_eq!(
                at_fork,
                declared.max(UNDECLARED_CLASS),
                "a {role:?} thread (0x{declared:x}) forked at 0x{at_fork:x}: a copy of \
                 aterm holding every descriptor it has open waited out its exec at a \
                 demoted class"
            );
            assert_eq!(
                after, declared,
                "a {role:?} thread came back from its fork at 0x{after:x}, not its own \
                 class"
            );
        }
    }

    /// The order is the contract: cosmetic work never outranks work a human is
    /// waiting on. A wrong role is invisible at its spawn site and shows up only as
    /// latency on a loaded machine.
    #[cfg(target_os = "macos")]
    #[test]
    fn roles_rank_interactive_above_cosmetic_work() {
        let rank = |role| qos_class(role) as u32;
        assert!(
            rank(Role::Interactive) > rank(Role::Responsive),
            "input egress must outrank the PTY drain"
        );
        assert!(
            rank(Role::Responsive) > rank(Role::Background),
            "the PTY drain must outrank font warming and package probes"
        );
        assert!(
            rank(Role::Background) > rank(Role::Housekeeping),
            "useful background work must outrank an opportunistic disk sweep"
        );
    }

    /// The Linux slice map keeps the enum's order: the keystroke path asks for
    /// the shortest slice, the PTY drain a longer one, and nothing below the
    /// PTY drain asks at all (a Linux nice would be inherited by children).
    #[test]
    fn only_the_keystroke_path_roles_request_a_linux_slice() {
        let interactive = linux_slice_ns(Role::Interactive).expect("interactive slice");
        let responsive = linux_slice_ns(Role::Responsive).expect("responsive slice");
        assert!(interactive < responsive);
        // The kernel's accepted range for a fair-policy slice request.
        for slice in [interactive, responsive] {
            assert!((100_000..=100_000_000).contains(&slice), "{slice} ns");
        }
        assert_eq!(linux_slice_ns(Role::Background), None);
        assert_eq!(linux_slice_ns(Role::Housekeeping), None);
    }

    /// The slice request changes the slice and nothing else: a `nice`d aterm keeps its
    /// nice (writing 0 back would be an unprivileged nice decrease, `EPERM`), a
    /// `SCHED_BATCH` thread stays batch, and a real-time thread is not touched.
    /// Negative control: a fresh policy-0, nice-0 request differs from it.
    #[test]
    fn a_slice_request_carries_policy_and_nice_over() {
        let niced = SchedAttr {
            sched_policy: 0,
            sched_nice: 10,
            ..SchedAttr::default()
        };
        let req = slice_request(&niced, 500_000).expect("fair policy");
        assert_eq!(req.sched_nice, 10, "a positive nice survives");
        assert_eq!(req.sched_policy, 0);
        assert_eq!(req.sched_runtime, 500_000);
        assert_eq!(req.sched_flags, SCHED_FLAG_RESET_ON_FORK);
        assert_eq!(req.size as usize, std::mem::size_of::<SchedAttr>());
        assert_eq!(std::mem::size_of::<SchedAttr>(), 56, "sched_attr VER1");
        let first_cut = SchedAttr {
            size: 56,
            sched_flags: SCHED_FLAG_RESET_ON_FORK,
            sched_runtime: 500_000,
            ..SchedAttr::default()
        };
        assert_ne!(req, first_cut, "the old request reset nice to 0");

        let batch = SchedAttr {
            sched_policy: 3,
            ..SchedAttr::default()
        };
        assert_eq!(slice_request(&batch, 1_000_000).unwrap().sched_policy, 3);

        let fifo = SchedAttr {
            sched_policy: 1,
            sched_priority: 10,
            ..SchedAttr::default()
        };
        assert_eq!(slice_request(&fifo, 500_000), None, "RT is left alone");

        // A negative nice would be reset to 0 in every child the thread
        // spawns once reset-on-fork is set, so it is not touched at all.
        let boosted = SchedAttr {
            sched_policy: 0,
            sched_nice: -5,
            ..SchedAttr::default()
        };
        assert_eq!(
            slice_request(&boosted, 500_000),
            None,
            "a negative nice keeps its inheritance"
        );
    }

    /// The same, against the real kernel: a thread `nice`d to +5 (an unprivileged
    /// increase) still has its slice request accepted, and keeps nice +5.
    #[cfg(target_os = "linux")]
    #[cfg(any(target_arch = "x86_64", target_arch = "aarch64"))]
    #[test]
    fn a_niced_thread_keeps_its_nice_and_still_takes_the_slice() {
        let (rc, attr) = std::thread::spawn(|| {
            // SAFETY: Linux nice is per-thread; `who = 0` is this thread.
            let reniced = unsafe { libc::setpriority(libc::PRIO_PROCESS, 0, 5) };
            assert_eq!(reniced, 0, "raising our own nice is unprivileged");
            let rc = set_self_slice(linux_slice_ns(Role::Interactive).unwrap());
            (
                rc.map_err(|e| e.to_string()),
                sched_attr_of_self().expect("getattr"),
            )
        })
        .join()
        .expect("the niced thread");
        assert_eq!(rc, Ok(()), "the slice request must not be refused");
        assert_eq!(attr.sched_nice, 5, "the thread's nice was preserved");
    }

    /// The slice, read off a running thread, and the reset-on-fork that keeps it from
    /// leaking into a thread (or shell) the declaring thread creates. A pre-6.12
    /// kernel accepts and ignores the request; that is reported, not failed.
    #[cfg(all(
        target_os = "linux",
        any(target_arch = "x86_64", target_arch = "aarch64")
    ))]
    #[test]
    fn a_linux_slice_lands_on_the_declaring_thread_and_stops_there() {
        let (declared, child) = std::thread::spawn(|| {
            set_self(Role::Interactive);
            let declared = slice_of_self().expect("sched_getattr on self");
            let child = std::thread::spawn(slice_of_self)
                .join()
                .expect("the child thread")
                .expect("sched_getattr on the child");
            (declared, child)
        })
        .join()
        .expect("the declaring thread");
        let want = linux_slice_ns(Role::Interactive).unwrap();
        if declared.0 != want {
            eprintln!(
                "kernel ignored the slice request (runtime {} ns): pre-6.12 EEVDF, \
                 the hint is a no-op here",
                declared.0
            );
            return;
        }
        assert_ne!(
            declared.1 & SCHED_FLAG_RESET_ON_FORK,
            0,
            "reset-on-fork must be set"
        );
        assert_ne!(
            child.0, want,
            "a thread created by the declaring thread inherited its slice"
        );
    }

    #[cfg(unix)]
    fn argv(command: &aterm_uds::spawnfd::Command) -> Vec<String> {
        std::iter::once(command.get_program())
            .chain(command.get_args())
            .map(|arg| arg.to_string_lossy().into_owned())
            .collect()
    }

    /// The wrapping decision, against a stand-in launcher so it runs the same on
    /// every Unix: work below the inherited default runs behind its clamp when both
    /// ends are real; everything else is the program alone, so a missing tool still
    /// fails at `spawn`.
    #[cfg(unix)]
    #[test]
    fn only_below_default_work_with_both_ends_real_is_spawned_behind_a_clamp() {
        use std::os::unix::fs::PermissionsExt as _;
        let dir = std::env::temp_dir().join(format!("aterm-qos-launcher-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir(&dir).unwrap();
        let launcher = dir.join("taskpolicy");
        std::fs::write(&launcher, "#!/bin/sh\nexit 0\n").unwrap();
        std::fs::set_permissions(&launcher, std::fs::Permissions::from_mode(0o755)).unwrap();
        let tool = std::path::Path::new("/bin/sh");
        let l = launcher.to_string_lossy().into_owned();

        let run = |role, program: &std::path::Path, launcher: &std::path::Path| {
            argv(&command_with(role, program, launcher))
        };
        assert_eq!(
            run(Role::Background, tool, &launcher),
            [l.as_str(), "-c", "utility", "/bin/sh"]
        );
        assert_eq!(
            run(Role::Housekeeping, tool, &launcher),
            [l.as_str(), "-c", "background", "/bin/sh"]
        );
        for role in [Role::Interactive, Role::Responsive] {
            assert_eq!(
                run(role, tool, &launcher),
                ["/bin/sh"],
                "{role:?} is above the default"
            );
        }
        let missing = dir.join("no-such-tool");
        assert_eq!(
            run(Role::Background, &missing, &launcher),
            [missing.to_string_lossy().into_owned()],
            "a missing program must still fail at spawn"
        );
        assert_eq!(
            run(Role::Background, std::path::Path::new("sh"), &launcher),
            ["sh"]
        );
        assert_eq!(
            run(Role::Background, tool, &dir.join("no-launcher")),
            ["/bin/sh"],
            "no launcher, no wrapper"
        );
        std::fs::remove_dir_all(&dir).unwrap();
    }

    /// A `Background` child really starts below the inherited band (pri 31).
    #[cfg(target_os = "macos")]
    #[test]
    fn a_background_child_starts_below_the_inherited_band() {
        let output = command(Role::Background, "/bin/sh")
            .args(["-c", "/bin/ps -o pri= -p $$"])
            .output()
            .unwrap();
        assert!(output.status.success(), "{output:?}");
        let said = String::from_utf8_lossy(&output.stdout);
        let pri: i32 = said
            .trim()
            .parse()
            .unwrap_or_else(|_| panic!("no priority in {said:?}"));
        assert!(
            pri <= 20,
            "a Background child ran at pri {pri}, not utility (20)"
        );
    }
}
