// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! The original Targo bank selects every loan. The host confirms that exact
//! loan before ARM and returns it only at a positively established no-ARM or
//! retired-member boundary. Transport failure never makes an occupied unit free.
use super::*;
use std::os::fd::{BorrowedFd, OwnedFd};
use trust_resource_controller::workload::host::bank as wire;

#[derive(Default)]
pub(super) struct Loans {
    sequence: u64,
    primaries: BTreeMap<[u8; 16], PrimaryLoan>,
    workers: BTreeMap<([u8; 16], [u8; 16]), WorkerState>,
    seen_workers: std::collections::BTreeSet<([u8; 16], [u8; 16])>,
}
enum WorkerState {
    Offered,
    Granted(Grant),
}
impl Loans {
    pub(super) fn drained(&self) -> bool {
        self.primaries.is_empty() && self.workers.is_empty()
    }
}
enum PrimaryLoan {
    Pending {
        slot: u32,
        response: crate::workload_transport::BankPending,
    },
    Granted(Grant),
}
struct Grant {
    slot: u32,
    loan: [u8; 16],
    // The actual bank-transferred original stays held until its positive return.
    _original: OwnedFd,
    returning: Option<crate::workload_transport::BankPending>,
    return_started: bool,
}
impl State {
    fn bank_request(
        &mut self,
        operation: wire::Operation,
        deadline: Instant,
    ) -> io::Result<crate::workload_transport::BankPending> {
        let ready = self
            .ready
            .as_ref()
            .ok_or_else(|| refused("original workload is not ready"))?;
        let sequence = self
            .bank
            .sequence
            .checked_add(1)
            .ok_or_else(|| refused("workload bank sequence exhausted"))?;
        let request = wire::Request {
            key: wire::Key {
                root_attempt: ready.root_attempt,
                invocation_sha256: ready.sha256,
                sequence,
            },
            operation,
        };
        let response = self
            .control
            .as_ref()
            .ok_or_else(|| refused("original workload control missing"))?
            .begin(request, deadline)?;
        // Full local queue does not consume a sequence or create a new request.
        self.bank.sequence = sequence;
        Ok(response)
    }
    fn accept_primary(&mut self, member: [u8; 16]) -> io::Result<bool> {
        let Some(PrimaryLoan::Pending { slot, response }) = self.bank.primaries.get(&member) else {
            return Ok(true);
        };
        let slot = *slot;
        let Some(answer) = response.poll()? else {
            return Ok(false);
        };
        match answer.reply.decision {
            wire::Decision::Granted {
                member_attempt,
                loan,
                slot: got,
                backing,
            } if member_attempt == member && got == slot => {
                let original = answer
                    .original
                    .ok_or_else(|| refused("original bank description missing"))?;
                let actual = self.primary.original_for(original.as_fd())?;
                if actual.slot() != slot
                    || !self
                        .primary
                        .observations()?
                        .iter()
                        .any(|item| item.slot == slot && item.backing == backing)
                {
                    return Err(refused(
                        "bank primary differs from original host reservation",
                    ));
                }
                self.bank.primaries.insert(
                    member,
                    PrimaryLoan::Granted(Grant {
                        slot,
                        loan,
                        _original: original,
                        returning: None,
                        return_started: false,
                    }),
                );
                Ok(true)
            }
            wire::Decision::Busy => {
                self.bank.primaries.remove(&member);
                Ok(false)
            }
            _ => Err(refused("original root bank refused the bound primary")),
        }
    }
    pub(in super::super) fn member_original(
        &mut self,
        member: [u8; 16],
        offered: BorrowedFd<'_>,
        deadline: Instant,
    ) -> io::Result<Option<aterm_capacity::build_cpu::Lease>> {
        let actual = self.primary.original_for(offered)?;
        let slot = actual.slot();
        if !self.bank.primaries.contains_key(&member) {
            let response = match self.bank_request(
                wire::Operation::Primary {
                    member_attempt: member,
                    slot,
                },
                deadline,
            ) {
                Ok(response) => response,
                Err(error) if error.kind() == io::ErrorKind::WouldBlock => return Ok(None),
                Err(error) => return Err(error),
            };
            self.bank
                .primaries
                .insert(member, PrimaryLoan::Pending { slot, response });
        }
        if !self.accept_primary(member)? {
            return Ok(None);
        }
        match self.bank.primaries.get(&member) {
            Some(PrimaryLoan::Granted(grant))
                if grant.slot == slot && grant.returning.is_none() =>
            {
                Ok(Some(actual))
            }
            _ => Err(refused(
                "original workload primary changed or was already returned",
            )),
        }
    }
    pub(in super::super) fn return_member(
        &mut self,
        member: [u8; 16],
        deadline: Instant,
    ) -> io::Result<()> {
        if self
            .bank
            .workers
            .keys()
            .any(|(parent, _)| *parent == member)
        {
            return Err(io::ErrorKind::WouldBlock.into());
        }
        if !self.bank.primaries.contains_key(&member) {
            return Ok(());
        }
        // A canceled pending attempt may have raced an actual grant. Observe
        // the SAME outstanding reply first; never discard it and replay.
        if !self.accept_primary(member)? {
            return if self.bank.primaries.contains_key(&member) {
                Err(io::ErrorKind::WouldBlock.into())
            } else {
                Ok(())
            };
        }
        let Some(PrimaryLoan::Granted(grant)) = self.bank.primaries.get(&member) else {
            return Err(refused("original primary return missing"));
        };
        if grant.returning.is_none() {
            let operation = wire::Operation::Returned {
                member_attempt: member,
                loan: grant.loan,
                slot: grant.slot,
            };
            let response = self.bank_request(operation, deadline)?;
            let Some(PrimaryLoan::Granted(grant)) = self.bank.primaries.get_mut(&member) else {
                return Err(refused("original primary return changed"));
            };
            grant.returning = Some(response);
        }
        let Some(PrimaryLoan::Granted(grant)) = self.bank.primaries.get(&member) else {
            return Err(refused("original primary return changed"));
        };
        let Some(answer) = grant
            .returning
            .as_ref()
            .ok_or_else(|| refused("original return reply missing"))?
            .poll()?
        else {
            return Err(io::ErrorKind::WouldBlock.into());
        };
        if !matches!(answer.reply.decision, wire::Decision::Returned) || answer.original.is_some() {
            return Err(refused(
                "original bank did not positively return the primary",
            ));
        }
        self.bank.primaries.remove(&member);
        Ok(())
    }
    pub(super) fn confirmed_primary(&self, member: [u8; 16], slot: u32) -> bool {
        matches!(self.bank.primaries.get(&member),Some(PrimaryLoan::Granted(grant)) if grant.slot==slot&&grant.returning.is_none())
    }
    pub(in crate::actor::owner) fn linker_parent(&self, member: [u8; 16], slot: u32) -> bool {
        self.compiler_dispatch()
            && self.confirmed_primary(member, slot)
            && !self
                .bank
                .workers
                .keys()
                .any(|(parent, _)| *parent == member)
    }
    pub(in super::super) fn host_closed_after_root_exit(&mut self, member: [u8; 16]) {
        // No table return is sent and no slot becomes reusable. The actual
        // root has already been reaped. Preserve every outstanding original
        // description here until the root's remaining native scopes/journal
        // settle under its still-held full-domain reservation and durable debt.
        // This marker is bookkeeping only; the caller owns the actual member
        // no-ARM/ScopeClosed receipt, never a decoded lifecycle frame.
        if let Some(entry) = self.census.get_mut(&member) {
            entry.closed = true;
        }
    }
}

