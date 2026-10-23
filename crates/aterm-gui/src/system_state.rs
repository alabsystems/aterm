// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! The System State tab: one editable setup plan and its measured progress.

use std::collections::VecDeque;
use std::time::Instant;

use aterm_grapheme::GraphemeClusters;
use aterm_secrets::SecretBytes;
use aterm_types::macos_setup::{Plan, SECRETS_SUBSCRIBER_ROW, SECRETS_VAULT_ROW, Setting};

use crate::native_app::{
    AppDescriptor, AppEvent, AppIcon, AppIndicators, AppKind, AppPresentation, CloseReadiness,
    CloseRequest, CloseScope, Command, CommonViewState, EventResult, NativeAppModel, SemanticInput,
    TextInputEvent, UpdateCx, ViewCx,
};
use crate::native_text_input::{TextInputProjection, TextInputState, TextSelection};
use crate::native_ui::{
    ActionId, ButtonSpec, Control, ControlState, GroupSpec, Insets, Layout, Length, SemanticRole,
    SemanticValue, StyleRef, SwitchSpec, TextFieldSpec, TextSpec, UiContent, UiKey, UiNode, UiTree,
};

use crate::workstation_setup::{Clock, Request, SetupAnswer, WorkstationSetupDecision};

/// A non-secret snapshot published by setup's worker.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct SetupStatus {
    pub plan: Option<Plan>,
    pub check: Option<SetupCheck>,
    pub permissions: Option<InitialSetupPermissionPolicy>,
    pub busy: bool,
    pub finished: bool,
    pub rows: Vec<SetupStatusRow>,
}

/// A read-only observation of one exact reviewed checklist and vault source.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct SetupCheck {
    pub plan: Plan,
    pub rows: Vec<SetupStatusRow>,
}

/// Native permission guidance follows this durably admitted setup run.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct InitialSetupPermissionPolicy {
    pub revision: u64,
    pub allowed: bool,
}

/// One measured setup step, with no credential values.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct SetupStatusRow {
    pub name: String,
    pub status: String,
    pub detail: String,
}

impl SetupStatus {
    /// Whether setup's own report, not a Check's, reads this Mac in its vault, synced, with
    /// its subscriber running: the Secrets stage reported at least one [`SECRETS_VAULT_ROW`] row
    /// (its vault item's, or its read of a vault it left in place beside the row saying so)
    /// and every one `ok` or `applied` (a pass that joined, synced or installed the
    /// subscriber reports `applied`; a Mac still waiting for approval reports otherwise),
    /// and no [`SECRETS_SUBSCRIBER_ROW`] row is anything but `ok`. The record's own row
    /// for a stage a run finished, `applied` with no detail, says what a run did once, not
    /// what holds now, and is not read; a row the item reported always says why.
    pub(crate) fn vault_synced(&self) -> bool {
        let mut reported = self
            .rows
            .iter()
            .filter(|row| {
                row.name == SECRETS_VAULT_ROW && !(row.status == "applied" && row.detail.is_empty())
            })
            .peekable();
        reported.peek().is_some()
            && reported.all(|row| matches!(row.status.as_str(), "ok" | "applied"))
            && self
                .rows
                .iter()
                .all(|row| row.name != SECRETS_SUBSCRIBER_ROW || row.status == "ok")
    }

    fn rows_for(
        &self,
        plan: Option<&Plan>,
    ) -> impl DoubleEndedIterator<Item = &SetupStatusRow> + Clone {
        let check = self
            .check
            .as_ref()
            .filter(|check| plan.is_none_or(|plan| check.plan == *plan));
        self.rows
            .iter()
            .chain(check.into_iter().flat_map(|check| check.rows.iter()))
    }

    pub(crate) fn needs_attention(&self, plan: Option<&Plan>) -> bool {
        !self.busy
            && self.rows_for(plan).any(|row| {
                matches!(
                    row.status.as_str(),
                    "drift" | "blocked" | "error" | "failed" | "unverified" | "not started"
                )
            })
    }

    fn summary(&self, plan: Option<&Plan>) -> Option<String> {
        let mut rows = self.rows_for(plan);
        let preferred = rows
            .clone()
            .filter_map(|row| {
                let priority = match row.status.as_str() {
                    "not started" => 5,
                    "error" | "failed" => 4,
                    "unverified" => 3,
                    "drift" | "blocked" => 2,
                    "pending" if !row.detail.is_empty() => 1,
                    "running" => 1,
                    "ok" | "applied" | "restored" | "stopped" => 0,
                    _ => return None,
                };
                Some((priority, row))
            })
            .reduce(|best, candidate| {
                // Show the earliest unmet dependency, including its approval
                // code, before a later stage merely waiting on it. Successful
                // observations instead keep the most recent result.
                if candidate.0 > best.0 || (candidate.0 == 0 && best.0 == 0) {
                    candidate
                } else {
                    best
                }
            });
        if let Some((priority, row)) = preferred
            && priority >= 3
        {
            return Some(row.words());
        }
        if self.busy {
            return Some(
                rows.rfind(|row| row.status == "running")
                    .map_or_else(|| "Working…".to_owned(), SetupStatusRow::words),
            );
        }
        preferred.map(|(_, row)| row.words())
    }
}

impl SetupStatusRow {
    fn words(&self) -> String {
        format!(
            "{}: {}{}",
            self.name,
            self.status,
            if self.detail.is_empty() {
                String::new()
            } else {
                format!(" · {}", self.detail)
            }
        )
    }
}

/// Empty preedit clears platform composition state without any typed input.
pub(crate) fn text_input_interacts(event: &TextInputEvent) -> bool {
    !matches!(event, TextInputEvent::Preedit { text, .. } if text.is_empty())
}

#[derive(Debug)]
pub(crate) enum SystemStateAction {
    Apply(SetupAnswer),
    Check(Plan),
    Restore,
    Cancel,
}

#[derive(Default)]
pub(crate) struct SystemStateViewState {
    pub(crate) common: CommonViewState,
    scroll: usize,
}

impl SystemStateViewState {
    pub(crate) fn reveal_listed_control(&mut self, key: &UiKey) -> bool {
        let Some(index) = key
            .as_str()
            .strip_prefix("system-state/setting/")
            .and_then(|value| value.parse::<usize>().ok())
            .filter(|index| *index < Setting::ALL.len())
        else {
            return false;
        };
        let changed = self.scroll != index;
        self.scroll = index;
        changed
    }
}

/// Password text never enters a UI projection, undo history, restore data or a
/// diagnostic String. Its selection addresses graphemes in the masked value.
#[derive(Default)]
struct Password {
    value: Option<SecretBytes>,
    anchor: usize,
    head: usize,
}

impl Password {
    fn text(&self) -> &str {
        self.value
            .as_ref()
            .and_then(SecretBytes::as_str)
            .unwrap_or_default()
    }

    fn boundaries(&self) -> Vec<usize> {
        self.text()
            .grapheme_indices()
            .map(|(byte, _)| byte)
            .chain([self.text().len()])
            .collect()
    }

    fn count(&self) -> usize {
        self.text().graphemes().count()
    }

    fn replace(&mut self, text: SecretBytes) {
        let boundaries = self.boundaries();
        let start = boundaries[self.anchor.min(self.head).min(boundaries.len() - 1)];
        let end = boundaries[self.anchor.max(self.head).min(boundaries.len() - 1)];
        let old = self.text().as_bytes();
        if old.len() - (end - start) + text.len() > 1023
            || text
                .as_slice()
                .iter()
                .any(|byte| matches!(byte, b'\n' | b'\r' | 0))
        {
            return;
        }
        let inserted_end = start + text.len();
        let mut bytes = Vec::with_capacity(old.len() - (end - start) + text.len());
        bytes.extend_from_slice(&old[..start]);
        bytes.extend_from_slice(text.as_slice());
        bytes.extend_from_slice(&old[end..]);
        self.value = Some(SecretBytes::new(bytes));
        // Insertion can join clusters on either side, as when a combining mark
        // follows a letter or a joiner connects two emoji. Resolve the caret in
        // the resulting value, at the first whole-cluster boundary after it.
        self.head = self
            .boundaries()
            .partition_point(|boundary| *boundary < inserted_end);
        self.anchor = self.head;
    }

