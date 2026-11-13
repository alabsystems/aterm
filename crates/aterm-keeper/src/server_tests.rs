// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

use super::*;
use crate::core::RecordState;
use crate::wire::{CAP_SENDS_BYE, PeerClass};

#[test]
fn a_lab_desks_marker_is_read_as_any_marker_is() {
    use std::os::unix::ffi::OsStrExt as _;
    let scratch = crate::test_scratch_dir("lab-marker");
    let dir = scratch.path();
    let pid = 4242;
    let m = MarkerRef {
        nanos: 7,
        dir: dir.as_os_str().as_bytes().to_vec(),
    };
    assert_eq!(read_marker(pid, &m), MarkerEvidence::Released, "none yet");
    let marker = dir.join(format!("crash-marker-{pid}-7-lab.log"));
    std::fs::write(&marker, b"").expect("a dead lab marker");
    assert_eq!(read_marker(pid, &m), MarkerEvidence::Dead);
    let held = std::fs::File::open(&marker).expect("open the marker");
    #[expect(
        clippy::disallowed_methods,
        reason = "the owner lock a live lab desk holds, let go when the file drops below"
    )]
    held.try_lock().expect("the owner lock");
    assert_eq!(read_marker(pid, &m), MarkerEvidence::Held);
    drop(held);
    let seen = dir.join(format!("crash-marker-{pid}-7-lab.log.seen"));
    std::fs::rename(&marker, &seen).expect("consumed");
    assert_eq!(read_marker(pid, &m), MarkerEvidence::Dead, "a consumed one");

    // NEGATIVE CONTROL: `.`, `..`, and embedded NULs in `marker.dir` return `Unknown` rather
    // than traversing into `dir`.
    for bad_dir in [
        format!(
            "{}/../{}",
            dir.display(),
            dir.file_name().unwrap().to_string_lossy()
        )
        .into_bytes(),
        format!("{}/.", dir.display()).into_bytes(),
        {
            let mut nul = dir.as_os_str().as_bytes().to_vec();
            nul.push(0);
            nul
        },
    ] {
        let bad = MarkerRef {
            nanos: 7,
            dir: bad_dir,
        };
        assert_eq!(read_marker(pid, &bad), MarkerEvidence::Unknown);
    }
}

#[test]
fn a_status_limit_inside_a_multibyte_character_keeps_valid_utf8() {
    let scratch = crate::test_scratch_dir("status-utf8");
    let mut server = Server::bind(ServeConfig {
        socket: scratch.path().join("k.sock"),
        identity: Identity::same_uid(),
        relaunch: Relauncher::Unavailable,
        version: "test".into(),
        test_drop_custody: false,
    })
    .expect("bind");
    let prefix = format!("keeper=running\npid={} version=", server.own_pid);
    server.version = format!(
        "{}é",
        "x".repeat(crate::wire::MAX_STATUS - prefix.len() - 1)
    );
    let text = server.status_text();
    assert_eq!(text.len(), crate::wire::MAX_STATUS - 1);
    assert!(text.starts_with(&prefix));
    assert!(Frame::Status { text }.encode().is_ok());
    drop(server);
}

