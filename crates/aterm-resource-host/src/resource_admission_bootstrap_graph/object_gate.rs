// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! The same Root sequence, actual input owners and one-use work guard. Source
//! FDs and object metadata supply no root, output-lock or resource authority.
use super::*;
use crate::actor::owner::bootstrap_object;
use wire::object;

pub(in crate::actor::owner) struct ObjectOperation {
    request: object::Request,
    context: trust_resource_controller::ContextReply,
    sdk: Arc<sdk::Keeper>,
    gnu: Arc<[OwnedFd; 2]>,
    deadline: Instant,
    environment: BTreeMap<Vec<u8>, Vec<u8>>,
    output: Option<super::super::OriginalOutput>,
    work: Arc<AtomicBool>,
}
impl ObjectOperation {
    pub(in crate::actor::owner) fn request(&self) -> &object::Request {
        &self.request
    }
    pub(in crate::actor::owner) fn context(&self) -> &trust_resource_controller::ContextReply {
        &self.context
    }
    pub(in crate::actor::owner) fn sdk(&self) -> Arc<sdk::Keeper> {
        Arc::clone(&self.sdk)
    }
    pub(in crate::actor::owner) fn gnu(&self) -> Arc<[OwnedFd; 2]> {
        Arc::clone(&self.gnu)
    }
    pub(in crate::actor::owner) fn deadline(&self) -> Instant {
        self.deadline
    }
    pub(in crate::actor::owner) fn environment(&self) -> &BTreeMap<Vec<u8>, Vec<u8>> {
        &self.environment
    }
    pub(in crate::actor::owner) fn take_output(
        &mut self,
    ) -> io::Result<super::super::OriginalOutput> {
        self.output
            .take()
            .ok_or_else(|| refused("object original output was already taken"))
    }
    pub(in crate::actor::owner) fn close_work(
        &mut self,
        proof: &bootstrap_object::NativeClosed,
    ) -> io::Result<()> {
        if proof.request() != &self.request || proof.context() != &self.context {
            return Err(refused(
                "native closed object belongs to another original operation",
            ));
        }
        self.work
            .compare_exchange(true, false, Ordering::AcqRel, Ordering::Acquire)
            .map_err(|_| refused("native object work already closed"))?;
        Ok(())
    }
}
enum ClosedObject {
    Complete(bootstrap_object::Completed),
    Failed(bootstrap_object::ClosedFailed),
}
impl Owner {
    fn bootstrap_graph_begin_object(
        &mut self,
        peer: Peer,
        request: object::Request,
        started: Instant,
    ) -> io::Result<ObjectOperation> {
        request.validate()?;
        self.refresh_bootstrap_query(peer, &request.key.graph)?;
        let context = self.bootstrap_graph_context(peer, &request.key.graph)?;
        let job = self
            .jobs
            .get_mut(&(
                request.key.graph.query.binding.session.clone(),
                request.key.graph.query.binding.root_attempt,
            ))
            .ok_or_else(|| refused("original Bootstrap object Job unavailable"))?;
        let root = job
            .bootstrap_graph
            .as_mut()
            .ok_or_else(|| refused("original Bootstrap object Root unavailable"))?;
        let state = &mut root.queries;
        state.check_key(&request.key.graph)?;
        state.directory_current()?;
        let deadline = started
            .checked_add(Duration::from_nanos(request.timeout_ns))
            .ok_or_else(|| refused("object original clock overflow"))?
            .min(root.deadline);
        time(deadline)?;
        let enrollment = root.inputs.enrolled(&request.key.graph)?;
        if state.active.is_some()
            || state.object_active.is_some()
            || request.key.sequence != state.next
            || state.next > MAX_QUERIES
            || state.operations.contains(&request.key.operation)
            || request.gnu != *root.gnu.identity()
            || request.sdk_root != root.sdk.execution().as_os_str().as_bytes()
            || request.source_root != enrollment.source
            || request.output_root != enrollment.output
        {
            return Err(refused(
                "object differs from original enrolled graph/sequence/input",
            ));
        }
        root.gnu.check()?;
        let output = root.inputs.original_output(&request.key.graph)?;
        if request.output_directory != output.attachment().root
            || request.output_lock != output.attachment().lock
        {
            return Err(refused(
                "object output differs from actual original accepted descriptions",
            ));
        }
        let driver = root.sdk.bootstrap_object_program(deadline)?;
        let compiler = root
            .sdk
            .bootstrap_query_program(query::Tool::Compiler, deadline)?;
        if bootstrap_object::observed_program(&driver.observation().executable)? != request.driver
            || bootstrap_object::observed_program(&compiler.observation().executable)?
                != request.compiler
        {
            return Err(refused(
                "object program differs from actual early Keeper capture",
            ));
        }
        time(deadline)?;
        let work = Arc::new(AtomicBool::new(true));
        state.key = Some(request.key.graph.clone());
        state.object_active = Some(request.clone());
        state.active_context = Some(context.clone());
        state.operations.insert(request.key.operation);
        state.activity.push(Arc::clone(&work));
        Ok(ObjectOperation {
            request,
            context,
            sdk: Arc::clone(&root.sdk),
            gnu: Arc::clone(&root.gnu.descriptions),
            deadline,
            environment: state.environment.clone(),
            output: Some(output),
            work,
        })
    }
    pub(in crate::actor) fn prepare_bootstrap_object(
        &mut self,
        peer: Peer,
        request: object::Request,
        stream: aterm_uds::CtlStream,
        started: Instant,
        deadline: Instant,
    ) -> io::Result<bootstrap_object::SourceOffer> {
        let operation = self.bootstrap_graph_begin_object(peer, request, started)?;
        let key = (
            operation.request().key.graph.query.binding.session.clone(),
            operation.request().key.graph.query.binding.root_attempt,
        );
        let job = self.jobs.get_mut(&key).expect("same original object Job");
        let root = job
            .bootstrap_graph
            .as_mut()
            .expect("same original object Root");
        let object = Box::new(bootstrap_object::Object::prepare(
            operation, stream, deadline,
        ));
        if root.queries.object_running.is_some() || root.queries.running.is_some() {
            root.queries.failed = true;
            std::mem::forget(object);
            return Err(refused("original object slot already occupied"));
        }
        root.queries.object_running = Some(object);
        root.queries
            .object_running
            .as_ref()
            .expect("same object slot")
            .offer()
    }
    pub(in crate::actor) fn bootstrap_object_source(
        &mut self,
        peer: Peer,
        offer: bootstrap_object::SourceOffer,
        source: io::Result<[OwnedFd; 2]>,
    ) -> io::Result<()> {
        // Reauthenticate this exact original before the source is allowed to ARM.
        let current = self.bootstrap_graph_context(peer, &offer.request().key.graph);
        let valid = current.as_ref().is_ok_and(|value| value == offer.context());
        let key = (
            offer.request().key.graph.query.binding.session.clone(),
            offer.request().key.graph.query.binding.root_attempt,
        );
        let Some(job) = self.jobs.get_mut(&key) else {
            std::mem::forget((offer, source));
            return Err(refused("object source lost original Job"));
        };
        let Some(root) = job.bootstrap_graph.as_mut() else {
            std::mem::forget((offer, source));
            return Err(refused("object source lost original Root"));
        };
        let Some(object) = root.queries.object_running.as_mut() else {
            std::mem::forget((offer, source));
            return Err(refused("object source lost actual pending issuer"));
        };
        let Custody::Fixed(cohort) = &mut job.custody else {
            std::mem::forget((offer, source));
            return Err(refused("object source lost original fixed cohort"));
        };
        let source = if valid {
            source
        } else {
            // Retain even a rejected full source offer with the actual issuer;
            // attach records descriptors before its validation and cancellation.
            object.reject_context();
            source
        };
        object.attach_source(
            offer,
            source,
            &self.fence,
            &self.journal,
            cohort,
            &mut job.child,
        )
    }
    fn consume_bootstrap_object(&mut self, token: ClosedObject) -> io::Result<()> {
        let (request, context) = match &token {
            ClosedObject::Complete(value) => (value.request().clone(), value.context().clone()),
            ClosedObject::Failed(value) => (value.request().clone(), value.context().clone()),
        };
        let Some(root) = self
            .jobs
            .get_mut(&(
                request.key.graph.query.binding.session.clone(),
                request.key.graph.query.binding.root_attempt,
            ))
            .and_then(|job| job.bootstrap_graph.as_mut())
        else {
            self.journal.fault(
                &mut self.faulted,
                "object-root",
                "original root disappeared while its completion was held",
            );
            std::mem::forget(token);
            return Err(refused("closed object lost original Root"));
        };
        let state = &mut root.queries;
        if state.object_active.as_ref() != Some(&request)
            || state.active_context.as_ref() != Some(&context)
        {
            state.failed = true;
            std::mem::forget(token);
            return Err(refused(
                "closed object differs from original active operation",
            ));
        }
        let complete = matches!(&token, ClosedObject::Complete(_));
        let retained = match token {
            ClosedObject::Complete(value) => value.into_retained().map_err(|value| {
                std::mem::forget(value);
                refused("completed object failed same-root work closure")
            }),
            ClosedObject::Failed(value) => value.into_retained().map_err(|value| {
                std::mem::forget(value);
                refused("failed object lacks same-root work closure")
            }),
        };
        let retained = match retained {
            Ok(value) => value,
            Err(error) => {
                state.failed = true;
                return Err(error);
            }
        };
        let diagnostic = retained.diagnostics();
        state.object_retained.push(retained);
        let binding = Binding {
            session: request.key.graph.query.binding.session.clone(),
            attempt: request.key.graph.query.binding.root_attempt,
        };
        if let Err(error) = self.journal.patch(
            &binding,
            aterm_json::json!({"bootstrap_object_last":diagnostic}),
        ) {
            state.failed = true;
            return Err(error);
        }
        if !complete {
            state.failed = true;
            return Ok(());
        }
        state.object_active = None;
        state.active_context = None;
        state.next = state
            .next
            .checked_add(1)
            .ok_or_else(|| refused("object sequence overflow"))?;
        Ok(())
    }
    pub(super) fn tick_bootstrap_objects(&mut self) {
        let mut closed = Vec::new();
        for job in self.jobs.values_mut() {
            if let Some(token) = Self::poll_bootstrap_object(&self.fence, &self.journal, job) {
                closed.push(token);
            }
        }
        for token in closed {
            if let Err(error) = self.consume_bootstrap_object(token) {
                self.journal
                    .fault(&mut self.faulted, "object-consume", error);
            }
        }
    }
    pub(super) fn refresh_bootstrap_object(
        &mut self,
        peer: Peer,
        key: &wire::Key,
    ) -> io::Result<()> {
        self.bootstrap_graph_context(peer, key)?;
        let token = {
            let job = self
                .jobs
                .get_mut(&(
                    key.query.binding.session.clone(),
                    key.query.binding.root_attempt,
                ))
                .ok_or_else(|| refused("original object Root unavailable"))?;
            Self::poll_bootstrap_object(&self.fence, &self.journal, job)
        };
        if let Some(token) = token {
            self.consume_bootstrap_object(token)?;
        }
        Ok(())
    }
    fn poll_bootstrap_object(
        fence: &aterm_capacity::leadership::Fence,
        journal: &journal::Journal,
        job: &mut Job,
    ) -> Option<ClosedObject> {
        let root = job.bootstrap_graph.as_mut()?;
        let mut object = root.queries.object_running.take()?;
        let Custody::Fixed(cohort) = &mut job.custody else {
            root.queries.failed = true;
            root.queries.object_running = Some(object);
            return None;
        };
        let cancel = if job.cleanup_at.is_some() || job.events.cancelled() {
            Some(match job.cause {
                CompletionCause::Timeout => trust_resource_controller::CompletionCause::Timeout,
                CompletionCause::ResourceLimit => {
                    trust_resource_controller::CompletionCause::ResourceLimit
                }
                CompletionCause::HostFault => trust_resource_controller::CompletionCause::HostFault,
                _ => trust_resource_controller::CompletionCause::Cancelled,
            })
        } else {
            None
        };
        let ready = object.poll(fence, journal, cohort, &mut job.child, cancel);
        let completed = if ready {
            object.into_completed()
        } else {
            Err(object)
        };
        match completed {
            Ok(value) => Some(ClosedObject::Complete(value)),
            Err(value) => match value.into_closed_failed() {
                Ok(value) => Some(ClosedObject::Failed(value)),
                Err(value) => {
                    root.queries.object_running = Some(value);
                    None
                }
            },
        }
    }
}
