// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Actual configuration descriptions remain in the same accepted source Job.
//! Frames and readonly descriptors do not reconstruct a Trust scope/Manager.
use super::*;

pub(crate) struct ConfigurationTransfer {
    request: wire::ConfigurationAttachment,
    context: trust_resource_controller::ContextReply,
    peer: Peer,
    active: Arc<AtomicBool>,
    deadline: Instant,
}
impl ConfigurationTransfer {
    pub(crate) fn context(&self) -> &trust_resource_controller::ContextReply {
        &self.context
    }
    pub(crate) fn deadline(&self) -> Instant {
        self.deadline
    }
}

/// Retains the actual offered file and actual accepted source, independently of
/// delivery observations. Only the original handler's one-use receiving slot
/// creates this value; it grants neither parser nor materialization authority.
pub(in crate::actor::owner) struct OriginalConfiguration {
    request: wire::ConfigurationAttachment,
    source: Arc<OriginalSource>,
    description: OwnedFd,
    deadline: Instant,
    revoked: AtomicBool,
}
impl OriginalConfiguration {
    pub(super) fn binding(&self) -> &wire::ConfigurationAttachment {
        &self.request
    }
    pub(super) fn quarantine(&self) {
        self.revoked.store(true, Ordering::Release);
        self.source.revoked.store(true, Ordering::Release);
    }
    pub(in crate::actor::owner) fn deadline(&self) -> Instant {
        self.deadline
    }
    pub(in crate::actor::owner) fn check_current(&self, deadline: Instant) -> io::Result<()> {
        let deadline = deadline.min(self.deadline);
        let result = (|| {
            time(deadline)?;
            if self.revoked.load(Ordering::Acquire) {
                return Err(refused("original configuration attachment is revoked"));
            }
            self.source.check_current(deadline)?;
            wire::transport::check_configuration(
                &self.request,
                self.source.directory(deadline)?,
                self.description.as_fd(),
                deadline,
            )?;
            self.source.check_current(deadline)?;
            time(deadline)
        })();
        if result.is_err() {
            self.revoked.store(true, Ordering::Release);
            self.source.revoked.store(true, Ordering::Release);
        }
        result
    }
}

impl Preparing {
    pub(crate) fn configuration_transfer(
        &mut self,
        request: wire::ConfigurationAttachment,
    ) -> io::Result<ConfigurationTransfer> {
        request.validate()?;
        if !self.submitted
            || !self.source_submitted
            || !self.materialization_submitted
            || self.output_submitted
            || request.key != self.request.key
            || !self.active.load(Ordering::Acquire)
        {
            return Err(refused(
                "Bootstrap configuration offer is foreign or repeated",
            ));
        }
        self.configuration_budget.reserve(&request)?;
        // This immutable enrollment anchor precedes the client's remaining-time
        // capture by the ordered Prepared/Enrolled/SourceAttached exchange.
        // Never reset it on a later request, timeout or partial delivery.
        let deadline = self
            .configuration_anchor
            .checked_add(std::time::Duration::from_nanos(request.input_remaining_ns))
            .ok_or_else(|| refused("configuration input endpoint overflow"))?
            .min(self.deadline);
        time(deadline)?;
        self.deadline = self.deadline.min(deadline);
        Ok(ConfigurationTransfer {
            request,
            context: self.context.clone(),
            peer: self.peer,
            active: Arc::clone(&self.active),
            deadline,
        })
    }
}

