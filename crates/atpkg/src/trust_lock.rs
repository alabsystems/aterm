// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! A TREE'S LOCKED TRUST BUILD IN THIS STORE (docs/DESIGN-fast-build-release-2026-11-05.md §4.7,
//! A14): the lock's format and its reading are `aterm_toolchain::trust_lock`'s; this is where the
//! store's side meets it.
//!
//! - [`standing`]: where a lock's build stands here. A build staged before stages recorded their
//!   tree roots is folded once ([`crate::tree`]) and the fold recorded, so the next look is a read.
//! - [`shim_target`]: what the `targo` shim's hop runs in a locked tree, the build the lock names
//!   in place of the one the shim was laid for, so plain `targo` and verify compile with one
//!   compiler, and an index that flips `current` changes no locked tree's build.
//! - [`shim_answer`]: whether this machine's shim does that, for verify to say when it does not.
//!
//! Every resolution marks the build ([`mark_locked`]), and gc keeps a build marked within
//! [`LOCKED_KEEP`] whether or not it is `current` (`gc.rs`).

use std::path::{Path, PathBuf};

pub use aterm_toolchain::trust_lock::*;

/// What `atpkg __build_admit --lock-capabilities` answers, alone on its line: this atpkg's shim hop
/// runs the build a tree's `trust.lock` names. An atpkg from before locks takes the word for its
/// target and fails to run it, which reads as not honouring them ([`helper_honours_lock`]).
pub const LOCK_CAPABILITY: &str = "aterm-trust-lock-v1";

/// The flag that asks a front for [`LOCK_CAPABILITY`].
pub const LOCK_CAPABILITY_FLAG: &str = "--lock-capabilities";

/// Hang detector for the lease required to fold a complete, unrecorded build.
const FOLD_LEASE_WAIT: std::time::Duration = std::time::Duration::from_secs(60);

/// Where `lock`'s build stands in the store at `prefix` ([`locked_build`]), absent unless the
/// store reads it complete for this slice ([`crate::store::build_is_complete`]). A complete build
/// the store holds with no record of its tree (staged before stages recorded one, or re-staged
/// since) is folded once under a lease, as `aterm pkg verify` folds it, and the fold is recorded,
/// so it stands [`LockedBuild::Present`] or [`LockedBuild::Mismatch`] from then on.
#[must_use]
pub fn standing(prefix: &Path, lock: &TrustLock) -> LockedBuild {
    standing_started(prefix, lock, std::time::Instant::now())
}

fn standing_started(prefix: &Path, lock: &TrustLock, started: std::time::Instant) -> LockedBuild {
    let first = locked_build(prefix, lock);
    if matches!(first, LockedBuild::Present { .. } | LockedBuild::Unrecorded)
        && !crate::store::build_is_complete(&build_dir(prefix, lock.build))
    {
        return LockedBuild::Absent;
    }
    if first != LockedBuild::Unrecorded {
        return first;
    }
    let Some(subject) = crate::lease::Subject::build(PROGRAM, lock.build) else {
        return first;
    };
    let Some(remaining) = FOLD_LEASE_WAIT
        .checked_sub(started.elapsed())
        .filter(|remaining| !remaining.is_zero())
    else {
        return first;
    };
    let Ok(_lease) = crate::lease::take(prefix, &subject, "trust.lock fold", remaining) else {
        return first;
    };
    let dir = build_dir(prefix, lock.build);
    match fold_and_record(&dir) {
        Ok(Some(root)) if root == lock.tree_root => LockedBuild::Present {
            bin: dir.join("bin"),
        },
        Ok(Some(root)) => LockedBuild::Mismatch { recorded: root },
        Ok(None) | Err(_) => first,
    }
}

/// FOLD THE TREE AT `build_dir` AND RECORD ITS ROOT ([`crate::tree`], as `aterm pkg verify` folds
/// it), the record bound to the tree's identity taken before the fold
/// ([`write_tree_root_record_as`]): `Ok(None)` when the tree there changed while it folded (a
/// re-stage), whose fold describes neither tree. The record is best-effort (a store this user
/// cannot write is folded again next time).
///
/// # Errors
/// When the tree does not fold.
pub fn fold_and_record(build_dir: &Path) -> std::io::Result<Option<String>> {
    let before = identity_or_dash(build_dir);
    let root = crate::store::declared_modes(build_dir)
        .and_then(|modes| crate::tree::tree_root_declared(build_dir, &modes))?;
    if identity_or_dash(build_dir) != before {
        return Ok(None);
    }
    let _ = write_tree_root_record_as(build_dir, &before, &root);
    Ok(Some(root))
}

