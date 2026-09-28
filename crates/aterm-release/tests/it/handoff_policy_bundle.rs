// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! The cutter seals the successor's handoff policy into the bundle (the
//! 2026-09-22/23 update audit, plan P0-5).
//!
//! An older aterm reads `Contents/Resources/aterm-handoff-policy.toml` from the
//! release it is handing its sessions to, and trusts it for one reason only: the
//! release's code signature seals it with the code that is about to run as the
//! successor. That holds only if the cutter writes the file BEFORE it signs. This
//! drives the cut's real bundle step and its real signing step, in the order the
//! cut runs them, and proves the file is there, byte for byte, and sealed — a
//! change to it after signing breaks the bundle's signature.

use crate::{bundle, sign};

use std::path::{Path, PathBuf};

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
    let out = std::process::Command::new("/usr/bin/codesign")
        .args(["--verify", "--deep", "--strict"])
        .arg(app)
        .output()
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

    let repo = repo_root();
    let scratch = std::env::temp_dir().join(format!(
        "aterm-release-policy-bundle-{}",
        std::process::id()
    ));
    let _ = std::fs::remove_dir_all(&scratch);
    std::fs::create_dir_all(&scratch).expect("scratch");
    // A real Mach-O for the bundle's executable — the cut's is the universal
    // `aterm`; any signable binary proves the same thing about the seal.
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

    // THE CUT'S ORDER: assemble, then sign.
    let app = bundle::assemble(&spec).expect("the bundle step");
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

    // SEALED: a policy edited after signing is a bundle whose signature fails,
    // so no producer's pre-verification would pass it, and none would read it.
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

/// The bundle step refuses a policy the gate would have refused — the gate runs
/// before the claim, and this is the same judgement over the same bytes, so a
/// file edited between the two cannot slip into the bundle.
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
    let refused = bundle::place_handoff_policy(&scratch, &scratch.join("aterm.app"))
        .expect_err("a typo'd key refuses");
    assert!(refused.contains("unknown key `cary`"), "{refused}");
    assert!(
        !scratch
            .join("aterm.app")
            .join(aterm_update_core::handoff_policy::BUNDLE_PATH)
            .exists(),
        "nothing is written into the bundle"
    );
    let _ = std::fs::remove_dir_all(&scratch);
}
