// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! GOLDENS FOR THE WIRES PHASE 5 MUST NOT MOVE (plan 5B-5): the `timeline`
//! reply and the `exits` reply here, the `events` digest's timeline frames
//! in `subscribe`'s `wire_goldens` (on this script), byte for byte, over a fixed script that writes one row of every timeline kind
//! (plan §0.1). Later Phase 5 steps add typed records beside these rows;
//! these goldens are what says the rows and their wire stayed put.
//!
//! Masked, and only because the process supplies them: the `t=` digits (the
//! process clock) and, in `exits`, the minted sids (named `<sidN>` in
//! registration order). Nothing else is masked: `kind=human`'s trailing
//! space is in the golden.
//!
//! The script writes each row through its production writer ([`scripted`]
//! names them), so a writer whose bytes move moves these goldens; only the
//! two rows whose writers no unit test reaches (`fetch`, `hold-advisory`)
//! and the update's `handoff` (whose writer starts the log over) are
//! hand-written, and named. The replies are read from the session log's
//! view, so a hand-written row is a `row` mark and its ring row
//! (`SessionTimeline::record_hand_row`).
//!
//! NEGATIVE CONTROLS: the script with one payload byte planted fails each
//! golden.

use std::sync::Arc;
use std::time::Instant;

use super::control_session::{cmd_exits, cmd_timeline};
use crate::session_store::{
    ExitActor, ExitReason, SessionHandle, SessionHandoff, SessionState, SessionStore, new_store,
    test_handle,
};
use crate::task_record::{TaskMark, TaskState};

/// `t=<digits>` as `t=*`: the process clock's reading.
pub(crate) fn mask_t(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    let mut rest = s;
    while let Some(i) = rest.find("t=") {
        let boundary = i == 0 || rest.as_bytes()[i - 1] == b' ';
        out.push_str(&rest[..i + 2]);
        rest = &rest[i + 2..];
        if boundary {
            let n = rest.bytes().take_while(u8::is_ascii_digit).count();
            if n > 0 {
                out.push('*');
                rest = &rest[n..];
            }
        }
    }
    out.push_str(rest);
    out
}

/// The script's title: one byte of it is the planted change.
pub(crate) fn title(plant: bool) -> &'static str {
    if plant { "vim bui1d" } else { "vim build" }
}

