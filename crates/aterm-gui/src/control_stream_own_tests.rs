// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! aterm's own records on the stream (HX-15c): `hand`, `policy` and `restart`, each written by
//! the wire library's codec and numbered in its epoch (`in`), and the harness's answers to
//! them, `took` and `refused`. Driven through the same rig as the stream's own tests: a real
//! claim on a real connection, the shipping lanes and reader, and the session's own writers (a
//! person's gesture, the screen reader's composer, a hold, the restart and policy rows). The
//! `hand` gate reads the service's frozen awake clock, which the tests move by hand: no test
//! here sleeps or waits on a latency.

use std::path::PathBuf;

use alab_wire::Account;
use alab_wire::record::Composer;
use aterm_agent::harness::accounts_pool::LoginEnv;
use aterm_agent::harness::restart_stamp::Cause;
use aterm_messages::presence::ComposerState;
use aterm_slog::{Conf, Key, Kind, Node, Op, Rec, Src};

use super::*;
use crate::restart_act::{RESTART_EVENT, record_restart, record_scoped_restart};

/// What a `hand` record says: `(human_seq, composer, hold)`.
fn hand_of(record: &Record) -> (u64, Composer, bool) {
    match record.body {
        Body::Hand {
            human_seq,
            composer,
            hold,
        } => (human_seq, composer, hold),
        ref other => panic!("{other:?} is no hand"),
    }
}

/// The screen reader reads `composer` in `sid`'s input box.
fn read_composer(rig: &Rig, sid: &str, composer: ComposerState) {
    rig.service
        .handle(sid)
        .ctx
        .timeline
        .lock()
        .unwrap_or_else(PoisonError::into_inner)
        .set_composer(composer);
}

/// A hold on `sid` is set (`on`) or lifted.
fn hold(rig: &Rig, sid: &str, on: bool) {
    let ctx = rig.service.handle(sid).ctx;
    let hold = on.then(|| crate::fabric::Hold {
        reason: "test".to_string(),
        origin: "local".to_string(),
    });
    crate::fabric::apply_hold_for_test(&ctx, hold);
    assert_eq!(ctx.fabric.hold().is_some(), on, "the hold moved");
}

/// A person types a key into `sid`, as the input seam counts it: `human_seq` after it.
fn typed(rig: &Rig, sid: &str) -> u64 {
    let ctx = rig.service.handle(sid).ctx;
    crate::app_input::note_person(&ctx, crate::human_input::Gesture::Typed);
    ctx.human_input.seq()
}

/// Send the next two `beat`s and read both acknowledgements. aterm writes what a look owes
/// after the acknowledgement of the record it read, so whatever any look before the second
/// `beat` sent is heard before its acknowledgement: an empty [`Client::heard`] afterwards says
/// nothing was sent.
fn two_beats(client: &mut Client, seq: &mut u64) {
    for _ in 0..2 {
        *seq += 1;
        assert_eq!(client.record(*seq, Body::Beat), (*seq, None));
    }
}

/// The newest timeline row of `sid`: its kind and payload.
fn last_row(rig: &Rig, sid: &str) -> (&'static str, String) {
    let rows = rig
        .service
        .handle(sid)
        .ctx
        .timeline
        .lock()
        .unwrap_or_else(PoisonError::into_inner)
        .view_copy(None, 0)
        .rows();
    let row = rows.last().expect("a row");
    (row.kind, row.payload.to_string())
}

/// The `refused` marks of `sid`'s log, oldest first.
fn refusals(rig: &Rig, sid: &str) -> Vec<Rec> {
    let handle = rig.service.handle(sid);
    let timeline = handle
        .ctx
        .timeline
        .lock()
        .unwrap_or_else(PoisonError::into_inner);
    timeline
        .slog()
        .log()
        .window()
        .filter(|rec| rec.k == Kind::Refused)
        .cloned()
        .collect()
}

