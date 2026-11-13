// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0

use super::*;

#[test]
fn a_record_keeps_its_landed_result_and_reports_a_failed_directory_flush() {
    use aterm_durable::seam::FailingDirs;
    let staging = crate::paths::Staging::scratch("record-directory-flush");
    let path = staging.installed_receipt();
    let commit = "c".repeat(40);
    let digest = "ab".repeat(32);
    let warnings = || {
        crate::log_capture::take()
            .into_iter()
            .filter(|(level, _)| *level == aterm_log::Level::Warn)
            .count()
    };
    let _ = crate::log_capture::take();
    for flush in [Flush::Durable, Flush::Ordered] {
        {
            let _failure = FailingDirs::with(libc::EIO);
            InstalledReceipt::record(&path, 3, &commit, &digest, flush).unwrap();
        }
        assert_eq!(InstalledReceipt::read(&path).unwrap().build_number, 3);
        assert_eq!(warnings(), 1);
        {
            let _refused = FailingDirs::with(libc::ENOTSUP);
            InstalledReceipt::record(&path, 4, &commit, &digest, flush).unwrap();
        }
        assert_eq!(InstalledReceipt::read(&path).unwrap().build_number, 4);
        assert_eq!(warnings(), 0);
    }
}

#[test]
fn a_failed_record_rename_keeps_the_destination_and_is_still_an_error() {
    use aterm_durable::seam::FailingDirs;
    let staging = crate::paths::Staging::scratch("record-rename-failure");
    let path = staging.root.join("occupied.toml");
    std::fs::create_dir(&path).unwrap();
    std::fs::write(path.join("witness"), b"keep").unwrap();
    let _ = crate::log_capture::take();
    let _refused = FailingDirs::with(libc::ENOTSUP);
    assert!(write_durable(&path, "new", "test record", Flush::Ordered).is_err());
    assert_eq!(std::fs::read(path.join("witness")).unwrap(), b"keep");
    assert!(
        std::fs::read_dir(&staging.root)
            .unwrap()
            .filter_map(Result::ok)
            .all(|e| !e.file_name().to_string_lossy().ends_with(".tmp"))
    );
    assert!(crate::log_capture::take().is_empty());
}

#[test]
fn each_record_write_names_its_flush() {
    use aterm_durable::seam::Recording;
    const O: Flush = Flush::Ordered;
    const D: Flush = Flush::Durable;
    let staging = crate::paths::Staging::scratch("flush-tiers");
    let digest = "ab".repeat(32);
    let recording = Recording::start();
    Floor::bump_and_write(&staging.floor(), 3, 0, 0);
    assert_eq!(
        recording.take(),
        [D, D],
        "the floor: its file and its entry"
    );
    InstalledReceipt::record(&staging.installed_receipt(), 3, &"c".repeat(40), &digest, O)
        .expect("receipt");
    assert_eq!(recording.take(), [O, O], "an ordered receipt");
    Trial::record_required(&staging.trial(), 3, &digest, Some(&staging.root)).expect("trial");
    assert_eq!(
        recording.take(),
        [O, O],
        "a trial beside a root on its device"
    );
    Trial::record_required(&staging.trial(), 3, &digest, None).expect("trial");
    assert_eq!(recording.take(), [D, D], "a trial with no root");
}

#[test]
fn a_vacuous_trial_marker_reads_as_absent() {
    let dir = crate::paths::scratch_dir("vacuous");
    let path = dir.join("trial.toml");
    std::fs::write(&path, "").unwrap();
    assert!(Trial::read(&path).is_none(), "zero length is absent");
    std::fs::write(&path, "build_number = 0\nsha256 = \"\"\n").unwrap();
    assert!(Trial::read(&path).is_none(), "all-defaults is absent");
    Trial::record_required(&path, 42, &"AB".repeat(32), None).unwrap();
    let read = Trial::read(&path).expect("a real marker reads back");
    assert_eq!(read.build_number, 42);
    assert_eq!(read.sha256, "ab".repeat(32), "stored lowercase");
    let strays: Vec<_> = std::fs::read_dir(&dir)
        .unwrap()
        .filter_map(Result::ok)
        .map(|e| e.file_name().to_string_lossy().into_owned())
        .filter(|name| name.contains(".tmp"))
        .collect();
    assert!(
        strays.is_empty(),
        "durable write leaves no scratch: {strays:?}"
    );
}