#[test]
fn a_failed_write_ends_the_writing_never_the_reading() {
    let scratch = crate::test_scratch_dir("unwritable");
    let mut server = Server::bind(ServeConfig {
        socket: scratch.path().join("k.sock"),
        identity: Identity::same_uid(),
        relaunch: Relauncher::Unavailable,
        version: "test".into(),
        test_drop_custody: false,
    })
    .expect("bind");
    let client = CtlStream::connect(server.socket()).expect("connect");
    server.step(0).expect("turn");
    assert_eq!(server.conns.len(), 1, "accepted");
    let null = std::fs::File::open("/dev/null").expect("a character device");
    let rdev = null.metadata().expect("stat").rdev();
    let me = std::process::id();
    let frames = [
        Frame::Hello {
            proto: PROTO,
            class: PeerClass::App,
            caps: CAP_SENDS_BYE,
            build: "test".into(),
            marker: None,
        },
        Frame::Register {
            header: MasterHeader {
                rdev,
                shell_pid: me,
                shell_birth: born(me).expect("our birth"),
                local_id: 1,
            },
            tag: b"sid=s-unit".to_vec(),
        },
        Frame::Bye { keep: Vec::new() },
    ];
    for frame in &frames {
        let fd = matches!(frame, Frame::Register { .. }).then(|| null.as_fd());
        write_frame(&client, frame, fd).expect("write");
    }
    drop(client);
    let fd = server
        .conns
        .values()
        .next()
        .expect("the connection")
        .stream
        .as_raw_fd();
    let started = Instant::now();
    loop {
        let mut pfd = [PollFd {
            fd,
            events: POLLIN,
            revents: 0,
        }];
        let _ = sys::poll(&mut pfd, 0);
        if pfd[0].revents & POLLHUP != 0 {
            break;
        }
        assert!(started.elapsed() < Duration::from_secs(60), "never hung up");
        std::thread::sleep(Duration::from_millis(1));
    }
    for _ in 0..4 {
        server.step(0).expect("turn");
    }
    assert_eq!(server.replies_lost, 1, "the WELCOME was not written");
    let (conn, peer) = server
        .core()
        .peers()
        .iter()
        .next()
        .expect("the peer, alive, is remembered");
    assert!(peer.bye, "the BYE behind the unwritable WELCOME was read");
    let rec = server
        .core()
        .records()
        .get(&rdev)
        .expect("the claim behind it was read");
    assert_eq!(rec.claimants, vec![*conn]);
    assert!(server.holds(rdev), "and its descriptor kept");
    assert!(server.conns.is_empty(), "the EOF dropped the connection");
    drop(server);
}

/// A turn whose poll found a connection readable handles every frame already waiting on it,
/// in order: a window's burst of SYNCs is answered in that one turn, not one a turn, each turn
/// a poll over every connection and exit watch (`DRAIN_MAX_FRAMES`). Negative control: a
/// turn taking one frame answers the HELLO and none of the SYNCs behind it.
#[test]
fn a_turn_answers_every_frame_already_waiting_on_a_connection() {
    let scratch = crate::test_scratch_dir("burst");
    let mut server = Server::bind(ServeConfig {
        socket: scratch.path().join("k.sock"),
        identity: Identity::same_uid(),
        relaunch: Relauncher::Unavailable,
        version: "test".into(),
        test_drop_custody: false,
    })
    .expect("bind");
    let client = CtlStream::connect(server.socket()).expect("connect");
    server.step(0).expect("turn");
    assert_eq!(server.conns.len(), 1, "accepted");
    const SYNCS: u64 = 96;
    write_frame(&client, &hello_app(), None).expect("write the HELLO");
    for seq in 1..=SYNCS {
        write_frame(&client, &Frame::Sync { seq }, None).expect("write a SYNC");
    }
    let fd = server
        .conns
        .values()
        .next()
        .expect("the connection")
        .stream
        .as_raw_fd();
    let started = Instant::now();
    loop {
        let mut pfd = [PollFd {
            fd,
            events: POLLIN,
            revents: 0,
        }];
        let _ = sys::poll(&mut pfd, 0);
        if pfd[0].revents & POLLIN != 0 {
            break;
        }
        assert!(
            started.elapsed() < Duration::from_secs(60),
            "never readable"
        );
        std::thread::sleep(Duration::from_millis(1));
    }
    server.step(0).expect("the one turn");
    let mut answered = Vec::new();
    let mut welcomed = false;
    loop {
        let mut pfd = [PollFd {
            fd: client.as_raw_fd(),
            events: POLLIN,
            revents: 0,
        }];
        let _ = sys::poll(&mut pfd, 0);
        if pfd[0].revents & POLLIN == 0 {
            break;
        }
        match read_frame(&client).expect("a frame").expect("open").frame {
            Frame::Welcome { .. } => welcomed = true,
            Frame::Sync { seq } => answered.push(seq),
            other => panic!("an unexpected frame: {other:?}"),
        }
    }
    assert!(welcomed, "the HELLO was answered");
    assert_eq!(
        answered,
        (1..=SYNCS).collect::<Vec<_>>(),
        "every SYNC waiting behind it was answered in the same turn, in order"
    );
    drop(server);
}

fn a_pty_master() -> (OwnedFd, Rdev) {
    use std::os::fd::FromRawFd as _;
    let (master, slave) = aterm_pty::open_pty_pair().expect("a fresh PTY pair");
    let (master, slave) =
        // SAFETY: `master` and `slave` are fresh descriptors owned exclusively here.
        unsafe { (OwnedFd::from_raw_fd(master), OwnedFd::from_raw_fd(slave)) };
    drop(slave);
    let file = std::fs::File::from(master);
    let rdev = file.metadata().expect("fstat").rdev();
    (OwnedFd::from(file), rdev)
}