/// How many records `sid`'s log holds.
fn log_len(rig: &Rig, sid: &str) -> usize {
    let handle = rig.service.handle(sid);
    let timeline = handle
        .ctx
        .timeline
        .lock()
        .unwrap_or_else(PoisonError::into_inner);
    timeline.slog().log().window().count()
}

/// `hand` IS SENT WHEN IT CHANGES, AT MOST EVERY 100 MS OF THE AWAKE CLOCK, ALWAYS THE LATEST:
/// the stream opens with the hand as it stands; a person's keys, the composer and a hold moved
/// inside the gap send nothing until it ends, and then one `hand` carrying the last of them; a
/// change once the gap has passed is sent at once. CONTROLS: an unchanged hand sends nothing
/// however far the clock moves, and a hand that moved and came back inside the gap sends
/// nothing either.
#[test]
fn hand_is_sent_when_it_changes_at_most_every_100_ms_and_always_the_latest() {
    let rig = Rig::new(2);
    read_composer(&rig, ONE, ComposerState::Empty);
    let mut client = rig.claimant(ONE, "state");
    let opened = client.open(ONE);
    let first = typed(&rig, TWO);
    assert_eq!(first, 1, "another tab's key is its own");
    assert_eq!(
        hand_of(&client.opening[0]),
        (0, Composer::Empty, false),
        "the hand as the stream opened"
    );
    let mut seq = 0;

    // Inside the gap: two keys, a draft and a hold. Nothing goes while the clock stands.
    typed(&rig, ONE);
    read_composer(&rig, ONE, ComposerState::Draft);
    hold(&rig, ONE, true);
    let latest = typed(&rig, ONE);
    assert_eq!(latest, 2);
    two_beats(&mut client, &mut seq);
    assert!(
        client.heard.is_empty(),
        "held inside the gap: {:?}",
        client.heard
    );

    // The gap ends 100 ms after the opening hand: one `hand`, the latest.
    rig.service.clock.store(1_100, Ordering::Release);
    let sent = client.own();
    assert_eq!(hand_of(&sent), (latest, Composer::Draft, true));
    assert_eq!(sent.seq, opened.sent + 3, "numbered after the opening pair");
    two_beats(&mut client, &mut seq);
    assert!(
        client.heard.is_empty(),
        "one hand for the changes: {:?}",
        client.heard
    );

    // CONTROL: an unchanged hand sends nothing as the clock moves on.
    rig.service.clock.store(1_300, Ordering::Release);
    two_beats(&mut client, &mut seq);
    assert!(client.heard.is_empty(), "{:?}", client.heard);

    // Past the gap a change goes at once.
    hold(&rig, ONE, false);
    let lifted = client.own();
    assert_eq!(hand_of(&lifted), (latest, Composer::Draft, false));
    assert_eq!(lifted.seq, sent.seq + 1);

    // CONTROL: a hand that moves and comes back inside the gap sends nothing when it ends.
    hold(&rig, ONE, true);
    hold(&rig, ONE, false);
    rig.service.clock.store(1_400, Ordering::Release);
    two_beats(&mut client, &mut seq);
    assert!(client.heard.is_empty(), "{:?}", client.heard);
    // The composer aterm cannot read is a draft to the harness: it holds for one it cannot
    // rule out. A placeholder is empty.
    read_composer(&rig, ONE, ComposerState::Placeholder);
    assert_eq!(
        hand_of(&client.own()),
        (latest, Composer::Empty, false),
        "a placeholder is empty"
    );
    rig.service.clock.store(1_500, Ordering::Release);
    read_composer(&rig, ONE, ComposerState::Unknown);
    assert_eq!(
        hand_of(&client.own()),
        (latest, Composer::Draft, false),
        "an unread composer is a draft"
    );
}

