// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! An isolated cached host path for M-HIST; no App, renderer, control socket or sampler.

use std::sync::OnceLock;

use super::*;

fn fixed_instant() -> Instant {
    static ORIGINAL: OnceLock<Instant> = OnceLock::new();
    *ORIGINAL.get_or_init(Instant::now)
}

fn fixed_continuous() -> Option<u64> {
    Some(1_000_000_000)
}

/// One original cached observation and the real Store/session admitted to read it.
/// This development-only probe fixes the observation-age clocks; the source snapshot and
/// its history timestamps remain unchanged through every measured pull.
pub struct HistoryPullProbe {
    shared: Shared,
    store: Store,
    session: crate::Session,
}

impl HistoryPullProbe {
    /// Register a headless session and publish the filled source once, outside measurement.
    #[must_use]
    pub fn new(mut snapshot: Arc<Snapshot>) -> Self {
        // History must read the latest machine publication without requiring a process census.
        Arc::make_mut(&mut snapshot).machine_only = true;
        let store = crate::session_store::new_store();
        let session = crate::stub_session_named(
            1,
            aterm_session::SessionId::generate(),
            Arc::new(crate::SinkWriter::new(-1)),
        );
        let handle = crate::session_store::SessionHandle::of_session(&session, None);
        {
            let mut registered = store.write().unwrap_or_else(|p| p.into_inner());
            drop(registered.register(handle));
        }
        let mut shared = Shared::with_clock(fixed_continuous);
        shared.instant_clock = fixed_instant;
        shared.set_facts(vec![PaneFact {
            key: session.id,
            window: 1,
            tab: 1,
            tab_id: None,
        }]);
        shared.publish(
            snapshot,
            &Machine::default(),
            ObservedAt {
                instant: fixed_instant(),
                continuous_ns: fixed_continuous(),
            },
            None,
            None,
            false,
            None,
        );
        Self {
            shared,
            store,
            session,
        }
    }

    /// Check the normal-library constructor, outside measurement. The native owner must remain
    /// absent; the same registration decision is exercised with an injected effect instead of
    /// starting a real mailbox worker. Unlike a unit test, this runs with `cfg(not(test))`.
    pub fn verify_isolation(&self) {
        #[cfg(unix)]
        assert!(
            !crate::fabric::direct_mail_watch_started(),
            "the cached host probe must not start a native mailbox owner"
        );
        let handle = crate::session_store::SessionHandle::of_session(&self.session, None);
        let store = crate::session_store::new_store();
        let mut started = 0;
        let mut registered = store.write().unwrap_or_else(|p| p.into_inner());
        registered
            .register(handle.clone())
            .start_mailbox_with(|ctx| {
                assert!(Arc::ptr_eq(ctx, &self.session.ctx));
                assert!(
                    store.try_write().is_err(),
                    "mailbox enqueue retains Store custody"
                );
                started += 1;
            });
        assert_eq!(
            started, 1,
            "a live first registration owes its mailbox hook"
        );
        registered
            .register(handle)
            .start_mailbox_with(|_| started += 1);
        assert_eq!(
            started, 1,
            "replacement does not start another mailbox hook"
        );
        drop(registered);
        assert!(store.try_write().is_ok());
        let registered = self.store.read().unwrap_or_else(|p| p.into_inner());
        let admitted = registered.by_local(self.session.id).unwrap();
        assert_eq!(admitted.sid, self.session.ctx.self_id);
        assert_eq!(admitted.nonce, self.session.ctx.nonce);
        assert!(Arc::ptr_eq(&admitted.term, &self.session.term));
        #[cfg(unix)]
        assert!(!crate::fabric::direct_mail_watch_started());
    }

    /// The actual host handler: parsing, freshness/cache reads, live Store identity proof,
    /// gathering, reply bookkeeping and complete text or JSON serialization.
    #[must_use]
    pub fn pull(&self, request: &str) -> String {
        cmd_sys_on(
            &self.shared,
            &self.store,
            AuthorizedTarget {
                session: self.session.id,
                term: &self.session.term,
                master: self.session.master,
                ctx: &self.session.ctx,
            },
            true,
            request,
        )
    }
}
