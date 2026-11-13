// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

use super::*;
use crate::server::{Relauncher, ServeConfig, Server};
use crate::wire::Birth;
use std::os::unix::fs::MetadataExt as _;
use std::time::Instant;

const TRUSTD_SILENT: &str = "the process trustd gate was silent — the machine's, not the code's";

fn trustd_silent(why: &str) -> bool {
    why.starts_with("macOS's code-signing service (trustd) is not answering")
}

fn serve(sock: &Path) -> Server {
    Server::bind(ServeConfig {
        socket: sock.to_path_buf(),
        identity: Identity::same_uid(),
        relaunch: Relauncher::Unavailable,
        version: "test".into(),
        test_drop_custody: false,
    })
    .expect("bind")
}

fn step_until(server: &mut Server, what: &str, done: impl Fn(&Server) -> bool) {
    let deadline = Instant::now() + Duration::from_secs(60);
    while !done(server) {
        assert!(Instant::now() < deadline, "never: {what}");
        server.step(20).expect("step");
    }
}

fn register_op(path: &str, local_id: u64) -> (u64, LinkOp) {
    let master = std::fs::File::open(path).expect("open");
    let rdev = master.metadata().expect("stat").rdev();
    let op = LinkOp::Register {
        master: OwnedFd::from(master),
        header: MasterHeader {
            rdev,
            shell_pid: std::process::id(),
            shell_birth: Birth::default(),
            local_id,
        },
        tag: Vec::new(),
    };
    (rdev, op)
}

#[test]
fn a_listener_of_other_code_is_refused_as_not_this_build() {
    use std::os::unix::fs::PermissionsExt as _;
    aterm_uds::deadline::require_trust_service();
    let scratch = crate::test_scratch_dir("other");
    let dir = scratch.path();
    let own = Identity::for_self(crate::identity::IdentityPolicy::Designated).expect("own");

    let sock = dir.join("nc.sock");
    let mut nc = aterm_uds::spawnfd::command("/usr/bin/nc")
        .arg("-dlU")
        .arg(&sock)
        .stdin(aterm_uds::spawnfd::Stdio::null())
        .stdout(aterm_uds::spawnfd::Stdio::null())
        .stderr(aterm_uds::spawnfd::Stdio::null())
        .spawn()
        .expect("spawn nc");
    let started = Instant::now();
    let refused = loop {
        match KeeperClient::connect(&sock, &own, Duration::from_secs(60)) {
            Err(e)
                if matches!(
                    e.kind(),
                    io::ErrorKind::NotFound | io::ErrorKind::ConnectionRefused
                ) =>
            {
                assert!(started.elapsed().as_secs() < 60, "nc never listened: {e}");
                std::thread::sleep(Duration::from_millis(5));
            }
            other => break other,
        }
    };
    let _ = nc.kill();
    let _ = nc.wait();
    let e = refused.expect_err("nc is not this build's code");
    if trustd_silent(&e.to_string()) {
        aterm_spec::host::could_not_run(format_args!("{TRUSTD_SILENT}: {e}"));
    }
    assert_eq!(e.kind(), io::ErrorKind::PermissionDenied, "{e}");
    assert!(refused_identity(&e), "{e}");

    let absent = KeeperClient::connect(&dir.join("none.sock"), &own, Duration::from_secs(2))
        .expect_err("nothing there");
    assert!(!refused_identity(&absent), "{absent}");

    let closed = dir.join("closed.sock");
    let _listener = std::os::unix::net::UnixListener::bind(&closed).expect("bind");
    std::fs::set_permissions(&closed, std::fs::Permissions::from_mode(0o000)).expect("chmod");
    let denied = KeeperClient::connect(&closed, &own, Duration::from_secs(2))
        .expect_err("a socket this process may not open");
    assert_eq!(denied.kind(), io::ErrorKind::PermissionDenied, "{denied}");
    assert!(!refused_identity(&denied), "{denied}");
}

#[test]
fn a_keeper_at_the_uid_floor_is_refused_by_a_window_that_checks_code() {
    aterm_uds::deadline::require_trust_service();
    let scratch = crate::test_scratch_dir("floor");
    let dir = scratch.path();
    let own = || Identity::for_self(IdentityPolicy::Designated).expect("own");
    for (name, keeper, accepted) in [
        ("uid.sock", Identity::same_uid(), false),
        ("designated.sock", own(), true),
    ] {
        let sock = dir.join(name);
        let mut server = Server::bind(ServeConfig {
            socket: sock.clone(),
            identity: keeper,
            relaunch: Relauncher::Unavailable,
            version: "test".into(),
            test_drop_custody: false,
        })
        .expect("bind");
        let window = std::thread::spawn(move || {
            let id = own();
            let dial = || KeeperClient::connect(&sock, &id, Duration::from_secs(60));
            let link = dial()?.hello_link("t", None, false);
            let boot = dial()?.hello_app("t").map(|_| ());
            let stat = dial()?.status("t").map(|_| ());
            Ok::<_, io::Error>((link, boot, stat))
        });
        while !window.is_finished() {
            server.step(20).expect("step");
        }
        let keeper_gate_silent = server
            .test_unchecked_ever()
            .iter()
            .any(|why| trustd_silent(why));
        let joined = window.join().expect("the window thread");
        let silent = |e: &io::Error| trustd_silent(&e.to_string()) || keeper_gate_silent;
        if let Err(e) = &joined
            && silent(e)
        {
            aterm_spec::host::could_not_run(format_args!("{TRUSTD_SILENT}: {name}: {e}"));
        }
        let (link, boot, stat) = joined.expect("the keeper's code is this build's");
        for (what, got) in [("link", link), ("boot", boot), ("status", stat)] {
            if let Err(e) = &got
                && silent(e)
            {
                aterm_spec::host::could_not_run(format_args!(
                    "{TRUSTD_SILENT}: {what} HELLO to {name}: {e}"
                ));
            }
            if accepted {
                got.unwrap_or_else(|e| panic!("{what} HELLO to {name}: {e}"));
            } else {
                let e = got.expect_err(what);
                assert!(refused_identity(&e), "{what} HELLO to {name}: {e}");
                assert!(e.to_string().contains("--identity uid"), "{e}");
            }
        }
    }
}

#[test]
fn only_a_refused_check_is_not_this_build() {
    let refused = check_failed(CheckError::Refused("OSStatus -67050".into()));
    assert_eq!(refused.kind(), io::ErrorKind::PermissionDenied);
    assert!(refused_identity(&refused), "{refused}");
    let unchecked = check_failed(CheckError::Unchecked(
        "the kernel did not give the peer's audit token".into(),
    ));
    assert!(!refused_identity(&unchecked), "{unchecked}");
    assert_eq!(
        unchecked.to_string(),
        "the kernel did not give the peer's audit token"
    );
    let unanswered = check_failed(CheckError::Unanswered(
        "macOS's code-signing service (trustd) is not answering".into(),
    ));
    assert!(!refused_identity(&unanswered), "{unanswered}");
    assert_eq!(unanswered.kind(), io::ErrorKind::Other, "{unanswered}");
}

