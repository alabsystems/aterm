// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! WHAT THE PERSON IS SHOWN IS A `shown` MARK:
//! each move of the printed need tuple (`status needs=`, its
//! severity, `hold_by=`) the one verdict writer publishes
//! (`SessionTimeline::set_need`) is one `mark - shown` by aterm, never a
//! publication that moves only the verdict's other facts; a per-session
//! bucket coalesces a flapping verdict to its newest tuple, which is never
//! lost; the mark is local and journaled, one `brief` count line, and the
//! carry keeps the tuple so a successor does not restate it. Every test is
//! driven by explicit publications on a hand-set clock.

use aterm_slog::{Kind, Op, Rec, Tier, Val};
use aterm_types::needs::{HoldWord, NeedWord, Severity, WaitWord};

use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use super::super::tests::{Hand, hand_clock, hand_over};
use super::*;
use crate::needs::{Need, Verdict, Waiting};
use crate::session_store::{SessionHandle, SessionStore, test_handle};
use crate::session_timeline::SessionTimeline;
use crate::{App, WindowId};

/// The need `word` as a verdict shows it.
fn need(word: NeedWord) -> Option<Need> {
    Some(Need {
        word,
        key: 1,
        what: String::new(),
        harness: None,
    })
}

/// A verdict that prints `word` and `held`.
fn verdict(word: Option<NeedWord>, held: Option<HoldWord>) -> Verdict {
    Verdict {
        need: word.and_then(need),
        held,
        ..Verdict::default()
    }
}

/// A registered session on a hand clock.
fn rig(local: u64) -> (SessionHandle, SessionStore, Hand) {
    let h = test_handle(local);
    let hand = hand_clock(&h.ctx.timeline);
    let mut store = SessionStore::default();
    store.register(h.clone());
    (h, store, hand)
}

/// One publication of the verdict writer.
fn publish(h: &SessionHandle, v: Verdict) -> bool {
    h.ctx.timeline.lock().unwrap().set_need(v)
}

/// A field as its row prints it, `-` for none.
fn word(r: &Rec, key: Key) -> String {
    match r.f.get(key) {
        Some(Val::U(n)) => n.to_string(),
        Some(_) => r.f.get_word(key).unwrap_or("-").to_string(),
        None => "-".to_string(),
    }
}

/// The session's shown marks, each `<need> <sev> <held>[ coalesced=<n>]`.
fn shown(h: &SessionHandle) -> Vec<String> {
    shown_after(h, 0)
}

/// [`shown`] past log seq `seq` (a successor's own, past what it carried).
fn shown_after(h: &SessionHandle, seq: u64) -> Vec<String> {
    shown_in(&h.ctx.timeline, seq)
}

/// The shown marks past log seq `seq` in the session log `tl` holds.
fn shown_in(tl: &Mutex<SessionTimeline>, seq: u64) -> Vec<String> {
    tl.lock()
        .unwrap()
        .slog()
        .log()
        .window()
        .filter(|r| r.seq > seq && r.op == Op::Mark && r.k == Kind::Shown)
        .map(|r| {
            let tuple = format!(
                "{} {} {}",
                word(r, Key::Need),
                word(r, Key::Sev),
                word(r, Key::Held)
            );
            match r.f.get(Key::Coalesced) {
                Some(_) => format!("{tuple} coalesced={}", word(r, Key::Coalesced)),
                None => tuple,
            }
        })
        .collect()
}

/// The `i`th publication of a verdict that flaps between an approval and a
/// question.
fn flap(i: u64) -> Verdict {
    let word = if i.is_multiple_of(2) {
        NeedWord::Approval
    } else {
        NeedWord::Question
    };
    verdict(Some(word), None)
}

/// A TUPLE MOVE IS ONE MARK: the first publication, which shows nothing, is
/// no news; then a need, a hold over it, another need under the hold, and
/// nothing, each one publication and one `mark - shown` by aterm on the
/// session, its fields the printed words (each absent for none).
#[test]
fn a_move_of_the_printed_tuple_is_one_shown_mark() {
    let (h, _store, hand) = rig(81);
    let steps = [
        (verdict(None, None), None),
        (
            verdict(Some(NeedWord::Approval), None),
            Some("approval decide -"),
        ),
        (
            verdict(Some(NeedWord::Approval), Some(HoldWord::Person)),
            Some("approval decide person"),
        ),
        (
            verdict(Some(NeedWord::Wall), Some(HoldWord::Person)),
            Some("wall stuck person"),
        ),
        (verdict(None, None), Some("- - -")),
    ];
    let mut want = Vec::new();
    for (i, (v, mark)) in steps.into_iter().enumerate() {
        hand.set(1_000 * (i as u64 + 1));
        let _ = publish(&h, v);
        want.extend(mark.map(str::to_string));
        assert_eq!(shown(&h), want, "after step {i}");
    }
    let g = h.ctx.timeline.lock().unwrap();
    let line = g
        .slog()
        .log()
        .window()
        .find(|r| r.k == Kind::Shown)
        .map(|r| aterm_slog::encode(r, Tier::Wire).expect("fits"))
        .expect("a shown mark");
    assert!(
        line.contains(" mark - shown need=approval sev=decide by=aterm src=aterm conf=strong "),
        "{line}"
    );
}

