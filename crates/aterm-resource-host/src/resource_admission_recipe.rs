// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Protocol-qualified native routes. The actual mandatory execution flag keeps
//! an updater replacing the probed image with an older image fail-closed.
use super::*;
use aterm_types::resource_admission::Submission;
use std::{
    ffi::{OsStr, OsString},
    os::unix::{
        ffi::{OsStrExt as _, OsStringExt as _},
        fs::PermissionsExt as _,
    },
    path::Path,
};

#[cfg(all(test, target_os = "macos"))]
#[path = "resource_admission_recipe_signed_tests.rs"]
mod signed_tests;

pub(super) enum Kind {
    #[cfg(test)]
    LifecycleFixture,
    Ay,
    Targo,
    Metadata,
    Resolver,
    Workload,
    BootstrapGraph,
    Preflight,
}
pub(super) struct Probe {
    worker: Option<preparation::Worker<()>>,
    receive: std::sync::mpsc::Receiver<io::Result<Option<Recipe>>>,
    calls: Option<std::sync::mpsc::Receiver<ProbeCall>>,
}

/// The selected command and its original guards move to the custody actor.
/// Only that actor may run it and return complete stdout after native closure.
pub(super) struct ProbeCall {
    pub(super) prepared: preflight::Prepared,
    pub(super) reply: std::sync::mpsc::SyncSender<io::Result<Vec<u8>>>,
}

pub(super) type ExecuteProbe<'a> = dyn Fn(spawnfd::Command, Instant) -> io::Result<Vec<u8>> + 'a;

fn request_native(
    send: &std::sync::mpsc::SyncSender<ProbeCall>,
    command: spawnfd::Command,
    deadline: Instant,
) -> io::Result<Vec<u8>> {
    if Instant::now() >= deadline {
        return Err(io::ErrorKind::TimedOut.into());
    }
    let (reply, receive) = std::sync::mpsc::sync_channel(1);
    send.try_send(ProbeCall {
        prepared: preflight::Prepared::new(command, deadline)?,
        reply,
    })
    .map_err(|_| refused("native recipe preparation owner unavailable"))?;
    crate::actor::wake();
    let remaining = deadline.saturating_duration_since(Instant::now());
    let reply = receive
        .recv_timeout(remaining)
        .map_err(|error| match error {
            std::sync::mpsc::RecvTimeoutError::Timeout => io::Error::from(io::ErrorKind::TimedOut),
            std::sync::mpsc::RecvTimeoutError::Disconnected => {
                refused("native recipe preparation lost its owning reply")
            }
        })?;
    if Instant::now() >= deadline {
        return Err(io::ErrorKind::TimedOut.into());
    }
    reply
}

