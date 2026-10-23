// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! THE FABRIC'S WAITS ON THE PRODUCTION CLOCK.

use std::time::{Duration, Instant};

use aterm_gui_fabric::{
    FabricFact, FabricSession, HOLD_BUSY, HOLD_FENCE_WAIT, SessionFabric, cmd_topic,
};
use aterm_session::SessionId;

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
    fn record_fact(&self, _kind: &'static str, _fact: FabricFact<'_>) -> bool {
        false
    }
    fn fabric_changed(&self) {}
    fn holder_token(&self) -> Option<String> {
        None
    }
}

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
