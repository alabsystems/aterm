// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Actual native jobs behind independent read/control clients, in a private actor.

use super::service::{Cursor, Event, Owner, ReadError, Rejection};
use super::*;
use aterm_capacity::{
    cohort::launch::CohortSignal,
    host_protocol::{Attempt, Generation, Snapshot},
};
use aterm_uds::spawnfd::{self, Stdio};
use std::{
    io::Write as _,
    time::{Duration, Instant},
};

const ACTOR: &str = "ATPKG_NATIVE_SESSION_WORLD";
const REAL: &str = "ATPKG_NATIVE_SESSION_REAL";
const FENCED: &str = "ATPKG_NATIVE_SESSION_FENCED";
const MONITOR: &str = "ATPKG_NATIVE_SESSION_MONITOR";

#[test]
fn monitor_reader_if_requested() {
    use super::service::monitor::Client;
    use std::os::fd::AsFd as _;
    let Some(world) = std::env::var_os(MONITOR) else {
        return;
    };
    let world = PathBuf::from(world);
    let generation = Generation(
        std::fs::read(world.join("monitor.generation"))
            .unwrap()
            .try_into()
            .unwrap(),
    );
    // Trusted parent explicitly handed this private socket to this reader.
    let stream = aterm_uds::CtlStream::from(std::io::stdin().as_fd().try_clone_to_owned().unwrap());
    let reply = Client::from_trusted_stream(stream, generation)
        .unwrap()
        .read_until(Instant::now() + aterm_spec::wait::MUST_HAPPEN)
        .unwrap();
    assert_eq!(
        reply.snapshot.jobs.len(),
        2,
        "both actual builds are observed at their barriers"
    );
    let age = reply.age_bounds().unwrap();
    assert!(age.upper >= age.lower);
    std::fs::write(
        world.join("monitor.reply"),
        reply.snapshot.encode().unwrap(),
    )
    .unwrap();
}

struct Driver {
    owner: Owner,
    done: Vec<super::host::Completion>,
    reject: Option<Attempt>,
    trace: PathBuf,
}

impl Driver {
    fn new(world: &Path) -> Self {
        Self {
            owner: Owner::new(2, 128).unwrap(),
            done: Vec::new(),
            reject: None,
            trace: world.join("phase"),
        }
    }
    fn tick(&mut self) {
        let reject = self.reject;
        self.done.extend(
            self.owner
                .tick(
                    &mut |id, event| {
                        if reject == Some(id)
                            && matches!(
                                event,
                                SupervisedEvent::GroupRequestAccepted(CohortSignal::Terminate)
                            )
                        {
                            return Err(std::io::Error::other("PRIVATE_SECRET_POLICY_FAILURE"));
                        }
                        Ok(JobControl::Wait)
                    },
                    &mut |_| {},
                )
                .unwrap(),
        );
        std::fs::write(
            &self.trace,
            format!("{:?}\n{:?}", self.owner.observer().read(None), self.done),
        )
        .unwrap();
    }
    fn until(&mut self, why: &str, found: impl Fn(&Self) -> bool) {
        let deadline = Instant::now() + aterm_spec::wait::MUST_HAPPEN;
        loop {
            self.tick();
            if found(self) {
                return;
            }
            assert!(!self.owner.is_empty(), "{why}: {:?}", self.done);
            assert!(Instant::now() < deadline, "native session hang: {why}");
            self.owner.wait(deadline).unwrap();
        }
    }
    fn ready(&mut self, paths: &[&Path]) {
        aterm_spec::wait::until_within(
            aterm_spec::wait::MUST_HAPPEN,
            "session fixture readiness",
            || {
                self.tick();
                assert!(
                    self.done.is_empty(),
                    "premature completion: {:?}",
                    self.done
                );
                paths.iter().all(|p| p.join("ready").exists()).then_some(())
            },
        );
    }
    fn outcome(&self, attempt: Attempt) -> &std::io::Result<JobOutcome> {
        &self
            .done
            .iter()
            .find(|row| row.id.sequence() == attempt.sequence)
            .unwrap()
            .result
    }
}