/// 100 SWEEPS THAT MOVE NO PRINTED TUPLE WRITE NO MARK: a need shown, then
/// 100 publications a second apart, each moving the verdict (its recheck
/// instant every time, its wait every other) and none what it prints.
/// NEGATIVE CONTROL (by scratch revert): keyed on the verdict's `moved`,
/// each writes one.
#[test]
fn sweeps_that_move_no_printed_tuple_write_no_shown_mark() {
    let (h, _store, hand) = rig(82);
    hand.set(1_000);
    let _ = publish(&h, verdict(Some(NeedWord::Question), None));
    assert_eq!(shown(&h), ["question decide -"]);
    for i in 0..100u64 {
        hand.set(2_000 + 1_000 * i);
        let mut v = verdict(Some(NeedWord::Question), None);
        v.recheck_unix = Some(1_900_000_000 + i64::try_from(i).expect("small"));
        v.waiting = i.is_multiple_of(2).then_some(Waiting {
            word: WaitWord::Limit,
            wall: Some("usage-session"),
            reset: None,
            entry: None,
        });
        assert!(publish(&h, v), "sweep {i}: the verdict moved");
    }
    assert_eq!(shown(&h), ["question decide -"], "no tuple moved");
}

/// A VERDICT FLAPPING 200 TIMES IN ONE HAND-SET SECOND writes the bucket
/// ([`PROGRAM_BURST`] marks) at once, then, once a token frees (the status
/// sweep's tick a second on), its newest tuple with `coalesced=` the tuples
/// it replaced: the newest is never lost. NEGATIVE CONTROL (by scratch
/// revert): a bucket that keeps its oldest pending tuple (drop-newest) ends
/// on a stale one.
#[test]
fn a_flapping_verdict_writes_the_bucket_then_its_newest_tuple() {
    let (h, _store, hand) = rig(83);
    hand.set(5_000);
    for i in 0..200u64 {
        let _ = publish(&h, flap(i));
    }
    let burst = usize::try_from(PROGRAM_BURST).expect("small");
    assert_eq!(shown(&h).len(), burst, "at most the bucket");
    hand.set(6_000);
    {
        let mut g = h.ctx.timeline.lock().unwrap();
        let at = g.slog().now();
        g.slog_mut().shown_tick(at);
    }
    let got = shown(&h);
    assert_eq!(got.len(), burst + 1, "then the newest");
    assert_eq!(
        got.last().map(String::as_str),
        Some("question decide - coalesced=179"),
        "the newest tuple, the 179 it replaced counted: {got:?}"
    );
}

/// THE NEWEST TUPLE IS WRITTEN BEFORE THE CLOSE AND THE CARRY: a flap past
/// the bucket, then the session closes: its pending tuple is a mark before
/// the session's close. Across a seamless update, the carry names the tuple
/// last shown (the pending one written first), so the successor's
/// publication of the same tuple writes nothing and another one does.
#[test]
fn the_newest_shown_tuple_is_written_before_the_close_and_the_carry() {
    let burst = PROGRAM_BURST + 1;
    // The close.
    let (h, mut store, hand) = rig(84);
    hand.set(1_000);
    for i in 0..burst {
        let held = i.is_multiple_of(2).then_some(HoldWord::Local);
        let _ = publish(&h, verdict(Some(NeedWord::Ask), held));
    }
    let _ = store.deregister_local(84);
    let g = h.ctx.timeline.lock().unwrap();
    let tail: Vec<&Rec> = g.slog().log().window().collect();
    let close = tail
        .iter()
        .rposition(|r| r.op == Op::Close && r.node == aterm_slog::Node::SESSION)
        .expect("the session's close");
    let last_shown = tail
        .iter()
        .rposition(|r| r.k == Kind::Shown)
        .expect("a shown mark");
    assert!(last_shown < close, "written before the close");
    assert_eq!(word(tail[last_shown], Key::Held), "local", "the newest");
    drop(g);
    // The carry.
    let (h, store, hand) = rig(85);
    hand.set(1_000);
    for i in 0..burst {
        let held = i.is_multiple_of(2).then_some(HoldWord::Bridge);
        let _ = publish(&h, verdict(Some(NeedWord::Login), held));
    }
    let (next, _next_store, _hand) = hand_over(&h, &store, 9_000, true);
    assert_eq!(
        shown(&h).last().map(String::as_str),
        Some("login stuck bridge"),
        "the pending tuple, written before the carry"
    );
    let head = next.ctx.timeline.lock().unwrap().slog().log().last_seq();
    let _ = publish(
        &next,
        verdict(Some(NeedWord::Login), Some(HoldWord::Bridge)),
    );
    assert!(
        shown_after(&next, head).is_empty(),
        "the successor restates the carried tuple: {:?}",
        shown_after(&next, head)
    );
    let _ = publish(&next, verdict(Some(NeedWord::Login), None));
    assert_eq!(
        shown_after(&next, head),
        ["login stuck -"],
        "a move is news"
    );
}