/// One row of every timeline kind, on a session the store registered and,
/// last, retired, each through its production writer but two (below).
/// Returns the session.
///
/// The writers: the store's `register`, `set_state` and
/// `deregister_local_as`; the `meta` verb; the timeline's `record_title`
/// (the drift flush's), `record_cwd_change`, `publish_agent` (the agent sweep's publication),
/// `mark_task`, `record_render` (the resize ledger's), `note_harness_act`,
/// `note_program_state`; the status sweep's payload
/// ([`crate::session_status::status_event_payload`], a failed command at
/// rest, recorded as the sweep records it); a person's key
/// ([`crate::app_input::note_person`]); the manual reset's
/// [`crate::manual_reset::record`]; [`crate::pty_idem::record_in_doubt`];
/// the policy cell's
/// [`crate::harness_policy_verb::record_policy_move`]; and the fabric's
/// verbs (a delivery, its `inbox seen`, a post answering it, its landing, a
/// hold, a topic). HAND-WRITTEN, the two rows whose writers no unit test
/// reaches (the overlap table names them, `every_named_difference_still_holds`):
/// `fetch` (the bridge's fetch) and `hold-advisory` (the bridge's lost-link
/// advisory); and the update's `handoff`, whose writer
/// ([`crate::spawn::mark_handoff_gap`]) starts the session log over, so the
/// view, which every reader reads, would begin at it.
pub(crate) fn scripted(plant: bool) -> SessionHandle {
    let store = new_store();
    let h = test_handle(5);
    let ctx = Arc::clone(&h.ctx);
    let sid = ctx.self_id.as_str().to_string();
    let wake = |_: &aterm_session::SessionId| {};
    {
        let mut g = store.write().unwrap_or_else(|p| p.into_inner());
        g.register(h.clone()); // spawned
        g.set_title(5, title(plant));
    }
    // title-change: the drift flush's writer, the title mark's row.
    assert!(
        ctx.timeline
            .lock()
            .unwrap_or_else(|p| p.into_inner())
            .record_title(title(plant))
    );
    let reply = crate::control::meta_line_for_test(
        &store,
        &h.term,
        h.local_id,
        &ctx,
        "set attention build agent",
    );
    assert_eq!(reply, "OK\n", "the meta verb");
    {
        let mut tl = ctx.timeline.lock().unwrap_or_else(|p| p.into_inner());
        tl.record_cwd_change(Some("/tmp/a b"));
        let stamp = crate::session_timeline::AgentStamp {
            generation: crate::control::ScreenGen { epoch: 0, seq: 1 },
            fp: 0xaa,
        };
        let _ = tl.publish_agent(
            "busy",
            None,
            None,
            Some(aterm_phase::Program::Claude),
            stamp,
        );
        let _ = tl.mark_task(&TaskMark::new(TaskState::Working), false, Instant::now(), 1);
        let failed = crate::session_status::Status {
            phase: crate::session_status::Phase::Idle,
            last_outcome: crate::session_status::Outcome::Failure { exit_code: 1 },
            since: Instant::now(),
            detail: None,
            confidence: crate::session_status::Confidence::Exact,
            reasons: Vec::new(),
            conflict: false,
        };
        tl.record(
            crate::session_status::STATUS_CHANGE,
            crate::session_status::status_event_payload(&failed),
        );
        // THE SESSION LOG's typed `status` mark, in the same hold, as the
        // sweep writes it (the view prints the row from it).
        let at = tl.slog().now();
        tl.slog_mut()
            .observe_status(crate::session_status::status_facts(&failed), at);
    }
    let _ = crate::app_input::note_person(&ctx, crate::human_input::Gesture::Other);
    let _ = ctx
        .timeline
        .lock()
        .unwrap_or_else(|p| p.into_inner())
        .record_render(&[crate::resize_ledger::Transition::Risk {
            run: 1,
            displaced: 2,
            net_zero: true,
            at_ms: 5,
        }]);
    crate::manual_reset::record(
        5,
        &ctx.timeline,
        crate::manual_reset::ResetSource::Menu,
        &crate::manual_reset::ResetReport {
            reverted: vec!["mouse", "paste"],
            bytes: 12,
        },
    );
    let key = crate::pty_idem::parse_key(&format!("{}:3:9", ctx.nonce.to_hex()), ctx.nonce)
        .expect("a key");
    let _ = crate::pty_idem::record_in_doubt(&ctx, "send", key, "ERR busy\n");
    // HAND-WRITTEN: the update's row, whose writer
    // (`spawn::mark_handoff_gap`) starts the session log over, so a
    // successor's view begins at it; this one session keeps its rows on
    // either side of it.
    ctx.timeline
        .lock()
        .unwrap_or_else(|p| p.into_inner())
        .record_hand_row(
            "handoff",
            crate::session_timeline::HandoffGap {
                from_build: Some(7),
            }
            .payload(),
        );
    {
        let mut tl = ctx.timeline.lock().unwrap_or_else(|p| p.into_inner());
        let _ = tl.note_harness_act("approve", "codex-alab", aterm_slog::Actor::Ctl);
        let _ = tl.note_program_state(None);
    }
    crate::harness_policy_verb::record_policy_move(&ctx.timeline, 2);
    let reply = crate::fabric::cmd_deliver_waking(
        &store,
        &format!("{sid} off=40 from=s-a kind=ask trust=agent text=SENTINEL"),
        &wake,
    );
    assert_eq!(reply, "OK 1\n", "the delivery");
    let reply = crate::fabric::cmd_inbox_seen(&ctx, "1 handled");
    assert!(reply.starts_with("OK seen=1"), "{reply}");
    let reply = crate::fabric::cmd_post(
        &ctx,
        "to=@s-b kind=answer re=1 --wait=0 SENTINEL-ANSWER",
        None,
    );
    // Queued (post id 1) whether or not a bridge is up: `OK 1`, or an `ERR`
    // naming it (`fabric absent id=1`, `timeout id=1`).
    assert!(
        reply.starts_with("OK 1") || reply.split_whitespace().any(|t| t == "id=1"),
        "the post: {reply}"
    );
    let reply = crate::fabric::cmd_outbox_sent_waking(&store, &format!("{sid} 1 off=41"), &wake);
    assert!(reply.starts_with("OK"), "the landing: {reply}");
    let reply = crate::fabric::cmd_hold_waking(
        &store,
        &format!("{sid} on reason=owner"),
        crate::fabric::HoldIssuer::Owner,
        &wake,
    );
    assert!(reply.starts_with("OK hold="), "the hold: {reply}");
    {
        let mut tl = ctx.timeline.lock().unwrap_or_else(|p| p.into_inner());
        // HAND-WRITTEN: the bridge's fetch, which no unit test reaches.
        tl.record_hand_row("fetch", "off=40".to_string());
    }
    assert_eq!(
        ctx.fabric.topic_add("news", "head", &ctx.timeline),
        Ok(true)
    );
    {
        let mut tl = ctx.timeline.lock().unwrap_or_else(|p| p.into_inner());
        // HAND-WRITTEN: the bridge's lost-link advisory, which no unit test
        // reaches.
        tl.record_hand_row(
            "hold-advisory",
            "1 reason=bridge-lost origin=fabric".to_string(),
        );
    }
    let mut g = store.write().unwrap_or_else(|p| p.into_inner());
    g.set_state(5, SessionState::Exited); // state-change
    g.deregister_local_as(5, ExitReason::CtlClose, ExitActor::Ctl); // closing, state-change
    drop(g);
    h
}