#[test]
fn session_actor_if_requested() {
    let Some(world) = std::env::var_os(ACTOR) else {
        return;
    };
    let world = PathBuf::from(world);
    let machine = Fixed::new(concurrent_reading(256, 200, 128));
    let mut seat = cohort_seat(&world, &machine);
    seat.own = None;
    let _turn = crate::platform::FORWARDING_TURN.lock().unwrap();
    let mut d = Driver::new(&world);
    if std::env::var_os(FENCED).is_some() {
        fenced_jobs(&world, &seat, &mut d);
        std::fs::write(world.join("done"), b"fenced native session completed").unwrap();
        return;
    }
    if std::env::var_os(REAL).is_some() {
        real_builds(&world, &seat, &mut d);
        std::fs::write(world.join("done"), b"native session completed").unwrap();
        return;
    }

    // Neither dropping every external endpoint nor a bad read is execution policy.
    let first = world.join("first");
    let peer = world.join("peer");
    let (mut command, mut input) = host_table::worker(&first, false);
    let a = d
        .owner
        .submit(
            host_table::held(&first, &seat),
            &mut command,
            Duration::ZERO,
        )
        .unwrap();
    let (mut command, mut peer_input) = host_table::worker(&peer, false);
    let b = d
        .owner
        .submit(host_table::held(&peer, &seat), &mut command, Duration::ZERO)
        .unwrap();
    d.ready(&[&first, &peer]);
    let observer = d.owner.observer();
    let controller = d.owner.controller();
    let read = observer.read(None).unwrap();
    assert_eq!(read.snapshot.jobs.len(), 2);
    assert_eq!(
        Snapshot::decode(&read.snapshot.encode().unwrap()).unwrap(),
        *read.snapshot
    );
    assert_eq!(
        observer
            .read(Some(Cursor {
                generation: Generation([0; 16]),
                revision: 0
            }))
            .unwrap_err(),
        ReadError::StaleGeneration
    );
    assert_eq!(
        observer
            .read(Some(Cursor {
                generation: a.generation,
                revision: u64::MAX
            }))
            .unwrap_err(),
        ReadError::FutureCursor
    );
    assert!(
        controller
            .cancel(
                Attempt {
                    generation: Generation([0; 16]),
                    ..a
                },
                CancelRequest::Kill
            )
            .is_err()
    );
    drop(observer);
    drop(controller);
    d.tick();
    assert!(d.done.is_empty());
    input.write_all(b"finish\n").unwrap();
    d.until("first finishes after all clients disconnect", |d| {
        !d.done.is_empty()
    });
    assert!(d.outcome(a).as_ref().unwrap().succeeded());
    assert!(!d.owner.is_empty());

    let observer = d.owner.observer();
    let controller = d.owner.controller();
    controller.cancel(a, CancelRequest::Kill).unwrap();
    d.tick();
    assert!(
        observer
            .read(None)
            .unwrap()
            .events
            .iter()
            .any(|r| r.attempt == Some(a.sequence)
                && matches!(r.event, Event::Rejected(Rejection::StaleAttempt)))
    );
    assert_eq!(
        observer.read(None).unwrap().snapshot.jobs[0].attempt,
        b.sequence
    );

    // A trusted execution-policy error remains private; the public projection is closed.
    d.reject = Some(b);
    controller.cancel(b, CancelRequest::Terminate).unwrap();
    let before = observer.read(None).unwrap();
    assert!(
        !before
            .events
            .iter()
            .any(|r| r.attempt == Some(b.sequence) && matches!(r.event, Event::IntentDelivered(_)))
    );
    d.until("failed policy cleans only peer", |d| d.owner.is_empty());
    assert!(
        d.outcome(b)
            .as_ref()
            .unwrap_err()
            .to_string()
            .contains("PRIVATE_SECRET")
    );
    let after = observer.read(None).unwrap();
    assert!(after.events.iter().any(|r| r.attempt == Some(b.sequence)
        && matches!(r.event, Event::IntentDelivered(CancelRequest::Terminate))));
    assert!(
        after
            .events
            .iter()
            .any(|r| r.attempt == Some(b.sequence) && matches!(r.event, Event::Failed(_)))
    );
    assert!(!format!("{after:?}").contains("PRIVATE_SECRET"));
    assert!(
        !String::from_utf8(after.snapshot.encode().unwrap())
            .unwrap()
            .contains("PRIVATE_SECRET")
    );
    assert!(seat.ledger.as_ref().unwrap().list().unwrap().is_empty());
    // Keep writers alive to make accidental inbox-driven termination observable.
    let _ = peer_input.write_all(b"already reaped");
    let at = after.observed_at;
    drop(d);
    let closed = observer.read(None).unwrap();
    assert!(closed.snapshot.closed);
    assert_eq!(closed.observed_at, at);
    assert!(matches!(
        closed.events.last().unwrap().event,
        Event::OwnerClosed
    ));
    std::fs::write(world.join("done"), b"native session completed").unwrap();
}