#[test]
fn a_trial_marker_an_earlier_updater_wrote_still_reads() {
    let dir = crate::paths::scratch_dir("trial-earlier");
    let path = dir.join("trial.toml");
    std::fs::write(
        &path,
        format!(
            "build_number = 42\nsha256 = \"{}\"\nattempts = 0\nretry_after = 0\n\
             quarantined = false\ninstall_root = \"/Applications/aterm.app\"\n",
            "ab".repeat(32)
        ),
    )
    .unwrap();
    let read = Trial::read(&path).expect("the in-flight trial's marker reads");
    assert!(read.matches(42, &"AB".repeat(32)));
    assert_eq!(
        read.install_root.as_deref(),
        Some("/Applications/aterm.app")
    );
}

#[test]
fn installed_receipt_is_exact_and_requires_sealed_bundle_identity() {
    let root = crate::paths::scratch_dir("installed-receipt");
    let path = root.join("installed.toml");
    let commit = "0123456789abcdef0123456789abcdef01234567";
    let digest = "ab".repeat(32);

    InstalledReceipt::record(&path, 53, commit, &digest, aterm_durable::Flush::Durable).unwrap();
    let receipt = InstalledReceipt::read(&path).expect("valid receipt");
    assert!(receipt.matches_sealed(53, commit));
    assert!(receipt.matches_sealed(53, "0123456789ab"));
    assert!(!receipt.matches_sealed(52, commit));
    assert!(!receipt.matches_sealed(53, "fedcba9876543210fedcba9876543210fedcba98"));

    for (bad_commit, bad_digest) in [
        ("0123456789ab", digest.as_str()),
        (commit, "ab"),
        (
            "fedcba9876543210fedcba9876543210fedcba98-dirty",
            digest.as_str(),
        ),
    ] {
        assert!(
            InstalledReceipt::record(
                &path,
                54,
                bad_commit,
                bad_digest,
                aterm_durable::Flush::Durable
            )
            .is_err()
        );
        assert_eq!(
            InstalledReceipt::read(&path),
            Some(receipt.clone()),
            "a rejected replacement must preserve the prior valid receipt"
        );
    }

    InstalledReceipt::clear(&path);
    assert!(InstalledReceipt::read(&path).is_none());
}

#[test]
fn concurrent_floor_writers_cannot_lose_independent_maxima() {
    let root = crate::paths::scratch_dir("floor-txn");
    let path = root.join("floor.toml");
    let start = std::sync::Arc::new(std::sync::Barrier::new(3));

    let writer = |seen_min_build, staged_build, seen_roster_seq| {
        let path = path.clone();
        let start = std::sync::Arc::clone(&start);
        std::thread::spawn(move || {
            start.wait();
            Floor::bump_and_write(&path, seen_min_build, staged_build, seen_roster_seq);
        })
    };
    let min_writer = writer(1_000, 1, 1);
    let high_writer = writer(1, 2_000, 9);
    start.wait();
    min_writer.join().unwrap();
    high_writer.join().unwrap();

    assert_eq!(
        Floor::read(&path),
        Floor {
            min_build: 1_000,
            high_water: 2_000,
            roster_seq: 9,
            ..Floor::default()
        },
        "read/max/write is one locked transaction; no coordinate may regress"
    );
}

