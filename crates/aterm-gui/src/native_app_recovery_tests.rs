// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

use super::*;
use aterm_models_native::native_recovery_interaction_model;
use aterm_spec::derive::Model;
use aterm_spec::interp::{State, admits};

/// A recovery page whose diagnostics span at least three pages at every target size,
/// with one unbroken token wider than the widest column so it hard-breaks mid-token.
fn recovery(capability: Option<RecoveryCapability>) -> RecoveryApp {
    let mut metadata = (0..60)
        .map(|line| format!("diagnostic-{line:03}=bounded detail\n"))
        .collect::<String>();
    metadata.push_str("unicode=e\u{301} · 👨‍👩‍👧‍👦 · 日本語 · مرحبًا\n");
    metadata.push_str("unbroken=");
    metadata.push_str(&"opaque-token-🦀".repeat(40));
    RecoveryApp {
        restore_tag: "markdown".to_string(),
        reason: "The original reader state could not be restored safely".to_string(),
        metadata,
        capability,
    }
}

fn recovery_model_projection(
    model: &Model,
    view: &RecoveryViewState,
    starts: i64,
    completions: i64,
    last_completion_was_stale: i64,
) -> State {
    let mut state = model.init_state();
    state.insert("page", i64::try_from(view.page).expect("bounded page"));
    state.insert(
        "pending",
        view.pending.map_or(0, |pending| match pending.action {
            RecoveryPendingAction::Retry | RecoveryPendingAction::OpenOriginal => 1,
            RecoveryPendingAction::CopyDiagnostics => 2,
        }),
    );
    state.insert("inflight", i64::from(view.pending.is_some()));
    state.insert("starts", starts);
    state.insert("completions", completions);
    state.insert("last_completion_was_stale", last_completion_was_stale);
    state
}

fn assert_recovery_transition(model: &Model, before: &State, after: &State, action: &'static str) {
    assert_eq!(admits(model, before, after), Some(action));
    for invariant in &model.invariants {
        assert!(
            model.check_invariant(invariant.name, after),
            "{}::{} rejected real recovery state {after:?}",
            model.name,
            invariant.name,
        );
    }
}

#[test]
fn recovery_pages_keep_actions_and_every_diagnostic_reachable_at_all_target_sizes() {
    let app = recovery(Some(RecoveryCapability::Document {
        kind: AppKind::Markdown,
        uri: "file:///tmp/Guide.md".to_string(),
        config_editor: false,
    }));
    for (width, height, scale) in [
        (320.0, 568.0, 0.85),
        (320.0, 568.0, 1.0),
        (390.0, 844.0, 2.0),
        (800.0, 320.0, 1.0),
        (800.0, 320.0, 2.0),
        (1_200.0, 400.0, 1.0),
    ] {
        let viewport = LogicalRect::new(0.0, 0.0, width, height);
        let first =
            recovery_page_projection_at_scale(&app, &RecoveryViewState::default(), viewport, scale);
        assert!(!first.lines.is_empty());
        assert!(
            first.pages >= 3,
            "{width}x{height}@{scale}: the fixture spans {} page(s), not the several this walk needs",
            first.pages
        );
        let available_width = width - first.padding * 2.0;
        for line in &first.lines {
            let (face, px) = recovery_line_typography(line.style, scale);
            let required = crate::tray_raster::ui_text_width_for(face, &line.text, px);
            assert!(
                required <= available_width + 0.5,
                "{width}x{height}@{scale} line overflows: {:?} needs {required} of {available_width}",
                line.text,
            );
        }
        let occupied = first.padding * 2.0
            + first.header_height
            + first.notice_height
            + first.action_height
            + first.gap * 3.0
            + first.lines.len() as f32 * first.line_height;
        assert!(
            occupied <= height + 0.5,
            "{width}x{height}@{scale} occupies {occupied}"
        );

        let final_page = recovery_page_projection_at_scale(
            &app,
            &RecoveryViewState {
                page: usize::MAX,
                ..RecoveryViewState::default()
            },
            viewport,
            scale,
        );
        assert_eq!(final_page.page + 1, final_page.pages);
        assert!(!final_page.lines.is_empty());

        let mut displayed = String::new();
        let mut token_lines = 0usize;
        for page in 0..first.pages {
            let projection = recovery_page_projection_at_scale(
                &app,
                &RecoveryViewState {
                    page,
                    ..RecoveryViewState::default()
                },
                viewport,
                scale,
            );
            for line in projection.lines {
                let (face, px) = recovery_line_typography(line.style, scale);
                let required = crate::tray_raster::ui_text_width_for(face, &line.text, px);
                assert!(required <= available_width + 0.5, "wrapped line must fit");
                token_lines += usize::from(line.text.contains("opaque-token"));
                displayed.push_str(&line.text);
            }
        }
        assert!(
            token_lines >= 2,
            "{width}x{height}@{scale}: the unbroken token hard-breaks across lines ({token_lines})"
        );
        assert!(displayed.contains("👨‍👩‍👧‍👦"), "ZWJ grapheme stays whole");
        assert!(displayed.contains("日本語"));
    }
}