fn real_builds(world: &Path, seat: &Seat<'_>, d: &mut Driver) {
    use aterm_capacity::{cohort_namespace::Namespace, leadership::Fence};
    let namespace = Namespace::resolve(seat.ledger.as_ref().unwrap().dir())
        .unwrap()
        .unwrap();
    d.owner = Owner::fenced(Fence::acquire(&namespace, Instant::now()).unwrap(), 2, 128).unwrap();
    struct Release(Vec<PathBuf>);
    impl Drop for Release {
        fn drop(&mut self) {
            for path in &self.0 {
                let _ = std::fs::write(path.join("release"), b"cleanup");
            }
        }
    }
    let mut release = Release(Vec::new());
    let mut attempts = Vec::new();
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
        attempts.push(
            d.owner
                .submit(host_table::held(&path, seat), &mut command, Duration::ZERO)
                .unwrap(),
        );
    }
    d.ready(&release.0.iter().map(PathBuf::as_path).collect::<Vec<_>>());
    let observer = d.owner.observer();
    assert_eq!(observer.read(None).unwrap().snapshot.jobs.len(), 2);
    drop(observer);
    drop(d.owner.controller());
    monitor_builds(world, d);
    let source = d.owner.monitor_source().unwrap();
    let (client, mut stalled) = source
        .pair(Instant::now() + aterm_spec::wait::MUST_HAPPEN)
        .unwrap();
    let mut slow = client.into_stream();
    slow.write_all(b"ATM").unwrap();
    assert_eq!(
        stalled.step().unwrap(),
        super::service::monitor::Progress::Pending
    );
    let (gone, mut disconnected) = source
        .pair(Instant::now() + aterm_spec::wait::MUST_HAPPEN)
        .unwrap();
    drop(gone);
    assert_eq!(
        disconnected.step().unwrap(),
        super::service::monitor::Progress::Complete
    );
    assert!(d.done.is_empty());
    for path in &release.0 {
        std::fs::write(path.join("release"), b"host accepts progress").unwrap();
    }
    d.until("two real targo builds complete without clients", |d| {
        d.owner.is_empty()
    });
    for (attempt, path) in attempts.iter().zip(&release.0) {
        assert!(d.outcome(*attempt).as_ref().unwrap().succeeded());
        let mut command = spawnfd::command(path.join("target.noindex/debug/headless_probe"));
        assert!(
            aterm_uds::deadline::output_within(&mut command, aterm_spec::wait::MUST_HAPPEN)
                .unwrap()
                .status
                .success()
        );
    }
    assert!(seat.ledger.as_ref().unwrap().list().unwrap().is_empty());
    assert_eq!(
        stalled.step().unwrap(),
        super::service::monitor::Progress::Pending
    );
    drop(slow);
    assert!(stalled.step().is_err());
}

