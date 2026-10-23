// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0

/// `ditto` names files the bundle chose, so a line naming one after the passing key
#[test]
fn a_ditto_failure_quoting_a_refusal_key_is_still_a_refusal() {
    use std::os::unix::process::ExitStatusExt as _;
    let status = std::process::ExitStatus::from_raw(1 << 8);
    let unpack = |stderr: &str| {
        super::unpack_failure(
            std::path::Path::new("/tmp"),
            "ditto extract failed",
            status,
            stderr,
        )
    };
    let refusal = unpack(&format!(
        "ditto: /mnt/A.app/./Contents/Resources/q: {}: Permission denied\n",
        crate::PASSING_REFUSAL_KEY
    ));
    assert!(!crate::is_passing_refusal(&refusal), "{refusal}");
    assert!(refusal.contains("Resources/q: "), "{refusal}");
    let plain = unpack("ditto: /mnt/A.app/./Contents/MacOS/A: Permission denied\n");
    assert!(plain.ends_with("MacOS/A: Permission denied"), "{plain}");
}

/// An installed bundle replaced by a hand-built, ad-hoc-signed one cannot be the
#[test]
fn a_process_with_no_replaceable_bundle_refuses_before_anything_parks() {
    super::preverify_installed_rollback_source(None, 100, Some("abcdef0"))
        .expect_err("no replaceable bundle cannot host a swap");
    let root = std::env::temp_dir().join(format!(
        "aterm-rollback-source-{}-{}",
        std::process::id(),
        super::unix_now_secs()
    ));
    let error = super::preverify_installed_rollback_source(Some(&root), 100, Some("abcdef0"))
        .expect_err("a path that is not a bundle cannot be a rollback source");
    assert!(
        error.contains(&root.display().to_string()),
        "the refusal names the bundle it examined: {error}"
    );
}

/// The post-swap re-exec command pins `--window` first and carries the re-exec nonce,
#[test]
fn boot_reexec_command_pins_mode_and_restores_handoff_env() {
    let handoff_env = vec![(
        std::ffi::OsString::from("ATERM_HANDOFF_PARENT_PID"),
        std::ffi::OsString::from("4242"),
    )];
    let command = super::boot_reexec_command(
        std::path::Path::new("/Applications/aterm.app/Contents/MacOS/aterm"),
        [std::ffi::OsString::from("--headless")].into_iter(),
        "0123456789abcdef",
        1785910394,
        &handoff_env,
    );
    assert_eq!(
        command.get_program(),
        std::ffi::OsStr::new("/Applications/aterm.app/Contents/MacOS/aterm")
    );
    assert_eq!(
        command.get_args().collect::<Vec<_>>(),
        ["--window", "--headless"],
        "the mode pin must precede the forwarded args"
    );
    let envs: Vec<_> = command.get_envs().collect();
    assert!(
        envs.contains(&(
            std::ffi::OsStr::new("ATERM_UPDATE_REEXEC"),
            Some(std::ffi::OsStr::new("0123456789abcdef")),
        )),
        "re-exec nonce set"
    );
    assert!(
        envs.contains(&(
            std::ffi::OsStr::new("ATERM_UPDATED_FROM"),
            Some(std::ffi::OsStr::new("1785910394")),
        )),
        "the cold-launch apply names the build it came from, so the successor shows \
         the post-update notice"
    );
    assert!(
        envs.contains(&(
            std::ffi::OsStr::new("ATERM_HANDOFF_PARENT_PID"),
            Some(std::ffi::OsStr::new("4242")),
        )),
        "handoff authority restored onto the exec image"
    );
}

/// The crash-loop revert's re-exec is pinned to its mode, as the boot swap's is: a
#[test]
fn rollback_reexec_command_pins_mode_and_restores_handoff_env() {
    let osv = |list: &[&str]| -> Vec<std::ffi::OsString> {
        list.iter().map(std::ffi::OsString::from).collect()
    };
    let handoff_env = vec![(
        std::ffi::OsString::from("ATERM_HANDOFF_PARENT_PID"),
        std::ffi::OsString::from("4242"),
    )];
    let exe = std::path::Path::new("/Applications/aterm.app/Contents/MacOS/aterm");
    for (launch, expected) in [
        (osv(&["--window"]), osv(&["--window"])),
        (osv(&[]), osv(&["--window"])),
        (
            osv(&["--window", "--window", "-d", "/tmp"]),
            osv(&["--window", "-d", "/tmp"]),
        ),
        (
            osv(&["--headless", "--control-sock", "/tmp/a.sock"]),
            osv(&["--window", "--headless", "--control-sock", "/tmp/a.sock"]),
        ),
        (
            osv(&["new-window", "-d", "/tmp"]),
            osv(&["new-window", "-d", "/tmp"]),
        ),
    ] {
        let command = super::rollback_reexec_command(exe, launch.clone().into_iter(), &handoff_env);
        assert_eq!(command.get_program(), exe.as_os_str());
        assert_eq!(
            command.get_args().collect::<Vec<_>>(),
            expected
                .iter()
                .map(|arg| arg.as_os_str())
                .collect::<Vec<_>>(),
            "{launch:?}: the revert's argv is the boot swap's"
        );
        assert!(
            command.get_envs().any(|pair| pair
                == (
                    std::ffi::OsStr::new("ATERM_HANDOFF_PARENT_PID"),
                    Some(std::ffi::OsStr::new("4242")),
                )),
            "handoff authority restored onto the exec image"
        );
    }
}

/// `rfc3339_delta_secs` inverts `format_rfc3339` across day and leap boundaries and
#[test]
fn rfc3339_delta_round_trips_and_fails_closed() {
    for (a, b, want) in [
        (0u64, 61, 61),
        (86_399, 86_401, 2),
        (1_709_164_799, 1_709_164_800, 1),
        (1_784_100_000, 1_784_101_800, 1_800),
    ] {
        let (ea, eb) = (super::format_rfc3339(a), super::format_rfc3339(b));
        assert_eq!(
            super::rfc3339_delta_secs(&ea, &eb),
            Some(want),
            "{ea}..{eb}"
        );
        assert_eq!(
            super::rfc3339_delta_secs(&eb, &ea),
            None,
            "reversed underflows to None"
        );
    }
    assert_eq!(super::rfc3339_delta_secs("", &super::now_rfc3339()), None);
    assert_eq!(
        super::rfc3339_delta_secs("garbage-not-a-date-Z", "2026-07-15T00:00:00Z"),
        None
    );
}
use super::*;
use std::sync::atomic::{AtomicU32, Ordering};

static SEQ: AtomicU32 = AtomicU32::new(0);

fn temp_staging() -> (crate::paths::ScratchStaging, std::path::PathBuf) {
    let s = Staging::scratch("rr");
    std::fs::create_dir_all(s.staged_dir()).unwrap();
    let root = s.root.clone();
    (s, root)
}

fn write_ready(s: &Staging, build: u64) {
    let r = Ready {
        build_number: build,
        version: format!("0.0.{build}"),
        commit: Some("0123456789abcdef0123456789abcdef01234567".into()),
        dmg_sha256: "ab".repeat(32),
        team_id: "T".into(),
        staged_at: String::new(),
        changelog: None,
        machine_id: None,
        roster_seq: None,
        roster_confirmed_at: Some(crate::install::now_rfc3339()),
    };
    std::fs::write(&s.ready, r.to_toml().unwrap()).unwrap();
}

/// Two processes arming one build swap it in once (Tier-1 of
#[test]
fn two_processes_arming_one_build_swap_it_once() {
    use aterm_spec::interp::State;
    type Lane = fn(
        &Staging,
        &mut dyn FnMut() -> Result<FileLock, ApplyOutcome>,
    ) -> Result<Ready, ApplyOutcome>;
    let model = aterm_models_update::native_update_two_process_apply_model();
    let project = |ready: bool, a_done: bool, b_peeked: bool, b_done: bool, swaps: i64| {
        State::from([
            ("ready", i64::from(ready)),
            ("a_done", i64::from(a_done)),
            ("b_peeked", i64::from(b_peeked)),
            ("b_done", i64::from(b_done)),
            ("swaps", swaps),
        ])
    };
    let marker_present = |s: &Staging| matches!(read_ready(s, 10), ReadyState::Newer(_));
    let acquire = |s: &Staging| {
        FileLock::acquire_within(&s.apply_lock, aterm_spec::wait::MUST_HAPPEN)
            .map_err(|e| ApplyOutcome::Deferred(format!("lock: {e}")))
    };
    let shipped: Lane =
        |s, lock| apply_lock_then_ready(s, 10, lock, || Ok(())).map(|(_lock, (), ready)| ready);
    let peek_trusting: Lane = |s, lock| {
        let peek = read_ready(s, 10);
        let _lock = lock()?;
        match peek {
            ReadyState::Newer(ready) => Ok(ready),
            _ => Err(ApplyOutcome::NoUpdate),
        }
    };
    let race = |b_lane: Lane| {
        let (s, root) = temp_staging();
        write_ready(&s, 11);
        let init = project(true, false, false, false, 0);
        let mut walked = vec![init.clone()];
        let b_decision = b_lane(&s, &mut || {
            let peeked = project(marker_present(&s), false, true, false, 0);
            walked.push(peeked.clone());
            let a_ready = shipped(&s, &mut || {
                let held = acquire(&s)?;
                assert!(
                    FileLock::acquire_within(&s.apply_lock, std::time::Duration::ZERO).is_err(),
                    "B waits behind A: the lock serializes the two processes"
                );
                Ok(held)
            })
            .expect("A finds the stage under the lock");
            assert_eq!(a_ready.build_number, 11);
            s.retire_published();
            walked.push(project(marker_present(&s), true, true, false, 1));
            acquire(&s)
        });
        let b_swaps = i64::from(b_decision.is_ok());
        walked.push(project(marker_present(&s), true, true, true, 1 + b_swaps));
        let _ = std::fs::remove_dir_all(&root);
        (b_swaps, walked)
    };

    let (b_swaps, walked) = race(shipped);
    assert_eq!(
        b_swaps, 0,
        "B's under-lock re-read must find nothing left to apply"
    );
    let [init, peeked, swapped, done] = &walked[..] else {
        panic!("the race walks four states: {walked:?}");
    };
    for (action, from, to) in [
        ("BPeek", init, peeked),
        ("ASwap", peeked, swapped),
        ("BApply", swapped, done),
    ] {
        assert!(
            model.successors(action, from).contains(to),
            "{action}: the real step is not a model transition: {to:?}"
        );
    }
    assert!(model.check_invariant("OneSwapPerBuild", done));

    let (b_swaps, walked) = race(peek_trusting);
    assert_eq!(b_swaps, 1, "a lane acting on its peek swaps build 11 again");
    let (swapped, second) = (&walked[2], &walked[3]);
    let buggy = aterm_spec::interp::with_buggy(&model, 1);
    assert!(
        buggy.successors("BApply", swapped).contains(second),
        "the peek-trusting lane is the model's Buggy = 1 step: {second:?}"
    );
    assert!(!model.check_invariant("OneSwapPerBuild", second));

    let (s, root) = temp_staging();
    write_ready(&s, 11);
    assert!(shipped(&s, &mut || acquire(&s)).is_ok());
    s.retire_published();
    assert!(matches!(
        shipped(&s, &mut || acquire(&s)),
        Err(ApplyOutcome::NoUpdate)
    ));
    let _ = std::fs::remove_dir_all(&root);
}

/// A `ready.toml` whose staged bundle is gone clears itself: this recovery runs
#[test]
fn a_ready_marker_whose_staged_bundle_vanished_retires_instead_of_deferring_forever() {
    let (s, root) = temp_staging();
    write_ready(&s, 7);
    let installed = root.join("Applications").join("aterm.app");
    std::fs::create_dir_all(&installed).unwrap();
    let ready = Ready::read(&s.ready).unwrap();
    assert!(
        !s.staged_app.exists(),
        "the staged bundle is the missing half"
    );
    assert!(
        !rollback_path(&installed).exists(),
        "and nothing at the fixed path could ever satisfy the marker"
    );

    assert!(
        recover_orphaned_prepared_candidate(&s, &installed, 6, &ready).is_ok(),
        "a dangling marker is a self-healing condition, not a permanent refusal"
    );
    assert!(!s.ready.exists(), "the dangling marker cleared itself");
    assert!(matches!(read_ready(&s, 6), ReadyState::Absent));
    let _ = std::fs::remove_dir_all(root);
}

/// The attribution fields are optional on the wire: a `ready.toml` written
#[test]
fn a_ready_marker_without_attribution_still_parses() {
    let legacy = r#"
build_number = 42
version = "0.42.0"
dmg_sha256 = "abcd"
team_id = "T"
staged_at = "2026-08-17T00:00:00Z"
"#;
    let ready: Ready = aterm_toml::from_str(legacy).expect("pre-attribution marker parses");
    assert_eq!(ready.build_number, 42);
    assert_eq!(ready.machine_id, None);
    assert_eq!(ready.roster_seq, None);
}

/// An oversized changelog would make the marker invisible, not large (every
#[test]
fn an_oversized_manifest_changelog_is_clamped_by_characters_not_bytes() {
    let wide = "—".repeat(MAX_READY_CHANGELOG_CHARS * 4);
    let clamped = clamp_ready_changelog(Some(&wide)).unwrap();
    assert!(clamped.starts_with('—'), "the head survives verbatim");
    assert!(clamped.ends_with(READY_CHANGELOG_TRUNCATED));
    assert_eq!(
        clamped.chars().count(),
        MAX_READY_CHANGELOG_CHARS + READY_CHANGELOG_TRUNCATED.chars().count()
    );

    let whole_changelog_md = "x".repeat(382 * 1024);
    let ready = Ready {
        build_number: 9,
        version: "0.9.0".into(),
        commit: Some("0123456789abcdef0123456789abcdef01234567".into()),
        dmg_sha256: "ab".repeat(32),
        team_id: "T".into(),
        staged_at: String::new(),
        changelog: clamp_ready_changelog(Some(&whole_changelog_md)),
        machine_id: None,
        roster_seq: None,
        roster_confirmed_at: Some(crate::install::now_rfc3339()),
    };
    let marker = ready.to_toml().unwrap();
    assert!(
        u64::try_from(marker.len()).unwrap() <= crate::MAX_LEDGER_BYTES,
        "clamped marker is {} bytes",
        marker.len()
    );
    let (s, _root) = temp_staging();
    std::fs::write(&s.ready, &marker).unwrap();
    assert!(crate::read_ledger_text(&s.ready).is_some());
}

/// A verified bundle unpacked beside the stage, its `Info.plist` naming `build` built
fn incoming_stage(s: &Staging, build: u64, commit: &str) -> PathBuf {
    let incoming = s.staged_dir().join("aterm.app.incoming");
    make_app(&incoming, "R");
    std::fs::create_dir_all(incoming.join("Contents")).unwrap();
    std::fs::write(
        incoming.join("Contents/Info.plist"),
        format!(
            "<plist><dict><key>CFBundleVersion</key><string>{build}</string>\
             <key>ATermGitCommit</key><string>{commit}</string></dict></plist>"
        ),
    )
    .unwrap();
    incoming
}

/// A STAGE WITHDRAWN WHILE IT DOWNLOADED IS NEVER PUBLISHED. The publish shares the
#[test]
fn a_stage_withdrawn_while_it_downloaded_is_never_published() {
    let commit = "0123456789abcdef0123456789abcdef01234567";
    let ready = Ready {
        build_number: 7,
        version: "0.0.7".into(),
        commit: Some(commit.into()),
        dmg_sha256: "ab".repeat(32),
        team_id: "T".into(),
        staged_at: String::new(),
        changelog: None,
        machine_id: Some("m11".into()),
        roster_seq: Some(9),
        roster_confirmed_at: Some(crate::install::now_rfc3339()),
    };

    let (s, root) = temp_staging();
    let incoming = incoming_stage(&s, 7, commit);
    crate::manifest::quarantine_artifact(&s, 7, &ready.dmg_sha256, aterm_durable::Flush::Durable);
    let refused = publish_verified_stage(&s, &incoming, &ready);
    assert!(
        refused
            .as_ref()
            .is_err_and(|error| error.contains("quarantined")),
        "{refused:?}"
    );
    assert!(Ready::read_publishable(&s).is_none());
    std::fs::rename(&incoming, &s.staged_app).unwrap();
    std::fs::write(&s.ready, ready.to_toml().unwrap()).unwrap();
    assert!(
        Ready::read_publishable(&s).is_none(),
        "a quarantined stage is not publishable"
    );
    let other = Ready {
        dmg_sha256: "cd".repeat(32),
        ..ready.clone()
    };
    std::fs::write(&s.ready, other.to_toml().unwrap()).unwrap();
    assert!(Ready::read_publishable(&s).is_some());
    let _ = std::fs::remove_dir_all(root);

    let (s, root) = temp_staging();
    let incoming = incoming_stage(&s, 7, commit);
    let revoked = vec!["m11".to_string()];
    crate::manifest::Floor::observe_and_write(
        &s.floor(),
        &crate::manifest::FloorObservation {
            roster_seq: 10,
            revoked: &revoked,
            ..crate::manifest::FloorObservation::default()
        },
    );
    let refused = publish_verified_stage(&s, &incoming, &ready);
    assert!(
        refused
            .as_ref()
            .is_err_and(|error| error.contains("m11") && error.contains("revoked")),
        "{refused:?}"
    );
    assert!(
        Ready::read_published_bundle(&s).is_none(),
        "nothing published"
    );
    crate::manifest::Floor::observe_and_write(
        &s.floor(),
        &crate::manifest::FloorObservation {
            roster_seq: 11,
            ..crate::manifest::FloorObservation::default()
        },
    );
    let by_m3 = Ready {
        machine_id: Some("m3".into()),
        ..ready
    };
    let recording = aterm_durable::seam::Recording::start();
    publish_verified_stage(&s, &incoming, &by_m3).expect("m3 is still trusted");
    assert_eq!(
        recording.take(),
        [aterm_durable::Flush::Ordered, aterm_durable::Flush::Ordered],
        "the ready marker, the stage's commit point, is only ordered"
    );
    assert!(Ready::read_publishable(&s).is_some());
    let _ = std::fs::remove_dir_all(root);
}

/// Build `build`, signed by `machine` under roster generation 9, published as a
fn publish_confirmed_stage(
    s: &Staging,
    build: u64,
    machine: &str,
    confirmed: Option<u64>,
) -> Ready {
    let commit = "0123456789abcdef0123456789abcdef01234567";
    let ready = Ready {
        build_number: build,
        version: format!("0.0.{build}"),
        commit: Some(commit.into()),
        dmg_sha256: "ab".repeat(32),
        team_id: "T".into(),
        staged_at: String::new(),
        changelog: None,
        machine_id: Some(machine.into()),
        roster_seq: Some(9),
        roster_confirmed_at: confirmed.map(aterm_types::rfc3339::format_rfc3339),
    };
    std::fs::write(&s.ready, ready.to_toml().unwrap()).unwrap();
    let contents = s.staged_app.join("Contents");
    std::fs::create_dir_all(&contents).unwrap();
    std::fs::write(
        contents.join("Info.plist"),
        format!(
            "<plist><dict><key>CFBundleVersion</key><string>{build}</string>\
             <key>ATermGitCommit</key><string>{commit}</string></dict></plist>"
        ),
    )
    .unwrap();
    assert!(ready.is_publishable(s), "PRECONDITION: a published stage");
    ready
}

/// A COLD LAUNCH DOES NOT SWAP IN A STAGE NO CHECK HAS CONFIRMED FOR A DAY. Build
#[test]
fn a_cold_launch_waits_for_a_check_to_confirm_a_stale_roster_authorization() {
    let (s, root) = temp_staging();
    let now = unix_now_secs();
    let day = crate::manifest::ROSTER_FRESHNESS_SECS;
    crate::manifest::Floor::observe_and_write(
        &s.floor(),
        &crate::manifest::FloorObservation {
            roster_seq: 9,
            ..crate::manifest::FloorObservation::default()
        },
    );
    for confirmed in [Some(now - 2 * day), None] {
        let ready = publish_confirmed_stage(&s, 20, "m11", confirmed);
        match stage_admission_gates(&s, 10, &ready, None, now) {
            Some(ApplyOutcome::Deferred(why)) => {
                assert!(crate::is_passing_refusal(&why), "{why}");
                assert!(
                    why.contains(crate::manifest::ROSTER_UNCONFIRMED_KEY),
                    "{why}"
                );
            }
            other => panic!("{confirmed:?}: a stale stage waits for a check: {other:?}"),
        }
        assert!(
            Ready::read_publishable(&s).is_some(),
            "a wait keeps the stage for the check to confirm"
        );
        assert!(
            crate::manifest::FailedMark::read(&s.failed()).is_none(),
            "nothing is memoized against the build"
        );
    }
    let stale = publish_confirmed_stage(&s, 20, "m11", Some(now - 2 * day));
    let revoked = vec!["m11".to_string()];
    crate::manifest::Floor::observe_and_write(
        &s.floor(),
        &crate::manifest::FloorObservation {
            roster_seq: 10,
            revoked: &revoked,
            ..crate::manifest::FloorObservation::default()
        },
    );
    assert_eq!(
        stage_admission_gates(&s, 10, &stale, None, now),
        Some(ApplyOutcome::NoUpdate)
    );
    assert!(!s.ready.exists(), "the revoked stage is retired");
    let _ = std::fs::remove_dir_all(root);
}

/// A launch waiting for its roster confirmation is no stuck update: booking the wait
#[test]
fn a_launch_waiting_for_its_roster_confirmation_restarts_the_overdue_clock() {
    let (s, root) = temp_staging();
    let now = unix_now_secs();
    let stale = publish_confirmed_stage(
        &s,
        20,
        "m3",
        Some(now - 2 * crate::manifest::ROSTER_FRESHNESS_SECS),
    );
    let long_ago = "2026-09-01T00:00:00Z";
    let seen_long_ago = || {
        std::fs::write(
            s.health(),
            format!("pending_build = 20\npending_since = \"{long_ago}\"\n"),
        )
        .unwrap();
    };
    seen_long_ago();
    record_boot_apply_refusal(&s, 10, "install location not writable");
    assert_eq!(
        crate::health::Health::read(&s.health()).pending_since,
        long_ago,
        "another refusal is the lane's own, and its clock runs on"
    );
    seen_long_ago();
    let Some(ApplyOutcome::Deferred(wait)) = stage_admission_gates(&s, 10, &stale, None, now)
    else {
        panic!("PRECONDITION: the launch waits");
    };
    record_boot_apply_refusal(&s, 10, &wait);
    let ledger = crate::health::Health::read(&s.health());
    assert_eq!(
        ledger.last_apply_refusal, wait,
        "the wait is booked and named"
    );
    assert_eq!(ledger.pending_build, 20);
    assert_ne!(ledger.pending_since, long_ago, "and its hour starts now");
    let _ = std::fs::remove_dir_all(root);
}

