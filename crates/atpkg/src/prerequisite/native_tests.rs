// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Real signed-fixture admission/native capture only. The structural Mach-O
//! fixture is NEVER executed or called a real CPython/compiler/SDK result.
use super::*;
use std::time::{Duration, Instant};
fn fixture() -> Fixture {
    fixture_for(Prerequisite::Python)
}
fn fixture_for(want: Prerequisite) -> Fixture {
    let dyld = b"/usr/lib/dyld\0";
    let size = (12 + dyld.len()).next_multiple_of(8);
    let mut image = vec![0; 32 + size];
    for (at, value) in [
        (0, 0xfeed_facfu32),
        (4, 0x0100_000c),
        (12, 2),
        (16, 1),
        (20, size as u32),
        (32, 0xe),
        (36, size as u32),
        (40, 12),
    ] {
        image[at..at + 4].copy_from_slice(&value.to_le_bytes());
    }
    image[44..44 + dyld.len()].copy_from_slice(dyld);
    signed_fixture_for(want, &image)
}
fn signed_fixture(image: &[u8]) -> Fixture {
    signed_fixture_for(Prerequisite::Python, image)
}
fn signed_fixture_for(want: Prerequisite, image: &[u8]) -> Fixture {
    let parent = std::env::current_exe()
        .unwrap()
        .parent()
        .unwrap()
        .to_owned();
    let mut fixture =
        Fixture::with_layout(want, crate::testdir::layout_in(&parent, "signed-native-"));
    let before = crate::tree::tree_root(&fixture.root).unwrap();
    std::fs::write(fixture.executable(), image).unwrap();
    let after = crate::tree::tree_root(&fixture.root).unwrap();
    fixture.change_package(&before, &after);
    crate::store::mark_build_ready(&fixture.root).unwrap();
    fixture
}

#[path = "native_exhaustion_tests.rs"]
mod exhaustion;
fn acquire(fixture: &Fixture) -> HeldTool {
    fixture
        .session(TRIPLE)
        .unwrap()
        .acquire(Prerequisite::Python)
        .unwrap()
}
fn kept(fixture: &Fixture) -> bool {
    matches!(
        crate::lease::reclaim(
            &fixture.layout.prefix,
            &Subject::build("cpython", 7).unwrap()
        ),
        crate::lease::Reclaim::Keep(_)
    )
}
#[test]
fn same_signed_owner_and_native_pins_keep_gc_blocked_until_last_actual_lease_copy_closes() {
    let fixture = fixture();
    let native = acquire(&fixture)
        .into_native(Instant::now() + Duration::from_secs(60))
        .unwrap();
    assert_eq!(native.original().unwrap().root(), fixture.root);
    assert_eq!(native.program().unwrap().program(), fixture.executable());
    let copy = native.lease_descriptor().unwrap();
    drop(native);
    assert!(kept(&fixture));
    drop(copy);
    assert!(!kept(&fixture));
}
#[test]
fn failed_native_capture_returns_the_same_actual_signed_lease() {
    let fixture = signed_fixture(b"signed ordinary bytes, not Mach-O");
    let refused = acquire(&fixture)
        .into_native(Instant::now() + Duration::from_secs(60))
        .err()
        .expect("unsupported image must refuse");
    assert!(kept(&fixture));
    let (_, original) = refused.into_parts();
    assert_eq!(original.root(), fixture.root);
    original.validate().unwrap();
    let original_end = original.native_attempt;
    let repeated = (*original)
        .into_native(Instant::now() + Duration::from_secs(120))
        .err()
        .expect("the same refused signed owner cannot renew native capture");
    assert_eq!(
        repeated.error().kind(),
        std::io::ErrorKind::PermissionDenied
    );
    let (_, original) = repeated.into_parts();
    assert_eq!(original.native_attempt, original_end);
    drop(original);
    assert!(!kept(&fixture));
}
#[test]
fn native_refusal_is_sticky_after_observed_image_replacement_and_restore() {
    let fixture = fixture();
    let native = acquire(&fixture)
        .into_native(Instant::now() + Duration::from_secs(60))
        .unwrap();
    let copy = native.lease_descriptor().unwrap();
    let path = fixture.executable();
    let old = fixture.root.join("original-image");
    std::fs::rename(&path, &old).unwrap();
    std::fs::write(&path, b"replacement").unwrap();
    assert!(native.check_current().is_err());
    std::fs::remove_file(&path).unwrap();
    std::fs::rename(&old, &path).unwrap();
    assert!(
        native.original().is_err()
            && native.program().is_err()
            && native.lease_descriptor().is_err()
    );
    drop(native);
    assert!(kept(&fixture));
    drop(copy);
    assert!(!kept(&fixture));
}
#[test]
fn expired_original_endpoint_cannot_start_native_capture_or_release_its_original_lease() {
    let fixture = fixture();
    let refused = acquire(&fixture)
        .into_native(Instant::now())
        .err()
        .expect("expired original endpoint refuses");
    assert_eq!(refused.error().kind(), std::io::ErrorKind::TimedOut);
    assert!(kept(&fixture));
    drop(refused);
    assert!(!kept(&fixture));
}
#[test]
fn foreign_signed_guard_cannot_become_a_new_native_owner_or_unlock_offered_custody() {
    let fixture = fixture();
    let mut tool = acquire(&fixture);
    let copy = tool.lease_descriptor().unwrap();
    tool.creator = 0;
    let refused = tool
        .into_native(Instant::now() + Duration::from_secs(60))
        .err()
        .expect("foreign Rust copy is not native ownership");
    assert_eq!(refused.error().kind(), std::io::ErrorKind::PermissionDenied);
    drop(refused);
    assert!(kept(&fixture));
    drop(copy);
    assert!(!kept(&fixture));
}
