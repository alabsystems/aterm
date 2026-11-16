// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

use super::*;
use crate::control::control_media::{Hops, take_hop};

/// A file URI this platform reads as an absolute path: Windows' needs a drive.
const X: &str = if cfg!(windows) {
    "file:///C:/tmp/x.txt"
} else {
    "file:///tmp/x.txt"
};

/// One park slice of at most 5 ms: these tests never wait out a real bound.
fn short(left: Duration) -> Duration {
    left.min(Duration::from_millis(5))
}

fn never() -> bool {
    false
}

fn always() -> bool {
    true
}

/// An unserved lane of [`short`] slices that asks after its caller with `hung_up`.
fn lane(hung_up: fn() -> bool) -> crate::park::Lane {
    crate::park::Lane {
        slice: short,
        hung_up,
        bell: None,
    }
}

thread_local! {
    /// The hops a counted `await edit`'s lane looks at, and what each of its asks found there.
    static ASKED: std::cell::RefCell<(Option<&'static Hops>, Vec<bool>)> =
        const { std::cell::RefCell::new((None, Vec::new())) };
}

/// A lane's ask after its caller that records whether its request's hop still counts.
fn counted_ask() -> bool {
    ASKED.with(|asked| {
        let mut asked = asked.borrow_mut();
        if let Some(hops) = asked.0 {
            let counted = hops.since_ns().is_some();
            asked.1.push(counted);
        }
    });
    false
}

/// Look at `hops` from this thread's next [`counted_ask`]s on.
fn count_asks(hops: &'static Hops) {
    ASKED.with(|asked| *asked.borrow_mut() = (Some(hops), Vec::new()));
}

/// What this thread's [`counted_ask`]s found since [`count_asks`].
fn asks() -> Vec<bool> {
    ASKED.with(|asked| std::mem::take(&mut asked.borrow_mut().1))
}

/// A count of hops of the calling test's own.
fn fresh_hops() -> &'static Hops {
    Box::leak(Box::new(Hops::new()))
}

/// Injected `MainPost` that unwraps each `Wake` hop before calling `serve`.
fn main_thread<'a>(mut serve: impl FnMut(Wake) -> bool + 'a) -> MainPost<'a> {
    MainPost::injected(move |wake| serve(take_hop(wake)), fresh_hops())
}

#[test]
fn edit_refuses_before_it_posts_anything() {
    let mut posted = Vec::new();
    let mut post = |wake: Wake| {
        posted.push(format!("{wake:?}"));
        true
    };
    let uri = X;
    let ask = |scope, session, rest: &str, post: &mut dyn FnMut(Wake) -> bool| {
        serve_edit(
            scope,
            session,
            rest,
            &mut main_thread(post),
            &mut never,
            Duration::ZERO,
            short,
        )
    };
    let edge = Scope::Edge(aterm_session::EdgeToken::from_bytes([7; 32]));
    assert_eq!(ask(edge, Some(1), uri, &mut post), "ERR scope\n");
    for (session, rest, refusal) in [
        (None, uri, "ERR usage: @<sid> edit "),
        (Some(1), "", "ERR usage: edit "),
        (Some(1), "file:///x line=0", "ERR bad line=0"),
        (Some(1), "file://host/x", "ERR refused bad-uri"),
    ] {
        let line = ask(Scope::Owner, session, rest, &mut post);
        assert!(line.starts_with(refusal), "{rest:?}: {line}");
    }
    assert!(posted.is_empty(), "{posted:?}");
}

#[test]
fn edit_answers_what_the_main_thread_answers() {
    let mut post = |wake: Wake| {
        if let Wake::EditRequest(call) = wake {
            assert_eq!(call.session, 9);
            assert_eq!(call.request.line, Some((3, None)));
            let line = format!("OK edit ticket={} view=4 doc=1 uri=x\n", call.ticket);
            call.reply.send(line).unwrap();
        }
        true
    };
    let line = serve_edit(
        Scope::Owner,
        Some(9),
        &format!("{X} line=3"),
        &mut main_thread(&mut post),
        &mut never,
        Duration::from_secs(60),
        short,
    );
    assert!(line.starts_with("OK edit ticket="), "{line}");
}