#[test]
fn zip_container_is_parsed_when_present_and_optional_when_not() {
    let with_zip = Manifest::parse(
        r#"schema = 1
               version = "0.10.0"
               build_number = 1234
               sha256 = "abc123"
               dmg = "aterm-0.10.0.dmg"
               zip = "aterm-0.10.0-mac.zip"
               zip_sha256 = "def456""#,
    )
    .unwrap();
    assert_eq!(with_zip.zip.as_deref(), Some("aterm-0.10.0-mac.zip"));
    assert_eq!(with_zip.zip_sha256.as_deref(), Some("def456"));

    let without_zip = Manifest::parse(
        r#"schema = 1
               version = "0.10.0"
               build_number = 1234
               sha256 = "abc123"
               dmg = "aterm-0.10.0.dmg""#,
    )
    .unwrap();
    assert_eq!(without_zip.zip, None);
    assert_eq!(without_zip.zip_sha256, None);
    assert_eq!(without_zip.dmg, "aterm-0.10.0.dmg");
}

#[test]
fn parses_full_gen_appcast_shape() {
    let m = Manifest::parse(
        r#"
# Auto-generated by tools/gen-appcast.sh
schema = 1
version = "0.2.0"
build_number = 1234
commit = "deadbeefcafe"
dmg = "aterm-0.2.0.dmg"
sha256 = "ABC123"
min_os = "11.0"
team_id = "TEAMID1234"
pub_date = "2026-06-21T00:00:00Z"
url = "https://github.com/o/r/releases/download/v0.2.0/aterm-0.2.0.dmg"
changelog = '''
### Added
- a `thing` with "quotes" and # hashes
'''
"#,
    )
    .unwrap();
    assert_eq!(m.schema, 1);
    assert_eq!(m.version, "0.2.0");
    assert_eq!(m.build_number, 1234);
    assert_eq!(m.sha256, "ABC123");
    assert_eq!(m.dmg, "aterm-0.2.0.dmg");
    assert_eq!(
        m.commit.as_deref(),
        Some("deadbeefcafe"),
        "the appcast commit binds the build to a source commit — must be captured"
    );
    assert_eq!(
        m.min_build, None,
        "an appcast without the field still parses"
    );
}

#[test]
fn publishable_ready_requires_real_directory_and_binds_available_plist() {
    let staging = crate::paths::Staging::scratch("publishable-ready");
    let root = &staging.root;
    let commit = "0123456789abcdef0123456789abcdef01234567";
    let ready = Ready {
        build_number: 54,
        version: "0.54.1".into(),
        commit: Some(commit.into()),
        dmg_sha256: "ab".repeat(32),
        team_id: "T".into(),
        staged_at: String::new(),
        changelog: None,
        machine_id: None,
        roster_seq: None,
        roster_confirmed_at: Some(crate::install::now_rfc3339()),
    };
    std::fs::write(&staging.ready, ready.to_toml().unwrap()).unwrap();
    assert!(
        !ready.is_publishable(&staging),
        "marker alone is not a stage"
    );

    std::fs::create_dir_all(&staging.staged_app).unwrap();
    assert!(
        !ready.is_publishable(&staging),
        "an empty app directory cannot masquerade as a published release bundle"
    );

    let contents = staging.staged_app.join("Contents");
    std::fs::create_dir_all(&contents).unwrap();
    std::fs::write(
        contents.join("Info.plist"),
        format!(
            "<plist><dict><key>CFBundleVersion</key><string>54</string>\
             <key>ATermGitCommit</key><string>{commit}</string></dict></plist>"
        ),
    )
    .unwrap();
    assert!(ready.is_publishable(&staging));

    std::fs::write(
        contents.join("Info.plist"),
        format!(
            "<plist><dict><key>CFBundleVersion</key><string>53</string>\
             <key>ATermGitCommit</key><string>{commit}</string></dict></plist>"
        ),
    )
    .unwrap();
    assert!(
        !ready.is_publishable(&staging),
        "available staged build identity must agree with Ready"
    );

    std::fs::write(
        contents.join("Info.plist"),
        format!(
            "<plist><dict><key>CFBundleVersion</key><key>Other</key><string>54</string>\
             <key>ATermGitCommit</key><string>{commit}</string></dict></plist>"
        ),
    )
    .unwrap();
    assert!(
        !ready.is_publishable(&staging),
        "an intervening plist key cannot lend its string to CFBundleVersion"
    );

    std::fs::remove_dir_all(&staging.staged_app).unwrap();
    let target = root.join("target.app");
    std::fs::create_dir_all(&target).unwrap();
    std::os::unix::fs::symlink(&target, &staging.staged_app).unwrap();
    assert!(
        !ready.is_publishable(&staging),
        "a staged-app symlink never grants publishable authority"
    );
}

