// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Candidate `trust.lock` resolution helpers for A14's release integration
//! (docs/DESIGN-fast-build-release-2026-11-05.md §4.7).
//!
//! These helpers resolve and check a candidate's compiler independently of the store's
//! `current`. The production release path does not call them yet. Its remaining integration
//! must choose the candidate before resolving its compiler, lease it before probing its identity,
//! seal inputs under it, retain the locked build and tree root in the journal, and use the lock's
//! stock toolchain throughout the cut.
//!
//! - [`lock_at`]: the lock a commit carries (`git show <rev>:trust.lock`).
//! - [`locked_prover`]: the store build that lock names, by number and tree root, reinstalled by
//!   (build, tree root) when the store lacks it, its compiler held to the lock's commit; a
//!   `TRUST_STAGE2_BIN` that is not that build is refused in a cut and taken, said, otherwise.
//! - [`install_locked`]: the reinstall, `aterm pkg install trust --build <N>` in this process.
//! - [`stock_toolchain`]: the lock's `[stock]` release, for the STOCK EXCEPTION slices.

use std::path::{Path, PathBuf};

use atpkg::trust_lock::{LOCK_FILE, LockedBuild, TrustLock};

use crate::ledger::GitRunner;

/// The lock commit `rev` carries, parsed; `Ok(None)` when it carries none.
///
/// # Errors
/// When git cannot answer for `rev`, or the lock it carries is not UTF-8 or does not parse (a
/// candidate that names its compiler unreadably is refused, never released on another one).
pub(crate) fn lock_at(git: &dyn GitRunner, rev: &str) -> Result<Option<TrustLock>, String> {
    let spec = format!("{rev}:{LOCK_FILE}");
    let listed = git
        .git(&["ls-tree", "--name-only", rev, "--", LOCK_FILE])
        .map_err(|e| e.to_string())?;
    if !listed.success() {
        return Err(format!(
            "git ls-tree {rev} failed: {}",
            listed.stderr_utf8().trim()
        ));
    }
    if String::from_utf8_lossy(&listed.stdout).trim().is_empty() {
        return Ok(None);
    }
    let out = crate::ledger::git_ok(git, &["show", &spec]).map_err(|e| e.to_string())?;
    let text = String::from_utf8(out.stdout).map_err(|_| format!("{spec} is not UTF-8"))?;
    atpkg::trust_lock::parse(&text)
        .map(Some)
        .map_err(|why| format!("{spec}: {why}"))
}

/// Which run resolves the prover: a cut binds its compiler to the candidate's lock; any other run
/// (a dry run, a check) may take an explicit override, which it says.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Run {
    /// A cut: only the lock's build proves and builds.
    Cut,
    /// Anything else: `TRUST_STAGE2_BIN` takes the lock's place, said in `source`.
    Other,
}

/// The prover a lock resolved ([`locked_prover`]).
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct LockedProver {
    /// The build's `bin`, canonical: `trustc` and `targo` are there.
    pub bin: PathBuf,
    /// Where it came from, for the run's notes.
    pub source: String,
}