/// Deadline expiry cancels the ticket and names it in the timeout reply.
#[test]
fn timeout_cancels_the_ticket_it_names() {
    let mut held = None;
    let mut cancelled = None;
    let mut post = |wake: Wake| {
        match wake {
            Wake::EditRequest(call) => held = Some(*call),
            Wake::EditCancel { ticket } => cancelled = Some(ticket),
            other => panic!("unexpected {other:?}"),
        }
        true
    };
    let line = serve_edit(
        Scope::Owner,
        Some(1),
        X,
        &mut main_thread(&mut post),
        &mut never,
        Duration::from_millis(20),
        short,
    );
    let ticket = held.expect("posted").ticket;
    assert_eq!(line, format!("ERR timeout ticket={ticket}\n"));
    assert_eq!(cancelled, Some(ticket));
}

/// `edit` and `await edit` count their request hops until taken or returned.
#[test]
fn edit_requests_are_counted_hops_until_taken() {
    let hops = fresh_hops();
    let (reply, _answer) = std::sync::mpsc::channel();
    let raw = Wake::EditRequest(Box::new(EditCall {
        ticket: mint_ticket(),
        session: 1,
        request: EditRequest::parse(X).expect("a request"),
        real_uri: None,
        reply,
    }));
    assert!(MainPost::injected(|_| true, hops).tell(raw));
    assert_eq!(hops.since_ns(), None, "a raw post is not counted");

    let mut posted = Vec::new();
    let mut counted = Vec::new();
    let line = serve_edit(
        Scope::Owner,
        Some(1),
        X,
        &mut MainPost::injected(
            |wake| {
                posted.push(wake);
                true
            },
            hops,
        ),
        &mut || {
            counted.push(hops.since_ns().is_some());
            false
        },
        Duration::from_millis(20),
        short,
    );
    assert!(line.starts_with("ERR timeout ticket="), "{line}");
    assert!(
        !counted.is_empty() && counted.iter().all(|&c| c),
        "{counted:?}"
    );
    assert_eq!(hops.since_ns(), None, "the lane returned");
    match posted.as_slice() {
        [Wake::Hop { wake, .. }, Wake::EditCancel { .. }]
            if matches!(**wake, Wake::EditRequest(_)) => {}
        other => panic!("the request counted, its cancel not: {other:?}"),
    }

    let mut posted = Vec::new();
    count_asks(hops);
    let line = serve_await_edit(
        Scope::Owner,
        "edit ticket=5 timeout=10",
        &mut MainPost::injected(
            |wake| {
                posted.push(wake);
                true
            },
            hops,
        ),
        lane(counted_ask),
    );
    let counted = asks();
    assert_eq!(line, "OK timeout\n");
    assert!(
        !counted.is_empty() && counted.iter().all(|&c| c),
        "{counted:?}"
    );
    assert_eq!(hops.since_ns(), None, "the lane returned");
    match posted.as_slice() {
        [Wake::Hop { wake, .. }, Wake::EditAwaitWithdraw { .. }]
            if matches!(**wake, Wake::EditAwait(_)) => {}
        other => panic!("the wait counted, its withdrawal not: {other:?}"),
    }

    let mut taken = Vec::new();
    count_asks(hops);
    let line = serve_await_edit(
        Scope::Owner,
        "edit ticket=5 timeout=10",
        &mut MainPost::injected(
            |wake| {
                taken.push(take_hop(wake));
                true
            },
            hops,
        ),
        lane(counted_ask),
    );
    let counted = asks();
    assert_eq!(line, "OK timeout\n");
    assert!(
        !counted.is_empty() && counted.iter().all(|&c| !c),
        "{counted:?}"
    );
    assert!(
        matches!(
            taken.as_slice(),
            [Wake::EditAwait(_), Wake::EditAwaitWithdraw { .. }]
        ),
        "{taken:?}"
    );
}

#[test]
fn edit_hangup_cancels_and_answers_hangup() {
    let mut cancelled = None;
    let mut keep = None;
    let mut post = |wake: Wake| {
        match wake {
            Wake::EditRequest(call) => keep = Some(*call),
            Wake::EditCancel { ticket } => cancelled = Some(ticket),
            other => panic!("unexpected {other:?}"),
        }
        true
    };
    let mut calls = 0;
    let mut hung = || {
        calls += 1;
        calls > 1
    };
    let line = serve_edit(
        Scope::Owner,
        Some(1),
        X,
        &mut main_thread(&mut post),
        &mut hung,
        Duration::from_secs(60),
        short,
    );
    assert_eq!(line, crate::control::HUNG_UP_REPLY);
    assert_eq!(cancelled, keep.map(|call| call.ticket));
}

