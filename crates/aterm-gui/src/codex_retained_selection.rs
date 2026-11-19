// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0

use super::*;

impl Delivery {
    fn backend_current(&self, backend: &registry::ServiceFrontend) -> Result<(), registry::Error> {
        self.inner
            .try_lock()
            .map_err(|_| registry::Error::Busy)?
            .service
            .as_mut()
            .ok_or(registry::Error::Busy)?
            .backend_current(backend)
    }
    fn selection_reply(&self, source: &SourceSelection, state: &str) -> Fields {
        let mut result = fields(&self.service_id, state, None, None);
        result.insert("source_operation".into(), aterm_json::json!(source.source));
        result.insert("selection".into(), aterm_json::json!(source.selection));
        result
    }
    pub(super) fn register_selection(
        &self,
        backend: &registry::ServiceFrontend,
        master: BorrowedFd<'_>,
        source: &str,
        attachment: &str,
    ) -> Result<Fields, registry::Error> {
        self.origin(backend, attachment, master)?;
        let mut running = self.inner.try_lock().map_err(|_| registry::Error::Busy)?;
        if let Some(registered) = running
            .lifecycle
            .selections
            .iter()
            .find(|entry| entry.source == source)
        {
            if !registered.backend.same_requester(backend)
                || registered.attachment != attachment
                || !registered.registered
                || registered
                    .phase
                    .try_lock()
                    .map_err(|_| registry::Error::Busy)?
                    .retired
            {
                return Err(registry::Error::Denied);
            }
            return Ok(self.selection_reply(registered, "registered"));
        }
        if running.lifecycle.selections.len() >= CAPACITY {
            return Err(registry::Error::Busy);
        }
        let original = running
            .delivered
            .iter()
            .find(|entry| {
                entry.attachment.as_deref() == Some(attachment)
                    && entry.result.is_ok()
                    && entry
                        .transport
                        .as_ref()
                        .is_some_and(frontend_delivery::FrontendDelivery::received)
            })
            .ok_or(registry::Error::Denied)?
            .frontend
            .clone();
        let registered = Arc::new(SourceSelection {
            source: source.into(),
            selection: aterm_uds::rand::hex_token::<16>()
                .map_err(|_| registry::Error::Unavailable)?,
            backend: backend.clone(),
            original,
            attachment: attachment.into(),
            registered: true,
            phase: Mutex::new(SelectionPhase::default()),
        });
        running.lifecycle.selections.push(registered.clone());
        Ok(self.selection_reply(&registered, "registered"))
    }
    fn selection(
        &self,
        selectors: &SelectionData,
    ) -> Result<Arc<SourceSelection>, registry::Error> {
        if selectors.service != self.service_id {
            return Err(registry::Error::Denied);
        }
        self.inner
            .try_lock()
            .map_err(|_| registry::Error::Busy)?
            .lifecycle
            .selections
            .iter()
            .find(|entry| {
                entry.source == selectors.source
                    && entry.selection == selectors.selection
                    && entry.registered
            })
            .cloned()
            .ok_or(registry::Error::Denied)
    }
    pub(super) fn inhibit_selection(
        &self,
        backend: &registry::ServiceFrontend,
        selectors: &SelectionData,
    ) -> Result<Fields, registry::Error> {
        self.backend_current(backend)?;
        let selection = self.selection(selectors)?;
        if !selection.backend.same_requester(backend) {
            return Err(registry::Error::Denied);
        }
        let existing = {
            let phase = selection
                .phase
                .try_lock()
                .map_err(|_| registry::Error::Busy)?;
            if phase.retired {
                return Err(registry::Error::Denied);
            }
            phase.inhibit.clone()
        };
        let packet = if let Some(packet) = existing {
            packet
        } else {
            let packet = self.packet(
                &selection.selection,
                registry::ServicePacketKind::SourceInhibit {
                    parent: &selection.source,
                    attachment: &selection.attachment,
                },
            )?;
            let mut phase = selection
                .phase
                .try_lock()
                .map_err(|_| registry::Error::Busy)?;
            if phase.retired {
                return Err(registry::Error::Denied);
            }
            phase.inhibit = Some(packet.clone());
            packet
        };
        packet.offer()?;
        Ok(self.selection_reply(
            &selection,
            if packet.acknowledged()? {
                "inhibited"
            } else {
                "waiting"
            },
        ))
    }
    pub(super) fn retire_selection(
        &self,
        backend: &registry::ServiceFrontend,
        selectors: &SelectionData,
    ) -> Result<Fields, registry::Error> {
        self.backend_current(backend)?;
        let selection = self.selection(selectors)?;
        if !selection.backend.same_requester(backend) {
            return Err(registry::Error::Denied);
        }
        // Original accepted Source owner may clean up after C1/F1 withdrawal.
        // This retires selection use only, not possible exec or original holds.
        selection
            .phase
            .try_lock()
            .map_err(|_| registry::Error::Busy)?
            .retired = true;
        Ok(self.selection_reply(&selection, "retired"))
    }
    pub(super) fn cancel_unaccepted(
        &self,
        backend: &registry::ServiceFrontend,
        source: &str,
        attachment: &str,
    ) -> Result<Fields, registry::Error> {
        self.backend_current(backend)?;
        let mut running = self.inner.try_lock().map_err(|_| registry::Error::Busy)?;
        let existing = running
            .lifecycle
            .selections
            .iter()
            .find(|entry| entry.source == source)
            .cloned();
        let (state, selection) = if let Some(selection) = existing {
            if !selection.backend.same_requester(backend) || selection.attachment != attachment {
                return Err(registry::Error::Denied);
            }
            let mut phase = selection
                .phase
                .try_lock()
                .map_err(|_| registry::Error::Busy)?;
            if phase.inhibit.is_some() {
                return Err(registry::Error::Denied);
            }
            phase.retired = true;
            (
                if selection.registered {
                    "unaccepted-retired"
                } else {
                    "unaccepted-absent"
                },
                Some(selection.clone()),
            )
        } else {
            if running.lifecycle.selections.len() >= CAPACITY {
                return Err(registry::Error::Busy);
            }
            let original = running
                .delivered
                .iter()
                .find(|entry| {
                    entry.attachment.as_deref() == Some(attachment)
                        && entry.result.is_ok()
                        && entry
                            .transport
                            .as_ref()
                            .is_some_and(frontend_delivery::FrontendDelivery::received)
                })
                .ok_or(registry::Error::Denied)?
                .frontend
                .clone();
            // Fence THIS exact original backend/source before replying absent;
            // a delayed registration on the same controller cannot revive it.
            running.lifecycle.selections.push(Arc::new(SourceSelection {
                source: source.into(),
                selection: aterm_uds::rand::hex_token::<16>()
                    .map_err(|_| registry::Error::Unavailable)?,
                backend: backend.clone(),
                original,
                attachment: attachment.into(),
                registered: false,
                phase: Mutex::new(SelectionPhase {
                    retired: true,
                    ..SelectionPhase::default()
                }),
            }));
            ("unaccepted-absent", None)
        };
        let mut result = fields(&self.service_id, state, None, None);
        result.insert("source_operation".into(), aterm_json::json!(source));
        result.insert("attachment".into(), aterm_json::json!(attachment));
        if state == "unaccepted-retired" {
            result.insert(
                "selection".into(),
                aterm_json::json!(selection.ok_or(registry::Error::Unavailable)?.selection),
            );
        }
        Ok(result)
    }
    pub(super) fn begin_selection(
        &self,
        frontend: &registry::ServiceFrontend,
        foreground: &registry::FrontendMember<'_>,
        operation: &str,
        selectors: &SelectionData,
        go: Option<&str>,
    ) -> Result<Fields, registry::Error> {
        let source = self.selection(selectors)?;
        let inhibit = {
            let phase = source.phase.try_lock().map_err(|_| registry::Error::Busy)?;
            if phase.retired {
                return Err(registry::Error::Denied);
            }
            phase.inhibit.clone().ok_or(registry::Error::Denied)?
        };
        if !inhibit.acknowledged()? {
            return Err(registry::Error::Denied);
        }
        let attachment = self.current_frontend(frontend, foreground)?;
        let accepted = if let Some(go) = go {
            let accepted = self.move_by_id(go)?;
            self.entered_current(&accepted, frontend, foreground)?;
            if accepted.attachment != source.attachment
                || !accepted.original.same_requester(&source.original)
            {
                return Err(registry::Error::Denied);
            }
            Some(accepted)
        } else {
            if !source.original.same_requester(frontend) || !self.creator.same(&frontend.identity())
            {
                return Err(registry::Error::Denied);
            }
            None
        };
        let existing = {
            self.inner
                .try_lock()
                .map_err(|_| registry::Error::Busy)?
                .lifecycle
                .selected
                .iter()
                .find(|entry| entry.runtime.operation == operation)
                .cloned()
        };
        let selected = if let Some(existing) = existing {
            if !Arc::ptr_eq(&existing.source, &source)
                || !existing.runtime.frontend.same_requester(frontend)
                || existing
                    .runtime
                    .go
                    .as_ref()
                    .map(|entry| entry.operation.as_str())
                    != go
                || existing.runtime.attachment != attachment
            {
                return Err(registry::Error::Denied);
            }
            existing
        } else {
            let packet = self.packet(
                operation,
                registry::ServicePacketKind::SourceSelection {
                    parent: &source.selection,
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
            let selected = Arc::new(SelectedRuntime { source, runtime });
            let mut running = self.inner.try_lock().map_err(|_| registry::Error::Busy)?;
            if running.lifecycle.selected.len() >= CAPACITY {
                return Err(registry::Error::Busy);
            }
            running.lifecycle.selected.push(selected.clone());
            selected
        };
        selected.runtime.packet.offer()?;
        retry_delivery_metadata(Instant::now() + Duration::from_secs(60), self, || {
            self.selected_current(&selected, frontend, foreground)?;
            if selected.runtime.packet.acknowledged()? {
                Ok(())
            } else {
                Err(registry::Error::Busy)
            }
        })?;
        selected.runtime.advance(Step::Runtime)?;
        Ok(self.selected_reply(&selected, "selection-frontier"))
    }
    fn selected(
        &self,
        selectors: &SelectionFrontier,
    ) -> Result<Arc<SelectedRuntime>, registry::Error> {
        let source = self.selection(&selectors.selection)?;
        self.inner
            .try_lock()
            .map_err(|_| registry::Error::Busy)?
            .lifecycle
            .selected
            .iter()
            .find(|selected| {
                Arc::ptr_eq(&selected.source, &source)
                    && selected.runtime.operation == selectors.frontier
                    && selected
                        .runtime
                        .go
                        .as_ref()
                        .map(|entry| entry.operation.as_str())
                        == selectors.go.as_deref()
            })
            .cloned()
            .ok_or(registry::Error::Denied)
    }
    fn selected_reply(&self, selected: &SelectedRuntime, state: &str) -> Fields {
        let mut result = self.selection_reply(&selected.source, state);
        result.insert(
            "frontier".into(),
            aterm_json::json!(selected.runtime.operation),
        );
        if let Some(go) = &selected.runtime.go {
            result.insert("go".into(), aterm_json::json!(go.operation));
        }
        result
    }
    fn selected_current(
        &self,
        selected: &SelectedRuntime,
        frontend: &registry::ServiceFrontend,
        foreground: &registry::FrontendMember<'_>,
    ) -> Result<(), registry::Error> {
        if selected
            .source
            .phase
            .try_lock()
            .map_err(|_| registry::Error::Busy)?
            .retired
        {
            return Err(registry::Error::Denied);
        }
        self.runtime_current(&selected.runtime, frontend, foreground)
    }
    pub(super) fn ui_selection(
        &self,
        frontend: &registry::ServiceFrontend,
        foreground: &registry::FrontendMember<'_>,
        selectors: &SelectionFrontier,
    ) -> Result<Fields, registry::Error> {
        let selected = self.selected(selectors)?;
        self.selected_current(&selected, frontend, foreground)?;
        selected.runtime.require_app_components()?;
        if !selected.runtime.packet.acknowledged()?
            || self.current_human()? != selected.runtime.human
        {
            return Err(registry::Error::Denied);
        }
        selected.runtime.advance(Step::Ui)?;
        selected
            .runtime
            .phase
            .try_lock()
            .map_err(|_| registry::Error::Busy)?
            .ui = true;
        Ok(self.selected_reply(&selected, "selection-activated"))
    }
    pub(super) fn observe_selection(
        &self,
        backend: &registry::ServiceFrontend,
        master: BorrowedFd<'_>,
        selectors: &SelectionFrontier,
        commit: bool,
    ) -> Result<Fields, registry::Error> {
        let selected = self.selected(selectors)?;
        if !selected.source.backend.same_requester(backend) {
            return Err(registry::Error::Denied);
        }
        self.origin(backend, &selected.runtime.attachment, master)?;
        selected
            .runtime
            .frontend
            .with_foreground(master, |foreground| {
                self.selected_current(&selected, &selected.runtime.frontend, foreground)
            })?;
        if !selected.runtime.packet.acknowledged()?
            || self.current_human()? != selected.runtime.human
        {
            return Err(registry::Error::Denied);
        }
        let mut phase = selected
            .runtime
            .phase
            .try_lock()
            .map_err(|_| registry::Error::Busy)?;
        if !phase.ui {
            return Err(registry::Error::Denied);
        }
        let owner = if commit && selected.runtime.go.is_some() {
            if !phase.observed {
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
        selected.runtime.advance(if commit {
            Step::Committed
        } else {
            Step::Observed
        })?;
        if commit {
            phase.committed = true;
        } else {
            phase.observed = true;
        }
        let mut result = self.selected_reply(
            &selected,
            if commit {
                "selection-committed"
            } else {
                "selection-observed"
            },
        );
        result.insert("human".into(), aterm_json::json!(selected.runtime.human));
        if let Some(owner) = owner {
            result.insert("app_owner".into(), aterm_json::json!(owner));
        }
        drop(phase);
        if commit {
            selected.runtime.seal_execution()?;
        }
        self.origin(backend, &selected.runtime.attachment, master)?;
        selected
            .runtime
            .frontend
            .with_foreground(master, |foreground| {
                self.selected_current(&selected, &selected.runtime.frontend, foreground)
            })?;
        if self.current_human()? != selected.runtime.human {
            return Err(registry::Error::Denied);
        }
        Ok(result)
    }
    pub(super) fn promote_selection(
        &self,
        frontend: &registry::ServiceFrontend,
        foreground: &registry::FrontendMember<'_>,
        selectors: &SelectionFrontier,
    ) -> Result<Fields, registry::Error> {
        let selected = self.selected(selectors)?;
        self.selected_current(&selected, frontend, foreground)?;
        selected.runtime.require_app_components()?;
        if selected.runtime.go.is_none() {
            return Err(registry::Error::Denied);
        }
        if self.current_human()? != selected.runtime.human {
            return Err(registry::Error::Denied);
        }
        let mut phase = selected
            .runtime
            .phase
            .try_lock()
            .map_err(|_| registry::Error::Busy)?;
        if !phase.ui || !phase.observed || !selected.runtime.packet.acknowledged()? {
            return Err(registry::Error::Denied);
        }
        let owner = match &phase.app_owner {
            Some(owner) => owner.clone(),
            None => aterm_uds::rand::hex_token::<16>().map_err(|_| registry::Error::Unavailable)?,
        };
        selected.runtime.advance(Step::Promoted)?;
        phase.app_owner = Some(owner.clone());
        self.publish_app(&selected.runtime, &owner)?;
        let mut result = self.selected_reply(&selected, "selection-app-current");
        result.insert("app_owner".into(), aterm_json::json!(owner));
        Ok(result)
    }
}
