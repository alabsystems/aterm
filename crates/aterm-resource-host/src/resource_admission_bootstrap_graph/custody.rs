// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Close-only graph input copies retained on the SAME original Job. Activity
//! belongs to the actual receiving handler, not an actor message or reply.
use super::*;

#[derive(Default)]
pub(super) struct State {
    attempt: Option<Attempt>,
}
struct Attempt {
    request: wire::Enrollment,
    binding: Binding,
    group: i32,
    leader_serial: u64,
    birth: u64,
    active: Arc<AtomicBool>,
    gnu: Option<[OwnedFd; 2]>,
    control: Option<OwnedFd>,
    output_requested: Option<wire::OutputAttachment>,
    output: Option<[OwnedFd; 2]>,
}
pub(crate) struct Support {
    pub(crate) context: trust_resource_controller::ContextReply,
    pub(crate) deadline: Instant,
}
pub(crate) struct Preparing {
    request: wire::Enrollment,
    context: trust_resource_controller::ContextReply,
    peer: Peer,
    active: Arc<AtomicBool>,
    deadline: Instant,
    submitted: bool,
    output_submitted: bool,
}
pub(crate) struct EnrollmentAttachment {
    request: wire::Enrollment,
    context: trust_resource_controller::ContextReply,
    peer: Peer,
    active: Arc<AtomicBool>,
}
pub(crate) struct OutputTransfer {
    request: wire::OutputAttachment,
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
    pub(crate) fn deadline(&self) -> Instant {
        self.deadline
    }
    pub(crate) fn enrollment_attachment(&mut self) -> io::Result<EnrollmentAttachment> {
        if self.submitted || !self.active.load(Ordering::Acquire) {
            return Err(refused(
                "Bootstrap enrollment attachment was already submitted",
            ));
        }
        self.submitted = true;
        Ok(EnrollmentAttachment {
            request: self.request.clone(),
            context: self.context.clone(),
            peer: self.peer,
            active: Arc::clone(&self.active),
        })
    }
    pub(crate) fn output_transfer(
        &mut self,
        request: wire::OutputAttachment,
    ) -> io::Result<OutputTransfer> {
        request.encode()?;
        if !self.submitted
            || self.output_submitted
            || request.key != self.request.key
            || !self.active.load(Ordering::Acquire)
        {
            return Err(refused("Bootstrap output offer is foreign or repeated"));
        }
        self.output_submitted = true;
        Ok(OutputTransfer {
            request,
            context: self.context.clone(),
            peer: self.peer,
            active: Arc::clone(&self.active),
        })
    }
}
impl OutputTransfer {
    pub(crate) fn context(&self) -> &trust_resource_controller::ContextReply {
        &self.context
    }
}
impl State {
    pub(super) fn preparation_finished(&self) -> bool {
        self.attempt
            .as_ref()
            .is_none_or(|attempt| !attempt.active.load(Ordering::Acquire))
    }
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
                "Bootstrap inputs still have original preparation custody",
            ));
        }
        // Closing these SAME descriptions never calls LOCK_UN or reacquires a
        // file. No ACK, EOF, source/output path or stored record grants release.
        self.attempt.take();
        Ok(())
    }
}

