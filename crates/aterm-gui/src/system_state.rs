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
    /// Setup report for vault sync ([`SECRETS_VAULT_ROW`], [`SECRETS_SUBSCRIBER_ROW`]).
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
pub(crate) const fn text_input_interacts(event: &TextInputEvent) -> bool {
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

/// Password text never enters a UI projection, undo history, restore data or a diagnostic String.
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
    /// The vault lines' press came before setup's first read.
    vault_sync_on_read: bool,
    anim_ticks: u64,
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
            vault_sync_on_read: false,
            anim_ticks: 0,
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
        self.vault_sync_on_read = false;
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
        if self.vault_sync_on_read && self.status.plan.is_some() {
            self.vault_sync_on_read = false;
            changed |= self.preset_vault_sync();
        }
        changed
    }

    pub(crate) fn take_action(&mut self) -> Option<SystemStateAction> {
        self.actions.pop_front()
    }

    /// Final-view disposal transfers explicit cancellation to the host.
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
        if self.status.busy {
            self.anim_ticks = self.anim_ticks.wrapping_add(1);
        }
        std::mem::take(&mut self.clock_repaint)
            || self.status.busy
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

    /// No status has reached this review yet: a tab opened just now.
    pub(crate) fn unread(&self) -> bool {
        self.status == SetupStatus::default()
    }

    /// The vault lines' remedy.
    pub(crate) fn preset_vault_sync(&mut self) -> bool {
        if self.status.busy || self.request.is_some() || self.has_unsubmitted_changes() {
            return false;
        }
        if self.status.plan.is_none() {
            self.vault_sync_on_read = true;
            return false;
        }
        let before = self.plan.selection;
        self.plan.selection.set(Setting::Secrets, true);
        for setting in Setting::ALL.into_iter().filter(|s| s.is_workstation()) {
            self.plan.selection.set(setting, false);
        }
        self.plan.selection != before
    }

    /// The review's rows are the vault sync's.
    pub(crate) fn shows_vault_sync(&self) -> bool {
        let on = |setting| self.plan.selection.contains(setting);
        on(Setting::Secrets)
            && !Setting::ALL
                .into_iter()
                .any(|s| s.is_workstation() && on(s))
    }

    fn apply(&mut self) {
        if self.status.busy {
            return;
        }
        self.interact();
        let plan = self.reviewed_plan();
        let vault_typed = plan.vault_url != self.submitted.vault_url.trim();
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
        let width = cx.viewport.width;
        let compact = width < 720.0;
        let padding = 12.0;
        let inner = (width - padding * 2.0).max(1.0);
        let reviewed_plan = self.reviewed_plan();
        let summary = self.status.summary(Some(&reviewed_plan));
        let vault_words = "Leave the password blank to join an open vault at once, or to wait for approval from another Mac.";
        let restore_words = "Restores saved Mac preferences. Repos, credentials and account sign-outs stay as they are.";
        let (vault_hint, vault_h) = wrapped_text(
            "system-state/vault-help",
            vault_words,
            StyleRef::Secondary,
            inner,
            None,
        );
        let (restore_hint, restore_h) = wrapped_text(
            "system-state/restore-help",
            restore_words,
            StyleRef::Secondary,
            inner,
            None,
        );
        let switch_w = crate::native_ui::switch_least_width().max(82.0);
        let line = line_pitch();
        let title_lines = wrap_lines("System State", inner, None);
        let title_h = if title_lines.len() > 1 {
            title_lines.len() as f32 * line
        } else {
            40.0
        };
        let wordmark_w = crate::native_ui::native_button_label_width("aterm · ALab").ceil();
        let brand_fits = inner - 12.0 - 48.0 >= wordmark_w;
        let results: Vec<String> = self
            .status
            .rows_for(Some(&reviewed_plan))
            .map(|row| row.words())
            .collect();
        let total = Setting::ALL.len() + results.len();
        let start = view.scroll.min(total.saturating_sub(1));
        let help_words = "Choose your Mac settings, then Apply.";
        let face_warnings = [
            "Click, type or scroll to stop automatic Apply.",
            "Click or scroll to stop automatic Apply.",
            "Click to stop automatic Apply.",
            "Click to stop.",
        ];
        let countdown_warnings = [
            "30 s to Apply. Click, type or scroll to stop.",
            "30 s to Apply. Click or scroll to stop.",
            "30 s to Apply. Click to stop.",
            "30 s. Click to stop.",
        ];
        let lines_of = |words: &str| wrap_lines(words, inner, None).len();
        // Every fit holds the shortest warning whole, whatever the state, so fields stay still.
        let least_help = lines_of(countdown_warnings[countdown_warnings.len() - 1]);
        let most_help = [help_words, face_warnings[0], countdown_warnings[0]]
            .map(lines_of)
            .into_iter()
            .fold(3, usize::max);
        struct Fit {
            shown: usize,
            title: bool,
            help_lines: usize,
            two_rows: bool,
        }
        let fits = [
            (3, true, most_help, true),
            (2, true, most_help, true),
            (1, true, most_help, true),
            (0, true, most_help, false),
            (0, false, most_help, false),
            (0, false, 3, false),
            (0, false, 2, false),
            (0, false, 1, false),
        ]
        .map(|(shown, title, help_lines, two_rows)| Fit {
            shown: if brand_fits { shown } else { shown.min(2) },
            title,
            help_lines: help_lines.max(least_help),
            two_rows,
        });
        let room = |fit: &Fit, rows: usize| {
            let optional = [vault_h + 6.0, restore_h + 6.0, 48.0 + 6.0];
            cx.viewport.height
                - padding * 2.0
                - optional[..fit.shown].iter().sum::<f32>()
                - if fit.title { title_h + 6.0 } else { 0.0 }
                - (fit.help_lines as f32 * line + 6.0)
                - 6.0
                - 2.0 * (34.0 + 6.0)
                - (rows as f32 * 44.0 - 8.0)
        };
        let label_fits = |label: &str, share: f32| {
            crate::native_ui::native_button_label_width(label)
                <= crate::native_ui::text_button_label_room(&ButtonSpec::new(label), share)
                    + crate::native_ui::TEXT_FIT_SLACK
        };
        let counting = "Apply (30)";
        let one_row_share = (inner - 3.0 * 8.0) / 4.0;
        let grid_share = (inner - 8.0) / 2.0;
        let shorts_fit = |share: f32| {
            ["Apply", "Cancel", "Check", "Restore"]
                .iter()
                .all(|label| label_fits(label, share))
        };
        let short_row_fits = shorts_fit(one_row_share);
        let grid_fits = shorts_fit(grid_share);
        let rows_at = |fit: &Fit| {
            if !compact || fit.shown < 2 && short_row_fits {
                1
            } else if grid_fits {
                2
            } else {
                4
            }
        };
        // Where a column of whole buttons leaves no checklist row, the grid's cut faces do.
        let mut candidates: Vec<(&Fit, usize)> =
            fits.iter().map(|fit| (fit, rows_at(fit))).collect();
        if let Some(&(last, 4)) = candidates.last() {
            candidates.push((last, 2));
        }
        let row_h = |switch_h: f32, lines: usize| (lines as f32 * line + switch_h).max(38.0);
        struct Arrangement {
            under: bool,
            label_w: f32,
            switch_h: f32,
            names: [Vec<String>; Setting::ALL.len()],
            fit: usize,
        }
        let beside_w = inner - switch_w - 12.0;
        let arrange = |under: bool| {
            let label_w = if under { inner } else { beside_w };
            let switch_h = if under { 6.0 + 34.0 + 4.0 } else { 8.0 };
            let names = Setting::ALL.map(|setting| wrap_lines(setting.label(), label_w, None));
            let heights = names.each_ref().map(|name| row_h(switch_h, name.len()));
            let two_rows = heights
                .windows(2)
                .map(|pair| pair[0] + pair[1])
                .fold(0.0, f32::max)
                + line;
            let fit = candidates
                .iter()
                .position(|(fit, rows)| {
                    room(fit, *rows)
                        >= if fit.two_rows {
                            two_rows
                        } else {
                            row_h(switch_h, 1) + line
                        }
                })
                .unwrap_or(candidates.len() - 1);
            Arrangement {
                under,
                label_w,
                switch_h,
                names,
                fit,
            }
        };
        let beside_holds_words = Setting::ALL.iter().all(|setting| {
            aterm_grapheme::pieces(setting.label(), aterm_grapheme::BreakKind::is_word_boundary)
                .iter()
                .all(|piece| line_fits(piece.text, beside_w))
        });
        let under = arrange(true);
        let beside = beside_holds_words.then(|| arrange(false));
        let (fit, button_rows) = candidates[beside
            .as_ref()
            .map_or(under.fit, |beside| beside.fit.min(under.fit))];
        let shown = fit.shown;
        let share = match button_rows {
            1 => one_row_share,
            2 => grid_share,
            _ => inner,
        };
        let pending = self.pending() && !self.clock.interacted();
        let on_face = label_fits(counting, share)
            && room(fit, button_rows) + (button_rows - 1) as f32 * 44.0 >= 0.0;
        let warnings = if on_face {
            face_warnings
        } else {
            countdown_warnings
        };
        let compose = |warning: Option<&str>| match (warning, &summary) {
            (Some(warning), Some(summary)) => format!("{warning} {summary}"),
            (Some(warning), None) => warning.to_owned(),
            (None, Some(summary)) => summary.clone(),
            (None, None) => help_words.to_owned(),
        };
        // A result follows the warning only where the warning keeps its words beside it.
        let mut guidance = if pending {
            warnings
                .iter()
                .find_map(|warning| {
                    let lines = wrapped_lines(&compose(Some(warning)), inner, Some(fit.help_lines));
                    if lines.join(" ").starts_with(warning.trim_end_matches('.')) {
                        Some(lines)
                    } else {
                        let lines = wrap_lines(warning, inner, None);
                        (lines.len() <= fit.help_lines).then_some(lines)
                    }
                })
                .unwrap_or_else(|| {
                    wrapped_lines(warnings[warnings.len() - 1], inner, Some(fit.help_lines))
                })
        } else {
            wrapped_lines(&compose(None), inner, Some(fit.help_lines))
        };
        if pending
            && !on_face
            && let Some(first) = guidance.first_mut()
            && let Some(rest) = first.strip_prefix("30")
        {
            *first = format!("{}{rest}", self.clock.seconds());
        }
        let (mut help, mut help_h) = text_lines(
            if summary.is_some() {
                "system-state/summary"
            } else {
                "system-state/help"
            },
            guidance,
            StyleRef::Secondary,
        );
        // On a page too short for its parts the guidance keeps the lines its fit holds, so the
        // fields below it stay still from state to state.
        if room(fit, button_rows) < 0.0 {
            help_h = fit.help_lines as f32 * line;
            help = help.layout(
                Layout::column()
                    .width(Length::Fill)
                    .height(Length::Fixed(help_h)),
            );
        }
        let room = (room(fit, button_rows) + fit.help_lines as f32 * line - help_h).max(34.0);
        let rows_room = (room - line).max(34.0);
        let lines_in = |height: f32| ((height / line).floor() as usize).max(1);
        // The switch stands beside a name only where its column holds every word, and the page
        // then shows as many rows from its top.
        let shows = |arrangement: &Arrangement| {
            let cap = lines_in(rows_room - arrangement.switch_h);
            let fill = |room: f32| {
                let mut used = 0.0;
                arrangement
                    .names
                    .iter()
                    .enumerate()
                    .take_while(|(index, name)| {
                        used += row_h(arrangement.switch_h, name.len().min(cap));
                        *index == 0 || used <= room
                    })
                    .count()
            };
            if total == Setting::ALL.len() && fill(room) == total {
                total
            } else {
                fill(room - line)
            }
        };
        let Arrangement {
            under: switch_under,
            label_w,
            switch_h,
            names,
            ..
        } = match beside {
            Some(beside)
                if beside.fit < under.fit
                    || beside.fit == under.fit && shows(&beside) >= shows(&under) =>
            {
                beside
            }
            _ => under,
        };
        let row_h = |lines: usize| row_h(switch_h, lines);
        let mut rows = Vec::new();
        for (index, (setting, name)) in Setting::ALL.iter().zip(names).enumerate() {
            let key = format!("system-state/setting/{index}");
            let whole = name.len() <= lines_in(rows_room - switch_h);
            let name = fit_lines(name, label_w, lines_in(rows_room - switch_h));
            let row_h = row_h(name.len());
            let (_, current_state, desired_state) =
                self.setting_station_state(*setting, &reviewed_plan);
            let state_summary = format!("Current: {current_state} → Desired: {desired_state}");
            let separator = crate::native_ui::FACT_SEPARATOR;
            let (label, _) = text_lines(
                format!("{key}/label"),
                if whole {
                    with_suffix(
                        name,
                        &[
                            format!("{separator}{state_summary}"),
                            format!("{separator}Current: {current_state}"),
                        ],
                        label_w,
                    )
                } else {
                    name
                },
                StyleRef::Plain,
            );
            let toggle = UiNode::new(
                key.clone(),
                UiContent::Switch(
                    Control::new(
                        SwitchSpec {
                            label: setting.label().to_owned(),
                            description: Some(state_summary),
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
                    .width(Length::Fixed(switch_w))
                    .height(Length::Fixed(34.0)),
            );
            rows.push((
                UiNode::new(
                    format!("{key}/row"),
                    UiContent::Group(GroupSpec::unlabeled(SemanticRole::Group)),
                )
                .layout(
                    if switch_under {
                        Layout::column().gap(6.0)
                    } else {
                        Layout::row().gap(12.0)
                    }
                    .height(Length::Fixed(row_h)),
                )
                .children(vec![label.paint_only_tree(), toggle]),
                row_h,
            ));
        }
        // A row past the page keeps its words unwrapped; its endless height ends the page.
        let last_shown = start + lines_in(room);
        for (index, words) in results.iter().enumerate() {
            let key = format!("system-state/status/{index}");
            rows.push(
                if (start..=last_shown).contains(&(Setting::ALL.len() + index)) {
                    wrapped_text(
                        key,
                        words,
                        StyleRef::Secondary,
                        inner,
                        Some(lines_in(rows_room)),
                    )
                } else {
                    (
                        text_lines(key, vec![words.clone()], StyleRef::Secondary).0,
                        f32::INFINITY,
                    )
                },
            );
        }
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
        let scroll_caption = if (start > 0 || end < total) && rows[start].1 + line <= room {
            end = page_end((room - line).max(34.0));
            let range = format!("{}–{end}", start + 1);
            let captions = [
                format!("{range} of {total} · Scroll for more"),
                format!("{range} of {total}"),
                format!("{range}/{total}"),
                format!("{end}/{total}"),
            ];
            let caption = captions
                .iter()
                .find(|caption| line_fits(caption, inner))
                .unwrap_or(&captions[captions.len() - 1]);
            Some(text(
                "system-state/scroll-caption",
                caption.clone(),
                StyleRef::Secondary,
                line,
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
        let apply = if pending {
            format!("Apply ({})", self.clock.seconds())
        } else {
            "Apply".to_owned()
        };
        let mut buttons = [
            ("apply", apply, counting, "Apply"),
            ("cancel", "Cancel".to_owned(), "Cancel", "Cancel"),
            ("check", "Check".to_owned(), "Check", "Check"),
            (
                "restore",
                "Restore preferences".to_owned(),
                "Restore preferences",
                "Restore",
            ),
        ]
        .into_iter()
        .map(|(name, label, widest, short)| {
            let key = format!("system-state/{name}");
            let mut spec = ButtonSpec::new(label);
            let short_face = if name == "apply" {
                !on_face
            } else {
                !label_fits(widest, share)
            };
            if short != spec.label && short_face {
                spec = spec.visual_label(short);
            }
            if name == "restore" {
                spec = spec.description(restore_words);
            }
            UiNode::new(
                key.clone(),
                UiContent::Button(
                    Control::new(spec, ActionId::new(key))
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
        let actions = if button_rows > 1 {
            let second = buttons.split_off(2);
            let half = if button_rows == 2 {
                Layout::row().gap(8.0).height(Length::Fixed(36.0))
            } else {
                Layout::column().gap(8.0).height(Length::Fixed(80.0))
            };
            UiNode::new(
                "system-state/actions",
                UiContent::Group(GroupSpec::new("System State actions")),
            )
            .layout(
                Layout::column()
                    .gap(8.0)
                    .height(Length::Fixed(button_rows as f32 * 44.0 - 8.0)),
            )
            .children(vec![
                UiNode::new(
                    "system-state/actions/main",
                    UiContent::Group(GroupSpec::unlabeled(SemanticRole::Group)),
                )
                .layout(half)
                .children(buttons),
                UiNode::new(
                    "system-state/actions/maintenance",
                    UiContent::Group(GroupSpec::unlabeled(SemanticRole::Group)),
                )
                .layout(half)
                .children(second),
            ])
        } else {
            UiNode::new(
                "system-state/actions",
                UiContent::Group(GroupSpec::new("System State actions")),
            )
            .layout(Layout::row().gap(8.0).height(Length::Fixed(36.0)))
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
                    .width(Length::Fixed(
                        wordmark_w.max(132.0).min(inner - 12.0 - 48.0),
                    ))
                    .height(Length::Fill),
            ),
            UiNode::new(
                "system-state/rainbow",
                UiContent::Custom(AuditedCustomNode {
                    audit_id: RAINBOW_BANNER_AUDIT,
                    role: SemanticRole::Text,
                    label: "aterm rainbow banner".to_owned(),
                    value: Some(self.village_banner_spec(&reviewed_plan, cx.animation_phase_ms)),
                    action: None,
                    focusable: false,
                    focused: false,
                    focus_visible: false,
                }),
            )
            .layout(Layout::default().width(Length::Fill).height(Length::Fill)),
        ]);
        let mut children = Vec::new();
        if shown >= 3 {
            children.push(brand);
        }
        if fit.title {
            children.push(if title_lines.len() > 1 {
                let (lines, height) =
                    text_lines("system-state/title/lines", title_lines, StyleRef::Hero);
                UiNode::new(
                    "system-state/title",
                    UiContent::Group(GroupSpec {
                        label: Some("System State".to_owned()),
                        role: SemanticRole::Text,
                        style: StyleRef::Plain,
                    }),
                )
                .layout(
                    Layout::column()
                        .width(Length::Fill)
                        .height(Length::Fixed(height)),
                )
                .children(vec![lines.paint_only_tree()])
            } else {
                text("system-state/title", "System State", StyleRef::Hero, 40.0)
            });
        }
        children.push(help);
        children.extend([
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
                None,
            ),
            self.field(
                view,
                "system-state/password",
                "Vault password (optional)",
                self.password.projection(),
                true,
                Some(vault_words),
            ),
        ]);
        if shown >= 1 {
            children.push(vault_hint);
        }
        if shown >= 2 {
            children.push(restore_hint);
        }
        children.push(actions);
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
            .children(children),
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
    fn setting_station_state(
        &self,
        setting: Setting,
        plan: &Plan,
    ) -> (char, &'static str, &'static str) {
        let selected = plan.selection.contains(setting);
        let desired = if selected {
            match setting {
                Setting::CapsLock => "Control",
                Setting::NaturalScrolling => "off",
                Setting::TrackpadScrolling => "3-axis + momentum",
                Setting::Desktop => "icons shown",
                Setting::Widgets => "hidden",
                Setting::Dock => "minimal Dock",
                Setting::Messages => "signed out",
                Setting::FaceTime => "signed out",
                Setting::Spotlight => "minimal + .noindex",
                Setting::Permissions => "all grants ready",
                Setting::Secrets => "vault synced",
                Setting::Repositories => "repos cloned",
            }
        } else {
            "unchanged"
        };
        let matches_setting = |name: &str| -> bool {
            match setting {
                Setting::CapsLock => name.contains("Caps Lock"),
                Setting::NaturalScrolling => {
                    name.contains("natural scrolling") || name.contains("Natural scrolling")
                }
                Setting::TrackpadScrolling => {
                    name.contains("trackpad") || name.contains("Trackpad")
                }
                Setting::Desktop => {
                    (name.contains("desktop")
                        || name.contains("Desktop")
                        || name.contains("wallpaper"))
                        && !name.contains("widget")
                        && !name.contains("Widget")
                }
                Setting::Widgets => name.contains("widget") || name.contains("Widget"),
                Setting::Dock => name.contains("Dock"),
                Setting::Messages => name.contains("Messages"),
                Setting::FaceTime => name.contains("FaceTime"),
                Setting::Spotlight => {
                    name.contains("Spotlight")
                        || name.contains("rustup")
                        || name.contains("rust-docs")
                        || name.contains(".noindex")
                }
                Setting::Permissions => {
                    name.contains("Permissions")
                        || name.contains("Command Line Tools")
                        || name.contains("Full Disk Access")
                        || name.contains("Debugger")
                        || name.contains("Developer Tools")
                        || name.contains("Accessibility")
                }
                Setting::Secrets => name.contains("Secrets") || name.contains("vault"),
                Setting::Repositories => name.contains("GitHub") || name.contains("repositories"),
            }
        };
        let mut saw_ok = false;
        let mut saw_drift = false;
        let mut saw_running = false;
        let mut saw_error = false;
        for row in self.status.rows_for(Some(plan)) {
            if !matches_setting(&row.name) {
                continue;
            }
            match row.status.as_str() {
                "error" | "failed" | "blocked" | "not started" => saw_error = true,
                "running" => saw_running = true,
                "drift" | "unverified" | "pending" => saw_drift = true,
                "ok" | "applied" => saw_ok = true,
                _ => {}
            }
        }
        if saw_error {
            ('E', "needs action", desired)
        } else if saw_running {
            ('W', "assembling…", desired)
        } else if saw_drift {
            ('D', "drift", desired)
        } else if self.status.busy && selected && !saw_ok {
            ('W', "assembling…", desired)
        } else if saw_ok || (self.status.finished && selected) {
            ('G', "verified", desired)
        } else if !selected {
            ('_', "skipped", desired)
        } else {
            ('D', "pending", desired)
        }
    }

    fn village_banner_spec(&self, plan: &Plan, phase_ms: u64) -> String {
        let mut states = String::with_capacity(Setting::ALL.len());
        let mut done = 0usize;
        let mut total = 0usize;
        for setting in Setting::ALL {
            let (code, _, _) = self.setting_station_state(setting, plan);
            states.push(code);
            if plan.selection.contains(setting) {
                total += 1;
                if code == 'G' {
                    done += 1;
                }
            }
        }
        let phase = phase_ms.wrapping_add(self.anim_ticks.wrapping_mul(120));
        let busy = u8::from(self.status.busy);
        format!("phase={phase};busy={busy};done={done};total={total};states={states}")
    }

    fn field(
        &self,
        view: &SystemStateViewState,
        key: &str,
        label: &str,
        input: TextInputProjection,
        secret: bool,
        hint: Option<&str>,
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
                        description: hint.map(str::to_owned),
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

fn line_pitch() -> f32 {
    let (px, _) = crate::native_ui::text_paint_metrics(SemanticRole::Text, StyleRef::Plain);
    (px * 1.25).ceil().max(20.0)
}

fn line_fits(line: &str, width: f32) -> bool {
    use crate::native_ui::MAX_VISUAL_GRAPHEMES;
    (line.len() <= MAX_VISUAL_GRAPHEMES
        || line.grapheme_indices().nth(MAX_VISUAL_GRAPHEMES).is_none())
        && crate::native_ui::native_button_label_width(line)
            <= width + crate::native_ui::TEXT_FIT_SLACK
}

fn most_fitting(count: usize, fits: impl Fn(usize) -> bool) -> usize {
    let (mut fitting, mut over) = (0, count + 1);
    while over - fitting > 1 {
        let mid = fitting + (over - fitting) / 2;
        if fits(mid) {
            fitting = mid;
        } else {
            over = mid;
        }
    }
    fitting
}

fn grapheme_ends(text: &str) -> Vec<usize> {
    text.grapheme_indices()
        .map(|(at, _)| at)
        .skip(1)
        .chain([text.len()])
        .collect()
}

fn wrap_lines(words: &str, width: f32, most: Option<usize>) -> Vec<String> {
    use crate::native_ui::MAX_VISUAL_GRAPHEMES;
    let most = most.unwrap_or(usize::MAX);
    let pieces = aterm_grapheme::pieces(words, aterm_grapheme::BreakKind::is_word_boundary);
    let mut lines: Vec<String> = Vec::new();
    let mut line = String::new();
    let mut at = 0;
    while at < pieces.len() && lines.len() <= most {
        let window = &pieces[at..pieces.len().min(at + MAX_VISUAL_GRAPHEMES)];
        let joined = |count: usize| {
            let mut joined = line.clone();
            for piece in &window[..count] {
                if piece.spaced && !joined.is_empty() {
                    joined.push(' ');
                }
                joined.push_str(piece.text);
            }
            joined
        };
        let fitting = most_fitting(window.len(), |count| line_fits(&joined(count), width));
        if fitting > 0 {
            line = joined(fitting);
            at += fitting;
            if at < pieces.len() {
                lines.push(std::mem::take(&mut line));
            }
        } else if !line.is_empty() {
            lines.push(std::mem::take(&mut line));
        } else {
            let text = window[0].text;
            let ends = grapheme_ends(text);
            let (mut from, mut taken) = (0, 0);
            loop {
                let next = &ends[taken..ends.len().min(taken + MAX_VISUAL_GRAPHEMES)];
                let count = most_fitting(next.len(), |count| {
                    line_fits(&text[from..next[count - 1]], width)
                })
                .max(1);
                let to = next[count - 1];
                taken += count;
                if taken == ends.len() {
                    line = text[from..to].to_owned();
                    break;
                }
                lines.push(text[from..to].to_owned());
                from = to;
                if lines.len() > most {
                    break;
                }
            }
            at += 1;
        }
    }
    if lines.len() <= most && (!line.is_empty() || lines.is_empty()) {
        lines.push(line);
    }
    lines
}

fn elided(line: &str, width: f32) -> String {
    let stop = |c: char| {
        matches!(
            c,
            '.' | ',' | ';' | ':' | '·' | '—' | '–' | '→' | '…' | '。' | '、' | '，' | '；' | '：'
        )
    };
    let ends = grapheme_ends(line);
    let kept = most_fitting(ends.len(), |count| {
        line_fits(&format!("{}…", &line[..ends[count - 1]]), width)
    });
    let cut = kept.checked_sub(1).map_or(0, |at| ends[at]);
    let pieces = aterm_grapheme::pieces(line, aterm_grapheme::BreakKind::is_word_boundary);
    let hangul = |c: char| matches!(c, '\u{1100}'..='\u{11FF}' | '\u{3130}'..='\u{318F}' | '\u{AC00}'..='\u{D7AF}');
    // A piece boundary ends a word unless it falls between two Hangul syllables of one word.
    let boundary = |at: usize| {
        pieces
            .iter()
            .any(|piece| piece.at == at || piece.at + piece.text.len() == at)
            && !line[..at]
                .chars()
                .next_back()
                .zip(line[at..].chars().next())
                .is_some_and(|(before, after)| hangul(before) && hangul(after))
    };
    // A cut ends a word where the stops and closing marks after it lead to a space, the end or
    // a word boundary, so "26.1" and "can’t" are not cut short.
    let word_end = |at: usize| {
        let rest = line[at..].trim_start_matches(|c: char| {
            stop(c)
                || matches!(
                    c,
                    ')' | ']' | '}' | '’' | '”' | '"' | '!' | '?' | '」' | '』' | '）'
                )
        });
        rest.is_empty()
            || rest.starts_with(char::is_whitespace)
            || boundary(at)
            || boundary(line.len() - rest.len())
    };
    let stem = if word_end(cut) {
        &line[..cut]
    } else {
        pieces
            .iter()
            .map(|piece| piece.at + piece.text.len())
            .take_while(|&end| end < cut)
            .filter(|&end| word_end(end))
            .last()
            .map_or(&line[..cut], |end| &line[..end])
    };
    let trimmed = stem.trim_end_matches(|c: char| c.is_whitespace() || stop(c));
    format!(
        "{}…",
        if trimmed.is_empty() {
            stem.trim_end()
        } else {
            trimmed
        }
    )
}

fn fit_lines(mut lines: Vec<String>, width: f32, limit: usize) -> Vec<String> {
    if lines.len() > limit {
        lines.truncate(limit.max(1));
        if let Some(last) = lines.last_mut() {
            *last = elided(last, width);
        }
    }
    lines
}

fn with_suffix(mut lines: Vec<String>, suffixes: &[String], width: f32) -> Vec<String> {
    if let Some(last) = lines.last_mut()
        && let Some(suffix) = suffixes
            .iter()
            .find(|suffix| line_fits(&format!("{last}{suffix}"), width))
    {
        last.push_str(suffix);
    }
    lines
}

fn wrapped_lines(words: &str, width: f32, max_lines: Option<usize>) -> Vec<String> {
    let lines = wrap_lines(words, width, max_lines);
    match max_lines {
        Some(limit) => fit_lines(lines, width, limit),
        None => lines,
    }
}

fn wrapped_text(
    key: impl Into<std::sync::Arc<str>>,
    words: &str,
    style: StyleRef,
    width: f32,
    max_lines: Option<usize>,
) -> (UiNode, f32) {
    text_lines(key, wrapped_lines(words, width, max_lines), style)
}

fn text_lines(
    key: impl Into<std::sync::Arc<str>>,
    lines: Vec<String>,
    style: StyleRef,
) -> (UiNode, f32) {
    let key = key.into();
    let pitch = line_pitch();
    let height = lines.len().max(1) as f32 * pitch;
    let children = lines
        .into_iter()
        .enumerate()
        .map(|(index, line)| text(format!("{key}/{index}"), line, style, pitch))
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
            for viewport in [
                LogicalRect::new(0.0, 0.0, 999.0, 740.0),
                LogicalRect::new(0.0, 0.0, 624.0, 334.0),
                LogicalRect::new(0.0, 0.0, 999.0, 334.0),
            ] {
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
    fn the_vault_sync_preset_moves_only_its_rows_and_never_over_an_edit_an_offer_or_a_run() {
        let mut read = Plan::default();
        read.selection.set(Setting::Secrets, false);
        read.selection.set(Setting::Repositories, false);
        read.vault_url = "https://vault.example/v1/team".into();
        let fresh = || {
            let mut app = SystemStateApp::new(Plan::default());
            app.publish(SetupStatus {
                plan: Some(read.clone()),
                ..SetupStatus::default()
            });
            app
        };

        let read_now = |busy| SetupStatus {
            plan: Some(read.clone()),
            busy,
            ..SetupStatus::default()
        };
        let mut unread = SystemStateApp::new(Plan::default());
        assert!(!unread.preset_vault_sync(), "no plan read yet");
        assert_eq!(unread.reviewed_plan(), Plan::default());
        assert!(unread.publish(read_now(false)));
        assert!(unread.shows_vault_sync(), "the first read takes the preset");
        assert_eq!(unread.reviewed_plan().vault_url, read.vault_url);
        let mut held = SystemStateApp::new(Plan::default());
        assert!(!held.preset_vault_sync());
        held.publish(read_now(true));
        held.publish(read_now(false));
        held.publish(read_now(false));
        assert_eq!(
            held.reviewed_plan(),
            read,
            "a read a run holds ends the wait"
        );
        let (send, _receive) = mpsc::channel();
        let mut left = SystemStateApp::new(Plan::default());
        assert!(!left.preset_vault_sync());
        let request = Request::new(read.clone(), send);
        assert!(left.offer(request.clone()));
        request.finish(WorkstationSetupDecision::Cancel);
        left.tick(Instant::now(), false);
        left.publish(read_now(false));
        assert_eq!(
            left.reviewed_plan(),
            read,
            "an offer in between ends the wait"
        );

        let mut app = fresh();
        assert!(!app.shows_vault_sync());
        assert!(app.preset_vault_sync());
        assert!(app.shows_vault_sync());
        for setting in Setting::ALL {
            let on = !setting.is_workstation() && setting != Setting::Repositories;
            assert_eq!(app.plan.selection.contains(setting), on, "{setting:?}");
        }
        assert_eq!(app.reviewed_plan().vault_url, read.vault_url);
        assert!(app.actions.is_empty() && !app.status.busy, "never an Apply");

        type Edit = fn(&mut SystemStateApp);
        let edits: [(&str, Edit); 3] = [
            ("row", |app| {
                app.plan.selection.set(Setting::Spotlight, false)
            }),
            ("vault", |app| {
                app.url.select_all();
                app.url.insert("http://draft.local/v1");
            }),
            ("password", |app| {
                app.password
                    .input(TextInputEvent::Commit("inert".to_owned()));
            }),
        ];
        for (edit, apply) in edits {
            let mut app = fresh();
            apply(&mut app);
            let edited = app.reviewed_plan();
            assert!(!app.preset_vault_sync(), "{edit}");
            assert_eq!(app.reviewed_plan(), edited, "{edit}");
        }

        let mut busy = fresh();
        busy.publish(SetupStatus {
            plan: Some(read.clone()),
            busy: true,
            ..SetupStatus::default()
        });
        assert!(!busy.preset_vault_sync());
        assert_eq!(busy.reviewed_plan(), read);

        let (send, _receive) = mpsc::channel();
        let mut offered = SystemStateApp::new(Plan::default());
        assert!(offered.offer(Request::new(read.clone(), send)));
        offered.publish(SetupStatus {
            plan: Some(read.clone()),
            ..SetupStatus::default()
        });
        assert!(!offered.preset_vault_sync());
        assert_eq!(offered.reviewed_plan(), read);
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
    fn a_short_page_keeps_the_checklist_apply_and_the_vault_fields_in_view() {
        short_pages(1.0);
    }

    #[test]
    fn a_short_page_keeps_them_in_view_at_a_large_text_size() {
        use crate::native_appearance::{AppearancePreferences, install_preferences};
        crate::tray_raster::prepare_ui_fonts_for_direct_view_test();
        for scale in [1.5, 2.0] {
            let _ = install_preferences(AppearancePreferences {
                text_scale: scale,
                ..AppearancePreferences::default()
            });
            short_pages(scale);
        }
        let _ = install_preferences(AppearancePreferences::default());
    }

    fn short_pages(scale: f32) {
        use crate::native_ui::LogicalRect;
        let compile = compile_view;
        let rect = |compiled: &crate::native_ui::CompiledUi, key: &str| {
            compiled
                .semantic(&UiKey::new(key))
                .filter(|node| !node.offscreen && node.rect.height > 0.0)
                .map(|node| node.rect)
        };
        let plain = SystemStateApp::new(Plan::default());
        let (send, _receive) = mpsc::channel();
        let mut offered = SystemStateApp::new(Plan::default());
        offered.offer(Request::new(Plan::default(), send.clone()));
        let mut interacted = SystemStateApp::new(Plan::default());
        interacted.offer(Request::new(Plan::default(), send));
        interacted.interact();
        let mut switched = Plan::default();
        for setting in [Setting::Widgets, Setting::Dock, Setting::Spotlight] {
            switched.selection.set(setting, false);
        }
        let switched = SystemStateApp::new(switched);
        let mut checking = SystemStateApp::new(Plan::default());
        checking.publish(SetupStatus {
            busy: true,
            rows: vec![SetupStatusRow {
                name: "Dock".into(),
                status: "running".into(),
                detail: String::new(),
            }],
            ..SetupStatus::default()
        });
        let mut reported = SystemStateApp::new(Plan::default());
        reported.publish(SetupStatus {
            busy: false,
            rows: vec![
                SetupStatusRow {
                    name: "Previous preferences".into(),
                    status: "applied".into(),
                    detail: String::new(),
                },
                SetupStatusRow {
                    name: "Check".into(),
                    status: "error".into(),
                    detail: format!("ERR42 {}", "extended diagnostic ".repeat(200)),
                },
            ],
            ..SetupStatus::default()
        });
        let (send_reported, _receive_reported) = mpsc::channel();
        let mut offered_reported = SystemStateApp::new(Plan::default());
        offered_reported.offer(Request::new(Plan::default(), send_reported));
        offered_reported.publish(SetupStatus {
            busy: false,
            rows: reported.status.rows.clone(),
            ..SetupStatus::default()
        });
        let optional = [
            "system-state/brand",
            "system-state/title",
            "system-state/vault-help",
            "system-state/restore-help",
            "system-state/actions/maintenance",
        ];
        let buttons = [
            "system-state/apply",
            "system-state/cancel",
            "system-state/check",
            "system-state/restore",
        ];
        for width in [
            150.0, 182.0, 196.0, 240.0, 300.0, 360.0, 371.0, 480.0, 624.0, 999.0, 1400.0,
        ] {
            for height in (166..=742).step_by(24).chain([172, 216]) {
                let page = LogicalRect::new(0.0, 0.0, width, height as f32);
                let mut first = None;
                for (app, state) in [
                    (&plain, "plain"),
                    (&offered, "offered"),
                    (&interacted, "interacted"),
                    (&switched, "switched"),
                    (&checking, "checking"),
                    (&reported, "reported"),
                    (&offered_reported, "offered beside a result"),
                ] {
                    let total = Setting::ALL.len()
                        + app.status.rows_for(Some(&app.reviewed_plan())).count();
                    for scroll in 0..total {
                        let compiled = compile(app, scroll, page);
                        let at = format!(
                            "a {state} {width}×{height} page scrolled to {scroll} at {scale}×"
                        );
                        let button_ys: Option<Vec<f32>> = buttons
                            .iter()
                            .map(|key| compiled.semantic(&UiKey::new(*key)).map(|node| node.rect.y))
                            .collect();
                        let button_rows = button_ys.map_or(4, |mut ys| {
                            ys.dedup();
                            ys.len()
                        });
                        let line = line_pitch();
                        let least = wrap_lines("30 s. Click to stop.", width - 24.0, None).len();
                        let fixed = |rows: usize| {
                            24.0 + least as f32 * line + 12.0 + 80.0 + rows as f32 * 44.0 - 8.0
                        };
                        let under = scroll >= Setting::ALL.len()
                            || compiled
                                .semantic(&UiKey::new(format!("system-state/setting/{scroll}")))
                                .is_some_and(|node| node.rect.x < 13.0);
                        let one_row = (line + if under { 44.0 } else { 8.0 }).max(38.0) + line;
                        let height_f = height as f32;
                        assert!(
                            whole(&compiled, page, "system-state/help/0")
                                || whole(&compiled, page, "system-state/summary/0"),
                            "the guidance in {at}"
                        );
                        if height_f >= fixed(button_rows) {
                            for key in ["system-state/url", "system-state/password"] {
                                assert!(whole(&compiled, page, key), "{key} in {at}");
                            }
                            for key in buttons {
                                assert!(
                                    compiled.paint.iter().any(|node| node.key.as_str() == key
                                        && !node.clipped()
                                        && node.rect.bottom() <= page.bottom()),
                                    "{key} in {at}"
                                );
                            }
                        }
                        if height_f >= fixed(4) + one_row {
                            for key in buttons {
                                assert!(whole(&compiled, page, key), "{key} in {at}");
                            }
                        }
                        if let Some(apply) = compiled.semantic(&UiKey::new("system-state/apply")) {
                            assert_eq!(
                                apply.label.contains('('),
                                state.starts_with("offered"),
                                "Apply's countdown in its name in {at}"
                            );
                        }
                        if height_f >= fixed(button_rows.min(2)) + one_row {
                            let key = if scroll < Setting::ALL.len() {
                                format!("system-state/setting/{scroll}")
                            } else {
                                format!("system-state/status/{}", scroll - Setting::ALL.len())
                            };
                            assert!(whole(&compiled, page, &key), "{key} in {at}");
                            if scroll > 0 {
                                assert!(
                                    whole(&compiled, page, "system-state/scroll-caption"),
                                    "the scroll caption in {at}"
                                );
                            }
                        }
                        if scale == 1.0
                            && width >= 624.0
                            && height >= 334
                            && scroll + 1 < Setting::ALL.len()
                            && !state.contains("report")
                            && !state.contains("result")
                        {
                            let key = format!("system-state/setting/{}", scroll + 1);
                            assert!(whole(&compiled, page, &key), "{key} in {at}");
                        }
                        if scale == 1.0 && width >= 480.0 && height >= 334 {
                            assert!(
                                whole(&compiled, page, "system-state/title"),
                                "title in {at}"
                            );
                        }
                        let frame: (Vec<&str>, Vec<_>) = (
                            optional
                                .into_iter()
                                .filter(|key| compiled.semantic(&UiKey::new(*key)).is_some())
                                .collect(),
                            ["system-state/url", "system-state/password"]
                                .into_iter()
                                .chain(buttons)
                                .map(|key| {
                                    compiled.semantic(&UiKey::new(key)).map(|node| node.rect)
                                })
                                .collect(),
                        );
                        match &first {
                            None => first = Some(frame),
                            Some(before) => assert_eq!(&frame, before, "what {at} shows"),
                        }
                    }
                }
            }
            let short = LogicalRect::new(0.0, 0.0, width, 334.0);
            if scale == 1.0 && width >= 300.0 {
                assert!(
                    whole(
                        &compile(&plain, 0, short),
                        short,
                        "system-state/scroll-caption"
                    ),
                    "the scroll caption in a {width}×334 page"
                );
            }
            if scale == 1.0 && width >= 624.0 {
                assert!(rect(&compile(&reported, 0, short), "system-state/summary/2").is_some());
                assert!(rect(&compile(&plain, 0, short), "system-state/help/1").is_none());
                let tall = LogicalRect::new(0.0, 0.0, width, 740.0);
                for app in [&plain, &reported] {
                    let compiled = compile(app, 0, tall);
                    for key in [
                        "system-state/brand",
                        "system-state/vault-help",
                        "system-state/restore-help",
                    ] {
                        assert!(whole(&compiled, tall, key), "{key} in a {width}×740 page");
                    }
                }
            }
        }
    }

    fn compile_view(
        app: &SystemStateApp,
        scroll: usize,
        viewport: crate::native_ui::LogicalRect,
    ) -> crate::native_ui::CompiledUi {
        let view = SystemStateViewState {
            scroll,
            ..SystemStateViewState::default()
        };
        app.view(
            &view,
            &ViewCx {
                viewport,
                animation_phase_ms: 0,
                motion: crate::native_app::ViewMotionCx::default(),
                terminal_font_px: 13.0,
                terminal_scale: 1.0,
                semantic_font: None,
                document: None,
            },
        )
        .compile(viewport)
        .unwrap()
    }

    fn whole(
        compiled: &crate::native_ui::CompiledUi,
        page: crate::native_ui::LogicalRect,
        key: &str,
    ) -> bool {
        let part = format!("{key}/");
        let mut nodes = compiled
            .paint
            .iter()
            .filter(|node| node.key.as_str() == key || node.key.as_str().starts_with(&part))
            .peekable();
        nodes.peek().is_some()
            && nodes.all(|node| {
                !node.clipped()
                    && node.rect.y >= page.y
                    && node.rect.bottom() <= page.bottom()
                    && compiled.text_fit_audit_line(&node.key).is_none_or(|audit| {
                        !(audit.contains("kind=text ") || audit.contains("kind=button "))
                            || !audit.contains("overflow=true")
                    })
            })
    }

    fn painted(audit: &str) -> &str {
        let (_, rest) = audit.split_once("painted=\"").unwrap();
        rest.split('"').next().unwrap()
    }

    fn reported_app() -> SystemStateApp {
        let mut reported = SystemStateApp::new(Plan::default());
        reported.publish(SetupStatus {
            busy: false,
            rows: vec![
                SetupStatusRow {
                    name: "Previous preferences".into(),
                    status: "applied".into(),
                    detail: String::new(),
                },
                SetupStatusRow {
                    name: "Check".into(),
                    status: "error".into(),
                    detail: format!("ERR42 {}", "extended diagnostic ".repeat(20)),
                },
            ],
            ..SetupStatus::default()
        });
        reported
    }

    /// The optional parts a page scrolled to its top shows (whole button faces counted as one),
    /// its guidance's lines and how many checklist rows it shows.
    fn page_shows(
        compiled: &crate::native_ui::CompiledUi,
        page: crate::native_ui::LogicalRect,
    ) -> (Vec<&'static str>, usize, usize) {
        let shown = |key: &str| compiled.semantic(&UiKey::new(key)).is_some();
        let mut parts: Vec<&'static str> = [
            "system-state/brand",
            "system-state/title",
            "system-state/vault-help",
            "system-state/restore-help",
        ]
        .into_iter()
        .filter(|key| shown(key))
        .collect();
        if ["apply", "cancel", "check", "restore"]
            .iter()
            .all(|key| whole(compiled, page, &format!("system-state/{key}")))
        {
            parts.push("faces");
        }
        let lines = (0..)
            .take_while(|line| shown(&format!("system-state/summary/{line}")))
            .count();
        let rows = compiled
            .semantic(&UiKey::new("system-state/rows"))
            .map_or(0, |node| {
                let (_, range) = node.label.rsplit_once("rows 1–").unwrap();
                range.split(' ').next().unwrap().parse::<usize>().unwrap()
            });
        (parts, lines, rows)
    }

    #[test]
    fn a_taller_page_never_shows_less() {
        taller_pages(&[
            196.0, 300.0, 360.0, 480.0, 624.0, 720.0, 740.0, 830.0, 999.0, 1400.0,
        ]);
    }

    #[test]
    fn a_taller_page_never_shows_less_at_any_text_size() {
        use crate::native_appearance::{AppearancePreferences, install_preferences};
        crate::tray_raster::prepare_ui_fonts_for_direct_view_test();
        for scale in [1.0, 1.5, 2.0] {
            let _ = install_preferences(AppearancePreferences {
                text_scale: scale,
                ..AppearancePreferences::default()
            });
            taller_pages(&[
                150.0, 182.0, 240.0, 300.0, 360.0, 480.0, 624.0, 720.0, 999.0,
            ]);
        }
        let _ = install_preferences(AppearancePreferences::default());
    }

    fn taller_pages(widths: &[f32]) {
        let (send, _receive) = mpsc::channel();
        let mut offered = SystemStateApp::new(Plan::default());
        offered.offer(Request::new(Plan::default(), send));
        let plain = SystemStateApp::new(Plan::default());
        let reported = reported_app();
        for (&width, app) in widths
            .iter()
            .flat_map(|width| [(width, &reported), (width, &plain), (width, &offered)])
        {
            let page = crate::native_ui::LogicalRect::new(0.0, 0.0, width, 1100.0);
            let (_, result, _) = page_shows(&compile_view(app, 0, page), page);
            let mut before: Option<(Vec<&str>, usize, usize)> = None;
            for height in (166..=1100).step_by(2) {
                let page = crate::native_ui::LogicalRect::new(0.0, 0.0, width, height as f32);
                let (parts, lines, rows) = page_shows(&compile_view(app, 0, page), page);
                if parts.contains(&"system-state/title") {
                    assert_eq!(
                        lines, result,
                        "the result under the title at {width}×{height}"
                    );
                }
                if let Some((parts_before, lines_before, rows_before)) = &before {
                    for part in parts_before {
                        assert!(parts.contains(part), "{part} at {width}×{height}");
                    }
                    assert!(lines >= *lines_before, "the result at {width}×{height}");
                    if parts == *parts_before && lines == *lines_before {
                        assert!(rows >= *rows_before, "the checklist at {width}×{height}");
                    }
                }
                before = Some((parts, lines, rows));
            }
        }
    }

    #[test]
    fn a_wider_page_never_shows_less() {
        use crate::native_appearance::{AppearancePreferences, install_preferences};
        crate::tray_raster::prepare_ui_fonts_for_direct_view_test();
        let plain = SystemStateApp::new(Plan::default());
        let reported = reported_app();
        for scale in [1.0, 1.5, 2.0] {
            let _ = install_preferences(AppearancePreferences {
                text_scale: scale,
                ..AppearancePreferences::default()
            });
            for height in [166, 216, 274, 334, 430, 600, 800, 1000] {
                for app in [&plain, &reported] {
                    let mut before: Option<(Vec<&str>, usize)> = None;
                    for width in (128..=720).step_by(2) {
                        let page = crate::native_ui::LogicalRect::new(
                            0.0,
                            0.0,
                            width as f32,
                            height as f32,
                        );
                        let (parts, _, rows) = page_shows(&compile_view(app, 0, page), page);
                        let at = format!("{width}×{height} and {scale}×");
                        if let Some((parts_before, rows_before)) = &before {
                            for part in parts_before {
                                assert!(parts.contains(part), "{part} at {at}");
                            }
                            if parts == *parts_before {
                                assert!(rows >= *rows_before, "the checklist at {at}");
                            }
                        }
                        before = Some((parts, rows));
                    }
                }
            }
        }
        let _ = install_preferences(AppearancePreferences::default());
    }

    #[test]
    fn an_offer_always_shows_its_countdown_and_how_to_stop() {
        use crate::native_appearance::{AppearancePreferences, install_preferences};
        crate::tray_raster::prepare_ui_fonts_for_direct_view_test();
        let (send, _receive) = mpsc::channel();
        let start = Instant::now();
        let offers = |result: bool| {
            [30, 10, 9, 1].map(|seconds| {
                let mut app = SystemStateApp::new(Plan::default());
                app.offer(Request::new(Plan::default(), send.clone()));
                if result {
                    app.publish(SetupStatus {
                        busy: false,
                        rows: vec![SetupStatusRow {
                            name: "Setup state".into(),
                            status: "pending".into(),
                            detail: "Not running; Apply to retry.".into(),
                        }],
                        ..SetupStatus::default()
                    });
                }
                app.tick(start, true);
                app.tick(start + Duration::from_secs(30 - seconds), true);
                assert!(app.pending() && !app.clock.interacted());
                assert_eq!(app.clock.seconds(), seconds);
                (seconds, app)
            })
        };
        let lanes: [(f32, Vec<u32>); 3] = [
            (
                1.0,
                (128..=720).step_by(4).chain([760, 1000, 1400]).collect(),
            ),
            (1.5, (128..=720).step_by(12).chain([1000]).collect()),
            (
                2.0,
                (128..=480).step_by(4).chain([720, 1000, 1400]).collect(),
            ),
        ];
        for (scale, widths) in lanes {
            let _ = install_preferences(AppearancePreferences {
                text_scale: scale,
                ..AppearancePreferences::default()
            });
            for result in [false, true] {
                let guidance = if result {
                    "system-state/summary"
                } else {
                    "system-state/help"
                };
                let apps = offers(result);
                for &width in &widths {
                    for height in [120, 166, 172, 200, 216, 240, 274, 334, 500] {
                        let page = crate::native_ui::LogicalRect::new(
                            0.0,
                            0.0,
                            width as f32,
                            height as f32,
                        );
                        let mut first = None;
                        for (seconds, app) in &apps {
                            let compiled = compile_view(app, 0, page);
                            let at = format!(
                                "{seconds} s into a {width}×{height} page at {scale}×{}",
                                if result { " beside a result" } else { "" }
                            );
                            let painted = |key: &str| {
                                compiled
                                    .text_fit_audit_line(&UiKey::new(key))
                                    .map(|audit| painted(&audit).to_owned())
                                    .unwrap_or_default()
                            };
                            let line = painted(&format!("{guidance}/0"));
                            assert!(whole(&compiled, page, &format!("{guidance}/0")), "{at}");
                            let face = painted("system-state/apply");
                            if whole(&compiled, page, "system-state/apply") {
                                let audit = compiled
                                    .text_fit_audit_line(&UiKey::new("system-state/apply"))
                                    .unwrap();
                                let (_, room) = audit.split_once(" available=").unwrap();
                                let room: f32 = room.split(' ').next().unwrap().parse().unwrap();
                                if crate::native_ui::native_button_label_width("Apply (30)")
                                    <= room + crate::native_ui::TEXT_FIT_SLACK
                                {
                                    assert!(
                                        face.contains(&format!("({seconds})")),
                                        "Apply's face {at}: {face:?}"
                                    );
                                }
                            }
                            assert!(
                                whole(&compiled, page, "system-state/apply")
                                    && face.contains(&format!("({seconds})"))
                                    || line.starts_with(&format!("{seconds} s")),
                                "the countdown {at}: {line:?}, Apply {face:?}"
                            );
                            if line.starts_with(&format!("{seconds} s")) {
                                assert!(!face.contains('('), "twice {at}: {line:?}, {face:?}");
                            }
                            let guided = (0..)
                                .map(|line| painted(&format!("{guidance}/{line}")))
                                .take_while(|line| !line.is_empty())
                                .collect::<Vec<_>>()
                                .join(" ");
                            let inner = width as f32 - 24.0;
                            assert!(guided.contains("to stop"), "how to stop {at}: {guided:?}");
                            if line_fits("Click, type or scroll to stop automatic Apply.…", inner)
                            {
                                assert!(
                                    guided.contains("Click, type or scroll to stop"),
                                    "how to stop {at}: {guided:?}"
                                );
                            }
                            let lines = (0..)
                                .take_while(|line| {
                                    compiled
                                        .semantic(&UiKey::new(format!("{guidance}/{line}")))
                                        .is_some()
                                })
                                .count();
                            let forms = if line.starts_with(&format!("{seconds} s")) {
                                [
                                    "30 s to Apply. Click, type or scroll to stop.",
                                    "30 s to Apply. Click or scroll to stop.",
                                    "30 s to Apply. Click to stop.",
                                    "30 s. Click to stop.",
                                ]
                            } else {
                                [
                                    "Click, type or scroll to stop automatic Apply.",
                                    "Click or scroll to stop automatic Apply.",
                                    "Click to stop automatic Apply.",
                                    "Click to stop.",
                                ]
                            };
                            let shown = forms
                                .iter()
                                .position(|form| {
                                    let form = form.replacen("30 s", &format!("{seconds} s"), 1);
                                    guided.starts_with(form.trim_end_matches('.'))
                                })
                                .unwrap_or_else(|| panic!("the warning {at}: {guided:?}"));
                            if let Some(fuller) = shown.checked_sub(1) {
                                assert!(
                                    wrap_lines(forms[fuller], inner, None).len() > lines,
                                    "{:?} fits {at}: {guided:?}",
                                    forms[fuller]
                                );
                            }
                            let frame = (
                                lines,
                                face.contains('('),
                                compiled
                                    .semantic(&UiKey::new("system-state/rows"))
                                    .map(|node| (node.label.clone(), node.rect)),
                                [
                                    "system-state/url",
                                    "system-state/password",
                                    "system-state/apply",
                                    "system-state/restore",
                                ]
                                .map(|key| {
                                    compiled.semantic(&UiKey::new(key)).map(|node| node.rect)
                                }),
                            );
                            match &first {
                                None => first = Some(frame),
                                Some(before) => assert_eq!(&frame, before, "the page {at}"),
                            }
                        }
                    }
                }
            }
        }
        let _ = install_preferences(AppearancePreferences::default());
    }

    #[test]
    fn a_settings_name_is_never_cut() {
        names_are_whole(1.0);
    }

    #[test]
    fn a_settings_name_is_never_cut_in_the_ui_face() {
        crate::tray_raster::prepare_ui_fonts_for_direct_view_test();
        names_are_whole(1.0);
    }

    #[test]
    fn a_settings_name_is_never_cut_at_a_large_text_size() {
        use crate::native_appearance::{AppearancePreferences, install_preferences};
        crate::tray_raster::prepare_ui_fonts_for_direct_view_test();
        for scale in [1.5, 2.0] {
            let _ = install_preferences(AppearancePreferences {
                text_scale: scale,
                ..AppearancePreferences::default()
            });
            names_are_whole(scale);
        }
        let _ = install_preferences(AppearancePreferences::default());
    }

    fn names_are_whole(scale: f32) {
        let plain = SystemStateApp::new(Plan::default());
        let mut checking = SystemStateApp::new(Plan::default());
        checking.publish(SetupStatus {
            busy: true,
            rows: vec![SetupStatusRow {
                name: "Dock".into(),
                status: "running".into(),
                detail: String::new(),
            }],
            ..SetupStatus::default()
        });
        let off = SystemStateApp::new(Plan::from(aterm_types::macos_setup::Selection::none()));
        let squeeze = |words: &str| words.split_whitespace().collect::<String>();
        for width in 128..=800 {
            let page = crate::native_ui::LogicalRect::new(0.0, 0.0, width as f32, 12_000.0);
            for app in [&plain, &checking, &off] {
                let compiled = compile_view(app, 0, page);
                for (index, setting) in Setting::ALL.iter().enumerate() {
                    let audits: Vec<String> = (0..)
                        .map_while(|line| {
                            compiled.text_fit_audit_line(&UiKey::new(format!(
                                "system-state/setting/{index}/label/{line}"
                            )))
                        })
                        .collect();
                    let lines: Vec<&str> = audits
                        .iter()
                        .map(|audit| {
                            assert!(!audit.contains("overflow=true"), "{audit} at {width}");
                            assert!(!audit.contains("……"), "{audit} at {width}");
                            painted(audit)
                        })
                        .collect();
                    let words = lines.join(" ");
                    let inner = width as f32 - 24.0;
                    if aterm_grapheme::pieces(
                        setting.label(),
                        aterm_grapheme::BreakKind::is_word_boundary,
                    )
                    .iter()
                    .all(|piece| line_fits(piece.text, inner))
                    {
                        assert!(words.starts_with(setting.label()), "{lines:?} at {width}");
                    } else {
                        assert!(
                            squeeze(&words).starts_with(&squeeze(setting.label())),
                            "{lines:?} at {width}"
                        );
                    }
                    let (_, current, desired) =
                        app.setting_station_state(*setting, &app.reviewed_plan());
                    let full = format!("Current: {current} → Desired: {desired}");
                    let short = format!("Current: {current}");
                    let state = words.split_once(" · ").map_or("", |(_, state)| state);
                    assert!(
                        ["", full.as_str(), short.as_str()].contains(&state),
                        "{words:?} at {width}"
                    );
                    let last = lines[lines.len() - 1];
                    let bare = last.split_once(" · ").map_or(last, |(bare, _)| bare);
                    let (_, room) = audits[audits.len() - 1].split_once(" available=").unwrap();
                    let room: f32 = room.split(' ').next().unwrap().parse().unwrap();
                    let fuller = if state == short {
                        Some(full.as_str())
                    } else if state.is_empty() && !bare.ends_with('…') {
                        Some(short.as_str())
                    } else {
                        None
                    };
                    if let Some(fuller) = fuller {
                        assert!(
                            !line_fits(&format!("{bare} · {fuller}"), room),
                            "{words:?} at {width}"
                        );
                    }
                }
            }
        }
        let page = crate::native_ui::LogicalRect::new(0.0, 0.0, 1_400.0, 4_000.0);
        for app in [&plain, &checking, &off]
            .into_iter()
            .filter(|_| scale == 1.0)
        {
            let compiled = compile_view(app, 0, page);
            for index in 0..Setting::ALL.len() {
                let words: String = (0..)
                    .map_while(|line| {
                        compiled.text_fit_audit_line(&UiKey::new(format!(
                            "system-state/setting/{index}/label/{line}"
                        )))
                    })
                    .map(|audit| painted(&audit).to_owned())
                    .collect();
                assert!(words.contains(" · Current: "), "{words:?}");
            }
        }
    }

    #[test]
    fn a_word_wider_than_the_page_breaks_between_graphemes() {
        let path = "/Users//someone/Library/Application Support/aterm/withheld-preferences.json";
        for width in [40.0, 80.0, 156.0] {
            let lines = wrap_lines(path, width, None);
            assert!(lines.len() > 1, "{lines:?} at {width}");
            for line in &lines {
                assert!(
                    crate::native_ui::native_button_label_width(line)
                        <= width + crate::native_ui::TEXT_FIT_SLACK,
                    "{line:?} at {width}"
                );
            }
            assert_eq!(lines.concat().replace(' ', ""), path.replace(' ', ""));
        }
        let long = "word ".repeat(120);
        let lines = wrap_lines(&long, 100_000.0, None);
        assert_eq!(lines.len(), 3, "the painter draws 256 graphemes of a line");
        assert_eq!(lines.join(" "), long.trim_end());
        let word: String = (0..2_000)
            .map(|at| char::from(b'a' + (at % 26) as u8))
            .collect();
        let lines = wrap_lines(&word, 156.0, None);
        assert!(lines.len() > 50, "{}", lines.len());
        assert!(lines.iter().all(|line| line_fits(line, 156.0)));
        assert_eq!(lines.concat(), word);
        assert_eq!(wrap_lines(&word, 156.0, Some(2)).len(), 3);
        assert_eq!(wrap_lines("", 80.0, None), vec![String::new()]);
        let widths = [10.0, 20.0, 19.0, 30.0, 25.0, 40.0];
        for room in [0.0, 10.0, 19.5, 20.0, 26.0, 100.0] {
            let fitting = most_fitting(widths.len(), |count| widths[count - 1] <= room);
            assert!(
                fitting == 0 || widths[fitting - 1] <= room,
                "{fitting} in {room}"
            );
        }
        let line = "Setup: failed · Cancel was not saved. Try again after 15 seconds; 3 settings";
        for width in (60..=600).step_by(3) {
            let cut = elided(line, width as f32);
            let stem = cut.strip_suffix('…').unwrap();
            assert!(line_fits(&cut, width as f32), "{cut:?} at {width}");
            assert!(line.starts_with(stem), "{cut:?} at {width}");
            assert!(
                line[stem.len()..].starts_with(|c: char| c.is_whitespace() || ".,;:·".contains(c))
                    || stem == line,
                "{cut:?} at {width}"
            );
            assert!(
                !stem.ends_with(|c: char| ".,;:·".contains(c)),
                "{cut:?} at {width}"
            );
            if stem != line {
                let rest = &line[stem.len()..];
                let next = rest
                    .find(|c: char| !(c.is_whitespace() || ".,;:·".contains(c)))
                    .unwrap_or(rest.len());
                let next = rest[next..]
                    .find(char::is_whitespace)
                    .map_or(line.len(), |end| stem.len() + next + end);
                let longer = line[..next].trim_end_matches(|c: char| ".,;:·".contains(c));
                assert!(
                    !line_fits(&format!("{longer}…"), width as f32),
                    "{cut:?} at {width}"
                );
            }
        }
        for width in (40..=300).step_by(2) {
            let cut = elided("Setup: assembling… then more words", width as f32);
            assert!(!cut.contains("……"), "{cut:?} at {width}");
        }
        let stem = "Check failed (Desktop";
        let width = crate::native_ui::native_button_label_width(&format!("{stem}…")) + 1.0;
        assert_eq!(
            elided("Check failed (Desktop) twice", width),
            format!("{stem}…")
        );
        let line = "設定を保存できませんでした。もう一度お試しください。";
        for width in (20..=400).step_by(2) {
            let cut = elided(line, width as f32);
            let stem = cut.strip_suffix('…').unwrap();
            assert!(line.starts_with(stem), "{cut:?} at {width}");
            assert!(!stem.ends_with('。'), "{cut:?} at {width}");
            assert!(
                line_fits(&cut, width as f32) || stem.is_empty(),
                "{cut:?} at {width}"
            );
            let rest = &line[stem.len()..];
            let stops = rest
                .chars()
                .take_while(|&c| c == '。')
                .map(char::len_utf8)
                .sum::<usize>();
            if let Some(next) = rest[stops..].chars().next() {
                let longer = format!("{}{next}…", &line[..stem.len() + stops]);
                assert!(!line_fits(&longer, width as f32), "{cut:?} at {width}");
            }
            if line_fits("設…", width as f32) {
                assert!(!stem.is_empty(), "{cut:?} at {width}");
            }
        }
        assert_eq!(
            wrap_lines("Sign out of Messages", 600.0, None),
            vec!["Sign out of Messages"]
        );
        for line in [
            "requires macOS 26.1 or later",
            "Moved 1,234 files today",
            "Range 1–5 shown",
            "You can’t apply this",
            "The vault isn’t open",
            "시스템 설정을 확인하세요",
        ] {
            let words: Vec<&str> = line.split(' ').collect();
            let stems: Vec<String> = (1..=words.len())
                .map(|count| words[..count].join(" "))
                .collect();
            for width in 10..=400 {
                let cut = elided(line, width as f32);
                let stem = cut.strip_suffix('…').unwrap();
                assert!(
                    stems.iter().any(|whole| whole == stem)
                        || words[0].starts_with(stem) && stem.len() < words[0].len(),
                    "{cut:?} at {width}"
                );
            }
        }
    }

    #[test]
    fn a_page_shows_every_result_that_fits() {
        let mut app = SystemStateApp::new(Plan::default());
        app.publish(SetupStatus {
            busy: false,
            rows: (0..200)
                .map(|at| SetupStatusRow {
                    name: format!("Row {at}"),
                    status: "applied".into(),
                    detail: String::new(),
                })
                .collect(),
            ..SetupStatus::default()
        });
        for height in [500.0, 900.0, 1_400.0] {
            let page = crate::native_ui::LogicalRect::new(0.0, 0.0, 720.0, height);
            for scroll in [Setting::ALL.len(), 100] {
                let compiled = compile_view(&app, scroll, page);
                let rect = |key: &str| {
                    compiled
                        .paint
                        .iter()
                        .find(|node| node.key.as_str() == key)
                        .map(|node| node.rect)
                        .unwrap()
                };
                let rows = compiled
                    .semantic(&UiKey::new("system-state/rows"))
                    .unwrap()
                    .rect;
                let caption = rect("system-state/scroll-caption");
                assert!(
                    rows.bottom() - caption.bottom() < 20.0,
                    "{} under the caption at {height} scrolled to {scroll}",
                    rows.bottom() - caption.bottom()
                );
            }
        }
    }

    #[test]
    fn a_page_wraps_only_the_results_it_can_show() {
        crate::tray_raster::prepare_ui_fonts_for_direct_view_test();
        let page = crate::native_ui::LogicalRect::new(0.0, 0.0, 720.0, 500.0);
        let cost = |count: usize| {
            let mut app = SystemStateApp::new(Plan::default());
            app.publish(SetupStatus {
                busy: false,
                rows: (0..count)
                    .map(|at| SetupStatusRow {
                        name: format!("Row {at}"),
                        status: "error".into(),
                        detail: format!("ERR{at} {}", "extended diagnostic ".repeat(12)),
                    })
                    .collect(),
                ..SetupStatus::default()
            });
            let before = crate::tray_raster::font_store_visits();
            compile_view(&app, 100, page);
            crate::tray_raster::font_store_visits() - before
        };
        let few = cost(200);
        let many = cost(2_000);
        assert!(many < few * 2, "{many} font-store visits against {few}");
    }

    #[test]
    fn a_button_face_is_never_cut() {
        use crate::native_appearance::{AppearancePreferences, install_preferences};
        crate::tray_raster::prepare_ui_fonts_for_direct_view_test();
        let (send, _receive) = mpsc::channel();
        let plain = SystemStateApp::new(Plan::default());
        let mut offered = SystemStateApp::new(Plan::default());
        offered.offer(Request::new(Plan::default(), send));
        for scale in [1.0, 1.5, 2.0] {
            let _ = install_preferences(AppearancePreferences {
                text_scale: scale,
                ..AppearancePreferences::default()
            });
            for width in (128..=720).step_by(4) {
                let inner = width as f32 - 24.0;
                let fits = ["Apply", "Cancel", "Check", "Restore"].iter().all(|label| {
                    crate::native_ui::native_button_label_width(label)
                        <= crate::native_ui::text_button_label_room(&ButtonSpec::new(*label), inner)
                            + crate::native_ui::TEXT_FIT_SLACK
                });
                if !fits {
                    continue;
                }
                let page = crate::native_ui::LogicalRect::new(0.0, 0.0, width as f32, 900.0);
                for app in [&plain, &offered] {
                    let compiled = compile_view(app, 0, page);
                    for key in ["apply", "cancel", "check", "restore"] {
                        let audit = compiled
                            .text_fit_audit_line(&UiKey::new(format!("system-state/{key}")))
                            .unwrap();
                        assert!(
                            !audit.contains("overflow=true"),
                            "{audit} at {width} and {scale}×"
                        );
                    }
                }
            }
        }
        let _ = install_preferences(AppearancePreferences::default());
    }

    #[test]
    fn a_switch_word_and_the_title_are_whole_at_every_text_size() {
        use crate::native_appearance::{AppearancePreferences, install_preferences};
        crate::tray_raster::prepare_ui_fonts_for_direct_view_test();
        let on = SystemStateApp::new(Plan::default());
        let off = SystemStateApp::new(Plan::from(aterm_types::macos_setup::Selection::none()));
        for scale in [1.0, 1.5, 2.0] {
            let _ = install_preferences(AppearancePreferences {
                text_scale: scale,
                ..AppearancePreferences::default()
            });
            for width in (128..=720).step_by(8) {
                let page = crate::native_ui::LogicalRect::new(0.0, 0.0, width as f32, 8_000.0);
                for app in [&on, &off] {
                    let compiled = compile_view(app, 0, page);
                    for index in 0..Setting::ALL.len() {
                        let audit = compiled
                            .text_fit_audit_line(&UiKey::new(format!(
                                "system-state/setting/{index}"
                            )))
                            .unwrap_or_else(|| panic!("row {index} at {width} and {scale}×"));
                        assert!(
                            !audit.contains("overflow=true"),
                            "{audit} at {width} and {scale}×"
                        );
                    }
                    assert!(
                        whole(&compiled, page, "system-state/title"),
                        "the title at {width} and {scale}×"
                    );
                    let help = (0..)
                        .map_while(|line| {
                            compiled
                                .text_fit_audit_line(&UiKey::new(format!(
                                    "system-state/help/{line}"
                                )))
                                .map(|audit| painted(&audit).to_owned())
                        })
                        .collect::<Vec<_>>()
                        .join(" ");
                    assert_eq!(
                        help, "Choose your Mac settings, then Apply.",
                        "the help at {width} and {scale}×"
                    );
                    let title = compiled
                        .semantic(&UiKey::new("system-state/title"))
                        .unwrap();
                    assert_eq!(title.label, "System State");
                    assert!(
                        compiled
                            .semantic(&UiKey::new("system-state/title/lines/0"))
                            .is_none()
                    );
                    if compiled
                        .semantic(&UiKey::new("system-state/brand"))
                        .is_some()
                    {
                        assert!(
                            whole(&compiled, page, "system-state/wordmark"),
                            "the wordmark at {width} and {scale}×"
                        );
                        let banner = compiled
                            .paint
                            .iter()
                            .find(|node| node.key.as_str() == "system-state/rainbow")
                            .unwrap();
                        assert!(
                            banner.rect.width >= 47.0,
                            "the banner at {width} and {scale}×"
                        );
                    }
                }
            }
        }
        let _ = install_preferences(AppearancePreferences::default());
    }

    #[test]
    fn a_scroll_caption_is_never_cut() {
        use crate::native_appearance::{AppearancePreferences, install_preferences};
        crate::tray_raster::prepare_ui_fonts_for_direct_view_test();
        let app = SystemStateApp::new(Plan::default());
        for scale in [1.0, 1.5, 2.0] {
            let _ = install_preferences(AppearancePreferences {
                text_scale: scale,
                ..AppearancePreferences::default()
            });
            for width in (128..=480).step_by(4) {
                for height in [194, 210, 226, 242, 258, 270, 286, 334, 500, 742] {
                    let page =
                        crate::native_ui::LogicalRect::new(0.0, 0.0, width as f32, height as f32);
                    for scroll in [0, 9] {
                        let compiled = compile_view(&app, scroll, page);
                        if let Some(audit) =
                            compiled.text_fit_audit_line(&UiKey::new("system-state/scroll-caption"))
                        {
                            let caption = compiled
                                .semantic(&UiKey::new("system-state/scroll-caption"))
                                .unwrap();
                            let at = format!("{width}×{height} and {scale}×");
                            assert_eq!(painted(&audit), caption.label, "{audit} at {at}");
                            assert!(
                                whole(&compiled, page, "system-state/scroll-caption"),
                                "{audit} at {at}"
                            );
                            let rows = compiled.semantic(&UiKey::new("system-state/rows")).unwrap();
                            let (_, range) = rows.label.rsplit_once("rows ").unwrap();
                            let (range, total) = range.split_once(" of ").unwrap();
                            let (_, end) = range.split_once('–').unwrap();
                            let forms = [
                                format!("{range} of {total} · Scroll for more"),
                                format!("{range} of {total}"),
                                format!("{range}/{total}"),
                                format!("{end}/{total}"),
                            ];
                            let fullest = forms
                                .iter()
                                .find(|form| line_fits(form, width as f32 - 24.0))
                                .unwrap_or(&forms[3]);
                            assert_eq!(&caption.label, fullest, "{at}");
                        }
                    }
                }
            }
        }
        let _ = install_preferences(AppearancePreferences::default());
    }

    #[test]
    fn a_line_is_as_tall_as_its_type() {
        use crate::native_appearance::{AppearancePreferences, install_preferences};
        crate::tray_raster::prepare_ui_fonts_for_direct_view_test();
        let (send, _receive) = mpsc::channel();
        let mut offered = SystemStateApp::new(Plan::default());
        offered.offer(Request::new(Plan::default(), send));
        offered.publish(SetupStatus {
            busy: false,
            rows: vec![SetupStatusRow {
                name: "Check".into(),
                status: "error".into(),
                detail: "extended diagnostic ".repeat(40),
            }],
            ..SetupStatus::default()
        });
        for scale in [1.0, 1.5, 2.0] {
            let _ = install_preferences(AppearancePreferences {
                text_scale: scale,
                ..AppearancePreferences::default()
            });
            let (px, _) = crate::native_ui::text_paint_metrics(SemanticRole::Text, StyleRef::Plain);
            for (width, height) in [(300.0, 500.0), (720.0, 900.0), (1_400.0, 1_000.0)] {
                let page = crate::native_ui::LogicalRect::new(0.0, 0.0, width, height);
                let compiled = compile_view(&offered, 0, page);
                let mut lines = 0;
                for node in &compiled.paint {
                    let on_page = node.rect.y >= 0.0 && node.rect.bottom() <= height;
                    if on_page
                        && compiled
                            .text_fit_audit_line(&node.key)
                            .is_some_and(|audit| audit.contains("kind=text "))
                    {
                        lines += 1;
                        assert!(
                            node.rect.height >= px * 1.2,
                            "{} is {} tall for {px} type at {width}×{height}",
                            node.key.as_str(),
                            node.rect.height
                        );
                    }
                }
                assert!(lines > 4, "{lines} lines at {width}×{height} and {scale}×");
            }
        }
        let _ = install_preferences(AppearancePreferences::default());
    }

    #[test]
    fn a_short_page_keeps_its_hints_for_voiceover() {
        let app = SystemStateApp::new(Plan::default());
        let page = crate::native_ui::LogicalRect::new(0.0, 0.0, 624.0, 334.0);
        let compiled = compile_view(&app, 0, page);
        for (hint, key) in [
            ("system-state/vault-help", "system-state/password"),
            ("system-state/restore-help", "system-state/restore"),
        ] {
            assert!(compiled.semantic(&UiKey::new(hint)).is_none(), "{hint}");
            let tall = compile_view(
                &app,
                0,
                crate::native_ui::LogicalRect::new(0.0, 0.0, 624.0, 1_000.0),
            );
            let words: String = (0..)
                .map_while(|line| {
                    tall.text_fit_audit_line(&UiKey::new(format!("{hint}/{line}")))
                        .map(|audit| painted(&audit).to_owned())
                })
                .collect::<Vec<_>>()
                .join(" ");
            assert_eq!(
                compiled
                    .semantic(&UiKey::new(key))
                    .unwrap()
                    .description
                    .as_deref(),
                Some(words.as_str()),
                "{key}"
            );
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

    #[test]
    fn rainbow_kitty_village_and_current_desired_state_animate_across_parallel_init() {
        use crate::native_app::ViewMotionCx;
        use crate::native_ui::LogicalRect;
        let viewport = LogicalRect::new(0.0, 0.0, 999.0, 740.0);
        let mut app = SystemStateApp::new(Plan::default());
        app.publish(SetupStatus {
            busy: true,
            rows: vec![
                SetupStatusRow {
                    name: "Caps Lock is Control on attached keyboards".into(),
                    status: "ok".into(),
                    detail: "0x700000039 -> 0x7000000E0".into(),
                },
                SetupStatusRow {
                    name: "Natural scrolling is off".into(),
                    status: "drift".into(),
                    detail: "true → false".into(),
                },
            ],
            ..SetupStatus::default()
        });
        let view = SystemStateViewState::default();
        let frame0 = app
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
        let setting0 = frame0
            .semantic(&UiKey::new("system-state/setting/0"))
            .unwrap();
        assert_eq!(
            setting0.description.as_deref(),
            Some("Current: verified → Desired: Control")
        );
        let rainbow0 = frame0
            .semantic(&UiKey::new("system-state/rainbow"))
            .unwrap();
        assert_eq!(
            rainbow0.value,
            SemanticValue::Text("phase=0;busy=1;done=1;total=12;states=GDWWWWWWWWWW".into())
        );

        assert!(app.tick(Instant::now(), true));
        let frame1 = app
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
        let rainbow1 = frame1
            .semantic(&UiKey::new("system-state/rainbow"))
            .unwrap();
        assert_eq!(
            rainbow1.value,
            SemanticValue::Text("phase=120;busy=1;done=1;total=12;states=GDWWWWWWWWWW".into())
        );
        assert_ne!(
            frame0.paint, frame1.paint,
            "animated rainbow kitty village advances paint commands across busy ticks"
        );
    }
}