/// A `restart` OR A `policy` CHANGE REACHES AN OPEN STREAM AS A RECORD: a `restart` row, from
/// the restart verb's scoped event (`record_restart`, which the watchdog's causes take too) and
/// from an account or model move (`record_scoped_restart`), is sent as a `restart` record with
/// its cause, its account (`to` and `home`) and its model, and the row stays for subscribers;
/// `harness policy` moving, and a placement that moves the claim's grant, each send a `policy`.
/// CONTROLS: a row written before the stream opened is sent on none, and the same grant placed
/// again sends nothing.
#[test]
fn a_restart_or_policy_change_reaches_an_open_stream_as_a_record() {
    let rig = Rig::new(2);
    let handle = rig.service.handle(ONE);
    let ctx = &handle.ctx;
    let mut client = rig.claimant(ONE, "relaunch,accounts");
    let id = client.id();
    record_restart(ctx, Cause::Asked);
    assert_eq!(
        last_row(&rig, ONE),
        (RESTART_EVENT, "cause=asked".to_string())
    );
    let opened = client.open(ONE);
    let mut seq = 0;
    two_beats(&mut client, &mut seq);
    assert!(
        client.heard.is_empty(),
        "CONTROL: a row from before the stream opened is not sent: {:?}",
        client.heard
    );

    record_restart(ctx, Cause::Stall);
    let restart = client.own();
    assert_eq!(
        (restart.seq, restart.body),
        (
            opened.sent + 3,
            Body::Restart {
                cause: "stall".to_string(),
                account: None,
                model: None,
            }
        )
    );
    assert_eq!(
        last_row(&rig, ONE),
        (RESTART_EVENT, "cause=stall".to_string()),
        "the row stays for subscribers"
    );

    let env = LoginEnv {
        label: "work".to_string(),
        var: "KIMI_CODE_HOME",
        value: Some("/tmp/account home".to_string()),
    };
    assert!(record_scoped_restart(
        ctx,
        "account",
        Some(&env),
        Some("model-b")
    ));
    assert_eq!(
        client.own().body,
        Body::Restart {
            cause: "account".to_string(),
            account: Some(Account {
                label: "work".to_string(),
                home: Some(PathBuf::from("/tmp/account home")),
            }),
            model: Some("model-b".to_string()),
        }
    );
    assert_eq!(
        last_row(&rig, ONE),
        (
            RESTART_EVENT,
            "cause=account to=work home=/tmp/account%20home model=model-b".to_string()
        )
    );
    assert!(record_scoped_restart(ctx, "model", None, Some("model-c")));
    assert_eq!(
        client.own().body,
        Body::Restart {
            cause: "model".to_string(),
            account: None,
            model: Some("model-c".to_string()),
        }
    );

    // `harness policy` moved.
    crate::harness_policy_verb::record_policy_move(ctx, 3);
    assert_eq!(client.own().body, Body::Policy);

    // The claim placed again on the stream's own connection with a wider grant.
    place(
        &handle,
        4,
        &[Duty::Relaunch, Duty::Accounts, Duty::State],
        id,
        running(),
        "claude-code-alab",
    )
    .expect("a wider grant");
    assert_eq!(client.own().body, Body::Policy, "the grant moved");
    // CONTROL: the same grant placed again moves nothing and sends nothing.
    place(
        &handle,
        4,
        &[Duty::Relaunch, Duty::Accounts, Duty::State],
        id,
        running(),
        "claude-code-alab",
    )
    .expect("a renewal in place");
    two_beats(&mut client, &mut seq);
    assert!(client.heard.is_empty(), "{:?}", client.heard);
    assert_eq!(
        rig.state(ONE).sent,
        opened.sent + 7,
        "two opening records, three restarts and two policies"
    );
}

