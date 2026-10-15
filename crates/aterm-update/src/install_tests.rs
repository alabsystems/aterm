// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0

/// An installed bundle replaced by a hand-built, ad-hoc-signed one cannot be the
/// rollback source, so the successor's boot apply would refuse to swap, stay the
/// old build and refuse its target (read by the parent as `ChildDied`). The
/// verdict is decidable before the first reader parks, so it refuses here.
///
/// `None` is `bundle::resolve()` finding nothing the updater may replace
/// (`ApplyOutcome::NotApplicable` at swap time): a successor whose swap is a
/// no-op can only refuse the target, so this must refuse now.
#[test]
fn a_process_with_no_replaceable_bundle_refuses_before_anything_parks() {
    let error = super::preverify_installed_rollback_source(None, 100, Some("abcdef0"))
        .expect_err("no replaceable bundle cannot host a swap");
    assert!(
        error.contains("does not run from an installed bundle"),
        "the refusal names the missing bundle rather than blaming the candidate: {error}"
    );
}

/// A bundle path that is not a verifiable bundle refuses, and the refusal names
/// the path, the one fact an operator needs.
#[test]
fn an_unverifiable_installed_bundle_refuses_and_names_itself() {
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
    assert!(
        error.contains("rollback source"),
        "the refusal names WHAT the bundle failed to be: {error}"
    );
}

/// The identity half of the same gate, pinned pure: a bundle sealed with a
/// different build than the running process (the `.app` replaced under a live
/// instance) is not a rollback source.
#[test]
fn only_the_running_image_can_be_the_rollback_source() {
    assert!(
        super::identity_matches_running(1787614521, "abcdef0123", 1787614521, Some("abcdef0")),
        "the sealed identity of the image we are executing is the rollback source"
    );
    assert!(
        !super::identity_matches_running(1787634715, "abcdef0123", 1787614521, Some("abcdef0")),
        "a bundle swapped under a live instance is a different build, not our rollback source"
    );
    assert!(
        !super::identity_matches_running(1787614521, "abcdef0123", 1787614521, Some("9999999")),
        "same build number, different provenance, is still not our image"
    );
}

