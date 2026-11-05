// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! One actual caller drives owned native jobs; private ledgers and native fixtures.

use super::host::{Completion, JobId, Jobs, Ticket};
use super::*;
use aterm_capacity::cohort::launch::CohortSignal;
use aterm_uds::spawnfd::{self, Stdio};
use std::{
    io::{Read as _, Write as _},
    os::fd::AsFd as _,
    time::{Duration, Instant},
};

const ACTOR: &str = "ATPKG_NATIVE_JOB_TABLE_WORLD";
const REAL: &str = "ATPKG_NATIVE_JOB_TABLE_REAL";

#[test]
fn shared_host_wait_services_mixed_interests_and_buffered_work() {
    use aterm_uds::ready::{Interest, Watch};
    let jobs = Jobs::new(1).unwrap();
    let (mut control, mut send) = std::io::pipe().unwrap();
    let (mut drain, mut input) = std::io::pipe().unwrap();
    let deadline = Instant::now() + aterm_spec::wait::MUST_HAPPEN;
    let wake = jobs
        .wait_with(
            control.as_fd(),
            &[Watch::new(input.as_fd(), Interest::Write)],
            false,
            deadline,
        )
        .unwrap();
    assert!(wake.jobs_hint && !wake.control_ready && !wake.deadline);
    input.write_all(b"helper-command").unwrap();
    let mut bytes = [0; 14];
    drain.read_exact(&mut bytes).unwrap();
    assert_eq!(&bytes, b"helper-command");
    // This is already buffered protocol work, not a claim of ready child state.
    let wake = jobs
        .wait_with(control.as_fd(), &[], true, deadline)
        .unwrap();
    assert!(wake.jobs_hint && !wake.deadline);
    send.write_all(b"c").unwrap();
    let wake = jobs
        .wait_with(control.as_fd(), &[], true, deadline)
        .unwrap();
    assert!(
        wake.control_ready && wake.jobs_hint && !wake.deadline,
        "local work must not mask queued host control"
    );
    assert!(jobs.wait(control.as_fd(), deadline).unwrap().control_ready);
    let mut byte = [0];
    control.read_exact(&mut byte).unwrap();
    assert_eq!(byte, *b"c");
    let wake = jobs
        .wait_with(control.as_fd(), &[], false, Instant::now())
        .unwrap();
    assert!(wake.deadline && !wake.jobs_hint && !wake.control_ready);
    let wake = jobs
        .wait_with(control.as_fd(), &[], true, Instant::now())
        .unwrap();
    assert!(wake.deadline && wake.jobs_hint && !wake.control_ready);
}

struct Driver {
    jobs: Jobs,
    control: std::io::PipeReader,
    sender: std::io::PipeWriter,
    events: Vec<(JobId, SupervisedEvent)>,
    done: Vec<Completion>,
    reject: Option<JobId>,
    thread: std::thread::ThreadId,
    trace: PathBuf,
}

impl Driver {
    fn new(world: &Path) -> Self {
        let (control, sender) = std::io::pipe().unwrap();
        Self {
            jobs: Jobs::new(2).unwrap(),
            control,
            sender,
            events: Vec::new(),
            done: Vec::new(),
            reject: None,
            thread: std::thread::current().id(),
            trace: world.join("phase"),
        }
    }
    fn phase(&self, why: &str) {
        let text = format!(
            "{why}\n{:?}\n{:?}",
            self.jobs.snapshot().unwrap(),
            self.events
        );
        std::fs::write(&self.trace, &text).unwrap();
        if let Some(path) = std::env::var_os("ATPKG_JOB_TABLE_TEST_TRACE") {
            std::fs::write(path, text).unwrap();
        }
    }
    fn tick(&mut self) {
        let Self {
            jobs,
            events,
            done,
            reject,
            thread,
            trace,
            ..
        } = self;
        done.extend(
            jobs.tick(
                &mut |id, event| {
                    assert_eq!(
                        std::thread::current().id(),
                        *thread,
                        "one host caller owns every observation"
                    );
                    events.push((id.clone(), event));
                    let text = format!("native event: {event:?}\n{events:?}");
                    std::fs::write(&*trace, &text).unwrap();
                    if let Some(path) = std::env::var_os("ATPKG_JOB_TABLE_TEST_TRACE") {
                        std::fs::write(path, text).unwrap();
                    }
                    if reject.as_ref() == Some(id)
                        && matches!(
                            event,
                            SupervisedEvent::GroupRequestAccepted(CohortSignal::Terminate)
                        )
                    {
                        return Err(std::io::Error::other("native table observer rejected TERM"));
                    }
                    Ok(JobControl::Wait)
                },
                &mut |line| eprintln!("{line}"),
            )
            .unwrap(),
        );
    }
    fn until(&mut self, why: &str, found: impl Fn(&Self) -> bool) {
        self.phase(why);
        let deadline = Instant::now() + aterm_spec::wait::MUST_HAPPEN;
        loop {
            self.tick();
            if found(self) {
                return;
            }
            assert!(
                !self.jobs.is_empty(),
                "unmet host phase {why}: {:?}; {:?}",
                self.events,
                self.done
            );
            self.phase(why);
            assert!(Instant::now() < deadline, "native table hang: {why}");
            self.jobs.wait(self.control.as_fd(), deadline).unwrap();
        }
    }
    fn ready(&mut self, paths: &[&Path]) {
        self.phase("waiting for actual fixture readiness");
        aterm_spec::wait::until_within(
            aterm_spec::wait::MUST_HAPPEN,
            "actual fixture readiness",
            || {
                self.tick();
                assert!(
                    self.done.is_empty(),
                    "no fixture may end before readiness: {:?}",
                    self.done
                );
                paths
                    .iter()
                    .all(|path| path.join("ready").exists())
                    .then_some(())
            },
        );
    }
    fn result(&self, id: &JobId) -> &std::io::Result<JobOutcome> {
        &self.done.iter().find(|done| &done.id == id).unwrap().result
    }
}

