// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Project compiler resolution runs only on the recipe worker. No signature
//! hashing, compiler execution or reclaim wait belongs on the custody actor.
use super::*;
use aterm_types::resource_admission::Submission;
use atpkg::project_compiler::{self, Inputs, Options, Selected};
use std::{
    ffi::OsString,
    os::unix::ffi::{OsStrExt as _, OsStringExt as _},
    path::Path,
    sync::Arc,
};

#[derive(Clone)]
pub(super) enum Selection {
    Managed(Arc<Selected>),
    // The distinct workload route selects this original whole manager SDK
    // without executing a preparatory compiler/version subprocess.
    HeldSdk(Arc<sdk::Keeper>),
    // A private, actual native fixture image qualifies only the custom integration binary.
    #[cfg(all(test, target_os = "macos"))]
    NativeFixture {
        execution: PathBuf,
        build: u64,
        lease: Arc<atpkg::lease::Lease>,
    },
}
impl Selection {
    pub(super) fn execution(&self) -> &Path {
        match self {
            Self::Managed(selected) => selected.execution(),
            Self::HeldSdk(selected) => selected.execution(),
            #[cfg(all(test, target_os = "macos"))]
            Self::NativeFixture { execution, .. } => execution,
        }
    }
    pub(super) fn lease_descriptor(&self) -> io::Result<std::os::fd::OwnedFd> {
        match self {
            Self::Managed(selected) => selected.lease_descriptor(),
            Self::HeldSdk(selected) => selected.descriptor(),
            #[cfg(all(test, target_os = "macos"))]
            Self::NativeFixture { lease, .. } => lease.try_clone_descriptor(),
        }
    }
    pub(super) fn check_lease(&self) -> io::Result<()> {
        match self {
            Self::Managed(selected) => selected.check_lease(),
            Self::HeldSdk(selected) => selected.validate(),
            #[cfg(all(test, target_os = "macos"))]
            Self::NativeFixture { lease, .. } => lease.check_current(),
        }
    }
    pub(super) fn lease_path(&self) -> &Path {
        match self {
            Self::Managed(selected) => selected.lease_path(),
            Self::HeldSdk(selected) => selected.lease_path(),
            #[cfg(all(test, target_os = "macos"))]
            Self::NativeFixture { lease, .. } => lease.path(),
        }
    }
    pub(super) fn build(&self) -> u64 {
        match self {
            Self::Managed(selected) => selected.build(),
            Self::HeldSdk(selected) => selected.build(),
            #[cfg(all(test, target_os = "macos"))]
            Self::NativeFixture { build, .. } => *build,
        }
    }
    pub(super) fn targo(&self) -> PathBuf {
        self.execution().join("bin/targo")
    }
}

pub(super) fn select(
    request: &Submission,
    resolved: &Path,
    execute: &recipe::ExecuteProbe<'_>,
) -> io::Result<Option<Selection>> {
    no_loader_overrides(&request.env)?;
    no_cli_overrides(&request.argv[1..])?;
    let deadline = native_inputs::deadline(Instant::now());
    let layout = atpkg::store::resolve_configured()
        .ok_or_else(|| refused("configured compiler store unavailable"))?;
    let cwd = PathBuf::from(OsString::from_vec(request.cwd.clone()));
    let env = |key: &[u8]| {
        request
            .env
            .iter()
            .find(|(name, _)| name == key)
            .map(|(_, value)| OsString::from_vec(value.clone()))
    };
    let cargo_home = match env(b"CARGO_HOME") {
        Some(path) => PathBuf::from(path),
        None => PathBuf::from(env(b"HOME").ok_or_else(|| refused("request has no Cargo home"))?)
            .join(".cargo"),
    };
    let absolute = |path: PathBuf| {
        if path.is_absolute() {
            path
        } else {
            cwd.join(path)
        }
    };
    let overrides = atpkg::toolchain::rustc_movers::RUSTC_VARS
        .iter()
        .copied()
        .chain([
            "TRUST_STAGE2_BIN",
            "ALAB_CARGO",
            "RUSTC_WRAPPER",
            "RUSTC_WORKSPACE_WRAPPER",
            "RUSTDOC",
            "CARGO_BUILD_RUSTDOC",
        ])
        .filter_map(|key| {
            env(key.as_bytes())
                .filter(|v| !v.is_empty())
                .map(|v| (key.into(), v.to_string_lossy().into_owned()))
        })
        .collect();
    let cargo_home = absolute(cargo_home);
    let inputs = Inputs {
        prefix: layout.prefix.clone(),
        repo: cwd,
        path: env(b"PATH").unwrap_or_default(),
        cargo_home,
        overrides,
    };
    // A malformed nearest lock is an explicit refusal, never a global-default fallback.
    let found = atpkg::trust_lock::find(&inputs.repo);
    let lock = found
        .map(|found| found.lock)
        .transpose()
        .map_err(|e| refused(&e))?;
    let selected = match project_compiler::select_with_lease(
        &inputs,
        lock.as_ref(),
        None,
        Options {
            deadline: Some(deadline),
            lease_wait: Duration::ZERO,
        },
        &|build, _| atpkg::cli::signed_tree_root_at(&layout, "trust", build),
        &|program, at, lease| {
            probe(
                program,
                at.unwrap_or(deadline),
                &inputs,
                request,
                lease,
                execute,
            )
        },
    ) {
        Ok(selected) => selected,
        Err(project_compiler::Error::Busy(_)) => return Ok(None),
        Err(error) => return Err(refused(&error.to_string())),
    };
    let shim = layout.shim(
        &atpkg::store::ToolName::new("targo").ok_or_else(|| refused("invalid native tool name"))?,
    );
    if resolved != selected.targo()
        && !(std::fs::symlink_metadata(&shim).is_ok_and(|meta| meta.is_file())
            && shim.canonicalize().ok().as_deref() == Some(resolved)
            && atpkg::ops::managed(&layout, "targo").is_some())
    {
        return Err(refused(
            "request is not its project's selected Targo or managed shim",
        ));
    }
    Ok(Some(Selection::Managed(Arc::new(selected))))
}

