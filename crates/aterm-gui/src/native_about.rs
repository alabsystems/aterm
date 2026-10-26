// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! About is a read-only tab application, with its own focus, scrolling and replies.

use std::cell::{Cell, RefCell};

use aterm_grapheme::GraphemeClusters;

use crate::about::AboutState;
use crate::native_app::{
    AppDescriptor, AppEvent, AppIcon, AppIndicators, AppKind, AppPresentation, ClipboardOutcome,
    ClipboardRequest, CloseReadiness, CloseRequest, Command, CommonViewState, DamageRegion,
    EventResult, ExternalOpenOutcome, ExternalOpenRequest, NativeAppModel, OperationId, UpdateCx,
    ViewCx,
};
use crate::native_ui::{
    ActionId, AuditedCustomNode, ButtonSpec, Control, ControlState, Flow, GroupSpec, Insets,
    Layout, Length, SemanticRole, SemanticValue, StyleRef, TextSpec, UiContent, UiKey, UiNode,
    UiTree,
};

const COPY: &str = "about/copy-build-info";
const SITE: &str = "about/open-site";
const DETAILS: &str = "about/details";

pub(crate) struct AboutApp {
    about: AboutState,
}

impl AboutApp {
    pub(crate) fn new() -> Self {
        Self {
            about: AboutState::new(),
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum PendingKind {
    Copy,
    Site,
}

#[derive(Clone, Copy)]
struct Pending {
    operation: OperationId,
    kind: PendingKind,
}

struct Span {
    key: UiKey,
    top: f32,
    bottom: f32,
}

#[derive(Default)]
pub(crate) struct AboutViewState {
    pub(crate) common: CommonViewState,
    details: bool,
    pending: Option<Pending>,
    feedback: Option<String>,
    scroll: Cell<f32>,
    room: Cell<f32>,
    limit: Cell<f32>,
    spans: RefCell<Vec<Span>>,
}

impl AboutViewState {
    #[cfg(test)]
    pub(crate) fn scroll_offset(&self) -> f32 {
        self.scroll.get()
    }

    pub(crate) fn reveal_listed_control(&self, key: &UiKey) -> bool {
        let Some(span) = self
            .spans
            .borrow()
            .iter()
            .find(|span| span.key == *key)
            .map(|span| (span.top, span.bottom))
        else {
            return false;
        };
        let before = self.scroll.get();
        let room = self.room.get();
        let next = if span.0 < before || span.1 - span.0 > room {
            span.0
        } else if span.1 > before + room {
            span.1 - room
        } else {
            before
        };
        self.scroll.set(next.clamp(0.0, self.limit.get()));
        (self.scroll.get() - before).abs() > 0.5
    }

    fn scroll_by(&self, points: f32) {
        self.scroll
            .set((self.scroll.get() + points).clamp(0.0, self.limit.get()));
    }

    fn changed(&mut self, cx: &mut UpdateCx<'_>) {
        self.common.presentation_revision = self.common.presentation_revision.wrapping_add(1);
        cx.repaint(DamageRegion::All);
    }
}

impl NativeAppModel for AboutApp {
    type ViewState = AboutViewState;

    fn descriptor(&self) -> AppDescriptor {
        AppDescriptor {
            kind: AppKind::About,
            name: "About",
            icon: AppIcon::About,
            singleton: true,
        }
    }

    fn update(
        &mut self,
        view: &mut AboutViewState,
        event: AppEvent,
        cx: &mut UpdateCx<'_>,
    ) -> EventResult {
        match event {
            AppEvent::FocusChanged(focus) => {
                if let Some(key) = &focus {
                    view.reveal_listed_control(key);
                }
                view.common.last_focus = focus;
            }
            AppEvent::ScrollLines(lines) | AppEvent::ScrollAt { lines, .. } => {
                view.scroll_by(lines as f32 * 28.0 * crate::native_appearance::text_scale());
            }
            AppEvent::ScrollPage(direction) => {
                view.scroll_by(direction as f32 * (view.room.get() - 28.0).max(28.0));
            }
            #[cfg(any(a11y_tree, test))]
            AppEvent::RevealControl(key) => {
                view.reveal_listed_control(&key);
            }
            AppEvent::Action(action) if action.id.as_str() == DETAILS => {
                view.details = !view.details;
            }
            AppEvent::Action(action) if action.id.as_str() == COPY && view.pending.is_none() => {
                let operation = cx.clipboard(ClipboardRequest::CopyText {
                    text: crate::about::provenance_text(),
                    sensitive: false,
                });
                view.pending = Some(Pending {
                    operation,
                    kind: PendingKind::Copy,
                });
                view.feedback = Some("Copying build information…".to_string());
            }
            AppEvent::Action(action) if action.id.as_str() == SITE && view.pending.is_none() => {
                let Some(uri) = crate::about::site_url(&self.about) else {
                    return EventResult::Bubble;
                };
                let operation = cx.open_external(ExternalOpenRequest {
                    uri,
                    user_initiated: true,
                });
                view.pending = Some(Pending {
                    operation,
                    kind: PendingKind::Site,
                });
                view.feedback = Some("Opening project site…".to_string());
            }
            AppEvent::ClipboardFinished { operation, outcome }
                if view.pending.is_some_and(|pending| {
                    pending.operation == operation && pending.kind == PendingKind::Copy
                }) =>
            {
                view.pending = None;
                view.feedback = Some(match outcome {
                    ClipboardOutcome::Copied => "Build information copied".to_string(),
                    ClipboardOutcome::Failed { message } => {
                        format!("Couldn’t copy build information: {message}")
                    }
                });
            }
            AppEvent::ExternalOpenFinished { operation, outcome }
                if view.pending.is_some_and(|pending| {
                    pending.operation == operation && pending.kind == PendingKind::Site
                }) =>
            {
                view.pending = None;
                view.feedback = Some(match outcome {
                    ExternalOpenOutcome::Opened => "Project site opened".to_string(),
                    ExternalOpenOutcome::Denied { message } => {
                        format!("Couldn’t open project site: {message}")
                    }
                });
            }
            _ => return EventResult::Bubble,
        }
        view.changed(cx);
        EventResult::Handled
    }

    fn view(&self, view: &AboutViewState, cx: &ViewCx<'_>) -> UiTree {
        let scale = crate::native_appearance::text_scale();
        let pad = if cx.viewport.width < 480.0 * scale {
            12.0
        } else {
            24.0
        };
        let width = (cx.viewport.width - pad * 2.0).clamp(1.0, 900.0);
        let outer = ((cx.viewport.width - width) / 2.0).max(0.0);
        let room = (cx.viewport.height - pad * 2.0).max(1.0);
        let inner_pad = if width < 280.0 * scale { 12.0 } else { 24.0 };
        let inner = (width - inner_pad * 2.0).max(1.0);
        let gap = 12.0 * scale;
        let mut identity = vec![
            UiNode::new(
                "about/wordmark",
                UiContent::Custom(AuditedCustomNode {
                    audit_id: crate::native_ui::WORDMARK_AUDIT,
                    role: SemanticRole::Heading,
                    label: "aterm".to_string(),
                    paint: None,
                    action: None,
                    focusable: false,
                    focused: false,
                    focus_visible: false,
                }),
            )
            .layout(Layout::default().height(Length::Fixed(44.0 * scale))),
        ];
        identity.push(wrapped(
            "about/tagline",
            crate::build_info::TAGLINE,
            inner,
            SemanticRole::Text,
            StyleRef::Plain,
        ));
        identity.push(wrapped(
            "about/version-summary",
            &format!("Version {}", crate::build_info::version_display()),
            inner,
            SemanticRole::Text,
            StyleRef::Quiet,
        ));
        identity.push(wrapped(
            "about/byline",
            crate::build_info::AUTHOR_COMPANY_BYLINE,
            inner,
            SemanticRole::Text,
            StyleRef::Plain,
        ));
        let site = crate::about::site_url(&self.about);
        let site_words = self
            .about
            .semantic_rows()
            .iter()
            .find(|(key, _)| *key == "site")
            .map_or("Project Site", |(_, value)| value.as_str());
        let height = 36.0 * scale;
        let site_face = fitted_button_label(site_words, "Site", inner);
        let copy_face = fitted_button_label("Copy Build Information", "Copy", inner);
        let details_name = if view.details {
            "Hide Details"
        } else {
            "More Details"
        };
        let details_face = fitted_button_label(
            if view.details {
                "Hide Details"
            } else {
                "Details"
            },
            if view.details { "Hide" } else { "More" },
            inner,
        );
        let button =
            |key: &str, name: String, face: &str, enabled: bool, kind: Option<PendingKind>| {
                UiNode::new(
                    key,
                    UiContent::Button(
                        Control::new(ButtonSpec::new(name).visual_label(face), ActionId::new(key))
                            .state(ControlState {
                                enabled,
                                busy: kind.is_some_and(|kind| {
                                    view.pending.is_some_and(|pending| pending.kind == kind)
                                }),
                                expanded: (key == DETAILS).then_some(view.details),
                                ..ControlState::default()
                            })
                            .style(if key == COPY {
                                StyleRef::Primary
                            } else {
                                StyleRef::Secondary
                            })
                            .value(if key == SITE {
                                SemanticValue::Text(site.clone().unwrap_or_default())
                            } else {
                                SemanticValue::None
                            }),
                    ),
                )
                .layout(Layout::default().height(Length::Fixed(height)))
            };
        let actions = vec![
            button(
                SITE,
                site.as_ref().map_or_else(
                    || "Project Site".to_string(),
                    |uri| format!("Project Site: {uri}"),
                ),
                &site_face,
                site.is_some() && view.pending.is_none(),
                Some(PendingKind::Site),
            ),
            button(
                COPY,
                "Copy Build Information".to_string(),
                &copy_face,
                view.pending.is_none(),
                Some(PendingKind::Copy),
            ),
            button(DETAILS, details_name.to_string(), &details_face, true, None),
        ];
        let button_width = |face: &str| {
            let (px, font) =
                crate::native_ui::text_paint_metrics(SemanticRole::Button, StyleRef::Secondary);
            crate::tray_raster::ui_text_width_for(font, face, px).ceil() + 32.0
        };
        let faces = [
            site_face.as_str(),
            copy_face.as_str(),
            details_face.as_str(),
        ];
        let row = faces.into_iter().map(button_width).sum::<f32>() + gap * 2.0 <= inner;
        let actions = if row {
            actions
                .into_iter()
                .zip(faces)
                .map(|(mut node, face)| {
                    node.layout.width = Length::Fixed(button_width(face));
                    node
                })
                .collect()
        } else {
            actions
        };
        identity.push(
            UiNode::new(
                "about/actions",
                UiContent::Group(GroupSpec::new("About actions")),
            )
            .layout(
                (if row { Layout::row() } else { Layout::column() })
                    .height(Length::Fixed(if row {
                        height
                    } else {
                        height * 3.0 + gap * 2.0
                    }))
                    .gap(gap),
            )
            .children(actions),
        );
        if let Some(feedback) = &view.feedback {
            identity.push(wrapped(
                "about/feedback",
                feedback,
                inner,
                SemanticRole::Status,
                StyleRef::Quiet,
            ));
        }
        let identity_content_height = children_height(&identity, gap);
        let vertical_pad = if view.details {
            inner_pad
        } else {
            ((room - identity_content_height) / 2.0).max(inner_pad)
        };
        let identity_height = identity_content_height + vertical_pad * 2.0;
        let hero = UiNode::new(
            "about/hero",
            UiContent::Group(GroupSpec::new("About aterm").style(StyleRef::Hero)),
        )
        .layout(
            Layout::column()
                .height(Length::Fixed(identity_height))
                .padding(Insets::symmetric(inner_pad, vertical_pad))
                .gap(gap),
        )
        .children(identity);
        let mut sections = vec![hero];
        if view.details {
            sections.push(wrapped(
                "about/build-heading",
                "Build Information",
                width,
                SemanticRole::Heading,
                StyleRef::Plain,
            ));
            for (key, value) in self.about.semantic_rows().iter().filter(|(key, _)| {
                !matches!(*key, "tagline" | "author" | "company" | "site" | "version")
            }) {
                sections.push(wrapped(
                    &format!("about/provenance/{key}"),
                    &format!("{key}: {value}"),
                    width,
                    SemanticRole::Text,
                    StyleRef::Quiet,
                ));
            }
            sections.push(wrapped(
                "about/font-heading",
                "Font Credits",
                width,
                SemanticRole::Heading,
                StyleRef::Plain,
            ));
            for (index, face) in aterm_render::DISPLAY_FACES.iter().enumerate() {
                sections.push(wrapped(
                    &format!("about/font-credits/{index}"),
                    &format!("{} · {}", face.label, face.face),
                    width,
                    SemanticRole::Text,
                    StyleRef::Quiet,
                ));
            }
        }
        let content = children_height(&sections, gap);
        let limit = (content - room).max(0.0);
        let mut spans = Vec::new();
        let mut y = 0.0;
        for node in &sections {
            collect_spans(node, y, &mut spans);
            y += node_height(node) + gap;
        }
        // Reflow keeps a control the view was showing in sight. An explicit scroll
        // can still leave focus offscreen; an unchanged layout never pulls it back.
        let edge = crate::native_ui::CLIP_EDGE_EPSILON;
        let preserve_focus = view.common.last_focus.as_ref().is_some_and(|focus| {
            let old = view.spans.borrow();
            let Some(before) = old.iter().find(|span| span.key == *focus) else {
                return false;
            };
            let Some(after) = spans.iter().find(|span| span.key == *focus) else {
                return false;
            };
            let visible = before.bottom.min(view.scroll.get() + view.room.get())
                - before.top.max(view.scroll.get());
            (room + edge < view.room.get()
                || (before.top - after.top).abs() > edge
                || (before.bottom - after.bottom).abs() > edge)
                && visible + edge >= crate::native_ui::LIST_PEEK_MIN.min(before.bottom - before.top)
        });
        view.room.set(room);
        view.limit.set(limit);
        view.scroll.set(view.scroll.get().clamp(0.0, limit));
        *view.spans.borrow_mut() = spans;
        if preserve_focus && let Some(focus) = view.common.last_focus.as_ref() {
            view.reveal_listed_control(focus);
        }
        let offset = view.scroll.get();
        let mut top = 0.0;
        let sections = sections
            .into_iter()
            .map(|node| {
                let bottom = top + node_height(&node);
                let hidden = bottom <= offset || top >= offset + room;
                top = bottom + gap;
                if hidden { node.offscreen() } else { node }
            })
            .collect();
        UiTree::new(
            UiNode::new(
                "about/root",
                UiContent::Group(GroupSpec {
                    label: Some("About aterm".to_string()),
                    role: SemanticRole::Application,
                    style: StyleRef::Plain,
                }),
            )
            .layout(Layout::column().padding(Insets::symmetric(outer, pad)))
            .children(vec![
                UiNode::new(
                    "about/content",
                    UiContent::Group(GroupSpec::new("About content")),
                )
                .layout(
                    Layout::column()
                        .height(Length::Fixed(room))
                        .gap(gap)
                        .scrolled(offset)
                        .clipped(),
                )
                .room(room, limit > 0.0)
                .lists_every_row()
                .children(sections),
            ]),
        )
    }

    fn commands(&self, view: &AboutViewState, out: &mut Vec<Command>) {
        for (id, title, enabled) in [
            (
                COPY,
                "About: Copy Build Information",
                view.pending.is_none(),
            ),
            (
                SITE,
                "About: Open Project Site",
                view.pending.is_none() && crate::about::site_url(&self.about).is_some(),
            ),
            (
                DETAILS,
                if view.details {
                    "About: Hide Details"
                } else {
                    "About: Show Details"
                },
                true,
            ),
        ] {
            out.push(Command {
                id: ActionId::new(id),
                title: title.to_string(),
                shortcut: None,
                enabled,
            });
        }
    }

    fn presentation(&self, _view: &AboutViewState) -> AppPresentation {
        AppPresentation {
            title: "About aterm".to_string(),
            icon: AppIcon::About,
            indicators: AppIndicators::default(),
            closable: true,
            tooltip: Some(format!("aterm {}", crate::build_info::version_display())),
        }
    }

    fn prepare_close(&mut self, _request: CloseRequest, _cx: &mut UpdateCx<'_>) -> CloseReadiness {
        CloseReadiness::Ready
    }
}

fn fitted_button_label(full: &str, short: &str, width: f32) -> String {
    let (px, face) =
        crate::native_ui::text_paint_metrics(SemanticRole::Button, StyleRef::Secondary);
    if crate::tray_raster::ui_text_width_for(face, full, px) + 32.0 <= width {
        full.to_string()
    } else {
        short.to_string()
    }
}

fn node_height(node: &UiNode) -> f32 {
    match node.layout.height {
        Length::Fixed(height) => height,
        _ => 0.0,
    }
}

fn children_height(children: &[UiNode], gap: f32) -> f32 {
    children.iter().map(node_height).sum::<f32>() + gap * children.len().saturating_sub(1) as f32
}

fn collect_spans(node: &UiNode, top: f32, spans: &mut Vec<Span>) {
    spans.push(Span {
        key: node.key.clone(),
        top,
        bottom: top + node_height(node),
    });
    let mut child_top = top + node.layout.padding.top;
    for child in &node.children {
        collect_spans(child, child_top, spans);
        if node.layout.flow == Flow::Column {
            child_top += node_height(child) + node.layout.gap;
        }
    }
}

fn wrapped(key: &str, text: &str, width: f32, role: SemanticRole, style: StyleRef) -> UiNode {
    let (px, face) = crate::native_ui::text_paint_metrics(role, style);
    let line_height = (px * 1.6).ceil();
    let mut lines = Vec::new();
    let mut line = String::new();
    for word in text.split_whitespace() {
        let candidate = if line.is_empty() {
            word.to_string()
        } else {
            format!("{line} {word}")
        };
        if crate::tray_raster::ui_text_width_for(face, &candidate, px) <= width {
            line = candidate;
            continue;
        }
        if !line.is_empty() {
            lines.push(std::mem::take(&mut line));
        }
        for glyph in word.graphemes() {
            let candidate = format!("{line}{glyph}");
            if !line.is_empty()
                && crate::tray_raster::ui_text_width_for(face, &candidate, px) > width
            {
                lines.push(std::mem::take(&mut line));
            }
            line.push_str(glyph);
        }
    }
    if !line.is_empty() {
        lines.push(line);
    }
    if lines.is_empty() {
        lines.push(String::new());
    }
    if lines.len() == 1 {
        return UiNode::new(
            key,
            UiContent::Text(TextSpec {
                text: lines.remove(0),
                role,
                style,
            }),
        )
        .layout(Layout::default().height(Length::Fixed(line_height)));
    }
    let height = line_height * lines.len() as f32;
    UiNode::new(
        key,
        UiContent::Group(GroupSpec {
            label: Some(text.to_string()),
            role,
            style: StyleRef::Plain,
        }),
    )
    .layout(Layout::column().height(Length::Fixed(height)))
    .children(
        lines
            .into_iter()
            .enumerate()
            .map(|(index, text)| {
                UiNode::new(
                    format!("{key}/line/{index}"),
                    UiContent::Text(TextSpec { text, role, style }),
                )
                .layout(Layout::default().height(Length::Fixed(line_height)))
                .paint_only()
            })
            .collect(),
    )
}

#[cfg(test)]
mod tests {
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
}