#[test]
fn await_edit_withdraws_on_every_exit() {
    let mut withdrawn = Vec::new();
    let mut post = |wake: Wake| {
        match wake {
            Wake::EditAwait(call) => {
                assert_eq!(call.key, AwaitKey::Ticket(Ticket(5)));
                call.reply
                    .send("OK edit done result=closed saved=1 seq=2\n".to_string())
                    .unwrap();
            }
            Wake::EditAwaitWithdraw { waiter } => withdrawn.push(waiter),
            other => panic!("unexpected {other:?}"),
        }
        true
    };
    let line = serve_await_edit(
        Scope::Owner,
        "edit ticket=5",
        &mut main_thread(&mut post),
        lane(never),
    );
    assert_eq!(line, "OK edit done result=closed saved=1 seq=2\n");
    let mut parked = Vec::new();
    let mut post_timeout = |wake: Wake| {
        match wake {
            Wake::EditAwait(call) => parked.push(*call),
            Wake::EditAwaitWithdraw { waiter } => withdrawn.push(waiter),
            other => panic!("unexpected {other:?}"),
        }
        true
    };
    let line = serve_await_edit(
        Scope::Owner,
        &format!("edit uri={X} timeout=10"),
        &mut main_thread(&mut post_timeout),
        lane(never),
    );
    assert_eq!(line, "OK timeout\n");
    let mut post_hung = |wake: Wake| {
        match wake {
            Wake::EditAwait(call) => parked.push(*call),
            Wake::EditAwaitWithdraw { waiter } => withdrawn.push(waiter),
            other => panic!("unexpected {other:?}"),
        }
        true
    };
    let line = serve_await_edit(
        Scope::Owner,
        "edit ticket=5",
        &mut main_thread(&mut post_hung),
        lane(always),
    );
    assert_eq!(line, crate::control::HUNG_UP_REPLY);
    assert_eq!(withdrawn.len(), 3, "{withdrawn:?}");
    assert_eq!(
        parked.iter().map(|call| call.waiter).collect::<Vec<_>>(),
        withdrawn[1..].to_vec(),
        "each waiter withdraws itself"
    );
    let mut nothing = |wake: Wake| panic!("posted {wake:?}");
    assert!(
        serve_await_edit(
            Scope::Owner,
            "edit ticket=x",
            &mut main_thread(&mut nothing),
            lane(never),
        )
        .starts_with("ERR")
    );
    let edge = Scope::Edge(aterm_session::EdgeToken::from_bytes([7; 32]));
    assert_eq!(
        serve_await_edit(
            edge,
            "edit ticket=5",
            &mut main_thread(&mut nothing),
            lane(never),
        ),
        "ERR scope\n"
    );
}

/// Resolves and forwards both the requested URI and its canonical realpath URI.
#[cfg(unix)]
#[test]
fn edit_lanes_resolve_the_realpath_spelling() {
    let dir = aterm_tempfile::Builder::new()
        .prefix("aterm-edit-real-")
        .tempdir()
        .unwrap();
    let real_dir = dir.path().join("real");
    std::fs::create_dir(&real_dir).unwrap();
    std::fs::write(real_dir.join("x.txt"), "x\n").unwrap();
    std::os::unix::fs::symlink(&real_dir, dir.path().join("link")).unwrap();
    let spelled = amacs::protocol::uri::path_to_file_uri(&dir.path().join("link/x.txt")).unwrap();
    let real = amacs::protocol::uri::path_to_file_uri(
        &std::fs::canonicalize(real_dir.join("x.txt")).unwrap(),
    )
    .unwrap();
    assert_ne!(spelled, real);
    assert_eq!(real_file_uri(&spelled).as_deref(), Some(real.as_str()));
    assert_eq!(real_file_uri(&format!("{spelled}.gone")), None);

    let mut parked = None;
    let mut post = |wake: Wake| {
        match wake {
            Wake::EditAwait(call) => parked = Some(*call),
            Wake::EditAwaitWithdraw { .. } => {}
            other => panic!("unexpected {other:?}"),
        }
        true
    };
    let rest = format!("edit uri={spelled} timeout=1");
    serve_await_edit(
        Scope::Owner,
        &rest,
        &mut main_thread(&mut post),
        lane(never),
    );
    let parked = parked.expect("posted");
    assert_eq!(
        parked.key,
        AwaitKey::Uri(spelled.clone()),
        "the key as asked"
    );
    assert_eq!(parked.real_uri, Some(real.clone()));

    let mut requested = None;
    let mut post = |wake: Wake| {
        match wake {
            Wake::EditRequest(call) => requested = Some(*call),
            Wake::EditCancel { .. } => {}
            other => panic!("unexpected {other:?}"),
        }
        true
    };
    serve_edit(
        Scope::Owner,
        Some(1),
        &spelled,
        &mut main_thread(&mut post),
        &mut never,
        Duration::ZERO,
        short,
    );
    let requested = requested.expect("posted");
    assert_eq!(requested.request.uri, spelled, "the request as asked");
    assert_eq!(requested.real_uri, Some(real));
}

