// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Control requests cannot advance state using a faulted or revoked owner.
use super::*;
use std::fs;
use trust_resource_controller::workload::host::{bank, census, phase};

fn calls(owner: &mut Owner) -> [io::Error; 2] {
    let peer = Peer {
        pid: std::process::id(),
        birth: 1,
    };
    [
        owner
            .workload_phase(
                peer,
                "s-acde",
                &phase::Request {
                    key: bank::Key {
                        root_attempt: [1; 16],
                        invocation_sha256: [2; 32],
                        sequence: 1,
                    },
                    operation: phase::Operation::Drained,
                },
                Vec::new(),
            )
            .unwrap_err(),
        owner
            .workload_census(
                peer,
                "s-acde",
                &census::Request {
                    root_attempt: [1; 16],
                    invocation_sha256: [2; 32],
                    snapshot: [3; 16],
                    offset: 0,
                },
            )
            .unwrap_err(),
    ]
}

#[test]
fn faulted_workload_owner_refuses_before_root_state_access() {
    let mut world = World::new();
    for error in calls(&mut world.owner) {
        assert!(error.to_string().contains("root unavailable"));
    }
    world.owner.faulted = true;
    for error in calls(&mut world.owner) {
        assert_eq!(error.kind(), io::ErrorKind::WouldBlock);
        assert!(error.to_string().contains("custody is faulted"));
    }
    assert!(world.owner.idle());
    assert_eq!(world.owner.debt.reserved().unwrap(), 0);
}

#[test]
fn revoked_workload_owner_refuses_even_without_fault_flag() {
    let mut world = World::new();
    let epoch = world.namespace.backend().join(".host-owner.epoch");
    let original = fs::read(&epoch).unwrap();
    fs::write(&epoch, b"revoked original leadership").unwrap();
    for error in calls(&mut world.owner) {
        assert_eq!(error.kind(), io::ErrorKind::InvalidData);
        assert!(!error.to_string().contains("root unavailable"));
    }
    // Restore only fixture storage for cleanup. The original fence stays revoked.
    fs::write(&epoch, original).unwrap();
    assert!(!world.owner.faulted);
    assert!(world.owner.fence.check_current().is_err());
    assert!(world.owner.idle());
    assert_eq!(world.owner.debt.reserved().unwrap(), 0);
}
