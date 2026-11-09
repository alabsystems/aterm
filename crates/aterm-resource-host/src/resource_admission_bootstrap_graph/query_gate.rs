// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Original Root expectations and actual operation lifetime. Neither this gate
//! nor its sequence supplies the separate mandatory supplementary GNU loan.
use super::super::bootstrap_query;
use super::*;
use std::{
    collections::{BTreeMap, BTreeSet},
    fs::{File, OpenOptions},
    os::unix::fs::OpenOptionsExt as _,
};
use wire::query;

#[path = "object_gate.rs"]
mod object_gate;
pub(in crate::actor::owner) use object_gate::ObjectOperation;
const MAX_QUERIES: u64 = 128;
enum ClosedQuery {
    Complete(bootstrap_query::Completed),
    Failed(bootstrap_query::ClosedFailed),
}
pub(super) struct State {
    cwd: PathBuf,
    directory: File,
    directory_identity: (u64, u64),
    environment: BTreeMap<Vec<u8>, Vec<u8>>,
    key: Option<wire::Key>,
    next: u64,
    active: Option<query::Request>,
    active_context: Option<trust_resource_controller::ContextReply>,
    operations: BTreeSet<[u8; 16]>,
    activity: Vec<Arc<AtomicBool>>,
    failed: bool,
    retained: Vec<bootstrap_query::Retained>,
    running: Option<Box<bootstrap_query::Query>>,
    object_active: Option<wire::object::Request>,
    object_running: Option<Box<super::super::bootstrap_object::Object>>,
    object_retained: Vec<super::super::bootstrap_object::Retained>,
}
impl State {
    pub(super) fn new(command: &mut spawnfd::Command) -> io::Result<Self> {
        let cwd = command
            .get_current_dir()
            .ok_or_else(|| refused("Bootstrap original cwd missing"))?
            .to_path_buf();
        if cwd.canonicalize()? != cwd {
            return Err(refused("Bootstrap original cwd is not canonical"));
        }
        let directory = aterm_uds::kept_out(|| {
            OpenOptions::new()
                .read(true)
                .custom_flags(libc::O_DIRECTORY | libc::O_NOFOLLOW | libc::O_CLOEXEC)
                .open(&cwd)
        })?;
        let metadata = directory.metadata()?;
        let mut environment = BTreeMap::new();
        let mut exact = Vec::new();
        for (name, value) in command.get_envs() {
            let value = value
                .ok_or_else(|| refused("Bootstrap original environment contains a removal"))?;
            if !matches!(
                name.as_bytes(),
                b"LANG"
                    | b"LC_ALL"
                    | b"CARGO"
                    | b"RUSTC"
                    | b"CARGO_HOME"
                    | b"CARGO_CACHE_RUSTC_INFO"
            ) {
                return Err(refused(
                    "Bootstrap original complete semantic environment is outside the closed query profile",
                ));
            }
            environment.insert(name.as_bytes().to_vec(), value.as_bytes().to_vec());
            exact.push((name.to_owned(), value.to_owned()));
        }
        if environment.len() != 6 {
            return Err(refused(
                "Bootstrap original semantic environment incomplete",
            ));
        }
        // Check every configured entry BEFORE clearing ambient inheritance.
        // No unobserved PATH/HOME/loader/config variable can reach this Root.
        command.env_clear().envs(exact);
        Ok(Self {
            cwd,
            directory,
            directory_identity: (metadata.dev(), metadata.ino()),
            environment,
            key: None,
            next: 1,
            active: None,
            active_context: None,
            operations: BTreeSet::new(),
            activity: Vec::new(),
            failed: false,
            retained: Vec::new(),
            running: None,
            object_active: None,
            object_running: None,
            object_retained: Vec::new(),
        })
    }
    pub(super) fn check_key(&self, key: &wire::Key) -> io::Result<()> {
        if self.failed || self.key.as_ref().is_some_and(|original| original != key) {
            return Err(refused(
                "Bootstrap graph is failed or has a foreign original key",
            ));
        }
        Ok(())
    }
    pub(super) fn enrollment_allowed(&self, key: &wire::Key) -> io::Result<()> {
        self.check_key(key)?;
        if self.active.is_some() || self.object_active.is_some() {
            return Err(refused("Bootstrap SDK operation is still unresolved"));
        }
        Ok(())
    }
    pub(super) fn bind_enrollment(&mut self, key: &wire::Key) -> io::Result<()> {
        self.enrollment_allowed(key)?;
        self.key = Some(key.clone());
        Ok(())
    }
    pub(super) fn native_queries_drained(&self) -> bool {
        self.running.is_none() && self.object_running.is_none() && self.preparation_finished()
    }
    pub(super) fn sampling_group(&self) -> io::Result<Option<i32>> {
        if let Some(object) = &self.object_running {
            return object.sampling_group();
        }
        self.running
            .as_ref()
            .map_or(Ok(None), |query| query.sampling_group())
    }
    pub(super) fn failed(&self) -> bool {
        self.failed
    }
    pub(super) fn preparation_finished(&self) -> bool {
        self.activity
            .iter()
            .all(|active| !active.load(Ordering::Acquire))
    }
    pub(super) fn release(&mut self, _closed: &settlement::ClosedRoot) -> io::Result<()> {
        if !self.preparation_finished() {
            return Err(refused("Bootstrap SDK operation still owns native work"));
        }
        self.retained.clear();
        self.object_retained.clear();
        Ok(())
    }
    fn directory_current(&self) -> io::Result<()> {
        let held = self.directory.metadata()?;
        let named = std::fs::symlink_metadata(&self.cwd)?;
        if !named.is_dir()
            || self.cwd.canonicalize()? != self.cwd
            || (held.dev(), held.ino()) != self.directory_identity
            || (named.dev(), named.ino()) != self.directory_identity
        {
            return Err(refused("Bootstrap original cwd observation changed"));
        }
        Ok(())
    }
}

