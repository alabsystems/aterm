// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Candidate-owned `trust.lock` authority (docs/DESIGN-fast-build-release-2026-11-05.md §4.7).
//!
//! Release selection reads the original lock at an exact admitted source commit, resolves its
//! managed build independently of `current`, holds and rechecks its tree before any compiler
//! probe, and carries the complete declaration through journals, input seals and cutter exec.
//!
//! - [`lock_at`]: the original regular lock blob a commit carries.
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
    let listed = git
        .git_original(&["ls-tree", "-z", rev, "--", LOCK_FILE])
        .map_err(|e| e.to_string())?;
    if !listed.success() {
        return Err(format!(
            "git ls-tree {rev} failed: {}",
            listed.stderr_utf8().trim()
        ));
    }
    if listed.stdout.is_empty() {
        return Ok(None);
    }
    let invalid = || format!("{rev}:{LOCK_FILE} is not one original regular Git blob");
    let row = std::str::from_utf8(&listed.stdout)
        .ok()
        .and_then(|row| row.strip_suffix('\0'))
        .ok_or_else(invalid)?;
    let (entry, name) = row.split_once('\t').ok_or_else(invalid)?;
    let fields = entry.split(' ').collect::<Vec<_>>();
    let [mode, kind, oid] = fields.as_slice() else {
        return Err(invalid());
    };
    if !matches!(*mode, "100644" | "100755")
        || *kind != "blob"
        || name != LOCK_FILE
        || !agh::path::is_sha(oid)
    {
        return Err(invalid());
    }
    let out = git
        .git_original(&["cat-file", "blob", oid])
        .map_err(|e| e.to_string())?;
    if !out.success() {
        return Err(format!(
            "original {rev}:{LOCK_FILE} could not be read: {}",
            out.stderr_utf8().trim()
        ));
    }
    let spec = format!("{rev}:{LOCK_FILE}");
    if out.stdout.len() > aterm_toolchain::discovery::MAX_ATERM_CONFIG_BYTES {
        return Err(format!(
            "{spec} exceeds the bounded Trust configuration size"
        ));
    }
    let text = String::from_utf8(out.stdout).map_err(|_| format!("{spec} is not UTF-8"))?;
    atpkg::trust_lock::parse(&text)
        .map(Some)
        .map_err(|why| format!("{spec}: {why}"))
}

/// The complete compiler declaration retained by a release, independently of mutable store
/// selectors and physical paths. Its source is the journal's exact candidate commit.
#[derive(Clone, Debug, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(deny_unknown_fields)]
pub struct CompilerPin {
    /// The exact managed Trust build.
    pub build: u64,
    /// Its declared signed content tree.
    pub tree_root: String,
    /// The compiler revision that build must report.
    pub compiler_commit: String,
    /// The lock's declared build-lineage receipt.
    pub receipt: String,
    /// The upstream compiler role selected by the same source.
    pub stock_toolchain: String,
}

/// The physical compiler and complete source declaration whose inputs a release seals.
/// Construction does not grant custody: the release retains its checked managed lease
/// for as long as it uses or hands on these inputs.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct CompilerInputs {
    pub(crate) bin: PathBuf,
    pub(crate) pin: CompilerPin,
    /// Captured host namespace for this source declaration, never reconstructed after sealing.
    pub(crate) stock: Result<crate::buildplan::Stock, String>,
}

impl CompilerInputs {
    pub(crate) fn stock(&self) -> Result<&crate::buildplan::Stock, String> {
        self.stock.as_ref().map_err(Clone::clone)
    }
}

/// One admitted package namespace, captured independently of subsequent HOME/config changes.
/// It grants no compiler authority: each selection still proves original source, full pin,
/// exact Trust subject, native lease and fresh tree before executing the compiler.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct ManagedStoreContext {
    prefix: PathBuf,
}

impl ManagedStoreContext {
    pub(crate) fn configured() -> Result<Self, String> {
        let home = aterm_types::dirs::home_dir();
        let configured = atpkg::config::load().prefix_path(home.as_deref());
        let layout = atpkg::store::resolve(configured.as_deref())
            .ok_or("the candidate compiler requires a managed atpkg store")?;
        Ok(Self {
            prefix: layout.prefix,
        })
    }

    /// An explicit caller declaration, vetted against its selected tool home. Tests use the
    /// verifier's existing tool/store context while keeping their HOME and journals private.
    #[cfg(test)]
    pub(crate) fn declared(tool_home: &Path, prefix: &Path) -> Result<Self, String> {
        let vetted = atpkg::store::vet_prefix(Some(prefix), tool_home);
        if vetted != prefix {
            return Err("the declared compiler store is not an admitted package prefix".into());
        }
        Ok(Self { prefix: vetted })
    }

