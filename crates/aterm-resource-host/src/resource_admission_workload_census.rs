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
mod tests {
    use super::*;

    fn request(offset: u32) -> wire::Request {
        wire::Request {
            root_attempt: [1; 16],
            invocation_sha256: [2; 32],
            snapshot: [3; 16],
            offset,
        }
    }

    fn source(kind: CensusKind) -> Census {
        Census {
            kind,
            identity: None,
            completion: None,
            started: None,
            retired: false,
            drained: false,
            uncertain: false,
            closed: true,
            refused: true,
        }
    }

    #[test]
    fn native_census_reducer_preserves_original_status_zero_missing_and_unknown() {
        // Reducer controls use explicit observations, not a decoded admission
        // or a claim that these fixture values came from an owning wait.
        let mut original = source(CensusKind::Compiler);
        original.identity = Some((42, 9));
        original.started = Some(member::Started {
            pid: 42,
            birth: 9,
            monotonic_ns: 11,
            unix_ns: 12,
        });
        let mut accounting = control::accounting::Observation::uncollected();
        accounting.cpu_times = Some(control::accounting::CpuTimes {
            user_ns: 0,
            system_ns: 0,
        });
        original.completion = Some(member::Completed {
            outcome: aterm_types::resource_admission::Outcome::Signal(9),
            cause: CompletionCause::Cancelled,
            runtime_ns: 17,
            accounting,
        });
        original.retired = true;
        original.drained = true;
        let observed = entry([4; 16], &original);
        assert_eq!(observed.started.unwrap().monotonic_ns, 11);
        assert_eq!(observed.accounting, accounting);
        assert!(matches!(
            observed.terminal,
            control::Status::Finished {
                outcome: control::Outcome::Signal(9),
                cause: control::CompletionCause::Cancelled,
                runtime_ns: 17
            }
        ));
        original.uncertain = true;
        let unknown = entry([4; 16], &original);
        assert!(matches!(unknown.terminal, control::Status::Unknown { .. }));
        assert_eq!(
            unknown.accounting,
            control::accounting::Observation::uncollected()
        );
        assert_eq!(unknown.identity, observed.identity);
        let refused = entry([5; 16], &source(CensusKind::Query));
        assert!(matches!(refused.terminal, control::Status::Refused { .. }));
        assert_eq!(
            refused.accounting,
            control::accounting::Observation::uncollected()
        );
        assert!(refused.identity.is_none());
    }

    #[test]
    fn immutable_native_population_pages_keep_every_role_and_refuse_replay_or_skips() {
        let kinds = [
            CensusKind::Compiler,
            CensusKind::Linker,
            CensusKind::Query,
            CensusKind::BuildScript,
            CensusKind::Inventory,
            CensusKind::Sample,
        ];
        let originals: Vec<_> = (0_u64..260)
            .map(|index| {
                let mut attempt = [0; 16];
                attempt[..8].copy_from_slice(&(index + 10).to_be_bytes());
                entry(attempt, &source(kinds[index as usize % kinds.len()]))
            })
            .collect();
        let mut snapshots = Snapshots::default();
        let mut req = request(0);
        let mut received = Vec::new();
        loop {
            let capture = (req.offset == 0).then(|| originals.clone());
            let reply = snapshots.page(&req, capture).unwrap();
            let wire::Reply::Page(page) =
                wire::Reply::decode(&reply.encode().unwrap(), &req).unwrap()
            else {
                panic!("expected original page")
            };
            assert_eq!(page.total, 260);
            received.extend(page.entries);
            assert!(
                snapshots.page(&req, None).is_err(),
                "one-use original offset"
            );
            if received.len() == 260 {
                break;
            }
            let mut foreign = req.clone();
            foreign.snapshot = [9; 16];
            foreign.offset = 0;
            assert!(
                snapshots.page(&foreign, Some(Vec::new())).is_err(),
                "incomplete original cannot be replaced"
            );
            let mut skipped = req.clone();
            skipped.offset = received.len() as u32 + 1;
            assert!(snapshots.page(&skipped, None).is_err());
            req.offset = received.len() as u32;
        }
        assert_eq!(received, originals);
        assert!(snapshots.page(&request(0), Some(Vec::new())).is_err());
        let mut empty = request(0);
        empty.snapshot = [8; 16];
        let wire::Reply::Page(empty) = snapshots.page(&empty, Some(Vec::new())).unwrap() else {
            panic!("expected empty original population")
        };
        assert_eq!(empty.total, 0);
        assert!(empty.entries.is_empty());
    }
}
