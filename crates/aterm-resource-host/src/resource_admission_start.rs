// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Admission precedes ARM; actual suspended birth precedes durable BIND; only
//! then can execution start and its timeout clock become meaningful.
use super::*;
use aterm_capacity::cohort::launch::LaunchError;
use aterm_types::resource_admission::Event;
use std::time::{SystemTime, UNIX_EPOCH};

impl Owner {
    pub(super) fn launch(&mut self, mut pending: Pending, mut custody: Custody) {
        let mut metadata_gnu = None;
        let service_sdk = pending
            .recipe
            .as_ref()
            .and_then(|recipe| recipe.metadata_sdk.clone());
        let result: io::Result<(spawnfd::Command, aterm_types::resource_subgroup::JobContext)> =
            (|| {
                let recipe = pending
                    .recipe
                    .as_ref()
                    .ok_or_else(|| refused("missing admitted recipe"))?;
                let mut command = recipe.command(&pending.submission.request);
                let resolver = matches!(recipe.kind, recipe::Kind::Resolver);
                if matches!(recipe.kind, recipe::Kind::Metadata | recipe::Kind::Resolver) {
                    let sdk = service_sdk
                        .as_ref()
                        .ok_or_else(|| refused("metadata service has no original SDK keeper"))?;
                    sdk.validate()?;
                    let mut gnu = metadata::RootGnu::new()?;
                    if !resolver {
                        gnu.configure(&mut command, vec![(sdk.descriptor()?, sdk::TARGET)])?;
                    }
                    command.env("ATERM_SDK_LEASE_V3", sdk::TARGET.to_string());
                    metadata_gnu = Some(gnu);
                } else {
                    pending.sdk = sdk::root(recipe.compiler.as_ref(), &mut command)?;
                }
                // Locator only: the helper itself accepts custody lookup only
                // from this exact cached image and a validated native ancestor.
                command.env(
                    "ATERM_RESOURCE_INSPECTOR",
                    crate::runtime::inspector_program()?,
                );
                let mut secret = [0; 32];
                aterm_uds::rand::fill(&mut secret)?;
                let delegation = aterm_types::resource_subgroup::JobContext {
                    job: pending.submission.request.binding.clone(),
                    capability: aterm_types::resource_subgroup::Capability::from_bytes(secret),
                };
                command.env("ATERM_RESOURCE_JOB_V3", delegation.encode()?);
                if resolver {
                    let sdk = service_sdk
                        .as_ref()
                        .ok_or_else(|| refused("resolver root SDK keeper is unavailable"))?;
                    let deadline = Instant::now()
                        + Duration::from_millis(
                            pending
                                .submission
                                .request
                                .timeout_ms
                                .unwrap_or(0)
                                .min(30_000),
                        );
                    let (mut preparation, mut prelude) =
                        resolver_root::Preparation::new(&command, sdk.clone(), deadline)?;
                    metadata_gnu
                        .as_mut()
                        .ok_or_else(|| refused("resolver root original GNU is unavailable"))?
                        .configure(
                            &mut prelude,
                            vec![(sdk.descriptor()?, sdk::TARGET), preparation.extra()?],
                        )?;
                    preparation.configured(&prelude)?;
                    pending.resolver = Some(preparation);
                    command = prelude;
                }
                command
                    .stdin(spawnfd::Stdio::from(
                        pending.submission.fds.stdin.try_clone()?,
                    ))
                    .stdout(spawnfd::Stdio::from(
                        pending.submission.fds.stdout.try_clone()?,
                    ))
                    .stderr(spawnfd::Stdio::from(
                        pending.submission.fds.stderr.try_clone()?,
                    ));
                Ok((command, delegation))
            })();
        let (mut command, delegation) = match result {
            Ok(command) => command,
            Err(_) => {
                // No ARM was attempted; keep the exact custody for its normal
                // no-launch completion rather than inventing a birth receipt.
                let refusal = failure::stage_refusal(
                    pending.events,
                    "child preparation refused before launch",
                );
                self.failed.push(Failed {
                    _compiler: pending.recipe.as_ref().and_then(|r| r.compiler.clone()),
                    binding: pending.submission.request.binding,
                    custody: Some(custody),
                    refusal,
                    settled: false,
                    spawned: None,
                    no_child: None,
                    checked_at: None,
                    _sdk: service_sdk,
                });
                return;
            }
        };
        let child = match &mut custody {
            Custody::Fixed(custody) => custody.launch(&self.fence, &mut command),
            Custody::Cooperative(claim) => claim.launch_cohort(&mut command),
        };
        match child {
            Ok(child) => self.start_bound(
                pending,
                custody,
                child,
                delegation,
                metadata_gnu,
                service_sdk,
            ),
            Err(error) => {
                let (spawned, no_child, refusal) = match error {
                    LaunchError::NotSpawned(proof) => (
                        None,
                        Some(proof),
                        failure::stage_refusal(pending.events, "native target did not start"),
                    ),
                    LaunchError::Unbound { spawned, .. } => (Some(spawned), None, None),
                    LaunchError::Refused(_) => (None, None, None),
                };
                // An unbound or unknown native outcome has no affirmative
                // Refused/Started fact. Closing yields transport-unknown.
                self.failed.push(Failed {
                    _compiler: pending.recipe.as_ref().and_then(|r| r.compiler.clone()),
                    binding: pending.submission.request.binding,
                    custody: Some(custody),
                    refusal,
                    settled: false,
                    spawned,
                    no_child,
                    checked_at: None,
                    _sdk: service_sdk,
                });
                self.faulted = true;
            }
        }
    }
    fn start_bound(
        &mut self,
        pending: Pending,
        custody: Custody,
        mut child: aterm_capacity::cohort::launch::CohortChild,
        delegation: aterm_types::resource_subgroup::JobContext,
        metadata_gnu: Option<metadata::RootGnu>,
        service_sdk: Option<std::sync::Arc<sdk::Keeper>>,
    ) {
        let binding = pending.submission.request.binding;
        let pid = child.child_mut().id();
        let birth = i32::try_from(pid)
            .ok()
            .and_then(aterm_uds::process::read_birth)
            .map(|birth| birth.0);
        let exit_moment =
            aterm_uds::exitwatch::ExitMoment::watch_then(pid, Instant::now, crate::actor::wake);
        let width = pending.recipe.as_ref().map_or(0, |recipe| recipe.width);
        let program = pending
            .recipe
            .as_ref()
            .map(|recipe| recipe.program.clone())
            .unwrap_or_default();
        let native_targo = pending
            .recipe
            .as_ref()
            .is_some_and(|recipe| matches!(recipe.kind, recipe::Kind::Targo));
        let metadata_root = pending
            .recipe
            .as_ref()
            .is_some_and(|recipe| matches!(recipe.kind, recipe::Kind::Metadata));
        let prepared = self.journal.update(
            &binding,
            "bound",
            aterm_json::json!({"root_pid":pid,"root_birth":birth,
            "cohort":child.cohort().record().encode(),"sdk":sdk::json(&pending.sdk)}),
        );
        let mut job = Job {
            binding: binding.clone(),
            delegation,
            program,
            native_targo,
            metadata_root,
            metadata_ready: false,
            resolver_ready: false,
            resolver_queries: 0,
            metadata_gnu,
            metadata_sdk: service_sdk,
            resolver: pending.resolver,
            compiler: pending.recipe.as_ref().and_then(|r| r.compiler.clone()),
            sdk: pending.sdk,
            members: BTreeMap::new(),
            loans: BTreeMap::new(),
            events: pending.events,
            custody,
            child,
            root_birth: birth.unwrap_or(0),
            cpu_ceiling: width,
            memory_limit: pending.submission.request.memory_bytes,
            memory_at: Instant::now(),
            memory_fault: false,
            started: Instant::now(),
            ended: None,
            exit_moment,
            timeout: pending
                .submission
                .request
                .timeout_ms
                .map(Duration::from_millis),
            cause: CompletionCause::Exited,
            cleanup_at: None,
            reaped: false,
            terminal_sent: false,
            completion: None,
            launched: false,
            settled: false,
            settlement_at: None,
        };
        if prepared.is_ok()
            && birth.is_some()
            && job.exit_moment.is_some()
            && !job.events.cancelled()
            && job.metadata_sdk.as_ref().is_none_or(|sdk| {
                sdk.validate().is_ok()
                    && (job.resolver.is_some()
                        || (sdk.validate_loaded(pid).is_ok()
                            && aterm_uds::process::LoadedImage::read(pid as i32).is_some_and(
                                |image| image.matches_file(&job.program) == Some(true),
                            )))
            })
            && job
                .metadata_gnu
                .as_mut()
                .is_none_or(|gnu| gnu.bind_child(pid).is_ok())
        {
            // The first hold is the trusted prelude, not the selected resolver.
            // Its owned preparation performs the two-stop transition; only a
            // positively validated selected image may receive final GO/Started.
            if job.resolver.is_some() {
                self.jobs.insert((binding.session, binding.attempt), job);
                return;
            }
            let running = self
                .journal
                .update(&binding, "running", aterm_json::json!({}));
            if running.is_ok() && job::inspection(&self.fence, &self.debt, &mut job).is_ok() {
                // Sample before GO: an immediately exiting child can publish
                // its exit stamp before the resume syscall returns. All queue,
                // probe and durable handoff waits have already ended.
                if let Ok((monotonic_ns, unix_ns)) = clocks() {
                    let started = Instant::now();
                    // A durable running write may have waited while the
                    // caller cancelled or disconnected. Recheck at GO.
                    if !job.events.cancelled() && job.child.child_mut().resume().is_ok() {
                        job.started = started;
                        job.launched = true;
                        let _ = job.events.emit(Event::Started {
                            pid,
                            birth: job.root_birth,
                            monotonic_ns,
                            unix_ns,
                        });
                    }
                }
            }
        }
        if !job.launched {
            job.events.abandon();
            job.cause = CompletionCause::Cancelled;
            job.cleanup_at = Some(Instant::now());
            let _ = job
                .child
                .request_group(aterm_capacity::cohort::launch::CohortSignal::Kill);
        }
        self.jobs.insert((binding.session, binding.attempt), job);
    }
}

pub(super) fn clocks() -> io::Result<(u64, u64)> {
    let mut value = std::mem::MaybeUninit::<libc::timespec>::uninit();
    // SAFETY: the successful read-only kernel clock call initializes exactly
    // the writable timespec allocated here; no process authority is requested.
    if unsafe { libc::clock_gettime(libc::CLOCK_MONOTONIC, value.as_mut_ptr()) } != 0 {
        return Err(io::Error::last_os_error());
    }
    // SAFETY: success above initialized the complete native timespec.
    let value = unsafe { value.assume_init() };
    let monotonic = u64::try_from(value.tv_sec)
        .ok()
        .and_then(|seconds| seconds.checked_mul(1_000_000_000))
        .zip(
            u64::try_from(value.tv_nsec)
                .ok()
                .filter(|n| *n < 1_000_000_000),
        )
        .and_then(|(seconds, nanos)| seconds.checked_add(nanos))
        .ok_or_else(|| refused("native launch clock unavailable"))?;
    let unix = u64::try_from(
        SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map_err(io::Error::other)?
            .as_nanos(),
    )
    .map_err(io::Error::other)?;
    Ok((monotonic, unix))
}