/// THE LOCK A TOOLCHAIN IS DISCOVERED FOR in a tree at or above `start` ([`find`]), made ready for
/// discovery's pure read (`aterm_toolchain::discovery`): a build the store at `prefix` holds
/// unrecorded is folded and recorded ([`standing`]), a build that stands is marked
/// ([`mark_locked`]) so gc keeps it, and one that does not carries why no published install of
/// it serves this machine, when the store's signed manifests say so ([`unpublished_in`]). `None`
/// when the tree has no lock.
#[must_use]
pub fn found_for(start: &Path, prefix: Option<&Path>) -> Option<Found> {
    let mut found = find(start)?;
    if let (Ok(lock), Some(prefix)) = (&found.lock, prefix) {
        if matches!(standing(prefix, lock), LockedBuild::Present { .. }) {
            let _ = mark_locked(prefix, lock.build);
        } else {
            found.unpublished = unpublished_in(prefix, lock);
        }
    }
    Some(found)
}

/// Why no published install of `lock`'s build can satisfy it on this machine's target, under
/// the last index the store at `prefix` verified and the manifests it holds
/// ([`crate::serving::lock_unpublished`]): `None` when one can, or nothing is known.
#[must_use]
pub fn unpublished_in(prefix: &Path, lock: &TrustLock) -> Option<String> {
    let layout = crate::store::Layout {
        prefix: prefix.to_path_buf(),
    };
    let index = crate::cli::cached_index(&layout)?;
    crate::serving::lock_unpublished(
        &layout,
        &index,
        lock.build,
        &lock.tree_root,
        crate::cli::current_triple(),
    )
}

/// The prefix, build and route of a shim's baked target: `<prefix>/store/trust/<build>/bin/<tool>`,
/// or `<prefix>/compat/trust/<build>/bin/<tool>` (an exec root, routed `true`). `None` for any
/// other path, a dev-linked checkout's among them.
fn baked_build(baked: &Path) -> Option<(PathBuf, u64, bool)> {
    let bin = baked.parent()?;
    if bin.file_name()? != "bin" {
        return None;
    }
    let build_dir = bin.parent()?;
    let name = build_dir.file_name()?.to_str()?;
    let build = name.strip_suffix(".noindex").unwrap_or(name).parse().ok()?;
    let program_dir = build_dir.parent()?;
    if program_dir.file_name()? != PROGRAM {
        return None;
    }
    let kind = program_dir.parent()?;
    let routed = match kind.file_name()?.to_str()? {
        aterm_toolchain::layout::STORE_DIR => false,
        aterm_toolchain::layout::COMPAT_DIR => true,
        _ => return None,
    };
    Some((kind.parent()?.to_path_buf(), build, routed))
}

/// WHAT THE `targo` SHIM'S HOP RUNS IN `cwd` for the target `baked` its shim was laid with: `None`
/// to run `baked` as laid (no lock above `cwd`; a target that is not a store build, such as a
/// dev-linked checkout's, which takes the lock's place; or the lock's own build), else the same
/// tool of the build the lock names (through its exec root when the shim was routed through one
/// and that build has one). A locked build the store lacks is installed first by `install`
/// ([`install_named`] in the hop) into the store at the shim's prefix, as rustup installs the
/// toolchain a `rust-toolchain.toml` names, unless the store's signed manifests say no published
/// install of it serves this machine ([`unpublished_in`]), which is refused with the local build
/// that does instead.
///
/// # Errors
/// A lock that does not read, an install that fails or cannot serve this machine, or a build the
/// store does not hold as the lock names it: the build is refused rather than run on another
/// compiler.
pub fn shim_target(
    baked: &Path,
    cwd: &Path,
    install: &mut dyn FnMut(&Path, &TrustLock) -> Result<(), String>,
) -> Result<Option<PathBuf>, String> {
    shim_target_with(baked, cwd, &unpublished_in, install)
}

