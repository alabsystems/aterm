// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0

//! Rollback-aware GC retention (§10.2).

use std::collections::{BTreeMap, BTreeSet};
use std::path::{Path, PathBuf};

use crate::manifest::Channel;
use crate::store::Layout;

/// Evidence that `build` is the live build of `program`.
#[derive(Clone, PartialEq, Eq, Debug)]
pub struct LiveBuild {
    program: String,
    build: u64,
}

impl LiveBuild {
    /// The program this witness is about.
    #[cfg(test)]
    #[must_use]
    pub(crate) fn program(&self) -> &str {
        &self.program
    }

    /// The build proven live.
    #[must_use]
    pub fn build(&self) -> u64 {
        self.build
    }
}

/// Why a program has no live witness: deleting anything would be a guess, so GC abstains and `doctor` names the disagreement.
#[derive(Clone, PartialEq, Eq, Debug)]
pub enum Diverged {
    /// The program's `current` link selects one build; its `bin/` shims run another.
    LinkShimMismatch { link_says: u64, shims_say: u64 },
    /// This program's own tools resolve into different builds: an [`crate::activate::install_shims`] loop that failed before its prune.
    ShimsDisagree { builds: Vec<u64> },
    /// The program is live on `PATH` but no `current` link resolves into it: shimmed but never activated, or its `current` dangles.
    NoLiveWitness { shims_say: u64 },
}

/// A program GC refused to consider, with the evidence from both views.
#[derive(Clone, PartialEq, Eq, Debug)]
pub struct Divergence {
    pub program: String,
    pub reason: Diverged,
}

/// The prefix's reconciled answer to "which build of each program is live".
#[derive(Clone, PartialEq, Eq, Debug, Default)]
pub struct LiveSet {
    live: BTreeMap<String, LiveBuild>,
    diverged: Vec<Divergence>,
}

impl LiveSet {
    /// The witness for `program`, if the prefix proved one.
    #[must_use]
    pub fn get(&self, program: &str) -> Option<&LiveBuild> {
        self.live.get(program)
    }

    /// The programs with no witness, ascending by name.
    #[must_use]
    pub fn diverged(&self) -> &[Divergence] {
        &self.diverged
    }

    /// Take the divergences, for folding into a [`GcReport`].
    #[must_use]
    pub fn into_diverged(self) -> Vec<Divergence> {
        self.diverged
    }

    /// How many programs have a proven live build.
    #[must_use]
    pub fn len(&self) -> usize {
        self.live.len()
    }

    /// Whether nothing could be proven live.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.live.is_empty()
    }
}

/// Why a reclaim was refused: a report line, not a panic mid-pass.
#[derive(Clone, PartialEq, Eq, Debug)]
pub enum Retained {
    /// The build is the live one.
    IsLive { program: String, build: u64 },
}

/// Resolve one `current` symlink into the `(program, build)` it proves live, or `None`.
fn current_target(prefix: &Path, current: &Path) -> Option<(String, u64)> {
    let target = std::fs::read_link(current).ok()?;
    if target
        .strip_prefix(prefix.join("store"))
        .ok()?
        .components()
        .count()
        != 2
    {
        return None;
    }
    if !target.is_dir() {
        return None;
    }
    crate::ops::store_build_of(prefix, &target)
}

/// Every build the authoritative `store/<program>/current` links select, one per program.
fn authority_claims(layout: &Layout) -> BTreeMap<String, u64> {
    let mut out: BTreeMap<String, u64> = BTreeMap::new();
    if let Ok(programs) = std::fs::read_dir(layout.prefix.join("store")) {
        for p in programs.flatten() {
            let Some((program, build)) = current_target(&layout.prefix, &p.path().join("current"))
            else {
                continue;
            };
            if p.file_name() == std::ffi::OsStr::new(&program) {
                out.insert(program, build);
            }
        }
    }
    out
}

/// Remove the retired per-channel `channels/<c>/current` links and their directories.
pub(crate) fn sweep_retired_channel_links(layout: &Layout) {
    let root = layout.prefix.join("channels");
    let Ok(channels) = std::fs::read_dir(&root) else {
        return;
    };
    for ch in channels.flatten() {
        let link = ch.path().join("current");
        if std::fs::symlink_metadata(&link).is_ok() {
            crate::platform::remove_link(&link);
        }
        let _ = std::fs::remove_dir(ch.path());
    }
    let _ = std::fs::remove_dir(&root);
}

/// Every build the derived `bin/` shims point into, keyed by program. Unlike
fn shim_claims(layout: &Layout) -> BTreeMap<String, BTreeSet<u64>> {
    let mut out: BTreeMap<String, BTreeSet<u64>> = BTreeMap::new();
    let Ok(entries) = std::fs::read_dir(layout.bin_dir()) else {
        return out;
    };
    for e in entries.flatten() {
        // resolve_shim, not read_link: a Windows shim is a `.cmd` file read_link errs on,
        if let Some(target) = crate::platform::resolve_shim(&e.path())
            && let Some((program, build)) = crate::ops::store_build_of(&layout.prefix, &target)
        {
            out.entry(program).or_default().insert(build);
        }
    }
    out
}

/// Reconcile the authoritative and derived views into witnesses plus divergences.
#[must_use]
pub fn live_builds(layout: &Layout) -> LiveSet {
    live_from_claims(&authority_claims(layout), &shim_claims(layout))
}

/// [`live_builds`]' reconciliation over claim views already read, so a gc pass, whose sweep
fn live_from_claims(
    authority: &BTreeMap<String, u64>,
    shims: &BTreeMap<String, BTreeSet<u64>>,
) -> LiveSet {
    let mut set = LiveSet::default();
    let empty = BTreeSet::new();
    let programs: BTreeSet<&String> = authority.keys().chain(shims.keys()).collect();
    for program in programs {
        let own = authority.get(program).copied();
        let sh = shims.get(program).unwrap_or(&empty);
        let reason = if sh.len() > 1 {
            Diverged::ShimsDisagree {
                builds: sh.iter().copied().collect(),
            }
        } else {
            match (own, sh.first().copied()) {
                (Some(c), Some(s)) if c != s => Diverged::LinkShimMismatch {
                    link_says: c,
                    shims_say: s,
                },
                (Some(c), _) => {
                    set.live.insert(
                        program.clone(),
                        LiveBuild {
                            program: program.clone(),
                            build: c,
                        },
                    );
                    continue;
                }
                (None, Some(s)) => Diverged::NoLiveWitness { shims_say: s },
                (None, None) => continue,
            }
        };
        set.diverged.push(Divergence {
            program: program.clone(),
            reason,
        });
    }
    set
}

/// Reclaimable store builds of a program given `installed`, its live witness, and `lands_on` ([`crate::flow::rollback_lands_on`]).
#[must_use]
pub fn reclaimable(
    installed: &[u64],
    live: &LiveBuild,
    lands_on: &dyn Fn(u64) -> bool,
) -> Vec<u64> {
    let mut keep: BTreeSet<u64> = BTreeSet::new();
    keep.insert(live.build);
    if let Some(rollback) = installed
        .iter()
        .copied()
        .filter(|&b| b < live.build && lands_on(b))
        .max()
    {
        keep.insert(rollback);
    }
    let mut out: Vec<u64> = installed
        .iter()
        .copied()
        .filter(|b| !keep.contains(b))
        .collect();
    out.sort_unstable();
    out.dedup();
    out
}

/// Reclaim a superseded build of the witnessed program.
pub fn discard_superseded(layout: &Layout, live: &LiveBuild, build: u64) -> Result<(), Retained> {
    if build == live.build {
        return Err(Retained::IsLive {
            program: live.program.clone(),
            build,
        });
    }
    crate::store::discard_build(&layout.build_dir(&live.program, build));
    Ok(())
}

/// What an interrupted install left in `store/<program>/`. A partial tree is named by a build
#[derive(Default)]
struct Debris {
    /// `(program, build, dir)` — numeric build dirs with no completeness marker.
    partial: Vec<(String, u64, PathBuf)>,
    /// `(program, dir name, dir)` — `<build>.incoming-<pid>` / `<build>.superseded-<pid>`.
    scratch: Vec<(String, String, PathBuf)>,
    /// Stage sidecar files (`<build>.vendor`, `.shim-env`, `.modes`) whose `<build>` is gone:
    orphan_sidecars: Vec<PathBuf>,
}

/// The sidecars swept once their build is gone ([`crate::store::STAGE_SIDECAR_SUFFIXES`]).
fn orphan_sidecar_build(name: &str) -> Option<u64> {
    crate::store::STAGE_SIDECAR_SUFFIXES
        .iter()
        .find_map(|suffix| name.strip_suffix(suffix))
        .and_then(crate::store::parse_build_name)
}

/// Whether `name` is stage scratch this manager produced: `<build>.incoming-<pid>` or
fn is_stage_scratch(name: &str) -> bool {
    crate::store::stage_scratch_of(name).is_some()
}

/// Move back any tree a swap was killed midway through, before the sweep decides what is
fn recover_interrupted_swaps(layout: &Layout) -> BTreeMap<String, BTreeSet<u64>> {
    let mut parked: BTreeMap<String, BTreeSet<u64>> = BTreeMap::new();
    let Ok(programs) = std::fs::read_dir(layout.prefix.join("store")) else {
        return parked;
    };
    for prog in programs.flatten() {
        let Some(program) = prog.file_name().to_str().map(str::to_string) else {
            continue;
        };
        let Ok(entries) = std::fs::read_dir(prog.path()) else {
            continue;
        };
        let mut candidates: BTreeSet<u64> = BTreeSet::new();
        for entry in entries.flatten() {
            let name = entry.file_name();
            if let Some(name) = name.to_str()
                && let Some((build, crate::store::Scratch::Superseded)) =
                    crate::store::stage_scratch_of(name)
            {
                candidates.insert(build);
            }
        }
        for build in candidates {
            let dir = prog.path().join(build.to_string());
            if !crate::store::recover_interrupted_swap(&dir)
                && std::fs::symlink_metadata(&dir).is_err()
            {
                parked.entry(program.clone()).or_default().insert(build);
            }
        }
    }
    parked
}

