// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! The cutter seals the successor's handoff policy into the bundle.
//!
//! An older aterm trusts `Contents/Resources/aterm-handoff-policy.toml` in the release it hands
//! its sessions to only because the code signature seals it, which holds only if the cutter
//! writes it before signing. This runs the real bundle and signing steps in the cut's order and
//! proves the file is there byte for byte and sealed.

use crate::bundle;
#[cfg(target_os = "macos")]
use crate::sign;

#[cfg(target_os = "macos")]
use std::path::{Path, PathBuf};

#[cfg(target_os = "macos")]
fn repo_root() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .and_then(Path::parent)
        .expect("crates/aterm-release sits two levels under the root")
        .to_path_buf()
}

/// `codesign --verify --deep --strict`, the structural check the updater runs
/// in its default tier: `Ok` when the seal is intact.
#[cfg(target_os = "macos")]
fn verify_seal(app: &Path) -> Result<(), String> {
    // Under a hang detector: a codesign that stops answering fails in a minute.
    let out = aterm_uds::deadline::output_until(
        aterm_uds::spawnfd::command("/usr/bin/codesign")
            .args(["--verify", "--deep", "--strict"])
            .arg(app),
        std::time::Instant::now() + aterm_uds::deadline::CODESIGN_HANG,
    )
    .map_err(|e| format!("run codesign: {e}"))?;
    if out.status.success() {
        Ok(())
    } else {
        Err(String::from_utf8_lossy(&out.stderr).trim().to_string())
    }
}

#[cfg(target_os = "macos")]
#[test]
fn the_cutter_seals_the_handoff_policy_into_the_bundle_before_it_signs() {
    use aterm_update_core::handoff_policy::{
        BUNDLE_PATH, PolicyRead, SOURCE_PATH, read_from_bundle,
    };

    // Signing and verifying a copy of an Apple binary wait on trustd.
    aterm_uds::deadline::require_trust_service();
    let repo = repo_root();
    let scratch = std::env::temp_dir().join(format!(
        "aterm-release-policy-bundle-{}",
        std::process::id()
    ));
    let _ = std::fs::remove_dir_all(&scratch);
    std::fs::create_dir_all(&scratch).expect("scratch");
    // Any signable Mach-O proves the same about the seal as the cut's universal `aterm`.
    let exe = scratch.join("aterm-bin");
    std::fs::copy("/usr/bin/true", &exe).expect("a Mach-O to bundle");
    let spec = bundle::BundleSpec {
        repo_root: repo.clone(),
        out_dir: scratch.join("dist"),
        short_version: "0.0.0".to_string(),
        build_number: 1_790_000_000,
        bundle_id: "com.aterm.policy-test".to_string(),
        git_commit: "0123456789ab".to_string(),
        aterm_bin: exe,
    };

    // The release's order: the build lays the bundle out, the cut signs it.
    let app = scratch.join("aterm.app");
    bundle::assemble_at(&spec, &app).expect("the bundle step");
    let placed = app.join(BUNDLE_PATH);
    let source = std::fs::read(repo.join(SOURCE_PATH)).expect("the checked-in policy");
    assert_eq!(
        std::fs::read(&placed).expect("the bundle carries the policy"),
        source,
        "the policy is placed in Contents/Resources byte for byte"
    );
    sign::sign_app(&app, &repo.join("apps/aterm-mac/aterm.entitlements"), None)
        .expect("the ad-hoc signing step");
    verify_seal(&app).expect("the signed bundle verifies");
    assert!(
        matches!(read_from_bundle(&app), PolicyRead::Parsed(_)),
        "a producer reads the sealed policy as schema 1"
    );

    // Sealed: a policy edited after signing breaks the bundle's signature.
    let mut tampered = source.clone();
    tampered.extend_from_slice(b"carry = \"repaint\"\n");
    std::fs::write(&placed, &tampered).expect("tamper");
    let broken = verify_seal(&app).expect_err("a changed policy breaks the seal");
    assert!(
        broken.contains("sealed resource"),
        "codesign names the sealed resource: {broken}"
    );
    let _ = std::fs::remove_dir_all(&scratch);
}

/// The bundle step re-judges the policy bytes it places, so a file edited after the gate cannot
/// slip into the bundle.
#[test]
fn the_bundle_step_refuses_a_policy_no_producer_could_follow() {
    let scratch = std::env::temp_dir().join(format!(
        "aterm-release-policy-refused-{}",
        std::process::id()
    ));
    let _ = std::fs::remove_dir_all(&scratch);
    std::fs::create_dir_all(scratch.join("publish")).expect("scratch repo");
    std::fs::create_dir_all(scratch.join("aterm.app/Contents/Resources")).expect("scratch app");
    std::fs::write(
        scratch.join(aterm_update_core::handoff_policy::SOURCE_PATH),
        "schema = 1\ncary = \"repaint\"\n",
    )
    .expect("write");
    bundle::place_handoff_policy(&scratch, &scratch.join("aterm.app"))
        .expect_err("a typo'd key refuses");
    assert!(
        !scratch
            .join("aterm.app")
            .join(aterm_update_core::handoff_policy::BUNDLE_PATH)
            .exists(),
        "nothing is written into the bundle"
    );
    let _ = std::fs::remove_dir_all(&scratch);
}
