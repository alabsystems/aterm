// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Thread SCHEDULING ROLE, declared once per worker.
//!
//! macOS schedules by Quality of Service, and on Apple Silicon QoS also steers
//! P-core versus E-core placement. A thread that never declares one inherits
//! `QOS_CLASS_DEFAULT`, which sits just below `USER_INITIATED` and therefore
//! competes with the UI thread's work whenever the machine is saturated.
//!
//! That default is wrong for most of this app. aterm spawns dozens of workers —
//! font warmers, image/video encoders, update checkers, package probes, log
//! flushers — and essentially all of them do work no human
//! is waiting on. Left at the default they are indistinguishable, to the
//! scheduler, from the thread drawing the next frame. Under load that is
//! precisely backwards: the cosmetic work keeps its share while keystroke
//! handling waits, which is felt as typing lag rather than as a slow font warm.
//!
//! Declaring the role makes the priority ORDER explicit and reviewable, instead
//! of an accident of which threads happened to get a `pthread_set_qos_class`
//! call. The scale is deliberately coarse — four roles, chosen by asking "who is
//! waiting for this?" — because a finer one invites per-thread tuning that no
//! one can reason about globally.
//!
//! Call sites stay platform-neutral: on macOS a role is a QoS class, on Linux
//! (x86_64/aarch64) the two roles at or above the UI thread are an EEVDF slice
//! request (see "LINUX" below), and everywhere else a role is a no-op.
//!
//! PROVENANCE. This module is a HAND-PORT of commit `61a6c8b62`
//! (`fix/event-loop-wake-spin-v2`, 2026-08-11), which found thread QoS to be
//! the second cause of the measured keystroke-dequeue lag: 41 named threads,
//! 2 declaring a class. It is a port and not a merge because main moved ~2100
//! commits past that branch's base before it could land, so no hunk of it
//! applies textually. The enum, its class mapping and `set_self` are carried
//! over verbatim; the docs gain the port-time floor rule below and the tests
//! gain one extra rank assertion; each worker's role is re-derived against
//! today's spawn site. One rule was added at port time
//! that the branch only applied to `aterm-scrollback-compress`: a worker that
//! initialises or holds a lock the UI thread contends (a `Mutex`, or a
//! `OnceLock` the UI thread's own first use would block on) is NOT demoted
//! below [`Role::Responsive`], because a descheduled lock holder at a lower
//! class is a priority inversion — the one hazard a QoS port can introduce.

/// What a thread's work is worth relative to the human at the keyboard.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Role {
    /// The human is watching this land RIGHT NOW: input egress, and anything
    /// else on the keystroke→glass path. Ranks at or above the UI thread.
    Interactive,
    /// The human asked for it and is waiting, but not frame-by-frame: PTY drain
    /// and parse, reflow. Below the UI thread, above everything cosmetic.
    ///
    /// Also the FLOOR for any worker that holds or initialises a lock the UI
    /// thread contends. Matching the PTY drain here does not make such work
    /// urgent; it shortens the window in which a lock holder can sit
    /// descheduled with the UI thread queued behind it.
    Responsive,
    /// Useful work nobody is blocked on: font warming, encoding, log flushing,
    /// package/update probes. Must never delay a keystroke.
    Background,
    /// Work with NO deadline at all, which may therefore run arbitrarily late:
    /// opportunistic disk sweeps and prefetch. Yields to literally everything.
    ///
    /// "Arbitrarily late" is the whole contract, and it is stricter than it
    /// sounds. A `QOS_CLASS_BACKGROUND` thread on a saturated machine can be
    /// starved for many seconds, so anything holding a lock, enforcing a
    /// timeout, or owing cleanup that something else waits on does NOT belong
    /// here. Process REAPERS in particular look like housekeeping and are not:
    /// they enforce a kill-and-reap contract on a real deadline, and a starved
    /// reaper leaks the very processes whose CPU use degrades the machine. They
    /// take [`Role::Background`]. (Pinned by experience — putting the managed
    /// runtime's reapers here made their 500 ms kill-and-reap assertions fail
    /// under load; so did the artifact-reply quarantine reaper, whose guards
    /// hold capture capacity, by 60 s, 2026-09-30.)
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