/// Everything an install killed mid-extract leaves behind, across all programs.
fn interrupted_debris(
    layout: &Layout,
    claimed: &BTreeMap<String, BTreeSet<u64>>,
    parked: &BTreeMap<String, BTreeSet<u64>>,
    in_scope: &dyn Fn(&str) -> bool,
) -> Debris {
    let mut out = Debris::default();
    let Ok(programs) = std::fs::read_dir(layout.prefix.join("store")) else {
        return out;
    };
    for prog in programs.flatten() {
        let Some(program) = prog.file_name().to_str().map(str::to_string) else {
            continue;
        };
        if !in_scope(&program) {
            continue;
        }
        let Ok(entries) = std::fs::read_dir(prog.path()) else {
            continue;
        };
        for entry in entries.flatten() {
            // A sidecar whose build is provably absent describes nothing. A build parked
            if entry.file_type().is_ok_and(|t| t.is_file())
                && let Some(build) = entry.file_name().to_str().and_then(orphan_sidecar_build)
                && !parked.get(&program).is_some_and(|b| b.contains(&build))
                && matches!(
                    crate::store::presence(&prog.path().join(build.to_string())),
                    crate::store::Presence::Absent
                )
            {
                out.orphan_sidecars.push(entry.path());
                continue;
            }
            if !entry.file_type().is_ok_and(|t| t.is_dir()) {
                continue;
            }
            let name = entry.file_name();
            let Some(name) = name.to_str() else { continue };
            if let Some(build) = crate::store::parse_build_name(name) {
                if crate::store::build_is_complete(&entry.path()) {
                    // an installed build — `reclaimable` owns it
                    continue;
                }
                if crate::store::build_marks_another_slice(&entry.path()) {
                    // A complete install of the universal binary's other slice:
                    continue;
                }
                if claimed.get(&program).is_some_and(|s| s.contains(&build)) {
                    // something on disk points into it: not ours to delete
                    continue;
                }
                out.partial.push((program.clone(), build, entry.path()));
            } else if is_stage_scratch(name) {
                // A parked `<build>.superseded-<pid>` (nothing at `<build>`) is that build's
                if let Some((build, crate::store::Scratch::Superseded)) =
                    crate::store::stage_scratch_of(name)
                    && parked.get(&program).is_some_and(|b| b.contains(&build))
                {
                    continue;
                }
                out.scratch
                    .push((program.clone(), name.to_string(), entry.path()));
            }
        }
    }
    out.partial.sort();
    out.scratch.sort();
    out
}

/// What a GC pass did: what it reclaimed, what it swept, and what it refused to touch.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct GcReport {
    /// `(program, discarded builds ascending)` for every program that had reclaimable builds.
    pub reclaimed: Vec<(String, Vec<u64>)>,
    /// `(program, swept builds ascending)` for marker-less trees an interrupted install left.
    pub swept_partial: Vec<(String, Vec<u64>)>,
    /// `(program, swept scratch dir names ascending)`.
    pub swept_scratch: Vec<(String, Vec<String>)>,
    /// Programs skipped because the prefix could not prove which of their builds is live.
    pub diverged: Vec<Divergence>,
    /// `(program, staged file names ascending)` for archives a killed download left in `staging/<program>/`.
    pub swept_staging: Vec<(String, Vec<String>)>,
    /// Exec roots (`<prefix>/compat/trust/<n>`, [`crate::compat`]) removed because their build is gone or no longer needs one, and the dot debris a killed rebuild left.
    pub swept_exec_roots: Vec<PathBuf>,
    /// What the exec-root sweep could not remove, one sentence each.
    pub exec_root_errors: Vec<String>,
    /// `(program, build, who)` for every build this pass would have reclaimed but KEPT because a run holds a lease on it ([`crate::lease`]).
    pub leased: Vec<(String, u64, crate::lease::Holders)>,
    /// `(program, staged file name)` for every archive kept because a held Trust toolchain update installs from it ([`crate::quiet::held_archives`]).
    pub held_staging: Vec<(String, String)>,
}

/// Reclaim superseded builds per program, keeping the live build and one rollback.
#[must_use]
pub fn run(layout: &Layout, channel: Option<&Channel>) -> GcReport {
    run_in(layout, None, channel, &|_| None, &running_executables)
}

/// [`run`], with the staging sweep sparing, per program, the `.part` and the complete archive named by `pinned_asset` (the program's current pinned asset, from the caller's resolved signed index; `None` spares nothing).
#[must_use]
pub fn run_keeping_pinned_partials(
    layout: &Layout,
    channel: Option<&Channel>,
    pinned_asset: &dyn Fn(&str) -> Option<String>,
) -> GcReport {
    run_in(
        layout,
        None,
        channel,
        &|p: &str| pinned_asset(p).or_else(|| pursued_asset(layout, p)),
        &running_executables,
    )
}

/// The staging name a vendor-direct program is still working towards: the asset of the last
fn pursued_asset(layout: &Layout, program: &str) -> Option<String> {
    let spec = crate::vendor_direct::spec(program)?;
    let head = crate::vendor_direct::ProgramStamp::read(layout, program)?.last_head?;
    let active = crate::ops::active_builds(layout).get(program).copied();
    (active < Some(head.build_id())).then(|| {
        crate::vendor_direct::resolve::staging_asset(spec, head, crate::cli::current_triple())
    })
}

/// [`run_keeping_pinned_partials`] confined to `programs`.
#[must_use]
pub fn run_for_programs(
    layout: &Layout,
    programs: &BTreeSet<String>,
    pinned_asset: &dyn Fn(&str) -> Option<String>,
) -> GcReport {
    run_in(
        layout,
        Some(&|p: &str| programs.contains(p)),
        None,
        &|p: &str| pinned_asset(p).or_else(|| pursued_asset(layout, p)),
        &running_executables,
    )
}

/// [`run_keeping_pinned_partials`] with no channel and the process enumeration injected:
#[cfg(test)]
fn run_with(
    layout: &Layout,
    pinned_asset: &dyn Fn(&str) -> Option<String>,
    running: &dyn Fn() -> Option<Vec<PathBuf>>,
) -> GcReport {
    run_in(layout, None, None, pinned_asset, running)
}

/// The whole pass over the programs `scope` admits (`None`: the whole prefix, exec roots
fn run_in(
    layout: &Layout,
    scope: Option<&dyn Fn(&str) -> bool>,
    channel: Option<&Channel>,
    pinned_asset: &dyn Fn(&str) -> Option<String>,
    running: &dyn Fn() -> Option<Vec<PathBuf>>,
) -> GcReport {
    let in_scope = |program: &str| scope.is_none_or(|admits| admits(program));
    let mut in_use = InUse {
        layout,
        enumerate: running,
        running: None,
    };
    let parked = recover_interrupted_swaps(layout);
    sweep_retired_channel_links(layout);
    let authority = authority_claims(layout);
    let shims = shim_claims(layout);
    let live = live_from_claims(&authority, &shims);
    let mut claimed: BTreeMap<String, BTreeSet<u64>> = authority
        .into_iter()
        .map(|(program, build)| (program, BTreeSet::from([build])))
        .collect();
    for (program, builds) in shims {
        claimed.entry(program).or_default().extend(builds);
    }
    let mut by_prog: BTreeMap<String, Vec<u64>> = BTreeMap::new();
    for (p, b) in crate::ops::list_installed(layout) {
        by_prog.entry(p).or_default().push(b);
    }
    let mut reclaimed = Vec::new();
    let mut leased = Vec::new();
    for (program, installed) in by_prog {
        if !in_scope(&program) {
            continue;
        }
        let Some(witness) = live.get(&program) else {
            continue;
        };
        let vendor_yanks = crate::vendor_direct::is_vendor(&program)
            .then(|| crate::vendor_direct::policy::Policy::current(layout, None));
        let lands_on = |b: u64| match &vendor_yanks {
            Some(policy) => {
                crate::vendor_direct::lane::rollback_lands_on(layout, policy, &program, b)
            }
            None => crate::flow::rollback_lands_on(layout, channel, &program, b),
        };
        let mut gone = Vec::new();
        for b in reclaimable(&installed, witness, &lands_on) {
            let Some(_gate) = lease_clear(layout, &program, b, &mut leased) else {
                continue;
            };
            if in_use.holds(&program, &layout.build_dir(&program, b), Some(b)) {
                continue;
            }
            if discard_superseded(layout, witness, b).is_ok()
                && is_gone(&layout.build_dir(&program, b))
            {
                gone.push(b);
            }
        }
        if !gone.is_empty() {
            reclaimed.push((program, gone));
        }
    }

    let debris = interrupted_debris(layout, &claimed, &parked, &in_scope);
    let mut swept_partial: BTreeMap<String, Vec<u64>> = BTreeMap::new();
    for (program, build, path) in debris.partial {
        let Some(_gate) = lease_clear(layout, &program, build, &mut leased) else {
            continue;
        };
        if in_use.holds(&program, &path, Some(build)) {
            continue;
        }
        crate::store::discard_build(&path);
        if is_gone(&path) {
            swept_partial.entry(program).or_default().push(build);
        }
    }
    for sidecar in debris.orphan_sidecars {
        let _ = std::fs::remove_file(sidecar);
    }
    let mut swept_scratch: BTreeMap<String, Vec<String>> = BTreeMap::new();
    for (program, name, path) in debris.scratch {
        // Reported only when removed; the next pass enumerates a survivor again.
        if crate::store::remove_tree(&path).is_ok() {
            swept_scratch.entry(program).or_default().push(name);
        }
    }

    let mut swept_staging: Vec<(String, Vec<String>)> = Vec::new();
    let mut held_staging: Vec<(String, String)> = Vec::new();
    let held = crate::quiet::held_archives(layout);
    if let Ok(programs) = std::fs::read_dir(layout.prefix.join("staging")) {
        for program in programs.filter_map(Result::ok) {
            let Ok(name) = program.file_name().into_string() else {
                continue;
            };
            if !in_scope(&name) {
                continue;
            }
            let Ok(entries) = std::fs::read_dir(program.path()) else {
                continue;
            };
            let pinned = pinned_asset(&name);
            let keep_part = pinned.as_ref().map(|asset| format!("{asset}.part"));
            let keep_archive = pinned;
            let mut gone: Vec<String> = Vec::new();
            for e in entries.filter_map(Result::ok) {
                // Regular files only: never follow a symlink out of the prefix, and
                if !e.file_type().is_ok_and(|t| t.is_file()) {
                    continue;
                }
                let entry_name = e.file_name().into_string().ok();
                if entry_name.is_some() && (keep_archive == entry_name || keep_part == entry_name) {
                    continue;
                }
                if let Some(n) = entry_name.as_ref()
                    && held.get(&name) == Some(n)
                {
                    held_staging.push((name.clone(), n.clone()));
                    continue;
                }
                if std::fs::remove_file(e.path()).is_ok()
                    && let Some(n) = entry_name
                {
                    gone.push(n);
                }
            }
            if !gone.is_empty() {
                gone.sort();
                swept_staging.push((name, gone));
            }
        }
        swept_staging.sort();
    }

    let exec_roots = if scope.is_none() {
        crate::compat::sweep(layout)
    } else {
        crate::compat::Report::default()
    };
    if scope.is_none() {
        crate::lease::sweep(&layout.prefix, &|subject| {
            std::fs::symlink_metadata(subject.path(layout)).is_ok()
        });
    }

    GcReport {
        swept_staging,
        reclaimed,
        swept_partial: swept_partial.into_iter().collect(),
        swept_scratch: swept_scratch.into_iter().collect(),
        diverged: live
            .into_diverged()
            .into_iter()
            .filter(|d| in_scope(&d.program))
            .collect(),
        swept_exec_roots: exec_roots.swept,
        exec_root_errors: exec_roots.errors,
        leased,
        held_staging,
    }
}