fn hello_app() -> Frame {
    Frame::Hello {
        proto: PROTO,
        class: PeerClass::App,
        caps: CAP_SENDS_BYE,
        build: "test".into(),
        marker: None,
    }
}

fn claim(rdev: Rdev, local_id: u64) -> Frame {
    let me = std::process::id();
    Frame::Register {
        header: MasterHeader {
            rdev,
            shell_pid: me,
            shell_birth: born(me).expect("our birth"),
            local_id,
        },
        tag: b"sid=s-unit".to_vec(),
    }
}

fn until_its_eof(server: &mut Server) {
    let started = Instant::now();
    while !server.conns.is_empty() {
        assert!(started.elapsed() < HANG, "no EOF");
        server.step(10).expect("turn");
    }
}

const HANG: Duration = Duration::from_secs(60);

struct OfferSeen {
    /// The frames it read.
    read: Vec<(Frame, Option<Rdev>)>,
    /// The master window A left orphaned.
    orphan_rdev: Rdev,
    bye_read: bool,
    claim_read: bool,
    /// The orphan is Orphaned again after B's EOF.
    returned_at_eof: bool,
    replies_lost: u64,
    dropped_at_eof: bool,
}

fn offer_to_a_second_window(fail_dup: bool) -> OfferSeen {
    let me = std::process::id();
    let scratch = crate::test_scratch_dir(&format!("undup-{}", u8::from(fail_dup)));
    let mut server = Server::bind(ServeConfig {
        socket: scratch.path().join("k.sock"),
        identity: Identity::same_uid(),
        relaunch: Relauncher::Unavailable,
        version: "test".into(),
        test_drop_custody: false,
    })
    .expect("bind");

    let (orphan, orphan_rdev) = a_pty_master();
    let a = CtlStream::connect(server.socket()).expect("connect A");
    server.step(0).expect("accept A");
    write_frame(&a, &hello_app(), None).expect("hello A");
    write_frame(&a, &claim(orphan_rdev, 1), Some(orphan.as_fd())).expect("claim A");
    for _ in 0..2 {
        server.step(0).expect("turn");
    }
    drop(a);
    until_its_eof(&mut server);
    let now = server.now();
    let mut env = server.env();
    let outs = server.core.peer_exit(me, 9, now, &mut env);
    server.apply(outs);
    let started = Instant::now();
    while server.core().records()[&orphan_rdev].state != RecordState::Orphaned {
        assert!(started.elapsed() < HANG, "A's master never orphaned");
        server.step(10).expect("turn");
    }

    let started = Instant::now();
    let (b, b_id, own_rdev) = loop {
        assert!(started.elapsed() < HANG, "the orphan was never offered");
        let b = CtlStream::connect(server.socket()).expect("connect B");
        b.set_read_timeout(Some(HANG)).expect("B's hang detector");
        server.step(0).expect("accept B");
        let b_id = *server.conns.keys().next_back().expect("B accepted");
        let (own, own_rdev) = a_pty_master();
        write_frame(&b, &hello_app(), None).expect("hello B");
        write_frame(&b, &claim(own_rdev, 1), Some(own.as_fd())).expect("claim B");
        write_frame(&b, &Frame::Bye { keep: Vec::new() }, None).expect("bye B");
        server.test_fail_next_dup = fail_dup;
        for _ in 0..3 {
            server.step(0).expect("turn");
        }
        let offered = server
            .core()
            .records()
            .get(&orphan_rdev)
            .is_some_and(|r| r.state == RecordState::Offered { to: b_id });
        if offered {
            break (b, b_id, own_rdev);
        }
        server.test_fail_next_dup = false;
        drop(b);
        until_its_eof(&mut server);
    };
    assert!(!server.test_fail_next_dup, "the OFFER's duplicate was made");
    let mut read = Vec::new();
    while read.len() < 2 {
        match read_frame(&b).expect("B reads") {
            Some(Incoming { frame, fd }) => {
                let rdev = fd.map(|fd| std::fs::File::from(fd).metadata().expect("fstat").rdev());
                read.push((frame, rdev));
            }
            None => break,
        }
    }
    let peer = server.core().peers().get(&b_id);
    let seen = OfferSeen {
        read,
        orphan_rdev,
        bye_read: peer.is_some_and(|p| p.bye),
        claim_read: server
            .core()
            .records()
            .get(&own_rdev)
            .is_some_and(|r| r.claimants == vec![b_id]),
        returned_at_eof: false,
        replies_lost: server.replies_lost,
        dropped_at_eof: false,
    };
    let open_before_eof = server.conns.contains_key(&b_id);
    drop(b);
    until_its_eof(&mut server);
    let seen = OfferSeen {
        dropped_at_eof: open_before_eof,
        returned_at_eof: server
            .core()
            .records()
            .get(&orphan_rdev)
            .is_some_and(|r| r.state == RecordState::Orphaned),
        ..seen
    };
    drop(server);
    seen
}