fn probe(
    program: &Path,
    deadline: Instant,
    inputs: &Inputs,
    request: &Submission,
    lease: &atpkg::lease::Lease,
    execute: &recipe::ExecuteProbe<'_>,
) -> Result<String, String> {
    let mut command = spawnfd::command(program);
    command
        .arg("-vV")
        .env_clear()
        .envs(
            request
                .env
                .iter()
                .filter(|(key, value)| !recipe::stale_fd_var(key, value))
                .map(|(k, v)| {
                    (
                        std::ffi::OsStr::from_bytes(k),
                        std::ffi::OsStr::from_bytes(v),
                    )
                }),
        )
        .current_dir(&inputs.repo)
        .stderr(spawnfd::Stdio::null());
    let guard = aterm_uds::kept_out(|| lease.try_clone_descriptor())
        .map_err(|error| format!("retain selected compiler version probe: {error}"))?;
    command
        .inherit_guards(vec![guard])
        .map_err(|error| format!("map selected compiler version probe guard: {error}"))?;
    let output =
        execute(command, deadline).map_err(|e| format!("probe selected project compiler: {e}"))?;
    String::from_utf8(output).map_err(|_| "selected compiler version is not UTF-8".into())
}

/// A signed executable does not qualify code introduced by its caller's loader.
pub(super) fn no_loader_overrides(environment: &[(Vec<u8>, Vec<u8>)]) -> io::Result<()> {
    if environment.iter().any(|(key, _)| {
        atpkg::shim_env::NEVER_SET_PREFIXES
            .iter()
            .any(|prefix| key.starts_with(prefix.as_bytes()))
    }) {
        return Err(refused(
            "native resource recipe refuses dynamic loader overrides",
        ));
    }
    Ok(())
}

// The shared disk/environment mover census does not resolve command-line
// configuration or alternate configuration roots. Refuse those before probes.
fn no_cli_overrides(arguments: &[Vec<u8>]) -> io::Result<()> {
    if arguments
        .iter()
        .any(|arg| arg == b"--config" || arg.starts_with(b"--config=") || arg.starts_with(b"-C"))
    {
        return Err(refused(
            "native recipe cannot authenticate command-line compiler configuration",
        ));
    }
    Ok(())
}

/// The selection, not a later global PATH shim, chooses both compiler programs.
pub(super) fn paired_environment(
    execution: &Path,
    environment: &mut Vec<(OsString, OsString)>,
) -> io::Result<()> {
    let bin = execution.join("bin");
    let old = environment
        .iter()
        .find(|(key, _)| key == "PATH")
        .map(|(_, value)| value.clone())
        .unwrap_or_default();
    let path =
        std::env::join_paths(std::iter::once(bin.clone()).chain(std::env::split_paths(&old)))
            .map_err(|error| io::Error::new(io::ErrorKind::InvalidInput, error))?;
    environment.retain(|(key, _)| key != "PATH" && key != "RUSTC" && key != "RUSTDOC");
    environment.extend([
        ("PATH".into(), path),
        ("RUSTC".into(), bin.join("trustc").into_os_string()),
        ("RUSTDOC".into(), bin.join("trustdoc").into_os_string()),
    ]);
    Ok(())
}

#[cfg(test)]
#[path = "resource_admission_compiler_tests.rs"]
mod tests;
