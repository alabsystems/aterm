// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Independent native jobs in one process, with no foreground front or real ledger.

use super::*;
use aterm_capacity::cohort::launch::CohortSignal;
use aterm_uds::spawnfd::{self, Stdio};
use std::{io::Write as _, time::Duration};

const ACTOR: &str = "ATPKG_HEADLESS_HOST_WORLD";
pub(super) const WORKER: &str = "ATPKG_HEADLESS_WORKER_WORLD";
pub(super) const STOP: &str = "ATPKG_HEADLESS_STOP_WORKER";

struct HeadlessJob {
    worker: Option<std::thread::JoinHandle<(std::io::Result<JobOutcome>, Vec<CohortSignal>)>>,
    input: std::io::PipeWriter,
    cancel: CancelHandle,
    world: PathBuf,
    started: std::sync::mpsc::Receiver<u32>,
}

impl HeadlessJob {
    fn is_finished(&self) -> bool {
        self.worker
            .as_ref()
            .is_none_or(|worker| worker.is_finished())
    }

    fn join(&mut self) -> std::thread::Result<(std::io::Result<JobOutcome>, Vec<CohortSignal>)> {
        self.worker.take().expect("one owning join").join()
    }

    fn keep_cleanup(&self, why: &str) {
        let _ = std::fs::write(self.world.join("owner-cleanup"), why);
        let _ = writeln!(std::io::stderr(), "KEPT {}: {why}", self.world.display());
    }
}

impl Drop for HeadlessJob {
    fn drop(&mut self) {
        if self.worker.is_none() {
            return;
        }
        let unwinding = std::thread::panicking();
        self.keep_cleanup("the headless job owner was dropped before its join");
        // Release any fixture barriers before awaiting either owner's drain.
        if let Some(world) = self.world.parent() {
            for index in 0..2 {
                let _ = std::fs::write(
                    world.join(index.to_string()).join("release"),
                    b"fixture cleanup",
                );
            }
        }
        if !self.is_finished() {
            let _ = self.cancel.request(CancelRequest::Terminate);
        }
        let finished = aterm_spec::wait::within(
            aterm_spec::wait::MUST_HAPPEN,
            Duration::from_millis(10),
            || self.is_finished().then_some(()),
        );
        if finished.is_none() {
            self.keep_cleanup("the owned headless supervisor did not finish within its cleanup hang detector; custody remains retained");
        } else if let Some(worker) = self.worker.take()
            && !matches!(worker.join(), Ok((Ok(outcome), _)) if outcome.cohort_settlement() == Some(CohortSettlement::Drained))
        {
            self.keep_cleanup("the owned headless supervisor did not report a drained cohort");
        }
        if !unwinding {
            panic!("the headless job owner was dropped before its owning join");
        }
    }
}

#[test]
fn headless_worker_if_requested() {
    let Some(world) = std::env::var_os(WORKER) else {
        return;
    };
    // SAFETY: this isolated stand-in changes only its own policy, before readiness.
    assert_ne!(
        unsafe { libc::signal(libc::SIGTERM, libc::SIG_IGN) },
        libc::SIG_ERR
    );
    // Readiness is the name appearing, so its reader must find the whole pid behind it.
    let world = PathBuf::from(world);
    std::fs::write(world.join("ready.partial"), std::process::id().to_string()).unwrap();
    std::fs::rename(world.join("ready.partial"), world.join("ready")).unwrap();
    if std::env::var_os(STOP).is_some() {
        // SAFETY: only this private worker stops itself after actual readiness.
        assert_eq!(unsafe { libc::kill(libc::getpid(), libc::SIGSTOP) }, 0);
    }
    std::io::stdin().read_line(&mut String::new()).unwrap();
}

fn claim_job(world: &Path, seat: &Seat<'_>) -> Held {
    let build = classify(&words(&["--unverified", "build"]), Tool::Targo).unwrap();
    match admit_cohort_job(&build, world, seat, &mut |_| {}) {
        Admission::Claimed(held) => *held,
        other => panic!("independent job refused: {other:?}"),
    }
}

fn wait_ready(world: &Path, root: u32, finished: impl Fn() -> bool) {
    aterm_spec::wait::until_while_working(root, "actual headless worker ready", || {
        if world.join("ready").exists() {
            return Some(());
        }
        assert!(
            !finished(),
            "worker ended before readiness: {}",
            std::fs::read_to_string(world.join("compiler.log")).unwrap_or_default()
        );
        None
    });
}