#[test]
fn an_offer_that_cannot_be_duplicated_ends_the_writing_never_the_reading() {
    let failed = offer_to_a_second_window(true);
    assert!(
        failed.claim_read,
        "the claim behind the unwritten OFFER was read"
    );
    assert!(
        failed.bye_read,
        "the BYE behind the unwritten OFFER was read"
    );
    assert!(failed.dropped_at_eof, "the connection lived until its EOF");
    assert!(
        failed.returned_at_eof,
        "the offer returns to Orphaned at B's EOF, never left with a dead link"
    );
    assert_eq!(failed.replies_lost, 1, "the OFFER was not written");
    assert!(
        matches!(
            failed.read.as_slice(),
            [(Frame::Welcome { offers: 1, .. }, None)]
        ),
        "B read the WELCOME, then EOF: {:?}",
        failed.read
    );

    let control = offer_to_a_second_window(false);
    assert!(control.claim_read && control.bye_read && control.dropped_at_eof);
    assert_eq!(control.replies_lost, 0);
    assert!(
        matches!(
            control.read.as_slice(),
            [
                (Frame::Welcome { offers: 1, .. }, None),
                (Frame::Offer { .. }, Some(r))
            ] if *r == control.orphan_rdev
        ),
        "the control's OFFER carries a descriptor: {:?}",
        control.read
    );
}

#[test]
fn a_companion_must_be_a_read_only_region_of_its_claimed_length() {
    use crate::wire::COMPANION_JOURNAL as JOURNAL;
    use aterm_uds::shm::{MAX_LEN, PAGE, Region, testing};
    let len = 2 * PAGE;
    #[expect(
        clippy::disallowed_methods,
        reason = "a test playing the window makes the region the keeper is sent"
    )]
    // SAFETY: `companion_fault` only inspects descriptor metadata; this custody stays local,
    // and neither it nor an alias modifies or resizes the object.
    let (_region, custody) = unsafe { Region::create(len) }.expect("a region");
    let ro = custody.as_fd();
    assert_eq!(
        companion_fault(ro, JOURNAL, len as u64),
        None,
        "the real thing"
    );

    #[expect(
        clippy::disallowed_methods,
        reason = "a test playing the window makes the region the keeper is sent"
    )]
    let (_twin, writable) =
        // SAFETY: `companion_fault` only inspects metadata and rejects writable custody; this
        // private descriptor is never written, resized or mapped.
        unsafe { testing::create_with_writable_custody(len) }.expect("a twin");
    let why = companion_fault(writable.as_fd(), JOURNAL, len as u64).expect("refused");
    assert!(why.contains("open for writing"), "{why}");
    let why = companion_fault(ro, JOURNAL, 3 * PAGE as u64).expect("refused");
    assert!(why.contains("not the"), "{why}");
    let why = companion_fault(ro, 2, len as u64).expect("refused");
    assert!(why.contains("no journal"), "{why}");
    for bad in [0, PAGE as u64 + 1, (MAX_LEN + PAGE) as u64] {
        let why = companion_fault(ro, JOURNAL, bad).expect("refused");
        assert!(why.contains("pages"), "{bad}: {why}");
    }
    let null = std::fs::File::open("/dev/null").expect("/dev/null");
    let why = companion_fault(null.as_fd(), JOURNAL, len as u64).expect("refused");
    assert!(why.contains("not POSIX shared memory"), "{why}");
}

