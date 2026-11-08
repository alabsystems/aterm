// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Close-only copies of the actual producer's descriptions, owned by the
//! original Job. Neither the wire request nor a reply grants execution,
//! SDK custody, a build-lease release right, or a completed runtime.

use super::*;
use std::{
    os::fd::{AsFd, OwnedFd},
    sync::{
        Arc,
        atomic::{AtomicBool, Ordering},
    },
};
use trust_resource_controller::runtime_input_retention as wire;

#[derive(Default)]
pub(in crate::actor::owner) struct State {
    attempt: Option<Attempt>,
    held: Option<Held>,
}

struct Attempt {
    binding: Binding,
    group: i32,
    leader_serial: u64,
    birth: u64,
    active: Arc<AtomicBool>,
}

struct Held {
    // Preserve the complete original description census. This metadata is an
    // observation, never an SDK constructor or a release capability.
    _request: wire::Request,
    _descriptions: Vec<OwnedFd>,
    _transport: OwnedFd,
}

/// A private, non-cloneable preparation owner. Only the original Job can mint
/// it. The actual handler holds it through all receive/validation work; wire
/// EOF, End, ACK and sender death do not mark that work finished.
pub(crate) struct Preparing {
    request: wire::Request,
    context: trust_resource_controller::ContextReply,
    peer: Peer,
    active: Arc<AtomicBool>,
    submitted: bool,
}

/// A single attachment request from the actual local preparation guard.
/// Dropping an actor message or reply cannot retire the handler's guard.
pub(crate) struct Attachment {
    request: wire::Request,
    context: trust_resource_controller::ContextReply,
    peer: Peer,
    active: Arc<AtomicBool>,
}

impl Drop for Preparing {
    fn drop(&mut self) {
        self.active.store(false, Ordering::Release);
    }
}

impl Preparing {
    pub(crate) fn context(&self) -> &trust_resource_controller::ContextReply {
        &self.context
    }

    pub(crate) fn attachment(&mut self) -> io::Result<Attachment> {
        if self.submitted || !self.active.load(Ordering::Acquire) {
            return Err(refused("runtime input attachment was already submitted"));
        }
        self.submitted = true;
        Ok(Attachment {
            request: self.request.clone(),
            context: self.context.clone(),
            peer: self.peer,
            active: Arc::clone(&self.active),
        })
    }
}

impl State {
    pub(super) fn preparation_finished(&self) -> bool {
        self.attempt
            .as_ref()
            .is_none_or(|attempt| !attempt.active.load(Ordering::Acquire))
    }

    /// Only the live original root's positive closure can discard its input
    /// copies. Drop closes descriptions; it never issues LOCK_UN on an OFD
    /// shared with the producer.
    pub(super) fn release(&mut self, closed: &settlement::ClosedRoot) -> io::Result<()> {
        if let Some(attempt) = &self.attempt
            && (attempt.active.load(Ordering::Acquire)
                || !closed.matches_original(
                    &attempt.binding,
                    attempt.group,
                    attempt.leader_serial,
                    attempt.birth,
                ))
        {
            return Err(refused(
                "runtime inputs still have original preparation ownership",
            ));
        }
        self.held.take();
        Ok(())
    }
}

impl Owner {
    pub(in crate::actor) fn runtime_inputs_begin(
        &mut self,
        peer: Peer,
        request: wire::Request,
    ) -> io::Result<Preparing> {
        request.validate()?;
        // Authenticate the actual peer and current coverage with the original
        // owner. No context reconstructed from the request is accepted.
        let context = self.context(peer, &request.query)?;
        let job = self
            .jobs
            .get_mut(&(
                request.query.binding.session.clone(),
                request.query.binding.root_attempt,
            ))
            .ok_or_else(|| refused("runtime input root unavailable"))?;
        if job.runtime_inputs.attempt.is_some() || job.runtime_inputs.held.is_some() {
            return Err(refused("runtime input transfer was already attempted"));
        }
        let active = Arc::new(AtomicBool::new(true));
        job.runtime_inputs.attempt = Some(Attempt {
            binding: job.binding.clone(),
            group: job.child.cohort().group(),
            leader_serial: job.child.cohort().leader_serial(),
            birth: job.root_birth,
            active: Arc::clone(&active),
        });
        Ok(Preparing {
            request,
            context,
            peer,
            active,
            submitted: false,
        })
    }

    pub(in crate::actor) fn runtime_inputs_retain(
        &mut self,
        preparing: Attachment,
        descriptions: Vec<OwnedFd>,
        transport: OwnedFd,
    ) -> io::Result<trust_resource_controller::ContextReply> {
        wire::transport::check_inputs(
            &preparing.request,
            &descriptions.iter().map(AsFd::as_fd).collect::<Vec<_>>(),
        )?;
        let current = self.context(preparing.peer, &preparing.request.query)?;
        if current != preparing.context || descriptions.len() != preparing.request.inputs.len() {
            return Err(refused(
                "runtime input transfer changed its original live context",
            ));
        }
        let job = self
            .jobs
            .get_mut(&(
                preparing.request.query.binding.session.clone(),
                preparing.request.query.binding.root_attempt,
            ))
            .ok_or_else(|| refused("runtime input root unavailable"))?;
        if job.runtime_inputs.held.is_some()
            || job.runtime_inputs.attempt.as_ref().is_none_or(|attempt| {
                !Arc::ptr_eq(&attempt.active, &preparing.active)
                    || !attempt.active.load(Ordering::Acquire)
                    || attempt.binding != job.binding
                    || attempt.group != job.child.cohort().group()
                    || attempt.leader_serial != job.child.cohort().leader_serial()
                    || attempt.birth != job.root_birth
            })
        {
            return Err(refused(
                "runtime input preparation is foreign to its original Job",
            ));
        }
        job.runtime_inputs.held = Some(Held {
            _request: preparing.request.clone(),
            _descriptions: descriptions,
            _transport: transport,
        });
        // The unique preparation guard is still in the actual handler.
        // Neither a successful reply nor a dropped receiver retires it.
        Ok(current)
    }
}
