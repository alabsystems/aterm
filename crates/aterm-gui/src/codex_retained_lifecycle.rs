// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0

//! Accepted operations belong to the existing Native backend service. Selectors
//! only find these owners. Socket/image/journal IO never holds Running or phase
//! metadata; Source can reenter original-origin/ACK queries during capture.
use super::*;
use crate::native_session_policy::retained_control::{
    Action, Frontier, Selection as SelectionData, SelectionFrontier,
};
use std::os::fd::BorrowedFd;

#[path = "codex_retained_phase.rs"]
mod ordering;
use ordering::Step;
#[path = "codex_retained_move.rs"]
mod replacement_move;
#[path = "codex_retained_selection.rs"]
mod selection;

const CAPACITY: usize = 256;
type Fields = aterm_json::Map;

struct Packet {
    operation: String,
    nonce: String,
    owned: Mutex<registry::PreparedServicePacket>,
}
impl Packet {
    fn new(operation: &str, packet: registry::PreparedServicePacket) -> Arc<Self> {
        Arc::new(Self {
            operation: operation.into(),
            nonce: packet.nonce_data().into(),
            owned: Mutex::new(packet),
        })
    }
    fn offer(&self) -> Result<(), registry::Error> {
        self.owned
            .try_lock()
            .map_err(|_| registry::Error::Busy)?
            .offer()
    }
    fn acknowledged(&self) -> Result<bool, registry::Error> {
        Ok(self
            .owned
            .try_lock()
            .map_err(|_| registry::Error::Busy)?
            .acknowledged())
    }
}
struct Move {
    operation: String,
    original: registry::ServiceFrontend,
    request: Action,
    hold: Arc<Packet>,
    attachment: String,
    phase: Mutex<MovePhase>,
    io: Mutex<MoveIo>,
}
#[derive(Default)]
struct MovePhase {
    go: bool,
    entry: Option<registry::ServiceFrontend>,
    closed: bool,
    ordering: ordering::Progress,
}
impl Move {
    fn advance(&self, step: Step) -> Result<(), registry::Error> {
        self.phase
            .lock()
            .map_err(|_| registry::Error::Unavailable)?
            .ordering
            .advance(step)
    }
}
#[derive(Default)]
struct MoveIo {
    image_attempted: bool,
    attempted: bool,
    images: Option<replacement::Images>,
    input: Option<Vec<u8>>,
    reservation: Option<registry::FrontendExecReservation>,
    journal: Option<aterm_agent::harness::native_receipts::RetainedExecution>,
    command: Option<body_delivery::BodyDelivery>,
    entry: Option<Entry>,
}
struct Entry {
    operation: String,
    callback: PathBuf,
    frontend: registry::ServiceFrontend,
    delivery: body_delivery::BodyDelivery,
}
struct Runtime {
    operation: String,
    frontend: registry::ServiceFrontend,
    go: Option<Arc<Move>>,
    packet: Arc<Packet>,
    attachment: String,
    human: u64,
    phase: Mutex<RuntimePhase>,
}
impl Runtime {
    fn require_app_components(&self) -> Result<(), registry::Error> {
        if let Some(accepted) = &self.go {
            let owned = accepted.io.try_lock().map_err(|_| registry::Error::Busy)?;
            let images = owned.images.as_ref().ok_or(registry::Error::Denied)?;
            images.validate()?;
            if !images.app_components() {
                return Err(registry::Error::Denied);
            }
        }
        Ok(())
    }
    fn advance(&self, step: Step) -> Result<(), registry::Error> {
        self.go
            .as_ref()
            .map_or(Ok(()), |accepted| accepted.advance(step))
    }
    fn seal_execution(&self) -> Result<(), registry::Error> {
        if let Some(accepted) = &self.go {
            let owned = accepted.io.try_lock().map_err(|_| registry::Error::Busy)?;
            owned
                .journal
                .as_ref()
                .ok_or(registry::Error::Denied)?
                .seal()
                .map_err(|_| registry::Error::Unavailable)?;
            accepted
                .phase
                .lock()
                .map_err(|_| registry::Error::Unavailable)?
                .closed = true;
        }
        Ok(())
    }
}
#[derive(Default)]
struct RuntimePhase {
    ui: bool,
    observed: bool,
    app_owner: Option<String>,
    committed: bool,
}
struct SourceSelection {
    source: String,
    selection: String,
    backend: registry::ServiceFrontend,
    original: registry::ServiceFrontend,
    attachment: String,
    registered: bool,
    phase: Mutex<SelectionPhase>,
}
#[derive(Default)]
struct SelectionPhase {
    inhibit: Option<Arc<Packet>>,
    retired: bool,
}
struct SelectedRuntime {
    source: Arc<SourceSelection>,
    runtime: Arc<Runtime>,
}
struct AppTenure {
    owner: String,
    runtime: Arc<Runtime>,
}
#[derive(Default)]
pub(super) struct Lifecycle {
    moves: Vec<Arc<Move>>,
    runtimes: Vec<Arc<Runtime>>,
    packets: Vec<Arc<Packet>>,
    selections: Vec<Arc<SourceSelection>>,
    selected: Vec<Arc<SelectedRuntime>>,
    app: Option<Arc<AppTenure>>,
}
impl Lifecycle {
    fn prune_completed(&mut self) {
        let current = self.app.as_ref().map(|app| app.runtime.clone());
        self.runtimes.retain(|runtime| {
            current
                .as_ref()
                .is_some_and(|current| Arc::ptr_eq(current, runtime))
                || !runtime.frontend.retired()
                || runtime
                    .phase
                    .try_lock()
                    .map_or(true, |phase| !phase.observed)
        });
        self.selected.retain(|selected| {
            current
                .as_ref()
                .is_some_and(|current| Arc::ptr_eq(current, &selected.runtime))
                || !selected.runtime.frontend.retired()
                || selected
                    .runtime
                    .phase
                    .try_lock()
                    .map_or(true, |phase| !phase.committed)
        });
        self.moves.retain(|accepted| {
            current
                .as_ref()
                .and_then(|current| current.go.as_ref())
                .is_some_and(|current| Arc::ptr_eq(current, accepted))
                || accepted.phase.try_lock().map_or(true, |phase| {
                    !phase.closed || phase.entry.as_ref().is_none_or(|entry| !entry.retired())
                })
        });
        self.selections.retain(|selection| {
            !selection.original.retired()
                || selection
                    .phase
                    .try_lock()
                    .map_or(true, |phase| !phase.retired)
                || Arc::strong_count(selection) > 1
        });
        // Only acknowledged packets with no remaining operation/runtime owner
        // can leave this completed generation. Uncertain sends remain owned.
        self.packets.retain(|packet| {
            Arc::strong_count(packet) > 1 || packet.acknowledged().map_or(true, |ack| !ack)
        });
    }
    pub(super) fn attached(
        &self,
        frontend: &registry::ServiceFrontend,
    ) -> Result<(), registry::Error> {
        for accepted in &self.moves {
            let matching = accepted
                .phase
                .lock()
                .map_err(|_| registry::Error::Unavailable)?
                .entry
                .as_ref()
                .is_some_and(|entry| entry.same_requester(frontend));
            if matching {
                accepted.advance(Step::Attached)?;
            }
        }
        Ok(())
    }
    pub(super) fn allow_attachment(
        &self,
        frontend: &registry::ServiceFrontend,
        foreground: &registry::FrontendMember<'_>,
        creator: &registry::ServiceIdentity,
    ) -> Result<(), registry::Error> {
        foreground.check_current()?;
        if creator.same(&frontend.identity()) {
            return Ok(());
        }
        for operation in &self.moves {
            let phase = operation
                .phase
                .try_lock()
                .map_err(|_| registry::Error::Busy)?;
            if phase.ordering.contains(Step::Entered)
                && phase
                    .entry
                    .as_ref()
                    .is_some_and(|entered| entered.same_requester(frontend))
            {
                return Ok(());
            }
        }
        Err(registry::Error::Denied)
    }
}