#[test]
fn the_loop_holds_exactly_the_regions_the_core_keeps() {
    use aterm_uds::shm::{PAGE, Region};
    let len = 7 * PAGE;
    let scratch = crate::test_scratch_dir("regions");
    let mut server = Server::bind(ServeConfig {
        socket: scratch.path().join("k.sock"),
        identity: Identity::same_uid(),
        relaunch: Relauncher::Unavailable,
        version: "test".into(),
        test_drop_custody: false,
    })
    .expect("bind");
    let client = CtlStream::connect(server.socket()).expect("connect");
    server.step(0).expect("the accept");
    let null = std::fs::File::open("/dev/null").expect("a character device");
    let rdev = null.metadata().expect("stat").rdev();
    let me = std::process::id();
    let held_here = || {
        aterm_uds::holders::pshm_sizes_of(me)
            .expect("our own table")
            .into_iter()
            .filter(|s| *s == len as u64)
            .count()
    };
    let in_step = |server: &Server| {
        let loop_holds: std::collections::BTreeSet<CompanionId> =
            server.companions.keys().copied().collect();
        assert_eq!(loop_holds, server.core().companions_held());
        loop_holds.len()
    };
    let send = |server: &mut Server, frame: Frame, fd: Option<std::os::fd::BorrowedFd<'_>>| {
        write_frame(&client, &frame, fd).expect("write");
        server.step(0).expect("turn");
    };
    let hello = Frame::Hello {
        proto: PROTO,
        class: PeerClass::App,
        caps: CAP_SENDS_BYE | CAP_JOURNAL,
        build: "test".into(),
        marker: None,
    };
    send(&mut server, hello, None);
    let register = Frame::Register {
        header: MasterHeader {
            rdev,
            shell_pid: me,
            shell_birth: born(me).expect("our birth"),
            local_id: 1,
        },
        tag: b"sid=s-unit".to_vec(),
    };
    send(&mut server, register, Some(null.as_fd()));
    let companion = Frame::Companion {
        rdev,
        kind: COMPANION_JOURNAL,
        len: len as u64,
    };
    for taken in 1..=2u64 {
        #[expect(
            clippy::disallowed_methods,
            reason = "a test playing the window makes the region the keeper is sent"
        )]
        // SAFETY: the fixture server only queries, holds, and closes custody without mapping or
        // resizing.
        let (region, custody) = unsafe { Region::create(len) }.expect("a region");
        send(&mut server, companion.clone(), Some(custody.as_fd()));
        drop((region, custody));
        assert_eq!(in_step(&server), 1, "the second supersedes the first");
        assert_eq!(held_here(), 1, "the kernel agrees");
        assert!(
            server.status_text().contains(&format!(
                "journals=1 virtual_mib=1 companions_taken={taken}"
            )),
            "the second is counted though the count held stays 1: {}",
            server.status_text()
        );
    }
    send(&mut server, Frame::Release { rdev }, None);
    assert!(!server.holds(rdev), "the master closed");
    assert_eq!(in_step(&server), 0, "and every region beside it");
    assert_eq!(held_here(), 0, "the kernel agrees");
    assert!(
        server.status_text().contains("journals=0 virtual_mib=0"),
        "{}",
        server.status_text()
    );
    drop(client);
    drop(server);
}

/// How the dials of [`dials_whose_checks_need_each_other`] ended.
#[derive(Debug, PartialEq, Eq)]
struct DialsEnded {
    /// Dials whose HELLO a WELCOME answered.
    welcomed: usize,
    /// Dials the keeper closed unanswered.
    closed: usize,
    /// The keeper's count of dials turned away.
    refused: u64,
    /// Dials turned away because their check did not answer.
    unanswered: usize,
}

