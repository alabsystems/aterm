// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Borrow the actual original claim and child. A member never owns a second
//! memory grant, and cleanup never reconstructs control from an observation.
use super::*;
use aterm_capacity::cohort::launch::{CohortChild, LaunchError};
use aterm_capacity::retained_cohort::CancelError;
use std::os::fd::{AsRawFd, OwnedFd};
use std::os::unix::fs::{FileTypeExt as _, MetadataExt as _};
use std::sync::Arc;

/// Actual pre-GO containment and the original scheduling descriptions. No wire
/// value can manufacture the native target witness.
pub(super) struct ProducerInputs {
    target: crate::metadata_prelude::NoForkTarget,
    gnu: GnuDescriptions,
    original: Option<OriginalProducer>,
    operation: Option<trust_resource_controller::workload::host::contained::Request>,
    transport: std::sync::Mutex<RetainedControl>,
}
enum RetainedControl {
    Absent,
    Pending(OwnedFd),
    Settled(OwnedFd),
    Closed,
}
enum GnuDescriptions {
    Owned(Vec<OwnedFd>),
    OriginalPhase(Arc<[OwnedFd; 2]>),
}
impl GnuDescriptions {
    fn descriptions(&self) -> &[OwnedFd] {
        match self {
            Self::Owned(fds) => fds,
            Self::OriginalPhase(fds) => fds.as_slice(),
        }
    }
}
struct OriginalProducer {
    root: Binding,
    root_identity: aterm_uds::process::Identity,
    member: [u8; 16],
    loan: Option<parent_loan::Edge>,
}
impl ProducerInputs {
    pub(super) fn new(
        target: crate::metadata_prelude::NoForkTarget,
        gnu: Vec<OwnedFd>,
    ) -> io::Result<Self> {
        let inputs = Self {
            target,
            gnu: GnuDescriptions::Owned(gnu),
            original: None,
            operation: None,
            transport: std::sync::Mutex::new(RetainedControl::Absent),
        };
        inputs.check_gnu()?;
        Ok(inputs)
    }
    pub(super) fn original_phase(
        target: crate::metadata_prelude::NoForkTarget,
        gnu: Arc<[OwnedFd; 2]>,
    ) -> io::Result<Self> {
        let inputs = Self {
            target,
            gnu: GnuDescriptions::OriginalPhase(gnu),
            original: None,
            operation: None,
            transport: std::sync::Mutex::new(RetainedControl::Absent),
        };
        inputs.check_gnu()?;
        Ok(inputs)
    }
    pub(super) fn contained(
        mut self,
        request: trust_resource_controller::workload::host::contained::Request,
        transport: OwnedFd,
    ) -> io::Result<Self> {
        request.validate()?;
        self.check_named_gnu(request.gnu)?;
        self.operation = Some(request);
        self.transport = std::sync::Mutex::new(RetainedControl::Pending(transport));
        Ok(self)
    }
    fn check_named_gnu(&self, expected: trust_resource_controller::FileIdentity) -> io::Result<()> {
        if self.gnu.descriptions().len() != 2 {
            return Err(refused("contained producer requires original FIFO pair"));
        }
        for fd in self.gnu.descriptions() {
            let file = std::fs::File::from(fd.try_clone()?);
            let metadata = file.metadata()?;
            if metadata.dev() != expected.device || metadata.ino() != expected.inode {
                return Err(refused("contained producer differs from original GNU FIFO"));
            }
        }
        self.check_gnu()
    }
    fn check_gnu(&self) -> io::Result<()> {
        // Role6 has no GNU delegation. Named FIFO may use one RDWR original;
        // numerical pipe and ordinary named FIFO retain their full pair.
        if self.gnu.descriptions().len() > 2 {
            return Err(refused("producer GNU population differs"));
        }
        for (index, fd) in self.gnu.descriptions().iter().enumerate() {
            let file = std::fs::File::from(fd.try_clone()?);
            let metadata = file.metadata()?;
            let flags = unsafe { libc::fcntl(fd.as_raw_fd(), libc::F_GETFL) };
            let expected = if self.gnu.descriptions().len() == 1 {
                libc::O_RDWR
            } else if index == 0 {
                libc::O_RDONLY
            } else {
                libc::O_WRONLY
            };
            if !metadata.file_type().is_fifo()
                || metadata.uid() != aterm_uds::peer::our_uid()
                || flags < 0
                || ![expected, libc::O_RDWR].contains(&(flags & libc::O_ACCMODE))
            {
                return Err(refused("original producer GNU description changed"));
            }
        }
        Ok(())
    }
    pub(super) fn check_child(&self, child: &CohortChild, pid: u32, birth: u64) -> io::Result<()> {
        if !self.target.matches(pid, birth, child.cohort().group()) {
            return Err(refused(
                "native producer containment belongs to another child",
            ));
        }
        self.check_gnu()
    }
}

