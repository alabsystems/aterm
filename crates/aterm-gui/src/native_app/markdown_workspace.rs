// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! One editor document, with source and formatted views sharing its lifecycle.

use super::*;
use crate::native_ui::{
    ButtonSpec, Control, ControlState, GroupSpec, Insets, Layout, Length, SemanticRole, StyleRef,
    TextSpec, UiContent, UiNode,
};

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub(crate) enum MarkdownWorkspaceMode {
    Read,
    #[default]
    Edit,
    Split,
}

impl MarkdownWorkspaceMode {
    pub(crate) const fn label(self) -> &'static str {
        match self {
            Self::Read => "Read",
            Self::Edit => "Edit",
            Self::Split => "Split",
        }
    }

    const fn action(self) -> &'static str {
        match self {
            Self::Read => "editor/markdown/read",
            Self::Edit => "editor/markdown/edit",
            Self::Split => "editor/markdown/split",
        }
    }
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub(crate) enum MarkdownPane {
    #[default]
    Editor,
    Reader,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct MarkdownWorkspaceView {
    pub(crate) mode: MarkdownWorkspaceMode,
    pub(crate) focus: MarkdownPane,
    pub(crate) reader: MarkdownViewState,
    pub(crate) split_per_mille: u16,
    /// A Read round-trip without navigation must not move the editor's selection or caret.
    pub(crate) reader_entry_anchor: Option<usize>,
}

impl MarkdownWorkspaceView {
    pub(crate) fn new(mode: MarkdownWorkspaceMode) -> Self {
        Self {
            mode,
            focus: if mode == MarkdownWorkspaceMode::Read {
                MarkdownPane::Reader
            } else {
                MarkdownPane::Editor
            },
            reader: MarkdownViewState::default(),
            split_per_mille: 500,
            reader_entry_anchor: None,
        }
    }

    pub(crate) fn editor_focused(&self) -> bool {
        match self.mode {
            MarkdownWorkspaceMode::Read => false,
            MarkdownWorkspaceMode::Edit => true,
            MarkdownWorkspaceMode::Split => self.focus == MarkdownPane::Editor,
        }
    }

    pub(crate) fn reader_focused(&self) -> bool {
        !self.editor_focused()
    }

    /// Reader positions follow the same canonical byte transforms as editor selections.
    /// This also runs on the source view: its editor already observed its own commit, while
    /// its preview has not. Edits above a paragraph preserve its wrapped-row offset.
    pub(crate) fn observe_document_edits(&mut self, deltas: &[crate::document_store::EditDelta]) {
        use crate::document_store::rebase_position;

        if deltas.iter().any(|delta| {
            delta.old.start <= self.reader.source_anchor
                && delta.old.end >= self.reader.source_anchor
        }) {
            self.reader.visual_row = 0;
        }
        self.reader.source_anchor = rebase_position(self.reader.source_anchor, deltas);
        if let Some(range) = &mut self.reader.selection {
            range.start = rebase_position(range.start, deltas);
            range.end = rebase_position(range.end, deltas);
            if range.start >= range.end {
                self.reader.selection = None;
            }
        }
        if let Some(anchor) = &mut self.reader_entry_anchor {
            *anchor = rebase_position(*anchor, deltas);
        }
        self.reader.history.map_locations(|mut location| {
            if deltas.iter().any(|delta| {
                delta.old.start <= location.source_anchor && delta.old.end >= location.source_anchor
            }) {
                location.visual_row = 0;
            }
            location.source_anchor = rebase_position(location.source_anchor, deltas);
            location
        });
    }
}

impl Default for MarkdownWorkspaceView {
    fn default() -> Self {
        Self::new(MarkdownWorkspaceMode::Edit)
    }
}

pub(crate) fn is_markdown_uri(uri: &str) -> bool {
    let path = amacs::protocol::uri::file_uri_to_path(uri).ok();
    let name = path.as_ref().and_then(|path| path.to_str()).unwrap_or(uri);
    amacs::language::Language::detect(name, "") == amacs::language::Language::Markdown
}

/// Resolve only the clicked local Markdown target. This performs no filesystem access or
/// grant expansion; the host admits the returned exact URI through its normal async path.
pub(crate) fn resolve_markdown_link(
    source_uri: &str,
    destination: &str,
) -> Result<(String, Option<String>), String> {
    use amacs::protocol::uri::{decode_uri_component, file_uri_to_path, path_to_file_uri};
    if destination.len() > 4096
        || destination
            .chars()
            .any(|c| c.is_whitespace() || c.is_control())
    {
        return Err("The Markdown document link is invalid".to_string());
    }
    let (target, fragment) = destination
        .split_once('#')
        .map_or((destination, None), |(target, fragment)| {
            (target, Some(fragment))
        });
    if target.is_empty()
        || target.contains(['?', ':'])
        || target.starts_with("//")
        || target.contains('\\')
    {
        return Err("Only local Markdown document paths are supported".to_string());
    }
    let source = file_uri_to_path(source_uri).map_err(|error| error.to_string())?;
    let parent = source
        .parent()
        .ok_or_else(|| "The source document has no parent directory".to_string())?;
    let parent_uri = path_to_file_uri(parent).map_err(|error| error.to_string())?;
    let uri = if target.starts_with('/') {
        format!("file://{target}")
    } else {
        format!(
            "{}{target}",
            if parent_uri.ends_with('/') {
                parent_uri
            } else {
                format!("{parent_uri}/")
            }
        )
    };
    let path = file_uri_to_path(&uri).map_err(|error| error.to_string())?;
    if amacs::language::Language::detect(path.to_str().unwrap_or_default(), "")
        != amacs::language::Language::Markdown
    {
        return Err("The link does not name a Markdown document".to_string());
    }
    let fragment = fragment
        .filter(|fragment| !fragment.is_empty())
        .map(decode_uri_component)
        .transpose()
        .map_err(|error| error.to_string())?;
    if fragment
        .as_ref()
        .is_some_and(|fragment| fragment.chars().any(char::is_control))
    {
        return Err("The heading fragment is invalid".to_string());
    }
    Ok((
        path_to_file_uri(&path).map_err(|error| error.to_string())?,
        fragment,
    ))
}

/// The exact inner shells used by painting, pointer input, scrolling and IME.
#[derive(Clone, Copy, Debug, PartialEq)]
pub(crate) struct MarkdownWorkspaceLayout {
    pub(crate) toolbar: LogicalRect,
    pub(crate) editor: Option<LogicalRect>,
    pub(crate) reader: Option<LogicalRect>,
    pub(crate) divider: Option<LogicalRect>,
    pub(crate) stacked: bool,
    notice: Option<LogicalRect>,
    status: Option<LogicalRect>,
    editor_frame: Option<LogicalRect>,
    reader_frame: Option<LogicalRect>,
    pane_header: f32,
}

impl MarkdownWorkspaceLayout {
    pub(crate) fn resize_per_mille(self, x: f32, y: f32) -> u16 {
        let (Some(first), Some(last), Some(divider)) =
            (self.editor_frame, self.reader_frame, self.divider)
        else {
            return 500;
        };
        let (position, start, room) = if self.stacked {
            (y, first.y, last.bottom() - first.y - divider.height)
        } else {
            (x, first.x, last.right() - first.x - divider.width)
        };
        if !position.is_finite() || room <= 0.0 {
            return 500;
        }
        ((position - start) / room * 1000.0)
            .round()
            .clamp(200.0, 800.0) as u16
    }
}

impl EditorApp {
    pub(crate) fn markdown_mode(&self, view: &EditorViewState) -> Option<MarkdownWorkspaceMode> {
        self.markdown.as_ref()?;
        if self.concealment().is_some() {
            return Some(MarkdownWorkspaceMode::Edit);
        }
        Some(
            view.markdown
                .as_ref()
                .map_or(MarkdownWorkspaceMode::Edit, |view| view.mode),
        )
    }

    /// Change presentation only. Document text, undo, save state and edit-ticket lifetime stay
    /// with the canonical EditorApp and EditorBufferView throughout.
    pub(crate) fn set_markdown_mode(
        &self,
        view: &mut EditorViewState,
        mode: MarkdownWorkspaceMode,
    ) -> bool {
        if self.markdown.is_none()
            || (mode != MarkdownWorkspaceMode::Edit && self.concealment().is_some())
            || !view.preedit.is_empty()
            || view
                .buffer
                .as_ref()
                .is_some_and(|buffer| buffer.minibuffer_active())
        {
            return false;
        }
        let workspace = view
            .markdown
            .get_or_insert_with(MarkdownWorkspaceView::default);
        let previous = workspace.mode;
        if previous == mode {
            return false;
        }
        if let Some(buffer) = view.buffer.as_mut() {
            if mode == MarkdownWorkspaceMode::Read {
                // Expanding an existing preview keeps its independent reading position.
                // Only a source-only view needs to find its corresponding reader paragraph.
                if previous == MarkdownWorkspaceMode::Edit {
                    workspace.reader.source_anchor = buffer.viewport_anchor;
                    workspace.reader.visual_row = 0;
                }
                workspace.reader_entry_anchor = Some(workspace.reader.source_anchor);
            } else if previous == MarkdownWorkspaceMode::Read {
                let anchor = workspace.reader.source_anchor;
                if workspace.reader_entry_anchor != Some(anchor) {
                    buffer.viewport_anchor = anchor;
                    let selection = buffer.primary_selection();
                    if selection.anchor == selection.head {
                        buffer.selections = vec![crate::native_editor::Selection::caret(anchor)];
                        buffer.primary = 0;
                    }
                }
            } else if mode == MarkdownWorkspaceMode::Split {
                workspace.reader.source_anchor = buffer.viewport_anchor;
                workspace.reader.visual_row = 0;
            }
        }
        workspace.mode = mode;
        workspace.focus = if mode == MarkdownWorkspaceMode::Read {
            MarkdownPane::Reader
        } else {
            MarkdownPane::Editor
        };
        view.common.last_focus = if workspace.editor_focused() {
            Some(UiKey::new("editor/buffer"))
        } else {
            None
        };
        view.common.retire_input();
        view.common.presentation_revision = view.common.presentation_revision.saturating_add(1);
        true
    }

    pub(crate) fn workspace_layout(
        &self,
        view: &EditorViewState,
        viewport: LogicalRect,
    ) -> MarkdownWorkspaceLayout {
        let Some(mode) = self.markdown_mode(view) else {
            return MarkdownWorkspaceLayout {
                toolbar: LogicalRect {
                    height: 0.0,
                    ..viewport
                },
                editor: Some(viewport),
                reader: None,
                divider: None,
                stacked: false,
                notice: None,
                status: None,
                editor_frame: Some(viewport),
                reader_frame: None,
                pane_header: 0.0,
            };
        };
        let scale = markdown_text_scale();
        let row = (crate::native_ui::native_button_line_height() + 20.0).max(40.0);
        let narrow_toolbar = viewport.width < 620.0 * scale;
        let toolbar_h = (row * if narrow_toolbar { 2.0 } else { 1.0 }).min(viewport.height);
        let toolbar = LogicalRect {
            height: toolbar_h,
            ..viewport
        };
        let notice =
            (self.disk_notice.is_some() || self.disk_conflict || view.edit_header.is_some())
                .then_some(LogicalRect {
                    y: viewport.y + toolbar_h,
                    height: row.min((viewport.height - toolbar_h).max(0.0)),
                    ..viewport
                });
        let notice_h = notice.map_or(0.0, |rect| rect.height);
        let messages = read_notices(self, view);
        let status =
            (mode == MarkdownWorkspaceMode::Read && !messages.is_empty()).then_some(LogicalRect {
                y: viewport.y + toolbar_h + notice_h,
                height: (row * messages.len() as f32)
                    .min((viewport.height - toolbar_h - notice_h).max(0.0)),
                ..viewport
            });
        let chrome_h = toolbar_h + notice_h + status.map_or(0.0, |rect| rect.height);
        let body = LogicalRect {
            y: viewport.y + chrome_h,
            height: (viewport.height - chrome_h).max(0.0),
            ..viewport
        };
        let stacked = mode == MarkdownWorkspaceMode::Split && body.width < 900.0 * scale;
        let pane_header = if mode == MarkdownWorkspaceMode::Split {
            row.min(32.0 * scale)
        } else {
            0.0
        };
        let fraction = view
            .markdown
            .as_ref()
            .map_or(500, |view| view.split_per_mille)
            .clamp(200, 800);
        let share = f32::from(fraction) / 1000.0;
        let (editor_frame, reader_frame, divider) = match mode {
            MarkdownWorkspaceMode::Edit => (Some(body), None, None),
            MarkdownWorkspaceMode::Read => (None, Some(body), None),
            MarkdownWorkspaceMode::Split if stacked => {
                let gap = 8.0_f32.min(body.height);
                let first = ((body.height - gap) * share).max(0.0);
                (
                    Some(LogicalRect {
                        height: first,
                        ..body
                    }),
                    Some(LogicalRect {
                        y: body.y + first + gap,
                        height: (body.height - first - gap).max(0.0),
                        ..body
                    }),
                    Some(LogicalRect {
                        y: body.y + first,
                        height: gap,
                        ..body
                    }),
                )
            }
            MarkdownWorkspaceMode::Split => {
                let gap = 8.0_f32.min(body.width);
                let first = ((body.width - gap) * share).max(0.0);
                (
                    Some(LogicalRect {
                        width: first,
                        ..body
                    }),
                    Some(LogicalRect {
                        x: body.x + first + gap,
                        width: (body.width - first - gap).max(0.0),
                        ..body
                    }),
                    Some(LogicalRect {
                        x: body.x + first,
                        width: gap,
                        ..body
                    }),
                )
            }
        };
        let inner = |rect: LogicalRect| LogicalRect {
            y: rect.y + pane_header.min(rect.height),
            height: (rect.height - pane_header).max(0.0),
            ..rect
        };
        MarkdownWorkspaceLayout {
            toolbar,
            editor: editor_frame.map(inner),
            reader: reader_frame.map(inner),
            divider,
            stacked,
            notice,
            status,
            editor_frame,
            reader_frame,
            pane_header,
        }
    }

    pub(super) fn markdown_action(
        &self,
        view: &mut EditorViewState,
        invocation: &ActionInvocation,
    ) -> bool {
        if self.markdown.is_none() {
            return false;
        }
        let action = invocation.id.as_str();
        let mode = match action {
            "editor/markdown/read" | "markdown/mode/preview" => Some(MarkdownWorkspaceMode::Read),
            "editor/markdown/edit" | "markdown/edit" | "markdown/mode/source" => {
                Some(MarkdownWorkspaceMode::Edit)
            }
            "editor/markdown/split" | "markdown/mode/split" => Some(MarkdownWorkspaceMode::Split),
            "editor/markdown/toggle" => Some(match self.markdown_mode(view) {
                Some(MarkdownWorkspaceMode::Edit) => MarkdownWorkspaceMode::Read,
                Some(MarkdownWorkspaceMode::Read) => MarkdownWorkspaceMode::Edit,
                _ => {
                    if let Some(workspace) = &mut view.markdown {
                        workspace.focus = MarkdownPane::Editor;
                    }
                    view.common.last_focus = Some(UiKey::new("editor/buffer"));
                    view.common.retire_input();
                    view.common.presentation_revision =
                        view.common.presentation_revision.saturating_add(1);
                    return true;
                }
            }),
            "editor/markdown/focus-editor" | "editor/markdown/focus-reader" => {
                if !view.preedit.is_empty()
                    || view
                        .buffer
                        .as_ref()
                        .is_some_and(|buffer| buffer.minibuffer_active())
                {
                    return true;
                }
                if let Some(workspace) = &mut view.markdown {
                    workspace.focus = if action.ends_with("focus-editor") {
                        MarkdownPane::Editor
                    } else {
                        MarkdownPane::Reader
                    };
                    view.common.last_focus = workspace
                        .editor_focused()
                        .then(|| UiKey::new("editor/buffer"));
                    view.common.retire_input();
                    view.common.presentation_revision =
                        view.common.presentation_revision.saturating_add(1);
                }
                return true;
            }
            "editor/markdown/resize" => {
                if let (Some(workspace), Some(SemanticInput::Number(value))) =
                    (&mut view.markdown, &invocation.value)
                    && value.is_finite()
                {
                    workspace.split_per_mille = value.round().clamp(200.0, 800.0) as u16;
                    view.common.presentation_revision =
                        view.common.presentation_revision.saturating_add(1);
                }
                return true;
            }
            _ => None,
        };
        if let Some(mode) = mode {
            self.set_markdown_mode(view, mode);
            true
        } else {
            false
        }
    }

    pub(super) fn reader_event(
        &mut self,
        view: &mut EditorViewState,
        event: AppEvent,
        cx: &mut UpdateCx<'_>,
    ) -> EventResult {
        if self.concealment().is_some() {
            return EventResult::Bubble;
        }
        let (Some(reader), Some(workspace)) = (&mut self.markdown, &mut view.markdown) else {
            return EventResult::Bubble;
        };
        workspace.reader.common = view.common.clone();
        let result = reader.update(&mut workspace.reader, event, cx);
        view.common = workspace.reader.common.clone();
        result
    }
}

pub(super) fn commands(app: &EditorApp, view: &EditorViewState, out: &mut Vec<Command>) {
    let Some(current) = app.markdown_mode(view) else {
        return;
    };
    let enabled = view.preedit.is_empty()
        && !view
            .buffer
            .as_ref()
            .is_some_and(|buffer| buffer.minibuffer_active());
    for (mode, title) in [
        (MarkdownWorkspaceMode::Read, "Read Document"),
        (MarkdownWorkspaceMode::Edit, "Edit Document"),
        (MarkdownWorkspaceMode::Split, "Split Edit and Preview"),
    ] {
        out.push(Command {
            id: ActionId::new(mode.action()),
            title: title.to_string(),
            shortcut: None,
            enabled: enabled
                && current != mode
                && (mode == MarkdownWorkspaceMode::Edit || app.concealment().is_none()),
        });
    }
    out.push(Command {
        id: ActionId::new("editor/markdown/toggle"),
        title: if current == MarkdownWorkspaceMode::Split {
            "Focus Editor"
        } else {
            "Toggle Read and Edit"
        }
        .to_string(),
        shortcut: Some("Cmd-E".to_string()),
        enabled,
    });
    if let (Some(reader), Some(workspace)) = (&app.markdown, &view.markdown)
        && workspace.reader_focused()
        && app.concealment().is_none()
    {
        let mut reader_commands = Vec::new();
        reader.commands(&workspace.reader, &mut reader_commands);
        out.extend(reader_commands.into_iter().filter(|command| {
            command.id.as_str() != "markdown/edit"
                && !command.id.as_str().starts_with("markdown/mode/")
        }));
    }
}

fn pane_cx<'a>(cx: &'a ViewCx<'a>, viewport: LogicalRect) -> ViewCx<'a> {
    ViewCx {
        viewport,
        animation_phase_ms: cx.animation_phase_ms,
        motion: cx.motion,
        terminal_font_px: cx.terminal_font_px,
        terminal_scale: cx.terminal_scale,
        semantic_font: cx.semantic_font.clone(),
        document: cx.document,
    }
}

fn button(
    key: &str,
    label: &str,
    visual: &str,
    action: &str,
    selected: bool,
    enabled: bool,
) -> UiNode {
    UiNode::new(
        key,
        UiContent::Button(
            Control::new(
                ButtonSpec::new(label).visual_label(visual),
                ActionId::new(action),
            )
            .state(ControlState {
                selected,
                enabled,
                ..ControlState::default()
            })
            .style(StyleRef::Navigation),
        ),
    )
    .layout(Layout::default().width(Length::Fill).height(Length::Fill))
}

/// Ticket completion and conflict choices belong to the document, in every presentation.
fn document_notice(app: &EditorApp, view: &EditorViewState, rect: LogicalRect) -> UiNode {
    let enabled = view.buffer.is_some()
        && !view
            .buffer
            .as_ref()
            .is_some_and(|buffer| buffer.minibuffer_active())
        && !app.checkpoint_pending;
    let mut actions = Vec::new();
    let words = if let Some(notice) = app
        .disk_notice
        .or(app.disk_conflict.then_some(DiskNotice::Changed))
    {
        if notice.offers_reload() {
            actions.push(button(
                "editor/disk-reload",
                DISK_RELOAD_LABEL,
                DISK_RELOAD_WORDS,
                "editor/revert",
                false,
                enabled,
            ));
        }
        actions.push(button(
            "editor/disk-keep",
            notice.keep_label(),
            DISK_KEEP_WORDS,
            "editor/keep-local",
            false,
            enabled,
        ));
        notice.words().to_string()
    } else if let Some(header) = &view.edit_header {
        actions.push(button(
            "editor/finish",
            &format!("Done: {}", header.done_label()),
            "Done",
            "editor/finish",
            false,
            enabled,
        ));
        if header.offers_abort() {
            actions.push(button(
                "editor/abort",
                &format!("Abandon: {}", header.abort_label()),
                "Abandon",
                "editor/abort",
                false,
                enabled,
            ));
        }
        header.words()
    } else {
        String::new()
    };
    let action_width = (82.0 * markdown_text_scale()).min(rect.width / actions.len().max(1) as f32);
    for action in &mut actions {
        action.layout.width = Length::Fixed(action_width);
    }
    let text = UiNode::new(
        "editor/markdown/notice-words",
        UiContent::Text(TextSpec {
            text: crate::native_ui::fit_native_status_label(
                &words,
                (rect.width - action_width * actions.len() as f32 - 32.0).max(0.0),
            ),
            role: SemanticRole::Status,
            style: StyleRef::Quiet,
        }),
    )
    .layout(Layout::default().width(Length::Fill).height(Length::Fill));
    let children = std::iter::once(text).chain(actions).collect();
    UiNode::new(
        "editor/markdown/notice",
        UiContent::Group(GroupSpec::new(words).style(StyleRef::Secondary)),
    )
    .layout(
        Layout::row()
            .height(Length::Fixed(rect.height))
            .width(Length::Fill)
            .gap(4.0)
            .padding(Insets::symmetric(8.0, 2.0))
            .clipped(),
    )
    .children(children)
}

fn read_notices<'a>(app: &'a EditorApp, view: &'a EditorViewState) -> Vec<&'a str> {
    let mut notices = Vec::new();
    for notice in [
        view.status.as_deref(),
        app.recovery_status.as_deref(),
        view.markdown
            .as_ref()
            .and_then(|workspace| workspace.reader.notice.as_deref()),
    ]
    .into_iter()
    .flatten()
    .filter(|notice| !notice.trim().is_empty())
    {
        if !notices.contains(&notice) {
            notices.push(notice);
        }
    }
    notices
}

pub(super) fn render(
    app: &EditorApp,
    view: &EditorViewState,
    cx: &ViewCx<'_>,
    source: impl Fn(&ViewCx<'_>) -> UiTree,
) -> UiTree {
    let Some(mode) = app.markdown_mode(view) else {
        return source(cx);
    };
    let layout = app.workspace_layout(view, cx.viewport);
    let fallback = MarkdownWorkspaceView::default();
    let workspace = view.markdown.as_ref().unwrap_or(&fallback);
    let controls_enabled = view.preedit.is_empty()
        && !view
            .buffer
            .as_ref()
            .is_some_and(|buffer| buffer.minibuffer_active());
    let reader_allowed = app.concealment().is_none();
    let modes = [
        MarkdownWorkspaceMode::Read,
        MarkdownWorkspaceMode::Edit,
        MarkdownWorkspaceMode::Split,
    ]
    .into_iter()
    .map(|choice| {
        button(
            choice.action(),
            &format!("{} Document (Cmd-E switches Read and Edit)", choice.label()),
            choice.label(),
            choice.action(),
            mode == choice,
            controls_enabled && (choice == MarkdownWorkspaceMode::Edit || reader_allowed),
        )
    })
    .collect::<Vec<_>>();
    let read_only = app.read_only
        || view
            .edit_header
            .as_ref()
            .is_some_and(|header| header.read_only);
    let status_words = if read_only {
        "Read-only"
    } else if app.checkpoint_pending {
        "Saving…"
    } else if app.disk_conflict {
        "Disk changed"
    } else if app.dirty {
        "Unsaved"
    } else {
        "Saved"
    };
    let title = UiNode::new(
        "editor/markdown/title",
        UiContent::Text(TextSpec {
            text: bounded_middle_label_to_width(
                &app.title,
                (cx.viewport.width - 390.0).max(80.0),
                14.0,
            ),
            role: SemanticRole::Heading,
            style: StyleRef::Primary,
        }),
    )
    .layout(Layout::default().width(Length::Fill).height(Length::Fill));
    let status = UiNode::new(
        "editor/markdown/save-status",
        UiContent::Text(TextSpec {
            text: status_words.to_string(),
            role: SemanticRole::Status,
            style: StyleRef::Quiet,
        }),
    )
    .layout(
        Layout::default()
            .width(Length::Fixed(108.0 * markdown_text_scale()))
            .height(Length::Fill),
    );
    let mut mode_controls = UiNode::new(
        "editor/markdown/modes",
        UiContent::Group(GroupSpec::new("Document view")),
    )
    .layout(
        Layout::row()
            .width(Length::Fixed(204.0 * markdown_text_scale()))
            .height(Length::Fill)
            .gap(4.0),
    )
    .children(modes);
    let save = button(
        "editor/markdown/save",
        "Save Document (Cmd-S)",
        "Save",
        "editor/save",
        false,
        app.can_save(view),
    )
    .layout(
        Layout::default()
            .width(Length::Fixed(64.0 * markdown_text_scale()))
            .height(Length::Fill),
    );
    let narrow = cx.viewport.width < 620.0 * markdown_text_scale();
    let toolbar_children = if narrow {
        mode_controls.layout.width = Length::Fill;
        let save_label = if app.dirty && !read_only && !app.checkpoint_pending && !app.disk_conflict
        {
            "Save · unsaved"
        } else {
            status_words
        };
        let save_status = button(
            "editor/markdown/save-status",
            &format!("{save_label}. Save Document (Cmd-S)"),
            save_label,
            "editor/save",
            false,
            app.can_save(view),
        )
        .layout(
            Layout::default()
                .width(Length::Fixed(
                    (150.0 * markdown_text_scale()).min(cx.viewport.width * 0.62),
                ))
                .height(Length::Fill),
        );
        vec![
            UiNode::new(
                "editor/markdown/identity",
                UiContent::Group(GroupSpec::unlabeled(SemanticRole::Group)),
            )
            .layout(Layout::row().height(Length::Fill).gap(8.0))
            .children(vec![title, save_status]),
            UiNode::new(
                "editor/markdown/actions",
                UiContent::Group(GroupSpec::unlabeled(SemanticRole::Group)),
            )
            .layout(Layout::row().height(Length::Fill).gap(8.0))
            .children(vec![mode_controls]),
        ]
    } else {
        vec![title, mode_controls, save, status]
    };
    let toolbar = UiNode::new(
        "editor/markdown/toolbar",
        UiContent::Group(GroupSpec::new("Markdown document").style(StyleRef::Secondary)),
    )
    .layout(
        (if narrow {
            Layout::column()
        } else {
            Layout::row()
        })
        .height(Length::Fixed(layout.toolbar.height))
        .width(Length::Fill)
        .gap(4.0)
        .padding(Insets::symmetric(8.0, 2.0))
        .clipped(),
    )
    .children(toolbar_children);
    let pane = |which: MarkdownPane, rect: LogicalRect, mut content: UiNode| {
        content.layout.width = Length::Fill;
        content.layout.height = Length::Fill;
        let (key, word, action, focused) = match which {
            MarkdownPane::Editor => (
                "editor/markdown/editor-pane",
                "Edit",
                "editor/markdown/focus-editor",
                workspace.editor_focused(),
            ),
            MarkdownPane::Reader => (
                "editor/markdown/reader-pane",
                "Read",
                "editor/markdown/focus-reader",
                workspace.reader_focused(),
            ),
        };
        let mut children = Vec::new();
        if layout.pane_header > 0.0 {
            let visual = if focused {
                format!("{word} · focused")
            } else {
                word.to_string()
            };
            children.push(
                button(
                    &format!("{key}/focus"),
                    &format!("{visual}: focus {word} pane"),
                    &visual,
                    action,
                    focused,
                    true,
                )
                .layout(
                    Layout::default()
                        .height(Length::Fixed(layout.pane_header))
                        .width(Length::Fill),
                ),
            );
        }
        children.push(content);
        UiNode::new(
            key,
            UiContent::Group(GroupSpec::new(format!("{word} pane"))),
        )
        .layout(
            Layout::column()
                .width(Length::Fixed(rect.width))
                .height(Length::Fixed(rect.height))
                .clipped(),
        )
        .children(children)
    };
    let mut panes = Vec::new();
    if let (Some(rect), Some(frame)) = (layout.editor, layout.editor_frame) {
        let tree = source(&pane_cx(cx, rect));
        panes.push(pane(MarkdownPane::Editor, frame, tree.root));
    }
    if let Some(rect) = layout.divider {
        panes.push(
            button(
                "editor/markdown/divider",
                "Resize editor and preview panes",
                "",
                "editor/markdown/resize",
                false,
                true,
            )
            .layout(
                Layout::default()
                    .width(Length::Fixed(rect.width))
                    .height(Length::Fixed(rect.height)),
            ),
        );
    }
    if let (Some(rect), Some(frame), Some(reader)) =
        (layout.reader, layout.reader_frame, app.markdown.as_ref())
    {
        let mut reader_view = workspace.reader.clone();
        reader_view.common = view.common.clone();
        reader_view.mode = MarkdownViewMode::Preview;
        let tree = reader.view(&reader_view, &pane_cx(cx, rect));
        panes.push(pane(MarkdownPane::Reader, frame, tree.root));
    }
    let body = UiNode::new(
        "editor/markdown/body",
        UiContent::Group(GroupSpec::unlabeled(SemanticRole::Group)),
    )
    .layout(
        (if layout.stacked {
            Layout::column()
        } else {
            Layout::row()
        })
        .width(Length::Fill)
        .height(Length::Fill)
        .clipped(),
    )
    .children(panes);
    let mut children = vec![toolbar];
    if let Some(rect) = layout.notice {
        children.push(document_notice(app, view, rect));
    }
    if let Some(rect) = layout.status {
        let notices = read_notices(app, view);
        let text = notices
            .iter()
            .map(|notice| {
                crate::native_ui::fit_native_status_label(notice, (rect.width - 24.0).max(0.0))
            })
            .collect::<Vec<_>>()
            .join("\n");
        children.push(
            UiNode::new(
                "editor/markdown/status",
                UiContent::Group(GroupSpec::new(notices.join("\n")).style(StyleRef::Secondary)),
            )
            .layout(
                Layout::default()
                    .height(Length::Fixed(rect.height))
                    .padding(Insets::symmetric(12.0, 4.0))
                    .clipped(),
            )
            .children(vec![UiNode::new(
                "editor/markdown/status-words",
                UiContent::Text(TextSpec {
                    text,
                    role: SemanticRole::Status,
                    style: StyleRef::Quiet,
                }),
            )]),
        );
    }
    children.push(body);
    UiTree::new(
        UiNode::new(
            "editor/markdown/workspace",
            UiContent::Group(GroupSpec::unlabeled(SemanticRole::Application)),
        )
        .layout(Layout::column().clipped())
        .children(children),
    )
}

#[cfg(test)]
#[path = "markdown_workspace_tests.rs"]
mod tests;