    fn input(&mut self, event: TextInputEvent) {
        match event {
            TextInputEvent::Commit(text) => self.replace(SecretBytes::from(text)),
            TextInputEvent::Preedit { text, .. } => drop(SecretBytes::from(text)),
            TextInputEvent::SelectAll => {
                self.anchor = 0;
                self.head = self.count();
            }
            TextInputEvent::Left { extend } | TextInputEvent::WordLeft { extend } => {
                self.head = if !extend && self.anchor != self.head {
                    self.anchor.min(self.head)
                } else {
                    self.head.saturating_sub(1)
                };
                if !extend {
                    self.anchor = self.head;
                }
            }
            TextInputEvent::Right { extend } | TextInputEvent::WordRight { extend } => {
                self.head = if !extend && self.anchor != self.head {
                    self.anchor.max(self.head)
                } else {
                    (self.head + 1).min(self.count())
                };
                if !extend {
                    self.anchor = self.head;
                }
            }
            TextInputEvent::Home { extend } => {
                self.head = 0;
                if !extend {
                    self.anchor = 0;
                }
            }
            TextInputEvent::End { extend } => {
                self.head = self.count();
                if !extend {
                    self.anchor = self.head;
                }
            }
            TextInputEvent::Backspace | TextInputEvent::DeleteWordBackward => {
                if self.anchor == self.head {
                    self.anchor = self.head.saturating_sub(1);
                }
                self.replace(SecretBytes::new(Vec::new()));
            }
            TextInputEvent::Delete => {
                if self.anchor == self.head {
                    self.anchor = (self.head + 1).min(self.count());
                }
                self.replace(SecretBytes::new(Vec::new()));
            }
            TextInputEvent::KillToStart => {
                self.anchor = 0;
                self.replace(SecretBytes::new(Vec::new()));
            }
            TextInputEvent::KillToEnd => {
                self.anchor = self.count();
                self.replace(SecretBytes::new(Vec::new()));
            }
            TextInputEvent::Undo
            | TextInputEvent::Redo
            | TextInputEvent::Submit
            | TextInputEvent::Cancel => {}
        }
    }

    fn projection(&self) -> TextInputProjection {
        TextInputProjection {
            text: "•".repeat(self.count()),
            selection: TextSelection {
                anchor: self.anchor * "•".len(),
                head: self.head * "•".len(),
            },
            preedit: None,
        }
    }

    fn take(&mut self) -> Option<SecretBytes> {
        self.anchor = 0;
        self.head = 0;
        self.value.take().filter(|value| !value.is_empty())
    }
}

pub(crate) struct SystemStateApp {
    plan: Plan,
    submitted: Plan,
    url: TextInputState,
    password: Password,
    status: SetupStatus,
    request: Option<Request>,
    actions: VecDeque<SystemStateAction>,
    clock: Clock,
    clock_repaint: bool,
}

impl SystemStateApp {
    pub(crate) fn text_input(&self, key: &str) -> Option<&TextInputState> {
        (key == "system-state/url").then_some(&self.url)
    }

    pub(crate) fn field_projection(&self, key: &str) -> Option<TextInputProjection> {
        match key {
            "system-state/url" => Some(self.url.projection()),
            "system-state/password" => Some(self.password.projection()),
            _ => None,
        }
    }

    pub(crate) fn new(plan: Plan) -> Self {
        Self {
            url: TextInputState::new(plan.vault_url.clone()),
            submitted: plan.clone(),
            plan,
            password: Password::default(),
            status: SetupStatus::default(),
            request: None,
            actions: VecDeque::new(),
            clock: Clock::default(),
            clock_repaint: false,
        }
    }

    pub(crate) fn can_offer(&self) -> bool {
        !self.status.busy
            && !self.request.as_ref().is_some_and(Request::pending)
            && !self.has_unsubmitted_changes()
    }

    pub(crate) fn offer(&mut self, request: Request) -> bool {
        if !request.pending() || !self.can_offer() {
            request.finish(WorkstationSetupDecision::Unavailable);
            return false;
        }
        self.plan = request.initial().clone();
        self.submitted = self.plan.clone();
        self.url = TextInputState::new(self.plan.vault_url.clone());
        self.password = Password::default();
        self.clock = Clock::default();
        self.clock_repaint = false;
        self.status = SetupStatus::default();
        self.request = Some(request);
        true
    }

    pub(crate) fn publish(&mut self, status: SetupStatus) -> bool {
        let mut changed = self.status != status;
        if self.request.is_none()
            && !self.has_unsubmitted_changes()
            && !self.status.busy
            && !status.busy
            && let Some(plan) = &status.plan
            && (self.plan != *plan || self.url.value() != plan.vault_url)
        {
            self.plan = plan.clone();
            self.submitted = plan.clone();
            self.url = TextInputState::new(plan.vault_url.clone());
            changed = true;
        }
        self.status = status;
        changed
    }

    pub(crate) fn take_action(&mut self) -> Option<SystemStateAction> {
        self.actions.pop_front()
    }

    /// Final-view disposal transfers explicit cancellation to the host before
    /// the form's remaining deferred actions are dropped.
    pub(crate) fn take_queued_cancel(&mut self) -> bool {
        let before = self.actions.len();
        self.actions
            .retain(|action| !matches!(action, SystemStateAction::Cancel));
        self.actions.len() != before
    }

    pub(crate) fn has_queued_cancel(&self) -> bool {
        self.actions
            .iter()
            .any(|action| matches!(action, SystemStateAction::Cancel))
    }

    pub(crate) fn interact(&mut self) {
        self.clock_repaint |= self.request.is_some() && !self.clock.interacted();
        self.clock.interact();
    }

    pub(crate) fn tick(&mut self, now: Instant, active: bool) -> bool {
        let before = (
            self.clock.seconds(),
            self.request.is_some(),
            self.status.busy,
        );
        if self
            .request
            .as_ref()
            .is_some_and(|request| !request.pending())
        {
            self.request = None;
            self.interact();
        }
        if self.request.is_some() && !self.status.busy {
            self.clock.tick(now, active);
            if active && self.clock.expired() {
                self.apply();
            }
        }
        std::mem::take(&mut self.clock_repaint)
            || before
                != (
                    self.clock.seconds(),
                    self.request.is_some(),
                    self.status.busy,
                )
    }

    pub(crate) fn pending(&self) -> bool {
        self.request.as_ref().is_some_and(Request::pending)
    }

    pub(crate) fn offered_request_matches(&self, request: &Request) -> bool {
        self.request
            .as_ref()
            .is_some_and(|current| current.pending() && current.same_offer(request))
    }

    fn has_unsubmitted_changes(&self) -> bool {
        (self.pending() && self.clock.interacted())
            || !self.password.text().is_empty()
            || self.plan.selection != self.submitted.selection
            || self.url.value() != self.submitted.vault_url
    }

    fn cancel(&mut self) {
        if self.status.busy {
            return;
        }
        self.interact();
        self.request = None;
        self.password = Password::default();
        self.plan = self.submitted.clone();
        self.url = TextInputState::new(self.plan.vault_url.clone());
        self.actions.push_back(SystemStateAction::Cancel);
    }

    pub(crate) fn reviewed_plan(&self) -> Plan {
        let mut plan = self.plan.clone();
        plan.vault_url = self.url.value().trim().to_owned();
        plan
    }

    pub(crate) fn needs_attention(&self) -> bool {
        self.status.needs_attention(Some(&self.reviewed_plan()))
    }