/// A private, non-Clone one-use native result. Construction follows original
/// owning reap and successful retire_subgroup under the original custody.
/// Group-only retirement, Finished, EOF and a saved status cannot create it.
pub(super) struct ProducerRetirement {
    original: OriginalProducer,
    inputs: ProducerInputs,
}

/// Only consuming the actual retirement in the SAME successful custody return
/// creates this one-use issuer. The Job shares lifetime of the same physical
/// inputs; that Arc is not another issuer or a capacity-release constructor.
pub(super) struct ReturnedProducer {
    retirement: std::sync::Arc<ProducerRetirement>,
}
impl ReturnedProducer {
    pub(super) fn settle(
        &self,
        journal: &super::journal::Journal,
        binding: &Binding,
    ) -> io::Result<()> {
        if binding.session != self.retirement.original.root.session
            || binding.attempt != self.retirement.original.member
        {
            return Err(refused("settlement differs from actual returned producer"));
        }
        journal.settle(binding)?;
        let mut transport = self
            .retirement
            .inputs
            .transport
            .lock()
            .map_err(|_| refused("original contained transport poisoned"))?;
        let original = std::mem::replace(&mut *transport, RetainedControl::Closed);
        *transport = match original {
            RetainedControl::Pending(fd) => RetainedControl::Settled(fd),
            other => other,
        };
        Ok(())
    }
    pub(super) fn completion(
        &self,
        request: &trust_resource_controller::workload::host::contained::Request,
        completion: trust_resource_controller::workload::host::Completion,
    ) -> io::Result<trust_resource_controller::workload::host::contained::Completion> {
        let original = &self.retirement.original;
        if self.retirement.inputs.operation.as_ref() != Some(request)
            || request.produced.key.root_attempt != original.root.attempt
            || request.produced.member.member_attempt != original.member
            || completion.completed.finished.root_attempt != original.root.attempt
            || completion.completed.finished.member_attempt != original.member
            || completion.phase != request.produced.key.ordinal
            || completion.kind != request.produced.kind
        {
            return Err(refused(
                "contained issuer belongs to another original operation",
            ));
        }
        self.retirement.inputs.check_named_gnu(request.gnu)?;
        Ok(
            trust_resource_controller::workload::host::contained::Completion {
                operation: request.operation,
                request_sha256: aterm_digest::Sha256::digest(request.binding_preimage()?),
                gnu: request.gnu,
                completion,
            },
        )
    }
}

impl ProducerRetirement {
    pub(super) fn close_exhausted_transport(&self) -> io::Result<()> {
        let mut control = self
            .inputs
            .transport
            .try_lock()
            .map_err(|_| refused("original transport unavailable"))?;
        let RetainedControl::Settled(fd) = &*control else {
            return Ok(());
        };
        let mut byte = 0u8;
        // Observe only the SAME accepted description. EOF never returns CPU,
        // GNU, SDK or a phase: the actual typed retirement and journal settlement
        // already happened, and their original owners stay in this same Job.
        let result = unsafe {
            libc::recv(
                fd.as_raw_fd(),
                (&mut byte as *mut u8).cast(),
                1,
                libc::MSG_PEEK | libc::MSG_DONTWAIT,
            )
        };
        if result == 0 {
            *control = RetainedControl::Closed;
            return Ok(());
        }
        if result > 0 {
            return Err(refused("trailing contained control bytes"));
        }
        let error = io::Error::last_os_error();
        if matches!(
            error.kind(),
            io::ErrorKind::WouldBlock | io::ErrorKind::Interrupted
        ) {
            Ok(())
        } else {
            Err(error)
        }
    }
}

