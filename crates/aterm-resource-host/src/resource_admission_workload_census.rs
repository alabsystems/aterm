// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Immutable paged DATA from all original registered native members. A flag,
//! status, metric, nonce or complete population never creates execution rights.
use super::*;
use trust_resource_controller::{self as control, workload::host::census as wire};

#[derive(Default)]
pub(super) struct Snapshots {
    seen: std::collections::BTreeSet<[u8; 16]>,
    current: Option<Snapshot>,
}
struct Snapshot {
    nonce: [u8; 16],
    entries: Vec<wire::Entry>,
    next: u32,
    complete: bool,
}
fn identity(pid: u32, birth: u64) -> control::Identity {
    control::Identity {
        pid,
        birth,
        birth_kind: if cfg!(target_os = "macos") {
            control::BirthKind::MacosWallMicroseconds
        } else {
            control::BirthKind::LinuxStartTicks
        },
    }
}
fn entry(member_attempt: [u8; 16], source: &Census) -> wire::Entry {
    let started = source.started.as_ref().map(|start| control::Started {
        identity: identity(start.pid, start.birth),
        monotonic_ns: start.monotonic_ns,
        unix_ns: start.unix_ns,
    });
    let observed = source.identity.map(|(pid, birth)| identity(pid, birth));
    let (terminal, accounting) = if source.uncertain {
        (
            control::Status::Unknown {
                reason: "native owner reported unresolved execution".into(),
                started,
            },
            control::accounting::Observation::uncollected(),
        )
    } else if let Some(complete) = &source.completion {
        let outcome = match &complete.outcome {
            aterm_types::resource_admission::Outcome::Exit(code) => control::Outcome::Exit(*code),
            aterm_types::resource_admission::Outcome::Signal(signal) => {
                control::Outcome::Signal(*signal)
            }
        };
        let cause = match complete.cause {
            CompletionCause::Exited => control::CompletionCause::Exited,
            CompletionCause::Timeout => control::CompletionCause::Timeout,
            CompletionCause::Cancelled => control::CompletionCause::Cancelled,
            CompletionCause::ResourceLimit => control::CompletionCause::ResourceLimit,
            CompletionCause::HostFault => control::CompletionCause::HostFault,
        };
        (
            control::Status::Finished {
                outcome,
                cause,
                runtime_ns: complete.runtime_ns,
            },
            complete.accounting,
        )
    } else if source.started.is_none() && source.closed {
        (
            control::Status::Refused {
                reason: "native target did not start; original no-ARM or scope closure settled"
                    .into(),
            },
            control::accounting::Observation::uncollected(),
        )
    } else {
        (
            control::Status::Unknown {
                reason: "original native terminal remains unavailable".into(),
                started,
            },
            control::accounting::Observation::uncollected(),
        )
    };
    let kind = match source.kind {
        CensusKind::Compiler => wire::SourceKind::Compiler,
        CensusKind::Linker => wire::SourceKind::Linker,
        CensusKind::Query => wire::SourceKind::Query,
        CensusKind::BuildScript => wire::SourceKind::BuildScript,
        CensusKind::Inventory => wire::SourceKind::Inventory,
        CensusKind::Sample => wire::SourceKind::Sample,
    };
    wire::Entry {
        member_attempt,
        kind,
        identity: observed.or_else(|| started.map(|start| start.identity)),
        started,
        terminal,
        accounting,
        retired: source.retired,
        drained: source.drained,
    }
}
impl Snapshots {
    fn page(
        &mut self,
        request: &wire::Request,
        entries: Option<Vec<wire::Entry>>,
    ) -> io::Result<wire::Reply> {
        let same = self
            .current
            .as_ref()
            .is_some_and(|current| current.nonce == request.snapshot);
        if !same {
            if request.offset != 0
                || self
                    .current
                    .as_ref()
                    .is_some_and(|current| !current.complete)
                || self.seen.contains(&request.snapshot)
                || self.seen.len() >= 4096
            {
                return Err(refused(
                    "census snapshot is replayed, incomplete or exhausted",
                ));
            }
            let entries =
                entries.ok_or_else(|| refused("actual native census capture unavailable"))?;
            if entries.len() > wire::MAX_ENTRIES as usize {
                return Err(refused(
                    "actual native member population exceeds closed bound",
                ));
            }
            self.seen.insert(request.snapshot);
            self.current = Some(Snapshot {
                nonce: request.snapshot,
                entries,
                next: 0,
                complete: false,
            });
        }
        let current = self
            .current
            .as_mut()
            .ok_or_else(|| refused("original census snapshot unavailable"))?;
        if current.complete || request.offset != current.next {
            return Err(refused("census page is replayed or skipped"));
        }
        let total = current.entries.len() as u32;
        let available = &current.entries[request.offset as usize..];
        let mut count = available.len().min(wire::MAX_PAGE_ENTRIES);
        loop {
            let reply = wire::Reply::Page(wire::Page {
                request: request.clone(),
                total,
                entries: available[..count].to_vec(),
            });
            if reply.encode().is_ok() {
                current.next = request.offset + count as u32;
                current.complete = current.next == total;
                return Ok(reply);
            }
            if count <= 1 {
                return Err(refused(
                    "complete census entry cannot fit the unchanged native frame",
                ));
            }
            // Keep every original entry. Large bounded terminal data reduces
            // one page, never the population, payload or evidence requirements.
            count /= 2;
        }
    }
}
impl Owner {
    pub(in crate::actor) fn workload_census(
        &mut self,
        peer: Peer,
        session: &str,
        request: &wire::Request,
    ) -> io::Result<wire::Reply> {
        request.encode()?;
        self.fence.check_current()?;
        if self.faulted {
            return Err(refused("workload census custody is faulted"));
        }
        let job = self
            .jobs
            .get_mut(&(session.to_owned(), request.root_attempt))
            .ok_or_else(|| refused("original census root unavailable"))?;
        if !job.launched
            || job.reaped
            || job.is_settled()
            || job.cleanup_at.is_some()
            || peer.pid != job.child.child_mut().id()
            || peer.birth != job.root_birth
        {
            return Err(refused("census caller is not its original live root"));
        }
        job.metadata_sdk
            .as_ref()
            .ok_or_else(|| refused("original census SDK unavailable"))?
            .validate_loaded(peer.pid)?;
        let state = job
            .workload
            .as_mut()
            .ok_or_else(|| refused("original workload census unavailable"))?;
        if state.ready_digest()? != request.invocation_sha256 {
            return Err(refused("census invocation differs from original owner"));
        }
        let same = state
            .snapshot
            .current
            .as_ref()
            .is_some_and(|current| current.nonce == request.snapshot);
        if !same {
            if request.offset != 0 {
                return Err(refused("new census must begin at original offset zero"));
            }
            if !job.members.is_empty()
                || !job.loans.is_empty()
                || !state.bank.drained()
                || state.produced.preparing()
            {
                return Ok(wire::Reply::Busy {
                    request: request.clone(),
                });
            }
        }
        let entries = (!same).then(|| {
            state
                .census
                .iter()
                .map(|(attempt, source)| entry(*attempt, source))
                .collect()
        });
        state.snapshot.page(request, entries)
    }
}

#[cfg(test)]
#[path = "resource_admission_workload_census_tests.rs"]
mod tests;