    fn apply(&mut self) {
        if self.status.busy {
            return;
        }
        self.interact();
        let plan = self.reviewed_plan();
        let vault_typed = plan.vault_url != self.submitted.vault_url.trim();
        // The callback may refuse to start. Keep the reviewed fields and their
        // scrubbable password until the host confirms this answer was accepted.
        let password = self
            .password
            .value
            .clone()
            .filter(|value| !value.is_empty());
        self.status.busy = true;
        self.actions
            .push_back(SystemStateAction::Apply(SetupAnswer {
                decision: WorkstationSetupDecision::Begin(plan),
                password,
                receipt: None,
                cancel_receipt: None,
                vault_typed,
            }));
    }

    pub(crate) fn apply_accepted(&mut self, plan: Plan) -> bool {
        self.plan = plan.clone();
        self.submitted = plan;
        self.url = TextInputState::new(self.plan.vault_url.clone());
        drop(self.password.take());
        self.request = None;
        true
    }

    /// The host calls this only after the final view has actually detached.
    /// Preflight and closing another view must leave the shared review intact.
    pub(crate) fn closed(&mut self, scope: CloseScope) {
        if let Some(request) = self.request.take() {
            request.finish(if matches!(scope, CloseScope::View | CloseScope::Tab) {
                WorkstationSetupDecision::Cancel
            } else {
                WorkstationSetupDecision::Unavailable
            });
        }
    }

    fn field_event(&mut self, key: &str, event: TextInputEvent) {
        if key == "system-state/password" {
            self.password.input(event);
            return;
        }
        if key != "system-state/url" {
            return;
        }
        match event {
            TextInputEvent::Commit(text) => self.url.insert(&text),
            TextInputEvent::Preedit { text, selection } => self.url.set_preedit(text, selection),
            TextInputEvent::Backspace => self.url.delete_backward(),
            TextInputEvent::Delete => self.url.delete_forward(),
            TextInputEvent::Left { extend } => self.url.move_left(extend),
            TextInputEvent::Right { extend } => self.url.move_right(extend),
            TextInputEvent::WordLeft { extend } => self.url.move_word_left(extend),
            TextInputEvent::WordRight { extend } => self.url.move_word_right(extend),
            TextInputEvent::Home { extend } => self.url.move_to_start(extend),
            TextInputEvent::End { extend } => self.url.move_to_end(extend),
            TextInputEvent::KillToEnd => self.url.kill_to_end(),
            TextInputEvent::KillToStart => self.url.kill_to_start(),
            TextInputEvent::DeleteWordBackward => self.url.delete_word_backward(),
            TextInputEvent::SelectAll => self.url.select_all(),
            TextInputEvent::Undo => {
                self.url.undo();
            }
            TextInputEvent::Redo => {
                self.url.redo();
            }
            TextInputEvent::Submit | TextInputEvent::Cancel => {}
        }
    }
}

impl Drop for SystemStateApp {
    fn drop(&mut self) {
        if let Some(request) = self.request.take() {
            request.finish(WorkstationSetupDecision::Unavailable);
        }
    }
}

impl NativeAppModel for SystemStateApp {
    type ViewState = SystemStateViewState;

    fn descriptor(&self) -> AppDescriptor {
        AppDescriptor {
            kind: AppKind::SystemState,
            name: "System State",
            icon: AppIcon::Settings,
            singleton: true,
        }
    }

    fn update(
        &mut self,
        view: &mut Self::ViewState,
        event: AppEvent,
        cx: &mut UpdateCx<'_>,
    ) -> EventResult {
        let shared = match &event {
            AppEvent::FocusChanged(_) => false,
            AppEvent::ScrollLines(_) | AppEvent::ScrollAt { .. } | AppEvent::ScrollPage(_) => {
                self.clock_repaint || (self.pending() && !self.clock.interacted())
            }
            #[cfg(any(a11y_tree, test))]
            AppEvent::RevealControl(_) => {
                self.clock_repaint || (self.pending() && !self.clock.interacted())
            }
            _ => true,
        };
        let handled = match event {
            AppEvent::FocusChanged(focus) => {
                view.common.last_focus = focus;
                true
            }
            AppEvent::ScrollLines(lines) | AppEvent::ScrollAt { lines, .. } => {
                self.interact();
                view.scroll = view.scroll.saturating_add_signed(lines as isize).min(
                    (Setting::ALL.len()
                        + self.status.rows_for(Some(&self.reviewed_plan())).count())
                    .saturating_sub(1),
                );
                true
            }
            AppEvent::ScrollPage(direction) => {
                self.interact();
                view.scroll = view
                    .scroll
                    .saturating_add_signed((direction as isize) * 5)
                    .min(
                        (Setting::ALL.len()
                            + self.status.rows_for(Some(&self.reviewed_plan())).count())
                        .saturating_sub(1),
                    );
                true
            }
            #[cfg(any(a11y_tree, test))]
            AppEvent::RevealControl(key) => {
                self.interact();
                view.reveal_listed_control(&key);
                true
            }
            AppEvent::TextInput(_) | AppEvent::Action(_) if self.status.busy => true,
            AppEvent::TextInput(event) => {
                if text_input_interacts(&event) {
                    self.interact();
                }
                if event != TextInputEvent::Cancel {
                    self.field_event(
                        view.common.last_focus.as_ref().map_or("", UiKey::as_str),
                        event,
                    );
                }
                true
            }
            #[cfg(any(a11y_tree, test))]
            AppEvent::InsertText(_) if self.status.busy => true,
            #[cfg(any(a11y_tree, test))]
            AppEvent::InsertText(text) => {
                self.interact();
                self.field_event(
                    view.common.last_focus.as_ref().map_or("", UiKey::as_str),
                    TextInputEvent::Commit(text),
                );
                true
            }
            AppEvent::Action(action) => {
                self.interact();
                let key = action.id.as_str();
                if key == "system-state/apply" {
                    cx.retire_review();
                    self.apply();
                } else if key == "system-state/cancel" {
                    cx.retire_review();
                    self.cancel();
                } else if key == "system-state/check" && !self.status.busy {
                    let plan = self.reviewed_plan();
                    self.status.busy = true;
                    self.actions.push_back(SystemStateAction::Check(plan));
                } else if key == "system-state/restore" && !self.status.busy {
                    cx.retire_review();
                    self.status.busy = true;
                    self.actions.push_back(SystemStateAction::Restore);
                } else if let Some(row) = key
                    .strip_prefix("system-state/setting/")
                    .and_then(|value| value.parse::<usize>().ok())
                    .and_then(|index| Setting::ALL.get(index))
                {
                    let on = if let Some(SemanticInput::Bool(on)) = action.value {
                        on
                    } else {
                        !self.plan.selection.contains(*row)
                    };
                    self.plan.selection.set(*row, on);
                } else if matches!(key, "system-state/url" | "system-state/password") {
                    view.common.last_focus = Some(UiKey::new(key));
                    match action.value {
                        Some(SemanticInput::Text(text)) if key.ends_with("password") => {
                            self.password.anchor = 0;
                            self.password.head = self.password.count();
                            self.password.replace(SecretBytes::from(text));
                        }
                        Some(SemanticInput::Text(text)) => {
                            self.url.select_all();
                            self.url.insert(&text);
                        }
                        Some(SemanticInput::TextPosition { byte, extend })
                            if key.ends_with("password") =>
                        {
                            self.password.head = (byte / "•".len()).min(self.password.count());
                            if !extend {
                                self.password.anchor = self.password.head;
                            }
                        }
                        Some(SemanticInput::TextPosition { byte, extend }) => {
                            let anchor = if extend {
                                self.url.selection().anchor
                            } else {
                                byte
                            };
                            self.url.set_selection(anchor, byte);
                        }
                        _ => {}
                    }
                }
                true
            }
            _ => false,
        };
        if handled {
            view.common.presentation_revision = view.common.presentation_revision.wrapping_add(1);
            if shared {
                cx.repaint_instance(crate::native_app::DamageRegion::All);
            } else {
                cx.repaint(crate::native_app::DamageRegion::All);
            }
            EventResult::Handled
        } else {
            EventResult::Bubble
        }
    }