fn dials_whose_checks_need_each_other(n: usize, max: usize, hang: Duration) -> DialsEnded {
    use std::sync::{Arc, Condvar, Mutex, PoisonError};
    static SCRIPTED: aterm_uds::deadline::Gate =
        aterm_uds::deadline::Gate::new("akk-unit-scripted-check");
    let scratch = crate::test_scratch_dir(&format!("dials-{max}"));
    let mut server = Server::bind(ServeConfig {
        socket: scratch.path().join("k.sock"),
        identity: Identity::same_uid(),
        relaunch: Relauncher::Unavailable,
        version: "test".into(),
        test_drop_custody: false,
    })
    .expect("bind");
    server.dial_checks_max = max;
    server.check_hang = hang;
    let started = Arc::new((Mutex::new(0usize), Condvar::new()));
    server.test_check = Some(Box::new({
        let started = Arc::clone(&started);
        move || {
            let (count, moved) = &*started;
            *count.lock().unwrap_or_else(PoisonError::into_inner) += 1;
            moved.notify_all();
            let started = Arc::clone(&started);
            PeerCheck::start_on(&SCRIPTED, HANG, move || {
                let (count, moved) = &*started;
                let count = count.lock().unwrap_or_else(PoisonError::into_inner);
                let (count, waited) = moved
                    .wait_timeout_while(count, HANG, |running| *running < n)
                    .unwrap_or_else(PoisonError::into_inner);
                drop(count);
                if waited.timed_out() {
                    Err(CheckError::Unchecked("the other checks never ran".into()))
                } else {
                    Ok(())
                }
            })
        }
    }));
    let mut dials: Vec<Option<CtlStream>> = (0..n)
        .map(|_| {
            let dial = CtlStream::connect(server.socket()).expect("dial");
            write_frame(&dial, &hello_app(), None).expect("hello");
            dial.set_read_timeout(Some(HANG))
                .expect("the hang detector");
            Some(dial)
        })
        .collect();
    let (mut welcomed, mut closed) = (0, 0);
    let began = Instant::now();
    while dials.iter().any(Option::is_some) {
        assert!(began.elapsed() < HANG, "a dial was never answered");
        server.step(10).expect("turn");
        for slot in &mut dials {
            let Some(dial) = slot else { continue };
            let mut pfd = [PollFd {
                fd: dial.as_raw_fd(),
                events: POLLIN,
                revents: 0,
            }];
            if !matches!(sys::poll(&mut pfd, 0), Ok(ready) if ready > 0) {
                continue;
            }
            match read_frame(dial) {
                Ok(Some(Incoming {
                    frame: Frame::Welcome { .. },
                    ..
                })) => welcomed += 1,
                Ok(None) | Err(_) => closed += 1,
                Ok(Some(other)) => panic!("a dial read {:?} first", other.frame),
            }
            *slot = None;
        }
    }
    let unanswered = server
        .test_unchecked_ever()
        .iter()
        .filter(|why| why.contains("trustd"))
        .count();
    let ended = DialsEnded {
        welcomed,
        closed,
        refused: server.refused_dials,
        unanswered,
    };
    drop(server);
    ended
}

#[test]
fn a_connection_is_served_while_another_dials_check_runs() {
    use std::sync::atomic::{AtomicBool, Ordering};
    use std::sync::{Mutex, PoisonError, mpsc};
    static SCRIPTED: aterm_uds::deadline::Gate =
        aterm_uds::deadline::Gate::new("akk-unit-scripted-serve");
    let scratch = crate::test_scratch_dir("serve");
    let mut server = Server::bind(ServeConfig {
        socket: scratch.path().join("k.sock"),
        identity: Identity::same_uid(),
        relaunch: Relauncher::Unavailable,
        version: "test".into(),
        test_drop_custody: false,
    })
    .expect("bind");
    assert_eq!(server.poll_wait(TICK_MS), TICK_MS, "idle");
    server.check_hang = 2 * HANG;
    let (answer, held) = mpsc::channel::<()>();
    let held = Mutex::new(Some(held));
    let first = AtomicBool::new(true);
    server.test_check = Some(Box::new(move || {
        if first.swap(false, Ordering::SeqCst) {
            return PeerCheck::start_on(&SCRIPTED, HANG, || Ok(()));
        }
        let held = held
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .take()
            .expect("two dials");
        PeerCheck::start_on(&SCRIPTED, 2 * HANG, move || {
            held.recv_timeout(2 * HANG)
                .map_err(|e| CheckError::Unchecked(e.to_string()))
        })
    }));
    let window = CtlStream::connect(server.socket()).expect("the window's dial");
    let began = Instant::now();
    while server.conns.is_empty() {
        assert!(began.elapsed() < HANG, "the window was never admitted");
        server.step(10).expect("turn");
    }
    let dial = CtlStream::connect(server.socket()).expect("the second dial");
    server.step(0).expect("its accept");
    assert_eq!(server.dialing.len(), 1, "its check runs");
    for (asked, waits) in [
        (TICK_MS, CHECK_TICK_MS),
        (-1, CHECK_TICK_MS),
        (0, 0),
        (1, 1),
    ] {
        assert_eq!(server.poll_wait(asked), waits, "asked {asked} ms");
    }
    write_frame(&window, &hello_app(), None).expect("the window's HELLO");
    let welcomed = std::thread::spawn(move || {
        window
            .set_read_timeout(Some(HANG))
            .expect("the hang detector");
        let heard = read_frame(&window).map(|i| i.map(|i| i.frame));
        answer.send(()).expect("the second check waits");
        (window, heard)
    });
    let began = Instant::now();
    while server.conns.len() < 2 {
        assert!(began.elapsed() < HANG, "the second dial was never admitted");
        server.step(10).expect("turn");
    }
    let (window, heard) = welcomed.join().expect("the window's reader");
    assert!(
        matches!(heard, Ok(Some(Frame::Welcome { .. }))),
        "the window's HELLO went unanswered while the check ran: {heard:?}"
    );
    assert_eq!(server.poll_wait(TICK_MS), TICK_MS, "no check runs");
    drop((window, dial, server));
}