#[test]
fn tickets_are_unique_and_carry_this_process() {
    let a = mint_ticket();
    let b = mint_ticket();
    assert_ne!(a, b);
    assert_eq!(a.0 >> 32, u64::from(std::process::id()));
}

/// The `await edit` (`amacs --wait`) the hang-up bell rings: its caller is a socket pair's
/// client, its lane a thread the test starts, its main thread a channel to the test. Each wait for
/// something that must happen is the one-minute hang detector, and no count depends on time.
#[cfg(unix)]
mod belled {
    use std::os::fd::AsRawFd as _;
    use std::sync::atomic::{AtomicUsize, Ordering};
    use std::time::Duration;

    use aterm_control::hangup::{HANGUP_POLL, ServingFd, caller_hung_up, hangup_park};
    use aterm_control::park::BellLane;
    use aterm_spec::wait::{MUST_HAPPEN, until_within};
    use aterm_uds::CtlStream;
    use aterm_uds::hangup::Bell;

    use super::{AwaitCall, MainPost, Scope, Wake, await_edit, main_thread, serve_await_edit};
    use crate::park::Lane;

    /// The `await edit` every test here parks: ten minutes, so only a ring ends it within the
    /// hang detector.
    const REST: &str = "edit ticket=5 timeout=600000";

    #[track_caller]
    fn until(what: &str, mut seen: impl FnMut() -> bool) {
        until_within(MUST_HAPPEN, what, || seen().then_some(()));
    }

    /// [`REST`] over `server` on `lane` ([`serve_await_edit`]), as [`served_by`] runs it.
    fn parked_await(
        server: CtlStream,
        lane: Lane,
    ) -> (
        std::thread::JoinHandle<(String, usize)>,
        std::sync::mpsc::Receiver<AwaitCall>,
    ) {
        served_by(server, move |post| {
            serve_await_edit(Scope::Owner, REST, post, lane)
        })
    }

