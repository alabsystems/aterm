// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0

//! Rollback-aware GC retention.

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
    pub const fn build(&self) -> u64 {
        self.build
    }
}

/// Why a program has no live witness.
#[derive(Clone, PartialEq, Eq, Debug)]
pub enum Diverged {
    /// The program's `current` link selects one build; its `bin/` shims run another.
    LinkShimMismatch { link_says: u64, shims_say: u64 },
    /// This program's own tools resolve into different builds.
    ShimsDisagree { builds: Vec<u64> },
    /// The program is live on `PATH` but no `current` link resolves into it.
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
    /// A native lease, unknown custody, or removal failure prevented reclamation.
    NotCleared { program: String, build: u64 },
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

/// Every build the derived `bin/` shims point into, keyed by program.
fn shim_claims(layout: &Layout) -> BTreeMap<String, BTreeSet<u64>> {
    let mut out: BTreeMap<String, BTreeSet<u64>> = BTreeMap::new();
    let Ok(entries) = std::fs::read_dir(layout.bin_dir()) else {
        return out;
    };
    for e in entries.flatten() {
        // resolve_shim, not read_link: a Windows shim is a `.cmd` file read_link errs on,
        // which would fail open there. store_build_of, not program_build_of_target: the
        // latter is unanchored, so a dev-link into a checkout with a `store/ay/18/` tail
        // would fabricate a claim about our ay@18.
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

/// [`live_builds`]' reconciliation over claim views already read.
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

/// Reclaimable store builds of a program given `installed`, its live witness, and `lands_on`.
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
    let dir = layout.build_dir(&live.program, build);
    let guard =
        crate::compat::sdk_mutation_guard_for_build(&dir).map_err(|_| Retained::NotCleared {
            program: live.program.clone(),
            build,
        })?;
    if let Some(guard) = guard {
        discard_superseded_reclaimed(layout, live, build, &guard)
    } else {
        crate::store::discard_build(&dir);
        if is_gone(&dir) {
            Ok(())
        } else {
            Err(Retained::NotCleared {
                program: live.program.clone(),
                build,
            })
        }
    }
}

fn discard_superseded_reclaimed(
    layout: &Layout,
    live: &LiveBuild,
    build: u64,
    guard: &crate::lease::ReclaimGuard,
) -> Result<(), Retained> {
    if build == live.build {
        return Err(Retained::IsLive {
            program: live.program.clone(),
            build,
        });
    }
    let dir = layout.build_dir(&live.program, build);
    crate::store::discard_build_reclaimed(&dir, guard);
    if !is_gone(&dir) {
        return Err(Retained::NotCleared {
            program: live.program.clone(),
            build,
        });
    }
    Ok(())
}

/// What an interrupted install left in `store/<program>/`.
#[derive(Default)]
struct Debris {
    /// `(program, build, dir)` — numeric build dirs with no completeness marker.
    partial: Vec<(String, u64, PathBuf)>,
    /// `(program, dir name, dir)` — `<build>.incoming-<pid>` / `<build>.superseded-<pid>`.
    scratch: Vec<(String, String, PathBuf)>,
    /// Stage sidecar files (`<build>.vendor`, `.shim-env`, `.modes`, `.exposes`) whose
    /// `<build>` is gone, and a gone build's named refusal memo past a lock's keep window.
    orphan_sidecars: Vec<PathBuf>,
}

/// The sidecars swept once their build is gone ([`crate::store::STAGE_SIDECAR_SUFFIXES`], and
/// a resolve check's [`crate::store::UNPROVEN_SUFFIX`] hold).
fn orphan_sidecar_build(name: &str) -> Option<u64> {
    crate::store::STAGE_SIDECAR_SUFFIXES
        .iter()
        .chain([&crate::store::UNPROVEN_SUFFIX])
        .find_map(|suffix| name.strip_suffix(suffix))
        .and_then(crate::store::parse_build_name)
}

/// Whether `name` is stage scratch this manager produced.
fn is_stage_scratch(name: &str) -> bool {
    crate::store::stage_scratch_of(name).is_some()
}

/// Move back any tree a swap was killed midway through, before the sweep decides what is debris.
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
            let Some(name) = name.to_str() else { continue };
            if let Some((build, crate::store::Scratch::Superseded)) =
                crate::store::stage_scratch_of(name)
            {
                candidates.insert(build);
            } else if let Some(build) = name
                .strip_suffix(aterm_toolchain::build_cache::NOINDEX)
                .and_then(crate::store::parse_build_name)
            {
                // An aside whose link a crash kept from being laid gets it back.
                crate::store::relink_aside(&prog.path().join(build.to_string()));
            }
        }
        for build in candidates {
            let dir = prog.path().join(build.to_string());
            if !crate::store::recover_interrupted_swap(&dir) && crate::store::tree_absent(&dir) {
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
    let now_unix = crate::flow::now_unix();
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
            // A named build's refusal memo stands for a build no tree holds, until it is older
            // than a lock's keep window ([`crate::store::is_stale_named_refusal`]).
            if entry.file_type().is_ok_and(|t| t.is_file())
                && let Some(build) = entry
                    .file_name()
                    .to_str()
                    .and_then(|n| n.strip_suffix(crate::store::STAGE_REFUSAL_SUFFIX))
                    .and_then(crate::store::parse_build_name)
                && matches!(
                    crate::store::presence(&prog.path().join(build.to_string())),
                    crate::store::Presence::Absent
                )
                && crate::store::is_stale_named_refusal(&entry.path(), now_unix)
            {
                out.orphan_sidecars.push(entry.path());
                continue;
            }
            // A sidecar whose build is provably absent describes nothing. A build parked
            // mid-swap is absent too, but its sidecars belong to the parked tree and stay.
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
            // A build laid aside is its `<build>` link; its `<build>.noindex` tree is no
            // build of its own and goes with the link.
            if !entry.file_type().is_ok_and(|t| t.is_dir())
                && !aterm_toolchain::layout::is_linked_aside(&entry.path())
            {
                continue;
            }
            let name = entry.file_name();
            let Some(name) = name.to_str() else { continue };
            if let Some(build) = crate::store::parse_build_name(name) {
                // A refused header is no evidence of interruption. Check it before the
                // complete-inventory reader, which a FIFO or alias must never reach here.
                if crate::store::build_marks_another_slice(&entry.path()).unwrap_or(true) {
                    continue;
                }
                if crate::store::build_is_complete(&entry.path()) {
                    continue;
                }
                if claimed.get(&program).is_some_and(|s| s.contains(&build)) {
                    continue;
                }
                out.partial.push((program.clone(), build, entry.path()));
            } else if is_stage_scratch(name) {
                // A parked `<build>.superseded-<pid>` (nothing at `<build>`) is that build's
                // only copy. Still swept: `.incoming-<pid>` half-extracts and superseded
                // siblings of a build that is there.
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
    /// `(program, staged file names)` for archives left in `staging/<program>/`.
    pub swept_staging: Vec<(String, Vec<String>)>,
    /// Exec roots (`<prefix>/compat/trust/<n>`) and dot debris removed by the sweep.
    pub swept_exec_roots: Vec<PathBuf>,
    /// What the exec-root sweep could not remove, one sentence each.
    pub exec_root_errors: Vec<String>,
    /// `(program, build, who)` for builds kept because a run holds a lease on them.
    pub leased: Vec<(String, u64, crate::lease::Holders)>,
    /// `(program, staged file name)` for archives kept for a held Trust toolchain update.
    pub held_staging: Vec<(String, String)>,
    /// `(program, builds ascending)` moved out of Spotlight's index into `<build>.noindex`
    /// ([`lay_aside_unused`]).
    pub laid_aside: Vec<(String, Vec<u64>)>,
}

/// Reclaim superseded builds per program, keeping the live build and one rollback.
#[must_use]
pub fn run(layout: &Layout, channel: Option<&Channel>) -> GcReport {
    run_in(layout, None, channel, &|_| Vec::new(), &running_executables)
}

/// [`run`], sparing each program's `.part` and complete archive named by `pinned_asset`.
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
        &|p: &str| pinned_asset(p).map_or_else(|| pursued_assets(layout, p), |asset| vec![asset]),
        &running_executables,
    )
}

/// Every staging name a vendor-direct program is still working towards.
fn pursued_assets(layout: &Layout, program: &str) -> Vec<String> {
    let Some(spec) = crate::vendor_direct::spec(program) else {
        return Vec::new();
    };
    let Some(head) =
        crate::vendor_direct::ProgramStamp::read(layout, program).and_then(|s| s.last_head)
    else {
        return Vec::new();
    };
    let active = crate::ops::active_build_of(layout, program);
    if active < Some(head.build_id()) {
        crate::vendor_direct::resolve::staging_assets(spec, head, crate::cli::current_triple())
    } else {
        Vec::new()
    }
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
        &|p: &str| pinned_asset(p).map_or_else(|| pursued_assets(layout, p), |asset| vec![asset]),
        &running_executables,
    )
}