/// Declare the CALLING thread's role. Call it as the first statement of a
/// worker's closure, so the whole body runs at the declared class.
///
/// Setting a thread's own QoS is the supported spelling (`_self_np`); it cannot
/// fail in a way worth branching on, so the result is discarded.
///
/// Every fork the thread makes afterwards runs at the default class or above
/// (`fork_at_default`, macOS), installed here BEFORE the class is set, so no
/// thread is ever demoted while a fork from it could run at the demoted class.
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

// ---------------------------------------------------------------------------
// A FORK FROM A DEMOTED THREAD RUNS AT THE DEFAULT CLASS.
//
// A fork-path spawn — any `Command` with a `pre_exec`, a raw `fork()`, a
// `forkpty` — makes a child that holds a copy of every descriptor aterm has
// open (each tab's pty master, the control listener, locks, pipe ends) from its
// copy inside `fork(2)` until the child's exec closes them. `fork(2)` runs on the
// FORKING thread: aterm's address space is copied there, and the child cannot
// run before that copy is done. So that window is paid at the forking thread's
// class, and a `Background` thread paid it at UTILITY. The retired Smart Titles
// worker starting its managed Ollama daemon (`setsid` and `InheritStrip` in a
// `pre_exec`) was that fork; every other fork-path spawn found runs on an
// undeclared or a raised thread (the update handoff worker, the shell spawns,
// the CLI's own verbs), and the guard below holds for whichever demoted thread
// forks next.
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
// THE GUARD IS `pthread_atfork`, so no spawn site can forget it: libSystem runs
// the prepare handler on the forking thread before `fork(2)` and the parent
// handler after it returns, for every fork in the process — std's `Command`
// fork path, aterm-pty's raw `fork`, any library's — and for no `posix_spawn`
// (aterm-pty measured 1 call per fork, 0 per spawn), which copies nothing of
// aterm's and so needs no lift. A thread below the default is lifted to the
// default; the parent handler puts its class and relative priority back as soon
// as `fork(2)` returns, and it waits for the child's exec at its own class. A
// thread at or above the default, or one opted out of QoS, is left exactly as
// it is: the guard never lowers anything.
//
// Installed by `set_self` BEFORE it sets a class — the one way this crate
// demotes a thread.
// ---------------------------------------------------------------------------
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

// ---------------------------------------------------------------------------
// LINUX: THE SAME ROLES, SPOKEN TO EEVDF.
//
// Until this, every function above was a no-op off macOS, so on Linux the
// keystroke path (the UI thread, the reply writer, the PTY drain) woke with
// exactly the latency of the compilers the user runs beside it. Linux has no
// QoS class an unprivileged process may raise itself into, but since 6.12
// EEVDF honours a per-task SLICE request (`sched_setattr` on a fair policy
// with `sched_runtime` set, allowed unprivileged, range 0.1-100 ms). A shorter
// slice is a LATENCY hint, not a bigger share: the task keeps its weight
// (its nice, which the request carries over untouched) and so its fair share
// of CPU, but gets an earlier virtual deadline and is picked sooner when it
// wakes onto a busy CPU.
//
// MEASURED 2026-09-24 on the Linux daily driver (aarch64, 20 cores, kernel
// 6.17, load average 65-75 from parallel agent builds): a thread sleeping
// 1 ms and doing a little work per wake, 3000 wakes per run, overshoot at the
// kernel's 2.8 ms default slice p99 3.1-3.7 ms / max 5.2-5.5 ms; at a 1 ms
// slice p99 1.4-2.5 ms / max 3.0-3.7 ms; at 0.1 ms p99 1.4-1.6 ms / max
// 2.2-2.7 ms. The UI thread is woken several times per keystroke (key event,
// PTY output, present), so that tail is paid more than once per echo.
//
// `SCHED_FLAG_RESET_ON_FORK` is set so the hint stops at this thread: a
// thread or a CHILD PROCESS it creates (a shell spawned from the UI thread,
// and therefore every compiler the user runs in it) starts back at the
// default slice — measured on the same kernel, both read `runtime=2800000`
// back after the parent set 500 µs. Only the roles that rank at or above the
// UI thread take a slice; `Background`/`Housekeeping` are left alone because
// a Linux nice value, unlike a macOS QoS class, IS inherited by child
// processes, and a demoted thread that launches an aterm would starve it.
// Older kernels accept the call and ignore `sched_runtime` for a fair
// policy, so the hint degrades to a no-op rather than an error.
// ---------------------------------------------------------------------------

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
/// Not in `libc`, which is generated from a pinned reference; declared here the
/// way this module declares the Darwin constants.
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

