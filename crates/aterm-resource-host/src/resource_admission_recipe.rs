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
}
pub(super) struct Probe {
    worker: Option<preparation::Worker<()>>,
    receive: std::sync::mpsc::Receiver<io::Result<Option<Recipe>>>,
}
impl Probe {
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
            },
            release,
        )
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
        let (send, receive) = std::sync::mpsc::sync_channel(1);
        let worker = std::thread::Builder::new()
            .name("resource-recipe-probe".into())
            .spawn(move || {
                let qualified = match layout {
                    Some(layout) => Self::qualify_in(&copied, physical, Some(&layout)),
                    None => Self::qualify(&copied, physical),
                };
                let _ = send.send(qualified);
            })?;
        Ok(Probe {
            receive,
            worker: Some(preparation::Worker::new(worker)),
        })
    }
    pub(super) fn qualify(request: &Submission, physical: u32) -> io::Result<Option<Self>> {
        Self::qualify_in(
            request,
            physical,
            atpkg::store::resolve_configured().as_ref(),
        )
    }
    /// The layout is trusted owner configuration, never a request/environment
    /// authority. Private native qualification supplies its own manager store.
    pub(super) fn qualify_in(
        request: &Submission,
        physical: u32,
        layout: Option<&atpkg::store::Layout>,
    ) -> io::Result<Option<Self>> {
        Self::qualify_with_ay_selection(request, physical, layout, &|layout| {
            atpkg::prerequisite::selected_in(
                layout,
                atpkg::prerequisite::Prerequisite::Ay,
                "retained AY resource root",
                Duration::ZERO,
            )
            .map_err(|error| refused(&error))
        })
    }

    // The private selector returns an actual signed payload owner, never a
    // boolean/path declaration. Production always uses the configured anchor;
    // qualification can use a separately signed private registry.
    fn qualify_with_ay_selection(
        request: &Submission,
        physical: u32,
        layout: Option<&atpkg::store::Layout>,
        select_ay: &dyn Fn(&atpkg::store::Layout) -> io::Result<atpkg::prerequisite::HeldTool>,
    ) -> io::Result<Option<Self>> {
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
        let executable = program
            .file_name()
            .and_then(|name| name.to_str())
            .map(str::to_owned);
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
        let mut kind = None;
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
            return Ok(Some(Self {
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
            }));
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
            let Some(selected) = compiler::select(request, &program)? else {
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
        if managed.is_some()
            && [requested, executable.as_deref()]
                .iter()
                .any(|name| matches!(name, Some("ay" | "targo")))
        {
            let mut probe = spawnfd::command(&program);
            probe
                .arg0(OsStr::from_bytes(&request.argv[0]))
                .arg("--resource-capabilities")
                .env_clear()
                .envs(environment.iter().map(|(key, value)| (key, value)))
                .current_dir(&cwd)
                .stderr(spawnfd::Stdio::null());
            configure_capability_backing(&mut probe, ay_selection.as_deref(), compiler.as_ref())?;
            if let Some(capability) = read_capability(&mut probe, Instant::now()) {
                let words = arguments
                    .iter()
                    .map(|word| word.to_str())
                    .collect::<Vec<_>>();
                let words = if words.first() == Some(&Some("--resource-worker-v3")) {
                    &words[1..]
                } else {
                    &words[..]
                };
                let words = if words.first() == Some(&Some("--resource-subgroups-v3")) {
                    &words[1..]
                } else {
                    words
                };
                if requested == Some("ay") && ay(&capability, words) {
                    kind = Some(Kind::Ay);
                }
                if requested == Some("targo") && targo(&capability, words) {
                    kind = Some(Kind::Targo);
                }
            }
        }
        // Declaring a budget does not enforce it on arbitrary foreign code.
        // CPU exclusivity/boot-retained closure alone supplies no memory
        // bound, so only actual maintained enforcement recipes may execute.
        let kind = kind.ok_or_else(|| refused("command has no qualified resource recipe"))?;
        let managed = managed.ok_or_else(|| refused("qualified artifact unavailable"))?;
        program = managed.target;
        for (key, value) in managed.env.entries() {
            environment.retain(|(existing, _)| existing != key.as_str());
            environment.push((key.as_str().into(), value.as_str().into()));
        }
        let width = request.cpu_units;
        if !matches!(kind, Kind::Metadata)
            && arguments
                .first()
                .is_none_or(|arg| arg != "--resource-worker-v3")
        {
            arguments.insert(0, "--resource-worker-v3".into());
        }
        if !matches!(kind, Kind::Metadata)
            && arguments
                .get(1)
                .is_none_or(|arg| arg != "--resource-subgroups-v3")
        {
            arguments.insert(1, "--resource-subgroups-v3".into());
        }
        Ok(Some(Self {
            kind,
            program,
            arguments,
            environment,
            cwd,
            width,
            metadata_sdk: None,
            compiler,
            ay: ay_selection,
        }))
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
mod tests {
    use super::*;

    #[test]
    #[cfg(target_os = "macos")]
    fn capability_probe_child_keeps_original_sdk_gc_backing() {
        let scratch = aterm_tempfile::tempdir().unwrap();
        let layout = atpkg::store::Layout {
            prefix: scratch.path().canonicalize().unwrap().join("manager"),
        };
        let build = layout.build_dir("trust", 1);
        std::fs::create_dir_all(&build).unwrap();
        let atpkg::lease::Hold::Held(lease) = atpkg::lease::hold_for(
            &layout.prefix,
            &build,
            "capability probe SDK backing",
            Duration::from_secs(30),
        )
        .unwrap() else {
            panic!("private original SDK lease unavailable");
        };
        let selection = compiler::Selection::NativeFixture {
            execution: build,
            build: 1,
            lease: std::sync::Arc::new(lease),
        };
        let mut probe = spawnfd::command("/usr/bin/true");
        probe.env_clear();
        assert!(configure_capability_backing(&mut probe, None, None).is_err());
        assert!(probe.guard_targets().next().is_none());
        configure_capability_backing(&mut probe, None, Some(&selection)).unwrap();
        assert_eq!(probe.guard_targets().count(), 1);
        let held = aterm_uds::deadline::Held::start(&mut probe).unwrap();
        drop(probe);
        drop(selection);
        let subject = atpkg::lease::Subject::build("trust", 1).unwrap();
        assert!(matches!(
            atpkg::lease::reclaim(&layout.prefix, &subject),
            atpkg::lease::Reclaim::Keep(atpkg::lease::Holders::Held(_))
        ));
        let output = held
            .output_until(
                crate::fixture_deadline(Instant::now()),
                Duration::from_secs(2),
            )
            .unwrap();
        assert!(output.status.success());
        assert!(matches!(
            atpkg::lease::reclaim(&layout.prefix, &subject),
            atpkg::lease::Reclaim::Clear(_)
        ));
    }

    #[test]
    #[cfg(target_os = "macos")]
    fn caller_loader_overrides_refuse_before_an_actual_managed_ay_probe() {
        if !aterm_uds::test_binary::isolated_lifecycle(
            "actor::owner::recipe::tests::caller_loader_overrides_refuse_before_an_actual_managed_ay_probe",
        ) {
            return;
        }
        let scratch = aterm_tempfile::tempdir().unwrap();
        let root = scratch.path().canonicalize().unwrap();
        let layout = atpkg::store::Layout {
            prefix: root.join("manager"),
        };
        let build = layout.build_dir("ay", 1);
        std::fs::create_dir_all(build.join("bin")).unwrap();
        std::fs::create_dir_all(layout.bin_dir()).unwrap();
        let program = build.join("bin/ay");
        std::fs::copy(std::env::current_exe().unwrap(), &program).unwrap();
        let runtime_data = build.join("runtime-data");
        std::fs::write(&runtime_data, b"signed original runtime data").unwrap();
        atpkg::store::mark_build_ready(&build).unwrap();
        let name = atpkg::store::ToolName::new("ay").unwrap();
        atpkg::platform::install_shim_to(&layout.shim(&name), &program).unwrap();
        let registry = signed_tests::Registry::new(&build);
        let select = |layout: &atpkg::store::Layout| registry.acquire(layout);
        let marker = root.join("candidate-ran");
        let mut request = Submission {
            binding: Binding {
                session: "s-a".into(),
                attempt: [31; 16],
            },
            peer_pid: std::process::id(),
            peer_birth: 1,
            argv: vec![
                program.as_os_str().as_bytes().to_vec(),
                b"bench".to_vec(),
                b"run".to_vec(),
            ],
            cwd: root.as_os_str().as_bytes().to_vec(),
            env: vec![(
                b"ATERM_TEST_RECIPE_PROBE_MARKER".to_vec(),
                marker.as_os_str().as_bytes().to_vec(),
            )],
            cpu_units: 1,
            memory_bytes: 1024 * 1024 * 1024,
            label: "loader prebirth refusal".into(),
            timeout_ms: Some(60_000),
        };
        // This controlled owner uses its own actual signing anchor. It does
        // not replace production's pinned anchor or grant resource custody.
        let qualified = Recipe::qualify_with_ay_selection(&request, 4, Some(&layout), &select);
        let no_loader_error = qualified.as_ref().err().map(|error| error.to_string());
        assert!(
            marker.exists(),
            "no-loader qualifier must actually reach the candidate probe: {no_loader_error:?}"
        );
        std::fs::remove_file(&marker).unwrap();
        let mut recipe = qualified
            .unwrap()
            .expect("actual signed canary qualifies its private profile");
        let mut command = recipe.command(&request);
        let original = recipe.ay.take();
        assert!(recipe.configure_ay(&mut command).is_err());
        assert!(command.guard_targets().next().is_none());
        recipe.ay = original;
        recipe.configure_ay(&mut command).unwrap();
        let targets = command.guard_targets().collect::<Vec<_>>();
        assert!(recipe.configure_ay(&mut command).is_err());
        assert_eq!(command.guard_targets().collect::<Vec<_>>(), targets);
        let held = aterm_uds::deadline::Held::start(&mut command).unwrap();
        drop(command);
        drop(recipe);
        let subject = atpkg::lease::Subject::build("ay", 1).unwrap();
        assert!(
            matches!(
                atpkg::lease::reclaim(&layout.prefix, &subject),
                atpkg::lease::Reclaim::Keep(_)
            ),
            "the actual stopped child must retain the original lease without Host-side copies"
        );
        // The controlled native test image refuses the production-only argv;
        // require its actual owned exit/drain before checking reclaimability.
        let output = held
            .output_until(
                Instant::now() + Duration::from_secs(60),
                Duration::from_secs(2),
            )
            .unwrap();
        assert!(!output.status.success());
        assert!(matches!(
            atpkg::lease::reclaim(&layout.prefix, &subject),
            atpkg::lease::Reclaim::Clear(_)
        ));
        for key in [b"LD_PRELOAD".as_slice(), b"DYLD_PRINT_LIBRARIES".as_slice()] {
            for value in [b"unqualified".as_slice(), b"".as_slice()] {
                request.env.push((key.to_vec(), value.to_vec()));
                assert!(managed_artifact_in(&layout, "ay", &program).is_some());
                let result = Recipe::qualify_with_ay_selection(&request, 4, Some(&layout), &select);
                request.env.pop();
                assert!(
                    !marker.exists(),
                    "an unqualified loader must not reach candidate code"
                );
                let error = result.err().unwrap();
                assert!(
                    error.to_string().contains("dynamic loader overrides"),
                    "{error}"
                );
            }
        }
        // A complete marker/shim and identical executable cannot substitute
        // for the signed whole payload; refuse changed non-executable data
        // before even the read-only capability subprocess.
        std::fs::write(&runtime_data, b"changed runtime data").unwrap();
        let error = Recipe::qualify_with_ay_selection(&request, 4, Some(&layout), &select)
            .err()
            .expect("changed signed payload must refuse");
        assert!(error.to_string().contains("signed tree"), "{error}");
        assert!(!marker.exists());

        // A signed package is not automatically an accepted native runtime.
        // Prove this candidate really runs and has a valid signed selection,
        // then require the actual qualifier to stop it before execution.
        std::fs::write(
            &program,
            b"#!/bin/sh\nprintf actual-script > \"$ATERM_TEST_RECIPE_PROBE_MARKER\"\n",
        )
        .unwrap();
        atpkg::store::mark_build_ready(&build).unwrap();
        let script_registry = signed_tests::Registry::new(&build);
        let script_select = |layout: &atpkg::store::Layout| script_registry.acquire(layout);
        drop(
            script_select(&layout)
                .expect("non-native candidate has actual signed payload authority"),
        );
        let mut direct = spawnfd::command(&program);
        direct
            .arg("--resource-capabilities")
            .env_clear()
            .env("ATERM_TEST_RECIPE_PROBE_MARKER", &marker);
        let output = aterm_uds::deadline::output_until_complete_stdout_with_limit(
            &mut direct,
            Instant::now() + Duration::from_secs(60),
            8192,
        )
        .unwrap();
        assert!(output.status.success() && marker.exists());
        std::fs::remove_file(&marker).unwrap();
        assert!(
            Recipe::qualify_with_ay_selection(&request, 4, Some(&layout), &script_select).is_err()
        );
        assert!(
            !marker.exists(),
            "signed source must still pass the native runtime profile before probing"
        );
    }

    #[test]
    #[cfg(target_os = "macos")]
    fn an_actual_native_capability_read_survives_scheduling_delay() {
        const BODY: &str = r#"{"version":3,"cpu_workers":true,"memory_custody":"resource-inspect-v3","worker_argument":"--resource-worker-v3","subgroup_custody":"host-owned-v3","subgroup_argument":"--resource-subgroups-v3"}"#;
        let program = aterm_uds::deadline::stand_in(Path::new("/usr/bin/printf"));
        let mut command = spawnfd::command(program);
        command.args(["%s", BODY]);
        let mut capability =
            read_capability(&mut command, Instant::now() - Duration::from_secs(3)).unwrap();
        assert!(targo(&capability, &[Some("--unverified"), Some("build")]));
        capability.as_object_mut().unwrap().insert(
            "subgroup_custody".into(),
            aterm_json::Value::String("foreign".into()),
        );
        assert!(!targo(&capability, &[Some("--unverified"), Some("build")]));
        assert!(read_capability(&mut command, Instant::now() - Duration::from_secs(61)).is_none());
        let program = aterm_uds::deadline::stand_in(Path::new("/usr/bin/printf"));
        let mut malformed = spawnfd::command(program);
        malformed.args(["%s", "{"]);
        assert!(read_capability(&mut malformed, Instant::now()).is_none());
        let program = aterm_uds::deadline::stand_in(Path::new("/bin/sh"));
        let mut failed = spawnfd::command(program);
        failed.args(["-c", "printf '%s' \"$1\"; exit 7", "fixture", BODY]);
        assert!(read_capability(&mut failed, Instant::now()).is_none());
    }
    #[test]
    fn malformed_metadata_purpose_cannot_fall_back_to_a_legacy_preparation_probe() {
        let mut request = Submission {
            binding: Binding {
                session: "s-a".into(),
                attempt: [1; 16],
            },
            peer_pid: std::process::id(),
            peer_birth: 1,
            argv: vec![
                b"/does-not-exist/targo".to_vec(),
                b"--resource-metadata-v1".to_vec(),
            ],
            cwd: b"/".to_vec(),
            env: Vec::new(),
            cpu_units: 2,
            memory_bytes: 1024,
            label: "closed metadata purpose".into(),
            timeout_ms: Some(600_000),
        };
        for extra in [false, true] {
            if extra {
                request.cpu_units = 1;
                request.argv.push(b"unexpected".to_vec());
            }
            let error = Recipe::qualify_in(&request, 2, None)
                .err()
                .expect("closed profile must refuse");
            assert!(
                error.to_string().contains("closed fixed-one request"),
                "refuse BEFORE executable/store selection: {error}"
            );
        }
        request.argv.pop();
        for timeout in [None, Some(600_001), Some(u64::MAX)] {
            request.timeout_ms = timeout;
            assert!(!metadata_request(&request));
            let error = Recipe::qualify_in(&request, 2, None).err().unwrap();
            assert!(error.to_string().contains("closed fixed-one request"));
        }
    }
    #[test]
    fn original_root_discards_ancestor_jobserver_endpoints() {
        assert!(stale_fd_var(
            b"CARGO_JOBSERVER_HANDOFF",
            b"private-parent-ad"
        ));
        for name in [b"MAKEFLAGS".as_slice(), b"CARGO_MAKEFLAGS", b"MFLAGS"] {
            assert!(stale_fd_var(
                name,
                b"-j --jobserver-auth=fifo:/private/ancestor"
            ));
            assert!(!stale_fd_var(name, b"-k"));
        }
        assert!(!stale_fd_var(b"CARGO_BUILD_JOBS", b"3"));
    }
    #[test]
    fn arbitrary_command_budget_is_not_a_qualified_memory_enforcement_recipe() {
        let pid = std::process::id();
        let request = Submission {
            binding: Binding {
                session: "s-a".into(),
                attempt: [1; 16],
            },
            peer_pid: pid,
            peer_birth: aterm_uds::process::read_birth(i32::try_from(pid).unwrap())
                .unwrap()
                .0,
            argv: vec![b"/bin/echo".to_vec(), b"unqualified".to_vec()],
            cwd: std::env::current_dir()
                .unwrap()
                .as_os_str()
                .as_bytes()
                .to_vec(),
            env: Vec::new(),
            cpu_units: 1,
            memory_bytes: 1024 * 1024,
            label: "unqualified".into(),
            timeout_ms: None,
        };
        let error = Recipe::qualify(&request, 2)
            .err()
            .expect("arbitrary argv must refuse");
        assert_eq!(error.kind(), io::ErrorKind::WouldBlock);
    }
}
