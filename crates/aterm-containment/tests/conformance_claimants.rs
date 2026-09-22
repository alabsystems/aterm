// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! TIER-1: the SHIPPING claimant census, driven through
//! `TccIdentityClaimExclusivity`.
//!
//! Tier-0 (`crates/aterm-spec/tests/conformance_tcc_identity.rs`) proves the
//! machine over its bounded space. That proves a property of the DESCRIPTION.
//! This file makes it a property of the program: it stages bundles on a real
//! disk, runs the real `classify_claimants` over them, projects what the census
//! actually decided onto the model's variables, and steps the model with it —
//! so a census that stopped naming a conflict would fail the invariant rather
//! than quietly passing its own unit tests.
//!
//! **Why this class needs the bind.** It already recurred once. `tools/dev-app.sh`
//! moved dev builds to their own bundle id on 2026-08-31, which stopped NEW
//! co-claimants being minted — and nothing detected the two already sitting in
//! `~/aterm/dist` from July, which destroyed two Full Disk Access grants on
//! 2026-09-21. A fix that closes the source without closing the detection is
//! exactly what a Tier-0-only model cannot tell you about.
//!
//! **The negative control is the first step**, not an afterthought: a clean
//! disk must leave `reported` at 0. Without it, a census hard-coded to shout
//! "conflict" would satisfy every other assertion here.

#![cfg(target_os = "macos")]

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use aterm_containment::consent::{BundleIdentity, Claimants, classify_claimants};
use aterm_spec::derive::tcc_identity_claim_exclusivity_model;

/// The identifier under test. A literal is correct HERE and nowhere in the
/// shipping half — `consent.rs` carries a test asserting exactly that, because
/// a shipping path that knows its own id could reset rows that are not its own.
const ID: &str = "com.test.aterm-conformance";

/// One scratch tree, removed on drop even when an assertion unwinds.
struct Scratch(PathBuf);