    pub(crate) fn prefix(&self) -> &Path {
        &self.prefix
    }

    #[cfg(unix)]
    pub(crate) fn offer(
        &self,
        bin: &Path,
        build: u64,
    ) -> Result<crate::gates::ToolchainLeaseOffer, String> {
        crate::gates::offer_managed_toolchain_lease(&self.prefix, bin, build)
    }
}

impl CompilerPin {
    pub(crate) fn of(lock: &TrustLock) -> Self {
        Self {
            build: lock.build,
            tree_root: lock.tree_root.clone(),
            compiler_commit: lock.compiler_commit.clone(),
            receipt: lock.receipt.clone(),
            stock_toolchain: lock.stock_toolchain.clone(),
        }
    }

    pub(crate) fn lock(&self) -> TrustLock {
        TrustLock {
            build: self.build,
            tree_root: self.tree_root.clone(),
            compiler_commit: self.compiler_commit.clone(),
            receipt: self.receipt.clone(),
            stock_toolchain: self.stock_toolchain.clone(),
        }
    }

    /// A release selects an immutable commit, never a moving ref or its working-tree lock.
    pub(crate) fn at(git: &dyn GitRunner, commit: &str) -> Result<Self, String> {
        if !agh::path::is_sha(commit) {
            return Err("compiler selection requires an exact candidate commit".into());
        }
        let kind = git
            .git_original(&["cat-file", "-t", commit])
            .map_err(|error| error.to_string())?;
        if !kind.success() || kind.stdout != b"commit\n" {
            return Err(format!(
                "compiler source {commit} is not an original commit object"
            ));
        }
        lock_at(git, commit)?
            .as_ref()
            .map(Self::of)
            .ok_or_else(|| format!("candidate {commit} declares no trust.lock"))
    }
}

