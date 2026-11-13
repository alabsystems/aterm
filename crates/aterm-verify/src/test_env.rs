// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! One declared environment for dev and release test children, never verifier admission.
//! A nonempty release history catches tests that assume a Mac has never cut a release.

use crate::{Ctx, exec::Cmd};
use std::ffi::{OsStr, OsString};
use std::path::PathBuf;

fn home(ctx: &Ctx) -> PathBuf {
    ctx.scratch.join("release-test-home")
}

/// Prepare a disposable home, including a persisted successful release's footprint shape.
/// The deliberately large record also catches assumptions about the warm disk floor.
pub(crate) fn prepare(ctx: &Ctx) -> std::io::Result<()> {
    let state = home(ctx).join(".local/state/aterm-release");
    std::fs::create_dir_all(&state)?;
    let mut history = aterm_capacity::Footprints::default();
    history.push(aterm_capacity::Footprint {
        peak: 64 << 30,
        grew: 64 << 30,
        at_ms: 1,
        dirs: None,
        scope: atpkg::admit_disk::scope_tag("release"),
    });
    history.save(&state.join("disk-footprint.json"))
}

/// Whether one of [`Ctx::child_env_add`]'s declared stage values belongs in a release test.
///
/// Each name has one reason to cross this boundary: Cargo's physical artifact root, the selected
/// Trust compiler, the test-width ceiling, the verifier's run record, the quiet temporary root,
/// or the stage's capacity cover. A new context variable stays out until it is named here.
fn context_variable(name: &OsStr) -> bool {
    matches!(
        name.to_str(),
        Some(
            "CARGO_TARGET_DIR"
                | "TRUST_STAGE2_BIN"
                | "RUST_TEST_THREADS"
                | "ATERM_VERIFY_RUN"
                | "TMPDIR"
                | aterm_capacity::COVER_ENV
        )
    )
}

/// Native build inputs have the same explicit identity in Cargo's recording pass and replays.
/// They are tool selections, not Rust compiler overrides, credentials or release capacity.
fn native_input(name: &OsStr, targets: &[String]) -> bool {
    let Some(name) = name.to_str() else {
        return false;
    };
    if matches!(
        name,
        "SDKROOT"
            | "DEVELOPER_DIR"
            | "LIBCLANG_PATH"
            | "MACOSX_DEPLOYMENT_TARGET"
            | "CRATE_CC_NO_DEFAULTS"
    ) {
        return true;
    }
    let bare = name
        .strip_prefix("HOST_")
        .or_else(|| name.strip_prefix("TARGET_"))
        .unwrap_or(name);
    [
        "CC", "CXX", "AR", "RANLIB", "CFLAGS", "CXXFLAGS", "CPPFLAGS", "LDFLAGS", "ARFLAGS",
    ]
    .iter()
    .any(|key| {
        bare == *key
            || bare
                .strip_prefix(key)
                .and_then(|s| s.strip_prefix('_'))
                .is_some_and(|target| {
                    // A lexical triple is not authority: only the maintained matrix's selected
                    // targets (or this native host) may contribute a target-specific input.
                    targets
                        .iter()
                        .any(|triple| target == triple || target == triple.replace('-', "_"))
                })
    })
}

fn native_targets(ctx: Option<&Ctx>) -> Vec<String> {
    let host = aterm_toolchain::rustup::NATIVE_TRIPLE;
    let mut targets = Vec::new();
    if !host.is_empty() {
        targets.push(host.to_string());
    }
    let cells = match ctx {
        // Capture once before CLI selection, then restrict again at the command boundary.
        None => aterm_forge::resolve::default_cells(),
        Some(ctx) if ctx.checks.contains(&crate::Check::Cells) => {
            if ctx.cells_pulled {
                crate::cells::pulled_cells(host)
            } else {
                // An invalid cell name gets no target-specific input, and the normal cell
                // validator still reports the selection error.
                crate::cells::selected(&ctx.cells, host).unwrap_or_default()
            }
        }
        Some(_) => Vec::new(),
    };
    targets.extend(cells.into_iter().map(|cell| cell.triple));
    targets.sort();
    targets.dedup();
    targets
}

pub(crate) fn native_inputs(
    variables: impl IntoIterator<Item = (OsString, OsString)>,
) -> Vec<(OsString, OsString)> {
    let targets = native_targets(None);
    let mut inputs: Vec<_> = variables
        .into_iter()
        .filter(|(name, _)| native_input(name, &targets))
        .collect();
    inputs.sort_by(|a, b| a.0.cmp(&b.0));
    inputs
}

/// Windows process launch and temporary-file lookup need these OS locations after `env_clear`.
/// They are named individually; account data and every other caller variable stay out.
#[cfg(windows)]
fn platform_variables(_ctx: &Ctx, mut cmd: Cmd) -> Cmd {
    for key in ["SystemRoot", "ComSpec", "PATHEXT", "TEMP", "TMP"] {
        if let Some(value) = std::env::var_os(key) {
            cmd = cmd.env(key, value);
        }
    }
    cmd
}

