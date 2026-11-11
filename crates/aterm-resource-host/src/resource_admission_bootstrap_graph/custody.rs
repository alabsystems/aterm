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
    source_requested: Option<wire::SourceAttachment>,
    source: Option<Arc<OriginalSource>>,
    output_requested: Option<wire::OutputAttachment>,
    output: Option<Arc<[OwnedFd; 2]>>,
}

/// Close-only originals accepted from this enrolled graph's actual source
/// producer. These descriptions never construct a Manager or release permit.
pub(in crate::actor::owner) struct OriginalSource {
    request: wire::SourceAttachment,
    path: PathBuf,
    descriptions: [OwnedFd; 4],
    // Use eligibility is distinct from the receiving handler's live barrier.
    revoked: AtomicBool,
    active: Arc<AtomicBool>,
    deadline: Instant,
}
impl OriginalSource {
    pub(in crate::actor::owner) fn check_current(&self, deadline: Instant) -> io::Result<()> {
        let result = (|| {
            let deadline = deadline.min(self.deadline);
            time(deadline)?;
            if self.revoked.load(Ordering::Acquire) || !self.active.load(Ordering::Acquire) {
                return Err(refused("original source enrollment is no longer active"));
            }
            wire::transport::check_source(
                &self.request,
                &self.descriptions.each_ref().map(AsFd::as_fd),
                deadline,
            )?;
            let named = std::fs::symlink_metadata(&self.path)?;
            if !named.is_dir()
                || self.path.canonicalize()? != self.path
                || (named.dev(), named.ino())
                    != (
                        self.request.source_directory.device,
                        self.request.source_directory.inode,
                    )
            {
                return Err(refused("original enrolled source directory changed"));
            }
            time(deadline)?;
            if self.revoked.load(Ordering::Acquire) || !self.active.load(Ordering::Acquire) {
                return Err(refused("original source revoked during observation"));
            }
            Ok(())
        })();
        if result.is_err() {
            // Eligibility is monotone. Refusal cannot end the actual handler's
            // custody barrier or retire its retained descriptions.
            self.revoked.store(true, Ordering::Release);
        }
        result
    }

    pub(in crate::actor::owner) fn matches(
        &self,
        key: &wire::Key,
        path: &[u8],
        directory: trust_resource_controller::FileIdentity,
        deadline: Instant,
    ) -> io::Result<()> {
        if &self.request.key != key
            || self.path.as_os_str().as_bytes() != path
            || self.request.source_directory != directory
        {
            return Err(refused(
                "object source differs from accepted original offer",
            ));
        }
        self.check_current(deadline)
    }

    pub(in crate::actor::owner) fn directory(
        &self,
        deadline: Instant,
    ) -> io::Result<std::os::fd::BorrowedFd<'_>> {
        self.check_current(deadline)?;
        Ok(self.descriptions[0].as_fd())
    }
}

/// Only State's already accepted original output offer creates this borrow.
/// It extends the SAME close-only directory/lock descriptions, never reopens
/// a lock or reconstructs its lineage from metadata or an object request.
pub(in crate::actor::owner) struct OriginalOutput {
    request: wire::OutputAttachment,
    path: PathBuf,
    descriptions: Arc<[OwnedFd; 2]>,
}
impl OriginalOutput {
    pub(in crate::actor::owner) fn check_current(&self) -> io::Result<()> {
        wire::transport::check_output(
            &self.request,
            &self.descriptions.each_ref().map(AsFd::as_fd),
        )?;
        for fd in self.descriptions.iter() {
            // ACL inspection requires a File. Protect its fresh duplicate before
            // any concurrent fork can inherit the original output custody.
            let original = aterm_uds::kept_out(|| fd.try_clone().map(std::fs::File::from))?;
            trust_resource_provenance::require_no_acl_grants(&original)?;
            let metadata = original.metadata()?;
            if metadata.uid() != aterm_uds::peer::our_uid() || metadata.mode() & 0o022 != 0 {
                return Err(refused(
                    "original object output has unsupported owner/permissions",
                ));
            }
        }
        let named = std::fs::symlink_metadata(&self.path)?;
        if !named.is_dir()
            || self.path.canonicalize()? != self.path
            || (named.dev(), named.ino()) != (self.request.root.device, self.request.root.inode)
        {
            return Err(refused("original enrolled output directory changed"));
        }
        Ok(())
    }