/// A NEW STREAM HEARS THE CURRENT `hand` AND A `policy`, AND NO RECORD AGAIN (D8): a reopened
/// stream of the same epoch answers `in` where the last left it and goes on from there with the
/// hand as it is now and a `policy`; nothing the closed stream was sent comes again, and a
/// `restart` row or a policy move written while no stream was open is owed to none. A new
/// epoch starts `in` at 0 and keeps `acked`.
#[test]
fn a_new_stream_hears_the_current_hand_and_policy_and_no_resent_record() {
    let rig = Rig::new(2);
    let handle = rig.service.handle(ONE);
    let ctx = &handle.ctx;
    read_composer(&rig, ONE, ComposerState::Empty);
    let mut first = rig.claimant(ONE, "relaunch");
    let first_id = first.id();
    let opened = first.open(ONE);
    assert_eq!(
        (opened.sent, first.opening[0].seq, first.opening[1].seq),
        (0, 1, 2)
    );
    record_restart(ctx, Cause::Asked);
    assert_eq!(first.own().seq, 3);
    crate::harness_policy_verb::record_policy_move(ctx, 4);
    let policy = first.own();
    assert_eq!((policy.seq, policy.body), (4, Body::Policy));
    assert_eq!(first.record(1, Body::Took { of: 4 }), (1, None));
    drop(first);
    rig.ended(first_id);
    rig.lanes_free();

    // No stream is open: a restart row and a policy move are owed to none; the hand moves.
    record_restart(ctx, Cause::Stall);
    crate::harness_policy_verb::record_policy_move(ctx, 5);
    let keys = typed(&rig, ONE);
    hold(&rig, ONE, true);
    let state = rig.state(ONE);
    assert_eq!(
        (state.open, state.restarts.len(), state.policy_due),
        (None, 0, false),
        "nothing is owed to a closed stream"
    );

    // The same claimant opens again in the same epoch.
    let mut again = rig.claimant(ONE, "relaunch");
    let again_id = again.id();
    let reopened = again.open(ONE);
    assert_eq!(
        (reopened.epoch.as_str(), reopened.acked, reopened.sent),
        (opened.epoch.as_str(), 1, 4),
        "`in` where the closed stream left it"
    );
    assert_eq!((again.opening[0].seq, again.opening[1].seq), (5, 6));
    assert_eq!(
        hand_of(&again.opening[0]),
        (keys, Composer::Empty, true),
        "the hand as it is now"
    );
    let mut seq = 1;
    two_beats(&mut again, &mut seq);
    assert!(
        again.heard.is_empty(),
        "no record sent again, and none owed while closed: {:?}",
        again.heard
    );
    drop(again);
    rig.ended(again_id);
    rig.lanes_free();

    // Another aterm instance answered the stream before (a new epoch): `in` starts at 0, and an
    // answer naming the old epoch's numbers names nothing sent in this one.
    rig.service
        .handle(ONE)
        .ctx
        .meta
        .lock()
        .unwrap_or_else(PoisonError::into_inner)
        .stream
        .as_mut()
        .expect("the stream's state")
        .epoch = "p0-before".to_string();
    let mut later = rig.claimant(ONE, "relaunch");
    let fresh = later.open(ONE);
    assert_eq!((fresh.acked, fresh.sent), (3, 0));
    assert_eq!((later.opening[0].seq, later.opening[1].seq), (1, 2));
    assert_eq!(
        later.record(4, Body::Took { of: 6 }),
        (4, Some("refused:in".to_string()))
    );
    assert_eq!(later.record(5, Body::Took { of: 2 }), (5, None));
}