    /// `serve` over `server`, on a lane thread of its own that serves that connection, whose main
    /// thread hands each posted wait to the receiver returned and counts the withdraws.
    fn served_by(
        server: CtlStream,
        serve: impl FnOnce(&mut MainPost<'_>) -> String + Send + 'static,
    ) -> (
        std::thread::JoinHandle<(String, usize)>,
        std::sync::mpsc::Receiver<AwaitCall>,
    ) {
        let (calls, posted) = std::sync::mpsc::channel();
        let thread = std::thread::spawn(move || {
            let _serving = ServingFd::enter(server.as_raw_fd());
            let mut withdrawn = 0;
            let mut post = |wake: Wake| {
                match wake {
                    Wake::EditAwait(call) => calls.send(*call).expect("the test hears it"),
                    Wake::EditAwaitWithdraw { .. } => withdrawn += 1,
                    other => panic!("unexpected {other:?}"),
                }
                true
            };
            let line = serve(&mut main_thread(&mut post));
            (line, withdrawn)
        });
        (thread, posted)
    }

    /// AN `await edit` WHOSE CALLER CLOSES ANSWERS `ERR hangup` AT THE RING AND WITHDRAWS, though
    /// its timeout is ten minutes: armed on the process's bell, it waits once at its door for all
    /// that is left (its bell lane asked once, its sliced lane never), and the caller closes only
    /// once that arm stands, so only the bell's ring of the door can end it within the hang
    /// detector.
    /// NEGATIVE CONTROL: arming the bell to ring a slot that is not the door leaves it waiting past
    /// the hang detector.
    #[test]
    fn an_await_edit_whose_caller_closes_answers_hangup_at_the_ring() {
        static BELLED: AtomicUsize = AtomicUsize::new(0);
        fn belled(left: Duration) -> Duration {
            BELLED.fetch_add(1, Ordering::SeqCst);
            left
        }
        fn sliced(_left: Duration) -> Duration {
            panic!("an armed await on a live bell is never sliced")
        }
        let (client, server) = CtlStream::pair().expect("socket pair");
        let served = server.as_raw_fd();
        let (lane, posted) = parked_await(
            server,
            Lane {
                slice: sliced,
                hung_up: caller_hung_up,
                bell: Some(BellLane {
                    bell: Bell::process,
                    slice: belled,
                }),
            },
        );
        let _call = posted
            .recv_timeout(MUST_HAPPEN)
            .expect("the wait is posted");
        until("the await to arm the bell on its connection", || {
            Bell::process().armed_for_test(served)
        });
        drop(client);
        until("the parked await to hear its caller hang up", || {
            lane.is_finished()
        });
        assert_eq!(
            lane.join().expect("the lane"),
            (crate::control::HUNG_UP_REPLY.to_string(), 1),
            "it answers the hang-up and withdraws its waiter"
        );
        assert_eq!(BELLED.load(Ordering::SeqCst), 1, "one wait, to the ring");
    }

    /// AN ARMED `await edit` HEARS ITS ANSWER AT ITS DOOR: its one wait is for all of its ten
    /// minutes, so the main thread's answer, sent once the arm stands, ends it within the hang
    /// detector only because the answer rings the door the lane parks at.
    /// NEGATIVE CONTROL: an answer that does not ring the door leaves the lane waiting past the
    /// hang detector.
    #[test]
    fn an_armed_await_edit_hears_its_answer_at_its_door() {
        fn unsliced(left: Duration) -> Duration {
            left
        }
        let (_client, server) = CtlStream::pair().expect("socket pair");
        let served = server.as_raw_fd();
        let (lane, posted) = parked_await(
            server,
            Lane {
                slice: unsliced,
                hung_up: caller_hung_up,
                bell: Some(BellLane {
                    bell: Bell::process,
                    slice: unsliced,
                }),
            },
        );
        let call = posted
            .recv_timeout(MUST_HAPPEN)
            .expect("the wait is posted");
        until("the await to arm the bell on its connection", || {
            Bell::process().armed_for_test(served)
        });
        let done = "OK edit done result=closed saved=1 seq=2\n";
        call.reply.send(done.to_string()).expect("the lane waits");
        until("the parked await to hear its answer", || lane.is_finished());
        assert_eq!(lane.join().expect("the lane"), (done.to_string(), 1));
        drop(call);
    }

    /// AN UNARMED `await edit` IS SLICED AT HANGUP_POLL: on a bell whose thread has ended it
    /// cannot arm, so it waits at its door by its lane's slices, each no longer than HANGUP_POLL,
    /// and asks after its caller between them, ending at the lane's second ask, its bell lane
    /// never asked.
    /// NEGATIVE CONTROL: a park that takes its bell lane whether or not the arm stands waits once
    /// for ten minutes and is reported at the hang detector.
    #[test]
    fn an_unarmed_await_edit_is_sliced_at_hangup_poll() {
        static BELLED: AtomicUsize = AtomicUsize::new(0);
        static SLICED: AtomicUsize = AtomicUsize::new(0);
        static LONGEST: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
        fn belled(left: Duration) -> Duration {
            BELLED.fetch_add(1, Ordering::SeqCst);
            left
        }
        fn sliced(left: Duration) -> Duration {
            SLICED.fetch_add(1, Ordering::SeqCst);
            let slice = hangup_park(left);
            let nanos = u64::try_from(slice.as_nanos()).unwrap_or(u64::MAX);
            LONGEST.fetch_max(nanos, Ordering::SeqCst);
            slice
        }
        fn gone_after_two_slices() -> bool {
            SLICED.load(Ordering::SeqCst) >= 2
        }
        let (_client, server) = CtlStream::pair().expect("socket pair");
        let (lane, posted) = parked_await(
            server,
            Lane {
                slice: sliced,
                hung_up: gone_after_two_slices,
                bell: Some(BellLane {
                    bell: Bell::ended_for_test,
                    slice: belled,
                }),
            },
        );
        let _call = posted
            .recv_timeout(MUST_HAPPEN)
            .expect("the wait is posted");
        until("the unarmed await to end", || lane.is_finished());
        assert_eq!(
            lane.join().expect("the lane"),
            (crate::control::HUNG_UP_REPLY.to_string(), 1)
        );
        assert_eq!(
            (BELLED.load(Ordering::SeqCst), SLICED.load(Ordering::SeqCst)),
            (0, 2),
            "a bell that has ended arms nothing, and the await is sliced"
        );
        let longest = Duration::from_nanos(LONGEST.load(Ordering::SeqCst));
        assert!(
            longest <= HANGUP_POLL,
            "a slice of {longest:?} outran HANGUP_POLL"
        );
    }

    /// AN ARMED `await edit` HEARS ITS WAIT DROPPED UNANSWERED AT ITS DOOR: its one wait is for
    /// all of its ten minutes (its bell lane asked once, its sliced lane never), and the main
    /// thread drops the posted wait without an answer (as the event loop does with a queued
    /// `Wake::EditAwait` it never serves) once the arm stands. The DoorReply's drop lets its answer
    /// go and then rings the door, so the woken lane reads the answer gone and answers
    /// `ERR edit waiter was dropped` and withdraws, within the hang detector.
    /// NEGATIVE CONTROL: a DoorReply whose drop does not ring leaves the lane waiting past the hang
    /// detector. (A drop that rings before it lets the answer go fails only where the lane reads
    /// the answer between the two, which no test can arrange.)
    #[test]
    fn an_armed_await_edit_hears_its_wait_dropped_at_its_door() {
        static BELLED: AtomicUsize = AtomicUsize::new(0);
        fn belled(left: Duration) -> Duration {
            BELLED.fetch_add(1, Ordering::SeqCst);
            left
        }
        fn sliced(_left: Duration) -> Duration {
            panic!("an armed await on a live bell is never sliced")
        }
        let (_client, server) = CtlStream::pair().expect("socket pair");
        let served = server.as_raw_fd();
        let (lane, posted) = parked_await(
            server,
            Lane {
                slice: sliced,
                hung_up: caller_hung_up,
                bell: Some(BellLane {
                    bell: Bell::process,
                    slice: belled,
                }),
            },
        );
        let call = posted
            .recv_timeout(MUST_HAPPEN)
            .expect("the wait is posted");
        until("the await to arm the bell on its connection", || {
            Bell::process().armed_for_test(served)
        });
        drop(call);
        until("the parked await to hear its wait dropped", || {
            lane.is_finished()
        });
        assert_eq!(
            lane.join().expect("the lane"),
            ("ERR edit waiter was dropped\n".to_string(), 1),
            "it answers the drop and withdraws its waiter"
        );
        assert_eq!(BELLED.load(Ordering::SeqCst), 1, "one wait, to the ring");
    }

    /// AN `await edit` ARMS THE PROCESS'S HANG-UP BELL ON ITS SHIPPED LANE: served as the
    /// dispatcher serves it ([`await_edit`], no lane of the test's own), it arms
    /// `Bell::process()` on its connection while it waits at its door, and a caller that then
    /// closes is answered `ERR hangup`, its waiter withdrawn, though its timeout is ten minutes.
    /// The arm is the sign: a shipped lane that armed no bell would hear the close too, by its
    /// HANGUP_POLL slices, so only the arm tells the bell's lane from the sliced one.
    /// NEGATIVE CONTROL: an [`await_edit`] whose lane carries no bell lane never arms, and the hang
    /// detector fires before the close.
    #[test]
    fn an_await_edit_arms_the_process_bell_on_its_shipped_lane() {
        let (client, server) = CtlStream::pair().expect("socket pair");
        let served = server.as_raw_fd();
        let (lane, posted) = served_by(server, |post| await_edit(post, Scope::Owner, REST));
        let _call = posted
            .recv_timeout(MUST_HAPPEN)
            .expect("the wait is posted");
        until(
            "the await to arm the process bell on its connection",
            || Bell::process().armed_for_test(served),
        );
        drop(client);
        until("the parked await to hear its caller hang up", || {
            lane.is_finished()
        });
        assert_eq!(
            lane.join().expect("the lane"),
            (crate::control::HUNG_UP_REPLY.to_string(), 1),
            "it answers the hang-up and withdraws its waiter"
        );
    }
}