    fn view(&self, view: &Self::ViewState, cx: &ViewCx<'_>) -> UiTree {
        use crate::native_ui::{AuditedCustomNode, RAINBOW_BANNER_AUDIT};
        let width = cx.viewport.width.max(180.0);
        let compact = width < 720.0;
        let padding = if compact { 12.0 } else { 24.0 };
        let inner = (width - padding * 2.0).max(1.0);
        let reviewed_plan = self.reviewed_plan();
        let summary = self.status.summary(Some(&reviewed_plan));
        let guidance = if self.pending() && !self.clock.interacted() {
            "Click, type or scroll to stop automatic Apply."
        } else {
            "Choose your Mac settings, then Apply."
        };
        let (help, help_h) = wrapped_text(
            if summary.is_some() {
                "system-state/summary"
            } else {
                "system-state/help"
            },
            summary.as_deref().unwrap_or(guidance),
            StyleRef::Secondary,
            inner,
            summary.is_some().then_some(3),
        );
        let (vault_hint, vault_h) = wrapped_text(
            "system-state/vault-help",
            "Leave password blank for approval from another Mac.",
            StyleRef::Secondary,
            inner,
            None,
        );
        let (restore_hint, restore_h) = wrapped_text(
            "system-state/restore-help",
            "Restores saved Mac preferences. Repos, credentials and account sign-outs stay as they are.",
            StyleRef::Secondary,
            inner,
            None,
        );
        let action_h = if compact { 80.0 } else { 36.0 };
        let room = (cx.viewport.height
            - padding * 2.0
            - 48.0
            - 40.0
            - help_h
            - 68.0
            - vault_h
            - restore_h
            - action_h
            - 48.0)
            .max(34.0);
        let mut rows = Vec::new();
        for (index, setting) in Setting::ALL.iter().enumerate() {
            let key = format!("system-state/setting/{index}");
            let (label, label_h) = wrapped_text(
                format!("{key}/label"),
                setting.label(),
                StyleRef::Plain,
                (inner - 94.0).max(30.0),
                None,
            );
            let row_h = (label_h + 8.0).max(38.0);
            let toggle = UiNode::new(
                key.clone(),
                UiContent::Switch(
                    Control::new(
                        SwitchSpec {
                            label: setting.label().to_owned(),
                            description: None,
                        },
                        ActionId::new(key.clone()),
                    )
                    .value(SemanticValue::Bool(self.plan.selection.contains(*setting)))
                    .state(ControlState {
                        enabled: !self.status.busy,
                        focused: view
                            .common
                            .last_focus
                            .as_ref()
                            .is_some_and(|focus| focus.as_str() == key),
                        ..ControlState::default()
                    }),
                ),
            )
            .layout(
                Layout::default()
                    .width(Length::Fixed(82.0))
                    .height(Length::Fixed(34.0)),
            );
            rows.push((
                UiNode::new(
                    format!("{key}/row"),
                    UiContent::Group(GroupSpec::unlabeled(SemanticRole::Group)),
                )
                .layout(Layout::row().gap(12.0).height(Length::Fixed(row_h)))
                .children(vec![label.paint_only_tree(), toggle]),
                row_h,
            ));
        }
        for (index, row) in self.status.rows_for(Some(&reviewed_plan)).enumerate() {
            let (node, height) = wrapped_text(
                format!("system-state/status/{index}"),
                &row.words(),
                StyleRef::Secondary,
                inner,
                None,
            );
            rows.push((node, height));
        }
        let total = rows.len();
        let start = view.scroll.min(total.saturating_sub(1));
        let page_end = |room| {
            let mut end = start;
            let mut used = 0.0;
            for (_, height) in rows.iter().skip(start) {
                if end > start && used + height > room {
                    break;
                }
                used += height;
                end += 1;
            }
            end
        };
        let mut end = page_end(room);
        let scroll_caption = if start > 0 || end < total {
            end = page_end((room - 20.0).max(34.0));
            Some(text(
                "system-state/scroll-caption",
                format!("{}–{} of {total} · Scroll for more", start + 1, end),
                StyleRef::Secondary,
                20.0,
            ))
        } else {
            None
        };
        let mut rows: Vec<_> = rows
            .into_iter()
            .enumerate()
            .map(|(index, (node, _))| {
                if index >= start && index < end {
                    node
                } else {
                    node.offscreen()
                        .layout(Layout::column().height(Length::Fixed(0.0)))
                }
            })
            .collect();
        if let Some(caption) = scroll_caption {
            rows.push(caption);
        }
        let apply = if self.pending() && !self.clock.interacted() {
            format!("Apply ({})", self.clock.seconds())
        } else {
            "Apply".to_owned()
        };
        let mut buttons = [
            ("apply", apply),
            ("cancel", "Cancel".to_owned()),
            ("check", "Check".to_owned()),
            ("restore", "Restore preferences".to_owned()),
        ]
        .into_iter()
        .map(|(name, label)| {
            let key = format!("system-state/{name}");
            UiNode::new(
                key.clone(),
                UiContent::Button(
                    Control::new(ButtonSpec::new(label), ActionId::new(key))
                        .state(ControlState {
                            enabled: !self.status.busy,
                            ..ControlState::default()
                        })
                        .style(if name == "apply" {
                            StyleRef::Accent
                        } else {
                            StyleRef::Secondary
                        }),
                ),
            )
            .layout(
                Layout::default()
                    .width(Length::Fill)
                    .height(Length::Fixed(36.0)),
            )
        })
        .collect::<Vec<_>>();
        let actions = if compact {
            let second = buttons.split_off(2);
            UiNode::new(
                "system-state/actions",
                UiContent::Group(GroupSpec::new("System State actions")),
            )
            .layout(Layout::column().gap(8.0).height(Length::Fixed(action_h)))
            .children(vec![
                UiNode::new(
                    "system-state/actions/main",
                    UiContent::Group(GroupSpec::unlabeled(SemanticRole::Group)),
                )
                .layout(Layout::row().gap(8.0).height(Length::Fixed(36.0)))
                .children(buttons),
                UiNode::new(
                    "system-state/actions/maintenance",
                    UiContent::Group(GroupSpec::unlabeled(SemanticRole::Group)),
                )
                .layout(Layout::row().gap(8.0).height(Length::Fixed(36.0)))
                .children(second),
            ])
        } else {
            UiNode::new(
                "system-state/actions",
                UiContent::Group(GroupSpec::new("System State actions")),
            )
            .layout(Layout::row().gap(8.0).height(Length::Fixed(action_h)))
            .children(buttons)
        };
        let brand = UiNode::new(
            "system-state/brand",
            UiContent::Group(GroupSpec::new("aterm · ALab")),
        )
        .layout(Layout::row().gap(12.0).height(Length::Fixed(48.0)))
        .children(vec![
            text(
                "system-state/wordmark",
                "aterm · ALab",
                StyleRef::Accent,
                48.0,
            )
            .layout(
                Layout::default()
                    .width(Length::Fixed(132.0))
                    .height(Length::Fill),
            ),
            UiNode::new(
                "system-state/rainbow",
                UiContent::Custom(AuditedCustomNode {
                    audit_id: RAINBOW_BANNER_AUDIT,
                    role: SemanticRole::Text,
                    label: "aterm rainbow banner".to_owned(),
                    value: None,
                    action: None,
                    focusable: false,
                    focused: false,
                    focus_visible: false,
                }),
            )
            .layout(Layout::default().width(Length::Fill).height(Length::Fill)),
        ]);
        UiTree::new(
            UiNode::new(
                "system-state/app",
                UiContent::Group(GroupSpec::unlabeled(SemanticRole::Application)),
            )
            .layout(
                Layout::column()
                    .padding(Insets::all(padding))
                    .gap(6.0)
                    .clipped(),
            )
            .children(vec![
                brand,
                text("system-state/title", "System State", StyleRef::Hero, 40.0),
                help,
                UiNode::new(
                    "system-state/rows",
                    UiContent::Group(GroupSpec::new(format!(
                        "Setup checklist and progress, rows {}–{} of {total}",
                        start + 1,
                        end
                    ))),
                )
                .layout(Layout::column().height(Length::Fill).clipped())
                .children(rows),
                self.field(
                    view,
                    "system-state/url",
                    "Secrets vault URL",
                    self.url.projection(),
                    false,
                ),
                self.field(
                    view,
                    "system-state/password",
                    "Vault password (optional)",
                    self.password.projection(),
                    true,
                ),
                vault_hint,
                restore_hint,
                actions,
            ]),
        )
    }