/// THE PROVER A CANDIDATE'S LOCK NAMES, in the store at `prefix`: its build when the store holds
/// it with the lock's tree root (folding a build staged before stages recorded one), else what
/// `install` stages for it, held to the same tree; its `trustc` must report the lock's
/// `compiler_commit`. An `explicit` `TRUST_STAGE2_BIN` that is not that tree (its build directory's
/// record or fold), or whose tree does not fold, is refused in a [`Run::Cut`] and taken, said, in
/// any other run.
///
/// # Errors
/// A store that holds the build with another tree, an install that fails or stages another tree,
/// a compiler of another commit, an override that does not resolve, and in a cut an override of
/// another or no tree. Each names the lock's build and what was found instead.
pub(crate) fn locked_prover(
    prefix: &Path,
    lock: &TrustLock,
    explicit: Option<&Path>,
    run: Run,
    install: &mut dyn FnMut(&Path, &TrustLock) -> Result<(), String>,
) -> Result<LockedProver, String> {
    let names = format!(
        "the candidate's trust.lock names Trust build {} (tree root {})",
        lock.build,
        short(&lock.tree_root)
    );
    if let Some(dir) = explicit {
        let bin = dir
            .canonicalize()
            .map_err(|e| format!("TRUST_STAGE2_BIN={} does not resolve: {e}", dir.display()))?;
        let root = bin
            .parent()
            .ok_or_else(|| "no build above it".to_string())
            .and_then(|tree| {
                atpkg::trust_lock::recorded_root_at(tree)
                    .map_or_else(|| atpkg::tree::tree_root(tree), Ok)
                    .map_err(|e| format!("its tree does not fold: {e}"))
            });
        let is = match root {
            Ok(root) if root == lock.tree_root => {
                return committed(
                    bin,
                    lock,
                    format!("TRUST_STAGE2_BIN, the lock's build {}", lock.build),
                );
            }
            Ok(root) => format!("is tree root {}", short(&root)),
            Err(why) => format!("has no tree root ({why})"),
        };
        let said = format!("TRUST_STAGE2_BIN={} {is}; {names}", bin.display());
        return match run {
            Run::Cut => Err(format!(
                "{said}: a cut compiles with its candidate's locked build only (unset \
                 TRUST_STAGE2_BIN)"
            )),
            Run::Other => Ok(LockedProver {
                bin,
                source: format!("{said}; this run takes the override"),
            }),
        };
    }
    let mut installed = false;
    loop {
        match atpkg::trust_lock::standing(prefix, lock) {
            LockedBuild::Present { bin } => {
                let bin = bin
                    .canonicalize()
                    .map_err(|e| format!("{names}, whose bin does not resolve: {e}"))?;
                let _ = atpkg::trust_lock::mark_locked(prefix, lock.build);
                let source = if installed {
                    format!("the candidate's locked build {}, reinstalled", lock.build)
                } else {
                    format!("the candidate's locked build {}", lock.build)
                };
                return committed(bin, lock, source);
            }
            LockedBuild::Absent if !installed => {
                install(prefix, lock).map_err(|why| {
                    format!("{names}, which this Mac's store does not hold, and its install failed: {why}")
                })?;
                installed = true;
            }
            LockedBuild::Absent | LockedBuild::Unrecorded => {
                return Err(format!(
                    "{names}, which this Mac's store does not hold{}",
                    if installed { " after its install" } else { "" }
                ));
            }
            LockedBuild::Mismatch { recorded } => {
                return Err(format!(
                    "{names}; this Mac's store holds that build with tree root {}: it is not the \
                     build the candidate was locked to",
                    short(&recorded)
                ));
            }
        }
    }
}

/// `bin` as the lock's prover, once its `trustc` reports the lock's compiler commit.
fn committed(bin: PathBuf, lock: &TrustLock, source: String) -> Result<LockedProver, String> {
    match atpkg::trust_lock::compiler_commit(&bin) {
        Some(commit) if commit == lock.compiler_commit => Ok(LockedProver { bin, source }),
        reported => Err(format!(
            "{}/trustc reports {}, and the candidate's trust.lock names compiler commit {}",
            bin.display(),
            reported.map_or_else(|| "no commit-hash".to_string(), |c| format!("commit {c}")),
            lock.compiler_commit
        )),
    }
}

/// THE REINSTALL A VANISHED LOCKED BUILD GETS: `aterm pkg install trust --build <N>` in this
/// process (atpkg's own install, verified under the head index, staged beside `current`, which
/// stays; [`atpkg::trust_lock::install_args`]: no first-run setup, waiting behind a held store
/// lock), refused when atpkg's store is not the one at `prefix`. [`locked_prover`] then holds
/// what it staged to the lock's tree.
///
/// # Errors
/// Another store, or the install's own exit status when it is not success.
pub(crate) fn install_locked(prefix: &Path, lock: &TrustLock) -> Result<(), String> {
    atpkg::trust_lock::installs_into(prefix)?;
    let argv: Vec<std::ffi::OsString> = atpkg::trust_lock::install_args(lock)
        .into_iter()
        .map(Into::into)
        .collect();
    let code = atpkg::cli::main_entry(argv);
    if code == std::process::ExitCode::SUCCESS {
        Ok(())
    } else {
        Err(format!(
            "`aterm pkg install trust --build {}` exited {code:?}",
            lock.build
        ))
    }
}

/// The upstream toolchain the STOCK EXCEPTION slices build with: the lock's `[stock]` release, or
/// rustup's `stable` for a candidate with no lock.
#[must_use]
pub(crate) fn stock_toolchain(lock: Option<&TrustLock>) -> &str {
    lock.map_or("stable", |l| l.stock_toolchain.as_str())
}

fn short(root: &str) -> &str {
    root.get(..12).unwrap_or(root)
}

#[cfg(test)]
#[path = "trust_lock_tests.rs"]
mod tests;