impl Probe {
    pub(super) fn spawn(
        work: impl FnOnce(&ExecuteProbe<'_>) -> io::Result<Option<Recipe>> + Send + 'static,
    ) -> io::Result<Self> {
        let (send, receive) = std::sync::mpsc::sync_channel(1);
        let (request, calls) = std::sync::mpsc::sync_channel(1);
        let worker = std::thread::Builder::new()
            .name("resource-recipe-probe".into())
            .spawn(move || {
                let qualified =
                    work(&|command, deadline| request_native(&request, command, deadline));
                let _ = send.send(qualified);
                crate::actor::wake();
            })?;
        Ok(Self {
            receive,
            worker: Some(preparation::Worker::new(worker)),
            calls: Some(calls),
        })
    }

    #[cfg(test)]
    pub(super) fn paused() -> (Self, std::sync::mpsc::SyncSender<()>) {
        let (release, wait) = std::sync::mpsc::sync_channel(0);
        let (send, receive) = std::sync::mpsc::sync_channel(1);
        let worker = std::thread::spawn(move || {
            let _ = wait.recv_timeout(Duration::from_secs(60));
            let _ = send.send(Err(refused(
                "controlled preparation did not qualify an executable",
            )));
        });
        (
            Self {
                receive,
                worker: Some(preparation::Worker::new(worker)),
                calls: None,
            },
            release,
        )
    }
    pub(super) fn take_call(&self) -> Option<ProbeCall> {
        self.calls.as_ref()?.try_recv().ok()
    }
    pub(super) fn poll(&mut self) -> io::Result<Option<Option<Recipe>>> {
        if !self
            .worker
            .as_ref()
            .is_some_and(|worker| worker.is_finished())
        {
            return Ok(None);
        }
        self.worker
            .take()
            .ok_or_else(|| refused("missing recipe worker"))?
            .join()
            .map_err(|_| refused("recipe worker failed"))?;
        self.receive
            .try_recv()
            .map_err(|_| refused("recipe probe unavailable"))?
            .map(Some)
    }
    pub(super) fn finished(&self) -> bool {
        self.worker
            .as_ref()
            .is_none_or(|worker| worker.is_finished())
    }
}
pub(super) fn metadata_request(request: &Submission) -> bool {
    request.cpu_units == 1
        && request
            .timeout_ms
            .is_some_and(|ms| (1..=600_000).contains(&ms))
        && matches!(request.argv.as_slice(), [_, purpose] if purpose == b"--resource-metadata-v1")
}
pub(super) fn metadata_purpose(request: &Submission) -> bool {
    request
        .argv
        .get(1)
        .is_some_and(|word| word == b"--resource-metadata-v1")
}
pub(super) struct Recipe {
    pub(super) kind: Kind,
    pub(super) program: PathBuf,
    pub(super) arguments: Vec<OsString>,
    pub(super) environment: Vec<(OsString, OsString)>,
    pub(super) cwd: PathBuf,
    pub(super) width: u32,
    pub(super) metadata_sdk: Option<std::sync::Arc<sdk::Keeper>>,
    pub(super) compiler: Option<compiler::Selection>,
    pub(super) ay: Option<std::sync::Arc<ay::Selection>>,
}

/// The selected workload and its final capability command are distinct.
/// A candidate retains its original image/GC selection, but cannot launch
/// the requested workload until the exact query result is checked.
enum PreparedRecipe {
    Direct(Recipe),
    Capability(CapabilityCandidate),
}

struct CapabilityCandidate(Recipe);

impl CapabilityCandidate {
    fn command(&self, request: &Submission) -> io::Result<spawnfd::Command> {
        let recipe = &self.0;
        let mut probe = spawnfd::command(&recipe.program);
        probe
            .arg0(OsStr::from_bytes(&request.argv[0]))
            .arg("--resource-capabilities")
            .env_clear()
            .envs(recipe.environment.iter().map(|(key, value)| (key, value)))
            .current_dir(&recipe.cwd)
            .stderr(spawnfd::Stdio::null());
        configure_capability_backing(&mut probe, recipe.ay.as_deref(), recipe.compiler.as_ref())?;
        Ok(probe)
    }

    fn qualify(self, capability: &aterm_json::Value) -> io::Result<Recipe> {
        let mut recipe = self.0;
        let words = recipe
            .arguments
            .iter()
            .map(|word| word.to_str())
            .collect::<Vec<_>>();
        let words = words
            .strip_prefix(&[Some("--resource-worker-v3")])
            .unwrap_or(&words);
        let words = words
            .strip_prefix(&[Some("--resource-subgroups-v3")])
            .unwrap_or(words);
        let supported = match recipe.kind {
            Kind::Ay => ay(capability, words),
            Kind::Targo => targo(capability, words),
            _ => false,
        };
        if !supported {
            return Err(refused("command has no qualified resource recipe"));
        }
        if recipe
            .arguments
            .first()
            .is_none_or(|arg| arg != "--resource-worker-v3")
        {
            recipe.arguments.insert(0, "--resource-worker-v3".into());
        }
        if recipe
            .arguments
            .get(1)
            .is_none_or(|arg| arg != "--resource-subgroups-v3")
        {
            recipe.arguments.insert(1, "--resource-subgroups-v3".into());
        }
        Ok(recipe)
    }
}

impl Recipe {
    pub(super) fn probe_in(
        request: &Submission,
        physical: u32,
        layout: Option<atpkg::store::Layout>,
    ) -> io::Result<Probe> {
        // This copy is private input for recipe selection, never a peer or
        // admission credential. Only the queue head may own an active probe.
        let copied = Submission {
            binding: request.binding.clone(),
            peer_pid: request.peer_pid,
            peer_birth: request.peer_birth,
            argv: request.argv.clone(),
            cwd: request.cwd.clone(),
            env: request.env.clone(),
            cpu_units: request.cpu_units,
            memory_bytes: request.memory_bytes,
            label: request.label.clone(),
            timeout_ms: request.timeout_ms,
        };
        Probe::spawn(move |execute| {
            let layout = layout.or_else(atpkg::store::resolve_configured);
            Self::qualify_in_using(&copied, physical, layout.as_ref(), execute)
        })
    }
    #[cfg(test)]
    pub(super) fn qualify(request: &Submission, physical: u32) -> io::Result<Option<Self>> {
        Self::qualify_in(
            request,
            physical,
            atpkg::store::resolve_configured().as_ref(),
        )
    }
    /// The layout is trusted owner configuration, never a request/environment
    /// authority. Private native qualification supplies its own manager store.
    #[cfg(test)]
    pub(super) fn qualify_in(
        request: &Submission,
        physical: u32,
        layout: Option<&atpkg::store::Layout>,
    ) -> io::Result<Option<Self>> {
        Self::qualify_in_using(request, physical, layout, &fixture_probe)
    }

