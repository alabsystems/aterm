// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Boundary between the System State tab and the binary's setup workers.

use std::cell::Cell;
use std::sync::Mutex;
use std::sync::mpsc::{self, Receiver, Sender, TryRecvError};
use std::time::{Duration, Instant};

use aterm_secrets::SecretBytes;
use aterm_types::macos_setup::Plan;

use crate::native_app::{AppKind, NativeApp};
use crate::system_state::{SetupStatus, SetupStatusRow, SystemStateAction};
use crate::workstation_setup::{Request, WorkstationSetupDecision};
use crate::{App, Wake};

/// Provisioning callbacks installed by the shipped binary.
#[derive(Clone, Copy)]
pub struct InitialSetupHandlers {
    /// Applies the reviewed plan with its vault-typed flag, optional join password, and receipt.
    pub apply: fn(Plan, bool, Option<SecretBytes>, InitialSetupApplyReceipt) -> bool,
    pub check: fn(Plan) -> bool,
    pub restore: fn() -> bool,
    pub cancel: fn(InitialSetupCancelReceipt) -> bool,
    pub status: fn() -> SetupStatus,
}

/// Receipt owned by a queued Apply until its plan is durably authorized.
#[must_use = "accept only after the plan is durable; dropping preserves the draft"]
#[derive(Debug)]
pub struct InitialSetupApplyReceipt {
    answer: Option<Sender<bool>>,
}

impl InitialSetupApplyReceipt {
    fn new() -> (Self, Receiver<bool>) {
        let (answer, receiver) = mpsc::channel();
        (
            Self {
                answer: Some(answer),
            },
            receiver,
        )
    }

    /// Confirm that the worker holds the claim and durably saved the reviewed plan.
    pub fn accept(mut self) {
        self.finish(true);
    }

    fn finish(&mut self, accepted: bool) {
        if let Some(answer) = self.answer.take() {
            let _ = answer.send(accepted);
            crate::workstation_setup::send_setup_wake(Wake::InitialSetupApplyResolved);
        }
    }
}

impl Drop for InitialSetupApplyReceipt {
    fn drop(&mut self) {
        self.finish(false);
    }
}

/// Receipt completed after the Cancel choice is durably saved or superseded.
#[must_use = "complete only after cancellation is durable"]
#[derive(Debug)]
pub struct InitialSetupCancelReceipt {
    complete: Sender<()>,
}

impl InitialSetupCancelReceipt {
    fn new() -> (Self, Receiver<()>) {
        let (complete, receive) = mpsc::channel();
        (Self { complete }, receive)
    }

    pub fn complete(self) {
        let _ = self.complete.send(());
    }
}

struct CancelCompletion {
    answer: Receiver<()>,
    result: Cell<Option<bool>>,
}

impl CancelCompletion {
    const fn new(answer: Receiver<()>) -> Self {
        Self {
            answer,
            result: Cell::new(None),
        }
    }

    fn poll(&self) -> Option<bool> {
        self.result.get().or_else(|| {
            let result = match self.answer.try_recv() {
                Ok(()) => Some(true),
                Err(TryRecvError::Empty) => None,
                Err(TryRecvError::Disconnected) => Some(false),
            };
            self.result.set(result);
            result
        })
    }
}

struct PendingApply {
    instance: crate::native_app::AppInstanceId,
    plan: Plan,
    answer: Receiver<bool>,
}

static HANDLERS: Mutex<Option<InitialSetupHandlers>> = Mutex::new(None);

pub fn set_initial_setup_handlers(handlers: InitialSetupHandlers) {
    if let Ok(mut current) = HANDLERS.lock() {
        *current = Some(handlers);
    }
}

pub fn publish_initial_setup_status(status: SetupStatus) {
    crate::workstation_setup::send_setup_wake(Wake::InitialSetupStatus {
        generation: None,
        status,
    });
}

#[derive(Default)]
pub(crate) struct Host {
    pending: Option<Request>,
    pending_apply: Option<PendingApply>,
    cancel_receipts: Vec<CancelCompletion>,
    cancel_failed: bool,
    next_status: Option<Instant>,
    status_in_flight: bool,
    next_tick: Option<Instant>,
    generation: u64,
    offer_presented: bool,
    action_refusal: Option<SetupStatusRow>,
    latest_status: SetupStatus,
    vault_synced: bool,
    #[cfg(target_os = "macos")]
    notice_shown: bool,
}

impl Host {
    pub(crate) fn deadline(&self, now: Instant) -> Option<Instant> {
        self.next_status
            .into_iter()
            .chain(self.next_tick)
            .min()
            .map(|next| next.max(now))
    }
}

impl App {
    fn resolve_system_state_cancellations(&mut self) -> bool {
        let before = self.system_state_host.cancel_failed;
        let failed = &mut self.system_state_host.cancel_failed;
        self.system_state_host
            .cancel_receipts
            .retain(|receipt| match receipt.poll() {
                Some(true) => false,
                None => true,
                Some(false) => {
                    *failed = true;
                    false
                }
            });
        !before && self.system_state_host.cancel_failed
    }

    pub(crate) fn system_state_cancel_pending(&self) -> bool {
        self.system_state_host.cancel_failed
            || self
                .system_state_host
                .cancel_receipts
                .iter()
                .any(|receipt| receipt.poll() != Some(true))
            || self
                .native_runtime
                .instance_by_kind(AppKind::SystemState)
                .and_then(|instance| self.native_runtime.app(instance))
                .is_some_and(
                    |app| matches!(app, NativeApp::SystemState(state) if state.has_queued_cancel()),
                )
    }

    /// Transfers remaining form cancellation intents after the event loop ends and waits for them.
    pub(crate) fn finish_system_state_shutdown(&mut self) -> Result<(), &'static str> {
        if let Some(instance) = self.native_runtime.instance_by_kind(AppKind::SystemState)
            && self.native_runtime.take_system_state_cancel(instance)
        {
            let handlers = HANDLERS.lock().ok().and_then(|handlers| *handlers);
            if !self.start_system_state_cancel(handlers) {
                self.system_state_host.cancel_failed = true;
            }
        }
        if let Some(request) = self.system_state_host.pending.take() {
            request.finish(WorkstationSetupDecision::Unavailable);
        }
        self.resolve_system_state_cancellations();
        let result =
            wait_for_cancellations(std::mem::take(&mut self.system_state_host.cancel_receipts));
        if self.system_state_host.cancel_failed {
            Err("Cancel was not saved before shutdown")
        } else {
            result
        }
    }

    pub(crate) fn receive_system_state_status(
        &mut self,
        generation: Option<u64>,
        status: SetupStatus,
    ) {
        let Some(generation) = generation else {
            self.system_state_host.generation = self.system_state_host.generation.wrapping_add(1);
            self.system_state_host.status_in_flight = false;
            self.system_state_host.next_status = Some(Instant::now());
            self.system_state_host.next_tick = None;
            self.request_redraw_all_windows();
            return;
        };
        if generation != self.system_state_host.generation {
            return;
        }
        self.observe_initial_setup_permissions(status.permissions);
        self.publish_system_state(status);
    }
    pub(crate) fn offer_system_state(&mut self, request: Request) {
        if !request.pending()
            || self.headless
            || !self.can_offer_system_state()
            || !self.open_system_state_tab()
        {
            request.finish(WorkstationSetupDecision::Unavailable);
            return;
        }
        self.attach_system_state_offer(request);
    }

    fn can_offer_system_state(&self) -> bool {
        self.system_state_host.pending.is_none()
            && !self.system_state_cancel_pending()
            && self
                .native_runtime
                .instance_by_kind(AppKind::SystemState)
                .is_none_or(|instance| {
                    matches!(self.native_runtime.app(instance), Some(NativeApp::SystemState(state)) if state.can_offer())
                })
    }

    fn attach_system_state_offer(&mut self, request: Request) {
        if !request.pending() || !self.can_offer_system_state() {
            request.finish(WorkstationSetupDecision::Unavailable);
            return;
        }
        if !self.native_runtime.offer_system_state(request.clone()) {
            request.finish(WorkstationSetupDecision::Unavailable);
            return;
        }
        self.system_state_host.pending = Some(request);
        self.hold_initial_setup_permissions();
        self.refresh_system_state_tab_indicators();
        self.system_state_host.pending_apply = None;
        self.system_state_host.offer_presented = false;
        self.system_state_host.action_refusal = None;
        self.system_state_host.latest_status = SetupStatus::default();
        let views: Vec<_> = self
            .view_store
            .iter()
            .filter_map(|(view, state)| match state {
                crate::tab_model::View::Native(native)
                    if self
                        .native_runtime
                        .app(native.instance)
                        .is_some_and(|app| app.kind() == AppKind::SystemState) =>
                {
                    Some(view)
                }
                _ => None,
            })
            .collect();
        for view in views {
            if let Some(state) = self.native_runtime.view_state_mut(view) {
                state.common_mut().last_focus = None;
                state.common_mut().focus_visible = false;
            }
        }
        self.system_state_host.next_status = Some(Instant::now());
        self.request_redraw_all_windows();
    }

    pub(crate) fn publish_system_state(&mut self, status: SetupStatus) {
        self.publish_system_state_with_notice(status, |app| app.present_system_state_indicator());
    }

    /// Opens or focuses System State in `wid` and presets its review for vault sync when allowed.
    pub(crate) fn review_vault_sync_in_window(&mut self, wid: crate::WindowId) -> bool {
        if !self.open_system_state_tab_in_window(wid) {
            return false;
        }
        self.interact_system_state(wid);
        let unread = self
            .native_runtime
            .instance_by_kind(AppKind::SystemState)
            .and_then(|instance| self.native_runtime.app(instance))
            .is_some_and(|app| matches!(app, NativeApp::SystemState(state) if state.unread()));
        if unread && self.system_state_host.latest_status.plan.is_some() {
            self.publish_system_state(self.system_state_host.latest_status.clone());
        }
        if self.native_runtime.preset_system_state_vault_sync() {
            self.refresh_system_state_tab_indicators();
            self.request_redraw_all_windows();
        }
        true
    }

    fn present_system_state_indicator(&mut self) -> bool {
        let has_tab = self.windows.values().any(|window| {
            window.tab_set.tabs().iter().any(|tab| {
                tab.root.any_leaf(&mut |view| {
                    matches!(self.view_store.get(*view), Some(crate::tab_model::View::Native(native))
                        if matches!(self.native_runtime.app(native.instance), Some(NativeApp::SystemState(_))))
                })
            })
        });
        if has_tab {
            self.refresh_system_state_tab_indicators();
        }
        has_tab
    }

    fn publish_system_state_with_notice(
        &mut self,
        mut status: SetupStatus,
        present_notice: impl FnOnce(&mut Self) -> bool,
    ) {
        #[cfg(not(target_os = "macos"))]
        let _ = present_notice;
        let synced = status.vault_synced();
        let was = std::mem::replace(&mut self.system_state_host.vault_synced, synced);
        if synced && !was {
            self.secrets_synced_by_setup();
        }
        self.system_state_host.latest_status = status.clone();
        if self
            .system_state_host
            .pending_apply
            .as_ref()
            .is_some_and(|pending| self.system_state_instance_has_view(pending.instance))
        {
            status.busy = true;
        }
        if let Some(refusal) = &self.system_state_host.action_refusal {
            status.rows.insert(0, refusal.clone());
        }
        #[cfg(target_os = "macos")]
        let reopened_attention =
            status.needs_attention(Some(status.plan.as_ref().unwrap_or(&Plan::default())));
        let changed = self.native_runtime.publish_system_state(status);
        #[cfg(target_os = "macos")]
        if !self.system_state_host.notice_shown
            && self
                .native_runtime
                .instance_by_kind(AppKind::SystemState)
                .and_then(|instance| self.native_runtime.app(instance))
                .and_then(|app| match app {
                    NativeApp::SystemState(state) => Some(state.needs_attention()),
                    _ => None,
                })
                .unwrap_or(reopened_attention)
            && self.system_state_host.pending.is_none()
            && !self.windows.keys().any(|wid| {
                self.active_native_view(*wid).is_some_and(|(instance, _)| {
                    self.native_runtime
                        .app(instance)
                        .is_some_and(|app| app.kind() == AppKind::SystemState)
                })
            })
        {
            self.system_state_host.notice_shown = present_notice(self);
        }
        self.system_state_host.status_in_flight = false;
        self.system_state_host.next_status = Some(Instant::now() + Duration::from_secs(1));
        let indicators_changed = self.refresh_system_state_tab_indicators();
        if changed || indicators_changed {
            self.request_redraw_all_windows();
        }
    }

    fn refresh_system_state_tab_indicators(&mut self) -> bool {
        let mut tabs = Vec::new();
        for (wid, window) in &self.windows {
            for tab in window.tab_set.tabs() {
                if tab.root.any_leaf(&mut |view| {
                    matches!(self.view_store.get(*view), Some(crate::tab_model::View::Native(native))
                        if matches!(self.native_runtime.app(native.instance), Some(NativeApp::SystemState(_))))
                }) {
                    tabs.push((*wid, tab.id));
                }
            }
        }
        let mut windows = Vec::new();
        for (wid, tab) in tabs {
            if self.refresh_native_tab_indicators(wid, tab) && !windows.contains(&wid) {
                windows.push(wid);
            }
        }
        let changed = !windows.is_empty();
        self.refresh_tab_chrome_windows(windows);
        changed
    }

    /// A fresh click, text/key input or scroll in this app stops automatic
    /// application, even when it does not change a control's value.
    pub(crate) fn interact_system_state(&mut self, wid: crate::WindowId) {
        let ours = self.active_native_view(wid).is_some_and(|(instance, _)| {
            matches!(
                self.native_runtime.app(instance),
                Some(NativeApp::SystemState(_))
            )
        });
        if ours && self.native_runtime.system_state_interact() {
            self.system_state_host.next_tick = None;
        }
    }

    pub(crate) fn tick_system_state(&mut self, now: Instant) {
        self.resolve_withdrawn_system_state_offer(now);
        if self.resolve_system_state_cancellations() {
            self.system_state_host.action_refusal = Some(SetupStatusRow {
                name: "Cancel".into(),
                status: "failed".into(),
                detail: "Cancel was not saved. Try again.".into(),
            });
            self.publish_system_state(self.system_state_host.latest_status.clone());
        }
        self.resolve_system_state_apply();
        if self
            .system_state_host
            .next_tick
            .is_some_and(|next| next > now)
        {
            return;
        }
        let Some(instance) = self.native_runtime.instance_by_kind(AppKind::SystemState) else {
            if let Some(request) = self.system_state_host.pending.take() {
                request.finish(WorkstationSetupDecision::Unavailable);
            }
            self.system_state_host.next_tick = None;
            self.poll_system_state_status(now);
            return;
        };
        let has_view = self.view_store.iter().any(|(_, view)| {
            matches!(view, crate::tab_model::View::Native(native) if native.instance == instance)
        });
        if !has_view {
            if let Some(request) = self.system_state_host.pending.take() {
                request.finish(WorkstationSetupDecision::Unavailable);
            }
            self.system_state_host.next_tick = None;
            self.poll_system_state_status(now);
            return;
        }
        self.system_state_host.next_tick = Some(now + Duration::from_millis(250));
        let active = self.system_state_countdown_active(instance);
        let changed = self.native_runtime.tick_system_state(now, active);
        loop {
            let action = self.native_runtime.take_system_state_action();
            let Some(action) = action else { break };
            self.run_system_state_action(action);
        }
        if !self.system_state_host.status_in_flight {
            self.system_state_host.next_status.get_or_insert(now);
        }
        self.poll_system_state_status(now);
        let indicators_changed = self.refresh_system_state_tab_indicators();
        if changed || indicators_changed {
            self.request_redraw_all_windows();
        }
    }

    fn resolve_withdrawn_system_state_offer(&mut self, now: Instant) {
        if !self
            .system_state_host
            .pending
            .as_ref()
            .is_some_and(|request| !request.pending())
        {
            return;
        }
        self.system_state_host.pending = None;
        self.recheck_initial_setup_permissions();
        self.system_state_host.generation = self.system_state_host.generation.wrapping_add(1);
        self.system_state_host.status_in_flight = false;
        self.system_state_host.next_status = Some(now);
        self.system_state_host.next_tick = None;
    }

    fn system_state_countdown_active(
        &mut self,
        instance: crate::native_app::AppInstanceId,
    ) -> bool {
        let offer_presented = self.system_state_host.offer_presented;
        let active = self.windows.iter().any(|(wid, window)| {
            crate::os_window_focused(window)
                && window.overlay.is_none()
                && self
                    .active_native_view(*wid)
                    .is_some_and(|(current, view)| {
                        current == instance
                            && self.active_visible_leaf_plan(*wid).is_some_and(|plan| {
                                plan.leaf(view).is_some()
                                    && (offer_presented
                                        || self
                                            .retained_native_leaf_artifact_from_plan(
                                                *wid, view, true, &plan,
                                            )
                                            .is_some())
                            })
                    })
        });
        if active {
            self.system_state_host.offer_presented = true;
        }
        active
    }

    fn poll_system_state_status(&mut self, now: Instant) {
        if !self.system_state_host.status_in_flight
            && self
                .system_state_host
                .next_status
                .is_some_and(|next| next <= now)
        {
            let Some(handlers) = HANDLERS.lock().ok().and_then(|handlers| *handlers) else {
                self.system_state_host.next_status = None;
                return;
            };
            self.system_state_host.status_in_flight = true;
            self.system_state_host.next_status = None;
            let generation = self.system_state_host.generation;
            let started = std::thread::Builder::new()
                .name("aterm-setup-status".into())
                .spawn(move || {
                    crate::workstation_setup::send_setup_wake(Wake::InitialSetupStatus {
                        generation: Some(generation),
                        status: (handlers.status)(),
                    });
                });
            if started.is_err() {
                self.system_state_host.status_in_flight = false;
                self.system_state_host.next_status = Some(now + Duration::from_secs(1));
            }
        }
    }

    fn run_system_state_action(&mut self, action: SystemStateAction) {
        let handlers = HANDLERS.lock().ok().and_then(|handlers| *handlers);
        self.run_system_state_action_with_handlers(action, handlers);
    }

    /// Hands explicit or close-triggered Cancel to its coordinator on view close.
    pub(crate) fn finish_system_state_close(
        &mut self,
        instance: crate::native_app::AppInstanceId,
        scope: crate::native_app::CloseScope,
    ) {
        let cancel = self.native_runtime.take_system_state_cancel(instance)
            || (matches!(scope, crate::native_app::CloseScope::View | crate::native_app::CloseScope::Tab)
                && self.system_state_host.pending.as_ref().is_some_and(|request| {
                    matches!(self.native_runtime.app(instance), Some(NativeApp::SystemState(state)) if state.offered_request_matches(request))
                }));
        if cancel {
            self.run_system_state_action(SystemStateAction::Cancel);
        }
        self.native_runtime
            .finish_system_state_close(instance, scope);
    }

    fn run_system_state_action_with_handlers(
        &mut self,
        action: SystemStateAction,
        handlers: Option<InitialSetupHandlers>,
    ) {
        let action_name = match &action {
            SystemStateAction::Apply(_) => "Apply",
            SystemStateAction::Cancel => "Cancel",
            SystemStateAction::Check(_) => "Check",
            SystemStateAction::Restore => "Restore preferences",
        };
        if !matches!(&action, SystemStateAction::Check(_)) {
            if let Some(instance) = self.native_runtime.instance_by_kind(AppKind::SystemState) {
                self.retire_native_review(instance);
            }
            self.hold_initial_setup_permissions();
            self.system_state_host.pending_apply = None;
        }
        self.system_state_host.action_refusal = None;
        self.system_state_host.generation = self.system_state_host.generation.wrapping_add(1);
        self.system_state_host.status_in_flight = false;
        let started = match action {
            SystemStateAction::Apply(mut answer) => {
                let WorkstationSetupDecision::Begin(plan) = &answer.decision else {
                    return self.finish_system_state_action(false, action_name);
                };
                let plan = plan.clone();
                let Some(instance) = self.native_runtime.instance_by_kind(AppKind::SystemState)
                else {
                    return;
                };
                let (receipt, receiver) = InitialSetupApplyReceipt::new();
                self.system_state_host.pending_apply = Some(PendingApply {
                    instance,
                    plan: plan.clone(),
                    answer: receiver,
                });
                let started = if let Some(request) = self.system_state_host.pending.take() {
                    answer.receipt = Some(receipt);
                    request.finish_answer(answer)
                } else if let Some(handlers) = handlers {
                    (handlers.apply)(plan, answer.vault_typed, answer.password, receipt)
                } else {
                    false
                };
                if !started {
                    self.system_state_host.pending_apply = None;
                }
                if started {
                    self.publish_system_state(self.system_state_host.latest_status.clone());
                }
                started
            }
            SystemStateAction::Cancel => self.start_system_state_cancel(handlers),
            SystemStateAction::Check(plan) => {
                handlers.is_some_and(|handlers| (handlers.check)(plan))
            }
            SystemStateAction::Restore => {
                if let Some(request) = self.system_state_host.pending.take() {
                    request.finish(WorkstationSetupDecision::Unavailable);
                    self.native_runtime.tick_system_state(Instant::now(), false);
                    self.refresh_system_state_tab_indicators();
                }
                handlers.is_some_and(|handlers| (handlers.restore)())
            }
        };
        self.finish_system_state_action(started, action_name);
    }

    fn start_system_state_cancel(&mut self, handlers: Option<InitialSetupHandlers>) -> bool {
        let (receipt, receiver) = InitialSetupCancelReceipt::new();
        let started = if let Some(request) = self.system_state_host.pending.take() {
            request.finish_answer(crate::workstation_setup::SetupAnswer {
                decision: WorkstationSetupDecision::Cancel,
                password: None,
                receipt: None,
                cancel_receipt: Some(receipt),
                vault_typed: false,
            })
        } else {
            handlers.is_some_and(|handlers| (handlers.cancel)(receipt))
        };
        if started {
            self.system_state_host.cancel_failed = false;
            self.system_state_host
                .cancel_receipts
                .push(CancelCompletion::new(receiver));
        }
        started
    }

    fn finish_system_state_action(&mut self, started: bool, action_name: &str) {
        if !started {
            self.system_state_host.action_refusal = Some(SetupStatusRow {
                name: action_name.into(),
                status: "not started".into(),
                detail: "Try again.".into(),
            });
            self.publish_system_state(self.system_state_host.latest_status.clone());
        }
        self.system_state_host.next_status = Some(Instant::now());
        self.request_redraw_all_windows();
    }

    pub(crate) fn resolve_system_state_apply(&mut self) {
        let Some(pending) = self.system_state_host.pending_apply.as_ref() else {
            return;
        };
        let accepted = match pending.answer.try_recv() {
            Ok(accepted) => accepted,
            Err(TryRecvError::Empty) => return,
            Err(TryRecvError::Disconnected) => false,
        };
        let pending = self.system_state_host.pending_apply.take().unwrap();
        self.system_state_host.generation = self.system_state_host.generation.wrapping_add(1);
        self.system_state_host.status_in_flight = false;
        if accepted {
            self.accepted_initial_setup_apply();
            if self.system_state_instance_has_view(pending.instance) {
                self.native_runtime.accept_system_state_apply(pending.plan);
                self.refresh_system_state_tab_indicators();
            }
        }
        self.finish_system_state_action(accepted, "Apply");
        if accepted {
            self.publish_system_state(self.system_state_host.latest_status.clone());
        }
    }

    fn system_state_instance_has_view(&self, instance: crate::native_app::AppInstanceId) -> bool {
        self.native_runtime
            .app(instance)
            .is_some_and(|app| app.kind() == AppKind::SystemState)
            && self.view_store.iter().any(|(_, view)| {
                matches!(view, crate::tab_model::View::Native(native) if native.instance == instance)
            })
    }
}

