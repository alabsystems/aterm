// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! The bridge's prompt wake, and the roster backstop behind it: a queued bus fetch or a
//! post wakes the bridge through its own push event, and when that event is lost the
//! roster round still delivers the post.

use crate::harness;

use aterm_models_session::fabric_outbox_wake_model;
use harness::{FLEET, World, until, watchers};

/// A newly parked bus fetch wakes the bridge through its `fetch` timeline
/// event. The test fault disables only the periodic outbox drain, so the reply
/// proves that the real prompt event path carried the request.
#[test]
fn a_queued_fetch_has_a_prompt_event_without_the_roster_drain() {
    let w = World::boot_with(
        "r2fetch",
        &[],
        &[("ATERM_LINK_FAULT", "skip-roster-outbox-while-marked")],
    );
    w.wait_ready();
    let (_a, b) = w.two_sessions();
    let presence = format!("/f/{FLEET}/pub/{}/{b}/presence", w.node);
    until("the bridge to adopt the fetcher's session", || {
        w.row(&presence)
            .filter(|body| body.contains("state=live"))
            .map(|_| ())
    });
    // And the push lane must watch it: the roster read can list the session before
    // the push lane adopts it, and a watch pushes only what is recorded after its
    // seed, so with the roster drain off an earlier fetch event reaches nothing.
    // `watchers>=1` is aterm's word that the seed happened.
    until("the push lane to watch the fetcher's session", || {
        (watchers(&w, &b)? >= 1).then_some(())
    });
    std::fs::write(w.state.join("skip-roster-outbox"), b"1\n").unwrap();

    let off = 1_000_000;
    // A hang detector, not a deadline: the endpoint bounds one parked read at 10 s
    // (`ERR timeout off=`), so a loaded machine asks again. Promptness is proved by
    // which path answered (the roster drain is off, `fault-fired` says the event
    // arrived), not by a clock.
    let reply = until("the pushed fetch to be answered", || {
        let reply = w.verb(&format!("@{b} inbox get @{off}"));
        (!reply.header().starts_with("ERR timeout")).then_some(reply)
    });
    let timeline = w.verb(&format!("@{b} timeline 20"));
    let events = timeline.rows().join("\n");
    assert!(
        reply
            .header()
            .starts_with(&format!("ERR no such record off={off}")),
        "the pushed fetch was answered through the broker: {} — event_fired={} timeline:\n{events}\nlog:\n{}",
        reply.header(),
        w.state.join("fault-fired").exists(),
        w.log_tail()
    );
    assert!(
        events.contains(&format!("kind=fetch off={off}")),
        "the endpoint recorded the push event: {events}"
    );
    assert!(
        w.state.join("fault-fired").exists(),
        "the bridge received the fetch event, rather than a roster fallback"
    );

    let model = fabric_outbox_wake_model();
    let mut state = model.init_state();
    assert!(model.fire("Queue", &mut state), "the real fetch was queued");
    assert!(
        model.action_enabled("PromptDrain", &state),
        "the real fetch event enables the prompt drain"
    );
    assert!(model.fire("PromptDrain", &mut state));
    assert_eq!(state["delivered"], 1);
}

/// A post whose push event was lost still reaches its recipient on the roster
/// backstop. The marker holds the fault open until the bridge acknowledges the
/// dropped event, so a normal post event cannot make this test pass by accident.
#[test]
fn a_lost_post_event_is_delivered_by_the_roster_backstop() {
    // The round IS the subject, so it runs at test pace
    // ([`harness::FAST_TIMERS`]).
    let w = World::boot_with(
        "r2postdrop",
        &[],
        &[
            ("ATERM_LINK_FAULT", "drop-post-event-while-marked"),
            harness::FAST_TIMERS,
        ],
    );
    w.wait_ready();
    let (a, b) = w.two_sessions();
    std::fs::write(w.state.join("drop-post-event"), b"1\n").unwrap();

    let posted = w.verb(&format!("@{a} post to=@{b} kind=note backstop-only"));
    assert!(posted.ok(), "{}", posted.header());
    let model = fabric_outbox_wake_model();
    let mut state = model.init_state();
    assert!(model.fire("Queue", &mut state), "the real post was queued");
    until("the bridge to discard the prompt post event", || {
        w.state.join("fault-fired").exists().then_some(())
    });
    assert!(model.fire("LoseEvent", &mut state));
    let _ = std::fs::remove_file(w.state.join("drop-post-event"));
    // A hang detector: the backstop is the only path left, and it runs every
    // roster round (500 ms at test pace), so a minute is a hundred rounds — and
    // the wait ends at the first one that delivers.
    let row = until(
        "the roster backstop to deliver a post without its event",
        || {
            w.inbox(&b)
                .into_iter()
                .find(|r| r.contains("backstop-only"))
        },
    );
    assert!(row.contains("kind=note"), "{row}");
    for _ in 0..2 {
        assert!(model.fire("BackstopTick", &mut state));
    }
    assert_eq!(state["delivered"], 1, "the real inbox received the post");
}
