// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

use super::{Scope, cmd_grant};
use crate::session_store::{SessionHandoff, SessionStore, test_handle};

#[test]
fn the_carry_holds_each_triple_once() {
    let controller = test_handle(1);
    let target = test_handle(2);
    let (src, dst) = (controller.sid.clone(), target.sid.clone());
    for _ in 0..1_000 {
        assert!(
            cmd_grant(&target.ctx, Scope::Owner, &format!("{src} read-screen")).starts_with("OK ")
        );
        assert!(
            cmd_grant(&target.ctx, Scope::Owner, "s-controller read-screen").starts_with("OK ")
        );
    }
    for op in ["signal", "write-input"] {
        assert!(cmd_grant(&target.ctx, Scope::Owner, &format!("{src} {op}")).starts_with("OK "));
    }
    assert!(cmd_grant(&target.ctx, Scope::Owner, &format!("{dst} signal")).starts_with("OK "));
    let mut store = SessionStore::default();
    store.register(controller);
    store.register(target);

    let carried = SessionHandoff::from_store(&store).connections;
    let triples = carried
        .iter()
        .map(|row| (row.src.as_str(), row.dst.as_str(), row.op.as_str()))
        .collect::<Vec<_>>();
    let mut expected = vec![
        (src.as_str(), dst.as_str(), "read-screen"),
        (src.as_str(), dst.as_str(), "signal"),
        (src.as_str(), dst.as_str(), "write-input"),
        ("s-controller", dst.as_str(), "read-screen"),
        (dst.as_str(), dst.as_str(), "signal"),
    ];
    expected.sort_unstable();
    assert_eq!(
        triples, expected,
        "each distinct triple, once, in `(src, dst, op)` order"
    );
}