#[test]
fn a_restarted_keeper_is_told_again_without_a_new_op() {
    let scratch = crate::test_scratch_dir("restart");
    let sock = scratch.path().join("k.sock");
    let mut first = serve(&sock);
    let master = std::fs::File::open("/dev/null").expect("open");
    let rdev = master.metadata().expect("stat").rdev();
    let link = KeeperLink::start(sock.clone(), Identity::same_uid(), "t".into()).expect("link");
    assert!(link.enqueue(LinkOp::Register {
        master: OwnedFd::from(master),
        header: MasterHeader {
            rdev,
            shell_pid: std::process::id(),
            shell_birth: Birth::default(),
            local_id: 1,
        },
        tag: Vec::new(),
    }));
    step_until(&mut first, "the first keeper holds it", |s| s.holds(rdev));
    drop(first);
    let mut second = serve(&sock);
    step_until(&mut second, "the restarted keeper holds it", |s| {
        s.holds(rdev)
    });
    link.shutdown();
    drop(second);
}

#[test]
fn a_restarted_keeper_is_told_each_region_again() {
    let scratch = crate::test_scratch_dir("journal");
    let sock = scratch.path().join("k.sock");
    let mut first = serve(&sock);
    let (journaled, journaled_op) = register_op("/dev/null", 1);
    let (unsaid, unsaid_op) = register_op("/dev/zero", 2);
    let link = KeeperLink::start_marked(
        sock.clone(),
        Identity::same_uid(),
        "t".into(),
        None,
        None,
        true,
    )
    .expect("link");
    assert!(link.enqueue(journaled_op));
    assert!(link.enqueue(unsaid_op));
    let len = aterm_uds::shm::PAGE;
    #[expect(
        clippy::disallowed_methods,
        reason = "a test playing the window makes the region the keeper is sent"
    )]
    // SAFETY: this fixture's link and keeper only transfer/hold/query custody, never reopen, map, write or resize it; `_region` remains inert until it is dropped.
    let (_region, custody) = unsafe { aterm_uds::shm::Region::create(len) }.expect("a region");
    assert!(link.enqueue(LinkOp::Companion {
        rdev: journaled,
        region: custody,
        len: len as u64,
        receipt: None,
    }));
    let view = |s: &Server, rdev| s.core().companion_view(rdev);
    let region_held = |s: &Server| {
        s.holds(journaled)
            && view(s, journaled)
                .is_some_and(|v| v.offered.is_some() && v.reader_settled == Some(true))
    };
    step_until(&mut first, "the first keeper holds the region", region_held);
    drop(first);
    let mut second = serve(&sock);
    step_until(&mut second, "the restarted keeper holds it again", |s| {
        region_held(s) && s.holds(unsaid)
    });
    assert_eq!(
        view(&second, unsaid).map(|v| (v.offered, v.reader_settled)),
        Some((None, Some(false))),
        "a region not yet said is not told as none"
    );
    assert!(link.enqueue(LinkOp::CompanionRelease {
        rdev: journaled,
        receipt: None
    }));
    let released = |s: &Server| {
        s.holds(journaled)
            && view(s, journaled)
                .is_some_and(|v| v.offered.is_none() && v.reader_settled == Some(true))
    };
    step_until(&mut second, "the release reaches it", released);
    drop(second);
    let mut third = serve(&sock);
    step_until(
        &mut third,
        "a third keeper is told it is released",
        released,
    );
    link.shutdown();
    drop(third);
}

#[test]
fn a_replacement_companion_is_confirmed_only_once_the_keeper_has_read_it() {
    let scratch = crate::test_scratch_dir("receipt");
    let sock = scratch.path().join("k.sock");
    let mut server = serve(&sock);
    let (rdev, register) = register_op("/dev/null", 1);
    let link = KeeperLink::start_marked(
        sock.clone(),
        Identity::same_uid(),
        "t".into(),
        None,
        None,
        true,
    )
    .expect("link");
    assert!(link.enqueue(register));
    let len = aterm_uds::shm::PAGE;
    let region = || {
        #[expect(
            clippy::disallowed_methods,
            reason = "a test playing the window makes the regions the keeper is sent"
        )]
        // SAFETY: this fixture's link and keeper only transfer/hold/query custody, never reopen, map, write or resize it; the region remains inert until it is dropped.
        let made = unsafe { aterm_uds::shm::Region::create(len) }.expect("a region");
        made
    };
    let (_first, custody) = region();
    assert!(link.enqueue(LinkOp::Companion {
        rdev,
        region: custody,
        len: len as u64,
        receipt: None,
    }));
    let offered = |s: &Server| s.core().companion_view(rdev).and_then(|v| v.offered);
    step_until(&mut server, "the keeper holds the first region", |s| {
        s.holds(rdev) && offered(s).is_some()
    });
    let first = offered(&server);

    let (_fresh, custody) = region();
    let receipt = std::sync::Arc::new(CompanionReceipt::default());
    assert!(link.enqueue(LinkOp::Companion {
        rdev,
        region: custody,
        len: len as u64,
        receipt: Some(std::sync::Arc::clone(&receipt)),
    }));
    assert!(
        link.send_and_wait(
            LinkOp::Meta {
                rdev,
                meta: b"m".to_vec(),
            },
            Duration::from_secs(60),
        ),
        "control: the COMPANION, its SYNC and the META behind them are written"
    );
    assert!(
        !receipt.confirmed(),
        "a written COMPANION is not a read one"
    );
    assert_eq!(
        offered(&server),
        first,
        "premise: the keeper has read nothing since"
    );
    step_until(
        &mut server,
        "the keeper's read confirms the receipt",
        |_| receipt.confirmed(),
    );
    assert!(
        offered(&server).is_some_and(|id| Some(id) != first),
        "the keeper offers the fresh region"
    );
    link.shutdown();
    drop(server);
}

/// An idle wait no hang detector outlasts: a link idling this long wakes only for an op
/// queued or a frame from its keeper.
const IDLE_FOR_GOOD: Duration = Duration::from_secs(3_600);

#[test]
fn an_interrupted_doorbell_write_is_retried_before_the_worker_is_left_waiting() {
    use std::io::Read as _;

    let (bell, door) = doorbell().expect("doorbell");
    let mut writes = 0;
    bell.ring_with(|| {
        writes += 1;
        if writes == 1 {
            return Err(io::ErrorKind::Interrupted.into());
        }
        aterm_uds::fdpass::send_vectored(&bell.near, &[io::IoSlice::new(&[1])])
    });
    assert_eq!(writes, 2);
    let mut byte = [0];
    assert_eq!((&door.far).read(&mut byte).expect("the wake byte"), 1);
    assert_eq!(byte, [1]);
}