#[test]
fn headless_host_actor_if_requested() {
    use std::os::unix::process::ExitStatusExt as _;
    let Some(world) = std::env::var_os(ACTOR) else {
        return;
    };
    let world = PathBuf::from(world);
    let machine = Fixed::new(concurrent_reading(256, 200, 128));
    let mut selected = cohort_seat(&world, &machine);
    selected.own = None;
    let ledger = selected.ledger.as_ref().unwrap();

    // Even the first job refuses a resource override or a fabricated front pid.
    selected.mode = Admit::Now;
    assert!(matches!(
        admit_cohort_job(&test_build(), &world, &selected, &mut |_| {}),
        Admission::Refused(_)
    ));
    selected.mode = Admit::Wait;
    selected.pid = selected.pid.wrapping_add(94);
    assert!(matches!(
        admit_cohort_job(&test_build(), &world, &selected, &mut |_| {}),
        Admission::Refused(_)
    ));
    selected.pid = std::process::id();

    if std::env::var_os(STOP).is_some() {
        let held = claim_job(&world, &selected);
        let (_cancel, cancellation) = Cancellation::channel(Duration::ZERO).unwrap();
        let mut command = spawnfd::command(std::env::current_exe().unwrap());
        command
            .args([
                "--exact",
                "build_admit::tests::headless::headless_worker_if_requested",
                "--test-threads=1",
            ])
            .env(WORKER, &world)
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null());
        let error = supervise_headless_controlled(
            held,
            &mut command,
            &mut |_| {},
            cancellation,
            &mut |event| {
                Ok(if matches!(event, SupervisedEvent::Stopped(_)) {
                    JobControl::ResumeForeground
                } else {
                    JobControl::Wait
                })
            },
        )
        .unwrap_err();
        assert_eq!(error.kind(), std::io::ErrorKind::PermissionDenied);
        assert!(ledger.list().unwrap().is_empty());
        let pid: u32 = std::fs::read_to_string(world.join("ready"))
            .unwrap()
            .parse()
            .unwrap();
        let mut info = std::mem::MaybeUninit::<libc::siginfo_t>::zeroed();
        // SAFETY: a non-consuming query for this fixture's actual root. ECHILD
        // proves the failed operation reaped it rather than abandoning a zombie.
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
        return;
    }

    for index in 0..2 {
        let job = world.join(index.to_string());
        std::fs::create_dir(&job).unwrap();
    }
    let first = claim_job(&world.join("0"), &selected);
    let second = claim_job(&world.join("1"), &selected);
    let second_need = second.claim().record().need.mem;
    let expected = first.claim().record().need.mem + second.claim().record().need.mem;
    let rows = ledger.list().unwrap();
    assert_eq!(rows.len(), 2, "same owner still reserves two actual jobs");
    assert_eq!(rows.iter().map(|r| r.need.mem).sum::<u64>(), expected);

    // A regression to a per-job Front cannot get through this occupied global turn.
    // The private actor's actual-root publication hang detector catches that deadlock.
    let _turn = crate::platform::FORWARDING_TURN.lock().unwrap();
    let mut jobs = Vec::new();
    for (index, held) in [first, second].into_iter().enumerate() {
        let job_world = world.join(index.to_string());
        let (input, writer) = aterm_uds::pipe().unwrap();
        let (cancel, cancellation) = Cancellation::channel(Duration::from_millis(100)).unwrap();
        let worker_world = job_world.clone();
        let (started, root) = std::sync::mpsc::channel();
        let worker = std::thread::spawn(move || {
            HEADLESS_ROOT_STARTED.with(|slot| *slot.borrow_mut() = Some(started));
            let mut command = spawnfd::command(std::env::current_exe().unwrap());
            command
                .args([
                    "--exact",
                    "build_admit::tests::headless::headless_worker_if_requested",
                    "--test-threads=1",
                ])
                .env(WORKER, &worker_world);
            command
                .stdin(Stdio::from(input))
                .stdout(Stdio::null())
                .stderr(Stdio::from(
                    std::fs::File::create(worker_world.join("compiler.log")).unwrap(),
                ));
            let mut requests = Vec::new();
            let outcome = supervise_headless_controlled(
                held,
                &mut command,
                &mut |_| {},
                cancellation,
                &mut |event| {
                    if let SupervisedEvent::GroupRequestAccepted(signal) = event {
                        requests.push(signal);
                    }
                    Ok(JobControl::Wait)
                },
            );
            assert!(!command.starts_suspended(), "caller start policy restored");
            (outcome, requests)
        });
        jobs.push(HeadlessJob {
            worker: Some(worker),
            input: writer,
            cancel,
            world: job_world,
            started: root,
        });
    }
    for job in &mut jobs {
        let root = aterm_spec::wait::until_within(
            aterm_spec::wait::MUST_HAPPEN,
            "the actual owned headless root is published",
            || match job.started.try_recv() {
                Ok(root) => Some(root),
                Err(std::sync::mpsc::TryRecvError::Empty) => {
                    assert!(
                        !job.is_finished(),
                        "job ended before publishing its owned root"
                    );
                    None
                }
                Err(std::sync::mpsc::TryRecvError::Disconnected) => {
                    panic!("job did not publish its owned root");
                }
            },
        );
        wait_ready(&job.world, root, || job.is_finished());
    }
    assert_eq!(ledger.list().unwrap().len(), 2);
    assert!(
        ledger
            .list()
            .unwrap()
            .iter()
            .all(|r| r.accounting == aterm_capacity::Accounting::Whole)
    );
    machine.set(concurrent_reading(256, 1, 128));
    assert!(
        matches!(
            admit_cohort_job(&test_build(), &world, &selected, &mut |_| {}),
            Admission::Refused(_)
        ),
        "a host cannot force a third job through memory pressure"
    );
    assert_eq!(
        ledger.list().unwrap().len(),
        2,
        "a refused one-shot leaves no waiting claim"
    );
    machine.set(concurrent_reading(256, 200, 128));
    let mut second = jobs.pop().unwrap();
    let mut first = jobs.pop().unwrap();
    first.cancel.request(CancelRequest::Terminate).unwrap();
    aterm_spec::wait::until_while_descendants_work("only cancelled job ends", || {
        first.is_finished().then_some(())
    });
    let (outcome, requests) = first.join().unwrap();
    let outcome = outcome.unwrap();
    assert!(outcome.cancelled());
    assert!(!outcome.succeeded());
    assert_eq!(outcome.root_status().signal(), Some(libc::SIGKILL));
    assert_eq!(outcome.cohort_settlement(), Some(CohortSettlement::Drained));
    assert_eq!(
        requests,
        [
            CohortSignal::Terminate,
            CohortSignal::Continue,
            CohortSignal::Kill
        ]
    );
    assert!(!second.is_finished(), "the other live job is not cancelled");
    let rows = ledger.list().unwrap();
    assert_eq!(rows.len(), 1);
    assert_eq!(rows[0].need.mem, second_need);
    assert_eq!(rows[0].accounting, aterm_capacity::Accounting::Whole);
    second.input.write_all(b"finish\n").unwrap();
    aterm_spec::wait::until_while_descendants_work("uncancelled peer completes", || {
        second.is_finished().then_some(())
    });
    let (outcome, requests) = second.join().unwrap();
    assert!(outcome.unwrap().succeeded());
    assert!(requests.is_empty());
    assert!(ledger.list().unwrap().is_empty());
}