    fn commands(&self, _view: &Self::ViewState, _out: &mut Vec<Command>) {}

    fn presentation(&self, _view: &Self::ViewState) -> AppPresentation {
        AppPresentation {
            title: "System State".to_owned(),
            icon: AppIcon::Settings,
            indicators: AppIndicators {
                dirty: self.has_unsubmitted_changes(),
                busy: self.status.busy,
                attention: self.needs_attention(),
            },
            closable: true,
            tooltip: Some("System State".to_owned()),
        }
    }

    fn prepare_close(&mut self, close: CloseRequest, _cx: &mut UpdateCx<'_>) -> CloseReadiness {
        if matches!(
            close.scope,
            CloseScope::Relaunch | CloseScope::CarriedRelaunch
        ) && self.has_unsubmitted_changes()
        {
            return CloseReadiness::Blocked {
                recovery: [
                    ("system-state/apply", "Apply"),
                    ("system-state/cancel", "Cancel"),
                ]
                .into_iter()
                .map(|(id, title)| Command {
                    id: ActionId::new(id),
                    title: title.to_owned(),
                    shortcut: None,
                    enabled: !self.status.busy,
                })
                .collect(),
            };
        }
        CloseReadiness::Ready
    }
}

impl SystemStateApp {
    fn field(
        &self,
        view: &SystemStateViewState,
        key: &str,
        label: &str,
        input: TextInputProjection,
        secret: bool,
    ) -> UiNode {
        UiNode::new(
            key.to_owned(),
            UiContent::TextField(
                Control::new(
                    TextFieldSpec {
                        label: label.to_owned(),
                        placeholder: Some(label.to_owned()),
                        secret,
                        visual_value: None,
                        input: Some(input.clone()),
                        swatch: None,
                        swatch_line: false,
                        trailing_hint: None,
                        description: None,
                        end_button: false,
                    },
                    ActionId::new(key.to_owned()),
                )
                .value(SemanticValue::Text(input.text))
                .state(ControlState {
                    enabled: !self.status.busy,
                    focused: view
                        .common
                        .last_focus
                        .as_ref()
                        .is_some_and(|focus| focus.as_str() == key),
                    ..ControlState::default()
                })
                .style(StyleRef::Secondary),
            ),
        )
        .layout(Layout::default().height(Length::Fixed(34.0)))
    }
}

fn text(
    key: impl Into<std::sync::Arc<str>>,
    words: impl Into<String>,
    style: StyleRef,
    height: f32,
) -> UiNode {
    UiNode::new(
        key,
        UiContent::Text(TextSpec {
            text: words.into(),
            role: SemanticRole::Text,
            style,
        }),
    )
    .layout(Layout::default().height(Length::Fixed(height)))
}