#[test]
fn a_full_doorbell_already_wakes_the_worker_and_coalesces_later_rings() {
    let (bell, mut door) = doorbell().expect("doorbell");
    let bytes = [1];
    loop {
        match aterm_uds::fdpass::send_vectored(&bell.near, &[io::IoSlice::new(&bytes)]) {
            Ok(n) => assert!(n > 0),
            Err(e) if e.kind() == io::ErrorKind::Interrupted => continue,
            Err(e) if e.kind() == io::ErrorKind::WouldBlock => break,
            Err(e) => panic!("fill the door: {e}"),
        }
    }
    let mut writes = 0;
    bell.ring_with(|| {
        writes += 1;
        let e = aterm_uds::fdpass::send_vectored(&bell.near, &[io::IoSlice::new(&[1])])
            .expect_err("the door is full");
        assert_eq!(e.kind(), io::ErrorKind::WouldBlock);
        Err(e)
    });
    assert_eq!(writes, 1);
    bell.ring_with(|| panic!("an unanswered ring is coalesced"));
    door.answer();
    // Once answered, the next queued op must be able to ring again.
    let mut writes = 0;
    bell.ring_with(|| {
        writes += 1;
        aterm_uds::fdpass::send_vectored(&bell.near, &[io::IoSlice::new(&[1])])
    });
    assert_eq!(writes, 1);
    use std::io::Read as _;
    let mut byte = [0];
    assert_eq!((&door.far).read(&mut byte).expect("a fresh wake byte"), 1);
    assert_eq!(byte, [1]);
}

#[test]
fn a_doorbell_write_that_writes_nothing_does_not_suppress_the_next_ring() {
    for failure in [Ok(0), Err(io::ErrorKind::BrokenPipe.into())] {
        let (bell, door) = doorbell().expect("doorbell");
        let mut failure = Some(failure);
        bell.ring_with(|| failure.take().expect("one refused write"));
        bell.ring();
        use std::io::Read as _;
        let mut byte = [0];
        assert_eq!((&door.far).read(&mut byte).expect("a later wake byte"), 1);
        assert_eq!(byte, [1]);
    }
}

/// A recovered window's fence lifts on the keeper's SYNC answer (K1): the worker reads it as
/// it arrives, not on its next op or its next idle wake, neither of which comes here.
#[test]
fn a_receipt_is_confirmed_as_the_keeper_answers_with_no_op_behind_it() {
    let scratch = crate::test_scratch_dir("answer");
    let sock = scratch.path().join("k.sock");
    let mut server = serve(&sock);
    let (rdev, register) = register_op("/dev/null", 1);
    let link = KeeperLink::start_idling(
        sock.clone(),
        Identity::same_uid(),
        "t".into(),
        None,
        None,
        true,
        IDLE_FOR_GOOD,
    )
    .expect("link");
    assert!(link.enqueue(register));
    let len = aterm_uds::shm::PAGE;
    #[expect(
        clippy::disallowed_methods,
        reason = "a test playing the window makes the region the keeper is sent"
    )]
    // SAFETY: this fixture's link and keeper only transfer/hold/query custody, never reopen, map, write or resize it; the region remains inert until it is dropped.
    let (_region, custody) = unsafe { aterm_uds::shm::Region::create(len) }.expect("a region");
    let receipt = std::sync::Arc::new(CompanionReceipt::default());
    assert!(link.enqueue(LinkOp::Companion {
        rdev,
        region: custody,
        len: len as u64,
        receipt: Some(std::sync::Arc::clone(&receipt)),
    }));
    step_until(
        &mut server,
        "the keeper's answer to the SYNC behind the COMPANION confirms the receipt",
        |_| receipt.confirmed(),
    );
    assert!(
        server
            .core()
            .companion_view(rdev)
            .is_some_and(|v| v.offered.is_some()),
        "the keeper holds the region it confirmed"
    );
    let (ended_tx, ended) = mpsc::sync_channel(1);
    std::thread::spawn(move || {
        link.shutdown();
        let _ = ended_tx.send(());
    });
    assert!(
        ended.recv_timeout(aterm_spec::wait::MUST_HAPPEN).is_ok(),
        "a link shut down ends without waiting out its idle"
    );
    drop(server);
}

/// A keeper that welcomes a link and then closes it is tried again on the link's next op or
/// its idle wait, never on the wake its close caused: a worker that dialled there would dial
/// a keeper dropping every link it welcomes as fast as it could, with no pause between.
#[test]
fn a_keeper_that_closes_the_link_is_not_dialled_again_on_its_close() {
    use crate::wire::Kind;
    use std::sync::atomic::{AtomicBool, Ordering};
    /// Read `stream`'s frames, welcoming its HELLO, up to and including one of kind `last`.
    fn kinds_until(stream: &std::os::unix::net::UnixStream, last: Kind) -> Vec<Kind> {
        stream
            .set_read_timeout(Some(aterm_spec::wait::MUST_HAPPEN))
            .expect("its hang detector");
        let mut kinds = Vec::new();
        loop {
            let frame = read_frame(stream).expect("read").expect("a frame").frame;
            kinds.push(frame.kind());
            if matches!(frame, Frame::Hello { .. }) {
                let welcome = Frame::Welcome {
                    proto: PROTO,
                    offers: 0,
                    version: "closing".into(),
                    identity: Some(IdentityPolicy::SameUid),
                    caps: Some(0),
                };
                write_frame(stream, &welcome, None).expect("welcome");
            }
            if frame.kind() == last {
                return kinds;
            }
        }
    }
    let scratch = crate::test_scratch_dir("closed");
    let sock = scratch.path().join("k.sock");
    let listener = std::os::unix::net::UnixListener::bind(&sock).expect("bind");
    let asked = std::sync::Arc::new(AtomicBool::new(false));
    let (closed_tx, closed) = mpsc::sync_channel(1);
    let keeper = {
        let asked = std::sync::Arc::clone(&asked);
        std::thread::spawn(move || {
            let (first, _) = aterm_uds::accept(
                &listener,
                aterm_uds::AcceptWait::Forever,
                aterm_uds::AcceptedMode::Blocking,
            )
            .expect("the first link");
            let first_kinds = kinds_until(&first, Kind::Register);
            drop(first);
            closed_tx.send(()).expect("the test waits for the close");
            let (second, _) = aterm_uds::accept(
                &listener,
                aterm_uds::AcceptWait::Forever,
                aterm_uds::AcceptedMode::Blocking,
            )
            .expect("the second link");
            let unasked = !asked.load(Ordering::SeqCst);
            (first_kinds, unasked, kinds_until(&second, Kind::Pending))
        })
    };
    let (_, register) = register_op("/dev/null", 1);
    let link = KeeperLink::start_idling(
        sock,
        Identity::same_uid(),
        "t".into(),
        None,
        None,
        false,
        IDLE_FOR_GOOD,
    )
    .expect("link");
    assert!(link.enqueue(register));
    closed
        .recv_timeout(aterm_spec::wait::MUST_HAPPEN)
        .expect("the keeper read the REGISTER and closed the link");
    // Neither an op nor the link's idle wait comes here, so a dial in this pause is one the
    // close's own wake made. Its length is only how sure the test is to see one.
    std::thread::sleep(LINK_POLL);
    asked.store(true, Ordering::SeqCst);
    assert!(
        link.send_and_wait(LinkOp::Pending { pid: 1 }, aterm_spec::wait::MUST_HAPPEN),
        "a waited op dials the keeper at once"
    );
    let (first, unasked, second) = keeper.join().expect("the closing keeper");
    assert_eq!(first, [Kind::Hello, Kind::Register]);
    assert!(
        !unasked,
        "the link dialled the keeper again on its close, with no op queued and its idle wait \
         not over"
    );
    assert_eq!(second, [Kind::Hello, Kind::Register, Kind::Pending]);
    link.shutdown();
}