pub(super) fn held(world: &Path, seat: &Seat<'_>) -> Held {
    let build = classify(&words(&["--unverified", "build"]), Tool::Targo).unwrap();
    match admit_cohort_job(&build, world, seat, &mut |_| {}) {
        Admission::Claimed(held) => *held,
        other => panic!("table job admission refused: {other:?}"),
    }
}

pub(super) fn worker(world: &Path, stop: bool) -> (spawnfd::Command, std::io::PipeWriter) {
    std::fs::create_dir(world).unwrap();
    let (input, writer) = std::io::pipe().unwrap();
    let mut command = spawnfd::command(std::env::current_exe().unwrap());
    command
        .args([
            "--exact",
            "build_admit::tests::headless::headless_worker_if_requested",
            "--test-threads=1",
        ])
        .env(headless::WORKER, world)
        .stdin(Stdio::from(input))
        .stdout(Stdio::null())
        .stderr(Stdio::null());
    if stop {
        command.env(headless::STOP, "1");
    }
    (command, writer)
}

#[test]
fn native_table_actor_if_requested() {
    let Some(world) = std::env::var_os(ACTOR) else {
        return;
    };
    let world = PathBuf::from(world);
    std::fs::write(world.join("phase"), b"starting actor").unwrap();
    let machine = Fixed::new(Reading::of_gib(256, 200, 128));
    let mut seat = cohort_seat(&world, &machine);
    seat.own = None;
    let ledger = seat.ledger.as_ref().unwrap();
    let _turn = crate::platform::FORWARDING_TURN.lock().unwrap();
    let mut driver = Driver::new(&world);
    assert!(Jobs::new(0).is_err());
    assert!(Jobs::new(257).is_err());
    if std::env::var_os(REAL).is_some() {
        real_builds(&world, &seat, &mut driver);
        return;
    }

    let first = world.join("first");
    driver.phase("submitting first root");
    let peer = world.join("peer");
    let (mut command, _input) = worker(&first, false);
    let ticket = driver
        .jobs
        .submit(
            held(&first, &seat),
            &mut command,
            Duration::from_millis(100),
        )
        .unwrap();
    assert!(!command.starts_suspended());
    let (mut command, mut peer_input) = worker(&peer, true);
    driver.phase("submitting peer");
    let peer_ticket = driver
        .jobs
        .submit(held(&peer, &seat), &mut command, Duration::ZERO)
        .unwrap();
    assert_eq!(driver.jobs.len(), 2);
    let mut forbidden = spawnfd::command("/usr/bin/touch");
    driver.phase("refusing full table");
    forbidden.arg(world.join("forbidden"));
    assert_eq!(
        driver
            .jobs
            .submit(held(&world, &seat), &mut forbidden, Duration::ZERO)
            .unwrap_err()
            .kind(),
        std::io::ErrorKind::WouldBlock
    );
    assert!(!world.join("forbidden").exists());
    driver.ready(&[&first, &peer]);
    assert!(
        ledger
            .list()
            .unwrap()
            .iter()
            .all(|r| r.accounting == aterm_capacity::Accounting::Whole)
    );
    driver.until("peer actually stops", |d| {
        d.events.iter().any(|(id, event)| {
            id == &peer_ticket.id && matches!(event, SupervisedEvent::Stopped(_))
        })
    });
    let mut foreign = Jobs::new(1).unwrap();
    assert_eq!(
        foreign
            .resume_background(&peer_ticket.id)
            .unwrap_err()
            .kind(),
        std::io::ErrorKind::PermissionDenied
    );
    assert_eq!(
        driver.jobs.resume_background(&peer_ticket.id).unwrap(),
        SupervisedEvent::ResumeAccepted { foreground: false }
    );
    driver.sender.write_all(b"c").unwrap();
    assert!(
        driver
            .jobs
            .wait(
                driver.control.as_fd(),
                Instant::now() + aterm_spec::wait::MUST_HAPPEN
            )
            .unwrap()
            .control_ready
    );
    let mut byte = [0];
    driver.control.read_exact(&mut byte).unwrap();
    assert_eq!(byte, *b"c", "host control wake did not consume its message");
    let issued = Instant::now();
    ticket.cancel.request(CancelRequest::Terminate).unwrap();
    driver.until("first cancelled root completes", |d| {
        d.done.iter().any(|done| done.id == ticket.id)
    });
    assert!(Instant::now() >= issued + Duration::from_millis(100));
    let outcome = driver.result(&ticket.id).as_ref().unwrap();
    assert!(outcome.cancelled());
    assert!(!outcome.succeeded());
    assert_eq!(outcome.cohort_settlement(), Some(CohortSettlement::Drained));
    assert_eq!(driver.jobs.len(), 1);
    assert_eq!(ledger.list().unwrap().len(), 1);
    assert!(
        driver
            .events
            .iter()
            .filter(|(id, _)| id == &peer_ticket.id)
            .all(|(_, event)| !matches!(event, SupervisedEvent::GroupRequestAccepted(_)))
    );
    assert_eq!(
        driver
            .jobs
            .resume_background(&ticket.id)
            .unwrap_err()
            .kind(),
        std::io::ErrorKind::NotFound
    );
    peer_input.write_all(b"done\n").unwrap();
    driver.until("peer completes independently", |d| d.jobs.is_empty());
    assert!(driver.result(&peer_ticket.id).as_ref().unwrap().succeeded());
    assert!(ledger.list().unwrap().is_empty());

    // The next attempt gets a new identity; stale control cannot select it.
    driver.done.clear();
    let failed = world.join("failed-observer");
    let (mut command, _input) = worker(&failed, false);
    let rejected = driver
        .jobs
        .submit(held(&failed, &seat), &mut command, Duration::ZERO)
        .unwrap();
    assert!(rejected.id.sequence() > peer_ticket.id.sequence());
    assert_eq!(
        driver
            .jobs
            .resume_background(&ticket.id)
            .unwrap_err()
            .kind(),
        std::io::ErrorKind::NotFound
    );
    driver.ready(&[&failed]);
    driver.reject = Some(rejected.id.clone());
    rejected.cancel.request(CancelRequest::Terminate).unwrap();
    driver.until("failed observer is cleaned without blocking table", |d| {
        d.jobs.is_empty()
    });
    assert_eq!(
        driver.result(&rejected.id).as_ref().unwrap_err().kind(),
        std::io::ErrorKind::Other
    );
    assert!(ledger.list().unwrap().is_empty());
    driver.reject = None;

    // A real cleanup root exits zero after reaping its own compiler, while the
    // table retains its exit pin until the explicit future grace has elapsed.
    driver.done.clear();
    let clean = world.join("cleanup-zero");
    std::fs::create_dir(&clean).unwrap();
    let (exit_reader, mut exit_writer) = std::io::pipe().unwrap();
    let mut command = spawnfd::command(std::env::current_exe().unwrap());
    command
        .args([
            "--exact",
            "build_admit::tests::terminal::native_cancel_worker_if_requested",
            "--test-threads=1",
        ])
        .env(terminal::WORLD, &clean)
        .env(terminal::CANCEL_WORKER, "running")
        .env(terminal::CANCEL_EXIT_BARRIER, "1")
        .stdin(Stdio::from(exit_reader))
        .stdout(Stdio::null())
        .stderr(Stdio::null());
    let clean_ticket = driver
        .jobs
        .submit(held(&clean, &seat), &mut command, Duration::from_secs(600))
        .unwrap();
    driver.ready(&[&clean]);
    clean_ticket
        .cancel
        .request(CancelRequest::Terminate)
        .unwrap();
    driver.until("cleanup root waits for the accepted CONT", |d| {
        d.events.iter().any(|(id, event)| {
            id == &clean_ticket.id
                && *event == SupervisedEvent::GroupRequestAccepted(CohortSignal::Continue)
        })
    });
    exit_writer.write_all(&[1]).unwrap();
    drop(exit_writer);
    driver.until("cleanup root exit observed without reaping", |d| {
        d.events.iter().any(|(id, event)| {
            id == &clean_ticket.id && *event == SupervisedEvent::RootExitObserved
        })
    });
    assert_eq!(driver.jobs.len(), 1);
    assert_eq!(
        ledger.list().unwrap()[0].accounting,
        aterm_capacity::Accounting::Whole
    );
    clean_ticket.cancel.request(CancelRequest::Kill).unwrap();
    driver.until("explicit KILL permits accounted reap", |d| {
        d.jobs.is_empty()
    });
    match driver.result(&clean_ticket.id) {
        Ok(outcome) => {
            assert!(outcome.root_status().success());
            assert!(outcome.cancelled());
            assert!(!outcome.succeeded());
        }
        Err(error) => {
            // Darwin can refuse KILL for an already exited-only group. Keep
            // that exact kernel refusal; reaping its owned exit still needs
            // no signal permission and group absence must settle separately.
            assert_eq!(error.raw_os_error(), Some(libc::EPERM));
            assert!(
                !driver
                    .events
                    .iter()
                    .any(|(id, event)| id == &clean_ticket.id
                        && *event == SupervisedEvent::GroupRequestAccepted(CohortSignal::Kill))
            );
        }
    }
    assert!(
        driver
            .events
            .iter()
            .any(|(id, event)| id == &clean_ticket.id
                && matches!(event, SupervisedEvent::RootExited(usage) if usage.status.success()))
    );
    assert!(
        driver
            .events
            .iter()
            .any(|(id, event)| id == &clean_ticket.id && *event == SupervisedEvent::CohortDrained)
    );
    let pid: u32 = std::fs::read_to_string(clean.join("root-pid"))
        .unwrap()
        .parse()
        .unwrap();
    assert_reaped(pid);
    assert!(ledger.list().unwrap().is_empty());
    let count = driver.done.len();
    driver.tick();
    assert_eq!(
        driver.done.len(),
        count,
        "completion is delivered only once"
    );

    // Emergency table Drop must reap its exact owned root as well.
    let abort = world.join("abort");
    let (mut command, _input) = worker(&abort, false);
    let _abort_ticket = driver
        .jobs
        .submit(held(&abort, &seat), &mut command, Duration::ZERO)
        .unwrap();
    driver.done.clear();
    driver.ready(&[&abort]);
    let pid: u32 = std::fs::read_to_string(abort.join("ready"))
        .unwrap()
        .parse()
        .unwrap();
    drop(driver);
    assert_reaped(pid);
    assert!(ledger.list().unwrap().is_empty());
}

