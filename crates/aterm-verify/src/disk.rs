// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! THE DISK A RUN WILL WRITE, CLAIMED BEFORE IT BUILDS (`docs/DESIGN-adaptive-build-2026-10-24.md`,
//! DISK-ADMIT).
//!
//! The preflight that refused a run short of disk went with the snapshot gate (42e8a9a273), and
//! since then a short volume showed only at `ENOSPC`, hours in, as a COULD NOT RUN row
//! ([`crate::exec::Run::environment_failure`]). Now a run whose checks build claims, in the
//! capacity ledger, what its directories (the main lane's build directory and `<target>/verify`)
//! grew by in the last runs of its scope on this target ([`scope_of`]: the checks that build and
//! the packages they read), or held at their peak beyond what they hold now
//! (a build directory reclaimed since is written again) but no more than [`COLD_BYTES`]
//! ([`FOOTPRINT`]), or where none was measured [`COLD_BYTES`] less what they hold already. A peak
//! further above what they hold measured more than one run writes (stale builds, or the build
//! root before the cache moved), so it claims the budget. A volume short of it is relieved first
//! (the build cache's pass under pressure, and inside aterm the window's disk watch), then waited
//! for with a line a minute naming the need, the free space and the largest holders, after one
//! saying why it needs that (`atpkg::admit_disk`).
//! `--admit now` starts at once and says what it lacks. A need above what the volume holds over
//! its floor is refused, and so is one still above what it has free over its floor once the
//! relief ran (UNMET, `aterm_capacity::Error::Unmet`): no other run's end gives that space, so a
//! wait could never end, and the run COULD NOT RUN, naming how much to free on which volume, what
//! holds the most there, and that `--admit now` starts it.
//!
//! Those directories are each checkout's own (the shared cache keeps one build directory per
//! workspace path), so a fresh worktree's hold no run of anything and its record beside them none:
//! every first run claimed [`COLD_BYTES`] less the verify tool's own build, 72 GiB for a scoped run
//! that wrote 3 GiB, and waited on a Mac with 170 GB free (V1, DA-MACHINE-FOOTPRINT). So a run that
//! passed is kept in the machine's record too, under this user's aterm state directory
//! ([`MACHINE_FOOTPRINT`]), by what it built ([`selection_of`]), and a run with no run of its scope
//! on its own directories claims what runs in this machine's checkouts bound
//! (`aterm_capacity::MachineFootprints::cover`): its own selection's run, a wider one's, or those of
//! selections whose crates together hold its, where that is less than the cold budget less what its
//! directories hold.
//!
//! A run that is part of work holding one disk claim for all of it (the release's, which sizes
//! that claim from its own record of what its runs wrote) is handed that claim's name
//! ([`aterm_capacity::DISK_COVER_ENV`], [`crate::EnvSnapshot::disk_cover`]) and covered by it
//! ([`Disk::Covered`], [`aterm_capacity::covering_disk`]): it walks nothing (no measure before it
//! builds, no meter while it does, no look at its end), claims nothing and keeps nothing, since
//! one walk of a release clone's build directory reads every file of hundreds of GiB. Every run
//! says at its end how its disk was held and how many walks it made ([`event`]). A release run
//! over the whole workspace that passed keeps the record of what its test compile used of the
//! build directory, and when it began ([`keep_used`]), which the cache's daily bound reads to take
//! the units of the workspace's own it superseded (`atpkg::build_cache::stale::USED`).

use std::path::{Path, PathBuf};

use crate::{Check, Ctx};

/// One gibibyte.
const GIB: u64 = 1 << 30;

/// The fallback for a run without measured footprints: 72 GiB. On 2026-10-27, the whole
/// workspace at `932f1ef624` grew empty tracked directories by 61.1 GiB; AppKit then brought
/// their peak to 63.2 GiB (`docs/measured/verify-disk-2026-10-27.md`). Round up with room for
/// growth. A first run claims this less what its directories hold already; a run measured
/// here claims what it wrote instead, and at most this to write again what they held at its
/// peak.
pub const COLD_BYTES: u64 = 72 * GIB;

/// The file in `<target>/verify` keeping what the last runs wrote (`aterm_capacity::Footprints`).
pub const FOOTPRINT: &str = "footprint.json";