#[test]
fn an_unreadable_floor_publishes_nothing() {
    use std::os::unix::fs::PermissionsExt;
    let staging = crate::paths::Staging::scratch("publishable-floor-unreadable");
    let commit = "0123456789abcdef0123456789abcdef01234567";
    let ready = Ready {
        build_number: 54,
        version: "0.54.0".into(),
        commit: Some(commit.into()),
        dmg_sha256: "ab".repeat(32),
        team_id: "T".into(),
        staged_at: String::new(),
        changelog: None,
        machine_id: Some("m11".into()),
        roster_seq: None,
        roster_confirmed_at: Some(crate::install::now_rfc3339()),
    };
    std::fs::write(&staging.ready, ready.to_toml().unwrap()).unwrap();
    let contents = staging.staged_app.join("Contents");
    std::fs::create_dir_all(&contents).unwrap();
    std::fs::write(
        contents.join("Info.plist"),
        format!(
            "<plist><dict><key>CFBundleVersion</key><string>54</string>\
             <key>ATermGitCommit</key><string>{commit}</string></dict></plist>"
        ),
    )
    .unwrap();
    std::fs::write(staging.floor(), "roster_seq = 3\n").unwrap();
    assert!(Ready::read_publishable(&staging).is_some(), "PRECONDITION");
    std::fs::write(
        staging.floor(),
        "roster_seq = 4\nrevoked_machines = [\"m11\"]\n",
    )
    .unwrap();
    assert!(Ready::read_publishable(&staging).is_none(), "revoked");

    let mode =
        |bits| std::fs::set_permissions(staging.floor(), std::fs::Permissions::from_mode(bits));
    mode(0o000).unwrap();
    if std::fs::File::open(staging.floor()).is_ok() {
        aterm_uds::host::could_not_run(
            "a mode-0 file opened anyway: root, or a file system that ignores modes",
        );
    }
    let read = Ready::read_publishable(&staging);
    mode(0o600).unwrap();
    assert!(read.is_none(), "an unreadable floor admitted {read:?}");
}

#[test]
fn manifest_rejects_min_build_above_its_own_build() {
    Manifest::parse(
        r#"version = "1.0.0"
               build_number = 10
               sha256 = "x"
               dmg = "a.dmg"
               min_build = 11"#,
    )
    .unwrap_err();

    let m = Manifest::parse(
        r#"version = "1.0.0"
               build_number = 10
               sha256 = "x"
               dmg = "a.dmg"
               min_build = 10"#,
    )
    .unwrap();
    assert_eq!(m.min_build, Some(10));
}

#[test]
fn floor_ratchets_monotonically_and_reads_default() {
    let dir = crate::paths::scratch_dir("floor-ratchet");
    let p = dir.join("floor.toml");
    assert_eq!(Floor::read(&p).min_build, 0);
    assert_eq!(Floor::read(&p).high_water, 0);
    assert_eq!(Floor::read(&p).roster_seq, 0);
    Floor::bump_and_write(&p, 5, 12, 3);
    let f = Floor::read(&p);
    assert_eq!((f.min_build, f.high_water, f.roster_seq), (5, 12, 3));
    Floor::bump_and_write(&p, 3, 8, 2);
    let f = Floor::read(&p);
    assert_eq!((f.min_build, f.high_water, f.roster_seq), (5, 12, 3));
    Floor::bump_and_write(&p, 9, 12, 3);
    let f = Floor::read(&p);
    assert_eq!((f.min_build, f.high_water), (9, 12));
    Floor::bump_and_write(&p, 0, 0, 7);
    assert_eq!(Floor::read(&p).roster_seq, 7);
}

