// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

use super::App;
use crate::session_store::SessionState;

#[test]
fn registered_session_starts_spawning_then_marks_alive_and_stays_addressable() {
    let app = App::headless_for_test();

    {
        let g = app.store.read().unwrap();
        assert_eq!(g.len(), 1, "session 0 is registered");
        let h = g.by_local(0).expect("session 0 addressable by local id");
        assert_eq!(h.state, SessionState::Spawning, "registers as Spawning");
    }

    assert!(
        app.store.write().unwrap().mark_alive(0),
        "first readiness performs the transition"
    );
    assert!(
        !app.store.write().unwrap().mark_alive(0),
        "a duplicate/late readiness is a fail-safe no-op"
    );

    {
        let g = app.store.read().unwrap();
        let h = g.by_local(0).expect("still addressable after transition");
        assert_eq!(h.state, SessionState::Alive);
        assert_eq!(g.len(), 1, "no handle churn across the transition");
    }
}
