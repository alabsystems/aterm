// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Fabric endpoint bindings ([`FabricSession`], [`Registry`], [`Timeline`]).

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

    fn record_fact(&self, kind: &'static str, fact: FabricFact<'_>) -> bool {
        self.timeline
            .lock()
            .unwrap_or_else(|p| p.into_inner())
            .record_fact(kind, fact)
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
        #[cfg(any(unix, test))]
        crate::message_inbox::queue_message(crate::message_reporters::fabric_lost_advisory(
            sessions,
        ));
        #[cfg(not(any(unix, test)))]
        let _ = sessions;
    }

    fn torn_state_set_aside(&self, files: u64, told_when_empty: bool) {
        #[cfg(any(unix, test))]
        crate::message_inbox::queue_exception(
            crate::message_reporters::torn_state_set_aside_entry(files, told_when_empty),
        );
        #[cfg(not(any(unix, test)))]
        let _ = (files, told_when_empty);
    }

    fn torn_state_held(&self, files: u64) {
        #[cfg(any(unix, test))]
        if files == 0 {
            crate::message_inbox::queue_exception_clear(crate::message_reporters::torn_state_key());
        }
        #[cfg(not(any(unix, test)))]
        let _ = files;
    }
}

impl Timeline for crate::session_timeline::SessionTimeline {
    /// THE ONE FABRIC HOOK ([`crate::session_timeline::SessionTimeline::slog_take_wake`]).
    fn record_fact(&mut self, kind: &'static str, fact: FabricFact<'_>) -> bool {
        let at = self.slog().now();
        self.slog_mut().observe_fabric(fact, at);
        crate::session_timeline::SessionTimeline::record(self, kind);
        self.slog_take_wake()
    }

    fn log_carried(&self) -> bool {
        self.slog().log_carried()
    }
}

/// [`aterm_gui_fabric::with_link_reset`].
#[cfg(test)]
pub(crate) fn with_link_reset<T>(f: impl FnOnce() -> T) -> T {
    #[cfg(unix)]
    also_reset_with_the_link(crate::fabric_launch::reset_for_tests);
    aterm_gui_fabric::with_link_reset(f)
}

/// THE NEGATIVE CONTROL OF A FENCED WAIT ([`SessionFabric::fence_waits`]).
#[cfg(all(test, unix))]
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