#[test]
fn a_release_the_link_could_not_write_is_told_to_the_next_connection() {
    let scratch = crate::test_scratch_dir("rel");
    let dir = scratch.path();
    let sock = dir.join("k.sock");
    let aside = dir.join("k.aside");
    let mut server = serve(&sock);
    let (closed, closed_op) = register_op("/dev/null", 1);
    let (kept, kept_op) = register_op("/dev/zero", 2);
    assert_ne!(closed, kept);
    let link = KeeperLink::start(sock.clone(), Identity::same_uid(), "t".into()).expect("link");
    assert!(link.enqueue(closed_op));
    assert!(link.enqueue(kept_op));
    step_until(&mut server, "the keeper holds both", |s| {
        s.holds(closed) && s.holds(kept)
    });
    std::fs::rename(&sock, &aside).expect("move the keeper's socket aside");
    server.test_drop_connections();
    let written = link.send_and_wait(LinkOp::Release { rdev: closed }, Duration::from_secs(60));
    assert!(!written, "no keeper was reachable");
    std::fs::rename(&aside, &sock).expect("restore the keeper's socket");
    step_until(&mut server, "the RELEASE reaches the keeper", |s| {
        !s.holds(closed)
    });
    assert!(server.holds(kept), "the open tab stays in custody");
    link.shutdown();
    drop(server);
}

#[test]
fn a_release_the_keeper_dropped_unread_is_told_to_the_next_connection() {
    let scratch = crate::test_scratch_dir("unread");
    let sock = scratch.path().join("k.sock");
    let mut server = serve(&sock);
    let (closed, closed_op) = register_op("/dev/null", 1);
    let (kept, kept_op) = register_op("/dev/zero", 2);
    let link = KeeperLink::start(sock.clone(), Identity::same_uid(), "t".into()).expect("link");
    assert!(link.enqueue(closed_op));
    assert!(link.enqueue(kept_op));
    step_until(&mut server, "the keeper holds both", |s| {
        s.holds(closed) && s.holds(kept)
    });
    let written = link.send_and_wait(LinkOp::Release { rdev: closed }, Duration::from_secs(60));
    assert!(written, "the RELEASE was written");
    server.test_drop_connections();
    assert!(server.holds(closed), "the keeper never read the RELEASE");
    step_until(&mut server, "the RELEASE reaches the keeper", |s| {
        !s.holds(closed)
    });
    assert!(server.holds(kept), "the open tab stays in custody");
    link.shutdown();
    drop(server);
}

#[test]
fn a_welcome_later_than_a_frame_still_answers_the_hello() {
    #[derive(Clone, Copy, Debug)]
    enum Hello {
        App,
        Link,
        Status,
    }
    let frame = Duration::from_millis(100);
    let late = Duration::from_millis(400);
    let dials: Vec<(usize, Hello)> = [Hello::App, Hello::Link, Hello::Status]
        .into_iter()
        .enumerate()
        .collect();
    aterm_spec::check::each(&dials, |&(n, hello)| {
        let scratch = crate::test_scratch_dir(&format!("late-{n}"));
        let sock = scratch.path().join("k.sock");
        let listener = std::os::unix::net::UnixListener::bind(&sock).expect("bind");
        let keeper = std::thread::spawn(move || {
            let (stream, _) = aterm_uds::accept(
                &listener,
                aterm_uds::AcceptWait::Forever,
                aterm_uds::AcceptedMode::Blocking,
            )
            .expect("accept");
            stream
                .set_read_timeout(Some(Duration::from_secs(60)))
                .expect("timeout");
            let hello = read_frame(&stream).expect("read").expect("a frame").frame;
            assert!(matches!(hello, Frame::Hello { .. }), "{hello:?}");
            std::thread::sleep(late);
            let welcome = Frame::Welcome {
                proto: PROTO,
                offers: 0,
                version: "late".into(),
                identity: Some(IdentityPolicy::SameUid),
                caps: Some(0),
            };
            write_frame(&stream, &welcome, None).expect("welcome");
            if matches!(
                hello,
                Frame::Hello {
                    class: PeerClass::Status,
                    ..
                }
            ) {
                let ask = read_frame(&stream).expect("read").expect("a frame").frame;
                assert!(matches!(ask, Frame::Status { .. }), "{ask:?}");
                std::thread::sleep(late);
                let status = Frame::Status {
                    text: "late".into(),
                };
                write_frame(&stream, &status, None).expect("status");
            }
            stream
        });
        let client = KeeperClient::connect(&sock, &Identity::same_uid(), frame).expect("dial");
        match hello {
            Hello::App => {
                let offers = client
                    .hello_app_marked("t", None, true)
                    .expect("the late WELCOME is heard");
                assert!(offers.is_empty());
            }
            Hello::Link => client
                .hello_link("t", None, true)
                .expect("the late WELCOME is heard"),
            Hello::Status => assert_eq!(
                client
                    .status("t")
                    .expect("the late WELCOME and STATUS are heard"),
                "late"
            ),
        }
        assert_eq!(
            client.stream.read_timeout().expect("its wait"),
            Some(frame),
            "{hello:?}"
        );
        drop(keeper.join().expect("the late keeper"));
    });
}

