// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Same-process preparation for the original native resolver root. The real
//! fixed CPU, memory claim, GNU pipe and SDK keeper belong to its admission
//! owner; neither the entry selector nor this preparation creates a grant.

use super::*;
use aterm_types::resource_admission::Submission;
use std::os::unix::ffi::OsStrExt as _;
use std::{os::fd::OwnedFd, sync::Arc};

pub(super) const ENTRY: &[u8] = b"--resource-resolver-v1";

pub(super) fn purpose(request: &Submission) -> bool {
    request.argv.get(1).is_some_and(|word| word == ENTRY)
}

pub(super) fn closed_request(request: &Submission) -> bool {
    request.cpu_units == 1
        && matches!(request.timeout_ms, Some(1..=600_000))
        && matches!(request.argv.as_slice(), [_, entry] if entry == ENTRY)
}

/// Own the first trusted-image hold and second selected-image hold. The
/// admitted cohort is the same original PID/session throughout both steps.
pub(super) struct Preparation {
    prelude: crate::metadata_prelude::Owner,
    sdk: Arc<sdk::Keeper>,
    prepared: bool,
    environment: Vec<(Vec<u8>, Vec<u8>)>,
}

impl Preparation {
    pub(super) fn new(
        command: &spawnfd::Command,
        sdk: Arc<sdk::Keeper>,
        deadline: Instant,
    ) -> io::Result<(Self, spawnfd::Command)> {
        // A marker only declares support. These checks still retain and bind
        // the actual selected program/runtime and the original managed lease.
        sdk.require_resolver_protocol(deadline)?;
        let mut environment = Vec::new();
        for name in [
            b"CARGO".as_slice(),
            b"RUSTC",
            b"CARGO_HOME",
            b"CARGO_CACHE_RUSTC_INFO",
        ] {
            let value = command
                .get_envs()
                .find_map(
                    |(key, value)| {
                        if key.as_bytes() == name { value } else { None }
                    },
                )
                .ok_or_else(|| refused("resolver root environment is incomplete"))?;
            environment.push((name.to_vec(), value.as_bytes().to_vec()));
        }
        let (prelude, command) =
            crate::metadata_prelude::Owner::new(command, &[3, 4, sdk::TARGET as u32], deadline)?;
        Ok((
            Self {
                prelude,
                sdk,
                prepared: false,
                environment,
            },
            command,
        ))
    }

    pub(super) fn extra(&mut self) -> io::Result<(OwnedFd, i32)> {
        self.prelude.extra()
    }

    pub(super) fn configured(&mut self, command: &mut spawnfd::Command) -> io::Result<()> {
        self.prelude.configured(command)
    }

    /// Return true only while the selected resolver image is still stopped
    /// after its same-PID exec and the OS no-fork policy is positively known.
    /// The caller still owns the final GO and original root Started event.
    pub(super) fn poll(&mut self, child: &mut spawnfd::Child) -> io::Result<bool> {
        let sdk = Arc::clone(&self.sdk);
        let prepared = self.prelude.poll(child, |pid| {
            sdk.validate()?;
            sdk.validate_loaded(pid)
        })?;
        if prepared {
            self.sdk.validate()?;
            self.sdk.validate_loaded(child.id())?;
            if !crate::metadata_prelude::no_fork(child.id())? {
                return Err(refused("resolver root no-fork policy is unavailable"));
            }
            self.prepared = true;
        }
        Ok(prepared)
    }

    pub(super) fn prepared(&self) -> bool {
        self.prepared
    }

    pub(super) fn query_environment_matches(&self, environment: &[(Vec<u8>, Vec<u8>)]) -> bool {
        self.environment
            .iter()
            .all(|original| environment.contains(original))
    }
}

impl Owner {
    pub(super) fn tick_resolver_roots(&mut self) {
        use aterm_capacity::cohort::launch::CohortSignal;
        use aterm_types::resource_admission::Event;

        for job in self.jobs.values_mut() {
            if job.launched || job.reaped || job.cleanup_at.is_some() {
                continue;
            }
            let Some(preparation) = job.resolver.as_mut() else {
                continue;
            };
            let checked = if job.events.cancelled() {
                Err(io::Error::new(
                    io::ErrorKind::Interrupted,
                    "resolver root cancelled",
                ))
            } else {
                preparation.poll(job.child.child_mut())
            };
            let checked = checked.and_then(|prepared| {
                if !prepared {
                    return Ok(false);
                }
                // Retain original admission through the durable write and
                // recheck its actual claim immediately before final GO.
                self.journal
                    .update(&job.binding, "running", aterm_json::json!({}))?;
                job::inspection(&self.fence, &self.debt, job)?;
                if job.events.cancelled() {
                    return Err(io::ErrorKind::Interrupted.into());
                }
                let (monotonic_ns, unix_ns) = start::clocks()?;
                let started = Instant::now();
                job.child.child_mut().resume()?;
                job.started = started;
                job.launched = true;
                let _ = job.events.emit(Event::Started {
                    pid: job.child.child_mut().id(),
                    birth: job.root_birth,
                    monotonic_ns,
                    unix_ns,
                });
                Ok(true)
            });
            if let Err(error) = checked {
                // The selected root has no Started claim. Keep its actual
                // cohort/custody until normal positive retirement; never
                // synthesize a selected-program exit or an empty scope.
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
