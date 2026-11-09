// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Original fixed Bootstrap Root preparation and retained graph inputs. Wire
//! observations do not create CPU, GNU, SDK, source, output or release owners.
use super::*;
use std::{
    ffi::OsStr,
    os::{
        fd::{AsFd, OwnedFd},
        unix::{ffi::OsStrExt as _, fs::MetadataExt as _},
    },
    sync::{
        Arc,
        atomic::{AtomicBool, Ordering},
    },
};
use trust_resource_controller::bootstrap_graph as wire;

#[path = "resource_admission_bootstrap_graph/custody.rs"]
mod input_custody;
pub(super) use input_custody::OriginalOutput;
pub(crate) use input_custody::{EnrollmentAttachment, OutputTransfer, Preparing, Support};
#[path = "resource_admission_bootstrap_graph/query_gate.rs"]
mod query_gate;
pub(super) use query_gate::{ObjectOperation, QueryOperation};

pub(super) const MAX_ROOT_TIMEOUT_MS: u64 = 24 * 60 * 60 * 1000;

pub(super) fn purpose(request: &aterm_types::resource_admission::Submission) -> bool {
    request.argv.get(1..4).is_some_and(|args| {
        args == [
            b"--unverified".to_vec(),
            b"resource".to_vec(),
            b"bootstrap-runtime".to_vec(),
        ]
    })
}

pub(super) fn closed_request(request: &aterm_types::resource_admission::Submission) -> bool {
    purpose(request)
        && request.cpu_units > 0
        && matches!(request.timeout_ms, Some(1..=MAX_ROOT_TIMEOUT_MS))
        && request
            .argv
            .get(4)
            .is_some_and(|arg| arg == b"--bootstrap-shims")
        && request.argv.get(5).is_some_and(|path| absolute(path))
        && request.argv.get(6).is_some_and(|arg| arg == b"--")
        && request.argv.get(7..).is_some_and(|args| {
            !args.is_empty()
                && args.len() <= 1024
                && args
                    .iter()
                    .try_fold(0usize, |sum, arg| sum.checked_add(arg.len()))
                    .is_some_and(|bytes| bytes <= 1024 * 1024)
        })
}
fn absolute(path: &[u8]) -> bool {
    path.len() > 1
        && path.len() <= 4096
        && path[0] == b'/'
        && !path.contains(&0)
        && path[1..]
            .split(|b| *b == b'/')
            .all(|p| !p.is_empty() && p != b"." && p != b"..")
}
fn time(deadline: Instant) -> io::Result<()> {
    if Instant::now() >= deadline {
        Err(io::ErrorKind::TimedOut.into())
    } else {
        Ok(())
    }
}

/// SAME original handoff created by RetainedCohort::prepare_gnu. Its creating
/// cohort, not these descriptions or their metadata, retains every CPU right.
pub(super) struct Gnu {
    descriptions: Arc<[OwnedFd; 2]>,
    identity: wire::GnuIdentity,
    handoff: jobserver_utils::ScopedHandoff,
}
impl Gnu {
    fn new(original: aterm_capacity::retained_cohort::GnuHandoff) -> io::Result<Arc<Self>> {
        let (descriptions, path) = original.into_parts();
        let metadata =
            spawnfd::descriptor_metadata(std::os::fd::AsRawFd::as_raw_fd(&descriptions[0]))?;
        let identity = wire::GnuIdentity::NamedFifo {
            path: path.as_os_str().as_bytes().to_vec(),
            backing: trust_resource_controller::FileIdentity {
                device: metadata.dev(),
                inode: metadata.ino(),
            },
        };
        wire::transport::check_gnu_identity(&identity, &descriptions.each_ref().map(AsFd::as_fd))?;
        let handoff = jobserver_utils::ScopedHandoff::named_fifo([
            aterm_uds::kept_out(|| descriptions[0].try_clone())?,
            aterm_uds::kept_out(|| descriptions[1].try_clone())?,
        ])?;
        Ok(Arc::new(Self {
            descriptions: Arc::new(descriptions),
            identity,
            handoff,
        }))
    }
    pub(super) fn descriptions(&self) -> io::Result<[OwnedFd; 2]> {
        self.check()?;
        Ok([
            aterm_uds::kept_out(|| self.descriptions[0].try_clone())?,
            aterm_uds::kept_out(|| self.descriptions[1].try_clone())?,
        ])
    }
    pub(super) fn identity(&self) -> &wire::GnuIdentity {
        &self.identity
    }
    fn check(&self) -> io::Result<()> {
        wire::transport::check_gnu_identity(
            &self.identity,
            &self.descriptions.each_ref().map(AsFd::as_fd),
        )
    }
    fn matches(&self, request: &wire::Enrollment, descriptions: &[OwnedFd; 2]) -> io::Result<()> {
        self.check()?;
        if request.gnu != self.identity {
            return Err(refused("Bootstrap GNU differs from original cohort epoch"));
        }
        wire::transport::check_gnu(request, &descriptions.each_ref().map(AsFd::as_fd))
    }
}

