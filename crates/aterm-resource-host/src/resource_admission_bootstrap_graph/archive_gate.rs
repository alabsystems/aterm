// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! One original graph sequence, with the SAME retained materialization bundle.
//! A request, report or ACK never issues its owners or supplementary GNU loan.
use super::*;
use crate::actor::owner::bootstrap_archive;
use wire::archive;

pub(in crate::actor::owner) struct ArchiveOperation {
    pub(in crate::actor::owner) request: archive::Request,
    pub(in crate::actor::owner) context: trust_resource_controller::ContextReply,
    pub(in crate::actor::owner) sdk: Arc<sdk::Keeper>,
    pub(in crate::actor::owner) gnu: Arc<[OwnedFd; 2]>,
    pub(in crate::actor::owner) inputs: Arc<super::super::OriginalMaterializationInputs>,
    pub(in crate::actor::owner) deadline: Instant,
    work: Arc<AtomicBool>,
}
impl ArchiveOperation {
    pub(in crate::actor::owner) fn close_work(
        &mut self,
        proof: &bootstrap_archive::NativeClosed,
    ) -> io::Result<()> {
        if proof.request() != &self.request || proof.context() != &self.context {
            return Err(refused(
                "archive closure belongs to another actual operation",
            ));
        }
        self.work
            .compare_exchange(true, false, Ordering::AcqRel, Ordering::Acquire)
            .map_err(|_| refused("archive work already closed"))?;
        Ok(())
    }
}

