// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Tier-1 conformance for versioned native Settings transactions.
//!
//! The test drives the genuine `toml_edit`-backed service and independently
//! projects its revisions and semantic values into `NativeConfigTransaction`.
//! Pending request and undo metadata belong to the controller/test driver; the
//! canonical key values and revision always come from the shipping service.

#![cfg(test)]

use aterm_spec::derive::{
    Model, config_file_commit_cas_model, manual_config_completion_model,
    manual_config_handoff_model, manual_config_problem_navigation_model,
    native_config_transaction_model,
};
use aterm_spec::interp::{State, admits};

use crate::native_config_service::{
    ConfigKeyEdit, ConfigPatchRequest, ConfigPatchResult, ConfigSnapshot, ExpectedValue, UndoToken,
    VersionedConfigService,
};

const KEY_A: &str = "theme";
const KEY_B: &str = "cursor_blink";

#[derive(Clone, Copy)]
struct ControllerProjection {
    baseline_revision: u64,
    patch_active: bool,
    patch_base: u64,
    expected_a: i64,
    undo_ready: bool,
    undo_before_a: i64,
    undo_expected_a: i64,
    accepted: u64,
}

impl ControllerProjection {
    fn new(service: &VersionedConfigService) -> Self {
        Self {
            baseline_revision: service.snapshot().revision,
            patch_active: false,
            patch_base: 0,
            expected_a: 0,
            undo_ready: false,
            undo_before_a: 0,
            undo_expected_a: 0,
            accepted: 0,
        }
    }
}

fn relative(revision: u64, baseline: u64) -> i64 {
    i64::try_from(revision.saturating_sub(baseline)).expect("bounded test revision")
}

fn key_class(service: &VersionedConfigService, key: &str) -> i64 {
    match service
        .value(key)
        .expect("valid service snapshot")
        .as_deref()
    {
        None => 0,
        Some("Nord" | "true") => 1,
        Some("External" | "false") => 2,
        value => panic!("unexpected semantic value for {key}: {value:?}"),
    }
}

fn project(
    model: &Model,
    service: &VersionedConfigService,
    controller: ControllerProjection,
) -> State {
    let mut state = model.init_state();
    state.insert(
        "revision",
        relative(service.snapshot().revision, controller.baseline_revision),
    );
    state.insert("key_a", key_class(service, KEY_A));
    state.insert("key_b", key_class(service, KEY_B));
    state.insert("patch_active", i64::from(controller.patch_active));
    state.insert(
        "patch_base",
        relative(controller.patch_base, controller.baseline_revision),
    );
    state.insert("expected_a", controller.expected_a);
    state.insert("undo_ready", i64::from(controller.undo_ready));
    state.insert("undo_before_a", controller.undo_before_a);
    state.insert("undo_expected_a", controller.undo_expected_a);
    state.insert(
        "accepted",
        i64::try_from(controller.accepted).expect("bounded accepted count"),
    );
    state.insert("stale_overwrite", 0);
    state.insert("partial_reset", 0);
    state
}

fn assert_transition(model: &Model, before: &State, after: &State, action: &'static str) {
    assert_eq!(
        model.successors(action, before).as_slice(),
        std::slice::from_ref(after),
        "real transition must conform specifically to {action}"
    );
    assert_eq!(admits(model, before, after), Some(action));
    for invariant in &model.invariants {
        assert!(
            model.check_invariant(invariant.name, after),
            "post-state violates {}::{}: {after:?}",
            model.name,
            invariant.name,
        );
    }
}

#[test]
fn manual_completion_selection_and_responsive_window_conform_to_the_model() {
    use crate::native_app::{
        ConfigCompletionNavigation, config_completion_selection_transition,
        config_completion_window,
    };

    let model = manual_config_completion_model();
    let initial = model.init_state();
    let mut real = model.successors("EnterSelection", &initial)[0].clone();
    for _ in 0..3 {
        let before = real.clone();
        let selected = config_completion_selection_transition(
            usize::try_from(before["selected"]).unwrap(),
            8,
            ConfigCompletionNavigation::Next,
        );
        let window = config_completion_window(selected, 8, 3);
        real.insert("selected", i64::try_from(selected).unwrap());
        real.insert("window_start", i64::try_from(window.start).unwrap());
        assert_transition(&model, &before, &real, "MoveNext");
    }
    assert_eq!(real["selected"], 3);
    assert_eq!(real["window_start"], 3);

    // Negative control: the historical fixed-first-page projection can select
    // result four while leaving it outside the compiled responsive window.
    let mut before_page = model.successors("EnterSelection", &initial)[0].clone();
    before_page = model.successors("MoveNext", &before_page)[0].clone();
    before_page = model.successors("MoveNext", &before_page)[0].clone();
    let mut stale_window = real.clone();
    stale_window.insert("window_start", 0);
    assert_eq!(admits(&model, &before_page, &stale_window), None);
    assert!(!model.check_invariant("SelectedCandidateVisible", &stale_window));
}

#[test]
fn manual_problem_navigation_transition_conforms_for_one_wrap_and_negative_control() {
    use crate::native_app::config_diagnostic_selection_transition;

    let model = manual_config_problem_navigation_model();
    let initial = model.init_state();
    for (load, count, current, previous, action) in [
        ("LoadOne", 1, 0, false, "JumpNext"),
        ("LoadThree", 3, 0, false, "JumpNext"),
        ("LoadThree", 3, 0, true, "JumpPrevious"),
    ] {
        let before = model.successors(load, &initial)[0].clone();
        let selected = config_diagnostic_selection_transition(current, count, previous);
        let mut after = before.clone();
        after.insert("selected", i64::try_from(selected).unwrap());
        after.insert("target", i64::try_from(selected + 1).unwrap());
        after.insert("caret_target", i64::try_from(selected + 1).unwrap());
        after.insert("revealed", 1);
        after.insert("semantic_full", 1);
        after.insert("jumps", 1);
        assert_transition(&model, &before, &after, action);
    }

    let before = model.successors("LoadOne", &initial)[0].clone();
    let mut paint_only = before.clone();
    paint_only.insert("target", 1);
    paint_only.insert("jumps", 1);
    assert_eq!(admits(&model, &before, &paint_only), None);
    assert!(!model.check_invariant("JumpMovesToExactProblem", &paint_only));
    assert!(!model.check_invariant("JumpRevealsProblem", &paint_only));
    assert!(!model.check_invariant("FullProblemIsSemantic", &paint_only));
}