/// [`shim_target`] with the store's word on whether the lock's build is published for this
/// machine read through `unpublished` (`prefix`, lock).
fn shim_target_with(
    baked: &Path,
    cwd: &Path,
    unpublished: &dyn Fn(&Path, &TrustLock) -> Option<String>,
    install: &mut dyn FnMut(&Path, &TrustLock) -> Result<(), String>,
) -> Result<Option<PathBuf>, String> {
    let Some(mut found) = find(cwd) else {
        return Ok(None);
    };
    let Some((prefix, build, routed)) = baked_build(baked) else {
        return Ok(None);
    };
    let lock = found.lock.as_ref().map_err(|why| {
        format!(
            "{why}: a tree whose trust.lock does not read builds with no compiler; restore it \
             (`git checkout -- {}`)",
            found.path.display()
        )
    })?;
    // Resolve and mark under a lease on the build: a gc pass reclaiming it is waited out (and the
    // build found gone), and one that has yet to judge it reads the mark under its gate.
    let _lease = crate::lease::Subject::build(PROGRAM, lock.build).and_then(|subject| {
        crate::lease::take(
            &prefix,
            &subject,
            "targo for a trust.lock",
            crate::lease::DEFAULT_WAIT,
        )
        .ok()
    });
    let mut stands = standing(&prefix, lock);
    if !matches!(stands, LockedBuild::Present { .. }) {
        found.unpublished = unpublished(&prefix, lock);
    }
    if stands == LockedBuild::Absent && found.unpublished.is_none() {
        install(&prefix, lock).map_err(|why| {
            format!(
                "{} names Trust build {}, which this machine's store does not hold, and its \
                 install failed: {why}",
                found.path.display(),
                lock.build
            )
        })?;
        stands = standing(&prefix, lock);
    }
    let bin = match stands {
        LockedBuild::Present { bin } => bin,
        // The hop reads no TRUST_STAGE2_BIN: only a dev link, whose shim it passes through
        // ([`baked_build`]), puts a local build in the lock's place here.
        other => {
            return Err(other
                .refusal(&found, lock, LOCAL_SYSROOT_REMEDY)
                .unwrap_or_default());
        }
    };
    let _ = mark_locked(&prefix, lock.build);
    if build == lock.build {
        return Ok(None);
    }
    let tool = baked.file_name().ok_or("the shim names no tool")?;
    let root = prefix
        .join(aterm_toolchain::layout::COMPAT_DIR)
        .join(PROGRAM)
        .join(lock.build.to_string());
    let via_root = root.join("bin").join(tool);
    if routed
        && std::fs::symlink_metadata(crate::compat::root_marker(&root)).is_ok_and(|m| m.is_file())
        && aterm_toolchain::discovery::is_executable_file(&via_root)
    {
        return Ok(Some(via_root));
    }
    Ok(Some(bin.join(tool)))
}

/// How long an install of a locked build waits behind another pass's store lock, as a spawned
/// pass waits (`--wait-lock`): one tree's build waits while another tree's hop installs the
/// same build, then finds it present.
pub const INSTALL_WAIT_LOCK_SECS: u64 = 1800;

/// The words of `aterm pkg install` that stage `lock`'s build into this atpkg's store: no
/// first-run setup, waiting [`INSTALL_WAIT_LOCK_SECS`] behind a held store lock, the tree root
/// held to the lock's (`install_build`'s `expect_root`, from the lock above the working directory).
#[must_use]
pub fn install_args(lock: &TrustLock) -> Vec<String> {
    vec![
        "install".to_string(),
        "--no-init".to_string(),
        PROGRAM.to_string(),
        "--build".to_string(),
        lock.build.to_string(),
        "--wait-lock".to_string(),
        INSTALL_WAIT_LOCK_SECS.to_string(),
    ]
}

/// Whether this atpkg's configured store is the one at `prefix`, which an install of a locked
/// build must stage into to be found there.
///
/// # Errors
/// Naming both stores when they differ (another `HOME` or `[packages].prefix` than the shim's).
pub fn installs_into(prefix: &Path) -> Result<(), String> {
    let configured = crate::store::resolve_configured().map(|l| l.prefix);
    let same = |p: &Path| match (p.canonicalize(), prefix.canonicalize()) {
        (Ok(a), Ok(b)) => a == b,
        _ => p == prefix,
    };
    match configured {
        Some(p) if same(&p) => Ok(()),
        other => Err(format!(
            "this atpkg installs into {}, not the store {} this build resolves from",
            other.map_or_else(|| "no store".to_string(), |p| p.display().to_string()),
            prefix.display()
        )),
    }
}

/// THE HOP'S INSTALL OF A LOCKED BUILD THE STORE AT `prefix` LACKS: `<this atpkg>`
/// [`install_args`] ([`crate::flow::install_build`] behind it: verified under the head index,
/// staged beside `current`, which stays, held to the lock's tree), refused when this atpkg's store
/// is another ([`installs_into`]); its output on this process's stderr so the build's own stdout
/// (`targo metadata`, JSON messages) carries nothing else.
///
/// # Errors
/// Another store, this executable not found or started, or an install that does not succeed.
pub fn install_named(prefix: &Path, lock: &TrustLock) -> Result<(), String> {
    installs_into(prefix)?;
    let me = std::env::current_exe().map_err(|e| format!("this atpkg's path: {e}"))?;
    let to_stderr = stderr_as_stdio().map_err(|e| format!("stderr: {e}"))?;
    eprintln!(
        "atpkg: installing Trust build {} for this tree's trust.lock (once; `current` stays)",
        lock.build
    );
    let status = aterm_uds::spawnfd::command(&me)
        .args(install_args(lock))
        .stdout(to_stderr)
        .status()
        .map_err(|e| format!("{}: {e}", me.display()))?;
    if status.success() {
        Ok(())
    } else {
        Err(format!(
            "`aterm pkg install trust --build {}` {status}",
            lock.build
        ))
    }
}