    fn qualify_in_using(
        request: &Submission,
        physical: u32,
        layout: Option<&atpkg::store::Layout>,
        execute: &ExecuteProbe<'_>,
    ) -> io::Result<Option<Self>> {
        Self::qualify_using(
            request,
            physical,
            layout,
            &|layout| {
                atpkg::prerequisite::selected_in(
                    layout,
                    atpkg::prerequisite::Prerequisite::Ay,
                    "retained AY resource root",
                    Duration::ZERO,
                )
                .map_err(|error| refused(&error))
            },
            execute,
        )
    }

    // The private selector returns an actual signed payload owner, never a
    // boolean/path declaration. Production always uses the configured anchor;
    // qualification can use a separately signed private registry.
    #[cfg(all(test, target_os = "macos"))]
    fn qualify_with_ay_selection(
        request: &Submission,
        physical: u32,
        layout: Option<&atpkg::store::Layout>,
        select_ay: &dyn Fn(&atpkg::store::Layout) -> io::Result<atpkg::prerequisite::HeldTool>,
    ) -> io::Result<Option<Self>> {
        Self::qualify_using(request, physical, layout, select_ay, &fixture_probe)
    }

    fn qualify_using(
        request: &Submission,
        physical: u32,
        layout: Option<&atpkg::store::Layout>,
        select_ay: &dyn Fn(&atpkg::store::Layout) -> io::Result<atpkg::prerequisite::HeldTool>,
        execute: &ExecuteProbe<'_>,
    ) -> io::Result<Option<Self>> {
        match Self::prepare_with_ay_selection(request, physical, layout, select_ay, execute)? {
            None => Ok(None),
            Some(PreparedRecipe::Direct(recipe)) => Ok(Some(recipe)),
            Some(PreparedRecipe::Capability(candidate)) => {
                let bytes = execute(
                    candidate.command(request)?,
                    Instant::now() + Duration::from_secs(60),
                )?;
                let capability = aterm_json::from_slice(&bytes)
                    .map_err(|_| refused("command has no qualified resource recipe"))?;
                candidate.qualify(&capability).map(Some)
            }
        }
    }

