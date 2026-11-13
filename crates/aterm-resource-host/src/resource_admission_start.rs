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
                if pending
                    .original_deadline()?
                    .is_some_and(|deadline| Instant::now() >= deadline)
                {
                    return Err(refused(
                        "original workload deadline expired before suspended birth",
                    ));
                }
                if let Some(preflight) = pending.preflight.as_mut() {
                    if !matches!(custody, Custody::Fixed(_)) {
                        return Err(refused("native preflight requires original fixed custody"));
                    }
                    let mut command = preflight.take_command()?;
                    stdio(&pending.submission.fds, &mut command)?;
                    // No job locator is exported to this internal query.
                    return Ok((command, context(&pending.submission.request.binding)?));
                }
                let recipe = pending
                    .recipe
                    .as_ref()
                    .ok_or_else(|| refused("missing admitted recipe"))?;
                let mut command = recipe.command(&pending.submission.request);
                recipe.configure_ay(&mut command)?;
                let resolver = matches!(recipe.kind, recipe::Kind::Resolver);
                if matches!(recipe.kind, recipe::Kind::BootstrapGraph) {
                    let sdk = service_sdk
                        .as_ref()
                        .ok_or_else(|| refused("Bootstrap Root SDK keeper missing"))?;
                    let deadline = pending
                        .original_deadline()?
                        .ok_or_else(|| refused("Bootstrap queued deadline missing"))?;
                    let Custody::Fixed(original) = &mut custody else {
                        return Err(refused(
                            "Bootstrap Root requires actual retained fixed custody",
                        ));
                    };
                    let ceiling = std::num::NonZeroU32::new(recipe.width)
                        .ok_or_else(|| refused("Bootstrap fixed ceiling is empty"))?;
                    let gnu = original.prepare_gnu(&self.fence, ceiling)?;
                    let (root, prelude) =
                        bootstrap_graph::Root::new(&mut command, gnu, sdk.clone(), deadline)?;
                    pending.sdk = Some(sdk.context()?);
                    pending.bootstrap_graph = Some(root);
                    command = prelude;
                } else if matches!(
                    recipe.kind,
                    recipe::Kind::Workload | recipe::Kind::NativeJob
                ) {
                    let sdk = service_sdk
                        .as_ref()
                        .ok_or_else(|| refused("original workload SDK missing"))?;
                    pending
                        .primary
                        .as_ref()
                        .ok_or_else(|| refused("original workload primary missing"))?
                        .configure(&mut command, sdk)?;
                    pending.sdk = Some(sdk.context()?);
                } else if matches!(recipe.kind, recipe::Kind::Metadata | recipe::Kind::Resolver) {
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
                if !matches!(recipe.kind, recipe::Kind::BootstrapGraph) {
                    command.env(
                        "ATERM_RESOURCE_INSPECTOR",
                        crate::runtime::inspector_program()?,
                    );
                }
                let delegation = context(&pending.submission.request.binding)?;
                command.env("ATERM_RESOURCE_JOB_V3", delegation.encode()?);
                if let Some(root) = pending.bootstrap_graph.as_mut() {
                    // Capture the actual original locator together with all
                    // original transport descriptions before the first ARM.
                    root.configured(&mut command)?;
                }
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
                    preparation.configured(&mut prelude)?;
                    pending.resolver = Some(preparation);
                    command = prelude;
                }
                stdio(&pending.submission.fds, &mut command)?;
                Ok((command, delegation))
            })();
        let (mut command, delegation) = match result {
            Ok(command) => command,
            Err(error) => {
                if pending.preflight.is_some() {
                    self.journal
                        .record_fault("preflight-launch-prepare", &error);
                }
                // No ARM was attempted; keep the exact custody for its normal
                // no-launch completion rather than inventing a birth receipt.
                let refusal = failure::stage_refusal(
                    pending.events,
                    "child preparation refused before launch",
                );
                self.failed.push(Failed {
                    _ay: pending.recipe.as_ref().and_then(|r| r.ay.clone()),
                    _primary: pending.primary.take(),
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
                self.journal.fault(&mut self.faulted, "root-launch", &error);
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
                    _ay: pending.recipe.as_ref().and_then(|r| r.ay.clone()),
                    _primary: pending.primary.take(),
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
            }
        }
    }
    fn start_bound(
        &mut self,
        mut pending: Pending,
        custody: Custody,
        mut child: aterm_capacity::cohort::launch::CohortChild,
        delegation: aterm_types::resource_subgroup::JobContext,
        metadata_gnu: Option<metadata::RootGnu>,
        service_sdk: Option<std::sync::Arc<sdk::Keeper>>,
    ) {
        let original_deadline = pending.original_deadline();
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
        let native_targo = pending.recipe.as_ref().is_some_and(|recipe| {
            matches!(
                recipe.kind,
                recipe::Kind::Targo
                    | recipe::Kind::Workload
                    | recipe::Kind::NativeJob
                    | recipe::Kind::BootstrapGraph
            )
        });
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
        let workload = pending.primary.take().and_then(|primary| {
            pending.recipe.as_ref().map(|recipe| {
                workload::State::new(primary, recipe, pending.submission.request.cwd.clone())
            })
        });
        let started = Instant::now();
        let selected_timeout = pending
            .submission
            .request
            .timeout_ms
            .map(Duration::from_millis);
        let timeout = match &original_deadline {
            Ok(Some(deadline)) => remaining_workload_timeout(*deadline, started),
            _ => selected_timeout,
        };
        // Check the SAME start stored in the original owner, after all paid
        // preparation/durable writes; an earlier now() is not this deadline.
        let deadline_supported = (workload.is_none()
            && pending.bootstrap_graph.is_none()
            && pending.preflight.is_none())
            || matches!(&original_deadline, Ok(Some(deadline)) if *deadline > started)
                && timeout.is_some_and(|timeout| started.checked_add(timeout).is_some());
        let mut job = Job {
            ay: pending.recipe.as_ref().and_then(|r| r.ay.clone()),
            #[cfg(all(test, target_os = "macos", target_arch = "aarch64"))]
            control_eof_before_reap_test: false,
            workload,
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
            contained_retirements: BTreeMap::new(),
            loans: BTreeMap::new(),
            events: pending.events,
            custody,
            child,
            root_birth: birth.unwrap_or(0),
            cpu_ceiling: width,
            memory_limit: pending.submission.request.memory_bytes,
            memory_at: Instant::now(),
            memory_fault: None,
            started,
            ended: None,
            exit_moment,
            timeout,
            cause: CompletionCause::Exited,
            cleanup_at: None,
            reaped: false,
            terminal_sent: false,
            completion: None,
            launched: false,
            closed: None,
            runtime_inputs: runtime_inputs::State::default(),
            bootstrap_graph: pending.bootstrap_graph,
            preflight: pending.preflight,
            settlement_at: None,
        };
        if prepared.is_ok()
            && deadline_supported
            && birth.is_some()
            && job.exit_moment.is_some()
            && !job.events.cancelled()
            && job.metadata_sdk.as_ref().is_none_or(|sdk| {
                sdk.validate().is_ok()
                    && (job.resolver.is_some()
                        || job.bootstrap_graph.is_some()
                        || (sdk.validate_loaded(pid).is_ok()
                            && aterm_uds::process::LoadedImage::read(pid as i32).is_some_and(
                                |image| image.matches_file(&job.program) == Some(true),
                            )))
            })
            && job
                .metadata_gnu
                .as_mut()
                .is_none_or(|gnu| gnu.bind_child(pid).is_ok())
            && job
                .workload
                .as_ref()
                .is_none_or(|owner| owner.primary.bind_child(pid).is_ok())
            && job
                .bootstrap_graph
                .as_ref()
                .is_none_or(|root| root.bind_child(pid).is_ok())
        {
            // The first hold is the trusted prelude, not the selected resolver.
            // Its owned preparation performs the two-stop transition; only a
            // positively validated selected image may receive final GO/Started.
            if job.resolver.is_some() || job.bootstrap_graph.is_some() || job.preflight.is_some() {
                self.jobs.insert((binding.session, binding.attempt), job);
                return;
            }
            if let Ok((monotonic_ns, unix_ns)) = clocks() {
                // Commit this exact authorization clock before GO. A durable
                // record alone does not prove that the stopped child ran.
                let started = Instant::now();
                let running = self.journal.update(
                    &binding,
                    "running",
                    aterm_json::json!({"monotonic_ns":monotonic_ns,"unix_ns":unix_ns}),
                );
                if running.is_ok() && job::inspection(&self.fence, &self.debt, &mut job).is_ok() {
                    let remaining = match &original_deadline {
                        Ok(Some(deadline)) => remaining_workload_timeout(*deadline, started),
                        _ => job.timeout,
                    };
                    let image_current = job
                        .ay
                        .as_ref()
                        .is_none_or(|ay| ay.validate_loaded(pid).is_ok());
                    // Neither durable IO nor image checks may renew a selected
                    // deadline or dispatch work after it has already expired.
                    let now = Instant::now();
                    let clock_current = match remaining {
                        Some(timeout) => started.checked_add(timeout).is_some_and(|end| now < end),
                        None => job.workload.is_none() && job.timeout.is_none(),
                    };
                    if image_current
                        && clock_current
                        && !job.events.cancelled()
                        && job.child.child_mut().resume().is_ok()
                    {
                        job.timeout = remaining;
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

fn context(binding: &Binding) -> io::Result<aterm_types::resource_subgroup::JobContext> {
    let mut secret = [0; 32];
    aterm_uds::rand::fill(&mut secret)?;
    Ok(aterm_types::resource_subgroup::JobContext {
        job: binding.clone(),
        capability: aterm_types::resource_subgroup::Capability::from_bytes(secret),
    })
}

fn stdio(fds: &PendingIo, command: &mut spawnfd::Command) -> io::Result<()> {
    let (stdin, (stdout, stderr)) = aterm_uds::kept_out(|| -> io::Result<_> {
        Ok((
            fds.stdin.try_clone()?,
            (fds.stdout.try_clone()?, fds.stderr.try_clone()?),
        ))
    })?;
    command
        .stdin(spawnfd::Stdio::from(stdin))
        .stdout(spawnfd::Stdio::from(stdout))
        .stderr(spawnfd::Stdio::from(stderr));
    Ok(())
}

/// Time-only observation. The SAME stored start/remaining pair must represent
/// a positive deadline no later than the original queued bound, including any
/// native clock conversion rounding.
pub(super) fn remaining_workload_timeout(deadline: Instant, started: Instant) -> Option<Duration> {
    let remaining = deadline.checked_duration_since(started)?;
    if remaining.is_zero() || started.checked_add(remaining)? > deadline {
        return None;
    }
    Some(remaining)
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