/// [`run_keeping_pinned_partials`] with no channel and the process enumeration injected.
#[cfg(test)]
fn run_with(
    layout: &Layout,
    pinned_asset: &dyn Fn(&str) -> Option<String>,
    running: &dyn Fn() -> Option<Vec<PathBuf>>,
) -> GcReport {
    run_in(
        layout,
        None,
        None,
        &|p| pinned_asset(p).into_iter().collect(),
        running,
    )
}

/// Runs the GC pass over `scope` with `channel` rollback targets and injected `running`.
fn run_in(
    layout: &Layout,
    scope: Option<&dyn Fn(&str) -> bool>,
    channel: Option<&Channel>,
    pinned_assets: &dyn Fn(&str) -> Vec<String>,
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
        let vendor_yanks = crate::vendor_direct::is_vendor(&program).then(|| {
            crate::vendor_direct::policy::Policy::current(layout, None).with_channel(channel)
        });
        let witness_is_alab = crate::alab::build_is_alab(layout, &program, witness.build);
        let lands_on = |b: u64| {
            (!witness_is_alab || crate::alab::build_is_alab(layout, &program, b))
                && match &vendor_yanks {
                    Some(policy) => {
                        crate::vendor_direct::lane::rollback_lands_on(layout, policy, &program, b)
                    }
                    None => crate::flow::rollback_lands_on(layout, channel, &program, b),
                }
        };
        let mut gone = Vec::new();
        let now = std::time::SystemTime::now();
        for b in reclaimable(&installed, witness, &lands_on) {
            let Some(native_gate) = lease_clear(layout, &program, b, &mut leased) else {
                continue;
            };
            // An explicit named install or trust.lock resolution retains its build for the
            // existing keep window. Read under the gate: installs mark under the store lock,
            // and resolvers under their lease, so a mark made after this pass began is seen.
            if crate::trust_lock::locked_within_keep(&layout.build_dir(&program, b), now) {
                continue;
            }
            if in_use.holds(&program, &layout.build_dir(&program, b), Some(b)) {
                continue;
            }
            let discarded = match native_gate.as_ref() {
                Some(guard) => discard_superseded_reclaimed(layout, witness, b, guard),
                None => discard_superseded(layout, witness, b),
            };
            if discarded.is_ok() && is_gone(&layout.build_dir(&program, b)) {
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
        let Some(native_gate) = lease_clear(layout, &program, build, &mut leased) else {
            continue;
        };
        if in_use.holds(&program, &path, Some(build)) {
            continue;
        }
        // A tree that never completed carries no verdict on its pack: the memo of a stage or
        // resolve check that refused it stays, so the next install does not download it again.
        if let Some(guard) = native_gate.as_ref() {
            crate::store::discard_build_keeping_memo_reclaimed(&path, guard);
        } else {
            crate::store::discard_build_keeping_memo(&path);
        }
        if is_gone(&path) {
            swept_partial.entry(program).or_default().push(build);
        }
    }
    for sidecar in debris.orphan_sidecars {
        let _ = std::fs::remove_file(sidecar);
    }
    // A discarded build's kept unit went with it; its `units/` link goes now too.
    crate::peers::sweep_kept(layout);
    let mut swept_scratch: BTreeMap<String, Vec<String>> = BTreeMap::new();
    for (program, name, path) in debris.scratch {
        // Reported only when removed; the next pass enumerates a survivor again.
        if crate::store::remove_tree(&path).is_ok() {
            swept_scratch.entry(program).or_default().push(name);
        }
    }
    let laid_aside = lay_aside_unused(layout, &in_scope, &claimed, &mut in_use);

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
            let kept = pinned_assets(&name);
            let mut gone: Vec<String> = Vec::new();
            for e in entries.filter_map(Result::ok) {
                // Regular files only: never follow a symlink out of the prefix, and
                // never recurse into something that is not ours to interpret.
                if !e.file_type().is_ok_and(|t| t.is_file()) {
                    continue;
                }
                let entry_name = e.file_name().into_string().ok();
                if let Some(ref n) = entry_name
                    && kept
                        .iter()
                        .any(|asset| n == asset || n.strip_prefix(asset.as_str()) == Some(".part"))
                {
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
        laid_aside,
    }
}

/// On macOS, move each Rust toolchain build Spotlight still indexes
/// ([`crate::store::spotlight_exposed_builds`]) into `<build>.noindex` behind a `<build>` link
/// ([`crate::store::lay_aside`]) where the rename disturbs nothing: never the build `current` or
/// a shim names, a build a run leases or a live process runs from, or one held while macOS may
/// still be assessing a first exec in it.
fn lay_aside_unused(
    layout: &Layout,
    in_scope: &dyn Fn(&str) -> bool,
    claimed: &BTreeMap<String, BTreeSet<u64>>,
    in_use: &mut InUse<'_>,
) -> Vec<(String, Vec<u64>)> {
    let mut laid: BTreeMap<String, Vec<u64>> = BTreeMap::new();
    if !cfg!(target_os = "macos") {
        return Vec::new();
    }
    for exposed in crate::store::spotlight_exposed_builds(&layout.prefix) {
        let crate::store::ExposedBuild {
            program,
            build,
            dir,
            active,
        } = exposed;
        if active
            || !in_scope(&program)
            || claimed.get(&program).is_some_and(|b| b.contains(&build))
            || !crate::store::build_is_complete(&dir)
            || crate::store::held_unproven(&dir)
        {
            continue;
        }
        let Some(subject) = crate::lease::Subject::build(&program, build) else {
            continue;
        };
        let crate::lease::Reclaim::Clear(_gate) = crate::lease::reclaim(&layout.prefix, &subject)
        else {
            continue;
        };
        // A process table that cannot be read rules nothing out, for any program.
        if !in_use.readable() || in_use.holds(&program, &dir, Some(build)) {
            continue;
        }
        if crate::store::lay_aside(&dir).is_ok() {
            laid.entry(program).or_default().push(build);
        }
    }
    laid.into_iter().collect()
}

/// Whether `tree` is gone from disk, all a reclaim or sweep line may claim.
fn is_gone(tree: &Path) -> bool {
    matches!(std::fs::symlink_metadata(tree), Err(e) if e.kind() == std::io::ErrorKind::NotFound)
}

/// The lease verdict on reclaiming `program`'s `build`.
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

/// Whether a live process runs from a build, asked before any build is reclaimed.
struct InUse<'a> {
    layout: &'a Layout,
    enumerate: &'a dyn Fn() -> Option<Vec<PathBuf>>,
    running: Option<Option<Vec<PathBuf>>>,
}

impl InUse<'_> {
    /// Whether the process table could be read (read once, shared with [`Self::holds`]).
    fn readable(&mut self) -> bool {
        let enumerate = self.enumerate;
        self.running.get_or_insert_with(enumerate).is_some()
    }

    /// `true` when `build_dir` of `program` must be kept because a process may run from it.
    fn holds(&mut self, program: &str, build_dir: &Path, build: Option<u64>) -> bool {
        let enumerate = self.enumerate;
        let Some(running) = self.running.get_or_insert_with(enumerate) else {
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
        // Something there that does not resolve is held; a build's own link to an aside that
        // is gone holds no tree anything could run from.
        if resolved.is_empty() && !crate::store::tree_absent(build_dir) {
            return true;
        }
        running.iter().any(|exe| {
            dirs.iter()
                .chain(resolved.iter())
                .any(|d| exe.starts_with(d))
        })
    }
}

/// Whether a live process runs from under `dir`.
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

/// The live process table ([`running_executables`]), for the rest of the crate.
pub(crate) fn process_table() -> Option<Vec<PathBuf>> {
    running_executables()
}

/// The executable path of every process alive now ([`running_processes`] without the pids).
fn running_executables() -> Option<Vec<PathBuf>> {
    Some(
        running_process_table()?
            .into_iter()
            .map(|(_, exe)| exe)
            .collect(),
    )
}

/// Every process alive now as `(pid, executable path)`, by the kernel's path for each.
#[must_use]
pub fn running_processes() -> Option<Vec<(i32, PathBuf)>> {
    running_process_table()
}

/// Pid and executable path of every live process via `proc_listallpids` and `proc_pidpath`.
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
        // SAFETY: `pids` holds exactly `bytes` bytes and outlives the call, which writes at most
        // that many.
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

/// Pid and executable path of every live process from `/proc/<pid>/exe`.
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

/// Pid and resolved executable path of every live process on Windows.
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
        // SAFETY: `pids` holds exactly `bytes` bytes and outlives the call, which writes at most
        // that many.
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
        // SAFETY: `buf` holds `len` UTF-16 units and outlives the call; `len` is a valid
        // in/out-param.
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

/// No process table is read here.
#[cfg(not(any(target_os = "macos", target_os = "linux", windows)))]
fn running_process_table() -> Option<Vec<(i32, PathBuf)>> {
    None
}

#[cfg(test)]
#[path = "gc_tests.rs"]
mod tests;