    fn prepare_with_ay_selection(
        request: &Submission,
        physical: u32,
        layout: Option<&atpkg::store::Layout>,
        select_ay: &dyn Fn(&atpkg::store::Layout) -> io::Result<atpkg::prerequisite::HeldTool>,
        execute: &ExecuteProbe<'_>,
    ) -> io::Result<Option<PreparedRecipe>> {
        // This applies to every native recipe, including AY, before resolving
        // or probing a candidate. Its pathname does not qualify injected code.
        compiler::no_loader_overrides(&request.env)?;
        if metadata_purpose(request) && !metadata_request(request) {
            return Err(refused(
                "metadata service requires the closed fixed-one request",
            ));
        }
        if resolver_root::purpose(request) && !resolver_root::closed_request(request) {
            return Err(refused(
                "resolver service requires the closed fixed-one request",
            ));
        }
        if workload::purpose(request) && !workload::closed_request(request) {
            return Err(refused(
                "workload service requires its closed cooperative request",
            ));
        }
        if bootstrap_graph::purpose(request) && !bootstrap_graph::closed_request(request) {
            return Err(refused(
                "Bootstrap graph requires its closed fixed Root request",
            ));
        }
        if request.cpu_units > physical {
            return Err(refused("requested CPU width exceeds this host"));
        }
        let mut program = resolve(request)?;
        let requested = Path::new(OsStr::from_bytes(&request.argv[0]))
            .file_name()
            .and_then(|name| name.to_str());
        let mut arguments = request.argv[1..]
            .iter()
            .map(|arg| OsString::from_vec(arg.clone()))
            .collect::<Vec<_>>();
        let mut environment = request
            .env
            .iter()
            .filter(|(name, value)| !stale_fd_var(name, value))
            .map(|(name, value)| {
                (
                    OsString::from_vec(name.clone()),
                    OsString::from_vec(value.clone()),
                )
            })
            .collect::<Vec<_>>();
        environment
            .retain(|(key, _)| key != "ATERM_SESSION_ID" && key != "ATERM_PARENT_SESSION_ID");
        for key in ["ATERM_SESSION_ID", "ATERM_PARENT_SESSION_ID"] {
            environment.push((key.into(), request.binding.session.clone().into()));
        }
        let cwd = PathBuf::from(OsString::from_vec(request.cwd.clone()));
        // Never execute an arbitrary argv merely because its basename claims
        // to be a native capability query. Partial overlap requires the actual
        // manager-selected complete artifact, not PATH or stdout as authority.
        let managed = requested
            .filter(|name| matches!(*name, "ay" | "targo"))
            .and_then(|name| managed_artifact_in(layout?, name, &program));
        if metadata_request(request)
            || resolver_root::closed_request(request)
            || workload::closed_request(request)
            || bootstrap_graph::closed_request(request)
        {
            let selected =
                managed.ok_or_else(|| refused("managed metadata service unavailable"))?;
            if requested != Some("targo") {
                return Err(refused(
                    "metadata service is not the selected managed Targo",
                ));
            }
            let resolver = resolver_root::closed_request(request);
            let workload = workload::closed_request(request);
            let graph = bootstrap_graph::closed_request(request);
            let mut environment =
                service_environment(request, &selected, resolver || workload || graph)?;
            if graph {
                // The graph query profile binds the COMPLETE original six
                // semantic entries. No PATH/config/loader/session input is
                // silently stripped to manufacture equality with a query.
                if request.env.len() != 6
                    || request
                        .env
                        .iter()
                        .map(|(name, _)| name)
                        .collect::<std::collections::BTreeSet<_>>()
                        .len()
                        != 6
                    || request.env.iter().any(|(name, _)| {
                        !matches!(
                            name.as_slice(),
                            b"LANG"
                                | b"LC_ALL"
                                | b"CARGO"
                                | b"RUSTC"
                                | b"CARGO_HOME"
                                | b"CARGO_CACHE_RUSTC_INFO"
                        )
                    })
                {
                    return Err(refused(
                        "Bootstrap Root requires its complete closed six-entry environment",
                    ));
                }
                environment = request
                    .env
                    .iter()
                    .map(|(key, value)| {
                        (
                            OsString::from_vec(key.clone()),
                            OsString::from_vec(value.clone()),
                        )
                    })
                    .collect();
            }
            // No preparatory subprocess: actual ARM/BIND/root ownership must
            // exist before ANY selected service code runs. Live readiness is
            // negotiated by that same original root before member admission.
            let sdk = sdk::Keeper::capture(
                layout.ok_or_else(|| refused("managed service layout unavailable"))?,
                &selected.target,
            )?;
            if resolver {
                sdk.require_resolver_protocol(native_inputs::deadline(Instant::now()))?;
            }
            if workload {
                sdk.require_workload_protocol(native_inputs::deadline(Instant::now()))?;
                // Still in the paid Preparing worker, before any Root GO or
                // project/configuration. Capture the compiler/runtime once;
                // Ready and each graph member reuse its original held inputs.
                sdk.capture_workload_compiler(native_inputs::deadline(Instant::now()))?;
            }
            if graph {
                let deadline = native_inputs::deadline(Instant::now());
                sdk.original_inputs()
                    .require_bootstrap_graph_protocol_marker(
                    &trust_resource_controller::bootstrap_graph::BOOTSTRAP_GRAPH_PROTOCOL_MARKER,
                    deadline,
                )?;
                // SAME paid preparation and original Keeper; no path-selected
                // SDK/compiler or late-capture fallback is permitted at query.
                sdk.capture_bootstrap_query_inputs(deadline)?;
                sdk.capture_bootstrap_object_input(deadline)?;
                arguments.splice(
                    0..0,
                    [
                        "--resource-worker-v3".into(),
                        "--resource-subgroups-v3".into(),
                    ],
                );
            }
            for (key, value) in sdk.runtime_environment()? {
                if graph {
                    return Err(refused(
                        "Bootstrap graph query profile does not support a loader override",
                    ));
                }
                environment.retain(|(old, _)| old != &key);
                environment.push((key, value));
            }
            return Ok(Some(PreparedRecipe::Direct(Self {
                kind: if graph {
                    Kind::BootstrapGraph
                } else if workload {
                    Kind::Workload
                } else if resolver {
                    Kind::Resolver
                } else {
                    Kind::Metadata
                },
                program: selected.target,
                arguments,
                environment,
                cwd,
                width: if workload || graph {
                    request.cpu_units
                } else {
                    1
                },
                compiler: workload.then(|| compiler::Selection::HeldSdk(sdk.clone())),
                metadata_sdk: Some(sdk),
                ay: None,
            })));
        }
        let ay_selection = if requested == Some("ay") {
            let selected =
                select_ay(layout.ok_or_else(|| refused("signed AY store unavailable"))?)?;
            if managed
                .as_ref()
                .is_none_or(|entry| entry.target != selected.physical_path())
            {
                return Err(refused(
                    "signed AY selection differs from the requested artifact",
                ));
            }
            let selected = ay::Selection::capture(selected)?;
            program = selected.physical_path().to_path_buf();
            Some(std::sync::Arc::new(selected))
        } else {
            None
        };
        let compiler = if requested == Some("targo") {
            let Some(selected) = compiler::select(request, &program, execute)? else {
                return Ok(None);
            };
            program = selected.targo();
            Some(selected)
        } else {
            None
        };
        if let Some(selected) = &compiler {
            compiler::paired_environment(selected.execution(), &mut environment)?;
        }
        let managed = if compiler.is_some() {
            Some(atpkg::ops::Managed {
                target: program.clone(),
                env: atpkg::shim_env::ShimEnv::NONE,
            })
        } else if let Some(selected) = &ay_selection {
            Some(atpkg::ops::Managed {
                target: selected.physical_path().to_path_buf(),
                env: selected.environment().clone(),
            })
        } else {
            // Keep the selection from this operation's trusted layout. A
            // second global lookup could select another prefix mid-operation.
            managed
        };
        if let Some(selected) = &ay_selection {
            for (key, value) in selected.environment().entries() {
                environment.retain(|(existing, _)| existing != key.as_str());
                environment.push((key.as_str().into(), value.as_str().into()));
            }
            for (key, value) in selected.runtime_environment()? {
                environment.retain(|(existing, _)| existing != &key);
                environment.push((key, value));
            }
        }
        // Declaring a budget does not enforce it on arbitrary foreign code.
        // CPU exclusivity/boot-retained closure alone supplies no memory
        // bound, so only actual maintained enforcement recipes may execute.
        let kind = match requested {
            Some("ay") if ay_selection.is_some() => Kind::Ay,
            Some("targo") if compiler.is_some() => Kind::Targo,
            _ => return Err(refused("command has no qualified resource recipe")),
        };
        let managed = managed.ok_or_else(|| refused("qualified artifact unavailable"))?;
        program = managed.target;
        for (key, value) in managed.env.entries() {
            environment.retain(|(existing, _)| existing != key.as_str());
            environment.push((key.as_str().into(), value.as_str().into()));
        }
        Ok(Some(PreparedRecipe::Capability(CapabilityCandidate(
            Self {
                kind,
                program,
                arguments,
                environment,
                cwd,
                width: request.cpu_units,
                metadata_sdk: None,
                compiler,
                ay: ay_selection,
            },
        ))))
    }
    pub(super) fn command(&self, request: &Submission) -> spawnfd::Command {
        let mut command = spawnfd::command(&self.program);
        command
            .arg0(OsStr::from_bytes(&request.argv[0]))
            .args(&self.arguments)
            .env_clear()
            .envs(self.environment.iter().map(|(key, value)| (key, value)))
            .current_dir(&self.cwd)
            .setsid(matches!(self.kind, Kind::Metadata))
            .start_suspended(true);
        command
    }

