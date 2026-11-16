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
        origin: RecoveryOrigin::Restore,
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

/// A Recovery tab standing for a document the person asked to open, which did not; `entry` when
/// that open was a directory listing's Enter its read never resolved.
fn failed_open_from(capability: Option<RecoveryCapability>, entry: bool) -> RecoveryApp {
    RecoveryApp {
        restore_tag: "markdown".to_string(),
        reason: crate::app_documents::DRAFTS_BUSY_PROBLEM.to_string(),
        metadata: crate::app_documents::InstallFailure::DRAFTS_BUSY.to_string(),
        capability,
        origin: RecoveryOrigin::Open {
            path: "/tmp/notes/Guide.md".to_string(),
            reveal: Some(crate::app_documents::DocumentReveal::MarkdownFragment(
                "usage".to_string(),
            )),
            entry,
            asked: None,
        },
    }
}

fn failed_open(capability: Option<RecoveryCapability>) -> RecoveryApp {
    failed_open_from(capability, false)
}

fn guide_document() -> Option<RecoveryCapability> {
    Some(RecoveryCapability::Document {
        kind: AppKind::Markdown,
        uri: "file:///tmp/notes/Guide.md".to_string(),
        config_editor: false,
    })
}

/// A failed open's page says what went wrong and names the file, not a restore type; its tab is
/// the file's name with no attention mark (the person is looking at the window that asked), and
/// Retry opens the file again. NEGATIVE CONTROL (scratch): drawing the Open origin with the
/// Restore arm's second line fails the "Restore type" check.
#[test]
fn an_open_origin_page_names_the_file_and_asks_for_no_attention() {
    let app = failed_open(guide_document());
    let view = RecoveryViewState::default();
    let lines = recovery_page_projection_at_scale(
        &app,
        &view,
        LogicalRect::new(0.0, 0.0, 1_200.0, 800.0),
        1.0,
    )
    .lines
    .into_iter()
    .map(|line| line.text)
    .collect::<Vec<_>>();
    assert!(
        !lines.iter().any(|line| line.contains("Restore type")),
        "{lines:?}"
    );
    assert!(
        lines
            .iter()
            .any(|line| line.contains("/tmp/notes/Guide.md")),
        "the page names the file: {lines:?}"
    );
    let presentation = app.presentation(&view);
    assert_eq!(presentation.title, "Guide.md");
    assert!(!presentation.indicators.attention);
    assert!(
        presentation
            .tooltip
            .as_deref()
            .is_some_and(|tooltip| tooltip.contains("/tmp/notes/Guide.md")),
        "{:?}",
        presentation.tooltip
    );
    let viewport = LogicalRect::new(0.0, 0.0, 800.0, 600.0);
    let compiled = app
        .view(
            &view,
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
    let heading = compiled
        .semantics
        .iter()
        .find(|node| node.role == crate::native_ui::SemanticRole::Heading)
        .expect("the page's heading");
    assert!(
        heading.label.contains("Guide.md"),
        "the heading names the file: {}",
        heading.label
    );
    let retry_of = |app: &RecoveryApp| {
        let mut commands = Vec::new();
        app.commands(&view, &mut commands);
        commands
            .into_iter()
            .find(|command| command.id.as_str() == "recovery/retry")
            .unwrap()
    };
    let retry = retry_of(&app);
    assert!(retry.enabled);

    let restored = recovery(guide_document());
    assert!(restored.presentation(&view).indicators.attention);
    assert_ne!(
        retry.title,
        retry_of(&restored).title,
        "an open's Retry is not a restore's"
    );
    let without = failed_open(None);
    let mut commands = Vec::new();
    without.commands(&view, &mut commands);
    assert!(
        commands
            .iter()
            .filter(|command| command.id.as_str() != "recovery/copy")
            .all(|command| !command.enabled),
        "no URI, no Retry and no Open Original"
    );
}

/// The Open origin's reducer refines the same single-flight model as a restored leaf's: Retry
/// starts one typed Reopen carrying the open's reveal, Open Original one Reopen in the editor, and
/// a matching Started completion clears pending like any other. NEGATIVE CONTROL: a Started
/// completion for another operation is StaleComplete and leaves pending.
#[test]
fn an_open_origin_retry_reopens_through_admission_and_started_clears_pending() {
    let model = native_recovery_interaction_model();
    let mut runtime = NativeRuntime::new();
    let instance = runtime
        .insert_instance(NativeApp::Recovery(failed_open(guide_document())))
        .unwrap();
    let view_id = ViewId::from_stored(92);
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
    let act = |runtime: &mut NativeRuntime, id: &str| {
        runtime
            .dispatch(
                instance,
                view_id,
                AppEvent::Action(ActionInvocation {
                    id: ActionId::new(id),
                    value: None,
                }),
            )
            .unwrap()
    };

    let before = recovery_model_projection(&model, &real_view(&runtime), 0, 0, 0);
    let report = act(&mut runtime, "recovery/retry");
    let operation = match report.effects.as_slice() {
        [
            AppEffect::Recovery {
                request:
                    RecoveryRequest::Reopen {
                        kind: AppKind::Markdown,
                        uri,
                        reveal:
                            Some(crate::app_documents::DocumentReveal::MarkdownFragment(fragment)),
                        entry: false,
                    },
                reply,
            },
        ] if uri == "file:///tmp/notes/Guide.md" && fragment == "usage" => reply.operation,
        effects => panic!("expected one typed Reopen, got {effects:?}"),
    };
    assert!(
        real_view(&runtime).notice.is_some(),
        "the tab says it is opening"
    );
    let pending = recovery_model_projection(&model, &real_view(&runtime), 1, 0, 0);
    assert_recovery_transition(&model, &before, &pending, "BeginRetry");

    runtime
        .dispatch(
            instance,
            view_id,
            AppEvent::RecoveryFinished {
                operation: OperationId(operation.get().saturating_add(10_000)),
                outcome: RecoveryOutcome::Started {
                    message: "stale".to_string(),
                },
            },
        )
        .unwrap();
    let stale = recovery_model_projection(&model, &real_view(&runtime), 1, 1, 1);
    assert_recovery_transition(&model, &pending, &stale, "StaleComplete");

    runtime
        .dispatch(
            instance,
            view_id,
            AppEvent::RecoveryFinished {
                operation,
                outcome: RecoveryOutcome::Started {
                    message: "Opening Guide.md…".to_string(),
                },
            },
        )
        .unwrap();
    let started = recovery_model_projection(&model, &real_view(&runtime), 1, 2, 0);
    assert_recovery_transition(&model, &stale, &started, "MatchingComplete");
    assert_eq!(real_view(&runtime).pending, None);
    assert_eq!(
        real_view(&runtime).notice.as_deref(),
        Some("Opening Guide.md…"),
        "the started outcome's own message"
    );

    let report = act(&mut runtime, "recovery/open-original");
    assert!(
        matches!(
            report.effects.as_slice(),
            [AppEffect::Recovery {
                request: RecoveryRequest::Reopen {
                    kind: AppKind::Editor,
                    reveal: None,
                    entry: false,
                    ..
                },
                ..
            }]
        ),
        "Open Original opens the file in the editor through admission: {:?}",
        report.effects
    );
}

/// A failed directory entry's Open Original follows the entry's link as its Retry does, so a link
/// whose target now exists opens: only the kind changes. NEGATIVE CONTROL (scratch): an Open
/// Original that drops the entry fails it.
#[test]
fn an_entrys_open_original_follows_its_link() {
    let mut runtime = NativeRuntime::new();
    let instance = runtime
        .insert_instance(NativeApp::Recovery(failed_open_from(
            guide_document(),
            true,
        )))
        .unwrap();
    let view_id = ViewId::from_stored(93);
    runtime
        .attach_view(
            view_id,
            instance,
            AppViewState::Recovery(RecoveryViewState::default()),
        )
        .unwrap();
    for (action, kind) in [
        ("recovery/open-original", AppKind::Editor),
        ("recovery/retry", AppKind::Markdown),
    ] {
        {
            let mut state = runtime.view_state_mut(view_id);
            if let Some(AppViewState::Recovery(view)) = state.as_deref_mut() {
                view.pending = None;
            }
        }
        let report = runtime
            .dispatch(
                instance,
                view_id,
                AppEvent::Action(ActionInvocation {
                    id: ActionId::new(action),
                    value: None,
                }),
            )
            .unwrap();
        assert!(
            matches!(
                report.effects.as_slice(),
                [AppEffect::Recovery {
                    request: RecoveryRequest::Reopen {
                        kind: requested,
                        entry: true,
                        ..
                    },
                    ..
                }] if *requested == kind
            ),
            "{action}: {:?}",
            report.effects
        );
    }
}