/// THE SHOWN WORDS ARE THE LOG'S: every printed need word, severity and
/// hold word is in its key's list, so every tuple's mark carries each of
/// its fields. NEGATIVE CONTROL: a word outside a list is refused.
#[test]
fn the_shown_words_are_the_logs() {
    for w in NeedWord::ALL {
        assert!(aterm_slog::vocab::NEED_WORDS.contains(&w.word()), "{w:?}");
    }
    assert_eq!(
        aterm_slog::vocab::SEV_WORDS,
        [Severity::Stuck, Severity::Decide].map(Severity::word)
    );
    assert_eq!(
        aterm_slog::vocab::HELD_WORDS,
        HoldWord::ALL.map(HoldWord::word)
    );
    let (h, _store, hand) = rig(86);
    let mut n = 0;
    for (i, w) in NeedWord::ALL.into_iter().enumerate() {
        for held in HoldWord::ALL {
            n += 1;
            hand.set(1_000 * n);
            let _ = publish(&h, verdict(Some(w), Some(held)));
            let got = shown(&h);
            assert_eq!(
                got.last().cloned(),
                Some(format!(
                    "{} {} {}",
                    w.word(),
                    w.severity().word(),
                    held.word()
                )),
                "{i}"
            );
        }
    }
    assert!(Fields::new().word(Key::Held, "fleet").is_err());
}

/// THE MARK IS LOCAL AND JOURNALED, ONE COUNT LINE: a shown mark is no bus
/// class (the mirror refuses it), is in the journal's lines, and `brief`
/// counts the shown marks on a count line (`shown <n>`).
#[test]
fn the_shown_mark_is_local_journaled_and_one_brief_count_line() {
    let (h, _store, hand) = rig(87);
    for (i, w) in [NeedWord::Ask, NeedWord::Frozen].into_iter().enumerate() {
        hand.set(1_000 * (i as u64 + 1));
        let _ = publish(&h, verdict(Some(w), None));
    }
    let g = h.ctx.timeline.lock().unwrap();
    let log = g.slog().log();
    let marks: Vec<&Rec> = log.window().filter(|r| r.k == Kind::Shown).collect();
    assert_eq!(marks.len(), 2);
    for r in &marks {
        assert_eq!(
            aterm_slog::encode_mirror(r, Tier::Wire, log.ep(), None, &|_| None),
            Err(aterm_slog::EncodeError::NotBusClass)
        );
    }
    let sid = aterm_slog::ShortId::new("s1").expect("an id");
    let take = aterm_slog::journal::take(log, 0, usize::MAX, true);
    let journal: Vec<String> = aterm_slog::journal::take_frames(sid, &take, true, 0)
        .iter()
        .filter_map(aterm_slog::journal::encode_frame)
        .collect();
    assert_eq!(
        journal
            .iter()
            .filter(|l| l.contains(" mark - shown need="))
            .count(),
        2,
        "{journal:?}"
    );
    let brief = super::super::render_brief(
        log,
        "s-1",
        aterm_slog::Node::SESSION,
        None,
        aterm_slog::DEFAULT_BUDGET,
        g.slog().now().wall_ms,
    );
    assert!(brief.lines().any(|l| l == "shown 2"), "{brief}");
}

