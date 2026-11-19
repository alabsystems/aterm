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

#[path = "build_admit_service_lifetime_tests.rs"]
mod lifetime;

const ACTOR: &str = "ATPKG_NATIVE_SESSION_WORLD";
const REAL: &str = "ATPKG_NATIVE_SESSION_REAL";
const FENCED: &str = "ATPKG_NATIVE_SESSION_FENCED";
const MONITOR: &str = "ATPKG_NATIVE_SESSION_MONITOR";
/// How long the driver waits on the owner's wake path between two ticks.
const WAKE: Duration = Duration::from_millis(50);

#[test]
fn monitor_reader_if_requested() {
    use super::service::monitor::Client;
    use std::{io::Read as _, os::fd::AsFd as _};
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
    // This bootstrap is private parent delegation, not a monitor endpoint. Prove
    // execution BEFORE the parent mints the production capability/deadline.
    let mut bootstrap = aterm_uds::CtlStream::from(
        aterm_uds::kept_out(|| std::io::stdin().as_fd().try_clone_to_owned()).unwrap(),
    );
    bootstrap.set_nonblocking(false).unwrap();
    bootstrap.write_all(b"B").unwrap();
    bootstrap.set_nonblocking(true).unwrap();
    let stream = loop {
        // The outer owned actor/peer capture supplies the progress hang detector.
        // Waiting here holds no descriptor-creation lock or protocol deadline.
        aterm_uds::ready::any(&[bootstrap.as_fd()], None).unwrap();
        let received = aterm_uds::kept_out(|| -> std::io::Result<aterm_uds::CtlStream> {
            let mut marker = [0];
            let mut received = aterm_uds::fdpass::recv_with_fds(&bootstrap, &mut marker, 1)?;
            if (received.bytes, marker, received.fds.len()) != (1, *b"M", 1) {
                return Err(std::io::Error::new(
                    std::io::ErrorKind::InvalidData,
                    "invalid delegated monitor descriptor",
                ));
            }
            Ok(aterm_uds::CtlStream::from(received.fds.pop().unwrap()))
        });
        match received {
            Ok(stream) => break stream,
            Err(error)
                if matches!(
                    error.kind(),
                    std::io::ErrorKind::WouldBlock | std::io::ErrorKind::Interrupted
                ) => {}
            Err(error) => panic!("delegated monitor descriptor: {error}"),
        }
    };
    bootstrap.set_nonblocking(false).unwrap();
    let mut transferred_generation = [0; 16];
    bootstrap.read_exact(&mut transferred_generation).unwrap();
    assert_eq!(
        transferred_generation, generation.0,
        "the trusted owner generation"
    );
    // This actual fresh FD is the authority. The generation bytes alone confer none.
    let client = Client::from_trusted_stream(stream, generation).unwrap();
    bootstrap.write_all(b"A").unwrap();
    let mut retired = [0];
    bootstrap.read_exact(&mut retired).unwrap();
    assert_eq!(
        retired, *b"D",
        "sender retired its accepted capability copy"
    );
    assert_eq!(
        bootstrap.read(&mut retired).unwrap(),
        0,
        "bootstrap closes after the ownership handoff"
    );
    drop(bootstrap);
    let reply = client
        .read_until(Instant::now() + aterm_spec::wait::MUST_HAPPEN)
        .unwrap();
    assert_eq!(reply.snapshot.generation, generation);
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

// An undrained fixture keeps its actual native owner instead of entering Jobs' blocking Drop.
struct FixtureOwner(Option<Owner>);

impl std::ops::Deref for FixtureOwner {
    type Target = Owner;
    fn deref(&self) -> &Owner {
        self.0.as_ref().expect("the fixture owns its service")
    }
}

impl std::ops::DerefMut for FixtureOwner {
    fn deref_mut(&mut self) -> &mut Owner {
        self.0.as_mut().expect("the fixture owns its service")
    }
}

impl FixtureOwner {
    fn replace(&mut self, owner: Owner) {
        assert!(self.is_empty(), "only an empty fixture owner is replaced");
        self.0 = Some(owner);
    }
}

struct TrackedJob {
    attempt: Attempt,
    path: PathBuf,
    root: Option<u32>,
}

struct Driver {
    owner: FixtureOwner,
    roots: Vec<TrackedJob>,
    done: Vec<super::host::Completion>,
    reject: Option<Attempt>,
    trace: PathBuf,
}

impl Driver {
    fn new(world: &Path) -> Self {
        Self {
            owner: FixtureOwner(Some(Owner::new(2, 128).unwrap())),
            roots: Vec::new(),
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
    fn submit(
        &mut self,
        path: &Path,
        held: Held,
        command: &mut spawnfd::Command,
        grace: Duration,
    ) -> Attempt {
        let (started, root) = std::sync::mpsc::channel();
        HEADLESS_ROOT_STARTED.with(|slot| {
            assert!(
                slot.borrow_mut().replace(started).is_none(),
                "one native root observer"
            );
        });
        let submitted = self.owner.submit(held, command, grace);
        HEADLESS_ROOT_STARTED.with(|slot| {
            slot.borrow_mut().take();
        });
        let attempt = submitted.expect("the native service accepted its owned job");
        self.roots.push(TrackedJob {
            attempt,
            path: path.to_owned(),
            root: None,
        });
        let root = root
            .recv_timeout(aterm_spec::wait::MUST_HAPPEN)
            .expect("the service publishes its actual owned native root");
        self.roots.last_mut().unwrap().root = Some(root);
        attempt
    }
    fn root(&self, attempt: Attempt) -> u32 {
        self.roots
            .iter()
            .find(|job| job.attempt == attempt)
            .and_then(|job| job.root)
            .expect("the attempt's actual owned root was published")
    }
    /// Tick until found while this attempt's actual root makes progress.
    /// An ended root still reaps and settles through the owner's wake path.
    fn until(&mut self, attempt: Attempt, why: &str, found: impl Fn(&Self) -> bool) {
        let root = self.root(attempt);
        match aterm_spec::wait::within_while_working(
            root,
            aterm_spec::wait::MUST_HAPPEN,
            aterm_spec::wait::WORKING_GUARD,
            WAKE,
            || {
                self.tick();
                if found(self) {
                    return Some(());
                }
                assert!(!self.owner.is_empty(), "{why}: {:?}", self.done);
                self.owner.wait(Instant::now() + WAKE).unwrap();
                None
            },
        ) {
            Ok(()) => {}
            Err(aterm_spec::wait::Unanswered::Ended) => {
                // A root can disappear before the owner reaps and settles its cohort.
                aterm_spec::wait::until_within(aterm_spec::wait::MUST_HAPPEN, why, || {
                    self.tick();
                    if found(self) {
                        return Some(());
                    }
                    assert!(!self.owner.is_empty(), "{why}: {:?}", self.done);
                    self.owner.wait(Instant::now() + WAKE).unwrap();
                    None
                });
            }
            Err(why_ended) => panic!("native session hang: {why}: {why_ended:?}: {:?}", self.done),
        }
    }
    fn ready(&mut self, paths: &[&Path]) {
        for path in paths {
            let attempt = self
                .roots
                .iter()
                .find(|job| job.path.as_path() == *path)
                .map(|job| job.attempt)
                .expect("readiness belongs to one submitted native job");
            let root = self.root(attempt);
            let found = || {
                self.tick();
                assert!(
                    self.done.is_empty(),
                    "premature completion: {:?}",
                    self.done
                );
                path.join("ready").exists().then_some(())
            };
            match std::env::var_os(lifetime::PEER_PID) {
                Some(pid) => lifetime::until_ready(
                    pid.to_str().unwrap().parse().expect("owned peer PID"),
                    found,
                ),
                None => {
                    aterm_spec::wait::until_while_working(root, "session fixture readiness", found)
                }
            }
        }
    }
    fn keep_cleanup(&self, why: &str) {
        if let Some(world) = self.trace.parent() {
            let _ = std::fs::write(world.join("owner-cleanup"), why);
            let _ = writeln!(std::io::stderr(), "KEPT {}: {why}", world.display());
        }
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

impl Drop for Driver {
    fn drop(&mut self) {
        let Some(mut owner) = self.owner.0.take() else {
            return;
        };
        if owner.is_empty() {
            drop(owner);
            return;
        }
        let unwinding = std::thread::panicking();
        self.keep_cleanup("the service fixture was dropped with owned jobs");
        if let Some(world) = self.trace.parent() {
            for index in 0..2 {
                let _ = std::fs::write(
                    world.join(index.to_string()).join("release"),
                    b"fixture cleanup",
                );
            }
        }
        let controller = owner.controller();
        for job in &self.roots {
            if !self
                .done
                .iter()
                .any(|row| row.id.sequence() == job.attempt.sequence)
            {
                let _ = controller.cancel(job.attempt, CancelRequest::Terminate);
            }
        }
        let deadline = Instant::now() + aterm_spec::wait::MUST_HAPPEN;
        while !owner.is_empty() && Instant::now() < deadline {
            match owner.tick(&mut |_, _| Ok(JobControl::Wait), &mut |_| {}) {
                Ok(done) => self.done.extend(done),
                Err(error) => {
                    self.keep_cleanup(&format!("the owned service cleanup failed: {error}"));
                    break;
                }
            }
            if !owner.is_empty() {
                std::thread::sleep(Duration::from_millis(10));
            }
        }
        if !owner.is_empty() {
            self.keep_cleanup("the service did not drain before its cleanup hang detector; native custody remains retained");
            std::mem::forget(owner);
        } else {
            if !self.done.iter().all(|row| {
                row.result.as_ref().is_ok_and(|outcome| {
                    outcome.cohort_settlement() == Some(CohortSettlement::Drained)
                })
            }) {
                self.keep_cleanup(
                    "a service cleanup completion has no positive drained-cohort outcome",
                );
            }
            drop(owner);
        }
        if !unwinding {
            panic!("the service fixture was dropped before its owned jobs completed");
        }
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
    let a = d.submit(
        &first,
        host_table::held(&first, &seat),
        &mut command,
        Duration::ZERO,
    );
    let (mut command, mut peer_input) = host_table::worker(&peer, false);
    let b = d.submit(
        &peer,
        host_table::held(&peer, &seat),
        &mut command,
        Duration::ZERO,
    );
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
    d.until(a, "first finishes after all clients disconnect", |d| {
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
    d.until(b, "failed policy cleans only peer", |d| d.owner.is_empty());
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
    d.owner.replace(
        Owner::fenced(Fence::acquire(&namespace, Instant::now()).unwrap(), 2, 128).unwrap(),
    );
    let socket = PathBuf::from(std::env::var_os(lifetime::SOCKET).unwrap());
    let mut paths = Vec::new();
    let mut attempts = Vec::new();
    for index in 0..2 {
        let path = world.join(index.to_string());
        std::fs::create_dir(&path).unwrap();
        headless::prepare_build(&path);
        paths.push(path.clone());
        let mut command = spawnfd::transparent(std::env::var_os("CARGO").unwrap()).into_command();
        command
            .args(["--unverified", "build", "--offline", "--target-dir"])
            .arg(path.join("target.noindex"))
            .args([
                "--config",
                &format!("build.build-dir='{}'", path.join("build.noindex").display()),
            ])
            .env("ATPKG_HEADLESS_REAL_BUILDS", &path)
            .env(lifetime::SOCKET, &socket)
            .env("CARGO_BUILD_JOBS", "1")
            .current_dir(&path)
            .stdout(Stdio::null())
            .stderr(Stdio::from(
                std::fs::File::create(path.join("compiler.log")).unwrap(),
            ));
        attempts.push(d.submit(
            &path,
            host_table::held(&path, seat),
            &mut command,
            Duration::ZERO,
        ));
    }
    d.ready(&paths.iter().map(PathBuf::as_path).collect::<Vec<_>>());
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
    aterm_uds::deadline::once_released(|| match disconnected.step()? {
        super::service::monitor::Progress::Complete => Ok(()),
        super::service::monitor::Progress::Pending => Err(std::io::ErrorKind::WouldBlock.into()),
    });
    // Close/truncate this partial request while both native scripts are still
    // held. A later product timeout must not impersonate the disconnect oracle.
    drop(slow);
    let truncated = stalled
        .step()
        .expect_err("partial monitor request cannot complete");
    assert_eq!(truncated.kind(), std::io::ErrorKind::InvalidData);
    assert!(d.done.is_empty());
    // Both already accepted scripts must still answer after the independent
    // observer/control clients disconnect; stalled monitor I/O is no release either.
    lifetime::disconnect(&socket);
    lifetime::request(&socket, b'P');
    if std::env::var_os(lifetime::AFTER_OWNER_DEATH).is_some() {
        std::fs::write(world.join("owner-death-ready"), b"both barriers still held").unwrap();
    } else {
        lifetime::request(&socket, b'R');
    }
    for attempt in &attempts {
        d.until(
            *attempt,
            "a real targo build completes without clients",
            |d| {
                d.done
                    .iter()
                    .any(|row| row.id.sequence() == attempt.sequence)
            },
        );
    }
    assert!(
        d.owner.is_empty(),
        "both actual builds completed without clients"
    );
    for (attempt, path) in attempts.iter().zip(&paths) {
        assert!(d.outcome(*attempt).as_ref().unwrap().succeeded());
        let mut command = spawnfd::command(path.join("target.noindex/debug/headless_probe"));
        let output = aterm_uds::deadline::output_while_process_progressing(
            &mut command,
            aterm_spec::wait::MUST_HAPPEN,
            "host service compiled probe",
        )
        .unwrap();
        assert!(output.status.success());
        assert_eq!(output.stdout, b"host service probe executed\n");
    }
    assert!(seat.ledger.as_ref().unwrap().list().unwrap().is_empty());
}

fn monitor_builds(world: &Path, d: &mut Driver) {
    use super::service::monitor::Progress;
    use std::{
        io::Read as _,
        os::fd::{AsFd as _, OwnedFd},
    };
    let source = d.owner.monitor_source().unwrap();
    let sibling = d.owner.monitor_source().unwrap();
    // Source::new captures this Owner's generation, and every pair uses it
    // unchanged. Check it again after minting the actual delegated capability.
    let generation = d.owner.observer().read(None).unwrap().snapshot.generation;
    std::fs::write(world.join("monitor.generation"), generation.0).unwrap();
    let (mut bootstrap, child_bootstrap) = aterm_uds::kept_out(aterm_uds::CtlStream::pair).unwrap();
    bootstrap.set_nonblocking(true).unwrap();
    let mut command = spawnfd::command(std::env::current_exe().unwrap());
    command
        .args([
            "--exact",
            "build_admit::tests::host_service::monitor_reader_if_requested",
            "--test-threads=1",
            "--nocapture",
        ])
        .env(MONITOR, world)
        .stdin(Stdio::from(OwnedFd::from(child_bootstrap)))
        .stdout(Stdio::from(
            std::fs::File::create(world.join("monitor-reader.stdout")).unwrap(),
        ))
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
    // Release the command's original bootstrap/stdio descriptions immediately;
    // only the running reader owns its explicitly delegated stdin endpoint.
    drop(command);
    aterm_spec::wait::until_while_working(
        reader.0.id(),
        "monitor reader executed before capability admission",
        || {
            let mut marker = [0];
            match bootstrap.read(&mut marker) {
                Ok(1) => {
                    assert_eq!(marker, *b"B", "actual bootstrap readiness");
                    Some(())
                }
                Ok(0) => panic!("monitor reader closed its bootstrap before readiness"),
                Ok(_) => unreachable!("one-byte readiness buffer"),
                Err(error) if error.kind() == std::io::ErrorKind::WouldBlock => {
                    assert!(
                        reader.0.try_wait().unwrap().is_none(),
                        "reader ended before readiness"
                    );
                    None
                }
                Err(error) if error.kind() == std::io::ErrorKind::Interrupted => None,
                Err(error) => panic!("monitor bootstrap readiness: {error}"),
            }
        },
    );
    bootstrap.set_nonblocking(false).unwrap();

    // Actual reader startup/first exec is outside every product exchange budget.
    let deadline = Instant::now() + aterm_spec::wait::MUST_HAPPEN;
    let mut held = Vec::new();
    for _ in 0..16 {
        held.push(source.pair(deadline).unwrap());
    }
    assert!(matches!(sibling.pair(deadline), Err(e) if e.kind() == std::io::ErrorKind::WouldBlock));
    d.tick();
    assert!(d.done.is_empty());
    drop(held);
    let (client, mut server) = sibling
        .pair(Instant::now() + aterm_spec::wait::MUST_HAPPEN)
        .unwrap();
    assert_eq!(
        d.owner.observer().read(None).unwrap().snapshot.generation,
        generation
    );
    let delegated = client.into_stream();
    let mut message = [0; 17];
    message[0] = b'M';
    message[1..].copy_from_slice(&generation.0);
    std::thread::scope(|scope| {
        // Only this independent I/O worker pumps monitor sockets. Native policy
        // never receives a client callback or network error.
        let io = scope.spawn(move || {
            while server.step().unwrap() == Progress::Pending {
                server.wait(server.deadline()).unwrap();
            }
        });
        assert_eq!(
            aterm_uds::fdpass::send_with_fds(&bootstrap, &message, &[delegated.as_fd()]).unwrap(),
            message.len(),
            "one complete capability/generation message",
        );
        bootstrap.set_nonblocking(true).unwrap();
        aterm_spec::wait::until_while_working(
            reader.0.id(),
            "monitor reader accepted its actual capability",
            || {
                d.tick();
                assert!(d.done.is_empty());
                let mut accepted = [0];
                match bootstrap.read(&mut accepted) {
                    Ok(1) => {
                        assert_eq!(accepted, *b"A", "actual capability acceptance");
                        Some(())
                    }
                    Ok(0) => panic!("monitor reader closed before accepting its capability"),
                    Ok(_) => unreachable!("one-byte acceptance buffer"),
                    Err(error) if error.kind() == std::io::ErrorKind::WouldBlock => {
                        assert!(
                            reader.0.try_wait().unwrap().is_none(),
                            "reader ended before capability acceptance"
                        );
                        None
                    }
                    Err(error) if error.kind() == std::io::ErrorKind::Interrupted => None,
                    Err(error) => panic!("monitor capability acceptance: {error}"),
                }
            },
        );
        drop(delegated);
        bootstrap.set_nonblocking(false).unwrap();
        bootstrap.write_all(b"D").unwrap();
        drop(bootstrap);
        aterm_spec::wait::until_while_working(
            reader.0.id(),
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
                    "reader ended before its reply: {exited:?}; stdout: {}; stderr: {}",
                    std::fs::read_to_string(world.join("monitor-reader.stdout"))
                        .unwrap_or_default(),
                    std::fs::read_to_string(world.join("monitor-reader.log")).unwrap_or_default()
                );
                None
            },
        );
        io.join().unwrap();
        aterm_spec::wait::until_while_working(reader.0.id(), "monitor reader exit", || {
            reader.0.try_wait().unwrap()
        });
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
    d.owner.replace(Owner::fenced(fence, 2, 128).unwrap());
    let first = world.join("fenced");
    let (mut command, mut input) = host_table::worker(&first, false);
    let attempt = d.submit(
        &first,
        host_table::held(&first, seat),
        &mut command,
        Duration::ZERO,
    );
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
        attempt,
        "original job succeeds and drains despite lost leadership",
        |d| d.owner.is_empty(),
    );
    assert!(d.outcome(attempt).as_ref().unwrap().succeeded());
    assert!(seat.ledger.as_ref().unwrap().list().unwrap().is_empty());
}

fn run_actor(real: bool, fenced: bool) {
    let world = lifetime::World::new();
    let socket = world.path().join("owner");
    // A peer process makes server work observable independently of this
    // libtest process's own polling and concurrently running cases.
    let owner = real.then(|| lifetime::Peer::start(world.path(), &socket));
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
        command
            .env(REAL, "1")
            .env(lifetime::SOCKET, &socket)
            .env(lifetime::PEER_PID, owner.as_ref().unwrap().id().to_string());
    }
    if fenced {
        command.env(FENCED, "1");
    }
    let output = if let Some(owner) = &owner {
        lifetime::capture(&mut command, || Some(owner.id()))
    } else {
        actor_output(&mut command, "the private native service actor finishes").unwrap_or_else(
            |e| {
                panic!(
                    "session actor {e:?}; {}",
                    std::fs::read_to_string(world.path().join("phase")).unwrap_or_default()
                )
            },
        )
    };
    lifetime::record_actor_output(world.path(), &output);
    if let Some(owner) = owner {
        owner.finish();
    }
    assert!(
        !world.path().join("owner-cleanup").exists(),
        "the native fixture required fallback owner cleanup"
    );
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

/// The observer owns both subprocess handles, but never a server endpoint. Killing
/// the socket owner skips all of its Drop guards; EOF alone must let the native
/// actor finish/reap both separately grouped real Targo builds.
#[test]
fn outer_owner_sigkill_releases_two_actual_targo_build_scripts() {
    let world = lifetime::World::new();
    let socket = world.path().join("owner");
    let mut owner = lifetime::Peer::start(world.path(), &socket);
    let mut command = spawnfd::command(std::env::current_exe().unwrap());
    command
        .args([
            "--exact",
            "build_admit::tests::host_service::session_actor_if_requested",
            "--test-threads=1",
            "--nocapture",
        ])
        .env(ACTOR, world.path())
        .env(REAL, "1")
        .env(lifetime::SOCKET, &socket)
        .env(lifetime::PEER_PID, owner.id().to_string())
        .env(lifetime::AFTER_OWNER_DEATH, "1");
    let output = lifetime::capture(&mut command, || {
        owner.poll_kill(world.path().join("owner-death-ready").is_file())
    });
    lifetime::record_actor_output(world.path(), &output);
    // Never wait for P inside A's prompt collector callbacks.
    owner.finish_kill();
    owner.assert_health();
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    assert!(
        world.path().join("done").is_file(),
        "exact native actor executed through completion"
    );
}