#[test]
fn settings_manual_handoff_conforms_for_selection_fallback_reuse_and_path_authority() {
    use crate::native_app::{AppViewState, ConfigEditorTarget};
    use crate::native_editor::Minibuffer;
    use crate::{App, WindowId};
    use std::sync::atomic::{AtomicU64, Ordering};

    static NEXT: AtomicU64 = AtomicU64::new(1);
    let root = std::env::temp_dir().join(format!(
        "aterm-manual-handoff-conformance-{}-{}",
        std::process::id(),
        NEXT.fetch_add(1, Ordering::Relaxed)
    ));
    std::fs::create_dir_all(&root).unwrap();
    let path = root.join("aterm.toml");
    let source = "theme = \"Nord\"\n";
    std::fs::write(&path, source).unwrap();

    let model = manual_config_handoff_model();
    let initial = model.init_state();
    let mut app = App::headless_for_test();
    let wid = WindowId(0);

    let uri = app
        .ensure_and_open_config_editor_path_at_in_window(
            wid,
            &path,
            Some(&ConfigEditorTarget::Key("theme".to_string())),
        )
        .unwrap();
    assert_eq!(
        uri,
        crate::native_document_host::path_to_file_uri(&path).unwrap(),
        "the host-selected config path remains authoritative"
    );
    let (instance, first_view) = app.active_native_view(wid).unwrap();
    let AppViewState::Editor(editor) = app.native_runtime.view_state(first_view).unwrap() else {
        panic!("Manual editor view");
    };
    let selection = editor.buffer.as_ref().unwrap().primary_selection().range();
    let expected = crate::native_config_language::config_key_source_range(source, "theme")
        .expect("authored theme value");
    assert_eq!(selection, expected);
    assert!(app.native_runtime.config_editor_enabled(instance));

    let mut selected = initial.clone();
    selected.insert("requests", 1);
    selected.insert("request_kind", 1);
    selected.insert("outcome", 1);
    selected.insert("selected_exact", 1);
    selected.insert("editor_instances", 1);
    selected.insert("focused", 1);
    assert_transition(&model, &initial, &selected, "RevealAuthoredKey");

    let second_uri = app
        .ensure_and_open_config_editor_path_at_in_window(
            wid,
            &path,
            Some(&ConfigEditorTarget::Key("gpu".to_string())),
        )
        .unwrap();
    let (instance, reused_view) = app.active_native_view(wid).unwrap();
    assert_eq!(second_uri, uri);
    assert_eq!(
        reused_view, first_view,
        "the canonical Manual editor is reused"
    );
    let AppViewState::Editor(editor) = app.native_runtime.view_state(reused_view).unwrap() else {
        panic!("Manual editor view");
    };
    assert!(matches!(
        editor.buffer.as_ref().unwrap().minibuffer,
        Minibuffer::Search { ref query, .. } if query == "gpu"
    ));
    assert!(app.native_runtime.config_editor_enabled(instance));

    let mut fallback = selected.clone();
    fallback.insert("requests", 2);
    fallback.insert("request_kind", 2);
    fallback.insert("outcome", 2);
    fallback.insert("selected_exact", 0);
    fallback.insert("search_exact", 1);
    fallback.insert("completion_ready", 1);
    assert_transition(&model, &selected, &fallback, "SeedAbsentKey");

    // The third action is bound independently because the model deliberately
    // caps one trace at two requests. Search targets use the same shipping host
    // resolver and select the exact matching bytes.
    let decision = crate::app_documents::config_editor_reveal_decision(
        source,
        &ConfigEditorTarget::Search("Nord".to_string()),
    )
    .unwrap();
    assert!(matches!(
        decision,
        crate::app_documents::ConfigEditorRevealDecision::Select {
            ref requested,
            ref range,
        } if requested == "Nord" && &source[range.clone()] == "Nord"
    ));
    let mut searched = initial.clone();
    searched.insert("requests", 1);
    searched.insert("request_kind", 3);
    searched.insert("outcome", 1);
    searched.insert("selected_exact", 1);
    searched.insert("editor_instances", 1);
    searched.insert("focused", 1);
    assert_transition(&model, &initial, &searched, "RevealMatchingSearch");

    // Negative control: a target-supplied path or a duplicate second editor is
    // not a transition admitted by the shipping model.
    let mut redirected = selected.clone();
    redirected.insert("canonical_path_authority", 0);
    assert_eq!(admits(&model, &initial, &redirected), None);
    assert!(!model.check_invariant("HostOwnsCanonicalPath", &redirected));
    let mut duplicate = fallback;
    duplicate.insert("editor_instances", 2);
    assert!(!model.check_invariant("OneManualEditor", &duplicate));

    let _ = std::fs::remove_dir_all(root);
}

fn begin_patch(
    model: &Model,
    service: &VersionedConfigService,
    controller: &mut ControllerProjection,
) {
    let before = project(model, service, *controller);
    controller.patch_active = true;
    controller.patch_base = service.snapshot().revision;
    controller.expected_a = key_class(service, KEY_A);
    let after = project(model, service, *controller);
    assert_transition(model, &before, &after, "BeginPatchA");
}

fn remove_a_request(controller: ControllerProjection) -> ConfigPatchRequest {
    let expected = match controller.expected_a {
        0 => None,
        1 => Some("Nord".to_string()),
        2 => Some("External".to_string()),
        value => panic!("unexpected projected A value: {value}"),
    };
    ConfigPatchRequest {
        base_revision: controller.patch_base,
        edits: vec![ConfigKeyEdit {
            key: KEY_A.to_string(),
            expected: ExpectedValue::Exact(expected),
            value: None,
        }],
    }
}

fn applied(result: ConfigPatchResult) -> UndoToken {
    match result {
        ConfigPatchResult::Applied { undo, .. } => undo,
        other => panic!("expected applied config transaction, got {other:?}"),
    }
}

