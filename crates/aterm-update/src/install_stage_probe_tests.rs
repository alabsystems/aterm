// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0

//! The stage's start probe (`verify::probe_staged_bundle_starts`), driven through
//! the real stage gate on an ad-hoc-signed bundle in the structural tier: the
//! candidate runs only once every check before it has passed, and one that cannot
//! start is refused before any ready marker.

use super::*;

const BUILD: u64 = 777;
const COMMIT: &str = "0123456789abcdef0123456789abcdef01234567";

/// A manifest naming `build` and `commit`, everything else inert.
fn manifest(build: u64, commit: &str) -> Manifest {
    Manifest {
        schema: 1,
        version: "0.0.777".into(),
        build_number: build,
        commit: Some(commit.into()),
        sha256: "ab".repeat(32),
        dmg: "aterm-0.0.777.dmg".into(),
        url: None,
        zip: None,
        zip_sha256: None,
        min_build: None,
        changelog: None,
        machine_id: None,
        roster_seq: None,
    }
}

/// An ad-hoc-signed `staged/aterm.app.incoming` sealed as [`BUILD`] / [`COMMIT`],
/// whose executable writes `ran` each time it starts and exits `exit`.
fn incoming(staging: &Staging, ran: &Path, exit: i32) -> PathBuf {
    use std::os::unix::fs::PermissionsExt as _;
    ensure_private_dir(&staging.staged_dir()).unwrap();
    let app = staging.staged_dir().join("aterm.app.incoming");
    let exe = app.join("Contents/MacOS/aterm");
    std::fs::create_dir_all(exe.parent().unwrap()).unwrap();
    std::fs::write(
        &exe,
        format!("#!/bin/sh\nprintf ran > '{}'\nexit {exit}\n", ran.display()),
    )
    .unwrap();
    std::fs::set_permissions(&exe, std::fs::Permissions::from_mode(0o755)).unwrap();
    std::fs::write(
        app.join("Contents/Info.plist"),
        format!(
            "<?xml version=\"1.0\" encoding=\"UTF-8\"?>\n<plist version=\"1.0\"><dict>\
             <key>CFBundleExecutable</key><string>aterm</string>\
             <key>CFBundleIdentifier</key><string>com.aterm.fixture</string>\
             <key>CFBundleVersion</key><string>{BUILD}</string>\
             <key>ATermGitCommit</key><string>{COMMIT}</string></dict></plist>\n"
        ),
    )
    .unwrap();
    aterm_uds::deadline::sign_ad_hoc(&app);
    app
}

/// Stage `incoming` against `manifest` as both container paths do, in the
/// structural tier (no Team ID pinned).
fn stage(staging: &Staging, incoming: &Path, manifest: &Manifest) -> Result<(), String> {
    let commit = staging_manifest_commit(manifest)?;
    verify_and_publish_incoming(staging, incoming, manifest, &commit, "", "")
}

/// THE CANDIDATE RUNS ONLY ONCE ITS WHOLE GATE HAS PASSED. A broken seal, a
/// build or a commit that does not match the manifest each refuse the stage
/// before anything of the bundle executes (its executable never wrote `ran`),
/// with no marker and no bundle left; a bundle that passes every check is started
/// once, before its marker, and published.
#[test]
fn the_stage_starts_the_candidate_only_after_its_whole_gate_passes() {
    // Signing and judging a bundle wait on trustd.
    if aterm_uds::deadline::trust_service_silent() {
        return;
    }
    /// What the case does to the bundle's executable, and the manifest it stages under.
    type Prepare = fn(&Path) -> Manifest;
    let refusals: [(&str, Prepare); 3] = [
        ("failed verification", |exe| {
            // Tampered after signing: the seal no longer holds.
            let mut text = std::fs::read_to_string(exe).unwrap();
            text.push_str("# changed\n");
            std::fs::write(exe, text).unwrap();
            manifest(BUILD, COMMIT)
        }),
        ("!= manifest build_number", |_| manifest(BUILD + 1, COMMIT)),
        ("does not match manifest commit", |_| {
            manifest(BUILD, "1111111111111111111111111111111111111111")
        }),
    ];
    for (why, prepare) in refusals {
        let staging = Staging::scratch("stage-probe-gate");
        let ran = staging.root.join("ran");
        let app = incoming(&staging, &ran, 0);
        let manifest = prepare(&app.join("Contents/MacOS/aterm"));
        let error = stage(&staging, &app, &manifest).expect_err(why);
        assert!(error.contains(why), "{why}: {error}");
        assert!(
            !ran.exists(),
            "{why}: the candidate ran before its gate passed"
        );
        assert!(!staging.ready.exists(), "{why}: a marker was published");
        assert!(!app.exists(), "{why}: the refused bundle was left behind");
    }

    let staging = Staging::scratch("stage-probe-pass");
    let ran = staging.root.join("ran");
    let app = incoming(&staging, &ran, 0);
    stage(&staging, &app, &manifest(BUILD, COMMIT)).expect("a passing bundle publishes");
    assert!(ran.exists(), "the stage started the candidate");
    let ready = Ready::read(&staging.ready).expect("the marker is published");
    assert_eq!(ready.build_number, BUILD);
    assert!(staging.staged_app.join("Contents/MacOS/aterm").is_file());
}

