// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Actual child custody and post-launch lifecycle. Root exit is an outcome,
//! never a memory/CPU closure receipt.
use super::*;
use aterm_capacity::cohort::launch::CohortSignal;
use aterm_types::resource_admission::{Event, Outcome};
use aterm_types::resource_inspection::{BirthKind, Cpu, Manifest, Memory, Subject};
use std::os::unix::process::ExitStatusExt as _;

#[path = "resource_admission_events.rs"]
mod events;
pub(super) use events::Events;

fn ended_cause(started: Instant, ended: Instant, timeout: Option<Duration>) -> CompletionCause {
    if timeout.is_some_and(|limit| ended.saturating_duration_since(started) >= limit) {
        CompletionCause::Timeout
    } else {
        CompletionCause::Exited
    }
}

fn memory_fault(
    sample: io::Result<u64>,
    limit: u64,
    pressure: Option<aterm_sysprobe::MemoryLevel>,
) -> Option<(CompletionCause, String)> {
    match sample {
        Err(error) => Some((
            CompletionCause::HostFault,
            format!("memory sampling: {error:?}"),
        )),
        Ok(bytes) if bytes > limit => Some((
            CompletionCause::ResourceLimit,
            format!("owned memory {bytes} exceeds claim {limit}"),
        )),
        Ok(bytes) if pressure == Some(aterm_sysprobe::MemoryLevel::Critical) => Some((
            CompletionCause::ResourceLimit,
            format!("critical system memory pressure; owned bytes={bytes}, claim={limit}"),
        )),
        Ok(_) => None,
    }
}