#[test]
fn real_config_service_conforms_for_rebase_conflict_undo_and_atomic_reset() {
    let model = native_config_transaction_model();
    let mut service = VersionedConfigService::new(
        "theme = \"Nord\"\ncursor_blink = true\ncustom = \"preserve\"\n".into(),
    )
    .expect("valid baseline config");
    let mut controller = ControllerProjection::new(&service);
    assert_eq!(project(&model, &service, controller), model.init_state());

    // A stale Settings edit may rebase across an unrelated external key.
    begin_patch(&model, &service, &mut controller);
    let before_external_b = project(&model, &service, controller);
    service
        .replace_external("theme = \"Nord\"\ncursor_blink = false\ncustom = \"preserve\"\n".into())
        .expect("valid external config");
    let after_external_b = project(&model, &service, controller);
    assert_transition(&model, &before_external_b, &after_external_b, "ExternalB");

    let before_commit = project(&model, &service, controller);
    let undo = applied(service.patch(remove_a_request(controller)));
    controller.patch_active = false;
    controller.undo_ready = true;
    controller.undo_before_a = controller.expected_a;
    controller.undo_expected_a = 0;
    controller.accepted += 1;
    let after_commit = project(&model, &service, controller);
    assert_transition(&model, &before_commit, &after_commit, "CommitPatchA");
    assert_eq!(service.value(KEY_B).unwrap().as_deref(), Some("false"));
    assert!(service.snapshot().text.contains("custom = \"preserve\""));

    // Conditional undo restores only A and preserves the unrelated B change.
    let before_undo = project(&model, &service, controller);
    applied(service.undo(undo));
    controller.undo_ready = false;
    controller.accepted += 1;
    let after_undo = project(&model, &service, controller);
    assert_transition(&model, &before_undo, &after_undo, "UndoPatchA");
    assert_eq!(service.value(KEY_A).unwrap().as_deref(), Some("Nord"));
    assert_eq!(service.value(KEY_B).unwrap().as_deref(), Some("false"));

    // A same-key watcher edit makes the queued request stale. The genuine OCC
    // lane reports Conflict and changes neither bytes nor revision.
    begin_patch(&model, &service, &mut controller);
    let before_external_a = project(&model, &service, controller);
    service
        .replace_external(
            "theme = \"External\"\ncursor_blink = false\ncustom = \"preserve\"\n".into(),
        )
        .expect("valid external config");
    let after_external_a = project(&model, &service, controller);
    assert_transition(
        &model,
        &before_external_a,
        &after_external_a,
        "ExternalAFromOne",
    );

    let before_conflict = project(&model, &service, controller);
    let snapshot_before_conflict = service.snapshot();
    assert!(matches!(
        service.patch(remove_a_request(controller)),
        ConfigPatchResult::Conflict { keys, .. } if keys == [KEY_A.to_string()]
    ));
    assert_eq!(service.snapshot(), snapshot_before_conflict);
    controller.patch_active = false;
    let after_conflict = project(&model, &service, controller);
    assert_transition(
        &model,
        &before_conflict,
        &after_conflict,
        "RejectPatchConflict",
    );

    // Negative control: a blind stale writer cannot masquerade as that rejection.
    let mut blind_overwrite = before_conflict.clone();
    blind_overwrite.insert("revision", before_conflict["revision"] + 1);
    blind_overwrite.insert("key_a", 0);
    blind_overwrite.insert("patch_active", 0);
    blind_overwrite.insert("stale_overwrite", 1);
    assert_eq!(admits(&model, &before_conflict, &blind_overwrite), None);
    assert!(!model.check_invariant("NoBlindOverwrite", &blind_overwrite));

    // Commit from the current external value, then change the same key while
    // its undo token is pending. Conditional undo must now conflict.
    begin_patch(&model, &service, &mut controller);
    let before_second_commit = project(&model, &service, controller);
    let second_undo = applied(service.patch(remove_a_request(controller)));
    controller.patch_active = false;
    controller.undo_ready = true;
    controller.undo_before_a = controller.expected_a;
    controller.undo_expected_a = 0;
    controller.accepted += 1;
    let after_second_commit = project(&model, &service, controller);
    assert_transition(
        &model,
        &before_second_commit,
        &after_second_commit,
        "CommitPatchA",
    );

    let before_external_from_zero = project(&model, &service, controller);
    service
        .replace_external(
            "theme = \"External\"\ncursor_blink = false\ncustom = \"preserve\"\n".into(),
        )
        .expect("valid external config");
    let after_external_from_zero = project(&model, &service, controller);
    assert_transition(
        &model,
        &before_external_from_zero,
        &after_external_from_zero,
        "ExternalAFromZero",
    );

    let before_undo_conflict = project(&model, &service, controller);
    let snapshot_before_undo = service.snapshot();
    assert!(matches!(
        service.undo(second_undo),
        ConfigPatchResult::Conflict { keys, .. } if keys == [KEY_A.to_string()]
    ));
    assert_eq!(service.snapshot(), snapshot_before_undo);
    controller.undo_ready = false;
    let after_undo_conflict = project(&model, &service, controller);
    assert_transition(
        &model,
        &before_undo_conflict,
        &after_undo_conflict,
        "RejectUndoConflict",
    );

    // Reset All is one genuine service transform and one revision. Unknown
    // hand-authored keys survive; there is no observable half-reset state.
    let before_reset = project(&model, &service, controller);
    let reset_revision = service.snapshot().revision;
    applied(service.reset_all(reset_revision, [KEY_A.to_string(), KEY_B.to_string()]));
    controller.undo_ready = false;
    controller.accepted += 1;
    let after_reset = project(&model, &service, controller);
    assert_transition(&model, &before_reset, &after_reset, "ResetAll");
    assert_eq!(service.snapshot().revision, reset_revision + 1);
    assert_eq!(service.value(KEY_A).unwrap(), None);
    assert_eq!(service.value(KEY_B).unwrap(), None);
    assert!(service.snapshot().text.contains("custom = \"preserve\""));

    // Negative control: publishing only the first reset key is neither admitted
    // nor invariant-safe at Buggy=0.
    let mut partial_reset = before_reset.clone();
    partial_reset.insert("revision", before_reset["revision"] + 1);
    partial_reset.insert("key_a", 0);
    partial_reset.insert("partial_reset", 1);
    assert_eq!(admits(&model, &before_reset, &partial_reset), None);
    assert!(!model.check_invariant("AtomicResetVisibility", &partial_reset));
}