/// The file under this user's aterm state directory (`aterm_types::dirs::account_state_dir`)
/// keeping what the runs that passed in every checkout on this machine wrote
/// (`aterm_capacity::MachineFootprints`), beside the stages' memory
/// (`aterm_capacity::book::VERIFY_RECORD`): a record of what a scope writes, not a cache, so a
/// build cache emptied leaves it true; and a test binary's state directory is its own.
pub const MACHINE_FOOTPRINT: &str = "verify/disk-footprint.json";

/// Whether `check` builds into the run's directories: every check but those that only read the
/// tree (fmt, census, forge, and export-scan, whose one write is a copy of the export in the
/// run's scratch, outside them) or the network (export).
#[must_use]
pub const fn builds(check: Check) -> bool {
    !matches!(
        check,
        Check::Fmt
            | Check::Census
            | Check::Forge
            | Check::ExportScan
            | Check::Export
            | Check::ReleasedShim
    )
}

/// The directory a `build.build-dir='…'` value names for the checkout at `root`.
#[must_use]
pub fn build_dir_path(value: &str, root: &Path) -> Option<PathBuf> {
    let dir = value
        .strip_prefix("build.build-dir='")?
        .strip_suffix('\'')?;
    Some(PathBuf::from(
        dir.replace("{workspace-root}", &root.to_string_lossy()),
    ))
}

/// What a run builds, spelled the same each time it builds the same, which its footprints are
/// kept by (`atpkg::admit_disk::Run::scope`): the checks it runs that build, sorted; where one
/// of them reads the selection, the packages it names, sorted, the whole workspace, or no
/// package (an empty cone); and the cells `--cell` chose. A `-p` run and a wider run of the same
/// checks are two scopes, so neither's growth is what the other claims.
#[must_use]
pub fn scope_of(ctx: &Ctx) -> String {
    let building = || ctx.checks.iter().copied().filter(|c| builds(*c));
    let mut checks: Vec<&str> = building().map(Check::name).collect();
    checks.sort_unstable();
    checks.dedup();
    let mut scope = checks.join(" ");
    if building().any(|c| c.reads(&ctx.scope)) {
        if ctx.scope.is_workspace() {
            scope.push_str(" --workspace");
        } else if ctx.scope.selects_nothing() {
            scope.push_str(" <no crates selected>");
        } else {
            let mut crates = ctx.scope.crates().to_vec();
            crates.sort_unstable();
            crates.dedup();
            for c in crates {
                scope.push_str(" -p ");
                scope.push_str(&c);
            }
        }
    }
    if !ctx.cells.is_empty() && building().any(|c| c == Check::Cells) {
        let mut cells = ctx.cells.clone();
        cells.sort_unstable();
        cells.dedup();
        scope.push_str(" --cell ");
        scope.push_str(&cells.join(","));
    }
    format!("{scope} --no-incremental")
}