pub(in crate::actor::owner) struct QueryOperation {
    request: query::Request,
    context: trust_resource_controller::ContextReply,
    sdk: Arc<sdk::Keeper>,
    gnu: Arc<[OwnedFd; 2]>,
    deadline: Instant,
    work: Arc<AtomicBool>,
}
impl QueryOperation {
    pub(in crate::actor::owner) fn request(&self) -> &query::Request {
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
    pub(in crate::actor::owner) fn close_work(
        &mut self,
        proof: &bootstrap_query::NativeClosed,
    ) -> io::Result<()> {
        if proof.request() != &self.request || proof.context() != &self.context {
            return Err(refused(
                "native closed query belongs to another original operation",
            ));
        }
        self.work
            .compare_exchange(true, false, Ordering::AcqRel, Ordering::Acquire)
            .map_err(|_| refused("native query work was already closed"))?;
        Ok(())
    }
    // Drop/error cannot clear work or the original active sequence. Only the
    // issuer's private native closure token closes this actual activity guard.
}

impl Owner {
    pub(super) fn bootstrap_graph_begin_query(
        &mut self,
        peer: Peer,
        request: query::Request,
        started: Instant,
    ) -> io::Result<QueryOperation> {
        request.validate()?;
        self.refresh_bootstrap_query(peer, &request.key.graph)?;
        let context = self.bootstrap_graph_context(peer, &request.key.graph)?;
        let job = self
            .jobs
            .get_mut(&(
                request.key.graph.query.binding.session.clone(),
                request.key.graph.query.binding.root_attempt,
            ))
            .ok_or_else(|| refused("original Bootstrap query Job unavailable"))?;
        let root = job
            .bootstrap_graph
            .as_mut()
            .ok_or_else(|| refused("original Bootstrap query Root unavailable"))?;
        let state = &mut root.queries;
        state.check_key(&request.key.graph)?;
        state.directory_current()?;
        let deadline = started
            .checked_add(Duration::from_nanos(request.timeout_ns))
            .ok_or_else(|| refused("Bootstrap query original clock overflow"))?
            .min(root.deadline);
        time(deadline)?;
        if state.active.is_some()
            || state.object_active.is_some()
            || request.key.sequence != state.next
            || state.next > MAX_QUERIES
            || state.operations.contains(&request.key.operation)
            || request.gnu != *root.gnu.identity()
            || request.sdk_root != root.sdk.execution().as_os_str().as_bytes()
            || request.cwd != state.cwd.as_os_str().as_bytes()
            || request
                .environment
                .iter()
                .cloned()
                .collect::<BTreeMap<_, _>>()
                != state.environment
        {
            return Err(refused(
                "Bootstrap SDK query differs from original Root input/sequence",
            ));
        }
        root.gnu.check()?;
        let held = root.sdk.bootstrap_query_program(request.tool, deadline)?;
        let executable = &held.observation().executable;
        let mut sha256 = [0; 32];
        if !aterm_codec::hex::decode_into(executable.sha256.as_bytes(), &mut sha256) {
            return Err(refused("original SDK tool digest is malformed"));
        }
        let expected = trust_resource_controller::MetadataFile {
            path: executable.path.as_os_str().as_bytes().to_vec(),
            sha256,
            bytes: executable.bytes,
            device: executable.device,
            inode: executable.inode,
        };
        if request.program != expected {
            return Err(refused(
                "query program differs from original captured SDK tool",
            ));
        }
        time(deadline)?;
        let work = Arc::new(AtomicBool::new(true));
        state.key = Some(request.key.graph.clone());
        state.active = Some(request.clone());
        state.active_context = Some(context.clone());
        state.operations.insert(request.key.operation);
        state.activity.push(Arc::clone(&work));
        Ok(QueryOperation {
            request,
            context,
            sdk: Arc::clone(&root.sdk),
            gnu: Arc::clone(&root.gnu.descriptions),
            deadline,
            work,
        })
    }
    pub(super) fn bootstrap_graph_complete_query(
        &mut self,
        completed: bootstrap_query::Completed,
    ) -> io::Result<()> {
        let request = completed.request().clone();
        let context = completed.context().clone();
        let Some(root) = self
            .jobs
            .get_mut(&(
                request.key.graph.query.binding.session.clone(),
                request.key.graph.query.binding.root_attempt,
            ))
            .and_then(|job| job.bootstrap_graph.as_mut())
        else {
            self.faulted = true;
            std::mem::forget(completed);
            return Err(refused("completed SDK query lost its original Root"));
        };
        let state = &mut root.queries;
        if state.active.as_ref() != Some(&request)
            || state.active_context.as_ref() != Some(&context)
        {
            state.failed = true;
            // Retain an unexpected genuine owner; no metadata-based disposal.
            std::mem::forget(completed);
            return Err(refused(
                "completed SDK query differs from original active operation",
            ));
        }
        match completed.into_retained() {
            Ok(retained) => {
                let diagnostic = retained.diagnostics();
                state.retained.push(retained);
                let binding = Binding {
                    session: request.key.graph.query.binding.session.clone(),
                    attempt: request.key.graph.query.binding.root_attempt,
                };
                if let Err(error) = self.journal.patch(
                    &binding,
                    aterm_json::json!({"bootstrap_query_last":diagnostic}),
                ) {
                    state.failed = true;
                    return Err(error);
                }
                state.active = None;
                state.active_context = None;
                state.next = state
                    .next
                    .checked_add(1)
                    .ok_or_else(|| refused("SDK query sequence overflow"))?;
                Ok(())
            }
            Err(original) => {
                state.failed = true;
                std::mem::forget(original);
                Err(refused(
                    "native SDK query closure did not close original work",
                ))
            }
        }
    }
    pub(super) fn bootstrap_graph_failed_query(
        &mut self,
        failed: bootstrap_query::ClosedFailed,
    ) -> io::Result<()> {
        let request = failed.request().clone();
        let context = failed.context().clone();
        let Some(root) = self
            .jobs
            .get_mut(&(
                request.key.graph.query.binding.session.clone(),
                request.key.graph.query.binding.root_attempt,
            ))
            .and_then(|job| job.bootstrap_graph.as_mut())
        else {
            self.faulted = true;
            std::mem::forget(failed);
            return Err(refused("failed SDK query lost its original Root"));
        };
        root.queries.failed = true;
        if root.queries.active.as_ref() != Some(&request)
            || root.queries.active_context.as_ref() != Some(&context)
        {
            std::mem::forget(failed);
            return Err(refused(
                "failed SDK query differs from original active operation",
            ));
        }
        match failed.into_retained() {
            Ok(retained) => {
                let diagnostic = retained.diagnostics();
                root.queries.retained.push(retained);
                let binding = Binding {
                    session: request.key.graph.query.binding.session.clone(),
                    attempt: request.key.graph.query.binding.root_attempt,
                };
                self.journal.patch(
                    &binding,
                    aterm_json::json!({"bootstrap_query_last":diagnostic}),
                )?;
                Ok(())
            }
            Err(original) => {
                std::mem::forget(original);
                Err(refused(
                    "failed SDK query has no positive native work closure",
                ))
            }
        }
    }
    pub(in crate::actor) fn submit_bootstrap_query(
        &mut self,
        peer: Peer,
        request: query::Request,
        stream: aterm_uds::CtlStream,
        started: Instant,
        caller_deadline: Instant,
    ) -> io::Result<()> {
        let operation = self.bootstrap_graph_begin_query(peer, request, started)?;
        let key = (
            operation.request().key.graph.query.binding.session.clone(),
            operation.request().key.graph.query.binding.root_attempt,
        );
        let job = self
            .jobs
            .get_mut(&key)
            .expect("same actor retains original query Job");
        let Custody::Fixed(cohort) = &mut job.custody else {
            std::mem::forget(operation);
            return Err(refused(
                "Bootstrap query lost its actual retained fixed cohort",
            ));
        };
        let query = bootstrap_query::Query::begin(
            operation,
            stream,
            &self.fence,
            &self.journal,
            cohort,
            &mut job.child,
            caller_deadline,
        );
        let root = job.bootstrap_graph.as_mut().expect("same original graph");
        if root.queries.running.is_some() {
            // The minted sequence forbids this; preserve actual unknown owners
            // rather than disposing a possible launched subgroup on an error.
            root.queries.failed = true;
            std::mem::forget(query);
            return Err(refused(
                "Bootstrap original query slot was already occupied",
            ));
        }
        root.queries.running = Some(Box::new(query));
        Ok(())
    }
    pub(in crate::actor::owner) fn tick_bootstrap_queries(&mut self) {
        self.tick_bootstrap_objects();
        let mut closed = Vec::new();
        for job in self.jobs.values_mut() {
            if let Some(token) = Self::poll_bootstrap_query(&self.fence, &self.journal, job) {
                closed.push(token);
            }
        }
        // Actual tokens retain their active guards while moving out of the
        // loop. No Root can settle between polling and consuming that owner.
        for token in closed {
            if self.consume_bootstrap_query(token).is_err() {
                self.faulted = true;
            }
        }
    }