fn assert_reaped(pid: u32) {
    let mut info = std::mem::MaybeUninit::<libc::siginfo_t>::zeroed();
    // SAFETY: query this fixture's actual child, without consuming anything.
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
}

fn real_builds(world: &Path, seat: &Seat<'_>, driver: &mut Driver) {
    let mut tickets = Vec::<Ticket>::new();
    let mut paths = Vec::new();
    struct Release(Vec<PathBuf>);
    impl Drop for Release {
        fn drop(&mut self) {
            for path in &self.0 {
                let _ = std::fs::write(path.join("release"), b"cleanup");
            }
        }
    }
    let mut release = Release(Vec::new());
    for index in 0..2 {
        let path = world.join(index.to_string());
        std::fs::create_dir(&path).unwrap();
        headless::prepare_build(&path);
        release.0.push(path.clone());
        let mut command = spawnfd::transparent(std::env::var_os("CARGO").unwrap()).into_command();
        command
            .args(["--unverified", "build", "--offline", "--target-dir"])
            .arg(path.join("target.noindex"))
            .args([
                "--config",
                &format!("build.build-dir='{}'", path.join("build.noindex").display()),
            ])
            .env("ATPKG_HEADLESS_REAL_BUILDS", &path)
            .env("CARGO_BUILD_JOBS", "1")
            .current_dir(&path)
            .stdout(Stdio::null())
            .stderr(Stdio::from(
                std::fs::File::create(path.join("compiler.log")).unwrap(),
            ));
        tickets.push(
            driver
                .jobs
                .submit(held(&path, seat), &mut command, Duration::ZERO)
                .unwrap(),
        );
        paths.push(path);
    }
    driver.ready(&paths.iter().map(PathBuf::as_path).collect::<Vec<_>>());
    assert_eq!(
        driver.jobs.len(),
        2,
        "both actual build scripts reached their barriers"
    );
    for path in &paths {
        std::fs::write(path.join("release"), b"host accepts progress").unwrap();
    }
    driver.until("both actual builds finish on one wake loop", |d| {
        d.jobs.is_empty()
    });
    for (ticket, path) in tickets.iter().zip(&paths) {
        assert!(driver.result(&ticket.id).as_ref().unwrap().succeeded());
        let mut command = spawnfd::command(path.join("target.noindex/debug/headless_probe"));
        assert!(
            aterm_uds::deadline::output_within(&mut command, aterm_spec::wait::MUST_HAPPEN)
                .unwrap()
                .status
                .success()
        );
    }
    assert!(seat.ledger.as_ref().unwrap().list().unwrap().is_empty());
}