impl Owner {
    pub(in crate::actor) fn bootstrap_graph_configuration_begin(
        &mut self,
        token: ConfigurationTransfer,
    ) -> io::Result<ConfigurationTransfer> {
        self.refresh_bootstrap_query(token.peer, &token.request.key)?;
        let context = self.bootstrap_graph_context(token.peer, &token.request.key)?;
        if context != token.context {
            return Err(refused(
                "Bootstrap context changed before configuration offer",
            ));
        }
        let root = self
            .jobs
            .get_mut(&(
                token.request.key.query.binding.session.clone(),
                token.request.key.query.binding.root_attempt,
            ))
            .and_then(|job| job.bootstrap_graph.as_mut())
            .ok_or_else(|| refused("Bootstrap configuration has no original Root"))?;
        let root_deadline = root.deadline();
        let deadline = root_deadline.min(token.deadline);
        time(deadline)?;
        root.sdk.validate()?;
        root.gnu.check()?;
        root.queries.enrollment_allowed(&token.request.key)?;
        let attempt = root
            .inputs
            .attempt
            .as_mut()
            .ok_or_else(|| refused("Bootstrap configuration has no original enrollment"))?;
        if attempt.request.key != token.request.key
            || !Arc::ptr_eq(&attempt.active, &token.active)
            || !attempt.active.load(Ordering::Acquire)
            || attempt.source.is_none()
            || attempt.configuration.len() as u64 + 1 != token.request.sequence
            || attempt
                .configuration_requested
                .as_ref()
                .is_some_and(|previous| previous.sequence != attempt.configuration.len() as u64)
            || attempt.materialization_requested.is_none()
            || attempt.workspace.is_none()
            || attempt.output_requested.is_some()
            || attempt.materialization_inputs.is_some()
        {
            return Err(refused(
                "Bootstrap configuration is foreign, unprepared or repeated",
            ));
        }
        let original_endpoint = attempt
            .configuration_anchor
            .checked_add(std::time::Duration::from_nanos(
                token.request.input_remaining_ns,
            ))
            .ok_or_else(|| refused("configuration input endpoint overflow"))?
            .min(root_deadline);
        if original_endpoint != token.deadline {
            return Err(refused(
                "configuration input endpoint is not the original bound",
            ));
        }
        token.request.validate()?;
        // Charge the declared bytes BEFORE receiving or validating contents.
        // Any later refusal keeps this reservation and original native slot.
        attempt.configuration_budget.reserve(&token.request)?;
        attempt.configuration_requested = Some(token.request.clone());
        // Only the earlier native preparation phase creates this workspace,
        // before the sender captures any configuration bytes. A configuration
        // frame can never retroactively create or adopt that original.
        attempt
            .workspace
            .as_ref()
            .expect("same prepared workspace")
            .check_current()?;
        time(deadline)?;
        Ok(token)
    }

    pub(in crate::actor) fn bootstrap_graph_configuration_retain(
        &mut self,
        token: ConfigurationTransfer,
        description: OwnedFd,
    ) -> io::Result<trust_resource_controller::ContextReply> {
        let context = self.bootstrap_graph_context(token.peer, &token.request.key);
        let root = self
            .jobs
            .get_mut(&(
                token.request.key.query.binding.session.clone(),
                token.request.key.query.binding.root_attempt,
            ))
            .and_then(|job| job.bootstrap_graph.as_mut());
        let Some(root) = root else {
            std::mem::forget(description);
            return Err(refused("delivered configuration lost its original Root"));
        };
        let deadline = root.deadline().min(token.deadline);
        let Some(attempt) = root.inputs.attempt.as_mut() else {
            std::mem::forget(description);
            return Err(refused("delivered configuration lost its enrollment"));
        };
        if attempt.configuration_requested.as_ref() != Some(&token.request)
            || !Arc::ptr_eq(&attempt.active, &token.active)
            || attempt.configuration.len() as u64 + 1 != token.request.sequence
            || attempt.materialization_inputs.is_some()
        {
            attempt.configuration_rejected.push(description);
            return Err(refused(
                "Bootstrap configuration transfer is foreign or repeated",
            ));
        }
        let Some(source) = attempt.source.as_ref() else {
            std::mem::forget(description);
            return Err(refused(
                "delivered configuration lost its actual original source",
            ));
        };
        let original = Arc::new(OriginalConfiguration {
            request: token.request,
            source: Arc::clone(source),
            description,
            deadline,
            revoked: AtomicBool::new(false),
        });
        // Install the real delivered resource BEFORE any content read or
        // context postcheck. Failure cannot drop it out of native settlement.
        attempt.configuration.push(Arc::clone(&original));
        let result = (|| {
            attempt
                .workspace
                .as_ref()
                .ok_or_else(|| refused("configuration has no original fresh workspace"))?
                .check_current()?;
            original.check_current(deadline)?;
            let context = context?;
            if context != token.context {
                return Err(refused(
                    "Bootstrap context changed during configuration offer",
                ));
            }
            attempt
                .workspace
                .as_ref()
                .expect("same original workspace")
                .check_current()?;
            Ok(context)
        })();
        if result.is_err() {
            original.revoked.store(true, Ordering::Release);
        }
        result
    }
}

#[cfg(test)]
#[path = "configuration/tests.rs"]
mod tests;

#[cfg(test)]
#[path = "materialization_inputs/tests.rs"]
mod materialization_inputs_tests;