pub(crate) struct WorkerOffer {
    request: host::worker::Request,
    peer: Peer,
    response: crate::workload_transport::BankPending,
    answer: Option<crate::workload_transport::BankAnswer>,
    deadline: Instant,
}
impl WorkerOffer {
    pub(crate) fn deadline(&self) -> Instant {
        self.deadline
    }
    pub(crate) fn poll(&mut self) -> io::Result<bool> {
        if self.answer.is_some() {
            return Err(refused("worker offer was already consumed"));
        }
        self.answer = self.response.poll()?;
        Ok(self.answer.is_some())
    }
}
pub(crate) struct WorkerCustody {
    pub(crate) reply: host::worker::Reply,
    original: Option<OwnedFd>,
    _owner: Arc<Primary>,
    _sdk: Arc<sdk::Keeper>,
}
impl WorkerCustody {
    pub(crate) fn description(&self) -> Option<BorrowedFd<'_>> {
        self.original.as_ref().map(|fd| fd.as_fd())
    }
}
pub(crate) struct WorkerReturn {
    request: host::worker::Return,
    peer: Peer,
    response: crate::workload_transport::BankPending,
    answer: Option<crate::workload_transport::BankAnswer>,
    deadline: Instant,
}
impl WorkerReturn {
    pub(crate) fn deadline(&self) -> Instant {
        self.deadline
    }
    pub(crate) fn poll(&mut self) -> io::Result<bool> {
        if self.answer.is_some() {
            return Err(refused("worker return was already consumed"));
        }
        self.answer = self.response.poll()?;
        Ok(self.answer.is_some())
    }
}