#[test]
fn queued_dials_are_checked_at_once_each_under_its_own_deadline() {
    assert_eq!(
        dials_whose_checks_need_each_other(3, DIAL_CHECKS_MAX, HANG),
        DialsEnded {
            welcomed: 3,
            closed: 0,
            refused: 0,
            unanswered: 0
        }
    );
    let one_at_a_time = dials_whose_checks_need_each_other(3, 1, Duration::from_millis(200));
    let DialsEnded {
        welcomed,
        closed,
        refused,
        unanswered,
    } = one_at_a_time;
    assert!(
        unanswered >= 2
            && closed == unanswered
            && refused == closed as u64
            && welcomed + closed == 3,
        "one check at a time: {one_at_a_time:?}"
    );
}

#[cfg(target_vendor = "apple")]
#[test]
fn a_valid_pending_peer_check_survives_scheduling_delay() {
    use std::sync::{Mutex, mpsc};
    static SCRIPTED: aterm_uds::deadline::Gate =
        aterm_uds::deadline::Gate::new("akk-delayed-valid-peer");
    for (backdate, admitted) in [
        (Duration::from_secs(11), true),
        (HANG + Duration::from_secs(1), false),
    ] {
        let scratch = crate::test_scratch_dir("delayed-valid-peer");
        let mut server = Server::bind(ServeConfig {
            socket: scratch.path().join("k.sock"),
            identity: Identity::same_uid(),
            relaunch: Relauncher::Unavailable,
            version: "test".into(),
            test_drop_custody: false,
        })
        .unwrap();
        let (release, held) = mpsc::channel();
        let (returned, proof) = mpsc::channel();
        let channels = Mutex::new(Some((held, returned)));
        server.test_check = Some(Box::new(move || {
            let (held, returned) = channels.lock().unwrap().take().unwrap();
            PeerCheck::start_on(&SCRIPTED, HANG, move || {
                let result = held
                    .recv_timeout(HANG)
                    .map_err(|error| CheckError::Unchecked(error.to_string()));
                returned.send(result.clone()).unwrap();
                result
            })
        }));
        let dial = CtlStream::connect(server.socket()).unwrap();
        dial.set_read_timeout(Some(HANG)).unwrap();
        write_frame(&dial, &hello_app(), None).unwrap();
        server.accept_all();
        assert_eq!(server.dialing.len(), 1);
        server.dialing[0].until -= backdate;
        server.admit_checked();
        release.send(()).unwrap();
        assert_eq!(proof.recv_timeout(HANG).unwrap(), Ok(()));
        let began = Instant::now();
        while !server.dialing.is_empty() {
            assert!(began.elapsed() < HANG, "the check never completed");
            server.step(10).unwrap();
        }
        server.step(0).unwrap();
        let welcomed = matches!(
            read_frame(&dial).unwrap(),
            Some(Incoming {
                frame: Frame::Welcome { .. },
                ..
            })
        );
        assert_eq!(welcomed, admitted, "{}", server.status_text());
        assert_eq!(server.refused_dials, u64::from(!admitted));
    }
}