/// The in-session lane waits too: the full pre-verification and the cached pass both
#[test]
fn the_pre_park_checks_refuse_a_stage_whose_roster_confirmation_is_stale() {
    let (s, root) = temp_staging();
    let now = unix_now_secs();
    let commit = "0123456789abcdef0123456789abcdef01234567";
    publish_confirmed_stage(
        &s,
        20,
        "m3",
        Some(now - 2 * crate::manifest::ROSTER_FRESHNESS_SECS),
    );
    let full =
        preverify_staged_handoff_candidate_at(&s, None, 10, None, Some(20), None, NEVER_READ)
            .unwrap_err();
    assert!(
        full.contains(crate::manifest::ROSTER_UNCONFIRMED_KEY) && crate::is_passing_refusal(&full),
        "{full}"
    );
    let cached = handoff_cached_pass_refusal_at(&s, 20, commit).expect("a stale stage waits");
    assert!(
        cached.contains(crate::manifest::ROSTER_UNCONFIRMED_KEY)
            && crate::is_passing_refusal(&cached),
        "{cached}"
    );
    publish_confirmed_stage(&s, 20, "m3", Some(now - 3_600));
    assert_eq!(handoff_cached_pass_refusal_at(&s, 20, commit), None);
    let _ = std::fs::remove_dir_all(root);
}

/// A check's confirmation lands on the stage it judged and ends the wait it answers:
#[test]
fn a_confirmation_stamps_the_judged_stage_and_withdraws_only_the_wait_it_answers() {
    let (s, root) = temp_staging();
    let now = unix_now_secs();
    let stale = publish_confirmed_stage(
        &s,
        20,
        "m3",
        Some(now - 2 * crate::manifest::ROSTER_FRESHNESS_SECS),
    );
    let wait = stale
        .roster_authorization_stale(now, true)
        .expect("PRECONDITION: stale");
    let long_ago = "2026-09-01T00:00:00Z";
    std::fs::write(
        s.health(),
        format!("pending_build = 20\npending_since = \"{long_ago}\"\n"),
    )
    .unwrap();
    crate::health::Health::record_apply_refusal(&s.health(), 10, &wait);
    let stamp = aterm_types::rfc3339::format_rfc3339(now);
    let recording = aterm_durable::seam::Recording::start();
    confirm_roster_authorization(&s, &stale, &stamp).unwrap();
    assert_eq!(
        recording.take(),
        [aterm_durable::Flush::Ordered, aterm_durable::Flush::Ordered],
        "the stamp's rewrite of the marker, file and entry, is only ordered: an older \
         stamp makes the stage wait for confirmation again"
    );
    drop(recording);
    let confirmed = Ready::read_publishable(&s).expect("still published");
    assert_eq!(
        confirmed.roster_confirmed_at.as_deref(),
        Some(stamp.as_str())
    );
    assert_eq!(confirmed.roster_authorization_stale(now, true), None);
    let ledger = crate::health::Health::read(&s.health());
    assert!(
        ledger.last_apply_refusal.is_empty(),
        "the wait it answered is withdrawn"
    );
    assert_eq!(ledger.pending_build, 20);
    assert_ne!(
        ledger.pending_since, long_ago,
        "installable only now: the overdue hour counts from here, not from the days \
         the app was closed"
    );
    std::fs::write(
        s.health(),
        format!("pending_build = 20\npending_since = \"{long_ago}\"\n"),
    )
    .unwrap();
    confirm_roster_authorization(&s, &confirmed, &stamp).unwrap();
    assert_eq!(
        crate::health::Health::read(&s.health()).pending_since,
        long_ago
    );

    crate::health::Health::record_apply_refusal(&s.health(), 10, "install location not writable");
    confirm_roster_authorization(&s, &confirmed, &stamp).unwrap();
    assert_eq!(
        crate::health::Health::read(&s.health()).last_apply_refusal,
        "install location not writable"
    );

    let replaced = publish_confirmed_stage(&s, 21, "m3", None);
    confirm_roster_authorization(&s, &stale, &stamp).unwrap();
    assert_eq!(
        Ready::read(&s.ready).unwrap().roster_confirmed_at,
        replaced.roster_confirmed_at,
        "build 21 waits for its own check"
    );
    assert!(confirm_roster_authorization(&s, &replaced, "").is_err());
    let _ = std::fs::remove_dir_all(root);
}

/// A confirmation is dated by the roster's read and never moves backwards: a stage is
#[test]
fn a_confirmation_is_dated_by_the_roster_read_and_never_moves_backwards() {
    let (s, root) = temp_staging();
    let now = unix_now_secs();
    let at = aterm_types::rfc3339::format_rfc3339;
    let stale = publish_confirmed_stage(
        &s,
        20,
        "m3",
        Some(now - 2 * crate::manifest::ROSTER_FRESHNESS_SECS),
    );
    let stamp = || Ready::read(&s.ready).unwrap().roster_confirmed_at;

    let read = at(now - 5 * 3_600);
    confirm_roster_authorization(&s, &stale, &read).unwrap();
    assert_eq!(stamp().as_deref(), Some(read.as_str()));
    confirm_roster_authorization(&s, &stale, &at(now - 6 * 3_600)).unwrap();
    assert_eq!(stamp().as_deref(), Some(read.as_str()), "never backwards");
    let newer = at(now - 60);
    confirm_roster_authorization(&s, &stale, &newer).unwrap();
    assert_eq!(stamp().as_deref(), Some(newer.as_str()));
    let mut ahead = Ready::read(&s.ready).unwrap();
    ahead.roster_confirmed_at = Some(at(now + 3_600));
    std::fs::write(&s.ready, ahead.to_toml().unwrap()).unwrap();
    confirm_roster_authorization(&s, &stale, &read).unwrap();
    assert_eq!(stamp().as_deref(), Some(read.as_str()));

    let manifest = Manifest {
        schema: 1,
        version: "0.0.20".into(),
        build_number: 20,
        commit: Some("0123456789abcdef0123456789abcdef01234567".into()),
        sha256: "AB".repeat(32),
        dmg: "aterm-0.0.20.dmg".into(),
        url: None,
        zip: None,
        zip_sha256: None,
        min_build: None,
        machine_id: Some("m3".into()),
        roster_seq: Some(9),
        changelog: None,
    };
    let published = at(now);
    let read_then = at(now - 5 * 3_600);
    let marker = staged_ready_marker(
        &manifest,
        "0123456789abcdef0123456789abcdef01234567",
        "T".into(),
        &published,
        &read_then,
    );
    assert_eq!(marker.staged_at, published);
    assert_eq!(
        marker.roster_confirmed_at.as_deref(),
        Some(read_then.as_str())
    );
    assert_eq!(
        (marker.machine_id.as_deref(), marker.roster_seq),
        (Some("m3"), Some(9))
    );
    assert_eq!(marker.dmg_sha256, "ab".repeat(32));
    assert!(
        marker
            .roster_authorization_stale(
                now - 5 * 3_600 + crate::manifest::ROSTER_FRESHNESS_SECS,
                true
            )
            .is_some()
    );
    let unread = staged_ready_marker(
        &manifest,
        "0123456789abcdef0123456789abcdef01234567",
        "T".into(),
        &published,
        "",
    );
    assert_eq!(unread.roster_confirmed_at, None);
    let _ = std::fs::remove_dir_all(root);
}

/// A FLOOR THAT IS THERE BUT COULD NOT BE READ WITHDRAWS NOTHING AND ADMITS
#[test]
fn a_floor_that_could_not_be_read_admits_no_stage() {
    use std::os::unix::fs::PermissionsExt;
    let (s, root) = temp_staging();
    let commit = "0123456789abcdef0123456789abcdef01234567";
    let ready = Ready {
        build_number: 20,
        version: "0.0.20".into(),
        commit: Some(commit.into()),
        dmg_sha256: "ab".repeat(32),
        team_id: "T".into(),
        staged_at: String::new(),
        changelog: None,
        machine_id: Some("m11".into()),
        roster_seq: Some(9),
        roster_confirmed_at: Some(crate::install::now_rfc3339()),
    };
    std::fs::write(&s.ready, ready.to_toml().unwrap()).unwrap();
    let incoming = incoming_stage(&s, 20, commit);
    std::fs::write(s.floor(), "roster_seq = 10\nrevoked_machines = [\"m11\"]\n").unwrap();
    std::fs::set_permissions(s.floor(), std::fs::Permissions::from_mode(0o000)).unwrap();

    let pre_park = handoff_apply_floor_refusal_at(&s, 20);
    let swap = floor_withdrawal_gate(&s, 10, &ready);
    let publish = publish_verified_stage(&s, &incoming, &ready);
    std::fs::set_permissions(s.floor(), std::fs::Permissions::from_mode(0o600)).unwrap();

    assert!(
        pre_park
            .as_deref()
            .is_some_and(|why| crate::is_passing_refusal(why) && why.contains("floor")),
        "{pre_park:?}"
    );
    assert!(
        matches!(&swap, Some(ApplyOutcome::Deferred(why)) if crate::is_passing_refusal(why)),
        "{swap:?}"
    );
    assert!(
        s.ready.exists(),
        "a floor it could not read retires nothing"
    );
    assert!(
        publish
            .as_ref()
            .is_err_and(|why| crate::is_passing_refusal(why) && why.contains("floor")),
        "{publish:?}"
    );
    assert!(incoming.exists(), "nothing published");

    let refusal = handoff_apply_floor_refusal_at(&s, 20).expect("revoked signer");
    assert!(!crate::is_passing_refusal(&refusal), "{refusal}");
    assert!(matches!(
        floor_withdrawal_gate(&s, 10, &ready),
        Some(ApplyOutcome::NoUpdate)
    ));
    assert!(!s.ready.exists(), "the readable floor retires the stage");

    for (min_build, withdrawn) in [(21, true), (20, false)] {
        std::fs::write(&s.ready, ready.to_toml().unwrap()).unwrap();
        std::fs::write(s.floor(), format!("min_build = {min_build}\n")).unwrap();
        assert_eq!(
            matches!(
                floor_withdrawal_gate(&s, 10, &ready),
                Some(ApplyOutcome::NoUpdate)
            ),
            withdrawn,
            "min_build {min_build}"
        );
        assert_eq!(s.ready.exists(), !withdrawn, "min_build {min_build}");
    }
    let _ = std::fs::remove_dir_all(root);
}

/// A sibling's hold of the apply lock is a moment, not a verdict: a publish that
#[test]
fn a_publish_that_outwaits_a_sibling_s_apply_lock_is_not_booked_against_the_build() {
    let (s, root) = temp_staging();
    write_ready(&s, 5);
    make_app(&s.staged_app, "OLD-STAGE");
    let incoming = s.staged_dir().join("aterm.app.incoming");
    make_app(&incoming, "NEW-STAGE");
    let mut ready = Ready::read(&s.ready).unwrap();
    ready.build_number = 6;

    let holder = FileLock::acquire(&s.apply_lock).expect("a sibling holds the apply lock");
    let error =
        publish_verified_stage_within(&s, &incoming, &ready, std::time::Duration::from_millis(40))
            .expect_err("a held apply lock refuses the bounded publish");
    drop(holder);
    assert!(
        crate::is_passing_refusal(&error),
        "a sibling's hold is a moment: {error}"
    );
    assert!(!stage_failure_is_a_verdict(&error), "{error}");
    assert_eq!(read_id(&s.staged_app), "OLD-STAGE");
    assert_eq!(read_id(&incoming), "NEW-STAGE");

    publish_verified_stage_within(&s, &incoming, &ready, std::time::Duration::from_secs(60))
        .expect("the lock is free");
    assert_eq!(read_id(&s.staged_app), "NEW-STAGE");
    let _ = std::fs::remove_dir_all(root);
}

/// A marker that would read as absent is never committed, and refusing it keeps the
#[test]
fn publish_refuses_a_marker_that_would_exceed_the_ledger_cap() {
    let (s, root) = temp_staging();
    write_ready(&s, 5);
    make_app(&s.staged_app, "OLD-STAGE");
    let incoming = s.staged_dir().join("aterm.app.incoming");
    make_app(&incoming, "NEW-STAGE");

    let mut ready = Ready::read(&s.ready).unwrap();
    ready.build_number = 6;
    ready.changelog = Some("y".repeat(usize::try_from(crate::MAX_LEDGER_BYTES).unwrap() + 1));
    publish_verified_stage(&s, &incoming, &ready).unwrap_err();

    assert!(
        matches!(
            read_ready(&s, 4),
            ReadyState::Newer(Ready {
                build_number: 5,
                ..
            })
        ),
        "a rejected publish leaves the previous readable generation staged"
    );
    assert_eq!(read_id(&s.staged_app), "OLD-STAGE");
    assert_eq!(read_id(&incoming), "NEW-STAGE");

    use std::os::unix::fs::MetadataExt as _;
    let retained = StageIncoming::create(&s).unwrap();
    let retained_path = retained.path.clone();
    make_app(&retained_path, "RETAINED-UNKNOWN");
    let identity = |path: &Path| {
        let metadata = std::fs::metadata(path).unwrap();
        (metadata.dev(), metadata.ino())
    };
    let retained_dir = identity(&retained_path);
    let retained_file = identity(&retained_path.join("id"));
    let unwound = std::panic::catch_unwind(move || {
        let _retained = retained;
        panic!("captured stage unwind");
    });
    assert!(unwound.is_err());

    let retry = StageIncoming::create(&s).unwrap();
    assert_ne!(retry.path, retained_path);
    assert_ne!(retry.path, incoming, "the legacy fixed path is not reused");
    assert_eq!(read_id(&retained_path), "RETAINED-UNKNOWN");
    assert_eq!(read_id(&incoming), "NEW-STAGE");
    use std::os::unix::process::ExitStatusExt as _;
    copy_into_isolated_destination(
        &[retry.path.clone().into_os_string()],
        "incoming retry fixture",
        |args, attempt| {
            assert_eq!(attempt.parent(), Some(s.staged_dir().as_path()));
            make_app(Path::new(args.last().unwrap()), "RETRY");
            Ok((std::process::ExitStatus::from_raw(0), String::new()))
        },
    )
    .unwrap();
    ready.changelog = None;
    publish_verified_stage(&s, &retry.path, &ready).unwrap();
    let retry_path = retry.path.clone();
    drop(retry);
    assert!(
        !retry_path.exists(),
        "the published attempt left no reservation"
    );
    assert_eq!(read_id(&s.staged_app), "RETRY");
    assert_eq!(read_id(&incoming), "NEW-STAGE");
    assert_eq!(read_id(&retained_path), "RETAINED-UNKNOWN");
    assert_eq!(identity(&retained_path), retained_dir);
    assert_eq!(identity(&retained_path.join("id")), retained_file);

    let empty = StageIncoming::create(&s).unwrap();
    let empty_path = empty.path.clone();
    drop(empty);
    assert!(
        !empty_path.exists(),
        "an unused empty reservation is reclaimed"
    );
    let _ = std::fs::remove_dir_all(root);
}

#[test]
fn read_ready_classifies_all_states() {
    let (s, root) = temp_staging();

    assert!(matches!(read_ready(&s, 10), ReadyState::Absent));

    std::fs::write(&s.ready, "this is not valid toml {{{").unwrap();
    assert!(matches!(read_ready(&s, 10), ReadyState::Corrupt));

    write_ready(&s, 20);
    assert!(matches!(read_ready(&s, 10), ReadyState::Newer(_)));

    assert!(matches!(read_ready(&s, 20), ReadyState::NotNewer));
    assert!(matches!(read_ready(&s, 21), ReadyState::NotNewer));

    let mut corrupt = Ready::read(&s.ready).unwrap();
    corrupt.commit = Some("0123456789ab".into());
    std::fs::write(&s.ready, corrupt.to_toml().unwrap()).unwrap();
    assert!(matches!(read_ready(&s, 10), ReadyState::Corrupt));

    let _ = std::fs::remove_dir_all(root);
}

/// THE SHARED STAGE'S STATE IS A MOMENT, NOT A VERDICT ON THE BUILD. A sibling
#[test]
fn a_stage_state_refusal_is_passing_and_a_verdict_is_not() {
    let (s, root) = temp_staging();

    let absent = preverify_staged_handoff_candidate_at(&s, None, 6, None, Some(7), None, &|_| ())
        .unwrap_err();
    assert!(crate::is_passing_refusal(&absent), "{absent}");

    write_ready(&s, 20);
    let not_newer =
        preverify_staged_handoff_candidate_at(&s, None, 20, None, None, None, NEVER_READ)
            .unwrap_err();
    assert!(crate::is_passing_refusal(&not_newer), "{not_newer}");

    let replaced =
        preverify_staged_handoff_candidate_at(&s, None, 10, None, Some(19), None, NEVER_READ)
            .unwrap_err();
    assert!(
        replaced.contains("not the authorized build") && crate::is_passing_refusal(&replaced),
        "{replaced}"
    );

    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(&s.ready, std::fs::Permissions::from_mode(0o000)).unwrap();
        let observable = std::fs::read(&s.ready).is_err();
        let unreadable =
            preverify_staged_handoff_candidate_at(&s, None, 10, None, Some(20), None, NEVER_READ)
                .unwrap_err();
        std::fs::set_permissions(&s.ready, std::fs::Permissions::from_mode(0o600)).unwrap();
        if observable {
            assert!(
                unreadable.contains("unreadable") && crate::is_passing_refusal(&unreadable),
                "{unreadable}"
            );
        }
    }

    let bundle_gone =
        preverify_staged_handoff_candidate_at(&s, None, 10, None, Some(20), None, NEVER_READ)
            .unwrap_err();
    assert!(
        bundle_gone.contains("no longer the published stage")
            && crate::is_passing_refusal(&bundle_gone),
        "{bundle_gone}"
    );

    std::fs::write(&s.staged_app, b"not a bundle").unwrap();
    let a_file =
        preverify_staged_handoff_candidate_at(&s, None, 10, None, Some(20), None, NEVER_READ)
            .unwrap_err();
    assert!(!crate::is_passing_refusal(&a_file), "{a_file}");
    std::fs::remove_file(&s.staged_app).unwrap();
    std::os::unix::fs::symlink(&root, &s.staged_app).unwrap();
    let symlinked =
        preverify_staged_handoff_candidate_at(&s, None, 10, None, Some(20), None, NEVER_READ)
            .unwrap_err();
    assert!(!crate::is_passing_refusal(&symlinked), "{symlinked}");
    std::fs::remove_file(&s.staged_app).unwrap();
    std::fs::write(s.floor(), "min_build = 21\n").unwrap();
    let yanked =
        preverify_staged_handoff_candidate_at(&s, None, 10, None, Some(20), None, NEVER_READ)
            .unwrap_err();
    assert!(
        yanked.contains("floor") && !crate::is_passing_refusal(&yanked),
        "{yanked}"
    );
    std::fs::remove_file(s.floor()).unwrap();
    crate::manifest::quarantine_artifact(&s, 20, &"ab".repeat(32), aterm_durable::Flush::Durable);
    let quarantined =
        preverify_staged_handoff_candidate_at(&s, None, 10, None, Some(20), None, NEVER_READ)
            .unwrap_err();
    assert!(
        quarantined.contains("quarantined") && !crate::is_passing_refusal(&quarantined),
        "{quarantined}"
    );

    let _ = std::fs::remove_dir_all(root);
}

/// The floor check a worker runs alone, when a fresh cached pass let it skip the full
#[test]
fn the_floor_check_alone_refuses_only_below_the_floor() {
    let (s, root) = temp_staging();
    assert_eq!(
        handoff_apply_floor_refusal_at(&s, 20),
        None,
        "no floor written"
    );
    std::fs::write(s.floor(), "min_build = 21\n").unwrap();
    handoff_apply_floor_refusal_at(&s, 20).expect("20 is below 21");
    assert_eq!(handoff_apply_floor_refusal_at(&s, 21), None, "at the floor");
    assert_eq!(
        handoff_apply_floor_refusal_at(&s, 22),
        None,
        "above the floor"
    );
    let _ = std::fs::remove_dir_all(root);
}

/// A check of the installed bundle that did not finish is not a person's to
#[test]
fn a_timeout_verifying_the_installed_bundle_is_not_a_person_refusal() {
    let root = std::env::temp_dir().join(format!(
        "aterm-update-installed-timeout-{}-{}",
        std::process::id(),
        unix_now_secs()
    ));
    let app = root.join("aterm.app");
    let _ = unsigned_bundle_with_a_policy(&app, 10);
    let commit = Some("0123456789abcdef0123456789abcdef01234567");

    let slow_machine = |_installed: &Path| -> Result<(u64, String), String> {
        Err(format!(
            "bundle policy: {}",
            crate::verify::timed_out("codesign --verify (team-pinned)", true)
        ))
    };
    let refusal = preverify_installed_rollback_source_with(Some(&app), 10, commit, &slow_machine)
        .expect_err("a check that did not finish must refuse, never pass");
    assert!(
        crate::is_passing_refusal(&refusal),
        "a verification that ran out of the apply budget is a passing refusal: {refusal}"
    );
    assert!(
        !is_rollback_source_refusal(&refusal),
        "a check that did not finish must not read as the installed-source refusal: {refusal}"
    );
    assert!(
        !crate::refusal_needs_person(&refusal),
        "nothing about the bundle is known, so nothing is asked of a person: {refusal}"
    );
    assert!(
        refusal.contains(&app.display().to_string()),
        "the refusal still names the bundle it was about: {refusal}"
    );
    let wrapped = rollback_source_refusal(&app, &refusal);
    assert!(is_rollback_source_refusal(&wrapped));
    assert!(!crate::refusal_needs_person(&wrapped), "{wrapped}");
    let _ = std::fs::remove_dir_all(root);
}