/// THE PRESENCE SWEEP'S PUBLICATION IS THE WRITER (`App::publish_need`): a
/// person's attention text (an ask) published is one shown mark on the
/// session; the same verdict published again is none.
#[test]
fn the_need_publication_writes_the_shown_mark() {
    let mut app = App::headless_for_test();
    let session = app
        .front_terminal(WindowId(0))
        .expect("a front terminal")
        .session;
    let pooled = app.pool.get(session).expect("pooled");
    let (ctx, term) = (
        std::sync::Arc::clone(&pooled.ctx),
        std::sync::Arc::clone(&pooled.term),
    );
    let reply =
        crate::control::meta_line_for_test(&app.store, &term, session, &ctx, "set attention look");
    assert_eq!(reply, "OK\n");
    let _ = app.publish_need(session);
    let _ = app.publish_need(session);
    let marks: Vec<String> = ctx
        .timeline
        .lock()
        .unwrap()
        .slog()
        .log()
        .window()
        .filter(|r| r.k == Kind::Shown)
        .map(|r| format!("{} {}", word(r, Key::Need), word(r, Key::Sev)))
        .collect();
    assert_eq!(marks, ["ask decide"]);
}

/// THE STATUS SWEEP WRITES THE NEWEST TUPLE ONCE A TOKEN FREES (U44; the
/// review of 6F-9): on an App's session, a verdict flapping 200 times in one
/// hand-set second writes the bucket, then the real status sweep
/// (`observe_session_statuses`, the pass that ticks the program and shown
/// buckets) a hand-set second later writes its newest tuple, `coalesced=`
/// the 179 it replaced, with no later move and no close to force it out.
/// NEGATIVE CONTROL (by scratch revert): a sweep that does not call
/// `shown_tick` leaves the newest tuple pending.
#[test]
fn the_status_sweep_writes_the_newest_shown_tuple_once_a_token_frees() {
    let mut app = App::headless_for_test();
    let session = app
        .front_terminal(WindowId(0))
        .expect("a front terminal")
        .session;
    let ctx = Arc::clone(&app.pool.get(session).expect("pooled").ctx);
    let hand = hand_clock(&ctx.timeline);
    hand.set(5_000);
    let head = ctx.timeline.lock().unwrap().slog().log().last_seq();
    for i in 0..200u64 {
        let _ = ctx.timeline.lock().unwrap().set_need(flap(i));
    }
    let burst = usize::try_from(PROGRAM_BURST).expect("small");
    assert_eq!(shown_in(&ctx.timeline, head).len(), burst, "the bucket");
    hand.set(6_000);
    let _ = app.observe_session_statuses(Instant::now());
    let got = shown_in(&ctx.timeline, head);
    assert_eq!(got.len(), burst + 1, "the sweep wrote the newest: {got:?}");
    assert_eq!(
        got.last().map(String::as_str),
        Some("question decide - coalesced=179"),
        "the newest tuple, the 179 it replaced counted: {got:?}"
    );
}

/// THE NEED PUBLICATION WAKES THE LOG'S READERS (the review of 6F-9): the
/// presence sweep's publication (`App::publish_need`) that moves the
/// printed tuple writes the shown mark and, in its hold, rings the
/// session's watchers, where a parked `await log` or `subscribe … log`
/// waits (a one-minute hang detector); `await log kind=shown` then answers
/// with the mark. NEGATIVE CONTROL (by scratch revert): a publication that
/// does not take the log's wake leaves the watchers unrung, and the wait
/// hangs.
#[test]
fn the_need_publication_wakes_the_logs_readers() {
    let mut app = App::headless_for_test();
    let session = app
        .front_terminal(WindowId(0))
        .expect("a front terminal")
        .session;
    let pooled = app.pool.get(session).expect("pooled");
    let (ctx, term) = (Arc::clone(&pooled.ctx), Arc::clone(&pooled.term));
    let reply =
        crate::control::meta_line_for_test(&app.store, &term, session, &ctx, "set attention look");
    assert_eq!(reply, "OK\n");
    let (ep, head) = {
        let g = ctx.timeline.lock().unwrap();
        (g.slog().log().ep(), g.slog().log().last_seq())
    };
    let sub = crate::session_watch::Subscription::single(&ctx.watchers);
    while sub.wait(Duration::ZERO) {}
    assert!(app.publish_need(session), "the verdict moved");
    assert!(
        sub.wait(aterm_spec::wait::MUST_HAPPEN),
        "the shown mark woke the session's watchers"
    );
    let reply = crate::control::cmd_await(
        &term,
        &app.store,
        session,
        &ctx,
        &format!("log kind=shown since={head} ep={ep} timeout=0"),
    );
    assert!(
        reply.starts_with("OK seq=") && reply.contains(" mark - shown need=ask sev=decide "),
        "{reply}"
    );
}
