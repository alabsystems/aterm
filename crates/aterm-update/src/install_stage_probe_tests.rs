// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0

//! Tests for `verify::probe_staged_bundle_starts` driven through the real stage gate.

use super::*;

const BUILD: u64 = 777;
const COMMIT: &str = "0123456789abcdef0123456789abcdef01234567";

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
/// whose executable writes its pid to `ran` each time it starts, and `stamped` to
/// [`stamped_marker`] when it finds its exec stamp, and exits `exit`.
fn incoming(staging: &Staging, ran: &Path, exit: i32) -> PathBuf {
    use std::os::unix::fs::PermissionsExt as _;
    ensure_private_dir(&staging.staged_dir()).unwrap();
    let app = staging.staged_dir().join("aterm.app.incoming");
    let exe = app.join("Contents/MacOS/aterm");
    std::fs::create_dir_all(exe.parent().unwrap()).unwrap();
    std::fs::write(
        &exe,
        format!(
            "#!/bin/sh\nprintf %s $$ > '{}'\n[ -f '{}' ] && printf stamped > '{}'\nexit {exit}\n",
            ran.display(),
            incoming_exec_home(&app)
                .join(aterm_uds::deadline::EXEC_STAMP)
                .display(),
            stamped_marker(ran).display()
        ),
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

/// Where [`incoming`]'s executable says it found its exec stamp when it started.
fn stamped_marker(ran: &Path) -> PathBuf {
    ran.with_extension("stamped")
}

fn stage(staging: &Staging, incoming: &Path, manifest: &Manifest) -> Result<(), String> {
    let commit = staging_manifest_commit(manifest)?;
    verify_and_publish_incoming(staging, incoming, manifest, &commit, "", "")
}

#[test]
fn the_stage_starts_the_candidate_only_after_its_whole_gate_passes() {
    aterm_uds::deadline::require_trust_service();
    type Prepare = fn(&Path) -> Manifest;
    let refusals: [(&str, Prepare); 3] = [
        ("failed verification", |exe| {
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
        assert!(
            !incoming_exec_home(&app).exists(),
            "{why}: stamped unstarted"
        );
    }

    let staging = Staging::scratch("stage-probe-pass");
    let ran = staging.root.join("ran");
    let app = incoming(&staging, &ran, 0);
    stage(&staging, &app, &manifest(BUILD, COMMIT)).expect("a passing bundle publishes");
    assert!(ran.exists(), "the stage started the candidate");
    assert!(
        stamped_marker(&ran).exists(),
        "the candidate's exec was stamped before the probe started it"
    );
    assert!(
        !incoming_exec_home(&app).exists(),
        "its exit of its own ends the stamp"
    );
    let ready = Ready::read(&staging.ready).expect("the marker is published");
    assert_eq!(ready.build_number, BUILD);
    assert!(staging.staged_app.join("Contents/MacOS/aterm").is_file());
}

/// NO STAMP, NO PROBE. When the exec stamp cannot be written in its new directory (here an
/// access list the directory inherits denies adding a file to it), the stage refuses before
/// the probe: the candidate never ran, nothing is published, and neither the bundle nor a
/// stampless directory that would age it on the wall clock is left.
#[test]
fn a_stage_whose_exec_stamp_cannot_be_written_starts_nothing() {
    aterm_uds::deadline::require_trust_service();
    let chmod = |args: &[&std::ffi::OsStr]| {
        let ran = aterm_uds::spawnfd::command("/bin/chmod")
            .args(args)
            .status()
            .expect("run chmod");
        assert!(ran.success(), "chmod {args:?}");
    };
    let staging = Staging::scratch("stage-probe-unstamped");
    let ran = staging.root.join("ran");
    let app = incoming(&staging, &ran, 0);
    let staged = staging.staged_dir();
    chmod(&[
        "+a".as_ref(),
        "everyone deny add_file,directory_inherit,only_inherit".as_ref(),
        staged.as_os_str(),
    ]);
    let refused = stage(&staging, &app, &manifest(BUILD, COMMIT));
    chmod(&["-N".as_ref(), staged.as_os_str()]);
    let error = refused.expect_err("a stage whose stamp cannot be written refuses");
    assert!(
        error.contains("could not stamp the staged bundle") && crate::is_passing_refusal(&error),
        "{error}"
    );
    assert!(!ran.exists(), "the candidate ran without its stamp");
    assert!(!staging.ready.exists());
    assert!(!app.exists());
    assert!(!incoming_exec_home(&app).exists());
    assert!(!staging.staged_app.exists());
}

#[test]
fn a_build_withdrawn_while_it_downloaded_is_never_started() {
    aterm_uds::deadline::require_trust_service();
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

#[test]
fn a_candidate_that_cannot_start_is_refused_before_any_marker() {
    aterm_uds::deadline::require_trust_service();
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
    assert!(!incoming_exec_home(&app).exists());
    assert!(!staging.staged_app.exists());
}

#[test]
fn a_stage_probe_that_runs_to_no_answer_publishes_for_the_apply_to_judge() {
    aterm_uds::deadline::require_trust_service();
    let refuse_next_probe = |errno| {
        verify::STAGE_PROBE_SPAWN_REFUSAL.with(|refusal| refusal.set(Some(errno)));
    };
    let staging = Staging::scratch("stage-probe-no-answer");
    let ran = staging.root.join("ran");
    let app = incoming(&staging, &ran, 0);
    refuse_next_probe(libc::EAGAIN);
    stage(&staging, &app, &manifest(BUILD, COMMIT))
        .expect("a probe that ran to no answer is no verdict");
    assert!(!ran.exists(), "the refused spawn started nothing");
    assert_eq!(
        Ready::read(&staging.ready).map(|r| r.build_number),
        Some(BUILD)
    );
    assert!(staging.staged_app.join("Contents/MacOS/aterm").is_file());
    assert!(
        !incoming_exec_home(&app).exists(),
        "a spawn refused started nothing to hold"
    );

    let staging = Staging::scratch("stage-probe-timed-out");
    let ran = staging.root.join("ran");
    let app = incoming(&staging, &ran, 0);
    verify::STAGE_PROBE_TIMED_OUT.with(|timed_out| timed_out.set(true));
    let error = stage(&staging, &app, &manifest(BUILD, COMMIT))
        .expect_err("a timed-out probe does not publish or move the assessed path");
    assert!(crate::is_passing_refusal(&error), "{error}");
    assert!(!staging.ready.exists());
    assert!(
        app.join("Contents/MacOS/aterm").is_file(),
        "incoming stays at its path while syspolicyd may still be assessing it"
    );
    let home = incoming_exec_home(&app);
    assert!(
        home.join(aterm_uds::deadline::EXEC_STAMP).is_file()
            && !aterm_uds::deadline::copy_may_go(&home, std::time::SystemTime::now()),
        "held by the stamp written before its probe, which a later check's sweep ages"
    );
    assert!(!staging.staged_app.exists());

    let staging = Staging::scratch("stage-probe-refused");
    let ran = staging.root.join("ran");
    let app = incoming(&staging, &ran, 0);
    refuse_next_probe(libc::EACCES);
    let error = stage(&staging, &app, &manifest(BUILD, COMMIT))
        .expect_err("a probe refused for good is a verdict");
    assert!(
        error.starts_with("staged bundle cannot start: spawn candidate --version probe"),
        "{error}"
    );
    assert!(!crate::is_passing_refusal(&error), "{error}");
    assert!(!staging.ready.exists());
    assert!(!app.exists());
    assert!(!incoming_exec_home(&app).exists());
    assert!(!staging.staged_app.exists());
}