impl Owner {
    fn begin_bootstrap_archive(
        &mut self,
        peer: Peer,
        request: archive::Request,
        started: Instant,
        caller_deadline: Instant,
    ) -> io::Result<ArchiveOperation> {
        request.validate()?;
        self.refresh_bootstrap_query(peer, &request.key.graph)?;
        let context = self.bootstrap_graph_context(peer, &request.key.graph)?;
        let job = self
            .jobs
            .get_mut(&(
                request.key.graph.query.binding.session.clone(),
                request.key.graph.query.binding.root_attempt,
            ))
            .ok_or_else(|| refused("original archive Job unavailable"))?;
        let root = job
            .bootstrap_graph
            .as_mut()
            .ok_or_else(|| refused("original archive Root unavailable"))?;
        let deadline = started
            .checked_add(Duration::from_nanos(request.timeout_ns))
            .ok_or_else(|| refused("archive original clock overflow"))?
            .min(root.deadline())
            .min(caller_deadline);
        let state = &mut root.queries;
        state.check_key(&request.key.graph)?;
        state.directory_current()?;
        let enrollment = root.inputs.enrolled(&request.key.graph)?;
        if state.active.is_some()
            || state.object_active.is_some()
            || state.archive_active.is_some()
            || request.key.sequence != state.next
            || state.next > MAX_QUERIES
            || state.operations.contains(&request.key.operation)
            || request.gnu != *root.gnu.identity()
            || request.sdk_root != root.sdk.execution().as_os_str().as_bytes()
            || request.source_root != enrollment.source
            || request.host != context.context.host
            || request.boot != context.context.boot
        {
            return Err(refused(
                "archive differs from actual original graph/sequence",
            ));
        }
        root.gnu.check()?;
        // Clone only the SAME issued bundle, never construct an owner from the
        // supplied identity or from a successful archive report.
        let inputs = root
            .inputs
            .original_materialization_inputs(&request.key.graph, deadline)?;
        let deadline = deadline.min(inputs.original_deadline());
        inputs.with_original_directories(deadline, |source, contents| {
            use std::os::fd::AsRawFd as _;
            for (actual, expected) in [
                (source, request.source_directory),
                (contents, request.contents_directory),
            ] {
                let m = spawnfd::descriptor_metadata(actual.as_raw_fd())?;
                if !m.is_dir() || m.dev() != expected.device || m.ino() != expected.inode {
                    return Err(refused("archive differs from actual original directory"));
                }
            }
            Ok(())
        })?;
        let held = root.sdk.bootstrap_archive_program(deadline)?;
        if bootstrap_archive::observed_program(&held.observation().executable)? != request.driver {
            return Err(refused(
                "archive driver differs from early captured Keeper image",
            ));
        }
        time(deadline)?;
        let work = Arc::new(AtomicBool::new(true));
        state.key = Some(request.key.graph.clone());
        state.archive_active = Some(request.clone());
        state.active_context = Some(context.clone());
        state.operations.insert(request.key.operation);
        state.activity.push(Arc::clone(&work));
        Ok(ArchiveOperation {
            request,
            context,
            inputs,
            deadline,
            work,
            sdk: Arc::clone(&root.sdk),
            gnu: Arc::clone(&root.gnu.descriptions),
        })
    }
    pub(in crate::actor) fn submit_bootstrap_archive(
        &mut self,
        peer: Peer,
        request: archive::Request,
        stream: aterm_uds::CtlStream,
        started: Instant,
        deadline: Instant,
    ) -> io::Result<()> {
        let operation = self.begin_bootstrap_archive(peer, request, started, deadline)?;
        let key = (
            operation.request.key.graph.query.binding.session.clone(),
            operation.request.key.graph.query.binding.root_attempt,
        );
        let job = self.jobs.get_mut(&key).expect("same actual archive Job");
        let root = job
            .bootstrap_graph
            .as_mut()
            .expect("same actual archive Root");
        let archive = Box::new(bootstrap_archive::Archive::prepare(operation, stream));
        if root.queries.archive_running.is_some()
            || root.queries.running.is_some()
            || root.queries.object_running.is_some()
        {
            root.queries.failed = true;
            std::mem::forget(archive);
            return Err(refused("original archive slot occupied"));
        }
        root.queries.archive_running = Some(archive);
        Ok(())
    }
    fn consume_bootstrap_archive(
        &mut self,
        retained: bootstrap_archive::Retained,
    ) -> io::Result<()> {
        let request = retained.request();
        let root = self
            .jobs
            .get_mut(&(
                request.key.graph.query.binding.session.clone(),
                request.key.graph.query.binding.root_attempt,
            ))
            .and_then(|job| job.bootstrap_graph.as_mut());
        let Some(root) = root else {
            std::mem::forget(retained);
            return Err(refused("closed archive lost original Root"));
        };
        let state = &mut root.queries;
        if state.archive_active.as_ref() != Some(request)
            || state.active_context.as_ref() != Some(retained.context())
        {
            state.failed = true;
            std::mem::forget(retained);
            return Err(refused(
                "closed archive differs from active original operation",
            ));
        }
        let success = retained.success();
        state.archive_retained.push(retained);
        if !success {
            state.failed = true;
            return Ok(());
        }
        state.archive_active = None;
        state.active_context = None;
        state.next = state
            .next
            .checked_add(1)
            .ok_or_else(|| refused("archive sequence overflow"))?;
        Ok(())
    }
    pub(super) fn tick_bootstrap_archives(&mut self) {
        let mut closed = Vec::new();
        for job in self.jobs.values_mut() {
            if let Some(value) = Self::poll_bootstrap_archive(&self.fence, &self.journal, job) {
                closed.push(value);
            }
        }
        for value in closed {
            if let Err(error) = self.consume_bootstrap_archive(value) {
                self.journal
                    .fault(&mut self.faulted, "archive-consume", error);
            }
        }
    }
    pub(super) fn refresh_bootstrap_archive(
        &mut self,
        peer: Peer,
        key: &wire::Key,
    ) -> io::Result<()> {
        self.bootstrap_graph_context(peer, key)?;
        let job = self
            .jobs
            .get_mut(&(
                key.query.binding.session.clone(),
                key.query.binding.root_attempt,
            ))
            .ok_or_else(|| refused("original archive Root unavailable"))?;
        if let Some(value) = Self::poll_bootstrap_archive(&self.fence, &self.journal, job) {
            self.consume_bootstrap_archive(value)?;
        }
        Ok(())
    }
    fn poll_bootstrap_archive(
        fence: &aterm_capacity::leadership::Fence,
        journal: &journal::Journal,
        job: &mut Job,
    ) -> Option<bootstrap_archive::Retained> {
        let root = job.bootstrap_graph.as_mut()?;
        let mut archive = root.queries.archive_running.take()?;
        let Custody::Fixed(cohort) = &mut job.custody else {
            root.queries.failed = true;
            root.queries.archive_running = Some(archive);
            return None;
        };
        let cancel = job.cleanup_at.is_some() || job.events.cancelled();
        archive.poll(fence, journal, cohort, &mut job.child, cancel);
        match archive.into_retained() {
            Ok(value) => Some(value),
            Err(value) => {
                root.queries.archive_running = Some(value);
                None
            }
        }
    }
}