pub(super) fn inspection(
    fence: &Fence,
    debt: &ExternalDebt,
    job: &mut Job,
) -> io::Result<Inspection> {
    let (reference, bytes, fixed) = match &job.custody {
        Custody::Fixed(custody) => {
            let observed = custody.observation(fence)?;
            (observed.memory, observed.memory_bytes, true)
        }
        Custody::Cooperative(claim) => {
            fence.check_claim(claim)?;
            if !claim.cohort_bound(job.child.cohort()) {
                return Err(refused(
                    "cooperative claim is not bound to this original child",
                ));
            }
            (claim.cohort_closure_ref()?, claim.record().need.mem, false)
        }
    };
    let (device, inode) = debt.native_manifest_identity()?;
    let native_manifest = Manifest { device, inode };
    let cpu = if fixed {
        Cpu::RetainedFixed {
            units: job.cpu_ceiling,
            native_manifest,
        }
    } else {
        Cpu::CooperativeShared {
            ceiling: job.cpu_ceiling,
            native_manifest,
        }
    };
    let encoded = job.child.cohort().record().encode();
    let scope = &encoded["captured"]["scope"];
    let string = |key: &str| {
        scope[key]
            .as_str()
            .map(str::to_owned)
            .ok_or_else(|| refused("original child scope unavailable"))
    };
    let guard = reference.guard();
    #[cfg(target_os = "macos")]
    let root_birth_kind = BirthKind::MacosWallMicroseconds;
    #[cfg(target_os = "linux")]
    let root_birth_kind = BirthKind::LinuxStartTicks;
    #[cfg(not(any(target_os = "macos", target_os = "linux")))]
    return Err(refused("native root identity unavailable"));
    let result = Inspection {
        session: job.binding.session.clone(),
        attempt: job.binding.attempt,
        host: string("host")?,
        boot: string("boot")?,
        root_pid: job.child.child_mut().id(),
        root_birth: job.root_birth,
        root_birth_kind,
        subject: Subject::Root {
            pid: job.child.child_mut().id(),
            birth: job.root_birth,
        },
        cpu,
        memory: Memory {
            bytes,
            namespace: reference.namespace().to_owned(),
            marker: reference.marker().to_owned(),
            epoch: reference.epoch().to_owned(),
            guard_device: guard.device,
            guard_inode: guard.inode,
        },
    };
    result.json()?;
    Ok(result)
}
impl Owner {
    pub(super) fn tick_jobs(&mut self) {
        self.tick_resolver_roots();
        self.tick_bootstrap_roots();
        self.tick_bootstrap_queries();
        let fence = &self.fence;
        let journal = &self.journal;
        let faulted = &mut self.faulted;
        self.jobs.retain(|_, job| {
            if job.cleanup_at.is_none()
                && (job
                    .bootstrap_graph
                    .as_ref()
                    .is_some_and(|root| root.query_failed())
                    || job
                        .workload
                        .as_ref()
                        .and_then(|state| state.control.as_ref())
                        .is_some_and(|link| link.closed() && !link.drained()))
            {
                // EOF caused by our existing cancellation/timeout is not a
                // second resource failure. Preserve the original accepted
                // cause through the same owning wait and settlement.
                journal.record_fault(
                    "root-control",
                    "bootstrap query failed or workload control closed before drain",
                );
                job.cause = CompletionCause::HostFault;
                job.cleanup_at.get_or_insert_with(Instant::now);
            }
            // This is sampled governance over actual owned groups, not a hard
            // kernel quota or evidence of closure. Unknown coverage is never
            // treated as zero. Original custody remains held during cleanup.
            if (job.launched || job.resolver.is_some() || job.bootstrap_graph.is_some())
                && !job.is_settled()
                && (!job.reaped || job.members.values().any(|member| member.group().is_some()))
                && job.memory_at.elapsed() >= Duration::from_millis(100)
            {
                job.memory_at = Instant::now();
                let mut groups: Vec<_> = job
                    .members
                    .values()
                    .filter_map(|member| member.group())
                    .collect();
                if let Some(root) = &job.bootstrap_graph {
                    match root.query_sampling_group() {
                        Ok(Some(group)) => groups.push(group),
                        Ok(None) => {}
                        Err(error) => {
                            journal.record_fault("memory-query-group", error);
                            job.memory_fault = Some(CompletionCause::HostFault);
                        }
                    }
                }
                if !job.reaped {
                    if let Ok(group) = i32::try_from(job.child.child_mut().id()) {
                        groups.push(group);
                    } else {
                        journal.record_fault(
                            "memory-root-group",
                            "root PID cannot be represented as a process group",
                        );
                        job.memory_fault = Some(CompletionCause::HostFault);
                    }
                }
                if job.memory_fault.is_none() {
                    let sample = crate::resource_memory::sample(&groups);
                    let pressure = aterm_sysprobe::memory_pressure();
                    if let Some((cause, detail)) = memory_fault(sample, job.memory_limit, pressure)
                    {
                        journal.record_fault("memory-observation", &detail);
                        job.memory_fault = Some(cause);
                    }
                }
            }
            if let Some(cause) = job.memory_fault
                && job.cause != cause
                && job.cause != CompletionCause::HostFault
            {
                job.cause = cause;
                job.cleanup_at.get_or_insert_with(Instant::now);
                if !job.reaped
                    && let Err(error) = job.child.request_group(CohortSignal::Kill)
                {
                    journal.fault(faulted, "memory-kill", error);
                    let _ = job.child.child_mut().kill();
                }
            }
            if !job.reaped {
                let exited = job.child.child_mut().exited().unwrap_or(false);
                if job.ended.is_none() {
                    job.ended = job
                        .exit_moment
                        .as_ref()
                        .and_then(aterm_uds::exitwatch::ExitMoment::get);
                }
                if job.cleanup_at.is_none() {
                    let cause = if let Some(ended) = job.ended {
                        Some(ended_cause(job.started, ended, job.timeout))
                    } else if exited {
                        // Wait for the already registered exit notification's
                        // stamp before deciding which side of the deadline it
                        // witnessed. Actor wake latency must not choose exit.
                        None
                    } else if job.memory_fault.is_some() {
                        job.memory_fault
                    } else if job
                        .timeout
                        .is_some_and(|limit| job.started.elapsed() >= limit)
                    {
                        Some(CompletionCause::Timeout)
                    } else if job.events.cancelled() {
                        Some(CompletionCause::Cancelled)
                    } else {
                        None
                    };
                    if let Some(cause) = cause {
                        job.cause = cause;
                        job.cleanup_at = Some(Instant::now());
                        let signal = if matches!(
                            cause,
                            CompletionCause::Timeout | CompletionCause::ResourceLimit
                        ) {
                            CohortSignal::Kill
                        } else {
                            CohortSignal::Terminate
                        };
                        if let Err(error) = job.child.request_group(signal) {
                            let already_drained = job.child.child_mut().exited().unwrap_or(false)
                                && crate::resource_memory::other_members_exited(
                                    job.child.child_mut().id(),
                                );
                            if !already_drained {
                                journal.fault(faulted, "cleanup-signal", error);
                                let _ = job.child.child_mut().kill();
                            }
                        }
                    }
                }
                // Signal only while the original unreaped leader still pins
                // its group identity. Reaping never grants later numeric control.
                let drained = exited
                    && job
                        .bootstrap_graph
                        .as_ref()
                        .is_none_or(|root| root.native_queries_drained())
                    && crate::resource_memory::other_members_exited(job.child.child_mut().id());
                if job
                    .cleanup_at
                    .is_some_and(|at| at.elapsed() >= Duration::from_secs(2))
                    && let Err(error) = job.child.request_group(CohortSignal::Kill)
                    && !drained
                {
                    journal.fault(faulted, "cleanup-kill", error);
                    let _ = job.child.child_mut().kill();
                }
                // For launched jobs use the kernel notification's stamp, not
                // the actor's next polling instant. Its publisher may still
                // be finishing when the independent exited probe first sees
                // the zombie; retain original control until it is available.
                if (!job.launched || job.ended.is_some()) && drained {
                    #[cfg(all(test, target_os = "macos", target_arch = "aarch64"))]
                    if job.control_eof_before_reap_test
                        && job
                            .workload
                            .as_ref()
                            .and_then(|state| state.control.as_ref())
                            .is_some_and(|control| !control.closed())
                    {
                        return true;
                    }
                    let _ = job.child.request_group(CohortSignal::Kill);
                    if let Ok(Some((status, observation))) = crate::resource_accounting::try_reap(
                        job.child.child_mut(),
                        job.events.requires_accounting(),
                    ) {
                        job.reaped = true;
                        let outcome = match (status.code(), status.signal()) {
                            (Some(code), _) => Some(Outcome::Exit(code)),
                            (_, Some(signal)) => Some(Outcome::Signal(signal)),
                            _ => None,
                        };
                        if let Some(outcome) = outcome {
                            if let Some(sdk) = job.metadata_sdk.as_ref()
                                && let Err(error) = sdk.validate()
                            {
                                // Preserve the real exit while refusing input
                                // integrity. This check grants no drain fact.
                                journal.record_fault("terminal-inputs", error);
                                job.cause = CompletionCause::HostFault;
                            }
                            let runtime_ns = u64::try_from(
                                job.ended
                                    .get_or_insert_with(Instant::now)
                                    .duration_since(job.started)
                                    .as_nanos(),
                            )
                            .unwrap_or(u64::MAX);
                            let event = Event::Finished {
                                outcome,
                                cause: job.cause,
                                runtime_ns,
                            };
                            if job.events.requires_accounting() && job.launched {
                                // An owning wait is an outcome observation, not a
                                // resource-settlement fact. Retain it until closure.
                                job.completion = Some((event, observation));
                            } else {
                                if job.launched {
                                    let _ = job.events.emit(event);
                                }
                                job.terminal_sent = true;
                            }
                            let faults = (job.cause == CompletionCause::HostFault).then(||
                                journal.fault_summary().chars().take(4096).collect::<String>());
                            if journal
                                .update(&job.binding, "terminal", aterm_json::json!({"cause": format!("{:?}", job.cause), "host_faults": faults}))
                                .is_err()
                            {
                                return true;
                            }
                        }
                    }
                }
            }
            if job.reaped
                && !job.is_settled()
                && job.members.is_empty()
                && job
                    .workload
                    .as_ref()
                    .is_none_or(|state| !state.registration_preparing())
                && job
                    .settlement_at
                    .is_none_or(|at| at.elapsed() >= Duration::from_secs(1))
            {
                job.settlement_at = Some(Instant::now());
                let _ = settlement::settle(job, fence, journal);
            }
            if job.is_settled()
                && !job.terminal_sent
                && let Some((event, observation)) = job.completion.take()
            {
                // All member owners and the original cohort have positively
                // settled. Drop remaining original SDK/GNU/image owners before
                // publishing the accounted root terminal.
                job.resolver.take();
                job.metadata_sdk.take();
                job.metadata_gnu.take();
                job.compiler.take();
                job.ay.take();
                job.workload.take();
                let _ = job.events.emit_terminal(event, observation);
                job.terminal_sent = true;
            }
            !(job.is_settled() && job.terminal_sent && job.events.drained())
        });
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn memory_sampling_error_names_host_fault_not_a_limit() {
        let (cause, detail) =
            memory_fault(Err(io::Error::from_raw_os_error(libc::EACCES)), 1024, None).unwrap();
        assert_eq!(cause, CompletionCause::HostFault);
        assert!(detail.contains("PermissionDenied"));
    }

    #[test]
    fn measured_memory_limit_and_critical_pressure_keep_their_evidence() {
        let (cause, detail) = memory_fault(Ok(1025), 1024, None).unwrap();
        assert_eq!(cause, CompletionCause::ResourceLimit);
        assert!(detail.contains("1025") && detail.contains("1024"));
        let (cause, detail) =
            memory_fault(Ok(512), 1024, Some(aterm_sysprobe::MemoryLevel::Critical)).unwrap();
        assert_eq!(cause, CompletionCause::ResourceLimit);
        assert!(detail.contains("critical") && detail.contains("512"));
        assert!(memory_fault(Ok(1024), 1024, None).is_none());
    }

    #[test]
    fn completed_stamp_decides_deadline_independently_of_actor_wake() {
        let start = Instant::now();
        let limit = Duration::from_secs(3);
        for (elapsed, expected) in [
            (Duration::from_secs(2), CompletionCause::Exited),
            (limit, CompletionCause::Timeout),
            (Duration::from_secs(4), CompletionCause::Timeout),
        ] {
            assert_eq!(ended_cause(start, start + elapsed, Some(limit)), expected);
            assert_eq!(
                ended_cause(start, start + elapsed, None),
                CompletionCause::Exited
            );
        }
    }
}
