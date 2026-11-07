// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

use super::*;
use std::os::unix::fs::PermissionsExt as _;

#[test]
fn retained_sdk_capture_and_validation_wait_for_unchanged_inputs() {
    let parent = trust_resource_provenance::process::account_home()
        .unwrap()
        .join(".cache/aterm/.ubd/sdk-hang-fixtures.noindex");
    std::fs::create_dir_all(&parent).unwrap();
    let scratch = aterm_tempfile::Builder::new()
        .prefix("sdk-")
        .tempdir_in(&parent)
        .unwrap();
    let layout = atpkg::store::Layout {
        prefix: scratch.path().join("manager"),
    };
    let build = layout.build_dir("trust", 1);
    std::fs::create_dir_all(build.join("bin")).unwrap();
    std::fs::create_dir_all(build.join("lib/rustlib/aarch64-apple-darwin/lib")).unwrap();
    let program = build.join("bin/targo");
    // This native image is read, never executed, so no assessment can outlive its fixture.
    std::fs::copy(std::env::current_exe().unwrap(), &program).unwrap();
    std::fs::set_permissions(&program, std::fs::Permissions::from_mode(0o555)).unwrap();
    atpkg::store::mark_build_ready(&build).unwrap();
    let keeper = Keeper::capture(&layout, &program).unwrap();
    let captured = Keeper::capture_at(&layout, &program, Instant::now() - Duration::from_secs(31));
    let validated = keeper.validate_at(Instant::now() - Duration::from_secs(3));
    assert!(
        captured.is_ok() && validated.is_ok(),
        "capture={:?}; validation={validated:?}",
        captured.as_ref().err()
    );
    assert!(
        Keeper::capture_at(&layout, &program, Instant::now() - Duration::from_secs(60)).is_err()
    );
    assert!(
        keeper
            .validate_at(Instant::now() - Duration::from_secs(60))
            .is_err()
    );
    keeper.lease.check_current().unwrap();
    std::fs::rename(&program, program.with_extension("original")).unwrap();
    std::fs::copy(program.with_extension("original"), &program).unwrap();
    assert!(
        keeper.validate().is_err(),
        "equal bytes at another inode cannot replace the original held image"
    );
}
