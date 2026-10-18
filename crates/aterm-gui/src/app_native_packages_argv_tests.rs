// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

use super::*;

/// The argv table, verbatim.
#[test]
fn packages_argv_is_pinned_per_request() {
    let s = |v: &[&str]| v.iter().map(ToString::to_string).collect::<Vec<String>>();
    assert_eq!(packages_argv(&PackagesRequest::CheckUpdate), s(&["update"]));
    assert_eq!(
        packages_argv(&PackagesRequest::InstallDefaultSet),
        s(&["install", "--default-set"])
    );
    assert_eq!(
        packages_argv(&PackagesRequest::UninstallAll),
        s(&["uninstall", "--all"])
    );
    assert_eq!(
        packages_argv(&PackagesRequest::MachineApply),
        s(&["machine", "apply"]),
        "no --wait-lock: the verb takes no store lock"
    );
}

/// The offline code reads as what it is, never as a bare exit status.
#[cfg(unix)]
#[test]
fn an_offline_check_says_offline() {
    use std::os::unix::process::ExitStatusExt as _;
    let code = i32::from(aterm_update_core::pkg_check::PASS_OFFLINE_EXIT);
    let status = std::process::ExitStatus::from_raw(code << 8);
    let said =
        packages_child_failure(Ok(status), &["update".to_string()], "").expect("not a success");
    assert!(said.contains("offline"), "{said}");
}

/// The machine apply worker sorts the child's stdout: the `machine-settings:`
/// row becomes the pull-down/card event, the last `atpkg machine: …` line is
/// the verdict, and a `machine settings not applied — …` line is the refusal
/// reported as the failure (by its reason).
#[test]
fn machine_apply_stdout_is_sorted_into_events_verdict_and_refusal() {
    use atpkg::cli::{MACHINE_NOT_APPLIED_PREFIX, MACHINE_SETTINGS_MARKER, MACHINE_VERDICT_PREFIX};
    // A verdict line after atpkg's own prefix; the worker passes its words on as-is.
    let nothing_changed = format!("{MACHINE_VERDICT_PREFIX}nothing changed — already applied");
    // The apply prints its own `machine-state:` record after the change line;
    // the worker hands it on as the card's newest state, so the completion
    // has nothing to confirm by a read.
    let applied = machine_apply_stdout(&format!(
        "atpkg: Universal Control disabled for this host (revert: …)\n\
         atpkg: {MACHINE_SETTINGS_MARKER}universal-control disabled\n\
         atpkg: {}universal-control=disabled; policy=off; home=account\n\
         {MACHINE_VERDICT_PREFIX}applied — universal-control disabled\n",
        atpkg::cli::MACHINE_STATE_MARKER
    ));
    assert_eq!(applied.events.len(), 1, "{:?}", applied.events);
    assert!(matches!(
        &applied.events[0],
        Wake::PkgMachineSettings(body) if body == "universal-control disabled"
    ));
    assert!(
        applied
            .record
            .as_ref()
            .is_some_and(|s| s.universal_control == atpkg::machine::UcPosture::Disabled),
        "the record rides the completion, not the event stream"
    );
    assert_eq!(
        applied.verdict.as_deref(),
        Some("applied — universal-control disabled")
    );
    assert!(applied.refusal.is_none());

    let nothing = machine_apply_stdout(&format!("{nothing_changed}\n"));
    assert!(nothing.events.is_empty());
    assert_eq!(
        nothing.verdict.as_deref(),
        Some("nothing changed — already applied")
    );
    assert!(nothing.refusal.is_none());

    // A refused apply: the refusal line, then the verb's own `not applied` verdict
    // (never a "nothing changed" beside a refusal).
    let refused = machine_apply_stdout(&format!(
        "{MACHINE_NOT_APPLIED_PREFIX}HOME is /tmp/synthetic, the account home is /Users//x\n\
         {MACHINE_VERDICT_PREFIX}not applied — HOME is /tmp/synthetic, the account home is /Users//x\n"
    ));
    assert_eq!(
        refused.refusal.as_deref(),
        Some("HOME is /tmp/synthetic, the account home is /Users//x")
    );
    assert_eq!(
        refused.verdict.as_deref(),
        Some("not applied — HOME is /tmp/synthetic, the account home is /Users//x")
    );
    assert!(refused.events.is_empty());

    // Nothing said: no verdict, no refusal, no events — the worker then reports
    // the exit status alone.
    let silent = machine_apply_stdout("");
    assert!(silent.verdict.is_none() && silent.refusal.is_none() && silent.events.is_empty());
}

