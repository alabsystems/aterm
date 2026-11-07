// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! A helper's transport peer is distinct from its proven native producer.
//! Admission still borrows the exact original job and actual descriptors.
use super::*;
use aterm_ctl::resource_subgroup_client::{AuthorizedSubgroup, Rights};
use aterm_types::{resource_admission::Submission, resource_subgroup as subgroup};
use trust_resource_controller as wire;

impl Owner {
    pub(in super::super) fn provisional_proxy(
        &mut self,
        request: &wire::Request,
        helper: Peer,
    ) -> bool {
        let member = Binding {
            session: request.binding.session.clone(),
            attempt: request.member_attempt,
        };
        if !matches!(self.journal.fresh(&member), Ok(true)) {
            return false;
        }
        let key = (
            request.binding.session.clone(),
            request.binding.root_attempt,
        );
        let Some(job) = self.jobs.get_mut(&key) else {
            return false;
        };
        if self.faulted
            || !job.native_targo
            || !job.launched
            || job.reaped
            || job.settled
            || job.cleanup_at.is_some()
            || !matches!(&job.custody, Custody::Cooperative(_))
        {
            return false;
        }
        let difference = request
            .binding
            .capability
            .iter()
            .zip(job.delegation.capability.as_bytes())
            .fold(0u8, |sum, (left, right)| sum | (left ^ right));
        if difference != 0 {
            return false;
        }
        let Ok(coverage) = job::inspection(&self.fence, &self.debt, job) else {
            return false;
        };
        translate(request, helper, coverage.memory.bytes).is_ok_and(|input| {
            member_recipe::command(job, &input, request.timeout_ns.map(Duration::from_nanos))
                .is_ok()
        })
    }
    pub(in super::super) fn submit_proxy(
        &mut self,
        incoming: crate::resource_proxy_intake::Incoming,
    ) {
        let crate::resource_proxy_intake::Incoming {
            request,
            helper,
            proof,
            rights,
            accounted,
        } = incoming;
        let producer = proof.producer();
        let peer = Peer {
            pid: producer.pid,
            birth: producer.birth,
        };
        let accepted = proof.validate().is_ok()
            && proof.binding() == (request.binding.root_attempt, request.member_attempt)
            && proof.helper().pid == helper.pid
            && proof.helper().birth == helper.birth
            && self.provisional_proxy(&request, helper);
        let key = (
            request.binding.session.clone(),
            request.binding.root_attempt,
        );
        let input = self
            .jobs
            .get_mut(&key)
            .and_then(|job| job::inspection(&self.fence, &self.debt, job).ok())
            .and_then(|coverage| translate(&request, peer, coverage.memory.bytes).ok());
        let Some(input) = input.filter(|_| accepted) else {
            if let Ok(mut events) = job::Events::new(
                rights.events,
                Binding {
                    session: request.binding.session,
                    attempt: request.member_attempt,
                },
            ) {
                if accounted && events.account().is_err() {
                    return;
                }
                let _ = events.emit(aterm_types::resource_admission::Event::Refused {
                    reason: "native producer or original coverage refused before launch".into(),
                });
            }
            return;
        };
        let named_targets = match &request.delegations.gnu {
            wire::GnuTransport::NamedFifo { read, write, .. } => Some([read.target, write.target]),
            _ => None,
        };
        let loan = match request.role {
            wire::Role::MachOLinker(loan) => {
                Some(parent_loan::Edge::of(loan, request.member_attempt))
            }
            _ => None,
        };
        self.submit_member_proved(
            AuthorizedSubgroup {
                request: input,
                peer,
                rights: Rights {
                    stdin: rights.stdin,
                    stdout: rights.stdout,
                    stderr: rights.stderr,
                    events: rights.events,
                    delegated: rights.delegated,
                },
            },
            Some(proof),
            request.command.arg0,
            request.timeout_ns.map(Duration::from_nanos),
            named_targets,
            member_queue::ProxyMode { loan, accounted },
        );
    }
}

fn translate(
    input: &wire::Request,
    peer: Peer,
    memory_bytes: u64,
) -> io::Result<subgroup::Request> {
    input.validate()?;
    let cpu = input
        .delegations
        .cpu
        .ok_or_else(|| refused("native metadata needs its borrowed CPU unit"))?;
    if !input.delegations.fences.is_empty() {
        return Err(refused(
            "legacy fences cannot qualify native proxy execution",
        ));
    }
    let binding = Binding {
        session: input.binding.session.clone(),
        attempt: input.binding.root_attempt,
    };
    let request = subgroup::Request {
        job: binding.clone(),
        capability: subgroup::Capability::from_bytes(input.binding.capability),
        member: Submission {
            binding: Binding {
                attempt: input.member_attempt,
                ..binding
            },
            peer_pid: peer.pid,
            peer_birth: peer.birth,
            argv: std::iter::once(input.command.program.clone())
                .chain(input.command.args.iter().cloned())
                .collect(),
            env: input.command.environment.clone(),
            cwd: input.command.cwd.clone(),
            cpu_units: 1,
            memory_bytes,
            label: "native compiler member".into(),
            // The proxy's exact nanosecond deadline stays in the owned pending
            // state; never round it into this legacy millisecond transport.
            timeout_ms: None,
        },
        cpu: subgroup::CpuDelegation::SharedBorrow {
            target_fd: cpu.target,
        },
        gnu: match &input.delegations.gnu {
            wire::GnuTransport::None => subgroup::GnuTransport::None,
            wire::GnuTransport::NumericPipe { read, write } => {
                subgroup::GnuTransport::NumericPipe {
                    read_target: read.target,
                    write_target: write.target,
                }
            }
            wire::GnuTransport::NamedFifo {
                locator,
                device,
                inode,
                ..
            } => subgroup::GnuTransport::NamedFifo {
                locator: locator.clone(),
                witness: subgroup::FifoWitness {
                    device: *device,
                    inode: *inode,
                },
                descriptors: 2,
            },
        },
        fence_targets: Vec::new(),
    };
    request.validate()?;
    Ok(request)
}