#[test]
fn independent_headless_jobs_run_without_a_global_front_and_cancel_in_isolation() {
    run_actor("cancel");
}

#[test]
fn a_headless_job_cannot_claim_foreground_and_failure_reaps_its_root() {
    run_actor("foreground");
}

fn run_actor(mode: &str) {
    let world = aterm_tempfile::tempdir().unwrap();
    let mut command = spawnfd::command(std::env::current_exe().unwrap());
    command
        .args([
            "--exact",
            "build_admit::tests::headless::headless_host_actor_if_requested",
            "--test-threads=1",
            "--nocapture",
        ])
        .env(ACTOR, world.path());
    if mode == "foreground" {
        command.env(STOP, "1");
    }
    let output = actor_output(&mut command, "the private headless host actor finishes");
    let output = match output {
        Ok(output) => output,
        Err(error) => {
            let world = world.keep();
            panic!("headless host failed: {error}; KEPT {}", world.display());
        }
    };
    let cleanup = (0..2).any(|index| {
        world
            .path()
            .join(index.to_string())
            .join("owner-cleanup")
            .exists()
    });
    if !output.status.success() || cleanup {
        let world = world.keep();
        panic!(
            "headless host failed ({}; owner cleanup={cleanup}): {}; KEPT {}",
            output.status,
            String::from_utf8_lossy(&output.stderr),
            world.display()
        );
    }
}

/// A one-crate package in `world` whose real compiled build script connects to the
/// outer test's lifetime socket before writing `ready`, then waits for release or EOF:
/// the two real
/// `targo` builds the session's test holds at their barriers
/// (`one_host_session_finishes_two_actual_targo_builds_after_clients_disconnect`).
pub(super) fn prepare_build(world: &Path) {
    std::fs::create_dir(world.join("src")).unwrap();
    std::fs::write(
        world.join("Cargo.toml"),
        "[package]\nname='headless_probe'\nversion='0.0.0'\nedition='2024'\n[workspace]\n",
    )
    .unwrap();
    std::fs::write(
        world.join("src/main.rs"),
        "fn main() { assert_eq!((0..10).sum::<u64>(), 45); println!(\"host service probe executed\"); }\n",
    )
    .unwrap();
    std::fs::write(
        world.join("build.rs"),
        r#"
fn main() {
    use std::io::{Read as _, Write as _};
    let world = std::path::PathBuf::from(std::env::var_os("ATPKG_HEADLESS_REAL_BUILDS").unwrap());
    let mut owner = std::os::unix::net::UnixStream::connect(
        std::env::var_os("ATPKG_HEADLESS_SESSION_OWNER").unwrap(),
    ).unwrap();
    owner.write_all(b"B").unwrap();
    let mut byte = [0];
    owner.read_exact(&mut byte).unwrap();
    assert_eq!(byte, *b"A");
    std::fs::write(world.join("ready"), b"actual compiled build script running\n").unwrap();
    loop {
        match owner.read(&mut byte) {
            Ok(0) => break,
            Ok(1) if byte[0] == b'R' => break,
            Ok(1) if byte[0] == b'?' => owner.write_all(b"A").unwrap(),
            Err(error) if error.kind() == std::io::ErrorKind::Interrupted => continue,
            other => panic!("invalid fixture lifetime response: {other:?}"),
        }
    }
}
"#,
    )
    .unwrap();
}
