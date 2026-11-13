// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

use super::*;

#[test]
fn receipt_and_expired_bodies_are_what_astream_reads() {
    use astream_wire::{Offset, ReceiptError, ReceiptVerdict, V1Receipt};
    const T: u64 = 1_700_000_000_999;
    for verdict in ReceiptVerdict::ALL {
        let word = verdict.as_str();
        let wire = receipt_body(T, "s-host", 41, word).encode();
        let line = String::from_utf8_lossy(&wire).into_owned();
        assert_eq!(
            V1Receipt::decode("ack", &wire),
            Ok(V1Receipt::Ack {
                re: Offset(41),
                verdict: Some(verdict),
                sender_t_ms: Some(T),
            }),
            "{line}"
        );
        let (ours, _) = Body::decode(&wire);
        assert_eq!(
            (ours.re, ours.verdict.as_deref(), ours.from.as_deref()),
            (Some(41), Some(word), Some("s-host")),
            "{line}"
        );
    }
    let posted = |dl: Option<u64>, via: Option<&str>| QueuedPost {
        sid: "s-b".into(),
        id: 1,
        to: "s-a".into(),
        kind: "ack".into(),
        re: Some(41),
        dl,
        via: via.map(str::to_string),
        key: None,
        body: b"got it".to_vec(),
    };
    for post in [posted(None, None), posted(Some(30_000), Some("h-x"))] {
        let wire = post_body(T, &post, None).encode();
        let line = String::from_utf8_lossy(&wire).into_owned();
        assert_eq!(
            V1Receipt::decode(&post.kind, &wire),
            Ok(V1Receipt::Ack {
                re: Offset(41),
                verdict: None,
                sender_t_ms: Some(T),
            }),
            "{line}"
        );
        let (ours, _) = Body::decode(&wire);
        assert_eq!(
            (ours.re, ours.verdict, ours.kind, ours.text.as_str()),
            (Some(41), None, None, "got it"),
            "{line}"
        );
    }
    for (dl, span) in [(240_000, "240 s"), (1_500, "1500 ms")] {
        let wire = expired_body(T, 7, dl).encode();
        let line = String::from_utf8_lossy(&wire).into_owned();
        assert_eq!(
            V1Receipt::decode("expired", &wire),
            Ok(V1Receipt::Expired {
                re: Offset(7),
                dl_ms: dl,
                sender_t_ms: Some(T),
            }),
            "{line}"
        );
        let (ours, _) = Body::decode(&wire);
        let text = format!("no reply within {span}");
        assert_eq!(
            (ours.re, ours.dl, ours.text.as_str()),
            (Some(7), Some(dl), text.as_str()),
            "{line}"
        );
    }
    let mut receipt = receipt_body(T, "s-host", 41, "handled");
    receipt.re = None;
    let mut post = posted(None, None);
    post.re = None;
    let unanswering = post_body(T, &post, None);
    let mut expired = expired_body(T, 7, 240_000);
    expired.re = None;
    for (kind, body) in [("ack", receipt), ("ack", unanswering), ("expired", expired)] {
        assert_eq!(
            V1Receipt::decode(kind, &body.encode()),
            Err(ReceiptError::MissingOrDuplicateField("re")),
            "{kind}"
        );
    }
}

#[test]
fn replay_scheduler_refines_shared_budget_and_rotation() {
    let model = aterm_models_session::say_replay_budget_model();
    let mut state = model.init_state();
    let now = Instant::now();
    let mut replay = SayReplay::new(now);
    let owed = BTreeSet::from(["s-a".to_string(), "s-b".to_string()]);

    assert!(model.fire("Control", &mut state));
    for _ in 0..SAY_REPLAY_PAGES {
        let sid = replay.take(&owed).expect("one shared page remains");
        let action = if sid == "s-a" { "FetchA" } else { "FetchB" };
        assert!(model.action_enabled(action, &state), "{action}: {state:?}");
        assert!(model.fire(action, &mut state));
    }
    assert_eq!((state["got_a"], state["got_b"]), (2, 2));
    replay.refill(now + roster_refresh() - Duration::from_millis(1));
    assert!(replay.take(&owed).is_none());

    let old = aterm_spec::interp::with_buggy(&model, 1);
    let mut unfair = old.init_state();
    assert!(old.fire("Control", &mut unfair));
    assert!(old.fire("FetchA", &mut unfair));
    assert!(old.fire("FetchA", &mut unfair));
    assert!(!old.check_invariant("FairRotation", &unfair));

    replay.refill(now + roster_refresh());
    assert!(model.fire("Refill", &mut state));
    for _ in 0..SAY_REPLAY_PAGES {
        let sid = replay.take(&owed).expect("the next interval has pages");
        let action = if sid == "s-a" { "FetchA" } else { "FetchB" };
        assert!(model.action_enabled(action, &state), "{action}: {state:?}");
        assert!(model.fire(action, &mut state));
    }
    assert_eq!((state["got_a"], state["got_b"]), (4, 4));

    let mailbox = Mailbox::default();
    let _ = mailbox.push(
        Source::Inbox,
        (1, "mail".to_string(), Vec::new()),
        None,
        0,
        Instant::now(),
    );
    let mut queued = model.init_state();
    assert!(model.fire("Control", &mut queued));
    assert!(model.fire("QueuePriority", &mut queued));
    assert!(
        !may_take_replay_page(&mailbox, false, 0),
        "a pushed opt-in cannot fetch ahead of queued mail"
    );
    assert!(may_take_replay_page(&mailbox, true, 0));
    assert!(model.action_enabled("FetchA", &queued));
    assert!(model.fire("FetchA", &mut queued));
    assert_eq!(
        may_take_replay_page(&mailbox, true, 1),
        model.action_enabled("FetchB", &queued),
        "the roster yields after one page"
    );
    assert!(matches!(
        mailbox.take(Duration::ZERO, Instant::now()),
        Some(Item::Inbox(..))
    ));
    assert!(model.fire("HandlePriority", &mut queued));
    assert_eq!(
        may_take_replay_page(&mailbox, false, 0),
        model.action_enabled("FetchB", &queued),
        "replay resumes once addressed mail was handled"
    );
    let _ = mailbox.push(
        Source::Fleet,
        (2, "halt".to_string(), Vec::new()),
        None,
        0,
        Instant::now(),
    );
    assert!(
        !may_take_replay_page(&mailbox, true, 0),
        "even the roster's one-page exception must yield to a halt"
    );

    let quiet = Mailbox::default();
    let mut pushed = model.init_state();
    assert!(model.fire("Control", &mut pushed));
    assert!(model.fire("BeginPush", &mut pushed));
    assert_eq!(
        may_take_replay_page(&quiet, false, 0),
        model.action_enabled("FetchA", &pushed)
    );
    assert!(model.fire("FetchA", &mut pushed));
    assert_eq!(
        may_take_replay_page(&quiet, false, 1),
        model.action_enabled("FetchB", &pushed),
        "a pushed add cannot use a second page in the same event"
    );
    assert!(model.fire("EndPush", &mut pushed));
    assert!(model.fire("BeginPush", &mut pushed));
    assert_eq!(
        may_take_replay_page(&quiet, false, 0),
        model.action_enabled("FetchB", &pushed)
    );
}

#[test]
fn many_backlogged_sessions_get_one_turn_before_any_get_a_second() {
    let now = Instant::now();
    let mut replay = SayReplay::new(now);
    let owed: BTreeSet<String> = (0..12).map(|n| format!("s-{n:02}")).collect();
    let mut served = Vec::new();
    for round in 0..3 {
        replay.refill(now + roster_refresh() * round);
        for _ in 0..SAY_REPLAY_PAGES {
            served.push(replay.take(&owed).expect("a page remains"));
        }
        assert!(
            replay.take(&owed).is_none(),
            "round {round} exceeds its Fetch bound"
        );
    }
    assert_eq!(served.into_iter().collect::<BTreeSet<_>>(), owed);

    let mut reduced = owed;
    reduced.remove("s-11");
    replay.refill(now + roster_refresh() * 3);
    assert_eq!(replay.take(&reduced).as_deref(), Some("s-00"));
}

#[test]
fn bridge_roster_accepts_only_complete_unique_identity_rows() {
    let a = "0123456789abcdef0123456789abcdef";
    let b = "fedcba9876543210fedcba9876543210";
    let rows = [
        format!("9 s-nine nonce={a}"),
        format!("2 s-two nonce={b} role=worker"),
    ];
    let (locals, epochs) = parse_bridge_roster(rows.iter().map(String::as_str)).unwrap();
    assert_eq!(locals.get(&2).map(String::as_str), Some("s-two"));
    assert_eq!(locals.get(&9).map(String::as_str), Some("s-nine"));
    assert_eq!(epochs.get("s-two").map(String::as_str), Some(b));
    assert_eq!(epochs.get("s-nine").map(String::as_str), Some(a));

    for bad in [
        "2 s-two",
        "2 s-two nonce=-",
        "2 s-two nonce=FFFFFFFFFFFFFFFFFFFFFFFFFFFFFFFF",
        "2 s-two nonce=0123456789abcdef0123456789abcdef my title",
        "2 s-two nonce=0123456789abcdef0123456789abcdef =1",
        "2 title s-two nonce=0123456789abcdef0123456789abcdef",
        "not-a-local s-two nonce=0123456789abcdef0123456789abcdef",
    ] {
        assert!(parse_bridge_roster(std::iter::once(bad)).is_err(), "{bad}");
    }
    for duplicate in [
        [format!("2 s-two nonce={a}"), format!("2 s-new nonce={b}")],
        [format!("2 s-two nonce={a}"), format!("3 s-two nonce={b}")],
    ] {
        assert!(
            parse_bridge_roster(duplicate.iter().map(String::as_str)).is_err(),
            "duplicate local id or sid cannot be admitted"
        );
    }
}

#[test]
fn status_batch_keeps_each_session_and_incarnation_separate() {
    let locals = BTreeMap::from([
        (1, "s-one".to_string()),
        (2, "s-two".to_string()),
        (3, "s-three".to_string()),
    ]);
    let epochs = BTreeMap::from([
        ("s-one".to_string(), "a1".to_string()),
        ("s-two".to_string(), "b2".to_string()),
        ("s-three".to_string(), "c3".to_string()),
    ]);
    let rows = [
        "1 s-one a1 sid=1 revision=7 hold=1 hold_origin=local detail=Claude%20Code \
         agent=prompt role=worker attention=stuck user_title=%2D gen=3.41",
        "2 s-two b2 sid=2 revision=4 hold=0 hold_origin=- detail=- agent=- gen=-",
        "3 s-three NEW sid=3 revision=99 hold=0 detail=new",
    ];
    let parsed = parse_batch_rows(rows.into_iter(), &locals, &epochs);
    assert!(matches!(
        parsed.get(&1),
        Some(BatchSample::Ready(StatusSample {
            hold: EndpointHold::Local,
            detail,
            agent: Some(agent),
            agent_deferred: false,
            screen: Some(ScreenGen { epoch: 3, seq: 41 }),
            meta: Some(meta),
        })) if detail == "Claude%20Code" && agent == "prompt"
            && meta == "role=worker attention=stuck user_title=%2D"
    ));
    assert!(
        matches!(
            parsed.get(&2),
            Some(BatchSample::Ready(StatusSample {
                hold: EndpointHold::Free,
                detail,
                agent: Some(agent),
                agent_deferred: false,
                screen: None,
                meta: None,
            })) if detail == "-" && agent == "-"
        ),
        "a row that left the meaning fields out is still Ready, and samples none"
    );
    assert!(matches!(parsed.get(&3), Some(BatchSample::Stale)));

    let wrong_sid = ["2 s-other b2 sid=2 revision=88 hold=1 detail=other"];
    let parsed = parse_batch_rows(wrong_sid.into_iter(), &locals, &epochs);
    assert!(matches!(parsed.get(&2), Some(BatchSample::Stale)));
}

#[test]
fn batch_agent_deferral_keeps_the_verdict_and_malformed_rows_retry_singly() {
    let locals = BTreeMap::from([(1, "s-one".to_string())]);
    let epochs = BTreeMap::from([("s-one".to_string(), "a1".to_string())]);
    let row = "1 s-one a1 sid=1 revision=7 hold=1 detail=Claude%20Code agent_deferred=1 gen=1.9";
    let mut parsed = parse_batch_rows(std::iter::once(row), &locals, &epochs);
    let Some(BatchSample::Ready(sample)) = parsed.remove(&1) else {
        panic!("a deferred agent is a usable hold/detail sample");
    };
    assert_eq!(sample.hold, EndpointHold::Unsaid);
    assert_eq!(sample.agent, None);
    assert!(sample.agent_deferred);
    let named = crate::presence::gen_token(ScreenGen { epoch: 1, seq: 9 });
    let mut slot = Slot::default();
    slot.fields.set_agent(Some("prompt"));
    sample.apply_presence(&mut slot);
    assert_eq!(slot.fields.detail, "Claude%20Code");
    assert_eq!(slot.fields.phase, "prompt", "a busy writer is not agent=-");
    assert_eq!(slot.row_gen(), named);
    StatusSample::default().apply_presence(&mut slot);
    assert_eq!(
        slot.fields.phase, "-",
        "an old single-status reply without agent= still clears the phase"
    );
    assert_eq!(
        slot.row_gen(),
        named,
        "a read that names no generation keeps the one before"
    );
    assert!(
        !slot.sampled,
        "a sample that carried no meaning fields samples none"
    );
    let row = "1 s-one a1 sid=1 revision=7 hold=0 detail=- agent=idle role=worker \
               attention=stuck user_title=lead gen=1.9";
    let Some(BatchSample::Ready(sample)) =
        parse_batch_rows(std::iter::once(row), &locals, &epochs).remove(&1)
    else {
        panic!("a row with its meaning fields is Ready");
    };
    sample.apply_presence(&mut slot);
    assert!(slot.sampled, "a row's meaning fields sample the slot");
    let mut read = crate::presence::Fields::default();
    read.read_meta(
        "OK title=- user_title=lead description=- icon=- role=worker attention=stuck cwd=- \
         state=alive",
    );
    assert_eq!(
        (
            &slot.fields.role,
            &slot.fields.attention,
            &slot.fields.title
        ),
        (&read.role, &read.attention, &read.title),
        "the row's fields fold as `@<sid> meta`'s header does"
    );

    for malformed in [
        "1 s-one a1 sid=1 revision=7 hold=1 detail=- agent_deferred=1 agent=idle gen=-",
        "1 s-one a1 sid=1 revision=7 hold=1 detail=- agent_deferred=1 agent_deferred=1 gen=-",
        "1 s-one a1 sid=1 revision=7 hold=1 detail=- agent_deferred=0 gen=-",
        "1 s-one a1 sid=1 revision=7 hold=1 detail=Claude Code agent=prompt gen=-",
        "1 s-one a1 sid=1 revision=4 hold=0 agent=idle gen=-",
        "1 s-one a1 sid=1 revision=7 hold=1 detail=- gen=-",
        "1 s-one a1 sid=1 revision=7 hold=1 detail=- agent=idle",
        "1 s-one a1 sid=1 revision=7 hold=1 detail=- agent=idle gen=- gen=1.2",
        "1 s-one a1 sid=1 revision=7 hold=1 detail=- agent=idle gen=1",
        "1 s-one a1 sid=1 revision=7 hold=1 detail=- agent=idle gen=1.2:ab",
        "1 s-one a1 sid=1 revision=7 hold=1 detail=- agent=idle gen=",
        "1 s-one a1 sid=1 revision=7 hold=1 detail=- agent=idle role=- gen=-",
        "1 s-one a1 sid=1 revision=7 hold=1 detail=- agent=idle role=- attention=- gen=-",
        "1 s-one a1 sid=1 revision=7 hold=1 detail=- agent=idle role=- attention=- \
         user_title=- role=lead gen=-",
        "1 s-one a1 sid=1 revision=7 hold=1 detail=- agent=idle role=- attention=- \
         user_title=- title=x gen=-",
    ] {
        assert!(
            parse_batch_rows(std::iter::once(malformed), &locals, &epochs).is_empty(),
            "a malformed row must retry singly: {malformed}"
        );
    }
}

#[test]
fn a_batch_row_says_whose_hold_stands() {
    let locals = BTreeMap::from([(1, "s-one".to_string())]);
    let epochs = BTreeMap::from([("s-one".to_string(), "a1".to_string())]);
    let parse = |fields: &str| {
        let row = format!("1 s-one a1 sid=1 revision=7 {fields} detail=- agent=- gen=-");
        match parse_batch_rows(std::iter::once(row.as_str()), &locals, &epochs).remove(&1) {
            Some(BatchSample::Ready(sample)) => Some(sample.hold),
            Some(BatchSample::Stale) => panic!("{row}: not a reused id"),
            None => None,
        }
    };
    assert_eq!(parse("hold=1 hold_origin=fleet"), Some(EndpointHold::Fleet));
    assert_eq!(parse("hold=1 hold_origin=local"), Some(EndpointHold::Local));
    assert_eq!(
        parse("hold=1 hold_origin=person"),
        Some(EndpointHold::Local)
    );
    assert_eq!(
        parse("hold=1 hold_origin=person,fleet,local"),
        Some(EndpointHold::Both),
    );
    assert!(!EndpointHold::of(true, Some("person")).fleet());
    assert!(EndpointHold::of(true, Some("person,fleet")).fleet());
    assert_eq!(
        parse("hold=1 hold_origin=remote"),
        Some(EndpointHold::Local)
    );
    assert_eq!(
        parse("hold=1 hold_origin=remote,fleet"),
        Some(EndpointHold::Both)
    );
    assert_eq!(
        parse("hold=1 hold_origin=person,remote,fleet,local"),
        Some(EndpointHold::Both)
    );
    assert!(!EndpointHold::of(true, Some("remote")).fleet());
    assert!(EndpointHold::of(true, Some("remote,fleet")).fleet());
    assert_eq!(parse("hold=1 hold_origin=person,person"), None);
    assert_eq!(parse("hold=1 hold_origin=someone"), None);
    assert_eq!(
        parse("hold=1 hold_origin=fleet,local"),
        Some(EndpointHold::Both),
        "both slots stand"
    );
    assert_eq!(parse("hold=0 hold_origin=-"), Some(EndpointHold::Free));
    assert_eq!(parse("hold=1"), Some(EndpointHold::Unsaid));
    assert_eq!(parse("hold=0"), Some(EndpointHold::Free));
    for malformed in [
        "hold=1 hold_origin=-",
        "hold=0 hold_origin=local",
        "hold=0 hold_origin=fleet",
        "hold=1 hold_origin=owner",
        "hold=1 hold_origin=local hold_origin=local",
        "hold=0 hold_origin=fleet,local",
        "hold=1 hold_origin=local,fleet",
        "hold=1 hold_origin=fleet,fleet",
        "hold=1 hold_origin=fleet,",
        "hold=1 hold_origin=local,person",
        "hold=1 hold_origin=fleet,remote",
        "hold=1 hold_origin=remote,remote",
        "hold=0 hold_origin=remote",
    ] {
        assert_eq!(parse(malformed), None, "{malformed}");
    }
    let duplicate = [
        "1 s-one a1 sid=1 revision=7 hold=1 detail=- agent=prompt gen=-",
        "1 s-one a1 sid=1 revision=8 hold=0 detail=- agent=idle gen=-",
    ];
    let parsed = parse_batch_rows(duplicate.into_iter(), &locals, &epochs);
    assert!(
        matches!(parsed.get(&1), Some(BatchSample::Stale)),
        "a duplicate row fails closed"
    );
}

#[cfg(unix)]
#[test]
fn a_refused_status_batch_never_fans_out() {
    use std::io::{BufRead as _, Write as _};
    use std::os::unix::net::UnixListener;

    for batch_reply in [
        "ERR main thread did not answer within the placement deadline\n",
        "ERR usage: sessions\n",
    ] {
        let dir = crate::test_scratch_dir("batch");
        let scratch = dir.path();
        let socket = scratch.join("ctl.sock");
        let listener = UnixListener::bind(&socket).unwrap();
        let responder = std::thread::spawn(move || {
            let (mut stream, _) = aterm_uds::accept(
                &listener,
                aterm_uds::AcceptWait::Forever,
                aterm_uds::AcceptedMode::Blocking,
            )
            .unwrap();
            let mut reader = std::io::BufReader::new(stream.try_clone().unwrap());
            let mut line = String::new();
            reader.read_line(&mut line).unwrap();
            assert_eq!(line, "AUTH test\n");
            line.clear();
            reader.read_line(&mut line).unwrap();
            assert_eq!(line, "outbox\n");
            stream.write_all(b"OK 0\n").unwrap();

            let mut requests = Vec::new();
            loop {
                line.clear();
                if reader.read_line(&mut line).unwrap() == 0 {
                    break;
                }
                requests.push(line.trim_end().to_string());
                if line == "sessions status\n" {
                    stream.write_all(batch_reply.as_bytes()).unwrap();
                } else if line == "@s-four meta\n" {
                    stream
                        .write_all(format!("{REFUSED_BATCH_META}\n").as_bytes())
                        .unwrap();
                } else {
                    panic!("unexpected control request: {line}");
                }
            }
            requests
        });

        let mut bridge = Bridge::new(Config {
            fleet: "test".into(),
            broker: String::new(),
            cap_files: Vec::new(),
            state_dir: scratch.join("state").to_string_lossy().into_owned(),
            accept_from: Vec::new(),
            sock: Some(socket.to_string_lossy().into_owned()),
            token: Some("test".into()),
            presence: Mode::Meta,
            receipts: true,
            log: crate::mirror::LogMode::Off,
            mesh: None,
        })
        .unwrap();
        bridge.attachment = Attachment::Inherited;
        for (local, sid) in [(1, "s-one"), (2, "s-two"), (3, "s-three")] {
            bridge.locals.insert(local, sid.to_string());
            bridge
                .per_sid
                .epochs
                .insert(sid.to_string(), "nonce".to_string());
            bridge.per_sid.pending_admit.insert(sid.to_string());
            let fields = &mut bridge
                .per_sid
                .presence
                .entry(sid.to_string())
                .or_default()
                .fields;
            fields.set_detail(Some("old"));
            fields.set_agent(Some("prompt"));
        }
        // Two sessions the round publishes for, their rows already on the bus: s-four's
        // meaning fields unsampled since (a GAP cleared them), s-five's sampled.
        for (local, sid, sampled) in [(4, "s-four", false), (5, "s-five", true)] {
            bridge.locals.insert(local, sid.to_string());
            bridge
                .per_sid
                .epochs
                .insert(sid.to_string(), "nonce".to_string());
            let slot = bridge.per_sid.presence.entry(sid.to_string()).or_default();
            slot.fields.set_detail(Some("old"));
            slot.fields.set_agent(Some("prompt"));
            slot.fields.read_meta(REFUSED_BATCH_META);
            let _ = slot.mark_published(Mode::Meta, bridge.fx.now());
            slot.sampled = sampled;
        }
        bridge.sample_local_control();
        for sid in ["s-one", "s-two", "s-three", "s-four", "s-five"] {
            let fields = &bridge.per_sid.presence[sid].fields;
            assert_eq!(fields.detail, "old", "{batch_reply:?}");
            assert_eq!(fields.phase, "prompt", "{batch_reply:?}");
        }
        assert!(bridge.per_sid.presence["s-four"].sampled);
        drop(bridge);
        let requests = responder.join().unwrap();
        assert_eq!(
            requests,
            ["sessions status", "@s-four meta"],
            "{batch_reply:?}: no session's status, and the meta of only a session the round \
             publishes for whose slot no row sampled"
        );
    }
}

/// The `meta` header `a_refused_status_batch_never_fans_out`'s lane answers.
#[cfg(unix)]
const REFUSED_BATCH_META: &str = "OK title=- user_title=- description=- icon=- role=worker \
                                  attention=- cwd=- state=alive";

/// **QUEUED LOCAL EVENTS CANNOT ACCELERATE A FAILED BROKER'S RETRY**
/// (`reconnect-wait-refines-backoff-model`): the 64 events queued ahead of the back-off's
/// wait are each handled, and the wait still lasts the whole back-off and returns at its
/// deadline; a closed aterm lane queued behind 64 more ends the wait with no park at all.
/// The waits run on a clock the test holds ([`Fx::on_a_held_clock`], critic item E04e),
/// which handling an event does not move and the park that found nothing moves by the rest
/// of the back-off: the verdict is that sequence, never how many events a loaded machine
/// handles in a stretch of the wall clock.
///
/// NEGATIVE CONTROL: with an event ending the wait
/// ([`crate::sim::world::Mutant::EventEndsTheBackoff`]) the first event ends it, none of the
/// back-off waited.
#[test]
fn busy_local_events_cannot_accelerate_a_failed_broker_retry() {
    const BACKOFF: Duration = Duration::from_millis(250);
    let busy_wait = |fx: Fx| {
        let mailbox = Mailbox::default();
        for i in 0..64 {
            mailbox.push_event(format!("EVENT 1 title {i}"));
        }
        let start = fx.now();
        let mut seen = 0;
        let closed = wait_for_retry_or_aterm_close(&fx, &mailbox, BACKOFF, |_| seen += 1);
        (closed, seen, fx.now() - start)
    };
    let model = aterm_models_session::fabric_reconnect_backoff_model();
    let mut retry = model.init_state();
    for _ in 0..2 {
        assert!(model.fire("Event", &mut retry));
    }
    assert_eq!(retry["result"], 0, "local events do not enable a retry");
    assert_eq!(
        busy_wait(Fx::os().on_a_held_clock()),
        (false, 64, BACKOFF),
        "queued local events must not turn the reconnect backoff into a rapid dial loop: \
         each is handled, and the wait lasts the whole back-off"
    );
    assert!(model.fire("Deadline", &mut retry));
    assert_eq!(retry["result"], 1, "the real wait reached its deadline");
    assert_eq!(
        busy_wait(Fx::os_without(crate::sim::world::Mutant::EventEndsTheBackoff).on_a_held_clock()),
        (false, 1, Duration::ZERO),
        "NEGATIVE CONTROL: the first event ended the wait, none of the back-off waited"
    );

    let fx = Fx::os().on_a_held_clock();
    let mailbox = Mailbox::default();
    let mut closed = model.init_state();
    for i in 0..64 {
        mailbox.push_event(format!("EVENT 1 bell {i}"));
        if i < 2 {
            assert!(model.fire("Event", &mut closed));
        }
    }
    mailbox.push_aterm_closed();
    let start = fx.now();
    assert!(wait_for_retry_or_aterm_close(
        &fx,
        &mailbox,
        aterm_spec::wait::MUST_HAPPEN,
        |_| {}
    ));
    assert_eq!(
        fx.now(),
        start,
        "a closed aterm lane must interrupt the backoff promptly: the wait parked none of it"
    );
    assert!(model.fire("Closed", &mut closed));
    assert_eq!(closed["result"], 2);
    assert!(!model.action_enabled("Deadline", &closed));
}

#[test]
fn idle_bridge_arms_only_the_earliest_deadline_and_retries_failed_due_work() {
    let now = Instant::now();
    let roster = now + ROSTER_REFRESH;
    let model = aterm_models_core::idle_deadline_model();
    let mut arm = model.init_state();
    let no_asks = deadline_wake(now, 1_000, None, roster, false);
    assert_eq!(no_asks, None);
    assert_eq!(
        idle_wait(now, roster, no_asks, None),
        ROSTER_REFRESH,
        "no ask or ghost needs the old 250 ms wake"
    );
    let far = deadline_wake(now, 1_000, Some(3_601_000), roster, false).unwrap();
    assert_eq!(far, roster, "a far ask shares the two-second roster wake");
    assert!(model.fire("ArmFar", &mut arm));
    assert_eq!(arm["armed"], 2);
    let near = deadline_wake(now, 1_000, Some(1_010), roster, false).unwrap();
    assert_eq!(near, now + Duration::from_millis(10));
    assert!(model.fire("ArmNear", &mut arm));
    assert_eq!(arm["armed"], 1);
    assert!(model.check_invariant("EarliestArmed", &arm));
    assert_eq!(
        idle_wait(now, roster, Some(far.min(near)), None),
        Duration::from_millis(10),
        "a new earlier ask moves the arm forward immediately"
    );
    let mut keep_first = arm.clone();
    keep_first.insert("armed", 2);
    assert!(
        !model.check_invariant("EarliestArmed", &keep_first),
        "a stale far arm after a near ask is the caught negative control"
    );
    assert_eq!(
        deadline_wake(now, 1_000, Some(1_000), roster, false),
        Some(now),
        "an already-due ask is checked on the next loop iteration"
    );
    assert_eq!(
        deadline_wake(now, 1_000, Some(1_000), roster, true),
        Some(now + DEADLINE_RETRY),
        "a failed due check backs off instead of spinning"
    );
    assert_eq!(
        deadline_wake(
            roster,
            3_601_000,
            Some(3_601_000),
            roster + ROSTER_REFRESH,
            false
        ),
        Some(roster),
        "the roster round notices a forward wall-clock jump"
    );
    assert_eq!(
        idle_wait(now, roster, None, Some(now + Duration::from_millis(100))),
        Duration::from_millis(100),
        "a ghost sweep due sooner than the roster still wakes on time"
    );
    assert_eq!(idle_wait(now, now, None, None), Duration::ZERO);
}

#[cfg(unix)]
#[test]
fn bridge_deadlines_restore_then_move_the_shipping_arm_from_far_to_near() {
    let dir = aterm_tempfile::Builder::new()
        .prefix("al-dl")
        .suffix(".noindex")
        .tempdir_in("/tmp")
        .expect("the fixture dir");
    let state_path = dir.path().join("state");
    StateDir::open(&state_path)
        .unwrap()
        .set_deadlines_as_an_older_build(&[Deadline {
            off: 1,
            sid: "s-one".into(),
            at: 0,
            dl: 1_000,
            to: None,
        }])
        .unwrap();
    let (_lane, mut bridge) = FakeVerbLane::bridge_with_state(&[], Some(&state_path));
    assert!(
        bridge.deadlines.contains_key(&1),
        "the old ask was restored"
    );
    bridge.roster_due = Instant::now() + Duration::from_secs(60);
    bridge.arm_deadline_sweep(true, false);
    assert!(
        bridge.deadline_due <= Instant::now(),
        "run's startup arm must check a restored overdue ask immediately"
    );

    bridge.deadlines.clear();
    bridge.note_deadline("s-one", 2, 3_600_000, None);
    let far = bridge.deadline_due;
    bridge.note_deadline("s-one", 3, 0, None);
    assert!(
        bridge.deadline_due < far,
        "a nearer ask replaces the far arm"
    );
}

#[test]
fn moving_irrelevant_cursors_get_one_checkpoint_per_256_broadcasts() {
    let mut topics = (0..64)
        .map(|i| {
            (
                format!("s-{i:03}"),
                BTreeMap::from([("quiet".to_string(), TopicCursor { next: 10 })]),
            )
        })
        .collect::<BTreeMap<_, _>>();
    let mut counts = BTreeMap::<String, usize>::new();
    for tick in 1..=SAY_CURSOR_CHECKPOINT_SHARDS {
        let checkpoint = advance_frontier_cursors(
            &mut topics,
            &BTreeMap::new(),
            9 + tick,
            10 + tick,
            tick % SAY_CURSOR_CHECKPOINT_SHARDS,
        );
        assert!(checkpoint.len() <= 1, "tick {tick}: {checkpoint:?}");
        for sid in checkpoint {
            *counts.entry(sid).or_default() += 1;
        }
    }
    assert!(topics.values().all(|t| t["quiet"].next == 266));
    assert!(
        topics.keys().all(|sid| counts.get(sid) == Some(&1)),
        "each continuously moving cursor was checkpointed exactly once: {counts:?}"
    );
}

#[test]
fn only_this_incarnation_its_dirs_earlier_ones_and_a_witnessed_dead_one_are_adoptable() {
    let (a_live, b) = (1, 2);
    assert!(
        !adoptable(Some(a_live), b, None, false),
        "an older sibling that is still running had its sessions retired"
    );
    assert!(adoptable(Some(b), b, None, true));
    assert!(
        !adoptable(Some(b), b, None, false),
        "a sibling can concurrently take the same incarnation: its writer mark is required"
    );
    assert!(adoptable(Some(a_live), b, Some(a_live), false));
    assert!(
        !adoptable(Some(1), 3, Some(2), false),
        "`inc <= witnessed` is the generous reading, and it is not proven"
    );
    assert!(adoptable(Some(1), 3, None, true));
    assert!(
        !adoptable(Some(1), 3, None, false),
        "a number this dir may have taken too says nothing of whose row it is"
    );
    assert!(!adoptable(Some(9), b, None, false));
    assert!(!adoptable(Some(9), b, Some(a_live), true));
    assert!(!adoptable(None, b, Some(a_live), true));
    assert!(!adoptable(None, b, None, false));
}

#[test]
fn a_bus_incarnation_past_the_will_space_is_not_adopted() {
    let inc = |raw: &str| presence_inc(&Body::decode(raw.as_bytes()).0);
    assert_eq!(inc("v=1 t=1 state=gone inc=7"), Some(7));
    assert_eq!(
        inc(&format!("v=1 t=1 state=live inc={INC_MAX}")),
        Some(INC_MAX)
    );
    assert_eq!(
        inc(&format!("v=1 t=1 state=gone inc={}", INC_MAX + 1)),
        None
    );
    assert_eq!(inc("v=1 t=1 state=gone inc=3000000000"), None);
    assert_eq!(inc(&format!("v=1 t=1 state=gone inc={}", u64::MAX)), None);
    assert_eq!(inc("v=1 t=1 state=gone inc=x"), None);

    use crate::state::next_incarnation;
    assert_eq!(next_incarnation(5, 3), Some(6));
    assert_eq!(next_incarnation(0, INC_MAX - 1), Some(INC_MAX));
    assert_eq!(next_incarnation(INC_MAX, 0), None, "a corrupt local inc");
    assert_eq!(next_incarnation(u64::MAX, 0), None, "checked, not wrapped");
}

#[test]
fn every_per_sid_map_is_pruned_to_the_roster() {
    const LIVE: &str = "s-live";
    const GONE: &str = "s-gone";
    let mut maps = PerSession::default();
    {
        let PerSession {
            epochs,
            pending_admit,
            topics,
            unhandled,
            topic_floors_dirty,
            handled_to,
            topics_sampled,
            watched,
            presence,
            lane_owed,
            given_up_told,
        } = &mut maps;
        for sid in [LIVE, GONE] {
            let sid = sid.to_string();
            given_up_told.insert((sid.clone(), String::new()), 1);
            epochs.insert(sid.clone(), "e".to_string());
            pending_admit.insert(sid.clone());
            topics.insert(sid.clone(), BTreeMap::new());
            unhandled.insert(sid.clone(), BTreeMap::new());
            topic_floors_dirty.insert(sid.clone());
            handled_to.insert(sid.clone(), 1);
            topics_sampled.insert(sid.clone());
            watched.insert(sid.clone(), 1);
            presence.insert(sid.clone(), Slot::default());
            lane_owed.insert(sid, None);
        }
    }
    maps.prune_to(&BTreeSet::from([LIVE.to_string()]));
    let PerSession {
        epochs,
        pending_admit,
        topics,
        unhandled,
        topic_floors_dirty,
        handled_to,
        topics_sampled,
        watched,
        presence,
        lane_owed,
        given_up_told,
    } = &maps;
    let keys: [(&str, Vec<&String>); 11] = [
        (
            "given_up_told",
            given_up_told.keys().map(|(sid, _)| sid).collect(),
        ),
        ("epochs", epochs.keys().collect()),
        ("pending_admit", pending_admit.iter().collect()),
        ("topics", topics.keys().collect()),
        ("unhandled", unhandled.keys().collect()),
        ("topic_floors_dirty", topic_floors_dirty.iter().collect()),
        ("handled_to", handled_to.keys().collect()),
        ("topics_sampled", topics_sampled.iter().collect()),
        ("watched", watched.keys().collect()),
        ("presence", presence.keys().collect()),
        ("lane_owed", lane_owed.keys().collect()),
    ];
    for (name, sids) in keys {
        assert_eq!(
            sids,
            [LIVE],
            "`{name}` must keep the live session and drop the departed one"
        );
    }
}

#[test]
fn the_node_row_carries_a_real_host_name() {
    let askable = std::path::Path::new("/usr/bin/uname").exists()
        || std::path::Path::new("/bin/uname").exists()
        || std::path::Path::new("/etc/hostname").exists();
    if !askable {
        aterm_spec::host::could_not_run(
            "no uname and no /etc/hostname: this host cannot be asked its name",
        );
    }
    let host = hostname();
    assert_ne!(
        host, "-",
        "the host name is readable on this machine, so the presence row must carry it"
    );
    assert!(
        !host.is_empty() && !host.contains(char::is_whitespace),
        "{host:?}"
    );
}

#[test]
fn a_drain_frame_carries_receipts_and_fetches_after_the_posts() {
    let frame = b"post sid=s-a id=1 to=%40s-b kind=ask re=7 len=8\nline\none\
                  receipt sid=s-a rid=3 off=40 verdict=handled kind=ask from=s-z@n-q\n\
                  receipt sid=bad rid=4 off=40 verdict=handled kind=ask from=s-z@n-q\n\
                  receipt sid=s-a rid=0 off=40 verdict=handled kind=ask from=s-z@n-q\n\
                  receipt sid=s-a rid=5 off=40 kind=ask from=s-z@n-q\n\
                  receipt sid=s-a rid=6 off=12 verdict=deferred kind=task from=h-x \
                  log=earlier\n\
                  receipt sid=s-a rid=7 off=12 verdict=deferred kind=task from=h-x log=a.0\n\
                  fetch sid=s-a off=41\nfetch sid=s-b off=7\nfetch sid=bad off=9\n\
                  more\n";
    let drain = parse_drain(frame);
    assert_eq!(drain.posts.len(), 1);
    assert!(drain.more, "the last line says the listing was cut");
    assert_eq!(
        drain.posts[0].body, b"line\none",
        "a body may hold newlines"
    );
    assert_eq!(drain.posts[0].re, Some(7));
    assert!(parse_drain(b"post sid=s-a id=1 to=x kind=note len=99\nhi")
        .posts
        .is_empty());
    let owed = OwedReceipt {
        sid: "s-a".to_string(),
        rid: 3,
        off: 40,
        verdict: "handled".to_string(),
        kind: "ask".to_string(),
        from: "s-z@n-q".to_string(),
        earlier_log: false,
    };
    let earlier = OwedReceipt {
        rid: 6,
        off: 12,
        verdict: "deferred".to_string(),
        kind: "task".to_string(),
        from: "h-x".to_string(),
        earlier_log: true,
        ..owed.clone()
    };
    let unknown = OwedReceipt {
        rid: 7,
        earlier_log: false,
        ..earlier.clone()
    };
    assert_eq!(
        drain.receipts,
        vec![owed, earlier, unknown],
        "a bad sid, a zero rid and a missing verdict are each dropped; `log=earlier` is \
         read, and no other `log=` word means it"
    );
    assert_eq!(
        drain.fetches,
        vec![("s-a".to_string(), 41), ("s-b".to_string(), 7)],
        "a sid that is not a principal is dropped, never spliced into a filter"
    );
    let ahead = b"fetch sid=s-a off=41\npost sid=s-a id=1 to=%40s-b kind=note len=0\n";
    let drain = parse_drain(ahead);
    assert_eq!((drain.posts.len(), drain.fetches.len()), (1, 1));
    assert!(!drain.more, "no `more` line, a whole listing");
}

/// Rules that keep every record of the lanes in one class and another lane's last value
/// only in a second (`first`), or one class for every lane, which reaches both and so is
/// whole only from the base (astream's `retention::carried_since`: a last-value class
/// carries none of its records): set after a cut under the first, the lanes' oldest lies
/// above the mail the cut carried.
fn lanes_whole_from_the_base(first: bool) -> astream_broker::Retention {
    use crate::broker_rules::{EV_FILTER, MAIL_FILTER};
    use astream_broker::{Retention, Rule};
    let mut classes = vec![Rule::prefix(EV_FILTER).unwrap()];
    if first {
        classes.push(Rule::hold("/f/*/in/*/s-z/>").unwrap().last_only());
    }
    classes.push(Rule::hold(MAIL_FILTER).unwrap());
    Retention::new(classes, Rule::default_keeping_all()).unwrap()
}

/// `lanes_whole_from_the_base` for the broadcast topics, rules no aterm build sets: every
/// topic's broadcasts in one class, and (`first`) another sender's last value only in a
/// second, so set after a cut under the first, a topic's oldest lies above the broadcasts
/// the cut carried.
fn topics_whole_from_the_base(first: bool) -> astream_broker::Retention {
    use crate::broker_rules::EV_FILTER;
    use astream_broker::{Retention, Rule};
    let mut classes = vec![Rule::prefix(EV_FILTER).unwrap()];
    if first {
        classes.push(Rule::hold("/f/*/pub/n-z/*/say/>").unwrap().last_only());
    }
    classes.push(Rule::hold("/f/*/pub/*/*/say/>").unwrap());
    Retention::new(classes, Rule::default_keeping_all()).unwrap()
}

/// **A FETCH OF A RECORD HELD BELOW ITS LANE'S OLDEST IS ANSWERED WITH THE RECORD**
/// (finding F3), on a real broker: a cut carries the session's unread mail below the base,
/// and the rules set next take its class whole from the base alone, so the lane's oldest
/// lies above the record the broker still holds. The fetch's read is refused there, read
/// again told, and answered with the record. CONTROL (`Mutant::ReadAtWithoutTheHeld`, the
/// refusal taken for no rows, as before F3): `err=retained` for a record the broker holds.
#[cfg(unix)]
#[test]
fn a_fetch_of_a_record_held_below_its_lanes_oldest_answers_the_record() {
    const SID: &str = "s-kept";
    for control in [false, true] {
        let (lane, mut bridge, _server, broker) =
            rig_retained_by(&[(1, SID)], Some(lanes_whole_from_the_base(true)));
        if control {
            bridge.fx = crate::fx::Fx::os_without(crate::sim::world::Mutant::ReadAtWithoutTheHeld);
        }
        let node = bridge.node.clone();
        let subject = format!("/f/f/in/{node}/{SID}/{node}/note");
        let held = bridge
            .publish_own_off(&subject, b"v=1 t=1 from=s-held text=held")
            .unwrap();
        let after = bridge
            .publish_own_off(&subject, b"v=1 t=1 from=s-held text=after")
            .unwrap();
        broker.retain_before_carrying_state(after + 1).unwrap();
        broker
            .set_retention(lanes_whole_from_the_base(false))
            .unwrap();
        let of_lane = astream_wire::Filter::new(format!("/f/f/in/{node}/{SID}/>")).unwrap();
        assert!(
            broker.oldest(&of_lane) > held,
            "PRECONDITION: the lane's oldest lies above the record the broker holds"
        );
        bridge.answer_fetch(SID, held);
        let head = format!("deliver {SID} fetched={held} ");
        let answer = lane
            .deliveries
            .lock()
            .unwrap()
            .iter()
            .find_map(|l| l.strip_prefix(&head).map(str::to_string))
            .unwrap_or_else(|| panic!("no answer for {held}"));
        if control {
            assert!(
                answer.starts_with("err=retained floor="),
                "CONTROL: answered `retained` for the record the broker holds: {answer}"
            );
        } else {
            assert!(
                answer.starts_with("from=") && answer.ends_with("text=held"),
                "the record held below the oldest: {answer}"
            );
        }
    }
}

/// **A FETCH WHOSE TOLD READ CANNOT BE MADE ANSWERS `unreadable` AND KEEPS THE LINK** (F3
/// review), on a real broker: a record held below its lane's oldest, as above, with the
/// broker at its connection cap, so the told read's connection of its own is refused while
/// the link stands. A read-only fetch keeps no post's fate unknown: it answers
/// `err=unreadable` and the link stays up, as a plain refusal leaves it (judged as the
/// link's read, the failed dial dropped the link of every session at each `inbox get` of a
/// held or cut offset). CONTROL: the cap lifted, the same fetch answers the record.
#[cfg(unix)]
#[test]
fn a_fetch_whose_told_read_cannot_be_dialled_answers_unreadable_and_keeps_the_link() {
    const SID: &str = "s-kept";
    for capped in [true, false] {
        let (lane, mut bridge, _server, broker) =
            rig_retained_by(&[(1, SID)], Some(lanes_whole_from_the_base(true)));
        let node = bridge.node.clone();
        let subject = format!("/f/f/in/{node}/{SID}/{node}/note");
        let held = bridge
            .publish_own_off(&subject, b"v=1 t=1 from=s-held text=held")
            .unwrap();
        let after = bridge
            .publish_own_off(&subject, b"v=1 t=1 from=s-held text=after")
            .unwrap();
        broker.retain_before_carrying_state(after + 1).unwrap();
        broker
            .set_retention(lanes_whole_from_the_base(false))
            .unwrap();
        if capped {
            broker.set_max_conns(1);
        }
        bridge.answer_fetch(SID, held);
        let head = format!("deliver {SID} fetched={held} ");
        let answer = lane
            .deliveries
            .lock()
            .unwrap()
            .iter()
            .find_map(|l| l.strip_prefix(&head).map(|a| a.trim_end().to_string()))
            .unwrap_or_else(|| panic!("no answer for {held}"));
        if capped {
            assert_eq!(answer, "err=unreadable", "no answer about the record yet");
        } else {
            assert!(
                answer.ends_with("text=held"),
                "CONTROL: the record held below the oldest: {answer}"
            );
        }
        assert!(
            bridge.conn.is_some() && bridge.link.up,
            "the link stands (capped: {capped})"
        );
    }
}

#[test]
fn a_fetch_below_the_lanes_retained_floor_names_the_floor() {
    const SID: &str = "s-kept";
    let (lane, mut bridge, _server, broker) = rig_with_kept_broker(&[(1, SID)], false);
    let node = bridge.node.clone();
    let subject = format!("/f/f/in/{node}/{SID}/{node}/note");
    let gone = bridge
        .publish_own_off(&subject, b"v=1 t=1 from=s-gone text=first")
        .unwrap();
    let kept = bridge
        .publish_own_off(&subject, b"v=1 t=1 from=s-gone text=second")
        .unwrap();
    assert!(
        broker.retain_before_carrying_state(kept).unwrap() > 0,
        "the cut"
    );

    bridge.answer_fetch(SID, gone);
    bridge.answer_fetch(SID, kept);
    bridge.answer_fetch(SID, kept + 1);
    let lines = lane.deliveries.lock().unwrap().clone();
    let first = |off: u64| {
        let head = format!("deliver {SID} fetched={off} ");
        lines
            .iter()
            .find_map(|l| l.strip_prefix(&head))
            .unwrap_or_else(|| panic!("no answer for {off}: {lines:?}"))
            .to_string()
    };
    let floor: u64 = first(gone)
        .strip_prefix("err=retained floor=")
        .unwrap_or_else(|| panic!("the lane's floor: {lines:?}"))
        .parse()
        .expect("a floor");
    assert!(gone < floor && floor <= kept, "{gone} < {floor} <= {kept}");
    let whole = first(kept);
    assert!(
        whole.starts_with("from=") && whole.ends_with("text=second"),
        "{lines:?}"
    );
    assert_eq!(first(kept + 1), "err=no-record");
}

#[test]
fn fetched_lines_carry_a_body_whole_in_chunks_and_cut_one_over_the_bound() {
    let fields = "from=s-a@n-b kind=note trust=agent";
    let decode = |lines: &[String]| -> String {
        let mut body = String::new();
        for (i, line) in lines.iter().enumerate() {
            let at: usize = line
                .split_whitespace()
                .find_map(|t| t.strip_prefix("at="))
                .map_or(0, |n| n.parse().expect("at="));
            assert_eq!(at, body.len(), "chunk {i} starts where the last ended");
            assert_eq!(
                line.contains(" more=1 "),
                i + 1 < lines.len(),
                "more=1 on every line but the last"
            );
            assert!(line.len() <= REQUEST_LINE_MAX, "chunk {i} fits one line");
            let text = line.split(" text=").nth(1).expect("text=");
            body.push_str(&crate::pct::decode(text));
        }
        body
    };
    let small = fetched_lines("s-x", 7, fields, "hello world").expect("small");
    assert_eq!(
        small,
        vec!["deliver s-x fetched=7 from=s-a@n-b kind=note trust=agent text=hello%20world"]
    );
    let mut text = "x ".repeat(19_999);
    text.push('x');
    let lines = fetched_lines("s-x", 4, fields, &text).expect("chunked");
    assert!(lines.len() > 1, "{} line(s)", lines.len());
    assert!(
        !lines.iter().any(|l| l.contains(" len=")),
        "not cut, so no len="
    );
    assert_eq!(decode(&lines), text, "whole, byte for byte");
    let wide = "é🙂".repeat(20_000);
    let lines = fetched_lines("s-x", 5, fields, &wide).expect("wide");
    assert!(lines.len() > 1);
    assert_eq!(decode(&lines), wide);
    let big = format!("{}é", "z".repeat(FETCH_BODY_MAX - 1));
    let lines = fetched_lines("s-x", 6, fields, &big).expect("big");
    let got = decode(&lines);
    assert_eq!(
        got.len(),
        FETCH_BODY_MAX - 1,
        "cut before the split character"
    );
    assert!(lines
        .iter()
        .all(|l| l.contains(&format!(" len={} ", big.len()))));
    let huge = "x".repeat(REQUEST_LINE_MAX);
    assert_eq!(fetched_lines("s-x", 8, &huge, "hi"), Err("oversize"));
}

#[test]
fn a_configs_debug_never_prints_the_instance_token() {
    let cfg = Config {
        fleet: "f".into(),
        broker: "/tmp/b.sock".into(),
        cap_files: vec!["/tmp/node.cap".into()],
        state_dir: "/tmp/state".into(),
        accept_from: Vec::new(),
        sock: None,
        token: Some("owner-token-0123".into()),
        presence: Mode::Meta,
        receipts: true,
        log: crate::mirror::LogMode::Off,
        mesh: None,
    };
    let printed = format!("{cfg:?}");
    assert!(
        !printed.contains("owner-token-0123") && printed.contains("<redacted>"),
        "{printed}"
    );
}

/// What a fleet subscription's reader pushes once a stand-in broker marked its tail caught
/// up and then hangs the stream up (`hang_up`: the reader's live read deadline is the hang
/// detector, so only the hang-up can end it) or holds it silent past the reader's 200 ms.
#[cfg(unix)]
fn lost_after_catch_up(hang_up: bool) -> Option<Item> {
    use astream_broker::proto::{
        decode_request, encode_response, read_frame, write_frame, Request, Response,
    };
    let dir = aterm_tempfile::Builder::new()
        .prefix("al-silent")
        .suffix(".noindex")
        .tempdir_in("/tmp")
        .expect("the fixture dir");
    let sock = dir.path().join("b.sock");
    let listener = std::os::unix::net::UnixListener::bind(&sock).expect("bind");
    let (release, held) = std::sync::mpsc::channel::<()>();
    let broker = std::thread::spawn(move || {
        let (mut c, _) = aterm_uds::accept(
            &listener,
            aterm_uds::AcceptWait::Forever,
            aterm_uds::AcceptedMode::Blocking,
        )
        .expect("accept");
        let mark = Response::Mark {
            next: 0,
            head: 0,
            resume: String::new(),
            durable: None,
            oldest: None,
        };
        for want in [Request::IdleMarks, Request::GapNotices] {
            let opt_in = read_frame(&mut c).expect("the opt-in").expect("a frame");
            assert_eq!(decode_request(&opt_in), Some(want));
            write_frame(&mut c, &encode_response(&mark)).expect("the opt-in's ack");
        }
        let subscribe = read_frame(&mut c).expect("the subscribe").expect("a frame");
        assert!(
            matches!(decode_request(&subscribe), Some(Request::Subscribe { .. })),
            "the subscription follows its opt-in"
        );
        write_frame(&mut c, &encode_response(&mark)).expect("the catch-up mark");
        if hang_up {
            drop(c);
        } else {
            let _ = held.recv();
        }
    });
    let (conn, closer) =
        transport::connect(&sock.to_string_lossy(), Duration::from_secs(60)).expect("connect");
    let sub = conn.subscribe(0, "/f/f/fleet/>").expect("subscribe");
    closer
        .set_read_timeout(None)
        .expect("lift the dial's bound");
    let mailbox = Arc::new(Mailbox::default());
    let deadline = if hang_up {
        aterm_spec::wait::MUST_HAPPEN
    } else {
        Duration::from_millis(200)
    };
    spawn_reader(
        sub,
        Arc::new(BusCloser::Os(closer)),
        deadline,
        Source::Fleet,
        Arc::clone(&mailbox),
    )
    .expect("the reader thread");
    let item = mailbox.take(aterm_spec::wait::MUST_HAPPEN, Instant::now());
    let _ = release.send(());
    broker.join().expect("the stand-in broker");
    item
}

/// A LINK THAT GOES SILENT ENDS ITS SUBSCRIPTION, AND SAYS SO: past the live read deadline
/// with nothing read, the reader pushes [`End::Silent`] with that deadline, never parks,
/// and never says the broker ended the stream ([`End::Eof`], the next test's).
///
/// PLANTED CONTROL (run, scratch-reverted): `reader_took` pushing every `Err` as
/// `End::Eof`, as the reader did before it named its end, fails here.
#[cfg(unix)]
#[test]
fn a_subscription_whose_link_goes_silent_is_closed_not_parked() {
    let item = lost_after_catch_up(false);
    assert!(
        matches!(
            item,
            Some(Item::Lost(Source::Fleet, End::Silent(d))) if d == Duration::from_millis(200)
        ),
        "a silent link ends the subscription, as silence: {item:?}"
    );
    let Some(Item::Lost(_, end)) = item else {
        unreachable!("matched above")
    };
    let said = end.said("fleet");
    assert!(
        said.contains(&span_of(200)) && !said.contains("broker"),
        "its line names the deadline and no end the broker made: {said}"
    );
}

/// A BROKER THAT HANGS THE STREAM UP ENDS IT AT END OF FILE ([`End::Eof`]), the one end
/// the bridge's line says the broker made.
///
/// PLANTED CONTROL (run, scratch-reverted): `reader_took` pushing `Ok(None)` as
/// `End::Silent`, fails here.
#[cfg(unix)]
#[test]
fn a_subscription_the_broker_hangs_up_ends_at_end_of_file() {
    let item = lost_after_catch_up(true);
    assert!(
        matches!(item, Some(Item::Lost(Source::Fleet, End::Eof))),
        "a hung-up stream ends at end of file: {item:?}"
    );
    let said = End::Eof.said("fleet");
    assert!(
        said.contains("broker"),
        "its line names the broker's end: {said}"
    );
}

#[test]
fn a_lost_verb_lane_reaches_the_loop_and_a_refused_line_does_not() {
    use std::os::unix::net::UnixStream;

    let (near, far) = UnixStream::pair().expect("socketpair");
    let mut ctl = Ctl::from_stream(near).expect("client");
    let mailbox = Mailbox::default();
    let err = ctl
        .request(&"x".repeat(REQUEST_LINE_MAX + 1))
        .expect_err("over the bound");
    note_ctl_loss(&ctl, &mailbox, Some(&err));
    assert!(
        mailbox
            .take(Duration::from_millis(0), Instant::now())
            .is_none(),
        "a refused line must not end the bridge"
    );
    drop(far);

    let (near, far) = UnixStream::pair().expect("socketpair");
    drop(far);
    let mut ctl = Ctl::from_stream(near).expect("client");
    let err = ctl.request("sessions").expect_err("the peer is gone");
    note_ctl_loss(&ctl, &mailbox, Some(&err));
    assert!(
        matches!(
            mailbox.take(Duration::from_millis(0), Instant::now()),
            Some(Item::Closed(Source::Aterm))
        ),
        "a lost verb lane must reach the loop as Closed(Aterm)"
    );
}

#[test]
fn a_bounded_encode_cuts_on_a_character_and_never_inside_an_escape() {
    let (out, cut) = encode_bounded("two words", 64);
    assert_eq!((out.as_str(), cut), ("two%20words", false));

    let text = "é β 漢 x".repeat(40);
    for budget in 0..64 {
        let (out, cut) = encode_bounded(&text, budget);
        assert!(out.len() <= budget, "budget {budget}: {out}");
        assert!(cut, "budget {budget} cannot hold the whole text");
        assert!(
            !out.ends_with('%') && !(out.len() >= 2 && out.as_bytes()[out.len() - 2] == b'%'),
            "budget {budget} split an escape: {out}"
        );
        assert!(
            text.starts_with(&crate::pct::decode(&out)),
            "budget {budget} did not cut on a character: {out}"
        );
    }
}

#[test]
fn the_self_lane_check_accuses_only_inside_the_window_it_remembers() {
    let acked: BTreeSet<u64> = [100, 200, 300].into_iter().collect();
    for floor in [100, 0] {
        assert!(!forged_self(&acked, floor, 200), "floor {floor}");
        assert!(forged_self(&acked, floor, 250), "floor {floor}");
        assert!(forged_self(&acked, floor, 9_000), "floor {floor}");
    }
    assert!(
        !forged_self(&acked, 100, 99),
        "below the floor: unknown, not accused"
    );
    assert!(!forged_self(&acked, 100, 0));
    assert!(
        forged_self(&acked, 0, 99),
        "a floor below the oldest offset held accuses between them"
    );
    assert!(forged_self(&BTreeSet::new(), 0, 7));
    assert!(
        !forged_self(&BTreeSet::new(), 12, 7),
        "below a move's floor"
    );
    assert!(forged_self(&BTreeSet::new(), 12, 12));
}

/// **A WINDOW LOADS AT THE FLOOR ITS FILE NAMES, AND AN OLDER BUILD'S AT ITS OLDEST OFFSET**
/// (critic item F17's review, `self_window`): a file this build wrote names its floor, which
/// loads apart from the offsets it holds; one an older build wrote names none and loads at
/// its oldest offset, the floor that build read off it, so it accuses nothing that build did
/// not; one that holds neither offset nor floor (no file, an unreadable one) names no floor,
/// which the attach takes from the head it reads (critic item F20); and past its keep the
/// window's floor rises to the oldest it keeps.
#[test]
fn a_window_loads_at_the_floor_its_file_names_and_an_older_builds_at_its_oldest_offset() {
    let log = |offsets: &[u64], floor: Option<u64>| crate::state::AckedLog {
        offsets: offsets.iter().copied().collect(),
        floor,
        entries: offsets.len(),
        appendable: true,
    };
    let set = |offsets: &[u64]| offsets.iter().copied().collect::<BTreeSet<u64>>();
    assert_eq!(self_window(log(&[], None)), (set(&[]), None), "no file");
    assert_eq!(
        self_window(log(&[40, 60], None)),
        (set(&[40, 60]), Some(40)),
        "an older build's file, at its oldest offset"
    );
    assert_eq!(
        self_window(log(&[40, 60], Some(0))),
        (set(&[40, 60]), Some(0)),
        "this build's, at the floor it names"
    );
    assert_eq!(
        self_window(log(&[], Some(12))),
        (set(&[]), Some(12)),
        "a move's"
    );
    let keep = SELF_ACK_KEEP as u64;
    let past: Vec<u64> = (10..=10 + keep).collect();
    for floor in [Some(0), None] {
        assert_eq!(
            self_window(log(&past, floor)),
            ((11..=10 + keep).collect(), Some(11)),
            "{floor:?}: past its keep, at the oldest kept"
        );
    }
}

#[test]
fn a_via_chain_is_bounded_by_grammar_and_by_length() {
    let hop = format!("n-{}", "a".repeat(32));
    assert_eq!(hop.len(), subject::PRINCIPAL_MAX);
    let worst_via = vec![hop; crate::body::VIA_MAX_HOPS].join(",");
    assert!(via_ok(&worst_via), "the longest chain the predicate admits");
    let worst_prefix = format!(
        "deliver {sid} off={off} from={sid}@{sid} kind=undeliverable trust=relayed \
         re={off} dl={off} demoted=undeliverable via={worst_via} len={off} text=",
        sid = "s-".to_string() + &"a".repeat(32),
        off = u64::MAX,
    );
    assert!(
        worst_prefix.len() < REQUEST_LINE_MAX,
        "the worst metadata prefix is {} bytes, against a {REQUEST_LINE_MAX}-byte bound",
        worst_prefix.len()
    );
}

#[test]
fn only_the_recipients_verdicts_book_a_refusal_and_each_reaches_the_bus_as_one_token() {
    assert_eq!(deliver_verdict("ERR quota", false), Some("quota"));
    assert_eq!(deliver_verdict("ERR too large", false), Some("too-large"));
    assert_eq!(
        deliver_verdict("ERR no such session", false),
        Some(Reject::NotHosted.token())
    );
    assert_eq!(deliver_verdict("ERR not-subscribed", false), None);
    assert_eq!(
        deliver_verdict("ERR not-subscribed", true),
        Some("not-subscribed")
    );
    for lane_fault in [
        "ERR usage: deliver <sid> off=<n> | deliver <sid> landed=<id>",
        "ERR unknown verb (try: help)",
        "ERR denied",
        "ERR quotas",
        "ERR no such sessionx",
        "ERR",
        "",
        "OK 3",
    ] {
        assert_eq!(deliver_verdict(lane_fault, false), None, "{lane_fault:?}");
        assert_eq!(deliver_verdict(lane_fault, true), None, "{lane_fault:?}");
    }
    assert_eq!(reason_token("ERR quota"), "quota");
    assert_eq!(reason_token("ERR"), "unknown");
    assert_eq!(reason_token(""), "unknown");
    for reply in [
        "ERR quota",
        "ERR no such session",
        "ERR usage: deliver <sid> off=<n> [via=<p,...>] | x",
        "ERR \u{1b}[31mred",
        "ERR a=b c=d",
    ] {
        let token = reason_token(reply);
        assert!(
            !token.is_empty()
                && token.len() <= REASON_TOKEN_MAX
                && token
                    .chars()
                    .all(|c| c.is_ascii_alphanumeric() || matches!(c, '-' | '_' | '.')),
            "{reply:?} became {token:?}"
        );
    }
}

#[test]
fn a_link_record_is_sent_on_change_on_a_2x_move_and_on_a_stale_report() {
    let t0 = Instant::now();
    let mut link = LinkReport::new();
    assert!(link.down_wants_report("refused"), "the first down is news");
    link.up = false;
    link.reason = "refused";
    link.reported_at = Some(t0);
    assert!(
        !link.down_wants_report("refused"),
        "a retry that fails the same way is not"
    );
    assert!(link.down_wants_report("no-socket"), "a different reason is");
    assert!(link.ack_wants_report(1, t0), "down to up is");

    let up = |rtt: u64| LinkReport {
        up: true,
        reason: "",
        rtt_ms: Some(rtt),
        reported_at: Some(t0),
    };
    assert!(up(2).down_wants_report("closed"), "up to down is");
    let ms = Duration::from_millis;
    for (was, rtt, after, wants, case) in [
        (2, 3, ms(300), false, "within 2x"),
        (2, 4, ms(300), false, "exactly 2x"),
        (2, 5, ms(300), true, "past 2x, past the gap"),
        (2, 5, ms(100), false, "past 2x, inside the gap"),
        (2, 1, LINK_MOVE_GAP, false, "a drop to exactly half"),
        (3, 1, LINK_MOVE_GAP, true, "a drop past 2x, past the gap"),
        (3, 1, ms(100), false, "a drop past 2x, inside the gap"),
        (2, 2, ms(1_999), false, "same rtt, inside the refresh"),
        (
            2,
            2,
            LINK_REFRESH,
            true,
            "same rtt, after the refresh window",
        ),
    ] {
        assert_eq!(up(was).ack_wants_report(rtt, t0 + after), wants, "{case}");
    }
}

#[test]
fn a_link_reason_is_one_token_per_failure_and_an_ack_never_reads_zero() {
    use io::ErrorKind as K;
    for (kind, token) in [
        (K::NotFound, "no-socket"),
        (K::ConnectionRefused, "refused"),
        (K::PermissionDenied, "denied"),
        (K::TimedOut, "no-ack"),
        (K::WouldBlock, "no-ack"),
        (K::UnexpectedEof, "closed"),
        (K::BrokenPipe, "closed"),
        (K::ConnectionReset, "closed"),
        (K::NotConnected, "closed"),
        (K::InvalidData, "error"),
    ] {
        assert_eq!(
            link_reason(&io::Error::new(kind, "x"), "attach"),
            token,
            "{kind:?}"
        );
    }
    assert_eq!(
        link_reason(&io::Error::other("denied by cap"), "attach"),
        "attach"
    );
    assert_eq!(
        link_reason(&io::Error::other("denied by cap"), "read"),
        "read"
    );
    use astream_broker::proto as p;
    let refusal = |code: u8| -> io::Error {
        astream_broker::BrokerRefusal {
            code,
            msg: "x".to_string(),
        }
        .into()
    };
    for (code, token) in [
        (p::ERR_TOO_MANY_CONNS, "busy"),
        (p::ERR_RECORD_REFUSED, "record"),
        (p::ERR_PROTOCOL, "protocol"),
        (p::ERR_UNAUTHORIZED, "attach"),
        (p::ERR_REFUSED, "attach"),
        (0xEE, "attach"),
    ] {
        assert_eq!(link_reason(&refusal(code), "attach"), token, "code {code}");
    }
    assert_eq!(link_reason(&refusal(p::ERR_UNAUTHORIZED), "read"), "read");
    let lapsed = |code: u8| -> io::Error {
        astream_broker::BrokerRefusal {
            code,
            msg: format!("{} at 1 (now 2, unix ms)", p::CAP_EXPIRED_PREFIX),
        }
        .into()
    };
    for caller in ["attach", "read", "subscribe"] {
        assert_eq!(link_reason(&lapsed(p::ERR_UNAUTHORIZED), caller), "expired");
    }
    assert!(!refused_for_good(&lapsed(p::ERR_UNAUTHORIZED)));
    assert_eq!(link_reason(&lapsed(p::ERR_REFUSED), "attach"), "attach");
    assert!(refused_for_good(&refusal(p::ERR_UNAUTHORIZED)));
    assert_eq!(
        link_reason(&refusal(p::ERR_UNAUTHORIZED), "attach"),
        "attach"
    );
    let words = |code: u8, msg: &str| -> io::Error {
        astream_broker::BrokerRefusal {
            code,
            msg: msg.to_string(),
        }
        .into()
    };
    let poisoned = format!(
        "{} (a flush failed (EIO) with records no full flush covers); reopen it to recover",
        p::LOG_POISONED_PREFIX
    );
    for msg in [poisoned.clone(), format!("{poisoned} — attach")] {
        for caller in ["attach", "read", "commit"] {
            assert_eq!(
                link_reason(&words(p::ERR_REFUSED, &msg), caller),
                "poisoned",
                "{msg}"
            );
        }
        assert!(!refusal_is_ack(&words(p::ERR_REFUSED, &msg)), "{msg}");
        assert!(!refused_for_good(&words(p::ERR_REFUSED, &msg)), "{msg}");
    }
    for (code, msg) in [
        (p::ERR_UNAUTHORIZED, poisoned.as_str()),
        (p::ERR_REFUSED, "commit failed: injected fsync fault"),
        (
            p::ERR_REFUSED,
            "durability flush failed: injected; the outcome is unknown, and the log refuses \
             every write until it is reopened: resend then",
        ),
        (
            p::ERR_REFUSED,
            "append failed: log poisoned (x); reopen it to recover",
        ),
    ] {
        assert_ne!(
            link_reason(&words(code, msg), "attach"),
            "poisoned",
            "{msg}"
        );
    }
    assert!(
        refusal_is_ack(&words(
            p::ERR_REFUSED,
            "commit failed: injected fsync fault"
        )),
        "a write rolled back is the broker working"
    );
    assert!(refusal_is_ack(&refusal(p::ERR_UNAUTHORIZED)));
    assert!(!refusal_is_ack(&lapsed(p::ERR_UNAUTHORIZED)));
    assert!(!refusal_is_ack(&refusal(p::ERR_TOO_MANY_CONNS)));
    assert!(!refusal_is_ack(&io::Error::new(K::BrokenPipe, "x")));
    let skew: io::Error = astream_broker::ProtocolMismatch {
        broker: astream_broker::proto::PROTO_VERSION + 1,
        client: astream_broker::proto::PROTO_VERSION,
    }
    .into();
    assert_eq!(link_reason(&skew, "attach"), "protocol");
    assert_eq!(link_reason(&skew, "read"), "protocol");
    assert_eq!(
        link_reason(&io::Error::new(K::Unsupported, "unsupported"), "attach"),
        "error"
    );
    assert_eq!(rtt_ms_of(Duration::from_micros(1)), 1);
    assert_eq!(rtt_ms_of(Duration::from_micros(999)), 1);
    assert_eq!(rtt_ms_of(Duration::from_micros(1_000)), 1);
    assert_eq!(rtt_ms_of(Duration::from_micros(1_001)), 2);
    assert_eq!(
        rtt_ms_of(Duration::ZERO),
        0,
        "only a zero-length round trip reads 0, and none is"
    );
}

#[test]
fn a_request_sent_past_an_owed_ack_is_this_bridges_bug() {
    let owed: io::Error = astream_broker::AcksOwed { owed: 2 }.into();
    assert!(sent_past_an_owed_ack(&owed));
    assert_eq!(link_reason(&owed, "read"), "error");
    assert_eq!(link_reason(&owed, "attach"), "error");
    let others: [(io::Error, &str); 3] = [
        (
            astream_broker::BrokerRefusal {
                code: astream_broker::proto::ERR_REFUSED,
                msg: "x".to_string(),
            }
            .into(),
            "link down reason=read",
        ),
        (
            io::Error::new(io::ErrorKind::BrokenPipe, "x"),
            "link down reason=closed",
        ),
        (
            io::Error::new(io::ErrorKind::ResourceBusy, "busy"),
            "link down reason=error",
        ),
    ];
    let (lane, mut bridge) = FakeVerbLane::bridge(&[]);
    let socket = lane.dir.path().join("broker.sock");
    let broker = crate::test_broker(lane.dir.path().join("broker.log")).unwrap();
    let _server = broker.serve(&socket).unwrap();
    bridge.cfg.broker = socket.to_string_lossy().into_owned();
    for (e, down) in &others {
        assert!(!sent_past_an_owed_ack(e), "{e}");
        bridge.conn = Some(bridge.connect().unwrap().0.into());
        bridge.note_ack(Duration::from_millis(1));
        assert!(bridge.link.up, "{e}: the link is up before the verdict");
        bridge.judge(bridge.fx.now(), Err(e), Some("read"));
        assert!(bridge.conn.is_none(), "{e}: the connection is dropped");
        assert!(!bridge.link.up, "{e}: the link is down");
        assert_eq!(
            lane.links.lock().unwrap().last().map(String::as_str),
            Some(*down),
            "{e}: reported under its own token"
        );
    }
    bridge.conn = Some(bridge.connect().unwrap().0.into());
    let stopped = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
        bridge.judge(bridge.fx.now(), Err(&owed), Some("read"));
    }));
    assert!(stopped.is_err(), "a debug build stops at the verdict");
}

/// **A READ REFUSED BELOW ITS FILTER'S OLDEST IS NO ANSWER YET** ([`plain_read_at`]):
/// astream's `retained-from=<oldest>` for an `<oldest>` above `off` hands the floor back,
/// for the read to be made again told ([`Bridge::read_at`]), and is never taken for no
/// rows (finding F3) nor judged as a record refused on its own terms, which would drop the
/// connection for a compaction.
///
/// Negative controls: an `<oldest>` at `off` (the record is reachable, so this is no such
/// answer), the same words under another code, a record refused on its own terms, and a
/// dead socket all stay the errors they are; a page's rows come through as they are.
#[test]
fn a_read_refused_below_its_filters_oldest_hands_its_floor_back() {
    use astream_broker::proto::{ERR_RECORD_REFUSED, ERR_REFUSED, RETAINED_FROM_PREFIX};
    let refusal =
        |code, msg: String| -> io::Error { astream_broker::BrokerRefusal { code, msg }.into() };
    let retained = || {
        refusal(
            ERR_RECORD_REFUSED,
            format!("{RETAINED_FROM_PREFIX}5: retention gave up the records below offset 5"),
        )
    };
    assert_eq!(
        link_reason(&retained(), "read"),
        "record",
        "PRECONDITION: judged as it stands, it would drop the connection"
    );
    assert_eq!(plain_read_at(Err(retained()), 4).unwrap(), Err(5));
    assert_eq!(plain_read_at(Err(retained()), 0).unwrap(), Err(5));
    for (e, off) in [
        (retained(), 5),
        (
            refusal(ERR_REFUSED, format!("{RETAINED_FROM_PREFIX}5: x")),
            4,
        ),
        (
            refusal(ERR_RECORD_REFUSED, "sequence in the reserved half".into()),
            4,
        ),
        (io::Error::new(io::ErrorKind::BrokenPipe, "x"), 4),
    ] {
        let what = e.to_string();
        assert!(plain_read_at(Err(e), off).is_err(), "{what} at {off}");
    }
    let row: BrokerRecord = (7, "/f/f/x".to_string(), b"b".to_vec());
    assert_eq!(
        plain_read_at(Ok((vec![row.clone()], (8, 8))), 7).unwrap(),
        Ok(vec![row])
    );
}

#[test]
fn a_halt_reason_is_bounded_sanitized_and_never_empty() {
    assert_eq!(halt_reason_token(None), "fleet-halt");
    assert_eq!(halt_reason_token(Some("")), "fleet-halt");
    assert_eq!(halt_reason_token(Some("main%20broken")), "main%20broken");
    assert_eq!(halt_reason_token(Some("a\u{7}b\nc\u{202e}d")), "abcd");
    let long = "x".repeat(200);
    assert_eq!(halt_reason_token(Some(&long)).len(), HALT_REASON_MAX);
    let escaped = format!("{}%41", "x".repeat(HALT_REASON_MAX - 2));
    let clamped = halt_reason_token(Some(&escaped));
    assert!(
        !clamped.ends_with('%') && !clamped.contains("%4"),
        "a clamp split an escape: {clamped}"
    );
}

struct FixtureLease;

static FIXTURE_ADMISSION: (std::sync::Mutex<usize>, std::sync::Condvar) =
    (std::sync::Mutex::new(0), std::sync::Condvar::new());

fn fixture_admission() -> usize {
    static ADMITTED: std::sync::OnceLock<usize> = std::sync::OnceLock::new();
    *ADMITTED.get_or_init(|| {
        if aterm_spec::host::raise_open_files(1024) >= 1024 {
            16
        } else {
            4
        }
    })
}

std::thread_local! {
    static FIXTURE_LEASE: std::cell::RefCell<std::sync::Weak<FixtureLease>> =
        const { std::cell::RefCell::new(std::sync::Weak::new()) };
}

impl FixtureLease {
    fn take() -> Arc<Self> {
        FIXTURE_LEASE.with(|current| {
            if let Some(lease) = current.borrow().upgrade() {
                return lease;
            }
            let (admission, changed) = &FIXTURE_ADMISSION;
            let mut active = admission.lock().unwrap_or_else(|e| e.into_inner());
            let until = Instant::now() + aterm_spec::wait::MUST_HAPPEN;
            let admitted = fixture_admission();
            while *active >= admitted {
                let remaining = until
                    .checked_duration_since(Instant::now())
                    .expect("a bridge fixture did not release its resource lease");
                (active, _) = changed
                    .wait_timeout(active, remaining)
                    .unwrap_or_else(|e| e.into_inner());
            }
            *active += 1;
            let lease = Arc::new(Self);
            *current.borrow_mut() = Arc::downgrade(&lease);
            lease
        })
    }
}

impl Drop for FixtureLease {
    fn drop(&mut self) {
        let (admission, changed) = &FIXTURE_ADMISSION;
        let mut active = admission.lock().unwrap_or_else(|e| e.into_inner());
        *active -= 1;
        changed.notify_one();
    }
}

#[derive(Default)]
struct LaneReaderState {
    stopped: bool,
    active: Option<std::sync::Weak<std::os::unix::net::UnixStream>>,
}

struct LaneReaderGuard {
    state: Arc<(std::sync::Mutex<LaneReaderState>, std::sync::Condvar)>,
    worker: Option<std::thread::JoinHandle<()>>,
}

impl Drop for LaneReaderGuard {
    fn drop(&mut self) {
        let (state, changed) = &*self.state;
        let mut state = state.lock().unwrap_or_else(|e| e.into_inner());
        state.stopped = true;
        if let Some(stream) = state.active.take().and_then(|stream| stream.upgrade()) {
            let _ = stream.shutdown(std::net::Shutdown::Write);
            let _ = stream.shutdown(std::net::Shutdown::Read);
        }
        changed.notify_one();
        drop(state);
        if let Some(worker) = self.worker.take() {
            if let Err(panic) = worker.join() {
                if !std::thread::panicking() {
                    std::panic::resume_unwind(panic);
                }
            }
        }
    }
}

struct FakeVerbLane {
    _lease: Arc<FixtureLease>,
    _reader: Option<LaneReaderGuard>,
    dir: aterm_tempfile::TempDir,
    sessions: Arc<std::sync::Mutex<Vec<(u64, String)>>>,
    topics: Arc<std::sync::Mutex<BTreeMap<String, Vec<String>>>>,
    topics_seen: Arc<std::sync::Mutex<BTreeMap<String, u64>>>,
    reads: Arc<std::sync::Mutex<Vec<String>>>,
    links: Arc<std::sync::Mutex<Vec<String>>>,
    log_answer: Arc<std::sync::Mutex<String>>,
    outbox: Arc<std::sync::Mutex<Option<String>>>,
    sent: Arc<std::sync::Mutex<Vec<String>>>,
    delivered: Arc<std::sync::Mutex<Vec<u64>>>,
    roster_refused: Arc<std::sync::atomic::AtomicBool>,
    roster_answer: Arc<std::sync::Mutex<Option<String>>>,
    hellos: Arc<std::sync::Mutex<Vec<String>>>,
    hello_answer: Arc<std::sync::Mutex<String>>,
    reads_refused: Arc<std::sync::Mutex<BTreeMap<String, usize>>>,
    /// The `roster=` this lane names in `outbox` and `sessions bridge`, `None` as an older
    /// aterm names none.
    roster_seq: Arc<std::sync::Mutex<Option<u64>>>,
    /// How many `sessions bridge` requests it has answered.
    roster_reads: Arc<std::sync::atomic::AtomicU64>,
    transcript: Arc<std::sync::Mutex<Vec<String>>>,
    on_deliver: Arc<std::sync::Mutex<Option<LaneHook>>>,
    /// Run at the first `outbox sent` it answers ([`FakeVerbLane::on_first_retire`]).
    on_retire: Arc<std::sync::Mutex<Option<LaneHook>>>,
    deliver_answers: Arc<std::sync::Mutex<BTreeMap<String, String>>>,
    /// The request the lane hangs up at, unanswered, as aterm's going does: the first line
    /// that starts with it ([`FakeVerbLane::hang_up_at`]).
    hang_up: Arc<std::sync::Mutex<Option<&'static str>>>,
    hold_refusals: Arc<std::sync::Mutex<BTreeMap<String, String>>>,
    holds: Arc<std::sync::Mutex<BTreeMap<String, bool>>>,
    status_rows: Arc<std::sync::Mutex<Option<Vec<String>>>>,
    retirements_refused: Arc<std::sync::atomic::AtomicBool>,
    outbox_retires: Arc<std::sync::atomic::AtomicBool>,
    deliveries: Arc<std::sync::Mutex<Vec<String>>>,
    mirror: Arc<std::sync::Mutex<FakeMirror>>,
    /// Every request line the lane answered (`AUTH` aside), in order.
    requests: Arc<std::sync::Mutex<Vec<String>>>,
    /// The header `@<sid> meta` answers, `None` to refuse it as before.
    meta_answer: Arc<std::sync::Mutex<Option<String>>>,
    /// The header `@<sid> status` answers, `None` to refuse it as before.
    single_status_answer: Arc<std::sync::Mutex<Option<String>>>,
}

/// What one `log-take` round trip costs the bridge's held clock at a fake lane that shares it
/// ([`FakeMirror::clock`]).
const TAKE_ROUND_TRIP: Duration = Duration::from_millis(1);

#[derive(Default)]
struct FakeMirror {
    logs: BTreeMap<String, aterm_slog::Log>,
    adopting: bool,
    gone_while_adopting: bool,
    skip_covers: bool,
    refuse_take: Option<String>,
    takes: usize,
    /// When the lane was asked each take, in order, on the bridge's clock when it shares one
    /// ([`FakeMirror::clock`]).
    taken_at: Vec<Instant>,
    /// The bridge's held clock ([`crate::fx::Fx::on_a_held_clock`]), which each take's
    /// round trip moves on by [`TAKE_ROUND_TRIP`] before the lane answers; `None` reads the
    /// wall clock and moves nothing.
    clock: Option<Arc<crate::fx::HeldClock>>,
    landings: Vec<String>,
    /// Sessions whose landing answers `done`, as the endpoint answers for a closed
    /// session whose tail is all on the bus.
    done: BTreeSet<String>,
}

impl FakeMirror {
    fn take(&mut self, rest: &str) -> String {
        self.takes += 1;
        self.taken_at
            .push(self.clock.as_ref().map_or_else(Instant::now, |c| c.now()));
        if let Some(clock) = &self.clock {
            clock.advance(TAKE_ROUND_TRIP);
        }
        if let Some(refusal) = &self.refuse_take {
            return refusal.clone();
        }
        let n: usize = rest
            .split_whitespace()
            .find_map(|t| t.strip_prefix("n="))
            .and_then(|n| n.parse().ok())
            .unwrap_or(aterm_slog::TAKE_MAX);
        let (mut body, mut k, mut more) = (String::new(), 0usize, false);
        for (sid, log) in &mut self.logs {
            if k == n {
                more = true;
                break;
            }
            let copy = log.bus_copy(n - k, aterm_slog::Stamp::new(1, 1));
            more |= copy.more();
            for s in copy.encode(aterm_slog::Tier::Wire, &|_| None).slots {
                if self.skip_covers && s.kind == aterm_slog::SlotKind::Cover {
                    continue;
                }
                body.push_str(&format!("{sid} {} {}\n", s.at.word(), s.body));
                k += 1;
            }
        }
        let more = if more { " more=1" } else { "" };
        format!("OK {} slots={k}{more}\n{body}", body.len())
    }

    fn land(&mut self, rest: &str) -> String {
        self.landings.push(rest.to_string());
        let toks: Vec<&str> = rest.split_whitespace().collect();
        let mut words = Vec::new();
        for pair in toks.as_chunks::<2>().0 {
            let at = aterm_slog::BusCursor::parse(pair[1]).expect("a cursor");
            let done = self.done.contains(pair[0]);
            let word = match self.logs.get_mut(pair[0]) {
                Some(log) => match log.land(at) {
                    Ok(_) if done => "done",
                    Ok(aterm_slog::Landed::Moved) => "ok",
                    Ok(aterm_slog::Landed::Stale) => "stale",
                    Err(aterm_slog::LandRefused::Fenced) => "fenced",
                    Err(aterm_slog::LandRefused::Ahead) => "ahead",
                },
                None if self.adopting && !self.gone_while_adopting => "adopting",
                None => "gone",
            };
            words.push(word);
        }
        format!("OK {}\n", words.join(" "))
    }
}

type LaneHook = Box<dyn FnOnce() + Send>;

impl FakeVerbLane {
    fn bridge(sessions: &[(u64, &str)]) -> (Self, Bridge) {
        Self::bridge_with_state(sessions, None)
    }

    fn bridge_with_state(
        sessions: &[(u64, &str)],
        previous: Option<&std::path::Path>,
    ) -> (Self, Bridge) {
        Self::bridge_configured(sessions, previous, None)
    }

    /// [`FakeVerbLane::bridge_with_state`], with the mesh on where `mesh` names its paths.
    fn bridge_configured(
        sessions: &[(u64, &str)],
        previous: Option<&std::path::Path>,
        mesh: Option<crate::federate::MeshPaths>,
    ) -> (Self, Bridge) {
        use std::io::{BufRead, BufReader, Write};
        let lease = FixtureLease::take();
        let dir = aterm_tempfile::Builder::new()
            .prefix("al-v")
            .suffix(".noindex")
            .tempdir_in("/tmp")
            .expect("the fixture dir");
        let sock = dir.path().join("v.sock");
        let broker = dir.path().join("no-broker").to_string_lossy().into_owned();
        let state_dir = previous
            .map_or_else(|| dir.path().join("state"), std::path::Path::to_path_buf)
            .to_string_lossy()
            .into_owned();
        let listener = std::os::unix::net::UnixListener::bind(&sock).expect("bind the lane");
        let mut lane = FakeVerbLane {
            _lease: lease.clone(),
            _reader: None,
            dir,
            sessions: Arc::new(std::sync::Mutex::new(
                sessions.iter().map(|(l, s)| (*l, s.to_string())).collect(),
            )),
            topics: Arc::default(),
            topics_seen: Arc::default(),
            reads: Arc::default(),
            links: Arc::default(),
            log_answer: Arc::new(std::sync::Mutex::new("OK\n".to_string())),
            outbox: Arc::default(),
            sent: Arc::default(),
            delivered: Arc::default(),
            roster_refused: Arc::default(),
            roster_seq: Arc::default(),
            roster_reads: Arc::default(),
            roster_answer: Arc::default(),
            hellos: Arc::default(),
            hello_answer: Arc::new(std::sync::Mutex::new(
                "ERR unknown verb (try: help)\n".to_string(),
            )),
            reads_refused: Arc::default(),
            transcript: Arc::default(),
            on_deliver: Arc::default(),
            on_retire: Arc::default(),
            deliver_answers: Arc::default(),
            hang_up: Arc::default(),
            hold_refusals: Arc::default(),
            holds: Arc::default(),
            status_rows: Arc::default(),
            retirements_refused: Arc::default(),
            outbox_retires: Arc::default(),
            deliveries: Arc::default(),
            mirror: Arc::default(),
            requests: Arc::default(),
            meta_answer: Arc::default(),
            single_status_answer: Arc::default(),
        };
        let (transcript, on_deliver) = (lane.transcript.clone(), lane.on_deliver.clone());
        let on_retire = lane.on_retire.clone();
        let deliver_answers = lane.deliver_answers.clone();
        let roster_answer = lane.roster_answer.clone();
        let (hellos, hello_answer) = (lane.hellos.clone(), lane.hello_answer.clone());
        let hang_up = lane.hang_up.clone();
        let (hold_refusals, holds) = (lane.hold_refusals.clone(), lane.holds.clone());
        let status_rows = lane.status_rows.clone();
        let (roster, topics, reads, delivered) = (
            lane.sessions.clone(),
            lane.topics.clone(),
            lane.reads.clone(),
            lane.delivered.clone(),
        );
        let topics_seen = lane.topics_seen.clone();
        let (roster_refused, reads_refused) =
            (lane.roster_refused.clone(), lane.reads_refused.clone());
        let (roster_seq, roster_reads) = (lane.roster_seq.clone(), lane.roster_reads.clone());
        let links = lane.links.clone();
        let log_answer = lane.log_answer.clone();
        let (outbox, sent) = (lane.outbox.clone(), lane.sent.clone());
        let retirements_refused = lane.retirements_refused.clone();
        let (outbox_retires, deliveries) = (lane.outbox_retires.clone(), lane.deliveries.clone());
        let mirror = lane.mirror.clone();
        let (requests, meta_answer) = (lane.requests.clone(), lane.meta_answer.clone());
        let single_status_answer = lane.single_status_answer.clone();
        let reader_state = Arc::new((
            std::sync::Mutex::new(LaneReaderState::default()),
            std::sync::Condvar::new(),
        ));
        let accepting = reader_state.clone();
        let worker = std::thread::spawn(move || {
            let _lease = lease;
            let (state, changed) = &*accepting;
            let stream = loop {
                let mut state = state.lock().unwrap_or_else(|e| e.into_inner());
                if state.stopped {
                    return;
                }
                match aterm_uds::accept(
                    &listener,
                    aterm_uds::AcceptWait::Ready,
                    aterm_uds::AcceptedMode::Blocking,
                ) {
                    Ok((stream, _)) => {
                        let stream = Arc::new(stream);
                        state.active = Some(Arc::downgrade(&stream));
                        break stream;
                    }
                    Err(e) if e.kind() == io::ErrorKind::WouldBlock => {
                        drop(
                            changed
                                .wait_timeout(state, Duration::from_millis(20))
                                .unwrap_or_else(|e| e.into_inner()),
                        );
                    }
                    Err(e) => panic!("accept the fake control lane: {e}"),
                }
            };
            let mut out = stream.as_ref();
            for line in BufReader::new(stream.as_ref()).lines() {
                let Ok(line) = line else { return };
                let roster_token = || {
                    roster_seq
                        .lock()
                        .unwrap()
                        .map_or_else(String::new, |seq| format!(" roster={seq}"))
                };
                if line == "sessions bridge" {
                    roster_reads.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
                }
                if !line.starts_with("AUTH ") {
                    requests.lock().unwrap().push(line.clone());
                }
                if hang_up
                    .lock()
                    .unwrap()
                    .is_some_and(|verb| line.starts_with(verb))
                {
                    transcript.lock().unwrap().push(line);
                    return;
                }
                // The canned per-session header: `@<sid> meta`'s, or `@<sid> status`'s.
                let canned = match line.strip_prefix('@').and_then(|l| l.split_once(' ')) {
                    Some((_, "meta")) => meta_answer.lock().unwrap().clone(),
                    Some((_, "status")) => single_status_answer.lock().unwrap().clone(),
                    _ => None,
                };
                let reply = if line.starts_with("AUTH ") {
                    continue;
                } else if let Some(header) = canned {
                    format!("{header}\n")
                } else if matches!(line.as_str(), "outbox" | "outbox earlier-receipts=1") {
                    match outbox.lock().unwrap().as_ref() {
                        Some(frame) => format!("OK {}{}\n{frame}", frame.len(), roster_token()),
                        None => "ERR denied\n".to_string(),
                    }
                } else if line.starts_with("outbox sent ") {
                    let hook = on_retire.lock().unwrap().take();
                    if let Some(hook) = hook {
                        hook();
                    }
                    if retirements_refused.load(std::sync::atomic::Ordering::SeqCst) {
                        "ERR busy\n".to_string()
                    } else {
                        if outbox_retires.load(std::sync::atomic::Ordering::SeqCst) {
                            let mut words = line.split_whitespace().skip(2);
                            let (sid, id) = (words.next(), words.next());
                            if let (Some(frame), Some(sid), Some(id)) =
                                (outbox.lock().unwrap().as_mut(), sid, id)
                            {
                                *frame = without_row(frame, sid, &format!("id={id}"));
                            }
                        }
                        sent.lock().unwrap().push(line);
                        "OK\n".to_string()
                    }
                } else if let Some(rest) = line.strip_prefix("log-take") {
                    mirror.lock().unwrap().take(rest)
                } else if let Some(rest) = line.strip_prefix("log-landed ") {
                    mirror.lock().unwrap().land(rest)
                } else if line.starts_with("link log ") {
                    links.lock().unwrap().push(line);
                    let reply = log_answer.lock().unwrap().clone();
                    if reply.is_empty() {
                        return;
                    }
                    reply
                } else if line.starts_with("link ") {
                    links.lock().unwrap().push(line);
                    "OK\n".to_string()
                } else if line.starts_with("hello ") {
                    hellos.lock().unwrap().push(line);
                    hello_answer.lock().unwrap().clone()
                } else if let Some(answer) = (line == "sessions bridge")
                    .then(|| roster_answer.lock().unwrap().clone())
                    .flatten()
                {
                    answer
                } else if line == "sessions bridge"
                    && roster_refused.load(std::sync::atomic::Ordering::SeqCst)
                {
                    "ERR busy\n".to_string()
                } else if let Some(rows) = (line == "sessions status")
                    .then(|| status_rows.lock().unwrap().clone())
                    .flatten()
                {
                    let mut r = format!("OK {}\n", rows.len());
                    for row in rows {
                        r.push_str(&row);
                        r.push('\n');
                    }
                    r
                } else if line == "sessions bridge" {
                    let rows = roster.lock().unwrap();
                    let mut r = format!("OK {}{}\n", rows.len(), roster_token());
                    for (local, sid) in rows.iter() {
                        r.push_str(&format!("{local} {sid} nonce={local:032x}\n"));
                    }
                    r
                } else if let Some(rest) = line.strip_prefix("hold ") {
                    transcript.lock().unwrap().push(line.clone());
                    let mut words = rest.split_whitespace();
                    let sid = words.next().expect("a hold's sid");
                    let on = words.next() == Some("on");
                    match hold_refusals.lock().unwrap().get(sid) {
                        Some(reply) => format!("{reply}\n"),
                        None => {
                            holds.lock().unwrap().insert(sid.to_string(), on);
                            format!("OK hold={}\n", u8::from(on))
                        }
                    }
                } else if let Some(rest) = line.strip_prefix("deliver ") {
                    deliveries.lock().unwrap().push(line.clone());
                    let off = rest
                        .split_whitespace()
                        .find_map(|word| word.strip_prefix("off="))
                        .and_then(|word| word.parse::<u64>().ok());
                    transcript.lock().unwrap().push(format!(
                        "deliver {}",
                        off.map_or_else(|| "-".to_string(), |o| o.to_string())
                    ));
                    let hook = on_deliver.lock().unwrap().take();
                    if let Some(hook) = hook {
                        hook();
                    }
                    let answer = rest
                        .split_whitespace()
                        .next()
                        .and_then(|sid| deliver_answers.lock().unwrap().get(sid).cloned());
                    // A receipt's retirement, refused as aterm refuses it while a seamless
                    // update commits, or taken out of the outbox it retires.
                    let receipt = rest
                        .split_whitespace()
                        .find_map(|word| word.strip_prefix("receipt="));
                    let refused = receipt.is_some()
                        && retirements_refused.load(std::sync::atomic::Ordering::SeqCst);
                    if answer.is_none()
                        && !refused
                        && outbox_retires.load(std::sync::atomic::Ordering::SeqCst)
                    {
                        let sid = rest.split_whitespace().next();
                        if let (Some(frame), Some(sid), Some(rid)) =
                            (outbox.lock().unwrap().as_mut(), sid, receipt)
                        {
                            *frame = without_row(frame, sid, &format!("rid={rid}"));
                        }
                    }
                    match (answer, off) {
                        (Some(answer), _) => answer,
                        (None, _) if refused => "ERR busy update-committing\n".to_string(),
                        (None, Some(off)) => {
                            let mut ring = delivered.lock().unwrap();
                            let id = match ring.iter().position(|have| *have == off) {
                                Some(i) => i + 1,
                                None => {
                                    ring.push(off);
                                    ring.len()
                                }
                            };
                            format!("OK {id}\n")
                        }
                        (None, None)
                            if rest.split_whitespace().any(|w| w.starts_with("receipt=")) =>
                        {
                            "OK\n".to_string()
                        }
                        (None, None) => "ERR invalid deliver\n".to_string(),
                    }
                } else if let Some(sid) = line
                    .strip_prefix('@')
                    .and_then(|l| l.strip_suffix(" topic ls"))
                {
                    reads.lock().unwrap().push(sid.to_string());
                    let refused = reads_refused
                        .lock()
                        .unwrap()
                        .get_mut(sid)
                        .filter(|left| **left > 0)
                        .map(|left| *left -= 1)
                        .is_some();
                    if refused {
                        if out.write_all(b"ERR busy\n").is_err() {
                            return;
                        }
                        continue;
                    }
                    let rows = topics.lock().unwrap().get(sid).cloned().unwrap_or_default();
                    let mut r = match topics_seen.lock().unwrap().get(sid) {
                        Some(seen) => format!("OK {} seen={seen}\n", rows.len()),
                        None => format!("OK {}\n", rows.len()),
                    };
                    for row in rows {
                        r.push_str(&row);
                        r.push('\n');
                    }
                    r
                } else {
                    "ERR denied\n".to_string()
                };
                if out.write_all(reply.as_bytes()).is_err() {
                    return;
                }
            }
        });
        lane._reader = Some(LaneReaderGuard {
            state: reader_state,
            worker: Some(worker),
        });
        let bridge = Bridge::new(Config {
            fleet: "f".to_string(),
            broker,
            cap_files: Vec::new(),
            state_dir,
            accept_from: Vec::new(),
            sock: Some(sock.to_string_lossy().into_owned()),
            token: Some("t".to_string()),
            presence: Mode::default(),
            receipts: true,
            log: crate::mirror::LogMode::Off,
            mesh,
        })
        .expect("a bridge on the fake lane");
        assert_eq!(bridge.attachment, Attachment::Observer);
        let mut bridge = bridge;
        bridge.attachment = Attachment::Inherited;
        (lane, bridge)
    }

    fn report_handled(&self, sid: &str, seen: u64) {
        self.topics_seen
            .lock()
            .unwrap()
            .insert(sid.to_string(), seen);
    }

    fn opt_in(&self, sid: &str, rows: &[&str]) {
        self.topics.lock().unwrap().insert(
            sid.to_string(),
            rows.iter().map(|r| (*r).to_string()).collect(),
        );
    }

    fn refuse_roster(&self, refused: bool) {
        self.roster_refused
            .store(refused, std::sync::atomic::Ordering::SeqCst);
    }

    fn refuse_retirements(&self, refused: bool) {
        self.retirements_refused
            .store(refused, std::sync::atomic::Ordering::SeqCst);
    }

    fn retire_from_outbox(&self) {
        self.outbox_retires
            .store(true, std::sync::atomic::Ordering::SeqCst);
    }

    fn deliveries(&self) -> Vec<String> {
        self.deliveries.lock().unwrap().clone()
    }

    /// The requests answered since the last call, each by the tally row it counts on
    /// ([`crate::wait_tally::lane_row`]).
    fn take_requests(&self) -> Vec<(crate::wait_tally::Row, String)> {
        std::mem::take(&mut *self.requests.lock().unwrap())
            .into_iter()
            .map(|line| (crate::wait_tally::lane_row(&line), line))
            .collect()
    }

    fn answer_meta_with(&self, header: &str) {
        *self.meta_answer.lock().unwrap() = Some(header.to_string());
    }

    /// Answers every `@<sid> status` with `header`, as aterm's single status record.
    fn answer_single_status_with(&self, header: &str) {
        *self.single_status_answer.lock().unwrap() = Some(header.to_string());
    }

    fn answer_status_with(&self, rows: &[String]) {
        *self.status_rows.lock().unwrap() = Some(rows.to_vec());
    }

    fn answer_roster_with(&self, reply: &str) {
        *self.roster_answer.lock().unwrap() = Some(reply.to_string());
    }

    fn links(&self) -> Vec<String> {
        self.links.lock().unwrap().clone()
    }

    fn answer_hello_with(&self, reply: &str) {
        *self.hello_answer.lock().unwrap() = reply.to_string();
    }

    fn hellos(&self) -> Vec<String> {
        self.hellos.lock().unwrap().clone()
    }

    fn refuse_reads(&self, sid: &str, n: usize) {
        self.reads_refused
            .lock()
            .unwrap()
            .insert(sid.to_string(), n);
    }

    fn reads_of(&self, sid: &str) -> usize {
        self.reads
            .lock()
            .unwrap()
            .iter()
            .filter(|s| *s == sid)
            .count()
    }

    fn delivery_offsets(&self) -> Vec<u64> {
        self.delivered.lock().unwrap().clone()
    }

    fn transcript(&self) -> Vec<String> {
        self.transcript.lock().unwrap().clone()
    }

    fn answer_deliveries_to(&self, sid: &str, reply: &str) {
        self.deliver_answers
            .lock()
            .unwrap()
            .insert(sid.to_string(), reply.to_string());
    }

    fn on_first_deliver(&self, hook: impl FnOnce() + Send + 'static) {
        *self.on_deliver.lock().unwrap() = Some(Box::new(hook));
    }

    fn on_first_retire(&self, hook: impl FnOnce() + Send + 'static) {
        *self.on_retire.lock().unwrap() = Some(Box::new(hook));
    }

    fn refuse_hold(&self, sid: &str, reply: Option<&str>) {
        let mut refused = self.hold_refusals.lock().unwrap();
        match reply {
            Some(reply) => {
                refused.insert(sid.to_string(), reply.to_string());
            }
            None => {
                refused.remove(sid);
            }
        }
    }

    fn holds(&self, sid: &str) -> bool {
        self.holds
            .lock()
            .unwrap()
            .get(sid)
            .copied()
            .unwrap_or(false)
    }

    /// Hang up at the next request that starts with `verb`, leaving it unanswered: aterm
    /// gone under the bridge's request.
    fn hang_up_at(&self, verb: &'static str) {
        *self.hang_up.lock().unwrap() = Some(verb);
    }
}

#[test]
fn a_live_sequence_collision_reports_the_link_down_before_reconnect() {
    let (lane, mut bridge) = FakeVerbLane::bridge(&[]);
    let socket = lane.dir.path().join("broker.sock");
    let broker = crate::test_broker(lane.dir.path().join("broker.log")).unwrap();
    let _server = broker.serve(&socket).unwrap();
    bridge.cfg.broker = socket.to_string_lossy().into_owned();
    let (conn, _) = bridge.connect().unwrap();
    bridge.conn = Some(conn.into());
    bridge.note_ack(Duration::from_millis(1));
    assert!(bridge.link.up);

    let subject = subject::node_face(&bridge.cfg.fleet, &bridge.node, "ev");
    let seq = bridge.next_seq().unwrap();
    let (own, deduped) = bridge.publish_at(seq, &subject, b"ours").unwrap();
    assert!(!deduped);
    assert_eq!(
        bridge.publish_at(seq, &subject, b"ours").unwrap(),
        (own, true)
    );
    assert!(bridge.link.up, "our own retry leaves the link connected");
    assert!(bridge.collided.is_none());

    let seq = bridge.next_seq().unwrap();
    let (mut other, _) = bridge.connect().unwrap();
    let (off, deduped) = other
        .publish(bridge.producer_id, seq, "/f/f/x/decoy", b"another writer")
        .unwrap();
    assert!(!deduped);
    let error = bridge
        .publish_at(seq, &subject, b"not published")
        .unwrap_err();
    assert_eq!(error.kind(), io::ErrorKind::AlreadyExists);
    assert!(
        bridge.conn.is_none(),
        "the collision dropped the connection"
    );
    assert!(
        !bridge.link.up,
        "a collision is stalled before any reconnect can restore the link"
    );
    assert_eq!(
        lane.links.lock().unwrap().last().map(String::as_str),
        Some("link down reason=attach"),
        "the endpoint hears the loss, so it can release queued post waits"
    );
    assert_eq!(bridge.collided, Some((seq, off)));
    assert!(!bridge.self_acked.contains(&off), "the offset is not ours");
}

#[test]
fn a_denied_dedup_verification_keeps_the_post_queued() {
    for verification in [false, true] {
        denied_publication_read(false, verification, false);
    }
}

#[test]
fn a_denied_dedup_verification_keeps_the_receipt_owed() {
    for verification in [false, true] {
        denied_publication_read(true, verification, false);
    }
}

#[test]
fn a_lapsed_publish_keeps_the_post_queued_and_the_receipt_owed() {
    for receipt in [false, true] {
        denied_publication_read(receipt, false, true);
    }
}

#[test]
fn a_poisoned_publish_downs_the_link_and_keeps_the_post_queued() {
    for poisoned in [true, false] {
        refused_publish(poisoned);
    }
}

fn refused_publish(poisoned: bool) {
    use astream_broker::proto::{
        decode_request, encode_response, read_frame, write_frame, Request, Response, ERR_REFUSED,
        LOG_POISONED_PREFIX,
    };
    let (lane, mut bridge) = FakeVerbLane::bridge(&[(1, "s-host")]);
    let face = format!("/f/f/in/p/h-a/{}/note", bridge.node);
    let mut body = Body::new(1);
    body.from = Some("s-host".into());
    body.text = "retry".into();
    *lane.outbox.lock().unwrap() =
        Some("post sid=s-host id=1 to=h-a kind=note len=5\nretry".into());
    let pin = PublishPin {
        seq: bridge.next_seq().unwrap(),
        via_key: false,
        identity: Some(publication_identity(&face, &body.encode())),
        original: None,
        original_off: None,
        owner: None,
        at: None,
    };
    bridge.state.set_post_pin("s-host", 1, &pin).unwrap();
    bridge.unsent = None;
    let socket = lane.dir.path().join("poisoned.sock");
    let listener = std::os::unix::net::UnixListener::bind(&socket).unwrap();
    let (producer, seq) = (bridge.producer_id, pin.seq);
    let (judged, wait_judged) = std::sync::mpsc::channel::<()>();
    let broker = std::thread::spawn(move || {
        let (mut stream, _) = aterm_uds::accept(
            &listener,
            aterm_uds::AcceptWait::Forever,
            aterm_uds::AcceptedMode::Blocking,
        )
        .unwrap();
        stream
            .set_read_timeout(Some(Duration::from_secs(60)))
            .unwrap();
        let request = read_frame(&mut stream).unwrap().unwrap();
        let Some(Request::Publish {
            producer_id,
            producer_seq,
            ..
        }) = decode_request(&request)
        else {
            panic!("the retry must publish first");
        };
        assert_eq!((producer_id, producer_seq), (producer, seq));
        let msg = if poisoned {
            format!(
                "{LOG_POISONED_PREFIX} (a flush failed (injected) with records no full flush \
                 covers); reopen it to recover"
            )
        } else {
            "commit failed: injected fsync fault".to_string()
        };
        write_frame(
            &mut stream,
            &encode_response(&Response::Error {
                code: ERR_REFUSED,
                msg,
            }),
        )
        .unwrap();
        let _ = wait_judged.recv_timeout(Duration::from_secs(60));
    });
    bridge.cfg.broker = socket.to_string_lossy().into_owned();
    bridge.conn = Some(bridge.connect().unwrap().0.into());
    bridge.note_ack(Duration::from_millis(1));
    bridge.drain_outbox();
    let retained = bridge.state.post_pin("s-host", 1).unwrap().is_some();
    let retired = !lane.sent.lock().unwrap().is_empty();
    assert_eq!(
        (retained, retired),
        (true, false),
        "nothing is retired, poisoned={poisoned}"
    );
    if poisoned {
        assert!(bridge.conn.is_none(), "the poisoned connection is dropped");
        assert!(!bridge.link.up);
        assert_eq!(
            lane.links.lock().unwrap().last().map(String::as_str),
            Some("link down reason=poisoned"),
        );
    } else {
        assert!(
            bridge.conn.is_some(),
            "a rolled-back write keeps the connection"
        );
        assert!(bridge.link.up, "a rolled-back write is the broker working");
        assert!(
            !lane
                .links
                .lock()
                .unwrap()
                .iter()
                .any(|l| l.starts_with("link down")),
            "{:?}",
            lane.links.lock().unwrap()
        );
    }
    judged.send(()).unwrap();
    broker.join().unwrap();
}

fn denied_publication_read(receipt: bool, verification: bool, lapsed: bool) {
    use astream_broker::proto::{
        decode_request, encode_response, read_frame, write_frame, Request, Response,
        CAP_EXPIRED_PREFIX, ERR_UNAUTHORIZED,
    };
    assert!(
        !(lapsed && verification),
        "a lapsed cap refuses the Publish"
    );
    let (lane, mut bridge) = FakeVerbLane::bridge(&[(1, "s-host")]);
    let kind = if receipt { "ack" } else { "note" };
    let face = format!("/f/f/in/p/h-a/{}/{kind}", bridge.node);
    let mut body = Body::new(1);
    body.from = Some("s-host".into());
    body.text = "retry".into();
    let owed = OwedReceipt {
        sid: "s-host".into(),
        rid: 1,
        off: 4,
        verdict: "handled".into(),
        kind: "ask".into(),
        from: "h-a".into(),
        earlier_log: false,
    };
    if receipt {
        body = receipt_body(1, "s-host", owed.off, &owed.verdict);
    } else {
        *lane.outbox.lock().unwrap() =
            Some("post sid=s-host id=1 to=h-a kind=note len=5\nretry".into());
    }
    let pin = PublishPin {
        seq: bridge.next_seq().unwrap(),
        via_key: false,
        identity: Some(publication_identity(&face, &body.encode())),
        original: None,
        original_off: None,
        owner: None,
        at: None,
    };
    if receipt {
        bridge.state.set_receipt_pin("s-host", 1, &pin).unwrap();
    } else {
        bridge.state.set_post_pin("s-host", 1, &pin).unwrap();
    }
    bridge.unsent = None;
    let socket = lane.dir.path().join("denied.sock");
    let listener = std::os::unix::net::UnixListener::bind(&socket).unwrap();
    let identity = pin.identity.clone().unwrap();
    let (producer, seq) = (bridge.producer_id, pin.seq);
    let broker = std::thread::spawn(move || {
        let (mut stream, _) = aterm_uds::accept(
            &listener,
            aterm_uds::AcceptWait::Forever,
            aterm_uds::AcceptedMode::Blocking,
        )
        .unwrap();
        stream
            .set_read_timeout(Some(Duration::from_secs(60)))
            .unwrap();
        let request = read_frame(&mut stream).unwrap().unwrap();
        let Some(Request::Publish {
            producer_id,
            producer_seq,
            subject,
            body,
        }) = decode_request(&request)
        else {
            panic!("the retry must publish first");
        };
        assert_eq!((producer_id, producer_seq), (producer, seq));
        assert_eq!(publication_identity(&subject, &body), identity);
        let refusal = Response::Error {
            code: ERR_UNAUTHORIZED,
            msg: if lapsed {
                format!(
                    "{CAP_EXPIRED_PREFIX} before this request: every grant that covered it \
                     has lapsed (now 2, unix ms)"
                )
            } else {
                "the requested operation is not granted".into()
            },
        };
        if verification {
            write_frame(
                &mut stream,
                &encode_response(&Response::PublishAck {
                    offset: 9,
                    deduped: true,
                }),
            )
            .unwrap();
            let request = read_frame(&mut stream).unwrap().unwrap();
            assert_eq!(
                decode_request(&request),
                Some(Request::Fetch {
                    from_offset: 9,
                    filter: subject,
                    max: 1,
                }),
                "the denial follows a successful dedup acknowledgement"
            );
        }
        write_frame(&mut stream, &encode_response(&refusal)).unwrap();
    });
    bridge.cfg.broker = socket.to_string_lossy().into_owned();
    bridge.conn = Some(bridge.connect().unwrap().0.into());
    let failed = if receipt {
        bridge.send_receipt(&owed).is_err()
    } else {
        bridge.drain_outbox();
        false
    };
    broker.join().unwrap();
    let retained = if receipt {
        bridge.state.receipt_pin("s-host", 1).unwrap().is_some()
    } else {
        bridge.state.post_pin("s-host", 1).unwrap().is_some()
    };
    let retired = if receipt {
        lane.transcript().iter().any(|line| line == "deliver -")
    } else {
        !lane.sent.lock().unwrap().is_empty()
    };
    if lapsed {
        assert_eq!(
            (retained, retired),
            (true, false),
            "a lapsed cap retires nothing: receipt={receipt}"
        );
        assert!(bridge.conn.is_none(), "the lapsed connection is dropped");
        assert!(!bridge.link.up);
        assert_eq!(
            lane.links.lock().unwrap().last().map(String::as_str),
            Some("link down reason=expired"),
        );
        if receipt {
            assert!(failed, "the receipt stays owed for the redial");
        }
        return;
    }
    assert_eq!(
        (retained, retired),
        (verification, !verification),
        "only an actual Publish refusal retires debt: receipt={receipt}, verification={verification}"
    );
    if receipt {
        assert_eq!(failed, verification);
    }
}

/// ONE OLDER BUILD'S DAMAGED RESERVATION LEAVES THE NODE ATTACHING (B19 review): an older
/// build's `sent/<sid>.<id>` that does not read holds that one post, as that build held it;
/// another session's reservation reads, the attach's in-flight settle goes on, and what is
/// owed reads.
#[test]
fn one_older_builds_damaged_reservation_leaves_the_node_attaching() {
    let (lane, mut bridge) = FakeVerbLane::bridge(&[]);
    let (socket, _broker, _server) = serve_log(&lane, "broker");
    bridge.cfg.broker = socket;
    bridge.conn = Some(bridge.connect().unwrap().0.into());
    let root = bridge.state.root().to_path_buf();
    std::fs::create_dir_all(root.join("sent")).unwrap();
    std::fs::write(root.join("sent/s-z.9"), b"7 key identity\n").unwrap();
    bridge.state = StateDir::open(&root).unwrap();
    assert_eq!(bridge.state.post_pin("s-a", 1).unwrap(), None);
    assert!(
        bridge.settle_in_flight(),
        "the attach's in-flight settle goes on"
    );
    assert!(bridge
        .state
        .owed_publications()
        .unwrap()
        .sids
        .contains("s-z"));
    let held = bridge.state.post_pin("s-z", 9).unwrap_err();
    assert!(held.to_string().contains("sent/s-z.9"), "{held}");
}

/// A DROP THE DISK REFUSES IS NOT PUBLISHED AT THE NEXT ATTACH (B19 review): a record the
/// bridge drops for good while the journal's writes are refused (a full disk) is forgotten by
/// the open all the same, as an older build's unlink of its file was done, so the attach
/// that follows in the same process publishes nothing.
#[test]
fn a_record_dropped_while_the_disk_is_full_is_not_published_at_the_next_attach() {
    let (lane, mut bridge) = FakeVerbLane::bridge(&[]);
    let (socket, _broker, _server) = serve_log(&lane, "broker");
    bridge.cfg.broker = socket;
    bridge.conn = Some(bridge.connect().unwrap().0.into());
    let face = format!(
        "/f/{}/in/{}/s-b/{}/note",
        bridge.cfg.fleet, bridge.node, bridge.node
    );
    let seq = bridge.state.reserve_next_sequence(bridge.inc).unwrap();
    let body = b"v=1 t=1 text=refused%20for%20good";
    bridge
        .state
        .set_in_flight(&InFlight {
            pin: plain_pin(seq, &face, body),
            subject: face.clone(),
            body: body.to_vec(),
            log: None,
            at: None,
        })
        .unwrap();
    bridge
        .state
        .refuse_writes_of(Some(crate::state::journal::FILE));
    bridge.drop_in_flight(seq);
    bridge.state.refuse_writes_of(None);
    assert!(bridge.settle_in_flight());
    let (mut reader, _) = bridge.connect().unwrap();
    let (rows, _) = reader.fetch(0, &face, 8).unwrap();
    assert!(
        rows.is_empty(),
        "a record the bridge dropped was published at the next attach: {rows:?}"
    );
}

#[derive(Clone, Copy, Debug)]
enum UncertainJournal {
    Listing,
    Entry,
    Damaged,
}

fn assert_uncertain_in_flight_waits(kind: UncertainJournal) {
    let (lane, mut bridge) = FakeVerbLane::bridge(&[]);
    let (socket, _broker, _server) = serve_log(&lane, "broker");
    bridge.cfg.broker = socket;
    bridge.conn = Some(bridge.connect().unwrap().0.into());
    // The floor the attach this skips takes from the empty log's head.
    bridge.floor_at_the_head(0);
    assert!(bridge.settle_in_flight(), "no journal is no pending work");
    let face = format!(
        "/f/{}/in/{}/s-b/{}/note",
        bridge.cfg.fleet, bridge.node, bridge.node
    );
    let seq = bridge.state.reserve_next_sequence(bridge.inc).unwrap();
    let body = b"v=1 t=1 text=already%20landed";
    bridge
        .state
        .set_in_flight(&InFlight {
            pin: plain_pin(seq, &face, body),
            subject: face.clone(),
            body: body.to_vec(),
            log: None,
            at: None,
        })
        .unwrap();
    let (mut reader, _) = bridge.connect().unwrap();
    let (off, deduped) = reader
        .publish(bridge.producer_id, seq, &face, body)
        .unwrap();
    assert!(!deduped);
    let root = bridge.state.root().to_path_buf();
    let dir = root.join(crate::state::journal::DIR);
    let entry = root.join(crate::state::journal::FILE);
    let (blocked, saved) = match kind {
        UncertainJournal::Listing => (dir.clone(), dir.with_extension("saved")),
        UncertainJournal::Entry | UncertainJournal::Damaged => (entry.clone(), dir.join(".saved")),
    };
    std::fs::rename(&blocked, &saved).unwrap();
    match kind {
        UncertainJournal::Entry => std::fs::create_dir(&blocked).unwrap(),
        UncertainJournal::Listing | UncertainJournal::Damaged => {
            std::fs::write(&blocked, b"not a journal entry\n").unwrap();
        }
    }
    bridge.state = StateDir::open(&root).unwrap();
    assert!(
        !bridge.settle_in_flight(),
        "{kind:?}: uncertain publication fate was accepted as settled"
    );
    assert!(
        saved.exists(),
        "{kind:?}: the original journal was discarded"
    );
    assert!(
        blocked.exists(),
        "{kind:?}: the uncertain entry was discarded"
    );
    match kind {
        UncertainJournal::Entry => std::fs::remove_dir(&blocked).unwrap(),
        UncertainJournal::Listing | UncertainJournal::Damaged => {
            std::fs::remove_file(&blocked).unwrap();
        }
    }
    std::fs::rename(&saved, &blocked).unwrap();
    assert!(
        bridge.settle_in_flight(),
        "{kind:?}: the restored journal recovers"
    );
    assert!(
        bridge.self_acked.contains(&off),
        "the already-landed offset is ours"
    );
    assert!(
        bridge.state.self_acked().contains(&off),
        "the offset is remembered on disk"
    );
    assert!(
        bridge.state.in_flight().unwrap().is_empty()
            && StateDir::open(&root)
                .unwrap()
                .in_flight()
                .unwrap()
                .is_empty(),
        "the known publication is settled"
    );
    let (rows, _) = reader.fetch(0, &face, 8).unwrap();
    assert_eq!(
        rows.len(),
        1,
        "same-sequence recovery duplicated the record: {rows:?}"
    );
    assert_eq!(rows[0].0, off);
}

#[test]
fn an_unlistable_in_flight_journal_stops_recovery_until_it_can_be_read() {
    assert_uncertain_in_flight_waits(UncertainJournal::Listing);
}

#[test]
fn an_unreadable_in_flight_entry_stops_recovery_until_it_can_be_read() {
    assert_uncertain_in_flight_waits(UncertainJournal::Entry);
}

#[test]
fn a_damaged_in_flight_entry_stops_recovery_until_it_is_repaired() {
    assert_uncertain_in_flight_waits(UncertainJournal::Damaged);
}

fn leave_journaled_publications(
    bridge: &mut Bridge,
    records: &[(String, Vec<u8>)],
) -> Vec<(u64, u64)> {
    let refused = bridge.state.refuse_writes_of(Some("acked"));
    let mut published = Vec::new();
    for (subject, body) in records {
        let pin = plain_pin(bridge.next_seq().unwrap(), subject, body);
        let (off, deduped, _) = bridge.publish_journaled(&pin, subject, body).unwrap();
        assert!(!deduped);
        published.push((pin.seq, off));
    }
    bridge.state.refuse_writes_of(refused.as_deref());
    assert_eq!(bridge.state.in_flight().unwrap().len(), records.len());
    published
}

#[cfg(unix)]
#[test]
fn referenced_journal_rows_do_not_cross_a_log_identity() {
    const SID: &str = "s-local";
    let (lane, mut bridge) = FakeVerbLane::bridge(&[(1, SID)]);
    lane.answer_hello_with(&format!("OK lane={LINK_LOG_LANE} build=test\n"));
    bridge.introduce();
    assert!(bridge.refresh_sessions().is_ok());
    let (a, _broker_a, _server_a) = serve_log(&lane, "journal-a");
    bridge.cfg.broker = a;
    assert!(bridge.attach_broker());
    let task = format!("/f/f/in/{}/{SID}/h-writer/task", bridge.node);
    let ack = format!("/f/f/in/{}/{SID}/{}/ack", bridge.node, bridge.node);
    let note = format!("/f/f/in/{}/{SID}/{}/note", bridge.node, bridge.node);
    let (mut writer, _) = bridge.connect().unwrap();
    let (old, _) = writer.publish(81, 1, &task, b"v=1 t=1 text=old").unwrap();
    let mut receipt = Body::new(1);
    receipt.re = Some(old);
    receipt.from = Some(SID.to_string());
    receipt.verdict = Some("handled".to_string());
    receipt.text = "old receipt".to_string();
    let published = leave_journaled_publications(
        &mut bridge,
        &[
            (ack.clone(), receipt.encode()),
            (note.clone(), b"v=1 t=1 text=keep-me".to_vec()),
        ],
    );

    bridge.close_subscriptions();
    bridge.conn = None;
    let (b, _broker_b, _server_b) = serve_log(&lane, "journal-b");
    bridge.cfg.broker = b;
    let (mut writer, _) = bridge.connect().unwrap();
    for seq in 0..old {
        writer
            .publish(91, seq + 1, "/f/f/x/history", b"v=1")
            .unwrap();
    }
    let (reused, _) = writer
        .publish(91, old + 1, &task, b"v=1 t=2 text=different")
        .unwrap();
    assert_eq!(reused, old);
    *lane.log_answer.lock().unwrap() = "ERR busy\n".to_string();
    assert!(!bridge.attach_broker());
    assert_eq!(
        bridge.state.in_flight().unwrap().len(),
        2,
        "no clock ack, no disposal"
    );
    *lane.log_answer.lock().unwrap() = "OK forgot=1\n".to_string();
    assert!(bridge.attach_broker());
    let (acks, _) = writer.fetch(0, &ack, 8).unwrap();
    assert!(
        acks.is_empty(),
        "A's receipt now answers B's different task: {acks:?}"
    );
    let (notes, _) = writer.fetch(0, &note, 8).unwrap();
    assert_eq!(notes.len(), 1, "no-reference publication must recover");
    assert_eq!(Body::decode(&notes[0].2).0.text, "keep-me");
    assert!(bridge.state.in_flight().unwrap().is_empty());
    assert!(
        StateDir::open(bridge.state.root())
            .unwrap()
            .in_flight()
            .unwrap()
            .is_empty(),
        "nothing in flight on disk either: {published:?}"
    );
}

#[cfg(unix)]
#[test]
fn referenced_journal_rows_recover_on_the_same_identified_log() {
    let (lane, mut bridge) = FakeVerbLane::bridge(&[]);
    let (socket, _broker, _server) = serve_log(&lane, "journal-same");
    bridge.cfg.broker = socket;
    assert!(bridge.attach_broker());
    let subject = format!("/f/f/in/{}/s-local/{}/ack", bridge.node, bridge.node);
    let body = b"v=1 t=1 re=0 verdict=handled text=same".to_vec();
    let published = leave_journaled_publications(&mut bridge, &[(subject.clone(), body)]);
    bridge.close_subscriptions();
    bridge.conn = None;
    assert!(bridge.attach_broker());
    let (rows, _) = bridge.conn_now().unwrap().fetch(0, &subject, 8).unwrap();
    assert_eq!(rows.len(), 1);
    assert_eq!(
        rows[0].0, published[0].1,
        "recovery dedups the same publication"
    );
    assert!(bridge.state.in_flight().unwrap().is_empty());
    assert!(bridge.state.self_acked().contains(&rows[0].0));
}

#[cfg(unix)]
#[test]
fn referenced_journal_rows_without_a_log_witness_remain_uncertain() {
    let (lane, mut bridge) = FakeVerbLane::bridge(&[]);
    let (socket, _broker, _server) = serve_log(&lane, "journal-legacy");
    bridge.cfg.broker = socket;
    assert!(bridge.attach_broker());
    let subject = format!("/f/f/in/{}/s-local/{}/ack", bridge.node, bridge.node);
    let published = leave_journaled_publications(
        &mut bridge,
        &[(
            subject,
            b"v=1 t=1 re=0 verdict=handled text=legacy".to_vec(),
        )],
    );
    let seq = published[0].0;
    let (_, entry) = bridge.state.in_flight().unwrap().remove(0);
    let legacy = entry
        .render()
        .lines()
        .take(3)
        .collect::<Vec<_>>()
        .join("\n");
    bridge.state.clear_in_flight(seq).unwrap();
    let root = bridge.state.root().to_path_buf();
    std::fs::create_dir_all(root.join("inflight")).unwrap();
    let path = root.join("inflight").join(seq.to_string());
    std::fs::write(&path, &legacy).unwrap();
    bridge.state = StateDir::open(&root).unwrap();
    bridge.close_subscriptions();
    bridge.conn = None;
    assert!(
        !bridge.attach_broker(),
        "an older build's reference was assigned this log without evidence"
    );
    assert_eq!(
        bridge.state.in_flight().unwrap(),
        vec![(
            seq,
            InFlight {
                log: None,
                at: None,
                ..entry
            }
        )],
        "uncertainty is retained"
    );

    std::fs::write(&path, format!("{legacy}\nlog=not-an-identity")).unwrap();
    assert!(
        StateDir::open(&root).unwrap().in_flight().is_err(),
        "malformed identity is never legacy absence"
    );
}

#[cfg(unix)]
#[test]
fn referenced_journal_rows_need_the_current_connection_log_identity() {
    let (lane, mut bridge) = FakeVerbLane::bridge(&[]);
    let (socket, broker, _server) = serve_log(&lane, "journal-unconfirmed");
    bridge.cfg.broker = socket;
    assert!(bridge.attach_broker());
    let subject = format!("/f/f/in/{}/s-local/{}/ack", bridge.node, bridge.node);
    leave_journaled_publications(
        &mut bridge,
        &[(
            subject.clone(),
            b"v=1 t=1 re=0 verdict=handled text=known".to_vec(),
        )],
    );
    bridge.conn = Some(bridge.connect().unwrap().0.into());
    assert_eq!(
        bridge.bus_log,
        Some(broker.log_identity()),
        "the old witness is retained"
    );
    assert_eq!(bridge.conn.as_ref().unwrap().seen_log_identity(), None);
    assert!(
        !bridge.settle_in_flight(),
        "a remembered witness was assigned to a silent connection"
    );
    assert_eq!(bridge.state.in_flight().unwrap().len(), 1);
    assert_eq!(
        bridge.read_log_identity().unwrap(),
        Some(broker.log_identity())
    );
    assert!(
        bridge.settle_in_flight(),
        "the actual same-log answer permits recovery"
    );

    bridge.conn = Some(bridge.connect().unwrap().0.into());
    leave_journaled_publications(
        &mut bridge,
        &[(
            subject,
            b"v=1 t=1 re=0 verdict=handled text=unknown".to_vec(),
        )],
    );
    assert_eq!(bridge.state.in_flight().unwrap()[0].1.log, None);
}

#[cfg(unix)]
#[test]
fn a_publish_with_no_connection_journals_nothing_and_the_attach_goes_on() {
    let (lane, mut bridge) = FakeVerbLane::bridge(&[]);
    let (socket, broker, _server) = serve_log(&lane, "journal-offline");
    bridge.cfg.broker = socket;
    assert!(bridge.attach_broker());
    let subject = format!("/f/f/in/{}/s-local/{}/ack", bridge.node, bridge.node);
    let body = b"v=1 t=1 re=0 verdict=handled text=offline".to_vec();
    bridge.close_subscriptions();
    bridge.conn = None;
    let pin = plain_pin(bridge.next_seq().unwrap(), &subject, &body);
    let e = bridge
        .publish_journaled(&pin, &subject, &body)
        .expect_err("no broker to publish to");
    assert_eq!(e.kind(), io::ErrorKind::NotConnected);
    assert!(
        bridge.state.in_flight().unwrap().is_empty(),
        "an entry that names no log was journaled for a publish that could not be sent"
    );
    let post = QueuedPost {
        sid: "s-local".to_string(),
        id: 1,
        to: "s-local".to_string(),
        kind: "note".to_string(),
        re: Some(0),
        dl: None,
        via: None,
        key: None,
        body: b"offline reply".to_vec(),
    };
    let reply = b"v=1 t=1 re=0 text=offline%20reply".to_vec();
    let reply_pin = plain_pin(bridge.next_seq().unwrap(), &subject, &reply);
    assert!(
        !bridge.send_post(post, reply_pin, subject.clone(), reply),
        "the drain ends with the link"
    );
    assert!(bridge.state.in_flight().unwrap().is_empty());

    bridge.conn = Some(bridge.connect().unwrap().0.into());
    assert_eq!(
        bridge.read_log_identity().unwrap(),
        Some(broker.log_identity())
    );
    let witnessed = InFlight {
        pin: pin.clone(),
        subject: subject.clone(),
        body: body.clone(),
        log: Some(broker.log_identity()),
        at: None,
    };
    bridge.state.set_in_flight(&witnessed).unwrap();
    bridge.close_subscriptions();
    bridge.conn = None;
    assert!(bridge.publish_journaled(&pin, &subject, &body).is_err());
    assert_eq!(
        bridge.state.in_flight().unwrap(),
        vec![(pin.seq, witnessed)],
        "the retry left the entry as its own publish wrote it"
    );
    assert!(bridge.attach_broker(), "the witnessed entry is recovered");
    assert!(bridge.state.in_flight().unwrap().is_empty());
}

#[test]
fn the_attach_relearns_its_own_in_flight_records_and_no_one_elses() {
    let (lane, mut bridge) = FakeVerbLane::bridge(&[]);
    let socket = lane.dir.path().join("broker.sock");
    let broker = crate::test_broker(lane.dir.path().join("broker.log")).unwrap();
    let _server = broker.serve(&socket).unwrap();
    bridge.cfg.broker = socket.to_string_lossy().into_owned();
    bridge.conn = Some(bridge.connect().unwrap().0.into());
    // The floor the attach this skips takes from the empty log's head.
    bridge.floor_at_the_head(0);
    let (mut other, _) = bridge.connect().unwrap();
    let face = format!(
        "/f/{}/in/{}/s-b/{}/note",
        bridge.cfg.fleet, bridge.node, bridge.node
    );
    let in_flight = |bridge: &Bridge, body: &[u8]| {
        let seq = bridge.state.reserve_next_sequence(bridge.inc).unwrap();
        let entry = InFlight {
            pin: plain_pin(seq, &face, body),
            subject: face.clone(),
            body: body.to_vec(),
            log: None,
            at: None,
        };
        bridge.state.set_in_flight(&entry).unwrap();
        seq
    };
    let landed = in_flight(&bridge, b"v=1 t=1 text=landed");
    let (landed_off, _) = other
        .publish(bridge.producer_id, landed, &face, b"v=1 t=1 text=landed")
        .unwrap();
    in_flight(&bridge, b"v=1 t=2 text=never%20sent");
    let claimed = in_flight(&bridge, b"v=1 t=3 text=ours");
    let (claimed_off, _) = other
        .publish(bridge.producer_id, claimed, &face, b"v=1 t=3 text=theirs")
        .unwrap();

    assert!(
        !bridge.settle_in_flight(),
        "the collision drops the connection, so the attach stops"
    );
    let (rows, _) = other.fetch(0, &face, 16).unwrap();
    let offs: Vec<(u64, String)> = rows
        .iter()
        .map(|(off, _, body)| (*off, String::from_utf8_lossy(body).into_owned()))
        .collect();
    assert_eq!(offs.len(), 3, "one record per sequence: {offs:#?}");
    let sent_off = offs
        .iter()
        .find(|(_, body)| body.contains("never%20sent"))
        .map(|(off, _)| *off)
        .expect("the record that never left is published by the re-learn");
    for off in [landed_off, sent_off] {
        assert!(bridge.self_acked.contains(&off), "@{off} is ours");
        assert!(bridge.state.self_acked().contains(&off), "@{off} on disk");
    }
    assert!(
        !bridge.self_acked.contains(&claimed_off)
            && !bridge.state.self_acked().contains(&claimed_off),
        "another writer's record is never whitelisted"
    );
    assert_eq!(bridge.collided, Some((claimed, claimed_off)));
    assert!(
        bridge.state.in_flight().unwrap().is_empty(),
        "every entry is settled"
    );
}

#[test]
fn a_reply_entry_cut_before_its_witness_is_set_aside_and_the_next_boot_attaches() {
    for sweep in [true, false] {
        let (lane, mut bridge) = FakeVerbLane::bridge(&[]);
        bridge.attach_past_an_earlier_boots_run = !sweep;
        let socket = lane.dir.path().join("broker.sock");
        let broker = crate::test_broker(lane.dir.path().join("broker.log")).unwrap();
        let _server = broker.serve(&socket).unwrap();
        bridge.cfg.broker = socket.to_string_lossy().into_owned();
        bridge.simulate_boot(crate::durability::test_boot(1), 0);
        bridge.begin_run();
        assert!(bridge.attach_broker(), "boot 1");
        let face = format!(
            "/f/{}/in/{}/s-b/{}/ack",
            bridge.cfg.fleet, bridge.node, bridge.node
        );
        let body = b"v=1 t=1 re=1 verdict=handled".to_vec();
        let pin = plain_pin(bridge.next_seq().unwrap(), &face, &body);
        let text = InFlight {
            pin: pin.clone(),
            subject: face.clone(),
            body,
            log: Some(broker.log_identity()),
            at: None,
        }
        .render();
        bridge.close_subscriptions();
        bridge.conn = None;
        let root = bridge.state.root().to_path_buf();
        std::fs::create_dir_all(root.join("inflight")).unwrap();
        let entry = root.join(format!("inflight/{}", pin.seq));
        let cut = &text[..text.find("log=").expect("its witness")];
        std::fs::write(&entry, cut).unwrap();
        bridge.state = StateDir::open(&root).unwrap();
        bridge.simulate_boot(crate::durability::test_boot(2), 0);
        if sweep {
            bridge.set_aside_torn_state();
            assert_eq!(bridge.set_aside, 1);
        }
        assert_eq!(
            bridge.attach_broker(),
            sweep,
            "sweep={sweep}: boot 2's attach"
        );
        if sweep {
            assert!(bridge.state.in_flight().unwrap().is_empty());
            assert_eq!(
                std::fs::read_to_string(
                    bridge
                        .state
                        .root()
                        .join(format!("quarantine/inflight.{}", pin.seq))
                )
                .unwrap(),
                cut,
                "its exact bytes, kept"
            );
        }
        bridge.close_subscriptions();
    }
}

#[test]
fn the_quarantine_count_is_told_and_told_again_as_the_owner_empties_it() {
    for endpoint_lane in [aterm_types::control_verbs::BRIDGE_LANE, 4] {
        let (lane, mut bridge) = FakeVerbLane::bridge(&[]);
        bridge.endpoint_lane = endpoint_lane;
        let reads = endpoint_lane >= LINK_QUARANTINE_LANE;
        let root = bridge.state.root().to_path_buf();
        bridge.simulate_boot(crate::durability::test_boot(1), 0);
        bridge.begin_run();
        std::fs::create_dir_all(root.join("inflight")).unwrap();
        std::fs::write(root.join("inflight/8"), b"v=1 cut").unwrap();
        bridge.simulate_boot(crate::durability::test_boot(2), 0);
        bridge.set_aside_torn_state();
        bridge.tell_set_aside();
        bridge.tell_quarantine();
        let told = |n: usize| lane.links()[n..].to_vec();
        if !reads {
            assert_eq!(told(0), ["link set-aside files=1"], "lane {endpoint_lane}");
            assert!(!bridge.watches_quarantine());
            continue;
        }
        assert_eq!(
            told(0),
            ["link set-aside files=1", "link quarantine torn=1"]
        );
        assert!(bridge.watches_quarantine(), "a count above 0 is watched");
        bridge.tell_quarantine();
        assert_eq!(told(2), Vec::<String>::new(), "nothing moved, nothing said");
        std::fs::remove_file(root.join("quarantine/inflight.8")).unwrap();
        assert!(bridge.watches_quarantine());
        bridge.tell_quarantine();
        assert_eq!(told(2), ["link quarantine torn=0"], "the owner emptied it");
        assert!(
            !bridge.watches_quarantine(),
            "and it is no longer looked at"
        );
        bridge.quarantine_said = None;
        bridge.tell_quarantine();
        assert_eq!(told(3), ["link quarantine torn=0"], "said at every start");
        std::fs::remove_dir(root.join("quarantine")).unwrap();
        bridge.quarantine_said = None;
        bridge.tell_quarantine();
        assert_eq!(
            told(4),
            ["link quarantine torn=0"],
            "a quarantine removed whole holds none"
        );
    }
}

#[test]
fn the_quarantine_emptied_is_told_while_the_broker_is_away() {
    let (lane, mut bridge) = FakeVerbLane::bridge(&[]);
    bridge.endpoint_lane = aterm_types::control_verbs::BRIDGE_LANE;
    let root = bridge.state.root().to_path_buf();
    bridge.simulate_boot(crate::durability::test_boot(1), 0);
    bridge.begin_run();
    std::fs::create_dir_all(root.join("inflight")).unwrap();
    std::fs::write(root.join("inflight/8"), b"v=1 cut").unwrap();
    bridge.simulate_boot(crate::durability::test_boot(2), 0);
    bridge.set_aside_torn_state();
    bridge.tell_set_aside();
    bridge.tell_quarantine();
    assert_eq!(
        lane.links(),
        ["link set-aside files=1", "link quarantine torn=1"]
    );
    std::fs::remove_file(root.join("quarantine/inflight.8")).unwrap();
    let quarantine_said = |lane: &FakeVerbLane| {
        lane.links()
            .into_iter()
            .filter(|l| l.starts_with("link quarantine"))
            .collect::<Vec<_>>()
    };
    let mut backoff = Backoff::default();
    bridge.roster_due = bridge.fx.now();
    assert!(bridge.pass(&mut backoff).is_none(), "the bridge runs on");
    assert!(bridge.conn.is_none(), "no broker was reached");
    assert!(
        lane.links().iter().any(|l| l.starts_with("link down")),
        "the attach failed: {:?}",
        lane.links()
    );
    assert_eq!(
        quarantine_said(&lane),
        ["link quarantine torn=1", "link quarantine torn=0"],
        "the owner emptied it, and aterm heard it with no broker"
    );
    assert!(!bridge.watches_quarantine());
    bridge.roster_due = bridge.fx.now();
    assert!(bridge.pass(&mut backoff).is_none());
    assert_eq!(
        quarantine_said(&lane).len(),
        2,
        "a count no longer watched is not looked at again"
    );
    bridge.close_subscriptions();
}

/// A QUIT WHOSE HAND-OFF THE DISK REFUSES IS NO CLEAN STOP (B19 review): the journal keeps a
/// record an older build would not find, so the run record stays open, and the next bridge
/// of this build reads the record from the journal, which names that run. CONTROL: the same
/// quit with its hand-off written stops cleanly, and the next bridge takes the file back.
#[test]
fn a_quit_whose_hand_off_the_disk_refuses_leaves_the_run_open() {
    for refused in [true, false] {
        let (_lane, mut bridge) = FakeVerbLane::bridge(&[]);
        bridge.simulate_boot(crate::durability::test_boot(1), 0);
        bridge.begin_run();
        let pin = plain_pin(7, "/f/f/in/n-1/s-b/n-1/note", b"v=1 t=1");
        bridge.state.set_post_pin("s-a", 1, &pin).unwrap();
        if refused {
            bridge.state.refuse_writes_of(Some("sent/s-a.1"));
        }
        bridge.bye = Some(Bye::Quit);
        bridge.end_run();
        bridge.state.refuse_writes_of(None);
        let run = bridge.state.last_run().unwrap().expect("a run record");
        assert_eq!(run.clean, !refused, "refused={refused}");
        let next = StateDir::open(bridge.state.root()).unwrap();
        assert_eq!(
            next.post_pin("s-a", 1).unwrap(),
            Some(pin),
            "refused={refused}"
        );
    }
}

#[test]
fn a_handoff_leaves_the_run_to_its_successor_and_a_quit_stops_it_cleanly() {
    for (bye, proof) in [
        (Bye::Handoff, true),
        (Bye::Handoff, false),
        (Bye::Quit, true),
    ] {
        let (_lane, mut bridge) = FakeVerbLane::bridge(&[]);
        bridge.simulate_boot(crate::durability::test_boot(1), 0);
        bridge.begin_run();
        let pin = plain_pin(7, "/f/f/in/n-1/s-b/n-1/note", b"v=1 t=1");
        bridge.state.set_post_pin("s-a", 1, &pin).unwrap();
        bridge.bye = Some(bye);
        bridge.end_run();
        let run = bridge.state.last_run().unwrap().expect("a run record");
        assert_eq!(
            run.clean,
            bye == Bye::Quit,
            "{bye:?}: a handoff leaves the run open"
        );
        if !proof {
            std::fs::remove_file(
                bridge
                    .state
                    .proof_of_what_reads("run")
                    .expect("the record reads"),
            )
            .expect("the proof the bridge's start in full made");
        }
        let successor = StateDir::open(bridge.state.root()).unwrap();
        successor.set_inc_seam(crate::state::IncSeam {
            boot: Some(Some(crate::durability::test_boot(1))),
            kill_at: None,
            mutant: 0,
            span: None,
            off_volume: false,
        });
        let began = successor.begin_run().unwrap();
        let stopped = successor.end_run_clean().unwrap();
        match bye {
            Bye::Handoff if proof => {
                assert_eq!(
                    (began.ordered, began.full, began.proven),
                    (0, 0, true),
                    "the successor admits the OPEN its predecessor published in full by its \
                     proof, writing nothing (R6)"
                );
                assert_eq!(
                    stopped.files, 2,
                    "the reservation and the heads kept beside it, inherited and flushed"
                );
            }
            Bye::Handoff => {
                assert_eq!(
                    (began.ordered, began.full, began.proven),
                    (0, 2, false),
                    "CONTROL: with no proof, the successor admits OPEN independently, in full"
                );
                assert_eq!(
                    stopped.files, 2,
                    "the reservation and the heads kept beside it, inherited and flushed"
                );
            }
            Bye::Quit => {
                assert_eq!(
                    (began.ordered, began.full),
                    (0, 2),
                    "CONTROL: the record written again, in full over a clean stop"
                );
                assert_eq!(stopped.files, 0, "CONTROL: nothing inherited");
            }
        }
    }
}

#[test]
fn an_in_flight_entry_of_an_earlier_boot_is_retired_and_its_attach_publishes_nothing() {
    for reboot in [true, false] {
        let (lane, mut bridge) = FakeVerbLane::bridge(&[]);
        let socket = lane.dir.path().join("broker.sock");
        let broker = crate::test_broker(lane.dir.path().join("broker.log")).unwrap();
        let _server = broker.serve(&socket).unwrap();
        bridge.cfg.broker = socket.to_string_lossy().into_owned();
        bridge.simulate_boot(crate::durability::test_boot(1), 0);
        bridge.begin_run();
        assert!(bridge.attach_broker(), "boot 1");
        let face = format!(
            "/f/{}/in/{}/s-b/{}/note",
            bridge.cfg.fleet, bridge.node, bridge.node
        );
        let body = b"v=1 t=1 from=s-a text=landed%20then%20lost".to_vec();
        let pin = plain_pin(bridge.next_seq().unwrap(), &face, &body);
        bridge
            .state
            .set_in_flight(&InFlight {
                pin: pin.clone(),
                subject: face.clone(),
                body,
                log: Some(broker.log_identity()),
                at: None,
            })
            .unwrap();
        bridge.close_subscriptions();
        bridge.conn = None;
        let boot = if reboot { 2 } else { 1 };
        bridge.simulate_boot(crate::durability::test_boot(boot), 0);
        bridge.set_aside_torn_state();
        bridge.retire_in_flight_state();
        bridge.begin_run();
        assert!(bridge.attach_broker(), "boot {boot}'s attach");
        let (mut reader, _) = bridge.connect().unwrap();
        let (rows, _) = reader.fetch(0, &face, 16).unwrap();
        assert!(
            bridge.state.in_flight().unwrap().is_empty(),
            "boot {boot}: the entry is settled either way"
        );
        let retired = bridge
            .state
            .root()
            .join(format!("quarantine/retired.inflight.{}", pin.seq));
        if reboot {
            assert!(rows.is_empty(), "a retired entry was published: {rows:#?}");
            assert!(retired.exists(), "its bytes are kept in the quarantine");
        } else {
            assert_eq!(
                rows.len(),
                1,
                "CONTROL: within one boot the attach publishes it: {rows:#?}"
            );
            assert!(!retired.exists(), "nothing is retired within one boot");
        }
        bridge.close_subscriptions();
    }
}

#[cfg(unix)]
#[test]
fn no_attach_runs_while_run_names_an_earlier_boot_so_a_relaunch_retires_nothing_live() {
    for reopen in [false, true] {
        let (lane, mut bridge) = FakeVerbLane::bridge(&[]);
        let (socket, _broker, _server) = serve_log(&lane, "open-fence");
        bridge.cfg.broker = socket;
        bridge.simulate_boot(crate::durability::test_boot(1), 0);
        bridge.begin_run();
        bridge.state.end_run_clean().unwrap();
        let root = bridge.state.root().to_path_buf();
        let refusal = (
            root.as_path(),
            aterm_durable::Flush::Durable,
            io::ErrorKind::PermissionDenied,
        );
        bridge.state.refuse_directory_flush(Some(refusal));
        bridge.begin_run();
        assert!(
            !bridge.state.last_run().unwrap().unwrap().clean,
            "OPEN is visible"
        );
        if reopen {
            bridge.state = StateDir::open(&root).unwrap();
            bridge.simulate_boot(crate::durability::test_boot(1), 0);
        }
        bridge.state.refuse_directory_flush(Some(refusal));
        assert!(!bridge.attach_broker(), "unadmitted OPEN, reopen={reopen}");
        assert!(bridge.conn.is_none());
        assert_eq!(bridge.state.incarnation(), 0);
        let (mut reader, _) = bridge.connect().unwrap();
        let (rows, _) = reader
            .fetch(0, &format!("/f/{}/>", bridge.cfg.fleet), 64)
            .unwrap();
        assert!(rows.is_empty(), "a refused admission published {rows:?}");
        bridge.state.refuse_directory_flush(None);
        assert!(bridge.attach_broker(), "publication retry, reopen={reopen}");
        bridge.close_subscriptions();
    }
    // Off the boot volume the lease of the attach past the wait names its boot (item RZ2),
    // so only an older build's write there (`legacy`), which names none, leaves the guard
    // nothing to read: the negative control.
    for (wait, on_volume, legacy) in [
        (true, false, false),
        (false, false, false),
        (false, true, false),
        (false, false, true),
    ] {
        let case = format!("wait={wait} on_volume={on_volume} legacy={legacy}");
        let (lane, mut bridge) = FakeVerbLane::bridge(&[]);
        let (socket, _broker, _server) = serve_log(&lane, "f2-run");
        bridge.cfg.broker = socket;
        bridge.attach_past_an_earlier_boots_run = !wait;
        bridge.state.lease_names_no_boot_off_the_boot_volume(legacy);
        let in_boot = |bridge: &Bridge, n| {
            bridge.state.set_inc_seam(crate::state::IncSeam {
                boot: Some(Some(crate::durability::test_boot(n))),
                off_volume: !on_volume,
                kill_at: None,
                mutant: 0,
                span: None,
            });
        };
        in_boot(&bridge, 1);
        bridge.begin_run();
        in_boot(&bridge, 2);
        bridge.state.refuse_writes_of(Some("run"));
        bridge.set_aside_torn_state();
        bridge.retire_in_flight_state();
        bridge.begin_run();
        if wait {
            assert!(
                !bridge.attach_broker(),
                "{case}: an attach ran while `run` named boot 1's unclean stop"
            );
            assert!(bridge.conn.is_none(), "{case}: no connection is kept");
            assert_eq!(bridge.state.incarnation(), 0, "{case}: no incarnation");
            let (mut reader, _) = bridge.connect().unwrap();
            let (rows, _) = reader
                .fetch(0, &format!("/f/{}/>", bridge.cfg.fleet), 64)
                .unwrap();
            assert!(rows.is_empty(), "{case}: the attach wrote: {rows:#?}");
            bridge.state.refuse_writes_of(None);
        }
        assert!(bridge.attach_broker(), "{case}: boot 2's attach");
        if wait {
            assert_eq!(
                bridge.state.last_run().unwrap(),
                Some(crate::state::LastRun {
                    boot: Some(crate::durability::test_boot(2)),
                    clean: false,
                }),
                "{case}: the attach wrote `run` before anything else"
            );
        }
        let subject = format!(
            "/f/{}/in/{}/s-local/{}/note",
            bridge.cfg.fleet, bridge.node, bridge.node
        );
        let published =
            leave_journaled_publications(&mut bridge, &[(subject, b"v=1 t=1 text=live".to_vec())]);
        let (seq, off) = published[0];
        bridge.close_subscriptions();
        bridge.conn = None;
        bridge.load_self_window();
        bridge.set_aside_torn_state();
        bridge.retire_in_flight_state();
        bridge.begin_run();
        assert!(bridge.attach_broker(), "{case}: the relaunch's attach");
        let retired = bridge
            .state
            .root()
            .join(format!(
                "{}/{}inflight.{seq}",
                crate::state::QUARANTINE,
                crate::state::RETIRED_PREFIX
            ))
            .exists();
        let relearned = bridge.state.self_acked().contains(&off);
        let accused = bridge.is_forged_self(off);
        if wait || on_volume || !legacy {
            assert!(
                !retired && relearned && !accused,
                "{case}: retired={retired} relearned={relearned} accused={accused}"
            );
        } else {
            assert!(
                retired && !relearned && accused,
                "{case}: NEGATIVE CONTROL: retired={retired} relearned={relearned} \
                 accused={accused}"
            );
        }
        bridge.close_subscriptions();
    }
}

#[cfg(unix)]
#[test]
fn no_attach_runs_under_a_clean_stops_record_so_the_next_boot_sweeps_what_it_wrote() {
    for widened in [true, false] {
        let case = format!("widened={widened}");
        let (lane, mut bridge) = FakeVerbLane::bridge(&[]);
        let (socket, broker, _server) = serve_log(&lane, "s7-run");
        bridge.cfg.broker = socket;
        bridge.attach_past_an_earlier_boots_run = !widened;
        bridge.simulate_boot(crate::durability::test_boot(1), 0);
        bridge.begin_run();
        bridge.state.end_run_clean().unwrap();
        bridge.state.refuse_writes_of(Some("run"));
        if bridge.recover() {
            bridge.begin_run();
        }
        let clean_one = Some(crate::state::LastRun {
            boot: Some(crate::durability::test_boot(1)),
            clean: true,
        });
        assert_eq!(bridge.state.last_run().unwrap(), clean_one, "{case}");
        if widened {
            assert!(
                !bridge.attach_broker(),
                "{case}: an attach ran under a clean stop's record"
            );
            assert!(bridge.conn.is_none(), "{case}: no connection is kept");
            assert_eq!(bridge.state.incarnation(), 0, "{case}: no incarnation");
            bridge.state.refuse_writes_of(None);
        }
        assert!(bridge.attach_broker(), "{case}: boot 1's attach");
        let run = bridge.state.last_run().unwrap();
        if widened {
            assert_eq!(
                run,
                Some(crate::state::LastRun {
                    boot: Some(crate::durability::test_boot(1)),
                    clean: false,
                }),
                "{case}: the attach wrote `run` before anything else"
            );
        } else {
            assert_eq!(run, clean_one, "{case}: CONTROL: attached under it");
        }
        let face = format!(
            "/f/{}/in/{}/s-b/{}/note",
            bridge.cfg.fleet, bridge.node, bridge.node
        );
        let body = b"v=1 t=1 text=live".to_vec();
        let pin = plain_pin(bridge.next_seq().unwrap(), &face, &body);
        bridge
            .state
            .set_in_flight(&InFlight {
                pin: pin.clone(),
                subject: face,
                body,
                log: Some(broker.log_identity()),
                at: None,
            })
            .unwrap();
        bridge.close_subscriptions();
        bridge.conn = None;
        let root = bridge.state.root().to_path_buf();
        let journal = root.join(crate::state::journal::FILE);
        let mut torn = std::fs::read(&journal).unwrap();
        let last = torn.len() - 1;
        torn[last] ^= 0x01;
        std::fs::write(&journal, &torn).unwrap();
        assert_eq!(
            crate::state::journal::read(&torn).end,
            crate::state::journal::End::Torn,
            "PRECONDITION: a tear inside the last record's bytes, as a power cut leaves"
        );
        bridge.state = StateDir::open(&root).unwrap();
        bridge.attach_past_an_earlier_boots_run = false;
        bridge.simulate_boot(crate::durability::test_boot(2), 0);
        if bridge.recover() {
            bridge.begin_run();
        }
        assert_eq!(
            bridge.attach_broker(),
            widened,
            "{case}: boot 2's attach (the torn journal set aside, or refused)"
        );
        assert_eq!(
            root.join("quarantine/journal.posts").exists(),
            widened,
            "{case}: the torn journal is in the quarantine"
        );
        bridge.close_subscriptions();
    }
}

#[cfg(unix)]
#[test]
fn a_post_routes_on_a_tofu_pin_only_once_it_is_on_the_media() {
    for settle in [true, false] {
        let (_lane, mut bridge) = FakeVerbLane::bridge(&[]);
        bridge.route_on_a_pin_that_reads = !settle;
        let root = bridge.state.root().to_path_buf();
        bridge.state.refuse_directory_flush(Some((
            root.as_path(),
            aterm_durable::Flush::Durable,
            io::ErrorKind::Other,
        )));
        assert!(
            bridge.state.pin("s-x", "n-far").is_err(),
            "the first sight's flush failed"
        );
        bridge.state.refuse_directory_flush(None);
        assert_eq!(
            bridge.state.pins().get("s-x").map(String::as_str),
            Some("n-far"),
            "and it reads"
        );
        assert!(!bridge.state.on_the_media("pins"));
        let _ = bridge.pinned_node_for("s-x");
        assert_eq!(
            bridge.state.on_the_media("pins"),
            settle,
            "settle={settle}: the table written again before the route"
        );
        assert_eq!(
            bridge.state.pins().get("s-x").map(String::as_str),
            Some("n-far")
        );
    }
}

#[test]
fn a_bridge_whose_aterm_is_gone_writes_nothing_and_never_redials() {
    let (lane, mut bridge) = FakeVerbLane::bridge(&[]);
    let socket = lane.dir.path().join("broker.sock");
    let broker = crate::test_broker(lane.dir.path().join("broker.log")).unwrap();
    let _server = broker.serve(&socket).unwrap();
    bridge.cfg.broker = socket.to_string_lossy().into_owned();
    let (mut reader, _) = bridge.connect().unwrap();
    let state = bridge.state.root().to_path_buf();
    bridge.mailbox.push_aterm_closed();

    assert!(
        !bridge.attach_broker(),
        "an attach for an aterm that is gone completed"
    );
    assert_eq!(
        StateDir::open(&state).unwrap().incarnation(),
        0,
        "the attach took an incarnation"
    );
    let (rows, _) = reader.fetch(0, "/f/f/>", 64).unwrap();
    assert!(rows.is_empty(), "the attach wrote to the bus: {rows:#?}");

    let quiet = lane.dir.path().join("quiet.sock");
    let listener = std::os::unix::net::UnixListener::bind(&quiet).unwrap();
    bridge.cfg.broker = quiet.to_string_lossy().into_owned();
    bridge.run().expect("a bridge with no aterm ends cleanly");
    assert_eq!(
        aterm_uds::accept(
            &listener,
            aterm_uds::AcceptWait::Ready,
            aterm_uds::AcceptedMode::Blocking,
        )
        .map(|_| ())
        .map_err(|e| e.kind()),
        Err(io::ErrorKind::WouldBlock),
        "the loop redialled for an aterm that is gone"
    );
    let (rows, _) = reader.fetch(0, "/f/f/>", 64).unwrap();
    assert!(rows.is_empty(), "the loop wrote to the bus: {rows:#?}");
}

fn rig_with_broker(
    sessions: &[(u64, &str)],
) -> (FakeVerbLane, Bridge, astream_broker::BrokerHandle) {
    let (lane, bridge, server, _) = rig_with_kept_broker(sessions, false);
    (lane, bridge, server)
}

fn rig_with_kept_broker(
    sessions: &[(u64, &str)],
    retained: bool,
) -> (
    FakeVerbLane,
    Bridge,
    astream_broker::BrokerHandle,
    astream_broker::Broker,
) {
    rig_retained_by(sessions, retained.then(crate::broker_rules::default_rules))
}

/// [`rig_with_kept_broker`] over a broker retained by `rules` (none: unretained), set as
/// aterm sets its own at the open.
fn rig_retained_by(
    sessions: &[(u64, &str)],
    rules: Option<astream_broker::Retention>,
) -> (
    FakeVerbLane,
    Bridge,
    astream_broker::BrokerHandle,
    astream_broker::Broker,
) {
    let (lane, mut bridge) = FakeVerbLane::bridge(sessions);
    let socket = lane.dir.path().join("broker.sock");
    let broker = crate::test_broker(lane.dir.path().join("broker.log")).unwrap();
    let retained = rules.is_some();
    if let Some(rules) = rules {
        broker.set_retention(rules).unwrap();
    }
    let server = broker.serve(&socket).unwrap();
    bridge.cfg.broker = socket.to_string_lossy().into_owned();
    let (conn, _) = bridge.connect().unwrap();
    bridge.conn = Some(conn.into());
    bridge.note_ack(Duration::from_millis(1));
    assert!(bridge.link.up);
    if retained {
        bridge.read_log_identity().unwrap();
        bridge.note_broker_retention();
    }
    (lane, bridge, server, broker)
}

#[test]
fn a_face_at_its_bound_pauses_and_resumes_with_every_record_once_in_order() {
    use crate::mailbox::QUEUE_ROWS_MAX;
    let (lane, mut bridge, _server) = rig_with_broker(&[]);
    let inc = bridge.inc;
    let links = lane.links.lock().unwrap().len();
    let (mut publisher, _) = bridge.connect().unwrap();
    let lane_subject = format!("/f/f/in/{}/s-a/n-x/note", bridge.node);
    let total = QUEUE_ROWS_MAX + QUEUE_ROWS_MAX / 2 + 7;
    let published: Vec<u64> = publisher
        .publish_pipelined(77, &lane_subject, &vec![b"v=1".as_slice(); total], 64)
        .unwrap()
        .into_iter()
        .map(|(off, deduped)| {
            assert!(!deduped);
            off
        })
        .collect();

    assert!(bridge.subscribe_inbox());
    let deadline = Instant::now() + aterm_spec::wait::MUST_HAPPEN;
    while bridge.mailbox.paused(Source::Inbox).is_none() {
        assert!(Instant::now() < deadline, "the inbox reader never paused");
        aterm_uds::deadline::poll_yield();
    }
    assert_eq!(
        bridge.mailbox.paused(Source::Inbox),
        Some(published[QUEUE_ROWS_MAX - 1] + 1),
        "it pauses at the record that fills the queue, to resume after it"
    );

    let mut taken = Vec::new();
    let mut resumes = 0;
    while taken.len() < total {
        match bridge
            .mailbox
            .take(aterm_spec::wait::MUST_HAPPEN, bridge.fx.now())
        {
            Some(Item::Inbox((off, _, _), _)) => taken.push(off),
            Some(Item::Resume(Source::Inbox, from)) => {
                resumes += 1;
                bridge.resume_face(Source::Inbox, from);
                assert!(bridge.conn.is_some(), "a resume is not a reconnect");
            }
            other => panic!("after {} records: {other:?}", taken.len()),
        }
    }
    assert_eq!(taken, published, "every record once, in log order");
    assert!(resumes >= 1);
    assert_eq!(
        lane.links.lock().unwrap().len(),
        links,
        "a pause tells the endpoint nothing"
    );
    assert!(bridge.link.up);
    assert_eq!(bridge.inc, inc, "no new incarnation");
    bridge.close_subscriptions();
}

#[test]
fn a_duty_resumes_a_paused_fleet_face_before_the_next_halt() {
    use crate::mailbox::QUEUE_ROWS_MAX;

    const SID: &str = "s-busy";
    let (lane, mut bridge, _server) = rig_with_broker(&[(1, SID)]);
    bridge.refresh_sessions().unwrap();
    let inc = bridge.inc;
    let links = lane.links.lock().unwrap().len();
    let (mut writer, _) = bridge.connect().unwrap();
    let filled = writer
        .publish_pipelined(
            77,
            "/f/f/fleet/h-fill/barrier",
            &[b"v=1".as_slice(); QUEUE_ROWS_MAX],
            64,
        )
        .unwrap();
    assert!(filled.iter().all(|&(_, deduped)| !deduped));
    assert!(bridge.subscribe_fleet(0));
    aterm_spec::wait::until_within(
        aterm_spec::wait::MUST_HAPPEN,
        "the fleet reader to pause at its mailbox bound",
        || bridge.mailbox.paused(Source::Fleet).map(|_| ()),
    );

    bridge.service_urgent();
    let pending = bridge.mailbox.take(Duration::ZERO, bridge.fx.now());
    assert!(
        pending.is_none(),
        "the duty left fleet work for the outer loop: {pending:?}"
    );
    writer
        .publish(
            91,
            1,
            "/f/f/fleet/h-t/halt",
            b"v=1 t=3 state=on reason=stop",
        )
        .unwrap();
    aterm_spec::wait::until_within(
        aterm_spec::wait::MUST_HAPPEN,
        "the resumed fleet reader to queue the next halt",
        || (bridge.mailbox.replay_pressure() == ReplayPressure::Urgent).then_some(()),
    );
    bridge.service_urgent();
    assert!(bridge.halt_applied);
    assert!(lane
        .transcript()
        .contains(&format!("hold {SID} on reason=stop origin=fleet")));
    assert_eq!(bridge.inc, inc, "resuming the face is not a reconnect");
    assert!(lane.links.lock().unwrap()[links..]
        .iter()
        .all(|line| !line.starts_with("link down")));
    assert!(bridge.conn.is_some() && bridge.link.up);
    bridge.close_subscriptions();
}

/// The link's reason is the face's (`cut-<face>`) or `closed`, whichever end the reader saw
/// ([`End`]): the end changes only the line the bridge prints.
#[test]
fn a_cut_subscription_is_named_by_its_face_and_a_dead_broker_is_closed() {
    let (lane, mut bridge, mut server) = rig_with_broker(&[]);
    let last_link = |lane: &FakeVerbLane| lane.links.lock().unwrap().last().cloned();

    bridge.subscription_lost(Source::Say, End::Eof);
    assert!(bridge.conn.is_none(), "the set reconnects either way");
    assert_eq!(
        last_link(&lane).as_deref(),
        Some("link down reason=cut-say")
    );

    let (conn, _) = bridge.connect().unwrap();
    bridge.conn = Some(conn.into());
    bridge.note_ack(Duration::from_millis(1));
    server.shutdown();
    bridge.subscription_lost(Source::Inbox, End::Silent(Duration::from_secs(30)));
    assert!(bridge.conn.is_none());
    assert_eq!(last_link(&lane).as_deref(), Some("link down reason=closed"));
}

#[test]
fn the_broadcast_face_is_held_only_while_a_session_has_a_topic() {
    let (lane, mut bridge, _server) = rig_with_broker(&[(1, "s-a")]);
    bridge.refresh_sessions().unwrap();
    let (mut publisher, _) = bridge.connect().unwrap();
    let body = Body::new(0).encode();
    let mut shout = |seq: u64| {
        publisher
            .publish(77, seq, "/f/f/pub/n-src/s-src/say/t", &body)
            .unwrap()
            .0
    };
    let topic = |bridge: &mut Bridge, words: &str| {
        bridge.topic_change("s-a", &mut words.split_whitespace());
        assert!(bridge.sync_say_face());
    };
    let take_say = |bridge: &mut Bridge| match bridge
        .mailbox
        .take(aterm_spec::wait::MUST_HAPPEN, bridge.fx.now())
    {
        Some(Item::Say(r)) => {
            bridge.on_say_record(&r);
            r.0
        }
        other => panic!("{other:?}"),
    };

    assert!(bridge.sync_say_face());
    assert!(
        !bridge.say_face_held(),
        "no topic, no fleet-wide subscription"
    );
    let before = shout(1);
    topic(&mut bridge, &format!("add t since=@{before}"));
    assert!(bridge.say_face_held());
    let live = shout(2);
    assert_eq!(
        (take_say(&mut bridge), take_say(&mut bridge)),
        (before, live)
    );
    assert_eq!(lane.delivery_offsets(), [before, live]);

    let generation = bridge.mailbox.broker_generation(Source::Say);
    topic(&mut bridge, "drop t");
    assert!(!bridge.say_face_held(), "the last topic closes the face");
    assert_ne!(bridge.mailbox.broker_generation(Source::Say), generation);
    let unheard = shout(3);

    topic(&mut bridge, "add t since=head");
    assert!(bridge.say_face_held());
    let next = shout(4);
    assert_eq!(take_say(&mut bridge), next);
    assert_eq!(lane.delivery_offsets(), [before, live, next]);
    assert!(!lane.delivery_offsets().contains(&unheard));
    assert!(bridge.conn.is_some() && bridge.link.up);
    assert!(
        bridge
            .mailbox
            .take(Duration::ZERO, bridge.fx.now())
            .is_none(),
        "the closed face's reader ended as no closure"
    );
    bridge.close_subscriptions();
}

#[test]
fn a_new_head_topic_after_a_shorter_log_hears_its_first_new_broadcast() {
    const SID: &str = "s-a";
    let (lane, mut bridge) = FakeVerbLane::bridge(&[(1, SID)]);
    bridge.refresh_sessions().unwrap();
    let (a, _broker_a, _server_a) = serve_log(&lane, "a");
    bridge.cfg.broker = a;
    let (mut writer_a, _) = bridge.connect().unwrap();
    for seq in 1..=64 {
        writer_a.publish(77, seq, "/f/f/x/history", b"v=1").unwrap();
    }
    lane.opt_in(SID, &["topic t since=head"]);
    assert!(bridge.attach_broker());
    assert_eq!(bridge.attachment, Attachment::Inherited);
    assert!(bridge.sync_say_face());
    let body = Body::new(0).encode();
    let (off, _) = writer_a
        .publish(77, 65, "/f/f/pub/n-src/s-src/say/t", &body)
        .unwrap();
    match bridge
        .mailbox
        .take(aterm_spec::wait::MUST_HAPPEN, bridge.fx.now())
    {
        Some(Item::Say(r)) => {
            assert_eq!(r.0, off);
            bridge.on_say_record(&r);
        }
        other => panic!("the old log's broadcast: {other:?}"),
    }
    lane.opt_in(SID, &[]);
    bridge.resample_topics();
    assert!(bridge.sync_say_face());
    assert!(bridge.per_sid.topics.is_empty() && !bridge.say_face_held());
    let old_drain = bridge.say_drained_to;
    bridge.close_subscriptions();
    bridge.conn = None;

    let (b, _broker_b, _server_b) = serve_log(&lane, "b");
    bridge.cfg.broker = b;
    lane.opt_in(SID, &["topic t since=head"]);
    assert!(bridge.attach_broker());
    let (mut writer_b, _) = bridge.connect().unwrap();
    let (_, (_, head)) = writer_b.fetch(0, &subject::say_filter("f"), 0).unwrap();
    assert!(head < old_drain, "the new log must be shorter");
    assert_eq!(
        bridge.per_sid.topics[SID]["t"].next, head,
        "an empty catch-up must not skip the next record at the new head"
    );
    assert!(bridge.sync_say_face());
    let before = lane.delivery_offsets().len();
    let (off, _) = writer_b
        .publish(78, 1, "/f/f/pub/n-src/s-src/say/t", &body)
        .unwrap();
    assert_eq!(off, head);
    match bridge
        .mailbox
        .take(aterm_spec::wait::MUST_HAPPEN, bridge.fx.now())
    {
        Some(Item::Say(r)) => bridge.on_say_record(&r),
        other => panic!("the new log's first broadcast: {other:?}"),
    }
    assert_eq!(&lane.delivery_offsets()[before..], &[off]);
    assert_eq!(bridge.per_sid.topics[SID]["t"].next, off + 1);
    assert_eq!(bridge.state.topics(SID), bridge.per_sid.topics[SID]);
    bridge.close_subscriptions();
}

#[cfg(unix)]
fn fan_in_next_say(bridge: &mut Bridge) -> u64 {
    match bridge
        .mailbox
        .take(aterm_spec::wait::MUST_HAPPEN, bridge.fx.now())
    {
        Some(Item::Say(r)) => {
            bridge.on_say_record(&r);
            r.0
        }
        other => panic!("a broadcast: {other:?}"),
    }
}

#[cfg(unix)]
#[test]
fn a_broadcast_cursor_waits_for_its_refill_floor_to_be_persisted() {
    const SID: &str = "s-kept";
    let (lane, mut bridge) = FakeVerbLane::bridge(&[(1, SID)]);
    let state = bridge.state.root().to_path_buf();
    let (bus, _broker, _server) = serve_log(&lane, "bus");
    bridge.cfg.broker = bus.clone();
    lane.opt_in(SID, &["topic t since=@0"]);
    assert!(bridge.attach_broker());
    assert!(bridge.sync_say_face());
    let before = bridge.per_sid.topics[SID]["t"].next;
    let (mut writer, _) = bridge.connect().unwrap();
    let (off, _) = writer
        .publish(77, 1, "/f/f/pub/n-src/s-src/say/t", &Body::new(0).encode())
        .unwrap();
    std::fs::write(state.join("unhandled"), b"owned write obstruction").unwrap();
    assert_eq!(fan_in_next_say(&mut bridge), off);
    assert_eq!(lane.delivery_offsets(), [off], "the endpoint took the row");
    assert_eq!(
        bridge.per_sid.topics[SID]["t"].next, before,
        "memory must wait"
    );
    assert_eq!(bridge.state.topics(SID)["t"].next, before, "disk must wait");
    for _ in 0..2 {
        assert!(!bridge.catch_up_topic_page(SID));
        assert_eq!(bridge.per_sid.topics[SID]["t"].next, before);
        assert_eq!(bridge.state.topics(SID)["t"].next, before);
        assert_eq!(lane.delivery_offsets(), [off], "retry dedups the ring row");
    }

    std::fs::remove_file(state.join("unhandled")).unwrap();
    assert!(bridge.catch_up_topic_page(SID));
    assert_eq!(bridge.per_sid.topics[SID]["t"].next, off + 1);
    assert_eq!(bridge.state.topics(SID)["t"].next, off + 1);
    assert_eq!(
        bridge.state.topic_floors(SID).unwrap(),
        BTreeMap::from([("t".to_string(), off)])
    );
    assert_eq!(lane.delivery_offsets(), [off]);
    bridge.close_subscriptions();
    drop(bridge);

    let (successor_lane, mut successor) =
        FakeVerbLane::bridge_with_state(&[(1, SID)], Some(&state));
    successor.cfg.broker = bus;
    successor_lane.opt_in(SID, &["topic t since=@0"]);
    assert!(successor.attach_broker());
    assert!(successor.sync_say_face());
    assert_eq!(fan_in_next_say(&mut successor), off);
    assert_eq!(
        successor_lane.delivery_offsets(),
        [off],
        "the empty successor refills"
    );
    successor.close_subscriptions();
}

#[cfg(unix)]
#[test]
fn an_uncertain_broadcast_refill_floor_refuses_attach_until_repaired() {
    const SID: &str = "s-kept";
    for damage in ["directory", "dangling", "utf8", "malformed"] {
        let (lane, mut first) = FakeVerbLane::bridge(&[(1, SID)]);
        let state = first.state.root().to_path_buf();
        let (bus, _broker, _server) = serve_log(&lane, "bus");
        first.cfg.broker = bus.clone();
        lane.opt_in(SID, &["topic t since=@0"]);
        assert!(first.attach_broker());
        assert!(first.sync_say_face());
        let (mut writer, _) = first.connect().unwrap();
        let (off, _) = writer
            .publish(77, 1, "/f/f/pub/n-src/s-src/say/t", &Body::new(0).encode())
            .unwrap();
        assert_eq!(fan_in_next_say(&mut first), off);
        first.close_subscriptions();
        drop(first);

        let floor = state.join("unhandled").join(SID);
        let saved = std::fs::read(&floor).unwrap();
        let cursors = std::fs::read(state.join("topics").join(SID)).unwrap();
        std::fs::remove_file(&floor).unwrap();
        match damage {
            "directory" => std::fs::create_dir(&floor).unwrap(),
            "dangling" => std::os::unix::fs::symlink(state.join("absent-floor"), &floor).unwrap(),
            "utf8" => std::fs::write(&floor, [0xff]).unwrap(),
            "malformed" => std::fs::write(&floor, format!("t {off}\nother invalid\n")).unwrap(),
            _ => unreachable!(),
        }
        let damaged_bytes = std::fs::read(&floor).ok();
        let (next_lane, mut next) = FakeVerbLane::bridge_with_state(&[(1, SID)], Some(&state));
        next.cfg.broker = bus;
        next_lane.opt_in(SID, &["topic t since=@0"]);
        assert!(
            !next.attach_broker(),
            "{damage} is uncertainty, not an empty floor"
        );
        assert!(next_lane.delivery_offsets().is_empty());
        assert_eq!(
            std::fs::read(state.join("topics").join(SID)).unwrap(),
            cursors
        );
        assert_eq!(std::fs::read(&floor).ok(), damaged_bytes, "retain {damage}");
        match damage {
            "directory" => std::fs::remove_dir(&floor).unwrap(),
            _ => std::fs::remove_file(&floor).unwrap(),
        }
        std::fs::write(&floor, saved).unwrap();
        assert!(
            next.attach_broker(),
            "{damage}: repaired on the same bridge"
        );
        assert!(next.sync_say_face());
        assert_eq!(fan_in_next_say(&mut next), off);
        assert_eq!(next_lane.delivery_offsets(), [off]);
        next.close_subscriptions();
    }
}

#[cfg(unix)]
#[test]
fn a_broadcast_refill_floor_without_a_cursor_is_replayed() {
    const SID: &str = "s-kept";
    let (lane, mut first) = FakeVerbLane::bridge(&[(1, SID)]);
    let state = first.state.root().to_path_buf();
    let (bus, _broker, _server) = serve_log(&lane, "bus");
    first.cfg.broker = bus.clone();
    lane.opt_in(SID, &["topic t since=@0"]);
    assert!(first.attach_broker());
    assert!(first.sync_say_face());
    let (mut writer, _) = first.connect().unwrap();
    let (off, _) = writer
        .publish(77, 1, "/f/f/pub/n-src/s-src/say/t", &Body::new(0).encode())
        .unwrap();
    assert_eq!(fan_in_next_say(&mut first), off);
    first.close_subscriptions();
    drop(first);
    std::fs::remove_file(state.join("topics").join(SID)).unwrap();
    let (next_lane, mut next) = FakeVerbLane::bridge_with_state(&[(1, SID)], Some(&state));
    next.cfg.broker = bus;
    next_lane.opt_in(SID, &["topic t since=head"]);
    assert!(next.attach_broker());
    assert_eq!(
        next.per_sid.topics[SID]["t"].next, off,
        "the valid floor is recovery data"
    );
    assert!(next.sync_say_face());
    assert_eq!(fan_in_next_say(&mut next), off);
    assert_eq!(next_lane.delivery_offsets(), [off]);
    next.close_subscriptions();
}

#[cfg(unix)]
#[test]
fn an_update_offers_the_successor_every_broadcast_its_session_had_not_handled() {
    const SID: &str = "s-kept";
    let (old_lane, mut old) = FakeVerbLane::bridge(&[(1, SID)]);
    let state = old.state.root().to_path_buf();
    let (bus, _broker, _server) = serve_log(&old_lane, "bus");
    old.cfg.broker = bus.clone();
    old_lane.opt_in(SID, &["topic t since=@0"]);
    assert!(old.attach_broker());
    assert!(old.sync_say_face());
    let (mut god, _) = old.connect().unwrap();
    let body = Body::new(0).encode();
    let mut shout = |seq: u64| {
        god.publish(77, seq, "/f/f/pub/n-src/s-src/say/t", &body)
            .unwrap()
            .0
    };
    let floors = |st: &StateDir| {
        st.topic_floors(SID)
            .unwrap()
            .into_iter()
            .collect::<Vec<_>>()
    };

    let b: Vec<u64> = (1..=4).map(&mut shout).collect();
    for want in &b {
        assert_eq!(fan_in_next_say(&mut old), *want);
    }
    assert_eq!(old_lane.delivery_offsets(), b, "ring ids 1 to 4");
    assert_eq!(floors(&old.state), [("t".to_string(), b[0])]);

    old.on_event(&format!("EVENT 1 inbox-seen 2 off={} floor=- seen=2", b[1]));
    assert_eq!(floors(&old.state), [("t".to_string(), b[2])]);
    old.on_event(&format!("EVENT 1 inbox-seen 1 off={} floor=- seen=2", b[0]));
    assert_eq!(floors(&old.state), [("t".to_string(), b[2])]);

    let overlap = shout(5);
    assert_eq!(fan_in_next_say(&mut old), overlap);
    old.fault = Fault::LoseAtermBeforeDeliver;
    let fenced = shout(6);
    assert_eq!(fan_in_next_say(&mut old), fenced);
    let mut taken = b.clone();
    taken.push(overlap);
    assert_eq!(old_lane.delivery_offsets(), taken);
    assert_eq!(old.state.topics(SID)["t"].next, fenced, "the cursor waits");
    assert_eq!(floors(&old.state), [("t".to_string(), b[2])]);
    old.close_subscriptions();
    drop(old);

    let (new_lane, mut new) = FakeVerbLane::bridge_with_state(&[(1, SID)], Some(&state));
    new.cfg.broker = bus;
    new_lane.opt_in(SID, &["topic t since=@0"]);
    assert!(new.attach_broker());
    assert!(new.sync_say_face());
    assert_eq!(new.state.topics(SID)["t"].next, b[2], "rewound, on disk");
    let want = [b[2], b[3], overlap, fenced];
    for off in want {
        assert_eq!(fan_in_next_say(&mut new), off);
    }
    assert_eq!(new_lane.delivery_offsets(), want, "in order, once each");
    assert_eq!(new.state.topics(SID)["t"].next, fenced + 1);
    assert_eq!(
        floors(&new.state),
        [("t".to_string(), b[2])],
        "re-learned from the rows it offered"
    );
    new.on_event(&format!("EVENT 1 inbox-seen 4 off={fenced} floor=- seen=4"));
    assert!(floors(&new.state).is_empty(), "handled in the successor");
    new.close_subscriptions();
}

#[cfg(unix)]
#[test]
fn a_restarted_bridge_learns_what_its_session_handled_while_it_was_away() {
    const SID: &str = "s-kept";
    for reported in [false, true] {
        let (lane, mut first) = FakeVerbLane::bridge(&[(1, SID)]);
        let state = first.state.root().to_path_buf();
        let (bus, _broker, _server) = serve_log(&lane, "bus");
        first.cfg.broker = bus.clone();
        lane.opt_in(SID, &["topic t since=@0"]);
        assert!(first.attach_broker());
        assert!(first.sync_say_face());
        let (mut god, _) = first.connect().unwrap();
        let body = Body::new(0).encode();
        let b: Vec<u64> = (1..=3)
            .map(|seq| {
                god.publish(77, seq, "/f/f/pub/n-src/s-src/say/t", &body)
                    .unwrap()
                    .0
            })
            .collect();
        for want in &b {
            assert_eq!(fan_in_next_say(&mut first), *want);
        }
        assert_eq!(first.state.topic_floors(SID).unwrap()["t"], b[0]);
        first.close_subscriptions();
        drop(first);

        let (kept, mut second) = FakeVerbLane::bridge_with_state(&[(1, SID)], Some(&state));
        *kept.delivered.lock().unwrap() = b.clone();
        kept.opt_in(SID, &["topic t since=@0"]);
        if reported {
            kept.report_handled(SID, 3);
        }
        second.cfg.broker = bus;
        assert!(second.attach_broker());
        assert!(second.sync_say_face());
        for want in &b {
            assert_eq!(fan_in_next_say(&mut second), *want);
        }
        assert_eq!(
            kept.delivery_offsets(),
            b,
            "each row once: reported={reported}"
        );
        assert_eq!(second.state.topics(SID)["t"].next, b[2] + 1);
        let floor = second.state.topic_floors(SID).unwrap().get("t").copied();
        assert_eq!(
            floor,
            (!reported).then_some(b[0]),
            "the floor goes once the watermark says the rows are handled"
        );
        second.close_subscriptions();
    }
}

#[test]
fn the_unhandled_set_is_released_by_the_watermark_and_bounded_oldest_first() {
    const SID: &str = "s-a";
    let (_lane, mut bridge) = FakeVerbLane::bridge(&[(1, SID)]);
    let on_disk = |bridge: &Bridge| bridge.state.topic_floors(SID).unwrap();
    let floors = |pairs: &[(&str, u64)]| -> BTreeMap<String, u64> {
        pairs.iter().map(|(t, o)| ((*t).to_string(), *o)).collect()
    };
    bridge.note_broadcast_taken(SID, "a", 1, 100);
    bridge.note_broadcast_taken(SID, "b", 2, 101);
    bridge.note_broadcast_taken(SID, "a", 3, 102);
    assert_eq!(on_disk(&bridge), floors(&[("a", 100), ("b", 101)]));
    bridge.note_broadcast_taken(SID, "a", 1, 100);
    bridge.note_broadcast_taken(SID, "a", 0, 99);
    assert_eq!(
        bridge.per_sid.unhandled[SID]["a"].len(),
        2,
        "covered, and `OK 0`"
    );

    bridge.note_handled(SID, 1);
    assert_eq!(on_disk(&bridge), floors(&[("a", 102), ("b", 101)]));
    bridge.note_handled(SID, 0);
    bridge.note_broadcast_taken(SID, "a", 1, 100);
    assert_eq!(
        on_disk(&bridge),
        floors(&[("a", 102), ("b", 101)]),
        "handled ids"
    );

    bridge.forget_unhandled_topic(SID, "b");
    assert_eq!(on_disk(&bridge), floors(&[("a", 102)]));

    let keep = u64::try_from(UNHANDLED_KEEP).unwrap();
    for i in 0..keep {
        bridge.note_broadcast_taken(SID, "c", 10 + i, 1000 + i);
    }
    assert_eq!(
        on_disk(&bridge),
        floors(&[("c", 1000)]),
        "a's row was the session's oldest"
    );
    bridge.note_broadcast_taken(SID, "c", 10 + keep, 1000 + keep);
    assert_eq!(on_disk(&bridge), floors(&[("c", 1001)]));
    assert_eq!(bridge.per_sid.unhandled[SID]["c"].len(), UNHANDLED_KEEP);

    bridge.note_handled(SID, 10 + keep);
    assert!(on_disk(&bridge).is_empty());
    assert!(!bridge.per_sid.unhandled.contains_key(SID));
}

fn receipt_collision_with_unclearable_pin(restart: bool) {
    use std::os::unix::fs::PermissionsExt as _;

    struct RestorePermissions(std::path::PathBuf, std::fs::Permissions);
    impl Drop for RestorePermissions {
        fn drop(&mut self) {
            let _ = std::fs::set_permissions(&self.0, self.1.clone());
        }
    }

    let (lane, mut bridge) = FakeVerbLane::bridge(&[]);
    let state = bridge.state.root().to_path_buf();
    let socket = lane.dir.path().join("broker.sock");
    let broker = crate::test_broker(lane.dir.path().join("broker.log")).unwrap();
    let _server = broker.serve(&socket).unwrap();
    bridge.cfg.broker = socket.to_string_lossy().into_owned();
    bridge.conn = Some(bridge.connect().unwrap().0.into());
    let r = OwedReceipt {
        sid: "s-host".to_string(),
        rid: 1,
        off: 7,
        verdict: "handled".to_string(),
        kind: "task".to_string(),
        from: format!("s-sender@{}", bridge.node),
        earlier_log: false,
    };
    let seq = bridge.next_seq().unwrap();
    bridge.state.set_receipt_seq(&r.sid, r.rid, seq).unwrap();
    let acks = state.join("acks");
    let _restore = RestorePermissions(
        acks.clone(),
        std::fs::metadata(&acks).unwrap().permissions(),
    );
    std::fs::set_permissions(&acks, std::fs::Permissions::from_mode(0o500)).unwrap();
    let (mut other, _) = bridge.connect().unwrap();
    let (off, deduped) = other
        .publish(bridge.producer_id, seq, "/f/f/x/decoy", b"another writer")
        .unwrap();
    assert!(!deduped);
    assert_eq!(
        bridge.send_receipt(&r).unwrap_err().kind(),
        io::ErrorKind::AlreadyExists
    );
    assert_eq!(
        bridge.state.receipt_seq(&r.sid, r.rid),
        Some(seq),
        "the read-only directory prevented invalidation"
    );

    let reopened_lane;
    let endpoint = if restart {
        drop(bridge);
        (reopened_lane, bridge) = FakeVerbLane::bridge_with_state(&[], Some(&state));
        bridge.cfg.broker = socket.to_string_lossy().into_owned();
        &reopened_lane
    } else {
        &lane
    };
    bridge.conn = Some(bridge.connect().unwrap().0.into());
    let retry = bridge.send_receipt(&r);
    assert!(
        retry.is_err() && !bridge.self_acked.contains(&off) && endpoint.delivery_offsets().is_empty(),
        "a collided pin must stay owed, restart={restart}: retry={retry:?}, foreign_offset_remembered={}, retired={:?}",
        bridge.self_acked.contains(&off), endpoint.delivery_offsets()
    );
}

#[test]
fn a_receipt_collision_cannot_reuse_a_pin_that_failed_to_clear() {
    for restart in [false, true] {
        receipt_collision_with_unclearable_pin(restart);
    }
}

#[test]
fn a_pinned_lost_ack_retries_its_original_face_after_restart() {
    let (lane, mut bridge) = FakeVerbLane::bridge(&[]);
    let state = bridge.state.root().to_path_buf();
    let socket = lane.dir.path().join("broker.sock");
    let broker = crate::test_broker(lane.dir.path().join("broker.log")).unwrap();
    let _server = broker.serve(&socket).unwrap();
    bridge.cfg.broker = socket.to_string_lossy().into_owned();
    bridge.conn = Some(bridge.connect().unwrap().0.into());
    let original = format!("/f/f/in/p/h-a/{}/note", bridge.node);
    let moved = format!("/f/f/in/p/h-b/{}/note", bridge.node);
    let mut body = Body::new(1);
    body.from = Some("s-host".into());
    body.text = "lost ack".into();
    let pin = PublishPin {
        seq: bridge.next_seq().unwrap(),
        via_key: false,
        identity: Some(publication_identity(&original, &body.encode())),
        original: None,
        original_off: None,
        owner: None,
        at: None,
    };
    bridge.state.set_post_pin("s-host", 1, &pin).unwrap();
    let (off, deduped, _) = bridge
        .publish_reserved(&pin, &original, &body.encode())
        .unwrap();
    assert!(!deduped);
    drop(bridge);
    let (_second, mut bridge) = FakeVerbLane::bridge_with_state(&[], Some(&state));
    bridge.cfg.broker = socket.to_string_lossy().into_owned();
    bridge.conn = Some(bridge.connect().unwrap().0.into());
    body.t = 99;
    let pin = bridge.state.post_pin("s-host", 1).unwrap().unwrap();
    let (again, deduped, actual) = bridge
        .publish_reserved(&pin, &moved, &body.encode())
        .unwrap();
    assert_eq!(again, off);
    assert!(deduped);
    assert_eq!(actual, pin.identity);
    assert!(
        bridge
            .conn_now()
            .unwrap()
            .fetch(0, &moved, 8)
            .unwrap()
            .0
            .is_empty(),
        "route movement does not append elsewhere"
    );
    body.text = "incompatible replay".into();
    let refused = bridge.publish_reserved(&pin, &moved, &body.encode());
    assert!(
        refused.is_err_and(|e| e.kind() == io::ErrorKind::InvalidData) && bridge.collided.is_none(),
        "a payload the pin cannot reconstruct stays queued, not a collision"
    );
}

#[test]
fn a_pinned_dedup_after_restart_requires_its_payload_and_legacy_ambiguity_stays_queued() {
    for modern in [false, true] {
        let (lane, mut bridge) = FakeVerbLane::bridge(&[]);
        let state = bridge.state.root().to_path_buf();
        let socket = lane.dir.path().join("broker.sock");
        let broker = crate::test_broker(lane.dir.path().join("broker.log")).unwrap();
        let _server = broker.serve(&socket).unwrap();
        bridge.cfg.broker = socket.to_string_lossy().into_owned();
        bridge.conn = Some(bridge.connect().unwrap().0.into());
        let face = format!("/f/f/in/p/h-a/{}/note", bridge.node);
        let mut intended = Body::new(1);
        intended.from = Some("s-host".into());
        intended.text = "the requested payload".into();
        let pin = PublishPin {
            seq: bridge.next_seq().unwrap(),
            via_key: !modern,
            identity: modern.then(|| publication_identity(&face, &intended.encode())),
            original: None,
            original_off: None,
            owner: None,
            at: None,
        };
        bridge.state.set_post_pin("s-host", 1, &pin).unwrap();
        let mut other_body = intended.clone();
        other_body.text = "a different original payload".into();
        let (mut other, _) = bridge.connect().unwrap();
        let (off, _) = other
            .publish(bridge.producer_id, pin.seq, &face, &other_body.encode())
            .unwrap();
        drop(bridge);
        let (_second, mut bridge) = FakeVerbLane::bridge_with_state(&[], Some(&state));
        bridge.cfg.broker = socket.to_string_lossy().into_owned();
        bridge.conn = Some(bridge.connect().unwrap().0.into());
        let pin = bridge.state.post_pin("s-host", 1).unwrap().unwrap();
        let error = bridge
            .publish_reserved(&pin, &face, &intended.encode())
            .unwrap_err();
        assert!(!bridge.self_acked.contains(&off));
        if modern {
            assert_eq!(error.kind(), io::ErrorKind::AlreadyExists);
            assert_eq!(bridge.collided, Some((pin.seq, off)));
        } else {
            assert_eq!(error.kind(), io::ErrorKind::InvalidData);
            assert!(
                bridge.collided.is_none(),
                "an older keyed body is not proof of a collision"
            );
            assert!(
                bridge.state.post_pin("s-host", 1).unwrap().is_some(),
                "an unverifiable reservation is not reseeded"
            );
        }
    }
}

#[test]
fn a_legacy_keys_fresh_append_recovers_ownership_after_reconciliation_and_restart() {
    let (lane, mut bridge) = FakeVerbLane::bridge(&[(1, "s-host")]);
    let state = bridge.state.root().to_path_buf();
    let socket = lane.dir.path().join("broker.sock");
    let broker = crate::test_broker(lane.dir.path().join("broker.log")).unwrap();
    let _server = broker.serve(&socket).unwrap();
    bridge.cfg.broker = socket.to_string_lossy().into_owned();
    bridge.conn = Some(bridge.connect().unwrap().0.into());
    let face = format!("/f/f/in/p/h-a/{}/ask", bridge.node);
    let mut body = Body::new(1);
    body.from = Some("s-host".into());
    body.dl = Some(30_000);
    body.text = "new legacy attempt".into();
    let seq = bridge.next_seq().unwrap();
    bridge.state.set_key_seq("s-host", "k-legacy", seq).unwrap();
    let pin = PublishPin {
        seq,
        via_key: true,
        identity: Some(publication_identity(&face, &body.encode())),
        original: None,
        original_off: None,
        owner: None,
        at: None,
    };
    bridge.state.set_post_pin("s-host", 1, &pin).unwrap();
    let (off, deduped, actual) = bridge
        .publish_reserved(&pin, &face, &body.encode())
        .unwrap();
    assert!(!deduped, "the legacy winner was already retained away");
    bridge
        .state
        .reconcile_key(
            "s-host",
            "k-legacy",
            seq,
            actual.as_ref().unwrap(),
            off,
            Some(1),
        )
        .unwrap();
    drop(bridge);
    let (second, mut bridge) = FakeVerbLane::bridge_with_state(&[(1, "s-host")], Some(&state));
    bridge.cfg.broker = socket.to_string_lossy().into_owned();
    bridge.conn = Some(bridge.connect().unwrap().0.into());
    *second.outbox.lock().unwrap() = Some(format!(
        "post sid=s-host id=1 to=h-a kind=ask key=k-legacy dl=30000 len={}\n{}",
        body.text.len(),
        body.text
    ));
    bridge.drain_outbox();
    assert_eq!(second.sent.lock().unwrap().len(), 1);
    assert!(!second.sent.lock().unwrap()[0].contains("dup=1"));
    assert_eq!(bridge.asked.get(&off).map(String::as_str), Some("h-a"));
    assert!(bridge.deadlines.contains_key(&off));
    assert_eq!(
        bridge
            .conn_now()
            .unwrap()
            .fetch(0, &face, 8)
            .unwrap()
            .0
            .len(),
        1
    );
}

/// The live tail's next record at `off`, offered to the bridge: `false` when it ran out
/// first.
#[cfg(unix)]
fn offer_live_record(bridge: &mut Bridge, off: u64) -> bool {
    loop {
        match bridge
            .mailbox
            .take(aterm_spec::wait::MUST_HAPPEN, bridge.fx.now())
        {
            Some(Item::Inbox(rec, origin)) => {
                let at = rec.0;
                bridge.on_inbox_record(&rec, origin);
                if at == off {
                    return true;
                }
            }
            Some(_) => {}
            None => return false,
        }
    }
}

/// The offsets `lane` delivered to `s-b` a note of `text` at.
#[cfg(unix)]
fn notes_delivered(lane: &FakeVerbLane, text: &str) -> BTreeSet<String> {
    let tail = format!("text={text}");
    lane.deliveries()
        .iter()
        .filter(|l| l.starts_with("deliver s-b ") && l.ends_with(&tail))
        .filter_map(|l| l.split_whitespace().find(|w| w.starts_with("off=")))
        .map(str::to_string)
        .collect()
}

/// **A LANDED POST ATERM KEEPS QUEUED IS NOT DELIVERED BEFORE ITS RETRY** (E07 review), on
/// both paths that keep it queued at its pin. `busy`: aterm answers its `outbox sent` with
/// `ERR busy` (aterm-gui-fabric's `retire_post` answers `ERR busy update-committing`, the
/// row untouched, while a seamless update's commit fences retirements). `held`: the post is
/// an ask whose deadline the disk refuses, so the bridge holds it for its retry and sends
/// no `outbox sent`. Its in-flight entry stands and the live tail holds its record back.
/// The log moves before the drain's retry, which finds no copy on the new log and
/// publishes there: s-b is handed that copy alone. NEGATIVE CONTROL: the entry ended at any
/// answer and at a hold, as E07's first cut ended it, lets the tail hand s-b the first copy
/// too.
#[cfg(unix)]
#[test]
fn a_post_aterm_keeps_queued_is_held_until_its_retry_so_a_move_delivers_it_once() {
    for (arm, control) in [
        ("busy", false),
        ("busy", true),
        ("held", false),
        ("held", true),
    ] {
        let case = format!("{arm} control={control}");
        let (lane, mut bridge) = FakeVerbLane::bridge(&[(1, "s-a"), (2, "s-b")]);
        if control {
            bridge.fx =
                crate::fx::Fx::os_without(crate::sim::world::Mutant::LandedWhileThePostStaysQueued);
        }
        let (a, _broker_a, _server_a) = serve_log(&lane, &format!("queued-{arm}-a"));
        bridge.cfg.broker = a;
        assert!(bridge.attach_broker(), "{case}: the attach on A");
        let post = if arm == "busy" {
            lane.refuse_retirements(true);
            "post sid=s-a id=1 to=@s-b kind=note len=4\nonce"
        } else {
            bridge.state.refuse_writes_of(Some("deadlines"));
            "post sid=s-a id=1 to=@s-b kind=ask dl=60000 len=4\nonce"
        };
        *lane.outbox.lock().unwrap() = Some(post.to_string());
        bridge.drain_outbox();
        bridge.state.refuse_writes_of(None);
        let pin = bridge
            .state
            .post_pin("s-a", 1)
            .unwrap()
            .expect("the post's pin stays while aterm keeps it queued");
        let face = pin.identity.as_ref().unwrap().subject.clone();
        let (rows, _) = bridge.conn_now().unwrap().fetch(0, &face, 1).unwrap();
        let on_a = rows.first().expect("the note on A").0;
        assert_eq!(
            bridge.state.in_flight().unwrap().len(),
            usize::from(!control),
            "{case}: the entry stands while aterm keeps the post queued"
        );
        assert!(
            offer_live_record(&mut bridge, on_a),
            "{case}: the live tail reached the note"
        );
        assert_eq!(
            lane.delivery_offsets().contains(&on_a),
            control,
            "{case}: the note on A handed to s-b while its post stood queued"
        );
        bridge.close_subscriptions();
        bridge.conn = None;

        let (b, _broker_b, _server_b) = serve_log(&lane, &format!("queued-{arm}-b"));
        bridge.cfg.broker = b;
        let (mut padding, _) = bridge.connect().unwrap();
        for seq in 1..=32 {
            padding.publish(92, seq, "/f/f/x/padding", b"v=1").unwrap();
        }
        drop(padding);
        lane.refuse_retirements(false);
        lane.retire_from_outbox();
        assert!(bridge.attach_broker(), "{case}: the attach on B");
        bridge.drain_outbox();
        let (mut writer, _) = bridge.connect().unwrap();
        let sentinel_face = format!(
            "/f/{}/in/{}/s-b/h-writer/note",
            bridge.cfg.fleet, bridge.node
        );
        let (sentinel, _) = writer
            .publish(81, 1, &sentinel_face, b"v=1 t=1 text=sentinel")
            .unwrap();
        assert!(
            offer_live_record(&mut bridge, sentinel),
            "{case}: the live tail on B reached the sentinel"
        );
        assert!(
            bridge.state.in_flight().unwrap().is_empty(),
            "{case}: the entry settled on B"
        );
        let (notes, _) = bridge.conn_now().unwrap().fetch(0, &face, 8).unwrap();
        let on_b: Vec<u64> = notes.iter().map(|(off, _, _)| *off).collect();
        assert_eq!(on_b.len(), 1, "{case}: the note on B once: {on_b:?}");
        let want: BTreeSet<String> = if control {
            [format!("off={on_a}"), format!("off={}", on_b[0])].into()
        } else {
            [format!("off={}", on_b[0])].into()
        };
        assert_eq!(
            notes_delivered(&lane, "once"),
            want,
            "{case}: once from B, or from A and again from B (the control)"
        );
        bridge.close_subscriptions();
    }
}

/// The offsets `lane` handed `sid` a receipt's answer at (`kind=ack`).
#[cfg(unix)]
fn answers_delivered(lane: &FakeVerbLane, sid: &str) -> BTreeSet<String> {
    let head = format!("deliver {sid} ");
    lane.deliveries()
        .iter()
        .filter(|l| l.starts_with(&head) && l.split_whitespace().any(|w| w == "kind=ack"))
        .filter_map(|l| l.split_whitespace().find(|w| w.starts_with("off=")))
        .map(str::to_string)
        .collect()
}

/// **A LANDED RECEIPT ATERM STILL OWES IS NOT DELIVERED BEFORE ITS RETRY** (critic item F15):
/// aterm answers the receipt's `deliver <sid> receipt=<rid>` with `ERR busy
/// update-committing` (aterm-gui-fabric's `deliver_receipt` while a seamless update's commit
/// fences retirements), so the receipt stays owed at its pin. Its in-flight entry stands and
/// the live tail holds its record back. The next attach's replay reads the record back where
/// it landed, its drain's retry finds it there and retires the receipt, and aterm's `OK`
/// ends the entry: the asker is handed the answer once, and the bus holds it once. Its offset
/// is remembered as this node's own from the first attempt on (`Landing::remembers` keeps an
/// own receipt's, the reverse of
/// `a_receipt_onto_a_persons_lane_lands_without_an_acked_write`). NEGATIVE
/// CONTROL: the entry ended at the ack, as every build before F15 ended it, lets the tail
/// hand the answer over while aterm still owes the receipt, which a retry past a cut that
/// gave the record up lands a second time (the sim's seed 478,
/// `sim::tests::a_landed_receipts_entry_stands_until_aterm_retired_it`). The sim's endpoint
/// never answers busy, so only this test covers that path.
#[cfg(unix)]
#[test]
fn a_receipt_aterm_still_owes_is_held_until_its_retry_and_answered_once() {
    for control in [false, true] {
        let case = format!("control={control}");
        let (lane, mut bridge) = FakeVerbLane::bridge(&[(1, "s-a"), (2, "s-b")]);
        if control {
            bridge.fx = crate::fx::Fx::os_without(crate::sim::world::Mutant::ReceiptRetiredAtAck);
        }
        let (a, _broker, _server) = serve_log(&lane, &format!("owed-receipt-{control}"));
        bridge.cfg.broker = a;
        assert!(bridge.attach_broker(), "{case}: the first attach");
        lane.refuse_retirements(true);
        *lane.outbox.lock().unwrap() =
            Some("receipt sid=s-b rid=1 off=3 verdict=handled kind=ask from=s-a\n".to_string());
        bridge.drain_outbox();
        let pin = bridge
            .state
            .receipt_pin("s-b", 1)
            .unwrap()
            .expect("the receipt's pin stays while aterm still owes it");
        let face = pin.identity.as_ref().unwrap().subject.clone();
        let (rows, _) = bridge.conn_now().unwrap().fetch(0, &face, 1).unwrap();
        let on_bus = rows.first().expect("the answer on the bus").0;
        assert_eq!(
            bridge.state.in_flight().unwrap().len(),
            usize::from(!control),
            "{case}: the entry stands while aterm still owes the receipt"
        );
        // Its offset is this node's own ahead of its retirement, in memory and on disk
        // (`Bridge::land_group`'s first step, `Landing::remembers`): an own receipt that
        // step skipped would leave the next bridge, once its entry ended, an own record
        // neither in flight nor known as its own, which it refuses as forged.
        assert!(
            bridge.self_acked.contains(&on_bus)
                && bridge.state.self_acked_log().offsets.contains(&on_bus),
            "{case}: the own receipt's offset @{on_bus} is remembered as this node's own"
        );
        assert!(
            offer_live_record(&mut bridge, on_bus),
            "{case}: the live tail reached the answer"
        );
        assert_eq!(
            answers_delivered(&lane, "s-a").contains(&format!("off={on_bus}")),
            control,
            "{case}: the answer handed to s-a while aterm still owed the receipt"
        );
        bridge.close_subscriptions();
        bridge.conn = None;

        lane.refuse_retirements(false);
        lane.retire_from_outbox();
        assert!(
            bridge.attach_broker(),
            "{case}: the attach after the commit"
        );
        bridge.drain_outbox();
        let (mut writer, _) = bridge.connect().unwrap();
        let sentinel_face = format!(
            "/f/{}/in/{}/s-a/h-writer/note",
            bridge.cfg.fleet, bridge.node
        );
        let (sentinel, _) = writer
            .publish(81, 1, &sentinel_face, b"v=1 t=1 text=sentinel")
            .unwrap();
        assert!(
            offer_live_record(&mut bridge, sentinel),
            "{case}: the live tail reached the sentinel"
        );
        assert!(
            bridge.state.in_flight().unwrap().is_empty(),
            "{case}: aterm's OK ended the entry"
        );
        assert!(
            bridge.state.receipt_pin("s-b", 1).unwrap().is_none(),
            "{case}: the reservation went with the receipt"
        );
        let (answers, _) = bridge.conn_now().unwrap().fetch(0, &face, 8).unwrap();
        assert_eq!(
            answers.iter().map(|(off, _, _)| *off).collect::<Vec<_>>(),
            vec![on_bus],
            "{case}: the answer on the bus once"
        );
        assert_eq!(
            answers_delivered(&lane, "s-a"),
            [format!("off={on_bus}")].into(),
            "{case}: s-a handed the answer at its one offset: {:?}",
            lane.deliveries()
        );
        bridge.close_subscriptions();
    }
}

/// **A SUCCESSOR ATTACHES ONCE AFTER ATERM WENT AWAY DURING A LANDED POST'S RETIREMENT** (E07
/// review): the first bridge's `outbox sent` meets aterm gone, so the post's in-flight entry
/// stands. The successor's replay holds the record, and its drain's retry reads the record
/// back at the post's pin and retires it: aterm's `OK` there ends the entry, so the attach's
/// refill delivers the record and the attach goes on. A read back that ended no entry left
/// the refill to meet the record held, drop the link and say the state dir refused a write;
/// only a second attach delivered it.
#[cfg(unix)]
#[test]
fn a_successor_attaches_once_after_aterm_went_away_during_a_landed_posts_retirement() {
    let sessions = [(1, "s-a"), (2, "s-b")];
    let (lane, mut first) = FakeVerbLane::bridge(&sessions);
    let state = first.state.root().to_path_buf();
    let (a, _broker_a, _server_a) = serve_log(&lane, "gone-a");
    first.cfg.broker = a.clone();
    assert!(first.attach_broker(), "the first bridge's attach");
    let post = "post sid=s-a id=1 to=@s-b kind=note len=4\nonce".to_string();
    *lane.outbox.lock().unwrap() = Some(post.clone());
    lane.hang_up_at("outbox sent ");
    first.drain_outbox();
    assert!(first.aterm_gone(), "the retirement met aterm gone");
    assert_eq!(
        first.state.in_flight().unwrap().len(),
        1,
        "the landed post's entry stands"
    );
    first.close_subscriptions();
    drop(first);

    let (second, mut bridge) = FakeVerbLane::bridge_with_state(&sessions, Some(&state));
    bridge.cfg.broker = a;
    second.retire_from_outbox();
    *second.outbox.lock().unwrap() = Some(post);
    let links_before = second.links.lock().unwrap().len();
    assert!(
        bridge.attach_broker(),
        "the successor's first attach goes on; links told: {:?}",
        second.links.lock().unwrap()[links_before..].to_vec()
    );
    assert!(
        bridge.state.in_flight().unwrap().is_empty(),
        "aterm's OK to the read back's `outbox sent` ended the entry"
    );
    assert_eq!(
        notes_delivered(&second, "once").len(),
        1,
        "the successor's attach delivered the note once: {:?}",
        second.deliveries()
    );
    bridge.close_subscriptions();
}

/// **A SUCCESSOR ATTACHES ONCE AFTER ATERM WENT AWAY DURING A LANDED RECEIPT'S RETIREMENT**
/// (critic item F15): the first bridge's `deliver <sid> receipt=<rid>` meets aterm gone, so
/// the receipt's in-flight entry stands, as a post's does. The successor's replay holds the
/// record, and its drain's retry finds it at the receipt's pin and retires the receipt:
/// aterm's `OK` there ends the entry, so the attach's refill hands the asker the answer once
/// and the attach goes on. A read back that ended no entry would leave the refill to meet
/// the record held and drop the link.
#[cfg(unix)]
#[test]
fn a_successor_attaches_once_after_aterm_went_away_during_a_landed_receipts_retirement() {
    let sessions = [(1, "s-a"), (2, "s-b")];
    let (lane, mut first) = FakeVerbLane::bridge(&sessions);
    let state = first.state.root().to_path_buf();
    let (a, _broker_a, _server_a) = serve_log(&lane, "gone-receipt");
    first.cfg.broker = a.clone();
    assert!(first.attach_broker(), "the first bridge's attach");
    let receipt = "receipt sid=s-b rid=1 off=3 verdict=handled kind=ask from=s-a\n".to_string();
    *lane.outbox.lock().unwrap() = Some(receipt.clone());
    lane.hang_up_at("deliver s-b receipt=");
    first.drain_outbox();
    assert!(first.aterm_gone(), "the retirement met aterm gone");
    assert_eq!(
        first.state.in_flight().unwrap().len(),
        1,
        "the landed receipt's entry stands"
    );
    first.close_subscriptions();
    drop(first);

    let (second, mut bridge) = FakeVerbLane::bridge_with_state(&sessions, Some(&state));
    bridge.cfg.broker = a;
    second.retire_from_outbox();
    *second.outbox.lock().unwrap() = Some(receipt);
    let links_before = second.links.lock().unwrap().len();
    assert!(
        bridge.attach_broker(),
        "the successor's first attach goes on; links told: {:?}",
        second.links.lock().unwrap()[links_before..].to_vec()
    );
    assert!(
        bridge.state.in_flight().unwrap().is_empty(),
        "aterm's OK to the read back's retirement ended the entry"
    );
    assert!(
        second
            .requests
            .lock()
            .unwrap()
            .iter()
            .any(|l| l.starts_with("deliver s-b receipt=1 off=") && !l.ends_with("off=-")),
        "the successor retired the receipt at its offset"
    );
    assert_eq!(
        answers_delivered(&second, "s-a").len(),
        1,
        "the successor's attach handed the answer over once: {:?}",
        second.deliveries()
    );
    bridge.close_subscriptions();
}

/// **A RECEIPT ONTO A PERSON'S LANE IS NOT REMEMBERED AS THIS NODE'S OWN** (critic item
/// F15, `Landing::remembers`): it lands through a post's steps, and its offset stays out of
/// the self-lane log, as every build before F15 kept it out: it journals no entry and is no
/// record of this node's lanes, so an `acked` write for it would be one more append that
/// guards nothing.
#[cfg(unix)]
#[test]
fn a_receipt_onto_a_persons_lane_lands_without_an_acked_write() {
    let (lane, mut bridge) = FakeVerbLane::bridge(&[(1, "s-a")]);
    let (a, _broker, _server) = serve_log(&lane, "person-receipt");
    bridge.cfg.broker = a;
    assert!(bridge.attach_broker(), "the attach");
    lane.retire_from_outbox();
    *lane.outbox.lock().unwrap() =
        Some("receipt sid=s-a rid=1 off=3 verdict=handled kind=task from=h-x\n".to_string());
    let acked_before = bridge.self_acked.clone();
    bridge.drain_outbox();
    let face = format!("{}/ack", subject::person_lane("f", "h-x", &bridge.node));
    let (rows, _) = bridge.conn_now().unwrap().fetch(0, &face, 8).unwrap();
    let [(off, _, _)] = rows.as_slice() else {
        panic!("the answer on the person's lane once: {rows:?}");
    };
    assert!(
        lane.requests
            .lock()
            .unwrap()
            .contains(&format!("deliver s-a receipt=1 off={off}")),
        "aterm told the receipt's offset"
    );
    assert!(
        bridge.state.receipt_pin("s-a", 1).unwrap().is_none(),
        "the reservation went with the receipt"
    );
    assert!(
        bridge.state.in_flight().unwrap().is_empty(),
        "no entry for a record off this node's lanes"
    );
    assert_eq!(
        bridge.self_acked, acked_before,
        "the receipt's offset @{off} is not this node's own"
    );
    bridge.close_subscriptions();
}

/// **A POST ONTO A PERSON'S LANE OR ANOTHER NODE'S TAKES NO SLOT OF THE SELF-LANE WINDOW**
/// (critic item F17, `Landing::remembers`): a co-holder of the node cap writes a record onto
/// this node's own lane under its `<src>`, and the node's window, full with its newest
/// [`SELF_ACK_KEEP`] own offsets, the oldest just below the forged record, accuses it. A post
/// to a person then lands above it, and one to another node's session settles beside it:
/// neither offset goes into the self-lane log, in memory or on disk, so the window and its
/// floor stay as they were and the forged record offered after them is refused as
/// `forged-self`. A post to a session of this node is remembered, as every own record is.
/// NEGATIVE CONTROL (`Mutant::WideAckedSet`, every landed post's offset written, as every
/// build before F17 wrote it): each post takes a slot, so the full window drops its oldest
/// own offset for the person's post, its floor rises past the forged record, and the record
/// is delivered as this node's own.
#[cfg(unix)]
#[test]
fn a_post_onto_a_persons_lane_or_another_nodes_takes_no_slot_of_the_self_lane_window() {
    for wide in [false, true] {
        let case = format!("wide={wide}");
        let (lane, mut bridge) = FakeVerbLane::bridge(&[(1, "s-a"), (2, "s-r")]);
        if wide {
            bridge.fx = Fx::os_without(crate::sim::world::Mutant::WideAckedSet);
        }
        assert!(bridge.refresh_sessions().is_ok());
        let (a, _broker, _server) = serve_log(&lane, &format!("wide-acked-{wide}"));
        bridge.cfg.broker = a;
        assert!(bridge.attach_broker(), "{case}: the attach");
        let own_lane = subject::post_lane("f", &bridge.node, "s-r", &bridge.node, "note");
        let forged_body = b"v=1 t=1 text=forged".to_vec();
        let (mut forger, _) = bridge.connect().unwrap();
        let (forged, _) = forger
            .publish(bridge.producer_id ^ 1, 1, &own_lane, &forged_body)
            .unwrap();
        assert!(
            forged > 0 && bridge.self_acked.is_empty() && bridge.self_floor == Some(0),
            "{case}: PRECONDITION: a fresh window below @{forged}: floor {:?}, {:?}",
            bridge.self_floor,
            bridge.self_acked
        );
        // The node's newest own records: the oldest just below the forged one, the rest
        // above every offset this test reaches.
        let far = 1u64 << 40;
        let keep = SELF_ACK_KEEP as u64;
        assert!(
            bridge
                .remember_self_acks(std::iter::once(forged - 1).chain(far..far + keep - 1))
                .is_ok(),
            "{case}: the window filled"
        );
        let full = bridge.self_acked.clone();
        assert!(
            full.len() == SELF_ACK_KEEP && bridge.window_accuses(forged),
            "{case}: PRECONDITION: the full window accuses @{forged}"
        );

        *lane.outbox.lock().unwrap() = Some(queued_note("s-a", 1, "h-x", "to a person"));
        bridge.drain_outbox();
        bridge.settle_acks();
        let person = format!("{}/>", subject::person_lane("f", "h-x", &bridge.node));
        let (rows, _) = forger.fetch(0, &person, 8).unwrap();
        let [(to_person, _, _)] = rows.as_slice() else {
            panic!("{case}: the note on the person's lane once: {rows:?}");
        };
        assert!(
            *to_person > forged
                && retirements(&lane).contains(&("s-a".to_string(), 1, Some(*to_person))),
            "{case}: the person's note landed above @{forged} and was retired: {:?}",
            retirements(&lane)
        );
        let other_lane = subject::post_lane("f", "n-9ee7000000000002", "s-x", &bridge.node, "note");
        let post = parse_drain(b"post sid=s-a id=2 to=@s-x kind=note len=5\nthere")
            .posts
            .remove(0);
        let identity = publication_identity(&other_lane, b"there");
        let pin = PublishPin {
            seq: bridge.next_seq().unwrap(),
            via_key: false,
            identity: Some(identity.clone()),
            original: None,
            original_off: None,
            owner: Some(2),
            at: None,
        };
        let to_other = to_person + 1;
        assert_eq!(
            bridge.land_alone(Landing {
                of: Outboxed::Post(post),
                subject: other_lane,
                pin,
                off: to_other,
                deduped: false,
                actual: Some(identity),
                entry: None,
            }),
            PostNext::Moves,
            "{case}: the other node's post settled"
        );
        let (held, floor) = (bridge.self_acked.clone(), bridge.self_floor);
        let on_disk = bridge.state.self_acked_log();
        if wide {
            assert!(
                held.contains(&to_other)
                    && !held.contains(&(forged - 1))
                    && floor.is_some_and(|floor| floor > forged),
                "{case}: the control writes the posts' offsets, each dropping the window's \
                 oldest, and the floor rises past @{forged} to {floor:?}"
            );
        } else {
            assert!(
                held == full
                    && floor == Some(0)
                    && on_disk.offsets == full
                    && on_disk.floor == Some(0),
                "{case}: no post off this node's lanes is its own: floor {floor:?}, disk's {:?}",
                on_disk.floor
            );
        }

        bridge.on_inbox_record(&(forged, own_lane.clone(), forged_body.clone()), None);
        bridge.settle_acks();
        let (accused, delivered) = forged_verdict(&lane, &mut forger, forged);
        assert_eq!(
            (accused, delivered),
            (!wide, wide),
            "{case}: the forged record @{forged} below the posts: accused {accused}, \
             delivered {delivered}; floor {:?}",
            bridge.self_floor
        );

        *lane.outbox.lock().unwrap() = Some(queued_note("s-a", 3, "@s-r", "to a session"));
        bridge.drain_outbox();
        bridge.settle_acks();
        let (rows, _) = forger.fetch(forged + 1, &own_lane, 8).unwrap();
        let [(own, _, _)] = rows.as_slice() else {
            panic!("{case}: the note on s-r's lane once: {rows:?}");
        };
        assert!(
            bridge.self_acked.contains(own) && bridge.state.self_acked().contains(own),
            "{case}: the own post's offset @{own} is remembered, in memory and on disk"
        );
        bridge.close_subscriptions();
    }
}

/// Whether the forged record at `forged` was accused (`undeliverable … reason=forged-self` on
/// the node's face) and whether it was delivered.
#[cfg(unix)]
fn forged_verdict(lane: &FakeVerbLane, reader: &mut Conn, forged: u64) -> (bool, bool) {
    let delivered = lane.transcript().contains(&format!("deliver {forged}"));
    let (rows, _) = reader.fetch(0, "/f/f/pub/>", 4096).unwrap();
    let accused = rows.iter().any(|(_, _, raw)| {
        let ev = crate::pct::decode(&String::from_utf8_lossy(raw));
        ev.contains(&format!("undeliverable off={forged} reason=forged-self"))
    });
    (accused, delivered)
}

/// **A FRESH WINDOW'S FIRST OWN RECORD MOVES NO FLOOR** (critic item F17's review,
/// `self_window`): a node whose window is fresh (floored at its attach at the empty log's
/// head, 0, so it accuses every offset)
/// posts to a person, landed; a co-holder of the node cap then writes a record onto this
/// node's own lane under its `<src>`; and the node's first own post lands above it. The
/// window holds the own post's offset alone, and its floor, kept apart from what it holds,
/// stays 0, in memory and in the `acked` file, which names it, so the window a relaunch
/// reads back has it too: the forged record offered after them is refused as `forged-self`.
/// Writing every landed post's offset (`Mutant::WideAckedSet`, as every build before F17
/// wrote it) puts the person's post in the window too, and the record is refused all the
/// same: the own-only window accuses what the wide one did. NEGATIVE CONTROL
/// (`Mutant::FloorOffTheOldest`, the floor read off the window's oldest offset, as every
/// build before the review read it): the own post's offset is the floor, above the forged
/// record, which is delivered as this node's own.
#[cfg(unix)]
#[test]
fn a_fresh_windows_first_own_record_moves_no_floor() {
    use crate::sim::world::Mutant;
    for (i, mutant) in [
        None,
        Some(Mutant::WideAckedSet),
        Some(Mutant::FloorOffTheOldest),
    ]
    .into_iter()
    .enumerate()
    {
        let case = format!("{mutant:?}");
        let (lane, mut bridge) = FakeVerbLane::bridge(&[(1, "s-a"), (2, "s-r")]);
        if let Some(mutant) = mutant {
            bridge.fx = Fx::os_without(mutant);
        }
        assert!(bridge.refresh_sessions().is_ok());
        let (a, _broker, _server) = serve_log(&lane, &format!("fresh-floor-{i}"));
        bridge.cfg.broker = a;
        assert!(bridge.attach_broker(), "{case}: the attach");
        assert!(
            bridge.self_acked.is_empty() && bridge.self_floor == Some(0),
            "{case}: PRECONDITION: a fresh window: floor {:?}, {:?}",
            bridge.self_floor,
            bridge.self_acked
        );
        let (mut forger, _) = bridge.connect().unwrap();

        *lane.outbox.lock().unwrap() = Some(queued_note("s-a", 1, "h-x", "to a person"));
        bridge.drain_outbox();
        bridge.settle_acks();
        let person = format!("{}/>", subject::person_lane("f", "h-x", &bridge.node));
        let (rows, _) = forger.fetch(0, &person, 8).unwrap();
        let [(to_person, _, _)] = rows.as_slice() else {
            panic!("{case}: the note on the person's lane once: {rows:?}");
        };
        let own_lane = subject::post_lane("f", &bridge.node, "s-r", &bridge.node, "note");
        let forged_body = b"v=1 t=1 text=forged".to_vec();
        let (forged, _) = forger
            .publish(bridge.producer_id ^ 1, 1, &own_lane, &forged_body)
            .unwrap();
        *lane.outbox.lock().unwrap() = Some(queued_note("s-a", 2, "@s-r", "to a session"));
        bridge.drain_outbox();
        bridge.settle_acks();
        let (rows, _) = forger.fetch(forged + 1, &own_lane, 8).unwrap();
        let [(own, _, _)] = rows.as_slice() else {
            panic!("{case}: the note on s-r's lane once: {rows:?}");
        };
        assert!(
            *to_person < forged && forged < *own,
            "{case}: PRECONDITION: @{to_person} < @{forged} < @{own}"
        );
        let want = if mutant == Some(Mutant::WideAckedSet) {
            BTreeSet::from([*to_person, *own])
        } else {
            BTreeSet::from([*own])
        };
        let on_disk = bridge.state.self_acked_log();
        assert_eq!(
            (
                (bridge.self_acked.clone(), bridge.self_floor),
                (on_disk.offsets, on_disk.floor)
            ),
            ((want.clone(), Some(0)), (want, Some(0))),
            "{case}: the window and the acked file, the floor 0 apart from what they hold"
        );
        bridge.load_self_window();
        assert_eq!(
            bridge.self_floor,
            Some(0),
            "{case}: a relaunch's window, read back"
        );

        bridge.on_inbox_record(&(forged, own_lane.clone(), forged_body.clone()), None);
        bridge.settle_acks();
        let (accused, delivered) = forged_verdict(&lane, &mut forger, forged);
        let control = mutant == Some(Mutant::FloorOffTheOldest);
        assert_eq!(
            (accused, delivered),
            (!control, control),
            "{case}: the forged record @{forged} between the posts: accused {accused}, \
             delivered {delivered}; window {:?}",
            bridge.self_acked
        );
        bridge.close_subscriptions();
    }
}

/// **A LOST OR UNREADABLE `acked` FILE ACCUSES NOTHING IT CANNOT JUDGE** (critic item F20,
/// `Bridge::floor_at_the_head`): a node posts to a session of its own, the record landed and
/// remembered; between two bridge runs its `acked` file is deleted, or its bytes stop
/// reading as text. The relaunch loads a window that waits for the head, and its attach
/// floors it at the log's head, above that record, and writes the floor, which a later
/// load reads back. The earlier own record offered again draws no `forged-self` verdict,
/// and a record a co-holder of the node cap writes onto the lane under the node's `<src>`
/// after the reload is refused as `forged-self`. NEGATIVE CONTROL
/// (`Mutant::UnreadWindowAtZero`, the floor 0 every build before F20 loaded such a file
/// at): the node's own earlier record is accused as forged.
#[cfg(unix)]
#[test]
fn a_lost_or_unreadable_acked_file_accuses_nothing_it_cannot_judge() {
    use crate::sim::world::Mutant;
    for (i, (unreadable, mutant)) in [
        (false, None),
        (true, None),
        (false, Some(Mutant::UnreadWindowAtZero)),
        (true, Some(Mutant::UnreadWindowAtZero)),
    ]
    .into_iter()
    .enumerate()
    {
        let case = format!("unreadable={unreadable} {mutant:?}");
        let sessions = [(1, "s-a"), (2, "s-r")];
        let (lane, mut bridge) = FakeVerbLane::bridge(&sessions);
        let state = bridge.state.root().to_path_buf();
        assert!(bridge.refresh_sessions().is_ok());
        let (socket, _broker, _server) = serve_log(&lane, &format!("lost-acked-{i}"));
        bridge.cfg.broker = socket.clone();
        assert!(bridge.attach_broker(), "{case}: the first run's attach");
        *lane.outbox.lock().unwrap() = Some(queued_note("s-a", 1, "@s-r", "mine"));
        bridge.drain_outbox();
        bridge.settle_acks();
        let own_lane = subject::post_lane("f", &bridge.node, "s-r", &bridge.node, "note");
        let (mut reader, _) = bridge.connect().unwrap();
        let (rows, _) = reader.fetch(0, &own_lane, 8).unwrap();
        let [(own, _, own_body)] = rows.as_slice() else {
            panic!("{case}: the note on s-r's lane once: {rows:?}");
        };
        let (own, own_body) = (*own, own_body.clone());
        assert!(
            bridge.state.self_acked().contains(&own),
            "{case}: PRECONDITION: the first run remembered @{own}"
        );
        bridge.close_subscriptions();
        drop(bridge);
        let acked = state.join("acked");
        if unreadable {
            std::fs::write(&acked, b"floor=0,\xff\xfe,").unwrap();
        } else {
            std::fs::remove_file(&acked).unwrap();
        }

        let (lane, mut bridge) = FakeVerbLane::bridge_with_state(&sessions, Some(&state));
        if let Some(mutant) = mutant {
            bridge.fx = Fx::os_without(mutant);
        }
        assert!(
            bridge.self_acked.is_empty() && bridge.self_floor.is_none(),
            "{case}: PRECONDITION: the relaunch's window waits for the head: floor {:?}, {:?}",
            bridge.self_floor,
            bridge.self_acked
        );
        assert!(bridge.refresh_sessions().is_ok());
        bridge.cfg.broker = socket;
        assert!(bridge.attach_broker(), "{case}: the relaunch's attach");
        let control = mutant.is_some();
        let floor = bridge.self_floor;
        let on_disk = bridge.state.self_acked_log();
        assert!(
            floor.is_some_and(|floor| control || floor > own) && on_disk.floor == floor,
            "{case}: the window floored at the head, above @{own}, and the file says so: \
             floor {floor:?}, the file's {:?}",
            on_disk.floor
        );
        bridge.load_self_window();
        assert_eq!(
            bridge.self_floor, floor,
            "{case}: a later load reads it back"
        );

        bridge.on_inbox_record(&(own, own_lane.clone(), own_body), None);
        bridge.settle_acks();
        let (accused, _) = forged_verdict(&lane, &mut reader, own);
        assert_eq!(
            accused, control,
            "{case}: the node's own earlier record @{own}: accused {accused}; floor {floor:?}"
        );

        let (forged, _) = reader
            .publish(bridge.producer_id ^ 1, 1, &own_lane, b"v=1 t=1 text=forged")
            .unwrap();
        bridge.on_inbox_record(
            &(forged, own_lane.clone(), b"v=1 t=1 text=forged".to_vec()),
            None,
        );
        bridge.settle_acks();
        let (accused, delivered) = forged_verdict(&lane, &mut reader, forged);
        assert!(
            accused && !delivered,
            "{case}: the record forged after the reload @{forged}: accused {accused}, \
             delivered {delivered}"
        );
        bridge.close_subscriptions();
    }
}

/// **AN ANSWER READ AFTER A LANDED POST IS TIMED TO ITS READ** (E07 review): a pass reads a
/// post's ack, which waits for its group, then another answer, whose conclusion settles the
/// group first (`Bridge::settled`). That settle's `outbox sent` takes thirty seconds on a
/// held clock here, and the answer's round trip still ends where its read ended: the post's
/// rows count the settle, and the answer's span counted it twice when it ended after it.
#[cfg(unix)]
#[test]
fn an_answer_read_after_a_landed_post_is_timed_to_its_read_not_past_the_posts_settle() {
    let (lane, mut bridge) = FakeVerbLane::bridge(&[(1, "s-a"), (2, "s-b")]);
    bridge.fx = Fx::os().on_a_held_clock();
    let clock = bridge.fx.clone();
    lane.on_first_retire(move || clock.advance(Duration::from_secs(30)));
    let post = parse_drain(b"post sid=s-a id=1 to=@s-b kind=note len=4\nonce")
        .posts
        .remove(0);
    let pin = PublishPin {
        seq: bridge.next_seq().unwrap(),
        via_key: false,
        identity: None,
        original: None,
        original_off: None,
        owner: Some(1),
        at: None,
    };
    let subject = format!("/f/f/in/{0}/s-b/{0}/note", bridge.node);
    bridge.landings.push(Landing {
        of: Outboxed::Post(post),
        subject,
        pin,
        off: 4,
        deduped: false,
        actual: None,
        entry: None,
    });
    bridge.attaching = true;
    let read = bridge.fx.now();
    let answer = crate::pipeline::Unacked {
        seq: 1,
        first: false,
        sent: read,
        tag: Then::Ev {
            payload: "ev".to_string(),
            sid: None,
            record: None,
        },
        rider: None,
    };
    bridge.grouping = true;
    bridge.settled((answer, Ok((5, false))), None, read);
    bridge.grouping = false;
    assert_eq!(
        lane.sent.lock().unwrap().as_slice(),
        ["outbox sent s-a 1 off=4"],
        "the landed post settled ahead of the answer's conclusion"
    );
    assert_eq!(
        bridge.fx.now().saturating_duration_since(read),
        Duration::from_secs(30),
        "the settle's `outbox sent` moved the held clock"
    );
    assert_eq!(
        bridge.attach_rtt,
        Some(Duration::ZERO),
        "the answer's round trip ends at its read, not past the post's settle"
    );
}

/// **A REPLAYED RECORD WAITS FOR ITS POST'S RETIREMENT, SO A MOVE AFTER IT ADDS NO COPY**: a
/// bridge left an own record's entry standing with its post queued (aterm refused the
/// retirement). The next attach's replay holds the record (hold), its drain's retry reads
/// it back at the post's pin and retires the post, aterm's `OK` ending the entry (E07
/// review), and only then does the refill deliver it, in the same attach; the log's move
/// after it finds the post retired and publishes nothing. CONTROL (the replay delivers at
/// once, `deliver_replay_at_once`): the record is handed over ahead of the retirement, the
/// post still queued, and after the move the drain's retry finds no copy on B and
/// publishes a second, handed over too.
#[cfg(unix)]
#[test]
fn a_replayed_record_waits_for_its_posts_retirement_so_a_move_delivers_it_once() {
    // `at`: an entry an older build journaled (published again at its pin) and one this
    // build journals (read back where it landed, `Bridge::landed_since`).
    for (hold, at) in [
        (true, None),
        (false, None),
        (true, Some(0)),
        (false, Some(0)),
    ] {
        let case = format!("hold={hold} at={at:?}");
        let sessions = [(1, "s-a"), (2, "s-b")];
        let (lane, mut first) = FakeVerbLane::bridge(&sessions);
        let state = first.state.root().to_path_buf();
        let (a, _broker_a, _server_a) = serve_log(&lane, "replay-a");
        first.cfg.broker = a.clone();
        assert!(first.attach_broker(), "{case}: boot's first attach");
        lane.refuse_retirements(true);
        let post = "post sid=s-a id=1 to=@s-b kind=note len=4\nonce".to_string();
        *lane.outbox.lock().unwrap() = Some(post.clone());
        first.drain_outbox();
        let pin = first
            .state
            .post_pin("s-a", 1)
            .unwrap()
            .expect("the post's pin stays with its retirement refused");
        let face = pin.identity.as_ref().unwrap().subject.clone();
        let (rows, _) = first.conn_now().unwrap().fetch(0, &face, 1).unwrap();
        let (on_a, subject, bytes) = rows.into_iter().next().expect("the note on A");
        first
            .state
            .set_in_flight(&InFlight {
                pin: pin.clone(),
                subject,
                body: bytes,
                log: first.conn.as_ref().and_then(|c| c.seen_log_identity()),
                at,
            })
            .unwrap();
        first.close_subscriptions();
        drop(first);

        let (second, mut bridge) = FakeVerbLane::bridge_with_state(&sessions, Some(&state));
        bridge.deliver_replay_at_once = !hold;
        bridge.cfg.broker = a;
        second.retire_from_outbox();
        *second.outbox.lock().unwrap() = Some(post.clone());
        bridge
            .state
            .refuse_writes_of(Some(&format!("inflight/{}", pin.seq)));
        assert!(bridge.attach_broker(), "{case}: the attach on A");
        assert!(
            second.delivery_offsets().contains(&on_a),
            "{case}: the replayed note delivered on A"
        );
        let requests = second.requests.lock().unwrap().clone();
        let retired = requests
            .iter()
            .position(|l| l.starts_with("outbox sent s-a 1 "));
        let delivered = requests.iter().position(|l| {
            l.starts_with("deliver s-b ")
                && l.split_whitespace().any(|w| w == format!("off={on_a}"))
        });
        assert_eq!(
            retired.is_some_and(|retired| delivered > Some(retired)),
            hold,
            "{case}: the note on A handed over after its post's retirement (hold), or ahead \
             of it (the control): {requests:?}"
        );
        bridge.close_subscriptions();
        bridge.conn = None;
        bridge.state.refuse_writes_of(None);

        let (b, _broker_b, _server_b) = serve_log(&second, "replay-b");
        bridge.cfg.broker = b;
        let (mut padding, _) = bridge.connect().unwrap();
        for seq in 1..=32 {
            padding.publish(92, seq, "/f/f/x/padding", b"v=1").unwrap();
        }
        drop(padding);
        assert!(bridge.attach_broker(), "{case}: the attach on B");
        bridge.drain_outbox();
        let (mut writer, _) = bridge.connect().unwrap();
        let sentinel_face = format!(
            "/f/{}/in/{}/s-b/h-writer/note",
            bridge.cfg.fleet, bridge.node
        );
        let (sentinel, _) = writer
            .publish(81, 1, &sentinel_face, b"v=1 t=1 text=sentinel")
            .unwrap();
        loop {
            match bridge
                .mailbox
                .take(aterm_spec::wait::MUST_HAPPEN, bridge.fx.now())
            {
                Some(Item::Inbox(rec, origin)) => {
                    let at = rec.0;
                    bridge.on_inbox_record(&rec, origin);
                    if at == sentinel {
                        break;
                    }
                }
                Some(_) => {}
                None => panic!("{case}: the live tail ended before the sentinel"),
            }
        }
        assert!(
            bridge.state.in_flight().unwrap().is_empty(),
            "{case}: no entry stands on B"
        );
        let (notes, _) = bridge.conn_now().unwrap().fetch(0, &face, 8).unwrap();
        let on_b: Vec<u64> = notes.iter().map(|(off, _, _)| *off).collect();
        assert_eq!(
            on_b.len(),
            usize::from(!hold),
            "{case}: the note on B: none, its post retired on A (hold), or the retry's copy \
             (the control): {on_b:?}"
        );
        let mut want: BTreeSet<String> = [format!("off={on_a}")].into();
        want.extend(on_b.iter().map(|off| format!("off={off}")));
        assert_eq!(
            notes_delivered(&second, "once"),
            want,
            "{case}: once from A (hold), or from A and again from B (the control)"
        );
        bridge.close_subscriptions();
    }
}

/// A READ BACK THAT MEETS ITS SUBJECT'S FLOOR READS AGAIN TOLD (sim item B05): the
/// entry's record is carried below the base as its subject's last value, and the rules
/// set next keep every record of its class, which they take whole from the base alone
/// (astream's `retention::carried_since`), so the subject's oldest lies above the record
/// the broker still holds. A read on at the floor finds nothing of it (the control:
/// `transport::fetch_kept`), the told one finds it, and the attach's replay and the
/// drain's retry both settle on it: the note stays on the bus once. Since the F3 review
/// the attach's refill reads the recipient's lane told (`Bridge::lane_page`) and so meets
/// the held note; since the E07 review the drain's retry, which reads it back at the
/// post's pin, ends its entry at aterm's `OK`, so that refill delivers it once, in the
/// same attach (`a_replayed_record_waits_for_its_posts_retirement_so_a_move_delivers_it_\
/// once`).
#[cfg(unix)]
#[test]
fn a_record_held_below_its_subjects_oldest_is_read_back_and_not_published_again() {
    use crate::broker_rules::{EV_FILTER, MAIL_FILTER};
    use astream_broker::{Retention, Rule};
    let sessions = [(1, "s-a"), (2, "s-b")];
    let (lane, mut first) = FakeVerbLane::bridge(&sessions);
    let state = first.state.root().to_path_buf();
    let (a, broker, _server) = serve_log(&lane, "held-oldest");
    broker.set_retention_interval(Duration::from_secs(3600));
    // First a class that keeps every record of the lanes and one that keeps another
    // lane's last value only; then one class for every lane, which reaches both and so is
    // whole only from the base (a last-value class carries none of its records).
    let rules = |first: bool| {
        let mut classes = vec![Rule::prefix(EV_FILTER).unwrap()];
        if first {
            classes.push(Rule::hold("/f/*/in/*/s-z/>").unwrap().last_only());
        }
        classes.push(Rule::hold(MAIL_FILTER).unwrap());
        Retention::new(classes, Rule::default_keeping_all()).unwrap()
    };
    broker.set_retention(rules(true)).unwrap();
    first.cfg.broker = a.clone();
    assert!(first.attach_broker(), "the first attach");
    lane.refuse_retirements(true);
    let post = "post sid=s-a id=1 to=@s-b kind=note len=4\nonce".to_string();
    *lane.outbox.lock().unwrap() = Some(post.clone());
    first.drain_outbox();
    let pin = first
        .state
        .post_pin("s-a", 1)
        .unwrap()
        .expect("the post's pin stays with its retirement refused");
    let face = pin.identity.as_ref().unwrap().subject.clone();
    let (rows, _) = first.conn_now().unwrap().fetch(0, &face, 1).unwrap();
    let (on_bus, subject, bytes) = rows.into_iter().next().expect("the note");
    first
        .state
        .set_in_flight(&InFlight {
            pin: pin.clone(),
            subject,
            body: bytes,
            log: first.conn.as_ref().and_then(|c| c.seen_log_identity()),
            at: Some(0),
        })
        .unwrap();
    first.close_subscriptions();
    drop(first);

    let (mut writer, _) = transport::connect(&a, aterm_spec::wait::MUST_HAPPEN).unwrap();
    for seq in 1..=4 {
        writer.publish(92, seq, "/f/f/x/padding", b"v=1").unwrap();
    }
    let (_, (_, head)) = writer.fetch(0, "/f/>", 0).unwrap();
    broker.retain_before_carrying_state(head).unwrap();
    broker.set_retention(rules(false)).unwrap();
    let of_face = astream_wire::Filter::new(&face).unwrap();
    assert!(
        broker.oldest(&of_face) > on_bus,
        "the subject's oldest lies above the note the broker still holds"
    );
    let ((rows, _), floor) = transport::fetch_kept(&mut writer, 0, &face, 8).unwrap();
    assert!(
        floor.is_some() && rows.is_empty(),
        "CONTROL: a read on at the floor finds nothing of the note: {rows:?}"
    );
    let (mut told, _) = transport::connect(&a, aterm_spec::wait::MUST_HAPPEN).unwrap();
    let ((rows, _), _) = transport::fetch_told_kept(&mut told, 0, &face, 8).unwrap();
    assert_eq!(
        rows.iter().map(|(off, _, _)| *off).collect::<Vec<_>>(),
        vec![on_bus],
        "a told read finds the note held below the oldest"
    );

    let (second, mut bridge) = FakeVerbLane::bridge_with_state(&sessions, Some(&state));
    bridge.cfg.broker = a;
    second.retire_from_outbox();
    *second.outbox.lock().unwrap() = Some(post);
    assert!(
        bridge.attach_broker(),
        "the attach's drain retires the post, and its refill delivers the held note"
    );
    bridge.drain_outbox();
    let ((rows, _), _) = transport::fetch_told_kept(&mut told, 0, &face, 8).unwrap();
    assert_eq!(
        rows.iter().map(|(off, _, _)| *off).collect::<Vec<_>>(),
        vec![on_bus],
        "the note is on the bus once: neither the replay nor the drain's retry published \
         it again"
    );
    assert!(
        bridge.state.in_flight().unwrap().is_empty(),
        "the replay settled the entry where its record is"
    );
    assert_eq!(
        second.delivery_offsets(),
        [on_bus],
        "the held note delivered once, from below the oldest"
    );
    bridge.close_subscriptions();
}

/// **A DEDUP'S READ BACK THAT A CUT MET SINCE THE ANSWER FINDS THE RECORD RETENTION STILL
/// HOLDS, AND THE POST IS NOT SENT AGAIN** (finding F3). A retry at a pin whose record
/// landed is answered deduped at that offset, which lies at or above the base (a cut drops
/// the keys below it); before its read back (`Bridge::cut_after_the_answer`, astream's
/// writer running retention between two requests of the connection) a cut carries the
/// unread record below the base, and the rules set next take its class whole from the base
/// alone, so the subject's oldest lies above the record the broker still holds. The read
/// back is refused, read again told, finds the record and verifies it: settled where it is,
/// the link kept. CONTROL (`Mutant::ReadAtWithoutTheHeld`, the refusal taken for no rows, as
/// before F3): nothing to verify, the post is left queued, and its retry, the key gone with
/// the cut, appends a second copy beside the one a reader can still be handed. A third arm
/// holds the broker at its connection cap from the cut on (F3 review), so the told read's
/// connection of its own is refused: the read back failed, so the link is dropped and the
/// post's fate left unknown (`Bridge::fate_unknown`), never taken for a cut.
#[cfg(unix)]
#[test]
fn a_dedup_whose_record_a_cut_carried_below_its_oldest_since_the_answer_is_not_sent_again() {
    let rules = lanes_whole_from_the_base;
    for arm in ["told", "control", "capped"] {
        let (control, capped) = (arm == "control", arm == "capped");
        let (lane, mut bridge) = FakeVerbLane::bridge(&[(1, "s-a"), (2, "s-b")]);
        let (a, broker, _server) = serve_log(&lane, "dedup-cut");
        let broker = Arc::new(broker);
        broker.set_retention_interval(Duration::from_secs(3600));
        broker.set_retention(rules(true)).unwrap();
        bridge.cfg.broker = a.clone();
        bridge.conn = Some(bridge.connect().unwrap().0.into());
        if control {
            bridge.fx = crate::fx::Fx::os_without(crate::sim::world::Mutant::ReadAtWithoutTheHeld);
        }
        let node = bridge.node.clone();
        let face = format!("/f/f/in/{node}/s-b/{node}/note");
        let mut body = Body::new(1);
        body.from = Some("s-a".into());
        body.text = "once".into();
        let encoded = body.encode();
        let identity = publication_identity(&face, &encoded);
        let pin = PublishPin {
            seq: bridge.next_seq().unwrap(),
            via_key: false,
            identity: Some(identity.clone()),
            original: None,
            original_off: None,
            owner: Some(1),
            at: None,
        };
        let (off, deduped, _) = bridge.publish_reserved(&pin, &face, &encoded).unwrap();
        assert!(!deduped, "the first attempt lands");

        let (cutter, at, of_face) = (Arc::clone(&broker), a.clone(), face.clone());
        bridge.cut_after_the_answer = Some(Box::new(move |answered| {
            assert_eq!(
                answered, off,
                "the retry is answered at the record's offset"
            );
            let (mut writer, _) = transport::connect(&at, aterm_spec::wait::MUST_HAPPEN).unwrap();
            for seq in 1..=4 {
                writer.publish(92, seq, "/f/f/x/padding", b"v=1").unwrap();
            }
            let (_, (_, head)) = writer.fetch(0, "/f/>", 0).unwrap();
            cutter.retain_before_carrying_state(head).unwrap();
            cutter.set_retention(rules(false)).unwrap();
            let of_face = astream_wire::Filter::new(&of_face).unwrap();
            assert!(
                cutter.oldest(&of_face) > off,
                "PRECONDITION: the subject's oldest lies above the record the broker holds"
            );
            if capped {
                cutter.set_max_conns(1);
            }
        }));
        let retried = bridge.publish_reserved(&pin, &face, &encoded);
        bridge.cut_after_the_answer = None;
        broker.set_max_conns(astream_broker::broker::MAX_CONNS);
        let (mut told, _) = transport::connect(&a, aterm_spec::wait::MUST_HAPPEN).unwrap();
        let copies = |told: &mut Conn| -> Vec<u64> {
            let ((rows, _), _) = transport::fetch_told_kept(told, 0, &face, 8).unwrap();
            rows.iter().map(|(off, _, _)| *off).collect()
        };
        if capped {
            let failed = retried.unwrap_err();
            assert!(
                failed
                    .get_ref()
                    .is_some_and(|inner| inner.is::<PublicationReadFailure>()),
                "the read back failed: {failed}"
            );
            assert!(
                bridge.conn.is_none() && bridge.fate_unknown(&failed),
                "the link dropped, the post's fate unknown: {failed}"
            );
            assert_eq!(copies(&mut told), vec![off], "nothing sent again");
            continue;
        }
        if !control {
            assert_eq!(
                retried.unwrap(),
                (off, true, Some(identity)),
                "the record held below the oldest is read back and verified"
            );
            assert!(bridge.conn.is_some(), "the link is kept");
            assert_eq!(copies(&mut told), vec![off], "the post is on the bus once");
            continue;
        }
        let refused = retried.unwrap_err();
        assert!(
            refused.to_string().contains("retention gave up the record"),
            "CONTROL: nothing to verify, the post left queued: {refused}"
        );
        bridge.conn = Some(bridge.connect().unwrap().0.into());
        let (again, deduped, _) = bridge.publish_reserved(&pin, &face, &encoded).unwrap();
        assert!(!deduped, "CONTROL: the cut dropped the key");
        assert_eq!(
            copies(&mut told),
            vec![off, again],
            "CONTROL: the post on the bus twice, both copies a reader can be handed"
        );
    }
}

/// **A SENDER'S EARLIER POST OF THE SAME WORDS, PUBLISHED AGAIN BY THE REPLAY ABOVE ITS NEXT
/// POST'S HEAD, IS NEVER TAKEN FOR THAT POST** (aterm-link B05 review). `s-a` posts `ok` to
/// `s-b` twice. Post 1 lands and is retired, but its record in flight stands (a clear the
/// disk refused); post 2 is reserved and journaled at the head past post 1, and the bridge
/// stops before it publishes. Retention gives up post 1's unread copy. The next attach's
/// replay finds nothing of post 1 and publishes it again, above post 2's head, and claims
/// it ([`Bridge::claims`]); post 2's read back skips that copy, finds none of its own, and
/// publishes post 2, and the drain's retry settles post 2 where it landed: `s-b` is handed
/// `ok` twice, once a post. CONTROL (`read_back_past_claims`): the read back takes post 1's
/// copy for post 2, which never reaches the bus.
#[cfg(unix)]
#[test]
fn a_senders_earlier_post_published_again_is_never_taken_for_its_next_post_of_the_same_words() {
    for control in [false, true] {
        let sessions = [(1, "s-a"), (2, "s-b")];
        let (lane, mut first) = FakeVerbLane::bridge(&sessions);
        let state = first.state.root().to_path_buf();
        let (a, broker, _server) = serve_log(&lane, "same-words");
        broker.set_retention_interval(Duration::from_secs(3600));
        first.cfg.broker = a.clone();
        assert!(first.attach_broker(), "the first attach");
        lane.refuse_retirements(true);
        lane.retire_from_outbox();
        let post = |id: u64| format!("post sid=s-a id={id} to=@s-b kind=note len=2\nok");
        *lane.outbox.lock().unwrap() = Some(post(1));
        first.drain_outbox();
        let pin = first
            .state
            .post_pin("s-a", 1)
            .unwrap()
            .expect("post 1's pin stays with its retirement refused");
        let face = pin.identity.as_ref().unwrap().subject.clone();
        let (rows, _) = first.conn_now().unwrap().fetch(0, &face, 1).unwrap();
        let (on_bus, subject, bytes) = rows.into_iter().next().expect("post 1 on the bus");
        let log = first.conn.as_ref().and_then(|c| c.seen_log_identity());
        let entry = |pin: &PublishPin, at: u64| InFlight {
            pin: pin.clone(),
            subject: subject.clone(),
            body: bytes.clone(),
            log,
            at: Some(at),
        };
        first.state.set_in_flight(&entry(&pin, on_bus)).unwrap();
        lane.refuse_retirements(false);
        *lane.outbox.lock().unwrap() = Some(post(1));
        // The clear the disk refuses: the post journal takes no write, and of the retry's
        // settle the retirement holds unwritten while the entry's end does not.
        first
            .state
            .refuse_writes_of(Some(crate::state::journal::FILE));
        first.drain_outbox();
        first.state.refuse_writes_of(None);
        assert_eq!(
            first.state.post_pin("s-a", 1).unwrap(),
            None,
            "post 1 retired"
        );
        assert_eq!(
            first.state.in_flight().unwrap().len(),
            1,
            "post 1's entry stands, as a clear the disk refused leaves it"
        );
        let next = PublishPin {
            seq: first.next_seq().unwrap(),
            owner: Some(2),
            at: Some(on_bus + 1),
            ..pin.clone()
        };
        first.state.set_post_pin("s-a", 2, &next).unwrap();
        first
            .state
            .set_in_flight(&entry(&next, on_bus + 1))
            .unwrap();
        first.close_subscriptions();
        drop(first);

        broker
            .set_retention(crate::broker_rules::rules_scaled(1 << 30, 1, 1 << 30))
            .unwrap();
        broker.retention_tick_at(broker.now_ms()).unwrap();
        let (mut told, _) = transport::connect(&a, aterm_spec::wait::MUST_HAPPEN).unwrap();
        let copies = |told: &mut Conn| -> Vec<u64> {
            let ((rows, _), _) = transport::fetch_told_kept(told, 0, &face, 8).unwrap();
            rows.iter().map(|(off, _, _)| *off).collect()
        };
        assert_eq!(
            copies(&mut told),
            Vec::<u64>::new(),
            "post 1's copy given up"
        );
        broker
            .set_retention(crate::broker_rules::default_rules())
            .unwrap();

        let (second, mut bridge) = FakeVerbLane::bridge_with_state(&sessions, Some(&state));
        bridge.cfg.broker = a;
        bridge.read_back_past_claims = control;
        second.retire_from_outbox();
        *second.outbox.lock().unwrap() = Some(post(2));
        // The attach meets post 2's record held until its post is retired (or, in the
        // control, post 1's copy post 2 settled on): the drain retires it.
        let _ = bridge.attach_broker();
        bridge.drain_outbox();
        let on_the_bus = copies(&mut told);
        let retired: Vec<String> = second
            .sent
            .lock()
            .unwrap()
            .iter()
            .filter(|l| l.starts_with("outbox sent s-a 2 "))
            .cloned()
            .collect();
        assert_eq!(
            retired.len(),
            1,
            "control={control}: post 2 retired: {retired:?}"
        );
        if control {
            assert_eq!(
                on_the_bus.len(),
                1,
                "CONTROL: post 2 settled on post 1's copy, never published: {on_the_bus:?}"
            );
            assert!(retired[0].contains(&format!("off={}", on_the_bus[0])));
        } else {
            assert_eq!(
                on_the_bus.len(),
                2,
                "post 1 again and post 2, each once: {on_the_bus:?}"
            );
            assert!(
                retired[0].contains(&format!("off={}", on_the_bus[1])),
                "post 2 retired where it landed, not on post 1's copy: {retired:?} \
                 {on_the_bus:?}"
            );
        }
        bridge.close_subscriptions();
    }
}

#[test]
fn a_key_winner_republished_on_another_log_does_not_become_a_later_ask() {
    const SID: &str = "s-host";
    const KEY: &str = "k-log";
    let (lane, mut bridge) = FakeVerbLane::bridge(&[(1, SID)]);
    let (a, _broker_a, _server_a) = serve_log(&lane, "key-a");
    bridge.cfg.broker = a;
    assert!(bridge.attach_broker());

    *lane.outbox.lock().unwrap() = Some(format!(
        "post sid={SID} id=1 to=@{SID} kind=note key={KEY} len=8\noriginal"
    ));
    bridge.drain_outbox();
    assert_eq!(lane.sent.lock().unwrap().len(), 1);
    let winner = bridge.state.key_pin(SID, KEY).unwrap().unwrap();
    let old_off = winner.original_off.unwrap();
    let original = winner.identity.as_ref().unwrap();
    let (rows, _) = bridge
        .conn_now()
        .unwrap()
        .fetch(old_off, &original.subject, 1)
        .unwrap();
    let (at, face, bytes) = rows.into_iter().next().unwrap();
    assert_eq!(at, old_off);
    assert_eq!(winner.owner, Some(1));
    bridge
        .state
        .set_in_flight(&InFlight {
            pin: winner.clone(),
            subject: face,
            body: bytes,
            log: None,
            at: None,
        })
        .unwrap();

    let ask_face = format!("{}/ask", original.subject.strip_suffix("/note").unwrap());
    let mut ask = Body::new(0);
    ask.from = Some(SID.into());
    ask.dl = Some(30_000);
    ask.text = "later ask".into();
    bridge
        .state
        .set_post_pin(
            SID,
            2,
            &PublishPin {
                seq: winner.seq,
                via_key: true,
                identity: Some(publication_identity(&ask_face, &ask.encode())),
                original: winner.identity.clone(),
                original_off: Some(old_off),
                owner: winner.owner,
                at: None,
            },
        )
        .unwrap();
    *lane.outbox.lock().unwrap() = Some(format!(
        "post sid={SID} id=2 to=@{SID} kind=ask key={KEY} dl=30000 len=9\nlater ask"
    ));

    bridge.close_subscriptions();
    bridge.conn = None;
    let (b, _broker_b, _server_b) = serve_log(&lane, "key-b");
    bridge.cfg.broker = b;
    let (mut padding, _) = bridge.connect().unwrap();
    for seq in 1..=32 {
        padding.publish(92, seq, "/f/f/x/padding", b"v=1").unwrap();
    }
    drop(padding);
    assert!(bridge.attach_broker());
    assert!(bridge.state.in_flight().unwrap().is_empty());
    let (notes, _) = bridge
        .conn_now()
        .unwrap()
        .fetch(0, &original.subject, 8)
        .unwrap();
    assert_eq!(notes.len(), 1, "the journal recovered exactly one note");
    let landed = notes[0].0;
    assert_ne!(landed, old_off, "offsets belong to their broker log");
    assert_eq!(publication_identity(&notes[0].1, &notes[0].2), *original);

    assert_eq!(lane.sent.lock().unwrap().len(), 2);
    assert!(
        bridge.asked.is_empty() && bridge.deadlines.is_empty(),
        "a deduped note became a later ask at @{landed}: asked={:?}, deadlines={:?}",
        bridge.asked,
        bridge.deadlines
    );
    assert!(lane.sent.lock().unwrap()[1].contains(&format!("off={landed} dup=1")));
    let reconciled = bridge.state.key_pin(SID, KEY).unwrap().unwrap();
    assert_eq!(reconciled.seq, winner.seq);
    assert_eq!(reconciled.identity, winner.identity);
    assert_eq!(reconciled.owner, Some(1));
    assert_eq!(reconciled.original_off, Some(landed));
    assert!(bridge
        .conn_now()
        .unwrap()
        .fetch(0, &ask_face, 8)
        .unwrap()
        .0
        .is_empty());
}

#[test]
fn keyed_reposts_keep_the_winner_and_reconcile_a_retained_away_winner_before_retirement() {
    use std::os::unix::fs::PermissionsExt as _;
    for same in [false, true] {
        let (initial_lane, mut bridge) = FakeVerbLane::bridge(&[(1, "s-host")]);
        let state = bridge.state.root().to_path_buf();
        let socket = initial_lane.dir.path().join("broker.sock");
        let broker = crate::test_broker(initial_lane.dir.path().join("broker.log")).unwrap();
        let _server = broker.serve(&socket).unwrap();
        bridge.cfg.broker = socket.to_string_lossy().into_owned();
        bridge.conn = Some(bridge.connect().unwrap().0.into());
        let recovered;
        let lane = if same {
            let mut original = Body::new(0);
            original.from = Some("s-host".into());
            original.dl = Some(30_000);
            original.text = "original".into();
            let face = format!("/f/f/in/p/h-a/{}/ask", bridge.node);
            let key = PublishPin {
                seq: bridge.next_seq().unwrap(),
                via_key: false,
                identity: Some(publication_identity(&face, &original.encode())),
                original: None,
                original_off: None,
                owner: Some(1),
                at: None,
            };
            bridge.state.set_key_pin("s-host", "k-first", &key).unwrap();
            drop(bridge);
            (recovered, bridge) = FakeVerbLane::bridge_with_state(&[(1, "s-host")], Some(&state));
            bridge.cfg.broker = socket.to_string_lossy().into_owned();
            bridge.conn = Some(bridge.connect().unwrap().0.into());
            &recovered
        } else {
            &initial_lane
        };
        let queue = |lane: &FakeVerbLane, id: u64, to: &str, kind: &str, text: &str| {
            *lane.outbox.lock().unwrap() = Some(format!(
                "post sid=s-host id={id} to={to} kind={kind} key=k-first dl=30000 len={}\n{text}",
                text.len()
            ));
        };
        queue(
            lane,
            1,
            "h-a",
            if same { "ask" } else { "note" },
            "original",
        );
        bridge.drain_outbox();
        assert_eq!(lane.sent.lock().unwrap().len(), 1);
        let first = bridge.state.key_pin("s-host", "k-first").unwrap().unwrap();
        let first_off = lane.sent.lock().unwrap()[0]
            .split_whitespace()
            .find_map(|word| word.strip_prefix("off="))
            .unwrap()
            .parse::<u64>()
            .unwrap();
        queue(
            lane,
            2,
            "h-b",
            "ask",
            "different destination, kind and text",
        );
        bridge.drain_outbox();
        assert_eq!(lane.sent.lock().unwrap().len(), 2);
        assert!(lane.sent.lock().unwrap()[1].contains(&format!("off={first_off} dup=1")));
        assert_eq!(
            bridge
                .state
                .key_pin("s-host", "k-first")
                .unwrap()
                .unwrap()
                .identity,
            first.identity
        );

        broker.retain_before_carrying_state(first_off + 1).unwrap();
        let keys = state.join("keys");
        let permissions = std::fs::metadata(&keys).unwrap().permissions();
        std::fs::set_permissions(&keys, std::fs::Permissions::from_mode(0o500)).unwrap();
        let (target, text) = if same {
            ("h-a", "original")
        } else {
            ("h-c", "after retention")
        };
        queue(lane, 3, target, "ask", text);
        bridge.drain_outbox();
        std::fs::set_permissions(&keys, permissions).unwrap();
        assert_eq!(
            lane.sent.lock().unwrap().len(),
            2,
            "no retirement before durable reconciliation"
        );
        let pending = bridge.state.post_pin("s-host", 3).unwrap().unwrap();
        assert_eq!(pending.identity == pending.original, same);
        assert_eq!(pending.original_off, Some(first_off));
        drop(bridge);

        let (second, mut bridge) = FakeVerbLane::bridge_with_state(&[(1, "s-host")], Some(&state));
        bridge.cfg.broker = socket.to_string_lossy().into_owned();
        bridge.conn = Some(bridge.connect().unwrap().0.into());
        queue(&second, 3, target, "ask", text);
        bridge.drain_outbox();
        assert_eq!(second.sent.lock().unwrap().len(), 1);
        let landed = second.sent.lock().unwrap()[0]
            .split_whitespace()
            .find_map(|word| word.strip_prefix("off="))
            .unwrap()
            .parse::<u64>()
            .unwrap();
        assert_ne!(landed, first_off);
        assert!(
            !second.sent.lock().unwrap()[0].contains("dup=1"),
            "this post's own crash retry"
        );
        assert_eq!(bridge.asked.get(&landed).map(String::as_str), Some(target));
        assert!(
            bridge.deadlines.contains_key(&landed),
            "the appended ask still expires, same={same}"
        );
        assert_eq!(
            bridge
                .state
                .key_pin("s-host", "k-first")
                .unwrap()
                .unwrap()
                .identity,
            pending.identity
        );
        let face = &pending.identity.unwrap().subject;
        assert_eq!(
            bridge
                .conn_now()
                .unwrap()
                .fetch(first_off + 1, face, 8)
                .unwrap()
                .0
                .len(),
            1,
            "lost reconciliation creates no duplicate"
        );
    }
}

/// **A KEYED RE-POST PAST A CUT THAT CARRIED ITS KEY'S WINNER IS RETIRED AT THE WINNER** (the
/// F23 sim's keyed world, [`Bridge::keyed_winner_at`]). The winner lands on the person's
/// lane, unread, and a cut under aterm's rules carries it below the base: astream drops its
/// dedup key all the same. The re-post under its key is read back at the winner's offset
/// first and retired there (`dup=1`), nothing published: by the drain, and by the attach's
/// replay where the re-post was in flight, its answer lost in a kill (`replay`), the drain
/// then retiring it at the winner. NEGATIVE CONTROL
/// (`Mutant::KeyedRepostWithoutReadingBack`): published with no read back, the dedup no
/// longer answers, and the re-post lands a second copy of its key beside the winner.
#[test]
fn a_keyed_repost_past_a_cut_that_carried_its_winner_is_retired_at_the_winner() {
    for (rule, replay) in [(true, false), (false, false), (true, true), (false, true)] {
        let (lane, mut bridge) = FakeVerbLane::bridge(&[(1, "s-host")]);
        if !rule {
            bridge.fx =
                crate::fx::Fx::os_without(crate::sim::world::Mutant::KeyedRepostWithoutReadingBack);
        }
        let socket = lane.dir.path().join("broker.sock");
        let broker = crate::test_broker(lane.dir.path().join("broker.log")).unwrap();
        broker.set_retention_interval(Duration::from_secs(3600));
        crate::broker_rules::retain(&broker).unwrap();
        let _server = broker.serve(&socket).unwrap();
        bridge.cfg.broker = socket.to_string_lossy().into_owned();
        bridge.conn = Some(bridge.connect().unwrap().0.into());
        let queue = |id: u64, text: &str| {
            *lane.outbox.lock().unwrap() = Some(format!(
                "post sid=s-host id={id} to=h-a kind=note key=k len={}\n{text}",
                text.len()
            ));
        };
        let off_of = |line: &str| -> u64 {
            line.split_whitespace()
                .find_map(|word| word.strip_prefix("off="))
                .and_then(|off| off.parse().ok())
                .unwrap()
        };
        queue(1, "winner");
        bridge.drain_outbox();
        let winner = off_of(&lane.sent.lock().unwrap()[0]);
        broker.retain_before_carrying_state(winner + 1).unwrap();
        let face = bridge
            .state
            .key_pin("s-host", "k")
            .unwrap()
            .unwrap()
            .identity
            .unwrap()
            .subject;
        // The control's second copy is what says the cut dropped the winner's key.
        assert!(
            bridge
                .conn_now()
                .unwrap()
                .fetch(winner, &face, 1)
                .unwrap()
                .0
                .first()
                .is_some_and(|(at, _, _)| *at == winner),
            "PRECONDITION: the cut carried the winner, unread"
        );
        if replay {
            // The re-post's pin as its drain makes it, and its entry, journaled before a
            // publish whose answer a kill lost.
            let keyed = bridge.state.key_pin("s-host", "k").unwrap().unwrap();
            let mut body = Body::new(1);
            body.from = Some("s-host".to_string());
            body.text = "again".to_string();
            let encoded = body.encode();
            let pin = PublishPin {
                seq: keyed.seq,
                via_key: true,
                identity: Some(publication_identity(&face, &encoded)),
                original: keyed.identity,
                original_off: keyed.original_off,
                owner: keyed.owner,
                at: keyed.at,
            };
            assert_eq!(
                pin.original_off,
                Some(winner),
                "PRECONDITION: the winner's offset"
            );
            bridge.state.set_post_pin("s-host", 2, &pin).unwrap();
            bridge
                .state
                .set_in_flight(&InFlight {
                    pin,
                    subject: face.clone(),
                    body: encoded,
                    log: bridge.conn.as_ref().and_then(|c| c.seen_log_identity()),
                    at: Some(head_of(&bridge)),
                })
                .unwrap();
            queue(2, "again");
            assert!(bridge.settle_in_flight(), "the replay settles the journal");
        } else {
            queue(2, "again");
        }
        bridge.drain_outbox();
        let retired = lane.sent.lock().unwrap()[1].clone();
        let copies = bridge
            .conn_now()
            .unwrap()
            .fetch(winner + 1, &face, 8)
            .unwrap()
            .0
            .len();
        if rule {
            assert!(
                retired.contains(&format!("off={winner} dup=1")) && copies == 0,
                "replay={replay}: the re-post is retired at its key's winner, nothing \
                 published: {retired}, {copies} copies above it"
            );
        } else {
            assert!(
                off_of(&retired) > winner && copies == 1,
                "CONTROL replay={replay}: the re-post lands a second copy of its key: \
                 {retired}, {copies} copies above the winner"
            );
        }
    }
}

/// **A KEYED RE-POST WHOSE OWN COPY THE REPLAY READ BACK IS RETIRED AT THAT COPY, NOT AT ITS
/// KEY'S WINNER** (the F23 review). An older build's state: the winner lands on the person's
/// lane, a cut under aterm's rules carries it below the base, unread, and drops its dedup
/// key; that build pins the re-post at the key's sequence, journals its entry, and publishes
/// it, which appends its own copy past the dropped key; a kill loses the answer. This build's
/// replay reads the re-post's own words back on the bus and holds that copy for the drain,
/// which retires the post there as its own landing (`off=<own>`, no `dup=1`): the key pin
/// moves to the re-post, and an ask's recipient and deadline are kept, so the copy its reader
/// is handed still expires. Retired at the winner instead, aterm would be told `dup=1` of a
/// record that is not where the re-post landed, and an ask on the bus would never expire.
#[test]
fn a_keyed_repost_whose_own_copy_the_replay_read_back_is_retired_at_that_copy() {
    for kind in ["note", "ask"] {
        let (lane, mut bridge) = FakeVerbLane::bridge(&[(1, "s-host")]);
        let socket = lane.dir.path().join("broker.sock");
        let broker = crate::test_broker(lane.dir.path().join("broker.log")).unwrap();
        broker.set_retention_interval(Duration::from_secs(3600));
        crate::broker_rules::retain(&broker).unwrap();
        let _server = broker.serve(&socket).unwrap();
        bridge.cfg.broker = socket.to_string_lossy().into_owned();
        bridge.conn = Some(bridge.connect().unwrap().0.into());
        let queue = |id: u64, kind: &str, text: &str| {
            let dl = if kind == "ask" { " dl=30000" } else { "" };
            *lane.outbox.lock().unwrap() = Some(format!(
                "post sid=s-host id={id} to=h-a kind={kind}{dl} key=k len={}\n{text}",
                text.len()
            ));
        };
        let off_of = |line: &str| -> u64 {
            line.split_whitespace()
                .find_map(|word| word.strip_prefix("off="))
                .and_then(|off| off.parse().ok())
                .unwrap()
        };
        queue(1, "note", "winner");
        bridge.drain_outbox();
        let winner = off_of(&lane.sent.lock().unwrap()[0]);
        broker.retain_before_carrying_state(winner + 1).unwrap();
        let keyed = bridge.state.key_pin("s-host", "k").unwrap().unwrap();
        let winner_face = keyed.identity.clone().unwrap().subject;
        let face = format!(
            "{}/{kind}",
            winner_face.strip_suffix("/note").unwrap_or(&winner_face)
        );
        // The older build's re-post: pinned at its key's sequence, journaled, published.
        let mut body = Body::new(1);
        body.from = Some("s-host".to_string());
        body.dl = (kind == "ask").then_some(30_000);
        body.text = "again".to_string();
        let encoded = body.encode();
        let pin = PublishPin {
            seq: keyed.seq,
            via_key: true,
            identity: Some(publication_identity(&face, &encoded)),
            original: keyed.identity,
            original_off: keyed.original_off,
            owner: keyed.owner,
            at: keyed.at,
        };
        assert_eq!(
            pin.original_off,
            Some(winner),
            "PRECONDITION: the winner's offset"
        );
        bridge.state.set_post_pin("s-host", 2, &pin).unwrap();
        // The entry names its log and head, as every build since read backs journals them.
        let log = bridge.read_log_identity().unwrap();
        assert_eq!(log, Some(broker.log_identity()), "PRECONDITION: its log");
        bridge
            .state
            .set_in_flight(&InFlight {
                pin: pin.clone(),
                subject: face.clone(),
                body: encoded.clone(),
                log,
                at: Some(head_of(&bridge)),
            })
            .unwrap();
        let (own, deduped) = bridge.publish_at(pin.seq, &face, &encoded).unwrap();
        assert!(
            !deduped && own > winner,
            "PRECONDITION: the cut dropped the winner's key, so the re-post appended @{own}"
        );
        queue(2, kind, "again");
        assert!(bridge.settle_in_flight(), "the replay settles the journal");
        bridge.drain_outbox();
        let retired = lane.sent.lock().unwrap()[1].clone();
        assert!(
            retired.contains(&format!("off={own}")) && !retired.contains("dup=1"),
            "{kind}: the re-post is retired at its own copy @{own}, not at its key's winner \
             @{winner}: {retired}"
        );
        let moved = bridge.state.key_pin("s-host", "k").unwrap().unwrap();
        assert_eq!(
            (moved.original_off, moved.owner),
            (Some(own), Some(2)),
            "{kind}: the key pin names the re-post's own copy"
        );
        if kind == "ask" {
            assert_eq!(bridge.asked.get(&own).map(String::as_str), Some("h-a"));
            assert!(
                bridge.deadlines.contains_key(&own),
                "the ask on the bus at @{own} still expires"
            );
        }
    }
}

#[test]
fn a_broadcast_with_no_topics_advances_the_head_without_decoding_its_body() {
    let (lane, mut bridge) = FakeVerbLane::bridge(&[(1, "s-a")]);
    let subject = "/f/f/pub/n-src/s-src/say/quiet".to_string();
    bridge.say_drained_to = 5;
    bridge.say_bus_head = 5;
    let decoded = crate::body::decode_calls();

    bridge.on_say_record(&(5, subject.clone(), vec![b'x'; 1024 * 1024]));
    assert_eq!(crate::body::decode_calls(), decoded);
    assert_eq!((bridge.say_drained_to, bridge.say_bus_head), (6, 6));
    assert_eq!(bridge.say_checkpoint_tick, 1);
    assert!(lane.delivery_offsets().is_empty());

    bridge.per_sid.topics.insert(
        "s-a".to_string(),
        BTreeMap::from([("quiet".to_string(), TopicCursor { next: 6 })]),
    );
    bridge.on_say_record(&(6, subject, Body::new(0).encode()));
    assert_eq!(crate::body::decode_calls(), decoded + 1);
    assert_eq!(lane.delivery_offsets(), [6]);
    assert_eq!(bridge.per_sid.topics["s-a"]["quiet"].next, 7);
}

#[test]
fn a_refused_broadcast_is_read_past_by_every_cursor_at_the_frontier() {
    let (lane, mut bridge) = FakeVerbLane::bridge(&[(1, "s-a"), (2, "s-b")]);
    bridge.say_drained_to = 5;
    bridge.say_bus_head = 5;
    for sid in ["s-a", "s-b"] {
        bridge.per_sid.topics.insert(
            sid.to_string(),
            BTreeMap::from([("news".to_string(), TopicCursor { next: 5 })]),
        );
    }
    let mut relayed = Body::new(0);
    relayed.via = Some("notaprincipal".to_string());
    let refused = [
        (5, "/f/f/pub/n-x/s-1/say/news/x", Body::new(0)),
        (6, "/f/f/pub/n-x/s-1/say/NEWS", Body::new(0)),
        (7, "/f/f/pub/n-x/s-1/say/news", relayed),
    ];
    for (off, subject, body) in refused {
        bridge.on_say_record(&(off, subject.to_string(), body.encode()));
        for sid in ["s-a", "s-b"] {
            assert_eq!(
                bridge.per_sid.topics[sid]["news"].next,
                off + 1,
                "{sid} was left behind by {subject}"
            );
        }
    }
    assert!(lane.delivery_offsets().is_empty());

    let live = "/f/f/pub/n-x/s-1/say/news".to_string();
    bridge.on_say_record(&(8, live, Body::new(0).encode()));
    assert_eq!(lane.transcript(), ["deliver 8", "deliver 8"]);
    for sid in ["s-a", "s-b"] {
        assert_eq!(bridge.per_sid.topics[sid]["news"].next, 9);
    }
}

#[cfg(unix)]
#[test]
fn a_catch_up_page_walks_one_topic_past_every_other_topics_traffic() {
    const SID: &str = "s-late";
    let (lane, mut bridge) = FakeVerbLane::bridge(&[(1, SID)]);
    let socket = lane.dir.path().join("broker.sock");
    let broker = crate::test_broker(lane.dir.path().join("broker.log")).unwrap();
    let _server = broker.serve(&socket).unwrap();
    bridge.cfg.broker = socket.to_string_lossy().into_owned();
    let (conn, _) = bridge.connect().unwrap();
    bridge.conn = Some(conn.into());

    let (mut god, _) = bridge.connect().unwrap();
    let note = Body::new(0).encode();
    let mut publish = |producer: u64, topic: &str, n: usize| -> Vec<u64> {
        let subject = format!("/f/f/pub/n-x/s-p{producer}/say/{topic}");
        let bodies = vec![note.as_slice(); n];
        god.publish_pipelined(producer, &subject, &bodies, 64)
            .unwrap()
            .into_iter()
            .map(|(off, deduped)| {
                assert!(!deduped);
                off
            })
            .collect()
    };
    let noise = publish(1, "noise", 300);
    let a1 = publish(2, "alpha", 1)[0];
    let beta = publish(3, "beta", 300);
    let a2 = publish(4, "alpha", 1)[0];
    let drained = a2 + 1;
    bridge.say_drained_to = drained;
    bridge.say_bus_head = drained;
    bridge.per_sid.topics.insert(
        SID.to_string(),
        BTreeMap::from([
            ("alpha".to_string(), TopicCursor { next: noise[0] }),
            ("beta".to_string(), TopicCursor { next: beta[0] }),
        ]),
    );
    let delivered =
        |offs: &[u64]| -> Vec<String> { offs.iter().map(|off| format!("deliver {off}")).collect() };

    assert!(bridge.catch_up_topic_page(SID));
    assert_eq!(lane.transcript(), delivered(&[a1, a2]));
    assert_eq!(bridge.per_sid.topics[SID]["alpha"].next, drained);
    assert_eq!(bridge.per_sid.topics[SID]["beta"].next, beta[0]);

    assert!(bridge.catch_up_topic_page(SID));
    assert_eq!(bridge.per_sid.topics[SID]["beta"].next, beta[255] + 1);
    assert!(bridge.catch_up_topic_page(SID));
    assert_eq!(bridge.per_sid.topics[SID]["beta"].next, drained);
    let mut want = vec![a1, a2];
    want.extend(&beta);
    assert_eq!(lane.transcript(), delivered(&want));
    assert_eq!(bridge.state.topics(SID), bridge.per_sid.topics[SID]);

    assert!(bridge.catch_up_topic_page(SID));
    assert_eq!(lane.transcript().len(), want.len());
}

#[cfg(unix)]
#[test]
fn a_live_record_does_not_cancel_a_backlog_still_owed() {
    const SID: &str = "s-late";
    const PAGE: usize = 8;
    let (lane, mut bridge) = FakeVerbLane::bridge(&[(1, SID)]);
    bridge.say_replay.page = u32::try_from(PAGE).expect("a small page");
    let socket = lane.dir.path().join("broker.sock");
    let broker = crate::test_broker(lane.dir.path().join("broker.log")).unwrap();
    let _server = broker.serve(&socket).unwrap();
    bridge.cfg.broker = socket.to_string_lossy().into_owned();
    let (conn, _) = bridge.connect().unwrap();
    bridge.conn = Some(conn.into());

    let (mut god, _) = bridge.connect().unwrap();
    let note = Body::new(0).encode();
    let old = "/f/f/pub/n-x/s-old/say/deep";
    let bodies = vec![note.as_slice(); SAY_REPLAY_PAGES * PAGE + 1];
    let backlog: Vec<u64> = god
        .publish_pipelined(1, old, &bodies, 64)
        .unwrap()
        .into_iter()
        .map(|(off, deduped)| {
            assert!(!deduped);
            off
        })
        .collect();
    let drained = backlog[backlog.len() - 1] + 1;
    bridge.say_drained_to = drained;
    bridge.say_bus_head = drained;
    bridge.per_sid.topics.insert(
        SID.to_string(),
        BTreeMap::from([("deep".to_string(), TopicCursor { next: backlog[0] })]),
    );
    let shout = "/f/f/pub/n-y/s-live/say/deep";
    let delivered =
        |offs: &[u64]| -> Vec<String> { offs.iter().map(|off| format!("deliver {off}")).collect() };
    let round = |bridge: &mut Bridge| {
        bridge.say_replay.refill_at = Instant::now();
        bridge.replay_topics(true);
    };

    let (live, _) = god.publish(2, 1, shout, &note).unwrap();
    bridge.on_say_record(&(live, shout.to_string(), note.clone()));
    assert!(
        lane.transcript().is_empty(),
        "a behind cursor is no live recipient"
    );
    assert_eq!(
        bridge.per_sid.topics[SID]["deep"].next, backlog[0],
        "the live record moved a cursor that owes the backlog"
    );
    assert_eq!(bridge.say_drained_to, live + 1);

    round(&mut bridge);
    let first = SAY_REPLAY_PAGES * PAGE;
    assert_eq!(lane.transcript(), delivered(&backlog[..first]));
    assert_eq!(
        bridge.per_sid.topics[SID]["deep"].next,
        backlog[first - 1] + 1
    );

    let (later, _) = god.publish(2, 2, shout, &note).unwrap();
    bridge.on_say_record(&(later, shout.to_string(), note.clone()));
    assert_eq!(lane.transcript().len(), first);
    assert_eq!(
        bridge.per_sid.topics[SID]["deep"].next,
        backlog[first - 1] + 1
    );

    round(&mut bridge);
    let mut want = backlog.clone();
    want.extend([live, later]);
    assert_eq!(lane.transcript(), delivered(&want));
    assert_eq!(bridge.per_sid.topics[SID]["deep"].next, later + 1);
    assert_eq!(bridge.state.topics(SID), bridge.per_sid.topics[SID]);

    let (next, _) = god.publish(2, 3, shout, &note).unwrap();
    bridge.on_say_record(&(next, shout.to_string(), note.clone()));
    want.push(next);
    assert_eq!(lane.transcript(), delivered(&want));
    assert_eq!(bridge.per_sid.topics[SID]["deep"].next, next + 1);
}

#[test]
fn a_remote_node_cannot_speak_for_a_local_session() {
    let (_lane, mut bridge) = FakeVerbLane::bridge(&[(1, "s-mgr")]);
    bridge
        .per_sid
        .epochs
        .insert("s-w".to_string(), "e".to_string());
    let local = bridge.node.clone();
    let addr = |src: &str, kind: WakeClass| subject::InAddr {
        node: local.clone(),
        sid: "s-mgr".to_string(),
        src: src.to_string(),
        kind,
    };
    let mut body = Body::new(0);
    body.from = Some("s-w".to_string());
    assert_eq!(
        bridge.render_from(&addr(&local, WakeClass::Report), &body),
        format!("s-w@{local}")
    );
    assert_eq!(
        bridge.render_from(&addr("n-evil", WakeClass::Report), &body),
        "n-evil"
    );
    body.from = Some("s-far".to_string());
    assert_eq!(
        bridge.render_from(&addr("n-far", WakeClass::Report), &body),
        "s-far@n-far"
    );
    bridge.state.pin("s-far", "n-far").expect("pin");
    assert_eq!(
        bridge.render_from(&addr("n-far", WakeClass::Report), &body),
        "s-far@n-far"
    );
    assert_eq!(
        bridge.render_from(&addr("n-evil", WakeClass::Report), &body),
        "n-evil"
    );
}

/// One cell of the nine kinds x an accepted sender x a relayed record x a stray reply:
/// `(claimed, accepted, relayed, stray)`.
type DemotionCell = (WakeClass, bool, bool, bool);

/// What `classify_delivery` answered in a cell: `(kind, demoted, stray)`.
type Delivered = (WakeClass, Option<WakeClass>, bool);

/// What `bridge` delivers in every [`DemotionCell`]. A stray cell's record answers an ask
/// that went to another node, or is another node's `expired`; its sender is accepted when
/// `--accept-from` names it.
fn delivered_in_every_cell(bridge: &mut Bridge) -> Vec<(DemotionCell, Delivered)> {
    let node = bridge.node.clone();
    bridge.asked.insert(91, "n-asked".to_string());
    let mut cells = Vec::new();
    for claimed in WakeClass::ALL {
        for (accepted, relayed, stray) in (0..8).map(|b| (b & 4 != 0, b & 2 != 0, b & 1 != 0)) {
            let src = match (claimed, stray) {
                (_, true) => "n-far".to_string(),
                (WakeClass::Expired, false) => node.clone(),
                (_, false) => "n-asked".to_string(),
            };
            bridge.cfg.accept_from = if accepted {
                vec![src.clone()]
            } else {
                Vec::new()
            };
            let addr = subject::InAddr {
                node: node.clone(),
                sid: "s-mgr".to_string(),
                src,
                kind: claimed,
            };
            let mut body = Body::new(0);
            body.re = Some(91);
            body.via = relayed.then(|| "s-inner".to_string());
            let delivered = bridge.classify_delivery(&addr, &body);
            cells.push(((claimed, accepted, relayed, stray), delivered));
        }
    }
    cells
}

/// The cells whose delivery differs from astream's sender demotion
/// ([`WakeClass::classify_for_sender`]): its kind, the claimed kind on `demoted=` when the
/// two differ or the record is relayed, and a stray reading for exactly the kinds whose
/// demotion reads one.
fn cells_off_astreams_demotion(cells: &[(DemotionCell, Delivered)]) -> Vec<DemotionCell> {
    cells
        .iter()
        .filter(|&&((claimed, accepted, relayed, stray), delivered)| {
            let reply = claimed.classify_for_sender(true, false, true)
                != claimed.classify_for_sender(true, false, false);
            let kind = claimed.classify_for_sender(accepted, relayed, stray);
            let demoted = (kind != claimed || relayed).then_some(claimed);
            delivered != (kind, demoted, stray && reply)
        })
        .map(|&(cell, _)| cell)
        .collect()
}

/// The relayed cells not delivered as §6.7 says, in aterm's own words rather than read back
/// from astream's function: a relayed record of any kind, from any sender, whatever
/// `--accept-from` says, lands `kind=note` with its claimed kind on `demoted=`.
fn relayed_cells_not_a_note(cells: &[(DemotionCell, Delivered)]) -> Vec<DemotionCell> {
    cells
        .iter()
        .filter(|&&((claimed, _, relayed, _), (kind, demoted, _))| {
            relayed && (kind, demoted) != (WakeClass::Note, Some(claimed))
        })
        .map(|&(cell, _)| cell)
        .collect()
}

/// THE BRIDGE DELIVERS EVERY RECORD AT THE KIND ASTREAM'S SENDER DEMOTION GIVES IT (item
/// E13, astream claim `wire.wake-and-control-acks`): in every cell of the nine kinds x an
/// accepted sender x a relayed record x a stray reply, `classify_delivery` answers
/// [`WakeClass::classify_for_sender`]'s kind; the claimed kind rides `demoted=` when the
/// two differ and on every relayed record (§6.7); and the bridge reads a reply as stray
/// (the asks table, an `expired`'s node) for exactly the kinds whose demotion reads one. A
/// remote node's answer to an ask that went elsewhere is one of the stray cells. The
/// relayed law (§6.7: every relayed record lands `kind=note demoted=<k>`, whatever
/// `--accept-from` says) is held as well in aterm's own words, so an astream pin whose
/// demotion lets a relayed record keep its kind fails here by name, not only in astream.
///
/// NEGATIVE CONTROL: a bridge whose own rule skips the relayed demotion
/// ([`crate::sim::world::Mutant::RelayedKeepsItsKind`]) is told by relayed cells alone,
/// against astream's function and against the relayed law, among them an accepted sender's
/// relayed `control`, delivered as a control.
#[test]
fn classify_delivery_is_astreams_sender_demotion_in_every_cell() {
    let (_lane, mut bridge) = FakeVerbLane::bridge(&[(1, "s-mgr")]);
    let cells = delivered_in_every_cell(&mut bridge);
    assert_eq!(cells.len(), WakeClass::ALL.len() * 8);
    assert_eq!(cells_off_astreams_demotion(&cells), []);
    assert_eq!(relayed_cells_not_a_note(&cells), []);

    bridge.fx = crate::fx::Fx::os_without(crate::sim::world::Mutant::RelayedKeepsItsKind);
    let cells = delivered_in_every_cell(&mut bridge);
    let off = cells_off_astreams_demotion(&cells);
    assert!(
        off.contains(&(WakeClass::Control, true, true, false)),
        "NEGATIVE CONTROL: an accepted sender's relayed control keeps its kind: {off:?}"
    );
    assert!(
        off.iter().all(|&(_, _, relayed, _)| relayed),
        "NEGATIVE CONTROL: only a relayed cell tells the local rule from astream's: {off:?}"
    );
    let unlawful = relayed_cells_not_a_note(&cells);
    assert!(
        unlawful.contains(&(WakeClass::Control, true, true, false)),
        "NEGATIVE CONTROL: the relayed law misses a relayed control kept: {unlawful:?}"
    );
}

#[cfg(unix)]
fn seeded_broker(
    lane: &FakeVerbLane,
    bridge: &mut Bridge,
    sids: &[&str],
    known: &[String],
    held: u64,
) -> (astream_broker::Broker, astream_broker::BrokerHandle) {
    let path = lane.dir.path().join("budget.log");
    {
        let mut log =
            astream_broker::BrokerLog::open_with(&path, astream_broker::Durability::Relaxed)
                .unwrap();
        let mut subjects: Vec<String> = ["presence", "ev", "ack"]
            .iter()
            .map(|face| subject::node_face("f", &bridge.node, face))
            .collect();
        for sid in sids {
            for face in ["presence", "ev"] {
                subjects.push(format!("/f/f/pub/{}/{sid}/{face}", bridge.node));
            }
        }
        subjects.extend(known.iter().cloned());
        let mut filler = 0;
        while (subjects.len() as u64) < held {
            subjects.push(format!("/f/f/pub/{}/node/filler/{filler}", bridge.node));
            filler += 1;
        }
        for (seq, s) in subjects.into_iter().enumerate() {
            log.publish(bridge.producer_id, seq as u64 + 1, s, b"v=1".to_vec())
                .unwrap();
        }
    }
    let give_up = std::time::Instant::now() + aterm_spec::wait::MUST_HAPPEN;
    let broker = loop {
        match crate::test_broker(&path) {
            Ok(broker) => break broker,
            Err(e)
                if e.kind() == std::io::ErrorKind::AlreadyExists
                    && std::time::Instant::now() < give_up =>
            {
                std::thread::sleep(std::time::Duration::from_millis(10));
            }
            Err(e) => panic!("the seeded log did not open: {e}"),
        }
    };
    let socket = lane.dir.path().join("budget.sock");
    let server = broker.serve(&socket).unwrap();
    bridge.cfg.broker = socket.to_string_lossy().into_owned();
    assert!(
        bridge.attach_broker(),
        "attached without spending a subject"
    );
    (broker, server)
}

#[cfg(unix)]
#[test]
fn a_new_lane_past_the_subject_budget_is_retired_and_the_queue_behind_it_lands() {
    let (lane, mut bridge) = FakeVerbLane::bridge(&[(1, "s-a"), (2, "s-b")]);
    assert!(bridge.refresh_sessions().is_ok());
    let task_lane = format!("/f/f/in/{0}/s-b/{0}/task", bridge.node);
    let ask_lane = format!("/f/f/in/{0}/s-b/{0}/ask", bridge.node);
    let max = astream_broker::store::MAX_SUBJECTS_PER_PRODUCER as u64;
    let (_broker, _server) = seeded_broker(&lane, &mut bridge, &["s-a", "s-b"], &[task_lane], max);
    bridge.indexed_subjects.insert(ask_lane.clone());
    *lane.outbox.lock().unwrap() = Some(
        "post sid=s-a id=1 to=@s-b kind=note len=5\nfirst\
         post sid=s-a id=2 to=@s-b kind=ask len=5\nstale\
         post sid=s-a id=3 to=@s-b kind=task len=6\nsecond\
         receipt sid=s-b rid=1 off=3 verdict=handled kind=ask from=s-a\n"
            .to_string(),
    );
    bridge.drain_outbox();
    let sent = lane.sent.lock().unwrap().clone();
    for refused in [1, 2] {
        assert!(
            sent.contains(&format!(
                "outbox sent s-a {refused} off=- reason=subject-budget"
            )),
            "post {refused}: {sent:#?}"
        );
    }
    assert!(
        !bridge.indexed_subjects.contains(&ask_lane),
        "a subject the broker refused as new is not believed indexed"
    );
    assert!(
        sent.iter()
            .any(|l| l.starts_with("outbox sent s-a 3 off=") && !l.contains("off=-")),
        "the task behind the refused posts did not land: {sent:#?}"
    );
    let (mut reader, _) = bridge.connect().unwrap();
    let ev_of = |reader: &mut Conn, face: String| -> Vec<String> {
        let (rows, _) = reader.fetch(0, &face, 4096).unwrap();
        rows.iter()
            .map(|(_, _, b)| crate::pct::decode(&String::from_utf8_lossy(b)))
            .collect()
    };
    let node_ev = ev_of(&mut reader, subject::node_face("f", &bridge.node, "ev"));
    assert!(
        node_ev
            .iter()
            .any(|e| e.contains("ev=undeliverable to=@s-b reason=subject-budget")),
        "{node_ev:#?}"
    );
    let b_ev = ev_of(&mut reader, format!("/f/f/pub/{}/s-b/ev", bridge.node));
    assert!(
        b_ev.iter()
            .any(|e| e.contains("unacked re=3 verdict=handled reason=subject-budget")),
        "{b_ev:#?}"
    );
}

#[cfg(unix)]
#[test]
fn a_broadcast_opens_a_subject_only_above_the_reserve() {
    let (lane, mut bridge) = FakeVerbLane::bridge(&[(1, "s-a"), (2, "s-b")]);
    assert!(bridge.refresh_sessions().is_ok());
    let known = format!("/f/f/pub/{}/s-a/say/known", bridge.node);
    let max = astream_broker::store::MAX_SUBJECTS_PER_PRODUCER as u64;
    let held = max - max / SAY_RESERVE_DIVISOR;
    let (_broker, _server) = seeded_broker(
        &lane,
        &mut bridge,
        &["s-a", "s-b"],
        std::slice::from_ref(&known),
        held,
    );
    *lane.outbox.lock().unwrap() = Some(
        "post sid=s-a id=1 to=say:fresh kind=note len=3\nnew\
         post sid=s-a id=2 to=say:known kind=note len=3\nold\
         post sid=s-b id=1 to=@s-a kind=note len=4\nmail"
            .to_string(),
    );
    bridge.drain_outbox();
    let sent = lane.sent.lock().unwrap().clone();
    assert!(
        sent.contains(&"outbox sent s-a 1 off=- reason=subject-budget".to_string()),
        "{sent:#?}"
    );
    for landed in ["outbox sent s-a 2 off=", "outbox sent s-b 1 off="] {
        assert!(
            sent.iter()
                .any(|l| l.starts_with(landed) && !l.contains("off=-")),
            "{landed}: {sent:#?}"
        );
    }
    let (mut reader, _) = bridge.connect().unwrap();
    let fresh = format!("/f/f/pub/{}/s-a/say/fresh", bridge.node);
    assert!(
        reader.fetch(0, &fresh, 8).unwrap().0.is_empty(),
        "nothing spent"
    );
    let budget = reader
        .subject_budget(bridge.producer_id)
        .unwrap()
        .expect("a broker that reads budgets");
    assert_eq!(budget.subjects, held + 1, "only the directed lane opened");
}

/// A drain of `s-a`'s notes to each of `to` on `lane`, their post ids from `first` on: what
/// came of each, in order (`true` landed, `false` retired at the subject budget's door),
/// and the `subject_budget` requests the drain made.
#[cfg(unix)]
fn budget_drain(
    lane: &FakeVerbLane,
    bridge: &mut Bridge,
    to: &[&str],
    first: u64,
) -> (Vec<bool>, u64) {
    let (sent_before, reads_before) = (lane.sent.lock().unwrap().len(), bridge.budget_queries);
    let listing: String = to
        .iter()
        .zip(first..)
        .map(|(sid, id)| format!("post sid=s-a id={id} to=@{sid} kind=note len=1\nn"))
        .collect();
    *lane.outbox.lock().unwrap() = Some(listing);
    bridge.drain_outbox();
    let sent = lane.sent.lock().unwrap()[sent_before..].to_vec();
    let landed = (first..first + to.len() as u64)
        .map(|id| {
            let head = format!("outbox sent s-a {id} off=");
            let line = sent
                .iter()
                .find(|l| l.starts_with(&head))
                .unwrap_or_else(|| panic!("post {id} was not concluded: {sent:#?}"));
            assert!(
                !line.contains("off=-") || line.ends_with(" reason=subject-budget"),
                "post {id} went for another reason: {line}"
            );
            !line.contains("off=-")
        })
        .collect();
    (landed, bridge.budget_queries - reads_before)
}

/// The fake lane and its bridge with five sessions, on a broker whose log charges `held`
/// subjects to the node, under every rule but `off`.
#[cfg(unix)]
fn budget_world(
    off: &[crate::sim::world::Mutant],
    held: u64,
) -> (
    FakeVerbLane,
    Bridge,
    astream_broker::Broker,
    astream_broker::BrokerHandle,
) {
    const SIDS: [&str; 5] = ["s-a", "s-b", "s-c", "s-d", "s-e"];
    let (lane, mut bridge) =
        FakeVerbLane::bridge(&[(1, "s-a"), (2, "s-b"), (3, "s-c"), (4, "s-d"), (5, "s-e")]);
    bridge.fx = crate::fx::Fx::os_without_each(off);
    assert!(bridge.refresh_sessions().is_ok());
    let (broker, server) = seeded_broker(&lane, &mut bridge, &SIDS, &[], held);
    (lane, bridge, broker, server)
}

/// The subjects a directed note may still open before it would take the node's budget into
/// the lanes' reserve, plus `room`: the `held` [`budget_world`] takes for that.
#[cfg(unix)]
fn held_leaving(room: u64) -> u64 {
    let max = astream_broker::store::MAX_SUBJECTS_PER_PRODUCER as u64;
    max - Opens::Lane.reserve(max) - room
}

/// **A DRAIN READS THE SUBJECT BUDGET ONCE FOR EVERY FRESH LANE IT OPENS** (critic item G04):
/// an attach's first drain over K lanes no reader has indexed makes one `subject_budget`
/// request, not K, and every lane opens. The wait tally counts it in its own row
/// (`budget_request`), apart from every other request that reads its own reply.
///
/// NEGATIVE CONTROL ([`crate::sim::world::Mutant::BudgetReadPerSubject`], the bridge before
/// G04): K requests, one a lane.
#[cfg(unix)]
#[test]
fn a_drain_reads_the_subject_budget_once_for_every_fresh_lane_it_opens() {
    use crate::sim::world::Mutant;
    use crate::wait_tally::{Row, Tally, FILE};
    let lanes = ["s-b", "s-c", "s-d", "s-e"];
    for (off, reads) in [
        (&[][..], 1),
        (&[Mutant::BudgetReadPerSubject][..], lanes.len() as u64),
    ] {
        let (lane, mut bridge, _broker, _server) = budget_world(off, 0);
        let tally = bridge.state.root().join(FILE);
        bridge.tally_to(&tally);
        let (landed, asked) = budget_drain(&lane, &mut bridge, &lanes, 1);
        assert_eq!(landed, [true; 4], "{off:?}: every fresh lane opens");
        assert_eq!(asked, reads, "{off:?}: the budget reads of one drain");
        bridge.tally_idle();
        let t = Tally::parse(&std::fs::read_to_string(&tally).unwrap()).unwrap();
        assert_eq!(
            t.row(Row::BudgetRequest).count,
            reads,
            "{off:?}: the tally counts each read in its own row"
        );
    }
}

/// **THE COUNT SINCE THE READING REFUSES WHERE THE PER-SUBJECT READ WOULD** (critic item G04),
/// for a budget one short: with room for two more lanes above the lanes' reserve, a drain
/// of three fresh lanes opens the first two on its one reading, and the third, which the
/// count says would take the budget into the reserve, is judged by a fresh reading and the
/// lane's own `last`, and retired at the door, as the per-subject read retires it.
///
/// NEGATIVE CONTROL ([`crate::sim::world::Mutant::BudgetCountTakenAsUnlimited`]): the third
/// lane opens a subject inside the reserve.
#[cfg(unix)]
#[test]
fn the_count_since_the_reading_refuses_where_the_per_subject_read_would() {
    use crate::sim::world::Mutant;
    let lanes = ["s-b", "s-c", "s-d"];
    let drain = |off: &[Mutant]| {
        let (lane, mut bridge, _broker, _server) = budget_world(off, held_leaving(2));
        budget_drain(&lane, &mut bridge, &lanes, 1)
    };
    let (per_subject, _) = drain(&[Mutant::BudgetReadPerSubject]);
    assert_eq!(
        per_subject,
        [true, true, false],
        "PRECONDITION: the per-subject read opens two and refuses the third"
    );
    assert_eq!(
        drain(&[]),
        (per_subject, 2),
        "the count opens the same two, and the third is read again and refused"
    );
    assert_eq!(
        drain(&[Mutant::BudgetCountTakenAsUnlimited]).0,
        [true, true, true],
        "NEGATIVE CONTROL: the count taken as unlimited opens the reserve's subject"
    );
}

/// **EACH DRAIN READS THE BUDGET AFRESH: WHAT ANOTHER WRITER SPENT OR A RETIREMENT FREED**
/// (critic item G04). A drain opens one lane, leaving room for one more above the lanes'
/// reserve; another connection under this producer (as a handoff's successor or `aterm
/// link`'s own writes are) opens a subject; the next drain's fresh lane would take the
/// budget into the reserve, and its own reading refuses it. A retirement then frees a
/// subject, and the next drain's reading lets the lane open.
///
/// NEGATIVE CONTROL ([`crate::sim::world::Mutant::BudgetReadOutlivesItsScope`]): the first
/// drain's reading, less what this bridge sent since, is reused, and the second drain's lane
/// opens a subject inside the reserve.
#[cfg(unix)]
#[test]
fn each_drain_reads_the_budget_afresh_what_another_writer_spent_or_a_retirement_freed() {
    use crate::sim::world::Mutant;
    for off in [&[][..], &[Mutant::BudgetReadOutlivesItsScope][..]] {
        let reused = !off.is_empty();
        let (lane, mut bridge, _broker, _server) = budget_world(off, held_leaving(2));
        let (landed, asked) = budget_drain(&lane, &mut bridge, &["s-b"], 1);
        assert_eq!((landed, asked), (vec![true], 1), "{off:?}: the first drain");
        let (mut other, _) = bridge.connect().unwrap();
        let spent = format!("/f/f/pub/{}/node/another-writer", bridge.node);
        other
            .publish(bridge.producer_id, 100_000, &spent, b"v=1")
            .expect("another writer under this producer opens a subject");
        let (landed, _) = budget_drain(&lane, &mut bridge, &["s-c"], 2);
        if reused {
            assert_eq!(
                landed,
                [true],
                "NEGATIVE CONTROL: the reused reading opens the reserve's subject"
            );
            continue;
        }
        assert_eq!(
            landed,
            [false],
            "the second drain reads what the other writer spent, and refuses the lane"
        );
        other
            .retire_subject(bridge.producer_id, &spent)
            .expect("the other writer's subject retired");
        let (landed, asked) = budget_drain(&lane, &mut bridge, &["s-c"], 3);
        assert_eq!(
            (landed, asked),
            (vec![true], 1),
            "the third drain reads what the retirement freed, and the lane opens"
        );
    }
}

/// **A BUDGET FREED INSIDE A DRAIN IS READ WHERE THE COUNT SAYS SHORT** (critic item G04). A
/// drain starts with room for one lane above the lanes' reserve and opens it; a retirement
/// frees a subject before the drain's second fresh lane is judged. The count since the
/// drain's reading cannot see it and says short, so the lane is read for: the fresh
/// reading lets it open, as the per-subject read does.
///
/// NEGATIVE CONTROL ([`crate::sim::world::Mutant::BudgetShortOnTheCount`]): the count's
/// "short" refuses the lane unread.
#[cfg(unix)]
#[test]
fn a_budget_freed_inside_a_drain_is_read_where_the_count_says_short() {
    use crate::sim::world::Mutant;
    let drain = |off: &[Mutant]| {
        let (lane, mut bridge, _broker, _server) = budget_world(off, held_leaving(1));
        let (mut other, _) = bridge.connect().unwrap();
        let (producer, filler) = (
            bridge.producer_id,
            format!("/f/f/pub/{}/node/filler/0", bridge.node),
        );
        lane.on_first_retire(move || {
            other
                .retire_subject(producer, &filler)
                .expect("a filler retired as the first post is concluded");
        });
        budget_drain(&lane, &mut bridge, &["s-b", "s-c"], 1)
    };
    let (per_subject, _) = drain(&[Mutant::BudgetReadPerSubject]);
    assert_eq!(
        per_subject,
        [true, true],
        "PRECONDITION: read for, the second lane has the freed room"
    );
    assert_eq!(
        drain(&[]),
        (per_subject, 2),
        "the count says short, the fresh reading lets the lane open"
    );
    assert_eq!(
        drain(&[Mutant::BudgetShortOnTheCount]).0,
        [true, false],
        "NEGATIVE CONTROL: refused on the count alone"
    );
}

#[cfg(unix)]
#[test]
fn a_new_post_id_under_a_winning_key_dedups_before_the_subject_budget_door() {
    let (lane, mut bridge) = FakeVerbLane::bridge(&[(1, "s-a"), (2, "s-b")]);
    bridge.refresh_sessions().unwrap();
    let note = format!("/f/f/in/{0}/s-b/{0}/note", bridge.node);
    let ask = format!("/f/f/in/{0}/s-b/{0}/ask", bridge.node);
    let max = astream_broker::store::MAX_SUBJECTS_PER_PRODUCER as u64;
    let (_broker, _server) = seeded_broker(
        &lane,
        &mut bridge,
        &["s-a", "s-b"],
        std::slice::from_ref(&note),
        max,
    );
    *lane.outbox.lock().unwrap() =
        Some("post sid=s-a id=1 to=@s-b kind=note key=k-first len=8\noriginal".into());
    bridge.drain_outbox();
    let winner = bridge.state.key_pin("s-a", "k-first").unwrap().unwrap();
    let off = winner.original_off.unwrap();
    assert!(lane
        .sent
        .lock()
        .unwrap()
        .contains(&format!("outbox sent s-a 1 off={off}")));

    *lane.outbox.lock().unwrap() = Some(
        "post sid=s-a id=2 to=@s-b kind=ask key=k-first dl=30000 len=5\nlater\
         post sid=s-a id=3 to=@s-b kind=ask len=5\nfresh"
            .into(),
    );
    bridge.drain_outbox();
    let sent = lane.sent.lock().unwrap().clone();
    assert!(
        sent.contains(&format!("outbox sent s-a 2 off={off} dup=1")),
        "the winning key was judged as a new subject: {sent:#?}"
    );
    assert!(sent.contains(&"outbox sent s-a 3 off=- reason=subject-budget".into()));
    assert!(bridge.asked.is_empty() && bridge.deadlines.is_empty());
    let kept = bridge.state.key_pin("s-a", "k-first").unwrap().unwrap();
    assert_eq!(
        (kept.seq, kept.owner, kept.original_off),
        (winner.seq, Some(1), Some(off))
    );
    let (mut reader, _) = bridge.connect().unwrap();
    assert!(reader.fetch(0, &ask, 8).unwrap().0.is_empty());
    let notes = reader.fetch(0, &note, 8).unwrap().0;
    assert_eq!(
        notes.len(),
        2,
        "one seeded row and the original, no duplicate"
    );
    assert_eq!(notes.last().unwrap().0, off);
    assert_eq!(
        reader
            .subject_budget(bridge.producer_id)
            .unwrap()
            .unwrap()
            .subjects,
        max
    );
}

#[cfg(unix)]
#[test]
fn unknown_post_or_key_reservation_state_cannot_retire_at_the_budget_door() {
    for (blocked, dangling) in [
        (crate::state::journal::FILE, false),
        ("keys/s-a", false),
        (crate::state::journal::FILE, true),
        ("keys/s-a", true),
    ] {
        let (lane, mut bridge) = FakeVerbLane::bridge(&[(1, "s-a"), (2, "s-b")]);
        bridge.refresh_sessions().unwrap();
        let max = astream_broker::store::MAX_SUBJECTS_PER_PRODUCER as u64;
        let (_broker, _server) = seeded_broker(&lane, &mut bridge, &["s-a", "s-b"], &[], max);
        let root = bridge.state.root().to_path_buf();
        let path = root.join(blocked);
        std::fs::create_dir_all(path.parent().expect("reservation parent")).unwrap();
        let _ = std::fs::remove_file(&path);
        if dangling {
            std::os::unix::fs::symlink("absent-reservation-target", &path)
                .expect("make the existing reservation target unreadable");
        } else {
            std::fs::create_dir(&path).expect("make the required reservation read fail");
        }
        bridge.state = StateDir::open(&root).unwrap();
        *lane.outbox.lock().unwrap() =
            Some("post sid=s-a id=1 to=@s-b kind=ask key=k-first len=5\nfresh".into());
        bridge.drain_outbox();
        let retired = || {
            lane.sent
                .lock()
                .unwrap()
                .iter()
                .any(|s| s.starts_with("outbox sent s-a 1 "))
        };
        assert!(
            !retired(),
            "unreadable {blocked} authorized a terminal retirement"
        );
        let metadata = std::fs::symlink_metadata(&path).expect("preserved witness");
        assert_eq!(metadata.file_type().is_symlink(), dangling);

        if dangling {
            std::fs::remove_file(&path).unwrap();
        } else {
            std::fs::remove_dir(&path).unwrap();
        }
        bridge.drain_outbox();
        assert!(lane
            .sent
            .lock()
            .unwrap()
            .contains(&"outbox sent s-a 1 off=- reason=subject-budget".into()));
        let (mut reader, _) = bridge.connect().unwrap();
        let ask = format!("/f/f/in/{0}/s-b/{0}/ask", bridge.node);
        assert!(reader.fetch(0, &ask, 8).unwrap().0.is_empty());
    }
}

#[cfg(unix)]
#[test]
fn directed_mail_opens_a_lane_only_to_a_node_the_fleet_has_shown_and_above_its_reserve() {
    let (lane, mut bridge) = FakeVerbLane::bridge(&[(1, "s-a")]);
    assert!(bridge.refresh_sessions().is_ok());
    let node = bridge.node.clone();
    let far = subject::node_face("f", "n-far", "presence");
    let known = format!("/f/f/in/n-far/s-x/{node}/note");
    let max = astream_broker::store::MAX_SUBJECTS_PER_PRODUCER as u64;
    let held = max - max / LANE_RESERVE_DIVISOR - 1;
    let (_broker, _server) = seeded_broker(&lane, &mut bridge, &["s-a"], &[far, known], held);
    // n-far advertises the sessions mailed there; its rows are its producer's, not this node's.
    let (mut far_node, _) = bridge.connect().unwrap();
    for (seq, sid) in (1..).zip(["s-x", "s-y", "s-z"]) {
        far_node
            .publish(
                9_000,
                seq,
                &subject::session_face("f", "n-far", sid, "presence"),
                b"v=1 t=1 inc=1 state=live",
            )
            .unwrap();
    }
    *lane.outbox.lock().unwrap() = Some(
        "post sid=s-a id=1 to=@s-x@n-nowhere kind=note len=1\n1\
         post sid=s-a id=2 to=@s-y@n-far kind=note len=1\n2\
         post sid=s-a id=3 to=@s-z@n-far kind=note len=1\n3\
         post sid=s-a id=4 to=h-made-up kind=note len=1\n4\
         post sid=s-a id=5 to=@s-x@n-far kind=note len=1\n5"
            .to_string(),
    );
    bridge.drain_outbox();
    let sent = lane.sent.lock().unwrap().clone();
    let retired = |id: u64, why: &str| {
        assert!(
            sent.contains(&format!("outbox sent s-a {id} off=- reason={why}")),
            "post {id} is retired {why}: {sent:#?}"
        );
    };
    let landed = |id: u64| {
        assert!(
            sent.iter()
                .any(|l| l.starts_with(&format!("outbox sent s-a {id} off="))
                    && !l.contains("off=-")),
            "post {id} landed: {sent:#?}"
        );
    };
    retired(1, "unroutable");
    landed(2);
    retired(3, SUBJECT_BUDGET);
    retired(4, SUBJECT_BUDGET);
    landed(5);
    let (mut reader, _) = bridge.connect().unwrap();
    for unminted in [
        format!("/f/f/in/n-nowhere/s-x/{node}/note"),
        format!("/f/f/in/n-far/s-z/{node}/note"),
        format!("/f/f/in/p/h-made-up/{node}/note"),
    ] {
        assert!(
            reader.fetch(0, &unminted, 8).unwrap().0.is_empty(),
            "{unminted} was minted"
        );
    }
    let node_ev: Vec<String> = reader
        .fetch(0, &subject::node_face("f", &node, "ev"), 4096)
        .unwrap()
        .0
        .iter()
        .map(|(_, _, b)| crate::pct::decode(&String::from_utf8_lossy(b)))
        .collect();
    assert!(
        node_ev
            .iter()
            .any(|e| e.contains("ev=undeliverable to=@s-x@n-nowhere reason=unroutable")),
        "the refusal is said where the node's operator reads: {node_ev:#?}"
    );
    let budget = reader
        .subject_budget(bridge.producer_id)
        .unwrap()
        .expect("a broker that reads budgets");
    assert_eq!(budget.subjects, held + 1, "only @s-y@n-far's lane opened");
    assert_eq!(
        budget.remaining(),
        max / LANE_RESERVE_DIVISOR,
        "the faces' reserve is whole"
    );
}

#[cfg(unix)]
#[test]
fn a_strangers_mail_spends_a_bounded_share_of_this_nodes_budget() {
    const SID: &str = "s-host";
    let (lane, mut bridge) = FakeVerbLane::bridge(&[(1, SID)]);
    let socket = lane.dir.path().join("broker.sock");
    let broker = crate::test_broker(lane.dir.path().join("broker.log")).unwrap();
    let _server = broker.serve(&socket).unwrap();
    bridge.cfg.broker = socket.to_string_lossy().into_owned();
    let (conn, _) = bridge.connect().unwrap();
    bridge.conn = Some(conn.into());
    assert!(bridge.refresh_sessions().is_ok());
    let node = bridge.node.clone();

    for (off, ghost) in [(10, "s-ghost1"), (11, "s-ghost2")] {
        let subject = format!("/f/f/in/{node}/{ghost}/n-evil/note");
        let outcome = bridge.deliver_record(off, &subject, b"v=1 t=1 text=x", None);
        assert_eq!(outcome, Delivery::Accounted);
    }
    let known = format!("/f/f/in/n-evil/s-known/{node}");
    bridge
        .publish(&format!("{known}/note"), b"v=1 t=1 text=hi")
        .unwrap();
    let refuse = |bridge: &mut Bridge, off: u64, from: &str| {
        let subject = format!("/f/f/in/{node}/{SID}/n-evil/note");
        let raw = format!("v=1 t=1 from={from} via=NOT-A-PRINCIPAL text=x");
        let outcome = bridge.deliver_record(off, &subject, raw.as_bytes(), None);
        assert_eq!(outcome, Delivery::Accounted);
    };
    refuse(&mut bridge, 20, "s-first");
    for i in 1..NEW_LANES_PER_NODE {
        let fill = format!("/f/f/in/n-evil/s-fill{i}/{node}");
        assert!(bridge.may_answer_on("n-evil", &fill).unwrap(), "{fill}");
    }
    refuse(&mut bridge, 21, "s-x");
    refuse(&mut bridge, 22, "s-known");
    for (rid, from) in [(1, "s-y@n-evil"), (2, "s-first@n-evil"), (3, "s-z@n-other")] {
        let owed = OwedReceipt {
            sid: SID.to_string(),
            rid,
            off: 30 + rid,
            verdict: "handled".to_string(),
            kind: "ask".to_string(),
            from: from.to_string(),
            earlier_log: false,
        };
        bridge.send_receipt(&owed).unwrap();
    }
    for i in 0..=NEW_LANES_PER_NODE {
        let mine = format!("/f/f/in/{node}/s-local{i}/{node}");
        assert!(bridge.may_answer_on(&node, &mine).unwrap(), "{mine}");
    }

    let (mut reader, _) = bridge.connect().unwrap();
    let (rows, _) = reader.last_all("/f/f/>").unwrap();
    let subjects: Vec<&str> = rows.iter().map(|(_, s, _)| s.as_str()).collect();
    assert!(
        !subjects.iter().any(|s| s.contains("s-ghost")),
        "a sid this node does not host became a subject: {subjects:#?}"
    );
    let lanes_on = |peer: &str| -> BTreeSet<&str> {
        let prefix = format!("/f/f/in/{peer}/");
        subjects
            .iter()
            .copied()
            .filter(|s| s.starts_with(&prefix))
            .collect()
    };
    let first = format!("/f/f/in/n-evil/s-first/{node}");
    assert_eq!(
        lanes_on("n-evil"),
        BTreeSet::from([
            format!("{first}/undeliverable").as_str(),
            format!("{first}/ack").as_str(),
            format!("{known}/note").as_str(),
            format!("{known}/undeliverable").as_str(),
        ]),
        "the first contact and the held lane, and no lane past the bound"
    );
    assert_eq!(
        lanes_on("n-other"),
        BTreeSet::from([format!("/f/f/in/n-other/s-z/{node}/ack").as_str()]),
        "another node's first contact draws on its own allowance"
    );

    let ev_of = |face: &str, reader: &mut Conn| -> Vec<String> {
        let (rows, _) = reader.fetch(0, face, 256).unwrap();
        rows.iter()
            .filter(|(_, s, _)| s == face)
            .filter_map(|(_, _, raw)| {
                let ev = Body::decode(raw).0.unknown.get("ev")?.clone();
                Some(crate::pct::decode(&ev))
            })
            .collect()
    };
    let node_ev = ev_of(&subject::node_face("f", &node, "ev"), &mut reader);
    for (off, ghost) in [(10, "s-ghost1"), (11, "s-ghost2")] {
        let want = format!("undeliverable off={off} reason=not-hosted sid={ghost}");
        assert!(node_ev.contains(&want), "{want} in {node_ev:#?}");
    }
    let host_ev = ev_of(&subject::session_face("f", &node, SID, "ev"), &mut reader);
    for want in [
        "unnotified off=21 src=n-evil reason=lane-cap",
        "unacked re=31 verdict=handled reason=lane-cap",
    ] {
        assert!(
            host_ev.iter().any(|e| e == want),
            "past the bound the answer is recorded unsent: {want} in {host_ev:#?}"
        );
    }
}

#[cfg(unix)]
#[test]
fn a_failed_lane_read_keeps_the_receipt_owed_and_sends_no_verdict() {
    const SID: &str = "s-host";
    let (lane, mut bridge) = FakeVerbLane::bridge(&[(1, SID)]);
    let socket = lane.dir.path().join("broker.sock");
    let broker = crate::test_broker(lane.dir.path().join("broker.log")).unwrap();
    let _server = broker.serve(&socket).unwrap();
    bridge.cfg.broker = socket.to_string_lossy().into_owned();
    let owed = OwedReceipt {
        sid: SID.to_string(),
        rid: 1,
        off: 31,
        verdict: "handled".to_string(),
        kind: "ask".to_string(),
        from: "s-y@n-evil".to_string(),
        earlier_log: false,
    };
    let addr = subject::InAddr {
        node: bridge.node.clone(),
        sid: SID.to_string(),
        src: "n-evil".to_string(),
        kind: WakeClass::Note,
    };
    let mut body = Body::new(0);
    body.from = Some("s-y".to_string());

    let (conn, closer) = bridge.connect().unwrap();
    bridge.conn = Some(conn.into());
    closer.close();
    assert!(
        bridge.send_receipt(&owed).is_err(),
        "the receipt stays owed"
    );
    assert!(bridge.conn.is_none(), "a failed read discards the socket");
    let (conn, closer) = bridge.connect().unwrap();
    bridge.conn = Some(conn.into());
    closer.close();
    assert_eq!(
        bridge.sender_lane(&addr, &body),
        Err(NoLane::Undrained),
        "no verdict, and no claim that the cap refused it"
    );

    let far = OwedReceipt {
        rid: 2,
        from: "s-far".to_string(),
        ..owed.clone()
    };
    let (conn, closer) = bridge.connect().unwrap();
    bridge.conn = Some(conn.into());
    closer.close();
    assert!(
        bridge.send_receipt(&far).is_err(),
        "a bare sender the roster could not place stays owed"
    );
    assert!(
        bridge.resolve_to("@s-far", Some(SID)).is_err(),
        "no connection at all is no answer either"
    );
    assert!(!bridge.state.pins().contains_key("s-far"));
    let (conn, _) = bridge.connect().unwrap();
    bridge.conn = Some(conn.into());
    assert_eq!(
        bridge.resolve_to("@s-far", Some(SID)).unwrap(),
        Route::Unroutable
    );
    assert!(!bridge.state.pins().contains_key("s-far"));

    bridge.send_receipt(&owed).unwrap();
    assert_eq!(lane.delivery_offsets().len(), 1, "retired with its offset");
}

#[cfg(unix)]
#[test]
fn the_routing_read_folds_in_the_records_its_walk_omitted() {
    use astream_broker::proto::{decode_request, encode_response, read_frame, write_frame};
    use astream_broker::{Request, Response};
    let scripted = |fetch_next: u64| -> Conn {
        let (ours, theirs) = std::os::unix::net::UnixStream::pair().unwrap();
        std::thread::spawn(move || {
            let mut s = theirs;
            let row = |offset: u64, node: &str, state: &str| Response::Delivery {
                offset,
                subject: format!("/f/f/pub/{node}/s-x/presence"),
                body: format!("v=1 t=1 state={state}").into_bytes(),
            };
            while let Ok(Some(frame)) = read_frame(&mut s) {
                let (rows, next, head) = match decode_request(&frame) {
                    Some(Request::Last { .. }) => {
                        (vec![row(3, "n-a", "live"), row(5, "n-b", "live")], 10, 10)
                    }
                    Some(Request::Fetch {
                        from_offset: 10, ..
                    }) => (
                        vec![row(11, "n-b", "exited"), row(12, "n-c", "live")],
                        fetch_next,
                        13,
                    ),
                    _ => return,
                };
                for r in rows.iter().chain([&Response::Mark {
                    next,
                    head,
                    resume: String::new(),
                    durable: None,
                    oldest: None,
                }]) {
                    if write_frame(&mut s, &encode_response(r)).is_err() {
                        return;
                    }
                }
            }
        });
        Conn::from_stream(Box::new(ours))
    };
    let (_lane, mut bridge) = FakeVerbLane::bridge(&[]);
    bridge.conn = Some(scripted(13).into());
    let live = bridge.advertisers("s-x").unwrap();
    assert_eq!(
        live,
        BTreeSet::from(["n-a".to_string(), "n-c".to_string()]),
        "n-b's `exited` row past the walk is no advertiser"
    );
    bridge.conn = Some(scripted(12).into());
    assert!(
        bridge.advertisers("s-x").is_err(),
        "a stretch read short of its head is unknown"
    );
}

#[cfg(unix)]
#[test]
fn a_presence_walk_services_a_halt_between_pages_and_stops_on_disconnect() {
    use astream_broker::proto::{decode_request, encode_response, read_frame, write_frame};
    use astream_broker::{Request, Response};
    const SID: &str = "s-local";
    for closed in [false, true] {
        let (lane, mut bridge) = FakeVerbLane::bridge(&[(1, SID)]);
        bridge.refresh_sessions().unwrap();
        let mailbox = Arc::clone(&bridge.mailbox);
        let generation = mailbox.broker_generation(Source::Fleet);
        let transcript = Arc::clone(&lane.transcript);
        let (ours, mut peer) = std::os::unix::net::UnixStream::pair().unwrap();
        for socket in [&ours, &peer] {
            socket
                .set_read_timeout(Some(aterm_spec::wait::MUST_HAPPEN))
                .unwrap();
            socket
                .set_write_timeout(Some(aterm_spec::wait::MUST_HAPPEN))
                .unwrap();
        }
        let server = std::thread::spawn(move || {
            let row = |offset, node: &str, state| Response::Delivery {
                offset,
                subject: format!("/f/f/pub/{node}/s-x/presence"),
                body: format!("v=1 t=1 state={state}").into_bytes(),
            };
            while let Ok(Some(frame)) = read_frame(&mut peer) {
                let responses = match decode_request(&frame).unwrap() {
                    Request::Last { after, .. } => {
                        transcript.lock().unwrap().push(format!("last {after}"));
                        if after.is_empty() {
                            let _ = mailbox.push(
                                Source::Fleet,
                                (
                                    7,
                                    "/f/f/fleet/h-t/halt".into(),
                                    b"v=1 t=1 state=on reason=stop".to_vec(),
                                ),
                                None,
                                generation,
                                Instant::now(),
                            );
                            if closed {
                                let inbox = mailbox.broker_generation(Source::Inbox);
                                mailbox.push_lost(Source::Inbox, inbox, End::Eof);
                            }
                            vec![Response::Mark {
                                next: 10,
                                head: 10,
                                resume: "cursor".into(),
                                durable: None,
                                oldest: None,
                            }]
                        } else {
                            assert_eq!(after, "cursor");
                            vec![
                                row(3, "n-a", "live"),
                                row(12, "n-b", "live"),
                                Response::Mark {
                                    next: 20,
                                    head: 20,
                                    resume: String::new(),
                                    durable: None,
                                    oldest: None,
                                },
                            ]
                        }
                    }
                    Request::Fetch { max: 0, .. } if closed => vec![Response::Mark {
                        next: 10,
                        head: 10,
                        resume: String::new(),
                        durable: None,
                        oldest: None,
                    }],
                    Request::Fetch { from_offset, .. } => {
                        assert_eq!(from_offset, 10, "the first page anchors the stretch");
                        vec![
                            row(21, "n-b", "exited"),
                            row(22, "n-c", "live"),
                            Response::Mark {
                                next: 30,
                                head: 30,
                                resume: String::new(),
                                durable: None,
                                oldest: None,
                            },
                        ]
                    }
                    Request::Publish { .. } => vec![Response::PublishAck {
                        offset: 14,
                        deduped: false,
                    }],
                    other => panic!("unexpected request: {other:?}"),
                };
                for response in responses {
                    write_frame(&mut peer, &encode_response(&response)).unwrap();
                }
            }
        });
        bridge.conn = Some(Conn::from_stream(Box::new(ours)).into());
        let answer = bridge.advertisers("s-x");
        if closed {
            assert!(
                answer.is_err(),
                "a disconnected walk is not a complete face"
            );
            assert!(bridge.conn.is_none());
        } else {
            assert_eq!(
                answer.unwrap(),
                BTreeSet::from(["n-a".into(), "n-c".into()])
            );
        }
        let transcript = lane.transcript();
        let held = transcript
            .iter()
            .position(|line| line == &format!("hold {SID} on reason=stop origin=fleet"))
            .unwrap();
        if closed {
            assert!(!transcript.iter().any(|line| line == "last cursor"));
        } else {
            let next = transcript
                .iter()
                .position(|line| line == "last cursor")
                .unwrap();
            assert!(held < next, "halt waited for later pages: {transcript:#?}");
        }
        assert!(bridge.halt_applied);
        bridge.conn = None;
        server.join().unwrap();
    }
}

#[cfg(unix)]
struct ReadTap {
    inner: std::os::unix::net::UnixStream,
    pending: Vec<u8>,
    reads: Arc<ReadCounts>,
}

#[cfg(unix)]
#[derive(Default)]
struct ReadCounts {
    last: std::sync::atomic::AtomicUsize,
    fetch: std::sync::atomic::AtomicUsize,
    fetched_from: std::sync::Mutex<Vec<u64>>,
}

#[cfg(unix)]
impl ReadCounts {
    fn take(&self) -> (usize, usize) {
        use std::sync::atomic::Ordering::SeqCst;
        (self.last.swap(0, SeqCst), self.fetch.swap(0, SeqCst))
    }

    fn last_fetched_from(&self) -> Option<u64> {
        self.fetched_from.lock().unwrap().last().copied()
    }
}

#[cfg(unix)]
impl std::io::Read for ReadTap {
    fn read(&mut self, buf: &mut [u8]) -> io::Result<usize> {
        self.inner.read(buf)
    }
}

#[cfg(unix)]
impl std::io::Write for ReadTap {
    fn write(&mut self, buf: &[u8]) -> io::Result<usize> {
        use astream_broker::proto::{decode_request, read_frame};
        use astream_broker::Request;
        use std::sync::atomic::Ordering::SeqCst;
        let n = self.inner.write(buf)?;
        self.pending.extend_from_slice(&buf[..n]);
        loop {
            let mut cursor = std::io::Cursor::new(&self.pending[..]);
            let Ok(Some(frame)) = read_frame(&mut cursor) else {
                break;
            };
            let used = usize::try_from(cursor.position()).unwrap();
            match decode_request(&frame) {
                Some(Request::Last { .. }) => {
                    self.reads.last.fetch_add(1, SeqCst);
                }
                Some(Request::Fetch { from_offset, .. }) => {
                    self.reads.fetch.fetch_add(1, SeqCst);
                    self.reads.fetched_from.lock().unwrap().push(from_offset);
                }
                _ => {}
            }
            self.pending.drain(..used);
        }
        Ok(n)
    }

    fn flush(&mut self) -> io::Result<()> {
        self.inner.flush()
    }
}

#[cfg(unix)]
#[test]
fn a_bare_post_to_a_remote_peer_walks_once_while_the_presence_view_is_live() {
    use crate::presence_view::Phase as ViewPhase;
    const SID: &str = "s-near";
    let (lane, mut bridge) = FakeVerbLane::bridge(&[(1, SID)]);
    let socket = lane.dir.path().join("broker.sock");
    let broker = crate::test_broker(lane.dir.path().join("broker.log")).unwrap();
    let _server = broker.serve(&socket).unwrap();
    bridge.cfg.broker = socket.to_string_lossy().into_owned();
    let reads = Arc::new(ReadCounts::default());
    let tap = ReadTap {
        inner: std::os::unix::net::UnixStream::connect(&socket).unwrap(),
        pending: Vec::new(),
        reads: Arc::clone(&reads),
    };
    bridge.conn = Some(Conn::from_stream(Box::new(tap)).into());
    let node = bridge.node.clone();
    let to = |far: &str, sid: &str| Route::To(format!("/f/f/in/{far}/{sid}/{node}"));
    let (mut god, _) = bridge.connect().unwrap();
    let (mut reader, _) = bridge.connect().unwrap();
    let mut seq = 0;
    let mut publish = |subject: &str, body: &str| -> u64 {
        seq += 1;
        god.publish(7, seq, subject, body.as_bytes()).unwrap().0
    };
    let view = Arc::clone(&bridge.presence_view);
    let caught_up = |off: u64| {
        assert!(
            view.wait_for(aterm_spec::wait::MUST_HAPPEN, |phase, reached| {
                phase == ViewPhase::Live && reached > off
            }),
            "the presence view never showed offset {off}"
        );
    };
    let counted = |bridge: &mut Bridge, to: &str| {
        reads.take();
        let route = bridge.resolve_to(to, Some(SID)).unwrap();
        let (last, fetch) = reads.take();
        (route, last, fetch)
    };
    let walked = |(_, last, fetch): &(Route, usize, usize)| *last >= 1 && *fetch >= 1;
    const LIVE: &str = "v=1 t=1 state=live";
    const EXITED: &str = "v=1 t=1 state=exited";
    let row = |far: &str, sid: &str| format!("/f/f/pub/{far}/{sid}/presence");

    let off = publish(&row("n-far", "s-far"), LIVE);
    assert_eq!(view.phase(), ViewPhase::Closed);
    let first = counted(&mut bridge, "@s-far");
    assert_eq!(first.0, to("n-far", "s-far"));
    assert!(walked(&first), "a Last and its stretch's Fetch: {first:?}");
    assert_eq!(bridge.state.pins()["s-far"], "n-far");
    caught_up(off);
    assert_eq!(counted(&mut bridge, "@s-far"), (to("n-far", "s-far"), 0, 1));

    publish(&row("n-a", "s-moved"), LIVE);
    publish(&row("n-a", "s-moved"), EXITED);
    let moved = publish(&row("n-b", "s-moved"), LIVE);
    caught_up(moved);
    assert_eq!(
        counted(&mut bridge, "@s-moved"),
        (to("n-b", "s-moved"), 0, 1)
    );
    assert_eq!(
        bridge.state.pins()["s-moved"],
        "n-b",
        "first sight, through the view"
    );

    publish(&row("n-far", "s-far"), EXITED);
    let off = publish(&row("n-c", "s-far"), LIVE);
    caught_up(off);
    let contested = counted(&mut bridge, "@s-far");
    assert_eq!(contested.0, Route::Ambiguous);
    assert!(
        walked(&contested),
        "the verdict that retires is the walk's: {contested:?}"
    );

    let miss = counted(&mut bridge, "@s-nobody");
    assert_eq!(miss.0, Route::Unroutable);
    assert!(
        walked(&miss),
        "the unroutable verdict is the walk's: {miss:?}"
    );

    view.cut();
    assert!(
        view.wait_for(aterm_spec::wait::MUST_HAPPEN, |phase, _| phase
            == ViewPhase::Dead),
        "a cut subscription is a dead view"
    );
    bridge.presence_view_retry_at = Some(Instant::now() + aterm_spec::wait::MUST_HAPPEN);
    let dead = counted(&mut bridge, "@s-moved");
    assert_eq!(dead.0, to("n-b", "s-moved"));
    assert!(walked(&dead), "a dead view is a walk: {dead:?}");
    assert_eq!(
        view.phase(),
        ViewPhase::Dead,
        "not redialled inside the bound"
    );
    bridge.presence_view_retry_at = None;
    assert!(walked(&counted(&mut bridge, "@s-moved")));
    caught_up(off);
    assert_eq!(
        counted(&mut bridge, "@s-moved"),
        (to("n-b", "s-moved"), 0, 1)
    );

    bridge.presence_view.close();
    let g = view.open();
    assert!(view.apply(
        g,
        "f",
        moved,
        "/f/f/pub/n-b/s-moved/presence",
        b"v=1 t=1 state=live"
    ));
    assert!(view.mark(g, off + 1, off + 1));
    let exact = || view.snapshot("s-moved").expect("live").exact_to;

    for i in 0..20 {
        publish(&format!("/f/f/term/n-z/s-z/{i}"), "busy");
    }
    let head = reader.fetch(0, "/f/f/>", 0).unwrap().1 .1;
    assert!(exact() < head, "the view is behind the traffic");
    assert_eq!(
        counted(&mut bridge, "@s-moved"),
        (to("n-b", "s-moved"), 0, 1)
    );
    assert_eq!(reads.last_fetched_from(), Some(off + 1));
    assert_eq!(exact(), head, "the confirmation moved the view to the head");
    assert_eq!(
        counted(&mut bridge, "@s-moved"),
        (to("n-b", "s-moved"), 0, 1)
    );
    assert_eq!(
        reads.last_fetched_from(),
        Some(head),
        "the next confirmation scans nothing the last one did"
    );

    let rival_off = publish(&row("n-d", "s-moved"), LIVE);
    assert_eq!(
        view.snapshot("s-moved").map(|s| s.nodes),
        Some(std::collections::BTreeSet::from(["n-b".to_string()])),
        "NEGATIVE CONTROL: unconfirmed, the view routes it to the pinned node"
    );
    let rival = counted(&mut bridge, "@s-moved");
    assert_eq!(
        rival.0,
        Route::Ambiguous,
        "a claim on the log before the post is the post's verdict"
    );
    assert!(
        rival.1 >= 1 && rival.2 >= 2,
        "the confirmation, then the walk: {rival:?}"
    );
    assert_eq!(
        exact(),
        rival_off,
        "exact up to the presence record it found, which it has not applied"
    );

    let g = view.open();
    assert!(view.apply(
        g,
        "f",
        moved,
        "/f/f/pub/n-b/s-moved/presence",
        b"v=1 t=1 state=live"
    ));
    assert!(view.mark(g, rival_off, rival_off));
    assert!(view.mark(g, rival_off, rival_off + CONFIRM_SCAN_MAX + 1));
    let long = counted(&mut bridge, "@s-moved");
    assert_eq!(long.0, Route::Ambiguous);
    assert_eq!((long.1, long.2), (1, 1), "the walk alone: {long:?}");
    let g = view.open();
    assert!(view.apply(
        g,
        "f",
        moved,
        "/f/f/pub/n-b/s-moved/presence",
        b"v=1 t=1 state=live"
    ));
    assert!(view.mark(g, rival_off, rival_off));
    let bus_high = std::mem::replace(&mut bridge.bus_high, rival_off + CONFIRM_SCAN_MAX + 1);
    let long = counted(&mut bridge, "@s-moved");
    assert_eq!((long.1, long.2), (1, 1), "the walk alone: {long:?}");
    bridge.bus_high = bus_high;

    let cut_a = publish(&row("n-cut-a", "s-cut"), LIVE);
    let solo = publish(&row("n-solo", "s-solo"), LIVE);
    let g = view.open();
    assert!(view.apply(g, "f", cut_a, &row("n-cut-a", "s-cut"), LIVE.as_bytes()));
    assert!(view.apply(g, "f", solo, &row("n-solo", "s-solo"), LIVE.as_bytes()));
    let before_cut = solo + 1;
    assert!(view.mark(g, before_cut, before_cut));
    let rival = publish(&row("n-cut-b", "s-cut"), LIVE);
    let floor = publish("/f/f/term/n-z/s-z/after-cut", "busy");
    assert!(before_cut <= rival && rival < floor);
    broker.retain_before_carrying_state(floor).expect("the cut");
    let filter = crate::presence_view::presence_filter("f");
    let refused = reader.fetch(before_cut, &filter, CONFIRM_ROWS).unwrap_err();
    assert_eq!(transport::retained_past(&refused, before_cut), Some(floor));
    let ((suffix, (next, head)), crossed) =
        transport::fetch_kept(&mut reader, before_cut, &filter, CONFIRM_ROWS).unwrap();
    assert_eq!(crossed, Some(floor));
    assert!(
        suffix.is_empty() && next == head,
        "Fetch cannot see the rival"
    );
    let (last, _) = reader.last(&filter, "", CONFIRM_ROWS).unwrap();
    for far in ["n-cut-a", "n-cut-b"] {
        assert!(
            last.iter()
                .any(|(_, subject, _)| subject == &row(far, "s-cut")),
            "Last preserves the actual claim of {far}"
        );
    }
    assert_eq!(
        view.snapshot("s-cut").unwrap().nodes,
        BTreeSet::from(["n-cut-a".to_string()]),
        "NEGATIVE CONTROL: the stale view alone would route to n-cut-a"
    );
    let cut = counted(&mut bridge, "@s-cut");
    assert_eq!(
        cut.0,
        Route::Ambiguous,
        "the retained rival is still a rival"
    );
    assert!(walked(&cut), "the cut requires Last: {cut:?}");
    assert_eq!(view.snapshot("s-cut").unwrap().exact_to, before_cut);
    let solo = counted(&mut bridge, "@s-solo");
    assert_eq!(solo.0, to("n-solo", "s-solo"));
    assert!(walked(&solo), "a unique route survives the cut: {solo:?}");
    assert_eq!(view.snapshot("s-solo").unwrap().exact_to, before_cut);

    bridge.close_subscriptions();
    assert_eq!(view.phase(), ViewPhase::Closed);
    assert!(bridge.presence_view_retry_at.is_none());
}

#[cfg(unix)]
#[test]
fn the_presence_view_is_dialled_off_the_bridges_loop() {
    use crate::presence_view::Phase as ViewPhase;
    use std::io::Read as _;
    let (lane, mut bridge) = FakeVerbLane::bridge(&[(1, "s-near")]);
    let socket = lane.dir.path().join("mute.sock");
    let mute = std::os::unix::net::UnixListener::bind(&socket).unwrap();
    bridge.cfg.broker = socket.to_string_lossy().into_owned();
    bridge.caps = crate::enable::node_rules("f", &bridge.node)
        .into_iter()
        .map(|grant| Cap {
            grant,
            tag: [0; 32],
        })
        .collect();
    let (publisher, _peer) = std::os::unix::net::UnixStream::pair().unwrap();
    bridge.conn = Some(Conn::from_stream(Box::new(publisher)).into());

    bridge.want_presence_view();
    let view = Arc::clone(&bridge.presence_view);
    assert_eq!(
        view.phase(),
        ViewPhase::Seeding,
        "the ask returned before any attach was answered"
    );
    let (mut dialled, _) = aterm_uds::accept(
        &mute,
        aterm_uds::AcceptWait::Until(Instant::now() + aterm_spec::wait::MUST_HAPPEN),
        aterm_uds::AcceptedMode::Blocking,
    )
    .expect("the view's reader to dial");
    dialled
        .set_read_timeout(Some(aterm_spec::wait::MUST_HAPPEN))
        .unwrap();
    let mut first = [0u8; 1];
    dialled
        .read_exact(&mut first)
        .expect("the reader's attach reaches the broker");
    drop(dialled);
    assert!(
        view.wait_for(aterm_spec::wait::MUST_HAPPEN, |phase, _| phase
            == ViewPhase::Dead),
        "a broker that hung up on the dial is a dead view"
    );
    assert!(bridge.conn.is_some(), "and nothing of the bridge's link");
}

#[cfg(unix)]
fn presence_view_under_the_guard(
    serve: impl FnOnce(
        &astream_broker::Broker,
        &std::path::Path,
    ) -> (astream_broker::BrokerHandle, String),
) {
    use crate::presence_view::Phase as ViewPhase;
    const FAR: &str = "n-00000000000000fa";
    let secret = vec![0x5a_u8; 32];
    let (lane, mut bridge) = FakeVerbLane::bridge(&[(1, "s-near")]);
    let broker =
        astream_broker::Broker::open_guarded(lane.dir.path().join("guarded.log"), secret.clone())
            .unwrap();
    let (_server, endpoint) = serve(&broker, lane.dir.path());
    bridge.cfg.broker = endpoint.clone();
    let ring = |node: &str, keep: &dyn Fn(&str) -> bool| -> Vec<Cap> {
        crate::enable::node_rules("f", node)
            .into_iter()
            .filter(|grant| keep(grant))
            .map(|grant| {
                let c = astream_cap::mint(&secret, &grant).expect("mint");
                Cap {
                    grant: c.filter,
                    tag: c.tag,
                }
            })
            .collect()
    };
    bridge.caps = ring(&bridge.node, &|_| true);
    bridge.conn = Some(bridge.connect().expect("the publisher attaches").0.into());
    let (mut far, _) = transport::connect(&endpoint, ACK_DEADLINE).unwrap();
    transport::attach_ring(&mut far, &ring(FAR, &|_| true)).unwrap();
    let subject = format!("/f/f/pub/{FAR}/s-far/presence");
    let off = far
        .publish(
            astream_cap::producer_id_of(FAR),
            1,
            &subject,
            b"v=1 t=1 state=live",
        )
        .unwrap()
        .0;
    let to = Route::To(format!("/f/f/in/{FAR}/s-far/{}", bridge.node));

    assert_eq!(bridge.resolve_to("@s-far", Some("s-near")).unwrap(), to);
    let view = Arc::clone(&bridge.presence_view);
    assert!(
        view.wait_for(aterm_spec::wait::MUST_HAPPEN, |phase, reached| {
            matches!(phase, ViewPhase::Live | ViewPhase::Dead)
                && (phase == ViewPhase::Dead || reached > off)
        }),
        "the view never settled"
    );
    assert_eq!(
        view.phase(),
        ViewPhase::Live,
        "the ring covers the view's seed, its subscription and its idle marks"
    );
    let head = far.fetch(0, "/f/f/pub/>", 0).unwrap().1 .1;
    assert_eq!(bridge.resolve_to("@s-far", Some("s-near")).unwrap(), to);
    assert!(bridge.conn.is_some());
    assert_eq!(
        view.snapshot("s-far").map(|s| s.exact_to),
        Some(head),
        "the confirmation answered, through to the head"
    );

    bridge.presence_view.close();
    bridge.presence_view_retry_at = None;
    bridge.caps = ring(&bridge.node, &|grant| grant != "ro:/f/f/pub/>");
    bridge.want_presence_view();
    assert!(
        view.wait_for(aterm_spec::wait::MUST_HAPPEN, |phase, _| phase
            == ViewPhase::Dead),
        "a ring without the presence read is refused the view"
    );
}

#[cfg(unix)]
#[test]
fn the_presence_view_goes_live_under_a_nodes_ring_on_a_guarded_broker() {
    presence_view_under_the_guard(|broker, dir| {
        let socket = dir.join("g.sock");
        let server = broker.serve(&socket).expect("serve");
        (server, socket.to_string_lossy().into_owned())
    });
}

#[cfg(unix)]
#[test]
fn dropping_a_bridge_closes_its_live_presence_reader() {
    use crate::presence_view::Phase;

    struct ObservedStream {
        inner: Option<Box<dyn transport::Stream>>,
        ended: std::sync::mpsc::Sender<()>,
    }
    impl std::io::Read for ObservedStream {
        fn read(&mut self, buf: &mut [u8]) -> io::Result<usize> {
            self.inner.as_mut().expect("live stream").read(buf)
        }
    }
    impl std::io::Write for ObservedStream {
        fn write(&mut self, buf: &[u8]) -> io::Result<usize> {
            self.inner.as_mut().expect("live stream").write(buf)
        }

        fn flush(&mut self) -> io::Result<()> {
            self.inner.as_mut().expect("live stream").flush()
        }
    }
    impl Drop for ObservedStream {
        fn drop(&mut self) {
            drop(self.inner.take());
            let _ = self.ended.send(());
        }
    }

    let (lane, mut bridge) = FakeVerbLane::bridge(&[(1, "s-near")]);
    let socket = lane.dir.path().join("broker.sock");
    let broker = crate::test_broker(lane.dir.path().join("broker.log")).unwrap();
    let _server = broker.serve(&socket).unwrap();
    bridge.cfg.broker = socket.to_string_lossy().into_owned();
    let (mut publisher, _) = bridge.connect().unwrap();
    let off = publisher
        .publish(7, 1, "/f/f/pub/n-far/s-far/presence", b"v=1 t=1 state=live")
        .unwrap()
        .0;
    bridge.conn = Some(publisher.into());

    let (conn, closer) = bridge.connect().unwrap();
    let (ended, stopped) = std::sync::mpsc::channel();
    let observed = ObservedStream {
        inner: Some(conn.into_stream()),
        ended,
    };
    let view = Arc::clone(&bridge.presence_view);
    let generation = view.open();
    crate::presence_view::spawn_reader(
        move || Ok((Conn::from_stream(Box::new(observed)), closer.into_os())),
        &bridge.cfg.fleet,
        Arc::clone(&view),
        generation,
    )
    .unwrap();
    assert!(
        view.wait_for(aterm_spec::wait::MUST_HAPPEN, |phase, reached| {
            phase == Phase::Live && reached > off
        })
    );
    assert_eq!(
        view.snapshot("s-far").map(|snapshot| snapshot.nodes),
        Some(BTreeSet::from(["n-far".to_string()]))
    );
    assert!(matches!(
        stopped.try_recv(),
        Err(std::sync::mpsc::TryRecvError::Empty)
    ));

    drop(bridge);
    assert_eq!(
        view.phase(),
        Phase::Closed,
        "the bridge's view outlived its owner"
    );
    stopped
        .recv_timeout(aterm_spec::wait::MUST_HAPPEN)
        .expect("the presence reader still owns its broker stream");
    assert!(
        !view.mark(generation, off + 1, off + 1),
        "the old reader stays fenced"
    );
}

#[cfg(unix)]
#[test]
fn a_record_refused_twice_answers_its_sender_once() {
    const SID: &str = "s-host";
    let (lane, mut bridge) = FakeVerbLane::bridge(&[(1, SID)]);
    let socket = lane.dir.path().join("broker.sock");
    let broker = crate::test_broker(lane.dir.path().join("broker.log")).unwrap();
    let _server = broker.serve(&socket).unwrap();
    bridge.cfg.broker = socket.to_string_lossy().into_owned();
    let (conn, _) = bridge.connect().unwrap();
    bridge.conn = Some(conn.into());
    assert!(bridge.refresh_sessions().is_ok());
    let node = bridge.node.clone();
    let local = format!("/f/f/in/{node}/{SID}/{node}/note");
    let raw = b"v=1 t=1 from=s-gone via=NOT-A-PRINCIPAL text=x";
    let off = bridge.publish_own_off(&local, raw).unwrap();
    for _ in 0..2 {
        assert_eq!(
            bridge.deliver_record(off, &local, raw, None),
            Delivery::Accounted
        );
    }
    let (mut reader, _) = bridge.connect().unwrap();
    let notices = format!("/f/f/in/{node}/s-gone/{node}/undeliverable");
    let (rows, _) = reader.fetch(0, &notices, 16).unwrap();
    let answers: Vec<Option<u64>> = rows.iter().map(|(_, _, b)| Body::decode(b).0.re).collect();
    assert_eq!(answers, [Some(off)], "one notice for one refused record");
}

const SHIPPED_DELIVER_USAGE: &str = "ERR usage: deliver <sid> off=<n> from=<p> kind=<k> \
    trust=<t> [re=<n>] [dl=<ms>] [late=1] [demoted=<k>] [via=<p,...>] \
    [verdict=<handled|refused|deferred>] [len=<n>] [text=<pct>] | deliver <sid> \
    landed=<post-id> off=<n> | deliver <sid> fetched=<off> (err=<token> | from=<p> \
    kind=<k> trust=<t> [re=<n>] [dl=<ms>] [demoted=<k>] [via=<p,...>] [verdict=<v>] \
    [len=<n>] [at=<n>] [more=1] [text=<pct>]) | deliver <sid> receipt=<rid> off=<n|->\n";

#[cfg(unix)]
#[test]
fn a_broadcast_opt_out_race_leaves_the_sessions_other_mail_deliverable() {
    const SID: &str = "s-optout";
    let (lane, mut bridge) = FakeVerbLane::bridge(&[(1, SID)]);
    let socket = lane.dir.path().join("broker.sock");
    let broker = crate::test_broker(lane.dir.path().join("broker.log")).unwrap();
    let _server = broker.serve(&socket).unwrap();
    bridge.cfg.broker = socket.to_string_lossy().into_owned();
    bridge.conn = Some(bridge.connect().unwrap().0.into());
    assert!(bridge.refresh_sessions().is_ok());
    bridge.per_sid.pending_admit.clear();
    bridge.per_sid.topics.insert(
        SID.to_string(),
        BTreeMap::from([
            ("declined".to_string(), TopicCursor { next: 20 }),
            ("wanted".to_string(), TopicCursor { next: 20 }),
        ]),
    );
    bridge.say_drained_to = 20;
    bridge.say_bus_head = 20;

    lane.opt_in(SID, &["topic wanted since=@20"]);
    lane.answer_deliveries_to(SID, "ERR not-subscribed\n");
    bridge.on_say_record(&(
        20,
        "/f/f/pub/n-peer/s-peer/say/declined".to_string(),
        b"v=1 t=1 text=declined".to_vec(),
    ));
    let read_past = bridge.per_sid.topics[SID]["declined"].next;

    lane.deliver_answers.lock().unwrap().remove(SID);
    bridge.topic_change(SID, &mut "drop declined".split_whitespace());
    bridge.on_say_record(&(
        21,
        "/f/f/pub/n-peer/s-peer/say/wanted".to_string(),
        b"v=1 t=2 text=wanted".to_vec(),
    ));
    bridge.on_inbox_record(
        &(
            22,
            format!("/f/f/in/{}/{SID}/n-peer/note", bridge.node),
            b"v=1 t=3 from=s-peer text=addressed".to_vec(),
        ),
        None,
    );
    assert_eq!(
        (
            read_past,
            bridge.per_sid.topics[SID]["wanted"].next,
            lane.delivery_offsets(),
            bridge.committed,
            bridge.per_sid.lane_owed.contains_key(SID),
        ),
        (21, 22, vec![21, 22], Some(22), false),
        "declining one broadcast must not turn the session's lane into a fault"
    );
}

#[cfg(unix)]
#[test]
fn an_answer_that_is_no_verdict_keeps_the_record_owed_and_holds_back_only_its_session() {
    const HELD: &str = "s-held";
    const OTHER: &str = "s-other";
    let (lane, mut bridge) = FakeVerbLane::bridge(&[(1, HELD), (2, OTHER)]);
    let socket = lane.dir.path().join("broker.sock");
    let broker = crate::test_broker(lane.dir.path().join("broker.log")).unwrap();
    let _server = broker.serve(&socket).unwrap();
    bridge.cfg.broker = socket.to_string_lossy().into_owned();
    let (conn, _) = bridge.connect().unwrap();
    bridge.conn = Some(conn.into());
    assert!(bridge.refresh_sessions().is_ok());
    bridge.per_sid.pending_admit.clear();
    let node = bridge.node.clone();
    let mail = |sid: &str, off: u64| -> BrokerRecord {
        (
            off,
            format!("/f/f/in/{node}/{sid}/n-peer/note"),
            b"v=1 t=1 from=s-peer text=hi".to_vec(),
        )
    };

    lane.answer_deliveries_to(HELD, SHIPPED_DELIVER_USAGE);
    bridge.on_inbox_record(&mail(HELD, 20), None);
    bridge.on_inbox_record(&mail(HELD, 21), None);
    bridge.on_inbox_record(&mail(OTHER, 22), None);
    assert_eq!(
        lane.transcript(),
        ["deliver 20", "deliver 22"],
        "the held session is asked once, and its next record is not sent"
    );
    assert_eq!(lane.delivery_offsets(), [22]);
    assert_eq!(bridge.inbox_commit_floor(), Some(20));
    assert_eq!(
        bridge.committed,
        Some(19),
        "a later record commits the group up to the owed one, never past it"
    );

    lane.answer_deliveries_to(OTHER, "ERR quota\n");
    bridge.on_inbox_record(&mail(OTHER, 23), None);
    lane.answer_deliveries_to(OTHER, "ERR no such session\n");
    bridge.on_inbox_record(&mail(OTHER, 24), None);
    assert_eq!(
        bridge.committed,
        Some(19),
        "and they do not move it past the floor"
    );

    let (mut reader, _) = bridge.connect().unwrap();
    let notices = format!("/f/f/in/n-peer/s-peer/{node}/undeliverable");
    let (rows, _) = reader.fetch(0, &notices, 16).unwrap();
    let told: Vec<(Option<u64>, String)> = rows
        .iter()
        .map(|(_, _, b)| {
            let (body, _) = Body::decode(b);
            (body.re, body.text)
        })
        .collect();
    assert_eq!(
        told,
        [
            (Some(23), "not delivered: quota".to_string()),
            (Some(24), "not delivered: not-hosted".to_string()),
        ],
        "the sender hears the verdicts, and nothing about the owed records"
    );
    let ev_of = |sid: &str, reader: &mut Conn| -> Vec<String> {
        let face = subject::session_face("f", &node, sid, "ev");
        let (rows, _) = reader.fetch(0, &face, 256).unwrap();
        rows.iter()
            .filter(|(_, s, _)| *s == face)
            .filter_map(|(_, _, raw)| {
                let ev = Body::decode(raw).0.unknown.get("ev")?.clone();
                Some(crate::pct::decode(&ev))
            })
            .collect()
    };
    assert_eq!(
        ev_of(HELD, &mut reader),
        ["lane-fault off=20 verb=deliver reply=usage lane=0"],
        "one fault, said once, and no `undeliverable`"
    );
    assert_eq!(
        ev_of(OTHER, &mut reader),
        [
            "undeliverable off=23 reason=quota",
            "undeliverable off=24 reason=not-hosted"
        ]
    );
}

#[cfg(unix)]
#[test]
fn inbox_commit_debt_follows_live_sessions_and_survives_reconnects() {
    const FIRST: &str = "s-first";
    const SECOND: &str = "s-second";
    const HEALTHY: &str = "s-healthy";
    for unattributed in [None, Some(10)] {
        let (lane, mut bridge) = FakeVerbLane::bridge(&[(1, FIRST), (2, SECOND), (3, HEALTHY)]);
        let socket = lane.dir.path().join("broker.sock");
        let broker = crate::test_broker(lane.dir.path().join("broker.log")).unwrap();
        let _server = broker.serve(&socket).unwrap();
        bridge.cfg.broker = socket.to_string_lossy().into_owned();
        bridge.conn = Some(bridge.connect().unwrap().0.into());
        assert!(bridge.refresh_sessions().is_ok());
        bridge.per_sid.pending_admit.clear();
        bridge.commit_floor = unattributed;
        let node = bridge.node.clone();
        let mail = |sid: &str, off: u64| -> BrokerRecord {
            (
                off,
                format!("/f/f/in/{node}/{sid}/n-peer/note"),
                b"v=1 t=1 from=s-peer text=mail".to_vec(),
            )
        };
        lane.answer_deliveries_to(FIRST, SHIPPED_DELIVER_USAGE);
        lane.answer_deliveries_to(SECOND, SHIPPED_DELIVER_USAGE);
        bridge.on_inbox_record(&mail(FIRST, 20), None);
        bridge.on_inbox_record(&mail(SECOND, 30), None);
        bridge.on_inbox_record(&mail(HEALTHY, 40), None);
        let mut committed = vec![bridge.committed];

        lane.sessions
            .lock()
            .unwrap()
            .retain(|(_, sid)| sid != FIRST);
        assert!(bridge.refresh_sessions().is_ok());
        bridge.on_inbox_record(&mail(HEALTHY, 41), None);
        committed.push(bridge.committed);

        bridge.conn = None;
        assert!(bridge.attach_broker());
        assert!(!bridge.per_sid.lane_owed.contains_key(FIRST));
        assert!(bridge.per_sid.lane_owed.contains_key(SECOND));
        bridge.on_inbox_record(&mail(SECOND, 31), None);
        bridge.on_inbox_record(&mail(HEALTHY, 42), None);
        committed.push(bridge.committed);

        lane.sessions
            .lock()
            .unwrap()
            .retain(|(_, sid)| sid != SECOND);
        assert!(bridge.refresh_sessions().is_ok());
        bridge.on_inbox_record(&mail(HEALTHY, 43), None);
        committed.push(bridge.committed);
        assert!(bridge.per_sid.lane_owed.is_empty());
        assert_eq!(
            committed,
            if unattributed.is_some() {
                [9, 9, 9, 9].map(Some)
            } else {
                [19, 29, 29, 43].map(Some)
            },
            "only departed, attributed debt is discharged: {unattributed:?}"
        );
        assert_eq!(
            lane.delivery_offsets(),
            [40, 41, 42, 43],
            "healthy mail keeps landing while the two incompatible lanes are frozen"
        );
    }
}

#[cfg(unix)]
#[test]
fn a_log_of_another_identity_owes_nothing_the_old_one_owed() {
    const FAULTED: &str = "s-faulted";
    const HEALTHY: &str = "s-healthy";
    let (lane, mut bridge) = FakeVerbLane::bridge(&[(1, FAULTED), (2, HEALTHY)]);
    assert!(bridge.refresh_sessions().is_ok());
    let (a, _broker_a, _server_a) = serve_log(&lane, "a");
    bridge.cfg.broker = a;
    assert!(bridge.attach_broker());
    let node = bridge.node.clone();
    let mail = |sid: &str, off: u64| -> BrokerRecord {
        (
            off,
            format!("/f/f/in/{node}/{sid}/n-peer/note"),
            b"v=1 t=1 from=s-peer text=mail".to_vec(),
        )
    };
    lane.answer_deliveries_to(FAULTED, SHIPPED_DELIVER_USAGE);
    bridge.on_inbox_record(&mail(FAULTED, 20), None);
    bridge.commit_floor = Some(10);
    assert_eq!(bridge.inbox_commit_floor(), Some(10));

    let (b, _broker_b, _server_b) = serve_log(&lane, "b");
    bridge.close_subscriptions();
    bridge.conn = None;
    bridge.cfg.broker = b;
    assert!(bridge.attach_broker());
    assert_eq!(
        bridge.inbox_commit_floor(),
        None,
        "A's debt is no floor on B"
    );
    assert_eq!(
        bridge.per_sid.lane_owed.get(FAULTED),
        Some(&None),
        "the faulted lane stays held, owing nothing on B yet"
    );
    bridge.on_inbox_record(&mail(HEALTHY, 40), None);
    assert_eq!(
        bridge.committed,
        Some(40),
        "B's mail commits past A's old debt"
    );
    bridge.on_inbox_record(&mail(FAULTED, 50), None);
    bridge.on_inbox_record(&mail(HEALTHY, 60), None);
    assert_eq!(
        bridge.committed,
        Some(49),
        "B's own owed record bounds B's group"
    );
}

#[test]
fn a_broadcast_on_a_dropped_topic_is_the_sessions_verdict_and_holds_nothing_back() {
    let (lane, mut bridge) = FakeVerbLane::bridge(&[(1, "s-a")]);
    bridge.say_drained_to = 5;
    bridge.say_bus_head = 5;
    bridge.per_sid.topics.insert(
        "s-a".to_string(),
        BTreeMap::from([("news".to_string(), TopicCursor { next: 5 })]),
    );
    lane.answer_deliveries_to("s-a", "ERR not-subscribed\n");
    let live = "/f/f/pub/n-x/s-1/say/news".to_string();
    bridge.on_say_record(&(5, live, Body::new(0).encode()));
    assert_eq!(lane.transcript(), ["deliver 5"]);
    assert!(
        bridge.per_sid.lane_owed.is_empty(),
        "the session is not held back: {:?}",
        bridge.per_sid.lane_owed
    );
    assert_eq!(
        bridge.per_sid.topics["s-a"]["news"].next, 6,
        "its cursor moves past the record it declined"
    );
}

#[cfg(unix)]
#[test]
fn the_hello_reads_each_endpoints_lane_and_build_and_refuses_none() {
    let own = aterm_stamp::build_id();
    let lane_now = aterm_types::control_verbs::BRIDGE_LANE;
    let (lane, mut bridge) = FakeVerbLane::bridge(&[(1, "s-a")]);
    bridge.introduce();
    assert_eq!(
        lane.hellos(),
        [format!("hello lane={lane_now} build={own} {HELLO_REFILL}")]
    );
    assert_eq!(
        (bridge.endpoint_lane, bridge.endpoint_build.clone()),
        (0, None)
    );

    lane.answer_hello_with(&format!("OK lane={lane_now} build={own}\n"));
    bridge.introduce();
    assert_eq!(
        (bridge.endpoint_lane, bridge.endpoint_build.as_deref()),
        (lane_now, Some(own.as_str()))
    );

    lane.answer_hello_with("OK lane=7 build=9.9.0+g0123456789ab future=1\n");
    bridge.introduce();
    assert_eq!(
        (bridge.endpoint_lane, bridge.endpoint_build.as_deref()),
        (7, Some("9.9.0+g0123456789ab"))
    );
    assert!(bridge.first_roster());
    assert!(lane.links().is_empty());
}

#[cfg(unix)]
#[test]
fn an_endpoint_without_the_bridge_roster_is_named_and_never_attached() {
    let (lane, mut bridge) = FakeVerbLane::bridge(&[(1, "s-a")]);
    lane.answer_roster_with("ERR usage: sessions\n");
    assert!(!bridge.first_roster(), "a grammar the bridge cannot read");
    assert_eq!(lane.links(), ["link down reason=endpoint-grammar"]);
    assert!(bridge.locals.is_empty() && bridge.per_sid.epochs.is_empty());

    let (lane, mut bridge) = FakeVerbLane::bridge(&[(1, "s-a")]);
    lane.refuse_roster(true);
    assert!(
        bridge.first_roster(),
        "a busy endpoint is asked again later"
    );
    assert!(lane.links().is_empty(), "and nothing is reported for it");
    lane.refuse_roster(false);
    assert!(bridge.first_roster());
    assert_eq!(bridge.locals.get(&1).map(String::as_str), Some("s-a"));
}

#[cfg(unix)]
#[test]
fn an_older_halt_record_on_the_tail_does_not_lift_a_newer_standing_halt() {
    let (_lane, mut bridge) = FakeVerbLane::bridge(&[(1, "s-a")]);
    let halt = |off: u64, state: &str| -> BrokerRecord {
        let body = format!("v=1 t=1 state={state} reason=stop");
        (off, "/f/f/fleet/h-b/halt".to_string(), body.into_bytes())
    };
    bridge.apply_fleet_halts(vec![halt(130, "on")], 100);
    assert!(bridge.halt_applied);
    bridge.on_fleet_record(&halt(120, "off"));
    assert!(bridge.halt_applied, "an older `off` lifted a standing halt");
    bridge.on_fleet_record(&halt(130, "on"));
    assert!(bridge.halt_applied);
    bridge.on_fleet_record(&halt(140, "off"));
    assert!(!bridge.halt_applied, "a newer `off` lifts it");
}

#[cfg(unix)]
fn local_pair_world(
    name: &str,
) -> (
    FakeVerbLane,
    Bridge,
    String,
    String,
    (astream_broker::Broker, astream_broker::BrokerHandle),
) {
    let (lane, mut bridge) = FakeVerbLane::bridge(&[(1, "s-a"), (2, "s-b")]);
    let (socket, broker, server) = serve_log(&lane, name);
    bridge.cfg.broker = socket.clone();
    assert!(bridge.attach_broker());
    let face = format!(
        "/f/{}/in/{}/s-b/{}/note",
        bridge.cfg.fleet, bridge.node, bridge.node
    );
    (lane, bridge, socket, face, (broker, server))
}

#[cfg(unix)]
fn retired_off(lane: &FakeVerbLane, id: u64) -> u64 {
    lane.sent
        .lock()
        .unwrap()
        .iter()
        .filter(|line| line.split_whitespace().nth(3) == Some(&id.to_string()))
        .find_map(|line| {
            line.split_whitespace()
                .find_map(|word| word.strip_prefix("off="))
                .and_then(|off| off.parse().ok())
        })
        .unwrap_or_else(|| panic!("post {id} was not retired as landed"))
}

#[cfg(unix)]
fn own_lane_post_costs(per_drain: u64, drains: u64) -> crate::state::FileOps {
    const A: &str = "s-a";
    const B: &str = "s-b";
    let (lane, mut bridge, _socket, _face, _broker) = local_pair_world("costs");
    let frame = |ids: std::ops::Range<u64>| -> String {
        ids.map(|id| format!("post sid={A} id={id} to=@{B} kind=note len=4\nping"))
            .collect()
    };
    *lane.outbox.lock().unwrap() = Some(frame(1..2));
    bridge.drain_outbox();
    bridge.seq_block = None;
    bridge.next_seq().unwrap();
    bridge.unsent = None;
    let before = bridge.state.ops();
    let mut next = 2;
    for _ in 0..drains {
        *lane.outbox.lock().unwrap() = Some(frame(next..next + per_drain));
        next += per_drain;
        bridge.drain_outbox();
    }
    let spent = bridge.state.ops() - before;
    let acked = bridge.state.self_acked();
    for id in 1..next {
        let off = retired_off(&lane, id);
        assert!(acked.contains(&off), "@{off} is not in acked: {acked:?}");
    }
    assert!(bridge.state.in_flight().unwrap().is_empty());
    for id in 1..next {
        assert_eq!(
            bridge.state.post_pin(A, id).unwrap(),
            None,
            "a retired post left its reservation"
        );
    }
    assert!(
        !bridge.state.root().join("sent").exists()
            && !bridge.state.root().join("inflight").exists(),
        "no file a post"
    );
    spent
}

/// WHAT A LOCAL POST COSTS THE STATE DIR (B19): four records of the post journal (its
/// reservation, its record in flight, that record landed, the reservation retired) and the
/// landed offset appended to `acked`, and nothing renamed, unlinked or read: four appends,
/// the landing and the retirement one write (critic item E07, `Bridge::land_group`). Each
/// post's ack is read alone here (a broker that does not gap-fence: a sender's post waits
/// for its last one's answer), so a landing group holds one post. Nothing is opened either:
/// the first post opened the journal and `acked`, and each is held open for the posts after
/// it (D05: `acked` was opened, checked and closed at every landing).
#[cfg(unix)]
#[test]
fn a_local_post_costs_the_state_dir_a_counted_set_of_operations() {
    for (per_drain, drains) in [(1, 8), (8, 1)] {
        let posts = per_drain * drains;
        let spent = own_lane_post_costs(per_drain, drains);
        assert_eq!(
            spent,
            crate::state::FileOps {
                reads: 0,
                replaces: 0,
                renames: 0,
                fsyncs: 0,
                barriers: 0,
                unlinks: 0,
                appends: 4 * posts,
                opens: 0,
            },
            "{drains} drain(s) of {per_drain} post(s)"
        );
    }
}

#[cfg(unix)]
#[test]
fn the_tallys_state_dir_rows_count_what_the_dir_did_and_an_off_tally_counts_none() {
    use crate::wait_tally::{Row, Tally, FILE};
    const A: &str = "s-a";
    const B: &str = "s-b";
    for on in [true, false] {
        let (lane, mut bridge, _socket, _face, _broker) = local_pair_world("tally-dir");
        let path = bridge.state.root().join(FILE);
        if on {
            bridge.tally_to(&path);
        }
        let before = bridge.state.ops();
        let (drains, per_drain) = (3u64, 2u64);
        let mut next = 1;
        for _ in 0..drains {
            *lane.outbox.lock().unwrap() = Some(
                (next..next + per_drain)
                    .map(|id| format!("post sid={A} id={id} to=@{B} kind=note len=4\nping"))
                    .collect(),
            );
            next += per_drain;
            bridge.drain_outbox();
        }
        let posts = drains * per_drain;
        let spent = bridge.state.ops() - before;
        assert_eq!(
            (spent.unlinks, spent.appends),
            (0, 4 * posts),
            "on={on}: what a local post costs the dir"
        );
        bridge.tally_idle();
        if !on {
            assert!(!path.exists(), "an off tally writes nothing");
            let mut kept = Tally::default();
            bridge.state.copy_dir_rows(&mut kept);
            assert_eq!(kept, Tally::default(), "and its dir counts nothing");
            continue;
        }
        let t = Tally::parse(&std::fs::read_to_string(&path).unwrap()).unwrap();
        let n = |r: Row| t.row(r).count;
        assert!(t.dir, "a test build counts the dir");
        assert_eq!(
            (n(Row::DirReplace), n(Row::DirUnlink), n(Row::DirAppend)),
            (spent.replaces, spent.unlinks, spent.appends),
            "each kind row is the dir's own count"
        );
        // Each post's ack read alone, each landing a group of one (critic item E07): its
        // entry's end is its group's settle, counted with the retirement.
        for row in [
            Row::SetPostPin,
            Row::SetInFlight,
            Row::ClearPostSeq,
            Row::AppendSelfAcked,
        ] {
            assert_eq!(n(row), posts, "{row:?}: one a post");
        }
        assert_eq!(n(Row::ClearInFlight), 0, "no entry ends alone");
        assert_eq!(n(Row::LaneOutbox), drains, "a peek a drain");
        assert_eq!(n(Row::LaneSessions), drains, "a roster read a drain");
        assert_eq!(n(Row::LaneSent), posts, "a retirement a post");
        assert_eq!(
            (n(Row::DrainPosts), t.row(Row::DrainPosts).total),
            (drains, posts),
            "each peek's posts"
        );
        assert_eq!(
            n(Row::PostPublish),
            0,
            "no `post` event ran these drains: the path is an event's"
        );
    }
}

#[cfg(unix)]
#[test]
fn a_post_events_path_is_timed_to_each_publish_and_its_event_counted_as_a_wake_or_queued() {
    use crate::wait_tally::{Row, Tally, FILE};
    let (lane, mut bridge) = FakeVerbLane::bridge(&[(1, "s-a"), (2, "s-b")]);
    assert!(bridge.refresh_sessions().is_ok());
    let (socket, _broker, _server) = serve_log(&lane, "tally-path");
    bridge.cfg.broker = socket;
    bridge.conn = Some(bridge.connect().unwrap().0.into());
    let node = bridge.node.clone();
    bridge
        .indexed_subjects
        .insert(subject::post_lane("f", &node, "s-b", &node, "note"));
    lane.retire_from_outbox();
    let path = bridge.state.root().join(FILE);
    bridge.tally_to(&path);
    let outbox = |ids: std::ops::Range<u64>| {
        *lane.outbox.lock().unwrap() = Some(
            ids.map(|id| queued_note("s-a", id, "@s-b", "ping"))
                .collect(),
        );
    };
    let tally = |bridge: &mut Bridge| {
        run_rounds(bridge);
        bridge.settle_acks();
        bridge.tally_idle();
        Tally::parse(&std::fs::read_to_string(&path).unwrap()).unwrap()
    };
    let rows = |t: &Tally| {
        [Row::PostPublish, Row::QueuedEvent, Row::WakeEvent].map(|row| t.row(row).count)
    };

    outbox(1..3);
    bridge.mailbox.push_event("EVENT 1 post 1".to_string());
    let queued = tally(&mut bridge);
    assert_eq!(
        rows(&queued),
        [2, 1, 0],
        "a path a fresh post, a queued event"
    );

    outbox(3..4);
    bridge.mailbox.push_event("EVENT 1 fetch off=9".to_string());
    let fetched = tally(&mut bridge);
    assert_eq!(rows(&fetched), [2, 2, 0], "a fetch's drain times no path");

    assert_eq!(
        bridge.conn.as_ref().map(Link::unacked),
        Some(0),
        "PRECONDITION: nothing owed, so the park is the whole wait"
    );
    outbox(4..5);
    let mailbox = bridge.mailbox_handle();
    let pusher = std::thread::spawn(move || {
        aterm_spec::wait::until_within(
            aterm_spec::wait::MUST_HAPPEN,
            "the loop parked on its mailbox",
            || mailbox.parked().then_some(()),
        );
        mailbox.push_event("EVENT 1 post 4".to_string());
    });
    assert!(bridge.round(aterm_spec::wait::MUST_HAPPEN));
    pusher.join().expect("the pusher");
    let woke = tally(&mut bridge);
    assert_eq!(rows(&woke), [3, 2, 1], "a wake, and its post's path");
    assert_eq!(
        retirements(&lane)
            .iter()
            .filter(|(_, _, off)| off.is_some())
            .count(),
        4,
        "every post landed: {:?}",
        retirements(&lane)
    );
    assert!(
        woke.row(Row::PostPublish).total > 0,
        "a path is timed, not only counted"
    );
}

#[cfg(unix)]
#[test]
fn acked_is_replaced_whole_after_a_failed_write_and_at_twice_its_keep() {
    let (lane, mut bridge, _socket, _face, _broker) = local_pair_world("acked-whole");
    let acked = bridge.state.root().join("acked");
    let state = bridge.state.root().to_path_buf();
    // Whether `acked` took the landed offset by one append (its text grown by that offset
    // alone, after the window's floor where the file named none, nothing replaced), and how
    // many replaces the drain made.
    let post = |lane: &FakeVerbLane, bridge: &mut Bridge, id: u64| {
        *lane.outbox.lock().unwrap() = Some(format!(
            "post sid=s-a id={id} to=@s-b kind=note len=4\nping"
        ));
        let text = || std::fs::read_to_string(&acked).unwrap_or_default();
        let (before, was) = (bridge.state.ops(), text());
        let named = match bridge.self_floor {
            Some(floor) if !was.contains("floor=") => format!("floor={floor},"),
            _ => String::new(),
        };
        bridge.drain_outbox();
        let spent = bridge.state.ops() - before;
        let off = retired_off(lane, id);
        let appended = spent.replaces == 0 && text() == format!("{was}{named}{off},");
        (off, u64::from(appended), spent.replaces)
    };
    let (first, appends, replaces) = post(&lane, &mut bridge, 1);
    assert_eq!((appends, replaces), (1, 0), "an ordinary append");

    bridge.state.refuse_writes_of(Some("acked"));
    let (second, _, _) = post(&lane, &mut bridge, 2);
    assert!(!bridge.acked_appendable, "the write failed");
    assert_eq!(
        bridge.state.in_flight().unwrap().len(),
        1,
        "the entry stays while acked does not hold its offset"
    );
    bridge.state.refuse_writes_of(None);
    let (third, appends, replaces) = post(&lane, &mut bridge, 3);
    assert_eq!((appends, replaces), (0, 1), "replaced whole");
    let on_disk = bridge.state.self_acked();
    assert!(
        [first, second, third]
            .iter()
            .all(|off| on_disk.contains(off)),
        "{on_disk:?}"
    );

    bridge.acked_entries = 2 * SELF_ACK_KEEP;
    let (fourth, appends, replaces) = post(&lane, &mut bridge, 4);
    assert_eq!((appends, replaces), (0, 1), "compacted");
    assert_eq!(bridge.acked_entries, bridge.self_acked.len());
    assert!(bridge.state.self_acked().contains(&fourth));
    drop(bridge);

    let window: u64 = SELF_ACK_KEEP as u64;
    let mut log = String::new();
    for off in 0..2 * window {
        log.push_str(&format!("{off},"));
    }
    std::fs::write(&acked, log).unwrap();
    let (_second_lane, bridge) =
        FakeVerbLane::bridge_with_state(&[(1, "s-a"), (2, "s-b")], Some(&state));
    assert_eq!(
        (bridge.self_acked.clone(), bridge.self_floor),
        (
            (window..2 * window).collect::<BTreeSet<u64>>(),
            Some(window)
        ),
        "the newest window of a longer log an older build wrote, at its oldest kept"
    );
    assert_eq!(bridge.acked_entries, 2 * SELF_ACK_KEEP);
    drop(bridge);

    std::fs::write(&acked, "7,9,").unwrap();
    let (_third_lane, mut bridge) =
        FakeVerbLane::bridge_with_state(&[(1, "s-a"), (2, "s-b")], Some(&state));
    let before = bridge.state.ops();
    assert!(bridge.remember_self_ack(11).is_ok());
    assert_eq!(
        (
            (bridge.state.ops() - before).replaces,
            std::fs::read_to_string(&acked).unwrap()
        ),
        (1, "floor=7,7,9,11,".to_string()),
        "an older build's file, short of its compaction, is replaced whole to name its floor"
    );
}

#[cfg(unix)]
#[test]
fn a_local_post_that_landed_is_not_published_again_on_the_next_log() {
    let (lane, mut bridge, _socket, face, _log_a) = local_pair_world("landed-a");
    let state = bridge.state.root().to_path_buf();
    *lane.outbox.lock().unwrap() =
        Some("post sid=s-a id=1 to=@s-b kind=note len=4\nping".to_string());
    bridge.drain_outbox();
    let off = retired_off(&lane, 1);
    assert!(
        bridge.state.self_acked().contains(&off),
        "acked holds the landed offset before anything else happens"
    );
    drop(bridge);

    let (_second, mut bridge) =
        FakeVerbLane::bridge_with_state(&[(1, "s-a"), (2, "s-b")], Some(&state));
    let (b, _broker_b, _server_b) = serve_log(&lane, "landed-b");
    bridge.cfg.broker = b;
    assert!(bridge.attach_broker());
    let (rows, _) = bridge.conn_now().unwrap().fetch(0, &face, 8).unwrap();
    assert!(
        rows.is_empty(),
        "a record that landed on the old log was published again: {rows:?}"
    );
}

#[cfg(unix)]
#[test]
fn an_own_lane_posts_reservation_stays_readable_by_an_older_bridge() {
    let (lane, mut bridge, _socket, _face, _log) = local_pair_world("older-reader");
    lane.refuse_retirements(true);
    *lane.outbox.lock().unwrap() =
        Some("post sid=s-a id=1 to=@s-b kind=note len=4\nping".to_string());
    bridge.drain_outbox();
    assert!(lane.sent.lock().unwrap().is_empty(), "left listed");
    let pin = bridge.state.post_pin("s-a", 1).unwrap();
    assert!(pin.is_some(), "the journal holds the reservation");
    bridge.end_run();
    let sent = bridge.state.root().join("sent");
    let mut names = Vec::new();
    for entry in std::fs::read_dir(&sent).unwrap() {
        let entry = entry.unwrap();
        let name = entry.file_name().to_string_lossy().into_owned();
        if name.starts_with('.') {
            continue;
        }
        let text = std::fs::read_to_string(entry.path()).unwrap();
        if let Err(e) = PublishPin::parse(&text) {
            panic!("an older bridge cannot read sent/{name} ({e}): {text:?}");
        }
        names.push(name);
    }
    assert_eq!(names, ["s-a.1"], "the reservation is kept");
    let inflight = bridge.state.root().join("inflight");
    if let Ok(entries) = std::fs::read_dir(&inflight) {
        for entry in entries.map(Result::unwrap) {
            let name = entry.file_name().to_string_lossy().into_owned();
            if name.starts_with('.') {
                continue;
            }
            let seq: u64 = name.parse().unwrap();
            let text = std::fs::read_to_string(entry.path()).unwrap();
            assert!(InFlight::parse(seq, &text).is_some(), "inflight/{name}");
        }
    }
    assert!(
        !bridge
            .state
            .root()
            .join(crate::state::journal::FILE)
            .exists(),
        "the stop unlinked the journal, so a later bridge of this build reads the files"
    );
    let next = StateDir::open(bridge.state.root()).unwrap();
    assert_eq!(next.post_pin("s-a", 1).unwrap(), pin, "and takes it back");
    assert!(!sent.join("s-a.1").exists());
}

#[cfg(unix)]
fn serve_log(
    lane: &FakeVerbLane,
    name: &str,
) -> (String, astream_broker::Broker, astream_broker::BrokerHandle) {
    let socket = lane.dir.path().join(format!("{name}.sock"));
    let broker = crate::test_broker(lane.dir.path().join(format!("{name}.log"))).unwrap();
    let server = broker.serve(&socket).unwrap();
    (socket.to_string_lossy().into_owned(), broker, server)
}

#[cfg(unix)]
fn node_evs(bridge: &Bridge) -> Vec<String> {
    let (mut reader, _) = bridge.connect().unwrap();
    let face = subject::node_face(&bridge.cfg.fleet, &bridge.node, "ev");
    let (rows, _) = reader.fetch(0, &face, 256).unwrap();
    rows.iter()
        .map(|(_, _, b)| crate::pct::decode(&String::from_utf8_lossy(b)))
        .collect()
}

#[cfg(unix)]
fn next_delivery<S: std::io::Read + std::io::Write>(
    sub: &mut astream_broker::Subscription<S>,
    what: &str,
) -> BrokerRecord {
    let until = Instant::now() + aterm_spec::wait::MUST_HAPPEN;
    loop {
        match sub.recv_event().expect(what) {
            Some(astream_broker::Event::Delivery {
                offset,
                subject,
                body,
            }) => return (offset, subject, body),
            Some(astream_broker::Event::Mark { .. }) => assert!(
                Instant::now() < until,
                "{what}: nothing delivered within {:?}",
                aterm_spec::wait::MUST_HAPPEN
            ),
            Some(astream_broker::Event::Gap(gap)) => {
                panic!("{what}: retention gave up records below the tail: {gap:?}")
            }
            None => panic!("{what}: the broker ended the subscription"),
        }
    }
}

#[cfg(unix)]
#[test]
fn a_halt_published_during_the_attach_refill_is_applied_before_it_ends() {
    const SID: &str = "s-busy";
    let (lane, mut bridge) = FakeVerbLane::bridge(&[(1, SID)]);
    let (socket, _broker, _server) = serve_log(&lane, "broker");
    bridge.cfg.broker = socket;
    let (mut writer, _) = bridge.connect().unwrap();
    let mail = format!("/f/f/in/{}/{SID}/h-writer/note", bridge.node);
    let (one, _) = writer.publish(81, 1, &mail, b"v=1 t=1 text=one").unwrap();
    let (two, _) = writer.publish(81, 2, &mail, b"v=1 t=2 text=two").unwrap();
    assert!(bridge.refresh_sessions().is_ok());
    assert!(
        bridge.per_sid.pending_admit.contains(SID),
        "the attach owes it a refill"
    );

    let mailbox = Arc::clone(&bridge.mailbox);
    let (mut human, _) = bridge.connect().unwrap();
    lane.on_first_deliver(move || {
        human
            .publish(
                91,
                1,
                "/f/f/fleet/h-t/halt",
                b"v=1 t=3 state=on reason=stop",
            )
            .unwrap();
        aterm_spec::wait::until_within(
            aterm_spec::wait::MUST_HAPPEN,
            "the fleet reader to queue the halt",
            || (mailbox.replay_pressure() == ReplayPressure::Urgent).then_some(()),
        );
    });
    assert!(bridge.attach_broker());
    let t = lane.transcript();
    let at = |line: &str| {
        t.iter()
            .position(|l| l == line)
            .unwrap_or_else(|| panic!("no `{line}` in {t:#?}"))
    };
    let held = at(&format!("hold {SID} on reason=stop origin=fleet"));
    assert!(at(&format!("deliver {one}")) < held, "{t:#?}");
    assert!(
        held < at(&format!("deliver {two}")),
        "the halt waited for the refill: {t:#?}"
    );
    assert!(bridge.halt_applied);
    assert!(
        !bridge.per_sid.pending_admit.contains(SID),
        "and the refill finished"
    );
}

/// A9 — WHAT THE BROKER GAVE UP UNREAD IS TOLD TO ATERM: five mails to this node, never
/// read, then three `ev` records, all of 4 KiB; the broker's rules keep two `ev` records'
/// bytes and three mails' bytes of unread mail below the cut, so the tick gives up the two
/// oldest mails. Attached, the inbox group is told the range and the count before its first
/// mail, and the bridge says `link gap from=<cursor> to=<oldest held> records=2` on the lane,
/// once. An aterm older than the `link gap` lane is told nothing (its mail still comes).
/// NC (run): a reader that drops the notice tells aterm nothing.
#[cfg(unix)]
#[test]
fn unread_mail_the_broker_gave_up_is_told_to_aterm_as_a_link_gap() {
    use crate::broker_rules::{EV_FILTER, MAIL_FILTER, MAIL_READER};
    use astream_broker::{Retention, Rule};
    const SID: &str = "s-gap";
    const BODY: u64 = 4096;
    let run = |endpoint_lane: u32| -> (Vec<u64>, Vec<u64>, Vec<String>) {
        let (lane, mut bridge) = FakeVerbLane::bridge(&[(1, SID)]);
        let (socket, broker, _server) = serve_log(&lane, "broker");
        bridge.cfg.broker = socket;
        bridge.endpoint_lane = endpoint_lane;
        let (mut writer, _) = bridge.connect().unwrap();
        let mail = format!("/f/f/in/{}/{SID}/h-writer/note", bridge.node);
        let text = format!("v=1 t=1 text={}", "m".repeat(BODY as usize));
        let offs: Vec<u64> = (1..=5)
            .map(|seq| writer.publish(81, seq, &mail, text.as_bytes()).unwrap().0)
            .collect();
        let filler = vec![b'e'; BODY as usize];
        for seq in 1..=3 {
            writer
                .publish(82, seq, "/f/f/pub/n-fill/s-fill/ev", &filler)
                .unwrap();
        }
        broker.set_retention_interval(Duration::from_secs(3600));
        let rules = Retention::new(
            vec![
                Rule::prefix(EV_FILTER).unwrap().max_bytes(5 * BODY / 2),
                Rule::hold(MAIL_FILTER)
                    .unwrap()
                    .max_bytes(7 * BODY / 2)
                    .until_read(MAIL_READER)
                    .unwrap(),
            ],
            Rule::default_keeping_all(),
        )
        .unwrap();
        broker.set_retention(rules).unwrap();
        broker.retention_tick_at(broker.now_ms()).unwrap();
        let mails = astream_wire::Filter::new(MAIL_FILTER).unwrap();
        assert_eq!(
            broker.oldest(&mails),
            offs[2],
            "the two oldest mails given up"
        );

        assert!(bridge.refresh_sessions().is_ok());
        assert!(bridge.attach_broker());
        aterm_spec::wait::until_within(
            aterm_spec::wait::MUST_HAPPEN,
            "the bridge to deliver the three held mails",
            || {
                assert!(bridge.round(Duration::from_millis(20)), "the lane closed");
                (lane.delivery_offsets().len() >= 3).then_some(())
            },
        );
        let gaps = lane
            .links()
            .into_iter()
            .filter(|l| l.starts_with("link gap "))
            .map(|l| l.trim_end().to_string())
            .collect();
        let delivered = lane.delivery_offsets();
        bridge.close_subscriptions();
        (offs, delivered, gaps)
    };
    let (offs, delivered, gaps) = run(aterm_types::control_verbs::BRIDGE_LANE);
    assert_eq!(delivered, offs[2..].to_vec());
    assert_eq!(
        gaps,
        [format!(
            "link gap from={} to={} records=2",
            offs[0], offs[2]
        )],
        "told once, with the broker's count"
    );
    let (offs, delivered, gaps) = run(LINK_GAP_LANE - 1);
    assert_eq!(
        delivered,
        offs[2..].to_vec(),
        "an older aterm's mail still comes"
    );
    assert!(gaps.is_empty(), "an older aterm is told nothing: {gaps:?}");
}

#[cfg(unix)]
#[test]
fn a_replaced_log_keeps_the_halts_it_lost_until_their_human_writes_a_row() {
    const SID: &str = "s-held";
    let held = format!("hold {SID} on reason=stop origin=fleet");
    let lifted = format!("hold {SID} off origin=fleet");
    let (lane, mut bridge) = FakeVerbLane::bridge(&[(1, SID)]);
    assert!(bridge.refresh_sessions().is_ok());

    let (a, _broker_a, _server_a) = serve_log(&lane, "a");
    bridge.cfg.broker = a;
    let (mut human, _) = bridge.connect().unwrap();
    for seq in 1..=64 {
        human.publish(92, seq, "/f/f/x/history", b"v=1").unwrap();
    }
    human
        .publish(
            91,
            1,
            "/f/f/fleet/h-x/halt",
            b"v=1 t=1 state=on reason=stop",
        )
        .unwrap();
    assert!(bridge.attach_broker());
    assert!(bridge.halt_applied);
    assert!(lane.transcript().contains(&held));
    assert!(bridge.state.halt_acked().is_some(), "A's halt is answered");
    let high_on_a = bridge.state.log_witness().expect("a witness").high;

    let (b, _broker_b, _server_b) = serve_log(&lane, "b");
    bridge.close_subscriptions();
    bridge.conn = None;
    bridge.cfg.broker = b;
    let before = lane.transcript().len();
    assert!(bridge.attach_broker());
    let attach_b = lane.transcript()[before..].to_vec();
    assert!(
        !attach_b.contains(&lifted),
        "a fresh log lifted the halt: {attach_b:#?}"
    );
    assert!(bridge.halt_applied);
    assert_eq!(bridge.kept_halts.keys().collect::<Vec<_>>(), ["h-x"]);
    assert_eq!(bridge.state.halt_acked(), None, "A's watermark is not B's");
    assert!(bridge.state.log_witness().expect("re-based").high < high_on_a);
    let evs = node_evs(&bridge);
    assert!(
        evs.iter().any(|e| e.contains("ev=log-replaced head="))
            && evs.iter().any(|e| e.contains("ev=halt-kept by=h-x")),
        "{evs:#?}"
    );

    bridge.halts.clear();
    bridge.kept_halts.clear();
    bridge.halt_applied = false;
    bridge.close_subscriptions();
    bridge.conn = None;
    assert!(bridge.attach_broker());
    assert!(bridge.halt_applied, "the halt went with the memory");
    assert_eq!(lane.transcript().last(), Some(&held));

    let (mut human_b, _) = bridge.connect().unwrap();
    let body = b"v=1 t=2 state=off";
    let (off, _) = human_b.publish(91, 1, "/f/f/fleet/h-x/halt", body).unwrap();
    bridge.on_fleet_record(&(off, "/f/f/fleet/h-x/halt".to_string(), body.to_vec()));
    assert!(!bridge.halt_applied);
    assert_eq!(lane.transcript().last(), Some(&lifted));
    assert!(bridge.kept_halts.is_empty());
    assert!(bridge.state.standing_halts("f").is_empty());
}

#[cfg(unix)]
#[test]
fn a_log_with_another_identity_has_every_old_offset_forgotten_however_long_it_is() {
    const SID: &str = "s-moved";
    let (lane, mut bridge) = FakeVerbLane::bridge(&[(1, SID)]);
    assert!(bridge.refresh_sessions().is_ok());
    bridge.endpoint_lane = aterm_types::control_verbs::BRIDGE_LANE;
    let halt = "/f/f/fleet/h-x/halt";

    let (a, broker_a, _server_a) = serve_log(&lane, "a");
    bridge.cfg.broker = a;
    let (mut human, _) = bridge.connect().unwrap();
    for seq in 1..=8 {
        human.publish(92, seq, "/f/f/x/history", b"v=1").unwrap();
    }
    let (a_halt, _) = human
        .publish(91, 1, halt, b"v=1 t=1 state=on reason=stop")
        .unwrap();
    assert!(bridge.attach_broker());
    assert_eq!(
        bridge.state.halt_acked(),
        Some(a_halt),
        "A's halt is answered"
    );
    let a_id = broker_a.log_identity();
    bridge.state.set_seen_off(SID, a_halt).unwrap();
    lane.opt_in(SID, &["topic build since=head"]);
    let topic = BTreeMap::from([("build".to_string(), TopicCursor { next: a_halt })]);
    bridge.state.set_topics(SID, &topic).unwrap();
    bridge.per_sid.topics.insert(SID.to_string(), topic);
    bridge.note_asked(a_halt, "n-peer");
    bridge.note_deadline(SID, a_halt, 60_000, Some("n-peer".to_string()));
    assert!(!bridge.deadlines.is_empty() && !bridge.state.asked().is_empty());

    let (b, broker_b, _server_b) = serve_log(&lane, "b");
    let b_id = broker_b.log_identity();
    assert_ne!(a_id, b_id);
    bridge.close_subscriptions();
    bridge.conn = None;
    bridge.cfg.broker = b;
    let (mut human_b, _) = bridge.connect().unwrap();
    let (b_halt, _) = human_b
        .publish(
            93,
            1,
            "/f/f/fleet/h-y/halt",
            b"v=1 t=2 state=on reason=again",
        )
        .unwrap();
    assert!(b_halt < a_halt);
    for seq in 1..=64 {
        human_b.publish(92, seq, "/f/f/x/history", b"v=1").unwrap();
    }
    let (_, (_, b_head)) = human_b.fetch(0, "/f/f/>", 0).unwrap();
    assert!(
        b_head > bridge.state.log_witness().expect("A's witness").high,
        "B's head alone cannot say it is another log"
    );
    assert!(bridge.attach_broker());

    assert_eq!(
        bridge.state.halt_acked(),
        Some(b_halt),
        "B's halt is answered"
    );
    let (mut reader, _) = bridge.connect().unwrap();
    let ack = subject::node_face("f", &bridge.node, "ack");
    let (rows, _) = reader.last(&ack, "", 8).unwrap();
    assert!(
        rows.iter()
            .any(|(_, s, b)| *s == ack
                && String::from_utf8_lossy(b).contains(&format!(" re={b_halt} "))),
        "no ack of B's halt on B: {rows:?}"
    );
    assert_eq!(bridge.state.seen_off(SID), None, "A's watermark is not B's");
    let resumed = bridge.state.topics(SID);
    assert!(
        resumed.get("build").is_some_and(|c| c.next >= b_head)
            && bridge.per_sid.topics[SID]["build"].next >= b_head,
        "{resumed:?}"
    );
    assert!(
        bridge.self_floor.is_some_and(|floor| floor >= b_head)
            && bridge.self_acked.first().is_none_or(|w| *w >= b_head),
        "the self-lane window starts at B's head: floor {:?}, {:?}",
        bridge.self_floor,
        bridge.self_acked
    );
    assert!(bridge.deadlines.is_empty() && bridge.asked.is_empty());
    assert!(bridge.state.deadlines().is_empty() && bridge.state.asked().is_empty());

    let links = lane.links.lock().unwrap().clone();
    let told =
        |id: astream_broker::LogIdentity| links.iter().position(|l| *l == format!("link log {id}"));
    assert!(
        told(a_id) < told(b_id) && told(b_id).is_some(),
        "{links:#?}"
    );
    let evs = node_evs(&bridge);
    assert!(
        evs.iter().any(|e| e.contains("ev=log-replaced head=")
            && e.contains(&format!(" log={b_id} was_log={a_id}"))
            && e.contains(" deadlines=1")),
        "{evs:#?}"
    );
    let witness = bridge.state.log_witness().expect("re-based");
    assert_eq!(witness.id, Some(b_id));
    assert!(witness.high >= b_head);
    assert!(bridge.log_replaced.is_none(), "the re-base is finished");

    bridge.state.set_seen_off(SID, b_halt).unwrap();
    bridge.close_subscriptions();
    bridge.conn = None;
    assert!(bridge.attach_broker());
    assert_eq!(bridge.state.seen_off(SID), Some(b_halt));

    let (c, broker_c, _server_c) = serve_log(&lane, "c");
    let witness = bridge.state.root().join("bus-log");
    std::fs::remove_file(&witness).unwrap();
    std::fs::create_dir(&witness).unwrap();
    bridge.close_subscriptions();
    bridge.conn = None;
    bridge.cfg.broker = c;
    assert!(
        !bridge.attach_broker(),
        "no attach past an unwritten witness"
    );
    assert!(bridge.conn.is_none(), "and no connection is kept");
    assert!(bridge.log_replaced.is_some(), "the re-base is owed");
    bridge.state.set_seen_off(SID, 1).unwrap();
    std::fs::remove_dir(&witness).unwrap();
    bridge.close_subscriptions();
    bridge.conn = None;
    assert!(bridge.attach_broker());
    assert_eq!(bridge.state.seen_off(SID), Some(1), "C's own offset stands");
    assert!(bridge.log_replaced.is_none(), "and the re-base is finished");
    assert_eq!(
        bridge.state.log_witness().expect("written").id,
        Some(broker_c.log_identity())
    );
}

#[cfg(unix)]
#[test]
fn a_known_log_identity_that_rewinds_is_refused_before_any_clock_mutation() {
    const SID: &str = "s-kept";
    let (lane, mut bridge) = FakeVerbLane::bridge(&[(1, SID)]);
    lane.answer_hello_with(&format!("OK lane={LINK_LOG_LANE} build=test\n"));
    bridge.begin_run();
    bridge.introduce();
    assert!(bridge.refresh_sessions().is_ok());
    let (socket, broker, _server) = serve_log(&lane, "rewound");
    bridge.cfg.broker = socket.clone();
    let (mut writer, _) = bridge.connect().unwrap();
    for seq in 1..=10 {
        writer.publish(81, seq, "/f/f/x/history", b"v=1").unwrap();
    }
    assert_eq!(broker.head(), 10, "the actual broker head");
    let log = writer.log_identity().unwrap().unwrap();
    assert_eq!(log, broker.log_identity(), "the actual publisher identity");

    let witnessed = LogWitness {
        high: 20,
        id: Some(log),
    };
    bridge.state.set_log_witness(&witnessed).unwrap();
    bridge.log_witness = witnessed;
    bridge.bus_log = Some(log);
    bridge.bus_high = 20;
    bridge.state.set_seen_off(SID, 17).unwrap();
    bridge
        .state
        .set_topics(
            SID,
            &BTreeMap::from([("desk".into(), TopicCursor { next: 18 })]),
        )
        .unwrap();
    let subject = format!("/f/f/in/{}/{SID}/{}/ack", bridge.node, bridge.node);
    let body = b"v=1 t=1 re=15 verdict=handled";
    let mut pin = plain_pin(bridge.next_seq().unwrap(), &subject, body);
    pin.original_off = Some(15);
    bridge.state.set_key_pin(SID, "kept", &pin).unwrap();
    bridge.state.set_post_pin(SID, 1, &pin).unwrap();
    bridge
        .state
        .set_in_flight(&InFlight {
            pin: pin.clone(),
            subject,
            body: body.to_vec(),
            log: Some(log),
            at: None,
        })
        .unwrap();
    let files = |root: &std::path::Path| {
        let mut todo = vec![root.to_path_buf()];
        let mut files = BTreeMap::new();
        while let Some(dir) = todo.pop() {
            for entry in std::fs::read_dir(dir).unwrap() {
                let entry = entry.unwrap();
                if entry.file_type().unwrap().is_dir() {
                    todo.push(entry.path());
                } else {
                    // The only new facts a refusal may record: that this
                    // identity can no longer vouch for its old offsets, and
                    // the run's OPEN fence the attach's admission writes
                    // before any broker exchange (768104f73e).
                    if entry.path() != root.join("rewound-log") && entry.path() != root.join("run")
                    {
                        files.insert(entry.path(), std::fs::read(entry.path()).unwrap());
                    }
                }
            }
        }
        files
    };
    let before = files(bridge.state.root());
    let start = lane.links.lock().unwrap().len();
    assert!(
        !bridge.attach_broker(),
        "the same known identity rewound from 20 to 10"
    );
    assert_eq!(
        broker.head(),
        10,
        "no presence or old reference was published"
    );
    assert_eq!(files(bridge.state.root()), before, "all durable facts stay");
    let run = std::fs::read_to_string(bridge.state.root().join("run")).unwrap();
    assert!(
        run.starts_with("open "),
        "the run file is its OPEN fence: {run:?}"
    );
    assert_eq!(bridge.log_witness, witnessed, "the high-water is retained");
    assert!(
        lane.delivery_offsets().is_empty(),
        "no refill was delivered"
    );
    assert!(
        !lane.links.lock().unwrap()[start..]
            .iter()
            .any(|line| line.starts_with("link log ")),
        "no endpoint clock transition was acknowledged"
    );

    for seq in 11..=20 {
        writer.publish(81, seq, "/f/f/x/history", b"v=1").unwrap();
    }
    assert_eq!(broker.head(), 20);
    assert!(!bridge.attach_broker(), "regrowth does not repair identity");
    assert_eq!(broker.head(), 20, "regrowth publishes no recovery rows");
    assert_eq!(files(bridge.state.root()), before);
    let state_dir = bridge.state.root().to_path_buf();
    drop(bridge);
    let (next_lane, mut bridge) = FakeVerbLane::bridge_with_state(&[(1, SID)], Some(&state_dir));
    next_lane.answer_hello_with(&format!("OK lane={LINK_LOG_LANE} build=test\n"));
    bridge.introduce();
    assert!(bridge.refresh_sessions().is_ok());
    bridge.cfg.broker = socket;
    assert!(
        !bridge.attach_broker(),
        "a restarted bridge retains the fence"
    );
    assert_eq!(broker.head(), 20, "the restarted bridge publishes nothing");
    assert_eq!(files(bridge.state.root()), before);
    assert_eq!(bridge.state.log_witness(), Some(witnessed));
    assert!(next_lane
        .links
        .lock()
        .unwrap()
        .iter()
        .all(|line| !line.starts_with("link log ")));

    let (new_socket, new_broker, _new_server) = serve_log(&lane, "new-clock");
    assert_ne!(new_broker.log_identity(), log);
    bridge.cfg.broker = new_socket;
    assert!(
        bridge.attach_broker(),
        "a different known identity can rebase"
    );
    assert!(bridge.state.in_flight().unwrap().is_empty());
    assert_eq!(
        bridge
            .state
            .key_pin(SID, "kept")
            .unwrap()
            .unwrap()
            .original_off,
        None,
        "the old offset loses its authority on the new log"
    );
    assert_eq!(
        bridge.state.log_witness().unwrap().id,
        Some(new_broker.log_identity())
    );
    assert!(
        !state_dir.join("rewound-log").exists(),
        "a witnessed new identity releases the fence"
    );
}

#[cfg(unix)]
#[test]
fn a_power_loss_under_a_relaxed_broker_continues_the_log_at_the_next_boot() {
    use crate::durability::{self, Durability};
    const SID: &str = "s-kept";
    let cases = [
        (true, false, true),
        (true, true, true),
        (true, false, false),
        (true, true, false),
        (false, false, true),
        (false, true, true),
    ];
    for (reboot, regrow, lineage) in cases {
        let (lane, mut bridge) = FakeVerbLane::bridge(&[(1, SID)]);
        lane.answer_hello_with(&format!("OK lane={LINK_LOG_LANE} build=test\n"));
        bridge.introduce();
        assert!(bridge.refresh_sessions().is_ok());
        bridge.ignore_lineage = !lineage;
        let log = lane.dir.path().join("relaxed.log");
        let socket = lane.dir.path().join("relaxed.sock");
        bridge.cfg.broker = socket.to_string_lossy().into_owned();
        let case = format!("reboot={reboot} regrow={regrow} lineage={lineage}");

        let (before, high, cut) = {
            let opened = durability::open(
                &log,
                None,
                Durability::Relaxed,
                Some(durability::test_boot(1)),
            )
            .unwrap();
            opened.broker.arm_unclean_stop();
            let mut server = opened.broker.serve(&socket).unwrap();
            let (mut writer, _) = bridge.connect().unwrap();
            for seq in 1..=4 {
                writer.publish(81, seq, "/f/f/x/history", b"v=1").unwrap();
            }
            let cut = std::fs::metadata(&log).unwrap().len();
            for seq in 5..=10 {
                writer.publish(81, seq, "/f/f/x/history", b"v=1").unwrap();
            }
            lane.opt_in(SID, &["topic t since=head"]);
            assert!(bridge.attach_broker(), "{case}: the first attach");
            assert!(bridge.sync_say_face(), "{case}");
            let high = bridge.state.log_witness().expect("witnessed").high;
            assert!(high >= 10, "{case}: {high}");
            assert!(bridge.per_sid.topics[SID]["t"].next >= 10, "{case}");
            bridge.state.set_seen_off(SID, high - 1).unwrap();
            drop(writer);
            bridge.close_subscriptions();
            bridge.conn = None;
            server.shutdown();
            (opened.broker.log_identity(), high, cut)
        };
        std::fs::OpenOptions::new()
            .write(true)
            .open(&log)
            .unwrap()
            .set_len(cut)
            .unwrap();

        let boot = durability::test_boot(if reboot { 2 } else { 1 });
        let reopened = durability::open(&log, None, Durability::Relaxed, Some(boot)).unwrap();
        assert_eq!(reopened.broker.head(), 4, "{case}: a clean prefix");
        let _server = reopened.broker.serve(&socket).unwrap();
        let body = Body::new(0).encode();
        let (mut other, _) = bridge.connect().unwrap();
        let (at_fork, _) = other
            .publish(82, 1, "/f/f/pub/n-src/s-src/say/t", &body)
            .unwrap();
        assert_eq!(
            at_fork, 4,
            "{case}: the broadcast takes the first offset given up"
        );
        let mut seq = 2;
        if regrow {
            while reopened.broker.head() <= high {
                other.publish(82, seq, "/f/f/x/history", b"v=1").unwrap();
                seq += 1;
            }
        }
        bridge.conn = None;
        let now = reopened.broker.log_identity();
        if reboot {
            assert_eq!(
                (now.id, now.generation),
                (before.id, before.generation + 1),
                "{case}: the next generation of the same log"
            );
            assert!(
                bridge.attach_broker(),
                "{case}: a moved generation, attached"
            );
            assert_eq!(bridge.state.log_witness().unwrap().id, Some(now), "{case}");
            assert_eq!(bridge.state.rewound_log().unwrap(), None, "{case}");
            assert!(bridge.sync_say_face(), "{case}");
            let (sentinel, _) = other
                .publish(82, seq, "/f/f/pub/n-src/s-src/say/t", &body)
                .unwrap();
            let first = fan_in_next_say(&mut bridge);
            if lineage {
                assert_eq!(
                    bridge.state.seen_off(SID),
                    Some(3),
                    "{case}: clamped to the fork"
                );
                assert_eq!(
                    first, at_fork,
                    "{case}: the broadcast at the fork is delivered"
                );
                assert_eq!(fan_in_next_say(&mut bridge), sentinel, "{case}");
            } else {
                assert_eq!(bridge.state.seen_off(SID), None, "{case}: forgotten");
                assert_eq!(
                    first, sentinel,
                    "{case}: CONTROL: re-based at the head, the broadcast at the fork is skipped"
                );
            }
            bridge.close_subscriptions();
        } else if regrow {
            assert_eq!(now, before, "{case}: the same identity");
            assert!(bridge.attach_broker(), "{case}: nothing to tell it by");
            assert_eq!(
                bridge.state.seen_off(SID),
                Some(high - 1),
                "{case}: the stale watermark survives — the hazard the generation move closes"
            );
            bridge.close_subscriptions();
        } else {
            assert_eq!(now, before, "{case}: the same identity");
            assert!(!bridge.attach_broker(), "{case}: refused as rewound");
            assert_eq!(bridge.state.rewound_log().unwrap(), Some(before), "{case}");
        }
    }
}

#[cfg(unix)]
#[test]
fn a_moved_generation_of_a_log_refused_as_rewound_continues_nothing() {
    use crate::durability::{self, Durability};
    const SID: &str = "s-kept";
    let (lane, mut bridge) = FakeVerbLane::bridge(&[(1, SID)]);
    lane.answer_hello_with(&format!("OK lane={LINK_LOG_LANE} build=test\n"));
    bridge.introduce();
    assert!(bridge.refresh_sessions().is_ok());
    let log = lane.dir.path().join("relaxed.log");
    let socket = lane.dir.path().join("relaxed.sock");
    bridge.cfg.broker = socket.to_string_lossy().into_owned();

    let backup;
    let (before, high, cut) = {
        let opened = durability::open(
            &log,
            None,
            Durability::Relaxed,
            Some(durability::test_boot(1)),
        )
        .unwrap();
        let mut server = opened.broker.serve(&socket).unwrap();
        let (mut writer, _) = bridge.connect().unwrap();
        for seq in 1..=4 {
            writer.publish(81, seq, "/f/f/x/history", b"v=1").unwrap();
        }
        let cut = std::fs::metadata(&log).unwrap().len();
        backup = std::fs::read(astream_broker::identity_path(&log)).unwrap();
        for seq in 5..=10 {
            writer.publish(81, seq, "/f/f/x/history", b"v=1").unwrap();
        }
        lane.opt_in(SID, &["topic t since=head"]);
        assert!(bridge.attach_broker(), "the first attach");
        assert!(bridge.sync_say_face());
        let high = bridge.state.log_witness().expect("witnessed").high;
        assert!(high >= 10, "{high}");
        bridge.state.set_seen_off(SID, high - 1).unwrap();
        drop(writer);
        bridge.close_subscriptions();
        bridge.conn = None;
        server.shutdown();
        (opened.broker.log_identity(), high, cut)
    };
    std::fs::write(astream_broker::identity_path(&log), &backup).unwrap();
    std::fs::OpenOptions::new()
        .write(true)
        .open(&log)
        .unwrap()
        .set_len(cut)
        .unwrap();
    {
        let restored = durability::open(
            &log,
            None,
            Durability::Relaxed,
            Some(durability::test_boot(1)),
        )
        .unwrap();
        restored.broker.arm_unclean_stop();
        assert_eq!(
            restored.broker.log_identity(),
            before,
            "no move astream can see"
        );
        assert_eq!(restored.broker.head(), 4);
        let mut server = restored.broker.serve(&socket).unwrap();
        assert!(!bridge.attach_broker(), "refused as rewound");
        assert_eq!(bridge.state.rewound_log().unwrap(), Some(before));
        let (mut other, _) = bridge.connect().unwrap();
        let mut seq = 1;
        while restored.broker.head() <= high {
            other.publish(82, seq, "/f/f/x/history", b"v=1").unwrap();
            seq += 1;
        }
        drop(other);
        bridge.conn = None;
        server.shutdown();
    }
    let moved = durability::open(
        &log,
        None,
        Durability::Relaxed,
        Some(durability::test_boot(2)),
    )
    .unwrap();
    let now = moved.broker.log_identity();
    assert_eq!(
        (now.id, now.generation),
        (before.id, before.generation + 1),
        "the next generation of the restored log"
    );
    let fork = moved.broker.head();
    assert!(
        fork > high,
        "regrown past what this node saw: {fork} > {high}"
    );
    let _server = moved.broker.serve(&socket).unwrap();
    {
        let mut client = astream_broker::Client::connect(&socket).unwrap();
        assert_eq!(client.log_identity().unwrap(), Some(now));
        let lineage = client
            .log_lineage()
            .cloned()
            .expect("the Hello carries lineage");
        assert_eq!(
            lineage.steps().last(),
            Some(&(now.generation, fork)),
            "astream continues the regrown log below its head"
        );
    }
    assert!(
        bridge.attach_broker(),
        "another known identity lifts the refusal"
    );
    assert_eq!(bridge.state.rewound_log().unwrap(), None);
    assert_eq!(bridge.state.log_witness().unwrap().id, Some(now));
    assert_eq!(
        bridge.state.seen_off(SID),
        None,
        "forgotten: a clamp at {fork} would keep {} and skip the records posted after the \
         restore",
        high - 1
    );
    assert_eq!(
        bridge.per_sid.topics[SID]["t"].next, fork,
        "the topic re-based at the head, not resumed at a cursor the restore made a lie"
    );
    bridge.close_subscriptions();
}

#[cfg(unix)]
#[test]
fn an_in_flight_reply_below_the_fork_is_published_on_the_moved_generation() {
    use crate::durability::{self, Durability};
    const SID: &str = "s-kept";
    for lineage in [true, false] {
        let (lane, mut bridge) = FakeVerbLane::bridge(&[(1, SID)]);
        lane.answer_hello_with(&format!("OK lane={LINK_LOG_LANE} build=test\n"));
        bridge.introduce();
        assert!(bridge.refresh_sessions().is_ok());
        bridge.ignore_lineage = !lineage;
        let log = lane.dir.path().join("relaxed.log");
        let socket = lane.dir.path().join("relaxed.sock");
        bridge.cfg.broker = socket.to_string_lossy().into_owned();
        let subject = format!("/f/f/in/{}/{SID}/{}/ack", bridge.node, bridge.node);
        let body = b"v=1 t=1 re=1 verdict=handled".to_vec();

        let cut = {
            let opened = durability::open(
                &log,
                None,
                Durability::Relaxed,
                Some(durability::test_boot(1)),
            )
            .unwrap();
            opened.broker.arm_unclean_stop();
            let mut server = opened.broker.serve(&socket).unwrap();
            let (mut writer, _) = bridge.connect().unwrap();
            for seq in 1..=4 {
                writer.publish(81, seq, "/f/f/x/history", b"v=1").unwrap();
            }
            let cut = std::fs::metadata(&log).unwrap().len();
            for seq in 5..=10 {
                writer.publish(81, seq, "/f/f/x/history", b"v=1").unwrap();
            }
            assert!(
                bridge.attach_broker(),
                "lineage={lineage}: the first attach"
            );
            let pin = plain_pin(bridge.next_seq().unwrap(), &subject, &body);
            bridge
                .state
                .set_in_flight(&InFlight {
                    pin,
                    subject: subject.clone(),
                    body: body.clone(),
                    log: Some(opened.broker.log_identity()),
                    at: None,
                })
                .unwrap();
            drop(writer);
            bridge.close_subscriptions();
            bridge.conn = None;
            server.shutdown();
            cut
        };
        std::fs::OpenOptions::new()
            .write(true)
            .open(&log)
            .unwrap()
            .set_len(cut)
            .unwrap();
        let reopened = durability::open(
            &log,
            None,
            Durability::Relaxed,
            Some(durability::test_boot(2)),
        )
        .unwrap();
        assert_eq!(reopened.broker.head(), 4, "lineage={lineage}");
        let _server = reopened.broker.serve(&socket).unwrap();
        assert!(
            bridge.attach_broker(),
            "lineage={lineage}: the moved generation"
        );
        assert!(
            bridge.state.in_flight().unwrap().is_empty(),
            "lineage={lineage}: settled either way"
        );
        let (mut reader, _) = bridge.connect().unwrap();
        let (rows, _) = reader.fetch(0, &subject, 64).unwrap();
        let replies: Vec<u64> = rows
            .into_iter()
            .filter(|(_, _, b)| b.as_slice() == body.as_slice())
            .map(|(off, _, _)| off)
            .collect();
        if lineage {
            assert_eq!(
                replies.len(),
                1,
                "the reply to offset 1, below the fork, is published once: {replies:?}"
            );
            assert!(replies[0] >= 4, "on the new generation: {replies:?}");
        } else {
            assert!(
                replies.is_empty(),
                "CONTROL: another log drops the reply: {replies:?}"
            );
        }
        bridge.close_subscriptions();
    }
}

/// **AN ENTRY A REPLAY PUBLISHED ON A MOVED GENERATION IS READ BACK FROM THE FORK BY THE
/// NEXT REPLAY** (aterm-link B05 review, `Bridge::read_back_from`). An own-lane note's
/// entry was journaled at head 10 of a log that a power loss cut back to 4 records, and the
/// broker reopens it as a moved generation that continues it at 4. The first bridge's
/// replay reads back from the fork, finds nothing, and publishes the note at the new head,
/// below 10; the bridge is killed before anything ends the entry. A successor's replay
/// reads back from the fork again, finds that copy and publishes nothing: the note is on
/// the bus once, though a cut carried it and dropped its key. NEGATIVE CONTROL
/// (`Mutant::ReadBackFromTheJournaledHead`): read back from the entry's own head, the
/// successor misses the copy below it and publishes a second. The sim's found seed for the
/// rule moves with every trajectory change (E07's review branch had none); this test holds it
/// with no seed.
#[cfg(unix)]
#[test]
fn an_entry_a_replay_published_on_a_moved_generation_is_read_back_from_the_fork() {
    use crate::durability::{self, Durability};
    let sessions = [(1, "s-a"), (2, "s-b")];
    for control in [false, true] {
        let case = format!("control={control}");
        let (lane, mut first) = FakeVerbLane::bridge(&sessions);
        *lane.outbox.lock().unwrap() = Some(String::new());
        lane.answer_hello_with(&format!("OK lane={LINK_LOG_LANE} build=test\n"));
        first.introduce();
        assert!(first.refresh_sessions().is_ok());
        let state = first.state.root().to_path_buf();
        let log = lane.dir.path().join("moved.log");
        let socket = lane.dir.path().join("moved.sock");
        first.cfg.broker = socket.to_string_lossy().into_owned();
        let subject = format!("/f/f/in/{0}/s-b/{0}/note", first.node);
        let mut note = Body::new(1);
        note.from = Some("s-a".into());
        note.text = "once".into();
        let body = note.encode();
        let (cut, old) = {
            let opened = durability::open(
                &log,
                None,
                Durability::Relaxed,
                Some(durability::test_boot(1)),
            )
            .unwrap();
            opened.broker.arm_unclean_stop();
            let mut server = opened.broker.serve(&socket).unwrap();
            let (mut writer, _) = first.connect().unwrap();
            for seq in 1..=4 {
                writer.publish(81, seq, "/f/f/x/history", b"v=1").unwrap();
            }
            let cut = std::fs::metadata(&log).unwrap().len();
            for seq in 5..=10 {
                writer.publish(81, seq, "/f/f/x/history", b"v=1").unwrap();
            }
            drop(writer);
            server.shutdown();
            (cut, opened.broker.log_identity())
        };
        let pin = plain_pin(first.next_seq().unwrap(), &subject, &body);
        first
            .state
            .set_in_flight(&InFlight {
                pin,
                subject: subject.clone(),
                body: body.clone(),
                log: Some(old),
                at: Some(10),
            })
            .unwrap();
        std::fs::OpenOptions::new()
            .write(true)
            .open(&log)
            .unwrap()
            .set_len(cut)
            .unwrap();
        let reopened = durability::open(
            &log,
            None,
            Durability::Relaxed,
            Some(durability::test_boot(2)),
        )
        .unwrap();
        assert_eq!(reopened.broker.head(), 4, "{case}: the moved generation");
        let _server = reopened.broker.serve(&socket).unwrap();
        let copies = |bridge: &Bridge| -> Vec<u64> {
            let (mut reader, _) = bridge.connect().unwrap();
            let ((rows, _), _) = transport::fetch_told_kept(&mut reader, 0, &subject, 64).unwrap();
            rows.into_iter()
                .filter(|(_, _, b)| b.as_slice() == body.as_slice())
                .map(|(off, _, _)| off)
                .collect()
        };
        // The first replay alone, on the moved generation's Hello, as a bridge killed right
        // after it leaves it.
        first.conn = Some(first.connect().unwrap().0.into());
        first
            .conn
            .as_mut()
            .unwrap()
            .ready()
            .unwrap()
            .hello()
            .unwrap();
        assert!(first.replay_in_flight(), "{case}: the first replay");
        let replayed = copies(&first);
        assert!(
            replayed.len() == 1 && replayed[0] < 10,
            "{case}: the first replay's copy, below the entry's head: {replayed:?}"
        );
        assert_eq!(
            first.state.in_flight().unwrap().len(),
            1,
            "{case}: the entry stands"
        );
        first.close_subscriptions();
        drop(first);
        // A cut past the copy carries it, the mail kept, and drops its key, as retention may
        // before the next replay: only a read back finds it then.
        reopened
            .broker
            .set_retention(lanes_whole_from_the_base(false))
            .unwrap();
        reopened
            .broker
            .retain_before_carrying_state(reopened.broker.head())
            .unwrap();

        let (second, mut bridge) = FakeVerbLane::bridge_with_state(&sessions, Some(&state));
        *second.outbox.lock().unwrap() = Some(String::new());
        second.answer_hello_with(&format!("OK lane={LINK_LOG_LANE} build=test\n"));
        bridge.introduce();
        assert!(bridge.refresh_sessions().is_ok());
        if control {
            bridge.fx =
                crate::fx::Fx::os_without(crate::sim::world::Mutant::ReadBackFromTheJournaledHead);
        }
        bridge.cfg.broker = socket.to_string_lossy().into_owned();
        let _ = bridge.attach_broker();
        let on_bus = copies(&bridge);
        assert_eq!(
            on_bus.len(),
            1 + usize::from(control),
            "{case}: the note on the bus once, or twice (the control): {on_bus:?}"
        );
        bridge.close_subscriptions();
    }
}

#[cfg(unix)]
#[test]
fn a_moved_generation_is_told_to_a_lane_3_endpoint_with_its_fork() {
    use crate::durability::{self, Durability};
    const SID: &str = "s-kept";
    for (endpoint_lane, lineage, refuse) in [
        (LINK_CONTINUES_LANE, true, false),
        (LINK_LOG_LANE, true, false),
        (LINK_CONTINUES_LANE, false, false),
        (LINK_CONTINUES_LANE, true, true),
    ] {
        let case = format!("lane={endpoint_lane} lineage={lineage} refuse={refuse}");
        let (lane, mut bridge) = FakeVerbLane::bridge(&[(1, SID)]);
        lane.answer_hello_with(&format!("OK lane={endpoint_lane} build=test\n"));
        bridge.introduce();
        assert_eq!(bridge.endpoint_lane, endpoint_lane, "{case}");
        assert!(bridge.refresh_sessions().is_ok());
        bridge.ignore_lineage = !lineage;
        let log = lane.dir.path().join("relaxed.log");
        let socket = lane.dir.path().join("relaxed.sock");
        bridge.cfg.broker = socket.to_string_lossy().into_owned();

        let (before, cut) = {
            let opened = durability::open(
                &log,
                None,
                Durability::Relaxed,
                Some(durability::test_boot(1)),
            )
            .unwrap();
            opened.broker.arm_unclean_stop();
            let mut server = opened.broker.serve(&socket).unwrap();
            let (mut writer, _) = bridge.connect().unwrap();
            for seq in 1..=4 {
                writer.publish(81, seq, "/f/f/x/history", b"v=1").unwrap();
            }
            let cut = std::fs::metadata(&log).unwrap().len();
            for seq in 5..=10 {
                writer.publish(81, seq, "/f/f/x/history", b"v=1").unwrap();
            }
            assert!(bridge.attach_broker(), "{case}: the first attach");
            bridge.state.set_seen_off(SID, 9).unwrap();
            drop(writer);
            bridge.close_subscriptions();
            bridge.conn = None;
            server.shutdown();
            (opened.broker.log_identity(), cut)
        };
        std::fs::OpenOptions::new()
            .write(true)
            .open(&log)
            .unwrap()
            .set_len(cut)
            .unwrap();
        let reopened = durability::open(
            &log,
            None,
            Durability::Relaxed,
            Some(durability::test_boot(2)),
        )
        .unwrap();
        assert_eq!(reopened.broker.head(), 4, "{case}");
        let now = reopened.broker.log_identity();
        assert_eq!(
            (now.id, now.generation),
            (before.id, before.generation + 1),
            "{case}: the next generation of the same log"
        );
        let _server = reopened.broker.serve(&socket).unwrap();
        if refuse {
            *lane.log_answer.lock().unwrap() = "ERR usage: link log <identity>\n".to_string();
        }
        let attached = bridge.attach_broker();
        let told = lane
            .links
            .lock()
            .unwrap()
            .iter()
            .rev()
            .find(|l| l.starts_with("link log "))
            .cloned();
        let continued = format!("link log {now} continues=4 was={before}");
        if refuse {
            assert!(!attached, "{case}: a refused move fails the attach");
            assert_eq!(told, Some(continued), "{case}");
            assert_eq!(
                bridge.state.seen_off(SID),
                Some(9),
                "{case}: nothing moved before the endpoint took the move"
            );
            assert_eq!(
                bridge.state.log_witness().unwrap().id,
                Some(before),
                "{case}"
            );
            continue;
        }
        assert!(attached, "{case}: the moved generation, attached");
        let expected = if endpoint_lane >= LINK_CONTINUES_LANE && lineage {
            continued
        } else {
            format!("link log {now}")
        };
        assert_eq!(told, Some(expected), "{case}");
        assert_eq!(
            bridge.state.seen_off(SID),
            if lineage { Some(3) } else { None },
            "{case}: the bridge clamps at the fork it told"
        );
        bridge.close_subscriptions();
    }
}

#[cfg(unix)]
#[test]
fn a_clamp_clears_halt_acked_and_the_halt_is_answered_again() {
    use crate::durability::{self, Durability};
    const SID: &str = "s-held";
    let (lane, mut bridge) = FakeVerbLane::bridge(&[(1, SID)]);
    assert!(bridge.refresh_sessions().is_ok());
    let log = lane.dir.path().join("relaxed.log");
    let socket = lane.dir.path().join("relaxed.sock");
    bridge.cfg.broker = socket.to_string_lossy().into_owned();
    let node = bridge.node.clone();
    let acks_of = |sock: &std::path::Path| -> Vec<String> {
        let (mut c, _) = transport::connect(&sock.to_string_lossy(), ACK_DEADLINE).unwrap();
        let (rows, _) = c
            .fetch(0, &subject::node_face("f", &node, "ack"), 64)
            .unwrap();
        rows.into_iter()
            .map(|(_, _, body)| String::from_utf8_lossy(&body).into_owned())
            .filter(|b| b.contains(" re=0 "))
            .collect()
    };
    let cut = {
        let opened = durability::open(
            &log,
            None,
            Durability::Relaxed,
            Some(durability::test_boot(1)),
        )
        .unwrap();
        opened.broker.arm_unclean_stop();
        let mut server = opened.broker.serve(&socket).unwrap();
        let (mut human, _) = bridge.connect().unwrap();
        let (halt, _) = human
            .publish(
                91,
                1,
                "/f/f/fleet/h-x/halt",
                b"v=1 t=1 state=on reason=stop",
            )
            .unwrap();
        assert_eq!(halt, 0);
        let cut = std::fs::metadata(&log).unwrap().len();
        assert!(bridge.attach_broker());
        assert!(bridge.halt_applied);
        assert_eq!(
            bridge.state.halt_acked(),
            Some(0),
            "answered on boot 1's log"
        );
        assert_eq!(acks_of(&socket).len(), 1);
        drop(human);
        bridge.close_subscriptions();
        bridge.conn = None;
        server.shutdown();
        cut
    };
    std::fs::OpenOptions::new()
        .write(true)
        .open(&log)
        .unwrap()
        .set_len(cut)
        .unwrap();
    let reopened = durability::open(
        &log,
        None,
        Durability::Relaxed,
        Some(durability::test_boot(2)),
    )
    .unwrap();
    assert_eq!(
        reopened.broker.head(),
        1,
        "the halt survived, its ack did not"
    );
    let _server = reopened.broker.serve(&socket).unwrap();
    assert!(acks_of(&socket).is_empty());
    assert!(bridge.attach_broker(), "the moved generation, clamped at 1");
    assert!(bridge.halt_applied, "the halt still holds");
    assert_eq!(bridge.state.halt_acked(), Some(0), "answered again");
    assert_eq!(acks_of(&socket).len(), 1, "one ack on the new generation");

    bridge.close_subscriptions();
    bridge.conn = None;
    assert!(bridge.attach_broker());
    assert_eq!(acks_of(&socket).len(), 1, "a reattach answers nothing more");
    bridge.close_subscriptions();
}

#[cfg(unix)]
#[test]
fn a_log_switch_drops_old_commit_floors_but_keeps_the_session_fence() {
    const HELD: &str = "s-held";
    const HEALTHY: &str = "s-healthy";
    let (lane, mut bridge) = FakeVerbLane::bridge(&[(1, HELD), (2, HEALTHY)]);
    assert!(bridge.refresh_sessions().is_ok());
    let (a, _broker_a, _server_a) = serve_log(&lane, "debt-a");
    bridge.cfg.broker = a;
    assert!(bridge.attach_broker());
    let node = bridge.node.clone();
    let face = |sid: &str| format!("/f/f/in/{node}/{sid}/n-peer/note");
    let body = b"v=1 t=1 from=s-peer text=mail";
    let (mut writer, _) = bridge.connect().unwrap();
    let (old, _) = writer.publish(81, 1, &face(HELD), body).unwrap();
    lane.answer_deliveries_to(HELD, SHIPPED_DELIVER_USAGE);
    bridge.on_inbox_record(&(old, face(HELD), body.to_vec()), None);
    bridge.commit_floor = Some(old);
    bridge.conn = None;
    assert!(bridge.attach_broker());
    assert_eq!(
        bridge.inbox_commit_floor(),
        Some(old),
        "same-log debt stays"
    );

    let (b, _broker_b, _server_b) = serve_log(&lane, "debt-b");
    bridge.close_subscriptions();
    bridge.conn = None;
    bridge.cfg.broker = b;
    let (mut writer, _) = bridge.connect().unwrap();
    for seq in 1..=32 {
        writer.publish(82, seq, "/f/f/x/history", b"v=1").unwrap();
    }
    assert!(bridge.attach_broker());
    let (healthy, _) = writer.publish(82, 33, &face(HEALTHY), body).unwrap();
    bridge.on_inbox_record(&(healthy, face(HEALTHY), body.to_vec()), None);
    assert_eq!(
        bridge.committed,
        Some(healthy),
        "A's debt cannot clamp B's group"
    );
    assert_eq!(
        bridge.per_sid.lane_owed.get(HELD),
        Some(&None),
        "grammar fence stays"
    );

    let delivered = lane.delivery_offsets();
    let (owed, _) = writer.publish(82, 34, &face(HELD), body).unwrap();
    bridge.on_inbox_record(&(owed, face(HELD), body.to_vec()), None);
    let (later, _) = writer.publish(82, 35, &face(HEALTHY), body).unwrap();
    bridge.on_inbox_record(&(later, face(HEALTHY), body.to_vec()), None);
    assert_eq!(
        bridge.committed,
        Some(owed - 1),
        "B's new owed row bounds B"
    );
    assert_eq!(bridge.per_sid.lane_owed[HELD], Some(owed));
    assert_eq!(lane.delivery_offsets(), [delivered, vec![later]].concat());
}

fn log_head(reader: &mut Conn) -> u64 {
    reader.fetch(0, "/f/>", 0).expect("a head query").1 .1
}

fn run_rounds(bridge: &mut Bridge) {
    loop {
        let idle = bridge.mailbox.is_idle();
        assert!(bridge.round(Duration::ZERO), "aterm did not close the lane");
        if idle {
            break;
        }
    }
}

fn queue_inbox(bridge: &Bridge, records: &[BrokerRecord]) {
    let generation = bridge.mailbox.broker_generation(Source::Inbox);
    for rec in records {
        assert_eq!(
            bridge.mailbox.push(
                Source::Inbox,
                rec.clone(),
                None,
                generation,
                bridge.fx.now()
            ),
            Pushed::Queued
        );
    }
}

fn burst_rig(
    lane: &FakeVerbLane,
    bridge: &mut Bridge,
    sid: &str,
    n: u64,
) -> (
    Vec<BrokerRecord>,
    Conn,
    String,
    astream_broker::Broker,
    astream_broker::BrokerHandle,
) {
    let (socket, broker, server) = serve_log(lane, "burst");
    bridge.cfg.broker = socket.clone();
    bridge.conn = Some(bridge.connect().unwrap().0.into());
    assert!(bridge.refresh_sessions().is_ok());
    bridge.per_sid.pending_admit.clear();
    let face = format!("/f/f/in/{}/{sid}/n-peer/note", bridge.node);
    let body = b"v=1 t=1 from=s-peer text=burst".to_vec();
    let (mut writer, _) = bridge.connect().unwrap();
    let records = (1..=n)
        .map(|seq| {
            let (off, _) = writer.publish(91, seq, &face, &body).unwrap();
            (off, face.clone(), body.clone())
        })
        .collect();
    (records, writer, socket, broker, server)
}

#[cfg(unix)]
#[test]
fn a_refill_below_the_retained_floor_reads_on_from_it() {
    const SID: &str = "s-kept";
    let (lane, mut bridge) = FakeVerbLane::bridge(&[(1, SID)]);
    let (records, mut reader, _socket, broker, _server) = burst_rig(&lane, &mut bridge, SID, 3);
    let offs: Vec<u64> = records.iter().map(|r| r.0).collect();
    broker
        .retain_before_carrying_state(offs[1])
        .expect("the cut");
    let filter = format!("/f/f/in/{}/{SID}/>", bridge.node);
    let refused = reader
        .fetch(offs[0], &filter, 8)
        .expect_err("PRECONDITION: a plain read below the floor is refused");
    assert_eq!(transport::retained_past(&refused, offs[0]), Some(offs[1]));
    assert_eq!(
        bridge.state.seen_off(SID),
        None,
        "PRECONDITION: no `seen`, so the refill reads from its floor span, offset 0"
    );

    assert!(bridge.refill(SID), "the refill reads on");
    assert_eq!(
        lane.delivery_offsets(),
        offs[1..],
        "the kept records, in order"
    );
    assert!(bridge.conn.is_some(), "and the link stays up");
}

/// The `link gap` lines a fake lane was told, trimmed.
fn told_gaps(lane: &FakeVerbLane) -> Vec<String> {
    lane.links()
        .into_iter()
        .filter(|l| l.starts_with("link gap "))
        .map(|l| l.trim_end().to_string())
        .collect()
}

/// B12 — A REFILL FROM `seen` THAT MEETS RETENTION'S FLOOR TELLS THAT SESSION: four mails on
/// the session's lane, its `seen` at the first, the log cut at the third. The refill reads on
/// from the floor and says `link gap from=<seen+1> to=<floor> sid=<sid>` before the first
/// mail it delivers from there, and a second refill over the same floor says nothing more.
/// A session with no `seen` refills from its floor span and, by choice, is told nothing
/// (with no watermark the bridge cannot tell what it handled; see `Bridge::refill`), nor is
/// an aterm older than the scoped `link gap` lane; both still get the kept mail. NC (run): a
/// `tell_given_up` that returns at once fails it.
#[cfg(unix)]
#[test]
fn a_refill_from_seen_below_the_retained_floor_tells_that_session_what_went() {
    const SID: &str = "s-kept";
    let run = |seen: bool, endpoint_lane: u32| {
        let (lane, mut bridge) = FakeVerbLane::bridge(&[(1, SID)]);
        bridge.endpoint_lane = endpoint_lane;
        let (records, _reader, _socket, broker, _server) = burst_rig(&lane, &mut bridge, SID, 4);
        let offs: Vec<u64> = records.iter().map(|r| r.0).collect();
        broker
            .retain_before_carrying_state(offs[2])
            .expect("the cut");
        if seen {
            bridge.state.set_seen_off(SID, offs[0]).unwrap();
        }
        let links = Arc::clone(&lane.links);
        let told_first = Arc::new(std::sync::Mutex::new(None));
        let at_first = Arc::clone(&told_first);
        lane.on_first_deliver(move || {
            let gaps = links.lock().unwrap();
            let gaps = gaps.iter().filter(|l| l.starts_with("link gap ")).count();
            *at_first.lock().unwrap() = Some(gaps);
        });
        assert!(bridge.refill(SID), "the refill reads on");
        let first = told_gaps(&lane);
        assert!(bridge.refill(SID), "and reads on again");
        assert_eq!(told_gaps(&lane), first, "the same floor is told once");
        assert_eq!(
            lane.delivery_offsets()[..2],
            offs[2..],
            "the kept mail, from the floor"
        );
        let told_first = *told_first.lock().unwrap();
        (offs, first, told_first)
    };

    let (offs, told, at_first) = run(true, aterm_types::control_verbs::BRIDGE_LANE);
    assert_eq!(
        told,
        [format!(
            "link gap from={} to={} sid={SID}",
            offs[0] + 1,
            offs[2]
        )]
    );
    assert_eq!(
        at_first,
        Some(1),
        "told before the first mail from the floor"
    );
    let (_, told, _) = run(false, aterm_types::control_verbs::BRIDGE_LANE);
    assert!(told.is_empty(), "no watermark, no report: {told:?}");
    let (_, told, _) = run(true, LINK_GAP_SCOPED_LANE - 1);
    assert!(told.is_empty(), "an older aterm is told nothing: {told:?}");
}

/// The rig of the attach's refill together (B09): three admitted sessions owed their
/// refill, `A`, `B` and `C`, a broker, and `rounds` rounds of one mail to each in turn,
/// with a record no lane holds between rounds. The mails, by session, in offset order.
#[cfg(unix)]
fn together_rig(
    rounds: u64,
) -> (
    FakeVerbLane,
    Bridge,
    astream_broker::Broker,
    astream_broker::BrokerHandle,
    BTreeMap<&'static str, Vec<u64>>,
) {
    let order: Vec<Option<&'static str>> = (0..rounds)
        .flat_map(|_| TOGETHER.map(Some).into_iter().chain([None]))
        .collect();
    together_rig_with(None, &order)
}

/// [`together_rig`] over a broker retained by `rules` (set before the first record, as
/// aterm sets its own at the open), publishing one mail to each session of `order` in
/// turn, `None` a record no lane holds.
#[cfg(unix)]
fn together_rig_with(
    rules: Option<astream_broker::Retention>,
    order: &[Option<&'static str>],
) -> (
    FakeVerbLane,
    Bridge,
    astream_broker::Broker,
    astream_broker::BrokerHandle,
    BTreeMap<&'static str, Vec<u64>>,
) {
    let (lane, mut bridge) =
        FakeVerbLane::bridge(&[(1, TOGETHER[0]), (2, TOGETHER[1]), (3, TOGETHER[2])]);
    bridge.endpoint_lane = aterm_types::control_verbs::BRIDGE_LANE;
    let (socket, broker, server) = serve_log(&lane, "together");
    if let Some(rules) = rules {
        broker.set_retention(rules).unwrap();
    }
    bridge.cfg.broker = socket;
    bridge.conn = Some(bridge.connect().unwrap().0.into());
    assert!(bridge.refresh_sessions().is_ok());
    assert_eq!(
        bridge.per_sid.pending_admit.len(),
        3,
        "PRECONDITION: every session is owed its refill"
    );
    let (mut writer, _) = bridge.connect().unwrap();
    let body = b"v=1 t=1 from=s-peer text=mail".to_vec();
    let elsewhere = subject::session_face("f", &bridge.node, "s-else", "ev");
    let mut mails: BTreeMap<&'static str, Vec<u64>> = BTreeMap::new();
    for (seq, to) in (1..).zip(order) {
        match to {
            Some(sid) => {
                let face = format!("/f/f/in/{}/{sid}/n-peer/note", bridge.node);
                let (off, _) = writer.publish(91, seq, &face, &body).unwrap();
                mails.entry(sid).or_default().push(off);
            }
            None => {
                writer.publish(91, seq, &elsewhere, b"v=1 t=1").unwrap();
            }
        }
    }
    (lane, bridge, broker, server, mails)
}

/// [`together_rig_with`] retained by aterm's rules and `rounds` rounds of one mail to each
/// session: the inbox group read through the second round, then the `ev` class's bytes cut
/// to one (an `ev` publish over its class's bytes waits for a tick, so the rules start
/// with room) and one retention tick gives up the read mail (every lane's floor at its
/// second round's mail + 1, the node inbox's oldest at `C`'s) and holds the rest below
/// the cut.
#[cfg(unix)]
fn together_rig_cut(
    rounds: u64,
) -> (
    FakeVerbLane,
    Bridge,
    astream_broker::Broker,
    astream_broker::BrokerHandle,
    BTreeMap<&'static str, Vec<u64>>,
) {
    let order: Vec<Option<&'static str>> = (0..rounds)
        .flat_map(|_| TOGETHER.map(Some).into_iter().chain([None]))
        .collect();
    let roomy = crate::broker_rules::rules_scaled(1 << 30, 1 << 30, 1 << 30);
    let rig = together_rig_with(Some(roomy), &order);
    let (_, bridge, broker, _, mails) = &rig;
    let (mut writer, _) = bridge.connect().unwrap();
    let group = subject::inbox_group("f", &bridge.node);
    writer.commit(&group, mails[TOGETHER[2]][1]).unwrap();
    broker
        .set_retention(crate::broker_rules::rules_scaled(1, 1 << 30, 1 << 30))
        .unwrap();
    broker.retention_tick_at(broker.now_ms()).unwrap();
    rig
}

const TOGETHER: [&str; 3] = ["s-ta", "s-tb", "s-tc"];

/// The lane's `deliver <off>` lines, in the order aterm was asked.
fn offered(lane: &FakeVerbLane) -> Vec<u64> {
    lane.transcript()
        .iter()
        .filter_map(|l| l.strip_prefix("deliver ")?.parse().ok())
        .collect()
}

/// B09 — AN ATTACH REFILLS ITS SESSIONS IN ONE READ, EACH FROM ITS OWN WATERMARK: `A` saw
/// its first mail, `B` its second, `C` none (its floor span reads from offset 0). One read
/// of the node's inbox offers each session its records past its own start, in offset order
/// across the sessions, where a refill lane by lane offers `A`'s, then `B`'s, then `C`'s.
/// NC (run): `refill_pending_admissions` with its `refill_together` arm cut offers them
/// lane by lane and fails the order.
#[cfg(unix)]
#[test]
fn an_attach_refills_its_sessions_in_one_read_each_from_its_own_watermark() {
    let (lane, mut bridge, _broker, _server, mails) = together_rig(3);
    let [a, b, c] = TOGETHER;
    bridge.state.set_seen_off(a, mails[a][0]).unwrap();
    bridge.state.set_seen_off(b, mails[b][1]).unwrap();

    assert!(bridge.refill_pending_admissions(false));
    let mut want: Vec<u64> = mails[a][1..]
        .iter()
        .chain(&mails[b][2..])
        .chain(&mails[c])
        .copied()
        .collect();
    want.sort_unstable();
    assert_eq!(
        offered(&lane),
        want,
        "each session's records past its start, offered once, in offset order"
    );
    assert!(bridge.per_sid.pending_admit.is_empty(), "and each is done");
    assert!(bridge.conn.is_some());
    assert!(told_gaps(&lane).is_empty(), "no floor, nothing told");
}

/// B09 — A SESSION BELOW THE NODE INBOX'S FLOOR IS TOLD, AS ITS OWN READ TELLS IT, AND
/// REFILLED WITH THE REST: the log is cut below the second round. `A`'s watermark is below
/// the cut, so `A` is told `link gap from=<seen+1> to=<floor> sid=A`, as its own lane's
/// read would tell it, and starts there; `B`'s is above it and is told nothing. The kept
/// records go in one read, in offset order.
#[cfg(unix)]
#[test]
fn a_session_below_the_inbox_floor_is_told_and_refilled_with_the_rest() {
    let (lane, mut bridge, broker, _server, mails) = together_rig(3);
    let [a, b, c] = TOGETHER;
    let floor = mails[a][1];
    broker.retain_before_carrying_state(floor).expect("the cut");
    bridge.state.set_seen_off(a, mails[a][0]).unwrap();
    bridge.state.set_seen_off(b, mails[b][1]).unwrap();

    assert!(bridge.refill_pending_admissions(false));
    assert_eq!(
        told_gaps(&lane),
        [format!(
            "link gap from={} to={floor} sid={a}",
            mails[a][0] + 1
        )],
        "A is told what went below the floor, and only A"
    );
    let mut want: Vec<u64> = mails[a][1..]
        .iter()
        .chain(&mails[b][2..])
        .chain(mails[c].iter().filter(|off| **off >= floor))
        .copied()
        .collect();
    want.sort_unstable();
    assert_eq!(offered(&lane), want, "the kept records, in one read");
    assert!(bridge.per_sid.pending_admit.is_empty());
}

/// B09 — A SESSION WHOSE DELIVERY IS OWED STOPS THERE, AND THE OTHERS READ ON: aterm owes
/// `B` its first mail (an answer with no verdict), so `B` is offered nothing more, as its own
/// refill stops; `A` and `C` are offered every record, and every session is done.
#[cfg(unix)]
#[test]
fn an_owed_session_stops_and_the_others_read_on() {
    let (lane, mut bridge, _broker, _server, mails) = together_rig(3);
    let [a, b, c] = TOGETHER;
    lane.answer_deliveries_to(b, SHIPPED_DELIVER_USAGE);

    assert!(bridge.refill_pending_admissions(false));
    let mut want: Vec<u64> = mails[a]
        .iter()
        .chain(&mails[b][..1])
        .chain(&mails[c])
        .copied()
        .collect();
    want.sort_unstable();
    assert_eq!(offered(&lane), want, "B is asked once, the others for all");
    assert!(bridge.per_sid.pending_admit.is_empty());
}

/// B09 — A FLOOR THAT RISES UNDER THE SHARED READ HANDS EACH SESSION TO ITS OWN LANE: more
/// than a page of mail, and the log cut past the first page while aterm takes its first
/// record. The shared read stops where it got to, and each session reads on alone from
/// there, meets its own lane's floor and is told it from the stop, as its own read tells
/// it. Every record below the stop and every one the cut kept is offered, and none twice.
#[cfg(unix)]
#[test]
fn a_floor_rising_under_the_shared_read_hands_each_session_to_its_own_lane() {
    let (lane, mut bridge, broker, _server, mails) = together_rig(120);
    let [a, b, c] = TOGETHER;
    bridge.state.set_seen_off(a, mails[a][0]).unwrap();
    bridge.state.set_seen_off(b, mails[b][0]).unwrap();
    bridge.state.set_seen_off(c, mails[c][0]).unwrap();
    let floor = mails[a][110];
    let cut = std::sync::Arc::new(std::sync::Mutex::new(Some(broker)));
    let cutter = std::sync::Arc::clone(&cut);
    lane.on_first_deliver(move || {
        if let Some(broker) = cutter.lock().unwrap().as_ref() {
            broker
                .retain_before_carrying_state(floor)
                .expect("the cut under the read");
        }
    });

    assert!(bridge.refill_pending_admissions(false));
    let offers = offered(&lane);
    let unique: BTreeSet<u64> = offers.iter().copied().collect();
    assert_eq!(unique.len(), offers.len(), "none offered twice: {offers:?}");
    let page_end = offers
        .iter()
        .copied()
        .take_while(|off| *off < floor)
        .max()
        .expect("the first page was offered");
    let mut gaps = told_gaps(&lane);
    gaps.sort();
    assert_eq!(
        gaps,
        TOGETHER.map(|sid| format!("link gap from={} to={floor} sid={sid}", page_end + 1)),
        "each session meets its own lane's floor from where the shared read stopped"
    );
    assert!(
        page_end + 1 < floor,
        "PRECONDITION: the cut is past the stop"
    );
    let _ = (b, c);
    for sid in TOGETHER {
        for off in &mails[sid][1..] {
            assert_eq!(
                unique.contains(off),
                *off <= page_end || *off >= floor,
                "{sid} @{off}: below the stop {page_end} or kept past the floor {floor}"
            );
        }
    }
    assert!(bridge.per_sid.pending_admit.is_empty());
    drop(cut);
}

/// B09 — A SESSION WHOSE DELIVERY IS OWED STAYS STOPPED WHEN A RISING FLOOR HANDS THE
/// OTHERS ON: the shared read of [`a_floor_rising_under_the_shared_read_hands_each_session_to_its_own_lane`]
/// with aterm owing `B` its first record. `B` is offered that record and nothing more, as
/// its own refill stops there, and is told no floor; `A` and `C` read on alone from the
/// stop and are told theirs.
#[cfg(unix)]
#[test]
fn an_owed_session_stays_stopped_when_a_rising_floor_hands_the_others_on() {
    let (lane, mut bridge, broker, _server, mails) = together_rig(120);
    let [a, b, c] = TOGETHER;
    for sid in TOGETHER {
        bridge.state.set_seen_off(sid, mails[sid][0]).unwrap();
    }
    lane.answer_deliveries_to(b, SHIPPED_DELIVER_USAGE);
    let floor = mails[a][110];
    let cut = std::sync::Arc::new(std::sync::Mutex::new(Some(broker)));
    let cutter = std::sync::Arc::clone(&cut);
    lane.on_first_deliver(move || {
        if let Some(broker) = cutter.lock().unwrap().as_ref() {
            broker
                .retain_before_carrying_state(floor)
                .expect("the cut under the read");
        }
    });

    assert!(bridge.refill_pending_admissions(false));
    let offers = offered(&lane);
    let to_b: Vec<u64> = offers
        .iter()
        .copied()
        .filter(|off| mails[b].contains(off))
        .collect();
    assert_eq!(to_b, [mails[b][1]], "B is offered its owed record only");
    let gaps = told_gaps(&lane);
    assert!(
        gaps.iter().all(|g| !g.ends_with(&format!("sid={b}"))),
        "B, stopped, meets no floor: {gaps:?}"
    );
    for sid in [a, c] {
        assert!(
            gaps.iter().any(|g| g.ends_with(&format!("sid={sid}"))),
            "{sid} reads on and meets its floor: {gaps:?}"
        );
        assert!(
            mails[sid]
                .iter()
                .filter(|off| **off >= floor)
                .all(|off| offers.contains(off)),
            "{sid} is offered every record the cut kept"
        );
    }
    assert!(bridge.per_sid.pending_admit.is_empty());
    drop(cut);
}

/// B09 — A SESSION BELOW ANOTHER LANE'S FLOOR JOINS THE SHARED READ AT THE INBOX'S OLDEST:
/// three rounds, the inbox group read through the second and retention's tick giving it
/// up ([`together_rig_cut`]), so the lanes' floors are their second mails + 1 and the
/// node inbox's oldest `C`'s. `A` saw its first mail, `B` and `C` their second. `A` is
/// told its lane's give-up (`from=<seen+1> to=<its floor>`), `B` and `C` nothing; `A` and
/// `B`, still below the inbox's oldest (`C`'s floor), read their own lanes up to it, find
/// nothing there, and the third round goes in one read in offset order, where the refill
/// sent each of them alone to the head after `C`'s.
#[cfg(unix)]
#[test]
fn a_session_below_another_lanes_floor_joins_the_shared_read_at_the_inbox_oldest() {
    let (lane, mut bridge, _broker, _server, mails) = together_rig_cut(3);
    let [a, b, c] = TOGETHER;
    bridge.state.set_seen_off(a, mails[a][0]).unwrap();
    bridge.state.set_seen_off(b, mails[b][1]).unwrap();
    bridge.state.set_seen_off(c, mails[c][1]).unwrap();

    assert!(bridge.refill_pending_admissions(false));
    assert_eq!(
        told_gaps(&lane),
        [format!(
            "link gap from={} to={} sid={a}",
            mails[a][0] + 1,
            mails[a][1] + 1
        )],
        "A is told its lane's give-up, and only A"
    );
    assert_eq!(
        offered(&lane),
        [mails[a][2], mails[b][2], mails[c][2]],
        "the third round, in one read"
    );
    assert!(bridge.per_sid.pending_admit.is_empty());
}

/// B09 — A LANE'S MAIL HELD BELOW ANOTHER LANE'S FLOOR IS OFFERED BEFORE THE SHARED READ: a
/// broker whose rules hold `A`'s mail for a reader group of its own, which never reads it,
/// and everyone else's for the node's inbox group, read through the second round and given
/// up by a tick. The node inbox's oldest is `C`'s floor, and `A`'s second mail lies below
/// it, still held. `A`'s own read up to the oldest offers it, and the third round goes in
/// one read. CONTROL (`refill_joins_below_the_oldest`): `A` joins at the oldest without
/// reading below it, and its held mail is never offered.
#[cfg(unix)]
#[test]
fn a_lanes_mail_held_below_another_lanes_floor_is_offered_before_the_shared_read() {
    use crate::broker_rules::{EV_FILTER, MAIL_FILTER, MAIL_READER};
    use astream_broker::{Retention, Rule};
    let [a, b, c] = TOGETHER;
    let rules = |ev: u64| {
        Retention::new(
            vec![
                Rule::prefix(EV_FILTER).unwrap().max_bytes(ev),
                Rule::hold(&format!("/f/*/in/*/{a}/>"))
                    .unwrap()
                    .until_read("/f/{2}/cur/{4}/held/ta")
                    .unwrap(),
                Rule::hold(MAIL_FILTER)
                    .unwrap()
                    .until_read(MAIL_READER)
                    .unwrap(),
            ],
            Rule::default_keeping_all(),
        )
        .unwrap()
    };
    let order: Vec<Option<&'static str>> = (0..3)
        .flat_map(|_| TOGETHER.map(Some).into_iter().chain([None]))
        .collect();
    for control in [false, true] {
        let (lane, mut bridge, broker, _server, mails) =
            together_rig_with(Some(rules(1 << 30)), &order);
        let (mut writer, _) = bridge.connect().unwrap();
        let group = subject::inbox_group("f", &bridge.node);
        writer.commit(&group, mails[c][1]).unwrap();
        broker.set_retention(rules(1)).unwrap();
        broker.retention_tick_at(broker.now_ms()).unwrap();
        bridge.state.set_seen_off(a, mails[a][0]).unwrap();
        bridge.state.set_seen_off(b, mails[b][1]).unwrap();
        bridge.state.set_seen_off(c, mails[c][1]).unwrap();
        bridge.refill_joins_below_the_oldest = control;

        assert!(bridge.refill_pending_admissions(false));
        assert!(
            told_gaps(&lane).is_empty(),
            "nothing given up past a watermark"
        );
        let want: &[u64] = if control {
            &[mails[a][2], mails[b][2], mails[c][2]]
        } else {
            &[mails[a][1], mails[a][2], mails[b][2], mails[c][2]]
        };
        assert_eq!(
            offered(&lane),
            want,
            "{}",
            if control {
                "CONTROL: A's held mail is not offered"
            } else {
                "A's held mail, then the third round in one read"
            }
        );
        assert!(bridge.per_sid.pending_admit.is_empty());
    }
}

/// B09 — SESSIONS BELOW THE INBOX'S OLDEST READ THEIR LANES ONLY UP TO IT: the cut world of
/// [`a_session_below_another_lanes_floor_joins_the_shared_read_at_the_inbox_oldest`] with
/// 300 more rounds, more than a page of each lane held past the floors. Every kept mail is
/// offered once, and the refill ends on the last round's three mails in offset order: the
/// sessions below the oldest joined the shared read, where alone each read its lane to the
/// head (and the last mails offered were `B`'s).
#[cfg(unix)]
#[test]
fn sessions_below_the_inbox_oldest_read_their_lanes_only_up_to_it() {
    let (lane, mut bridge, _broker, _server, mails) = together_rig_cut(302);
    let [a, b, c] = TOGETHER;
    bridge.state.set_seen_off(a, mails[a][0]).unwrap();
    bridge.state.set_seen_off(b, mails[b][1]).unwrap();
    bridge.state.set_seen_off(c, mails[c][1]).unwrap();

    assert!(bridge.refill_pending_admissions(false));
    let offers = offered(&lane);
    let unique: BTreeSet<u64> = offers.iter().copied().collect();
    assert_eq!(unique.len(), offers.len(), "none offered twice");
    for sid in TOGETHER {
        assert!(
            mails[sid][2..].iter().all(|off| unique.contains(off)),
            "{sid}: every kept mail offered"
        );
    }
    assert_eq!(unique.len(), 3 * 300, "and nothing else");
    assert_eq!(
        offers[offers.len() - 3..],
        [mails[a][301], mails[b][301], mails[c][301]],
        "the refill ends in one read of every lane"
    );
    assert!(bridge.per_sid.pending_admit.is_empty());
}

/// B09 — A SHARED READ THAT CARRIES MORE THAN IT SAVES HANDS EACH SESSION TO ITS OWN LANE:
/// `A` saw its one mail, then 1,500 mails each go to `B` and `C`, who saw their last, so
/// nobody is owed anything. The shared read from `A`'s start carries `B`'s and `C`'s mail
/// below their starts and saves no scan (only `A` reads there); once what it carried costs
/// more than astream's slack at its `REFILL_ROW_COST` a record, each session reads on
/// alone, so it carries at most that and a page. CONTROL (`refill_never_hands_on`): the
/// read carries all 3,000 to the head (lane by lane carried none).
#[cfg(unix)]
#[test]
fn a_shared_read_that_carries_more_than_it_saves_hands_each_session_to_its_own_lane() {
    const EACH: usize = 1_500;
    let [a, b, c] = TOGETHER;
    let order: Vec<Option<&'static str>> = std::iter::once(Some(a))
        .chain((0..EACH).flat_map(|_| [Some(b), Some(c)]))
        .collect();
    let bound = astream_broker::refill::DEFAULT_REFILL_SPLIT_SLACK
        / astream_broker::refill::REFILL_ROW_COST
        + u64::from(transport::POST_PAGE);
    for control in [false, true] {
        let (lane, mut bridge, _broker, _server, mails) = together_rig_with(None, &order);
        for sid in TOGETHER {
            let last = *mails[sid].last().unwrap();
            bridge.state.set_seen_off(sid, last).unwrap();
        }
        bridge.refill_never_hands_on = control;

        assert!(bridge.refill_pending_admissions(false));
        assert!(offered(&lane).is_empty(), "nobody is owed anything");
        assert!(bridge.per_sid.pending_admit.is_empty());
        if control {
            assert_eq!(
                bridge.refill_carried,
                2 * EACH as u64,
                "CONTROL: never handed on, the read carries every mail"
            );
        } else {
            assert!(
                bridge.refill_carried <= bound,
                "carried {} past the bound {bound}",
                bridge.refill_carried
            );
        }
    }
}

/// B09 — A SHARED READ'S PAGE THAT MAKES NO PROGRESS IS WEIGHED AS NONE: a page of the
/// node's inbox from 500 (its oldest 100) that ends at or below 500, short of the head, is
/// no progress, and [`weigh_shared_page`] answers no step; one at the head ends the read, one
/// past 500 reads on. CONTROL: astream's meter alone hands each session on to its own lane
/// there, so the check ahead of it is what keeps the step from handing on. That the read
/// then fails is held through [`Bridge::refill_node`] by
/// [`a_shared_refill_read_that_makes_no_progress_fails_with_its_sessions_still_owed`].
#[test]
fn a_shared_refill_page_that_makes_no_progress_is_weighed_as_none() {
    let scan_max = astream_broker::broker::FETCH_SCAN_MAX as u64;
    let meter = SharedRefillMeter::new(100, 500, scan_max);
    for next in [500, 499] {
        assert_eq!(
            weigh_shared_page(&mut meter.clone(), next, 900, 0, 3),
            None,
            "a page ending at @{next} from @500 short of the head"
        );
        assert_eq!(
            meter.clone().observe_page(next, 900, 0, 3),
            RefillStep::HandOn {
                handoff_offset: 500
            },
            "CONTROL: the meter alone hands on from @500"
        );
    }
    assert_eq!(
        weigh_shared_page(&mut meter.clone(), 500, 500, 0, 3),
        Some(RefillStep::Complete { head: 500 }),
        "a page at the head ends the read"
    );
    assert_eq!(
        weigh_shared_page(&mut meter.clone(), 756, 900, 0, 3),
        Some(RefillStep::Continue {
            next_cursor: 756,
            active_debt: 0
        }),
        "a page that moves on reads on"
    );
}

/// B09 — A SHARED REFILL READ THAT MAKES NO PROGRESS FAILS WITH ITS SESSIONS STILL OWED:
/// three sessions, each past its first of three mails, owed their refill, and a broker that
/// answers the shared read's page with no records, ending where it began
/// (`refill_page_stalls`). The refill fails, nothing is offered, and every session is still
/// owed its refill, as a lane's read that makes no progress leaves its session. Handing each
/// session on to its own lane there instead (the meter's step taken without
/// [`weigh_shared_page`]'s check, or its `None` taken as `HandedOn`) refills them all from
/// their lanes and fails this. CONTROL: the same rig, its pages answered, refills every
/// session.
#[cfg(unix)]
#[test]
fn a_shared_refill_read_that_makes_no_progress_fails_with_its_sessions_still_owed() {
    for stalls in [true, false] {
        let (lane, mut bridge, _broker, _server, mails) = together_rig(3);
        for sid in TOGETHER {
            bridge.state.set_seen_off(sid, mails[sid][0]).unwrap();
        }
        bridge.refill_page_stalls = stalls;

        let refilled = bridge.refill_pending_admissions(false);
        if stalls {
            assert!(!refilled, "the read fails");
            assert!(offered(&lane).is_empty(), "nothing is offered");
            assert_eq!(
                bridge.per_sid.pending_admit.len(),
                3,
                "every session is still owed its refill"
            );
        } else {
            assert!(refilled, "CONTROL: the read refills");
            let mut want: Vec<u64> = TOGETHER
                .iter()
                .flat_map(|sid| mails[sid][1..].iter().copied())
                .collect();
            want.sort_unstable();
            assert_eq!(offered(&lane), want, "CONTROL: every mail past a watermark");
            assert!(
                bridge.per_sid.pending_admit.is_empty(),
                "CONTROL: nobody is owed"
            );
        }
    }
}

/// A broker on `lane` retaining by `lanes_whole_from_the_base(true)`, its ticks left to the
/// test, with `bridge` linked to it.
#[cfg(unix)]
fn held_lane_rig(
    lane: &FakeVerbLane,
    bridge: &mut Bridge,
    name: &str,
) -> (astream_broker::Broker, astream_broker::BrokerHandle) {
    let (socket, broker, server) = serve_log(lane, name);
    broker.set_retention_interval(Duration::from_secs(3600));
    broker
        .set_retention(lanes_whole_from_the_base(true))
        .unwrap();
    bridge.cfg.broker = socket;
    bridge.conn = Some(bridge.connect().unwrap().0.into());
    (broker, server)
}

/// A cut carrying `lane`'s records below `before` (its base), and the rules set next taking
/// their class whole from the base alone: the lane's oldest then lies above what the broker
/// still holds of it.
#[cfg(unix)]
fn carry_below_the_lanes_oldest(broker: &astream_broker::Broker, lane: &str, before: u64) {
    broker.retain_before_carrying_state(before).unwrap();
    broker
        .set_retention(lanes_whole_from_the_base(false))
        .unwrap();
    let of_lane = astream_wire::Filter::new(lane).unwrap();
    assert!(
        broker.oldest(&of_lane) >= before,
        "PRECONDITION: the lane's oldest lies above the records the broker holds"
    );
}

/// **A REFILL OFFERS THE MAIL RETENTION HOLDS BELOW ITS LANE'S OLDEST** (F3 review), on a
/// real broker: a peer's two notes on the session's lane, carried below the base by a cut
/// and held there while the rules set next take their class whole from the base alone, so
/// the lane's oldest lies above them; a third note lands after. The refill from the
/// session's `seen`, below the notes, meets the oldest, reads again told, and offers all
/// three in order, telling the session of no give-up, since none of its lane was given up.
/// CONTROL (`Mutant::LaneReadWithoutTheHeld`, the read going on at the floor, as before the
/// review): only the third is offered, and the session is told `link gap` over the two the
/// broker still holds.
#[cfg(unix)]
#[test]
fn a_refill_offers_the_mail_retention_holds_below_its_lanes_oldest() {
    const SID: &str = "s-kept";
    for control in [false, true] {
        let (lane, mut bridge) = FakeVerbLane::bridge(&[(1, SID)]);
        bridge.endpoint_lane = aterm_types::control_verbs::BRIDGE_LANE;
        let (broker, _server) = held_lane_rig(&lane, &mut bridge, "refill-held");
        assert!(bridge.refresh_sessions().is_ok());
        bridge.per_sid.pending_admit.clear();
        if control {
            bridge.fx =
                crate::fx::Fx::os_without(crate::sim::world::Mutant::LaneReadWithoutTheHeld);
        }
        let node = bridge.node.clone();
        let face = format!("/f/f/in/{node}/{SID}/n-peer/note");
        let note = |n: u64| format!("v=1 t=1 from=s-peer text=note-{n}").into_bytes();
        let (mut writer, _) = bridge.connect().unwrap();
        let (seen, _) = writer.publish(91, 1, "/f/f/x/padding", b"v=1").unwrap();
        let (first, _) = writer.publish(92, 1, &face, &note(1)).unwrap();
        let (second, _) = writer.publish(92, 2, &face, &note(2)).unwrap();
        carry_below_the_lanes_oldest(&broker, &format!("/f/f/in/{node}/{SID}/>"), second + 1);
        let (third, _) = writer.publish(92, 3, &face, &note(3)).unwrap();
        bridge.state.set_seen_off(SID, seen).unwrap();

        assert!(bridge.refill(SID), "the refill reads on");
        assert!(bridge.conn.is_some(), "and the link stays up");
        if control {
            assert_eq!(
                lane.delivery_offsets(),
                [third],
                "CONTROL: the held notes skipped"
            );
            assert_eq!(
                told_gaps(&lane),
                [format!("link gap from={first} to={third} sid={SID}")],
                "CONTROL: told a give-up over the notes the broker holds"
            );
            continue;
        }
        assert_eq!(
            lane.delivery_offsets(),
            [first, second, third],
            "every note, the held ones first, in order"
        );
        assert!(
            told_gaps(&lane).is_empty(),
            "nothing was given up: {:?}",
            told_gaps(&lane)
        );
    }
}

#[cfg(unix)]
#[test]
fn an_inbox_burst_costs_one_commit_record_and_one_record_at_a_time_costs_one_each() {
    const SID: &str = "s-burst";
    let (lane, mut bridge) = FakeVerbLane::bridge(&[(1, SID)]);
    let (records, mut reader, _socket, _broker, _server) = burst_rig(&lane, &mut bridge, SID, 32);
    let (burst, singles) = records.split_at(16);

    let before = log_head(&mut reader);
    queue_inbox(&bridge, burst);
    run_rounds(&mut bridge);
    let offs: Vec<u64> = burst.iter().map(|r| r.0).collect();
    assert_eq!(lane.delivery_offsets(), offs, "every record, in order");
    assert_eq!(
        bridge.committed,
        Some(burst[15].0),
        "committed through the last"
    );
    assert_eq!(
        log_head(&mut reader) - before,
        1,
        "one commit for the burst"
    );

    let before = log_head(&mut reader);
    for rec in singles {
        bridge.on_inbox_record(rec, None);
    }
    assert_eq!(bridge.committed, Some(singles[15].0));
    assert_eq!(
        log_head(&mut reader) - before,
        16,
        "the control: a commit per record handed over alone"
    );
}

#[cfg(unix)]
#[test]
fn the_round_takes_its_next_item_while_its_commit_is_in_the_flush_and_reads_it_after() {
    let (lane, mut bridge) = FakeVerbLane::bridge(&[(1, "s-a"), (2, "s-b")]);
    let (records, _reader, _socket, broker, _server) = burst_rig(&lane, &mut bridge, "s-b", 1);
    let before = bridge.committed;
    bridge.in_round = true;
    bridge.on_inbox_record(&records[0], None);
    bridge.in_round = false;
    assert_eq!(
        bridge.inbox_pending,
        Some(records[0].0),
        "PRECONDITION: the delivery owes its commit, nothing sent"
    );
    assert_eq!(bridge.conn.as_ref().map(Link::unacked), Some(0));

    let node = bridge.node.clone();
    bridge
        .indexed_subjects
        .insert(subject::post_lane("f", &node, "s-b", &node, "note"));
    let held = broker.pause_next_acks();
    let (go, gate) = std::sync::mpsc::channel::<()>();
    let watchdog = std::thread::spawn(move || {
        let reached = held.wait();
        let _ = gate.recv_timeout(aterm_spec::wait::MUST_HAPPEN);
        held.resume();
        reached
    });
    *lane.outbox.lock().unwrap() =
        Some("post sid=s-a id=1 to=@s-b kind=note len=4\npong".to_string());
    bridge.mailbox.push_event("EVENT 1 post 1".to_string());
    assert!(bridge.round(aterm_spec::wait::MUST_HAPPEN));
    let owed: Vec<&str> = bridge
        .conn
        .as_ref()
        .expect("the link stands")
        .owed()
        .map(|then| match then {
            Then::Commit { .. } => "commit",
            Then::Post(_) => "post",
            _ => "other",
        })
        .collect();
    assert_eq!(
        owed,
        ["commit", "post"],
        "the post went out behind the commit, its answer unread"
    );
    assert_eq!(bridge.committed, before, "the group has not moved");
    assert_eq!(bridge.inbox_pending, Some(records[0].0), "still owed");

    go.send(()).expect("the watchdog waits");
    assert!(
        watchdog.join().expect("the watchdog"),
        "the commit's batch was the one held"
    );
    assert!(bridge.mailbox.is_idle());
    assert!(bridge.round(Duration::ZERO));
    assert_eq!(bridge.conn.as_ref().map(Link::unacked), Some(0));
    assert_eq!(bridge.committed, Some(records[0].0), "the group is there");
    assert_eq!(bridge.inbox_pending, None);
    assert!(
        lane.sent
            .lock()
            .unwrap()
            .iter()
            .any(|l| l.starts_with("outbox sent s-a 1 off=")),
        "the post retired: {:?}",
        lane.sent.lock().unwrap()
    );
}

/// **AN IDLE ROUND'S INBOX COMMIT RIDES THE REPLY, IN ONE WRITE AND ONE BATCH** (critic
/// item B08). A delivery owes its commit; the round that finds the mailbox idle holds it
/// rather than send it alone, and the reply the woken session posts next carries it: the
/// commit and the post go out as one burst, the commit first, and the broker takes both
/// into one writer batch. Once their acks are read the group is through the delivery and
/// the post is retired. The rig holds the bridge's clock
/// ([`crate::fx::Fx::on_a_held_clock`]), so the grace cannot lapse inside the settle that
/// holds the commit however slowly a loaded machine runs it.
#[cfg(unix)]
#[test]
fn an_idle_rounds_inbox_commit_rides_the_reply_in_one_write_and_one_batch() {
    let (lane, mut bridge) = FakeVerbLane::bridge(&[(1, "s-a"), (2, "s-b")]);
    bridge.fx = crate::fx::Fx::os().on_a_held_clock();
    let (records, _reader, _socket, broker, _server) = burst_rig(&lane, &mut bridge, "s-b", 1);
    bridge.in_round = true;
    bridge.on_inbox_record(&records[0], None);
    bridge.in_round = false;
    assert_eq!(
        bridge.inbox_pending,
        Some(records[0].0),
        "PRECONDITION: the delivery owes its commit"
    );

    assert!(bridge.mailbox.is_idle());
    bridge.settle_owed();
    assert!(
        bridge.commit_held.is_some(),
        "the idle round holds the commit"
    );
    assert_eq!(
        bridge.conn.as_ref().map(Link::unacked),
        Some(0),
        "nothing went out alone"
    );

    let node = bridge.node.clone();
    bridge
        .indexed_subjects
        .insert(subject::post_lane("f", &node, "s-b", &node, "note"));
    *lane.outbox.lock().unwrap() =
        Some("post sid=s-a id=1 to=@s-b kind=note len=4\npong".to_string());
    bridge.mailbox.push_event("EVENT 1 post 1".to_string());
    let batches = broker.stats().writer_batches;
    assert!(bridge.park_and_take(aterm_spec::wait::MUST_HAPPEN));
    let owed: Vec<&str> = bridge
        .conn
        .as_ref()
        .expect("the link stands")
        .owed()
        .map(|then| match then {
            Then::Commit { .. } => "commit",
            Then::Post(_) => "post",
            _ => "other",
        })
        .collect();
    assert_eq!(
        owed,
        ["commit", "post"],
        "the reply carried the held commit"
    );
    assert_eq!(bridge.commit_held, None, "taken by the reply");

    bridge.settle_acks();
    assert_eq!(
        broker.stats().writer_batches - batches,
        1,
        "the commit and the post were one writer batch"
    );
    assert_eq!(
        bridge.committed,
        Some(records[0].0),
        "the group is through the delivery"
    );
    assert_eq!(bridge.inbox_pending, None);
    assert!(
        lane.sent
            .lock()
            .unwrap()
            .iter()
            .any(|l| l.starts_with("outbox sent s-a 1 off=")),
        "the post retired: {:?}",
        lane.sent.lock().unwrap()
    );
}

/// **A HELD COMMIT RIDES CLAMPED BELOW A RECORD STILL OWED** (critic item B08): with the
/// third of three delivered records still owed to its session, the commit the reply
/// carries stops just below it, as every inbox commit does ([`Bridge::commit_target`]),
/// not at what the bridge owes the group; the group resumes there.
///
/// NEGATIVE CONTROL: with the clamp off
/// ([`crate::sim::world::Mutant::HeldCommitPastTheFloor`]) the same round commits past the
/// owed record. The seeded worlds never reach a record owed to a session (only a lane
/// fault leaves one) under a held commit, so this is its control.
#[cfg(unix)]
#[test]
fn a_held_commit_rides_clamped_below_a_record_still_owed() {
    let (rode, records) = ride_with_a_record_owed(crate::fx::Fx::os());
    assert_eq!(rode, [records[1].0], "clamped below the owed record");
    let (rode, records) = ride_with_a_record_owed(crate::fx::Fx::os_without(
        crate::sim::world::Mutant::HeldCommitPastTheFloor,
    ));
    assert_eq!(
        rode,
        [records[2].0],
        "NEGATIVE CONTROL: unclamped, the commit passes the owed record"
    );
}

/// The round of [`a_held_commit_rides_clamped_below_a_record_still_owed`] on `fx`, its clock
/// held ([`crate::fx::Fx::on_a_held_clock`]) so the hold cannot lapse inside its settle:
/// what the held commit that rode the reply named, and the three records.
#[cfg(unix)]
fn ride_with_a_record_owed(fx: crate::fx::Fx) -> (Vec<u64>, Vec<BrokerRecord>) {
    let (lane, mut bridge) = FakeVerbLane::bridge(&[(1, "s-a"), (2, "s-b")]);
    bridge.fx = fx.on_a_held_clock();
    let (records, _reader, _socket, _broker, _server) = burst_rig(&lane, &mut bridge, "s-b", 3);
    bridge.in_round = true;
    for rec in &records {
        bridge.on_inbox_record(rec, None);
    }
    bridge.in_round = false;
    assert_eq!(bridge.inbox_pending, Some(records[2].0), "PRECONDITION");
    bridge
        .per_sid
        .lane_owed
        .insert("s-b".to_string(), Some(records[2].0));
    bridge.settle_owed();
    assert!(bridge.commit_held.is_some(), "PRECONDITION: held");

    let node = bridge.node.clone();
    bridge
        .indexed_subjects
        .insert(subject::post_lane("f", &node, "s-b", &node, "note"));
    *lane.outbox.lock().unwrap() =
        Some("post sid=s-a id=1 to=@s-b kind=note len=4\npong".to_string());
    bridge.mailbox.push_event("EVENT 1 post 1".to_string());
    assert!(bridge.park_and_take(aterm_spec::wait::MUST_HAPPEN));
    let rode: Vec<u64> = bridge
        .conn
        .as_ref()
        .expect("the link stands")
        .owed()
        .filter_map(|then| match then {
            Then::Commit { upto, .. } => Some(*upto),
            _ => None,
        })
        .collect();
    bridge.settle_acks();
    assert_eq!(
        bridge.committed,
        rode.first().copied(),
        "the broker acked the commit that rode"
    );
    (rode, records)
}

/// **A MIRROR SNAPSHOT INSIDE THE GRACE CARRIES THE HELD COMMIT IN ONE WRITE AND ONE
/// BATCH** (critic items C13 and E04). A delivery owes its commit; the delivery moves the
/// session's mail count, so the endpoint's line for it wakes the bus mirror, whose take
/// holds the session's snapshot. The mirror's send is the loop's next write: the commit
/// goes out ahead of the batch's slots in one burst, and the broker takes them into one
/// writer batch. Once the acks are read the group is through the delivery and every slot
/// is on the bus. So it goes in each order the loop meets the two in:
///
/// - [`MirrorPass::AfterTheSettle`]: the line comes while the loop parks, after the idle
///   round held the commit;
/// - [`MirrorPass::BeforeTheSettle`]: the line was read before the delivery's round
///   ended, so the mirror's pass, which runs ahead of the round's settle
///   ([`Bridge::mirror_then_round`]), finds the commit due and not yet held;
/// - [`MirrorPass::AfterAShortPark`]: the round held the commit with the mirror's pass due
///   now, so the park is cut short of the grace and takes nothing.
///
/// A paced note between two sessions of one node ([`a_paced_note_on_one_node`]) owes the
/// mirror the sender's snapshot before the delivery owes a commit: that take, which nothing
/// would carry, waits for the delivery ([`Bridge::mirror_rides_nothing`]), and the pass
/// behind the delivery takes both snapshots, its burst carrying the commit ahead of them:
/// `note | commit+evA+evB`, two writer batches.
///
/// NEGATIVE CONTROLS, one per order: with the mirror sending its slots bare
/// ([`crate::sim::world::Mutant::MirrorSendsItsSlotsBare`], the bridge before C13), with
/// its burst carrying only a commit already held
/// ([`crate::sim::world::Mutant::MirrorRidesOnlyAHeldCommit`]), and with every park that
/// takes nothing sending the held commit
/// ([`crate::sim::world::Mutant::ShortParkSendsTheHeldCommit`]), no write carries both the
/// commit and a slot, and the commit costs a writer batch of its own. With the sender's
/// take sent the round it falls due
/// ([`crate::sim::world::Mutant::MirrorSendsAnUnowedSlotAtOnce`], the bridge before E04),
/// the paced note is `note | evA | commit+evB`: the sender's snapshot goes in a write and a
/// batch of its own ahead of the delivery's commit, three batches. The rigs hold the
/// bridge's clock ([`crate::fx::Fx::on_a_held_clock`]), so no grace lapses between their
/// steps however slowly a loaded machine runs them.
#[cfg(unix)]
#[test]
fn a_mirror_snapshot_inside_the_grace_carries_the_held_commit_in_one_write_and_one_batch() {
    use crate::sim::world::Mutant;
    for (pass, mutant) in [
        (MirrorPass::AfterTheSettle, Mutant::MirrorSendsItsSlotsBare),
        (
            MirrorPass::BeforeTheSettle,
            Mutant::MirrorRidesOnlyAHeldCommit,
        ),
        (
            MirrorPass::AfterAShortPark,
            Mutant::ShortParkSendsTheHeldCommit,
        ),
    ] {
        let rode = mirror_after_a_delivery(crate::fx::Fx::os(), pass, true);
        assert_eq!(
            rode.owed.first().copied(),
            Some("commit"),
            "{pass:?}: the commit went out first: {:?}",
            rode.owed
        );
        assert!(
            rode.owed[1..].iter().all(|kind| *kind == "mirror") && rode.owed.len() > 1,
            "{pass:?}: every slot behind it, in the one burst: {:?}",
            rode.owed
        );
        assert_eq!(
            rode.writes,
            ["commit+evB"],
            "{pass:?}: one write, the commit ahead of the session's slots"
        );
        assert!(
            rode.snapshot,
            "{pass:?}: PRECONDITION: the take holds the snapshot"
        );
        assert_eq!(
            (rode.held_after_send, rode.batches),
            (false, 1),
            "{pass:?}: taken by the mirror's burst, the commit and every slot one writer batch"
        );
        assert!(
            rode.committed && rode.on_bus,
            "{pass:?}: the group is through the delivery"
        );

        let bare = mirror_after_a_delivery(crate::fx::Fx::os_without(mutant), pass, true);
        assert!(
            bare.writes.contains(&"commit".to_string())
                && bare.writes.iter().any(|w| w.starts_with("ev"))
                && !bare.writes.iter().any(|w| w.contains('+')),
            "{pass:?}: NEGATIVE CONTROL ({mutant:?}): the commit and the slots went in \
             writes of their own: {:?}",
            bare.writes
        );
        assert_eq!(
            bare.batches, 2,
            "{pass:?}: NEGATIVE CONTROL ({mutant:?}): the commit went alone, in a batch of \
             its own"
        );
        assert!(bare.committed && bare.on_bus, "{pass:?} ({mutant:?})");
    }

    let paced = a_paced_note_on_one_node(crate::fx::Fx::os(), Snapshots::Taken);
    assert_eq!(
        (paced.writes.as_slice(), paced.batches),
        (["note", "commit+evA+evB"].map(String::from).as_slice(), 2),
        "the sender's snapshot waited for the delivery's commit and rode it beside the \
         recipient's"
    );
    assert!(paced.committed, "the group is through the note");
    let at_once = a_paced_note_on_one_node(
        crate::fx::Fx::os_without(Mutant::MirrorSendsAnUnowedSlotAtOnce),
        Snapshots::Taken,
    );
    assert_eq!(
        (at_once.writes.as_slice(), at_once.batches),
        (
            ["note", "evA", "commit+evB"].map(String::from).as_slice(),
            3
        ),
        "NEGATIVE CONTROL (MirrorSendsAnUnowedSlotAtOnce): the sender's snapshot went in a \
         write and a batch of its own, ahead of the delivery's commit"
    );
    assert!(at_once.committed);
}

/// **A TAKE NOTHING CARRIES GOES ALONE ONCE ITS GRACE IS OUT** (critic item E04): a post to
/// a session on another node, or to a person, owes the mirror the sender's snapshot and no
/// delivery here owes a commit. The round that finds the take due holds it and parks the
/// rest of [`IDLE_ACK_GRACE`], all of it; the round after that park takes it and sends it in
/// a write of its own, one writer batch, and the next lands it. What ends the hold is the
/// bridge's clock, which the rig holds ([`crate::fx::Fx::on_a_held_clock`]) and only the
/// park moves on, so the hold neither lapses early on a loaded machine nor needs a wait.
///
/// NEGATIVE CONTROL: with the take sent the round it falls due
/// ([`crate::sim::world::Mutant::MirrorSendsAnUnowedSlotAtOnce`]) that round takes it.
#[cfg(unix)]
#[test]
fn a_take_nothing_carries_goes_alone_once_its_grace_is_out() {
    for (fx, held) in [
        (crate::fx::Fx::os(), true),
        (
            crate::fx::Fx::os_without(crate::sim::world::Mutant::MirrorSendsAnUnowedSlotAtOnce),
            false,
        ),
    ] {
        let (lane, mut bridge) = FakeVerbLane::bridge(&[(1, "s-a")]);
        bridge.fx = fx.on_a_held_clock();
        let (_records, _reader, _socket, broker, _server) = burst_rig(&lane, &mut bridge, "s-a", 0);
        bridge.mirror = Some(mirror_run::MirrorRun::acked_for_tests(
            &bridge.state,
            aterm_slog::TAKE_MAX,
        ));
        bridge.mirror_step();
        assert!(
            bridge.state.logpin_fence_held() && bridge.mirror_due(bridge.fx.now()).is_none(),
            "PRECONDITION: the open's first pass made the fence and took nothing"
        );
        bridge.roster_due = Instant::now() + aterm_spec::wait::MUST_HAPPEN;
        lane.mirror
            .lock()
            .unwrap()
            .logs
            .insert("s-a".to_string(), an_opened_session_log());
        bridge.mirror_wake();
        let batches = broker.stats().writer_batches;
        let writes = tap_shapes(&mut bridge);
        let takes = lane.mirror.lock().unwrap().takes;

        assert!(bridge.mirror_then_round());
        let first = lane.mirror.lock().unwrap().takes - takes;
        if !held {
            assert_eq!(
                first, 1,
                "NEGATIVE CONTROL: the round it fell due in took it"
            );
            continue;
        }
        assert_eq!(first, 0, "the round that found the take due held it");
        assert_eq!(
            bridge.parked,
            Some(IDLE_ACK_GRACE),
            "and parked the grace out"
        );
        assert!(bridge.mirror_then_round());
        assert_eq!(
            lane.mirror.lock().unwrap().takes - takes,
            1,
            "the round after the park took it: {:?}",
            writes.lock().unwrap()
        );
        assert!(bridge.mirror_then_round());
        assert_eq!(
            lane.mirror.lock().unwrap().landings.len(),
            1,
            "and the next landed it"
        );
        assert_eq!(
            (
                writes.lock().unwrap().as_slice(),
                broker.stats().writer_batches - batches
            ),
            (["evA".to_string()].as_slice(), 1),
            "the take went alone, in one write and one writer batch"
        );
    }
}

/// **A TAKE THAT COMES BACK EMPTY RUNS INSIDE THE COMMIT'S GRACE** (critic item E04's
/// review): a paced note sent inside a second of the last finds both sessions' snapshots
/// held back to the endpoint's rate, so every take comes back empty (`due=`). The sender's
/// take is held until the delivery owes its commit ([`Bridge::mirror_rides_nothing`]), and
/// the round after the delivery takes, carries nothing, and holds the commit for the loop's
/// next write from the round's start ([`Bridge::mirror_pass_then_settle`]): the commit's
/// [`IDLE_ACK_GRACE`] runs as it would with no take in the round, and the take's ctl round
/// trip runs inside the grace rather than ahead of it. Nothing rides the commit, which goes
/// alone: `note | commit`, two writer batches. The take's round trip costs the bridge's held
/// clock [`TAKE_ROUND_TRIP`] ([`a_paced_note_on_one_node`]), so a hold dated from the round's
/// start (no later than the lane is asked the take) is told apart from one dated after the
/// lane answered; the hold's date comes from the round's start, not from that cost.
///
/// NEGATIVE CONTROL: with the hold dated from the settle
/// ([`crate::sim::world::Mutant::CommitGraceFromTheSettle`], E04 as it first landed), the
/// grace starts after the lane answered the take, a round trip after it was asked.
#[cfg(unix)]
#[test]
fn a_take_that_comes_back_empty_runs_inside_the_commits_grace() {
    for (fx, inside) in [
        (crate::fx::Fx::os(), true),
        (
            crate::fx::Fx::os_without(crate::sim::world::Mutant::CommitGraceFromTheSettle),
            false,
        ),
    ] {
        let paced = a_paced_note_on_one_node(fx, Snapshots::HeldBack);
        let took = paced
            .took
            .expect("PRECONDITION: the round after the delivery took");
        let held = paced.held.expect("PRECONDITION: and held the commit");
        if inside {
            assert!(
                held <= took,
                "the commit's grace began before the lane was asked the take: held \
                 {held:?}, took {took:?}"
            );
        } else {
            assert!(
                held >= took + TAKE_ROUND_TRIP,
                "NEGATIVE CONTROL (CommitGraceFromTheSettle): the grace began after the \
                 take's round trip: held {held:?}, took {took:?}"
            );
        }
        assert_eq!(
            (paced.writes.as_slice(), paced.batches),
            (["note", "commit"].map(String::from).as_slice(), 2),
            "inside {inside}: nothing rode the commit, which went alone"
        );
        assert!(paced.committed, "the group is through the note");
    }
}

/// **A TAKE INSIDE THE ENDPOINT'S RATE HOLD GOES AFTER THE COMMIT'S WRITE** (critic item
/// E04c): the endpoint's answer to a take before a paced note came back empty and said when
/// the snapshot it holds back to its rate falls due, as in the fabric bench's latency phase,
/// where each round trip lies inside a second of the last. The note's wakes owe the mirror a
/// take that hold answers empty: it waits while the delivery's commit is owed
/// ([`Bridge::mirror_behind_the_commit`]), so no `log-take` goes between the post and the
/// commit's write, which goes alone, and one take follows it. `note | commit`, two writer
/// batches, as before.
///
/// NEGATIVE CONTROL: with the take sent the round it falls due
/// ([`crate::sim::world::Mutant::RateHeldTakeAheadOfTheCommit`], E04's review), the round
/// after the delivery takes, ahead of its settle and the commit's write.
#[cfg(unix)]
#[test]
fn a_take_inside_the_rate_hold_goes_after_the_commits_write() {
    for (fx, behind) in [
        (crate::fx::Fx::os(), true),
        (
            crate::fx::Fx::os_without(crate::sim::world::Mutant::RateHeldTakeAheadOfTheCommit),
            false,
        ),
    ] {
        let paced = a_paced_note_on_one_node(fx, Snapshots::InsideTheRateHold);
        assert_eq!(
            (paced.writes.as_slice(), paced.batches),
            (["note", "commit"].map(String::from).as_slice(), 2),
            "behind {behind}: nothing rode the commit, which went alone"
        );
        assert!(
            paced.committed,
            "behind {behind}: the group is through the note"
        );
        assert_eq!(
            paced.takes, 1,
            "behind {behind}: the note's wakes cost one take: {:?}",
            paced.takes_at
        );
        if behind {
            assert_eq!(
                paced.took, None,
                "the round after the delivery took nothing ahead of its settle"
            );
            assert_eq!(
                paced.takes_at,
                [0, 0],
                "no take went between the post and the commit's write"
            );
        } else {
            assert!(
                paced.took.is_some() && paced.takes_at == [0, 1],
                "NEGATIVE CONTROL (RateHeldTakeAheadOfTheCommit): the round after the \
                 delivery took ahead of the commit's write: took {:?}, takes by write {:?}",
                paced.took,
                paced.takes_at
            );
        }
    }
}

/// **A TAKE ANOTHER SESSION'S HOLD DOES NOT COVER STILL RIDES THE COMMIT** (critic item
/// E04c's review): the endpoint's rate hold is per session. A take before a paced note came
/// back empty with a third session's snapshot held back (`held=s-c`), so the bridge keeps a
/// rate hold that lasts past the test, but the note's sessions owe their snapshots at once.
/// The note's wakes are theirs, which that hold does not cover
/// ([`Bridge::mirror_woken_by`]), so the take goes as E04's rule has it: the sender's
/// snapshot waits for the delivery's commit, and the pass behind the delivery takes both
/// snapshots, its burst carrying the commit ahead of them. `note | commit+evA+evB`, two
/// writer batches, the shape of [`Snapshots::Taken`].
///
/// NEGATIVE CONTROL: with the take waiting for the commit's write whichever session woke it
/// ([`crate::sim::world::Mutant::RateHoldCoversEverySession`], E04c as it first landed), the
/// commit goes alone at its grace's end and both snapshots follow in a write of their own:
/// `note | commit | evA+evB`, three writer batches.
#[cfg(unix)]
#[test]
fn a_take_another_sessions_hold_does_not_cover_still_rides_the_commit() {
    for (fx, rides) in [
        (crate::fx::Fx::os(), true),
        (
            crate::fx::Fx::os_without(crate::sim::world::Mutant::RateHoldCoversEverySession),
            false,
        ),
    ] {
        let paced = a_paced_note_on_one_node(fx, Snapshots::OtherSessionHeld);
        assert!(
            paced.committed,
            "rides {rides}: the group is through the note"
        );
        if rides {
            assert_eq!(
                (paced.writes.as_slice(), paced.batches),
                (["note", "commit+evA+evB"].map(String::from).as_slice(), 2),
                "both snapshots rode the delivery's commit in one write"
            );
        } else {
            assert_eq!(
                (paced.writes.as_slice(), paced.batches),
                (
                    ["note", "commit", "evA+evB"].map(String::from).as_slice(),
                    3
                ),
                "NEGATIVE CONTROL (RateHoldCoversEverySession): the take waited behind the \
                 commit, which went alone"
            );
        }
    }
}

/// **A MIRROR PASS AT THE WINDOW'S EDGE STILL CARRIES THE DUE COMMIT** (critic item C13
/// review): with one ack short of [`PIPELINE_WINDOW`] owed when the pass starts, the
/// burst waits for room for the commit and a slot rather than writing its slots one bare
/// write each, so the first burst carries the commit ahead of the snapshot.
#[cfg(unix)]
#[test]
fn a_mirror_pass_at_the_windows_edge_still_carries_the_due_commit() {
    let rode = mirror_after_a_delivery(crate::fx::Fx::os(), MirrorPass::AtTheWindowsEdge, true);
    let sent: Vec<&str> = rode
        .owed
        .iter()
        .copied()
        .filter(|kind| *kind != "other")
        .collect();
    assert_eq!(
        sent.first().copied(),
        Some("commit"),
        "the commit went out ahead of every slot: {:?}",
        rode.owed
    );
    assert!(
        sent.len() > 1 && sent[1..].iter().all(|kind| *kind == "mirror"),
        "every slot behind it: {:?}",
        rode.owed
    );
    assert!(rode.snapshot, "PRECONDITION: the take holds the snapshot");
    assert!(
        !rode.held_after_send && rode.committed && rode.on_bus,
        "taken by the mirror's burst, and the group is through the delivery"
    );
}

/// What [`first_rounds_after_an_attach`] saw of each step, in order.
#[cfg(unix)]
#[derive(Debug, PartialEq, Eq)]
struct FirstRound {
    /// The lane's deliveries and the mirror's takes the step asked for, in its order.
    asked: Vec<&'static str>,
    /// Whether the open held the pin's fence once the step was over.
    fenced: bool,
}

/// One step of the loop after an attach ([`first_rounds_after_an_attach`]).
#[cfg(unix)]
#[derive(Clone, Copy, PartialEq, Eq)]
enum FirstStep {
    /// A turn of [`Bridge::mirror_then_round`].
    Round,
    /// The turn's first half: the read of the pin's settle's answer
    /// ([`Bridge::read_the_pins_settle`]), the hold's judgement ([`Bridge::mirror_yields`])
    /// and the pass it lets go ([`Bridge::mirror_step`]), with no settle or park behind them,
    /// so a round a hold would park for a minute is read without the wait.
    Pass,
    /// The next record comes back on the inbox face (one the attach's drain published).
    Arrive,
}

/// The broker link an attach's first rounds run on ([`first_rounds_after_an_attach`]).
#[cfg(unix)]
#[derive(Clone, Copy, PartialEq, Eq)]
enum FirstLink {
    /// One whose broker named no device: the pin's fence is the bridge's own two full
    /// flushes ([`StateDir::ensure_logpin_durable`]).
    Unnamed,
    /// One that said `Hello` and named the state dir's device (the test's scratch holds the
    /// log and the state dir), as every attach of a bridge whose log shares its volume
    /// does: the fence is the pin's barriers and the broker's settle (critic item F07,
    /// [`Bridge::make_the_pin_fence`]). Apple only: elsewhere no log's full flush drains its
    /// device (`astream_broker::LogDevice::flush_drains_device`), so no broker settles the pin.
    #[cfg_attr(
        not(target_vendor = "apple"),
        expect(dead_code, reason = "no broker settles the pin off Apple")
    )]
    Hello,
    /// [`FirstLink::Hello`] on a broker whose every full flush fails
    /// (`astream_broker::InjectedFaults::sync`), so it refuses the settle.
    Refusing,
}

/// What [`first_rounds_after_an_attach`] saw: each step's [`FirstRound`], the wait tally's
/// [`crate::wait_tally::Row::LogpinFence`] count over them (the pin's barriers and each
/// fence of the bridge's own full flushes), and the settles the link sent.
#[cfg(unix)]
struct FirstRun {
    rounds: Vec<FirstRound>,
    fences: u64,
    settles: usize,
}

/// An attach of a fresh open (no fence on `logpin`) on `link`, whose first pass is held for
/// at most `hold`, its drain having listed `drained` posts to publish, with `queued` inbox
/// records waiting for the loop as that pass comes due and one more for each
/// [`FirstStep::Arrive`]; then `steps` on the live world with `off` switched off.
///
/// On a link that settles, every try of the loop's waits for the answer it looks for
/// ([`crate::sim::world::Mutant::WaitForEveryAck`]), so a step that looks for the settle's
/// answer finds it in and a step that does not leaves it unread: the rounds say which steps
/// look, never how soon the broker's full flush returned.
#[cfg(unix)]
fn first_rounds_after_an_attach(
    link: FirstLink,
    off: &[crate::sim::world::Mutant],
    queued: u64,
    hold: Duration,
    drained: u64,
    steps: &[FirstStep],
) -> FirstRun {
    first_rounds_after_a_drain(link, off, queued, hold, Drain::Counted(drained), steps)
}

/// What the attach's drain published before its first pass came due
/// ([`first_rounds_after_a_drain`]).
#[cfg(unix)]
#[derive(Clone, Copy)]
enum Drain {
    /// That many posts to `s-b`, a session this node hosts, counted as its drain counts
    /// each ([`Bridge::mirror_drained_post`]), their records left for
    /// [`FirstStep::Arrive`].
    Counted(u64),
    /// A real drain of this outbox listing ([`Bridge::drain_outbox`]), its posts' records
    /// left on the broker, never on the inbox face: the rig reads none. `s-w` is `n-far`'s
    /// session, pinned there and advertised live, so a post `to=@s-w@n-far` routes to its
    /// lane.
    Listed(&'static str),
}

/// [`first_rounds_after_an_attach`], its drain's posts `drained`.
#[cfg(unix)]
fn first_rounds_after_a_drain(
    link: FirstLink,
    off: &[crate::sim::world::Mutant],
    queued: u64,
    hold: Duration,
    drained: Drain,
    steps: &[FirstStep],
) -> FirstRun {
    use crate::wait_tally::{Row, Tally, FILE};
    let (lane, mut bridge) = FakeVerbLane::bridge(&[(1, "s-a"), (2, "s-b")]);
    let mut off = off.to_vec();
    if link != FirstLink::Unnamed {
        off.push(crate::sim::world::Mutant::WaitForEveryAck);
    }
    bridge.fx = crate::fx::Fx::os_without_each(&off);
    let arrivals = steps.iter().filter(|s| **s == FirstStep::Arrive).count();
    let (records, _writer, _socket, broker, _server) = burst_rig(
        &lane,
        &mut bridge,
        "s-b",
        queued + u64::try_from(arrivals).unwrap(),
    );
    if link != FirstLink::Unnamed {
        assert!(
            bridge.read_log_identity().unwrap().is_some(),
            "PRECONDITION: the link says Hello, as an attach does"
        );
    }
    assert_eq!(
        bridge.broker_settles_the_pin(),
        link != FirstLink::Unnamed,
        "PRECONDITION: a broker that named the state dir's device settles the pin, and one \
         that named none does not"
    );
    if link == FirstLink::Refusing {
        broker.inject_faults(astream_broker::InjectedFaults {
            write: false,
            sync: true,
            truncate: false,
        });
    }
    let settles = Arc::new(std::sync::Mutex::new(0_usize));
    let tapped = Arc::clone(&settles);
    bridge
        .conn
        .as_mut()
        .expect("the link stands")
        .tap_writes(move |frames| {
            *tapped.lock().unwrap() += frames.iter().filter(|(t, _)| t.is_settle()).count();
        });
    let tally = bridge.state.root().join(FILE);
    bridge.tally_to(&tally);
    let mut run = mirror_run::MirrorRun::acked_for_tests(&bridge.state, aterm_slog::TAKE_MAX);
    run.hold_first_pass_for_tests(hold);
    bridge.mirror = Some(run);
    let mut log = aterm_slog::Log::with_limits(
        1,
        aterm_slog::Limits::new(8, 1 << 20).expect("a small ring"),
    );
    log.open_session(
        aterm_slog::Kind::Spawn,
        aterm_slog::Src::Aterm,
        aterm_slog::Conf::Exact,
        aterm_slog::Actor::Aterm,
        aterm_slog::Fields::new(),
        aterm_slog::Stamp::new(1, 1),
    )
    .expect("the session opens");
    lane.mirror
        .lock()
        .unwrap()
        .logs
        .insert("s-b".to_string(), log);
    bridge.mirror_attached();
    match drained {
        Drain::Counted(n) => {
            let hosted = format!("/f/f/in/{0}/s-b/{0}/note", bridge.node);
            for _ in 0..n {
                bridge.mirror_drained_post(&hosted);
            }
        }
        Drain::Listed(listing) => {
            bridge.state.pin("s-w", "n-far").expect("pin");
            bridge
                .connect()
                .unwrap()
                .0
                .publish(
                    9_000,
                    1,
                    &subject::session_face("f", "n-far", "s-w", "presence"),
                    b"v=1 t=1 inc=1 state=live",
                )
                .expect("n-far advertises s-w");
            *lane.outbox.lock().unwrap() = Some(listing.to_string());
            bridge.drain_outbox();
            let posts = listing.matches("post sid=").count();
            let sent = lane.sent.lock().unwrap().clone();
            assert!(
                sent.len() == posts
                    && sent
                        .iter()
                        .all(|l| l.starts_with("outbox sent ") && !l.contains("off=-")),
                "PRECONDITION: every listed post landed: {sent:#?}"
            );
        }
    }
    assert!(
        !bridge.state.logpin_fence_held(),
        "PRECONDITION: a fresh open holds no fence"
    );
    let generation = bridge.mailbox.broker_generation(Source::Inbox);
    let mut records = records.into_iter();
    let mut push = |bridge: &mut Bridge| {
        let record = records.next().expect("a record for each arrival");
        let _ = bridge
            .mailbox
            .push(Source::Inbox, record, None, generation, bridge.fx.now());
    };
    for _ in 0..queued {
        push(&mut bridge);
    }
    let now = bridge.fx.now();
    assert_eq!(
        bridge.mirror_due(now),
        Some(now),
        "PRECONDITION: the pass is due now"
    );
    let mut seen = Vec::new();
    for step in steps {
        let before = lane.requests.lock().unwrap().len();
        match step {
            FirstStep::Round => assert!(bridge.mirror_then_round()),
            FirstStep::Pass => {
                bridge.read_the_pins_settle();
                if !bridge.mirror_yields() {
                    bridge.mirror_step();
                }
            }
            FirstStep::Arrive => push(&mut bridge),
        }
        let asked = lane.requests.lock().unwrap()[before..]
            .iter()
            .filter_map(|line| {
                if line.starts_with("deliver ") {
                    Some("deliver")
                } else if line.starts_with("log-take ") {
                    Some("log-take")
                } else {
                    None
                }
            })
            .collect();
        seen.push(FirstRound {
            asked,
            fenced: bridge.state.logpin_fence_held(),
        });
    }
    bridge.tally_idle();
    let t = Tally::parse(&std::fs::read_to_string(&tally).unwrap()).unwrap();
    let settles = *settles.lock().unwrap();
    FirstRun {
        rounds: seen,
        fences: t.row(Row::LogpinFence).count,
        settles,
    }
}

/// **THE FIRST DELIVERY AFTER AN ATTACH GOES AHEAD OF THE PIN'S FENCE** (critic item D06),
/// on a link whose broker named no device ([`FirstLink::Unnamed`]). A fresh open holds no
/// fence on `logpin`, so its first mirror pass writes the pin durably (two full flushes of
/// the bridge's own, the wait tally's `logpin_fence` row) before it takes. That pass is held
/// for what the attach owes a delivery, and follows it, the fence still ahead of its take
/// (the seeded worlds' `mirror fence:` law):
/// - two records queued: two rounds that deliver one each and leave the fence unmade, then
///   a round that finds nothing queued and whose pass makes the fence and takes;
/// - nothing queued and no post drained: the pass is not held;
/// - a drain that listed a post, its record not yet back as the pass comes due: the hold
///   waits for it past an empty queue, the record is delivered once it comes, and the hold
///   ends at the first turn after it (the whole hold is a minute, so the loop, never the
///   clock, ends it);
/// - a hold that is out lets the pass go ahead of what is still queued (the bound: a stream
///   of items cannot starve the pass).
///
/// Each hold but the last is a minute long ([`aterm_spec::wait::MUST_HAPPEN`]), so what ends
/// it is the loop going idle, never the clock. The same rounds on a link that settles the
/// pin are [`the_first_take_after_an_attach_goes_at_the_round_that_reads_the_settle`].
///
/// NEGATIVE CONTROLS: with the pass ahead of what is queued
/// ([`crate::sim::world::Mutant::MirrorAheadOfTheFirstDelivery`], the loop before D06), the
/// first round makes the fence and takes before it delivers; with the drained posts
/// forgotten ([`crate::sim::world::Mutant::FirstPassForgetsTheDrainedPosts`], D06's first
/// form, which measured no gain), the pass goes ahead of the post's record; and with the
/// record not counted off ([`crate::sim::world::Mutant::FirstPassAwaitsPastItsRecords`]),
/// the pass is still held after the record came.
#[cfg(unix)]
#[test]
fn the_first_delivery_after_an_attach_goes_ahead_of_the_pins_fence() {
    use crate::sim::world::Mutant;
    use FirstLink::Unnamed;
    use FirstStep::{Arrive, Pass, Round};
    let long = aterm_spec::wait::MUST_HAPPEN;
    let round = |asked: &[&'static str], fenced| FirstRound {
        asked: asked.to_vec(),
        fenced,
    };
    let run = first_rounds_after_an_attach(Unnamed, &[], 2, long, 0, &[Round; 3]);
    assert_eq!(
        run.rounds,
        [
            round(&["deliver"], false),
            round(&["deliver"], false),
            round(&["log-take"], true),
        ],
        "the queued deliveries first, then the pass, its fence ahead of its take"
    );
    assert_eq!(run.fences, 1, "the open's one fence, timed in its own row");
    assert_eq!(
        run.settles, 0,
        "PRECONDITION: no settle on a link that named no device"
    );

    let run = first_rounds_after_an_attach(Unnamed, &[], 0, long, 0, &[Round]);
    assert_eq!(
        run.rounds,
        [round(&["log-take"], true)],
        "nothing queued and nothing drained: the pass is not held"
    );
    assert_eq!(run.fences, 1);

    let awaited = [Pass, Pass, Arrive, Round, Pass];
    let run = first_rounds_after_an_attach(Unnamed, &[], 0, long, 1, &awaited);
    assert_eq!(
        run.rounds,
        [
            round(&[], false),
            round(&[], false),
            round(&[], false),
            round(&["deliver"], false),
            round(&["log-take"], true),
        ],
        "a drained post is waited for past an empty queue, delivered once its record comes, \
         and the pass goes at the first turn after it"
    );
    assert_eq!(run.fences, 1);

    let run = first_rounds_after_an_attach(
        Unnamed,
        &[Mutant::FirstPassForgetsTheDrainedPosts],
        0,
        long,
        1,
        &awaited[..1],
    );
    assert_eq!(
        run.rounds,
        [round(&["log-take"], true)],
        "NEGATIVE CONTROL: with the drained post forgotten, the pass, its fence and its take \
         go ahead of the post's record"
    );
    let run = first_rounds_after_an_attach(
        Unnamed,
        &[Mutant::FirstPassAwaitsPastItsRecords],
        0,
        long,
        1,
        &awaited,
    );
    assert_eq!(
        run.rounds.last(),
        Some(&round(&[], false)),
        "NEGATIVE CONTROL: with the record not counted off, the pass is still held after it"
    );

    let run = first_rounds_after_an_attach(Unnamed, &[], 2, Duration::ZERO, 1, &[Round; 2]);
    assert_eq!(
        run.rounds,
        [
            round(&["deliver"], false),
            round(&["log-take", "deliver"], true),
        ],
        "a hold that is out: the pass goes ahead of what is still queued"
    );
    assert_eq!(run.fences, 1);

    let run = first_rounds_after_an_attach(
        Unnamed,
        &[Mutant::MirrorAheadOfTheFirstDelivery],
        2,
        long,
        1,
        &[Round],
    );
    assert_eq!(
        run.rounds,
        [round(&["log-take", "deliver"], true)],
        "NEGATIVE CONTROL: the pass, its fence and its take ahead of the first delivery"
    );
    assert_eq!(run.fences, 1);
}

/// **A POST TO ANOTHER NODE'S SESSION DOES NOT HOLD THE FIRST PASS** (critic item G04). The
/// attach's drain publishes a post to `n-far`'s session, whose record lands on `n-far`'s
/// lane, never on this node's inbox face, so nothing the loop takes counts it off: the pass
/// due after the drain is not held for it, and makes the fence and takes at once. A post
/// the same drain publishes to a session this node hosts is awaited, as D06 awaits it, past
/// an empty queue (the hold is a minute, so only the loop ends it).
///
/// NEGATIVE CONTROLS: counting every drained post
/// ([`crate::sim::world::Mutant::FirstPassCountsAnotherNodesPost`], D06's count of every
/// post a drain listed), the remote post holds the pass; with the drained posts forgotten
/// ([`crate::sim::world::Mutant::FirstPassForgetsTheDrainedPosts`]), the hosted one does
/// not, which shows the drain's own call counts it.
#[cfg(unix)]
#[test]
fn a_post_to_another_nodes_session_does_not_hold_the_first_pass() {
    use crate::sim::world::Mutant;
    use FirstLink::Unnamed;
    use FirstStep::Pass;
    let long = aterm_spec::wait::MUST_HAPPEN;
    let round = |asked: &[&'static str], fenced| FirstRound {
        asked: asked.to_vec(),
        fenced,
    };
    const REMOTE: &str = "post sid=s-a id=1 to=@s-w@n-far kind=note len=4\nmail";
    const HOSTED: &str = "post sid=s-a id=1 to=@s-b kind=note len=4\nmail";
    let pass = |off: &[Mutant], listing| {
        first_rounds_after_a_drain(Unnamed, off, 0, long, Drain::Listed(listing), &[Pass]).rounds
    };
    assert_eq!(
        pass(&[], REMOTE),
        [round(&["log-take"], true)],
        "the remote post's record never comes back here: the pass goes at once"
    );
    assert_eq!(
        pass(&[], HOSTED),
        [round(&[], false)],
        "the hosted post's record comes back on the inbox face: the pass waits for it"
    );
    assert_eq!(
        pass(&[Mutant::FirstPassCountsAnotherNodesPost], REMOTE),
        [round(&[], false)],
        "NEGATIVE CONTROL: counted, the remote post holds the pass for a record that never \
         comes"
    );
    assert_eq!(
        pass(&[Mutant::FirstPassForgetsTheDrainedPosts], HOSTED),
        [round(&["log-take"], true)],
        "NEGATIVE CONTROL: with the drained posts forgotten, the hosted post does not hold it"
    );
}

/// **THE FIRST TAKE AFTER AN ATTACH GOES AT THE ROUND THAT READS THE SETTLE** (critic item
/// F07's review of D06), on a link that said `Hello` and named the state dir's device
/// ([`FirstLink::Hello`]), as every attach of a bridge whose log shares its volume does. The
/// first pass writes the pin by its barriers (the one `logpin_fence` row) and sends the
/// broker's settle, and the take waits for the settle's answer ([`Bridge::mirror_due`]).
/// D06's hold and its bound stand: the deliveries an attach owes go first, and once the hold
/// is out the pass goes whatever is still queued. Here that is the round that reads the
/// answer: every round's pass looks for it first ([`Bridge::read_the_pins_settle`]), and the
/// take it lets go is the open's first, which no commit holds ([`MirrorRun::fence_settled`]).
/// - two records queued: two rounds that deliver one each, a round whose pass sends the
///   settle and whose idle moment reads its answer, and the round after it takes;
/// - nothing queued and nothing drained: the pass is not held, and its settle is read at the
///   same round's idle moment;
/// - a drained post: the pass after its record sends the settle, and the next pass reads the
///   answer and takes;
/// - a hold that is out, three records queued: the second round's pass sends the settle and
///   the round delivers, and the third round reads the answer and takes ahead of the record
///   still queued.
///
/// NEGATIVE CONTROLS: with the answer read only where F07 first read it
/// ([`crate::sim::world::Mutant::SettleReadOnlyWhenIdle`]), the third round of the hold that
/// is out delivers with the settle still unread, and so would every round while items keep
/// coming; with the take the settle lets go held for a commit to carry
/// ([`crate::sim::world::Mutant::SettledFenceTakeWaitsForACommit`]), that round makes the
/// fence and delivers but does not take, and a take after the idle moment's read waits a
/// grace more; and with the pass ahead of what is queued
/// ([`crate::sim::world::Mutant::MirrorAheadOfTheFirstDelivery`]), the first round sends the
/// settle ahead of its delivery and the second takes.
#[cfg(target_vendor = "apple")]
#[test]
fn the_first_take_after_an_attach_goes_at_the_round_that_reads_the_settle() {
    use crate::sim::world::Mutant;
    use FirstLink::Hello;
    use FirstStep::{Arrive, Pass, Round};
    let long = aterm_spec::wait::MUST_HAPPEN;
    let round = |asked: &[&'static str], fenced| FirstRound {
        asked: asked.to_vec(),
        fenced,
    };
    let run = first_rounds_after_an_attach(Hello, &[], 2, long, 0, &[Round; 4]);
    assert_eq!(
        run.rounds,
        [
            round(&["deliver"], false),
            round(&["deliver"], false),
            round(&[], true),
            round(&["log-take"], true),
        ],
        "the queued deliveries first, then the pass and its settle, then the take"
    );
    assert_eq!(
        run.fences, 1,
        "the open's one fence write, its barriers, timed in its own row"
    );
    assert_eq!(run.settles, 1, "one settle, the fence's");

    let run = first_rounds_after_an_attach(Hello, &[], 0, long, 0, &[Round; 2]);
    assert_eq!(
        run.rounds,
        [round(&[], true), round(&["log-take"], true)],
        "nothing queued and nothing drained: the pass is not held"
    );
    assert_eq!((run.fences, run.settles), (1, 1));

    let awaited = [Pass, Pass, Arrive, Round, Pass, Pass];
    let run = first_rounds_after_an_attach(Hello, &[], 0, long, 1, &awaited);
    assert_eq!(
        run.rounds,
        [
            round(&[], false),
            round(&[], false),
            round(&[], false),
            round(&["deliver"], false),
            round(&[], false),
            round(&["log-take"], true),
        ],
        "a drained post is waited for, delivered once its record comes, the pass after it \
         sends the settle, and the next pass reads its answer and takes"
    );
    assert_eq!((run.fences, run.settles), (1, 1));

    let run = first_rounds_after_an_attach(Hello, &[], 3, Duration::ZERO, 1, &[Round; 3]);
    assert_eq!(
        run.rounds,
        [
            round(&["deliver"], false),
            round(&["deliver"], false),
            round(&["log-take", "deliver"], true),
        ],
        "a hold that is out: the round that reads the settle takes ahead of what is still \
         queued"
    );
    assert_eq!((run.fences, run.settles), (1, 1));

    let run = first_rounds_after_an_attach(
        Hello,
        &[Mutant::SettleReadOnlyWhenIdle],
        3,
        Duration::ZERO,
        1,
        &[Round; 3],
    );
    assert_eq!(
        run.rounds.last(),
        Some(&round(&["deliver"], false)),
        "NEGATIVE CONTROL: with the answer read only at an idle moment, a busy round leaves \
         it unread and the take waits for the queue to drain"
    );
    let run = first_rounds_after_an_attach(
        Hello,
        &[Mutant::SettledFenceTakeWaitsForACommit],
        3,
        Duration::ZERO,
        1,
        &[Round; 3],
    );
    assert_eq!(
        run.rounds.last(),
        Some(&round(&["deliver"], true)),
        "NEGATIVE CONTROL: with the settled fence's take held for a commit, the round that \
         reads the answer does not take"
    );
    let run = first_rounds_after_an_attach(
        Hello,
        &[Mutant::SettledFenceTakeWaitsForACommit],
        2,
        long,
        0,
        &[Round; 4],
    );
    assert_eq!(
        run.rounds.last(),
        Some(&round(&[], true)),
        "NEGATIVE CONTROL: nor does the round after an idle moment read it"
    );

    let run = first_rounds_after_an_attach(
        Hello,
        &[Mutant::MirrorAheadOfTheFirstDelivery],
        2,
        long,
        1,
        &[Round; 2],
    );
    assert_eq!(
        run.rounds,
        [
            round(&["deliver"], false),
            round(&["log-take", "deliver"], true),
        ],
        "NEGATIVE CONTROL: the pass and its settle ahead of the first delivery, the take at \
         the next round"
    );
    let run = first_rounds_after_an_attach(Hello, &[], 2, long, 1, &[Round; 2]);
    assert_eq!(
        run.rounds,
        [round(&["deliver"], false), round(&["deliver"], false)],
        "which the hold puts behind both deliveries"
    );
}

/// **A SETTLE THE BROKER REFUSED LEAVES THE PIN TO THE BRIDGE'S OWN FULL FLUSH** (critic item
/// F07): on a link that named the state dir's device (Apple only, as [`FirstLink::Hello`]), a
/// broker whose full flush fails (`astream_broker::InjectedFaults::sync`) refuses the settle
/// (`durability flush failed`, [`Bridge::pin_settle_refused`]). The fence is then the
/// bridge's own durable replace of the pin ([`StateDir::ensure_logpin_durable`]: a second
/// `logpin_fence` row behind the barriers'), made by the next pass, which takes; that broker
/// is not asked again on this link ([`Bridge::broker_settles_the_pin`]).
///
/// NEGATIVE CONTROLS: with the refusal forgotten
/// ([`crate::sim::world::Mutant::SettleRefusalForgotten`]), the next pass asks again and
/// takes nothing; with the refusal taken as the answer
/// ([`crate::sim::world::Mutant::RefusedSettleMakesTheFence`]), the pin is held as the fence
/// on its barriers alone, no full flush behind them.
#[cfg(target_vendor = "apple")]
#[test]
fn a_refused_settle_leaves_the_pin_to_the_bridges_own_full_flush() {
    use crate::sim::world::Mutant;
    use FirstLink::Refusing;
    use FirstStep::{Pass, Round};
    let long = aterm_spec::wait::MUST_HAPPEN;
    let round = |asked: &[&'static str], fenced| FirstRound {
        asked: asked.to_vec(),
        fenced,
    };
    let run = first_rounds_after_an_attach(Refusing, &[], 0, long, 0, &[Round, Pass]);
    assert_eq!(
        run.rounds,
        [round(&[], false), round(&["log-take"], true)],
        "the refused settle makes no fence; the next pass makes its own and takes"
    );
    assert_eq!(
        run.fences, 2,
        "the pin's barriers, then the bridge's own durable replace of it"
    );
    assert_eq!(run.settles, 1, "the broker that refused is not asked again");

    let run = first_rounds_after_an_attach(
        Refusing,
        &[Mutant::SettleRefusalForgotten],
        0,
        long,
        0,
        &[Round, Pass],
    );
    assert_eq!(
        (run.rounds, run.settles),
        (vec![round(&[], false), round(&[], false)], 2),
        "NEGATIVE CONTROL: with the refusal forgotten, the next pass asks again and takes \
         nothing"
    );
    let run = first_rounds_after_an_attach(
        Refusing,
        &[Mutant::RefusedSettleMakesTheFence],
        0,
        long,
        0,
        &[Round, Pass],
    );
    assert_eq!(
        (run.rounds[0].fenced, run.fences),
        (true, 1),
        "NEGATIVE CONTROL: with the refusal taken as the answer, the pin is the fence on its \
         barriers alone"
    );
}

/// **A PASS THAT MAKES THE PIN'S FENCE SENDS THE COMMIT ITS DELIVERY OWES AHEAD OF IT**
/// (critic item D06's review). In a fresh open the first pass after a delivery, which the
/// hold lets go behind it ([`Bridge::mirror_yields`]), makes the fence (two full flushes)
/// before its take, so the commit the delivery owes goes out alone ahead of the fence
/// ([`Bridge::commit_ahead_of_the_fence`]) rather than in the pass's burst behind it: the
/// commit's write and the burst's, two writer batches, and the group through the delivery.
///
/// NEGATIVE CONTROL: with the commit left for the burst
/// ([`crate::sim::world::Mutant::CommitBehindTheFence`]), it rides the burst behind the
/// fence, one batch.
#[cfg(unix)]
#[test]
fn a_pass_that_makes_the_pins_fence_sends_the_owed_commit_ahead_of_it() {
    use crate::sim::world::Mutant;
    for pass in [MirrorPass::AfterTheSettle, MirrorPass::BeforeTheSettle] {
        let ahead = mirror_after_a_delivery(crate::fx::Fx::os(), pass, false);
        assert_eq!(
            (ahead.held_after_send, ahead.batches),
            (false, 2),
            "{pass:?}: the commit alone, then the burst: {:?}",
            ahead.owed
        );
        assert!(
            ahead.snapshot && ahead.committed && ahead.on_bus,
            "{pass:?}: the take, the group through the delivery and every slot on the bus"
        );
        let behind = mirror_after_a_delivery(
            crate::fx::Fx::os_without(Mutant::CommitBehindTheFence),
            pass,
            false,
        );
        assert_eq!(
            (behind.owed.first().copied(), behind.batches),
            (Some("commit"), 1),
            "{pass:?}: NEGATIVE CONTROL: the commit rode the burst behind the fence: {:?}",
            behind.owed
        );
    }
}

/// The order the loop meets a delivery's commit and the mirror pass it owes in
/// ([`mirror_after_a_delivery`]).
#[cfg(unix)]
#[derive(Clone, Copy, Debug)]
enum MirrorPass {
    /// The idle round holds the commit, then the endpoint's line wakes the mirror.
    AfterTheSettle,
    /// The mirror is owed already when the delivery's round ends: its pass comes first.
    BeforeTheSettle,
    /// The idle round holds the commit with the mirror's pass due now: the park is zero.
    AfterAShortPark,
    /// [`MirrorPass::BeforeTheSettle`] with the pipeline window one ack short of full
    /// when the pass starts, the session's face already indexed (no budget read settles
    /// the window first).
    AtTheWindowsEdge,
}

/// What [`mirror_after_a_delivery`] saw.
#[cfg(unix)]
struct MirrorRide {
    /// What the mirror's send left owed on the link, oldest first.
    owed: Vec<&'static str>,
    /// Every write from the round's park (or the mirror's send) on, each one shape step
    /// ([`shape_step`]).
    writes: Vec<String>,
    /// Whether a slot of the take was a snapshot.
    snapshot: bool,
    /// Whether a commit was still held once the mirror's send went out.
    held_after_send: bool,
    /// The writer batches from the round's park (or the mirror's send) through the
    /// commit's landing.
    batches: u64,
    /// Whether the group ended through the delivery.
    committed: bool,
    /// Whether every slot ended on the bus.
    on_bus: bool,
}

/// A delivery that owes its commit, then the mirror pass the endpoint's line owes, met in
/// the order `pass` names, on `fx` with its clock held ([`crate::fx::Fx::on_a_held_clock`],
/// so the commit's grace cannot lapse between the steps), in an open that holds the pin's
/// fence (`fenced`, the steady state C13 is about) or a fresh one, whose pass makes it; once
/// its acks are read, a commit still owed goes alone, as the backstop sends it.
#[cfg(unix)]
fn mirror_after_a_delivery(fx: crate::fx::Fx, pass: MirrorPass, fenced: bool) -> MirrorRide {
    let (lane, mut bridge) = FakeVerbLane::bridge(&[(1, "s-a"), (2, "s-b")]);
    bridge.fx = fx.on_a_held_clock();
    let (records, _reader, _socket, broker, _server) = burst_rig(&lane, &mut bridge, "s-b", 1);
    bridge.mirror = Some(mirror_run::MirrorRun::acked_for_tests(
        &bridge.state,
        aterm_slog::TAKE_MAX,
    ));
    if fenced {
        assert!(
            bridge.mirror_key_on_the_media() && bridge.state.logpin_fence_held(),
            "PRECONDITION: the open made the pin's fence"
        );
    }
    let mut log = aterm_slog::Log::with_limits(
        1,
        aterm_slog::Limits::new(8, 1 << 20).expect("a small ring"),
    );
    log.open_session(
        aterm_slog::Kind::Spawn,
        aterm_slog::Src::Aterm,
        aterm_slog::Conf::Exact,
        aterm_slog::Actor::Aterm,
        aterm_slog::Fields::new(),
        aterm_slog::Stamp::new(1, 1),
    )
    .expect("the session opens");
    lane.mirror
        .lock()
        .unwrap()
        .logs
        .insert("s-b".to_string(), log);

    bridge.in_round = true;
    bridge.on_inbox_record(&records[0], None);
    bridge.in_round = false;
    assert_eq!(
        bridge.inbox_pending,
        Some(records[0].0),
        "PRECONDITION: the delivery owes its commit"
    );
    assert!(bridge.mailbox.is_idle());
    assert_eq!(bridge.conn.as_ref().map(Link::unacked), Some(0));
    let batches = broker.stats().writer_batches;
    let writes = tap_shapes(&mut bridge);
    match pass {
        MirrorPass::AfterTheSettle => {
            bridge.settle_owed();
            assert!(
                bridge.commit_held.is_some(),
                "PRECONDITION: the idle round holds it"
            );
            bridge.mirror_wake();
        }
        MirrorPass::BeforeTheSettle => {
            bridge.mirror_wake();
            assert!(
                bridge.commit_held.is_none(),
                "PRECONDITION: no round held it yet"
            );
        }
        MirrorPass::AtTheWindowsEdge => {
            let node = bridge.node.clone();
            bridge
                .indexed_subjects
                .insert(subject::session_face("f", &node, "s-b", "ev"));
            bridge.in_round = true;
            for i in 0..PIPELINE_WINDOW - 1 {
                bridge.publish_ev(&format!("ahead {i}"));
            }
            bridge.in_round = false;
            assert_eq!(
                bridge.conn.as_ref().map(Link::unacked),
                Some(PIPELINE_WINDOW - 1),
                "PRECONDITION: room for one write"
            );
            bridge.mirror_wake();
            assert!(
                bridge.commit_held.is_none(),
                "PRECONDITION: no round held it yet"
            );
        }
        MirrorPass::AfterAShortPark => {
            bridge.mirror_wake();
            bridge.settle_owed();
            assert!(
                bridge.commit_held.is_some(),
                "PRECONDITION: the idle round holds it"
            );
            let wait = bridge.park_wait(bridge.fx.now());
            assert_eq!(
                wait,
                Duration::ZERO,
                "PRECONDITION: the mirror's pass is due now"
            );
            assert!(bridge.park_and_take(wait));
            assert_eq!(
                bridge.parked,
                Some(Duration::ZERO),
                "PRECONDITION: the park took nothing"
            );
        }
    }
    bridge.mirror_step();
    let link = bridge.conn.as_ref().expect("the link stands");
    let owed: Vec<&'static str> = link
        .owed()
        .map(|then| match then {
            Then::Commit { .. } => "commit",
            Then::Mirror { .. } => "mirror",
            _ => "other",
        })
        .collect();
    let held_after_send = bridge.commit_held.is_some();
    bridge.settle_acks();
    bridge.flush_inbox_commit();
    bridge.settle_acks();
    // A pass that found no commit to carry held its take (critic item E04): it is made once
    // its hold is out.
    if bridge
        .mirror
        .as_deref()
        .is_some_and(|m| m.uncarried().is_some())
    {
        pass_past_its_hold(&mut bridge);
        bridge.settle_acks();
    }
    let snapshot = bridge
        .mirror
        .as_deref()
        .is_some_and(|m| m.batch().0.iter().any(|s| s.body.contains(" op=snap ")));
    let on_bus = bridge.mirror.as_deref().is_some_and(|m| {
        !m.batch().1.is_empty()
            && m.batch()
                .1
                .iter()
                .all(|o| matches!(o, mirror_run::Outcome::OnBus(_)))
    });
    let writes = writes.lock().unwrap().clone();
    MirrorRide {
        owed,
        writes,
        snapshot,
        held_after_send,
        batches: broker.stats().writer_batches - batches,
        committed: bridge.committed == Some(records[0].0) && bridge.inbox_pending.is_none(),
        on_bus,
    }
}

/// The pass the loop owes now, made without waiting out the hold of a take nothing would
/// carry (critic item E04): the pass that finds it due holds it, and once the bridge's held
/// clock ([`crate::fx::Fx::on_a_held_clock`]) is moved past the hold's grace the next one
/// takes.
fn pass_past_its_hold(bridge: &mut Bridge) {
    bridge.mirror_step();
    if bridge
        .mirror
        .as_deref()
        .is_some_and(|m| m.uncarried().is_some())
    {
        bridge.fx.advance(IDLE_ACK_GRACE);
        bridge.mirror_step();
    }
}

/// One write's frames named as the fabric bench names a paced note's records in its shapes:
/// `note` a post, `commit` the inbox commit, `evA` and `evB` a mirror slot of `s-a`'s log
/// and of `s-b`'s, `ev` any other publish; a run of one name is one, and the names are
/// joined by `+`.
#[cfg(unix)]
fn shape_step(frames: &[(&Then, Option<&str>)]) -> String {
    let mut names: Vec<&'static str> = Vec::new();
    for (then, subject) in frames {
        let name = match (then, subject) {
            (Then::Commit { .. }, _) => "commit",
            (Then::Post(_), _) => "note",
            (Then::Mirror { .. }, Some(s)) if s.ends_with("/s-a/ev") => "evA",
            (Then::Mirror { .. }, Some(s)) if s.ends_with("/s-b/ev") => "evB",
            _ => "ev",
        };
        if names.last() != Some(&name) {
            names.push(name);
        }
    }
    names.join("+")
}

/// The writes `bridge`'s link makes from now on, each one shape step ([`shape_step`]).
#[cfg(unix)]
fn tap_shapes(bridge: &mut Bridge) -> Arc<std::sync::Mutex<Vec<String>>> {
    let writes = Arc::new(std::sync::Mutex::new(Vec::new()));
    let tapped = Arc::clone(&writes);
    bridge
        .conn
        .as_mut()
        .expect("the link stands")
        .tap_writes(move |frames| tapped.lock().unwrap().push(shape_step(frames)));
    writes
}

/// A session's log at the lane holding its open (a snapshot owed with it).
#[cfg(unix)]
fn an_opened_session_log() -> aterm_slog::Log {
    let mut log = aterm_slog::Log::with_limits(
        1,
        aterm_slog::Limits::new(8, 1 << 20).expect("a small ring"),
    );
    log.open_session(
        aterm_slog::Kind::Spawn,
        aterm_slog::Src::Aterm,
        aterm_slog::Conf::Exact,
        aterm_slog::Actor::Aterm,
        aterm_slog::Fields::new(),
        aterm_slog::Stamp::new(1, 1),
    )
    .expect("the session opens");
    log
}

/// What [`a_paced_note_on_one_node`] saw.
#[cfg(unix)]
struct PacedNote {
    /// Every write from the post on, each one shape step ([`shape_step`]).
    writes: Vec<String>,
    /// The writer batches over the same span.
    batches: u64,
    /// Whether the group ended through the note's delivery.
    committed: bool,
    /// When the lane was asked the first take after the delivery, if that round's pass took,
    /// on the bridge's held clock, which the take's round trip then moved on
    /// ([`TAKE_ROUND_TRIP`]).
    took: Option<Instant>,
    /// The inbox commit's hold as that round's settle left it, before its park
    /// ([`Bridge::mirror_pass_then_settle`]).
    held: Option<Instant>,
    /// By write ([`PacedNote::writes`]): how many takes the lane had answered since the post
    /// when it went out.
    takes_at: Vec<usize>,
    /// How many takes the lane answered from the post until the loop's rounds ended.
    takes: usize,
}

/// How the lane answers a paced note's takes ([`a_paced_note_on_one_node`]).
#[cfg(unix)]
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Snapshots {
    /// Each take holds the snapshots its sessions' logs owe.
    Taken,
    /// Each take comes back empty, `due=` a moved mail count's snapshot the endpoint holds
    /// back to its one a second (`aterm_slog::MAIL_SNAP_GAP_MS`), as for a note sent inside
    /// a second of the last.
    HeldBack,
    /// Each take comes back empty as with [`Snapshots::HeldBack`], and a take before the
    /// note already did, so the note's falls inside the endpoint's rate hold
    /// ([`Bridge::mirror_behind_the_commit`]). The hold's end is put past the one-minute
    /// hang detector, so it never ends inside the test. The loop's rounds run until the
    /// mirror owes nothing. That answer names both sessions as held (`held=`), so the note's
    /// wakes are ones the hold covers.
    InsideTheRateHold,
    /// A take before the note came back empty with a third session's snapshot held back to
    /// the endpoint's rate past the test's span (`held=s-c`), and every take after it holds
    /// the snapshots the two sessions' logs owe, as with [`Snapshots::Taken`]: the hold is
    /// per session, so the note's wakes are ones it does not cover (critic item E04c's
    /// review).
    OtherSessionHeld,
}

/// A PACED NOTE BETWEEN TWO SESSIONS OF ONE NODE, as the fabric bench's paced phase sends one
/// (critic item E04), in an open that holds the pin's fence: `s-a` posts to `s-b`. The post
/// moved A's mail count, so A's log at the lane owes its snapshot, and the endpoint's line
/// for the post wakes the mirror as it lists the post. The note's ack is read before the
/// next pass, so the note's write is a writer batch before anything the pass sends. The
/// note's record comes back on the inbox face while that round parks, and its delivery
/// moves B's mail count (B's log joins the lane's as the delivery is answered). The round
/// after the delivery runs its pass and settle, and what they left is read before its park.
/// Then the loop's rounds run on `fx` until the group is through the note and, with
/// [`Snapshots::Taken`] or [`Snapshots::OtherSessionHeld`], both logs' takes have landed,
/// or, with [`Snapshots::InsideTheRateHold`], the mirror owes nothing, under a one-minute
/// hang detector. A tap on the link counts the lane's takes since the post as each write
/// goes out.
///
/// The bridge runs on a clock the rig holds ([`crate::fx::Fx::on_a_held_clock`]), shared
/// with the lane, whose every take costs it [`TAKE_ROUND_TRIP`]: the clock moves only by the
/// loop's parks, the lane's takes and the rig's own moves, so no grace lapses because a
/// loaded machine ran the rig's steps slowly.
#[cfg(unix)]
fn a_paced_note_on_one_node(fx: crate::fx::Fx, snapshots: Snapshots) -> PacedNote {
    let (lane, mut bridge) = FakeVerbLane::bridge(&[(1, "s-a"), (2, "s-b")]);
    bridge.fx = fx.on_a_held_clock();
    lane.mirror.lock().unwrap().clock = bridge.fx.held_clock();
    let (_records, mut reader, _socket, broker, _server) = burst_rig(&lane, &mut bridge, "s-b", 0);
    bridge.mirror = Some(mirror_run::MirrorRun::acked_for_tests(
        &bridge.state,
        aterm_slog::TAKE_MAX,
    ));
    bridge.mirror_step();
    assert!(
        bridge.state.logpin_fence_held(),
        "PRECONDITION: the open's first pass made the pin's fence"
    );
    assert_eq!(
        bridge.mirror_due(bridge.fx.now()),
        None,
        "PRECONDITION: and took nothing, so the mirror owes nothing"
    );
    match snapshots {
        Snapshots::Taken => {}
        Snapshots::HeldBack => {
            lane.mirror.lock().unwrap().refuse_take = Some("OK 0 slots=0 due=400\n".to_string());
        }
        Snapshots::InsideTheRateHold => {
            lane.mirror.lock().unwrap().refuse_take =
                Some("OK 0 slots=0 due=120000 held=s-a,s-b\n".to_string());
        }
        Snapshots::OtherSessionHeld => {
            lane.mirror.lock().unwrap().refuse_take =
                Some("OK 0 slots=0 due=120000 held=s-c\n".to_string());
        }
    }
    lane.mirror
        .lock()
        .unwrap()
        .logs
        .insert("s-a".to_string(), an_opened_session_log());
    if matches!(
        snapshots,
        Snapshots::InsideTheRateHold | Snapshots::OtherSessionHeld
    ) {
        let takes = lane.mirror.lock().unwrap().takes;
        bridge.mirror_wake();
        pass_past_its_hold(&mut bridge);
        let now = bridge.fx.now();
        let (took, due) = (
            lane.mirror.lock().unwrap().takes - takes,
            bridge.mirror_due(now),
        );
        assert!(
            took == 1 && due.is_some_and(|at| at > now + aterm_spec::wait::MUST_HAPPEN),
            "PRECONDITION: a take before the note came back empty, its snapshot held back to \
             the endpoint's rate past the test's span: {took} takes, due {due:?} at {now:?}"
        );
        if snapshots == Snapshots::OtherSessionHeld {
            lane.mirror.lock().unwrap().refuse_take = None;
        }
    }
    let at_the_lane = Arc::clone(&lane.mirror);
    let b_log = an_opened_session_log();
    lane.on_first_deliver(move || {
        at_the_lane
            .lock()
            .unwrap()
            .logs
            .insert("s-b".to_string(), b_log);
    });
    let node = bridge.node.clone();
    let lane_face = subject::post_lane("f", &node, "s-b", &node, "note");
    bridge.indexed_subjects.insert(lane_face.clone());
    *lane.outbox.lock().unwrap() =
        Some("post sid=s-a id=1 to=@s-b kind=note len=4\nnote".to_string());
    bridge.mailbox.push_event("EVENT 1 post 1".to_string());
    let batches = broker.stats().writer_batches;
    let takes_before = lane.mirror.lock().unwrap().takes;
    let takes_since = |lane: &FakeVerbLane| lane.mirror.lock().unwrap().takes - takes_before;
    let writes = Arc::new(std::sync::Mutex::new(Vec::<(String, usize)>::new()));
    let (tapped, at_the_lane) = (Arc::clone(&writes), Arc::clone(&lane.mirror));
    bridge
        .conn
        .as_mut()
        .expect("the link stands")
        .tap_writes(move |frames| {
            let takes = at_the_lane.lock().unwrap().takes - takes_before;
            tapped.lock().unwrap().push((shape_step(frames), takes));
        });

    assert!(bridge.mirror_then_round());
    bridge.settle_acks();
    let off = lane
        .sent
        .lock()
        .unwrap()
        .iter()
        .find_map(|l| l.strip_prefix("outbox sent s-a 1 off="))
        .and_then(|rest| rest.split_whitespace().next()?.parse::<u64>().ok())
        .expect("PRECONDITION: the note landed");
    let (rows, _) = reader
        .fetch(off, &lane_face, 1)
        .expect("the note reads back");
    let record = rows.into_iter().next().expect("the note's record");

    if !bridge.mirror_yields() {
        bridge.mirror_step();
    }
    bridge.settle_owed();
    let generation = bridge.mailbox.broker_generation(Source::Inbox);
    assert_eq!(
        bridge
            .mailbox
            .push(Source::Inbox, record, None, generation, bridge.fx.now()),
        Pushed::Queued
    );
    let wait = bridge.park_wait(bridge.fx.now());
    assert!(bridge.park_and_take(wait));
    assert_eq!(
        bridge.inbox_pending.max(bridge.committed),
        Some(off),
        "PRECONDITION: the note was delivered"
    );

    // The round after the delivery (`Bridge::mirror_then_round`), read before its park.
    let takes = lane.mirror.lock().unwrap().takes;
    bridge.mirror_pass_then_settle();
    let took = lane.mirror.lock().unwrap().taken_at.get(takes).copied();
    let held = bridge.commit_held;
    let wait = bridge.park_wait(bridge.fx.now());
    assert!(bridge.park_and_take(wait));

    let landed = |sid: &str| {
        lane.mirror
            .lock()
            .unwrap()
            .landings
            .iter()
            .any(|l| l.split_whitespace().step_by(2).any(|word| word == sid))
    };
    let done = |bridge: &Bridge| match snapshots {
        Snapshots::Taken | Snapshots::OtherSessionHeld => landed("s-a") && landed("s-b"),
        Snapshots::HeldBack => true,
        Snapshots::InsideTheRateHold => bridge.mirror.as_deref().is_some_and(|m| !m.owed()),
    };
    // The loop's rounds (`Bridge::mirror_then_round`), judged again before each park, which
    // with nothing owed waits for the roster's backstop.
    let started = Instant::now();
    while !(bridge.committed == Some(off) && done(&bridge)) {
        assert!(
            started.elapsed() < aterm_spec::wait::MUST_HAPPEN,
            "HANG: the commit and the takes never landed: {:?}",
            writes.lock().unwrap()
        );
        assert!(bridge.mirror_engaged(), "the mirror runs");
        bridge.mirror_pass_then_settle();
        if bridge.committed == Some(off) && done(&bridge) {
            break;
        }
        let wait = bridge.park_wait(bridge.fx.now());
        assert!(bridge.park_and_take(wait));
    }
    let (writes, takes_at) = writes.lock().unwrap().iter().cloned().unzip();
    PacedNote {
        writes,
        batches: broker.stats().writer_batches - batches,
        committed: bridge.inbox_pending.is_none(),
        took,
        held,
        takes_at,
        takes: takes_since(&lane),
    }
}

/// **THE WIND-DOWN LANDS A HELD INBOX COMMIT** (critic item B08): a commit held for the
/// next write when aterm closes the bridge is the owed commit the wind-down lands, so
/// the group is through the delivery and the next bridge redelivers nothing. The delivery
/// is a fresh log's first record, at offset 0, so the broker's group is read beside the
/// bridge's own cursor: each says whether a commit through 0 landed.
///
/// NEGATIVE CONTROL: with the held commit dropped at the wind-down
/// ([`crate::sim::world::Mutant::DropHeldCommitAtWindDown`]) the group stays where it
/// was and nothing is left for the next bridge. The seeded worlds' quits always publish
/// before aterm closes, and that publish carries the held commit, so this is its control.
/// The rig holds the bridge's clock ([`crate::fx::Fx::on_a_held_clock`]), so the hold
/// cannot lapse inside the settle that makes it.
#[cfg(unix)]
#[test]
fn the_wind_down_lands_a_held_inbox_commit() {
    for (fx, lands) in [
        (crate::fx::Fx::os(), true),
        (
            crate::fx::Fx::os_without(crate::sim::world::Mutant::DropHeldCommitAtWindDown),
            false,
        ),
    ] {
        let (lane, mut bridge) = FakeVerbLane::bridge(&[(1, "s-b")]);
        bridge.fx = fx.on_a_held_clock();
        let (records, _reader, _socket, broker, _server) = burst_rig(&lane, &mut bridge, "s-b", 1);
        let group = subject::inbox_group(&bridge.cfg.fleet, &bridge.node);
        assert_eq!(
            broker.group_start(&group),
            0,
            "PRECONDITION: never committed"
        );
        bridge.in_round = true;
        bridge.on_inbox_record(&records[0], None);
        bridge.in_round = false;
        bridge.settle_owed();
        assert!(bridge.commit_held.is_some(), "PRECONDITION: held");

        bridge.wind_down();
        let ended = (
            bridge.committed,
            bridge.inbox_pending,
            bridge.state.inbox_owed(&bridge.node),
            broker.group_start(&group),
        );
        if lands {
            assert_eq!(
                ended,
                (Some(records[0].0), None, None, records[0].0 + 1),
                "landed at the wind-down"
            );
        } else {
            assert_eq!(
                ended,
                (None, None, None, 0),
                "NEGATIVE CONTROL: dropped, neither landed nor left"
            );
        }
    }
}

/// **A HELD INBOX COMMIT GOES ALONE ONCE ITS GRACE IS OUT** (critic item B08): at the
/// park's backstop when nothing came, and at the next settle when what came published
/// nothing: an endpoint line that publishes nothing keeps the hold while it is handled,
/// sending nothing, and the settle after it sends the commit once the grace is out.
/// Neither waits on a clock here: the bridge's clock is held
/// ([`crate::fx::Fx::on_a_held_clock`]), so the hold lapses only when the test moves it a
/// grace on. The first delivery is a fresh log's first record, at offset 0, so each landing
/// is read off the broker's group beside the bridge's own cursor: each says whether a
/// commit through 0 landed.
#[cfg(unix)]
#[test]
fn a_held_inbox_commit_goes_alone_once_its_grace_is_out() {
    let (lane, mut bridge) = FakeVerbLane::bridge(&[(1, "s-b")]);
    bridge.fx = crate::fx::Fx::os().on_a_held_clock();
    let (records, _reader, _socket, broker, _server) = burst_rig(&lane, &mut bridge, "s-b", 2);
    let group = subject::inbox_group(&bridge.cfg.fleet, &bridge.node);

    bridge.in_round = true;
    bridge.on_inbox_record(&records[0], None);
    bridge.in_round = false;
    bridge.settle_owed();
    assert!(bridge.commit_held.is_some(), "PRECONDITION: held");
    bridge.fx.advance(IDLE_ACK_GRACE);
    assert!(bridge.park_and_take(aterm_spec::wait::MUST_HAPPEN));
    assert_eq!(bridge.commit_held, None);
    assert_eq!(
        (
            bridge.committed,
            bridge.inbox_pending,
            broker.group_start(&group)
        ),
        (Some(records[0].0), None, records[0].0 + 1),
        "the backstop sent it and read its ack"
    );

    bridge.in_round = true;
    bridge.on_inbox_record(&records[1], None);
    bridge.in_round = false;
    bridge.settle_owed();
    assert!(bridge.commit_held.is_some(), "PRECONDITION: held again");
    *lane.outbox.lock().unwrap() = Some(String::new());
    bridge.mailbox.push_event("EVENT 1 post 1".to_string());
    assert!(bridge.park_and_take(aterm_spec::wait::MUST_HAPPEN));
    assert!(bridge.mailbox.is_idle(), "PRECONDITION: the line was taken");
    assert!(
        bridge.commit_held.is_some(),
        "a line that published nothing keeps the hold"
    );
    assert_eq!(
        (
            bridge.conn.as_ref().map(Link::unacked),
            bridge.inbox_pending
        ),
        (Some(0), Some(records[1].0)),
        "and sent nothing"
    );
    bridge.settle_owed();
    assert!(
        bridge.commit_held.is_some(),
        "a settle inside the grace keeps the hold"
    );

    bridge.fx.advance(IDLE_ACK_GRACE);
    bridge.settle_owed();
    assert_eq!(bridge.commit_held, None);
    // The settle may already have read the commit's ack off the socket, so it is judged by
    // where the cursor lands, not by what is still owed; reading acks sends nothing.
    bridge.settle_acks();
    assert_eq!(
        (
            bridge.committed,
            bridge.inbox_pending,
            broker.group_start(&group)
        ),
        (Some(records[1].0), None, records[1].0 + 1),
        "the settle sent it alone"
    );
    assert_eq!(
        bridge.conn.as_ref().map(Link::unacked),
        Some(0),
        "nothing is left owed"
    );
}

/// **RECORD 0 COMMITS BELOW A RECORD OWED JUST PAST IT**: on a fresh log the first record
/// sits at offset 0. With it delivered and the next one still owed to a session (a lane
/// fault leaves one), the inbox commit stops just below the owed record, at record 0, as
/// it stops below any other ([`Bridge::commit_target`]), and the link lands it once: a
/// bridge that committed nothing yet has not committed through offset 0, and one that
/// has does not commit it again.
#[cfg(unix)]
#[test]
fn record_zero_commits_below_a_record_owed_just_past_it() {
    let (lane, mut bridge) = FakeVerbLane::bridge(&[(1, "s-b")]);
    let (records, _reader, _socket, broker, _server) =
        owe_the_record_past_record_zero(&lane, &mut bridge);
    let group = subject::inbox_group(&bridge.cfg.fleet, &bridge.node);
    bridge.flush_inbox_commit();
    let commits: Vec<u64> = bridge
        .conn
        .as_ref()
        .expect("the link stands")
        .owed()
        .filter_map(|then| match then {
            Then::Commit { upto, .. } => Some(*upto),
            _ => None,
        })
        .collect();
    assert_eq!(
        commits,
        [records[0].0],
        "through record 0, short of the owed one"
    );
    bridge.settle_acks();
    assert_eq!(
        (bridge.committed, broker.group_start(&group)),
        (Some(records[0].0), records[1].0),
        "the group resumes at the owed record"
    );

    bridge.flush_inbox_commit();
    bridge.settle_owed();
    assert!(!bridge.holds_inbox_commit(), "nothing more is held");
    assert_eq!(
        bridge.conn.as_ref().map(Link::unacked),
        Some(0),
        "nor sent again"
    );
}

/// **RECORD 0 IS LEFT FOR THE NEXT BRIDGE BELOW A RECORD OWED JUST PAST IT**: the case of
/// [`record_zero_commits_below_a_record_owed_just_past_it`] with no link to commit on,
/// so the wind-down hands record 0's commit to the next bridge.
#[cfg(unix)]
#[test]
fn record_zero_is_left_for_the_next_bridge_below_a_record_owed_just_past_it() {
    let (lane, mut bridge) = FakeVerbLane::bridge(&[(1, "s-b")]);
    let (records, _writer, _socket, _broker, _server) =
        owe_the_record_past_record_zero(&lane, &mut bridge);
    bridge.conn = None;
    bridge.wind_down();
    assert_eq!(
        bridge.state.inbox_owed(&bridge.node),
        Some(records[0].0),
        "left for the next bridge"
    );
}

/// A fresh log's first two records delivered to `s-b`, the second still owed to it.
#[cfg(unix)]
fn owe_the_record_past_record_zero(
    lane: &FakeVerbLane,
    bridge: &mut Bridge,
) -> (
    Vec<BrokerRecord>,
    Conn,
    String,
    astream_broker::Broker,
    astream_broker::BrokerHandle,
) {
    let rig = burst_rig(lane, bridge, "s-b", 2);
    let records = &rig.0;
    assert_eq!(records[0].0, 0, "PRECONDITION: a fresh log's first record");
    bridge.in_round = true;
    for rec in records {
        bridge.on_inbox_record(rec, None);
    }
    bridge.in_round = false;
    bridge
        .per_sid
        .lane_owed
        .insert("s-b".to_string(), Some(records[1].0));
    assert_eq!(bridge.inbox_pending, Some(records[1].0), "PRECONDITION");
    rig
}

/// **A COMMIT THAT A RECORD OWED TO A SESSION STOPS IS NOT HELD** (critic item B08
/// review): with the first record still owed to a session just past the committed
/// cursor (a lane fault leaves one), no inbox commit can go out however long the bridge
/// waits, so the idle round holds none and the loop parks its full wait, as it did
/// before B08, round after round, instead of waking every grace to send nothing.
#[cfg(unix)]
#[test]
fn a_commit_that_a_record_owed_to_a_session_stops_is_not_held() {
    let (lane, mut bridge) = FakeVerbLane::bridge(&[(1, "s-b")]);
    let (records, _reader, _socket, _broker, _server) = burst_rig(&lane, &mut bridge, "s-b", 3);
    bridge.in_round = true;
    bridge.on_inbox_record(&records[0], None);
    bridge.in_round = false;
    bridge.flush_inbox_commit();
    bridge.settle_acks();
    assert_eq!(
        bridge.committed,
        Some(records[0].0),
        "PRECONDITION: committed"
    );

    bridge.in_round = true;
    for rec in &records[1..] {
        bridge.on_inbox_record(rec, None);
    }
    bridge.in_round = false;
    let owed_off = records[0].0 + 1;
    bridge
        .per_sid
        .lane_owed
        .insert("s-b".to_string(), Some(owed_off));
    assert_eq!(bridge.inbox_pending, Some(records[2].0), "PRECONDITION");
    assert_eq!(
        bridge.commit_target(records[2].0),
        None,
        "PRECONDITION: nothing can be committed"
    );

    let wait = Duration::from_millis(20);
    for _ in 0..2 {
        bridge.settle_owed();
        assert!(
            !bridge.holds_inbox_commit(),
            "nothing it could send is held"
        );
        assert!(bridge.park_and_take(wait));
        assert_eq!(bridge.parked, Some(wait), "the park takes its full wait");
    }
    assert_eq!(bridge.committed, Some(records[0].0));
    assert_eq!(bridge.conn.as_ref().map(Link::unacked), Some(0));
}

/// **AN ITEM OFF THE BUS SENDS A HELD COMMIT BEFORE IT IS HANDLED** (critic item B08
/// review): only aterm's push lane brings the reply a held commit waits for, so the next
/// inbox record, whose delivery waits on aterm's answer for as long as aterm takes,
/// finds the commit for the delivery before it already sent, through that delivery and
/// no further: a slow answer does not keep it off the wire. The rig holds the bridge's
/// clock ([`crate::fx::Fx::on_a_held_clock`]), so the hold cannot lapse inside the settle
/// that makes it.
#[cfg(unix)]
#[test]
fn an_item_off_the_bus_sends_a_held_commit_before_it_is_handled() {
    let (lane, mut bridge) = FakeVerbLane::bridge(&[(1, "s-b")]);
    bridge.fx = crate::fx::Fx::os().on_a_held_clock();
    let (records, _reader, _socket, _broker, _server) = burst_rig(&lane, &mut bridge, "s-b", 2);
    bridge.in_round = true;
    bridge.on_inbox_record(&records[0], None);
    bridge.in_round = false;
    bridge.settle_owed();
    assert!(bridge.holds_inbox_commit(), "PRECONDITION: held");

    queue_inbox(&bridge, &records[1..]);
    assert!(bridge.park_and_take(aterm_spec::wait::MUST_HAPPEN));
    assert_eq!(
        lane.delivery_offsets(),
        [records[0].0, records[1].0],
        "PRECONDITION: the second record was taken and delivered"
    );
    let commits: Vec<u64> = bridge
        .conn
        .as_ref()
        .expect("the link stands")
        .owed()
        .filter_map(|then| match then {
            Then::Commit { upto, .. } => Some(*upto),
            _ => None,
        })
        .collect();
    assert_eq!(
        commits,
        [records[0].0],
        "the held commit went out before the next record was handled"
    );
    assert!(!bridge.holds_inbox_commit());
    bridge.settle_acks();
    assert_eq!(bridge.committed, Some(records[0].0));
    assert_eq!(
        bridge.inbox_pending,
        Some(records[1].0),
        "the next one is owed"
    );
}

#[cfg(unix)]
#[test]
fn a_commit_refused_by_a_rolled_back_batch_keeps_the_link_and_its_retry_lands_it() {
    let (lane, mut bridge) = FakeVerbLane::bridge(&[(1, "s-a")]);
    let socket = lane.dir.path().join("commit-refused.sock");
    let broker = astream_broker::Broker::open(lane.dir.path().join("commit-refused.log")).unwrap();
    let _server = broker.serve(&socket).unwrap();
    bridge.cfg.broker = socket.to_string_lossy().into_owned();
    bridge.conn = Some(bridge.connect().unwrap().0.into());
    let face = format!("/f/f/in/{}/s-a/n-peer/note", bridge.node);
    let (mut writer, _) = bridge.connect().unwrap();
    let (r0, _) = writer.publish(91, 1, &face, b"v=1 t=1 text=a").unwrap();
    let (r1, _) = writer.publish(91, 2, &face, b"v=1 t=1 text=b").unwrap();
    let fsync_fails = |on: bool| {
        broker.inject_faults(astream_broker::InjectedFaults {
            write: false,
            sync: on,
            truncate: false,
        });
    };

    bridge.inbox_pending = Some(r0);
    fsync_fails(true);
    bridge.in_round = true;
    bridge.flush_inbox_commit();
    bridge.in_round = false;
    assert!(
        bridge
            .conn
            .as_ref()
            .is_some_and(|link| link.owed().any(Then::is_commit)),
        "PRECONDITION: the commit rides the pipeline"
    );
    bridge.settle_acks();
    assert!(
        bridge.conn.is_some(),
        "a commit refused by a rolled-back batch is an answer, not a lost link"
    );
    assert_eq!(bridge.committed, None, "the group has not moved");
    assert_eq!(bridge.inbox_pending, Some(r0), "the debt is kept");

    fsync_fails(false);
    bridge.land_inbox_commit();
    assert!(bridge.conn.is_some(), "the link stood throughout");
    assert_eq!(bridge.committed, Some(r0), "the retry landed the group");
    assert_eq!(bridge.inbox_pending, None);

    bridge.inbox_pending = Some(r1);
    fsync_fails(true);
    bridge.in_round = true;
    bridge.flush_inbox_commit();
    bridge.in_round = false;
    bridge.settle_acks();
    assert!(bridge.conn.is_some(), "the first refusal is an answer");
    bridge.land_inbox_commit();
    fsync_fails(false);
    assert!(
        bridge.conn.is_none(),
        "a refused retry is the link down, as the blocking commit's was"
    );
    assert_eq!(bridge.committed, Some(r0), "the group stayed where it was");
    assert_eq!(
        bridge.inbox_pending,
        Some(r1),
        "the debt kept for the next connection"
    );
}

#[cfg(unix)]
#[test]
fn the_wait_tally_counts_the_commit_and_the_ack_the_loop_waited_on_and_an_off_one_counts_nothing() {
    use crate::wait_tally::{Row, Tally, FILE};
    const SID: &str = "s-tally";
    for on in [true, false] {
        let (lane, mut bridge) = FakeVerbLane::bridge(&[(1, SID)]);
        let (records, _reader, _socket, _broker, _server) = burst_rig(&lane, &mut bridge, SID, 4);
        let path = bridge.state.root().join(FILE);
        assert!(!bridge.waits.on(), "off unless the seam is set");
        if on {
            bridge.tally_to(&path);
        }
        let read = || Tally::parse(&std::fs::read_to_string(&path).unwrap()).unwrap();

        queue_inbox(&bridge, &records);
        run_rounds(&mut bridge);
        assert_eq!(bridge.committed, Some(records[3].0), "on={on}");
        let counted = bridge.waits.tally();
        if !on {
            assert_eq!(counted, Tally::default(), "an off tally counts nothing");
            assert!(!path.exists(), "and writes nothing");
            continue;
        }
        let counts = |t: &Tally, r: Row| t.row(r).count;
        assert_eq!(
            (
                counts(&counted, Row::Request),
                counts(&counted, Row::Commit),
                counts(&counted, Row::Ack)
            ),
            (0, 1, 1),
            "the burst's one commit is the one wait, a pipelined ack"
        );
        assert!(counted.row(Row::Commit).total <= counted.row(Row::Ack).total);
        let written = read();
        assert_eq!(
            written.committed, records[3].0,
            "written at the idle moment, past the commit"
        );
        for row in [Row::Request, Row::Commit, Row::Ack, Row::LaneDeliver] {
            assert_eq!(written.row(row), counted.row(row), "{row:?}");
        }
        assert_eq!(counts(&written, Row::LaneDeliver), 4);
        assert_eq!(
            (
                counts(&written, Row::QueuedRecord),
                counts(&written, Row::WakeRecord)
            ),
            (4, 0)
        );
        assert!(counts(&written, Row::Park) >= 4, "a take per record");

        bridge.in_round = true;
        bridge.publish_ev("tallied");
        bridge.in_round = false;
        run_rounds(&mut bridge);
        let waited = read().since(&written).unwrap();
        assert_eq!(
            (
                waited.row(Row::Request).count,
                waited.row(Row::Commit).count,
                waited.row(Row::Ack).count
            ),
            (0, 0, 1),
            "a pipelined ack, read at the idle moment"
        );
        assert!(waited.at_ns > 0, "the bridge's own clock moved");
    }
}

#[cfg(unix)]
#[test]
fn a_crash_before_the_bursts_commit_redelivers_the_burst_and_the_endpoint_keeps_one_of_each() {
    const SID: &str = "s-crash";
    const N: usize = 8;
    let (lane, mut bridge) = FakeVerbLane::bridge(&[(1, SID)]);
    let (burst, mut reader, socket, _broker, _server) =
        burst_rig(&lane, &mut bridge, SID, N as u64);
    let offs: Vec<u64> = burst.iter().map(|r| r.0).collect();
    let group = subject::inbox_group("f", &bridge.node);
    let filter = subject::inbox_filter("f", &bridge.node);
    let group_offers = |what: &str, n: usize| -> Vec<u64> {
        let (client, _) = transport::connect(&socket, aterm_spec::wait::MUST_HAPPEN).unwrap();
        let mut sub = client.subscribe_group(&group, &filter).unwrap();
        (0..n).map(|_| next_delivery(&mut sub, what).0).collect()
    };

    queue_inbox(&bridge, &burst);
    for _ in 0..N {
        assert!(bridge.round(Duration::ZERO));
    }
    assert_eq!(lane.delivery_offsets(), offs, "the endpoint took the burst");
    assert_eq!(
        bridge.inbox_pending,
        Some(offs[N - 1]),
        "its commit is owed"
    );
    let before = log_head(&mut reader);
    assert_eq!(
        group_offers("the durable cursor after the crash", N),
        offs,
        "nothing was committed: the next bridge is offered the whole burst"
    );

    bridge.inbox_pending = None;
    queue_inbox(&bridge, &burst);
    run_rounds(&mut bridge);
    assert_eq!(
        lane.delivery_offsets(),
        offs,
        "redelivered, answered from the endpoint's memory, appended nowhere"
    );
    assert_eq!(bridge.committed, Some(offs[N - 1]));
    assert_eq!(log_head(&mut reader) - before, 1, "and committed once");
    let (mut writer, _) = bridge.connect().unwrap();
    let (sentinel, _) = writer
        .publish(92, 1, &burst[0].1, b"v=1 t=2 from=s-peer text=after")
        .unwrap();
    assert_eq!(
        group_offers("the cursor after the commit", 1),
        [sentinel],
        "the burst is behind the cursor now"
    );
}

#[cfg(unix)]
#[test]
fn a_reconnect_does_not_commit_a_local_refusal_whose_verdict_never_landed() {
    const HOSTED: &str = "s-hosted";
    const ABSENT: &str = "s-not-hosted";
    let (lane, bridge) = FakeVerbLane::bridge(&[(1, HOSTED)]);
    let (socket, _broker, _server) = serve_log(&lane, "unproved");
    struct CloseBridge(Bridge);
    impl Drop for CloseBridge {
        fn drop(&mut self) {
            self.0.close_subscriptions();
            self.0.conn = None;
        }
    }
    let mut owned = CloseBridge(bridge);
    let bridge = &mut owned.0;
    bridge.cfg.broker = socket.clone();
    bridge.conn = Some(bridge.connect().unwrap().0.into());
    assert!(bridge.refresh_sessions().is_ok());
    bridge.per_sid.pending_admit.clear();
    let (mut writer, _) = bridge.connect().unwrap();
    let same_log = writer.log_identity().expect("the private log identity");
    let node = bridge.node.clone();
    let face = |sid: &str| format!("/f/f/in/{node}/{sid}/h-sender/note");
    let body = b"v=1 t=1 text=owned-regression";
    let mut record = |seq: u64, sid: &str| {
        let subject = face(sid);
        let (off, _) = writer.publish(812, seq, &subject, body).unwrap();
        (off, subject, body.to_vec())
    };
    let accepted = record(1, HOSTED);
    bridge.on_inbox_record(&accepted, None);
    assert_eq!(lane.delivery_offsets(), [accepted.0], "endpoint control");
    assert_eq!(bridge.committed, Some(accepted.0));
    let refused = record(2, ABSENT);
    bridge.on_inbox_record(&refused, None);
    assert_eq!(bridge.committed, Some(refused.0), "durable refusal control");

    let (mut reader, _) = bridge.connect().unwrap();
    let ev_face = bridge.ev_face(None);
    let verdict_landed = |reader: &mut Conn, off: u64| {
        let (rows, _) = reader.fetch(0, &ev_face, 64).unwrap();
        rows.iter().any(|(_, _, raw)| {
            Body::decode(raw).0.unknown.get("ev").is_some_and(|v| {
                crate::pct::decode(v).starts_with(&format!("undeliverable off={off} "))
            })
        })
    };
    assert!(verdict_landed(&mut reader, refused.0));
    let unproved = record(3, ABSENT);
    let sentinel = record(4, ABSENT);
    let (publisher, closer) = bridge.connect().unwrap();
    bridge.conn = Some(publisher.into());
    closer.close();
    queue_inbox(bridge, std::slice::from_ref(&unproved));
    assert!(bridge.round(Duration::ZERO));
    assert!(
        bridge.conn.is_none(),
        "the pre-write refusal lost the publisher"
    );
    assert_eq!(
        bridge.committed,
        Some(refused.0),
        "no failed write committed"
    );
    assert!(
        !verdict_landed(&mut reader, unproved.0),
        "no verdict landed"
    );
    assert_eq!(lane.delivery_offsets(), [accepted.0], "not an endpoint ack");

    assert!(
        bridge.attach_broker(),
        "actual re-attach to the private broker"
    );
    assert_eq!(reader.log_identity().unwrap(), same_log, "the same log");
    assert!(
        !verdict_landed(&mut reader, unproved.0),
        "no refill re-proved it"
    );
    assert_eq!(
        lane.delivery_offsets(),
        [accepted.0],
        "still no endpoint ack"
    );
    let (client, _) = transport::connect(&socket, aterm_spec::wait::MUST_HAPPEN).unwrap();
    let group = subject::inbox_group("f", &bridge.node);
    let filter = subject::inbox_filter("f", &bridge.node);
    let mut sub = client.subscribe_group(&group, &filter).unwrap();
    let offered = next_delivery(&mut sub, "the first record still owed after reconnect").0;
    assert_eq!(
        offered, unproved.0,
        "an unproven local refusal stays offered, rather than advancing to sentinel {}",
        sentinel.0
    );
}

#[cfg(unix)]
#[test]
fn no_burst_commit_passes_an_owed_record() {
    const HELD: &str = "s-held";
    const OK: &str = "s-ok";
    let (lane, mut bridge) = FakeVerbLane::bridge(&[(1, HELD), (2, OK)]);
    let (records, _reader, _socket, _broker, _server) = burst_rig(&lane, &mut bridge, OK, 6);
    let node = bridge.node.clone();
    let mail = |off: u64, sid: &str, body: &[u8]| -> BrokerRecord {
        (
            off,
            format!("/f/f/in/{node}/{sid}/n-peer/note"),
            body.to_vec(),
        )
    };
    let plain = b"v=1 t=1 from=s-peer text=hi";
    lane.answer_deliveries_to(HELD, SHIPPED_DELIVER_USAGE);
    queue_inbox(
        &bridge,
        &[
            mail(records[0].0, OK, plain),
            mail(records[1].0, HELD, plain),
            mail(records[2].0, OK, plain),
        ],
    );
    run_rounds(&mut bridge);
    assert_eq!(bridge.inbox_commit_floor(), Some(records[1].0));
    assert_eq!(
        bridge.committed,
        Some(records[0].0),
        "the burst's commit stops below the owed record"
    );

    bridge.in_round = true;
    for rec in &records[3..] {
        bridge.on_inbox_record(&mail(rec.0, OK, plain), None);
    }
    bridge.in_round = false;
    assert_eq!(bridge.inbox_pending, Some(records[5].0), "accounted for");
    bridge.wind_down();
    assert_eq!(
        bridge.committed,
        Some(records[0].0),
        "the exit's commit is still below the owed record"
    );
    assert_eq!(bridge.inbox_commit_floor(), Some(records[1].0));
    assert_eq!(
        bridge.state.inbox_owed(&bridge.node),
        None,
        "nor is anything past it handed to the next bridge"
    );
}

#[cfg(unix)]
#[test]
fn no_exit_commit_passes_an_unaccounted_record() {
    const OK: &str = "s-ok";
    let (lane, mut bridge) = FakeVerbLane::bridge(&[(1, OK)]);
    let (records, _reader, _socket, _broker, _server) = burst_rig(&lane, &mut bridge, OK, 4);
    bridge.on_inbox_record(&records[0], None);
    assert_eq!(bridge.committed, Some(records[0].0));

    bridge.in_round = true;
    bridge.on_inbox_record(&records[1], None);
    bridge.fault = Fault::LoseAtermBeforeDeliver;
    bridge.on_inbox_record(&records[2], None);
    assert!(bridge.ctl.lost(), "the lane is gone");
    let (off, subject, _) = &records[3];
    bridge.on_inbox_record(
        &(
            *off,
            subject.clone(),
            b"v=1 t=1 from=s-peer via=NOT-A-PRINCIPAL text=x".to_vec(),
        ),
        None,
    );
    bridge.in_round = false;
    assert_eq!(
        bridge.inbox_pending,
        Some(records[3].0),
        "the local refusal accounted for itself past the unaccounted record"
    );
    assert_eq!(
        bridge.inbox_commit_floor(),
        Some(records[2].0),
        "and the unaccounted record is the only floor"
    );
    bridge.wind_down();
    assert_eq!(
        bridge.committed,
        Some(records[1].0),
        "through the delivered record, and not past the unaccounted one"
    );
    assert!(
        bridge
            .state
            .inbox_owed(&bridge.node)
            .is_none_or(|owed| owed < records[2].0),
        "nothing past it is handed over"
    );
}

/// A LOCAL REFUSAL SENDS THE ROWS ITS INBOX RUN QUEUED AHEAD OF IT FIRST (critic item C08,
/// [`Bridge::on_inbox_run`]). Four records come as one run under a same-build aterm: the
/// first two queue for one write, the third is refused here (a `via` no principal could
/// write), and its notice can commit the group through it. On a live lane the two go in
/// one write ahead of the notice, the fourth in one of its own, and the group commits
/// through all four. On a lane aterm leaves at the run's first line, the two are left
/// unaccounted ahead of the notice, which then commits nothing: the group stays below them.
/// NEGATIVE CONTROL: [`Mutant::RefusalAheadOfQueuedRows`] sends the notice first, and it
/// commits the group through the refused record, past the two aterm never took.
#[cfg(unix)]
#[test]
fn a_local_refusal_sends_the_rows_its_run_queued_ahead_of_it_first() {
    use crate::sim::world::Mutant;
    const OK: &str = "s-ok";
    let run =
        |fx: crate::fx::Fx, aterm_leaves: bool| -> (Vec<u64>, Option<u64>, Vec<usize>, Vec<u64>) {
            let (lane, mut bridge) = FakeVerbLane::bridge(&[(1, OK)]);
            bridge.fx = fx;
            let (mut records, _reader, _socket, _broker, _server) =
                burst_rig(&lane, &mut bridge, OK, 4);
            records[2].2 = b"v=1 t=1 from=s-peer via=NOT-A-PRINCIPAL text=x".to_vec();
            bridge.endpoint_lane = aterm_types::control_verbs::BRIDGE_LANE;
            bridge.endpoint_build = Some(aterm_stamp::build_id());
            // No `link up` report falls due while the run is handled: it is a lane request,
            // which sends the queued rows ahead of the notice whatever the refusal does.
            bridge.link = LinkReport {
                up: true,
                reason: "",
                rtt_ms: Some(0),
                reported_at: Some(bridge.fx.now() + Duration::from_secs(3600)),
            };
            if aterm_leaves {
                lane.hang_up_at("deliver");
            }
            queue_inbox(&bridge, &records);
            bridge.round(Duration::ZERO);
            assert!(!bridge.mailbox.inbox_queued(), "the run took all four");
            bridge.wind_down();
            let offs = records.iter().map(|r| r.0).collect();
            let sent = bridge.rows_sent.clone();
            (offs, bridge.committed, sent, lane.delivery_offsets())
        };

    let (offs, committed, sent, delivered) = run(crate::fx::Fx::os(), false);
    assert_eq!(sent, [2, 1], "the two ahead of the refusal in one write");
    assert_eq!(delivered, [offs[0], offs[1], offs[3]]);
    assert_eq!(
        committed,
        Some(offs[3]),
        "the group commits through all four"
    );

    let (offs, committed, sent, delivered) = run(crate::fx::Fx::os(), true);
    assert_eq!(sent, [2, 1]);
    assert!(delivered.is_empty(), "aterm took nothing");
    assert!(
        committed.is_none_or(|c| c < offs[0]),
        "the refusal's notice waited for the rows queued ahead of it, and committed nothing \
         past them: {committed:?} of {offs:?}"
    );

    let (offs, committed, ..) = run(
        crate::fx::Fx::os_without(Mutant::RefusalAheadOfQueuedRows),
        true,
    );
    assert_eq!(
        committed,
        Some(offs[2]),
        "the control: a notice sent ahead of the queued rows commits past the two aterm \
         never took"
    );
}

#[cfg(unix)]
fn first_offered(socket: &str, node: &str, what: &str) -> u64 {
    let (client, _) = transport::connect(socket, aterm_spec::wait::MUST_HAPPEN).unwrap();
    let group = subject::inbox_group("f", node);
    let filter = subject::inbox_filter("f", node);
    let mut sub = client.subscribe_group(&group, &filter).unwrap();
    next_delivery(&mut sub, what).0
}

#[cfg(unix)]
#[test]
fn a_records_commit_waits_for_its_own_verdicts_answer_and_dies_with_its_connection() {
    const SID: &str = "s-host";
    const ABSENT: &str = "s-not-hosted";
    for dropped in [false, true] {
        let (lane, mut bridge) = FakeVerbLane::bridge(&[(1, SID)]);
        let (records, mut writer, socket, _broker, _server) = burst_rig(&lane, &mut bridge, SID, 1);
        let node = bridge.node.clone();
        let (delivered, hosted, body) = records[0].clone();
        let absent = format!("/f/f/in/{node}/{ABSENT}/n-peer/note");
        let (refused, _) = writer.publish(91, 2, &absent, &body).unwrap();
        let (after, _) = writer.publish(91, 3, &hosted, &body).unwrap();

        bridge.in_round = true;
        bridge.publish_ev("unrelated");
        bridge.on_inbox_record(&(delivered, hosted.clone(), body.clone()), None);
        assert_eq!(
            bridge.inbox_pending,
            Some(delivered),
            "an ack that is no output of it holds nothing back"
        );
        bridge.on_inbox_record(&(refused, absent.clone(), body.clone()), None);
        assert_eq!(
            bridge.inbox_pending,
            Some(delivered),
            "its verdict is still owed an answer"
        );
        bridge.on_inbox_record(&(after, hosted.clone(), body.clone()), None);
        assert_eq!(
            bridge.inbox_pending,
            Some(delivered),
            "a later record waits on the earlier one's verdict"
        );
        bridge.in_round = false;
        assert_eq!(
            lane.delivery_offsets(),
            [delivered, after],
            "the endpoint took both hosted records"
        );

        if !dropped {
            bridge.settle_acks();
            assert_eq!(bridge.inbox_pending, Some(after), "answered: all three");
            bridge.land_inbox_commit();
            assert_eq!(bridge.committed, Some(after));
            continue;
        }
        bridge.conn = None;
        assert_eq!(bridge.inbox_pending, Some(delivered), "the claim went too");
        assert!(bridge.attach_broker(), "re-attached to the same log");
        assert_eq!(
            bridge.committed,
            Some(delivered),
            "the attach commits what was proven, and no further"
        );
        let (sentinel, _) = writer.publish(91, 4, &hosted, &body).unwrap();
        assert_eq!(
            first_offered(&socket, &node, "the durable cursor after the reconnect"),
            refused,
            "the refused record is offered again, not skipped for sentinel {sentinel}"
        );
        bridge.close_subscriptions();
    }
}

#[cfg(unix)]
#[test]
fn a_notice_a_dead_publisher_never_carried_is_not_committed_past() {
    const SID: &str = "s-host";
    let (lane, mut bridge) = FakeVerbLane::bridge(&[(1, SID)]);
    let (records, mut reader, socket, _broker, _server) = burst_rig(&lane, &mut bridge, SID, 1);
    let node = bridge.node.clone();
    let face = format!("/f/f/in/{node}/{SID}/n-peer/note");
    bridge.on_inbox_record(&records[0], None);
    assert_eq!(bridge.committed, Some(records[0].0));
    let refused = b"v=1 t=1 from=s-peer via=NOT-A-PRINCIPAL text=x";
    let (mut writer, _) = bridge.connect().unwrap();
    let off = writer.publish(93, 1, &face, refused).unwrap().0;
    let (conn, closer) = bridge.connect().unwrap();
    bridge.conn = Some(conn.into());
    closer.close();
    bridge.in_round = true;
    bridge.on_inbox_record(&(off, face.clone(), refused.to_vec()), None);
    bridge.in_round = false;
    assert!(bridge.conn.is_none(), "the connection died");
    assert_eq!(bridge.inbox_pending, None, "nothing it did is proven");

    assert!(bridge.attach_broker(), "re-attached to the same log");
    assert_eq!(
        bridge.committed,
        Some(records[0].0),
        "nothing past the refusal"
    );
    let (sentinel, _) = writer
        .publish(94, 1, &face, b"v=1 t=2 from=s-peer text=s")
        .unwrap();
    assert_eq!(
        first_offered(&socket, &node, "the durable cursor"),
        off,
        "offered again, not committed past for sentinel {sentinel}"
    );
    let notices = format!("/f/f/in/n-peer/s-peer/{node}/undeliverable");
    let (rows, _) = reader.fetch(0, &notices, 64).unwrap();
    assert!(rows.is_empty(), "no notice landed: {rows:?}");
    bridge.close_subscriptions();
}

#[cfg(unix)]
#[test]
fn an_exit_with_no_connection_hands_the_bursts_commit_to_the_next_bridge() {
    const SID: &str = "s-burst";
    for handed in [true, false] {
        let (lane, mut old) = FakeVerbLane::bridge(&[(1, SID)]);
        let state = old.state.root().to_path_buf();
        let (burst, mut writer, socket, _broker, _server) = burst_rig(&lane, &mut old, SID, 4);
        let node = old.node.clone();
        old.in_round = true;
        for rec in &burst {
            old.on_inbox_record(rec, None);
        }
        old.in_round = false;
        assert_eq!(
            old.inbox_pending,
            Some(burst[3].0),
            "the burst's commit is owed"
        );
        assert_eq!(lane.delivery_offsets().len(), 4, "the endpoint took it");
        old.state.set_seen_off(SID, burst[3].0).unwrap();
        old.conn = None;
        old.wind_down();
        assert_eq!(
            old.state.inbox_owed(&old.node),
            Some(burst[3].0),
            "handed over"
        );
        drop(old);
        if !handed {
            old_bridge_forgets_the_hand_over(&state);
        }

        let (next_lane, mut next) = FakeVerbLane::bridge_with_state(&[(1, SID)], Some(&state));
        next.cfg.broker = socket.clone();
        assert!(next.attach_broker(), "the next bridge attached");
        let (sentinel, _) = writer.publish(91, 5, &burst[0].1, &burst[0].2).unwrap();
        let first = first_offered(&socket, &node, "the durable cursor after the handoff");
        if handed {
            assert_eq!(first, sentinel, "the burst is committed, not offered again");
            assert_eq!(next.committed, Some(burst[3].0));
            assert_eq!(
                next.state.inbox_owed(&next.node),
                None,
                "and the hand-over forgotten"
            );
        } else {
            assert_eq!(first, burst[0].0, "the control: the whole burst again");
        }
        next.close_subscriptions();
        drop(next_lane);
    }
}

#[cfg(unix)]
fn old_bridge_forgets_the_hand_over(state: &std::path::Path) {
    std::fs::remove_file(state.join("inbox-owed")).unwrap();
}

/// A commit a bridge of another node handed over, as the bridge of a node this Mac held
/// before its key rotated leaves one (§11 C3b), is that node's group's: the next bridge, of
/// the node this Mac holds now, makes none of it on its own group, so mail for its node that
/// landed below it is offered all the same; and the hand-over stays for the node service's
/// move of that node's mail, which makes it first.
/// NEGATIVE CONTROL: a next bridge of the same node makes it
/// (`an_exit_with_no_connection_hands_the_bursts_commit_to_the_next_bridge`).
#[cfg(unix)]
#[test]
fn a_commit_another_nodes_bridge_handed_over_is_not_made_on_this_nodes_group() {
    const SID: &str = "s-burst";
    const NOW: &str = "n-rotated";
    let (lane, mut old) = FakeVerbLane::bridge(&[(1, SID)]);
    let state = old.state.root().to_path_buf();
    let (burst, mut writer, socket, _broker, _server) = burst_rig(&lane, &mut old, SID, 1);
    let node = old.node.clone();
    // Mail for the node this Mac holds next lands among the old node's, unread.
    let face = format!("/f/f/in/{NOW}/{SID}/n-peer/note");
    let (unread, _) = writer.publish(92, 1, &face, &burst[0].2).unwrap();
    let mut taken = burst.clone();
    for seq in 2..=3 {
        let (off, _) = writer.publish(91, seq, &burst[0].1, &burst[0].2).unwrap();
        taken.push((off, burst[0].1.clone(), burst[0].2.clone()));
    }
    let upto = taken.last().unwrap().0;
    assert!(unread < upto);
    old.in_round = true;
    for rec in &taken {
        old.on_inbox_record(rec, None);
    }
    old.in_round = false;
    old.state.set_seen_off(SID, upto).unwrap();
    old.conn = None;
    old.wind_down();
    assert_eq!(old.state.inbox_owed(&node), Some(upto), "handed over");
    drop(old);

    let (next_lane, mut next) = FakeVerbLane::bridge_with_state(&[(1, SID)], Some(&state));
    next.node = NOW.to_owned();
    next.cfg.broker = socket.clone();
    assert!(next.attach_broker(), "the next bridge attached");
    let (sentinel, _) = writer.publish(92, 2, &face, &burst[0].2).unwrap();
    assert_eq!(
        first_offered(&socket, NOW, "the new node's cursor"),
        unread,
        "the new node's mail below the old node's commit is offered, not {sentinel}"
    );
    assert_eq!(next.committed, None, "nothing made on this node's group");
    assert_eq!(
        next.state.inbox_owed(&node),
        Some(upto),
        "the hand-over stays for the move of its node's mail"
    );
    next.close_subscriptions();
    drop(next_lane);
}

#[cfg(unix)]
#[test]
fn a_handed_over_inbox_commit_above_the_fork_dies_with_a_relaxed_generation() {
    use crate::durability::{self, Durability};
    const SID: &str = "s-kept";
    for reboot in [true, false] {
        let case = format!("reboot={reboot}");
        let (lane, mut old) = FakeVerbLane::bridge(&[(1, SID)]);
        lane.answer_hello_with(&format!("OK lane={LINK_LOG_LANE} build=test\n"));
        old.introduce();
        assert!(old.refresh_sessions().is_ok());
        let state = old.state.root().to_path_buf();
        let log = lane.dir.path().join("relaxed.log");
        let socket = lane.dir.path().join("relaxed.sock");
        old.cfg.broker = socket.to_string_lossy().into_owned();
        let node = old.node.clone();
        let face = format!("/f/f/in/{node}/{SID}/n-peer/note");
        let body = b"v=1 t=1 from=s-peer text=burst".to_vec();
        let (high, cut, burst) = {
            let opened = durability::open(
                &log,
                None,
                Durability::Relaxed,
                Some(durability::test_boot(1)),
            )
            .unwrap();
            opened.broker.arm_unclean_stop();
            let mut server = opened.broker.serve(&socket).unwrap();
            let (mut writer, _) = old.connect().unwrap();
            let mut burst = Vec::new();
            for seq in 1..=2 {
                let (off, _) = writer.publish(91, seq, &face, &body).unwrap();
                burst.push((off, face.clone(), body.clone()));
            }
            let cut = std::fs::metadata(&log).unwrap().len();
            for seq in 3..=4 {
                let (off, _) = writer.publish(91, seq, &face, &body).unwrap();
                burst.push((off, face.clone(), body.clone()));
            }
            assert!(old.attach_broker(), "{case}: the first attach");
            old.mailbox.reset_broker_sources();
            old.in_round = true;
            for rec in &burst {
                old.on_inbox_record(rec, None);
            }
            old.in_round = false;
            assert_eq!(old.inbox_pending, Some(burst[3].0), "{case}: owed");
            let high = old.state.log_witness().expect("witnessed").high;
            drop(writer);
            old.conn = None;
            old.wind_down();
            assert_eq!(
                old.state.inbox_owed(&old.node),
                Some(burst[3].0),
                "{case}: handed over"
            );
            old.close_subscriptions();
            drop(old);
            server.shutdown();
            (high, cut, burst)
        };
        std::fs::OpenOptions::new()
            .write(true)
            .open(&log)
            .unwrap()
            .set_len(cut)
            .unwrap();
        let boot = durability::test_boot(if reboot { 2 } else { 1 });
        let reopened = durability::open(&log, None, Durability::Relaxed, Some(boot)).unwrap();
        assert_eq!(reopened.broker.head(), 2, "{case}: a clean prefix");
        let _server = reopened.broker.serve(&socket).unwrap();
        let (mut other, _) = transport::connect(&socket.to_string_lossy(), ACK_DEADLINE).unwrap();
        let mut new_mail = Vec::new();
        let mut seq = 1;
        while reopened.broker.head() <= high {
            let (off, _) = other
                .publish(92, seq, &face, b"v=1 t=1 from=s-peer text=new")
                .unwrap();
            new_mail.push(off);
            seq += 1;
        }
        assert!(
            new_mail.contains(&burst[2].0),
            "{case}: the new mail reuses the burst's offsets"
        );

        let (next_lane, mut next) = FakeVerbLane::bridge_with_state(&[(1, SID)], Some(&state));
        next_lane.answer_hello_with(&format!("OK lane={LINK_LOG_LANE} build=test\n"));
        next.introduce();
        assert!(next.refresh_sessions().is_ok());
        next.cfg.broker = socket.to_string_lossy().into_owned();
        assert!(next.attach_broker(), "{case}: the next bridge attached");
        let first = first_offered(&socket.to_string_lossy(), &node, "after the power loss");
        if reboot {
            assert!(
                first <= new_mail[0],
                "{case}: the group offers the new mail (first offered {first}, new mail \
                 {new_mail:?})"
            );
            assert_eq!(
                next.state.inbox_owed(&next.node),
                None,
                "{case}: the hand-over, above the fork, dropped"
            );
        } else {
            assert!(
                first > new_mail[0],
                "{case}: CONTROL: the kept identity commits the hand-over over the new \
                 mail (first offered {first}, new mail {new_mail:?})"
            );
        }
        next.close_subscriptions();
        drop(next_lane);
    }
}

#[cfg(unix)]
#[test]
fn a_refused_live_records_notice_and_its_commit_are_one_record() {
    const SID: &str = "s-host";
    let (lane, mut bridge) = FakeVerbLane::bridge(&[(1, SID)]);
    let (records, mut reader, socket, _broker, _server) = burst_rig(&lane, &mut bridge, SID, 1);
    let node = bridge.node.clone();
    let face = format!("/f/f/in/{node}/{SID}/n-peer/note");
    let refused = b"v=1 t=1 from=s-peer via=NOT-A-PRINCIPAL text=x";
    let (mut writer, _) = bridge.connect().unwrap();
    let mut refused_at = |seq: u64| writer.publish(93, seq, &face, refused).unwrap().0;
    let notices = format!("/f/f/in/n-peer/s-peer/{node}/undeliverable");
    let answered = |reader: &mut Conn| -> Vec<Option<u64>> {
        let (rows, _) = reader.fetch(0, &notices, 64).unwrap();
        rows.iter().map(|(_, _, b)| Body::decode(b).0.re).collect()
    };
    bridge.on_inbox_record(&records[0], None);

    let live = refused_at(1);
    let before = log_head(&mut reader);
    bridge.on_inbox_record(&(live, face.clone(), refused.to_vec()), None);
    assert_eq!(
        log_head(&mut reader) - before,
        2,
        "the ev, and notice+commit"
    );
    assert_eq!(answered(&mut reader), [Some(live)]);
    assert_eq!(bridge.committed, Some(live));
    let group = subject::inbox_group("f", &node);
    let filter = subject::inbox_filter("f", &node);
    let (mut w, _) = bridge.connect().unwrap();
    let (sentinel, _) = w
        .publish(94, 1, &face, b"v=1 t=2 from=s-peer text=s")
        .unwrap();
    let (client, _) = transport::connect(&socket, aterm_spec::wait::MUST_HAPPEN).unwrap();
    let mut sub = client.subscribe_group(&group, &filter).unwrap();
    assert_eq!(
        next_delivery(&mut sub, "the cursor after the notice").0,
        sentinel,
        "the notice's own commit moved the durable cursor past the record"
    );
    drop(sub);
    bridge.on_inbox_record(
        &(
            sentinel,
            face.clone(),
            b"v=1 t=2 from=s-peer text=s".to_vec(),
        ),
        None,
    );

    let old_way = refused_at(2);
    let before = log_head(&mut reader);
    assert_eq!(
        bridge.deliver_record(old_way, &face, refused, None),
        Delivery::Accounted
    );
    bridge.commit_upto(old_way);
    bridge.settle_acks();
    assert_eq!(
        log_head(&mut reader) - before,
        3,
        "ev, notice, then a commit"
    );

    let refilled = refused_at(3);
    bridge.state.set_seen_off(SID, refilled - 1).unwrap();
    let committed = bridge.committed;
    let before = log_head(&mut reader);
    assert!(bridge.refill(SID));
    assert_eq!(
        log_head(&mut reader) - before,
        2,
        "ev and notice, no commit"
    );
    assert_eq!(bridge.committed, committed, "a refill commits nothing");
    let before = log_head(&mut reader);
    bridge.on_inbox_record(&(refilled, face.clone(), refused.to_vec()), None);
    assert_eq!(log_head(&mut reader) - before, 2, "ev and a plain commit");
    assert_eq!(bridge.committed, Some(refilled));

    let floored = refused_at(4);
    bridge.commit_floor = Some(floored);
    let before = log_head(&mut reader);
    bridge.on_inbox_record(&(floored, face.clone(), refused.to_vec()), None);
    assert_eq!(
        log_head(&mut reader) - before,
        3,
        "ev, the notice alone, and a commit stopped below the floor"
    );
    assert_eq!(bridge.committed, Some(floored - 1), "never at the floor");
    assert_eq!(
        answered(&mut reader),
        [Some(live), Some(old_way), Some(refilled), Some(floored)],
        "every refusal told its sender once"
    );
}

#[cfg(unix)]
#[test]
fn an_attach_sends_every_sessions_presence_row_before_reading_one_ack() {
    const N: u64 = 24;
    let sids: Vec<String> = (1..=N).map(|i| format!("s-p{i:02}")).collect();
    let roster: Vec<(u64, &str)> = sids
        .iter()
        .enumerate()
        .map(|(i, sid)| (i as u64 + 1, sid.as_str()))
        .collect();
    let (lane, mut bridge) = FakeVerbLane::bridge(&roster);
    assert!(bridge.refresh_sessions().is_ok());
    let (socket, _broker, _server) = serve_log(&lane, "attach");
    bridge.cfg.broker = socket;
    assert!(bridge.attach_broker());
    let link = bridge.conn.as_ref().expect("attached");
    assert_eq!(link.unacked(), 0, "the attach read every ack it owed");
    assert!(
        link.peak() >= N as usize,
        "the rows went out together: peak {}",
        link.peak()
    );
    let (mut reader, _) = bridge.connect().unwrap();
    let mut rows = BTreeSet::new();
    transport::walk_last(
        &mut reader,
        &format!("/f/f/pub/{}/*/presence", bridge.node),
        |(_, subject, raw)| {
            if Body::decode(raw).0.unknown.get("state").map(String::as_str) == Some("live") {
                rows.insert(subject.split('/').nth(5).unwrap_or_default().to_string());
            }
            Ok(())
        },
    )
    .unwrap();
    for sid in &sids {
        assert!(rows.contains(sid), "{sid}'s row landed: {rows:?}");
    }

    bridge.conn = Some(bridge.connect().unwrap().0.into());
    for sid in &sids {
        bridge.publish_session_presence(sid, "live");
    }
    assert_eq!(
        bridge.conn.as_ref().unwrap().peak(),
        1,
        "the control: one at a time"
    );
}

fn queued_note(sid: &str, id: u64, to: &str, text: &str) -> String {
    format!(
        "post sid={sid} id={id} to={to} kind=note len={}\n{text}",
        text.len()
    )
}

/// `frame` without the outbox row of `sid` that names `id` (`id=<n>` for a post, `rid=<n>`
/// for a receipt).
fn without_row(frame: &str, sid: &str, id: &str) -> String {
    let sid = format!("sid={sid}");
    let mut kept = String::new();
    let mut rest = frame;
    while let Some((line, tail)) = rest.split_once('\n') {
        let words: Vec<&str> = line.split_whitespace().collect();
        let len = words
            .iter()
            .find_map(|word| word.strip_prefix("len="))
            .and_then(|n| n.parse::<usize>().ok())
            .filter(|_| line.starts_with("post "))
            .unwrap_or(0)
            .min(tail.len());
        let (body, next) = tail.split_at(len);
        if !(words.contains(&sid.as_str()) && words.contains(&id)) {
            kept.push_str(line);
            kept.push('\n');
            kept.push_str(body);
        }
        rest = next;
    }
    kept
}

/// Every retirement aterm was told ([`retirements`]): the sender, the post's id, and the
/// offset it was retired at (`None` for `off=-`).
type Retired = Vec<(String, u64, Option<u64>)>;

fn retirements(lane: &FakeVerbLane) -> Retired {
    lane.sent
        .lock()
        .unwrap()
        .iter()
        .filter_map(|line| {
            let mut words = line.strip_prefix("outbox sent ")?.split_whitespace();
            let sid = words.next()?.to_string();
            let id = words.next()?.parse().ok()?;
            let off = words.next()?.strip_prefix("off=")?.parse().ok();
            Some((sid, id, off))
        })
        .collect()
}

fn notes_on(reader: &mut Conn, subject: &str) -> Vec<(u64, String)> {
    let (rows, _) = reader.fetch(0, subject, 256).unwrap();
    rows.iter()
        .map(|(off, _, raw)| (*off, Body::decode(raw).0.text))
        .collect()
}

#[cfg(unix)]
#[test]
fn a_burst_from_many_senders_is_on_the_wire_at_once_and_each_lands_in_its_order() {
    const SENDERS: [&str; 3] = ["s-a", "s-b", "s-c"];
    let (lane, mut bridge) =
        FakeVerbLane::bridge(&[(1, "s-a"), (2, "s-b"), (3, "s-c"), (4, "s-r")]);
    assert!(bridge.refresh_sessions().is_ok());
    let (socket, _broker, _server) = serve_log(&lane, "burst");
    bridge.cfg.broker = socket;
    bridge.conn = Some(bridge.connect().unwrap().0.into());
    let lane_of = format!("/f/f/in/{0}/s-r/{0}/note", bridge.node);
    bridge.indexed_subjects.insert(lane_of.clone());
    let burst = |key: bool| -> String {
        let mut frame = String::new();
        for n in 1..=3u64 {
            for sid in SENDERS {
                let text = format!("{sid}-{n}");
                let line = queued_note(sid, n, "@s-r", &text);
                frame.push_str(&if key {
                    line.replacen(" kind=", &format!(" key={text} kind="), 1)
                } else {
                    line
                });
            }
        }
        frame
    };
    *lane.outbox.lock().unwrap() = Some(burst(false));
    bridge.drain_outbox();

    let link = bridge.conn.as_ref().expect("still connected");
    assert_eq!(
        link.unacked(),
        0,
        "a drain outside the round reads its acks"
    );
    assert_eq!(
        link.peak(),
        SENDERS.len(),
        "one post of each sender on the wire at once, never two of one"
    );
    let retired = retirements(&lane);
    assert_eq!(retired.len(), 9, "every post retired once: {retired:#?}");
    let (mut reader, _) = bridge.connect().unwrap();
    let notes = notes_on(&mut reader, &lane_of);
    assert_eq!(notes.len(), 9, "each note exactly once: {notes:#?}");
    for sid in SENDERS {
        let mine: Vec<String> = notes
            .iter()
            .map(|(_, text)| text.clone())
            .filter(|text| text.starts_with(sid))
            .collect();
        let posted: Vec<String> = (1..=3).map(|n| format!("{sid}-{n}")).collect();
        assert_eq!(mine, posted, "{sid}'s notes in the order it posted them");
        for n in 1..=3u64 {
            let at = notes
                .iter()
                .find(|(_, text)| *text == format!("{sid}-{n}"))
                .map(|(off, _)| *off);
            assert!(
                retired.contains(&(sid.to_string(), n, at)),
                "{sid} {n} retired at its own record's offset {at:?}: {retired:#?}"
            );
            assert_eq!(bridge.state.post_pin(sid, n).unwrap(), None);
            assert!(at.is_some_and(|off| bridge.self_acked.contains(&off)));
        }
    }
    assert!(
        bridge.state.in_flight().unwrap().is_empty(),
        "every own-lane entry went with its answer"
    );

    bridge.conn = Some(bridge.connect().unwrap().0.into());
    lane.sent.lock().unwrap().clear();
    *lane.outbox.lock().unwrap() = Some(burst(true));
    bridge.drain_outbox();
    assert_eq!(retirements(&lane).len(), 9);
    assert_eq!(
        bridge.conn.as_ref().unwrap().peak(),
        0,
        "the control: nothing pipelined"
    );
}

/// `s-a`'s post 1 under a pin from a build before publication identities (no identity), and
/// another writer's record at its sequence on `face`: astream's dedup answers the retry
/// there, and the read back cannot tell that record from the post's first copy, so the
/// answer is unverifiable ([`Bridge::unverified_publication`]'s case 2). The record's
/// offset.
#[cfg(unix)]
fn a_legacy_pin_another_writer_answers(bridge: &mut Bridge, face: &str) -> u64 {
    let pin = PublishPin {
        seq: bridge.next_seq().unwrap(),
        via_key: false,
        identity: None,
        original: None,
        original_off: None,
        owner: Some(1),
        at: None,
    };
    bridge.unsent = None;
    bridge.state.set_post_pin("s-a", 1, &pin).unwrap();
    let (mut other, _) = bridge.connect().unwrap();
    let (foreign, deduped) = other
        .publish(
            bridge.producer_id,
            pin.seq,
            face,
            b"v=1 t=1 from=s-x text=another%20writer",
        )
        .unwrap();
    assert!(!deduped);
    foreign
}

/// The log's head, read on a connection of its own.
#[cfg(unix)]
fn head_of(bridge: &Bridge) -> u64 {
    let (mut reader, _) = bridge.connect().unwrap();
    let (_, (_, head)) = reader.fetch(0, "/f/>", 0).unwrap();
    head
}

/// The offset aterm was told `sid`'s post `id` landed at, if it was.
fn retired_at(retired: &Retired, sid: &str, id: u64) -> Option<u64> {
    retired
        .iter()
        .find(|(s, i, _)| s == sid && *i == id)
        .and_then(|&(_, _, off)| off)
}

/// RETENTION CUTS every record below the head, and their dedup keys with them, and the loop
/// runs as [`Bridge::pass`] does, parked no later than a hold's end and draining at it
/// ([`Bridge::after_holds`]), until aterm was told both of `s-a`'s posts landed.
#[cfg(unix)]
fn cut_and_run_out_the_holds(
    lane: &FakeVerbLane,
    bridge: &mut Bridge,
    broker: &astream_broker::Broker,
) -> Retired {
    broker
        .retain_before_carrying_state(head_of(bridge))
        .unwrap();
    let deadline = Instant::now() + aterm_spec::wait::MUST_HAPPEN;
    let done = |lane: &FakeVerbLane| {
        let retired = retirements(lane);
        retired_at(&retired, "s-a", 1).is_some() && retired_at(&retired, "s-a", 2).is_some()
    };
    while !done(lane) {
        assert!(
            Instant::now() < deadline,
            "HANG: s-a's posts were not retired in a minute: {:#?}",
            retirements(lane)
        );
        let now = bridge.fx.now();
        match bridge.unverified.next_end(now) {
            Some(end) => {
                let wait = bridge.duty_wait(now);
                assert!(
                    wait <= end.saturating_duration_since(now),
                    "the loop parks no later than the hold's end: {wait:?}"
                );
                bridge.fx.sleep(wait);
                bridge.after_holds();
            }
            None => bridge.drain_outbox(),
        }
    }
    retirements(lane)
}

/// What one world of [`an_unverifiable_reservation_holds_only_its_own_sender`] showed.
#[cfg(unix)]
struct HeldWorld {
    /// What the first drain retired, and whether it kept the link.
    first: Retired,
    kept: bool,
    /// Whether that drain held `s-a` and made no pin for its post 2.
    held: bool,
    /// Every retirement once retention cut the record the answer named and the holds ran
    /// out: empty where the first drain lost the link.
    last: Retired,
    /// The record another writer put at `s-a`'s pinned sequence.
    foreign: u64,
}

#[cfg(unix)]
fn unverifiable_reservation_world(fx: crate::fx::Fx) -> HeldWorld {
    let (lane, mut bridge) = FakeVerbLane::bridge(&[(1, "s-a"), (2, "s-b"), (3, "s-r")]);
    bridge.fx = fx;
    assert!(bridge.refresh_sessions().is_ok());
    lane.retire_from_outbox();
    let (broker, _server) = on_a_broker(&lane, &mut bridge, true);
    let lane_of = format!("/f/f/in/{0}/s-r/{0}/note", bridge.node);
    bridge.indexed_subjects.insert(lane_of.clone());
    let foreign = a_legacy_pin_another_writer_answers(&mut bridge, &lane_of);
    *lane.outbox.lock().unwrap() = Some(
        [("s-a", 1), ("s-b", 1), ("s-a", 2), ("s-b", 2)]
            .iter()
            .map(|&(sid, id)| queued_note(sid, id, "@s-r", &format!("{}{id}", &sid[2..])))
            .collect(),
    );
    bridge.drain_outbox();
    let first = retirements(&lane);
    let kept = bridge.conn.is_some();
    let held = bridge.unverified.holds(
        "s-a",
        crate::unverified_hold::Domain::Posts,
        bridge.fx.now(),
    ) && bridge.state.post_pin("s-a", 2).unwrap().is_none();
    let last = if kept {
        cut_and_run_out_the_holds(&lane, &mut bridge, &broker)
    } else {
        Vec::new()
    };
    HeldWorld {
        first,
        kept,
        held,
        last,
        foreign,
    }
}

/// **AN UNVERIFIABLE RESERVATION HOLDS ONLY ITS OWN SENDER** (critic item F21). `s-a`'s post
/// 1 is retried at a pin from a build before publication identities, and astream's dedup
/// answers it with another writer's record, which its read back cannot tell from the post's
/// first copy ([`Bridge::unverified_publication`]): never taken for its delivery, it stays
/// queued at its pin. aterm lists `s-a`'s two posts and `s-b`'s two, interleaved, `s-a`'s
/// first. The drain holds `s-a` (no pin is made for its post 2) and lands both of `s-b`'s on
/// the link it keeps. Once retention cuts the record the answer named, and its key with it,
/// the loop parks to the hold's end, and the drain's retry at the pin appends anew: `s-a`'s
/// posts land, in its order, above that record. NEGATIVE CONTROLS:
/// `Mutant::UnverifiableDropsTheLink`, the whole-link stop of every build before F21: the
/// drain ends at `s-a`'s post 1 and `s-b`'s posts wait with it; and
/// `Mutant::UnverifiableHoldsNothing`: the drain goes past the reservation to `s-a`'s post
/// 2, which lands ahead of post 1's retry.
#[cfg(unix)]
#[test]
fn an_unverifiable_reservation_holds_only_its_own_sender() {
    let world = unverifiable_reservation_world(crate::fx::Fx::os());
    assert!(
        world.kept,
        "a verdict is no transport fault: the link is kept"
    );
    assert!(world.held, "s-a is held, its post 2 unpinned");
    let b1 = retired_at(&world.first, "s-b", 1);
    let b2 = retired_at(&world.first, "s-b", 2);
    assert!(
        b1.zip(b2).is_some_and(|(b1, b2)| b1 < b2),
        "s-b's posts land, in its order, while s-a is held: {:#?}",
        world.first
    );
    assert!(
        world.first.iter().all(|(sid, _, _)| sid != "s-a"),
        "nothing of s-a's is retired while it is held: {:#?}",
        world.first
    );
    let a1 = retired_at(&world.last, "s-a", 1);
    let a2 = retired_at(&world.last, "s-a", 2);
    assert!(
        a1.zip(a2)
            .is_some_and(|(a1, a2)| world.foreign < a1 && a1 < a2),
        "past the cut s-a's posts land in its order, above the record the answer named \
         (@{}): {:#?}",
        world.foreign,
        world.last
    );

    let stop = unverifiable_reservation_world(crate::fx::Fx::os_without(
        crate::sim::world::Mutant::UnverifiableDropsTheLink,
    ));
    assert!(
        !stop.kept && stop.first.is_empty(),
        "CONTROL: the link dropped at s-a's reservation, and s-b's posts waited with it: \
         {:#?}",
        stop.first
    );

    let unheld = unverifiable_reservation_world(crate::fx::Fx::os_without(
        crate::sim::world::Mutant::UnverifiableHoldsNothing,
    ));
    let (a1, a2) = (
        retired_at(&unheld.last, "s-a", 1),
        retired_at(&unheld.last, "s-a", 2),
    );
    assert!(
        retired_at(&unheld.first, "s-a", 2).is_some() && a1.zip(a2).is_some_and(|(a1, a2)| a2 < a1),
        "CONTROL: s-a's post 2 lands ahead of post 1's retry: first {:#?}, last {:#?}",
        unheld.first,
        unheld.last
    );
}

/// What one world of [`the_attachs_replay_holds_an_unverifiable_entrys_sender`] showed.
#[cfg(unix)]
struct ReplayWorld {
    /// Whether the attach's replay settled the journal.
    replayed: bool,
    /// What the attach's drain retired.
    first: Retired,
    /// Where `s-a`'s post 2 was on the bus before the attach, if it was.
    landed: Option<u64>,
    /// Whether `s-a`'s post 2's record still waited for the drain after it.
    waits: bool,
    /// Every retirement once retention cut the record the answer named and the holds ran
    /// out, for a post 2 that had not landed.
    last: Retired,
}

#[cfg(unix)]
fn replay_behind_an_unverifiable_entry(fx: crate::fx::Fx, landed: bool) -> ReplayWorld {
    let (lane, mut bridge) = FakeVerbLane::bridge(&[(1, "s-a"), (2, "s-b"), (3, "s-r")]);
    bridge.fx = fx;
    assert!(bridge.refresh_sessions().is_ok());
    lane.retire_from_outbox();
    let (broker, _server) = on_a_broker(&lane, &mut bridge, true);
    let lane_of = format!("/f/f/in/{0}/s-r/{0}/note", bridge.node);
    bridge.indexed_subjects.insert(lane_of.clone());
    let log = bridge.conn.as_ref().and_then(|c| c.seen_log_identity());
    assert!(log.is_some(), "PRECONDITION: the link read its log");
    let head = head_of(&bridge);
    let words = |sid: &str, text: &str| {
        let mut body = Body::new(1);
        body.from = Some(sid.to_string());
        body.text = text.to_string();
        body.encode()
    };
    let entry = |pin: &PublishPin, body: Vec<u8>| InFlight {
        pin: pin.clone(),
        subject: lane_of.clone(),
        body,
        log,
        at: Some(head),
    };
    // s-a's post 1, in flight under a pin another writer's record answers.
    a_legacy_pin_another_writer_answers(&mut bridge, &lane_of);
    let pin1 = bridge.state.post_pin("s-a", 1).unwrap().unwrap();
    bridge
        .state
        .set_in_flight(&entry(&pin1, words("s-a", "a1")))
        .unwrap();
    let pinned = |bridge: &mut Bridge, sid: &str, id: u64, body: &[u8]| {
        let pin = PublishPin {
            seq: bridge.next_seq().unwrap(),
            via_key: false,
            identity: Some(publication_identity(&lane_of, body)),
            original: None,
            original_off: None,
            owner: Some(id),
            at: Some(head),
        };
        bridge.unsent = None;
        bridge.state.set_post_pin(sid, id, &pin).unwrap();
        bridge
            .state
            .set_in_flight(&entry(&pin, body.to_vec()))
            .unwrap();
        pin
    };
    // s-a's post 2 behind it, and s-b's post 1 behind both, each in flight.
    let a2 = words("s-a", "a2");
    let pin2 = pinned(&mut bridge, "s-a", 2, &a2);
    pinned(&mut bridge, "s-b", 1, &words("s-b", "b1"));
    let landed = landed.then(|| {
        let (mut other, _) = bridge.connect().unwrap();
        let (off, deduped) = other
            .publish(bridge.producer_id, pin2.seq, &lane_of, &a2)
            .unwrap();
        assert!(!deduped);
        off
    });
    *lane.outbox.lock().unwrap() = Some(
        [("s-a", 1), ("s-b", 1), ("s-a", 2)]
            .iter()
            .map(|&(sid, id)| queued_note(sid, id, "@s-r", &format!("{}{id}", &sid[2..])))
            .collect(),
    );
    let replayed = bridge.settle_in_flight();
    bridge.drain_outbox();
    let first = retirements(&lane);
    let waits = bridge.awaiting_drain.contains_key(&pin2.seq);
    let last = if landed.is_none() && bridge.conn.is_some() {
        cut_and_run_out_the_holds(&lane, &mut bridge, &broker)
    } else {
        Vec::new()
    };
    ReplayWorld {
        replayed,
        first,
        landed,
        waits,
        last,
    }
}

/// **THE ATTACH'S REPLAY HOLDS AN UNVERIFIABLE ENTRY'S SENDER, AND ONLY IT** (critic item
/// F21, [`Bridge::replay_in_flight`]'s point 5). `s-a`'s post 1, its post 2 and `s-b`'s post
/// 1 are in flight, by sequence, and the replay's answer to post 1 is unverifiable (a pin
/// from a build before publication identities, another writer's record at its sequence): its
/// entry is dropped and the post kept queued at its pin, and no fence follows a verdict.
/// Where `s-a`'s post 2 had not landed, the replay reads it back, finds none and does not
/// publish it, its post kept queued at its pin; `s-b`'s goes on, and the attach's drain
/// retires it while `s-a` is held. Past the cut `s-a`'s posts land in its order. Where post
/// 2 had landed, the replay settles it where it is, held for the drain, which retires it
/// though `s-a` is held: nothing is sent, and its record does not wait out the hold,
/// dropping the link at each delivery that meets it. NEGATIVE CONTROLS:
/// `Mutant::ReplayPastAnUnverifiableEntry`: the replay publishes post 2, which lands ahead of
/// post 1's retry; `Mutant::HeldRecordWaitsForTheHold`: the landed post 2 waits for the hold,
/// its record held from every delivery.
#[cfg(unix)]
#[test]
fn the_attachs_replay_holds_an_unverifiable_entrys_sender() {
    let world = replay_behind_an_unverifiable_entry(crate::fx::Fx::os(), false);
    assert!(world.replayed, "the replay settles the journal");
    assert!(
        retired_at(&world.first, "s-b", 1).is_some()
            && world.first.iter().all(|(sid, _, _)| sid != "s-a"),
        "s-b's post lands while s-a is held: {:#?}",
        world.first
    );
    let (a1, a2) = (
        retired_at(&world.last, "s-a", 1),
        retired_at(&world.last, "s-a", 2),
    );
    assert!(
        a1.zip(a2).is_some_and(|(a1, a2)| a1 < a2),
        "past the cut s-a's posts land in its order: {:#?}",
        world.last
    );
    let past = replay_behind_an_unverifiable_entry(
        crate::fx::Fx::os_without(crate::sim::world::Mutant::ReplayPastAnUnverifiableEntry),
        false,
    );
    let (a1, a2) = (
        retired_at(&past.last, "s-a", 1),
        retired_at(&past.last, "s-a", 2),
    );
    assert!(
        a1.zip(a2).is_some_and(|(a1, a2)| a2 < a1),
        "CONTROL: the replay published s-a's post 2 ahead of post 1's retry: {:#?}",
        past.last
    );

    let landed = replay_behind_an_unverifiable_entry(crate::fx::Fx::os(), true);
    assert!(
        landed.landed.is_some()
            && retired_at(&landed.first, "s-a", 2) == landed.landed
            && retired_at(&landed.first, "s-a", 1).is_none()
            && !landed.waits,
        "a landed post 2 is retired where it landed while s-a is held: {:#?}",
        landed.first
    );
    let waiting = replay_behind_an_unverifiable_entry(
        crate::fx::Fx::os_without(crate::sim::world::Mutant::HeldRecordWaitsForTheHold),
        true,
    );
    assert!(
        retired_at(&waiting.first, "s-a", 2).is_none() && waiting.waits,
        "CONTROL: the landed post 2 waits for the hold, its record held from delivery: \
         {:#?}",
        waiting.first
    );
}

/// `sid`'s entry at `pin`, of `body` on `subject`, journaled at `head` on the link's log.
#[cfg(unix)]
fn journal_entry(bridge: &Bridge, pin: &PublishPin, subject: &str, body: &[u8], head: u64) {
    let log = bridge.conn.as_ref().and_then(|c| c.seen_log_identity());
    assert!(log.is_some(), "PRECONDITION: the link read its log");
    bridge
        .state
        .set_in_flight(&InFlight {
            pin: pin.clone(),
            subject: subject.to_string(),
            body: body.to_vec(),
            log,
            at: Some(head),
        })
        .unwrap();
}

/// A fresh pin of `body`'s words on `subject` at the next sequence, made at `head`.
#[cfg(unix)]
fn fresh_pin(bridge: &mut Bridge, subject: &str, body: &[u8], head: u64) -> PublishPin {
    let pin = PublishPin {
        seq: bridge.next_seq().unwrap(),
        via_key: false,
        identity: Some(publication_identity(subject, body)),
        original: None,
        original_off: None,
        owner: None,
        at: Some(head),
    };
    bridge.unsent = None;
    pin
}

/// `body` on `subject` at sequence `seq` under the bridge's producer id, from a connection
/// of its own, as an earlier attempt landed it: its offset.
#[cfg(unix)]
fn landed_earlier(bridge: &Bridge, seq: u64, subject: &str, body: &[u8]) -> u64 {
    let (mut other, _) = bridge.connect().unwrap();
    let (off, deduped) = other
        .publish(bridge.producer_id, seq, subject, body)
        .unwrap();
    assert!(!deduped, "PRECONDITION: a first copy");
    off
}

/// What one world of
/// [`an_entry_whose_post_left_the_outbox_is_published_behind_an_unverifiable_one`] showed.
#[cfg(unix)]
struct LeftWorld {
    replayed: bool,
    /// The copies of `s-a`'s post 2 on the bus after the attach and its drain.
    copies: usize,
    /// Whether post 2's entry still stood.
    stands: bool,
}

#[cfg(unix)]
fn left_the_outbox_behind_an_unverifiable_entry(fx: crate::fx::Fx) -> LeftWorld {
    let (lane, mut bridge) = FakeVerbLane::bridge(&[(1, "s-a"), (3, "s-r")]);
    bridge.fx = fx;
    assert!(bridge.refresh_sessions().is_ok());
    lane.retire_from_outbox();
    let (_broker, _server) = on_a_broker(&lane, &mut bridge, true);
    let lane_of = format!("/f/f/in/{0}/s-r/{0}/note", bridge.node);
    bridge.indexed_subjects.insert(lane_of.clone());
    let head = head_of(&bridge);
    let words = |text: &str| {
        let mut body = Body::new(1);
        body.from = Some("s-a".to_string());
        body.text = text.to_string();
        body.encode()
    };
    // s-a's post 1, in flight under a pin another writer's record answers, still queued.
    a_legacy_pin_another_writer_answers(&mut bridge, &lane_of);
    let pin1 = bridge.state.post_pin("s-a", 1).unwrap().unwrap();
    journal_entry(&bridge, &pin1, &lane_of, &words("a1"), head);
    // s-a's post 2 behind it: aterm was told it landed, so it left the outbox, and its
    // entry stands (a kill before the journal's write); retention gave up its record, which
    // no reader was handed, so it is not on the bus.
    let a2 = words("a2");
    let pin2 = fresh_pin(&mut bridge, &lane_of, &a2, head);
    journal_entry(&bridge, &pin2, &lane_of, &a2, head);
    *lane.outbox.lock().unwrap() = Some(queued_note("s-a", 1, "@s-r", "a1"));
    let replayed = bridge.settle_in_flight();
    bridge.drain_outbox();
    let (mut reader, _) = bridge.connect().unwrap();
    let copies = notes_on(&mut reader, &lane_of)
        .iter()
        .filter(|(_, text)| text == "a2")
        .count();
    let stands = bridge
        .state
        .in_flight()
        .unwrap()
        .iter()
        .any(|(seq, _)| *seq == pin2.seq);
    LeftWorld {
        replayed,
        copies,
        stands,
    }
}

/// **AN ENTRY WHOSE POST LEFT THE OUTBOX IS PUBLISHED BEHIND AN UNVERIFIABLE ONE** (critic
/// item F21's review, [`Bridge::replay_in_flight`]'s point 5). `s-a`'s post 1 is in flight
/// under a pin another writer's record answers, and still queued; its post 2's entry stands
/// though aterm no longer lists it, its record not on the bus. The replay holds only the
/// lanes aterm lists an entry in, so post 2's entry, all that remembers it, is published at
/// its pin: one copy, and its entry ends. NEGATIVE CONTROL (`Mutant::HoldPastTheListing`):
/// held by its record's `from=`, the entry is dropped, and the drain, which aterm no longer
/// lists it to, never sends it: no copy.
#[cfg(unix)]
#[test]
fn an_entry_whose_post_left_the_outbox_is_published_behind_an_unverifiable_one() {
    let world = left_the_outbox_behind_an_unverifiable_entry(crate::fx::Fx::os());
    assert!(world.replayed, "the replay settles the journal");
    assert!(
        world.copies == 1 && !world.stands,
        "post 2 is published once and its entry ends: {} copies, standing {}",
        world.copies,
        world.stands
    );
    let control = left_the_outbox_behind_an_unverifiable_entry(crate::fx::Fx::os_without(
        crate::sim::world::Mutant::HoldPastTheListing,
    ));
    assert!(
        control.copies == 0 && !control.stands,
        "CONTROL: post 2's entry is dropped and nothing publishes it: {} copies",
        control.copies
    );
}

/// What one world of
/// [`a_held_lanes_receipt_or_keyed_post_the_replay_read_back_is_retired_where_it_landed`]
/// showed.
#[cfg(unix)]
struct HeldReadBack {
    replayed: bool,
    /// Whether the drain held the lane behind the first one's unverifiable retry.
    held: bool,
    /// Where the second one's record landed, and where aterm was told it did, if it was.
    landed: u64,
    retired: Option<u64>,
    /// Whether its record still waited for the drain, and whether a delivery of it would
    /// keep the link.
    waits: bool,
    delivers: bool,
}

/// The world of a receipt (`keyed` false) or a keyed re-post (`keyed` true) whose record
/// the attach's replay reads back behind an unverifiable reservation of its lane's.
#[cfg(unix)]
fn held_lane_read_back(fx: crate::fx::Fx, keyed: bool) -> HeldReadBack {
    let (lane, mut bridge) = FakeVerbLane::bridge(&[(1, "s-a"), (2, "s-b"), (3, "s-r")]);
    bridge.fx = fx;
    assert!(bridge.refresh_sessions().is_ok());
    lane.retire_from_outbox();
    let (_broker, _server) = on_a_broker(&lane, &mut bridge, true);
    let head = head_of(&bridge);
    let (sid, domain, second) = if keyed {
        let face = format!("/f/f/in/{0}/s-r/{0}/note", bridge.node);
        bridge.indexed_subjects.insert(face.clone());
        let words = |text: &str| {
            let mut body = Body::new(1);
            body.from = Some("s-a".to_string());
            body.text = text.to_string();
            body.encode()
        };
        // s-a's post 1 under a pin another writer's record answers; its post 2 a re-post
        // of a key post 9 holds, at the key's sequence.
        a_legacy_pin_another_writer_answers(&mut bridge, &face);
        let pin1 = bridge.state.post_pin("s-a", 1).unwrap().unwrap();
        journal_entry(&bridge, &pin1, &face, &words("a1"), head);
        let a2 = words("a2");
        let fresh = fresh_pin(&mut bridge, &face, &a2, head);
        let original = publication_identity(&face, &words("the key's first words"));
        let holder = PublishPin {
            identity: Some(original.clone()),
            owner: Some(9),
            ..fresh.clone()
        };
        bridge.state.set_key_pin("s-a", "k", &holder).unwrap();
        let pin2 = PublishPin {
            via_key: true,
            original: Some(original),
            original_off: Some(head),
            owner: Some(9),
            ..fresh
        };
        bridge.state.set_post_pin("s-a", 2, &pin2).unwrap();
        journal_entry(&bridge, &pin2, &face, &a2, head);
        let landed = landed_earlier(&bridge, pin2.seq, &face, &a2);
        *lane.outbox.lock().unwrap() = Some(format!(
            "{}{}",
            queued_note("s-a", 1, "@s-r", "a1"),
            queued_note("s-a", 2, "@s-r", "a2").replacen(" kind=", " key=k kind=", 1)
        ));
        ("s-a", crate::unverified_hold::Domain::Posts, landed)
    } else {
        let prefix = bridge.receipt_lane("s-a").unwrap().unwrap();
        let face = format!("{prefix}/ack");
        let receipt = |bridge: &mut Bridge, off: u64| {
            let re = bridge.wire_re(&prefix, off).unwrap();
            receipt_body(1, "s-b", re, "handled").encode()
        };
        // s-b's receipt 1 under a pin from a build before publication identities, another
        // writer's record at its sequence; its receipt 2 landed, its entry standing.
        let legacy = PublishPin {
            seq: bridge.next_seq().unwrap(),
            via_key: false,
            identity: None,
            original: None,
            original_off: None,
            owner: None,
            at: None,
        };
        bridge.unsent = None;
        bridge.state.set_receipt_pin("s-b", 1, &legacy).unwrap();
        landed_earlier(
            &bridge,
            legacy.seq,
            &face,
            b"v=1 t=1 from=s-x text=another%20writer",
        );
        let r1 = receipt(&mut bridge, 3);
        journal_entry(&bridge, &legacy, &face, &r1, head);
        let r2 = receipt(&mut bridge, 4);
        let pin2 = fresh_pin(&mut bridge, &face, &r2, head);
        bridge.state.set_receipt_pin("s-b", 2, &pin2).unwrap();
        journal_entry(&bridge, &pin2, &face, &r2, head);
        let landed = landed_earlier(&bridge, pin2.seq, &face, &r2);
        *lane.outbox.lock().unwrap() = Some(
            "receipt sid=s-b rid=1 off=3 verdict=handled kind=ask from=s-a\n\
             receipt sid=s-b rid=2 off=4 verdict=handled kind=ask from=s-a\n"
                .to_string(),
        );
        ("s-b", crate::unverified_hold::Domain::Receipts, landed)
    };
    let replayed = bridge.settle_in_flight();
    bridge.drain_outbox();
    let held = bridge.unverified.holds(sid, domain, bridge.fx.now());
    let retired = if keyed {
        retired_at(&retirements(&lane), "s-a", 2)
    } else {
        lane.requests.lock().unwrap().iter().find_map(|line| {
            line.strip_prefix("deliver s-b receipt=2 off=")
                .and_then(|off| off.parse().ok())
        })
    };
    let waits = bridge
        .awaiting_drain
        .values()
        .any(|&(off, _)| off == second);
    let delivers = bridge.settled_before_delivery(second);
    HeldReadBack {
        replayed,
        held,
        landed: second,
        retired,
        waits,
        delivers,
    }
}

/// **A HELD LANE'S RECEIPT OR KEYED POST THE REPLAY READ BACK IS RETIRED WHERE IT LANDED**
/// (critic item F21's review, [`Bridge::landed_behind_an_unverifiable_hold`]). Two of a
/// lane's are in flight, by sequence: the first under a pin another writer's record
/// answers, the second landed, its entry standing. The replay reads the second back and
/// holds its record for the drain; the drain's retry of the first is unverifiable and holds
/// the lane, and the second is retired at its offset all the same, nothing sent, so a
/// delivery of its record keeps the link. Both for `s-b`'s owed receipts and for `s-a`'s
/// posts, the second a keyed re-post. NEGATIVE CONTROL (`Mutant::HeldRecordWaitsForTheHold`):
/// the second waits for the hold, and each delivery of its record drops the link.
#[cfg(unix)]
#[test]
fn a_held_lanes_receipt_or_keyed_post_the_replay_read_back_is_retired_where_it_landed() {
    for keyed in [false, true] {
        let what = if keyed { "keyed post" } else { "receipt" };
        let world = held_lane_read_back(crate::fx::Fx::os(), keyed);
        assert!(
            world.replayed && world.held,
            "{what}: the replay settles the journal and the drain holds the lane"
        );
        assert!(
            world.retired == Some(world.landed) && !world.waits && world.delivers,
            "{what}: retired where it landed (@{}): retired {:?}, waits {}, delivers {}",
            world.landed,
            world.retired,
            world.waits,
            world.delivers
        );
        let control = held_lane_read_back(
            crate::fx::Fx::os_without(crate::sim::world::Mutant::HeldRecordWaitsForTheHold),
            keyed,
        );
        assert!(
            control.held && control.retired.is_none() && control.waits && !control.delivers,
            "CONTROL {what}: it waits for the hold, its record dropping the link at \
             delivery: retired {:?}, waits {}, delivers {}",
            control.retired,
            control.waits,
            control.delivers
        );
    }
}

#[cfg(unix)]
#[test]
fn a_senders_post_refused_mid_burst_holds_its_later_posts_and_no_one_elses() {
    let (lane, mut bridge) = FakeVerbLane::bridge(&[(1, "s-a"), (2, "s-b"), (3, "s-r")]);
    assert!(bridge.refresh_sessions().is_ok());
    let socket = lane.dir.path().join("refused.sock");
    let broker = astream_broker::Broker::open(lane.dir.path().join("refused.log")).unwrap();
    let _server = broker.serve(&socket).unwrap();
    let socket = socket.to_string_lossy().into_owned();
    let broker = Arc::new(broker);
    bridge.cfg.broker = socket;
    bridge.conn = Some(bridge.connect().unwrap().0.into());
    let lane_of = format!("/f/f/in/{0}/s-r/{0}/note", bridge.node);
    bridge.indexed_subjects.insert(lane_of.clone());
    *lane.outbox.lock().unwrap() = Some(
        [
            queued_note("s-a", 1, "@s-r", "a1"),
            queued_note("s-a", 2, "@s-r", "a2"),
            queued_note("s-b", 1, "@s-r", "b1"),
            queued_note("s-a", 3, "@s-r", "a3"),
            queued_note("s-b", 2, "@s-r", "b2"),
        ]
        .concat(),
    );

    broker.inject_faults(astream_broker::InjectedFaults {
        write: false,
        sync: true,
        truncate: false,
    });
    let held = broker.pause_next_acks();
    let (done_tx, done) = std::sync::mpsc::channel();
    let writer = broker.clone();
    std::thread::spawn(move || {
        let tried = held.wait();
        writer.inject_faults(astream_broker::InjectedFaults::default());
        held.resume();
        let _ = done_tx.send(tried);
    });
    bridge.drain_outbox();
    assert_eq!(
        done.recv_timeout(aterm_spec::wait::MUST_HAPPEN),
        Ok(true),
        "a1's batch was tried and held unanswered (a hang detector)"
    );

    assert!(
        bridge.conn.is_some(),
        "a refusal is an answer, not a lost link"
    );
    let retired = retirements(&lane);
    let (mut reader, _) = bridge.connect().unwrap();
    let notes = notes_on(&mut reader, &lane_of);
    let texts: Vec<&str> = notes.iter().map(|(_, text)| text.as_str()).collect();
    assert_eq!(texts, ["b1", "b2"], "only s-b's notes landed");
    for (n, text) in [(1, "b1"), (2, "b2")] {
        let at = notes.iter().find(|(_, t)| t == text).map(|(off, _)| *off);
        assert!(
            retired.contains(&("s-b".to_string(), n, at)),
            "{retired:#?}"
        );
    }
    assert!(
        retired.iter().all(|(sid, _, _)| sid == "s-b"),
        "nothing of s-a's retired: {retired:#?}"
    );
    assert!(
        bridge.state.post_pin("s-a", 1).unwrap().is_some(),
        "a1 keeps its sequence for the retry"
    );
    for n in [2, 3] {
        assert_eq!(
            bridge.state.post_pin("s-a", n).unwrap(),
            None,
            "a{n} was never reserved, let alone sent"
        );
    }
    assert!(bridge.state.in_flight().unwrap().is_empty());

    lane.sent.lock().unwrap().clear();
    *lane.outbox.lock().unwrap() = Some(
        [
            queued_note("s-a", 1, "@s-r", "a1"),
            queued_note("s-a", 2, "@s-r", "a2"),
            queued_note("s-a", 3, "@s-r", "a3"),
        ]
        .concat(),
    );
    bridge.drain_outbox();
    let retired = retirements(&lane);
    let notes = notes_on(&mut reader, &lane_of);
    let texts: Vec<&str> = notes.iter().map(|(_, text)| text.as_str()).collect();
    assert_eq!(
        texts,
        ["b1", "b2", "a1", "a2", "a3"],
        "s-a's notes once each, in its order"
    );
    for (n, text) in [(1, "a1"), (2, "a2"), (3, "a3")] {
        let at = notes.iter().find(|(_, t)| t == text).map(|(off, _)| *off);
        assert!(
            retired.contains(&("s-a".to_string(), n, at)),
            "{retired:#?}"
        );
    }
}

/// **BEHIND A BROKER'S GAP FENCE A SENDER'S POSTS GO OUT TOGETHER, AND A REFUSED ONE HOLDS
/// BACK EVERY POST SENT AFTER IT** (C12): the burst is on the wire before any answer is
/// read, several of a sender's posts at once; `a1`'s batch fails, so the broker refuses all
/// that went after it (`gap-fenced=`), whoever sent them, and the next drain lifts the fence
/// and sends them again at their pins, each sender's in its order, each post once. The
/// control is [`a_senders_post_refused_mid_burst_holds_its_later_posts_and_no_one_elses`]'s
/// broker, which does not gap-fence: there a sender's posts go one at a time.
#[cfg(unix)]
#[test]
fn behind_a_gap_fence_a_refused_post_holds_back_what_went_after_it_and_each_lands_once() {
    let (lane, mut bridge) = FakeVerbLane::bridge(&[(1, "s-a"), (2, "s-b"), (3, "s-r")]);
    assert!(bridge.refresh_sessions().is_ok());
    let (broker, _server) = on_a_broker(&lane, &mut bridge, true);
    let lane_of = format!("/f/f/in/{0}/s-r/{0}/note", bridge.node);
    bridge.indexed_subjects.insert(lane_of.clone());
    let posts = [
        queued_note("s-a", 1, "@s-r", "a1"),
        queued_note("s-a", 2, "@s-r", "a2"),
        queued_note("s-b", 1, "@s-r", "b1"),
        queued_note("s-a", 3, "@s-r", "a3"),
        queued_note("s-b", 2, "@s-r", "b2"),
    ]
    .concat();
    *lane.outbox.lock().unwrap() = Some(posts.clone());

    broker.inject_faults(astream_broker::InjectedFaults {
        write: false,
        sync: true,
        truncate: false,
    });
    let held = broker.pause_next_acks();
    let (done_tx, done) = std::sync::mpsc::channel();
    let writer = broker.clone();
    std::thread::spawn(move || {
        let tried = held.wait();
        writer.inject_faults(astream_broker::InjectedFaults::default());
        held.resume();
        let _ = done_tx.send(tried);
    });
    bridge.drain_outbox();
    assert_eq!(
        done.recv_timeout(aterm_spec::wait::MUST_HAPPEN),
        Ok(true),
        "a1's batch was tried and held unanswered (a hang detector)"
    );
    let link = bridge
        .conn
        .as_ref()
        .expect("a refusal is an answer, not a lost link");
    assert_eq!(
        link.peak(),
        5,
        "the burst went out before any answer was read, three of s-a's at once"
    );
    assert!(link.fenced(), "the refusals left the fence up");
    assert!(retirements(&lane).is_empty(), "nothing landed behind a1");
    let (mut reader, _) = bridge.connect().unwrap();
    assert!(notes_on(&mut reader, &lane_of).is_empty());
    for (sid, n) in [("s-a", 1), ("s-a", 2), ("s-b", 1), ("s-a", 3), ("s-b", 2)] {
        assert!(
            bridge.state.post_pin(sid, n).unwrap().is_some(),
            "{sid} {n} keeps its sequence for the retry"
        );
    }
    assert!(bridge.state.in_flight().unwrap().is_empty());

    lane.sent.lock().unwrap().clear();
    *lane.outbox.lock().unwrap() = Some(posts);
    bridge.drain_outbox();
    assert!(
        !bridge.conn.as_ref().unwrap().fenced(),
        "the drain lifted the fence"
    );
    let retired = retirements(&lane);
    let notes = notes_on(&mut reader, &lane_of);
    let texts: Vec<&str> = notes.iter().map(|(_, text)| text.as_str()).collect();
    for sid in ["a", "b"] {
        let mine: Vec<&str> = texts
            .iter()
            .copied()
            .filter(|text| text.starts_with(sid))
            .collect();
        let posted: &[&str] = if sid == "a" {
            &["a1", "a2", "a3"]
        } else {
            &["b1", "b2"]
        };
        assert_eq!(mine, posted, "s-{sid}'s notes once each, in its order");
    }
    assert_eq!(texts.len(), 5, "each note exactly once: {texts:?}");
    for (sid, n, text) in [
        ("s-a", 1, "a1"),
        ("s-a", 2, "a2"),
        ("s-a", 3, "a3"),
        ("s-b", 1, "b1"),
        ("s-b", 2, "b2"),
    ] {
        let at = notes.iter().find(|(_, t)| t == text).map(|(off, _)| *off);
        assert!(retired.contains(&(sid.to_string(), n, at)), "{retired:#?}");
    }
}
/// `bridge` connected to a broker that GAP-FENCES (`fenced`), as `aterm link broker` opens
/// every tier (`durability.rs`), or to one an older build opened, its `Hello` read and the
/// fence asked for where it offered it ([`Bridge::ask_gap_fences`]): only on the first does
/// the bridge keep several of a sender's posts in flight ([`Bridge::pipelines_posts`]).
#[cfg(unix)]
fn on_a_broker(
    lane: &FakeVerbLane,
    bridge: &mut Bridge,
    fenced: bool,
) -> (Arc<astream_broker::Broker>, astream_broker::BrokerHandle) {
    let socket = lane.dir.path().join("bus.sock");
    let broker = astream_broker::Broker::open_opts(
        lane.dir.path().join("bus.log"),
        astream_broker::OpenOptions::new(astream_broker::Durability::Strict)
            .with_sticky_batch_refusal(fenced),
        || Ok(astream_broker::Prepared::Nothing),
    )
    .unwrap();
    let server = broker.serve(&socket).unwrap();
    bridge.cfg.broker = socket.to_string_lossy().into_owned();
    bridge.conn = Some(bridge.connect().unwrap().0.into());
    let link = bridge.conn.as_mut().unwrap();
    link.ready().unwrap().hello().unwrap();
    assert!(
        !bridge.pipelines_posts(),
        "a Hello that offers the fence is not the fence, which the bridge has not asked for"
    );
    assert!(bridge.ask_gap_fences(), "the attach asks for the fence");
    assert_eq!(
        bridge.pipelines_posts(),
        fenced,
        "the bridge gap-fences where its Hello offered it, and only there"
    );
    (Arc::new(broker), server)
}

/// **A DRAIN THAT FINDS ITS ATERM GONE SENDS NO POST IT HAD NOT SENT** (the orphan of
/// `fabric_world`'s `an_orphaned_bridge_mid_drain_neither_publishes_more_nor_shares_the_node`).
/// The drain's look at aterm before a post is not the last call before its send: a blocking
/// one between reads an earlier post's answer, and that post's `outbox sent` meets aterm
/// gone. Each seam that reads one is driven. Behind a gap fence, where a sender's posts go
/// out without waiting for their answers: a new lane's subject-budget read (a drain reads
/// the budget at the first lane it opens, critic item G04, so the drain's first post goes
/// to a lane already open and its second opens `s-q`'s), the full window before a send,
/// and the settle before a keyed post. On a broker that does not gap-fence, where a
/// sender's post waits only for the answer to its own last: the budget read that takes
/// another sender's answer. And a retry at a pin an older build made, which names no
/// head to read back from, so the settle before its send is the first to take another
/// sender's answer.
/// Only the posts sent before the loss are on the bus, and a successor's attach, replaying
/// the entries they left, publishes none of them again.
#[cfg(unix)]
#[test]
fn a_drain_that_finds_its_aterm_gone_sends_no_post_it_had_not_sent() {
    struct Seam {
        name: &'static str,
        fenced: bool,
        indexed: bool,
        /// `s-a`'s post 1 waits at a pin an older build made (no head to read back from).
        pinned: bool,
        posts: Vec<(String, String)>,
        /// How many of `posts`, in order, went before the loss.
        sent: usize,
    }
    let note = |sid: &str, id: u64, key: Option<&str>| {
        let text = format!("{}{id}", &sid[2..]);
        let line = queued_note(sid, id, "@s-r", &text);
        let line = match key {
            Some(key) => line.replacen(" kind=", &format!(" key={key} kind="), 1),
            None => line,
        };
        (line, text)
    };
    let fresh = |sid: &str, id: u64| {
        let text = format!("{}{id}", &sid[2..]);
        (queued_note(sid, id, "@s-q", &text), text)
    };
    let window = u64::try_from(PIPELINE_WINDOW).unwrap();
    let seams = [
        Seam {
            name: "a new lane's budget read",
            fenced: true,
            indexed: true,
            pinned: false,
            posts: vec![note("s-a", 1, None), fresh("s-a", 2)],
            sent: 1,
        },
        Seam {
            name: "a full window",
            fenced: true,
            indexed: true,
            pinned: false,
            posts: (1..=window + 1).map(|id| note("s-a", id, None)).collect(),
            sent: PIPELINE_WINDOW,
        },
        Seam {
            name: "the settle before a keyed post",
            fenced: true,
            indexed: true,
            pinned: false,
            posts: vec![note("s-b", 1, None), note("s-a", 1, Some("k"))],
            sent: 1,
        },
        Seam {
            name: "a new lane's budget read, unfenced",
            fenced: false,
            indexed: true,
            pinned: false,
            posts: vec![note("s-b", 1, None), fresh("s-a", 1)],
            sent: 1,
        },
        Seam {
            name: "the settle before a retry at an older build's pin",
            fenced: true,
            indexed: true,
            pinned: true,
            posts: vec![note("s-b", 1, None), note("s-a", 1, None)],
            sent: 1,
        },
    ];
    let mut wrong = Vec::new();
    for seam in seams {
        let name = seam.name;
        let (lane, mut bridge) =
            FakeVerbLane::bridge(&[(1, "s-a"), (2, "s-b"), (3, "s-r"), (4, "s-q")]);
        assert!(bridge.refresh_sessions().is_ok());
        let (_broker, _server) = on_a_broker(&lane, &mut bridge, seam.fenced);
        let lane_of = format!("/f/f/in/{0}/s-r/{0}/note", bridge.node);
        let inbox = format!("/f/f/in/{}/>", bridge.node);
        if seam.indexed {
            bridge.indexed_subjects.insert(lane_of.clone());
        }
        if seam.pinned {
            let pin = PublishPin {
                seq: bridge.next_seq().unwrap(),
                via_key: false,
                identity: None,
                original: None,
                original_off: None,
                owner: Some(1),
                at: None,
            };
            bridge.state.set_post_pin("s-a", 1, &pin).unwrap();
        }
        *lane.outbox.lock().unwrap() =
            Some(seam.posts.iter().map(|(line, _)| line.as_str()).collect());
        lane.hang_up_at("outbox sent ");
        bridge.drain_outbox();
        assert!(
            bridge.aterm_gone(),
            "{name}: the first retirement met aterm gone"
        );
        let (mut reader, _) = bridge.connect().unwrap();
        let landed: Vec<String> = notes_on(&mut reader, &inbox)
            .into_iter()
            .map(|(_, text)| text)
            .collect();
        let before: Vec<String> = seam.posts[..seam.sent]
            .iter()
            .map(|(_, text)| text.clone())
            .collect();
        match landed.strip_prefix(before.as_slice()) {
            Some([]) => {}
            Some(after) => wrong.push(format!("{name}: sent after the loss {after:?}")),
            None => wrong.push(format!("{name}: on the bus {landed:?}, not {before:?}")),
        }
        // The entries left are the landed posts' (critic item E07: an own record's entry
        // stands until its post left aterm's outbox, and the `outbox sent` met aterm gone),
        // and a successor's replay reads each back where it landed: it publishes none again.
        let in_flight = bridge.state.in_flight().unwrap();
        if in_flight.len() != seam.sent {
            wrong.push(format!(
                "{name}: {} in-flight entries left, not the {} landed posts'",
                in_flight.len(),
                seam.sent
            ));
        }
        let replayed = bridge.settle_in_flight();
        let (mut reader, _) = bridge.connect().unwrap();
        let after: Vec<String> = notes_on(&mut reader, &inbox)
            .into_iter()
            .map(|(_, text)| text)
            .collect();
        if !replayed || after != landed {
            wrong.push(format!(
                "{name}: the replay (settled={replayed}) left {after:?} on the bus, not \
                 {landed:?}"
            ));
        }
    }
    assert!(
        wrong.is_empty(),
        "posts went out after aterm was gone: {wrong:#?}"
    );
}

/// **A POST ITS SEND CONCLUDES UNSENT HAS NO IN-FLIGHT ENTRY, WHATEVER BECAME OF THE LINK**
/// (the orphan of `fabric_world`'s `an_orphaned_bridge_mid_drain_…`, its review): `s-b`'s 64
/// posts fill the window behind the gap fence, their answers unread, and `s-a`'s post's send
/// waits for one. The broker link is cut meanwhile, so the wait reads the cut, concludes
/// `b1` with its fate unknown and drops the link; and aterm goes in the same moment, as its
/// push lane can say at any time, or stays. `a1` is not sent: with aterm gone the send's look
/// at aterm stops it ([`Bridge::unsendable`]) and the bridge exits, and the next attach on
/// its state dir is its successor's; with aterm there the lost link stops it, and that attach
/// is this bridge's own. The attach's replay ([`Bridge::settle_in_flight`]) finds `s-b`'s 64
/// on the bus and publishes no copy, and finds no entry for `a1`, which no connection carried
/// ([`Unsent`]): the successor never sends it, and the live bridge's drain sends it once at
/// its pin.
#[cfg(unix)]
#[test]
fn a_post_its_send_concludes_unsent_has_no_in_flight_entry_whatever_became_of_the_link() {
    let sessions = [(1, "s-a"), (2, "s-b"), (3, "s-r")];
    let window = u64::try_from(PIPELINE_WINDOW).unwrap();
    let note = |sid: &str, id: u64| queued_note(sid, id, "@s-r", &format!("{}{id}", &sid[2..]));
    let window_full: String = (1..=window).map(|id| note("s-b", id)).collect();
    let sent: Vec<String> = (1..=window).map(|id| format!("b{id}")).collect();
    // A link as the attach opens one, the fence asked for; its closer cuts it.
    let link = |bridge: &mut Bridge| {
        let (conn, cut) = bridge.connect().unwrap();
        bridge.conn = Some(conn.into());
        bridge
            .conn
            .as_mut()
            .unwrap()
            .ready()
            .unwrap()
            .hello()
            .unwrap();
        assert!(bridge.ask_gap_fences() && bridge.pipelines_posts());
        cut
    };
    for aterm_goes in [true, false] {
        let (lane, mut bridge) = FakeVerbLane::bridge(&sessions);
        assert!(bridge.refresh_sessions().is_ok());
        let (_broker, _server) = on_a_broker(&lane, &mut bridge, true);
        let cut = link(&mut bridge);
        let lane_of = format!("/f/f/in/{0}/s-r/{0}/note", bridge.node);
        bridge.indexed_subjects.insert(lane_of.clone());
        *lane.outbox.lock().unwrap() = Some(window_full.clone());
        // As the loop runs it: an answer is read when the loop reads it, not after the drain.
        bridge.in_round = true;
        bridge.drain_outbox();
        assert_eq!(
            bridge.conn.as_ref().map(Link::unacked),
            Some(PIPELINE_WINDOW),
            "aterm_goes={aterm_goes}: the window is full, no answer read"
        );
        cut.close();
        if aterm_goes {
            bridge.mailbox.push_aterm_closed();
        }
        let a1 = parse_drain(note("s-a", 1).as_bytes()).posts.remove(0);
        bridge.drain_post(a1);
        assert!(
            bridge.conn.is_none(),
            "aterm_goes={aterm_goes}: a1's wait read the cut"
        );
        let journaled: Vec<String> = bridge
            .state
            .in_flight()
            .unwrap()
            .into_iter()
            .map(|(_, entry)| Body::decode(&entry.body).0.text)
            .collect();
        assert_eq!(
            journaled, sent,
            "aterm_goes={aterm_goes}: an entry stands for each post sent and none other"
        );

        let state = bridge.state.root().to_path_buf();
        let broker = bridge.cfg.broker.clone();
        let (_successor_lane, mut next) = if aterm_goes {
            bridge.close_subscriptions();
            drop(bridge);
            let (successor_lane, mut successor) =
                FakeVerbLane::bridge_with_state(&sessions, Some(&state));
            successor.cfg.broker = broker;
            (Some(successor_lane), successor)
        } else {
            bridge.in_round = false;
            (None, bridge)
        };
        let _cut = link(&mut next);
        assert!(
            next.settle_in_flight(),
            "aterm_goes={aterm_goes}: the attach's replay"
        );
        let (mut reader, _) = next.connect().unwrap();
        let on_bus = |reader: &mut Conn| -> Vec<String> {
            notes_on(reader, &lane_of)
                .into_iter()
                .map(|(_, text)| text)
                .collect()
        };
        assert_eq!(
            on_bus(&mut reader),
            sent,
            "aterm_goes={aterm_goes}: the attach replays no copy and nothing unsent"
        );
        if !aterm_goes {
            *lane.outbox.lock().unwrap() = Some(format!("{window_full}{}", note("s-a", 1)));
            next.drain_outbox();
            next.settle_acks();
            let mut once = sent.clone();
            once.push("a1".to_string());
            assert_eq!(
                on_bus(&mut reader),
                once,
                "the live bridge's drain sends a1 once, at its pin"
            );
        }
        next.close_subscriptions();
    }
}

/// **A LATER POST WHOSE ANSWER WAS NEVER READ WAITS FOR ITS SENDER'S REFUSED ONE** (C12s1's
/// review): `a1` and `a2` of `s-a` go out together behind the gap fence and their batch fails
/// its fsync. The bridge reads `a1`'s refusal, which drops `a2`'s entry with `a1`'s, and the
/// link is lost before `a2`'s answer is read. The attach's replay then has nothing of `s-a`'s
/// to publish, and the drain sends `a1`, then `a2`, each once. Before the fix `a2`'s entry
/// outlived `a1`'s refusal and the replay landed `a2` first.
#[cfg(unix)]
#[test]
fn a_later_post_whose_answer_was_never_read_waits_for_its_senders_refused_one() {
    let (lane, mut bridge) = FakeVerbLane::bridge(&[(1, "s-a"), (2, "s-r")]);
    assert!(bridge.refresh_sessions().is_ok());
    let socket = lane.dir.path().join("unread.sock");
    let broker = astream_broker::Broker::open_opts(
        lane.dir.path().join("unread.log"),
        astream_broker::OpenOptions::new(astream_broker::Durability::Strict)
            .with_sticky_batch_refusal(true),
        || Ok(astream_broker::Prepared::Nothing),
    )
    .unwrap();
    let _server = broker.serve(&socket).unwrap();
    bridge.cfg.broker = socket.to_string_lossy().into_owned();
    let attach = |bridge: &mut Bridge| {
        bridge.conn = Some(bridge.connect().unwrap().0.into());
        bridge
            .conn
            .as_mut()
            .unwrap()
            .ready()
            .unwrap()
            .hello()
            .unwrap();
        assert!(bridge.ask_gap_fences());
    };
    attach(&mut bridge);
    assert!(bridge.pipelines_posts());
    let lane_of = format!("/f/f/in/{0}/s-r/{0}/note", bridge.node);
    bridge.indexed_subjects.insert(lane_of.clone());
    let posts = format!(
        "{}{}",
        queued_note("s-a", 1, "@s-r", "a1"),
        queued_note("s-a", 2, "@s-r", "a2")
    );
    *lane.outbox.lock().unwrap() = Some(posts.clone());
    broker.inject_faults(astream_broker::InjectedFaults {
        write: false,
        sync: true,
        truncate: false,
    });
    // As the loop runs it: an answer is read when the loop reads it, not after the drain.
    bridge.in_round = true;
    bridge.drain_outbox();
    assert_eq!(
        bridge.state.in_flight().unwrap().len(),
        2,
        "a1 and a2 are in flight together"
    );
    assert!(bridge.settle_one(bridge.fx.now()), "a1's refusal is read");
    assert!(bridge.held_senders.contains("s-a"));
    assert!(
        bridge.state.in_flight().unwrap().is_empty(),
        "a2's entry left the journal with a1's"
    );

    // The link lost, as a kill loses it, with a2's answer unread; the next attach starts
    // with no sender held and a healthy disk.
    bridge.conn = None;
    bridge.in_round = false;
    bridge.held_senders.clear();
    broker.inject_faults(astream_broker::InjectedFaults::default());
    attach(&mut bridge);
    assert!(bridge.settle_in_flight());
    *lane.outbox.lock().unwrap() = Some(posts);
    bridge.drain_outbox();
    let (mut reader, _) = bridge.connect().unwrap();
    let texts: Vec<String> = notes_on(&mut reader, &lane_of)
        .into_iter()
        .map(|(_, text)| text)
        .collect();
    assert_eq!(texts, ["a1", "a2"], "s-a's notes once each, in its order");
}

/// **AN `ev` A GAP FENCE REFUSED IS SENT AGAIN** (C12s1's review): `first`'s batch fails its
/// fsync, and `second`, written behind it once the disk is healthy again, is refused only
/// for the fence (`gap-fenced=`). The next round lifts the fence and sends `second` again,
/// so the node's `ev` face holds it, as it would on a broker that does not gap-fence;
/// `first`, refused for its own sake, is lost as it always was.
#[cfg(unix)]
#[test]
fn an_ev_a_gap_fence_refused_is_sent_again_once_the_fence_is_lifted() {
    let (lane, mut bridge) = FakeVerbLane::bridge(&[]);
    let socket = lane.dir.path().join("evs.sock");
    let broker = astream_broker::Broker::open_opts(
        lane.dir.path().join("evs.log"),
        astream_broker::OpenOptions::new(astream_broker::Durability::Strict)
            .with_sticky_batch_refusal(true),
        || Ok(astream_broker::Prepared::Nothing),
    )
    .unwrap();
    let _server = broker.serve(&socket).unwrap();
    bridge.cfg.broker = socket.to_string_lossy().into_owned();
    bridge.conn = Some(bridge.connect().unwrap().0.into());
    bridge
        .conn
        .as_mut()
        .unwrap()
        .ready()
        .unwrap()
        .hello()
        .unwrap();
    assert!(bridge.ask_gap_fences());
    // As the loop runs it: an answer is read when the loop reads it, not at each write.
    bridge.in_round = true;

    broker.inject_faults(astream_broker::InjectedFaults {
        write: false,
        sync: true,
        truncate: false,
    });
    let held = broker.pause_next_acks();
    bridge.publish_ev("first");
    assert!(held.wait(), "first's batch was tried and held unanswered");
    broker.inject_faults(astream_broker::InjectedFaults::default());
    bridge.publish_ev("second");
    held.resume();
    bridge.settle_acks();
    assert_eq!(
        bridge.evs_fenced,
        [(None, "second".to_string())],
        "second was refused for the fence alone"
    );
    assert!(node_evs(&bridge).is_empty(), "nothing landed behind first");

    bridge.settle_owed();
    bridge.settle_acks();
    assert!(bridge.evs_fenced.is_empty());
    let evs = node_evs(&bridge);
    assert_eq!(evs.len(), 1, "{evs:?}");
    assert!(evs[0].ends_with("ev=second"), "{evs:?}");
}

/// **A SENDER'S SECOND POST IN THE SAME WORDS LANDS THOUGH THE FIRST LANDED JUST AHEAD OF
/// IT** (C12s1's review, `Bridge::settle_same_words`): `s-a` posts `ok` twice behind the gap
/// fence, and between them goes `s-b`'s post, which the broker refuses for its bytes (past
/// the byte rate's whole burst, so at every try), so the second `ok` is refused behind it
/// and retried at its pin. A retry reads back from its pin's head first, and takes the first record in its words above that head for its own: had the
/// second pin been made while the first `ok` was still owed, its head would lie below the
/// first, and the retry would settle on the first's record and never publish. Each lands,
/// and each is retired at its own record.
///
/// NEGATIVE CONTROL: with the pin made ahead of the first `ok`'s answer
/// ([`crate::sim::world::Mutant::PinAheadOfSameWords`]), the retry takes the first `ok`'s
/// record for its own: one `ok` on the bus, both posts retired at it, the second lost.
#[cfg(unix)]
#[test]
fn a_senders_second_post_in_the_same_words_lands_after_the_first() {
    let SameWordsOutcome { notes, retired } =
        two_posts_in_the_same_words_behind_a_refusal(crate::fx::Fx::os());
    let texts: Vec<&str> = notes.iter().map(|(_, text)| text.as_str()).collect();
    assert_eq!(texts, ["ok", "ok"], "each `ok` once: {notes:?}");
    for (n, (at, _)) in notes.iter().enumerate() {
        let id = u64::try_from(n).unwrap() + 1;
        assert!(
            retired.contains(&("s-a".to_string(), id, Some(*at))),
            "s-a {id} retired at @{at}: {retired:#?}"
        );
    }

    let SameWordsOutcome { notes, retired } = two_posts_in_the_same_words_behind_a_refusal(
        crate::fx::Fx::os_without(crate::sim::world::Mutant::PinAheadOfSameWords),
    );
    let [(first, text)] = notes.as_slice() else {
        panic!("NEGATIVE CONTROL: one `ok` on the bus, the second lost: {notes:?}");
    };
    assert_eq!(text, "ok");
    for id in [1, 2] {
        assert!(
            retired.contains(&("s-a".to_string(), id, Some(*first))),
            "NEGATIVE CONTROL: s-a {id} retired at the first's @{first}: {retired:#?}"
        );
    }
}

#[cfg(unix)]
struct SameWordsOutcome {
    notes: Vec<(u64, String)>,
    retired: Retired,
}

/// The scenario of [`a_senders_second_post_in_the_same_words_lands_after_the_first`] under
/// `fx`: the notes on `s-r`'s lane, by offset, and every retirement aterm was told.
#[cfg(unix)]
fn two_posts_in_the_same_words_behind_a_refusal(fx: crate::fx::Fx) -> SameWordsOutcome {
    let (lane, mut bridge) = FakeVerbLane::bridge(&[(1, "s-a"), (2, "s-b"), (3, "s-r")]);
    bridge.fx = fx;
    assert!(bridge.refresh_sessions().is_ok());
    let socket = lane.dir.path().join("words.sock");
    let broker = astream_broker::Broker::open_opts(
        lane.dir.path().join("words.log"),
        astream_broker::OpenOptions::new(astream_broker::Durability::Strict)
            .with_sticky_batch_refusal(true)
            .with_rate_limit(
                astream_broker::RateLimit::records(1000, 1000)
                    .and_then(|limit| limit.with_bytes(1, 4096))
                    .unwrap(),
            ),
        || Ok(astream_broker::Prepared::Nothing),
    )
    .unwrap();
    let _server = broker.serve(&socket).unwrap();
    bridge.cfg.broker = socket.to_string_lossy().into_owned();
    bridge.conn = Some(bridge.connect().unwrap().0.into());
    bridge
        .conn
        .as_mut()
        .unwrap()
        .ready()
        .unwrap()
        .hello()
        .unwrap();
    assert!(bridge.ask_gap_fences());
    let lane_of = format!("/f/f/in/{0}/s-r/{0}/note", bridge.node);
    let other = format!("/f/f/in/{0}/s-r/{0}/other", bridge.node);
    bridge.indexed_subjects.insert(lane_of.clone());
    bridge.indexed_subjects.insert(other);
    let big = "z".repeat(16 * 1024);
    let second = queued_note("s-a", 2, "@s-r", "ok");
    *lane.outbox.lock().unwrap() = Some(format!(
        "{}post sid=s-b id=1 to=@s-r kind=other len={}\n{big}{second}",
        queued_note("s-a", 1, "@s-r", "ok"),
        big.len(),
    ));
    bridge.drain_outbox();
    bridge.settle_acks();
    assert!(
        bridge.state.post_pin("s-a", 2).unwrap().is_some(),
        "PRECONDITION: the second `ok` was refused behind s-b's and keeps its pin"
    );

    broker.set_rate_limit(None).unwrap();
    // And the waits its refusals named are over ([`Bridge::rate`]): a byte rate of one a
    // second names hours.
    bridge.rate = crate::rate_hold::RateHold::default();
    *lane.outbox.lock().unwrap() = Some(second);
    bridge.drain_outbox();
    bridge.settle_acks();
    let (mut reader, _) = bridge.connect().unwrap();
    SameWordsOutcome {
        notes: notes_on(&mut reader, &lane_of),
        retired: retirements(&lane),
    }
}

/// **THE ATTACH'S REPLAY HOLDS THE FENCES A REFUSED ENTRY SET** (C12s1's review,
/// `Bridge::fences_held`): `s-a`'s `a1` and `a2` go out together behind the gap fence, and
/// the link is lost before either answer is read, so both entries stand. The broker refuses
/// `a1` for its bytes (past the byte rate's whole burst, so at every try: a wait no hold
/// covers), at its first attempt and again at the attach's replay, which drops its entry and
/// keeps the post at its pin; the fence that refusal set is held through the replay, so
/// `a2`'s entry, sent once the replay has waited out the hold the refusal set
/// ([`Bridge::rate`]), is refused behind it and dropped too. Once the byte rate is lifted the
/// drain sends `a1`, then `a2`, each at its pin: the log holds them in `s-a`'s order.
///
/// NEGATIVE CONTROL: with the fences lifted mid-replay
/// ([`crate::sim::world::Mutant::LiftFencesDuringReplay`]), and the refusal's wait not held
/// ([`crate::sim::world::Mutant::RetryRateLimitedAtOnce`]), as a refusal that names none
/// leaves it, the replay's publish of `a2` lifts `a1`'s fence and lands, and `a1`'s retry lands
/// after it. The seeded worlds meet no
/// kill between two answers to one sender's posts whose first is refused again at the
/// replay, so this rule's control is here.
#[cfg(unix)]
#[test]
fn the_attachs_replay_holds_the_fences_a_refused_entry_set() {
    let (replayed, drained) = replay_of_a_refused_entry_and_the_next(crate::fx::Fx::os());
    assert!(
        replayed.is_empty(),
        "nothing of s-a's landed at the replay: {replayed:?}"
    );
    assert_eq!(drained, ["a1", "a2"], "s-a's posts once each, in its order");

    let (replayed, drained) =
        replay_of_a_refused_entry_and_the_next(crate::fx::Fx::os_without_each(&[
            crate::sim::world::Mutant::LiftFencesDuringReplay,
            crate::sim::world::Mutant::RetryRateLimitedAtOnce,
        ]));
    assert_eq!(
        replayed,
        ["a2"],
        "NEGATIVE CONTROL: the replay lifted a1's fence and landed a2"
    );
    assert_eq!(
        drained,
        ["a2", "a1"],
        "NEGATIVE CONTROL: a1's retry landed behind a2"
    );
}

/// The scenario of [`the_attachs_replay_holds_the_fences_a_refused_entry_set`] under `fx`:
/// the posts of `s-a`'s on `s-r`'s lane, by offset, after the attach's replay, and after
/// the drain that follows it.
#[cfg(unix)]
fn replay_of_a_refused_entry_and_the_next(fx: crate::fx::Fx) -> (Vec<String>, Vec<String>) {
    let (lane, mut bridge) = FakeVerbLane::bridge(&[(1, "s-a"), (2, "s-r")]);
    bridge.fx = fx;
    assert!(bridge.refresh_sessions().is_ok());
    let socket = lane.dir.path().join("replay.sock");
    let broker = astream_broker::Broker::open_opts(
        lane.dir.path().join("replay.log"),
        astream_broker::OpenOptions::new(astream_broker::Durability::Strict)
            .with_sticky_batch_refusal(true)
            .with_rate_limit(
                astream_broker::RateLimit::records(1000, 1000)
                    .and_then(|limit| limit.with_bytes(1, 4096))
                    .unwrap(),
            ),
        || Ok(astream_broker::Prepared::Nothing),
    )
    .unwrap();
    let _server = broker.serve(&socket).unwrap();
    bridge.cfg.broker = socket.to_string_lossy().into_owned();
    let attach = |bridge: &mut Bridge| {
        bridge.conn = Some(bridge.connect().unwrap().0.into());
        bridge
            .conn
            .as_mut()
            .unwrap()
            .ready()
            .unwrap()
            .hello()
            .unwrap();
        assert!(bridge.ask_gap_fences());
    };
    attach(&mut bridge);
    assert!(bridge.pipelines_posts());
    let lane_of = format!("/f/f/in/{0}/s-r/{0}/note", bridge.node);
    bridge.indexed_subjects.insert(lane_of.clone());
    let a1 = format!("a1{}", "z".repeat(16 * 1024));
    let posts = format!(
        "{}{}",
        queued_note("s-a", 1, "@s-r", &a1),
        queued_note("s-a", 2, "@s-r", "a2")
    );
    *lane.outbox.lock().unwrap() = Some(posts.clone());
    // As the loop runs it: an answer is read when the loop reads it, not after the drain.
    bridge.in_round = true;
    bridge.drain_outbox();
    assert_eq!(
        bridge.state.in_flight().unwrap().len(),
        2,
        "PRECONDITION: a1 and a2 are in flight together"
    );

    // The link lost, as a kill loses it, with both answers unread: both entries stand.
    bridge.conn = None;
    bridge.in_round = false;
    attach(&mut bridge);
    assert!(bridge.settle_in_flight());
    let (mut reader, _) = bridge.connect().unwrap();
    let posted = |reader: &mut Conn| -> Vec<String> {
        notes_on(reader, &lane_of)
            .into_iter()
            .map(|(_, text)| text.chars().take(2).collect())
            .collect()
    };
    let replayed = posted(&mut reader);

    broker.set_rate_limit(None).unwrap();
    // And the waits its refusals named are over ([`Bridge::rate`]): a byte rate of one a
    // second names hours.
    bridge.rate = crate::rate_hold::RateHold::default();
    *lane.outbox.lock().unwrap() = Some(posts);
    bridge.drain_outbox();
    bridge.settle_acks();
    (replayed, posted(&mut reader))
}

/// **A POST WITH AN ENTRY WAITS FOR THE ANSWER TO ITS SENDER'S POST WITH NONE**
/// (`Bridge::settle_in_flight`'s 4): `s-a`'s `a1`, to a person's lane (never journaled), and
/// `a2`, to `s-r` on this node (journaled), come in one listing. The broker refuses `a1` for
/// its bytes, and `a2` is not sent: no entry stands, so the attach after a lost link
/// publishes nothing, and the drain sends `a1`, then `a2`, in `s-a`'s order.
///
/// NEGATIVE CONTROL ([`crate::sim::world::Mutant::EntryAheadOfAPostWithNone`]): `a2` goes out
/// behind `a1` and is fenced, its answer lost with the link, so its entry stands; the replay
/// lands it, and `a1`'s retry lands behind it, as the sim's found row for that mutant met.
#[cfg(unix)]
#[test]
fn a_post_with_an_entry_waits_for_the_answer_to_its_senders_post_with_none() {
    let (entries, replayed, drained) = behind_a_post_with_no_entry(crate::fx::Fx::os());
    assert_eq!(entries, 0, "a2 was not sent behind a1's refusal");
    assert!(replayed.is_empty(), "nothing to replay: {replayed:?}");
    assert_eq!(drained, ["a1", "a2"], "s-a's posts once each, in its order");

    let (entries, replayed, drained) = behind_a_post_with_no_entry(crate::fx::Fx::os_without(
        crate::sim::world::Mutant::EntryAheadOfAPostWithNone,
    ));
    assert_eq!(entries, 1, "NEGATIVE CONTROL: a2's entry stands");
    assert_eq!(replayed, ["a2"], "NEGATIVE CONTROL: the replay landed a2");
    assert_eq!(
        drained,
        ["a2", "a1"],
        "NEGATIVE CONTROL: a1's retry landed behind a2"
    );
}

/// What [`a_post_with_an_entry_waits_for_the_answer_to_its_senders_post_with_none`] reads
/// under `fx`: the in-flight entries the link's loss leaves, then `s-a`'s posts by offset
/// over both lanes, after the attach's replay and after the drain that follows it.
#[cfg(unix)]
fn behind_a_post_with_no_entry(fx: crate::fx::Fx) -> (usize, Vec<String>, Vec<String>) {
    let (lane, mut bridge) = FakeVerbLane::bridge(&[(1, "s-a"), (2, "s-r")]);
    bridge.fx = fx;
    assert!(bridge.refresh_sessions().is_ok());
    let socket = lane.dir.path().join("replay.sock");
    let broker = astream_broker::Broker::open_opts(
        lane.dir.path().join("replay.log"),
        astream_broker::OpenOptions::new(astream_broker::Durability::Strict)
            .with_sticky_batch_refusal(true)
            .with_rate_limit(
                astream_broker::RateLimit::records(1000, 1000)
                    .and_then(|limit| limit.with_bytes(1, 4096))
                    .unwrap(),
            ),
        || Ok(astream_broker::Prepared::Nothing),
    )
    .unwrap();
    let _server = broker.serve(&socket).unwrap();
    bridge.cfg.broker = socket.to_string_lossy().into_owned();
    let attach = |bridge: &mut Bridge| {
        bridge.conn = Some(bridge.connect().unwrap().0.into());
        bridge
            .conn
            .as_mut()
            .unwrap()
            .ready()
            .unwrap()
            .hello()
            .unwrap();
        assert!(bridge.ask_gap_fences());
    };
    attach(&mut bridge);
    assert!(bridge.pipelines_posts());
    let own = format!("/f/f/in/{0}/s-r/{0}/note", bridge.node);
    let person = format!("/f/f/in/p/h-alice/{}/note", bridge.node);
    bridge.indexed_subjects.insert(own.clone());
    bridge.indexed_subjects.insert(person.clone());
    let a1 = format!("a1{}", "z".repeat(16 * 1024));
    let posts = format!(
        "{}{}",
        queued_note("s-a", 1, "h-alice", &a1),
        queued_note("s-a", 2, "@s-r", "a2")
    );
    *lane.outbox.lock().unwrap() = Some(posts.clone());
    // As the loop runs it: an answer is read when the loop reads it, not after the drain.
    bridge.in_round = true;
    bridge.drain_outbox();
    let entries = bridge.state.in_flight().unwrap().len();

    // The link lost, as a kill loses it, with whatever answers are owed unread.
    bridge.conn = None;
    bridge.in_round = false;
    attach(&mut bridge);
    assert!(bridge.settle_in_flight());
    let (mut reader, _) = bridge.connect().unwrap();
    let posted = |reader: &mut Conn| -> Vec<String> {
        let mut rows = notes_on(reader, &person);
        rows.extend(notes_on(reader, &own));
        rows.sort_unstable();
        rows.into_iter()
            .map(|(_, text)| text.chars().take(2).collect())
            .collect()
    };
    let replayed = posted(&mut reader);

    // The limit is gone, and with it what `a1`'s hold waits for: at this rate, hours.
    broker.set_rate_limit(None).unwrap();
    bridge
        .rate
        .expire(bridge.fx.now() + Duration::from_secs(24 * 60 * 60));
    *lane.outbox.lock().unwrap() = Some(posts);
    bridge.drain_outbox();
    bridge.settle_acks();
    (entries, replayed, posted(&mut reader))
}

/// **ONLY THE POST WITH AN ENTRY BEHIND ONE WITH NONE WAITS** (`Bridge::settle_unjournaled_ahead`):
/// of `s-a`'s `a1` to a person, `a2` and `a3` to `s-r` on this node, and `a4` to the person
/// again, one listing reads `a1`'s answer before `a2` goes and nothing more: `a2`, `a3` and
/// `a4` go on in flight together, and so does another sender's `b1`, listed behind `a1`.
#[cfg(unix)]
#[test]
fn only_the_post_with_an_entry_behind_one_with_none_waits() {
    let (lane, mut bridge) = FakeVerbLane::bridge(&[(1, "s-a"), (2, "s-b"), (3, "s-r")]);
    assert!(bridge.refresh_sessions().is_ok());
    let socket = lane.dir.path().join("one-wait.sock");
    let broker = astream_broker::Broker::open_opts(
        lane.dir.path().join("one-wait.log"),
        astream_broker::OpenOptions::new(astream_broker::Durability::Strict)
            .with_sticky_batch_refusal(true),
        || Ok(astream_broker::Prepared::Nothing),
    )
    .unwrap();
    let _server = broker.serve(&socket).unwrap();
    bridge.cfg.broker = socket.to_string_lossy().into_owned();
    bridge.conn = Some(bridge.connect().unwrap().0.into());
    bridge
        .conn
        .as_mut()
        .unwrap()
        .ready()
        .unwrap()
        .hello()
        .unwrap();
    assert!(bridge.ask_gap_fences());
    assert!(bridge.pipelines_posts());
    bridge
        .indexed_subjects
        .insert(format!("/f/f/in/{0}/s-r/{0}/note", bridge.node));
    bridge
        .indexed_subjects
        .insert(format!("/f/f/in/p/h-alice/{}/note", bridge.node));
    *lane.outbox.lock().unwrap() = Some(format!(
        "{}{}{}{}{}",
        queued_note("s-a", 1, "h-alice", "a1"),
        queued_note("s-b", 1, "@s-r", "b1"),
        queued_note("s-a", 2, "@s-r", "a2"),
        queued_note("s-a", 3, "@s-r", "a3"),
        queued_note("s-a", 4, "h-alice", "a4"),
    ));
    bridge.in_round = true;
    bridge.drain_outbox();
    let mut owed: Vec<(String, u64)> = bridge
        .conn
        .as_ref()
        .unwrap()
        .owed()
        .filter_map(Then::post)
        .map(|post| (post.sid.clone(), post.id))
        .collect();
    owed.sort_unstable();
    assert_eq!(
        owed,
        [
            ("s-a".to_string(), 2),
            ("s-a".to_string(), 3),
            ("s-a".to_string(), 4),
            ("s-b".to_string(), 1),
        ],
        "a1's answer alone was read"
    );
}

/// **A BURST THE BROKER'S RATE LIMIT REFUSES MID-WINDOW IS WAITED OUT, AND LANDS WHOLE, ONCE
/// AND IN ORDER** (critic item E06, `Bridge::rate`): `s-a` and `s-b` post three notes each in
/// one drain, pipelined behind the gap fence, onto a broker whose bucket takes two records and
/// one more a second. The limit refuses the third post mid-window, and the fence (or the empty
/// bucket) every post sent after it. The bridge holds every publish of its producer for the
/// wait the refusal names, and the refused post's sender for the whole of it; the loop, parked
/// no later than a hold's end ([`Bridge::duty_wait`]), drains again
/// ([`Bridge::after_holds`]), the refused post first at its own pin, the fence its refusal
/// set lifted ahead of it, its sender's later posts after it. That drain's posts go out
/// pipelined, so the bucket that refilled for the first refuses those behind it in the same
/// window (the log shows three refusals read together after the first hold's end), each kept
/// at its pin for the next end. Every note lands once, each sender's in its order, each retired
/// at its own record; no fence is left up and no entry in flight; and the broker refused at
/// most a post apiece and one window. A drain at the instant of the first refusals, inside
/// every hold they set, publishes nothing: the broker hears none of it.
///
/// The bridge runs on a clock the rig holds ([`crate::fx::Fx::on_a_held_clock`]), which only
/// the loop's parks move, each by the wait to a hold's end (critic item E04e): a hold cannot
/// lapse between two of the rig's steps however slowly a loaded machine runs them (C08r's
/// verify saw one lapse between the read of its acks and the loop's next look, which then
/// found no hold to end). The broker's bucket keeps the wall clock, which a park's sleep moves
/// at least as far as it moves the held clock, so the bucket has refilled for whatever a park
/// waited out.
///
/// NEGATIVE CONTROLS: a rate-limited post retired at the door
/// ([`crate::sim::world::Mutant::RateLimitedIsForGood`]) is lost; one retried at the next
/// drain with no wait ([`crate::sim::world::Mutant::RetryRateLimitedAtOnce`]) is sent again
/// by the drain at its refusal's instant. That verdict is the clock's sequence, not a count of
/// refusals, which turns on how many drains a machine runs before the bucket refills.
#[cfg(unix)]
#[test]
fn a_burst_the_rate_limit_refuses_mid_window_lands_whole_once_and_in_order() {
    let bound = RATE_POSTS + u64::try_from(PIPELINE_WINDOW).unwrap();
    let run = rate_limited_burst(crate::fx::Fx::os());
    assert!(
        run.refused_mid_window,
        "PRECONDITION: the limit refused part of the burst: {run:#?}"
    );
    assert_eq!(
        run.notes.len(),
        usize::try_from(RATE_POSTS).unwrap(),
        "each note exactly once: {run:#?}"
    );
    assert_eq!(
        run.retired.len(),
        run.notes.len(),
        "each post retired once: {run:#?}"
    );
    for sid in RATE_SENDERS {
        let mine: Vec<&str> = run
            .notes
            .iter()
            .map(|(_, text)| text.as_str())
            .filter(|text| text.starts_with(sid))
            .collect();
        let posted: Vec<String> = (1..=3).map(|n| format!("{sid}-{n}")).collect();
        assert_eq!(mine, posted, "{sid}'s notes in the order it posted them");
        for n in 1..=3u64 {
            let at = run
                .notes
                .iter()
                .find(|(_, text)| *text == format!("{sid}-{n}"))
                .map(|(off, _)| *off);
            assert!(
                run.retired.contains(&(sid.to_string(), n, at)),
                "{sid} {n} retired at its own record's offset {at:?}: {run:#?}"
            );
        }
    }
    assert!(!run.fenced, "no gap fence left up: {run:#?}");
    assert_eq!(
        run.in_flight, 0,
        "every own-lane entry went with its answer"
    );
    assert!(
        run.rate_limited <= bound,
        "at most a refusal a post and one window's: {run:#?}"
    );
    assert_eq!(
        run.heard_at_the_refusal, 0,
        "a drain inside the holds the refusals set publishes nothing: {run:#?}"
    );

    let lost = rate_limited_burst(crate::fx::Fx::os_without(
        crate::sim::world::Mutant::RateLimitedIsForGood,
    ));
    assert!(lost.refused_mid_window, "PRECONDITION: {lost:#?}");
    assert!(
        lost.retired.iter().any(|(_, _, off)| off.is_none())
            && lost.notes.len() < usize::try_from(RATE_POSTS).unwrap(),
        "NEGATIVE CONTROL: a rate-limited post retired at the door is lost: {lost:#?}"
    );

    let hot = rate_limited_burst(crate::fx::Fx::os_without(
        crate::sim::world::Mutant::RetryRateLimitedAtOnce,
    ));
    assert!(hot.refused_mid_window, "PRECONDITION: {hot:#?}");
    assert!(
        hot.heard_at_the_refusal > 0,
        "NEGATIVE CONTROL: a refusal retried with no wait is sent again by the drain at the \
         instant of its refusal: {hot:#?}"
    );
    assert!(
        hot.rate_limited <= 2 * RATE_POSTS,
        "the control's run lifts the limit once its verdict is read: at most a refusal a \
         post in each of the two drains before it, and none spinning on the empty bucket \
         after it: {hot:#?}"
    );
}

#[cfg(unix)]
const RATE_SENDERS: [&str; 2] = ["s-a", "s-b"];

#[cfg(unix)]
const RATE_POSTS: u64 = 6;

/// What [`rate_limited_burst`] found.
#[cfg(unix)]
#[derive(Debug)]
struct RateLimitedRun {
    /// The notes on `s-r`'s lane, by offset.
    notes: Vec<(u64, String)>,
    retired: Retired,
    /// The broker refused part of the first drain's burst for the rate.
    refused_mid_window: bool,
    /// Every publish the broker's limit refused (`BrokerStats::rate_limited`).
    rate_limited: u64,
    /// The publishes the broker heard from a drain at the instant the first drain's
    /// refusals were read, on the bridge's held clock, inside every hold they set: each
    /// refused again for the rate (`BrokerStats::rate_limited`) or appended (`LogStats::head`).
    heard_at_the_refusal: u64,
    /// The link holds a fence a refusal set ([`Link::fenced`]).
    fenced: bool,
    in_flight: usize,
}

/// The scenario of [`a_burst_the_rate_limit_refuses_mid_window_lands_whole_once_and_in_order`]
/// under `fx` with its clock held ([`crate::fx::Fx::on_a_held_clock`]): the burst's drain,
/// then a drain at the instant its refusals were read, as a wake inside the holds they set
/// brings one, then the run as the bridge's loop runs it until every post is retired (a
/// one-minute hang detector): parked for its duty wait, which ends no later than the next
/// hold's ([`Bridge::duty_wait`]) and moves the held clock by itself ([`Fx::sleep`]), then the
/// drain a hold's end makes ([`Bridge::after_holds`], as [`Bridge::pass`] runs it). With
/// the hold switched off ([`crate::sim::world::Mutant::RetryRateLimitedAtOnce`]) nothing is
/// held: the run lifts the broker's limit once the drain at the refusals' instant is read, and
/// each turn is the next drain any wake brings; otherwise a post left unretired with no hold to
/// end would wait for a wake nothing sends, and the run says so.
#[cfg(unix)]
fn rate_limited_burst(fx: crate::fx::Fx) -> RateLimitedRun {
    let (lane, mut bridge) = FakeVerbLane::bridge(&[(1, "s-a"), (2, "s-b"), (3, "s-r")]);
    bridge.fx = fx.on_a_held_clock();
    // The roster's refresh, the loop's one other duty here, falls past the run.
    bridge.roster_due = bridge.fx.now() + Duration::from_secs(3600);
    lane.retire_from_outbox();
    assert!(bridge.refresh_sessions().is_ok());
    let socket = lane.dir.path().join("rate.sock");
    let broker = astream_broker::Broker::open_opts(
        lane.dir.path().join("rate.log"),
        astream_broker::OpenOptions::new(astream_broker::Durability::Strict)
            .with_sticky_batch_refusal(true)
            .with_rate_limit(astream_broker::RateLimit::records(1, 2).unwrap()),
        || Ok(astream_broker::Prepared::Nothing),
    )
    .unwrap();
    let _server = broker.serve(&socket).unwrap();
    bridge.cfg.broker = socket.to_string_lossy().into_owned();
    bridge.conn = Some(bridge.connect().unwrap().0.into());
    bridge
        .conn
        .as_mut()
        .unwrap()
        .ready()
        .unwrap()
        .hello()
        .unwrap();
    assert!(bridge.ask_gap_fences());
    assert!(bridge.pipelines_posts());
    let lane_of = format!("/f/f/in/{0}/s-r/{0}/note", bridge.node);
    bridge.indexed_subjects.insert(lane_of.clone());
    let mut frame = String::new();
    for n in 1..=3u64 {
        for sid in RATE_SENDERS {
            frame.push_str(&queued_note(sid, n, "@s-r", &format!("{sid}-{n}")));
        }
    }
    *lane.outbox.lock().unwrap() = Some(frame);
    bridge.drain_outbox();
    bridge.settle_acks();
    let refused_mid_window = broker.stats().rate_limited > 0;

    // A DRAIN AT THE REFUSALS' INSTANT, as a wake inside their holds brings one: what the
    // broker hears of it, each publish refused again for the rate or appended.
    let heard = |broker: &astream_broker::Broker| {
        let stats = broker.stats();
        stats.rate_limited + stats.log.head.0
    };
    let before = heard(&broker);
    bridge.drain_outbox();
    bridge.settle_acks();
    let heard_at_the_refusal = heard(&broker) - before;
    // The no-wait control's verdict is read. With nothing held its loop would drain at once
    // again and again until the broker's bucket refilled, each refusal a line on stderr,
    // so its run lifts the limit here and its next drain lands the rest.
    if bridge
        .fx
        .mutant(crate::sim::world::Mutant::RetryRateLimitedAtOnce)
    {
        broker.set_rate_limit(None).unwrap();
    }

    let posts = usize::try_from(RATE_POSTS).unwrap();
    let deadline = Instant::now() + aterm_spec::wait::MUST_HAPPEN;
    while retirements(&lane).len() < posts {
        assert!(
            Instant::now() < deadline,
            "HANG: the burst was not retired whole in a minute: {:#?}",
            retirements(&lane)
        );
        let now = bridge.fx.now();
        match bridge.rate.next_end(now) {
            Some(end) => {
                let wait = bridge.duty_wait(now);
                assert!(
                    wait <= end.saturating_duration_since(now),
                    "the loop parks no later than a hold's end: {wait:?}"
                );
                bridge.fx.sleep(wait);
                bridge.after_holds();
            }
            None if bridge
                .fx
                .mutant(crate::sim::world::Mutant::RetryRateLimitedAtOnce) =>
            {
                bridge.drain_outbox();
            }
            None => panic!(
                "a post stands unretired with no hold to end, and no drain is owed it: {:#?}",
                retirements(&lane)
            ),
        }
        bridge.settle_acks();
    }
    let (mut reader, _) = bridge.connect().unwrap();
    RateLimitedRun {
        notes: notes_on(&mut reader, &lane_of),
        retired: retirements(&lane),
        refused_mid_window,
        rate_limited: broker.stats().rate_limited,
        heard_at_the_refusal,
        fenced: bridge.conn.as_ref().is_some_and(Link::fenced),
        in_flight: bridge.state.in_flight().unwrap().len(),
    }
}

/// A broker on `lane`'s scratch dir, under `limit`, sticky batch refusal on, served on a
/// socket of `name`, and `bridge` attached to it with the gap fences asked.
#[cfg(unix)]
fn rate_limited_broker(
    lane: &FakeVerbLane,
    bridge: &mut Bridge,
    name: &str,
    limit: astream_broker::RateLimit,
) -> (astream_broker::Broker, astream_broker::BrokerHandle) {
    let socket = lane.dir.path().join(format!("{name}.sock"));
    let broker = astream_broker::Broker::open_opts(
        lane.dir.path().join(format!("{name}.log")),
        astream_broker::OpenOptions::new(astream_broker::Durability::Strict)
            .with_sticky_batch_refusal(true)
            .with_rate_limit(limit),
        || Ok(astream_broker::Prepared::Nothing),
    )
    .unwrap();
    let server = broker.serve(&socket).unwrap();
    bridge.cfg.broker = socket.to_string_lossy().into_owned();
    bridge.conn = Some(bridge.connect().unwrap().0.into());
    bridge
        .conn
        .as_mut()
        .unwrap()
        .ready()
        .unwrap()
        .hello()
        .unwrap();
    assert!(bridge.ask_gap_fences());
    (broker, server)
}

/// Another write of `bridge`'s producer spends `bytes` of its bucket, a record each of
/// `records` (a burst of the bridge's own: posts, presence, `ev`, the mirror's slots).
#[cfg(unix)]
fn spend_the_bucket(bridge: &Bridge, records: u64, bytes: usize) {
    let (mut other, _) = bridge.connect().unwrap();
    let body = vec![b'x'; bytes];
    for seq in 1..=records {
        other
            .publish(
                bridge.producer_id,
                (1u64 << 40) + seq,
                "/f/f/pub/n-x/s-x/ev",
                &body,
            )
            .unwrap();
    }
}

/// **AN `undeliverable` NOTICE THE RATE LIMIT HELD BACK IS SENT ONCE THE HOLD IS OVER, AND
/// THE INBOX COMMIT WAITS FOR IT** (critic item E06's review, `Bridge::notices_held`): another
/// write of the bridge's producer has spent its bucket (two records, one more a second), and
/// an inbound note @23 for `s-other` is answered `ERR quota`. The bridge's `undeliverable` ev
/// for it is refused for the rate, which holds every publish of the producer, so the notice
/// to the remote sender is held back unsent. It is kept, the inbox commit stays below @23
/// while it is, and once the hold is over the round's settle sends it at its own sequence:
/// the sender is told once, and only then does the commit pass @23.
///
/// NEGATIVE CONTROL: with a held notice dropped
/// ([`crate::sim::world::Mutant::NoticeHeldIsDropped`]), as every notice whose publish failed
/// was before, the commit passes @23 and the sender is never told.
#[cfg(unix)]
#[test]
fn an_undeliverable_notice_the_rate_limit_held_is_sent_once_the_hold_is_over() {
    let told = notice_under_a_rate_hold(crate::fx::Fx::os());
    assert_eq!(
        told,
        [Some(23)],
        "the sender is told once that @23 was not delivered"
    );

    let told = notice_under_a_rate_hold(crate::fx::Fx::os_without(
        crate::sim::world::Mutant::NoticeHeldIsDropped,
    ));
    assert!(
        told.is_empty(),
        "NEGATIVE CONTROL: the held notice is lost: {told:?}"
    );
}

/// The scenario of
/// [`an_undeliverable_notice_the_rate_limit_held_is_sent_once_the_hold_is_over`] under `fx`,
/// run until the inbox commit passes @23 (a one-minute hang detector), each turn as the loop's:
/// parked to a hold's end, its drain ([`Bridge::after_holds`]), the round's settle
/// ([`Bridge::settle_owed`]). The `re=` of each notice on the sender's lane.
#[cfg(unix)]
fn notice_under_a_rate_hold(fx: crate::fx::Fx) -> Vec<Option<u64>> {
    const OTHER: &str = "s-other";
    const AT: u64 = 23;
    let (lane, mut bridge) = FakeVerbLane::bridge(&[(2, OTHER)]);
    bridge.fx = fx;
    let (broker, _server) = rate_limited_broker(
        &lane,
        &mut bridge,
        "notice",
        astream_broker::RateLimit::records(1, 2).unwrap(),
    );
    assert!(bridge.refresh_sessions().is_ok());
    bridge.per_sid.pending_admit.clear();
    let node = bridge.node.clone();
    spend_the_bucket(&bridge, 2, 16);
    let rec: BrokerRecord = (
        AT,
        format!("/f/f/in/{node}/{OTHER}/n-peer/note"),
        b"v=1 t=1 from=s-peer text=hi".to_vec(),
    );
    lane.answer_deliveries_to(OTHER, "ERR quota\n");
    // aterm answers `outbox` (an empty listing): the bridge stays an inheriting one.
    *lane.outbox.lock().unwrap() = Some(String::new());
    bridge.on_inbox_record(&rec, None);
    assert_eq!(bridge.attachment, Attachment::Inherited);
    assert!(
        broker.stats().rate_limited > 0 && bridge.rate_held().is_some(),
        "PRECONDITION: the limit refused the ev and holds the producer"
    );
    let held = !bridge
        .fx
        .mutant(crate::sim::world::Mutant::NoticeHeldIsDropped);
    assert_eq!(
        bridge.notices_held.contains_key(&AT),
        held,
        "PRECONDITION: the notice was held back"
    );

    let deadline = Instant::now() + aterm_spec::wait::MUST_HAPPEN;
    while !bridge.committed_through(AT) {
        assert!(
            Instant::now() < deadline,
            "HANG: the inbox commit did not pass @{AT} in a minute (held: {:?})",
            bridge.notices_held.keys().collect::<Vec<_>>()
        );
        let now = bridge.fx.now();
        let wait = bridge
            .rate
            .next_end(now)
            .map_or(Duration::from_millis(10), |end| {
                end.saturating_duration_since(now)
            });
        std::thread::sleep(wait);
        bridge.after_holds();
        bridge.settle_owed();
        bridge.settle_acks();
        assert!(
            bridge.notices_held.is_empty() || !bridge.committed_through(AT),
            "the commit passed @{AT} while its notice is held"
        );
    }
    let (mut reader, _) = bridge.connect().unwrap();
    let notices = format!("/f/f/in/n-peer/s-peer/{node}/undeliverable");
    let (rows, _) = reader.fetch(0, &notices, 16).unwrap();
    rows.iter()
        .filter(|(_, s, _)| *s == notices)
        .map(|(_, _, b)| Body::decode(b).0.re)
        .collect()
}

/// **A POST THE BYTE RATE REFUSED WAITS THE WHOLE WAIT THE BROKER NAMED, WHILE ITS
/// PRODUCER'S OTHER POSTS GO ON** (critic item E06's review, `RateHold::hold_sender`): the
/// bridge's byte bucket (16 KiB, 1 KiB a second) is spent, and `s-a` posts a note of 15 KiB,
/// then `s-b` one of a few bytes, in one drain. The broker refuses `s-a`'s for its bytes,
/// naming a wait of about fifteen seconds, and `s-b`'s behind the fence. The producer is held
/// a second at most ([`crate::rate_hold::PRODUCER_HOLD_MAX`]); at that hold's end the drain
/// sends `s-b`'s, which the bucket has refilled for, and nothing of `s-a`'s, which is still
/// held: no second refusal, and the loop's next wake is `s-a`'s own hold's end.
///
/// NEGATIVE CONTROL: with the sender held only by the producer's hold
/// ([`crate::sim::world::Mutant::SenderWaitsOnlyTheProducerHold`]), the drain at its end sends
/// `s-a`'s post again, into a bucket that holds a second's bytes: refused again, and so after
/// every producer hold, a one-a-second loop of refusals.
#[cfg(unix)]
#[test]
fn a_post_the_byte_rate_refused_waits_its_whole_wait_while_the_others_go_on() {
    let run = byte_refused_post_and_another(crate::fx::Fx::os());
    assert_eq!(
        run.refusals_after_the_hold, 0,
        "s-a's post waits its own wait: {run:#?}"
    );
    assert_eq!(run.landed, ["small"], "s-b's post landed: {run:#?}");
    assert!(
        run.next_wake > crate::rate_hold::PRODUCER_HOLD_MAX,
        "the next wake is s-a's own hold's end: {run:#?}"
    );

    let run = byte_refused_post_and_another(crate::fx::Fx::os_without(
        crate::sim::world::Mutant::SenderWaitsOnlyTheProducerHold,
    ));
    assert!(
        run.refusals_after_the_hold > 0,
        "NEGATIVE CONTROL: s-a's post is refused again at the producer hold's end: {run:#?}"
    );
}

/// What [`byte_refused_post_and_another`] found.
#[cfg(unix)]
#[derive(Debug)]
struct ByteRefusedRun {
    /// The broker's refusals for the rate in the drain at the producer hold's end.
    refusals_after_the_hold: u64,
    /// The texts on `s-r`'s lane but the big one's.
    landed: Vec<String>,
    /// How long after that drain the next hold ends.
    next_wake: Duration,
}

/// The scenario of
/// [`a_post_the_byte_rate_refused_waits_its_whole_wait_while_the_others_go_on`] under `fx`.
#[cfg(unix)]
fn byte_refused_post_and_another(fx: crate::fx::Fx) -> ByteRefusedRun {
    const KIB: u64 = 1024;
    let (lane, mut bridge) = FakeVerbLane::bridge(&[(1, "s-a"), (2, "s-b"), (3, "s-r")]);
    bridge.fx = fx;
    bridge.roster_due = Instant::now() + Duration::from_secs(3600);
    lane.retire_from_outbox();
    assert!(bridge.refresh_sessions().is_ok());
    let (broker, _server) = rate_limited_broker(
        &lane,
        &mut bridge,
        "bytes",
        astream_broker::RateLimit::records(1000, 1000)
            .and_then(|limit| limit.with_bytes(KIB, 16 * KIB))
            .unwrap(),
    );
    assert!(bridge.pipelines_posts());
    let lane_of = format!("/f/f/in/{0}/s-r/{0}/note", bridge.node);
    bridge.indexed_subjects.insert(lane_of.clone());
    spend_the_bucket(&bridge, 1, usize::try_from(16 * KIB).unwrap() - 64);
    let big = "z".repeat(usize::try_from(15 * KIB).unwrap());
    *lane.outbox.lock().unwrap() = Some(format!(
        "{}{}",
        queued_note("s-a", 1, "@s-r", &big),
        queued_note("s-b", 1, "@s-r", "small"),
    ));
    bridge.drain_outbox();
    bridge.settle_acks();
    let refused = broker.stats().rate_limited;
    assert!(
        refused > 0 && bridge.rate_held().is_some(),
        "PRECONDITION: s-a's post was refused for its bytes"
    );

    let left = bridge.rate_held().unwrap_or_default();
    std::thread::sleep(left);
    bridge.after_holds();
    bridge.settle_acks();
    let now = bridge.fx.now();
    let (mut reader, _) = bridge.connect().unwrap();
    ByteRefusedRun {
        refusals_after_the_hold: broker.stats().rate_limited - refused,
        landed: notes_on(&mut reader, &lane_of)
            .into_iter()
            .map(|(_, text)| text)
            .filter(|text| *text != big)
            .collect(),
        next_wake: bridge
            .rate
            .next_end(now)
            .map_or(Duration::ZERO, |end| end - now),
    }
}

#[cfg(unix)]
#[test]
fn an_own_lane_record_is_judged_only_once_its_pipelined_post_is_answered() {
    let (lane, mut bridge) = FakeVerbLane::bridge(&[(1, "s-a"), (2, "s-r")]);
    assert!(bridge.refresh_sessions().is_ok());
    let (socket, _broker, _server) = serve_log(&lane, "self-lane");
    bridge.cfg.broker = socket;
    assert!(bridge.attach_broker());
    let lane_of = format!("/f/f/in/{0}/s-r/{0}/note", bridge.node);
    bridge.indexed_subjects.insert(lane_of.clone());
    *lane.outbox.lock().unwrap() = Some(queued_note("s-a", 1, "@s-r", "mine"));
    bridge.in_round = true;
    bridge.drain_outbox();
    assert_eq!(
        bridge.conn.as_ref().unwrap().unacked(),
        1,
        "the post's ack is left for the loop"
    );
    assert!(
        retirements(&lane).is_empty(),
        "not retired before its answer"
    );

    let (mut reader, _) = bridge.connect().unwrap();
    let (off, raw) = aterm_spec::wait::until_within(
        aterm_spec::wait::MUST_HAPPEN,
        "the note on the log",
        || {
            let (rows, _) = reader.fetch(0, &lane_of, 1).ok()?;
            rows.into_iter().next().map(|(off, _, raw)| (off, raw))
        },
    );
    assert!(
        bridge.window_accuses(off),
        "the control: with its ack unread the set calls the node's own post a forgery"
    );
    bridge.on_inbox_record(&(off, lane_of.clone(), raw), None);
    bridge.in_round = false;

    assert!(bridge.self_acked.contains(&off), "the ack was read first");
    assert!(
        lane.transcript().contains(&format!("deliver {off}")),
        "delivered: {:#?}",
        lane.transcript()
    );
    assert!(retirements(&lane).contains(&("s-a".to_string(), 1, Some(off))));
    bridge.settle_acks();
    let (rows, _) = reader.fetch(0, "/f/f/pub/>", 4096).unwrap();
    let evs: Vec<String> = rows
        .iter()
        .map(|(_, _, raw)| crate::pct::decode(&String::from_utf8_lossy(raw)))
        .filter(|ev| ev.contains("forged-self") || ev.contains("cap-compromised"))
        .collect();
    assert!(evs.is_empty(), "no accusation: {evs:#?}");
}

#[cfg(unix)]
#[test]
fn a_halt_queued_at_a_drain_of_fresh_posts_is_applied_inside_the_drain() {
    let (lane, mut bridge) = FakeVerbLane::bridge(&[(1, "s-a"), (2, "s-b"), (3, "s-r")]);
    assert!(bridge.refresh_sessions().is_ok());
    let (socket, _broker, _server) = serve_log(&lane, "halt");
    bridge.cfg.broker = socket;
    bridge.conn = Some(bridge.connect().unwrap().0.into());
    let lane_of = format!("/f/f/in/{0}/s-r/{0}/note", bridge.node);
    bridge.indexed_subjects.insert(lane_of.clone());
    let mut frame = String::new();
    for n in 1..=4u64 {
        for sid in ["s-a", "s-b"] {
            frame.push_str(&queued_note(sid, n, "@s-r", &format!("{sid}-{n}")));
        }
    }
    *lane.outbox.lock().unwrap() = Some(frame);
    let generation = bridge.mailbox.broker_generation(Source::Fleet);
    let pushed = bridge.mailbox.push(
        Source::Fleet,
        (
            900,
            "/f/f/fleet/h-t/halt".to_string(),
            b"v=1 t=3 state=on reason=stop".to_vec(),
        ),
        None,
        generation,
        bridge.fx.now(),
    );
    assert!(matches!(pushed, Pushed::Queued));
    bridge.drain_outbox();

    assert!(bridge.conn.is_some(), "nothing here drops the connection");
    assert!(bridge.halt_applied, "the halt was applied by the drain");
    let holds: Vec<String> = lane
        .transcript()
        .into_iter()
        .filter(|line| line.starts_with("hold "))
        .collect();
    for sid in ["s-a", "s-b", "s-r"] {
        assert!(
            holds
                .iter()
                .any(|line| line.starts_with(&format!("hold {sid} on"))),
            "{sid} held: {holds:#?}"
        );
    }
    let retired = retirements(&lane);
    assert_eq!(
        retired.len(),
        8,
        "a halt holds PTY-reaching verbs, not the outbox: {retired:#?}"
    );
    let (mut reader, _) = bridge.connect().unwrap();
    assert_eq!(notes_on(&mut reader, &lane_of).len(), 8);
}

#[cfg(unix)]
#[test]
fn a_fresh_posts_unwritable_in_flight_entry_ends_the_drain_as_a_blocking_one_does() {
    for key in [false, true] {
        let (lane, mut bridge) = FakeVerbLane::bridge(&[(1, "s-a"), (3, "s-r")]);
        assert!(bridge.refresh_sessions().is_ok());
        let (socket, _broker, _server) = serve_log(&lane, "inflight");
        bridge.cfg.broker = socket;
        bridge.conn = Some(bridge.connect().unwrap().0.into());
        let own = format!("/f/f/in/{0}/s-r/{0}/note", bridge.node);
        let other = format!("/f/f/in/p/h-human/{}/note", bridge.node);
        bridge.indexed_subjects.insert(own.clone());
        bridge.indexed_subjects.insert(other.clone());
        let inflight = bridge.state.root().join("inflight");
        let _ = std::fs::remove_dir_all(&inflight);
        std::fs::write(&inflight, b"not a directory").unwrap();
        let keyed = |line: String, text: &str| {
            if key {
                line.replacen(" kind=", &format!(" key={text} kind="), 1)
            } else {
                line
            }
        };
        *lane.outbox.lock().unwrap() = Some(
            [
                keyed(queued_note("s-a", 1, "@s-r", "a1"), "a1"),
                keyed(queued_note("s-a", 2, "h-human", "a2"), "a2"),
            ]
            .concat(),
        );
        bridge.drain_outbox();

        assert!(bridge.conn.is_some(), "key={key}: the link stands");
        let (mut reader, _) = bridge.connect().unwrap();
        assert_eq!(
            notes_on(&mut reader, &own),
            [],
            "key={key}: a1 did not land"
        );
        assert_eq!(
            notes_on(&mut reader, &other),
            [],
            "key={key}: a2 never went out ahead of a1"
        );
        assert_eq!(
            retirements(&lane),
            [],
            "key={key}: nothing of s-a's retired"
        );
    }
}

#[cfg(unix)]
#[test]
fn a_reply_naming_a_pipelined_ask_is_judged_once_the_asks_answer_is_read() {
    let (lane, mut bridge) = FakeVerbLane::bridge(&[(1, "s-a")]);
    assert!(bridge.refresh_sessions().is_ok());
    let (socket, _broker, _server) = serve_log(&lane, "stray");
    bridge.cfg.broker = socket;
    assert!(bridge.attach_broker());
    bridge.state.pin("s-w", "n-far").expect("pin");
    bridge
        .connect()
        .unwrap()
        .0
        .publish(
            9_000,
            1,
            &subject::session_face("f", "n-far", "s-w", "presence"),
            b"v=1 t=1 inc=1 state=live",
        )
        .expect("n-far advertises s-w");
    let face = format!("/f/f/in/n-far/s-w/{}/ask", bridge.node);
    bridge.indexed_subjects.insert(face.clone());
    *lane.outbox.lock().unwrap() =
        Some("post sid=s-a id=1 to=@s-w@n-far kind=ask len=1\nq".to_string());
    bridge.in_round = true;
    bridge.drain_outbox();
    assert_eq!(
        bridge.conn.as_ref().unwrap().unacked(),
        1,
        "the ask's ack is left for the loop"
    );

    let (mut reader, _) = bridge.connect().unwrap();
    let asked =
        aterm_spec::wait::until_within(aterm_spec::wait::MUST_HAPPEN, "the ask on the log", || {
            let (rows, _) = reader.fetch(0, &face, 1).ok()?;
            rows.into_iter().next().map(|(off, _, _)| off)
        });
    assert_eq!(
        bridge.reply_from(asked, "n-evil"),
        None,
        "the control: with the ask's ack unread, nothing says where it went"
    );
    let stray = format!("/f/f/in/{}/s-a/n-evil/answer", bridge.node);
    let mut body = Body::new(0);
    body.re = Some(asked);
    body.text = "forged".to_string();
    let off = asked + 1;
    bridge.on_inbox_record(&(off, stray, body.encode()), None);
    bridge.in_round = false;

    assert_eq!(
        bridge.reply_from(asked, "n-evil"),
        Some(false),
        "the ack was read first: the ask is on record as n-far's to answer"
    );
    let delivered: Vec<String> = lane
        .deliveries()
        .into_iter()
        .filter(|line| line.contains(&format!(" off={off} ")))
        .collect();
    assert_eq!(delivered.len(), 1, "{delivered:#?}");
    assert!(
        delivered[0].contains(" kind=note ") && delivered[0].contains(" demoted=answer"),
        "the stray delivered as what it is: {delivered:#?}"
    );
    assert!(retirements(&lane).contains(&("s-a".to_string(), 1, Some(asked))));
}

#[cfg(unix)]
#[test]
fn a_post_still_in_flight_at_the_next_drains_peek_lands_once() {
    let (lane, mut bridge) = FakeVerbLane::bridge(&[(1, "s-a"), (2, "s-r")]);
    assert!(bridge.refresh_sessions().is_ok());
    let (socket, _broker, _server) = serve_log(&lane, "peek");
    bridge.cfg.broker = socket;
    bridge.conn = Some(bridge.connect().unwrap().0.into());
    let lane_of = format!("/f/f/in/{0}/s-r/{0}/note", bridge.node);
    bridge.indexed_subjects.insert(lane_of.clone());
    lane.retire_from_outbox();
    *lane.outbox.lock().unwrap() = Some(queued_note("s-a", 1, "@s-r", "once"));
    bridge.in_round = true;
    bridge.drain_outbox();
    assert_eq!(
        bridge.conn.as_ref().unwrap().unacked(),
        1,
        "the note's ack is left for the loop"
    );
    assert!(
        lane.outbox
            .lock()
            .unwrap()
            .as_deref()
            .is_some_and(|frame| frame.contains("sid=s-a id=1 ")),
        "the control: the endpoint still lists the note in flight"
    );

    bridge.drain_outbox();
    bridge.in_round = false;
    bridge.settle_acks();

    let (mut reader, _) = bridge.connect().unwrap();
    let notes = notes_on(&mut reader, &lane_of);
    let texts: Vec<&str> = notes.iter().map(|(_, text)| text.as_str()).collect();
    assert_eq!(texts, ["once"], "one record: {notes:#?}");
    assert_eq!(
        retirements(&lane),
        [("s-a".to_string(), 1, Some(notes[0].0))],
        "retired once, at its own offset"
    );
}

#[cfg(unix)]
#[test]
fn a_whole_drain_answers_the_drain_events_queued_before_its_peek_and_a_short_one_none() {
    let (lane, mut bridge) = FakeVerbLane::bridge(&[(1, "s-a"), (2, "s-b"), (3, "s-r")]);
    assert!(bridge.refresh_sessions().is_ok());
    let (socket, _broker, _server) = serve_log(&lane, "forget");
    bridge.cfg.broker = socket;
    bridge.conn = Some(bridge.connect().unwrap().0.into());
    let lane_of = format!("/f/f/in/{0}/s-r/{0}/note", bridge.node);
    bridge.indexed_subjects.insert(lane_of.clone());
    lane.retire_from_outbox();
    let queue = |bridge: &Bridge| {
        for line in [
            "EVENT 1 post 2",
            "EVENT 2 inbox 7 from=s-a kind=note off=40",
            "EVENT 2 post 1",
            "GAP 2 events-dropped=1",
            "EVENT 1 fetch off=9",
        ] {
            bridge.mailbox.push_event(line.to_string());
        }
    };
    let left = |bridge: &Bridge| {
        let mut left = Vec::new();
        while let Some(Item::Event(line)) = bridge.mailbox.take(Duration::ZERO, bridge.fx.now()) {
            left.push(line);
        }
        left
    };
    queue(&bridge);
    bridge.on_event("EVENT 2 inbox 6 from=s-a kind=note off=39");
    assert_eq!(
        left(&bridge).len(),
        5,
        "the control: an inbox event forgets nothing"
    );

    *lane.outbox.lock().unwrap() = Some(format!("{}more\n", queued_note("s-a", 1, "@s-r", "a1")));
    queue(&bridge);
    bridge.on_event("EVENT 1 post 1");
    assert_eq!(
        retirements(&lane).len(),
        1,
        "the post it was shown is published: {:?}",
        retirements(&lane)
    );
    assert_eq!(
        left(&bridge).len(),
        5,
        "a cut listing leaves every drain event for the next drain"
    );

    std::fs::write(bridge.state.root().join("fail-post-publish"), "s-b\n").unwrap();
    bridge.fault = Fault::FailPostPublishWhileMarked;
    *lane.outbox.lock().unwrap() = Some(queued_note("s-b", 1, "@s-r", "b1"));
    queue(&bridge);
    bridge.on_event("EVENT 2 post 1");
    assert!(
        bridge.held_senders.contains("s-b"),
        "the refusal holds its sender"
    );
    assert_eq!(
        left(&bridge).len(),
        5,
        "a held sender leaves every drain event for the next drain"
    );

    std::fs::remove_file(bridge.state.root().join("fail-post-publish")).unwrap();
    queue(&bridge);
    bridge.on_event("EVENT 2 post 1");
    assert_eq!(
        retirements(&lane)
            .iter()
            .filter(|(sid, _, off)| sid == "s-b" && off.is_some())
            .count(),
        1,
        "b1 landed: {:?}",
        retirements(&lane)
    );
    assert_eq!(
        left(&bridge),
        [
            "EVENT 2 inbox 7 from=s-a kind=note off=40",
            "GAP 2 events-dropped=1"
        ]
    );
}

#[cfg(unix)]
#[test]
fn a_refused_pipelined_publish_is_its_own_records_answer() {
    let (lane, mut bridge) = FakeVerbLane::bridge(&[(1, "s-a")]);
    assert!(bridge.refresh_sessions().is_ok());
    let max = astream_broker::store::MAX_SUBJECTS_PER_PRODUCER as u64;
    let (_broker, _server) = seeded_broker(&lane, &mut bridge, &["s-a"], &[], max);
    bridge
        .per_sid
        .presence
        .entry("s-new".to_string())
        .or_default()
        .sampled = true;
    bridge.batched(|bridge| {
        bridge.publish_ev_for(Some("s-a"), "one");
        bridge.publish_session_presence("s-new", "live");
        bridge.publish_ev_for(Some("s-a"), "two");
        assert_eq!(bridge.conn.as_ref().unwrap().unacked(), 3, "all in flight");
    });
    assert!(bridge.conn.is_some(), "a refusal leaves the link up");
    assert_eq!(
        bridge.per_sid.presence["s-new"].due(bridge.cfg.presence, Instant::now()),
        crate::presence::Due::First,
        "the refused row is not on the bus"
    );
    let (mut reader, _) = bridge.connect().unwrap();
    let face = subject::session_face("f", &bridge.node, "s-a", "ev");
    let (rows, _) = reader.fetch(0, &face, 64).unwrap();
    let evs: Vec<String> = rows
        .iter()
        .filter(|(_, s, _)| *s == face)
        .filter_map(|(_, _, raw)| {
            let ev = Body::decode(raw).0.unknown.get("ev")?.clone();
            Some(crate::pct::decode(&ev))
        })
        .collect();
    assert_eq!(evs, ["one", "two"]);
}

#[cfg(unix)]
#[test]
fn a_pipelined_halt_ack_moves_the_watermark_once_it_lands_and_goes_once() {
    let (lane, mut bridge) = FakeVerbLane::bridge(&[]);
    let (socket, _broker, _server) = serve_log(&lane, "ack");
    bridge.cfg.broker = socket;
    bridge.conn = Some(bridge.connect().unwrap().0.into());
    bridge.in_round = true;
    bridge.ack_halt(41);
    bridge.ack_halt(41);
    assert_eq!(bridge.conn.as_ref().unwrap().unacked(), 1, "sent once");
    assert_eq!(bridge.state.halt_acked(), None, "not before it lands");
    bridge.in_round = false;
    bridge.settle_acks();
    assert_eq!(bridge.state.halt_acked(), Some(41));
    let (mut reader, _) = bridge.connect().unwrap();
    let face = subject::node_face("f", &bridge.node, "ack");
    let (rows, _) = reader.fetch(0, &face, 8).unwrap();
    assert_eq!(
        rows.iter()
            .map(|(_, _, b)| Body::decode(b).0.re)
            .collect::<Vec<_>>(),
        [Some(41)]
    );
}

#[test]
fn sequences_are_reserved_a_block_at_a_time_and_a_restart_never_reuses_one() {
    let (_lane, mut bridge) = FakeVerbLane::bridge(&[]);
    let state = bridge.state.root().to_path_buf();
    let first = bridge.next_seq().unwrap();
    let end = bridge.state.sequence();
    assert_eq!(end, first + SEQ_BLOCK - 1, "one write reserves the block");
    let mut handed = vec![first];
    for _ in 1..SEQ_BLOCK {
        handed.push(bridge.next_seq().unwrap());
        assert_eq!(bridge.state.sequence(), end, "no write inside the block");
    }
    handed.push(bridge.next_seq().unwrap());
    assert_eq!(bridge.state.sequence(), end + SEQ_BLOCK, "the next block");
    for _ in 0..3 {
        handed.push(bridge.next_seq().unwrap());
    }
    assert!(
        handed.windows(2).all(|w| w[0] + 1 == w[1]),
        "gapless while alive"
    );
    let inc = bridge.inc;
    drop(bridge);
    let (_again_lane, mut again) = FakeVerbLane::bridge_with_state(&[], Some(&state));
    again.inc = inc;
    let resumed = again.next_seq().unwrap();
    assert_eq!(
        resumed,
        end + SEQ_BLOCK + 1,
        "above the dead block's end: its unused rest is burned"
    );
    assert!(handed.iter().all(|seq| *seq < resumed), "nothing is reused");
}

#[cfg(unix)]
#[test]
fn a_log_switch_rechecks_a_cached_broadcast_against_the_new_subject_budget() {
    let (lane, mut bridge) = FakeVerbLane::bridge(&[(1, "s-a")]);
    assert!(bridge.refresh_sessions().is_ok());
    let (a, _broker_a, _server_a) = serve_log(&lane, "say-a");
    bridge.cfg.broker = a;
    assert!(bridge.attach_broker());
    *lane.outbox.lock().unwrap() =
        Some("post sid=s-a id=1 to=say:known kind=note len=3\nold".to_string());
    bridge.drain_outbox();
    let subject = format!("/f/f/pub/{}/s-a/say/known", bridge.node);
    assert!(bridge.indexed_subjects.contains(&subject));
    bridge.conn = None;
    assert!(bridge.attach_broker());
    assert!(
        bridge.indexed_subjects.contains(&subject),
        "same-log cache stays"
    );

    bridge.close_subscriptions();
    bridge.conn = None;
    let max = astream_broker::store::MAX_SUBJECTS_PER_PRODUCER as u64;
    let held = max - max / SAY_RESERVE_DIVISOR;
    let (_broker_b, _server_b) = seeded_broker(&lane, &mut bridge, &["s-a"], &[], held);
    *lane.outbox.lock().unwrap() =
        Some("post sid=s-a id=2 to=say:known kind=note len=3\nnew".to_string());
    bridge.drain_outbox();
    assert!(
        lane.sent
            .lock()
            .unwrap()
            .iter()
            .any(|line| line == "outbox sent s-a 2 off=- reason=subject-budget"),
        "A's cache spent B's reserve: {:?}",
        lane.sent.lock().unwrap()
    );
    let (mut reader, _) = bridge.connect().unwrap();
    assert!(reader.fetch(0, &subject, 8).unwrap().0.is_empty());
    assert_eq!(
        reader
            .subject_budget(bridge.producer_id)
            .unwrap()
            .unwrap()
            .subjects,
        held
    );
}

#[cfg(unix)]
#[test]
fn a_log_switch_waits_until_every_old_offset_directory_can_be_read() {
    const SID: &str = "s-kept";
    for blocked in ["seen", "topics"] {
        let (lane, mut bridge) = FakeVerbLane::bridge(&[(1, SID)]);
        assert!(bridge.refresh_sessions().is_ok());
        let (a, broker_a, _server_a) = serve_log(&lane, "read-a");
        bridge.cfg.broker = a;
        assert!(bridge.attach_broker());
        bridge.state.set_seen_off(SID, 40).unwrap();
        bridge
            .state
            .set_topics(
                SID,
                &BTreeMap::from([("build".to_string(), TopicCursor { next: 41 })]),
            )
            .unwrap();
        let area = bridge.state.root().join(blocked);
        let saved = bridge.state.root().join(format!("{blocked}-saved"));
        std::fs::rename(&area, &saved).unwrap();
        std::fs::write(&area, b"not a directory").unwrap();
        let (b, broker_b, _server_b) = serve_log(&lane, "read-b");
        bridge.close_subscriptions();
        bridge.conn = None;
        bridge.cfg.broker = b;
        assert!(
            !bridge.attach_broker(),
            "unreadable {blocked} was treated as empty"
        );
        assert_eq!(
            bridge.state.log_witness().unwrap().id,
            Some(broker_a.log_identity())
        );
        assert_eq!(std::fs::read(&area).unwrap(), b"not a directory");
        assert!(saved.join(SID).is_file(), "the old offset was preserved");

        std::fs::remove_file(&area).unwrap();
        std::fs::rename(&saved, &area).unwrap();
        assert!(bridge.attach_broker(), "readable {blocked} can be retried");
        assert_eq!(
            bridge.state.log_witness().unwrap().id,
            Some(broker_b.log_identity())
        );
        assert_eq!(bridge.state.seen_off(SID), None);
    }
}

#[cfg(unix)]
#[test]
fn a_log_switch_waits_until_every_listed_topic_file_can_be_read() {
    const SID: &str = "s-kept";
    let (lane, mut bridge) = FakeVerbLane::bridge(&[(1, SID)]);
    assert!(bridge.refresh_sessions().is_ok());
    let (a, broker_a, _server_a) = serve_log(&lane, "entry-a");
    bridge.cfg.broker = a;
    assert!(bridge.attach_broker());
    bridge.state.set_seen_off(SID, 40).unwrap();
    let topic = bridge.state.root().join("topics").join(SID);
    std::fs::create_dir_all(topic.parent().unwrap()).unwrap();
    std::os::unix::fs::symlink(lane.dir.path().join("missing-topic"), &topic).unwrap();
    let (b, broker_b, _server_b) = serve_log(&lane, "entry-b");
    bridge.close_subscriptions();
    bridge.conn = None;
    bridge.cfg.broker = b;
    assert!(
        !bridge.attach_broker(),
        "a listed unreadable cursor was treated as empty"
    );
    assert_eq!(
        bridge.state.log_witness().unwrap().id,
        Some(broker_a.log_identity())
    );
    assert_eq!(
        bridge.state.seen_off(SID),
        Some(40),
        "preflight changed nothing"
    );
    assert!(
        topic.is_symlink(),
        "the unreadable entry was not overwritten"
    );

    std::fs::remove_file(&topic).unwrap();
    bridge
        .state
        .set_topics(
            SID,
            &BTreeMap::from([("build".to_string(), TopicCursor { next: 41 })]),
        )
        .unwrap();
    assert!(bridge.attach_broker());
    assert_eq!(
        bridge.state.log_witness().unwrap().id,
        Some(broker_b.log_identity())
    );
    assert_eq!(bridge.state.seen_off(SID), None);
}

#[cfg(unix)]
#[test]
fn a_log_transition_requires_the_endpoints_ack_before_refill() {
    const SID: &str = "s-kept";
    for (index, reply) in [
        "ERR busy\n",
        "ERR usage link\n",
        "ERR denied\n",
        "OK stale=1\n",
    ]
    .into_iter()
    .enumerate()
    {
        let (lane, mut bridge) = FakeVerbLane::bridge(&[(1, SID)]);
        lane.answer_hello_with(&format!("OK lane={LINK_LOG_LANE} build=test\n"));
        bridge.introduce();
        assert!(bridge.refresh_sessions().is_ok());
        let (socket, broker, _server) = serve_log(&lane, "log-ack");
        bridge.cfg.broker = socket;
        let (mut writer, _) = bridge.connect().unwrap();
        let subject = format!("/f/f/in/{}/{SID}/h-writer/note", bridge.node);
        let (off, _) = writer
            .publish(81, 1, &subject, b"v=1 t=1 text=mail")
            .unwrap();
        let head = broker.head();
        *lane.log_answer.lock().unwrap() = reply.to_string();
        let attached = bridge.attach_broker();
        assert!(
            lane.delivery_offsets().is_empty(),
            "{reply:?} allowed an unrebased refill"
        );
        assert!(!attached, "{reply:?} acknowledged no transition");
        assert_eq!(
            broker.head(),
            head,
            "a failed clock barrier published presence"
        );
        assert!(bridge.per_sid.pending_admit.contains(SID));
        assert_eq!(bridge.state.seen_off(SID), None);

        *lane.log_answer.lock().unwrap() = if index % 2 == 0 {
            "OK\n"
        } else {
            "OK forgot=1\n"
        }
        .to_string();
        assert!(
            bridge.attach_broker(),
            "an acknowledged transition can finish"
        );
        assert_eq!(lane.delivery_offsets(), [off]);
        assert!(!bridge.per_sid.pending_admit.contains(SID));
    }
}

#[cfg(unix)]
#[test]
fn a_log_transition_with_a_lost_endpoint_publishes_nothing() {
    let (lane, mut bridge) = FakeVerbLane::bridge(&[(1, "s-kept")]);
    lane.answer_hello_with(&format!("OK lane={LINK_LOG_LANE} build=test\n"));
    bridge.introduce();
    assert!(bridge.refresh_sessions().is_ok());
    let (socket, broker, _server) = serve_log(&lane, "log-lost");
    bridge.cfg.broker = socket;
    let head = broker.head();
    lane.log_answer.lock().unwrap().clear();
    let attached = bridge.attach_broker();
    assert_eq!(
        broker.head(),
        head,
        "a dead endpoint's attach wrote to the broker"
    );
    assert!(!attached);
    assert!(bridge.aterm_gone());
    assert!(lane.delivery_offsets().is_empty());
}

#[cfg(unix)]
#[test]
fn a_log_transition_is_not_sent_to_an_endpoint_below_lane_two() {
    const SID: &str = "s-kept";
    for endpoint_lane in [0, LINK_LOG_LANE - 1] {
        let (lane, mut bridge) = FakeVerbLane::bridge(&[(1, SID)]);
        lane.answer_hello_with(&format!("OK lane={endpoint_lane} build=old\n"));
        bridge.introduce();
        assert!(bridge.refresh_sessions().is_ok());
        let (socket, _broker, _server) = serve_log(&lane, "old-lane");
        bridge.cfg.broker = socket;
        let (mut writer, _) = bridge.connect().unwrap();
        let subject = format!("/f/f/in/{}/{SID}/h-writer/note", bridge.node);
        let (off, _) = writer
            .publish(81, 1, &subject, b"v=1 t=1 text=mail")
            .unwrap();
        lane.log_answer.lock().unwrap().clear();
        assert!(bridge.attach_broker());
        assert_eq!(lane.delivery_offsets(), [off]);
        assert!(!lane
            .links()
            .iter()
            .any(|line| line.starts_with("link log ")));
    }
}

#[cfg(unix)]
#[test]
fn a_record_decided_on_one_log_is_never_published_on_another() {
    const SID: &str = "s-decider";
    let (lane, mut bridge) = FakeVerbLane::bridge(&[(1, SID)]);
    assert!(bridge.refresh_sessions().is_ok());
    let node = bridge.node.clone();
    let lanes = format!("/f/f/in/{node}/>");
    let expired_face = format!("/f/f/in/{node}/{SID}/{node}/expired");
    let expired = |re: u64| {
        let mut body = Body::new(1);
        body.re = Some(re);
        body.dl = Some(1_000);
        body.encode()
    };
    let cut = |bridge: &mut Bridge| {
        let (conn, closer) = bridge.connect().unwrap();
        bridge.conn = Some(conn.into());
        bridge.read_log_identity().unwrap();
        closer.close();
    };

    let (a, broker_a, _server_a) = serve_log(&lane, "a");
    bridge.cfg.broker = a;
    assert!(bridge.attach_broker());
    let owed = OwedReceipt {
        sid: SID.to_string(),
        rid: 1,
        off: 5,
        verdict: "handled".to_string(),
        kind: "ask".to_string(),
        from: format!("s-asker@{node}"),
        earlier_log: false,
    };
    cut(&mut bridge);
    assert!(
        bridge.send_receipt(&owed).is_err(),
        "the receipt stays owed"
    );
    cut(&mut bridge);
    assert!(bridge.publish_own_off(&expired_face, &expired(5)).is_err());
    let journal = bridge.state.in_flight().unwrap();
    assert_eq!(journal.len(), 2, "{journal:#?}");
    assert!(
        journal
            .iter()
            .all(|(_, e)| e.log == Some(broker_a.log_identity())),
        "the journal names A: {journal:#?}"
    );

    let (b, broker_b, _server_b) = serve_log(&lane, "b");
    bridge.close_subscriptions();
    bridge.conn = None;
    bridge.cfg.broker = b;
    assert!(bridge.attach_broker());
    assert!(bridge.state.in_flight().unwrap().is_empty());
    let (mut reader, _) = bridge.connect().unwrap();
    let on_lanes = |reader: &mut Conn| -> Vec<(u64, String, String)> {
        let (rows, _) = reader.fetch(0, &lanes, 64).unwrap();
        rows.into_iter()
            .map(|(off, s, b)| (off, s, String::from_utf8_lossy(&b).into_owned()))
            .collect()
    };
    let rows = on_lanes(&mut reader);
    assert!(
        rows.is_empty(),
        "A's decisions reached B's lanes: {rows:#?}"
    );
    assert!(
        node_evs(&bridge)
            .iter()
            .any(|e| e.contains("ev=log-replaced head=") && e.contains(" inflight=2 ")),
        "{:#?}",
        node_evs(&bridge)
    );

    let before = lane.transcript().len();
    bridge
        .send_receipt(&OwedReceipt {
            earlier_log: true,
            ..owed.clone()
        })
        .unwrap();
    let rows = on_lanes(&mut reader);
    assert!(rows.is_empty(), "A's receipt reached B: {rows:#?}");
    assert_eq!(lane.transcript()[before..], ["deliver -".to_string()]);
    assert_eq!(bridge.state.receipt_pin(SID, 1).unwrap(), None);
    let ev_face = subject::session_face("f", &node, SID, "ev");
    let (rows, _) = reader.fetch(0, &ev_face, 64).unwrap();
    let said: Vec<String> = rows
        .iter()
        .map(|(_, _, b)| crate::pct::decode(&String::from_utf8_lossy(b)))
        .collect();
    assert!(
        said.iter()
            .any(|e| e.contains("ev=unacked re=5 verdict=handled reason=log-replaced")),
        "{said:#?}"
    );

    cut(&mut bridge);
    assert!(bridge.publish_own_off(&expired_face, &expired(7)).is_err());
    assert_eq!(
        bridge.state.in_flight().unwrap()[0].1.log,
        Some(broker_b.log_identity())
    );
    bridge.close_subscriptions();
    bridge.conn = None;
    assert!(bridge.attach_broker());
    let rows = on_lanes(&mut reader);
    assert_eq!(rows.len(), 1, "{rows:#?}");
    assert_eq!(Body::decode(rows[0].2.as_bytes()).0.re, Some(7));
    assert!(bridge.self_acked.contains(&rows[0].0), "and it is ours");
    assert!(bridge.state.in_flight().unwrap().is_empty());
}

#[cfg(unix)]
#[test]
fn a_log_restored_under_its_own_identity_replays_no_record_decided_before_it() {
    const SID: &str = "s-decider";
    let (lane, mut bridge) = FakeVerbLane::bridge(&[(1, SID)]);
    assert!(bridge.refresh_sessions().is_ok());
    let node = bridge.node.clone();
    let expired_face = format!("/f/f/in/{node}/{SID}/{node}/expired");
    let mut expired = Body::new(1);
    expired.re = Some(500);
    expired.dl = Some(1_000);
    let (a, broker, _server) = serve_log(&lane, "a");
    bridge.cfg.broker = a;
    assert!(bridge.attach_broker());
    let (mut conn, closer) = bridge.connect().unwrap();
    assert_eq!(conn.log_identity().unwrap(), Some(broker.log_identity()));
    bridge.conn = Some(conn.into());
    closer.close();
    assert!(bridge
        .publish_own_off(&expired_face, &expired.encode())
        .is_err());
    let journal = bridge.state.in_flight().unwrap();
    assert_eq!(journal.len(), 1, "{journal:#?}");
    assert_eq!(journal[0].1.log, Some(broker.log_identity()));

    bridge.bus_high = 1_000;
    bridge.close_subscriptions();
    bridge.conn = None;
    assert!(!bridge.attach_broker());
    let retained = bridge.state.in_flight().unwrap();
    assert_eq!(retained.len(), 1);
    assert_eq!(retained[0].1.body, journal[0].1.body);
    assert_eq!(
        bridge.state.rewound_log().unwrap(),
        Some(broker.log_identity())
    );
    let (mut reader, _) = bridge.connect().unwrap();
    let (rows, _) = reader.fetch(0, &format!("/f/f/in/{node}/>"), 64).unwrap();
    assert!(
        rows.is_empty(),
        "a record decided before the restore reached it: {rows:#?}"
    );
}

#[cfg(unix)]
#[test]
fn an_observer_answers_no_halt() {
    for (attachment, answers) in [(Attachment::Inherited, true), (Attachment::Observer, false)] {
        let (lane, mut bridge) = FakeVerbLane::bridge(&[(1, "s-s")]);
        let (socket, _broker, _server) = serve_log(&lane, "broker");
        bridge.cfg.broker = socket;
        bridge.conn = Some(bridge.connect().unwrap().0.into());
        bridge.attachment = attachment;
        let halt = (
            7,
            "/f/f/fleet/h-x/halt".to_string(),
            b"v=1 t=1 state=on".to_vec(),
        );
        bridge.on_fleet_record(&halt);
        assert!(bridge.halt_applied, "{attachment:?} records the transition");
        let ack_face = subject::node_face("f", &bridge.node, "ack");
        let (acks, _) = bridge.conn_now().unwrap().last(&ack_face, "", 8).unwrap();
        assert_eq!(!acks.is_empty(), answers, "{attachment:?}: {acks:?}");
        assert_eq!(
            bridge.state.halt_acked().is_some(),
            answers,
            "{attachment:?}"
        );
    }
}

#[cfg(unix)]
#[test]
fn a_halt_the_verb_lane_died_under_is_answered_by_the_bridge_that_applies_it() {
    const SID: &str = "s-held";
    const HALT: &str = "/f/f/fleet/h-x/halt";
    let (lane, mut bridge) = FakeVerbLane::bridge(&[(1, SID)]);
    let (socket, _broker, _server) = serve_log(&lane, "broker");
    bridge.cfg.broker = socket.clone();
    bridge.conn = Some(bridge.connect().unwrap().0.into());
    let (mut human, _) = bridge.connect().unwrap();
    let ack_face = subject::node_face("f", &bridge.node, "ack");
    let answers = |bridge: &Bridge| -> Vec<String> {
        let (mut reader, _) = bridge.connect().unwrap();
        let (rows, _) = reader.last(&ack_face, "", 8).unwrap();
        rows.iter()
            .map(|(_, _, body)| String::from_utf8_lossy(body).into_owned())
            .collect()
    };
    let names = |rows: &[String], re: u64, state: Option<&str>| {
        rows.iter().any(|row| {
            let mut words = row.split_whitespace();
            words.clone().any(|w| w == format!("re={re}"))
                && state.is_none_or(|state| words.any(|w| w == format!("state={state}")))
        })
    };

    let on = b"v=1 t=1 state=on reason=stop";
    let (on_at, _) = human.publish(91, 1, HALT, on).unwrap();
    bridge.on_fleet_record(&(on_at, HALT.to_string(), on.to_vec()));
    assert!(bridge.halt_applied);
    assert_eq!(bridge.state.halt_acked(), Some(on_at));
    let rows = answers(&bridge);
    assert!(names(&rows, on_at, Some("held")), "{rows:?}");

    let off = b"v=1 t=2 state=off";
    let (lift, _) = human.publish(91, 2, HALT, off).unwrap();
    lane.hang_up_at("hold ");
    bridge.on_fleet_record(&(lift, HALT.to_string(), off.to_vec()));
    assert!(bridge.ctl.lost(), "the lane closed under the `hold`");
    assert!(
        lane.transcript()
            .contains(&format!("hold {SID} off origin=fleet")),
        "{:#?}",
        lane.transcript()
    );
    assert!(bridge.halt_applied, "the belief is still the previous one");
    assert_eq!(
        bridge.state.halt_acked(),
        Some(on_at),
        "a barrier this bridge did not apply was marked answered"
    );
    let rows = answers(&bridge);
    assert!(
        names(&rows, on_at, Some("held")) && !names(&rows, lift, None),
        "the dying bridge answered the lift: {rows:?}"
    );

    let state_dir = bridge.state.root().to_path_buf();
    drop(bridge);
    let (next_lane, mut successor) = FakeVerbLane::bridge_with_state(&[(1, SID)], Some(&state_dir));
    successor.cfg.broker = socket;
    assert!(successor.refresh_sessions().is_ok());
    assert!(successor.attach_broker());
    assert!(!successor.halt_applied);
    assert!(
        next_lane
            .transcript()
            .contains(&format!("hold {SID} off origin=fleet")),
        "{:#?}",
        next_lane.transcript()
    );
    assert_eq!(successor.state.halt_acked(), Some(lift));
    let rows = answers(&successor);
    assert!(names(&rows, lift, Some("ready")), "{rows:?}");
}

#[cfg(unix)]
fn halt_answers(bridge: &Bridge) -> Vec<Body> {
    let (mut reader, _) = bridge.connect().unwrap();
    let ack_face = subject::node_face("f", &bridge.node, "ack");
    let (rows, _) = reader.fetch(0, &ack_face, 256).unwrap();
    rows.into_iter()
        .map(|(_, _, raw)| Body::decode(&raw).0)
        .collect()
}

#[cfg(unix)]
#[test]
fn a_busy_hold_is_answered_only_after_a_later_roster_round_reaches_it() {
    const FIRST: &str = "s-first";
    const LATER: &str = "s-later";
    const HALT: &str = "/f/f/fleet/h-x/halt";
    for on in [true, false] {
        let (lane, mut bridge) = FakeVerbLane::bridge(&[(1, FIRST), (2, LATER)]);
        let (socket, _broker, _server) = serve_log(&lane, "broker");
        bridge.cfg.broker = socket;
        bridge.conn = Some(bridge.connect().unwrap().0.into());
        assert!(bridge.refresh_sessions().is_ok());
        bridge.per_sid.pending_admit.clear();
        let (mut human, _) = bridge.connect().unwrap();
        if !on {
            let body = b"v=1 t=1 state=on reason=stop";
            let (off, _) = human.publish(91, 1, HALT, body).unwrap();
            bridge.on_fleet_record(&(off, HALT.into(), body.to_vec()));
            assert!(lane.holds(FIRST) && lane.holds(LATER));
        }
        let previous_ack = bridge.state.halt_acked();
        lane.refuse_hold(FIRST, Some("ERR busy update-committing"));
        let state = if on { "on" } else { "off" };
        let body = format!("v=1 t=2 state={state} reason=stop").into_bytes();
        let (barrier, _) = human.publish(91, 2, HALT, &body).unwrap();
        let before = lane.transcript().len();
        bridge.on_fleet_record(&(barrier, HALT.into(), body));
        assert_eq!(lane.holds(FIRST), !on, "a refused hold moved");
        assert_eq!(lane.holds(LATER), on, "the refusal skipped later sessions");
        assert!(lane.transcript()[before..]
            .iter()
            .any(|line| { line.starts_with(&format!("hold {LATER} {state} ")) }));
        assert_eq!(
            bridge.state.halt_acked(),
            previous_ack,
            "an unconfirmed transition was marked answered"
        );
        assert!(!halt_answers(&bridge)
            .iter()
            .any(|row| row.re == Some(barrier)));

        let endpoint = if on {
            EndpointHold::Fleet
        } else {
            EndpointHold::Free
        };
        bridge.converge_hold(LATER, endpoint);
        assert_eq!(
            lane.holds(LATER),
            on,
            "convergence rolled back the new intent"
        );
        bridge.roster_backstop();
        assert_eq!(bridge.state.halt_acked(), previous_ack);
        lane.refuse_hold(FIRST, None);
        bridge.roster_backstop();
        assert_eq!(lane.holds(FIRST), on);
        assert_eq!(lane.holds(LATER), on);
        assert_eq!(bridge.state.halt_acked(), Some(barrier));
        let expected = if on { "held" } else { "ready" };
        let answers = halt_answers(&bridge);
        let answer = answers.iter().find(|row| row.re == Some(barrier)).unwrap();
        assert_eq!(
            answer.unknown.get("state").map(String::as_str),
            Some(expected)
        );
        bridge.roster_backstop();
        assert_eq!(
            halt_answers(&bridge)
                .iter()
                .filter(|row| row.re == Some(barrier))
                .count(),
            1
        );
    }
}

#[cfg(unix)]
#[test]
fn only_an_absent_session_satisfies_a_halt_without_an_ok_hold() {
    for (reply, reached) in [("ERR no such session", true), ("ERR denied", false)] {
        let (lane, mut bridge) = FakeVerbLane::bridge(&[(1, "s-first"), (2, "s-later")]);
        let (socket, _broker, _server) = serve_log(&lane, "broker");
        bridge.cfg.broker = socket;
        bridge.conn = Some(bridge.connect().unwrap().0.into());
        lane.refuse_hold("s-first", Some(reply));
        let (mut human, _) = bridge.connect().unwrap();
        let body = b"v=1 t=1 state=on reason=stop";
        let subject = "/f/f/fleet/h-x/halt";
        let (barrier, _) = human.publish(91, 1, subject, body).unwrap();
        bridge.on_fleet_record(&(barrier, subject.into(), body.to_vec()));
        assert!(
            lane.holds("s-later"),
            "later sessions were skipped after {reply}"
        );
        assert_eq!(
            bridge.state.halt_acked(),
            reached.then_some(barrier),
            "{reply}"
        );
        assert_eq!(
            halt_answers(&bridge)
                .iter()
                .any(|row| row.re == Some(barrier)),
            reached
        );
    }
}

#[cfg(unix)]
#[test]
fn an_unread_roster_leaves_its_halt_owed_until_a_successful_round() {
    let (lane, mut bridge) = FakeVerbLane::bridge(&[(1, "s-first"), (2, "s-later")]);
    let (socket, _broker, _server) = serve_log(&lane, "broker");
    bridge.cfg.broker = socket;
    bridge.conn = Some(bridge.connect().unwrap().0.into());
    assert!(bridge.refresh_sessions().is_ok());
    bridge.per_sid.pending_admit.clear();
    lane.refuse_roster(true);
    let (mut human, _) = bridge.connect().unwrap();
    let body = b"v=1 t=1 state=on reason=stop";
    let subject = "/f/f/fleet/h-x/halt";
    let (barrier, _) = human.publish(91, 1, subject, body).unwrap();
    bridge.on_fleet_record(&(barrier, subject.into(), body.to_vec()));
    assert!(lane.holds("s-first") && lane.holds("s-later"));
    assert_eq!(bridge.state.halt_acked(), None);
    bridge.converge_hold("s-later", EndpointHold::Fleet);
    assert!(
        lane.holds("s-later"),
        "unread roster rolled back a standing halt"
    );
    bridge.roster_backstop();
    assert_eq!(bridge.state.halt_acked(), None);
    lane.refuse_roster(false);
    bridge.roster_backstop();
    assert_eq!(bridge.state.halt_acked(), Some(barrier));
    assert!(halt_answers(&bridge)
        .iter()
        .any(|row| row.re == Some(barrier)));
}

#[cfg(unix)]
#[test]
fn convergence_never_lifts_the_owners_local_hold() {
    const SID: &str = "s-held";
    let (lane, mut bridge) = FakeVerbLane::bridge(&[(1, SID)]);
    let (socket, _broker, _server) = serve_log(&lane, "broker");
    bridge.cfg.broker = socket;
    bridge.conn = Some(bridge.connect().unwrap().0.into());
    assert!(bridge.refresh_sessions().is_ok());
    bridge.per_sid.pending_admit.clear();
    let row = |origin: &str| {
        format!(
            "1 {SID} {:032x} sid=1 revision=1 hold=1 hold_origin={origin} detail=- agent=- gen=-",
            1
        )
    };
    let writes = |lane: &FakeVerbLane| -> Vec<String> {
        let hold = format!("hold {SID} ");
        let transcript = lane.transcript();
        transcript
            .into_iter()
            .filter(|line| line.starts_with(&hold))
            .collect()
    };

    lane.answer_status_with(&[row("local")]);
    bridge.sample_local_control();
    bridge.on_event("EVENT 1 hold 1 reason=mine origin=local");
    assert_eq!(
        writes(&lane),
        Vec::<String>::new(),
        "the owner's hold was lifted"
    );

    lane.answer_status_with(&[row("fleet")]);
    bridge.sample_local_control();
    bridge.on_event("EVENT 1 hold 1 reason=fabric-lost origin=fleet");
    let lift = format!("hold {SID} off origin=fleet");
    assert_eq!(writes(&lane), [lift.clone(), lift.clone()]);

    let (mut human, _) = bridge.connect().unwrap();
    let halt = "/f/f/fleet/h-x/halt";
    let body = b"v=1 t=1 state=on reason=stop";
    let (off, _) = human.publish(91, 1, halt, body).unwrap();
    bridge.on_fleet_record(&(off, halt.into(), body.to_vec()));
    let held = format!("hold {SID} on reason=stop origin=fleet");
    let before = writes(&lane);
    assert_eq!(before.last(), Some(&held), "the halt reached the session");
    lane.answer_status_with(&[row("local")]);
    bridge.sample_local_control();
    assert_eq!(&writes(&lane)[before.len()..], std::slice::from_ref(&held));
    let before = writes(&lane);
    lane.answer_status_with(&[row("fleet,local")]);
    bridge.sample_local_control();
    bridge.on_event("EVENT 1 hold 1 reason=stop origin=fleet hold_origin=fleet,local");
    assert_eq!(writes(&lane)[before.len()..], [] as [String; 0]);

    let off_body = b"v=1 t=2 state=off";
    let (off, _) = human.publish(91, 2, halt, off_body).unwrap();
    bridge.on_fleet_record(&(off, halt.into(), off_body.to_vec()));
    assert_eq!(writes(&lane).last(), Some(&lift), "the fleet's lift");
    let before = writes(&lane);
    lane.answer_status_with(&[row("local")]);
    bridge.sample_local_control();
    bridge.on_event("EVENT 1 hold 1 reason=mine origin=local hold_origin=local");
    assert_eq!(
        writes(&lane)[before.len()..],
        [] as [String; 0],
        "the owner's stop outlives the fleet's halt"
    );
    bridge.on_event("EVENT 1 hold 1 reason=stop origin=fleet hold_origin=fleet,local");
    assert_eq!(writes(&lane)[before.len()..], [lift]);
}

/// A `sessions status` row of the scripted lane for local `local`, Ready, naming the screen
/// generation `1.<seq>` and carrying the meaning fields [`META_HEADER`] says.
#[cfg(unix)]
fn ready_status_row(local: u64, seq: u64) -> String {
    format!(
        "{local} s-{local} {local:032x} sid={local} revision=1 hold=0 hold_origin=- \
         detail=- agent=idle {META_WORDS_SAID} gen=1.{seq}"
    )
}

#[cfg(unix)]
const META_HEADER: &str = "OK title=- user_title=worker description=- icon=- role=worker \
                           attention=- cwd=- state=alive";

/// [`META_HEADER`]'s meaning fields as a `sessions status` row and a watch ack say them.
#[cfg(unix)]
const META_WORDS_SAID: &str = "role=worker attention=- user_title=worker";

/// The `gen=` of every presence row of `sid` on the bus, oldest first.
#[cfg(unix)]
fn presence_gens(bridge: &Bridge, reader: &mut Conn, sid: &str) -> Vec<String> {
    presence_phases_and_gens(bridge, reader, sid)
        .into_iter()
        .map(|(_, gen)| gen)
        .collect()
}

/// The `(phase=, gen=)` of every presence row of `sid` on the bus, oldest first.
#[cfg(unix)]
fn presence_phases_and_gens(
    bridge: &Bridge,
    reader: &mut Conn,
    sid: &str,
) -> Vec<(String, String)> {
    let face = subject::session_face(&bridge.cfg.fleet, &bridge.node, sid, "presence");
    let (rows, _) = reader.fetch(0, &face, 256).unwrap();
    rows.iter()
        .filter(|(_, s, _)| *s == face)
        .map(|(_, _, raw)| {
            let body = Body::decode(raw).0;
            let phase = body.unknown.get("phase").cloned().unwrap_or_default();
            (phase, body.gen.unwrap_or_default())
        })
        .collect()
}

/// How many of `asked` count on each of the presence reads' tally rows: the batch, a
/// session's `status`, its `meta`, its screen.
#[cfg(unix)]
fn presence_reads(asked: &[(crate::wait_tally::Row, String)]) -> [usize; 4] {
    use crate::wait_tally::Row;
    [
        Row::LaneSessionsStatus,
        Row::LaneStatus,
        Row::LaneMeta,
        Row::LaneText,
    ]
    .map(|row| asked.iter().filter(|(r, _)| *r == row).count())
}

/// The `key=` of every presence row of `sid` on the bus, oldest first.
#[cfg(unix)]
fn presence_said(bridge: &Bridge, reader: &mut Conn, sid: &str, key: &str) -> Vec<String> {
    let face = subject::session_face(&bridge.cfg.fleet, &bridge.node, sid, "presence");
    let (rows, _) = reader.fetch(0, &face, 256).unwrap();
    rows.iter()
        .filter(|(_, s, _)| *s == face)
        .map(|(_, _, raw)| {
            Body::decode(raw)
                .0
                .unknown
                .get(key)
                .cloned()
                .unwrap_or_default()
        })
        .collect()
}

/// THE ATTACH READS NO SCREEN, ITS SESSIONS' STATUS ONCE, AND NO SESSION'S META (critic items
/// D03 and E05): a bridge attaching N sessions whose `sessions status` rows are Ready, each
/// naming a generation, giving the verdict and carrying the session's `role= attention=
/// user_title=`, asks that batch once and no session's `meta`, `status` or screen
/// (`text --json`), and its sessions' watch acks, which carry the same three, ask nothing
/// either. Each row's `gen=` is the token of the generation its batch row named, and its
/// `role=` and `title=` the batch row's. CONTROLS: `Mutant::ReadTheScreenEveryRow` (a screen
/// read for every row, as before D03) reads a `text --json` a session;
/// `Mutant::ReadMetaPerSession` (each session's own `@<sid> meta`, as before E05) reads a
/// `meta` a session at the attach and another at its ack.
#[cfg(unix)]
#[test]
fn an_attach_of_ready_sessions_reads_one_status_batch_and_no_meta_or_screen() {
    use crate::sim::world::Mutant;
    const N: u64 = 4;
    for control in [
        None,
        Some(Mutant::ReadTheScreenEveryRow),
        Some(Mutant::ReadMetaPerSession),
    ] {
        let sessions: Vec<(u64, String)> = (1..=N).map(|l| (l, format!("s-{l}"))).collect();
        let named: Vec<(u64, &str)> = sessions.iter().map(|(l, s)| (*l, s.as_str())).collect();
        let (lane, mut bridge) = FakeVerbLane::bridge(&named);
        if let Some(mutant) = control {
            bridge.fx = crate::fx::Fx::os_without(mutant);
        }
        let (socket, _broker, _server) = serve_log(&lane, "attach");
        bridge.cfg.broker = socket;
        lane.answer_meta_with(META_HEADER);
        lane.answer_status_with(
            &(1..=N)
                .map(|l| ready_status_row(l, 10 + l))
                .collect::<Vec<_>>(),
        );
        assert!(bridge.first_roster());
        lane.take_requests();
        assert!(bridge.attach_broker(), "the attach");
        let n = usize::try_from(N).unwrap();
        let reads = presence_reads(&lane.take_requests());
        for (local, sid) in &sessions {
            bridge.on_event(&format!("sub {local} {sid} {META_WORDS_SAID}"));
        }
        let acks = presence_reads(&lane.take_requests());
        match control {
            Some(Mutant::ReadTheScreenEveryRow) => {
                assert_eq!(reads[3], n, "the control reads a screen a row");
                continue;
            }
            Some(_) => {
                assert_eq!(
                    (reads[2], acks[2]),
                    (n, n),
                    "the control reads a meta a session, at the attach and at its ack"
                );
                continue;
            }
            None => {}
        }
        assert_eq!(
            reads,
            [1, 0, 0, 0],
            "the attach's [sessions status, @sid status, @sid meta, @sid text]"
        );
        assert_eq!(acks, [0, 0, 0, 0], "the watch acks' reads");
        let (mut reader, _) = bridge.connect().unwrap();
        for (local, sid) in &sessions {
            let screen = ScreenGen {
                epoch: 1,
                seq: 10 + local,
            };
            assert_eq!(
                presence_gens(&bridge, &mut reader, sid),
                [crate::presence::gen_token(screen)],
                "{sid}'s row says the generation its batch row named"
            );
            assert_eq!(
                (
                    presence_said(&bridge, &mut reader, sid, "role"),
                    presence_said(&bridge, &mut reader, sid, "title"),
                ),
                (vec!["worker".to_string()], vec!["worker".to_string()]),
                "{sid}'s row says the meaning fields its batch row carried"
            );
        }
    }
}

/// A WATCH ACK NEVER ROLLS A SLOT BACK, AND A CHANGE IT SAYS REACHES PRESENCE (critic item
/// E05 and its review). aterm reads an ack's meaning fields in one meta hold with its watch's
/// start, and the bridge cannot order that start against the read that sampled the slot, so
/// an ack that agrees with the slot reads nothing and one that differs reads the session's
/// own `@<sid> meta`, which is later than the watch's start, whichever came first. s-1's
/// batch said `role=worker` and its role became `lead` before its watch began, so no `EVENT
/// meta` will tell it and the ack says `lead`. s-2's watch began first (as `start()`
/// subscribes before the first attach), its ack saying `worker`, and its role became `lead`
/// before the batch read it, so the ack is the older. Both slots, and their newest rows, say
/// `lead`, and no row of s-2 says the ack's `worker`. s-3's ack says only some of the three
/// and reads the session's own meta too; s-4's agrees and reads nothing.
#[cfg(unix)]
#[test]
fn a_watch_ack_never_rolls_a_slot_back_and_a_change_it_says_reaches_presence() {
    let lead_words = META_WORDS_SAID.replace("role=worker", "role=lead");
    let lead_header = META_HEADER.replace("role=worker", "role=lead");
    let (lane, mut bridge) =
        FakeVerbLane::bridge(&[(1, "s-1"), (2, "s-2"), (3, "s-3"), (4, "s-4")]);
    let (socket, _broker, _server) = serve_log(&lane, "ack-meta");
    bridge.cfg.broker = socket;
    lane.answer_meta_with(META_HEADER);
    lane.answer_status_with(&[
        ready_status_row(1, 11),
        ready_status_row(2, 12).replace(META_WORDS_SAID, &lead_words),
        ready_status_row(3, 13),
        ready_status_row(4, 14),
    ]);
    assert!(bridge.first_roster());
    assert!(bridge.attach_broker(), "the attach");
    assert_eq!(
        presence_reads(&lane.take_requests())[2],
        0,
        "the attach reads no meta"
    );
    // The sessions' meta as `@<sid> meta` reads it from here on: s-1's and s-2's role is
    // `lead`, the change each ack races.
    lane.answer_meta_with(&lead_header);

    for ack in [
        format!("sub 1 s-1 {lead_words}"),
        format!("sub 2 s-2 {META_WORDS_SAID}"),
        "sub 3 s-3 role=boss attention=-".to_string(),
    ] {
        bridge.on_event(&ack);
        assert_eq!(
            presence_reads(&lane.take_requests()),
            [0, 0, 1, 0],
            "an ack that differs from its slot reads the session's own meta: {ack}"
        );
    }
    bridge.on_event(&format!("sub 4 s-4 {META_WORDS_SAID}"));
    assert_eq!(
        presence_reads(&lane.take_requests()),
        [0, 0, 0, 0],
        "an ack that agrees with its slot reads nothing"
    );
    for (sid, role) in [
        ("s-1", "lead"),
        ("s-2", "lead"),
        ("s-3", "lead"),
        ("s-4", "worker"),
    ] {
        let slot = &bridge.per_sid.presence[sid];
        assert!(slot.sampled, "{sid}");
        assert_eq!(slot.fields.role, role, "{sid}'s slot");
    }

    let (mut reader, _) = bridge.connect().unwrap();
    for sid in ["s-1", "s-2"] {
        assert!(bridge.batched(|b| b.publish_session_presence(sid, "live")));
    }
    let said = presence_said(&bridge, &mut reader, "s-1", "role");
    assert_eq!(
        (
            said.first().map(String::as_str),
            said.last().map(String::as_str)
        ),
        (Some("worker"), Some("lead")),
        "s-1's attach row said the batch's role, and its newest says the change"
    );
    assert_eq!(
        presence_said(&bridge, &mut reader, "s-2", "role")
            .iter()
            .filter(|role| *role != "lead")
            .count(),
        0,
        "no row of s-2 says its older ack's role"
    );
}

/// AN ATTACH READS A SESSION'S OWN STATUS WHERE ITS BATCH ROW LEFT THE SLOT SHORT (the D03
/// review): a Ready row that deferred the agent verdict (`agent_deferred=1`: a writer held
/// the session's timeline) or named no generation (`gen=-`: the terminal was busy) has that
/// session's row read `@<sid> status`, which waits for the verdict, so its first row says
/// the verdict, as every attach row did before D03. A session whose row gave both reads none
/// (the batch-only attach is `an_attach_of_ready_sessions_reads_one_status_batch_…`). Its
/// `meta` is read alone only where its row left the meaning fields out (critic item E05: a
/// writer held the session's meta), whatever the row said of its status.
#[cfg(unix)]
#[test]
fn an_attach_reads_its_own_status_for_a_row_that_deferred_its_verdict_or_named_no_screen() {
    let (lane, mut bridge) = FakeVerbLane::bridge(&[(1, "s-1"), (2, "s-2"), (3, "s-3")]);
    let (socket, _broker, _server) = serve_log(&lane, "attach-short");
    bridge.cfg.broker = socket;
    lane.answer_meta_with(META_HEADER);
    lane.answer_single_status_with("OK hold=0 detail=- agent=busy gen=1.99");
    let short = |local: u64, tail: &str| {
        format!(
            "{local} s-{local} {local:032x} sid={local} revision=1 hold=0 hold_origin=- \
             detail=- {tail}"
        )
    };
    lane.answer_status_with(&[
        ready_status_row(1, 11),
        short(2, "agent_deferred=1 gen=1.12"),
        short(3, &format!("agent=idle {META_WORDS_SAID} gen=-")),
    ]);
    assert!(bridge.first_roster());
    lane.take_requests();
    assert!(bridge.attach_broker(), "the attach");
    let asked = lane.take_requests();
    let single = |lane_row: crate::wait_tally::Row| -> Vec<&str> {
        asked
            .iter()
            .filter(|(row, _)| *row == lane_row)
            .map(|(_, line)| line.as_str())
            .collect()
    };
    assert_eq!(
        single(crate::wait_tally::Row::LaneStatus),
        ["@s-2 status", "@s-3 status"],
        "the two short rows' sessions, and only they, read their own status"
    );
    assert_eq!(
        single(crate::wait_tally::Row::LaneMeta),
        ["@s-2 meta"],
        "the row that left its meaning fields out, and only it, reads its own meta"
    );
    assert_eq!(
        presence_reads(&asked),
        [1, 2, 1, 0],
        "[sessions status, @sid status, @sid meta, @sid text]"
    );
    let (mut reader, _) = bridge.connect().unwrap();
    let token = |seq| crate::presence::gen_token(ScreenGen { epoch: 1, seq });
    assert_eq!(
        presence_phases_and_gens(&bridge, &mut reader, "s-1"),
        [("idle".to_string(), token(11))],
    );
    assert_eq!(
        presence_phases_and_gens(&bridge, &mut reader, "s-2"),
        [("busy".to_string(), token(99))],
        "a deferred verdict's first row says the verdict its own read gave"
    );
    assert_eq!(
        presence_phases_and_gens(&bridge, &mut reader, "s-3"),
        [("busy".to_string(), token(99))],
        "a busy terminal's first row says the generation its own read named"
    );
}

/// AN OBSERVER'S EVERY ROW READS ITS SESSION'S STATUS (the D03 review): an observer runs no
/// round, so no `sessions status` batch keeps its slots current; each of its live rows reads
/// `@<sid> status` (never the screen) and says the generation and verdict that read gave, as
/// each of its rows read the screen before D03. CONTROL: an inherited bridge, whose round
/// keeps the slot current, reads the batch at the attach and nothing for a later row, which
/// says the slot's generation.
#[cfg(unix)]
#[test]
fn an_observers_every_row_reads_its_sessions_status() {
    let token = |seq| crate::presence::gen_token(ScreenGen { epoch: 1, seq });
    for attachment in [Attachment::Observer, Attachment::Inherited] {
        let (lane, mut bridge) = FakeVerbLane::bridge(&[(1, "s-1")]);
        bridge.attachment = attachment;
        let (socket, _broker, _server) = serve_log(&lane, "observer-rows");
        bridge.cfg.broker = socket;
        lane.answer_meta_with(META_HEADER);
        lane.answer_single_status_with("OK hold=0 detail=- agent=idle gen=1.5");
        lane.answer_status_with(&[ready_status_row(1, 5)]);
        assert!(bridge.first_roster());
        lane.take_requests();
        assert!(bridge.attach_broker(), "{attachment:?}: the attach");
        let attach = presence_reads(&lane.take_requests());
        lane.answer_single_status_with("OK hold=0 detail=- agent=busy gen=1.900");
        assert!(bridge.batched(|b| b.publish_session_presence("s-1", "live")));
        let later = presence_reads(&lane.take_requests());
        let (mut reader, _) = bridge.connect().unwrap();
        let rows = presence_phases_and_gens(&bridge, &mut reader, "s-1");
        let row = |phase: &str, seq| (phase.to_string(), token(seq));
        if attachment == Attachment::Inherited {
            assert_eq!(
                attach,
                [1, 0, 0, 0],
                "the control's attach reads the batch alone"
            );
            assert_eq!(later, [0, 0, 0, 0], "the control's later row reads nothing");
            assert_eq!(rows, [row("idle", 5), row("idle", 5)]);
            continue;
        }
        assert_eq!(
            attach,
            [0, 1, 1, 0],
            "an observer's attach row reads its status"
        );
        assert_eq!(later, [0, 1, 0, 0], "and so does its every later row");
        assert_eq!(
            rows,
            [row("idle", 5), row("busy", 900)],
            "each row says what its own read gave"
        );
    }
}

/// AN EVENT'S ROW READS NO SCREEN EITHER (critic item D03): an agent verdict's row says the
/// generation the verdict names and reads nothing; a meta change's row reads the session's
/// `meta` alone, and says the generation the batch named. CONTROL
/// (`Mutant::ReadTheScreenEveryRow`): a screen read each.
#[cfg(unix)]
#[test]
fn an_agent_or_meta_events_row_reads_no_screen() {
    for control in [false, true] {
        let (lane, mut bridge) = FakeVerbLane::bridge(&[(1, "s-1"), (2, "s-2")]);
        if control {
            bridge.fx = crate::fx::Fx::os_without(crate::sim::world::Mutant::ReadTheScreenEveryRow);
        }
        let (socket, _broker, _server) = serve_log(&lane, "events");
        bridge.cfg.broker = socket;
        bridge.conn = Some(bridge.connect().unwrap().0.into());
        bridge.refresh_sessions().unwrap();
        bridge.per_sid.pending_admit.clear();
        lane.answer_meta_with(META_HEADER);
        lane.answer_status_with(&[ready_status_row(1, 5), ready_status_row(2, 6)]);
        bridge.sample_meta("s-1");
        bridge.presence_from_status_batch();
        lane.take_requests();

        bridge.batched(|b| b.on_event("EVENT 1 agent busy rev=2 gen=1.77 fp=0"));
        let agent = presence_reads(&lane.take_requests());
        bridge.batched(|b| b.on_event("EVENT 2 meta"));
        let meta = presence_reads(&lane.take_requests());
        if control {
            assert_eq!(
                (agent[3], meta[3]),
                (1, 1),
                "the control reads a screen a row"
            );
            continue;
        }
        assert_eq!(agent, [0, 0, 0, 0], "an agent verdict's row reads nothing");
        assert_eq!(
            meta,
            [0, 0, 1, 0],
            "a meta change's row reads the meta alone"
        );
        let (mut reader, _) = bridge.connect().unwrap();
        assert_eq!(
            presence_gens(&bridge, &mut reader, "s-1"),
            [crate::presence::gen_token(ScreenGen { epoch: 1, seq: 77 })],
            "the verdict's row says the screen it was judged on"
        );
        assert_eq!(
            presence_gens(&bridge, &mut reader, "s-2"),
            [crate::presence::gen_token(ScreenGen { epoch: 1, seq: 6 })],
        );
    }
}

#[cfg(unix)]
#[test]
fn an_observer_leaves_the_nodes_inbox_group_where_it_found_it() {
    const SID: &str = "s-watched";
    let (lane, mut bridge) = FakeVerbLane::bridge(&[(1, SID)]);
    bridge.attachment = Attachment::Observer;
    let (socket, _broker, _server) = serve_log(&lane, "broker");
    bridge.cfg.broker = socket;
    let (mut writer, _) = bridge.connect().unwrap();
    let mail = format!("/f/f/in/{}/{SID}/h-writer/note", bridge.node);
    let (old, _) = writer.publish(81, 1, &mail, b"v=1 t=1 text=old").unwrap();
    assert!(bridge.refresh_sessions().is_ok());
    assert!(bridge.attach_broker());
    assert!(!bridge.per_sid.pending_admit.contains(SID));
    assert!(
        !node_evs(&bridge)
            .iter()
            .any(|e| e.contains("reason=observer")),
        "an observer refilled its node's mail"
    );

    let (live, _) = writer.publish(81, 2, &mail, b"v=1 t=2 text=live").unwrap();
    let rec = match bridge
        .mailbox
        .take(aterm_spec::wait::MUST_HAPPEN, bridge.fx.now())
    {
        Some(Item::Inbox(rec, _)) => rec,
        other => panic!("the observer's lane reader: {other:?}"),
    };
    assert_eq!(rec.0, live);
    bridge.on_inbox_record(&rec, None);
    assert!(
        node_evs(&bridge)
            .iter()
            .any(|e| e.contains(&format!("undeliverable off={live} reason=observer"))),
        "live mail gets its one honest record"
    );
    assert_eq!(lane.delivery_offsets(), [], "and nothing is delivered");

    let (consumer, closer) = bridge.connect().unwrap();
    closer
        .set_read_timeout(Some(aterm_spec::wait::MUST_HAPPEN))
        .unwrap();
    let group = subject::inbox_group("f", &bridge.node);
    let mut sub = consumer
        .subscribe_group(&group, &subject::inbox_filter("f", &bridge.node))
        .unwrap();
    let first = next_delivery(&mut sub, "the group's first record");
    assert_eq!(first.0, old, "the observer moved the node's cursor");
}

#[cfg(unix)]
#[test]
fn an_observer_on_a_copy_of_a_live_nodes_state_dir_is_refused() {
    let (lane, mut bridge) = FakeVerbLane::bridge(&[(1, "s-copy")]);
    bridge.attachment = Attachment::Observer;
    let (socket, _broker, _server) = serve_log(&lane, "broker");
    bridge.cfg.broker = socket;
    let (mut node, _) = bridge.connect().unwrap();
    let face = subject::node_face("f", &bridge.node, "presence");
    let host = crate::pct::encode(&hostname());
    let holder = std::os::unix::process::parent_id();
    let live = format!("v=1 t=1 state=live inc=4 fabric=connected host={host} pid={holder}");
    node.publish(77, 1, &face, live.as_bytes()).unwrap();
    assert!(!bridge.attach_broker());
    let refused = bridge.refused.take().expect("refused, not retried");
    assert_eq!(refused.kind(), io::ErrorKind::AlreadyExists);
    assert_eq!(bridge.state.incarnation(), 0, "it took no incarnation");
    let (rows, _) = node.last(&face, "", 8).unwrap();
    assert_eq!(rows.len(), 1);
    assert_eq!(rows[0].2, live.as_bytes(), "it wrote no node row");

    let gone = "v=1 t=2 state=gone inc=4 fabric=disconnected";
    node.publish(77, 2, &face, gone.as_bytes()).unwrap();
    assert!(bridge.attach_broker());
    let (rows, _) = node.last(&face, "", 8).unwrap();
    let row = String::from_utf8_lossy(&rows[0].2).into_owned();
    assert!(
        row.contains("state=live inc=5") && row.contains("observer=1"),
        "{row}"
    );
}

#[test]
fn a_node_row_is_carried_elsewhere_only_by_a_live_holder() {
    let running = std::os::unix::process::parent_id();
    let (dead, me) = (u32::MAX, std::process::id());
    for (state, holder, ours, carried) in [
        ("live", format!("host=here pid={running}"), 0, true),
        ("live", format!("host=here pid={running}"), 3, false),
        ("gone", format!("host=here pid={running}"), 0, false),
        ("live", format!("host=here pid={dead}"), 0, false),
        ("live", format!("host=here pid={me}"), 0, false),
        ("live", format!("host=far pid={dead}"), 0, true),
        ("live", String::new(), 0, true),
        ("live", "host=here".to_string(), 0, true),
        ("live", "host=here pid=unknown".to_string(), 0, true),
        ("live", "host=here pid=4294967296".to_string(), 0, true),
    ] {
        let row = format!("v=1 state={state} inc=3 {holder}");
        let body = Body::decode(row.as_bytes()).0;
        assert_eq!(
            carried_elsewhere(&body, 3, ours, "here").is_some(),
            carried,
            "{row} (our inc {ours})"
        );
    }
}

#[cfg(unix)]
#[test]
fn the_ghost_sweep_never_retires_a_row_superseded_past_its_walk() {
    use astream_broker::proto::{decode_request, encode_response, read_frame, write_frame};
    use astream_broker::{Request, Response};
    let (_lane, mut bridge) = FakeVerbLane::bridge(&[]);
    bridge.inc = 2;
    bridge.fault = Fault::SweepGhostsAtOnceWhileMarked;
    std::fs::write(bridge.state.root().join("sweep-ghosts-now"), b"").unwrap();
    let node = bridge.node.clone();
    let published = Arc::new(std::sync::atomic::AtomicBool::new(false));
    let (ours, theirs) = std::os::unix::net::UnixStream::pair().unwrap();
    {
        let published = Arc::clone(&published);
        std::thread::spawn(move || {
            let mut s = theirs;
            let row = |offset: u64, inc: u64| Response::Delivery {
                offset,
                subject: format!("/f/f/pub/{node}/s-x/presence"),
                body: format!("v=1 t=1 inc={inc} state=live").into_bytes(),
            };
            while let Ok(Some(frame)) = read_frame(&mut s) {
                let (rows, next, head) = match decode_request(&frame) {
                    Some(Request::Last { .. }) => (vec![row(3, 2)], 10, 10),
                    Some(Request::Fetch {
                        from_offset: 10, ..
                    }) => (vec![row(11, 3)], 12, 12),
                    _ => {
                        published.store(true, std::sync::atomic::Ordering::SeqCst);
                        return;
                    }
                };
                for r in rows.iter().chain([&Response::Mark {
                    next,
                    head,
                    resume: String::new(),
                    durable: None,
                    oldest: None,
                }]) {
                    if write_frame(&mut s, &encode_response(r)).is_err() {
                        return;
                    }
                }
            }
        });
    }
    bridge.conn = Some(Conn::from_stream(Box::new(ours)).into());
    bridge.retire_ghost_presence();
    assert!(bridge.conn.is_some(), "the read was answered");
    assert!(bridge.ghosts.is_empty(), "{:?}", bridge.ghosts);
    assert!(
        !published.load(std::sync::atomic::Ordering::SeqCst),
        "the sweep wrote over a newer incarnation's row"
    );
}

#[cfg(unix)]
#[test]
fn a_refused_exited_row_is_owed_to_the_sweep_until_it_lands() {
    let (lane, mut bridge) = FakeVerbLane::bridge(&[]);
    let (socket, _broker, _server) = serve_log(&lane, "exits-owed");
    bridge.cfg.broker = socket;
    assert!(bridge.attach_broker());
    assert!(!bridge.sweep_owed(), "nothing owed after the attach");
    let (fleet, node) = (bridge.cfg.fleet.clone(), bridge.node.clone());
    let face = |sid: &str| subject::session_face(&fleet, &node, sid, "presence");
    let (mut writer, _) = bridge.connect().unwrap();
    let mark = bridge.mark.clone().expect("the state dir's mark");
    let ours = |row: &str| format!("{row} {}={mark}", crate::state::PRESENCE_MARK);
    let live = ours(&format!("v=1 t=1 inc={} state=live", bridge.inc));
    writer
        .publish(91, 1, &face("s-gone"), live.as_bytes())
        .unwrap();
    writer
        .publish(91, 2, &face("s-done"), live.as_bytes())
        .unwrap();
    let done = ours(&format!("v=1 t=2 inc={} state=exited", bridge.inc));
    writer
        .publish(91, 3, &face("s-done"), done.as_bytes())
        .unwrap();
    for sid in ["s-gone", "s-done"] {
        bridge.conclude(
            Then::Presence {
                sid: sid.to_string(),
                mark: None,
                exited: true,
            },
            Err(io::Error::other("batch rolled back")),
        );
    }
    assert_eq!(
        bridge.exits_owed,
        ["s-done", "s-gone"].map(String::from).into_iter().collect(),
        "each refused `exited` row is owed"
    );
    assert!(bridge.sweep_owed(), "the sweep's arm has work");
    bridge.retire_ghost_presence();
    let newest = |bridge: &Bridge, sid: &str| {
        let (mut reader, _) = bridge.connect().unwrap();
        let (rows, _) = reader.last(&face(sid), "", 8).unwrap();
        String::from_utf8_lossy(&rows.last().expect("a row").2).into_owned()
    };
    assert!(
        newest(&bridge, "s-gone").contains("state=exited"),
        "the owed row was written"
    );
    assert_eq!(
        newest(&bridge, "s-done"),
        done,
        "nothing written over a row that already says exited"
    );
    assert!(bridge.exits_owed.is_empty(), "{:?}", bridge.exits_owed);
    assert!(bridge.ghosts.is_empty(), "an owed exit is no ghost");
    assert!(
        !node_evs(&bridge)
            .iter()
            .any(|ev| ev.contains("presence-retired")),
        "a departure's own row is no retirement"
    );
}

#[cfg(unix)]
#[test]
fn failed_live_admission_keeps_new_group_mail_behind_old_refill() {
    const SID: &str = "s-live-refill";
    let (lane, mut bridge) = FakeVerbLane::bridge(&[(1, SID)]);
    let socket = lane.dir.path().join("broker.sock");
    let broker = crate::test_broker(lane.dir.path().join("broker.log")).unwrap();
    let _server = broker.serve(&socket).unwrap();
    bridge.cfg.broker = socket.to_string_lossy().into_owned();

    let (mut writer, _) = bridge.connect().unwrap();
    let subject = format!("/f/f/in/{}/{SID}/h-writer/note", bridge.node);
    let (old, _) = writer
        .publish(82, 1, &subject, b"v=1 t=1 text=old")
        .unwrap();
    let group = subject::inbox_group("f", &bridge.node);
    writer.commit(&group, old).unwrap();
    let model = aterm_models_session::fabric_refill_retry_model();
    let mut state = model.init_state();
    let (first, first_closer) = bridge.connect().unwrap();
    bridge.conn = Some(first.into());
    assert!(model.fire("Attach", &mut state));
    let (consumer, closer) = bridge.connect().unwrap();
    closer
        .set_read_timeout(Some(Duration::from_secs(60)))
        .unwrap();
    let mut sub = consumer
        .subscribe_group(&group, &subject::inbox_filter("f", &bridge.node))
        .unwrap();
    assert!(model.fire("Subscribe", &mut state));

    assert!(bridge.refresh_sessions().is_ok());
    assert!(bridge.per_sid.pending_admit.contains(SID));
    assert!(model.fire("Discover", &mut state));
    first_closer.close();
    bridge.admit_fresh_sessions();
    assert!(model.fire("FetchFails", &mut state));
    assert!(bridge.per_sid.pending_admit.contains(SID));
    assert!(bridge.conn.is_none());

    let (new, _) = writer
        .publish(82, 2, &subject, b"v=1 t=2 text=new")
        .unwrap();
    let live = next_delivery(&mut sub, "the live group offered new mail");
    assert_eq!(live.0, new);
    bridge.on_inbox_record(&live, None);
    assert!(!model.fire("DeliverNew", &mut state));
    assert_eq!(
        lane.delivery_offsets(),
        [],
        "new live mail cannot overtake a failed old-mail refill"
    );

    let stale_generation = bridge.mailbox.broker_generation(Source::Inbox);
    let _ = bridge.mailbox.push(
        Source::Inbox,
        live.clone(),
        None,
        stale_generation,
        bridge.fx.now(),
    );
    bridge.mailbox.reset_broker_sources();
    assert!(
        bridge
            .mailbox
            .take(Duration::ZERO, bridge.fx.now())
            .is_none(),
        "the previous subscription's queued copy is gone"
    );
    let (second, _) = bridge.connect().unwrap();
    bridge.conn = Some(second.into());
    assert!(model.fire("Attach", &mut state));
    assert!(bridge.refill_pending_admissions(false));
    assert!(model.fire("RefillCompletes", &mut state));
    assert_eq!(lane.delivery_offsets(), [old, new]);
    assert!(model.fire("DeliverNew", &mut state));
    bridge.on_inbox_record(&live, None);
    assert_eq!(
        lane.delivery_offsets(),
        [old, new],
        "a broker redelivery remains idempotent"
    );
    for name in ["RefillDebtSurvivesFailure", "OldBeforeLive", "OldBeforeNew"] {
        assert!(model.check_invariant(name, &state));
    }
}

struct FakePushLane {
    ours: std::os::unix::net::UnixStream,
    barriers: usize,
}

impl FakePushLane {
    fn attach(bridge: &Bridge) -> Self {
        use std::io::{BufRead, BufReader, Write};
        let (mut ours, theirs) = std::os::unix::net::UnixStream::pair().expect("a push lane");
        spawn_event_reader(
            Ctl::from_stream(theirs).expect("the reader's half"),
            bridge.mailbox.clone(),
        );
        let mut asked = String::new();
        BufReader::new(ours.try_clone().expect("a read half"))
            .read_line(&mut asked)
            .expect("the reader subscribes");
        assert_eq!(asked, "subscribe @* events,sessions\n");
        ours.write_all(b"OK subscribe 0\n").expect("the handshake");
        FakePushLane { ours, barriers: 0 }
    }

    fn write(&mut self, lines: &[&str]) {
        use std::io::Write;
        for line in lines {
            self.ours
                .write_all(format!("{line}\n").as_bytes())
                .expect("the push lane");
        }
    }

    fn park_through(&mut self, bridge: &mut Bridge, lines: &[&str]) {
        self.barriers += 1;
        let sid = format!("s-park-barrier-{}", self.barriers);
        let barrier = format!("sub {} {sid}", 1_000_000 + self.barriers);
        let mut all = lines.to_vec();
        all.push(&barrier);
        self.write(&all);
        let deadline = Instant::now() + Duration::from_secs(60);
        while !bridge.per_sid.watched.contains_key(&sid) {
            assert!(
                !bridge.park_detached(Duration::from_millis(20)),
                "aterm is still there"
            );
            assert!(
                Instant::now() < deadline,
                "the parked loop never took {barrier:?}"
            );
        }
        bridge.per_sid.watched.remove(&sid);
    }

    fn deliver(&mut self, bridge: &mut Bridge, lines: &[&str]) {
        use std::io::Write;
        self.barriers += 1;
        let barrier = format!("EVENT * test-barrier {}", self.barriers);
        for line in lines.iter().copied().chain([barrier.as_str()]) {
            self.ours
                .write_all(format!("{line}\n").as_bytes())
                .expect("the push lane");
        }
        let deadline = Instant::now() + Duration::from_secs(60);
        loop {
            match bridge
                .mailbox
                .take(Duration::from_millis(50), bridge.fx.now())
            {
                Some(Item::Event(line)) if line == barrier => return,
                Some(Item::Event(line)) => bridge.on_event(&line),
                Some(Item::Lost(source, end)) => panic!("{source:?} ended mid-test: {end:?}"),
                Some(_) => {}
                None => assert!(
                    Instant::now() < deadline,
                    "the reader never delivered {barrier:?}"
                ),
            }
        }
    }
}

fn holds(bridge: &Bridge, sid: &str, topic: &str) -> bool {
    bridge
        .per_sid
        .topics
        .get(sid)
        .is_some_and(|t| t.contains_key(topic))
}

#[test]
fn a_kept_sid_listed_after_the_attach_resumes_from_its_cursor_on_disk() {
    const S: &str = "s-s";
    const T: &str = "s-t";
    const U: &str = "s-u";
    const V: &str = "s-v";
    let (lane, mut bridge) = FakeVerbLane::bridge(&[(2, S), (3, T), (4, U), (5, V)]);
    let kept = BTreeMap::from([("builds".to_string(), TopicCursor { next: 91234 })]);
    for sid in [S, U, V] {
        bridge
            .state
            .set_topics(sid, &kept)
            .expect("a previous bridge's cursors");
    }
    assert!(
        bridge.per_sid.topics.is_empty(),
        "PRECONDITION: no attach loaded any cursor"
    );
    lane.opt_in(S, &["topic builds since=@5"]);
    lane.opt_in(T, &["topic builds since=@5"]);
    lane.refuse_reads(U, 1);
    lane.opt_in(U, &["topic builds since=@5", "topic other since=@7"]);
    let mut push = FakePushLane::attach(&bridge);
    push.deliver(&mut bridge, &["EVENT * session-created s-s watch=deferred"]);
    assert_eq!(lane.reads_of(S), 1, "REACH: the announcement read S");
    assert_eq!(
        (
            bridge.per_sid.topics[S]["builds"],
            bridge.state.topics(S)["builds"]
        ),
        (TopicCursor { next: 91234 }, TopicCursor { next: 91234 }),
        "the kept cursor resumes, in memory and on disk"
    );
    assert_eq!(
        bridge.per_sid.topics[T]["builds"],
        TopicCursor { next: 5 },
        "nothing on disk: the row's since= resolves"
    );
    assert!(
        !bridge.per_sid.topics.contains_key(V) && bridge.state.topics(V).is_empty(),
        "an endpoint that lists nothing for V removes V's cursors"
    );

    push.deliver(&mut bridge, &["EVENT 4 topic add other since=@7"]);
    assert_eq!(
        bridge.state.topics(U),
        BTreeMap::from([
            ("builds".to_string(), TopicCursor { next: 91234 }),
            ("other".to_string(), TopicCursor { next: 7 }),
        ]),
        "the pushed add keeps U's other cursor"
    );
    bridge.roster_backstop();
    assert_eq!(
        bridge.per_sid.topics[U]["builds"],
        TopicCursor { next: 91234 },
        "and U's later read resumes it"
    );
}

#[test]
fn inbox_seen_persists_the_endpoints_addressed_floor() {
    const T: &str = "s-t";
    let (_lane, mut bridge) = FakeVerbLane::bridge(&[(1, T)]);
    bridge.locals.insert(1, T.to_string());
    let mut push = FakePushLane::attach(&bridge);
    let mut seen = |line: &str| {
        push.deliver(&mut bridge, &[line]);
        bridge.state.seen_off(T)
    };
    assert_eq!(
        seen("EVENT 1 inbox-seen 1 off=1000 floor=-"),
        None,
        "no addressed row yet: nothing to persist"
    );
    assert_eq!(
        seen("EVENT 1 inbox-seen 2 off=1100 floor=900"),
        Some(899),
        "the unhandled task at 900 is refilled, whatever the acked row's offset"
    );
    assert_eq!(
        seen("EVENT 1 inbox-seen 5 off=1200 floor=2001"),
        Some(2000),
        "the handled task at 2000 is behind the floor"
    );
    assert_eq!(
        seen("EVENT 1 inbox-seen 6 off=3000 floor=0"),
        Some(2000),
        "a zero floor persists nothing"
    );
    assert_eq!(
        seen("EVENT 1 inbox-seen 6 off=3000 floor=junk"),
        Some(2000),
        "a floor that is no number persists nothing — never the row's offset"
    );
    assert_eq!(
        seen("EVENT 1 inbox-seen 7 off=1500"),
        Some(2000),
        "an older endpoint's `off=` is read, and the watermark never goes back"
    );
    assert_eq!(seen("EVENT 1 inbox-seen 8 off=2500"), Some(2500));
}

#[test]
fn an_observer_reads_each_session_once_and_does_not_poll() {
    const S: &str = "s-s";
    let (lane, mut bridge) = FakeVerbLane::bridge(&[(2, S)]);
    bridge.attachment = Attachment::Observer;
    bridge.roster_backstop();
    assert_eq!(lane.reads_of(S), 1, "REACH: the first round read S");
    bridge.roster_backstop();
    bridge.roster_backstop();
    assert_eq!(lane.reads_of(S), 1, "an observer polled a session");
}

const ANNOUNCEMENT_LAWS: [&str; 3] = [
    "NotAnnouncedBeforeItsWatchOrExit",
    "StoppedOnlyWhileWatched",
    "NoTopicAddLostSilently",
];

struct BridgeWorld {
    lane: FakeVerbLane,
    push: FakePushLane,
    bridge: Bridge,
    model: aterm_spec::derive::Model,
    state: aterm_spec::interp::State,
}

impl BridgeWorld {
    const LOCAL: u64 = 2;
    const S: &'static str = "s-s";
    const ACK: &'static str = "sub 2 s-s";
    const PUSH: &'static str = "EVENT 2 topic add probe since=@5";

    fn new() -> Self {
        let (lane, bridge) = FakeVerbLane::bridge(&[]);
        let push = FakePushLane::attach(&bridge);
        let model = aterm_models_session::subscribe_announcement_order_model();
        let state = model.init_state();
        let w = BridgeWorld {
            lane,
            push,
            bridge,
            model,
            state,
        };
        assert_eq!(w.project(), w.state, "the world starts at Init");
        w
    }

    fn project(&self) -> aterm_spec::interp::State {
        let b = &self.bridge;
        let acked = b.per_sid.watched.get(Self::S) == Some(&Self::LOCAL);
        let listed = b.locals.get(&Self::LOCAL).is_some_and(|sid| sid == Self::S);
        let mut s = self.state.clone();
        s.insert("listed", i64::from(listed));
        s.insert("acked", i64::from(acked));
        s.insert("known", i64::from(holds(b, Self::S, "probe")));
        s.insert(
            "stopped",
            i64::from(acked && b.per_sid.topics_sampled.contains(Self::S)),
        );
        s.insert("asking", 0);
        s.insert("seen", 0);
        s
    }

    fn step(
        &self,
        prev: &aterm_spec::interp::State,
        next: &aterm_spec::interp::State,
        action: &str,
    ) {
        let (ok, why) = aterm_spec::verify::validate_transition_tiered(
            &self.model,
            &[],
            prev,
            next,
            Some(action),
            &format!("bridge {action}"),
        );
        assert!(
            ok,
            "the real {action} is not the model's: {prev:?} -> {next:?}\n{why}"
        );
        for law in ANNOUNCEMENT_LAWS {
            assert!(
                self.model.check_invariant(law, next),
                "{law} fails at {next:?}"
            );
        }
    }

    fn endpoint(&mut self, action: &str) {
        assert!(
            self.model.fire(action, &mut self.state),
            "{action}: {:?}",
            self.state
        );
        for law in ANNOUNCEMENT_LAWS {
            assert!(self.model.check_invariant(law, &self.state), "{law}");
        }
        let mut rows = self.lane.sessions.lock().unwrap();
        rows.retain(|(local, _)| *local != Self::LOCAL);
        if self.state["live"] == 1 {
            rows.push((Self::LOCAL, Self::S.to_string()));
        }
        drop(rows);
        if action == "Add" {
            self.lane.opt_in(Self::S, &["topic probe since=@5"]);
        }
    }

    fn reading(&mut self, call: impl FnOnce(&mut Self)) -> Option<bool> {
        let before = self.lane.reads_of(Self::S);
        let refusing = self
            .lane
            .reads_refused
            .lock()
            .unwrap()
            .get(Self::S)
            .is_some_and(|left| *left > 0);
        call(self);
        match self.lane.reads_of(Self::S) - before {
            0 => None,
            1 => Some(!refusing),
            n => panic!("one call read S {n} times"),
        }
    }

    fn read_tail(
        &mut self,
        from: aterm_spec::interp::State,
        read: Option<bool>,
        after: aterm_spec::interp::State,
    ) {
        assert_eq!(
            self.model.action_enabled("Ask", &from),
            read.is_some(),
            "the bridge reads S exactly when the rule says it still reads it: {from:?}"
        );
        match read {
            None => assert_eq!(from, after, "no read, and nothing else moved"),
            Some(answered) => {
                let mut asking = from.clone();
                asking.insert("asking", 1);
                asking.insert("seen", from["added"]);
                self.step(&from, &asking, "Ask");
                self.step(&asking, &after, if answered { "Answer" } else { "Fail" });
            }
        }
        self.state = after;
    }

    fn round(&mut self) {
        let prev = self.state.clone();
        let roster_read = !self
            .lane
            .roster_refused
            .load(std::sync::atomic::Ordering::SeqCst);
        let read = self.reading(|w| w.bridge.roster_backstop());
        let after = self.project();
        let mut listed = prev.clone();
        if roster_read {
            listed.insert("listed", after["listed"]);
            if after["listed"] == 0 {
                for gone in ["acked", "stopped", "known"] {
                    listed.insert(gone, after[gone]);
                }
            }
            self.step(&prev, &listed, "Roster");
        }
        self.read_tail(listed, read, after);
    }

    fn hear_ack(&mut self) {
        assert_eq!(self.state["ackwire"], 1, "S's ack is on the wire");
        let prev = self.state.clone();
        let read = self.reading(|w| w.push.deliver(&mut w.bridge, &[Self::ACK]));
        self.state.insert("ackwire", 0);
        let after = self.project();
        let mut heard = prev.clone();
        heard.insert("ackwire", 0);
        heard.insert("acked", after["acked"]);
        heard.insert("stopped", 0);
        self.step(&prev, &heard, "HearAck");
        self.read_tail(heard, read, after);
    }

    fn hear_push(&mut self) {
        assert_eq!(self.state["pushed"], 1, "the add's push is on the wire");
        let prev = self.state.clone();
        let read = self.reading(|w| w.push.deliver(&mut w.bridge, &[Self::PUSH]));
        assert_eq!(read, None, "a pushed change is not a read");
        self.state.insert("pushed", 0);
        let after = self.project();
        self.step(&prev, &after, "HearPush");
        self.state = after;
    }

    fn outage(&mut self) {
        let prev = self.state.clone();
        let mut lines = Vec::new();
        if prev["ackwire"] == 1 {
            lines.push(Self::ACK);
        }
        if prev["pushed"] == 1 {
            lines.push(Self::PUSH);
        }
        self.push.park_through(&mut self.bridge, &lines);
        self.state.insert("ackwire", 0);
        self.state.insert("pushed", 0);
        let read = self.reading(|w| w.bridge.resample_topics());
        let after = self.project();
        let mut down = prev.clone();
        down.insert("ackwire", 0);
        down.insert("pushed", 0);
        down.insert("acked", after["acked"]);
        down.insert("stopped", 0);
        self.step(&prev, &down, "Outage");
        self.read_tail(down, read, after);
    }
}

#[test]
fn bridge_rounds_refine_the_announcement_model() {
    let mut w = BridgeWorld::new();
    for e in ["Register", "Read", "Drain", "Seed", "Write"] {
        w.endpoint(e);
    }
    assert_eq!(
        w.state["told"], 2,
        "REACH: S was announced `watch=deferred`"
    );
    w.lane.refuse_roster(true);
    w.round();
    w.lane.refuse_roster(false);
    w.round();
    w.endpoint("Add");
    w.round();
    w.round();
    assert_eq!(
        (w.state["known"], w.state["stopped"]),
        (1, 0),
        "REACH: learned by a round, and still read"
    );

    let mut w = BridgeWorld::new();
    w.endpoint("Register");
    w.round();
    for e in ["Read", "Drain", "Seed", "Write", "Add", "FreeSlot"] {
        w.endpoint(e);
    }
    for e in ["Read", "Drain", "Seed", "Write"] {
        w.endpoint(e);
    }
    assert_eq!(w.state["ackwire"], 1, "REACH: the freed slot adopted S");
    w.lane.refuse_reads(BridgeWorld::S, 1);
    w.hear_ack();
    assert_eq!(
        (w.state["acked"], w.state["known"]),
        (1, 0),
        "REACH: the ack's read was refused"
    );
    w.lane.refuse_reads(BridgeWorld::S, 1);
    w.round();
    assert_eq!(w.state["known"], 0, "REACH: the round's read was refused");
    w.round();
    w.round();
    assert_eq!((w.state["known"], w.state["stopped"]), (1, 1));

    let mut w = BridgeWorld::new();
    w.endpoint("FreeSlot");
    w.endpoint("Register");
    w.round();
    for e in ["Read", "Drain", "Seed", "Write", "Add"] {
        w.endpoint(e);
    }
    assert_eq!(
        (w.state["ackwire"], w.state["pushed"]),
        (1, 1),
        "REACH: both lines are on the wire"
    );
    w.outage();
    assert_eq!(
        (w.state["acked"], w.state["known"], w.state["stopped"]),
        (1, 1, 1)
    );
    w.round();

    let mut w = BridgeWorld::new();
    for e in ["FreeSlot", "Register", "Read", "Drain", "Seed", "Write"] {
        w.endpoint(e);
    }
    w.hear_ack();
    w.round();
    w.endpoint("Add");
    w.hear_push();
    assert_eq!((w.state["known"], w.state["stopped"]), (1, 1));
    w.endpoint("Exit");
    w.round();
    assert_eq!(
        (w.state["listed"], w.state["acked"], w.state["known"]),
        (0, 0, 0)
    );

    for refused in [false, true] {
        let mut w = BridgeWorld::new();
        w.endpoint("Register");
        if refused {
            w.endpoint("Add");
            w.lane.refuse_reads(BridgeWorld::S, 1);
        }
        let _ = w.bridge.refresh_sessions();
        let mut listed = w.state.clone();
        listed.insert("listed", 1);
        assert_eq!(w.project(), listed, "REACH: the roster lists S");
        let read = w.reading(|w| {
            let _ = w.bridge.sample_topics(BridgeWorld::S);
            w.bridge
                .per_sid
                .topics_sampled
                .insert(BridgeWorld::S.to_string());
        });
        assert_eq!(read, Some(!refused), "REACH: S was read once");
        let mut asking = listed.clone();
        asking.insert("asking", 1);
        asking.insert("seen", listed["added"]);
        let mut once = asking.clone();
        once.insert("asking", 0);
        once.insert("seen", 0);
        once.insert("stopped", 1);
        let action = if refused { "Fail" } else { "Answer" };
        let admits = |overrides: &[(&'static str, i64)]| {
            aterm_spec::verify::validate_transition_tiered(
                &w.model,
                overrides,
                &asking,
                &once,
                Some(action),
                "bridge read-once",
            )
        };
        assert!(
            !admits(&[]).0,
            "refused={refused}: the healthy machine admitted the read-once rule"
        );
        let (old, why) = admits(&[("Buggy", 1)]);
        assert!(old, "refused={refused}: Buggy=1 must reproduce it\n{why}");
        let buggy = aterm_spec::interp::with_buggy(&w.model, 1);
        let mut end = once.clone();
        if !refused {
            assert!(buggy.fire("Add", &mut end));
        }
        assert!(
            !buggy.check_invariant("NoTopicAddLostSilently", &end),
            "refused={refused}: an add read by nobody and pushed by nobody: {end:?}"
        );
    }
}

#[cfg(unix)]
fn advertising(
    sessions: &[(u64, &str)],
) -> (
    FakeVerbLane,
    Bridge,
    FakePushLane,
    Conn,
    astream_broker::BrokerHandle,
) {
    let (lane, mut bridge) = FakeVerbLane::bridge(sessions);
    let socket = lane.dir.path().join("broker.sock");
    let broker = astream_broker::Broker::open(lane.dir.path().join("broker.log")).unwrap();
    let server = broker.serve(&socket).unwrap();
    bridge.cfg.broker = socket.to_string_lossy().into_owned();
    let (conn, _) = bridge.connect().unwrap();
    bridge.conn = Some(conn.into());
    let push = FakePushLane::attach(&bridge);
    bridge.refresh_sessions().unwrap();
    bridge.admit_fresh_sessions();
    let (reader, _) = bridge.connect().unwrap();
    (lane, bridge, push, reader, server)
}

#[cfg(unix)]
fn presence_states(bridge: &Bridge, reader: &mut Conn, sid: &str) -> Vec<String> {
    let face = subject::session_face("f", &bridge.node, sid, "presence");
    let (rows, _) = reader.fetch(0, &face, 256).unwrap();
    rows.iter()
        .filter(|(_, s, _)| *s == face)
        .filter_map(|(_, _, raw)| Body::decode(raw).0.unknown.get("state").cloned())
        .collect()
}

#[cfg(unix)]
fn node_ev_words(bridge: &Bridge, reader: &mut Conn) -> Vec<String> {
    let face = subject::node_face("f", &bridge.node, "ev");
    let (rows, _) = reader.fetch(0, &face, 256).unwrap();
    rows.iter()
        .filter(|(_, s, _)| *s == face)
        .filter_map(|(_, _, raw)| {
            let ev = Body::decode(raw).0.unknown.get("ev")?.clone();
            Some(crate::pct::decode(&ev))
        })
        .collect()
}

#[cfg(unix)]
#[test]
fn a_quit_withdraws_every_session_once_and_a_handoff_withdraws_none() {
    const A: &str = "s-a";
    const B: &str = "s-b";
    const C: &str = "s-c";
    let (lane, mut bridge, mut push, mut reader, _server) = advertising(&[(1, A), (2, B)]);
    for sid in [A, B] {
        assert_eq!(presence_states(&bridge, &mut reader, sid), ["live"]);
    }

    push.deliver(&mut bridge, &["EVENT * bye handoff"]);
    assert!(lane
        .links
        .lock()
        .unwrap()
        .contains(&"link bye handoff".to_string()));
    for sid in [A, B] {
        assert_eq!(
            presence_states(&bridge, &mut reader, sid),
            ["live"],
            "a handoff's successor re-attaches {sid}: nothing is withdrawn"
        );
    }
    assert!(node_ev_words(&bridge, &mut reader).contains(&"bye reason=handoff".to_string()));

    push.deliver(&mut bridge, &["EVENT * bye quit"]);
    assert!(lane
        .links
        .lock()
        .unwrap()
        .contains(&"link bye quit".to_string()));
    for sid in [A, B] {
        assert_eq!(
            presence_states(&bridge, &mut reader, sid),
            ["live", "exited"]
        );
    }
    push.deliver(&mut bridge, &["EVENT 1 agent busy rev=2 gen=0.1 fp=0"]);
    bridge.roster_backstop();
    assert!(bridge.attach_broker(), "the re-attach");
    lane.sessions.lock().unwrap().push((3, C.to_string()));
    bridge.refresh_sessions().unwrap();
    bridge.admit_fresh_sessions();
    lane.sessions.lock().unwrap().retain(|(_, sid)| sid != A);
    push.deliver(&mut bridge, &[&format!("EVENT * session-exited {A}")]);
    for sid in [A, B] {
        assert_eq!(
            presence_states(&bridge, &mut reader, sid),
            ["live", "exited"],
            "{sid} was advertised again, or withdrawn twice, after the quit"
        );
    }
    assert_eq!(
        presence_states(&bridge, &mut reader, C),
        Vec::<String>::new(),
        "a session born after the quit was advertised"
    );
    let quits = |bridge: &Bridge, reader: &mut Conn| -> Vec<String> {
        node_ev_words(bridge, reader)
            .into_iter()
            .filter(|e| e.starts_with("bye reason=quit"))
            .collect()
    };
    assert_eq!(quits(&bridge, &mut reader), ["bye reason=quit withdrawn=2"]);

    bridge.on_aterm_closed();
    assert_eq!(presence_states(&bridge, &mut reader, C), ["exited"]);
    for sid in [A, B] {
        assert_eq!(
            presence_states(&bridge, &mut reader, sid),
            ["live", "exited"]
        );
    }
    assert_eq!(
        quits(&bridge, &mut reader),
        ["bye reason=quit withdrawn=2", "bye reason=quit withdrawn=3"],
        "the lanes closing withdraws the session born since, and says the new count"
    );
}

/// **A QUIT UNDER A HOLD BEHIND THE RATE LIMIT WITHDRAWS ITS SESSIONS** (critic item E06's
/// review, `Bridge::publish_withdrawal`): `s-a` and `s-b` are advertised `live`; the broker
/// is then limited to a record a second, another write of the bridge's producer spends the
/// bucket, and the bridge reads the refusal of its next write, so every publish of its
/// producer is held. aterm quits (`EVENT * bye quit`, then the lanes close): each
/// `state=exited` row waits out the hold, and the refusal for the rate after it, and lands.
///
/// NEGATIVE CONTROL: with a withdrawal that waits nothing
/// ([`crate::sim::world::Mutant::WithdrawalWaitsNothing`]), each row is held back unsent, at
/// the bye and again at the close, and both sessions stay `live`.
#[cfg(unix)]
#[test]
fn a_quit_under_a_rate_hold_withdraws_its_sessions() {
    let exited = vec!["live".to_string(), "exited".to_string()];
    assert_eq!(
        quit_under_a_rate_hold(crate::fx::Fx::os()),
        [exited.clone(), exited],
        "each session is withdrawn by the quit"
    );
    let live = vec!["live".to_string()];
    assert_eq!(
        quit_under_a_rate_hold(crate::fx::Fx::os_without(
            crate::sim::world::Mutant::WithdrawalWaitsNothing
        )),
        [live.clone(), live],
        "NEGATIVE CONTROL: no row waits out the hold, and each stays live"
    );
}

/// The scenario of [`a_quit_under_a_rate_hold_withdraws_its_sessions`] under `fx`: each
/// session's presence states, `s-a`'s then `s-b`'s.
#[cfg(unix)]
fn quit_under_a_rate_hold(fx: crate::fx::Fx) -> [Vec<String>; 2] {
    const A: &str = "s-a";
    const B: &str = "s-b";
    let (lane, mut bridge) = FakeVerbLane::bridge(&[(1, A), (2, B)]);
    bridge.fx = fx;
    let socket = lane.dir.path().join("broker.sock");
    let broker = astream_broker::Broker::open(lane.dir.path().join("broker.log")).unwrap();
    let _server = broker.serve(&socket).unwrap();
    bridge.cfg.broker = socket.to_string_lossy().into_owned();
    let (conn, _) = bridge.connect().unwrap();
    bridge.conn = Some(conn.into());
    let mut push = FakePushLane::attach(&bridge);
    bridge.refresh_sessions().unwrap();
    bridge.admit_fresh_sessions();
    let (mut reader, _) = bridge.connect().unwrap();
    for sid in [A, B] {
        assert_eq!(presence_states(&bridge, &mut reader, sid), ["live"]);
    }
    broker
        .set_rate_limit(Some(astream_broker::RateLimit::records(1, 1).unwrap()))
        .unwrap();
    spend_the_bucket(&bridge, 1, 16);
    bridge.publish_ev("the bucket is spent");
    bridge.settle_acks();
    assert!(
        broker.stats().rate_limited > 0 && bridge.rate_held().is_some(),
        "PRECONDITION: the limit refused the ev and holds the producer"
    );
    push.deliver(&mut bridge, &["EVENT * bye quit"]);
    bridge.on_aterm_closed();
    [A, B].map(|sid| presence_states(&bridge, &mut reader, sid))
}

#[cfg(unix)]
#[test]
fn a_refused_withdrawal_row_stops_only_itself() {
    const A: &str = "s-a";
    const B: &str = "s-b";
    const C: &str = "s-c";
    let secret = [7u8; 32];
    let (lane, mut bridge) = FakeVerbLane::bridge(&[(1, A), (2, B), (3, C)]);
    let socket = lane.dir.path().join("broker.sock");
    let broker =
        astream_broker::Broker::open_guarded(lane.dir.path().join("broker.log"), secret.to_vec())
            .unwrap();
    let _server = broker.serve(&socket).unwrap();
    bridge.cfg.broker = socket.to_string_lossy().into_owned();
    let node = bridge.node.clone();
    bridge.caps = [
        format!("rw,p={node}:/f/f/pub/{node}/node/>"),
        format!("rw,p={node}:/f/f/pub/{node}/{A}/>"),
        format!("rw,p={node}:/f/f/pub/{node}/{C}/>"),
        "ro:/f/f/pub/>".to_string(),
        format!("ro:/f/f/in/{node}/>"),
    ]
    .iter()
    .map(|grant| {
        let cap = astream_cap::mint(&secret, grant).unwrap();
        Cap {
            grant: cap.filter,
            tag: cap.tag,
        }
    })
    .collect();
    let (conn, _) = bridge.connect().unwrap();
    bridge.conn = Some(conn.into());
    let mut push = FakePushLane::attach(&bridge);
    bridge.refresh_sessions().unwrap();
    bridge.admit_fresh_sessions();
    let (mut reader, _) = bridge.connect().unwrap();
    for (sid, rows) in [(A, &["live"][..]), (B, &[]), (C, &["live"])] {
        assert_eq!(presence_states(&bridge, &mut reader, sid), rows, "{sid}");
    }

    push.deliver(&mut bridge, &["EVENT * bye quit"]);
    assert!(bridge.conn.is_some(), "a refusal keeps the link");
    for (sid, rows) in [
        (A, &["live", "exited"][..]),
        (B, &[]),
        (C, &["live", "exited"]),
    ] {
        assert_eq!(presence_states(&bridge, &mut reader, sid), rows, "{sid}");
    }
    let evs = node_ev_words(&bridge, &mut reader);
    assert_eq!(
        evs.iter()
            .filter(|e| e.starts_with("bye reason=quit"))
            .collect::<Vec<_>>(),
        ["bye reason=quit withdrawn=2"],
        "{evs:#?}"
    );
}

#[cfg(unix)]
#[test]
fn a_quit_the_loop_never_took_is_withdrawn_when_the_lanes_close() {
    const A: &str = "s-a";
    let (_lane, mut bridge, mut push, mut reader, _server) = advertising(&[(1, A)]);
    bridge.on_aterm_closed();
    assert_eq!(presence_states(&bridge, &mut reader, A), ["live"]);

    push.write(&["EVENT * bye quit"]);
    drop(push);
    let deadline = Instant::now() + Duration::from_secs(60);
    loop {
        match bridge
            .mailbox
            .take(Duration::from_millis(50), bridge.fx.now())
        {
            Some(Item::Closed(Source::Aterm)) => break,
            Some(_) => {}
            None => assert!(Instant::now() < deadline, "the lane never closed"),
        }
    }
    assert_eq!(bridge.mailbox.bye(), Some(Bye::Quit));
    bridge.on_aterm_closed();
    assert_eq!(presence_states(&bridge, &mut reader, A), ["live", "exited"]);
}

#[cfg(unix)]
#[test]
fn only_a_withdrawn_address_is_refused_and_an_away_one_routes() {
    let (lane, mut bridge) = FakeVerbLane::bridge(&[]);
    let socket = lane.dir.path().join("broker.sock");
    let broker = astream_broker::Broker::open(lane.dir.path().join("broker.log")).unwrap();
    let _server = broker.serve(&socket).unwrap();
    bridge.cfg.broker = socket.to_string_lossy().into_owned();
    let (conn, _) = bridge.connect().unwrap();
    bridge.conn = Some(conn.into());
    let (mut peer, _) = bridge.connect().unwrap();
    let mut seq = 0;
    let mut row = |subject: &str, body: &str| {
        seq += 1;
        peer.publish(9_000, seq, subject, body.as_bytes()).unwrap();
    };
    row("/f/f/pub/n-far/node/presence", "v=1 t=1 state=away inc=3");
    row("/f/f/pub/n-far/s-away/presence", "v=1 t=1 inc=3 state=live");
    row("/f/f/pub/n-far/s-quit/presence", "v=1 t=1 inc=3 state=live");
    for sid in ["s-away", "s-quit"] {
        assert_eq!(
            bridge.resolve_to(&format!("@{sid}"), Some("s-me")).unwrap(),
            Route::To(format!("/f/f/in/n-far/{sid}/{}", bridge.node))
        );
    }
    row(
        "/f/f/pub/n-far/s-quit/presence",
        "v=1 t=2 inc=3 state=exited",
    );
    assert_eq!(
        bridge.resolve_to("@s-away", Some("s-me")).unwrap(),
        Route::To(format!("/f/f/in/n-far/s-away/{}", bridge.node)),
        "an away node's session still routes: its mail queues for the node"
    );
    assert_eq!(
        bridge.resolve_to("@s-quit", Some("s-me")).unwrap(),
        Route::Unroutable,
        "a pinned sid its node withdrew routed to a lane nobody fetches"
    );
    assert_eq!(
        bridge.resolve_to("@s-quit@n-far", Some("s-me")).unwrap(),
        Route::Unroutable,
        "an address naming the node that withdrew it routed all the same"
    );
    assert_eq!(
        bridge.resolve_to("@s-away@n-far", Some("s-me")).unwrap(),
        Route::To(format!("/f/f/in/n-far/s-away/{}", bridge.node))
    );
}

#[test]
fn the_node_is_the_state_dirs_and_a_cap_for_another_node_is_refused() {
    let ring = |node: &str| -> Vec<Cap> {
        crate::enable::node_rules("f1", node)
            .into_iter()
            .map(|grant| Cap {
                grant,
                tag: [0; 32],
            })
            .collect()
    };
    let person: Vec<Cap> = crate::enable::person_rules("f1", "h-x")
        .into_iter()
        .map(|grant| Cap {
            grant,
            tag: [0; 32],
        })
        .collect();
    let at = |tag: &str, node: Option<&str>| {
        let dir = crate::test_scratch_dir(tag);
        let state = StateDir::open(dir.path()).expect("state dir");
        if let Some(n) = node {
            std::fs::write(dir.path().join("node"), format!("{n}\n")).expect("seed node");
        }
        (dir, state)
    };
    let (s, n) = ("n-00000000000000bb", "n-00000000000000aa");

    let (_dir, state) = at("same", Some(s));
    assert_eq!(node_for(&state, &ring(s)).unwrap(), s);
    let mut both = ring(s);
    both.extend(person.clone());
    assert_eq!(node_for(&state, &both).unwrap(), s);
    let e = node_for(&state, &ring(n)).unwrap_err();
    assert_eq!(e.kind(), io::ErrorKind::InvalidInput);
    assert!(
        e.to_string().contains(s) && e.to_string().contains(n),
        "{e}"
    );
    assert_eq!(state.node().as_deref(), Some(s));

    let (_dir, state) = at("empty", None);
    let e = node_for(&state, &ring(n)).unwrap_err();
    assert!(e.to_string().contains(n), "{e}");
    assert_eq!(state.node(), None, "a refused start writes no node id");
    let minted = node_for(&state, &person).unwrap();
    assert!(crate::enable::is_node_id(&minted), "{minted}");
    assert_eq!(node_for(&state, &[]).unwrap(), minted);

    let (_dir, state) = at("person", Some("h-x"));
    assert!(node_for(&state, &[]).is_err());
    assert!(node_for(&state, &person).is_err());
}

#[cfg(unix)]
#[test]
fn the_deadline_sweep_counts_a_receipt_only_from_the_node_the_ask_went_to() {
    const SID: &str = "s-asker";
    let (lane, mut bridge) = FakeVerbLane::bridge(&[]);
    let socket = lane.dir.path().join("broker.sock");
    let broker = astream_broker::Broker::open(lane.dir.path().join("broker.log")).unwrap();
    let _server = broker.serve(&socket).unwrap();
    bridge.cfg.broker = socket.to_string_lossy().into_owned();
    let (conn, _) = bridge.connect().unwrap();
    bridge.conn = Some(conn.into());
    bridge
        .per_sid
        .epochs
        .insert(SID.to_string(), "e".to_string());
    let node = bridge.node.clone();
    let (mut peer, _) = bridge.connect().unwrap();

    let mut asks = Vec::new();
    for seq in 1..=3 {
        let lane = format!("/f/f/in/n-far/s-far/{node}/task");
        let (off, _) = peer.publish(9_100, seq, &lane, b"v=1 t=1 text=q").unwrap();
        bridge.note_deadline(SID, off, 1_000, Some("n-far".to_string()));
        asks.push(off);
    }
    let mut seq = 0;
    let mut reply = |src: &str, kind: &str, re: u64| {
        let mut body = Body::new(1);
        body.re = Some(re);
        body.verdict = (kind == "ack").then(|| "handled".to_string());
        seq += 1;
        let subject = format!("/f/f/in/{node}/{SID}/{src}/{kind}");
        peer.publish(9_200, seq, &subject, &body.encode()).unwrap();
    };
    reply("n-evil", "ack", asks[0]);
    reply("n-evil", "expired", asks[1]);
    reply("n-far", "ack", asks[2]);

    bridge.expire_deadlines(true);

    let verdicts = format!("/f/f/in/{node}/{SID}/{node}/expired");
    let (rows, _) = peer.fetch(0, &verdicts, 64).unwrap();
    let expired: Vec<u64> = rows
        .iter()
        .filter(|(_, s, _)| *s == verdicts)
        .filter_map(|(_, _, raw)| Body::decode(raw).0.re)
        .collect();
    assert_eq!(
        expired,
        [asks[0], asks[1]],
        "a stray receipt settled a deadline, or the recipient's did not"
    );
    assert!(bridge.deadlines.is_empty(), "{:?}", bridge.deadlines);
}

/// B12 — THE DEADLINE SWEEP TELLS THE ASKER WHAT RETENTION GAVE UP UNDER ITS JUDGEMENT: an
/// ask with a deadline, its answer on the asker's lane, then the log cut past the answer. The
/// sweep finds no answer in what the broker keeps and records `expired`, and the asker is
/// told `link gap from=<ask+1> to=<floor> sid=<asker>`: the expiry was judged over a range
/// the broker let go of. An asker whose `seen` is past the floor handled the range, so it is
/// told nothing. NC (run): a `tell_given_up` that returns at once fails it.
#[cfg(unix)]
#[test]
fn the_deadline_sweep_tells_the_asker_what_retention_gave_up_under_its_verdict() {
    const SID: &str = "s-asker";
    let run = |seen_past_the_answer: bool| {
        let (lane, mut bridge) = FakeVerbLane::bridge(&[]);
        bridge.endpoint_lane = aterm_types::control_verbs::BRIDGE_LANE;
        let socket = lane.dir.path().join("broker.sock");
        let broker = astream_broker::Broker::open(lane.dir.path().join("broker.log")).unwrap();
        let _server = broker.serve(&socket).unwrap();
        bridge.cfg.broker = socket.to_string_lossy().into_owned();
        let (conn, _) = bridge.connect().unwrap();
        bridge.conn = Some(conn.into());
        bridge
            .per_sid
            .epochs
            .insert(SID.to_string(), "e".to_string());
        let node = bridge.node.clone();
        let (mut peer, _) = bridge.connect().unwrap();
        let ask_lane = format!("/f/f/in/n-far/s-far/{node}/ask");
        let (ask, _) = peer
            .publish(9_100, 1, &ask_lane, b"v=1 t=1 text=q")
            .unwrap();
        bridge.note_deadline(SID, ask, 1_000, Some("n-far".to_string()));
        let mut body = Body::new(1);
        body.re = Some(ask);
        let answer_lane = format!("/f/f/in/{node}/{SID}/n-far/answer");
        let (answer, _) = peer
            .publish(9_200, 1, &answer_lane, &body.encode())
            .unwrap();
        let (after, _) = peer.publish(9_300, 1, "/f/f/x/after", b"v=1").unwrap();
        assert!(answer < after);
        broker
            .retain_before_carrying_state(answer + 1)
            .expect("the cut");
        if seen_past_the_answer {
            bridge.state.set_seen_off(SID, answer).unwrap();
        }

        bridge.expire_deadlines(true);

        let verdicts = format!("/f/f/in/{node}/{SID}/{node}/expired");
        let (rows, _) = peer.fetch(answer + 1, &verdicts, 64).unwrap();
        let expired: Vec<u64> = rows
            .iter()
            .filter(|(_, s, _)| *s == verdicts)
            .filter_map(|(_, _, raw)| Body::decode(raw).0.re)
            .collect();
        assert_eq!(expired, [ask], "judged on what the broker keeps");
        (ask, answer, told_gaps(&lane))
    };

    let (ask, answer, told) = run(false);
    assert_eq!(
        told,
        [format!(
            "link gap from={} to={} sid={SID}",
            ask + 1,
            answer + 1
        )]
    );
    let (_, _, told) = run(true);
    assert!(told.is_empty(), "a handled range is not news: {told:?}");
}

/// **THE DEADLINE SWEEP FINDS AN ANSWER HELD BELOW THE ASKER'S OLDEST** (F3 review), on a
/// real broker: an ask with a deadline, its answer on the asker's lane, carried below the
/// base by a cut and held there while the rules set next take its class whole from the
/// base alone, so the lane's oldest lies above it. The sweep meets the oldest, reads again
/// told, finds the answer and settles the deadline: no `expired`, and no give-up told.
/// CONTROL (`Mutant::LaneReadWithoutTheHeld`, the read going on at the floor, as before the
/// review): the answer is skipped, the ask judged `expired`, and the asker told `link gap`
/// over the answer the broker still holds.
#[cfg(unix)]
#[test]
fn the_deadline_sweep_finds_an_answer_held_below_the_askers_oldest() {
    const SID: &str = "s-asker";
    for control in [false, true] {
        let (lane, mut bridge) = FakeVerbLane::bridge(&[]);
        bridge.endpoint_lane = aterm_types::control_verbs::BRIDGE_LANE;
        let (broker, _server) = held_lane_rig(&lane, &mut bridge, "sweep-held");
        bridge
            .per_sid
            .epochs
            .insert(SID.to_string(), "e".to_string());
        if control {
            bridge.fx =
                crate::fx::Fx::os_without(crate::sim::world::Mutant::LaneReadWithoutTheHeld);
        }
        let node = bridge.node.clone();
        let (mut peer, _) = bridge.connect().unwrap();
        let ask_lane = format!("/f/f/in/n-far/s-far/{node}/ask");
        let (ask, _) = peer
            .publish(9_100, 1, &ask_lane, b"v=1 t=1 text=q")
            .unwrap();
        bridge.note_deadline(SID, ask, 1_000, Some("n-far".to_string()));
        let mut body = Body::new(1);
        body.re = Some(ask);
        let answer_lane = format!("/f/f/in/{node}/{SID}/n-far/answer");
        let (answer, _) = peer
            .publish(9_200, 1, &answer_lane, &body.encode())
            .unwrap();
        carry_below_the_lanes_oldest(&broker, &format!("/f/f/in/{node}/{SID}/>"), answer + 1);

        bridge.expire_deadlines(true);

        let verdicts = format!("/f/f/in/{node}/{SID}/{node}/expired");
        let ((rows, _), _) = transport::fetch_told_kept(&mut peer, 0, &verdicts, 64).unwrap();
        let expired: Vec<u64> = rows
            .iter()
            .filter(|(_, s, _)| *s == verdicts)
            .filter_map(|(_, _, raw)| Body::decode(raw).0.re)
            .collect();
        assert!(bridge.deadlines.is_empty(), "{:?}", bridge.deadlines);
        if control {
            assert_eq!(expired, [ask], "CONTROL: the held answer skipped");
            assert_eq!(
                told_gaps(&lane).len(),
                1,
                "CONTROL: told a give-up over the answer the broker holds: {:?}",
                told_gaps(&lane)
            );
            continue;
        }
        assert!(
            expired.is_empty(),
            "the held answer settles it: {expired:?}"
        );
        assert!(
            told_gaps(&lane).is_empty(),
            "nothing was given up: {:?}",
            told_gaps(&lane)
        );
    }
}

/// **A HELD ASK'S RETRY ARMS NO DEADLINE THE HOLD SAW SETTLED** (F4 review), on a real
/// broker: an ask with `dl=` posted while the state dir refuses the deadline table is left
/// queued, its deadline in memory (`Bridge::landing_line`). While it is held the disk takes
/// writes again and the ask is settled: its answer delivered, or the sweep's `expired`
/// published. The retry at its pin retires the post and arms no deadline for the settled
/// ask, which a sweep would expire once retention gave the answer, or the `expired`, up:
/// an `expired` after the answer the asker was handed, or a second one.
#[cfg(unix)]
#[test]
fn a_held_asks_retry_arms_no_deadline_the_hold_saw_settled() {
    const SID: &str = "s-host";
    for answered in [true, false] {
        let case = if answered { "answered" } else { "expired" };
        let (lane, mut bridge) = FakeVerbLane::bridge(&[(1, SID)]);
        let (socket, _broker, _server) = serve_log(&lane, "held-ask");
        bridge.cfg.broker = socket;
        bridge.conn = Some(bridge.connect().unwrap().0.into());
        assert!(bridge.refresh_sessions().is_ok());
        lane.retire_from_outbox();
        bridge.state.refuse_writes_of(Some("deadlines"));
        *lane.outbox.lock().unwrap() =
            Some("post sid=s-host id=1 to=h-a kind=ask dl=60000 len=1\nq".to_string());
        bridge.drain_outbox();
        assert!(
            lane.sent.lock().unwrap().is_empty(),
            "{case}: held while its deadline is not on disk"
        );
        let ask = *bridge
            .deadlines
            .keys()
            .next()
            .expect("the held ask's deadline, in memory");
        bridge.state.refuse_writes_of(None);
        let node = bridge.node.clone();
        if answered {
            let mut body = Body::new(1);
            body.re = Some(ask);
            body.text = "a".to_string();
            let subject = format!("/f/f/in/{node}/{SID}/h-a/answer");
            let (mut peer, _) = bridge.connect().unwrap();
            let (off, _) = peer.publish(9_200, 1, &subject, &body.encode()).unwrap();
            bridge.deliver_record(off, &subject, &body.encode(), None);
            assert!(
                lane.delivery_offsets().contains(&off),
                "{case}: the answer is delivered"
            );
        } else {
            bridge.expire_deadlines(true);
            let verdicts = format!("/f/f/in/{node}/{SID}/{node}/expired");
            let (rows, _) = bridge.connect().unwrap().0.fetch(0, &verdicts, 8).unwrap();
            assert_eq!(rows.len(), 1, "{case}: the sweep expired the held ask");
        }
        assert!(
            bridge.deadlines.is_empty(),
            "{case}: settled while its post was held: {:?}",
            bridge.deadlines
        );

        bridge.drain_outbox();

        assert!(
            lane.sent
                .lock()
                .unwrap()
                .iter()
                .any(|l| l.starts_with(&format!("outbox sent {SID} 1 off={ask}"))),
            "{case}: the retry retires the post at its pin: {:?}",
            lane.sent.lock().unwrap()
        );
        assert!(
            bridge.deadlines.is_empty() && bridge.state.deadlines().is_empty(),
            "{case}: the retry armed the settled ask's deadline again: {:?}",
            bridge.deadlines
        );
    }
}

/// **A HELD ASK'S RETRY BY THE NEXT BRIDGE ARMS NO DEADLINE THE HOLD SAW SETTLED** (C04,
/// closing F4's residual), on a real broker: as above, an ask with `dl=` is held while the
/// state dir refuses its deadline, and its answer is delivered while it is held; then the
/// bridge is killed, and the next bridge on the same state dir retries the post at its pin.
/// The settle is the asks journal's, replayed at the next open, so the retry retires the
/// post and arms nothing. CONTROL (`Mutant::ArmOverASettle`, the journal's settle not
/// consulted, as a bridge before C04 held the hold in memory only): the retry arms the
/// answered ask's deadline again, which a sweep expires once retention gave the answer up.
#[cfg(unix)]
#[test]
fn a_held_asks_retry_after_a_kill_arms_no_deadline_the_hold_saw_settled() {
    const SID: &str = "s-host";
    const POST: &str = "post sid=s-host id=1 to=h-a kind=ask dl=60000 len=1\nq";
    for control in [false, true] {
        let (lane, mut bridge) = FakeVerbLane::bridge(&[(1, SID)]);
        let state = bridge.state.root().to_path_buf();
        let (socket, _broker, _server) = serve_log(&lane, "held-ask-kill");
        bridge.cfg.broker = socket.clone();
        bridge.conn = Some(bridge.connect().unwrap().0.into());
        assert!(bridge.refresh_sessions().is_ok());
        lane.retire_from_outbox();
        bridge.state.refuse_writes_of(Some("deadlines"));
        *lane.outbox.lock().unwrap() = Some(POST.to_string());
        bridge.drain_outbox();
        assert!(
            lane.sent.lock().unwrap().is_empty(),
            "held while its deadline is not on disk"
        );
        let ask = *bridge
            .deadlines
            .keys()
            .next()
            .expect("the held ask's deadline, in memory");
        bridge.state.refuse_writes_of(None);
        let node = bridge.node.clone();
        let mut body = Body::new(1);
        body.re = Some(ask);
        body.text = "a".to_string();
        let subject = format!("/f/f/in/{node}/{SID}/h-a/answer");
        let (mut peer, _) = bridge.connect().unwrap();
        let (off, _) = peer.publish(9_200, 1, &subject, &body.encode()).unwrap();
        bridge.deliver_record(off, &subject, &body.encode(), None);
        assert!(
            lane.delivery_offsets().contains(&off),
            "the answer is delivered"
        );
        assert!(
            bridge.deadlines.is_empty(),
            "settled while its post was held"
        );
        drop(bridge);

        let (next_lane, mut next) = FakeVerbLane::bridge_with_state(&[(1, SID)], Some(&state));
        if control {
            next.fx = crate::fx::Fx::os_without(crate::sim::world::Mutant::ArmOverASettle);
        }
        next.cfg.broker = socket;
        next.conn = Some(next.connect().unwrap().0.into());
        assert!(next.refresh_sessions().is_ok());
        next_lane.retire_from_outbox();
        *next_lane.outbox.lock().unwrap() = Some(POST.to_string());
        next.drain_outbox();
        assert!(
            next_lane
                .sent
                .lock()
                .unwrap()
                .iter()
                .any(|l| l.starts_with(&format!("outbox sent {SID} 1 off={ask}"))),
            "control={control}: the next bridge's retry retires the post at its pin: {:?}",
            next_lane.sent.lock().unwrap()
        );
        if control {
            assert!(
                next.deadlines.contains_key(&ask),
                "CONTROL: the answered ask's deadline is armed again"
            );
            continue;
        }
        assert!(
            next.deadlines.is_empty() && next.state.deadlines().is_empty(),
            "the retry armed the settled ask's deadline again: {:?}",
            next.deadlines
        );
    }
}

/// **AN ANSWER THE NEXT BRIDGE DELIVERS AHEAD OF THE RETRY ARMS NO DEADLINE** (C04 review), on
/// a real broker: an ask with `dl=` is held while the state dir refuses its deadline, so the
/// journal holds none, and the bridge is killed. The next bridge reads the answer first, as a
/// real attach does (`Bridge::refill_pending_admissions` ahead of the drain). Its retry then
/// writes the settle and arms nothing, where a sweep would expire the answered ask once
/// retention gave the answer up. CONTROL (`Mutant::ArmOverASettle`, the settle not consulted):
/// the retry arms it.
#[cfg(unix)]
#[test]
fn an_answer_the_next_bridge_delivers_ahead_of_the_retry_arms_no_deadline() {
    const SID: &str = "s-host";
    const POST: &str = "post sid=s-host id=1 to=h-a kind=ask dl=60000 len=1\nq";
    for control in [false, true] {
        let (lane, mut bridge) = FakeVerbLane::bridge(&[(1, SID)]);
        let state = bridge.state.root().to_path_buf();
        let (socket, _broker, _server) = serve_log(&lane, "answer-first");
        bridge.cfg.broker = socket.clone();
        bridge.conn = Some(bridge.connect().unwrap().0.into());
        assert!(bridge.refresh_sessions().is_ok());
        lane.retire_from_outbox();
        bridge.state.refuse_writes_of(Some("deadlines"));
        *lane.outbox.lock().unwrap() = Some(POST.to_string());
        bridge.drain_outbox();
        assert!(lane.sent.lock().unwrap().is_empty(), "held");
        let ask = *bridge.deadlines.keys().next().expect("in memory");
        let node = bridge.node.clone();
        drop(bridge);
        assert!(
            StateDir::peek(&state).deadlines().is_empty(),
            "the journal holds no deadline"
        );

        let (next_lane, mut next) = FakeVerbLane::bridge_with_state(&[(1, SID)], Some(&state));
        if control {
            next.fx = crate::fx::Fx::os_without(crate::sim::world::Mutant::ArmOverASettle);
        }
        next.cfg.broker = socket;
        next.conn = Some(next.connect().unwrap().0.into());
        assert!(next.refresh_sessions().is_ok());
        let mut body = Body::new(1);
        body.re = Some(ask);
        body.text = "a".to_string();
        let subject = format!("/f/f/in/{node}/{SID}/h-a/answer");
        let (mut peer, _) = next.connect().unwrap();
        let (off, _) = peer.publish(9_200, 1, &subject, &body.encode()).unwrap();
        next.deliver_record(off, &subject, &body.encode(), None);
        assert!(next_lane.delivery_offsets().contains(&off), "delivered");

        next_lane.retire_from_outbox();
        *next_lane.outbox.lock().unwrap() = Some(POST.to_string());
        next.drain_outbox();
        assert!(
            next_lane
                .sent
                .lock()
                .unwrap()
                .iter()
                .any(|l| l.starts_with(&format!("outbox sent {SID} 1 off={ask}"))),
            "the retry retires the post at its pin: {:?}",
            next_lane.sent.lock().unwrap()
        );
        if control {
            assert!(next.deadlines.contains_key(&ask), "CONTROL: armed");
            continue;
        }
        assert!(
            next.deadlines.is_empty() && next.state.deadlines().is_empty(),
            "the retry armed the answered ask's deadline: {:?}",
            next.deadlines
        );
        drop(next);
        assert!(
            StateDir::open(&state)
                .expect("reopen")
                .deadline_settled(ask, SID),
            "the settle is written"
        );
    }
}

/// A post's journal costs, as the outbox spends them, for `post` retired on a real broker.
#[cfg(unix)]
fn cost_of_post(bridge: &mut Bridge, lane: &FakeVerbLane, post: String) -> crate::state::FileOps {
    lane.sent.lock().unwrap().clear();
    let before = bridge.state.ops();
    *lane.outbox.lock().unwrap() = Some(post.clone());
    bridge.drain_outbox();
    assert!(
        lane.sent
            .lock()
            .unwrap()
            .iter()
            .any(|l| l.starts_with("outbox sent")),
        "{post}: {:?}",
        lane.sent.lock().unwrap()
    );
    bridge.state.ops() - before
}

/// **A LANDED ASK IS ONE APPEND OF THE ASKS JOURNAL, ITS DEADLINE RIDING WITH ITS RECIPIENT**
/// (C04 review, the change's main lever): on the real outbox path, against a real broker, an
/// ask with `dl=` spends what one without does, and each one append more than a note, which
/// writes nothing to this journal; no ask replaces a file a note does not. A kill right after
/// the `outbox sent` loses neither row, the recipient-alone write of an ask with no deadline
/// included.
#[cfg(unix)]
#[test]
fn a_landed_ask_is_one_append_its_deadline_riding_with_its_recipient() {
    const SID: &str = "s-host";
    let (lane, mut bridge) = FakeVerbLane::bridge(&[(1, SID)]);
    let state = bridge.state.root().to_path_buf();
    let (socket, _broker, _server) = serve_log(&lane, "ask-cost");
    bridge.cfg.broker = socket;
    bridge.conn = Some(bridge.connect().unwrap().0.into());
    assert!(bridge.refresh_sessions().is_ok());
    lane.retire_from_outbox();
    let post = |id: u64, tail: &str| format!("post sid={SID} id={id} to=h-a {tail} len=1\nq");
    cost_of_post(&mut bridge, &lane, post(1, "kind=note"));
    let note = cost_of_post(&mut bridge, &lane, post(2, "kind=note"));
    let bare = cost_of_post(&mut bridge, &lane, post(3, "kind=ask"));
    let with_deadline = cost_of_post(&mut bridge, &lane, post(4, "kind=ask dl=60000"));
    assert_eq!(bare.appends, note.appends + 1, "an ask, past a note");
    assert_eq!(
        with_deadline.appends, bare.appends,
        "its deadline rides in the recipient's append"
    );
    assert_eq!(
        (bare.replaces, with_deadline.replaces),
        (note.replaces, note.replaces),
        "no file replaced"
    );
    drop(bridge);
    let disk = StateDir::peek(&state);
    assert_eq!(disk.asked().len(), 2, "both recipients: {:?}", disk.asked());
    assert_eq!(disk.deadlines().len(), 1, "the deadline");
}

/// **THE SWEEP'S SETTLE IS ON DISK BEFORE THE NEXT BRIDGE** (C04 review): the sweep publishes
/// `expired`, then writes its settle, so a kill after the sweep leaves the next bridge no
/// deadline to expire twice, on a real broker. The orphan prune's likewise: a deadline whose
/// session is gone leaves the journal with the refresh that saw it go.
#[cfg(unix)]
#[test]
fn the_sweep_and_the_orphan_prune_write_their_settles() {
    const SID: &str = "s-host";
    let (lane, mut bridge) = FakeVerbLane::bridge(&[(1, SID)]);
    let state = bridge.state.root().to_path_buf();
    let (socket, _broker, _server) = serve_log(&lane, "settle-written");
    bridge.cfg.broker = socket;
    bridge.conn = Some(bridge.connect().unwrap().0.into());
    assert!(bridge.refresh_sessions().is_ok());
    let node = bridge.node.clone();
    let (mut peer, _) = bridge.connect().unwrap();
    let ask_lane = format!("/f/f/in/n-far/s-far/{node}/ask");
    let (ask, _) = peer
        .publish(9_100, 1, &ask_lane, b"v=1 t=1 text=q")
        .unwrap();
    bridge.note_deadline(SID, ask, 1_000, Some("n-far".to_string()));
    assert!(!StateDir::peek(&state).deadlines().is_empty(), "armed");
    bridge.expire_deadlines(true);
    let verdicts = format!("/f/f/in/{node}/{SID}/{node}/expired");
    let (rows, _) = peer.fetch(0, &verdicts, 8).unwrap();
    assert_eq!(rows.len(), 1, "the sweep expired the ask");
    assert!(
        StateDir::peek(&state).deadlines().is_empty(),
        "a kill here leaves no deadline to expire twice"
    );

    bridge.note_deadline("s-gone", ask + 100, 1_000, None);
    assert_eq!(StateDir::peek(&state).deadlines().len(), 1, "armed");
    assert!(bridge.refresh_sessions().is_ok());
    assert!(bridge.deadlines.is_empty(), "pruned");
    assert!(
        StateDir::peek(&state).deadlines().is_empty(),
        "a kill here leaves no deadline of a session that is gone"
    );
}

/// **A SWEEP READS BACK AN `expired` A KILLED BRIDGE PUBLISHED** (F4), on a real broker: a
/// deadline still standing with this node's `expired` for it on the asker's lane. The sweep
/// takes that for its verdict, publishes no second one and drops the deadline. CONTROL
/// (`Mutant::ExpireWithoutReadingBack`): a second `expired` goes over the first. The seeded
/// worlds reached this only through a retry arming a deadline over an `expired` it replayed,
/// which `Bridge::settle_a_retry_answered_ahead` closes (C04 review), so this is its control.
#[cfg(unix)]
#[test]
fn a_sweep_reads_back_an_expired_a_killed_bridge_published() {
    const SID: &str = "s-host";
    for control in [false, true] {
        let (lane, mut bridge) = FakeVerbLane::bridge(&[(1, SID)]);
        let (socket, _broker, _server) = serve_log(&lane, "own-expired");
        bridge.cfg.broker = socket;
        bridge.conn = Some(bridge.connect().unwrap().0.into());
        assert!(bridge.refresh_sessions().is_ok());
        if control {
            bridge.fx =
                crate::fx::Fx::os_without(crate::sim::world::Mutant::ExpireWithoutReadingBack);
        }
        let node = bridge.node.clone();
        let (mut peer, _) = bridge.connect().unwrap();
        let ask_lane = format!("/f/f/in/n-far/s-far/{node}/ask");
        let (ask, _) = peer
            .publish(9_100, 1, &ask_lane, b"v=1 t=1 text=q")
            .unwrap();
        let verdicts = format!("/f/f/in/{node}/{SID}/{node}/expired");
        peer.publish(9_100, 2, &verdicts, &expired_body(1, ask, 1_000).encode())
            .unwrap();
        bridge.note_deadline(SID, ask, 1_000, Some("n-far".to_string()));
        bridge.expire_deadlines(true);
        assert!(bridge.deadlines.is_empty(), "control={control}: settled");
        let (rows, _) = peer.fetch(0, &verdicts, 8).unwrap();
        assert_eq!(
            rows.len(),
            if control { 2 } else { 1 },
            "control={control}: how many `expired` the ask has"
        );
    }
}

/// **A STOP HANDS THE ASKS JOURNAL BACK AS THE TABLES AN OLDER BUILD READS** (C04 review), the
/// rollback safety the changelog promises: whichever way the bridge stops, `Bridge::end_run`
/// leaves each recipient and each deadline where a build before the journal looks, under a
/// line naming the hand-off the journal records. CONTROL: nothing is there before the stop.
#[cfg(unix)]
#[test]
fn a_stop_hands_the_asks_journal_back_as_the_tables_an_older_build_reads() {
    const SID: &str = "s-host";
    for bye in [None, Some(Bye::Handoff)] {
        let (_lane, mut bridge) = FakeVerbLane::bridge(&[(1, SID)]);
        let state = bridge.state.root().to_path_buf();
        bridge.note_asked(7, "h-a");
        bridge.note_deadline(SID, 7, 60_000, Some("h-a".to_string()));
        let (asked, deadlines) = (state.join(asks::ASKED), state.join(asks::DEADLINES));
        assert!(!asked.exists() && !deadlines.exists(), "CONTROL: {bye:?}");
        bridge.bye = bye;
        bridge.end_run();
        let asked = std::fs::read_to_string(&asked).expect("the asked table");
        let deadlines = std::fs::read_to_string(&deadlines).expect("the deadline table");
        let read_asked: Vec<Asked> = asked.lines().filter_map(Asked::parse).collect();
        assert_eq!(
            read_asked,
            [Asked {
                off: 7,
                to: "h-a".to_string()
            }],
            "{bye:?}"
        );
        let read_deadlines: Vec<Deadline> = deadlines.lines().filter_map(Deadline::parse).collect();
        assert_eq!(read_deadlines.len(), 1, "{bye:?}");
        assert_eq!(
            (read_deadlines[0].off, read_deadlines[0].sid.as_str()),
            (7, SID),
            "{bye:?}"
        );
        assert_eq!(asks::legacy_mark(&asked), Some(1), "{bye:?}");
        assert_eq!(asks::legacy_mark(&deadlines), Some(1), "{bye:?}");
    }
}

/// **AN UNWRITTEN DEADLINE TABLE WITHHOLDS ONLY THE ANSWER WHOSE DEADLINE IT HOLDS** (F4
/// review): the state dir refuses the table, so another session's deadline is not on disk.
/// An answer to an ask with no deadline is delivered at once and the link stays up, as
/// before item F4: no write stands between it and its session. The answer whose own
/// deadline's removal the disk refused is withheld, the link dropped to offer it again,
/// until a write of the table takes; then it is delivered.
#[cfg(unix)]
#[test]
fn an_unwritten_deadline_table_withholds_only_the_answer_whose_deadline_it_holds() {
    let (lane, mut bridge) = FakeVerbLane::bridge(&[(1, "s-host"), (2, "s-other")]);
    let (socket, _broker, _server) = serve_log(&lane, "unwritten");
    bridge.cfg.broker = socket;
    bridge.conn = Some(bridge.connect().unwrap().0.into());
    assert!(bridge.refresh_sessions().is_ok());
    let node = bridge.node.clone();
    bridge.state.refuse_writes_of(Some("deadlines"));
    assert!(
        !bridge.note_deadline("s-other", 5, 60_000, None),
        "the table's write is refused"
    );
    let answer = |sid: &str, re: u64| {
        let mut body = Body::new(1);
        body.re = Some(re);
        body.text = "a".to_string();
        (format!("/f/f/in/{node}/{sid}/h-a/answer"), body.encode())
    };

    let (subject, raw) = answer("s-host", 3);
    bridge.deliver_record(40, &subject, &raw, None);
    assert!(
        lane.delivery_offsets().contains(&40),
        "an answer that settles no deadline is delivered"
    );
    assert!(bridge.conn.is_some(), "and the link stays up");

    let (subject, raw) = answer("s-other", 5);
    bridge.deliver_record(41, &subject, &raw, None);
    assert!(
        !lane.delivery_offsets().contains(&41),
        "withheld while its deadline's removal is not on disk"
    );
    assert!(bridge.conn.is_none(), "the link dropped to offer it again");

    bridge.state.refuse_writes_of(None);
    bridge.conn = Some(bridge.connect().unwrap().0.into());
    bridge.deliver_record(41, &subject, &raw, None);
    assert!(
        lane.delivery_offsets().contains(&41),
        "delivered once the table is written"
    );
    assert!(bridge.state.deadlines().is_empty());
}

/// B12 — A TOPIC REPLAY THAT MEETS RETENTION'S FLOOR TELLS THE SESSION THE TOPIC'S RANGE: three
/// broadcasts on `alpha`, the session's cursor at the first, the log cut at the second. The
/// replay delivers from the floor and says `link gap from=<cursor> to=<floor> sid=<sid>
/// topic=alpha` once; dropping the topic forgets what was told. NC (run): a
/// `tell_given_up` that returns at once fails it.
#[cfg(unix)]
#[test]
fn a_topic_replay_below_the_retained_floor_tells_the_session_the_topics_range() {
    const SID: &str = "s-late";
    let (lane, mut bridge) = FakeVerbLane::bridge(&[(1, SID)]);
    bridge.endpoint_lane = aterm_types::control_verbs::BRIDGE_LANE;
    let socket = lane.dir.path().join("broker.sock");
    let broker = crate::test_broker(lane.dir.path().join("broker.log")).unwrap();
    let _server = broker.serve(&socket).unwrap();
    bridge.cfg.broker = socket.to_string_lossy().into_owned();
    let (conn, _) = bridge.connect().unwrap();
    bridge.conn = Some(conn.into());
    let (mut god, _) = bridge.connect().unwrap();
    let note = Body::new(0).encode();
    let subject = "/f/f/pub/n-x/s-p/say/alpha";
    let alpha: Vec<u64> = (1..=3)
        .map(|seq| god.publish(7, seq, subject, &note).unwrap().0)
        .collect();
    let drained = alpha[2] + 1;
    bridge.say_drained_to = drained;
    bridge.say_bus_head = drained;
    bridge.per_sid.topics.insert(
        SID.to_string(),
        BTreeMap::from([("alpha".to_string(), TopicCursor { next: alpha[0] })]),
    );
    broker
        .retain_before_carrying_state(alpha[1])
        .expect("the cut");

    assert!(bridge.catch_up_topic_page(SID));
    assert_eq!(lane.delivery_offsets(), alpha[1..], "from the floor");
    let told = [format!(
        "link gap from={} to={} sid={SID} topic=alpha",
        alpha[0], alpha[1]
    )];
    assert_eq!(told_gaps(&lane), told);
    bridge
        .per_sid
        .topics
        .get_mut(SID)
        .unwrap()
        .get_mut("alpha")
        .unwrap()
        .next = alpha[0];
    assert!(bridge.catch_up_topic_page(SID));
    assert_eq!(told_gaps(&lane), told, "the same floor is told once");

    let key = (SID.to_string(), "alpha".to_string());
    assert!(bridge.per_sid.given_up_told.contains_key(&key));
    bridge.topic_change(SID, &mut "drop alpha".split_whitespace());
    assert!(
        !bridge.per_sid.given_up_told.contains_key(&key),
        "a dropped topic forgets what was told of it"
    );
}

/// **A TOPIC REPLAY OFFERS THE BROADCASTS RETENTION HOLDS BELOW THE TOPIC'S OLDEST** (F3
/// review), on a real broker under rules no aterm build sets (`topics_whole_from_the_base`):
/// two broadcasts on `alpha`, carried below the base by a cut and held there while the
/// rules set next take their class whole from the base alone, and a third after. The replay
/// from the session's cursor at the first meets the topic's oldest, reads again told, and
/// delivers all three, telling no give-up. CONTROL (`Mutant::LaneReadWithoutTheHeld`, the
/// read going on at the floor): only the third, and the session is told `link gap …
/// topic=alpha` over the two the broker still holds.
#[cfg(unix)]
#[test]
fn a_topic_replay_offers_the_broadcasts_retention_holds_below_the_topics_oldest() {
    const SID: &str = "s-late";
    for control in [false, true] {
        let (lane, mut bridge) = FakeVerbLane::bridge(&[(1, SID)]);
        bridge.endpoint_lane = aterm_types::control_verbs::BRIDGE_LANE;
        let (socket, broker, _server) = serve_log(&lane, "topic-held");
        broker.set_retention_interval(Duration::from_secs(3600));
        broker
            .set_retention(topics_whole_from_the_base(true))
            .unwrap();
        bridge.cfg.broker = socket;
        bridge.conn = Some(bridge.connect().unwrap().0.into());
        if control {
            bridge.fx =
                crate::fx::Fx::os_without(crate::sim::world::Mutant::LaneReadWithoutTheHeld);
        }
        let (mut god, _) = bridge.connect().unwrap();
        let note = Body::new(0).encode();
        let subject = "/f/f/pub/n-x/s-p/say/alpha";
        let (first, _) = god.publish(7, 1, subject, &note).unwrap();
        let (second, _) = god.publish(7, 2, subject, &note).unwrap();
        broker.retain_before_carrying_state(second + 1).unwrap();
        broker
            .set_retention(topics_whole_from_the_base(false))
            .unwrap();
        let of_topic = astream_wire::Filter::new(subject::say_topic_filter("f", "alpha")).unwrap();
        assert!(
            broker.oldest(&of_topic) > second,
            "PRECONDITION: the topic's oldest lies above the broadcasts the broker holds"
        );
        let (third, _) = god.publish(7, 3, subject, &note).unwrap();
        bridge.say_drained_to = third + 1;
        bridge.say_bus_head = third + 1;
        bridge.per_sid.topics.insert(
            SID.to_string(),
            BTreeMap::from([("alpha".to_string(), TopicCursor { next: first })]),
        );

        assert!(bridge.catch_up_topic_page(SID));
        assert!(bridge.conn.is_some(), "the link stays up");
        if control {
            assert_eq!(
                lane.delivery_offsets(),
                [third],
                "CONTROL: the held ones skipped"
            );
            assert_eq!(
                told_gaps(&lane),
                [format!(
                    "link gap from={first} to={third} sid={SID} topic=alpha"
                )],
                "CONTROL: told a give-up over the broadcasts the broker holds"
            );
            continue;
        }
        assert_eq!(
            lane.delivery_offsets(),
            [first, second, third],
            "every broadcast, the held ones first"
        );
        assert!(
            told_gaps(&lane).is_empty(),
            "nothing was given up: {:?}",
            told_gaps(&lane)
        );
    }
}

/// `<dir>/<name>` holding one line of a ring, at `mode`.
#[cfg(unix)]
fn a_ring_at(dir: &std::path::Path, name: &str, mode: u32) -> String {
    use std::os::unix::fs::PermissionsExt as _;
    let path = dir.join(name);
    let c = astream_cap::mint(&[7u8; 32], "ro:/f/f1/pub/>").expect("mint");
    std::fs::write(
        &path,
        astream_cap::capfile::format_line(&c.filter, &c.tag) + "\n",
    )
    .expect("the ring");
    std::fs::set_permissions(&path, std::fs::Permissions::from_mode(mode)).expect("chmod");
    path.to_string_lossy().into_owned()
}

/// What a cap can be refused for, and the words that say what to do: the mode's `chmod`, and
/// what makes a right one for what is no file of its own.
#[cfg(unix)]
fn refusal(path: &str) -> (io::ErrorKind, String) {
    let e = read_cap_file(path).expect_err("a refused cap");
    (e.kind(), e.to_string())
}

/// A cap is authority: one others can read is refused at once, naming the file and the `chmod`
/// that closes it, and so is one read back by a bridge that holds a ring.
#[cfg(unix)]
#[test]
fn a_cap_file_others_can_read_is_refused_naming_the_file_and_the_chmod() {
    let dir = aterm_tempfile::Builder::new()
        .prefix("al-cap-wide")
        .suffix(".noindex")
        .tempdir_in("/tmp")
        .expect("scratch");
    for mode in [0o644, 0o640, 0o604, 0o660] {
        let path = a_ring_at(dir.path(), "wide.cap", mode);
        let (kind, said) = refusal(&path);
        assert_eq!(kind, io::ErrorKind::PermissionDenied, "{mode:o}: {said}");
        assert!(
            said.starts_with(&format!("{path} is mode {mode:04o}: ")),
            "{said}"
        );
        assert!(said.contains(&format!("chmod 600 {path}")), "{said}");
    }
}

/// A FIFO for a cap has no writer and never will: the read says so at once, behind the
/// one-minute hang detector, where a plain read waits for ever.
#[cfg(unix)]
#[test]
fn a_fifo_for_a_cap_file_is_refused_without_waiting_for_a_writer() {
    let dir = aterm_tempfile::Builder::new()
        .prefix("al-cap-fifo")
        .suffix(".noindex")
        .tempdir_in("/tmp")
        .expect("scratch");
    let fifo = dir.path().join("fifo.cap");
    let made = aterm_uds::spawnfd::command("mkfifo")
        .arg("-m")
        .arg("600")
        .arg(&fifo)
        .status()
        .expect("run mkfifo");
    assert!(made.success(), "mkfifo");
    let path = fifo.to_string_lossy().into_owned();
    let (tx, rx) = std::sync::mpsc::channel();
    let reading = path.clone();
    std::thread::spawn(move || {
        let _ = tx.send(read_cap_file(&reading).map_err(|e| (e.kind(), e.to_string())));
    });
    let (kind, said) = rx
        .recv_timeout(aterm_spec::wait::MUST_HAPPEN)
        .expect("the read waited on a FIFO")
        .expect_err("a FIFO is no cap file");
    assert_eq!(kind, io::ErrorKind::InvalidInput, "{said}");
    assert!(said.starts_with(&format!("{path} is a FIFO: ")), "{said}");
    assert!(said.contains("aterm fabric mint-for"), "what to do: {said}");
}

/// A link is refused without being followed, naming the link and what to do.
#[cfg(unix)]
#[test]
fn a_symlink_for_a_cap_file_is_refused_without_following_it() {
    let dir = aterm_tempfile::Builder::new()
        .prefix("al-cap-link")
        .suffix(".noindex")
        .tempdir_in("/tmp")
        .expect("scratch");
    let real = a_ring_at(dir.path(), "real.cap", 0o600);
    let link = dir.path().join("link.cap");
    std::os::unix::fs::symlink(&real, &link).expect("link");
    let path = link.to_string_lossy().into_owned();
    let (kind, said) = refusal(&path);
    assert_eq!(kind, io::ErrorKind::PermissionDenied, "{said}");
    assert!(
        said.starts_with(&format!("{path} is a symlink: ")),
        "{said}"
    );
    assert!(said.contains("name the file it points at"), "{said}");
    assert_eq!(read_cap_file(&real).expect("the file itself").len(), 1);
}

/// What every build wrote keeps reading: a ring at 0600 or tighter, in a directory of any mode
/// (`mint-for --out ~/h-andrew.cap`, a home directory being 0750 or 0755) and by any number of
/// names (`fabric on` holds a second one while it keeps an old ring aside). A wrong line is
/// still a wrong line, and an absent file still says so.
#[cfg(unix)]
#[test]
fn a_cap_file_every_build_wrote_keeps_reading_wherever_a_person_keeps_it() {
    use std::os::unix::fs::PermissionsExt as _;
    let dir = aterm_tempfile::Builder::new()
        .prefix("al-cap-kept")
        .suffix(".noindex")
        .tempdir_in("/tmp")
        .expect("scratch");
    std::fs::set_permissions(dir.path(), std::fs::Permissions::from_mode(0o755))
        .expect("a directory others can list");
    for mode in [0o600, 0o400] {
        let path = a_ring_at(dir.path(), &format!("kept-{mode:o}.cap"), mode);
        assert_eq!(
            read_ring(std::slice::from_ref(&path)).expect("kept").len(),
            1
        );
    }
    let named = a_ring_at(dir.path(), "named.cap", 0o600);
    std::fs::hard_link(&named, dir.path().join("named.cap.aside")).expect("a second name");
    assert_eq!(read_cap_file(&named).expect("two names").len(), 1);

    let empty = dir.path().join("empty.cap");
    std::fs::write(&empty, "").expect("empty");
    std::fs::set_permissions(&empty, std::fs::Permissions::from_mode(0o600)).expect("chmod");
    assert!(read_cap_file(&empty.to_string_lossy())
        .expect("no length floor")
        .is_empty());

    let broken = dir.path().join("broken.cap");
    std::fs::write(&broken, "ro:/f/f1/> nothex\n").expect("broken");
    std::fs::set_permissions(&broken, std::fs::Permissions::from_mode(0o600)).expect("chmod");
    let e = read_cap_file(&broken.to_string_lossy()).expect_err("malformed");
    assert_eq!(e.kind(), io::ErrorKind::InvalidData);
    assert!(
        e.to_string()
            .starts_with(&format!("{}:1: ", broken.display())),
        "{e}"
    );

    let absent = dir.path().join("absent.cap");
    let e = read_cap_file(&absent.to_string_lossy()).expect_err("absent");
    assert_eq!(e.kind(), io::ErrorKind::NotFound);
    assert!(
        e.to_string()
            .starts_with(&format!("{}: ", absent.display())),
        "{e}"
    );
}

/// Supervised startup must refuse the same files as every other reader, before it can
/// admit control lanes, and leave both bytes and permissions untouched.
#[cfg(unix)]
#[test]
fn supervised_start_refuses_unsafe_rings_before_admitting_its_lanes() {
    use std::os::unix::fs::PermissionsExt as _;
    let dir = aterm_tempfile::Builder::new()
        .prefix("al-cap-start")
        .suffix(".noindex")
        .tempdir_in("/tmp")
        .expect("scratch");
    let real = a_ring_at(dir.path(), "node.cap", 0o600);
    let bytes = std::fs::read(&real).unwrap();
    let link = dir.path().join("link.cap");
    std::os::unix::fs::symlink(&real, &link).unwrap();
    let link = link.to_string_lossy().into_owned();
    for folder_mode in [0o700, 0o755] {
        std::fs::set_permissions(dir.path(), std::fs::Permissions::from_mode(folder_mode)).unwrap();
        for (path, mode) in [(&real, 0o644), (&real, 0o000), (&link, 0o600)] {
            std::fs::set_permissions(path, std::fs::Permissions::from_mode(mode)).unwrap();
            let cfg = Config {
                fleet: "f1".into(),
                broker: dir
                    .path()
                    .join("unused.sock")
                    .to_string_lossy()
                    .into_owned(),
                cap_files: vec![path.clone()],
                state_dir: dir.path().join("state").to_string_lossy().into_owned(),
                accept_from: Vec::new(),
                sock: None,
                token: None,
                presence: Mode::Meta,
                receipts: true,
                log: crate::mirror::LogMode::Off,
                mesh: None,
            };
            // These sentinels are not sockets: a skipped capability check fails at a
            // different boundary, without waiting for an endpoint that will never reply.
            let lanes = InheritedLanes {
                verb: std::fs::File::open("/dev/null").unwrap().into(),
                push: std::fs::File::open("/dev/null").unwrap().into(),
            };
            let error = match Bridge::with_inherited_lanes(cfg, lanes) {
                Ok(_) => panic!("a refused ring started a bridge"),
                Err(error) => error,
            };
            assert_eq!(error.kind(), io::ErrorKind::PermissionDenied, "{error}");
            assert!(error.to_string().contains(path.as_str()), "{error}");
            assert_eq!(
                std::fs::metadata(path).unwrap().permissions().mode() & 0o777,
                mode
            );
            std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o600)).unwrap();
            assert_eq!(std::fs::read(path).unwrap(), bytes);
        }
        assert!(read_cap_file(&real).is_ok(), "the underlying ring is valid");
        assert!(std::fs::symlink_metadata(&link)
            .unwrap()
            .file_type()
            .is_symlink());
    }
}

#[test]
fn os_permission_denials_are_refusals_not_repairable_missing_files() {
    assert!(cap_refused(&io::Error::from(
        io::ErrorKind::PermissionDenied
    )));
    assert!(cap_refused(&io::Error::from(io::ErrorKind::Other)));
    assert!(!cap_refused(&io::Error::from(io::ErrorKind::NotFound)));
    assert!(!cap_refused(&io::Error::from(io::ErrorKind::InvalidData)));
}

/// A RUNNING BRIDGE IS NEVER STRANDED: a ring widened or swapped under it is not taken, and the
/// verdict names what to do, so the bridge attaches with the ring it holds at its next dial.
#[cfg(unix)]
#[test]
fn a_ring_refused_on_disk_leaves_a_running_bridge_with_the_ring_it_holds() {
    use std::os::unix::fs::PermissionsExt as _;
    let dir = aterm_tempfile::Builder::new()
        .prefix("al-cap-held")
        .suffix(".noindex")
        .tempdir_in("/tmp")
        .expect("scratch");
    let path = a_ring_at(dir.path(), "node.cap", 0o600);
    let files = vec![path.clone()];
    let held = read_ring(&files).expect("the ring it holds");
    assert_eq!(ring_on_disk(&files, &held, "n-a"), Ok(held.clone()));
    std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o644)).expect("widen");
    let why = ring_on_disk(&files, &held, "n-a").expect_err("a ring others can read");
    assert!(why.contains(&format!("chmod 600 {path}")), "{why}");
}

#[test]
fn a_ring_read_again_is_taken_only_when_it_is_still_this_nodes() {
    let ring = |node: &str, tag: u8, exp: Option<&str>| -> Vec<Cap> {
        crate::enable::node_rules("f1", node)
            .into_iter()
            .map(|grant| Cap {
                grant: exp.map_or_else(
                    || grant.clone(),
                    |e| grant.replacen(':', &format!(",exp={e}:"), 1),
                ),
                tag: [tag; 32],
            })
            .collect()
    };
    let person: Vec<Cap> = crate::enable::person_rules("f1", "h-x")
        .into_iter()
        .map(|grant| Cap {
            grant,
            tag: [0; 32],
        })
        .collect();
    let (s, n) = ("n-00000000000000bb", "n-00000000000000aa");
    let held = ring(s, 0, Some("1"));
    assert_eq!(
        ring_refusal(&ring(s, 1, Some("99999999999999")), &held, s),
        None
    );
    assert_eq!(ring_refusal(&ring(s, 1, None), &held, s), None);
    let mut both = ring(s, 1, None);
    both.extend(person.clone());
    assert_eq!(ring_refusal(&both, &held, s), None);
    let other = ring_refusal(&ring(n, 1, None), &held, s);
    assert!(other.as_deref().is_some_and(|w| w.contains(n)), "{other:?}");
    let mut mixed = ring(s, 1, None);
    mixed.extend(ring(n, 1, None));
    assert!(ring_refusal(&mixed, &held, s).is_some());
    assert!(ring_refusal(&[], &held, s).is_some());
    assert!(ring_refusal(&person, &held, s).is_some());
    assert_eq!(ring_refusal(&[], &[], s), None);
    assert_eq!(ring_refusal(&ring(s, 1, None), &[], s), None);
}

mod mirror_conformance {
    include!("bridge_mirror_tests.rs");
}

mod reclaim {
    include!("bridge_reclaim_tests.rs");
}

mod roster_count {
    include!("bridge_roster_tests.rs");
}

mod mesh {
    include!("bridge_federate_tests.rs");

    mod halts {
        include!("bridge_halt_tests.rs");
    }
}
