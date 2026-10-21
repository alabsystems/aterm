// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! THE FABRIC'S WAITS ON THE PRODUCTION CLOCK. This process never selects the
//! test timing value (every unit test's session helper does,
//! `aterm_gui_timing::select_fast`), so the commit-fence bound a session reads
//! here is [`HOLD_FENCE_WAIT`] itself, as a shipped build reads it. That a
//! fenced verb waits its session's bound, and no other, is checked on a zero
//! and a short bound set on the session, and on the production bound read off
//! the waiter it parks ([`SessionFabric::fence_bound`]), so the test does not
//! wait the production length out.

use std::time::{Duration, Instant};

use aterm_gui_fabric::{FabricSession, HOLD_BUSY, HOLD_FENCE_WAIT, SessionFabric, cmd_topic};
use aterm_session::SessionId;

/// A session with nothing but its fabric endpoint.
struct Session {
    id: SessionId,
    fabric: SessionFabric,
}

impl FabricSession for Session {
    fn self_id(&self) -> &SessionId {
        &self.id
    }
    fn fabric(&self) -> &SessionFabric {
        &self.fabric
    }
    fn record(&self, _kind: &'static str, _payload: String) {}
    fn fabric_changed(&self) {}
    fn holder_token(&self) -> Option<String> {
        None
    }
}

/// A session's fence bound is the production one, and a topic change under a
/// committing update's fence waits its session's bound, then answers busy with
/// nothing changed. CONTROL: with the fence lifted, the same change lands at
/// once. AND THE PRODUCTION BOUND ITSELF: on a session whose bound is its own,
/// an add and a drop each park under [`HOLD_FENCE_WAIT`], and land as the
/// fence lifts — so a verb that waited any other bound at the production
/// value alone, which neither set bound shows, fails here.
#[test]
fn a_fenced_change_waits_the_production_bound_then_answers_busy() {
    if aterm_gui_timing::current().is_fast() {
        aterm_spec::host::could_not_run(format_args!(
            "{} is set in this test's environment, so the process runs the test values, not \
             the production clock",
            aterm_gui_timing::DEV_FAST_TIMERS
        ));
    }
    let session = Session {
        id: SessionId::generate(),
        fabric: SessionFabric::default(),
    };
    assert_eq!(
        aterm_gui_timing::current(),
        aterm_gui_timing::Timing::PRODUCTION
    );
    assert_eq!(session.fabric.fence_wait(), HOLD_FENCE_WAIT);
    session.fabric.fence_hold(false);
    // The verb waits the bound its session reads, whatever its length: a zero
    // bound answers busy without parking on the fence, and a short one is
    // waited out first, so a verb waiting any bound but its session's fails
    // one of the two.
    session.fabric.set_fence_wait(Duration::ZERO);
    assert_eq!(cmd_topic(&session, "add news"), HOLD_BUSY);
    assert_eq!(session.fabric.fence_waits(), 0, "a zero bound never parks");
    let bound = Duration::from_millis(50);
    session.fabric.set_fence_wait(bound);
    let started = Instant::now();
    assert_eq!(cmd_topic(&session, "add news"), HOLD_BUSY);
    let waited = started.elapsed();
    assert!(
        waited >= bound,
        "answered busy after {waited:?}, inside the {bound:?} bound"
    );
    assert!(session.fabric.fence_waits() >= 1, "it parked on the fence");
    let listed = cmd_topic(&session, "ls");
    assert!(listed.starts_with("OK 0 "), "nothing changed: {listed}");
    session.fabric.lift_hold_fence();
    let added = cmd_topic(&session, "add news");
    assert!(added.starts_with("OK news "), "{added}");
    let listed = cmd_topic(&session, "ls");
    assert!(listed.contains("topic news "), "{listed}");

    let own = Session {
        id: SessionId::generate(),
        fabric: SessionFabric::default(),
    };
    for (verb, landed) in [("add news", "OK news "), ("drop news", "OK news dropped=1")] {
        own.fabric.fence_hold(false);
        let before = own.fabric.fence_waits();
        std::thread::scope(|s| {
            let waiter = s.spawn(|| cmd_topic(&own, verb));
            let ended = aterm_spec::wait::within(
                aterm_spec::wait::MUST_HAPPEN,
                Duration::from_millis(1),
                || (own.fabric.fence_waits() > before || waiter.is_finished()).then_some(()),
            );
            assert!(
                ended.is_some() && own.fabric.fence_waits() > before,
                "{verb}: it blocks on the fence"
            );
            assert_eq!(
                own.fabric.fence_bound(),
                Some(Some(HOLD_FENCE_WAIT)),
                "{verb}: it waits the production bound"
            );
            own.fabric.lift_hold_fence();
            let reply = waiter.join().unwrap();
            assert!(
                reply.starts_with(landed),
                "{verb}: it lands as the fence lifts: {reply}"
            );
        });
    }
}
