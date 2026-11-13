// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

use super::*;
use crate::native_app::{
    ActionInvocation, AppEffect, AppViewState, NativeApp, NativeRuntime, ViewId, ViewMotionCx,
};
use crate::native_ui::LogicalRect;

fn setup() -> (NativeRuntime, crate::tab_model::AppInstanceId, ViewId) {
    let mut app = AboutApp::new();
    app.about.add_test_site("https://example.test/about");
    let mut runtime = NativeRuntime::new();
    let instance = runtime
        .insert_instance(NativeApp::About(Box::new(app)))
        .unwrap();
    let view = ViewId::from_stored(711);
    runtime
        .attach_view(view, instance, AppViewState::About(Box::default()))
        .unwrap();
    (runtime, instance, view)
}

fn action(
    runtime: &mut NativeRuntime,
    instance: crate::tab_model::AppInstanceId,
    view: ViewId,
    id: &str,
) -> crate::native_app::DispatchOutcome {
    runtime
        .dispatch(
            instance,
            view,
            AppEvent::Action(ActionInvocation {
                id: ActionId::new(id),
                value: None,
            }),
        )
        .unwrap()
}

fn context(width: f32, height: f32) -> ViewCx<'static> {
    ViewCx {
        viewport: LogicalRect::new(0.0, 0.0, width, height),
        animation_phase_ms: 0,
        motion: ViewMotionCx::default(),
        terminal_font_px: 16.0,
        terminal_scale: 1.0,
        semantic_font: None,
        document: None,
    }
}

#[test]
fn about_actions_have_typed_replies_and_duplicate_or_late_completions_are_inert() {
    let (mut runtime, instance, view) = setup();
    let copy = action(&mut runtime, instance, view, COPY);
    let (operation, text) = copy
        .effects
        .iter()
        .find_map(|effect| match effect {
            AppEffect::Clipboard {
                request:
                    ClipboardRequest::CopyText {
                        text,
                        sensitive: false,
                    },
                reply,
            } => Some((reply.operation, text.clone())),
            _ => None,
        })
        .expect("copy emits its view-bound clipboard request");
    assert!(text.contains(crate::build_info::version_display()));
    assert!(text.contains(aterm_types::identity::AUTHOR));
    assert!(
        action(&mut runtime, instance, view, COPY)
            .effects
            .is_empty()
    );
    assert!(
        action(&mut runtime, instance, view, SITE)
            .effects
            .is_empty()
    );
    let wrong = runtime
        .dispatch(
            instance,
            view,
            AppEvent::ExternalOpenFinished {
                operation,
                outcome: ExternalOpenOutcome::Opened,
            },
        )
        .unwrap();
    assert_eq!(wrong.result, EventResult::Bubble);
    assert!(
        !runtime
            .commands(instance, view)
            .unwrap()
            .iter()
            .find(|command| command.id.as_str() == COPY)
            .unwrap()
            .enabled
    );
    runtime
        .dispatch(
            instance,
            view,
            AppEvent::ClipboardFinished {
                operation,
                outcome: ClipboardOutcome::Failed {
                    message: "fixture refusal".to_string(),
                },
            },
        )
        .unwrap();
    let site = action(&mut runtime, instance, view, SITE);
    let site_operation = site
        .effects
        .iter()
        .find_map(|effect| match effect {
            AppEffect::OpenExternal { request, reply } => {
                assert_eq!(request.uri, "https://example.test/about");
                assert!(request.user_initiated);
                Some(reply.operation)
            }
            _ => None,
        })
        .expect("site emits an external intent without opening a browser");
    assert_ne!(operation, site_operation);
    let Some(AppViewState::About(state)) = runtime.view_state(view) else {
        unreachable!()
    };
    let site_feedback = state.feedback.clone();
    let stale = runtime
        .dispatch(
            instance,
            view,
            AppEvent::ClipboardFinished {
                operation,
                outcome: ClipboardOutcome::Copied,
            },
        )
        .unwrap();
    assert_eq!(stale.result, EventResult::Bubble);
    let Some(AppViewState::About(state)) = runtime.view_state(view) else {
        unreachable!()
    };
    assert_eq!(state.feedback, site_feedback);
    runtime
        .dispatch(
            instance,
            view,
            AppEvent::ExternalOpenFinished {
                operation: site_operation,
                outcome: ExternalOpenOutcome::Denied {
                    message: "fixture denial".to_string(),
                },
            },
        )
        .unwrap();
    let Some(AppViewState::About(state)) = runtime.view_state(view) else {
        unreachable!()
    };
    assert!(state.pending.is_none());
    assert!(state.feedback.as_ref().unwrap().contains("fixture denial"));
    assert!(
        runtime
            .commands(instance, view)
            .unwrap()
            .iter()
            .all(|command| command.enabled)
    );
    let copied = action(&mut runtime, instance, view, COPY);
    let operation = copied
        .effects
        .iter()
        .find_map(|effect| match effect {
            AppEffect::Clipboard { reply, .. } => Some(reply.operation),
            _ => None,
        })
        .unwrap();
    let finished = runtime
        .dispatch(
            instance,
            view,
            AppEvent::ClipboardFinished {
                operation,
                outcome: ClipboardOutcome::Copied,
            },
        )
        .unwrap();
    assert_eq!(finished.result, EventResult::Handled);
    let Some(AppViewState::About(state)) = runtime.view_state(view) else {
        unreachable!()
    };
    assert!(state.pending.is_none());
    assert!(state.feedback.is_some());
    assert!(runtime.presentation(instance, view).unwrap().closable);
}

