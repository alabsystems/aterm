// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0

use super::*;

fn revoked(ids: &[&str]) -> Vec<String> {
    ids.iter().map(|id| (*id).to_string()).collect()
}

#[test]
fn a_revocation_takes_back_its_machines_part_and_nothing_else() {
    let none = FloorSources::default();
    let (v, s) = none.step(0, &[], 900, Some("m3"));
    let (v, s) = s.step(v, &[], 50, None);
    let (v, s) = s.step(v, &[], 9_999_999_999, Some("m11"));
    assert_eq!(v, 9_999_999_999);
    let (same, s) = s.step(v, &revoked(&["m20"]), 0, None);
    assert_eq!(same, 9_999_999_999);
    let (v, s) = s.step(same, &revoked(&["m11"]), 0, None);
    assert_eq!(v, 900);
    let (v, s) = s.step(v, &revoked(&["m11"]), 10_000_000_000, Some("m11"));
    assert_eq!(v, 900);
    let (v, s) = s.step(v, &revoked(&["m11"]), 1_000, Some("m3"));
    assert_eq!(v, 1_000);
    let (v, _) = s.step(v, &revoked(&["m11"]), 10, Some("m3"));
    assert_eq!(v, 1_000);
}

#[test]
fn an_unexplained_floor_is_unattributed_and_a_hand_edit_is_capped() {
    let mut attributed = FloorSources::default();
    attributed.machines.insert("m11".into(), 9_000);
    let (v, s) = attributed.step(9_500, &revoked(&["m11"]), 0, None);
    assert_eq!(v, 9_500);
    assert!(s.is_empty(), "written as before provenance: {s:?}");
    let (v, _) = attributed.step(100, &[], 0, None);
    assert_eq!(v, 100);
    let (v, _) = attributed.step(100, &revoked(&["m11"]), 0, None);
    assert_eq!(v, 0, "the capped part was m11's alone");
    let based = FloorSources {
        base: 9_000,
        ..FloorSources::default()
    };
    assert_eq!(based.step(100, &[], 0, None).0, 100);
    let (v, s) = FloorSources::default().step(100, &[], 0, Some("m3"));
    assert_eq!(v, 100);
    assert!(s.is_empty(), "{s:?}");
    let (v, _) = attributed.step(9_000, &revoked(&["m11"]), 0, None);
    assert_eq!(v, 0);
}