/// A CODESIGN RUN THAT ENDED WITHOUT A VERDICT IS NOT ONE,
#[test]
fn a_codesign_run_without_a_verdict_on_the_installed_bundle_is_not_a_person_refusal() {
    use std::os::unix::fs::PermissionsExt as _;
    let root = std::env::temp_dir().join(format!(
        "aterm-update-installed-unreadable-{}-{}",
        std::process::id(),
        SEQ.fetch_add(1, Ordering::Relaxed)
    ));
    let app = root.join("aterm.app");
    let _ = unsigned_bundle_with_a_policy(&app, 10);
    let commit = Some("0123456789abcdef0123456789abcdef01234567");
    let exe = app.join("Contents/MacOS/aterm");

    std::fs::set_permissions(&exe, std::fs::Permissions::from_mode(0o000)).unwrap();
    let premise = std::fs::read(&exe).is_err();
    let unread =
        preverify_installed_rollback_source_with(Some(&app), 10, commit, &verified_bundle_identity);
    std::fs::set_permissions(&exe, std::fs::Permissions::from_mode(0o755)).unwrap();
    let verdict =
        preverify_installed_rollback_source_with(Some(&app), 10, commit, &verified_bundle_identity);
    let _ = std::fs::remove_dir_all(&root);

    if premise {
        let refusal = unread.expect_err("a check that did not pass must refuse");
        assert!(crate::is_passing_refusal(&refusal), "{refusal}");
        assert!(!is_rollback_source_refusal(&refusal), "{refusal}");
        assert!(
            !crate::refusal_needs_person(&refusal),
            "codesign could not read the bundle, so nothing is asked of a person: {refusal}"
        );
    }
    let verdict = verdict.expect_err("an unsigned installed bundle is refused");
    assert!(!crate::is_passing_refusal(&verdict), "{verdict}");
    assert!(is_rollback_source_refusal(&verdict), "{verdict}");
    assert!(crate::refusal_needs_person(&verdict), "{verdict}");
}