#[test]
fn a_floor_without_provenance_is_never_lowered_and_an_old_reader_sees_the_numbers() {
    #[derive(Deserialize)]
    struct OldFloor {
        #[serde(default)]
        min_build: u64,
        #[serde(default)]
        high_water: u64,
        #[serde(default)]
        roster_seq: u64,
    }
    let root = crate::paths::scratch_dir("floor-mixed");
    let path = root.join("floor.toml");
    let revoked = vec!["m11".to_string()];
    Floor::observe_and_write(
        &path,
        &FloorObservation {
            min_build: 9_000,
            high_water: 9_500,
            roster_seq: 9,
            machine: Some("m11"),
            revoked: &[],
        },
    );
    let old: OldFloor = aterm_toml::from_str(&std::fs::read_to_string(&path).unwrap())
        .expect("an older build parses the file this one wrote");
    assert_eq!(
        (old.min_build, old.high_water, old.roster_seq),
        (9_000, 9_500, 9)
    );

    std::fs::write(
        &path,
        "min_build = 9000\nhigh_water = 9500\nroster_seq = 9\n",
    )
    .unwrap();
    Floor::observe_and_write(
        &path,
        &FloorObservation {
            roster_seq: 10,
            revoked: &revoked,
            ..FloorObservation::default()
        },
    );
    let floor = Floor::read(&path);
    assert_eq!(
        (floor.min_build, floor.high_water, floor.roster_seq),
        (9_000, 9_500, 10),
        "an unattributed floor is never lowered: {floor:?}"
    );
    assert!(floor.revokes(Some("m11")) && !floor.revokes(None));

    let _ = std::fs::remove_file(&path);
    Floor::observe_and_write(
        &path,
        &FloorObservation {
            min_build: 9_000,
            high_water: 9_500,
            roster_seq: 9,
            machine: Some("m11"),
            revoked: &[],
        },
    );
    Floor::observe_and_write(
        &path,
        &FloorObservation {
            roster_seq: 10,
            revoked: &revoked,
            ..FloorObservation::default()
        },
    );
    let floor = Floor::read(&path);
    assert_eq!((floor.min_build, floor.high_water), (0, 0), "{floor:?}");
    assert_eq!(
        floor
            .withdraws(5, Some("m11"))
            .map(|why| why.contains("revoked")),
        Some(true)
    );
    assert_eq!(floor.withdraws(5, Some("m3")), None);
    let yank = Floor {
        min_build: 5,
        ..Floor::default()
    };
    assert_eq!(yank.withdraws(5, None), None);
    assert!(yank.withdraws(4, None).is_some());
}

#[test]
fn a_floor_that_cannot_be_committed_is_reported_and_leaves_no_temp_file() {
    let root = crate::paths::scratch_dir("floor-commit");
    let path = root.join("floor.toml");
    std::fs::create_dir(&path).unwrap();
    let tmp = path.with_extension(format!("toml.{}.tmp", std::process::id()));

    Floor::bump_and_write_reporting(&path, 5, 12, 3)
        .expect_err("a floor that cannot be committed must not report success");
    assert!(
        !tmp.exists(),
        "the temp file must never outlive a failed commit"
    );
    let leaked: Vec<String> = std::fs::read_dir(&root)
        .unwrap()
        .filter_map(Result::ok)
        .map(|e| e.file_name().to_string_lossy().into_owned())
        .filter(|name| name.ends_with(".tmp"))
        .collect();
    assert!(leaked.is_empty(), "a failed commit left temps: {leaked:?}");
    assert_eq!(Floor::read(&path), Floor::default());

    let ok_path = root.join("ok.toml");
    assert_eq!(Floor::bump_and_write_reporting(&ok_path, 5, 12, 3), Ok(()));
    assert_eq!(Floor::read(&ok_path).roster_seq, 3);
    let ok_tmp = ok_path.with_extension(format!("toml.{}.tmp", std::process::id()));
    assert!(!ok_tmp.exists(), "a successful commit leaves no temp file");
}