#[test]
fn recovery_compiled_ui_is_in_bounds_actionable_and_text_fit() {
    let app = recovery(Some(RecoveryCapability::Document {
        kind: AppKind::Markdown,
        uri: "file:///tmp/Guide.md".to_string(),
        config_editor: false,
    }));
    for (width, height) in [(320.0, 568.0), (800.0, 320.0), (1_200.0, 400.0)] {
        let viewport = LogicalRect::new(0.0, 0.0, width, height);
        let compiled = app
            .view(
                &RecoveryViewState::default(),
                &ViewCx {
                    viewport,
                    animation_phase_ms: 0,
                    motion: ViewMotionCx::default(),
                    terminal_font_px: 13.0,
                    terminal_scale: 1.0,
                    semantic_font: None,
                    document: None,
                },
            )
            .compile(viewport)
            .unwrap();
        for (key, action) in [
            ("recovery/retry", "recovery/retry"),
            ("recovery/open-original", "recovery/open-original"),
            ("recovery/copy", "recovery/copy"),
            ("recovery/previous", "recovery/page/0"),
            ("recovery/next", "recovery/page/1"),
        ] {
            let semantic = compiled
                .semantic(&UiKey::new(key))
                .expect("recovery action");
            assert_eq!(semantic.action.as_ref().map(ActionId::as_str), Some(action));
            assert!(semantic.rect.x >= 0.0 && semantic.rect.y >= 0.0);
            assert!(semantic.rect.right() <= width && semantic.rect.bottom() <= height);
        }
        let audit = compiled.paint_audit_lines();
        assert!(audit.iter().all(|line| {
            !(line.contains("key=\"recovery/line/") && line.contains("overflow=true"))
        }));
        for key in [
            "recovery/retry",
            "recovery/open-original",
            "recovery/copy",
            "recovery/previous",
            "recovery/next",
        ] {
            assert!(audit.iter().any(|line| {
                line.contains(&format!("key={key:?}")) && line.contains("overflow=false")
            }));
        }
        compiled.validate_parity().unwrap();
    }
}

/// A blank line in the diagnostics is the space between paragraphs: it paints, and a screen
/// reader never stops on it, as it would on an element with nothing to say.
#[test]
fn a_recovery_pages_blank_lines_are_not_spoken() {
    let mut app = recovery(None);
    app.metadata = "first=1\n\nsecond=2\n".to_string();
    let viewport = LogicalRect::new(0.0, 0.0, 800.0, 600.0);
    let compiled = app
        .view(
            &RecoveryViewState::default(),
            &ViewCx {
                viewport,
                animation_phase_ms: 0,
                motion: ViewMotionCx::default(),
                terminal_font_px: 13.0,
                terminal_scale: 1.0,
                semantic_font: None,
                document: None,
            },
        )
        .compile(viewport)
        .unwrap();
    let blank = compiled
        .paint
        .iter()
        .find(|node| {
            node.key.as_str().starts_with("recovery/line/")
                && matches!(&node.content, crate::native_ui::UiContent::Text(spec) if spec.text.trim().is_empty())
        })
        .expect("the blank line paints");
    assert!(compiled.semantic(&blank.key).is_none(), "{:?}", blank.key);
    assert!(
        compiled
            .semantics
            .iter()
            .any(|node| node.label == "second=2"),
        "the lines around it are read"
    );
}