/// A STREAM REOPENED BEFORE THE LAST ONE'S LANE CLOSED HEARS NOTHING OWED TO THE LAST (D8): the
/// claimant's stream lane is busy serving a `claim` record (its image proof waits on the test)
/// when a `restart` row and a policy move are owed to that stream; the same claimant then
/// claims from a new connection and opens its stream there while the busy lane has not closed.
/// The new stream hears its opening `hand` and `policy` and no `restart`, and the old stream,
/// once its lane goes on, sends neither. CONTROLS: the row and the policy were owed to the
/// stream open when they were written, and a row written once the new stream is open reaches
/// it.
#[test]
fn a_stream_reopened_before_the_last_closed_hears_nothing_owed_to_the_last() {
    let rig = Rig::new(2);
    let handle = rig.service.handle(ONE);
    let ctx = &handle.ctx;
    let mut first = rig.claimant(ONE, "relaunch");
    first.open(ONE);

    // The first stream's lane takes a `claim` record and waits on the image proof, which
    // proves nothing once it goes on, so the claim does not come back to that connection.
    let mut proof = rig
        .service
        .proof
        .lock()
        .unwrap_or_else(PoisonError::into_inner);
    *proof = None;
    let served = rig.service.lines().len();
    let claim = Body::Claim {
        child: None,
        scope: None,
        version: Some("0.3.0".to_string()),
        vendor: None,
        login: None,
    };
    first.send(
        &Record {
            seq: 1,
            ms: 0,
            body: claim,
        }
        .line()
        .expect("a record line"),
    );
    first.send("\n");
    let deadline = Instant::now() + SETTLE;
    while rig.service.lines().len() == served {
        assert!(
            Instant::now() < deadline,
            "the lane never served the claim record"
        );
        std::thread::sleep(Duration::from_millis(2));
    }

    // Owed to the stream open now, whose lane cannot send them.
    record_restart(ctx, Cause::Stall);
    crate::harness_policy_verb::record_policy_move(ctx, 3);
    let state = rig.state(ONE);
    assert_eq!(
        (state.restarts.len(), state.policy_due),
        (1, true),
        "CONTROL: owed to the stream open when written"
    );

    // The same claimant claims from a new connection and opens its stream there.
    let mut second = rig.claimant(ONE, "relaunch");
    let reopened = second.open(ONE);
    assert_eq!(reopened.acked, 1, "the busy lane took its `claim` record");
    let mut seq = reopened.acked;
    two_beats(&mut second, &mut seq);
    assert!(
        second.heard.is_empty(),
        "nothing owed to the earlier stream: {:?}",
        second.heard
    );
    assert_eq!(
        rig.state(ONE).sent,
        reopened.sent + 2,
        "the opening pair only"
    );

    // The busy lane goes on: its `claim` record is refused, and the stream it served closes
    // having sent nothing of aterm's.
    drop(proof);
    let (acked, dropped) = first.ack();
    assert_eq!(acked, 1);
    assert!(dropped.is_some(), "the claim record proved nothing");
    assert_eq!(first.end(), "<EOF>");
    assert!(
        first.heard.is_empty(),
        "the closed stream sent them neither: {:?}",
        first.heard
    );

    // CONTROL: a row written while the new stream is open reaches it.
    record_restart(ctx, Cause::Asked);
    assert_eq!(
        second.own().body,
        Body::Restart {
            cause: "asked".to_string(),
            account: None,
            model: None,
        }
    );
}

