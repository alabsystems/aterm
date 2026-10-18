// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! THE FABRIC'S WAITS ON THE PRODUCTION CLOCK: the one real-clock test of the
//! family. This process never selects the test timing value (every unit test's
//! session helper does, `aterm_gui_timing::select_fast`), so the commit-fence
//! bound a fenced verb waits here is [`HOLD_FENCE_WAIT`] itself, as a shipped
//! build waits it.

use std::time::Instant;

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

/// A topic change under a committing update's fence waits the production
/// bound, then answers busy with nothing changed. CONTROL: with the fence
/// lifted, the same change lands at once.
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
    assert_eq!(session.fabric.fence_wait(), HOLD_FENCE_WAIT);
    session.fabric.fence_hold(false);
    let started = Instant::now();
    assert_eq!(cmd_topic(&session, "add news").0, HOLD_BUSY);
    let waited = started.elapsed();
    assert!(
        waited >= HOLD_FENCE_WAIT,
        "answered busy after {waited:?}, inside the {HOLD_FENCE_WAIT:?} bound"
    );
    assert!(session.fabric.fence_waits() >= 1, "it parked on the fence");
    let listed = cmd_topic(&session, "ls").0;
    assert!(listed.starts_with("OK 0 "), "nothing changed: {listed}");
    session.fabric.lift_hold_fence();
    let added = cmd_topic(&session, "add news").0;
    assert!(added.starts_with("OK news "), "{added}");
    let listed = cmd_topic(&session, "ls").0;
    assert!(listed.contains("topic news "), "{listed}");
}
