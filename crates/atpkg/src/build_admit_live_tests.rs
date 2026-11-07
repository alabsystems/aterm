// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Actual isolated terminal jobs; no guessed signal target or latency budget.

use super::*;
use aterm_capacity::cohort::launch::CohortSignal;
use std::{
    cell::{Cell, RefCell},
    sync::{Arc, OnceLock},
    time::{Duration, Instant},
};

const DEAF: &str = "ATPKG_LIVE_DEAF_WORKER";

#[test]
fn native_deaf_worker_if_requested() {
    if std::env::var_os(DEAF).is_none() {
        return;
    }
    let world = PathBuf::from(std::env::var_os(WORLD).unwrap());
    // SAFETY: only this private stand-in, before its actual readiness proof.
    assert_ne!(
        unsafe { libc::signal(libc::SIGTERM, libc::SIG_IGN) },
        libc::SIG_ERR
    );
    // Readiness is the name appearing, so its reader must find the whole pid behind it.
    std::fs::write(world.join("ready.partial"), std::process::id().to_string()).unwrap();
    std::fs::rename(world.join("ready.partial"), world.join("ready")).unwrap();
    let _ = std::io::stdin().read_line(&mut String::new()).unwrap();
}

pub(super) fn actor(world: &Path, mode: &str) {
    use std::os::unix::process::ExitStatusExt as _;
    if mode == "live-bad-policy" {
        let mut previous = std::mem::MaybeUninit::<libc::sigaction>::zeroed();
        // SAFETY: only this private actor's action, with no child of its own running.
        assert_eq!(
            unsafe { libc::sigaction(libc::SIGCHLD, std::ptr::null(), previous.as_mut_ptr()) },
            0
        );
        // SAFETY: initialized by the successful query.
        let previous = unsafe { previous.assume_init() };
        let mut incompatible = previous;
        incompatible.sa_flags |= libc::SA_NOCLDWAIT;
        // SAFETY: private actor, exact original action restored before any return.
        assert_eq!(
            unsafe { libc::sigaction(libc::SIGCHLD, &incompatible, std::ptr::null_mut()) },
            0
        );
        let refused = Cancellation::channel(Duration::ZERO);
        // SAFETY: restore this actor's complete captured action.
        assert_eq!(
            unsafe { libc::sigaction(libc::SIGCHLD, &previous, std::ptr::null_mut()) },
            0
        );
        std::fs::write(world.join("ready"), b"refused before child launch").unwrap();
        assert_eq!(refused.unwrap_err().kind(), std::io::ErrorKind::Unsupported);
        return;
    }
    let normal = mode == "live-normal";
    let error = mode == "live-error";
    let cleanup = mode == "live-cleanup";
    let timed = mode == "live-grace-expiry";
    let pinned = mode == "live-pin" || cleanup;
    let private = pinned || mode == "live-timer";
    let machine = machine();
    let selected = cohort_seat(world, &machine);
    let ledger = selected.ledger.as_ref().unwrap();
    let Admission::Claimed(held) = admit_cohort(&test_build(), world, &selected, &mut |_| {})
    else {
        panic!("live admission refused")
    };
    let expected = held.claim().record().need.mem;
    let original_mask = signal_mask();
    let original_group = foreground();
    let grace = if pinned {
        Duration::from_secs(600)
    } else if timed {
        Duration::from_millis(100)
    } else {
        Duration::ZERO
    };
    let (cancel, live) = Cancellation::channel(grace).unwrap();
    let issued = Arc::new(OnceLock::new());
    let requested = if normal {
        None
    } else {
        let cancel = cancel.clone();
        let world = world.to_owned();
        let issued = Arc::clone(&issued);
        Some(std::thread::spawn(move || {
            aterm_spec::wait::until_within(
                aterm_spec::wait::MUST_HAPPEN,
                "external cancellation intent",
                || world.join("cancel").exists().then_some(()),
            );
            issued.set(Instant::now()).unwrap();
            cancel.request(CancelRequest::Terminate).unwrap();
        }))
    };
    let member: RefCell<Option<(OwnedChild, std::process::ChildStdin)>> = RefCell::new(None);
    let root_input = RefCell::new(None);
    let requests = RefCell::new(Vec::new());
    let observed = Cell::new(false);
    let reaped = Cell::new(false);
    let drained = Cell::new(false);
    let member_reaped = Cell::new(false);
    let mut events = |event| {
        match event {
            SupervisedEvent::Stopped(signal) => {
                panic!("running cancellation unexpectedly stopped {signal}")
            }
            SupervisedEvent::ResumeAccepted { .. } => panic!("no interactive resume requested"),
            SupervisedEvent::GroupRequestAccepted(signal) => {
                requests.borrow_mut().push(signal);
                if timed && signal == CohortSignal::Kill {
                    // This worker stays running and ignores TERM. No later
                    // request or child transition wakes the supervisor: its
                    // future deadline must expire. This is grace semantics,
                    // not an upper latency budget; MUST_HAPPEN detects hangs.
                    assert!(Instant::now() >= *issued.get().unwrap() + grace);
                }
                if error {
                    return Err(std::io::Error::other(
                        "fixture observer rejected its accepted request",
                    ));
                }
                let rows = ledger.list().unwrap();
                assert_eq!(rows.len(), 1);
                assert_eq!(rows[0].need.mem, expected);
                assert_eq!(rows[0].accounting, aterm_capacity::Accounting::Whole);
                if signal == CohortSignal::Kill && private {
                    assert!(!reaped.get(), "group authority must precede root reap");
                    let mut member = member.borrow_mut();
                    let (compiler, _) = member.as_mut().unwrap();
                    assert_eq!(compiler.wait().unwrap().signal(), Some(libc::SIGKILL));
                    member_reaped.set(true);
                }
            }
            SupervisedEvent::RootExitObserved => {
                assert!(pinned, "zero grace requires no idle waiting");
                assert!(!observed.replace(true));
                assert!(!reaped.get());
                assert!(
                    !member.borrow_mut().as_mut().unwrap().0.exited().unwrap(),
                    "TERM-ignoring compiler survives root exit"
                );
                assert_eq!(
                    ledger.list().unwrap()[0].accounting,
                    aterm_capacity::Accounting::Whole
                );
                cancel.request(CancelRequest::Kill).unwrap();
            }
            SupervisedEvent::RootExited(usage) => {
                assert!(!reaped.replace(true));
                if normal || cleanup {
                    assert!(usage.status.success());
                } else {
                    assert!(!usage.status.success());
                }
            }
            SupervisedEvent::CohortDrained => {
                assert!(error || reaped.get());
                if private {
                    assert!(member_reaped.get());
                }
                drained.set(true);
            }
            SupervisedEvent::CohortRetained => panic!("all fixture members are owned and reaped"),
        }
        Ok(JobControl::Wait)
    };
    let mut command = if cleanup {
        let mut command = spawnfd::command(std::env::current_exe().unwrap());
        command
            .args([
                "--exact",
                "build_admit::tests::terminal::native_cancel_worker_if_requested",
                "--test-threads=1",
                "--nocapture",
            ])
            .env(CANCEL_WORKER, "running")
            .start_suspended(true)
            .stdout(Stdio::null())
            .stderr(Stdio::null());
        command
    } else if private {
        let mut command = spawnfd::command("/bin/cat");
        command
            .start_suspended(true)
            .stdin(Stdio::piped())
            .stdout(Stdio::piped());
        command
    } else {
        let mut command = spawnfd::command(std::env::current_exe().unwrap());
        command
            .args([
                "--exact",
                if normal {
                    "build_admit::tests::terminal::native_job_worker_if_requested"
                } else {
                    "build_admit::tests::terminal::live_tests::native_deaf_worker_if_requested"
                },
                "--test-threads=1",
                "--nocapture",
            ])
            .env(if normal { JOB_WORKER } else { DEAF }, "1")
            .stdout(Stdio::null())
            .stderr(Stdio::null());
        command
    };
    let status = if private {
        supervise_launch(
            Some(*held),
            &mut command,
            &mut |_| {},
            front::Front::arm_cohort,
            |owned, _| {
                if cleanup {
                    aterm_spec::wait::until_within(
                        aterm_spec::wait::MUST_HAPPEN,
                        "running cleanup worker ready",
                        || world.join("ready").exists().then_some(()),
                    );
                } else {
                    *root_input.borrow_mut() =
                        Some(aterm_uds::deadline::cat_echoes(owned.child_mut()));
                }
                let SupervisedChild::Bound(bound) = owned else {
                    panic!("missing actual bound receipt")
                };
                let mut compiler = spawnfd::command("/bin/sh");
                compiler
                    .args(["-c", "trap '' TERM; exec /bin/cat"])
                    .stdin(Stdio::piped())
                    .stdout(Stdio::piped());
                // The actual bound leader is borrowed/pinned here throughout
                // the member birth; no decoded or historical group is used.
                let mut compiler = OwnedChild(
                    compiler
                        .spawn_in_process_group(bound.cohort().group())
                        .unwrap(),
                );
                let input = aterm_uds::deadline::cat_echoes(&mut compiler);
                *member.borrow_mut() = Some((compiler, input));
                std::fs::write(world.join("ready"), b"actual root and compiler ready").unwrap();
            },
            Some(JobRun {
                terminal: Some(std::fs::File::open("/dev/tty").unwrap()),
                events: Some(&mut events),
                live: Some(live),
                cancelled: false,
            }),
        )
    } else {
        supervise_terminal_controlled(
            *held,
            &mut command,
            std::fs::File::open("/dev/tty").unwrap(),
            &mut |_| {},
            live,
            &mut events,
        )
    };
    if let Some(requested) = requested {
        requested.join().unwrap();
    }
    if error {
        assert_eq!(status.unwrap_err().kind(), std::io::ErrorKind::Other);
        let pid: u32 = std::fs::read_to_string(world.join("ready"))
            .unwrap()
            .parse()
            .unwrap();
        let mut info = std::mem::MaybeUninit::<libc::siginfo_t>::zeroed();
        // SAFETY: observe only this fixture's actual root after its supervisor
        // returned. WNOWAIT consumes nothing; ECHILD proves it left no zombie.
        assert_eq!(
            unsafe {
                libc::waitid(
                    libc::P_PID,
                    pid,
                    info.as_mut_ptr(),
                    libc::WEXITED | libc::WNOWAIT | libc::WNOHANG,
                )
            },
            -1
        );
        assert_eq!(
            std::io::Error::last_os_error().raw_os_error(),
            Some(libc::ECHILD)
        );
        assert!(drained.get());
        assert!(ledger.list().unwrap().is_empty());
        assert_eq!(foreground(), original_group);
        assert_eq!(signal_mask(), original_mask);
        assert!(!selected.own.as_ref().unwrap().exists());
        return;
    }
    let status = status.unwrap();
    assert_eq!(status.root_status().success(), normal || cleanup);
    assert_eq!(status.succeeded(), normal);
    assert_eq!(status.cancelled(), !normal);
    assert_eq!(status.cohort_settlement(), Some(CohortSettlement::Drained));
    assert_eq!(
        *requests.borrow(),
        if normal {
            Vec::new()
        } else {
            vec![
                CohortSignal::Terminate,
                CohortSignal::Continue,
                CohortSignal::Kill,
            ]
        }
    );
    assert_eq!(observed.get(), pinned);
    assert!(drained.get());
    assert_eq!(foreground(), original_group);
    assert_eq!(signal_mask(), original_mask);
    assert!(ledger.list().unwrap().is_empty());
    assert_eq!(
        selected.own.as_ref().unwrap().exists(),
        normal,
        "only a completed uncancelled build trains its memory sample"
    );
}