fn fields(service: &str, state: &str, go: Option<&str>, frontier: Option<&str>) -> Fields {
    let mut result = Fields::new();
    result.insert("service".into(), aterm_json::json!(service));
    result.insert("state".into(), aterm_json::json!(state));
    if let Some(go) = go {
        result.insert("go".into(), aterm_json::json!(go));
    }
    if let Some(frontier) = frontier {
        result.insert("frontier".into(), aterm_json::json!(frontier));
    }
    result
}
impl Delivery {
    pub(in crate::native_session_policy) fn request(
        &self,
        policy: &crate::native_session_policy::SessionPolicy,
        sources: &Arc<registry::Registry>,
        frontend: &registry::ServiceFrontend,
        master: BorrowedFd<'_>,
        operation: &str,
        action: &Action,
    ) -> Result<Fields, registry::Error> {
        if !self.live() {
            return Err(registry::Error::Stale);
        }
        frontend.current()?;
        match action {
            Action::FrontierAck { nonce } => self.acknowledge(frontend, operation, nonce),
            Action::Origin { attachment } => {
                self.origin(frontend, attachment, master)?;
                let mut result = fields(&self.service_id, "current", None, None);
                result.insert("attachment".into(), aterm_json::json!(attachment));
                Ok(result)
            }
            Action::InitialActivation(frontier) | Action::Activation(frontier) => {
                self.activation(frontend, master, frontier, false)
            }
            Action::Commit(frontier) => self.activation(frontend, master, frontier, true),
            Action::Restoration {
                go,
                old_attachment,
                new_attachment,
                frontier,
                ..
            } => self.restoration_current(
                frontend,
                master,
                go,
                old_attachment,
                new_attachment,
                frontier,
            ),
            Action::Register {
                source, attachment, ..
            } => self.register_selection(frontend, master, source, attachment),
            Action::Inhibit(selection) => self.inhibit_selection(frontend, selection),
            Action::Retire(selection) => self.retire_selection(frontend, selection),
            Action::CancelUnaccepted {
                source, attachment, ..
            } => self.cancel_unaccepted(frontend, source, attachment),
            Action::SelectionObserve(frontier) => {
                self.observe_selection(frontend, master, frontier, false)
            }
            Action::SelectionCommit(frontier) => {
                self.observe_selection(frontend, master, frontier, true)
            }
            _ => frontend.with_foreground(master, |foreground| match action {
                Action::Prepare { .. } => {
                    self.prepare_move(policy, sources, frontend, foreground, operation, action)
                }
                Action::Enter { go, callback, .. } => {
                    self.enter_move(frontend, foreground, operation, go, callback)
                }
                Action::InitialBegin { .. } => {
                    self.begin_runtime(frontend, foreground, operation, None)
                }
                Action::ReplacementBegin { go, .. } => {
                    self.begin_runtime(frontend, foreground, operation, Some(go))
                }
                Action::UiAck(frontier) => self.ui_ack(frontend, foreground, frontier),
                Action::Promote(frontier) => self.promote(frontend, foreground, frontier),
                Action::AppCurrent { app_owner } => {
                    self.app_current(frontend, foreground, app_owner)
                }
                Action::SelectionBegin { selection, go } => {
                    self.begin_selection(frontend, foreground, operation, selection, go.as_deref())
                }
                Action::SelectionUiAck(frontier) => {
                    self.ui_selection(frontend, foreground, frontier)
                }
                Action::SelectionPromote(frontier) => {
                    self.promote_selection(frontend, foreground, frontier)
                }
                _ => Err(registry::Error::Denied),
            }),
        }
    }
    fn current_human(&self) -> Result<u64, registry::Error> {
        Ok(self
            .ctx
            .upgrade()
            .ok_or(registry::Error::Stale)?
            .human_input
            .seq())
    }
    fn restoration_current(
        &self,
        backend: &registry::ServiceFrontend,
        master: BorrowedFd<'_>,
        go: &str,
        old_attachment: &str,
        new_attachment: &str,
        frontier: &str,
    ) -> Result<Fields, registry::Error> {
        let accepted = self.move_by_id(go)?;
        if accepted.attachment != old_attachment || !accepted.hold.acknowledged()? {
            return Err(registry::Error::Denied);
        }
        let runtime = self.runtime(&Frontier {
            service: self.service_id.clone(),
            go: Some(go.into()),
            frontier: frontier.into(),
        })?;
        if runtime
            .go
            .as_ref()
            .is_none_or(|actual| !Arc::ptr_eq(actual, &accepted))
            || runtime.attachment != new_attachment
            || !runtime.packet.acknowledged()?
        {
            return Err(registry::Error::Denied);
        }
        self.origin(backend, new_attachment, master)?;
        runtime.frontend.with_foreground(master, |foreground| {
            self.runtime_current(&runtime, &runtime.frontend, foreground)
        })?;
        // No UI-ACK/App tenure/effect permission is inferred. This query binds
        // only the actual old→new Native operation for Source's own restoring
        // owner, which must still match original Core/lease/capture resources.
        let mut result = fields(
            &self.service_id,
            "restoration-current",
            Some(go),
            Some(frontier),
        );
        result.insert("old_attachment".into(), aterm_json::json!(old_attachment));
        result.insert("new_attachment".into(), aterm_json::json!(new_attachment));
        Ok(result)
    }
    fn entered_current(
        &self,
        accepted: &Move,
        frontend: &registry::ServiceFrontend,
        foreground: &registry::FrontendMember<'_>,
    ) -> Result<(), registry::Error> {
        if accepted
            .phase
            .try_lock()
            .map_err(|_| registry::Error::Busy)?
            .entry
            .as_ref()
            .is_none_or(|entered| !entered.same_requester(frontend))
        {
            return Err(registry::Error::Denied);
        }
        let owned = accepted.io.try_lock().map_err(|_| registry::Error::Busy)?;
        owned
            .images
            .as_ref()
            .ok_or(registry::Error::Denied)?
            .validate()?;
        owned
            .reservation
            .as_ref()
            .ok_or(registry::Error::Denied)?
            .entered(frontend, foreground)
    }
    fn begin_runtime(
        &self,
        frontend: &registry::ServiceFrontend,
        foreground: &registry::FrontendMember<'_>,
        operation: &str,
        go: Option<&str>,
    ) -> Result<Fields, registry::Error> {
        let attachment = self.current_frontend(frontend, foreground)?;
        let accepted = if let Some(go) = go {
            let accepted = self.move_by_id(go)?;
            self.entered_current(&accepted, frontend, foreground)?;
            Some(accepted)
        } else {
            if !self.creator.same(&frontend.identity()) {
                return Err(registry::Error::Denied);
            }
            None
        };
        let existing = {
            let running = self.inner.try_lock().map_err(|_| registry::Error::Busy)?;
            running
                .lifecycle
                .runtimes
                .iter()
                .find(|runtime| runtime.operation == operation)
                .cloned()
        };
        let runtime = if let Some(runtime) = existing {
            if !runtime.frontend.same_requester(frontend)
                || runtime.attachment != attachment
                || runtime.go.as_ref().map(|entry| entry.operation.as_str()) != go
            {
                return Err(registry::Error::Denied);
            }
            runtime
        } else {
            let packet = self.packet(
                operation,
                registry::ServicePacketKind::Runtime {
                    go,
                    attachment: &attachment,
                },
            )?;
            let runtime = Arc::new(Runtime {
                operation: operation.into(),
                frontend: frontend.clone(),
                go: accepted,
                packet,
                attachment,
                human: self.current_human()?,
                phase: Mutex::new(RuntimePhase::default()),
            });
            let mut running = self.inner.try_lock().map_err(|_| registry::Error::Busy)?;
            if running.lifecycle.runtimes.len() >= CAPACITY {
                return Err(registry::Error::Busy);
            }
            running.lifecycle.runtimes.push(runtime.clone());
            runtime
        };
        runtime.packet.offer()?;
        // The SAME accepted packet is observed; no callback, constructor or
        // packet is recreated while the backend drains its original frontier.
        let deadline = Instant::now() + Duration::from_secs(60);
        retry_delivery_metadata(deadline, self, || {
            self.runtime_current(&runtime, frontend, foreground)?;
            if self.current_human()? != runtime.human {
                return Err(registry::Error::Denied);
            }
            if runtime.packet.acknowledged()? {
                Ok(())
            } else {
                Err(registry::Error::Busy)
            }
        })?;
        runtime.advance(Step::Runtime)?;
        Ok(fields(
            &self.service_id,
            if go.is_some() {
                "frontier"
            } else {
                "initial-frontier"
            },
            go,
            Some(operation),
        ))
    }
    fn runtime(&self, frontier: &Frontier) -> Result<Arc<Runtime>, registry::Error> {
        if frontier.service != self.service_id {
            return Err(registry::Error::Denied);
        }
        let running = self.inner.try_lock().map_err(|_| registry::Error::Busy)?;
        running
            .lifecycle
            .runtimes
            .iter()
            .find(|runtime| {
                runtime.operation == frontier.frontier
                    && runtime.go.as_ref().map(|entry| entry.operation.as_str())
                        == frontier.go.as_deref()
            })
            .cloned()
            .ok_or(registry::Error::Denied)
    }
    fn runtime_current(
        &self,
        runtime: &Runtime,
        frontend: &registry::ServiceFrontend,
        foreground: &registry::FrontendMember<'_>,
    ) -> Result<(), registry::Error> {
        if !runtime.frontend.same_requester(frontend)
            || self.current_frontend(frontend, foreground)? != runtime.attachment
        {
            return Err(registry::Error::Denied);
        }
        if let Some(accepted) = &runtime.go {
            self.entered_current(accepted, frontend, foreground)?;
        } else if !self.creator.same(&frontend.identity()) {
            return Err(registry::Error::Denied);
        }
        Ok(())
    }
    fn ui_ack(
        &self,
        frontend: &registry::ServiceFrontend,
        foreground: &registry::FrontendMember<'_>,
        frontier: &Frontier,
    ) -> Result<Fields, registry::Error> {
        let runtime = self.runtime(frontier)?;
        self.runtime_current(&runtime, frontend, foreground)?;
        runtime.require_app_components()?;
        if self.current_human()? != runtime.human {
            return Err(registry::Error::Denied);
        }
        if !runtime.packet.acknowledged()? {
            return Err(registry::Error::Denied);
        }
        runtime.advance(Step::Ui)?;
        runtime
            .phase
            .try_lock()
            .map_err(|_| registry::Error::Busy)?
            .ui = true;
        Ok(fields(
            &self.service_id,
            if frontier.go.is_some() {
                "activated"
            } else {
                "initial-activated"
            },
            frontier.go.as_deref(),
            Some(&frontier.frontier),
        ))
    }
    fn activation(
        &self,
        backend: &registry::ServiceFrontend,
        master: BorrowedFd<'_>,
        frontier: &Frontier,
        commit: bool,
    ) -> Result<Fields, registry::Error> {
        let runtime = self.runtime(frontier)?;
        self.origin(backend, &runtime.attachment, master)?;
        runtime.frontend.with_foreground(master, |foreground| {
            self.runtime_current(&runtime, &runtime.frontend, foreground)
        })?;
        if self.current_human()? != runtime.human {
            return Err(registry::Error::Denied);
        }
        if !runtime.packet.acknowledged()? {
            return Err(registry::Error::Denied);
        }
        let mut phase = runtime
            .phase
            .try_lock()
            .map_err(|_| registry::Error::Busy)?;
        if !phase.ui {
            return Err(registry::Error::Denied);
        }
        let app_owner = if commit {
            if runtime.go.is_none() || !phase.observed {
                return Err(registry::Error::Denied);
            }
            Some(
                phase
                    .app_owner
                    .as_ref()
                    .ok_or(registry::Error::Denied)?
                    .clone(),
            )
        } else {
            None
        };
        runtime.advance(if commit {
            Step::Committed
        } else {
            Step::Observed
        })?;
        if commit {
            phase.committed = true;
        } else {
            phase.observed = true;
        }
        let mut result = fields(
            &self.service_id,
            if commit {
                "committed"
            } else if frontier.go.is_some() {
                "activated"
            } else {
                "initial-activated"
            },
            frontier.go.as_deref(),
            Some(&frontier.frontier),
        );
        result.insert("human".into(), aterm_json::json!(runtime.human));
        if let Some(app_owner) = app_owner {
            result.insert("app_owner".into(), aterm_json::json!(app_owner));
        }
        drop(phase);
        if commit {
            runtime.seal_execution()?;
        }
        self.origin(backend, &runtime.attachment, master)?;
        runtime.frontend.with_foreground(master, |foreground| {
            self.runtime_current(&runtime, &runtime.frontend, foreground)
        })?;
        if self.current_human()? != runtime.human {
            return Err(registry::Error::Denied);
        }
        Ok(result)
    }
    fn promote(
        &self,
        frontend: &registry::ServiceFrontend,
        foreground: &registry::FrontendMember<'_>,
        frontier: &Frontier,
    ) -> Result<Fields, registry::Error> {
        let runtime = self.runtime(frontier)?;
        self.runtime_current(&runtime, frontend, foreground)?;
        runtime.require_app_components()?;
        if self.current_human()? != runtime.human {
            return Err(registry::Error::Denied);
        }
        if runtime.go.is_none() {
            return Err(registry::Error::Denied);
        }
        let mut phase = runtime
            .phase
            .try_lock()
            .map_err(|_| registry::Error::Busy)?;
        // Genuine backend activation observation precedes App promotion. A
        // Go, E1, fresh F1, packet ACK or frontend/UI RPC alone cannot promote.
        if !phase.ui || !phase.observed || !runtime.packet.acknowledged()? {
            return Err(registry::Error::Denied);
        }
        let owner = match &phase.app_owner {
            Some(owner) => owner.clone(),
            None => aterm_uds::rand::hex_token::<16>().map_err(|_| registry::Error::Unavailable)?,
        };
        runtime.advance(Step::Promoted)?;
        phase.app_owner = Some(owner.clone());
        self.publish_app(&runtime, &owner)?;
        let mut result = fields(
            &self.service_id,
            "app-current",
            frontier.go.as_deref(),
            Some(&frontier.frontier),
        );
        result.insert("app_owner".into(), aterm_json::json!(owner));
        Ok(result)
    }
    fn app_current(
        &self,
        frontend: &registry::ServiceFrontend,
        foreground: &registry::FrontendMember<'_>,
        owner: &str,
    ) -> Result<Fields, registry::Error> {
        let app = {
            self.inner
                .try_lock()
                .map_err(|_| registry::Error::Busy)?
                .lifecycle
                .app
                .clone()
                .ok_or(registry::Error::Denied)?
        };
        if app.owner != owner {
            return Err(registry::Error::Denied);
        }
        self.runtime_current(&app.runtime, frontend, foreground)?;
        let mut result = Fields::new();
        result.insert("state".into(), aterm_json::json!("app-current"));
        result.insert("app_owner".into(), aterm_json::json!(owner));
        Ok(result)
    }
    fn publish_app(&self, runtime: &Arc<Runtime>, owner: &str) -> Result<(), registry::Error> {
        let mut running = self.inner.try_lock().map_err(|_| registry::Error::Busy)?;
        if let Some(old) = &running.lifecycle.app {
            if old.owner == owner {
                return if Arc::ptr_eq(&old.runtime, runtime) {
                    Ok(())
                } else {
                    Err(registry::Error::Denied)
                };
            }
            // A historical frontier cannot supersede a later App tenure on
            // the same current physical constructor by replaying promotion.
            if old.runtime.frontend.same_requester(&runtime.frontend) {
                return Err(registry::Error::Denied);
            }
        }
        running.lifecycle.app = Some(Arc::new(AppTenure {
            owner: owner.into(),
            runtime: runtime.clone(),
        }));
        Ok(())
    }
    fn current_frontend(
        &self,
        frontend: &registry::ServiceFrontend,
        foreground: &registry::FrontendMember<'_>,
    ) -> Result<String, registry::Error> {
        if !self.live() {
            return Err(registry::Error::Stale);
        }
        foreground.check_current()?;
        let mut running = self.inner.try_lock().map_err(|_| registry::Error::Busy)?;
        let Running {
            service, delivered, ..
        } = &mut *running;
        let entry = delivered
            .iter_mut()
            .find(|entry| {
                entry.frontend.same_requester(frontend)
                    && entry.result.is_ok()
                    && entry
                        .transport
                        .as_ref()
                        .is_some_and(frontend_delivery::FrontendDelivery::received)
            })
            .ok_or(registry::Error::Denied)?;
        let service = service.as_mut().ok_or(registry::Error::Busy)?;
        let actual = service.attachment_data(&entry.receipt, foreground)?;
        if entry.attachment.as_ref() != Some(&actual) {
            return Err(registry::Error::Denied);
        }
        Ok(actual)
    }
    fn packet(
        &self,
        operation: &str,
        kind: registry::ServicePacketKind<'_>,
    ) -> Result<Arc<Packet>, registry::Error> {
        let mut running = self.inner.try_lock().map_err(|_| registry::Error::Busy)?;
        if running.lifecycle.packets.len() >= CAPACITY {
            return Err(registry::Error::Busy);
        }
        if running
            .lifecycle
            .packets
            .iter()
            .any(|packet| packet.operation == operation)
        {
            return Err(registry::Error::Denied);
        }
        let packet = Packet::new(
            operation,
            running
                .service
                .as_mut()
                .ok_or(registry::Error::Busy)?
                .prepare_packet(operation, kind)?,
        );
        running.lifecycle.packets.push(packet.clone());
        Ok(packet)
    }
    fn acknowledge(
        &self,
        backend: &registry::ServiceFrontend,
        operation: &str,
        nonce: &str,
    ) -> Result<Fields, registry::Error> {
        let packet = {
            let running = self.inner.try_lock().map_err(|_| registry::Error::Busy)?;
            running
                .lifecycle
                .packets
                .iter()
                .find(|packet| {
                    packet.operation == operation
                        && aterm_digest::ct_eq(packet.nonce.as_bytes(), nonce.as_bytes())
                })
                .cloned()
                .ok_or(registry::Error::Denied)?
        };
        // Never wait for packet IO under Running. The packet's owner retains the
        // original Control; ACK independently checks the real backend constructor.
        let mut packet = packet
            .owned
            .lock()
            .map_err(|_| registry::Error::Unavailable)?;
        let mut running = self.inner.try_lock().map_err(|_| registry::Error::Busy)?;
        running
            .service
            .as_mut()
            .ok_or(registry::Error::Busy)?
            .acknowledge_packet(&mut packet, backend, operation, nonce)?;
        Ok(Fields::new())
    }
}