    /// Refresh only this authenticated original Root's existing operation.
    /// Its ACK+physical EOF may already be queued before the next Root message,
    /// even though the actor's ordinary post-message tick has not run yet.
    /// No new request is retried and no transport observation constructs native
    /// closure: the same consuming query constructors remain mandatory.
    pub(super) fn refresh_bootstrap_query(
        &mut self,
        peer: Peer,
        key: &wire::Key,
    ) -> io::Result<()> {
        self.bootstrap_graph_context(peer, key)?;
        self.refresh_bootstrap_object(peer, key)?;
        let job_key = (
            key.query.binding.session.clone(),
            key.query.binding.root_attempt,
        );
        let closed = {
            let job = self
                .jobs
                .get_mut(&job_key)
                .ok_or_else(|| refused("original Bootstrap query Job unavailable"))?;
            Self::poll_bootstrap_query(&self.fence, &self.journal, job)
        };
        if let Some(closed) = closed {
            self.consume_bootstrap_query(closed)?;
        }
        Ok(())
    }

    fn consume_bootstrap_query(&mut self, token: ClosedQuery) -> io::Result<()> {
        match token {
            ClosedQuery::Complete(completed) => self.bootstrap_graph_complete_query(completed),
            ClosedQuery::Failed(failed) => self.bootstrap_graph_failed_query(failed),
        }
    }