/// A BUILD THE RECORDS WITHDREW WHILE IT DOWNLOADED IS NEVER STARTED. A sibling's
/// check recorded the revocation of the machine that signed it; the stage refuses
/// it with the publish's words, and its executable never ran, though every check
/// of the bundle itself passed.
#[test]
fn a_build_withdrawn_while_it_downloaded_is_never_started() {
    if aterm_uds::deadline::trust_service_silent() {
        return;
    }
    let staging = Staging::scratch("stage-probe-withdrawn");
    let revoked = vec!["m11".to_string()];
    crate::manifest::Floor::observe_and_write(
        &staging.floor(),
        &crate::manifest::FloorObservation {
            roster_seq: 10,
            revoked: &revoked,
            ..crate::manifest::FloorObservation::default()
        },
    );
    let ran = staging.root.join("ran");
    let app = incoming(&staging, &ran, 0);
    let signed_by_revoked = Manifest {
        machine_id: Some("m11".into()),
        ..manifest(BUILD, COMMIT)
    };
    let error = stage(&staging, &app, &signed_by_revoked).expect_err("withdrawn");
    assert!(
        error.contains("which the machine roster has revoked — withdrawn"),
        "{error}"
    );
    assert!(!ran.exists(), "a withdrawn build was started");
    assert!(!staging.ready.exists());
}

/// A CANDIDATE THAT CANNOT START IS A STAGE VERDICT, refused before its marker:
/// the bundle is gone, nothing is published, and the refusal is no passing
/// moment, so the check books it against the build like any other verdict.
#[test]
fn a_candidate_that_cannot_start_is_refused_before_any_marker() {
    if aterm_uds::deadline::trust_service_silent() {
        return;
    }
    let staging = Staging::scratch("stage-probe-fails");
    let ran = staging.root.join("ran");
    let app = incoming(&staging, &ran, 1);
    let error = stage(&staging, &app, &manifest(BUILD, COMMIT))
        .expect_err("a candidate that exits 1 cannot be staged");
    assert!(error.starts_with("staged bundle cannot start: "), "{error}");
    assert!(!crate::is_passing_refusal(&error), "{error}");
    assert!(ran.exists(), "it was started");
    assert!(!staging.ready.exists());
    assert!(!app.exists());
    assert!(!staging.staged_app.exists());
}

/// A PROBE THAT DOES NOT FINISH IS NO VERDICT: a candidate still running at the
/// deadline (a loaded machine, a slow first-exec scan) is killed, and the stage
/// publishes — the apply's own probe still has to pass before any swap, so
/// nothing is trusted that was not, and the download is not thrown away.
#[test]
fn a_stage_probe_that_does_not_finish_publishes_for_the_apply_to_judge() {
    if aterm_uds::deadline::trust_service_silent() {
        return;
    }
    use std::os::unix::fs::PermissionsExt as _;
    let staging = Staging::scratch("stage-probe-slow");
    let ran = staging.root.join("ran");
    let app = incoming(&staging, &ran, 0);
    // Re-sealed as a candidate that never answers `--version`.
    let exe = app.join("Contents/MacOS/aterm");
    std::fs::write(
        &exe,
        format!(
            "#!/bin/sh\nprintf ran > '{}'\nexec sleep 600\n",
            ran.display()
        ),
    )
    .unwrap();
    std::fs::set_permissions(&exe, std::fs::Permissions::from_mode(0o755)).unwrap();
    aterm_uds::deadline::sign_ad_hoc(&app);
    let staged = {
        // The deadline the probe outlives; the gate before it needs a fraction.
        let _budget = verify::ApplyBudget::start(std::time::Duration::from_secs(5));
        stage(&staging, &app, &manifest(BUILD, COMMIT))
    };
    staged.expect("a probe that did not finish is no verdict");
    assert!(ran.exists(), "the candidate was started");
    assert_eq!(
        Ready::read(&staging.ready).map(|r| r.build_number),
        Some(BUILD)
    );
}
