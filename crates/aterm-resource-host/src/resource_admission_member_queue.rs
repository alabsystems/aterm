// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Preserve authorized rights across nonblocking admission. Each attempt has
//! one journal identity; busy never asks a caller to replay execution.
use super::*;
use crate::subgroup_delegation::Prepared;
use aterm_ctl::resource_subgroup_client::{AuthorizedSubgroup, Delegations, Rights};
use aterm_types::resource_admission::Event;
use aterm_types::resource_subgroup::Request;
use std::os::{
    fd::{AsFd as _, OwnedFd},
    unix::ffi::OsStrExt as _,
};

enum Purpose {
    Member {
        accounted: bool,
    },
    Sealed(Box<metadata::Purpose>),
    Loan {
        edge: parent_loan::Edge,
        accounted: bool,
    },
    Linker(Box<trust_resource_controller::linker::Request>),
}
#[derive(Default)]
pub(super) struct ProxyMode {
    pub loan: Option<parent_loan::Edge>,
    pub accounted: bool,
}

struct Pending {
    request: Request,
    peer: Peer,
    stdin: OwnedFd,
    stdout: OwnedFd,
    stderr: OwnedFd,
    delegated: Delegations,
    events: job::Events,
    sealed: Option<metadata::Purpose>,
    proof: Option<crate::resource_producer::TransportProof>,
    arg0: Option<Vec<u8>>,
    timeout: Option<Duration>,
    named_targets: Option<[u32; 2]>,
    loan: Option<parent_loan::Edge>,
    linker: Option<trust_resource_controller::linker::Request>,
}
struct Preparing {
    // Declaration order joins the worker before returning any paid scheduling
    // fields on coordinator unwind, not just the normal cancellation branch.
    worker: preparation::Worker<io::Result<metadata::Command>>,
    pending: Box<Pending>,
    scheduling: Prepared,
    cancelled: bool,
}
struct Active {
    attempt: member::OwnedAttempt<metadata::Resources>,
    peer: Peer,
    proof: Option<crate::resource_producer::TransportProof>,
    cpu: Option<u32>,
    cpu_target: Option<u32>,
    loan: Option<parent_loan::Edge>,
    sdk: Option<trust_resource_controller::SdkCustody>,
    bound: bool,
    terminal: bool,
    settled: bool,
    retired: bool,
    drained: bool,
}
enum State {
    Produced(Box<workload::ProducedMember>),
    Pending(Box<Pending>),
    Preparing(Box<Preparing>),
    Active(Box<Active>),
    Refused {
        events: Box<job::Events>,
        settled: bool,
    },
    Vacant,
}
pub(super) struct Slot {
    binding: Binding,
    units: u32,
    state: State,
}
impl Slot {
    #[cfg(all(test, target_os = "macos", target_arch = "aarch64"))]
    pub(super) fn close_linker_lifeline(&mut self) -> bool {
        match &mut self.state {
            State::Active(active) => active
                .attempt
                .test_resources_mut()
                .is_some_and(metadata::Resources::close_linker_lifeline),
            _ => false,
        }
    }
    pub(super) fn produced(binding: Binding, member: workload::ProducedMember) -> Self {
        Self {
            binding,
            units: 0,
            state: State::Produced(Box::new(member)),
        }
    }
    pub(super) fn is_produced(&self) -> bool {
        matches!(self.state, State::Produced(_))
    }
    pub(super) fn workload_primary(&mut self, peer: Peer) -> Option<(u32, u32)> {
        match &mut self.state {
            State::Active(active) => {
                if !active.retired
                    && active.attempt.identity() == Some((peer.pid, peer.birth))
                    && active.attempt.covers(peer)
                {
                    Some((active.cpu?, active.cpu_target?))
                } else {
                    None
                }
            }
            _ => None,
        }
    }
    pub(super) fn loan_parent(&mut self, peer: Peer, edge: parent_loan::Edge) -> bool {
        self.binding.attempt == edge.parent
            && match &mut self.state {
                State::Active(active) => {
                    !active.retired
                        && active.cpu == Some(edge.cpu)
                        && active.attempt.identity() == Some((peer.pid, peer.birth))
                        && active.attempt.covers(peer)
                }
                _ => false,
            }
    }
    #[cfg(all(test, target_os = "macos"))]
    pub(super) fn audit_to(&mut self, file: std::fs::File) {
        let State::Pending(pending) = &mut self.state else {
            panic!("audit attaches before admission");
        };
        pending.events.audit_to(file);
    }
    pub(super) fn covers(&mut self, peer: Peer) -> bool {
        match &mut self.state {
            State::Produced(member) => member.covers(peer),
            State::Active(active) => active.attempt.covers(peer),
            _ => false,
        }
    }
    pub(super) fn group(&self) -> Option<i32> {
        match &self.state {
            State::Produced(member) => member.group(),
            State::Active(active) => active.attempt.group(),
            _ => None,
        }
    }
    pub(super) fn sdk(&self) -> Option<trust_resource_controller::SdkCustody> {
        match &self.state {
            State::Active(active) if !active.retired => active.sdk.clone(),
            _ => None,
        }
    }
    pub(super) fn active_cpu(&self) -> Option<u32> {
        match &self.state {
            State::Active(active) if !active.retired => active.cpu,
            State::Preparing(preparing) => preparing.scheduling.cpu_slot(),
            _ => None,
        }
    }
    fn active_units(&self) -> u32 {
        match &self.state {
            // Keep the parked parent's ceiling reservation charged. Its exact
            // continuation consumes that same unit, never another entitlement.
            State::Active(active) if !active.retired && active.loan.is_none() => self.units,
            State::Preparing(_) => self.units,
            _ => 0,
        }
    }
    pub(super) fn subject(&self) -> Option<aterm_types::resource_inspection::Subject> {
        if let State::Produced(member) = &self.state {
            return member.subject();
        }
        let State::Active(active) = &self.state else {
            return None;
        };
        let (pid, birth) = active.attempt.identity()?;
        Some(
            aterm_types::resource_inspection::Subject::RegisteredMember {
                pid,
                birth,
                attempt: self.binding.attempt,
            },
        )
    }
}
impl member::MemberResources for Prepared {
    fn configure(&mut self, command: &mut spawnfd::Command) -> io::Result<()> {
        Prepared::configure(self, command)
    }
    fn bind_child(&mut self, pid: u32) -> io::Result<()> {
        Prepared::bind_child(self, pid)
    }
}
impl Owner {
    pub(in super::super) fn submit_linker(&mut self, incoming: crate::linker_transport::Incoming) {
        let timeout = incoming.sealed.member.timeout_ns.map(Duration::from_nanos);
        self.submit_member_inner(
            incoming.subgroup,
            Purpose::Linker(Box::new(incoming.sealed)),
            None,
            None,
            timeout,
            None,
        );
    }
    pub(in super::super) fn submit_member(&mut self, incoming: AuthorizedSubgroup) {
        let timeout = incoming
            .request
            .member
            .timeout_ms
            .map(Duration::from_millis);
        self.submit_member_proved(incoming, None, None, timeout, None, ProxyMode::default());
    }
    pub(in super::super) fn submit_metadata(
        &mut self,
        incoming: crate::metadata_transport::Incoming,
    ) {
        let timeout = incoming.sealed.member.timeout_ns.map(Duration::from_nanos);
        self.submit_member_inner(
            incoming.subgroup,
            Purpose::Sealed(Box::new(metadata::Purpose::Metadata(incoming.sealed))),
            None,
            None,
            timeout,
            None,
        );
    }
    pub(in super::super) fn submit_resolver_query(
        &mut self,
        incoming: crate::resolver_transport::Incoming,
    ) {
        let timeout = incoming.sealed.member.timeout_ns.map(Duration::from_nanos);
        self.submit_member_inner(
            incoming.subgroup,
            Purpose::Sealed(Box::new(metadata::Purpose::Resolver(incoming.sealed))),
            None,
            None,
            timeout,
            None,
        );
    }
    pub(in super::super) fn submit_workload_query(
        &mut self,
        incoming: crate::workload_transport::Incoming,
    ) {
        let timeout = incoming.sealed.member.timeout_ns.map(Duration::from_nanos);
        self.submit_member_inner(
            incoming.subgroup,
            Purpose::Sealed(Box::new(metadata::Purpose::WorkloadQuery(incoming.sealed))),
            None,
            None,
            timeout,
            None,
        );
    }
    pub(in super::super) fn submit_native_job_query(
        &mut self,
        incoming: crate::workload_transport::NativeIncoming,
    ) {
        let timeout = incoming.sealed.member.timeout_ns.map(Duration::from_nanos);
        self.submit_member_inner(
            incoming.subgroup,
            Purpose::Sealed(Box::new(metadata::Purpose::NativeJobQuery(incoming.sealed))),
            None,
            None,
            timeout,
            None,
        );
    }
    pub(super) fn submit_member_proved(
        &mut self,
        incoming: AuthorizedSubgroup,
        proof: Option<crate::resource_producer::TransportProof>,
        arg0: Option<Vec<u8>>,
        timeout: Option<Duration>,
        named_targets: Option<[u32; 2]>,
        mode: ProxyMode,
    ) {
        let purpose = match mode.loan {
            None => Purpose::Member {
                accounted: mode.accounted,
            },
            Some(edge) => Purpose::Loan {
                edge,
                accounted: mode.accounted,
            },
        };
        self.submit_member_inner(incoming, purpose, proof, arg0, timeout, named_targets);
    }
    fn submit_member_inner(
        &mut self,
        incoming: AuthorizedSubgroup,
        purpose: Purpose,
        proof: Option<crate::resource_producer::TransportProof>,
        arg0: Option<Vec<u8>>,
        timeout: Option<Duration>,
        named_targets: Option<[u32; 2]>,
    ) {
        let (sealed, loan, accounted, linker) = match purpose {
            Purpose::Member { accounted } => (None, None, accounted, None),
            Purpose::Sealed(sealed) => (Some(*sealed), None, false, None),
            Purpose::Loan { edge, accounted } => (None, Some(edge), accounted, None),
            Purpose::Linker(request) => {
                let Ok(loan) = request.loan() else {
                    return;
                };
                (
                    None,
                    Some(parent_loan::Edge::of(loan, request.member.member_attempt)),
                    false,
                    Some(*request),
                )
            }
        };
        // A replay receives no new no-child claim or implicit permission to retry.
        if !matches!(
            self.journal.fresh(&incoming.request.member.binding),
            Ok(true)
        ) {
            return;
        }
        let accepted =
            members::Authorization::of(&incoming.request, incoming.peer).is_ok_and(|mut check| {
                if let Some(purpose) = &sealed {
                    purpose.authorize(&mut check);
                }
                check.linker = linker.clone();
                self.authorize_member(&check)
            }) && !(sealed.is_none()
                && linker.is_none()
                && proof.is_none()
                && self
                    .jobs
                    .get(&(
                        incoming.request.job.session.clone(),
                        incoming.request.job.attempt,
                    ))
                    .is_some_and(|job| job.workload.is_some()));
        let AuthorizedSubgroup {
            request,
            peer,
            rights,
        } = incoming;
        let Rights {
            stdin,
            stdout,
            stderr,
            events,
            delegated,
        } = rights;
        let Ok(mut events) = job::Events::new(events, request.member.binding.clone()) else {
            return;
        };
        if accounted && events.account().is_err() {
            return;
        }
        if let Some(linker) = &linker
            && events.linker(linker).is_err()
        {
            return;
        }
        if let Some(sealed) = &sealed {
            if let metadata::Purpose::NativeJobQuery(query) = sealed {
                if events
                    .native_job(query.member.binding.root_attempt, query.plan_sha256)
                    .is_err()
                {
                    return;
                }
            } else if sealed.cooperative_query() {
                if events
                    .workload(sealed.member().binding.root_attempt)
                    .is_err()
                {
                    return;
                }
            } else {
                events.metadata(sealed.member().binding.root_attempt);
            }
        }
        if !accepted {
            let _ = events.emit(Event::Refused {
                reason: "original member coverage refused before launch".into(),
            });
            return;
        }
        let binding = request.member.binding.clone();
        if let Err(error) = self.journal.create(&binding) {
            self.journal
                .fault(&mut self.faulted, "member-create", error);
            let _ = events.emit(Event::Refused {
                reason: "member journal refused before launch".into(),
            });
            return;
        }
        let _ = events.emit(Event::Queued);
        let root = (request.job.session.clone(), request.job.attempt);
        if let Some(job) = self.jobs.get_mut(&root) {
            if let Some(workload) = job.workload.as_mut() {
                let kind = if sealed
                    .as_ref()
                    .is_some_and(metadata::Purpose::cooperative_query)
                {
                    workload::CensusKind::Query
                } else if loan.is_some() {
                    workload::CensusKind::Linker
                } else {
                    workload::CensusKind::Compiler
                };
                if let Err(error) = workload.register(binding.attempt, kind) {
                    self.journal
                        .fault(&mut self.faulted, "member-register", error);
                    let _ = self.journal.settle(&binding); // Lexical no ARM or downstream delivery.
                    let _ = events.emit(Event::Refused {
                        reason: "workload census refused before ARM".into(),
                    });
                    return;
                }
                // Legacy Role1/4 wire remains byte-for-byte unchanged; the new
                // workload census independently retains the SAME owning wait.
                events.observe_accounting();
            }
            if matches!(&sealed, Some(metadata::Purpose::Resolver(_))) {
                job.resolver_queries += 1;
            }
            if sealed
                .as_ref()
                .is_some_and(metadata::Purpose::cooperative_query)
                && let Some(state) = job.workload.as_mut()
            {
                state.queries += 1;
            }
            job.members.insert(
                binding.attempt,
                Slot {
                    binding,
                    units: request.member.cpu_units,
                    state: State::Pending(Box::new(Pending {
                        request,
                        peer,
                        stdin,
                        stdout,
                        stderr,
                        delegated,
                        events,
                        sealed,
                        proof,
                        arg0,
                        timeout,
                        named_targets,
                        loan,
                        linker,
                    })),
                },
            );
        }
    }
    pub(super) fn tick_members(&mut self) {
        let roots: Vec<_> = self.jobs.keys().cloned().collect();
        for root in roots {
            let Some(mut job) = self.jobs.remove(&root) else {
                continue;
            };
            for original in job.contained_retirements.values() {
                // Transport cleanup only after actual returned native custody,
                // actual journal settlement and actual peer EOF. Any error
                // retains it; the original phase/SDK/GNU owners stay live.
                let _ = original.close_exhausted_transport();
            }
            let attempts: Vec<_> = job.members.keys().copied().collect();
            for attempt in attempts {
                let Some(mut slot) = job.members.remove(&attempt) else {
                    continue;
                };
                let occupied = self
                    .jobs
                    .values()
                    .chain(std::iter::once(&job))
                    .flat_map(|job| job.members.values().filter_map(Slot::active_cpu))
                    .collect::<std::collections::BTreeSet<_>>();
                if !self.advance_member(&mut job, &mut slot, &occupied) {
                    job.members.insert(attempt, slot);
                } else {
                    // return_unit forbids retiring a parent with an outgoing
                    // edge. Discard only its now-runnable in-memory bookkeeping.
                    job.loans.remove(&attempt);
                }
            }
            self.jobs.insert(root, job);
        }
    }
    fn advance_member(
        &mut self,
        job: &mut Job,
        slot: &mut Slot,
        occupied: &std::collections::BTreeSet<u32>,
    ) -> bool {
        let state = std::mem::replace(&mut slot.state, State::Vacant);
        slot.state = match state {
            State::Produced(mut member) => {
                match member.tick(self, job, self.faulted) {
                    Ok(true) => return true,
                    Ok(false) => {}
                    Err(error) => {
                        self.journal.fault(
                            &mut self.faulted,
                            "produced-member",
                            (&slot.binding, error),
                        );
                        job.cause = CompletionCause::HostFault;
                        job.cleanup_at.get_or_insert_with(Instant::now);
                    }
                }
                State::Produced(member)
            }
            State::Pending(mut pending) => {
                let allowed = members::Authorization::of(&pending.request, pending.peer).is_ok_and(
                    |mut check| {
                        if let Some(purpose) = &pending.sealed {
                            purpose.authorize(&mut check);
                        }
                        check.linker = pending.linker.clone();
                        members::live(&self.fence, &self.debt, job, &check)
                    },
                );
                let proof_valid = pending
                    .proof
                    .as_ref()
                    .is_none_or(|proof| proof.validate().is_ok());
                if !allowed || !proof_valid || pending.events.cancelled() || self.faulted {
                    refuse_pending(*pending, "member cancelled or original coverage lost")
                } else {
                    let scheduling = if let Some(workload) = &mut job.workload {
                        let deadline = job
                            .timeout
                            .map(|timeout| job.started + timeout)
                            .unwrap_or_else(Instant::now);
                        pending
                            .delegated
                            .cpu
                            .as_ref()
                            .ok_or_else(|| refused("workload member has no original CPU"))
                            .and_then(|offered| {
                                if pending.loan.is_some() {
                                    // A linker is the explicit parked compiler's
                                    // SAME continuation, never another bank loan.
                                    workload.primary.original_for(offered.as_fd()).map(Some)
                                } else {
                                    workload.member_original(
                                        pending.request.member.binding.attempt,
                                        offered.as_fd(),
                                        deadline,
                                    )
                                }
                            })
                            .and_then(|original| {
                                original
                                    .map(|original| {
                                        Prepared::from_workload_original(
                                            &self.debt,
                                            &pending.request,
                                            &pending.delegated,
                                            pending.named_targets,
                                            original,
                                        )
                                    })
                                    .transpose()
                            })
                    } else {
                        Prepared::try_new_mapped(
                            &self.pool,
                            &self.debt,
                            self.pool.slots().get(),
                            &pending.request,
                            &pending.delegated,
                            pending.named_targets,
                        )
                    };
                    match scheduling {
                        Ok(None) => State::Pending(pending),
                        Ok(Some(prepared))
                            if pending.loan.is_some_and(|edge| {
                                prepared.cpu_slot() != Some(edge.cpu)
                                    || !parent_loan::matches_parent(job, pending.peer, edge)
                            }) =>
                        {
                            refuse_pending(
                                *pending,
                                "loan differs from the original live compiler unit",
                            )
                        }
                        Ok(Some(prepared))
                            if prepared
                                .cpu_slot()
                                .is_some_and(|cpu| occupied.contains(&cpu))
                                && pending.loan.is_none() =>
                        {
                            refuse_pending(
                                *pending,
                                "CPU unit is already executing another member; explicit park/transfer is required",
                            )
                        }
                        Ok(Some(_))
                            if job
                                .members
                                .values()
                                .map(Slot::active_units)
                                .sum::<u32>()
                                .checked_add(if pending.loan.is_some() {
                                    0
                                } else {
                                    pending.request.member.cpu_units
                                })
                                .is_none_or(|units| units > job.cpu_ceiling) =>
                        {
                            refuse_pending(
                                *pending,
                                "member units exceed the original root ceiling",
                            )
                        }
                        Ok(Some(prepared))
                            if pending.sealed.is_some() || job.workload.is_some() =>
                        {
                            let original_gnu = if pending.linker.is_some()
                                || matches!(
                                    &pending.sealed,
                                    Some(
                                        metadata::Purpose::WorkloadQuery(_)
                                            | metadata::Purpose::NativeJobQuery(_)
                                    )
                                ) {
                                pending.delegated.gnu.is_empty() && prepared.cpu_slot().is_some()
                            } else if pending.sealed.is_none() && job.workload.is_some() {
                                // Prepared validates the actual original GNU
                                // descriptors; no count creates that transport.
                                !pending.delegated.gnu.is_empty()
                            } else {
                                job.metadata_gnu.as_ref().is_some_and(|gnu| {
                                    gnu.matches(&pending.delegated.gnu).unwrap_or(false)
                                })
                            };
                            if !original_gnu {
                                refuse_pending(
                                    *pending,
                                    "metadata GNU differs from the original admitted root",
                                )
                            } else {
                                let input: io::Result<metadata::Preparation> = (|| {
                                    let deadline = native_inputs::deadline(Instant::now());
                                    if let Some(request) = &pending.linker {
                                        let sdk = job.metadata_sdk.as_ref().ok_or_else(|| {
                                            refused("original linker SDK missing")
                                        })?;
                                        let held = sdk.workload_linker(deadline)?;
                                        let descriptor = aterm_uds::kept_out(|| sdk.descriptor())?;
                                        return Ok(metadata::Preparation::Linker {
                                            request: request.clone(),
                                            held,
                                            sdk: descriptor,
                                        });
                                    }
                                    let original = if job.workload.is_some() {
                                        Some(
                                            job.metadata_sdk
                                                .as_ref()
                                                .ok_or_else(|| {
                                                    refused("workload SDK keeper missing")
                                                })?
                                                .workload_compiler(deadline)?,
                                        )
                                    } else {
                                        None
                                    };
                                    if let Some(sealed) = &pending.sealed {
                                        Ok(metadata::Preparation::Metadata {
                                            purpose: sealed.clone(),
                                            held: original,
                                        })
                                    } else {
                                        let command = member_recipe::command(
                                            job,
                                            &pending.request,
                                            pending.timeout,
                                        )?;
                                        let held = original.ok_or_else(|| {
                                            refused("workload original compiler not captured")
                                        })?;
                                        let targets = prepared.child_targets()?;
                                        Ok(metadata::Preparation::Native {
                                            command,
                                            held,
                                            targets,
                                        })
                                    }
                                })(
                                );
                                #[cfg(all(test, target_os = "macos"))]
                                let pause = self.metadata_pause.take();
                                #[cfg(all(test, target_os = "macos"))]
                                let drop_notice =
                                    pause.as_ref().map(|pause| pause.drop_notice.clone());
                                let worker = std::thread::Builder::new()
                                    .name("resource-metadata-prepare".into())
                                    .spawn(move || {
                                        #[cfg(all(test, target_os = "macos"))]
                                        if let Some(pause) = pause {
                                            pause.wait()?;
                                        }
                                        input?.run(native_inputs::deadline(Instant::now()))
                                    });
                                match worker {
                                    Ok(worker) => {
                                        let worker = preparation::Worker::new(worker);
                                        #[cfg(all(test, target_os = "macos"))]
                                        let worker = {
                                            let mut worker = worker;
                                            if let Some(notice) = drop_notice {
                                                worker.notify_drop(notice);
                                            }
                                            worker
                                        };
                                        State::Preparing(Box::new(Preparing {
                                            pending,
                                            scheduling: prepared,
                                            worker,
                                            cancelled: false,
                                        }))
                                    }
                                    Err(_) => refuse_pending(
                                        *pending,
                                        "metadata preparation worker unavailable",
                                    ),
                                }
                            }
                        }
                        Ok(Some(prepared)) => {
                            self.launch_member(job, slot.units, *pending, prepared, None)
                        }
                        Err(_) => {
                            refuse_pending(*pending, "member descriptors refused before launch")
                        }
                    }
                }
            }
            State::Preparing(mut preparing) => {
                preparing.cancelled |= preparing.pending.events.cancelled()
                    || self.faulted
                    || job.cleanup_at.is_some()
                    || job.reaped;
                let allowed =
                    members::Authorization::of(&preparing.pending.request, preparing.pending.peer)
                        .is_ok_and(|mut check| {
                            if let Some(purpose) = &preparing.pending.sealed {
                                purpose.authorize(&mut check);
                            }
                            check.linker = preparing.pending.linker.clone();
                            members::live(&self.fence, &self.debt, job, &check)
                        });
                preparing.cancelled |= !allowed;
                if preparing.worker.is_finished() {
                    let Preparing {
                        pending,
                        scheduling,
                        worker,
                        cancelled,
                    } = *preparing;
                    let result = worker.join();
                    if cancelled {
                        refuse_pending(
                            *pending,
                            "metadata cancelled while original resources were retained",
                        )
                    } else {
                        match result {
                            Ok(Ok(command)) => self.launch_member(
                                job,
                                slot.units,
                                *pending,
                                scheduling,
                                Some(command),
                            ),
                            Ok(Err(error)) => {
                                let detail = error
                                    .to_string()
                                    .chars()
                                    .filter(|ch| !ch.is_control())
                                    .take(512)
                                    .collect::<String>();
                                refuse_pending(
                                    *pending,
                                    &format!(
                                        "held metadata program qualification refused: {detail}"
                                    ),
                                )
                            }
                            Err(_) => refuse_pending(
                                *pending,
                                "held metadata program qualification refused",
                            ),
                        }
                    }
                } else {
                    State::Preparing(preparing)
                }
            }
            State::Active(mut active) => {
                self.poll_member(job, &slot.binding, slot.units, &mut active);
                if active.settled && active.retired && active.drained {
                    return true;
                }
                State::Active(active)
            }
            State::Refused {
                mut events,
                mut settled,
            } => {
                let returned = job.workload.as_mut().map_or(
                    Ok(member::ReturnProgress::Returned),
                    |workload| {
                        if job.reaped {
                            workload.host_closed_after_root_exit(slot.binding.attempt);
                            Ok(member::ReturnProgress::Returned)
                        } else {
                            workload.return_member(
                                slot.binding.attempt,
                                job.timeout
                                    .map(|timeout| job.started + timeout)
                                    .unwrap_or_else(Instant::now),
                            )
                        }
                    },
                );
                if returned.is_err() {
                    self.journal
                        .fault(&mut self.faulted, "member-return", &returned);
                    job.cause = CompletionCause::HostFault;
                    job.cleanup_at.get_or_insert_with(Instant::now);
                }
                if !settled && matches!(returned, Ok(member::ReturnProgress::Returned)) {
                    settled = self.journal.settle(&slot.binding).is_ok();
                }
                if settled && events.drained() {
                    if let Some(workload) = job.workload.as_mut()
                        && let Err(error) = workload.refused(slot.binding.attempt)
                    {
                        self.journal
                            .fault(&mut self.faulted, "member-refused", error);
                    }
                    return true;
                }
                State::Refused { events, settled }
            }
            State::Vacant => {
                self.journal.fault(
                    &mut self.faulted,
                    "member-vacant",
                    "queued member has no state",
                );
                State::Vacant
            }
        };
        false
    }
    fn launch_member(
        &mut self,
        job: &mut Job,
        reserved_units: u32,
        mut pending: Pending,
        mut prepared: Prepared,
        metadata: Option<metadata::Command>,
    ) -> State {
        let (mut command, inputs) = if let Some(prepared) = metadata {
            if let Some(inventory) = prepared.inputs.inventory.as_ref() {
                if pending.events.prepared(inventory).is_err() {
                    return refuse_pending(pending, "metadata input inventory unavailable");
                }
            } else if pending.sealed.is_some() {
                return refuse_pending(
                    pending,
                    "metadata purpose lost its original input inventory",
                );
            }
            (prepared.command, Some(prepared.inputs))
        } else {
            match member_recipe::command(job, &pending.request, pending.timeout) {
                Ok(command) => (command, None),
                Err(_) => {
                    return refuse_pending(pending, "member executable recipe is not qualified");
                }
            }
        };
        if let Some(arg0) = &pending.arg0 {
            command.arg0(std::ffi::OsStr::from_bytes(arg0));
        }
        // Linker is a nondelegating target: original CPU5 and Host-selected
        // read-only lifeline6 and close-only original SDK7 cross final exec.
        // The sole writer stays with Host; no SDK/root/control locator leaks.
        let sdk = match if pending.linker.is_some() {
            Ok(None)
        } else {
            sdk::member(job.compiler.as_ref(), &mut prepared, &mut command)
        } {
            Ok(value) => value,
            Err(_) => return refuse_pending(pending, "original SDK guard unavailable"),
        };
        let coverage = match job::inspection(&self.fence, &self.debt, job) {
            Ok(coverage) => coverage,
            Err(_) => return refuse_pending(pending, "member original resources unavailable"),
        };
        if self.journal.update(&pending.request.member.binding,"member_prepared",aterm_json::json!({
            "parent_session":job.binding.session,"parent_attempt":aterm_codec::hex::encode_lower(&job.binding.attempt),"sdk":sdk::json(&sdk),
            "memory":{"v":3,"namespace":coverage.memory.namespace,"marker":coverage.memory.marker,"epoch":coverage.memory.epoch,
            "guard":{"device":coverage.memory.guard_device,"inode":coverage.memory.guard_inode}} })).is_err() {
            self.faulted=true;
            return refuse_pending(pending,"member intent could not be committed before ARM");
        }
        if pending.linker.is_some()
            || matches!(
                &pending.sealed,
                Some(
                    metadata::Purpose::Resolver(_)
                        | metadata::Purpose::WorkloadQuery(_)
                        | metadata::Purpose::NativeJobQuery(_)
                )
            )
        {
            // Role 5 has no caller-supplied source input. Own the empty reader
            // before GO even when a direct peer transferred another handle.
            match File::open("/dev/null") {
                Ok(empty) => pending.stdin = empty.into(),
                Err(_) => return refuse_pending(pending, "resolver empty input unavailable"),
            }
        }
        let Pending {
            request,
            peer,
            stdin,
            stdout,
            stderr,
            delegated: _,
            events,
            sealed: _,
            proof,
            arg0: _,
            timeout,
            named_targets: _,
            loan,
            linker: _,
        } = pending;
        command
            .stdin(spawnfd::Stdio::from(stdin))
            .stdout(spawnfd::Stdio::from(stdout))
            .stderr(spawnfd::Stdio::from(stderr));
        let cpu = prepared.cpu_slot();
        let cpu_target = match request.cpu {
            aterm_types::resource_subgroup::CpuDelegation::SharedBorrow { target_fd } => {
                Some(target_fd)
            }
            _ => None,
        };
        let retained_sdk = job.metadata_sdk.clone();
        let retained_compiler = job.compiler.clone();
        let retained_primary = job.workload.as_ref().map(|state| state.primary.clone());
        let retained_output = job
            .workload
            .as_ref()
            .and_then(workload::State::retained_output_locks);
        let attempt = member::OwnedAttempt::launch(
            &mut member_custody::Borrowed {
                job,
                fence: &self.fence,
                debt: &self.debt,
                peer,
                proof: proof.as_ref(),
                member: request.member.binding.attempt,
                reserved_units,
                loan,
                journal: &self.journal,
            },
            command,
            metadata::Resources {
                scheduling: prepared,
                inputs,
                _sdk: retained_sdk,
                _compiler: retained_compiler,
                _primary: retained_primary,
                _output: retained_output,
            },
            events,
            request.member.binding,
            timeout,
            crate::actor::wake,
        );
        State::Active(Box::new(Active {
            attempt,
            peer,
            proof,
            cpu,
            cpu_target,
            loan,
            sdk,
            bound: false,
            terminal: false,
            settled: false,
            retired: false,
            drained: false,
        }))
    }
    fn poll_member(
        &mut self,
        job: &mut Job,
        binding: &Binding,
        reserved_units: u32,
        active: &mut Active,
    ) {
        let cause = if self.faulted {
            Some(CompletionCause::HostFault)
        } else {
            job.cleanup_at
                .map(|_| {
                    if job.cause == CompletionCause::Exited {
                        CompletionCause::Cancelled
                    } else {
                        job.cause
                    }
                })
                .or_else(|| job.reaped.then_some(CompletionCause::Cancelled))
        };
        let mut custody = member_custody::Borrowed {
            job,
            fence: &self.fence,
            debt: &self.debt,
            peer: active.peer,
            proof: active.proof.as_ref(),
            member: binding.attempt,
            reserved_units,
            loan: active.loan,
            journal: &self.journal,
        };
        let progress = active.attempt.poll(&mut custody, cause);
        if let Some(workload) = custody.job.workload.as_mut()
            && let Err(error) =
                workload.observe(binding.attempt, active.attempt.identity(), &progress)
        {
            self.journal
                .fault(&mut self.faulted, "member-observe", error);
            custody.job.cause = CompletionCause::HostFault;
            custody.job.cleanup_at.get_or_insert_with(Instant::now);
        }
        if progress.uncertain
            || (progress.launched && !progress.go_attempted)
            || active.attempt.binding() != binding
        {
            self.journal.fault(
                &mut self.faulted,
                "member-progress",
                format!("binding={binding:?} uncertain={} launched={} go_attempted={} binding_matches={} detail={:?}",
                    progress.uncertain, progress.launched, progress.go_attempted,
                    active.attempt.binding() == binding, active.attempt.failure()),
            );
            custody.job.cause = CompletionCause::HostFault;
            custody.job.cleanup_at.get_or_insert_with(Instant::now);
            if !custody.job.reaped
                && custody
                    .job
                    .child
                    .request_group(aterm_capacity::cohort::launch::CohortSignal::Kill)
                    .is_err()
            {
                let _ = custody.job.child.child_mut().kill();
            }
            let _ = active
                .attempt
                .poll(&mut custody, Some(CompletionCause::HostFault));
        }
        if progress.ready_to_start && !self.faulted {
            if let Some((pid, birth)) = active.attempt.identity() {
                let bound = if active.bound {
                    Ok(())
                } else {
                    self.journal.update(
                        binding,
                        "member_bound",
                        aterm_json::json!({"pid":pid,"birth":birth}),
                    )
                };
                match bound {
                    Ok(()) => active.bound = true,
                    Err(error) => {
                        self.journal
                            .fault(&mut self.faulted, "member-bind-journal", error);
                        let _ = active
                            .attempt
                            .poll(&mut custody, Some(CompletionCause::HostFault));
                    }
                }
                if active.bound && !self.faulted {
                    match active.attempt.start(&mut custody) {
                        Ok(started) => {
                            if let Some(workload) = custody.job.workload.as_mut()
                                && let Err(error) = workload.started(binding.attempt, started)
                            {
                                self.journal
                                    .fault(&mut self.faulted, "member-started", error);
                                custody.job.cause = CompletionCause::HostFault;
                                custody.job.cleanup_at.get_or_insert_with(Instant::now);
                            }
                        }
                        Err(error) if error.kind() == io::ErrorKind::WouldBlock => {}
                        Err(error) => self.journal.record_fault("member-start", error),
                    }
                }
            } else {
                // A ready attempt without its original bound identity cannot
                // authorize GO. Preserve custody and begin actual cleanup.
                self.journal.fault(
                    &mut self.faulted,
                    "member-ready-identity",
                    "ready attempt has no original bound identity",
                );
                let _ = active
                    .attempt
                    .poll(&mut custody, Some(CompletionCause::HostFault));
            }
        }
        if progress.completed.is_some() && !active.terminal {
            active.terminal = self
                .journal
                .update(binding, "member_terminal", aterm_json::json!({}))
                .is_ok();
        }
        active.retired = progress.retired;
        active.drained = progress.drained;
        if progress.retired && !active.settled {
            active.settled = self.journal.settle(binding).is_ok();
        }
    }
}
fn refuse_pending(mut pending: Pending, reason: &str) -> State {
    let _ = pending.events.emit(Event::Refused {
        reason: reason.into(),
    });
    State::Refused {
        events: Box::new(pending.events),
        settled: false,
    }
}