#[test]
fn a_passed_dial_is_admitted_behind_a_dial_whose_check_still_runs() {
    use std::sync::atomic::{AtomicBool, Ordering};
    use std::sync::{Mutex, PoisonError, mpsc};
    static SCRIPTED: aterm_uds::deadline::Gate =
        aterm_uds::deadline::Gate::new("akk-unit-scripted-behind");
    let scratch = crate::test_scratch_dir("behind");
    let mut server = Server::bind(ServeConfig {
        socket: scratch.path().join("k.sock"),
        identity: Identity::same_uid(),
        relaunch: Relauncher::Unavailable,
        version: "test".into(),
        test_drop_custody: false,
    })
    .expect("bind");
    server.check_hang = 2 * HANG;
    let (release, held) = mpsc::channel::<()>();
    let held = Mutex::new(Some(held));
    let first = AtomicBool::new(true);
    server.test_check = Some(Box::new(move || {
        if !first.swap(false, Ordering::SeqCst) {
            return PeerCheck::start_on(&SCRIPTED, HANG, || Ok(()));
        }
        let held = held
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .take()
            .expect("one slow dial");
        PeerCheck::start_on(&SCRIPTED, 2 * HANG, move || {
            held.recv_timeout(2 * HANG)
                .map_err(|e| CheckError::Unchecked(e.to_string()))
        })
    }));
    let slow = CtlStream::connect(server.socket()).expect("the slow dial");
    server.step(0).expect("its accept");
    assert_eq!(server.dialing.len(), 1, "the slow check runs");
    let fast = CtlStream::connect(server.socket()).expect("the fast dial");
    let began = Instant::now();
    while server.conns.is_empty() {
        assert!(
            began.elapsed() < HANG,
            "the fast dial waited behind the slow one's check"
        );
        server.step(10).expect("turn");
    }
    assert_eq!(
        (server.conns.len(), server.dialing.len()),
        (1, 1),
        "the slow dial is still being checked"
    );
    release.send(()).expect("the slow check waits");
    let began = Instant::now();
    while server.conns.len() < 2 {
        assert!(began.elapsed() < HANG, "the slow dial was never admitted");
        server.step(10).expect("turn");
    }
    assert_eq!(server.refused_dials, 0, "none turned away");
    drop((slow, fast, server));
}

#[test]
fn bind_refuses_non_socket_and_symlink_paths_without_unlinking_them() {
    use std::os::unix::fs::PermissionsExt as _;
    let scratch = crate::test_scratch_dir("bind-nonsock");
    std::fs::set_permissions(scratch.path(), std::fs::Permissions::from_mode(0o700)).expect("0700");

    let regular = scratch.path().join("not-a.sock");
    std::fs::write(&regular, b"keep").expect("write regular");
    let err = match Server::bind(ServeConfig {
        socket: regular.clone(),
        identity: Identity::same_uid(),
        relaunch: Relauncher::Unavailable,
        version: "test".into(),
        test_drop_custody: false,
    }) {
        Ok(_) => panic!("regular file must not be unlinked and bound over"),
        Err(e) => e,
    };
    assert_eq!(err.kind(), io::ErrorKind::AlreadyExists, "{err}");
    assert_eq!(std::fs::read(&regular).expect("kept"), b"keep");

    let link = scratch.path().join("link.sock");
    std::os::unix::fs::symlink(&regular, &link).expect("symlink");
    let err = match Server::bind(ServeConfig {
        socket: link.clone(),
        identity: Identity::same_uid(),
        relaunch: Relauncher::Unavailable,
        version: "test".into(),
        test_drop_custody: false,
    }) {
        Ok(_) => panic!("symlink must not be unlinked"),
        Err(e) => e,
    };
    assert_eq!(err.kind(), io::ErrorKind::AlreadyExists, "{err}");
    assert!(std::fs::symlink_metadata(&link).is_ok());
    assert_eq!(std::fs::read(&regular).expect("kept"), b"keep");

    let dangling = scratch.path().join("dangling.sock");
    std::os::unix::fs::symlink(scratch.path().join("missing"), &dangling).expect("dangling");
    let err = match Server::bind(ServeConfig {
        socket: dangling.clone(),
        identity: Identity::same_uid(),
        relaunch: Relauncher::Unavailable,
        version: "test".into(),
        test_drop_custody: false,
    }) {
        Ok(_) => panic!("dangling symlink must be refused"),
        Err(e) => e,
    };
    assert_eq!(err.kind(), io::ErrorKind::AlreadyExists, "{err}");
    assert!(std::fs::symlink_metadata(&dangling).is_ok());
}