/// What a run builds as the machine's record keeps it (`atpkg::admit_disk::Machinewide`), in the
/// record's words (`aterm_capacity::Selection::work`), sorted, so a run in another checkout
/// bounds it only where it did all of its work: the checks it runs that build, each that does
/// not read the selection marked `:tree` (what it builds does not follow `-p`'s crates: `cells`
/// named under the change cone lints every cell's whole graph, more than a `-p` run of the same
/// crates does); `as-release` where its `test` or `doc` carries the checkers (`--as-release`,
/// [`Ctx::checkers_in_tests`]), which they then build too; and what narrows a check, as
/// `<check>--<how>`, each a run that does less than one without it: the cells `--cell` chose
/// (`cells--cell=linux,win`), cells the selection pulled (`cells--pulled`: the cone closed over
/// its dependents in each cell's graph, not the whole graph), `--no-windows` for each check that
/// opens windows (its windowed stages are named skips), and `measure` with no `--app`
/// (`measure--no-app`: its measuring stages are named skips and its prebuild is not planned, so
/// it builds nothing, where `--app` builds the measuring benches). `--compile-only` and
/// `--cursor-pacing` keep one stage of the whole plan, whatever check holds the rest (a pulled
/// `wasm-smoke` too: [`crate::plan`]'s `wanted`), so each narrows all of the work
/// (`--compile-only`, `--cursor-pacing`). Its crates are the selection's, sorted and each once
/// (none for the empty cone); `None` for the whole workspace, or where no check that builds reads
/// the selection.
#[must_use]
pub fn selection_of(ctx: &Ctx) -> atpkg::admit_disk::Selection {
    let building: Vec<Check> = ctx.checks.iter().copied().filter(|c| builds(*c)).collect();
    let has = |check: Check| building.contains(&check);
    let mut words: Vec<String> = building
        .iter()
        .map(|c| {
            if c.reads(&ctx.scope) {
                c.name().to_owned()
            } else {
                format!("{}:tree", c.name())
            }
        })
        .collect();
    if ctx.checkers_in_tests && (has(Check::Test) || has(Check::Doc)) {
        words.push("as-release".to_owned());
    }
    if has(Check::Cells) && !ctx.cells.is_empty() {
        let mut cells = ctx.cells.clone();
        cells.sort_unstable();
        cells.dedup();
        words.push(format!("cells--cell={}", cells.join(",")));
    }
    if has(Check::Cells) && ctx.cells_pulled {
        words.push("cells--pulled".to_owned());
    }
    for c in &building {
        if ctx.no_windows && c.spec().stages.iter().any(|id| id.opens_windows()) {
            words.push(format!("{}--no-windows", c.name()));
        }
    }
    if has(Check::Measure) && ctx.measure_artifact.is_none() {
        words.push("measure--no-app".to_owned());
    }
    for (narrowed, how) in [
        (ctx.compile_only, "--compile-only"),
        (ctx.cursor_pacing, "--cursor-pacing"),
    ] {
        if narrowed {
            words.push(how.to_owned());
        }
    }
    words.sort_unstable();
    words.dedup();
    let mut work = words.join(" ");
    let crates =
        (building.iter().any(|c| c.reads(&ctx.scope)) && !ctx.scope.is_workspace()).then(|| {
            let mut crates = ctx.scope.crates().to_vec();
            crates.sort_unstable();
            crates.dedup();
            crates
        });
    // Older, incremental runs still bound this smaller layout. The reverse must
    // never happen: these observations omit compiler state those runs would write.
    work.push_str(" --no-incremental");
    atpkg::admit_disk::Selection { work, crates }
}

/// The run's disk as it is claimed: its build directory and `<target>/verify`, measured, on the
/// target's volume, kept by its scope ([`scope_of`]) there, and by what it builds
/// ([`selection_of`]) in the machine's record ([`MACHINE_FOOTPRINT`]) where this user has a state
/// directory and its intermediate root has the workspace-specific artifact scope: a directory
/// verify places, or a validated caller-selected exact shared cache. Opaque
/// caller folders remain unwalked and claim conservatively without machine-wide history.
/// A compiler stage in the unskipped plan is needed before claiming build space: a GUI-only
/// pacing burst claims no build directories.
///
/// # Errors
/// The captured native layout is invalid, or a recognized shared cache is unsafe, replaced,
/// or on another volume. No caller path is silently substituted after such a refusal.
pub fn run_of(ctx: &Ctx) -> Result<Option<atpkg::admit_disk::Run>, String> {
    if !ctx.checks.iter().any(|c| builds(*c)) || !crate::plan::has_compiler_stage(ctx) {
        return Ok(None);
    }
    let leaf = main_leaf(ctx)?;
    let build = leaf
        .as_ref()
        .map(MainLeaf::path)
        .transpose()?
        .map(Path::to_path_buf);
    let measured = build.is_some();
    let mut dirs: Vec<PathBuf> = build.into_iter().collect();
    dirs.push(ctx.state_dir());
    let names: Vec<&str> = ctx.checks.iter().map(|c| c.name()).collect();
    let scope = if ctx.checks.iter().any(|c| c.reads(&ctx.scope)) {
        format!(" {}", ctx.scope.label())
    } else {
        String::new()
    };
    Ok(Some(atpkg::admit_disk::Run {
        what: format!("verify {}{scope}", names.join(" ")),
        scope: scope_of(ctx),
        dirs,
        record: ctx.state_dir().join(FOOTPRINT),
        cold: COLD_BYTES,
        on: ctx.target_dir.clone(),
        stuck_ends: Some(crate::STUCK_ENDS),
        scratch: false,
        machine: aterm_types::dirs::account_state_dir()
            .filter(|_| measured)
            .map(|state| atpkg::admit_disk::Machinewide {
                record: state.join(MACHINE_FOOTPRINT),
                selection: selection_of(ctx),
            }),
    }))
}