#[cfg(all(test, target_os = "macos"))]
impl ProducerRetirement {
    pub(super) fn returned_fixture(self: Box<Self>) -> Box<ReturnedProducer> {
        Box::new(ReturnedProducer {
            retirement: std::sync::Arc::from(self),
        })
    }
    // A fixture owns a real original Claim/root/subgroup. It performs the same
    // native retirement here; no status or test observation mints the result.
    pub(super) fn retire_fixture(
        claim: &mut aterm_capacity::Claim,
        root: &mut CohortChild,
        child: &mut CohortChild,
        inputs: &mut Option<ProducerInputs>,
        binding: Binding,
    ) -> io::Result<Self> {
        if !claim.cohort_bound(root.cohort()) || root.child_mut().exited()? {
            return Err(refused("fixture original root unavailable"));
        }
        inputs
            .as_ref()
            .ok_or_else(|| {
                io::Error::new(
                    io::ErrorKind::Unsupported,
                    "group-only producer has no enforced containment",
                )
            })?
            .check_gnu()?;
        if !child.child_mut().is_reaped() {
            return Err(refused("original producer child has not been reaped"));
        }
        claim.retire_subgroup(child)?;
        let mut inputs = inputs.take().ok_or_else(|| refused("inputs consumed"))?;
        let original = inputs
            .original
            .take()
            .ok_or_else(|| refused("original pre-GO producer owner missing"))?;
        if original.root != binding {
            return Err(refused("fixture original root differs"));
        }
        Ok(Self { original, inputs })
    }
    pub(super) fn check_fixture(
        &self,
        root: &mut CohortChild,
        binding: &Binding,
    ) -> io::Result<()> {
        if &self.original.root != binding
            || self.original.member != binding.attempt
            || self.original.loan.is_some()
            || aterm_uds::process::read_identity(root.child_mut().id() as i32)
                != Some(self.original.root_identity)
            || root.child_mut().exited()?
        {
            return Err(refused(
                "fixture retirement belongs to another original root",
            ));
        }
        self.inputs.check_gnu()
    }
}

#[cfg(all(test, target_os = "macos"))]
impl ProducerInputs {
    pub(super) fn bind_fixture(
        &mut self,
        root: &mut CohortChild,
        binding: Binding,
    ) -> io::Result<()> {
        if self.original.is_some() || root.child_mut().exited()? {
            return Err(refused("original producer cannot be rebound"));
        }
        let root_identity = aterm_uds::process::read_identity(root.child_mut().id() as i32)
            .ok_or_else(|| refused("fixture original root unavailable"))?;
        self.original = Some(OriginalProducer {
            member: binding.attempt,
            root: binding,
            root_identity,
            loan: None,
        });
        Ok(())
    }
}