/// Tier-1 bind for the shared filesystem commit authority. Both interleavings
/// go through the shipping hosts: Manual uses the real Editor save reducer and
/// `finish_native_document_save`; Settings uses `save_prefs_snapshot` followed
/// by `finish_native_config_write`. This test therefore checks not just the
/// atomic primitive, but the production completion code that publishes its
/// proof into the process-global config service.
#[test]
fn real_config_commit_cas_conforms_for_both_winners_and_rejects_blind_publish() {
    use crate::app_native::NativeConfigOrigin;
    use crate::native_app::{AppEvent, ConfigPatchOutcome, TextInputEvent};
    use crate::{App, WindowId};

    fn prepare_manual(
        app: &mut App,
        path: &std::path::Path,
        text: &str,
    ) -> (crate::document_store::DocumentId, crate::tab_model::ViewId) {
        let wid = WindowId(0);
        app.ensure_and_open_config_editor_path_in_window(wid, path)
            .unwrap();
        let (instance, view) = app.active_native_view(wid).unwrap();
        let document = app.native_runtime.document_id(instance).unwrap();
        app.dispatch_native_event(wid, AppEvent::TextInput(TextInputEvent::SelectAll))
            .unwrap();
        app.dispatch_native_event(
            wid,
            AppEvent::TextInput(TextInputEvent::Commit(text.to_string())),
        )
        .unwrap();
        (document, view)
    }

    fn prepare_settings(
        app: &mut App,
        value: &str,
    ) -> (crate::native_config_service::ConfigPersistencePlan, u64) {
        let snapshot = app.native_config_service.snapshot();
        let expected = snapshot.values().unwrap().remove(KEY_A);
        match app.native_config_service.patch(ConfigPatchRequest {
            base_revision: snapshot.revision,
            edits: vec![ConfigKeyEdit {
                key: KEY_A.to_string(),
                expected: ExpectedValue::Exact(expected),
                value: Some(value.to_string()),
            }],
        }) {
            ConfigPatchResult::Applied { snapshot, undo } => (
                app.native_config_service.persistence_plan(snapshot),
                undo.get(),
            ),
            other => panic!("Settings candidate must apply: {other:?}"),
        }
    }

    fn finish_settings(
        app: &mut App,
        plan: &crate::native_config_service::ConfigPersistencePlan,
        undo: u64,
    ) -> bool {
        let completion = crate::app_native::execute_native_config_persistence(plan, Some(undo));
        let committed = matches!(&completion.outcome, ConfigPatchOutcome::Applied { .. });
        app.finish_native_config_write(
            NativeConfigOrigin::SeriousMode { desired: false },
            completion,
        );
        committed
    }

    let root = std::env::temp_dir().join(format!(
        "aterm-config-file-cas-conformance-{}",
        std::process::id()
    ));
    let _ = std::fs::remove_dir_all(&root);
    std::fs::create_dir_all(&root).unwrap();
    let model = config_file_commit_cas_model();

    // Interleaving 1: Manual acquires the real lock and publishes first.
    let path = root.join("manual-wins.toml");
    std::fs::write(&path, "theme = \"Default\"\n").unwrap();
    let mut app = App::headless_for_test();
    app.native_config_service = VersionedConfigService::load_path(&path).unwrap();
    let (manual_document, manual_view) = prepare_manual(&mut app, &path, "theme = \"Dracula\"\n");
    let (settings_plan, settings_undo) = prepare_settings(&mut app, "Nord");
    let mut state = model.init_state();
    for action in ["BeginManual", "BeginSettings", "LockManual"] {
        let next = model.successors(action, &state)[0].clone();
        assert_transition(&model, &state, &next, action);
        state = next;
    }
    app.save_document_checkpoint(manual_document, manual_view)
        .unwrap();
    let next = model.successors("ResolveManual", &state)[0].clone();
    assert_transition(&model, &state, &next, "ResolveManual");
    state = next;
    assert_eq!(state["disk"], 1);
    assert_eq!(state["service"], 1);
    assert_eq!(
        app.native_config_service.snapshot().text.as_ref(),
        "theme = \"Dracula\"\n"
    );

    let next = model.successors("LockSettings", &state)[0].clone();
    assert_transition(&model, &state, &next, "LockSettings");
    state = next;
    assert!(!finish_settings(&mut app, &settings_plan, settings_undo));
    let before_reject = state.clone();
    let next = model.successors("ResolveSettings", &state)[0].clone();
    assert_transition(&model, &state, &next, "ResolveSettings");
    assert_eq!(next["settings_phase"], 4);
    assert_eq!(
        std::fs::read_to_string(&path).unwrap(),
        "theme = \"Dracula\"\n"
    );

    // Negative control: a blind second publication from that exact stale real
    // pre-state is neither an admitted transition nor invariant-safe.
    let mut blind = before_reject.clone();
    blind.insert("disk", 2);
    blind.insert("service", 2);
    blind.insert("settings_phase", 3);
    blind.insert("lock_owner", 0);
    blind.insert("settings_committed", 1);
    blind.insert("double_winner", 1);
    blind.insert("stale_publication", 1);
    assert_eq!(admits(&model, &before_reject, &blind), None);
    assert!(!model.check_invariant("SameBaselineHasOneWinner", &blind));
    assert!(!model.check_invariant("NoStalePublication", &blind));

    // Interleaving 2: structured Settings acquires the same primitive first.
    let path = root.join("settings-wins.toml");
    std::fs::write(&path, "theme = \"Default\"\n").unwrap();
    let mut app = App::headless_for_test();
    app.native_config_service = VersionedConfigService::load_path(&path).unwrap();
    let (manual_document, manual_view) = prepare_manual(&mut app, &path, "theme = \"Dracula\"\n");
    let (settings_plan, settings_undo) = prepare_settings(&mut app, "Nord");
    let mut state = model.init_state();
    for action in ["BeginManual", "BeginSettings", "LockSettings"] {
        let next = model.successors(action, &state)[0].clone();
        assert_transition(&model, &state, &next, action);
        state = next;
    }
    assert!(finish_settings(&mut app, &settings_plan, settings_undo));
    let next = model.successors("ResolveSettings", &state)[0].clone();
    assert_transition(&model, &state, &next, "ResolveSettings");
    state = next;
    assert_eq!(state["disk"], 2);
    assert_eq!(state["service"], 2);
    assert_eq!(
        app.native_config_service.snapshot().text.as_ref(),
        "theme = \"Nord\"\n"
    );

    let next = model.successors("LockManual", &state)[0].clone();
    assert_transition(&model, &state, &next, "LockManual");
    state = next;
    assert!(
        app.save_document_checkpoint(manual_document, manual_view)
            .is_err(),
        "Manual must reject its stale physical generation"
    );
    let next = model.successors("ResolveManual", &state)[0].clone();
    assert_transition(&model, &state, &next, "ResolveManual");
    assert_eq!(next["manual_phase"], 4);
    assert_eq!(
        std::fs::read_to_string(&path).unwrap(),
        "theme = \"Nord\"\n"
    );

    // One final config symlink is an admitted, identity-bound capability. The
    // model first admits that stable baseline, then advances both its link and
    // target generations when the real link is replaced. Resolution must conflict
    // without redirecting bytes.
    #[cfg(unix)]
    {
        use std::os::unix::fs::symlink;

        let target = root.join("managed.toml");
        let replacement = root.join("replacement.toml");
        let logical = root.join("linked.toml");
        std::fs::write(&target, "theme = \"Default\"\n").unwrap();
        std::fs::write(&replacement, "theme = \"Dracula\"\n").unwrap();
        symlink(&target, &logical).unwrap();
        let mut service = VersionedConfigService::load_path(&logical).unwrap();
        let base = service.snapshot().revision;
        let ConfigPatchResult::Applied { snapshot, .. } = service.patch(ConfigPatchRequest {
            base_revision: base,
            edits: vec![ConfigKeyEdit {
                key: "theme".to_string(),
                expected: ExpectedValue::Exact(Some("Default".to_string())),
                value: Some("Nord".to_string()),
            }],
        }) else {
            panic!("safe final-link baseline admits semantic planning")
        };
        let plan = service.persistence_plan(snapshot);
        std::fs::remove_file(&logical).unwrap();
        symlink(&replacement, &logical).unwrap();
        let mut state = model.init_state();
        for action in ["BeginSettingsSymlink", "Retarget", "LockSettings"] {
            let next = model.successors(action, &state)[0].clone();
            assert_transition(&model, &state, &next, action);
            state = next;
        }
        assert!(matches!(
            crate::prefs::save_prefs_snapshot(&plan),
            crate::prefs::SaveOutcome::Conflict { .. }
        ));
        let next = model.successors("ResolveSettings", &state)[0].clone();
        assert_transition(&model, &state, &next, "ResolveSettings");
        assert_eq!(next["settings_phase"], 4);
        assert_eq!(
            std::fs::read_to_string(&target).unwrap(),
            "theme = \"Default\"\n"
        );
        assert_eq!(
            std::fs::read_to_string(&replacement).unwrap(),
            "theme = \"Dracula\"\n"
        );
        assert!(
            std::fs::symlink_metadata(&logical)
                .unwrap()
                .file_type()
                .is_symlink()
        );
    }
    let _ = std::fs::remove_dir_all(root);
}

