// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Preserve authorized rights across nonblocking admission. Each attempt has
//! one journal identity; busy never asks a caller to replay execution.
use super::*;
use crate::subgroup_delegation::Prepared;
use aterm_ctl::resource_subgroup_client::{AuthorizedSubgroup, Delegations, Rights};
use aterm_types::resource_admission::Event;
use aterm_types::resource_subgroup::Request;
use std::os::{fd::OwnedFd, unix::ffi::OsStrExt as _};

struct Pending {
    request: Request,
    peer: Peer,
    stdin: OwnedFd,
    stdout: OwnedFd,
    stderr: OwnedFd,
    delegated: Delegations,
    events: job::Events,
    proof: Option<crate::resource_producer::TransportProof>,
    arg0: Option<Vec<u8>>,
    timeout: Option<Duration>,
    named_targets: Option<[u32; 2]>,
}
struct Active {
    attempt: member::OwnedAttempt<Prepared>,
    peer: Peer,
    proof: Option<crate::resource_producer::TransportProof>,
    cpu: Option<u32>,
    bound: bool,
    terminal: bool,
    settled: bool,
    retired: bool,
    drained: bool,
}
enum State {
    Pending(Box<Pending>),
    Active(Box<Active>),
    Refused { events: job::Events, settled: bool },
    Vacant,
}
pub(super) struct Slot {
    binding: Binding,
    units: u32,
    state: State,
}
impl Slot {
    pub(super) fn covers(&mut self, peer: Peer) -> bool {
        match &mut self.state {
            State::Active(active) => active.attempt.covers(peer),
            _ => false,
        }
    }
    pub(super) fn group(&self) -> Option<i32> {
        match &self.state {
            State::Active(active) => active.attempt.group(),
            _ => None,
        }
    }
    pub(super) fn active_cpu(&self) -> Option<u32> {
        match &self.state {
            State::Active(active) if !active.retired => active.cpu,
            _ => None,
        }
    }
    fn active_units(&self) -> u32 {
        match &self.state {
            State::Active(active) if !active.retired => self.units,
            _ => 0,
        }
    }
    pub(super) fn subject(&self) -> Option<aterm_types::resource_inspection::Subject> {
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
    pub(in super::super) fn submit_member(&mut self, incoming: AuthorizedSubgroup) {
        let timeout = incoming
            .request
            .member
            .timeout_ms
            .map(Duration::from_millis);
        self.submit_member_proved(incoming, None, None, timeout, None);
    }
    pub(super) fn submit_member_proved(
        &mut self,
        incoming: AuthorizedSubgroup,
        proof: Option<crate::resource_producer::TransportProof>,
        arg0: Option<Vec<u8>>,
        timeout: Option<Duration>,
        named_targets: Option<[u32; 2]>,
    ) {
        // Never tell a replaying caller that an already-known binding had no
        // execution. Closing its new channel preserves unknown delivery.
        if !matches!(
            self.journal.fresh(&incoming.request.member.binding),
            Ok(true)
        ) {
            return;
        }
        let accepted = members::Authorization::of(&incoming.request, incoming.peer)
            .is_ok_and(|check| self.authorize_member(&check));
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
        if !accepted {
            let _ = events.emit(Event::Refused {
                reason: "original member coverage refused before launch".into(),
            });
            return;
        }
        let binding = request.member.binding.clone();
        if self.journal.create(&binding).is_err() {
            self.faulted = true;
            let _ = events.emit(Event::Refused {
                reason: "member journal refused before launch".into(),
            });
            return;
        }
        let _ = events.emit(Event::Queued);
        let root = (request.job.session.clone(), request.job.attempt);
        if let Some(job) = self.jobs.get_mut(&root) {
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
                        proof,
                        arg0,
                        timeout,
                        named_targets,
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
            State::Pending(mut pending) => {
                let allowed = members::Authorization::of(&pending.request, pending.peer)
                    .is_ok_and(|check| members::live(&self.fence, &self.debt, job, &check));
                let proof_valid = pending
                    .proof
                    .as_ref()
                    .is_none_or(|proof| proof.validate().is_ok());
                if !allowed || !proof_valid || pending.events.cancelled() || self.faulted {
                    refuse_pending(*pending, "member cancelled or original coverage lost")
                } else {
                    match Prepared::try_new_mapped(
                        &self.pool,
                        &self.debt,
                        self.pool.slots().get(),
                        &pending.request,
                        &pending.delegated,
                        pending.named_targets,
                    ) {
                        Ok(None) => State::Pending(pending),
                        Ok(Some(prepared))
                            if prepared
                                .cpu_slot()
                                .is_some_and(|cpu| occupied.contains(&cpu)) =>
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
                                .checked_add(pending.request.member.cpu_units)
                                .is_none_or(|units| units > job.cpu_ceiling) =>
                        {
                            refuse_pending(
                                *pending,
                                "member units exceed the original root ceiling",
                            )
                        }
                        Ok(Some(prepared)) => self.launch_member(job, *pending, prepared),
                        Err(_) => {
                            refuse_pending(*pending, "member descriptors refused before launch")
                        }
                    }
                }
            }
            State::Active(mut active) => {
                self.poll_member(job, &slot.binding, &mut active);
                if active.settled && active.retired && active.drained {
                    return true;
                }
                State::Active(active)
            }
            State::Refused {
                mut events,
                mut settled,
            } => {
                if !settled {
                    settled = self.journal.settle(&slot.binding).is_ok();
                }
                if settled && events.drained() {
                    return true;
                }
                State::Refused { events, settled }
            }
            State::Vacant => {
                self.faulted = true;
                State::Vacant
            }
        };
        false
    }
    fn launch_member(&mut self, job: &mut Job, pending: Pending, prepared: Prepared) -> State {
        let mut command = match member_recipe::command(job, &pending.request) {
            Ok(command) => command,
            Err(_) => return refuse_pending(pending, "member executable recipe is not qualified"),
        };
        if let Some(arg0) = &pending.arg0 {
            command.arg0(std::ffi::OsStr::from_bytes(arg0));
        }
        let coverage = match job::inspection(&self.fence, &self.debt, job) {
            Ok(coverage) => coverage,
            Err(_) => return refuse_pending(pending, "member original resources unavailable"),
        };
        if self.journal.update(&pending.request.member.binding,"member_prepared",aterm_json::json!({
            "parent_session":job.binding.session,"parent_attempt":aterm_codec::hex::encode_lower(&job.binding.attempt),
            "memory":{"v":3,"namespace":coverage.memory.namespace,"marker":coverage.memory.marker,"epoch":coverage.memory.epoch,
            "guard":{"device":coverage.memory.guard_device,"inode":coverage.memory.guard_inode}} })).is_err() {
            self.faulted=true;
            return refuse_pending(pending,"member intent could not be committed before ARM");
        }
        let Pending {
            request,
            peer,
            stdin,
            stdout,
            stderr,
            delegated: _,
            events,
            proof,
            arg0: _,
            timeout,
            named_targets: _,
        } = pending;
        command
            .stdin(spawnfd::Stdio::from(stdin))
            .stdout(spawnfd::Stdio::from(stdout))
            .stderr(spawnfd::Stdio::from(stderr));
        let cpu = prepared.cpu_slot();
        let attempt = member::OwnedAttempt::launch(
            &mut member_custody::Borrowed {
                job,
                fence: &self.fence,
                debt: &self.debt,
                peer,
                proof: proof.as_ref(),
            },
            command,
            prepared,
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
            bound: false,
            terminal: false,
            settled: false,
            retired: false,
            drained: false,
        }))
    }
    fn poll_member(&mut self, job: &mut Job, binding: &Binding, active: &mut Active) {
        let cause = if self.faulted {
            Some(CompletionCause::ResourceLimit)
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
        };
        let progress = active.attempt.poll(&mut custody, cause);
        if progress.uncertain
            || (progress.launched && !progress.go_attempted)
            || active.attempt.binding() != binding
        {
            self.faulted = true;
            custody.job.cause = CompletionCause::ResourceLimit;
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
                .poll(&mut custody, Some(CompletionCause::ResourceLimit));
        }
        if progress.ready_to_start && !active.bound && !self.faulted {
            if let Some((pid, birth)) = active.attempt.identity() {
                active.bound = self
                    .journal
                    .update(
                        binding,
                        "member_bound",
                        aterm_json::json!({"pid":pid,"birth":birth}),
                    )
                    .is_ok()
                    && self
                        .journal
                        .update(binding, "member_running", aterm_json::json!({}))
                        .is_ok();
                if active.bound {
                    let _ = active.attempt.start(&mut custody);
                } else {
                    self.faulted = true;
                    let _ = active
                        .attempt
                        .poll(&mut custody, Some(CompletionCause::Cancelled));
                }
            } else {
                // A ready attempt without its original bound identity cannot
                // authorize GO. Preserve custody and begin actual cleanup.
                self.faulted = true;
                let _ = active
                    .attempt
                    .poll(&mut custody, Some(CompletionCause::ResourceLimit));
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
        events: pending.events,
        settled: false,
    }
}
