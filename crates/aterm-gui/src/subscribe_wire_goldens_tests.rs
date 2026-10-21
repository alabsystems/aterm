// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! THE `events` DIGEST'S GOLDEN (plan 5B-5): the timeline frames
//! [`drain_timeline_events`] pushes, byte for byte, over the fixed script of
//! one row of every timeline kind (`control`'s `wire_goldens`, beside the
//! `timeline` and `exits` goldens), from no watermark, past an id, and from
//! a watermark below the retained low (the `GAP` line), every row through
//! its production writer but the two the script names. Nothing is masked:
//! a frame carries no clock.
//!
//! NEGATIVE CONTROL: the script with one payload byte planted fails it.

use super::drain_timeline_events;
use crate::control::wire_goldens::{scripted, title};
use crate::session_store::{new_store, test_handle};
use crate::session_timeline::TIMELINE_CAP;

/// The `events` digest's timeline frames the golden holds: from no
/// watermark, past id 6, and from a watermark below the retained low.
fn events_text(plant: bool) -> String {
    let h = scripted(plant);
    let mut out = String::new();
    for (name, wm) in [("none", None), ("6", Some(6))] {
        let mut frames = String::new();
        let high = drain_timeline_events(&h.ctx.timeline.lock().unwrap(), "5", wm, &mut frames);
        out.push_str(&format!("> watermark={name} -> {high:?}\n{frames}"));
    }
    // A watcher fallen behind the ring: TIMELINE_CAP rows the digest skips,
    // then three it carries, each through its production writer (the store's
    // title, the `meta` verb, the agent sweep's publication, a delivery).
    let store = new_store();
    let b = test_handle(6);
    store
        .write()
        .unwrap_or_else(|p| p.into_inner())
        .register(b.clone());
    for i in 0..TIMELINE_CAP {
        store
            .write()
            .unwrap_or_else(|p| p.into_inner())
            .set_title(6, &format!("t{i}"));
    }
    let reply = crate::control::meta_line_for_test(
        &store,
        &b.term,
        b.local_id,
        &b.ctx,
        &format!("set attention {}", title(plant)),
    );
    assert_eq!(reply, "OK\n", "the meta verb");
    let stamp = crate::session_timeline::AgentStamp {
        generation: crate::control::ScreenGen { epoch: 0, seq: 2 },
        fp: 0xbb,
    };
    let _ = b
        .ctx
        .timeline
        .lock()
        .unwrap_or_else(|p| p.into_inner())
        .publish_agent(
            "idle",
            None,
            None,
            Some(aterm_phase::Program::Claude),
            stamp,
        );
    let line = format!(
        "{} off=44 from=s-a kind=ask trust=agent text=SENTINEL",
        b.ctx.self_id.as_str()
    );
    let reply = crate::fabric::cmd_deliver_waking(&store, &line, &|_| {});
    assert_eq!(reply, "OK 1\n", "the delivery");
    let behind = &b.ctx.timeline;
    let mut frames = String::new();
    let high = drain_timeline_events(&behind.lock().unwrap(), "6", Some(1), &mut frames);
    out.push_str(&format!(
        "> watermark=1 (below the low) -> {high:?}\n{frames}"
    ));
    out
}

const EVENTS_GOLDEN: &str = concat!(
    "> watermark=none -> Some(26)\n",
    "EVENT 5 meta field=attention value=build%20agent\n",
    "EVENT 5 agent busy rev=1 gen=0.1 fp=00000000000000aa\n",
    "EVENT 5 task rev=1 state=working attention=0\n",
    "EVENT 5 status phase=idle outcome=failure exit_code=1 signal=- confidence=exact\n",
    "EVENT 5 human\n",
    "EVENT 5 render desync-risk run=1 displaced=+2 net=zero at=5\n",
    "EVENT 5 harness-policy gen=2\n",
    "EVENT 5 inbox 1 from=s-a kind=ask off=40\n",
    "EVENT 5 inbox-seen 1 off=40 floor=41 seen=1 verdict=handled kind=ask from=s-a\n",
    "EVENT 5 post 1 to=@s-b kind=answer re=1\n",
    "EVENT 5 post-landed 1 off=41\n",
    "EVENT 5 hold 1 reason=owner origin=local hold_origin=local\n",
    "EVENT 5 fetch off=40\n",
    "EVENT 5 topic add news since=head\n",
    "EVENT 5 closing reason=ctl-close by=ctl\n",
    "> watermark=6 -> Some(26)\n",
    "EVENT 5 status phase=idle outcome=failure exit_code=1 signal=- confidence=exact\n",
    "EVENT 5 human\n",
    "EVENT 5 render desync-risk run=1 displaced=+2 net=zero at=5\n",
    "EVENT 5 harness-policy gen=2\n",
    "EVENT 5 inbox 1 from=s-a kind=ask off=40\n",
    "EVENT 5 inbox-seen 1 off=40 floor=41 seen=1 verdict=handled kind=ask from=s-a\n",
    "EVENT 5 post 1 to=@s-b kind=answer re=1\n",
    "EVENT 5 post-landed 1 off=41\n",
    "EVENT 5 hold 1 reason=owner origin=local hold_origin=local\n",
    "EVENT 5 fetch off=40\n",
    "EVENT 5 topic add news since=head\n",
    "EVENT 5 closing reason=ctl-close by=ctl\n",
    "> watermark=1 (below the low) -> Some(516)\n",
    "GAP 6 events-dropped=3\n",
    "EVENT 6 meta field=attention value=vim%20build\n",
    "EVENT 6 agent idle rev=1 gen=0.2 fp=00000000000000bb\n",
    "EVENT 6 inbox 1 from=s-a kind=ask off=44\n",
);

/// The `events` digest's timeline frames, byte for byte.
#[test]
fn events_digest_golden() {
    assert_eq!(events_text(false), EVENTS_GOLDEN);
    assert_ne!(
        events_text(true),
        EVENTS_GOLDEN,
        "NEGATIVE CONTROL: a planted payload byte"
    );
}