fn monitor_builds(world: &Path, d: &mut Driver) {
    use super::service::monitor::Progress;
    use std::os::fd::OwnedFd;
    let source = d.owner.monitor_source().unwrap();
    let sibling = d.owner.monitor_source().unwrap();
    let deadline = Instant::now() + aterm_spec::wait::MUST_HAPPEN;
    let mut held = Vec::new();
    for _ in 0..16 {
        held.push(source.pair(deadline).unwrap());
    }
    assert!(matches!(sibling.pair(deadline), Err(e) if e.kind() == std::io::ErrorKind::WouldBlock));
    d.tick();
    assert!(d.done.is_empty());
    drop(held);
    let (client, mut server) = sibling.pair(deadline).unwrap();
    let generation = d.owner.observer().read(None).unwrap().snapshot.generation;
    std::fs::write(world.join("monitor.generation"), generation.0).unwrap();
    let mut command = spawnfd::command(std::env::current_exe().unwrap());
    command
        .args([
            "--exact",
            "build_admit::tests::host_service::monitor_reader_if_requested",
            "--test-threads=1",
        ])
        .env(MONITOR, world)
        .stdin(Stdio::from(OwnedFd::from(client.into_stream())))
        .stdout(Stdio::null())
        .stderr(Stdio::from(
            std::fs::File::create(world.join("monitor-reader.log")).unwrap(),
        ));
    struct Reader(spawnfd::Child);
    impl Drop for Reader {
        fn drop(&mut self) {
            let _ = self.0.kill();
            let _ = self.0.wait();
        }
    }
    let mut reader = Reader(command.spawn().unwrap());
    std::thread::scope(|scope| {
        // Only this independent I/O worker pumps monitor sockets. Native policy
        // never receives a client callback or network error.
        let io = scope.spawn(move || {
            while server.step().unwrap() == Progress::Pending {
                server.wait(server.deadline()).unwrap();
            }
        });
        aterm_spec::wait::until_within(
            aterm_spec::wait::MUST_HAPPEN,
            "delegated monitor reader snapshot",
            || {
                d.tick();
                assert!(d.done.is_empty());
                // The reader writes its reply before it exits, so an exit seen
                // first means the look after it finds the reply. Here only an
                // exit with no reply fails; a reader that replied and then
                // failed is judged by its exit status below.
                let exited = reader.0.try_wait().unwrap();
                if world.join("monitor.reply").exists() {
                    return Some(());
                }
                assert!(
                    exited.is_none(),
                    "reader failed: {}",
                    std::fs::read_to_string(world.join("monitor-reader.log")).unwrap_or_default()
                );
                None
            },
        );
        io.join().unwrap();
        aterm_spec::wait::until_within(
            aterm_spec::wait::MUST_HAPPEN,
            "monitor reader exit",
            || reader.0.try_wait().unwrap(),
        );
        assert!(reader.0.wait().unwrap().success());
    });
    let observed = Snapshot::decode(&std::fs::read(world.join("monitor.reply")).unwrap()).unwrap();
    assert_eq!(observed.generation, generation);
    assert_eq!(observed.jobs.len(), 2);
}