/// The `sched_attr` that asks for `slice_ns` while changing NOTHING ELSE about
/// `current` — its policy, nice and priority are carried over. Writing a fresh
/// `SCHED_OTHER`/nice-0 struct instead would silently reset a user-chosen
/// `SCHED_BATCH`, reset a privileged negative nice, and — for an aterm started
/// under `nice` — FAIL outright (lowering nice unprivileged is `EPERM`), so the
/// slice would never land. `None` for a non-fair policy.
///
/// `None`, too, for a NEGATIVE nice. The reset-on-fork this request must carry
/// (so the slice stops at this thread) also resets a negative nice to 0 in
/// every thread and process the thread creates afterwards (`sched_fork`), and
/// the thread asking is the UI thread, which spawns the shells. An aterm the
/// user started at nice -5 (privileged, `RLIMIT_NICE`, a unit's `Nice=`) would
/// otherwise hand every shell, and every compiler in it, nice 0 instead of the
/// -5 it inherited before: exactly the "worse for a niced aterm" this request
/// must never be. Such a thread already out-ranks its peers; it keeps the
/// kernel default slice and its inheritance.
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

/// Request `slice_ns` for the CALLING thread (tid 0): read its attributes,
/// change only the slice ([`slice_request`]), write them back. The error is
/// returned for the tests; `set_self` discards it for the same reason it
/// discards macOS's — a scheduling hint must never be the reason a worker
/// fails.
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

/// The CALLING thread's `sched_attr`, read back from the kernel.
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

/// The slice (ns) and flags the CALLING thread runs with — the Linux twin of
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

// ---------------------------------------------------------------------------
// CHILD PROCESSES. A role is a THREAD attribute, and it stops at the spawn.
//
// Measured 2026-09-15 on macOS: a thread at UTILITY (`ps` pri 20) or BACKGROUND
// (pri 4) still starts its child at pri 31, the default band. That is the band
// the user's shell and the program being typed into run in. So the `Background`
// workers that launch `atpkg update`, a package verb or a login-shell PATH probe
// handed that work straight back to the scheduler
// as an equal of the foreground TUI: the thread yielded, and the process doing
// the actual CPU work did not. Nothing in aterm attributes the lost time either,
// because the starved party is a sibling process aterm launched.
// ---------------------------------------------------------------------------

/// macOS's QoS launcher — the one `aterm-verify` wraps compile-only children in.
/// Absent elsewhere, where a [`command`] child runs as is.
const TASKPOLICY: &str = "/usr/sbin/taskpolicy";

/// The `taskpolicy -c` clamp for a child spawned on behalf of `role`, or `None`
/// when the child keeps the tier it inherits.
///
/// Mirrors [`qos_class`]: a `Background` worker's child runs at UTILITY, the class
/// the worker's own thread runs at, and a `Housekeeping` worker's at BACKGROUND.
/// `Interactive` and `Responsive` rank ABOVE the inherited default, and a clamp can
/// only lower a child, so their children are spawned bare.
const fn child_clamp(role: Role) -> Option<&'static str> {
    match role {
        Role::Interactive | Role::Responsive => None,
        Role::Background => Some("utility"),
        Role::Housekeeping => Some("background"),
    }
}