#[test]
fn a_floor_the_write_could_not_read_is_never_replaced_with_defaults() {
    use std::os::unix::fs::PermissionsExt;

    let dir = crate::paths::scratch_dir("floor-unread");
    let recorded = "min_build = 500\nhigh_water = 600\nroster_seq = 10\n\
                    revoked_machines = [\"m11\"]\n";
    let observation = FloorObservation {
        high_water: 700,
        machine: Some("m3"),
        ..FloorObservation::default()
    };
    let keeps_the_record = |floor: &Floor| {
        floor.roster_seq == 10 && floor.min_build == 500 && floor.revokes(Some("m11"))
    };

    let path = dir.join("floor.toml");
    std::fs::write(&path, recorded).unwrap();
    std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o000)).unwrap();
    let outcome = Floor::observe_reporting(&path, &observation);
    std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o600)).unwrap();
    let floor = Floor::read(&path);
    assert!(
        keeps_the_record(&floor),
        "an unreadable floor was replaced with defaults: {floor:?}"
    );
    assert!(
        outcome.is_err(),
        "the refused advance must be reported: {outcome:?}"
    );

    let ok_path = dir.join("ok.toml");
    std::fs::write(&ok_path, recorded).unwrap();
    assert_eq!(Floor::observe_reporting(&ok_path, &observation), Ok(()));
    let floor = Floor::read(&ok_path);
    assert!(keeps_the_record(&floor), "{floor:?}");
    assert_eq!(floor.high_water, 700);
}

#[test]
fn a_trial_marker_matches_on_build_and_hash_only() {
    let dir = crate::paths::scratch_dir("trial-match");
    let p = dir.join("trial.toml");
    assert!(Trial::read(&p).is_none());
    Trial::record(&p, 42, "ABCdef");
    let m = Trial::read(&p).unwrap();
    assert!(m.matches(42, "abcdef"), "hash compare is case-insensitive");
    assert!(!m.matches(43, "abcdef"), "different build");
    assert!(!m.matches(42, "999"), "different hash (re-published)");
    Trial::clear(&p);
    assert!(Trial::read(&p).is_none());
}

fn confirmed_ready(confirmed: Option<u64>) -> Ready {
    Ready {
        build_number: 20,
        version: "0.0.20".into(),
        commit: Some("0123456789abcdef0123456789abcdef01234567".into()),
        dmg_sha256: "ab".repeat(32),
        team_id: "T".into(),
        staged_at: String::new(),
        changelog: None,
        machine_id: Some("m11".into()),
        roster_seq: Some(9),
        roster_confirmed_at: confirmed.map(aterm_types::rfc3339::format_rfc3339),
    }
}

