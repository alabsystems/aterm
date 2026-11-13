// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

use super::*;

/// [`sleep_until`] takes a long wait as one timer that ends a
/// [`COALESCE_MARGIN`] early and the rest in [`PRECISE_SLICE`]s, and returns
/// once the deadline has passed, never before.
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
/// (an override is invisible to the class a thread reads of itself). That
/// record's base priority also carries a push from any thread waiting on a
/// lock the worker holds (a turnstile's inheritance), so the worker is read
/// only once it is blocked in a read(2) after declaring its role, where it
/// holds no lock and runs nothing until it is released. A worker still
/// running (sending on a channel, allocating, parking) can hold such a lock
/// while one of this binary's default-class threads waits on it, and then
/// reads 31, as a standing override does.
#[cfg(target_os = "macos")]
#[test]
fn a_boost_raises_the_worker_while_it_stands_and_no_longer() {
    use std::io::{Read as _, Write as _};
    use std::os::unix::thread::JoinHandleExt as _;
    unsafe extern "C" {
        fn pthread_threadid_np(thread: libc::pthread_t, id: *mut u64) -> libc::c_int;
    }
    /// `<sys/proc_info.h>`'s by-thread-id flavor of `PROC_PIDTHREADINFO`.
    const PROC_PIDTHREADID64INFO: libc::c_int = 15;
    /// `<mach/thread_info.h>`'s `TH_STATE_WAITING`: blocked in the kernel.
    const TH_STATE_WAITING: i32 = 3;
    /// `struct proc_threadinfo` (`<sys/proc_info.h>`, not in the generated
    /// `libc`): 112 bytes, the run state `pth_run_state` at offset 24 and the
    /// base priority `pth_priority` at offset 40.
    #[repr(C)]
    struct ProcThreadInfo {
        _user_and_system_time: [u64; 2],
        _usage_policy: [i32; 2],
        run_state: i32,
        _flags_sleep_curpri: [i32; 3],
        priority: i32,
        _max_priority: i32,
        _name: [u8; 64],
    }
    const _: () = assert!(size_of::<ProcThreadInfo>() == 112);
    let record_of = |thread: libc::pthread_t| {
        let mut id = 0u64;
        // SAFETY: `thread` is a live, unjoined thread; `id` a writable slot.
        assert_eq!(unsafe { pthread_threadid_np(thread, &mut id) }, 0);
        let mut info = std::mem::MaybeUninit::<ProcThreadInfo>::uninit();
        let size = size_of::<ProcThreadInfo>() as libc::c_int;
        // SAFETY: `info` is `size` writable bytes laid out as the record this flavor fills (its size pinned above).
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
        unsafe { info.assume_init() }
    };
    let (mut release, mut held) = std::os::unix::net::UnixStream::pair().expect("a socket pair");
    let worker = std::thread::spawn(move || {
        set_self(Role::Background);
        let _ = held.read(&mut [0u8; 1]);
    });
    let thread = worker.as_pthread_t() as libc::pthread_t;
    aterm_spec::wait::until_within(
        aterm_spec::wait::MUST_HAPPEN,
        "the worker blocked in its read at its declared role",
        || {
            (class_of(&worker) == Some(class_for(Role::Background))
                && record_of(thread).run_state == TH_STATE_WAITING)
                .then_some(())
        },
    );
    let parked = record_of(thread).priority;
    let boost = Boost::start(Some(&worker));
    assert!(boost.armed(), "an unjoined thread takes an override");
    let raised = record_of(thread).priority;
    assert!(raised > parked, "boosted {raised}, parked {parked}");
    drop(boost);
    assert_eq!(
        record_of(thread).priority,
        parked,
        "the override ended with it"
    );
    release.write_all(b"x").expect("the worker's release");
    worker.join().expect("the worker");
}

/// The class a fork from the calling thread ran at, as the forked child reads it
/// before its exec: its copy of this thread's QoS word, taken when `fork(2)` copied
/// the address space ([`fork_at_default`]).
#[cfg(target_os = "macos")]
fn class_a_fork_ran_at() -> u32 {
    use std::io::Read as _;
    use std::os::fd::AsRawFd as _;
    use std::os::unix::process::CommandExt as _;

    let (mut probe, probe_end) = std::io::pipe().unwrap();
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
    let mut class = [0; 4];
    probe
        .read_exact(&mut class)
        .expect("the pre_exec probe wrote nothing before the exec");
    u32::from_ne_bytes(class)
}

/// A fork from a demoted thread runs at the default class, and the thread has its
/// own class back once `spawn` returns ([`fork_at_default`]).
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
/// waiting on.
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
    for slice in [interactive, responsive] {
        assert!((100_000..=100_000_000).contains(&slice), "{slice} ns");
    }
    assert_eq!(linux_slice_ns(Role::Background), None);
    assert_eq!(linux_slice_ns(Role::Housekeeping), None);
}