#[test]
fn a_keeper_that_answers_no_sync_is_sent_none() {
    use crate::wire::Kind;
    let scratch = crate::test_scratch_dir("nosync");
    let sock = scratch.path().join("k.sock");
    let listener = std::os::unix::net::UnixListener::bind(&sock).expect("bind");
    let older = std::thread::spawn(move || {
        let (stream, _) = aterm_uds::accept(
            &listener,
            aterm_uds::AcceptWait::Forever,
            aterm_uds::AcceptedMode::Blocking,
        )
        .expect("accept");
        stream
            .set_read_timeout(Some(Duration::from_secs(60)))
            .expect("timeout");
        let mut kinds = Vec::new();
        loop {
            let frame = read_frame(&stream).expect("read").expect("a frame").frame;
            kinds.push(frame.kind());
            match frame {
                Frame::Hello { .. } => {
                    let welcome = Frame::Welcome {
                        proto: PROTO,
                        offers: 0,
                        version: "older".into(),
                        identity: Some(IdentityPolicy::SameUid),
                        caps: Some(CAP_JOURNAL),
                    };
                    write_frame(&stream, &welcome, None).expect("welcome");
                }
                Frame::Pending { .. } => return kinds,
                _ => {}
            }
        }
    });
    let (rdev, op) = register_op("/dev/null", 1);
    let link = KeeperLink::start(sock, Identity::same_uid(), "t".into()).expect("link");
    assert!(link.enqueue(op));
    let within = Duration::from_secs(60);
    assert!(link.send_and_wait(LinkOp::Release { rdev }, within));
    let retired = std::sync::Arc::new(ReadObserver::default());
    assert!(link.send_and_wait(
        LinkOp::Retire {
            rdev: rdev + 1,
            observer: std::sync::Arc::clone(&retired),
        },
        within
    ));
    assert!(
        retired.confirmed(),
        "to a keeper without CAP_SYNC a written RELEASE is done"
    );
    assert!(link.send_and_wait(LinkOp::Pending { pid: 1 }, within));
    let kinds = older.join().expect("the older keeper");
    assert_eq!(
        kinds,
        [
            Kind::Hello,
            Kind::Register,
            Kind::Release,
            Kind::Release,
            Kind::Pending
        ]
    );
    link.shutdown();
}

/// A link already speaking to `server`'s keeper (its REGISTER of the device at `own` held
/// there), and that master's rdev.
fn linked(server: &mut Server, sock: &Path, idle: Duration, own: &str) -> (KeeperLink, u64) {
    let (own, register) = register_op(own, 9);
    let link = KeeperLink::start_idling(
        sock.to_path_buf(),
        Identity::same_uid(),
        "t".into(),
        None,
        None,
        true,
        idle,
    )
    .expect("link");
    assert!(link.enqueue(register));
    step_until(server, "the link's keeper holds its master", |s| {
        s.holds(own)
    });
    (link, own)
}

/// A window's boot connection to `server`'s keeper: its HELLO answered, no offers.
fn booted(server: &mut Server, sock: &Path) -> KeeperClient {
    let sock = sock.to_path_buf();
    let hello = std::thread::spawn(move || {
        let c = KeeperClient::connect(&sock, &Identity::same_uid(), Duration::from_secs(60))
            .expect("dial");
        let offers = c.hello_app_marked("t", None, true).expect("the WELCOME");
        assert!(offers.is_empty(), "premise: nothing to offer");
        c
    });
    step_until(server, "the boot HELLO is answered", |_| {
        hello.is_finished()
    });
    hello.join().expect("the boot HELLO")
}

/// The window retires a journal its keeper still holds ([`LinkOp::Retire`]): the observer
/// hears of it once the keeper answers the SYNC behind the RELEASE, and not while the keeper
/// holds the connection unread past their write. The negative control's target: a link that
/// confirms on the RELEASE's write is caught here.
#[test]
fn a_retirement_is_confirmed_only_once_the_keeper_answers_the_sync_behind_its_release() {
    let scratch = crate::test_scratch_dir("retire");
    let sock = scratch.path().join("k.sock");
    let mut server = serve(&sock);
    let (older, _) = linked(&mut server, &sock, IDLE_FOR_GOOD, "/dev/zero");
    let (rdev, register) = register_op("/dev/null", 1);
    assert!(older.enqueue(register));
    step_until(&mut server, "the keeper holds the old master", |s| {
        s.holds(rdev)
    });
    let (link, _) = linked(&mut server, &sock, IDLE_FOR_GOOD, "/dev/random");
    let observer = std::sync::Arc::new(ReadObserver::default());
    assert!(
        link.send_and_wait(
            LinkOp::Retire {
                rdev,
                observer: std::sync::Arc::clone(&observer),
            },
            Duration::from_secs(60),
        ),
        "control: the RELEASE and its SYNC are written"
    );
    assert!(
        server.holds(rdev),
        "premise: the keeper has read nothing since"
    );
    assert!(
        !observer.confirmed(),
        "a written RELEASE is not a retirement the keeper confirmed"
    );
    step_until(&mut server, "the keeper's answer confirms it", |_| {
        observer.confirmed()
    });
    assert!(!server.holds(rdev), "the keeper closed its copy");
    let read = observer.confirmation().expect("its read");
    assert!(read.sync_sent <= read.answer_read);
    link.shutdown();
    older.shutdown();
}

/// A REFUSED that is no owed retirement's moves nothing owed: not a plain RELEASE (owed until
/// its own SYNC is answered), and not a retirement whose SYNC this connection never wrote or
/// had answered before the refusal was read.
#[test]
fn a_refusal_that_is_no_owed_retirement_s_leaves_what_is_owed() {
    let now = std::time::Instant::now();
    let observer = std::sync::Arc::new(ReadObserver::default());
    let mut owed = Owed::new();
    owed.insert(
        5,
        Owe {
            sync: Some((3, now)),
            retires: Vec::new(),
        },
    );
    owed.insert(
        6,
        Owe {
            sync: None,
            retires: vec![std::sync::Arc::clone(&observer)],
        },
    );
    owed.insert(
        7,
        Owe {
            sync: Some((2, now)),
            retires: vec![std::sync::Arc::clone(&observer)],
        },
    );
    refuse_retirement(&mut owed, 5, RefuseCode::NotClaimant, 0);
    refuse_retirement(&mut owed, 6, RefuseCode::NoSuchRecord, 0);
    refuse_retirement(&mut owed, 7, RefuseCode::NoSuchRecord, 2);
    assert_eq!(owed.keys().copied().collect::<Vec<_>>(), [5, 6, 7]);
    assert!(!observer.confirmed(), "no retirement was refused");
    refuse_retirement(&mut owed, 7, RefuseCode::NoSuchRecord, 1);
    assert_eq!(owed.keys().copied().collect::<Vec<_>>(), [5, 6]);
    assert!(
        observer.confirmed(),
        "control: the retirement's own refusal confirms it"
    );
}