#[test]
fn shipping_recovery_reducer_conforms_to_page_single_flight_and_matching_completion_model() {
    let model = native_recovery_interaction_model();
    let app = recovery(Some(RecoveryCapability::Document {
        kind: AppKind::Markdown,
        uri: "file:///tmp/Guide.md".to_string(),
        config_editor: false,
    }));
    let mut runtime = NativeRuntime::new();
    let instance = runtime.insert_instance(NativeApp::Recovery(app)).unwrap();
    let view_id = ViewId::from_stored(91);
    runtime
        .attach_view(
            view_id,
            instance,
            AppViewState::Recovery(RecoveryViewState::default()),
        )
        .unwrap();
    let real_view = |runtime: &NativeRuntime| match runtime.view_state(view_id).unwrap() {
        AppViewState::Recovery(view) => view.clone(),
        _ => unreachable!(),
    };

    let mut before = recovery_model_projection(&model, &real_view(&runtime), 0, 0, 0);
    assert_eq!(before, model.init_state());
    runtime
        .dispatch(
            instance,
            view_id,
            AppEvent::Action(ActionInvocation {
                id: ActionId::new("recovery/page/1"),
                value: None,
            }),
        )
        .unwrap();
    let after_page = recovery_model_projection(&model, &real_view(&runtime), 0, 0, 0);
    assert_recovery_transition(&model, &before, &after_page, "NextPage");
    before = after_page;

    let report = runtime
        .dispatch(
            instance,
            view_id,
            AppEvent::Action(ActionInvocation {
                id: ActionId::new("recovery/retry"),
                value: None,
            }),
        )
        .unwrap();
    let operation = match report.effects.as_slice() {
        [
            AppEffect::Recovery {
                request:
                    RecoveryRequest::Retry(RecoveryCapability::Document {
                        kind: AppKind::Markdown,
                        uri,
                        config_editor: false,
                    }),
                reply,
            },
        ] if uri == "file:///tmp/Guide.md" => reply.operation,
        effects => panic!("expected one typed recovery start, got {effects:?}"),
    };
    let pending = recovery_model_projection(&model, &real_view(&runtime), 1, 0, 0);
    assert_recovery_transition(&model, &before, &pending, "BeginRetry");

    let duplicate = runtime
        .dispatch(
            instance,
            view_id,
            AppEvent::Action(ActionInvocation {
                id: ActionId::new("recovery/copy"),
                value: None,
            }),
        )
        .unwrap();
    assert!(duplicate.effects.is_empty());
    assert_eq!(
        recovery_model_projection(&model, &real_view(&runtime), 1, 0, 0),
        pending
    );

    runtime
        .dispatch(
            instance,
            view_id,
            AppEvent::ClipboardFinished {
                operation,
                outcome: ClipboardOutcome::Copied,
            },
        )
        .unwrap();
    let stale_kind = recovery_model_projection(&model, &real_view(&runtime), 1, 1, 1);
    assert_recovery_transition(&model, &pending, &stale_kind, "StaleComplete");

    let wrong_operation = OperationId(operation.get().saturating_add(10_000));
    runtime
        .dispatch(
            instance,
            view_id,
            AppEvent::RecoveryFinished {
                operation: wrong_operation,
                outcome: RecoveryOutcome::Opened {
                    message: "stale".to_string(),
                },
            },
        )
        .unwrap();
    let stale_id = recovery_model_projection(&model, &real_view(&runtime), 1, 2, 1);
    assert_recovery_transition(&model, &stale_kind, &stale_id, "StaleComplete");

    runtime
        .dispatch(
            instance,
            view_id,
            AppEvent::RecoveryFinished {
                operation,
                outcome: RecoveryOutcome::Opened {
                    message: "Recovered".to_string(),
                },
            },
        )
        .unwrap();
    let completed = recovery_model_projection(&model, &real_view(&runtime), 1, 3, 0);
    assert_recovery_transition(&model, &stale_id, &completed, "MatchingComplete");

    let mut stale_cleared = pending.clone();
    stale_cleared.insert("pending", 0);
    stale_cleared.insert("inflight", 0);
    stale_cleared.insert("completions", 1);
    stale_cleared.insert("last_completion_was_stale", 1);
    assert_eq!(admits(&model, &pending, &stale_cleared), None);
    assert!(!model.check_invariant("StaleCannotClear", &stale_cleared));
}

#[test]
fn diagnostics_only_recovery_exposes_literal_disabled_reasons() {
    let app = recovery(None);
    let view = RecoveryViewState::default();
    let mut commands = Vec::new();
    app.commands(&view, &mut commands);
    let retry = commands
        .iter()
        .find(|command| command.id.as_str() == "recovery/retry")
        .unwrap();
    assert!(!retry.enabled);
    let original = commands
        .iter()
        .find(|command| command.id.as_str() == "recovery/open-original")
        .unwrap();
    assert!(!original.enabled);
}