fn trail_pack_asset(name: &str) -> String {
    std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../aterm-effects/assets/trail-packs")
        .join(name)
        .to_string_lossy()
        .into_owned()
}

fn trail_config(path: &str, id: &str, theme: &str, kitty_sprite: &str) -> String {
    let sparkle = if theme == "Dracula" {
        concat!(
            "[[sparkle_words.custom]]\n",
            "words = [\"cataloggeneration\"]\n",
            "burst = { kind = \"nova\", chance = 100 }\n",
        )
    } else {
        ""
    };
    format!(
        "theme = {theme:?}\ncursor_trail_style = {:?}\ncursor_trail_packs = [{:?}]\n\
         cursor_nyan_sprite = {kitty_sprite:?}\n{sparkle}",
        format!("pack:{id}"),
        path,
    )
}

struct KittyFixtures {
    root: std::path::PathBuf,
    sources: [String; 3],
}

impl KittyFixtures {
    fn new() -> Self {
        use std::sync::atomic::{AtomicU64, Ordering};
        static NEXT: AtomicU64 = AtomicU64::new(1);
        let root = std::env::temp_dir().join(format!(
            "aterm-config-assets-{}-{}",
            std::process::id(),
            NEXT.fetch_add(1, Ordering::Relaxed)
        ));
        std::fs::create_dir_all(&root).expect("create asset fixture root");
        let colors = [
            [0xf0, 0x33, 0x55, 0xff],
            [0x33, 0xf0, 0x88, 0xff],
            [0x44, 0x77, 0xf0, 0xff],
        ];
        let sources = std::array::from_fn(|index| {
            let path = root.join(format!("nyan-{index}.png"));
            let mut rgba = Vec::with_capacity(8);
            rgba.extend_from_slice(&colors[index]);
            rgba.extend_from_slice(&colors[index]);
            let png = crate::app_introspect::encode_rgba8_png(&rgba, 2, 1)
                .expect("encode fixture sprite");
            std::fs::write(&path, png).expect("write fixture sprite");
            path.to_string_lossy().into_owned()
        });
        Self { root, sources }
    }
}

impl Drop for KittyFixtures {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.root);
    }
}

fn snapshot_catalog_is_generation_consistent(snapshot: &ConfigSnapshot) -> bool {
    let Ok(config) = toml::from_str::<crate::app_config::Config>(&snapshot.text) else {
        return false;
    };
    let resolved = crate::app_config::resolve_trail_style(
        config.cursor_trail_style_raw(),
        &snapshot.assets.trail_packs,
    );
    let kitty_consistent = match &snapshot.assets.kitty_sprite {
        crate::app_config::KittySpriteAsset::Ready { source_id, .. } => config
            .cursor_nyan_sprite
            .as_deref()
            .is_some_and(|source| source.trim() == source_id.as_ref()),
        crate::app_config::KittySpriteAsset::BuiltIn => config
            .cursor_nyan_sprite
            .as_deref()
            .is_none_or(|source| source.trim().is_empty()),
        crate::app_config::KittySpriteAsset::Invalid { .. } => false,
    };
    let sparkle_consistent = snapshot
        .assets
        .sparkle_spec_consumers
        .as_deref()
        .is_some_and(|observed| {
            *observed
                == snapshot
                    .config
                    .prepare_sparkle_runtime()
                    .consumer_capabilities()
        });
    kitty_consistent
        && resolved.issue.is_none()
        && sparkle_consistent
        && resolved.pack.is_some_and(|pack| {
            snapshot
                .assets
                .trail_packs
                .packs
                .values()
                .any(|p| *p == pack)
        })
}

#[derive(Clone, Copy, Default)]
struct CatalogConsumers<'a> {
    view_one: Option<&'a std::sync::Arc<crate::app_config::ConfigAssetCatalog>>,
    view_two: Option<&'a std::sync::Arc<crate::app_config::ConfigAssetCatalog>>,
    live: Option<&'a std::sync::Arc<crate::app_config::ConfigAssetCatalog>>,
    capture: Option<&'a std::sync::Arc<crate::app_config::ConfigAssetCatalog>>,
}

fn config_catalog_projection(
    model: &Model,
    snapshot: &ConfigSnapshot,
    baseline_revision: u64,
    catalogs: &[&std::sync::Arc<crate::app_config::ConfigAssetCatalog>],
    consumers: CatalogConsumers<'_>,
) -> State {
    let config: crate::app_config::Config =
        toml::from_str(&snapshot.text).expect("service snapshot config");
    let text_generation = match config.theme.as_deref() {
        Some("Default") => 0,
        Some("Nord") => 1,
        Some("Dracula") => 2,
        theme => panic!("unexpected conformance generation marker: {theme:?}"),
    };
    let asset_generation = |catalog: &std::sync::Arc<crate::app_config::ConfigAssetCatalog>| {
        catalogs
            .iter()
            .position(|candidate| std::sync::Arc::ptr_eq(candidate, catalog))
            .map_or(-1, |index| index as i64)
    };
    let trail_generation = catalogs
        .iter()
        .rposition(|candidate| {
            std::sync::Arc::ptr_eq(&candidate.trail_packs, &snapshot.assets.trail_packs)
        })
        .map_or(-1, |index| index as i64);
    let kitty_generation = catalogs
        .iter()
        .rposition(|candidate| {
            candidate.kitty_sprite.fingerprint() == snapshot.assets.kitty_sprite.fingerprint()
        })
        .map_or(-1, |index| index as i64);
    let theme_generation = catalogs
        .iter()
        .rposition(|candidate| std::sync::Arc::ptr_eq(&candidate.themes, &snapshot.assets.themes))
        .map_or(-1, |index| index as i64);
    let sparkle_generation = catalogs
        .iter()
        .rposition(|candidate| {
            match (
                candidate.sparkle_spec_consumers.as_ref(),
                snapshot.assets.sparkle_spec_consumers.as_ref(),
            ) {
                (Some(candidate), Some(observed)) => std::sync::Arc::ptr_eq(candidate, observed),
                // Preliminary catalogs are never consumer-publishable, but
                // retaining an identity here keeps diagnostic projections
                // total and makes their incompleteness fail the consistency
                // predicate above.
                (None, None) => std::sync::Arc::ptr_eq(candidate, &snapshot.assets),
                _ => false,
            }
        })
        .map_or(-1, |index| index as i64);
    let mut state = model.init_state();
    state.insert("revision", relative(snapshot.revision, baseline_revision));
    state.insert("text_generation", text_generation);
    state.insert("trail_generation", trail_generation);
    state.insert("kitty_generation", kitty_generation);
    state.insert("theme_generation", theme_generation);
    state.insert("sparkle_generation", sparkle_generation);
    state.insert(
        "view_one_generation",
        consumers.view_one.map_or(0, asset_generation),
    );
    state.insert(
        "view_two_generation",
        consumers.view_two.map_or(0, asset_generation),
    );
    state.insert(
        "live_generation",
        consumers.live.map_or(0, asset_generation),
    );
    state.insert(
        "capture_generation",
        consumers.capture.map_or(0, asset_generation),
    );
    state
}