/// Whether `tree` is gone from disk, all a reclaim or sweep line may claim. Only `NotFound`
fn is_gone(tree: &Path) -> bool {
    matches!(std::fs::symlink_metadata(tree), Err(e) if e.kind() == std::io::ErrorKind::NotFound)
}

/// The lease verdict on reclaiming `program`'s `build`: `Some(guard)` when no run holds it,
fn lease_clear(
    layout: &Layout,
    program: &str,
    build: u64,
    leased: &mut Vec<(String, u64, crate::lease::Holders)>,
) -> Option<Option<crate::lease::ReclaimGuard>> {
    let Some(subject) = crate::lease::Subject::build(program, build) else {
        return Some(None);
    };
    match crate::lease::reclaim(&layout.prefix, &subject) {
        crate::lease::Reclaim::Clear(guard) => Some(Some(guard)),
        crate::lease::Reclaim::Keep(holders) => {
            leased.push((program.to_string(), build, holders));
            None
        }
    }
}

/// Whether a live process runs from a build, asked before any build is reclaimed: a `codex`
struct InUse<'a> {
    layout: &'a Layout,
    enumerate: &'a dyn Fn() -> Option<Vec<PathBuf>>,
    running: Option<Option<Vec<PathBuf>>>,
}

impl InUse<'_> {
    /// `true` when `build_dir` of `program` (build number `build`, when it is one) must be
    fn holds(&mut self, program: &str, build_dir: &Path, build: Option<u64>) -> bool {
        let enumerate = self.enumerate;
        let Some(running) = self.running.get_or_insert_with(enumerate) else {
            // No process table: keep agent and trust builds.
            return crate::stub::is_agent_program(program) || program == crate::seam::SEAM_PROGRAM;
        };
        let root = build
            .filter(|_| program == crate::seam::SEAM_PROGRAM)
            .map(|b| crate::compat::root_dir(self.layout, b));
        let mut dirs = vec![build_dir.to_path_buf()];
        dirs.extend(root);
        let resolved: Vec<PathBuf> = dirs
            .iter()
            .filter_map(|d| std::fs::canonicalize(d).ok())
            .collect();
        if resolved.is_empty() && std::fs::symlink_metadata(build_dir).is_ok() {
            // there, but not resolvable: keep it
            return true;
        }
        running.iter().any(|exe| {
            dirs.iter()
                .chain(resolved.iter())
                .any(|d| exe.starts_with(d))
        })
    }
}

/// Whether a live process runs from under `dir` (either spelling: the kernel reports resolved paths).
#[must_use]
pub fn runs_from(dir: &Path) -> Option<bool> {
    running_from(dir, &running_executables).map(|exe| exe.is_some())
}

/// [`runs_from`]'s rule over an injected enumeration, naming the first executable under `dir`.
pub(crate) fn running_from(
    dir: &Path,
    running: &dyn Fn() -> Option<Vec<PathBuf>>,
) -> Option<Option<PathBuf>> {
    let running = running()?;
    Some(running_from_table(dir, &running))
}

/// Search an already-read process table, so several directories share one snapshot.
pub(crate) fn running_from_table(dir: &Path, running: &[PathBuf]) -> Option<PathBuf> {
    let resolved = std::fs::canonicalize(dir).ok();
    running
        .iter()
        .find(|exe| exe.starts_with(dir) || resolved.as_deref().is_some_and(|r| exe.starts_with(r)))
        .cloned()
}

/// The live process table ([`running_executables`]) under one ungated name, for another module to hand to [`running_from`].
pub(crate) fn process_table() -> Option<Vec<PathBuf>> {
    running_executables()
}

/// The executable path of every process alive now ([`running_processes`] without the
fn running_executables() -> Option<Vec<PathBuf>> {
    Some(
        running_process_table()?
            .into_iter()
            .map(|(_, exe)| exe)
            .collect(),
    )
}

/// Every process alive now as `(pid, executable path)`, by the kernel's path for each (so a relative, symlinked or PATH launch is seen, unlike `ps`'s argv[0]).
#[must_use]
pub fn running_processes() -> Option<Vec<(i32, PathBuf)>> {
    running_process_table()
}

/// The pid and executable path of every process alive now, from `proc_listallpids` +
#[cfg(target_os = "macos")]
fn running_process_table() -> Option<Vec<(i32, PathBuf)>> {
    use std::os::unix::ffi::OsStrExt as _;
    unsafe extern "C" {
        fn proc_listallpids(buffer: *mut libc::c_void, buffersize: libc::c_int) -> libc::c_int;
        fn proc_pidpath(
            pid: libc::c_int,
            buffer: *mut libc::c_void,
            buffersize: u32,
        ) -> libc::c_int;
    }
    /// `PROC_PIDPATHINFO_MAXSIZE` — `4 * MAXPATHLEN`.
    const PATH_MAX_BYTES: usize = 4 * 1024;
    /// A pid table this long is not a Mac; refuse rather than allocate for it.
    const MAX_PIDS: usize = 1 << 20;
    // SAFETY: a null buffer asks only for the number of pids; nothing is written.
    let estimate = unsafe { proc_listallpids(std::ptr::null_mut(), 0) };
    let mut capacity = usize::try_from(estimate).ok()?.saturating_add(64);
    let pids = loop {
        if capacity > MAX_PIDS {
            return None;
        }
        let mut pids: Vec<libc::c_int> = vec![0; capacity];
        let bytes = libc::c_int::try_from(capacity * std::mem::size_of::<libc::c_int>()).ok()?;
        // SAFETY: `pids` holds exactly `bytes` bytes and outlives the call, which writes at most that many and answers how many pids it wrote.
        let n = unsafe { proc_listallpids(pids.as_mut_ptr().cast(), bytes) };
        let n = usize::try_from(n).ok().filter(|&n| n > 0)?;
        if n < capacity {
            pids.truncate(n);
            break pids;
        }
        capacity = capacity.saturating_mul(2);
    };
    let mut buf = vec![0u8; PATH_MAX_BYTES];
    let len = u32::try_from(buf.len()).ok()?;
    let mut out = Vec::with_capacity(pids.len());
    for pid in pids.into_iter().filter(|&pid| pid > 0) {
        // SAFETY: `buf` holds `len` bytes and outlives the call, which writes at most that.
        let written = unsafe { proc_pidpath(pid, buf.as_mut_ptr().cast(), len) };
        match usize::try_from(written) {
            Ok(n) if n > 0 => {
                let path = buf.get(..n)?;
                out.push((pid, PathBuf::from(std::ffi::OsStr::from_bytes(path))));
            }
            _ => match std::io::Error::last_os_error().raw_os_error() {
                Some(libc::ESRCH | libc::ENOENT) => {}
                _ => return None,
            },
        }
    }
    Some(out)
}

/// The pid and executable path of every process alive now, from `/proc/<pid>/exe`
#[cfg(target_os = "linux")]
fn running_process_table() -> Option<Vec<(i32, PathBuf)>> {
    use std::os::unix::ffi::OsStrExt as _;
    let mut out = Vec::new();
    for entry in std::fs::read_dir("/proc").ok()? {
        let entry = entry.ok()?;
        let name = entry.file_name();
        if !name.as_bytes().iter().all(u8::is_ascii_digit) {
            continue;
        }
        let Some(pid) = name.to_str().and_then(|n| n.parse::<i32>().ok()) else {
            continue;
        };
        match std::fs::read_link(entry.path().join("exe")) {
            Ok(exe) => {
                let bytes = exe.as_os_str().as_bytes();
                let bytes = bytes.strip_suffix(b" (deleted)").unwrap_or(bytes);
                out.push((pid, PathBuf::from(std::ffi::OsStr::from_bytes(bytes))));
            }
            Err(e)
                if matches!(
                    e.kind(),
                    std::io::ErrorKind::NotFound | std::io::ErrorKind::PermissionDenied
                ) => {}
            Err(_) => return None,
        }
    }
    Some(out)
}

/// The pid and executable path of every process alive now, from `K32EnumProcesses` and
#[cfg(windows)]
fn running_process_table() -> Option<Vec<(i32, PathBuf)>> {
    use std::os::windows::ffi::OsStringExt as _;
    unsafe extern "system" {
        fn K32EnumProcesses(pids: *mut u32, bytes: u32, needed: *mut u32) -> i32;
        fn OpenProcess(access: u32, inherit: i32, pid: u32) -> *mut core::ffi::c_void;
        fn QueryFullProcessImageNameW(
            process: *mut core::ffi::c_void,
            flags: u32,
            name: *mut u16,
            len: *mut u32,
        ) -> i32;
        fn CloseHandle(handle: *mut core::ffi::c_void) -> i32;
    }
    const PROCESS_QUERY_LIMITED_INFORMATION: u32 = 0x1000;
    /// A pid table this long is not a desktop; refuse rather than allocate for it.
    const MAX_PIDS: usize = 1 << 20;
    /// The longest path an image name can be (`UNICODE_STRING`'s bound), in UTF-16 units.
    const PATH_MAX_UNITS: usize = 32_768;
    let mut capacity = 1024;
    let pids = loop {
        if capacity > MAX_PIDS {
            return None;
        }
        let mut pids: Vec<u32> = vec![0; capacity];
        let bytes = u32::try_from(capacity * std::mem::size_of::<u32>()).ok()?;
        let mut needed = 0;
        // SAFETY: `pids` holds exactly `bytes` bytes and outlives the call, which writes at most that many and answers in `needed` how many it wrote.
        if unsafe { K32EnumProcesses(pids.as_mut_ptr(), bytes, &mut needed) } == 0 {
            return None;
        }
        let n = usize::try_from(needed).ok()? / std::mem::size_of::<u32>();
        if n < capacity {
            pids.truncate(n);
            break pids;
        }
        capacity = capacity.saturating_mul(2);
    };
    let mut buf = vec![0u16; PATH_MAX_UNITS];
    let mut out = Vec::with_capacity(pids.len());
    for pid in pids.into_iter().filter(|&pid| pid != 0) {
        // SAFETY: an open by pid for a limited query; null is a failure.
        let process = unsafe { OpenProcess(PROCESS_QUERY_LIMITED_INFORMATION, 0, pid) };
        if process.is_null() {
            continue;
        }
        let mut len = u32::try_from(buf.len()).ok()?;
        // SAFETY: `buf` holds `len` UTF-16 units and outlives the call; `len` is a valid in/out-param.
        let named = unsafe { QueryFullProcessImageNameW(process, 0, buf.as_mut_ptr(), &mut len) };
        // SAFETY: the handle `OpenProcess` returned, closed exactly once.
        unsafe { CloseHandle(process) };
        let (Some(name), Ok(pid)) = (
            buf.get(..usize::try_from(len).ok()?).filter(|_| named != 0),
            i32::try_from(pid),
        ) else {
            continue;
        };
        let path = PathBuf::from(std::ffi::OsString::from_wide(name));
        out.push((pid, std::fs::canonicalize(&path).unwrap_or(path)));
    }
    Some(out)
}