impl Drop for Scratch {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

impl Scratch {
    /// `label` is load-bearing: the tests in this file run CONCURRENTLY in one
    /// process, so a directory named only after the pid is the same directory
    /// for both — and each would then census the other's fixtures. (It did:
    /// the first run reported four claimants where two were staged.)
    fn new(label: &str) -> Self {
        let dir =
            std::env::temp_dir().join(format!("aterm-claimants-{label}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).expect("scratch");
        Self(dir)
    }
}

/// Lay a real bundle: a `Contents/MacOS` layout and an `Info.plist` carrying
/// `bundle_id`. The census reads these with the same reader the product uses.
fn lay_bundle(root: &Path, bundle_id: &str) {
    std::fs::create_dir_all(root.join("Contents/MacOS")).expect("layout");
    std::fs::write(
        root.join("Contents/Info.plist"),
        format!(
            "<?xml version=\"1.0\" encoding=\"UTF-8\"?>\n<plist version=\"1.0\"><dict>\n\
             <key>CFBundleIdentifier</key><string>{bundle_id}</string>\n\
             <key>CFBundleExecutable</key><string>aterm</string>\n\
             </dict></plist>\n"
        ),
    )
    .expect("plist");
    std::fs::write(root.join("Contents/MacOS/aterm"), b"#!/bin/sh\nexit 0\n").expect("exe");
}

/// Run the REAL census over one directory.
///
/// The identity reader is injected rather than shelling out to `codesign`: the
/// requirement STRING is what this model is about, and a test that had to sign
/// fixtures would be a test about the signing toolchain. `classify_claimants`
/// — the function under test — is unchanged either way; only its oracle is.
fn census(dir: &Path, running: Option<&Path>, dr_for: &dyn Fn(&Path) -> &'static str) -> Claimants {
    classify_claimants(
        ID,
        running,
        &[dir.to_path_buf()],
        |root| {
            let read = std::fs::read_dir(root).ok()?;
            Some(read.flatten().map(|e| e.path()).collect())
        },
        |root| {
            let plist = std::fs::read_to_string(root.join("Contents/Info.plist")).ok()?;
            let id = plist
                .split("<key>CFBundleIdentifier</key><string>")
                .nth(1)?
                .split("</string>")
                .next()?
                .to_string();
            Some(BundleIdentity {
                bundle_id: Some(id),
                dr_text: dr_for(root).to_string(),
                signing: "adhoc",
                team: None,
            })
        },
    )
}

/// Step the model with what the real census decided, failing on the model's own
/// terms rather than on a restatement of them.
fn fire(model: &aterm_spec::derive::Model, state: &mut BTreeMap<&'static str, i64>, action: &str) {
    assert!(
        model.fire(action, state),
        "{action} is not enabled in {state:?} — the real census reached a state the machine \
         says is unreachable"
    );
    for name in [
        "AConflictIsNeverSilent",
        "NothingIsRetiredUnnamed",
        "GrantLossImpliesMismatch",
    ] {
        assert!(
            model.check_invariant(name, state),
            "{name} violated after {action}: {state:?}"
        );
    }
}

#[test]
fn the_real_census_never_leaves_a_conflict_silent() {
    let scratch = Scratch::new("silent");
    let dir = &scratch.0;
    let model = tcc_identity_claim_exclusivity_model();
    let mut state = model.init_state();

    const LIVE_DR: &str = "designated => identifier \"x\" and anchor apple generic";
    const FOSSIL_A: &str = "designated => cdhash H\"4249c4f5\"";
    const FOSSIL_B: &str = "designated => cdhash H\"20d41c8d\"";
    let dr_for = |root: &Path| -> &'static str {
        match root.file_name().and_then(|n| n.to_str()) {
            Some("aterm.app") => LIVE_DR,
            Some("aterm.app.rollback") => FOSSIL_A,
            _ => FOSSIL_B,
        }
    };

    // The live install, alone.
    let live = dir.join("aterm.app");
    lay_bundle(&live, ID);
    let running = live.join("Contents/MacOS/aterm");

    // STEP 1 — THE NEGATIVE CONTROL. A clean disk must report nothing. Without
    // this, a census that always cried conflict would pass everything below.
    let clean = census(dir, Some(&running), &dr_for);
    assert_eq!(clean.found.len(), 1, "only the live install: {clean:?}");
    assert!(clean.conflicting().is_empty());
    assert!(clean.sole_claimant(), "a complete look at a clean disk");
    fire(&model, &mut state, "CensusClean");
    assert_eq!(state["reported"], 0, "nothing to name");

    // The owner grants.
    fire(&model, &mut state, "Grant");

    // STEP 2 — the shape that happened: a `.rollback` sibling with a DIFFERENT
    // requirement appears beside the install.
    lay_bundle(&dir.join("aterm.app.rollback"), ID);
    fire(&model, &mut state, "StageForeign");
    let seen = census(dir, Some(&running), &dr_for);
    assert_eq!(seen.found.len(), 2, "{seen:?}");
    assert_eq!(
        seen.conflicting().len(),
        1,
        "the rollback's requirement differs from the live install's"
    );
    assert!(!seen.sole_claimant());
    // The REAL census named it, so the model's reporting step is licensed.
    fire(&model, &mut state, "Census");
    assert_eq!(state["reported"], 1);

    // STEP 3 — a second fossil, a backup this time. Staging invalidates the
    // previous look: a census is about a disk, not about having once looked.
    lay_bundle(&dir.join("aterm-b826-backup.app"), ID);
    fire(&model, &mut state, "StageForeign");
    assert_eq!(state["reported"], 0, "the earlier look is stale");
    let seen = census(dir, Some(&running), &dr_for);
    assert_eq!(seen.conflicting().len(), 2, "{seen:?}");
    fire(&model, &mut state, "Census");

    // STEP 4 — the owner retires one. Only after it was named, which is the
    // fence `NothingIsRetiredUnnamed` states.
    std::fs::remove_dir_all(dir.join("aterm.app.rollback")).expect("retire");
    fire(&model, &mut state, "Retire");
    let seen = census(dir, Some(&running), &dr_for);
    assert_eq!(seen.conflicting().len(), 1, "one fossil left: {seen:?}");
    fire(&model, &mut state, "Census");

    std::fs::remove_dir_all(dir.join("aterm-b826-backup.app")).expect("retire");
    fire(&model, &mut state, "Retire");
    let seen = census(dir, Some(&running), &dr_for);
    assert!(seen.sole_claimant(), "back to one claimant: {seen:?}");
    fire(&model, &mut state, "CensusClean");
}

/// A bundle carrying ANOTHER program's identifier is not a claimant, and a
/// requirement identical to the live install's is not a conflict.
///
/// Both directions matter: the first is what keeps the census from reporting
/// every `.app` on the disk, and the second is what keeps two legitimate
/// Developer-ID installs — an ordinary thing to have — from being named as a
/// hazard they cannot cause.
#[test]
fn the_real_census_reports_only_what_can_actually_destroy_a_grant() {
    let scratch = Scratch::new("destroy");
    let dir = &scratch.0;

    let live = dir.join("aterm.app");
    lay_bundle(&live, ID);
    let running = live.join("Contents/MacOS/aterm");
    lay_bundle(&dir.join("Safari.app"), "com.apple.Safari");
    lay_bundle(&dir.join("aterm-second.app"), ID);

    const SAME: &str = "designated => identifier \"x\" and anchor apple generic";
    let seen = census(dir, Some(&running), &|_| SAME);

    assert_eq!(
        seen.found.len(),
        2,
        "the two copies of US, and never Safari: {seen:?}"
    );
    assert!(
        !seen.found.iter().any(|c| c.path.ends_with("Safari.app")),
        "another program's bundle is not a claimant"
    );
    assert!(
        seen.conflicting().is_empty(),
        "a copy sharing the running requirement cannot cause the destructive \
         resolution, so it is counted and not named: {seen:?}"
    );
    assert!(
        !seen.sole_claimant(),
        "but it IS a second claimant, and `sole` must not claim otherwise"
    );
}

/// THE REAL READER, end to end: actual `codesign`, actual `Info.plist`.
///
/// Everything above injects the identity oracle, which is right for a test
/// about `classify_claimants`. But the census the product runs reads a bundle
/// through `bundle_identity` -> `codesign_report` -> `designated_requirement`,
/// and that chain carried a defect no injected oracle could have found:
/// `codesign` prints an IMPLICIT requirement — which every ad-hoc signature has
/// — as a COMMENT (`# designated => cdhash H"…"`), so the reader returned
/// `None` for exactly the bundles whose identity is unstable, `dr=` read
/// `unknown` instead of `cdhash`, and two DIFFERENT ad-hoc bundles both carried
/// the empty requirement and compared EQUAL. Measured against real fixtures
/// 2026-09-22; this is the test that would have caught it.
///
/// Hermetic: it signs its OWN bundles ad-hoc and never consults an installed
/// copy, so it asserts about this tree rather than about the machine.
#[test]
fn the_real_codesign_reader_tells_two_ad_hoc_bundles_apart() {
    let scratch = Scratch::new("codesign");
    let dir = &scratch.0;

    // Two bundles, same identifier, DIFFERENT code — so `codesign --sign -`
    // gives each its own cdhash, which is the whole point.
    for (name, marker) in [("aterm.app", "ONE"), ("aterm.app.rollback", "TWO")] {
        let root = dir.join(name);
        lay_bundle(&root, ID);
        std::fs::write(
            root.join("Contents/MacOS/aterm"),
            format!("#!/bin/sh\necho {marker}\n"),
        )
        .expect("distinct bytes");
        let signed = std::process::Command::new("/usr/bin/codesign")
            .args(["--force", "--sign", "-"])
            .arg(&root)
            .stdout(std::process::Stdio::null())
            .stderr(std::process::Stdio::null())
            .status();
        if !matches!(signed, Ok(s) if s.success()) {
            eprintln!("codesign unavailable; skipping the real-reader assertions");
            return;
        }
    }

    let running = dir.join("aterm.app/Contents/MacOS/aterm");
    let protected: Vec<PathBuf> = Vec::new();

    // The reader itself: a commented requirement is still a requirement.
    let live = aterm_containment::consent::bundle_identity(&dir.join("aterm.app"), &protected)
        .expect("the live bundle reads");
    assert_eq!(live.bundle_id.as_deref(), Some(ID));
    assert_eq!(live.signing, "adhoc");
    assert!(
        live.dr_text.starts_with("designated =>"),
        "the ad-hoc clause is COMMENTED by codesign and must still be read: {:?}",
        live.dr_text
    );
    assert!(live.dr_text.contains("cdhash"), "{:?}", live.dr_text);

    // And the census built on it tells the two apart.
    let seen = classify_claimants(
        ID,
        Some(&running),
        std::slice::from_ref(dir),
        |root| {
            let read = std::fs::read_dir(root).ok()?;
            Some(read.flatten().map(|e| e.path()).collect())
        },
        |root| aterm_containment::consent::bundle_identity(root, &protected),
    );
    assert_eq!(seen.found.len(), 2, "{seen:?}");
    assert!(
        seen.found
            .iter()
            .all(|c| c.dr == aterm_containment::DrClass::Cdhash),
        "an ad-hoc bundle is cdhash-keyed, not `unknown`: {seen:?}"
    );
    assert_eq!(
        seen.conflicting().len(),
        1,
        "two ad-hoc bundles with different cdhashes are two identities, not one: {seen:?}"
    );
}