/// Linux tools need the login runtime location even under the declared test
/// environment. Test binaries redirect this canonical location into their own
/// private scratch; another caller-selected runtime must not cross this boundary.
#[cfg(target_os = "linux")]
fn platform_variables(ctx: &Ctx, mut cmd: Cmd) -> Cmd {
    if let Some(dir) = ctx.env.xdg_runtime_dir.as_deref()
        && aterm_uds::test_binary::is_login_runtime_dir(dir.as_os_str())
        && private_runtime(dir, aterm_uds::peer::our_uid())
    {
        cmd = cmd.env("XDG_RUNTIME_DIR", dir);
    }
    cmd
}

#[cfg(target_os = "linux")]
fn private_runtime(dir: &std::path::Path, owner: u32) -> bool {
    use std::os::unix::fs::MetadataExt as _;
    dir.is_absolute()
        && std::fs::symlink_metadata(dir)
            .is_ok_and(|md| md.is_dir() && md.uid() == owner && md.mode() & 0o7777 == 0o700)
        && std::fs::canonicalize(dir).is_ok_and(|physical| physical == dir)
}

#[cfg(not(any(windows, target_os = "linux")))]
fn platform_variables(_ctx: &Ctx, cmd: Cmd) -> Cmd {
    cmd
}

/// Windows directory discovery reads `USERPROFILE`, while Unix discovery reads `HOME`.
/// Both names point at the same disposable release-test home; the owner's profile stays out.
#[cfg(windows)]
fn home_variables(cmd: Cmd, home: &std::path::Path) -> Cmd {
    cmd.env("HOME", home).env("USERPROFILE", home)
}

#[cfg(not(windows))]
fn home_variables(cmd: Cmd, home: &std::path::Path) -> Cmd {
    cmd.env("HOME", home)
}

/// Turn an inherited stage command into the whole declared release-test environment.
///
/// PATH is the verifier's selected tool path. Context values are restricted by
/// [`context_variable`]. Captured native tool selections remain explicit inputs after isolation.
/// The build width and its admission marker keep a managed build bounded.
/// Command-local values were explicitly chosen by the stage (checker policy, Cargo layout,
/// doctest driver, test width and subprocess slots), rather than inherited from the cutter.
/// An already-exact command is a replay recorded by Cargo after this boundary; Cargo's runtime
/// metadata, binary paths and loader path are therefore part of that declared record.
fn declared(ctx: &Ctx, mut cmd: Cmd) -> Cmd {
    if cmd.exact_env {
        return cmd;
    }
    let local = std::mem::take(&mut cmd.envs);
    cmd.exact_env = true;
    cmd = platform_variables(ctx, cmd.env("PATH", &ctx.path_env));
    for (key, value) in &ctx.child_env_add {
        if context_variable(key) {
            cmd = cmd.env(key, value);
        }
    }
    if let Some(jobs) = &ctx.env.cargo_build_jobs {
        cmd = cmd.env("CARGO_BUILD_JOBS", jobs);
    }
    if let Some(held) = &ctx.env.build_slot_held {
        cmd = cmd.env("BUILD_SLOT_HELD", held);
    }
    let targets = native_targets(Some(ctx));
    for (key, value) in &ctx.env.native_toolchain {
        if native_input(key, &targets) {
            cmd = cmd.env(key, value);
        }
    }
    for (key, value) in local {
        cmd = cmd.env(key, value);
    }
    cmd
}

/// Test policy shared by dev and release, Cargo's recording pass, replays and doctests.
/// Cache locations stay those selected before HOME is replaced; no credentials are copied.
/// Release capacity is authority held by the caller, not a test condition. Do not manufacture
/// it here: a test's compiler can otherwise preempt the Background judgment that launched it.
/// Tests of release admission inject that role in their fixture. Checker execution is separately
/// selected by `stages::with_checkers`: only qualification owes the golden suite.
pub(crate) fn command(ctx: &Ctx, mut cmd: Cmd) -> Cmd {
    cmd.envs
        .retain(|(key, _)| key != aterm_capacity::RELEASE_RUN_ENV);
    let disposable_home = home(ctx);
    let cargo_home = cmd
        .envs
        .iter()
        .find(|(name, _)| name == "CARGO_HOME")
        .map(|(_, value)| value.clone())
        .or_else(|| {
            (!cmd.remove_envs.iter().any(|name| name == "CARGO_HOME")).then(|| {
                ctx.env
                    .cargo_home
                    .clone()
                    .unwrap_or_else(|| ctx.env.home.join(".cargo"))
                    .into_os_string()
            })
        });
    cmd = home_variables(declared(ctx, cmd), &disposable_home)
        .env(
            "ATERM_TEST_TOOL_HOME",
            ctx.env
                .tool_home
                .clone()
                .unwrap_or_else(|| ctx.env.home.clone()),
        )
        .env(
            "RUSTUP_HOME",
            aterm_toolchain::rustup::rustup_home(ctx.env.rustup_home.as_deref(), &ctx.env.home),
        );
    if let Some(cargo_home) = cargo_home {
        cmd = cmd.env("CARGO_HOME", cargo_home);
    }
    if let Some(prefix) = &ctx.env.test_package_prefix {
        cmd = cmd.env("ATERM_TEST_PACKAGE_PREFIX", prefix);
    }
    if let Some(marker) = &ctx.env.qualify_red_marker {
        cmd = cmd.env("ATERM_QUALIFY_RED_MARKER", marker.as_os_str());
    }
    cmd
}

#[cfg(test)]
#[path = "test_env_tests.rs"]
mod tests;