/// The machine read parses the `machine-state:` record out of the prose
/// `atpkg machine` prints around it; prose alone or a malformed record is no
/// record, reported as "printed no state".
#[test]
fn machine_state_is_read_from_the_marker_line_and_never_guessed() {
    use atpkg::machine::{HomePosture, UcPosture};
    let stdout = "atpkg machine: warn — Universal Control is at the OS default (…)\n\
                  atpkg: machine-state: universal-control=default; policy=off; home=account\n\
                  atpkg machine: next — aterm pkg machine apply (Universal Control off for this host)\n";
    let state = parse_machine_state_output(stdout).expect("the marker line parses");
    assert_eq!(state.universal_control, UcPosture::Default);
    assert_eq!(state.policy, atpkg::config::UniversalControlPolicy::Off);
    assert_eq!(state.home, HomePosture::Account);
    assert!(state.next());
    assert!(
        parse_machine_state_output(
            "atpkg machine: ok — Universal Control is disabled on this host\n\
             atpkg machine: nothing to apply — Universal Control is where [machine] wants it\n"
        )
        .is_none(),
        "prose is not a record"
    );
    assert!(
        parse_machine_state_output("atpkg: machine-state: universal-control=default; policy=off\n")
            .is_none(),
        "a record missing fields is no record"
    );
    assert!(
        parse_machine_state_output(
            "atpkg machine: not applicable — these are macOS host settings\n"
        )
        .is_none()
    );
}

/// The worker's exit classification: success is no sentence; the contention
/// code (75) never quotes the child's own line (it names a peer and a lock)
/// and reads the same with or without stderr; any other exit is the last
/// stderr line.
#[cfg(unix)]
#[test]
fn a_contended_verb_names_the_other_installer_by_exit_code() {
    use std::os::unix::process::ExitStatusExt as _;
    let status = |code: i32| std::process::ExitStatus::from_raw(code << 8);
    let s = |v: &[&str]| v.iter().map(ToString::to_string).collect::<Vec<String>>();
    let lock_line = "atpkg: another atpkg process holds the store lock at /p/store.lock \
                     \u{2014} refusing to mutate the store concurrently (retry when it exits)";
    assert_eq!(
        packages_child_failure(Ok(status(0)), &s(&["update"]), ""),
        None
    );
    let contended = packages_child_failure(
        Ok(status(i32::from(atpkg::lock::CONTENDED_EXIT))),
        &s(&["install", "--default-set"]),
        lock_line,
    )
    .unwrap();
    assert!(!contended.contains("holds"), "{contended}");
    let quiet = packages_child_failure(
        Ok(status(i32::from(atpkg::lock::CONTENDED_EXIT))),
        &s(&["update"]),
        "",
    )
    .unwrap();
    assert_eq!(quiet, contended);
    assert_eq!(
        packages_child_failure(Ok(status(1)), &s(&["update"]), "first\nlast line").as_deref(),
        Some("last line")
    );
}

/// A Settings Check that stood down behind a sibling's cached-index pass
/// names the cause. atpkg writes that stand-down line to stdout, and the
/// Check maps failures through [`packages_child_failure`] over stderr alone.
/// The child is built from atpkg's real stand-down decision
/// (`atpkg::cli::stand_down_exit` over `atpkg::status::stale_index_only_end`)
/// and fed to [`check_pass_failure`], binding the two halves. Also covers the
/// Check's own pass on the cached index. Controls: an unmoved record, no
/// store, a holder that also failed a member (stderr quoted), and a success.
#[cfg(unix)]
#[test]
fn a_check_behind_a_cached_index_pass_names_the_cause_regression_2026_09_26() {
    use aterm_update_core::pkg_check::PassOutcome;
    use std::os::unix::process::ExitStatusExt as _;
    let dir = std::env::temp_dir().join(format!(
        "aterm-gui-check-cached-index-{}",
        std::process::id()
    ));
    let _ = std::fs::remove_dir_all(&dir);
    let layout = atpkg::store::Layout {
        prefix: dir.join("pkg"),
    };
    std::fs::create_dir_all(&layout.prefix).unwrap();
    let cached = "up to date (index build 46) (index from cache \u{2014} GitHub rate limit \
                  hit (HTTP 403))";
    let cause = "GitHub rate limit hit (HTTP 403)";
    // The sibling's pass end as atpkg records it: seq 8, failed, the cache clause, and
    // whether it failed for its index alone.
    let sibling_ended = |stale_only: bool| {
        let status = atpkg::status::Status {
            schema: 1,
            outcome: cached.to_string(),
            last_pass: PassOutcome::Failed.word().to_string(),
            last_pass_at: "2026-09-26T10:00:00Z".to_string(),
            pass_seq: 8,
            stale_index_only_pass_seq: if stale_only { 8 } else { 0 },
            ..atpkg::status::Status::default()
        };
        atpkg::status::write(&layout, &status).unwrap();
        status
    };
    let child = |code: i32, said: String, pass_seq_before: Option<u64>, waited: bool| {
        let run = crate::PassRun {
            seen: crate::SeedMarkers {
                saw_lock_wait: waited,
                ..crate::SeedMarkers::default()
            },
            code: Some(code),
            holder_advanced: false,
            said,
            pass_seq_before,
        };
        (run, std::process::ExitStatus::from_raw(code << 8))
    };
    // The Check's child: read seq 7 before the spawn, queued, stood down — its streams
    // and exit exactly as atpkg's stand-down decides them.
    let stood_down = |record: &atpkg::status::Status| {
        let exit = atpkg::cli::stand_down_exit(
            PassOutcome::Failed,
            3,
            &record.outcome,
            atpkg::status::stale_index_only_end(record),
        );
        let said = if exit.on_stderr {
            format!("{}\n", exit.line)
        } else {
            String::new()
        };
        child(i32::from(exit.code), said, Some(7), true)
    };
    let verb = vec!["update".to_string()];
    let check = |(run, status): &(crate::PassRun, std::process::ExitStatus), layout: Option<&_>| {
        check_pass_failure(run, Ok(*status), &verb, layout)
    };
    let bare = Some("atpkg update exited with exit status: 1".to_string());

    // THE GAP: stood down behind a stale-index-only pass — exit 1, stderr silent.
    let stale = stood_down(&sibling_ended(true));
    assert!(
        stale.0.said.is_empty(),
        "the stand-down said nothing on stderr"
    );
    let said = check(&stale, Some(&layout)).expect("the Check still reports a failure");
    assert!(said.contains(cause), "the recorded cause is named: {said}");
    assert!(!said.contains("exit status"), "never the bare exit: {said}");
    assert_eq!(said, cached_index_check_message(cause));

    // The Check's OWN pass on the cached index: the same record, no queue.
    let own = child(1, String::new(), Some(7), false);
    assert_eq!(
        check(&own, Some(&layout)),
        Some(cached_index_check_message(cause))
    );

    // Control: the record did not move past the one read before the spawn (not this
    // child's pass; an edge refusal writes none): the bare exit.
    let unmoved = child(1, String::new(), Some(8), false);
    assert_eq!(check(&unmoved, Some(&layout)), bare);
    // Control: no store to read.
    assert_eq!(check(&stale, None), bare);
    // Control: the sibling also failed a member; the stand-down speaks on stderr and
    // that line is the Check's sentence.
    let failed = stood_down(&sibling_ended(false));
    let said = check(&failed, Some(&layout)).expect("a failure");
    assert!(said.contains("the last update failed"), "{said}");
    assert_eq!(said, failed.0.said.trim());
    // Control: success is no sentence, whatever the record says.
    let ok = child(0, String::new(), Some(7), false);
    assert_eq!(check(&ok, Some(&layout)), None);
    let _ = std::fs::remove_dir_all(&dir);
}