#[test]
fn a_stage_is_swapped_only_inside_a_day_of_its_roster_confirmation() {
    let now = 1_790_000_000;
    let day = ROSTER_FRESHNESS_SECS;
    for (confirmed, stale) in [
        (Some(now), false),
        (Some(now - 3_600), false),
        (Some(now - day + 1), false),
        (Some(now + ROSTER_CONFIRMATION_SKEW_SECS), false),
        (Some(now - day), true),
        (Some(now - day - 3_600), true),
        (Some(now + ROSTER_CONFIRMATION_SKEW_SECS + 1), true),
        (None, true),
    ] {
        let ready = confirmed_ready(confirmed);
        let refusal = ready.roster_authorization_stale(now, true);
        assert_eq!(refusal.is_some(), stale, "{confirmed:?}: {refusal:?}");
        if let Some(refusal) = refusal {
            assert!(crate::is_passing_refusal(&refusal), "{refusal}");
            assert!(
                crate::is_roster_confirmation_wait(&refusal),
                "the window's lane files it as the wait, not a failure: {refusal}"
            );
            assert!(
                refusal.contains(ROSTER_UNCONFIRMED_KEY) && refusal.contains("20"),
                "{refusal}"
            );
            if let Some(stamp) = ready.roster_confirmed_at.as_deref() {
                assert!(refusal.contains(stamp), "names when: {refusal}");
            }
        }
        assert_eq!(ready.roster_authorization_stale(now, false), None);
    }
    let mut malformed = confirmed_ready(Some(now));
    malformed.roster_confirmed_at = Some("2026-10-02 09:00:00".into());
    assert!(malformed.roster_authorization_stale(now, true).is_some());
    assert!(!crate::is_roster_confirmation_wait(
        &crate::verify::mark_passing("could not take the apply lock within 10s".to_string())
    ));
    assert!(!crate::is_roster_confirmation_wait(ROSTER_UNCONFIRMED_KEY));

    let reread = ROSTER_RECONFIRM_AFTER_SECS;
    assert!(!confirmed_ready(Some(now - reread + 1)).needs_roster_reconfirmation(now, true));
    assert!(confirmed_ready(Some(now - reread)).needs_roster_reconfirmation(now, true));
    assert!(confirmed_ready(None).needs_roster_reconfirmation(now, true));
    assert!(!confirmed_ready(None).needs_roster_reconfirmation(now, false));
    let oldest_at_quit =
        reread + crate::cadence::INTERVAL_SECS * (100 + crate::cadence::JITTER_PCT) / 100;
    assert!(
        oldest_at_quit <= 2 * 60 * 60,
        "a running client's stamp is about an hour old at most: {oldest_at_quit} s"
    );
    let quit = now - 13 * 3_600 - 1_800;
    assert_eq!(
        confirmed_ready(Some(quit - oldest_at_quit)).roster_authorization_stale(now, true),
        None,
        "an overnight quit does not make the morning's launch wait"
    );
    assert!(
        confirmed_ready(Some(now - day - 1))
            .roster_authorization_stale(now, true)
            .is_some(),
        "a day with no check does"
    );
}

#[test]
fn the_roster_confirmation_is_one_additive_key_both_ways() {
    #[derive(serde::Deserialize)]
    #[allow(dead_code)]
    struct ReadyV098 {
        build_number: u64,
        version: String,
        #[serde(default)]
        commit: Option<String>,
        dmg_sha256: String,
        team_id: String,
        staged_at: String,
        #[serde(default)]
        changelog: Option<String>,
        #[serde(default)]
        machine_id: Option<String>,
        #[serde(default)]
        roster_seq: Option<u64>,
    }
    let confirmed = confirmed_ready(Some(1_790_000_000));
    let text = confirmed.to_toml().unwrap();
    assert!(text.contains("roster_confirmed_at"), "{text}");
    let old: ReadyV098 = aterm_toml::from_str(&text).expect("v0.98.0 reads the new marker");
    assert_eq!((old.build_number, old.roster_seq), (20, Some(9)));

    let unconfirmed = confirmed_ready(None);
    let text = unconfirmed.to_toml().unwrap();
    assert!(
        !text.contains("roster_confirmed_at"),
        "skipped while empty: {text}"
    );
    let back: Ready = aterm_toml::from_str(&text).unwrap();
    assert_eq!(back.roster_confirmed_at, None);
    assert_eq!(
        aterm_toml::from_str::<Ready>(&confirmed.to_toml().unwrap())
            .unwrap()
            .roster_confirmed_at,
        confirmed.roster_confirmed_at
    );
}