/// No process table is read here, so whether a session runs from an agent build is
#[cfg(not(any(target_os = "macos", target_os = "linux", windows)))]
fn running_process_table() -> Option<Vec<(i32, PathBuf)>> {
    None
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A witness for the pure retention tests; `tests` is a child of `gc`, so it may build one.
    fn live(program: &str, build: u64) -> LiveBuild {
        LiveBuild {
            program: program.to_string(),
            build,
        }
    }

    /// The reconciliation table, over claim views handed in.
    #[test]
    fn live_from_claims_reconciles_the_two_views() {
        fn claims(pairs: &[(&str, &[u64])]) -> BTreeMap<String, BTreeSet<u64>> {
            pairs
                .iter()
                .map(|(p, b)| ((*p).to_string(), b.iter().copied().collect()))
                .collect()
        }
        let build_of = |set: &LiveSet| set.live.get("ay").map(|l| l.build);
        let link = |pairs: &[(&str, u64)]| -> BTreeMap<String, u64> {
            pairs.iter().map(|(p, b)| ((*p).to_string(), *b)).collect()
        };

        let set = live_from_claims(&link(&[("ay", 19)]), &claims(&[("ay", &[19])]));
        assert_eq!(build_of(&set), Some(19));
        assert!(set.diverged.is_empty());
        let set = live_from_claims(&link(&[("ay", 19)]), &claims(&[]));
        assert_eq!(build_of(&set), Some(19));
        assert!(set.diverged.is_empty());

        let set = live_from_claims(&link(&[("ay", 19)]), &claims(&[("ay", &[18])]));
        assert!(set.live.is_empty());
        assert_eq!(set.diverged.len(), 1);
        for (a, sh) in [
            (link(&[("ay", 19)]), claims(&[("ay", &[18, 19])])),
            (link(&[]), claims(&[("ay", &[19])])),
        ] {
            let set = live_from_claims(&a, &sh);
            assert!(set.live.is_empty(), "no witness from {a:?} / {sh:?}");
            assert_eq!(set.diverged.len(), 1);
        }
    }

    /// The retention rule with live build 19: it and one rollback below it are kept; the rest,
    #[test]
    fn reclaimable_keeps_current_plus_one_rollback() {
        let rows: [(&[u64], &[u64], &str); 4] = [
            (
                &[16, 17, 18, 19],
                &[16, 17],
                "live 19, rollback 18: the older 16, 17 go",
            ),
            (&[19], &[], "a single installed build that is live is kept"),
            (
                &[19, 20, 21],
                &[20, 21],
                "no rollback below the live build: only it stays",
            ),
            (
                &[18, 16, 19, 17, 16],
                &[16, 17],
                "duplicates and unsorted input",
            ),
        ];
        for (installed, reclaimed, why) in rows {
            let got = reclaimable(installed, &live("ay", 19), &|_| true);
            assert_eq!(got, reclaimed, "{why}");
            assert!(
                !got.contains(&19),
                "the live build is never reclaimed: {why}"
            );
        }
    }

    use crate::activate::{activate_build, install_shims};

    fn tool(name: &str) -> crate::store::ToolName {
        crate::store::ToolName::new(name).unwrap()
    }

    fn layout(label: &str) -> crate::testdir::ScratchLayout {
        crate::testdir::layout(&format!("atpkg-gc-{label}-"))
    }

    /// A complete (marked) build dir with `bin/<program>`; `shim` also installs its shims and
    fn seed(layout: &Layout, program: &str, build: u64, shim: bool) -> PathBuf {
        let dir = layout.build_dir(program, build);
        std::fs::create_dir_all(dir.join("bin")).unwrap();
        std::fs::write(
            dir.join("bin").join(tool(program).exe_file()),
            b"#!/bin/true\n",
        )
        .unwrap();
        if shim {
            install_shims(
                layout,
                &dir,
                &[program.to_string()],
                crate::activate::Aliases::Off,
            )
            .unwrap();
            activate_build(layout, &dir).unwrap();
        }
        crate::store::mark_build_ready(&dir).unwrap();
        dir
    }

    /// A populated build dir with no completeness marker: what an install killed between its
    fn seed_partial(layout: &Layout, program: &str, build: u64) -> PathBuf {
        let dir = layout.build_dir(program, build);
        std::fs::create_dir_all(dir.join("bin")).unwrap();
        std::fs::write(
            dir.join("bin").join(tool(program).exe_file()),
            b"#!/bin/true\n",
        )
        .unwrap();
        assert!(
            !crate::store::build_is_complete(&dir),
            "the fixture is only interesting while it is marker-less"
        );
        dir
    }

    /// Stage scratch beside a build: the `<build>.incoming-<pid>` / `<build>.superseded-<pid>`
    fn seed_scratch(layout: &Layout, program: &str, name: &str) -> PathBuf {
        let dir = layout.prefix.join("store").join(program).join(name);
        std::fs::create_dir_all(dir.join("bin")).unwrap();
        std::fs::write(dir.join("bin").join("payload"), b"half an extract\n").unwrap();
        dir
    }

    #[test]
    fn run_keeps_current_and_rollback_reclaims_the_rest() {
        let l = layout("run-keeps");
        for b in [16u64, 17, 18] {
            seed(&l, "ay", b, false);
        }
        seed(&l, "ay", 19, true);
        let report = run(&l, None);
        assert_eq!(report.reclaimed, vec![("ay".to_string(), vec![16u64, 17])]);
        assert!(report.diverged.is_empty(), "the two views agree");
        for gone in [16u64, 17] {
            assert!(!l.build_dir("ay", gone).exists(), "{gone} reclaimed");
        }
        for keep in [18u64, 19] {
            assert!(l.build_dir("ay", keep).exists(), "{keep} survives");
        }
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt as _;
            let stuck = seed(&l, "ay", 15, false);
            let mode = |m| {
                for d in [stuck.join("bin"), stuck.clone()] {
                    std::fs::set_permissions(&d, std::fs::Permissions::from_mode(m)).unwrap();
                }
            };
            mode(0o500);
            let report = run(&l, None);
            mode(0o755);
            assert!(
                stuck.join("bin").join(tool("ay").exe_file()).exists(),
                "the fixture's payload could not be removed"
            );
            assert!(report.reclaimed.is_empty(), "{:?}", report.reclaimed);
        }
        let _ = std::fs::remove_dir_all(&l.prefix);
    }

    #[test]
    fn run_skips_a_program_with_no_active_build() {
        let l = layout("run-noactive");
        seed(&l, "ay", 17, false);
        seed(&l, "ay", 18, false);
        let report = run(&l, None);
        assert!(
            report.reclaimed.is_empty(),
            "no witness => nothing reclaimed"
        );
        assert!(report.diverged.is_empty(), "unclaimed is not diverged");
        assert!(l.build_dir("ay", 17).exists());
        assert!(l.build_dir("ay", 18).exists());
        let _ = std::fs::remove_dir_all(&l.prefix);
    }

    /// A shim left on an older build must not let retention delete the build `current`
    #[test]
    fn a_stale_shim_never_deletes_the_linked_live_build() {
        let l = layout("stale-shim-brick");
        seed(&l, "ay", 18, false);
        let b19 = seed(&l, "ay", 19, false);
        activate_build(&l, &b19).unwrap();
        crate::platform::ensure_private_dir(&l.bin_dir()).unwrap();
        let ay = tool("ay");
        crate::platform::install_shim(&l.build_dir("ay", 18).join("bin"), &ay, &l.shim(&ay))
            .unwrap();

        let report = run(&l, None);
        assert!(
            report.reclaimed.is_empty(),
            "a contested program is never reclaimed"
        );
        assert!(
            l.build_dir("ay", 19).exists(),
            "the link's LIVE build must survive a stale shim"
        );
        assert!(l.build_dir("ay", 18).exists(), "and so must the shims'");
        assert_eq!(
            report.diverged,
            vec![Divergence {
                program: "ay".to_string(),
                reason: Diverged::LinkShimMismatch {
                    link_says: 19,
                    shims_say: 18,
                },
            }]
        );
        let _ = std::fs::remove_dir_all(&l.prefix);
    }

    /// One program's tools split across two builds (an `install_shims` loop that failed
    #[test]
    fn tools_of_one_program_split_across_builds_yield_no_witness() {
        let l = layout("split-shims");
        let b18 = seed(&l, "ay", 18, false);
        let b19 = seed(&l, "ay", 19, false);
        activate_build(&l, &b19).unwrap();
        install_shims(&l, &b19, &["ay".to_string()], crate::activate::Aliases::Off).unwrap();
        let aylint = tool("aylint");
        crate::platform::install_shim(&b18.join("bin"), &aylint, &l.shim(&aylint)).unwrap();

        let report = run(&l, None);
        assert!(report.reclaimed.is_empty());
        assert!(l.build_dir("ay", 18).exists() && l.build_dir("ay", 19).exists());
        assert_eq!(
            report.diverged,
            vec![Divergence {
                program: "ay".to_string(),
                reason: Diverged::ShimsDisagree {
                    builds: vec![18, 19],
                },
            }]
        );
        let _ = std::fs::remove_dir_all(&l.prefix);
    }

    /// An authority that cannot be resolved (dangling, out-of-store, absent) gives no witness,
    #[cfg(unix)]
    #[test]
    fn a_broken_authority_skips_the_program_instead_of_reclaiming() {
        let l = layout("bad-channel");
        seed(&l, "ay", 17, false);
        seed(&l, "ay", 18, false);
        seed(&l, "ay", 19, true);
        let links = [l.program_current("ay")];

        for link in &links {
            crate::activate::atomic_symlink(&l.build_dir("ay", 77), link).unwrap();
        }
        let report = run(&l, None);
        assert!(
            report.reclaimed.is_empty(),
            "a dangling authority reclaims nothing"
        );
        assert_eq!(
            report.diverged,
            vec![Divergence {
                program: "ay".to_string(),
                reason: Diverged::NoLiveWitness { shims_say: 19 },
            }]
        );

        let outside = l.prefix.join("elsewhere").join("ay").join("19");
        std::fs::create_dir_all(&outside).unwrap();
        for link in &links {
            crate::activate::atomic_symlink(&outside, link).unwrap();
        }
        assert!(
            run(&l, None).reclaimed.is_empty(),
            "an out-of-store authority proves nothing"
        );

        std::fs::remove_file(l.program_current("ay")).unwrap();
        assert!(
            run(&l, None).reclaimed.is_empty(),
            "no links => no witnesses"
        );

        for b in [17u64, 18, 19] {
            assert!(l.build_dir("ay", b).exists(), "{b} must survive all three");
        }
        let _ = std::fs::remove_dir_all(&l.prefix);
    }

    /// Installed and shimmed but never activated: skipped, and reported, or the program is
    #[test]
    fn a_program_that_was_never_activated_is_reported_diverged() {
        let l = layout("no-channel");
        seed(&l, "ay", 17, false);
        let b18 = seed(&l, "ay", 18, false);
        install_shims(&l, &b18, &["ay".to_string()], crate::activate::Aliases::Off).unwrap();

        let report = run(&l, None);
        assert!(
            report.reclaimed.is_empty(),
            "no witness => nothing reclaimed"
        );
        assert!(l.build_dir("ay", 17).exists());
        assert_eq!(
            report.diverged,
            vec![Divergence {
                program: "ay".to_string(),
                reason: Diverged::NoLiveWitness { shims_say: 18 },
            }]
        );
        let _ = std::fs::remove_dir_all(&l.prefix);
    }

    /// One `current` link per program, so activating `ny` leaves `ay` witnessed and its
    #[test]
    fn two_programs_on_one_channel_are_both_reclaimed() {
        let l = layout("two-progs");
        for b in [16u64, 17, 18] {
            seed(&l, "ay", b, false);
        }
        seed(&l, "ay", 19, true);
        seed(&l, "ny", 6, false);
        seed(&l, "ny", 7, true);

        let report = run(&l, None);
        assert_eq!(
            report.reclaimed,
            vec![("ay".to_string(), vec![16u64, 17])],
            "ay must still be witnessed after ny was activated"
        );
        assert!(
            report.diverged.is_empty(),
            "neither program is contested: {:?}",
            report.diverged
        );
        for keep in [("ay", 18u64), ("ay", 19), ("ny", 6), ("ny", 7)] {
            assert!(
                l.build_dir(keep.0, keep.1).exists(),
                "{}@{} survives",
                keep.0,
                keep.1
            );
        }
        let _ = std::fs::remove_dir_all(&l.prefix);
    }

    /// Remove a `current` link whatever shape the platform gave it (a symlink, or a junction
    fn unlink_current(link: &Path) {
        let _ = std::fs::remove_file(link);
        let _ = std::fs::remove_dir(link);
        assert!(
            std::fs::symlink_metadata(link).is_err(),
            "{} must be gone for the pre-migration fixture",
            link.display()
        );
    }

    /// A retired `channels/<c>/current` is swept (the link, never what it names) and proves
    #[test]
    fn a_retired_channel_link_is_swept_and_is_never_a_witness() {
        let l = layout("legacy-channel");
        seed(&l, "ay", 17, false);
        seed(&l, "ay", 18, false);
        let b19 = seed(&l, "ay", 19, true);
        unlink_current(&l.program_current("ay"));
        let legacy = l.prefix.join("channels").join("stable");
        std::fs::create_dir_all(&legacy).unwrap();
        crate::activate::atomic_symlink(&b19, &legacy.join("current")).unwrap();

        let report = run(&l, None);
        assert!(
            report.reclaimed.is_empty(),
            "the retired link is not an authority: {:?}",
            report.reclaimed
        );
        assert_eq!(
            report.diverged,
            vec![Divergence {
                program: "ay".to_string(),
                reason: Diverged::NoLiveWitness { shims_say: 19 },
            }]
        );
        assert!(
            std::fs::symlink_metadata(l.prefix.join("channels")).is_err(),
            "the retired directory is swept"
        );
        for b in [17u64, 18, 19] {
            assert!(
                l.build_dir("ay", b).is_dir(),
                "{b} is untouched by the sweep"
            );
        }
        activate_build(&l, &b19).unwrap();
        assert_eq!(
            run(&l, None).reclaimed,
            vec![("ay".to_string(), vec![17u64])]
        );
        let _ = std::fs::remove_dir_all(&l.prefix);
    }

    /// The sweep removes only what the retired layout put there: a `channels/<c>/` that
    #[test]
    fn the_channel_sweep_leaves_what_it_did_not_write() {
        let l = layout("legacy-channel-foreign");
        let b19 = seed(&l, "ay", 19, true);
        let beta = l.prefix.join("channels").join("beta");
        std::fs::create_dir_all(&beta).unwrap();
        crate::activate::atomic_symlink(&b19, &beta.join("current")).unwrap();
        std::fs::write(beta.join("notes.txt"), b"mine\n").unwrap();
        sweep_retired_channel_links(&l);
        assert!(std::fs::symlink_metadata(beta.join("current")).is_err());
        assert!(beta.join("notes.txt").is_file(), "a foreign file survives");
        assert!(b19.is_dir());
        let _ = std::fs::remove_dir_all(&l.prefix);
    }

    /// The shim view uses the anchored containment test, so a dev-link into
    #[cfg(unix)]
    #[test]
    fn a_shim_into_a_lookalike_store_outside_the_prefix_claims_nothing() {
        let l = layout("foreign-store-claim");
        seed(&l, "ay", 17, false);
        seed(&l, "ay", 18, false);
        seed(&l, "ay", 19, true);

        let devco = l
            .prefix
            .parent()
            .unwrap()
            .join(format!("atpkg-gc-devco-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&devco);
        let checkout = devco.join("store/ay/18/bin");
        std::fs::create_dir_all(&checkout).unwrap();
        std::fs::write(checkout.join("aydev"), b"#!/bin/true\n").unwrap();
        let aydev = tool("aydev");
        std::os::unix::fs::symlink(checkout.join("aydev"), l.shim(&aydev)).unwrap();

        let report = run(&l, None);
        assert_eq!(
            report.reclaimed,
            vec![("ay".to_string(), vec![17u64])],
            "a target outside <prefix>/store is not a claim about ay"
        );
        assert!(report.diverged.is_empty(), "{:?}", report.diverged);
        assert!(l.build_dir("ay", 18).exists() && l.build_dir("ay", 19).exists());
        let _ = std::fs::remove_dir_all(&devco);
        let _ = std::fs::remove_dir_all(&l.prefix);
    }

    /// A tree an install killed mid-extract is invisible to `list_installed`; the sweep takes
    #[test]
    fn an_unclaimed_partial_tree_is_swept_and_never_counted_as_a_reclaim() {
        let l = layout("sweep-partial");
        for b in [16u64, 17, 18] {
            seed(&l, "ay", b, false);
        }
        seed(&l, "ay", 19, true);
        let partial = seed_partial(&l, "ay", 20);

        let report = run(&l, None);
        assert!(!partial.exists(), "the interrupted tree must be reclaimed");
        assert_eq!(report.swept_partial, vec![("ay".to_string(), vec![20u64])]);
        assert_eq!(
            report.reclaimed,
            vec![("ay".to_string(), vec![16u64, 17])],
            "an ordinary reclaim must not absorb the sweep"
        );
        assert!(report.diverged.is_empty(), "{:?}", report.diverged);
        for keep in [18u64, 19] {
            assert!(l.build_dir("ay", keep).exists(), "{keep} survives");
        }
        let _ = std::fs::remove_dir_all(&l.prefix);
    }

    /// A first install killed mid-extract has no live build, shim or `current` link; the sweep
    #[test]
    fn an_interrupted_fresh_install_is_swept_although_nothing_is_live() {
        let l = layout("sweep-fresh");
        let partial = seed_partial(&l, "ny", 7);

        let report = run(&l, None);
        assert!(
            !partial.exists(),
            "a never-activated program leaks disk too"
        );
        assert_eq!(report.swept_partial, vec![("ny".to_string(), vec![7u64])]);
        assert!(report.reclaimed.is_empty());
        assert!(
            report.diverged.is_empty(),
            "a program that never finished installing is not a disagreement"
        );
        let _ = std::fs::remove_dir_all(&l.prefix);
    }

    /// A live build can lose its sibling marker and keep its tree; the claim
    #[test]
    fn a_live_build_that_lost_its_marker_is_claimed_and_never_swept() {
        let l = layout("marker-less-live");
        seed(&l, "ay", 18, false);
        let b19 = seed(&l, "ay", 19, true);
        crate::store::clear_build_ready(&b19).unwrap();
        assert!(!crate::store::build_is_complete(&b19));

        let report = run(&l, None);
        assert!(
            report.swept_partial.is_empty(),
            "a claimed tree is not debris: {:?}",
            report.swept_partial
        );
        assert!(b19.exists(), "the live tree must survive losing its marker");
        assert!(l.build_dir("ay", 18).exists(), "and so must its rollback");
        let _ = std::fs::remove_dir_all(&l.prefix);
    }

    /// …and from the side no witness could guard: the views disagree, so there is no
    #[test]
    fn a_marker_less_build_the_shims_still_run_is_never_swept() {
        let l = layout("marker-less-contested");
        let b18 = seed(&l, "ay", 18, false);
        let b19 = seed(&l, "ay", 19, false);
        activate_build(&l, &b19).unwrap();
        crate::platform::ensure_private_dir(&l.bin_dir()).unwrap();
        let ay = tool("ay");
        crate::platform::install_shim(&b18.join("bin"), &ay, &l.shim(&ay)).unwrap();
        crate::store::clear_build_ready(&b18).unwrap();

        let report = run(&l, None);
        assert!(
            report.swept_partial.is_empty(),
            "the shims' claim is a claim: {:?}",
            report.swept_partial
        );
        assert!(b18.exists(), "the tree on PATH must survive");
        assert!(b19.exists(), "and so must the one the link selects");
        let _ = std::fs::remove_dir_all(&l.prefix);
    }

    /// Stage scratch a killed install left is swept, reported by name: a
    #[test]
    fn stage_scratch_of_a_killed_install_is_swept_by_name_not_by_build() {
        let l = layout("sweep-scratch");
        seed(&l, "ay", 18, false);
        seed(&l, "ay", 19, true);
        let incoming = seed_scratch(&l, "ay", "20.incoming-4242");
        let superseded = seed_scratch(&l, "ay", "19.superseded-4242");

        let report = run(&l, None);
        assert!(!incoming.exists() && !superseded.exists(), "scratch swept");
        assert_eq!(
            report.swept_scratch,
            vec![(
                "ay".to_string(),
                vec![
                    "19.superseded-4242".to_string(),
                    "20.incoming-4242".to_string(),
                ]
            )]
        );
        assert!(report.swept_partial.is_empty(), "scratch is not a build");
        assert!(
            l.build_dir("ay", 19).exists(),
            "the live build survives the sweep of its own scratch"
        );
        assert!(l.build_dir("ay", 18).exists());
        let _ = std::fs::remove_dir_all(&l.prefix);
    }

    /// The scratch test licenses a `remove_dir_all` in a directory the user can write, so it
    #[test]
    fn only_the_producers_own_scratch_shape_is_sweepable() {
        for ours in [
            "18.incoming-1",
            "18.superseded-4242",
            "0.incoming-999999",
            "18446744073709551615.incoming-7",
        ] {
            assert!(is_stage_scratch(ours), "{ours} is ours");
        }
        for theirs in [
            "18",
            "18.ready",
            "notes.incoming-drafts",
            "18.incoming-",
            "18.incoming-4242x",
            "ay.incoming-4242",
            ".incoming-4242",
            "18.pending-4242",
            "18incoming-4242",
            "18.incoming-42.42",
            "+18.incoming-1",
            "018.incoming-1",
            "+0.incoming-1",
            "0018.superseded-4242",
        ] {
            assert!(!is_stage_scratch(theirs), "{theirs} is NOT ours to delete");
        }
    }

    /// A directory that merely looks scratch-ish survives a real pass: `run` consults the
    #[test]
    fn a_lookalike_directory_survives_a_gc_pass() {
        let l = layout("sweep-lookalike");
        seed(&l, "ay", 18, false);
        seed(&l, "ay", 19, true);
        let theirs = seed_scratch(&l, "ay", "notes.incoming-drafts");
        let plus_scratch = seed_scratch(&l, "ay", "+20.incoming-4242");
        let plus_build = seed_scratch(&l, "ay", "+21");
        let ours = seed_scratch(&l, "ay", "20.incoming-4242");

        let report = run(&l, None);
        assert!(!ours.exists(), "our own scratch is still swept");
        assert!(theirs.exists(), "a user directory is not ours to delete");
        assert!(
            plus_scratch.exists(),
            "`+20.incoming-4242` is not a name this manager ever wrote"
        );
        assert!(
            plus_build.exists(),
            "`+21` is not a build directory this manager ever wrote"
        );
        assert_eq!(
            report.swept_scratch,
            vec![("ay".to_string(), vec!["20.incoming-4242".to_string()])]
        );
        let _ = std::fs::remove_dir_all(&l.prefix);
    }

    /// A marker-bearing lookalike must not displace the rollback target: read by a bare
    #[test]
    fn a_marker_bearing_lookalike_cannot_displace_the_rollback_target() {
        let l = layout("lookalike-rollback-slot");
        let real_rollback = seed(&l, "ay", 17, false);
        seed(&l, "ay", 25, true);
        let lookalike = l.prefix.join("store").join("ay").join("+19");
        std::fs::create_dir_all(lookalike.join("bin")).unwrap();
        std::fs::write(
            lookalike.join("bin").join(tool("ay").exe_file()),
            b"#!/bin/true\n",
        )
        .unwrap();
        crate::store::mark_build_ready(&lookalike).unwrap();
        assert!(
            crate::store::build_is_complete(&lookalike),
            "the fixture must look every bit as complete as a real build, or it proves nothing"
        );

        let report = run(&l, None);

        assert!(
            real_rollback.exists(),
            "the real rollback target was reclaimed to make room for a build number that \
             never existed: {:?}",
            report.reclaimed
        );
        assert!(
            report.reclaimed.is_empty(),
            "nothing here is reclaimable: {:?}",
            report.reclaimed
        );
        assert!(
            lookalike.exists(),
            "and a directory this manager never wrote is not its to delete"
        );
        assert!(l.build_dir("ay", 25).exists(), "the live build survives");
        let _ = std::fs::remove_dir_all(&l.prefix);
    }

    /// Park a build's tree at `<build>.superseded-<pid>` with nothing at `<build>`: what a
    fn seed_killed_mid_swap(l: &Layout, program: &str, build: u64) -> (PathBuf, PathBuf) {
        let dir = l.build_dir(program, build);
        let parked = dir.with_file_name(format!("{build}.superseded-4242"));
        crate::store::clear_build_ready(&dir).unwrap();
        std::fs::rename(&dir, &parked).unwrap();
        assert!(
            !dir.exists() && parked.is_dir(),
            "the fixture is only interesting in the two-rename window"
        );
        (dir, parked)
    }

    /// GC run from the other slice must not delete the native install: `build_is_complete`'s
    #[test]
    fn a_build_the_other_slice_installed_is_not_an_interrupted_install() {
        let l = layout("other-slice");
        seed(&l, "ay", 18, false);
        seed(&l, "ay", 19, true);
        let marker = l.prefix.join("store").join("ay").join("18.ready");
        std::fs::write(&marker, b"ok\nplatform=sparc64-solaris\n").unwrap();
        assert!(
            !crate::store::build_is_complete(&l.build_dir("ay", 18)),
            "PRECONDITION: this slice reads it as not-complete and re-stages it"
        );

        let report = run(&l, None);

        assert!(
            l.build_dir("ay", 18).join("bin").exists(),
            "a finished install of the other slice is not an interrupted one"
        );
        assert!(
            report.swept_partial.is_empty(),
            "and it must not be reported as reclaimed debris: {:?}",
            report.swept_partial
        );

        let partial = seed_partial(&l, "ay", 17);
        let report = run(&l, None);
        assert!(
            !partial.exists(),
            "a real interrupted install is still reclaimed"
        );
        assert_eq!(report.swept_partial, vec![("ay".to_string(), vec![17u64])]);
        let _ = std::fs::remove_dir_all(&l.prefix);
    }

    /// A crash in the swap window is recovered, not swept: between `rename(build,
    #[test]
    fn a_tree_parked_by_a_killed_swap_is_moved_back_not_swept() {
        let l = layout("swap-window-recover");
        seed(&l, "ay", 18, false);
        seed(&l, "ay", 19, true);
        let (build19, parked) = seed_killed_mid_swap(&l, "ay", 19);

        let report = run(&l, None);

        assert!(
            build19.is_dir(),
            "the only copy of the live tree was swept as scratch"
        );
        assert!(
            build19.join("bin").join(tool("ay").exe_file()).exists(),
            "and it is THAT tree, not an empty shell"
        );
        assert!(!parked.exists(), "the scratch name is released");
        assert!(
            report.swept_scratch.is_empty(),
            "a recovered tree must not be reported as reclaimed disk: {:?}",
            report.swept_scratch
        );
        assert!(
            report.swept_partial.is_empty(),
            "and the claim guard must keep the recovered tree: {:?}",
            report.swept_partial
        );
        assert!(
            !crate::store::build_is_complete(&build19),
            "recovery must never claim a completeness it cannot prove"
        );
        assert!(l.build_dir("ay", 18).exists(), "and 18 is untouched");
        let _ = std::fs::remove_dir_all(&l.prefix);
    }

    /// A recovered tree nothing claims is an ordinary orphan, swept as a partial under the
    #[test]
    fn a_recovered_tree_nothing_claims_is_still_swept_as_a_partial() {
        let l = layout("swap-window-orphan");
        seed(&l, "ay", 19, true);
        seed(&l, "ny", 7, false);
        let (build7, parked) = seed_killed_mid_swap(&l, "ny", 7);

        let report = run(&l, None);

        assert!(!parked.exists(), "the scratch name is released either way");
        assert!(!build7.exists(), "an unclaimed orphan is still reclaimed");
        assert_eq!(
            report.swept_partial,
            vec![("ny".to_string(), vec![7u64])],
            "and it is reported as a partial, not as scratch"
        );
        assert!(report.swept_scratch.is_empty());
        let _ = std::fs::remove_dir_all(&l.prefix);
    }

    /// Two superseded siblings with `<build>` absent: recovery cannot tell which is the
    #[test]
    fn an_ambiguous_pair_with_the_build_absent_is_reported_and_left_standing() {
        let l = layout("swap-window-ambiguous");
        seed(&l, "ay", 19, true);
        let (build19, first) = seed_killed_mid_swap(&l, "ay", 19);
        let second = seed_scratch(&l, "ay", "19.superseded-9999");
        let incoming = seed_scratch(&l, "ay", "19.incoming-4242");

        let report = run(&l, None);

        assert!(
            !build19.exists(),
            "an ambiguous window must not be guessed at"
        );
        assert!(
            first.is_dir() && second.is_dir(),
            "the only copies of build 19 went to the sweep the refusal called off"
        );
        assert!(
            first.join("bin").join(tool("ay").exe_file()).exists(),
            "and the parked tree is intact, not an emptied shell"
        );
        assert!(!incoming.exists(), "a half-extract is still swept");
        assert_eq!(
            report.swept_scratch,
            vec![("ay".to_string(), vec!["19.incoming-4242".to_string()])],
            "a tree still standing must not be reported as reclaimed disk"
        );
        assert!(
            report.diverged.iter().any(|d| d.program == "ay"),
            "and the state IS reported, not silently kept: {:?}",
            report.diverged
        );
        let _ = std::fs::remove_dir_all(&l.prefix);
    }

    /// The destructive entry point refuses the witness's own build itself.
    #[test]
    fn discard_superseded_refuses_the_live_build() {
        let l = layout("refuse-live");
        seed(&l, "ay", 18, false);
        seed(&l, "ay", 19, true);
        let set = live_builds(&l);
        let witness = set.get("ay").expect("link + shims agree on 19").clone();
        assert_eq!((witness.program(), witness.build()), ("ay", 19));

        assert_eq!(
            discard_superseded(&l, &witness, 19),
            Err(Retained::IsLive {
                program: "ay".to_string(),
                build: 19,
            })
        );
        assert!(l.build_dir("ay", 19).exists(), "the live tree is untouched");
        assert_eq!(discard_superseded(&l, &witness, 18), Ok(()));
        assert!(!l.build_dir("ay", 18).exists());
        let _ = std::fs::remove_dir_all(&l.prefix);
    }

    /// The scoped form touches only its programs; the whole-prefix form still takes the rest.
    #[test]
    fn the_scoped_sweep_touches_only_its_programs() {
        let l = layout("scoped");
        for b in [16, 17] {
            seed(&l, "ay", b, false);
        }
        seed(&l, "ay", 18, true);
        for b in [4, 5] {
            seed(&l, "claude", b, false);
        }
        seed(&l, "claude", 6, true);
        let other_partial = seed_partial(&l, "ay", 19);
        let ay_staging = l.staging_dir("ay");
        let claude_staging = l.staging_dir("claude");
        std::fs::create_dir_all(&ay_staging).unwrap();
        std::fs::create_dir_all(&claude_staging).unwrap();
        let resume = ay_staging.join("ay-20.tar.zst.part");
        let carried = ay_staging.join("ay-20.tar.zst");
        let stale = claude_staging.join("claude-5.part");
        let spared = claude_staging.join("claude-7.part");
        for p in [&resume, &carried, &stale, &spared] {
            std::fs::write(p, b"x").unwrap();
        }
        let scope = BTreeSet::from([String::from("claude")]);
        let report = run_for_programs(&l, &scope, &|program| {
            (program == "claude").then(|| String::from("claude-7"))
        });
        assert_eq!(report.reclaimed, vec![(String::from("claude"), vec![4])]);
        assert!(
            resume.exists() && carried.exists(),
            "ay's staging is not ours"
        );
        assert!(l.build_dir("ay", 16).exists(), "ay's builds are not ours");
        assert!(other_partial.exists(), "nor its debris");
        assert!(!stale.exists(), "claude's superseded partial is swept");
        assert!(spared.exists(), "claude's resolved partial is spared");
        assert_eq!(
            report.swept_staging,
            vec![(String::from("claude"), vec![String::from("claude-5.part")])]
        );
        let report = run(&l, None);
        assert!(!resume.exists() && !l.build_dir("ay", 16).exists());
        assert!(!other_partial.exists());
        assert!(report.reclaimed.contains(&(String::from("ay"), vec![16])));
        let _ = std::fs::remove_dir_all(&l.prefix);
    }

    /// The pass-closing sweep spares exactly the pinned artifact's `.part` and complete
    #[test]
    fn the_pass_closing_sweep_spares_the_live_pinned_archive_and_partial_only() {
        let l = layout("staging-keep");
        let staging = l.staging_dir("ay");
        std::fs::create_dir_all(&staging).unwrap();
        let pinned = staging.join("ay-18.tar.zst.part");
        let carried = staging.join("ay-18.tar.zst");
        let superseded = staging.join("ay-17.tar.zst.part");
        let debris = staging.join("ay-17.tar.zst");
        let other = l.staging_dir("ny");
        std::fs::create_dir_all(&other).unwrap();
        let unpinned = other.join("ny-7.tar.zst.part");
        for p in [&pinned, &carried, &superseded, &debris, &unpinned] {
            std::fs::write(p, b"x").unwrap();
        }

        let report = run_keeping_pinned_partials(&l, None, &|program| {
            (program == "ay").then(|| "ay-18.tar.zst".to_string())
        });

        assert!(
            pinned.exists(),
            "the pinned partial is the next pass's resume state"
        );
        assert!(
            carried.exists(),
            "the pinned COMPLETE archive is an aborted tuple's carry — the next pass \
             re-stages that member from it rather than re-downloading a multi-GB asset"
        );
        assert!(!superseded.exists(), "a superseded partial is still swept");
        assert!(
            !debris.exists(),
            "a SUPERSEDED complete asset is still swept — only the pinned one is spared"
        );
        assert!(
            !unpinned.exists(),
            "a program the caller pins nothing for is fully swept"
        );
        assert_eq!(
            report.swept_staging,
            vec![
                (
                    "ay".to_string(),
                    vec![
                        "ay-17.tar.zst".to_string(),
                        "ay-17.tar.zst.part".to_string()
                    ]
                ),
                ("ny".to_string(), vec!["ny-7.tar.zst.part".to_string()]),
            ],
            "the report lists what went, never what was spared"
        );

        let _ = run(&l, None);
        assert!(
            !pinned.exists(),
            "a plain `gc` still reclaims the spared partial"
        );
        assert!(
            !carried.exists(),
            "…and the spared archive: an aborted tuple never permanently strands bytes"
        );
        let _ = std::fs::remove_dir_all(&l.prefix);
    }

    /// A pass that did not read a vendor program's head still spares the `.part` and carried
    #[test]
    fn an_unread_vendor_head_keeps_the_resume_state_of_the_head_it_pursues() {
        use crate::vendor_direct::{ProgramStamp, Version, spec};
        let l = layout("pursued-head");
        let v = |text: &str| Version::parse(text).unwrap();
        let asset = |version| {
            let triple = crate::cli::current_triple();
            crate::vendor_direct::resolve::staging_asset(spec("muse").unwrap(), version, triple)
        };
        let head = v("1.4.9-R6299");
        let staging = l.staging_dir("muse");
        std::fs::create_dir_all(&staging).unwrap();
        let part = staging.join(format!("{}.part", asset(head)));
        let carried = staging.join(asset(head));
        let stale = format!("{}.part", asset(v("1.4.9-R6298")));
        for p in [&part, &carried, &staging.join(&stale)] {
            std::fs::write(p, b"x").unwrap();
        }
        ProgramStamp::record_check(&l, "muse", 1_790_000_000, Some(head), None).unwrap();
        let muse = BTreeSet::from(["muse".to_string()]);

        let report = run_keeping_pinned_partials(&l, None, &|_| None);
        assert!(part.exists(), "the pursued head's resume state survives");
        assert!(carried.exists(), "…and its carried archive");
        assert_eq!(
            report.swept_staging,
            vec![("muse".to_string(), vec![stale])]
        );
        let _ = run_for_programs(&l, &muse, &|_| None);
        assert!(
            part.exists() && carried.exists(),
            "the scoped form spares them too"
        );

        seed(&l, "muse", head.build_id(), true);
        let _ = run_keeping_pinned_partials(&l, None, &|_| None);
        assert!(!part.exists(), "an installed head pursues nothing");
        assert!(!carried.exists());
        let _ = std::fs::remove_dir_all(&l.prefix);
    }

    /// The archive a held toolchain flip waits on survives an install's pass-end sweep and a
    #[test]
    fn a_held_flips_staged_archive_survives_every_gc_while_the_wait_stands() {
        let l = layout("held-archive");
        seed(&l, "trust", 9192, true);
        let staging = l.staging_dir("trust");
        std::fs::create_dir_all(&staging).unwrap();
        let held = staging.join("trust-9200.tar.zst");
        let stray = staging.join("trust-9100.tar.zst");
        for p in [&held, &stray] {
            std::fs::write(p, b"x").unwrap();
        }
        assert!(crate::quiet::note_deferred(
            &l,
            "rustc",
            1_790_000_000,
            1_790_000_000,
            &crate::quiet::Busy::Quiet,
            &crate::quiet::Moves {
                from: BTreeMap::from([("trust".to_string(), 9192u64)]),
                to: BTreeMap::from([("trust".to_string(), 9200u64)]),
                assets: BTreeMap::from([("trust".to_string(), "trust-9200.tar.zst".to_string())]),
            },
        ));
        let report = run_keeping_pinned_partials(&l, None, &|program| {
            (program == "ay").then(|| "ay-18.tar.zst".to_string())
        });
        assert!(held.exists(), "an install's sweep keeps the held archive");
        assert!(!stray.exists(), "…and takes the rest");
        assert_eq!(
            report.held_staging,
            vec![("trust".to_string(), "trust-9200.tar.zst".to_string())],
            "and says what it kept"
        );
        let report = run(&l, None);
        assert!(held.exists(), "a plain gc keeps the held archive");
        assert_eq!(report.held_staging.len(), 1);
        crate::ops::uninstall(&l, "trust").unwrap();
        assert!(!l.build_dir("trust", 9192).exists());
        let report = run(&l, None);
        assert!(!held.exists(), "a stale record keeps nothing");
        assert!(report.held_staging.is_empty());
        let _ = std::fs::remove_dir_all(&l.prefix);
    }

    /// `codex` live at 20 with 18 and 19 below it: under the plain rule 18 goes.
    fn codex_with_a_reclaimable_build(label: &str) -> crate::testdir::ScratchLayout {
        let l = layout(label);
        seed(&l, "codex", 18, false);
        seed(&l, "codex", 19, false);
        seed(&l, "codex", 20, true);
        l
    }

    /// A live process keeps its agent build; once nothing runs from it, the plain rule
    #[test]
    fn an_agent_build_a_live_process_runs_from_is_not_reclaimed() {
        let l = codex_with_a_reclaimable_build("agent-live");
        let exe = l.build_dir("codex", 18).join("bin/codex");
        let running = || Some(vec![PathBuf::from("/usr/bin/login"), exe.clone()]);
        let report = run_with(&l, &|_| None, &running);
        assert!(report.reclaimed.is_empty(), "{:?}", report.reclaimed);
        assert!(crate::store::build_is_complete(&l.build_dir("codex", 18)));

        let report = run_with(&l, &|_| None, &|| {
            Some(vec![PathBuf::from("/usr/bin/login")])
        });
        assert_eq!(report.reclaimed, vec![("codex".to_string(), vec![18u64])]);
        assert!(!l.build_dir("codex", 18).exists());
        let _ = std::fs::remove_dir_all(&l.prefix);
    }

    /// A process table that cannot be read keeps every agent and trust build, and only those:
    #[test]
    fn an_unreadable_process_table_keeps_every_agent_and_toolchain_build() {
        let l = codex_with_a_reclaimable_build("agent-unknown");
        for b in [16u64, 17] {
            seed(&l, "ay", b, false);
        }
        seed(&l, "ay", 18, true);
        for b in [8580u64, 8590] {
            seed(&l, "trust", b, false);
        }
        seed(&l, "trust", 9192, true);
        let report = run_with(&l, &|_| None, &|| None);
        assert_eq!(report.reclaimed, vec![("ay".to_string(), vec![16u64])]);
        assert!(l.build_dir("codex", 18).exists());
        assert!(
            l.build_dir("trust", 8580).exists(),
            "the toolchain is kept too"
        );
        let report = run_with(&l, &|_| None, &|| Some(Vec::new()));
        assert!(
            report
                .reclaimed
                .contains(&("trust".to_string(), vec![8580u64])),
            "{:?}",
            report.reclaimed
        );
        let _ = std::fs::remove_dir_all(&l.prefix);
    }

    /// A LEASED BUILD IS KEPT, AND SAID (gap #31). A release cut pinned to a
    #[test]
    fn a_leased_build_is_kept_until_its_run_ends() {
        let l = layout("leased");
        for b in [8580u64, 8590, 8595] {
            seed(&l, "trust", b, false);
        }
        seed(&l, "trust", 9192, true);
        let subject = crate::lease::Subject::build("trust", 8590).unwrap();
        let lease = crate::lease::take(
            &l.prefix,
            &subject,
            "aterm-release (pid 7) — a release cut in /w",
            crate::lease::DEFAULT_WAIT,
        )
        .unwrap();
        let report = run_with(&l, &|_| None, &|| Some(Vec::new()));
        assert_eq!(report.reclaimed, vec![("trust".to_string(), vec![8580u64])]);
        assert!(crate::store::build_is_complete(&l.build_dir("trust", 8590)));
        assert_eq!(
            report.leased,
            vec![(
                "trust".to_string(),
                8590,
                crate::lease::Holders::Held(vec![
                    "aterm-release (pid 7) — a release cut in /w".to_string()
                ])
            )]
        );
        drop(lease);
        let report = run_with(&l, &|_| None, &|| Some(Vec::new()));
        assert_eq!(report.reclaimed, vec![("trust".to_string(), vec![8590u64])]);
        assert!(report.leased.is_empty());
        assert!(!l.build_dir("trust", 8590).exists());
        let left: Vec<String> = std::fs::read_dir(crate::lease::dir(&l.prefix))
            .unwrap()
            .flatten()
            .filter_map(|e| e.file_name().into_string().ok())
            .filter(|n| n.starts_with("store-trust-8590"))
            .collect();
        assert!(left.is_empty(), "{left:?}");
        let _ = std::fs::remove_dir_all(&l.prefix);
    }

    /// A lease whose state cannot be read keeps its build — never read as free.
    #[cfg(unix)]
    #[test]
    fn an_unreadable_lease_directory_keeps_every_candidate() {
        use std::os::unix::fs::PermissionsExt as _;
        let l = layout("lease-unreadable");
        for b in [16u64, 17] {
            seed(&l, "ay", b, false);
        }
        seed(&l, "ay", 18, true);
        let leases = crate::lease::dir(&l.prefix);
        std::fs::create_dir_all(&leases).unwrap();
        std::fs::set_permissions(&leases, std::fs::Permissions::from_mode(0o000)).unwrap();
        let readable = std::fs::read_dir(&leases).is_ok();
        let report = run_with(&l, &|_| None, &|| Some(Vec::new()));
        std::fs::set_permissions(&leases, std::fs::Permissions::from_mode(0o700)).unwrap();
        assert!(
            !readable,
            "NOT RUN as written: this process reads a 0000 directory (root?)"
        );
        assert!(report.reclaimed.is_empty(), "{:?}", report.reclaimed);
        assert_eq!(report.leased.len(), 1, "{:?}", report.leased);
        assert!(matches!(
            report.leased[0].2,
            crate::lease::Holders::Unknown(_)
        ));
        assert!(l.build_dir("ay", 16).exists());
        let _ = std::fs::remove_dir_all(&l.prefix);
    }

    /// Every program is asked about, and the process table is read at most once per pass and
    #[test]
    fn a_live_process_keeps_any_programs_build_and_the_table_is_read_once() {
        let l = layout("any-once");
        let calls = std::cell::Cell::new(0u32);
        let inside = l.build_dir("ay", 16).join("bin/ay");
        let running = || {
            calls.set(calls.get() + 1);
            Some(vec![inside.clone()])
        };
        seed(&l, "ay", 19, true);
        let _ = run_with(&l, &|_| None, &running);
        assert_eq!(calls.get(), 0, "no candidate, no enumeration");

        for b in [16u64, 17, 18] {
            seed(&l, "ay", b, false);
        }
        seed(&l, "codex", 17, false);
        seed(&l, "codex", 18, false);
        seed(&l, "codex", 19, false);
        seed(&l, "codex", 20, true);
        let report = run_with(&l, &|_| None, &running);
        assert_eq!(
            report.reclaimed,
            vec![
                ("ay".to_string(), vec![17u64]),
                ("codex".to_string(), vec![17u64, 18])
            ],
            "16 runs `ay`: kept"
        );
        assert!(l.build_dir("ay", 16).exists());
        assert_eq!(calls.get(), 1, "one enumeration serves every candidate");
        let _ = std::fs::remove_dir_all(&l.prefix);
    }

    /// A trust build run from its exec root is in use too: a shim routed through
    #[test]
    fn a_trust_build_running_from_its_exec_root_is_not_reclaimed() {
        let l = layout("trust-root");
        for b in [8580u64, 8590, 8595] {
            seed(&l, "trust", b, false);
        }
        seed(&l, "trust", 9192, true);
        let root = crate::compat::root_dir(&l, 8590);
        std::fs::create_dir_all(root.join("bin")).unwrap();
        let exe = root.join("bin/trustc");
        let report = run_with(&l, &|_| None, &|| Some(vec![exe.clone()]));
        assert_eq!(report.reclaimed, vec![("trust".to_string(), vec![8580u64])]);
        assert!(l.build_dir("trust", 8590).exists());
        let _ = std::fs::remove_dir_all(&l.prefix);
    }

    /// An interrupted agent install a process still runs from is not swept as debris.
    #[test]
    fn a_partial_agent_tree_a_process_runs_from_is_not_swept() {
        let l = layout("agent-partial");
        let partial = seed_partial(&l, "claude", 7);
        let exe = partial.join("bin/claude");
        let report = run_with(&l, &|_| None, &|| Some(vec![exe.clone()]));
        assert!(report.swept_partial.is_empty());
        assert!(partial.exists());
        let report = run_with(&l, &|_| None, &|| Some(Vec::new()));
        assert_eq!(
            report.swept_partial,
            vec![("claude".to_string(), vec![7u64])]
        );
        let _ = std::fs::remove_dir_all(&l.prefix);
    }

    /// `runs_from` sees a process by the kernel's path: a launch through a symlink is caught,
    #[cfg(target_os = "macos")]
    #[test]
    fn runs_from_sees_a_symlinked_launch_and_not_a_prefix_sibling() {
        let scratch = std::mem::ManuallyDrop::new(layout("runs-from"));
        let root = scratch.prefix.clone();
        let app = root.join("A.app");
        let macos = app.join("Contents/MacOS");
        std::fs::create_dir_all(&macos).expect("layout");
        let exe = macos.join("A");
        aterm_uds::deadline::ad_hoc_copy(Path::new("/bin/cat"), &exe);
        let link = root.join("via-link");
        std::os::unix::fs::symlink(&exe, &link).expect("symlink");
        std::fs::create_dir_all(root.join("A.app.rollback")).expect("sibling");

        assert_eq!(runs_from(&app), Some(false), "nothing runs from it yet");
        let mut child = aterm_uds::spawnfd::command(&link)
            .stdin(aterm_uds::spawnfd::Stdio::piped())
            .stdout(aterm_uds::spawnfd::Stdio::piped())
            .spawn()
            .expect("spawn through the symlink");
        let stdin = aterm_uds::deadline::cat_echoes(&mut child);
        let deadline = std::time::Instant::now() + aterm_spec::wait::MUST_HAPPEN;
        while runs_from(&app) != Some(true) && std::time::Instant::now() < deadline {
            std::thread::sleep(std::time::Duration::from_millis(20));
        }
        let seen = runs_from(&app);
        let sibling = runs_from(&root.join("A.app.rollback"));
        let alive = child.try_wait().expect("poll the cat").is_none();
        let _ = child.kill();
        let _ = child.wait();
        drop(stdin);
        drop(std::mem::ManuallyDrop::into_inner(scratch));
        assert!(
            alive,
            "the process seen is running, not a dead one awaiting its reap"
        );
        assert_eq!(seen, Some(true), "a symlinked launch runs from the bundle");
        assert_eq!(sibling, Some(false), "`A.app.rollback` is not `A.app`");
    }

    /// The real enumeration sees this very test process — the proof it reads the table
    #[cfg(any(target_os = "macos", target_os = "linux", windows))]
    #[test]
    fn the_process_table_includes_this_process() {
        let me = std::fs::canonicalize(std::env::current_exe().unwrap()).unwrap();
        let running = running_executables().expect("the process table reads");
        assert!(
            running.contains(&me),
            "{} not among {} processes",
            me.display(),
            running.len()
        );
    }

    /// A stage sidecar whose build is gone is swept; one beside a build that exists is not;
    #[test]
    fn orphan_sidecars_are_swept_and_a_discard_takes_the_vendor_record() {
        let l = layout("orphan-sidecars");
        seed(&l, "claude", 30, true);
        let beside_live =
            |suffix| crate::store::sidecar_path(&l.build_dir("claude", 30), suffix).unwrap();
        let orphan =
            |b, suffix| crate::store::sidecar_path(&l.build_dir("claude", b), suffix).unwrap();
        for suffix in crate::store::STAGE_SIDECAR_SUFFIXES {
            std::fs::write(beside_live(suffix), b"kept").unwrap();
            std::fs::write(orphan(31, suffix), b"orphan").unwrap();
        }
        let not_ours = l.prefix.join("store/claude/notes.vendor");
        std::fs::write(&not_ours, b"a file the user put here").unwrap();
        let refusal = l.build_dir("claude", 32).with_file_name("32.refused");
        std::fs::write(&refusal, b"stage-refusal v1\n").unwrap();

        let _ = run_with(&l, &|_| None, &|| Some(Vec::new()));
        for suffix in crate::store::STAGE_SIDECAR_SUFFIXES {
            assert!(
                beside_live(suffix).is_file(),
                "{suffix} beside a live build stays"
            );
            assert!(
                !orphan(31, suffix).exists(),
                "{suffix} of a gone build is swept"
            );
        }
        assert!(not_ours.is_file(), "only `<build><suffix>` names are ours");
        assert!(
            refusal.is_file(),
            "a refusal memo outlives its build by design"
        );

        let dir = seed(&l, "claude", 29, false);
        std::fs::write(orphan(29, crate::store::VENDOR_SIDECAR_SUFFIX), b"record").unwrap();
        crate::store::discard_build(&dir);
        assert!(!orphan(29, crate::store::VENDOR_SIDECAR_SUFFIX).exists());
        let _ = std::fs::remove_dir_all(&l.prefix);
    }

    /// Builds that look absent but are not keep their sidecars: a partial a session runs
    #[test]
    fn sidecars_of_a_partial_or_a_swap_parked_build_are_kept() {
        let l = layout("orphan-sidecars-kept");
        seed(&l, "claude", 40, true);
        let partial = seed_partial(&l, "claude", 34);
        seed(&l, "claude", 33, false);
        let (build33, first) = seed_killed_mid_swap(&l, "claude", 33);
        let second = seed_scratch(&l, "claude", "33.superseded-9999");
        let sidecar =
            |b, suffix| crate::store::sidecar_path(&l.build_dir("claude", b), suffix).unwrap();
        for suffix in crate::store::STAGE_SIDECAR_SUFFIXES {
            std::fs::write(sidecar(34, suffix), b"partial").unwrap();
            std::fs::write(sidecar(33, suffix), b"parked").unwrap();
        }
        let exe = partial.join("bin/claude");

        let _ = run_with(&l, &|_| None, &|| Some(vec![exe.clone()]));
        assert!(
            partial.exists(),
            "the fixture keeps the partial through the pass"
        );
        assert!(
            !build33.exists() && first.is_dir() && second.is_dir(),
            "and the ambiguous swap stays parked"
        );
        for suffix in crate::store::STAGE_SIDECAR_SUFFIXES {
            assert!(sidecar(34, suffix).is_file(), "{suffix} beside a partial");
            assert!(sidecar(33, suffix).is_file(), "{suffix} of a parked build");
        }

        let report = run_with(&l, &|_| None, &|| Some(Vec::new()));
        assert_eq!(
            report.swept_partial,
            vec![("claude".to_string(), vec![34u64])]
        );
        let _ = run_with(&l, &|_| None, &|| Some(Vec::new()));
        for suffix in crate::store::STAGE_SIDECAR_SUFFIXES {
            assert!(
                !sidecar(34, suffix).exists(),
                "{suffix} follows its partial"
            );
            assert!(sidecar(33, suffix).is_file(), "{suffix} of a parked build");
        }
        let _ = std::fs::remove_dir_all(&l.prefix);
    }
}
