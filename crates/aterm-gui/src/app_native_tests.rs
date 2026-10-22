// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

use super::*;
use crate::update_words::LiveHealth as _;

#[path = "app_native_settings_keep_tests.rs"]
mod settings_keep_tests;

#[path = "app_native_settings_press_tests.rs"]
mod settings_press_tests;

#[test]
fn messages_quiet_gap_folds_mixed_split_feedback_and_visibility() {
    use crate::native_app::AppViewState;
    use crate::native_settings::{SettingsRoute, SettingsViewState};
    use crate::tab_model::{SplitAxis, ViewId};

    fn settings(app: &mut App, view: ViewId) -> &mut SettingsViewState {
        match app.native_runtime.view_state_mut(view).unwrap() {
            AppViewState::Settings(state) => state,
            _ => panic!("expected Settings"),
        }
    }

    let mut app = App::headless_for_test();
    let wid = WindowId(0);
    let terminal = app.windows[&wid].tab_set.active().unwrap().focus;
    app.windows.get_mut(&wid).unwrap().band_on_screen_for_test = true;
    // A record stamped now: the page says `just now`, so its clock is owed
    // the minute.
    app.post_message(aterm_messages::Message::new(
        aterm_messages::tags::CONFIG,
        aterm_messages::Severity::Warn,
        "a record the page dates",
    ));
    assert!(app.open_settings_tab_in_window(wid, SettingsRoute::Messages));
    let (instance, first) = app.active_native_view(wid).unwrap();
    let sibling = app.view_store.insert_native(instance).unwrap();
    let mut state =
        SettingsViewState::from_snapshot(&app.native_config_service.snapshot()).unwrap();
    state.navigate(SettingsRoute::Messages);
    app.native_runtime
        .attach_view(sibling, instance, AppViewState::Settings(Box::new(state)))
        .unwrap();
    let tab = app
        .windows
        .get_mut(&wid)
        .unwrap()
        .tab_set
        .active_mut()
        .unwrap();
    assert!(tab.root.split_leaf(first, SplitAxis::Horizontal, terminal));
    assert!(tab.root.split_leaf(terminal, SplitAxis::Vertical, sibling));
    let settings_tab = tab.id;

    assert_eq!(
        app.settings_messages_quiet_gap(),
        Some(MESSAGES_PUBLISH_TICK)
    );
    settings(&mut app, sibling).show_transient_status_for_test("Copied");
    assert_eq!(
        app.settings_messages_quiet_gap(),
        Some(MESSAGES_PUBLISH_MIN_GAP),
        "feedback in the last leaf beats the first leaf's minute tick"
    );
    app.publish_native_messages_state();
    assert_eq!(
        app.settings_messages_quiet_gap(),
        Some(MESSAGES_FEEDBACK_WAIT)
    );
    settings(&mut app, first).navigate(SettingsRoute::Home);
    assert_eq!(
        app.settings_messages_quiet_gap(),
        Some(MESSAGES_FEEDBACK_WAIT)
    );

    assert!(
        app.windows
            .get_mut(&wid)
            .unwrap()
            .tab_set
            .switch_to_index(0)
    );
    assert_eq!(
        app.settings_messages_quiet_gap(),
        None,
        "background Settings tab"
    );
    assert!(
        app.windows
            .get_mut(&wid)
            .unwrap()
            .tab_set
            .switch_to(settings_tab)
    );
    assert_eq!(
        app.settings_messages_quiet_gap(),
        Some(MESSAGES_FEEDBACK_WAIT)
    );
    settings(&mut app, sibling).navigate(SettingsRoute::Home);
    assert_eq!(
        app.settings_messages_quiet_gap(),
        None,
        "both views off Messages"
    );
}

/// Whether `pid` is still this process's child, polled with `WNOWAIT` so the helper under test
/// keeps the reap, until it is not or `within` (a hang detector) runs out.
#[cfg(unix)]
fn child_still_ours(pid: libc::pid_t, within: std::time::Duration) -> bool {
    use std::time::{Duration, Instant};
    let deadline = Instant::now() + within;
    loop {
        // SAFETY: a zeroed `siginfo_t` is a valid out-parameter.
        let mut info: libc::siginfo_t = unsafe { std::mem::zeroed() };
        // SAFETY: `WNOWAIT` leaves the child waitable; `WNOHANG` never blocks.
        let ours = unsafe {
            libc::waitid(
                libc::P_PID,
                pid as libc::id_t,
                &mut info,
                libc::WEXITED | libc::WNOWAIT | libc::WNOHANG,
            )
        } == 0;
        if !ours || Instant::now() >= deadline {
            return ours;
        }
        std::thread::sleep(Duration::from_millis(20));
    }
}

/// A timed-out read reaps the child it owns. The child sleeps `JOB`, far past the budget, so a
/// read that waited for the child instead of its budget is caught by cause.
#[cfg(unix)]
#[test]
fn machine_command_timeout_reaps_owned_child() {
    use std::time::{Duration, Instant};
    const JOB: Duration = Duration::from_secs(120);
    let mut command = aterm_uds::spawnfd::command("/bin/sleep");
    command.arg(JOB.as_secs().to_string());
    LAST_MACHINE_CHILD.set(None);
    let began = Instant::now();
    let result = machine_command_output(
        &mut command,
        "owned machine read",
        Duration::from_millis(400),
    );
    let elapsed = began.elapsed();
    let pid = LAST_MACHINE_CHILD
        .get()
        .and_then(|pid| libc::pid_t::try_from(pid).ok())
        .expect("the read spawned its child");
    let error = result.unwrap_err();
    assert!(elapsed < JOB, "the read waited for its child: {elapsed:?}");
    assert!(error.contains("timed out"), "{error}");
    // Reaped by the helper or, when the kill's exit outlasts its 250 ms cleanup reserve, by
    // `aterm_uds::reaper`, which a dropped running `spawnfd::Child` goes to.
    assert!(
        !child_still_ours(pid, aterm_spec::wait::MUST_HAPPEN),
        "the timed-out child was left an unreaped zombie: {error}"
    );
}

/// An exited child is not stdout EOF, and a flood is capped; both bounded by cause. The grandchild
/// holding the inherited stdout sleeps `JOB`, far past the budget, so a read waiting for EOF
/// instead of the budget returns after `JOB`. The budget leaves the shell, which exits at once,
/// most of a second to do so before the read gives up. The shell runs in its own process group,
/// and the test kills that group as soon as the call returns, so the grandchild does not hold
/// this process's unflagged descriptors open for sibling tests.
#[cfg(unix)]
#[test]
fn machine_command_bounds_inherited_pipe_eof_and_output_flood() {
    use std::time::{Duration, Instant};
    const JOB: Duration = Duration::from_secs(60);
    let mut command = aterm_uds::spawnfd::command("/bin/sh");
    command
        .args(["-c", r#"/bin/sleep "$1" & exit 0"#, "--"])
        .arg(JOB.as_secs().to_string())
        .process_group(0);
    LAST_MACHINE_CHILD.set(None);
    let began = Instant::now();
    let result =
        machine_command_output(&mut command, "owned inherited pipe", Duration::from_secs(1));
    let elapsed = began.elapsed();
    let group = LAST_MACHINE_CHILD
        .get()
        .and_then(|pid| libc::pid_t::try_from(pid).ok())
        .filter(|&pgid| pgid > 1)
        .expect("the read spawned its shell");
    // SAFETY: `killpg` takes no pointers; `group` > 1 is the shell's own
    // group, which outlives the reaped shell while its sleeper lives.
    unsafe { libc::killpg(group, libc::SIGKILL) };
    assert!(
        result.as_ref().is_err_and(|e| e.contains("timed out")),
        "{result:?}"
    );
    assert!(
        elapsed < JOB,
        "an exited child is not stdout EOF: {elapsed:?}"
    );

    let mut command = aterm_uds::spawnfd::command("/usr/bin/yes");
    command.arg("owned output flood");
    let result = machine_command_output(&mut command, "owned flood", aterm_spec::wait::MUST_HAPPEN);
    assert!(
        result.as_ref().is_err_and(|e| e.contains("64 KiB")),
        "{result:?}"
    );
}

/// atpkg's `machine apply` stdout protocol: a failed-write marker settles the
/// apply as a failure even beside a verdict line, and the settings marker is
/// posted once. (A timed-out apply is `machine_command_timeout_reaps_owned_child`'s
/// error, settled through the same `Err` arm.)
#[cfg(unix)]
#[test]
fn machine_apply_failed_write_settles_as_failure() {
    use std::os::unix::process::ExitStatusExt as _;
    let stdout = format!(
        "atpkg: {}some-setting changed\n{}preference write did not land\n{}applied — some-setting changed\n",
        atpkg::cli::MACHINE_SETTINGS_MARKER,
        atpkg::cli::MACHINE_APPLY_FAILED_PREFIX,
        atpkg::cli::MACHINE_VERDICT_PREFIX
    );
    let output = |bytes: Vec<u8>| std::process::Output {
        status: std::process::ExitStatus::from_raw(0),
        stdout: bytes,
        stderr: Vec::new(),
    };
    let mut events = Vec::new();
    let (outcome, verdict, record) =
        machine_apply_completion(Ok(output(stdout.as_bytes().to_vec())), |event| {
            events.push(event)
        });
    assert!(record.is_none(), "no record was printed");
    assert!(
        matches!(&outcome, PackagesCommandOutcome::Failed { message, .. } if message == "preference write did not land")
    );
    // The change alone: no `machine-state:` record rode this stdout, and a
    // completion carrying none makes the host re-read; no record-missing
    // event is posted here, so that read is the only one.
    assert_eq!(events.len(), 1, "{events:?}");
    assert!(matches!(&events[0], Wake::PkgMachineSettings(body) if body == "some-setting changed"));
    assert_eq!(verdict.as_deref(), Some("applied — some-setting changed"));
    let historical = stdout
        .lines()
        .filter(|line| !line.starts_with(atpkg::cli::MACHINE_APPLY_FAILED_PREFIX))
        .collect::<Vec<_>>()
        .join("\n");
    let (old, _, _) = machine_apply_completion(Ok(output(historical.into_bytes())), |_| {});
    assert!(
        matches!(old, PackagesCommandOutcome::Succeeded { .. }),
        "ignoring the failed-write marker would falsely report success"
    );
}

fn machine_read_dispatch_model() -> aterm_spec::derive::Model {
    aterm_spec::ty_model! {
        NativeMachineReadDispatch {
            const Buggy = 0;
            var requests = 0;
            var refreshing = 0;
            var rerun = 0;
            var workers = 0;
            action Request when (requests <= 2) {
                requests = requests + 1;
                rerun = if refreshing == 1 { 1 } else { 0 };
                refreshing = 1;
                workers = 1;
            }
            action Complete when (workers == 1) {
                workers = if rerun == 1 && Buggy == 0 { 1 } else { 0 };
                refreshing = rerun;
                rerun = if rerun == 1 && Buggy == 1 { 1 } else { 0 };
            }
            action RerunSpawnFailure when (workers == 1 && rerun == 1) {
                workers = 0;
                refreshing = 0;
                rerun = 0;
            }
            action InitialSpawnFailure when (refreshing == 0 && requests <= 2) {
                requests = requests + 1;
                workers = 0;
                refreshing = 0;
                rerun = 0;
            }
            invariant Bounded: requests <= 3 && workers <= 1 && rerun <= 1;
            invariant ReservedReadHasWorker: refreshing == workers;
            invariant QueuedReadHasWorker: rerun <= workers;
        }
    }
}

#[test]
fn machine_read_dispatch_conforms_and_rejects_the_historical_orphan() {
    use crate::packages_screen::PackagesService;

    let model = machine_read_dispatch_model();
    aterm_spec::verify::prove_and_catch_scalar(&model, model.name);
    for actions in [
        &["Request", "Request", "Request", "Complete", "Complete"][..],
        &[
            "Request",
            "Request",
            "RerunSpawnFailure",
            "Request",
            "Complete",
        ],
        &["InitialSpawnFailure", "Request", "Complete"],
        &[
            "Request", "Complete", "Request", "Request", "Complete", "Complete",
        ],
    ] {
        let mut service = PackagesService::new();
        let mut expected = model.init_state();
        let mut requests = 0;
        let mut workers = 0;
        let mut launches = 0;
        for action in actions {
            let is_request = matches!(*action, "Request" | "InitialSpawnFailure");
            let spawn_fails = matches!(*action, "InitialSpawnFailure" | "RerunSpawnFailure");
            let event = if is_request {
                requests += 1;
                MachineReadEvent::Request
            } else {
                assert_eq!(workers, 1, "only a running worker can complete");
                workers -= 1;
                // A failed read is still a terminal completion. It must
                // release or restart the read exactly like a parsed record.
                MachineReadEvent::Finished(Err("owned read fixture completed".to_string()))
            };
            dispatch_native_machine_read(&mut service, event, || {
                launches += 1;
                assert_eq!(workers, 0, "dispatch must never overlap machine workers");
                if spawn_fails {
                    Err("owned spawn refusal".to_string())
                } else {
                    workers += 1;
                    Ok(())
                }
            });
            assert!(model.fire(action, &mut expected), "{action}");
            let mut actual = expected.clone();
            actual.insert("requests", requests);
            actual.insert("workers", workers);
            actual.insert("refreshing", i64::from(service.machine().refreshing));
            actual.insert("rerun", i64::from(service.machine().rerun));
            assert_eq!(actual, expected, "{actions:?}: {action}");
        }
        assert!(
            launches >= 2,
            "every script reaches a real replacement attempt"
        );
        assert_eq!(workers, 0);
        assert!(!service.machine().refreshing);
    }

    // Negative control: a completion that requests admission again for the reserved rerun leaves
    // the finished worker with no successor despite the refreshing flag.
    let mut historical = PackagesService::new();
    assert!(historical.request_machine_read());
    assert!(!historical.request_machine_read());
    let mut before = model.init_state();
    assert!(model.fire("Request", &mut before));
    assert!(model.fire("Request", &mut before));
    let expected = model.successors("Complete", &before);
    let reserved = historical.replace_machine_state(Err("old read completed".into()));
    assert!(reserved);
    let spawned = reserved && historical.request_machine_read();
    assert!(!spawned, "the historical dispatch loses the rerun");
    let mut broken = before;
    broken.insert("workers", i64::from(spawned));
    broken.insert("refreshing", i64::from(historical.machine().refreshing));
    broken.insert("rerun", i64::from(historical.machine().rerun));
    assert!(
        !expected.contains(&broken),
        "the conformance bind must reject a refreshing status with no worker"
    );
}

/// A control `settings set serious_mode` and the native toggle share one
/// semantic projection: a toggle queued behind the control write toggles the
/// queued `true` intent, not the live `false` policy, and each completion
/// moves the live policy in queue order. (The intent queue's model is bound
/// by `three_rapid_serious_mode_intents_rebase_through_each_completion`.)
#[test]
fn legacy_serious_mode_set_and_native_toggle_share_one_semantic_projection() {
    fn complete_next(app: &mut App) {
        let NativeConfigRequest { origin, work } = app.native_config_pending.pop_front().unwrap();
        let ConfigPatchResult::Applied { snapshot, undo } =
            app.reduce_native_config_work(work).unwrap()
        else {
            panic!("a serialized Serious Mode intent must apply");
        };
        let outcome = ConfigPatchOutcome::Applied {
            revision: snapshot.revision,
            undo: Some(undo.get()),
        };
        app.publish_native_config_origin(origin, outcome, Some(snapshot), false, None);
    }

    let mut app = App::headless_for_test();
    app.native_config_service = crate::native_config_service::VersionedConfigService::new(
        "serious_mode = false\n".to_string(),
    )
    .unwrap();
    assert!(!app.serious_mode_enabled());

    let (reply, completion) = std::sync::mpsc::channel();
    app.enqueue_control_settings_field_intent(
        crate::prefs::EDIT_SERIOUS_MODE.to_string(),
        Some(" true ".to_string()),
        reply,
    );
    assert_eq!(app.serious_mode_queued_projection, Some(true));

    // The native command arrives before the control write completes: it
    // toggles the queued true intent to false, not the live false policy to
    // true.
    app.enqueue_serious_mode_intent().unwrap();
    assert_eq!(app.serious_mode_queued_projection, Some(false));
    assert!(matches!(
        app.native_config_pending
            .get(1)
            .map(|request| &request.work),
        Some(NativeConfigWork::SeriousMode(false))
    ));

    complete_next(&mut app);
    assert!(app.serious_mode_enabled());
    assert_eq!(app.serious_mode_queued_projection, Some(false));
    assert!(completion.recv().unwrap().unwrap().starts_with("saved:"));

    complete_next(&mut app);
    assert!(!app.serious_mode_enabled());
    assert_eq!(app.serious_mode_queued_projection, None);
    // Off is Serious Mode's default, so the toggle removes the key.
    assert_eq!(
        app.native_config_service
            .value(crate::prefs::EDIT_SERIOUS_MODE)
            .unwrap(),
        None
    );
}

#[test]
fn malformed_legacy_serious_mode_value_cannot_poison_toggle_projection() {
    let mut app = App::headless_for_test();
    app.native_config_service = crate::native_config_service::VersionedConfigService::new(
        "serious_mode = false\n".to_string(),
    )
    .unwrap();
    let (reply, _completion) = std::sync::mpsc::channel();
    app.enqueue_control_settings_field_intent(
        crate::prefs::EDIT_SERIOUS_MODE.to_string(),
        Some("TRUE".to_string()),
        reply,
    );
    assert_eq!(app.serious_mode_queued_projection, None);

    app.enqueue_serious_mode_intent().unwrap();
    assert_eq!(app.serious_mode_queued_projection, Some(true));
    assert!(matches!(
        app.native_config_pending
            .back()
            .map(|request| &request.work),
        Some(NativeConfigWork::SeriousMode(true))
    ));
}

/// A reconciliation worker that comes back broken answers every queued request, so a blocked
/// `aterm ctl settings set` gets a bounded ERR instead of holding its one-shot until the wire
/// deadline.
#[test]
fn failed_reconciliation_answers_queued_callers_instead_of_leaving_them_pending() {
    let mut app = App::headless_for_test();
    app.native_config_service = crate::native_config_service::VersionedConfigService::new(
        "serious_mode = false\n".to_string(),
    )
    .unwrap();
    let (reply, completion) = std::sync::mpsc::channel();
    app.enqueue_control_settings_field_intent(
        crate::prefs::EDIT_FONT_PX.to_string(),
        Some("14".to_string()),
        reply,
    );
    app.enqueue_serious_mode_intent().unwrap();
    assert_eq!(app.native_config_pending.len(), 2);

    app.fail_native_config_reconciliation("config path is unreadable");

    assert!(
        app.native_config_pending.is_empty(),
        "a fence with no live retry must not retain requests nobody will settle"
    );
    assert_eq!(app.serious_mode_queued_projection, None);
    let error = completion
        .try_recv()
        .expect("the control caller's one-shot must be settled, not abandoned")
        .unwrap_err();
    assert!(error.contains("config path is unreadable"), "{error}");
    assert!(
        app.native_config_service.reconciliation_required(),
        "only an admitted disk generation may reopen the lane"
    );
}

/// A hand-broken `aterm.toml` is reported as what it is: the watcher posts
/// "not valid TOML" on the config lane's row and the reconciliation sample
/// fails the same way. With nothing queued, no "queued changes were not
/// written" row replaces the true one on the shared row.
#[test]
fn a_failed_reconciliation_with_nothing_queued_leaves_the_true_row_standing() {
    let mut app = App::headless_for_test();
    app.native_config_service = crate::native_config_service::VersionedConfigService::new(
        "serious_mode = false\n".to_string(),
    )
    .unwrap();
    app.surface_native_config_lane_error(
        "Config observation was not valid TOML: TOML parse error at line 2, column 19".to_string(),
    );
    assert!(app.native_config_pending.is_empty(), "nothing is queued");
    let rows = |app: &App| {
        app.messages
            .live_rows()
            .map(|l| (l.id, l.msg.title.clone(), l.msg.detail.clone()))
            .collect::<Vec<_>>()
    };
    let standing = rows(&app);
    assert!(!standing.is_empty(), "PRECONDITION: the lane's row is up");

    app.fail_native_config_reconciliation(
        "existing aterm.toml is not valid TOML: TOML parse error at line 2, column 19",
    );

    assert_eq!(
        rows(&app),
        standing,
        "nothing was queued, so nothing is reported as lost and the true row stands"
    );
    assert!(
        app.native_config_service.reconciliation_required(),
        "the fence still stands; only its false report is gone"
    );
}

/// A SAVE AN UNREADABLE aterm.toml REFUSES IS SAID IN WORDS THROUGH THE READ
/// AFTER IT: the worker's save is refused ("aterm.toml isn't readable ·
/// check its permissions"), and the read it makes for the lane to
/// resynchronize, refused the same way, carries the same words, never the
/// host's account; the `settings set` reply says the cause once. Before,
/// that read said "could not open …/aterm.toml: Permission denied (os error
/// 13)", which the reply appended ("; reconciliation required: …") and the
/// Messages entry "Couldn't save a settings change" painted as its detail.
#[cfg(unix)]
#[test]
fn a_save_an_unreadable_file_refuses_is_worded_through_the_read_after_it() {
    use crate::native_config_service::{
        ConfigKeyEdit, ConfigPatchRequest, ConfigPatchResult, ExpectedValue, VersionedConfigService,
    };
    use std::os::unix::fs::PermissionsExt as _;

    let scratch = aterm_tempfile::tempdir().expect("private unreadable config");
    let path = scratch.path().join("aterm.toml");
    std::fs::write(&path, "theme = \"Default\"\n").unwrap();
    let mut service = VersionedConfigService::load_path(&path).unwrap();
    let ConfigPatchResult::Applied { snapshot, .. } = service.patch(ConfigPatchRequest {
        base_revision: service.snapshot().revision,
        edits: vec![ConfigKeyEdit {
            key: "theme".into(),
            expected: ExpectedValue::Exact(Some("Default".to_string())),
            value: Some("Nord".to_string()),
        }],
    }) else {
        panic!("the patch reduces before it is written");
    };
    let plan = service.persistence_plan(snapshot);
    let mode = |mode: u32| {
        std::fs::set_permissions(&path, std::fs::Permissions::from_mode(mode)).unwrap();
    };
    mode(0o200);
    let completion = execute_native_config_persistence(&plan, None);
    mode(0o600);
    let words = crate::save_words::words(
        crate::save_words::Speaker::Aterm,
        &path,
        &crate::save_words::Failure::Read(&crate::native_document_host::DocumentHostError::Io {
            stage: crate::native_document_io::AtomicSaveStage::Preflight,
            message: String::new(),
            kind: Some(std::io::ErrorKind::PermissionDenied),
        }),
    );
    assert_eq!(
        completion.outcome,
        ConfigPatchOutcome::Rejected {
            message: words.clone()
        }
    );
    let Err(read) = &completion.observation else {
        panic!("the read after the save is refused too");
    };
    assert_eq!(read, &words);
    let reply = control_settings_completion_reply(
        "theme",
        Some("Nord"),
        None,
        &completion.outcome,
        Some(read),
    )
    .unwrap_err();
    assert_eq!(reply, format!("save failed: {words}"));
    assert!(!reply.contains("os error"), "{reply}");
    // A read that met something else still says it after the cause.
    let other = control_settings_completion_reply(
        "theme",
        Some("Nord"),
        None,
        &completion.outcome,
        Some("aterm.toml is too large for aterm to read"),
    )
    .unwrap_err();
    assert!(
        other.ends_with("; reconciliation required: aterm.toml is too large for aterm to read")
    );
}

#[test]
fn legacy_control_completion_never_translates_conflict_or_unverified_publication_to_ok() {
    let conflict = control_settings_completion_reply(
        crate::prefs::EDIT_COPY_ON_SELECT,
        Some("true"),
        None,
        &ConfigPatchOutcome::Conflict { revision: 9 },
        None,
    )
    .unwrap_err();
    assert!(conflict.starts_with("save conflict"), "{conflict}");

    let unverified = control_settings_completion_reply(
        crate::prefs::EDIT_COPY_ON_SELECT,
        Some("true"),
        None,
        &ConfigPatchOutcome::Indeterminate {
            message: "post-rename observation failed".to_string(),
        },
        None,
    )
    .unwrap_err();
    assert!(
        unverified.starts_with("publication unverified"),
        "{unverified}"
    );

    assert_eq!(
        control_settings_completion_reply(
            crate::prefs::EDIT_COPY_ON_SELECT,
            Some("true"),
            None,
            &ConfigPatchOutcome::Applied {
                revision: 2,
                undo: Some(1),
            },
            None,
        )
        .unwrap(),
        format!("saved: {} = true", crate::prefs::EDIT_COPY_ON_SELECT),
    );
    // A removal is never "saved" with an empty value.
    let removed = control_settings_completion_reply(
        crate::prefs::EDIT_BIDI,
        None,
        None,
        &ConfigPatchOutcome::Applied {
            revision: 3,
            undo: Some(2),
        },
        None,
    )
    .unwrap();
    assert_eq!(removed, "removed: bidi");
    assert!(!removed.contains(" = "), "{removed}");
}

/// Enqueue one `aterm ctl settings set|unset` write, reduce it at the lane
/// head and publish it with its snapshot: the reply the caller gets.
fn complete_control_field(app: &mut App, key: &str, value: Option<&str>) -> Result<String, String> {
    let (reply, completion) = std::sync::mpsc::channel();
    app.enqueue_control_settings_field_intent(key.to_string(), value.map(str::to_string), reply);
    let NativeConfigRequest { origin, work } = app.native_config_pending.pop_front().unwrap();
    let (outcome, snapshot) = match app.reduce_native_config_work(work).unwrap() {
        ConfigPatchResult::Applied { snapshot, undo } => (
            ConfigPatchOutcome::Applied {
                revision: snapshot.revision,
                undo: Some(undo.get()),
            },
            snapshot,
        ),
        ConfigPatchResult::Unchanged { snapshot } => (
            ConfigPatchOutcome::Applied {
                revision: snapshot.revision,
                undo: None,
            },
            snapshot,
        ),
        other => panic!("the write must apply: {other:?}"),
    };
    app.publish_native_config_origin(origin, outcome, Some(snapshot), false, None);
    completion.try_recv().expect("answered")
}

/// THE REPLY SAYS WHAT THE WRITE DID: `settings unset` answers "removed"
/// with what the key now reads as, in one grammar for every kind — the words
/// its Settings row's control says, then "(default)" — a `settings set` to
/// the key's default is written as a removal and answered the same way, and
/// a write that changed nothing says so. Negative controls: a real value is
/// "saved: k = v".
#[test]
fn control_settings_replies_name_removals_defaults_and_no_ops() {
    let mut app = App::headless_for_test();
    app.native_config_service = crate::native_config_service::VersionedConfigService::new(
        "bidi = \"disabled\"\nwindow_theme = \"dark\"\nallow_osc52_query = true\n[matrix_rain]\nenabled = true\n"
            .to_string(),
    )
    .unwrap();
    assert_eq!(
        complete_control_field(&mut app, crate::prefs::EDIT_BIDI, None),
        Ok("removed: bidi; now Auto (default)".to_string())
    );
    assert_eq!(
        complete_control_field(&mut app, "matrix_rain.enabled", None),
        Ok("removed: matrix_rain.enabled; now Off (default)".to_string())
    );
    assert_eq!(
        complete_control_field(&mut app, crate::prefs::EDIT_BIDI, None),
        Ok("bidi: unchanged".to_string()),
        "a second unset of an absent key changes nothing"
    );
    // `settings set <key> <default>` removes the key.
    let reply = complete_control_field(&mut app, crate::prefs::EDIT_WINDOW_THEME, Some("auto"));
    assert_eq!(
        reply,
        Ok("removed: window_theme; now Auto (default)".to_string())
    );
    let reply = complete_control_field(
        &mut app,
        crate::prefs::EDIT_ALLOW_OSC52_QUERY,
        Some("false"),
    );
    assert_eq!(
        reply,
        Ok("removed: allow_osc52_query; now Off (default)".to_string())
    );
    let text = app.native_config_service.snapshot().text;
    assert!(
        !text.contains("window_theme")
            && !text.contains("allow_osc52_query")
            && !text.contains("bidi"),
        "{text}"
    );
    assert_eq!(
        complete_control_field(&mut app, crate::prefs::EDIT_WINDOW_THEME, Some("auto")),
        Ok("window_theme: unchanged".to_string()),
        "the default again, with the key absent, changes nothing"
    );
    assert_eq!(
        complete_control_field(&mut app, crate::prefs::EDIT_BIDI, Some("disabled")),
        Ok("saved: bidi = disabled".to_string())
    );
}

/// ONE GRAMMAR FOR WHAT A KEY NOW READS AS, from the row: a list's empty
/// field says "none", never its input hint; a choice its picker's words, not
/// its config token; a value a theme supplies is the theme's, never aterm's
/// default (a theme setting `line_height = 1.5` and `ligatures = false` reads
/// "1.5 … (from theme …)" and "Off (from theme …)"). Negative control: the
/// same keys with no theme read as aterm's defaults.
#[test]
fn control_settings_unset_says_the_rows_value_and_where_it_comes_from() {
    let mut app = App::headless_for_test();
    app.native_config_service = crate::native_config_service::VersionedConfigService::new(
        "font_features = [\"ss01\"]\ncursor_trail_style = \"comet\"\nmotion = \"full\"\n"
            .to_string(),
    )
    .unwrap();
    assert_eq!(
        complete_control_field(&mut app, crate::prefs::EDIT_FONT_FEATURES, None),
        Ok("removed: font_features; now none (default)".to_string())
    );
    assert_eq!(
        complete_control_field(&mut app, crate::prefs::EDIT_CURSOR_TRAIL_STYLE, None),
        Ok("removed: cursor_trail_style; now Rainbow kitty (default)".to_string())
    );
    assert_eq!(
        complete_control_field(&mut app, crate::prefs::EDIT_MOTION, None),
        Ok("removed: motion; now Automatic (default)".to_string())
    );

    let studio = {
        let text =
            "foreground = #e0e0e0\nbackground = #101418\nline_height = 1.5\nligatures = false\n";
        let parsed = aterm_types::scheme::parse_theme_str(text).expect("a theme");
        crate::app_config::ThemeCatalog::from_parsed([("Studio".to_string(), parsed)])
    };
    for (source, themed) in [
        (
            "theme = \"Studio\"\nline_height = 1.2\nligatures = true\n",
            true,
        ),
        ("line_height = 1.2\nligatures = true\n", false),
    ] {
        let mut app = App::headless_for_test();
        app.native_config_service =
            crate::native_config_service::VersionedConfigService::new_with_themes(
                source.to_string(),
                std::sync::Arc::clone(&studio),
            )
            .unwrap();
        let line = complete_control_field(&mut app, crate::prefs::EDIT_LINE_HEIGHT, None).unwrap();
        let ligatures =
            complete_control_field(&mut app, crate::prefs::EDIT_LIGATURES, None).unwrap();
        if themed {
            assert!(
                line.starts_with("removed: line_height; now 1.5")
                    && line.ends_with("(from theme Studio)"),
                "{line}"
            );
            assert_eq!(ligatures, "removed: ligatures; now Off (from theme Studio)");
        } else {
            assert!(
                line.starts_with("removed: line_height; now 1") && line.ends_with("(default)"),
                "{line}"
            );
            assert_eq!(ligatures, "removed: ligatures; now On (default)");
        }
    }
}

/// A ROW HELD OFF IS NEVER ITS KEY'S DEFAULT: under Serious Mode the cursor
/// style and Music effects read Off, and with every word effect switched off
/// Keyword kitties does, yet each key's own default is a trail or On; so
/// `settings unset` says the key's value and then what holds the row Off,
/// never "now Off (default)". A theme's font is named as from that theme
/// once. Negative control: with nothing holding it, the switch is just "On
/// (default)".
#[test]
fn control_settings_unset_never_calls_a_held_off_row_its_default() {
    let unset = |source: &str, key: &str| {
        let mut app = App::headless_for_test();
        app.native_config_service =
            crate::native_config_service::VersionedConfigService::new(source.to_string()).unwrap();
        complete_control_field(&mut app, key, None).unwrap()
    };
    let serious = "serious_mode = true\ncursor_trail_style = \"comet\"\ntrail_sounds = false\n";
    if crate::app_config::DEFAULT_DECORATIVE_EFFECTS {
        assert_eq!(
            unset(serious, crate::prefs::EDIT_CURSOR_TRAIL_STYLE),
            "removed: cursor_trail_style; now Rainbow kitty (default); Off while Serious Mode \
             hides playful effects"
        );
    }
    assert_eq!(
        unset(serious, crate::prefs::EDIT_TRAIL_SOUNDS),
        "removed: trail_sounds; now On (default); Off while Serious Mode silences sound"
    );
    assert_eq!(
        unset(
            "[sparkle_words]\nenabled = false\n[sparkle_words.feline]\nenabled = false\n",
            "sparkle_words.feline.enabled",
        ),
        "removed: sparkle_words.feline.enabled; now On (default); Off while all word effects \
         are switched off in aterm.toml"
    );
    assert_eq!(
        unset("trail_sounds = false\n", crate::prefs::EDIT_TRAIL_SOUNDS),
        "removed: trail_sounds; now On (default)"
    );

    let mut app = App::headless_for_test();
    let studio = {
        let text = "foreground = #e0e0e0\nbackground = #101418\nfont_family = Menlo\n";
        let parsed = aterm_types::scheme::parse_theme_str(text).expect("a theme");
        crate::app_config::ThemeCatalog::from_parsed([("Studio".to_string(), parsed)])
    };
    app.native_config_service =
        crate::native_config_service::VersionedConfigService::new_with_themes(
            "theme = \"Studio\"\nfont_family = \"Monaco\"\n".to_string(),
            studio,
        )
        .unwrap();
    assert_eq!(
        complete_control_field(&mut app, crate::prefs::EDIT_FONT_FAMILY, None),
        Ok("removed: font_family; now Menlo (from theme Studio)".to_string())
    );
}

/// A CHOICE SET IN ITS ROW'S WORDS IS THE OPTION THEY NAME: `settings set
/// motion Reduce` (the segment's word) and `Always animate` (the picker's)
/// save `reduced` and `full`, a switch's "On"/"Off" its `true`/`false`; a
/// value no option takes is refused with every option and its words, before
/// anything is queued. Negative control: a config token still saves as is.
#[test]
fn control_settings_take_a_rows_words_and_refuse_with_every_option() {
    let mut app = App::headless_for_test();
    app.native_config_service =
        crate::native_config_service::VersionedConfigService::new(String::new()).unwrap();
    // The switch's word for the value that is not its default, which a write
    // saves: copy-on-select is off by default on Linux (the selection owns
    // PRIMARY there) and on elsewhere.
    let (switch_words, switch_saved) = if cfg!(target_os = "linux") {
        ("On", "true")
    } else {
        ("Off", "false")
    };
    for (key, words, saved) in [
        (crate::prefs::EDIT_MOTION, "Reduce", "reduced"),
        (crate::prefs::EDIT_MOTION, "Always animate", "full"),
        (crate::prefs::EDIT_MOTION, "reduced", "reduced"),
        (crate::prefs::EDIT_BIDI, "If asked", "explicit"),
        (
            crate::prefs::EDIT_COPY_ON_SELECT,
            switch_words,
            switch_saved,
        ),
    ] {
        let reply = complete_control_field(&mut app, key, Some(words));
        assert_eq!(
            reply,
            Ok(format!("saved: {key} = {saved}")),
            "{key} {words:?}"
        );
    }
    let (reply, answer) = std::sync::mpsc::channel();
    app.queue_control_settings_field(
        crate::prefs::EDIT_MOTION.to_string(),
        Some("reduce motions".to_string()),
        reply,
    );
    let refusal = answer.try_recv().expect("answered at once").unwrap_err();
    assert_eq!(
        refusal,
        "save failed: invalid value for motion: \"reduce motions\"; it must be one of: auto \
         (Automatic), full (Always animate, Animate), reduced (Reduce motion, Reduce)"
    );
    assert!(app.native_config_pending.is_empty(), "nothing is queued");
}

/// A DURABLE WRITE WHOSE DISK GENERATION COULD NOT BE ADMITTED ANSWERS WITH
/// WHAT IT WROTE, not the text it was asked for: the worker proved the
/// service's candidate on disk, so a `settings set` to the key's default is
/// "removed" (it used to say "saved: window_theme = auto" for a key the file
/// no longer holds), a number is spelled as written ("15", not the "15.0"
/// typed), and that the save still needs reconciling comes after what it
/// did. Negative control: the same writes with their snapshot read the same.
#[test]
fn a_durable_write_awaiting_reconciliation_answers_with_what_it_wrote() {
    let publish_unadmitted = |app: &mut App, key: &str, value: &str| {
        let (reply, completion) = std::sync::mpsc::channel();
        app.enqueue_control_settings_field_intent(key.to_string(), Some(value.to_string()), reply);
        let NativeConfigRequest { origin, work } = app.native_config_pending.pop_front().unwrap();
        let ConfigPatchResult::Applied { snapshot, undo } =
            app.reduce_native_config_work(work).unwrap()
        else {
            panic!("the write must apply");
        };
        app.publish_native_config_origin(
            origin,
            ConfigPatchOutcome::Applied {
                revision: snapshot.revision,
                undo: Some(undo.get()),
            },
            None,
            false,
            Some("the file changed again before it could be read".to_string()),
        );
        completion.try_recv().expect("answered")
    };
    let mut app = App::headless_for_test();
    app.native_config_service = crate::native_config_service::VersionedConfigService::new(
        "window_theme = \"dark\"\nfont_px = 13.0\n".to_string(),
    )
    .unwrap();
    assert_eq!(
        publish_unadmitted(&mut app, crate::prefs::EDIT_WINDOW_THEME, "auto"),
        Ok(
            "removed: window_theme; now Auto (default); reconciliation required: the file \
            changed again before it could be read"
                .to_string()
        )
    );
    assert_eq!(
        publish_unadmitted(&mut app, crate::prefs::EDIT_FONT_PX, "15.0"),
        Ok(
            "saved: font_px = 15; reconciliation required: the file changed again before it \
            could be read"
                .to_string()
        )
    );
    let text = app.native_config_service.snapshot().text;
    assert!(
        !text.contains("window_theme") && text.contains("font_px = 15\n"),
        "{text}"
    );
    // The same writes, admitted, answer the same.
    let mut app = App::headless_for_test();
    app.native_config_service = crate::native_config_service::VersionedConfigService::new(
        "window_theme = \"dark\"\nfont_px = 13.0\n".to_string(),
    )
    .unwrap();
    assert_eq!(
        complete_control_field(&mut app, crate::prefs::EDIT_WINDOW_THEME, Some("auto")),
        Ok("removed: window_theme; now Auto (default)".to_string())
    );
    assert_eq!(
        complete_control_field(&mut app, crate::prefs::EDIT_FONT_PX, Some("15.0")),
        Ok("saved: font_px = 15".to_string())
    );
}

#[test]
fn legacy_control_field_is_materialized_at_the_versioned_lane_head() {
    // copy_on_select's default is this platform's (on, but off on Linux,
    // where a selection already owns PRIMARY): the file names the other value
    // and the control writes the default back.
    let default = crate::app_config::Config::default().copy_on_select_or_default();
    let mut app = App::headless_for_test();
    app.native_config_service = crate::native_config_service::VersionedConfigService::new(format!(
        "copy_on_select = {}\nfont_px = 14.0\n",
        !default
    ))
    .unwrap();

    // Advance the service before reducing the absolute control intent: the
    // work binds to this newest revision/value, not a file read or an
    // enqueue-time baseline.
    let snapshot = app.native_config_service.snapshot();
    let base = snapshot.revision;
    let expected_font = snapshot
        .values()
        .unwrap()
        .remove(crate::prefs::EDIT_FONT_PX);
    assert!(matches!(
        app.native_config_service.patch(ConfigPatchRequest {
            base_revision: base,
            edits: vec![ConfigKeyEdit {
                key: crate::prefs::EDIT_FONT_PX.to_string(),
                expected: ExpectedValue::Exact(expected_font),
                value: Some("16.0".to_string()),
            }],
        }),
        ConfigPatchResult::Applied { .. }
    ));

    let reduced = app
        .reduce_native_config_work(NativeConfigWork::ControlField {
            key: crate::prefs::EDIT_COPY_ON_SELECT.to_string(),
            value: Some(default.to_string()),
        })
        .unwrap();
    assert!(matches!(reduced, ConfigPatchResult::Applied { .. }));
    // The value written is copy_on_select's default, so the write removes the key.
    assert_eq!(
        app.native_config_service
            .value(crate::prefs::EDIT_COPY_ON_SELECT)
            .unwrap(),
        None
    );
    assert_eq!(
        app.native_config_service
            .value(crate::prefs::EDIT_FONT_PX)
            .unwrap()
            .as_deref(),
        Some("16"),
        "the serialized legacy edit preserves earlier lane work"
    );
}

#[test]
fn legacy_control_enqueue_reports_an_unknown_key_without_stranding_a_reply() {
    let mut app = App::headless_for_test();
    let (unknown_reply, unknown_completion) = std::sync::mpsc::channel();
    app.queue_control_settings_field(
        "not_a_real_setting".to_string(),
        Some("true".to_string()),
        unknown_reply,
    );
    assert!(
        unknown_completion
            .recv()
            .unwrap()
            .unwrap_err()
            .contains("unknown key")
    );

    // A retired key is not an unknown one: `settings set|unset` tells its
    // removal story, the band's sentence, for both verbs.
    for value in [Some("off".to_string()), None] {
        let (retired_reply, retired_completion) = std::sync::mpsc::channel();
        app.queue_control_settings_field(
            "title_summary_provider".to_string(),
            value,
            retired_reply,
        );
        let error = retired_completion.recv().unwrap().unwrap_err();
        assert_eq!(
            error,
            "title_summary_provider does nothing: Smart Titles was removed; safe to delete"
        );
    }
    assert!(app.native_config_pending.is_empty());
}

#[test]
fn applied_completion_observation_failure_closes_gate_before_pumping_next_write() {
    let dir = std::env::temp_dir().join(format!(
        "aterm-native-config-applied-reconcile-{}",
        std::process::id()
    ));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    let path = dir.join("aterm.toml");
    std::fs::write(&path, "serious_mode = false\n").unwrap();

    let mut app = App::headless_for_test();
    app.native_config_service =
        crate::native_config_service::VersionedConfigService::load_path(&path).unwrap();
    let base_revision = app.native_config_service.snapshot().revision;
    let ConfigPatchResult::Applied { snapshot, undo } =
        app.native_config_service.patch(ConfigPatchRequest {
            base_revision,
            edits: vec![ConfigKeyEdit {
                key: crate::prefs::EDIT_SERIOUS_MODE.to_string(),
                expected: ExpectedValue::Exact(Some("false".to_string())),
                value: Some("true".to_string()),
            }],
        })
    else {
        panic!("test candidate must reduce");
    };
    std::fs::write(&path, snapshot.text.as_bytes()).unwrap();
    app.native_config_inflight = true;
    app.native_config_pending.push_back(NativeConfigRequest {
        origin: NativeConfigOrigin::SeriousMode { desired: false },
        work: NativeConfigWork::SeriousMode(false),
    });

    // The worker proved its candidate, then a non-cooperating writer made
    // the path unobservable before completion reconciliation.
    std::fs::remove_file(&path).unwrap();
    std::fs::create_dir(&path).unwrap();
    app.finish_native_config_write(
        NativeConfigOrigin::SeriousMode { desired: true },
        NativeConfigPersistenceCompletion {
            outcome: ConfigPatchOutcome::Applied {
                revision: snapshot.revision,
                undo: Some(undo.get()),
            },
            observation: Err("exact committed observation was lost".to_string()),
        },
    );

    assert!(app.native_config_service.reconciliation_required());
    assert_eq!(
        app.native_config_pending.len(),
        1,
        "queued work cannot be consumed against the pre-write baseline"
    );
    assert!(!app.native_config_inflight);
    let _ = std::fs::remove_dir_all(dir);
}

#[test]
fn three_rapid_serious_mode_intents_rebase_through_each_completion() {
    struct PendingCompletion {
        desired: bool,
        outcome: ConfigPatchOutcome,
        snapshot: crate::native_config_service::ConfigSnapshot,
    }

    fn start_next(app: &mut App) -> PendingCompletion {
        let NativeConfigRequest { origin, work } = app
            .native_config_pending
            .pop_front()
            .expect("queued Serious Mode intent");
        let desired = match origin {
            NativeConfigOrigin::SeriousMode { desired } => desired,
            NativeConfigOrigin::View { .. }
            | NativeConfigOrigin::Control { .. }
            | NativeConfigOrigin::Presence { .. }
            | NativeConfigOrigin::ThemeChoice { .. }
            | NativeConfigOrigin::StatsBars { .. }
            | NativeConfigOrigin::DeckSize { .. }
            | NativeConfigOrigin::HarnessSwitch { .. } => {
                panic!("unexpected non-Serious-Mode request")
            }
        };
        assert!(matches!(
            &work,
            NativeConfigWork::SeriousMode(value) if *value == desired
        ));
        let (outcome, snapshot) = match app.reduce_native_config_work(work).unwrap() {
            ConfigPatchResult::Applied { snapshot, undo } => (
                ConfigPatchOutcome::Applied {
                    revision: snapshot.revision,
                    undo: Some(undo.get()),
                },
                snapshot,
            ),
            ConfigPatchResult::Unchanged { snapshot } => (
                ConfigPatchOutcome::Applied {
                    revision: snapshot.revision,
                    undo: None,
                },
                snapshot,
            ),
            result => panic!("serialized semantic intent must apply: {result:?}"),
        };
        PendingCompletion {
            desired,
            outcome,
            snapshot,
        }
    }

    fn complete(app: &mut App, completion: PendingCompletion) {
        app.publish_native_config_origin(
            NativeConfigOrigin::SeriousMode {
                desired: completion.desired,
            },
            completion.outcome,
            Some(completion.snapshot),
            false,
            None,
        );
        assert_eq!(app.serious_mode_enabled(), completion.desired);
    }

    fn project(
        model: &aterm_spec::derive::Model,
        app: &App,
        current: Option<bool>,
        queued_expected: &std::collections::VecDeque<bool>,
        issued: i64,
        completed: i64,
    ) -> aterm_spec::interp::State {
        let mut state = model.init_state();
        let live = app.serious_mode_enabled();
        let service = app
            .native_config_service
            .value(crate::prefs::EDIT_SERIOUS_MODE)
            .unwrap()
            .as_deref()
            == Some("true");
        let queued = app
            .native_config_pending
            .iter()
            .map(|request| match request.origin {
                NativeConfigOrigin::SeriousMode { desired } => desired,
                NativeConfigOrigin::View { .. }
                | NativeConfigOrigin::Control { .. }
                | NativeConfigOrigin::Presence { .. }
                | NativeConfigOrigin::ThemeChoice { .. }
                | NativeConfigOrigin::StatsBars { .. }
                | NativeConfigOrigin::DeckSize { .. }
                | NativeConfigOrigin::HarnessSwitch { .. } => {
                    panic!("unexpected non-Serious-Mode request")
                }
            })
            .collect::<Vec<_>>();
        state.insert("live", i64::from(live));
        state.insert("service", i64::from(service));
        state.insert(
            "projection",
            i64::from(app.serious_mode_queued_projection.unwrap_or(live)),
        );
        state.insert("inflight", i64::from(current.is_some()));
        state.insert("current_desired", i64::from(current.unwrap_or(live)));
        state.insert("queue_count", queued.len() as i64);
        state.insert("q1", i64::from(queued.first().copied().unwrap_or(false)));
        state.insert("q2", i64::from(queued.get(1).copied().unwrap_or(false)));
        state.insert(
            "q1_expected",
            i64::from(queued_expected.front().copied().unwrap_or(false)),
        );
        state.insert(
            "q2_expected",
            i64::from(queued_expected.get(1).copied().unwrap_or(false)),
        );
        state.insert("issued", issued);
        state.insert("completed", completed);
        state.insert("conflict", 0);
        // Read from the lane (newest pending, else in flight), not from
        // `serious_mode_queued_projection` (`projection`'s source), so
        // `ProjectionTracksLatestIntent` is not trivially true.
        state.insert(
            "last_desired",
            i64::from(queued.last().copied().or(current).unwrap_or(live)),
        );
        state.insert("intent_kind", i64::from(issued > 0));
        state
    }

    fn assert_action(
        model: &aterm_spec::derive::Model,
        before: &aterm_spec::interp::State,
        after: &aterm_spec::interp::State,
        action: &'static str,
    ) {
        assert_eq!(
            model.successors(action, before).as_slice(),
            std::slice::from_ref(after),
            "shipping queue transition must refine {action}"
        );
        assert_eq!(
            aterm_spec::interp::admits(model, before, after),
            Some(action)
        );
        for invariant in &model.invariants {
            assert!(
                model.check_invariant(invariant.name, after),
                "post-state violates {}::{}: {after:?}",
                model.name,
                invariant.name
            );
        }
    }

    let model = aterm_models_native::serious_mode_intent_queue_model();
    let mut app = App::headless_for_test();
    app.native_config_service = crate::native_config_service::VersionedConfigService::new(
        "serious_mode = false\n".to_string(),
    )
    .unwrap();
    assert!(!app.set_serious_mode(false));
    let baseline_revision = app.native_config_service.snapshot().revision;
    let mut issued = 0;
    let mut completed = 0;
    let mut queued_expected = std::collections::VecDeque::new();
    let mut state = project(&model, &app, None, &queued_expected, issued, completed);
    assert_eq!(state, model.init_state());

    app.enqueue_serious_mode_intent().unwrap();
    let mut current = start_next(&mut app);
    issued += 1;
    let after = project(
        &model,
        &app,
        Some(current.desired),
        &queued_expected,
        issued,
        completed,
    );
    assert_action(&model, &state, &after, "StartToggle");
    state = after;

    queued_expected.push_back(true);
    app.enqueue_serious_mode_intent().unwrap();
    issued += 1;
    let after = project(
        &model,
        &app,
        Some(current.desired),
        &queued_expected,
        issued,
        completed,
    );
    assert_action(&model, &state, &after, "QueueToggle");
    state = after;

    queued_expected.push_back(true);
    app.enqueue_serious_mode_intent().unwrap();
    issued += 1;
    let after = project(
        &model,
        &app,
        Some(current.desired),
        &queued_expected,
        issued,
        completed,
    );
    assert_action(&model, &state, &after, "QueueToggle");
    state = after;

    complete(&mut app, current);
    queued_expected.pop_front();
    current = start_next(&mut app);
    completed += 1;
    let after = project(
        &model,
        &app,
        Some(current.desired),
        &queued_expected,
        issued,
        completed,
    );
    assert_action(&model, &state, &after, "Complete");

    // Negative control: a completion that re-reads the pending queue
    // front-first (`refresh_serious_mode_queued_projection` without its
    // `.rev()`) shows the OFF intent now in flight while ON is the newest.
    let mut oldest_first = after.clone();
    oldest_first.insert("projection", i64::from(current.desired));
    assert_ne!(oldest_first["projection"], after["projection"]);
    assert_eq!(
        aterm_spec::interp::with_buggy(&model, 1).successors("Complete", &state),
        vec![oldest_first.clone()]
    );
    assert_eq!(
        aterm_spec::interp::admits(&model, &state, &oldest_first),
        None
    );
    assert!(!model.check_invariant("ProjectionTracksLatestIntent", &oldest_first));
    state = after;

    // Negative control: an enqueue-time expected value conflicts the third
    // intent here and is not an admitted transition of the shipping
    // (Buggy=0) model.
    let buggy = aterm_spec::interp::with_buggy(&model, 1);
    let stale_third = buggy.successors("Complete", &state)[0].clone();
    assert_eq!(stale_third["conflict"], 1);
    assert_eq!(
        aterm_spec::interp::admits(&model, &state, &stale_third),
        None
    );
    assert!(!buggy.check_invariant("NoSerializedConflict", &stale_third));

    complete(&mut app, current);
    queued_expected.pop_front();
    current = start_next(&mut app);
    completed += 1;
    let after = project(
        &model,
        &app,
        Some(current.desired),
        &queued_expected,
        issued,
        completed,
    );
    assert_action(&model, &state, &after, "Complete");
    state = after;

    complete(&mut app, current);
    completed += 1;
    let after = project(&model, &app, None, &queued_expected, issued, completed);
    assert_action(&model, &state, &after, "Complete");
    assert!(app.native_config_pending.is_empty());
    assert_eq!(app.serious_mode_queued_projection, None);
    assert_eq!(
        app.native_config_service.snapshot().revision,
        baseline_revision + 3
    );
    assert_eq!(
        app.native_config_service
            .value(crate::prefs::EDIT_SERIOUS_MODE)
            .unwrap()
            .as_deref(),
        Some("true")
    );
}

#[test]
fn durable_serious_mode_snapshot_updates_runtime_and_open_settings_immediately() {
    let mut app = App::headless_for_test();
    let wid = WindowId(0);
    assert!(app.open_settings_tab(crate::native_settings::SettingsRoute::Home));
    let (_, view) = app.active_native_view(wid).unwrap();
    let before_presentation = app
        .native_runtime
        .view_state(view)
        .unwrap()
        .common()
        .presentation_revision;
    let mut service =
        crate::native_config_service::VersionedConfigService::new(String::new()).unwrap();
    let snapshot = match service.patch(crate::native_config_service::ConfigPatchRequest {
        base_revision: 1,
        edits: vec![crate::native_config_service::ConfigKeyEdit {
            key: crate::prefs::EDIT_SERIOUS_MODE.to_string(),
            expected: crate::native_config_service::ExpectedValue::Exact(None),
            value: Some("true".to_string()),
        }],
    }) {
        crate::native_config_service::ConfigPatchResult::Applied { snapshot, .. } => snapshot,
        other => panic!("serious-mode fixture patch failed: {other:?}"),
    };

    assert!(app.apply_serious_mode_config_snapshot(&snapshot));

    assert!(app.serious_mode_enabled());
    assert_eq!(app.config.serious_mode, Some(true));
    let Some(crate::native_app::AppViewState::Settings(state)) =
        app.native_runtime.view_state(view)
    else {
        panic!("Settings view");
    };
    assert!(state.common.presentation_revision > before_presentation);
    let field = state
        .legacy
        .fields
        .iter()
        .find(|field| field.key == crate::prefs::EDIT_SERIOUS_MODE)
        .unwrap();
    assert_eq!(crate::settings::SettingsState::display_value(field), "true");
}

#[test]
fn serious_mode_conflict_keeps_disk_authority() {
    let mut app = App::headless_for_test();
    assert!(app.set_serious_mode(true));
    let authoritative = crate::native_config_service::VersionedConfigService::new(
        "serious_mode = false\n".to_string(),
    )
    .unwrap()
    .snapshot();

    app.publish_serious_mode_completion(
        true,
        ConfigPatchOutcome::Conflict {
            revision: authoritative.revision,
        },
        Some(authoritative),
        None,
    );

    assert!(!app.serious_mode_enabled());
    assert_eq!(app.config.serious_mode, Some(false));
}

#[test]
fn sole_worker_stamps_fifo_read_order_not_request_order() {
    let (sender, receiver) = std::sync::mpsc::sync_channel(4);
    let (drained_tx, drained_rx) = std::sync::mpsc::sync_channel(4);
    let worker = std::thread::spawn(move || {
        run_native_update_worker(
            receiver,
            |ticket, observation_sequence, _| NativeUpdateReconcileFacts {
                _ticket: ticket,
                observation_sequence,
                observed_at: std::time::Instant::now(),
                durable: None,
                installed: None,
                installed_unproven: false,
                installable: None,
            },
            |_| {},
            || drained_tx.send(()).unwrap(),
        );
    });
    let enqueue = |request_sequence| {
        let (reply, result) = std::sync::mpsc::sync_channel(1);
        sender
            .send(NativeUpdateWorkerRequest::Reconcile(
                NativeUpdateReconcileRequest {
                    ticket: NativeUpdateReconcileTicket { request_sequence },
                    current_build: 10,
                    destination: NativeUpdateFactDestination::Reply(reply),
                },
            ))
            .unwrap();
        result
    };

    // A later-minted request can reach the shared FIFO first (handoff and UI
    // producers race). Observation identity follows the actual read order.
    let later_request = enqueue(2);
    let earlier_request = enqueue(1);
    let later_facts = later_request.recv().unwrap();
    let earlier_facts = earlier_request.recv().unwrap();
    assert_eq!(later_facts._ticket.request_sequence(), 2);
    assert_eq!(later_facts.observation_sequence, 1);
    assert_eq!(earlier_facts._ticket.request_sequence(), 1);
    assert_eq!(earlier_facts.observation_sequence, 2);
    drained_rx.recv().unwrap();
    drained_rx.recv().unwrap();
    drop(sender);
    worker.join().unwrap();
}

#[test]
fn durable_marker_is_observed_before_installed_bundle_identity() {
    let step = std::cell::Cell::new(0_u8);
    let facts = read_native_update_reconcile_facts_with(
        NativeUpdateReconcileTicket {
            request_sequence: 1,
        },
        1,
        || {
            assert_eq!(step.replace(1), 0);
            None
        },
        || {
            assert_eq!(step.replace(2), 1, "installed probe ran before ready read");
            Ok(None)
        },
    );
    assert_eq!(step.get(), 2);
    assert!(facts.durable.is_none() && facts.installed.is_none());
    assert!(!facts.installed_unproven, "no bundle is a proven absence");

    // An installed bundle whose verification did not finish (a passing refusal: `spctl` past its
    // ceiling) is unproven, no observation; one refused as a verdict is a proven absence.
    let read = |installed: Result<Option<InstalledUpdate>, String>| {
        read_native_update_reconcile_facts_with(
            NativeUpdateReconcileTicket {
                request_sequence: 1,
            },
            1,
            || None,
            || installed,
        )
    };
    let moment = read(Err(format!(
        "bundle policy: spctl --assess did not finish within 30s ({})",
        aterm_update::PASSING_REFUSAL_KEY
    )));
    assert!(moment.installed.is_none() && moment.installed_unproven);
    let verdict = read(Err(
        "bundle policy: codesign --verify (team-pinned requirement) failed: x: code object \
         is not signed at all"
            .to_string(),
    ));
    assert!(verdict.installed.is_none() && !verdict.installed_unproven);
}

#[test]
fn updater_ui_snapshots_do_not_probe_ledgers_or_installed_bundle() {
    let app = App::headless_for_test();
    let before = UPDATE_FACT_PROBES_ON_THREAD.with(std::cell::Cell::get);
    let _ = app.update_snapshot(false).projection();
    let after = UPDATE_FACT_PROBES_ON_THREAD.with(std::cell::Cell::get);
    assert_eq!(before, after, "UI projection invoked an updater disk probe");
}

#[test]
fn saturated_boot_health_queue_retains_nonblocking_retry() {
    let mut app = App::headless_for_test();
    let (sender, receiver) = std::sync::mpsc::sync_channel(1);
    sender
        .try_send(NativeUpdateWorkerRequest::ConfirmBootHealth {
            current_build: 9,
            witness: aterm_update::TrialWitness::Window,
        })
        .unwrap();
    app.native_update_reconcile_worker = Some(sender);

    assert_eq!(
        app.request_native_boot_health_confirmation(),
        NativeUpdateDispatch::Saturated,
        "a full queue must retain the pending latch"
    );
    let _ = receiver.recv().unwrap();
    assert_eq!(
        app.request_native_boot_health_confirmation(),
        NativeUpdateDispatch::Queued
    );
    // The fixture is headless: its confirmation says so, and settles only a trial no window has
    // launched. A windowed App's is a window's.
    assert!(matches!(
        receiver.recv().unwrap(),
        NativeUpdateWorkerRequest::ConfirmBootHealth {
            current_build: 10,
            witness: aterm_update::TrialWitness::Headless,
        }
    ));
    app.headless = false;
    assert_eq!(
        app.request_native_boot_health_confirmation(),
        NativeUpdateDispatch::Queued
    );
    assert!(matches!(
        receiver.recv().unwrap(),
        NativeUpdateWorkerRequest::ConfirmBootHealth {
            current_build: 10,
            witness: aterm_update::TrialWitness::Window,
        }
    ));
}

#[test]
fn recovery_host_revalidates_capabilities_and_never_interprets_diagnostics() {
    let mut app = App::headless_for_test();
    let denied = app.execute_recovery_request(
        WindowId(0),
        crate::native_app::RecoveryRequest::OpenOriginal {
            uri: "file:///tmp/safe.md\nhttps://attacker.example".to_string(),
        },
    );
    assert!(matches!(
        denied,
        crate::native_app::RecoveryOutcome::Denied { .. }
    ));

    let invalid_route = app.execute_recovery_request(
        WindowId(0),
        crate::native_app::RecoveryRequest::Retry(
            crate::native_app::RecoveryCapability::Settings {
                route: "../../diagnostics-from-metadata".to_string(),
            },
        ),
    );
    assert!(matches!(
        invalid_route,
        crate::native_app::RecoveryOutcome::Denied { .. }
    ));

    let settings = app.execute_recovery_request(
        WindowId(0),
        crate::native_app::RecoveryRequest::Retry(
            crate::native_app::RecoveryCapability::Settings {
                route: "/about".to_string(),
            },
        ),
    );
    assert!(matches!(
        settings,
        crate::native_app::RecoveryOutcome::Opened { .. }
    ));
}

#[test]
fn idle_reconcile_park_never_materializes_a_destination_or_warning_path() {
    let mut app = App::headless_for_test();
    assert!(!app.has_pending_native_update_reconcile());

    // The shipping wrapper with only proxy materialization injected: moving
    // the clone above the guard increments the counter and fails the test.
    let materializations = std::cell::Cell::new(0_u32);
    app.retry_pending_native_update_reconcile_via(|_| {
        materializations.set(materializations.get() + 1);
        None
    });
    assert_eq!(
        materializations.get(),
        0,
        "idle park materialized/woke an EventLoopProxy"
    );
}

/// Tier-1 binding for `NativeUpdateWorkerQueue`: the real capacity through `try_send`
/// Full/drain/retry, the purpose coalescer and the disconnected restart path. The model covers
/// that visible boundary, not worker scheduling.
#[test]
fn native_update_worker_queue_conforms_to_saturation_coalescing_and_disconnect() {
    // Records an apply refusal in the shared update ledger.
    let _ledger = crate::app_update_screen::hold_update_ledger_for_test();
    let model = aterm_models_update::native_update_worker_queue_model();
    let mut state = model.init_state();

    let mut app = App::headless_for_test();
    let (sender, receiver) = std::sync::mpsc::sync_channel(NATIVE_UPDATE_WORKER_CAPACITY);
    sender
        .try_send(NativeUpdateWorkerRequest::ConfirmBootHealth {
            current_build: 9,
            witness: aterm_update::TrialWitness::Window,
        })
        .unwrap();
    app.native_update_reconcile_worker = Some(sender);
    assert!(model.fire("OccupyWorker", &mut state));

    let (stage_reply, _stage_result) = std::sync::mpsc::sync_channel(1);
    assert!(app.request_native_update_reconcile_with(
        NativeUpdateReconcilePurpose::StageAvailable,
        |_| NativeUpdateFactDestination::Reply(stage_reply),
    ));
    assert!(model.fire("RequestStageFull", &mut state));
    assert_eq!(
        app.pending_native_update_reconcile_purpose,
        Some(NativeUpdateReconcilePurpose::StageAvailable)
    );
    assert_eq!(state.get("pending"), Some(&1));

    // A stronger intent arriving during the same saturation episode must
    // replace only the purpose, never allocate or lose a second request.
    let (apply_reply, _apply_result) = std::sync::mpsc::sync_channel(1);
    assert!(
        app.request_native_update_reconcile_with(
            NativeUpdateReconcilePurpose::ApplyControl,
            |_| NativeUpdateFactDestination::Reply(apply_reply),
        )
    );
    assert!(model.fire("UpgradePendingToApply", &mut state));
    assert_eq!(
        app.pending_native_update_reconcile_purpose,
        Some(NativeUpdateReconcilePurpose::ApplyControl)
    );
    assert_eq!(state.get("purpose"), Some(&2));

    let _ = receiver.recv().unwrap();
    assert!(model.fire("WorkerDrainsFiller", &mut state));
    let observed = std::cell::Cell::new(None);
    let (reply, result) = std::sync::mpsc::sync_channel(1);
    assert_eq!(
        app.retry_pending_native_update_reconcile_with(|purpose| {
            observed.set(Some(purpose));
            NativeUpdateFactDestination::Reply(reply)
        }),
        NativeUpdateDispatch::Queued
    );
    assert!(model.fire("RetryPendingOnDrain", &mut state));
    assert!(app.pending_native_update_reconcile_purpose.is_none());
    assert_eq!(
        observed.get(),
        Some(NativeUpdateReconcilePurpose::ApplyControl),
        "the real coalescer must preserve the strongest accepted purpose"
    );

    let NativeUpdateWorkerRequest::Reconcile(request) = receiver.recv().unwrap() else {
        panic!("the retained intent must become one real worker request");
    };
    assert!(model.fire("WorkerCompletesIntent", &mut state));
    let NativeUpdateFactDestination::Reply(reply) = request.destination else {
        panic!("the conformance request uses a reply destination");
    };
    reply
        .send(reconcile_facts(request.ticket.request_sequence(), 1, None))
        .unwrap();
    app.finish_native_update_reconcile(observed.get().unwrap(), result.recv().unwrap());
    assert!(model.fire("ReduceCompletion", &mut state));
    assert_eq!(state.get("delivered"), Some(&1));
    assert!(model.check_invariant("NoSilentlyLostAcceptedIntent", &state));

    // Dispatch-time disconnection gets exactly one restart attempt. A headless
    // App has no proxy to spawn that worker, so the real path reports
    // Unavailable and clears the latch; repeated turns cannot hot-loop.
    let mut disconnected_app = App::headless_for_test();
    let (sender, receiver) = std::sync::mpsc::sync_channel(1);
    sender
        .try_send(NativeUpdateWorkerRequest::ConfirmBootHealth {
            current_build: 9,
            witness: aterm_update::TrialWitness::Window,
        })
        .unwrap();
    disconnected_app.native_update_reconcile_worker = Some(sender);
    let mut disconnected_state = model.init_state();
    assert!(model.fire("OccupyWorker", &mut disconnected_state));
    let (discard_reply, _discard_result) = std::sync::mpsc::sync_channel(1);
    assert!(disconnected_app.request_native_update_reconcile_with(
        NativeUpdateReconcilePurpose::StageAvailable,
        |_| NativeUpdateFactDestination::Reply(discard_reply),
    ));
    assert!(model.fire("RequestStageFull", &mut disconnected_state));
    drop(receiver);
    assert!(model.fire("DisconnectWithPending", &mut disconnected_state));

    let destinations = std::cell::Cell::new(0_u32);
    let (unavailable_reply, _unavailable_result) = std::sync::mpsc::sync_channel(1);
    assert_eq!(
        disconnected_app.retry_pending_native_update_reconcile_with(|_| {
            destinations.set(destinations.get() + 1);
            NativeUpdateFactDestination::Reply(unavailable_reply)
        }),
        NativeUpdateDispatch::Unavailable
    );
    assert!(model.fire("RestartPendingUnavailable", &mut disconnected_state));
    for _ in 0..32 {
        let (reply, _result) = std::sync::mpsc::sync_channel(1);
        assert_eq!(
            disconnected_app.retry_pending_native_update_reconcile_with(|_| {
                destinations.set(destinations.get() + 1);
                NativeUpdateFactDestination::Reply(reply)
            }),
            NativeUpdateDispatch::Queued
        );
    }
    assert_eq!(destinations.get(), 1);
    assert!(
        disconnected_app
            .pending_native_update_reconcile_purpose
            .is_none()
    );
    assert_eq!(disconnected_state.get("failed_explicitly"), Some(&1));
    assert_eq!(disconnected_state.get("restarts"), Some(&1));

    // Negative control: a "Full means success but retain nothing" projection
    // is rejected by the same invariant.
    let buggy = aterm_spec::interp::with_buggy(&model, 1);
    let occupied = buggy.successors("OccupyWorker", &buggy.init_state())[0].clone();
    let lost = buggy.successors("DropApplyLatchWhenFull", &occupied)[0].clone();
    assert!(!buggy.check_invariant("NoSilentlyLostAcceptedIntent", &lost));

    // Negative control: a restart retried every turn (the hot loop) breaks `RestartAtMostOnce`
    // on the second retry.
    let mut disconnected = model.init_state();
    for action in ["OccupyWorker", "RequestStageFull", "DisconnectWithPending"] {
        assert!(model.fire(action, &mut disconnected), "{action}");
    }
    for _ in 0..2 {
        assert!(buggy.fire("RetryRestartEveryTurn", &mut disconnected));
    }
    assert!(!buggy.check_invariant("RestartAtMostOnce", &disconnected));
}

#[test]
fn boot_health_failure_rearms_bounded_retry_then_success_closes_latch() {
    let mut app = App::headless_for_test();
    let now = std::time::Instant::now();
    app.boot_health_confirmation_dispatched = true;
    app.finish_native_boot_health_confirmation(false, now);
    assert!(!app.boot_health_confirmation_dispatched);
    let first = app
        .boot_health_confirmation_retry_at
        .expect("a failure re-arms a retry");
    assert!(first > now);

    // Consecutive failures back off to a ceiling, each retry queued before it
    // is answered, as `about_to_wait` dispatches it: the retry never runs away.
    for _ in 0..16 {
        app.note_boot_health_dispatch(crate::app_native::NativeUpdateDispatch::Queued, now);
        app.finish_native_boot_health_confirmation(false, now);
    }
    assert!(
        app.boot_health_confirmation_retry_at
            .is_some_and(|at| at > first && at <= now + std::time::Duration::from_secs(60)),
        "the backoff grows and stays bounded"
    );

    app.finish_native_boot_health_confirmation(true, now);
    assert!(app.boot_health_confirmation_dispatched);
    assert!(app.boot_health_confirmation_retry_at.is_none());
    // Success resets the backoff for any later failure.
    app.finish_native_boot_health_confirmation(false, now);
    assert_eq!(app.boot_health_confirmation_retry_at, Some(first));
}

#[test]
fn regional_damage_rejects_theme_or_font_paint_revision_changes() {
    let previous = NativeUiCompileStamp {
        instance: crate::tab_model::AppInstanceId::from_stored(1),
        view: crate::tab_model::ViewId::from_stored(2),
        generation: 7,
        geometry: 11,
        config_revision: 13,
        update_revision: 17,
        document_seq: None,
        presentation_revision: 19,
        paint_revision: 23,
    };
    let mut current = previous;
    current.generation += 1;
    assert!(current.accepts_regional_damage_from(previous));
    current.paint_revision += 1;
    assert!(
        !current.accepts_regional_damage_from(previous),
        "paint input changes promote reducer-local damage to a full raster"
    );
}

#[test]
fn native_appearance_inputs_have_distinct_cache_revisions() {
    let base = crate::native_appearance::AppearancePreferences::default();
    let revision = native_appearance_revision(base);
    for preferences in [
        crate::native_appearance::AppearancePreferences {
            high_contrast: true,
            ..base
        },
        crate::native_appearance::AppearancePreferences {
            reduced_transparency: true,
            ..base
        },
        crate::native_appearance::AppearancePreferences {
            text_scale: 1.25,
            ..base
        },
    ] {
        assert_ne!(native_appearance_revision(preferences), revision);
    }
}

#[test]
fn live_motion_context_changes_the_native_compile_stamp() {
    let mut app = App::headless_for_test();
    let wid = WindowId(0);
    assert!(app.open_settings_tab(crate::native_settings::SettingsRoute::CursorMotion));
    app.windows.get_mut(&wid).unwrap().focused = true;

    let base = app.native_ui_compile_stamp(wid).unwrap();
    app.system_reduce_motion = true;
    let system_reduced = app.native_ui_compile_stamp(wid).unwrap();
    assert_ne!(system_reduced.paint_revision, base.paint_revision);

    app.system_reduce_motion = false;
    app.windows.get_mut(&wid).unwrap().focused = false;
    let unfocused = app.native_ui_compile_stamp(wid).unwrap();
    assert_ne!(unfocused.paint_revision, base.paint_revision);

    app.windows.get_mut(&wid).unwrap().focused = true;
    app.perf_reduced = true;
    let performance_reduced = app.native_ui_compile_stamp(wid).unwrap();
    assert_ne!(performance_reduced.paint_revision, base.paint_revision);

    app.perf_reduced = false;
    app.set_serious_mode(true);
    let serious = app.native_ui_compile_stamp(wid).unwrap();
    assert_ne!(serious.paint_revision, base.paint_revision);
}

#[test]
fn plain_terminal_theme_os_flip_invalidates_auto_window_preview() {
    let mut app = App::headless_for_test();
    let wid = WindowId(0);
    assert!(app.open_settings_tab(crate::native_settings::SettingsRoute::Appearance));
    let theme = app.theme;
    app.os_appearance = aterm_types::Appearance::Light;
    let light = app.native_ui_compile_stamp(wid).unwrap();
    app.os_appearance = aterm_types::Appearance::Dark;
    assert_eq!(
        app.theme.fg, theme.fg,
        "plain terminal foreground stays unchanged"
    );
    assert_eq!(
        app.theme.bg, theme.bg,
        "plain terminal background stays unchanged"
    );
    let dark = app.native_ui_compile_stamp(wid).unwrap();
    assert_ne!(
        light.paint_revision, dark.paint_revision,
        "live OS appearance participates in retained native paint identity"
    );
}

#[test]
fn shared_native_model_edits_invalidate_sibling_presentations() {
    use crate::native_app::{ActionInvocation, AppEvent, DamageRegion, SemanticInput};
    use crate::native_ui::{ActionId, UiKey};
    use crate::system_state::SystemStateAction;
    use crate::workstation_setup::WorkstationSetupDecision;
    use aterm_types::macos_setup::{Plan, Setting};

    let mut app = App::headless_for_test();
    let first = WindowId(0);
    {
        let window = app.windows.get_mut(&first).unwrap();
        window.rows = 90;
        window.cols = 160;
    }
    assert!(app.open_system_state_tab_in_window(first));
    let (instance, first_view) = app.active_native_view(first).unwrap();
    let next_session = app.next_session_id;
    let second = app.insert_logical_window(crate::stub_session(next_session), 90, 160);
    assert!(app.open_system_state_tab_in_window(second));
    let (sibling_instance, second_view) = app.active_native_view(second).unwrap();
    assert_eq!(instance, sibling_instance);
    let initial = app.native_ui_compile_stamp(second).unwrap();

    // Focus and list position remain independent for each linked view.
    for event in [
        AppEvent::FocusChanged(Some(UiKey::new("system-state/url"))),
        AppEvent::ScrollLines(1),
    ] {
        app.dispatch_native_view_event(first, first_view, event)
            .unwrap();
    }
    assert_eq!(app.native_ui_compile_stamp(second).unwrap(), initial);
    assert!(
        app.native_runtime
            .view_state(second_view)
            .unwrap()
            .common()
            .last_focus
            .is_none()
    );
    app.windows
        .get_mut(&second)
        .unwrap()
        .leaf_render_cache
        .entry(second_view)
        .or_default()
        .native_damage = None;

    let mut selected = Plan {
        vault_url: "http://selected.invalid:7484/v1/team".into(),
        ..Plan::default()
    };
    selected.selection.set(Setting::ALL[0], false);
    for (key, value) in [
        ("system-state/setting/0", SemanticInput::Bool(false)),
        (
            "system-state/url",
            SemanticInput::Text(selected.vault_url.clone()),
        ),
    ] {
        let before = app.native_ui_compile_stamp(second).unwrap();
        app.dispatch_native_view_event(
            first,
            first_view,
            AppEvent::Action(ActionInvocation {
                id: ActionId::new(key),
                value: Some(value),
            }),
        )
        .unwrap();
        assert_ne!(
            app.native_ui_compile_stamp(second)
                .unwrap()
                .presentation_revision,
            before.presentation_revision,
            "a linked view must invalidate the old choices before it can submit them"
        );
        assert_eq!(
            app.windows[&second].leaf_render_cache[&second_view].native_damage,
            Some(DamageRegion::All)
        );
    }
    app.dispatch_native_view_event(
        second,
        second_view,
        AppEvent::Action(ActionInvocation {
            id: ActionId::new("system-state/apply"),
            value: None,
        }),
    )
    .unwrap();
    let Some(SystemStateAction::Apply(answer)) = app.native_runtime.take_system_state_action()
    else {
        panic!("the sibling Apply must use the newly presented shared choices");
    };
    assert!(matches!(answer.decision, WorkstationSetupDecision::Begin(plan) if plan == selected));
}

#[test]
fn native_compile_uses_each_window_font_for_its_stamp() {
    let mut app = App::headless_for_test();
    let first = WindowId(0);
    app.windows.get_mut(&first).unwrap().metrics.font_px = 12.0;
    assert!(app.open_settings_tab(crate::native_settings::SettingsRoute::TextFonts));
    let (first_instance, first_view) = app.active_native_view(first).unwrap();
    let first_viewport = app.native_ui_viewport(first).unwrap();

    let next_session = app.next_session_id;
    let (rows, cols) = (app.windows[&first].rows, app.windows[&first].cols);
    let second = app.insert_logical_window(crate::stub_session(next_session), rows, cols);
    {
        let window = app.windows.get_mut(&second).unwrap();
        window.metrics.font_px = 24.0;
        window.scale = 2.0;
    }
    assert!(app.open_settings_tab(crate::native_settings::SettingsRoute::TextFonts));
    let (second_instance, second_view) = app.active_native_view(second).unwrap();
    let second_viewport = app.native_ui_viewport(second).unwrap();
    assert_eq!(
        second_instance, first_instance,
        "the Settings controller remains process-singleton"
    );

    let first_stamp = app
        .native_ui_compile_stamp_for(first, first_instance, first_view, first_viewport)
        .unwrap();
    let second_stamp = app
        .native_ui_compile_stamp_for(second, second_instance, second_view, second_viewport)
        .unwrap();
    assert_ne!(
        first_stamp.paint_revision, second_stamp.paint_revision,
        "window-local font size is a native paint input"
    );

    // Moving the process-global renderer activation must not perturb either
    // window's native stamp; their MetricsView records remain authoritative.
    app.font_px = 31.0;
    assert_eq!(
        app.native_ui_compile_stamp_for(first, first_instance, first_view, first_viewport)
            .unwrap()
            .paint_revision,
        first_stamp.paint_revision
    );
    assert_eq!(
        app.native_ui_compile_stamp_for(second, second_instance, second_view, second_viewport)
            .unwrap()
            .paint_revision,
        second_stamp.paint_revision
    );
}

#[test]
fn native_damage_union_is_bounded_and_all_dominates() {
    assert_eq!(
        union_native_damage(
            DamageRegion::Rect {
                x: 10,
                y: 20,
                width: 8,
                height: 9,
            },
            DamageRegion::Rect {
                x: 4,
                y: 25,
                width: 20,
                height: 10,
            },
        ),
        DamageRegion::Rect {
            x: 4,
            y: 20,
            width: 20,
            height: 15,
        }
    );
    assert_eq!(
        union_native_damage(
            DamageRegion::Rect {
                x: u32::MAX - 2,
                y: u32::MAX - 1,
                width: 20,
                height: 20,
            },
            DamageRegion::Rect {
                x: u32::MAX - 4,
                y: u32::MAX - 3,
                width: 1,
                height: 1,
            },
        ),
        DamageRegion::Rect {
            x: u32::MAX - 4,
            y: u32::MAX - 3,
            width: 4,
            height: 3,
        }
    );
    assert_eq!(
        union_native_damage(
            DamageRegion::Rect {
                x: 1,
                y: 2,
                width: 3,
                height: 4,
            },
            DamageRegion::All,
        ),
        DamageRegion::All
    );
}

fn status(staged_build: Option<u64>, failing_checks: u32) -> DurableUpdateStatus {
    DurableUpdateStatus {
        linux_host: false,
        linux: None,
        enabled: true,
        current_build: 10,
        staged_build,
        staged_version: staged_build.map(|build| format!("1.0.{build}")),
        staged_commit: Some("0123456789abcdef0123456789abcdef01234567".to_string()),
        staged_dmg_sha256: staged_build.map(|_| "ab".repeat(32)),
        changelog: Some("# Release notes".to_string()),
        outcome: if failing_checks == 0 {
            "up to date".to_string()
        } else {
            "network failed".to_string()
        },
        failing_checks,
        failing_persistent: false,
        failing_kind: String::new(),
        failing_applies: 0,
        apply_failure: String::new(),
        apply_failure_build: 0,
        apply_failures_for_target: 0,
        installable: Some(true),
        channel_unreadable: false,
        checked_at: None,
    }
}

fn reconcile_facts(
    request_sequence: u64,
    observation_sequence: u64,
    durable: Option<DurableUpdateStatus>,
) -> NativeUpdateReconcileFacts {
    reconcile_facts_with_installed(request_sequence, observation_sequence, durable, None)
}

fn reconcile_facts_with_installed(
    request_sequence: u64,
    observation_sequence: u64,
    durable: Option<DurableUpdateStatus>,
    installed: Option<InstalledUpdate>,
) -> NativeUpdateReconcileFacts {
    // The read's installability is the ledger's own answer, as production's.
    let installable = durable.as_ref().and_then(|durable| durable.installable);
    NativeUpdateReconcileFacts {
        _ticket: NativeUpdateReconcileTicket { request_sequence },
        observation_sequence,
        observed_at: std::time::Instant::now(),
        durable,
        installed,
        installed_unproven: false,
        installable,
    }
}

/// The sealed `ATermGitCommit` an installed bundle carries is the 12-char
/// short sha, not the manifest's full 40, so the fixture uses one.
fn installed_update(build: u64) -> InstalledUpdate {
    InstalledUpdate {
        build,
        commit: "248091d23ab0".to_string(),
        version: None,
        receipt_build: None,
        receipt_dmg_sha256: None,
        trial_launches: 0,
    }
}

/// The stage the activation lane imports for an installed bundle: the sealed
/// identity under `installed_activation_digest`, and nothing else.
fn assert_activation_stage(app: &App, build: u64, commit: &str) {
    let staged =
        app.native_updater_service.snapshot().staged.clone().expect(
            "an installed bundle newer than the process is imported as an activation stage",
        );
    assert_eq!(
        staged.build, build,
        "the activation names the installed build"
    );
    assert_eq!(
        staged.commit.as_deref(),
        Some(commit),
        "…and its sealed commit"
    );
    assert!(
        staged.is_installed_activation(),
        "…under the activation identity, not any DMG digest: {}",
        staged.dmg_sha256
    );
    assert_eq!(
        staged.dmg_sha256,
        crate::native_updater_service::installed_activation_digest(build, commit)
    );
}

/// The activation lane: an on-disk bundle already at the staged build while the process runs the
/// older image is imported as an activation stage, the bundle under the executable as the
/// artifact. Returned-apply arms carrying disk facts (`finish_async_native_update_handoff` with
/// `reconcile: Some`, which shipping sites leave `None`) reduce through the arming door, so an
/// activation our own child installed is armed, not left idle.
#[test]
fn a_returned_apply_whose_child_swapped_the_bundle_arms_the_activation() {
    let mut app = App::headless_for_test();
    let (_, _settings) = park_a_settings_draft_in_a_background_tab(&mut app);
    let build = app.native_updater_service.snapshot().current_build + 1;
    stage_one_build_for_test(&mut app, build);
    let ApplyPreflightStart::Inspect(preflight) =
        app.native_updater_service.begin_apply_preflight()
    else {
        panic!("the stage must admit an apply preflight");
    };
    let ApplyDecision::Execute(command) = app
        .native_updater_service
        .finish_apply_preflight(preflight, ClosePreflight::Ready)
    else {
        panic!("a ready close preflight must authorize the replacement");
    };
    let attempt = command.attempt();
    command.execute(|| ());
    app.auto_apply_intent = None;
    // The child swapped the bundle (installed == target, receipt names the
    // download) and then failed to prove readiness.
    let facts = reconcile_facts_with_installed(
        7,
        7,
        Some(durable_download_status(&app, build)),
        Some(InstalledUpdate {
            build,
            commit: PREFLIGHT_TEST_COMMIT.to_string(),
            version: None,
            receipt_build: Some(build),
            receipt_dmg_sha256: Some("ab".repeat(32)),
            trial_launches: 0,
        }),
    );
    let outcome = app.finish_async_native_update_handoff(
        attempt,
        facts,
        "overlap handoff failed safely: handoff proof ended TimedOut".to_string(),
        HandoffFailureLane::Physical(PhysicalFailureShape::Transient),
    );
    assert!(
        matches!(outcome, Some(UpdateOutcome::InstalledNeedsRelaunch { .. })),
        "the disposition names the installed build, got {outcome:?}"
    );
    assert_activation_stage(&app, build, PREFLIGHT_TEST_COMMIT);
    let staged = app
        .native_updater_service
        .snapshot()
        .staged
        .clone()
        .unwrap();
    let activation = decode_dmg_sha256(&staged.dmg_sha256).unwrap();
    // The failure is charged and its latch guards the activation, so the
    // activation does not arm at once but waits its schedule's deadline.
    assert_eq!(
        app.auto_apply_physical_retry.map(|retry| retry.cycles),
        Some(1),
        "the returned physical failure spends the shared budget"
    );
    let latch = app
        .auto_apply_manual_only
        .expect("the failure latches the automatic lane");
    assert_eq!(
        (latch.build, latch.dmg_sha256, latch.activation),
        (build, activation, true),
        "…keyed to the activation the reduction just imported"
    );
    assert!(
        latch.retry_at.is_some(),
        "a transient failure has a deadline"
    );
    assert!(
        app.auto_apply_intent.is_none(),
        "the activation does not arm inside the failure's own latch"
    );
    // …and it is armed, not merely described, the moment that latch lapses.
    app.auto_apply_manual_only = Some(crate::AutoApplyManualOnly {
        retry_at: Some(std::time::Instant::now() - std::time::Duration::from_secs(1)),
        ..latch
    });
    app.rearm_native_auto_apply_after_lapse();
    assert!(
        app.auto_apply_intent
            .is_some_and(|intent| intent.build == build && intent.dmg_sha256 == activation),
        "the activation the returned facts imported is ARMED once the latch lapses"
    );
}

#[test]
fn an_unknown_probe_after_returned_activation_keeps_the_bounded_retry() {
    let _ledger = crate::app_update_screen::hold_update_ledger_for_test();
    let mut app = App::headless_for_test();
    let build = app.native_updater_service.snapshot().current_build + 1;
    let installed = installed_update(build);
    let _ = app.reconcile_native_update_facts(reconcile_facts_with_installed(
        1,
        1,
        Some(status(None, 0)),
        Some(installed.clone()),
    ));
    assert_activation_stage(&app, build, &installed.commit);
    let ApplyPreflightStart::Inspect(preflight) =
        app.native_updater_service.begin_apply_preflight()
    else {
        panic!("activation must admit preflight");
    };
    let ApplyDecision::Execute(command) = app
        .native_updater_service
        .finish_apply_preflight(preflight, ClosePreflight::Ready)
    else {
        panic!("ready activation must issue an apply ticket");
    };
    let attempt = command.attempt();
    command.execute(|| ());
    app.auto_apply_intent = None;
    let outcome = app.finish_async_native_update_handoff(
        attempt.clone(),
        reconcile_facts(2, 2, Some(status(None, 0))),
        "candidate timed out and installed-bundle verification was inconclusive".to_string(),
        HandoffFailureLane::Physical(PhysicalFailureShape::Transient),
    );
    assert!(matches!(outcome, Some(UpdateOutcome::Failed { .. })));
    assert_activation_stage(&app, build, &installed.commit);
    let retry = app
        .auto_apply_manual_only
        .expect("failure schedules its retry");
    assert_eq!(retry.build, build);
    assert!(
        retry
            .retry_at
            .is_some_and(|at| at > std::time::Instant::now())
    );
    assert!(app.automatic_apply_retry_scheduled(build));
    assert!(
        app.auto_apply_intent.is_none(),
        "the cooldown must not be bypassed"
    );

    let budget = app.auto_apply_physical_retry;
    assert!(
        budget.is_some(),
        "the real completion must charge the physical budget"
    );
    assert!(
        app.finish_async_native_update_handoff(
            attempt,
            reconcile_facts(3, 3, Some(status(None, 0))),
            "duplicate completion".to_string(),
            HandoffFailureLane::Physical(PhysicalFailureShape::Transient),
        )
        .is_none()
    );
    assert_eq!(app.auto_apply_physical_retry, budget);
    assert_eq!(app.auto_apply_manual_only, Some(retry));
}

#[test]
fn a_freshly_imported_stage_raises_no_card_and_no_glow_and_a_repeat_import_is_quiet() {
    let mut app = App::headless_for_test();
    let running = app.native_updater_service.snapshot().current_build;
    let build = running + 1;
    assert!(app.level_up.is_none() && app.relaunch.is_none());
    let rows_before = app.messages.live_rows().count();
    let durable = durable_download_status(&app, build);
    let facts = || {
        reconcile_facts_with_installed(1, 1, Some(durable.clone()), Some(installed_update(running)))
    };
    app.finish_native_update_reconcile(NativeUpdateReconcilePurpose::StageAvailable, facts());
    assert_eq!(
        app.native_updater_service
            .snapshot()
            .staged
            .as_ref()
            .map(|s| s.build),
        Some(build),
        "PRECONDITION: the stage imported"
    );
    // The announcement is the band's staged row; nothing floats and no border
    // glow is raised (the glow fires when the apply runs).
    let rows = |app: &App| {
        app.messages
            .live_rows()
            .map(|l| (l.id, l.msg.title.clone()))
            .collect::<Vec<_>>()
    };
    let announced = rows(&app);
    assert!(
        announced.len() <= rows_before + 1,
        "one staged row at most: {announced:?}"
    );
    assert!(app.level_up.is_none(), "…and no stage-time glow");
    // The same stage again is not news.
    app.level_up = None;
    let mut again = facts();
    again.observation_sequence = 2;
    again._ticket = NativeUpdateReconcileTicket {
        request_sequence: 2,
    };
    app.finish_native_update_reconcile(NativeUpdateReconcilePurpose::StageAvailable, again);
    assert!(
        rows(&app) == announced && app.level_up.is_none(),
        "a repeat import is quiet"
    );
}

/// A copy's Linux facts holding `staged` downloaded and unapplied, as the facts
/// worker reads them on a Linux host.
fn linux_status_holding(running: u64, staged: Option<u64>) -> DurableUpdateStatus {
    let mut durable = status(None, 0);
    durable.current_build = running;
    durable.linux_host = true;
    durable.linux = Some(aterm_update::LinuxUpdateStatus {
        installed_build: running,
        installed_version: format!("1.0.{running}"),
        staged_build: staged,
        staged_version: staged.map(|build| format!("1.0.{build}")),
        staged_commit: staged.map(|_| "0123456789abcdef0123456789abcdef01234567".to_string()),
        trial_phase: None,
        trial_starts: 0,
        trial_healthy: false,
        refused_newer: false,
    });
    durable
}

/// A LINUX BUILD HELD FOR A PERSON IS ON THE BAND (ATERM-KI-1015-2, U21). With
/// automatic install off, a Linux check downloads a newer build and holds it for
/// `aterm update apply`. Settings ▸ Software Update showed it once re-read; the
/// band never did, since the macOS apply authority takes no Linux stage
/// (`staged_from_status`) and no Linux row stood in. The held build is said once
/// per build — found by the loop's pass or at launch — and its row leaves with the
/// stage. Negative control: a reduction holding nothing says nothing.
#[test]
fn a_linux_build_held_for_a_person_is_said_once_and_leaves_with_its_stage() {
    let mut app = App::headless_for_test();
    let running = app.native_updater_service.snapshot().current_build;
    let held = |app: &App| {
        app.messages
            .live_by_key(crate::update_words::KEY_HELD)
            .map(|l| (l.msg.title.clone(), l.msg.detail.join(" ")))
    };
    let mut sequence = 0;
    let mut reduce = |app: &mut App, purpose, staged| {
        sequence += 1;
        app.finish_native_update_reconcile(
            purpose,
            reconcile_facts(
                sequence,
                sequence,
                Some(linux_status_holding(running, staged)),
            ),
        );
    };
    reduce(&mut app, NativeUpdateReconcilePurpose::Startup, None);
    assert_eq!(held(&app), None, "nothing held, nothing said");
    let build = running + 2;
    reduce(
        &mut app,
        NativeUpdateReconcilePurpose::StageAvailable,
        Some(build),
    );
    let (title, detail) = held(&app).expect("the held build is on the band");
    assert!(title.contains(&format!("1.0.{build}")), "{title}");
    assert!(detail.contains("aterm update apply"), "{detail}");
    assert!(
        app.native_updater_service.snapshot().staged.is_none(),
        "PRECONDITION: no macOS apply authority holds it"
    );
    let id = app
        .messages
        .live_by_key(crate::update_words::KEY_HELD)
        .unwrap()
        .id;
    app.withdraw_message(id);
    reduce(
        &mut app,
        NativeUpdateReconcilePurpose::StageAvailable,
        Some(build),
    );
    assert_eq!(held(&app), None, "said once per build");
    reduce(
        &mut app,
        NativeUpdateReconcilePurpose::StageAvailable,
        Some(build + 1),
    );
    assert!(held(&app).is_some(), "a newer held build is said");
    reduce(&mut app, NativeUpdateReconcilePurpose::StageAvailable, None);
    assert_eq!(held(&app), None, "its row leaves with the stage");
}

#[test]
fn a_bundle_newer_than_the_process_becomes_an_activation_stage_and_stays_one() {
    let mut app = App::headless_for_test();
    let running = app.native_updater_service.snapshot().current_build;
    assert!(running < 12, "fixture must model a newer installed build");
    let mut installed = installed_update(12);
    installed.version = Some("1.0.12".to_string());
    let _ = app.reconcile_native_update_facts(reconcile_facts_with_installed(
        1,
        1,
        Some(status(Some(12), 0)),
        Some(installed.clone()),
    ));
    assert_activation_stage(&app, 12, &installed.commit);

    // Stable: the same facts again keep the same activation stage (no retire,
    // no re-import churn that would reset the automatic intent every pass).
    let before = app.native_updater_service.snapshot().staged.clone();
    let _ = app.reconcile_native_update_facts(reconcile_facts_with_installed(
        2,
        2,
        Some(status(Some(12), 0)),
        Some(installed.clone()),
    ));
    assert_eq!(
        app.native_updater_service.snapshot().staged,
        before,
        "repeating the reconcile must not disturb the activation stage"
    );

    // It retires when the bundle moves on: a bundle no longer newer (rolled
    // back under us) drops the activation; the next observation imports what
    // is really there.
    let _ = app.reconcile_native_update_facts(reconcile_facts_with_installed(
        3,
        3,
        Some(status(None, 0)),
        Some(installed_update(running)),
    ));
    assert!(
        app.native_updater_service.snapshot().staged.is_none(),
        "an activation whose bundle is no longer newer must retire"
    );
}

#[test]
fn observation_order_prevents_old_or_missing_read_from_downgrading_stage() {
    let mut missing_then_new = App::headless_for_test();
    let _ = missing_then_new.reconcile_native_update_facts(reconcile_facts(2, 1, None));
    let _ = missing_then_new.reconcile_native_update_facts(reconcile_facts(
        1,
        2,
        Some(status(Some(12), 0)),
    ));
    assert_eq!(
        missing_then_new
            .native_updater_service
            .snapshot()
            .staged
            .as_ref()
            .map(|stage| stage.build),
        Some(12)
    );

    let mut completion_reordered = App::headless_for_test();
    let _ = completion_reordered.reconcile_native_update_facts(reconcile_facts(
        1,
        2,
        Some(status(Some(12), 0)),
    ));
    assert!(matches!(
        completion_reordered.reconcile_native_update_facts(reconcile_facts(2, 1, None)),
        NativeUpdateFactsResult::IgnoredStale
    ));
    assert_eq!(
        completion_reordered
            .native_updater_service
            .snapshot()
            .staged
            .as_ref()
            .map(|stage| stage.build),
        Some(12),
        "older observation completion cannot retire the newer imported stage"
    );
}

#[cfg(unix)]
#[test]
fn installed_environment_repair_conforms_and_preserves_unrelated_budgets() {
    use crate::update_apply_trouble::ApplyRetry;
    let _ledger = crate::app_update_screen::hold_update_ledger_for_test();
    let model = aterm_models_update::native_update_environment_repair_model();
    // Good, stale, unknown, wrong build, wrong commit, unrelated latch, newer cache, auto
    // opt-out, an ordinary exhausted budget, a verified but noninstallable (e.g. dev-marked)
    // source, and a folder this user cannot write under a copy that verifies: the installed
    // copy's observation is not the model's `Verify` for a folder (that bind is
    // `a_folder_that_took_the_write_since_it_refused_releases_its_block`).
    for case in 0..11 {
        let mut app = App::headless_for_test();
        app.native_updater_service = NativeUpdaterService::new(10, "test", true);
        stage_one_build_for_test(&mut app, 11);
        let (reason, cause) = if case == 10 {
            (
                "install location not writable: /Applications/aterm.app; this install \
                 cannot update itself",
                EnvironmentBlockCause::InstallLocation,
            )
        } else {
            (
                "the installed bundle cannot be the rollback source",
                EnvironmentBlockCause::InstalledSource,
            )
        };
        app.block_native_auto_apply_environment(11, [0xab; 32], cause);
        let blocked_at = app.auto_apply_environment_block.unwrap().blocked_at;
        app.native_updater_service
            .note_apply_failure(&aterm_update::RecordedApplyFailure {
                apply_failures: 1,
                failures_for_target: 1,
                target_build: 11,
                reason: reason.to_string(),
                persistent: false,
            });
        assert_eq!(
            app.apply_retry_for(Some(11)),
            ApplyRetry::NeedsPerson(cause.remedy())
        );
        let cache = std::sync::Arc::clone(&app.handoff_preverified);
        *cache.lock().unwrap() = Some(crate::HandoffPreverification {
            build: 11,
            commit: PREFLIGHT_TEST_COMMIT.to_string(),
            artifact: "ab".repeat(32),
            at: if case == 6 {
                blocked_at + std::time::Duration::from_secs(2)
            } else {
                blocked_at
            },
            passed: false,
            reason: Some(reason.to_string()),
            grant_chunks: false,
            policy: None,
        });
        if case == 5 {
            app.auto_apply_manual_only.as_mut().unwrap().dmg_sha256 = [0xcd; 32];
        }
        if case == 8 {
            app.auto_apply_environment_block = None;
        }
        if case == 7 {
            app.config.update = Some(crate::app_config::UpdateConfig {
                auto_apply: Some(false),
                ..Default::default()
            });
        }
        app.auto_apply_physical_retry = Some(crate::AutoOverlapRetry {
            build: 12,
            dmg_sha256: [0xcd; 32],
            activation: false,
            cycles: PHYSICAL_FAILURE_LIFETIME_ATTEMPTS,
            last_attempt: blocked_at,
        });
        let budget = app.auto_apply_physical_retry;
        let owned = !matches!(case, 5 | 8);
        let fresh = !matches!(case, 1 | 6);
        let verified = !matches!(case, 2..=4 | 9 | 10);
        let mut durable = status(Some(11), 0);
        durable.failing_applies = 1;
        durable.apply_failure_build = 11;
        durable.apply_failures_for_target = 1;
        durable.apply_failure = reason.to_string();
        durable.installable = Some(case != 9);
        let mut facts = reconcile_facts_with_installed(
            1,
            1,
            Some(durable),
            (case != 2).then(|| InstalledUpdate {
                build: if case == 3 { 9 } else { 10 },
                commit: if case == 4 {
                    "f".repeat(40)
                } else {
                    PREFLIGHT_TEST_COMMIT.to_string()
                },
                version: None,
                receipt_build: None,
                receipt_dmg_sha256: None,
                trial_launches: 0,
            }),
        );
        facts.observed_at = if case == 1 {
            blocked_at - std::time::Duration::from_millis(1)
        } else {
            blocked_at + std::time::Duration::from_secs(1)
        };
        let mut before = model.init_state();
        for (guard, action) in [(owned, "Own"), (fresh, "Fresh"), (verified, "Verify")] {
            if guard {
                before = model.successors(action, &before).remove(0);
            }
        }
        if case == 0 {
            facts.durable.as_mut().unwrap().enabled = false;
            app.release_repaired_auto_apply_environment_for_commit(&facts, PREFLIGHT_TEST_COMMIT);
            assert!(
                app.auto_apply_manual_only.is_some(),
                "disabled updater cannot release a repair latch"
            );
            facts.durable.as_mut().unwrap().enabled = true;
            app.release_repaired_auto_apply_environment_for_commit(
                &facts,
                &format!("{PREFLIGHT_TEST_COMMIT}-dirty"),
            );
            assert!(
                app.auto_apply_manual_only.is_some(),
                "a dirty running identity cannot prove a signed-source repair"
            );
            let held = cache.lock().unwrap();
            app.release_repaired_auto_apply_environment_for_commit(&facts, PREFLIGHT_TEST_COMMIT);
            assert!(
                app.auto_apply_manual_only.is_some(),
                "cache contention must not wait or release"
            );
            drop(held);
        }
        // A dirty test binary has no verifiable committed identity, so it supplies a clean one;
        // shipping passes `build_info::git_commit()` and keeps its dirty-identity refusal.
        app.release_repaired_auto_apply_environment_for_commit(&facts, PREFLIGHT_TEST_COMMIT);
        let mut after = before.clone();
        after.insert("decided", 1);
        after.insert("latched", i64::from(app.auto_apply_manual_only.is_some()));
        assert_eq!(
            model.successors("Reduce", &before),
            vec![after.clone()],
            "case {case}"
        );
        assert_eq!(app.auto_apply_physical_retry, budget);
        if owned && fresh && verified {
            assert!(
                cache.lock().unwrap().is_none(),
                "old denial is invalidated, never forged into success"
            );
            // Substitute the next full preverification result, avoiding any
            // OS verifier in this policy fixture, then drive real arming.
            *cache.lock().unwrap() = Some(crate::HandoffPreverification {
                build: 11,
                commit: PREFLIGHT_TEST_COMMIT.to_string(),
                artifact: "ab".repeat(32),
                at: std::time::Instant::now(),
                passed: true,
                reason: None,
                policy: None,
                grant_chunks: false,
            });
            assert_eq!(app.arm_native_auto_apply(11, &"ab".repeat(32)), case != 7);
            assert_eq!(
                app.apply_retry_for(Some(11)),
                if case == 7 {
                    ApplyRetry::ManualOnly
                } else {
                    ApplyRetry::Scheduled
                }
            );
            assert_eq!(app.native_updater_service.snapshot().apply_failure, reason);
            // A permanent latch fails this recovery law.
            after.insert("latched", 1);
            assert!(!model.check_invariant("RepairedSourceRecovers", &after));
        }
    }
}

/// A non-writable install location is the person's. A standard user runs a signed aterm.app from
/// an admin-owned /Applications: the pre-park check refuses with the install location's refusal,
/// and the synchronous arm's automatic attempt books it as the environment block a person must
/// clear, so nothing re-arms the attempt the folder refuses. (The block holding through a
/// reconcile whose installed copy verifies is case 10 of
/// `installed_environment_repair_conforms_and_preserves_unrelated_budgets`; what releases the
/// folder's block is `a_folder_that_took_the_write_since_it_refused_releases_its_block`.)
// `aterm_update::refusal_needs_person` is macOS's: no other lane has a
// refusal only a person can clear.
#[cfg(target_os = "macos")]
#[test]
fn a_non_writable_install_location_is_booked_as_the_persons_at_the_arm() {
    use crate::update_apply_trouble::{ApplyRetry, PersonRemedy};
    let _ledger = crate::app_update_screen::hold_update_ledger_for_test();
    let reason = "install location not writable: /Applications/aterm.app; this install \
                  cannot update itself: move aterm.app to a folder you can write to (such \
                  as ~/Applications), or have an administrator install the update";
    let mut app = App::headless_for_test();
    app.native_updater_service = NativeUpdaterService::new(10, "test", true);
    stage_one_build_for_test(&mut app, 11);
    *app.handoff_preverified.lock().unwrap() = Some(crate::HandoffPreverification {
        build: 11,
        commit: PREFLIGHT_TEST_COMMIT.to_string(),
        artifact: "ab".repeat(32),
        at: std::time::Instant::now(),
        passed: false,
        reason: Some(format!(
            "staged update failed pre-park verification: {reason}"
        )),
        grant_chunks: false,
        policy: None,
    });
    // The attempt, as the synchronous arm meets it: the cached verdict
    // answers it (`cold_lane_verdict_gate`), and the arm books it by that
    // verdict.
    let ticket = crate::native_updater_service::ApplyAttemptTicket::for_test(
        11,
        PREFLIGHT_TEST_COMMIT,
        &"ab".repeat(32),
    );
    let message = app
        .cold_lane_verdict_gate(ApplyMode::AutomaticPastGrace, Some(&ticket))
        .expect_err("the cached refusal answers the automatic attempt")
        .to_string();
    let cached = app
        .cached_handoff_refusal_reason(11)
        .expect("the arm's one typed fact");
    assert!(crate::update_apply_trouble::ApplyTrouble::needs_person(
        &cached
    ));
    app.auto_apply_intent = None;
    app.suspend_automatic_apply_for_person(11, [0xab; 32], &cached, message);
    assert_eq!(
        app.apply_retry_for(Some(11)),
        ApplyRetry::NeedsPerson(PersonRemedy::MoveOrAskAdmin)
    );
    assert!(app.auto_apply_environment_blocked(11));
    assert!(
        !app.arm_native_auto_apply(11, &"ab".repeat(32)),
        "nothing re-arms the attempt the folder refuses"
    );
    assert!(app.auto_apply_intent.is_none());
}

/// A folder that took the swap's write since it refused releases its block: a person's retry
/// that passed the pre-park check (which asks the folder for the swap's write) and then failed for
/// an ordinary reason (a successor that died) proves the folder, so the reconcile that follows
/// releases the block. The Tier-1 bind of `NativeUpdateEnvironmentRepair`'s folder case: the block
/// owns its latch (`Own`), the pass was published after the block (`Fresh`), and it is a pass of
/// exactly the blocked artifact by the check that asks the folder (`Verify`). Negative controls
/// (cases 1–5 below, and no new verdict in case 10 of
/// `installed_environment_repair_conforms_and_preserves_unrelated_budgets`) keep the block.
#[cfg(unix)]
#[test]
fn a_folder_that_took_the_write_since_it_refused_releases_its_block() {
    use crate::update_apply_trouble::{ApplyRetry, PersonRemedy};
    let _ledger = crate::app_update_screen::hold_update_ledger_for_test();
    let model = aterm_models_update::native_update_environment_repair_model();
    let location = "staged update failed pre-park verification: install location not \
                    writable: /Applications/aterm.app; this install cannot update itself: \
                    move aterm.app to a folder you can write to (such as ~/Applications), \
                    or have an administrator install the update";
    let died = "overlap handoff failed safely: handoff proof ended ChildDied";
    let running = InstalledUpdate {
        build: 10,
        commit: PREFLIGHT_TEST_COMMIT.to_string(),
        version: None,
        receipt_build: None,
        receipt_dmg_sha256: None,
        trial_launches: 0,
    };
    // 0 the person's retry passed the check, then its successor died; 1 the
    // retry met the folder's refusal again; 2 the pass on record is no later
    // than the block; 3 a pass of another artifact; 4 the block no longer
    // owns its latch; 5 the stage is an activation, whose check never asks
    // the folder.
    for case in 0..6 {
        let mut app = App::headless_for_test();
        app.native_updater_service = NativeUpdaterService::new(10, "test", true);
        let installed = installed_update(11);
        let (commit, artifact) = if case == 5 {
            let _ = app.reconcile_native_update_facts(reconcile_facts_with_installed(
                1,
                1,
                Some(status(None, 0)),
                Some(installed.clone()),
            ));
            assert_activation_stage(&app, 11, &installed.commit);
            (
                installed.commit.clone(),
                crate::native_updater_service::installed_activation_digest(11, &installed.commit),
            )
        } else {
            stage_one_build_for_test(&mut app, 11);
            (PREFLIGHT_TEST_COMMIT.to_string(), "ab".repeat(32))
        };
        let digest = decode_dmg_sha256(&artifact).expect("a stage's digest decodes");
        let verdict = |at: std::time::Instant, artifact: &str, passed: bool| {
            Some(crate::HandoffPreverification {
                build: 11,
                commit: commit.clone(),
                artifact: artifact.to_string(),
                at,
                passed,
                reason: (!passed).then(|| location.to_string()),
                grant_chunks: false,
                policy: None,
            })
        };
        // The folder refused (as `publish_refusal` keeps the worker's
        // verdict), the automatic lane took the person's block, and the
        // ledger booked the refusal.
        *app.handoff_preverified.lock().unwrap() =
            verdict(std::time::Instant::now(), &artifact, false);
        app.auto_apply_intent = None;
        app.block_native_auto_apply_environment(11, digest, EnvironmentBlockCause::InstallLocation);
        let blocked_at = app.auto_apply_environment_block.unwrap().blocked_at;
        app.native_updater_service
            .note_apply_failure(&aterm_update::RecordedApplyFailure {
                apply_failures: 1,
                failures_for_target: 1,
                target_build: 11,
                reason: location.to_string(),
                persistent: false,
            });
        assert_eq!(
            app.apply_retry_for(Some(11)),
            ApplyRetry::NeedsPerson(PersonRemedy::MoveOrAskAdmin)
        );
        // The person's retry, as the worker's pre-park check keeps its
        // verdict (`publish_pass`, or `publish_refusal` for case 1), and the
        // ledger's booking of how that attempt ended.
        let at = if case == 2 {
            blocked_at
        } else {
            blocked_at + std::time::Duration::from_secs(2)
        };
        let verified_artifact = if case == 3 {
            "cd".repeat(32)
        } else {
            artifact.clone()
        };
        *app.handoff_preverified.lock().unwrap() = verdict(at, &verified_artifact, case != 1);
        if case == 4 {
            app.auto_apply_manual_only.as_mut().unwrap().dmg_sha256 = [0xcd; 32];
        }
        let booked = if case == 1 { location } else { died };
        app.native_updater_service
            .note_apply_failure(&aterm_update::RecordedApplyFailure {
                apply_failures: 2,
                failures_for_target: 2,
                target_build: 11,
                reason: booked.to_string(),
                persistent: false,
            });
        let owned = case != 4;
        let fresh = case != 2;
        let verified = !matches!(case, 1 | 3 | 5);
        let mut before = model.init_state();
        for (guard, action) in [(owned, "Own"), (fresh, "Fresh"), (verified, "Verify")] {
            if guard {
                before = model.successors(action, &before).remove(0);
            }
        }
        // The reconcile the worker posts once a returned attempt is reaped
        // (`send_post_failure_reconcile_facts`): case 0 through the production entry, the
        // controls through the reduction it makes.
        let mut durable = if case == 5 {
            status(None, 0)
        } else {
            durable_download_status(&app, 11)
        };
        durable.failing_applies = 2;
        durable.apply_failure = booked.to_string();
        durable.apply_failure_build = 11;
        durable.apply_failures_for_target = 2;
        let facts = |durable: DurableUpdateStatus| {
            let mut facts = reconcile_facts_with_installed(
                2,
                2,
                Some(durable),
                Some(if case == 5 {
                    installed.clone()
                } else {
                    running.clone()
                }),
            );
            facts.observed_at = blocked_at + std::time::Duration::from_secs(3);
            facts
        };
        if case == 0 {
            let _ = app.reconcile_native_update_facts(facts(durable));
        } else {
            app.release_repaired_auto_apply_environment_for_commit(
                &facts(durable),
                PREFLIGHT_TEST_COMMIT,
            );
        }
        let mut after = before.clone();
        after.insert("decided", 1);
        after.insert("latched", i64::from(app.auto_apply_manual_only.is_some()));
        assert_eq!(
            model.successors("Reduce", &before),
            vec![after.clone()],
            "case {case}"
        );
        if !(owned && fresh && verified) {
            if owned {
                assert!(
                    app.auto_apply_environment_blocked(11),
                    "case {case}: nothing said the folder took the write"
                );
                assert_eq!(
                    app.apply_retry_for(Some(11)),
                    ApplyRetry::NeedsPerson(PersonRemedy::MoveOrAskAdmin)
                );
                assert!(!app.arm_native_auto_apply(11, &artifact));
            }
            continue;
        }
        assert!(
            !app.auto_apply_environment_blocked(11),
            "the folder took the swap's write after it refused: the block is released"
        );
        assert!(
            !matches!(app.apply_retry_for(Some(11)), ApplyRetry::NeedsPerson(_)),
            "nothing is left for the person to do"
        );
        assert!(
            app.handoff_preverified
                .lock()
                .unwrap()
                .as_ref()
                .is_some_and(|entry| entry.passed),
            "the pass is a verdict, kept for the attempt it re-arms"
        );
        assert!(
            app.arm_native_auto_apply(11, &artifact),
            "the automatic lane takes build 11 up again"
        );
        assert_eq!(app.apply_retry_for(Some(11)), ApplyRetry::Scheduled);
        // A block only a landed retry releases fails the model's recovery invariant.
        after.insert("latched", 1);
        assert!(!model.check_invariant("RepairedSourceRecovers", &after));
    }
}

/// An attempt that outran the arm-time read and met the folder's refusal is the person's, not the
/// build's. The launched lane's worker verifies for itself when no verdict is cached and keeps its
/// verdict where the arm-time read does (`PreverifyPublisher::publish_refusal`); the returned
/// attempt is booked by it as the synchronous arm books it: the environment block, no structural
/// budget spent. Negative control: a structural refusal no person can clear still spends the
/// budget and takes the physical latch.
#[cfg(target_os = "macos")]
#[test]
fn a_returned_attempt_the_folder_refused_is_the_persons_not_the_builds() {
    use crate::update_apply_trouble::{ApplyRetry, PersonRemedy};
    let _ledger = crate::app_update_screen::hold_update_ledger_for_test();
    let ticket = crate::native_updater_service::ApplyAttemptTicket::for_test(
        11,
        PREFLIGHT_TEST_COMMIT,
        &"ab".repeat(32),
    );
    for (reason, person) in [
        (
            "staged update failed pre-park verification: install location not writable: \
             /Applications/aterm.app; this install cannot update itself",
            true,
        ),
        (
            "staged update failed pre-park verification: codesign --verify (structural) \
             failed",
            false,
        ),
    ] {
        let mut app = App::headless_for_test();
        app.native_updater_service = NativeUpdaterService::new(10, "test", true);
        stage_one_build_for_test(&mut app, 11);
        // What the worker kept, as `publish_refusal` keeps it.
        *app.handoff_preverified.lock().unwrap() = Some(crate::HandoffPreverification {
            build: 11,
            commit: PREFLIGHT_TEST_COMMIT.to_string(),
            artifact: "ab".repeat(32),
            at: std::time::Instant::now(),
            passed: false,
            reason: Some(reason.to_string()),
            grant_chunks: false,
            policy: None,
        });
        let outcome = app.settle_rearmed_apply_failure(
            &ticket,
            format!("overlap handoff failed safely: {reason}"),
            HandoffFailureLane::Physical(PhysicalFailureShape::Structural),
            None,
        );
        assert!(matches!(outcome, UpdateOutcome::Failed { .. }));
        assert_eq!(
            app.auto_apply_environment_blocked(11),
            person,
            "booked by the verifier's word: {reason}"
        );
        assert!(app.auto_apply_intent.is_none());
        if person {
            assert_eq!(
                app.apply_retry_for(Some(11)),
                ApplyRetry::NeedsPerson(PersonRemedy::MoveOrAskAdmin)
            );
            assert!(
                app.auto_apply_physical_retry.is_none()
                    && app.auto_apply_structural_verdict.is_none(),
                "no structural budget is spent on the person's refusal"
            );
        } else {
            assert!(
                app.auto_apply_physical_retry.is_some(),
                "NEGATIVE: a verdict on the build spends its budget"
            );
            assert!(
                app.auto_apply_manual_only
                    .is_some_and(|latch| latch.retry_at.is_some()),
                "…and takes the physical latch, which lapses"
            );
        }
    }
}

/// A standing block names its own remedy over a later booking. With the folder's block standing
/// for build 11, a press fails on a passing refusal (the staged `codesign` past its budget) before
/// the folder is asked, and the ledger books that reason. The block holds, and the remedy is the
/// block's, not the ledger's latest reason. Control: the installed copy's block over the same
/// booking keeps its own remedy.
// The bundle apply lane and its ledger (`aterm_update::apply_lane_report`) are
// macOS's: a Linux update replaces the executable on disk and hands nothing off.
#[cfg(target_os = "macos")]
#[test]
fn a_standing_block_names_its_own_remedy_over_a_later_booking() {
    use crate::update_apply_trouble::{ApplyRetry, PersonRemedy};
    let _ledger = crate::app_update_screen::hold_update_ledger_for_test();
    let passing = "overlap handoff failed safely: staged update failed pre-park \
                   verification: codesign --verify (staged) ran past this apply's \
                   verification budget; treating as a rejection (a passing condition, not \
                   a verdict on the update)";
    assert!(aterm_update::is_passing_refusal(passing));
    for (cause, refusal, remedy) in [
        (
            EnvironmentBlockCause::InstallLocation,
            "staged update failed pre-park verification: install location not writable: \
             /Applications/aterm.app; this install cannot update itself",
            PersonRemedy::MoveOrAskAdmin,
        ),
        (
            EnvironmentBlockCause::InstalledSource,
            "installed bundle failed pre-park verification: the installed bundle at \
             /Applications/aterm.app cannot be the rollback source the swap installs: \
             codesign refused it",
            PersonRemedy::InstallSignedRelease,
        ),
    ] {
        let mut app = App::headless_for_test();
        app.native_updater_service = NativeUpdaterService::new(10, "test", true);
        stage_one_build_for_test(&mut app, 11);
        // The block, as the automatic attempt took it, and its booking.
        app.auto_apply_intent = None;
        app.block_native_auto_apply_environment(11, [0xab; 32], cause);
        app.native_updater_service
            .note_apply_failure(&aterm_update::RecordedApplyFailure {
                apply_failures: 1,
                failures_for_target: 1,
                target_build: 11,
                reason: refusal.to_string(),
                persistent: false,
            });
        // The person's press, failing for a passing reason before the
        // refusal was reached, booked through the production write.
        app.surface_update_apply_outcome(
            "control request",
            UpdateOutcome::Failed {
                message: passing.to_string(),
            },
            false,
        );
        assert_eq!(
            app.native_updater_service.snapshot().apply_failure,
            passing,
            "PRECONDITION: the ledger's latest reason is the passing one"
        );
        assert!(
            app.auto_apply_environment_blocked(11),
            "nothing answered what refused: the block holds"
        );
        assert_eq!(
            app.apply_retry_for(Some(11)),
            ApplyRetry::NeedsPerson(remedy),
            "{cause:?}"
        );
    }
}

/// A press the folder answered is not sent back to the folder. With the folder's block standing
/// for build 11, the person fixes the folder and presses Install now: the worker's pre-park check
/// passes and keeps its pass, then the successor dies. The completion is surfaced before the
/// reconcile facts arrive, while the block still stands, so the pass is read where the remedy is:
/// the press's row does not name the folder and nothing is asked of the person. Negative control:
/// a press the folder refused again keeps the folder's remedy. (The reconcile that follows the
/// pass releases the block: `a_folder_that_took_the_write_since_it_refused_releases_its_block`.)
#[cfg(unix)]
#[test]
fn a_press_the_folder_answered_is_not_sent_back_to_the_folder() {
    use crate::update_apply_trouble::{ApplyRetry, PersonRemedy};
    let _ledger = crate::app_update_screen::hold_update_ledger_for_test();
    let location = "staged update failed pre-park verification: install location not \
                    writable: /Applications/aterm.app; this install cannot update itself";
    let ticket = crate::native_updater_service::ApplyAttemptTicket::for_test(
        11,
        PREFLIGHT_TEST_COMMIT,
        &"ab".repeat(32),
    );
    let folder = PersonRemedy::MoveOrAskAdmin.sentence();
    for answered in [true, false] {
        let mut app = App::headless_for_test();
        app.native_updater_service = NativeUpdaterService::new(10, "test", true);
        stage_one_build_for_test(&mut app, 11);
        // The folder's block, as the automatic attempt took it, and its booking.
        app.auto_apply_intent = None;
        app.block_native_auto_apply_environment(
            11,
            [0xab; 32],
            EnvironmentBlockCause::InstallLocation,
        );
        let blocked_at = app.auto_apply_environment_block.unwrap().blocked_at;
        app.native_updater_service
            .note_apply_failure(&aterm_update::RecordedApplyFailure {
                apply_failures: 1,
                failures_for_target: 1,
                target_build: 11,
                reason: location.to_string(),
                persistent: false,
            });
        // The press's own pre-park check, as the worker keeps its verdict:
        // the folder took the write (or, the control, refused again).
        *app.handoff_preverified.lock().unwrap() = Some(crate::HandoffPreverification {
            build: 11,
            commit: PREFLIGHT_TEST_COMMIT.to_string(),
            artifact: "ab".repeat(32),
            at: blocked_at + std::time::Duration::from_secs(2),
            passed: answered,
            reason: (!answered).then(|| location.to_string()),
            grant_chunks: false,
            policy: None,
        });
        let message = if answered {
            "overlap handoff failed safely: handoff proof ended ChildDied".to_string()
        } else {
            format!("overlap handoff failed safely: {location}")
        };
        // The completion, surfaced before any reconcile: the reaped abort's
        // settle, then the production booking and row.
        let outcome =
            app.settle_rearmed_apply_failure(&ticket, message, HandoffFailureLane::Manual, None);
        assert!(matches!(outcome, UpdateOutcome::Failed { .. }));
        app.surface_update_apply_outcome_for_target("manual handoff", outcome, false, 11);
        assert!(
            app.auto_apply_environment_blocked(11),
            "PRECONDITION: no reconcile has run, so the block still stands"
        );
        let row = app
            .messages
            .live_by_key(crate::update_words::KEY_OUTCOME)
            .expect("the press's row");
        let words = format!("{} {}", row.msg.title, row.msg.detail.join(" "));
        assert_eq!(words.contains(folder), !answered, "{words}");
        if answered {
            assert!(
                !matches!(app.apply_retry_for(Some(11)), ApplyRetry::NeedsPerson(_)),
                "nothing is asked of the person"
            );
        } else {
            assert_eq!(
                app.apply_retry_for(Some(11)),
                ApplyRetry::NeedsPerson(PersonRemedy::MoveOrAskAdmin)
            );
        }
    }
}

/// A later refusal of another cause names the standing block. With the folder's block standing
/// for build 11, the person retries and the press's pre-park check passes the folder but refuses
/// the installed copy ("cannot be the rollback source"), which only a person can clear too. The
/// refusal re-keys the block before the press's row is built, so `apply_retry_for` names the
/// signed release, and the reconcile releases the block once the installed copy verifies; with no
/// settle before it (the third case) the reconcile re-keys by the same reading. Negative control:
/// the folder's refusal met again keeps the folder's block, and that reconcile releases nothing.
#[cfg(target_os = "macos")]
#[test]
fn a_later_refusal_of_another_cause_names_the_standing_block() {
    use crate::update_apply_trouble::ApplyRetry;
    let _ledger = crate::app_update_screen::hold_update_ledger_for_test();
    let location = "staged update failed pre-park verification: install location not \
                    writable: /Applications/aterm.app; this install cannot update itself";
    let installed = "installed bundle failed pre-park verification: the installed bundle \
                     at /Applications/aterm.app cannot be the rollback source the swap \
                     installs: codesign refused it";
    let ticket = crate::native_updater_service::ApplyAttemptTicket::for_test(
        11,
        PREFLIGHT_TEST_COMMIT,
        &"ab".repeat(32),
    );
    for (refusal, cause, settled) in [
        (installed, EnvironmentBlockCause::InstalledSource, true),
        (location, EnvironmentBlockCause::InstallLocation, true),
        (installed, EnvironmentBlockCause::InstalledSource, false),
    ] {
        let mut app = App::headless_for_test();
        app.native_updater_service = NativeUpdaterService::new(10, "test", true);
        stage_one_build_for_test(&mut app, 11);
        app.auto_apply_intent = None;
        app.block_native_auto_apply_environment(
            11,
            [0xab; 32],
            EnvironmentBlockCause::InstallLocation,
        );
        let blocked_at = app.auto_apply_environment_block.unwrap().blocked_at;
        app.native_updater_service
            .note_apply_failure(&aterm_update::RecordedApplyFailure {
                apply_failures: 1,
                failures_for_target: 1,
                target_build: 11,
                reason: location.to_string(),
                persistent: false,
            });
        // The press's pre-park check, as `publish_refusal` keeps it.
        let refused_at = blocked_at + std::time::Duration::from_secs(2);
        *app.handoff_preverified.lock().unwrap() = Some(crate::HandoffPreverification {
            build: 11,
            commit: PREFLIGHT_TEST_COMMIT.to_string(),
            artifact: "ab".repeat(32),
            at: refused_at,
            passed: false,
            reason: Some(refusal.to_string()),
            grant_chunks: false,
            policy: None,
        });
        if settled {
            let outcome = app.settle_rearmed_apply_failure(
                &ticket,
                format!("overlap handoff failed safely: {refusal}"),
                HandoffFailureLane::Manual,
                None,
            );
            app.surface_update_apply_outcome_for_target("manual handoff", outcome, false, 11);
            let block = app.auto_apply_environment_block.expect("the block stands");
            assert_eq!(block.cause, cause, "the block names the newest refusal");
            assert_eq!(
                app.apply_retry_for(Some(11)),
                ApplyRetry::NeedsPerson(cause.remedy()),
                "{cause:?}"
            );
            let counted_from = if cause == EnvironmentBlockCause::InstalledSource {
                refused_at
            } else {
                blocked_at
            };
            assert_eq!(block.blocked_at, counted_from, "{cause:?}");
        }
        // The installed copy verifies against the running image, observed
        // after the refusal: the installed copy's block is released; the
        // folder's is not (nothing asked the folder).
        let mut durable = durable_download_status(&app, 11);
        durable.apply_failure = refusal.to_string();
        durable.apply_failure_build = 11;
        let mut facts = reconcile_facts_with_installed(
            2,
            2,
            Some(durable),
            Some(InstalledUpdate {
                commit: PREFLIGHT_TEST_COMMIT.to_string(),
                ..installed_update(10)
            }),
        );
        facts.observed_at = refused_at + std::time::Duration::from_secs(1);
        app.release_repaired_auto_apply_environment_for_commit(&facts, PREFLIGHT_TEST_COMMIT);
        assert_eq!(
            app.auto_apply_environment_blocked(11),
            cause == EnvironmentBlockCause::InstallLocation,
            "{cause:?}: released by its own cause's evidence"
        );
    }
}

/// The OS banner of an update-health announcement is owed to one cause: the copy in Applications
/// is not the signed release, so no lane can install over it until the owner acts
/// (`NeedsPerson`). Negative controls: every other kind, an install the lane will try again, and
/// `desktop_alerts` off post nothing.
#[cfg(unix)]
#[test]
fn the_health_banner_is_posted_for_an_install_only_the_owner_can_mend() {
    use crate::messages_host::UPDATE_HEALTH_BANNERS;
    use crate::update_apply_trouble::{ApplyRetry, PersonRemedy};
    use crate::update_words::HealthKind;
    let _ledger = crate::app_update_screen::hold_update_ledger_for_test();
    let take = || UPDATE_HEALTH_BANNERS.with(|b| std::mem::take(&mut *b.borrow_mut()));
    let title = aterm_update::health_failing_title("apply");
    let body = "3 failed attempts to install in a row since 2026-09-24T01:00:00Z: why.";
    let mut app = App::headless_for_test();
    app.config.desktop_alerts = Some(true);
    app.native_updater_service = NativeUpdaterService::new(10, "test", true);
    stage_one_build_for_test(&mut app, 11);
    // Scheduled by the lane itself: nothing for the owner.
    assert!(!matches!(
        app.apply_retry_for(Some(11)),
        ApplyRetry::NeedsPerson(_)
    ));
    let _ = take();
    assert!(!app.post_update_health_banner(HealthKind::Install, title, body));
    assert!(take().is_empty(), "the lane will try again");
    // The installed copy refuses: only the owner can mend that.
    app.block_native_auto_apply_environment(11, [0xab; 32], EnvironmentBlockCause::InstalledSource);
    assert_eq!(
        app.apply_retry_for(Some(11)),
        ApplyRetry::NeedsPerson(PersonRemedy::InstallSignedRelease)
    );
    assert!(app.post_update_health_banner(HealthKind::Install, title, body));
    let posted = take();
    assert_eq!(posted.len(), 1, "{posted:?}");
    for kind in HealthKind::ALL
        .into_iter()
        .filter(|k| *k != HealthKind::Install)
    {
        assert!(
            !app.post_update_health_banner(kind, title, body),
            "{kind:?}"
        );
    }
    assert!(take().is_empty());
    app.config.desktop_alerts = Some(false);
    assert!(!app.post_update_health_banner(HealthKind::Install, title, body));
    assert!(take().is_empty(), "desktop_alerts off decides last");
}

#[test]
fn retiring_a_withdrawn_stage_releases_only_its_exact_latch_and_rearms_a_replacement() {
    let _ledger = crate::app_update_screen::hold_update_ledger_for_test();
    let model = aterm_models_update::native_update_retired_intent_model();
    for unrelated in [0, 1, 2] {
        let mut app = App::headless_for_test();
        let withdrawn = app.native_updater_service.snapshot().current_build + 2;
        let replacement = withdrawn - 1;
        let _ = app.reconcile_native_update_facts(reconcile_facts(
            1,
            1,
            Some(status(Some(withdrawn), 0)),
        ));
        let latch = crate::AutoApplyManualOnly {
            build: withdrawn + u64::from(unrelated == 1),
            dmg_sha256: if unrelated == 2 {
                [0xcd; 32]
            } else {
                [0xab; 32]
            },
            activation: false,
            retry_at: None,
        };
        app.auto_apply_manual_only = Some(latch);
        // An unchanged exact stage and an older observation must keep the
        // latch: a periodic reconcile is not permission to reset a budget.
        let _ = app.reconcile_native_update_facts(reconcile_facts(
            2,
            2,
            Some(status(Some(withdrawn), 0)),
        ));
        assert_eq!(app.auto_apply_manual_only, Some(latch));
        assert!(matches!(
            app.reconcile_native_update_facts(reconcile_facts(1, 1, Some(status(None, 0)))),
            NativeUpdateFactsResult::IgnoredStale
        ));
        assert_eq!(app.auto_apply_manual_only, Some(latch));

        let mut before = model.init_state();
        if unrelated != 0 {
            before = model.successors("OtherArtifact", &before).remove(0);
        }
        let _ = app.reconcile_native_update_facts(reconcile_facts(3, 3, Some(status(None, 0))));
        let mut after = before.clone();
        after.insert(
            "retired",
            i64::from(app.native_updater_service.snapshot().staged.is_none()),
        );
        after.insert("latched", i64::from(app.auto_apply_manual_only.is_some()));
        assert_eq!(model.successors("Retire", &before), vec![after.clone()]);
        if unrelated != 0 {
            assert_eq!(app.auto_apply_manual_only, Some(latch));
            continue;
        }
        assert!(app.auto_apply_manual_only.is_none());
        app.finish_native_update_reconcile(
            NativeUpdateReconcilePurpose::StageAvailable,
            reconcile_facts(4, 4, Some(status(Some(replacement), 0))),
        );
        assert!(
            app.auto_apply_intent
                .is_some_and(|intent| intent.build == replacement)
        );
        assert!(app.automatic_apply_retry_scheduled(replacement));

        // The leftover latch alone would strand a valid replacement newer than the running
        // process.
        app.auto_apply_manual_only = Some(latch);
        app.auto_apply_intent = None;
        assert!(!app.arm_native_auto_apply(replacement, &"ab".repeat(32)));
        assert!(app.auto_apply_intent.is_none());
        let mut historical = after;
        historical.insert("latched", 1);
        assert!(!model.successors("Retire", &before).contains(&historical));
        assert!(!model.check_invariant("NoObsoleteLatch", &historical));
    }
}

#[test]
fn a_refresh_never_outranks_a_purpose_that_announces_or_applies() {
    use NativeUpdateReconcilePurpose::{ApplyControl, Refresh, StageAvailable, Startup};
    for other in [Startup, StageAvailable, ApplyControl] {
        assert_eq!(merge_reconcile_purpose(Refresh, other), other);
        assert_eq!(merge_reconcile_purpose(other, Refresh), other);
    }
    assert_eq!(merge_reconcile_purpose(Refresh, Refresh), Refresh);
    // And a startup fact import coalesced with a refresh still announces.
    assert_eq!(merge_reconcile_purpose(Startup, Refresh), Startup);
}

#[test]
fn deferred_reconcile_merges_strongest_purpose_independently_of_fact_order() {
    for (first_purpose, first_observation, second_purpose, second_observation) in [
        (
            NativeUpdateReconcilePurpose::StageAvailable,
            9,
            NativeUpdateReconcilePurpose::ApplyControl,
            8,
        ),
        (
            NativeUpdateReconcilePurpose::ApplyControl,
            8,
            NativeUpdateReconcilePurpose::Startup,
            9,
        ),
        (
            NativeUpdateReconcilePurpose::Startup,
            7,
            NativeUpdateReconcilePurpose::ApplyControl,
            10,
        ),
    ] {
        let mut app = App::headless_for_test();
        let _active = start(&mut app.native_updater_service);
        app.finish_native_update_reconcile(
            first_purpose,
            reconcile_facts(1, first_observation, Some(status(Some(11), 0))),
        );
        app.finish_native_update_reconcile(
            second_purpose,
            reconcile_facts(2, second_observation, Some(status(Some(12), 0))),
        );
        let (purpose, facts) = app
            .deferred_native_update_reconcile
            .as_ref()
            .expect("active service defers facts");
        assert_eq!(*purpose, NativeUpdateReconcilePurpose::ApplyControl);
        assert_eq!(
            facts.observation_sequence,
            first_observation.max(second_observation),
            "purpose merge and newest-facts selection are independent"
        );
    }
}

/// "Check for Updates…" parks a Refresh behind its own check; when the check
/// stages, the parked pre-stage facts are dropped for a fresh observation
/// and the stage survives.
#[test]
fn facts_parked_behind_a_check_cannot_retire_the_stage_that_check_imports() {
    let mut app = App::headless_for_test();
    let running = app.native_updater_service.snapshot().current_build;
    let build = running + 1;
    let ticket = start(&mut app.native_updater_service);
    // Observed before the check staged anything: nothing staged, bundle == running.
    app.finish_native_update_reconcile(
        NativeUpdateReconcilePurpose::Refresh,
        reconcile_facts_with_installed(
            1,
            3,
            Some(status(None, 0)),
            Some(installed_update(running)),
        ),
    );
    assert!(
        app.deferred_native_update_reconcile.is_some(),
        "PRECONDITION: parked"
    );
    // The check completes with a stage.
    app.finish_native_update_check(ticket, status(Some(build), 0));
    assert_eq!(
        app.native_updater_service
            .snapshot()
            .staged
            .as_ref()
            .map(|s| s.build),
        Some(build),
        "the stage the check imported survives the parked pre-stage facts"
    );
    assert!(
        app.deferred_native_update_reconcile.is_none(),
        "the stale parked facts are gone (re-observed fresh, not replayed)"
    );
}

/// A read that began before the check staged (its wake lands after, so it is
/// not parked) must not retire the stage either; `observed_at` is the floor.
#[test]
fn facts_read_before_the_stage_import_cannot_retire_it_however_late_they_land() {
    let mut app = App::headless_for_test();
    let running = app.native_updater_service.snapshot().current_build;
    let build = running + 1;
    // Read began before the import…
    let mut early = reconcile_facts_with_installed(
        1,
        3,
        Some(status(None, 0)),
        Some(installed_update(running)),
    );
    early.observed_at = std::time::Instant::now() - std::time::Duration::from_secs(1);
    let ticket = start(&mut app.native_updater_service);
    app.finish_native_update_check(ticket, status(Some(build), 0));
    assert!(
        app.native_stage_imported_at.is_some(),
        "PRECONDITION: the import is floored"
    );
    // …and its wake lands after the check completed (reducer free, not parked).
    app.finish_native_update_reconcile(NativeUpdateReconcilePurpose::Refresh, early);
    assert_eq!(
        app.native_updater_service
            .snapshot()
            .staged
            .as_ref()
            .map(|s| s.build),
        Some(build),
        "a pre-import observation is stale by construction and retires nothing"
    );
    // A read that began after the import and sees the stage on disk is reduced
    // normally and keeps it.
    app.finish_native_update_reconcile(
        NativeUpdateReconcilePurpose::Refresh,
        reconcile_facts_with_installed(
            2,
            4,
            Some(status(Some(build), 0)),
            Some(installed_update(running)),
        ),
    );
    assert_eq!(
        app.native_updater_service
            .snapshot()
            .staged
            .as_ref()
            .map(|s| s.build),
        Some(build)
    );
}

/// A control apply parked while the reducer was busy rides the newer facts
/// of the failed attempt's own Startup wake, instead of being dropped as
/// stale.
#[test]
fn a_parked_control_apply_rides_the_next_newer_facts_instead_of_going_stale() {
    // Records an apply refusal in the shared update ledger.
    let _ledger = crate::app_update_screen::hold_update_ledger_for_test();
    let mut app = App::headless_for_test();
    let running = app.native_updater_service.snapshot().current_build;
    let build = running + 1;
    // Park an ApplyControl behind an active check (the reducer defers facts
    // while work is active).
    let active = start(&mut app.native_updater_service);
    app.finish_native_update_reconcile(
        NativeUpdateReconcilePurpose::ApplyControl,
        reconcile_facts(1, 5, Some(status(Some(build), 0))),
    );
    assert!(
        app.deferred_native_update_reconcile
            .as_ref()
            .is_some_and(|(p, f)| *p == NativeUpdateReconcilePurpose::ApplyControl
                && f.observation_sequence == 5),
        "PRECONDITION: the control apply is parked"
    );
    // The check completes with nothing (the reducer is free again)…
    let _ = app
        .native_updater_service
        .finish_check(active, status(None, 0));
    // …and newer facts arrive under a plain Startup purpose, like the failed
    // attempt's own facts wake.
    app.finish_native_update_reconcile(
        NativeUpdateReconcilePurpose::Startup,
        reconcile_facts(2, 9, Some(status(Some(build), 0))),
    );
    assert!(
        app.deferred_native_update_reconcile.is_none(),
        "the parked purpose merged into the newer facts instead of waiting to go stale"
    );
    // ApplyControl acted: an Immediate apply was attempted on the imported
    // stage; a headless host refuses it at preflight (no event-loop service),
    // visible as a surfaced control-request outcome.
    let outcome = app.native_updater_service.snapshot().outcome.clone();
    assert!(
        app.native_updater_service
            .snapshot()
            .staged
            .as_ref()
            .map(|s| s.build)
            == Some(build),
        "the newer facts imported the stage, got outcome {outcome:?}"
    );
    assert!(
        app.update_row_text().is_some()
            || app.native_updater_service.snapshot().phase != UpdaterPhase::Staged,
        "the control apply was acted on (surfaced or moved the phase), not dropped"
    );
}

/// A SEAMLESS SUCCESSOR'S OWN UPDATE FACTS OUTLIVE ITS COMMIT WAIT (round eight of
/// the update audit, U14). The successor asks the facts worker for its Startup
/// read as it boots; the answer usually lands inside the overlap, before the
/// Commit, and nothing asks again after it. Swallowed there, a restored Settings ▸
/// Software Update page kept the pristine reducer's "You're up to date." while the
/// ledger said updates had been failing for half an hour. Now the answer waits for
/// the Commit wake, which reduces it once this process is no candidate.
///
/// RED before the change: the wake was dropped, nothing was held, and the headline
/// stayed "You're up to date." with no republish. NEGATIVE CONTROL: before its
/// Commit the candidate reduces nothing.
#[cfg(unix)]
#[test]
fn a_successors_startup_update_facts_survive_the_commit_wait() {
    let mut app = App::headless_for_test();
    app.incoming_handoff_pending = true;
    let mut failing = status(None, 9);
    failing.failing_persistent = true;
    failing.failing_kind = "manifest".to_string();
    let wake = crate::Wake::NativeUpdateReconcileFinished {
        purpose: NativeUpdateReconcilePurpose::Startup,
        facts: reconcile_facts(1, 1, Some(failing)),
    };
    assert!(
        app.admit_wake_before_commit(wake).is_none(),
        "a candidate does not act on it before its Commit"
    );
    assert_ne!(
        app.update_snapshot(false).projection().headline,
        "Updates are failing on this machine.",
        "NEGATIVE: nothing is reduced before the Commit"
    );
    // The Commit wake: the handoff is no longer pending, and what was held runs
    // through `user_event`, whose arm for this wake is this call.
    app.incoming_handoff_pending = false;
    let published = FULL_UPDATE_PUBLISHES.with(std::cell::Cell::get);
    for held in std::mem::take(&mut app.handoff_committed_wakes) {
        let crate::Wake::NativeUpdateReconcileFinished { purpose, facts } = held else {
            panic!("only the update facts were held");
        };
        app.finish_native_update_reconcile(purpose, facts);
    }
    assert_eq!(
        app.update_snapshot(false).projection().headline,
        "Updates are failing on this machine."
    );
    assert!(
        FULL_UPDATE_PUBLISHES.with(std::cell::Cell::get) > published,
        "the restored page is republished"
    );
}

fn start(service: &mut NativeUpdaterService) -> UpdaterWorkTicket {
    match service.request_check() {
        CheckStart::Start(ticket) => ticket,
        other => panic!("expected updater work, got {other:?}"),
    }
}

/// `arm_native_auto_apply` applies each `arm` decision to the retained
/// intent (the decisions themselves are `native_update_auto_intent`'s): the
/// same exact target keeps its backoff and attempt count; a newer build
/// replaces it, fresh, due at once; a manual-only latch on exactly
/// these bytes survives duplicate wakes while new bytes re-arm and release
/// it; and `[update] auto_apply = false` clears the retained intent.
#[test]
fn arm_applies_each_decision_to_the_retained_intent() {
    let mut app = App::headless_for_test();
    let current = app.native_updater_service.snapshot().current_build;
    let retained = crate::AutoApplyIntent {
        build: current + 1,
        dmg_sha256: [0xab; 32],
        retry_at: std::time::Instant::now() + std::time::Duration::from_secs(23),
        attempts: 3,
    };
    app.auto_apply_intent = Some(retained);
    assert!(!app.arm_native_auto_apply(current + 1, &"ab".repeat(32)));
    assert_eq!(
        app.auto_apply_intent,
        Some(retained),
        "the same target keeps"
    );

    let before = std::time::Instant::now();
    assert!(app.arm_native_auto_apply(current + 2, &"cd".repeat(32)));
    let after = std::time::Instant::now();
    let intent = app.auto_apply_intent.expect("newer build stays armed");
    assert_eq!(
        (intent.build, intent.dmg_sha256, intent.attempts),
        (current + 2, [0xcd; 32], 0)
    );
    assert!(
        intent.retry_at >= before && intent.retry_at <= after,
        "due at once"
    );

    let latch = crate::AutoApplyManualOnly {
        build: current + 2,
        dmg_sha256: [0xcd; 32],
        activation: false,
        retry_at: None,
    };
    app.auto_apply_intent = None;
    app.auto_apply_manual_only = Some(latch);
    for _ in 0..3 {
        assert!(!app.arm_native_auto_apply(current + 2, &"cd".repeat(32)));
        assert!(app.auto_apply_intent.is_none());
        assert_eq!(app.auto_apply_manual_only, Some(latch), "a duplicate wake");
    }
    assert!(app.arm_native_auto_apply(current + 2, &"ef".repeat(32)));
    assert_eq!(
        app.auto_apply_intent.map(|intent| intent.dmg_sha256),
        Some([0xef; 32])
    );
    assert!(app.auto_apply_manual_only.is_none(), "new bytes release it");

    app.config.update = Some(crate::app_config::UpdateConfig {
        auto_apply: Some(false),
        ..crate::app_config::UpdateConfig::default()
    });
    assert!(!app.arm_native_auto_apply(current + 3, &"12".repeat(32)));
    assert!(app.auto_apply_intent.is_none(), "auto_apply off clears it");
}

/// Presentation-ack liveness trace: HiddenOutput -> WakeHandledNoPresent ->
/// quiet epoch. The hidden session keeps its first-edge presentation stamp
/// forever (no incidental redraw clears it), yet its output ages to quiet, so
/// it never holds automatic apply off for good.
#[test]
fn old_hidden_output_without_present_ages_to_quiet() {
    let mut app = App::headless_for_test();
    app.config.cursor_blink = Some(false);
    app.serious_mode = true;
    // Serious Mode suppresses word decorations without a hidden runtime
    // override. Matrix rain needs no kill: its config default is off and no
    // session override exists, so no effect redraw can arise.
    if let Some(window) = app.windows.get_mut(&crate::WindowId(0)) {
        window.focused = false;
        window.next_blink = None;
    }

    // Appending session 1 makes session 0 a genuine hidden background tab.
    app.push_stub_tab(crate::WindowId(0), crate::stub_session(app.next_session_id));
    assert!(!app.is_visible_session(0));
    assert!(app.headless);

    let output_at = std::time::Instant::now();
    let output_ns = u64::try_from(
        output_at
            .saturating_duration_since(app.lat_epoch)
            .as_nanos(),
    )
    .unwrap_or(u64::MAX)
    .max(1);
    let (hidden_present_stamp, hidden_activity_stamp) = {
        let hidden = app.pool.get(0).expect("hidden session remains pooled");
        (
            hidden.last_output_ns.clone(),
            hidden.latest_output_activity_ns.clone(),
        )
    };
    hidden_present_stamp.store(output_ns, std::sync::atomic::Ordering::Relaxed);
    hidden_activity_stamp.store(output_ns, std::sync::atomic::Ordering::Release);

    // Before the output wake is handled, the latest-output clock alone closes
    // the race even though the preceding handled activity is already quiet.
    app.last_update_activity_at = output_at
        .checked_sub(crate::AUTOMATIC_UPDATE_QUIET_EPOCH + std::time::Duration::from_nanos(1))
        .expect("monotonic clock has at least one quiet epoch of history");
    assert!(!app.automatic_update_activity_quiet_with_pending_input(output_at, false));

    // Handle the wake without presenting the hidden tab. Both recent-output
    // clocks reject immediately after the wake.
    app.note_update_handoff_activity();
    let wake_handled_at = app.last_update_activity_at;
    assert!(!app.automatic_update_activity_quiet_with_pending_input(wake_handled_at, false));

    let quiet_at =
        wake_handled_at + crate::AUTOMATIC_UPDATE_QUIET_EPOCH + std::time::Duration::from_nanos(1);
    assert!(app.automatic_update_activity_quiet_with_pending_input(quiet_at, false));
    assert_ne!(
        hidden_present_stamp.load(std::sync::atomic::Ordering::Relaxed),
        0,
        "no present acknowledged the hidden latency sample"
    );
}

#[cfg(unix)]
/// An activity-revoked overlap schedules spaced re-attempts that saturate at the last rung (inside
/// the ladder's one-minute bound) and never exhaust.
#[test]
fn activity_revoked_retry_spacing_saturates_and_never_exhausts() {
    // Spaced: every rung waits, and each waits longer than the one before, so
    // a walk that refills the ladder is distinguishable from one that climbs it.
    let rungs: Vec<_> = (0..ACTIVITY_REVOKED_LADDER_RUNGS)
        .map(|cycles| {
            automatic_retry_delay(cycles, AutomaticRetryKind::ActivityRevoked)
                .expect("inside the ladder")
        })
        .collect();
    assert!(
        rungs[0] > std::time::Duration::ZERO && rungs.windows(2).all(|pair| pair[0] < pair[1]),
        "the spacing widens rung by rung: {rungs:?}"
    );
    let last = *rungs.last().expect("the ladder's last rung");
    for cycles in [ACTIVITY_REVOKED_LADDER_RUNGS, 50, u8::MAX] {
        assert_eq!(
            automatic_retry_delay(cycles, AutomaticRetryKind::ActivityRevoked),
            Some(last),
            "cycle {cycles}: the last rung, forever — never `None`"
        );
    }
    assert!(
        last < crate::native_update_auto_intent::LANDS_WITHIN,
        "the saturated spacing fits inside the bound it serves"
    );
}

#[cfg(unix)]
/// A fork-lane park miss is booked as a refusal where `update status` reads
/// it (`apply_refusal=`), never a failure (`failing_applies` does not move),
/// with the spaced activity retry armed. A person's apply is told it failed
/// and arms nothing.
// The bundle apply lane and its ledger (`aterm_update::apply_lane_report`) are
// macOS's: a Linux update replaces the executable on disk and hands nothing off.
#[cfg(target_os = "macos")]
#[test]
fn a_fork_lane_park_miss_is_booked_as_a_refusal_not_a_failure() {
    use crate::native_updater_service::ApplyMode;
    // The ledger is one file per test process: hold it, so a sibling
    // test's failure cannot move `failing_applies` under the comparison.
    let _ledger = crate::app_update_screen::hold_update_ledger_for_test();
    let mut app = App::headless_for_test();
    let current = app.native_updater_service.snapshot().current_build;
    let build = current + 6_127;
    let ticket = crate::native_updater_service::ApplyAttemptTicket::for_test(
        build,
        PREFLIGHT_TEST_COMMIT,
        &"e7".repeat(32),
    );
    const BEFORE: &str = "test marker before the fork-lane park miss";
    aterm_update::record_apply_refusal(current, BEFORE);
    let before = aterm_update::status(current).unwrap();
    let miss =
        format!("a PTY reader missed the 250 ms handoff park deadline (fork-lane test {build})");

    let outcome = app.fork_park_miss_outcome(&ticket, ApplyMode::AutomaticPastGrace, miss.clone());
    assert!(
        matches!(&outcome, UpdateOutcome::Deferred { reason } if *reason == miss),
        "the automatic lane defers: {outcome:?}"
    );
    assert!(
        app.auto_apply_intent
            .is_some_and(|intent| intent.build == build),
        "the spaced activity retry is armed"
    );
    let report = aterm_update::apply_lane_report(current).unwrap();
    assert!(
        report.last_refusal.contains(&miss),
        "the miss is booked where `update status` reads it: {:?}",
        report.last_refusal
    );
    assert_eq!(
        aterm_update::status(current).unwrap().failing_applies,
        before.failing_applies,
        "a missed stopwatch is not a failed apply"
    );
    assert!(
        !report.last_failure.contains(&miss),
        "and it is never booked in the failure slot: {:?}",
        report.last_failure
    );

    // A person's apply: a failure they are told about, nothing armed.
    let mut person = App::headless_for_test();
    let outcome = person.fork_park_miss_outcome(&ticket, ApplyMode::Immediate, miss.clone());
    assert!(
        matches!(outcome, UpdateOutcome::Failed { .. }),
        "{outcome:?}"
    );
    assert!(person.auto_apply_intent.is_none());
}

#[cfg(unix)]
/// A capture refusal gets one verdict per asker whichever lane carried it:
/// a person's press answers `Failed` on the fork lane as on the launched
/// lane. Table-driven from the launched lane's own classifier.
#[test]
fn a_capture_refusal_answers_by_who_asked_on_both_lanes() {
    use crate::native_updater_service::ApplyMode;
    for mode in [
        ApplyMode::Automatic,
        ApplyMode::AutomaticPastGrace,
        ApplyMode::Immediate,
    ] {
        let fork = fork_capture_refusal_outcome(mode, "session 3".to_string());
        match HandoffFailureLane::classify(
            mode,
            crate::UpdateHandoffOutcome::CaptureRefused,
            crate::ChildDeathEvidence::Unobserved,
            false,
        ) {
            HandoffFailureLane::Refused => assert!(
                matches!(fork, UpdateOutcome::CaptureRefused { .. }),
                "{mode:?}: {fork:?}"
            ),
            HandoffFailureLane::Manual => assert!(
                matches!(fork, UpdateOutcome::Failed { .. }),
                "{mode:?}: {fork:?}"
            ),
            other => panic!("{mode:?}: a capture refusal classified {other:?}"),
        }
    }
}

/// Converge automatic apply structurally on the activation of `installed` (imported at reconcile
/// sequence `observation`) through the shipping completion lane, and return its digest.
fn converge_structurally_on_the_activation(
    app: &mut App,
    installed: &InstalledUpdate,
    observation: u64,
) -> String {
    let _ = app.reconcile_native_update_facts(reconcile_facts_with_installed(
        observation,
        observation,
        Some(status(None, 0)),
        Some(installed.clone()),
    ));
    assert_activation_stage(app, installed.build, &installed.commit);
    let activation = crate::native_updater_service::installed_activation_digest(
        installed.build,
        &installed.commit,
    );
    for _ in 0..STRUCTURAL_FAILURE_LIFETIME_ATTEMPTS {
        fail_the_activation_structurally(app, installed, &activation);
    }
    assert!(
        app.auto_apply_structural_verdict
            .is_some_and(|verdict| verdict.build == installed.build && verdict.activation),
        "PRECONDITION: the lane converged structurally on the activation"
    );
    activation
}

/// One structural failure of the activation of `installed`, returned through
/// the shipping completion lane.
fn fail_the_activation_structurally(
    app: &mut App,
    installed: &InstalledUpdate,
    activation: &str,
) -> UpdateOutcome {
    let ticket = crate::native_updater_service::ApplyAttemptTicket::for_test(
        installed.build,
        &installed.commit,
        activation,
    );
    ticket.make_current_apply_for_test(&mut app.native_updater_service);
    app.abort_reaped_native_apply_before_reconcile(
        &ticket,
        "overlap handoff failed safely: handoff proof ended AdoptionMismatch".to_string(),
        HandoffFailureLane::Physical(PhysicalFailureShape::Structural),
    )
}

/// The observation the facts worker posts after a failed attempt returned:
/// nothing newer on disk, and the boot sentinel's count for `installed` read
/// after the attempt, the only count a structural latch may act on.
fn observe_after_the_attempt(app: &mut App, installed: &InstalledUpdate, observation: u64) {
    assert!(
        app.native_installed_trial.is_none(),
        "PRECONDITION: the returned attempt left the count unmeasured"
    );
    let _ = app.reconcile_native_update_facts(reconcile_facts_with_installed(
        observation,
        observation,
        Some(status(None, 0)),
        Some(installed.clone()),
    ));
    assert_eq!(
        app.native_installed_trial,
        Some((installed.build, installed.trial_launches)),
        "an observation read after the attempt measures the count"
    );
}

/// Put a structural latch's one re-sample deadline (the latch's and the verdict's, the same
/// instant) in the past: the day passing.
fn let_the_day_pass(app: &mut App) {
    let past = std::time::Instant::now() - std::time::Duration::from_secs(1);
    let latch = app
        .auto_apply_manual_only
        .expect("PRECONDITION: a structural latch");
    let verdict = app
        .auto_apply_structural_verdict
        .expect("PRECONDITION: its verdict");
    assert_eq!(
        latch.retry_at, verdict.resample_at,
        "PRECONDITION: the latch's deadline IS the verdict's re-sample"
    );
    app.auto_apply_manual_only = Some(crate::AutoApplyManualOnly {
        retry_at: Some(past),
        ..latch
    });
    app.auto_apply_structural_verdict = Some(crate::AutoApplyStructuralVerdict {
        resample_at: Some(past),
        ..verdict
    });
}

/// A converged latch on the activation is superseded by a newer release: build 12 arriving starts
/// the retire of 11's activation, naming the exact sealed bytes it may take off the install path,
/// and while it runs a press on the activation waits (a candidate launched now could come up as
/// build 11 from the bundle being retired). A latch with no verdict behind it (the policy
/// fail-safe, the environment block) is not this path's to release. (The retire landing, being
/// refused and retried is `native_updater_conformance`'s supersede bind.)
#[test]
fn a_newer_release_starts_the_retire_of_a_converged_activation_latch() {
    let _ledger = crate::app_update_screen::hold_update_ledger_for_test();
    let mut app = App::headless_for_test();
    let installed = installed_update(11);
    let _ = converge_structurally_on_the_activation(&mut app, &installed, 1);
    let _ = app.reconcile_native_update_facts(reconcile_facts_with_installed(
        2,
        2,
        Some(status(Some(12), 0)),
        Some(installed.clone()),
    ));
    assert_eq!(
        app.native_activation_supersede.as_ref().map(|job| (
            job.activation,
            job.newer,
            job.activation_commit.clone()
        )),
        Some((11, 12, installed.commit.clone())),
        "build 12 asks for build 11's place, naming the exact sealed bytes"
    );
    assert!(
        matches!(
            app.apply_native_update(ApplyMode::Immediate),
            UpdateOutcome::Deferred { .. }
        ),
        "the Version menu's press waits for the retire"
    );

    // A latch with no verdict is not this path's: the policy fail-safe and
    // the environment block wait on what they wait on.
    let mut app = App::headless_for_test();
    let _ = app.reconcile_native_update_facts(reconcile_facts_with_installed(
        1,
        1,
        Some(status(None, 0)),
        Some(installed.clone()),
    ));
    let activation =
        crate::native_updater_service::installed_activation_digest(11, &installed.commit);
    app.auto_apply_manual_only = Some(crate::AutoApplyManualOnly {
        build: 11,
        dmg_sha256: decode_dmg_sha256(&activation).expect("an activation digest"),
        activation: true,
        retry_at: None,
    });
    let _ = app.reconcile_native_update_facts(reconcile_facts_with_installed(
        2,
        2,
        Some(status(Some(12), 0)),
        Some(installed.clone()),
    ));
    assert!(
        app.native_activation_supersede.is_none()
            && app
                .auto_apply_manual_only
                .is_some_and(|manual| manual.retry_at.is_none()),
        "a verdict-less latch holds whatever arrives"
    );
    // …nor the environment block, even with a verdict on its build: it waits
    // on the installed copy changing, which a release cannot do.
    let mut app = App::headless_for_test();
    let activation = converge_structurally_on_the_activation(&mut app, &installed, 1);
    app.block_native_auto_apply_environment(
        11,
        decode_dmg_sha256(&activation).expect("an activation digest"),
        EnvironmentBlockCause::InstalledSource,
    );
    let _ = app.reconcile_native_update_facts(reconcile_facts_with_installed(
        2,
        2,
        Some(status(Some(12), 0)),
        Some(installed.clone()),
    ));
    assert!(
        app.native_activation_supersede.is_none()
            && app.auto_apply_environment_block.is_some()
            && app
                .auto_apply_manual_only
                .is_some_and(|manual| manual.retry_at.is_none()),
        "the environment block holds whatever arrives"
    );
}

/// A landed retire with no observation leaves the latch standing, so an older in-flight
/// observation cannot arm build 11; the observation of the new disk releases it and arms 12.
#[test]
fn only_an_observation_of_the_new_disk_releases_a_superseded_activation_latch() {
    let _ledger = crate::app_update_screen::hold_update_ledger_for_test();
    let mut app = App::headless_for_test();
    let installed = installed_update(11);
    let activation = converge_structurally_on_the_activation(&mut app, &installed, 1);
    let _ = app.reconcile_native_update_facts(reconcile_facts_with_installed(
        2,
        2,
        Some(status(Some(12), 0)),
        Some(installed.clone()),
    ));
    let job = app
        .native_activation_supersede
        .clone()
        .expect("PRECONDITION: the retire started");
    app.finish_activation_supersede(ActivationSupersedeCompletion {
        job,
        outcome: Ok(aterm_update::SupersededActivation {
            activation_build: 11,
            restored_build: 10,
            newer_build: 12,
            trial_launches: 0,
            trial_disarm_deferred: None,
        }),
        facts: None,
    });
    assert!(app.native_activation_supersede.is_none());
    // An observation read before the retire, delivered after it.
    app.finish_native_update_reconcile(
        NativeUpdateReconcilePurpose::Refresh,
        reconcile_facts_with_installed(3, 3, Some(status(Some(12), 0)), Some(installed.clone())),
    );
    assert!(
        app.auto_apply_manual_only.is_some() && app.auto_apply_intent.is_none(),
        "a stale observation of the activation arms nothing"
    );
    assert!(!app.arm_native_auto_apply(11, &activation));
    // The observation of the new disk.
    app.finish_native_update_reconcile(
        NativeUpdateReconcilePurpose::StageAvailable,
        reconcile_facts_with_installed(
            4,
            4,
            Some(status(Some(12), 0)),
            Some(InstalledUpdate {
                build: 10,
                ..installed.clone()
            }),
        ),
    );
    assert!(app.auto_apply_manual_only.is_none());
    assert_eq!(app.auto_apply_intent.map(|intent| intent.build), Some(12));
}

/// Build 11's latched activation converged under a running 10, and a verified
/// download of 12 reconciled, so the look started the retire. Returns the
/// converged latch and the retire in flight.
fn converge_and_start_the_retire(
    app: &mut App,
    installed: &InstalledUpdate,
) -> (crate::AutoApplyManualOnly, ActivationSupersedeJob) {
    let _ = converge_structurally_on_the_activation(app, installed, 1);
    let converged = app.auto_apply_manual_only.expect("latched");
    let _ = app.reconcile_native_update_facts(reconcile_facts_with_installed(
        2,
        2,
        Some(status(Some(12), 0)),
        Some(installed.clone()),
    ));
    let job = app
        .native_activation_supersede
        .clone()
        .expect("PRECONDITION: the retire started");
    assert_eq!((job.newer, job.spent_before), (12, 0));
    (converged, job)
}

/// Bring a refused release's retry deadline to now: the backoff passing.
fn let_the_retry_come_due(app: &mut App) {
    let verdict = app
        .auto_apply_structural_verdict
        .expect("PRECONDITION: the verdict");
    let retry = verdict
        .newer_retry
        .expect("PRECONDITION: a refused release waits out its retry");
    app.auto_apply_structural_verdict = Some(crate::AutoApplyStructuralVerdict {
        newer_retry: Some(crate::SupersedeRetry {
            at: std::time::Instant::now() - std::time::Duration::from_secs(1),
            ..retry
        }),
        ..verdict
    });
}

/// A refused retire leaves the latch as it was and the event loop folds the
/// release's retry deadline. A refusal about the moment puts nothing on
/// glass; one about the disk's shape is the convergence notice. (The release
/// given back and offered again at its deadline is
/// `native_updater_conformance`'s structural latch arc.)
#[test]
fn a_refused_supersede_keeps_the_latch_and_says_only_a_refusal_of_the_disk() {
    let _ledger = crate::app_update_screen::hold_update_ledger_for_test();
    for settled in [false, true] {
        let kind = if settled { "the disk" } else { "a moment" };
        let mut app = App::headless_for_test();
        let installed = installed_update(11);
        let (converged, job) = converge_and_start_the_retire(&mut app, &installed);
        // Forget the convergence's own notice, so what the refusal says is
        // what is read below.
        app.auto_apply_stranded_announced = None;
        let refusal = if settled {
            aterm_update::SupersedeRefusal::Settled(
                "the installed build 11 stays in place: no boot trial is armed for it".to_string(),
            )
        } else {
            aterm_update::SupersedeRefusal::ForNow(
                "the installed build 11 stays in place: the staged bundle does not \
                 verify: the apply budget ran out"
                    .to_string(),
            )
        };
        app.finish_activation_supersede(ActivationSupersedeCompletion {
            job,
            outcome: Err(refusal),
            facts: Some(reconcile_facts_with_installed(
                3,
                3,
                Some(status(Some(12), 0)),
                Some(installed.clone()),
            )),
        });
        assert!(app.native_activation_supersede.is_none(), "{kind}");
        assert_eq!(
            app.auto_apply_manual_only,
            Some(converged),
            "{kind}: the latch stands exactly as it was"
        );
        let verdict = app.auto_apply_structural_verdict.expect("the verdict");
        assert_eq!(
            app.structural_supersede_retry_at(),
            verdict.newer_retry.map(|retry| retry.at),
            "{kind}: the event loop folds the retry's deadline"
        );
        assert!(app.structural_supersede_retry_at().is_some(), "{kind}");
        assert_eq!(
            app.auto_apply_stranded_announced.is_some(),
            settled,
            "{kind}: only a refusal about the disk is the convergence notice; a moment \
             puts nothing on glass — the lane is not stranded"
        );
    }
}

/// The backoff doubles per consecutive refusal up to its ceiling; a still
/// newer release is not held to the refused one's deadline; and a refused
/// release that left the disk is dropped with its retry.
#[test]
fn a_refused_release_backs_off_and_a_newer_one_does_not_wait_for_it() {
    assert_eq!(supersede_retry_after(40), SUPERSEDE_RETRY_CEILING);
    let _ledger = crate::app_update_screen::hold_update_ledger_for_test();
    let mut app = App::headless_for_test();
    let installed = installed_update(11);
    let (_, job) = converge_and_start_the_retire(&mut app, &installed);
    let refuse = |app: &mut App, job: ActivationSupersedeJob, sequence: u64, newer: u64| {
        app.finish_activation_supersede(ActivationSupersedeCompletion {
            job,
            outcome: Err(aterm_update::SupersedeRefusal::ForNow(
                "apply lock: busy".to_string(),
            )),
            facts: Some(reconcile_facts_with_installed(
                sequence,
                sequence,
                Some(status(Some(newer), 0)),
                Some(installed.clone()),
            )),
        });
    };
    refuse(&mut app, job, 3, 12);
    let_the_retry_come_due(&mut app);
    assert!(!app.lapse_expired_auto_apply_manual_only());
    let job = app
        .native_activation_supersede
        .clone()
        .expect("the retry started");
    let before = std::time::Instant::now();
    refuse(&mut app, job, 4, 12);
    let verdict = app.auto_apply_structural_verdict.expect("the verdict");
    assert_eq!(verdict.newer_refusals, 2);
    assert!(
        verdict
            .newer_retry
            .is_some_and(|retry| retry.at >= before + SUPERSEDE_RETRY_AFTER * 2),
        "the second refusal in a row waits twice as long: {verdict:?}"
    );
    // A still newer release is offered at once, not held to 12's deadline.
    let _ = app.reconcile_native_update_facts(reconcile_facts_with_installed(
        5,
        5,
        Some(status(Some(13), 0)),
        Some(installed.clone()),
    ));
    assert_eq!(
        app.native_activation_supersede
            .as_ref()
            .map(|job| (job.newer, job.spent_before)),
        Some((13, 0)),
        "a still newer release is offered the install path at once"
    );
    assert_eq!(
        app.auto_apply_structural_verdict
            .and_then(|verdict| verdict.newer_retry),
        None,
        "12's retry is moot"
    );
    // Refused too, and then the release leaves the disk: the retry goes
    // with it, and nothing more is folded.
    let job = app
        .native_activation_supersede
        .clone()
        .expect("13's retire");
    refuse(&mut app, job, 6, 13);
    assert!(app.structural_supersede_retry_at().is_some());
    let _ = app.reconcile_native_update_facts(reconcile_facts_with_installed(
        7,
        7,
        Some(status(None, 0)),
        Some(installed.clone()),
    ));
    assert_eq!(
        app.structural_supersede_retry_at(),
        None,
        "a release that left the disk is not retried"
    );
}

/// A retire whose disarm was deferred is a retire: the running build is back
/// at the install path and only the trial disarm is left to the next boot
/// apply. Nothing is said as a refusal, and the lane stages and arms the
/// newer build as after a clean retire.
#[test]
fn a_retire_with_its_disarm_deferred_is_not_said_as_a_refusal() {
    let _ledger = crate::app_update_screen::hold_update_ledger_for_test();
    let mut app = App::headless_for_test();
    let installed = installed_update(11);
    let (_, job) = converge_and_start_the_retire(&mut app, &installed);
    app.auto_apply_stranded_announced = None;
    #[cfg(target_os = "macos")]
    let refusal_before = aterm_update::apply_lane_report(10).map(|report| report.last_refusal);
    app.finish_activation_supersede(ActivationSupersedeCompletion {
        job,
        outcome: Ok(aterm_update::SupersededActivation {
            activation_build: 11,
            restored_build: 10,
            newer_build: 12,
            trial_launches: installed.trial_launches,
            trial_disarm_deferred: Some(
                "build 11's boot trial could not be disarmed (Permission denied); the next \
                 boot apply finishes it"
                    .to_string(),
            ),
        }),
        facts: Some(reconcile_facts_with_installed(
            3,
            3,
            Some(status(Some(12), 0)),
            Some(InstalledUpdate {
                build: 10,
                trial_launches: 0,
                ..installed.clone()
            }),
        )),
    });
    assert_eq!(
        app.auto_apply_stranded_announced, None,
        "no notice that the release is held"
    );
    #[cfg(target_os = "macos")]
    assert_eq!(
        aterm_update::apply_lane_report(10).map(|report| report.last_refusal),
        refusal_before,
        "`update status` records no refusal"
    );
    assert!(app.auto_apply_manual_only.is_none(), "the latch went");
    assert_eq!(
        app.auto_apply_intent.map(|intent| intent.build),
        Some(12),
        "the lane arms the newer build"
    );
}

/// A newer release already on disk at convergence supersedes at the first
/// look: the first observation after convergence starts the retire, though
/// nothing about the release changed.
#[test]
fn a_newer_release_already_on_disk_at_convergence_supersedes_at_the_first_look() {
    let _ledger = crate::app_update_screen::hold_update_ledger_for_test();
    let mut app = App::headless_for_test();
    let installed = installed_update(11);
    let _ = app.reconcile_native_update_facts(reconcile_facts_with_installed(
        1,
        1,
        Some(status(Some(12), 0)),
        Some(installed.clone()),
    ));
    assert_activation_stage(&app, 11, &installed.commit);
    let activation =
        crate::native_updater_service::installed_activation_digest(11, &installed.commit);
    for _ in 0..STRUCTURAL_FAILURE_LIFETIME_ATTEMPTS {
        let _ = fail_the_activation_structurally(&mut app, &installed, &activation);
    }
    assert_eq!(
        app.auto_apply_structural_verdict
            .map(|verdict| verdict.newer_spent),
        Some(0),
        "no release has been offered the install path yet"
    );
    let _ = app.reconcile_native_update_facts(reconcile_facts_with_installed(
        2,
        2,
        Some(status(Some(12), 0)),
        Some(installed.clone()),
    ));
    assert_eq!(
        app.native_activation_supersede
            .as_ref()
            .map(|job| job.newer),
        Some(12),
        "the release on disk takes the activation's place"
    );
}

/// With no newer release the latch holds, with no press: a converged activation latch whose
/// re-sample the boot trial cannot afford has no deadline, and no start of the build may be
/// offered (its next launch is the one the trial reverts on). The staged posture (computed as a
/// windowed process would, with the handoff available) is a record with no `Install now`, a press
/// is refused and launches nothing, and the refusal is what `update status` reads.
// The bundle apply lane and its ledger (`aterm_update::apply_lane_report`) are
// macOS's: a Linux update replaces the executable on disk and hands nothing off.
#[cfg(target_os = "macos")]
#[test]
fn a_converged_activation_latch_with_no_newer_release_is_said_and_left_to_a_newer_release() {
    use crate::update_words::ApplyPosture;
    let _ledger = crate::app_update_screen::hold_update_ledger_for_test();
    let spent_trial = InstalledUpdate {
        trial_launches: BOOT_TRIAL_LAUNCH_LIMIT - 1,
        ..installed_update(11)
    };
    let mut app = App::headless_for_test();
    let _ = converge_structurally_on_the_activation(&mut app, &spent_trial, 1);
    // The hold itself (no deadline, no supersede without a newer release) is
    // `native_updater_conformance`'s structural latch decision.
    observe_after_the_attempt(&mut app, &spent_trial, 2);
    let posture = app.apply_posture_with(
        11,
        crate::app_update_screen::ApplyPostureEnv {
            handoff_unavailable: None,
            auto_apply: crate::app_config::AutoApplySetting::On,
            nudge_seam: false,
        },
    );
    assert_eq!(posture, ApplyPosture::TrialSpent);
    assert_eq!(
        app.apply_retry_for(Some(11)),
        crate::update_apply_trouble::ApplyRetry::NewerRelease
    );
    assert!(
        app.update_snapshot(false).projection().staged_on_hold,
        "Settings reads the same hold"
    );
    // A control `update apply` is refused and nothing launches: through its reconcile no row
    // offers `Install now`, and `apply_refusal=` says it is on hold.
    let what = app
        .native_updater_service
        .snapshot()
        .staged
        .as_ref()
        .map(|staged| crate::update_words::staged_build_name(staged.build, &staged.version))
        .expect("the activation is staged");
    // The ready row `[update] auto_apply = false` raises for the build (the
    // posture law ranks the config before the hold): the refusal must not
    // leave it, or raise it again, offering `Install now`.
    app.staged_said = None;
    app.ensure_staged_decision_in(11, ApplyPosture::ManualByConfig);
    assert!(
        app.staged_update_row().is_some(),
        "PRECONDITION: a ready row offers the press"
    );
    assert!(
        matches!(
            app.apply_native_update(ApplyMode::Immediate),
            UpdateOutcome::Blocked { .. }
        ),
        "the press would start the launch the trial reverts on"
    );
    app.finish_native_update_reconcile(
        NativeUpdateReconcilePurpose::ApplyControl,
        reconcile_facts_with_installed(3, 3, Some(status(None, 0)), Some(spent_trial.clone())),
    );
    assert!(
        app.native_updater_service.snapshot().active.is_none(),
        "nothing launched"
    );
    for live in app.messages.live_rows() {
        assert!(
            !live
                .msg
                .actions
                .iter()
                .any(|a| matches!(a, aterm_messages::Intent::ApplyUpdate { .. })),
            "{:?}",
            live.msg
        );
    }
    let refusal =
        aterm_update::apply_lane_report(app.native_updater_service.snapshot().current_build)
            .expect("the ledger reads")
            .last_refusal;
    assert_eq!(refusal, crate::update_words::trial_spent_refusal(&what));
    // A press on the row — the Version menu's, the palette's — opens the
    // details and starts nothing: no apply outcome, the Software Update
    // page on screen.
    assert_eq!(
        app.apply_update_or_details_with_facts(true, false),
        None,
        "a press starts no launch of a build on hold"
    );
    let (_, view) = app
        .active_native_view(WindowId(0))
        .expect("the Software Update page");
    assert!(matches!(
        app.native_runtime.view_state(view),
        Some(crate::native_app::AppViewState::Settings(state))
            if state.route == crate::native_settings::SettingsRoute::SoftwareUpdate
    ));
    // An unmeasured count is not a spent one: the menu stays the remedy.
    app.native_installed_trial = None;
    assert!(!app.latched_activation_trial_spent(11));
    assert_eq!(
        app.apply_retry_for(Some(11)),
        crate::update_apply_trouble::ApplyRetry::ManualOnly
    );
    assert_activation_stage(&app, 11, &spent_trial.commit);
}

/// A structural re-sample that fails the same way says so again, though the
/// convergence already said "re-sampling": the stopped lane's notice and its
/// decision row with the press. (The lapse, the re-arm and the re-latch with
/// nothing owed are `native_updater_conformance`'s structural latch arc.)
#[test]
fn a_failed_structural_resample_is_said_again_with_its_decision_row() {
    let _ledger = crate::app_update_screen::hold_update_ledger_for_test();
    let mut app = App::headless_for_test();
    let installed = installed_update(11);
    let activation = converge_structurally_on_the_activation(&mut app, &installed, 1);
    observe_after_the_attempt(&mut app, &installed, 2);
    let_the_day_pass(&mut app);
    assert!(app.lapse_expired_auto_apply_manual_only());
    assert!(app.arm_native_auto_apply(11, &activation));

    // It fails the same way: done with these bytes, and said so again.
    let outcome = fail_the_activation_structurally(&mut app, &installed, &activation);
    app.surface_update_apply_outcome("automatic handoff", outcome, false);
    assert_eq!(
        app.auto_apply_stranded_announced,
        Some((11, false)),
        "the lane that stopped says so, though it said re-sampling before"
    );
    assert!(
        app.messages
            .live_by_key(crate::update_words::KEY_OUTCOME)
            .is_some_and(|l| l
                .msg
                .actions
                .contains(&aterm_messages::Intent::ApplyUpdate { build: 11 })),
        "the stopped lane's decision row carries the press"
    );
}

/// A night asleep counts toward the day: an `Instant` stops while a Mac sleeps, so the event
/// loop's first turn after a sleep brings the re-sample's deadline forward by the time slept. A
/// wall clock stepped back brings nothing.
#[test]
fn a_structural_resample_counts_the_night_the_mac_slept() {
    let _ledger = crate::app_update_screen::hold_update_ledger_for_test();
    let mut app = App::headless_for_test();
    let installed = installed_update(11);
    let _ = converge_structurally_on_the_activation(&mut app, &installed, 1);
    observe_after_the_attempt(&mut app, &installed, 2);
    let now = std::time::Instant::now();
    let stepped_back = std::time::SystemTime::now() - STRUCTURAL_RESAMPLE_AFTER;
    app.count_sleep_toward_update_deadlines(now, std::time::SystemTime::now());
    app.count_sleep_toward_update_deadlines(now, stepped_back);
    assert!(
        !app.lapse_expired_auto_apply_manual_only(),
        "CONTROL: a wall clock stepped back a day brings nothing"
    );
    // The lid closes and opens a day later: the wall clock moved a day, the
    // monotonic clock not at all.
    app.count_sleep_toward_update_deadlines(now, stepped_back + STRUCTURAL_RESAMPLE_AFTER);
    assert!(
        app.lapse_expired_auto_apply_manual_only(),
        "a day asleep is a day: the re-sample releases the latch"
    );
    assert_eq!(
        app.auto_apply_structural_verdict
            .map(|verdict| verdict.resample_at),
        Some(None),
        "the ONE re-sample is spent"
    );
}

/// A structural latch whose count no observation has measured spends nothing when its day comes
/// due and moves its deadline off the past, so the event loop does not spin on it. (The count's
/// own decision, the trial-spent hold and the supersede that goes ahead whatever the count says
/// are `native_updater_conformance`'s structural latch arc.)
#[test]
fn an_unmeasured_structural_latch_spends_nothing_and_moves_its_deadline() {
    let _ledger = crate::app_update_screen::hold_update_ledger_for_test();
    let mut app = App::headless_for_test();
    let installed = installed_update(11);
    let _ = converge_structurally_on_the_activation(&mut app, &installed, 1);
    app.native_installed_trial = None;
    let_the_day_pass(&mut app);
    let now = std::time::Instant::now();
    assert!(!app.lapse_expired_auto_apply_manual_only());
    assert!(
        app.auto_apply_manual_only
            .and_then(|manual| manual.retry_at)
            .is_some_and(|at| at > now),
        "a look that could not measure moves the deadline, or the event loop \
         wakes on it forever"
    );
    assert!(
        app.auto_apply_structural_verdict
            .is_some_and(|verdict| verdict.resample_at.is_some()),
        "and spends nothing"
    );
}

/// A count read before the build's latest launch says nothing about the next one: every attempt
/// at the installed activation (a Version-menu press included) launches it, and whether that
/// launch stays counted is decided after the completion (`forgives_the_counted_trial_launch`), so
/// a count held from before may be one launch low. The day coming due right after a person's
/// attempt waits for a count observed after the attempt returned;
/// `a_trial_count_read_mid_attempt_never_authorizes_the_next` covers facts replayed from
/// mid-attempt.
#[test]
fn a_trial_count_read_before_the_latest_attempt_never_authorizes_the_next() {
    let _ledger = crate::app_update_screen::hold_update_ledger_for_test();
    // One launch already counted and kept, so one launch of room is left
    // before the revert (1 + 1 < 3, and 2 + 1 is the revert).
    let one_kept = InstalledUpdate {
        trial_launches: 1,
        ..installed_update(11)
    };
    let two_kept = InstalledUpdate {
        trial_launches: BOOT_TRIAL_LAUNCH_LIMIT - 1,
        ..installed_update(11)
    };

    // The day, right behind a person's attempt.
    let mut app = App::headless_for_test();
    let activation = converge_structurally_on_the_activation(&mut app, &one_kept, 1);
    observe_after_the_attempt(&mut app, &one_kept, 2);
    assert_eq!(
        app.native_installed_trial,
        Some((11, 1)),
        "PRECONDITION: measured with room"
    );
    let _ = fail_the_activation_by_hand(&mut app, &one_kept, &activation);
    let_the_day_pass(&mut app);
    assert!(
        !app.lapse_expired_auto_apply_manual_only(),
        "the count read before build 11's latest launch cannot vouch for the next"
    );
    assert!(
        app.auto_apply_structural_verdict
            .is_some_and(|verdict| verdict.resample_at.is_some()),
        "and the re-sample is not spent on a count nobody has read"
    );
    // The observation after the attempt reads the launch it kept: held, said.
    let _ = app.reconcile_native_update_facts(reconcile_facts_with_installed(
        3,
        3,
        Some(status(None, 0)),
        Some(two_kept.clone()),
    ));
    assert_eq!(
        app.auto_apply_manual_only.map(|manual| manual.retry_at),
        Some(None),
        "measured after the attempt: one more launch would be the revert, so it is held"
    );
}

/// One failing attempt at the activation of `installed` by a person (the
/// Version menu), returned through the shipping completion lane: it charges
/// the automatic lane nothing, but it launched the build.
fn fail_the_activation_by_hand(
    app: &mut App,
    installed: &InstalledUpdate,
    activation: &str,
) -> UpdateOutcome {
    let ticket = crate::native_updater_service::ApplyAttemptTicket::for_test(
        installed.build,
        &installed.commit,
        activation,
    );
    ticket.make_current_apply_for_test(&mut app.native_updater_service);
    app.abort_reaped_native_apply_before_reconcile(
        &ticket,
        "overlap handoff failed safely: handoff proof ended AdoptionMismatch".to_string(),
        HandoffFailureLane::Manual,
    )
}

/// The second arc of
/// [`a_trial_count_read_before_the_latest_attempt_never_authorizes_the_next`]:
/// facts read while a person's attempt was in flight are replayed as
/// `Deferred` once it returns, before the worker's post-failure observation,
/// and their count predates the launch; the re-sample is not released off it.
#[test]
fn a_trial_count_read_mid_attempt_never_authorizes_the_next() {
    let _ledger = crate::app_update_screen::hold_update_ledger_for_test();
    let one_kept = InstalledUpdate {
        trial_launches: 1,
        ..installed_update(11)
    };
    let mut app = App::headless_for_test();
    let activation = converge_structurally_on_the_activation(&mut app, &one_kept, 1);
    observe_after_the_attempt(&mut app, &one_kept, 2);
    let ticket = crate::native_updater_service::ApplyAttemptTicket::for_test(
        11,
        &one_kept.commit,
        &activation,
    );
    ticket.make_current_apply_for_test(&mut app.native_updater_service);
    app.finish_native_update_reconcile(
        NativeUpdateReconcilePurpose::Refresh,
        reconcile_facts_with_installed(3, 3, Some(status(None, 0)), Some(one_kept.clone())),
    );
    assert!(
        app.deferred_native_update_reconcile.is_some(),
        "PRECONDITION: facts landing mid-attempt are held"
    );
    let _ = app.abort_reaped_native_apply_before_reconcile(
        &ticket,
        "overlap handoff failed safely: handoff proof ended AdoptionMismatch".to_string(),
        HandoffFailureLane::Manual,
    );
    app.finish_deferred_native_update_reconcile();
    assert_eq!(
        app.native_installed_trial, None,
        "a count read before the attempt returned measures nothing"
    );
    let_the_day_pass(&mut app);
    assert!(
        !app.lapse_expired_auto_apply_manual_only(),
        "so the day's re-sample is not released off it"
    );
    assert!(
        app.auto_apply_structural_verdict
            .is_some_and(|verdict| verdict.resample_at.is_some()),
        "and nothing is spent"
    );
}

/// The shared counter is shape-blind: it counts physical failures for these
/// bytes, so an artifact that already cost round trips does not buy a fresh
/// pair by failing in a new way.
#[test]
fn a_structural_failure_inherits_the_round_trips_the_artifact_already_cost() {
    let mut app = App::headless_for_test();
    let build = app.native_updater_service.snapshot().current_build + 1;
    let dmg = [0xab_u8; 32];

    assert!(matches!(
        app.spend_physical_failure_budget(build, dmg, false, PhysicalFailureShape::Transient),
        PhysicalFailureSchedule::Retry(_)
    ));
    assert!(matches!(
        app.spend_physical_failure_budget(build, dmg, false, PhysicalFailureShape::Transient),
        PhysicalFailureSchedule::Retry(_)
    ));
    let converged =
        app.spend_physical_failure_budget(build, dmg, false, PhysicalFailureShape::Structural);
    assert!(
        matches!(
            converged,
            PhysicalFailureSchedule::Converged {
                resample_at: Some(_)
            }
        ),
        "two round trips are already spent on these bytes and the structural \
         budget is {STRUCTURAL_FAILURE_LIFETIME_ATTEMPTS}; a `Retry` here would \
         mean the structural verdict RESET the artifact's history — it \
         converges, onto its one re-sample a day out: {converged:?}"
    );
}

/// The ledger reason the submission-time arm reads back from a pre-park check
/// that ran out of the apply budget, spelled as the verifier spells it, with
/// the key from the one constant.
fn passing_pre_park_reason() -> String {
    format!(
        "staged update failed pre-park verification: bundle policy: codesign --verify \
         (team-pinned) ran past this apply's verification budget; treating as a rejection \
         ({})",
        aterm_update::PASSING_REFUSAL_KEY
    )
}

/// Project the lane after a physical failure booked for `build` onto `NativeUpdateApplyLadder`'s
/// latch variables over `before`; `converged` is a verdict for these bytes or a deadline-less
/// latch.
fn project_passing_failure(
    before: &aterm_spec::interp::State,
    app: &App,
    build: u64,
    passing: bool,
) -> aterm_spec::interp::State {
    let latch = app
        .auto_apply_manual_only
        .filter(|latch| latch.build == build);
    let retry_at = latch.and_then(|latch| latch.retry_at);
    let verdict = app
        .auto_apply_structural_verdict
        .is_some_and(|verdict| verdict.build == build);
    let mut state = before.clone();
    state.insert("latched", i64::from(latch.is_some()));
    state.insert(
        "failures",
        i64::from(app.auto_apply_physical_retry.is_some()),
    );
    state.insert("passing", i64::from(passing));
    state.insert(
        "due",
        i64::from(retry_at.is_some_and(|at| at <= std::time::Instant::now())),
    );
    state.insert(
        "converged",
        i64::from(verdict || (latch.is_some() && retry_at.is_none())),
    );
    state
}

/// Tier-1: the real booking of a passing pre-park refusal is the ladder model's `PassingFailure`.
/// The submission-time arm's two steps (`cached_refusal_shape`, then
/// `latch_after_physical_failure`) run twice, the structural lane's lifetime: the first booking
/// projects onto the model's post-`PassingFailure` state and the second still has a deadline.
/// Negative control: the same reason booked `Structural` converges on its second failure,
/// projecting onto the mutant `PassingFailureConverges`, which `APassingFailureNeverConverges`
/// catches.
#[test]
fn real_passing_refusal_keeps_a_deadline_as_the_model_s_passing_failure() {
    let _ledger = crate::app_update_screen::hold_update_ledger_for_test();
    let model = aterm_models_update::native_update_apply_ladder_model();
    let mut keys_only = model.init_state();
    for _ in 0..2 {
        keys_only = model.successors("Advance", &keys_only).remove(0);
    }
    let reason = passing_pre_park_reason();
    let passing = aterm_update::is_passing_refusal(&reason);
    assert!(passing, "PRECONDITION: the reason carries the passing key");

    let mut app = App::headless_for_test();
    let build = app.native_updater_service.snapshot().current_build + 1;
    let dmg = [0xab_u8; 32];
    let shape = PhysicalFailureShape::cached_refusal_shape(Some(&reason));
    let first = app.latch_after_physical_failure(build, dmg, false, shape, &reason);
    let projected = project_passing_failure(&keys_only, &app, build, passing);
    assert_eq!(
        model.successors("PassingFailure", &keys_only),
        vec![projected.clone()],
        "the real booking must be exactly the model's PassingFailure: {first:?}"
    );
    for invariant in &model.invariants {
        assert!(model.check_invariant(invariant.name, &projected));
    }
    let second = app.latch_after_physical_failure(build, dmg, false, shape, &reason);
    assert!(
        matches!(second, PhysicalFailureSchedule::Retry(_)),
        "the structural lane's whole lifetime later, a moment still has a deadline: \
         {second:?}"
    );
    assert_eq!(
        project_passing_failure(&keys_only, &app, build, passing),
        projected,
        "a second moment is still the model's PassingFailure latch, never a verdict"
    );

    // Negative control: the same reason booked as structural.
    let mut old = App::headless_for_test();
    for _ in 0..2 {
        let _ = old.latch_after_physical_failure(
            build,
            dmg,
            false,
            PhysicalFailureShape::Structural,
            &reason,
        );
    }
    let held = project_passing_failure(&keys_only, &old, build, passing);
    let buggy = aterm_spec::interp::with_buggy(&model, 1);
    assert_eq!(
        buggy.successors("PassingFailureConverges", &projected),
        vec![held.clone()],
        "the old booking is exactly the mutant: the latch converged on a moment"
    );
    assert!(!buggy.check_invariant("APassingFailureNeverConverges", &held));
}

/// A cold relaunch that keeps being refused converges, like any failure inside one process. Each
/// relaunch is a new process, so the budget is carried across them; otherwise a boot swap that
/// keeps refusing for a reason the pre-park read cannot see (an apply lock held by a stopped
/// sibling, a candidate that cannot start) is booked as the first failure every time. Each
/// "process" here is a fresh `App` seeded from what the last one carried (`cold_lane_budget` →
/// the environment → `ColdRelaunch` → `seed_refused_cold_relaunch`), its first automatic attempt
/// answered by the refusal (`cold_lane_verdict_gate`) and booked as the synchronous arm books it.
///
/// A verdict converges on its second refusal and, its one re-sample spent, for good; a passing
/// moment climbs the transient schedule to its ninth. Negative control: with nothing carried,
/// every relaunch is booked at the first rung.
#[cfg(unix)]
#[test]
fn a_refused_cold_relaunch_converges_across_its_own_relaunches() {
    use crate::app_update_handoff::ColdRelaunch;
    use std::ffi::OsStr;
    let _ledger = crate::app_update_screen::hold_update_ledger_for_test();
    let running = crate::build_info::build_number();
    let build = crate::running_build_number().saturating_add(1);
    let build_text = build.to_string();
    let artifact = "ab".repeat(32);
    let dmg = [0xab_u8; 32];
    let ticket = crate::native_updater_service::ApplyAttemptTicket::for_test(
        build,
        PREFLIGHT_TEST_COMMIT,
        &artifact,
    );
    let label = |schedule: PhysicalFailureSchedule| match schedule {
        PhysicalFailureSchedule::Retry(_) | PhysicalFailureSchedule::StandDown(_) => "retry",
        PhysicalFailureSchedule::Converged { resample_at: None } => "done",
        PhysicalFailureSchedule::Converged {
            resample_at: Some(_),
        } => "converged",
    };
    // One relaunch: a new process, seeded from `carried`, whose boot swap refused with `reason`.
    let relaunch = |carried: Option<&str>, reason: &str| {
        let mut app = App::headless_for_test();
        let seed = ColdRelaunch::from_values(
            Some(OsStr::new(running)),
            Some(OsStr::new(&build_text)),
            Some(OsStr::new(PREFLIGHT_TEST_COMMIT)),
            Some(OsStr::new(&artifact)),
            carried.map(OsStr::new),
            running,
        )
        .and_then(|cold| cold.seed(&aterm_update::ApplyOutcome::Deferred(reason.to_string())))
        .expect("the cold lane's relaunch, refused");
        app.seed_refused_cold_relaunch(seed, std::time::Instant::now());
        let error = app
            .cold_lane_verdict_gate(ApplyMode::AutomaticPastGrace, Some(&ticket))
            .expect_err("the refusal answers the first attempt: no relaunch");
        let cached = app.cached_handoff_preverification_reason(&ticket);
        let shape = PhysicalFailureShape::cached_refusal_shape(cached.as_deref());
        let schedule =
            app.latch_after_physical_failure(build, dmg, false, shape, &error.to_string());
        (app, label(schedule))
    };
    let carry = |app: &App| {
        app.cold_lane_budget(&ticket, std::time::Instant::now())
            .encode(build, &artifact)
    };

    // A verdict: a candidate that cannot start.
    let cannot_start = "staged candidate cannot start: dyld refused the binary";
    let (first, rung) = relaunch(None, cannot_start);
    assert_eq!(rung, "retry", "the one confirming retry");
    let (mut second, rung) = relaunch(Some(&carry(&first)), cannot_start);
    assert_eq!(
        rung, "converged",
        "the second refusal converges: a day's re-sample"
    );
    // The day passes and the re-sample is released, as it is in one process.
    let past = std::time::Instant::now()
        .checked_sub(std::time::Duration::from_secs(1))
        .expect("the clock has run a second");
    if let Some(latch) = second.auto_apply_manual_only.as_mut() {
        latch.retry_at = Some(past);
    }
    if let Some(verdict) = second.auto_apply_structural_verdict.as_mut() {
        verdict.resample_at = Some(past);
    }
    assert!(
        second.lapse_expired_auto_apply_manual_only(),
        "the re-sample is released"
    );
    let (third, rung) = relaunch(Some(&carry(&second)), cannot_start);
    assert_eq!(rung, "done", "a spent re-sample that fails again is final");
    assert!(
        third
            .auto_apply_manual_only
            .is_some_and(|latch| latch.retry_at.is_none()),
        "no deadline left, so no relaunch"
    );

    // A passing moment: the apply lock held past its wait, every time.
    let moment = passing_pre_park_reason();
    let mut carried: Option<String> = None;
    let mut rungs = Vec::new();
    for _ in 0..PHYSICAL_FAILURE_LIFETIME_ATTEMPTS {
        let (app, rung) = relaunch(carried.as_deref(), &moment);
        rungs.push(rung);
        carried = Some(carry(&app));
    }
    let (last, schedule) = rungs.split_last().expect("the lifetime is not empty");
    assert!(
        *last == "converged" && schedule.iter().all(|rung| *rung == "retry"),
        "the whole transient schedule, across its relaunches, then convergence: {rungs:?}"
    );

    // Negative control: nothing carried, every relaunch is a first failure, so neither shape ever
    // converges.
    for _ in 0..3 {
        assert_eq!(relaunch(None, cannot_start).1, "retry");
        assert_eq!(relaunch(None, &moment).1, "retry");
    }
}

/// A physical-failure latch lapsing keeps the ladder's anchor: the landing bound counts from the
/// first arming, so the lapse re-arms the intent where the clock is. Driven through the real
/// completion lane and the Refresh reconcile's `arm_native_auto_apply`, which lapses the latch.
#[test]
fn a_physical_latch_lapse_resumes_the_ladder_where_the_clock_left_it() {
    use crate::native_update_auto_intent::{ApplyPhase, LANDS_WITHIN};
    let _ledger = crate::app_update_screen::hold_update_ledger_for_test();
    let mut app = App::headless_for_test();
    let build = app.native_updater_service.snapshot().current_build + 1;
    let digest = "ab".repeat(32);
    assert!(
        app.arm_native_auto_apply(build, &digest),
        "PRECONDITION: automatic apply is enabled and armable for these bytes"
    );
    let armed_at = std::time::Instant::now() - LANDS_WITHIN - std::time::Duration::from_secs(30);
    app.auto_apply_ladder = Some(crate::AutoApplyLadder {
        build,
        armed_at,
        announced: ApplyPhase::KeysOnly,
        restored_hold_said: false,
    });

    // One transient physical failure (the rendezvous dial never arrived).
    let ticket = crate::native_updater_service::ApplyAttemptTicket::for_test(
        build,
        PREFLIGHT_TEST_COMMIT,
        &digest,
    );
    ticket.make_current_apply_for_test(&mut app.native_updater_service);
    app.abort_reaped_native_apply_before_reconcile(
        &ticket,
        "overlap handoff failed safely: handoff proof ended TimedOut".to_string(),
        HandoffFailureLane::Physical(PhysicalFailureShape::Transient),
    );
    let latch = app
        .auto_apply_manual_only
        .expect("a returned physical failure latches manual-only");
    assert!(
        latch.retry_at.is_some(),
        "a first transient failure schedules a retry"
    );
    assert!(
        app.auto_apply_intent.is_none(),
        "the intent is dropped under the latch"
    );
    assert_eq!(
        app.auto_apply_ladder.map(|ladder| ladder.armed_at),
        Some(armed_at),
        "a physical failure does not touch the anchor"
    );

    // The latch lapses: expire the deadline, then take the Refresh
    // reconcile's re-arm.
    app.auto_apply_manual_only = Some(crate::AutoApplyManualOnly {
        retry_at: Some(std::time::Instant::now() - std::time::Duration::from_secs(1)),
        ..latch
    });
    assert!(
        app.arm_native_auto_apply(build, &digest),
        "the lapsed latch re-arms the intent for the same bytes"
    );
    assert!(
        app.auto_apply_manual_only.is_none(),
        "the latch is released"
    );
    assert!(app.auto_apply_intent.is_some(), "a live intent again");
    assert_eq!(
        app.auto_apply_ladder.map(|ladder| ladder.armed_at),
        Some(armed_at),
        "THE MUTANT: a lapse that clears the anchor starts a second ladder for \
         the same artifact"
    );
}

#[cfg(unix)]
/// A candidate that fails after booting has already swapped the installed
/// bundle, so the retry for the same update arrives as an activation with a
/// different digest. The budget counts attempts at the logical update, so
/// the schedule continues across the flip. Each retry re-arms a live intent
/// and clears a manual-only latch; different bytes of the same build start a
/// fresh spacing.
#[test]
fn the_activation_of_a_failed_download_continues_its_ladder_instead_of_refilling_it() {
    let rung = |n: u8| automatic_retry_delay(n, AutomaticRetryKind::ActivityRevoked);
    const COMMIT: &str = "0123456789abcdef0123456789abcdef01234567";
    let mut app = App::headless_for_test();
    let download =
        crate::native_updater_service::ApplyAttemptTicket::for_test(77, COMMIT, &"ab".repeat(32));
    let activation = crate::native_updater_service::ApplyAttemptTicket::for_test(
        77,
        COMMIT,
        &crate::native_updater_service::installed_activation_digest(77, COMMIT),
    );
    assert!(
        activation.is_installed_activation() && !download.is_installed_activation(),
        "the fixture must actually straddle the swap"
    );
    app.auto_apply_manual_only = Some(crate::AutoApplyManualOnly {
        build: 77,
        dmg_sha256: [0xab; 32],
        activation: false,
        retry_at: None,
    });

    assert_eq!(
        app.arm_activity_revoked_overlap_retry(&download),
        rung(0),
        "rung 1, as a download"
    );
    assert_eq!(
        app.auto_apply_intent
            .map(|intent| (intent.build, intent.dmg_sha256)),
        Some((77, [0xab; 32])),
        "the retry re-arms a live intent for these bytes"
    );
    assert!(
        app.auto_apply_manual_only.is_none(),
        "a live retry clears the manual-only latch"
    );
    assert_eq!(
        app.arm_activity_revoked_overlap_retry(&download),
        rung(1),
        "rung 2, still a download"
    );

    // The candidate swapped the bundle before it failed. Same update, new
    // artifact identity: the ladder continues.
    assert_eq!(
        app.arm_activity_revoked_overlap_retry(&activation),
        rung(2),
        "rung 3 across the flip — not a refilled first rung"
    );

    for (cycle, want) in [rung(3), rung(4), rung(4)].into_iter().enumerate() {
        assert_eq!(
            app.arm_activity_revoked_overlap_retry(&activation),
            want,
            "post-flip cycle {cycle}"
        );
    }
    // The last rung repeats rather than ending anything.
    assert_eq!(
        app.auto_overlap_retry.map(|retry| retry.cycles),
        Some(ACTIVITY_REVOKED_LADDER_RUNGS + 1),
        "six attempts total, not five per artifact identity"
    );
    assert!(
        app.auto_overlap_retry.is_some_and(|retry| retry.activation),
        "and the record now names the side of the swap it was last spent on"
    );

    // A different download (same build, new bytes) starts a fresh spacing.
    let mut app = App::headless_for_test();
    let other =
        crate::native_updater_service::ApplyAttemptTicket::for_test(77, COMMIT, &"cd".repeat(32));
    assert_eq!(app.arm_activity_revoked_overlap_retry(&download), rung(0));
    assert_eq!(app.arm_activity_revoked_overlap_retry(&download), rung(1));
    assert_eq!(app.arm_activity_revoked_overlap_retry(&other), rung(0));
}

/// The physical ladder carries the same key and the same flip.
#[test]
fn a_physical_failure_ladder_survives_the_download_to_activation_flip() {
    let mut app = App::headless_for_test();
    let build = 77;
    let dmg = [0xab_u8; 32];
    let activation_dmg = [0xcd_u8; 32];

    for cycle in 0..PHYSICAL_FAILURES_PER_EPOCH {
        let _ =
            app.spend_physical_failure_budget(build, dmg, false, PhysicalFailureShape::Transient);
        assert_eq!(
            app.auto_apply_physical_retry.map(|r| r.cycles),
            Some(cycle + 1),
            "download cycle {cycle}"
        );
    }

    // Same logical update, now an activation: the count continues.
    let _ = app.spend_physical_failure_budget(
        build,
        activation_dmg,
        true,
        PhysicalFailureShape::Transient,
    );
    assert_eq!(
        app.auto_apply_physical_retry.map(|r| r.cycles),
        Some(PHYSICAL_FAILURES_PER_EPOCH + 1),
        "the flip must not refill the lifetime counter"
    );
}

/// A latch with a deadline lapses, restoring automatic apply and the artifact's activity budget;
/// a deadline-less one (the policy fail-safe, an environment block, a spent structural re-sample)
/// does not. The lapse reads `retry_at` and nothing else.
///
/// The lapse never hands back the PHYSICAL budget: that counter must outlive
/// both the latch (cleared here) and the intent (re-armed at `attempts: 0`),
/// or "two tries" is an unbounded ten-minute loop.
#[test]
fn a_deadlined_manual_only_latch_lapses_but_the_fail_safe_one_does_not() {
    let mut app = App::headless_for_test();
    let build = app.native_updater_service.snapshot().current_build + 1;

    // Fail-safe latch: no deadline, never lapses, budget untouched.
    app.auto_apply_manual_only = Some(crate::AutoApplyManualOnly {
        build,
        dmg_sha256: [0xab; 32],
        activation: false,
        retry_at: None,
    });
    assert!(!app.lapse_expired_auto_apply_manual_only());
    assert!(app.auto_apply_manual_only.is_some());

    // Activity-shaped, deadline passed: lapses, and the artifact's retry
    // budget starts over so the next attempt is not instantly exhausted.
    app.auto_overlap_retry = Some(crate::AutoOverlapRetry {
        build,
        dmg_sha256: [0xab; 32],
        activation: false,
        cycles: ACTIVITY_REVOKED_LADDER_RUNGS,
        last_attempt: std::time::Instant::now(),
    });
    let spent = crate::AutoOverlapRetry {
        build,
        dmg_sha256: [0xab; 32],
        activation: false,
        cycles: MAX_PHYSICAL_FAILURE_CYCLES,
        last_attempt: std::time::Instant::now(),
    };
    app.auto_apply_physical_retry = Some(spent);
    app.auto_apply_manual_only = Some(crate::AutoApplyManualOnly {
        build,
        dmg_sha256: [0xab; 32],
        activation: false,
        retry_at: Some(std::time::Instant::now() - std::time::Duration::from_secs(1)),
    });
    assert!(app.lapse_expired_auto_apply_manual_only());
    assert!(app.auto_apply_manual_only.is_none());
    assert!(
        app.auto_overlap_retry.is_none(),
        "a lapsed latch replenishes the activity retry budget"
    );
    assert_eq!(
        app.auto_apply_physical_retry.map(|retry| retry.cycles),
        Some(spent.cycles),
        "but never the physical budget it gated"
    );
    // …and automatic apply is armable again for the same artifact.
    assert!(app.arm_native_auto_apply(build, &"ab".repeat(32)));
}

const PREFLIGHT_TEST_COMMIT: &str = "0123456789abcdef0123456789abcdef01234567";

/// Drive the real check reducer to a Staged phase for `build`, as a completed updater worker
/// does, so the intent under test is the one production arms. The pre-park verdict is pre-seeded
/// `passed: true` so `arm_native_auto_apply` skips its worker thread; a `passed: false` fixture
/// would describe an artifact `start_unix_update_handoff` declines before any reader parks.
fn stage_one_build_for_test(app: &mut App, build: u64) {
    #[cfg(unix)]
    {
        *app.handoff_preverified
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner) =
            Some(crate::HandoffPreverification {
                build,
                commit: PREFLIGHT_TEST_COMMIT.to_string(),
                artifact: "ab".repeat(32),
                at: std::time::Instant::now(),
                passed: true,
                reason: None,
                policy: None,
                grant_chunks: false,
            });
    }
    let current_build = app.native_updater_service.snapshot().current_build;
    assert!(
        build > current_build,
        "the staged build must supersede the running one or nothing is armable"
    );
    let CheckStart::Start(ticket) = app.native_updater_service.request_check() else {
        panic!("a fresh service must start exactly one check");
    };
    // The reducer and `arm_native_auto_apply` run for real; only the presentation fan-out
    // `App::finish_native_update_check` adds (repaints, tab strip, palette) is skipped.
    let durable = durable_download_status(app, build);
    assert_eq!(
        app.native_updater_service.finish_check(ticket, durable),
        CheckCompletion::Reduced,
        "PRECONDITION: the check must actually reduce, or nothing is staged"
    );
    let staged = app
        .native_updater_service
        .snapshot()
        .staged
        .clone()
        .expect("PRECONDITION: the reduced check staged the build");
    assert_eq!(staged.build, build);
    assert!(
        app.arm_native_auto_apply(staged.build, &staged.dmg_sha256),
        "PRECONDITION: a strictly newer staged build arms automatic intent"
    );
}

/// The read a held press waits on: `build`'s pre-park verification, running
/// until the returned flag is raised.
#[cfg(unix)]
fn preverify_read_running(
    app: &mut App,
    build: u64,
) -> std::sync::Arc<std::sync::atomic::AtomicBool> {
    let done = std::sync::Arc::new(std::sync::atomic::AtomicBool::new(false));
    app.handoff_preverify_in_flight = Some(crate::PreverifyInFlight {
        build,
        commit: PREFLIGHT_TEST_COMMIT.to_string(),
        artifact: "ab".repeat(32),
        deadline: std::time::Instant::now() + crate::HANDOFF_PREVERIFY_READ_CEILING,
        done: std::sync::Arc::clone(&done),
        passing: Default::default(),
    });
    done
}

/// What a person's press leaves when the fork lane holds it for the candidate's read
/// ([`crate::UpdateHandoffStartError::HeldForRead`]): its installing row, its switch on record,
/// and the hold.
#[cfg(unix)]
fn press_held_for_the_read(app: &mut App, build: u64) {
    let running = app.native_updater_service.snapshot().current_build;
    app.begin_update_installing(true);
    app.record_update_switch_started(running, false, false);
    app.hold_person_apply(crate::HeldPersonApply {
        target_build: build,
        until: std::time::Instant::now() + crate::HANDOFF_PREVERIFY_READ_CEILING,
    });
}

/// A person's install held for the candidate's read runs when the read ends. With `[update]
/// auto_apply = false`, while the read runs the press stands held: its row still says it
/// installs, a second press joins it and starts nothing, and the loop looks again within the
/// poll. Once the read ends the press runs as the person's own `Immediate` apply (here reaching
/// the admission gate this process never passes), and how it ended is a person's stop on record,
/// never "tries again by itself", with a row on the band and the installing row gone.
#[cfg(unix)]
#[test]
fn a_person_s_install_held_for_the_read_runs_when_it_ends_and_says_how_it_ended() {
    let _ledger = crate::app_update_screen::hold_update_ledger_for_test();
    let mut app = App::headless_for_test();
    let build = app.native_updater_service.snapshot().current_build + 1;
    stage_one_build_for_test(&mut app, build);
    app.config.update = Some(crate::app_config::UpdateConfig {
        auto_apply: Some(false),
        ..Default::default()
    });
    app.auto_apply_intent = None;
    app.clear_messages_for_test();
    let done = preverify_read_running(&mut app, build);
    press_held_for_the_read(&mut app, build);
    let installing = app
        .update_row_text()
        .expect("PRECONDITION: the press's row");
    assert!(installing.contains("installing"), "{installing}");

    // While the read runs: held, row up, nothing else run.
    let now = std::time::Instant::now();
    app.run_held_person_apply(now);
    assert!(app.person_apply_is_held(), "held while the read runs");
    assert_eq!(app.update_row_text(), Some(installing.clone()));
    let next = app
        .held_person_apply_deadline(now)
        .expect("the loop looks again");
    assert!(
        next <= now + super::PREVERIFY_READ_POLL,
        "within the poll: the read raises only a flag"
    );
    let records = app.messages.log().records().count();
    assert_eq!(
        app.apply_native_update(ApplyMode::Immediate),
        UpdateOutcome::Accepted,
        "a second press joins the held one"
    );
    assert_eq!(
        app.native_updater_service.snapshot().phase,
        UpdaterPhase::Staged,
        "and starts nothing"
    );
    assert_eq!(app.messages.log().records().count(), records, "nor records");

    // The read ends: the press runs, and says how it ended.
    done.store(true, std::sync::atomic::Ordering::Release);
    app.run_held_person_apply(std::time::Instant::now());
    assert!(!app.person_apply_is_held());
    assert!(
        app.held_person_apply_deadline(std::time::Instant::now())
            .is_none()
    );
    assert!(
        app.native_updater_service.snapshot().error.is_some(),
        "the press ran: its attempt reached the admission gate"
    );
    let row = app.update_row_text().expect("a row says how it ended");
    assert!(
        !row.contains("installing"),
        "the installing row is gone: {row}"
    );
}

/// The held press's bounds: a read that outruns the hold's window does not keep the press (it
/// runs, as the fork lane parks once the window is spent), and a stage that moved on under the
/// hold drops it, running nothing, its row put back and its switch closed as a person's stop.
/// While it stands the automatic lane waits, so its own deferral cannot retire the press's row or
/// close its record; the intent, phase and hold read the same either way, so the row, the attempt
/// count and the record count are what this asserts.
#[cfg(unix)]
#[test]
fn a_held_press_is_bounded_by_its_window_and_its_stage_and_the_lane_waits_for_it() {
    let _ledger = crate::app_update_screen::hold_update_ledger_for_test();
    // The automatic lane waits while a press is held.
    let mut app = App::headless_for_test();
    let build = app.native_updater_service.snapshot().current_build + 1;
    stage_one_build_for_test(&mut app, build);
    let _read = preverify_read_running(&mut app, build);
    press_held_for_the_read(&mut app, build);
    let installing = app
        .update_row_text()
        .expect("PRECONDITION: the press's row");
    assert!(installing.contains("installing"), "{installing}");
    let records = app.messages.log().records().count();
    force_auto_apply_attempt_now(&mut app);
    app.try_pending_native_auto_apply(false);
    assert!(
        app.auto_apply_intent
            .is_some_and(|intent| intent.build == build),
        "the intent is kept"
    );
    assert_eq!(
        app.native_updater_service.snapshot().phase,
        UpdaterPhase::Staged,
        "and nothing was attempted beside the held press"
    );
    assert!(app.person_apply_is_held());
    assert_eq!(
        app.update_row_text(),
        Some(installing),
        "the press's installing row stands: no attempt of the lane's retired it"
    );
    assert_eq!(
        app.auto_apply_intent.map(|intent| intent.attempts),
        Some(0),
        "the lane spent no attempt beside the held press"
    );
    assert_eq!(
        app.messages.log().records().count(),
        records,
        "and closed no switch on record: nothing was recorded"
    );

    // A read still running past the hold's window: the press runs anyway.
    let held = app.person_apply_held.expect("held");
    if let Some(read) = app.handoff_preverify_in_flight.as_mut() {
        read.deadline = held.until + std::time::Duration::from_secs(60);
    }
    app.run_held_person_apply(held.until);
    assert!(!app.person_apply_is_held(), "the window bounds the hold");
    assert!(
        app.native_updater_service.snapshot().error.is_some(),
        "the press ran"
    );

    // A stage that moved on: dropped, nothing run, the switch closed.
    let mut app = App::headless_for_test();
    let build = app.native_updater_service.snapshot().current_build + 1;
    stage_one_build_for_test(&mut app, build);
    app.auto_apply_intent = None;
    app.clear_messages_for_test();
    press_held_for_the_read(&mut app, build + 1);
    app.run_held_person_apply(std::time::Instant::now());
    assert!(!app.person_apply_is_held());
    assert!(
        app.native_updater_service.snapshot().error.is_none(),
        "nothing ran for a stage that is not the one held"
    );
    assert!(
        app.update_row_text()
            .is_none_or(|row| !row.contains("installing")),
        "the installing row is put back"
    );
    assert!(
        app.update_switch_on_record.is_none(),
        "the press's switch is closed on record"
    );
}

/// A held press run again does not double up: `run_held_person_apply` runs a held press as a
/// second start of the same attempt, which finds the row installing and its switch on record
/// already. The begin keeps what the row said before the press (a refusal puts that back), and
/// the switch is one start and one stop on record. Driven through the two seams directly, since
/// the press test above stops at the admission gate before either: without
/// `begin_update_installing`'s early return the refusal leaves the row at "installing…"; without
/// `note_update_switch_started`'s `update_switch_on_record` clause a second start is recorded.
#[cfg(unix)]
#[test]
fn a_press_begun_twice_puts_its_row_back_and_is_one_switch_on_record() {
    let _ledger = crate::app_update_screen::hold_update_ledger_for_test();
    let mut app = App::headless_for_test();
    let staged = aterm_update::Progress::Staged {
        version: "9.9.9".into(),
        build: 41,
    };
    app.post_update_row(
        crate::update_words::progress(&staged, None, ""),
        "9.9.9",
        crate::messages_host::FlowPhase::Staged { build: 41 },
    );
    let ready = app.update_row_text().expect("PRECONDITION: the ready row");
    let records = app.messages.log().records().count();
    let running = app.native_updater_service.snapshot().current_build;

    // The press, then the same press run again once its read ended.
    for _ in 0..2 {
        app.begin_update_installing(true);
        app.record_update_switch_started(running, false, false);
    }
    // Its run refused: the row goes back, the switch stops.
    app.retire_update_installing();
    app.record_update_switch_stopped(false, "the handoff was refused");

    assert_eq!(
        app.update_row_text(),
        Some(ready),
        "the row from before the press is back, not the installing words"
    );
    let added = app
        .messages
        .log()
        .records()
        .skip(records)
        .map(|record| record.title.clone())
        .collect::<Vec<_>>();
    assert_eq!(
        added.len(),
        2,
        "one start and one stop on record for one press: {added:?}"
    );
}

/// The durable marker a download stage of `build` leaves on disk, as the
/// reconcile tests below feed it back.
fn durable_download_status(app: &App, build: u64) -> DurableUpdateStatus {
    DurableUpdateStatus {
        linux_host: false,
        linux: None,
        enabled: true,
        current_build: app.native_updater_service.snapshot().current_build,
        staged_build: Some(build),
        staged_version: Some(format!("1.0.{build}")),
        staged_commit: Some(PREFLIGHT_TEST_COMMIT.to_string()),
        staged_dmg_sha256: Some("ab".repeat(32)),
        changelog: None,
        outcome: "staged".to_string(),
        failing_checks: 0,
        failing_persistent: false,
        failing_kind: String::new(),
        failing_applies: 0,
        apply_failure: String::new(),
        apply_failure_build: 0,
        apply_failures_for_target: 0,
        installable: Some(true),
        channel_unreadable: false,
        checked_at: None,
    }
}

/// A retired download takes its older latches with it, carries its own to the activation with its
/// deadline, and leaves a newer one alone (a completion can land out of order, as in the
/// `Retired` arm of `reconcile_returned_native_apply_with_facts`).
#[test]
fn retiring_a_download_for_its_activation_carries_only_its_own_latch() {
    for offset in [-1_i64, 0, 1] {
        let mut app = App::headless_for_test();
        let build = app.native_updater_service.snapshot().current_build + 1;
        stage_one_build_for_test(&mut app, build);
        let latched_build = build.saturating_add_signed(offset);
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(600);
        app.auto_apply_manual_only = Some(crate::AutoApplyManualOnly {
            build: latched_build,
            dmg_sha256: [0xab; 32],
            activation: false,
            retry_at: Some(deadline),
        });

        // The survivor state a swapped-but-uncommitted handoff leaves behind:
        // the canonical bundle carries this build and its receipt names this
        // exact artifact, so the reducer retires the stage as installed.
        app.finish_native_update_reconcile(
            NativeUpdateReconcilePurpose::Startup,
            reconcile_facts_with_installed(
                11,
                11,
                Some(durable_download_status(&app, build)),
                Some(InstalledUpdate {
                    build,
                    commit: PREFLIGHT_TEST_COMMIT.to_string(),
                    version: None,
                    receipt_build: Some(build),
                    receipt_dmg_sha256: Some("ab".repeat(32)),
                    trial_launches: 0,
                }),
            ),
        );

        // The download stage retires and the activation (newer than the process) takes its
        // place, under the activation identity, not the retired DMG digest.
        let staged = app
            .native_updater_service
            .snapshot()
            .staged
            .clone()
            .expect("PRECONDITION: the newer installed bundle became an activation stage");
        assert!(
            staged.build == build && staged.is_installed_activation(),
            "the stage on record is the activation, got {staged:?}"
        );
        let activation = decode_dmg_sha256(&staged.dmg_sha256).unwrap();
        match offset {
            -1 => assert!(
                app.auto_apply_manual_only.is_none(),
                "a latch on an older build retires with the download"
            ),
            0 => assert_eq!(
                app.auto_apply_manual_only,
                Some(crate::AutoApplyManualOnly {
                    build,
                    dmg_sha256: activation,
                    activation: true,
                    retry_at: Some(deadline),
                }),
                "the latch on the update itself is carried to its activation, \
                 deadline and all"
            ),
            _ => assert_eq!(
                app.auto_apply_manual_only,
                Some(crate::AutoApplyManualOnly {
                    build: latched_build,
                    dmg_sha256: [0xab; 32],
                    activation: false,
                    retry_at: Some(deadline),
                }),
                "a latch about a newer artifact is untouched"
            ),
        }
        assert_eq!(
            app.auto_apply_intent.is_some(),
            offset == -1,
            "the activation arms only where no latch holds it (offset {offset})"
        );
    }
}

/// One Settings view holding an unsaved draft, parked in a background tab while the person works
/// in the terminal tab; returns its instance and view. A Settings draft, not `pending_restore`
/// (which the UI-less `native_update_close_preflight` finds): it is found by
/// `prepare_all_native_shutdown` → `CloseReadiness::Blocked` → `surface_native_close_recovery`,
/// the path that could switch tabs, move focus, re-front the window and open Close Recovery.
fn park_a_settings_draft_in_a_background_tab(
    app: &mut App,
) -> (crate::tab_model::AppInstanceId, crate::tab_model::ViewId) {
    let wid = WindowId(0);
    assert!(
        app.open_settings_tab(crate::native_settings::SettingsRoute::Home),
        "PRECONDITION: the Settings tab opens"
    );
    let (instance, view) = app
        .active_native_view(wid)
        .expect("PRECONDITION: the new Settings tab is the active native view");
    // The font row's text entry (what "Other…" opens), then a name typed in.
    for event in [
        crate::native_app::AppEvent::Action(crate::native_app::ActionInvocation {
            id: crate::native_ui::ActionId::new(format!(
                "settings/set/{}",
                crate::prefs::EDIT_FONT_FAMILY
            )),
            value: None,
        }),
        crate::native_app::AppEvent::TextInput(crate::native_app::TextInputEvent::SelectAll),
        crate::native_app::AppEvent::TextInput(crate::native_app::TextInputEvent::Commit(
            "Update Probe Mono".to_string(),
        )),
    ] {
        app.dispatch_native_view_event(wid, view, event)
            .expect("PRECONDITION: the Settings draft edit dispatches");
    }
    // The reducer's own verdict: `closable == false` is what `prepare_close`
    // turns into `CloseReadiness::Blocked { recovery }`, so the probe below
    // reaches the recovery-surfacing branch.
    assert!(
        !app.native_runtime
            .presentation(instance, view)
            .expect("PRECONDITION: the Settings view still presents")
            .closable,
        "PRECONDITION: the draft makes the Settings view refuse a close, which is \
         the only verdict that surfaces Close Recovery"
    );
    // Back to the terminal tab; later asserts that it stays active show no probe moved the person.
    app.switch_tab_in(wid, 0);
    assert!(
        app.active_native_view(wid).is_none(),
        "PRECONDITION: the user is on the terminal tab, not on Settings"
    );
    (instance, view)
}

/// Clear the draft through the app's recovery command, as a user would: go
/// to the Settings tab, discard, come back (so later "nothing moved"
/// assertions start from elsewhere).
fn discard_settings_drafts(app: &mut App, view: crate::tab_model::ViewId) {
    let wid = WindowId(0);
    app.switch_tab_in(wid, 1);
    assert!(
        app.active_native_view(wid)
            .is_some_and(|(_, active)| active == view),
        "the Settings tab is where the draft lives"
    );
    for _ in 0..2 {
        app.dispatch_native_view_event(
            WindowId(0),
            view,
            crate::native_app::AppEvent::Action(crate::native_app::ActionInvocation {
                id: crate::native_ui::ActionId::new("settings/drafts/discard-all"),
                value: None,
            }),
        )
        .expect("the discard-all recovery command dispatches");
    }
    assert!(
        app.native_runtime
            .presentation(
                app.active_native_view(wid).expect("still on Settings").0,
                view
            )
            .expect("the Settings view still presents")
            .closable,
        "the discard really cleared the blocker — otherwise the 'way back' \
         below would be testing the blocked path all over again"
    );
    app.switch_tab_in(wid, 0);
    assert!(
        app.active_native_view(wid).is_none(),
        "the user went back to their terminal tab"
    );
}

/// Nothing on screen moved. Called after every automatic probe.
fn assert_no_probe_disturbance(app: &App, wid: WindowId, active_tab: crate::tab_model::TabId) {
    assert!(
        app.windows[&wid].palette().is_none(),
        "a background update probe must never open a palette over the user's work"
    );
    assert!(
        app.windows[&wid].overlay.is_none(),
        "a background update probe must never install a window overlay"
    );
    assert_eq!(
        app.windows[&wid].tab_set.active_id(),
        Some(active_tab),
        "a background update probe must never switch the active tab"
    );
    assert!(
        app.active_native_view(wid).is_none(),
        "a background update probe must never move focus onto the blocking \
         native view"
    );
    assert_eq!(
        app.frontmost_window,
        Some(wid),
        "a background update probe must never change the frontmost window"
    );
}

/// Make the retained intent eligible now with its ladder at `Land`, so the
/// attempt takes the `AutomaticPastGrace` lane and no activity gate holds
/// it, whatever the machine's load.
fn force_auto_apply_attempt_now(app: &mut App) {
    let mut intent = app
        .auto_apply_intent
        .expect("an automatic intent must be armed to force an attempt");
    let now = std::time::Instant::now();
    intent.retry_at = now - std::time::Duration::from_secs(1);
    app.auto_apply_intent = Some(intent);
    app.auto_apply_ladder = Some(crate::AutoApplyLadder {
        build: intent.build,
        armed_at: now - crate::native_update_auto_intent::LANDS_WITHIN,
        announced: crate::native_update_auto_intent::ApplyPhase::Land,
        restored_hold_said: false,
    });
}

/// Drive the automatic lane through one whole preflight-block budget against
/// whatever blocker the caller has already installed.
fn spend_one_preflight_block_budget(app: &mut App) {
    for attempt in 1..=u32::from(MAX_AUTOMATIC_UPDATE_CYCLES) {
        force_auto_apply_attempt_now(app);
        app.try_pending_native_auto_apply(false);
        // Precondition: a `Wait` would leave `attempts` untouched, so this
        // proves the preflight ran and came back `Blocked`.
        assert_eq!(
            app.auto_apply_intent.map(|intent| intent.attempts),
            Some(u8::try_from(attempt).expect("small")),
            "attempt {attempt} must have consumed exactly one retry budget cycle"
        );
    }
}

/// The overdue notice only claims a lane that lands by itself: the updater
/// is told `false` with `[update] auto_apply` off, when the in-session
/// handoff cannot run (`HandoffDisabled`), while a person's unsaved work
/// holds the lane, and while a process that keeps its image has no terminal
/// for its only lane to hand over (`KeptImageNeedsTerminal`).
#[test]
fn a_lane_that_waits_by_design_is_not_reported_as_landing_by_itself() {
    use crate::app_update_handoff::HandoffUnavailable;
    let mut app = App::headless_for_test();
    assert!(app.update_lands_by_itself_with(true, None));
    assert!(
        !app.update_lands_by_itself_with(false, None),
        "the switch off"
    );
    for why in HandoffUnavailable::ALL {
        assert!(!app.update_lands_by_itself_with(true, Some(why)), "{why:?}");
    }
    app.update_waits_on_person = Some(7);
    assert!(
        !app.update_lands_by_itself_with(true, None),
        "a person holds it"
    );
    assert!(
        !app.update_lands_by_itself(true),
        "and the live reading agrees (a headless App has no handoff either)"
    );
    app.update_waits_on_person = None;
    #[cfg(unix)]
    {
        app.launch_image_kept = true;
        assert!(
            app.update_lands_by_itself_with(true, None),
            "CONTROL: a kept image with a live terminal hands it over by itself"
        );
        let ids: Vec<u64> = app.pool.iter().map(|session| session.id).collect();
        assert!(!ids.is_empty(), "PRECONDITION: the test App has a terminal");
        for id in ids {
            app.store
                .write()
                .unwrap_or_else(std::sync::PoisonError::into_inner)
                .set_state(id, crate::session_store::SessionState::Exited);
        }
        assert!(
            !app.update_lands_by_itself_with(true, None),
            "a kept image with no live terminal waits for one"
        );
    }
}

/// Every wait on a person has its notice: the standing row's key is shared by every wait and a
/// read keeps a key quiet for a day, yet a wait that begins after a read one ended is news with a
/// row of its own. The control: the same wait, once read, stays quiet.
#[test]
fn a_wait_that_begins_after_a_read_one_ended_is_said_again() {
    let _ledger = crate::app_update_screen::hold_update_ledger_for_test();
    let mut app = App::headless_for_test();
    let (_, settings) = park_a_settings_draft_in_a_background_tab(&mut app);
    let build = app.native_updater_service.snapshot().current_build + 1;
    stage_one_build_for_test(&mut app, build);
    app.clear_messages_for_test();
    let waits_row = |app: &App| {
        app.messages
            .live_by_key(crate::update_words::KEY_WAITS_FOR_YOU)
            .map(|l| l.id)
    };
    for _ in 0..=MAX_AUTOMATIC_UPDATE_CYCLES {
        force_auto_apply_attempt_now(&mut app);
        app.try_pending_native_auto_apply(false);
    }
    let first = waits_row(&app).expect("PRECONDITION: the exhaustion said the wait");
    // The person reads it: it folds, and the same wait stays quiet.
    assert!(app.messages.mark_seen(first, std::time::Instant::now()));
    app.note_update_blockers(&[App::UNSAVED_NATIVE_WORK_BLOCKS_APPLY.to_string()], true);
    assert!(
        waits_row(&app).is_none(),
        "CONTROL: the wait the person read stays quiet"
    );
    // Saved: the next attempt answers otherwise, and the wait is over.
    discard_settings_drafts(&mut app, settings);
    force_auto_apply_attempt_now(&mut app);
    app.try_pending_native_auto_apply(false);
    assert_eq!(app.update_waits_on_person, None, "PRECONDITION: saved");
    // Unsaved again: the same build is held anew, past its budget.
    let _ = park_a_settings_draft_in_a_background_tab(&mut app);
    force_auto_apply_attempt_now(&mut app);
    app.try_pending_native_auto_apply(false);
    assert_eq!(
        app.update_waits_on_person,
        Some(build),
        "PRECONDITION: a person holds the lane again"
    );
    assert!(
        !app.update_lands_by_itself_with(true, None),
        "PRECONDITION: the overdue notice is off"
    );
    let second = waits_row(&app).expect("a wait that began is news: its row stands");
    assert_ne!(second, first, "a row of its own");
    assert!(
        matches!(
            app.messages
                .log()
                .get(first)
                .and_then(aterm_messages::LogRecord::retired),
            Some(aterm_messages::Retired::Withdrawn)
        ),
        "the read of the ended wait is on record as withdrawn"
    );
}

/// The wait ends with the lane that waited: a `PollDecision::Clear` (`[update] auto_apply` turned
/// off, the stage retired or consumed) ends it, and the loop's own reconcile
/// (`settle_update_wait_on_person`) ends one whose intent another path dropped.
#[test]
fn a_wait_on_a_person_ends_when_the_lane_stops_trying() {
    for path in ["auto_apply off, the Clear", "the intent dropped, the loop"] {
        let _ledger = crate::app_update_screen::hold_update_ledger_for_test();
        let mut app = App::headless_for_test();
        let _ = park_a_settings_draft_in_a_background_tab(&mut app);
        let build = app.native_updater_service.snapshot().current_build + 1;
        stage_one_build_for_test(&mut app, build);
        app.clear_messages_for_test();
        for _ in 0..=MAX_AUTOMATIC_UPDATE_CYCLES {
            force_auto_apply_attempt_now(&mut app);
            app.try_pending_native_auto_apply(false);
        }
        assert_eq!(
            app.update_waits_on_person,
            Some(build),
            "{path}: PRECONDITION"
        );
        assert!(
            app.messages
                .live_by_key(crate::update_words::KEY_WAITS_FOR_YOU)
                .is_some(),
            "{path}: PRECONDITION: the wait's row stands"
        );
        // Control: while the lane still means to land it, the wait stands.
        app.settle_update_wait_on_person();
        assert_eq!(app.update_waits_on_person, Some(build), "{path}: CONTROL");
        if path.starts_with("auto_apply off") {
            app.config.update = Some(crate::app_config::UpdateConfig {
                auto_apply: Some(false),
                ..Default::default()
            });
            force_auto_apply_attempt_now(&mut app);
            app.try_pending_native_auto_apply(false);
            assert!(
                app.auto_apply_intent.is_none(),
                "{path}: PRECONDITION: the lane cleared its intent"
            );
        } else {
            app.auto_apply_intent = None;
            app.settle_update_wait_on_person();
        }
        assert_eq!(app.update_waits_on_person, None, "{path}: the wait ended");
        assert!(
            app.messages
                .live_by_key(crate::update_words::KEY_WAITS_FOR_YOU)
                .is_none(),
            "{path}: no row says the lane waits for a save that changes nothing"
        );
    }
}

/// An editor blocking the install is one row through the whole budget: the exhaustion's repeat is
/// the center's Duplicate, never a second row or record.
#[test]
fn an_announced_editor_block_stays_one_row_through_the_budget() {
    let _ledger = crate::app_update_screen::hold_update_ledger_for_test();
    let mut app = App::headless_for_test();
    let _ = park_a_settings_draft_in_a_background_tab(&mut app);
    let build = app.native_updater_service.snapshot().current_build + 1;
    stage_one_build_for_test(&mut app, build);
    app.clear_messages_for_test();
    let waits = |app: &App| {
        app.messages
            .log()
            .records()
            .filter(|r| r.title == crate::app_update_screen::UPDATE_WAITS_FOR_YOU)
            .count()
    };
    // The lane retries by itself, so the row stands.
    let outcome_row = |app: &App| {
        app.messages
            .live_by_key(crate::update_words::KEY_WAITS_FOR_YOU)
            .map(|l| l.id)
    };

    force_auto_apply_attempt_now(&mut app);
    app.try_pending_native_auto_apply(true);
    assert_eq!(app.auto_apply_intent.map(|i| i.attempts), Some(1));
    let row = outcome_row(&app).expect("the announced first block is the row");
    assert_eq!(waits(&app), 1);

    for attempt in 2..=MAX_AUTOMATIC_UPDATE_CYCLES {
        force_auto_apply_attempt_now(&mut app);
        app.try_pending_native_auto_apply(false);
        assert_eq!(app.auto_apply_intent.map(|i| i.attempts), Some(attempt));
    }
    // Precondition: the budget is spent, so the exhaustion said it again.
    assert!(
        app.auto_apply_intent
            .is_some_and(|i| i.retry_at > std::time::Instant::now() + PREFLIGHT_BLOCK_COOLDOWN / 2),
        "the budget must be exhausted, or the second saying never ran"
    );
    assert_eq!(outcome_row(&app), Some(row), "the same row stands");
    assert_eq!(waits(&app), 1, "one record for one blocker");
}

/// The refusal the swap's gate 4d and the pre-park checks give a stage whose
/// signer no check has confirmed for a day (`aterm_update`'s
/// `Ready::roster_authorization_stale`), as the worker reports it.
#[cfg(unix)]
fn roster_confirmation_wait(build: u64) -> String {
    format!(
        "staged update failed pre-park verification: staged build {build} has not been \
         {} since 2026-09-30T08:00:00Z, so a revocation of its signer could have been \
         missed; it installs once the next update check confirms it ({})",
        aterm_update::ROSTER_UNCONFIRMED_KEY,
        aterm_update::PASSING_REFUSAL_KEY
    )
}

/// An automatic attempt on a stage waiting for a check to confirm its signer books no failure and
/// no latch, and goes on once a check reports the stage. The pre-park refusal is its own lane
/// (`HandoffFailureLane::AwaitsRosterConfirmation`): a deferral, the intent held for the check,
/// nothing charged, latched or booked toward `record_apply_failure`'s `apply` streak, and the
/// ledger's standing refusal names the wait. While it holds no attempt is made.
///
/// A person's press classifies the same way and is not booked as a failure either.
// The bundle apply lane and its ledger (`aterm_update::apply_lane_report`) are
// macOS's: a Linux update replaces the executable on disk and hands nothing off.
#[cfg(target_os = "macos")]
#[test]
fn a_stage_waiting_for_its_roster_confirmation_is_held_and_never_a_failure() {
    use crate::native_updater_service::ApplyAttemptTicket;
    let _ledger = crate::app_update_screen::hold_update_ledger_for_test();
    let mut app = App::headless_for_test();
    let current = app.native_updater_service.snapshot().current_build;
    let build = current + 5_031;
    stage_one_build_for_test(&mut app, build);
    let dmg_sha256 = [0xab; 32];
    let wait = roster_confirmation_wait(build);

    // The worker's filing of the refusal, and the lane it lands in.
    let outcome = crate::app_update_handoff::pre_park_refusal_outcome(&wait);
    assert_eq!(outcome, crate::UpdateHandoffOutcome::StageUnconfirmed);
    let death = crate::ChildDeathEvidence::Unobserved;
    for (mode, automatic) in [
        (ApplyMode::Automatic, true),
        (ApplyMode::AutomaticPastGrace, true),
        (ApplyMode::Immediate, false),
    ] {
        assert_eq!(
            HandoffFailureLane::classify(mode, outcome, death, false),
            HandoffFailureLane::AwaitsRosterConfirmation { automatic },
            "{mode:?}"
        );
    }

    // The automatic attempt comes back with the wait.
    let before = aterm_update::status(current).expect("the scratch ledger");
    let ticket = ApplyAttemptTicket::for_test(build, PREFLIGHT_TEST_COMMIT, &"ab".repeat(32));
    ticket.make_current_apply_for_test(&mut app.native_updater_service);
    // The poll gave the intent up on the way into the attempt.
    app.auto_apply_intent = None;
    let surfaced = app.abort_reaped_native_apply_before_reconcile(
        &ticket,
        format!("overlap handoff failed safely: {wait}"),
        HandoffFailureLane::classify(ApplyMode::Automatic, outcome, death, false),
    );
    assert!(
        matches!(&surfaced, UpdateOutcome::Deferred { reason } if reason.contains(&wait)),
        "a deferral, not a failure: {surfaced:?}"
    );
    app.surface_update_apply_outcome_for_target("automatic handoff", surfaced, false, build);
    assert!(app.auto_apply_manual_only.is_none(), "no latch");
    assert!(app.auto_apply_physical_retry.is_none(), "no budget spent");
    let after = aterm_update::status(current).expect("the scratch ledger");
    assert_eq!(
        after.failing_applies, before.failing_applies,
        "no apply failure booked, so no `apply` streak toward the failing alarm"
    );
    let report = aterm_update::apply_lane_report(current).expect("the scratch ledger");
    assert!(
        aterm_update::is_roster_confirmation_wait(&report.last_refusal),
        "the standing refusal names the wait: {report:?}"
    );
    let held = app
        .auto_apply_roster_wait
        .clone()
        .expect("the lane holds for the check");
    assert!(held.covers(build, dmg_sha256));
    let intent = app.auto_apply_intent.expect("the intent is kept");
    assert_eq!((intent.build, intent.dmg_sha256), (build, dmg_sha256));
    assert_eq!(intent.retry_at, held.reread_at);

    // While it holds, the poll makes no attempt, even at `Land`.
    force_auto_apply_attempt_now(&mut app);
    app.try_pending_native_auto_apply(false);
    let intent = app.auto_apply_intent.expect("still held");
    assert_eq!(intent.attempts, 0, "no attempt while the stage waits");
    assert_eq!(
        intent.retry_at, held.reread_at,
        "looked at again when the hold says"
    );
    assert_ne!(
        app.native_updater_service.snapshot().phase,
        UpdaterPhase::Applying
    );

    // A check reports the stage: the lane asks the stage again, it passes (on the cached verdict
    // the stage armed with), and the attempt goes on, stopped at the preflight by a Settings
    // draft, which counts it.
    let _ = park_a_settings_draft_in_a_background_tab(&mut app);
    app.roster_wait_check_reported(build);
    assert!(
        app.auto_apply_intent
            .is_some_and(|intent| intent.retry_at <= std::time::Instant::now()),
        "the report makes the held intent due"
    );
    force_auto_apply_attempt_now(&mut app);
    app.try_pending_native_auto_apply(false);
    assert_eq!(app.auto_apply_roster_wait, None, "the hold ends");
    assert_eq!(
        app.auto_apply_intent.map(|intent| intent.attempts),
        Some(1),
        "the attempt reached the apply preflight"
    );
}

/// AN ARMED LANE LOOKS AT ONCE AND STARTS AS ITS CANDIDATE'S PRE-PARK READ ENDS. The arm makes the
/// intent due at once (the quiet gate reads the epoch behind it, so a first look one epoch out only
/// waited); while the arm-time read of exactly these bytes runs, a look holds the attempt (intent
/// kept, nothing spent, looked at again [`super::PREVERIFY_READ_POLL`] later) instead of letting
/// its worker verify the same bytes beside it; the first look after the read ends makes the
/// attempt (stopped here at the preflight by a Settings draft, which counts it). Negative controls,
/// run when this was written: the arm one quiet epoch out fails "due at once", and without the
/// hold the look during the read makes the attempt.
#[cfg(unix)]
#[test]
fn an_armed_lane_looks_at_once_and_starts_as_its_pre_park_read_ends() {
    let _ledger = crate::app_update_screen::hold_update_ledger_for_test();
    let mut app = App::headless_for_test();
    let current = app.native_updater_service.snapshot().current_build;
    let build = current + 5_034;
    let before = std::time::Instant::now();
    stage_one_build_for_test(&mut app, build);
    let armed = app.auto_apply_intent.expect("the stage arms the lane");
    assert!(
        armed.retry_at >= before && armed.retry_at <= std::time::Instant::now(),
        "due at once"
    );
    // The arm-time read of these bytes is still running, and no verdict stands yet.
    *app.handoff_preverified
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner) = None;
    let done = preverify_read_running(&mut app, build);
    let _ = park_a_settings_draft_in_a_background_tab(&mut app);
    force_auto_apply_attempt_now(&mut app);
    let looked = std::time::Instant::now();
    app.try_pending_native_auto_apply(false);
    let held = app.auto_apply_intent.expect("the intent is kept");
    assert_eq!(held.attempts, 0, "no attempt while the read runs");
    assert!(
        held.retry_at >= looked + super::PREVERIFY_READ_POLL
            && held.retry_at <= std::time::Instant::now() + super::PREVERIFY_READ_POLL,
        "looked at again one read poll later"
    );

    // The read ends with its verdict: the next look makes the attempt.
    *app.handoff_preverified
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner) = Some(crate::HandoffPreverification {
        build,
        commit: PREFLIGHT_TEST_COMMIT.to_string(),
        artifact: "ab".repeat(32),
        at: std::time::Instant::now(),
        passed: true,
        reason: None,
        policy: None,
        grant_chunks: false,
    });
    done.store(true, std::sync::atomic::Ordering::Release);
    force_auto_apply_attempt_now(&mut app);
    app.try_pending_native_auto_apply(false);
    assert_eq!(
        app.auto_apply_intent.map(|intent| intent.attempts),
        Some(1),
        "the attempt reached the apply preflight"
    );
}

/// At a launch that met a stale stage, the first attempt is never made: the arm-time pre-park read
/// ends waiting for a check, and the poll holds on that answer (no attempt, nothing to book). A
/// re-read that answers the wait again renews the hold; any other answer ends it.
// The bundle apply lane and its ledger (`aterm_update::apply_lane_report`) are
// macOS's: a Linux update replaces the executable on disk and hands nothing off.
#[cfg(target_os = "macos")]
#[test]
fn the_arm_time_read_holds_the_lane_before_its_first_attempt() {
    use std::sync::Arc;
    use std::sync::atomic::AtomicBool;
    // The last attempt is stopped at the preflight, which books its refusal.
    let _ledger = crate::app_update_screen::hold_update_ledger_for_test();
    let mut app = App::headless_for_test();
    let current = app.native_updater_service.snapshot().current_build;
    let build = current + 5_032;
    stage_one_build_for_test(&mut app, build);
    // No verdict stands: the arm-time read refused, passing.
    *app.handoff_preverified
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner) = None;
    let read_ending = |reason: &str, ended: std::time::Instant| {
        let passing = Arc::new(std::sync::OnceLock::new());
        passing
            .set(crate::PassingPreverification {
                reason: reason.to_string(),
                ended,
            })
            .unwrap();
        crate::PreverifyInFlight {
            build,
            commit: PREFLIGHT_TEST_COMMIT.to_string(),
            artifact: "ab".repeat(32),
            deadline: std::time::Instant::now() + crate::HANDOFF_PREVERIFY_READ_CEILING,
            done: Arc::new(AtomicBool::new(true)),
            passing,
        }
    };
    let ended = std::time::Instant::now();
    app.handoff_preverify_in_flight = Some(read_ending(&roster_confirmation_wait(build), ended));
    force_auto_apply_attempt_now(&mut app);
    app.try_pending_native_auto_apply(false);
    let held = app
        .auto_apply_roster_wait
        .clone()
        .expect("the read's answer holds the lane");
    assert_eq!(held.answered_at, ended);
    let intent = app.auto_apply_intent.expect("the intent is kept");
    assert_eq!(intent.attempts, 0, "the first attempt is never made");
    assert_eq!(intent.retry_at, held.reread_at);
    assert!(
        aterm_update::is_roster_confirmation_wait(
            &aterm_update::apply_lane_report(current)
                .expect("the scratch ledger")
                .last_refusal
        ),
        "a new hold names its wait as the standing refusal"
    );

    // The re-read comes due and answers the wait again: renewed, still no attempt.
    app.auto_apply_roster_wait = Some(crate::AutoApplyRosterWait {
        reread_at: std::time::Instant::now(),
        rereading: true,
        ..held
    });
    let again = std::time::Instant::now();
    app.handoff_preverify_in_flight = Some(read_ending(&roster_confirmation_wait(build), again));
    force_auto_apply_attempt_now(&mut app);
    app.try_pending_native_auto_apply(false);
    let renewed = app.auto_apply_roster_wait.clone().expect("renewed");
    assert_eq!(renewed.answered_at, again);
    assert!(!renewed.rereading);
    assert_eq!(app.auto_apply_intent.map(|intent| intent.attempts), Some(0));

    // A re-read that answered something else (another passing moment) ends the
    // hold; the attempt answers for itself (stopped here at the preflight).
    let _ = park_a_settings_draft_in_a_background_tab(&mut app);
    app.auto_apply_roster_wait = Some(crate::AutoApplyRosterWait {
        reread_at: std::time::Instant::now(),
        rereading: true,
        ..renewed
    });
    app.handoff_preverify_in_flight = Some(read_ending(
        &format!(
            "lock: could not take the apply lock ({})",
            aterm_update::PASSING_REFUSAL_KEY
        ),
        std::time::Instant::now(),
    ));
    force_auto_apply_attempt_now(&mut app);
    app.try_pending_native_auto_apply(false);
    assert_eq!(app.auto_apply_roster_wait, None);
    assert_eq!(app.auto_apply_intent.map(|intent| intent.attempts), Some(1));
}

/// A cold relaunch whose boot swap waits for a roster confirmation books nothing. On a desk with
/// no terminal the cold lane execs into its own build, whose gate 4d waits for a check: the launch
/// seeds a hold, not a refusal verdict (nothing kept apart, the carried budget put back as it
/// was), so the first attempt is never made, the cold lane defers (for a person's press too)
/// instead of relaunching again, and nothing is booked, latched or spent. (Any other boot refusal
/// is still seeded as the verdict it is:
/// `a_refused_cold_relaunch_converges_across_its_own_relaunches`.)
#[cfg(unix)]
#[test]
fn a_cold_relaunch_waiting_for_its_roster_confirmation_holds_and_books_nothing() {
    use crate::app_update_handoff::{CarriedFailures, ColdLaneBudget, ColdRelaunch};
    use crate::native_updater_service::ApplyAttemptTicket;
    use std::ffi::OsStr;
    let _ledger = crate::app_update_screen::hold_update_ledger_for_test();
    let running = crate::build_info::build_number();
    let mut app = App::headless_for_test();
    let current = app.native_updater_service.snapshot().current_build;
    let build = current + 5_033;
    let build_text = build.to_string();
    let artifact = "ab".repeat(32);
    let dmg_sha256 = [0xab_u8; 32];
    let wait = roster_confirmation_wait(build);
    let relaunch = |budget: Option<&str>| {
        ColdRelaunch::from_values(
            Some(OsStr::new(running)),
            Some(OsStr::new(&build_text)),
            Some(OsStr::new(PREFLIGHT_TEST_COMMIT)),
            Some(OsStr::new(&artifact)),
            budget.map(OsStr::new),
            running,
        )
        .expect("the cold lane's relaunch of this build")
    };
    // The outgoing process had booked two failures of these bytes.
    let carried = ColdLaneBudget {
        failures: Some(CarriedFailures {
            cycles: 2,
            since_secs: 60,
            activation: false,
        }),
        verdict: None,
    }
    .encode(build, &artifact);
    let seed = relaunch(Some(&carried))
        .seed(&aterm_update::ApplyOutcome::Deferred(wait.clone()))
        .expect("the boot swap waited");
    assert_eq!(seed.roster_wait.as_deref(), Some(wait.as_str()));
    let before = aterm_update::status(current).expect("the scratch ledger");
    app.seed_refused_cold_relaunch(seed, std::time::Instant::now());
    assert!(
        app.handoff_preverified
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .is_none(),
        "no verdict is seeded, so the arm-time read runs"
    );
    assert!(
        app.cold_relaunch_refused.is_none(),
        "nothing is kept apart for an attempt to book"
    );
    let held = app
        .auto_apply_roster_wait
        .clone()
        .expect("the lane holds for the check");
    assert!(held.said && held.covers(build, dmg_sha256));
    assert_eq!(app.auto_apply_intent, None, "the seed arms nothing itself");
    assert_eq!(
        app.auto_apply_physical_retry.map(|retry| retry.cycles),
        Some(2),
        "the carried budget is put back as it was"
    );

    // The startup reconcile stages the build and arms the lane, and its first
    // attempt comes due: none is made.
    stage_one_build_for_test(&mut app, build);
    force_auto_apply_attempt_now(&mut app);
    app.try_pending_native_auto_apply(false);
    let intent = app.auto_apply_intent.expect("the intent is kept");
    assert_eq!(intent.attempts, 0, "the first attempt is never made");
    assert_eq!(
        intent.retry_at, held.reread_at,
        "looked at when the hold says"
    );
    // A desk with no terminal: the cold lane defers for either mode instead of
    // relaunching into the same wait, and fails nothing.
    let ticket = ApplyAttemptTicket::for_test(build, PREFLIGHT_TEST_COMMIT, &artifact);
    for mode in [ApplyMode::AutomaticPastGrace, ApplyMode::Immediate] {
        let error = app
            .cold_lane_verdict_gate(mode, Some(&ticket))
            .expect_err("not relaunched into the wait");
        assert!(
            aterm_update::is_roster_confirmation_wait(&error.to_string()),
            "{mode:?}: {error}"
        );
    }
    assert!(app.auto_apply_manual_only.is_none(), "no latch");
    assert_eq!(
        app.auto_apply_physical_retry.map(|retry| retry.cycles),
        Some(2),
        "no budget spent"
    );
    let after = aterm_update::status(current).expect("the scratch ledger");
    assert_eq!(
        after.failing_applies, before.failing_applies,
        "no apply failure booked"
    );
}

/// A check's report is never lost to a read that began before it. Two orderings:
///
/// 1. the arm-time read ends waiting; the check confirms and reports before the lane's first poll
///    reaches the hold (that poll waits for a quiet terminal);
/// 2. a hold's re-read starts; the check reports; the re-read, which read the marker before the
///    confirmation, ends waiting.
///
/// Either way the stale answer is read again at once (held provisionally, nothing said or booked)
/// and the fresh answer decides. (A read that ends waiting with no report before it holds and
/// books: `the_arm_time_read_holds_the_lane_before_its_first_attempt`.)
// The bundle apply lane and its ledger (`aterm_update::apply_lane_report`) are
// macOS's: a Linux update replaces the executable on disk and hands nothing off.
#[cfg(target_os = "macos")]
#[test]
fn a_check_report_is_never_lost_to_a_read_that_began_before_it() {
    use std::sync::Arc;
    use std::sync::atomic::{AtomicBool, Ordering};
    use std::time::{Duration, Instant};
    let _ledger = crate::app_update_screen::hold_update_ledger_for_test();
    let mut app = App::headless_for_test();
    let current = app.native_updater_service.snapshot().current_build;
    let build = current + 5_034;
    stage_one_build_for_test(&mut app, build);
    let dmg_sha256 = [0xab_u8; 32];
    let wait = roster_confirmation_wait(build);
    let no_verdict = |app: &App| {
        *app.handoff_preverified
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner) = None;
    };
    let confirmed = |app: &App| {
        *app.handoff_preverified
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner) =
            Some(crate::HandoffPreverification {
                build,
                commit: PREFLIGHT_TEST_COMMIT.to_string(),
                artifact: "ab".repeat(32),
                at: Instant::now(),
                passed: true,
                reason: None,
                policy: None,
                grant_chunks: false,
            });
    };
    // A read of the stage that began at `began` and ended at `ended`, waiting for
    // a check when `reason` is given and passing otherwise.
    let read = |reason: Option<&str>, began: Instant, ended: Instant| {
        let passing = Arc::new(std::sync::OnceLock::new());
        if let Some(reason) = reason {
            passing
                .set(crate::PassingPreverification {
                    reason: reason.to_string(),
                    ended,
                })
                .unwrap();
        }
        crate::PreverifyInFlight {
            build,
            commit: PREFLIGHT_TEST_COMMIT.to_string(),
            artifact: "ab".repeat(32),
            deadline: began + crate::HANDOFF_PREVERIFY_READ_CEILING,
            done: Arc::new(AtomicBool::new(true)),
            passing,
        }
    };
    // The fresh read the lane starts runs for real against the scratch root;
    // the test answers in its place once it has ended.
    let read_ends = |app: &App| {
        let since = Instant::now();
        while !app
            .handoff_preverify_in_flight
            .as_ref()
            .is_some_and(|read| read.done.load(Ordering::Acquire))
        {
            assert!(
                since.elapsed() < Duration::from_secs(60),
                "the fresh read never ended"
            );
            std::thread::sleep(Duration::from_millis(5));
        }
    };
    let standing = || {
        aterm_update::apply_lane_report(current)
            .expect("the scratch ledger")
            .last_refusal
    };
    let _ = park_a_settings_draft_in_a_background_tab(&mut app);

    // 1. The report lands before any hold exists.
    no_verdict(&app);
    let began = Instant::now();
    app.handoff_preverify_in_flight = Some(read(Some(&wait), began, began));
    app.roster_wait_check_reported(build);
    assert_eq!(
        app.auto_apply_roster_wait, None,
        "PRECONDITION: nothing held"
    );
    let booked = standing();
    force_auto_apply_attempt_now(&mut app);
    app.try_pending_native_auto_apply(false);
    let provisional = app
        .auto_apply_roster_wait
        .clone()
        .expect("held while the stage is read again");
    assert!(
        !provisional.said && provisional.rereading,
        "{provisional:?}"
    );
    let intent = app.auto_apply_intent.expect("the intent is kept");
    assert_eq!(intent.attempts, 0, "no attempt on a stale answer");
    assert!(
        intent.retry_at <= Instant::now() + crate::ROSTER_WAIT_POLL,
        "looked at again in seconds, not in ten minutes"
    );
    assert_eq!(standing(), booked, "the stale wait is not booked");
    assert!(
        app.handoff_preverify_in_flight
            .as_ref()
            .is_some_and(|fresh| fresh.started() > began),
        "a fresh read was started"
    );
    // The fresh read finds the stage the check confirmed: the attempt goes on.
    read_ends(&app);
    confirmed(&app);
    app.handoff_preverify_in_flight = Some(read(None, Instant::now(), Instant::now()));
    force_auto_apply_attempt_now(&mut app);
    app.try_pending_native_auto_apply(false);
    assert_eq!(app.auto_apply_roster_wait, None, "the hold ends");
    assert_eq!(app.auto_apply_intent.map(|intent| intent.attempts), Some(1));

    // 2. The report lands while the hold's re-read runs.
    no_verdict(&app);
    let answered = Instant::now();
    let mut hold =
        app.hold_for_roster_confirmation(build, dmg_sha256, &wait, answered, false, false);
    hold.rereading = true;
    app.auto_apply_roster_wait = Some(hold.clone());
    let reread_began = Instant::now();
    app.roster_wait_check_reported(build);
    assert_eq!(
        app.auto_apply_roster_wait.as_ref(),
        Some(&hold),
        "a re-read runs: the report is recorded, the hold left as it is"
    );
    app.handoff_preverify_in_flight = Some(read(Some(&wait), reread_began, Instant::now()));
    let booked = standing();
    force_auto_apply_attempt_now(&mut app);
    app.try_pending_native_auto_apply(false);
    let still = app
        .auto_apply_roster_wait
        .clone()
        .expect("held while the stage is read again");
    assert!(still.said && still.rereading, "{still:?}");
    assert!(
        app.auto_apply_intent
            .is_some_and(|intent| intent.retry_at <= Instant::now() + crate::ROSTER_WAIT_POLL),
        "not renewed for ten minutes"
    );
    assert_eq!(standing(), booked, "nothing booked again");
    assert!(
        app.handoff_preverify_in_flight
            .as_ref()
            .is_some_and(|fresh| fresh.started() > reread_began),
        "a fresh read was started"
    );
    read_ends(&app);
    confirmed(&app);
    app.handoff_preverify_in_flight = Some(read(None, Instant::now(), Instant::now()));
    force_auto_apply_attempt_now(&mut app);
    app.try_pending_native_auto_apply(false);
    assert_eq!(app.auto_apply_roster_wait, None, "the hold ends");
    assert_eq!(app.auto_apply_intent.map(|intent| intent.attempts), Some(2));
}

/// Drives the real nonannounced timer path into a dirty-Settings refusal and reads its durable
/// answer, in a child process so the shared scratch ledger is isolated from other updater tests.
#[cfg(target_os = "macos")]
#[test]
fn nonannounced_automatic_blocks_are_durable_only_after_real_attempts() {
    if !crate::test_alone::alone() {
        return;
    }

    let _ledger = crate::app_update_screen::hold_update_ledger_for_test();
    let mut app = App::headless_for_test();
    assert!(crate::app_config::update_auto_apply(&app.config));
    assert!(!App::relaunch_nudge_seam_suppresses_auto_apply());
    let _ = park_a_settings_draft_in_a_background_tab(&mut app);
    let current = app.native_updater_service.snapshot().current_build;
    stage_one_build_for_test(&mut app, current + 4_913);
    let before = aterm_update::status(current).unwrap();
    app.clear_messages_for_test();

    force_auto_apply_attempt_now(&mut app);
    let attempted_at = std::time::Instant::now();
    app.try_pending_native_auto_apply(false);
    assert_eq!(app.auto_apply_intent.unwrap().attempts, 1);
    assert!(
        app.auto_apply_intent.unwrap().retry_at > attempted_at,
        "the blocked attempt sets its own retry in the future"
    );
    let report = aterm_update::apply_lane_report(current).unwrap();
    assert_eq!(report.last_refusal, App::UNSAVED_NATIVE_WORK_BLOCKS_APPLY);
    assert!(!report.last_refusal_at.is_empty());
    let after = aterm_update::status(current).unwrap();
    assert!(
        after
            .outcome
            .contains(App::UNSAVED_NATIVE_WORK_BLOCKS_APPLY)
    );
    assert_eq!(after.failing_applies, before.failing_applies);
    assert_eq!(after.failing_checks, before.failing_checks);
    assert!(
        app.update_row_text().is_none(),
        "announce=false remains quiet"
    );

    // A distinct marker shows an accidental rewrite despite second-precision timestamps: polling
    // a future retry neither spends an attempt nor replaces the standing explanation.
    const BETWEEN: &str = "test marker between actual automatic attempts";
    aterm_update::record_apply_refusal(current, BETWEEN);
    // The polls run inside the retry window however slowly a loaded machine runs them:
    // the window is pinned an hour out (what is under test is the poll's `now <
    // retry_at`, not the length of the first delay).
    let mut intent = app.auto_apply_intent.unwrap();
    intent.retry_at = std::time::Instant::now() + std::time::Duration::from_secs(3600);
    app.auto_apply_intent = Some(intent);
    for _ in 0..16 {
        app.try_pending_native_auto_apply(false);
    }
    assert_eq!(app.auto_apply_intent.unwrap().attempts, 1);
    assert_eq!(
        aterm_update::apply_lane_report(current)
            .unwrap()
            .last_refusal,
        BETWEEN
    );

    force_auto_apply_attempt_now(&mut app);
    app.try_pending_native_auto_apply(false);
    assert_eq!(app.auto_apply_intent.unwrap().attempts, 2);
    assert_eq!(
        aterm_update::apply_lane_report(current)
            .unwrap()
            .last_refusal,
        App::UNSAVED_NATIVE_WORK_BLOCKS_APPLY,
    );
    assert_eq!(
        aterm_update::status(current).unwrap().failing_applies,
        before.failing_applies
    );
    assert!(app.update_row_text().is_none());
}

/// A close-preflight block exhausting its budget neither retires automatic apply nor becomes a
/// scheduled disturbance: two full cooldown rounds of the real lane
/// (`try_pending_native_auto_apply` → `apply_native_update` → `prepare_all_native_shutdown` →
/// the preflight reducer) cost one attempt per cooldown, no row after the first and nothing on
/// screen, with a way back once the blocker (an unsaved Settings draft) clears.
#[test]
fn an_exhausted_preflight_block_budget_neither_latches_forever_nor_nags_on_a_schedule() {
    let _ledger = crate::app_update_screen::hold_update_ledger_for_test();
    let mut app = App::headless_for_test();
    let wid = WindowId(0);
    assert!(
        crate::app_config::update_auto_apply(&app.config)
            && !App::relaunch_nudge_seam_suppresses_auto_apply(),
        "PRECONDITION: the automatic lane must be enabled or every poll below \
         answers Clear and this test proves nothing"
    );
    let (_, settings) = park_a_settings_draft_in_a_background_tab(&mut app);
    let working_tab = app.windows[&wid]
        .tab_set
        .active_id()
        .expect("PRECONDITION: the user is on a tab");
    let build = app.native_updater_service.snapshot().current_build + 1;
    stage_one_build_for_test(&mut app, build);
    assert!(
        app.auto_apply_intent
            .is_some_and(|intent| intent.build == build),
        "PRECONDITION: the staged build armed automatic intent"
    );

    app.clear_messages_for_test();
    spend_one_preflight_block_budget(&mut app);

    // Not permanent: the intent is retained on a cooldown that `about_to_wait` folds into
    // `WaitUntil` (`fold_auto_apply_deadline`), so it fires on an idle terminal.
    let cooling = app
        .auto_apply_intent
        .expect("a spent preflight budget must NOT retire the automatic lane");
    assert_eq!(cooling.build, build);
    assert!(
        app.auto_apply_manual_only.is_none(),
        "a transient block must not install a manual-only latch at all — that \
         latch is what `arm` reads as SuppressManualOnly"
    );
    let cooldown = cooling
        .retry_at
        .saturating_duration_since(std::time::Instant::now());
    assert!(
        cooldown > PREFLIGHT_BLOCK_COOLDOWN / 2 && cooldown <= PREFLIGHT_BLOCK_COOLDOWN,
        "the retry must be spaced by the cooldown, not by the 5s/15s probe \
         cadence (got {cooldown:?})"
    );

    // Further rounds: each attempt costs one probe and adds nothing to the screen.
    for round in 0..4 {
        app.clear_messages_for_test();
        force_auto_apply_attempt_now(&mut app);
        app.try_pending_native_auto_apply(false);
        let again = app
            .auto_apply_intent
            .expect("the lane keeps trying while the blocker persists");
        assert_eq!(
            again.attempts,
            MAX_AUTOMATIC_UPDATE_CYCLES + 1 + u8::try_from(round).expect("small"),
            "round {round}: a cooldown probe must consume exactly one attempt, \
             and `attempts` must keep counting — resetting it is what would \
             re-arm the whole budget and re-fire the pill"
        );
        assert!(
            again
                .retry_at
                .saturating_duration_since(std::time::Instant::now())
                > PREFLIGHT_BLOCK_COOLDOWN / 2,
            "round {round}: still spaced by the cooldown"
        );
        assert!(
            app.update_row_text().is_none() && app.update_record_text().is_none(),
            "round {round}: THE NAG. The user was already told once; telling \
             them again on a two-hour schedule is the regression this test exists \
             for (got {:?} / {:?})",
            app.update_row_text(),
            app.update_record_text()
        );
        // No tab switch, focus theft or recovery palette either.
        assert_no_probe_disturbance(&app, wid, working_tab);
    }

    // The way back: once the blocker clears, the next cooldown probe leaves the Blocked lane. The
    // receipt that the attempt got past the close preflight is the recorded refusal, the admission
    // gate's ("Update kept …"), reachable only past it (the reducer answered cleanly, so no
    // `Failed` either). Headless, `native_update_admission::classify` never reaches
    // `Apply(Seamless)`, so this does not show the update landing.
    assert!(
        app.pool.iter().count() > 0,
        "PRECONDITION AND A SAFETY RAIL: this app must own at least one live \
         session. With an empty pool `native_update_admission::classify` admits \
         the COLD lane, and the authorized apply below would `exec()` the test \
         binary instead of returning"
    );
    discard_settings_drafts(&mut app, settings);
    app.clear_messages_for_test();
    force_auto_apply_attempt_now(&mut app);
    app.try_pending_native_auto_apply(false);

    // The cleared blocker reaches the admission gate, which refuses: past the spent budget the
    // lane re-probes silently on the cooldown, with no new row, no latch and no physical failure.
    assert!(
        app.update_row_text().is_none(),
        "a refused re-probe past the exhaustion pill paints nothing: {:?}",
        app.update_row_text()
    );
    let refusal = app
        .native_updater_service
        .snapshot()
        .error
        .clone()
        .expect("the refused attempt records why it stopped");
    assert!(
        refusal.starts_with("Update kept"),
        "the receipt that the attempt got PAST the close preflight to the \
         admission gate, got {refusal:?}"
    );
    assert!(
        app.auto_apply_intent.is_some_and(
            |intent| intent.build == build && intent.retry_at > std::time::Instant::now()
        ),
        "the admission refusal keeps the intent armed on its cooldown"
    );
    assert!(
        app.auto_apply_manual_only.is_none() && app.auto_apply_physical_retry.is_none(),
        "an admission refusal neither latches manual-only nor books a \
         physical failure"
    );
    assert_no_probe_disturbance(&app, wid, working_tab);
}

/// With the same Settings draft, a background probe and a person's press get the same verdict,
/// but only the press surfaces Close Recovery.
#[test]
fn an_automatic_probe_is_silent_while_a_person_s_apply_still_surfaces_recovery() {
    let mut app = App::headless_for_test();
    let wid = WindowId(0);
    let (_, settings) = park_a_settings_draft_in_a_background_tab(&mut app);
    let working_tab = app.windows[&wid]
        .tab_set
        .active_id()
        .expect("PRECONDITION: the user is on a tab");
    let build = app.native_updater_service.snapshot().current_build + 1;
    stage_one_build_for_test(&mut app, build);

    // Background: the verdict is `Blocked` by the native-app barrier (not the
    // UI-less `native_update_close_preflight` counters, whose string differs).
    let UpdateOutcome::Blocked { reasons } = app.apply_native_update(ApplyMode::AutomaticPastGrace)
    else {
        panic!("an unsaved Settings draft blocks an update apply");
    };
    assert_eq!(
        reasons,
        vec![App::UNSAVED_NATIVE_WORK_BLOCKS_APPLY.to_string()],
        "the block came from the native close barrier — the one that surfaces \
         recovery — and not from a later, UI-less blocker"
    );
    assert_no_probe_disturbance(&app, wid, working_tab);

    // A person: same state, same verdict, but now the blocking leaf is focused
    // and its reducer-supplied recovery commands are on screen.
    let UpdateOutcome::Blocked { reasons: manual } = app.apply_native_update(ApplyMode::Immediate)
    else {
        panic!("the same draft blocks a manual apply");
    };
    assert_eq!(manual, reasons, "identical verdict, by construction");
    assert_eq!(
        app.active_native_view(wid).map(|(_, view)| view),
        Some(settings),
        "a person's apply focuses the leaf that refused"
    );
    let lines = app.windows[&wid]
        .palette()
        .expect("a person's apply opens Close Recovery")
        .controls_lines();
    for action in ["settings/drafts/review", "settings/drafts/discard-all"] {
        assert!(
            lines.iter().any(|line| {
                line.contains("target=native") && line.contains(&format!("action={action}"))
            }),
            "recovery exposes {action}: {lines:?}"
        );
    }
}

/// The pre-park verdict answers for one artifact only. A downloaded `.app` and the same build
/// installed under our executable share (build, commit) but are different bytes: the download's
/// `passed` must not skip the activation's last codesign check, and a corrupt download's `false`
/// must not refuse a good installed bundle.
#[cfg(unix)]
#[test]
fn a_pre_park_verdict_answers_only_for_its_own_artifact() {
    let mut app = App::headless_for_test();
    let build = app.native_updater_service.snapshot().current_build + 1;
    stage_one_build_for_test(&mut app, build);
    let download = crate::native_updater_service::ApplyAttemptTicket::for_test(
        build,
        PREFLIGHT_TEST_COMMIT,
        &"ab".repeat(32),
    );
    let activation = crate::native_updater_service::ApplyAttemptTicket::for_test(
        build,
        PREFLIGHT_TEST_COMMIT,
        &crate::native_updater_service::installed_activation_digest(build, PREFLIGHT_TEST_COMMIT),
    );
    assert_eq!(app.cached_handoff_preverification(&download), Some(true));
    assert_eq!(
        app.cached_handoff_preverification(&activation),
        None,
        "the download's verdict must not be reused for the installed bundle"
    );
    *app.handoff_preverified
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner) = Some(crate::HandoffPreverification {
        build,
        commit: PREFLIGHT_TEST_COMMIT.to_string(),
        artifact: "ab".repeat(32),
        at: std::time::Instant::now(),
        passed: false,
        reason: None,
        policy: None,
        grant_chunks: false,
    });
    assert_eq!(app.cached_handoff_preverification(&download), Some(false));
    assert_eq!(
        app.cached_handoff_preverification(&activation),
        None,
        "a corrupt download must not refuse a good installed bundle"
    );
}

/// Every physical lane shares one budget, the budget runs out, and the person is told once, when
/// it has: failures returning through `abort_reaped_native_apply_before_reconcile` consult the
/// budget, the stand-down never resets the counter, a late completion past convergence buys no
/// fourth epoch, and only the lane running out of retries posts a row.
#[test]
fn the_async_physical_lane_converges_and_stops_nagging() {
    let _ledger = crate::app_update_screen::hold_update_ledger_for_test();
    let mut app = App::headless_for_test();
    // Relative to the running build: `arm` answers `Clear` for anything not
    // strictly newer, which would make the final `arm_native_auto_apply`
    // assertions pass for the wrong reason.
    let build = app.native_updater_service.snapshot().current_build + 1;
    let ticket = crate::native_updater_service::ApplyAttemptTicket::for_test(
        build,
        PREFLIGHT_TEST_COMMIT,
        &"ab".repeat(32),
    );

    // The whole lifetime through the real completion lane, one entry per
    // failure: what the latch said and what the user saw.
    let mut latched = Vec::new();
    let mut pills = Vec::new();
    for _ in 0..usize::from(PHYSICAL_FAILURE_LIFETIME_ATTEMPTS) {
        ticket.make_current_apply_for_test(&mut app.native_updater_service);
        app.clear_messages_for_test();
        let outcome = app.abort_reaped_native_apply_before_reconcile(
            &ticket,
            "overlap handoff failed safely: handoff proof ended TimedOut".to_string(),
            HandoffFailureLane::Physical(PhysicalFailureShape::Transient),
        );
        app.surface_update_apply_outcome("automatic handoff", outcome, false);
        latched.push(
            app.auto_apply_manual_only
                .expect("a returned physical failure always latches manual-only")
                .retry_at
                .map(|at| at.saturating_duration_since(std::time::Instant::now())),
        );
        // A retry the lane scheduled is silent on the glass; the "pill"
        // counted here is the outcome row, raised only by a stopped lane.
        pills.push(app.update_row_text());
    }

    // Every failure latches with a deadline; the last is the converged lane's
    // quiet re-sample one epoch cooldown out.
    assert!(
        latched.iter().all(Option::is_some),
        "every returned failure schedules a deadline: {latched:?}"
    );
    let resample = latched
        .last()
        .copied()
        .flatten()
        .expect("the converged transient lane re-samples");
    assert!(
        resample > PHYSICAL_FAILURE_EPOCH_COOLDOWN - std::time::Duration::from_secs(60),
        "…one epoch cooldown out, never sooner: {resample:?}"
    );
    assert_eq!(
        app.auto_apply_physical_retry.map(|retry| retry.cycles),
        Some(PHYSICAL_FAILURE_LIFETIME_ATTEMPTS),
        "each returned failure spends exactly one attempt"
    );
    // A late or duplicate completion cannot mint a fourth epoch out of the
    // saturating counter: it stays converged on the one-cooldown re-sample.
    ticket.make_current_apply_for_test(&mut app.native_updater_service);
    let _ = app.abort_reaped_native_apply_before_reconcile(
        &ticket,
        "overlap handoff failed safely: handoff proof ended TimedOut".to_string(),
        HandoffFailureLane::Physical(PhysicalFailureShape::Transient),
    );
    assert!(
        app.auto_apply_manual_only
            .and_then(|manual| manual.retry_at)
            .is_some_and(|at| {
                at.saturating_duration_since(std::time::Instant::now())
                    > PHYSICAL_FAILURE_EPOCH_COOLDOWN - std::time::Duration::from_secs(60)
            }),
        "a late completion stays converged, never a fresh epoch"
    );
    // The counter must outlive the longest gap the schedule itself produces,
    // or epochs cannot be counted and the cap is unreachable.
    const {
        assert!(
            PHYSICAL_RETRY_BUDGET_REPLENISH.as_secs() > PHYSICAL_FAILURE_EPOCH_COOLDOWN.as_secs(),
            "the replenish window must outlast the stand-down between epochs"
        )
    };

    // The nag, counted: silence while the lane is coming back, one row at the end.
    assert_eq!(
        pills.iter().filter(|pill| pill.is_some()).count(),
        1,
        "nine failures may cost at most one row, got {pills:?}"
    );
    assert!(
        pills[..usize::from(PHYSICAL_FAILURE_LIFETIME_ATTEMPTS) - 1]
            .iter()
            .all(Option::is_none),
        "every failure before the last must pass in silence — the user has \
         nothing to do and the lane is already coming back, got {pills:?}"
    );

    // Converged until the re-sample: `arm` refuses the artifact, and the latch
    // lapses only at its one-cooldown deadline.
    assert!(!app.lapse_expired_auto_apply_manual_only());
    assert!(
        app.auto_apply_manual_only
            .is_some_and(|manual| manual.retry_at.is_some()),
        "the converged latch stays, with its re-sample deadline"
    );
    assert!(
        !app.arm_native_auto_apply(build, &"ab".repeat(32)),
        "a duplicate stage wake for the SAME artifact must not restart the lane"
    );
    // …but a strictly newer build is a different artifact and is not punished
    // for this one's history.
    assert!(
        app.arm_native_auto_apply(build + 1, &"cd".repeat(32)),
        "convergence is per-artifact; a newer build still arms automatically"
    );
}

/// The standing health row, when one is live: its title, its detail joined,
/// and its capsules.
fn health_row(app: &App) -> Option<(String, String, Vec<aterm_messages::Intent>)> {
    app.messages.live_health().map(|live| {
        (
            live.msg.title.clone(),
            live.msg.detail.join("; "),
            live.msg.actions.clone(),
        )
    })
}

/// A structural convergence is said at once: the second failure through the shipping completion
/// lane posts the install half's health warning (the updater's own streak gate needs 3;
/// structural converges at 2), naming the one re-sample a day out and the Version menu, with no
/// decision row while it is pending. A late third completion says nothing new and buys no new
/// deadline.
#[test]
fn a_structural_convergence_posts_the_health_notice_at_once() {
    let _ledger = crate::app_update_screen::hold_update_ledger_for_test();
    let mut app = App::headless_for_test();
    let build = app.native_updater_service.snapshot().current_build + 1;
    let ticket = crate::native_updater_service::ApplyAttemptTicket::for_test(
        build,
        PREFLIGHT_TEST_COMMIT,
        &"ab".repeat(32),
    );
    let fail = |app: &mut App| {
        ticket.make_current_apply_for_test(&mut app.native_updater_service);
        let outcome = app.abort_reaped_native_apply_before_reconcile(
            &ticket,
            "overlap handoff failed safely: handoff proof ended AdoptionMismatch".to_string(),
            HandoffFailureLane::Physical(PhysicalFailureShape::Structural),
        );
        app.surface_update_apply_outcome("automatic handoff", outcome, false);
    };

    fail(&mut app);
    assert!(
        health_row(&app).is_none(),
        "one structural failure buys a confirming retry, not an alarm"
    );
    assert!(
        app.auto_apply_manual_only
            .is_some_and(|manual| manual.retry_at.is_some())
            && app.auto_apply_structural_verdict.is_none(),
        "the one confirming retry"
    );
    fail(&mut app);
    let (_, _, actions) =
        health_row(&app).expect("the converged lane is announced the moment it converges");
    assert!(
        !actions
            .iter()
            .any(|a| matches!(a, aterm_messages::Intent::ApplyUpdate { .. })),
        "one decision per stopped lane (rulings 119/143): the press is never the \
         warning's: {actions:?}"
    );
    assert!(
        app.messages
            .live_by_key(crate::update_words::KEY_OUTCOME)
            .is_none(),
        "a lane with its re-sample still to come has not stopped, so no decision \
         row stands beside the warning (ruling 143)"
    );
    let deadline = app
        .auto_apply_manual_only
        .and_then(|manual| manual.retry_at)
        .expect("a structural convergence re-samples ONCE, a day out");
    assert!(
        app.auto_apply_structural_verdict
            .is_some_and(|verdict| verdict.resample_at == Some(deadline)),
        "the latch's deadline is the verdict's one re-sample"
    );
    // Once per build: a late or duplicate completion re-announces nothing and,
    // with the verdict standing, buys neither a confirming retry nor a new day.
    app.clear_messages_for_test();
    fail(&mut app);
    assert!(
        health_row(&app).is_none(),
        "the row was said once; a later failure of the same build is quiet"
    );
    assert_eq!(
        app.auto_apply_manual_only
            .and_then(|manual| manual.retry_at),
        Some(deadline),
        "the verdict's one re-sample keeps its deadline"
    );
}

#[cfg(unix)]
/// A spent unexplained budget converges into a quiet re-sample one epoch
/// cooldown out: the health row is posted once at convergence, the latch
/// lapses at its deadline and the intent re-arms, and a re-sample that fails
/// again converges quietly onto the next cooldown.
#[test]
fn a_spent_unexplained_budget_resamples_after_the_cooldown_and_announces_once() {
    let _ledger = crate::app_update_screen::hold_update_ledger_for_test();
    let mut app = App::headless_for_test();
    let build = app.native_updater_service.snapshot().current_build + 1;
    let ticket = crate::native_updater_service::ApplyAttemptTicket::for_test(
        build,
        PREFLIGHT_TEST_COMMIT,
        &"ab".repeat(32),
    );
    let fail = |app: &mut App| {
        ticket.make_current_apply_for_test(&mut app.native_updater_service);
        app.clear_messages_for_test();
        let outcome = app.abort_reaped_native_apply_before_reconcile(
            &ticket,
            "overlap handoff failed safely: handoff proof ended ChildDied".to_string(),
            HandoffFailureLane::Physical(PhysicalFailureShape::Unexplained),
        );
        app.surface_update_apply_outcome("automatic handoff", outcome, false);
        health_row(app)
    };
    let mut announced = Vec::new();
    for _ in 0..UNEXPLAINED_FAILURE_LIFETIME_ATTEMPTS {
        announced.push(fail(&mut app).is_some());
    }
    let mut expected = vec![false; usize::from(UNEXPLAINED_FAILURE_LIFETIME_ATTEMPTS)];
    *expected.last_mut().unwrap() = true;
    assert_eq!(
        announced, expected,
        "silent while the schedule runs, loud exactly once at convergence"
    );
    let latch = app
        .auto_apply_manual_only
        .expect("the converged lane is latched");
    assert!(
        latch.retry_at.is_some(),
        "an unexplained convergence re-samples; it never latches forever"
    );

    // The cooldown passes: the latch lapses and automatic apply is armable
    // for the same bytes again.
    app.auto_apply_manual_only = Some(crate::AutoApplyManualOnly {
        retry_at: Some(std::time::Instant::now() - std::time::Duration::from_secs(1)),
        ..latch
    });
    assert!(
        app.arm_native_auto_apply(build, &"ab".repeat(32)),
        "the lapsed re-sample re-arms the automatic lane"
    );

    // The re-sample fails too: converged again, onto the next cooldown, and
    // quiet — the row was already said for this build.
    assert!(
        fail(&mut app).is_none(),
        "a failed re-sample does not announce again"
    );
    assert!(
        app.auto_apply_manual_only
            .and_then(|manual| manual.retry_at)
            .is_some_and(|at| {
                at.saturating_duration_since(std::time::Instant::now())
                    > PHYSICAL_FAILURE_EPOCH_COOLDOWN - std::time::Duration::from_secs(60)
            }),
        "and it re-samples again one cooldown later"
    );
}

/// Every latch writes its standing note: the asynchronous completion lane queues its schedule's
/// note with the latch, and the surfacing writes it after the failure it explains (booking a
/// failure clears any standing refusal). Read back from the real ledger in a child process,
/// isolated from sibling tests.
#[cfg(target_os = "macos")]
#[test]
fn the_async_physical_lane_records_its_schedule_after_the_failure() {
    if !crate::test_alone::alone() {
        return;
    }

    let _ledger = crate::app_update_screen::hold_update_ledger_for_test();
    let mut app = App::headless_for_test();
    let current = app.native_updater_service.snapshot().current_build;
    let build = current + 7_211;
    let ticket = crate::native_updater_service::ApplyAttemptTicket::for_test(
        build,
        PREFLIGHT_TEST_COMMIT,
        &"ab".repeat(32),
    );
    ticket.make_current_apply_for_test(&mut app.native_updater_service);
    let outcome = app.abort_reaped_native_apply_before_reconcile(
        &ticket,
        "overlap handoff failed safely: handoff proof ended TimedOut".to_string(),
        HandoffFailureLane::Physical(PhysicalFailureShape::Transient),
    );
    let queued = app
        .apply_schedule_standing
        .clone()
        .expect("the async lane queues its schedule's standing note");
    // The production caller surfaces the returned outcome, which books the
    // failure and then writes the note.
    app.surface_update_apply_outcome_for_target("automatic handoff", outcome, false, build);
    assert!(
        app.apply_schedule_standing.is_none(),
        "the note is written once"
    );
    let report = aterm_update::apply_lane_report(current).expect("the ledger is readable");
    assert!(
        report.last_failure.contains("TimedOut"),
        "the failure was booked: {report:?}"
    );
    assert_eq!(
        report.last_refusal, queued,
        "…and the schedule stands beside it, not erased by it"
    );
}

/// The Staged bar never outlives its promise: a handoff worker that killed and reaped its child
/// latches manual-only on the physical budget, and a Staged bar still up for that build restates
/// at once. The gate's `seamless_capable` and the bar's posture read one predicate
/// (`App::seamless_handoff_unavailable`), so a headless `App` already paints the handoff-off
/// posture; a windowed app's promise is modelled onto the bar by hand so the restatement shows as
/// a change of words.
#[test]
fn a_reaped_abort_restates_the_staged_bar_it_stands_down() {
    use crate::update_words::{ApplyPosture, staged_detail};
    let mut app = App::headless_for_test();
    let build = app.native_updater_service.snapshot().current_build + 1;
    let staged = aterm_update::Progress::Staged {
        version: format!("1.0.{build}"),
        build,
    };
    app.note_update_progress(&staged);
    let ticket = crate::native_updater_service::ApplyAttemptTicket::for_test(
        build,
        PREFLIGHT_TEST_COMMIT,
        &"ab".repeat(32),
    );
    ticket.make_current_apply_for_test(&mut app.native_updater_service);
    // The staged build is a record under every posture but a stood-down one
    // where a press applies it.
    let recorded = |app: &App| {
        app.messages
            .log()
            .records()
            .rev()
            .find(|r| r.key.as_deref() == Some(crate::update_words::KEY_PROGRESS))
            .map(|r| r.detail.join("; "))
            .expect("the staged report is recorded")
    };
    // A headless App has no seamless lane and the report painted that (the
    // posture reads the gate's predicate); model a windowed app's promise so
    // the restatement below is a change of words.
    let unavailable = ApplyPosture::HandoffDisabled {
        why: app
            .seamless_handoff_unavailable()
            .expect("a headless App has no seamless lane"),
        veto: None,
        image_kept: false,
    };
    assert_eq!(recorded(&app), staged_detail(unavailable));
    // The ready row a windowed app with automatic install off paints.
    app.post_update_row(
        crate::update_words::progress(&staged, Some(ApplyPosture::ManualByConfig), ""),
        &format!("1.0.{build}"),
        crate::messages_host::FlowPhase::Staged { build },
    );
    assert_eq!(
        app.update_row_detail().as_deref(),
        Some(staged_detail(ApplyPosture::ManualByConfig).as_str())
    );

    app.abort_reaped_native_apply_before_reconcile(
        &ticket,
        "overlap handoff failed safely: handoff proof ended ChildDied".to_string(),
        HandoffFailureLane::Physical(PhysicalFailureShape::Transient),
    );

    let latched = app
        .auto_apply_manual_only
        .expect("a returned physical failure latches manual-only");
    assert_eq!(latched.build, build);
    let posture = app.apply_posture_for(build);
    assert_eq!(
        posture, unavailable,
        "this build's latch does not outrank a lane that cannot run"
    );
    // The posture is a record now: the ready row folded and its words
    // are on record; nothing is raised over a lane that cannot run.
    assert!(
        app.staged_update_row().is_none(),
        "a record posture raises no decision"
    );
    assert_eq!(recorded(&app), staged_detail(posture));
}

/// A native-close reducer `Err` is a failure, not a block: `Ok(false)` (an app kept its view) is
/// live user state, `Err` a broken reducer, driven here by removing the Settings view's instance
/// from the runtime (`RuntimeError::UnknownInstance`).
#[test]
fn a_broken_close_reducer_is_a_failure_not_a_block() {
    let _ledger = crate::app_update_screen::hold_update_ledger_for_test();
    let mut app = App::headless_for_test();
    let wid = crate::WindowId(0);
    assert!(
        app.open_settings_tab(crate::native_settings::SettingsRoute::Home),
        "PRECONDITION: a native view must exist for a close reducer to break"
    );
    let (instance, _view) = app
        .active_native_view(wid)
        .expect("PRECONDITION: the Settings tab is the active native view");

    let build = app.native_updater_service.snapshot().current_build + 1;
    stage_one_build_for_test(&mut app, build);
    // An ordinary transient blocker, constant across both phases, so the only
    // variable is whether the close reducer works; it also keeps the
    // preflight from answering Ready (a real park/spawn/exec).
    app.pending_restore = Some(crate::restore::RestoreManifest::new(Vec::new()));

    // Anti-vacuity: with the runtime intact this apply is not a failure, so
    // what the assertions below catch is the reducer error.
    force_auto_apply_attempt_now(&mut app);
    app.try_pending_native_auto_apply(false);
    assert!(
        app.auto_apply_intent
            .is_some_and(|intent| intent.attempts >= 1),
        "PRECONDITION: an intact runtime leaves the attempt on the cheap \
         Blocked lane with a live intent"
    );
    assert!(
        app.auto_apply_physical_retry.is_none(),
        "PRECONDITION: an ordinary block spends no physical budget"
    );

    // Break it: the view is still in the tab tree, its app instance is gone.
    assert!(
        app.native_runtime.remove_instance(instance).is_some(),
        "PRECONDITION: the instance existed to be removed"
    );

    force_auto_apply_attempt_now(&mut app);
    app.try_pending_native_auto_apply(false);

    assert!(
        app.auto_apply_intent.is_none(),
        "a broken close reducer must NOT be treated as a transient block — a \
         live intent would re-probe it every cooldown for the life of the process"
    );
    let latched = app
        .auto_apply_manual_only
        .expect("a reducer error takes the strict Failed lane and latches manual-only");
    assert_eq!(latched.build, build);
    assert_eq!(
        app.auto_apply_physical_retry.map(|retry| retry.cycles),
        Some(1),
        "and it spends the strict, converging budget rather than the cheap \
         preflight cooldown"
    );
}

#[cfg(unix)]
/// The retry spacing starts over after a long idle gap: a busy hour widens
/// the spacing to its last rung, a quiet half hour brings it back to the
/// first.
#[test]
fn an_idle_gap_restarts_the_activity_revoked_retry_spacing() {
    let rung = |n: u8| automatic_retry_delay(n, AutomaticRetryKind::ActivityRevoked);
    let mut app = App::headless_for_test();
    let ticket = crate::native_updater_service::ApplyAttemptTicket::for_test(
        77,
        "0123456789abcdef0123456789abcdef01234567",
        &"ab".repeat(32),
    );
    app.auto_overlap_retry = Some(crate::AutoOverlapRetry {
        build: 77,
        dmg_sha256: [0xab; 32],
        activation: false,
        cycles: ACTIVITY_REVOKED_LADDER_RUNGS,
        last_attempt: std::time::Instant::now(),
    });
    assert_eq!(
        app.arm_activity_revoked_overlap_retry(&ticket),
        rung(4),
        "past the last rung the spacing stays at the last rung"
    );
    app.auto_overlap_retry = Some(crate::AutoOverlapRetry {
        build: 77,
        dmg_sha256: [0xab; 32],
        activation: false,
        cycles: ACTIVITY_REVOKED_LADDER_RUNGS,
        last_attempt: std::time::Instant::now()
            - crate::ACTIVITY_RETRY_BUDGET_REPLENISH
            - std::time::Duration::from_secs(1),
    });
    assert_eq!(
        app.arm_activity_revoked_overlap_retry(&ticket),
        rung(0),
        "after a long idle gap the spacing starts over"
    );
}

#[test]
fn absent_auto_apply_intent_contributes_no_event_loop_deadline() {
    let other = std::time::Instant::now() + std::time::Duration::from_secs(7);
    assert_eq!(crate::fold_auto_apply_deadline(None, None), None);
    assert_eq!(
        crate::fold_auto_apply_deadline(None, Some(other)),
        Some(other)
    );

    let retry = other + std::time::Duration::from_secs(5);
    let intent = crate::AutoApplyIntent {
        build: 11,
        dmg_sha256: [0xab; 32],
        retry_at: retry,
        attempts: 1,
    };
    assert_eq!(
        crate::fold_auto_apply_deadline(Some(intent), None),
        Some(retry)
    );
    assert_eq!(
        crate::fold_auto_apply_deadline(Some(intent), Some(other)),
        Some(other),
        "the retry joins the existing minimum-deadline fold"
    );
}

#[test]
fn app_reduces_up_to_date_completion_and_broadcasts_revision() {
    let mut app = App::headless_for_test();
    assert!(app.open_settings_tab(crate::native_settings::SettingsRoute::SoftwareUpdate));
    let (_, view) = app
        .active_native_view(WindowId(0))
        .expect("active Settings view");
    let before_presentation = match app.native_runtime.view_state(view) {
        Some(crate::native_app::AppViewState::Settings(state)) => {
            state.common.presentation_revision
        }
        _ => panic!("Settings view state"),
    };

    let ticket = start(&mut app.native_updater_service);
    app.finish_native_update_check(ticket, status(None, 0));

    let after_presentation = match app.native_runtime.view_state(view) {
        Some(crate::native_app::AppViewState::Settings(state)) => {
            state.common.presentation_revision
        }
        _ => panic!("Settings view state after completion"),
    };
    assert!(after_presentation > before_presentation);
}

#[test]
fn rejected_native_completion_clears_the_settings_tab_busy_indicator() {
    let mut app = App::headless_for_test();
    assert!(app.open_settings_tab(crate::native_settings::SettingsRoute::Appearance));
    let wid = WindowId(0);
    let (instance, view) = app.active_native_view(wid).expect("active Settings view");
    let pending = app
        .native_runtime
        .dispatch(
            instance,
            view,
            AppEvent::Action(crate::native_app::ActionInvocation {
                id: crate::native_ui::ActionId::new(format!(
                    "settings/set/{}",
                    crate::prefs::EDIT_CURSOR_BLINK
                )),
                value: Some(crate::native_app::SemanticInput::Bool(false)),
            }),
        )
        .unwrap();
    let operation = pending
        .effects
        .iter()
        .find_map(|effect| match effect {
            AppEffect::ConfigPatch { reply, .. } => Some(reply.operation),
            _ => None,
        })
        .expect("Settings reducer owns pending config work");
    // The refresh the initiating dispatch performs, with the effect kept unexecuted.
    app.refresh_native_presentation(wid, instance, view);
    assert!(
        app.windows
            .get(&wid)
            .and_then(|window| window.tab_set.active())
            .unwrap()
            .presentation
            .indicators
            .busy
    );

    app.dispatch_native_completion(
        wid,
        instance,
        view,
        AppEvent::ConfigPatchFinished {
            operation,
            outcome: crate::native_app::ConfigPatchOutcome::Rejected {
                message: "read-only test config".to_string(),
            },
        },
    )
    .unwrap();
    assert!(
        !app.windows
            .get(&wid)
            .and_then(|window| window.tab_set.active())
            .unwrap()
            .presentation
            .indicators
            .busy,
        "completion-time presentation refresh removes stale busy state"
    );
}

/// A FOCUS THAT LEFT THE FOCUS ORDER RESUMES WHERE IT STOOD (s-25): a control that
/// disabled itself under the keyboard (the Activity pager's Older at its end) left
/// the order, and Tab jumped to the page's first control, Shift-Tab to its last.
/// Now Tab takes the nearest focusable after it in reading order, Shift-Tab the
/// nearest before. NEGATIVE CONTROL: with no focus at all, Tab starts at the first.
#[test]
fn tab_from_a_focus_that_left_the_order_resumes_where_it_stood() {
    let mut app = App::headless_for_test();
    assert!(app.open_settings_tab(crate::native_settings::SettingsRoute::Appearance));
    let wid = WindowId(0);
    let (_, view) = app.active_native_view(wid).expect("active Settings view");
    let compiled = app.compiled_native_ui(wid).expect("the page compiles");
    let order_of = |key: &crate::native_ui::UiKey| {
        compiled
            .semantics
            .iter()
            .position(|node| &node.key == key)
            .expect("in the tree")
    };
    // A node in the middle of the page that is not a stop of the focus order.
    let middle = order_of(&compiled.focus_order[compiled.focus_order.len() / 2]);
    let stranded = compiled.semantics[..middle]
        .iter()
        .rev()
        .map(|node| node.key.clone())
        .find(|key| !compiled.focus_order.contains(key))
        .expect("an unfocusable node before the middle control");
    let at = order_of(&stranded);
    let after = compiled
        .focus_order
        .iter()
        .find(|key| order_of(key) > at)
        .cloned()
        .expect("a control after it");
    let before = compiled
        .focus_order
        .iter()
        .rev()
        .find(|key| order_of(key) < at)
        .cloned()
        .expect("a control before it");
    let focus = |app: &App| {
        app.native_runtime
            .view_state(view)
            .and_then(|state| state.common().last_focus.clone())
    };
    let strand = |app: &mut App| {
        app.native_runtime
            .view_state_mut(view)
            .expect("the view")
            .common_mut()
            .last_focus = Some(stranded.clone());
    };
    strand(&mut app);
    app.move_native_focus(wid, false).unwrap();
    assert_eq!(
        focus(&app),
        Some(after.clone()),
        "Tab takes the next control"
    );
    strand(&mut app);
    app.move_native_focus(wid, true).unwrap();
    assert_eq!(focus(&app), Some(before), "Shift-Tab takes the one before");
    // NEGATIVE CONTROL: nothing focused starts at the first control.
    app.native_runtime
        .view_state_mut(view)
        .expect("the view")
        .common_mut()
        .last_focus = None;
    app.move_native_focus(wid, false).unwrap();
    assert_ne!(focus(&app), Some(after), "no focus is not a stranded one");
}

#[test]
fn tab_focus_walks_the_compiled_semantic_order() {
    let mut app = App::headless_for_test();
    assert!(app.open_settings_tab(crate::native_settings::SettingsRoute::Appearance));
    let wid = WindowId(0);
    let (_, view) = app.active_native_view(wid).expect("active Settings view");

    app.move_native_focus(wid, false).unwrap();
    let first = app
        .native_runtime
        .view_state(view)
        .and_then(|state| state.common().last_focus.clone())
        .expect("first focusable semantic node");
    app.move_native_focus(wid, false).unwrap();
    let second = app
        .native_runtime
        .view_state(view)
        .and_then(|state| state.common().last_focus.clone())
        .expect("second focusable semantic node");
    assert_ne!(first, second);

    app.move_native_focus(wid, true).unwrap();
    assert_eq!(
        app.native_runtime
            .view_state(view)
            .and_then(|state| state.common().last_focus.as_ref()),
        Some(&first)
    );
}

/// TAB WALKS THE WHOLE LIST: a Settings list has no Next button, so Tab from
/// the last control a screen shows moves on to the list's next one, Tab into
/// the list starts at its first, and the list scrolls to each; every control
/// Tab lands on is on screen.
#[test]
fn tab_walks_the_whole_settings_list() {
    let mut app = App::headless_for_test();
    assert!(app.open_settings_tab(crate::native_settings::SettingsRoute::Appearance));
    let wid = WindowId(0);
    let (_, view) = app.active_native_view(wid).expect("active Settings view");
    let last = crate::native_ui::UiKey::new("settings/control/matrix_rain.enabled");
    assert!(
        app.compiled_native_ui(wid)
            .unwrap()
            .semantic(&last)
            .is_none(),
        "the list's last control starts off screen"
    );
    let mut reached = false;
    for _ in 0..64 {
        app.move_native_focus(wid, false).unwrap();
        let focus = app
            .native_runtime
            .view_state(view)
            .and_then(|state| state.common().last_focus.clone())
            .expect("a focused control");
        assert!(
            app.compiled_native_ui(wid)
                .unwrap()
                .semantic(&focus)
                .is_some(),
            "{focus:?} is on screen"
        );
        if focus == last {
            reached = true;
            break;
        }
    }
    assert!(reached, "Tab reaches the list's last control");

    // Round to the search field, then into the list at its first control,
    // which the list scrolls back to.
    let search = crate::native_ui::UiKey::new("settings/search");
    let focus = |app: &App| {
        app.native_runtime
            .view_state(view)
            .and_then(|state| state.common().last_focus.clone())
    };
    for _ in 0..8 {
        if focus(&app).as_ref() == Some(&search) {
            break;
        }
        app.move_native_focus(wid, false).unwrap();
    }
    assert_eq!(focus(&app).as_ref(), Some(&search));
    app.move_native_focus(wid, false).unwrap();
    // The theme card leads with its Follow system switch.
    let first = crate::native_ui::UiKey::new("settings/theme-mode");
    assert_eq!(
        focus(&app).as_ref(),
        Some(&first),
        "Tab enters the list at its top"
    );
    assert!(
        app.compiled_native_ui(wid)
            .unwrap()
            .semantic(&first)
            .is_some()
    );
}

#[test]
fn focused_native_text_field_receives_space_and_submit_before_activation() {
    use aterm_types::keyboard::{Key, KeyEventType, Modifiers, NamedKey};

    let mut app = App::headless_for_test();
    let wid = WindowId(0);
    assert!(app.open_settings_tab(crate::native_settings::SettingsRoute::Appearance));
    app.dispatch_native_event(
        wid,
        AppEvent::Action(crate::native_app::ActionInvocation {
            id: crate::native_ui::ActionId::new("settings/search"),
            value: None,
        }),
    )
    .unwrap();

    let press = |key| crate::input::InputEvent::Key {
        key: Key::Named(key),
        mods: Modifiers::empty(),
        base_layout: None,
        layout_text: None,
        event_type: KeyEventType::Press,
    };
    assert!(app.native_input_event(wid, &press(NamedKey::Space)));
    let (_, view) = app.active_native_view(wid).unwrap();
    let Some(crate::native_app::AppViewState::Settings(state)) =
        app.native_runtime.view_state(view)
    else {
        panic!("Settings view");
    };
    assert_eq!(state.search_input.value(), " ");

    app.dispatch_native_event(
        wid,
        AppEvent::TextInput(TextInputEvent::Preedit {
            text: "候補".to_string(),
            selection: None,
        }),
    )
    .unwrap();
    assert!(app.native_input_event(wid, &press(NamedKey::Enter)));
    let Some(crate::native_app::AppViewState::Settings(state)) =
        app.native_runtime.view_state(view)
    else {
        panic!("Settings view");
    };
    assert_eq!(state.search_input.value(), " ");
    assert!(
        state.search_input.preedit().is_none(),
        "Return is Submit for a focused text field, not generic activation"
    );
}

/// Searching a font list through the real key path: once a name is begun,
/// Space is part of it ("iosevka t") instead of pressing the highlighted
/// font, and the list stays open on what was typed.
#[test]
fn space_types_into_a_font_list_search_once_a_name_is_begun() {
    use aterm_types::keyboard::{Key, KeyEventType, Modifiers, NamedKey};

    let mut app = App::headless_for_test();
    let wid = WindowId(0);
    assert!(app.open_settings_tab(crate::native_settings::SettingsRoute::TextFonts));
    app.dispatch_native_event(
        wid,
        AppEvent::Action(crate::native_app::ActionInvocation {
            id: crate::native_ui::ActionId::new("settings/font-picker/font_family"),
            value: None,
        }),
    )
    .unwrap();
    let press = |key| crate::input::InputEvent::Key {
        key,
        mods: Modifiers::empty(),
        base_layout: None,
        layout_text: None,
        event_type: KeyEventType::Press,
    };
    let takes_space = |app: &App| {
        let (_, view) = app.active_native_view(wid).unwrap();
        let Some(crate::native_app::AppViewState::Settings(state)) =
            app.native_runtime.view_state(view)
        else {
            panic!("Settings view");
        };
        state.font_picker_takes_space()
    };
    assert!(!takes_space(&app), "an empty search leaves Space to press");
    // Where Iosevka Term sits in the whole list, after Automatic: the bundled
    // faces lead it in the platform's order (JetBrains Mono first off macOS).
    let iosevka_key = |app: &App| {
        let compiled = app.compiled_native_ui(wid).unwrap();
        (0..64)
            .map(|at| format!("settings/choice/font_family/{at}"))
            .find(|key| {
                compiled
                    .semantic(&crate::native_ui::UiKey::new(key.as_str()))
                    .is_some_and(|option| option.label == "Iosevka Term")
            })
    };
    let iosevka = iosevka_key(&app).expect("the open list offers Iosevka Term");
    assert_ne!(
        iosevka, "settings/choice/font_family/0",
        "Automatic leads the list"
    );
    for ch in "iosevka".chars() {
        assert!(app.native_input_event(wid, &press(Key::Character(ch))));
    }
    assert!(takes_space(&app));
    assert!(app.native_input_event(wid, &press(Key::Named(NamedKey::Space))));
    assert!(app.native_input_event(wid, &press(Key::Character('t'))));
    assert!(takes_space(&app), "the list is still open");
    let compiled = app.compiled_native_ui(wid).unwrap();
    let title = compiled
        .semantic(&crate::native_ui::UiKey::new(
            "settings/choice-title/font_family",
        ))
        .expect("the font list is open");
    assert!(
        title.label.contains("\u{201c}iosevka t\u{201d}"),
        "{}",
        title.label
    );
    // Options keep their places while typing narrows the list: Iosevka Term
    // is still the option it was in the whole list, its place among the faces
    // aterm carries, after Automatic — the first of them on macOS, the second
    // on Linux, the fourth on Windows (`bundled::selectable_families`).
    let place = 1 + aterm_render::bundled::selectable_families()
        .iter()
        .position(|family| *family == "Iosevka Term")
        .expect("Iosevka Term is bundled");
    assert_eq!(iosevka, format!("settings/choice/font_family/{place}"));
    let option = compiled.semantic(&crate::native_ui::UiKey::new(iosevka.as_str()));
    assert!(
        option.is_some_and(|option| option.label == "Iosevka Term"),
        "option {place}: {:?}",
        option.map(|option| &option.label)
    );
}

/// The keypad on a native page through the real key path (`App::on_key` ->
/// `on_key_native_mode` -> `keymap::build_key_input` -> `App::input` ->
/// `native_input_event`): KP_5 reaches the reducer as `Numpad5` and types
/// `5` in a focused Settings search field; NumLock-off KP_Decimal
/// (`NumpadDelete`) is a forward delete.
#[test]
fn keypad_digit_and_delete_reach_a_focused_settings_field() {
    use aterm_window::event::{ElementState, KeyEvent};
    use aterm_window::keyboard::{
        Key as WinitKey, KeyCode, KeyLocation, NamedKey as WinitNamed, PhysicalKey, SmolStr,
    };

    let mut app = App::headless_for_test();
    let wid = WindowId(0);
    assert!(app.open_settings_tab(crate::native_settings::SettingsRoute::Appearance));
    app.dispatch_native_event(
        wid,
        AppEvent::Action(crate::native_app::ActionInvocation {
            id: crate::native_ui::ActionId::new("settings/search"),
            value: None,
        }),
    )
    .unwrap();
    let (_, view) = app.active_native_view(wid).unwrap();
    let search_value = |app: &App| {
        let Some(crate::native_app::AppViewState::Settings(state)) =
            app.native_runtime.view_state(view)
        else {
            panic!("Settings view");
        };
        state.search_input.value().to_string()
    };
    // A keypad press exactly as the desktop backends deliver it: the
    // keypad in `location`, the layout's glyph (or NumLock-off name) in
    // `logical_key`, the glyph's text alongside a `Character`.
    let keypad = |code: KeyCode, logical: WinitKey| {
        let text = match &logical {
            WinitKey::Character(s) => Some(s.clone()),
            _ => None,
        };
        KeyEvent::synthetic_for_test(
            PhysicalKey::Code(code),
            logical,
            text,
            KeyLocation::Numpad,
            ElementState::Pressed,
            false,
        )
    };

    app.on_key(
        wid,
        keypad(KeyCode::Numpad5, WinitKey::Character(SmolStr::new("5"))),
    );
    assert_eq!(
        search_value(&app),
        "5",
        "KP_5 types its glyph into the focused field"
    );
    app.on_key(
        wid,
        keypad(KeyCode::Numpad1, WinitKey::Character(SmolStr::new("1"))),
    );
    assert_eq!(search_value(&app), "51");

    // Caret to the start, then the NumLock-off decimal key: a forward
    // delete of the `5`.
    app.dispatch_native_event(
        wid,
        AppEvent::TextInput(TextInputEvent::Home { extend: false }),
    )
    .unwrap();
    app.on_key(
        wid,
        keypad(KeyCode::NumpadDecimal, WinitKey::Named(WinitNamed::Delete)),
    );
    assert_eq!(
        search_value(&app),
        "1",
        "NumLock-off KP_Decimal is a forward delete"
    );
}

#[test]
fn bare_return_activates_the_pages_primary_default_button() {
    use aterm_types::keyboard::{Key, KeyEventType, Modifiers, NamedKey};

    // About route: `navigate()` anchors focus on the page container (a
    // non-actionable Group), so a fresh page has no activatable focus and
    // "Copy Build Information" is its Primary. A bare Return fires it; Space
    // must not (on macOS Space only activates the focused control).
    let mut app = App::headless_for_test();
    let wid = WindowId(0);
    // A regular-width viewport: the default headless grid is Compact, where
    // the paginated About page has no action row and so no default.
    if let Some(ws) = app.windows.get_mut(&wid) {
        ws.cols = 220;
        ws.rows = 60;
    }
    assert!(app.open_settings_tab(crate::native_settings::SettingsRoute::About));
    let (_, view) = app.active_native_view(wid).unwrap();
    assert!(
        !app.activate_native_focus(wid).unwrap_or(false),
        "fresh page must hold no ACTIVATABLE focus for this test to exercise the fallback"
    );

    let press = |key| crate::input::InputEvent::Key {
        key: Key::Named(key),
        mods: Modifiers::empty(),
        base_layout: None,
        layout_text: None,
        event_type: KeyEventType::Press,
    };
    assert!(app.native_input_event(wid, &press(NamedKey::Space)));
    let Some(crate::native_app::AppViewState::Settings(state)) =
        app.native_runtime.view_state(view)
    else {
        panic!("Settings view");
    };
    assert_eq!(
        state.feedback, None,
        "unfocused Space must not trigger the default button"
    );

    assert!(app.native_input_event(wid, &press(NamedKey::Enter)));
    let Some(crate::native_app::AppViewState::Settings(state)) =
        app.native_runtime.view_state(view)
    else {
        panic!("Settings view");
    };
    assert!(
        state.feedback.is_some(),
        "unfocused Return must fire the page's Primary (default) button"
    );
}

#[test]
fn settings_search_field_honors_readline_control_keys() {
    use aterm_types::keyboard::{Key, KeyEventType, Modifiers};

    let mut app = App::headless_for_test();
    let wid = WindowId(0);
    assert!(app.open_settings_tab(crate::native_settings::SettingsRoute::Appearance));
    app.dispatch_native_event(
        wid,
        AppEvent::Action(crate::native_app::ActionInvocation {
            id: crate::native_ui::ActionId::new("settings/search"),
            value: None,
        }),
    )
    .unwrap();
    app.dispatch_native_event(
        wid,
        AppEvent::TextInput(TextInputEvent::Commit("cursor trail".to_string())),
    )
    .unwrap();

    let ctrl = |c| crate::input::InputEvent::Key {
        key: Key::Character(c),
        mods: Modifiers::CTRL,
        base_layout: None,
        layout_text: None,
        event_type: KeyEventType::Press,
    };
    let (_, view) = app.active_native_view(wid).unwrap();
    let search = |app: &App| match app.native_runtime.view_state(view) {
        Some(crate::native_app::AppViewState::Settings(state)) => (
            state.search_input.value().to_string(),
            state.search_input.selection().range(),
        ),
        _ => panic!("Settings view"),
    };

    // Ctrl-A: caret to the start (not select-all, not swallowed).
    assert!(app.native_input_event(wid, &ctrl('a')));
    assert_eq!(search(&app), ("cursor trail".to_string(), 0..0));
    // Ctrl-F / Ctrl-B: caret right then left.
    assert!(app.native_input_event(wid, &ctrl('f')));
    assert_eq!(search(&app).1, 1..1);
    assert!(app.native_input_event(wid, &ctrl('b')));
    assert_eq!(search(&app).1, 0..0);
    // Ctrl-D: forward-delete at the caret.
    assert!(app.native_input_event(wid, &ctrl('d')));
    assert_eq!(search(&app), ("ursor trail".to_string(), 0..0));
    // Ctrl-E: caret to the end; Ctrl-W: previous word dies.
    assert!(app.native_input_event(wid, &ctrl('e')));
    assert_eq!(search(&app).1, 11..11);
    assert!(app.native_input_event(wid, &ctrl('w')));
    assert_eq!(search(&app).0, "ursor ");
    // Ctrl-K after Ctrl-A kills the whole line; Ctrl-U from the end does too.
    assert!(app.native_input_event(wid, &ctrl('a')));
    assert!(app.native_input_event(wid, &ctrl('k')));
    assert_eq!(search(&app).0, "");
    app.dispatch_native_event(
        wid,
        AppEvent::TextInput(TextInputEvent::Commit("beam".to_string())),
    )
    .unwrap();
    assert!(app.native_input_event(wid, &ctrl('u')));
    assert_eq!(search(&app).0, "");
}

/// Off macOS, Ctrl+←/→ and Ctrl+⌫ are word operations in Settings text fields (the cfg'd
/// `alt_word || readline` term); the Ctrl-letter readline arms stay by-character because they
/// match first.
#[cfg(not(target_os = "macos"))]
#[test]
fn settings_search_field_ctrl_arrows_walk_words_off_macos() {
    use aterm_types::keyboard::{Key, KeyEventType, Modifiers, NamedKey};

    let mut app = App::headless_for_test();
    let wid = WindowId(0);
    assert!(app.open_settings_tab(crate::native_settings::SettingsRoute::Appearance));
    app.dispatch_native_event(
        wid,
        AppEvent::Action(crate::native_app::ActionInvocation {
            id: crate::native_ui::ActionId::new("settings/search"),
            value: None,
        }),
    )
    .unwrap();
    app.dispatch_native_event(
        wid,
        AppEvent::TextInput(TextInputEvent::Commit("cursor trail".to_string())),
    )
    .unwrap();

    let chord = |key, mods| crate::input::InputEvent::Key {
        key,
        mods,
        base_layout: None,
        layout_text: None,
        event_type: KeyEventType::Press,
    };
    let left = |mods| chord(Key::Named(NamedKey::ArrowLeft), mods);
    let right = |mods| chord(Key::Named(NamedKey::ArrowRight), mods);
    let (_, view) = app.active_native_view(wid).unwrap();
    let search = |app: &App| match app.native_runtime.view_state(view) {
        Some(crate::native_app::AppViewState::Settings(state)) => (
            state.search_input.value().to_string(),
            state.search_input.selection().range(),
        ),
        _ => panic!("Settings view"),
    };

    // Alt+← is the existing word motion; note where it lands from the end.
    assert!(app.native_input_event(wid, &left(Modifiers::ALT)));
    let alt_word_stop = search(&app).1;
    // Back to the end, then Ctrl+←: the same stop.
    assert!(app.native_input_event(wid, &chord(Key::Character('e'), Modifiers::CTRL)));
    assert!(app.native_input_event(wid, &left(Modifiers::CTRL)));
    assert_eq!(search(&app).1, alt_word_stop, "Ctrl+← = Alt+← (word left)");
    assert!(app.native_input_event(wid, &left(Modifiers::CTRL)));
    assert_eq!(search(&app).1, 0..0, "second Ctrl+← reaches the start");
    // Ctrl+→ lands where Alt+→ does: the end of the word (emacs-style), not `alt_word_stop`.
    assert!(app.native_input_event(wid, &right(Modifiers::CTRL)));
    let ctrl_right_stop = search(&app).1;
    assert!(app.native_input_event(wid, &left(Modifiers::CTRL)));
    assert_eq!(search(&app).1, 0..0, "Ctrl+← undoes the word step");
    assert!(app.native_input_event(wid, &right(Modifiers::ALT)));
    assert_eq!(
        search(&app).1,
        ctrl_right_stop,
        "Ctrl+→ = Alt+→ (word right)"
    );
    // Ctrl+⌫ from the end kills the last word — exactly Ctrl+W's cut.
    assert!(app.native_input_event(wid, &chord(Key::Character('e'), Modifiers::CTRL)));
    assert!(app.native_input_event(
        wid,
        &chord(Key::Named(NamedKey::Backspace), Modifiers::CTRL)
    ));
    assert_eq!(search(&app).0, "cursor ", "Ctrl+⌫ = backward-kill-word");
    // The Ctrl-letter readline arms match first: ^B is still one character.
    assert!(app.native_input_event(wid, &chord(Key::Character('b'), Modifiers::CTRL)));
    assert_eq!(search(&app).1, 6..6, "^B stays by-character");
}

#[test]
fn explicit_font_metrics_seed_additional_headless_windows() {
    let mut app = App::headless_for_test();
    app.backend.activate_px(16.0);
    app.font_px = 16.0;
    app.backend.set_pad(crate::pad_for_scale(1.0));
    app.backend.set_head(0);
    let expected = app.unattached_window_metrics();
    // A window installed after startup takes the renderer's metrics.
    let sid = app.next_session_id;
    let additional = app.insert_logical_window(crate::stub_session(sid), 24, 80);
    assert_eq!(app.windows[&additional].metrics, expected);
    assert_eq!(app.win_cell_size(additional), app.cell_size());
    assert_eq!(app.win_pad(additional), app.backend.pad());
}

/// Tier-1 conformance for the exact-observation handoff: a failed reconciliation keeps the
/// deferred external bytes, and the retry admits that generation. The negative control shows the
/// model rejects the lost-candidate state.
#[test]
fn exact_observation_handoff_conforms_and_preserves_failed_reconciliation() {
    fn project(
        model: &aterm_spec::derive::Model,
        app: &App,
        phase: i64,
        sampled: i64,
        admitted: i64,
        reconciliation_failed: bool,
    ) -> aterm_spec::interp::State {
        let pending = i64::from(app.native_config_external_pending.is_some());
        let mut state = model.init_state();
        state.insert("phase", phase);
        state.insert("pending", pending);
        state.insert("sampled", sampled);
        state.insert(
            "gate",
            i64::from(app.native_config_service.reconciliation_required() || pending == 1),
        );
        state.insert("queued", i64::from(!app.native_config_pending.is_empty()));
        state.insert("admitted", admitted);
        state.insert("reconciliation_failed", i64::from(reconciliation_failed));
        state
    }

    fn assert_step(
        model: &aterm_spec::derive::Model,
        before: &aterm_spec::interp::State,
        after: &aterm_spec::interp::State,
        action: &str,
    ) {
        assert_eq!(
            model.successors(action, before).as_slice(),
            std::slice::from_ref(after),
            "shipping transition must refine {action}"
        );
        for invariant in &model.invariants {
            assert!(
                model.check_invariant(invariant.name, after),
                "post-state violates {}::{}: {after:?}",
                model.name,
                invariant.name
            );
        }
    }

    let root = std::env::temp_dir().join(format!(
        "aterm-config-observation-handoff-{}-{:?}",
        std::process::id(),
        std::thread::current().id()
    ));
    let _ = std::fs::remove_dir_all(&root);
    std::fs::create_dir_all(&root).unwrap();
    let path = root.join("aterm.toml");
    std::fs::write(&path, "serious_mode = false\n").unwrap();

    let model = aterm_models_native::native_config_observation_handoff_model();
    let mut app = App::headless_for_test();
    app.native_config_service =
        crate::native_config_service::VersionedConfigService::load_path(&path).unwrap();
    let mut state = project(&model, &app, 0, 0, 0, false);
    assert_eq!(state, model.init_state());

    app.native_config_inflight = true;
    let after = project(&model, &app, 1, 0, 0, false);
    assert_step(&model, &state, &after, "BeginWrite");
    state = after;

    app.enqueue_serious_mode_intent().unwrap();
    let after = project(&model, &app, 1, 0, 0, false);
    assert_step(&model, &state, &after, "QueueWrite");
    state = after;

    std::fs::write(&path, "serious_mode = true\n").unwrap();
    let observation =
        crate::native_config_service::VersionedConfigService::observe_path(&path, false).unwrap();
    assert!(
        app.sync_native_config_external_observation(observation.clone())
            .unwrap()
            .is_none()
    );
    let after = project(&model, &app, 1, 0, 0, false);
    assert_step(&model, &state, &after, "ObserveFirst");
    state = after;

    app.finish_native_config_write(
        NativeConfigOrigin::SeriousMode { desired: false },
        NativeConfigPersistenceCompletion {
            outcome: ConfigPatchOutcome::Conflict {
                revision: app.native_config_service.snapshot().revision,
            },
            observation: Err("publication was overtaken".to_string()),
        },
    );
    let after = project(&model, &app, 0, 0, 0, false);
    assert_step(&model, &state, &after, "FinishWrite");
    state = after;
    assert_eq!(app.native_config_pending.len(), 1);
    assert!(app.native_config_external_pending.is_some());

    app.native_config_inflight = true;
    let after = project(&model, &app, 2, 1, 0, false);
    assert_step(&model, &state, &after, "StartReconcile");
    state = after;
    app.finish_native_config_reconciliation(NativeConfigReconciliationCompletion {
        pending_sequence: app.native_config_external_sequence,
        observation: Err("transient stable-read failure".to_string()),
    });
    let after = project(&model, &app, 0, 0, 0, true);
    assert_step(&model, &state, &after, "FailReconcile");
    state = after;
    // The failed reconciliation answers its queued caller with the error (see
    // `failed_reconciliation_answers_queued_callers_…`); the external observation is its own
    // pending state and survives for the retry.
    assert_eq!(app.native_config_pending.len(), 0);
    assert!(app.native_config_external_pending.is_some());

    let themes = std::sync::Arc::clone(&app.native_config_service.snapshot().assets.themes);
    let reconciled = crate::native_config_service::VersionedConfigService::prepare_observation(
        observation,
        themes,
    )
    .unwrap();
    app.native_config_inflight = true;
    let after = project(&model, &app, 2, 1, 0, false);
    assert_step(&model, &state, &after, "RetryReconcile");
    state = after;
    app.finish_native_config_reconciliation(NativeConfigReconciliationCompletion {
        pending_sequence: app.native_config_external_sequence,
        observation: Ok(reconciled),
    });
    let after = project(&model, &app, 0, 0, 1, false);
    assert_step(&model, &state, &after, "AdmitExact");
    assert_eq!(
        app.native_config_service.snapshot().text.as_ref(),
        "serious_mode = true\n"
    );
    assert!(app.native_config_external_pending.is_none());
    // Still empty: the queued caller was answered at FailReconcile.
    assert_eq!(app.native_config_pending.len(), 0);

    let mut lost = after;
    lost.insert("dropped_candidate", 1);
    assert!(
        !model.check_invariant("DeferredGenerationNeverLost", &lost),
        "negative control: dropping the failed observation must be rejected"
    );
    let _ = std::fs::remove_dir_all(root);
}

#[test]
fn matching_prepared_generation_reconciliation_admits_without_requeue() {
    let root = std::env::temp_dir().join(format!(
        "aterm-config-prepared-reconcile-{}-{:?}",
        std::process::id(),
        std::thread::current().id()
    ));
    let _ = std::fs::remove_dir_all(&root);
    std::fs::create_dir_all(&root).unwrap();
    let path = root.join("aterm.toml");
    std::fs::write(&path, "serious_mode = false\n").unwrap();

    let mut app = App::headless_for_test();
    app.native_config_service =
        crate::native_config_service::VersionedConfigService::load_path(&path).unwrap();
    std::fs::write(&path, "serious_mode = true\n").unwrap();
    let observation =
        crate::native_config_service::VersionedConfigService::observe_path(&path, false).unwrap();
    let themes = std::sync::Arc::clone(&app.native_config_service.snapshot().assets.themes);
    let prepared = crate::native_config_service::VersionedConfigService::prepare_observation(
        observation,
        themes,
    )
    .unwrap();
    let expected_assets = std::sync::Arc::clone(&prepared.assets);
    let generation = crate::native_font_catalog::PreparedConfigGeneration {
        observation: prepared.observation.clone(),
        config: prepared.config.clone(),
        values: prepared.values.clone(),
        disk_faults: std::sync::Arc::clone(&prepared.disk_faults),
        assets: std::sync::Arc::clone(&prepared.assets),
        path_feed_fps: prepared.config.path_feed_fingerprints(),
        sparkle: prepared.config.prepare_sparkle_runtime(),
        fonts: None,
        warnings: Vec::new(),
        font_refusals: Vec::new(),
        reason: crate::app_colorscheme::ThemeChange::ConfigReload,
    };

    app.native_config_service.mark_reconciliation_required();
    app.defer_prepared_config_generation(generation);
    let deferred_sequence = app.native_config_external_sequence;
    assert!(app.native_config_external_pending.is_some());
    app.native_config_inflight = true;

    app.finish_native_config_reconciliation(NativeConfigReconciliationCompletion {
        pending_sequence: deferred_sequence,
        observation: Ok(prepared),
    });

    assert!(!app.native_config_inflight);
    assert!(!app.native_config_service.reconciliation_required());
    assert!(app.native_config_external_pending.is_none());
    assert_eq!(app.native_config_external_sequence, deferred_sequence);
    let snapshot = app.native_config_service.snapshot();
    assert_eq!(snapshot.text.as_ref(), "serious_mode = true\n");
    assert!(std::sync::Arc::ptr_eq(&snapshot.assets, &expected_assets));
    assert!(app.config.serious_mode_or_default());
    let _ = std::fs::remove_dir_all(root);
}

/// Keep-stale must not swap rungs: on a contended terminal lock,
/// `refresh_active_split_presentation` keeps this tab's own previous title
/// (or the titleless `aterm`) and never imports the window's
/// `tab_title_cache` (a different rung that can hold `meta set title` or the
/// abbreviated cwd).
#[test]
fn contended_split_presentation_keeps_its_own_title_rung() {
    let mut app = App::headless_for_test();
    let wid = WindowId(0);
    let term = app.pool.get(0).expect("session 0").term.clone();
    crate::term_lock(&term).process(b"\x1b]0;osc-live\x07");

    let active_title = |app: &App| {
        app.windows[&wid]
            .tab_set
            .active()
            .expect("active tab")
            .presentation
            .title
            .clone()
    };

    // Uncontended: the fold publishes its own rung, the live OSC title.
    app.refresh_active_split_presentation(wid);
    assert_eq!(active_title(&app), "osc-live");

    // A foreign rung in the tab-label cache: what `tab_titles` caches for a cwd-only pane.
    app.windows
        .get_mut(&wid)
        .expect("test window")
        .tab_title_cache
        .insert(0, "~/repo".to_string());

    // Contended: holding the terminal mutex sends the leaf read down its WouldBlock arm.
    let flood = crate::term_lock(&term);
    app.refresh_active_split_presentation(wid);
    drop(flood);

    assert_eq!(
        active_title(&app),
        "osc-live",
        "contention must keep this tab's own OSC-title rung, never the \
         tab-label cache's cwd/operator rung"
    );

    // A tab with no title yet: keep-stale lands on the titleless pane's
    // `aterm`, never another rung's label.
    app.windows
        .get_mut(&wid)
        .expect("test window")
        .tab_set
        .active_mut()
        .expect("active tab")
        .presentation
        .title
        .clear();
    let flood = crate::term_lock(&term);
    app.refresh_active_split_presentation(wid);
    drop(flood);
    assert_eq!(active_title(&app), "aterm");
}

/// The watcher is the one writer of the harness policy: a valid edit A still in the config lane
/// when the next save R was refused cannot lift R's kill switch when A lands
/// (`apply_prepared_config_generation` admits A's settings under the running policy). Controls:
/// A's own parse carries approvals on, and the watcher's next policy post runs at once.
#[test]
fn a_late_admission_never_moves_the_watchers_harness_policy() {
    use crate::app_config::{Config, HarnessPolicy};
    let dir = std::env::temp_dir().join(format!("aterm-one-harness-writer-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    let path = dir.join("aterm.toml");
    let older = "font_px = 15\n[harness]\napprove = \"all\"\n";
    std::fs::write(&path, older).unwrap();
    let observation =
        crate::native_config_service::VersionedConfigService::observe_path(&path, false).unwrap();
    let config = Config::parse(older).unwrap();
    assert!(
        config.harness.policy.enabled,
        "control: A carries approvals on"
    );
    let assets = config.resolve_asset_catalog();
    let feeds = config.path_feed_fingerprints();
    let sparkle = config.prepare_sparkle_runtime();
    let generation = crate::native_font_catalog::PreparedConfigGeneration {
        observation,
        config,
        values: std::collections::BTreeMap::new(),
        disk_faults: std::sync::Arc::default(),
        assets,
        path_feed_fps: feeds,
        sparkle,
        fonts: None,
        warnings: Vec::new(),
        font_refusals: Vec::new(),
        reason: crate::app_colorscheme::ThemeChange::ConfigReload,
    };

    let mut app = App::headless_for_test();
    let mut stamps = crate::harness_policy_cell::HarnessStampSource::new();
    let refused = HarnessPolicy::read("# caf\u{fffd}\n[harness]\nenabled = false\n");
    assert!(!refused.policy.enabled);
    app.adopt_harness_policy(stamps.stamp(refused.clone()));

    app.apply_prepared_config_generation(generation);
    assert_eq!(app.config.font_px, Some(15.0), "A's settings are admitted");
    assert_eq!(
        *app.harness_policy.get(),
        refused,
        "A's late admission cannot lift the kill switch the newer read posted"
    );
    assert_eq!(app.config.harness, refused, "and Settings shows what runs");

    let lifted = HarnessPolicy::read("[harness]\napprove = \"all\"\n");
    app.adopt_harness_policy(stamps.stamp(lifted.clone()));
    assert_eq!(
        *app.harness_policy.get(),
        lifted,
        "the watcher's next post runs at once"
    );
    assert_eq!(app.config.harness, lifted);
    let _ = std::fs::remove_dir_all(dir);
}

/// Run one queued control `settings set` request through the lane the way the
/// worker would: reduce it against the service and publish its completion.
/// Returns the caller's reply and the authoritative snapshot.
fn complete_control_theme_set(
    app: &mut App,
    value: &str,
) -> (
    Result<String, String>,
    crate::native_config_service::ConfigSnapshot,
) {
    complete_control_theme_field(app, Some(value))
}

fn complete_control_theme_field(
    app: &mut App,
    value: Option<&str>,
) -> (
    Result<String, String>,
    crate::native_config_service::ConfigSnapshot,
) {
    let (reply, completion) = std::sync::mpsc::channel();
    app.enqueue_control_settings_field_intent(
        crate::prefs::EDIT_THEME.to_string(),
        value.map(str::to_string),
        reply,
    );
    let snapshot = complete_queued_control_theme_field(app);
    (completion.try_recv().expect("answered"), snapshot)
}

fn complete_queued_control_theme_field(
    app: &mut App,
) -> crate::native_config_service::ConfigSnapshot {
    let NativeConfigRequest { origin, work } = app.native_config_pending.pop_front().unwrap();
    assert!(matches!(&origin, NativeConfigOrigin::Control { .. }));
    let (outcome, snapshot) = match app.reduce_native_config_work(work).unwrap() {
        ConfigPatchResult::Applied { snapshot, undo } => (
            ConfigPatchOutcome::Applied {
                revision: snapshot.revision,
                undo: Some(undo.get()),
            },
            snapshot,
        ),
        ConfigPatchResult::Unchanged { snapshot } => (
            ConfigPatchOutcome::Applied {
                revision: snapshot.revision,
                undo: None,
            },
            snapshot,
        ),
        other => panic!("a theme write must apply: {other:?}"),
    };
    app.publish_native_config_origin(origin, outcome, Some(snapshot.clone()), false, None);
    snapshot
}

/// `settings set theme Nrod` (and `theme set Nrod`, the same lane) saves, but
/// its OK says the name is unavailable and Default is drawn. Negative control:
/// a name the catalog resolves says nothing more than `saved`.
#[test]
fn control_theme_set_of_an_unavailable_name_says_so_in_the_reply() {
    let mut app = App::headless_for_test();
    app.native_config_service = crate::native_config_service::VersionedConfigService::new(
        "theme = \"Dracula\"\n".to_string(),
    )
    .unwrap();
    let (reply, _) = complete_control_theme_set(&mut app, "Nrod");
    let reply = reply.expect("the write itself succeeds");
    assert!(reply.starts_with("saved: theme = Nrod; "), "{reply}");
    assert!(
        reply.contains("\"Nrod\" is unavailable") && reply.contains("Default is used"),
        "{reply}"
    );

    let (reply, _) = complete_control_theme_set(&mut app, "Nord");
    assert_eq!(reply.as_deref(), Ok("saved: theme = Nord"));

    // A selector issue is a miss too.
    let (reply, _) = complete_control_theme_set(&mut app, "dark:Nord,dusk:Dracula");
    let reply = reply.expect("saved");
    assert!(reply.contains("; "), "the selector issue is named: {reply}");
}

/// `settings set theme dark:Work,light:GitHub Light` against a catalog that
/// holds Work saves with no miss, and once the write is adopted the side the
/// OS appearance picks is what every session draws.
#[test]
fn control_theme_set_of_a_split_installs_the_os_side() {
    let mut app = App::headless_for_test();
    app.native_config_service = crate::native_config_service::VersionedConfigService::new(
        "theme = \"Dracula\"\n".to_string(),
    )
    .unwrap();
    let work = aterm_types::ColorScheme {
        background: aterm_types::Rgb::new(0x10, 0x20, 0x30),
        ..aterm_types::ColorScheme::default()
    };
    app.reload_theme_catalog(crate::app_config::ThemeCatalog::from_schemes([(
        "Work".to_string(),
        work.clone(),
    )]));

    let (reply, snapshot) = complete_control_theme_set(&mut app, "dark:Work,light:GitHub Light");
    assert_eq!(
        reply.as_deref(),
        Ok("saved: theme = dark:Work,light:GitHub Light"),
        "both sides resolve, so nothing is unavailable"
    );

    // The watcher's reload adopts the written config.
    app.config = (*snapshot.config).clone();
    app.install_resolved_colors(crate::app_colorscheme::ThemeChange::ConfigReload);
    let session_bg = |app: &App| {
        crate::term_lock(&app.pool.get(0).expect("session 0").term).default_background()
    };
    assert_eq!(
        app.installed_colors.id, "Work",
        "a dark OS draws the dark side"
    );
    assert_eq!(app.theme, aterm_render::Theme::from(&work));
    assert_eq!(session_bg(&app), work.background);

    app.sync_app_theme_to_appearance(aterm_types::Appearance::Light);
    let light = aterm_types::scheme::builtin("GitHub Light").unwrap();
    assert_eq!(
        app.installed_colors.id, "GitHub Light",
        "a light OS the light side"
    );
    assert_eq!(session_bg(&app), light.background);
}

/// Run the palette's one queued theme-choice request through the lane the way
/// the worker would, returning the authoritative snapshot.
fn complete_theme_choice(app: &mut App) -> crate::native_config_service::ConfigSnapshot {
    let NativeConfigRequest { origin, work } = app.native_config_pending.pop_front().unwrap();
    assert!(matches!(&origin, NativeConfigOrigin::ThemeChoice { .. }));
    let (outcome, snapshot) = match app.reduce_native_config_work(work).unwrap() {
        ConfigPatchResult::Applied { snapshot, undo } => (
            ConfigPatchOutcome::Applied {
                revision: snapshot.revision,
                undo: Some(undo.get()),
            },
            snapshot,
        ),
        other => panic!("a theme write must apply: {other:?}"),
    };
    app.publish_native_config_origin(origin, outcome, Some(snapshot.clone()), false, None);
    snapshot
}

/// Choosing the named dark Default is an explicit selection, including on a
/// light OS. Its preview and its saved completion must agree. Removing the
/// setting is a separate action that draws the same Default: an unset theme
/// is Default on every appearance, never Default Light.
#[test]
fn palette_explicit_default_stays_dark_after_save_on_a_light_os() {
    use crate::control_theme::PreviewHold;

    let mut app = App::headless_for_test();
    app.native_config_service = crate::native_config_service::VersionedConfigService::new(
        "theme = \"Dracula\"\n".to_string(),
    )
    .unwrap();
    app.config = (*app.native_config_service.snapshot().config).clone();
    app.on_os_appearance(aterm_types::Appearance::Light);
    assert_eq!(app.os_appearance, aterm_types::Appearance::Light);
    let chosen = aterm_types::scheme::builtin("Default").expect("explicit dark theme");
    let light = aterm_types::scheme::builtin("Default Light").expect("Default's light pair");
    assert_ne!(chosen.background, light.background, "the choices differ");

    app.begin_theme_preview("Default", PreviewHold::Committing)
        .expect("the named theme previews");
    let preview = app.theme;
    assert_eq!(preview, aterm_render::Theme::from(&chosen));
    app.enqueue_theme_choice("Default");
    assert_eq!(app.native_config_pending.len(), 1, "one choice write");
    let saved = complete_theme_choice(&mut app);
    assert_eq!(
        saved.config.theme.as_deref(),
        Some("Default"),
        "the explicit choice is not an unset"
    );
    assert!(saved.text.contains("theme = \"Default\""), "{}", saved.text);
    assert_eq!(app.config.theme.as_deref(), Some("Default"));
    assert!(app.theme_preview.is_none());
    assert_eq!(app.installed_colors.id, "Default");
    assert_eq!(app.theme, preview, "the saved choice keeps its preview");
    assert_eq!(
        app.session_factory
            .terminal_config
            .as_ref()
            .and_then(|tc| tc.color_scheme),
        Some(aterm_types::Appearance::Dark)
    );
    {
        let term = crate::term_lock(&app.pool.get(0).unwrap().term);
        assert_eq!(term.default_background(), chosen.background);
        assert_eq!(term.default_foreground(), chosen.foreground);
        for (index, color) in chosen.ansi.iter().enumerate() {
            assert_eq!(term.palette_color(index as u8), *color);
        }
    }

    // The wire/Settings unset draws Default too, on this light OS. Run that
    // real lane, then admit its snapshot as the file watcher does.
    let (reply, unset) = complete_control_theme_field(&mut app, None);
    assert_eq!(reply, Ok("removed: theme; now Default".to_string()));
    assert_eq!(unset.config.theme, None);
    assert!(!unset.text.contains("theme ="), "{}", unset.text);
    app.config = (*unset.config).clone();
    app.install_resolved_colors(crate::app_colorscheme::ThemeChange::ConfigReload);
    assert_eq!(app.installed_colors.id, "Default");
    assert_eq!(app.theme, preview);
    assert_eq!(
        app.session_factory
            .terminal_config
            .as_ref()
            .and_then(|tc| tc.color_scheme),
        Some(aterm_types::Appearance::Dark)
    );
    assert_eq!(
        crate::term_lock(&app.pool.get(0).unwrap().term).default_background(),
        chosen.background
    );
}

/// A commit that the disk lane admitted is no longer a timed trial: an
/// injected clock cannot expire it while the save is pending. Its own applied
/// or refused completion ends it, drawing the proven saved or retained config.
#[test]
fn control_theme_commit_stands_past_its_ttl_until_its_own_completion() {
    let t0 = std::time::Instant::now();
    for rejected in [false, true] {
        let mut app = App::headless_for_test();
        app.native_config_service = crate::native_config_service::VersionedConfigService::new(
            "theme = \"Dracula\"\n".to_string(),
        )
        .unwrap();
        app.config = (*app.native_config_service.snapshot().config).clone();
        app.install_resolved_colors(crate::app_colorscheme::ThemeChange::ConfigReload);
        let before = app.theme;
        app.preview_theme("Nord", std::time::Duration::from_secs(5), t0)
            .unwrap();
        let preview = app.theme;
        let (reply, completion) = std::sync::mpsc::channel();
        app.commit_theme_preview_with(reply, |app, id, request_id, reply| {
            app.enqueue_control_settings_field_intent_with_id(
                crate::prefs::EDIT_THEME.to_string(),
                Some(id),
                reply,
                request_id,
            );
            Some(request_id)
        });
        assert_eq!(app.native_config_pending.len(), 1, "one admitted save");
        assert!(matches!(
            completion.try_recv(),
            Err(std::sync::mpsc::TryRecvError::Empty)
        ));
        assert!(
            !app.expire_theme_preview(t0 + std::time::Duration::from_secs(6)),
            "a pending commit must not expire as a trial"
        );
        assert_eq!(app.next_theme_preview_wake(), None);
        assert_eq!(app.theme, preview);

        if rejected {
            let NativeConfigRequest { origin, .. } = app.native_config_pending.pop_front().unwrap();
            app.publish_native_config_origin(
                origin,
                ConfigPatchOutcome::Rejected {
                    message: "disk full".to_string(),
                },
                None,
                false,
                None,
            );
            assert!(completion.try_recv().expect("answered").is_err());
            assert_eq!(app.config.theme.as_deref(), Some("Dracula"));
            assert_eq!(
                app.theme, before,
                "a refused commit restores the retained config"
            );
        } else {
            let saved = complete_queued_control_theme_field(&mut app);
            completion.try_recv().expect("answered").expect("saved");
            assert_eq!(saved.config.theme.as_deref(), Some("Nord"));
            assert_eq!(app.config.theme.as_deref(), Some("Nord"));
            assert_eq!(
                app.theme, preview,
                "completion does not flash the old theme"
            );
        }
        assert!(
            app.theme_preview.is_none(),
            "the owning completion retires its preview"
        );
    }
}

/// A's real disk completion remains configured truth while a newer B preview
/// is standing. It cannot retire B or flash A. Only B's own completion can
/// finish B, adopting B on success or restoring the actually saved A on refusal.
#[test]
fn older_theme_save_completion_preserves_a_newer_committing_preview() {
    use crate::control_theme::PreviewHold;
    for rejected in [false, true] {
        let mut app = App::headless_for_test();
        app.native_config_service = crate::native_config_service::VersionedConfigService::new(
            "theme = \"Dracula\"\n".to_string(),
        )
        .unwrap();
        app.config = (*app.native_config_service.snapshot().config).clone();
        app.install_resolved_colors(crate::app_colorscheme::ThemeChange::ConfigReload);
        app.begin_theme_preview("Nord", PreviewHold::Committing)
            .unwrap();
        app.enqueue_theme_choice("Nord");
        app.begin_theme_preview("Gruvbox Light", PreviewHold::Committing)
            .unwrap();
        app.enqueue_theme_choice("Gruvbox Light");
        let newer = app.theme;
        let saved_a = complete_theme_choice(&mut app);
        assert_eq!(saved_a.config.theme.as_deref(), Some("Nord"));
        assert_eq!(
            app.config.theme.as_deref(),
            Some("Nord"),
            "A is actual saved truth"
        );
        assert_eq!(
            app.theme, newer,
            "A cannot flash through B's pending preview"
        );
        assert_eq!(
            app.theme_preview.as_ref().map(|p| p.id.as_str()),
            Some("Gruvbox Light")
        );

        if rejected {
            let NativeConfigRequest { origin, .. } = app.native_config_pending.pop_front().unwrap();
            app.publish_native_config_origin(
                origin,
                ConfigPatchOutcome::Rejected {
                    message: "disk full".to_string(),
                },
                None,
                false,
                None,
            );
            assert_eq!(app.config.theme.as_deref(), Some("Nord"));
            assert_eq!(app.installed_colors.id, "Nord");
            assert_eq!(
                app.theme,
                aterm_render::Theme::from(&aterm_types::scheme::builtin("Nord").unwrap())
            );
        } else {
            let saved_b = complete_theme_choice(&mut app);
            assert_eq!(saved_b.config.theme.as_deref(), Some("Gruvbox Light"));
            assert_eq!(app.config.theme.as_deref(), Some("Gruvbox Light"));
            assert_eq!(app.theme, newer);
        }
        assert!(
            app.theme_preview.is_none(),
            "only B's completion finishes B"
        );
    }
}

/// The queue pump can complete unchanged work inline. Binding after that
/// completion would resurrect a hold whose completion has already happened.
/// A preflight refusal has no pending save and also cannot leave a hold.
#[test]
fn control_theme_commit_inline_completion_or_queue_refusal_leaves_no_hold() {
    for admitted in [true, false] {
        let mut app = App::headless_for_test();
        let text = if admitted {
            "theme = \"Nord\"\n"
        } else {
            "theme = \"Dracula\"\n"
        };
        app.native_config_service =
            crate::native_config_service::VersionedConfigService::new(text.to_string()).unwrap();
        app.config = (*app.native_config_service.snapshot().config).clone();
        app.install_resolved_colors(crate::app_colorscheme::ThemeChange::ConfigReload);
        let configured = app.theme;
        app.preview_theme(
            "Nord",
            std::time::Duration::from_secs(5),
            std::time::Instant::now(),
        )
        .unwrap();
        let (reply, completion) = std::sync::mpsc::channel();
        app.commit_theme_preview_with(reply, |app, id, request_id, reply| {
            if !admitted {
                let _ = reply.send(Err("queue refused".to_string()));
                return None;
            }
            app.enqueue_control_settings_field_intent_with_id(
                crate::prefs::EDIT_THEME.to_string(),
                Some(id),
                reply,
                request_id,
            );
            complete_queued_control_theme_field(app);
            Some(request_id)
        });
        assert_eq!(completion.try_recv().expect("answered").is_ok(), admitted);
        assert!(app.native_config_pending.is_empty());
        assert!(app.theme_preview.is_none(), "no save remains to own a hold");
        assert_eq!(app.next_theme_preview_wake(), None);
        assert_eq!(app.theme, configured);
    }
}

/// An older control commit still changes configured truth after its preview
/// is cancelled or replaced by another trial. Its completion cannot retire
/// the newer trial, and reverting that trial reveals the actually saved theme.
#[test]
fn control_theme_saved_truth_survives_preview_cancellation_or_replacement() {
    let now = std::time::Instant::now();
    for cancelled in [true, false] {
        let mut app = App::headless_for_test();
        app.native_config_service = crate::native_config_service::VersionedConfigService::new(
            "theme = \"Dracula\"\n".to_string(),
        )
        .unwrap();
        app.config = (*app.native_config_service.snapshot().config).clone();
        app.install_resolved_colors(crate::app_colorscheme::ThemeChange::ConfigReload);
        app.preview_theme("Nord", std::time::Duration::from_secs(5), now)
            .unwrap();
        let (reply, completion) = std::sync::mpsc::channel();
        app.commit_theme_preview_with(reply, |app, id, request_id, reply| {
            app.enqueue_control_settings_field_intent_with_id(
                crate::prefs::EDIT_THEME.to_string(),
                Some(id),
                reply,
                request_id,
            );
            Some(request_id)
        });
        if cancelled {
            app.revert_theme_preview(now);
        } else {
            app.preview_theme("GitHub Light", std::time::Duration::from_secs(20), now)
                .unwrap();
        }
        complete_queued_control_theme_field(&mut app);
        completion.try_recv().expect("answered").expect("saved");
        assert_eq!(app.config.theme.as_deref(), Some("Nord"));
        if !cancelled {
            assert_eq!(app.installed_colors.id, "GitHub Light");
            assert!(app.theme_preview.is_some(), "the newer trial stays visible");
            app.revert_theme_preview(now);
        }
        assert!(app.theme_preview.is_none());
        assert_eq!(app.installed_colors.id, "Nord", "actual disk truth remains");
    }
}

/// An ordinary theme set supersedes a trial already standing when it was
/// queued. A trial or commit begun later remains newer intent. Drive the real prepared
/// disk observation and runtime reload, not only the origin's reply formatter.
#[test]
fn ordinary_theme_set_retires_older_preview_but_preserves_newer_trial() {
    let now = std::time::Instant::now();
    for newer in [0, 1, 2] {
        let scratch = aterm_tempfile::tempdir().expect("private config");
        let path = scratch.path().join("aterm.toml");
        std::fs::write(&path, "theme = \"Dracula\"\n").unwrap();
        let mut app = App::headless_for_test();
        app.native_config_service =
            crate::native_config_service::VersionedConfigService::load_path(&path).unwrap();
        app.config = (*app.native_config_service.snapshot().config).clone();
        app.install_resolved_colors(crate::app_colorscheme::ThemeChange::ConfigReload);
        app.preview_theme("Nord", std::time::Duration::from_secs(30), now)
            .unwrap();
        let (reply, completion) = std::sync::mpsc::channel();
        app.enqueue_control_settings_field_intent(
            crate::prefs::EDIT_THEME.to_string(),
            Some("GitHub Light".to_string()),
            reply,
        );
        let mut later_commit = None;
        if newer == 1 {
            app.preview_theme("Gruvbox Dark", std::time::Duration::from_secs(30), now)
                .unwrap();
        } else if newer == 2 {
            // The same standing Nord trial now has a newer save intent than
            // the ordinary set. Its birth alone must not decide its ordering.
            let (reply, completion) = std::sync::mpsc::channel();
            app.commit_theme_preview_with(reply, |app, id, request_id, reply| {
                app.enqueue_control_settings_field_intent_with_id(
                    crate::prefs::EDIT_THEME.to_string(),
                    Some(id),
                    reply,
                    request_id,
                );
                Some(request_id)
            });
            later_commit = Some(completion);
        }
        let NativeConfigRequest { origin, work } = app.native_config_pending.pop_front().unwrap();
        let (snapshot, undo) = match app.reduce_native_config_work(work).unwrap() {
            ConfigPatchResult::Applied { snapshot, undo } => (snapshot, undo),
            other => panic!("theme set applies: {other:?}"),
        };
        std::fs::write(&path, snapshot.text.as_ref()).unwrap();
        let observation =
            crate::native_config_service::VersionedConfigService::observe_path(&path, false)
                .unwrap();
        let prepared = crate::native_config_service::VersionedConfigService::prepare_observation(
            observation,
            std::sync::Arc::clone(&snapshot.assets.themes),
        )
        .unwrap();
        app.native_config_inflight = true;
        app.finish_native_config_write(
            origin,
            NativeConfigPersistenceCompletion {
                outcome: ConfigPatchOutcome::Applied {
                    revision: snapshot.revision,
                    undo: Some(undo.get()),
                },
                observation: Ok(prepared),
            },
        );
        completion.try_recv().expect("answered").expect("saved");
        assert_eq!(app.config.theme.as_deref(), Some("GitHub Light"));
        if let Some(later_commit) = later_commit {
            assert_eq!(
                app.installed_colors.id, "Nord",
                "the later commit stays visible"
            );
            assert!(app.theme_preview.is_some());
            complete_queued_control_theme_field(&mut app);
            later_commit.try_recv().expect("answered").expect("saved");
            assert!(app.theme_preview.is_none());
            assert_eq!(app.config.theme.as_deref(), Some("Nord"));
            assert_eq!(app.installed_colors.id, "Nord");
            continue;
        }
        if newer == 1 {
            assert_eq!(app.installed_colors.id, "Gruvbox Dark");
            let preview = app.theme_preview.as_ref().expect("newer trial remains");
            assert_eq!(
                preview.restore, "GitHub Light",
                "restore names actual saved truth"
            );
            app.revert_theme_preview(now);
        }
        assert!(
            app.theme_preview.is_none(),
            "a superseded trial is not left standing"
        );
        assert_eq!(app.installed_colors.id, "GitHub Light");
        assert_eq!(
            app.theme,
            aterm_render::Theme::from(&aterm_types::scheme::builtin("GitHub Light").unwrap())
        );
    }
}

/// A theme chosen in the palette is ONE write of `theme`, and the screen goes
/// straight from the preview to the configured (now chosen) theme: the
/// Committing preview stands while the write is in flight, and its completion
/// adopts the durable `theme` and ends the preview without a flash of the old
/// theme. Choosing Default keeps that explicit named theme.
#[test]
fn palette_theme_choice_is_one_write_and_lands_without_a_flash() {
    use crate::control_theme::PreviewHold;
    let mut app = App::headless_for_test();
    app.native_config_service = crate::native_config_service::VersionedConfigService::new(
        "theme = \"Dracula\"\n".to_string(),
    )
    .unwrap();
    app.config = (*app.native_config_service.snapshot().config).clone();
    app.install_resolved_colors(crate::app_colorscheme::ThemeChange::ConfigReload);
    let revision = app.native_config_service.snapshot().revision;

    app.begin_theme_preview("Gruvbox Dark", PreviewHold::Committing)
        .expect("a built-in previews");
    app.enqueue_theme_choice("Gruvbox Dark");
    assert_eq!(app.native_config_pending.len(), 1, "one request");
    let gruvbox = aterm_render::Theme::from(&aterm_types::scheme::builtin("Gruvbox Dark").unwrap());
    assert_eq!(
        app.theme, gruvbox,
        "the choice stands while the write is in flight"
    );
    assert!(
        !app.expire_theme_preview(
            std::time::Instant::now() + std::time::Duration::from_secs(86_400)
        ),
        "no clock ends a committing preview"
    );

    let snapshot = complete_theme_choice(&mut app);
    assert!(app.native_config_pending.is_empty());
    assert_eq!(snapshot.revision, revision + 1, "exactly one write");
    assert!(
        snapshot.text.contains("theme = \"Gruvbox Dark\""),
        "{}",
        snapshot.text
    );
    assert_eq!(
        app.config.theme.as_deref(),
        Some("Gruvbox Dark"),
        "adopted at once"
    );
    assert!(
        app.theme_preview.is_none(),
        "the preview gave way to the config"
    );
    assert_eq!(app.installed_colors.id, "Gruvbox Dark");
    assert_eq!(app.theme, gruvbox);

    // Default is a named theme, and the palette writes its name.
    app.enqueue_theme_choice("Default");
    let snapshot = complete_theme_choice(&mut app);
    assert!(
        snapshot.text.contains("theme = \"Default\""),
        "{}",
        snapshot.text
    );
    assert_eq!(app.config.theme.as_deref(), Some("Default"));
    assert_eq!(app.installed_colors.id, "Default");
}

/// A headless App on `os`, its config lane and live config holding `toml`.
fn theme_choice_app(toml: &str, os: aterm_types::Appearance) -> App {
    let mut app = App::headless_for_test();
    app.native_config_service =
        crate::native_config_service::VersionedConfigService::new(toml.to_string()).unwrap();
    app.config = (*app.native_config_service.snapshot().config).clone();
    app.os_appearance = os;
    app.install_resolved_colors(crate::app_colorscheme::ThemeChange::ConfigReload);
    app
}

/// A palette choice while `theme` is a light/dark pair replaces only the side
/// the OS shows, as the Settings pickers do, so the pair keeps following the
/// OS; Enter on the side already drawn writes nothing. Negative control: a
/// plain name is replaced whole.
#[test]
fn palette_choice_keeps_a_follow_system_pair() {
    use aterm_types::Appearance::{Dark, Light};
    let pair = "theme = \"dark:Aterm Night,light:Aterm Day\"\n";
    // A theme made for the side on screen takes that side; one made for the
    // other appearance is kept for both (Follow system off), so what is on
    // screen stays and no slot holds a theme of the wrong appearance.
    for (os, chosen, written) in [
        (Dark, "Nord", "dark:Nord,light:Aterm Day"),
        (Light, "GitHub Light", "dark:Aterm Night,light:GitHub Light"),
        (Light, "Nord", "Nord"),
        (Dark, "Linen", "Linen"),
    ] {
        let mut app = theme_choice_app(pair, os);
        app.enqueue_theme_choice(chosen);
        let snapshot = complete_theme_choice(&mut app);
        assert!(
            snapshot.text.contains(&format!("theme = \"{written}\"")),
            "{os:?} {chosen}: {}",
            snapshot.text
        );
        assert_eq!(app.installed_colors.id, chosen, "{os:?}");
    }

    let mut app = theme_choice_app(pair, Dark);
    let revision = app.native_config_service.snapshot().revision;
    app.palette_choose_theme("Aterm Night");
    assert!(app.native_config_pending.is_empty(), "nothing to write");
    assert_eq!(app.native_config_service.snapshot().revision, revision);
    assert_eq!(
        app.config.theme.as_deref(),
        Some("dark:Aterm Night,light:Aterm Day")
    );

    let mut app = theme_choice_app("theme = \"Aterm Night\"\n", Light);
    app.enqueue_theme_choice("Nord");
    let snapshot = complete_theme_choice(&mut app);
    assert!(
        snapshot.text.contains("theme = \"Nord\""),
        "{}",
        snapshot.text
    );
}

/// A palette choice while `theme` is unset (Default, one theme) writes the
/// chosen name, as Settings ▸ Themes' Keep and the Settings pickers do, on
/// either OS appearance: choosing never starts following the OS. (A pair
/// keeping its other side is `palette_choice_keeps_a_follow_system_pair`.)
#[test]
fn palette_choice_from_an_unset_theme_is_its_plain_name() {
    use aterm_types::Appearance::{Dark, Light};
    for (os, chosen) in [(Dark, "Aterm Night"), (Light, "Aterm Day")] {
        let mut app = theme_choice_app("", os);
        app.enqueue_theme_choice(chosen);
        let snapshot = complete_theme_choice(&mut app);
        assert!(
            snapshot.text.contains(&format!("theme = \"{chosen}\"")),
            "{os:?}: {}",
            snapshot.text
        );
        assert_eq!(app.installed_colors.id, chosen, "{os:?}");
    }
}

/// A theme file whose `name =` is not its id reads the same in the palette's
/// Theme section as in Settings: its name, the one fact separator, its id.
/// Negative control: a file named as its id is its id alone.
#[test]
fn a_theme_file_reads_in_the_palette_as_it_does_in_settings() {
    let mut app = App::headless_for_test();
    app.reload_theme_catalog(crate::app_config::ThemeCatalog::from_schemes([
        (
            "studio".to_string(),
            aterm_types::ColorScheme {
                name: "Studio Night".to_string(),
                ..aterm_types::ColorScheme::default()
            },
        ),
        (
            "paper".to_string(),
            aterm_types::ColorScheme {
                name: "paper".to_string(),
                ..aterm_types::ColorScheme::default()
            },
        ),
    ]));
    let label = |id: &str| {
        app.palette_themes()
            .into_iter()
            .find(|theme| theme.id == id)
            .unwrap_or_else(|| panic!("{id} is listed"))
            .label
    };
    assert_eq!(
        label("studio"),
        format!("Studio Night{}studio", crate::native_ui::FACT_SEPARATOR)
    );
    assert_eq!(label("paper"), "paper");
}

/// A refused theme choice reverts its preview to the configured theme and
/// says so; the config is untouched. Negative control: a refusal leaves a
/// palette-held or TTL preview of someone else alone.
#[test]
fn palette_theme_choice_refused_reverts_and_says_why() {
    use crate::control_theme::PreviewHold;
    let mut app = App::headless_for_test();
    app.config = aterm_toml::from_str("theme = \"Dracula\"\n").unwrap();
    app.install_resolved_colors(crate::app_colorscheme::ThemeChange::ConfigReload);
    let before = app.theme;

    app.begin_theme_preview("Nord", PreviewHold::Committing)
        .unwrap();
    app.enqueue_theme_choice("Nord");
    let NativeConfigRequest { origin, .. } = app.native_config_pending.pop_front().unwrap();
    app.publish_native_config_origin(
        origin,
        ConfigPatchOutcome::Rejected {
            message: "disk full".to_string(),
        },
        None,
        false,
        None,
    );
    assert!(app.theme_preview.is_none());
    assert_eq!(
        app.theme, before,
        "the unsaved choice is not left on screen"
    );
    assert_eq!(app.config.theme.as_deref(), Some("Dracula"));
    assert!(app.has_live_message("Couldn't save the theme"));

    let now = std::time::Instant::now();
    app.preview_theme("Nord", std::time::Duration::from_secs(60), now)
        .unwrap();
    app.enqueue_theme_choice("Gruvbox Dark");
    let NativeConfigRequest { origin, .. } = app.native_config_pending.pop_front().unwrap();
    app.publish_native_config_origin(
        origin,
        ConfigPatchOutcome::Rejected {
            message: "disk full".to_string(),
        },
        None,
        false,
        None,
    );
    assert_eq!(
        app.theme_preview.as_ref().map(|p| p.hold),
        Some(PreviewHold::Until(now + std::time::Duration::from_secs(60))),
        "a refusal ends only the committing preview"
    );
}

/// THE THEME LIST IS ONE TAB STOP: with a theme tried from the filter, Tab
/// from the filter lands on the highlighted row (not the list's first), the
/// next Tab leaves the list in one step, and Shift-Tab comes back to the
/// highlighted row. Negative control: the first row is not the one tried.
#[test]
fn tab_enters_the_theme_list_at_the_highlighted_row_and_leaves_in_one_step() {
    use crate::native_ui::UiKey;
    let mut app = App::headless_for_test();
    let wid = WindowId(0);
    {
        let window = app.windows.get_mut(&wid).unwrap();
        window.cols = 160;
        window.rows = 50;
    }
    assert!(app.open_settings_tab(crate::native_settings::SettingsRoute::Themes));
    let (instance, view) = app.active_native_view(wid).expect("active Settings view");
    let _ = app.compiled_native_ui(wid).unwrap();
    let filter = UiKey::new("settings/showcase/filter");
    app.native_runtime
        .dispatch(instance, view, AppEvent::FocusChanged(Some(filter.clone())))
        .unwrap();
    for _ in 0..3 {
        app.native_runtime
            .dispatch(instance, view, AppEvent::StepFocus(1))
            .unwrap();
    }
    app.invalidate_native_view_cache(wid, view, DamageRegion::All);
    let focus = |app: &App| {
        app.native_runtime
            .view_state(view)
            .and_then(|state| state.common().last_focus.clone())
            .expect("a focused control")
    };
    assert_eq!(
        focus(&app),
        filter,
        "the arrows leave the keyboard in the filter"
    );
    let Some(crate::native_app::AppViewState::Settings(state)) =
        app.native_runtime.view_state(view)
    else {
        panic!("a Settings view")
    };
    let highlighted = UiKey::new(format!(
        "settings/showcase/card/{}",
        state.highlighted_slug().expect("a highlighted row")
    ));
    let compiled = app.compiled_native_ui(wid).unwrap();
    let first_row = compiled
        .focus_order
        .iter()
        .find(|key| {
            key.as_str()
                .strip_prefix("settings/showcase/card/")
                .is_some_and(|slug| !slug.contains('/'))
        })
        .expect("a row in Tab order")
        .clone();
    assert_ne!(first_row, highlighted, "the control: a row below the first");

    app.move_native_focus(wid, false).unwrap();
    assert_eq!(
        focus(&app),
        highlighted,
        "Tab enters at the highlighted row"
    );
    app.move_native_focus(wid, false).unwrap();
    let out = focus(&app);
    assert!(
        !out.as_str().starts_with("settings/showcase/card/"),
        "the next Tab leaves the list: {}",
        out.as_str()
    );
    app.move_native_focus(wid, true).unwrap();
    assert_eq!(focus(&app), highlighted, "Shift-Tab comes back to it");
}

/// A SEGMENTED CONTROL IS ONE CONTROL TO THE KEYBOARD, on every page that
/// draws one ([`crate::native_ui::UiNode::segmented`]): one Tab stop (the
/// chosen segment, or the one the keyboard is on); Left and Right move
/// between its segments and stop at its ends, choosing nothing; Return or
/// Space chooses the one the keyboard is on. From a segment not chosen, Down
/// steps to the next row and Tab leaves the control for the one after it
/// (never to the list's first control, never to another segment), and
/// Shift-Tab back into the control lands on its chosen segment. Settings ▸
/// Terminal's Bidirectional text and Messages' severity filter answer alike.
#[test]
fn a_segmented_control_is_one_tab_stop_and_one_arrow_model() {
    use crate::native_ui::UiKey;
    use aterm_types::keyboard::{Key, KeyEventType, Modifiers, NamedKey};
    let press = |key| crate::input::InputEvent::Key {
        key: Key::Named(key),
        mods: Modifiers::empty(),
        base_layout: None,
        layout_text: None,
        event_type: KeyEventType::Press,
    };
    let mut app = App::headless_for_test();
    let wid = WindowId(0);
    {
        let window = app.windows.get_mut(&wid).unwrap();
        window.cols = 150;
        window.rows = 48;
    }
    assert!(app.open_settings_tab(crate::native_settings::SettingsRoute::Terminal));
    let (_, view) = app.active_native_view(wid).expect("active Settings view");
    let focus = |app: &App| {
        app.native_runtime
            .view_state(view)
            .and_then(|state| state.common().last_focus.clone())
    };
    let key = |text: &str| UiKey::new(text);
    let bidi = crate::prefs::EDIT_BIDI;
    let saved = key(&format!("settings/control/{bidi}"));
    let off = key(&format!("settings/segment/{bidi}/1"));
    let asked = key(&format!("settings/segment/{bidi}/2"));
    let segments = [saved.clone(), off.clone(), asked.clone()];
    let stops = |app: &App| {
        let compiled = app.compiled_native_ui(wid).unwrap();
        segments
            .iter()
            .filter(|segment| compiled.focus_order.contains(segment))
            .cloned()
            .collect::<Vec<_>>()
    };
    assert_eq!(
        stops(&app),
        std::slice::from_ref(&saved),
        "at rest: one stop, the chosen"
    );

    // Left/Right move, choose nothing, stop at the ends.
    app.dispatch_native_event(wid, AppEvent::FocusChanged(Some(saved.clone())))
        .unwrap();
    assert!(app.native_input_event(wid, &press(NamedKey::ArrowRight)));
    assert_eq!(focus(&app).as_ref(), Some(&off));
    assert_eq!(
        stops(&app),
        std::slice::from_ref(&off),
        "the stop is the one the keyboard is on"
    );
    assert!(app.native_input_event(wid, &press(NamedKey::ArrowRight)));
    assert!(app.native_input_event(wid, &press(NamedKey::ArrowRight)));
    assert_eq!(focus(&app).as_ref(), Some(&asked), "it stops at its end");
    assert!(app.native_input_event(wid, &press(NamedKey::ArrowLeft)));
    assert_eq!(focus(&app).as_ref(), Some(&off));
    let label = |app: &App, segment: &UiKey| {
        app.compiled_native_ui(wid)
            .unwrap()
            .semantic(segment)
            .map(|node| node.label.clone())
    };
    assert!(
        app.native_config_pending.is_empty(),
        "an arrow chooses nothing"
    );

    // Down from a segment not chosen steps to the next row; Up back.
    let after = key(&format!(
        "settings/control/{}",
        crate::prefs::EDIT_AMBIGUOUS_WIDTH
    ));
    let before = key(&format!(
        "settings/control/{}",
        crate::prefs::EDIT_SEARCH_HISTORY_LINES
    ));
    assert!(app.native_input_event(wid, &press(NamedKey::ArrowDown)));
    assert_eq!(focus(&app).as_ref(), Some(&after), "Down: the next row");
    app.dispatch_native_event(wid, AppEvent::FocusChanged(Some(off.clone())))
        .unwrap();
    assert!(app.native_input_event(wid, &press(NamedKey::ArrowUp)));
    assert_eq!(focus(&app).as_ref(), Some(&before), "Up: the row before");

    // Tab from a segment not chosen leaves the control; Shift-Tab back in
    // lands on the chosen segment; Shift-Tab from a segment leaves it.
    app.dispatch_native_event(wid, AppEvent::FocusChanged(Some(off.clone())))
        .unwrap();
    app.move_native_focus(wid, false).unwrap();
    assert_eq!(focus(&app).as_ref(), Some(&after), "Tab: the control after");
    app.move_native_focus(wid, true).unwrap();
    assert_eq!(
        focus(&app).as_ref(),
        Some(&saved),
        "Shift-Tab: the chosen segment"
    );
    app.dispatch_native_event(wid, AppEvent::FocusChanged(Some(off.clone())))
        .unwrap();
    app.move_native_focus(wid, true).unwrap();
    assert_eq!(
        focus(&app).as_ref(),
        Some(&before),
        "Shift-Tab: the control before"
    );

    // Space chooses the one the keyboard is on: it is the chosen segment at
    // once, carrying the setting's own key, and the keyboard goes with it.
    app.dispatch_native_event(wid, AppEvent::FocusChanged(Some(off.clone())))
        .unwrap();
    let off_label = label(&app, &off).expect("Off is drawn");
    assert!(app.native_input_event(wid, &press(NamedKey::Space)));
    assert_eq!(app.native_config_pending.len(), 1, "Space saves Off");
    assert_eq!(
        label(&app, &saved),
        Some(off_label),
        "Off is the chosen one"
    );
    assert_eq!(
        focus(&app).as_ref(),
        Some(&saved),
        "the keyboard follows it"
    );

    // Messages' severity filter: the same control.
    {
        use aterm_messages::{Message, Severity, tags};
        app.post_message(Message::new(tags::SYSTEM, Severity::Warn, "a problem"));
        app.post_message(Message::new(tags::SYSTEM, Severity::Info, "a note"));
    }
    assert!(app.open_settings_tab(crate::native_settings::SettingsRoute::Messages));
    let (_, view) = app.active_native_view(wid).expect("active Settings view");
    let all = key("settings/messages/filter/all");
    let warn = key("settings/messages/filter/warn");
    let compiled = app.compiled_native_ui(wid).unwrap();
    assert_eq!(
        [&all, &warn]
            .into_iter()
            .filter(|segment| compiled.focus_order.contains(segment))
            .count(),
        1,
        "the severity filter is one Tab stop"
    );
    app.dispatch_native_event(wid, AppEvent::FocusChanged(Some(all.clone())))
        .unwrap();
    assert!(app.native_input_event(wid, &press(NamedKey::ArrowRight)));
    let state = |app: &App| match app.native_runtime.view_state(view) {
        Some(crate::native_app::AppViewState::Settings(state)) => (
            state.common.last_focus.clone(),
            state.messages_filter.warn_only,
        ),
        _ => panic!("Settings view"),
    };
    assert_eq!(
        state(&app),
        (Some(warn.clone()), false),
        "Right moves, chooses nothing"
    );
    assert!(app.native_input_event(wid, &press(NamedKey::Space)));
    assert_eq!(
        state(&app),
        (Some(warn.clone()), true),
        "Space chooses Problems"
    );
}

/// An Editor on `text` in a fresh file under `dir`, in window 0, laid out
/// large enough for the M-x rows: `(app, view, document, path)`.
fn editor_app(
    dir: &std::path::Path,
    name: &str,
    text: &str,
) -> (
    App,
    crate::tab_model::ViewId,
    crate::document_store::DocumentId,
    std::path::PathBuf,
) {
    let mut app = App::headless_for_test();
    let wid = WindowId(0);
    if let Some(window) = app.windows.get_mut(&wid) {
        window.cols = 92;
        window.rows = 34;
    }
    let path = dir.join(name);
    std::fs::write(&path, text).unwrap();
    let uri = crate::native_document_host::path_to_file_uri(&path).unwrap();
    app.open_document_tab(crate::native_app::AppKind::Editor, &uri)
        .unwrap();
    let (instance, view) = app.active_native_view(wid).unwrap();
    let document = app.native_runtime.document_id(instance).unwrap();
    (app, view, document, path)
}

fn editor_status(app: &App, view: crate::tab_model::ViewId) -> Option<String> {
    match app.native_runtime.view_state(view) {
        Some(crate::native_app::AppViewState::Editor(state)) => state.status.clone(),
        _ => panic!("an Editor view"),
    }
}

fn editor_key(
    key: aterm_types::keyboard::Key,
    mods: aterm_types::keyboard::Modifiers,
) -> crate::input::InputEvent {
    crate::input::InputEvent::Key {
        key,
        mods,
        base_layout: None,
        layout_text: None,
        event_type: aterm_types::keyboard::KeyEventType::Press,
    }
}

/// `act` is a control client's gesture, so an editor command it names runs
/// only within a control client's grant (`command_registry::editor_grant`:
/// Read, Edit, Save). `kill-ring-save` copies text to the system
/// clipboard, so `act` on its M-x row is refused: nothing is copied, and the
/// reason is the caller's answer and the status line. The person's own press
/// of the same row copies, to the kill ring and the clipboard; a control
/// client's M-w key meets the same check in the reducer, and `act` still
/// saves.
#[test]
fn act_dispatch_consults_authority() {
    use crate::native_editor::{EditorCommand, EditorCompletionAction};
    use aterm_types::keyboard::{Key, Modifiers};
    let dir = std::env::temp_dir().join(format!(
        "aterm-editor-act-authority-{}-{:?}",
        std::process::id(),
        std::thread::current().id(),
    ));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    let (mut app, view, document, path) = editor_app(&dir, "notes.txt", "alpha beta\n");
    let wid = WindowId(0);
    let refusal = "kill-ring-save needs clipboard authority, which this input does not carry";
    app.dispatch_native_event(wid, AppEvent::EditorSetSelection { anchor: 0, head: 5 })
        .unwrap();
    app.dispatch_native_event(wid, AppEvent::EditorCommand(EditorCommand::ExecuteCommand))
        .unwrap();
    app.dispatch_native_event(wid, AppEvent::InsertText("kill-ring-save".to_string()))
        .unwrap();
    assert_eq!(
        crate::native_editor::command_completions("kill-ring-save").first(),
        Some(&EditorCommand::KillRingSave)
    );
    let act = |app: &mut App, key: &str, action: &str| {
        app.act_app(crate::app_control::ActRequest {
            view,
            ui_key: key.to_string(),
            action: action.to_string(),
            value: None,
        })
    };
    assert_eq!(
        act(&mut app, "editor/completion/0", "editor/completion/0"),
        Err(refusal.to_string())
    );
    let board = || crate::control::PBPASTE_STUB.with(|board| board.borrow().clone());
    assert!(app.editor_workspace.kill_ring.is_empty());
    assert_eq!(board(), None);
    assert_eq!(editor_status(&app, view).as_deref(), Some(refusal));
    assert!(!app.person_gesture_refused(), "the mark ends with the act");

    // The person's press of the same row copies the region.
    app.dispatch_native_event(
        wid,
        AppEvent::EditorCompletion(EditorCompletionAction::Choose(0)),
    )
    .unwrap();
    assert_eq!(
        app.editor_workspace.kill_ring.front().map(String::as_str),
        Some("alpha")
    );
    assert_eq!(board().as_deref(), Some("alpha"));

    // A control client's M-w key reaches the reducer's own check.
    app.dispatch_native_event(
        wid,
        AppEvent::EditorSetSelection {
            anchor: 6,
            head: 10,
        },
    )
    .unwrap();
    assert!(app.with_gesture_source(true, |app| {
        app.native_input_event(wid, &editor_key(Key::Character('w'), Modifiers::ALT))
    }));
    assert_eq!(app.editor_workspace.kill_ring.len(), 1);
    assert_eq!(editor_status(&app, view).as_deref(), Some(refusal));

    // Save is in the grant.
    app.dispatch_native_event(wid, AppEvent::InsertText("!".to_string()))
        .unwrap();
    act(&mut app, "editor/save-button", "editor/save").unwrap();
    assert_eq!(std::fs::read_to_string(&path).unwrap(), "alpha !\n");
    assert_eq!(
        app.document_store.snapshot(document).unwrap().text.as_ref(),
        "alpha !\n"
    );
    let _ = std::fs::remove_dir_all(&dir);
}

/// A person's kill reaches the system clipboard, the kill ring's bridge
/// (design §2.1); a control client's stays in the ring, because its input
/// carries no Clipboard authority (`command_registry::editor_grant`), even
/// when it joins the person's kill.
#[test]
fn a_persons_kill_reaches_the_clipboard_and_a_controllers_does_not() {
    use aterm_types::keyboard::{Key, Modifiers};
    let dir = std::env::temp_dir().join(format!(
        "aterm-editor-kill-clipboard-{}-{:?}",
        std::process::id(),
        std::thread::current().id(),
    ));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    let (mut app, _, _, _) = editor_app(&dir, "kills.txt", "one\ntwo\n");
    let wid = WindowId(0);
    let board = || crate::control::PBPASTE_STUB.with(|board| board.borrow().clone());
    let kill_line = editor_key(Key::Character('k'), Modifiers::CTRL);
    assert_eq!(board(), None);
    assert!(app.native_input_event(wid, &kill_line));
    assert_eq!(board().as_deref(), Some("one"));
    assert!(app.with_gesture_source(true, |app| app.native_input_event(wid, &kill_line)));
    assert_eq!(
        app.editor_workspace.kill_ring.front().map(String::as_str),
        Some("one\n")
    );
    assert_eq!(board().as_deref(), Some("one"));
    let _ = std::fs::remove_dir_all(&dir);
}

/// The closing keys close the Editor view the way ⌘W does: `C-x k` with
/// another buffer's file URI closes that buffer's tab and leaves the person
/// where they were, and `C-x C-c` closes this one. `C-c C-c` with no client
/// waiting on the file closes nothing and says so.
#[test]
fn closing_keys_close_the_editor_view() {
    use aterm_types::keyboard::{Key, Modifiers, NamedKey};
    let dir = std::env::temp_dir().join(format!(
        "aterm-editor-close-keys-{}-{:?}",
        std::process::id(),
        std::thread::current().id(),
    ));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    let (mut app, first_view, first_document, _) = editor_app(&dir, "first.txt", "one\n");
    let wid = WindowId(0);
    let second = dir.join("second.txt");
    std::fs::write(&second, "two\n").unwrap();
    let second_uri = crate::native_document_host::path_to_file_uri(&second).unwrap();
    app.open_document_tab(crate::native_app::AppKind::Editor, &second_uri)
        .unwrap();
    let (_, second_view) = app.active_native_view(wid).unwrap();
    assert_ne!(second_view, first_view);
    let tabs = |app: &App| app.windows.get(&wid).unwrap().tab_set.len();
    // The window's own first tab, and the two Editors.
    let opened = tabs(&app);
    let first_uri = app
        .document_store
        .canonical_uri(first_document)
        .unwrap()
        .to_string();
    let press = |app: &mut App, keys: &[crate::input::InputEvent]| {
        for key in keys {
            assert!(app.native_input_event(wid, key));
        }
    };
    let ctrl = |c| editor_key(Key::Character(c), Modifiers::CTRL);

    press(&mut app, &[ctrl('c'), ctrl('c')]);
    assert_eq!(
        editor_status(&app, second_view).as_deref(),
        Some("nothing is waiting on this file; C-x C-s saves")
    );
    assert_eq!(tabs(&app), opened);

    press(
        &mut app,
        &[
            ctrl('x'),
            editor_key(Key::Character('k'), Modifiers::empty()),
        ],
    );
    app.dispatch_native_event(wid, AppEvent::InsertText(first_uri))
        .unwrap();
    press(
        &mut app,
        &[editor_key(Key::Named(NamedKey::Enter), Modifiers::empty())],
    );
    assert_eq!(tabs(&app), opened - 1);
    assert_eq!(
        app.active_native_view(wid).map(|(_, view)| view),
        Some(second_view)
    );
    assert!(app.view_store.get(first_view).is_none());
    assert_eq!(
        editor_status(&app, second_view).as_deref(),
        Some("Killed buffer first.txt")
    );

    press(&mut app, &[ctrl('x'), ctrl('c')]);
    assert_eq!(tabs(&app), opened - 2);
    assert!(app.view_store.get(second_view).is_none());
    let _ = std::fs::remove_dir_all(&dir);
}

/// THE HARNESS SWITCH'S WRITES ARE ONES THE LANE APPLIES (owner,
/// 2026-10-20: "there needs to be an \"on\" button drawn for the claude
/// harness"): for each aterm.toml switch that can hold a Claude Code
/// session off — the master switch, the headless gate, Claude Code's own
/// switch, and all three — the writes the top rule's `on` button queues
/// (`autopilot::switch_on_plan`, each `true`), reduced in order on a config
/// holding it off, give a policy under which the session is supervised, and
/// none writes the session's word (the table's default carries it).
/// NEGATIVE CONTROL: the config before the writes holds the session off.
#[test]
fn the_harness_switch_writes_turn_every_switch_on() {
    use crate::autopilot::{HarnessPosture, switch_on_plan};
    use aterm_phase::Program;
    let policy = |text: &str| crate::app_config::HarnessPolicy::read(text).policy;
    let carried = |text: &str, headless: bool| {
        matches!(
            HarnessPosture::of(true, &policy(text), headless).for_agent(Some(Program::Claude)),
            HarnessPosture::On(_)
        )
    };
    for (text, headless, writes) in [
        ("[harness]\nenabled = false\n", false, 1),
        ("[harness]\nheadless = false\n", true, 1),
        ("[harness.claude]\nenabled = false\n", false, 1),
        (
            "[harness]\nenabled = false\nheadless = false\n\n[harness.claude]\nenabled = false\n",
            true,
            3,
        ),
    ] {
        assert!(!carried(text, headless), "control: {text:?} holds it off");
        let mut app = App::headless_for_test();
        app.native_config_service =
            crate::native_config_service::VersionedConfigService::new(text.to_string()).unwrap();
        let plan = switch_on_plan(&policy(text), headless, Some(Program::Claude), None);
        assert_eq!(plan.keys.len(), writes, "{text:?}: {plan:?}");
        assert!(!plan.cruise, "{text:?}: the default carries the session");
        for key in plan.keys {
            let result = app
                .reduce_native_config_work(NativeConfigWork::ControlField {
                    key: key.to_string(),
                    value: Some("true".to_string()),
                })
                .unwrap_or_else(|e| panic!("{text:?}: {key}: {e}"));
            assert!(
                matches!(&result, ConfigPatchResult::Applied { .. }),
                "{text:?}: {key} applies: {result:?}"
            );
            let ConfigPatchResult::Applied { snapshot, undo } = result else {
                unreachable!("asserted applied above")
            };
            app.publish_native_config_origin(
                NativeConfigOrigin::HarnessSwitch { key },
                ConfigPatchOutcome::Applied {
                    revision: snapshot.revision,
                    undo: Some(undo.get()),
                },
                Some(snapshot),
                false,
                None,
            );
        }
        let after = app.native_config_service.snapshot().text.to_string();
        assert!(carried(&after, headless), "{text:?} became {after:?}");
        assert!(
            app.messages.log().records().all(|r| {
                !r.key
                    .as_deref()
                    .is_some_and(|k| k.starts_with("harness.switch-save."))
            }),
            "ordinary writes confirm each effective switch, including intermediate multi-key completions"
        );
    }

    // A valid but misplaced limit is intentionally effective. A root write
    // preserves it, and completion must report inability rather than saying
    // nothing while the harness stays off. Cover each On-plan key family.
    for (text, headless, key) in [
        (
            "[project.harness]\nenabled = false\n",
            false,
            crate::prefs::EDIT_HARNESS_ENABLED,
        ),
        (
            "[project.harness]\nheadless = false\n",
            true,
            crate::autopilot::HARNESS_HEADLESS_KEY,
        ),
        (
            "[project.harness.claude]\nenabled = false\n",
            false,
            crate::prefs::EDIT_HARNESS_CLAUDE_ENABLED,
        ),
    ] {
        let mut app = App::headless_for_test();
        app.native_config_service =
            crate::native_config_service::VersionedConfigService::new(text.to_string()).unwrap();
        let plan = switch_on_plan(&policy(text), headless, Some(Program::Claude), None);
        assert_eq!(plan.keys, [key]);
        assert!(
            !carried(text, headless),
            "the admitted misplaced limit is effective"
        );
        let result = app
            .reduce_native_config_work(NativeConfigWork::ControlField {
                key: key.to_string(),
                value: Some("true".to_string()),
            })
            .unwrap();
        let (snapshot, undo) = match result {
            ConfigPatchResult::Applied { snapshot, undo } => (snapshot, Some(undo.get())),
            ConfigPatchResult::Unchanged { snapshot } => (snapshot, None),
            other => panic!("the root write is valid: {other:?}"),
        };
        assert!(
            snapshot.text.contains("[project.harness"),
            "unrelated table retained"
        );
        assert!(
            !carried(&snapshot.text, headless),
            "the limit must remain effective"
        );
        app.publish_native_config_origin(
            NativeConfigOrigin::HarnessSwitch { key },
            ConfigPatchOutcome::Applied {
                revision: snapshot.revision,
                undo,
            },
            Some(snapshot),
            false,
            None,
        );
        let message_key = format!("harness.switch-save.{key}");
        let record = app
            .messages
            .log()
            .records()
            .find(|r| r.key.as_deref() == Some(message_key.as_str()))
            .expect("an ineffective enable is reported");
        assert_eq!(record.severity, aterm_messages::Severity::Error);
    }

    // An Applied outcome without an admitted observation is not confirmation
    // that the switch reads on, even if some bytes were published.
    let mut app = App::headless_for_test();
    app.publish_native_config_origin(
        NativeConfigOrigin::HarnessSwitch {
            key: crate::prefs::EDIT_HARNESS_ENABLED,
        },
        ConfigPatchOutcome::Applied {
            revision: 1,
            undo: None,
        },
        None,
        false,
        None,
    );
    assert!(app.messages.log().records().any(|r| {
        r.key.as_deref() == Some("harness.switch-save.harness.enabled")
            && r.severity == aterm_messages::Severity::Warn
    }));
}

/// TAB ENTERS A LIST WHOSE SCREEN SHOWS NONE OF ITS CONTROLS: Software
/// Update at 220x12 with 2× text opens on its status hero, words only, so
/// the list's screen held no control Tab could land on, and Tab from the
/// search field went round the sidebar and back — the keyboard never got
/// into the list (the lab found 13 such pages at 220x12, 17 at 102x12).
/// Tab goes to the list's first control, Shift-Tab to its last, which the
/// list scrolls into view. All the opening screen's focusable controls
/// precede the list: Shift-Tab must reach it by wrapping past the end.
/// Fourteen rows, not twelve: the sidebar holds two whole rows there, so
/// Tab from the first starts on a raw step to another sidebar row (at
/// twelve the second is cut through its words, a picture and no stop).
#[test]
fn tab_enters_a_settings_list_whose_screen_shows_no_control() {
    let preferences = crate::native_appearance::current_preferences();
    // The window's chrome: the sidebar and the search field.
    let chrome = |key: &crate::native_ui::UiKey| {
        key.as_str().starts_with("settings/nav/") || key.as_str() == "settings/search"
    };
    for backwards in [false, true] {
        let mut app = App::headless_for_test();
        crate::native_appearance::install_preferences(
            crate::native_appearance::AppearancePreferences {
                text_scale: 2.0,
                ..preferences
            },
        );
        assert_eq!(crate::native_appearance::text_scale(), 2.0);
        let wid = WindowId(0);
        if let Some(ws) = app.windows.get_mut(&wid) {
            ws.cols = 220;
            ws.rows = 14;
        }
        assert!(app.open_settings_tab(crate::native_settings::SettingsRoute::SoftwareUpdate));
        let (_, view) = app.active_native_view(wid).expect("active Settings view");
        let compiled = app.compiled_native_ui(wid).unwrap();
        assert!(
            compiled.focus_order.iter().all(chrome),
            "PREMISE: the opening screen shows no control of the page: {:?}",
            compiled.focus_order
        );
        let list = compiled
            .semantics
            .iter()
            .position(|node| {
                node.role == crate::native_ui::SemanticRole::List
                    && node.key.as_str() != "settings/navigation/list"
            })
            .expect("the page has a list");
        assert!(compiled.focus_order.iter().all(|key| {
            compiled
                .semantics
                .iter()
                .position(|node| &node.key == key)
                .is_some_and(|at| at < list)
        }));
        // End focuses the sidebar's last row without changing the page.
        // Software Update is its section's first row, so the initial rail
        // shows its heading above it, not a second row. At the end of the
        // group the same room shows the preceding row instead.
        let rail = compiled
            .focus_order
            .iter()
            .find(|key| key.as_str().starts_with("settings/nav/"))
            .expect("the sidebar has a visible row")
            .clone();
        app.focus_native_control(wid, rail).unwrap();
        app.dispatch_native_view_event(
            wid,
            view,
            crate::native_app::AppEvent::TextInput(crate::native_app::TextInputEvent::End {
                extend: false,
            }),
        )
        .unwrap();
        let end = app.compiled_native_ui(wid).unwrap();
        assert!(matches!(
            app.native_runtime.view_state(view),
            Some(crate::native_app::AppViewState::Settings(state))
                if state.route == crate::native_settings::SettingsRoute::SoftwareUpdate
        ));
        assert!(
            end.focus_order.iter().all(chrome),
            "PREMISE: End left the page's screen without a control"
        );
        // Start from the first visible row, not its last: Tab leaves the
        // sidebar as one stop even when raw next is another sidebar row.
        let rail = end
            .focus_order
            .iter()
            .find(|key| key.as_str().starts_with("settings/nav/"))
            .expect("End shows a sidebar row")
            .clone();
        app.focus_native_control(wid, rail.clone()).unwrap();
        let seated = app.compiled_native_ui(wid).unwrap();
        let at = seated
            .focus_order
            .iter()
            .position(|key| *key == rail)
            .expect("the focused sidebar row is visible");
        assert!(
            seated
                .focus_order
                .get(at + 1)
                .is_some_and(|key| key.as_str().starts_with("settings/nav/")),
            "PREMISE: the raw next focus entry is another sidebar row"
        );
        let focus = |app: &App| {
            app.native_runtime
                .view_state(view)
                .and_then(|state| state.common().last_focus.clone())
        };
        let mut entered = None;
        for _ in 0..6 {
            app.move_native_focus(wid, backwards).unwrap();
            if let Some(key) = focus(&app).filter(|key| !chrome(key)) {
                entered = Some(key);
                break;
            }
        }
        let entered =
            entered.unwrap_or_else(|| panic!("backwards={backwards}: Tab enters the list"));
        assert!(
            app.compiled_native_ui(wid)
                .unwrap()
                .semantic(&entered)
                .is_some(),
            "backwards={backwards}: {entered:?} is on screen"
        );
    }
    crate::native_appearance::install_preferences(preferences);
}