    pub(super) fn configure_ay(&self, command: &mut spawnfd::Command) -> io::Result<()> {
        if matches!(self.kind, Kind::Ay) && self.ay.is_none() {
            return Err(refused("AY Root has no retained signed selection"));
        }
        if let Some(ay) = &self.ay {
            ay.configure(command)?;
        }
        Ok(())
    }
}

fn configure_capability_backing(
    probe: &mut spawnfd::Command,
    ay: Option<&ay::Selection>,
    compiler: Option<&compiler::Selection>,
) -> io::Result<()> {
    // The original artifact owner must survive in the actual query child,
    // independently of this selection worker. Neither branch creates a lease.
    match (ay, compiler) {
        (Some(selected), None) => selected.configure(probe),
        (None, Some(selected)) => sdk::root(Some(selected), probe).map(drop),
        _ => Err(refused(
            "capability probe needs exactly one retained artifact",
        )),
    }
}

#[cfg(test)]
fn fixture_probe(mut command: spawnfd::Command, deadline: Instant) -> io::Result<Vec<u8>> {
    let limit = preflight::output_limit(&command)?;
    let output =
        aterm_uds::deadline::output_until_complete_stdout_with_limit(&mut command, deadline, limit)
            .map_err(|error| io::Error::other(error.to_string()))?;
    if !output.status.success() {
        return Err(refused("controlled capability probe failed"));
    }
    Ok(output.stdout)
}

#[cfg(all(test, target_os = "macos"))]
fn read_capability(probe: &mut spawnfd::Command, started: Instant) -> Option<aterm_json::Value> {
    let deadline = started + Duration::from_secs(60);
    let output =
        aterm_uds::deadline::output_until_complete_stdout_with_limit(probe, deadline, 8192).ok()?;
    if !output.status.success() {
        return None;
    }
    aterm_json::from_slice(&output.stdout).ok()
}

fn service_environment(
    request: &Submission,
    selected: &atpkg::ops::Managed,
    resolver: bool,
) -> io::Result<Vec<(OsString, OsString)>> {
    let resolver_keys: &[&[u8]] = &[b"CARGO_HOME", b"CARGO", b"RUSTC", b"CARGO_CACHE_RUSTC_INFO"];
    let compiler = selected
        .target
        .parent()
        .ok_or_else(|| refused("SDK bin missing"))?
        .join("trustc");
    if resolver
        && resolver_keys
            .iter()
            .any(|key| !request.env.iter().any(|(name, _)| name == key))
    {
        return Err(refused(
            "resolver root requires explicit request-bound Cargo environment",
        ));
    }
    for (key, value) in &request.env {
        let resolver_value = if resolver {
            match key.as_slice() {
                b"CARGO" => Some(value == selected.target.as_os_str().as_bytes()),
                b"RUSTC" => Some(value == compiler.as_os_str().as_bytes()),
                b"CARGO_CACHE_RUSTC_INFO" => Some(value == b"0"),
                b"CARGO_HOME" => Some(
                    value.len() > 1
                        && value.len() <= 4096
                        && value.starts_with(b"/")
                        && !value.contains(&0)
                        && value[1..]
                            .split(|byte| *byte == b'/')
                            .all(|part| !part.is_empty() && part != b"." && part != b".."),
                ),
                _ => None,
            }
        } else {
            None
        };
        if resolver_value == Some(false) {
            return Err(refused(
                "resolver environment differs from the selected SDK/profile",
            ));
        }
        let semantic = matches!(key.as_slice(), b"LANG" | b"LC_ALL") && value == b"C";
        let selection = key == b"PATH";
        let owned = selected
            .env
            .entries()
            .iter()
            .any(|(name, expected)| key == name.as_bytes() && value == expected.as_bytes());
        if !semantic && !selection && !owned && resolver_value != Some(true) {
            return Err(refused(
                "metadata service environment is outside the closed native profile",
            ));
        }
    }
    let mut environment: Vec<(OsString, OsString)> = vec![
        ("LANG".into(), "C".into()),
        ("LC_ALL".into(), "C".into()),
        (
            "ATERM_SESSION_ID".into(),
            request.binding.session.clone().into(),
        ),
        (
            "ATERM_PARENT_SESSION_ID".into(),
            request.binding.session.clone().into(),
        ),
    ];
    for (key, value) in selected.env.entries() {
        environment.retain(|(old, _)| old != key.as_str());
        environment.push((key.as_str().into(), value.as_str().into()));
    }
    if resolver {
        for (key, value) in &request.env {
            if resolver_keys.contains(&key.as_slice()) {
                environment.retain(|(old, _)| old.as_bytes() != key);
                environment.push((
                    OsString::from_vec(key.clone()),
                    OsString::from_vec(value.clone()),
                ));
            }
        }
    }
    Ok(environment)
}

fn ay(value: &aterm_json::Value, words: &[Option<&str>]) -> bool {
    let command = match words {
        [Some("bench"), Some(command), ..] => format!("bench {command}"),
        _ => return false,
    };
    value["version"].as_u64() == Some(3)
        && value["native_campaign"].as_bool() == Some(true)
        && value["admission"].as_str() == Some("resource-inspect-v3")
        && value["worker_argument"].as_str() == Some("--resource-worker-v3")
        && value["subgroup_custody"].as_str() == Some("host-owned-v3")
        && value["subgroup_argument"].as_str() == Some("--resource-subgroups-v3")
        && ["bench run", "bench chc-gate", "bench sat-delta"].contains(&command.as_str())
        && value["commands"].as_array().is_some_and(|commands| {
            commands
                .iter()
                .any(|value| value.as_str() == Some(command.as_str()))
        })
}
fn targo(value: &aterm_json::Value, words: &[Option<&str>]) -> bool {
    matches!(words, [Some("--unverified"), Some("build" | "check"), ..])
        && value["version"].as_u64() == Some(3)
        && value["cpu_workers"].as_bool() == Some(true)
        && value["memory_custody"].as_str() == Some("resource-inspect-v3")
        && value["worker_argument"].as_str() == Some("--resource-worker-v3")
        && value["subgroup_custody"].as_str() == Some("host-owned-v3")
        && value["subgroup_argument"].as_str() == Some("--resource-subgroups-v3")
}
pub(super) fn stale_fd_var(name: &[u8], value: &[u8]) -> bool {
    matches!(
        name,
        b"TRUST_CPU_HANDOFF"
            | b"ATERM_SDK_LEASE_V3"
            | b"ATERM_RESOURCE_INSPECTOR"
            | b"ATERM_RESOURCE_JOB_V3"
            | b"TRUST_JOBSERVER_HANDOFF"
            | b"CARGO_JOBSERVER_HANDOFF"
            | b"BUILD_SLOT_FENCE_FDS"
            | b"BUILD_SLOT_CPU_LEASE_FD"
            | b"BUILD_SLOT_CPU_PROTOCOL"
            | b"BUILD_SLOT_HELD"
    ) || (matches!(name, b"CARGO_MAKEFLAGS" | b"MAKEFLAGS" | b"MFLAGS")
        && value
            .windows(b"--jobserver-".len())
            .any(|word| word == b"--jobserver-"))
}
pub(super) fn resolve(request: &Submission) -> io::Result<PathBuf> {
    let word = OsStr::from_bytes(&request.argv[0]);
    let cwd = Path::new(OsStr::from_bytes(&request.cwd));
    let mut candidates = Vec::new();
    if request.argv[0].contains(&b'/') {
        candidates.push(if Path::new(word).is_absolute() {
            PathBuf::from(word)
        } else {
            cwd.join(word)
        });
    } else {
        let path = request
            .env
            .iter()
            .find(|(key, _)| key == b"PATH")
            .map(|(_, value)| OsStr::from_bytes(value))
            .unwrap_or_else(|| OsStr::new("/usr/bin:/bin"));
        for directory in std::env::split_paths(path) {
            candidates.push(if directory.is_absolute() {
                directory.join(word)
            } else {
                cwd.join(directory).join(word)
            });
        }
    }
    for path in candidates {
        if let Ok(metadata) = std::fs::metadata(&path)
            && metadata.is_file()
            && metadata.permissions().mode() & 0o111 != 0
        {
            return path.canonicalize();
        }
    }
    Err(io::Error::new(
        io::ErrorKind::NotFound,
        "resource command executable unavailable",
    ))
}

fn managed_artifact_in(
    layout: &atpkg::store::Layout,
    name: &str,
    selected: &Path,
) -> Option<atpkg::ops::Managed> {
    let managed = atpkg::ops::managed(layout, name)?;
    let target = managed.target.canonicalize().ok()?;
    let (program, build) = atpkg::ops::store_build_of(&layout.prefix, &target)?;
    if !atpkg::store::build_is_complete(&layout.build_dir(&program, build)) {
        return None;
    }
    let shim = layout
        .shim(&atpkg::store::ToolName::new(name)?)
        .canonicalize()
        .ok()?;
    if selected != shim && selected != target {
        return None;
    }
    Some(atpkg::ops::Managed {
        target,
        env: managed.env,
    })
}

#[cfg(test)]
#[path = "resource_admission_recipe_tests.rs"]
mod tests;