#[test]
fn about_uses_its_canvas_and_reveals_details_at_narrow_and_short_sizes() {
    struct Scale(Option<f32>);
    impl Drop for Scale {
        fn drop(&mut self) {
            crate::native_appearance::inject_text_scale(self.0);
        }
    }
    let _scale = Scale(crate::native_appearance::injected_text_scale());
    let (mut runtime, instance, view) = setup();
    for scale in [1.0, 2.0] {
        crate::native_appearance::inject_text_scale(Some(scale));
        for (width, height) in [
            (1200.0, 820.0),
            (360.0, 640.0),
            (164.0, 558.0),
            (240.0, 180.0),
        ] {
            let cx = context(width, height);
            let compact = runtime
                .render(instance, view, &cx)
                .unwrap()
                .compile(cx.viewport)
                .unwrap();
            assert!(
                compact
                    .semantic(&UiKey::new("about/root"))
                    .is_some_and(|node| node.role == SemanticRole::Application)
            );
            assert!(
                compact
                    .semantics
                    .iter()
                    .all(|node| !node.key.as_str().starts_with("settings/"))
            );
            assert!(
                compact
                    .semantics
                    .iter()
                    .all(|node| !node.key.as_str().starts_with("about/provenance/"))
            );
            action(&mut runtime, instance, view, DETAILS);
            let full = runtime
                .render(instance, view, &cx)
                .unwrap()
                .compile(cx.viewport)
                .unwrap();
            assert!(
                full.semantics
                    .iter()
                    .any(|node| node.key.as_str().starts_with("about/provenance/commit"))
            );
            for node in &full.paint {
                if node.peek || !matches!(node.content, UiContent::Text(_)) {
                    continue;
                }
                assert!(
                    node.rect.x >= node.clip.x - 0.5
                        && node.rect.right() <= node.clip.right() + 0.5,
                    "{}: {:?} in {:?}",
                    node.key.as_str(),
                    node.rect,
                    node.clip
                );
                let audit = full.text_fit_audit_line(&node.key).unwrap();
                assert!(
                    audit.contains(" overflow=false "),
                    "{width}x{height}: {audit}"
                );
            }
            runtime
                .dispatch(
                    instance,
                    view,
                    AppEvent::FocusChanged(Some(UiKey::new(SITE))),
                )
                .unwrap();
            let focused = runtime
                .render(instance, view, &cx)
                .unwrap()
                .compile(cx.viewport)
                .unwrap();
            let site = focused.semantic(&UiKey::new(SITE)).unwrap();
            assert!(site.rect.y >= 0.0 && site.rect.bottom() <= cx.viewport.bottom() + 0.5);
            assert!(site.label.contains("https://example.test/about"));
            action(&mut runtime, instance, view, DETAILS);
        }
    }
}