impl Owner {
    pub(in crate::actor) fn bootstrap_graph_support(
        &mut self,
        peer: Peer,
        query: wire::SupportQuery,
    ) -> io::Result<Support> {
        let context = self.bootstrap_graph_context(peer, &query.key)?;
        let root = self
            .jobs
            .get(&(
                query.key.query.binding.session.clone(),
                query.key.query.binding.root_attempt,
            ))
            .and_then(|job| job.bootstrap_graph.as_ref())
            .ok_or_else(|| refused("original Bootstrap Root unavailable"))?;
        time(root.deadline())?;
        Ok(Support {
            context,
            deadline: root.deadline(),
        })
    }
    pub(in crate::actor) fn bootstrap_graph_begin(
        &mut self,
        peer: Peer,
        request: wire::Enrollment,
    ) -> io::Result<Preparing> {
        request.validate()?;
        self.refresh_bootstrap_query(peer, &request.key)?;
        let context = self.bootstrap_graph_context(peer, &request.key)?;
        let job = self
            .jobs
            .get_mut(&(
                request.key.query.binding.session.clone(),
                request.key.query.binding.root_attempt,
            ))
            .ok_or_else(|| refused("original Bootstrap Job unavailable"))?;
        let root = job
            .bootstrap_graph
            .as_mut()
            .ok_or_else(|| refused("original Bootstrap preparation missing"))?;
        if root.inputs.attempt.is_some() || request.gnu != *root.gnu.identity() {
            return Err(refused(
                "Bootstrap enrollment is repeated or has foreign GNU custody",
            ));
        }
        root.gnu.check()?;
        root.queries.bind_enrollment(&request.key)?;
        let active = Arc::new(AtomicBool::new(true));
        root.inputs.attempt = Some(Attempt {
            request: request.clone(),
            binding: job.binding.clone(),
            group: job.child.cohort().group(),
            leader_serial: job.child.cohort().leader_serial(),
            birth: job.root_birth,
            active: Arc::clone(&active),
            gnu: None,
            control: None,
            output_requested: None,
            output: None,
        });
        Ok(Preparing {
            request,
            context,
            peer,
            active,
            deadline: root.deadline(),
            submitted: false,
            output_submitted: false,
        })
    }
    pub(in crate::actor) fn bootstrap_graph_retain(
        &mut self,
        token: EnrollmentAttachment,
        descriptions: [OwnedFd; 2],
        control: OwnedFd,
    ) -> io::Result<trust_resource_controller::ContextReply> {
        let context = self.bootstrap_graph_context(token.peer, &token.request.key)?;
        if context != token.context {
            return Err(refused("Bootstrap live context changed during enrollment"));
        }
        let job = self
            .jobs
            .get_mut(&(
                token.request.key.query.binding.session.clone(),
                token.request.key.query.binding.root_attempt,
            ))
            .ok_or_else(|| refused("original Bootstrap Job unavailable"))?;
        let root = job
            .bootstrap_graph
            .as_mut()
            .ok_or_else(|| refused("original Bootstrap preparation missing"))?;
        root.gnu.matches(&token.request, &descriptions)?;
        let attempt = root
            .inputs
            .attempt
            .as_mut()
            .ok_or_else(|| refused("Bootstrap enrollment was not prepared"))?;
        if attempt.request != token.request
            || !Arc::ptr_eq(&attempt.active, &token.active)
            || !attempt.active.load(Ordering::Acquire)
            || attempt.gnu.is_some()
            || attempt.control.is_some()
        {
            return Err(refused("Bootstrap GNU offer is foreign or repeated"));
        }
        attempt.gnu = Some(descriptions);
        attempt.control = Some(control);
        Ok(context)
    }
    pub(in crate::actor) fn bootstrap_graph_output_begin(
        &mut self,
        token: OutputTransfer,
    ) -> io::Result<OutputTransfer> {
        self.refresh_bootstrap_query(token.peer, &token.request.key)?;
        let context = self.bootstrap_graph_context(token.peer, &token.request.key)?;
        if context != token.context {
            return Err(refused(
                "Bootstrap live context changed before output offer",
            ));
        }
        let attempt = self
            .jobs
            .get_mut(&(
                token.request.key.query.binding.session.clone(),
                token.request.key.query.binding.root_attempt,
            ))
            .and_then(|job| job.bootstrap_graph.as_mut())
            .and_then(|root| root.inputs.attempt.as_mut())
            .ok_or_else(|| refused("Bootstrap output has no enrolled original graph"))?;
        if attempt.request.key != token.request.key
            || !Arc::ptr_eq(&attempt.active, &token.active)
            || !attempt.active.load(Ordering::Acquire)
            || attempt.gnu.is_none()
            || attempt.control.is_none()
            || attempt.output_requested.is_some()
            || attempt.output.is_some()
        {
            return Err(refused(
                "Bootstrap output is foreign, unprepared or repeated",
            ));
        }
        attempt.output_requested = Some(token.request.clone());
        Ok(token)
    }
    pub(in crate::actor) fn bootstrap_graph_output_retain(
        &mut self,
        token: OutputTransfer,
        descriptions: [OwnedFd; 2],
    ) -> io::Result<trust_resource_controller::ContextReply> {
        wire::transport::check_output(&token.request, &descriptions.each_ref().map(AsFd::as_fd))?;
        let context = self.bootstrap_graph_context(token.peer, &token.request.key)?;
        if context != token.context {
            return Err(refused(
                "Bootstrap live context changed during output offer",
            ));
        }
        let attempt = self
            .jobs
            .get_mut(&(
                token.request.key.query.binding.session.clone(),
                token.request.key.query.binding.root_attempt,
            ))
            .and_then(|job| job.bootstrap_graph.as_mut())
            .and_then(|root| root.inputs.attempt.as_mut())
            .ok_or_else(|| refused("Bootstrap output has no original enrollment"))?;
        if attempt.output_requested.as_ref() != Some(&token.request)
            || !Arc::ptr_eq(&attempt.active, &token.active)
            || !attempt.active.load(Ordering::Acquire)
            || attempt.output.is_some()
        {
            return Err(refused("Bootstrap output transfer is foreign or repeated"));
        }
        let output = std::path::Path::new(OsStr::from_bytes(&attempt.request.output));
        let actual = std::fs::symlink_metadata(output)?;
        if !actual.is_dir()
            || output.canonicalize()? != output
            || (actual.dev(), actual.ino()) != (token.request.root.device, token.request.root.inode)
        {
            return Err(refused(
                "Bootstrap output description differs from original selected name",
            ));
        }
        // The selected original SDK's unsafe sender contract preserves actual
        // OutputLock lineage. These observations never construct a lock/lease.
        attempt.output = Some(descriptions);
        Ok(context)
    }
}
