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

/// THE CHECK IS THE WINDOW'S OWN PASS (Phase 3): the worker spawns the Check with the
/// lanes' argv (`pass_args(PassVerb::Update, …)`: `--wait-lock` with their bound and
/// `--progress-file`), the spawner's pid and the login shell's PATH — the orphan watch
/// and the progress file are armed by exactly those — and answers at once only when a
/// person's typed verb holds the store. Pinned by scrape: the spawn is inside the
/// worker thread, which a unit test cannot reach without a real child.
#[test]
fn the_settings_check_runs_the_windows_pass_argv() {
    let src = include_str!("app_native.rs");
    let start = src
        .find("pub(crate) fn execute_native_packages(")
        .expect("the worker");
    let body = &src[start..start + src[start..].find("\n    }\n").unwrap()];
    let gate = body.find("if check {").expect("the Check's own spawn");
    let spawn = &body[gate..gate + 600];
    // Match the exact call after removing formatting whitespace: rustfmt
    // wraps this argument list across lines.
    let compact: String = spawn.chars().filter(|ch| !ch.is_whitespace()).collect();
    assert!(
        compact.contains("crate::pass_args(crate::PassVerb::Update,layout.as_ref(),None,)"),
        "Check uses the package lane's update argv: {spawn}"
    );
    for needle in [
        "atpkg::cli::SPAWNER_PID_ENV",
        "crate::spawn::atpkg_child_path()",
    ] {
        assert!(spawn.contains(needle), "{needle}: {spawn}");
    }
    let fail_fast = body
        .find("atpkg::lock::person_holder")
        .expect("the fail-fast rule");
    assert!(fail_fast < gate, "decided before anything is spawned");
    // The Install is the same shape (ruling 225): the lanes' lock wait and
    // progress file, the spawner's pid and PATH.
    let install = body
        .find("} else if install {")
        .expect("the Install's spawn");
    let install_spawn: String = body[install..install + 700]
        .chars()
        .filter(|ch| !ch.is_whitespace())
        .collect();
    assert!(
        install_spawn
            .contains(".args(verb).args(crate::pass_wait_and_progress_args(layout.as_ref()))"),
        "{install_spawn}"
    );
    assert!(install_spawn.contains("atpkg::cli::SPAWNER_PID_ENV"));
    // …and RUN the lanes' way: its stdout streamed, not collected at exit,
    // as a pass a PERSON waits on (ruling 224).
    let compact_body: String = body.chars().filter(|ch| !ch.is_whitespace()).collect();
    assert!(
        compact_body.contains("ifcheck||install{"),
        "both verbs stream"
    );
    let runs = body[gate..]
        .find("crate::run_pass_child(")
        .map(|i| gate + i)
        .expect("the Check runs through the lanes' child runner");
    let call: String = body[runs..runs + 400]
        .chars()
        .filter(|ch| !ch.is_whitespace())
        .collect();
    assert!(
        call.starts_with(
            "crate::run_pass_child(&mutchild,layout.as_ref(),false,aterm_messages::Waiter::Person,&post,)"
        ),
        "{call}"
    );
    let collected = body
        .find("let result = child.output();")
        .expect("the other verbs");
    assert!(
        runs < collected,
        "the Check leaves the loop before the collect-at-exit"
    );
    // …and a PERSON's: no `--defer-busy-flip` (2026-09-26), the one flag the loop's
    // own spawn adds (`lane_pass_args`) — a Check clicked in Settings flips a new
    // Trust toolchain at once, however busy the old one is.
    let args = crate::pass_args(crate::PassVerb::Update, None, None);
    assert_eq!(
        args,
        vec![
            std::ffi::OsString::from("update"),
            "--wait-lock".into(),
            crate::ATPKG_WAIT_LOCK_SECS.to_string().into()
        ],
        "the lanes' own bound, and never the unattended flip hold"
    );
    assert!(check_held_by_person_message(4242).contains("pid 4242"));
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

/// The worker's exit classification: success is no sentence; `--default-set`
/// exit 2 keeps its "nothing was installed" words; the contention code (75)
/// names the other installer by code; any other exit is the last stderr line,
/// or the status when there is none.
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
    assert!(
        packages_child_failure(Ok(status(2)), &s(&["install", "--default-set"]), "")
            .unwrap()
            .starts_with("Nothing was installed"),
    );
    let contended = packages_child_failure(
        Ok(status(i32::from(atpkg::lock::CONTENDED_EXIT))),
        &s(&["install", "--default-set"]),
        lock_line,
    )
    .unwrap();
    assert!(
        contended.starts_with("A toolchain pass is already running on this store"),
        "{contended}"
    );
    // Never a claim about another aterm: after a self-update the holder is
    // this app's predecessor's pass.
    assert!(
        !contended.to_lowercase().contains("another aterm"),
        "{contended}"
    );
    assert!(!contended.to_lowercase().contains("lock"), "{contended}");
    assert!(contended.ends_with("try again in a moment."), "{contended}");
    // The child's own line is never quoted: it names a peer and a lock.
    assert!(!contended.contains("holds"), "{contended}");
    // With nothing on stderr the sentence is the same.
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
    assert_eq!(
        packages_child_failure(Ok(status(1)), &s(&["update"]), "").as_deref(),
        Some("atpkg update exited with exit status: 1")
    );
    let error = std::io::Error::other("no such file");
    assert_eq!(
        packages_child_failure(Err(&error), &s(&["update"]), "").as_deref(),
        Some("could not launch atpkg update: no such file")
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

#[test]
fn every_request_reserves_its_own_busy_label() {
    assert_eq!(
        packages_busy(&PackagesRequest::CheckUpdate),
        PackagesBusy::Check
    );
    assert_eq!(
        packages_busy(&PackagesRequest::InstallDefaultSet),
        PackagesBusy::Install
    );
    assert_eq!(
        packages_busy(&PackagesRequest::UninstallAll),
        PackagesBusy::Uninstall
    );
    assert_eq!(
        packages_busy(&PackagesRequest::MachineApply),
        PackagesBusy::MachineApply
    );
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

/// Under a private state root Apply Now is refused by name, and nothing else
/// is; unset, every request passes. The Check and the Install then lead with
/// the private window's flag, from the same live answer.
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
    let refused = packages_request_refusal(&PackagesRequest::MachineApply, true)
        .expect("Apply now under a private state root");
    assert!(
        refused.contains("private state root (ATERM_STATE_HOME)")
            && refused.contains("`aterm pkg machine apply` in a terminal"),
        "it names why and the way left: {refused}"
    );
    // The host asks it with the live answer, as a refusal (not a failure), before the
    // busy gate and the worker thread, so no child is spawned.
    let src = include_str!("app_native.rs");
    let start = src
        .find("pub(crate) fn execute_native_packages(")
        .expect("the host executor");
    let body = &src[start..];
    let body = &body[..body.find("\n    }\n").expect("its end")];
    // Whitespace out, so the formatter's line breaks cannot move the needles.
    let flat: String = body.chars().filter(|c| !c.is_whitespace()).collect();
    let live = flat
        .find("letprivate_state_root=aterm_types::dirs::runs_under_private_state_root();")
        .expect("the executor reads the live answer once");
    let ask = flat
        .find("packages_request_refusal(&request,private_state_root)")
        .expect("the executor asks with it");
    assert!(live < ask, "{body}");
    let blocked = flat[ask..]
        .find("returnPackagesOutcome::Blocked{message};")
        .map(|at| ask + at)
        .expect("a refusal is Blocked");
    let busy = flat
        .find("self.native_packages_service.busy()")
        .expect("the busy gate");
    let worker = flat.find(".spawn(move||").expect("the worker");
    assert!(ask < blocked && blocked < busy && busy < worker, "{body}");
    // Both passes lead with it, ahead of their verb: the `atpkg` beside a dev
    // build may lack the seam, and one built before the flag refuses it as an
    // unknown verb instead of applying.
    for (branch, verb) in [
        ("ifcheck{", ".args(crate::pass_args("),
        ("}elseifinstall{", ".args(verb)"),
    ] {
        let at = worker + flat[worker..].find(branch).expect(branch);
        let lead = at
            + flat[at..]
                .find("child.args(crate::pass_lead(private_state_root))")
                .expect("the lead");
        let args = at + flat[at..].find(verb).expect(verb);
        assert!(lead < args, "{branch}: the lead comes first");
        assert_eq!(
            args,
            lead + "child.args(crate::pass_lead(private_state_root))".len(),
            "{branch}: nothing between the lead and the verb"
        );
    }
}