    fn poll_bootstrap_query(
        fence: &aterm_capacity::leadership::Fence,
        journal: &journal::Journal,
        job: &mut Job,
    ) -> Option<ClosedQuery> {
        let root = job.bootstrap_graph.as_mut()?;
        let mut query = root.queries.running.take()?;
        let Custody::Fixed(cohort) = &mut job.custody else {
            root.queries.failed = true;
            root.queries.running = Some(query);
            return None;
        };
        let cancel = if job.cleanup_at.is_some() || job.events.cancelled() {
            Some(match job.cause {
                CompletionCause::Timeout => trust_resource_controller::CompletionCause::Timeout,
                CompletionCause::ResourceLimit => {
                    trust_resource_controller::CompletionCause::ResourceLimit
                }
                _ => trust_resource_controller::CompletionCause::Cancelled,
            })
        } else {
            None
        };
        let delivery_ready = query.poll(fence, journal, cohort, &mut job.child, cancel);
        // A false delivery hint can still follow genuine failed cleanup.
        // Both constructors consume original owners and return the same Query
        // when their independent native closure bookends are unavailable.
        let completed = if delivery_ready {
            query.into_completed()
        } else {
            Err(query)
        };
        match completed {
            Ok(completed) => Some(ClosedQuery::Complete(completed)),
            Err(query) => match query.into_closed_failed() {
                Ok(failed) => Some(ClosedQuery::Failed(failed)),
                Err(query) => {
                    root.queries.running = Some(query);
                    None
                }
            },
        }
    }
}