/// Tier-1 binding for the revisioned snapshot/catalog protocol. This drives the
/// genuine config reducer, Settings runtime, live host/window installer, and
/// capture preparation seam. Every consumer receives the exact same outer Arc;
/// independent stale-Trail and stale-rainbow kitty negative controls prove the path-asset
/// aggregate is not vacuous. Theme-directory staleness has its own parsed-catalog
/// Tier-1 test below because config edits deliberately retain that independent
/// immutable catalog.
#[test]
fn config_snapshot_catalog_is_atomic_across_patch_external_and_cross_view_delivery() {
    use std::sync::Arc;

    use crate::native_app::{AppEvent, AppViewState, NativeApp, NativeRuntime};
    use crate::native_settings::{SettingsApp, SettingsViewState};
    use crate::tab_model::ViewStore;
    use crate::update_screen::UpdateState;

    let synthwave = trail_pack_asset("synthwave.toml");
    let emberfall = trail_pack_asset("emberfall.toml");
    let sprites = KittyFixtures::new();
    let model = aterm_spec::derive::config_catalog_snapshot_model();
    let initial_text = trail_config(&synthwave, "synthwave", "Default", &sprites.sources[0]);
    let mut service = VersionedConfigService::new(initial_text).expect("valid initial config");
    let mut initial = service.snapshot();
    let initial_consumers = Arc::new(
        initial
            .config
            .prepare_sparkle_runtime()
            .consumer_capabilities(),
    );
    initial.assets = Arc::new(crate::app_config::ConfigAssetCatalog {
        trail_packs: Arc::clone(&initial.assets.trail_packs),
        kitty_sprite: initial.assets.kitty_sprite.clone(),
        wallpaper: initial.assets.wallpaper.clone(),
        themes: Arc::clone(&initial.assets.themes),
        sparkle_spec_consumers: Some(Arc::clone(&initial_consumers)),
    });
    assert!(snapshot_catalog_is_generation_consistent(&initial));
    assert_eq!(initial.assets.trail_packs.ids, ["synthwave"]);
    let initial_state = config_catalog_projection(
        &model,
        &initial,
        initial.revision,
        &[&initial.assets],
        CatalogConsumers::default(),
    );
    assert_eq!(initial_state, model.init_state());

    let mut patched = match service.patch(ConfigPatchRequest {
        base_revision: initial.revision,
        edits: vec![ConfigKeyEdit {
            key: "theme".to_string(),
            expected: ExpectedValue::Exact(Some("Default".to_string())),
            value: Some("Nord".to_string()),
        }],
    }) {
        ConfigPatchResult::Applied { snapshot, .. } => snapshot,
        result => panic!("patch must admit a complete generation: {result:?}"),
    };
    patched.assets = Arc::new(crate::app_config::ConfigAssetCatalog {
        trail_packs: Arc::clone(&patched.assets.trail_packs),
        kitty_sprite: patched.assets.kitty_sprite.clone(),
        wallpaper: patched.assets.wallpaper.clone(),
        themes: Arc::clone(&patched.assets.themes),
        sparkle_spec_consumers: Some(Arc::clone(&initial_consumers)),
    });
    assert_eq!(patched.revision, initial.revision + 1);
    assert!(snapshot_catalog_is_generation_consistent(&patched));
    assert_eq!(patched.assets.trail_packs.ids, ["synthwave"]);
    assert!(
        Arc::ptr_eq(
            initial
                .assets
                .sparkle_spec_consumers
                .as_ref()
                .expect("exact initial projection"),
            patched
                .assets
                .sparkle_spec_consumers
                .as_ref()
                .expect("exact patched projection")
        ),
        "a patch with unchanged sparkle sources reuses the exact admitted projection"
    );
    assert!(Arc::ptr_eq(
        &initial.assets.trail_packs,
        &patched.assets.trail_packs
    ));
    assert!(
        Arc::ptr_eq(&initial.assets.themes, &patched.assets.themes),
        "a config patch retains the independently watched immutable theme catalog"
    );
    assert_eq!(
        initial.assets.kitty_sprite.fingerprint(),
        patched.assets.kitty_sprite.fingerprint()
    );

    let patched_state = config_catalog_projection(
        &model,
        &patched,
        initial.revision,
        &[&initial.assets, &patched.assets],
        CatalogConsumers::default(),
    );
    assert_transition(&model, &initial_state, &patched_state, "AdmitPatch");

    let mut external = service
        .replace_external(trail_config(
            &emberfall,
            "emberfall",
            "Dracula",
            &sprites.sources[2],
        ))
        .expect("valid external generation");
    let external_consumers = Arc::new(
        external
            .config
            .prepare_sparkle_runtime()
            .consumer_capabilities(),
    );
    external.assets = Arc::new(crate::app_config::ConfigAssetCatalog {
        trail_packs: Arc::clone(&external.assets.trail_packs),
        kitty_sprite: external.assets.kitty_sprite.clone(),
        wallpaper: external.assets.wallpaper.clone(),
        themes: Arc::clone(&external.assets.themes),
        sparkle_spec_consumers: Some(Arc::clone(&external_consumers)),
    });
    assert_eq!(external.revision, patched.revision + 1);
    assert!(snapshot_catalog_is_generation_consistent(&external));
    assert_eq!(external.assets.trail_packs.ids, ["emberfall"]);
    let catalogs = [&initial.assets, &patched.assets, &external.assets];
    let external_state = config_catalog_projection(
        &model,
        &external,
        initial.revision,
        &catalogs,
        CatalogConsumers::default(),
    );
    assert_transition(&model, &patched_state, &external_state, "AdmitExternal");

    let mut runtime = NativeRuntime::new();
    let instance = runtime
        .insert_instance(NativeApp::Settings(SettingsApp::new_at_config_revision(
            UpdateState::from_status(1, "test", None, false),
            initial.revision,
        )))
        .unwrap();
    let mut views = ViewStore::default();
    let first = views.insert_native(instance).unwrap();
    let second = views.insert_native(instance).unwrap();
    let mut before_publish = external_state.clone();
    let mut first_assets = None;
    let mut second_assets = None;
    for (view, action) in [(first, "PublishOne"), (second, "PublishTwo")] {
        runtime
            .attach_view(
                view,
                instance,
                AppViewState::Settings(Box::new(
                    SettingsViewState::from_snapshot(&initial).unwrap(),
                )),
            )
            .unwrap();
        runtime
            .dispatch(instance, view, AppEvent::ConfigChanged(external.clone()))
            .unwrap();
        let Some(AppViewState::Settings(state)) = runtime.view_state(view) else {
            panic!("Settings view");
        };
        assert!(Arc::ptr_eq(state.config_assets(), &external.assets));
        assert!(Arc::ptr_eq(
            state
                .config_assets()
                .sparkle_spec_consumers
                .as_ref()
                .expect("Settings receives an exact sparkle projection"),
            external
                .assets
                .sparkle_spec_consumers
                .as_ref()
                .expect("external exact sparkle projection")
        ));
        assert!(Arc::ptr_eq(
            state.trail_pack_catalog(),
            &external.assets.trail_packs
        ));
        assert_eq!(state.legacy.trail_pack_ids, ["emberfall"]);
        if view == first {
            first_assets = Some(Arc::clone(state.config_assets()));
        } else {
            second_assets = Some(Arc::clone(state.config_assets()));
        }
        let after_publish = config_catalog_projection(
            &model,
            &external,
            initial.revision,
            &catalogs,
            CatalogConsumers {
                view_one: first_assets.as_ref(),
                view_two: second_assets.as_ref(),
                ..CatalogConsumers::default()
            },
        );
        assert_transition(&model, &before_publish, &after_publish, action);
        before_publish = after_publish;
    }

    // Delete every authored PNG after config admission. The genuine live and
    // capture paths below must continue to install the retained Ready Arc; any
    // accidental presentation-time reopen/decode would now fail deterministically.
    for source in &sprites.sources {
        std::fs::remove_file(source).expect("remove admitted source before presentation");
    }

    // The genuine live host and window effects state receive the same outer Arc
    // and the effects engine retains the exact Ready RGBA allocation.
    let mut app = crate::App::headless_for_test();
    let wid = crate::WindowId(0);
    assert_eq!(app.publish_config_assets(Arc::clone(&external.assets)), 1);
    assert!(Arc::ptr_eq(&app.config_assets, &external.assets));
    assert!(!app.install_window_config_assets(wid));
    let live_assets = app
        .windows
        .get(&wid)
        .and_then(|window| window.installed_config_assets.as_ref())
        .map(Arc::clone)
        .expect("live window catalog");
    assert!(Arc::ptr_eq(&live_assets, &external.assets));
    assert!(Arc::ptr_eq(
        live_assets
            .sparkle_spec_consumers
            .as_ref()
            .expect("live host exact sparkle projection"),
        external
            .assets
            .sparkle_spec_consumers
            .as_ref()
            .expect("external exact sparkle projection")
    ));
    let crate::app_config::KittySpriteAsset::Ready { rgba, fp, .. } = &external.assets.kitty_sprite
    else {
        panic!("external custom Nyan asset must be Ready");
    };
    let window = app.windows.get(&wid).expect("headless window");
    assert_eq!(
        window.installed_kitty_asset_fp,
        external.assets.kitty_sprite.fingerprint()
    );
    assert_eq!(
        window.word_decos.kitty_sprite_source_fingerprint(),
        Some(*fp)
    );
    assert!(Arc::ptr_eq(
        window
            .word_decos
            .kitty_sprite_rgba()
            .expect("installed RGBA"),
        rgba
    ));
    let after_live = config_catalog_projection(
        &model,
        &external,
        initial.revision,
        &catalogs,
        CatalogConsumers {
            view_one: first_assets.as_ref(),
            view_two: second_assets.as_ref(),
            live: Some(&live_assets),
            capture: None,
        },
    );
    assert_transition(&model, &before_publish, &after_live, "PublishLive");

    // Force the actual capture preparation seam to reinstall. It must land the
    // identical outer catalog and RGBA Arc without resolving any source again.
    {
        let window = app.windows.get_mut(&wid).expect("headless window");
        window.installed_config_assets = None;
        window
            .word_decos
            .set_kitty_sprite_source(aterm_effects::word_decorations::KittySpriteSource::BuiltIn);
    }
    app.splice_word_decorations(wid, std::time::Instant::now());
    let capture_assets = app
        .windows
        .get(&wid)
        .and_then(|window| window.installed_config_assets.as_ref())
        .map(Arc::clone)
        .expect("capture catalog");
    assert!(Arc::ptr_eq(&capture_assets, &external.assets));
    assert!(Arc::ptr_eq(
        capture_assets
            .sparkle_spec_consumers
            .as_ref()
            .expect("capture exact sparkle projection"),
        external
            .assets
            .sparkle_spec_consumers
            .as_ref()
            .expect("external exact sparkle projection")
    ));
    assert!(Arc::ptr_eq(
        app.windows[&wid]
            .word_decos
            .kitty_sprite_rgba()
            .expect("capture RGBA"),
        rgba
    ));
    let after_capture = config_catalog_projection(
        &model,
        &external,
        initial.revision,
        &catalogs,
        CatalogConsumers {
            view_one: first_assets.as_ref(),
            view_two: second_assets.as_ref(),
            live: Some(&live_assets),
            capture: Some(&capture_assets),
        },
    );
    assert_transition(&model, &after_live, &after_capture, "PublishCapture");

    // Independent negative control 1: external text/rainbow kitty paired with the stale
    // initial Trail generation must be rejected.
    let stale_trail = ConfigSnapshot {
        revision: external.revision,
        analysis_generation: external.analysis_generation,
        text: Arc::clone(&external.text),
        config: Arc::clone(&external.config),
        semantic_values: Arc::clone(&external.semantic_values),
        assets: Arc::new(crate::app_config::ConfigAssetCatalog {
            trail_packs: Arc::clone(&initial.assets.trail_packs),
            kitty_sprite: external.assets.kitty_sprite.clone(),
        wallpaper: external.assets.wallpaper.clone(),
            themes: Arc::clone(&external.assets.themes),
            sparkle_spec_consumers: external.assets.sparkle_spec_consumers.clone(),
        }),
    };
    assert!(!snapshot_catalog_is_generation_consistent(&stale_trail));
    let stale_trail_state = config_catalog_projection(
        &model,
        &stale_trail,
        initial.revision,
        &catalogs,
        CatalogConsumers::default(),
    );
    assert_eq!(stale_trail_state["trail_generation"], 1);
    assert_eq!(stale_trail_state["kitty_generation"], 2);
    assert_eq!(stale_trail_state["theme_generation"], 2);
    assert_eq!(stale_trail_state["sparkle_generation"], 2);
    assert_eq!(admits(&model, &patched_state, &stale_trail_state), None);
    assert!(!model.check_invariant("SnapshotAtomic", &stale_trail_state));

    // Independent negative control 2: external text/Trail paired with the stale
    // initial rainbow kitty generation must also be rejected.
    let stale_kitty = ConfigSnapshot {
        revision: external.revision,
        analysis_generation: external.analysis_generation,
        text: Arc::clone(&external.text),
        config: Arc::clone(&external.config),
        semantic_values: Arc::clone(&external.semantic_values),
        assets: Arc::new(crate::app_config::ConfigAssetCatalog {
            trail_packs: Arc::clone(&external.assets.trail_packs),
            kitty_sprite: initial.assets.kitty_sprite.clone(),
        wallpaper: initial.assets.wallpaper.clone(),
            themes: Arc::clone(&external.assets.themes),
            sparkle_spec_consumers: external.assets.sparkle_spec_consumers.clone(),
        }),
    };
    assert!(!snapshot_catalog_is_generation_consistent(&stale_kitty));
    let stale_kitty_state = config_catalog_projection(
        &model,
        &stale_kitty,
        initial.revision,
        &catalogs,
        CatalogConsumers::default(),
    );
    assert_eq!(stale_kitty_state["trail_generation"], 2);
    assert_eq!(stale_kitty_state["kitty_generation"], 1);
    assert_eq!(stale_kitty_state["theme_generation"], 2);
    assert_eq!(stale_kitty_state["sparkle_generation"], 2);
    assert_eq!(admits(&model, &patched_state, &stale_kitty_state), None);
    assert!(!model.check_invariant("SnapshotAtomic", &stale_kitty_state));

    // Independent negative control 3: every visual asset from the external
    // generation paired with the stale initial custom-spec projection must be
    // rejected even though both payloads are immutable and well-typed.
    let stale_sparkle = ConfigSnapshot {
        revision: external.revision,
        analysis_generation: external.analysis_generation,
        text: Arc::clone(&external.text),
        config: Arc::clone(&external.config),
        semantic_values: Arc::clone(&external.semantic_values),
        assets: Arc::new(crate::app_config::ConfigAssetCatalog {
            trail_packs: Arc::clone(&external.assets.trail_packs),
            kitty_sprite: external.assets.kitty_sprite.clone(),
        wallpaper: external.assets.wallpaper.clone(),
            themes: Arc::clone(&external.assets.themes),
            sparkle_spec_consumers: initial.assets.sparkle_spec_consumers.clone(),
        }),
    };
    assert!(!snapshot_catalog_is_generation_consistent(&stale_sparkle));
    let stale_sparkle_state = config_catalog_projection(
        &model,
        &stale_sparkle,
        initial.revision,
        &catalogs,
        CatalogConsumers::default(),
    );
    assert_eq!(stale_sparkle_state["trail_generation"], 2);
    assert_eq!(stale_sparkle_state["kitty_generation"], 2);
    assert_eq!(stale_sparkle_state["theme_generation"], 2);
    assert_eq!(stale_sparkle_state["sparkle_generation"], 1);
    assert_eq!(admits(&model, &patched_state, &stale_sparkle_state), None);
    assert!(!model.check_invariant("SnapshotAtomic", &stale_sparkle_state));
}