pub(super) struct Root {
    prelude: crate::metadata_prelude::Owner,
    sdk: Arc<sdk::Keeper>,
    gnu: Arc<Gnu>,
    deadline: Instant,
    target: Option<crate::metadata_prelude::NoForkTarget>,
    inputs: input_custody::State,
    queries: query_gate::State,
}
impl Root {
    /// Only the original paid, unused RetainedCohort can supply this handoff.
    pub(super) fn new(
        command: &mut spawnfd::Command,
        original: aterm_capacity::retained_cohort::GnuHandoff,
        sdk: Arc<sdk::Keeper>,
        deadline: Instant,
    ) -> io::Result<(Self, spawnfd::Command)> {
        time(deadline)?;
        let queries = query_gate::State::new(command)?;
        sdk.validate()?;
        sdk.original_inputs()
            .require_bootstrap_graph_protocol_marker(
                &wire::BOOTSTRAP_GRAPH_PROTOCOL_MARKER,
                deadline,
            )?;
        let gnu = Gnu::new(original)?;
        let wire::GnuIdentity::NamedFifo { path, .. } = gnu.identity() else {
            return Err(refused("Bootstrap cohort has no original named GNU epoch"));
        };
        let flags = format!(
            "-j --jobserver-auth=fifo:{}",
            OsStr::from_bytes(path)
                .to_str()
                .ok_or_else(|| refused("GNU locator is not UTF8"))?
        );
        for key in ["MAKEFLAGS", "MFLAGS", "CARGO_MAKEFLAGS"] {
            command.env(key, &flags);
        }
        command.env(
            jobserver_utils::DESCENDANT_HANDOFF_ENV,
            gnu.handoff.advertisement(),
        );
        command.env("ATERM_SDK_LEASE_V3", sdk::TARGET.to_string());
        command.env(
            wire::HOST_STORE_FD_ENV,
            wire::HOST_STORE_TARGET_FD.to_string(),
        );
        let (mut prelude, mut child) = crate::metadata_prelude::Owner::new(
            command,
            &[3, 4, sdk::TARGET as u32, wire::HOST_STORE_TARGET_FD],
            deadline,
        )?;
        if child.guard_targets().next().is_some() {
            return Err(refused(
                "Bootstrap cannot replace original inherited guards",
            ));
        }
        let [read, write] = gnu.descriptions()?;
        child.inherit_guards_at(vec![
            (read, 3),
            (write, 4),
            (sdk.descriptor()?, sdk::TARGET),
            (
                crate::runtime::workload_store_description()?,
                wire::HOST_STORE_TARGET_FD as i32,
            ),
            prelude.extra()?,
        ])?;
        Ok((
            Self {
                prelude,
                sdk,
                gnu,
                deadline,
                target: None,
                inputs: input_custody::State::default(),
                queries,
            },
            child,
        ))
    }
    pub(super) fn configured(&mut self, command: &spawnfd::Command) -> io::Result<()> {
        self.prelude.configured(command)
    }
    pub(super) fn bind_child(&self, pid: u32) -> io::Result<()> {
        self.gnu.handoff.bind_child(pid)
    }
    pub(super) fn poll(&mut self, child: &mut spawnfd::Child) -> io::Result<bool> {
        time(self.deadline)?;
        let sdk = Arc::clone(&self.sdk);
        if !self.prelude.poll(child, |pid| sdk.validate_loaded(pid))? {
            return Ok(false);
        }
        if self.target.is_none() {
            self.target = Some(
                self.prelude
                    .take_retirement()
                    .ok_or_else(|| refused("Bootstrap original no-fork target missing"))?,
            );
        }
        self.validate(child.id(), self.deadline)?;
        Ok(true)
    }
    pub(super) fn deadline(&self) -> Instant {
        self.deadline
    }
    pub(super) fn no_fork_target(&self) -> io::Result<&crate::metadata_prelude::NoForkTarget> {
        self.target
            .as_ref()
            .ok_or_else(|| refused("Bootstrap original two-stop target unavailable"))
    }
    pub(super) fn preparation_finished(&self) -> bool {
        self.inputs.preparation_finished() && self.queries.preparation_finished()
    }
    pub(super) fn native_queries_drained(&self) -> bool {
        self.queries.native_queries_drained()
    }
    pub(super) fn query_sampling_group(&self) -> io::Result<Option<i32>> {
        self.queries.sampling_group()
    }
    pub(super) fn query_failed(&self) -> bool {
        self.queries.failed()
    }
    pub(super) fn release(&mut self, closed: &settlement::ClosedRoot) -> io::Result<()> {
        self.inputs.release(closed)?;
        self.queries.release(closed)
    }
    fn validate(&self, pid: u32, deadline: Instant) -> io::Result<()> {
        time(self.deadline.min(deadline))?;
        self.sdk.validate_loaded(pid)?;
        self.gnu.check()?;
        if !crate::metadata_prelude::no_fork(pid)? {
            return Err(refused("Bootstrap Root lost native no-fork confinement"));
        }
        time(self.deadline.min(deadline))
    }
}

