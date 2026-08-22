// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Closed, capability-shaped runtime for first-party native tab applications.
//!
//! The generic tab host owns `ViewId -> View::Native(instance)` links in
//! [`crate::tab_model`].  This module owns app instances and the presentation state
//! keyed by those view ids; it never owns tabs, windows, PTYs, or renderer handles.

#![allow(
    dead_code,
    reason = "native tab-app migration foundation; host wiring lands in stages"
)]

use std::collections::BTreeMap;
use std::marker::PhantomData;

use aterm_grapheme::GraphemeClusters;

pub(crate) use crate::tab_model::{AppInstanceId, ViewId};

use crate::document_store::DocumentId;
use crate::native_ui::{ActionId, LogicalRect, UiKey, UiTree};

#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub(crate) struct ServiceId(u64);

impl ServiceId {
    pub(crate) const UPDATER: Self = Self(1);
    pub(crate) const CONFIG: Self = Self(2);
    pub(crate) const PACKAGES: Self = Self(3);

    pub(crate) const fn get(self) -> u64 {
        self.0
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub(crate) struct OperationId(u64);

impl OperationId {
    pub(crate) const fn get(self) -> u64 {
        self.0
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub(crate) enum AppKind {
    Settings,
    Markdown,
    Editor,
    Recovery,
}

impl AppKind {
    pub(crate) const fn as_str(self) -> &'static str {
        match self {
            Self::Settings => "settings",
            Self::Markdown => "markdown",
            Self::Editor => "editor",
            Self::Recovery => "recovery",
        }
    }

    /// Human-facing name, exactly as the apps title themselves (their ext
    /// tooltips author `"Markdown · {uri}"`); the install-time presentation
    /// must match so a tab's tooltip never changes case on first refresh.
    pub(crate) const fn display_name(self) -> &'static str {
        match self {
            Self::Settings => "Settings",
            Self::Markdown => "Markdown",
            Self::Editor => "Editor",
            Self::Recovery => "Recovery",
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum AppIcon {
    Settings,
    Markdown,
    Editor,
    Recovery,
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub(crate) struct AppIndicators {
    pub(crate) dirty: bool,
    pub(crate) busy: bool,
    pub(crate) attention: bool,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct AppDescriptor {
    pub(crate) kind: AppKind,
    pub(crate) name: &'static str,
    pub(crate) icon: AppIcon,
    pub(crate) singleton: bool,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct AppPresentation {
    pub(crate) title: String,
    pub(crate) icon: AppIcon,
    pub(crate) indicators: AppIndicators,
    pub(crate) closable: bool,
    pub(crate) tooltip: Option<String>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct CommonViewState {
    pub(crate) last_focus: Option<UiKey>,
    pub(crate) hovered: Option<UiKey>,
    pub(crate) pressed: Option<UiKey>,
    pub(crate) focus_visible: bool,
    pub(crate) presentation_revision: u64,
}

impl Default for CommonViewState {
    fn default() -> Self {
        Self {
            last_focus: None,
            hovered: None,
            pressed: None,
            focus_visible: false,
            presentation_revision: 1,
        }
    }
}

pub(crate) enum AppViewState {
    Settings(Box<crate::native_settings::SettingsViewState>),
    Markdown(MarkdownViewState),
    Editor(Box<EditorViewState>),
    Recovery(RecoveryViewState),
}

impl AppViewState {
    pub(crate) const fn kind(&self) -> AppKind {
        match self {
            Self::Settings(_) => AppKind::Settings,
            Self::Markdown(_) => AppKind::Markdown,
            Self::Editor(_) => AppKind::Editor,
            Self::Recovery(_) => AppKind::Recovery,
        }
    }

    pub(crate) fn common(&self) -> &CommonViewState {
        match self {
            Self::Settings(view) => &view.common,
            Self::Markdown(view) => &view.common,
            Self::Editor(view) => &view.common,
            Self::Recovery(view) => &view.common,
        }
    }

    pub(crate) fn common_mut(&mut self) -> &mut CommonViewState {
        match self {
            Self::Settings(view) => &mut view.common,
            Self::Markdown(view) => &mut view.common,
            Self::Editor(view) => &mut view.common,
            Self::Recovery(view) => &mut view.common,
        }
    }
}

/// Markdown presentation is per-view: history, scroll, and selection never leak
/// into another view of the same canonical document.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub(crate) enum MarkdownViewMode {
    #[default]
    Preview,
    Source,
    Split,
}

impl MarkdownViewMode {
    pub(crate) const fn label(self) -> &'static str {
        match self {
            Self::Preview => "Preview",
            Self::Source => "Source",
            Self::Split => "Split",
        }
    }
}

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub(crate) struct MarkdownViewState {
    pub(crate) common: CommonViewState,
    pub(crate) history: crate::native_markdown::MarkdownHistory,
    pub(crate) source_anchor: usize,
    /// Exact wrapped reader row within the block containing `source_anchor`.
    /// This is semantic view state (and restore/history state), never a pixel
    /// offset inferred by paint.
    pub(crate) visual_row: usize,
    pub(crate) selection: Option<std::ops::Range<usize>>,
    /// One document can be previewed, inspected as exact source, and split in
    /// different views without leaking presentation state between them.
    pub(crate) mode: MarkdownViewMode,
    /// Short, view-local feedback for clipboard and external-link operations.
    /// It is deliberately not document state and never leaks across split views.
    pub(crate) notice: Option<String>,
}

/// Editor presentation is per-view. Canonical bytes/edit sequence remain in the
/// process-wide `DocumentStore`.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub(crate) struct EditorViewState {
    pub(crate) common: CommonViewState,
    /// Canonical editor interaction state for this view. Document bytes remain
    /// exclusively in `DocumentStore`; this carries only selections, viewport,
    /// chord/minibuffer state, and the store-issued `DocumentViewId`.
    pub(crate) buffer: Option<crate::native_editor::EditorBufferView>,
    pub(crate) preedit: String,
    pub(crate) status: Option<String>,
    /// Persistent host-watcher warning for the canonical config/theme inputs.
    /// Kept separate from command feedback and diagnostics so either can change
    /// without erasing the other; a typed recovery edge alone clears it.
    pub(crate) config_watch_status: Option<String>,
    /// Selected contextual aterm.toml completion. The visible window is
    /// derived from this index and the exact responsive row capacity.
    pub(crate) config_completion_selected: usize,
    /// Exact assist context whose completion row owns keyboard navigation.
    /// Merely showing suggestions must never steal Enter or the arrow keys
    /// from ordinary editing.
    pub(crate) config_completion_interaction:
        Option<crate::native_config_language::ConfigCompletionContext>,
    /// Escape dismisses contextual assistance only for the immutable document
    /// sequence + caret that produced it. Any edit or caret move naturally
    /// creates a new context and may offer assistance again.
    pub(crate) config_completion_dismissed:
        Option<crate::native_config_language::ConfigCompletionContext>,
    /// Retained Manual diagnostics are navigable without moving the document
    /// caret. The semantic modeline announces the complete selected message.
    pub(crate) config_diagnostic_selected: usize,
}

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub(crate) struct RecoveryViewState {
    pub(crate) common: CommonViewState,
    pub(crate) page: usize,
    pub(crate) notice: Option<String>,
    pub(crate) pending: Option<RecoveryPending>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum ConfigCompletionNavigation {
    Previous,
    Next,
    Page(usize),
}

#[must_use]
pub(crate) fn config_completion_selection_transition(
    current: usize,
    candidates: usize,
    navigation: ConfigCompletionNavigation,
) -> usize {
    let last = candidates.saturating_sub(1);
    if candidates == 0 {
        return 0;
    }
    match navigation {
        ConfigCompletionNavigation::Previous => current.min(last).saturating_sub(1),
        ConfigCompletionNavigation::Next => current.min(last).saturating_add(1).min(last),
        ConfigCompletionNavigation::Page(index) => index.min(last),
    }
}

#[must_use]
pub(crate) fn config_completion_window(
    selected: usize,
    candidates: usize,
    capacity: usize,
) -> std::ops::Range<usize> {
    if candidates == 0 || capacity == 0 {
        return 0..0;
    }
    let selected = selected.min(candidates - 1);
    let capacity = capacity.min(candidates);
    let start = (selected / capacity) * capacity;
    start..start.saturating_add(capacity).min(candidates)
}

#[must_use]
pub(crate) fn config_diagnostic_selection_transition(
    current: usize,
    diagnostics: usize,
    previous: bool,
) -> usize {
    match diagnostics {
        0 | 1 => 0,
        _ if previous => (current.min(diagnostics - 1) + diagnostics - 1) % diagnostics,
        _ => (current.min(diagnostics - 1) + 1) % diagnostics,
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum RecoveryPendingAction {
    Retry,
    OpenOriginal,
    CopyDiagnostics,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct RecoveryPending {
    pub(crate) operation: OperationId,
    pub(crate) action: RecoveryPendingAction,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) enum RecoveryCapability {
    Settings {
        route: String,
    },
    Document {
        kind: AppKind,
        uri: String,
        /// Preserve the privileged config-editor reducer on Retry. Treating
        /// this document as an ordinary Editor would bypass schema diagnostics
        /// and the versioned config save lane after a failed restore.
        config_editor: bool,
    },
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) enum RecoveryRequest {
    Retry(RecoveryCapability),
    OpenOriginal { uri: String },
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) enum RecoveryOutcome {
    Opened { message: String },
    Denied { message: String },
    Failed { message: String },
}

/// Read-only, capability-free presentation for an unavailable or corrupt restored leaf.
/// The original bounded metadata remains copyable; it is never interpreted as an action,
/// URI, path, or shell input.
pub(crate) struct RecoveryApp {
    pub(crate) restore_tag: String,
    pub(crate) reason: String,
    pub(crate) metadata: String,
    /// Structured authority retained from a validated restore descriptor. The
    /// copyable metadata above is never parsed back into a URI or action.
    pub(crate) capability: Option<RecoveryCapability>,
}

pub(crate) enum NativeApp {
    Settings(crate::native_settings::SettingsApp),
    Markdown(MarkdownApp),
    Editor(EditorApp),
    Recovery(RecoveryApp),
}

type UpdateCallback = for<'a> fn(
    &mut NativeApp,
    &mut AppViewState,
    AppEvent,
    &mut UpdateCx<'a>,
) -> Result<EventResult, RuntimeError>;
type ViewCallback =
    for<'a> fn(&NativeApp, &AppViewState, &ViewCx<'a>) -> Result<UiTree, RuntimeError>;
type CommandsCallback =
    fn(&NativeApp, &AppViewState, &mut Vec<Command>) -> Result<(), RuntimeError>;
type PresentationCallback = fn(&NativeApp, &AppViewState) -> Result<AppPresentation, RuntimeError>;
type CloseCallback = for<'a> fn(&mut NativeApp, CloseRequest, &mut UpdateCx<'a>) -> CloseReadiness;

/// Required behavior table for every first-party app variant. The single
/// no-wildcard [`NativeApp::vtable`] match makes an added enum variant fail to
/// compile until all runtime surfaces are supplied.
pub(crate) struct AppVTable {
    pub(crate) kind: AppKind,
    pub(crate) restore_tag: &'static str,
    descriptor: fn(&NativeApp) -> AppDescriptor,
    update: UpdateCallback,
    view: ViewCallback,
    commands: CommandsCallback,
    presentation: PresentationCallback,
    prepare_close: CloseCallback,
}

impl NativeApp {
    pub(crate) const fn vtable(&self) -> &'static AppVTable {
        match self {
            Self::Settings(_) => &settings_vtable::VTABLE,
            Self::Markdown(_) => &markdown_vtable::VTABLE,
            Self::Editor(_) => &editor_vtable::VTABLE,
            Self::Recovery(_) => &recovery_vtable::VTABLE,
        }
    }

    pub(crate) const fn kind(&self) -> AppKind {
        self.vtable().kind
    }

    pub(crate) const fn document_id(&self) -> Option<DocumentId> {
        match self {
            Self::Settings(_) => None,
            Self::Markdown(app) => Some(app.document),
            Self::Editor(app) => Some(app.document),
            Self::Recovery(_) => None,
        }
    }

    pub(crate) fn descriptor(&self) -> AppDescriptor {
        (self.vtable().descriptor)(self)
    }

    fn update(
        &mut self,
        view: &mut AppViewState,
        event: AppEvent,
        cx: &mut UpdateCx<'_>,
    ) -> Result<EventResult, RuntimeError> {
        (self.vtable().update)(self, view, event, cx)
    }

    fn view(&self, view: &AppViewState, cx: &ViewCx<'_>) -> Result<UiTree, RuntimeError> {
        (self.vtable().view)(self, view, cx)
    }

    fn commands(&self, view: &AppViewState, out: &mut Vec<Command>) -> Result<(), RuntimeError> {
        (self.vtable().commands)(self, view, out)
    }

    fn presentation(&self, view: &AppViewState) -> Result<AppPresentation, RuntimeError> {
        (self.vtable().presentation)(self, view)
    }

    fn prepare_close(&mut self, request: CloseRequest, cx: &mut UpdateCx<'_>) -> CloseReadiness {
        (self.vtable().prepare_close)(self, request, cx)
    }
}

pub(crate) trait NativeAppModel {
    type ViewState;

    fn descriptor(&self) -> AppDescriptor;
    fn update(
        &mut self,
        view: &mut Self::ViewState,
        event: AppEvent,
        cx: &mut UpdateCx<'_>,
    ) -> EventResult;
    fn view(&self, view: &Self::ViewState, cx: &ViewCx<'_>) -> UiTree;
    fn commands(&self, view: &Self::ViewState, out: &mut Vec<Command>);
    fn presentation(&self, view: &Self::ViewState) -> AppPresentation;
    fn prepare_close(&mut self, request: CloseRequest, cx: &mut UpdateCx<'_>) -> CloseReadiness;
}

macro_rules! define_app_vtable {
    (
        $module:ident,
        $variant:ident,
        $state:ident,
        $kind:expr,
        $restore_tag:literal
    ) => {
        mod $module {
            use super::*;

            fn descriptor(app: &NativeApp) -> AppDescriptor {
                let NativeApp::$variant(app) = app else {
                    unreachable!("vtable selected for the wrong native app variant")
                };
                app.descriptor()
            }

            fn update(
                app: &mut NativeApp,
                view: &mut AppViewState,
                event: AppEvent,
                cx: &mut UpdateCx<'_>,
            ) -> Result<EventResult, RuntimeError> {
                match (app, view) {
                    (NativeApp::$variant(app), AppViewState::$state(view)) => {
                        Ok(app.update(view, event, cx))
                    }
                    (app, view) => Err(RuntimeError::KindMismatch {
                        app: app.kind(),
                        view: view.kind(),
                    }),
                }
            }

            fn view(
                app: &NativeApp,
                view: &AppViewState,
                cx: &ViewCx<'_>,
            ) -> Result<UiTree, RuntimeError> {
                match (app, view) {
                    (NativeApp::$variant(app), AppViewState::$state(view)) => {
                        Ok(app.view(view, cx))
                    }
                    (app, view) => Err(RuntimeError::KindMismatch {
                        app: app.kind(),
                        view: view.kind(),
                    }),
                }
            }

            fn commands(
                app: &NativeApp,
                view: &AppViewState,
                out: &mut Vec<Command>,
            ) -> Result<(), RuntimeError> {
                match (app, view) {
                    (NativeApp::$variant(app), AppViewState::$state(view)) => {
                        app.commands(view, out);
                        Ok(())
                    }
                    (app, view) => Err(RuntimeError::KindMismatch {
                        app: app.kind(),
                        view: view.kind(),
                    }),
                }
            }

            fn presentation(
                app: &NativeApp,
                view: &AppViewState,
            ) -> Result<AppPresentation, RuntimeError> {
                match (app, view) {
                    (NativeApp::$variant(app), AppViewState::$state(view)) => {
                        Ok(app.presentation(view))
                    }
                    (app, view) => Err(RuntimeError::KindMismatch {
                        app: app.kind(),
                        view: view.kind(),
                    }),
                }
            }

            fn prepare_close(
                app: &mut NativeApp,
                request: CloseRequest,
                cx: &mut UpdateCx<'_>,
            ) -> CloseReadiness {
                let NativeApp::$variant(app) = app else {
                    unreachable!("vtable selected for the wrong native app variant")
                };
                app.prepare_close(request, cx)
            }

            pub(super) static VTABLE: AppVTable = AppVTable {
                kind: $kind,
                restore_tag: $restore_tag,
                descriptor,
                update,
                view,
                commands,
                presentation,
                prepare_close,
            };
        }
    };
}

define_app_vtable!(
    settings_vtable,
    Settings,
    Settings,
    AppKind::Settings,
    "settings"
);
define_app_vtable!(
    markdown_vtable,
    Markdown,
    Markdown,
    AppKind::Markdown,
    "markdown"
);
define_app_vtable!(editor_vtable, Editor, Editor, AppKind::Editor, "editor");
define_app_vtable!(
    recovery_vtable,
    Recovery,
    Recovery,
    AppKind::Recovery,
    "recovery"
);

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum RecoveryLineStyle {
    Danger,
    Primary,
    Secondary,
    Code,
}

struct RecoveryDisplayLine {
    text: String,
    style: RecoveryLineStyle,
}

struct RecoveryPageProjection {
    lines: Vec<RecoveryDisplayLine>,
    page: usize,
    pages: usize,
    line_height: f32,
    header_height: f32,
    action_height: f32,
    notice_height: f32,
    padding: f32,
    gap: f32,
    stacked_actions: bool,
}

fn recovery_page_projection(
    app: &RecoveryApp,
    view: &RecoveryViewState,
    viewport: LogicalRect,
) -> RecoveryPageProjection {
    recovery_page_projection_at_scale(app, view, viewport, crate::native_appearance::text_scale())
}

fn recovery_page_projection_at_scale(
    app: &RecoveryApp,
    view: &RecoveryViewState,
    viewport: LogicalRect,
    scale: f32,
) -> RecoveryPageProjection {
    let scale = if scale.is_finite() && scale > 0.0 {
        scale.clamp(0.85, 2.0)
    } else {
        1.0
    };
    let chrome_scale = scale.min(1.35);
    let short = viewport.height < 420.0;
    let padding = if short {
        6.0 * chrome_scale
    } else if viewport.width < 560.0 {
        10.0 * chrome_scale
    } else {
        20.0 * chrome_scale
    };
    let gap = 6.0 * chrome_scale;
    let header_height = if short {
        (36.0 * scale).min(56.0)
    } else {
        (48.0 * scale).min(72.0)
    };
    let notice_height = if short {
        (18.0 * scale).min(30.0)
    } else {
        (22.0 * scale).min(36.0)
    };
    let button_height = if short {
        (32.0 * scale).min(44.0)
    } else {
        (36.0 * scale).min(48.0)
    };
    let stacked_actions = viewport.width < 620.0 * scale.min(1.4);
    let action_height = if stacked_actions {
        button_height * 2.0 + gap
    } else {
        button_height
    };
    let line_height = 20.0 * scale;
    let body_height = (viewport.height
        - padding * 2.0
        - header_height
        - notice_height
        - action_height
        - gap * 3.0)
        .max(line_height);
    let line_capacity = ((body_height / line_height).floor() as usize).clamp(1, 64);
    let text_width = (viewport.width - padding * 2.0).max(1.0);

    let mut all = Vec::new();
    recovery_push_wrapped(
        &mut all,
        &format!("Problem: {}", app.reason),
        RecoveryLineStyle::Danger,
        text_width,
        scale,
    );
    recovery_push_wrapped(
        &mut all,
        &format!("Restore type: {}", app.restore_tag),
        RecoveryLineStyle::Secondary,
        text_width,
        scale,
    );
    let capability = match &app.capability {
        Some(RecoveryCapability::Settings { .. }) => {
            "A validated Settings route is retained; Retry is available."
        }
        Some(RecoveryCapability::Document { .. }) => {
            "A validated local-document capability is retained; Retry and Open Original are available."
        }
        None => {
            "Retry unavailable: this record contains diagnostics only, not executable restore authority."
        }
    };
    recovery_push_wrapped(
        &mut all,
        capability,
        RecoveryLineStyle::Primary,
        text_width,
        scale,
    );
    all.push(RecoveryDisplayLine {
        text: "Diagnostics".to_string(),
        style: RecoveryLineStyle::Primary,
    });
    recovery_push_wrapped(
        &mut all,
        if app.metadata.is_empty() {
            "No additional metadata was retained."
        } else {
            &app.metadata
        },
        RecoveryLineStyle::Code,
        text_width,
        scale,
    );
    if all.is_empty() {
        all.push(RecoveryDisplayLine {
            text: "No recovery details were retained.".to_string(),
            style: RecoveryLineStyle::Secondary,
        });
    }
    let pages = all.len().div_ceil(line_capacity).max(1);
    let page = view.page.min(pages - 1);
    let start = page * line_capacity;
    let lines = all.into_iter().skip(start).take(line_capacity).collect();
    RecoveryPageProjection {
        lines,
        page,
        pages,
        line_height,
        header_height,
        action_height,
        notice_height,
        padding,
        gap,
        stacked_actions,
    }
}

fn recovery_push_wrapped(
    output: &mut Vec<RecoveryDisplayLine>,
    text: &str,
    style: RecoveryLineStyle,
    available_width: f32,
    scale: f32,
) {
    use aterm_grapheme::GraphemeClusters;

    let (face, px) = recovery_line_typography(style, scale);
    for physical in text.lines().chain(text.is_empty().then_some("")) {
        if physical.is_empty() {
            output.push(RecoveryDisplayLine {
                text: " ".to_string(),
                style,
            });
            continue;
        }
        // Never bisect a user-perceived character. This matters for copied
        // crash reports containing combining marks, flags, or ZWJ emoji.
        let graphemes = physical.graphemes().collect::<Vec<_>>();
        let mut start = 0;
        while start < graphemes.len() {
            let hard_end = recovery_fitting_end(&graphemes, start, available_width, face, px);
            let end = if hard_end < graphemes.len() {
                graphemes[start..hard_end]
                    .iter()
                    .rposition(|grapheme| grapheme.chars().all(char::is_whitespace))
                    .map(|relative| start + relative + 1)
                    .filter(|end| *end > start)
                    .unwrap_or(hard_end)
            } else {
                hard_end
            };
            let line = graphemes[start..end].concat().trim().to_string();
            output.push(RecoveryDisplayLine {
                text: if line.is_empty() {
                    " ".to_string()
                } else {
                    line
                },
                style,
            });
            start = end;
            while start < graphemes.len() && graphemes[start].chars().all(char::is_whitespace) {
                start += 1;
            }
        }
    }
}

fn recovery_line_typography(
    style: RecoveryLineStyle,
    scale: f32,
) -> (crate::widget::TextFace, f32) {
    use crate::widget::TextFace;
    match style {
        RecoveryLineStyle::Danger => (TextFace::Ui, 11.0 * scale),
        RecoveryLineStyle::Code => (TextFace::Mono, 13.0 * scale),
        RecoveryLineStyle::Primary | RecoveryLineStyle::Secondary => (TextFace::Ui, 13.0 * scale),
    }
}

fn recovery_fitting_end(
    graphemes: &[&str],
    start: usize,
    available_width: f32,
    face: crate::widget::TextFace,
    px: f32,
) -> usize {
    if start >= graphemes.len() {
        return start;
    }
    let fits = |end: usize| {
        let text = graphemes[start..end].concat();
        crate::tray_raster::ui_text_width_for(face, &text, px) <= available_width + 0.25
    };
    let mut low = start + 1;
    let mut high = graphemes.len();
    if fits(high) {
        return high;
    }
    while low < high {
        let middle = low + (high - low).div_ceil(2);
        if fits(middle) {
            low = middle;
        } else {
            high = middle - 1;
        }
    }
    low.max(start + 1)
}

impl NativeAppModel for RecoveryApp {
    type ViewState = RecoveryViewState;

    fn descriptor(&self) -> AppDescriptor {
        AppDescriptor {
            kind: AppKind::Recovery,
            name: "Recovery",
            icon: AppIcon::Recovery,
            singleton: false,
        }
    }

    fn update(
        &mut self,
        view: &mut Self::ViewState,
        event: AppEvent,
        cx: &mut UpdateCx<'_>,
    ) -> EventResult {
        match event {
            AppEvent::FocusChanged(focus) => {
                view.common.last_focus = focus;
                EventResult::Handled
            }
            AppEvent::Action(invocation)
                if invocation.id.as_str().starts_with("recovery/page/") =>
            {
                if let Some(page) = invocation
                    .id
                    .as_str()
                    .strip_prefix("recovery/page/")
                    .and_then(|value| value.parse::<usize>().ok())
                {
                    view.page = page;
                    view.common.presentation_revision =
                        view.common.presentation_revision.saturating_add(1);
                }
                EventResult::Handled
            }
            AppEvent::Action(invocation) if invocation.id.as_str() == "recovery/copy" => {
                if view.pending.is_none() {
                    let operation = cx.clipboard(ClipboardRequest::CopyText {
                        text: self.diagnostics_text(),
                        sensitive: false,
                    });
                    view.pending = Some(RecoveryPending {
                        operation,
                        action: RecoveryPendingAction::CopyDiagnostics,
                    });
                    view.notice = Some("Copying diagnostics…".to_string());
                    view.common.presentation_revision =
                        view.common.presentation_revision.saturating_add(1);
                }
                EventResult::Handled
            }
            AppEvent::Action(invocation) if invocation.id.as_str() == "recovery/retry" => {
                if view.pending.is_none()
                    && let Some(capability) = self.capability.clone()
                {
                    let operation = cx.recovery(RecoveryRequest::Retry(capability));
                    view.pending = Some(RecoveryPending {
                        operation,
                        action: RecoveryPendingAction::Retry,
                    });
                    view.notice = Some("Retrying restore…".to_string());
                    view.common.presentation_revision =
                        view.common.presentation_revision.saturating_add(1);
                }
                EventResult::Handled
            }
            AppEvent::Action(invocation) if invocation.id.as_str() == "recovery/open-original" => {
                if view.pending.is_none()
                    && let Some(RecoveryCapability::Document { uri, .. }) = &self.capability
                {
                    let operation = cx.recovery(RecoveryRequest::OpenOriginal { uri: uri.clone() });
                    view.pending = Some(RecoveryPending {
                        operation,
                        action: RecoveryPendingAction::OpenOriginal,
                    });
                    view.notice = Some("Opening original…".to_string());
                    view.common.presentation_revision =
                        view.common.presentation_revision.saturating_add(1);
                }
                EventResult::Handled
            }
            AppEvent::ClipboardFinished { operation, outcome }
                if view.pending.is_some_and(|pending| {
                    pending.operation == operation
                        && pending.action == RecoveryPendingAction::CopyDiagnostics
                }) =>
            {
                view.pending = None;
                view.notice = Some(match outcome {
                    ClipboardOutcome::Copied => "Diagnostics copied".to_string(),
                    ClipboardOutcome::Denied { message } | ClipboardOutcome::Failed { message } => {
                        message
                    }
                });
                view.common.presentation_revision =
                    view.common.presentation_revision.saturating_add(1);
                EventResult::Handled
            }
            AppEvent::RecoveryFinished { operation, outcome }
                if view.pending.is_some_and(|pending| {
                    pending.operation == operation
                        && matches!(
                            pending.action,
                            RecoveryPendingAction::Retry | RecoveryPendingAction::OpenOriginal
                        )
                }) =>
            {
                view.pending = None;
                view.notice = Some(match outcome {
                    RecoveryOutcome::Opened { message }
                    | RecoveryOutcome::Denied { message }
                    | RecoveryOutcome::Failed { message } => message,
                });
                view.common.presentation_revision =
                    view.common.presentation_revision.saturating_add(1);
                EventResult::Handled
            }
            AppEvent::TextInput(TextInputEvent::Cancel) => {
                if view.notice.take().is_some() {
                    view.common.presentation_revision =
                        view.common.presentation_revision.saturating_add(1);
                }
                EventResult::Handled
            }
            _ => EventResult::Bubble,
        }
    }

    fn view(&self, view: &Self::ViewState, cx: &ViewCx<'_>) -> UiTree {
        use crate::native_ui::{
            ButtonSpec, Control, ControlState, GroupSpec, Insets, Layout, Length, SemanticRole,
            StyleRef, TextSpec, UiContent, UiNode,
        };

        let projection = recovery_page_projection(self, view, cx.viewport);
        let text_scale = crate::native_appearance::text_scale();
        let compact_action_labels = cx.viewport.width < 480.0 * text_scale;
        let pending = view.pending.is_some();
        let pending_action = view.pending.map(|pending| pending.action);
        let retry_enabled = self.capability.is_some() && !pending;
        let original_enabled =
            matches!(self.capability, Some(RecoveryCapability::Document { .. })) && !pending;
        let button = |key: &'static str,
                      visual: &'static str,
                      label: String,
                      action: String,
                      enabled: bool| {
            UiNode::new(
                key,
                UiContent::Button(
                    Control::new(
                        ButtonSpec::new(label).visual_label(visual),
                        ActionId::new(action),
                    )
                    .state(ControlState {
                        enabled,
                        busy: matches!(
                            (key, pending_action),
                            ("recovery/retry", Some(RecoveryPendingAction::Retry))
                                | (
                                    "recovery/open-original",
                                    Some(RecoveryPendingAction::OpenOriginal)
                                )
                                | (
                                    "recovery/copy",
                                    Some(RecoveryPendingAction::CopyDiagnostics)
                                )
                        ),
                        ..ControlState::default()
                    })
                    // Retry is the page's DEFAULT while it is actionable: the
                    // highlighted Primary a bare Return activates (see
                    // `CompiledUi::default_action`). With no retained
                    // capability nothing is highlighted and Return stays inert.
                    .style(if key == "recovery/retry" && enabled {
                        StyleRef::Primary
                    } else {
                        StyleRef::Secondary
                    }),
                ),
            )
            .layout(Layout::default().width(Length::Fill).height(Length::Fill))
        };

        let retry = || {
            button(
                "recovery/retry",
                "Retry",
                if self.capability.is_some() {
                    "Retry the validated restore request".to_string()
                } else {
                    "Retry unavailable: no retained executable restore capability".to_string()
                },
                "recovery/retry".to_string(),
                retry_enabled,
            )
        };
        let original = || {
            button(
                "recovery/open-original",
                if compact_action_labels {
                    "Open"
                } else {
                    "Open Original"
                },
                if matches!(self.capability, Some(RecoveryCapability::Document { .. })) {
                    "Open the retained local document in the native editor".to_string()
                } else {
                    "Open Original unavailable: no validated local-document capability".to_string()
                },
                "recovery/open-original".to_string(),
                original_enabled,
            )
        };
        let copy = || {
            button(
                "recovery/copy",
                if compact_action_labels {
                    "Copy"
                } else {
                    "Copy Diagnostics"
                },
                "Copy bounded recovery diagnostics".to_string(),
                "recovery/copy".to_string(),
                !pending,
            )
        };
        let previous = || {
            button(
                "recovery/previous",
                if compact_action_labels {
                    "Prev"
                } else {
                    "Previous"
                },
                "Previous diagnostics page".to_string(),
                format!("recovery/page/{}", projection.page.saturating_sub(1)),
                projection.page > 0,
            )
        };
        let next = || {
            button(
                "recovery/next",
                "Next",
                "Next diagnostics page".to_string(),
                format!(
                    "recovery/page/{}",
                    projection.page.saturating_add(1).min(projection.pages - 1)
                ),
                projection.page + 1 < projection.pages,
            )
        };
        let body = projection
            .lines
            .iter()
            .enumerate()
            .map(|(index, line)| {
                let (role, style) = match line.style {
                    RecoveryLineStyle::Danger => (SemanticRole::Status, StyleRef::Danger),
                    RecoveryLineStyle::Primary => (SemanticRole::Text, StyleRef::Primary),
                    RecoveryLineStyle::Secondary => (SemanticRole::Text, StyleRef::Secondary),
                    RecoveryLineStyle::Code => (SemanticRole::Text, StyleRef::Code),
                };
                UiNode::new(
                    format!("recovery/line/{}/{}", projection.page, index),
                    UiContent::Text(TextSpec {
                        text: line.text.clone(),
                        role,
                        style,
                    }),
                )
                .layout(Layout::default().height(Length::Fixed(projection.line_height)))
            })
            .collect::<Vec<_>>();
        let actions = if projection.stacked_actions {
            UiNode::new(
                "recovery/actions",
                UiContent::Group(GroupSpec::new("Recovery actions")),
            )
            .layout(
                Layout::column()
                    .gap(projection.gap)
                    .height(Length::Fixed(projection.action_height)),
            )
            .children(vec![
                UiNode::new(
                    "recovery/action-primary",
                    UiContent::Group(GroupSpec::unlabeled(SemanticRole::Group)),
                )
                .layout(Layout::row().gap(projection.gap).height(Length::Fill))
                .children(vec![retry(), original()]),
                UiNode::new(
                    "recovery/action-navigation",
                    UiContent::Group(GroupSpec::unlabeled(SemanticRole::Group)),
                )
                .layout(Layout::row().gap(projection.gap).height(Length::Fill))
                .children(vec![copy(), previous(), next()]),
            ])
        } else {
            UiNode::new(
                "recovery/actions",
                UiContent::Group(GroupSpec::new("Recovery actions")),
            )
            .layout(
                Layout::row()
                    .gap(projection.gap)
                    .height(Length::Fixed(projection.action_height)),
            )
            .children(vec![retry(), original(), copy(), previous(), next()])
        };

        UiTree::new(
            UiNode::new(
                "recovery/app",
                UiContent::Group(GroupSpec::unlabeled(SemanticRole::Application)),
            )
            .layout(
                Layout::column()
                    .padding(Insets::all(projection.padding))
                    .gap(projection.gap)
                    .clipped(),
            )
            .children(vec![
                UiNode::new(
                    "recovery/title",
                    UiContent::Text(TextSpec {
                        text: "This view needs recovery".to_string(),
                        role: SemanticRole::Heading,
                        style: StyleRef::Hero,
                    }),
                )
                .layout(Layout::default().height(Length::Fixed(projection.header_height))),
                UiNode::new(
                    "recovery/body",
                    UiContent::Group(GroupSpec::new(format!(
                        "Recovery details page {} of {}",
                        projection.page + 1,
                        projection.pages
                    ))),
                )
                .layout(Layout::column().height(Length::Fill).clipped())
                .children(body),
                UiNode::new(
                    "recovery/status",
                    UiContent::Text(TextSpec {
                        text: view.notice.clone().unwrap_or_else(|| {
                            format!("Page {} of {}", projection.page + 1, projection.pages)
                        }),
                        role: SemanticRole::Status,
                        style: if view.notice.is_some() {
                            StyleRef::Primary
                        } else {
                            StyleRef::Quiet
                        },
                    }),
                )
                .layout(Layout::default().height(Length::Fixed(projection.notice_height))),
                actions,
            ]),
        )
    }

    fn commands(&self, view: &Self::ViewState, out: &mut Vec<Command>) {
        let idle = view.pending.is_none();
        out.extend([
            Command {
                id: ActionId::new("recovery/retry"),
                title: if self.capability.is_some() {
                    "Retry Restore".to_string()
                } else {
                    "Retry Restore — unavailable without a retained capability".to_string()
                },
                shortcut: None,
                enabled: idle && self.capability.is_some(),
            },
            Command {
                id: ActionId::new("recovery/open-original"),
                title: if matches!(self.capability, Some(RecoveryCapability::Document { .. })) {
                    "Open Original in Editor".to_string()
                } else {
                    "Open Original — unavailable for this recovery record".to_string()
                },
                shortcut: None,
                enabled: idle
                    && matches!(self.capability, Some(RecoveryCapability::Document { .. })),
            },
            Command {
                id: ActionId::new("recovery/copy"),
                title: "Copy Diagnostics".to_string(),
                shortcut: None,
                enabled: idle,
            },
        ]);
    }

    fn presentation(&self, view: &Self::ViewState) -> AppPresentation {
        AppPresentation {
            title: "Recovery".to_string(),
            icon: AppIcon::Recovery,
            indicators: AppIndicators {
                attention: true,
                busy: view.pending.is_some(),
                ..AppIndicators::default()
            },
            closable: true,
            tooltip: Some(format!("{} · {}", self.restore_tag, self.reason)),
        }
    }

    fn prepare_close(&mut self, _request: CloseRequest, _cx: &mut UpdateCx<'_>) -> CloseReadiness {
        CloseReadiness::Ready
    }
}

impl RecoveryApp {
    fn diagnostics_text(&self) -> String {
        let capability = match &self.capability {
            Some(RecoveryCapability::Settings { route }) => format!("settings-route={route}"),
            Some(RecoveryCapability::Document {
                kind,
                uri,
                config_editor,
            }) => {
                format!(
                    "document-kind={}\noriginal={uri}\nconfig-editor={config_editor}",
                    kind.as_str()
                )
            }
            None => "capability=none".to_string(),
        };
        format!(
            "aterm Recovery\nrestore-type={}\nreason={}\n{}\nmetadata:\n{}",
            self.restore_tag,
            self.reason,
            capability,
            if self.metadata.is_empty() {
                "(none)"
            } else {
                &self.metadata
            }
        )
    }
}

#[cfg(test)]
mod recovery_tests {
    use super::*;
    use aterm_spec::derive::{Model, native_recovery_interaction_model};
    use aterm_spec::interp::{State, admits};

    fn recovery(capability: Option<RecoveryCapability>) -> RecoveryApp {
        let mut metadata = (0..240)
            .map(|line| format!("diagnostic-{line:03}=bounded detail\n"))
            .collect::<String>();
        metadata.push_str("unicode=e\u{301} · 👨‍👩‍👧‍👦 · 日本語 · مرحبًا\n");
        metadata.push_str("unbroken=");
        metadata.push_str(&"opaque-token-🦀".repeat(240));
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

    fn assert_recovery_transition(
        model: &Model,
        before: &State,
        after: &State,
        action: &'static str,
    ) {
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
            let first = recovery_page_projection_at_scale(
                &app,
                &RecoveryViewState::default(),
                viewport,
                scale,
            );
            assert!(!first.lines.is_empty());
            assert!(first.pages > 1);
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
                    displayed.push_str(&line.text);
                }
            }
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
                        config_revision: 1,
                        update_revision: 1,
                        animation_phase_ms: 0,
                        motion: ViewMotionCx::default(),
                        terminal_font_px: 13.0,
                        terminal_theme: aterm_render::Theme::default(),
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

    #[test]
    fn recovery_actions_emit_typed_capabilities_and_reduce_current_completion() {
        let app = recovery(Some(RecoveryCapability::Document {
            kind: AppKind::Markdown,
            uri: "file:///tmp/Guide.md".to_string(),
            config_editor: false,
        }));
        let mut runtime = NativeRuntime::new();
        let instance = runtime.insert_instance(NativeApp::Recovery(app)).unwrap();
        let view = ViewId::from_stored(44);
        runtime
            .attach_view(
                view,
                instance,
                AppViewState::Recovery(RecoveryViewState::default()),
            )
            .unwrap();

        let retry = runtime
            .dispatch(
                instance,
                view,
                AppEvent::Action(ActionInvocation {
                    id: ActionId::new("recovery/retry"),
                    value: None,
                }),
            )
            .unwrap();
        let (operation, request) = match retry.effects.as_slice() {
            [AppEffect::Recovery { request, reply }] => (reply.operation, request.clone()),
            effects => panic!("expected one typed recovery effect, got {effects:?}"),
        };
        assert!(matches!(
            request,
            RecoveryRequest::Retry(RecoveryCapability::Document {
                kind: AppKind::Markdown,
                ref uri,
                config_editor: false,
            }) if uri == "file:///tmp/Guide.md"
        ));
        runtime
            .dispatch(
                instance,
                view,
                AppEvent::RecoveryFinished {
                    operation,
                    outcome: RecoveryOutcome::Opened {
                        message: "Opened original in markdown".to_string(),
                    },
                },
            )
            .unwrap();
        let state = match runtime.view_state(view).unwrap() {
            AppViewState::Recovery(state) => state,
            _ => unreachable!(),
        };
        assert_eq!(state.pending, None);
        assert_eq!(state.notice.as_deref(), Some("Opened original in markdown"));

        let commands = runtime.commands(instance, view).unwrap();
        assert!(
            commands.iter().any(|command| {
                command.id.as_str() == "recovery/open-original" && command.enabled
            })
        );
        assert!(
            commands
                .iter()
                .any(|command| command.id.as_str() == "recovery/copy" && command.enabled)
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
            [AppEffect::Recovery { reply, .. }] => reply.operation,
            effects => panic!("expected one recovery start, got {effects:?}"),
        };
        let pending = recovery_model_projection(&model, &real_view(&runtime), 1, 0, 0);
        assert_recovery_transition(&model, &before, &pending, "BeginRetry");

        // A peer action cannot start while Retry owns the one capability flight.
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

        // Even the right operation identifier arriving on the wrong completion
        // channel is stale and cannot clear Retry's ownership.
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

        // Negative control: the pre-fix reducer cleared ownership on a stale
        // completion. That state is neither an admitted transition nor valid.
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
        assert!(retry.title.contains("unavailable"));
        let original = commands
            .iter()
            .find(|command| command.id.as_str() == "recovery/open-original")
            .unwrap();
        assert!(!original.enabled);
        assert!(original.title.contains("unavailable"));
    }
}

#[derive(Clone, Debug, PartialEq)]
pub(crate) enum AppEvent {
    Action(ActionInvocation),
    FocusChanged(Option<UiKey>),
    InsertText(String),
    TextInput(TextInputEvent),
    EditorChord(crate::native_editor::KeyChord),
    EditorCommand(crate::native_editor::EditorCommand),
    EditorCompletion(crate::native_editor::EditorCompletionAction),
    EditorConfigCompletion(crate::native_config_language::ConfigCompletionEdit),
    EditorConfigCompletionRejected,
    EditorConfigNavigate {
        navigation: ConfigCompletionNavigation,
        candidates: usize,
        context: crate::native_config_language::ConfigCompletionContext,
    },
    EditorConfigDismiss {
        context: crate::native_config_language::ConfigCompletionContext,
    },
    EditorConfigDiagnosticNavigate {
        previous: bool,
    },
    /// Accessibility-owned source-byte selection for the editable text viewport.
    /// The document workspace validates UTF-8 boundaries before installing it.
    EditorSetSelection {
        anchor: usize,
        head: usize,
    },
    /// Host-owned semantic viewport capacity, derived from the exact editor
    /// rect and active text scale before an editor reducer transition.
    EditorViewportChanged {
        visible_lines: usize,
    },
    ScrollLines(i32),
    /// Markdown scrolling needs the exact semantic viewport measure so a tall
    /// block advances by rows rather than collapsing to one block step.
    MarkdownScroll {
        lines: i32,
        viewport_width: f32,
        viewport_height: f32,
    },
    /// One viewport-relative Markdown move. Page keys remain distinct from
    /// wheel rows all the way through the reducer, so compact and landscape
    /// surfaces advance by their real visible capacity after every resize.
    MarkdownPage {
        direction: i32,
        viewport_width: f32,
        viewport_height: f32,
    },
    /// Complete, atomically revisioned service snapshot. Text and the immutable
    /// Trail Pack catalog are one payload so cross-window Settings delivery can
    /// never observe catalog N with text/revision N+1.
    ConfigChanged(crate::native_config_service::ConfigSnapshot),
    UpdateChanged {
        revision: u64,
    },
    /// The packages projection advanced (worker completion or busy flip);
    /// every Settings view repaints from the shared controller snapshot.
    PackagesChanged {
        revision: u64,
    },
    DocumentChanged {
        document: DocumentId,
        revision: u64,
    },
    /// The host successfully focused or installed the editor for a Markdown
    /// document. This closes the synchronous open handshake in the originating
    /// reader instead of leaving its transient "Opening editor…" status behind.
    DocumentEditorOpened {
        document: DocumentId,
    },
    ConfigEditorFinished {
        operation: OperationId,
        outcome: ConfigEditorOutcome,
    },
    ConfigPatchFinished {
        operation: OperationId,
        outcome: ConfigPatchOutcome,
    },
    ExternalOpenFinished {
        operation: OperationId,
        outcome: ExternalOpenOutcome,
    },
    UpdateFinished {
        operation: OperationId,
        outcome: UpdateOutcome,
    },
    PackagesFinished {
        operation: OperationId,
        outcome: PackagesOutcome,
    },
    ClipboardFinished {
        operation: OperationId,
        outcome: ClipboardOutcome,
    },
    RecoveryFinished {
        operation: OperationId,
        outcome: RecoveryOutcome,
    },
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) enum TextInputEvent {
    Preedit(String),
    Commit(String),
    Backspace,
    Delete,
    Left {
        extend: bool,
    },
    Right {
        extend: bool,
    },
    /// Caret to the start of the value (readline/macOS Ctrl-A).
    Home {
        extend: bool,
    },
    /// Caret to the end of the value (readline/macOS Ctrl-E).
    End {
        extend: bool,
    },
    /// Delete from the caret to the end of the value (readline Ctrl-K).
    KillToEnd,
    /// Delete from the start of the value to the caret (readline Ctrl-U).
    KillToStart,
    /// Delete the word before the caret (readline Ctrl-W).
    DeleteWordBackward,
    /// Caret one WORD left/right (⌥←/⌥→, ⌥B/⌥F) in a Settings text field.
    WordLeft {
        extend: bool,
    },
    WordRight {
        extend: bool,
    },
    SelectAll,
    Undo,
    Redo,
    Submit,
    Cancel,
}

#[derive(Clone, Debug, PartialEq)]
pub(crate) struct ActionInvocation {
    pub(crate) id: ActionId,
    pub(crate) value: Option<SemanticInput>,
}

#[derive(Clone, Debug, PartialEq)]
pub(crate) enum SemanticInput {
    Text(String),
    Bool(bool),
    Number(f64),
    /// A UTF-8 byte boundary resolved from the exact painted text-field
    /// projection. `extend` preserves the field's current anchor (Shift-click
    /// and pointer release); otherwise the reducer collapses to a caret first.
    /// This is deliberately reducer input rather than host-side field mutation.
    TextPosition {
        byte: usize,
        extend: bool,
    },
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum EventResult {
    Handled,
    Bubble,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct Command {
    pub(crate) id: ActionId,
    pub(crate) title: String,
    pub(crate) shortcut: Option<String>,
    pub(crate) enabled: bool,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum CloseScope {
    View,
    Tab,
    Window,
    AppQuit,
    Relaunch,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct CloseRequest {
    pub(crate) scope: CloseScope,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) enum CloseReadiness {
    Ready,
    Pending { operation: OperationId },
    Blocked { recovery: Vec<Command> },
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum DamageRegion {
    All,
    Rect {
        x: u32,
        y: u32,
        width: u32,
        height: u32,
    },
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) enum ExpectedConfigValue {
    Any,
    Exact(Option<String>),
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct ConfigEdit {
    pub(crate) key: String,
    pub(crate) expected: ExpectedConfigValue,
    pub(crate) value: Option<String>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct ConfigPatch {
    /// Revision observed by the reducer when the user made this edit. The host
    /// must not replace it with its current revision or stale UI writes would
    /// silently defeat optimistic concurrency control.
    pub(crate) base_revision: u64,
    pub(crate) edits: Vec<ConfigEdit>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) enum ConfigPatchOutcome {
    Applied {
        revision: u64,
        undo: Option<u64>,
    },
    Conflict {
        revision: u64,
    },
    /// Replacement may be visible, but the atomic writer could not prove its
    /// final bytes/durability. Callers must reconcile before claiming success
    /// or issuing a blind follow-up.
    Indeterminate {
        message: String,
    },
    Rejected {
        message: String,
    },
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct ExternalOpenRequest {
    pub(crate) uri: String,
    pub(crate) user_initiated: bool,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) enum ExternalOpenOutcome {
    Opened,
    Denied { message: String },
    Failed { message: String },
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) enum ConfigEditorOutcome {
    Opened { canonical_uri: String },
    Failed { message: String },
}

/// Path-authority-free intent carried from Settings to the canonical Manual
/// editor. The host resolves only its own aterm.toml capability; this value can
/// choose text inside that document but can never redirect the filesystem open.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) enum ConfigEditorTarget {
    Key(String),
    Search(String),
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum UpdateRequest {
    Check,
    InstallAndRelaunch,
    Retry,
    InstallWhenSafe,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) enum UpdateOutcome {
    Accepted,
    InstalledNeedsRelaunch {
        build: u64,
        message: String,
    },
    /// Automatic apply observed input/output before reader park. Exact intent
    /// remains armed and no physical retry budget is consumed.
    Deferred {
        reason: String,
    },
    Blocked {
        reasons: Vec<String>,
    },
    Failed {
        message: String,
    },
}

/// One user-initiated packages verb, executed by the host against the
/// CO-LOCATED `atpkg` binary (never `PATH`), always off the UI thread. Plain
/// status refreshes are not a request: the host calls its own
/// `start_native_packages_refresh` directly (Settings open, verb completion).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum PackagesRequest {
    /// `atpkg update` — check/update every installed managed program now.
    CheckUpdate,
    /// `atpkg install --default-set` — the explicit ALab-toolset consent click.
    InstallDefaultSet,
    /// `atpkg uninstall --all` — remove the whole managed toolset and reclaim its
    /// disk (multiple GB).
    ///
    /// The way IN is zero clicks and no prompt (§9.1: the bytes ship inside the app,
    /// so installing the app is the consent). A way OUT that exists only as a CLI verb
    /// the user must first learn about is not a matching exit, and the asymmetry is
    /// what makes an unprompted multi-GB install feel like something done TO someone.
    /// Trashing aterm.app does not reclaim the store either — it orphans it under
    /// Application Support.
    UninstallAll,
}

/// Synchronous admission outcome for a [`PackagesRequest`] (the worker's real
/// results arrive later through the packages projection's revision fan-out,
/// exactly like update checks).
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) enum PackagesOutcome {
    Accepted,
    /// Structurally sound but refused right now (busy, or the manager refuses).
    Blocked {
        message: String,
    },
    Failed {
        message: String,
    },
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) enum ClipboardRequest {
    CopyText {
        text: String,
        sensitive: bool,
    },
    /// Resolve an exact UTF-8 source range in the host-owned canonical document
    /// at effect execution time. Native apps never receive ambient document or
    /// clipboard access merely because they can paint a preview.
    CopyDocumentRange {
        document: DocumentId,
        range: std::ops::Range<usize>,
        sensitive: bool,
    },
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) enum ClipboardOutcome {
    Copied,
    Denied { message: String },
    Failed { message: String },
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum WorkOwner {
    View {
        instance: AppInstanceId,
        view: ViewId,
        generation: u64,
    },
    Instance {
        instance: AppInstanceId,
        generation: u64,
    },
    Document {
        document: DocumentId,
        generation: u64,
    },
    Service {
        service: ServiceId,
        generation: u64,
    },
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum CompletionSink {
    View {
        instance: AppInstanceId,
        view: ViewId,
        generation: u64,
    },
    Instance {
        instance: AppInstanceId,
        generation: u64,
    },
    DocumentReducer {
        document: DocumentId,
        generation: u64,
    },
    ServiceReducer {
        service: ServiceId,
        generation: u64,
    },
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct ReplyToken<T> {
    pub(crate) work_owner: WorkOwner,
    pub(crate) sink: CompletionSink,
    pub(crate) operation: OperationId,
    output: PhantomData<fn() -> T>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) enum AppEffect {
    ConfigPatch {
        patch: ConfigPatch,
        reply: ReplyToken<ConfigPatchOutcome>,
    },
    ConfigUndo {
        token: u64,
        reply: ReplyToken<ConfigPatchOutcome>,
    },
    OpenExternal {
        request: ExternalOpenRequest,
        reply: ReplyToken<ExternalOpenOutcome>,
    },
    /// Resolve, ensure, and open the process's canonical `aterm.toml` through
    /// the native editor host. The reducer receives no ambient path authority.
    OpenConfigEditor {
        target: Option<ConfigEditorTarget>,
        reply: ReplyToken<ConfigEditorOutcome>,
    },
    Update {
        request: UpdateRequest,
        reply: ReplyToken<UpdateOutcome>,
    },
    Packages {
        request: PackagesRequest,
        reply: ReplyToken<PackagesOutcome>,
    },
    Clipboard {
        request: ClipboardRequest,
        reply: ReplyToken<ClipboardOutcome>,
    },
    Recovery {
        request: RecoveryRequest,
        reply: ReplyToken<RecoveryOutcome>,
    },
    /// Request the host's already-granted canonical document in the native
    /// editor. The reducer supplies identity, never a path.
    OpenDocumentEditor {
        document: DocumentId,
    },
    /// Open the host's native image picker for the terminal WALLPAPER; on an
    /// approved selection the host writes the `wallpaper` key through the
    /// versioned config lane (which re-decodes the image), so no reply is
    /// needed — the view converges via the ordinary config-change projection.
    ChooseWallpaperImage,
    RequestCloseSelf,
    InvalidateOwnPresentation,
    RepaintSelf(DamageRegion),
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct ViewMotionCx {
    pub(crate) system_reduced: bool,
    pub(crate) focused: bool,
    pub(crate) performance_reduced: bool,
    /// Live OS light/dark appearance. This is a host fact, not terminal palette
    /// state: Settings uses it to resolve `window_theme = "auto"` previews.
    pub(crate) system_dark: bool,
    /// The process-wide serious-mode override. Unlike Reduce Motion, this also
    /// suppresses static cursor trails and post-processing in authored previews.
    pub(crate) serious: bool,
}

impl Default for ViewMotionCx {
    fn default() -> Self {
        Self {
            system_reduced: false,
            focused: true,
            performance_reduced: false,
            system_dark: false,
            serious: false,
        }
    }
}

pub(crate) struct ViewCx<'a> {
    pub(crate) viewport: LogicalRect,
    pub(crate) config_revision: u64,
    pub(crate) update_revision: u64,
    /// Monotonic, host-injected phase for bounded semantic demonstrations.
    /// App models remain clockless and deterministic for an equal context.
    pub(crate) animation_phase_ms: u64,
    /// Unresolved host accessibility/focus/load facts. Settings resolves its
    /// currently highlighted candidate mode against these exact inputs, so
    /// preview paint and scheduling agree before the candidate is committed.
    pub(crate) motion: ViewMotionCx,
    /// The terminal renderer's actually-applied font size for this host. This
    /// keeps an unset/automatic Settings preview honest across scale factors,
    /// environment overrides, live zoom, and future mobile hosts.
    pub(crate) terminal_font_px: f32,
    /// The renderer's actually-applied terminal palette. Settings may layer an
    /// uncommitted built-in theme or color draft over this value without
    /// recoloring its own chrome or consulting platform APIs.
    pub(crate) terminal_theme: aterm_render::Theme,
    /// One exact host-prepared semantic renderer source for this view pass.
    /// Semantic compile and paint only consume this immutable snapshot; they
    /// never request, poll, or lock the candidate service.
    pub(crate) semantic_font: Option<crate::tray_raster::PreparedSemanticFont>,
    pub(crate) document: Option<&'a crate::document_store::DocumentSnapshot>,
}

pub(crate) struct UpdateCx<'a> {
    instance: AppInstanceId,
    view: ViewId,
    instance_generation: u64,
    view_generation: u64,
    next_operation: &'a mut u64,
    service_generations: &'a mut BTreeMap<ServiceId, u64>,
    effects: Vec<AppEffect>,
}

impl UpdateCx<'_> {
    pub(crate) const fn view_id(&self) -> ViewId {
        self.view
    }

    fn operation(&mut self) -> OperationId {
        let raw = (*self.next_operation).max(1);
        *self.next_operation = raw.saturating_add(1);
        OperationId(raw)
    }

    fn service_generation(&mut self, service: ServiceId) -> u64 {
        *self.service_generations.entry(service).or_insert(1)
    }

    fn service_reply<T>(&mut self, service: ServiceId) -> ReplyToken<T> {
        let generation = self.service_generation(service);
        ReplyToken {
            work_owner: WorkOwner::Service {
                service,
                generation,
            },
            sink: CompletionSink::ServiceReducer {
                service,
                generation,
            },
            operation: self.operation(),
            output: PhantomData,
        }
    }

    fn view_reply<T>(&mut self) -> ReplyToken<T> {
        ReplyToken {
            work_owner: WorkOwner::View {
                instance: self.instance,
                view: self.view,
                generation: self.view_generation,
            },
            sink: CompletionSink::View {
                instance: self.instance,
                view: self.view,
                generation: self.view_generation,
            },
            operation: self.operation(),
            output: PhantomData,
        }
    }

    pub(crate) fn config_patch(&mut self, patch: ConfigPatch) -> OperationId {
        let reply = self.service_reply(ServiceId::CONFIG);
        let operation = reply.operation;
        self.effects.push(AppEffect::ConfigPatch { patch, reply });
        operation
    }

    pub(crate) fn config_undo(&mut self, token: u64) -> OperationId {
        let reply = self.service_reply(ServiceId::CONFIG);
        let operation = reply.operation;
        self.effects.push(AppEffect::ConfigUndo { token, reply });
        operation
    }

    pub(crate) fn update(&mut self, request: UpdateRequest) -> OperationId {
        let reply = self.service_reply(ServiceId::UPDATER);
        let operation = reply.operation;
        self.effects.push(AppEffect::Update { request, reply });
        operation
    }

    pub(crate) fn packages(&mut self, request: PackagesRequest) -> OperationId {
        let reply = self.service_reply(ServiceId::PACKAGES);
        let operation = reply.operation;
        self.effects.push(AppEffect::Packages { request, reply });
        operation
    }

    pub(crate) fn open_external(&mut self, request: ExternalOpenRequest) -> OperationId {
        let reply = self.view_reply();
        let operation = reply.operation;
        self.effects
            .push(AppEffect::OpenExternal { request, reply });
        operation
    }

    pub(crate) fn open_config_editor(&mut self) -> OperationId {
        self.open_config_editor_at(None)
    }

    pub(crate) fn open_config_editor_at(
        &mut self,
        target: Option<ConfigEditorTarget>,
    ) -> OperationId {
        let reply = self.view_reply();
        let operation = reply.operation;
        self.effects
            .push(AppEffect::OpenConfigEditor { target, reply });
        operation
    }

    pub(crate) fn open_document_editor(&mut self, document: DocumentId) {
        self.effects
            .push(AppEffect::OpenDocumentEditor { document });
    }

    pub(crate) fn choose_wallpaper_image(&mut self) {
        self.effects.push(AppEffect::ChooseWallpaperImage);
    }

    pub(crate) fn clipboard(&mut self, request: ClipboardRequest) -> OperationId {
        let reply = self.view_reply();
        let operation = reply.operation;
        self.effects.push(AppEffect::Clipboard { request, reply });
        operation
    }

    pub(crate) fn recovery(&mut self, request: RecoveryRequest) -> OperationId {
        let reply = self.view_reply();
        let operation = reply.operation;
        self.effects.push(AppEffect::Recovery { request, reply });
        operation
    }

    pub(crate) fn request_close_self(&mut self) {
        self.effects.push(AppEffect::RequestCloseSelf);
    }

    pub(crate) fn invalidate_presentation(&mut self) {
        self.effects.push(AppEffect::InvalidateOwnPresentation);
    }

    pub(crate) fn repaint(&mut self, damage: DamageRegion) {
        self.effects.push(AppEffect::RepaintSelf(damage));
    }
}

#[derive(Debug)]
pub(crate) struct DispatchOutcome {
    pub(crate) result: EventResult,
    pub(crate) effects: Vec<AppEffect>,
}

#[derive(Debug, PartialEq, Eq)]
pub(crate) enum RuntimeError {
    IdExhausted,
    DuplicateInstance(AppInstanceId),
    UnknownInstance(AppInstanceId),
    DuplicateView(ViewId),
    UnknownView(ViewId),
    KindMismatch { app: AppKind, view: AppKind },
    StaleCompletion(OperationId),
}

/// Process-wide native-app instance and view-state store. Stable ids are never
/// reused; removing an owner advances its generation before stale async work can
/// be observed.
pub(crate) struct NativeRuntime {
    instance_ids: crate::tab_model::IdAllocator<AppInstanceId>,
    instances: BTreeMap<AppInstanceId, NativeApp>,
    instance_generations: BTreeMap<AppInstanceId, u64>,
    views: BTreeMap<ViewId, AppViewState>,
    view_generations: BTreeMap<ViewId, u64>,
    view_lifecycles: BTreeMap<ViewId, crate::front_content::ViewLifecycle>,
    document_generations: BTreeMap<DocumentId, u64>,
    service_generations: BTreeMap<ServiceId, u64>,
    /// Process-global failure/recovery projection for the config and theme
    /// watcher. New Settings/Manual views inherit this state at attach time.
    config_watch_status: crate::config_watcher::WatchStatusState,
    next_operation: u64,
}

impl Default for NativeRuntime {
    fn default() -> Self {
        Self {
            instance_ids: crate::tab_model::IdAllocator::default(),
            instances: BTreeMap::new(),
            instance_generations: BTreeMap::new(),
            views: BTreeMap::new(),
            view_generations: BTreeMap::new(),
            view_lifecycles: BTreeMap::new(),
            document_generations: BTreeMap::new(),
            service_generations: BTreeMap::new(),
            config_watch_status: crate::config_watcher::WatchStatusState::default(),
            next_operation: 1,
        }
    }
}

impl NativeRuntime {
    pub(crate) fn new() -> Self {
        Self::default()
    }

    pub(crate) fn insert_instance(
        &mut self,
        app: NativeApp,
    ) -> Result<AppInstanceId, RuntimeError> {
        let id = self
            .instance_ids
            .allocate()
            .map_err(|_| RuntimeError::IdExhausted)?;
        if self.instances.insert(id, app).is_some() {
            return Err(RuntimeError::DuplicateInstance(id));
        }
        self.instance_generations.insert(id, 1);
        Ok(id)
    }

    /// Attach state for a core-owned native view. The host passes the instance
    /// from `tab_model::NativeViewRef`; it is validated but not duplicated in the
    /// stored view state.
    pub(crate) fn attach_view(
        &mut self,
        view: ViewId,
        instance: AppInstanceId,
        mut state: AppViewState,
    ) -> Result<(), RuntimeError> {
        let Some(app) = self.instances.get(&instance) else {
            return Err(RuntimeError::UnknownInstance(instance));
        };
        if app.kind() != state.kind() {
            return Err(RuntimeError::KindMismatch {
                app: app.kind(),
                view: state.kind(),
            });
        }
        if self.views.contains_key(&view) {
            return Err(RuntimeError::DuplicateView(view));
        }
        let watch_status = self.config_watch_status.message();
        match &mut state {
            AppViewState::Settings(settings) => {
                settings.config_watch_status.clone_from(&watch_status);
            }
            AppViewState::Editor(editor) if matches!(app, NativeApp::Editor(app) if app.config_editor) =>
            {
                editor.config_watch_status.clone_from(&watch_status);
            }
            AppViewState::Markdown(_) | AppViewState::Editor(_) | AppViewState::Recovery(_) => {}
        }
        self.views.insert(view, state);
        self.view_generations.insert(view, 1);
        let mut lifecycle = crate::front_content::ViewLifecycle::Created;
        let mounted = lifecycle.transition(crate::front_content::ViewLifecycle::Mounted);
        debug_assert!(mounted);
        self.view_lifecycles.insert(view, lifecycle);
        Ok(())
    }

    pub(crate) fn remove_view(&mut self, view: ViewId) -> Option<AppViewState> {
        self.take_view_state(view)
    }

    /// Take one detached view state and invalidate every view-owned reply token.
    /// The core host must remove the `ViewStore` link in the same transaction.
    pub(crate) fn take_view_state(&mut self, view: ViewId) -> Option<AppViewState> {
        let state = self.views.remove(&view)?;
        if let Some(lifecycle) = self.view_lifecycles.get_mut(&view) {
            let closing = lifecycle.transition(crate::front_content::ViewLifecycle::Closing);
            debug_assert!(closing);
            let closed = lifecycle.transition(crate::front_content::ViewLifecycle::Closed);
            debug_assert!(closed);
        }
        let generation = self.view_generations.entry(view).or_insert(1);
        *generation = generation.saturating_add(1);
        Some(state)
    }

    pub(crate) fn remove_instance(&mut self, instance: AppInstanceId) -> Option<NativeApp> {
        let app = self.instances.remove(&instance)?;
        let generation = self.instance_generations.entry(instance).or_insert(1);
        *generation = generation.saturating_add(1);
        Some(app)
    }

    pub(crate) fn app(&self, instance: AppInstanceId) -> Option<&NativeApp> {
        self.instances.get(&instance)
    }

    pub(crate) fn instance_by_kind(&self, kind: AppKind) -> Option<AppInstanceId> {
        self.instances
            .iter()
            .find_map(|(instance, app)| (app.kind() == kind).then_some(*instance))
    }

    pub(crate) fn instance_for_document(
        &self,
        kind: AppKind,
        document: DocumentId,
    ) -> Option<AppInstanceId> {
        self.instances.iter().find_map(|(instance, app)| {
            (app.kind() == kind && app.document_id() == Some(document)).then_some(*instance)
        })
    }

    pub(crate) fn document_id(&self, instance: AppInstanceId) -> Option<DocumentId> {
        self.instances.get(&instance)?.document_id()
    }

    /// Replace the process-global Settings updater projection. There is at most
    /// one Settings controller; any number of views subscribe to that instance.
    pub(crate) fn replace_settings_update(
        &mut self,
        update: crate::update_screen::UpdateState,
        revision: u64,
    ) -> bool {
        let Some(settings) = self.instances.values_mut().find_map(|app| match app {
            NativeApp::Settings(settings) => Some(settings),
            NativeApp::Markdown(_) | NativeApp::Editor(_) | NativeApp::Recovery(_) => None,
        }) else {
            return false;
        };
        settings.replace_update(update, revision);
        true
    }

    /// Replace the process-global Settings packages projection (the packages
    /// analogue of [`Self::replace_settings_update`]).
    pub(crate) fn replace_settings_packages(
        &mut self,
        packages: crate::packages_screen::PackagesState,
        revision: u64,
    ) -> bool {
        let Some(settings) = self.instances.values_mut().find_map(|app| match app {
            NativeApp::Settings(settings) => Some(settings),
            NativeApp::Markdown(_) | NativeApp::Editor(_) | NativeApp::Recovery(_) => None,
        }) else {
            return false;
        };
        settings.replace_packages(packages, revision);
        true
    }

    /// Refresh every controller derived from one canonical document. The source
    /// remains owned by `DocumentStore`; Markdown retains only its parsed projection.
    pub(crate) fn publish_document(
        &mut self,
        document: DocumentId,
        source: &str,
        revision: u64,
        dirty: bool,
    ) {
        for app in self.instances.values_mut() {
            match app {
                NativeApp::Markdown(markdown) if markdown.document == document => {
                    markdown.parsed = crate::native_markdown::parse(source);
                    markdown.dirty = dirty;
                }
                NativeApp::Editor(editor) if editor.document == document => {
                    editor.dirty = dirty;
                    if editor.config_editor && editor.config_analysis_revision != revision {
                        // Pure TOML/schema analysis is worker-owned. Publishing
                        // new bytes only invalidates the old projection and
                        // closes Save until that exact revision completes.
                        editor.config_analysis = None;
                        editor.config_analysis_revision = revision;
                        editor.config_host_requested_revision = None;
                        editor.config_assist_cache.borrow_mut().clear();
                    }
                }
                _ => {}
            }
        }
    }

    /// Mark one already-granted canonical document as the real aterm config.
    /// The host resolves that identity; the editor model receives only immutable
    /// bytes and a document revision, never a path or filesystem capability.
    pub(crate) fn enable_config_editor(
        &mut self,
        document: DocumentId,
        _source: &str,
        revision: u64,
    ) -> bool {
        let mut enabled = false;
        for app in self.instances.values_mut() {
            if let NativeApp::Editor(editor) = app
                && editor.document == document
            {
                editor.config_editor = true;
                editor.config_analysis = None;
                editor.config_analysis_revision = revision;
                editor.config_host_requested_revision = None;
                editor.config_assist_cache.borrow_mut().clear();
                enabled = true;
            }
        }
        if enabled {
            self.synchronize_config_watch_status_views();
        }
        enabled
    }

    /// Apply a de-duplicated watcher edge and fan the resulting persistent
    /// status to every live Settings and config-editor view. Returns whether the
    /// process-global projection changed; callers then schedule one redraw.
    pub(crate) fn apply_config_watch_status(
        &mut self,
        event: crate::config_watcher::WatchStatusEvent,
    ) -> bool {
        if !self.config_watch_status.reduce(event) {
            return false;
        }
        self.synchronize_config_watch_status_views();
        true
    }

    pub(crate) fn note_config_watch_candidate(
        &mut self,
        baseline: crate::native_document_host::AtomicFileBaseline,
    ) {
        self.config_watch_status.note_config_candidate(baseline);
    }

    #[must_use]
    pub(crate) fn has_config_watch_candidate(&self) -> bool {
        self.config_watch_status.has_config_candidate()
    }

    pub(crate) fn acknowledge_config_watch_candidate(
        &mut self,
        baseline: &crate::native_document_host::AtomicFileBaseline,
    ) -> bool {
        if !self
            .config_watch_status
            .acknowledge_config_candidate(baseline)
        {
            return false;
        }
        self.synchronize_config_watch_status_views();
        true
    }

    pub(crate) fn reject_config_watch_candidate(
        &mut self,
        baseline: &crate::native_document_host::AtomicFileBaseline,
        kind: crate::config_watcher::WatchFailureKind,
    ) -> bool {
        if !self
            .config_watch_status
            .reject_config_candidate(baseline, kind)
        {
            return false;
        }
        self.synchronize_config_watch_status_views();
        true
    }

    fn synchronize_config_watch_status_views(&mut self) {
        let message = self.config_watch_status.message();
        let config_documents = self
            .instances
            .values()
            .filter_map(|app| match app {
                NativeApp::Editor(editor) if editor.config_editor => Some(editor.document),
                _ => None,
            })
            .collect::<std::collections::BTreeSet<_>>();
        for state in self.views.values_mut() {
            let status = match state {
                AppViewState::Settings(settings) => &mut settings.config_watch_status,
                AppViewState::Editor(editor)
                    if editor
                        .buffer
                        .as_ref()
                        .is_some_and(|buffer| config_documents.contains(&buffer.document)) =>
                {
                    &mut editor.config_watch_status
                }
                AppViewState::Markdown(_) | AppViewState::Editor(_) | AppViewState::Recovery(_) => {
                    continue;
                }
            };
            if *status == message {
                continue;
            }
            status.clone_from(&message);
            match state {
                AppViewState::Settings(settings) => {
                    settings.common.presentation_revision =
                        settings.common.presentation_revision.saturating_add(1);
                }
                AppViewState::Editor(editor) => {
                    editor.common.presentation_revision =
                        editor.common.presentation_revision.saturating_add(1);
                }
                AppViewState::Markdown(_) | AppViewState::Recovery(_) => {}
            }
        }
    }

    /// Latch one complete Manual analysis request for an exact source and host
    /// environment generation. The worker runs pure parsing first and probes
    /// filesystem-backed semantics only when that parse is admissible.
    pub(crate) fn begin_config_host_analysis(
        &mut self,
        document: DocumentId,
        revision: u64,
        analysis_generation: u64,
    ) -> bool {
        let mut requested = false;
        for app in self.instances.values_mut() {
            let NativeApp::Editor(editor) = app else {
                continue;
            };
            let request = (revision, analysis_generation);
            if !editor.config_editor
                || editor.document != document
                || editor.config_analysis_revision != revision
                || editor.config_host_requested_revision == Some(request)
            {
                continue;
            }
            // A byte-identical environment refresh can invalidate host-backed
            // asset/font diagnostics without changing the document revision.
            // Retaining the previous generation here would leave every Save
            // face enabled during the worker gap, so make validation pending
            // until this exact `(revision, generation)` completes.
            editor.config_analysis = None;
            editor.config_assist_cache.borrow_mut().clear();
            editor.config_host_requested_revision = Some(request);
            requested = true;
        }
        requested
    }

    /// Merge a host completion only into the exact Manual source revision that
    /// requested it. A completion from older text is presentation-inert.
    pub(crate) fn finish_config_host_analysis(
        &mut self,
        document: DocumentId,
        revision: u64,
        analysis_generation: u64,
        analysis: crate::native_config_language::ConfigAnalysis,
    ) -> bool {
        let mut changed = false;
        let mut accepted = false;
        for app in self.instances.values_mut() {
            let NativeApp::Editor(editor) = app else {
                continue;
            };
            if !editor.config_editor
                || editor.document != document
                || editor.config_analysis_revision != revision
                || editor.config_host_requested_revision != Some((revision, analysis_generation))
            {
                continue;
            }
            accepted = true;
            changed |= editor.config_analysis.as_ref() != Some(&analysis);
            editor.config_analysis = Some(analysis.clone());
        }
        if accepted {
            for state in self.views.values_mut() {
                let AppViewState::Editor(state) = state else {
                    continue;
                };
                if state
                    .buffer
                    .as_ref()
                    .is_some_and(|buffer| buffer.document == document)
                    && state
                        .status
                        .as_deref()
                        .is_some_and(|status| status.starts_with("Save blocked"))
                {
                    state.status = None;
                    state.common.presentation_revision =
                        state.common.presentation_revision.saturating_add(1);
                    changed = true;
                }
            }
        }
        changed
    }

    pub(crate) fn config_editor_enabled(&self, instance: AppInstanceId) -> bool {
        matches!(
            self.instances.get(&instance),
            Some(NativeApp::Editor(editor)) if editor.config_editor
        )
    }

    pub(crate) fn config_editor_analysis(
        &self,
        instance: AppInstanceId,
    ) -> Option<&crate::native_config_language::ConfigAnalysis> {
        match self.instances.get(&instance)? {
            NativeApp::Editor(editor) if editor.config_editor => editor.config_analysis.as_ref(),
            NativeApp::Settings(_)
            | NativeApp::Markdown(_)
            | NativeApp::Editor(_)
            | NativeApp::Recovery(_) => None,
        }
    }

    /// Resolve and retain assistance for one exact document sequence + caret.
    /// Both paint and keyboard input cross this seam, so activation never
    /// depends on a prior `EditorApp::view` pass having happened to fill the
    /// cache. The analysis-owned lexical index keeps a cache miss bounded to
    /// one line lookup and the size-capped current line.
    pub(crate) fn config_editor_assist(
        &self,
        instance: AppInstanceId,
        snapshot: &crate::document_store::DocumentSnapshot,
        caret: usize,
    ) -> Option<(
        crate::native_config_language::ConfigCompletionContext,
        crate::native_config_language::ConfigAssist,
    )> {
        match self.instances.get(&instance)? {
            NativeApp::Editor(editor) => editor.config_assist(snapshot, caret),
            NativeApp::Settings(_) | NativeApp::Markdown(_) | NativeApp::Recovery(_) => None,
        }
    }

    #[cfg(test)]
    pub(crate) fn clear_config_assist_cache(&self, instance: AppInstanceId) {
        if let Some(NativeApp::Editor(editor)) = self.instances.get(&instance) {
            editor.config_assist_cache.borrow_mut().clear();
        }
    }

    pub(crate) fn cached_config_completion_count(
        &self,
        instance: AppInstanceId,
        context: crate::native_config_language::ConfigCompletionContext,
    ) -> usize {
        match self.instances.get(&instance) {
            Some(NativeApp::Editor(editor)) if editor.config_editor => editor
                .config_assist_cache
                .borrow()
                .iter()
                .find(|(cached, _)| *cached == context)
                .map_or(0, |(_, assist)| assist.completions.len()),
            _ => 0,
        }
    }

    pub(crate) fn cached_config_assist_present(
        &self,
        instance: AppInstanceId,
        context: crate::native_config_language::ConfigCompletionContext,
    ) -> bool {
        match self.instances.get(&instance) {
            Some(NativeApp::Editor(editor)) if editor.config_editor => editor
                .config_assist_cache
                .borrow()
                .iter()
                .find(|(cached, _)| *cached == context)
                .is_some_and(|(_, assist)| assist.help.is_some() || !assist.completions.is_empty()),
            _ => false,
        }
    }

    pub(crate) fn config_editor_document(&self) -> Option<DocumentId> {
        self.instances.values().find_map(|app| match app {
            NativeApp::Editor(editor) if editor.config_editor => Some(editor.document),
            _ => None,
        })
    }

    pub(crate) fn config_editor_save_error(&self, document: DocumentId) -> Option<String> {
        self.instances.values().find_map(|app| match app {
            NativeApp::Editor(editor) if editor.document == document && editor.config_editor => {
                editor.config_analysis.as_ref().map_or_else(
                    || Some("Config validation is still in progress".to_string()),
                    |analysis| analysis.has_errors().then(|| analysis.summary()).flatten(),
                )
            }
            _ => None,
        })
    }

    pub(crate) fn set_document_saving(&mut self, document: DocumentId, saving: bool) {
        for app in self.instances.values_mut() {
            if let NativeApp::Editor(editor) = app
                && editor.document == document
            {
                editor.checkpoint_pending = saving;
            }
        }
    }

    pub(crate) fn set_document_disk_conflict(&mut self, document: DocumentId, disk_conflict: bool) {
        for app in self.instances.values_mut() {
            if let NativeApp::Editor(editor) = app
                && editor.document == document
            {
                editor.disk_conflict = disk_conflict;
            }
        }
    }

    pub(crate) fn set_editor_history_availability(
        &mut self,
        document: DocumentId,
        can_undo: bool,
        can_redo: bool,
    ) {
        for app in self.instances.values_mut() {
            if let NativeApp::Editor(editor) = app
                && editor.document == document
            {
                editor.can_undo = can_undo;
                editor.can_redo = can_redo;
            }
        }
    }

    pub(crate) fn document_identities(&self) -> Vec<(AppInstanceId, AppKind, String, String)> {
        self.instances
            .iter()
            .filter_map(|(instance, app)| match app {
                NativeApp::Markdown(markdown) => Some((
                    *instance,
                    AppKind::Markdown,
                    markdown.title.clone(),
                    markdown.canonical_uri.clone(),
                )),
                NativeApp::Editor(editor) => Some((
                    *instance,
                    AppKind::Editor,
                    editor.title.clone(),
                    editor.canonical_uri.clone(),
                )),
                NativeApp::Settings(_) | NativeApp::Recovery(_) => None,
            })
            .collect()
    }

    /// Recompute human-readable document titles from immutable canonical
    /// identity. Equal basenames remain short when unique; collisions receive
    /// the shortest unique parent suffix. The canonical URI itself never
    /// changes and remains the exact tooltip/restore/save authority.
    pub(crate) fn disambiguate_document_titles(&mut self) -> Vec<AppInstanceId> {
        let identities = self
            .instances
            .iter()
            .filter_map(|(instance, app)| match app {
                NativeApp::Markdown(app) => {
                    Some((*instance, app.base_title.clone(), app.canonical_uri.clone()))
                }
                NativeApp::Editor(app) => {
                    Some((*instance, app.base_title.clone(), app.canonical_uri.clone()))
                }
                NativeApp::Settings(_) | NativeApp::Recovery(_) => None,
            })
            .collect::<Vec<_>>();
        let mut by_basename = BTreeMap::<String, Vec<(AppInstanceId, String)>>::new();
        for (instance, basename, uri) in &identities {
            by_basename
                .entry(basename.clone())
                .or_default()
                .push((*instance, uri.clone()));
        }

        let mut desired = BTreeMap::new();
        for (basename, group) in by_basename {
            let distinct_uri_count = group
                .iter()
                .map(|(_, uri)| uri.as_str())
                .collect::<std::collections::BTreeSet<_>>()
                .len();
            if distinct_uri_count <= 1 {
                for (instance, _) in group {
                    desired.insert(instance, basename.clone());
                }
                continue;
            }
            for (instance, uri) in &group {
                let suffix = shortest_unique_document_parent(uri, &group);
                desired.insert(
                    *instance,
                    format!(
                        "{basename} — {}",
                        bounded_document_parent_label(&suffix, uri)
                    ),
                );
            }
        }

        let mut changed = Vec::new();
        for (instance, title) in desired {
            let current = match self.instances.get_mut(&instance) {
                Some(NativeApp::Markdown(app)) => &mut app.title,
                Some(NativeApp::Editor(app)) => &mut app.title,
                _ => continue,
            };
            if *current != title {
                *current = title;
                changed.push(instance);
            }
        }
        changed
    }

    pub(crate) fn set_document_display_title(
        &mut self,
        instance: AppInstanceId,
        title: String,
    ) -> bool {
        match self.instances.get_mut(&instance) {
            Some(NativeApp::Markdown(app)) => {
                app.title = title;
                true
            }
            Some(NativeApp::Editor(app)) => {
                app.title = title;
                true
            }
            _ => false,
        }
    }

    pub(crate) fn set_document_recovery_status(
        &mut self,
        document: DocumentId,
        status: Option<String>,
    ) {
        for app in self.instances.values_mut() {
            match app {
                NativeApp::Markdown(markdown) if markdown.document == document => {
                    markdown.recovery_status.clone_from(&status);
                }
                NativeApp::Editor(editor) if editor.document == document => {
                    editor.recovery_status.clone_from(&status);
                }
                _ => {}
            }
        }
    }

    pub(crate) fn view_state(&self, view: ViewId) -> Option<&AppViewState> {
        self.views.get(&view)
    }

    /// Current lifecycle generation for one live view. Host-owned async and
    /// accessibility routes stamp this value so a detached view can never receive a
    /// completion or platform action issued for its former incarnation.
    pub(crate) fn view_generation(&self, view: ViewId) -> Option<u64> {
        if !self.views.contains_key(&view) {
            return None;
        }
        self.view_generations.get(&view).copied()
    }

    pub(crate) fn view_lifecycle(
        &self,
        view: ViewId,
    ) -> Option<crate::front_content::ViewLifecycle> {
        self.view_lifecycles.get(&view).copied()
    }

    /// Suspend hidden native views without destroying their view-local state.
    /// A closing/closed view rejects attempts to become visible again.
    pub(crate) fn set_view_suspended(
        &mut self,
        view: ViewId,
        suspended: bool,
    ) -> Result<(), RuntimeError> {
        if !self.views.contains_key(&view) {
            return Err(RuntimeError::UnknownView(view));
        }
        let lifecycle = self
            .view_lifecycles
            .get_mut(&view)
            .ok_or(RuntimeError::UnknownView(view))?;
        let next = if suspended {
            crate::front_content::ViewLifecycle::Suspended
        } else {
            crate::front_content::ViewLifecycle::Mounted
        };
        if *lifecycle == next {
            return Ok(());
        }
        lifecycle
            .transition(next)
            .then_some(())
            .ok_or(RuntimeError::UnknownView(view))
    }

    /// Mutate view-local controller state without exposing the runtime's app or
    /// generation maps. Document hosts use this immediately after attaching a
    /// core-owned view to install its store-issued editor buffer handle.
    pub(crate) fn view_state_mut(&mut self, view: ViewId) -> Option<&mut AppViewState> {
        self.views.get_mut(&view)
    }

    pub(crate) fn dispatch(
        &mut self,
        instance: AppInstanceId,
        view: ViewId,
        event: AppEvent,
    ) -> Result<DispatchOutcome, RuntimeError> {
        let instance_generation = *self
            .instance_generations
            .get(&instance)
            .ok_or(RuntimeError::UnknownInstance(instance))?;
        let view_generation = *self
            .view_generations
            .get(&view)
            .ok_or(RuntimeError::UnknownView(view))?;
        let app = self
            .instances
            .get_mut(&instance)
            .ok_or(RuntimeError::UnknownInstance(instance))?;
        let state = self
            .views
            .get_mut(&view)
            .ok_or(RuntimeError::UnknownView(view))?;
        let mut cx = UpdateCx {
            instance,
            view,
            instance_generation,
            view_generation,
            next_operation: &mut self.next_operation,
            service_generations: &mut self.service_generations,
            effects: Vec::new(),
        };
        let result = app.update(state, event, &mut cx)?;
        Ok(DispatchOutcome {
            result,
            effects: cx.effects,
        })
    }

    pub(crate) fn render(
        &self,
        instance: AppInstanceId,
        view: ViewId,
        cx: &ViewCx<'_>,
    ) -> Result<UiTree, RuntimeError> {
        let app = self
            .instances
            .get(&instance)
            .ok_or(RuntimeError::UnknownInstance(instance))?;
        let state = self
            .views
            .get(&view)
            .ok_or(RuntimeError::UnknownView(view))?;
        app.view(state, cx).map(|mut tree| {
            let common = state.common();
            tree.apply_interaction(
                common.last_focus.as_ref(),
                common.hovered.as_ref(),
                common.pressed.as_ref(),
                common.focus_visible,
            );
            tree
        })
    }

    pub(crate) fn commands(
        &self,
        instance: AppInstanceId,
        view: ViewId,
    ) -> Result<Vec<Command>, RuntimeError> {
        let app = self
            .instances
            .get(&instance)
            .ok_or(RuntimeError::UnknownInstance(instance))?;
        let state = self
            .views
            .get(&view)
            .ok_or(RuntimeError::UnknownView(view))?;
        let mut commands = Vec::new();
        app.commands(state, &mut commands)?;
        Ok(commands)
    }

    pub(crate) fn presentation(
        &self,
        instance: AppInstanceId,
        view: ViewId,
    ) -> Result<AppPresentation, RuntimeError> {
        let app = self
            .instances
            .get(&instance)
            .ok_or(RuntimeError::UnknownInstance(instance))?;
        let state = self
            .views
            .get(&view)
            .ok_or(RuntimeError::UnknownView(view))?;
        app.presentation(state)
    }

    pub(crate) fn prepare_close(
        &mut self,
        instance: AppInstanceId,
        view: ViewId,
        request: CloseRequest,
    ) -> Result<(CloseReadiness, Vec<AppEffect>), RuntimeError> {
        let instance_generation = *self
            .instance_generations
            .get(&instance)
            .ok_or(RuntimeError::UnknownInstance(instance))?;
        let view_generation = *self
            .view_generations
            .get(&view)
            .ok_or(RuntimeError::UnknownView(view))?;
        let app = self
            .instances
            .get_mut(&instance)
            .ok_or(RuntimeError::UnknownInstance(instance))?;
        let mut cx = UpdateCx {
            instance,
            view,
            instance_generation,
            view_generation,
            next_operation: &mut self.next_operation,
            service_generations: &mut self.service_generations,
            effects: Vec::new(),
        };
        let readiness = app.prepare_close(request, &mut cx);
        Ok((readiness, cx.effects))
    }

    pub(crate) fn bump_service_generation(&mut self, service: ServiceId) -> u64 {
        let generation = self.service_generations.entry(service).or_insert(1);
        *generation = generation.saturating_add(1);
        *generation
    }

    pub(crate) fn set_document_generation(&mut self, document: DocumentId, generation: u64) {
        self.document_generations
            .insert(document, generation.max(1));
    }

    pub(crate) fn completion_is_current<T>(&self, reply: &ReplyToken<T>) -> bool {
        Self::owner_matches_sink(reply.work_owner, reply.sink)
            && self.owner_is_current(reply.work_owner)
            && self.sink_is_current(reply.sink)
    }

    /// A reply token is one routing proof, not two unrelated live handles.  Keep
    /// the owner and reducer identity/generation coupled so a result of one kind
    /// can never be redirected into another currently-live sink merely because
    /// both halves pass their individual lifecycle checks.
    fn owner_matches_sink(owner: WorkOwner, sink: CompletionSink) -> bool {
        match (owner, sink) {
            (
                WorkOwner::View {
                    instance: owner_instance,
                    view: owner_view,
                    generation: owner_generation,
                },
                CompletionSink::View {
                    instance: sink_instance,
                    view: sink_view,
                    generation: sink_generation,
                },
            ) => {
                owner_instance == sink_instance
                    && owner_view == sink_view
                    && owner_generation == sink_generation
            }
            (
                WorkOwner::Instance {
                    instance: owner_instance,
                    generation: owner_generation,
                },
                CompletionSink::Instance {
                    instance: sink_instance,
                    generation: sink_generation,
                },
            ) => owner_instance == sink_instance && owner_generation == sink_generation,
            (
                WorkOwner::Document {
                    document: owner_document,
                    generation: owner_generation,
                },
                CompletionSink::DocumentReducer {
                    document: sink_document,
                    generation: sink_generation,
                },
            ) => owner_document == sink_document && owner_generation == sink_generation,
            (
                WorkOwner::Service {
                    service: owner_service,
                    generation: owner_generation,
                },
                CompletionSink::ServiceReducer {
                    service: sink_service,
                    generation: sink_generation,
                },
            ) => owner_service == sink_service && owner_generation == sink_generation,
            _ => false,
        }
    }

    fn owner_is_current(&self, owner: WorkOwner) -> bool {
        match owner {
            WorkOwner::View {
                instance,
                view,
                generation,
            } => {
                self.instances.contains_key(&instance)
                    && self.views.contains_key(&view)
                    && self.view_generations.get(&view) == Some(&generation)
            }
            WorkOwner::Instance {
                instance,
                generation,
            } => self.instance_generations.get(&instance) == Some(&generation),
            WorkOwner::Document {
                document,
                generation,
            } => self.document_generations.get(&document) == Some(&generation),
            WorkOwner::Service {
                service,
                generation,
            } => self.service_generations.get(&service) == Some(&generation),
        }
    }

    fn sink_is_current(&self, sink: CompletionSink) -> bool {
        match sink {
            CompletionSink::View {
                instance,
                view,
                generation,
            } => {
                self.instances.contains_key(&instance)
                    && self.views.contains_key(&view)
                    && self.view_generations.get(&view) == Some(&generation)
            }
            CompletionSink::Instance {
                instance,
                generation,
            } => self.instance_generations.get(&instance) == Some(&generation),
            CompletionSink::DocumentReducer {
                document,
                generation,
            } => self.document_generations.get(&document) == Some(&generation),
            CompletionSink::ServiceReducer {
                service,
                generation,
            } => self.service_generations.get(&service) == Some(&generation),
        }
    }
}

fn canonical_uri_parent_segments(uri: &str) -> Vec<String> {
    let (scheme, remainder) = uri
        .split_once("://")
        .map_or((None, uri), |(scheme, remainder)| (Some(scheme), remainder));
    let mut segments = Vec::new();
    if let Some(scheme) = scheme {
        segments.push(format!("{scheme}:"));
    }
    segments.extend(
        remainder
            .split('/')
            .filter(|segment| !segment.is_empty())
            .map(display_uri_segment),
    );
    // The final URI segment is the basename already present in the title.
    let _ = segments.pop();
    segments
}

fn display_uri_segment(segment: &str) -> String {
    let bytes = segment.as_bytes();
    let mut decoded = Vec::with_capacity(bytes.len());
    let mut index = 0;
    while index < bytes.len() {
        if bytes[index] == b'%'
            && index + 2 < bytes.len()
            && let (Some(high), Some(low)) =
                (hex_nibble(bytes[index + 1]), hex_nibble(bytes[index + 2]))
        {
            decoded.push((high << 4) | low);
            index += 3;
        } else {
            decoded.push(bytes[index]);
            index += 1;
        }
    }
    String::from_utf8(decoded)
        .unwrap_or_else(|_| segment.to_string())
        .chars()
        .map(|character| {
            if character.is_control() {
                '�'
            } else {
                character
            }
        })
        .collect()
}

const fn hex_nibble(byte: u8) -> Option<u8> {
    match byte {
        b'0'..=b'9' => Some(byte - b'0'),
        b'a'..=b'f' => Some(byte - b'a' + 10),
        b'A'..=b'F' => Some(byte - b'A' + 10),
        _ => None,
    }
}

fn document_parent_suffix(uri: &str, depth: usize) -> String {
    let segments = canonical_uri_parent_segments(uri);
    if segments.is_empty() {
        return "root".to_string();
    }
    segments[segments.len().saturating_sub(depth)..].join("/")
}

fn shortest_unique_document_parent(uri: &str, group: &[(AppInstanceId, String)]) -> String {
    let maximum_depth = canonical_uri_parent_segments(uri).len().max(1);
    for depth in 1..=maximum_depth {
        let candidate = document_parent_suffix(uri, depth);
        let unique = group
            .iter()
            .all(|(_, other)| other == uri || document_parent_suffix(other, depth) != candidate);
        if unique {
            return candidate;
        }
    }
    format!("location-{:06x}", stable_uri_tag(uri) & 0x00ff_ffff)
}

fn stable_uri_tag(uri: &str) -> u64 {
    // Explicit FNV-1a keeps presentation stable across runs/toolchains. This is
    // a disambiguating display tag, never authority or a security decision.
    let mut hash = 0xcbf2_9ce4_8422_2325_u64;
    for byte in uri.as_bytes() {
        hash ^= u64::from(*byte);
        hash = hash.wrapping_mul(0x0000_0100_0000_01b3);
    }
    hash
}

fn bounded_document_parent_label(label: &str, uri: &str) -> String {
    use aterm_grapheme::GraphemeClusters;

    let graphemes = label.graphemes().collect::<Vec<_>>();
    if graphemes.len() <= 48 {
        return label.to_string();
    }
    format!(
        "{}…{}·{:06x}",
        graphemes[..18].concat(),
        graphemes[graphemes.len() - 18..].concat(),
        stable_uri_tag(uri) & 0x00ff_ffff,
    )
}

/// Compact Markdown controller. It references one canonical document and keeps
/// only the parsed projection required to build the current semantic tree.
#[derive(Debug)]
pub(crate) struct MarkdownApp {
    pub(crate) document: DocumentId,
    pub(crate) parsed: crate::native_markdown::MarkdownDocument,
    pub(crate) title: String,
    pub(crate) base_title: String,
    pub(crate) canonical_uri: String,
    pub(crate) dirty: bool,
    pub(crate) recovery_status: Option<String>,
}

impl MarkdownApp {
    pub(crate) fn new(document: DocumentId, title: String, source: &str) -> Self {
        Self::new_with_uri(
            document,
            title,
            format!("document:local/{}", document.get()),
            source,
        )
    }

    pub(crate) fn new_with_uri(
        document: DocumentId,
        title: String,
        canonical_uri: String,
        source: &str,
    ) -> Self {
        Self {
            document,
            parsed: crate::native_markdown::parse(source),
            title: title.clone(),
            base_title: title,
            canonical_uri,
            dirty: false,
            recovery_status: None,
        }
    }

    fn selection(&self, view: &MarkdownViewState) -> Option<std::ops::Range<usize>> {
        view.selection
            .clone()
            .filter(|range| range.start < range.end && range.end <= self.parsed.source_len)
    }

    fn can_select_source(&self) -> bool {
        self.parsed.source_len > 0 && !self.parsed.blocks.is_empty()
    }

    fn location(view: &MarkdownViewState) -> crate::native_markdown::MarkdownLocation {
        crate::native_markdown::MarkdownLocation::new(view.source_anchor, view.visual_row)
    }

    fn navigate(view: &mut MarkdownViewState, location: crate::native_markdown::MarkdownLocation) {
        if view.history.current().is_none() {
            view.history.visit(Self::location(view));
        }
        if location == Self::location(view) {
            return;
        }
        view.source_anchor = location.source_anchor;
        view.visual_row = location.visual_row;
        view.history.visit(location);
        view.common.presentation_revision = view.common.presentation_revision.saturating_add(1);
    }

    fn navigate_source(view: &mut MarkdownViewState, anchor: usize) {
        Self::navigate(
            view,
            crate::native_markdown::MarkdownLocation::new(anchor, 0),
        );
    }

    fn outline_target(&self, view: &MarkdownViewState, delta: isize) -> Option<usize> {
        let active = crate::native_markdown::heading_at_source(&self.parsed, view.source_anchor)?;
        let current = self.parsed.outline.get(active)?;
        let target = if delta < 0 && view.source_anchor > current.source_start {
            active
        } else {
            active
                .saturating_add_signed(delta)
                .min(self.parsed.outline.len().saturating_sub(1))
        };
        self.parsed
            .outline
            .get(target)
            .map(|heading| heading.source_start)
    }

    fn link_enabled(&self, index: usize) -> bool {
        use crate::native_markdown::LinkPolicy;
        self.parsed
            .links
            .get(index)
            .is_some_and(|link| match link.policy {
                LinkPolicy::LocalAnchor => {
                    crate::native_markdown::local_anchor(&self.parsed, &link.destination).is_some()
                }
                LinkPolicy::ExplicitExternal => link.destination.len() <= 2_048,
                LinkPolicy::LocalDocument | LinkPolicy::DeniedScheme => false,
            })
    }
}

impl NativeAppModel for MarkdownApp {
    type ViewState = MarkdownViewState;

    fn descriptor(&self) -> AppDescriptor {
        AppDescriptor {
            kind: AppKind::Markdown,
            name: "Markdown",
            icon: AppIcon::Markdown,
            singleton: false,
        }
    }

    fn update(
        &mut self,
        view: &mut Self::ViewState,
        event: AppEvent,
        cx: &mut UpdateCx<'_>,
    ) -> EventResult {
        match event {
            AppEvent::DocumentChanged { document, .. } if document == self.document => {
                view.source_anchor = view.source_anchor.min(self.parsed.source_len);
                if !self.can_select_source() || self.selection(view).is_none() {
                    view.selection = None;
                }
                view.common.presentation_revision =
                    view.common.presentation_revision.saturating_add(1);
                EventResult::Handled
            }
            AppEvent::DocumentEditorOpened { document } if document == self.document => {
                view.notice = Some("Editor opened".to_string());
                view.common.presentation_revision =
                    view.common.presentation_revision.saturating_add(1);
                EventResult::Handled
            }
            AppEvent::FocusChanged(focus) => {
                view.common.last_focus = focus;
                EventResult::Handled
            }
            // Runtime-only callers (principally conformance tests) do not own a
            // window geometry. Production input is upgraded by the host to
            // `MarkdownScroll`; retain a deterministic reader measure here.
            AppEvent::ScrollLines(lines) => {
                let next = crate::native_markdown::move_visual_rows(
                    &self.parsed,
                    Self::location(view),
                    680.0,
                    lines.clamp(-256, 256) as isize,
                );
                Self::navigate(view, next);
                EventResult::Handled
            }
            AppEvent::MarkdownScroll {
                lines,
                viewport_width,
                viewport_height: _,
            } => {
                let extreme = lines.unsigned_abs() >= 10_000;
                match view.mode {
                    MarkdownViewMode::Source => {
                        let delta = if extreme && lines < 0 {
                            isize::MIN
                        } else if extreme {
                            isize::MAX
                        } else {
                            lines.clamp(-256, 256) as isize
                        };
                        let next = crate::native_markdown::move_source_lines(
                            &self.parsed,
                            view.source_anchor,
                            delta,
                        );
                        Self::navigate_source(view, next);
                    }
                    MarkdownViewMode::Preview | MarkdownViewMode::Split => {
                        let delta = if extreme && lines < 0 {
                            isize::MIN
                        } else if extreme {
                            isize::MAX
                        } else {
                            lines.clamp(-256, 256) as isize
                        };
                        let next = crate::native_markdown::move_visual_rows(
                            &self.parsed,
                            Self::location(view),
                            viewport_width.max(1.0),
                            delta,
                        );
                        Self::navigate(view, next);
                    }
                }
                EventResult::Handled
            }
            AppEvent::MarkdownPage {
                direction,
                viewport_width,
                viewport_height,
            } => {
                let page_rows = (viewport_height.max(22.0) / 22.0 * 0.82).floor().max(1.0) as isize;
                let delta = page_rows.saturating_mul(direction.signum() as isize);
                match view.mode {
                    MarkdownViewMode::Source => {
                        let next = crate::native_markdown::move_source_lines(
                            &self.parsed,
                            view.source_anchor,
                            delta,
                        );
                        Self::navigate_source(view, next);
                    }
                    MarkdownViewMode::Preview | MarkdownViewMode::Split => {
                        let next = crate::native_markdown::move_visual_rows(
                            &self.parsed,
                            Self::location(view),
                            viewport_width.max(1.0),
                            delta,
                        );
                        Self::navigate(view, next);
                    }
                }
                EventResult::Handled
            }
            AppEvent::Action(invocation) if invocation.id.as_str() == "markdown/back" => {
                if view.history.can_back()
                    && let Some(location) = view.history.back()
                {
                    view.source_anchor = location.source_anchor;
                    view.visual_row = location.visual_row;
                    view.common.presentation_revision =
                        view.common.presentation_revision.saturating_add(1);
                }
                EventResult::Handled
            }
            AppEvent::Action(invocation) if invocation.id.as_str() == "markdown/forward" => {
                if view.history.can_forward()
                    && let Some(location) = view.history.forward()
                {
                    view.source_anchor = location.source_anchor;
                    view.visual_row = location.visual_row;
                    view.common.presentation_revision =
                        view.common.presentation_revision.saturating_add(1);
                }
                EventResult::Handled
            }
            AppEvent::Action(invocation)
                if invocation.id.as_str() == "markdown/previous-section" =>
            {
                if let Some(anchor) = self.outline_target(view, -1) {
                    Self::navigate_source(view, anchor);
                }
                EventResult::Handled
            }
            AppEvent::Action(invocation) if invocation.id.as_str() == "markdown/next-section" => {
                if let Some(anchor) = self.outline_target(view, 1) {
                    Self::navigate_source(view, anchor);
                }
                EventResult::Handled
            }
            AppEvent::Action(invocation)
                if markdown_page_location(invocation.id.as_str()).is_some() =>
            {
                if let Some(location) = markdown_page_location(invocation.id.as_str()) {
                    Self::navigate(view, location);
                }
                EventResult::Handled
            }
            AppEvent::Action(invocation)
                if markdown_action_index(invocation.id.as_str(), "markdown/outline/").is_some() =>
            {
                if let Some(anchor) =
                    markdown_action_index(invocation.id.as_str(), "markdown/outline/")
                        .and_then(|index| self.parsed.outline.get(index))
                        .map(|heading| heading.source_start)
                {
                    Self::navigate_source(view, anchor);
                }
                EventResult::Handled
            }
            AppEvent::Action(invocation)
                if markdown_action_index(invocation.id.as_str(), "markdown/link/").is_some() =>
            {
                use crate::native_markdown::LinkPolicy;
                let Some(index) = markdown_action_index(invocation.id.as_str(), "markdown/link/")
                else {
                    return EventResult::Handled;
                };
                let Some(link) = self.parsed.links.get(index) else {
                    return EventResult::Handled;
                };
                match link.policy {
                    LinkPolicy::LocalAnchor => {
                        if let Some(anchor) =
                            crate::native_markdown::local_anchor(&self.parsed, &link.destination)
                        {
                            Self::navigate_source(view, anchor);
                        }
                    }
                    LinkPolicy::ExplicitExternal if link.destination.len() <= 2_048 => {
                        cx.open_external(ExternalOpenRequest {
                            uri: link.destination.clone(),
                            user_initiated: true,
                        });
                        view.notice = Some("Opening link…".to_string());
                        view.common.presentation_revision =
                            view.common.presentation_revision.saturating_add(1);
                    }
                    // Local documents require an explicit host open workflow and
                    // denied schemes never emit a capability request.
                    LinkPolicy::LocalDocument
                    | LinkPolicy::DeniedScheme
                    | LinkPolicy::ExplicitExternal => {}
                }
                EventResult::Handled
            }
            AppEvent::Action(invocation)
                if markdown_mode_from_action(invocation.id.as_str()).is_some() =>
            {
                if let Some(mode) = markdown_mode_from_action(invocation.id.as_str())
                    && view.mode != mode
                {
                    view.mode = mode;
                    view.notice = Some(format!("{} mode", mode.label()));
                    view.common.presentation_revision =
                        view.common.presentation_revision.saturating_add(1);
                }
                EventResult::Handled
            }
            AppEvent::Action(invocation)
                if markdown_action_index(invocation.id.as_str(), "markdown/select-block/")
                    .is_some() =>
            {
                if let Some(range) =
                    markdown_action_index(invocation.id.as_str(), "markdown/select-block/")
                        .and_then(|index| self.parsed.blocks.get(index))
                        .map(crate::native_markdown::MarkdownBlock::source)
                        .cloned()
                {
                    view.selection = Some(range);
                    view.notice = Some("Block source selected".to_string());
                    view.common.presentation_revision =
                        view.common.presentation_revision.saturating_add(1);
                }
                EventResult::Handled
            }
            AppEvent::Action(invocation)
                if markdown_range_from_action(invocation.id.as_str()).is_some() =>
            {
                if let Some(range) = markdown_range_from_action(invocation.id.as_str())
                    .filter(|range| range.start < range.end && range.end <= self.parsed.source_len)
                {
                    view.selection = Some(range);
                    view.notice = Some("Visible source selected".to_string());
                    view.common.presentation_revision =
                        view.common.presentation_revision.saturating_add(1);
                }
                EventResult::Handled
            }
            AppEvent::Action(invocation)
                if markdown_action_index(invocation.id.as_str(), "markdown/image/").is_some() =>
            {
                let Some(image) = markdown_action_index(invocation.id.as_str(), "markdown/image/")
                    .and_then(|index| self.parsed.images.get(index))
                else {
                    return EventResult::Handled;
                };
                match crate::native_markdown::reduce_image_action(image, true) {
                    crate::native_markdown::MarkdownImageAction::SelectLocalSource { range } => {
                        view.selection = Some(range);
                        view.notice = Some("Local image source selected".to_string());
                    }
                    crate::native_markdown::MarkdownImageAction::OpenRemote { uri } => {
                        cx.open_external(ExternalOpenRequest {
                            uri,
                            user_initiated: true,
                        });
                        view.notice = Some("Opening remote image…".to_string());
                    }
                    crate::native_markdown::MarkdownImageAction::Denied { message } => {
                        view.notice = Some(message.to_string());
                    }
                }
                view.common.presentation_revision =
                    view.common.presentation_revision.saturating_add(1);
                EventResult::Handled
            }
            AppEvent::Action(invocation) if invocation.id.as_str() == "markdown/edit" => {
                cx.open_document_editor(self.document);
                view.notice = Some("Opening editor…".to_string());
                view.common.presentation_revision =
                    view.common.presentation_revision.saturating_add(1);
                EventResult::Handled
            }
            AppEvent::TextInput(TextInputEvent::SelectAll) => {
                view.selection = self
                    .can_select_source()
                    .then_some(0..self.parsed.source_len);
                view.notice = view
                    .selection
                    .as_ref()
                    .map(|_| "All source selected".to_string());
                view.common.presentation_revision =
                    view.common.presentation_revision.saturating_add(1);
                EventResult::Handled
            }
            AppEvent::Action(invocation) if invocation.id.as_str() == "markdown/select-all" => {
                view.selection = self
                    .can_select_source()
                    .then_some(0..self.parsed.source_len);
                view.notice = view
                    .selection
                    .as_ref()
                    .map(|_| "All source selected".to_string());
                view.common.presentation_revision =
                    view.common.presentation_revision.saturating_add(1);
                EventResult::Handled
            }
            AppEvent::TextInput(TextInputEvent::Cancel) => {
                let changed = view.selection.take().is_some() || view.notice.take().is_some();
                if changed {
                    view.common.presentation_revision =
                        view.common.presentation_revision.saturating_add(1);
                }
                EventResult::Handled
            }
            AppEvent::Action(invocation)
                if invocation.id.as_str() == "markdown/clear-selection" =>
            {
                let changed = view.selection.take().is_some() || view.notice.take().is_some();
                if changed {
                    view.common.presentation_revision =
                        view.common.presentation_revision.saturating_add(1);
                }
                EventResult::Handled
            }
            AppEvent::Action(invocation) if invocation.id.as_str() == "markdown/copy" => {
                if let Some(range) = self.selection(view) {
                    cx.clipboard(ClipboardRequest::CopyDocumentRange {
                        document: self.document,
                        range,
                        sensitive: false,
                    });
                    view.notice = Some("Copying selection…".to_string());
                    view.common.presentation_revision =
                        view.common.presentation_revision.saturating_add(1);
                }
                EventResult::Handled
            }
            AppEvent::ClipboardFinished { outcome, .. } => {
                view.notice = Some(match outcome {
                    ClipboardOutcome::Copied => "Copied source selection".to_string(),
                    ClipboardOutcome::Denied { message } | ClipboardOutcome::Failed { message } => {
                        bounded_markdown_text(&message, 160)
                    }
                });
                view.common.presentation_revision =
                    view.common.presentation_revision.saturating_add(1);
                EventResult::Handled
            }
            AppEvent::ExternalOpenFinished { outcome, .. } => {
                view.notice = Some(match outcome {
                    ExternalOpenOutcome::Opened => "Opened link in the default browser".to_string(),
                    ExternalOpenOutcome::Denied { message }
                    | ExternalOpenOutcome::Failed { message } => {
                        bounded_markdown_text(&message, 160)
                    }
                });
                view.common.presentation_revision =
                    view.common.presentation_revision.saturating_add(1);
                EventResult::Handled
            }
            _ => EventResult::Bubble,
        }
    }

    fn view(&self, view: &Self::ViewState, cx: &ViewCx<'_>) -> UiTree {
        use crate::native_markdown::MarkdownBlock;
        use crate::native_ui::{
            ButtonIcon, ButtonSpec, Control, ControlState, GroupSpec, Insets, Layout, Length,
            MarkdownBlockKind, MarkdownBlockSpec, SemanticRole, StyleRef, TextSpec, UiContent,
            UiNode,
        };

        let wide_outline = view.mode == MarkdownViewMode::Preview
            && cx.viewport.width >= 920.0
            && cx.viewport.height >= 380.0;
        let outline_width = if wide_outline { 220.0 } else { 0.0 };
        let main_width = (cx.viewport.width - outline_width).max(280.0);
        let compact = main_width < 590.0 || cx.viewport.height < 440.0;
        let horizontal_padding = if compact {
            14.0
        } else if view.mode == MarkdownViewMode::Split {
            ((main_width - 1_240.0) / 2.0).max(28.0)
        } else {
            ((main_width - 760.0) / 2.0).max(28.0)
        };
        let content_width = (main_width - horizontal_padding * 2.0).max(252.0);
        let header_height = if compact { 92.0 } else { 102.0 };
        let main_vertical_padding = if compact { 12.0 } else { 20.0 };
        let main_gap = if compact { 8.0 } else { 10.0 };
        let preview_horizontal_inset = if compact { 10.0 } else { 14.0 };
        let preview_vertical_inset = if compact { 8.0 } else { 10.0 };
        let split_gap = 16.0;
        let preview_content_width = if view.mode == MarkdownViewMode::Split {
            ((content_width - split_gap) / 2.0 - preview_horizontal_inset * 2.0).max(180.0)
        } else {
            (content_width - preview_horizontal_inset * 2.0).max(180.0)
        };
        let preview_viewport_height = (cx.viewport.height
            - header_height
            - main_vertical_padding * 2.0
            - main_gap
            - preview_vertical_inset * 2.0)
            .max(120.0);
        let visible = crate::native_markdown::layout_visible_blocks(
            &self.parsed,
            view.source_anchor,
            view.visual_row,
            preview_content_width,
            preview_viewport_height,
        );
        let anchor_index = visible.anchor_index;
        // Page controls carry an exact block target in their semantic action.
        // Both directions use the same bounded, width-aware height estimator as
        // the visible layout, so inspect/act and a pointer click advance by the
        // same reading band without smuggling pixel offsets into the reducer.
        let page_rows = (preview_viewport_height / 22.0 * 0.82).floor().max(1.0) as isize;
        let current_location = Self::location(view);
        let (previous_page, next_page) = if view.mode == MarkdownViewMode::Source {
            (
                crate::native_markdown::MarkdownLocation::new(
                    crate::native_markdown::move_source_lines(
                        &self.parsed,
                        view.source_anchor,
                        -page_rows,
                    ),
                    0,
                ),
                crate::native_markdown::MarkdownLocation::new(
                    crate::native_markdown::move_source_lines(
                        &self.parsed,
                        view.source_anchor,
                        page_rows,
                    ),
                    0,
                ),
            )
        } else {
            (
                crate::native_markdown::move_visual_rows(
                    &self.parsed,
                    current_location,
                    preview_content_width,
                    -page_rows,
                ),
                crate::native_markdown::move_visual_rows(
                    &self.parsed,
                    current_location,
                    preview_content_width,
                    page_rows,
                ),
            )
        };
        let can_page_back = previous_page != current_location;
        let can_page_forward = next_page != current_location;
        let current_heading =
            crate::native_markdown::heading_at_source(&self.parsed, view.source_anchor);
        let current_section = current_heading
            .and_then(|index| self.parsed.outline.get(index))
            .map_or("Document", |heading| heading.text.as_str());
        let selected = self.selection(view);
        let visible_source_start = visible
            .blocks
            .iter()
            .find(|block| block.index >= anchor_index)
            .map_or(view.source_anchor, |block| block.source.start);
        let visible_source_end = visible
            .blocks
            .last()
            .map_or(visible_source_start, |block| block.source.end);
        let visible_links =
            markdown_links_in_range(&self.parsed, visible_source_start..visible_source_end, 3);
        let visible_images =
            markdown_images_in_range(&self.parsed, visible_source_start..visible_source_end, 2);
        let mut preview_blocks = Vec::with_capacity(visible.blocks.len() + 2);
        let section_count = self.parsed.outline.len();
        let section_position = current_heading.map_or(0, |index| index.saturating_add(1));
        let progress_percent = view
            .source_anchor
            .min(self.parsed.source_len)
            .saturating_mul(100)
            .checked_div(self.parsed.source_len)
            .unwrap_or(0);
        let state_suffix = if selected.is_some() {
            " · source selected"
        } else if self.dirty {
            " · modified"
        } else {
            ""
        };
        let progress = if self.parsed.blocks.is_empty() {
            format!("Empty document{state_suffix}")
        } else if section_count == 0 {
            if compact {
                format!(
                    "Block {}/{} · {}% read{state_suffix}",
                    anchor_index.saturating_add(1).min(self.parsed.blocks.len()),
                    self.parsed.blocks.len(),
                    progress_percent,
                )
            } else {
                format!(
                    "{}% read · block {} of {}{state_suffix}",
                    progress_percent,
                    anchor_index.saturating_add(1).min(self.parsed.blocks.len()),
                    self.parsed.blocks.len(),
                )
            }
        } else if compact {
            format!(
                "Section {}/{} · {}% read{state_suffix}",
                section_position.max(1),
                section_count,
                progress_percent,
            )
        } else {
            format!(
                "{} · section {} of {} · {}% read{state_suffix}",
                bounded_markdown_label(current_section, if compact { 24 } else { 42 }),
                section_position.max(1),
                section_count,
                progress_percent,
            )
        };
        let reader_status = view
            .notice
            .as_deref()
            .or(self.recovery_status.as_deref())
            .map_or(progress, |notice| bounded_markdown_text(notice, 180));
        let reader_status = bounded_markdown_label(&reader_status, if compact { 52 } else { 96 });
        // When the wide outline is already consuming reader width, dormant
        // history affordances should not erase the document identity. They
        // reappear as one stable pair as soon as either direction is useful;
        // narrower/split layouts keep the familiar fixed toolbar group.
        let show_history_controls =
            !wide_outline || view.history.can_back() || view.history.can_forward();

        let button = |key: String,
                      spec: ButtonSpec,
                      action: String,
                      enabled: bool,
                      selected: bool,
                      width: Length| {
            UiNode::new(
                key,
                UiContent::Button(
                    Control::new(spec, ActionId::new(action))
                        .state(ControlState {
                            enabled,
                            selected,
                            ..ControlState::default()
                        })
                        .style(StyleRef::Navigation),
                ),
            )
            .layout(Layout::default().width(width).height(Length::Fill))
        };

        let text_scale = crate::native_appearance::text_scale();
        let preview_button_width = markdown_toolbar_label_width("Preview", 62.0, text_scale);
        let source_button_width = markdown_toolbar_label_width("Source", 56.0, text_scale);
        let split_button_width = markdown_toolbar_label_width("Split", 48.0, text_scale);
        let edit_button_width = markdown_toolbar_label_width("Edit", 44.0, text_scale);
        let selection_label = if selected.is_some() { "Clear" } else { "All" };
        let selection_button_width =
            markdown_toolbar_label_width(selection_label, 54.0, text_scale);
        let compact_mode_width = markdown_toolbar_label_width(view.mode.label(), 58.0, text_scale);
        let compact_edit_width = markdown_toolbar_label_width("Edit", 38.0, text_scale);
        let compact_selection_width =
            markdown_toolbar_label_width(selection_label, 42.0, text_scale);

        // The title is the only flexible item in this row. Bound it with the
        // same proportional title metrics used by the rasterizer, after
        // reserving the exact button, gap, and inset budget. A character cap
        // can still hard-clip wide glyphs (and wastes room on narrow ones).
        let wide_mode_controls = preview_button_width
            + source_button_width
            + split_button_width
            + edit_button_width
            + selection_button_width
            + 38.0;
        let compact_mode_controls =
            compact_mode_width + compact_edit_width + compact_selection_width + 34.0;
        let (fixed_control_width, button_count, header_horizontal_inset) = if compact {
            if main_width >= 400.0 && show_history_controls {
                (compact_mode_controls + 76.0, 6, 10.0)
            } else {
                (compact_mode_controls, 4, 10.0)
            }
        } else if show_history_controls {
            (wide_mode_controls + 76.0, 8, 14.0)
        } else {
            (wide_mode_controls, 6, 14.0)
        };
        let title_width = (content_width
            - header_horizontal_inset * 2.0
            - fixed_control_width
            - button_count as f32 * 6.0
            - 4.0)
            .max(0.0);
        let header_title = bounded_middle_label_to_width(
            &self.title,
            title_width,
            20.0 * crate::native_appearance::text_scale(),
        );
        let header_label = format!(
            "Markdown reader: {}",
            bounded_markdown_text(&self.title, 180)
        );
        // Compact chrome already exposes the canonical document name in the
        // selected native tab. Repeating it in this narrow action row turns a
        // useful filename into an opaque `S…` fragment and steals touch-target
        // space. Desktop retains the in-view reader title; compact surfaces
        // devote this row entirely to reader actions.
        let mut header_row_children = if compact {
            Vec::new()
        } else {
            vec![
                UiNode::new(
                    "markdown/title",
                    UiContent::Text(TextSpec {
                        text: header_title,
                        role: SemanticRole::Heading,
                        style: StyleRef::Primary,
                    }),
                )
                .layout(Layout::default().width(Length::Fill).height(Length::Fill)),
            ]
        };
        if compact {
            if main_width >= 400.0 && show_history_controls {
                header_row_children.extend([
                    button(
                        "markdown/back-button".to_string(),
                        ButtonSpec::new("Back in reading history").visual_icon(ButtonIcon::Back),
                        "markdown/back".to_string(),
                        view.history.can_back(),
                        false,
                        Length::Fixed(38.0),
                    ),
                    button(
                        "markdown/forward-button".to_string(),
                        ButtonSpec::new("Forward in reading history")
                            .visual_icon(ButtonIcon::Forward),
                        "markdown/forward".to_string(),
                        view.history.can_forward(),
                        false,
                        Length::Fixed(38.0),
                    ),
                ]);
            }
            let (next_mode, mode_label) = match view.mode {
                MarkdownViewMode::Preview => ("markdown/mode/source", "Preview"),
                MarkdownViewMode::Source => ("markdown/mode/preview", "Source"),
                MarkdownViewMode::Split => ("markdown/mode/preview", "Split"),
            };
            header_row_children.extend([
                button(
                    "markdown/mode-compact".to_string(),
                    ButtonSpec::new(format!("{mode_label} mode; switch view"))
                        .visual_label(mode_label),
                    next_mode.to_string(),
                    true,
                    true,
                    Length::Fixed(compact_mode_width),
                ),
                button(
                    "markdown/edit-button".to_string(),
                    ButtonSpec::new("Edit this document").visual_label("Edit"),
                    "markdown/edit".to_string(),
                    true,
                    false,
                    Length::Fixed(compact_edit_width),
                ),
                button(
                    "markdown/selection-button".to_string(),
                    ButtonSpec::new(if selected.is_some() {
                        "Clear source selection"
                    } else {
                        "Select all source"
                    })
                    .visual_label(if selected.is_some() {
                        "Clear"
                    } else {
                        "All"
                    }),
                    if selected.is_some() {
                        "markdown/clear-selection".to_string()
                    } else {
                        "markdown/select-all".to_string()
                    },
                    self.can_select_source(),
                    selected.is_some(),
                    Length::Fixed(compact_selection_width),
                ),
                button(
                    "markdown/copy-button".to_string(),
                    ButtonSpec::new("Copy source selection").visual_icon(ButtonIcon::Copy),
                    "markdown/copy".to_string(),
                    selected.is_some(),
                    false,
                    Length::Fixed(34.0),
                ),
            ]);
        } else {
            if show_history_controls {
                header_row_children.extend([
                    button(
                        "markdown/back-button".to_string(),
                        ButtonSpec::new("Back in reading history").visual_icon(ButtonIcon::Back),
                        "markdown/back".to_string(),
                        view.history.can_back(),
                        false,
                        Length::Fixed(38.0),
                    ),
                    button(
                        "markdown/forward-button".to_string(),
                        ButtonSpec::new("Forward in reading history")
                            .visual_icon(ButtonIcon::Forward),
                        "markdown/forward".to_string(),
                        view.history.can_forward(),
                        false,
                        Length::Fixed(38.0),
                    ),
                ]);
            }
            header_row_children.extend([
                button(
                    "markdown/preview-mode".to_string(),
                    ButtonSpec::new("Preview mode").visual_label("Preview"),
                    "markdown/mode/preview".to_string(),
                    true,
                    view.mode == MarkdownViewMode::Preview,
                    Length::Fixed(preview_button_width),
                ),
                button(
                    "markdown/source-mode".to_string(),
                    ButtonSpec::new("Source mode").visual_label("Source"),
                    "markdown/mode/source".to_string(),
                    true,
                    view.mode == MarkdownViewMode::Source,
                    Length::Fixed(source_button_width),
                ),
                button(
                    "markdown/split-mode".to_string(),
                    ButtonSpec::new("Split preview and source").visual_label("Split"),
                    "markdown/mode/split".to_string(),
                    cx.viewport.width >= 620.0,
                    view.mode == MarkdownViewMode::Split,
                    Length::Fixed(split_button_width),
                ),
                button(
                    "markdown/edit-button".to_string(),
                    ButtonSpec::new("Edit this document").visual_label("Edit"),
                    "markdown/edit".to_string(),
                    true,
                    false,
                    Length::Fixed(edit_button_width),
                ),
                button(
                    "markdown/selection-button".to_string(),
                    ButtonSpec::new(if selected.is_some() {
                        "Clear source selection"
                    } else {
                        "Select all source"
                    })
                    .visual_label(if selected.is_some() {
                        "Clear"
                    } else {
                        "All"
                    }),
                    if selected.is_some() {
                        "markdown/clear-selection".to_string()
                    } else {
                        "markdown/select-all".to_string()
                    },
                    self.can_select_source(),
                    selected.is_some(),
                    Length::Fixed(selection_button_width),
                ),
                button(
                    "markdown/copy-button".to_string(),
                    ButtonSpec::new("Copy source selection").visual_icon(ButtonIcon::Copy),
                    "markdown/copy".to_string(),
                    selected.is_some(),
                    false,
                    Length::Fixed(38.0),
                ),
            ]);
        }
        let header = UiNode::new(
            "markdown/header",
            UiContent::Group(GroupSpec::new(header_label).style(StyleRef::Secondary)),
        )
        .layout(
            Layout::column()
                .height(Length::Fixed(header_height))
                .padding(Insets::symmetric(if compact { 10.0 } else { 14.0 }, 10.0))
                .gap(4.0),
        )
        .children(vec![
            UiNode::new(
                "markdown/header-row",
                UiContent::Group(GroupSpec::unlabeled(SemanticRole::Group)),
            )
            .layout(
                Layout::row()
                    .height(Length::Fixed(if compact { 36.0 } else { 42.0 }))
                    .gap(6.0),
            )
            .children(header_row_children),
            UiNode::new(
                "markdown/status-row",
                UiContent::Group(GroupSpec::new("Reading position and page navigation")),
            )
            .layout(Layout::row().height(Length::Fixed(28.0)).gap(6.0))
            .children(vec![
                UiNode::new(
                    "markdown/status",
                    UiContent::Text(TextSpec {
                        text: reader_status,
                        role: SemanticRole::Status,
                        style: StyleRef::Primary,
                    }),
                )
                .layout(Layout::default().width(Length::Fill).height(Length::Fill)),
                button(
                    "markdown/previous-page-button".to_string(),
                    ButtonSpec::new("Previous reading page").visual_label(if compact {
                        "Prev"
                    } else {
                        "Previous"
                    }),
                    format!(
                        "markdown/page/{}/{}",
                        previous_page.source_anchor, previous_page.visual_row
                    ),
                    can_page_back,
                    false,
                    Length::Fixed(markdown_toolbar_label_width(
                        if compact { "Prev" } else { "Previous" },
                        if compact { 46.0 } else { 68.0 },
                        text_scale,
                    )),
                ),
                button(
                    "markdown/next-page-button".to_string(),
                    ButtonSpec::new("Next reading page").visual_label("Next"),
                    format!(
                        "markdown/page/{}/{}",
                        next_page.source_anchor, next_page.visual_row
                    ),
                    can_page_forward,
                    false,
                    Length::Fixed(markdown_toolbar_label_width(
                        "Next",
                        if compact { 46.0 } else { 54.0 },
                        text_scale,
                    )),
                ),
            ]),
        ]);
        if self.parsed.blocks.is_empty() {
            preview_blocks.push(
                UiNode::new(
                    "markdown/empty",
                    UiContent::Group(
                        GroupSpec::new("Empty Markdown document").style(StyleRef::Secondary),
                    ),
                )
                .layout(
                    Layout::column()
                        .height(Length::Fixed(if compact { 96.0 } else { 112.0 }))
                        .padding(Insets::all(if compact { 14.0 } else { 18.0 }))
                        .gap(6.0),
                )
                .children(vec![
                    UiNode::new(
                        "markdown/empty-title",
                        UiContent::Text(TextSpec {
                            text: "Nothing here yet".to_string(),
                            role: SemanticRole::Heading,
                            style: StyleRef::Primary,
                        }),
                    )
                    .layout(Layout::default().height(Length::Fixed(28.0))),
                    UiNode::new(
                        "markdown/empty-copy",
                        UiContent::Text(TextSpec {
                            text: "This file has no Markdown content.".to_string(),
                            role: SemanticRole::Status,
                            style: StyleRef::Quiet,
                        }),
                    )
                    .layout(Layout::default().height(Length::Fixed(22.0))),
                ]),
            );
        }
        for visible_block in visible
            .blocks
            .into_iter()
            .filter(|block| block.index >= anchor_index)
        {
            let index = visible_block.index;
            let Some(block) = self.parsed.blocks.get(index) else {
                continue;
            };
            let key = format!("markdown/block/{index}");
            let paint_height =
                (visible_block.height - crate::native_markdown::VISUAL_BLOCK_GAP).max(20.0);
            let kind = match block {
                MarkdownBlock::Heading { level, .. } => MarkdownBlockKind::Heading(*level),
                MarkdownBlock::Paragraph { .. } => MarkdownBlockKind::Paragraph,
                MarkdownBlock::ListItem { depth, ordinal, .. } => MarkdownBlockKind::ListItem {
                    depth: *depth,
                    ordinal: *ordinal,
                },
                MarkdownBlock::Quote { .. } => MarkdownBlockKind::Quote,
                MarkdownBlock::CodeBlock { language, .. } => MarkdownBlockKind::Code {
                    language: language.clone(),
                },
                MarkdownBlock::Table { .. } => MarkdownBlockKind::Table,
                MarkdownBlock::ThematicBreak { .. } => MarkdownBlockKind::Rule,
            };
            let node = UiContent::MarkdownBlock(MarkdownBlockSpec {
                text: bounded_markdown_block_text(block),
                kind,
                dense: false,
                selectable: !matches!(block, MarkdownBlock::ThematicBreak { .. }),
                action: (!matches!(block, MarkdownBlock::ThematicBreak { .. }))
                    .then(|| ActionId::new(format!("markdown/select-block/{index}"))),
                selected: selected.as_ref().is_some_and(|selection| {
                    selection.start < block.source().end && selection.end > block.source().start
                }),
                source: block.source().clone(),
                estimated_height: paint_height.clamp(20.0, 1_000_000.0),
                visual_row: visible_block.visual_row,
                total_visual_rows: visible_block.total_visual_rows,
            });
            preview_blocks.push(
                UiNode::new(key, node).layout(
                    Layout::default()
                        .height(Length::Fixed(paint_height.clamp(20.0, 1_000_000.0)))
                        .width(Length::Fill),
                ),
            );
        }

        let preview = UiNode::new(
            "markdown/preview",
            UiContent::Group(GroupSpec::new("Rendered Markdown").style(StyleRef::Secondary)),
        )
        .layout(
            Layout::column()
                .width(Length::Fill)
                .height(Length::Fill)
                .padding(Insets::symmetric(
                    preview_horizontal_inset,
                    preview_vertical_inset,
                ))
                // Move a small reserved tail from each virtual block outside
                // its painted rect. Cards no longer collide, while block+gap
                // remains aligned with virtual scroll geometry.
                .gap(crate::native_markdown::VISUAL_BLOCK_GAP)
                .clipped(),
        )
        .children(preview_blocks);
        let source_window = cx.document.map_or_else(
            crate::native_markdown::MarkdownSourceWindow::default,
            |snapshot| {
                crate::native_markdown::source_window_from_anchor(
                    &snapshot.text,
                    view.source_anchor,
                    96 * 1024,
                    256,
                )
            },
        );
        let source_action = (source_window.source.start < source_window.source.end).then(|| {
            ActionId::new(format!(
                "markdown/select-range/{}/{}",
                source_window.source.start, source_window.source.end
            ))
        });
        let source_selected = selected.as_ref().is_some_and(|selection| {
            selection.start < source_window.source.end && selection.end > source_window.source.start
        });
        let source_visual_rows = source_window.text.lines().count().max(1);
        let source = UiNode::new(
            "markdown/source",
            UiContent::MarkdownBlock(MarkdownBlockSpec {
                text: source_window.text,
                kind: MarkdownBlockKind::Code {
                    language: Some("markdown".to_string()),
                },
                dense: view.mode == MarkdownViewMode::Split,
                selectable: source_action.is_some(),
                action: source_action,
                selected: source_selected,
                source: source_window.source,
                estimated_height: (cx.viewport.height - header_height - 36.0).max(80.0),
                visual_row: 0,
                total_visual_rows: source_visual_rows,
            }),
        )
        .layout(Layout::default().width(Length::Fill).height(Length::Fill));
        let content = match view.mode {
            MarkdownViewMode::Preview => preview,
            MarkdownViewMode::Source => source,
            MarkdownViewMode::Split => UiNode::new(
                "markdown/split",
                UiContent::Group(
                    GroupSpec::new("Rendered preview and exact source").style(StyleRef::Secondary),
                ),
            )
            .layout(
                Layout::row()
                    .width(Length::Fill)
                    .height(Length::Fill)
                    .gap(split_gap)
                    .clipped(),
            )
            .children(vec![preview, source]),
        };

        let main = UiNode::new(
            "markdown/reader",
            UiContent::Group(GroupSpec::unlabeled(SemanticRole::Group)),
        )
        .layout(
            Layout::column()
                .width(Length::Fill)
                .height(Length::Fill)
                .padding(Insets::symmetric(horizontal_padding, main_vertical_padding))
                .gap(main_gap)
                .clipped(),
        )
        .children(vec![header, content]);

        let mut shell_children = Vec::with_capacity(if wide_outline { 2 } else { 1 });
        if wide_outline {
            let row_budget = (((cx.viewport.height - 116.0) / 34.0).floor() as usize).clamp(3, 18);
            let resource_reserve = usize::from(!visible_links.is_empty())
                .saturating_add(visible_links.len())
                .saturating_add(usize::from(!visible_images.is_empty()))
                .saturating_add(visible_images.len());
            let outline_limit = row_budget.saturating_sub(resource_reserve).clamp(3, 16);
            let outline_range = crate::native_markdown::outline_window(
                &self.parsed,
                view.source_anchor,
                outline_limit,
            );
            let mut outline = Vec::with_capacity(
                outline_range
                    .len()
                    .saturating_add(visible_links.len())
                    .saturating_add(visible_images.len())
                    .saturating_add(4),
            );
            outline.push(
                UiNode::new(
                    "markdown/outline-title",
                    UiContent::Text(TextSpec {
                        text: "CONTENTS".to_string(),
                        role: SemanticRole::Heading,
                        style: StyleRef::Quiet,
                    }),
                )
                .layout(Layout::default().height(Length::Fixed(20.0))),
            );
            outline.push(
                UiNode::new(
                    "markdown/current-section",
                    UiContent::Text(TextSpec {
                        text: if section_count == 0 {
                            "NO SECTIONS".to_string()
                        } else {
                            format!("SECTION {} OF {}", section_position.max(1), section_count)
                        },
                        role: SemanticRole::Status,
                        style: StyleRef::Primary,
                    }),
                )
                .layout(Layout::default().height(Length::Fixed(28.0))),
            );
            if outline_range.is_empty() {
                outline.push(
                    UiNode::new(
                        "markdown/no-outline",
                        UiContent::Text(TextSpec {
                            text: "No headings".to_string(),
                            role: SemanticRole::Status,
                            style: StyleRef::Quiet,
                        }),
                    )
                    .layout(Layout::default().height(Length::Fixed(32.0))),
                );
            } else {
                for index in outline_range {
                    let Some(heading) = self.parsed.outline.get(index) else {
                        continue;
                    };
                    let indent = "  ".repeat(usize::from(heading.level.saturating_sub(1)).min(3));
                    let visual = format!(
                        "{indent}{}",
                        bounded_markdown_label(
                            &heading.text,
                            32usize.saturating_sub(indent.chars().count())
                        )
                    );
                    outline.push(
                        UiNode::new(
                            format!("markdown/outline-item/{index}"),
                            UiContent::Button(
                                Control::new(
                                    ButtonSpec::new(visual),
                                    ActionId::new(format!("markdown/outline/{index}")),
                                )
                                .state(ControlState {
                                    selected: current_heading == Some(index),
                                    ..ControlState::default()
                                })
                                .style(StyleRef::Navigation),
                            ),
                        )
                        .layout(
                            Layout::default()
                                .height(Length::Fixed(32.0))
                                .width(Length::Fill),
                        ),
                    );
                }
            }
            if !visible_links.is_empty() {
                outline.push(
                    UiNode::new(
                        "markdown/links-title",
                        UiContent::Text(TextSpec {
                            text: "LINKS HERE".to_string(),
                            role: SemanticRole::Heading,
                            style: StyleRef::Quiet,
                        }),
                    )
                    .layout(Layout::default().height(Length::Fixed(20.0))),
                );
                for index in visible_links {
                    let Some(link) = self.parsed.links.get(index) else {
                        continue;
                    };
                    let local =
                        matches!(link.policy, crate::native_markdown::LinkPolicy::LocalAnchor);
                    let mark = if local { "#" } else { "Link" };
                    outline.push(
                        UiNode::new(
                            format!("markdown/link-item/{index}"),
                            UiContent::Button(
                                Control::new(
                                    ButtonSpec::new(format!(
                                        "{mark} {}",
                                        bounded_markdown_label(&link.label, 24)
                                    )),
                                    ActionId::new(format!("markdown/link/{index}")),
                                )
                                .state(ControlState {
                                    enabled: self.link_enabled(index),
                                    ..ControlState::default()
                                })
                                .style(StyleRef::Navigation),
                            ),
                        )
                        .layout(
                            Layout::default()
                                .height(Length::Fixed(32.0))
                                .width(Length::Fill),
                        ),
                    );
                }
            }
            if !visible_images.is_empty() {
                outline.push(
                    UiNode::new(
                        "markdown/images-title",
                        UiContent::Text(TextSpec {
                            text: "IMAGES HERE".to_string(),
                            role: SemanticRole::Heading,
                            style: StyleRef::Quiet,
                        }),
                    )
                    .layout(Layout::default().height(Length::Fixed(20.0))),
                );
                for index in visible_images {
                    let Some(image) = self.parsed.images.get(index) else {
                        continue;
                    };
                    let label = image.alt.as_deref().unwrap_or(&image.source_uri);
                    outline.push(
                        UiNode::new(
                            format!("markdown/image-item/{index}"),
                            UiContent::Button(
                                Control::new(
                                    ButtonSpec::new(format!(
                                        "Image {}",
                                        bounded_markdown_label(label, 22)
                                    )),
                                    ActionId::new(format!("markdown/image/{index}")),
                                )
                                .state(ControlState {
                                    enabled: !matches!(
                                        crate::native_markdown::reduce_image_action(image, true),
                                        crate::native_markdown::MarkdownImageAction::Denied { .. }
                                    ),
                                    ..ControlState::default()
                                })
                                .style(StyleRef::Navigation),
                            ),
                        )
                        .layout(
                            Layout::default()
                                .height(Length::Fixed(32.0))
                                .width(Length::Fill),
                        ),
                    );
                }
            }
            shell_children.push(
                UiNode::new(
                    "markdown/outline",
                    UiContent::Group(GroupSpec::unlabeled(SemanticRole::Navigation)),
                )
                .layout(
                    Layout::column()
                        .width(Length::Fixed(outline_width))
                        .height(Length::Fill)
                        .padding(Insets::all(16.0))
                        .gap(6.0)
                        .clipped(),
                )
                .children(outline),
            );
        }
        shell_children.push(main);
        UiTree::new(
            UiNode::new(
                "markdown/app",
                UiContent::Group(GroupSpec::unlabeled(SemanticRole::Application)),
            )
            .layout(Layout::row().clipped())
            .children(shell_children),
        )
    }

    fn commands(&self, view: &Self::ViewState, out: &mut Vec<Command>) {
        let active_heading =
            crate::native_markdown::heading_at_source(&self.parsed, view.source_anchor);
        // AUDIT I9 — the reader's navigation chords are NOT macOS-only: their
        // dispatch arm tests `SUPER | CTRL` (`app_native`'s `command`), so
        // Ctrl+[ / Ctrl+] / Ctrl+E genuinely work off macOS and blanking the
        // label there would HIDE a live chord rather than merely stop lying
        // about a dead one. Named per platform so the palette says the true
        // half; `platform_accel` still strips any `Cmd-` that reaches it.
        out.push(Command {
            id: ActionId::new("markdown/back"),
            title: "Reader: Back".to_string(),
            shortcut: Some(reader_command_chord("[")),
            enabled: view.history.can_back(),
        });
        out.push(Command {
            id: ActionId::new("markdown/forward"),
            title: "Reader: Forward".to_string(),
            shortcut: Some(reader_command_chord("]")),
            enabled: view.history.can_forward(),
        });
        out.push(Command {
            id: ActionId::new("markdown/previous-section"),
            title: "Reader: Previous Section".to_string(),
            shortcut: None,
            enabled: active_heading.is_some_and(|index| {
                index > 0 || self.parsed.outline[index].source_start < view.source_anchor
            }),
        });
        out.push(Command {
            id: ActionId::new("markdown/next-section"),
            title: "Reader: Next Section".to_string(),
            shortcut: None,
            enabled: active_heading
                .is_some_and(|index| index.saturating_add(1) < self.parsed.outline.len()),
        });
        out.push(Command {
            id: ActionId::new("markdown/select-all"),
            title: "Reader: Select All Source".to_string(),
            // LIVE off macOS, same as back/forward/edit above — an earlier
            // pass blanked this row there on a false premise. `app_native`'s
            // `Key::Character('a' | 'A') if command` arm maps ⌘/Ctrl+A to
            // `TextInput(SelectAll)`, and `MarkdownApp`'s reducer answers that
            // event with the exact body of this action. The readline arms that
            // shadow 'a' are gated on `settings_active` (false for the reader)
            // and `ctrl+a` is not in `PLATFORM_DEFAULT_PAIRS`, so nothing
            // upstream claims the chord. Blanking it hid a chord that works.
            shortcut: Some(reader_command_chord("A")),
            enabled: self.can_select_source()
                && self.selection(view).as_ref() != Some(&(0..self.parsed.source_len)),
        });
        out.push(Command {
            id: ActionId::new("markdown/copy"),
            title: "Reader: Copy Source Selection".to_string(),
            // Off macOS the live chord is the SEEDED `ctrl+shift+c` (`copy`),
            // which `on_key_native_mode` special-cases into
            // `copy_native_selection` — not Ctrl+C, and certainly not Win+C.
            shortcut: Some(
                if cfg!(target_os = "macos") {
                    "Cmd-C"
                } else {
                    "Ctrl-Shift-C"
                }
                .to_string(),
            ),
            enabled: self.selection(view).is_some(),
        });
        out.push(Command {
            id: ActionId::new("markdown/clear-selection"),
            title: "Reader: Clear Selection".to_string(),
            shortcut: Some("Esc".to_string()),
            enabled: view.selection.is_some() || view.notice.is_some(),
        });
        for (mode, id) in [
            (MarkdownViewMode::Preview, "markdown/mode/preview"),
            (MarkdownViewMode::Source, "markdown/mode/source"),
            (MarkdownViewMode::Split, "markdown/mode/split"),
        ] {
            out.push(Command {
                id: ActionId::new(id),
                title: format!("Reader: {} Mode", mode.label()),
                shortcut: None,
                enabled: view.mode != mode,
            });
        }
        out.push(Command {
            id: ActionId::new("markdown/edit"),
            title: "Edit Document".to_string(),
            shortcut: Some(reader_command_chord("E")),
            enabled: true,
        });

        let outline = crate::native_markdown::outline_window(&self.parsed, view.source_anchor, 32);
        for index in outline {
            let Some(heading) = self.parsed.outline.get(index) else {
                continue;
            };
            out.push(Command {
                id: ActionId::new(format!("markdown/outline/{index}")),
                title: format!(
                    "Go to Section: {}",
                    bounded_markdown_text(&heading.text, 120)
                ),
                shortcut: None,
                enabled: active_heading != Some(index),
            });
        }

        let block =
            crate::native_markdown::block_at_source(&self.parsed, view.source_anchor).unwrap_or(0);
        let link_end = self
            .parsed
            .blocks
            .get(
                block
                    .saturating_add(12)
                    .min(self.parsed.blocks.len().saturating_sub(1)),
            )
            .map_or(view.source_anchor, |block| block.source().end);
        for index in markdown_links_in_range(
            &self.parsed,
            view.source_anchor..link_end.max(view.source_anchor.saturating_add(1)),
            8,
        ) {
            let Some(link) = self.parsed.links.get(index) else {
                continue;
            };
            out.push(Command {
                id: ActionId::new(format!("markdown/link/{index}")),
                title: format!("Open Link: {}", bounded_markdown_text(&link.label, 120)),
                shortcut: None,
                enabled: self.link_enabled(index),
            });
        }
        for (index, image) in self
            .parsed
            .images
            .iter()
            .enumerate()
            .filter(|(_, image)| {
                image.source.start < link_end && image.source.end > view.source_anchor
            })
            .take(8)
        {
            out.push(Command {
                id: ActionId::new(format!("markdown/image/{index}")),
                title: format!(
                    "Image: {}",
                    bounded_markdown_text(image.alt.as_deref().unwrap_or(&image.source_uri), 120,)
                ),
                shortcut: None,
                enabled: !matches!(
                    crate::native_markdown::reduce_image_action(image, true),
                    crate::native_markdown::MarkdownImageAction::Denied { .. }
                ),
            });
        }
    }

    fn presentation(&self, _view: &Self::ViewState) -> AppPresentation {
        AppPresentation {
            title: self.title.clone(),
            icon: AppIcon::Markdown,
            indicators: AppIndicators {
                dirty: self.dirty,
                ..AppIndicators::default()
            },
            closable: true,
            tooltip: Some(
                self.recovery_status
                    .clone()
                    .unwrap_or_else(|| format!("Markdown · {}", self.canonical_uri)),
            ),
        }
    }

    fn prepare_close(&mut self, _request: CloseRequest, _cx: &mut UpdateCx<'_>) -> CloseReadiness {
        CloseReadiness::Ready
    }
}

fn markdown_action_index(action: &str, prefix: &str) -> Option<usize> {
    let suffix = action.strip_prefix(prefix)?;
    (!suffix.is_empty() && suffix.bytes().all(|byte| byte.is_ascii_digit()))
        .then(|| suffix.parse().ok())
        .flatten()
}

fn markdown_page_location(action: &str) -> Option<crate::native_markdown::MarkdownLocation> {
    let suffix = action.strip_prefix("markdown/page/")?;
    let (source, row) = suffix.split_once('/')?;
    if source.is_empty()
        || row.is_empty()
        || !source.bytes().all(|byte| byte.is_ascii_digit())
        || !row.bytes().all(|byte| byte.is_ascii_digit())
    {
        return None;
    }
    Some(crate::native_markdown::MarkdownLocation::new(
        source.parse().ok()?,
        row.parse().ok()?,
    ))
}

fn markdown_mode_from_action(action: &str) -> Option<MarkdownViewMode> {
    match action {
        "markdown/mode/preview" => Some(MarkdownViewMode::Preview),
        "markdown/mode/source" => Some(MarkdownViewMode::Source),
        "markdown/mode/split" => Some(MarkdownViewMode::Split),
        _ => None,
    }
}

fn markdown_range_from_action(action: &str) -> Option<std::ops::Range<usize>> {
    let suffix = action.strip_prefix("markdown/select-range/")?;
    let (start, end) = suffix.split_once('/')?;
    if !start.bytes().all(|byte| byte.is_ascii_digit())
        || !end.bytes().all(|byte| byte.is_ascii_digit())
    {
        return None;
    }
    Some(start.parse().ok()?..end.parse().ok()?)
}

/// Resolve only the link rows intersecting a materialized source window. Links
/// are parser-ordered, so the initial partition is logarithmic and the remainder
/// is capped by `limit` rather than total document size.
fn markdown_links_in_range(
    document: &crate::native_markdown::MarkdownDocument,
    source: std::ops::Range<usize>,
    limit: usize,
) -> Vec<usize> {
    if source.start >= source.end || limit == 0 {
        return Vec::new();
    }
    let start = document
        .links
        .partition_point(|link| link.source.end <= source.start);
    document
        .links
        .iter()
        .enumerate()
        .skip(start)
        .take_while(|(_, link)| link.source.start < source.end)
        .take(limit.min(16))
        .map(|(index, _)| index)
        .collect()
}

fn markdown_images_in_range(
    document: &crate::native_markdown::MarkdownDocument,
    source: std::ops::Range<usize>,
    limit: usize,
) -> Vec<usize> {
    if source.start >= source.end || limit == 0 {
        return Vec::new();
    }
    let start = document
        .images
        .partition_point(|image| image.source.end <= source.start);
    document
        .images
        .iter()
        .enumerate()
        .skip(start)
        .take_while(|(_, image)| image.source.start < source.end)
        .take(limit.min(8))
        .map(|(index, _)| index)
        .collect()
}

fn bounded_markdown_block_text(block: &crate::native_markdown::MarkdownBlock) -> String {
    use crate::native_markdown::MarkdownBlock;
    const LIMIT: usize = 128 * 1024;

    let source = match block {
        MarkdownBlock::Heading { text, .. }
        | MarkdownBlock::Paragraph { text, .. }
        | MarkdownBlock::ListItem { text, .. }
        | MarkdownBlock::Quote { text, .. } => return bounded_markdown_text(text, LIMIT),
        MarkdownBlock::CodeBlock { code, .. } => return bounded_markdown_text(code, LIMIT),
        MarkdownBlock::Table { .. } => None,
        MarkdownBlock::ThematicBreak { .. } => return String::new(),
    };
    if let Some(source) = source {
        return bounded_markdown_text(source, LIMIT);
    }
    let MarkdownBlock::Table { header, rows, .. } = block else {
        return String::new();
    };
    let mut output = String::new();
    for row in std::iter::once(header).chain(rows) {
        for (index, cell) in row.iter().enumerate() {
            if index > 0 {
                output.push_str("  │  ");
            }
            let remaining = LIMIT.saturating_sub(output.len());
            if remaining == 0 {
                break;
            }
            output.push_str(&bounded_markdown_text(cell, remaining));
        }
        if output.len() >= LIMIT {
            break;
        }
        output.push('\n');
    }
    if output.len() >= LIMIT {
        output.truncate(output.floor_char_boundary(LIMIT.saturating_sub('…'.len_utf8())));
        output.push('…');
    }
    output
}

/// The reader's own `command`-modifier chord for `key`, spelled the way THIS
/// platform's keyboard actually reaches it (audit I9).
///
/// The dispatch arm those commands come out of tests `SUPER | CTRL`
/// (`app_native`'s `command` flag), so both spellings are live on every
/// platform — but only one of them is the one a user reaches for, and printing
/// `Cmd-[` to a Windows user names the Windows key, which the shell owns.
fn reader_command_chord(key: &str) -> String {
    let modifier = if cfg!(target_os = "macos") {
        "Cmd-"
    } else {
        "Ctrl-"
    };
    format!("{modifier}{key}")
}

fn bounded_markdown_text(text: &str, limit: usize) -> String {
    if text.len() <= limit {
        return text.to_string();
    }
    let end = text.floor_char_boundary(limit.saturating_sub('…'.len_utf8()));
    let mut output = text[..end].to_string();
    output.push('…');
    output
}

/// Bounded visual label projection. Unlike byte caps used for clipboard and
/// semantic block materialization, chrome is sized in displayed characters so
/// compact layouts receive a deliberate ellipsis instead of relying on clipping.
fn bounded_markdown_label(text: &str, max_chars: usize) -> String {
    if max_chars == 0 {
        return String::new();
    }
    let Some((end, _)) = text.grapheme_indices().nth(max_chars) else {
        return text.to_string();
    };
    let mut output = text[..end].to_string();
    output.push('…');
    output
}

/// Width for a centered Markdown toolbar label using the exact UI-face
/// measurer and type step consumed by `native_ui` paint. The fixed historical
/// widths were correct only at 1x; at an accessibility scale of 2x they made
/// even "Preview" and "Source" visibly ellipsize in a wide reader.
fn markdown_toolbar_label_width(label: &str, minimum: f32, text_scale: f32) -> f32 {
    let scale = if text_scale.is_finite() && text_scale > 0.0 {
        text_scale.clamp(0.85, 2.0)
    } else {
        1.0
    };
    (crate::tray_raster::ui_text_width(label, 13.0 * scale) + 20.0)
        .ceil()
        .max(minimum)
}

/// Filename-like chrome keeps both identity-bearing ends. A long document title
/// should retain its extension and distinguishing prefix instead of relying on a
/// parent clip that can also cover the cursor/save indicators beside it.
fn bounded_middle_label(text: &str, max_chars: usize) -> String {
    bounded_middle_label_from_graphemes(&text.graphemes().collect::<Vec<_>>(), max_chars)
}

fn bounded_middle_label_from_graphemes(graphemes: &[&str], max_chars: usize) -> String {
    let count = graphemes.len();
    if count <= max_chars {
        return graphemes.concat();
    }
    if max_chars == 0 {
        return String::new();
    }
    if max_chars == 1 {
        return "…".to_string();
    }
    let head = (max_chars - 1).div_ceil(2);
    let tail = max_chars - 1 - head;
    let prefix = graphemes[..head].concat();
    let suffix = graphemes[count - tail..].concat();
    format!("{prefix}…{suffix}")
}

/// Fit filename-like chrome to a real pixel budget with the same face metrics
/// used to paint it. The bounded search keeps both identity-bearing ends and is
/// deliberately capped because filesystem titles are short while app-provided
/// titles are not required to be.
fn bounded_middle_label_to_width(text: &str, max_width: f32, px: f32) -> String {
    bounded_middle_label_to_width_for_face(text, max_width, px, crate::widget::TextFace::UiBold)
}

fn bounded_middle_label_to_width_for_face(
    text: &str,
    max_width: f32,
    px: f32,
    face: crate::widget::TextFace,
) -> String {
    use crate::widget::TextFace;

    debug_assert!(matches!(face, TextFace::Ui | TextFace::UiBold));
    let measure = |label: &str| crate::tray_raster::ui_text_width_for(face, label, px);
    if max_width <= 0.0 || !max_width.is_finite() || px <= 0.0 || !px.is_finite() {
        return String::new();
    }
    if measure(text) <= max_width {
        return text.to_string();
    }

    // 256 displayed graphemes is already far beyond any title that can fit in
    // native app chrome. Capping also prevents adversarial service titles from
    // turning a layout pass into an unbounded quadratic search.
    let graphemes = text.graphemes().collect::<Vec<_>>();
    let count = graphemes.len().min(256);
    for max_chars in (2..=count).rev() {
        let candidate = bounded_middle_label_from_graphemes(&graphemes, max_chars);
        if measure(&candidate) <= max_width {
            return candidate;
        }
    }
    if measure("…") <= max_width {
        "…".to_string()
    } else {
        String::new()
    }
}

/// Preserve the complete metadata-derived help in accessibility/status
/// semantics while expressing verbose platform constraints compactly in the
/// one-line visual strip. Every operational distinction remains visible.
fn config_visual_help(help: &str) -> String {
    help.replace(
        "GPU rendering (restart) · gpu · true / false · Applies next launch · ",
        "GPU rendering · ",
    )
    .replace("default 1.0 (solid)", "default 1.0")
    .replace(
        "macOS GPU window path only; CPU and non-macOS GPU grids stay solid",
        "macOS GPU; CPU/other GPU solid",
    )
    .replace(
        "translucent backgrounds enforce at least 4.5:1 text contrast",
        "translucency ≥4.5:1 contrast",
    )
    .replace(
        "the last --cpu/--gpu flag wins; inherited $ATERM_CPU otherwise wins over $ATERM_GPU; both override this value",
        "last --cpu/--gpu > $ATERM_CPU > $ATERM_GPU > config",
    )
}

/// Compact editor controller seam. The editor module owns commands/edit
/// reduction; the app runtime owns lifecycle, semantic presentation, and close
/// readiness around the shared `DocumentId`.
#[derive(Debug)]
pub(crate) struct EditorApp {
    pub(crate) document: DocumentId,
    pub(crate) title: String,
    pub(crate) base_title: String,
    pub(crate) canonical_uri: String,
    pub(crate) dirty: bool,
    pub(crate) checkpoint_pending: bool,
    /// The saver baseline no longer matches disk. Save stays unavailable until
    /// the host installs an explicitly reconciled observation.
    pub(crate) disk_conflict: bool,
    pub(crate) recovery_status: Option<String>,
    pub(crate) can_undo: bool,
    pub(crate) can_redo: bool,
    config_editor: bool,
    config_analysis: Option<crate::native_config_language::ConfigAnalysis>,
    config_analysis_revision: u64,
    /// Exact source/environment generation admitted to the off-thread config
    /// analysis lane.
    /// Keeping the latch beside the pure analysis makes repeat presentation
    /// refreshes idempotent and rejects stale worker completions by construction.
    config_host_requested_revision: Option<(u64, u64)>,
    config_assist_cache: std::cell::RefCell<
        Vec<(
            crate::native_config_language::ConfigCompletionContext,
            crate::native_config_language::ConfigAssist,
        )>,
    >,
    /// `(document, seq, line-start index)` for the last rendered revision — see
    /// [`EditorApp::line_index`]. Same memo shape as `config_assist_cache`, keyed
    /// on the same document revision, so staleness is structurally excluded.
    line_index_cache: std::cell::RefCell<LineIndexMemo>,
}

/// `(document id, document seq, line-start index)` — the memo behind
/// [`EditorApp::line_index`], keyed on the revision it was built from.
/// The inner `Option` is the DECISION, not a miss: `None` records that this
/// revision is too line-dense to index within
/// [`crate::native_editor::MAX_LINE_INDEX_ENTRIES`], so the refusal is memoized
/// too and a pathological document does not re-scan on every frame.
type LineIndexMemo = Option<(u64, u64, Option<std::sync::Arc<[usize]>>)>;

impl EditorApp {
    pub(crate) fn new(document: DocumentId, title: String) -> Self {
        Self::new_with_uri(
            document,
            title,
            format!("document:local/{}", document.get()),
        )
    }

    pub(crate) fn new_with_uri(document: DocumentId, title: String, canonical_uri: String) -> Self {
        Self {
            document,
            title: title.clone(),
            base_title: title,
            canonical_uri,
            dirty: false,
            checkpoint_pending: false,
            disk_conflict: false,
            recovery_status: None,
            can_undo: false,
            can_redo: false,
            config_editor: false,
            config_analysis: None,
            config_analysis_revision: 0,
            config_host_requested_revision: None,
            config_assist_cache: std::cell::RefCell::new(Vec::new()),
            line_index_cache: std::cell::RefCell::new(None),
        }
    }

    /// Line-start index for `snapshot`, built once per document REVISION.
    ///
    /// `view` runs on every frame and every keystroke, and it asks three separate
    /// questions that each used to re-derive the document's line structure from
    /// the bytes: `reconcile_viewport` (one full newline scan plus two
    /// caret-length scans), `project_viewport` (a second full scan), and the
    /// cursor label (another caret-length scan). That is four to six passes over
    /// the WHOLE document per frame, so editor frame time — and, since the
    /// reducer re-renders, typing latency — grew linearly with file size, up to
    /// ~150 ms/frame at the 32 MiB document limit. One pass per revision answers
    /// all of them (binary search / direct index thereafter).
    ///
    /// `None` means "do not index this revision" — see [`LineIndexMemo`]. Callers
    /// fall back to `EditorLines::scanning`, which answers identically.
    fn line_index(
        &self,
        snapshot: &crate::document_store::DocumentSnapshot,
    ) -> Option<std::sync::Arc<[usize]>> {
        let hit = self
            .line_index_cache
            .borrow()
            .as_ref()
            .filter(|(document, seq, _)| *document == snapshot.id.get() && *seq == snapshot.seq.0)
            .map(|(_, _, starts)| starts.clone());
        if let Some(decision) = hit {
            return decision;
        }
        let starts: Option<std::sync::Arc<[usize]>> =
            crate::native_editor::line_starts_capped(&snapshot.text).map(Into::into);
        *self.line_index_cache.borrow_mut() =
            Some((snapshot.id.get(), snapshot.seq.0, starts.clone()));
        starts
    }

    fn config_assist(
        &self,
        snapshot: &crate::document_store::DocumentSnapshot,
        caret: usize,
    ) -> Option<(
        crate::native_config_language::ConfigCompletionContext,
        crate::native_config_language::ConfigAssist,
    )> {
        if !self.config_editor
            || self.document != snapshot.id
            || self.config_analysis_revision != snapshot.seq.0
        {
            return None;
        }
        let analysis = self.config_analysis.as_ref()?;
        let context = crate::native_config_language::ConfigCompletionContext::new(
            snapshot.id.get(),
            snapshot.seq.0,
            caret,
        );
        if let Some(assist) = self
            .config_assist_cache
            .borrow()
            .iter()
            .find(|(cached, _)| *cached == context)
            .map(|(_, assist)| assist.clone())
        {
            return Some((context, assist));
        }
        let assist =
            crate::native_config_language::assist_with_analysis(&snapshot.text, caret, analysis);
        let mut cache = self.config_assist_cache.borrow_mut();
        if cache.len() >= 16 {
            cache.remove(0);
        }
        cache.push((context, assist.clone()));
        Some((context, assist))
    }
}

impl NativeAppModel for EditorApp {
    type ViewState = EditorViewState;

    fn descriptor(&self) -> AppDescriptor {
        AppDescriptor {
            kind: AppKind::Editor,
            name: "Editor",
            icon: AppIcon::Editor,
            singleton: false,
        }
    }

    fn update(
        &mut self,
        view: &mut Self::ViewState,
        event: AppEvent,
        _cx: &mut UpdateCx<'_>,
    ) -> EventResult {
        match event {
            AppEvent::FocusChanged(focus) => {
                view.common.last_focus = focus;
                EventResult::Handled
            }
            AppEvent::DocumentChanged { document, .. } if document == self.document => {
                if view
                    .status
                    .as_deref()
                    .is_some_and(|status| status.starts_with("Save blocked"))
                {
                    view.status = None;
                }
                view.config_completion_selected = 0;
                view.config_completion_interaction = None;
                view.config_completion_dismissed = None;
                view.config_diagnostic_selected = 0;
                view.common.presentation_revision =
                    view.common.presentation_revision.saturating_add(1);
                EventResult::Handled
            }
            AppEvent::EditorConfigNavigate {
                navigation,
                candidates,
                context,
            } if self.config_editor => {
                view.config_completion_selected = config_completion_selection_transition(
                    view.config_completion_selected,
                    candidates,
                    navigation,
                );
                view.config_completion_interaction = Some(context);
                view.config_completion_dismissed = None;
                view.common.last_focus = (candidates > 0).then(|| {
                    UiKey::new(format!(
                        "editor/config-completion/{}",
                        view.config_completion_selected
                    ))
                });
                view.common.presentation_revision =
                    view.common.presentation_revision.saturating_add(1);
                EventResult::Handled
            }
            AppEvent::EditorConfigDismiss { context } if self.config_editor => {
                view.config_completion_dismissed = Some(context);
                view.config_completion_interaction = None;
                view.common.last_focus = Some(UiKey::new("editor/buffer"));
                view.common.presentation_revision =
                    view.common.presentation_revision.saturating_add(1);
                EventResult::Handled
            }
            AppEvent::EditorConfigDiagnosticNavigate { previous } if self.config_editor => {
                let count = self.config_analysis.as_ref().map_or(
                    0,
                    crate::native_config_language::ConfigAnalysis::diagnostic_count,
                );
                view.config_diagnostic_selected = config_diagnostic_selection_transition(
                    view.config_diagnostic_selected,
                    count,
                    previous,
                );
                view.common.presentation_revision =
                    view.common.presentation_revision.saturating_add(1);
                EventResult::Handled
            }
            AppEvent::Action(invocation) if invocation.id.as_str() == "editor/focus-buffer" => {
                EventResult::Handled
            }
            _ => EventResult::Bubble,
        }
    }

    fn view(&self, view: &Self::ViewState, cx: &ViewCx<'_>) -> UiTree {
        use crate::native_ui::{
            ActionId, ButtonSpec, Control, ControlState, GroupSpec, Insets, Layout, Length,
            SemanticRole, StyleRef, TextViewportSpec, UiContent, UiKey, UiNode,
        };
        let command_completion = view.buffer.as_ref().and_then(|buffer| {
            let crate::native_editor::Minibuffer::Command { query, selected } = &buffer.minibuffer
            else {
                return None;
            };
            Some((
                query.clone(),
                crate::native_editor::command_completions(query),
                *selected,
            ))
        });
        let minibuffer_active = view
            .buffer
            .as_ref()
            .is_some_and(crate::native_editor::EditorBufferView::minibuffer_active);
        let config_assist = (!minibuffer_active
            && view
                .buffer
                .as_ref()
                .is_some_and(|buffer| !buffer.chord_pending()))
        .then(|| {
            let snapshot = cx.document?;
            let caret = view.buffer.as_ref()?.primary_selection().head;
            self.config_assist(snapshot, caret)
        })
        .flatten()
        .filter(|(context, _)| view.config_completion_dismissed != Some(*context))
        .filter(|(_, assist)| assist.help.is_some() || !assist.completions.is_empty());
        let completion_count = command_completion.as_ref().map_or_else(
            || {
                config_assist
                    .as_ref()
                    .map_or(0, |(_, assist)| assist.completions.len().max(1))
            },
            |(_, candidates, _)| candidates.len().max(1),
        );
        let shell = crate::native_ui::editor_shell_metrics(cx.viewport, completion_count);
        let outer_padding = shell.outer_padding;
        let compact_commands = shell.compact_commands;
        let narrow_problem_commands =
            cx.viewport.width < 900.0 * crate::native_appearance::text_scale().min(1.4);
        let command_gap = shell.command_gap;
        let command_bar_height = shell.command_bar_height;
        let editor_gap = shell.content_gap;
        let document_key = cx.document.map_or_else(
            || format!("document:{}", self.document.get()),
            |snapshot| format!("document:{}@{}", snapshot.id.get(), snapshot.seq.0),
        );
        let editor_rect =
            crate::native_ui::editor_text_viewport_rect_with_palette(cx.viewport, completion_count);
        let editor_geometry = crate::native_ui::text_viewport_geometry(editor_rect);
        let line_capacity = crate::native_ui::editor_visible_line_capacity_with_palette(
            cx.viewport,
            completion_count,
        );
        let column_capacity = ((editor_rect.right() - editor_geometry.text_x).max(1.0)
            / editor_geometry.cell_w)
            .floor() as usize;
        let mut projection = cx.document.and_then(|snapshot| {
            view.buffer.as_ref().map(|buffer| {
                // ONE line index per document revision serves the reconcile, the
                // projection, and the cursor label below (the second and third
                // asks are memo hits) — see `EditorApp::line_index`.
                let starts = self.line_index(snapshot);
                let lines = match starts.as_deref() {
                    Some(starts) => {
                        crate::native_editor::EditorLines::indexed(&snapshot.text, starts)
                    }
                    None => crate::native_editor::EditorLines::scanning(&snapshot.text),
                };
                // Runtime-only renderers do not have a host resize event. Use a
                // reconciled view clone so direct semantic/introspection renders
                // still obey the renderer's actual capacity; the window host
                // persists this same capacity before ordinary reducer input.
                let mut effective = buffer.clone();
                effective.reconcile_viewport_with(&lines, line_capacity);
                crate::native_editor::project_viewport_with(
                    &lines,
                    &effective,
                    line_capacity,
                    column_capacity,
                )
            })
        });
        if let (Some(projection), Some(analysis)) =
            (projection.as_mut(), self.config_analysis.as_ref())
        {
            crate::native_config_language::decorate_projection(projection, analysis);
        }
        let cursor_label = cx.document.and_then(|snapshot| {
            view.buffer.as_ref().map(|buffer| {
                let starts = self.line_index(snapshot);
                let lines = match starts.as_deref() {
                    Some(starts) => {
                        crate::native_editor::EditorLines::indexed(&snapshot.text, starts)
                    }
                    None => crate::native_editor::EditorLines::scanning(&snapshot.text),
                };
                editor_cursor_label_with(&lines, buffer)
            })
        });
        let footer_chars =
            (((editor_rect.width - 88.0).max(112.0) / 7.0).floor() as usize).clamp(16, 256);
        let minibuffer = view
            .buffer
            .as_ref()
            .and_then(|buffer| editor_minibuffer_label(buffer, &view.preedit, footer_chars));
        let semantic_status = editor_status_message(
            self,
            view,
            projection.as_ref(),
            cx.document.is_some_and(|snapshot| snapshot.text.is_empty()),
        );
        let status = semantic_status
            .as_deref()
            .map(|label| bounded_markdown_label(label, footer_chars));
        let document_preedit = if view
            .buffer
            .as_ref()
            .is_some_and(crate::native_editor::EditorBufferView::minibuffer_active)
        {
            String::new()
        } else {
            view.preedit.clone()
        };
        let title_chars = (((editor_rect.width
            - if self.checkpoint_pending {
                188.0
            } else {
                158.0
            })
        .max(84.0)
            / 7.0)
            .floor() as usize)
            .clamp(12, 96);
        let title = bounded_middle_label(&self.title, title_chars);
        let focused = view.common.focus_visible
            && view.common.last_focus.as_ref() == Some(&UiKey::new("editor/buffer"));
        let buffer_ready = view.buffer.is_some();
        let config_valid = !self.config_editor
            || self
                .config_analysis
                .as_ref()
                .is_some_and(|analysis| !analysis.has_errors());
        let config_diagnostic_count = if self.config_editor {
            self.config_analysis.as_ref().map_or(
                0,
                crate::native_config_language::ConfigAnalysis::diagnostic_count,
            )
        } else {
            0
        };
        let command_button = |key: &'static str,
                              visual_label: &'static str,
                              label: &'static str,
                              action: &'static str,
                              enabled: bool| {
            UiNode::new(
                key,
                UiContent::Button(
                    Control::new(
                        ButtonSpec::new(label).visual_label(visual_label),
                        ActionId::new(action),
                    )
                    .state(ControlState {
                        enabled,
                        busy: action == "editor/save" && self.checkpoint_pending,
                        ..ControlState::default()
                    })
                    .style(StyleRef::Secondary),
                ),
            )
            .layout(Layout::default().width(Length::Fill).height(Length::Fill))
        };
        let save = || {
            command_button(
                "editor/save-button",
                "Save",
                "Save buffer (Cmd-S or C-x C-s)",
                "editor/save",
                self.dirty && !self.checkpoint_pending && !self.disk_conflict && config_valid,
            )
        };
        let undo = || {
            command_button(
                "editor/undo-button",
                "Undo",
                "Undo (Cmd-Z or C-/)",
                "editor/undo",
                buffer_ready && self.can_undo,
            )
        };
        let redo = || {
            command_button(
                "editor/redo-button",
                "Redo",
                "Redo (Cmd-Shift-Z)",
                "editor/redo",
                buffer_ready && self.can_redo,
            )
        };
        let find = || {
            command_button(
                "editor/find-button",
                "Find",
                "Incremental search (C-s)",
                "editor/find",
                buffer_ready,
            )
        };
        let goto_line = || {
            command_button(
                "editor/goto-line-button",
                "Line",
                "Go to line (M-g g)",
                "editor/goto-line",
                buffer_ready,
            )
        };
        let commands = || {
            command_button(
                "editor/commands-button",
                "M-x",
                "Execute command (M-x)",
                "editor/commands",
                buffer_ready,
            )
        };
        let recovery_or_commands = || {
            if self.disk_conflict && self.dirty {
                command_button(
                    "editor/revert-button",
                    "Reload",
                    "Discard Changes and Reload from Disk",
                    "editor/revert",
                    buffer_ready && !self.checkpoint_pending,
                )
            } else {
                commands()
            }
        };
        let previous_problem = || {
            command_button(
                "editor/config-problem-previous-button",
                if compact_commands {
                    "‹!"
                } else if narrow_problem_commands {
                    "Prev"
                } else {
                    "Prev Issue"
                },
                "Previous config problem (Shift-F8)",
                "editor/config-problem-previous",
                config_diagnostic_count > 0,
            )
        };
        let next_problem = || {
            command_button(
                "editor/config-problem-next-button",
                if compact_commands {
                    "!›"
                } else if narrow_problem_commands {
                    "Next"
                } else {
                    "Next Issue"
                },
                "Next config problem (F8)",
                "editor/config-problem-next",
                config_diagnostic_count > 0,
            )
        };
        let command_bar = if compact_commands {
            let mut navigation_commands = vec![find(), goto_line(), recovery_or_commands()];
            if config_diagnostic_count > 0 {
                navigation_commands.extend([previous_problem(), next_problem()]);
            }
            UiNode::new(
                "editor/command-bar",
                UiContent::Group(GroupSpec::new("Editor commands")),
            )
            .layout(
                Layout::column()
                    .gap(command_gap)
                    .width(Length::Fill)
                    .height(Length::Fixed(command_bar_height)),
            )
            .children(vec![
                UiNode::new(
                    "editor/command-row-primary",
                    UiContent::Group(GroupSpec::unlabeled(SemanticRole::Group)),
                )
                .layout(
                    Layout::row()
                        .gap(command_gap)
                        .width(Length::Fill)
                        .height(Length::Fill),
                )
                .children(vec![save(), undo(), redo()]),
                UiNode::new(
                    "editor/command-row-navigation",
                    UiContent::Group(GroupSpec::unlabeled(SemanticRole::Group)),
                )
                .layout(
                    Layout::row()
                        .gap(command_gap)
                        .width(Length::Fill)
                        .height(Length::Fill),
                )
                .children(navigation_commands),
            ])
        } else {
            let mut commands = vec![
                save(),
                undo(),
                redo(),
                find(),
                goto_line(),
                recovery_or_commands(),
            ];
            if config_diagnostic_count > 0 {
                commands.extend([previous_problem(), next_problem()]);
            }
            UiNode::new(
                "editor/command-bar",
                UiContent::Group(GroupSpec::new("Editor commands")),
            )
            .layout(
                Layout::row()
                    .gap(command_gap)
                    .width(Length::Fill)
                    .height(Length::Fixed(command_bar_height)),
            )
            .children(commands)
        };
        let command_palette = command_completion.as_ref().and_then(
            |(query, candidates, selected)| {
                if shell.palette_visible_rows == 0 {
                    return None;
                }
                let selected = if candidates.is_empty() {
                    0
                } else {
                    (*selected).min(candidates.len() - 1)
                };
                let capacity = shell.palette_visible_rows.min(candidates.len().max(1));
                let start = if candidates.is_empty() {
                    0
                } else {
                    selected
                        .saturating_add(1)
                        .saturating_sub(capacity)
                        .min(candidates.len().saturating_sub(capacity))
                };
                let end = (start + capacity).min(candidates.len());
                let text_scale = crate::native_appearance::text_scale();
                let compact_label = cx.viewport.width < 480.0 * text_scale;
                let visual_width = (cx.viewport.width
                    - shell.outer_padding * 2.0
                    - shell.palette_padding * 2.0
                    - 20.0)
                    .max(0.0);
                let mut children = vec![
                    UiNode::new(
                        "editor/completion-status",
                        UiContent::Text(crate::native_ui::TextSpec {
                            text: command_palette_status(candidates.len(), compact_label),
                            role: SemanticRole::Status,
                            style: StyleRef::Quiet,
                        }),
                    )
                    .layout(
                        Layout::default()
                            .width(Length::Fill)
                            .height(Length::Fixed(shell.palette_header_height)),
                    ),
                ];
                if candidates.is_empty() {
                    children.push(
                        UiNode::new(
                            "editor/completion-empty",
                            UiContent::Text(crate::native_ui::TextSpec {
                                text: if compact_label {
                                    "No match · Esc".to_string()
                                } else {
                                    "No matching command — edit the query or press Esc"
                                        .to_string()
                                },
                                role: SemanticRole::Status,
                                style: StyleRef::Danger,
                            }),
                        )
                        .layout(
                            Layout::default()
                                .width(Length::Fill)
                                .height(Length::Fixed(shell.palette_row_height)),
                        ),
                    );
                } else {
                    children.extend(candidates[start..end]
                        .iter()
                        .enumerate()
                        .map(|(visible_index, command)| {
                            let index = start + visible_index;
                            let selected = index == selected;
                            let name = command.name();
                            let visual = bounded_middle_label_to_width(
                                name,
                                visual_width,
                                13.0 * text_scale,
                            );
                            UiNode::new(
                                format!("editor/completion/{index}"),
                                UiContent::Button(
                                    Control::new(
                                        ButtonSpec::new(format!(
                                            "Run command {name}, result {} of {}{}",
                                            index + 1,
                                            candidates.len(),
                                            if selected { ", selected" } else { "" },
                                        ))
                                        .visual_label(visual),
                                        ActionId::new(format!("editor/completion/{index}")),
                                    )
                                    .state(ControlState {
                                        selected,
                                        ..ControlState::default()
                                    })
                                    .style(if selected {
                                        StyleRef::Accent
                                    } else {
                                        StyleRef::Secondary
                                    }),
                                ),
                            )
                            .layout(
                                Layout::default()
                                    .width(Length::Fill)
                                    .height(Length::Fixed(shell.palette_row_height)),
                            )
                        }));
                }
                Some(
                    UiNode::new(
                        "editor/command-completions",
                        UiContent::Group(
                            GroupSpec::new(format!(
                                "Command completions for {}; use Up and Down to choose, Tab to complete, Enter to run, Escape to close",
                                bounded_markdown_text(query, 120),
                            ))
                            .style(StyleRef::Secondary),
                        ),
                    )
                    .layout(
                        Layout::column()
                            .padding(Insets::all(shell.palette_padding))
                            .gap(shell.palette_row_gap)
                            .width(Length::Fill)
                            .height(Length::Fixed(shell.palette_height))
                            .clipped(),
                    )
                    .children(children),
                )
            },
        );
        let config_palette = config_assist.as_ref().and_then(|(context, assist)| {
            if shell.palette_visible_rows == 0 {
                return None;
            }
            let text_scale = crate::native_appearance::text_scale();
            let available_width = (cx.viewport.width
                - shell.outer_padding * 2.0
                - shell.palette_padding * 2.0
                // TextSpec's visual status treatment owns additional optical
                // inset beyond the palette's structural padding. Keep the
                // width-bound copy inside that painted measure as well.
                - 72.0)
                .max(0.0);
            let help = assist
                .help
                .as_deref()
                .unwrap_or("Type a setting name for metadata-derived completions");
            let capacity = shell
                .palette_visible_rows
                .min(assist.completions.len().max(1));
            let total = assist.completions.len();
            let selected_for_context = if view.config_completion_interaction == Some(*context) {
                view.config_completion_selected
            } else {
                0
            };
            let selected = config_completion_selection_transition(
                selected_for_context,
                total,
                ConfigCompletionNavigation::Page(selected_for_context),
            );
            let window = config_completion_window(selected, total, capacity);
            let start = window.start;
            let end = window.end;
            let help_status = if total > capacity {
                format!(
                    "{help} · {}–{end} of {total} · Tab or Ctrl-Space choose · ↑/↓ move · Enter/Tab insert",
                    start + 1
                )
            } else if total > 0 {
                format!("{help} · Tab or Ctrl-Space choose · ↑/↓ move · Enter/Tab insert")
            } else {
                help.to_string()
            };
            // Keep the complete help in the status semantics, while the
            // visible one-line LSP strip uses compact, still-actionable key
            // guidance. Measure it with the exact regular/caption typography
            // that TextSpec paints; the title-oriented semibold helper can
            // disagree materially with the proportional regular face.
            let concise_help = config_visual_help(help);
            let visual_status = if total > capacity {
                format!(
                    "{concise_help} · {}–{end}/{total} · Ctrl-Space · ↑↓ select · Enter/Tab insert",
                    start + 1
                )
            } else if total > 0 {
                format!("{concise_help} · Ctrl-Space · ↑↓ select · Enter/Tab insert")
            } else {
                concise_help
            };
            let visual_help = bounded_middle_label_to_width_for_face(
                &visual_status,
                available_width,
                13.0 * text_scale,
                crate::widget::TextFace::Ui,
            );
            let mut children = vec![
                UiNode::new(
                    "editor/config-help",
                    UiContent::Group(GroupSpec {
                        label: Some(help_status),
                        role: SemanticRole::Status,
                        style: StyleRef::Quiet,
                    }),
                )
                .layout(
                    Layout::default()
                        .width(Length::Fill)
                        .height(Length::Fixed(shell.palette_header_height)),
                )
                .children(vec![
                    UiNode::new(
                        "editor/config-help/visual",
                        UiContent::Text(crate::native_ui::TextSpec {
                            text: visual_help,
                            role: SemanticRole::Text,
                            style: StyleRef::Quiet,
                        }),
                    )
                    .layout(Layout::default().width(Length::Fill).height(Length::Fill))
                    .paint_only(),
                ]),
            ];
            if assist.completions.is_empty() {
                children.push(
                    UiNode::new(
                        "editor/config-completion-empty",
                        UiContent::Text(crate::native_ui::TextSpec {
                            text: "Context help · no value completion at this caret".to_string(),
                            role: SemanticRole::Status,
                            style: StyleRef::Plain,
                        }),
                    )
                    .layout(
                        Layout::default()
                            .width(Length::Fill)
                            .height(Length::Fixed(shell.palette_row_height)),
                    ),
                );
            } else {
                let overflow = total > capacity;
                let navigation_width = shell.palette_row_height.max(34.0);
                let completion_width = (available_width
                    - if overflow {
                        navigation_width * 2.0 + shell.palette_row_gap * 2.0
                    } else {
                        0.0
                    })
                .max(0.0);
                for (visible_index, completion) in assist.completions[start..end].iter().enumerate()
                {
                    let index = start + visible_index;
                    let is_selected = index == selected;
                    let visual = bounded_middle_label_to_width(
                        &completion.display,
                        completion_width,
                        13.0 * text_scale,
                    );
                    let completion_node = UiNode::new(
                        format!("editor/config-completion/{index}"),
                        UiContent::Button(
                            Control::new(
                                ButtonSpec::new(format!(
                                    "Insert {}, result {} of {}{}. {}",
                                    completion.display,
                                    index + 1,
                                    total,
                                    if is_selected { ", selected" } else { "" },
                                    completion.help
                                ))
                                .visual_label(visual),
                                ActionId::new(
                                    crate::native_config_language::config_completion_action(
                                        *context, index, completion,
                                    ),
                                ),
                            )
                            .state(ControlState {
                                selected: is_selected,
                                ..ControlState::default()
                            })
                            .style(if is_selected {
                                StyleRef::Accent
                            } else {
                                StyleRef::Secondary
                            }),
                        ),
                    )
                    .layout(
                        Layout::default()
                            .width(Length::Fill)
                            .height(Length::Fixed(shell.palette_row_height)),
                    );
                    if overflow && visible_index == 0 {
                        let page_button =
                            |key: &'static str,
                             label: &'static str,
                             visual: &'static str,
                             target: usize,
                             enabled: bool| {
                                UiNode::new(
                                    key,
                                    UiContent::Button(
                                        Control::new(
                                            ButtonSpec::new(label).visual_label(visual),
                                            ActionId::new(format!(
                                                "editor/config-page/{target}/{total}"
                                            )),
                                        )
                                        .state(ControlState {
                                            enabled,
                                            ..ControlState::default()
                                        })
                                        .style(StyleRef::Quiet),
                                    ),
                                )
                                .layout(
                                    Layout::default()
                                        .width(Length::Fixed(navigation_width))
                                        .height(Length::Fixed(shell.palette_row_height)),
                                )
                            };
                        children.push(
                            UiNode::new(
                                "editor/config-completion-window",
                                UiContent::Group(GroupSpec::new(format!(
                                    "Config completions {} through {end} of {total}",
                                    start + 1
                                ))),
                            )
                            .layout(
                                Layout::row()
                                    .height(Length::Fixed(shell.palette_row_height))
                                    .gap(shell.palette_row_gap),
                            )
                            .children(vec![
                                page_button(
                                    "editor/config-page-previous",
                                    "Previous config completions",
                                    "‹",
                                    start.saturating_sub(capacity),
                                    start > 0,
                                ),
                                completion_node,
                                page_button(
                                    "editor/config-page-next",
                                    "Next config completions",
                                    "›",
                                    end.min(total.saturating_sub(1)),
                                    end < total,
                                ),
                            ]),
                        );
                    } else {
                        children.push(completion_node);
                    }
                }
            }
            Some(
                UiNode::new(
                    "editor/config-assist",
                    UiContent::Group(
                        GroupSpec::new("aterm.toml contextual help and completions")
                            .style(StyleRef::Secondary),
                    ),
                )
                .layout(
                    Layout::column()
                        .padding(Insets::all(shell.palette_padding))
                        .gap(shell.palette_row_gap)
                        .width(Length::Fill)
                        .height(Length::Fixed(shell.palette_height))
                        .clipped(),
                )
                .children(children),
            )
        });
        let mut children = vec![command_bar];
        if let Some(command_palette) = command_palette {
            children.push(command_palette);
        } else if let Some(config_palette) = config_palette {
            children.push(config_palette);
        }
        children.push(
            UiNode::new(
                "editor/buffer",
                UiContent::TextViewport(TextViewportSpec {
                    label: title,
                    document_key,
                    selectable: true,
                    projection,
                    preedit: document_preedit,
                    status,
                    semantic_status,
                    minibuffer,
                    cursor_label,
                    dirty: self.dirty,
                    saving: self.checkpoint_pending,
                    focused,
                    action: Some(ActionId::new("editor/focus-buffer")),
                }),
            )
            .layout(Layout::default().width(Length::Fill).height(Length::Fill)),
        );
        UiTree::new(
            UiNode::new(
                "editor/app",
                UiContent::Group(GroupSpec::unlabeled(SemanticRole::Application)),
            )
            .layout(
                Layout::column()
                    .padding(Insets::all(outer_padding))
                    .gap(editor_gap)
                    .clipped(),
            )
            .children(children),
        )
    }

    fn commands(&self, view: &Self::ViewState, out: &mut Vec<Command>) {
        let buffer_ready = view.buffer.is_some();
        let config_valid = !self.config_editor
            || self
                .config_analysis
                .as_ref()
                .is_some_and(|analysis| !analysis.has_errors());
        out.extend([
            Command {
                id: ActionId::new("editor/save"),
                title: "Save Buffer".to_string(),
                // AUDIT I9. The `Cmd-S` half is macOS's and `platform_accel`
                // strips it elsewhere; `C-x C-s` is emacs's and true everywhere.
                // On Windows the editor also seeds the reflex every Windows app
                // owes its user — plain Ctrl+S — see `Keymap::emacs`.
                shortcut: Some(
                    if cfg!(windows) {
                        "C-s \u{b7} C-x C-s"
                    } else {
                        "Cmd-S \u{b7} C-x C-s"
                    }
                    .to_string(),
                ),
                enabled: self.dirty
                    && !self.checkpoint_pending
                    && !self.disk_conflict
                    && config_valid,
            },
            Command {
                id: ActionId::new("editor/undo"),
                title: "Undo".to_string(),
                shortcut: Some("Cmd-Z · C-/".to_string()),
                enabled: buffer_ready && self.can_undo,
            },
            Command {
                id: ActionId::new("editor/redo"),
                title: "Redo".to_string(),
                shortcut: Some("Cmd-Shift-Z".to_string()),
                enabled: buffer_ready && self.can_redo,
            },
            Command {
                id: ActionId::new("editor/find"),
                title: "Incremental Search".to_string(),
                // Windows spends `C-s` on Save (the reflex it cannot not have),
                // so isearch answers to `M-s` there — emacs's own search-map
                // prefix letter, free in this keymap. Everywhere else `C-s` is
                // isearch, exactly as emacs users expect.
                shortcut: Some(if cfg!(windows) { "M-s" } else { "C-s" }.to_string()),
                enabled: buffer_ready,
            },
            Command {
                id: ActionId::new("editor/goto-line"),
                title: "Go to Line".to_string(),
                shortcut: Some("M-g g".to_string()),
                enabled: buffer_ready,
            },
            Command {
                id: ActionId::new("editor/commands"),
                title: "Execute Command…".to_string(),
                shortcut: Some("M-x".to_string()),
                enabled: buffer_ready,
            },
            Command {
                id: ActionId::new("editor/revert"),
                title: "Discard Changes and Reload from Disk".to_string(),
                shortcut: None,
                enabled: self.dirty && !self.checkpoint_pending,
            },
        ]);
        let diagnostic_count = self.config_analysis.as_ref().map_or(
            0,
            crate::native_config_language::ConfigAnalysis::diagnostic_count,
        );
        if self.config_editor && diagnostic_count > 0 {
            out.extend([
                Command {
                    id: ActionId::new("editor/config-problem-next"),
                    title: "Config: Next Problem".to_string(),
                    shortcut: Some("F8".to_string()),
                    enabled: diagnostic_count > 0,
                },
                Command {
                    id: ActionId::new("editor/config-problem-previous"),
                    title: "Config: Previous Problem".to_string(),
                    shortcut: Some("Shift-F8".to_string()),
                    enabled: diagnostic_count > 0,
                },
            ]);
        }
    }

    fn presentation(&self, _view: &Self::ViewState) -> AppPresentation {
        AppPresentation {
            title: self.title.clone(),
            icon: AppIcon::Editor,
            indicators: AppIndicators {
                dirty: self.dirty,
                busy: self.checkpoint_pending,
                attention: self.recovery_status.is_some(),
            },
            closable: !self.checkpoint_pending,
            tooltip: Some(
                self.recovery_status
                    .clone()
                    .unwrap_or_else(|| format!("Editor · {}", self.canonical_uri)),
            ),
        }
    }

    fn prepare_close(&mut self, _request: CloseRequest, _cx: &mut UpdateCx<'_>) -> CloseReadiness {
        // Document durability is owned by `DocumentStore` + the document host,
        // because one canonical document can be shared by Markdown and editor
        // views. An app-local dirty check cannot know whether this is the final
        // view and previously (incorrectly) reused the updater service as a save
        // operation. The host performs the proof-gated final-view checkpoint
        // before it commits any detach.
        CloseReadiness::Ready
    }
}

fn editor_minibuffer_label(
    view: &crate::native_editor::EditorBufferView,
    preedit: &str,
    max_chars: usize,
) -> Option<String> {
    use crate::native_editor::Minibuffer;

    if let Some(prefix) = view.prefix_hud.as_ref() {
        return Some(bounded_markdown_label(prefix, max_chars));
    }
    match &view.minibuffer {
        Minibuffer::Inactive => None,
        Minibuffer::Command { query, .. } => {
            Some(editor_prompt_label("M-x ", query, preedit, max_chars))
        }
        Minibuffer::Search { query, .. } => {
            Some(editor_prompt_label("I-search: ", query, preedit, max_chars))
        }
        Minibuffer::Buffer { query } => Some(editor_prompt_label(
            "Switch buffer: ",
            query,
            preedit,
            max_chars,
        )),
        Minibuffer::GotoLine { query, .. } => Some(editor_prompt_label(
            "Goto line: ",
            query,
            preedit,
            max_chars,
        )),
        Minibuffer::Message(message) => Some(bounded_markdown_label(message, max_chars)),
    }
}

fn editor_prompt_label(prefix: &str, query: &str, preedit: &str, max_chars: usize) -> String {
    let body = format!("{query}{preedit}").replace(['\r', '\n'], " ");
    let prefix_chars = prefix.graphemes().count();
    let body_graphemes = body.graphemes().collect::<Vec<_>>();
    let body_chars = body_graphemes.len();
    if prefix_chars.saturating_add(body_chars) <= max_chars {
        return format!("{prefix}{body}");
    }
    let tail_chars = max_chars.saturating_sub(prefix_chars.saturating_add(1));
    if tail_chars == 0 {
        return bounded_markdown_label(prefix, max_chars);
    }
    let tail = body_graphemes[body_chars - tail_chars..].concat();
    format!("{prefix}…{tail}")
}

fn editor_status_message(
    app: &EditorApp,
    view: &EditorViewState,
    projection: Option<&crate::native_editor::EditorViewportProjection>,
    empty: bool,
) -> Option<String> {
    let lines = projection.map_or(1, |projection| projection.total_lines.max(1));
    let line_noun = if lines == 1 { "line" } else { "lines" };
    let label = if app.disk_conflict {
        let recovery = app
            .recovery_status
            .as_deref()
            .filter(|status| status.contains("Save is blocked"))
            .unwrap_or(
                "File changed on disk; Save is blocked. Copy any local edits you need to keep, then choose ‘Discard Changes and Reload from Disk’",
            );
        format!("Recovery · {recovery}")
    } else if let Some(recovery) = app.recovery_status.as_deref() {
        format!("Recovery · {recovery}")
    } else if app.checkpoint_pending {
        "Saving checkpoint...".to_string()
    } else if app.config_editor && app.config_analysis.is_none() {
        "Validating aterm.toml…".to_string()
    } else if let Some(blocked) = view
        .status
        .as_deref()
        .filter(|status| status.starts_with("Save blocked"))
    {
        blocked.to_string()
    } else if let Some(summary) = app
        .config_analysis
        .as_ref()
        .and_then(|analysis| analysis.summary_at(view.config_diagnostic_selected))
    {
        summary
    } else if let Some(status) = view.status.as_deref().filter(|status| !status.is_empty()) {
        status.to_string()
    } else if app.dirty {
        format!("Modified · Cmd-S to save · {lines} {line_noun}")
    } else if empty {
        "Empty buffer · Ready".to_string()
    } else {
        format!("{lines} {line_noun} · Ready")
    };
    let watcher = app
        .config_editor
        .then_some(view.config_watch_status.as_deref())
        .flatten()
        .filter(|status| !status.is_empty());
    Some(match watcher {
        Some(watcher) => format!("{label} · Reload warning · {watcher}"),
        None => label,
    })
}

fn command_palette_status(matches: usize, compact: bool) -> String {
    if compact && matches == 0 {
        return "M-x · no matches".to_string();
    }
    let noun = if matches == 1 { "match" } else { "matches" };
    if compact {
        format!("M-x · {matches} {noun}")
    } else {
        format!("M-x · {matches} {noun} · ↑/↓ choose · Tab complete · Enter run · Esc close")
    }
}

/// Scanning form: derives the caret's line by walking the document. Kept for
/// one-shot callers; the render path uses [`editor_cursor_label_with`] with the
/// resident line index instead of re-scanning every frame.
#[cfg(test)]
fn editor_cursor_label(text: &str, view: &crate::native_editor::EditorBufferView) -> String {
    editor_cursor_label_with(&crate::native_editor::EditorLines::scanning(text), view)
}

fn editor_cursor_label_with(
    lines: &crate::native_editor::EditorLines<'_>,
    view: &crate::native_editor::EditorBufferView,
) -> String {
    let text = lines.text();
    let caret = view.primary_selection().head.min(text.len());
    let caret = (0..=caret)
        .rev()
        .find(|candidate| text.is_char_boundary(*candidate))
        .unwrap_or(0);
    let before = &text[..caret];
    let line = lines.number_at(caret) + 1;
    let line_start = before.rfind('\n').map_or(0, |newline| newline + 1);
    // Give the shared editor geometry the complete logical line. A defensive
    // selection can arrive on a UTF-8 character boundary inside a combining
    // or ZWJ cluster; slicing at the caret would make that partial cluster
    // look complete and overstate the column. The canonical helper instead
    // stops before the enclosing grapheme and applies the same tab/CJK/emoji
    // widths used by projection, paint, pointer hit testing, and motion.
    let line_end = text[caret..]
        .find('\n')
        .map_or(text.len(), |relative| caret + relative);
    let column = crate::native_editor::editor_display_column(
        &text[line_start..line_end],
        caret - line_start,
        0,
    ) + 1;
    format!("Ln {line}, Col {column}")
}

#[cfg(test)]
mod markdown_reader_tests {
    use super::*;
    use crate::native_ui::{
        ButtonIcon, MarkdownBlockKind, MarkdownBlockSpec, TextViewportSpec, UiContent,
    };
    use aterm_grapheme::GraphemeClusters;

    fn command<'a>(commands: &'a [Command], id: &str) -> &'a Command {
        commands
            .iter()
            .find(|command| command.id.as_str() == id)
            .unwrap_or_else(|| panic!("missing command {id}"))
    }

    #[test]
    fn manual_host_diagnostics_are_exact_revision_latest_and_idempotent() {
        let mut documents = crate::document_store::DocumentStore::new();
        let document = documents.open(
            "file:///tmp/aterm.toml".to_string(),
            "theme = \"Default\"\n".to_string(),
        );
        let mut runtime = NativeRuntime::new();
        let instance = runtime
            .insert_instance(NativeApp::Editor(EditorApp::new(
                document,
                "aterm.toml".to_string(),
            )))
            .unwrap();
        let view = ViewId::from_stored(7);
        runtime
            .attach_view(view, instance, AppViewState::Editor(Box::default()))
            .unwrap();
        assert!(runtime.enable_config_editor(document, "theme = \"Default\"\n", 7));
        assert!(runtime.begin_config_host_analysis(document, 7, 3));
        assert!(!runtime.begin_config_host_analysis(document, 7, 3));

        let warning = crate::native_config_language::ConfigDiagnostic {
            bytes: 8..17,
            line: 1,
            column: 9,
            severity: crate::native_config_language::ConfigDiagnosticSeverity::Warning,
            message: "configured theme is unavailable".to_string(),
        };
        let mut analysis = crate::native_config_language::analyze("theme = \"Default\"\n");
        assert!(analysis.merge_host_diagnostics(vec![warning.clone()]));
        assert!(
            !runtime.finish_config_host_analysis(document, 6, 3, analysis.clone()),
            "an older worker completion must not alter current editor assistance"
        );
        assert!(runtime.finish_config_host_analysis(document, 7, 3, analysis.clone()));
        assert!(
            !runtime.finish_config_host_analysis(document, 7, 3, analysis),
            "a replayed exact completion must not duplicate diagnostics"
        );

        runtime.publish_document(document, "theme = \"Nord\"\n", 8, true);
        assert!(runtime.begin_config_host_analysis(document, 8, 3));
        let valid = crate::native_config_language::analyze("theme = \"Nord\"\n");
        assert!(runtime.finish_config_host_analysis(document, 8, 3, valid.clone()));
        assert!(runtime.config_editor_save_error(document).is_none());
        assert!(
            runtime.begin_config_host_analysis(document, 8, 4),
            "same source revision is rechecked after the host environment advances"
        );
        assert!(
            runtime
                .config_editor_save_error(document)
                .is_some_and(|message| message.contains("still in progress")),
            "the previous host generation cannot authorize Save during revalidation"
        );
        assert!(
            !command(&runtime.commands(instance, view).unwrap(), "editor/save").enabled,
            "the visible command face must agree with the fail-closed host gate"
        );
        assert!(
            !runtime.finish_config_host_analysis(document, 8, 3, valid.clone()),
            "the old generation remains stale even though the document revision matches"
        );
        assert!(runtime.config_editor_save_error(document).is_some());
        assert!(runtime.finish_config_host_analysis(document, 8, 4, valid));
        assert!(runtime.config_editor_save_error(document).is_none());
        assert!(command(&runtime.commands(instance, view).unwrap(), "editor/save").enabled);
        assert!(runtime.enable_config_editor(document, "theme = \"Nord\n", 9));
        assert!(runtime.begin_config_host_analysis(document, 9, 4));
        let invalid = crate::native_config_language::analyze("theme = \"Nord\n");
        assert!(invalid.has_errors());
        assert!(runtime.finish_config_host_analysis(document, 9, 4, invalid));
    }

    #[test]
    fn manual_host_diagnostic_replacement_at_capacity_reports_presentation_damage() {
        let mut documents = crate::document_store::DocumentStore::new();
        let document = documents.open(
            "file:///tmp/aterm-capped.toml".to_string(),
            "theme = \"Default\"\n".to_string(),
        );
        let mut runtime = NativeRuntime::new();
        let instance = runtime
            .insert_instance(NativeApp::Editor(EditorApp::new(
                document,
                "aterm.toml".to_string(),
            )))
            .unwrap();
        assert!(runtime.enable_config_editor(document, "theme = \"Default\"\n", 11));
        let mut analysis = crate::native_config_language::analyze("theme = \"Default\"\n");
        analysis.diagnostics = (0..32)
            .map(|index| crate::native_config_language::ConfigDiagnostic {
                bytes: 0..1,
                line: 1,
                column: 1,
                severity: crate::native_config_language::ConfigDiagnosticSeverity::Warning,
                message: format!("pure warning {index}"),
            })
            .collect();
        assert_eq!(analysis.diagnostics.len(), 32);
        assert!(runtime.begin_config_host_analysis(document, 11, 1));
        assert!(runtime.finish_config_host_analysis(document, 11, 1, analysis.clone()));
        let host = crate::native_config_language::ConfigDiagnostic {
            bytes: 8..17,
            line: 1,
            column: 9,
            severity: crate::native_config_language::ConfigDiagnosticSeverity::Error,
            message: "configured theme asset is invalid".to_string(),
        };
        assert!(analysis.merge_host_diagnostics(vec![host.clone()]));
        assert!(runtime.begin_config_host_analysis(document, 11, 2));

        assert!(
            runtime.finish_config_host_analysis(document, 11, 2, analysis.clone()),
            "a late host error must evict one capped warning and invalidate presentation"
        );
        let NativeApp::Editor(editor) = runtime.instances.get(&instance).unwrap() else {
            panic!("Manual editor instance");
        };
        let diagnostics = &editor.config_analysis.as_ref().unwrap().diagnostics;
        assert_eq!(diagnostics.len(), 32);
        assert!(diagnostics.contains(&host));
        assert!(
            !runtime.finish_config_host_analysis(document, 11, 2, analysis),
            "replayed completion remains presentation-inert"
        );
    }

    #[test]
    fn canonical_document_identity_disambiguates_duplicate_basenames_without_splitting_one_uri() {
        let mut documents = crate::document_store::DocumentStore::new();
        let one_uri = "file:///Users//alice/one/README.md";
        let two_uri = "file:///Users//alice/two/README.md";
        let one = documents.open(one_uri.to_string(), "one".to_string());
        let two = documents.open(two_uri.to_string(), "two".to_string());
        let mut runtime = NativeRuntime::new();
        let markdown_one = runtime
            .insert_instance(NativeApp::Markdown(MarkdownApp::new_with_uri(
                one,
                "README.md".to_string(),
                one_uri.to_string(),
                "one",
            )))
            .unwrap();
        let editor_one = runtime
            .insert_instance(NativeApp::Editor(EditorApp::new_with_uri(
                one,
                "README.md".to_string(),
                one_uri.to_string(),
            )))
            .unwrap();
        let editor_two = runtime
            .insert_instance(NativeApp::Editor(EditorApp::new_with_uri(
                two,
                "README.md".to_string(),
                two_uri.to_string(),
            )))
            .unwrap();
        let views = [
            (ViewId::from_stored(101), markdown_one, AppKind::Markdown),
            (ViewId::from_stored(102), editor_one, AppKind::Editor),
            (ViewId::from_stored(103), editor_two, AppKind::Editor),
        ];
        for (view, instance, kind) in views {
            let state = match kind {
                AppKind::Markdown => AppViewState::Markdown(MarkdownViewState::default()),
                AppKind::Editor => AppViewState::Editor(Box::default()),
                AppKind::Settings | AppKind::Recovery => unreachable!(),
            };
            runtime.attach_view(view, instance, state).unwrap();
        }

        let changed = runtime.disambiguate_document_titles();
        assert_eq!(changed.len(), 3);
        let identities = runtime.document_identities();
        for (_, _, title, uri) in &identities {
            if uri == one_uri {
                assert_eq!(title, "README.md — one");
            } else if uri == two_uri {
                assert_eq!(title, "README.md — two");
            } else {
                panic!("unexpected canonical URI {uri}");
            }
        }
        assert_eq!(
            identities
                .iter()
                .filter(|(_, _, _, uri)| uri == one_uri)
                .count(),
            2,
            "Markdown and Editor views of one canonical URI are not false duplicates"
        );
        assert!(runtime.disambiguate_document_titles().is_empty());
        for (view, instance, kind) in views {
            let presentation = runtime.presentation(instance, view).unwrap();
            assert_eq!(
                presentation.tooltip.as_deref(),
                Some(match kind {
                    AppKind::Markdown => "Markdown · file:///Users//alice/one/README.md",
                    AppKind::Editor if instance == editor_one => {
                        "Editor · file:///Users//alice/one/README.md"
                    }
                    AppKind::Editor => "Editor · file:///Users//alice/two/README.md",
                    AppKind::Settings | AppKind::Recovery => unreachable!(),
                })
            );
        }

        runtime.remove_view(ViewId::from_stored(103));
        runtime.remove_instance(editor_two);
        let reverted = runtime.disambiguate_document_titles();
        assert_eq!(reverted.len(), 2);
        assert!(
            runtime
                .document_identities()
                .iter()
                .all(|(_, _, title, uri)| uri != one_uri || title == "README.md")
        );
        assert_eq!(runtime.document_id(markdown_one), Some(one));
        assert_eq!(runtime.document_id(editor_one), Some(one));
    }

    #[test]
    fn duplicate_title_suffix_is_shortest_unique_unicode_parent_and_bounded() {
        let group = vec![
            (
                AppInstanceId::from_stored(1),
                "file:///tmp/%E6%97%A5%E6%9C%AC/notes.md".to_string(),
            ),
            (
                AppInstanceId::from_stored(2),
                "file:///tmp/other/notes.md".to_string(),
            ),
        ];
        assert_eq!(shortest_unique_document_parent(&group[0].1, &group), "日本");
        assert_eq!(
            shortest_unique_document_parent(&group[1].1, &group),
            "other"
        );
        let long = "a".repeat(200);
        let bounded = bounded_document_parent_label(&long, "file:///long/notes.md");
        assert!(bounded.graphemes().count() <= 44);
        assert!(bounded.contains('…'));
    }

    fn markdown_runtime(source: &str) -> (NativeRuntime, AppInstanceId, ViewId) {
        let mut runtime = NativeRuntime::new();
        let mut documents = crate::document_store::DocumentStore::new();
        let document = documents.open("file:///Guide.md".to_string(), source.to_string());
        let instance = runtime
            .insert_instance(NativeApp::Markdown(MarkdownApp::new(
                document,
                "Guide.md".to_string(),
                source,
            )))
            .unwrap();
        let view = ViewId::from_stored(1);
        runtime
            .attach_view(
                view,
                instance,
                AppViewState::Markdown(MarkdownViewState::default()),
            )
            .unwrap();
        (runtime, instance, view)
    }

    #[test]
    fn exhaustive_vtable_and_view_lifecycle_cover_runtime_surfaces() {
        let (mut runtime, instance, view) = markdown_runtime("# Guide\n");
        let table = runtime.app(instance).unwrap().vtable();
        assert_eq!(table.kind, AppKind::Markdown);
        assert_eq!(table.restore_tag, "markdown");
        assert_eq!(
            runtime.view_lifecycle(view),
            Some(crate::front_content::ViewLifecycle::Mounted)
        );
        runtime.set_view_suspended(view, true).unwrap();
        assert_eq!(
            runtime.view_lifecycle(view),
            Some(crate::front_content::ViewLifecycle::Suspended)
        );
        runtime.set_view_suspended(view, false).unwrap();
        let _ = runtime.take_view_state(view).unwrap();
        assert_eq!(
            runtime.view_lifecycle(view),
            Some(crate::front_content::ViewLifecycle::Closed)
        );
        assert_eq!(
            runtime.set_view_suspended(view, false),
            Err(RuntimeError::UnknownView(view))
        );
    }

    #[test]
    fn markdown_navigation_metadata_drives_real_per_view_history() {
        let source = (0..12)
            .map(|index| format!("Paragraph {index}.\n\n"))
            .collect::<String>();
        let (mut runtime, instance, view) = markdown_runtime(&source);

        let initial = runtime.commands(instance, view).unwrap();
        assert!(!command(&initial, "markdown/back").enabled);
        assert!(!command(&initial, "markdown/forward").enabled);
        assert!(command(&initial, "markdown/select-all").enabled);
        assert!(!command(&initial, "markdown/copy").enabled);
        runtime
            .dispatch(instance, view, AppEvent::ScrollLines(3))
            .unwrap();
        let after_scroll = runtime.commands(instance, view).unwrap();
        assert!(
            command(&after_scroll, "markdown/back").enabled,
            "Back becomes actionable"
        );
        assert!(
            !command(&after_scroll, "markdown/forward").enabled,
            "Forward has no future entry yet"
        );

        runtime
            .dispatch(
                instance,
                view,
                AppEvent::Action(ActionInvocation {
                    id: ActionId::new("markdown/back"),
                    value: None,
                }),
            )
            .unwrap();
        let state = match runtime.view_state(view).unwrap() {
            AppViewState::Markdown(state) => state,
            _ => panic!("Markdown view changed kind"),
        };
        assert_eq!(state.source_anchor, 0);
        let after_back = runtime.commands(instance, view).unwrap();
        assert!(!command(&after_back, "markdown/back").enabled);
        assert!(
            command(&after_back, "markdown/forward").enabled,
            "Forward replays the visited anchor"
        );
    }

    #[test]
    fn thousand_line_paragraph_reaches_tail_in_preview_source_split_and_after_resize() {
        let source = (0..1_000)
            .map(|line| format!("line-{line:04} carries readable paragraph words\n"))
            .collect::<String>();
        let mut documents = crate::document_store::DocumentStore::new();
        let document = documents.open("file:///Long.md".to_string(), source.clone());
        let snapshot = documents.snapshot(document).unwrap();
        let mut runtime = NativeRuntime::new();
        let instance = runtime
            .insert_instance(NativeApp::Markdown(MarkdownApp::new(
                document,
                "Long.md".to_string(),
                &source,
            )))
            .unwrap();
        let view = ViewId::from_stored(1);
        runtime
            .attach_view(
                view,
                instance,
                AppViewState::Markdown(MarkdownViewState::default()),
            )
            .unwrap();

        let page = |runtime: &mut NativeRuntime, height: f32| {
            runtime
                .dispatch(
                    instance,
                    view,
                    AppEvent::MarkdownPage {
                        direction: 1,
                        viewport_width: 320.0,
                        viewport_height: height,
                    },
                )
                .unwrap();
        };
        page(&mut runtime, 320.0);
        let short_page_row = match runtime.view_state(view).unwrap() {
            AppViewState::Markdown(state) => state.visual_row,
            _ => unreachable!(),
        };
        runtime
            .dispatch(
                instance,
                view,
                AppEvent::MarkdownScroll {
                    lines: -10_000,
                    viewport_width: 320.0,
                    viewport_height: 568.0,
                },
            )
            .unwrap();
        page(&mut runtime, 568.0);
        let tall_page_row = match runtime.view_state(view).unwrap() {
            AppViewState::Markdown(state) => state.visual_row,
            _ => unreachable!(),
        };
        assert!(tall_page_row > short_page_row, "page size tracks resize");

        runtime
            .dispatch(
                instance,
                view,
                AppEvent::MarkdownScroll {
                    lines: 10_000,
                    viewport_width: 320.0,
                    viewport_height: 568.0,
                },
            )
            .unwrap();
        let preview_row = match runtime.view_state(view).unwrap() {
            AppViewState::Markdown(state) => state.visual_row,
            _ => unreachable!(),
        };
        assert!(
            preview_row > 100,
            "single paragraph has a reachable visual tail"
        );

        runtime
            .dispatch(
                instance,
                view,
                AppEvent::Action(ActionInvocation {
                    id: ActionId::new("markdown/mode/source"),
                    value: None,
                }),
            )
            .unwrap();
        runtime
            .dispatch(
                instance,
                view,
                AppEvent::MarkdownScroll {
                    lines: 10_000,
                    viewport_width: 320.0,
                    viewport_height: 568.0,
                },
            )
            .unwrap();
        let viewport = LogicalRect::new(0.0, 0.0, 320.0, 568.0);
        let source_ui = runtime
            .render(
                instance,
                view,
                &ViewCx {
                    viewport,
                    config_revision: 1,
                    update_revision: 1,
                    animation_phase_ms: 0,
                    motion: ViewMotionCx::default(),
                    terminal_font_px: 12.0,
                    terminal_theme: aterm_render::Theme::default(),
                    semantic_font: None,
                    document: Some(&snapshot),
                },
            )
            .unwrap()
            .compile(viewport)
            .unwrap();
        assert!(source_ui.paint.iter().any(|node| {
            matches!(&node.content, UiContent::MarkdownBlock(spec)
                if node.key.as_str() == "markdown/source"
                    && spec.text.contains("line-0999"))
        }));

        runtime
            .dispatch(
                instance,
                view,
                AppEvent::Action(ActionInvocation {
                    id: ActionId::new("markdown/mode/split"),
                    value: None,
                }),
            )
            .unwrap();
        runtime
            .dispatch(
                instance,
                view,
                AppEvent::MarkdownScroll {
                    lines: -10_000,
                    viewport_width: 800.0,
                    viewport_height: 320.0,
                },
            )
            .unwrap();
        for _ in 0..8 {
            runtime
                .dispatch(
                    instance,
                    view,
                    AppEvent::MarkdownPage {
                        direction: 1,
                        viewport_width: 800.0,
                        viewport_height: 320.0,
                    },
                )
                .unwrap();
        }
        let split_state = match runtime.view_state(view).unwrap() {
            AppViewState::Markdown(state) => state,
            _ => unreachable!(),
        };
        assert!(split_state.visual_row > 0);
        assert!(split_state.source_anchor > 0);
    }

    #[test]
    fn markdown_reader_compiles_centered_typed_blocks_and_bounded_paint() {
        let source = "<!-- reader metadata -->\n# **Native apps**\n\nReadable `body` copy wraps into a calm measure.\n\n- first\n\n> quoted\n\n```rust\nfn main() {}\n```\n\n| A | B |\n|---|---|\n| 1 | 2 |\n";
        let (runtime, instance, view) = markdown_runtime(source);
        let tree = runtime
            .render(
                instance,
                view,
                &ViewCx {
                    viewport: LogicalRect::new(0.0, 0.0, 1_000.0, 1_400.0),
                    config_revision: 1,
                    update_revision: 1,
                    animation_phase_ms: 720,
                    motion: ViewMotionCx::default(),
                    terminal_font_px: 12.0,
                    terminal_theme: aterm_render::Theme::default(),
                    semantic_font: None,
                    document: None,
                },
            )
            .unwrap();
        let compiled = tree
            .compile(LogicalRect::new(0.0, 0.0, 1_000.0, 1_400.0))
            .unwrap();
        let outline = compiled.semantic(&UiKey::new("markdown/outline")).unwrap();
        assert_eq!(outline.rect.x, 0.0);
        assert_eq!(outline.rect.width, 220.0);
        let header = compiled.semantic(&UiKey::new("markdown/header")).unwrap();
        assert_eq!(header.rect.x, 248.0);
        assert_eq!(header.rect.width, 724.0);
        let preview = compiled.semantic(&UiKey::new("markdown/preview")).unwrap();
        let first_block = compiled.semantic(&UiKey::new("markdown/block/0")).unwrap();
        assert!(first_block.rect.x >= preview.rect.x + 14.0);
        assert!(first_block.rect.y >= preview.rect.y + 10.0);
        assert!(first_block.rect.right() <= preview.rect.right() - 14.0);
        assert!(compiled.paint.iter().any(|node| matches!(
            &node.content,
            UiContent::MarkdownBlock(MarkdownBlockSpec {
                kind: MarkdownBlockKind::Code { language: Some(language) },
                ..
            }) if language == "rust"
        )));
        assert!(compiled.paint.iter().any(|node| matches!(
            &node.content,
            UiContent::MarkdownBlock(MarkdownBlockSpec {
                kind: MarkdownBlockKind::Quote,
                ..
            })
        )));
        let prims = compiled.tray(aterm_render::Theme::default(), 13.0).prims;
        assert!(prims.iter().any(
            |primitive| matches!(primitive, crate::widget::DrawPrim::Text { s, .. } if s == "RUST")
        ));
        assert!(prims.iter().any(
            |primitive| matches!(primitive, crate::widget::DrawPrim::Text { s, .. } if s == "•")
        ));
        assert!(compiled.semantics.iter().all(|node| {
            !node.label.contains("reader metadata")
                && !node.label.contains("**")
                && !node.label.contains('`')
        }));
        assert!(prims.iter().all(|primitive| {
            !matches!(primitive, crate::widget::DrawPrim::Text { s, .. }
                if s.contains("reader metadata") || s.contains("**"))
        }));
        assert!(prims.len() < 256, "one viewport produces bounded draw work");
        compiled.validate_parity().unwrap();
    }

    #[test]
    fn markdown_select_all_copy_and_cancel_are_exact_source_actions() {
        let source =
            "<!-- reader-hidden -->\n# **Héllo**\n\nA [jump](#next).\n\n## Next\nBody 🦀\n";
        let (mut runtime, instance, view) = markdown_runtime(source);

        runtime
            .dispatch(
                instance,
                view,
                AppEvent::TextInput(TextInputEvent::SelectAll),
            )
            .unwrap();
        let state = match runtime.view_state(view).unwrap() {
            AppViewState::Markdown(state) => state,
            _ => panic!("Markdown view changed kind"),
        };
        assert_eq!(state.selection, Some(0..source.len()));
        let selected_commands = runtime.commands(instance, view).unwrap();
        assert!(command(&selected_commands, "markdown/copy").enabled);
        assert!(!command(&selected_commands, "markdown/select-all").enabled);
        let viewport = LogicalRect::new(0.0, 0.0, 760.0, 540.0);
        let selected_ui = runtime
            .render(
                instance,
                view,
                &ViewCx {
                    viewport,
                    config_revision: 1,
                    update_revision: 1,
                    animation_phase_ms: 720,
                    motion: ViewMotionCx::default(),
                    terminal_font_px: 12.0,
                    terminal_theme: aterm_render::Theme::default(),
                    semantic_font: None,
                    document: None,
                },
            )
            .unwrap()
            .compile(viewport)
            .unwrap();
        assert!(selected_ui.semantics.iter().any(|node| {
            node.key.as_str().starts_with("markdown/block/")
                && node.state.is_some_and(|state| state.selected)
                && matches!(&node.value, crate::native_ui::SemanticValue::Text(_))
        }));

        let copied = runtime
            .dispatch(
                instance,
                view,
                AppEvent::Action(ActionInvocation {
                    id: ActionId::new("markdown/copy"),
                    value: None,
                }),
            )
            .unwrap();
        assert!(matches!(
            copied.effects.as_slice(),
            [AppEffect::Clipboard {
                request: ClipboardRequest::CopyDocumentRange { document: _, range, sensitive: false },
                ..
            }] if range == &(0..source.len())
        ));

        runtime
            .dispatch(instance, view, AppEvent::TextInput(TextInputEvent::Cancel))
            .unwrap();
        let cleared = runtime.commands(instance, view).unwrap();
        assert!(!command(&cleared, "markdown/copy").enabled);
        assert!(command(&cleared, "markdown/select-all").enabled);
    }

    /// AUDIT I9 — every Reader row whose chord `app_native` really dispatches
    /// must ADVERTISE that chord on THIS platform.
    ///
    /// The four rows below are all reached through the same `command`
    /// (`SUPER | CTRL`) predicate in `App::on_key_native_mode`: Back/Forward/Edit
    /// through the `markdown_active && command && Character(..)` arms, and Select
    /// All through `Character('a' | 'A') if command` ->
    /// `TextInput(SelectAll)`, which `MarkdownApp`'s reducer answers with the
    /// body of `markdown/select-all` (pinned by
    /// `markdown_select_all_copy_and_cancel_are_exact_source_actions`, which
    /// dispatches that very event). None of the
    /// four chords is seeded in `Keybindings::PLATFORM_DEFAULT_PAIRS`, and the
    /// readline arms that shadow 'a'/'e' are gated on `settings_active`, so
    /// nothing upstream claims them.
    ///
    /// Select All regressed here: it was pinned to the literal `"Cmd-A"`, which
    /// `palette::platform_accel` BLANKS off macOS (correctly — a ⌘ chord would
    /// mislead, and the string is also spoken aloud by Narrator through the
    /// AccessKit description). The result was a live Ctrl+A shown with no
    /// accelerator at all. Asserting the whole class, not just the one row,
    /// keeps the next chord from landing the same way.
    #[test]
    fn reader_rows_advertise_the_chord_this_platform_actually_dispatches() {
        let source = "# Guide\n\nBody.\n\n## Next\n\nMore.\n";
        let (mut runtime, instance, view) = markdown_runtime(source);
        // Give history a back entry so Back/Forward are enabled rows.
        runtime
            .dispatch(instance, view, AppEvent::ScrollLines(3))
            .unwrap();
        let commands = runtime.commands(instance, view).unwrap();

        for (id, key) in [
            ("markdown/back", "["),
            ("markdown/forward", "]"),
            ("markdown/edit", "E"),
            ("markdown/select-all", "A"),
        ] {
            let shortcut = command(&commands, id)
                .shortcut
                .as_deref()
                .unwrap_or_else(|| panic!("{id} must advertise its live chord"));
            assert_eq!(
                shortcut,
                super::reader_command_chord(key),
                "{id} must name the chord THIS platform dispatches"
            );
            // The property `platform_accel` enforces: off macOS a `Cmd-`
            // accelerator is dropped, so a live chord spelled that way is
            // shown (and spoken) as nothing at all.
            if !cfg!(target_os = "macos") {
                assert!(
                    !shortcut.starts_with("Cmd-"),
                    "{id}: a Cmd- chord is blanked off macOS, hiding a live binding"
                );
            }
        }
    }

    #[test]
    fn markdown_preview_source_split_and_block_selection_are_real_view_state() {
        let source = "# **Guide**\n\nBody with ![local](assets/pic.png).\n";
        let (mut runtime, instance, view) = markdown_runtime(source);
        let document = match runtime.app(instance).unwrap() {
            NativeApp::Markdown(app) => app.document,
            _ => unreachable!(),
        };
        let snapshot = crate::document_store::DocumentSnapshot {
            id: document,
            seq: aterm_buffer::Seq(1),
            file_version: crate::document_store::FileVersion::default(),
            text: std::sync::Arc::from(source),
        };
        let viewport = LogicalRect::new(0.0, 0.0, 900.0, 640.0);

        runtime
            .dispatch(
                instance,
                view,
                AppEvent::Action(ActionInvocation {
                    id: ActionId::new("markdown/mode/source"),
                    value: None,
                }),
            )
            .unwrap();
        assert!(matches!(
            runtime.view_state(view),
            Some(AppViewState::Markdown(MarkdownViewState {
                mode: MarkdownViewMode::Source,
                ..
            }))
        ));
        let source_ui = runtime
            .render(
                instance,
                view,
                &ViewCx {
                    viewport,
                    config_revision: 1,
                    update_revision: 1,
                    animation_phase_ms: 720,
                    motion: ViewMotionCx::default(),
                    terminal_font_px: 12.0,
                    terminal_theme: aterm_render::Theme::default(),
                    semantic_font: None,
                    document: Some(&snapshot),
                },
            )
            .unwrap()
            .compile(viewport)
            .unwrap();
        let source_node = source_ui.semantic(&UiKey::new("markdown/source")).unwrap();
        assert!(source_node.label.contains("**Guide**"));
        assert!(
            source_node
                .action
                .as_ref()
                .is_some_and(|action| action.as_str().starts_with("markdown/select-range/"))
        );

        runtime
            .dispatch(
                instance,
                view,
                AppEvent::Action(ActionInvocation {
                    id: source_node.action.clone().unwrap(),
                    value: None,
                }),
            )
            .unwrap();
        assert!(matches!(
            runtime.view_state(view),
            Some(AppViewState::Markdown(state)) if state.selection == Some(0..source.len())
        ));

        runtime
            .dispatch(
                instance,
                view,
                AppEvent::Action(ActionInvocation {
                    id: ActionId::new("markdown/mode/split"),
                    value: None,
                }),
            )
            .unwrap();
        let split_ui = runtime
            .render(
                instance,
                view,
                &ViewCx {
                    viewport,
                    config_revision: 1,
                    update_revision: 1,
                    animation_phase_ms: 720,
                    motion: ViewMotionCx::default(),
                    terminal_font_px: 12.0,
                    terminal_theme: aterm_render::Theme::default(),
                    semantic_font: None,
                    document: Some(&snapshot),
                },
            )
            .unwrap()
            .compile(viewport)
            .unwrap();
        assert!(split_ui.semantic(&UiKey::new("markdown/split")).is_some());
        assert!(split_ui.semantic(&UiKey::new("markdown/preview")).is_some());
        assert!(split_ui.semantic(&UiKey::new("markdown/source")).is_some());
        split_ui.validate_parity().unwrap();
    }

    #[test]
    fn markdown_image_actions_select_local_source_and_gate_remote_open() {
        let source = "![local](assets/pic.png) ![remote](https://example.com/pic.png)";
        let (mut runtime, instance, view) = markdown_runtime(source);
        let local = runtime
            .dispatch(
                instance,
                view,
                AppEvent::Action(ActionInvocation {
                    id: ActionId::new("markdown/image/0"),
                    value: None,
                }),
            )
            .unwrap();
        assert!(local.effects.is_empty());
        assert!(matches!(
            runtime.view_state(view),
            Some(AppViewState::Markdown(state))
                if state.selection.as_ref().is_some_and(|range| &source[range.clone()] == "![local](assets/pic.png)")
        ));

        let remote = runtime
            .dispatch(
                instance,
                view,
                AppEvent::Action(ActionInvocation {
                    id: ActionId::new("markdown/image/1"),
                    value: None,
                }),
            )
            .unwrap();
        assert!(matches!(
            remote.effects.as_slice(),
            [AppEffect::OpenExternal {
                request: ExternalOpenRequest { uri, user_initiated: true },
                ..
            }] if uri == "https://example.com/pic.png"
        ));
    }

    #[test]
    fn markdown_outline_and_links_use_typed_policy_without_local_file_access() {
        let source = "# Start\n\n[jump](#next) [web](https://example.com/docs) [file](README.md) [bad](javascript:alert)\n\n## Next\nDone\n";
        let (mut runtime, instance, view) = markdown_runtime(source);
        let initial_commands = runtime.commands(instance, view).unwrap();
        assert!(command(&initial_commands, "markdown/link/0").enabled);
        assert!(command(&initial_commands, "markdown/link/1").enabled);
        assert!(!command(&initial_commands, "markdown/link/2").enabled);
        assert!(!command(&initial_commands, "markdown/link/3").enabled);

        let outline = runtime
            .dispatch(
                instance,
                view,
                AppEvent::Action(ActionInvocation {
                    id: ActionId::new("markdown/outline/1"),
                    value: None,
                }),
            )
            .unwrap();
        assert!(outline.effects.is_empty());
        let next_anchor =
            crate::native_markdown::local_anchor(&crate::native_markdown::parse(source), "#next")
                .unwrap();
        assert!(matches!(
            runtime.view_state(view),
            Some(AppViewState::Markdown(state)) if state.source_anchor == next_anchor
        ));

        let local = runtime
            .dispatch(
                instance,
                view,
                AppEvent::Action(ActionInvocation {
                    id: ActionId::new("markdown/link/0"),
                    value: None,
                }),
            )
            .unwrap();
        assert!(local.effects.is_empty());
        assert!(matches!(
            runtime.view_state(view),
            Some(AppViewState::Markdown(state)) if state.source_anchor == next_anchor
        ));

        let external = runtime
            .dispatch(
                instance,
                view,
                AppEvent::Action(ActionInvocation {
                    id: ActionId::new("markdown/link/1"),
                    value: None,
                }),
            )
            .unwrap();
        assert!(matches!(
            external.effects.as_slice(),
            [AppEffect::OpenExternal {
                request: ExternalOpenRequest { uri, user_initiated: true },
                ..
            }] if uri == "https://example.com/docs"
        ));

        for denied in [2, 3] {
            let outcome = runtime
                .dispatch(
                    instance,
                    view,
                    AppEvent::Action(ActionInvocation {
                        id: ActionId::new(format!("markdown/link/{denied}")),
                        value: None,
                    }),
                )
                .unwrap();
            assert!(outcome.effects.is_empty(), "link {denied} must stay inert");
        }
    }

    #[test]
    fn markdown_reader_is_responsive_and_all_observers_stay_in_bounds() {
        let source = (0..40)
            .map(|index| format!("## Section {index}\n\nBody [jump](#section-0).\n\n"))
            .collect::<String>();
        let (runtime, instance, view) = markdown_runtime(&source);
        for (width, height, expects_outline) in [
            (1_120.0, 720.0, true),
            (800.0, 620.0, false),
            (420.0, 360.0, false),
        ] {
            let viewport = LogicalRect::new(0.0, 0.0, width, height);
            let compiled = runtime
                .render(
                    instance,
                    view,
                    &ViewCx {
                        viewport,
                        config_revision: 1,
                        update_revision: 1,
                        animation_phase_ms: 720,
                        motion: ViewMotionCx::default(),
                        terminal_font_px: 12.0,
                        terminal_theme: aterm_render::Theme::default(),
                        semantic_font: None,
                        document: None,
                    },
                )
                .unwrap()
                .compile(viewport)
                .unwrap();
            assert_eq!(
                compiled.semantic(&UiKey::new("markdown/outline")).is_some(),
                expects_outline
            );
            assert!(compiled.semantic(&UiKey::new("markdown/header")).is_some());
            assert!(compiled.semantics.iter().all(|node| {
                node.rect.x >= 0.0
                    && node.rect.y >= 0.0
                    && node.rect.right() <= width
                    && node.rect.bottom() <= height
            }));
            assert!(compiled.hits.iter().all(|hit| {
                hit.rect.x >= 0.0
                    && hit.rect.y >= 0.0
                    && hit.rect.right() <= width
                    && hit.rect.bottom() <= height
            }));
            compiled.validate_parity().unwrap();
        }
    }

    #[test]
    fn markdown_frame_commands_and_pixels_remain_bounded_for_large_outline() {
        let source = (0..5_000)
            .map(|index| format!("# Heading {index}\n\nParagraph {index}.\n\n"))
            .collect::<String>();
        let (mut runtime, instance, view) = markdown_runtime(&source);
        let commands = runtime.commands(instance, view).unwrap();
        assert!(commands.len() <= 47, "command materialization is capped");

        let viewport = LogicalRect::new(0.0, 0.0, 1_000.0, 700.0);
        let before = runtime
            .render(
                instance,
                view,
                &ViewCx {
                    viewport,
                    config_revision: 1,
                    update_revision: 1,
                    animation_phase_ms: 720,
                    motion: ViewMotionCx::default(),
                    terminal_font_px: 12.0,
                    terminal_theme: aterm_render::Theme::default(),
                    semantic_font: None,
                    document: None,
                },
            )
            .unwrap()
            .compile(viewport)
            .unwrap();
        assert!(
            before.semantics.len() < 96,
            "one frame stays viewport-bounded"
        );
        let before_prims = before.tray(aterm_render::Theme::default(), 13.0).prims;
        let (before_pixels, width, height) =
            crate::tray_raster::rasterize_tray(&before_prims, 1_000, 700, 1.0, [0, 0, 0, 255]);
        assert_eq!((width, height), (1_000, 700));

        runtime
            .dispatch(
                instance,
                view,
                AppEvent::TextInput(TextInputEvent::SelectAll),
            )
            .unwrap();
        let after = runtime
            .render(
                instance,
                view,
                &ViewCx {
                    viewport,
                    config_revision: 1,
                    update_revision: 1,
                    animation_phase_ms: 720,
                    motion: ViewMotionCx::default(),
                    terminal_font_px: 12.0,
                    terminal_theme: aterm_render::Theme::default(),
                    semantic_font: None,
                    document: None,
                },
            )
            .unwrap()
            .compile(viewport)
            .unwrap();
        let after_prims = after.tray(aterm_render::Theme::default(), 13.0).prims;
        let after_pixels =
            crate::tray_raster::rasterize_tray(&after_prims, 1_000, 700, 1.0, [0, 0, 0, 255]).0;
        assert_ne!(
            before_pixels, after_pixels,
            "selection changes real raster pixels"
        );
    }

    #[test]
    fn markdown_block_copy_is_utf8_safe_and_capped() {
        let source = format!("```text\n{}\n```", "🦀".repeat(100_000));
        let parsed = crate::native_markdown::parse(&source);
        let code = parsed
            .blocks
            .iter()
            .find(|block| {
                matches!(
                    block,
                    crate::native_markdown::MarkdownBlock::CodeBlock { .. }
                )
            })
            .unwrap();
        let bounded = bounded_markdown_block_text(code);
        assert!(bounded.len() <= 128 * 1024);
        assert!(bounded.ends_with('…'));
        assert!(bounded.is_char_boundary(bounded.len()));
    }

    #[test]
    fn markdown_header_uses_native_icons_pointer_selection_and_empty_state() {
        let (mut runtime, instance, view) = markdown_runtime("# Start\n\nBody\n");
        let viewport = LogicalRect::new(0.0, 0.0, 474.0, 468.0);
        let initial = runtime
            .render(
                instance,
                view,
                &ViewCx {
                    viewport,
                    config_revision: 1,
                    update_revision: 1,
                    animation_phase_ms: 720,
                    motion: ViewMotionCx::default(),
                    terminal_font_px: 12.0,
                    terminal_theme: aterm_render::Theme::default(),
                    semantic_font: None,
                    document: None,
                },
            )
            .unwrap()
            .compile(viewport)
            .unwrap();
        let icon = |key: &str| {
            initial
                .paint
                .iter()
                .find(|node| node.key.as_str() == key)
                .and_then(|node| match &node.content {
                    UiContent::Button(control) => control.spec.visual_icon,
                    _ => None,
                })
        };
        assert_eq!(icon("markdown/back-button"), Some(ButtonIcon::Back));
        assert_eq!(icon("markdown/forward-button"), Some(ButtonIcon::Forward));
        assert_eq!(icon("markdown/copy-button"), Some(ButtonIcon::Copy));
        let selection = initial
            .semantic(&UiKey::new("markdown/selection-button"))
            .unwrap();
        assert_eq!(selection.label, "Select all source");
        assert_eq!(
            selection.action.as_ref().map(ActionId::as_str),
            Some("markdown/select-all")
        );
        assert!(initial.hits.iter().any(|hit| {
            hit.key.as_str() == "markdown/selection-button"
                && hit.action.as_str() == "markdown/select-all"
        }));
        assert!(
            initial
                .semantic(&UiKey::new("markdown/status"))
                .is_some_and(|node| node.label.starts_with("Section 1/1 · 0% read")),
            "compact status must lead with the position that its page controls change"
        );

        runtime
            .dispatch(
                instance,
                view,
                AppEvent::Action(ActionInvocation {
                    id: ActionId::new("markdown/select-all"),
                    value: None,
                }),
            )
            .unwrap();
        let selected = runtime
            .render(
                instance,
                view,
                &ViewCx {
                    viewport,
                    config_revision: 1,
                    update_revision: 1,
                    animation_phase_ms: 720,
                    motion: ViewMotionCx::default(),
                    terminal_font_px: 12.0,
                    terminal_theme: aterm_render::Theme::default(),
                    semantic_font: None,
                    document: None,
                },
            )
            .unwrap()
            .compile(viewport)
            .unwrap();
        let selection = selected
            .semantic(&UiKey::new("markdown/selection-button"))
            .unwrap();
        assert_eq!(selection.label, "Clear source selection");
        assert_eq!(
            selection.action.as_ref().map(ActionId::as_str),
            Some("markdown/clear-selection")
        );

        let (runtime, instance, view) = markdown_runtime("\n");
        let empty = runtime
            .render(
                instance,
                view,
                &ViewCx {
                    viewport,
                    config_revision: 1,
                    update_revision: 1,
                    animation_phase_ms: 720,
                    motion: ViewMotionCx::default(),
                    terminal_font_px: 12.0,
                    terminal_theme: aterm_render::Theme::default(),
                    semantic_font: None,
                    document: None,
                },
            )
            .unwrap()
            .compile(viewport)
            .unwrap();
        assert!(empty.semantic(&UiKey::new("markdown/empty")).is_some());
        assert_eq!(
            empty
                .semantic(&UiKey::new("markdown/status"))
                .map(|node| node.label.as_str()),
            Some("Empty document")
        );
        assert_eq!(
            empty
                .semantic(&UiKey::new("markdown/selection-button"))
                .and_then(|node| node.state)
                .map(|state| state.enabled),
            Some(false)
        );
        empty.validate_parity().unwrap();
    }

    #[test]
    fn markdown_toolbar_widths_follow_the_renderer_text_scale() {
        for (label, minimum) in [
            ("Preview", 62.0),
            ("Source", 56.0),
            ("Split", 48.0),
            ("Edit", 44.0),
            ("Clear", 54.0),
        ] {
            let width = markdown_toolbar_label_width(label, minimum, 2.0);
            let painted = crate::tray_raster::ui_text_width(label, 26.0);
            assert!(
                width >= painted + 20.0,
                "{label} must retain the renderer's centered-label insets"
            );
        }
        assert!(markdown_toolbar_label_width("Preview", 62.0, 2.0) > 62.0);
        assert!(markdown_toolbar_label_width("Source", 56.0, 2.0) > 56.0);
    }

    #[test]
    fn markdown_page_controls_are_semantic_exact_and_advance_read_status() {
        let source = (0..80)
            .map(|index| format!("Paragraph {index} has enough words for a reading row.\n\n"))
            .collect::<String>();
        let (mut runtime, instance, view) = markdown_runtime(&source);
        let viewport = LogicalRect::new(0.0, 0.0, 800.0, 520.0);
        let render = |runtime: &NativeRuntime| {
            runtime
                .render(
                    instance,
                    view,
                    &ViewCx {
                        viewport,
                        config_revision: 1,
                        update_revision: 1,
                        animation_phase_ms: 720,
                        motion: ViewMotionCx::default(),
                        terminal_font_px: 12.0,
                        terminal_theme: aterm_render::Theme::default(),
                        semantic_font: None,
                        document: None,
                    },
                )
                .unwrap()
                .compile(viewport)
                .unwrap()
        };

        let initial = render(&runtime);
        let previous = initial
            .semantic(&UiKey::new("markdown/previous-page-button"))
            .unwrap();
        let next = initial
            .semantic(&UiKey::new("markdown/next-page-button"))
            .unwrap();
        assert_eq!(previous.label, "Previous reading page");
        assert!(previous.state.is_some_and(|state| !state.enabled));
        assert!(next.state.is_some_and(|state| state.enabled));
        let next_action = next.action.clone().expect("next page action");
        assert!(next_action.as_str().starts_with("markdown/page/"));
        assert!(initial.hits.iter().any(|hit| {
            hit.key.as_str() == "markdown/next-page-button" && hit.action == next_action
        }));

        runtime
            .dispatch(
                instance,
                view,
                AppEvent::Action(ActionInvocation {
                    id: next_action,
                    value: None,
                }),
            )
            .unwrap();
        assert!(matches!(
            runtime.view_state(view),
            Some(AppViewState::Markdown(state)) if state.source_anchor > 0
        ));
        let advanced = render(&runtime);
        assert!(
            advanced
                .semantic(&UiKey::new("markdown/previous-page-button"))
                .and_then(|node| node.state)
                .is_some_and(|state| state.enabled)
        );
        assert!(
            advanced
                .semantic(&UiKey::new("markdown/status"))
                .is_some_and(|node| node.label.contains("% read"))
        );
        advanced.validate_parity().unwrap();
    }

    #[test]
    fn markdown_editor_open_completion_replaces_transient_status() {
        let (mut runtime, instance, view) = markdown_runtime("# Editable\n\nBody\n");
        let document = runtime.document_id(instance).unwrap();
        let opening = runtime
            .dispatch(
                instance,
                view,
                AppEvent::Action(ActionInvocation {
                    id: ActionId::new("markdown/edit"),
                    value: None,
                }),
            )
            .unwrap();
        assert!(matches!(
            opening.effects.as_slice(),
            [AppEffect::OpenDocumentEditor { document: requested }] if *requested == document
        ));
        assert!(matches!(
            runtime.view_state(view),
            Some(AppViewState::Markdown(state)) if state.notice.as_deref() == Some("Opening editor…")
        ));

        runtime
            .dispatch(instance, view, AppEvent::DocumentEditorOpened { document })
            .unwrap();
        assert!(matches!(
            runtime.view_state(view),
            Some(AppViewState::Markdown(state)) if state.notice.as_deref() == Some("Editor opened")
        ));
    }

    fn editor_fixture(
        text: &str,
        title: &str,
    ) -> (
        EditorApp,
        EditorViewState,
        crate::document_store::DocumentSnapshot,
    ) {
        let mut store = crate::document_store::DocumentStore::new();
        let document = store.open("file:///visual-editor.md".to_string(), text.to_string());
        let snapshot = store.snapshot(document).unwrap();
        let buffer = crate::native_editor::EditorBufferView::new(
            document,
            crate::document_store::DocumentViewId(1),
            snapshot.seq,
        );
        (
            EditorApp::new(document, title.to_string()),
            EditorViewState {
                buffer: Some(buffer),
                ..EditorViewState::default()
            },
            snapshot,
        )
    }

    fn editor_spec(compiled: &crate::native_ui::CompiledUi) -> &TextViewportSpec {
        compiled
            .paint
            .iter()
            .find_map(|node| match &node.content {
                UiContent::TextViewport(spec) => Some(spec),
                _ => None,
            })
            .expect("editor viewport paint node")
    }

    #[test]
    fn manual_watcher_warning_does_not_destroy_editor_feedback_and_recovery_clears_it() {
        let source = "font_px = \"huge\"\n";
        let (mut app, mut view, _snapshot) = editor_fixture(source, "aterm.toml");
        app.config_editor = true;
        app.config_analysis = Some(crate::native_config_language::analyze(source));
        view.status = Some("Saved".to_string());
        view.config_watch_status =
            Some("aterm.toml reload rejected: the file is not valid UTF-8.".to_string());

        let warning = editor_status_message(&app, &view, None, false).unwrap();
        assert!(warning.contains("Reload warning"));
        assert!(warning.contains("not valid UTF-8"));
        assert!(warning.contains("Ln 1, Col"));
        assert!(warning.contains("font_px"));
        assert_eq!(view.status.as_deref(), Some("Saved"));

        view.config_watch_status = None;
        let recovered = editor_status_message(&app, &view, None, false).unwrap();
        assert!(recovered.contains("Ln 1, Col"));
        assert!(!recovered.contains("Reload warning"));
        assert_eq!(view.status.as_deref(), Some("Saved"));
    }

    #[test]
    fn editor_view_is_responsive_bounded_and_reports_real_buffer_state() {
        let text = (1..=80)
            .map(|line| format!("line {line}\n"))
            .collect::<String>();
        let title = format!("{}-document.md", "very-long-🦀-".repeat(20));
        let (app, view, snapshot) = editor_fixture(&text, &title);
        for (width, height, expected_padding, expected_editor_y) in [
            (1_424.0, 658.0, 16.0, 60.0),
            (804.0, 582.0, 16.0, 60.0),
            (474.0, 468.0, 8.0, 52.0),
        ] {
            let viewport = LogicalRect::new(0.0, 0.0, width, height);
            let compiled = app
                .view(
                    &view,
                    &ViewCx {
                        viewport,
                        config_revision: 1,
                        update_revision: 1,
                        animation_phase_ms: 720,
                        motion: ViewMotionCx::default(),
                        terminal_font_px: 12.0,
                        terminal_theme: aterm_render::Theme::default(),
                        semantic_font: None,
                        document: Some(&snapshot),
                    },
                )
                .compile(viewport)
                .unwrap();
            let semantic = compiled.semantic(&UiKey::new("editor/buffer")).unwrap();
            assert_eq!(semantic.rect.x, expected_padding);
            assert_eq!(semantic.rect.y, expected_editor_y);
            assert_eq!(semantic.rect.right(), width - expected_padding);
            assert_eq!(semantic.rect.bottom(), height - expected_padding);
            let spec = editor_spec(&compiled);
            let projection = spec.projection.as_ref().unwrap();
            assert!(projection.lines.len() <= 33);
            assert_eq!(projection.total_lines, 81);
            assert_eq!(spec.status.as_deref(), Some("81 lines · Ready"));
            assert!(spec.label.starts_with("very-long-"));
            assert!(spec.label.ends_with("document.md"));
            assert!(spec.label.chars().count() <= 96);
            assert!(compiled.semantics.iter().all(|node| {
                node.rect.x >= 0.0
                    && node.rect.y >= 0.0
                    && node.rect.right() <= width
                    && node.rect.bottom() <= height
            }));
            compiled.validate_parity().unwrap();
        }
    }

    #[test]
    fn manual_config_completions_page_every_candidate_and_keep_full_live_help() {
        let source = "";
        let (mut app, mut view, snapshot) = editor_fixture(source, "aterm.toml");
        app.config_editor = true;
        app.config_analysis = Some(crate::native_config_language::analyze(source));
        app.config_analysis_revision = 1;
        let assist = crate::native_config_language::assist(source, 0);
        assert_eq!(assist.completions.len(), 8);
        view.config_completion_interaction =
            Some(crate::native_config_language::ConfigCompletionContext::new(
                snapshot.id.get(),
                snapshot.seq.0,
                0,
            ));
        let viewport = LogicalRect::new(0.0, 0.0, 320.0, 300.0);
        let capacity = crate::native_ui::editor_shell_metrics(viewport, 8).palette_visible_rows;
        assert!(capacity > 0 && capacity < assist.completions.len());
        let mut reachable = std::collections::BTreeSet::new();

        for selected in 0..assist.completions.len() {
            view.config_completion_selected = selected;
            let compiled = app
                .view(
                    &view,
                    &ViewCx {
                        viewport,
                        config_revision: 1,
                        update_revision: 1,
                        animation_phase_ms: 720,
                        motion: ViewMotionCx::default(),
                        terminal_font_px: 12.0,
                        terminal_theme: aterm_render::Theme::default(),
                        semantic_font: None,
                        document: Some(&snapshot),
                    },
                )
                .compile(viewport)
                .unwrap();
            let visible = compiled
                .semantics
                .iter()
                .filter_map(|node| {
                    node.key
                        .as_str()
                        .strip_prefix("editor/config-completion/")?
                        .parse::<usize>()
                        .ok()
                })
                .collect::<Vec<_>>();
            assert!(visible.contains(&selected));
            assert!(visible.len() <= capacity);
            reachable.extend(visible);

            let help = compiled
                .semantic(&UiKey::new("editor/config-help"))
                .unwrap();
            assert_eq!(help.role, crate::native_ui::SemanticRole::Status);
            assert!(help.label.contains("Tab or Ctrl-Space choose"));
            assert!(help.label.contains(assist.help.as_deref().unwrap()));
            assert!(
                compiled
                    .semantic(&UiKey::new("editor/config-help/visual"))
                    .is_none(),
                "bounded visual copy must not duplicate the live status"
            );
            let visual = compiled
                .paint
                .iter()
                .find(|node| node.key == UiKey::new("editor/config-help/visual"))
                .and_then(|node| match &node.content {
                    UiContent::Text(spec) => Some(spec.text.as_str()),
                    _ => None,
                })
                .expect("painted config help");
            assert!(visual.chars().count() < help.label.chars().count());

            for key in ["editor/config-page-previous", "editor/config-page-next"] {
                let action = compiled
                    .semantic(&UiKey::new(key))
                    .and_then(|node| node.action.as_ref());
                assert!(action.is_some());
                assert!(
                    crate::command_registry::native_document_action(action.unwrap().as_str())
                        .is_some()
                );
            }
            compiled.validate_parity().unwrap();
        }
        assert_eq!(
            reachable,
            (0..assist.completions.len()).collect::<std::collections::BTreeSet<_>>()
        );
    }

    #[test]
    fn manual_gpu_help_fits_a_wide_editor_without_eliding_lsp_guidance() {
        let source = "background_opacity = \n";
        let (mut app, mut view, snapshot) = editor_fixture(source, "aterm.toml");
        app.config_editor = true;
        app.config_analysis = Some(crate::native_config_language::analyze(source));
        app.config_analysis_revision = snapshot.seq.0;
        let caret = source.find('\n').expect("one-line config");
        let buffer = view.buffer.as_mut().expect("editor buffer");
        buffer.selections = vec![crate::native_editor::Selection::caret(caret)];
        buffer.primary = 0;

        let viewport = LogicalRect::new(0.0, 0.0, 1_200.0, 822.0);
        let compiled = app
            .view(
                &view,
                &ViewCx {
                    viewport,
                    config_revision: 1,
                    update_revision: 1,
                    animation_phase_ms: 720,
                    motion: ViewMotionCx::default(),
                    terminal_font_px: 12.0,
                    terminal_theme: aterm_render::Theme::default(),
                    semantic_font: None,
                    document: Some(&snapshot),
                },
            )
            .compile(viewport)
            .unwrap();
        let semantic = compiled
            .semantic(&UiKey::new("editor/config-help"))
            .expect("complete config help semantics");
        assert!(
            compiled
                .semantic(&UiKey::new("editor/config-help/visual"))
                .is_none()
        );
        let visual = compiled
            .paint
            .iter()
            .find(|node| node.key == UiKey::new("editor/config-help/visual"))
            .and_then(|node| match &node.content {
                UiContent::Text(spec) => Some(spec.text.as_str()),
                _ => None,
            })
            .expect("visible config help");

        for expected in [
            "background_opacity",
            "number",
            "default 1.0",
            "range 0–1",
            "macOS GPU",
            "CPU",
            "4.5:1",
        ] {
            assert!(semantic.label.contains(expected), "{}", semantic.label);
            assert!(visual.contains(expected), "{visual}");
        }
        assert!(!visual.contains('…'), "{visual}");
        let audit = compiled.paint_audit_lines();
        assert!(
            audit.iter().any(|line| {
                line.contains("key=\"editor/config-help/visual\"")
                    && line.contains("overflow=false")
            }),
            "{audit:#?}"
        );
        compiled.validate_parity().unwrap();
    }

    #[test]
    fn manual_gpu_completion_help_fits_the_live_wide_editor_path() {
        let source = "";
        let (mut app, view, snapshot) = editor_fixture(source, "aterm.toml");
        app.config_editor = true;
        app.config_analysis = Some(crate::native_config_language::analyze(source));
        app.config_analysis_revision = snapshot.seq.0;
        let viewport = LogicalRect::new(0.0, 0.0, 1_200.0, 822.0);
        let compiled = app
            .view(
                &view,
                &ViewCx {
                    viewport,
                    config_revision: 1,
                    update_revision: 1,
                    animation_phase_ms: 720,
                    motion: ViewMotionCx::default(),
                    terminal_font_px: 12.0,
                    terminal_theme: aterm_render::Theme::default(),
                    semantic_font: None,
                    document: Some(&snapshot),
                },
            )
            .compile(viewport)
            .unwrap();
        let semantic = compiled
            .semantic(&UiKey::new("editor/config-help"))
            .expect("complete completion help semantics");
        assert!(
            compiled
                .semantic(&UiKey::new("editor/config-help/visual"))
                .is_none()
        );
        let visual = compiled
            .paint
            .iter()
            .find(|node| node.key == UiKey::new("editor/config-help/visual"))
            .and_then(|node| match &node.content {
                UiContent::Text(spec) => Some(spec.text.as_str()),
                _ => None,
            })
            .expect("visible completion help");

        assert!(semantic.label.contains("inherited $ATERM_CPU"));
        for expected in [
            "GPU rendering",
            "last --cpu/--gpu",
            "$ATERM_CPU",
            "$ATERM_GPU",
            "Ctrl-Space",
            "↑↓ select",
            "Enter/Tab insert",
        ] {
            assert!(visual.contains(expected), "{visual}");
        }
        assert!(!visual.contains('…'), "{visual}");
        assert!(compiled.paint_audit_lines().iter().any(|line| {
            line.contains("key=\"editor/config-help/visual\"") && line.contains("overflow=false")
        }));
        compiled.validate_parity().unwrap();
    }

    #[test]
    fn manual_editor_chrome_elision_preserves_complete_grapheme_clusters() {
        for (source, expected) in [
            ("A👩‍💻WWWW", "A👩‍💻…"),
            ("Ae\u{301}WWWW", "Ae\u{301}…"),
            ("A🇺🇳WWWW", "A🇺🇳…"),
        ] {
            assert_eq!(bounded_markdown_label(source, 2), expected);
        }

        let prompt = editor_prompt_label("I-search: ", "discarded👩‍💻🇺🇳", "e\u{301}", 14);
        assert_eq!(prompt, "I-search: …👩‍💻🇺🇳e\u{301}");
        assert_eq!(prompt.graphemes().count(), 14);
    }

    #[test]
    fn manual_editor_cursor_label_uses_shared_grapheme_cell_geometry() {
        let text = "first\n\te\u{301}中👩‍💻🇺🇳Z\n";
        let (_app, mut view, _snapshot) = editor_fixture(text, "aterm.toml");
        let buffer = view.buffer.as_mut().expect("editor buffer");
        let line_start = text.find('\n').unwrap() + 1;
        let after_tab = line_start + '\t'.len_utf8();
        let after_combining = after_tab + "e\u{301}".len();
        let after_cjk = after_combining + "中".len();
        let after_emoji = after_cjk + "👩‍💻".len();
        let after_flag = after_emoji + "🇺🇳".len();

        for (caret, expected) in [
            (after_tab, "Ln 2, Col 5"),
            (after_combining, "Ln 2, Col 6"),
            (after_cjk, "Ln 2, Col 8"),
            (after_emoji, "Ln 2, Col 10"),
            (after_flag, "Ln 2, Col 12"),
            (after_flag + 1, "Ln 2, Col 13"),
            // Defensive selections on character boundaries inside a cluster
            // resolve to the cluster's leading edge, never a phantom cell.
            (after_tab + 'e'.len_utf8(), "Ln 2, Col 5"),
            (after_cjk + '👩'.len_utf8(), "Ln 2, Col 8"),
        ] {
            buffer.selections = vec![crate::native_editor::Selection::caret(caret)];
            assert_eq!(editor_cursor_label(text, buffer), expected, "caret={caret}");
        }
    }

    #[test]
    fn manual_config_footer_bounds_paint_but_retains_full_unicode_diagnostic() {
        let source = "future = true\n";
        let (mut app, view, snapshot) = editor_fixture(source, "aterm.toml");
        app.config_editor = true;
        let unknown_key = "future.👩‍💻.e\u{301}.🇺🇳.a-deliberately-long-forward-compatible-key";
        let mut analysis = crate::native_config_language::analyze(source);
        analysis.diagnostics = vec![crate::native_config_language::ConfigDiagnostic {
            bytes: 0..6,
            line: 1,
            column: 1,
            severity: crate::native_config_language::ConfigDiagnosticSeverity::Warning,
            message: format!("unknown configuration key {unknown_key}"),
        }];
        app.config_analysis = Some(analysis);
        app.config_analysis_revision = snapshot.seq.0;
        let viewport = LogicalRect::new(0.0, 0.0, 286.5, 558.0);
        let compiled = app
            .view(
                &view,
                &ViewCx {
                    viewport,
                    config_revision: 1,
                    update_revision: 1,
                    animation_phase_ms: 720,
                    motion: ViewMotionCx::default(),
                    terminal_font_px: 12.0,
                    terminal_theme: aterm_render::Theme::default(),
                    semantic_font: None,
                    document: Some(&snapshot),
                },
            )
            .compile(viewport)
            .unwrap();
        let spec = editor_spec(&compiled);
        let semantic = spec
            .semantic_status
            .as_deref()
            .expect("complete diagnostic status");
        let visual = spec.status.as_deref().expect("bounded diagnostic footer");
        assert!(semantic.contains(unknown_key), "{semantic}");
        assert_ne!(visual, semantic);
        assert!(visual.ends_with('…'), "{visual}");
        compiled.validate_parity().unwrap();
    }

    #[test]
    fn editor_command_bar_is_labeled_actionable_and_responsive() {
        let (mut app, view, snapshot) = editor_fixture("first\nsecond\n", "notes.md");
        app.dirty = true;
        app.can_undo = true;
        app.can_redo = true;
        for (width, height, compact) in [(804.0, 582.0, false), (286.5, 558.0, true)] {
            let viewport = LogicalRect::new(0.0, 0.0, width, height);
            let compiled = app
                .view(
                    &view,
                    &ViewCx {
                        viewport,
                        config_revision: 1,
                        update_revision: 1,
                        animation_phase_ms: 720,
                        motion: ViewMotionCx::default(),
                        terminal_font_px: 12.0,
                        terminal_theme: aterm_render::Theme::default(),
                        semantic_font: None,
                        document: Some(&snapshot),
                    },
                )
                .compile(viewport)
                .unwrap();
            for (key, action, label) in [
                ("editor/save-button", "editor/save", "Save buffer"),
                ("editor/undo-button", "editor/undo", "Undo"),
                ("editor/redo-button", "editor/redo", "Redo"),
                ("editor/find-button", "editor/find", "Incremental search"),
                ("editor/goto-line-button", "editor/goto-line", "Go to line"),
                (
                    "editor/commands-button",
                    "editor/commands",
                    "Execute command",
                ),
            ] {
                let semantic = compiled.semantic(&UiKey::new(key)).unwrap();
                assert_eq!(semantic.action.as_ref().map(ActionId::as_str), Some(action));
                assert!(semantic.label.starts_with(label), "{}", semantic.label);
                assert!(semantic.state.is_some_and(|state| state.enabled));
                assert!(
                    compiled
                        .hits
                        .iter()
                        .any(|hit| { hit.key.as_str() == key && hit.action.as_str() == action })
                );
            }
            let audit = compiled.paint_audit_lines();
            for key in [
                "editor/save-button",
                "editor/undo-button",
                "editor/redo-button",
                "editor/find-button",
                "editor/goto-line-button",
                "editor/commands-button",
            ] {
                assert!(audit.iter().any(|line| {
                    line.contains(&format!("key={key:?}")) && line.contains("overflow=false")
                }));
            }
            let primary = compiled.semantic(&UiKey::new("editor/command-row-primary"));
            let navigation = compiled.semantic(&UiKey::new("editor/command-row-navigation"));
            if compact {
                assert!(primary.is_some());
                assert!(navigation.is_some());
                assert!(primary.unwrap().rect.bottom() <= navigation.unwrap().rect.y);
            } else {
                assert!(primary.is_none());
                assert!(navigation.is_none());
            }
            compiled.validate_parity().unwrap();
        }

        app.disk_conflict = true;
        let mut commands = Vec::new();
        app.commands(&view, &mut commands);
        assert!(
            !command(&commands, "editor/save").enabled,
            "shortcut/controller Save must share the conflict gate"
        );
        let viewport = LogicalRect::new(0.0, 0.0, 474.0, 468.0);
        let compiled = app
            .view(
                &view,
                &ViewCx {
                    viewport,
                    config_revision: 1,
                    update_revision: 1,
                    animation_phase_ms: 720,
                    motion: ViewMotionCx::default(),
                    terminal_font_px: 12.0,
                    terminal_theme: aterm_render::Theme::default(),
                    semantic_font: None,
                    document: Some(&snapshot),
                },
            )
            .compile(viewport)
            .unwrap();
        assert!(
            compiled
                .semantic(&UiKey::new("editor/save-button"))
                .and_then(|node| node.state)
                .is_some_and(|state| !state.enabled),
            "the painted/semantic Save button must be disabled during conflict"
        );
        let reload = compiled
            .semantic(&UiKey::new("editor/revert-button"))
            .expect("conflict recovery is visible without opening the command palette");
        assert_eq!(
            reload.action.as_ref().map(ActionId::as_str),
            Some("editor/revert")
        );
        assert!(reload.state.is_some_and(|state| state.enabled));
        assert!(
            reload
                .label
                .contains("Discard Changes and Reload from Disk")
        );
        assert!(
            editor_spec(&compiled)
                .semantic_status
                .as_deref()
                .is_some_and(|status| {
                    status.contains("Save is blocked")
                        && status.contains("Discard Changes and Reload from Disk")
                }),
            "conflict recovery stays actionable even if no generic recovery notice exists"
        );
    }

    #[test]
    fn manual_problem_navigation_is_visible_actionable_and_responsive() {
        let source = "future_first_setting = 1\nfuture_second_setting = 2\n";
        let (mut app, view, snapshot) = editor_fixture(source, "aterm.toml");
        app.config_editor = true;
        app.config_analysis = Some(crate::native_config_language::analyze(source));
        app.config_analysis_revision = snapshot.seq.0;
        assert_eq!(
            app.config_analysis.as_ref().map_or(
                0,
                crate::native_config_language::ConfigAnalysis::diagnostic_count
            ),
            2
        );

        for (width, height) in [(804.0, 582.0), (286.5, 558.0)] {
            let viewport = LogicalRect::new(0.0, 0.0, width, height);
            let compiled = app
                .view(
                    &view,
                    &ViewCx {
                        viewport,
                        config_revision: 1,
                        update_revision: 1,
                        animation_phase_ms: 720,
                        motion: ViewMotionCx::default(),
                        terminal_font_px: 12.0,
                        terminal_theme: aterm_render::Theme::default(),
                        semantic_font: None,
                        document: Some(&snapshot),
                    },
                )
                .compile(viewport)
                .unwrap();

            for (key, action, label) in [
                (
                    "editor/config-problem-previous-button",
                    "editor/config-problem-previous",
                    "Previous config problem (Shift-F8)",
                ),
                (
                    "editor/config-problem-next-button",
                    "editor/config-problem-next",
                    "Next config problem (F8)",
                ),
            ] {
                let semantic = compiled.semantic(&UiKey::new(key)).expect(key);
                assert_eq!(semantic.action.as_ref().map(ActionId::as_str), Some(action));
                assert_eq!(semantic.label, label);
                assert!(semantic.state.is_some_and(|state| state.enabled));
                assert!(
                    compiled
                        .hits
                        .iter()
                        .any(|hit| { hit.key.as_str() == key && hit.action.as_str() == action })
                );
                let audit = compiled.paint_audit_lines();
                assert!(
                    audit.iter().any(|line| {
                        line.contains(&format!("key={key:?}")) && line.contains("overflow=false")
                    }),
                    "{key} at {width}×{height}: {audit:#?}"
                );
            }
            compiled.validate_parity().unwrap();
        }
    }

    #[test]
    fn compact_editor_raster_and_pointer_follow_an_offscreen_caret() {
        let text = "0123456789".repeat(24);
        let (app, view, snapshot) = editor_fixture(&text, "wide-line.md");
        let viewport = LogicalRect::new(0.0, 0.0, 474.0, 468.0);
        let render = |view: &EditorViewState| {
            app.view(
                view,
                &ViewCx {
                    viewport,
                    config_revision: 1,
                    update_revision: 1,
                    animation_phase_ms: 720,
                    motion: ViewMotionCx::default(),
                    terminal_font_px: 12.0,
                    terminal_theme: aterm_render::Theme::default(),
                    semantic_font: None,
                    document: Some(&snapshot),
                },
            )
            .compile(viewport)
            .unwrap()
        };
        let before = render(&view);
        let mut moved = view.clone();
        moved.buffer.as_mut().unwrap().selections =
            vec![crate::native_editor::Selection::caret(173)];
        let after = render(&moved);
        let spec = editor_spec(&after);
        let line = &spec.projection.as_ref().unwrap().lines[0];
        assert!(line.source.start > 0);
        let [(local_caret, true)] = line.carets.as_slice() else {
            panic!("the compact projection must retain exactly one primary caret");
        };
        assert!(*local_caret <= line.text.len());
        assert_eq!(
            line.source.start + *local_caret,
            173,
            "horizontal projection preserves the canonical document byte across fonts"
        );
        let rect = after.semantic(&UiKey::new("editor/buffer")).unwrap().rect;
        let geometry = crate::native_ui::text_viewport_geometry(rect);
        assert_eq!(
            crate::native_ui::text_viewport_byte_at(
                spec,
                rect,
                geometry.text_x,
                geometry.body_y + geometry.line_h / 2.0,
            ),
            Some(line.source.start),
            "the left painted cell maps to the horizontally shifted canonical source"
        );
        let raster = |compiled: &crate::native_ui::CompiledUi| {
            crate::tray_raster::rasterize_tray(
                &compiled.tray(aterm_render::Theme::default(), 13.0).prims,
                474,
                468,
                1.0,
                [0, 0, 0, 255],
            )
            .0
        };
        assert_ne!(
            raster(&before),
            raster(&after),
            "caret reveal changes the real compact editor pixels"
        );
        after.validate_parity().unwrap();
    }

    #[test]
    fn command_palette_count_copy_uses_match_and_matches() {
        assert_eq!(command_palette_status(0, true), "M-x · no matches");
        assert_eq!(command_palette_status(1, true), "M-x · 1 match");
        assert_eq!(command_palette_status(2, true), "M-x · 2 matches");
        assert_eq!(
            command_palette_status(0, false),
            "M-x · 0 matches · ↑/↓ choose · Tab complete · Enter run · Esc close"
        );
        assert_eq!(
            command_palette_status(1, false),
            "M-x · 1 match · ↑/↓ choose · Tab complete · Enter run · Esc close"
        );
        assert_eq!(
            command_palette_status(2, false),
            "M-x · 2 matches · ↑/↓ choose · Tab complete · Enter run · Esc close"
        );
    }

    #[test]
    fn editor_recovery_wins_and_long_minibuffer_keeps_the_live_tail() {
        let (mut app, mut view, snapshot) = editor_fixture("", "empty.md");
        let viewport = LogicalRect::new(0.0, 0.0, 474.0, 468.0);
        let clean = app
            .view(
                &view,
                &ViewCx {
                    viewport,
                    config_revision: 1,
                    update_revision: 1,
                    animation_phase_ms: 720,
                    motion: ViewMotionCx::default(),
                    terminal_font_px: 12.0,
                    terminal_theme: aterm_render::Theme::default(),
                    semantic_font: None,
                    document: Some(&snapshot),
                },
            )
            .compile(viewport)
            .unwrap();
        assert_eq!(
            editor_spec(&clean).status.as_deref(),
            Some("Empty buffer · Ready")
        );
        let clean_pixels = crate::tray_raster::rasterize_tray(
            &clean.tray(aterm_render::Theme::default(), 13.0).prims,
            474,
            468,
            1.0,
            [0, 0, 0, 255],
        )
        .0;

        app.dirty = true;
        let dirty = app
            .view(
                &view,
                &ViewCx {
                    viewport,
                    config_revision: 1,
                    update_revision: 1,
                    animation_phase_ms: 720,
                    motion: ViewMotionCx::default(),
                    terminal_font_px: 12.0,
                    terminal_theme: aterm_render::Theme::default(),
                    semantic_font: None,
                    document: Some(&snapshot),
                },
            )
            .compile(viewport)
            .unwrap();
        assert_eq!(
            editor_spec(&dirty).status.as_deref(),
            Some("Modified · Cmd-S to save · 1 line")
        );
        let two_line_projection = crate::native_editor::EditorViewportProjection {
            first_line: 0,
            total_lines: 2,
            lines: Vec::new(),
        };
        assert_eq!(
            editor_status_message(&app, &view, Some(&two_line_projection), false).as_deref(),
            Some("Modified · Cmd-S to save · 2 lines")
        );
        let dirty_pixels = crate::tray_raster::rasterize_tray(
            &dirty.tray(aterm_render::Theme::default(), 13.0).prims,
            474,
            468,
            1.0,
            [0, 0, 0, 255],
        )
        .0;
        assert_ne!(
            clean_pixels, dirty_pixels,
            "dirty state changes real pixels"
        );

        app.recovery_status = Some("journal restored; review before saving".to_string());
        view.status = Some("Mark set".to_string());
        let buffer = view.buffer.as_mut().unwrap();
        buffer.minibuffer = crate::native_editor::Minibuffer::Command {
            query: format!("{}TAIL", "λ".repeat(200)),
            selected: 0,
        };
        view.preedit = "終".to_string();
        let recovered = app
            .view(
                &view,
                &ViewCx {
                    viewport,
                    config_revision: 1,
                    update_revision: 1,
                    animation_phase_ms: 720,
                    motion: ViewMotionCx::default(),
                    terminal_font_px: 12.0,
                    terminal_theme: aterm_render::Theme::default(),
                    semantic_font: None,
                    document: Some(&snapshot),
                },
            )
            .compile(viewport)
            .unwrap();
        let spec = editor_spec(&recovered);
        assert_eq!(
            spec.status.as_deref(),
            Some("Recovery · journal restored; review before saving")
        );
        let minibuffer = spec.minibuffer.as_deref().unwrap();
        assert!(minibuffer.starts_with("M-x …"));
        assert!(minibuffer.ends_with("TAIL終"));
        assert!(minibuffer.chars().count() <= 52);
        assert!(
            spec.preedit.is_empty(),
            "IME preedit belongs only to minibuffer"
        );
        recovered.validate_parity().unwrap();
    }

    #[test]
    fn middle_label_is_utf8_safe_and_retains_filename_identity() {
        let label = bounded_middle_label("prefix-🦀-a-very-long-document.md", 18);
        assert!(label.starts_with("prefix-🦀"));
        assert!(label.ends_with("ument.md"));
        assert_eq!(label.graphemes().count(), 18);
        assert_eq!(bounded_middle_label("abc", 1), "…");
        assert_eq!(bounded_middle_label("abc", 0), "");
        assert_eq!(bounded_middle_label("A👩‍💻BCDE", 4), "A👩‍💻…E");
        assert_eq!(bounded_middle_label("ABCDEé", 3), "A…é");
        assert_eq!(bounded_middle_label("ABCDE🇺🇳", 3), "A…🇺🇳");
    }

    #[test]
    fn middle_label_fits_proportional_pixel_budget() {
        use crate::widget::TextFace;

        let _ = crate::native_appearance::install_preferences(
            crate::native_appearance::AppearancePreferences::default(),
        );
        crate::tray_raster::prepare_ui_fonts_for_direct_view_test();
        let px = 20.0 * crate::native_appearance::text_scale();
        let max_width = 160.0;
        let title = "aterm-native-final-sample-with-a-distinguishing-suffix.md";
        let label = bounded_middle_label_to_width(title, max_width, px);
        assert!(crate::tray_raster::ui_text_width_for(TextFace::UiBold, &label, px) <= max_width);
        assert!(label.starts_with("aterm"));
        assert!(label.ends_with(".md"));
        assert_eq!(
            bounded_middle_label_to_width("short.md", 500.0, px),
            "short.md"
        );
        assert_eq!(bounded_middle_label_to_width(title, 0.0, px), "");
    }

    #[test]
    fn markdown_header_keeps_identity_without_redundant_compact_fragment() {
        use crate::widget::TextFace;

        let _ = crate::native_appearance::install_preferences(
            crate::native_appearance::AppearancePreferences::default(),
        );
        crate::tray_raster::prepare_ui_fonts_for_direct_view_test();
        let title = "aterm-native-final-sample-with-a-distinguishing-suffix.md";
        let mut documents = crate::document_store::DocumentStore::new();
        let document = documents.open(
            "file:///long-title.md".to_string(),
            "# Reader\n".to_string(),
        );
        let app = MarkdownApp::new(document, title.to_string(), "# Reader\n");
        let view = MarkdownViewState::default();
        let px = 20.0 * crate::native_appearance::text_scale();

        {
            let viewport = LogicalRect::new(0.0, 0.0, 928.0, 620.0);
            let compiled = app
                .view(
                    &view,
                    &ViewCx {
                        viewport,
                        config_revision: 1,
                        update_revision: 1,
                        animation_phase_ms: 720,
                        motion: ViewMotionCx::default(),
                        terminal_font_px: 12.0,
                        terminal_theme: aterm_render::Theme::default(),
                        semantic_font: None,
                        document: None,
                    },
                )
                .compile(viewport)
                .unwrap();
            let visual = compiled.semantic(&UiKey::new("markdown/title")).unwrap();
            assert!(
                crate::tray_raster::ui_text_width_for(TextFace::UiBold, &visual.label, px)
                    <= visual.rect.width - 4.0 + 0.01,
                "{}px title paints inside its exact flexible rect",
                viewport.width,
            );
            assert!(
                visual.label.starts_with("at"),
                "{}px title lost its identity-bearing prefix: {}",
                viewport.width,
                visual.label,
            );
            assert!(visual.label.ends_with(".md"));
            assert_eq!(
                compiled
                    .semantic(&UiKey::new("markdown/header"))
                    .unwrap()
                    .label,
                format!("Markdown reader: {title}"),
                "visual elision never truncates the semantic document identity",
            );
            compiled.validate_parity().unwrap();
        }

        for viewport in [
            LogicalRect::new(0.0, 0.0, 474.0, 468.0),
            LogicalRect::new(0.0, 0.0, 286.5, 558.0),
        ] {
            let compiled = app
                .view(
                    &view,
                    &ViewCx {
                        viewport,
                        config_revision: 1,
                        update_revision: 1,
                        animation_phase_ms: 720,
                        motion: ViewMotionCx::default(),
                        terminal_font_px: 12.0,
                        terminal_theme: aterm_render::Theme::default(),
                        semantic_font: None,
                        document: None,
                    },
                )
                .compile(viewport)
                .unwrap();
            assert!(
                compiled.semantic(&UiKey::new("markdown/title")).is_none(),
                "compact chrome must not paint an opaque filename fragment"
            );
            assert_eq!(
                compiled
                    .semantic(&UiKey::new("markdown/header"))
                    .unwrap()
                    .label,
                format!("Markdown reader: {title}"),
                "the tab/header semantics retain complete document identity"
            );
            assert!(compiled.paint_audit_lines()[0].contains("overflow=0"));
            compiled.validate_parity().unwrap();
        }
    }
}