/// `took` AND `refused` ARE READ, AND A REFUSAL IS LOGGED: both are counted in `meta`
/// (`stream_took=`, `stream_refused=`), and a refusal is written by the stream's write arm as
/// `mark - refused in=<n> text=<why>` with `holder=`, `src=harness`, `conf=claim` and
/// `by=<the tab>`, spending none of the session's mark budget (D10). The harness's words are
/// data: hidden characters go, the text is clamped to its bound, and nothing reaches the closed
/// `why=`. CONTROLS: a `took` writes nothing to the log, an answer naming no record aterm sent
/// in this epoch is dropped `refused:in` and counted as neither, and one naming `in` 0 is no
/// record at all (`refused:usage`).
#[test]
fn took_and_refused_are_read_and_a_refusal_is_logged() {
    let rig = Rig::new(2);
    let by = crate::session_log::sid_actor(ONE).expect("a minted session id");
    let holder = format!("claude-code-alab@{}", running().pid);
    // The session log on a hand clock, so no token of the mark budget comes back.
    rig.service
        .handle(ONE)
        .ctx
        .timeline
        .lock()
        .unwrap_or_else(PoisonError::into_inner)
        .slog_mut()
        .set_clock(crate::session_log::Clock::reading(|| {
            aterm_slog::Stamp::new(1_792_000_000_000, 5_000)
        }));
    let mut client = rig.claimant(ONE, "state");
    client.open(ONE);
    // The first applied record opens the tab's agent run (D15); after it, a `took` writes
    // nothing.
    assert_eq!(client.record(1, Body::Beat), (1, None));
    let before = log_len(&rig, ONE);
    assert_eq!(client.record(2, Body::Took { of: 1 }), (2, None));
    assert_eq!(
        log_len(&rig, ONE),
        before,
        "CONTROL: a `took` writes no row"
    );

    let why = "no relaunch grant";
    assert_eq!(
        client.record(
            3,
            Body::Refused {
                of: 2,
                why: why.to_string()
            }
        ),
        (3, None)
    );
    let meta = rig.meta(ONE);
    assert!(meta.contains(" stream_took=1 stream_refused=1 "), "{meta}");
    let state = rig.state(ONE);
    assert!(
        state.applied.contains("took") && state.applied.contains("refused"),
        "{:?}",
        state.applied
    );
    let read = |rec: &Rec| {
        (
            (rec.op, rec.node),
            rec.f.get_u(Key::In),
            rec.f.get_str(Key::Text).map(str::to_string),
            rec.f.get(Key::Why).is_some(),
            rec.f.get_str(Key::Holder).map(str::to_string),
            (rec.src, rec.conf, rec.by),
        )
    };
    let stamps = (Src::Harness, Conf::Claim, by);
    assert_eq!(
        refusals(&rig, ONE).iter().map(read).collect::<Vec<_>>(),
        [(
            (Op::Mark, Node::SESSION),
            Some(2),
            Some(why.to_string()),
            false,
            Some(holder.clone()),
            stamps
        )]
    );

    // CONTROLS: one past what aterm sent names nothing of this epoch, and `in` 0 is no record
    // (the codec reads an ordinal).
    let state = rig.state(ONE);
    let (dropped, past) = (state.dropped, state.sent + 1);
    assert_eq!(
        client.raw(r#"{"k":"took","seq":4,"ms":0,"in":0}"#),
        (4, Some("refused:usage".to_string()))
    );
    assert_eq!(
        client.record(
            5,
            Body::Refused {
                of: past,
                why: why.to_string()
            }
        ),
        (5, Some("refused:in".to_string()))
    );
    let state = rig.state(ONE);
    assert_eq!(
        (state.took, state.refused, state.dropped),
        (1, 1, dropped + 2)
    );
    assert_eq!(
        refusals(&rig, ONE).len(),
        1,
        "a dropped answer logs nothing"
    );

    // The session's whole mark budget spent: a refusal is still logged, and gives none back.
    let spend = |most: usize| {
        let handle = rig.service.handle(ONE);
        let mut timeline = handle
            .ctx
            .timeline
            .lock()
            .unwrap_or_else(PoisonError::into_inner);
        (0..most)
            .take_while(|_| timeline.note_harness_act("approve", &holder, by).is_some())
            .count()
    };
    let budget = spend(10_000);
    assert!(
        (1..10_000).contains(&budget),
        "CONTROL: the budget is finite: {budget}"
    );
    assert_eq!(spend(1), 0, "the budget is spent");
    // Words with hidden characters and far past the text's bound.
    let long = format!("\u{202e}stop\u{200b}\n{}", "x".repeat(10_000));
    assert_eq!(
        client.record(6, Body::Refused { of: 1, why: long }),
        (6, None)
    );
    let logged = refusals(&rig, ONE);
    assert_eq!(logged.len(), 2, "logged with the mark budget at zero");
    let last = read(logged.last().expect("the refusal"));
    assert_eq!(
        last,
        (
            (Op::Mark, Node::SESSION),
            Some(1),
            Some(format!("stop{}", "x".repeat(156))),
            false,
            Some(holder.clone()),
            stamps
        ),
        "hidden characters gone, clamped to 160 bytes, never in `why=`"
    );
    assert_eq!(spend(1), 0, "and the stream's mark gave nothing back");
    assert!(
        rig.meta(ONE).contains(" stream_took=1 stream_refused=2 "),
        "{}",
        rig.meta(ONE)
    );
}