/// How the run's disk is held while it builds ([`admit`]).
#[derive(Debug)]
pub enum Disk {
    /// Its own claim, its writes measured ([`run_of`]).
    Held(Box<atpkg::admit_disk::Admitted>),
    /// The claim the work it is part of holds, named by [`crate::EnvSnapshot::disk_cover`]: its
    /// holder's record. Nothing is walked, claimed or kept.
    Covered(Box<aterm_capacity::Record>),
    /// None: no check it runs builds, or no ledger could take a claim (said).
    Unclaimed,
}

impl Disk {
    /// The word [`event`] says it by.
    #[must_use]
    pub const fn mode(&self) -> &'static str {
        match self {
            Self::Held(_) => "claimed",
            Self::Covered(_) => "covered",
            Self::Unclaimed => "unclaimed",
        }
    }
}

/// Claim the run's disk ([`run_of`]) before it builds, in this user's ledger on the live machine
/// ([`admit_in`]), saying on stderr what it does and waits for. No ledger to claim in is said, and
/// the run goes on [`Disk::Unclaimed`].
///
/// # Errors
/// As [`run_of`] and [`answer`].
pub fn admit(ctx: &Ctx, mode: atpkg::admit_disk::Admit) -> Result<Disk, String> {
    let Some(run) = run_of(ctx)? else {
        return Ok(Disk::Unclaimed);
    };
    let Some(ledger) = aterm_capacity::Ledger::user() else {
        eprintln!(
            "verify: the run's disk is not claimed (no home to keep the ledger under); it runs without a claim"
        );
        return Ok(Disk::Unclaimed);
    };
    admit_run(
        ctx,
        run,
        &ledger,
        &aterm_capacity::Live,
        std::process::id(),
        mode,
    )
}

/// [`admit`] in `ledger` on `machine` for the run's process `pid`: [`Disk::Covered`] where the
/// claim [`crate::EnvSnapshot::disk_cover`] names is held by `pid` or an ancestor and is of disk
/// ([`aterm_capacity::covering_disk`]), said on stderr, and nothing measured; else the run's own
/// claim ([`run_of`], relieved by the build cache's live pass, [`atpkg::admit_disk::Reclaim::live`]),
/// or [`Disk::Unclaimed`] where no check builds.
///
/// # Errors
/// As [`run_of`] and [`answer`].
pub fn admit_in(
    ctx: &Ctx,
    ledger: &aterm_capacity::Ledger,
    machine: &dyn aterm_capacity::Machine,
    pid: u32,
    mode: atpkg::admit_disk::Admit,
) -> Result<Disk, String> {
    let Some(run) = run_of(ctx)? else {
        return Ok(Disk::Unclaimed);
    };
    admit_run(ctx, run, ledger, machine, pid, mode)
}

fn admit_run(
    ctx: &Ctx,
    run: atpkg::admit_disk::Run,
    ledger: &aterm_capacity::Ledger,
    machine: &dyn aterm_capacity::Machine,
    pid: u32,
    mode: atpkg::admit_disk::Admit,
) -> Result<Disk, String> {
    if let Some(marker) = ctx.env.disk_cover.as_deref() {
        match aterm_capacity::covering_disk(ledger, machine, pid, marker, &run.on) {
            Ok(Some(by)) => {
                eprintln!("verify: {}", covered_line(&by));
                return Ok(Disk::Covered(Box::new(by)));
            }
            Ok(None) => eprintln!(
                "verify: disk: the claim {marker} this run was handed covers nothing here (it \
                 ended, waits, is not of disk, or is not an ancestor's); it claims for itself"
            ),
            Err(e) => eprintln!(
                "verify: disk: the claim {marker} this run was handed could not be read ({e}); \
                 it claims for itself"
            ),
        }
    }
    let mut relief = atpkg::admit_disk::Reclaim::live(&run.dirs);
    answer(atpkg::admit_disk::admit_on(
        run,
        ledger,
        machine,
        mode,
        aterm_capacity::Pace::default(),
        &mut relief,
        &mut |line| eprintln!("verify: {line}"),
    ))
}