/// Which run resolves the prover. Release judgments, including dry runs, use `Cut`;
/// a standalone nonrelease check may explicitly choose another compiler.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Run {
    /// A cut: only the lock's build proves and builds.
    Cut,
    /// The test's non-release comparator, never a cut or a shipped override route.
    #[cfg(test)]
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
/// `before_probe` must establish and retain the selected build's checked lease before its
/// compiler executes; a failed hold returns without executing that compiler.
pub(crate) fn locked_prover(
    prefix: &Path,
    lock: &TrustLock,
    explicit: Option<&Path>,
    run: Run,
    install: &mut dyn FnMut(&Path, &TrustLock) -> Result<(), String>,
    before_probe: &mut dyn FnMut(&Path) -> Result<(), String>,
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
                    before_probe,
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
            #[cfg(test)]
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
                return committed(bin, lock, source, before_probe);
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
fn committed(
    bin: PathBuf,
    lock: &TrustLock,
    source: String,
    before_probe: &mut dyn FnMut(&Path) -> Result<(), String>,
) -> Result<LockedProver, String> {
    before_probe(&bin)?;
    // Resolution above only chooses a slot. Its bytes become authority after the caller
    // holds that slot: a replacement between selection and the hold must not inherit the
    // selected tree's verdict merely because its compiler reports the same commit.
    let tree = bin
        .parent()
        .ok_or_else(|| "the selected compiler has no build directory".to_string())?;
    let held_bin = bin
        .canonicalize()
        .map_err(|error| format!("the held compiler bin does not resolve: {error}"))?;
    let root = atpkg::trust_lock::fold_and_record(tree)
        .map_err(|error| format!("the held compiler tree does not fold: {error}"))?;
    if held_bin != bin || root.as_deref() != Some(lock.tree_root.as_str()) {
        return Err(format!(
            "the held compiler tree changed before its identity probe; the candidate requires Trust build {} (tree root {})",
            lock.build,
            short(&lock.tree_root),
        ));
    }
    match atpkg::trust_lock::compiler_commit(&bin) {
        Ok(commit) if commit == lock.compiler_commit => Ok(LockedProver { bin, source }),
        Ok(commit) => Err(format!(
            "{}/trustc reports commit {commit}, and the candidate's trust.lock names compiler commit {}",
            bin.display(),
            lock.compiler_commit
        )),
        Err(error) => Err(format!("identify {}/trustc: {error}", bin.display())),
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

/// THE UPSTREAM RELEASE THE STOCK EXCEPTION SLICES BUILD WITH, which they drive by name in
/// `RUSTUP_TOOLCHAIN`: the universal binary's x86_64 slice, `ship check --compat` (and `verify
/// watch`'s stage that rehearses it), a Linux worker's static bootstrap verifier, `ship
/// provision`'s x86 slice probe, and the stock compiler identity the release records of them.
/// It is the `[stock] toolchain` of `lock`, the lock `tree` carries. A lock with no `[stock]`
/// toolchain does not parse (`atpkg::trust_lock::parse`), so its refusal comes from the reader.
///
/// # Errors
/// `tree` carries no lock (`None`): the release never takes rustup's floating `stable`, whose
/// release is whatever a machine last installed, in the lock's place.
pub(crate) fn stock_toolchain(lock: Option<TrustLock>, tree: &str) -> Result<String, String> {
    lock.map(|lock| lock.stock_toolchain).ok_or_else(|| {
        format!(
            "{tree} carries no {LOCK_FILE}, whose `[stock] toolchain` names the release the \
             STOCK EXCEPTION slices build with; a release never falls back to rustup's `stable`"
        )
    })
}

/// The stock release commit `rev` carries ([`lock_at`], [`stock_toolchain`]): a cut's candidate,
/// whose snapshot the slices build and whose identity the release records.
///
/// # Errors
/// What [`lock_at`] refuses (among it a lock with no `[stock] toolchain`), and a commit that
/// carries no lock.
pub(crate) fn stock_toolchain_at(git: &dyn GitRunner, rev: &str) -> Result<String, String> {
    stock_toolchain(lock_at(git, rev)?, &format!("commit {rev}"))
}

/// The stock release of the checkout this process runs in ([`stock_toolchain_in`] of its
/// directory): what the release reads of this machine for it (its build-inputs seal, `ship
/// provision`'s probe). In a cut that is the release clone, at the candidate.
///
/// # Errors
/// This process has no directory, and what [`stock_toolchain_in`] refuses.
pub(crate) fn checkout_stock_toolchain() -> Result<String, String> {
    let dir = std::env::current_dir().map_err(|e| format!("this process's directory: {e}"))?;
    stock_toolchain_in(&dir)
}

/// The stock release of the checkout at or above `dir`: the first `trust.lock` on the way up
/// (`atpkg::trust_lock::find`), as the release's own Trust resolution finds the lock it compiles
/// with (`gates::trust_stage2_bin`).
///
/// # Errors
/// No lock at or above `dir`, and one that cannot be read or does not parse (among it one with no
/// `[stock] toolchain`).
pub(crate) fn stock_toolchain_in(dir: &Path) -> Result<String, String> {
    let lock = atpkg::trust_lock::find(dir)
        .map(|found| found.lock)
        .transpose()?;
    stock_toolchain(lock, &format!("the checkout at {}", dir.display()))
}

/// Resolve exact-source input roles under a managed lease retained in gates' process set.
/// This does not pin or replace the launcher's global compiler. Release orchestration also
/// retains an owned descriptor and passes the same validated declaration across exec.
pub(crate) fn inputs_at(git: &dyn GitRunner, commit: &str) -> Result<CompilerInputs, String> {
    inputs_with_install(git, commit, &mut install_locked)
}

/// Exact input roles for a read-only observer. An absent build is unknown here; only an
/// active release or preparation may reinstall its candidate's exact managed build.
pub(crate) fn available_inputs_at(
    git: &dyn GitRunner,
    commit: &str,
) -> Result<CompilerInputs, String> {
    inputs_with_install(git, commit, &mut |_, lock| {
        Err(format!(
            "Trust build {} is unavailable to this read-only observer",
            lock.build,
        ))
    })
}

fn inputs_with_install(
    git: &dyn GitRunner,
    commit: &str,
    install: &mut dyn FnMut(&Path, &TrustLock) -> Result<(), String>,
) -> Result<CompilerInputs, String> {
    let pin = CompilerPin::at(git, commit)?;
    let store = ManagedStoreContext::configured()?;
    let explicit = std::env::var_os("TRUST_STAGE2_BIN")
        .filter(|value| !value.is_empty())
        .map(PathBuf::from);
    let selected = locked_prover(
        store.prefix(),
        &pin.lock(),
        explicit.as_deref(),
        Run::Cut,
        install,
        &mut |bin| {
            #[cfg(unix)]
            {
                drop(store.offer(bin, pin.build)?);
                Ok(())
            }
            #[cfg(not(unix))]
            Err("candidate compiler custody requires Unix descriptors".into())
        },
    )?;
    let home = std::env::var_os("HOME").ok_or("compiler inputs require HOME")?;
    let stock = crate::buildplan::Stock::under(&home, &pin.stock_toolchain);
    Ok(CompilerInputs {
        bin: selected.bin,
        pin,
        stock,
    })
}

fn short(root: &str) -> &str {
    root.get(..12).unwrap_or(root)
}

#[cfg(test)]
#[path = "trust_lock_tests.rs"]
mod tests;