/// The host re-reads the machine record after exactly the verbs whose atpkg
/// pass runs `apply_machine_settings` first, derived from the pinned argv
/// table to mirror atpkg's `verb_applies_machine_settings` (`update`, every
/// `install`, `machine apply`; not `uninstall --all`).
#[test]
fn a_pass_that_applies_the_machine_settings_rereads_the_record() {
    let requests = [
        PackagesRequest::CheckUpdate,
        PackagesRequest::InstallDefaultSet,
        PackagesRequest::UninstallAll,
        PackagesRequest::MachineApply,
    ];
    let mut rereads = 0;
    for request in requests {
        let argv = packages_argv(&request);
        let verb: Vec<&str> = argv.iter().map(String::as_str).collect();
        let atpkg_applies = matches!(
            verb.as_slice(),
            ["update"] | ["install", ..] | ["machine", "apply"]
        );
        assert_eq!(
            packages_busy(&request).applies_machine_settings(),
            atpkg_applies,
            "{request:?} → {argv:?}"
        );
        rereads += usize::from(atpkg_applies);
    }
    assert_eq!(rereads, 3, "update, install --default-set, machine apply");
}

/// The structural admission: every verb but the `[machine]` host settings is
/// admissible anywhere.
#[test]
fn packages_requests_are_admitted_by_platform() {
    for request in [
        PackagesRequest::CheckUpdate,
        PackagesRequest::InstallDefaultSet,
        PackagesRequest::UninstallAll,
    ] {
        assert!(packages_request_admissible(&request).is_ok(), "{request:?}");
    }
    // The [machine] host settings are macOS host settings: admissible there,
    // refused by name everywhere else — before any child is spawned.
    let machine = packages_request_admissible(&PackagesRequest::MachineApply);
    if cfg!(target_os = "macos") {
        assert!(machine.is_ok());
    } else {
        assert!(
            machine
                .as_ref()
                .is_err_and(|m| m.contains("macOS host settings")),
            "{machine:?}"
        );
    }
}

/// Under a private state root Apply Now is refused, and nothing else is;
/// unset, every request passes.
#[test]
fn apply_now_is_refused_under_a_private_state_root_and_nothing_else_is() {
    let all = [
        PackagesRequest::CheckUpdate,
        PackagesRequest::InstallDefaultSet,
        PackagesRequest::UninstallAll,
        PackagesRequest::MachineApply,
    ];
    for request in &all {
        assert_eq!(
            packages_request_refusal(request, false),
            None,
            "{request:?}"
        );
    }
    for request in &all[..3] {
        assert_eq!(packages_request_refusal(request, true), None, "{request:?}");
    }
    assert!(
        packages_request_refusal(&PackagesRequest::MachineApply, true).is_some(),
        "Apply now under a private state root is refused"
    );
}