fn wrapped_text(
    key: impl Into<std::sync::Arc<str>>,
    words: &str,
    style: StyleRef,
    width: f32,
    max_lines: Option<usize>,
) -> (UiNode, f32) {
    let key = key.into();
    let mut lines = crate::native_ui::native_button_label_lines(words, width)
        .unwrap_or_else(|| vec![words.to_owned()]);
    if let Some(limit) = max_lines
        && lines.len() > limit
    {
        lines.truncate(limit);
        if let Some(last) = lines.last_mut() {
            while crate::native_ui::native_button_label_lines(&format!("{last}…"), width)
                .is_some_and(|lines| lines.len() > 1)
            {
                let Some((end, _)) = last.grapheme_indices().last() else {
                    break;
                };
                last.truncate(end);
            }
            last.push('…');
        }
    }
    let height = lines.len().max(1) as f32 * 20.0;
    let children = lines
        .into_iter()
        .enumerate()
        .map(|(index, line)| text(format!("{key}/{index}"), line, style, 20.0))
        .collect();
    (
        UiNode::new(
            key,
            UiContent::Group(GroupSpec::unlabeled(SemanticRole::Group)),
        )
        .layout(
            Layout::column()
                .width(Length::Fill)
                .height(Length::Fixed(height)),
        )
        .children(children),
        height,
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::native_app::{AppViewState, NativeApp, NativeRuntime, ViewId};
    use std::sync::mpsc;
    use std::time::Duration;

    fn runtime_with_app(
        app: SystemStateApp,
    ) -> (NativeRuntime, crate::native_app::AppInstanceId, ViewId) {
        let mut runtime = NativeRuntime::new();
        let instance = runtime
            .insert_instance(NativeApp::SystemState(Box::new(app)))
            .unwrap();
        let view = ViewId::from_stored(1);
        runtime
            .attach_view(view, instance, AppViewState::SystemState(Box::default()))
            .unwrap();
        (runtime, instance, view)
    }

    #[test]
    fn fresh_failures_require_attention_after_completion_and_successful_cancel_clears_them() {
        let mut app = SystemStateApp::new(Plan::default());
        let view = SystemStateViewState::default();
        for status in ["error", "failed", "unverified", "not started", "blocked"] {
            app.publish(SetupStatus {
                finished: true,
                rows: vec![SetupStatusRow {
                    name: "Check".into(),
                    status: status.into(),
                    detail: "An explicit action needs another attempt.".into(),
                }],
                ..SetupStatus::default()
            });
            assert!(app.presentation(&view).indicators.attention);
            app.cancel();
            assert!(matches!(app.take_action(), Some(SystemStateAction::Cancel)));
            // Successful cancellation supplies current rows with the previous
            // operation's failure removed; historical completion stays intact.
            app.publish(SetupStatus {
                finished: true,
                ..SetupStatus::default()
            });
            assert!(!app.presentation(&view).indicators.attention);
            assert!(app.status.summary(None).is_none());
        }
        for status in ["pending", "running", "ok", "applied", "stopped"] {
            app.publish(SetupStatus {
                rows: vec![SetupStatusRow {
                    name: "Mac preferences".into(),
                    status: status.into(),
                    detail: String::new(),
                }],
                ..SetupStatus::default()
            });
            assert!(!app.presentation(&view).indicators.attention);
            if status == "pending" {
                assert!(app.status.summary(None).is_none());
            }
        }
        app.publish(SetupStatus {
            busy: true,
            rows: vec![SetupStatusRow {
                name: "Previous result".into(),
                status: "error".into(),
                detail: String::new(),
            }],
            ..SetupStatus::default()
        });
        assert!(!app.presentation(&view).indicators.attention);
    }

    #[test]
    fn check_results_follow_the_reviewed_plan_across_linked_views() {
        use crate::native_app::{ActionInvocation, ViewMotionCx};
        use crate::native_ui::LogicalRect;
        let viewport = LogicalRect::new(0.0, 0.0, 999.0, 740.0);
        for (key, changed, restored) in [
            (
                "system-state/url",
                SemanticInput::Text("http://other.local:7484/v1/team".into()),
                SemanticInput::Text(format!("  {}  ", Plan::default().vault_url)),
            ),
            (
                "system-state/setting/5",
                SemanticInput::Bool(false),
                SemanticInput::Bool(true),
            ),
        ] {
            let mut app = SystemStateApp::new(Plan::default());
            let status = SetupStatus {
                plan: Some(Plan::default()),
                finished: true,
                check: Some(SetupCheck {
                    plan: Plan::default(),
                    rows: vec![SetupStatusRow {
                        name: "Check: Secrets vault".into(),
                        status: "blocked".into(),
                        detail: "approval code CHECK42".into(),
                    }],
                }),
                rows: vec![SetupStatusRow {
                    name: "Previous preferences".into(),
                    status: "applied".into(),
                    detail: "saved result DURABLE17".into(),
                }],
                ..SetupStatus::default()
            };
            app.publish(status.clone());
            app.password
                .input(TextInputEvent::Commit("private-draft".into()));
            let (mut runtime, instance, first) = runtime_with_app(app);
            let second = ViewId::from_stored(2);
            runtime
                .attach_view(second, instance, AppViewState::SystemState(Box::default()))
                .unwrap();
            assert!(
                runtime
                    .presentation(instance, second)
                    .unwrap()
                    .indicators
                    .attention
            );
            runtime
                .dispatch(
                    instance,
                    first,
                    AppEvent::Action(ActionInvocation {
                        id: ActionId::new(key),
                        value: Some(changed),
                    }),
                )
                .unwrap();
            // Ordinary polls must not reinstall observations of a different
            // reviewed URL or checklist in either shared view.
            runtime.publish_system_state(status.clone());
            for view in [first, second] {
                assert!(
                    !runtime
                        .presentation(instance, view)
                        .unwrap()
                        .indicators
                        .attention
                );
                let compiled = runtime
                    .render(
                        instance,
                        view,
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
                    .unwrap()
                    .compile(viewport)
                    .unwrap();
                assert!(
                    compiled
                        .semantics
                        .iter()
                        .all(|node| !node.label.contains("CHECK42"))
                );
                assert!(
                    compiled
                        .semantics
                        .iter()
                        .any(|node| node.label.contains("DURABLE17"))
                );
            }
            let app = runtime.system_state_mut().unwrap();
            assert_eq!(app.password.projection().text, "•".repeat(13));
            assert!(app.has_unsubmitted_changes());
            // A different current worker failure remains actionable even while
            // an old read-only observation is irrelevant to the edited plan.
            let mut failed = status.clone();
            failed.rows.push(SetupStatusRow {
                name: "Restore preferences".into(),
                status: "failed".into(),
                detail: "recovery result ERROR19".into(),
            });
            runtime.publish_system_state(failed);
            assert!(
                runtime
                    .presentation(instance, second)
                    .unwrap()
                    .indicators
                    .attention
            );
            let app = runtime.system_state_mut().unwrap();
            let summary = app.status.summary(Some(&app.reviewed_plan())).unwrap();
            assert!(summary.contains("ERROR19"));
            assert!(!summary.contains("CHECK42"));
            runtime
                .dispatch(
                    instance,
                    second,
                    AppEvent::Action(ActionInvocation {
                        id: ActionId::new(key),
                        value: Some(restored),
                    }),
                )
                .unwrap();
            runtime.publish_system_state(status);
            assert!(
                runtime
                    .presentation(instance, first)
                    .unwrap()
                    .indicators
                    .attention
            );
        }
    }

    #[test]
    fn status_results_show_check_drift_over_historical_success() {
        let mut app = SystemStateApp::new(Plan::default());
        let result = SetupStatusRow {
            name: "Check: current setting".into(),
            status: "drift".into(),
            detail: "observed value differs: DIFF42".into(),
        };
        app.publish(SetupStatus {
            finished: true,
            rows: vec![
                SetupStatusRow {
                    name: "Previous preferences".into(),
                    status: "applied".into(),
                    detail: String::new(),
                },
                result.clone(),
            ],
            ..SetupStatus::default()
        });
        assert!(
            app.presentation(&SystemStateViewState::default())
                .indicators
                .attention
        );
        let summary = app.status.summary(None).unwrap();
        assert!(summary.contains(&result.name));
        assert!(summary.contains(&result.status));
        assert!(summary.contains(&result.detail));

        let mut interrupted = app.status.clone();
        interrupted.finished = false;
        interrupted.rows.insert(
            0,
            SetupStatusRow {
                name: "Previous attempt".into(),
                status: "pending".into(),
                detail: "INTERRUPTED17: an older attempt did not finish".into(),
            },
        );
        assert!(
            !SetupStatus {
                rows: vec![interrupted.rows[0].clone()],
                ..SetupStatus::default()
            }
            .needs_attention(None)
        );
        app.publish(interrupted);
        assert!(app.status.summary(None).unwrap().contains(&result.detail));
        assert!(
            app.presentation(&SystemStateViewState::default())
                .indicators
                .attention
        );

        let approval = SetupStatusRow {
            name: "Secrets vault".into(),
            status: "blocked".into(),
            detail: "approve code ABC123".into(),
        };
        app.status.rows.insert(0, approval.clone());
        assert!(app.status.summary(None).unwrap().contains(&approval.detail));
        app.status.busy = true;
        let active = SetupStatusRow {
            name: "Current pass".into(),
            status: "running".into(),
            detail: "MEASURE17".into(),
        };
        app.status.rows.push(active.clone());
        assert!(
            !app.presentation(&SystemStateViewState::default())
                .indicators
                .attention
        );
        assert!(app.status.summary(None).unwrap().contains(&active.detail));
    }

    #[test]
    fn status_results_show_successful_restore_over_the_durable_stop() {
        let mut app = SystemStateApp::new(Plan::default());
        let result = SetupStatusRow {
            name: "Original preference".into(),
            status: "restored".into(),
            detail: "saved value loaded: RESTORE42".into(),
        };
        app.publish(SetupStatus {
            finished: true,
            rows: vec![
                SetupStatusRow {
                    name: "Selected Mac settings".into(),
                    status: "stopped".into(),
                    detail: "restoration requested".into(),
                },
                result.clone(),
            ],
            ..SetupStatus::default()
        });
        assert!(
            !app.presentation(&SystemStateViewState::default())
                .indicators
                .attention
        );
        let summary = app.status.summary(None).unwrap();
        assert!(summary.contains(&result.name));
        assert!(summary.contains(&result.status));
        assert!(summary.contains(&result.detail));
    }

    #[test]
    fn result_and_approval_guidance_stays_visible_above_an_unscrolled_checklist() {
        use crate::native_app::ViewMotionCx;
        use crate::native_ui::LogicalRect;
        let viewport = LogicalRect::new(0.0, 0.0, 999.0, 740.0);
        for (row, complete, busy) in [
            (
                SetupStatusRow {
                    name: "Check: current setting".into(),
                    status: "drift".into(),
                    detail: "DRIFT42: observed value differs".into(),
                },
                true,
                false,
            ),
            (
                SetupStatusRow {
                    name: "Restore preferences".into(),
                    status: "not started".into(),
                    detail: "Try again.".into(),
                },
                true,
                false,
            ),
            (
                SetupStatusRow {
                    name: "Secrets vault".into(),
                    status: "blocked".into(),
                    detail: "Ask an administrator on another Mac to approve code ABC123, then leave aterm open while the subscriber synchronizes this machine's development credentials.".into(),
                },
                true,
                false,
            ),
            (
                SetupStatusRow {
                    name: "Check".into(),
                    status: "error".into(),
                    detail: format!("ERR42 {}", "extended diagnostic ".repeat(200)),
                },
                false,
                false,
            ),
            (
                SetupStatusRow {
                    name: "Cancel".into(),
                    status: "failed".into(),
                    detail: "ERR57: the cancellation record could not be saved; retaining this choice for retry.".into(),
                },
                true,
                true,
            ),
        ] {
            let mut app = SystemStateApp::new(Plan::default());
            let mut rows = vec![
                SetupStatusRow {
                    name: "Previous preferences".into(),
                    status: "applied".into(),
                    detail: String::new(),
                },
                row.clone(),
            ];
            if row.status == "blocked" {
                // The real engine reports vault approval before the dependent
                // repository stage; its generic blocker must not hide the code.
                rows.push(SetupStatusRow {
                    name: "GitHub repositories".into(),
                    status: "blocked".into(),
                    detail: "waiting for the selected vault to synchronize credentials".into(),
                });
            }
            rows.push(SetupStatusRow {
                name: "Another observation".into(),
                status: if busy { "running" } else { "ok" }.into(),
                detail: String::new(),
            });
            app.publish(SetupStatus {
                busy,
                rows,
                ..SetupStatus::default()
            });
            let view = SystemStateViewState::default();
            if busy {
                assert!(!app.presentation(&view).indicators.attention);
            }
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
            let summary = compiled
                .semantic(&UiKey::new("system-state/summary"))
                .unwrap();
            assert!(!summary.offscreen);
            assert!(summary.rect.bottom() <= viewport.bottom());
            if row.status == "blocked" {
                assert!(summary.rect.height > 20.0, "long approval guidance wraps");
            }
            let words = compiled
                .semantics
                .iter()
                .filter(|node| node.key.as_str().starts_with("system-state/summary/"))
                .map(|node| node.label.as_str())
                .collect::<Vec<_>>()
                .join(" ");
            assert!(words.contains(&row.name));
            assert!(words.contains(&row.status));
            if complete {
                assert!(words.contains(&row.detail));
            } else {
                assert!(words.contains("ERR42"));
                assert!(words.ends_with('…'));
                assert!(summary.rect.height <= 60.0);
            }
            assert!(
                compiled
                    .semantic(&UiKey::new("system-state/status/1"))
                    .unwrap()
                    .offscreen,
                "the list result remains below the default checklist page"
            );
            for key in [
                "system-state/setting/0",
                "system-state/url",
                "system-state/password",
                "system-state/apply",
                "system-state/cancel",
            ] {
                let node = compiled.semantic(&UiKey::new(key)).unwrap();
                assert!(!node.offscreen);
                assert!(node.rect.y >= 0.0 && node.rect.bottom() <= viewport.bottom());
            }
            assert_eq!(view.scroll, 0);
        }
    }

    #[test]
    fn reviewed_offer_survives_update_preflight_until_apply_or_cancel() {
        for apply in [true, false] {
            let (send, receive) = mpsc::channel();
            let request = Request::new(Plan::default(), send);
            let mut app = SystemStateApp::new(Plan::default());
            app.offer(request.clone());
            app.interact();
            app.password
                .input(TextInputEvent::Commit("private-draft".to_owned()));
            app.url.select_all();
            app.url.insert("http://draft.local:7484/v1/team");
            let (mut runtime, instance, view) = runtime_with_app(app);
            for scope in [CloseScope::Relaunch, CloseScope::CarriedRelaunch] {
                assert!(matches!(
                    runtime
                        .prepare_close(instance, view, CloseRequest { scope })
                        .unwrap()
                        .0,
                    CloseReadiness::Blocked { .. }
                ));
            }
            assert!(request.pending());
            assert!(matches!(receive.try_recv(), Err(mpsc::TryRecvError::Empty)));
            let Some(app) = runtime.system_state_mut() else {
                panic!("System State must remain registered");
            };
            assert_eq!(app.password.projection().text, "•".repeat(13));
            assert!(
                app.presentation(&SystemStateViewState::default())
                    .indicators
                    .dirty
            );
            if apply {
                app.apply();
                assert!(app.has_unsubmitted_changes());
                let Some(SystemStateAction::Apply(answer)) = app.take_action() else {
                    panic!("reviewed Apply is queued");
                };
                let WorkstationSetupDecision::Begin(plan) = &answer.decision else {
                    panic!("Apply carries its reviewed plan");
                };
                let plan = plan.clone();
                assert!(request.finish_answer(answer));
                app.apply_accepted(plan);
            } else {
                app.cancel();
            }
            assert!(!app.has_unsubmitted_changes());
            assert_eq!(
                runtime
                    .prepare_close(
                        instance,
                        view,
                        CloseRequest {
                            scope: CloseScope::Relaunch
                        }
                    )
                    .unwrap()
                    .0,
                CloseReadiness::Ready
            );
        }
    }

    #[test]
    fn readonly_tab_allows_update_but_changed_manual_plan_does_not() {
        let (mut runtime, instance, view) = runtime_with_app(SystemStateApp::new(Plan::default()));
        assert_eq!(
            runtime
                .prepare_close(
                    instance,
                    view,
                    CloseRequest {
                        scope: CloseScope::Relaunch
                    }
                )
                .unwrap()
                .0,
            CloseReadiness::Ready
        );
        let Some(app) = runtime.system_state_mut() else {
            panic!("System State must remain registered");
        };
        app.interact();
        assert!(!app.has_unsubmitted_changes());
        app.plan.selection.set(Setting::Dock, false);
        assert!(matches!(
            runtime
                .prepare_close(
                    instance,
                    view,
                    CloseRequest {
                        scope: CloseScope::CarriedRelaunch
                    }
                )
                .unwrap()
                .0,
            CloseReadiness::Blocked { .. }
        ));
    }

    #[test]
    fn reviewing_an_offer_without_changing_values_still_protects_it_from_relaunch() {
        let (send, _receive) = mpsc::channel();
        let mut app = SystemStateApp::new(Plan::default());
        app.offer(Request::new(Plan::default(), send));
        app.interact();
        let (mut runtime, instance, view) = runtime_with_app(app);
        assert!(matches!(
            runtime
                .prepare_close(
                    instance,
                    view,
                    CloseRequest {
                        scope: CloseScope::Relaunch
                    }
                )
                .unwrap()
                .0,
            CloseReadiness::Blocked { .. }
        ));
    }

    #[test]
    fn delayed_automatic_offer_cannot_replace_a_manual_draft() {
        let mut app = SystemStateApp::new(Plan::default());
        app.url.select_all();
        app.url.insert("http://draft.local:7484/v1/team");
        app.password
            .input(TextInputEvent::Commit("private-draft".to_owned()));
        let (send, receive) = mpsc::channel();
        app.offer(Request::new(Plan::default(), send));
        assert_eq!(
            receive.try_recv().unwrap().decision,
            WorkstationSetupDecision::Unavailable
        );
        assert_eq!(app.url.value(), "http://draft.local:7484/v1/team");
        assert_eq!(app.password.projection().text, "•".repeat(13));
        assert!(!app.pending());
    }

    #[test]
    fn busy_setup_ignores_escape_and_accessibility_text_edits() {
        let (send, _receive) = mpsc::channel();
        let mut app = SystemStateApp::new(Plan::default());
        app.offer(Request::new(Plan::default(), send));
        app.status.busy = true;
        let (mut runtime, instance, view) = runtime_with_app(app);
        for event in [
            AppEvent::FocusChanged(Some(UiKey::new("system-state/url"))),
            AppEvent::TextInput(TextInputEvent::Cancel),
            AppEvent::InsertText("unwanted-edit".to_owned()),
        ] {
            runtime.dispatch(instance, view, event).unwrap();
        }
        let Some(NativeApp::SystemState(app)) = runtime.app(instance) else {
            panic!("System State must remain registered");
        };
        assert_eq!(app.url.value(), Plan::default().vault_url);
        assert!(app.actions.is_empty());
        assert!(app.pending());
    }

    #[test]
    fn close_preflight_retains_the_offer_and_only_disposal_resolves_it() {
        for (scope, expected) in [
            (CloseScope::View, WorkstationSetupDecision::Cancel),
            (CloseScope::Tab, WorkstationSetupDecision::Cancel),
            (CloseScope::Window, WorkstationSetupDecision::Unavailable),
            (CloseScope::AppQuit, WorkstationSetupDecision::Unavailable),
        ] {
            let (send, receive) = mpsc::channel();
            let mut app = SystemStateApp::new(Plan::default());
            let request = Request::new(Plan::default(), send);
            app.offer(request.clone());
            app.password
                .input(TextInputEvent::Commit("inert-password".into()));
            let (mut runtime, instance, view) = runtime_with_app(app);
            assert_eq!(
                runtime
                    .prepare_close(instance, view, CloseRequest { scope })
                    .unwrap()
                    .0,
                CloseReadiness::Ready
            );
            assert!(request.pending());
            assert!(matches!(receive.try_recv(), Err(mpsc::TryRecvError::Empty)));
            let Some(NativeApp::SystemState(app)) = runtime.app(instance) else {
                panic!("shared form exists")
            };
            assert_eq!(app.password.projection().text, "•".repeat(14));
            runtime.finish_system_state_close(instance, scope);
            runtime.remove_view(view);
            runtime.remove_instance(instance);
            assert_eq!(receive.try_recv().unwrap().decision, expected);
        }
    }

    #[test]
    fn virtualized_settings_can_be_revealed_without_exposing_secret_fields() {
        let mut view = SystemStateViewState::default();
        assert!(view.reveal_listed_control(&UiKey::new("system-state/setting/11")));
        assert_eq!(view.scroll, 11);
        assert!(!view.reveal_listed_control(&UiKey::new("system-state/password")));
        assert_eq!(view.scroll, 11);
        assert!(!view.reveal_listed_control(&UiKey::new("system-state/setting/12")));
        assert_eq!(view.scroll, 11);
    }

    #[test]
    fn edited_offer_waits_past_thirty_seconds_and_apply_returns_url_selection_and_one_password() {
        let (send, receive) = mpsc::channel();
        let request = Request::new(Plan::default(), send);
        let mut app = SystemStateApp::new(Plan::default());
        app.offer(request.clone());
        let start = Instant::now();
        app.tick(start, true);
        app.interact();
        app.plan.selection.set(Setting::Dock, false);
        app.url.select_all();
        app.url.insert("http://vault.local:7484/v1/team");
        app.password
            .input(TextInputEvent::Commit("test-only-password".to_owned()));
        app.tick(start + Duration::from_secs(90), true);
        assert!(app.take_action().is_none());
        app.apply();
        let Some(SystemStateAction::Apply(answer)) = app.take_action() else {
            panic!("Apply must produce one action");
        };
        let WorkstationSetupDecision::Begin(plan) = &answer.decision else {
            panic!("Apply must carry the selected plan");
        };
        assert!(!plan.selection.contains(Setting::Dock));
        assert_eq!(plan.vault_url, "http://vault.local:7484/v1/team");
        assert_eq!(
            answer.password.as_ref().unwrap().as_slice(),
            b"test-only-password"
        );
        assert_eq!(app.password.projection().text, "•".repeat(18));
        assert!(app.pending());
        assert!(matches!(receive.try_recv(), Err(mpsc::TryRecvError::Empty)));
        let plan = plan.clone();
        assert!(request.finish_answer(answer));
        app.apply_accepted(plan);
        assert!(app.password.take().is_none());
        assert!(!app.pending());
        assert!(matches!(
            receive
                .recv_timeout(Duration::from_secs(60))
                .unwrap()
                .decision,
            WorkstationSetupDecision::Begin(_)
        ));
    }

    /// An Apply says whether the person typed the vault. A field left as System State
    /// showed it (here from a published status) asks for the vault setup shows when the
    /// Apply is admitted; only an edited one is a vault the person chose. Negative control:
    /// a flag that compares nothing reads every Apply as typed.
    #[test]
    fn an_apply_says_whether_the_vault_was_typed() {
        let mut app = SystemStateApp::new(Plan::default());
        app.publish(SetupStatus {
            plan: Some(Plan {
                vault_url: "http://shown.local:7484/v1/team".into(),
                ..Plan::default()
            }),
            ..SetupStatus::default()
        });
        let typed = |app: &mut SystemStateApp| {
            app.apply();
            let Some(SystemStateAction::Apply(answer)) = app.take_action() else {
                panic!("Apply must produce one action");
            };
            app.status.busy = false;
            answer.vault_typed
        };
        assert!(!typed(&mut app));
        app.url.select_all();
        app.url.insert("http://typed.local:7484/v1/team");
        assert!(typed(&mut app));
    }

    #[test]
    fn initial_offer_requires_thirty_visible_seconds_and_does_not_resolve_its_own_request() {
        let (send, receive) = mpsc::channel();
        let request = Request::new(Plan::default(), send);
        let mut app = SystemStateApp::new(Plan::default());
        app.offer(request.clone());
        let start = Instant::now();
        app.tick(start, false);
        app.tick(start + Duration::from_secs(100), true);
        app.tick(start + Duration::from_secs(129), true);
        assert!(app.take_action().is_none());
        app.tick(start + Duration::from_secs(130), true);
        assert!(matches!(
            app.take_action(),
            Some(SystemStateAction::Apply(_))
        ));
        assert!(matches!(receive.try_recv(), Err(mpsc::TryRecvError::Empty)));
    }

    #[test]
    fn password_edit_incremental_clusters_keep_a_valid_caret_and_delete_whole() {
        for (initial, insertion, middle) in [
            ("e", "\u{301}", false),
            ("👩👩", "\u{200d}", true),
            ("🇺", "🇸", false),
        ] {
            let mut password = Password::default();
            password.input(TextInputEvent::Commit(initial.into()));
            if middle {
                password.input(TextInputEvent::Left { extend: false });
            }
            password.input(TextInputEvent::Commit(insertion.into()));
            let projection = password.projection();
            assert_eq!(projection.text, "•");
            assert_eq!(projection.selection, TextSelection::caret("•".len()));
            password.input(TextInputEvent::Backspace);
            assert!(password.projection().text.is_empty());
            assert!(password.take().is_none());
        }
    }

    #[test]
    fn password_edit_arrows_collapse_either_selection_before_typing() {
        for (right, reverse) in [(false, false), (true, false), (false, true), (true, true)] {
            let mut password = Password::default();
            password.input(TextInputEvent::Commit("aβz".into()));
            if reverse {
                password.input(TextInputEvent::Home { extend: true });
            } else {
                password.input(TextInputEvent::SelectAll);
            }
            password.input(if right {
                TextInputEvent::Right { extend: false }
            } else {
                TextInputEvent::Left { extend: false }
            });
            assert_eq!(
                password.projection().selection,
                TextSelection::caret(if right { "•••".len() } else { 0 })
            );
            password.input(TextInputEvent::Commit("x".into()));
            assert_eq!(
                password.take().unwrap().as_str(),
                Some(if right { "aβzx" } else { "xaβz" })
            );
        }
    }

    #[test]
    fn password_projection_never_contains_the_secret_and_grapheme_edits_work() {
        let mut password = Password::default();
        password.input(TextInputEvent::Commit("e\u{301}👩‍💻z".to_owned()));
        assert_eq!(password.projection().text, "•••");
        password.input(TextInputEvent::Left { extend: false });
        password.input(TextInputEvent::Backspace);
        assert_eq!(password.take().unwrap().as_str(), Some("e\u{301}z"));
        assert!(password.projection().text.is_empty());
    }

    #[test]
    fn progress_snapshots_cannot_overwrite_interacted_drafts() {
        let mut app = SystemStateApp::new(Plan::default());
        let mut saved = Plan::default();
        saved.selection.set(Setting::Messages, false);
        saved.vault_url = "http://saved.local:7484/v1/team".to_owned();
        app.publish(SetupStatus {
            plan: Some(saved.clone()),
            ..SetupStatus::default()
        });
        assert_eq!(app.plan, saved);
        app.interact();
        app.url.select_all();
        app.url.insert("http://draft.local:7484/v1/team");
        app.publish(SetupStatus {
            plan: Some(Plan::default()),
            ..SetupStatus::default()
        });
        assert_eq!(app.url.value(), "http://draft.local:7484/v1/team");
    }

    #[test]
    fn submitting_without_focused_apply_only_stops_the_timer() {
        let mut runtime = NativeRuntime::new();
        let instance = runtime
            .insert_instance(NativeApp::SystemState(Box::new(SystemStateApp::new(
                Plan::default(),
            ))))
            .unwrap();
        let view = crate::native_app::ViewId::from_stored(1);
        runtime
            .attach_view(view, instance, AppViewState::SystemState(Box::default()))
            .unwrap();
        runtime
            .dispatch(instance, view, AppEvent::TextInput(TextInputEvent::Submit))
            .unwrap();
        let Some(NativeApp::SystemState(app)) = runtime.app(instance) else {
            panic!("System State must remain registered");
        };
        assert!(app.actions.is_empty());
        assert!(app.clock.interacted());
    }
}