fn wait_for_cancellations(receipts: Vec<CancelCompletion>) -> Result<(), &'static str> {
    wait_for_cancellations_until(receipts, Instant::now() + Duration::from_secs(5))
}

fn wait_for_cancellations_until(
    receipts: Vec<CancelCompletion>,
    deadline: Instant,
) -> Result<(), &'static str> {
    let mut failed = false;
    for receipt in receipts {
        if let Some(complete) = receipt.poll() {
            failed |= !complete;
            continue;
        }
        let Some(remaining) = deadline
            .checked_duration_since(Instant::now())
            .filter(|remaining| !remaining.is_zero())
        else {
            failed = true;
            break;
        };
        match receipt.answer.recv_timeout(remaining) {
            Ok(()) => {}
            Err(mpsc::RecvTimeoutError::Timeout) => {
                aterm_log::warn!("Cancel was not saved before the shutdown deadline; quitting");
                failed = true;
                break;
            }
            Err(mpsc::RecvTimeoutError::Disconnected) => failed = true,
        }
    }
    if failed {
        Err("Cancel was not saved before shutdown")
    } else {
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::input::InputEvent;
    use crate::native_app::{ActionInvocation, AppEvent, SemanticInput};
    use crate::native_ui::{ActionId, UiKey};
    use crate::workstation_setup::SetupAnswer;
    use aterm_types::keyboard::{Key, KeyEventType, Modifiers, NamedKey};
    use std::sync::mpsc;

    thread_local! {
        static APPLY_RECEIPTS: std::cell::RefCell<Vec<InitialSetupApplyReceipt>> = const {
            std::cell::RefCell::new(Vec::new())
        };
        static CANCEL_RECEIPTS: std::cell::RefCell<Vec<InitialSetupCancelReceipt>> = const {
            std::cell::RefCell::new(Vec::new())
        };
    }

    fn queued_apply_handlers() -> InitialSetupHandlers {
        InitialSetupHandlers {
            apply: |_, _, _, receipt| {
                APPLY_RECEIPTS.with(|receipts| receipts.borrow_mut().push(receipt));
                true
            },
            check: |_| false,
            restore: || false,
            cancel: |_| false,
            status: SetupStatus::default,
        }
    }

    #[test]
    fn linked_system_state_tab_flags_follow_status_and_acceptance_without_grid_locks() {
        use crate::tab_model::{SplitAxis, TabIndicators};
        let mut app = App::headless_for_test();
        let first_window = crate::WindowId(0);
        assert!(app.open_system_state_tab());
        let (instance, first_view) = app.active_native_view(first_window).unwrap();
        let first_tab = app.windows[&first_window].tab_set.active_id().unwrap();
        edit_review(
            &mut app,
            first_window,
            first_view,
            "http://reviewed.local/v1/team",
        );
        app.stage_system_state_tab(first_window).unwrap();
        let second_window = app.detach_active_tab_logical().unwrap();
        let second_tab = app.windows[&second_window].tab_set.active_id().unwrap();
        let (session, _) = app.split_active_with_stub_terminal(second_window, SplitAxis::Vertical);
        app.switch_tab_in(first_window, 0);
        assert_ne!(
            app.windows[&first_window].tab_set.active_id(),
            Some(first_tab)
        );
        let tabs = [(first_window, first_tab), (second_window, second_tab)];
        let assert_flags = |app: &App, expected: TabIndicators| {
            for (wid, tab) in tabs {
                assert_eq!(
                    app.windows[&wid]
                        .tab_set
                        .get(tab)
                        .unwrap()
                        .presentation
                        .indicators,
                    expected
                );
            }
        };

        let term = app.pool.get(session).unwrap().term.clone();
        let (ready_tx, ready_rx) = std::sync::mpsc::channel();
        let (release_tx, release_rx) = std::sync::mpsc::channel();
        let parser_holder = std::thread::spawn(move || {
            let _guard = term.lock().unwrap();
            if ready_tx.send(()).is_err() {
                return false;
            }
            matches!(
                release_rx.recv_timeout(Duration::from_secs(60)),
                Err(std::sync::mpsc::RecvTimeoutError::Timeout)
            )
        });
        ready_rx
            .recv_timeout(Duration::from_secs(60))
            .expect("parser lock holder must become ready");
        app.publish_system_state(SetupStatus {
            busy: true,
            ..SetupStatus::default()
        });
        let busy = TabIndicators {
            dirty: true,
            busy: true,
            ..TabIndicators::default()
        };
        assert_flags(&app, busy);
        app.refresh_tab_status_indicators(second_window, second_tab);
        assert_flags(&app, busy);

        app.publish_system_state(SetupStatus {
            finished: true,
            rows: vec![SetupStatusRow {
                name: "Check".into(),
                status: "failed".into(),
                detail: "injected fresh failure".into(),
            }],
            ..SetupStatus::default()
        });
        let failed = TabIndicators {
            dirty: true,
            attention: true,
            ..TabIndicators::default()
        };
        assert_flags(&app, failed);
        app.refresh_tab_status_indicators(second_window, second_tab);
        assert_flags(&app, failed);

        app.native_runtime
            .dispatch(
                instance,
                first_view,
                AppEvent::Action(ActionInvocation {
                    id: ActionId::new("system-state/cancel"),
                    value: None,
                }),
            )
            .unwrap();
        let cancel = app.native_runtime.take_system_state_action().unwrap();
        app.run_system_state_action_with_handlers(
            cancel,
            Some(InitialSetupHandlers {
                cancel: |receipt| {
                    receipt.complete();
                    true
                },
                ..queued_apply_handlers()
            }),
        );
        let clean = SetupStatus {
            finished: true,
            ..SetupStatus::default()
        };
        app.publish_system_state(clean.clone());
        assert_flags(&app, TabIndicators::default());

        app.native_runtime
            .dispatch(
                instance,
                first_view,
                AppEvent::Action(ActionInvocation {
                    id: ActionId::new("system-state/url"),
                    value: Some(SemanticInput::Text(
                        "http://new-review.local/v1/team".into(),
                    )),
                }),
            )
            .unwrap();
        app.publish_system_state(clean);
        assert_flags(
            &app,
            TabIndicators {
                dirty: true,
                ..TabIndicators::default()
            },
        );
        app.native_runtime
            .dispatch(
                instance,
                first_view,
                AppEvent::Action(ActionInvocation {
                    id: ActionId::new("system-state/apply"),
                    value: None,
                }),
            )
            .unwrap();
        let apply = app.native_runtime.take_system_state_action().unwrap();
        app.run_system_state_action_with_handlers(
            apply,
            Some(InitialSetupHandlers {
                apply: |_, _, _, receipt| {
                    receipt.accept();
                    true
                },
                ..queued_apply_handlers()
            }),
        );
        assert_flags(&app, busy);
        app.resolve_system_state_apply();
        assert_flags(&app, TabIndicators::default());
        let _ = release_tx.send(());
        assert!(
            !parser_holder.join().unwrap(),
            "status publication blocked on the parser lock"
        );
        assert!(app.structural_invariants_ok());
    }

    fn reviewed_apply() -> (
        App,
        crate::WindowId,
        crate::tab_model::ViewId,
        InitialSetupApplyReceipt,
    ) {
        let mut app = App::headless_for_test();
        let wid = crate::WindowId(0);
        let window = app.windows.get_mut(&wid).unwrap();
        window.rows = 90;
        window.cols = 160;
        assert!(app.open_system_state_tab());
        let (_, view) = app.active_native_view(wid).unwrap();
        app.publish_system_state(SetupStatus {
            plan: Some(Plan::default()),
            ..SetupStatus::default()
        });
        edit_review(&mut app, wid, view, "http://reviewed.local:7484/v1/team");
        let receipt = queue_review(&mut app, wid, view);
        (app, wid, view, receipt)
    }

    fn edit_review(app: &mut App, wid: crate::WindowId, view: crate::tab_model::ViewId, url: &str) {
        let dock = format!(
            "system-state/setting/{}",
            aterm_types::macos_setup::Setting::ALL
                .iter()
                .position(|setting| *setting == aterm_types::macos_setup::Setting::Dock)
                .unwrap()
        );
        for (key, value) in [
            (dock.as_str(), SemanticInput::Bool(false)),
            ("system-state/url", SemanticInput::Text(url.into())),
            (
                "system-state/password",
                SemanticInput::Text("inert-password".into()),
            ),
        ] {
            app.dispatch_native_view_event(
                wid,
                view,
                AppEvent::Action(ActionInvocation {
                    id: ActionId::new(key),
                    value: Some(value),
                }),
            )
            .unwrap();
        }
    }

    fn queue_review(
        app: &mut App,
        wid: crate::WindowId,
        view: crate::tab_model::ViewId,
    ) -> InitialSetupApplyReceipt {
        app.dispatch_native_view_event(
            wid,
            view,
            AppEvent::Action(ActionInvocation {
                id: ActionId::new("system-state/apply"),
                value: None,
            }),
        )
        .unwrap();
        let action = app.native_runtime.take_system_state_action().unwrap();
        app.run_system_state_action_with_handlers(action, Some(queued_apply_handlers()));
        APPLY_RECEIPTS.with(|receipts| receipts.borrow_mut().pop().unwrap())
    }

    fn assert_review(
        app: &mut App,
        wid: crate::WindowId,
        view: crate::tab_model::ViewId,
        url: &str,
        busy: bool,
    ) {
        let (instance, _) = app.active_native_view(wid).unwrap();
        let indicators = app
            .native_runtime
            .presentation(instance, view)
            .unwrap()
            .indicators;
        assert!(indicators.dirty);
        assert_eq!(indicators.busy, busy);
        let Some(NativeApp::SystemState(state)) = app.native_runtime.app(instance) else {
            panic!("system state")
        };
        assert_eq!(
            state.field_projection("system-state/url").unwrap().text,
            url
        );
        assert_eq!(
            state
                .field_projection("system-state/password")
                .unwrap()
                .text,
            "•".repeat("inert-password".len())
        );
        assert!(app.commit_glassless_native_frame(wid));
        let dock = format!(
            "system-state/setting/{}",
            aterm_types::macos_setup::Setting::ALL
                .iter()
                .position(|setting| *setting == aterm_types::macos_setup::Setting::Dock)
                .unwrap()
        );
        assert_eq!(
            app.windows[&wid].leaf_render_cache[&view]
                .native
                .as_ref()
                .unwrap()
                .compiled
                .semantic(&UiKey::new(dock))
                .unwrap()
                .value,
            crate::native_ui::SemanticValue::Bool(false)
        );
    }

    fn vault_read() -> Plan {
        use aterm_types::macos_setup::Setting;
        let mut read = Plan::default();
        read.selection.set(Setting::Secrets, false);
        read.selection.set(Setting::Repositories, false);
        read.vault_url = "https://vault.example/v1/team".into();
        read
    }

    fn reviewed(app: &App) -> Plan {
        let instance = app
            .native_runtime
            .instance_by_kind(AppKind::SystemState)
            .unwrap();
        let Some(NativeApp::SystemState(state)) = app.native_runtime.app(instance) else {
            panic!("system state")
        };
        state.reviewed_plan()
    }

    const SET: &str = "Opened System State with the vault row on and the Mac settings rows off";
    const CHECK: &str = "Opened System State; check its vault and Mac settings rows before Apply";

    fn performed(feedback: &str) -> crate::native_app::MessageActOutcome {
        crate::native_app::MessageActOutcome::Performed {
            feedback: feedback.into(),
        }
    }

    fn assert_synced(app: &mut App, plan: &Plan) {
        use aterm_types::macos_setup::Setting;
        for setting in Setting::ALL {
            let on = !setting.is_workstation() && setting != Setting::Repositories;
            assert_eq!(plan.selection.contains(setting), on, "{setting:?}");
        }
        assert_eq!(plan.vault_url, vault_read().vault_url);
        assert!(
            app.native_runtime.take_system_state_action().is_none()
                && app.system_state_host.pending_apply.is_none(),
            "never an Apply"
        );
    }

    fn press_vault_line(
        app: &mut App,
        wid: crate::WindowId,
    ) -> crate::native_app::MessageActOutcome {
        let entry = crate::secrets_join::failed_entry("the vault refused");
        app.except(&entry);
        let line = app
            .messages
            .digest()
            .entries
            .into_iter()
            .find(|line| line.key == entry.key())
            .unwrap();
        assert_eq!(
            line.actions.first(),
            Some(&aterm_messages::Intent::ReviewVaultSync)
        );
        app.perform_message_act(wid, line.id.raw(), 0)
    }

    #[test]
    fn the_vault_lines_press_opens_system_state_set_for_the_sync_in_its_window() {
        let mut app = App::headless_for_test();
        let first = crate::WindowId(0);
        assert!(app.open_settings_tab(crate::native_settings::SettingsRoute::About));
        let second = app.detach_active_tab_logical().unwrap();
        app.frontmost_window = Some(first);
        let first_tab = app.windows[&first].tab_set.active_id();
        app.publish_system_state(SetupStatus {
            plan: Some(vault_read()),
            ..SetupStatus::default()
        });
        assert_eq!(
            app.native_runtime.instance_by_kind(AppKind::SystemState),
            None
        );

        assert_eq!(press_vault_line(&mut app, second), performed(SET));
        let (instance, _) = app.active_native_view(second).unwrap();
        assert_eq!(
            app.native_runtime.app(instance).map(NativeApp::kind),
            Some(AppKind::SystemState)
        );
        assert_eq!(app.windows[&first].tab_set.active_id(), first_tab);
        let plan = reviewed(&app);
        assert_synced(&mut app, &plan);
        assert_eq!(press_vault_line(&mut app, second), performed(SET), "again");
        app.publish_system_state(SetupStatus {
            plan: Some(vault_read()),
            ..SetupStatus::default()
        });
        assert_eq!(reviewed(&app), plan, "a later read keeps the preset");
    }

    #[test]
    fn the_vault_lines_press_leaves_the_line_up() {
        let mut app = App::headless_for_test();
        let entry = crate::secrets_join::failed_entry("the vault refused");
        let up = |app: &App| {
            app.messages
                .digest()
                .entries
                .iter()
                .any(|line| line.key == entry.key())
        };
        let _ = press_vault_line(&mut app, crate::WindowId(0));
        assert!(up(&app), "pressed");
        app.except(&entry);
        assert!(up(&app), "raised again");
    }

    #[test]
    fn the_vault_lines_press_before_setups_first_read_presets_that_read() {
        let mut app = App::headless_for_test();
        assert_eq!(
            press_vault_line(&mut app, crate::WindowId(0)),
            performed(CHECK)
        );
        assert_eq!(reviewed(&app), Plan::default(), "no read yet");
        app.publish_system_state(SetupStatus {
            plan: Some(vault_read()),
            ..SetupStatus::default()
        });
        let plan = reviewed(&app);
        assert_synced(&mut app, &plan);
    }

    #[test]
    fn the_vault_lines_press_only_shows_an_edited_review() {
        let mut app = App::headless_for_test();
        let wid = crate::WindowId(0);
        let window = app.windows.get_mut(&wid).unwrap();
        window.rows = 90;
        window.cols = 160;
        assert!(app.open_system_state_tab());
        let review = app.active_native_view(wid).unwrap();
        app.publish_system_state(SetupStatus {
            plan: Some(vault_read()),
            ..SetupStatus::default()
        });
        edit_review(&mut app, wid, review.1, "http://reviewed.local/v1/team");
        let edited = reviewed(&app);
        assert!(app.open_settings_tab(crate::native_settings::SettingsRoute::About));
        assert_ne!(app.active_native_view(wid), Some(review));

        assert_eq!(press_vault_line(&mut app, wid), performed(CHECK));
        assert_eq!(app.active_native_view(wid), Some(review));
        assert_eq!(reviewed(&app), edited);
        assert_review(
            &mut app,
            wid,
            review.1,
            "http://reviewed.local/v1/team",
            false,
        );
    }

    #[test]
    fn the_vault_lines_press_changes_nothing_held_by_a_run_or_an_offer() {
        let read = || SetupStatus {
            plan: Some(vault_read()),
            ..SetupStatus::default()
        };
        let wid = crate::WindowId(0);
        let mut app = App::headless_for_test();
        assert!(app.open_system_state_tab());
        app.publish_system_state(read());
        app.publish_system_state(SetupStatus {
            busy: true,
            ..read()
        });
        assert_eq!(press_vault_line(&mut app, wid), performed(CHECK));
        assert_eq!(reviewed(&app), vault_read(), "setup's own pass");

        let mut app = App::headless_for_test();
        assert!(app.open_system_state_tab());
        let (_, view) = app.active_native_view(wid).unwrap();
        app.publish_system_state(read());
        let _receipt = queue_review(&mut app, wid, view);
        assert!(app.system_state_host.pending_apply.is_some());
        assert_eq!(press_vault_line(&mut app, wid), performed(CHECK));
        assert_eq!(
            reviewed(&app),
            vault_read(),
            "an Apply awaiting its receipt"
        );

        let mut app = App::headless_for_test();
        assert!(app.open_system_state_tab());
        let (send, _receive) = mpsc::channel();
        let request = Request::new(vault_read(), send);
        app.attach_system_state_offer(request.clone());
        app.publish_system_state(SetupStatus {
            plan: Some(Plan::default()),
            ..SetupStatus::default()
        });
        let start = Instant::now();
        app.native_runtime.tick_system_state(start, true);
        assert!(app.open_settings_tab(crate::native_settings::SettingsRoute::About));
        assert_eq!(press_vault_line(&mut app, wid), performed(CHECK));
        assert_eq!(reviewed(&app), vault_read(), "a first-run offer");
        app.native_runtime
            .tick_system_state(start + Duration::from_secs(31), true);
        assert!(
            app.native_runtime.take_system_state_action().is_none(),
            "the offer waits for the person"
        );
        assert!(request.pending());
    }

    #[test]
    fn withdrawn_system_state_request_cannot_open_or_hold_a_finished_review() {
        for route in ["window", "attach", "model"] {
            let mut app = App::headless_for_test();
            let wid = crate::WindowId(0);
            app.workstation_setup =
                crate::workstation_setup::Host::for_launch(Some(|| true), false);
            if route != "window" {
                assert!(app.open_system_state_tab());
            }
            app.receive_system_state_status(
                Some(app.system_state_host.generation),
                SetupStatus {
                    permissions: Some(crate::InitialSetupPermissionPolicy {
                        revision: 5,
                        allowed: true,
                    }),
                    ..SetupStatus::default()
                },
            );
            assert!(!app.workstation_setup.suppresses_permissions());
            let active = app.windows[&wid].tab_set.active_id();
            let focus = app.windows[&wid].window_focus;
            let instance = app.native_runtime.instance_by_kind(AppKind::SystemState);
            let (send, receive) = mpsc::channel();
            let request = Request::new(Plan::default(), send);
            request.finish(WorkstationSetupDecision::Unavailable);
            assert_eq!(
                receive.try_recv().unwrap().decision,
                WorkstationSetupDecision::Unavailable
            );
            match route {
                "window" => {
                    assert!(!app.apprt.probe_preferences);
                    assert!(app.windows[&wid].os_window.is_none());
                    app.headless = false;
                    app.offer_system_state(request);
                }
                "attach" => app.attach_system_state_offer(request),
                _ => assert!(!app.native_runtime.offer_system_state(request)),
            }
            assert_eq!(app.windows[&wid].tab_set.active_id(), active);
            assert_eq!(app.windows[&wid].window_focus, focus);
            assert_eq!(
                app.native_runtime.instance_by_kind(AppKind::SystemState),
                instance
            );
            assert!(app.system_state_host.pending.is_none());
            assert!(!app.workstation_setup.suppresses_permissions());
        }
    }

    #[test]
    fn withdrawn_system_state_offer_restores_policy_only_after_a_fresh_read() {
        let policy = || SetupStatus {
            permissions: Some(crate::InitialSetupPermissionPolicy {
                revision: 5,
                allowed: true,
            }),
            ..SetupStatus::default()
        };
        let mut app = App::headless_for_test();
        app.workstation_setup = crate::workstation_setup::Host::for_launch(Some(|| true), false);
        app.receive_system_state_status(Some(app.system_state_host.generation), policy());
        assert!(!app.workstation_setup.suppresses_permissions());
        assert!(app.open_system_state_tab());
        let (send, receive) = mpsc::channel();
        let request = Request::new(Plan::default(), send);
        app.attach_system_state_offer(request.clone());
        let before_withdrawal = app.system_state_host.generation;
        app.receive_system_state_status(Some(before_withdrawal), policy());
        assert!(app.workstation_setup.suppresses_permissions());
        request.finish(WorkstationSetupDecision::Unavailable);
        assert_eq!(
            receive.try_recv().unwrap().decision,
            WorkstationSetupDecision::Unavailable
        );
        let now = Instant::now();
        app.system_state_host.next_tick = Some(now + Duration::from_secs(60));
        app.tick_system_state(now);
        assert!(app.system_state_host.pending.is_none());
        assert!(!app.native_runtime.system_state_mut().unwrap().pending());
        assert!(app.native_runtime.take_system_state_action().is_none());
        assert!(app.workstation_setup.suppresses_permissions());
        app.publish_system_state(policy());
        app.receive_system_state_status(Some(before_withdrawal), policy());
        assert!(app.workstation_setup.suppresses_permissions());
        app.receive_system_state_status(Some(app.system_state_host.generation), policy());
        assert!(
            !app.workstation_setup.suppresses_permissions(),
            "a withdrawn review cannot retain consent after a fresh allowed snapshot"
        );
        assert_ne!(app.system_state_host.generation, before_withdrawal);
    }

    #[test]
    fn withdrawn_system_state_offer_cannot_release_a_newer_local_decision() {
        let policy = || SetupStatus {
            permissions: Some(crate::InitialSetupPermissionPolicy {
                revision: 6,
                allowed: true,
            }),
            ..SetupStatus::default()
        };
        for action in ["apply", "cancel", "restore"] {
            let mut app = App::headless_for_test();
            let wid = crate::WindowId(0);
            assert!(app.open_system_state_tab());
            let (_, view) = app.active_native_view(wid).unwrap();
            let (send, receive) = mpsc::channel();
            let request = Request::new(Plan::default(), send);
            app.attach_system_state_offer(request.clone());
            let previous = app.system_state_host.generation;
            app.dispatch_native_view_event(
                wid,
                view,
                AppEvent::Action(ActionInvocation {
                    id: ActionId::new(format!("system-state/{action}")),
                    value: None,
                }),
            )
            .unwrap();
            let queued = app.native_runtime.take_system_state_action().unwrap();
            app.run_system_state_action_with_handlers(
                queued,
                Some(InitialSetupHandlers {
                    restore: || true,
                    ..queued_apply_handlers()
                }),
            );
            let mut answer = receive.try_recv().unwrap();
            let receipt = answer.receipt.take();
            if let Some(cancel) = answer.cancel_receipt.take() {
                cancel.complete();
            }
            request.finish(WorkstationSetupDecision::Unavailable);
            app.tick_system_state(Instant::now());
            app.receive_system_state_status(Some(previous), policy());
            app.receive_system_state_status(Some(app.system_state_host.generation), policy());
            assert!(app.workstation_setup.suppresses_permissions());
            if let Some(receipt) = receipt {
                assert!(app.system_state_host.pending_apply.is_some());
                let before_ack = app.system_state_host.generation;
                receipt.accept();
                app.resolve_system_state_apply();
                app.receive_system_state_status(Some(before_ack), policy());
                assert!(app.workstation_setup.suppresses_permissions());
                app.receive_system_state_status(Some(app.system_state_host.generation), policy());
                assert!(!app.workstation_setup.suppresses_permissions());
            } else {
                assert_ne!(action, "apply");
                assert!(app.system_state_host.pending_apply.is_none());
            }
        }
    }

    #[test]
    fn refused_automatic_system_state_offer_does_not_take_the_active_tab_or_focus() {
        for busy in [false, true] {
            let mut app = App::headless_for_test();
            let wid = crate::WindowId(0);
            let terminal = app.windows[&wid].tab_set.active_index().unwrap();
            assert!(app.open_system_state_tab());
            let (instance, view) = app.active_native_view(wid).unwrap();
            if busy {
                app.dispatch_native_view_event(
                    wid,
                    view,
                    AppEvent::Action(ActionInvocation {
                        id: ActionId::new("system-state/check"),
                        value: None,
                    }),
                )
                .unwrap();
            } else {
                edit_review(&mut app, wid, view, "http://reviewed.local/v1/team");
            }
            app.switch_tab_in(wid, terminal);
            let active = app.windows[&wid].tab_set.active_id();
            let focus = app.windows[&wid].window_focus;
            let snapshot = |app: &App| {
                let Some(NativeApp::SystemState(model)) = app.native_runtime.app(instance) else {
                    panic!("the manual review must remain live")
                };
                (
                    model.reviewed_plan(),
                    model.field_projection("system-state/password").unwrap(),
                    app.native_runtime
                        .presentation(instance, view)
                        .unwrap()
                        .indicators,
                    app.native_runtime
                        .view_state(view)
                        .unwrap()
                        .common()
                        .last_focus
                        .clone(),
                )
            };
            let reviewed = snapshot(&app);
            assert_eq!(reviewed.2.busy, busy);
            assert_eq!(reviewed.2.dirty, !busy);

            assert!(!app.apprt.probe_preferences);
            assert!(app.windows[&wid].os_window.is_none());
            app.headless = false;
            let (send, receive) = mpsc::channel();
            app.offer_system_state(Request::new(Plan::default(), send));
            assert_eq!(
                receive.try_recv().unwrap().decision,
                WorkstationSetupDecision::Unavailable
            );
            assert_eq!(app.windows[&wid].tab_set.active_id(), active);
            assert_eq!(app.windows[&wid].window_focus, focus);
            assert!(app.active_native_view(wid).is_none());
            assert_eq!(snapshot(&app), reviewed);
            assert!(app.system_state_host.pending.is_none());
            assert_eq!(
                matches!(
                    app.native_runtime.take_system_state_action(),
                    Some(SystemStateAction::Check(_))
                ),
                busy
            );
        }

        let mut app = App::headless_for_test();
        let wid = crate::WindowId(0);
        assert!(!app.apprt.probe_preferences);
        assert!(app.windows[&wid].os_window.is_none());
        app.headless = false;
        let (send, receive) = mpsc::channel();
        let request = Request::new(Plan::default(), send);
        app.offer_system_state(request.clone());
        let (instance, _) = app.active_native_view(wid).unwrap();
        assert!(matches!(
            app.native_runtime.app(instance),
            Some(NativeApp::SystemState(_))
        ));
        assert!(request.pending());
        assert!(matches!(receive.try_recv(), Err(mpsc::TryRecvError::Empty)));
        assert!(
            app.system_state_host
                .pending
                .as_ref()
                .unwrap()
                .same_offer(&request)
        );
    }

    #[test]
    fn late_automatic_offer_cannot_replace_an_unchanged_apply_awaiting_its_receipt() {
        let mut app = App::headless_for_test();
        let wid = crate::WindowId(0);
        assert!(app.open_system_state_tab());
        let (instance, view) = app.active_native_view(wid).unwrap();
        let receipt = queue_review(&mut app, wid, view);
        let indicators = app
            .native_runtime
            .presentation(instance, view)
            .unwrap()
            .indicators;
        assert!(
            !indicators.dirty && indicators.busy,
            "an unchanged Apply still owns admission"
        );

        let (send, receive) = mpsc::channel();
        let request = Request::new(Plan::default(), send);
        app.attach_system_state_offer(request.clone());
        assert_eq!(
            receive.try_recv().unwrap().decision,
            WorkstationSetupDecision::Unavailable
        );
        assert!(!request.pending());
        assert!(app.system_state_host.pending.is_none());
        assert!(app.system_state_host.pending_apply.is_some());
        assert!(
            app.native_runtime
                .presentation(instance, view)
                .unwrap()
                .indicators
                .busy
        );

        receipt.accept();
        app.resolve_system_state_apply();
        assert!(app.system_state_host.pending_apply.is_none());
        let indicators = app
            .native_runtime
            .presentation(instance, view)
            .unwrap()
            .indicators;
        assert!(!indicators.dirty && !indicators.busy);
    }

    #[test]
    fn late_automatic_offer_cannot_replace_an_unchanged_check_already_queued() {
        let mut app = App::headless_for_test();
        let wid = crate::WindowId(0);
        assert!(app.open_system_state_tab());
        let (instance, view) = app.active_native_view(wid).unwrap();
        app.dispatch_native_view_event(
            wid,
            view,
            AppEvent::Action(ActionInvocation {
                id: ActionId::new("system-state/check"),
                value: None,
            }),
        )
        .unwrap();
        let (send, receive) = mpsc::channel();
        app.attach_system_state_offer(Request::new(Plan::default(), send));
        assert_eq!(
            receive.try_recv().unwrap().decision,
            WorkstationSetupDecision::Unavailable
        );
        assert!(matches!(
            app.native_runtime.take_system_state_action(),
            Some(SystemStateAction::Check(_))
        ));
        let indicators = app
            .native_runtime
            .presentation(instance, view)
            .unwrap()
            .indicators;
        assert!(!indicators.dirty && indicators.busy);
        assert!(app.system_state_host.pending.is_none());
    }

    #[test]
    fn manual_cancel_waits_for_durability_and_reports_a_worker_that_ends_unsaved() {
        fn start_cancel(receipt: InitialSetupCancelReceipt) -> bool {
            CANCEL_RECEIPTS.with(|receipts| receipts.borrow_mut().push(receipt));
            true
        }
        for saved in [false, true] {
            let mut app = App::headless_for_test();
            let wid = crate::WindowId(0);
            assert!(app.open_system_state_tab());
            let (_, view) = app.active_native_view(wid).unwrap();
            app.dispatch_native_view_event(
                wid,
                view,
                AppEvent::Action(ActionInvocation {
                    id: ActionId::new("system-state/cancel"),
                    value: None,
                }),
            )
            .unwrap();
            let action = app.native_runtime.take_system_state_action().unwrap();
            app.run_system_state_action_with_handlers(
                action,
                Some(InitialSetupHandlers {
                    cancel: start_cancel,
                    ..queued_apply_handlers()
                }),
            );
            app.close_active_native_tab(wid).unwrap();
            assert!(app.system_state_cancel_pending());
            assert!(app.open_system_state_tab());
            let refuse_offer = |app: &mut App| {
                let (send, receive) = mpsc::channel();
                app.attach_system_state_offer(Request::new(Plan::default(), send));
                assert_eq!(
                    receive.try_recv().unwrap().decision,
                    WorkstationSetupDecision::Unavailable
                );
                assert!(app.system_state_host.pending.is_none());
            };
            refuse_offer(&mut app);
            let receipt = CANCEL_RECEIPTS.with(|receipts| receipts.borrow_mut().pop().unwrap());
            let fresh = if saved {
                receipt.complete();
                assert!(!app.system_state_cancel_pending());
                let (send, receive) = mpsc::channel();
                app.attach_system_state_offer(Request::new(Plan::default(), send));
                assert!(app.system_state_host.pending.is_some());
                assert!(matches!(receive.try_recv(), Err(mpsc::TryRecvError::Empty)));
                Some(receive)
            } else {
                drop(receipt);
                app.tick_system_state(Instant::now());
                let failure = app.system_state_host.action_refusal.as_ref().unwrap();
                assert_eq!(failure.name, "Cancel");
                assert_eq!(failure.status, "failed");
                assert!(app.system_state_cancel_pending());
                refuse_offer(&mut app);
                None
            };
            assert_eq!(app.finish_system_state_shutdown().is_ok(), saved);
            if let Some(receive) = fresh {
                assert_eq!(
                    receive.try_recv().unwrap().decision,
                    WorkstationSetupDecision::Unavailable
                );
            }
        }
    }

    #[test]
    fn cancel_receipts_hold_relaunch_after_the_form_closes_until_saved() {
        use crate::native_app::CloseScope;
        for explicit in [false, true] {
            let mut app = App::headless_for_test();
            let wid = crate::WindowId(0);
            assert!(app.open_system_state_tab());
            let (_, view) = app.active_native_view(wid).unwrap();
            let (send, receive) = mpsc::channel();
            app.attach_system_state_offer(Request::new(Plan::default(), send));
            if explicit {
                app.dispatch_native_view_event(
                    wid,
                    view,
                    AppEvent::Action(ActionInvocation {
                        id: ActionId::new("system-state/cancel"),
                        value: None,
                    }),
                )
                .unwrap();
            }
            app.close_active_native_tab(wid).unwrap();
            let mut answer = receive.try_recv().unwrap();
            assert_eq!(answer.decision, WorkstationSetupDecision::Cancel);
            let receipt = answer.cancel_receipt.take().unwrap();
            for scope in [CloseScope::Relaunch, CloseScope::CarriedRelaunch] {
                assert!(
                    !app.prepare_all_native_shutdown(
                        scope,
                        crate::app_tabs::ClosePreflightVisibility::Quiet
                    )
                    .unwrap()
                );
            }
            receipt.complete();
            for _ in 0..2 {
                assert!(!app.system_state_cancel_pending());
            }
            assert!(
                app.prepare_all_native_shutdown(
                    CloseScope::Relaunch,
                    crate::app_tabs::ClosePreflightVisibility::Quiet
                )
                .unwrap()
            );
            assert!(app.finish_system_state_shutdown().is_ok());
        }
    }

    #[test]
    fn quit_drains_a_queued_cancel_without_another_event_loop_tick() {
        let mut app = App::headless_for_test();
        let wid = crate::WindowId(0);
        assert!(app.open_system_state_tab());
        let (_, view) = app.active_native_view(wid).unwrap();
        let (send, receive) = mpsc::channel();
        app.attach_system_state_offer(Request::new(Plan::default(), send));
        app.dispatch_native_view_event(
            wid,
            view,
            AppEvent::Action(ActionInvocation {
                id: ActionId::new("system-state/cancel"),
                value: None,
            }),
        )
        .unwrap();
        let generation = app.system_state_host.generation;
        let review = app
            .native_runtime
            .view_state(view)
            .unwrap()
            .common()
            .review_revision;
        let worker = std::thread::spawn(move || {
            let mut answer = receive.recv_timeout(Duration::from_secs(60)).unwrap();
            assert_eq!(answer.decision, WorkstationSetupDecision::Cancel);
            answer.cancel_receipt.take().unwrap().complete();
        });
        assert!(app.finish_system_state_shutdown().is_ok());
        worker.join().unwrap();
        assert!(app.system_state_host.pending.is_none());
        assert!(app.native_runtime.take_system_state_action().is_none());
        assert_eq!(app.system_state_host.generation, generation);
        assert_eq!(
            app.native_runtime
                .view_state(view)
                .unwrap()
                .common()
                .review_revision,
            review
        );
        assert!(app.system_state_host.action_refusal.is_none());
    }

    #[test]
    fn quit_reports_a_cancel_that_could_not_start_without_changing_presentation() {
        let mut app = App::headless_for_test();
        let wid = crate::WindowId(0);
        assert!(app.open_system_state_tab());
        let (_, view) = app.active_native_view(wid).unwrap();
        app.dispatch_native_view_event(
            wid,
            view,
            AppEvent::Action(ActionInvocation {
                id: ActionId::new("system-state/cancel"),
                value: None,
            }),
        )
        .unwrap();
        let generation = app.system_state_host.generation;
        let review = app
            .native_runtime
            .view_state(view)
            .unwrap()
            .common()
            .review_revision;
        assert!(app.finish_system_state_shutdown().is_err());
        assert!(app.system_state_host.cancel_failed);
        assert_eq!(app.system_state_host.generation, generation);
        assert_eq!(
            app.native_runtime
                .view_state(view)
                .unwrap()
                .common()
                .review_revision,
            review
        );
        assert!(app.system_state_host.action_refusal.is_none());
    }

    #[test]
    fn cancel_shutdown_drains_other_operations_even_after_a_failed_receipt() {
        for already_observed in [false, true] {
            let mut app = App::headless_for_test();
            let (failed, failed_receiver) = InitialSetupCancelReceipt::new();
            app.system_state_host
                .cancel_receipts
                .push(CancelCompletion::new(failed_receiver));
            drop(failed);
            if already_observed {
                assert!(app.resolve_system_state_cancellations());
            }
            let (finish, receive) = mpsc::sync_channel(0);
            app.system_state_host
                .cancel_receipts
                .push(CancelCompletion::new(receive));
            let worker = std::thread::spawn(move || finish.send(()));
            assert!(app.finish_system_state_shutdown().is_err());
            assert!(worker.join().unwrap().is_ok());
        }
    }

    #[test]
    fn cancel_shutdown_refuses_an_unsaved_receipt_after_its_deadline() {
        let (_send, receive) = mpsc::channel();
        let deadline = Instant::now();
        assert!(
            wait_for_cancellations_until(vec![CancelCompletion::new(receive)], deadline).is_err()
        );
    }

    #[test]
    fn cancel_shutdown_does_not_wait_for_apply_admission() {
        let (mut app, wid, _, receipt) = reviewed_apply();
        app.close_active_native_tab(wid).unwrap();
        assert!(app.finish_system_state_shutdown().is_ok());
        assert!(app.system_state_host.pending_apply.is_some());
        drop(receipt);
    }

    #[test]
    fn deferred_cancel_survives_closing_the_final_system_state_tab() {
        let mut app = App::headless_for_test();
        let wid = crate::WindowId(0);
        assert!(app.open_system_state_tab());
        let (instance, view) = app.active_native_view(wid).unwrap();
        let (send, receive) = mpsc::channel();
        let request = Request::new(Plan::default(), send);
        app.attach_system_state_offer(request.clone());
        app.dispatch_native_view_event(
            wid,
            view,
            AppEvent::Action(ActionInvocation {
                id: ActionId::new("system-state/cancel"),
                value: None,
            }),
        )
        .unwrap();
        app.close_active_native_tab(wid).unwrap();
        assert!(app.native_runtime.app(instance).is_none());
        assert!(app.structural_invariants_ok());
        app.tick_system_state(Instant::now());
        assert_eq!(
            receive.try_recv().map(|answer| answer.decision),
            Ok(WorkstationSetupDecision::Cancel)
        );
        assert!(!request.pending());
        assert!(app.system_state_host.pending.is_none());
        assert!(app.system_state_host.next_tick.is_none());
    }

    #[test]
    fn closing_a_review_does_not_dispatch_its_queued_apply_or_check() {
        for action in ["apply", "check"] {
            let mut app = App::headless_for_test();
            let wid = crate::WindowId(0);
            assert!(app.open_system_state_tab());
            let (_, view) = app.active_native_view(wid).unwrap();
            let (send, receive) = mpsc::channel();
            app.attach_system_state_offer(Request::new(Plan::default(), send));
            app.dispatch_native_view_event(
                wid,
                view,
                AppEvent::Action(ActionInvocation {
                    id: ActionId::new(format!("system-state/{action}")),
                    value: None,
                }),
            )
            .unwrap();
            let generation = app.system_state_host.generation;
            app.close_active_native_tab(wid).unwrap();
            app.tick_system_state(Instant::now());
            assert_eq!(
                receive.try_recv().map(|answer| answer.decision),
                Ok(WorkstationSetupDecision::Cancel)
            );
            assert_eq!(app.system_state_host.generation, generation.wrapping_add(1));
            assert!(app.system_state_host.action_refusal.is_none());
            assert!(app.system_state_host.pending.is_none());
            assert!(app.system_state_host.pending_apply.is_none());
            assert!(app.structural_invariants_ok());
        }
    }

    #[test]
    fn deferred_manual_cancel_reaches_the_host_when_its_final_view_closes() {
        let mut app = App::headless_for_test();
        let wid = crate::WindowId(0);
        assert!(app.open_system_state_tab());
        let (_, view) = app.active_native_view(wid).unwrap();
        app.dispatch_native_view_event(
            wid,
            view,
            AppEvent::Action(ActionInvocation {
                id: ActionId::new("system-state/cancel"),
                value: None,
            }),
        )
        .unwrap();
        let generation = app.system_state_host.generation;
        app.close_active_native_tab(wid).unwrap();
        app.tick_system_state(Instant::now());
        let refusal = app.system_state_host.action_refusal.as_ref().unwrap();
        assert_eq!(refusal.name, "Cancel");
        assert_eq!(refusal.status, "not started");
        assert_eq!(app.system_state_host.generation, generation.wrapping_add(1));
        assert!(app.structural_invariants_ok());
    }

    #[test]
    fn deferred_cancel_stays_queued_when_a_linked_system_state_view_survives() {
        for automatic in [false, true] {
            let mut app = App::headless_for_test();
            let wid = crate::WindowId(0);
            assert!(app.open_system_state_tab());
            let (instance, _) = app.active_native_view(wid).unwrap();
            app.stage_system_state_tab(wid).unwrap();
            let (_, view) = app.active_native_view(wid).unwrap();
            let (send, receive) = mpsc::channel();
            if automatic {
                app.attach_system_state_offer(Request::new(Plan::default(), send));
            }
            app.dispatch_native_view_event(
                wid,
                view,
                AppEvent::Action(ActionInvocation {
                    id: ActionId::new("system-state/cancel"),
                    value: None,
                }),
            )
            .unwrap();
            let generation = app.system_state_host.generation;
            app.close_active_native_tab(wid).unwrap();
            assert!(app.native_runtime.app(instance).is_some());
            assert_eq!(app.system_state_host.generation, generation);
            assert!(app.system_state_host.action_refusal.is_none());
            if automatic {
                assert!(matches!(receive.try_recv(), Err(mpsc::TryRecvError::Empty)));
            }
            app.tick_system_state(Instant::now());
            assert_eq!(app.system_state_host.generation, generation.wrapping_add(1));
            if automatic {
                assert_eq!(
                    receive.try_recv().map(|answer| answer.decision),
                    Ok(WorkstationSetupDecision::Cancel)
                );
                assert!(app.system_state_host.action_refusal.is_none());
            } else {
                assert_eq!(
                    app.system_state_host.action_refusal.as_ref().unwrap().name,
                    "Cancel"
                );
            }
            assert!(app.native_runtime.take_system_state_action().is_none());
            app.tick_system_state(Instant::now() + Duration::from_secs(1));
            assert_eq!(app.system_state_host.generation, generation.wrapping_add(1));
            assert!(app.structural_invariants_ok());
        }
    }

    #[test]
    #[cfg(target_os = "macos")]
    fn system_state_attention_surfaces_as_a_nonmodal_indicator_without_focus() {
        let mut app = App::headless_for_test();
        let wid = crate::WindowId(0);
        let terminal = app.windows[&wid].tab_set.active_index().unwrap();
        assert!(app.open_system_state_tab());
        let (instance, view) = app.active_native_view(wid).unwrap();
        app.switch_tab_in(wid, terminal);
        app.windows.get_mut(&wid).unwrap().focused = false;
        assert!(app.active_native_view(wid).is_none());
        let status = SetupStatus {
            rows: vec![SetupStatusRow {
                name: "Secrets vault".into(),
                status: "blocked".into(),
                detail: "approval code NOTICE73".into(),
            }],
            ..SetupStatus::default()
        };
        for _ in 0..2 {
            app.publish_system_state(status.clone());
            assert!(
                app.native_runtime
                    .presentation(instance, view)
                    .unwrap()
                    .indicators
                    .attention
            );
            assert!(
                app.system_state_host.notice_shown,
                "the existing tab indicator surfaces attention without focus or a dialog"
            );
        }
    }

    #[test]
    #[cfg(target_os = "macos")]
    fn system_state_notice_retries_deferred_presentation_until_one_success() {
        let mut app = App::headless_for_test();
        let wid = crate::WindowId(0);
        let terminal = app.windows[&wid].tab_set.active_index().unwrap();
        assert!(app.open_system_state_tab());
        let (instance, view) = app.active_native_view(wid).unwrap();
        let status = SetupStatus {
            rows: vec![SetupStatusRow {
                name: "Secrets vault".into(),
                status: "blocked".into(),
                detail: "approval code NOTICE73".into(),
            }],
            ..SetupStatus::default()
        };
        app.publish_system_state_with_notice(status.clone(), |_| {
            panic!("an active System State review needs no attention sheet")
        });
        assert!(!app.system_state_host.notice_shown);
        app.switch_tab_in(wid, terminal);

        let mut calls = 0;
        let mut revision = None;
        for shown in [false, false, true] {
            app.publish_system_state_with_notice(status.clone(), |_| {
                calls += 1;
                shown
            });
            assert_eq!(app.system_state_host.notice_shown, shown);
            assert!(
                app.native_runtime
                    .presentation(instance, view)
                    .unwrap()
                    .indicators
                    .attention
            );
            let current = app
                .native_runtime
                .view_state(view)
                .unwrap()
                .common()
                .presentation_revision;
            if let Some(previous) = revision {
                assert_eq!(
                    current, previous,
                    "notice retries must not repaint the form"
                );
            }
            revision = Some(current);
        }
        assert_eq!(calls, 3);
        for _ in 0..2 {
            app.publish_system_state_with_notice(status.clone(), |_| {
                panic!("a successfully presented notice must not be repeated")
            });
            assert!(app.system_state_host.notice_shown);
        }
    }

    #[test]
    #[cfg(target_os = "macos")]
    fn closed_check_notices_match_the_plan_that_reopening_restores() {
        use aterm_types::macos_setup::Setting;

        for (saved, edit, action_failure) in [
            (None, None, false),
            (Some("http://saved.local/v1/team"), None, false),
            (Some("http://saved.local/v1/team"), Some("url"), true),
            (Some("http://saved.local/v1/team"), Some("url"), false),
            (Some("http://saved.local/v1/team"), Some("choice"), false),
            (None, Some("url"), false),
        ] {
            let mut app = App::headless_for_test();
            let wid = crate::WindowId(0);
            let saved = saved.map(|url| Plan {
                vault_url: url.into(),
                ..Plan::default()
            });
            let reopened_plan = saved.clone().unwrap_or_default();
            let matches_reopened = edit.is_none();
            assert!(app.open_system_state_tab());
            let (instance, view) = app.active_native_view(wid).unwrap();
            app.publish_system_state(SetupStatus {
                plan: saved.clone(),
                ..SetupStatus::default()
            });
            let edit = match edit {
                Some("url") => Some((
                    "system-state/url",
                    SemanticInput::Text("http://checked.local/v1/team".into()),
                )),
                Some("choice") => Some(("system-state/setting/0", SemanticInput::Bool(false))),
                _ => None,
            };
            if let Some((key, value)) = edit {
                app.dispatch_native_view_event(
                    wid,
                    view,
                    AppEvent::Action(ActionInvocation {
                        id: ActionId::new(key),
                        value: Some(value),
                    }),
                )
                .unwrap();
            }
            app.dispatch_native_view_event(
                wid,
                view,
                AppEvent::Action(ActionInvocation {
                    id: ActionId::new("system-state/check"),
                    value: None,
                }),
            )
            .unwrap();
            let Some(SystemStateAction::Check(checked)) =
                app.native_runtime.take_system_state_action()
            else {
                panic!("the displayed review must dispatch Check")
            };
            assert_eq!(checked == reopened_plan, matches_reopened);
            if !checked.selection.contains(Setting::CapsLock) {
                assert!(reopened_plan.selection.contains(Setting::CapsLock));
            }
            app.run_system_state_action_with_handlers(
                SystemStateAction::Check(checked.clone()),
                Some(InitialSetupHandlers {
                    check: |_| true,
                    ..queued_apply_handlers()
                }),
            );
            app.close_active_native_tab(wid).unwrap();
            assert!(app.native_runtime.app(instance).is_none());
            assert!(app.structural_invariants_ok());
            let status = SetupStatus {
                plan: saved,
                check: Some(crate::SetupCheck {
                    plan: checked,
                    rows: vec![SetupStatusRow {
                        name: "Check: Secrets vault".into(),
                        status: "blocked".into(),
                        detail: "approval code CLOSED73".into(),
                    }],
                }),
                rows: if action_failure {
                    vec![SetupStatusRow {
                        name: "Restore preferences".into(),
                        status: "failed".into(),
                        detail: "injected action failure".into(),
                    }]
                } else {
                    Vec::new()
                },
                ..SetupStatus::default()
            };
            let expected_attention = matches_reopened || action_failure;
            let mut notices = 0;
            app.publish_system_state_with_notice(status.clone(), |_| {
                notices += 1;
                true
            });
            assert_eq!(notices, usize::from(expected_attention));
            assert_eq!(app.system_state_host.notice_shown, expected_attention);

            assert!(app.open_system_state_tab());
            app.publish_system_state(status);
            let (reopened, _) = app.active_native_view(wid).unwrap();
            let Some(NativeApp::SystemState(state)) = app.native_runtime.app(reopened) else {
                panic!("the notice reopens System State")
            };
            assert_eq!(state.reviewed_plan(), reopened_plan);
            assert_eq!(state.needs_attention(), expected_attention);
        }
    }

    #[test]
    #[cfg(target_os = "macos")]
    fn check_plan_mismatch_does_not_announce_background_attention() {
        let mut app = App::headless_for_test();
        let wid = crate::WindowId(0);
        let terminal = app.windows[&wid].tab_set.active_index().unwrap();
        let window = app.windows.get_mut(&wid).unwrap();
        window.rows = 90;
        window.cols = 160;
        assert!(app.open_system_state_tab());
        let (instance, view) = app.active_native_view(wid).unwrap();
        let url = "http://reviewed.local:7484/v1/team";
        edit_review(&mut app, wid, view, url);
        app.switch_tab_in(wid, terminal);
        assert!(app.active_native_view(wid).is_none());
        assert!(app.structural_invariants_ok());
        let mut status = SetupStatus {
            plan: Some(Plan::default()),
            check: Some(crate::SetupCheck {
                plan: Plan::default(),
                rows: vec![SetupStatusRow {
                    name: "Check: Secrets vault".into(),
                    status: "blocked".into(),
                    detail: "approval code CHECK73".into(),
                }],
            }),
            ..SetupStatus::default()
        };
        for _ in 0..2 {
            app.receive_system_state_status(Some(app.system_state_host.generation), status.clone());
            assert!(!app.system_state_host.notice_shown);
            assert!(
                !app.native_runtime
                    .presentation(instance, view)
                    .unwrap()
                    .indicators
                    .attention
            );
        }
        let check = status.check.as_mut().unwrap();
        check.plan.vault_url = url.into();
        check
            .plan
            .selection
            .set(aterm_types::macos_setup::Setting::Dock, false);
        app.receive_system_state_status(Some(app.system_state_host.generation), status.clone());
        assert!(
            app.system_state_host.notice_shown,
            "a matching background check surfaces through the non-modal indicator"
        );
        assert!(
            app.native_runtime
                .presentation(instance, view)
                .unwrap()
                .indicators
                .attention
        );
        app.publish_system_state_with_notice(status, |_| {
            panic!("a shown non-modal notice is not presented again")
        });
        assert!(app.system_state_host.notice_shown);
        assert!(app.open_system_state_tab());
        assert_review(&mut app, wid, view, url, false);
    }

    #[test]
    fn check_receives_the_reviewed_plan_once_and_preserves_unsubmitted_inputs() {
        use aterm_types::macos_setup::Setting;
        use std::cell::RefCell;
        thread_local! {
            static CHECKED: RefCell<Vec<Plan>> = const { RefCell::new(Vec::new()) };
        }
        fn check(plan: Plan) -> bool {
            CHECKED.with(|plans| plans.borrow_mut().push(plan));
            true
        }
        for startup_offer in [false, true] {
            let mut app = App::headless_for_test();
            let wid = crate::WindowId(0);
            let window = app.windows.get_mut(&wid).unwrap();
            window.rows = 90;
            window.cols = 160;
            assert!(app.open_system_state_tab());
            let (_, view) = app.active_native_view(wid).unwrap();
            let (send, receive) = mpsc::channel();
            let request = Request::new(Plan::default(), send);
            if startup_offer {
                app.attach_system_state_offer(request.clone());
            }
            let old_status = SetupStatus {
                plan: Some(Plan::default()),
                ..SetupStatus::default()
            };
            app.publish_system_state(old_status.clone());
            let start = Instant::now();
            app.native_runtime.tick_system_state(start, true);
            let url = "  http://reviewed.local:7484/v1/team  ";
            edit_review(&mut app, wid, view, url);
            for _ in 0..2 {
                app.dispatch_native_view_event(
                    wid,
                    view,
                    AppEvent::Action(ActionInvocation {
                        id: ActionId::new("system-state/check"),
                        value: None,
                    }),
                )
                .unwrap();
            }
            let action = app.native_runtime.take_system_state_action().unwrap();
            assert!(app.native_runtime.take_system_state_action().is_none());
            app.run_system_state_action_with_handlers(
                action,
                Some(InitialSetupHandlers {
                    check,
                    ..queued_apply_handlers()
                }),
            );
            let mut expected = Plan {
                vault_url: url.trim().into(),
                ..Plan::default()
            };
            expected.selection.set(Setting::Dock, false);
            CHECKED.with(|plans| {
                assert_eq!(std::mem::take(&mut *plans.borrow_mut()), vec![expected]);
            });
            app.receive_system_state_status(Some(app.system_state_host.generation), old_status);
            assert_review(&mut app, wid, view, url, false);
            assert!(app.system_state_host.pending_apply.is_none());
            app.native_runtime
                .tick_system_state(start + Duration::from_secs(60), true);
            assert!(app.native_runtime.take_system_state_action().is_none());
            if startup_offer {
                assert!(request.pending());
                assert!(matches!(receive.try_recv(), Err(mpsc::TryRecvError::Empty)));
            }
        }
    }

    #[test]
    fn restore_retires_the_automatic_offer_and_preserves_a_review_for_manual_apply() {
        for restore_started in [true, false] {
            let mut app = App::headless_for_test();
            let wid = crate::WindowId(0);
            let window = app.windows.get_mut(&wid).unwrap();
            window.rows = 90;
            window.cols = 160;
            assert!(app.open_system_state_tab());
            let (_, view) = app.active_native_view(wid).unwrap();
            let (send, receive) = mpsc::channel();
            let request = Request::new(Plan::default(), send);
            app.attach_system_state_offer(request.clone());
            let url = "http://reviewed.local:7484/v1/team";
            edit_review(&mut app, wid, view, url);
            app.dispatch_native_view_event(
                wid,
                view,
                AppEvent::Action(ActionInvocation {
                    id: ActionId::new("system-state/restore"),
                    value: None,
                }),
            )
            .unwrap();
            let action = app.native_runtime.take_system_state_action().unwrap();
            app.run_system_state_action_with_handlers(
                action,
                Some(InitialSetupHandlers {
                    restore: if restore_started { || true } else { || false },
                    ..queued_apply_handlers()
                }),
            );
            assert!(!request.pending());
            assert_eq!(
                receive.try_recv().unwrap().decision,
                WorkstationSetupDecision::Unavailable
            );
            assert!(app.system_state_host.pending.is_none());
            assert!(!app.native_runtime.system_state_mut().unwrap().pending());

            app.receive_system_state_status(
                Some(app.system_state_host.generation),
                SetupStatus {
                    plan: Some(Plan::default()),
                    finished: restore_started,
                    ..SetupStatus::default()
                },
            );
            assert_review(&mut app, wid, view, url, false);
            let receipt = queue_review(&mut app, wid, view);
            assert!(app.system_state_host.pending_apply.is_some());
            assert_review(&mut app, wid, view, url, true);
            drop(receipt);
            app.resolve_system_state_apply();
            assert_review(&mut app, wid, view, url, false);
        }
    }

    #[test]
    fn automatic_apply_retains_its_review_until_the_worker_acknowledges_admission() {
        for accepted in [true, false] {
            let mut app = App::headless_for_test();
            let wid = crate::WindowId(0);
            let window = app.windows.get_mut(&wid).unwrap();
            window.rows = 90;
            window.cols = 160;
            assert!(app.open_system_state_tab());
            let (_, view) = app.active_native_view(wid).unwrap();
            let (send, receive) = mpsc::channel();
            app.attach_system_state_offer(Request::new(Plan::default(), send));
            let url = "http://reviewed.local:7484/v1/team";
            edit_review(&mut app, wid, view, url);
            app.dispatch_native_view_event(
                wid,
                view,
                AppEvent::Action(ActionInvocation {
                    id: ActionId::new("system-state/apply"),
                    value: None,
                }),
            )
            .unwrap();
            let action = app.native_runtime.take_system_state_action().unwrap();
            app.run_system_state_action_with_handlers(action, None);
            let mut answer = receive.try_recv().unwrap();
            assert_eq!(
                answer.password.take().unwrap().as_slice(),
                b"inert-password"
            );
            app.receive_system_state_status(
                Some(app.system_state_host.generation),
                SetupStatus {
                    plan: Some(Plan::default()),
                    ..SetupStatus::default()
                },
            );
            assert_review(&mut app, wid, view, url, true);
            let receipt = answer.receipt.take().unwrap();
            if accepted {
                receipt.accept();
            } else {
                drop(receipt);
            }
            app.resolve_system_state_apply();
            if accepted {
                let (instance, _) = app.active_native_view(wid).unwrap();
                assert!(
                    !app.native_runtime
                        .presentation(instance, view)
                        .unwrap()
                        .indicators
                        .dirty
                );
                let model = app.native_runtime.system_state_mut().unwrap();
                assert_eq!(
                    model.field_projection("system-state/url").unwrap().text,
                    url
                );
                assert!(
                    model
                        .field_projection("system-state/password")
                        .unwrap()
                        .text
                        .is_empty()
                );
            } else {
                assert_review(&mut app, wid, view, url, false);
            }
        }
    }

    #[test]
    fn permission_policy_requires_a_matching_apply_receipt_and_fresh_status() {
        let policy = |revision, allowed| SetupStatus {
            permissions: Some(crate::InitialSetupPermissionPolicy { revision, allowed }),
            ..SetupStatus::default()
        };
        for (startup, permitted) in [(true, true), (true, false), (false, true), (false, false)] {
            let mut app = App::headless_for_test();
            let wid = crate::WindowId(0);
            app.workstation_setup =
                crate::workstation_setup::Host::for_launch(Some(|| true), false);
            assert!(app.open_system_state_tab());
            let (_, view) = app.active_native_view(wid).unwrap();
            app.receive_system_state_status(
                Some(app.system_state_host.generation),
                policy(5, true),
            );
            assert!(!app.workstation_setup.suppresses_permissions());
            let (send, receive) = mpsc::channel();
            let mut plan = Plan::default();
            plan.selection
                .set(aterm_types::macos_setup::Setting::Permissions, permitted);
            if startup {
                app.attach_system_state_offer(Request::new(plan.clone(), send));
                app.receive_system_state_status(
                    Some(app.system_state_host.generation),
                    policy(5, true),
                );
                assert!(app.workstation_setup.suppresses_permissions());
            } else if !permitted {
                let index = aterm_types::macos_setup::Setting::ALL
                    .iter()
                    .position(|setting| *setting == aterm_types::macos_setup::Setting::Permissions)
                    .unwrap();
                app.dispatch_native_view_event(
                    wid,
                    view,
                    AppEvent::Action(ActionInvocation {
                        id: ActionId::new(format!("system-state/setting/{index}")),
                        value: Some(SemanticInput::Bool(false)),
                    }),
                )
                .unwrap();
            }
            let receipt = if startup {
                app.dispatch_native_view_event(
                    wid,
                    view,
                    AppEvent::Action(ActionInvocation {
                        id: ActionId::new("system-state/apply"),
                        value: None,
                    }),
                )
                .unwrap();
                let action = app.native_runtime.take_system_state_action().unwrap();
                app.run_system_state_action_with_handlers(action, None);
                receive.try_recv().unwrap().receipt.take().unwrap()
            } else {
                queue_review(&mut app, wid, view)
            };
            let before_ack = app.system_state_host.generation;
            app.receive_system_state_status(Some(before_ack), policy(6, permitted));
            assert!(app.workstation_setup.suppresses_permissions());
            receipt.accept();
            app.resolve_system_state_apply();
            app.publish_system_state(policy(6, permitted));
            app.receive_system_state_status(Some(before_ack), policy(6, permitted));
            assert!(app.workstation_setup.suppresses_permissions());
            app.receive_system_state_status(
                Some(app.system_state_host.generation),
                policy(6, permitted),
            );
            assert_eq!(app.workstation_setup.suppresses_permissions(), !permitted);
            app.run_system_state_action_with_handlers(
                SystemStateAction::Check(plan),
                Some(InitialSetupHandlers {
                    check: |_| true,
                    ..queued_apply_handlers()
                }),
            );
            app.receive_system_state_status(
                Some(app.system_state_host.generation),
                policy(6, permitted),
            );
            assert_eq!(app.workstation_setup.suppresses_permissions(), !permitted);

            for action in [SystemStateAction::Cancel, SystemStateAction::Restore] {
                app.run_system_state_action_with_handlers(
                    action,
                    Some(InitialSetupHandlers {
                        cancel: |receipt| {
                            receipt.complete();
                            true
                        },
                        restore: || false,
                        ..queued_apply_handlers()
                    }),
                );
                app.publish_system_state(policy(6, true));
                app.receive_system_state_status(
                    Some(app.system_state_host.generation),
                    policy(6, true),
                );
                assert!(app.workstation_setup.suppresses_permissions());
            }
            app.close_active_native_tab(wid).unwrap();
            app.receive_system_state_status(
                Some(app.system_state_host.generation),
                policy(6, false),
            );
            assert!(app.workstation_setup.suppresses_permissions());
            assert!(app.system_state_host.next_status.is_some());
        }
    }

    #[test]
    fn manual_apply_receipt_keeps_inputs_busy_until_a_spawned_worker_accepts_or_refuses() {
        let (mut app, wid, view, receipt) = reviewed_apply();
        for _ in 0..3 {
            app.receive_system_state_status(None, SetupStatus::default());
            app.receive_system_state_status(
                Some(app.system_state_host.generation),
                SetupStatus {
                    plan: Some(Plan::default()),
                    ..SetupStatus::default()
                },
            );
            app.resolve_system_state_apply();
            assert_review(
                &mut app,
                wid,
                view,
                "http://reviewed.local:7484/v1/team",
                true,
            );
        }
        drop(receipt);
        app.resolve_system_state_apply();
        assert_review(
            &mut app,
            wid,
            view,
            "http://reviewed.local:7484/v1/team",
            false,
        );
        assert_eq!(
            app.system_state_host.action_refusal.as_ref().unwrap().name,
            "Apply"
        );
    }

    #[test]
    fn manual_apply_receipt_accepts_once_and_old_answers_cannot_clear_a_new_review() {
        let (mut app, wid, view, stale) = reviewed_apply();
        app.run_system_state_action_with_handlers(
            SystemStateAction::Cancel,
            Some(InitialSetupHandlers {
                cancel: |receipt| {
                    receipt.complete();
                    true
                },
                ..queued_apply_handlers()
            }),
        );
        app.receive_system_state_status(
            Some(app.system_state_host.generation),
            SetupStatus {
                plan: Some(Plan::default()),
                ..SetupStatus::default()
            },
        );
        assert!(app.system_state_host.pending_apply.is_none());
        assert_review(
            &mut app,
            wid,
            view,
            "http://reviewed.local:7484/v1/team",
            false,
        );
        edit_review(&mut app, wid, view, "http://newer.local:7484/v1/team");
        let current = queue_review(&mut app, wid, view);
        stale.accept();
        app.resolve_system_state_apply();
        assert_review(&mut app, wid, view, "http://newer.local:7484/v1/team", true);
        let before_ack = app.system_state_host.generation;
        current.accept();
        app.resolve_system_state_apply();
        app.receive_system_state_status(
            Some(before_ack),
            SetupStatus {
                plan: Some(Plan::default()),
                ..SetupStatus::default()
            },
        );
        let (instance, _) = app.active_native_view(wid).unwrap();
        let indicators = app
            .native_runtime
            .presentation(instance, view)
            .unwrap()
            .indicators;
        assert!(!indicators.dirty && !indicators.busy);
        let Some(NativeApp::SystemState(state)) = app.native_runtime.app(instance) else {
            panic!("system state")
        };
        assert_eq!(
            state
                .field_projection("system-state/password")
                .unwrap()
                .text,
            ""
        );
        assert_eq!(
            state.field_projection("system-state/url").unwrap().text,
            "http://newer.local:7484/v1/team"
        );
        let revision = app
            .native_runtime
            .view_state(view)
            .unwrap()
            .common()
            .presentation_revision;
        app.resolve_system_state_apply();
        assert_eq!(
            app.native_runtime
                .view_state(view)
                .unwrap()
                .common()
                .presentation_revision,
            revision
        );
    }

    #[test]
    fn manual_apply_receipt_cannot_accept_a_closed_and_reopened_instance() {
        let (mut app, wid, _, receipt) = reviewed_apply();
        let (old, _) = app.active_native_view(wid).unwrap();
        app.close_active_native_tab(wid).unwrap();
        assert!(app.native_runtime.app(old).is_none());
        assert!(app.structural_invariants_ok());
        assert!(app.open_system_state_tab());
        let (new, view) = app.active_native_view(wid).unwrap();
        assert_ne!(old, new);
        edit_review(&mut app, wid, view, "http://reopened.local:7484/v1/team");
        receipt.accept();
        app.resolve_system_state_apply();
        assert_review(
            &mut app,
            wid,
            view,
            "http://reopened.local:7484/v1/team",
            false,
        );
        assert!(app.system_state_host.pending_apply.is_none());
    }

    #[test]
    fn detached_admission_updates_permissions_without_accepting_a_reopened_review() {
        for reopen in [false, true] {
            let (mut app, wid, _, receipt) = reviewed_apply();
            app.close_active_native_tab(wid).unwrap();
            app.tick_system_state(Instant::now());
            let policy = SetupStatus {
                permissions: Some(crate::InitialSetupPermissionPolicy {
                    revision: 9,
                    allowed: true,
                }),
                ..SetupStatus::default()
            };
            let new_view = if reopen {
                assert!(app.open_system_state_tab());
                let (_, view) = app.active_native_view(wid).unwrap();
                app.receive_system_state_status(
                    Some(app.system_state_host.generation),
                    policy.clone(),
                );
                let url = "http://reopened.local:7484/v1/team";
                edit_review(&mut app, wid, view, url);
                assert_review(&mut app, wid, view, url, false);
                app.run_system_state_action_with_handlers(
                    SystemStateAction::Check(Plan::default()),
                    Some(InitialSetupHandlers {
                        check: |_| true,
                        ..queued_apply_handlers()
                    }),
                );
                Some(view)
            } else {
                None
            };
            receipt.accept();
            app.resolve_system_state_apply();
            assert!(app.workstation_setup.suppresses_permissions());
            app.receive_system_state_status(Some(app.system_state_host.generation), policy);
            assert!(!app.workstation_setup.suppresses_permissions());
            if let Some(view) = new_view {
                assert_review(
                    &mut app,
                    wid,
                    view,
                    "http://reopened.local:7484/v1/team",
                    false,
                );
            }
        }
    }

    #[test]
    fn newer_system_state_intents_retire_detached_admission() {
        for next in ["cancel", "restore", "apply"] {
            let (mut app, wid, _, old_receipt) = reviewed_apply();
            app.close_active_native_tab(wid).unwrap();
            assert!(app.open_system_state_tab());
            let (_, view) = app.active_native_view(wid).unwrap();
            let url = "http://reopened.local:7484/v1/team";
            edit_review(&mut app, wid, view, url);
            let new_receipt = if next == "apply" {
                Some(queue_review(&mut app, wid, view))
            } else {
                app.run_system_state_action_with_handlers(
                    if next == "cancel" {
                        SystemStateAction::Cancel
                    } else {
                        SystemStateAction::Restore
                    },
                    Some(InitialSetupHandlers {
                        cancel: |receipt| {
                            receipt.complete();
                            true
                        },
                        restore: || true,
                        ..queued_apply_handlers()
                    }),
                );
                None
            };
            old_receipt.accept();
            app.resolve_system_state_apply();
            app.receive_system_state_status(
                Some(app.system_state_host.generation),
                SetupStatus {
                    permissions: Some(crate::InitialSetupPermissionPolicy {
                        revision: 9,
                        allowed: true,
                    }),
                    ..SetupStatus::default()
                },
            );
            assert!(app.workstation_setup.suppresses_permissions());
            assert_review(&mut app, wid, view, url, next == "apply");
            drop(new_receipt);
        }
    }

    fn press(key: NamedKey) -> InputEvent {
        InputEvent::Key {
            key: Key::Named(key),
            mods: Modifiers::empty(),
            base_layout: None,
            layout_text: None,
            event_type: KeyEventType::Press,
        }
    }

    #[test]
    fn held_return_cannot_repeat_system_state_apply_after_the_first_pass_finishes() {
        let mut app = App::headless_for_test();
        let wid = crate::WindowId(0);
        let window = app.windows.get_mut(&wid).unwrap();
        window.rows = 90;
        window.cols = 160;
        assert!(app.open_system_state_tab());
        let (instance, view) = app.active_native_view(wid).unwrap();
        app.dispatch_native_view_event(
            wid,
            view,
            AppEvent::FocusChanged(Some(UiKey::new("system-state/apply"))),
        )
        .unwrap();
        let enter = press(NamedKey::Enter);
        let mut repeat = enter.clone();
        let InputEvent::Key { event_type, .. } = &mut repeat else {
            unreachable!()
        };
        *event_type = KeyEventType::Repeat;
        assert!(app.native_input_event(wid, &enter));
        let Some(action @ SystemStateAction::Apply(_)) =
            app.native_runtime.take_system_state_action()
        else {
            panic!("the initial press must activate the focused Apply button")
        };
        assert!(app.native_runtime.take_system_state_action().is_none());
        app.run_system_state_action_with_handlers(
            action,
            Some(InitialSetupHandlers {
                apply: |_, _, _, receipt| {
                    receipt.accept();
                    true
                },
                ..queued_apply_handlers()
            }),
        );
        assert!(app.native_input_event(wid, &repeat));
        assert!(app.native_runtime.take_system_state_action().is_none());
        app.resolve_system_state_apply();
        app.publish_system_state(SetupStatus::default());
        assert!(
            !app.native_runtime
                .presentation(instance, view)
                .unwrap()
                .indicators
                .busy
        );
        assert!(app.native_input_event(wid, &repeat));
        assert!(
            app.native_runtime.take_system_state_action().is_none(),
            "Return auto-repeat must not queue another Apply"
        );
        assert!(app.native_input_event(wid, &enter));
        assert!(matches!(
            app.native_runtime.take_system_state_action(),
            Some(SystemStateAction::Apply(_))
        ));
        assert!(app.native_runtime.take_system_state_action().is_none());
    }

    #[test]
    fn automatic_system_state_offer_clears_prior_apply_focus_in_every_view() {
        let mut app = App::headless_for_test();
        let wid = crate::WindowId(0);
        assert!(app.open_system_state_tab());
        let (instance, first) = app.active_native_view(wid).unwrap();
        let common = app
            .native_runtime
            .view_state_mut(first)
            .unwrap()
            .common_mut();
        common.last_focus = Some(UiKey::new("system-state/apply"));
        common.focus_visible = true;

        assert!(app.native_input_event(wid, &press(NamedKey::Enter)));
        assert!(matches!(
            app.native_runtime.system_state_mut().unwrap().take_action(),
            Some(SystemStateAction::Apply(_))
        ));
        app.publish_system_state(SetupStatus::default());
        let (_, second) = app.stage_system_state_tab(wid).unwrap();
        for view in [first, second] {
            let common = app
                .native_runtime
                .view_state_mut(view)
                .unwrap()
                .common_mut();
            common.last_focus = Some(UiKey::new("system-state/apply"));
            common.focus_visible = true;
        }

        let (send, receive) = mpsc::channel();
        let request = Request::new(Plan::default(), send);
        app.attach_system_state_offer(request.clone());
        assert_eq!(app.active_native_view(wid), Some((instance, second)));
        for view in [first, second] {
            let common = app.native_runtime.view_state(view).unwrap().common();
            assert!(common.last_focus.is_none());
            assert!(!common.focus_visible);
        }
        assert!(app.native_input_event(wid, &press(NamedKey::Enter)));
        assert!(
            app.native_runtime
                .system_state_mut()
                .unwrap()
                .take_action()
                .is_none()
        );
        assert!(request.pending());
        assert!(matches!(receive.try_recv(), Err(mpsc::TryRecvError::Empty)));
    }

    #[test]
    fn queued_escape_stops_the_system_state_timer_without_persisting_cancel() {
        let mut app = App::headless_for_test();
        let wid = crate::WindowId(0);
        assert!(app.open_system_state_tab());
        let (send, receive) = mpsc::channel();
        let request = Request::new(Plan::default(), send);
        app.attach_system_state_offer(request.clone());
        assert!(app.native_input_event(wid, &press(NamedKey::Escape)));
        let model = app.native_runtime.system_state_mut().unwrap();
        model.tick(Instant::now() + Duration::from_secs(90), true);
        assert!(model.take_action().is_none());
        assert!(model.pending());
        assert!(request.pending());
        assert!(matches!(receive.try_recv(), Err(mpsc::TryRecvError::Empty)));
    }

    #[test]
    fn empty_ime_lifecycle_preserves_an_automatic_system_state_offer() {
        use crate::native_app::{CloseReadiness, CloseRequest, CloseScope};

        for field in ["system-state/url", "system-state/password"] {
            let mut app = App::headless_for_test();
            let wid = crate::WindowId(0);
            assert!(app.open_system_state_tab());
            let (instance, view) = app.active_native_view(wid).unwrap();
            let (send, _receive) = mpsc::channel();
            app.attach_system_state_offer(Request::new(Plan::default(), send));
            app.native_runtime
                .dispatch(
                    instance,
                    view,
                    AppEvent::FocusChanged(Some(UiKey::new(field))),
                )
                .unwrap();
            let start = Instant::now();
            app.native_runtime.tick_system_state(start, true);

            app.on_ime_enablement_changed(wid);
            app.on_ime_preedit(wid, String::new(), None);
            assert!(app.windows[&wid].preedit.is_empty());
            assert_eq!(
                app.native_runtime
                    .prepare_close(
                        instance,
                        view,
                        CloseRequest {
                            scope: CloseScope::Relaunch,
                        },
                    )
                    .unwrap()
                    .0,
                CloseReadiness::Ready
            );
            app.native_runtime
                .tick_system_state(start + Duration::from_secs(31), true);
            let Some(SystemStateAction::Apply(answer)) =
                app.native_runtime.take_system_state_action()
            else {
                panic!("an untouched offer must still expire after the IME reset");
            };
            assert_eq!(
                answer.decision,
                WorkstationSetupDecision::Begin(Plan::default())
            );
            assert!(answer.password.is_none());
        }
    }

    #[test]
    fn composing_in_system_state_stops_the_timer_even_after_the_ime_clears() {
        use crate::native_app::{CloseReadiness, CloseRequest, CloseScope};

        for field in ["system-state/url", "system-state/password"] {
            let mut app = App::headless_for_test();
            let wid = crate::WindowId(0);
            assert!(app.open_system_state_tab());
            let (instance, view) = app.active_native_view(wid).unwrap();
            let (send, receive) = mpsc::channel();
            let request = Request::new(Plan::default(), send);
            app.attach_system_state_offer(request.clone());
            app.native_runtime
                .dispatch(
                    instance,
                    view,
                    AppEvent::FocusChanged(Some(UiKey::new(field))),
                )
                .unwrap();
            let start = Instant::now();
            app.native_runtime.tick_system_state(start, true);
            app.on_ime_preedit(wid, "e\u{301}".into(), Some((0, 3)));
            assert_eq!(app.windows[&wid].preedit, "e\u{301}");
            app.on_ime_enablement_changed(wid);
            assert!(app.windows[&wid].preedit.is_empty());
            assert_eq!(
                app.native_runtime
                    .system_state_mut()
                    .unwrap()
                    .field_projection(field)
                    .unwrap()
                    .preedit,
                None
            );
            assert!(matches!(
                app.native_runtime
                    .prepare_close(
                        instance,
                        view,
                        CloseRequest {
                            scope: CloseScope::Relaunch,
                        },
                    )
                    .unwrap()
                    .0,
                CloseReadiness::Blocked { .. }
            ));
            app.on_ime_commit(wid, "synthetic-ime".into());
            let projection = app
                .native_runtime
                .system_state_mut()
                .unwrap()
                .field_projection(field)
                .unwrap();
            if field.ends_with("password") {
                assert_eq!(projection.text, "•".repeat(13));
            } else {
                assert!(projection.text.contains("synthetic-ime"));
            }
            app.native_runtime
                .tick_system_state(start + Duration::from_secs(90), true);
            assert!(app.native_runtime.take_system_state_action().is_none());
            assert!(request.pending());
            assert!(matches!(receive.try_recv(), Err(mpsc::TryRecvError::Empty)));
        }
    }

    #[test]
    fn refused_system_state_offer_preserves_the_manual_drafts_typing_focus() {
        let mut app = App::headless_for_test();
        let wid = crate::WindowId(0);
        assert!(app.open_system_state_tab());
        let (_, view) = app.active_native_view(wid).unwrap();
        app.dispatch_native_view_event(
            wid,
            view,
            AppEvent::Action(ActionInvocation {
                id: ActionId::new("system-state/password"),
                value: Some(SemanticInput::Text("synthetic-password".into())),
            }),
        )
        .unwrap();
        app.native_runtime
            .view_state_mut(view)
            .unwrap()
            .common_mut()
            .focus_visible = true;
        assert!(app.commit_glassless_native_frame(wid));
        let revision = app
            .native_runtime
            .view_state(view)
            .unwrap()
            .common()
            .presentation_revision;

        let (send, receive) = mpsc::channel();
        app.attach_system_state_offer(Request::new(Plan::default(), send));
        assert_eq!(
            receive.try_recv().unwrap().decision,
            WorkstationSetupDecision::Unavailable
        );
        assert!(app.system_state_host.pending.is_none());
        assert!(app.retained_native_leaf_artifact(wid, view, true).is_some());
        let common = app.native_runtime.view_state(view).unwrap().common();
        assert_eq!(common.presentation_revision, revision);
        assert_eq!(
            common.last_focus.as_ref().map(UiKey::as_str),
            Some("system-state/password")
        );
        assert!(common.focus_visible);
        assert!(app.native_input_event(wid, &InputEvent::Text("x".to_owned())));
        assert_eq!(
            app.native_runtime
                .system_state_mut()
                .unwrap()
                .field_projection("system-state/password")
                .unwrap()
                .text,
            "•".repeat(19)
        );
    }

    #[test]
    fn stale_system_state_snapshots_cannot_unlock_a_new_apply() {
        let mut app = App::headless_for_test();
        let wid = crate::WindowId(0);
        assert!(app.open_system_state_tab());
        let (instance, view) = app.active_native_view(wid).unwrap();
        let (send, receive) = mpsc::channel();
        app.attach_system_state_offer(Request::new(Plan::default(), send));
        app.publish_system_state(SetupStatus {
            busy: true,
            ..SetupStatus::default()
        });
        let old_poll = app.system_state_host.generation;
        app.run_system_state_action(SystemStateAction::Apply(SetupAnswer::from(
            WorkstationSetupDecision::Begin(Plan::default()),
        )));
        let mut answer = receive.try_recv().unwrap();
        assert!(matches!(
            answer.decision,
            WorkstationSetupDecision::Begin(_)
        ));
        let before_notification = app.system_state_host.generation;
        app.receive_system_state_status(Some(old_poll), SetupStatus::default());
        assert!(
            app.native_runtime
                .presentation(instance, view)
                .unwrap()
                .indicators
                .busy
        );

        app.system_state_host.status_in_flight = true;
        app.system_state_host.next_tick = Some(Instant::now() + Duration::from_secs(10));
        app.receive_system_state_status(None, SetupStatus::default());
        assert!(
            app.native_runtime
                .presentation(instance, view)
                .unwrap()
                .indicators
                .busy
        );
        assert_eq!(
            app.system_state_host.generation,
            before_notification.wrapping_add(1)
        );
        assert!(!app.system_state_host.status_in_flight);
        assert!(app.system_state_host.next_tick.is_none());
        assert!(
            app.system_state_host
                .next_status
                .is_some_and(|due| due <= Instant::now())
        );
        app.receive_system_state_status(Some(before_notification), SetupStatus::default());
        assert!(
            app.native_runtime
                .presentation(instance, view)
                .unwrap()
                .indicators
                .busy
        );

        app.receive_system_state_status(
            Some(app.system_state_host.generation),
            SetupStatus::default(),
        );
        assert!(
            app.native_runtime
                .presentation(instance, view)
                .unwrap()
                .indicators
                .busy
        );
        answer.receipt.take().unwrap().accept();
        app.resolve_system_state_apply();
        app.receive_system_state_status(
            Some(app.system_state_host.generation),
            SetupStatus::default(),
        );
        assert!(
            !app.native_runtime
                .presentation(instance, view)
                .unwrap()
                .indicators
                .busy
        );
    }

    #[test]
    fn refused_actions_survive_current_status_without_stalling_updates_or_repainting_identical_rows()
     {
        let mut app = App::headless_for_test();
        let wid = crate::WindowId(0);
        let window = app.windows.get_mut(&wid).unwrap();
        window.rows = 90;
        window.cols = 160;
        assert!(app.open_system_state_tab());
        let (instance, view) = app.active_native_view(wid).unwrap();
        let (send, receive) = mpsc::channel();
        app.attach_system_state_offer(Request::new(Plan::default(), send));
        let completed = SetupStatus {
            finished: true,
            rows: vec![SetupStatusRow {
                name: "Preferences".into(),
                status: "complete".into(),
                detail: String::new(),
            }],
            ..SetupStatus::default()
        };
        app.publish_system_state(completed.clone());
        app.dispatch_native_view_event(
            wid,
            view,
            AppEvent::Action(ActionInvocation {
                id: ActionId::new("system-state/restore"),
                value: None,
            }),
        )
        .unwrap();
        let action = app.native_runtime.take_system_state_action().unwrap();
        app.run_system_state_action(action);
        assert_eq!(
            receive.try_recv().unwrap().decision,
            WorkstationSetupDecision::Unavailable
        );
        assert!(app.system_state_host.pending.is_none());
        let notice = app
            .system_state_host
            .action_refusal
            .clone()
            .expect("refused restore");
        assert_eq!(notice.name, "Restore preferences");
        let visible_notices = |app: &App| {
            app.windows[&wid].leaf_render_cache[&view]
                .native
                .as_ref()
                .unwrap()
                .compiled
                .semantics
                .iter()
                .filter(|node| {
                    !node.offscreen
                        && node.key.as_str().starts_with("system-state/status/")
                        && node.label.contains(&notice.name)
                        && node.label.contains(&notice.detail)
                })
                .count()
        };
        assert_eq!(app.system_state_host.latest_status, completed);
        assert!(app.commit_glassless_native_frame(wid));
        assert_eq!(visible_notices(&app), 1);
        let revision = app
            .native_runtime
            .view_state(view)
            .unwrap()
            .common()
            .presentation_revision;
        for _ in 0..3 {
            app.receive_system_state_status(
                Some(app.system_state_host.generation),
                completed.clone(),
            );
            assert!(app.retained_native_leaf_artifact(wid, view, true).is_some());
            assert_eq!(
                app.native_runtime
                    .view_state(view)
                    .unwrap()
                    .common()
                    .presentation_revision,
                revision
            );
            assert_eq!(visible_notices(&app), 1);
        }
        let working = SetupStatus {
            busy: true,
            rows: vec![SetupStatusRow {
                name: "Repositories".into(),
                status: "running".into(),
                detail: String::new(),
            }],
            ..SetupStatus::default()
        };
        app.receive_system_state_status(Some(app.system_state_host.generation), working.clone());
        assert!(
            app.native_runtime
                .presentation(instance, view)
                .unwrap()
                .indicators
                .busy
        );
        assert_eq!(app.system_state_host.latest_status, working);
        assert!(app.commit_glassless_native_frame(wid));
        assert_eq!(visible_notices(&app), 1);
        app.receive_system_state_status(Some(app.system_state_host.generation), completed.clone());
        assert!(
            !app.native_runtime
                .presentation(instance, view)
                .unwrap()
                .indicators
                .busy
        );
        let old_generation = app.system_state_host.generation;
        app.dispatch_native_view_event(
            wid,
            view,
            AppEvent::Action(ActionInvocation {
                id: ActionId::new("system-state/apply"),
                value: None,
            }),
        )
        .unwrap();
        let action = app.native_runtime.take_system_state_action().unwrap();
        app.run_system_state_action_with_handlers(action, Some(queued_apply_handlers()));
        let receipt = APPLY_RECEIPTS.with(|receipts| receipts.borrow_mut().pop().unwrap());
        assert!(matches!(
            receive.try_recv(),
            Err(mpsc::TryRecvError::Disconnected)
        ));
        assert!(app.system_state_host.action_refusal.is_none());
        app.receive_system_state_status(Some(old_generation), completed);
        assert!(
            app.native_runtime
                .presentation(instance, view)
                .unwrap()
                .indicators
                .busy
        );
        receipt.accept();
        app.resolve_system_state_apply();
        app.receive_system_state_status(Some(app.system_state_host.generation), working);
        assert!(app.commit_glassless_native_frame(wid));
        assert_eq!(visible_notices(&app), 0);
        assert!(
            app.native_runtime
                .presentation(instance, view)
                .unwrap()
                .indicators
                .busy
        );
    }

    #[test]
    fn refused_apply_preserves_reviewed_inputs_until_a_callback_accepts_them() {
        use crate::native_ui::SemanticValue;
        use aterm_types::macos_setup::Setting;
        use std::cell::RefCell;
        thread_local! {
            static ACCEPTED: RefCell<Vec<SetupAnswer>> = const { RefCell::new(Vec::new()) };
        }
        fn accept(
            plan: Plan,
            vault_typed: bool,
            password: Option<SecretBytes>,
            receipt: InitialSetupApplyReceipt,
        ) -> bool {
            ACCEPTED.with(|answers| {
                answers.borrow_mut().push(SetupAnswer {
                    decision: WorkstationSetupDecision::Begin(plan),
                    password,
                    receipt: None,
                    cancel_receipt: None,
                    vault_typed,
                })
            });
            receipt.accept();
            true
        }
        let refused = InitialSetupHandlers {
            apply: |_, _, _, _| false,
            check: |_| false,
            restore: || false,
            cancel: |_| false,
            status: SetupStatus::default,
        };
        for closed_receiver in [false, true] {
            let mut app = App::headless_for_test();
            let wid = crate::WindowId(0);
            let window = app.windows.get_mut(&wid).unwrap();
            window.rows = 90;
            window.cols = 160;
            assert!(app.open_system_state_tab());
            let (instance, view) = app.active_native_view(wid).unwrap();
            if closed_receiver {
                let (send, receive) = mpsc::channel();
                app.attach_system_state_offer(Request::new(Plan::default(), send));
                drop(receive);
            }
            let old_status = SetupStatus {
                plan: Some(Plan::default()),
                ..SetupStatus::default()
            };
            app.publish_system_state(old_status.clone());
            let dock = format!(
                "system-state/setting/{}",
                Setting::ALL
                    .iter()
                    .position(|setting| *setting == Setting::Dock)
                    .unwrap()
            );
            let url = " http://reviewed.local:7484/v1/team ";
            let secret = "inert-vault-password";
            for (key, value) in [
                (dock.as_str(), SemanticInput::Bool(false)),
                ("system-state/url", SemanticInput::Text(url.into())),
                ("system-state/password", SemanticInput::Text(secret.into())),
            ] {
                app.dispatch_native_view_event(
                    wid,
                    view,
                    AppEvent::Action(ActionInvocation {
                        id: ActionId::new(key),
                        value: Some(value),
                    }),
                )
                .unwrap();
            }
            let apply = |app: &mut App| {
                app.dispatch_native_view_event(
                    wid,
                    view,
                    AppEvent::Action(ActionInvocation {
                        id: ActionId::new("system-state/apply"),
                        value: None,
                    }),
                )
                .unwrap();
                let action = app.native_runtime.take_system_state_action().unwrap();
                assert!(app.native_runtime.take_system_state_action().is_none());
                action
            };
            let action = apply(&mut app);
            app.run_system_state_action_with_handlers(action, Some(refused));
            assert!(app.system_state_host.action_refusal.is_some());
            for _ in 0..3 {
                app.receive_system_state_status(
                    Some(app.system_state_host.generation),
                    old_status.clone(),
                );
                assert!(app.commit_glassless_native_frame(wid));
                let ui = &app.windows[&wid].leaf_render_cache[&view]
                    .native
                    .as_ref()
                    .unwrap()
                    .compiled;
                assert_eq!(
                    ui.semantic(&UiKey::new(dock.as_str())).unwrap().value,
                    SemanticValue::Bool(false)
                );
                assert_eq!(
                    ui.semantic(&UiKey::new("system-state/url")).unwrap().value,
                    SemanticValue::Text(url.into())
                );
                assert_eq!(
                    ui.semantic(&UiKey::new("system-state/password"))
                        .unwrap()
                        .value,
                    SemanticValue::Text("•".repeat(secret.len()))
                );
                let indicators = app
                    .native_runtime
                    .presentation(instance, view)
                    .unwrap()
                    .indicators;
                assert!(indicators.dirty);
                assert!(!indicators.busy);
            }
            let action = apply(&mut app);
            app.run_system_state_action_with_handlers(
                action,
                Some(InitialSetupHandlers {
                    apply: accept,
                    ..refused
                }),
            );
            app.resolve_system_state_apply();
            let mut answers = ACCEPTED.with(|answers| std::mem::take(&mut *answers.borrow_mut()));
            assert_eq!(answers.len(), 1);
            let answer = answers.pop().unwrap();
            let WorkstationSetupDecision::Begin(plan) = answer.decision else {
                panic!("accepted reviewed plan")
            };
            assert!(!plan.selection.contains(Setting::Dock));
            assert_eq!(plan.vault_url, url.trim());
            assert!(answer.vault_typed, "the reviewed vault was typed");
            assert_eq!(answer.password, Some(SecretBytes::from(secret.as_bytes())));
            assert!(app.system_state_host.action_refusal.is_none());
            assert!(
                !app.native_runtime
                    .presentation(instance, view)
                    .unwrap()
                    .indicators
                    .dirty
            );
            assert!(app.commit_glassless_native_frame(wid));
            let ui = &app.windows[&wid].leaf_render_cache[&view]
                .native
                .as_ref()
                .unwrap()
                .compiled;
            assert_eq!(
                ui.semantic(&UiKey::new("system-state/password"))
                    .unwrap()
                    .value,
                SemanticValue::Text(String::new())
            );
            assert_eq!(
                ui.semantic(&UiKey::new("system-state/url")).unwrap().value,
                SemanticValue::Text(url.trim().into())
            );
        }
    }

    #[test]
    fn background_system_state_notification_keeps_permission_policy_polled_without_a_tab() {
        let mut app = App::headless_for_test();
        app.workstation_setup = crate::workstation_setup::Host::for_launch(Some(|| true), false);
        app.receive_system_state_status(None, SetupStatus::default());
        assert!(app.system_state_host.next_status.is_some());
        app.receive_system_state_status(
            Some(app.system_state_host.generation),
            SetupStatus {
                permissions: Some(crate::InitialSetupPermissionPolicy {
                    revision: 8,
                    allowed: true,
                }),
                ..SetupStatus::default()
            },
        );
        assert!(!app.workstation_setup.suppresses_permissions());
        assert!(
            app.native_runtime
                .instance_by_kind(AppKind::SystemState)
                .is_none()
        );
        let due = app.system_state_host.next_status.unwrap();
        assert_eq!(app.system_state_host.deadline(due), Some(due));
    }

    #[test]
    fn permission_suppression_retains_the_system_state_countdown_deadline() {
        let mut app = App::headless_for_test();
        app.workstation_setup = crate::workstation_setup::Host::for_launch(Some(|| true), false);
        assert!(app.workstation_setup.suppresses_permissions());
        let now = Instant::now();
        let due = now + Duration::from_millis(250);
        app.system_state_host.next_tick = Some(due);
        let mut deadline = None;
        let mut owner = crate::metrics::DeadlineOwner::None;
        app.fold_consent_lane_deadlines(now, &mut deadline, &mut owner);
        assert_eq!(deadline, Some(due));
        assert_eq!(owner, crate::metrics::DeadlineOwner::ConsentCard);
    }

    #[test]
    fn countdown_publication_repaints_native_apply_without_keyboard_input() {
        let mut app = App::headless_for_test();
        let wid = crate::WindowId(0);
        assert!(app.open_system_state_tab());
        let (_, view) = app.active_native_view(wid).unwrap();
        let (send, _receive) = mpsc::channel();
        app.attach_system_state_offer(Request::new(Plan::default(), send));
        let start = Instant::now();
        app.native_runtime.tick_system_state(start, true);
        assert!(app.prepare_native_input_scratch(wid));
        let apply_label = |app: &App| {
            app.windows[&wid].leaf_render_cache[&view]
                .native
                .as_ref()
                .unwrap()
                .compiled
                .semantic(&UiKey::new("system-state/apply"))
                .unwrap()
                .label
                .clone()
        };
        assert_eq!(apply_label(&app), "Apply (30)");
        assert!(
            app.native_runtime
                .tick_system_state(start + Duration::from_secs(2), true)
        );
        assert!(app.prepare_native_input_scratch(wid));
        assert_eq!(apply_label(&app), "Apply (28)");
    }

    #[test]
    fn first_presented_offer_is_required_but_status_repaint_does_not_pause_its_clock() {
        let mut app = App::headless_for_test();
        let wid = crate::WindowId(0);
        app.windows.get_mut(&wid).unwrap().os_window_for_test = true;
        assert!(app.open_system_state_tab());
        let (instance, view) = app.active_native_view(wid).unwrap();
        let (send, _receive) = mpsc::channel();
        let request = Request::new(Plan::default(), send);
        app.attach_system_state_offer(request.clone());
        assert!(!app.system_state_countdown_active(instance));
        assert!(app.prepare_native_input_scratch(wid));
        assert!(!app.system_state_countdown_active(instance));
        assert!(app.commit_glassless_native_frame(wid));
        assert!(app.system_state_countdown_active(instance));

        app.publish_system_state(SetupStatus {
            rows: vec![SetupStatusRow {
                name: "Preferences".into(),
                status: "pending".into(),
                detail: String::new(),
            }],
            ..SetupStatus::default()
        });
        assert!(app.retained_native_leaf_artifact(wid, view, true).is_none());
        assert!(app.system_state_countdown_active(instance));
        app.windows.get_mut(&wid).unwrap().focused = false;
        assert!(!app.system_state_countdown_active(instance));
        app.windows.get_mut(&wid).unwrap().focused = true;

        request.finish(WorkstationSetupDecision::Unavailable);
        app.system_state_host.pending = None;
        let (send, _receive_next) = mpsc::channel();
        app.attach_system_state_offer(Request::new(Plan::default(), send));
        assert!(!app.system_state_host.offer_presented);
        assert!(!app.system_state_countdown_active(instance));
    }

    #[test]
    fn system_state_pointer_clicks_toggle_a_choice_and_cancel_the_offer() {
        use crate::app_mouse::{NativePointerTarget, PointerAction};
        use crate::native_ui::SemanticValue;
        let mut app = App::headless_for_test();
        let wid = crate::WindowId(0);
        let window = app.windows.get_mut(&wid).unwrap();
        window.rows = 60;
        window.cols = 120;
        assert!(app.open_system_state_tab());
        let (send, _receive) = mpsc::channel();
        app.attach_system_state_offer(Request::new(Plan::default(), send));
        app.pointer_cmd(PointerAction::MoveNative(NativePointerTarget::Key(
            "system-state/setting/0".to_owned(),
        )))
        .unwrap();
        app.pointer_cmd(PointerAction::Click).unwrap();
        assert_eq!(
            app.compiled_native_ui(wid)
                .unwrap()
                .semantic(&UiKey::new("system-state/setting/0"))
                .unwrap()
                .value,
            SemanticValue::Bool(false)
        );
        app.pointer_cmd(PointerAction::MoveNative(NativePointerTarget::Key(
            "system-state/cancel".to_owned(),
        )))
        .unwrap();
        app.pointer_cmd(PointerAction::Click).unwrap();
        assert!(matches!(
            app.native_runtime.system_state_mut().unwrap().take_action(),
            Some(SystemStateAction::Cancel)
        ));
    }

    #[test]
    fn idle_system_state_ticks_retain_frames_after_expiry_and_interaction() {
        use crate::app_mouse::{NativePointerTarget, PointerAction, WheelUnit};
        for expire in [true, false] {
            let mut app = App::headless_for_test();
            let wid = crate::WindowId(0);
            let window = app.windows.get_mut(&wid).unwrap();
            window.rows = 60;
            window.cols = 120;
            window.os_window_for_test = true;
            assert!(app.open_system_state_tab());
            let (_, view) = app.active_native_view(wid).unwrap();
            let (send, receive) = mpsc::channel();
            let request = Request::new(Plan::default(), send);
            app.attach_system_state_offer(request.clone());
            assert!(app.commit_glassless_native_frame(wid));
            let start = Instant::now();
            app.tick_system_state(start);
            let status = if expire {
                app.tick_system_state(start + Duration::from_secs(30));
                let mut answer = receive.try_recv().unwrap();
                assert!(matches!(
                    answer.decision,
                    WorkstationSetupDecision::Begin(_)
                ));
                answer.receipt.take().unwrap().accept();
                app.resolve_system_state_apply();
                SetupStatus {
                    finished: true,
                    ..SetupStatus::default()
                }
            } else {
                app.interact_system_state(wid);
                app.tick_system_state(start + Duration::from_secs(30));
                assert!(request.pending());
                assert!(receive.try_recv().is_err());
                SetupStatus::default()
            };
            app.publish_system_state(status.clone());
            assert!(app.commit_glassless_native_frame(wid));
            let revision = app
                .native_runtime
                .view_state(view)
                .unwrap()
                .common()
                .presentation_revision;
            for tick in 1..=40 {
                app.tick_system_state(
                    start + Duration::from_secs(30) + Duration::from_millis(tick * 250),
                );
                app.publish_system_state(status.clone());
                assert!(app.retained_native_leaf_artifact(wid, view, true).is_some());
                assert_eq!(
                    app.native_runtime
                        .view_state(view)
                        .unwrap()
                        .common()
                        .presentation_revision,
                    revision
                );
            }
            app.pointer_cmd(PointerAction::Wheel {
                dx: 0.0,
                dy: 1.0,
                unit: WheelUnit::Lines,
                aim: Some(NativePointerTarget::Key("system-state/setting/0".into())),
            })
            .unwrap();
            let key = if expire { "check" } else { "cancel" };
            app.pointer_cmd(PointerAction::MoveNative(NativePointerTarget::Key(
                format!("system-state/{key}"),
            )))
            .unwrap();
            app.pointer_cmd(PointerAction::Click).unwrap();
            assert!(matches!(
                (expire, app.native_runtime.take_system_state_action()),
                (true, Some(SystemStateAction::Check(_)))
                    | (false, Some(SystemStateAction::Cancel))
            ));
        }
    }
}