#[test]
fn about_views_keep_their_own_details_scroll_and_idle_render() {
    let (mut runtime, instance, first) = setup();
    let second = ViewId::from_stored(712);
    runtime
        .attach_view(second, instance, AppViewState::About(Box::default()))
        .unwrap();
    let mut cx = context(360.0, 300.0);
    let before = runtime.render(instance, second, &cx).unwrap();
    action(&mut runtime, instance, first, DETAILS);
    runtime.render(instance, first, &cx).unwrap();
    runtime
        .dispatch(instance, first, AppEvent::ScrollPage(1))
        .unwrap();
    let Some(AppViewState::About(a)) = runtime.view_state(first) else {
        unreachable!()
    };
    assert!(a.details && a.scroll_offset() > 0.0);
    let Some(AppViewState::About(b)) = runtime.view_state(second) else {
        unreachable!()
    };
    assert!(!b.details && b.scroll_offset() == 0.0);
    cx.animation_phase_ms = 60_000;
    assert_eq!(
        runtime.render(instance, second, &cx).unwrap(),
        before,
        "an idle About has no phase-driven drawing or state from its sibling"
    );
}

#[test]
fn about_resize_keeps_its_already_visible_focused_action_in_view() {
    let (mut runtime, instance, first) = setup();
    let second = ViewId::from_stored(713);
    runtime
        .attach_view(second, instance, AppViewState::About(Box::default()))
        .unwrap();
    let wide = context(1_200.0, 820.0);
    let other_before = runtime.render(instance, second, &wide).unwrap();
    runtime.render(instance, first, &wide).unwrap();
    runtime
        .dispatch(
            instance,
            first,
            AppEvent::FocusChanged(Some(UiKey::new(SITE))),
        )
        .unwrap();
    let before = runtime
        .render(instance, first, &wide)
        .unwrap()
        .compile(wide.viewport)
        .unwrap();
    let site = before.semantic(&UiKey::new(SITE)).unwrap();
    assert!(site.state.is_some_and(|state| state.focused));
    assert!(!site.offscreen);
    assert!(site.rect.y >= wide.viewport.y);
    assert!(site.rect.bottom() <= wide.viewport.bottom());
    let Some(AppViewState::About(state)) = runtime.view_state(first) else {
        unreachable!()
    };
    assert_eq!(
        state.scroll_offset(),
        0.0,
        "the wide hero fits without scrolling"
    );

    // Resizing renders the same view; no new focus event repairs its position.
    let narrow = context(240.0, 180.0);
    let after = runtime
        .render(instance, first, &narrow)
        .unwrap()
        .compile(narrow.viewport)
        .unwrap();
    let site = after.semantic(&UiKey::new(SITE)).unwrap();
    assert!(site.state.is_some_and(|state| state.focused));
    let Some(AppViewState::About(state)) = runtime.view_state(first) else {
        unreachable!()
    };
    assert_eq!(state.common.last_focus.as_ref(), Some(&UiKey::new(SITE)));
    assert!(state.pending.is_none());
    let Some(AppViewState::About(other)) = runtime.view_state(second) else {
        unreachable!()
    };
    assert_eq!(other.scroll_offset(), 0.0);
    assert!(other.common.last_focus.is_none());
    assert_eq!(
        runtime.render(instance, second, &wide).unwrap(),
        other_before
    );
    assert!(
        !site.offscreen
            && site.rect.y >= narrow.viewport.y
            && site.rect.bottom() <= narrow.viewport.bottom(),
        "resizing hid the still-focused site action: {:?} in {:?}, offscreen={}",
        site.rect,
        narrow.viewport,
        site.offscreen
    );
    let revealed = state.scroll_offset();
    runtime
        .dispatch(instance, first, AppEvent::ScrollPage(-1))
        .unwrap();
    let Some(AppViewState::About(state)) = runtime.view_state(first) else {
        unreachable!()
    };
    let scrolled = state.scroll_offset();
    assert!(
        scrolled < revealed,
        "explicit scrolling moves focus out of view"
    );
    runtime.render(instance, first, &narrow).unwrap();
    let Some(AppViewState::About(state)) = runtime.view_state(first) else {
        unreachable!()
    };
    assert_eq!(
        state.scroll_offset(),
        scrolled,
        "paint must preserve explicit scrolling"
    );
}
