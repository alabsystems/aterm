// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

use super::*;

#[test]
fn only_live_connection_bound_claims_spare_their_connection() {
    let store = crate::session_store::new_store();
    let supervisor = crate::session_store::test_handle(1);
    let driver = crate::session_store::test_handle(2);
    store.write().unwrap().register(supervisor.clone());
    store.write().unwrap().register(driver.clone());
    let claimed = |conn, now| connection_has_live_claim(&store, conn, now);
    assert!(!claimed(7, 100));
    assert_eq!(
        crate::session_timeline::claim_supervisor(&supervisor.ctx, "sup", Some(7), None, 100),
        Ok(true)
    );
    assert!(claimed(7, 100));
    assert!(!claimed(8, 100), "another connection owns no claim");
    assert!(crate::session_timeline::release_supervisor(
        &supervisor.ctx,
        Some(7),
        100,
        aterm_slog::Actor::Ctl
    ));
    assert!(!claimed(7, 100), "release makes the connection evictable");
    assert_eq!(
        crate::session_timeline::claim_supervisor(&supervisor.ctx, "sup", None, Some(500), 100),
        Ok(true)
    );
    assert!(
        supervisor
            .ctx
            .meta
            .lock()
            .unwrap()
            .live_claim(499)
            .is_some()
    );
    assert!(
        !claimed(7, 499),
        "a detached live TTL outlives the connection"
    );
    assert!(
        supervisor
            .ctx
            .meta
            .lock()
            .unwrap()
            .live_claim(500)
            .is_none()
    );
    assert!(
        !claimed(7, 500),
        "expired supervisor authority spares no connection"
    );

    struct Restore(Option<u64>, bool);
    impl Drop for Restore {
        fn drop(&mut self) {
            SERVING_CONNECTION.with(|s| s.set(self.0));
            CONNECTION_CLAIMED.with(|c| c.set(self.1));
        }
    }
    let _restore = Restore(
        SERVING_CONNECTION.with(|s| s.replace(Some(7))),
        CONNECTION_CLAIMED.with(|c| c.replace(false)),
    );
    let lease = |rest| control_session::cmd_lease(&driver.ctx, rest);
    assert!(lease("acquire holder=drive ttl=60000 hard here").starts_with("OK lease acquired"));
    let expiry = match driver.ctx.turn_lease.lock().unwrap().as_ref().unwrap() {
        crate::Lease::Drive { expires_us, .. } => *expires_us,
        other => panic!("expected the acquired drive lease, got {other:?}"),
    };
    assert!(claimed(7, expiry - 1));
    assert!(
        !claimed(8, expiry - 1),
        "another owner does not protect this connection"
    );
    assert!(
        !claimed(7, expiry),
        "the drive lease's expiry is authoritative"
    );
    assert_eq!(lease("release holder=drive here"), "OK lease released\n");
    assert!(!claimed(7, crate::metrics::now_us()));
    assert!(
        CONNECTION_CLAIMED.with(std::cell::Cell::get),
        "Drop still owes cleanup"
    );
    assert!(lease("acquire holder=drive ttl=60000 hard here").starts_with("OK lease acquired"));
    SERVING_CONNECTION.with(|s| s.set(Some(8)));
    assert!(lease("acquire holder=drive ttl=60000 hard").starts_with("OK lease acquired"));
    assert!(!claimed(7, crate::metrics::now_us()));
    assert!(
        !claimed(8, crate::metrics::now_us()),
        "a detached renewal outlives either connection"
    );
}
