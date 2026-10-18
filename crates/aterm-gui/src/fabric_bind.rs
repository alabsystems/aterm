// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! The fabric endpoint, `crates/aterm-gui-fabric`, under its old
//! module name, and what binds it to this crate: a session is a
//! [`crate::SessionCtx`] ([`FabricSession`]), the registry is the
//! [`crate::session_store::SessionStore`] ([`Registry`]), a timeline is a
//! [`crate::session_timeline::SessionTimeline`] ([`Timeline`]). Every item of
//! the endpoint is reachable as `crate::fabric::…`, as before the split.

use std::sync::Arc;

use aterm_session::SessionId;

pub(crate) use aterm_gui_fabric::*;

impl FabricSession for crate::SessionCtx {
    fn self_id(&self) -> &SessionId {
        &self.self_id
    }

    fn fabric(&self) -> &SessionFabric {
        &self.fabric
    }

    fn record(&self, kind: &'static str, payload: String) {
        self.timeline
            .lock()
            .unwrap_or_else(|p| p.into_inner())
            .record(kind, payload);
    }

    fn fabric_changed(&self) {
        crate::presence::post_fabric_changed(&self.self_id);
    }

    fn holder_token(&self) -> Option<String> {
        let now = crate::metrics::now_us();
        self.turn_lease
            .lock()
            .unwrap_or_else(|p| p.into_inner())
            .as_ref()
            .and_then(|l| l.driving_token(now))
    }
}

impl Registry for crate::session_store::SessionStore {
    fn sessions(&self) -> Vec<Arc<dyn FabricSession>> {
        let mut live: Vec<_> = self.live_handles().collect();
        live.sort_by_key(|h| h.local_id);
        live.into_iter()
            .map(|h| Arc::clone(&h.ctx) as Arc<dyn FabricSession>)
            .collect()
    }

    fn session(&self, sid: &SessionId) -> Option<Arc<dyn FabricSession>> {
        self.by_sid(sid)
            .map(|h| Arc::clone(&h.ctx) as Arc<dyn FabricSession>)
    }

    fn bridge_lost_advised(&self, sessions: usize) {
        // Only a unix instance supervises a bridge it can lose.
        #[cfg(any(unix, test))]
        crate::message_inbox::queue_message(crate::message_reporters::fabric_lost_advisory(
            sessions,
        ));
        #[cfg(not(any(unix, test)))]
        let _ = sessions;
    }
}

impl Timeline for crate::session_timeline::SessionTimeline {
    fn record(&mut self, kind: &'static str, payload: String) {
        crate::session_timeline::SessionTimeline::record(self, kind, payload);
    }
}

/// [`aterm_gui_fabric::with_link_reset`], resetting the launcher's supervisor
/// record with the link (`fabric_launch::arm` sets both under its own lock).
#[cfg(test)]
pub(crate) fn with_link_reset<T>(f: impl FnOnce() -> T) -> T {
    #[cfg(unix)]
    also_reset_with_the_link(crate::fabric_launch::reset_for_tests);
    aterm_gui_fabric::with_link_reset(f)
}

/// THE NEGATIVE CONTROL OF A FENCED WAIT, WITHOUT A CLOCK: a waiter
/// another thread started on `fabric` under a raised fence is PARKED — it
/// blocked on the fence ([`SessionFabric::fence_waits`] moved past `before`,
/// awaited under the hang detector) — and, woken while the fence still stands,
/// blocks AGAIN rather than answering. `finished` reads the waiter's thread.
///
/// A copy of aterm-gui-fabric's own: there it is `cfg(test)`, because the hang
/// detector it waits under (`aterm_spec::wait`) is only that crate's
/// dev-dependency, so it cannot be exported to these tests.
#[cfg(test)]
pub(crate) fn assert_parked_on_the_fence(
    fabric: &SessionFabric,
    before: u64,
    finished: impl Fn() -> bool,
    what: &str,
) {
    let parked = |n: u64| {
        aterm_spec::wait::within(
            aterm_spec::wait::MUST_HAPPEN,
            std::time::Duration::from_millis(1),
            || (fabric.fence_waits() > n).then_some(()),
        )
        .is_some()
    };
    assert!(
        parked(before),
        "{what}: the waiter never blocked on the fence"
    );
    assert!(!finished(), "{what}: blocked, so not answered");
    let blocked = fabric.fence_waits();
    fabric.wake_waiters_for_test();
    assert!(
        parked(blocked),
        "{what}: woken under the standing fence, it must block again"
    );
    assert!(!finished(), "{what}: still not answered");
}

#[cfg(test)]
#[path = "fabric_handoff_carry_tests.rs"]
mod handoff_carry;