fn run_actor(real: bool) {
    let world = aterm_tempfile::tempdir().unwrap();
    let mut command = spawnfd::command(std::env::current_exe().unwrap());
    command
        .args([
            "--exact",
            "build_admit::tests::host_table::native_table_actor_if_requested",
            "--test-threads=1",
            "--nocapture",
        ])
        .env(ACTOR, world.path());
    if real {
        command.env(REAL, "1");
    }
    let output = aterm_uds::deadline::output_within(&mut command, aterm_spec::wait::MUST_HAPPEN)
        .unwrap_or_else(|error| {
            panic!(
                "actor {error:?}; last phase: {}",
                std::fs::read_to_string(world.path().join("phase")).unwrap_or_default()
            )
        });
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
}

#[test]
fn one_table_services_native_control_deadlines_errors_and_stale_attempts() {
    run_actor(false);
}

#[test]
fn one_table_services_two_real_targo_builds_without_per_job_waiting_threads() {
    run_actor(true);
}

#[test]
fn failed_cleanup_ends_a_surviving_member_before_reaping_its_exited_leader() {
    use std::os::unix::process::ExitStatusExt as _;
    struct Root(SupervisedChild);
    impl Drop for Root {
        fn drop(&mut self) {
            let child = self.0.child_mut();
            let _ = child.kill();
            let _ = child.wait();
        }
    }
    struct Member(spawnfd::Child);
    impl Drop for Member {
        fn drop(&mut self) {
            let _ = self.0.kill();
            let _ = self.0.wait();
        }
    }
    let world = aterm_tempfile::tempdir().unwrap();
    let machine = Fixed::new(Reading::of_gib(256, 200, 128));
    let seat = cohort_seat(world.path(), &machine);
    let mut admission = held(world.path(), &seat);
    let mut command = spawnfd::command("/bin/cat");
    command.stdin(Stdio::piped()).stdout(Stdio::piped());
    let (owned, error) = launch_held(Some(&mut admission), &mut command).unwrap();
    assert!(error.is_none());
    let mut root = Root(owned);
    let input = aterm_uds::deadline::cat_echoes(root.0.child_mut());
    let SupervisedChild::Bound(bound) = &mut root.0 else {
        panic!("actual bound leader");
    };
    let mut compiler = spawnfd::command("/bin/sh");
    compiler
        .args(["-c", "trap '' TERM; exec /bin/cat"])
        .stdin(Stdio::piped())
        .stdout(Stdio::piped());
    let mut member = Member(
        admission
            .claim
            .launch_cohort_member(bound, &mut compiler)
            .unwrap(),
    );
    let _member_input = aterm_uds::deadline::cat_echoes(&mut member.0);
    drop(input);
    {
        let observed = root.0.child_mut().wait_job_event().unwrap();
        assert_eq!(observed.notice(), spawnfd::ChildNotice::Exited);
    }
    assert!(request_owned_cleanup(&mut root.0, &mut |_| {}));
    assert!(
        !root.0.child_mut().is_reaped(),
        "keep group authority through the request"
    );
    aterm_spec::wait::until_within(
        aterm_spec::wait::MUST_HAPPEN,
        "authorized cleanup kills the surviving actual compiler",
        || member.0.exited().unwrap().then_some(()),
    );
    assert_eq!(member.0.wait().unwrap().signal(), Some(libc::SIGKILL));
    assert!(root.0.child_mut().wait().unwrap().success());
    admission.claim.finish_cohort().unwrap();
    drop(admission);
    assert!(seat.ledger.as_ref().unwrap().list().unwrap().is_empty());
}