/// The line a covered run says, as it is admitted and in its log at its end: whose claim covers
/// it.
#[must_use]
pub fn covered_line(by: &aterm_capacity::Record) -> String {
    format!(
        "disk: covered by {} (claim {}, pid {}); nothing walked or kept",
        by.need.what,
        by.cover_marker(),
        by.pid
    )
}

/// The line every run says on stderr at its end: how its disk was held ([`Disk::mode`]) and how
/// many walks of directories its process made ([`aterm_capacity::walks`]), which the release counts
/// as its `disk_walks`.
#[must_use]
pub fn event(mode: &str, walks: u64) -> String {
    format!(
        "verify-event: {}",
        crate::event_log::event_value(
            crate::event_log::EventKind::Disk,
            aterm_json::json!({  "mode": mode, "walks": walks})
        )
    )
}

struct MainLeaf {
    path: PathBuf,
    known: Option<std::sync::Arc<atpkg::build_cache::registered::KnownCargoCache>>,
}

impl MainLeaf {
    fn path(&self) -> Result<&Path, String> {
        self.known
            .as_ref()
            .map_or(Ok(self.path.as_path()), |known| {
                known.path().map_err(|error| error.to_string())
            })
    }
}

/// Select the captured intermediate root, separately from whether the producer owns it.
/// Only the exact shared namespace for this workspace admits recursive measurement of a
/// caller choice. Its typed observation grants no registration or deletion authority.
fn main_leaf(ctx: &Ctx) -> Result<Option<MainLeaf>, String> {
    let plan = ctx.native_build_plan()?;
    if let Some(path) = ctx
        .main_build_dir
        .as_deref()
        .and_then(|value| build_dir_path(value, &ctx.root))
    {
        return Ok(Some(MainLeaf { path, known: None }));
    }
    if !cfg!(target_os = "macos") {
        return Ok(None);
    }
    let Some(selected) = plan.explicit_build_dir() else {
        return Ok(None);
    };
    let Some(known) = atpkg::build_cache::registered::KnownCargoCache::observe(
        &ctx.env.home,
        &ctx.root,
        selected,
        &ctx.target_dir,
    )
    .map_err(|error| error.to_string())?
    else {
        return Ok(None);
    };
    let path = known
        .path()
        .map_err(|error| error.to_string())?
        .to_path_buf();
    let known = std::sync::Arc::new(known);
    let mut run = ctx
        .shape_run
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner);
    if let Some(previous) = &run.used_cache {
        known
            .continue_from(previous)
            .map_err(|error| error.to_string())?;
    }
    run.used_cache = Some(std::sync::Arc::clone(&known));
    Ok(Some(MainLeaf {
        path,
        known: Some(known),
    }))
}

/// Note what a call of the test compile used of the main lane's build directory
/// ([`crate::shape::Run::used`]): the packages it built tests of (the workspace's own), and the
/// unit each artifact's file and each build script's run (`script_runs`, their `out_dir`s) is of,
/// fresh or built. A file cargo copied out of the build directory into the target (a bin a test
/// needs) is read as the unit it is a copy of, by its package and the profile it lies under in
/// the target ([`atpkg::build_cache::stale::Used::add_uplifted`]).
pub fn note_used(ctx: &Ctx, artifacts: &[crate::shape::Artifact], script_runs: &[String]) {
    let Ok(Some(leaf)) = main_leaf(ctx) else {
        return;
    };
    let Ok(leaf) = leaf.path() else {
        return;
    };
    let mut run = ctx
        .shape_run
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner);
    let used = &mut run.used;
    for artifact in artifacts {
        if artifact.test
            && let Some(package) = &artifact.package
        {
            used.members.insert(package.clone());
        }
        for file in artifact.filenames.iter().map(Path::new) {
            if file.starts_with(leaf) {
                used.add(leaf, file);
            } else if let (Some(package), Some(profile)) =
                (artifact.package.as_deref(), uplift_profile(ctx, file))
            {
                used.add_uplifted(leaf, &profile, package, file);
            }
        }
    }
    for out in script_runs {
        used.add(leaf, Path::new(out));
    }
}