/// A retirement's RELEASE refused for any reason but naming no record (or an earlier frame of
/// its rdev refused, which is taken as its) drops its observers unconfirmed, since the keeper
/// may still offer that journal, but stays owed as a plain RELEASE until its own SYNC is
/// answered: the refused frame may be the master's COMPANION, its RELEASE still unread.
#[test]
fn a_refused_retirement_stays_owed_as_a_plain_release() {
    let now = std::time::Instant::now();
    let observer = std::sync::Arc::new(ReadObserver::default());
    let mut owed = Owed::new();
    owed.insert(
        8,
        Owe {
            sync: Some((4, now)),
            retires: vec![std::sync::Arc::clone(&observer)],
        },
    );
    refuse_retirement(&mut owed, 8, RefuseCode::NotClaimant, 3);
    assert!(
        owed.get(&8).is_some_and(|owe| owe.retires.is_empty()),
        "owed still, as a plain RELEASE"
    );
    refuse_retirement(&mut owed, 8, RefuseCode::NoSuchRecord, 3);
    settle_owed(&mut owed, 3);
    assert!(owed.contains_key(&8), "its own SYNC is not yet answered");
    settle_owed(&mut owed, 4);
    assert!(owed.is_empty(), "its SYNC's answer settles it");
    assert!(
        !observer.confirmed(),
        "a refused retirement is never confirmed, not by a later refusal nor by its answer"
    );
}

/// A RELEASE the keeper refuses because it holds the master for another claim leaves that
/// journal offerable, so its retirement is never confirmed, though the SYNC behind it is
/// answered; a RELEASE naming no record confirms its retirement (the journal went with it).
#[test]
fn a_retirement_the_keeper_refuses_for_another_claim_is_not_confirmed() {
    let scratch = crate::test_scratch_dir("refuse");
    let sock = scratch.path().join("k.sock");
    let mut server = serve(&sock);
    let (link, _) = linked(&mut server, &sock, IDLE_FOR_GOOD, "/dev/zero");
    let (newer, _) = linked(&mut server, &sock, IDLE_FOR_GOOD, "/dev/random");
    let (claimed, register) = register_op("/dev/null", 1);
    assert!(newer.enqueue(register));
    step_until(&mut server, "a newer link claims the master", |s| {
        s.holds(claimed)
    });
    let held_elsewhere = std::sync::Arc::new(ReadObserver::default());
    let gone = std::sync::Arc::new(ReadObserver::default());
    for (rdev, observer) in [(claimed, &held_elsewhere), (claimed ^ 0x5a5a, &gone)] {
        assert!(link.enqueue(LinkOp::Retire {
            rdev,
            observer: std::sync::Arc::clone(observer),
        }));
    }
    step_until(&mut server, "the RELEASE of no record confirms", |_| {
        gone.confirmed()
    });
    assert!(
        !held_elsewhere.confirmed(),
        "the keeper keeps the master for the newer claim: its journal is still offerable"
    );
    assert!(server.holds(claimed), "the newer claim keeps its master");
    link.shutdown();
    newer.shutdown();
}

/// A retirement whose keeper ended before answering is told to the next keeper, which, holding
/// no record of the master, confirms it.
#[test]
fn a_retirement_unanswered_when_its_keeper_ended_is_told_to_the_next_one() {
    let scratch = crate::test_scratch_dir("reretire");
    let sock = scratch.path().join("k.sock");
    let mut first = serve(&sock);
    let (link, zero) = linked(&mut first, &sock, LINK_POLL, "/dev/zero");
    let observer = std::sync::Arc::new(ReadObserver::default());
    assert!(link.send_and_wait(
        LinkOp::Retire {
            rdev: zero ^ 0x5a5a,
            observer: std::sync::Arc::clone(&observer),
        },
        Duration::from_secs(60),
    ));
    drop(first);
    assert!(!observer.confirmed(), "the first keeper read nothing");
    let mut second = serve(&sock);
    step_until(&mut second, "the next keeper confirms it", |_| {
        observer.confirmed()
    });
    assert!(second.holds(zero), "and holds the link's master again");
    link.shutdown();
}

/// The boot connection's RELEASE of an offer the window refused ([`KeeperClient::retire`]) is
/// seen through by the link started on that connection, once the keeper answers its SYNC.
#[test]
fn a_boot_connection_s_retirement_is_seen_through_by_its_link() {
    let scratch = crate::test_scratch_dir("bootret");
    let sock = scratch.path().join("k.sock");
    let mut server = serve(&sock);
    let (older, _) = linked(&mut server, &sock, IDLE_FOR_GOOD, "/dev/zero");
    let (rdev, register) = register_op("/dev/null", 1);
    assert!(older.enqueue(register));
    step_until(&mut server, "the keeper holds the old master", |s| {
        s.holds(rdev)
    });
    let boot = booted(&mut server, &sock);
    let observer = std::sync::Arc::new(ReadObserver::default());
    boot.retire(rdev, std::sync::Arc::clone(&observer))
        .expect("written");
    let link = KeeperLink::start_idling(
        sock.clone(),
        Identity::same_uid(),
        "t".into(),
        None,
        Some(boot),
        true,
        IDLE_FOR_GOOD,
    )
    .expect("link");
    assert!(!observer.confirmed(), "the keeper has not read the RELEASE");
    step_until(&mut server, "the link reads the keeper's answer", |_| {
        observer.confirmed()
    });
    assert!(!server.holds(rdev), "the keeper closed its copy");
    link.shutdown();
    older.shutdown();
}

/// An adopted session with no journal tells the keeper so (COMPANION_RELEASE), and the
/// receipt it carries is confirmed only once the keeper has read it.
#[test]
fn a_companion_release_is_confirmed_only_once_the_keeper_has_read_it() {
    let scratch = crate::test_scratch_dir("relrcpt");
    let sock = scratch.path().join("k.sock");
    let mut server = serve(&sock);
    let (link, _) = linked(&mut server, &sock, IDLE_FOR_GOOD, "/dev/zero");
    let (rdev, register) = register_op("/dev/null", 1);
    assert!(link.enqueue(register));
    let observer = std::sync::Arc::new(ReadObserver::default());
    assert!(
        link.send_and_wait(
            LinkOp::CompanionRelease {
                rdev,
                receipt: Some(std::sync::Arc::new(
                    CompanionReceipt::reporting_confirmation(std::sync::Arc::clone(&observer))
                )),
            },
            Duration::from_secs(60),
        ),
        "control: the REGISTER and the fenced COMPANION_RELEASE are written"
    );
    assert!(
        !server.holds(rdev),
        "premise: the keeper has read nothing since"
    );
    assert!(
        !observer.confirmed(),
        "a written COMPANION_RELEASE is not a read one"
    );
    step_until(&mut server, "the keeper's read confirms it", |_| {
        observer.confirmed()
    });
    assert_eq!(
        server
            .core()
            .companion_view(rdev)
            .map(|v| (v.offered, v.reader_settled)),
        Some((None, Some(true))),
        "the keeper holds no region an offer of it could carry"
    );
    link.shutdown();
}