impl Owner {
    pub(in crate::actor) fn bootstrap_graph_context(
        &mut self,
        peer: Peer,
        key: &wire::Key,
    ) -> io::Result<trust_resource_controller::ContextReply> {
        key.validate()?;
        let context = self.context(peer, &key.query)?;
        let job = self
            .jobs
            .get_mut(&(
                key.query.binding.session.clone(),
                key.query.binding.root_attempt,
            ))
            .ok_or_else(|| refused("original Bootstrap Job unavailable"))?;
        let root = job
            .bootstrap_graph
            .as_ref()
            .ok_or_else(|| refused("Job has no original Bootstrap preparation"))?;
        if !matches!(job.custody, Custody::Fixed(_))
            || peer.pid != job.child.child_mut().id()
            || peer.birth != job.root_birth
            || !root
                .no_fork_target()?
                .matches(peer.pid, peer.birth, job.child.cohort().group())
            || context.context.sdk.is_none()
        {
            return Err(refused(
                "Bootstrap support requires the actual fixed original Root and SDK",
            ));
        }
        root.validate(peer.pid, root.deadline())?;
        root.queries.check_key(key)?;
        Ok(context)
    }
    pub(super) fn tick_bootstrap_roots(&mut self) {
        use aterm_capacity::cohort::launch::CohortSignal;
        use aterm_types::resource_admission::Event;
        for job in self.jobs.values_mut() {
            if job.launched || job.reaped || job.cleanup_at.is_some() {
                continue;
            }
            if job.bootstrap_graph.is_none() {
                continue;
            }
            let result = (|| -> io::Result<bool> {
                if job.events.cancelled() {
                    return Err(io::ErrorKind::Interrupted.into());
                }
                let deadline = {
                    let root = job
                        .bootstrap_graph
                        .as_mut()
                        .expect("original Bootstrap Root");
                    if !root.poll(job.child.child_mut())? {
                        return Ok(false);
                    }
                    root.deadline()
                };
                self.journal
                    .update(&job.binding, "running", aterm_json::json!({}))?;
                job::inspection(&self.fence, &self.debt, job)?;
                time(deadline)?;
                if job.events.cancelled() {
                    return Err(io::ErrorKind::Interrupted.into());
                }
                let (monotonic_ns, unix_ns) = start::clocks()?;
                let started = Instant::now();
                let remaining = deadline
                    .checked_duration_since(started)
                    .ok_or(io::ErrorKind::TimedOut)?;
                job.child.child_mut().resume()?;
                job.started = started;
                job.timeout = Some(remaining);
                job.launched = true;
                let _ = job.events.emit(Event::Started {
                    pid: job.child.child_mut().id(),
                    birth: job.root_birth,
                    monotonic_ns,
                    unix_ns,
                });
                Ok(true)
            })();
            if let Err(error) = result {
                job.events.abandon();
                job.cause = if error.kind() == io::ErrorKind::TimedOut {
                    CompletionCause::Timeout
                } else {
                    CompletionCause::Cancelled
                };
                job.cleanup_at = Some(Instant::now());
                if let Err(error) = job.child.request_group(CohortSignal::Kill) {
                    self.journal.fault(&mut self.faulted, "root-kill", error);
                    let _ = job.child.child_mut().kill();
                }
            }
        }
    }
}