/// The profile a file cargo copied into the run's target lies under: `debug` for
/// `<target>/debug/<bin>` and `<target>/debug/examples/<example>`, `<triple>/debug` under a cross
/// target's; `None` for a file elsewhere.
fn uplift_profile(ctx: &Ctx, file: &Path) -> Option<String> {
    let rel = file.strip_prefix(&ctx.target_dir).ok()?;
    let mut parts: Vec<&str> = rel
        .parent()?
        .components()
        .map(|c| match c {
            std::path::Component::Normal(n) => n.to_str(),
            _ => None,
        })
        .collect::<Option<_>>()?;
    if parts.last() == Some(&"examples") {
        parts.pop();
    }
    (!parts.is_empty()).then(|| parts.join("/"))
}

/// Keep, for a release run (`release`, or `--as-release`: [`Ctx::checkers_in_tests`]) over the
/// whole workspace that `passed`, the record of what its test compile used ([`note_used`]) and
/// when it began ([`Ctx::began`]) at the top of its build directory
/// (`atpkg::build_cache::stale::USED`), which the cache's daily bound reads to take what it
/// superseded: the line its log says, or `None` where nothing is kept (another run; one over less
/// than the workspace, whose graph builds its crates under other features than the release's, so
/// its record would not name the release's units; one that failed; one whose test compile reported
/// no unit of a build directory verify places).
#[must_use]
pub fn keep_used(ctx: &Ctx, passed: bool) -> Option<String> {
    if !passed || !ctx.checkers_in_tests || !ctx.scope.is_workspace() {
        return None;
    }
    let binding = main_leaf(ctx).ok()??;
    let mut used = ctx
        .shape_run
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
        .used
        .clone();
    used.began_ms = ctx
        .began
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(0, |d| u64::try_from(d.as_millis()).unwrap_or(u64::MAX));
    let units: usize = used
        .profiles
        .values()
        .map(std::collections::BTreeSet::len)
        .sum();
    if units == 0 {
        return None;
    }
    let leaf = binding.path().ok()?;
    Some(match used.keep(leaf) {
        Ok(()) => format!(
            "disk: kept the {units} unit(s) this release run used as {}, which the cache's \
             bound reads to take what it superseded",
            leaf.join(atpkg::build_cache::stale::USED).display()
        ),
        Err(e) => format!(
            "disk: the {units} unit(s) this release run used were not kept ({e}); the cache's \
             bound reads the record before"
        ),
    })
}

/// What the run does with its disk claim's answer: holds the claim while it builds; or, where no
/// ledger could take it, says so and runs without one.
///
/// # Errors
/// Why the run COULD NOT RUN, no check started: the need is more than the target's volume holds
/// above its floor, so the run can never fit there; it is more than the volume has free above
/// its floor once the relief ran, which no wait admits (UNMET), named with what to free and
/// `--admit now`; an interrupt ended the wait; or the wait gave up behind holders that made no
/// progress for [`crate::STUCK_ENDS`], named, and left alone.
fn answer(
    admitted: Result<atpkg::admit_disk::Admitted, atpkg::admit_disk::Error>,
) -> Result<Disk, String> {
    match admitted {
        Ok(held) => Ok(Disk::Held(Box::new(held))),
        Err(atpkg::admit_disk::Error::Never(why)) => Err(format!(
            "{why}; it can never be admitted there. Fix: point CARGO_TARGET_DIR at a larger volume"
        )),
        Err(e @ atpkg::admit_disk::Error::Unmet { .. }) => Err(format!(
            "the run's disk claim was refused, no check started: {e}, or start it at once with \
             `--admit now`"
        )),
        Err(e @ atpkg::admit_disk::Error::Stopped { .. }) => Err(format!(
            "the run's disk claim was {e}; no check started, so nothing was decided"
        )),
        Err(e @ atpkg::admit_disk::Error::Stuck { .. }) => Err(format!(
            "the run's disk claim {e}; no check started, so nothing was decided"
        )),
        Err(e) => {
            eprintln!("verify: the run's disk is not claimed ({e}); it runs without a claim");
            Ok(Disk::Unclaimed)
        }
    }
}

#[cfg(all(test, target_os = "macos"))]
#[path = "disk_cache_tests.rs"]
mod cache_tests;

#[cfg(test)]
#[path = "disk_tests.rs"]
mod tests;
