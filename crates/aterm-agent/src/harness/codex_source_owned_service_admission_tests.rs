// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0

//! Native principal fixtures, not production SDK or foreground qualifications.
use super::*;
use crate::harness::account_work::Lifetime;

fn peer() -> Peer {
    let process = Process::observe(std::process::id()).unwrap();
    Peer {
        caller: slot::Caller {
            sid: "s-retained-control-fixture".into(),
            pid: process.pid,
            birth: process.birth,
        },
        process,
        launch: None,
    }
}

#[test]
fn frontend_requires_independent_control_namespace_and_actual_app_scope() {
    let lifetime = Lifetime::default();
    let scope = lifetime.enter().unwrap();
    let registry = Registry::for_app(&lifetime);
    let connection = registry.fixture_connection();
    let foreign = registry.fixture_connection();
    let peer = peer();
    let operation = "a".repeat(32);
    let member = registry
        .open_frontend_controller(&peer, &connection, &operation, &scope)
        .unwrap()
        .member;
    assert!(
        registry
            .frontend_controller(&peer, &foreign, &member, &operation)
            .is_err()
    );
    let controller = registry
        .frontend_controller(&peer, &connection, &member, &operation)
        .unwrap();
    assert!(!controller.credential_construction_started().unwrap());
    let admitted = registry.service_frontend(&controller, &scope).unwrap();
    let identity = admitted.identity();
    admitted.current().unwrap();
    assert!(admitted.same_requester(&registry.service_frontend(&controller, &scope).unwrap()));
    assert!(
        identity.same(
            &registry
                .service_frontend(&controller, &scope)
                .unwrap()
                .identity()
        )
    );
    let other_namespace = Registry::for_app(&lifetime);
    assert!(matches!(
        other_namespace.service_frontend(&controller, &scope),
        Err(Error::Denied)
    ));
    let other_lifetime = Lifetime::default();
    let other_scope = other_lifetime.enter().unwrap();
    assert!(matches!(
        registry.service_frontend(&controller, &other_scope),
        Err(Error::Denied)
    ));
    let source = registry
        .open_member(&peer, &connection, &"b".repeat(32))
        .unwrap()
        .member;
    assert!(controller.credential_construction_started().unwrap());
    assert!(
        registry
            .frontend_controller(&peer, &connection, &source, &operation)
            .is_err(),
        "a Source member is not an independent controller"
    );
    {
        let _contended = registry.state.lock().unwrap();
        assert_eq!(controller.check_current(), Err(Error::Busy));
        assert_eq!(admitted.current(), Err(Error::Busy));
        assert!(
            !admitted.retired(),
            "contention must not erase replay custody"
        );
    }
    controller.check_current().unwrap();
    admitted.current().unwrap();
    let other_operation = "c".repeat(32);
    let other_member = registry
        .open_frontend_controller(&peer, &foreign, &other_operation, &scope)
        .unwrap()
        .member;
    let other = registry
        .frontend_controller(&peer, &foreign, &other_member, &other_operation)
        .unwrap();
    assert!(!admitted.same_requester(&registry.service_frontend(&other, &scope).unwrap()));
    assert!(
        !identity.same(
            &registry
                .service_frontend(&other, &scope)
                .unwrap()
                .identity()
        )
    );
    scope.cancellation().cancel();
    assert!(
        !admitted.retired(),
        "cancelled requests cannot erase live-connection replay custody"
    );
    assert_eq!(admitted.current(), Err(Error::Stale));
    assert!(connection.is_live());
    drop(admitted);
    drop(scope);
    assert!(
        lifetime.pause().unwrap().try_wait().is_ok(),
        "comparison identity must not keep the original request operation alive"
    );
}

#[test]
fn original_control_eof_invalidates_a_retained_admission() {
    let lifetime = Lifetime::default();
    let scope = lifetime.enter().unwrap();
    let registry = Registry::for_app(&lifetime);
    let connection = registry.fixture_connection();
    let peer = peer();
    let operation = "d".repeat(32);
    let member = registry
        .open_frontend_controller(&peer, &connection, &operation, &scope)
        .unwrap()
        .member;
    let controller = registry
        .frontend_controller(&peer, &connection, &member, &operation)
        .unwrap();
    let admitted = registry.service_frontend(&controller, &scope).unwrap();
    admitted.current().unwrap();
    drop(connection);
    assert_eq!(
        controller.credential_construction_started(),
        Err(Error::Stale)
    );
    assert!(admitted.retired());
    assert_eq!(admitted.current(), Err(Error::Stale));
}