    pub(in crate::actor::owner) fn inherited(&self) -> io::Result<[OwnedFd; 2]> {
        self.check_current()?;
        aterm_uds::kept_out(|| {
            Ok((
                self.descriptions[0].try_clone()?,
                self.descriptions[1].try_clone()?,
            ))
        })
        .map(|(directory, lock)| [directory, lock])
    }

    pub(in crate::actor::owner) fn attachment(&self) -> &wire::OutputAttachment {
        &self.request
    }

    pub(in crate::actor::owner) fn path(&self) -> &std::path::Path {
        &self.path
    }

    pub(in crate::actor::owner) fn directory(&self) -> io::Result<std::os::fd::BorrowedFd<'_>> {
        self.check_current()?;
        Ok(self.descriptions[0].as_fd())
    }
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
    source_submitted: bool,
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
pub(crate) struct SourceTransfer {
    request: wire::SourceAttachment,
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
            || !self.source_submitted
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

    pub(crate) fn source_transfer(
        &mut self,
        request: wire::SourceAttachment,
    ) -> io::Result<SourceTransfer> {
        request.validate()?;
        if !self.submitted
            || self.source_submitted
            || self.output_submitted
            || request.key != self.request.key
            || !self.active.load(Ordering::Acquire)
        {
            return Err(refused("Bootstrap source offer is foreign or repeated"));
        }
        self.source_submitted = true;
        Ok(SourceTransfer {
            request,
            context: self.context.clone(),
            peer: self.peer,
            active: Arc::clone(&self.active),
        })
    }
}
impl SourceTransfer {
    pub(crate) fn context(&self) -> &trust_resource_controller::ContextReply {
        &self.context
    }
}
impl OutputTransfer {
    pub(crate) fn context(&self) -> &trust_resource_controller::ContextReply {
        &self.context
    }
}
impl State {
    pub(super) fn original_source(
        &self,
        key: &wire::Key,
        deadline: Instant,
    ) -> io::Result<Arc<OriginalSource>> {
        self.enrolled(key)?;
        let source = self
            .attempt
            .as_ref()
            .and_then(|attempt| attempt.source.as_ref())
            .ok_or_else(|| refused("object has no accepted original source offer"))?;
        source.check_current(deadline)?;
        Ok(Arc::clone(source))
    }
    pub(super) fn enrolled(&self, key: &wire::Key) -> io::Result<&wire::Enrollment> {
        let attempt = self
            .attempt
            .as_ref()
            .ok_or_else(|| refused("object has no original enrollment"))?;
        if &attempt.request.key != key
            || !attempt.active.load(Ordering::Acquire)
            || attempt.gnu.is_none()
            || attempt.control.is_none()
        {
            return Err(refused("object lost its original active enrollment"));
        }
        Ok(&attempt.request)
    }
    pub(super) fn original_output(&self, key: &wire::Key) -> io::Result<OriginalOutput> {
        let attempt = self
            .attempt
            .as_ref()
            .ok_or_else(|| refused("object operation has no original enrollment"))?;
        if &attempt.request.key != key
            || !attempt.active.load(Ordering::Acquire)
            || attempt.gnu.is_none()
            || attempt.control.is_none()
        {
            return Err(refused("object operation lost its original enrolled graph"));
        }
        let request = attempt
            .output_requested
            .as_ref()
            .ok_or_else(|| refused("object operation has no accepted output offer"))?;
        let descriptions = attempt
            .output
            .as_ref()
            .ok_or_else(|| refused("object operation has no retained original output"))?;
        if &request.key != key {
            return Err(refused("object operation has a foreign output key"));
        }
        let original = OriginalOutput {
            request: request.clone(),
            path: PathBuf::from(OsStr::from_bytes(&attempt.request.output)),
            descriptions: Arc::clone(descriptions),
        };
        original.check_current()?;
        Ok(original)
    }

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
            source_requested: None,
            source: None,
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
            source_submitted: false,
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
    pub(in crate::actor) fn bootstrap_graph_source_begin(
        &mut self,
        token: SourceTransfer,
    ) -> io::Result<SourceTransfer> {
        self.refresh_bootstrap_query(token.peer, &token.request.key)?;
        let context = self.bootstrap_graph_context(token.peer, &token.request.key)?;
        if context != token.context {
            return Err(refused(
                "Bootstrap live context changed before source offer",
            ));
        }
        let root = self
            .jobs
            .get_mut(&(
                token.request.key.query.binding.session.clone(),
                token.request.key.query.binding.root_attempt,
            ))
            .and_then(|job| job.bootstrap_graph.as_mut())
            .ok_or_else(|| refused("Bootstrap source has no original Root"))?;
        time(root.deadline())?;
        let attempt = root
            .inputs
            .attempt
            .as_mut()
            .ok_or_else(|| refused("Bootstrap source has no enrolled original graph"))?;
        if attempt.request.key != token.request.key
            || !Arc::ptr_eq(&attempt.active, &token.active)
            || !attempt.active.load(Ordering::Acquire)
            || attempt.gnu.is_none()
            || attempt.control.is_none()
            || attempt.source_requested.is_some()
            || attempt.source.is_some()
            || attempt.output_requested.is_some()
        {
            return Err(refused(
                "Bootstrap source is foreign, unprepared or repeated",
            ));
        }
        token.request.validate()?;
        attempt.source_requested = Some(token.request.clone());
        Ok(token)
    }
    pub(in crate::actor) fn bootstrap_graph_source_retain(
        &mut self,
        token: SourceTransfer,
        descriptions: [OwnedFd; 4],
    ) -> io::Result<trust_resource_controller::ContextReply> {
        // Even a failed context recheck must not discard an already delivered
        // original population. The matching one-use slot stores it first.
        let context = self.bootstrap_graph_context(token.peer, &token.request.key);
        let root = self
            .jobs
            .get_mut(&(
                token.request.key.query.binding.session.clone(),
                token.request.key.query.binding.root_attempt,
            ))
            .and_then(|job| job.bootstrap_graph.as_mut());
        let Some(root) = root else {
            std::mem::forget(descriptions);
            return Err(refused("delivered Bootstrap source lost its original Root"));
        };
        let deadline = root.deadline();
        let Some(attempt) = root.inputs.attempt.as_mut() else {
            std::mem::forget(descriptions);
            return Err(refused(
                "delivered Bootstrap source lost its original enrollment",
            ));
        };
        if attempt.source_requested.as_ref() != Some(&token.request)
            || !Arc::ptr_eq(&attempt.active, &token.active)
            || attempt.source.is_some()
        {
            return Err(refused("Bootstrap source transfer is foreign or repeated"));
        }
        let original = Arc::new(OriginalSource {
            request: token.request,
            path: PathBuf::from(OsStr::from_bytes(&attempt.request.source)),
            descriptions,
            revoked: AtomicBool::new(false),
            active: Arc::clone(&attempt.active),
            deadline,
        });
        attempt.source = Some(Arc::clone(&original));
        let result = (|| {
            original.check_current(deadline)?;
            let context = context?;
            if context != token.context {
                return Err(refused(
                    "Bootstrap live context changed during source offer",
                ));
            }
            Ok(context)
        })();
        if result.is_err() {
            original.revoked.store(true, Ordering::Release);
        }
        result
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
            || attempt.source.is_none()
            || attempt.output_requested.is_some()
            || attempt.output.is_some()
        {
            return Err(refused(
                "Bootstrap output is foreign, unprepared or repeated",
            ));
        }
        let source = attempt
            .source
            .as_ref()
            .expect("accepted source checked above");
        source.check_current(source.deadline)?;
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
        attempt.output = Some(Arc::new(descriptions));
        Ok(context)
    }
}

#[cfg(test)]
#[path = "custody_tests.rs"]
mod tests;