/// A bundle modified after signing is a verdict, through the real reader and
#[test]
fn a_bundle_modified_after_signing_is_a_verdict_not_a_moment() {
    let root = std::env::temp_dir().join(format!(
        "aterm-update-tampered-{}-{}",
        std::process::id(),
        SEQ.fetch_add(1, Ordering::Relaxed)
    ));
    let signed = root.join("signed").join("aterm.app");
    let _ = unsigned_bundle_with_a_policy(&signed, 10);
    let sign = std::process::Command::new("/usr/bin/codesign")
        .args(["--force", "--sign", "-"])
        .arg(&signed)
        .output()
        .expect("codesign runs");
    assert!(
        sign.status.success(),
        "ad hoc signing: {}",
        String::from_utf8_lossy(&sign.stderr)
    );
    const COMMIT: &str = "0123456789abcdef0123456789abcdef01234567";
    type Tamper = (&'static str, fn(&Path));
    let tamper: [Tamper; 3] = [
        ("a sealed resource is missing or invalid", |app: &Path| {
            let policy = app.join(aterm_update_core::handoff_policy::BUNDLE_PATH);
            let mut text = std::fs::read_to_string(&policy).unwrap();
            text.push_str("carry = \"none\"\n");
            std::fs::write(policy, text).unwrap();
        }),
        ("invalid Info.plist", |app: &Path| {
            let plist = app.join("Contents/Info.plist");
            let mut text = std::fs::read_to_string(&plist).unwrap();
            text.push('\n');
            std::fs::write(plist, text).unwrap();
        }),
        (
            "unsealed contents present in the bundle root",
            |app: &Path| {
                std::fs::write(app.join("stray"), b"x").unwrap();
            },
        ),
    ];
    let pinned = !crate::effective_team_id().is_empty();
    let control = verified_bundle_identity(&signed);
    if pinned {
        let control = control.expect_err("an ad hoc signer is not the pinned team");
        assert!(!crate::is_passing_refusal(&control), "{control}");
    } else {
        assert_eq!(
            control,
            Ok((10, COMMIT.to_string())),
            "control: the bundle as signed passes"
        );
    }
    for (case, (words, modify)) in tamper.iter().enumerate() {
        let app = root.join(format!("tampered-{case}")).join("aterm.app");
        std::fs::create_dir_all(app.parent().unwrap()).unwrap();
        let copied = std::process::Command::new("/usr/bin/ditto")
            .arg(&signed)
            .arg(&app)
            .status()
            .expect("ditto runs");
        assert!(copied.success());
        modify(&app);

        let stage = verified_bundle_identity(&app).expect_err("a tampered bundle refuses");
        assert!(
            stage_failure_is_a_verdict(&stage),
            "{words}: a tampered stage is discarded, not kept: {stage}"
        );
        let installed = preverify_installed_rollback_source_with(
            Some(&app),
            10,
            Some(COMMIT),
            &verified_bundle_identity,
        )
        .expect_err("a tampered installed bundle refuses");
        assert!(
            is_rollback_source_refusal(&installed),
            "{words}: a tampered installed bundle is the person's to reinstall: {installed}"
        );
        assert!(!crate::is_passing_refusal(&installed), "{installed}");
    }
    let _ = std::fs::remove_dir_all(&root);
}

/// The pre-park check refuses a non-writable install location before the park, naming
#[test]
fn a_non_writable_install_location_is_refused_before_the_park_as_a_person_s() {
    use std::os::unix::fs::PermissionsExt;
    const COMMIT: &str = "0123456789abcdef0123456789abcdef01234567";
    let root = std::env::temp_dir().join(format!(
        "aterm-update-ro-install-{}-{}",
        std::process::id(),
        SEQ.fetch_add(1, Ordering::Relaxed)
    ));
    let ro = root.join("ro");
    let app = ro.join("aterm.app");
    std::fs::create_dir_all(&app).unwrap();
    let identity = |_: &Path| -> Result<(u64, String), String> { Ok((7, COMMIT.to_string())) };
    assert_eq!(
        preverify_installed_rollback_source_with(Some(&app), 7, Some(COMMIT), &identity),
        Ok(()),
        "control: a writable install location passes"
    );
    std::fs::set_permissions(&ro, std::fs::Permissions::from_mode(0o555)).unwrap();
    let refused = preverify_installed_rollback_source_with(Some(&app), 7, Some(COMMIT), &identity);
    std::fs::set_permissions(&ro, std::fs::Permissions::from_mode(0o755)).unwrap();
    let _ = std::fs::remove_dir_all(&root);
    let refusal = refused.expect_err("a folder this user cannot write is refused");
    assert!(crate::is_install_location_refusal(&refusal), "{refusal}");
    assert!(
        refusal.contains(&app.display().to_string()),
        "names the install: {refusal}"
    );
    assert!(!crate::is_passing_refusal(&refusal), "{refusal}");
    assert!(
        crate::refusal_needs_person(&refusal),
        "only a person can move the app or open the folder: {refusal}"
    );
}

/// A write that failed for a moment (a full disk) is a passing refusal on every
#[test]
fn a_full_disk_at_the_install_location_is_a_passing_refusal_not_the_person_s() {
    let app = Path::new("/Applications/aterm.app");
    let probe = bundle::classify_probe_error(
        Path::new("/Applications"),
        &std::io::Error::from_raw_os_error(libc::ENOSPC),
    );
    let refusal = install_location_verdict(app, probe).expect_err("a full disk refuses");
    assert!(crate::is_passing_refusal(&refusal), "{refusal}");
    assert!(!crate::is_install_location_refusal(&refusal), "{refusal}");
    assert!(!crate::refusal_needs_person(&refusal), "{refusal}");
    let denied = install_location_verdict(
        app,
        bundle::classify_probe_error(
            Path::new("/Applications"),
            &std::io::Error::from_raw_os_error(libc::EACCES),
        ),
    )
    .expect_err("a denied write refuses");
    assert!(crate::refusal_needs_person(&denied), "{denied}");
}

/// The handoff-policy reader for a pre-verification that must refuse: any read is
const NEVER_READ: &dyn Fn(&Path) -> aterm_update_core::handoff_policy::PolicyRead = &|path| {
    panic!(
        "a refused candidate's handoff policy was read: {}",
        path.display()
    )
};

/// A staged `.app` shaped like a release (an `Info.plist` naming `build` and a
fn unsigned_bundle_with_a_policy(
    app: &Path,
    build: u64,
) -> aterm_update_core::handoff_policy::PolicyRead {
    let contents = app.join("Contents");
    std::fs::create_dir_all(contents.join("MacOS")).unwrap();
    std::fs::create_dir_all(contents.join("Resources")).unwrap();
    std::fs::write(
        contents.join("Info.plist"),
        format!(
            "<?xml version=\"1.0\" encoding=\"UTF-8\"?>\n<plist version=\"1.0\"><dict>\n\
             <key>CFBundleExecutable</key><string>aterm</string>\n\
             <key>CFBundleVersion</key><string>{build}</string>\n\
             <key>ATermGitCommit</key>\
             <string>0123456789abcdef0123456789abcdef01234567</string>\n\
             </dict></plist>\n"
        ),
    )
    .unwrap();
    std::fs::write(contents.join("MacOS/aterm"), b"#!/bin/sh\nexit 0\n").unwrap();
    std::fs::write(
        app.join(aterm_update_core::handoff_policy::BUNDLE_PATH),
        "schema = 1\ncarry = \"repaint\"\npark_quiet_gate_at_land = \"relaxed\"\n",
    )
    .unwrap();
    aterm_update_core::handoff_policy::read_from_bundle(app)
}

/// A handoff policy in an unverified bundle is never read, since its authority is the
#[test]
fn a_handoff_policy_in_an_unverified_bundle_is_never_read() {
    let (s, root) = temp_staging();
    write_ready(&s, 20);
    let held = unsigned_bundle_with_a_policy(&s.staged_app, 20);
    assert!(
        matches!(&held, aterm_update_core::handoff_policy::PolicyRead::Parsed(policy)
            if policy.legacy.carry == Some(aterm_update_core::handoff_policy::CarryCeiling::Repaint)),
        "PRECONDITION: the unsigned stage holds a policy a reader would follow: {held:?}"
    );
    let reads = std::cell::Cell::new(0_u32);
    let counting = |path: &Path| {
        reads.set(reads.get() + 1);
        aterm_update_core::handoff_policy::read_from_bundle(path)
    };
    let refused =
        preverify_staged_handoff_candidate_at(&s, None, 10, None, Some(20), None, &counting);
    assert!(
        refused
            .as_ref()
            .is_err_and(|why| why.contains("bundle policy")),
        "the unsigned stage fails its codesign policy: {refused:?}"
    );
    assert_eq!(reads.get(), 0, "the refused stage's policy was never read");

    let installed = root.join("Applications").join("aterm.app");
    let held = unsigned_bundle_with_a_policy(&installed, 20);
    assert!(
        matches!(
            held,
            aterm_update_core::handoff_policy::PolicyRead::Parsed(_)
        ),
        "{held:?}"
    );
    let refused = crate::preverify_installed_at(&installed, 0, 10, 20, "0123456789ab", &counting);
    assert!(
        refused
            .as_ref()
            .is_err_and(|why| why.contains("bundle policy")),
        "the unsigned installed bundle fails its codesign policy: {refused:?}"
    );
    assert_eq!(
        reads.get(),
        0,
        "the refused activation's policy was never read"
    );
    let _ = std::fs::remove_dir_all(root);
}

/// The pre-verification is timed as a whole: entering one holds the apply lock and
#[test]
fn a_pre_verification_holds_the_lock_and_runs_under_the_verification_budget() {
    let (s, root) = temp_staging();
    assert!(verify::current_budget_deadline().is_none());
    {
        let entered = std::time::Instant::now();
        let _guard = enter_preverify(Some(&s.apply_lock)).expect("an idle lock is taken");
        let deadline = verify::current_budget_deadline()
            .expect("every helper under a pre-verification answers inside the budget");
        assert!(
            deadline <= std::time::Instant::now() + verify::APPLY_BUDGET
                && deadline >= entered + verify::APPLY_BUDGET,
            "the budget is the apply budget, opened once the lock is held"
        );
        assert!(
            FileLock::acquire_within(&s.apply_lock, std::time::Duration::from_millis(50)).is_err(),
            "the apply lock is held for the whole check"
        );
    }
    assert!(
        verify::current_budget_deadline().is_none(),
        "the budget closes with the check"
    );
    let _free = FileLock::acquire_within(&s.apply_lock, aterm_spec::wait::MUST_HAPPEN)
        .expect("and the lock is let go");
    {
        let _guard = enter_preverify(None).expect("no lock to wait for");
        assert!(verify::current_budget_deadline().is_some());
    }
    let _ = std::fs::remove_dir_all(root);
}

#[test]
fn successful_swap_status_is_new_build_and_has_no_staged_claim() {
    let (s, root) = temp_staging();
    write_ready(&s, 54);
    let ready = Ready::read(&s.ready).unwrap();
    s.retire_published();
    record_activating_status(&s, &ready);

    let text = std::fs::read_to_string(&s.status).expect("activation status written");
    let value: aterm_toml::Value = aterm_toml::from_str(&text).expect("activation status parses");
    assert_eq!(
        value
            .get("current_build")
            .and_then(aterm_toml::Value::as_integer),
        Some(54)
    );
    assert!(
        value.get("staged_build").is_none(),
        "retired ready marker cannot leave a staged status field: {text}"
    );
    let _ = std::fs::remove_dir_all(root);
}

#[test]
fn release_and_ready_commits_bind_to_sealed_bundle_provenance() {
    let full = "0123456789abcdef0123456789abcdef01234567";
    assert_eq!(canonical_release_commit(Some(full)), Some(full.to_string()));
    assert!(sealed_commit_matches(Some(full), "0123456789ab"));

    for invalid in [
        None,
        Some(""),
        Some("unknown"),
        Some("0123456"),
        Some("0123456789ab"),
        Some("0123456789abcdef0123456789abcdef0123456"),
        Some("0123456789abcdef0123456789abcdef012345678"),
        Some("0123456789abcdef0123456789abcdef0123456789abcdef0123456789abcdef"),
        Some("0123456789ab-dirty"),
    ] {
        assert!(canonical_release_commit(invalid).is_none());
    }
    assert!(
        !sealed_commit_matches(Some(full), "fedcba9876543210fedcba9876543210fedcba98"),
        "negative control: same build/digest marker cannot authorize wrong sealed commit"
    );
}

#[test]
fn expected_handoff_authority_is_exact_build_commit_and_digest() {
    let ready = Ready {
        build_number: 54,
        version: "0.54".into(),
        commit: Some("0123456789abcdef0123456789abcdef01234567".into()),
        dmg_sha256: "ab".repeat(32),
        team_id: "T".into(),
        staged_at: String::new(),
        changelog: None,
        machine_id: None,
        roster_seq: None,
        roster_confirmed_at: Some(crate::install::now_rfc3339()),
    };
    let exact = ExpectedArtifact {
        build: 54,
        commit: "0123456789abcdef0123456789abcdef01234567".into(),
        dmg_sha256: "ab".repeat(32),
    };
    assert!(ready_matches_expected(&ready, &exact));
    let mut wrong_digest = exact.clone();
    wrong_digest.dmg_sha256 = "cd".repeat(32);
    assert!(!ready_matches_expected(&ready, &wrong_digest));
    let mut wrong_commit = exact.clone();
    wrong_commit.commit = "fedcba9876543210fedcba9876543210fedcba98".into();
    assert!(!ready_matches_expected(&ready, &wrong_commit));
    let mut wrong_build = exact.clone();
    wrong_build.build = 55;
    assert!(!ready_matches_expected(&ready, &wrong_build));

    assert_eq!(expected_artifact_refusal(&ready, &exact), None);
    let replaced = expected_artifact_refusal(&ready, &wrong_build).unwrap();
    assert!(crate::is_passing_refusal(&replaced), "{replaced}");
    for other_bytes in [&wrong_commit, &wrong_digest] {
        let refusal = expected_artifact_refusal(&ready, other_bytes).unwrap();
        assert!(!crate::is_passing_refusal(&refusal), "{refusal}");
    }

    let (s, root) = temp_staging();
    let staged = publish_confirmed_stage(&s, 54, "m3", Some(unix_now_secs()));
    let gates = |expected| stage_admission_gates(&s, 10, &staged, Some(expected), unix_now_secs());
    assert!(
        matches!(gates(&wrong_build), Some(ApplyOutcome::Deferred(why)) if crate::is_passing_refusal(&why)),
        "another build in the stage is a moment"
    );
    for other_bytes in [&wrong_commit, &wrong_digest] {
        assert!(
            matches!(gates(other_bytes), Some(ApplyOutcome::Deferred(why)) if !crate::is_passing_refusal(&why)),
            "{other_bytes:?}: other bytes of the build are a verdict"
        );
    }
    assert!(
        Ready::read_publishable(&s).is_some(),
        "a refused authority retires nothing"
    );
    assert_eq!(gates(&exact), None);
    let _ = std::fs::remove_dir_all(root);
}

/// A QUARANTINE RECORD THAT COULD NOT BE READ ADMITS NOTHING. Build 20 crash-looped
#[test]
fn a_quarantine_record_that_could_not_be_read_admits_no_stage() {
    use std::os::unix::fs::PermissionsExt;
    let (s, root) = temp_staging();
    let commit = "0123456789abcdef0123456789abcdef01234567";
    let digest = "ab".repeat(32);
    let mode = |path: &Path, bits| {
        std::fs::set_permissions(path, std::fs::Permissions::from_mode(bits)).unwrap();
    };
    for alone in [s.quarantine(), s.failed()] {
        let _ = std::fs::remove_file(s.quarantine());
        let _ = std::fs::remove_file(s.failed());
        let now = unix_now_secs();
        let ready = publish_confirmed_stage(&s, 20, "m3", Some(now));
        if alone == s.quarantine() {
            crate::manifest::quarantine_artifact(&s, 20, &digest, aterm_durable::Flush::Durable);
            crate::manifest::FailedMark::record_stage_failure(
                &s.failed(),
                21,
                &"cd".repeat(32),
                now,
            );
            assert!(
                crate::manifest::FailedMark::read(&s.failed()).is_some_and(|m| !m.is_quarantine()),
                "PRECONDITION: failed.toml no longer names the quarantine"
            );
        } else {
            crate::manifest::FailedMark::record_quarantine(
                &s.failed(),
                20,
                &digest,
                aterm_durable::Flush::Durable,
            );
            assert!(!s.quarantine().exists(), "PRECONDITION: failed.toml alone");
        }
        let at = alone.display().to_string();
        assert!(Ready::read_publishable(&s).is_none(), "{at}: PRECONDITION");
        let incoming = incoming_stage(&s, 20, commit);

        mode(&alone, 0o000);
        if std::fs::File::open(&alone).is_ok() {
            mode(&alone, 0o600);
            break;
        }
        let publishable = Ready::read_publishable(&s);
        let swap = stage_admission_gates(&s, 10, &ready, None, now);
        let cached = handoff_cached_pass_refusal_at(&s, 20, commit);
        let full =
            preverify_staged_handoff_candidate_at(&s, None, 10, None, Some(20), None, NEVER_READ);
        let publish = publish_verified_stage(&s, &incoming, &ready);
        mode(&alone, 0o600);

        let moment = |why: &str| crate::is_passing_refusal(why) && why.contains("update records");
        assert!(publishable.is_none(), "{at}: {publishable:?}");
        assert!(
            matches!(&swap, Some(ApplyOutcome::Deferred(why)) if moment(why)),
            "{at}: {swap:?}"
        );
        assert!(
            s.ready.exists() && s.staged_app.exists(),
            "{at}: a record it could not read retires nothing"
        );
        assert!(cached.as_deref().is_some_and(moment), "{at}: {cached:?}");
        assert!(
            full.as_ref().is_err_and(|why| moment(why)),
            "{at}: {full:?}"
        );
        assert!(
            publish.as_ref().is_err_and(|why| moment(why)),
            "{at}: {publish:?}"
        );
        assert!(incoming.exists(), "{at}: nothing published");

        let verdict = |why: &str| why.contains("quarantined") && !crate::is_passing_refusal(why);
        let cached = handoff_cached_pass_refusal_at(&s, 20, commit);
        assert!(cached.as_deref().is_some_and(verdict), "{at}: {cached:?}");
        let full =
            preverify_staged_handoff_candidate_at(&s, None, 10, None, Some(20), None, NEVER_READ);
        assert!(
            full.as_ref().is_err_and(|why| verdict(why)),
            "{at}: {full:?}"
        );
        let publish = publish_verified_stage(&s, &incoming, &ready);
        assert!(
            publish.as_ref().is_err_and(|why| verdict(why)),
            "{at}: {publish:?}"
        );
        assert_eq!(
            stage_admission_gates(&s, 10, &ready, None, now),
            Some(ApplyOutcome::NoUpdate),
            "{at}"
        );
        assert!(
            !s.ready.exists(),
            "{at}: the swap retires a quarantined stage"
        );
        let _ = std::fs::remove_dir_all(&incoming);
    }
    let _ = std::fs::remove_dir_all(root);
}

/// Rollback authority is the receipt of the exact sealed OLD build and commit, and a
#[test]
fn inverse_swap_receipt_restore_binds_old_identity_and_fails_closed() {
    let (staging, root) = temp_staging();
    let old_commit = "c16c6fd7955b0011223344556677889900aabbcc";
    let old_digest = "cd".repeat(32);
    crate::manifest::InstalledReceipt::record(
        &staging.installed_receipt(),
        52,
        old_commit,
        &old_digest,
        aterm_durable::Flush::Durable,
    )
    .unwrap();
    let previous = previous_receipt_for_sealed_old(&staging, 52, old_commit)
        .expect("exact sealed OLD receipt");
    assert!(
        previous_receipt_for_sealed_old(&staging, 52, &"f".repeat(40)).is_none(),
        "well-formed but stale local receipt is not rollback authority"
    );
    assert!(
        previous_receipt_for_sealed_old(&staging, 51, old_commit).is_none(),
        "receipt build must bind the sealed OLD build"
    );

    crate::manifest::InstalledReceipt::record(
        &staging.installed_receipt(),
        53,
        "0123456789abcdef0123456789abcdef01234567",
        &"ef".repeat(32),
        aterm_durable::Flush::Durable,
    )
    .unwrap();
    let blocked_tmp = staging
        .installed_receipt()
        .with_extension(format!("toml.{}.tmp", std::process::id()));
    std::fs::create_dir(&blocked_tmp).unwrap();
    restore_installed_receipt(&staging, Some(&previous), aterm_durable::Flush::Durable)
        .expect_err("blocked kind-preserving rewrite must surface failure");
    assert!(
        !staging.installed_receipt().exists(),
        "failed NEW receipt is cleared instead of surviving as OLD authority"
    );
    std::fs::remove_dir(&blocked_tmp).unwrap();
    let _ = std::fs::remove_dir_all(root);
}

#[test]
fn concurrent_apply_retirement_cannot_erase_a_newer_stage_publisher() {
    let (s, root) = temp_staging();
    write_ready(&s, 5);
    make_app(&s.staged_app, "OLD-STAGE");

    let apply = FileLock::acquire(&s.apply_lock).unwrap();
    let stage = s.clone();
    let (prepared_tx, prepared_rx) = std::sync::mpsc::sync_channel(0);
    let publisher = std::thread::spawn(move || {
        let _stage_lock = FileLock::acquire(&stage.stage_lock).unwrap();
        let incoming = stage.staged_dir().join("aterm.app.incoming");
        make_app(&incoming, "NEW-STAGE");
        let scratch = stage.download.join("aterm-0.0.6.dmg");
        std::fs::write(&scratch, b"new download").unwrap();
        prepared_tx.send((incoming.clone(), scratch)).unwrap();
        let ready = Ready {
            build_number: 6,
            version: "0.0.6".into(),
            commit: Some("0123456789abcdef0123456789abcdef01234567".into()),
            dmg_sha256: "cd".repeat(32),
            team_id: "T".into(),
            staged_at: String::new(),
            changelog: None,
            machine_id: None,
            roster_seq: None,
            roster_confirmed_at: Some(crate::install::now_rfc3339()),
        };
        publish_verified_stage(&stage, &incoming, &ready)
    });

    let (incoming, scratch) = prepared_rx.recv().unwrap();
    s.retire_published();
    assert!(incoming.is_dir(), "apply must not erase unpublished input");
    assert_eq!(
        std::fs::read(&scratch).unwrap(),
        b"new download",
        "apply must not erase stage-owned download scratch"
    );
    drop(apply);

    publisher.join().unwrap().unwrap();
    assert_eq!(read_id(&s.staged_app), "NEW-STAGE");
    assert!(matches!(
        read_ready(&s, 5),
        ReadyState::Newer(Ready {
            build_number: 6,
            ..
        })
    ));
    let _ = std::fs::remove_dir_all(root);
}

#[test]
fn trial_arm_failure_keeps_verified_stage_ready_and_unswapped() {
    let (s, root) = temp_staging();
    write_ready(&s, 6);
    make_app(&s.staged_app, "VERIFIED-NEW");
    std::fs::create_dir_all(s.root.join("boot.sentinel")).unwrap();
    let ready = Ready::read(&s.ready).unwrap();

    assert!(prepare_trial(&s, &ready, Path::new("/Applications/aterm.app")).is_err());
    assert!(s.ready.exists());
    assert_eq!(read_id(&s.staged_app), "VERIFIED-NEW");
    assert!(
        crate::manifest::FailedMark::read(&s.trial()).is_none(),
        "a failed sentinel arm cannot mint partial trial authority"
    );
    let _ = std::fs::remove_dir_all(root);
}

fn make_app(dir: &Path, id: &str) {
    std::fs::create_dir_all(dir).unwrap();
    std::fs::write(dir.join("id"), id).unwrap();
}
fn read_id(dir: &Path) -> String {
    std::fs::read_to_string(dir.join("id")).unwrap()
}
fn copied_candidate(path: &Path) -> PreparedSwapCandidate {
    PreparedSwapCandidate {
        fixed: path.to_path_buf(),
        moved_from_stage: false,
    }
}

fn disk_model_step(
    model: &aterm_spec::derive::Model,
    state: &mut aterm_spec::interp::State,
    action: &'static str,
) {
    let successors = model.successors(action, state);
    assert_eq!(
        successors.len(),
        1,
        "real transaction step must map to exactly one {action} successor: {state:?}"
    );
    *state = successors[0].clone();
    for invariant in &model.invariants {
        assert!(
            model.check_invariant(invariant.name, state),
            "real transaction step violates {}::{} after {action}: {state:?}",
            model.name,
            invariant.name,
        );
    }
}

fn disk_model_ready(model: &aterm_spec::derive::Model) -> aterm_spec::interp::State {
    let mut state = model.init_state();
    for action in [
        "ConsumeStartupAuthority",
        "ObserveBootHealth",
        "EnterDiskLane",
    ] {
        disk_model_step(model, &mut state, action);
    }
    state
}

fn fixture_identity(path: &Path) -> i64 {
    match std::fs::read_to_string(path.join("id")).as_deref() {
        Ok("OLD") => 1,
        Ok("NEW") => 2,
        Ok(_) => 3,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => 0,
        Err(error) => panic!("read fixture identity {}: {error}", path.display()),
    }
}

/// Projects the real fixed paths and exact ledgers back onto the derived
fn assert_real_disk_projection(
    state: &aterm_spec::interp::State,
    staging: &Staging,
    installed: &Path,
    current_build: u64,
    current_commit: &str,
    current_digest: &str,
) {
    let installed_identity = fixture_identity(installed);
    let fixed = rollback_path(installed);
    let fixed_identity = match fixture_identity(&fixed) {
        0 => 0,
        _ if KeptPredecessor::read(staging).is_some_and(|kept| kept.bundle == fixed) => 4,
        identity => identity,
    };
    assert_eq!(state["installed"], installed_identity);
    assert_eq!(state["fixed"], fixed_identity);
    assert_eq!(
        state["fixed_exact"],
        i64::from(matches!(fixed_identity, 1 | 2))
    );

    let trial_exact =
        matches!(
            boot_sentinel(staging).read_state(),
            Some((build, _)) if build == current_build
        ) && crate::manifest::FailedMark::read(&staging.trial()).is_some_and(|trial| {
            trial.build_number == current_build && trial.sha256.eq_ignore_ascii_case(current_digest)
        });
    assert_eq!(state["trial"], i64::from(trial_exact));

    let receipt_exact = crate::manifest::InstalledReceipt::read(&staging.installed_receipt())
        .is_some_and(|receipt| {
            receipt.matches_sealed(current_build, current_commit)
                && receipt.dmg_sha256.eq_ignore_ascii_case(current_digest)
        });
    assert_eq!(state["receipt"], i64::from(receipt_exact));
    assert_eq!(state["receipt_exact"], i64::from(receipt_exact));
}
/// A same-volume temp root (both bundles must share a volume for RENAME_SWAP).
fn swap_root(label: &str) -> PathBuf {
    let n = SEQ.fetch_add(1, Ordering::Relaxed);
    let root = std::env::temp_dir().join(format!("aterm-swap-{label}-{}-{n}", std::process::id()));
    let _ = std::fs::remove_dir_all(&root);
    std::fs::create_dir_all(&root).unwrap();
    root
}

/// A swap's parents take the tier its caller found (aterm-durable's counting seam):
#[test]
fn a_swap_on_the_install_device_orders_its_entries_and_another_device_stays_durable() {
    use aterm_durable::{Flush, seam::Recording};
    let root = swap_root("flush-tiers");
    let installed = root.join("installed.app");
    let fixed = rollback_path(&installed);
    make_app(&fixed, "NEW");
    make_app(&installed, "OLD");
    let staging = crate::paths::Staging::scratch("swap-flush-tiers");
    let flush = install_flush(&staging, &installed);
    assert_eq!(flush, Flush::Ordered, "both under the temp volume");
    #[cfg(any(target_vendor = "apple", target_os = "linux"))]
    assert_eq!(
        install_flush(&staging, Path::new("/dev")),
        Flush::Durable,
        "an install root on another device keeps the full flush"
    );
    let candidate = copied_candidate(&fixed);
    let recording = Recording::start();
    let rollback = swap_fixed_candidate(&candidate, &installed, flush).expect("fixed swap");
    assert_eq!(
        recording.take(),
        [Flush::Ordered],
        "the shared parent, once"
    );
    restore_rollback(&rollback, &installed, flush).unwrap();
    assert_eq!(recording.take(), [Flush::Ordered], "the inverse swap");
    assert_eq!(read_id(&installed), "OLD");
    let _ = std::fs::remove_dir_all(root);
}

/// The boot lane's own transaction takes [`install_flush`]'s tier at every step
#[test]
fn the_boot_lanes_arm_swap_and_unswap_take_the_install_tier() {
    use aterm_durable::{Flush, seam::Recording};
    const O: Flush = Flush::Ordered;
    const D: Flush = Flush::Durable;
    let [here, elsewhere] = on_and_off_the_install_device(|| {
        let (s, root) = temp_staging();
        write_ready(&s, 11);
        let installed = root.join("Applications/aterm.app");
        make_app(&installed, "OLD");
        let fixed = rollback_path(&installed);
        make_app(&fixed, "NEW");
        let ready = Ready::read(&s.ready).unwrap();
        crate::manifest::InstalledReceipt::record(
            &s.installed_receipt(),
            10,
            "89abcdef0123456789abcdef0123456789abcdef",
            &"cd".repeat(32),
            D,
        )
        .unwrap();
        let previous = crate::manifest::InstalledReceipt::read(&s.installed_receipt()).unwrap();
        let recording = Recording::start();
        let sentinel = prepare_trial(&s, &ready, &installed).unwrap();
        let armed = recording.take();
        let retained = swap_in_and_record_receipt(
            &s,
            &copied_candidate(&fixed),
            &installed,
            &ready,
            &sentinel,
        )
        .expect("swapped in, receipt recorded");
        let swapped = recording.take();
        assert_eq!(read_id(&installed), "NEW");
        let exec_error = std::io::Error::from(std::io::ErrorKind::NotFound);
        let receipt_error = unswap_after_failed_exec(
            &s,
            &retained,
            &installed,
            &sentinel,
            Some(&previous),
            &exec_error,
        )
        .expect("OLD back, trial disarmed");
        assert_eq!(receipt_error, None);
        let unswapped = recording.take();
        assert_eq!(read_id(&installed), "OLD");
        let _ = std::fs::remove_dir_all(root);
        [armed, swapped, unswapped]
    });
    assert_eq!(
        here,
        [vec![O, O, O, O], vec![O, O, O], vec![O, O, O]],
        "one device: the trial identity and the arm, file and entry each; the swap's \
         parent, then the receipt; the inverse swap, then OLD's receipt"
    );
    assert_eq!(
        elsewhere,
        [vec![O, O, D, D], vec![D, D, D], vec![D, D, D]],
        "another device: the trial identity keeps its own same-device guard \
         (`FailedMark::record_required`); every other step is durable"
    );
}

/// A swap whose receipt could not be written is rolled back with [`install_flush`]'s
#[test]
fn an_unreceipted_swaps_rollback_takes_the_install_tier() {
    use aterm_durable::{Flush, seam::Recording};
    let [here, elsewhere] = on_and_off_the_install_device(|| {
        let (s, root) = temp_staging();
        write_ready(&s, 11);
        let installed = root.join("Applications/aterm.app");
        make_app(&installed, "OLD");
        let fixed = rollback_path(&installed);
        make_app(&fixed, "NEW");
        let prepared = copied_candidate(&fixed);
        let ready = Ready::read(&s.ready).unwrap();
        let sentinel = prepare_trial(&s, &ready, &installed).unwrap();
        let retained =
            swap_fixed_candidate(&prepared, &installed, aterm_durable::Flush::Durable).unwrap();
        let recording = Recording::start();
        let outcome =
            roll_back_unreceipted_swap(&prepared, &retained, &installed, &sentinel, &s, "full");
        let flushes = recording.take();
        assert!(matches!(outcome, ApplyOutcome::Deferred(_)), "{outcome:?}");
        assert_eq!(read_id(&installed), "OLD");
        let _ = std::fs::remove_dir_all(root);
        flushes
    });
    assert_eq!(here, [Flush::Ordered], "one device: the inverse swap");
    assert_eq!(elsewhere, [Flush::Durable], "another device");
}

/// The receipt a counted launch binds from `ready.toml` after a cut between the swap
#[test]
fn a_trials_recovered_receipt_takes_the_install_tier() {
    use aterm_durable::{Flush, seam::Recording};
    const COMMIT: &str = "0123456789abcdef0123456789abcdef01234567";
    let [here, elsewhere] = on_and_off_the_install_device(|| {
        let (s, root) = temp_staging();
        write_ready(&s, 11);
        let installed = root.join("Applications/aterm.app");
        make_app(&installed, "NEW");
        crate::manifest::FailedMark::record_required(
            &s.trial(),
            11,
            &"ab".repeat(32),
            Some(&installed),
        )
        .unwrap();
        let sealed = installed.clone();
        let identity = move |path: &Path| {
            if path == sealed.as_path() {
                Ok((11, COMMIT.to_string()))
            } else {
                Err("not the install".to_string())
            }
        };
        let recording = Recording::start();
        prove_current_trial(&s, &installed, 11, Some(COMMIT), &identity)
            .expect("the receipt is bound from ready.toml");
        let flushes = recording.take();
        assert!(
            crate::manifest::InstalledReceipt::read(&s.installed_receipt()).is_some(),
            "the receipt is there"
        );
        let _ = std::fs::remove_dir_all(root);
        flushes
    });
    assert_eq!(here, [Flush::Ordered, Flush::Ordered], "one device");
    assert_eq!(
        elsewhere,
        [Flush::Durable, Flush::Durable],
        "another device"
    );
}

/// A newer stage superseding an unconfirmed activation puts the running build back
#[test]
fn a_superseded_activations_inverse_swap_takes_the_install_tier() {
    use aterm_durable::{Flush, seam::Recording};
    let [here, elsewhere] = on_and_off_the_install_device(|| {
        let (s, root, installed) = superseded_activation_desk();
        let recording = Recording::start();
        supersede_desk(&s, &installed, (10, SUPERSEDE_RUNNING_COMMIT), 12)
            .expect("a newer verified stage supersedes the activation");
        let flushes = recording.take();
        let _ = std::fs::remove_dir_all(root);
        flushes
    });
    assert_eq!(here, [Flush::Ordered], "one device");
    assert_eq!(elsewhere, [Flush::Durable], "another device");
}

#[cfg(unix)]
#[test]
fn fixed_candidate_symlink_is_never_swapped_into_canonical_install() {
    use std::os::unix::fs::symlink;

    let root = swap_root("fixed-symlink");
    let installed = root.join("installed.app");
    let target = root.join("signed-looking-target.app");
    let fixed = rollback_path(&installed);
    make_app(&installed, "OLD");
    make_app(&target, "TARGET");
    symlink(&target, &fixed).unwrap();

    swap_fixed_candidate(
        &copied_candidate(&fixed),
        &installed,
        aterm_durable::Flush::Durable,
    )
    .unwrap_err();
    assert_eq!(read_id(&installed), "OLD");
    assert!(
        std::fs::symlink_metadata(&fixed)
            .unwrap()
            .file_type()
            .is_symlink()
    );
    let _ = std::fs::remove_dir_all(root);
}

#[cfg(unix)]
#[test]
fn inverse_swap_rejects_substituted_symlink_rollback() {
    use std::os::unix::fs::symlink;

    let root = swap_root("rollback-symlink");
    let installed = root.join("installed.app");
    let target = root.join("target.app");
    let rollback = rollback_path(&installed);
    make_app(&installed, "NEW");
    make_app(&target, "OLD-TARGET");
    symlink(&target, &rollback).unwrap();

    restore_rollback(&rollback, &installed, aterm_durable::Flush::Durable).unwrap_err();
    assert_eq!(read_id(&installed), "NEW");
    assert_eq!(read_id(&target), "OLD-TARGET");
    assert!(
        std::fs::symlink_metadata(&rollback)
            .unwrap()
            .file_type()
            .is_symlink()
    );
    let _ = std::fs::remove_dir_all(root);
}

#[test]
fn same_volume_pre_swap_failure_restores_published_stage() {
    let (s, root) = temp_staging();
    write_ready(&s, 54);
    make_app(&s.staged_app, "NEW");
    let installed = root.join("Applications/aterm.app");
    make_app(&installed, "OLD");
    let fixed = rollback_path(&installed);
    std::fs::rename(&s.staged_app, &fixed).unwrap();
    let prepared = PreparedSwapCandidate {
        fixed: fixed.clone(),
        moved_from_stage: true,
    };
    recover_prepared_candidate(&prepared, &s);
    assert_eq!(read_id(&s.staged_app), "NEW");
    assert!(!fixed.exists());
    assert!(s.ready.exists());
    assert_eq!(read_id(&installed), "OLD");
    let _ = std::fs::remove_dir_all(root);
}

/// A swap whose installed receipt cannot be written rolls back, and on a
#[test]
fn a_failed_receipt_after_a_copied_swap_keeps_the_intact_stage() {
    let (s, root) = temp_staging();
    write_ready(&s, 11);
    make_app(&s.staged_app, "STAGED-NEW");
    let installed = root.join("Applications").join("aterm.app");
    make_app(&installed, "OLD");
    let fixed = rollback_path(&installed);
    make_app(&fixed, "COPIED-NEW");
    let prepared = copied_candidate(&fixed);
    let sentinel = boot_sentinel(&s);
    sentinel.arm(11).unwrap();
    crate::manifest::FailedMark::record(&s.trial(), 11, &"ab".repeat(32));
    let retained =
        swap_fixed_candidate(&prepared, &installed, aterm_durable::Flush::Durable).unwrap();
    assert_eq!(read_id(&installed), "COPIED-NEW");

    let outcome =
        roll_back_unreceipted_swap(&prepared, &retained, &installed, &sentinel, &s, "disk full");

    assert!(
        matches!(&outcome, ApplyOutcome::Deferred(reason) if reason.contains("disk full")),
        "{outcome:?}"
    );
    assert_eq!(read_id(&installed), "OLD", "the previous build is back");
    assert!(sentinel.read_state().is_none(), "the trial is disarmed");
    assert_eq!(
        read_id(&s.staged_app),
        "STAGED-NEW",
        "the intact published stage is kept, not replaced by the fixed copy"
    );
    assert!(
        Ready::read(&s.ready).is_some(),
        "the publication still stands, so the next launch retries without a download"
    );
    assert!(!fixed.exists(), "the redundant fixed copy is reclaimed");
    let _ = std::fs::remove_dir_all(root);
}

#[test]
fn old_process_confirmation_cannot_gc_a_newer_trial_or_its_receipt() {
    let (s, root) = temp_staging();
    let sentinel = boot_sentinel(&s);
    sentinel.arm(1000).unwrap();
    crate::manifest::FailedMark::record(&s.trial(), 1000, &"ab".repeat(32));
    crate::manifest::InstalledReceipt::record(
        &s.installed_receipt(),
        1000,
        "0123456789abcdef0123456789abcdef01234567",
        &"ab".repeat(32),
        aterm_durable::Flush::Durable,
    )
    .unwrap();
    make_app(&s.staged_app, "SWAPPED-OUT-OLD");
    let installed = root.join("Applications").join("aterm.app");
    make_app(&installed, "RUNNING-NEW");
    let rollback = rollback_path(&installed);
    make_app(&rollback, "ROLLBACK");

    let _apply = FileLock::acquire(&s.apply_lock).unwrap();
    assert!(
        !confirm_health_under_apply_lock(&s, 999, None, Some(&installed)),
        "an overlapping old process has no authority over the newer trial"
    );
    assert_eq!(
        sentinel.read_state(),
        Some((1000, 0)),
        "non-matching confirm is a no-op"
    );
    assert!(s.trial().exists());
    assert!(s.installed_receipt().exists());
    assert!(s.staged_app.exists());
    assert!(rollback.exists());
    let _ = std::fs::remove_dir_all(root);
}

#[test]
fn unresolved_installed_bundle_cannot_disarm_or_gc_trial() {
    let (s, root) = temp_staging();
    let sentinel = boot_sentinel(&s);
    sentinel.arm(1000).unwrap();
    crate::manifest::FailedMark::record(&s.trial(), 1000, &"ab".repeat(32));
    let _apply = FileLock::acquire(&s.apply_lock).unwrap();
    assert!(!confirm_health_under_apply_lock(&s, 1000, None, None));
    assert_eq!(sentinel.read_state(), Some((1000, 0)));
    assert!(s.trial().exists());
    let _ = std::fs::remove_dir_all(root);
}

/// The two mismatched-sentinel shapes are disjoint, and neither admits a process
#[test]
fn dead_authority_and_abandoned_preswap_shapes_are_disjoint() {
    assert!(dead_authority_trial(1001, 1001, 1000));
    assert!(!abandoned_preswap_trial(1001, 1001, 1000));
    assert!(abandoned_preswap_trial(1000, 1000, 1001));
    assert!(!dead_authority_trial(1000, 1000, 1001));
    assert!(!dead_authority_trial(999, 1001, 1000));
    assert!(!abandoned_preswap_trial(999, 1000, 1001));
}

const SUPERSEDE_RUNNING_COMMIT: &str = "1111111111111111111111111111111111111111";
const SUPERSEDE_ACTIVATION_COMMIT: &str = "2222222222222222222222222222222222222222";
const SUPERSEDE_NEWER_COMMIT: &str = "3333333333333333333333333333333333333333";
const SUPERSEDE_ACTIVATION_DIGEST: &str =
    "abababababababababababababababababababababababababababababababab";

fn sealed_fixture(app: &Path, build: u64, commit: &str) {
    std::fs::create_dir_all(app).unwrap();
    std::fs::write(app.join("sealed"), format!("{build} {commit}")).unwrap();
}

fn sealed_fixture_identity(app: &Path) -> Result<(u64, String), String> {
    let text = std::fs::read_to_string(app.join("sealed"))
        .map_err(|error| format!("{}: {error}", app.display()))?;
    let (build, commit) = text
        .split_once(' ')
        .ok_or_else(|| "malformed fixture".to_string())?;
    Ok((
        build.parse().map_err(|error| format!("{error}"))?,
        commit.to_string(),
    ))
}

fn write_ready_for(s: &Staging, build: u64, commit: &str) {
    let r = Ready {
        build_number: build,
        version: format!("0.{build}.0"),
        commit: Some(commit.into()),
        dmg_sha256: "cd".repeat(32),
        team_id: "T".into(),
        staged_at: String::new(),
        changelog: None,
        machine_id: None,
        roster_seq: None,
        roster_confirmed_at: Some(crate::install::now_rfc3339()),
    };
    std::fs::write(&s.ready, r.to_toml().unwrap()).unwrap();
}

/// The disk a failed launched-lane handoff leaves behind, with a newer release staged:
fn superseded_activation_desk() -> (crate::paths::ScratchStaging, PathBuf, PathBuf) {
    let (s, root) = temp_staging();
    let installed = root.join("Applications").join("aterm.app");
    sealed_fixture(&installed, 11, SUPERSEDE_ACTIVATION_COMMIT);
    sealed_fixture(&rollback_path(&installed), 10, SUPERSEDE_RUNNING_COMMIT);
    write_ready_for(&s, 12, SUPERSEDE_NEWER_COMMIT);
    sealed_fixture(&s.staged_app, 12, SUPERSEDE_NEWER_COMMIT);
    crate::manifest::FailedMark::record_required(
        &s.trial(),
        11,
        SUPERSEDE_ACTIVATION_DIGEST,
        Some(&installed),
    )
    .unwrap();
    crate::manifest::InstalledReceipt::record(
        &s.installed_receipt(),
        11,
        SUPERSEDE_ACTIVATION_COMMIT,
        SUPERSEDE_ACTIVATION_DIGEST,
        aterm_durable::Flush::Durable,
    )
    .unwrap();
    let sentinel = boot_sentinel(&s);
    sentinel.arm(11).unwrap();
    sentinel.observe_launch(11).unwrap();
    sentinel.observe_launch(11).unwrap();
    (s, root, installed)
}

fn supersede_desk(
    s: &Staging,
    installed: &Path,
    running: (u64, &str),
    newer: u64,
) -> Result<crate::SupersededActivation, crate::SupersedeRefusal> {
    supersede_activation_locked(
        s,
        installed,
        running,
        (11, SUPERSEDE_ACTIVATION_COMMIT),
        newer,
        &sealed_fixture_identity,
    )
}

/// A newer stage retires the unconfirmed activation without counting a launch of
#[test]
fn a_newer_stage_supersedes_an_unconfirmed_activation_without_counting_a_launch() {
    let (s, root, installed) = superseded_activation_desk();
    assert_eq!(boot_sentinel(&s).read_state(), Some((11, 2)));

    let superseded = supersede_desk(&s, &installed, (10, SUPERSEDE_RUNNING_COMMIT), 12)
        .expect("a newer verified stage supersedes the activation");
    assert_eq!(
        superseded,
        crate::SupersededActivation {
            activation_build: 11,
            restored_build: 10,
            newer_build: 12,
            trial_launches: 2,
            trial_disarm_deferred: None,
        }
    );
    assert_eq!(
        sealed_fixture_identity(&installed),
        Ok((10, SUPERSEDE_RUNNING_COMMIT.to_string())),
        "the running build is back at the install path"
    );
    assert!(
        !rollback_path(&installed).exists(),
        "the retired activation is reclaimed"
    );
    assert_eq!(
        boot_sentinel(&s).read_state(),
        None,
        "the trial is disarmed, never walked"
    );
    assert!(!s.trial().exists());
    assert!(
        crate::manifest::InstalledReceipt::read(&s.installed_receipt()).is_none(),
        "a receipt naming the retired activation is not proof for what is installed"
    );
    assert!(
        crate::manifest::FailedMark::read(&s.failed()).is_none(),
        "nothing is quarantined: the activation never failed to BOOT"
    );
    assert_eq!(
        sealed_fixture_identity(&s.staged_app),
        Ok((12, SUPERSEDE_NEWER_COMMIT.to_string())),
        "the stage is untouched"
    );

    assert!(identity_matches_running(
        10,
        SUPERSEDE_RUNNING_COMMIT,
        10,
        Some(SUPERSEDE_RUNNING_COMMIT)
    ));
    let ReadyState::Newer(ready) = read_ready(&s, 10) else {
        panic!("the newer stage is still ready over the running build");
    };
    assert_eq!(ready.build_number, 12);
    assert!(
        recover_abandoned_preswap_trial_if_exact(
            &s,
            &installed,
            10,
            Some(SUPERSEDE_RUNNING_COMMIT)
        ),
        "no trial is left for the next boot apply to refuse on"
    );
    assert_eq!(
        recover_orphaned_prepared_candidate(&s, &installed, 10, &ready),
        Ok(())
    );
    assert!(s.ready.exists(), "the stage survives the orphan check");
    let _ = std::fs::remove_dir_all(root);
}

/// While another window's candidate of the activation runs (the trial's launcher
#[test]
fn a_supersede_waits_while_another_windows_candidate_of_the_activation_runs() {
    use aterm_uds::spawnfd::Command;
    let (s, root, installed) = superseded_activation_desk();
    let mut sibling = Command::new("/bin/sleep")
        .arg("30")
        .spawn()
        .expect("start a stand-in candidate");
    count_trial_launch(
        &s,
        &boot_sentinel(&s),
        11,
        TrialLauncher::of(sibling.id()),
        true,
        crate::TrialWitness::Window,
    )
    .unwrap();
    let counted = boot_sentinel(&s).read_state();

    let refused = supersede_desk(&s, &installed, (10, SUPERSEDE_RUNNING_COMMIT), 12);
    let held = matches!(
        &refused,
        Err(crate::SupersedeRefusal::ForNow(why)) if why.contains(&sibling.id().to_string())
    );
    sibling.kill().unwrap();
    sibling.wait().unwrap();
    assert!(
        held,
        "a live candidate of the activation holds its retire, for the moment: {refused:?}"
    );
    assert_eq!(
        boot_sentinel(&s).read_state(),
        counted,
        "the trial stays armed"
    );
    assert_eq!(
        sealed_fixture_identity(&installed),
        Ok((11, SUPERSEDE_ACTIVATION_COMMIT.to_string())),
        "the activation stays installed under its candidate"
    );
    assert_eq!(
        sealed_fixture_identity(&rollback_path(&installed)),
        Ok((10, SUPERSEDE_RUNNING_COMMIT.to_string())),
        "the predecessor stays retained"
    );

    let retired = supersede_desk(&s, &installed, (10, SUPERSEDE_RUNNING_COMMIT), 12)
        .expect("with no live candidate the newer stage supersedes the activation");
    assert_eq!(retired.activation_build, 11);
    assert_eq!(
        sealed_fixture_identity(&installed),
        Ok((10, SUPERSEDE_RUNNING_COMMIT.to_string()))
    );
    let _ = std::fs::remove_dir_all(root);
}

/// A window's fresh cached pass for an activation another window has since retired
#[test]
fn a_cached_activation_pass_refuses_once_the_install_path_holds_another_build() {
    let (s, root, _installed) = superseded_activation_desk();
    assert_eq!(
        handoff_activation_cached_pass_refusal_at(&s, 11, || Some(Ok(11))),
        None,
        "the activation is still installed"
    );
    assert_eq!(
        handoff_activation_cached_pass_refusal_at(&s, 11, || None),
        None,
        "no installed bundle to read: nothing this pass could launch differently"
    );
    let moved = handoff_activation_cached_pass_refusal_at(&s, 11, || Some(Ok(10)))
        .expect("the install path holds the retired activation's predecessor");
    assert!(crate::is_passing_refusal(&moved), "{moved}");
    let unread = handoff_activation_cached_pass_refusal_at(&s, 11, || Some(Err("EIO".to_string())))
        .expect("an unread install path refuses");
    assert!(crate::is_passing_refusal(&unread), "{unread}");
    crate::manifest::Floor::bump_and_write(&s.floor(), 12, 0, 0);
    assert!(
        handoff_activation_cached_pass_refusal_at(&s, 11, || Some(Ok(11)))
            .is_some_and(|refusal| refusal.contains("floor")),
        "a yanked activation is refused whatever the install path holds"
    );
    let _ = std::fs::remove_dir_all(root);
}

/// The crash cut where the exchange happened and the disarm did not (the staging root
#[test]
fn a_supersede_that_cannot_disarm_leaves_the_cut_the_next_boot_apply_finishes() {
    use std::os::unix::fs::PermissionsExt;
    let (s, root, installed) = superseded_activation_desk();
    std::fs::set_permissions(&s.root, std::fs::Permissions::from_mode(0o500)).unwrap();
    let result = supersede_desk(&s, &installed, (10, SUPERSEDE_RUNNING_COMMIT), 12);
    std::fs::set_permissions(&s.root, std::fs::Permissions::from_mode(0o700)).unwrap();
    let retired = result.expect("the exchange happened: a retire, with its disarm deferred");
    assert_eq!(
        (
            retired.activation_build,
            retired.restored_build,
            retired.newer_build
        ),
        (11, 10, 12)
    );
    assert!(retired.trial_disarm_deferred.is_some(), "{retired:?}");

    assert_eq!(
        sealed_fixture_identity(&installed),
        Ok((10, SUPERSEDE_RUNNING_COMMIT.to_string()))
    );
    let fixed = rollback_path(&installed);
    assert_eq!(
        sealed_fixture_identity(&fixed),
        Ok((11, SUPERSEDE_ACTIVATION_COMMIT.to_string()))
    );
    assert_eq!(
        boot_sentinel(&s).read_state(),
        Some((11, 2)),
        "not one launch counted"
    );
    assert!(abandoned_preswap_trial(10, 10, 11));
    assert!(trial_owned_by(&s, &installed));
    assert!(same_volume(&fixed, &installed));
    assert!(trial_authorizes_candidate(
        &s,
        11,
        SUPERSEDE_ACTIVATION_COMMIT
    ));
    assert!(
        s.ready.exists() && s.staged_app.exists(),
        "the stage the recovery then applies is untouched"
    );
    let _ = std::fs::remove_dir_all(root);
}

/// Nothing moves unless every precondition holds: each way the disk can differ
#[test]
fn a_supersede_moves_nothing_unless_every_precondition_holds() {
    type Mutation = fn(&Staging, &Path, &Path);
    type Case = (&'static str, Mutation, (u64, &'static str), u64, bool);
    let other_install = |s: &Staging, _installed: &Path, root: &Path| {
        let other = root.join("Other").join("aterm.app");
        sealed_fixture(&other, 11, SUPERSEDE_ACTIVATION_COMMIT);
        crate::manifest::FailedMark::record_required(
            &s.trial(),
            11,
            SUPERSEDE_ACTIVATION_DIGEST,
            Some(&other),
        )
        .unwrap();
    };
    fn unverifiable(app: &Path) {
        std::fs::remove_file(app.join("sealed")).unwrap();
    }
    let cases: [Case; 14] = [
        (
            "not newer",
            |_, _, _| {},
            (10, SUPERSEDE_RUNNING_COMMIT),
            11,
            false,
        ),
        (
            "the marker names another build",
            |s, _, _| write_ready_for(s, 13, SUPERSEDE_NEWER_COMMIT),
            (10, SUPERSEDE_RUNNING_COMMIT),
            12,
            true,
        ),
        (
            "the staged bundle is not the marker's",
            |s, _, _| sealed_fixture(&s.staged_app, 13, SUPERSEDE_NEWER_COMMIT),
            (10, SUPERSEDE_RUNNING_COMMIT),
            12,
            true,
        ),
        (
            "the staged bundle does not verify",
            |s, _, _| unverifiable(&s.staged_app),
            (10, SUPERSEDE_RUNNING_COMMIT),
            12,
            true,
        ),
        (
            "the stage is yanked",
            |s, _, _| std::fs::write(s.floor(), "min_build = 13\n").unwrap(),
            (10, SUPERSEDE_RUNNING_COMMIT),
            12,
            false,
        ),
        (
            "the installed bundle is not the activation",
            |_, installed, _| sealed_fixture(installed, 13, SUPERSEDE_NEWER_COMMIT),
            (10, SUPERSEDE_RUNNING_COMMIT),
            12,
            false,
        ),
        (
            "the installed bundle does not verify",
            |_, installed, _| unverifiable(installed),
            (10, SUPERSEDE_RUNNING_COMMIT),
            12,
            true,
        ),
        (
            "the trial is armed for another build",
            |s, _, _| boot_sentinel(s).arm(12).unwrap(),
            (10, SUPERSEDE_RUNNING_COMMIT),
            12,
            false,
        ),
        (
            "no trial is armed",
            |s, _, _| boot_sentinel(s).confirm().unwrap(),
            (10, SUPERSEDE_RUNNING_COMMIT),
            12,
            false,
        ),
        (
            "the trial belongs to another install",
            other_install,
            (10, SUPERSEDE_RUNNING_COMMIT),
            12,
            false,
        ),
        (
            "the trial marker names other bytes",
            |s, _, _| {
                crate::manifest::InstalledReceipt::clear(&s.installed_receipt());
            },
            (10, SUPERSEDE_RUNNING_COMMIT),
            12,
            false,
        ),
        (
            "the retained predecessor is not the running build",
            |_, installed, _| {
                sealed_fixture(&rollback_path(installed), 9, SUPERSEDE_RUNNING_COMMIT);
            },
            (10, SUPERSEDE_RUNNING_COMMIT),
            12,
            false,
        ),
        (
            "the retained predecessor does not verify",
            |_, installed, _| unverifiable(&rollback_path(installed)),
            (10, SUPERSEDE_RUNNING_COMMIT),
            12,
            true,
        ),
        (
            "the running build is another commit",
            |_, _, _| {},
            (10, "4444444444444444444444444444444444444444"),
            12,
            false,
        ),
    ];
    for (why, mutate, running, newer, for_now) in cases {
        let (s, root, installed) = superseded_activation_desk();
        mutate(&s, &installed, &root);
        let installed_before = sealed_fixture_identity(&installed);
        let fixed_before = sealed_fixture_identity(&rollback_path(&installed));
        let sentinel_before = boot_sentinel(&s).read_state();
        let refusal = supersede_desk(&s, &installed, running, newer)
            .expect_err(&format!("{why}: must refuse"));
        assert_eq!(
            refusal.is_for_now(),
            for_now,
            "{why}: the refusal's kind ({refusal:?})"
        );
        assert_eq!(
            sealed_fixture_identity(&installed),
            installed_before,
            "{why}"
        );
        assert_eq!(
            sealed_fixture_identity(&rollback_path(&installed)),
            fixed_before,
            "{why}"
        );
        assert_eq!(boot_sentinel(&s).read_state(), sentinel_before, "{why}");
        assert!(s.ready.exists() && s.staged_app.exists(), "{why}");
        let _ = std::fs::remove_dir_all(root);
    }
}