/// The `timeline` replies the golden holds: all, the newest 3, past id 20,
/// and an argument the grammar refuses.
fn timeline_text(plant: bool) -> String {
    let h = scripted(plant);
    let mut out = String::new();
    for rest in ["", "3", "since=20", "since=x"] {
        out.push_str(&format!("> timeline {rest}\n"));
        out.push_str(&mask_t(&cmd_timeline(&h.ctx, rest)));
    }
    out
}

const TIMELINE_GOLDEN: &str = concat!(
    "> timeline \n",
    "OK 26\n",
    "event 1 t=* kind=spawned state=alive\n",
    "event 2 t=* kind=title-change title=vim%20build\n",
    "event 3 t=* kind=meta-change field=attention value=build%20agent\n",
    "event 4 t=* kind=cwd-change cwd=/tmp/a%20b\n",
    "event 5 t=* kind=agent-change busy rev=1 gen=0.1 fp=00000000000000aa\n",
    "event 6 t=* kind=task-change rev=1 state=working attention=0\n",
    "event 7 t=* kind=status-change phase=idle outcome=failure exit_code=1 signal=- confidence=exact\n",
    "event 8 t=* kind=human \n",
    "event 9 t=* kind=render desync-risk run=1 displaced=+2 net=zero at=5\n",
    "event 10 t=* kind=modes-restored reason=manual source=menu reverted=mouse,paste bytes=12\n",
    "event 11 t=* kind=in-doubt producer=3 seq=9 reply=ERR-busy\n",
    "event 12 t=* kind=handoff carried=0 from_build=7\n",
    "event 13 t=* kind=harness-act act=approve holder=codex-alab\n",
    "event 14 t=* kind=program-state state=- kind=-\n",
    "event 15 t=* kind=harness-policy gen=2\n",
    "event 16 t=* kind=inbox 1 from=s-a kind=ask off=40\n",
    "event 17 t=* kind=inbox-seen 1 off=40 floor=41 seen=1 verdict=handled kind=ask from=s-a\n",
    "event 18 t=* kind=post 1 to=@s-b kind=answer re=1\n",
    "event 19 t=* kind=post-landed 1 off=41\n",
    "event 20 t=* kind=hold 1 reason=owner origin=local hold_origin=local\n",
    "event 21 t=* kind=fetch off=40\n",
    "event 22 t=* kind=topic add news since=head\n",
    "event 23 t=* kind=hold-advisory 1 reason=bridge-lost origin=fabric\n",
    "event 24 t=* kind=state-change state=exited\n",
    "event 25 t=* kind=closing reason=ctl-close by=ctl\n",
    "event 26 t=* kind=state-change state=closed\n",
    "> timeline 3\n",
    "OK 3\n",
    "event 24 t=* kind=state-change state=exited\n",
    "event 25 t=* kind=closing reason=ctl-close by=ctl\n",
    "event 26 t=* kind=state-change state=closed\n",
    "> timeline since=20\n",
    "OK 6\n",
    "event 21 t=* kind=fetch off=40\n",
    "event 22 t=* kind=topic add news since=head\n",
    "event 23 t=* kind=hold-advisory 1 reason=bridge-lost origin=fabric\n",
    "event 24 t=* kind=state-change state=exited\n",
    "event 25 t=* kind=closing reason=ctl-close by=ctl\n",
    "event 26 t=* kind=state-change state=closed\n",
    "> timeline since=x\n",
    "ERR usage: timeline [<n>] [since=<id>]\n",
);

/// `timeline`, byte for byte, over one row of every kind.
#[test]
fn timeline_reply_golden() {
    assert_eq!(timeline_text(false), TIMELINE_GOLDEN);
    assert_ne!(
        timeline_text(true),
        TIMELINE_GOLDEN,
        "NEGATIVE CONTROL: a planted payload byte"
    );
}

/// Every minted sid in `text` named `<sidN>`, in the order given.
fn mask_sids(text: &str, sids: &[String]) -> String {
    let mut out = text.to_string();
    for (i, sid) in sids.iter().enumerate() {
        out = out.replace(sid.as_str(), &format!("<sid{i}>"));
    }
    out
}