impl Owner {
    fn worker_context(
        &mut self,
        peer: Peer,
        request: &host::MemberOriginalRequest,
    ) -> io::Result<trust_resource_controller::ContextReply> {
        let context = self.context(peer, &request.query)?;
        let job = self
            .jobs
            .get_mut(&(
                request.query.binding.session.clone(),
                request.query.binding.root_attempt,
            ))
            .ok_or_else(|| refused("original worker root unavailable"))?;
        let (slot, _) = job
            .members
            .get_mut(&request.member_attempt)
            .and_then(|member| member.workload_primary(peer))
            .ok_or_else(|| refused("worker is not an original live registered compiler"))?;
        let state = job
            .workload
            .as_ref()
            .ok_or_else(|| refused("worker has no retained root owner"))?;
        if !state.compiler_dispatch()
            || !state.member_originals.contains(&request.member_attempt)
            || !state.confirmed_primary(request.member_attempt, slot)
            || !state
                .census
                .get(&request.member_attempt)
                .is_some_and(|entry| matches!(entry.kind, CensusKind::Compiler))
            || !matches!(context.context.subject,trust_resource_controller::Subject::RegisteredMember{attempt,..} if attempt==request.member_attempt)
            || job
                .loans
                .get(&request.member_attempt)
                .is_some_and(|loan| !loan.runnable())
            || !crate::metadata_prelude::no_fork(peer.pid)?
        {
            return Err(refused(
                "worker lifecycle or original compiler custody differs",
            ));
        }
        let sdk = job
            .metadata_sdk
            .as_ref()
            .ok_or_else(|| refused("worker SDK keeper unavailable"))?;
        sdk.validate_loaded(peer.pid)?;
        let image = aterm_uds::process::LoadedImage::read(peer.pid as i32)
            .ok_or_else(|| refused("worker image unavailable"))?;
        if image.matches_file(&sdk.execution().join("bin/trustc")) != Some(true) {
            return Err(refused("worker peer is not the original selected compiler"));
        }
        Ok(context)
    }
    pub(in crate::actor) fn workload_worker_offer(
        &mut self,
        peer: Peer,
        request: host::worker::Request,
    ) -> io::Result<WorkerOffer> {
        request.encode()?;
        let context = self.worker_context(peer, &request.original)?;
        let job = self
            .jobs
            .get_mut(&(
                request.original.query.binding.session.clone(),
                request.original.query.binding.root_attempt,
            ))
            .ok_or_else(|| refused("original worker root unavailable"))?;
        let deadline = job.started
            + job
                .timeout
                .ok_or_else(|| refused("original worker deadline unavailable"))?;
        let state = job
            .workload
            .as_mut()
            .ok_or_else(|| refused("original worker owner unavailable"))?;
        let key = (request.original.member_attempt, request.worker_request);
        if state.bank.seen_workers.len() >= 4096 || state.bank.seen_workers.contains(&key) {
            return Err(refused("worker request is exhausted or replayed"));
        }
        let response = state.bank_request(
            wire::Operation::Extra {
                member_attempt: request.original.member_attempt,
                worker_request: request.worker_request,
                producer: trust_resource_controller::Identity {
                    pid: peer.pid,
                    birth: peer.birth,
                    birth_kind: context.context.original_root.birth_kind,
                },
            },
            deadline,
        )?;
        state.bank.seen_workers.insert(key);
        state.bank.workers.insert(key, WorkerState::Offered);
        Ok(WorkerOffer {
            request,
            peer,
            response,
            answer: None,
            deadline,
        })
    }
    pub(in crate::actor) fn workload_worker_accept(
        &mut self,
        mut offer: WorkerOffer,
    ) -> io::Result<WorkerCustody> {
        let context = self.worker_context(offer.peer, &offer.request.original)?;
        let job = self
            .jobs
            .get_mut(&(
                offer.request.original.query.binding.session.clone(),
                offer.request.original.query.binding.root_attempt,
            ))
            .ok_or_else(|| refused("original worker root unavailable"))?;
        let state = job
            .workload
            .as_mut()
            .ok_or_else(|| refused("original worker owner unavailable"))?;
        let key = (
            offer.request.original.member_attempt,
            offer.request.worker_request,
        );
        if !matches!(state.bank.workers.get(&key), Some(WorkerState::Offered)) {
            return Err(refused("worker offer changed or was already consumed"));
        }
        let answer = offer
            .answer
            .take()
            .ok_or_else(|| refused("original worker reply unavailable"))?;
        let (decision, original) = match answer.reply.decision {
            wire::Decision::Granted {
                member_attempt,
                loan,
                slot,
                backing,
            } if member_attempt == key.0 => {
                let original = answer
                    .original
                    .ok_or_else(|| refused("worker original description missing"))?;
                let actual = state.primary.original_for(original.as_fd())?;
                if actual.slot() != slot
                    || !state
                        .primary
                        .observations()?
                        .iter()
                        .any(|item| item.slot == slot && item.backing == backing)
                {
                    return Err(refused(
                        "worker bank description differs from original reservation",
                    ));
                }
                let outgoing = aterm_uds::kept_out(|| original.as_fd().try_clone_to_owned())?;
                state.bank.workers.insert(
                    key,
                    WorkerState::Granted(Grant {
                        slot,
                        loan,
                        _original: original,
                        returning: None,
                        return_started: false,
                    }),
                );
                (
                    host::worker::Decision::Granted {
                        loan,
                        slot,
                        backing,
                    },
                    Some(outgoing),
                )
            }
            wire::Decision::Busy if answer.original.is_none() => {
                state.bank.workers.remove(&key);
                (host::worker::Decision::Busy, None)
            }
            _ => return Err(refused("original bank refused the compiler worker")),
        };
        let reply = host::worker::Reply {
            context,
            worker_request: offer.request.worker_request,
            decision,
        };
        reply.encode()?;
        Ok(WorkerCustody {
            reply,
            original,
            _owner: Arc::clone(&state.primary),
            _sdk: Arc::clone(
                job.metadata_sdk
                    .as_ref()
                    .ok_or_else(|| refused("original SDK keeper unavailable"))?,
            ),
        })
    }
    pub(in crate::actor) fn workload_worker_return(
        &mut self,
        peer: Peer,
        request: host::worker::Return,
    ) -> io::Result<WorkerReturn> {
        request.encode()?;
        self.worker_context(peer, &request.request.original)?;
        let job = self
            .jobs
            .get_mut(&(
                request.request.original.query.binding.session.clone(),
                request.request.original.query.binding.root_attempt,
            ))
            .ok_or_else(|| refused("original worker root unavailable"))?;
        let deadline = job.started
            + job
                .timeout
                .ok_or_else(|| refused("original worker deadline unavailable"))?;
        let state = job
            .workload
            .as_mut()
            .ok_or_else(|| refused("original worker owner unavailable"))?;
        let key = (
            request.request.original.member_attempt,
            request.request.worker_request,
        );
        let Some(WorkerState::Granted(grant)) = state.bank.workers.get(&key) else {
            return Err(refused(
                "worker loan is not original or has already returned",
            ));
        };
        if grant.slot != request.slot || grant.loan != request.loan || grant.return_started {
            return Err(refused("worker return differs from original loan"));
        }
        let response = state.bank_request(
            wire::Operation::Returned {
                member_attempt: key.0,
                loan: request.loan,
                slot: request.slot,
            },
            deadline,
        )?;
        // The private returned handle owns the sole response. Mark this entry
        // returning before releasing the actor, so a duplicate cannot begin.
        let Some(WorkerState::Granted(grant)) = state.bank.workers.get_mut(&key) else {
            return Err(refused("original worker changed"));
        };
        // A separate boolean is necessary: returning's receiver remains in the
        // private non-Clone handle, not duplicated in this registry.
        grant.return_started = true;
        Ok(WorkerReturn {
            request,
            peer,
            response,
            answer: None,
            deadline,
        })
    }
    pub(in crate::actor) fn workload_worker_returned(
        &mut self,
        mut returned: WorkerReturn,
    ) -> io::Result<host::worker::Returned> {
        let context = self.worker_context(returned.peer, &returned.request.request.original)?;
        let job = self
            .jobs
            .get_mut(&(
                returned
                    .request
                    .request
                    .original
                    .query
                    .binding
                    .session
                    .clone(),
                returned.request.request.original.query.binding.root_attempt,
            ))
            .ok_or_else(|| refused("original worker root unavailable"))?;
        let state = job
            .workload
            .as_mut()
            .ok_or_else(|| refused("original worker owner unavailable"))?;
        let key = (
            returned.request.request.original.member_attempt,
            returned.request.request.worker_request,
        );
        let Some(WorkerState::Granted(grant)) = state.bank.workers.get(&key) else {
            return Err(refused("original worker loan unavailable"));
        };
        if grant.slot != returned.request.slot
            || grant.loan != returned.request.loan
            || !grant.return_started
        {
            return Err(refused("original worker return changed"));
        }
        let answer = returned
            .answer
            .take()
            .ok_or_else(|| refused("worker return reply unavailable"))?;
        if !matches!(answer.reply.decision, wire::Decision::Returned) || answer.original.is_some() {
            return Err(refused("worker did not receive a positive original return"));
        }
        state.bank.workers.remove(&key);
        let reply = host::worker::Returned {
            context,
            worker_request: key.1,
            loan: returned.request.loan,
            slot: returned.request.slot,
            settlement: returned.request.settlement,
        };
        reply.encode()?;
        Ok(reply)
    }
}