/// An out-of-band install (install.sh, a DMG drag) during an armed trial leaves a
#[test]
fn a_trial_armed_below_the_running_build_is_retired_not_kept_forever() {
    let (s, root) = temp_staging();
    let commit = "0123456789abcdef0123456789abcdef01234567";
    let sentinel = boot_sentinel(&s);
    sentinel.arm(1000).unwrap();
    crate::manifest::FailedMark::record(&s.trial(), 1000, &"ab".repeat(32));
    crate::manifest::InstalledReceipt::record(
        &s.installed_receipt(),
        1000,
        commit,
        &"ab".repeat(32),
        aterm_durable::Flush::Durable,
    )
    .unwrap();
    let installed = root.join("Applications").join("aterm.app");
    make_app(&installed, "HAND-INSTALLED-1001");
    let rollback = rollback_path(&installed);
    make_app(&rollback, "OLDER-THAN-RUNNING");

    assert!(disarm_dead_authority_trial(
        &s, &installed, 1000, 1001, commit, 1001
    ));
    assert_eq!(sentinel.read_state(), None, "the wedge is cleared");
    assert!(!s.trial().exists(), "the dead trial's identity goes too");
    assert!(
        !rollback.exists(),
        "its rollback source is older than the running build and has no sentinel behind it"
    );
    assert!(
        !s.installed_receipt().exists(),
        "a receipt for the dead build is not proof for the installed one"
    );
    let _ = std::fs::remove_dir_all(root);
}

/// A trial belongs to the install it swapped. A same-build process launched from
#[test]
fn a_trial_is_owned_by_the_install_it_swapped_and_no_sibling_bundle() {
    let (s, root) = temp_staging();
    let owner = root.join("Applications").join("aterm.app");
    let sibling = root.join("dist").join("aterm.app");
    make_app(&owner, "OWNER");
    make_app(&sibling, "SIBLING");
    write_ready(&s, 6);
    let ready = Ready::read(&s.ready).unwrap();
    let sentinel = prepare_trial(&s, &ready, &owner).unwrap();
    assert!(trial_owned_by(&s, &owner));
    assert!(!trial_owned_by(&s, &sibling));
    assert_eq!(sentinel.read_state(), Some((6, 0)));
    assert!(dead_authority_trial(7, 7, 6) && !trial_owned_by(&s, &sibling));
    assert!(may_recover_abandoned_preswap(&s, &owner, (6, 6, 7), true));
    assert!(!may_recover_abandoned_preswap(
        &s,
        &sibling,
        (6, 6, 7),
        true
    ));
    std::fs::remove_dir_all(&owner).unwrap();
    assert!(trial_owned_by(&s, &sibling), "a ghost owns nothing");
    crate::manifest::FailedMark::record(&s.trial(), 6, &"ab".repeat(32));
    assert!(trial_owned_by(&s, &sibling) && trial_owned_by(&s, &owner));
    let _ = std::fs::remove_dir_all(root);
}

/// The unrecoverable mismatched-sentinel shapes get a budgeted escape counted in
#[test]
fn an_unrecoverable_foreign_trial_is_disarmed_on_a_budget_not_on_sight() {
    let (s, root) = temp_staging();
    let sentinel = boot_sentinel(&s);
    sentinel.arm(1000).unwrap();
    crate::manifest::FailedMark::record(&s.trial(), 1000, &"ab".repeat(32));

    for launch in 1..MAX_BOOT_ATTEMPTS {
        let recording = aterm_durable::seam::Recording::start();
        assert!(
            !escape_wedged_foreign_trial(&s, 1001, 1000),
            "launch {launch} is inside the budget"
        );
        let flushes = recording.take();
        assert!(
            !flushes.is_empty() && flushes.iter().all(|f| *f == aterm_durable::Flush::Ordered),
            "launch {launch}: {flushes:?}"
        );
        assert_eq!(
            sentinel.read_state(),
            Some((1000, 0)),
            "the trialed build's OWN attempt count is never touched"
        );
    }

    assert!(
        escape_wedged_foreign_trial(&s, 1001, 1000),
        "the budget is exhausted"
    );
    assert_eq!(sentinel.read_state(), None, "the wedge is cleared");
    assert!(!s.trial().exists());
    assert!(
        !s.root.join("foreign-trial").exists(),
        "the counter is retired with the sentinel it was counting"
    );
    let _ = std::fs::remove_dir_all(root);
}

/// A trial owned by another install that still exists is not disarmed by a
#[test]
fn a_present_owners_trial_survives_a_siblings_budget_until_the_owner_is_long_idle() {
    let (s, root) = temp_staging();
    let owner = root.join("Applications").join("aterm.app");
    make_app(&owner, "OWNER");
    let sentinel = boot_sentinel(&s);
    sentinel.arm(1000).unwrap();
    crate::manifest::FailedMark::record_required(&s.trial(), 1000, &"ab".repeat(32), Some(&owner))
        .unwrap();
    for _ in 0..MAX_BOOT_ATTEMPTS + 2 {
        assert!(
            !escape_wedged_foreign_trial(&s, 1001, 1000),
            "a present, recently-touched owner keeps its trial past the budget"
        );
    }
    assert_eq!(
        sentinel.read_state(),
        Some((1000, 0)),
        "still armed, still untouched"
    );
    std::fs::remove_dir_all(&owner).unwrap();
    let mut disarmed = false;
    for _ in 0..MAX_BOOT_ATTEMPTS + 1 {
        disarmed |= escape_wedged_foreign_trial(&s, 1001, 1000);
    }
    assert!(disarmed, "a ghost owner cedes at the budget");
    assert_eq!(sentinel.read_state(), None);
    let _ = std::fs::remove_dir_all(root);
}

/// A live trial belonging to another installation is never disarmed. The staging
#[test]
fn a_foreign_trial_whose_owner_is_still_launching_is_never_disarmed() {
    let (s, root) = temp_staging();
    let sentinel = boot_sentinel(&s);
    sentinel.arm(1000).unwrap();

    for round in 0..4 {
        for _ in 1..MAX_BOOT_ATTEMPTS {
            assert!(
                !escape_wedged_foreign_trial(&s, 1001, 1000),
                "round {round}: inside the budget"
            );
        }
        std::thread::sleep(std::time::Duration::from_millis(10));
        let attempts = sentinel.observe_launch(1000).unwrap();
        assert!(attempts >= 1, "the owner really did advance its own trial");
        std::thread::sleep(std::time::Duration::from_millis(10));
        assert!(
            !escape_wedged_foreign_trial(&s, 1001, 1000),
            "round {round}: an advance restarts the budget instead of completing it"
        );
        assert!(
            sentinel.read_state().is_some(),
            "round {round}: the live trial survives"
        );
    }

    let _ = std::fs::remove_dir_all(root);
}

/// A new armed build restarts the budget: launches counted against a replaced
#[test]
fn the_foreign_trial_budget_does_not_accumulate_across_trials() {
    let (s, root) = temp_staging();
    let sentinel = boot_sentinel(&s);
    sentinel.arm(1000).unwrap();
    for _ in 1..MAX_BOOT_ATTEMPTS {
        assert!(!escape_wedged_foreign_trial(&s, 1001, 1000));
    }
    sentinel.arm(1002).unwrap();
    assert!(
        !escape_wedged_foreign_trial(&s, 1001, 1002),
        "the new trial gets the full budget, not the tail of the old one"
    );
    assert_eq!(sentinel.read_state(), Some((1002, 0)));
    let _ = std::fs::remove_dir_all(root);
}

/// Only a refusal is recorded as one: the every-launch outcomes would rewrite
#[test]
fn only_a_deferred_boot_apply_is_recorded_as_a_refusal() {
    let deferred = ApplyOutcome::Deferred("trial 1000 unconfirmed".to_string());
    assert_eq!(
        boot_apply_refusal_reason(&deferred),
        Some("trial 1000 unconfirmed")
    );
    for quiet in [
        ApplyOutcome::NotApplicable,
        ApplyOutcome::NoUpdate,
        ApplyOutcome::ReExecFailed("exec: ENOMEM".to_string()),
        ApplyOutcome::Reverted("build 1000 crash-looped; build 999 restored".to_string()),
    ] {
        assert_eq!(
            boot_apply_refusal_reason(&quiet),
            None,
            "{quiet:?} is not an apply refusal"
        );
    }
}

#[test]
fn active_trial_digest_rejects_same_build_commit_receipt_with_other_artifact() {
    let (s, root) = temp_staging();
    let commit = "0123456789abcdef0123456789abcdef01234567";
    crate::manifest::FailedMark::record(&s.trial(), 1000, &"ab".repeat(32));
    crate::manifest::InstalledReceipt::record(
        &s.installed_receipt(),
        1000,
        commit,
        &"cd".repeat(32),
        aterm_durable::Flush::Durable,
    )
    .unwrap();
    assert!(
        !trial_authorizes_candidate(&s, 1000, commit),
        "same build+commit with a different DMG must not authorize health or recovery"
    );
    let _ = std::fs::remove_dir_all(root);
}

