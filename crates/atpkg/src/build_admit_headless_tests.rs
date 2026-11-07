// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Independent native jobs in one process, with no foreground front or real ledger.

use super::*;
use aterm_capacity::cohort::launch::CohortSignal;
use aterm_uds::spawnfd::{self, Stdio};
use std::{io::Write as _, time::Duration};

const ACTOR: &str = "ATPKG_HEADLESS_HOST_WORLD";
pub(super) const WORKER: &str = "ATPKG_HEADLESS_WORKER_WORLD";
const REAL: &str = "ATPKG_HEADLESS_REAL_BUILDS";
pub(super) const STOP: &str = "ATPKG_HEADLESS_STOP_WORKER";

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

fn wait_ready(world: &Path, finished: impl Fn() -> bool) {
    aterm_spec::wait::until_within(
        aterm_spec::wait::MUST_HAPPEN,
        "actual headless worker ready",
        || {
            if world.join("ready").exists() {
                return Some(());
            }
            assert!(
                !finished(),
                "worker ended before readiness: {}",
                std::fs::read_to_string(world.join("compiler.log")).unwrap_or_default()
            );
            None
        },
    );
}

#[test]
fn headless_host_actor_if_requested() {
    use std::os::unix::process::ExitStatusExt as _;
    let Some(world) = std::env::var_os(ACTOR) else {
        return;
    };
    let world = PathBuf::from(world);
    let real = std::env::var_os(REAL).is_some();
    let machine = Fixed::new(Reading::of_gib(256, 200, 128));
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
        if real {
            prepare_build(&job);
        }
    }
    struct Release(PathBuf);
    impl Drop for Release {
        fn drop(&mut self) {
            // Also release both compiled fixture barriers on an assertion failure.
            for index in 0..2 {
                let _ = std::fs::write(
                    self.0.join(index.to_string()).join("release"),
                    b"fixture cleanup",
                );
            }
        }
    }
    let _release = Release(world.clone());
    let first = claim_job(&world.join("0"), &selected);
    let second = claim_job(&world.join("1"), &selected);
    let second_need = second.claim().record().need.mem;
    let expected = first.claim().record().need.mem + second.claim().record().need.mem;
    let rows = ledger.list().unwrap();
    assert_eq!(rows.len(), 2, "same owner still reserves two actual jobs");
    assert_eq!(rows.iter().map(|r| r.need.mem).sum::<u64>(), expected);

    // A regression to a per-job Front cannot get through this occupied global
    // turn. The outer private actor's one-minute detector catches that deadlock.
    let _turn = crate::platform::FORWARDING_TURN.lock().unwrap();
    let mut jobs = Vec::new();
    for (index, held) in [first, second].into_iter().enumerate() {
        let job_world = world.join(index.to_string());
        let (input, writer) = std::io::pipe().unwrap();
        let (cancel, cancellation) = Cancellation::channel(Duration::from_millis(100)).unwrap();
        let worker_world = job_world.clone();
        let worker = std::thread::spawn(move || {
            let mut command = if real {
                let mut command =
                    spawnfd::transparent(std::env::var_os("CARGO").unwrap()).into_command();
                command
                    .args(["--unverified", "build", "--offline", "--target-dir"])
                    .arg(worker_world.join("target.noindex"))
                    .args([
                        "--config",
                        &format!(
                            "build.build-dir='{}'",
                            worker_world.join("build.noindex").display()
                        ),
                    ])
                    .env(REAL, &worker_world)
                    .env("CARGO_BUILD_JOBS", "1")
                    .current_dir(&worker_world);
                command
            } else {
                let mut command = spawnfd::command(std::env::current_exe().unwrap());
                command
                    .args([
                        "--exact",
                        "build_admit::tests::headless::headless_worker_if_requested",
                        "--test-threads=1",
                    ])
                    .env(WORKER, &worker_world);
                command
            };
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
        jobs.push((worker, writer, cancel, job_world));
    }
    for (job, _, _, world) in &jobs {
        wait_ready(world, || job.is_finished());
    }
    assert_eq!(ledger.list().unwrap().len(), 2);
    assert!(
        ledger
            .list()
            .unwrap()
            .iter()
            .all(|r| r.accounting == aterm_capacity::Accounting::Whole)
    );
    machine.set(Reading::of_gib(256, 1, 128));
    assert!(
        matches!(
            admit_cohort_job(&test_build(), &world, &selected, &mut |_| {}),
            Admission::Stopped(Error::Stopped {
                signal: None,
                short: Some(_)
            })
        ),
        "a host cannot force a third job through memory pressure"
    );
    assert_eq!(
        ledger.list().unwrap().len(),
        2,
        "a refused one-shot leaves no waiting claim"
    );
    machine.set(Reading::of_gib(256, 200, 128));
    if real {
        for (_, _, _, world) in &jobs {
            std::fs::write(world.join("release"), b"host permits completion").unwrap();
        }
        for (job, _, _, world) in jobs {
            aterm_spec::wait::until_within(
                aterm_spec::wait::MUST_HAPPEN,
                "actual Targo compile finishes",
                || job.is_finished().then_some(()),
            );
            let (outcome, requests) = job.join().unwrap();
            assert!(outcome.unwrap().succeeded());
            assert!(requests.is_empty());
            let mut program = spawnfd::command(world.join("target.noindex/debug/headless_probe"));
            assert!(
                aterm_uds::deadline::output_within(&mut program, aterm_spec::wait::MUST_HAPPEN)
                    .unwrap()
                    .status
                    .success()
            );
        }
        assert!(ledger.list().unwrap().is_empty());
        return;
    }
    let (second, mut second_input, _second_cancel, _) = jobs.pop().unwrap();
    let (first, _first_input, first_cancel, _) = jobs.pop().unwrap();
    first_cancel.request(CancelRequest::Terminate).unwrap();
    aterm_spec::wait::until_within(
        aterm_spec::wait::MUST_HAPPEN,
        "only cancelled job ends",
        || first.is_finished().then_some(()),
    );
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
    second_input.write_all(b"finish\n").unwrap();
    aterm_spec::wait::until_within(
        aterm_spec::wait::MUST_HAPPEN,
        "uncancelled peer completes",
        || second.is_finished().then_some(()),
    );
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
fn one_headless_host_runs_two_actual_targo_builds_concurrently() {
    run_actor("real");
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
    if mode == "real" {
        command.env(REAL, "true");
    }
    if mode == "foreground" {
        command.env(STOP, "1");
    }
    let output =
        aterm_uds::deadline::output_within(&mut command, aterm_spec::wait::MUST_HAPPEN).unwrap();
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
}

pub(super) fn prepare_build(world: &Path) {
    std::fs::create_dir(world.join("src")).unwrap();
    std::fs::write(
        world.join("Cargo.toml"),
        "[package]\nname='headless_probe'\nversion='0.0.0'\nedition='2024'\n[workspace]\n",
    )
    .unwrap();
    std::fs::write(
        world.join("src/main.rs"),
        "fn main() { assert_eq!((0..10).sum::<u64>(), 45); }\n",
    )
    .unwrap();
    std::fs::write(
        world.join("build.rs"),
        r#"
fn main() {
    let world = std::path::PathBuf::from(std::env::var_os("ATPKG_HEADLESS_REAL_BUILDS").unwrap());
    std::fs::write(world.join("ready"), b"actual compiled build script running").unwrap();
    while !world.join("release").exists() {
        assert!(world.is_dir(), "build fixture owner disappeared");
        std::thread::sleep(std::time::Duration::from_millis(10));
    }
}
"#,
    )
    .unwrap();
}