/// A journal region a window would send, and its read-only custody descriptor.
fn journal_region() -> (aterm_uds::shm::Region, OwnedFd) {
    #[expect(
        clippy::disallowed_methods,
        reason = "a test playing the window makes the region the keeper is sent"
    )]
    // SAFETY: this fixture's link and keeper only transfer/hold/query custody, never reopen, map, write or resize it; the returned region remains inert until it is dropped.
    let made = unsafe { aterm_uds::shm::Region::create(aterm_uds::shm::PAGE) };
    made.expect("a region")
}

/// A held master's journal frame that reports to a [`ReadObserver`], superseded before the
/// link read the keeper's answer to the SYNC behind it, hands that observer on to the frame
/// that superseded it. A tab's RELEASE: the keeper's read closes the record, and the old
/// journal with it. A receipt-less COMPANION_RELEASE (the journal stopped): its read leaves
/// the keeper no region an offer could carry. Every frame is written before the keeper reads
/// one, as when the keeper is slow or the link is between connections. Before, the observer
/// went with the superseded read, and a recovered window's fence stood for its run.
#[test]
fn a_journal_frame_superseded_before_its_answer_hands_its_observer_on() {
    let scratch = crate::test_scratch_dir("supersede");
    let sock = scratch.path().join("k.sock");
    let mut server = serve(&sock);
    let (link, _) = linked(&mut server, &sock, IDLE_FOR_GOOD, "/dev/zero");
    let within = Duration::from_secs(60);
    let reporting = |observer: &std::sync::Arc<ReadObserver>| {
        Some(std::sync::Arc::new(
            CompanionReceipt::reporting_confirmation(std::sync::Arc::clone(observer)),
        ))
    };
    for (case, journaled, stops) in [
        ("a session with no journal closed", false, false),
        ("a journaled session closed", true, false),
        ("a session's journal stopped", true, true),
    ] {
        let (rdev, register) = register_op("/dev/null", 1);
        let observer = std::sync::Arc::new(ReadObserver::default());
        let (_region, custody) = journal_region();
        let journal = if journaled {
            LinkOp::Companion {
                rdev,
                region: custody,
                len: aterm_uds::shm::PAGE as u64,
                receipt: reporting(&observer),
            }
        } else {
            LinkOp::CompanionRelease {
                rdev,
                receipt: reporting(&observer),
            }
        };
        let next = if stops {
            LinkOp::CompanionRelease {
                rdev,
                receipt: None,
            }
        } else {
            LinkOp::Release { rdev }
        };
        assert!(link.enqueue(register), "{case}");
        assert!(link.enqueue(journal), "{case}");
        assert!(
            link.send_and_wait(next, within),
            "{case}: control: every frame is written"
        );
        assert!(
            !server.holds(rdev),
            "{case}: premise: the keeper has read none of them"
        );
        assert!(!observer.confirmed(), "{case}: nothing unread is confirmed");
        step_until(&mut server, case, |_| observer.confirmed());
        if stops {
            assert_eq!(
                server
                    .core()
                    .companion_view(rdev)
                    .map(|v| (v.offered, v.reader_settled)),
                Some((None, Some(true))),
                "{case}: the keeper holds no region an offer of it could carry"
            );
            assert!(link.send_and_wait(LinkOp::Release { rdev }, within));
        }
        step_until(&mut server, "the record is closed", |s| !s.holds(rdev));
    }
    link.shutdown();
}

/// A REGISTER at the rdev of a retirement still owed confirms it at once: the kernel gives a
/// pty's device to another master only once every descriptor of the old one, the keeper's
/// custody copy among them, is closed, so the old record and its journal are gone. The keeper
/// has read nothing when the link takes the REGISTER, so nothing else could confirm it.
#[test]
fn a_register_at_a_retired_rdev_confirms_the_retirement() {
    let scratch = crate::test_scratch_dir("reuse");
    let sock = scratch.path().join("k.sock");
    let mut server = serve(&sock);
    let (link, _) = linked(&mut server, &sock, IDLE_FOR_GOOD, "/dev/zero");
    let (rdev, register) = register_op("/dev/null", 1);
    let observer = std::sync::Arc::new(ReadObserver::default());
    let within = Duration::from_secs(60);
    assert!(
        link.send_and_wait(
            LinkOp::Retire {
                rdev,
                observer: std::sync::Arc::clone(&observer),
            },
            within,
        ),
        "control: the RELEASE and its SYNC are written"
    );
    assert!(!observer.confirmed(), "premise: nothing has confirmed it");
    assert!(
        link.send_and_wait(register, within),
        "the REGISTER at its rdev is written"
    );
    assert!(
        !server.holds(rdev),
        "premise: the keeper has still read nothing"
    );
    let read = observer
        .confirmation()
        .expect("the REGISTER at its rdev confirms the retirement");
    assert!(read.sync_sent <= read.answer_read);
    step_until(&mut server, "the new master's REGISTER is read", |s| {
        s.holds(rdev)
    });
    link.shutdown();
}

#[test]
fn pending_and_bye_are_acknowledged_once_written() {
    let scratch = crate::test_scratch_dir("ack");
    let sock = scratch.path().join("k.sock");
    let mut server = serve(&sock);
    let master = std::fs::File::open("/dev/null").expect("open");
    let rdev = master.metadata().expect("stat").rdev();
    let link = KeeperLink::start(sock.clone(), Identity::same_uid(), "t".into()).expect("link");
    assert!(link.enqueue(LinkOp::Register {
        master: OwnedFd::from(master),
        header: MasterHeader {
            rdev,
            shell_pid: std::process::id(),
            shell_birth: Birth::default(),
            local_id: 1,
        },
        tag: Vec::new(),
    }));
    let waiter = std::thread::spawn(move || {
        let pending = link.send_and_wait(LinkOp::Pending { pid: 1 }, Duration::from_secs(60));
        let bye = link.send_and_wait(LinkOp::Bye { keep: Vec::new() }, Duration::from_secs(60));
        let after = link.send_and_wait(LinkOp::Release { rdev }, Duration::from_millis(200));
        (pending, bye, after)
    });
    step_until(&mut server, "the keeper read the BYE", |s| {
        s.core().peers().values().any(|p| p.bye)
    });
    let (pending, bye, after) = waiter.join().expect("waiter");
    assert!(pending && bye, "written: pending={pending} bye={bye}");
    assert!(!after, "no op is written after the BYE");
    assert_eq!(
        server
            .core()
            .pending_successors()
            .iter()
            .map(|p| p.pid)
            .collect::<Vec<_>>(),
        [1],
        "the PENDING arrived before the BYE"
    );
    assert!(server.holds(rdev), "the RELEASE after the BYE never came");
    drop(server);
}