/// The slice request changes the slice and nothing else: a `nice`d aterm keeps its
/// nice (writing 0 back would be an unprivileged nice decrease, `EPERM`), a
/// `SCHED_BATCH` thread stays batch, and a real-time thread is not touched.
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
/// leaking into a thread (or shell) the declaring thread creates.
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
        aterm_spec::host::could_not_run(format!(
            "kernel ignored the slice request (runtime {} ns): pre-6.12 EEVDF",
            declared.0
        ));
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
    let tmp = aterm_tempfile::Builder::new()
        .prefix("aterm-qos-launcher-")
        .tempdir()
        .unwrap();
    let dir = tmp.path();
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
}

/// In a process held as background work (`aterm node serve`,
/// [`hold_process_as_background_work`]), each thread it starts from then on starts at
/// utility, a thread that declares any role stays at or below utility, and a child started on
/// any role's behalf starts behind a clamp at or below utility, as the `Standard` launchd job
/// the node job ran in clamped every thread and child; and a lift
/// ([`aterm_uds::qos::lift`]) still raises the thread that asks for it to the default class,
/// which that clamp held at utility. The hold is the process's own, so the body runs alone in
/// a child of this test binary. Each class is read from outside the thread that has it, by a
/// thread at the default class.
#[cfg(target_os = "macos")]
#[test]
fn a_process_held_as_background_work_holds_each_thread_at_utility_and_a_lift_still_lifts() {
    holds_each_thread_at(Role::Background, hold_process_as_background_work);
}

/// The same of a process held as housekeeping (the vault subscriber's converge runner,
/// [`hold_process_as_housekeeping`]): each thread at the background class, every role and
/// role child at or below it, and a lift still to the default class.
#[cfg(target_os = "macos")]
#[test]
fn a_process_held_as_housekeeping_holds_each_thread_at_background_and_a_lift_still_lifts() {
    holds_each_thread_at(Role::Housekeeping, hold_process_as_housekeeping);
}

/// The body of the hold tests: alone in a child of this test binary, `hold` holds the process
/// at `at`'s class.
#[cfg(target_os = "macos")]
fn holds_each_thread_at(at: Role, hold: fn()) {
    use std::sync::mpsc::{Receiver, Sender, channel};

    /// A thread started now that runs each step it is told, on itself, saying when it has.
    struct Stepped {
        tell: Sender<Box<dyn FnOnce() + Send>>,
        hear: Receiver<()>,
        thread: std::thread::JoinHandle<()>,
    }
    impl Stepped {
        /// Started, once it says so.
        fn start() -> Self {
            let (tell, told) = channel::<Box<dyn FnOnce() + Send>>();
            let (said, hear) = channel();
            let thread = std::thread::spawn(move || {
                let _ = said.send(());
                while let Ok(step) = told.recv() {
                    step();
                    let _ = said.send(());
                }
            });
            hear.recv_timeout(aterm_spec::wait::MUST_HAPPEN)
                .expect("the thread started");
            Self { tell, hear, thread }
        }
        /// `step` run on the thread, once it says so.
        fn step(&self, step: impl FnOnce() + Send + 'static) {
            self.tell.send(Box::new(step)).expect("the thread waits");
            self.hear
                .recv_timeout(aterm_spec::wait::MUST_HAPPEN)
                .expect("the thread ran its step");
        }
        /// The thread's class, read from this one.
        fn class(&self) -> u32 {
            class_of(&self.thread).expect("its class reads")
        }
    }
    thread_local! {
        /// What a held thread's lift raised it from, for its put back.
        static LIFTED: std::cell::Cell<Option<aterm_uds::qos::Lifted>> =
            const { std::cell::Cell::new(None) };
    }
    if !crate::test_alone::alone() {
        return;
    }
    let held_class = class_for(at);

    let unheld = Stepped::start().class();
    assert_eq!(
        unheld, UNDECLARED_CLASS,
        "an unheld process's new thread read 0x{unheld:x}"
    );
    let declarers = [
        Role::Interactive,
        Role::Responsive,
        Role::Background,
        Role::Housekeeping,
    ]
    .map(|role| (role, Stepped::start()));

    std::thread::spawn(hold).join().expect("the hold");
    assert_eq!(held(), Some(at));

    let thread = Stepped::start();
    let started = thread.class();
    assert_eq!(
        started, held_class,
        "a held process's new thread read 0x{started:x}"
    );
    thread.step(|| LIFTED.set(aterm_uds::qos::lift()));
    let lifted = thread.class();
    assert_eq!(
        lifted, UNDECLARED_CLASS,
        "a lift in a held process left its thread at 0x{lifted:x}"
    );
    thread.step(|| {
        if let Some(lifted) = LIFTED.take() {
            aterm_uds::qos::restore(lifted);
        }
    });
    assert_eq!(thread.class(), held_class, "the lifted thread put back");

    for (role, declarer) in declarers {
        declarer.step(move || set_self(role));
        let declared = declarer.class();
        assert_eq!(
            declared,
            class_for(capped(role, Some(at))),
            "a {role:?} thread of a held process declared 0x{declared:x}"
        );
        assert!(declared <= held_class, "{role:?}: 0x{declared:x}");
        let clamp = if role == Role::Housekeeping || at == Role::Housekeeping {
            "background"
        } else {
            "utility"
        };
        assert_eq!(
            argv(&command(role, "/bin/sh")),
            [TASKPOLICY, "-c", clamp, "/bin/sh"],
            "a child of a held process's {role:?} work"
        );
    }
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