/// Tier-1 conformance for the native update disk machine: drives the real
#[test]
fn native_update_disk_transaction_model_conforms_to_real_swap_recovery_and_health_guards() {
    let model = aterm_models_misc::native_update_disk_transaction_model();
    let commit = "0123456789abcdef0123456789abcdef01234567";
    let other_commit = "fedcba9876543210fedcba9876543210fedcba98";
    let digest = "ab".repeat(32);
    let build = 1000;

    assert_eq!(
        startup_authority_decision(false, ReexecAuthority::Absent, false),
        StartupAuthorityDecision::ObserveBootHealth
    );
    assert_eq!(
        startup_authority_decision(false, ReexecAuthority::Matched, true),
        StartupAuthorityDecision::ObserveBootHealth
    );
    assert_eq!(
        startup_authority_decision(true, ReexecAuthority::Absent, false),
        StartupAuthorityDecision::ReturnMalformedExpected
    );
    assert_eq!(
        startup_authority_decision(true, ReexecAuthority::Matched, true),
        StartupAuthorityDecision::ReturnMatchedReexec
    );

    assert!(identity_matches_running(
        build,
        commit,
        build,
        Some("0123456789ab")
    ));
    assert!(
        !identity_matches_running(build, other_commit, build, Some("0123456789ab")),
        "negative control: build-only comparison would authorize the wrong OLD"
    );
    assert!(
        !identity_matches_running(build + 1, commit, build, Some("0123456789ab")),
        "a bundle swapped under a live instance is a different build"
    );

    let (failed_staging, failed_root) = temp_staging();
    write_ready(&failed_staging, build);
    let failed_installed = failed_root.join("Applications/aterm.app");
    make_app(&failed_installed, "OLD");
    let failed_ready = Ready::read(&failed_staging.ready).unwrap();
    let failed_sentinel = prepare_trial(&failed_staging, &failed_ready, &failed_installed).unwrap();
    let mut failed_state = disk_model_ready(&model);
    disk_model_step(&model, &mut failed_state, "RemovePreviousReceipt");
    disk_model_step(&model, &mut failed_state, "PrepareFixedNew");
    disk_model_step(&model, &mut failed_state, "ArmExactTrial");
    let missing = rollback_path(&failed_installed);
    assert!(
        swap_fixed_candidate(
            &copied_candidate(&missing),
            &failed_installed,
            aterm_durable::Flush::Durable
        )
        .is_err()
    );
    let buggy = aterm_spec::interp::with_buggy(&model, 1);
    let torn = buggy.successors("BuggyTwoRenameSwapFailsMidway", &failed_state)[0].clone();
    assert!(!buggy.check_invariant("FailedSwapNeverReplacesOld", &torn));
    failed_sentinel.confirm().unwrap();
    crate::manifest::FailedMark::clear(&failed_staging.trial());
    disk_model_step(&model, &mut failed_state, "SwapFailsAndDisarms");
    assert_real_disk_projection(
        &failed_state,
        &failed_staging,
        &failed_installed,
        build,
        commit,
        &digest,
    );
    let _ = std::fs::remove_dir_all(failed_root);

    let (staging, root) = temp_staging();
    write_ready(&staging, build);
    make_app(&staging.staged_app, "OLD");
    let installed = root.join("Applications/aterm.app");
    let fixed = rollback_path(&installed);
    make_app(&installed, "OLD");
    make_app(&fixed, "NEW");
    let mut state = disk_model_ready(&model);
    disk_model_step(&model, &mut state, "RemovePreviousReceipt");
    disk_model_step(&model, &mut state, "PrepareFixedNew");
    assert_real_disk_projection(&state, &staging, &installed, build, commit, &digest);

    let ready = Ready::read(&staging.ready).unwrap();
    let _sentinel = prepare_trial(&staging, &ready, &installed).unwrap();
    disk_model_step(&model, &mut state, "ArmExactTrial");
    assert_real_disk_projection(&state, &staging, &installed, build, commit, &digest);
    let early_receipt = buggy.successors("BuggyWriteReceiptBeforeSwap", &state)[0].clone();
    assert!(!buggy.check_invariant("ReceiptBindsExactNewIdentity", &early_receipt));

    swap_fixed_candidate(
        &copied_candidate(&fixed),
        &installed,
        aterm_durable::Flush::Durable,
    )
    .unwrap();
    disk_model_step(&model, &mut state, "AtomicSwap");
    assert_real_disk_projection(&state, &staging, &installed, build, commit, &digest);

    crate::manifest::InstalledReceipt::record(
        &staging.installed_receipt(),
        build,
        commit,
        &digest,
        aterm_durable::Flush::Durable,
    )
    .unwrap();
    std::fs::remove_file(&staging.ready).unwrap();
    disk_model_step(&model, &mut state, "RecordExactReceipt");
    disk_model_step(&model, &mut state, "VerifyExactRollback");
    assert_real_disk_projection(&state, &staging, &installed, build, commit, &digest);

    disk_model_step(&model, &mut state, "PresentInstalledUi");
    let _apply = FileLock::acquire(&staging.apply_lock).unwrap();
    assert!(!confirm_health_under_apply_lock_with_proof(
        &staging,
        build,
        &installed,
        || Err("injected health-proof failure".to_string()),
    ));
    disk_model_step(&model, &mut state, "HealthProofFails");
    assert_real_disk_projection(&state, &staging, &installed, build, commit, &digest);
    assert!(staging.staged_app.exists() && fixed.exists());

    disk_model_step(&model, &mut state, "RetryHealthProof");
    assert!(confirm_health_under_apply_lock_with_proof(
        &staging,
        build,
        &installed,
        || Ok(()),
    ));
    for action in [
        "ProveInstalledHealth",
        "DisarmTrial",
        "GarbageCollectRollback",
    ] {
        disk_model_step(&model, &mut state, action);
    }
    assert_real_disk_projection(&state, &staging, &installed, build, commit, &digest);
    assert!(!staging.staged_app.exists() && read_id(&fixed) == "OLD");
    let me = TrialLauncher::this_process().unwrap();
    assert_eq!(
        KeptPredecessor::read(&staging),
        Some(KeptPredecessor {
            holder: me,
            bundle: fixed.clone()
        }),
        "kept for the window that started from it"
    );
    release_kept_predecessor_under_apply_lock(&staging, |_| Some(false));
    assert_eq!(read_id(&fixed), "OLD", "kept while its window runs");
    let gone = KeptPredecessor {
        holder: TrialLauncher {
            pid: me.pid,
            started: (1, 0),
        },
        bundle: fixed.clone(),
    };
    gone.record(&staging).unwrap();
    release_kept_predecessor_under_apply_lock(&staging, |_| Some(true));
    release_kept_predecessor_under_apply_lock(&staging, |_| None);
    assert_eq!(
        read_id(&fixed),
        "OLD",
        "kept while a process runs from it, or none can be read"
    );
    release_kept_predecessor_under_apply_lock(&staging, |_| Some(false));
    disk_model_step(&model, &mut state, "ReleaseKeptPredecessor");
    assert_real_disk_projection(&state, &staging, &installed, build, commit, &digest);
    assert!(!fixed.exists() && KeptPredecessor::read(&staging).is_none());
    drop(_apply);
    let _ = std::fs::remove_dir_all(root);

    let running = build - 1;
    let launch_desk = || {
        let (s, root) = temp_staging();
        write_ready(&s, build);
        make_app(&s.staged_app, "NEW");
        std::fs::create_dir_all(s.staged_app.join("Contents")).unwrap();
        std::fs::write(
            s.staged_app.join("Contents/Info.plist"),
            format!(
                "<plist><dict><key>CFBundleVersion</key><string>{build}</string>\
                 <key>ATermGitCommit</key><string>{commit}</string></dict></plist>"
            ),
        )
        .unwrap();
        let installed = root.join("Applications/aterm.app");
        make_app(&installed, "OLD");
        (s, root, installed)
    };
    let launch_swap = |s: &Staging, installed: &Path, expected: &ExpectedArtifact| {
        let read = |path: &Path| -> Result<(u64, String), String> {
            if path == installed {
                Ok((running, other_commit.to_string()))
            } else {
                Ok((build, commit.to_string()))
            }
        };
        let read_both = |[a, b]: [&Path; 2]| [read(a), read(b)];
        let starts = |_: &Path, _: u64| -> Result<(), String> { Ok(()) };
        swap_staged_for_launch_at(
            s,
            installed,
            running,
            Some(other_commit),
            expected,
            &SwapSeams {
                one: &read,
                both: &read_both,
                probe: &starts,
            },
        )
    };
    let (s, root, installed) = launch_desk();
    let mut state = disk_model_ready(&model);
    assert_real_disk_projection(&state, &s, &installed, build, commit, &digest);
    let authorized = ExpectedArtifact::from_parts(build, commit, &digest).unwrap();
    assert_eq!(launch_swap(&s, &installed, &authorized), Ok(build));
    for action in [
        "RemovePreviousReceipt",
        "PrepareFixedNew",
        "ArmExactTrial",
        "AtomicSwap",
        "RecordExactReceipt",
    ] {
        disk_model_step(&model, &mut state, action);
    }
    assert_real_disk_projection(&state, &s, &installed, build, commit, &digest);
    assert!(
        FileLock::acquire_within(&s.apply_lock, std::time::Duration::ZERO).is_ok(),
        "the parent lets the apply lock go before it launches: the successor's boot \
         apply takes it to count its launch"
    );
    assert_eq!(crate::verify::current_budget_deadline(), None);
    assert!(
        !s.reexec_stamp().exists(),
        "no re-exec stamp: nothing execs on this lane"
    );
    let _ = std::fs::remove_dir_all(root);

    let (s, root, installed) = launch_desk();
    let other_artifact = ExpectedArtifact::from_parts(build, commit, &"cd".repeat(32)).unwrap();
    assert!(matches!(
        launch_swap(&s, &installed, &other_artifact),
        Err(ApplyOutcome::Deferred(_))
    ));
    assert_real_disk_projection(
        &disk_model_ready(&model),
        &s,
        &installed,
        build,
        commit,
        &digest,
    );
    let _ = std::fs::remove_dir_all(root);

    fn verifier(
        installed: &Path,
        build: u64,
        commit: &str,
        fixed: Result<(u64, String), String>,
    ) -> impl Fn(&Path) -> Result<(u64, String), String> + use<> {
        let installed = installed.to_path_buf();
        let commit = commit.to_string();
        move |path: &Path| {
            if path == installed.as_path() {
                Ok((build, commit.clone()))
            } else {
                fixed.clone()
            }
        }
    }
    let verdict = || -> Result<(u64, String), String> {
        Err(
            "bundle policy: codesign --verify (team-pinned requirement) failed: code failed \
             to satisfy specified code requirement(s)"
                .to_string(),
        )
    };
    let swapped_trial = || {
        let (s, root) = temp_staging();
        write_ready(&s, build);
        let installed = root.join("Applications/aterm.app");
        let fixed = rollback_path(&installed);
        make_app(&installed, "OLD");
        make_app(&fixed, "NEW");
        let mut state = disk_model_ready(&model);
        disk_model_step(&model, &mut state, "RemovePreviousReceipt");
        disk_model_step(&model, &mut state, "PrepareFixedNew");
        let ready = Ready::read(&s.ready).unwrap();
        prepare_trial(&s, &ready, &installed).unwrap();
        disk_model_step(&model, &mut state, "ArmExactTrial");
        swap_fixed_candidate(
            &copied_candidate(&fixed),
            &installed,
            aterm_durable::Flush::Durable,
        )
        .unwrap();
        disk_model_step(&model, &mut state, "AtomicSwap");
        crate::manifest::InstalledReceipt::record(
            &s.installed_receipt(),
            build,
            commit,
            &digest,
            aterm_durable::Flush::Durable,
        )
        .unwrap();
        std::fs::remove_file(&s.ready).unwrap();
        disk_model_step(&model, &mut state, "RecordExactReceipt");
        assert_real_disk_projection(&state, &s, &installed, build, commit, &digest);
        (s, root, installed, state)
    };
    let counted_launch = |s: &Staging, installed: &Path, verify: &IdentityReader| {
        let resolved = installed.to_path_buf();
        check_boot_health_with_proof(
            s,
            build,
            RollbackHandoff::NONE,
            false,
            crate::TrialWitness::Window,
            std::time::Duration::from_millis(100),
            move || {
                Some(bundle::Bundle {
                    exe: resolved.join("Contents/MacOS/aterm"),
                    app_root: resolved,
                })
            },
            |root| prove_current_trial(s, root, build, Some(commit), verify),
        )
    };

    let (s, root, installed, mut state) = swapped_trial();
    let verifies = verifier(
        &installed,
        build,
        commit,
        Ok((build - 1, commit.to_string())),
    );
    assert!(matches!(
        fixed_predecessor(&installed, build, &verifies),
        Predecessor::Revertible(_)
    ));
    disk_model_step(&model, &mut state, "VerifyExactRollback");
    assert!(
        model
            .successors("RunningVerifierRefusesRetainedOld", &state)
            .is_empty()
    );
    assert!(model.successors("SettleWithoutRevert", &state).is_empty());
    let discarded = buggy.successors("BuggySettleOverVerifiedRollback", &state)[0].clone();
    assert!(!buggy.check_invariant("GarbageCollectionRequiresProofAndDisarm", &discarded));
    assert!(!buggy.check_invariant(
        "SettleWithoutRevertOnlyOverAnUnrevertiblePredecessor",
        &discarded
    ));
    let outcome = counted_launch(&s, &installed, &verifies);
    assert!(
        matches!(outcome, Some(ApplyOutcome::NotApplicable)),
        "{outcome:?}"
    );
    assert_real_disk_projection(&state, &s, &installed, build, commit, &digest);
    let _ = std::fs::remove_dir_all(root);

    let (s, root, installed, state) = swapped_trial();
    let unfinished = verifier(
        &installed,
        build,
        commit,
        Err(format!(
            "codesign --verify ran out of time ({})",
            crate::PASSING_REFUSAL_KEY
        )),
    );
    assert!(matches!(
        fixed_predecessor(&installed, build, &unfinished),
        Predecessor::Unproven(_)
    ));
    let outcome = counted_launch(&s, &installed, &unfinished);
    assert!(
        matches!(&outcome, Some(ApplyOutcome::Deferred(why)) if why.contains("trial recovery proof")),
        "{outcome:?}"
    );
    assert_real_disk_projection(&state, &s, &installed, build, commit, &digest);
    let _ = std::fs::remove_dir_all(root);

    let (s, root, installed, mut state) = swapped_trial();
    let refuses = verifier(&installed, build, commit, verdict());
    assert!(matches!(
        fixed_predecessor(&installed, build, &refuses),
        Predecessor::Unavailable(_)
    ));
    disk_model_step(&model, &mut state, "RunningVerifierRefusesRetainedOld");
    assert!(model.successors("VerifyExactRollback", &state).is_empty());
    assert!(
        model.successors("DetectCrashLoop", &state).is_empty(),
        "no revert is available over a refused predecessor"
    );
    let outcome = counted_launch(&s, &installed, &refuses);
    assert!(outcome.is_none(), "{outcome:?}");
    assert_eq!(
        boot_sentinel(&s).read_state(),
        None,
        "SettleWithoutRevert disarms"
    );
    disk_model_step(&model, &mut state, "SettleWithoutRevert");
    assert_real_disk_projection(&state, &s, &installed, build, commit, &digest);
    let _ = std::fs::remove_dir_all(root);

    let (s, root, installed, mut state) = swapped_trial();
    let refuses = verifier(&installed, build, commit, verdict());
    disk_model_step(&model, &mut state, "RunningVerifierRefusesRetainedOld");
    disk_model_step(&model, &mut state, "PresentInstalledUi");
    let apply = FileLock::acquire(&s.apply_lock).unwrap();
    assert!(confirm_health_under_apply_lock_with_proof(
        &s,
        build,
        &installed,
        || prove_current_trial(&s, &installed, build, Some(commit), &refuses).map(|_| ()),
    ));
    for action in [
        "ProveInstalledHealth",
        "DisarmTrial",
        "GarbageCollectRollback",
    ] {
        disk_model_step(&model, &mut state, action);
    }
    assert_real_disk_projection(&state, &s, &installed, build, commit, &digest);
    drop(apply);
    let _ = std::fs::remove_dir_all(root);

    let (rollback_staging, rollback_root) = temp_staging();
    write_ready(&rollback_staging, build);
    let rollback_installed = rollback_root.join("Applications/aterm.app");
    let rollback_fixed = rollback_path(&rollback_installed);
    make_app(&rollback_installed, "OLD");
    make_app(&rollback_fixed, "NEW");
    let rollback_ready = Ready::read(&rollback_staging.ready).unwrap();
    let rollback_sentinel =
        prepare_trial(&rollback_staging, &rollback_ready, &rollback_installed).unwrap();
    let mut rollback_state = disk_model_ready(&model);
    disk_model_step(&model, &mut rollback_state, "RemovePreviousReceipt");
    for action in ["PrepareFixedNew", "ArmExactTrial"] {
        disk_model_step(&model, &mut rollback_state, action);
    }
    swap_fixed_candidate(
        &copied_candidate(&rollback_fixed),
        &rollback_installed,
        aterm_durable::Flush::Durable,
    )
    .unwrap();
    disk_model_step(&model, &mut rollback_state, "AtomicSwap");
    crate::manifest::InstalledReceipt::record(
        &rollback_staging.installed_receipt(),
        build,
        commit,
        &digest,
        aterm_durable::Flush::Durable,
    )
    .unwrap();
    for action in ["RecordExactReceipt", "VerifyExactRollback", "ExecFails"] {
        disk_model_step(&model, &mut rollback_state, action);
    }
    assert!(
        restore_rollback(
            &rollback_root.join("missing-rollback.app"),
            &rollback_installed,
            aterm_durable::Flush::Durable,
        )
        .is_err()
    );
    disk_model_step(&model, &mut rollback_state, "RestoreExactOldFails");
    assert_real_disk_projection(
        &rollback_state,
        &rollback_staging,
        &rollback_installed,
        build,
        commit,
        &digest,
    );
    restore_rollback(
        &rollback_fixed,
        &rollback_installed,
        aterm_durable::Flush::Durable,
    )
    .unwrap();
    disk_model_step(&model, &mut rollback_state, "RestoreExactOld");
    rollback_sentinel.confirm().unwrap();
    crate::manifest::FailedMark::clear(&rollback_staging.trial());
    std::fs::remove_dir_all(&rollback_fixed).unwrap();
    restore_installed_receipt(&rollback_staging, None, aterm_durable::Flush::Durable).unwrap();
    disk_model_step(
        &model,
        &mut rollback_state,
        "DisarmRestoredTrialAndClearUnboundReceipt",
    );
    assert_real_disk_projection(
        &rollback_state,
        &rollback_staging,
        &rollback_installed,
        build,
        commit,
        &digest,
    );
    let _ = std::fs::remove_dir_all(rollback_root);
}

/// One central directory file header for `name`, declaring `uncompressed`
fn central_header(name: &str, uncompressed: u32, extra: &[u8]) -> Vec<u8> {
    let mut header = Vec::new();
    header.extend_from_slice(&CENTRAL_HEADER_SIGNATURE.to_le_bytes());
    header.extend_from_slice(&[0u8; 4]);
    header.extend_from_slice(&[0u8; 2]);
    header.extend_from_slice(&[0u8; 2]);
    header.extend_from_slice(&[0u8; 4]);
    header.extend_from_slice(&[0u8; 4]);
    header.extend_from_slice(&0u32.to_le_bytes());
    header.extend_from_slice(&uncompressed.to_le_bytes());
    header.extend_from_slice(&u16::try_from(name.len()).unwrap().to_le_bytes());
    header.extend_from_slice(&u16::try_from(extra.len()).unwrap().to_le_bytes());
    header.extend_from_slice(&0u16.to_le_bytes());
    header.extend_from_slice(&[0u8; 2]);
    header.extend_from_slice(&[0u8; 2]);
    header.extend_from_slice(&[0u8; 4]);
    header.extend_from_slice(&0u32.to_le_bytes());
    assert_eq!(
        header.len(),
        CENTRAL_HEADER_FIXED_LEN,
        "fixed header layout"
    );
    header.extend_from_slice(name.as_bytes());
    header.extend_from_slice(extra);
    header
}

/// An End Of Central Directory record with the given (already-encoded)
fn eocd(entries: u16, size: u32, offset: u32, comment: &[u8]) -> Vec<u8> {
    let mut record = Vec::new();
    record.extend_from_slice(&EOCD_SIGNATURE.to_le_bytes());
    record.extend_from_slice(&0u16.to_le_bytes());
    record.extend_from_slice(&0u16.to_le_bytes());
    record.extend_from_slice(&entries.to_le_bytes());
    record.extend_from_slice(&entries.to_le_bytes());
    record.extend_from_slice(&size.to_le_bytes());
    record.extend_from_slice(&offset.to_le_bytes());
    record.extend_from_slice(&u16::try_from(comment.len()).unwrap().to_le_bytes());
    record.extend_from_slice(comment);
    record
}

/// A stand-in for the local-file-header area, so the directory offset under
const ZIP_PREFIX: &[u8] = b"PK\x03\x04stub!!";

/// Write `bytes` to a fresh temp path and hand back a guard that removes it.
struct TempZip(std::path::PathBuf);

impl TempZip {
    fn new(label: &str, bytes: &[u8]) -> Self {
        let n = SEQ.fetch_add(1, Ordering::Relaxed);
        let path = std::env::temp_dir().join(format!(
            "aterm-zipbound-{label}-{}-{n}.zip",
            std::process::id()
        ));
        std::fs::write(&path, bytes).unwrap();
        Self(path)
    }
}

impl Drop for TempZip {
    fn drop(&mut self) {
        let _ = std::fs::remove_file(&self.0);
    }
}

/// Assemble prefix + headers + an EOCD whose extent and count match them.
fn well_formed_zip(entries: &[(&str, u32)]) -> Vec<u8> {
    let mut bytes = ZIP_PREFIX.to_vec();
    let offset = u32::try_from(bytes.len()).unwrap();
    for &(name, uncompressed) in entries {
        bytes.extend_from_slice(&central_header(name, uncompressed, &[]));
    }
    let size = u32::try_from(bytes.len()).unwrap() - offset;
    let count = u16::try_from(entries.len()).unwrap();
    bytes.extend_from_slice(&eocd(count, size, offset, b""));
    bytes
}

/// What the claim admits, read exactly — entry count and summed uncompressed bytes:
#[test]
fn zip_claim_reads_every_directory_a_publisher_could_write() {
    assert_eq!(
        MAX_EXTRACT_BYTES,
        aterm_update_core::RELEASE_ASSET_DOWNLOAD_BOUND,
        "the extract cap must never sit below the bound the cutter enforces"
    );
    let commented = {
        let mut bytes = ZIP_PREFIX.to_vec();
        let offset = u32::try_from(bytes.len()).unwrap();
        bytes.extend_from_slice(&central_header("only", 7, &[]));
        let size = u32::try_from(bytes.len()).unwrap() - offset;
        bytes.extend_from_slice(&eocd(1, size, offset, &[b'c'; 4096]));
        bytes
    };
    let batteries: u32 = 1_610_612_736;
    for (label, bytes, entries, uncompressed_bytes) in [
        (
            "ok",
            well_formed_zip(&[
                ("aterm.app/", 0),
                ("aterm.app/Contents/Info.plist", 1_024),
                ("aterm.app/Contents/MacOS/aterm", 40_000_000),
            ]),
            3,
            40_001_024,
        ),
        ("comment", commented, 1, 7),
        (
            "batteries",
            well_formed_zip(&[(
                "aterm.app/Contents/Resources/toolchain-seed/trust.tar.zst",
                batteries,
            )]),
            1,
            u64::from(batteries),
        ),
    ] {
        let zip = TempZip::new(label, &bytes);
        assert_eq!(
            checked_zip_extraction_claim(&zip.0),
            Ok(ZipExtractClaim {
                entries,
                uncompressed_bytes,
            }),
            "{label}"
        );
    }
}