/// A lasting wait that runs out leaves its op queued, to be written in its turn; an ordinary
/// waited op whose wait ran out is never written.
#[test]
fn a_lasting_waited_op_outlives_its_wait() {
    let (link, far) = KeeperLink::stalled();
    assert!(
        !link.send_lasting_and_wait(LinkOp::Release { rdev: 7 }, Duration::ZERO),
        "no keeper wrote it"
    );
    assert!(!link.send_and_wait(LinkOp::Release { rdev: 8 }, Duration::from_millis(1)));
    let queued: Vec<u64> = far
        .drain()
        .into_iter()
        .filter_map(|op| match op {
            LinkOp::Release { rdev } => Some(rdev),
            _ => None,
        })
        .collect();
    assert_eq!(
        queued,
        [7],
        "the lasting op stays queued; the ordinary one went with its wait"
    );
}

/// The link says its keeper keeps only once its connection reached one whose WELCOME stated
/// [`CAP_KEEPS`], and that a BYE would keep a master only once that connection carried its
/// REGISTER with no refusal read since ([`KeeperLink::keeps`]); its BYE names the masters to
/// keep to that keeper, which records them beside the quit.
///
/// NEGATIVE CONTROL: before any op the worker has dialled nobody, so the link says no keeper
/// keeps; a master it never registered, and one whose REGISTER the keeper refused (its rdev
/// not the descriptor's), it says the keeper would not keep, though that keeper keeps.
#[test]
fn a_bye_names_what_it_keeps_to_a_keeper_that_keeps() {
    let scratch = crate::test_scratch_dir("keeps");
    let sock = scratch.path().join("k.sock");
    let mut server = serve(&sock);
    let master = std::fs::File::open("/dev/null").expect("open");
    let rdev = master.metadata().expect("stat").rdev();
    let forged = std::fs::File::open("/dev/null").expect("open");
    let link = KeeperLink::start(sock.clone(), Identity::same_uid(), "t".into()).expect("link");
    assert!(
        !link.keeper_keeps(),
        "no connection yet, so no keeper keeps"
    );
    let register = |file: std::fs::File, rdev, local_id| LinkOp::Register {
        master: OwnedFd::from(file),
        header: MasterHeader {
            rdev,
            shell_pid: std::process::id(),
            shell_birth: Birth::default(),
            local_id,
        },
        tag: Vec::new(),
    };
    let (good, bad) = (register(master, rdev, 1), register(forged, rdev + 1, 2));
    let waiter = std::thread::spawn(move || {
        let registered = link.send_and_wait(good, Duration::from_secs(60))
            && link.send_and_wait(bad, Duration::from_secs(60));
        let (keeps, holds) = (link.keeper_keeps(), link.keeps(rdev));
        (link, registered, keeps, holds)
    });
    step_until(&mut server, "both REGISTERs written", |s| {
        s.core().records().contains_key(&rdev) && waiter.is_finished()
    });
    let (link, registered, keeps, holds) = waiter.join().expect("waiter");
    assert!(registered, "both REGISTERs written");
    assert!(keeps, "this keeper's WELCOME stated it keeps");
    assert!(
        holds,
        "a BYE now keeps the master its connection registered"
    );
    assert!(
        !link.keeps(rdev + 2),
        "CONTROL: a master the link never registered"
    );
    let deadline = Instant::now() + Duration::from_secs(60);
    while link.keeps(rdev + 1) {
        assert!(
            Instant::now() < deadline,
            "CONTROL: the refused REGISTER's master never left the custody the link says"
        );
        server.step(20).expect("step");
    }
    let waiter = std::thread::spawn(move || {
        link.send_and_wait(LinkOp::Bye { keep: vec![rdev] }, Duration::from_secs(60))
    });
    step_until(&mut server, "the keeper read the BYE", |s| {
        s.core().peers().values().any(|p| p.bye)
    });
    assert!(waiter.join().expect("waiter"), "the BYE written");
    assert!(
        server
            .core()
            .peers()
            .values()
            .any(|p| p.bye && p.keeps == [rdev]),
        "the BYE's keep set reached the keeper"
    );
    drop(server);
}

#[test]
fn a_waited_op_with_no_keeper_is_refused_promptly() {
    let scratch = crate::test_scratch_dir("none");
    let sock = scratch.path().join("absent.sock");
    let link = KeeperLink::start(sock, Identity::same_uid(), "t".into()).expect("link");
    let master = std::fs::File::open("/dev/null").expect("open");
    let rdev = master.metadata().expect("stat").rdev();
    assert!(link.enqueue(LinkOp::Register {
        master: OwnedFd::from(master),
        header: MasterHeader {
            rdev,
            shell_pid: std::process::id(),
            shell_birth: Birth::default(),
            local_id: 1,
        },
        tag: Vec::new(),
    }));
    let started = Instant::now();
    assert!(!link.send_and_wait(LinkOp::Pending { pid: 1 }, Duration::from_secs(300)));
    assert!(
        started.elapsed() < aterm_spec::wait::MUST_HAPPEN,
        "refused without waiting out the bound: {:?}",
        started.elapsed()
    );
    link.shutdown();
}

#[test]
fn a_refused_companion_does_not_poison_a_later_valid_companion_on_the_same_link() {
    let scratch = crate::test_scratch_dir("rc");
    let sock = scratch.path().join("k.sock");
    let mut server = serve(&sock);
    let (rdev, register) = register_op("/dev/null", 1);
    let link = KeeperLink::start_marked(sock, Identity::same_uid(), "t".into(), None, None, true)
        .expect("link");
    assert!(link.enqueue(register));
    let len = aterm_uds::shm::PAGE;
    let bad_fd = OwnedFd::from(std::fs::File::open("/dev/null").expect("/dev/null"));
    let refused_receipt = std::sync::Arc::new(CompanionReceipt::default());
    assert!(link.enqueue(LinkOp::Companion {
        rdev,
        region: bad_fd,
        len: len as u64,
        receipt: Some(std::sync::Arc::clone(&refused_receipt)),
    }));
    #[expect(
        clippy::disallowed_methods,
        reason = "a test playing the window makes the region the keeper is sent"
    )]
    // SAFETY: this fixture's link and keeper only transfer/hold/query custody, never reopen, map, write or resize it; `_good` remains inert until it is dropped.
    let (_good, good_custody) = unsafe { aterm_uds::shm::Region::create(len) }.expect("a region");
    let accepted_receipt = std::sync::Arc::new(CompanionReceipt::default());
    assert!(link.enqueue(LinkOp::Companion {
        rdev,
        region: good_custody,
        len: len as u64,
        receipt: Some(std::sync::Arc::clone(&accepted_receipt)),
    }));
    step_until(
        &mut server,
        "the valid replacement companion after a refusal is confirmed",
        |_| accepted_receipt.confirmed(),
    );
    assert!(
        !refused_receipt.confirmed(),
        "the refused companion's receipt is never confirmed"
    );
    assert!(
        server
            .core()
            .companion_view(rdev)
            .is_some_and(|v| v.offered.is_some()),
        "the keeper holds the accepted replacement region"
    );
    link.shutdown();
    drop(server);
}