/// Tier-1 binding for the byte-identical external refresh arm: a referenced
/// manifest can change while `aterm.toml` does not. The service must publish a
/// new complete snapshot generation rather than retaining the old catalog.
#[test]
fn byte_equal_external_refresh_conforms_to_atomic_catalog_model() {
    use std::sync::atomic::{AtomicU64, Ordering};

    static NEXT: AtomicU64 = AtomicU64::new(1);
    let root = std::env::temp_dir().join(format!(
        "aterm-config-refresh-conformance-{}-{}",
        std::process::id(),
        NEXT.fetch_add(1, Ordering::Relaxed)
    ));
    std::fs::create_dir_all(&root).unwrap();
    let manifest = root.join("trail-pack.toml");
    let first = include_str!("../../aterm-effects/assets/trail-packs/synthwave.toml");
    std::fs::write(&manifest, first).unwrap();
    let text = format!(
        "theme = \"Default\"\ncursor_trail_style = \"pack:synthwave\"\n\
         cursor_trail_packs = [{:?}]\n",
        manifest.to_string_lossy()
    );
    let mut service = VersionedConfigService::new(text.clone()).unwrap();
    let initial = service.snapshot();
    let initial_pack = initial.assets.trail_packs.packs["synthwave"];

    let second = first.replace("span_deg = 300.0", "span_deg = 240.0");
    assert_ne!(second, first, "fixture mutation must be effective");
    std::fs::write(&manifest, second).unwrap();
    let refreshed = service.replace_external(text).unwrap();
    assert_eq!(refreshed.text, initial.text);
    assert_eq!(refreshed.revision, initial.revision + 1);
    assert_ne!(
        refreshed.assets.trail_packs.packs["synthwave"],
        initial_pack
    );

    let model = aterm_spec::derive::config_catalog_snapshot_model();
    let mut projected = model.init_state();
    for key in [
        "revision",
        "text_generation",
        "trail_generation",
        "kitty_generation",
        "theme_generation",
        "sparkle_generation",
    ] {
        projected.insert(key, 1);
    }
    projected.insert("asset_refresh", 1);
    assert_transition(&model, &model.init_state(), &projected, "RefreshAssets");
    assert!(model.check_invariant("SnapshotAtomic", &projected));

    // Negative control: retaining the genuine initial Trail catalog would be
    // the stale mixed-generation state forbidden by the model.
    let mut stale = projected;
    stale.insert("trail_generation", 0);
    assert!(!model.check_invariant("SnapshotAtomic", &stale));
    let _ = std::fs::remove_dir_all(root);
}