/// The post-swap re-exec image carries both the single-use re-exec nonce and the
/// caller's restored handoff authority pairs; without them the successor's
/// prearm cannot validate the inherited handoff. Asserted on the constructed
/// Command (program, `--window` pin first, envs), so it is testable without exec.
#[test]
fn boot_reexec_command_pins_mode_and_restores_handoff_env() {
    let handoff_env = vec![(
        std::ffi::OsString::from("ATERM_HANDOFF_PARENT_PID"),
        std::ffi::OsString::from("4242"),
    )];
    let command = super::boot_reexec_command(
        std::path::Path::new("/Applications/aterm.app/Contents/MacOS/aterm"),
        "0123456789abcdef",
        1785910394,
        &handoff_env,
    );
    assert_eq!(
        command.get_program(),
        std::ffi::OsStr::new("/Applications/aterm.app/Contents/MacOS/aterm")
    );
    assert_eq!(
        command.get_args().next(),
        Some(std::ffi::OsStr::new("--window")),
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

/// THE CRASH-LOOP REVERT'S RE-EXEC IS PINNED TO ITS MODE as the boot swap's
/// is. A window started as `aterm --window` from a shell still has that terminal
/// on stdin; handed the stripped argv alone, the one-binary router would read it
/// as a SESSION and nest the restored build in that terminal. Every build a
/// revert can restore has the router (d2a29b6c7, an ancestor of v0.21.0, the
/// oldest install that can still update), which strips the pin.
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
        // A verb's launch is forwarded as the verb, unpinned, exactly as the
        // boot swap forwards it.
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
        assert_eq!(
            command
                .get_args()
                .map(std::ffi::OsStr::to_os_string)
                .collect::<Vec<_>>(),
            super::window_successor_args(launch.into_iter()),
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

/// The re-exec forwards this process's own argv, which already carries the mode
/// pin the previous swap prepended; the strip makes the forwarded argv a fixed
/// point (no `--window` per update) while never reaching past the first non-pin
/// token, so `-e`/`--command` payloads are untouched.
#[test]
fn reexec_forwarded_args_is_a_fixed_point_and_never_reads_past_the_lead() {
    let args = |list: &[&str]| -> Vec<std::ffi::OsString> {
        list.iter().map(std::ffi::OsString::from).collect()
    };
    // The field case: six accumulated pins collapse to none forwarded
    // (boot_reexec_command re-pins exactly one).
    assert_eq!(
        crate::relaunch::reexec_forwarded_args(args(&["--window"; 6]).into_iter()),
        args(&[]),
    );
    // Fixed point: a once-stripped argv re-strips to itself.
    let once = crate::relaunch::reexec_forwarded_args(
        args(&["--window", "--font-px", "30", "--window"]).into_iter(),
    );
    assert_eq!(once, args(&["--font-px", "30", "--window"]));
    assert_eq!(
        crate::relaunch::reexec_forwarded_args(once.clone().into_iter()),
        once,
        "stripping must converge after one pass"
    );
    // A `--window` past the first other token is payload-adjacent and
    // survives verbatim — including inside an -e command.
    assert_eq!(
        crate::relaunch::reexec_forwarded_args(args(&["-e", "sh", "-c", "--window"]).into_iter()),
        args(&["-e", "sh", "-c", "--window"]),
    );
    // Empty argv (Finder/launchd launch) stays empty.
    assert_eq!(
        crate::relaunch::reexec_forwarded_args(args(&[]).into_iter()),
        args(&[])
    );
}

/// `rfc3339_delta_secs` inverts `format_rfc3339` across day/month/year and
/// leap boundaries, and fails closed on malformed input (the pre-epoch
/// empty-string fallback) — the duration gate on the persistent-failure
/// notice depends on both properties.
#[test]
fn rfc3339_delta_round_trips_and_fails_closed() {
    for (a, b, want) in [
        (0u64, 61, 61),
        (86_399, 86_401, 2),                   // midnight crossing
        (1_709_164_799, 1_709_164_800, 1),     // Feb 29 2024 (leap) boundary
        (1_784_100_000, 1_784_101_800, 1_800), // a modern 30-min streak
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
/// `NativeUpdateTwoProcessApply`). Both run build 10 with 11 staged and run steps
/// 3–4 ([`apply_lock_then_ready`]). B peeks and waits for the lock (injected at
/// B's `lock`) while A wins the real `apply_lock`, reads the marker under it and
/// swaps (`retire_published`); B then finds nothing to apply. Each real step is
/// projected onto the model and checked as one of its transitions, in both
/// orders.
///
/// Negative control in real code: a lane acting on its pre-lock peek (the
/// model's `Buggy = 1`) swaps build 11 twice, which the model's invariant
/// refutes. `the_boot_lane_decides_through_apply_lock_then_ready` pins that the
/// lane runs the function this drives.
#[test]
fn two_processes_arming_one_build_swap_it_once() {
    use aterm_spec::interp::State;
    type Lane = fn(
        &Staging,
        &mut dyn FnMut() -> Result<FileLock, ApplyOutcome>,
    ) -> Result<Ready, ApplyOutcome>;
    let model = aterm_spec::derive::native_update_two_process_apply_model();
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
    // The lane (steps 3–4, exactly as `apply_staged_if_ready_inner` runs them) and
    // the peek-trusting one the negative control runs the same race through.
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
    // A wins the lock while B waits; returns (B's swaps, the model states it walked).
    let race = |b_lane: Lane| {
        let (s, root) = temp_staging();
        write_ready(&s, 11);
        let init = project(true, false, false, false, 0);
        let mut walked = vec![init.clone()];
        let b_decision = b_lane(&s, &mut || {
            // B got here only because its peek saw the stage.
            let peeked = project(marker_present(&s), false, true, false, 0);
            walked.push(peeked.clone());
            // A's successor runs the SAME lane: it holds the real lock (B is refused
            // it meanwhile), reads under it, swaps and retires.
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
            // A's lock went with `shipped`'s return: a `FileLock` drop is `LOCK_UN`
            // (aterm-update-core `sys.rs`), a deterministic release, so B's one
            // acquire below cannot meet a forked copy of A's descriptor.
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

    // Negative control: the peek-trusting lane, same race, swaps twice.
    let (b_swaps, walked) = race(peek_trusting);
    assert_eq!(b_swaps, 1, "a lane acting on its peek swaps build 11 again");
    let (swapped, second) = (&walked[2], &walked[3]);
    let buggy = aterm_spec::interp::with_buggy(&model, 1);
    assert!(
        buggy.successors("BApply", swapped).contains(second),
        "the peek-trusting lane is the model's Buggy = 1 step: {second:?}"
    );
    assert!(!model.check_invariant("OneSwapPerBuild", second));

    // B wins the lock instead: B swaps, A finds nothing — still exactly one swap.
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

/// The boot lane decides through [`apply_lock_then_ready`], the function the
/// two-process Tier-1 drives, and nothing else in shipped code calls the
/// under-lock re-read; otherwise that Tier-1 could stay green over code that no
/// longer runs. Asserted on source, since the alternative is a packaged bundle
/// and a second process.
#[test]
fn the_boot_lane_decides_through_apply_lock_then_ready() {
    let src = include_str!("install.rs");
    let shipped = &src[..src
        .find("#[cfg(test)]\n#[path = \"install_tests.rs\"]\nmod tests;")
        .expect("the test module")];
    let lane = &shipped[shipped
        .find("fn apply_staged_if_ready_inner(")
        .expect("the boot lane")..];
    let lane = &lane[..lane.find("\n}\n").expect("end of the boot lane")];
    // Split so these assertions do not match themselves.
    assert_eq!(
        lane.matches(concat!("apply_lock_then_ready", "(")).count(),
        1,
        "the boot lane decides through apply_lock_then_ready, once"
    );
    assert!(
        lane.contains(concat!(
            "let (_lock, _budget, ready) = match ",
            "apply_lock_then_ready("
        )),
        "the `ready` the lane acts on is the one apply_lock_then_ready decided under the lock"
    );
    // Its one other marker read is the orphan recovery, inside `under_lock`
    // (`settle_trials_under_apply_lock`, which the lane calls there); a read in the
    // lane itself (a peek bound before the lock and acted on after) is the regression.
    assert_eq!(lane.matches(concat!("read_ready", "(")).count(), 0);
    assert!(!lane.contains(concat!("Ready::", "read(")));
    assert_eq!(
        lane.matches(concat!("settle_trials_under_apply_lock", "("))
            .count(),
        1
    );
    let settle = &shipped[shipped
        .find(concat!("fn settle_trials_under_apply_lock", "("))
        .expect("the under-lock work")..];
    let settle = &settle[..settle.find("\n}\n").expect("its end")];
    assert_eq!(settle.matches(concat!("read_ready", "(")).count(), 1);
    assert!(!settle.contains(concat!("Ready::", "read(")));
    assert_eq!(
        shipped
            .matches(concat!("ready_under_apply_lock", "("))
            .count(),
        2,
        "ready_under_apply_lock: its definition and apply_lock_then_ready's one call"
    );
    let helper = &shipped[shipped
        .find(concat!("fn apply_lock_then_ready", "<"))
        .expect("the helper")..];
    let (locked, decided) = (
        helper.find("let held = lock()?;").expect("the lock"),
        helper
            .find(concat!("ready_under_apply_lock", "(staging"))
            .expect("the decision"),
    );
    assert!(locked < decided, "the decision is made under the lock");
}

/// A `ready.toml` whose staged bundle is gone clears itself: this recovery runs
/// before the `is_publishable` retirement, so returning Err would defer every
/// launch permanently.
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
/// without them keeps parsing, so an upgrade does not strand a staged build
/// behind a `Corrupt` marker.
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
/// reader goes through `read_ledger_text`, `None` above `MAX_LEDGER_BYTES`). The
/// clamp counts characters: a byte-offset cut inside a multi-byte sequence
/// panics.
#[test]
fn an_oversized_manifest_changelog_is_clamped_by_characters_not_bytes() {
    assert_eq!(clamp_ready_changelog(None), None);
    assert_eq!(
        clamp_ready_changelog(Some("a short release note")).as_deref(),
        Some("a short release note")
    );

    // Three bytes per char, so any byte-indexed cut would land mid-sequence.
    let wide = "—".repeat(MAX_READY_CHANGELOG_CHARS * 4);
    let clamped = clamp_ready_changelog(Some(&wide)).unwrap();
    assert!(clamped.starts_with('—'), "the head survives verbatim");
    assert!(clamped.ends_with(READY_CHANGELOG_TRUNCATED));
    assert_eq!(
        clamped.chars().count(),
        MAX_READY_CHANGELOG_CHARS + READY_CHANGELOG_TRUNCATED.chars().count()
    );

    // A whole ~382 KiB CHANGELOG.md in one manifest field: clamped, the marker
    // stays inside the cap every reader enforces.
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

/// A stage decided before a crash-loop revert cannot outlive it: a checker on the
/// old build downloads Q during Q's boot trial, the third failed launch reverts
/// and quarantines Q, and the in-flight check must not then publish Q. The
/// publish shares the revert's apply lock and checks the quarantine under it;
/// the readers refuse what an older stager published.
#[test]
fn a_stage_decided_before_a_crash_loop_revert_cannot_republish_the_quarantined_build() {
    let (s, root) = temp_staging();
    let commit = "0123456789abcdef0123456789abcdef01234567";
    let sha = "ab".repeat(32);
    let plist = format!(
        "<plist><dict><key>CFBundleVersion</key><string>7</string>\
         <key>ATermGitCommit</key><string>{commit}</string></dict></plist>"
    );
    let incoming = s.staged_dir().join("aterm.app.incoming");
    make_app(&incoming, "Q");
    std::fs::create_dir_all(incoming.join("Contents")).unwrap();
    std::fs::write(incoming.join("Contents/Info.plist"), &plist).unwrap();
    let ready = Ready {
        build_number: 7,
        version: "0.0.7".into(),
        commit: Some(commit.into()),
        dmg_sha256: sha.clone(),
        team_id: "T".into(),
        staged_at: String::new(),
        changelog: None,
        machine_id: None,
        roster_seq: None,
        roster_confirmed_at: Some(crate::install::now_rfc3339()),
    };

    // The revert lands while the stager is still downloading.
    crate::manifest::quarantine_artifact(&s, 7, &sha);
    let refused = publish_verified_stage(&s, &incoming, &ready);
    assert!(
        refused
            .as_ref()
            .is_err_and(|error| error.contains("quarantined")),
        "{refused:?}"
    );
    assert!(Ready::read_publishable(&s).is_none());
    // The stager's success tail must not erase the verdict.
    crate::manifest::FailedMark::clear_backoff(&s.failed());
    assert!(crate::manifest::artifact_quarantined(&s, 7, &sha));

    // A stager of an older build publishes without asking: every reader
    // still refuses the quarantined stage.
    std::fs::rename(&incoming, &s.staged_app).unwrap();
    std::fs::write(&s.ready, ready.to_toml().unwrap()).unwrap();
    assert!(
        Ready::read_publishable(&s).is_none(),
        "a quarantined stage is not publishable"
    );
    let preverify =
        preverify_staged_handoff_candidate_at(&s, None, 6, None, Some(7), None, &|_| ());
    assert!(
        preverify
            .as_ref()
            .is_err_and(|error| error.contains("quarantined")),
        "{preverify:?}"
    );
    // Negative control: the same stage of an artifact nobody quarantined reads
    // as publishable.
    let other = Ready {
        dmg_sha256: "cd".repeat(32),
        ..ready
    };
    std::fs::write(&s.ready, other.to_toml().unwrap()).unwrap();
    assert!(Ready::read_publishable(&s).is_some());
    let _ = std::fs::remove_dir_all(root);
}

/// A stage whose signer a sibling saw revoked mid-download is never published:
/// check A authorized build 7 by m11 and began downloading; check B admitted a
/// roster revoking m11, recorded it in the floor, and found no published stage
/// to retire. The publish re-reads the floor under the lock the retirement
/// holds.
#[test]
fn a_stage_whose_signer_was_revoked_during_its_download_is_not_published() {
    let (s, root) = temp_staging();
    let commit = "0123456789abcdef0123456789abcdef01234567";
    let incoming = s.staged_dir().join("aterm.app.incoming");
    make_app(&incoming, "R");
    std::fs::create_dir_all(incoming.join("Contents")).unwrap();
    std::fs::write(
        incoming.join("Contents/Info.plist"),
        format!(
            "<plist><dict><key>CFBundleVersion</key><string>7</string>\
             <key>ATermGitCommit</key><string>{commit}</string></dict></plist>"
        ),
    )
    .unwrap();
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
    // The sibling's check, landing while this one downloaded.
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

    // Negative control: a generation that only adds a machine (a join) does not
    // withdraw a stage signed by one that stays, the exactness a
    // roster-generation comparison would lose.
    let by_m3 = Ready {
        machine_id: Some("m3".into()),
        ..ready
    };
    crate::manifest::Floor::observe_and_write(
        &s.floor(),
        &crate::manifest::FloorObservation {
            roster_seq: 11,
            ..crate::manifest::FloorObservation::default()
        },
    );
    publish_verified_stage(&s, &incoming, &by_m3).expect("m3 is still trusted");
    assert!(Ready::read_publishable(&s).is_some());
    let _ = std::fs::remove_dir_all(root);
}

/// A FLOOR THAT COULD NOT BE READ ADMITS NOTHING: never read as "no floor". The
/// swap DEFERS (passing, the stage kept for the next attempt) and the pre-park
/// check refuses, passing.
///
/// CONTROL: the same floor, readable, retires the revoked stage as before.
#[cfg(unix)]
#[test]
fn an_unreadable_floor_defers_the_swap_and_refuses_the_handoff() {
    use std::os::unix::fs::PermissionsExt as _;
    let (s, root) = temp_staging();
    let ready = Ready {
        build_number: 20,
        version: "0.0.20".into(),
        commit: Some("0123456789abcdef0123456789abcdef01234567".into()),
        dmg_sha256: "ab".repeat(32),
        team_id: "T".into(),
        staged_at: String::new(),
        changelog: None,
        machine_id: Some("m11".into()),
        roster_seq: Some(9),
        roster_confirmed_at: Some(crate::install::now_rfc3339()),
    };
    std::fs::write(&s.ready, ready.to_toml().unwrap()).unwrap();
    std::fs::write(s.floor(), "roster_seq = 10\nrevoked_machines = [\"m11\"]\n").unwrap();

    std::fs::set_permissions(s.floor(), std::fs::Permissions::from_mode(0o000)).unwrap();
    let premise = std::fs::read(s.floor()).is_err();
    let swap = floor_withdrawal_gate(&s, 10, &ready);
    let handoff = handoff_apply_floor_refusal_at(&s, 20);
    std::fs::set_permissions(s.floor(), std::fs::Permissions::from_mode(0o600)).unwrap();
    if premise {
        match &swap {
            Some(ApplyOutcome::Deferred(why)) => {
                assert!(crate::is_passing_refusal(why), "{why}");
            }
            other => panic!("an unreadable floor defers the swap: {other:?}"),
        }
        assert!(s.ready.exists(), "a deferral keeps the stage");
        let refusal = handoff.expect("an unreadable floor refuses the handoff");
        assert!(crate::is_passing_refusal(&refusal), "{refusal}");
    }

    // CONTROL: readable, the revocation retires the stage.
    assert!(
        handoff_apply_floor_refusal_at(&s, 20).is_some_and(|r| r.contains("revoked")),
        "the revoked signer is refused"
    );
    assert!(matches!(
        floor_withdrawal_gate(&s, 10, &ready),
        Some(ApplyOutcome::NoUpdate)
    ));
    assert!(!s.ready.exists(), "the stage is retired");
    let _ = std::fs::remove_dir_all(root);
}

/// THE SWAP REFUSES A STAGE SIGNED BY A MACHINE THIS CLIENT HAS SEEN REVOKED. The
/// check that admits the revoking roster retires the stage under the apply lock —
/// unless it could not take that lock in time, leaving the stage for its next
/// check — so gate 4c reads the floor's durable revocation set itself.
#[test]
fn the_swap_retires_a_stage_whose_signer_the_floor_has_seen_revoked() {
    let (s, root) = temp_staging();
    let ready = |machine: &str| Ready {
        build_number: 20,
        version: "0.0.20".into(),
        commit: Some("0123456789abcdef0123456789abcdef01234567".into()),
        dmg_sha256: "ab".repeat(32),
        team_id: "T".into(),
        staged_at: String::new(),
        changelog: None,
        machine_id: Some(machine.into()),
        roster_seq: Some(9),
        roster_confirmed_at: Some(crate::install::now_rfc3339()),
    };
    std::fs::write(&s.ready, ready("m11").to_toml().unwrap()).unwrap();
    // Negative control: nothing revoked, nothing yanked, and the gate passes it.
    assert!(floor_withdrawal_gate(&s, 10, &ready("m11")).is_none());
    assert_eq!(handoff_apply_floor_refusal_at(&s, 20), None);

    let revoked = vec!["m11".to_string()];
    crate::manifest::Floor::observe_and_write(
        &s.floor(),
        &crate::manifest::FloorObservation {
            roster_seq: 10,
            revoked: &revoked,
            ..crate::manifest::FloorObservation::default()
        },
    );
    // Another machine's stage is untouched by m11's revocation.
    assert!(floor_withdrawal_gate(&s, 10, &ready("m3")).is_none());
    // The pre-park check refuses it before any reader parks…
    let refusal = handoff_apply_floor_refusal_at(&s, 20).expect("revoked signer");
    assert!(
        refusal.contains("m11") && refusal.contains("revoked"),
        "{refusal}"
    );
    // …and the swap retires it.
    let outcome = floor_withdrawal_gate(&s, 10, &ready("m11"));
    assert!(
        matches!(outcome, Some(ApplyOutcome::NoUpdate)),
        "{outcome:?}"
    );
    assert!(!s.ready.exists(), "the stage is retired");
    let status = std::fs::read_to_string(&s.status).unwrap_or_default();
    assert!(status.contains("revoked"), "{status}");
    let _ = std::fs::remove_dir_all(root);
}

/// Build `build`, signed by `machine` under roster generation 9, published as a
/// stage whose roster authorization was last confirmed at `confirmed` (unix
/// seconds; `None`: never, as an older build's marker).
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
/// 20, signed by machine m11, was staged and confirmed
/// against roster generation 9 two days ago; the app was quit before the
/// in-session lane applied it, and the owner may since have revoked m11 in a
/// release no check on this Mac has read (gate 4c reads only revocations a check
/// has seen, and the floor here has seen none). The swap's gates WAIT — a passing refusal that keeps the
/// stage, memoizes nothing and names the roster — for the check that runs right
/// after launch. A stage an older build staged (no confirmation at all) waits
/// the same way. CONTROL: confirmed an hour ago, the gates admit it.
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
        match stage_admission_gates(&s, 10, &ready, now) {
            Some(ApplyOutcome::Deferred(why)) => {
                assert!(crate::is_passing_refusal(&why), "{why}");
                assert!(
                    why.contains(crate::manifest::ROSTER_UNCONFIRMED_KEY),
                    "{why}"
                );
                assert!(
                    boot_apply_refusal_reason(&ApplyOutcome::Deferred(why.clone())).is_some(),
                    "the wrapper books it, so a wait that lasts is named"
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
    // CONTROL: confirmed an hour ago, the swap goes on as today.
    let fresh = publish_confirmed_stage(&s, 20, "m11", Some(now - 3_600));
    assert_eq!(stage_admission_gates(&s, 10, &fresh, now), None);
    // A revocation a check HAS seen still retires, stale or not (gate 4c first).
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
        stage_admission_gates(&s, 10, &stale, now),
        Some(ApplyOutcome::NoUpdate)
    );
    assert!(!s.ready.exists(), "the revoked stage is retired");
    let _ = std::fs::remove_dir_all(root);
}

/// THE LAUNCH'S WAIT IS NO STUCK UPDATE (law L5). The pending clock counts from
/// the stage's first sighting, so across the days the app was closed. The boot
/// lane's booking of a wait for the roster confirmation restarts the clock, so the notice speaks only if the
/// wait outlasts its hour. CONTROL: any other refusal leaves the clock alone.
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
    let Some(ApplyOutcome::Deferred(wait)) = stage_admission_gates(&s, 10, &stale, now) else {
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

/// THE IN-SESSION LANE WAITS TOO. A stage the launch declined for a stale roster
/// confirmation must not be swapped in by the seamless lane before the check can
/// confirm or retire it. The full pre-verification
/// and the cached pass both refuse it, PASSING, before `codesign` and before any
/// policy is read. CONTROL: confirmed an hour ago, the cached pass lets it on.
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
    // CONTROL: confirmed an hour ago.
    publish_confirmed_stage(&s, 20, "m3", Some(now - 3_600));
    assert_eq!(handoff_cached_pass_refusal_at(&s, 20, commit), None);
    let _ = std::fs::remove_dir_all(root);
}

/// A CHECK'S CONFIRMATION LANDS ON THE STAGE IT JUDGED, AND ENDS THE WAIT IT
/// ANSWERS. The marker's stamp moves to the check's time; a
/// marker another process replaced meanwhile is left alone; the ledger's
/// standing refusal is withdrawn when it was the wait for a confirmation, and
/// kept when it was anything else.
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
    // Seen waiting since a month ago (the app was closed since), and the launch
    // booked its wait.
    let long_ago = "2026-09-01T00:00:00Z";
    std::fs::write(
        s.health(),
        format!("pending_build = 20\npending_since = \"{long_ago}\"\n"),
    )
    .unwrap();
    crate::health::Health::record_apply_refusal(&s.health(), 10, &wait);
    let stamp = aterm_types::rfc3339::format_rfc3339(now);
    confirm_roster_authorization(&s, &stale, &stamp).unwrap();
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
    // Confirming a stage that was not waiting moves no clock.
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

    // Another refusal stands.
    crate::health::Health::record_apply_refusal(&s.health(), 10, "install location not writable");
    confirm_roster_authorization(&s, &confirmed, &stamp).unwrap();
    assert_eq!(
        crate::health::Health::read(&s.health()).last_apply_refusal,
        "install location not writable"
    );

    // A stage replaced since the check judged it is not the one confirmed.
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

/// A CONFIRMATION IS DATED BY THE ROSTER'S READ, AND NEVER MOVES BACKWARDS. The
/// window bounds how long ago a roster was read, so a stage is stamped with the
/// instant its check began reading the roster, not the instant of the write (a
/// publish after a download of hours must not add those hours to the window).
/// And a later read already on the
/// marker (a sibling's check) is kept over an earlier one landing after it,
/// unless it sits ahead of this Mac's clock.
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

    // A check that began reading the roster five hours ago stamps that.
    let read = at(now - 5 * 3_600);
    confirm_roster_authorization(&s, &stale, &read).unwrap();
    assert_eq!(stamp().as_deref(), Some(read.as_str()));
    // A slower check's earlier read, landing after it, leaves the later stamp.
    confirm_roster_authorization(&s, &stale, &at(now - 6 * 3_600)).unwrap();
    assert_eq!(stamp().as_deref(), Some(read.as_str()), "never backwards");
    // A newer read moves it on.
    let newer = at(now - 60);
    confirm_roster_authorization(&s, &stale, &newer).unwrap();
    assert_eq!(stamp().as_deref(), Some(newer.as_str()));
    // A stamp ahead of the clock (a clock stepped back) has no knowable age and
    // is replaced.
    let mut ahead = Ready::read(&s.ready).unwrap();
    ahead.roster_confirmed_at = Some(at(now + 3_600));
    std::fs::write(&s.ready, ahead.to_toml().unwrap()).unwrap();
    confirm_roster_authorization(&s, &stale, &read).unwrap();
    assert_eq!(stamp().as_deref(), Some(read.as_str()));

    // THE STAGE A CHECK PUBLISHES carries its roster read, not its publish.
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
    // A day after the READ (not after the publish) the swap waits for a check.
    assert!(
        marker
            .roster_authorization_stale(
                now - 5 * 3_600 + crate::manifest::ROSTER_FRESHNESS_SECS,
                true
            )
            .is_some()
    );
    // A clock that could not be read stamps nothing: unconfirmed.
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

/// The boot lane runs the stage gates (4b-4d) once, after the under-lock read and
/// before the install location; and gate 4d is the last of them, after the
/// retiring ones. Asserted on source, as the lane needs a packaged bundle.
#[test]
fn the_boot_lane_runs_the_stage_admission_gates_before_the_install_location() {
    let src = include_str!("install.rs");
    let shipped = &src[..src
        .find("#[cfg(test)]\n#[path = \"install_tests.rs\"]\nmod tests;")
        .expect("the test module")];
    let lane = &shipped[shipped
        .find("fn apply_staged_if_ready_inner(")
        .expect("the boot lane")..];
    let lane = &lane[..lane.find("\n}\n").expect("end of the boot lane")];
    assert_eq!(
        lane.matches(concat!("stage_admission_gates", "(")).count(),
        1
    );
    let (decided, gated, location) = (
        lane.find(concat!("apply_lock_then_ready", "("))
            .expect("the decision"),
        lane.find(concat!("stage_admission_gates", "("))
            .expect("the gates"),
        lane.find(concat!("install_location_check", "("))
            .expect("step 5"),
    );
    assert!(decided < gated && gated < location);
    let gates = &shipped[shipped
        .find(concat!("fn stage_admission_gates", "("))
        .expect("the gates")..];
    let gates = &gates[..gates.find("\n}\n").expect("their end")];
    let (floor, fresh) = (
        gates
            .find(concat!("floor_withdrawal_gate", "("))
            .expect("4b/4c"),
        gates
            .find(concat!("roster_freshness_gate", "("))
            .expect("4d"),
    );
    assert!(floor < fresh, "the retiring gates come first");
}

/// A FLOOR THAT IS THERE BUT COULD NOT BE READ WITHDRAWS NOTHING AND ADMITS
/// NOTHING. `floor.toml` yanks build 20 (`min_build = 21`) and revokes its signer m11; a
/// moment's `EMFILE`/`EIO` — here a mode this uid cannot open — must not read as
/// "no floor". The pre-park check, the swap's gates 4b/4c and the publish each
/// refuse as a PASSING condition (retried, never a verdict on the
/// build), and the swap leaves the stage for the next attempt to judge.
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
    let incoming = s.staged_dir().join("aterm.app.incoming");
    make_app(&incoming, "R");
    std::fs::create_dir_all(incoming.join("Contents")).unwrap();
    std::fs::write(
        incoming.join("Contents/Info.plist"),
        format!(
            "<plist><dict><key>CFBundleVersion</key><string>20</string>\
             <key>ATermGitCommit</key><string>{commit}</string></dict></plist>"
        ),
    )
    .unwrap();
    std::fs::write(
        s.floor(),
        "min_build = 21\nroster_seq = 10\nrevoked_machines = [\"m11\"]\n",
    )
    .unwrap();
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

    // NEGATIVE CONTROL: the same floor, readable, withdraws the stage by name —
    // the verdict, not a moment.
    let refusal = handoff_apply_floor_refusal_at(&s, 20).expect("revoked signer");
    assert!(!crate::is_passing_refusal(&refusal), "{refusal}");
    assert!(matches!(
        floor_withdrawal_gate(&s, 10, &ready),
        Some(ApplyOutcome::NoUpdate)
    ));
    assert!(!s.ready.exists(), "the readable floor retires the stage");
    let _ = std::fs::remove_dir_all(root);
}

/// A SIBLING'S HOLD OF THE APPLY LOCK IS A MOMENT, NOT A VERDICT (round
/// eight, finding 81). The publish step's bounded wait was the one apply-lock
/// wait on the staging path that formatted its error by hand, so a sibling
/// stopped while it held the lock (or a cold launch's cross-volume copy) read
/// to `book_stage_failure` as a verdict on a healthy, verified build: a 15 min
/// to 24 h re-download backoff and the "will not verify or install" notice.
/// It is passing, and the bundle and the staged generation are left as they
/// were for the next check to publish.
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
    assert!(error.starts_with("publish lock: "), "{error}");
    assert!(
        crate::is_passing_refusal(&error),
        "a sibling's hold is a moment: {error}"
    );
    assert!(!stage_failure_is_a_verdict(&error), "{error}");
    assert_eq!(read_id(&s.staged_app), "OLD-STAGE");
    assert_eq!(read_id(&incoming), "NEW-STAGE");

    // Once the holder lets go the same publish lands.
    publish_verified_stage_within(&s, &incoming, &ready, std::time::Duration::from_secs(60))
        .expect("the lock is free");
    assert_eq!(read_id(&s.staged_app), "NEW-STAGE");
    let _ = std::fs::remove_dir_all(root);
}

/// A marker that would read as ABSENT must never be committed — and refusing it
/// must not cost the readable generation that is already staged, which is why the
/// size guard runs before the lock and before the old marker/bundle are removed.
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
    let error = publish_verified_stage(&s, &incoming, &ready).unwrap_err();
    assert!(error.contains("ready marker"), "{error}");

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
    let _ = std::fs::remove_dir_all(root);
}

#[test]
fn read_ready_classifies_all_states() {
    let (s, root) = temp_staging();

    // Absent: no marker file.
    assert!(matches!(read_ready(&s, 10), ReadyState::Absent));

    // Corrupt: present but unparseable → must be discardable, not Absent.
    std::fs::write(&s.ready, "this is not valid toml {{{").unwrap();
    assert!(matches!(read_ready(&s, 10), ReadyState::Corrupt));

    // Newer: staged build strictly greater than running.
    write_ready(&s, 20);
    assert!(matches!(read_ready(&s, 10), ReadyState::Newer(_)));

    // NotNewer: equal or lower than running (downgrade gate).
    assert!(matches!(read_ready(&s, 20), ReadyState::NotNewer));
    assert!(matches!(read_ready(&s, 21), ReadyState::NotNewer));

    // Parseable is not sufficient: a short commit cannot authorize apply and
    // must not be surfaced by the status reader as a staged update either.
    let mut corrupt = Ready::read(&s.ready).unwrap();
    corrupt.commit = Some("0123456789ab".into());
    std::fs::write(&s.ready, corrupt.to_toml().unwrap()).unwrap();
    assert!(matches!(read_ready(&s, 10), ReadyState::Corrupt));

    let _ = std::fs::remove_dir_all(root);
}

/// Overlap seam 1 (pre-park verification): every refusal on this ladder is a
/// plain `Err` the GUI handoff starter receives before `park_all_readers`, so an
/// absent, stale, wrong-artifact or unverifiable candidate never costs a parked
/// reader, a frozen frame, or a doomed child spawn.
#[test]
fn preverify_refuses_unverifiable_candidates_before_any_reader_could_park() {
    let (s, root) = temp_staging();

    // Nothing staged: refuse immediately.
    let absent = preverify_staged_handoff_candidate_at(&s, None, 10, None, None, None, NEVER_READ);
    assert!(
        absent.clone().unwrap_err().contains("no verified update"),
        "{absent:?}"
    );

    // Staged but not strictly newer than the running build.
    write_ready(&s, 20);
    let stale = preverify_staged_handoff_candidate_at(&s, None, 20, None, None, None, NEVER_READ);
    assert!(
        stale.clone().unwrap_err().contains("strictly newer"),
        "{stale:?}"
    );

    // Staged build is not the artifact the updater reducer authorized.
    let wrong =
        preverify_staged_handoff_candidate_at(&s, None, 10, None, Some(21), None, NEVER_READ);
    assert!(
        wrong
            .clone()
            .unwrap_err()
            .contains("not the authorized build"),
        "{wrong:?}"
    );

    // Right identity on the marker, but no verifiable bundle exists at the
    // staged path: the sealed-identity gate must fail closed.
    let unverifiable =
        preverify_staged_handoff_candidate_at(&s, None, 10, None, Some(20), None, NEVER_READ);
    assert!(unverifiable.is_err(), "{unverifiable:?}");

    let _ = std::fs::remove_dir_all(root);
}

/// THE SHARED STAGE'S STATE IS A MOMENT, NOT A VERDICT ON THE BUILD. A sibling
/// aterm that landed the same update first retires `ready.toml`; one that published a newer stage
/// mid-attempt replaces it; a read the kernel refused for a moment (`EMFILE`
/// in a Dock-launched window) reads as an unreadable marker. Each of those
/// refusals must read as passing, or the GUI files it STRUCTURAL against a
/// healthy build and two of them converge it for a day.
///
/// And only those: the stage's VERDICTS — quarantine, the operator floor, a
/// bundle that does not verify — keep their verdict (negative controls).
#[test]
fn a_stage_state_refusal_is_passing_and_a_verdict_is_not() {
    let (s, root) = temp_staging();

    // A sibling's swap consumed the stage.
    let absent = preverify_staged_handoff_candidate_at(&s, None, 6, None, Some(7), None, &|_| ())
        .unwrap_err();
    assert!(crate::is_passing_refusal(&absent), "{absent}");

    // The sibling's swap landed and the running build caught up with it.
    write_ready(&s, 20);
    let not_newer =
        preverify_staged_handoff_candidate_at(&s, None, 20, None, None, None, NEVER_READ)
            .unwrap_err();
    assert!(crate::is_passing_refusal(&not_newer), "{not_newer}");

    // A newer stage was published mid-attempt.
    let replaced =
        preverify_staged_handoff_candidate_at(&s, None, 10, None, Some(19), None, NEVER_READ)
            .unwrap_err();
    assert!(
        replaced.contains("not the authorized build") && crate::is_passing_refusal(&replaced),
        "{replaced}"
    );

    // A marker present but unreadable (a refused read stands in for EMFILE).
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(&s.ready, std::fs::Permissions::from_mode(0o000)).unwrap();
        // (root reads a 0o000 file, so there the case is not observable.)
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

    // NEGATIVE CONTROLS: verdicts keep their verdict.
    let unverifiable =
        preverify_staged_handoff_candidate_at(&s, None, 10, None, Some(20), None, NEVER_READ)
            .unwrap_err();
    assert!(!crate::is_passing_refusal(&unverifiable), "{unverifiable}");
    std::fs::write(s.floor(), "min_build = 21\n").unwrap();
    let yanked =
        preverify_staged_handoff_candidate_at(&s, None, 10, None, Some(20), None, NEVER_READ)
            .unwrap_err();
    assert!(
        yanked.contains("floor") && !crate::is_passing_refusal(&yanked),
        "{yanked}"
    );
    std::fs::remove_file(s.floor()).unwrap();
    crate::manifest::quarantine_artifact(&s, 20, &"ab".repeat(32));
    let quarantined =
        preverify_staged_handoff_candidate_at(&s, None, 10, None, Some(20), None, NEVER_READ)
            .unwrap_err();
    assert!(
        quarantined.contains("quarantined") && !crate::is_passing_refusal(&quarantined),
        "{quarantined}"
    );

    let _ = std::fs::remove_dir_all(root);
}

/// The floor check a worker runs ALONE when a fresh cached pass let it skip the
/// full pre-verification: the same refusal the full check gives, read off the same file, and silent at
/// or above the floor and when no floor was ever written.
#[test]
fn the_floor_check_alone_refuses_only_below_the_floor() {
    let (s, root) = temp_staging();
    assert_eq!(
        handoff_apply_floor_refusal_at(&s, 20),
        None,
        "no floor written"
    );
    std::fs::write(s.floor(), "min_build = 21\n").unwrap();
    let refusal = handoff_apply_floor_refusal_at(&s, 20).expect("20 is below 21");
    assert!(
        refusal.contains("below the operator apply floor 21") && refusal.contains("yanked"),
        "{refusal}"
    );
    assert_eq!(handoff_apply_floor_refusal_at(&s, 21), None, "at the floor");
    assert_eq!(
        handoff_apply_floor_refusal_at(&s, 22),
        None,
        "above the floor"
    );
    let _ = std::fs::remove_dir_all(root);
}

#[test]
fn preverify_refuses_a_stage_below_the_operator_floor() {
    let (s, root) = temp_staging();
    write_ready(&s, 20);
    std::fs::write(s.floor(), "min_build = 21\n").unwrap();
    let yanked =
        preverify_staged_handoff_candidate_at(&s, None, 10, None, Some(20), None, NEVER_READ);
    let error = yanked.unwrap_err();
    assert!(
        error.contains("below the operator apply floor 21") && error.contains("yanked"),
        "{error}"
    );
    // Read-only: the stage is still there for the apply to retire.
    assert!(matches!(read_ready(&s, 10), ReadyState::Newer(_)));
    // At the floor, the floor is not the refusal.
    std::fs::write(s.floor(), "min_build = 20\n").unwrap();
    let at_floor =
        preverify_staged_handoff_candidate_at(&s, None, 10, None, Some(20), None, NEVER_READ)
            .unwrap_err();
    assert!(!at_floor.contains("floor"), "{at_floor}");
    let _ = std::fs::remove_dir_all(root);
}

/// A check of the installed bundle that did not finish is not a person's to
/// clear.
///
/// The installed bundle's sealed-identity reader answers what
/// `verified_bundle_identity` answers when its `codesign` outruns the apply
/// budget: the verifier's own `timed_out`, under the "bundle policy:" prefix (the
/// real helper outrunning a real budget is `verify`'s
/// `a_helper_timeout_is_a_passing_refusal`). That refusal must carry the passing
/// key and must not be the installed-source refusal, which suspends automatic
/// apply and asks for a reinstall.
///
/// Control: the real reader, given all the time it needs, refuses the same
/// unsigned bundle as a verdict, and that one is the person's.
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
    // The shape the old wrapping minted — the installed-source key AROUND a
    // passing error — is not a person's either, wherever it is still read.
    let wrapped = rollback_source_refusal(&app, &refusal);
    assert!(is_rollback_source_refusal(&wrapped));
    assert!(!crate::refusal_needs_person(&wrapped), "{wrapped}");

    // CONTROL: the verdict on the same unsigned bundle keeps its remedy.
    let verdict =
        preverify_installed_rollback_source_with(Some(&app), 10, commit, &verified_bundle_identity)
            .expect_err("an unsigned installed bundle is refused");
    assert!(!crate::is_passing_refusal(&verdict), "{verdict}");
    assert!(is_rollback_source_refusal(&verdict), "{verdict}");
    assert!(crate::refusal_needs_person(&verdict), "{verdict}");

    let _ = std::fs::remove_dir_all(root);
}

/// Codesign ended by a signal while checking the INSTALLED bundle judged nothing,
/// so the pre-park refusal is retried, never handed to a person as "reinstall the
/// signed release".
#[test]
fn a_codesign_run_killed_on_the_installed_bundle_is_not_a_person_refusal() {
    let root = std::env::temp_dir().join(format!(
        "aterm-update-installed-codesign-signal-{}-{}",
        std::process::id(),
        unix_now_secs()
    ));
    let app = root.join("aterm.app");
    let _ = unsigned_bundle_with_a_policy(&app, 10);
    let commit = Some("0123456789abcdef0123456789abcdef01234567");
    let killed = |_installed: &Path| -> Result<(u64, String), String> {
        Err(format!(
            "bundle policy: {}",
            crate::verify::codesign_refusal(
                aterm_update_core::codesign::CodesignError::Refused {
                    code: None,
                    stderr: String::new(),
                },
                false,
            )
        ))
    };
    let refusal = preverify_installed_rollback_source_with(Some(&app), 10, commit, &killed)
        .expect_err("a check that did not finish must refuse, never pass");
    assert!(crate::is_passing_refusal(&refusal), "{refusal}");
    assert!(!crate::refusal_needs_person(&refusal), "{refusal}");
    let _ = std::fs::remove_dir_all(root);
}

/// A CODESIGN RUN THAT ENDED WITHOUT A VERDICT IS NOT ONE,
/// driven through the REAL reader and the real `codesign`: an installed bundle
/// whose main executable codesign cannot read just then exits 1 with "Permission
/// denied" — a fact about this machine (`CodesignError::judged_nothing`), never
/// the installed-source refusal that tells a person to reinstall a healthy app.
///
/// CONTROL: the same bundle, readable again, is refused as the VERDICT it is
/// (codesign: "not signed at all"), and that one is still the person's.
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
    // Root reads a mode-000 file, and then the premise does not hold.
    let premise = std::fs::read(&exe).is_err();
    let unread =
        preverify_installed_rollback_source_with(Some(&app), 10, commit, &verified_bundle_identity);
    std::fs::set_permissions(&exe, std::fs::Permissions::from_mode(0o755)).unwrap();
    let verdict =
        preverify_installed_rollback_source_with(Some(&app), 10, commit, &verified_bundle_identity);
    let _ = std::fs::remove_dir_all(&root);

    if premise {
        let refusal = unread.expect_err("a check that did not pass must refuse");
        assert!(refusal.contains("Permission denied"), "{refusal}");
        assert!(crate::is_passing_refusal(&refusal), "{refusal}");
        assert!(!is_rollback_source_refusal(&refusal), "{refusal}");
        assert!(
            !crate::refusal_needs_person(&refusal),
            "codesign could not read the bundle, so nothing is asked of a person: {refusal}"
        );
    }
    let verdict = verdict.expect_err("an unsigned installed bundle is refused");
    assert!(verdict.contains("not signed at all"), "{verdict}");
    assert!(!crate::is_passing_refusal(&verdict), "{verdict}");
    assert!(is_rollback_source_refusal(&verdict), "{verdict}");
    assert!(crate::refusal_needs_person(&verdict), "{verdict}");
}

/// A TAMPERED BUNDLE IS STILL A VERDICT, driven through the REAL reader and the
/// real `codesign`. A bundle modified after signing — here its handoff policy, the
/// resource the successor's whole carry rests on — is not "could not be verified
/// just then". Only a run that knowably judged nothing is passing, so the seal's
/// words — "a sealed resource is missing or invalid" (modified or added), "invalid
/// Info.plist", "unsealed contents present in the bundle root" — are verdicts on
/// the stage and the installed-source refusal on the installed bundle.
///
/// CONTROL: the same bundle as signed passes the seal (its build and commit read).
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
    // What codesign says of the bundle, and the change after signing that makes it.
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
    // The pinned lane checks the seal before the requirement (measured), so
    // either lane reads the tamper first; a pinned build also refuses the ad hoc
    // signer of the untouched bundle, a verdict too — the control is then that
    // verdict, not a pass.
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
        assert!(stage.contains(words), "{words}: {stage}");
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

/// The pre-park check asks whether the install location can be written, so a
/// read-only install never costs a park and a structural failure booked against a
/// healthy build. The refusal comes before the park, names the
/// folder and the remedy, and is the person's to clear. CONTROL: the same
/// install in a writable folder passes.
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
    assert!(refusal.contains("not writable"), "{refusal}");
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
    assert!(crate::refusal_needs_person(&install_location_refusal(
        Path::new("/Applications/aterm.app")
    )));
}

/// A write that failed for a moment (a full disk) is a passing refusal on every
/// lane that asks, never the person's "move aterm".
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
    assert!(refusal.contains("No space left on device"), "{refusal}");
    // CONTROL: a refused write is still the person's.
    let denied = install_location_verdict(
        app,
        bundle::classify_probe_error(
            Path::new("/Applications"),
            &std::io::Error::from_raw_os_error(libc::EACCES),
        ),
    )
    .expect_err("a denied write refuses");
    assert!(crate::refusal_needs_person(&denied), "{denied}");
    // CONTROL: the check itself answers Ok for a writable folder.
    let root = std::env::temp_dir().join(format!(
        "aterm-update-rw-install-{}-{}",
        std::process::id(),
        SEQ.fetch_add(1, Ordering::Relaxed)
    ));
    let app = root.join("aterm.app");
    std::fs::create_dir_all(&app).unwrap();
    let checked = install_location_check(&app);
    let _ = std::fs::remove_dir_all(&root);
    assert_eq!(checked, Ok(()));
}

/// Only a verdict discards a stage or memos the artifact as failed: the cold
/// launch's step 6 and step 7b keep a stage whose re-verification or start probe
/// merely ran out of time. Passing shapes minted for real (the apply lock held
/// past its wait, a copy past its limit, a helper past the apply budget) are
/// kept; a codesign rejection is not.
#[test]
fn a_stage_check_that_did_not_finish_is_not_a_verdict_on_the_bytes() {
    let held = std::env::temp_dir().join(format!(
        "aterm-update-held-apply-lock-{}-{}",
        std::process::id(),
        unix_now_secs()
    ));
    let holder = FileLock::acquire(&held).expect("take the lock as a sibling would");
    let waited = FileLock::acquire_within(&held, std::time::Duration::from_millis(40))
        .err()
        .expect("a held lock refuses the bounded wait");
    let lock = crate::verify::lock_wait_refusal("lock", &waited);
    drop(holder);
    let _ = std::fs::remove_file(&held);

    let copy = crate::verify::status_bounded_with_stderr_observed(
        aterm_uds::spawnfd::command("/bin/sleep").arg("30"),
        "ditto to fixed swap path",
        std::time::Duration::from_millis(50),
    )
    .expect_err("a copy past its limit refuses")
    .message;
    let budget = crate::verify::timed_out("candidate --version probe", true);
    for passing in [&lock, &copy, &budget] {
        assert!(
            crate::is_passing_refusal(passing),
            "a moment must be marked passing: {passing}"
        );
        assert!(
            !stage_failure_is_a_verdict(passing),
            "a moment must not discard the stage: {passing}"
        );
    }
    let verdict = "bundle policy: codesign --verify (structural) failed: code object is not \
                   signed at all";
    assert!(stage_failure_is_a_verdict(verdict));
}

/// The handoff-policy reader for a pre-verification that must refuse: any read is
/// the failure (no refused bundle's policy is ever read).
const NEVER_READ: &dyn Fn(&Path) -> aterm_update_core::handoff_policy::PolicyRead = &|path| {
    panic!(
        "a refused candidate's handoff policy was read: {}",
        path.display()
    )
};

/// A staged `.app` shaped like a release (an `Info.plist` naming `build` and a
/// handoff policy asking every producer for blank screens) that carries no
/// signature. Returns the policy it holds, as a reader would parse it.
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

/// A handoff policy in an unverified bundle is never read: its authority is the
/// signature that seals it with the successor's code, so the pre-verification
/// reads it only after every check passed. Here the codesign policy refuses an
/// unsigned stage whose marker, plist and policy all look right, and the reader
/// must never run, for both a staged download and an installed bundle an
/// activation would exec.
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

/// An activation's pre-verify runs under the apply lock, as the staged lane's
/// does: a sibling's swap renames a new bundle onto the installed path while
/// holding it, and the check and the policy read must land on one side of it.
/// Here a sibling holds the lock for 400 ms, and the pre-verify must not verify
/// or read anything until it lets go.
#[test]
fn an_activation_pre_verify_waits_out_a_siblings_swap_before_it_verifies() {
    let (s, root) = temp_staging();
    let installed = root.join("Applications").join("aterm.app");
    let _ = unsigned_bundle_with_a_policy(&installed, 20);
    let sibling = FileLock::acquire(&s.apply_lock).expect("the sibling's swap holds it");
    let hold = std::time::Duration::from_millis(400);
    let started = std::time::Instant::now();
    let releaser = std::thread::spawn(move || {
        std::thread::sleep(hold);
        drop(sibling);
    });
    let refused =
        crate::preverify_installed_locked(Some(&s), &installed, 10, 20, "0123456789ab", NEVER_READ);
    let waited = started.elapsed();
    releaser.join().expect("the sibling let go");
    assert!(
        waited >= hold,
        "the check ran inside the sibling's swap ({waited:?} < {hold:?})"
    );
    assert!(
        refused
            .as_ref()
            .is_err_and(|why| why.contains("bundle policy")),
        "then it verified, and refused the unsigned bundle: {refused:?}"
    );
    let _ = std::fs::remove_dir_all(root);
}

/// The pre-verification is timed as a whole: the fork lane waits up to a ceiling
/// derived from [`crate::HANDOFF_PREVERIFY_BOUND`] (lock wait plus verification
/// budget), which is a bound only if the check really runs under the budget.
/// Entering a pre-verification holds the apply lock and opens a budget no longer
/// than `APPLY_BUDGET`, both released on drop, and the bound is their sum.
#[test]
fn a_pre_verification_holds_the_lock_and_runs_under_the_verification_budget() {
    assert_eq!(
        crate::HANDOFF_PREVERIFY_BOUND,
        APPLY_LOCK_WAIT + verify::APPLY_BUDGET,
        "the bound a waiter derives its ceiling from is the lock wait plus the budget"
    );
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
    // No staging root: nothing to lock, and the budget still opens.
    {
        let _guard = enter_preverify(None).expect("no lock to wait for");
        assert!(verify::current_budget_deadline().is_some());
    }
    let _ = std::fs::remove_dir_all(root);
}

/// Both pre-verifications (the staged candidate's and the activation's) enter
/// through [`enter_preverify`] once and take the apply lock nowhere else, so none
/// can drop the budget. Asserted on source, since the helpers under the budget
/// need a signed bundle to run long.
#[test]
fn both_pre_verifications_enter_through_the_budgeted_door() {
    fn body<'a>(source: &'a str, signature: &str) -> &'a str {
        let from = &source[source.find(signature).expect(signature)..];
        &from[..from.find("\n}\n").expect("end of fn")]
    }
    let install = include_str!("install.rs");
    let install = install
        .split_once("#[cfg(test)]\n#[path = \"install_tests.rs\"]\nmod tests;")
        .map_or(install, |(before, _)| before);
    let lib = include_str!("lib.rs");
    for (what, lane) in [
        (
            "the staged candidate's",
            body(install, "fn preverify_staged_handoff_candidate_at<"),
        ),
        (
            "the activation's",
            body(lib, "fn preverify_installed_locked<"),
        ),
    ] {
        // Split so these assertions do not match themselves.
        assert_eq!(
            lane.matches(concat!("enter_preverify", "(")).count(),
            1,
            "{what} pre-verification enters through enter_preverify, once"
        );
        assert!(
            !lane.contains(concat!("acquire_within", "(")),
            "{what} pre-verification takes the lock only through the budgeted door"
        );
    }
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
    assert_eq!(
        value.get("outcome").and_then(aterm_toml::Value::as_str),
        Some("installed 0.0.54 (build 54); activating now")
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
fn running_rollback_identity_requires_build_and_commit() {
    let running = "0123456789ab";
    let same = "0123456789abcdef0123456789abcdef01234567";
    let other = "fedcba9876543210fedcba9876543210fedcba98";
    assert!(identity_matches_running(53, same, 53, Some(running)));
    assert!(!identity_matches_running(53, other, 53, Some(running)));
    assert!(!identity_matches_running(54, same, 53, Some(running)));
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
    let mut wrong_build = exact;
    wrong_build.build = 55;
    assert!(!ready_matches_expected(&ready, &wrong_build));
}

#[test]
fn inverse_swap_receipt_restore_binds_old_identity_and_fails_closed() {
    let model = aterm_spec::derive::native_update_disk_transaction_model();
    let (staging, root) = temp_staging();
    let old_commit = "c16c6fd7955b0011223344556677889900aabbcc";
    let old_digest = "cd".repeat(32);
    crate::manifest::InstalledReceipt::record(
        &staging.installed_receipt(),
        52,
        old_commit,
        &old_digest,
    )
    .unwrap();
    let previous = previous_receipt_for_sealed_old(&staging, 52, old_commit)
        .expect("exact sealed OLD receipt");
    let mut stale_state = disk_model_ready(&model);
    disk_model_step(&model, &mut stale_state, "CorruptPreviousReceipt");
    assert!(
        previous_receipt_for_sealed_old(&staging, 52, &"f".repeat(40)).is_none(),
        "well-formed but stale local receipt is not rollback authority"
    );
    assert!(
        previous_receipt_for_sealed_old(&staging, 51, old_commit).is_none(),
        "receipt build must bind the sealed OLD build"
    );
    disk_model_step(&model, &mut stale_state, "PrepareFixedNew");
    assert_eq!(stale_state["previous_receipt_saved"], 0);

    crate::manifest::InstalledReceipt::record(
        &staging.installed_receipt(),
        53,
        "0123456789abcdef0123456789abcdef01234567",
        &"ef".repeat(32),
    )
    .unwrap();
    let blocked_tmp = staging
        .installed_receipt()
        .with_extension(format!("toml.{}.tmp", std::process::id()));
    std::fs::create_dir(&blocked_tmp).unwrap();
    let error = restore_installed_receipt(&staging, Some(&previous))
        .expect_err("blocked kind-preserving rewrite must surface failure");
    let mut failure_state = disk_model_ready(&model);
    for action in [
        "PrepareFixedNew",
        "ArmExactTrial",
        "AtomicSwap",
        "RecordExactReceipt",
        "VerifyExactRollback",
        "ExecFails",
        "RestoreExactOld",
        "DisarmRestoredTrialReceiptRestoreFailsClosed",
    ] {
        disk_model_step(&model, &mut failure_state, action);
    }
    assert!(error.contains("superseded receipt cleared"), "{error}");
    assert!(
        !staging.installed_receipt().exists(),
        "failed NEW receipt is cleared instead of surviving as OLD authority"
    );
    assert_eq!(failure_state["receipt_restore_failed"], 1);
    assert_eq!(failure_state["superseded_receipt_cleared"], 1);
    std::fs::remove_dir(&blocked_tmp).unwrap();
    let _ = std::fs::remove_dir_all(root);
}

#[cfg(target_os = "macos")]
#[test]
fn apply_retirement_never_removes_stage_owned_download_scratch() {
    let (s, root) = temp_staging();
    write_ready(&s, 5);
    std::fs::create_dir_all(&s.staged_app).unwrap();
    std::fs::write(s.download.join("aterm-0.0.5.dmg.part"), b"partial").unwrap();
    let _apply = FileLock::acquire(&s.apply_lock).unwrap();
    s.retire_published();
    assert!(!s.ready.exists());
    assert!(!s.staged_app.exists());
    assert_eq!(
        std::fs::read(s.download.join("aterm-0.0.5.dmg.part")).unwrap(),
        b"partial",
        "apply owns only the published generation; stage scratch survives"
    );
    let _ = std::fs::remove_dir_all(root);
}

#[test]
fn concurrent_apply_retirement_cannot_erase_a_newer_stage_publisher() {
    let (s, root) = temp_staging();
    write_ready(&s, 5);
    make_app(&s.staged_app, "OLD-STAGE");

    // Apply acquires only the short publication/swap lock. A stager may keep
    // downloading under stage_lock while this guard is held, but must wait at
    // the final publication boundary.
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
    // Atomic rename cannot replace a directory at the sentinel path.
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

// --- swap / rollback machinery (real same-volume RENAME_SWAP on macOS) --------
//
// The invariants the self-updater must never get wrong: the live install is never
// left missing, the swapped-out OLD bundle is kept as a rollback source, and a
// revert restores the previous build.

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
/// transaction variables. The fixture id stands in only for the separately tested
/// codesign-sealed OLD/NEW identity guard; every exchange, sentinel transition
/// and receipt write uses shipping code.
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
    let fixed_identity = fixture_identity(&fixed);
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

#[test]
fn fixed_swap_places_old_at_the_only_recovery_path_atomically() {
    let root = swap_root("basic");
    let installed = root.join("installed.app");
    let fixed = rollback_path(&installed);
    make_app(&fixed, "NEW");
    make_app(&installed, "OLD");

    let rollback = swap_fixed_candidate(&copied_candidate(&fixed), &installed).expect("fixed swap");
    assert_eq!(read_id(&installed), "NEW");
    assert_eq!(read_id(&rollback), "OLD");
    assert_eq!(rollback, rollback_path(&installed));
    let _ = std::fs::remove_dir_all(root);
}

#[test]
fn restore_rollback_swaps_the_previous_build_back() {
    let root = swap_root("restore");
    let installed = root.join("installed.app");
    let fixed = rollback_path(&installed);
    make_app(&fixed, "NEW");
    make_app(&installed, "OLD");

    let rollback = swap_fixed_candidate(&copied_candidate(&fixed), &installed).unwrap();
    assert_eq!(read_id(&installed), "NEW");
    restore_rollback(&rollback, &installed).unwrap();
    assert_eq!(read_id(&installed), "OLD");
    assert_eq!(read_id(&fixed), "NEW");
    let _ = std::fs::remove_dir_all(root);
}

#[test]
fn fixed_swap_crash_cuts_are_always_discoverable() {
    let root = swap_root("crash-cuts");
    let installed = root.join("installed.app");
    let fixed = rollback_path(&installed);
    make_app(&installed, "OLD");
    make_app(&fixed, "NEW");

    // Pre-swap crash cut: OLD remains installed; prepared NEW is fixed and
    // harmless. There is no transient/pid-only authority.
    assert_eq!(read_id(&installed), "OLD");
    assert_eq!(read_id(&fixed), "NEW");

    swap_fixed_candidate(&copied_candidate(&fixed), &installed).unwrap();
    // Post-swap crash cut: NEW is installed and OLD is already at the exact
    // fixed path recovery probes. No later rename is required.
    assert_eq!(read_id(&installed), "NEW");
    assert_eq!(read_id(&fixed), "OLD");
    restore_rollback(&fixed, &installed).unwrap();
    assert_eq!(read_id(&installed), "OLD");
    assert_eq!(read_id(&fixed), "NEW");
    let _ = std::fs::remove_dir_all(root);
}

#[test]
fn failed_fixed_exchange_leaves_old_install_untouched() {
    let root = swap_root("failed-fixed");
    let installed = root.join("installed.app");
    make_app(&installed, "OLD");
    let error =
        swap_fixed_candidate(&copied_candidate(&root.join("missing.app")), &installed).unwrap_err();
    assert!(error.contains("changed into a symlink/non-directory"));
    assert_eq!(read_id(&installed), "OLD");
    let _ = std::fs::remove_dir_all(root);
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

    let error = swap_fixed_candidate(&copied_candidate(&fixed), &installed).unwrap_err();
    assert!(error.contains("symlink/non-directory"));
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

    let error = restore_rollback(&rollback, &installed).unwrap_err();
    assert!(error.contains("symlink/non-directory"));
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
/// cross-volume install (the candidate copied to the fixed path, the stage left
/// intact) the stage comes out exactly as it stood: the published bytes kept and
/// the redundant fixed copy reclaimed, never both deleted by a failed EXDEV move.
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
    let retained = swap_fixed_candidate(&prepared, &installed).unwrap();
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

    assert!(confirm_health_under_apply_lock_with_proof(
        &s,
        1000,
        &installed,
        || Ok(())
    ));
    assert_eq!(sentinel.read_state(), None, "matching confirm disarms");
    assert!(
        !s.trial().exists(),
        "health-only crash-loop state is cleared"
    );
    assert!(!s.staged_app.exists(), "matching owner may GC swap orphan");
    assert!(!rollback.exists(), "matching owner may GC rollback");
    assert!(
        s.installed_receipt().exists(),
        "exact installed proof survives confirm-before-reconcile ordering"
    );
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
/// not running the installed build: an overlapping process from another build has
/// no authority to disarm anything.
#[test]
fn dead_authority_and_abandoned_preswap_shapes_are_disjoint() {
    // Out-of-band replacement: armed BELOW the build that is installed and
    // running. Nothing in the tree could ever clear this one before.
    assert!(dead_authority_trial(1001, 1001, 1000));
    assert!(!abandoned_preswap_trial(1001, 1001, 1000));
    // The pre-swap crash cut the exact recovery was written for: armed ABOVE.
    assert!(abandoned_preswap_trial(1000, 1000, 1001));
    assert!(!dead_authority_trial(1000, 1000, 1001));
    // Neither fires for a process that is not the installed build.
    assert!(!dead_authority_trial(999, 1001, 1000));
    assert!(!abandoned_preswap_trial(999, 1000, 1001));
}

// --- a newer stage supersedes an unconfirmed activation ------------------------
//
// A fixture bundle carries its sealed identity in a `sealed` file ("<build>
// <commit>"), the one stand-in for the codesign read `verified_bundle_identity`
// performs in production. Every exchange, sentinel transition, marker and receipt
// write is the shipping code.

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

/// THE DISK A FAILED LAUNCHED-LANE HANDOFF LEAVES BEHIND, with a newer release
/// staged: build 11 INSTALLED and mid-trial (two counted launches — a structural
/// `ChildDied` keeps its launch), the running build 10 retained at the fixed
/// rollback path, 11's receipt and trial marker, and build 12 staged and ready.
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
/// it: the running build is back at the install path, the activation's trial is
/// disarmed at its two counted launches (never a third, which `check_boot_health`
/// would revert and poison on), nothing is quarantined, the stage is untouched,
/// and the boot apply's preconditions hold for the next candidate (the installed
/// bundle is the running build, no trial owns the fixed path, `ready.toml` names
/// the newer build).
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

    // …and it is now an ordinary stage over the running build.
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

/// ANOTHER WINDOW'S CANDIDATE OF THE ACTIVATION IS STILL RUNNING. Several windows
/// of one build each run their own candidate of the installed activation, and the
/// trial's launcher record knows one is alive. The retire must not disarm the
/// trial and delete the bundle under a candidate that could still prove and
/// Commit: it waits, for the moment, and moves nothing.
///
/// NEGATIVE CONTROL: once that candidate is gone, the same desk is retired.
#[test]
fn a_supersede_waits_while_another_windows_candidate_of_the_activation_runs() {
    // `spawnfd`'s: the stand-in holds its stdio and nothing else of this
    // process, not another test thread's pipe for as long as it lives.
    use aterm_uds::spawnfd::Command;
    let (s, root, installed) = superseded_activation_desk();
    let mut sibling = Command::new("/bin/sleep")
        .arg("30")
        .spawn()
        .expect("start a stand-in candidate");
    // The other window's candidate booted: its launch is on the record, counted
    // or not (here the burst rule keeps the count where it was).
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
    // A stand-in that must die dies by SIGKILL (AGENTS.md).
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

    // The candidate is gone: nothing holds the retire any more.
    let retired = supersede_desk(&s, &installed, (10, SUPERSEDE_RUNNING_COMMIT), 12)
        .expect("with no live candidate the newer stage supersedes the activation");
    assert_eq!(retired.activation_build, 11);
    assert_eq!(
        sealed_fixture_identity(&installed),
        Ok((10, SUPERSEDE_RUNNING_COMMIT.to_string()))
    );
    let _ = std::fs::remove_dir_all(root);
}

/// U56's second path: a window whose fresh cached pass for the activation lets
/// its worker skip the full check, after another window retired that activation
/// and put the running build back at the install path. The launch would start
/// that older build, whose boot apply takes the newer stage this attempt never
/// authorized. The cached pass now asks which build the install path holds, and
/// a moved path (or a read that fails) is a moment, never a verdict.
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
    assert!(
        crate::is_passing_refusal(&moved) && moved.contains("build 10"),
        "{moved}"
    );
    let unread = handoff_activation_cached_pass_refusal_at(&s, 11, || Some(Err("EIO".to_string())))
        .expect("an unread install path refuses");
    assert!(crate::is_passing_refusal(&unread), "{unread}");
    // The floor is still asked first.
    crate::manifest::Floor::bump_and_write(&s.floor(), 12, 0, 0);
    assert!(
        handoff_activation_cached_pass_refusal_at(&s, 11, || Some(Ok(11)))
            .is_some_and(|refusal| refusal.contains("floor")),
        "a yanked activation is refused whatever the install path holds"
    );
    let _ = std::fs::remove_dir_all(root);
}

/// The crash cut: the exchange happened and the disarm did not (the staging root
/// refuses the sentinel's removal). The disk is then exactly the "crash right
/// after inverse rollback" shape `recover_abandoned_preswap_trial_if_exact`
/// finishes (installed = the running build, fixed = the activation, the trial
/// armed for it, owned by this install and authorized by its receipt), and the
/// sentinel still reads two launches. It is reported as the retire it was: the
/// newer stage applies next as after a clean retire.
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
    assert!(
        retired
            .trial_disarm_deferred
            .as_deref()
            .is_some_and(|why| why.contains("could not be disarmed")
                && why.contains("the next boot apply finishes it")),
        "{retired:?}"
    );

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
    // Every conjunct `recover_abandoned_preswap_trial_if_exact` checks past the
    // sealed-identity reads.
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
/// from the one shape the transaction can undo refuses, leaving the activation
/// installed, its trial armed at the same count, the predecessor retained, and
/// the stage in place.
#[test]
fn a_supersede_moves_nothing_unless_every_precondition_holds() {
    type Mutation = fn(&Staging, &Path, &Path);
    type Case = (
        &'static str,
        Mutation,
        (u64, &'static str),
        u64,
        &'static str,
        bool,
    );
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
    // Rows: (why, the disk's difference, the running identity, the newer build,
    // the refusal's expected words, whether it is about the moment rather than the
    // disk's shape). A bundle whose sealed identity cannot be read stands in for a
    // verification that failed or ran out of its budget.
    fn unverifiable(app: &Path) {
        std::fs::remove_file(app.join("sealed")).unwrap();
    }
    let cases: [Case; 14] = [
        (
            "not newer",
            |_, _, _| {},
            (10, SUPERSEDE_RUNNING_COMMIT),
            11,
            "is not newer than it",
            false,
        ),
        (
            "the marker names another build",
            |s, _, _| write_ready_for(s, 13, SUPERSEDE_NEWER_COMMIT),
            (10, SUPERSEDE_RUNNING_COMMIT),
            12,
            "no verified stage of build 12",
            true,
        ),
        (
            "the staged bundle is not the marker's",
            |s, _, _| sealed_fixture(&s.staged_app, 13, SUPERSEDE_NEWER_COMMIT),
            (10, SUPERSEDE_RUNNING_COMMIT),
            12,
            "not the marker's build 12",
            true,
        ),
        (
            "the staged bundle does not verify",
            |s, _, _| unverifiable(&s.staged_app),
            (10, SUPERSEDE_RUNNING_COMMIT),
            12,
            "the staged bundle does not verify",
            true,
        ),
        (
            "the stage is yanked",
            |s, _, _| std::fs::write(s.floor(), "min_build = 13\n").unwrap(),
            (10, SUPERSEDE_RUNNING_COMMIT),
            12,
            "below the operator floor 13",
            false,
        ),
        (
            "the installed bundle is not the activation",
            |_, installed, _| sealed_fixture(installed, 13, SUPERSEDE_NEWER_COMMIT),
            (10, SUPERSEDE_RUNNING_COMMIT),
            12,
            "sealed 13, not the activation",
            false,
        ),
        (
            "the installed bundle does not verify",
            |_, installed, _| unverifiable(installed),
            (10, SUPERSEDE_RUNNING_COMMIT),
            12,
            "the installed bundle does not verify",
            true,
        ),
        (
            "the trial is armed for another build",
            |s, _, _| boot_sentinel(s).arm(12).unwrap(),
            (10, SUPERSEDE_RUNNING_COMMIT),
            12,
            "armed for build 12, not for it",
            false,
        ),
        (
            "no trial is armed",
            |s, _, _| boot_sentinel(s).confirm().unwrap(),
            (10, SUPERSEDE_RUNNING_COMMIT),
            12,
            "no boot trial is armed",
            false,
        ),
        (
            "the trial belongs to another install",
            other_install,
            (10, SUPERSEDE_RUNNING_COMMIT),
            12,
            "belongs to another install",
            false,
        ),
        (
            "the trial marker names other bytes",
            |s, _, _| {
                crate::manifest::InstalledReceipt::clear(&s.installed_receipt());
            },
            (10, SUPERSEDE_RUNNING_COMMIT),
            12,
            "does not name these bytes",
            false,
        ),
        (
            "the retained predecessor is not the running build",
            |_, installed, _| {
                sealed_fixture(&rollback_path(installed), 9, SUPERSEDE_RUNNING_COMMIT);
            },
            (10, SUPERSEDE_RUNNING_COMMIT),
            12,
            "is 9/1111111111111111111111111111111111111111, not the running 10/",
            false,
        ),
        (
            "the retained predecessor does not verify",
            |_, installed, _| unverifiable(&rollback_path(installed)),
            (10, SUPERSEDE_RUNNING_COMMIT),
            12,
            "the retained predecessor does not verify",
            true,
        ),
        (
            "the running build is another commit",
            |_, _, _| {},
            (10, "4444444444444444444444444444444444444444"),
            12,
            "not the running 10/4444444444444444444444444444444444444444",
            false,
        ),
    ];
    for (why, mutate, running, newer, words, for_now) in cases {
        let (s, root, installed) = superseded_activation_desk();
        mutate(&s, &installed, &root);
        let installed_before = sealed_fixture_identity(&installed);
        let fixed_before = sealed_fixture_identity(&rollback_path(&installed));
        let sentinel_before = boot_sentinel(&s).read_state();
        let refusal = supersede_desk(&s, &installed, running, newer)
            .expect_err(&format!("{why}: must refuse"));
        let error = refusal.reason();
        assert!(
            error.contains("stays in place") && error.contains(words),
            "{why}: {error}"
        );
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
/// sentinel for a build no longer installed. It can never be confirmed and must
/// never be reverted, so the terminal transition disarms it; otherwise every
/// future apply is refused.
#[test]
fn a_trial_armed_below_the_running_build_is_retired_not_kept_forever() {
    let (s, root) = temp_staging();
    let commit = "0123456789abcdef0123456789abcdef01234567";
    let sentinel = boot_sentinel(&s);
    sentinel.arm(1000).unwrap();
    crate::manifest::FailedMark::record(&s.trial(), 1000, &"ab".repeat(32));
    // A receipt for the DEAD trial's build — not proof for what is installed.
    crate::manifest::InstalledReceipt::record(
        &s.installed_receipt(),
        1000,
        commit,
        &"ab".repeat(32),
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
    assert!(
        std::fs::read_to_string(&s.status)
            .unwrap()
            .contains("updates re-enabled"),
        "the repair is durable and explains itself"
    );
    let _ = std::fs::remove_dir_all(root);
}

/// A trial belongs to the install it swapped. A same-build process launched from
/// another bundle (`~/Applications/aterm.app` beside `/Applications/aterm.app`)
/// must not count launches against it, confirm it, or disarm it as dead
/// authority. Markers without a recorded root stay owned by whoever asks.
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
    // A sibling launch of the trialed build does not count against it
    // (`check_boot_health` returns before `observe_launch` for a non-owner:
    // exercised through the pure predicate here, and through
    // `recover_abandoned_preswap_trial_if_exact` below).
    assert_eq!(sentinel.read_state(), Some((6, 0)));
    // …and a sibling that is NEWER than the armed build may not retire it as
    // dead authority either: the pure gate is the ownership test.
    assert!(dead_authority_trial(7, 7, 6) && !trial_owned_by(&s, &sibling));
    // The abandoned-pre-swap gate: the owner may recover, a same-build
    // sibling may not. Drop the ownership term and the second assert fails.
    assert!(may_recover_abandoned_preswap(&s, &owner, (6, 6, 7), true));
    assert!(!may_recover_abandoned_preswap(
        &s,
        &sibling,
        (6, 6, 7),
        true
    ));
    // The owner install GONE (moved/deleted mid-trial): whoever runs the build
    // now inherits the trial, so it can still be counted, confirmed or escaped.
    std::fs::remove_dir_all(&owner).unwrap();
    assert!(trial_owned_by(&s, &sibling), "a ghost owns nothing");
    // A marker with no recorded root is owned by whoever asks.
    crate::manifest::FailedMark::record(&s.trial(), 6, &"ab".repeat(32));
    assert!(trial_owned_by(&s, &sibling) && trial_owned_by(&s, &owner));
    let _ = std::fs::remove_dir_all(root);
}

/// The unrecoverable mismatched-sentinel shapes get a budgeted escape counted in
/// a separate file: incrementing the boot sentinel from another build's launches
/// could revert a build that never crashed.
#[test]
fn an_unrecoverable_foreign_trial_is_disarmed_on_a_budget_not_on_sight() {
    let (s, root) = temp_staging();
    let sentinel = boot_sentinel(&s);
    sentinel.arm(1000).unwrap();
    crate::manifest::FailedMark::record(&s.trial(), 1000, &"ab".repeat(32));

    for launch in 1..MAX_BOOT_ATTEMPTS {
        assert!(
            !escape_wedged_foreign_trial(&s, 1001, 1000),
            "launch {launch} is inside the budget"
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
    assert!(
        std::fs::read_to_string(&s.status)
            .unwrap()
            .contains("updates re-enabled")
    );
    let _ = std::fs::remove_dir_all(root);
}

/// A trial owned by another install that still exists is not disarmed by a
/// sibling's exhausted budget unless that owner has been idle for a day; an owner
/// that is gone cedes at the budget.
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
    // The owner install GONE: the next exhausted budget disarms (ghost cedes).
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
/// root is per user, so another copy shares this sentinel, and disarming its
/// trial would strip crash-loop protection from the build that is crash-looping.
/// The owner advancing its attempt count proves it alive, so a budget that spans
/// such an advance restarts.
#[test]
fn a_foreign_trial_whose_owner_is_still_launching_is_never_disarmed() {
    let (s, root) = temp_staging();
    let sentinel = boot_sentinel(&s);
    sentinel.arm(1000).unwrap();

    // Run the budget to one launch short of the escape, repeatedly, with the
    // trial's owner observing a launch each time (a crash-looping second
    // installation). The escape must never fire.
    for round in 0..4 {
        for _ in 1..MAX_BOOT_ATTEMPTS {
            assert!(
                !escape_wedged_foreign_trial(&s, 1001, 1000),
                "round {round}: inside the budget"
            );
        }
        // The other copy launches: `observe_launch` counts because IT is build 1000.
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

    // Negative control: the same launches with the owner gone (nothing advances
    // the sentinel) reach the escape, so the guard above is what holds.
    std::thread::sleep(std::time::Duration::from_millis(10));
    let mut fired = false;
    for _ in 0..MAX_BOOT_ATTEMPTS + 1 {
        fired |= escape_wedged_foreign_trial(&s, 1001, 1000);
    }
    assert!(fired, "an abandoned trial is still escaped");
    assert_eq!(sentinel.read_state(), None);
    let _ = std::fs::remove_dir_all(root);
}

/// A new armed build restarts the budget: launches counted against a replaced
/// trial must not carry over and disarm the new one early.
#[test]
fn the_foreign_trial_budget_does_not_accumulate_across_trials() {
    let (s, root) = temp_staging();
    let sentinel = boot_sentinel(&s);
    sentinel.arm(1000).unwrap();
    for _ in 1..MAX_BOOT_ATTEMPTS {
        assert!(!escape_wedged_foreign_trial(&s, 1001, 1000));
    }
    // A different build is armed now (the previous one was resolved).
    sentinel.arm(1002).unwrap();
    assert!(
        !escape_wedged_foreign_trial(&s, 1001, 1002),
        "the new trial gets the full budget, not the tail of the old one"
    );
    assert_eq!(sentinel.read_state(), Some((1002, 0)));
    let _ = std::fs::remove_dir_all(root);
}

/// Only a refusal is recorded as one: the every-launch outcomes would rewrite
/// `status.toml` on every boot, and a re-exec failure belongs in the failure
/// ledger (`record_apply_failure` clears refusals, never the reverse).
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
    )
    .unwrap();
    assert!(
        !trial_authorizes_candidate(&s, 1000, commit),
        "same build+commit with a different DMG must not authorize health or recovery"
    );
    let _ = std::fs::remove_dir_all(root);
}

/// Tier-1 conformance for the native update disk machine: drives the real
/// fixed-path exchange, inverse rollback, sentinel, exact receipt, startup
/// authority reducer and health cleanup, projecting each observation onto the
/// model Tier-0 proves and bug-mutates.
#[test]
fn native_update_disk_transaction_model_conforms_to_real_swap_recovery_and_health_guards() {
    let model = aterm_spec::derive::native_update_disk_transaction_model();
    let commit = "0123456789abcdef0123456789abcdef01234567";
    let other_commit = "fedcba9876543210fedcba9876543210fedcba98";
    let digest = "ab".repeat(32);
    let build = 1000;

    // Startup authority must not return before the real boot-health lane has run:
    // a malformed expected-artifact tuple and a matching re-exec both produce the
    // observe verdict before observation (the production negative control).
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
    let mut malformed = model.init_state();
    for action in [
        "InheritMalformedAuthority",
        "ConsumeStartupAuthority",
        "ObserveBootHealth",
        "ReturnAfterObservedAuthority",
    ] {
        disk_model_step(&model, &mut malformed, action);
    }

    // The post-swap startup lane: `ready.toml` is still present, so the receipt is
    // rebuilt from evidence the updater writes. This binds that recovery to the
    // model, then proves each corrupted disk fact disables it.
    let mut modern_state = model.init_state();
    for action in [
        "EnterLegacyPostSwapExact",
        "SupplyModernReadyRecovery",
        "ConsumeStartupAuthority",
        "RecoverModernReceiptFromReady",
    ] {
        disk_model_step(&model, &mut modern_state, action);
    }
    assert_eq!(modern_state["receipt_exact"], 1);
    assert_eq!(modern_state["modern_receipt_recovered"], 1);

    // Each corrupted disk fact must leave its refusal enabled and preserve
    // recovery authority (trial armed, rollback fixed, no receipt minted), so a
    // later launch can act on unchanged evidence; disabling both the recovery and
    // its refusal would strand the machine.
    //
    // `legacy_variant` is the model's mutex: modern ready evidence and a corrupted
    // fact are exclusive scenarios, so these runs do not supply ready; with
    // `ready_present == 0`, `RecoverModernReceiptFromReady` is disabled by its
    // guard.
    for (corrupt, refuse) in [
        ("CorruptLegacySentinel", "RefuseLegacySentinelMismatch"),
        (
            "CorruptLegacyCurrentBuild",
            "RefuseLegacyCurrentBuildMismatch",
        ),
        (
            "CorruptLegacyCurrentCommit",
            "RefuseLegacyCurrentCommitMismatch",
        ),
        ("CorruptLegacyTrialBuild", "RefuseLegacyTrialBuildMismatch"),
        (
            "CorruptLegacyTrialDigest",
            "RefuseLegacyTrialDigestMismatch",
        ),
        ("CorruptLegacyRollback", "RefuseLegacyRollbackMismatch"),
    ] {
        let mut rejected_state = model.init_state();
        for action in [
            "EnterLegacyPostSwapExact",
            corrupt,
            "ConsumeStartupAuthority",
        ] {
            disk_model_step(&model, &mut rejected_state, action);
        }
        assert!(
            model
                .successors("RecoverModernReceiptFromReady", &rejected_state)
                .is_empty(),
            "{corrupt} left receipt recovery model-enabled"
        );
        disk_model_step(&model, &mut rejected_state, refuse);
        assert_eq!(rejected_state["trial"], 1);
        assert_eq!(rejected_state["fixed"], 1);
        assert_eq!(rejected_state["receipt"], 0);
    }

    // OLD authority is build+commit, not build-only. Likewise NEW handoff
    // authority is the exact marker build+commit+digest tuple.
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
    let mut wrong_old = disk_model_ready(&model);
    disk_model_step(&model, &mut wrong_old, "CorruptOldCommit");
    assert!(
        model.successors("PrepareFixedNew", &wrong_old).is_empty(),
        "healthy model must reject the same build with the wrong commit"
    );
    let ready = Ready {
        build_number: build,
        version: "1.0.1000".into(),
        commit: Some(commit.into()),
        dmg_sha256: digest.clone(),
        team_id: "T".into(),
        staged_at: String::new(),
        changelog: None,
        machine_id: None,
        roster_seq: None,
        roster_confirmed_at: Some(crate::install::now_rfc3339()),
    };
    let exact = ExpectedArtifact {
        build,
        commit: commit.into(),
        dmg_sha256: digest.clone(),
    };
    assert!(ready_matches_expected(&ready, &exact));
    let mut wrong_new = exact.clone();
    wrong_new.commit = other_commit.into();
    assert!(!ready_matches_expected(&ready, &wrong_new));

    // A genuine failed fixed exchange plus shipping disarm/recovery leaves
    // canonical OLD intact and no partial trial authority.
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
    assert!(swap_fixed_candidate(&copied_candidate(&missing), &failed_installed).is_err());
    // Negative control: a two-rename "swap" failing between its renames would
    // leave NEW installed and OLD off the fixed path with the trial armed. The
    // real exchange just failed as one step; the projection below shows
    // canonical OLD intact.
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

    // Successful fixed-path transaction and exact receipt.
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
    // Negative control: the receipt is recorded only after the exchange below.
    // Written here, it would name NEW over an installed OLD.
    let early_receipt = buggy.successors("BuggyWriteReceiptBeforeSwap", &state)[0].clone();
    assert!(!buggy.check_invariant("ReceiptBindsExactNewIdentity", &early_receipt));

    swap_fixed_candidate(&copied_candidate(&fixed), &installed).unwrap();
    disk_model_step(&model, &mut state, "AtomicSwap");
    assert_real_disk_projection(&state, &staging, &installed, build, commit, &digest);

    crate::manifest::InstalledReceipt::record(&staging.installed_receipt(), build, commit, &digest)
        .unwrap();
    std::fs::remove_file(&staging.ready).unwrap();
    disk_model_step(&model, &mut state, "RecordExactReceipt");
    disk_model_step(&model, &mut state, "VerifyExactRollback");
    assert_real_disk_projection(&state, &staging, &installed, build, commit, &digest);

    // The GUI first-present edge precedes the worker proof. A failed proof is
    // a strict no-op on every recovery artifact, then a retry may prove,
    // disarm, and garbage-collect in that order.
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
    assert!(!staging.staged_app.exists() && !fixed.exists());
    drop(_apply);
    let _ = std::fs::remove_dir_all(root);

    // Exec-failure inverse rollback: a rejected/missing rollback input leaves
    // NEW+exact OLD untouched; the genuine fixed path then restores OLD and
    // successful disarm permits cleanup.
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
    swap_fixed_candidate(&copied_candidate(&rollback_fixed), &rollback_installed).unwrap();
    disk_model_step(&model, &mut rollback_state, "AtomicSwap");
    crate::manifest::InstalledReceipt::record(
        &rollback_staging.installed_receipt(),
        build,
        commit,
        &digest,
    )
    .unwrap();
    for action in ["RecordExactReceipt", "VerifyExactRollback", "ExecFails"] {
        disk_model_step(&model, &mut rollback_state, action);
    }
    assert!(
        restore_rollback(
            &rollback_root.join("missing-rollback.app"),
            &rollback_installed,
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
    restore_rollback(&rollback_fixed, &rollback_installed).unwrap();
    disk_model_step(&model, &mut rollback_state, "RestoreExactOld");
    rollback_sentinel.confirm().unwrap();
    crate::manifest::FailedMark::clear(&rollback_staging.trial());
    std::fs::remove_dir_all(&rollback_fixed).unwrap();
    restore_installed_receipt(&rollback_staging, None).unwrap();
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

// -----------------------------------------------------------------------
// The zip extraction bound. These build central directories by hand, since the
// shapes that matter are ones a real archiver will not produce: a saturated
// ZIP64 sentinel with nothing behind it, a directory cut mid-header, a size
// field no filesystem could satisfy.
// -----------------------------------------------------------------------

/// One central directory file header for `name`, declaring `uncompressed`
/// bytes and carrying `extra` as its extra field.
fn central_header(name: &str, uncompressed: u32, extra: &[u8]) -> Vec<u8> {
    let mut header = Vec::new();
    header.extend_from_slice(&CENTRAL_HEADER_SIGNATURE.to_le_bytes());
    header.extend_from_slice(&[0u8; 4]); // version made by + version needed
    header.extend_from_slice(&[0u8; 2]); // general purpose flags
    header.extend_from_slice(&[0u8; 2]); // method (stored)
    header.extend_from_slice(&[0u8; 4]); // mod time + mod date
    header.extend_from_slice(&[0u8; 4]); // crc32
    header.extend_from_slice(&0u32.to_le_bytes()); // compressed size
    header.extend_from_slice(&uncompressed.to_le_bytes());
    header.extend_from_slice(&u16::try_from(name.len()).unwrap().to_le_bytes());
    header.extend_from_slice(&u16::try_from(extra.len()).unwrap().to_le_bytes());
    header.extend_from_slice(&0u16.to_le_bytes()); // file comment length
    header.extend_from_slice(&[0u8; 2]); // disk number start
    header.extend_from_slice(&[0u8; 2]); // internal attributes
    header.extend_from_slice(&[0u8; 4]); // external attributes
    header.extend_from_slice(&0u32.to_le_bytes()); // local header offset
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
/// fields, so a test can declare a sentinel or a deliberate mismatch.
fn eocd(entries: u16, size: u32, offset: u32, comment: &[u8]) -> Vec<u8> {
    let mut record = Vec::new();
    record.extend_from_slice(&EOCD_SIGNATURE.to_le_bytes());
    record.extend_from_slice(&0u16.to_le_bytes()); // this disk
    record.extend_from_slice(&0u16.to_le_bytes()); // disk with the directory
    record.extend_from_slice(&entries.to_le_bytes()); // entries on this disk
    record.extend_from_slice(&entries.to_le_bytes()); // total entries
    record.extend_from_slice(&size.to_le_bytes());
    record.extend_from_slice(&offset.to_le_bytes());
    record.extend_from_slice(&u16::try_from(comment.len()).unwrap().to_le_bytes());
    record.extend_from_slice(comment);
    record
}

/// A stand-in for the local-file-header area, so the directory offset under
/// test is never the degenerate zero.
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

/// Assemble prefix + headers + an EOCD whose extent and count match them —
/// the shape every refusal test then perturbs in exactly one place.
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

/// The happy path: a directory `ditto -c -k` could plausibly have written is
/// read exactly — entry count and summed uncompressed bytes both.
#[test]
fn zip_claim_reads_a_well_formed_central_directory() {
    let zip = TempZip::new(
        "ok",
        &well_formed_zip(&[
            ("aterm.app/", 0),
            ("aterm.app/Contents/Info.plist", 1_024),
            ("aterm.app/Contents/MacOS/aterm", 40_000_000),
        ]),
    );
    let claim = checked_zip_extraction_claim(&zip.0).expect("well-formed directory is read");
    assert_eq!(
        claim,
        ZipExtractClaim {
            entries: 3,
            uncompressed_bytes: 40_001_024,
        }
    );
}

/// An archive comment does not hide the end record: the scan must mind the
/// variable-length tail rather than assume the record sits at EOF - 22.
#[test]
fn zip_claim_finds_the_end_record_behind_an_archive_comment() {
    let mut bytes = ZIP_PREFIX.to_vec();
    let offset = u32::try_from(bytes.len()).unwrap();
    bytes.extend_from_slice(&central_header("only", 7, &[]));
    let size = u32::try_from(bytes.len()).unwrap() - offset;
    // A 4 KiB comment of bytes that are not a signature, so the only valid
    // candidate is the real record.
    let comment = [b'c'; 4096];
    bytes.extend_from_slice(&eocd(1, size, offset, &comment));
    let zip = TempZip::new("comment", &bytes);
    assert_eq!(
        checked_zip_extraction_claim(&zip.0).expect("comment does not hide the end record"),
        ZipExtractClaim {
            entries: 1,
            uncompressed_bytes: 7,
        }
    );
}

/// A container with the toolchain seed sealed into the signed `.app` declares far
/// more than 1 GiB while staying inside the one bound the cutter validates every
/// published asset against. The client must be able to unpack anything the
/// publisher may publish.
#[test]
fn zip_claim_admits_a_batteries_sized_container_the_cutter_would_publish() {
    assert_eq!(
        MAX_EXTRACT_BYTES,
        aterm_update_core::RELEASE_ASSET_DOWNLOAD_BOUND,
        "the extract cap must never sit below the bound the cutter enforces"
    );
    let declared: u32 = 1_610_612_736; // 1.5 GiB
    let zip = TempZip::new(
        "batteries",
        &well_formed_zip(&[(
            "aterm.app/Contents/Resources/toolchain-seed/trust.tar.zst",
            declared,
        )]),
    );
    assert_eq!(
        checked_zip_extraction_claim(&zip.0).expect("a publishable container is accepted"),
        ZipExtractClaim {
            entries: 1,
            uncompressed_bytes: u64::from(declared),
        }
    );
}

/// An absurd declared size is refused. `u32::MAX - 1` is the largest size a
/// classic entry can state without the ZIP64 escape and already exceeds the cap,
/// so it is refused on bytes, with nothing written.
#[test]
fn zip_claim_refuses_an_absurd_declared_size() {
    let zip = TempZip::new(
        "huge",
        &well_formed_zip(&[("aterm.app/huge", u32::MAX - 1)]),
    );
    let error = checked_zip_extraction_claim(&zip.0).expect_err("absurd size is refused");
    assert!(
        error.contains("uncompressed bytes") && error.contains("extraction cap"),
        "refusal names the byte cap: {error}"
    );
}

/// This machine's free space is checked before `ditto` is spawned: a declared
/// size the volume cannot hold is refused with free, needed and the volume
/// named; a size it holds passes; a volume that will not report passes too
/// (`ditto` decides, with its stderr kept).
#[test]
fn the_stage_refuses_an_unpack_the_volume_cannot_hold_and_names_the_numbers() {
    let (_s, root) = temp_staging();
    let free = free_bytes_at(&root).expect("the scratch volume reports its free space");
    // More than the volume has, by a margin no headroom explains.
    let error = refuse_without_room(&root, free.saturating_add(1 << 40), "the update zip")
        .expect_err("a declared size past the volume is refused before any byte lands");
    assert!(
        error.contains("not enough free space")
            && error.contains("MiB free")
            && error.contains("MiB needed")
            && error.contains(&root.display().to_string()),
        "the refusal names the numbers and the volume: {error}"
    );
    // A modest size the volume holds is let through.
    refuse_without_room(&root, 1024, "the update zip").expect("a small unpack fits");
    // A path no volume answers for is let through, not refused: ditto decides.
    refuse_without_room(
        Path::new("/nonexistent/volume/for/this/test"),
        u64::MAX / 2,
        "the update zip",
    )
    .expect("an unanswerable volume defers to ditto");
    assert!(free_bytes_at(Path::new("/nonexistent/volume/for/this/test")).is_none());
}

/// `tree_bytes` is what the DMG lane's copy will write: regular files summed,
/// directories and symlinks counted as nothing and not followed.
#[test]
fn tree_bytes_sums_regular_files_and_never_follows_links() {
    let (_s, root) = temp_staging();
    let tree = root.join("tree.app");
    std::fs::create_dir_all(tree.join("Contents/MacOS")).unwrap();
    std::fs::write(tree.join("Contents/MacOS/aterm"), vec![7u8; 1000]).unwrap();
    std::fs::write(tree.join("Contents/Info.plist"), vec![1u8; 24]).unwrap();
    // A link to a big file outside the tree must not be counted through.
    let big = root.join("big.bin");
    std::fs::write(&big, vec![0u8; 100_000]).unwrap();
    std::os::unix::fs::symlink(&big, tree.join("Contents/MacOS/link")).unwrap();
    assert_eq!(tree_bytes(&tree), 1024);
    assert_eq!(tree_bytes(&root.join("absent")), 0);
}

/// Many small entries are refused on COUNT even though their declared bytes
/// are zero — the inode/block cost bytes alone cannot see.
#[test]
fn zip_claim_refuses_an_absurd_entry_count() {
    let over = usize::try_from(MAX_EXTRACT_ENTRIES).unwrap() + 1;
    let mut bytes = ZIP_PREFIX.to_vec();
    let offset = u32::try_from(bytes.len()).unwrap();
    for _ in 0..over {
        bytes.extend_from_slice(&central_header("e", 0, &[]));
    }
    let size = u32::try_from(bytes.len()).unwrap() - offset;
    bytes.extend_from_slice(&eocd(u16::try_from(over).unwrap(), size, offset, b""));
    let zip = TempZip::new("many", &bytes);
    let error = checked_zip_extraction_claim(&zip.0).expect_err("absurd count is refused");
    assert!(
        error.contains("entries"),
        "refusal names the entry cap: {error}"
    );
}

/// A directory cut mid-header is refused. The end record still declares the
/// full extent, so the walk runs out of header before it runs out of extent
/// — the shape a truncated upload produces.
#[test]
fn zip_claim_refuses_a_truncated_central_directory() {
    let mut bytes = ZIP_PREFIX.to_vec();
    let offset = u32::try_from(bytes.len()).unwrap();
    bytes.extend_from_slice(&central_header("first", 10, &[]));
    let full = central_header("second", 20, &[]);
    let present = u32::try_from(bytes.len()).unwrap() - offset;
    // Half of the second header survives; the extent still claims both.
    let size = present + u32::try_from(full.len()).unwrap();
    bytes.extend_from_slice(&full[..full.len() / 2]);
    bytes.extend_from_slice(&eocd(2, size, offset, b""));
    let zip = TempZip::new("cut", &bytes);
    let error = checked_zip_extraction_claim(&zip.0).expect_err("truncation is refused");
    assert!(
        error.contains("does not lie before its end record")
            || error.contains("ends inside a file header"),
        "refusal names the truncation: {error}"
    );
}

/// A directory whose walked entry count disagrees with the trailer is refused:
/// both records must describe the same archive the bound is measured on.
#[test]
fn zip_claim_refuses_a_directory_that_contradicts_its_trailer() {
    let mut bytes = ZIP_PREFIX.to_vec();
    let offset = u32::try_from(bytes.len()).unwrap();
    bytes.extend_from_slice(&central_header("one", 1, &[]));
    bytes.extend_from_slice(&central_header("two", 2, &[]));
    let size = u32::try_from(bytes.len()).unwrap() - offset;
    // Two headers present, seven declared.
    bytes.extend_from_slice(&eocd(7, size, offset, b""));
    let zip = TempZip::new("miscount", &bytes);
    let error = checked_zip_extraction_claim(&zip.0).expect_err("miscount is refused");
    assert!(
        error.contains("end record declares"),
        "refusal names the disagreement: {error}"
    );
}

/// A ZIP64 sentinel with no locator behind it is a shape this reader cannot
/// describe, and the saturated field is the one the cap is measured against, so
/// it is a refusal.
#[test]
fn zip_claim_refuses_a_zip64_sentinel_without_a_locator() {
    for (label, entries, size, offset) in [
        ("count", u16::MAX, 46u32, 12u32),
        ("size", 1u16, u32::MAX, 12u32),
        ("offset", 1u16, 46u32, u32::MAX),
    ] {
        let mut bytes = ZIP_PREFIX.to_vec();
        bytes.extend_from_slice(&central_header("one", 1, &[]));
        bytes.extend_from_slice(&eocd(entries, size, offset, b""));
        let zip = TempZip::new(label, &bytes);
        let error = checked_zip_extraction_claim(&zip.0)
            .expect_err("a sentinel with no ZIP64 record is refused");
        assert!(
            error.contains("carries no ZIP64 one"),
            "{label}: refusal names the missing ZIP64 record: {error}"
        );
    }
}

/// An entry escaping to a ZIP64 size with no ZIP64 extra field is refused rather
/// than read as a literal ~4 GiB (which would also fail, for the wrong reason:
/// the escape is unresolved).
#[test]
fn zip_claim_refuses_a_zip64_escape_with_no_extra_field() {
    let zip = TempZip::new("escape", &well_formed_zip(&[("aterm.app/x", u32::MAX)]));
    let error = checked_zip_extraction_claim(&zip.0).expect_err("bare escape is refused");
    assert!(
        error.contains("carries no ZIP64 extra field"),
        "refusal names the missing extra field: {error}"
    );
}

/// Two trailers that both run exactly to EOF are ambiguous: the bound would
/// be measured off a record that may not be the one `ditto` follows.
#[test]
fn zip_claim_refuses_an_ambiguous_pair_of_end_records() {
    let mut bytes = ZIP_PREFIX.to_vec();
    let offset = u32::try_from(bytes.len()).unwrap();
    bytes.extend_from_slice(&central_header("one", 1, &[]));
    let size = u32::try_from(bytes.len()).unwrap() - offset;
    // The decoy's comment swallows the real record, so both end exactly at EOF
    // and both parse.
    let real = eocd(1, size, offset, b"");
    let filler = vec![0u8; real.len()];
    bytes.extend_from_slice(&eocd(1, size, offset, &filler));
    let decoy_comment_start = bytes.len() - real.len();
    bytes[decoy_comment_start..].copy_from_slice(&real);
    let zip = TempZip::new("ambiguous", &bytes);
    let error = checked_zip_extraction_claim(&zip.0).expect_err("ambiguity is refused");
    assert!(
        error.contains("more than one end-of-central-directory record"),
        "refusal names the ambiguity: {error}"
    );
}

/// A file too short to hold an end record at all is refused, not read as an
/// empty archive.
#[test]
fn zip_claim_refuses_a_file_with_no_end_record() {
    let zip = TempZip::new("stub", b"PK\x03\x04not-a-zip");
    let error = checked_zip_extraction_claim(&zip.0).expect_err("a non-archive is refused");
    assert!(
        error.contains("end-of-central-directory"),
        "refusal names the missing end record: {error}"
    );
}

// -----------------------------------------------------------------------
// Stage-verify tests.
// -----------------------------------------------------------------------

/// A hand-installed, ad-hoc-signed bundle in `/Applications` can never be
/// updated by any lane (the seamless apply and the cold-launch fallback's step 7
/// both refuse it as the rollback source). The refusal names the cause and the
/// path, and the way out: reinstall the signed release (`tools/install.sh`, or
/// the release DMG), or mark a local build `ATermDevBuild`.
#[test]
fn a_rollback_source_refusal_names_the_remedy() {
    let root = std::env::temp_dir().join(format!(
        "aterm-rollback-remedy-{}-{}",
        std::process::id(),
        super::unix_now_secs()
    ));
    let error = super::preverify_installed_rollback_source(Some(&root), 100, Some("abcdef0"))
        .expect_err("a path that is not a bundle cannot be a rollback source");
    assert!(
        error.contains("tools/install.sh"),
        "the refusal must say how to put a signed release back: {error}"
    );
    assert!(
        error.contains("ATermDevBuild"),
        "…and how to take a local build out of the channel on purpose: {error}"
    );
}

/// Both lanes sweep both kinds of scratch: a `mnt-<pid>` a DMG stage left behind
/// (possibly with its image still attached) is reclaimed by the zip lane too, and
/// the sweep runs before the unpack, so a zip `ditto` cannot open still reclaims
/// the leftover.
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