/// This process's stderr, duplicated, as a child's stdio.
#[cfg(unix)]
fn stderr_as_stdio() -> std::io::Result<aterm_uds::spawnfd::Stdio> {
    use std::os::fd::AsFd as _;
    Ok(std::io::stderr().as_fd().try_clone_to_owned()?.into())
}

/// This process's stderr, duplicated, as a child's stdio.
#[cfg(windows)]
fn stderr_as_stdio() -> std::io::Result<aterm_uds::spawnfd::Stdio> {
    use std::os::windows::io::AsHandle as _;
    Ok(std::io::stderr().as_handle().try_clone_to_owned()?.into())
}

/// Whether `helper` (the atpkg a `targo` shim hops through) runs a tree's locked build: it answers
/// `__build_admit --lock-capabilities` with [`LOCK_CAPABILITY`] alone, exit 0, within ten seconds.
#[must_use]
pub fn helper_honours_lock(helper: &Path) -> bool {
    let mut command = aterm_uds::spawnfd::command(helper);
    command.args([crate::build_admit::VERB, LOCK_CAPABILITY_FLAG]);
    aterm_uds::deadline::output_within(&mut command, std::time::Duration::from_secs(10)).is_ok_and(
        |out| out.status.success() && out.stdout == format!("{LOCK_CAPABILITY}\n").as_bytes(),
    )
}

/// What plain `targo` runs on this machine in a locked tree ([`shim_answer`]).
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum ShimAnswer {
    /// The shim hops through an atpkg that runs the lock's build.
    Locked,
    /// The shim runs the build it was laid for, whatever a lock says: its target, and that
    /// target's store build when it is one.
    Laid {
        /// The shim's target.
        target: PathBuf,
        /// The store build the target is in, if it is a store build.
        build: Option<u64>,
    },
    /// There is no readable `targo` shim in the prefix.
    NoShim,
}

/// What the `targo` shim in `<prefix>/bin` runs in a locked tree ([`ShimAnswer`]).
#[must_use]
pub fn shim_answer(prefix: &Path) -> ShimAnswer {
    let Some(targo) = crate::store::ToolName::new("targo") else {
        return ShimAnswer::NoShim;
    };
    let shim = prefix.join("bin").join(targo.shim_file());
    let Ok(text) = crate::metadata_io::read_bounded_regular_utf8(&shim, 64 << 10) else {
        return ShimAnswer::NoShim;
    };
    let hops = text.contains(crate::build_admit::VERB);
    if hops
        && crate::build_cache::registered::shim_helper(&text)
            .is_some_and(|helper| helper_honours_lock(&helper))
    {
        return ShimAnswer::Locked;
    }
    #[cfg(unix)]
    let target = crate::platform::parse_sh_shim_target(&text);
    #[cfg(not(unix))]
    let target = crate::platform::parse_cmd_shim_target(&text);
    match target {
        Some(target) => ShimAnswer::Laid {
            build: baked_build(&target).map(|(_, build, _)| build),
            target,
        },
        None => ShimAnswer::NoShim,
    }
}

/// The sentence verify states when plain `targo` and verify compile with different builds: the
/// shim runs the build it was laid for, whose `bin` is not `resolved` (the directory the run's
/// discovery chose for this tree: the lock's build, or what took its place), so a plain build and
/// verify's alternate units. `None` when the shim honours locks or runs `resolved` itself.
#[must_use]
pub fn shim_mismatch(
    answer: &ShimAnswer,
    found: &Found,
    lock: &TrustLock,
    resolved: &Path,
) -> Option<String> {
    let ShimAnswer::Laid { target, build } = answer else {
        return None;
    };
    let physical = |p: &Path| p.canonicalize().unwrap_or_else(|_| p.to_path_buf());
    if target.parent().map(physical) == Some(physical(resolved)) {
        return None;
    }
    Some(format!(
        "plain `targo` resolves {} (the build its shim was laid for, {}); {} names {} and this \
         run compiles with {}: plain builds and verify alternate units until this machine's \
         aterm runs locked builds through its shim",
        build.map_or_else(|| "another build".to_string(), |b| b.to_string()),
        target.display(),
        found.path.display(),
        lock.build,
        resolved.display()
    ))
}

#[cfg(test)]
#[path = "trust_lock_tests.rs"]
mod tests;