/// The `exits` replies the golden holds: every reason, each actor kind and
/// both exit-code cases; the window, the cursor and the usage refusal; and
/// over a seamless update, the successor's page past the producer's last
/// row.
fn exits_text(plant: bool) -> String {
    let store = new_store();
    let mut sids = Vec::new();
    {
        let mut g = store.write().unwrap_or_else(|p| p.into_inner());
        for (i, reason) in ExitReason::ALL.iter().enumerate() {
            let local = i as u64 + 10;
            let h = test_handle(local);
            sids.push(h.sid.as_str().to_string());
            g.register(h);
            let driver = if plant { "s-drivex" } else { "s-driver" };
            let actor = match i % 5 {
                0 => ExitActor::Sid(format!("{driver}{i}")),
                1 => ExitActor::Human,
                2 => ExitActor::Ctl,
                3 => ExitActor::Bridge,
                _ => ExitActor::Unknown,
            };
            if i % 2 == 0 {
                g.note_exit_code(local, Some(i32::try_from(i).unwrap_or(0)));
            }
            g.deregister_local_as(local, *reason, actor);
        }
    }
    let mut out = String::new();
    for rest in ["", "2", "since=4", "since=x"] {
        out.push_str(&format!("> exits {rest}\n"));
        out.push_str(&cmd_exits(&store, rest));
    }
    // Over an update: the successor's ids go on above the producer's.
    let mut producer = SessionStore::default();
    for local in 1..=3 {
        let h = test_handle(local);
        sids.push(h.sid.as_str().to_string());
        producer.register(h);
    }
    producer.deregister_local_as(3, ExitReason::ShellExit, ExitActor::Human);
    let manifest = SessionHandoff::from_store(&producer);
    let read =
        SessionHandoff::from_toml(&manifest.to_toml().expect("a manifest")).expect("it reads");
    let successor = new_store();
    {
        let mut g = successor.write().unwrap_or_else(|p| p.into_inner());
        g.continue_roster_seq(read.roster_seq.expect("the producer's count"));
        for rec in &read.sessions {
            let h = test_handle(rec.local_id);
            sids.push(h.sid.as_str().to_string());
            g.register(h);
        }
        g.deregister_local_as(1, ExitReason::UiClose, ExitActor::Human);
        g.deregister_local_as(2, ExitReason::WindowClose, ExitActor::Ctl);
    }
    for rest in ["", "since=4", "1"] {
        out.push_str(&format!("> successor exits {rest}\n"));
        out.push_str(&cmd_exits(&successor, rest));
    }
    mask_sids(&mask_t(&out), &sids)
}

const EXITS_GOLDEN: &str = concat!(
    "> exits \n",
    "OK 6\n",
    "exit 2 t=* sid=<sid0> local=10 reason=shell-exit exit_code=0 by=s-driver0\n",
    "exit 4 t=* sid=<sid1> local=11 reason=ctl-close exit_code=- by=human\n",
    "exit 6 t=* sid=<sid2> local=12 reason=ui-close exit_code=2 by=ctl\n",
    "exit 8 t=* sid=<sid3> local=13 reason=window-close exit_code=- by=bridge\n",
    "exit 10 t=* sid=<sid4> local=14 reason=app-quit exit_code=4 by=-\n",
    "exit 12 t=* sid=<sid5> local=15 reason=unknown exit_code=- by=s-driver5\n",
    "> exits 2\n",
    "OK 2\n",
    "exit 10 t=* sid=<sid4> local=14 reason=app-quit exit_code=4 by=-\n",
    "exit 12 t=* sid=<sid5> local=15 reason=unknown exit_code=- by=s-driver5\n",
    "> exits since=4\n",
    "OK 4\n",
    "exit 6 t=* sid=<sid2> local=12 reason=ui-close exit_code=2 by=ctl\n",
    "exit 8 t=* sid=<sid3> local=13 reason=window-close exit_code=- by=bridge\n",
    "exit 10 t=* sid=<sid4> local=14 reason=app-quit exit_code=4 by=-\n",
    "exit 12 t=* sid=<sid5> local=15 reason=unknown exit_code=- by=s-driver5\n",
    "> exits since=x\n",
    "ERR usage: exits [<n>] [since=<id>]\n",
    "> successor exits \n",
    "OK 2\n",
    "exit 7 t=* sid=<sid9> local=1 reason=ui-close exit_code=- by=human\n",
    "exit 8 t=* sid=<sid10> local=2 reason=window-close exit_code=- by=ctl\n",
    "> successor exits since=4\n",
    "OK 2\n",
    "exit 7 t=* sid=<sid9> local=1 reason=ui-close exit_code=- by=human\n",
    "exit 8 t=* sid=<sid10> local=2 reason=window-close exit_code=- by=ctl\n",
    "> successor exits 1\n",
    "OK 1\n",
    "exit 8 t=* sid=<sid10> local=2 reason=window-close exit_code=- by=ctl\n",
);

/// `exits`, byte for byte, over every reason and actor and an update.
#[test]
fn exits_reply_golden() {
    assert_eq!(exits_text(false), EXITS_GOLDEN);
    assert_ne!(
        exits_text(true),
        EXITS_GOLDEN,
        "NEGATIVE CONTROL: a planted payload byte"
    );
}