pub(super) struct Borrowed<'a> {
    pub job: &'a mut Job,
    pub fence: &'a Fence,
    pub debt: &'a ExternalDebt,
    pub peer: Peer,
    pub proof: Option<&'a crate::resource_producer::TransportProof>,
    pub member: [u8; 16],
    pub loan: Option<parent_loan::Edge>,
    pub journal: &'a journal::Journal,
}
impl member::MemberCustody for Borrowed<'_> {
    fn bind_producer(&mut self, inputs: &mut ProducerInputs) -> io::Result<()> {
        self.validate()?;
        if inputs.original.is_some() {
            return Err(refused("original producer cannot be rebound"));
        }
        let root_identity =
            aterm_uds::process::read_identity(self.job.child.child_mut().id() as i32)
                .ok_or_else(|| refused("original producer root unavailable before GO"))?;
        if root_identity.birth.0 != self.job.root_birth {
            return Err(refused("original producer root changed"));
        }
        if let Some(operation) = &inputs.operation {
            if operation.produced.member.member_attempt != self.member
                || operation.produced.key.root_attempt != self.job.binding.attempt
            {
                return Err(refused("contained operation differs before original GO"));
            }
            inputs.check_named_gnu(operation.gnu)?;
        }
        inputs.original = Some(OriginalProducer {
            root: self.job.binding.clone(),
            root_identity,
            member: self.member,
            loan: self.loan,
        });
        Ok(())
    }
    fn retire_producer(
        &mut self,
        child: &mut CohortChild,
        inputs: &mut Option<ProducerInputs>,
    ) -> io::Result<ProducerRetirement> {
        // Root/caller loss is not successful producer retirement. Preserve the
        // candidate and its original descriptors across every ambiguous error.
        self.validate()?;
        let inputs_ref = inputs
            .as_ref()
            .ok_or_else(|| refused("native producer inputs missing"))?;
        inputs_ref.check_gnu()?;
        if !child.child_mut().is_reaped() {
            return Err(refused("original producer child has not been reaped"));
        }
        let original = inputs_ref
            .original
            .as_ref()
            .ok_or_else(|| refused("pre-GO producer owner missing"))?;
        self.check_producer(original)?;
        self.retire(child)?;
        self.validate()?;
        let mut inputs = inputs
            .take()
            .ok_or_else(|| refused("native producer witness consumed"))?;
        let original = inputs
            .original
            .take()
            .ok_or_else(|| refused("native producer owner consumed"))?;
        Ok(ProducerRetirement { original, inputs })
    }
    fn return_producer(
        &mut self,
        retirement: Box<ProducerRetirement>,
    ) -> Result<member::ProducerReturn, (io::Error, Box<ProducerRetirement>)> {
        let returned = (|| {
            self.validate()?;
            self.check_producer(&retirement.original)?;
            retirement.inputs.check_gnu()?;
            self.return_unit(member::ReturnReceipt::ScopeClosed)
        })();
        match returned {
            Ok(member::ReturnProgress::Pending) => Ok(member::ProducerReturn::Pending(retirement)),
            Ok(member::ReturnProgress::Returned) => {
                let retirement: std::sync::Arc<ProducerRetirement> =
                    std::sync::Arc::from(retirement);
                if retirement.inputs.operation.is_some() {
                    self.job
                        .contained_retirements
                        .insert(self.member, std::sync::Arc::clone(&retirement));
                }
                Ok(member::ProducerReturn::Returned(Box::new(
                    ReturnedProducer { retirement },
                )))
            }
            Err(error) => Err((error, retirement)),
        }
    }
    fn before_arm(&mut self) -> io::Result<()> {
        if let Some(edge) = self.loan {
            self.validate()?;
            parent_loan::intent(self.job, self.peer, edge, self.journal)?;
        }
        Ok(())
    }
    fn before_go(&mut self) -> io::Result<()> {
        if let Some(edge) = self.loan {
            parent_loan::lend(self.job, edge, self.journal)?;
            // The durable write may block. Recheck the actual producer/root
            // afterward, before the caller stamps GO and checks cancellation.
            self.validate()?;
        }
        Ok(())
    }
    fn commit_started(
        &mut self,
        binding: &Binding,
        started: &member::Started,
        requested: Option<member::RequestedStartLimits>,
    ) -> io::Result<()> {
        self.validate()?;
        if binding.session != self.job.binding.session
            || binding.attempt != self.member
            || started.pid <= 1
            || started.birth == 0
            || started.monotonic_ns == 0
            || started.unix_ns == 0
            || requested.is_some_and(|limits| limits.units == 0 || limits.footprint_mib == 0)
        {
            return Err(refused("member Started journal binding changed"));
        }
        // The owner bound this original child before dispatch. Select the known
        // running phase with its exact clock before the first GO/Run byte;
        // this intent cannot reconstruct execution or cold resume authority.
        self.journal.update(
            binding,
            "member_running",
            aterm_json::json!({
                "pid": started.pid,
                "birth": started.birth,
                "monotonic_ns": started.monotonic_ns,
                "unix_ns": started.unix_ns,
                "requested_units": requested.map(|limits| limits.units),
                "requested_footprint_mib": requested.map(|limits| limits.footprint_mib),
            }),
        )?;
        // No Run byte may be offered on a root/fence lost during durable I/O.
        self.validate()
    }
    fn launch(&mut self, command: &mut spawnfd::Command) -> Result<CohortChild, LaunchError> {
        self.validate().map_err(LaunchError::Refused)?;
        match &mut self.job.custody {
            Custody::Fixed(custody) => {
                custody.launch_subgroup(self.fence, &mut self.job.child, command)
            }
            Custody::Cooperative(claim) => claim.launch_subgroup(&mut self.job.child, command),
        }
    }
    fn bind(&mut self, child: spawnfd::Spawned) -> Result<CohortChild, LaunchError> {
        // A dead caller/root forbids GO, but must not discard an actual held
        // child whose durable BIND needs retrying for cancellation and drain.
        match &mut self.job.custody {
            Custody::Fixed(custody) => custody.bind_subgroup(self.fence, child),
            Custody::Cooperative(claim) => {
                if let Err(error) = self.fence.check_claim(claim) {
                    return Err(LaunchError::Unbound {
                        spawned: Box::new(child),
                        error,
                    });
                }
                claim.bind_subgroup(child)
            }
        }
    }
    fn cancel_no_child(&mut self, proof: spawnfd::NotSpawned) -> Result<(), CancelError> {
        match &mut self.job.custody {
            Custody::Fixed(custody) => custody.cancel_subgroup(self.fence, proof),
            Custody::Cooperative(claim) => {
                if let Err(error) = self.fence.check_claim(claim) {
                    return Err(CancelError::Refused {
                        error,
                        proof: Box::new(proof),
                    });
                }
                // Once accepted the claim retains the original receipt before
                // its first persistence attempt, including on an I/O failure.
                claim.cancel_subgroup(proof).map_err(CancelError::Retained)
            }
        }
    }
    fn retire(&mut self, child: &CohortChild) -> io::Result<()> {
        match &mut self.job.custody {
            Custody::Fixed(custody) => custody.retire_subgroup(self.fence, child),
            Custody::Cooperative(claim) => {
                self.fence.check_claim(claim)?;
                claim.retire_subgroup(child)
            }
        }
    }
    fn validate(&mut self) -> io::Result<()> {
        if let Some(proof) = self.proof {
            proof.validate()?;
        }
        self.fence.check_current()?;
        if !self.job.launched
            || self.job.reaped
            || self.job.is_settled()
            || self.job.cleanup_at.is_some()
            || self.job.child.child_mut().exited()?
            || !members::covered(self.job, self.peer)
            || !members::trusted_caller(self.job, self.peer)
        {
            return Err(refused(
                "original member caller/root is no longer executable",
            ));
        }
        job::inspection(self.fence, self.debt, self.job)?;
        Ok(())
    }
    fn return_unit(
        &mut self,
        receipt: member::ReturnReceipt,
    ) -> io::Result<member::ReturnProgress> {
        if parent_loan::outgoing(self.job, self.member).is_some() {
            return Err(refused("member still owns an outgoing CPU continuation"));
        }
        if let Some(edge) = self.loan {
            self.fence.check_current()?;
            parent_loan::returned(self.job, edge, receipt, self.journal)?;
        } else if let Some(workload) = self.job.workload.as_mut() {
            if workload.foreign_member(self.member) {
                // This private registered member consumed the already parked
                // phase, not a compiler-bank loan. The actual scope receipt
                // above returns no fresh CPU or GNU unit.
                return Ok(member::ReturnProgress::Returned);
            }
            // The owning state machine supplies this only after no ARM or
            // positive native scope closure. A nonblocking reply keeps the
            // original resources and event writer until the SAME bank ACK.
            if self.job.reaped {
                workload.host_closed_after_root_exit(self.member);
            } else {
                return workload.return_member(
                    self.member,
                    self.job
                        .timeout
                        .map(|timeout| self.job.started + timeout)
                        .unwrap_or_else(Instant::now),
                );
            }
        }
        Ok(member::ReturnProgress::Returned)
    }
}

impl Borrowed<'_> {
    fn check_producer(&mut self, original: &OriginalProducer) -> io::Result<()> {
        if original.root != self.job.binding
            || original.member != self.member
            || original.loan != self.loan
            || aterm_uds::process::read_identity(self.job.child.child_mut().id() as i32)
                != Some(original.root_identity)
        {
            return Err(refused(
                "producer retirement belongs to another original custody",
            ));
        }
        Ok(())
    }
}