/// What the claim refuses, each for its own reason. These build central directories
#[test]
fn zip_claim_refuses_every_directory_it_cannot_bound() {
    let over = usize::try_from(MAX_EXTRACT_ENTRIES).unwrap() + 1;
    let many = {
        let mut bytes = ZIP_PREFIX.to_vec();
        let offset = u32::try_from(bytes.len()).unwrap();
        for _ in 0..over {
            bytes.extend_from_slice(&central_header("e", 0, &[]));
        }
        let size = u32::try_from(bytes.len()).unwrap() - offset;
        bytes.extend_from_slice(&eocd(u16::try_from(over).unwrap(), size, offset, b""));
        bytes
    };
    let cut = {
        let mut bytes = ZIP_PREFIX.to_vec();
        let offset = u32::try_from(bytes.len()).unwrap();
        bytes.extend_from_slice(&central_header("first", 10, &[]));
        let full = central_header("second", 20, &[]);
        let present = u32::try_from(bytes.len()).unwrap() - offset;
        let size = present + u32::try_from(full.len()).unwrap();
        bytes.extend_from_slice(&full[..full.len() / 2]);
        bytes.extend_from_slice(&eocd(2, size, offset, b""));
        bytes
    };
    let miscount = {
        let mut bytes = ZIP_PREFIX.to_vec();
        let offset = u32::try_from(bytes.len()).unwrap();
        bytes.extend_from_slice(&central_header("one", 1, &[]));
        bytes.extend_from_slice(&central_header("two", 2, &[]));
        let size = u32::try_from(bytes.len()).unwrap() - offset;
        bytes.extend_from_slice(&eocd(7, size, offset, b""));
        bytes
    };
    let sentinel = |entries: u16, size: u32, offset: u32| {
        let mut bytes = ZIP_PREFIX.to_vec();
        bytes.extend_from_slice(&central_header("one", 1, &[]));
        bytes.extend_from_slice(&eocd(entries, size, offset, b""));
        bytes
    };
    let ambiguous = {
        let mut bytes = ZIP_PREFIX.to_vec();
        let offset = u32::try_from(bytes.len()).unwrap();
        bytes.extend_from_slice(&central_header("one", 1, &[]));
        let size = u32::try_from(bytes.len()).unwrap() - offset;
        let real = eocd(1, size, offset, b"");
        let filler = vec![0u8; real.len()];
        bytes.extend_from_slice(&eocd(1, size, offset, &filler));
        let decoy_comment_start = bytes.len() - real.len();
        bytes[decoy_comment_start..].copy_from_slice(&real);
        bytes
    };
    let cases: [(&str, Vec<u8>, &[&str]); 10] = [
        (
            "huge",
            well_formed_zip(&[("aterm.app/huge", u32::MAX - 1)]),
            &["extraction cap"],
        ),
        ("many", many, &["entries"]),
        (
            "cut",
            cut,
            &[
                "does not lie before its end record",
                "ends inside a file header",
            ],
        ),
        ("miscount", miscount, &["end record declares"]),
        (
            "sentinel-count",
            sentinel(u16::MAX, 46, 12),
            &["carries no ZIP64 one"],
        ),
        (
            "sentinel-size",
            sentinel(1, u32::MAX, 12),
            &["carries no ZIP64 one"],
        ),
        (
            "sentinel-offset",
            sentinel(1, 46, u32::MAX),
            &["carries no ZIP64 one"],
        ),
        (
            "escape",
            well_formed_zip(&[("aterm.app/x", u32::MAX)]),
            &["carries no ZIP64 extra field"],
        ),
        (
            "ambiguous",
            ambiguous,
            &["more than one end-of-central-directory record"],
        ),
        (
            "stub",
            b"PK\x03\x04not-a-zip".to_vec(),
            &["end-of-central-directory"],
        ),
    ];
    for (label, bytes, reasons) in cases {
        let zip = TempZip::new(label, &bytes);
        let error =
            checked_zip_extraction_claim(&zip.0).expect_err(&format!("{label}: must refuse"));
        assert!(
            reasons.iter().any(|reason| error.contains(reason)),
            "{label}: refused for another reason: {error}"
        );
    }
}

/// This machine's free space is checked before `ditto` is spawned: a declared
#[test]
fn the_stage_refuses_an_unpack_the_volume_cannot_hold() {
    let (_s, root) = temp_staging();
    let free = free_bytes_at(&root).expect("the scratch volume reports its free space");
    refuse_without_room(&root, free.saturating_add(1 << 40), "the update zip")
        .expect_err("a declared size past the volume is refused before any byte lands");
    refuse_without_room(&root, 1024, "the update zip").expect("a small unpack fits");
    refuse_without_room(
        Path::new("/nonexistent/volume/for/this/test"),
        u64::MAX / 2,
        "the update zip",
    )
    .expect("an unanswerable volume defers to ditto");
    assert!(free_bytes_at(Path::new("/nonexistent/volume/for/this/test")).is_none());
}

/// `tree_bytes` is what the DMG lane's copy will write: regular files summed,
#[test]
fn tree_bytes_sums_regular_files_and_never_follows_links() {
    let (_s, root) = temp_staging();
    let tree = root.join("tree.app");
    std::fs::create_dir_all(tree.join("Contents/MacOS")).unwrap();
    std::fs::write(tree.join("Contents/MacOS/aterm"), vec![7u8; 1000]).unwrap();
    std::fs::write(tree.join("Contents/Info.plist"), vec![1u8; 24]).unwrap();
    let big = root.join("big.bin");
    std::fs::write(&big, vec![0u8; 100_000]).unwrap();
    std::os::unix::fs::symlink(&big, tree.join("Contents/MacOS/link")).unwrap();
    assert_eq!(tree_bytes(&tree), 1024);
    assert_eq!(tree_bytes(&root.join("absent")), 0);
}

/// Both lanes sweep both kinds of scratch: a `mnt-<pid>` a DMG stage left behind
#[test]
fn the_zip_lane_reclaims_a_dmg_era_mountpoint() {
    let (s, root) = temp_staging();
    let stale_mount = s.root.join("mnt-424242");
    std::fs::create_dir_all(&stale_mount).unwrap();
    let stale_extract = s.staged_dir().join("zx-424242");
    std::fs::create_dir_all(&stale_extract).unwrap();
    let zip = TempZip::new("sweep", &well_formed_zip(&[("aterm.app/", 0)]));
    let manifest = Manifest {
        schema: 1,
        version: "0.0.9".into(),
        build_number: 9,
        commit: Some("0123456789abcdef0123456789abcdef01234567".into()),
        sha256: "ab".repeat(32),
        dmg: "aterm-0.0.9.dmg".into(),
        url: None,
        zip: Some("aterm-0.0.9-mac.zip".into()),
        zip_sha256: Some("cd".repeat(32)),
        min_build: None,
        machine_id: None,
        roster_seq: None,
        changelog: None,
    };
    let outcome = stage_from_zip(&s, &zip.0, &manifest, "", &now_rfc3339());
    assert!(
        outcome.is_err(),
        "a synthetic central directory has nothing ditto can unpack: {outcome:?}"
    );
    assert!(
        !stale_extract.exists(),
        "the zip lane reclaims its own leftover extract dir"
    );
    assert!(
        !stale_mount.exists(),
        "the DMG lane's leftover mountpoint must be reclaimed by the zip lane too"
    );
    let _ = std::fs::remove_dir_all(root);
}

/// A window's settled trial keeps its predecessor at the fixed path for that window:
#[test]
fn a_kept_predecessor_is_released_only_after_its_window_and_outside_a_trial() {
    let (staging, root) = temp_staging();
    let installed = root.join("Applications/aterm.app");
    let fixed = rollback_path(&installed);
    make_app(&installed, "NEW");
    make_app(&fixed, "OLD");
    let gone = KeptPredecessor {
        holder: TrialLauncher {
            pid: std::process::id(),
            started: (1, 0),
        },
        bundle: fixed.clone(),
    };

    keep_settled_predecessor(&staging, &fixed);
    assert_eq!(
        KeptPredecessor::read(&staging).map(|kept| kept.holder),
        TrialLauncher::this_process()
    );
    let held = FileLock::acquire(&staging.apply_lock).unwrap();
    release_kept_predecessor(&staging, std::time::Duration::ZERO, |_| Some(false));
    assert_eq!(
        read_id(&fixed),
        "OLD",
        "its window runs: no lock, no release"
    );

    gone.record(&staging).unwrap();
    boot_sentinel(&staging).arm(7).unwrap();
    release_kept_predecessor_under_apply_lock(&staging, |_| Some(false));
    assert_eq!(
        read_id(&fixed),
        "OLD",
        "an armed trial's rollback is never released"
    );
    boot_sentinel(&staging).confirm().unwrap();

    std::fs::rename(&fixed, root.join("elsewhere")).unwrap();
    release_kept_predecessor_under_apply_lock(&staging, |_| Some(false));
    assert!(
        KeptPredecessor::read(&staging).is_none(),
        "its bundle is gone"
    );
    assert_eq!(read_id(&root.join("elsewhere")), "OLD");

    let installed_named = KeptPredecessor {
        bundle: installed.clone(),
        ..gone.clone()
    }
    .encode();
    write_private_file(&kept_predecessor_path(&staging), installed_named.as_bytes()).unwrap();
    assert!(KeptPredecessor::read(&staging).is_none());
    release_kept_predecessor_under_apply_lock(&staging, |_| Some(false));
    assert_eq!(
        read_id(&installed),
        "NEW",
        "only a fixed rollback path is ever named"
    );

    make_app(&fixed, "OLD");
    gone.record(&staging).unwrap();
    write_ready(&staging, 9);
    make_app(&staging.staged_app, "NEWER");
    let ready = Ready::read(&staging.ready).unwrap();
    let commit = ready.commit.clone().unwrap();
    prepare_fixed_swap_candidate(&staging, &installed, &ready, 8, (9, &commit)).unwrap();
    assert!(KeptPredecessor::read(&staging).is_none());
    assert_eq!(read_id(&fixed), "NEWER", "the path now holds the candidate");
    release_kept_predecessor_under_apply_lock(&staging, |_| Some(false));
    assert_eq!(read_id(&fixed), "NEWER");
    drop(held);
    let _ = std::fs::remove_dir_all(root);
}

/// [`process_uses`] reads the live process list: this test's own executable is under its
#[test]
fn process_uses_sees_a_running_executable_and_not_an_unused_directory() {
    let exe = std::env::current_exe().unwrap();
    assert_eq!(process_uses(exe.parent().unwrap()), Some(true));
    let (_staging, root) = temp_staging();
    let unused = root.join("unused");
    std::fs::create_dir_all(&unused).unwrap();
    assert_ne!(process_uses(&unused), Some(true));
    let _ = std::fs::remove_dir_all(root);
}

/// A path scan is clear only when it found no user and no unreadable process that may
#[test]
fn a_kept_bundle_is_not_unused_when_a_process_that_may_run_from_it_could_not_be_read() {
    let dir = Path::new("/Applications/aterm.app.rollback");
    let held = || aterm_uds::holders::PathUse {
        pid: 17,
        how: "exe",
        path: dir.join("Contents/MacOS/aterm"),
    };
    let scan = |users, unreadable| aterm_uds::holders::PathScan {
        users,
        unreadable,
        scanned: 2,
    };
    assert_eq!(
        path_scan_uses(scan(Vec::new(), Vec::new()), dir),
        Some(false)
    );
    assert_eq!(
        path_scan_uses(scan(vec![held()], Vec::new()), dir),
        Some(true)
    );
    assert_eq!(
        path_scan_uses(scan(vec![held()], vec![(18, None)]), dir),
        Some(true),
        "a known user keeps the bundle even when another process is unreadable"
    );

    let uncertain = [
        path_scan_uses(scan(Vec::new(), vec![(18, None)]), dir),
        path_scan_uses(
            scan(
                Vec::new(),
                vec![(19, Some(dir.join("Contents/MacOS/aterm")))],
            ),
            dir,
        ),
    ];
    assert_eq!(
        uncertain,
        [None, None],
        "KEPT_SCAN_TREATED_UNREADABLE_AS_CLEAR"
    );
    assert_eq!(
        path_scan_uses(
            scan(
                Vec::new(),
                vec![(20, Some(PathBuf::from("/usr/libexec/lsd")))]
            ),
            dir
        ),
        Some(false)
    );
}

/// The kept record names the very bundle whose window confirmed: path display text is
#[test]
fn a_kept_predecessors_record_preserves_its_exact_path_and_refuses_extra_input() {
    use std::os::unix::ffi::OsStringExt;

    let record = |bundle| KeptPredecessor {
        holder: TrialLauncher {
            pid: 4242,
            started: (17, 23),
        },
        bundle,
    };
    for path in [
        "/Applications/aterm.app.rollback",
        "/private/Two Words/aterm.app.rollback",
        "/private/构建/aterm.app.rollback",
    ] {
        let kept = record(PathBuf::from(path));
        assert_eq!(KeptPredecessor::parse(&kept.encode()), Some(kept));
    }
    for malformed in [
        "",
        "not a record",
        "4242 extra 23\n/Applications/aterm.app.rollback\n",
    ] {
        assert!(KeptPredecessor::parse(malformed).is_none());
    }

    let unusual = [
        PathBuf::from("/private/custody/aterm.app.rollback\n/Applications/aterm.app.rollback"),
        PathBuf::from(std::ffi::OsString::from_vec(
            b"/private/non-utf8-\xff/Applications/aterm.app.rollback".to_vec(),
        )),
    ];
    let roundtrips: Vec<_> = unusual
        .into_iter()
        .map(|path| {
            let exe = path
                .with_file_name("aterm.app")
                .join("Contents/MacOS/aterm");
            assert!(
                bundle::resolve_from(&exe).is_some(),
                "the real layout admits this ancestor"
            );
            let kept = record(path);
            (KeptPredecessor::parse(&kept.encode()), kept)
        })
        .collect();
    let ordinary = record(PathBuf::from("/Applications/aterm.app.rollback"));
    let encoded = ordinary.encode();
    let mut lines = encoded.lines();
    let header = lines.next().unwrap();
    let path = lines.next().unwrap();
    let malformed = [
        format!("{encoded}another record\n"),
        format!("{header} extra\n{path}\n"),
        format!("{header}\n{path}x\n"),
        format!("{header}\n\n"),
        "4242 17 23\n/private/custody/aterm.app.rollback\n/Applications/aterm.app.rollback\n"
            .to_string(),
        record(PathBuf::from("/Applications/aterm.app")).encode(),
        record(PathBuf::from("relative/aterm.app.rollback")).encode(),
        record(PathBuf::from(std::ffi::OsString::from_vec(
            b"/private/nul-\0/aterm.app.rollback".to_vec(),
        )))
        .encode(),
    ];
    let all_roundtrip = roundtrips
        .iter()
        .all(|(got, expected)| got.as_ref() == Some(expected));
    let all_refused = malformed
        .iter()
        .all(|text| KeptPredecessor::parse(text).is_none());
    assert!(
        all_roundtrip && all_refused,
        "KEPT_PREDECESSOR_RECORD_CHANGED_ITS_PATH: {roundtrips:?}"
    );
}

/// An unreadable holder is not an ended one: its successor image is outside the kept
#[test]
fn a_kept_predecessor_outlives_an_unreadable_holder() {
    use aterm_uds::holders::Exe;
    use std::cell::Cell;

    let holder = TrialLauncher {
        pid: 4242,
        started: (17, 23),
    };
    assert!(!holder.has_ended_with(
        |pid| {
            assert_eq!(pid, holder.pid);
            Some(holder)
        },
        |_| panic!("same birth needs no executable observation"),
    ));
    let replacement = TrialLauncher {
        started: (18, 24),
        ..holder
    };
    assert!(holder.has_ended_with(
        |_| Some(replacement),
        |_| panic!("a different birth already proves the holder ended"),
    ));
    let reads = Cell::new(0);
    let ended = |exe| {
        holder.has_ended_with(
            |pid| {
                assert_eq!(pid, holder.pid);
                None
            },
            |pid| {
                assert_eq!(pid, holder.pid);
                reads.set(reads.get() + 1);
                exe
            },
        )
    };
    assert!(ended(Exe::Exited), "positive disappearance permits release");
    let uncertain = [
        ended(Exe::At(PathBuf::from(
            "/Applications/aterm.app/Contents/MacOS/aterm",
        ))),
        ended(Exe::Unnamed),
    ];
    assert!(
        uncertain == [false, false] && reads.get() == 3,
        "KEPT_HOLDER_UNREADABLE_TREATED_AS_ENDED: {uncertain:?}"
    );
}

/// ONE CHECK OF BOTH BUNDLES, BEFORE ANYTHING MOVES (steps 6–7,
#[test]
fn both_bundles_are_checked_at_once_before_anything_moves() {
    type Identity = Result<(u64, String), String>;
    const BUILD: u64 = 11;
    const RUNNING: u64 = 10;
    let commit = "0123456789abcdef0123456789abcdef01234567".to_string();
    let running_commit = "fedcba9876543210fedcba9876543210fedcba98";
    let accepted: Identity = Ok((BUILD, commit.clone()));
    let old: Identity = Ok((RUNNING, running_commit.into()));
    let passing = crate::verify::mark_passing("codesign did not finish within the budget".into());
    let verdict = "bundle policy: code object is not signed at all".to_string();
    #[derive(Debug, PartialEq)]
    struct Asked {
        paths: Vec<PathBuf>,
        fixed: Option<String>,
        staged: Option<String>,
    }
    let id_at = |dir: &Path| std::fs::read_to_string(dir.join("id")).ok();
    let desk = |predecessor: bool| {
        let (staging, root) = temp_staging();
        write_ready(&staging, BUILD);
        make_app(&staging.staged_app, "NEW");
        let installed = root.join("Applications/aterm.app");
        make_app(&installed, "OLD");
        if predecessor {
            make_app(&rollback_path(&installed), "PRED");
        }
        (staging, installed)
    };
    let run = |staging: &Staging, installed: &Path, stage: Identity, installed_is: Identity| {
        let ones = std::cell::RefCell::new(Vec::new());
        let boths = std::cell::RefCell::new(Vec::new());
        let fixed = rollback_path(installed);
        let ask = |paths: Vec<PathBuf>| Asked {
            paths,
            fixed: id_at(&fixed),
            staged: id_at(&staging.staged_app),
        };
        let result = verify_stage_and_prepare(
            staging,
            installed,
            &Ready::read(&staging.ready).unwrap(),
            (RUNNING, Some(running_commit)),
            |path| {
                ones.borrow_mut().push(ask(vec![path.to_path_buf()]));
                if path == staging.staged_app {
                    stage.clone()
                } else {
                    installed_is.clone()
                }
            },
            |[a, b]| {
                boths
                    .borrow_mut()
                    .push(ask(vec![a.to_path_buf(), b.to_path_buf()]));
                [stage.clone(), installed_is.clone()]
            },
        );
        (result, ones.into_inner(), boths.into_inner())
    };

    for predecessor in [true, false] {
        let (staging, installed) = desk(predecessor);
        let fixed = rollback_path(&installed);
        let (result, ones, boths) = run(&staging, &installed, accepted.clone(), old.clone());
        let verified = result.unwrap_or_else(|_| panic!("both pass: {ones:?} {boths:?}"));
        assert!(ones.is_empty(), "no bundle is checked alone: {ones:?}");
        assert_eq!(
            boths,
            [Asked {
                paths: vec![staging.staged_app.clone(), installed.clone()],
                fixed: predecessor.then(|| "PRED".into()),
                staged: Some("NEW".into()),
            }]
        );
        assert_eq!(verified.prepared.fixed, fixed);
        assert_eq!(verified.old, (RUNNING, running_commit.to_string()));
        assert_eq!(id_at(&fixed).as_deref(), Some("NEW"));
        assert_eq!(id_at(&installed).as_deref(), Some("OLD"));
    }

    for (refusal, kept) in [(passing.clone(), true), (verdict.clone(), false)] {
        let (staging, installed) = desk(true);
        let (result, _, boths) = run(&staging, &installed, Err(refusal.clone()), old.clone());
        assert!(result.is_err(), "{refusal}");
        assert_eq!(boths.len(), 1, "{refusal}");
        assert_eq!(
            id_at(&rollback_path(&installed)).as_deref(),
            Some("PRED"),
            "{refusal}: the kept predecessor survives"
        );
        assert_eq!(
            id_at(&staging.staged_app).is_some(),
            kept,
            "{refusal}: the stage where ready.toml names it, or discarded"
        );
        assert_eq!(staging.ready.exists(), kept, "{refusal}");
    }

    for installed_is in [Err(verdict.clone()), Err(passing.clone()), accepted.clone()] {
        let (staging, installed) = desk(true);
        let (result, ones, boths) =
            run(&staging, &installed, accepted.clone(), installed_is.clone());
        assert!(result.is_err(), "{installed_is:?}");
        assert!(ones.is_empty(), "{installed_is:?}: {ones:?}");
        assert_eq!(boths.len(), 1, "{installed_is:?}");
        assert_eq!(
            id_at(&rollback_path(&installed)).as_deref(),
            Some("PRED"),
            "{installed_is:?}: the kept predecessor survives a refused OLD"
        );
        assert_eq!(id_at(&staging.staged_app).as_deref(), Some("NEW"));
        assert!(
            staging.ready.exists(),
            "{installed_is:?}: the stage is kept"
        );
    }
}

