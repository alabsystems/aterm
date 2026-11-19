// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0

use super::*;
use std::os::unix::ffi::OsStrExt;

impl Delivery {
    pub(super) fn prepare_move(
        &self,
        policy: &crate::native_session_policy::SessionPolicy,
        sources: &Arc<registry::Registry>,
        frontend: &registry::ServiceFrontend,
        foreground: &registry::FrontendMember<'_>,
        operation: &str,
        request: &Action,
    ) -> Result<Fields, registry::Error> {
        let Action::Prepare {
            source_build,
            target_build,
            settings,
            input,
            command,
            ..
        } = request
        else {
            return Err(registry::Error::Denied);
        };
        let attachment = self.current_frontend(frontend, foreground)?;
        let existing = {
            let mut running = self.inner.try_lock().map_err(|_| registry::Error::Busy)?;
            running.lifecycle.prune_completed();
            let lifecycle = &running.lifecycle;
            if lifecycle.moves.iter().any(|accepted| {
                accepted.operation != operation && accepted.original.same_requester(frontend)
            }) {
                return Err(registry::Error::Denied);
            }
            if !self.creator.same(&frontend.identity())
                && lifecycle
                    .app
                    .as_ref()
                    .is_none_or(|app| !app.runtime.frontend.same_requester(frontend))
            {
                return Err(registry::Error::Denied);
            }
            lifecycle
                .moves
                .iter()
                .find(|entry| entry.operation == operation)
                .cloned()
        };
        let accepted = if let Some(accepted) = existing {
            if accepted.request != *request || !accepted.original.same_requester(frontend) {
                return Err(registry::Error::Denied);
            }
            accepted
        } else {
            let hold = self.packet(operation, registry::ServicePacketKind::Hold)?;
            let accepted = Arc::new(Move {
                operation: operation.into(),
                original: frontend.clone(),
                request: request.clone(),
                hold,
                attachment,
                phase: Mutex::new(MovePhase::default()),
                io: Mutex::new(MoveIo::default()),
            });
            let mut running = self.inner.try_lock().map_err(|_| registry::Error::Busy)?;
            if running.lifecycle.moves.len() >= CAPACITY {
                return Err(registry::Error::Busy);
            }
            running.lifecycle.moves.push(accepted.clone());
            accepted
        };
        {
            let mut owned = accepted.io.try_lock().map_err(|_| registry::Error::Busy)?;
            if owned.images.is_none() {
                if owned.image_attempted {
                    return Err(registry::Error::Unavailable);
                }
                owned.image_attempted = true;
                owned.images = Some(replacement::Images::resolve(
                    policy,
                    frontend.process(),
                    *source_build,
                    *target_build,
                )?);
            }
            owned
                .images
                .as_ref()
                .ok_or(registry::Error::Unavailable)?
                .validate()?;
        }
        self.current_frontend(frontend, foreground)?;
        accepted.hold.offer()?;
        if !accepted.hold.acknowledged()? {
            return Ok(fields(&self.service_id, "holding", Some(operation), None));
        }
        accepted.advance(Step::Held)?;
        if accepted
            .phase
            .try_lock()
            .map_err(|_| registry::Error::Busy)?
            .go
        {
            return Ok(fields(&self.service_id, "go", Some(operation), None));
        }
        let mut owned = accepted.io.try_lock().map_err(|_| registry::Error::Busy)?;
        if owned.attempted {
            return Err(registry::Error::Unavailable);
        }
        owned.attempted = true; // retained before callbacks/journal/possible grant.
        let deadline = Instant::now() + Duration::from_secs(60);
        self.current_frontend(frontend, foreground)?;
        owned
            .images
            .as_ref()
            .ok_or(registry::Error::Unavailable)?
            .validate()?;
        let socket = connect_callback(settings, frontend.process(), deadline)?;
        let snapshot = replacement::receive_settings(&socket, deadline)?;
        drop(socket);
        self.current_frontend(frontend, foreground)?;
        let socket = connect_callback(input, frontend.process(), deadline)?;
        owned.input = Some(replacement::receive_input(&socket, deadline)?);
        drop(socket);
        self.current_frontend(frontend, foreground)?;
        let target = owned
            .images
            .as_ref()
            .ok_or(registry::Error::Unavailable)?
            .target()
            .clone();
        owned.reservation = Some(sources.reserve_frontend_exec(
            frontend,
            foreground,
            &target,
            &snapshot.environment,
        )?);
        let reservation = owned
            .reservation
            .as_ref()
            .ok_or(registry::Error::Unavailable)?;
        let cwd = aterm_sysprobe::process_cwd(frontend.process().pid)
            .ok_or(registry::Error::Unavailable)?;
        if !cwd.is_absolute() {
            return Err(registry::Error::Unavailable);
        }
        let mut environment = snapshot.environment;
        environment.retain(|(name, _)| name != registry::LAUNCH_ENV);
        environment.push((registry::LAUNCH_ENV.into(), reservation.token_data().into()));
        let argv: Vec<Vec<u8>> = [
            OsString::from("--alab-retained-frontend"),
            operation.into(),
            self.service_id.clone().into(),
        ]
        .into_iter()
        .chain(snapshot.arguments)
        .map(|word| word.as_bytes().to_vec())
        .collect();
        let env: Vec<(Vec<u8>, Vec<u8>)> = environment
            .iter()
            .map(|(key, value)| (key.as_bytes().to_vec(), value.as_bytes().to_vec()))
            .collect();
        let body = aterm_json::to_vec(&aterm_json::json!({"schema":1,"program":target.path,
            "argv":argv,"env":env,"cwd":cwd.as_os_str().as_bytes()}))
        .map_err(|_| registry::Error::Unavailable)?;
        let capability = owned
            .reservation
            .as_ref()
            .ok_or(registry::Error::Unavailable)?
            .token_data()
            .to_owned();
        // Own the complete command before recording/granting possible execution.
        // Preparation can fail without ever offering a G1 byte.
        let callback = connect_callback(command, frontend.process(), deadline)?;
        owned.command = Some(body_delivery::BodyDelivery::new(
            callback,
            body_delivery::Tag::Command,
            operation,
            &body,
        )?);
        owned.journal = Some(
            aterm_agent::harness::native_receipts::RetainedExecution::prepare(
                &self.service_id,
                &self.creator,
                frontend,
                operation,
                &capability,
                &target,
                &body,
            )
            .map_err(|_| registry::Error::Unavailable)?,
        );
        owned
            .journal
            .as_ref()
            .ok_or(registry::Error::Unavailable)?
            .may_execute()
            .map_err(|_| registry::Error::Unavailable)?;
        accepted.advance(Step::Recorded)?;
        self.current_frontend(frontend, foreground)?;
        owned
            .images
            .as_ref()
            .ok_or(registry::Error::Unavailable)?
            .validate()?;
        owned
            .reservation
            .as_ref()
            .ok_or(registry::Error::Unavailable)?
            .grant(frontend, foreground)?;
        accepted.advance(Step::Granted)?;
        let MoveIo {
            images, command, ..
        } = &mut *owned;
        command
            .as_mut()
            .ok_or(registry::Error::Unavailable)?
            .exchange(deadline, || {
                self.current_frontend(frontend, foreground)?;
                images
                    .as_ref()
                    .ok_or(registry::Error::Unavailable)?
                    .validate()
            })?;
        accepted.advance(Step::Command)?;
        accepted
            .phase
            .lock()
            .map_err(|_| registry::Error::Unavailable)?
            .go = true;
        Ok(fields(&self.service_id, "go", Some(operation), None))
    }
    pub(super) fn enter_move(
        &self,
        frontend: &registry::ServiceFrontend,
        foreground: &registry::FrontendMember<'_>,
        operation: &str,
        go: &str,
        callback: &std::path::Path,
    ) -> Result<Fields, registry::Error> {
        let accepted = self.move_by_id(go)?;
        if !accepted
            .phase
            .try_lock()
            .map_err(|_| registry::Error::Busy)?
            .go
        {
            return Err(registry::Error::Denied);
        }
        let mut owned = accepted.io.try_lock().map_err(|_| registry::Error::Busy)?;
        owned
            .images
            .as_ref()
            .ok_or(registry::Error::Denied)?
            .validate()?;
        owned
            .reservation
            .as_ref()
            .ok_or(registry::Error::Denied)?
            .entered(frontend, foreground)?;
        if let Some(entry) = &owned.entry {
            if entry.operation != operation
                || entry.callback != callback
                || !entry.frontend.same_requester(frontend)
            {
                return Err(registry::Error::Denied);
            }
            return if entry.delivery.received() {
                Ok(fields(&self.service_id, "entered", Some(go), None))
            } else {
                Err(registry::Error::Unavailable)
            };
        }
        let deadline = Instant::now() + Duration::from_secs(60);
        let socket = connect_callback(callback, frontend.process(), deadline)?;
        let input = owned.input.as_ref().ok_or(registry::Error::Unavailable)?;
        owned.entry = Some(Entry {
            operation: operation.into(),
            callback: callback.into(),
            frontend: frontend.clone(),
            delivery: body_delivery::BodyDelivery::new(
                socket,
                body_delivery::Tag::Entry,
                go,
                input,
            )?,
        });
        // Entropy/socket preparation failures leave the original input held.
        // Only the complete Entry now owns the exact E1 frame.
        owned.input = None;
        let MoveIo {
            reservation,
            entry,
            images,
            ..
        } = &mut *owned;
        let reservation = reservation.as_ref().ok_or(registry::Error::Unavailable)?;
        let entry = entry.as_mut().ok_or(registry::Error::Unavailable)?;
        entry.delivery.exchange(deadline, || {
            if !self.live() {
                return Err(registry::Error::Stale);
            }
            images
                .as_ref()
                .ok_or(registry::Error::Unavailable)?
                .validate()?;
            reservation.entered(frontend, foreground)
        })?;
        accepted.advance(Step::Entered)?;
        accepted
            .phase
            .lock()
            .map_err(|_| registry::Error::Unavailable)?
            .entry = Some(frontend.clone());
        Ok(fields(&self.service_id, "entered", Some(go), None))
    }
    pub(super) fn move_by_id(&self, go: &str) -> Result<Arc<Move>, registry::Error> {
        let running = self.inner.try_lock().map_err(|_| registry::Error::Busy)?;
        running
            .lifecycle
            .moves
            .iter()
            .find(|entry| entry.operation == go)
            .cloned()
            .ok_or(registry::Error::Denied)
    }
}