fn fenced_jobs(world: &Path, seat: &Seat<'_>, d: &mut Driver) {
    use aterm_capacity::{cohort_namespace::Namespace, leadership::Fence};
    use std::os::unix::fs::DirBuilderExt as _;
    let namespace = Namespace::resolve(seat.ledger.as_ref().unwrap().dir())
        .unwrap()
        .unwrap();
    let fence = Fence::acquire(&namespace, Instant::now()).unwrap();
    let generation = fence.generation();
    d.owner = Owner::fenced(fence, 2, 128).unwrap();
    let first = world.join("fenced");
    let (mut command, mut input) = host_table::worker(&first, false);
    let attempt = d
        .owner
        .submit(host_table::held(&first, seat), &mut command, Duration::ZERO)
        .unwrap();
    assert_eq!(attempt.generation, generation);
    d.ready(&[&first]);
    let observer = d.owner.observer();
    assert_eq!(observer.read(None).unwrap().snapshot.generation, generation);

    let foreign = world.join("foreign");
    std::fs::DirBuilder::new()
        .mode(0o700)
        .create(&foreign)
        .unwrap();
    let machine = Fixed::new(concurrent_reading(256, 200, 128));
    let mut other = cohort_seat(&foreign, &machine);
    other.own = None;
    let mut forbidden = spawnfd::command("/usr/bin/touch");
    forbidden.arg(world.join("forbidden"));
    assert!(
        d.owner
            .submit(
                host_table::held(&foreign, &other),
                &mut forbidden,
                Duration::ZERO
            )
            .is_err()
    );
    assert!(!world.join("forbidden").exists());
    assert_eq!(
        Fence::acquire(&namespace, Instant::now())
            .unwrap_err()
            .kind(),
        std::io::ErrorKind::WouldBlock
    );

    // The namespace birth manifest still names the removed original inode.
    // Neither this owner nor a reopened process may create a replacement lock.
    std::fs::remove_file(namespace.backend().join(".host-owner.lock")).unwrap();
    assert!(
        d.owner
            .submit(
                host_table::held(world, seat),
                &mut forbidden,
                Duration::ZERO
            )
            .is_err()
    );
    assert!(!world.join("forbidden").exists());
    assert!(Fence::acquire(&namespace, Instant::now()).is_err());
    d.tick();
    assert!(d.done.is_empty());
    let read = observer.read(None).unwrap();
    assert_eq!(read.snapshot.jobs.len(), 1);
    assert!(
        read.events
            .iter()
            .any(|row| matches!(row.event, Event::AdmissionRefused(_)))
    );
    input.write_all(b"finish after leadership loss\n").unwrap();
    d.until(
        "original job succeeds and drains despite lost leadership",
        |d| d.owner.is_empty(),
    );
    assert!(d.outcome(attempt).as_ref().unwrap().succeeded());
    assert!(seat.ledger.as_ref().unwrap().list().unwrap().is_empty());
}

fn run_actor(real: bool, fenced: bool) {
    let world = aterm_tempfile::tempdir().unwrap();
    // The parent outlives a timed-out actor. Its separately grouped builds must
    // never stay parked if actor Drop cannot run. The fixture also refuses when
    // this parent removes its world before a build script observes release.
    struct Release(PathBuf);
    impl Drop for Release {
        fn drop(&mut self) {
            for index in 0..2 {
                let _ = std::fs::write(
                    self.0.join(index.to_string()).join("release"),
                    b"parent cleanup",
                );
            }
        }
    }
    let _release = Release(world.path().to_path_buf());
    let mut command = spawnfd::command(std::env::current_exe().unwrap());
    command
        .args([
            "--exact",
            "build_admit::tests::host_service::session_actor_if_requested",
            "--test-threads=1",
            "--nocapture",
        ])
        .env(ACTOR, world.path());
    if real {
        command.env(REAL, "1");
    }
    if fenced {
        command.env(FENCED, "1");
    }
    let output = aterm_uds::deadline::output_within(&mut command, aterm_spec::wait::MUST_HAPPEN)
        .unwrap_or_else(|e| {
            panic!(
                "session actor {e:?}; {}",
                std::fs::read_to_string(world.path().join("phase")).unwrap_or_default()
            )
        });
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    assert!(
        world.path().join("done").is_file(),
        "exact actor must actually execute"
    );
}

#[test]
fn clients_disappear_stale_controls_refuse_and_private_policy_failure_stays_private() {
    run_actor(false, false);
}

#[test]
fn one_host_session_finishes_two_actual_targo_builds_after_clients_disconnect() {
    run_actor(true, false);
}

#[test]
fn fenced_owner_refuses_foreign_claims_and_lost_leadership_but_finishes_owned_work() {
    run_actor(false, true);
}