/// A `Command` for `program`, run on behalf of `role`'s work: behind
/// `taskpolicy -c <clamp>` whenever that work ranks below the inherited default,
/// so the child — and everything it launches — yields to the program being typed
/// into. Measured: under `taskpolicy -c utility` a child and its own child both
/// read pri 20, against 31 spawned bare.
///
/// A WRAPPER, NOT A HOOK OF OURS. `taskpolicy` replaces itself with the program
/// (`POSIX_SPAWN_SETEXEC`), so `Child::id` is the program's own pid and its parent
/// is aterm: pid-keyed tailers, group kills and `SPAWNER_PID_ENV` checks see exactly
/// what they saw before. (The `Command` comes from `aterm_uds::spawnfd::command`, so
/// neither `taskpolicy` nor the program inherits a descriptor of aterm's above stdio,
/// and on Darwin the spawn is one `posix_spawn`: no fork of the window.)
///
/// Wrapped only when both ends are real: `taskpolicy` exists and `program` is an
/// absolute path to an executable file. A missing program under `taskpolicy` is an
/// exit 66 rather than a spawn error, so such a program is spawned bare and still
/// fails the way its caller expects.
///
/// NOT FOR a child whose pid is attested the moment `spawn` returns: until
/// `taskpolicy` execs the program, that pid is still running taskpolicy's image.
/// NOT FOR a child that launches an aterm either: the clamp is inherited, and
/// docs/RELEASING.md measured an aterm under UTILITY starving its paint probe.
///
/// NO CONSOLE WINDOW ON WINDOWS. Both arms build through
/// [`aterm_uds::spawnfd::command`]: the window has no console, and every console
/// child it started bare — each `atpkg.exe` pass, a Settings ▸ Packages verb, the
/// daily `machine apply` — got a VISIBLE console window for its whole life
/// (measured 2026-09-30: 9–18 s per pass, at every launch and update). It gets a
/// windowless console now, which atpkg's own children (curl, `cmd /C mklink`)
/// share.
pub(crate) fn command(
    role: Role,
    program: impl AsRef<std::path::Path>,
) -> aterm_uds::spawnfd::Command {
    command_with(role, program.as_ref(), std::path::Path::new(TASKPOLICY))
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

// ---------------------------------------------------------------------------
// LENDING A WAITER'S URGENCY TO THE WORKER IT WAITS ON.
//
// A channel donates no priority: a UI-thread caller that waits (bounded) for
// a `Background` worker to finish what is queued waits on a UTILITY thread
// the scheduler may run late on a busy Mac. A QoS OVERRIDE (`<pthread/qos.h>`)
// raises the worker for exactly as long as the dependency stands, leaves the
// class it requested alone, and may be ended from any thread — so the worker
// ends it itself once the awaited job is done. libSystem exports, declared
// here for the reason the next section gives.
// ---------------------------------------------------------------------------
#[cfg(target_os = "macos")]
unsafe extern "C" {
    fn pthread_override_qos_class_start_np(
        thread: libc::pthread_t,
        class: libc::qos_class_t,
        relative_priority: libc::c_int,
    ) -> *mut std::ffi::c_void;
    fn pthread_override_qos_class_end_np(over: *mut std::ffi::c_void) -> libc::c_int;
}

/// A worker raised to [`Role::Responsive`] until this drops — carried with
/// the job it is waited on for, and dropped by the worker once that job is
/// done. Inert where the platform has no override, or declined one.
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

// ---------------------------------------------------------------------------
// READING A CLASS BACK.
//
// Until this, nothing in the tree ever observed the class a thread ACTUALLY
// runs at. `set_self` returns nothing worth branching on, and every QoS test
// below asserts either this enum's ORDER or the SOURCE TEXT of a spawn site —
// both of them claims about code. The class is a property of a RUNNING THREAD,
// and it is the only thing the scheduler reads.
//
// None of these symbols is in `libc`: that crate is GENERATED from a pinned
// reference and asserted item-for-item by its oracle ("Regenerate rather than
// edit"), so declaring them there would be a hand edit of generated code. All
// are ordinary libSystem exports (`<sys/qos.h>`, `<pthread/qos.h>`,
// `<pthread.h>`), declared here.
//
// Declared returning `c_uint`, NOT `libc::qos_class_t`: a `repr(u32)` enum
// return carrying a value outside its six discriminants would be undefined
// behaviour, so a class read back is compared as a number.
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

/// The class and relative priority the CALLING thread has REQUESTED — its own
/// word, which no override another thread pushes onto it changes — or `None` if
/// the platform declined to answer.
#[cfg(target_os = "macos")]
fn requested_class_of_self() -> Option<(libc::c_uint, libc::c_int)> {
    let mut class: libc::c_uint = 0;
    let mut relative: libc::c_int = 0;
    // SAFETY: both out-parameters are live, correctly typed locals, and
    // `pthread_self` names the calling thread, alive across the call.
    let rc = unsafe { pthread_get_qos_class_np(pthread_self().addr(), &mut class, &mut relative) };
    (rc == 0).then_some((class, relative))
}

/// The class the CALLING thread is actually running at.
#[cfg(all(test, target_os = "macos"))]
pub(crate) fn class_of_self() -> u32 {
    // SAFETY: reads the calling thread's own class. Takes no pointers and
    // mutates no shared state.
    unsafe { qos_class_self() }
}

/// The class ANOTHER thread is actually running at, named by its pthread handle
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

/// The class a worker that declares no role runs at — what every silent `spawn`
/// in this crate gets, and what a deleted `set_self` line leaves behind.
/// MEASURED, not assumed: see `tests::a_role_reaches_only_the_thread_that_declares_it`.
#[cfg(all(test, target_os = "macos"))]
pub(crate) const UNDECLARED_CLASS: u32 = libc::qos_class_t::QOS_CLASS_DEFAULT as u32;

#[cfg(test)]
mod tests {
    use super::*;

    // -----------------------------------------------------------------------
    // WHAT THE SCHEDULER ACTUALLY SEES
    // -----------------------------------------------------------------------

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

    /// THE CLASS, READ OFF A RUNNING THREAD. Every other QoS assertion in this
    /// crate is about text — this enum's order, or a `set_self` appearing in a
    /// spawn site's source. None of them can tell whether a worker HAS the class
    /// its role names. This one declares each role on a real thread and asks the
    /// kernel what that thread is running at.
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

    /// A ROLE REACHES ONLY THE THREAD THAT DECLARES IT — the measurement the
    /// rest of this file rests on, and until now only a comment in it. It is why
    /// a declaration must be the first statement INSIDE a worker's closure, why
    /// `every_control_plane_thread_declares_its_role_inside_its_closure` insists
    /// the `set_self` sits after the `.spawn(`, and why a `Background` worker's
    /// CHILD PROCESS needs [`command`]'s clamp: neither a thread nor a process
    /// inherits the class of whatever created it.
    #[cfg(target_os = "macos")]
    #[test]
    fn a_role_reaches_only_the_thread_that_declares_it() {
        let inherited = std::thread::spawn(|| {
            set_self(Role::Interactive);
            std::thread::spawn(class_of_self)
                .join()
                .expect("the spawned thread")
        })
        .join()
        .expect("the declaring thread");
        assert_eq!(
            inherited, UNDECLARED_CLASS,
            "a spawned thread inherited its creator's class (0x{inherited:x}); every \
             `set_self` this crate places inside a closure on the opposite \
             assumption would need re-deriving"
        );
    }

    /// The class a fork from the CALLING thread ran at, as the forked child
    /// reads it in its last step before the exec: its copy of this thread's QoS
    /// word, which `fork(2)` took when it copied the address space — the copy
    /// the forking thread's class paces ([`fork_at_default`]). Not the child's
    /// base priority: the kernel starts a forked thread with no class whichever
    /// class forked it, so that reads the same with or without the guard.
    #[cfg(target_os = "macos")]
    fn class_a_fork_ran_at() -> u32 {
        use std::io::Read as _;
        use std::os::fd::AsRawFd as _;
        use std::os::unix::process::CommandExt as _;

        let (mut probe, probe_end) = std::io::pipe().unwrap();
        // std's Command on purpose: the probe IS a `pre_exec` in a forked child.
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

    /// A FORK FROM A DEMOTED THREAD RUNS AT THE DEFAULT CLASS, and the thread
    /// has its own class back once `spawn` returns. Until its exec the forked
    /// child holds every descriptor aterm has open, and that window is paid at
    /// the forking thread's class: from a UTILITY thread p50 26 ms / max 431 ms
    /// at load ~130, against 7 ms / 32 ms from a DEFAULT one
    /// ([`fork_at_default`]). Read off the fork itself, not timed. A thread at
    /// or above the default forks at its own class — the guard never lowers —
    /// which is also what shows the probe reads the forking thread's class
    /// rather than a constant. Negative control: without the guard a
    /// `Background` thread forks at UTILITY (0x11).
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

    /// The ORDER is the contract: cosmetic work must never outrank the work a
    /// human is waiting on. Pinned as a test because the roles are assigned at
    /// ~20 scattered spawn sites, and a wrong one is invisible at the call site
    /// — it shows up only as latency on a loaded machine.
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

    /// The inherited class a silent `spawn` leaves a thread at sits BETWEEN the
    /// two roles the floor rule chooses between: demoting a lock holder to
    /// `Background` really is a demotion below what it had, and `Responsive`
    /// really is a promotion. If Apple ever renumbers the classes this is the
    /// assertion that notices.
    #[cfg(target_os = "macos")]
    #[test]
    fn the_undeclared_default_sits_between_responsive_and_background() {
        let default = libc::qos_class_t::QOS_CLASS_DEFAULT as u32;
        assert!(qos_class(Role::Responsive) as u32 > default);
        assert!(default > qos_class(Role::Background) as u32);
    }

    /// The Linux slice map keeps the enum's order: the keystroke path asks for
    /// the shortest slice, the PTY drain a longer one, and nothing below the
    /// UI thread asks at all (a Linux nice would be inherited by children).
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

    /// The slice request changes the slice and NOTHING ELSE: a `nice`d aterm
    /// keeps its nice (writing 0 back would be an unprivileged nice decrease —
    /// EPERM, and the slice would never land), a `SCHED_BATCH` thread stays
    /// batch, and a real-time thread is not touched at all. Negative control:
    /// the struct the first cut wrote (policy 0, nice 0) differs from it.
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

    /// The same, against the real kernel: a thread that was `nice`d to +5 (an
    /// unprivileged INCREASE, so this needs no rights) still gets its slice
    /// request accepted, and keeps nice +5. The first cut wrote nice 0 back,
    /// which the kernel refuses unprivileged, so on a niced aterm the whole
    /// call failed and the slice never landed.
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

    /// THE SLICE, READ OFF A RUNNING THREAD, and the reset-on-fork that keeps it
    /// from leaking into a thread (or shell) the declaring thread creates. On a
    /// kernel older than 6.12 the request is accepted and ignored for a fair
    /// policy; that is reported, not failed, because the hint degrading to a
    /// no-op is the designed behaviour there.
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

    /// The live contract (2026-09-15): a `Background` child really starts below the
    /// inherited band (pri 31). Before [`command`], every such child did not.
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

    /// Every thread of the package lane DECLARES its role. A role does not reach
    /// a thread any more than it reaches a child: measured 2026-09-15, a thread
    /// created from a `QOS_CLASS_UTILITY` thread reads `QOS_CLASS_DEFAULT`, so a
    /// lane whose entry thread declares `Background` still hands every thread it
    /// spawns back to the band the program being typed into runs in. Undeclared
    /// costs nothing functional — it shows up only as echo lag while a six-hourly
    /// pass parses its child's stdout and tails its progress file.
    #[test]
    fn every_package_lane_thread_declares_its_role() {
        let lib = include_str!("lib.rs");
        let native = include_str!("app_native.rs");
        for (file, source, thread) in [
            ("lib.rs", lib, "atpkg-update"),
            ("lib.rs", lib, "atpkg-machine"),
            ("lib.rs", lib, "atpkg-progress-tail"),
            // The Settings ▸ Packages verb worker: same lane, same six-hourly
            // store, reached by a click instead of by the park. Its CHILD already
            // takes the clamp; the thread that waits on it and parses its output
            // was the half still in the typing band.
            ("app_native.rs", native, "aterm-packages-verb"),
        ] {
            let needle = format!(".name(\"{thread}\".into())");
            let at = source
                .find(&needle)
                .unwrap_or_else(|| panic!("no thread named {thread} in {file}"));
            let head = &source[at..source.len().min(at + 600)];
            assert!(
                head.contains("qos::set_self(crate::qos::Role::Background)"),
                "the {thread} thread declares no role at the top of its closure, \
                 so it runs at the inherited DEFAULT band"
            );
        }
    }

    /// The pooled scrollback rewrap declares [`Role::Responsive`] — the class this
    /// module's own doc names for reflow, and the FLOOR the port-time rule sets for
    /// a worker holding a lock the UI thread contends (`REFLOW_POOL`, which the
    /// main thread takes on every settle to submit). Undeclared it sat at the
    /// inherited DEFAULT: below its intended class, and low enough that a
    /// descheduled worker can hold the pool lock with the resizing UI thread queued
    /// behind it.
    ///
    /// Anchored on the FUNCTION, not on a `.name(…)` closure: the pool spawns
    /// `.spawn(reflow_worker_main)` by name, so the declaration belongs at the
    /// worker's entry rather than at either of the two spawn sites.
    #[test]
    fn the_reflow_pool_worker_declares_its_role_at_its_entry() {
        let source = include_str!("app_render.rs");
        let at = source
            .find("fn reflow_worker_main() {")
            .expect("no reflow_worker_main in app_render.rs");
        let head = &source[at..source.len().min(at + 600)];
        assert!(
            head.contains("qos::set_self(crate::qos::Role::Responsive)"),
            "the aterm-reflow worker declares no role at its entry, so it runs at \
             the inherited DEFAULT band — below the class reflow is meant to have"
        );
    }

    /// EVERY CONTROL-PLANE THREAD THAT SERVES A VERB declares [`Role::Responsive`],
    /// the floor this module's port-time rule sets for a worker holding a lock the
    /// UI thread contends. These lanes hold the TERMINAL MUTEX itself: a `text`
    /// verb formats every visible row under one `term_lock`, and a subscription's
    /// push loop takes it per target per tick. Undeclared, all thirteen of them ran
    /// at the inherited DEFAULT band — below the UI thread, which takes that same
    /// mutex on the key path (`term_lock_ui`) and in the redraw — so an agent
    /// driving a session with `ctl text`/`screen` could leave a descheduled holder
    /// in front of the next keystroke.
    ///
    /// What is asserted is that the declaration sits INSIDE the spawned closure. A
    /// role does not reach a thread from the one that creates it (measured
    /// 2026-09-15, the same finding that put every other entry in this file), so a
    /// `set_self` on the spawning side would read as a fix and change nothing.
    #[test]
    fn every_control_plane_thread_declares_its_role_inside_its_closure() {
        let control = include_str!("control.rs");
        let lanes = include_str!("control_lanes.rs");
        let listener = include_str!("control_listener.rs");
        for (thread, source, anchor) in [
            (
                "aterm-control-N (8 request lanes)",
                lanes,
                "pub(super) fn start(self: &Arc<Self>)",
            ),
            (
                "aterm-control-parker",
                lanes,
                "fn start_parker(self: &Arc<Self>)",
            ),
            (
                "aterm-subscribe-N (4 push lanes)",
                control,
                "fn spawn_subscription_workers(",
            ),
            ("aterm-fabric-bridge", control, "fn attach_fabric_bridge("),
            (
                "aterm-control-listener",
                control,
                ".name(\"aterm-control-listener\".into())",
            ),
            // The supervised accept thread admits every peer (the listener
            // thread above is its watchdog), and the startup signature hash is
            // what a lane's `version` answer may wait on.
            (
                "aterm-control-accept-N",
                listener,
                ".name(format!(\"aterm-control-accept-{generation}\"))",
            ),
            (
                "aterm-build-signature",
                control,
                ".name(\"aterm-build-signature\".into())",
            ),
        ] {
            let at = source
                .find(anchor)
                .unwrap_or_else(|| panic!("no `{anchor}` in its source"));
            let head = &source[at..source.len().min(at + 2000)];
            let spawn = head
                .find(".spawn(")
                .unwrap_or_else(|| panic!("no spawn under `{anchor}`"));
            let declared = head
                .find("qos::set_self(crate::qos::Role::Responsive)")
                .unwrap_or_else(|| {
                    panic!(
                        "the {thread} thread declares no role, so it serves control \
                         verbs at the inherited DEFAULT band while holding the \
                         terminal mutex the UI thread takes on the key path"
                    )
                });
            assert!(
                declared > spawn,
                "the {thread} thread's role is declared on the SPAWNING thread; a \
                 role does not reach the thread that thread creates"
            );
        }
        // The wait lanes are spawned by name (`.spawn(move || lanes.wait_lane())`,
        // on demand), so their declaration belongs at the lane's entry, as the
        // reflow worker's does.
        let at = lanes
            .find("fn wait_lane(self: &Arc<Self>) {")
            .expect("no wait_lane in control_lanes.rs");
        let head = &lanes[at..lanes.len().min(at + 200)];
        assert!(
            head.contains("qos::set_self(crate::qos::Role::Responsive)"),
            "the aterm-control-wait-N lanes declare no role at their entry"
        );
    }

    /// Every `atpkg` child the GUI launches takes the clamp. A bare
    /// `Command::new(atpkg)` compiles, runs and passes every functional test; it
    /// shows up only as echo lag while an update pass verifies a snapshot.
    #[test]
    fn no_atpkg_child_is_spawned_at_the_inherited_band() {
        for (file, source) in [
            ("lib.rs", include_str!("lib.rs")),
            ("app_native.rs", include_str!("app_native.rs")),
        ] {
            for bare in ["Command::new(atpkg)", "Command::new(&atpkg)"] {
                assert!(
                    !source.contains(bare),
                    "{file} spawns `{bare}` at the inherited band; use \
                     `qos::command(Role::Background, …)`"
                );
            }
        }
    }
}