#[test]
fn the_old_bundle_recheck_follows_the_operation_that_prepared_new() {
    use std::cell::RefCell;

    let running = "0123456789abcdef0123456789abcdef01234567";
    assert!(identity_matches_running(10, "0123456", 10, Some(running)));
    assert!(identity_matches_running(10, running, 10, Some(running)));

    fn exercise(
        _preliminary: bool,
        actual_move: bool,
        recheck_ok: bool,
        prepare_ok: bool,
    ) -> (Result<VerifiedSwap, ApplyOutcome>, Vec<&'static str>) {
        let events = RefCell::new(Vec::new());
        let answer = finish_swap_preparation(
            (10, "0123456".to_string()),
            || {
                events.borrow_mut().push("prepare");
                if !prepare_ok {
                    return Err("prepare refused".to_string());
                }
                Ok(PreparedSwapCandidate {
                    fixed: PathBuf::from("/model/fixed/aterm.app"),
                    moved_from_stage: actual_move,
                })
            },
            || {
                events.borrow_mut().push("recheck");
                if recheck_ok {
                    Ok((10, "0123456789abcdef0123456789abcdef01234567".to_string()))
                } else {
                    Err(ApplyOutcome::Deferred("old refused".to_string()))
                }
            },
            |prepared| {
                assert_eq!(prepared.moved_from_stage, actual_move);
                assert_eq!(prepared.fixed, PathBuf::from("/model/fixed/aterm.app"));
                events.borrow_mut().push("recover");
            },
        );
        (answer, events.into_inner())
    }

    let (moved, events) = exercise(true, true, false, true);
    let Ok(moved) = moved else {
        panic!("ordinary move refused")
    };
    assert!(moved.prepared.moved_from_stage);
    assert_eq!(moved.old, (10, "0123456".to_string()));
    assert_eq!(events, ["prepare"]);
    let (copied, events) = exercise(false, false, true, true);
    let Ok(copied) = copied else {
        panic!("ordinary copy refused")
    };
    assert!(!copied.prepared.moved_from_stage);
    assert_eq!(
        copied.old,
        (10, "0123456789abcdef0123456789abcdef01234567".to_string())
    );
    assert_eq!(events, ["prepare", "recheck"]);
    let (refused, events) = exercise(false, false, false, true);
    assert!(matches!(refused, Err(ApplyOutcome::Deferred(ref why)) if why == "old refused"));
    assert_eq!(events, ["prepare", "recheck", "recover"]);
    let (failed, events) = exercise(true, true, true, false);
    assert!(matches!(failed, Err(ApplyOutcome::Deferred(ref why)) if why == "prepare refused"));
    assert_eq!(events, ["prepare"]);

    let (copied, copy_events) = exercise(true, false, true, true);
    let copied_is_rechecked = matches!(copied, Ok(ref value)
        if !value.prepared.moved_from_stage
            && value.old == (10, "0123456789abcdef0123456789abcdef01234567".to_string()))
        && copy_events == ["prepare", "recheck"];
    let (refused, refusal_events) = exercise(true, false, false, true);
    let copied_is_recovered = matches!(refused, Err(ApplyOutcome::Deferred(ref why))
        if why == "old refused")
        && refusal_events == ["prepare", "recheck", "recover"];
    let (moved, move_events) = exercise(false, true, false, true);
    let moved_keeps_proof = matches!(moved, Ok(ref value)
        if value.prepared.moved_from_stage
            && value.old == (10, "0123456".to_string()))
        && move_events == ["prepare"];
    assert!(
        copied_is_rechecked && copied_is_recovered && moved_keeps_proof,
        "OLD_RECHECK_MUST_FOLLOW_THE_PREPARED_OPERATION"
    );
}

/// A WINDOW LAUNCH LEAVES THE STAGE FOR ITS WINDOW, AND EXECS ONLY TO REVERT OR ESCAPE
#[test]
fn a_window_launch_leaves_the_stage_for_its_window_and_execs_only_to_revert_or_escape() {
    use crate::ImageReplacement::{LaunchedSuccessor, Replaceable, WindowKeepsImage};
    use aterm_spec::interp::{State, admits, with_buggy};
    use aterm_uds::spawnfd::Command;
    let model = aterm_models_update::native_update_boot_image_model();
    let step = |state: &State, action: &str| -> State {
        let next = model.successors(action, state);
        assert_eq!(next.len(), 1, "{action} from {state:?}");
        next[0].clone()
    };
    let before = |(image, crash_loop, stage, ended, press_only, unusable): (
        crate::ImageReplacement,
        bool,
        bool,
        usize,
        bool,
        bool,
    )| {
        let mut state = model.init_state();
        if stage {
            state = step(&state, "Stage");
            for _ in 0..ended {
                state = step(&state, "PriorLeftLaunchEnded");
            }
            if unusable {
                state = step(&state, "RecordUnusable");
            }
        }
        if press_only {
            state = step(&state, "PressOnly");
        }
        if crash_loop {
            state = step(&state, "CrashLoop");
        }
        step(
            &state,
            match image {
                Replaceable => "ClassifyReplaceable",
                WindowKeepsImage => "ClassifyWindow",
                LaunchedSuccessor => "ClassifySuccessor",
            },
        )
    };
    let settings = [(false, false), (false, true), (true, false), (true, true)];
    for image in [Replaceable, WindowKeepsImage, LaunchedSuccessor] {
        for crash_loop in [false, true] {
            for stage in [false, true] {
                for ended in 0..=crate::LEFT_STAGE_ESCAPE {
                    for (press_only, unusable) in settings {
                        if !stage && (ended > 0 || unusable) {
                            continue;
                        }
                        let facts = (image, crash_loop, stage, ended, press_only, unusable);
                        let from = before(facts);
                        let mut to = from.clone();
                        let action = if crash_loop {
                            to.insert("crash_loop", 0);
                            to.insert("reverted", 1);
                            if stage {
                                to.insert("stage", 2);
                            }
                            match revert_lane(image) {
                                RevertLane::ReExec => {
                                    to.insert("execs", 1);
                                    "RevertExec"
                                }
                                RevertLane::ExitInstead => {
                                    to.insert("phase", 2);
                                    "RevertExit"
                                }
                            }
                        } else if !stage {
                            to.insert("phase", 1);
                            "BootNothing"
                        } else {
                            to.insert("phase", 1);
                            let record = if unusable {
                                LeftRecord::Unusable
                            } else {
                                LeftRecord::Ended(ended)
                            };
                            match boot_image_lane(image, !press_only, record) {
                                BootImageLane::NeverSwap => "BootNothing",
                                BootImageLane::LeaveForSession => {
                                    to.insert("left", 1);
                                    "BootLeave"
                                }
                                BootImageLane::LeaveForPress => "BootLeaveForPress",
                                BootImageLane::SwapAndExec => {
                                    to.insert("swaps", 1);
                                    to.insert("execs", 1);
                                    to.insert("stage", 2);
                                    to.insert("ended_left", 0);
                                    if image == WindowKeepsImage {
                                        to.insert("escaped", 1);
                                        if unusable {
                                            "BootRecordUnusableSwapExec"
                                        } else {
                                            "BootEscapeSwapExec"
                                        }
                                    } else {
                                        "BootSwapExec"
                                    }
                                }
                            }
                        };
                        assert_eq!(
                            model.successors(action, &from).as_slice(),
                            std::slice::from_ref(&to),
                            "{facts:?} (class, crash loop, stage, ended, press-only, unusable \
                             record) must take {action}"
                        );
                        assert_eq!(admits(&model, &from, &to), Some(action));
                        for invariant in &model.invariants {
                            assert!(
                                model.check_invariant(invariant.name, &to),
                                "{facts:?} breaks {}: {to:?}",
                                invariant.name
                            );
                        }
                    }
                }
            }
        }
    }
    let kept = before((WindowKeepsImage, false, true, 0, false, false));
    assert_eq!(
        boot_image_lane(WindowKeepsImage, true, LeftRecord::Ended(0)),
        BootImageLane::LeaveForSession
    );
    let buggy = with_buggy(&model, 1);
    let swapped = buggy.successors("BootSwapExecKeptImage", &kept)[0].clone();
    assert_eq!(admits(&model, &kept, &swapped), None);
    assert!(!buggy.check_invariant("WindowExecsOnlyToRevertOrEscape", &swapped));
    let reverting = before((LaunchedSuccessor, true, false, 0, false, false));
    assert_eq!(revert_lane(LaunchedSuccessor), RevertLane::ExitInstead);
    let buggy = with_buggy(&model, 2);
    let reexec = buggy.successors("RevertExecSuccessor", &reverting)[0].clone();
    assert_eq!(admits(&model, &reverting, &reexec), None);
    assert!(!buggy.check_invariant("LaunchedSuccessorNeverExecs", &reexec));
    let waiting = before((
        WindowKeepsImage,
        false,
        true,
        crate::LEFT_STAGE_ESCAPE,
        true,
        false,
    ));
    assert_eq!(
        boot_image_lane(
            WindowKeepsImage,
            false,
            LeftRecord::Ended(crate::LEFT_STAGE_ESCAPE)
        ),
        BootImageLane::LeaveForPress
    );
    let buggy = with_buggy(&model, 4);
    let unpressed = buggy.successors("BootEscapeSwapExecPressOnly", &waiting)[0].clone();
    assert_eq!(admits(&model, &waiting, &unpressed), None);
    assert!(!buggy.check_invariant("PressOnlyWindowExecsOnlyToRevert", &unpressed));

    let (running, build, commit, other_commit) = (
        10,
        11,
        "0123456789abcdef0123456789abcdef01234567",
        "fedcba9876543210fedcba9876543210fedcba98",
    );
    let (s, root) = temp_staging();
    let installed = root.join("Applications/aterm.app");
    assert!(matches!(
        admit_stage_under_apply_lock(&s, &installed, running, Some(other_commit), None),
        Err(ApplyOutcome::NoUpdate)
    ));
    write_ready(&s, build);
    make_app(&s.staged_app, "NEW");
    std::fs::create_dir_all(s.staged_app.join("Contents")).unwrap();
    std::fs::write(
        s.staged_app.join("Contents/Info.plist"),
        format!(
            "<plist><dict><key>CFBundleVersion</key><string>{build}</string>\
             <key>ATermGitCommit</key><string>{commit}</string></dict></plist>"
        ),
    )
    .unwrap();
    make_app(&installed, "OLD");
    let ready = Ready::read(&s.ready).expect("the stage");
    let boot = |image: crate::ImageReplacement, auto_apply: bool| {
        let admitted =
            admit_stage_under_apply_lock(&s, &installed, running, Some(other_commit), None)
                .unwrap_or_else(|refused| panic!("the stage is admitted: {refused:?}"));
        boot_lane_under_apply_lock(&s, image, auto_apply, admitted)
    };
    let assert_left = |what: &str, auto_apply: bool| {
        match boot(WindowKeepsImage, auto_apply) {
            Err(outcome) => assert_eq!(
                outcome,
                ApplyOutcome::LeftForSession {
                    build,
                    dmg_sha256: "ab".repeat(32)
                },
                "{what}"
            ),
            Ok(_) => panic!("{what}: the boot took the stage"),
        }
        assert!(
            matches!(read_ready(&s, running), ReadyState::Newer(_)),
            "{what}: the stage stays published"
        );
        assert_eq!(
            read_id(&installed),
            "OLD",
            "{what}: the install is untouched"
        );
        assert!(
            FileLock::acquire_within(&s.apply_lock, std::time::Duration::ZERO).is_ok(),
            "{what}: the apply lock is let go before the window"
        );
        assert_eq!(crate::verify::current_budget_deadline(), None, "{what}");
        assert!(!s.reexec_stamp().exists(), "{what}: nothing execs");
    };
    assert!(
        boot(Replaceable, true).is_ok(),
        "a replaceable launch hands the stage on to the boot's swap"
    );
    assert!(
        !left_stage_path(&s).exists(),
        "a replaceable launch is never on the record"
    );
    let me = TrialLauncher::this_process().expect("this process");
    assert_left("a window launch", true);
    assert_eq!(read_left_launchers(&s, &ready).unwrap(), [me]);
    assert_left("the same launch, again", true);
    assert_eq!(read_left_launchers(&s, &ready).unwrap(), [me]);
    struct StandInLaunch(aterm_uds::spawnfd::Child);
    impl Drop for StandInLaunch {
        fn drop(&mut self) {
            let _ = self.0.kill();
            let _ = self.0.wait();
        }
    }
    let ended: Vec<TrialLauncher> = (0..2)
        .map(|_| {
            let mut child = StandInLaunch(
                Command::new("/bin/sleep")
                    .arg("30")
                    .spawn()
                    .expect("start a stand-in launch"),
            );
            let launcher = TrialLauncher::of(child.0.id()).expect("a running child");
            child.0.kill().unwrap();
            child.0.wait().unwrap();
            launcher
        })
        .collect();
    write_left_launchers(&s, &ready, &[ended[0], me]).unwrap();
    assert_left("beside one ended launch", true);
    let other_stage = Ready {
        dmg_sha256: "cd".repeat(32),
        ..ready.clone()
    };
    write_left_launchers(&s, &other_stage, &[ended[0], ended[1]]).unwrap();
    assert_left("beside two ended launches of another stage", true);
    assert_eq!(read_left_launchers(&s, &ready).unwrap(), [me]);
    write_left_launchers(&s, &ready, &[ended[0], ended[1]]).unwrap();
    assert_left("a press-only window beside two ended launches", false);
    assert_eq!(
        read_left_launchers(&s, &ready).unwrap(),
        [ended[0], ended[1]],
        "a launch that left the stage for a press is never recorded"
    );
    std::fs::remove_file(left_stage_path(&s)).unwrap();
    std::fs::create_dir(left_stage_path(&s)).unwrap();
    assert!(
        boot(WindowKeepsImage, true).is_ok(),
        "a record that cannot count this launch must escape to the boot's swap"
    );
    assert_left("a press-only window beside a record it cannot write", false);
    std::fs::remove_dir(left_stage_path(&s)).unwrap();
    write_left_launchers(&s, &ready, &[ended[0], ended[1], me]).unwrap();
    let Ok(escaped) = boot(WindowKeepsImage, true) else {
        panic!("two ended launches that left the stage must escape to the boot's swap");
    };
    let read = |path: &Path| -> Result<(u64, String), String> {
        if path == installed.as_path() {
            Ok((running, other_commit.to_string()))
        } else {
            Ok((build, commit.to_string()))
        }
    };
    let read_both = |[a, b]: [&Path; 2]| [read(a), read(b)];
    let starts = |_: &Path, _: u64| -> Result<(), String> { Ok(()) };
    let swapped = swap_admitted_stage(
        &s,
        &installed,
        running,
        Some(other_commit),
        escaped,
        &SwapSeams {
            one: &read,
            both: &read_both,
            probe: &starts,
        },
    );
    assert!(swapped.is_ok(), "the escape swaps the stage in");
    drop(swapped);
    assert_eq!(read_id(&installed), "NEW");
    assert!(!s.ready.exists(), "the swapped stage is retired");
    assert!(
        !left_stage_path(&s).exists(),
        "a swap of the stage removes the record of the launches that left it"
    );
}

/// Which start-probe failures leave the candidate unproven: a probe killed at its
#[test]
fn only_a_probe_that_lost_a_started_candidate_leaves_it_unproven() {
    let what = "candidate --version probe";
    for unproven in [
        crate::verify::timed_out(what, false),
        crate::verify::timed_out(what, true),
        crate::verify::mark_passing(format!("wait for {what}: Interrupted system call")),
    ] {
        assert!(probe_left_unproven(&unproven), "{unproven}");
    }
    for answered in [
        crate::verify::spawn_refusal(what, &std::io::Error::from_raw_os_error(libc::EAGAIN)),
        crate::verify::spawn_refusal(what, &std::io::Error::from_raw_os_error(libc::EACCES)),
        "the staged bundle could not start (exit status: 1): dyld: boom".to_string(),
        "no executable at /x/aterm.app/Contents/MacOS/aterm".to_string(),
    ] {
        assert!(!probe_left_unproven(&answered), "{answered}");
    }
}

/// A START PROBE KILLED BEFORE ITS CANDIDATE EXITED LEAVES IT WHERE IT RAN (r4g-2).
#[test]
fn a_start_probe_killed_before_its_candidate_exited_leaves_it_where_it_ran() {
    let (running, build, commit, other_commit) = (
        10,
        11,
        "0123456789abcdef0123456789abcdef01234567",
        "fedcba9876543210fedcba9876543210fedcba98",
    );
    let stage = |s: &Staging, id: &str| {
        write_ready(s, build);
        make_app(&s.staged_app, id);
        std::fs::create_dir_all(s.staged_app.join("Contents")).unwrap();
        std::fs::write(
            s.staged_app.join("Contents/Info.plist"),
            format!(
                "<plist><dict><key>CFBundleVersion</key><string>{build}</string>\
                 <key>ATermGitCommit</key><string>{commit}</string></dict></plist>"
            ),
        )
        .unwrap();
    };
    let desk = |id: &str| {
        let (s, root) = temp_staging();
        stage(&s, id);
        let installed = root.join("Applications/aterm.app");
        make_app(&installed, "OLD");
        (s, root, installed)
    };
    let swap = |s: &Staging,
                installed: &Path,
                probe: &dyn Fn(&Path, u64) -> Result<(), String>|
     -> Result<(), ApplyOutcome> {
        let read = |path: &Path| -> Result<(u64, String), String> {
            if path == installed {
                Ok((running, other_commit.to_string()))
            } else {
                Ok((build, commit.to_string()))
            }
        };
        let read_both = |[a, b]: [&Path; 2]| [read(a), read(b)];
        let admitted =
            admit_stage_under_apply_lock(s, installed, running, Some(other_commit), None)?;
        swap_admitted_stage(
            s,
            installed,
            running,
            Some(other_commit),
            admitted,
            &SwapSeams {
                one: &read,
                both: &read_both,
                probe,
            },
        )
        .map(drop)
    };
    let killed = |_: &Path, _: u64| -> Result<(), String> {
        Err(crate::verify::timed_out("candidate --version probe", false))
    };
    let starts = |_: &Path, _: u64| -> Result<(), String> { Ok(()) };

    let (s, root, installed) = desk("NEW");
    let fixed = rollback_path(&installed);
    assert!(matches!(
        swap(&s, &installed, &killed),
        Err(ApplyOutcome::Deferred(_))
    ));
    assert_eq!(read_id(&fixed), "NEW", "left exactly where it ran");
    assert!(
        !s.staged_app.exists() && !s.ready.exists(),
        "the moved stage's marker is retired"
    );
    assert_eq!(read_id(&installed), "OLD");
    let record = UnprovenCandidate::read(&s).expect("the unproven candidate is recorded");
    assert_eq!(record.bundle, fixed);
    stage(&s, "NEW2");
    match swap(&s, &installed, &starts) {
        Err(ApplyOutcome::Deferred(why)) => assert!(
            crate::is_passing_refusal(&why) && why.contains("first exec"),
            "{why}"
        ),
        other => panic!("swapped through an unproven candidate: {other:?}"),
    }
    assert_eq!(read_id(&fixed), "NEW");
    assert_eq!(read_id(&s.staged_app), "NEW2");
    assert_eq!(read_id(&installed), "OLD");
    UnprovenCandidate {
        bundle: fixed.clone(),
        killed_at: record.killed_at - aterm_uds::deadline::FIRST_EXEC_HANG.as_secs(),
    }
    .record(&s)
    .unwrap();
    assert!(swap(&s, &installed, &starts).is_ok());
    assert_eq!(read_id(&installed), "NEW2");
    assert_eq!(read_id(&fixed), "OLD", "the rollback source");
    assert!(
        UnprovenCandidate::read(&s).is_none(),
        "the record is dropped"
    );
    let _ = std::fs::remove_dir_all(root);

    let verdict = |_: &Path, _: u64| -> Result<(), String> {
        Err("the staged bundle could not start (exit status: 1): dyld: boom".to_string())
    };
    let refused = |_: &Path, _: u64| -> Result<(), String> {
        Err(crate::verify::spawn_refusal(
            "candidate --version probe",
            &std::io::Error::from_raw_os_error(libc::EAGAIN),
        ))
    };
    for (what, probe) in [
        (
            "a verdict",
            &verdict as &dyn Fn(&Path, u64) -> Result<(), String>,
        ),
        ("a refused spawn", &refused),
    ] {
        let (s, root, installed) = desk("NEW");
        assert!(
            matches!(swap(&s, &installed, probe), Err(ApplyOutcome::Deferred(_))),
            "{what}"
        );
        assert_eq!(
            read_id(&s.staged_app),
            "NEW",
            "{what}: the stage is restored"
        );
        assert!(s.ready.exists(), "{what}: its marker stays");
        assert!(!rollback_path(&installed).exists(), "{what}");
        assert!(
            UnprovenCandidate::read(&s).is_none(),
            "{what}: nothing recorded"
        );
        let _ = std::fs::remove_dir_all(root);
    }
}

/// Across volumes the candidate is a copy at the fixed path beside an intact stage: an
#[test]
fn an_unproven_copied_candidate_stays_beside_its_stage() {
    let (s, root) = temp_staging();
    write_ready(&s, 54);
    make_app(&s.staged_app, "NEW");
    let installed = root.join("Applications/aterm.app");
    make_app(&installed, "OLD");
    let fixed = rollback_path(&installed);
    make_app(&fixed, "COPY");
    keep_unproven_candidate(&copied_candidate(&fixed), &s, 1_800_000_000);
    assert_eq!(read_id(&fixed), "COPY");
    assert_eq!(read_id(&s.staged_app), "NEW");
    assert!(s.ready.exists());
    assert_eq!(
        UnprovenCandidate::read(&s),
        Some(UnprovenCandidate {
            bundle: fixed.clone(),
            killed_at: 1_800_000_000
        })
    );
    std::fs::remove_dir_all(&s.staged_app).unwrap();
    UnprovenCandidate {
        bundle: fixed.clone(),
        killed_at: u64::MAX,
    }
    .record(&s)
    .unwrap();
    let ready_bytes = std::fs::read(&s.ready).unwrap();
    let ready = Ready::read(&s.ready).unwrap();
    let refusal = recover_orphaned_prepared_candidate(&s, &installed, 53, &ready).unwrap_err();
    assert!(crate::is_passing_refusal(&refusal), "{refusal}");
    assert_eq!(read_id(&fixed), "COPY");
    assert_eq!(std::fs::read(&s.ready).unwrap(), ready_bytes);
    assert!(
        !s.staged_app.exists(),
        "the held copy stays at its fixed path"
    );
    make_app(&s.staged_app, "NEW");
    UnprovenCandidate {
        bundle: fixed.clone(),
        killed_at: 1_800_000_000,
    }
    .record(&s)
    .unwrap();

    let hang = aterm_uds::deadline::FIRST_EXEC_HANG.as_secs();
    for earlier in [1_800_000_000 - hang, 0] {
        assert_eq!(
            UnprovenCandidate::holds(&s, &fixed, earlier),
            Some(hang),
            "a backward or unavailable clock cannot age the candidate"
        );
        assert!(UnprovenCandidate::read(&s).is_some());
    }
    assert_eq!(
        UnprovenCandidate::holds(&s, &fixed, 1_800_000_000 + 60),
        Some(hang - 60)
    );
    assert_eq!(
        UnprovenCandidate::holds(&s, &root.join("elsewhere"), 1_800_000_060),
        None,
        "it holds only the path it names"
    );
    assert!(
        UnprovenCandidate::read(&s).is_some(),
        "kept for its own path"
    );
    assert_eq!(
        UnprovenCandidate::holds(&s, &fixed, 1_800_000_000 + hang),
        None
    );
    assert!(
        UnprovenCandidate::read(&s).is_none(),
        "run its course: dropped"
    );
    recover_prepared_candidate(&copied_candidate(&fixed), &s);
    assert!(!fixed.exists(), "backed out, the copy goes");
    assert_eq!(read_id(&s.staged_app), "NEW");
    let _ = std::fs::remove_dir_all(root);
}