/// Tier-1 binding for the background theme-directory refresh. The worker hands
/// the reducer a fully parsed immutable catalog; the reducer advances one outer
/// config snapshot and every consumer-visible generation moves atomically.
#[test]
fn parsed_theme_catalog_refresh_conforms_to_atomic_catalog_model() {
    use std::sync::Arc;

    let mut service = VersionedConfigService::new("theme = \"Default\"\n".into()).unwrap();
    let initial = service.snapshot();
    let scheme = aterm_types::scheme::builtin("Dracula").unwrap();
    let themes =
        crate::app_config::ThemeCatalog::from_schemes([("Work".to_string(), scheme.clone())]);
    let refreshed = service.replace_theme_catalog(themes);
    assert_eq!(refreshed.text, initial.text);
    assert_eq!(refreshed.revision, initial.revision + 1);
    assert!(!Arc::ptr_eq(&refreshed.assets, &initial.assets));
    assert!(Arc::ptr_eq(
        &refreshed.assets.trail_packs,
        &initial.assets.trail_packs
    ));
    assert_eq!(
        refreshed.assets.kitty_sprite.fingerprint(),
        initial.assets.kitty_sprite.fingerprint()
    );
    assert_eq!(refreshed.assets.themes.resolve("Work"), Ok(scheme));

    let model = aterm_spec::derive::config_catalog_snapshot_model();
    let mut projected = model.init_state();
    for key in [
        "revision",
        "text_generation",
        "trail_generation",
        "kitty_generation",
        "theme_generation",
        "sparkle_generation",
    ] {
        projected.insert(key, 1);
    }
    projected.insert("asset_refresh", 2);
    assert_transition(&model, &model.init_state(), &projected, "RefreshThemes");
    assert!(model.check_invariant("SnapshotAtomic", &projected));

    let mut stale = projected;
    stale.insert("theme_generation", 0);
    assert!(!model.check_invariant("SnapshotAtomic", &stale));
}
