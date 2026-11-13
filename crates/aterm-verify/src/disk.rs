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
//! says at its end how its disk was held and how many walks it made ([`event`]). A run whose
//! compile calls into a build directory completed keeps, at that directory's top, what they used
//! of it and when the run began, as its scope's run ([`note_call`], [`close`], [`keep_used`];
//! `atpkg::build_cache::stale::USED`): the record holds the newest run of each scope, and a unit
//! they stopped using is what the cache takes (`atpkg::build_cache::stale::decide`). Where the
//! run's record retired units, the run trims that directory itself before it exits, within a
//! bound on its time ([`trim_used`], `atpkg::build_cache::trim_run_end`), so a checkout's build
//! directory holds the builds its last runs use and not the ones they replaced: on 2026-11-12
//! one session's landing checks grew a build directory from 7 to 64 GiB in six hours, and
//! nothing took what they had replaced.

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

/// A build directory a run's compile calls build into, which [`crate::shape::Run::used`] keeps
/// what they reported by.
#[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub enum Dir {
    /// The main lane's, resolved when it is read ([`main_leaf`]): the build directory of the
    /// run's own target ([`Ctx::target_dir`]).
    Main,
    /// A side lane's, by its path: the lane's target directory, which holds its intermediates
    /// too (`crate::plan::lane_dir`).
    Side(PathBuf),
}

/// The build directory `cmd` builds into, by the last `CARGO_TARGET_DIR` among its own variables
/// ([`crate::exec::Cmd::envs`]): the main lane's where it names none (every child is handed the
/// run's target, [`Ctx::child_env_add`]) or names the run's target; the lint lane's where it
/// names that lane's directory; `None` for any other (a directory no record is kept of).
fn dir_of(ctx: &Ctx, cmd: &crate::exec::Cmd) -> Option<Dir> {
    let Some((_, target)) = cmd
        .envs
        .iter()
        .rev()
        .find(|(name, _)| name.as_os_str() == "CARGO_TARGET_DIR")
    else {
        return Some(Dir::Main);
    };
    let target = Path::new(target);
    if target == ctx.target_dir.as_path() {
        return Some(Dir::Main);
    }
    crate::plan::lane_dir(ctx, crate::plan::Lane::TippyTarget)
        .filter(|lane| lane.as_path() == target)
        .map(Dir::Side)
}

/// Note what the compile call `cmd` used of the build directory it builds into ([`dir_of`],
/// [`note_used`]): the artifacts and build script runs its `stdout` reports (Cargo's messages,
/// `None` where they could not be read), and whether it passed (`ok`). A call whose messages
/// could not be read is noted as one that did not pass: what it used is not known.
pub fn note_call(ctx: &Ctx, cmd: &crate::exec::Cmd, stdout: Option<&str>, ok: bool) {
    let Some(dir) = dir_of(ctx, cmd) else {
        return;
    };
    let artifacts = stdout.map(crate::shape::artifacts).unwrap_or_default();
    let script_runs = stdout.map(crate::shape::script_runs).unwrap_or_default();
    note_used(ctx, &dir, &artifacts, &script_runs, ok && stdout.is_some());
}

/// Note what a compile call used of the build directory `dir` ([`crate::shape::Run::used`]): the
/// packages it built tests of (the workspace's own), and the unit each artifact's file and each
/// build script's run (`script_runs`, their `out_dir`s) is of, fresh or built. For the main
/// lane's, a file cargo copied out of the build directory into the target (a bin a test needs)
/// is read as the unit it is a copy of, by its package and the profile it lies under in the
/// target ([`atpkg::build_cache::stale::Used::add_uplifted`]). A call that did not pass (`ok`
/// false), or one into a main lane whose build directory cannot be resolved, marks the
/// directory's note failed ([`crate::shape::Noted::failed`]), so the run keeps nothing of it.
pub fn note_used(
    ctx: &Ctx,
    dir: &Dir,
    artifacts: &[crate::shape::Artifact],
    script_runs: &[String],
    ok: bool,
) {
    // `main_leaf` takes the run's lock itself: resolve before taking it here.
    let leaf = match dir {
        Dir::Main => main_leaf(ctx)
            .ok()
            .flatten()
            .and_then(|leaf| leaf.path().ok().map(Path::to_path_buf)),
        Dir::Side(dir) => Some(dir.clone()),
    };
    let mut run = ctx
        .shape_run
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner);
    let noted = run.used.entry(dir.clone()).or_default();
    noted.failed |= !ok;
    let Some(leaf) = leaf else {
        noted.failed = true;
        return;
    };
    let leaf = leaf.as_path();
    let used = &mut noted.used;
    for artifact in artifacts {
        if artifact.test
            && let Some(package) = &artifact.package
        {
            used.members.insert(package.clone());
        }
        for file in artifact.filenames.iter().map(Path::new) {
            if file.starts_with(leaf) {
                used.add(leaf, file);
            } else if matches!(dir, Dir::Main)
                && let (Some(package), Some(profile)) =
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

/// The call that completes what the run builds into `dir` passed ([`crate::shape::Noted::closed`]):
/// for the main lane's, the test compile's whole or closing call; for the lint lane's, the last
/// of a dev run's lint passes over its selection, all of which passed. Without it the run keeps
/// nothing of `dir`: a run that never made that call (a check that builds no tests, a test
/// compile that failed before or in it, a release's lint, which lints package by package)
/// reported only some of what its scope uses.
pub fn close(ctx: &Ctx, dir: &Dir) {
    ctx.shape_run
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
        .used
        .entry(dir.clone())
        .or_default()
        .closed = true;
}

/// The run skipped, as it ran, a stage that builds into the main lane's build directory, named
/// by `stage` (a windowed check behind a locked console: no flag of the run says it, so no word
/// of its selection does). What the run reports of that directory leaves that stage's units out,
/// so the run is kept under a scope of its own ([`run_key`]) and replaces no run that built
/// them: under the same scope its record would retire every unit only that stage reported.
pub fn note_skipped(ctx: &Ctx, stage: &'static str) {
    ctx.shape_run
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
        .skipped
        .insert(stage);
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

/// What a run's record of the units it used is kept by, in the record at the top of a build
/// directory (`atpkg::build_cache::stale::Kept::runs`): its checkout, the work it does as the
/// machine's record spells it ([`selection_of`]: the checks that build, `as-release`, and what
/// narrows them, `--compile-only` and `--no-windows` among them), its packages or the whole
/// workspace, and each stage that builds into the main lane's build directory which it skipped
/// as it ran ([`note_skipped`]: so it is the key of a run that has ended, and read at its end).
/// Two runs that build the same units under the same features spell it alike, so the later
/// replaces the earlier and retires what it stopped using; a run that builds other units (other
/// packages, another check, a narrowed one, one that skipped a stage that builds) keeps a run of
/// its own beside it.
#[must_use]
pub fn run_key(ctx: &Ctx) -> String {
    let selection = selection_of(ctx);
    let mut key = format!("verify {} {}", ctx.root.display(), selection.work);
    match selection.crates {
        Some(crates) => {
            for name in crates {
                key.push_str(" -p ");
                key.push_str(&name);
            }
        }
        None => key.push_str(" --workspace"),
    }
    let run = ctx
        .shape_run
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner);
    for stage in &run.skipped {
        key.push_str(" --skipped=");
        key.push_str(stage);
    }
    key
}

/// One build directory a run kept its record in ([`keep_used`]).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct KeptDir {
    /// Which of the run's build directories it is.
    pub of: Dir,
    /// The build directory, whose top holds the record.
    pub dir: PathBuf,
    /// What the run's log says of it.
    pub line: String,
    /// How many units the record says this run retired
    /// (`atpkg::build_cache::stale::Kept::end`); 0 where the record was not written.
    pub retired: usize,
    /// The directory the record was written in, by its device and inode as they were read once
    /// it was (the path's own, never a link's target): what [`trim_used`] trims, and nothing
    /// else that later stands at the path. `None` where the record was not written, or the
    /// directory could not be read: nothing of it is trimmed.
    pub bound: Option<(u64, u64)>,
}

/// Keep, for each build directory the run's compile calls completed in (its closing call
/// passed, [`close`], and no call into it failed, [`note_used`]), what they used of it and when
/// the run began ([`Ctx::began`]) as its scope's run ([`run_key`]) in the record at the
/// directory's top (`atpkg::build_cache::stale::USED`), which replaces the scope's run before
/// and retires what the kept runs no longer use: one entry for each directory written or tried,
/// with the line the run's log says and, where the record was written, the directory it was
/// written in ([`KeptDir::bound`]). Nothing where the main lane's build directory cannot be
/// read (an error, or one replaced under the run), and no entry for a directory whose calls did
/// not complete or reported no unit of it; a run of any check, selection or tier keeps its own.
#[must_use]
pub fn keep_used(ctx: &Ctx) -> Vec<KeptDir> {
    let Ok(main) = main_leaf(ctx) else {
        return Vec::new();
    };
    let noted: Vec<(Dir, atpkg::build_cache::stale::Used)> = ctx
        .shape_run
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
        .used
        .iter()
        .filter(|(_, noted)| noted.closed && !noted.failed)
        .map(|(dir, noted)| (dir.clone(), noted.used.clone()))
        .collect();
    let began_ms = ctx
        .began
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(0, |d| u64::try_from(d.as_millis()).unwrap_or(u64::MAX));
    let key = run_key(ctx);
    let mut out = Vec::new();
    for (of, mut used) in noted {
        let leaf = match &of {
            Dir::Main => match main.as_ref().map(MainLeaf::path) {
                Some(Ok(leaf)) => leaf.to_path_buf(),
                _ => continue,
            },
            Dir::Side(dir) => dir.clone(),
        };
        used.began_ms = began_ms;
        let units: usize = used
            .profiles
            .values()
            .map(std::collections::BTreeSet::len)
            .sum();
        if units == 0 {
            continue;
        }
        let mut kept = atpkg::build_cache::stale::Kept::read(&leaf);
        let retired = kept.end(&key, used);
        let record = leaf.join(atpkg::build_cache::stale::USED);
        let (line, retired, bound) = match kept.keep(&leaf) {
            Ok(()) => (
                format!(
                    "disk: kept the {units} unit(s) this run used in {}, {retired} unit(s) its \
                     kept runs stopped using retired",
                    record.display()
                ),
                retired,
                identity(&leaf),
            ),
            Err(e) => (
                format!(
                    "disk: the {units} unit(s) this run used were not kept in {} ({e}); the \
                     record before stands",
                    record.display()
                ),
                0,
                None,
            ),
        };
        out.push(KeptDir {
            of,
            dir: leaf,
            line,
            retired,
            bound,
        });
    }
    out
}

/// The real directory at `dir`, by its device and inode; `None` where nothing stands there, it
/// cannot be read, or it is no directory (a link is not followed).
fn identity(dir: &Path) -> Option<(u64, u64)> {
    use std::os::unix::fs::MetadataExt as _;
    std::fs::symlink_metadata(dir)
        .ok()
        .filter(std::fs::Metadata::is_dir)
        .map(|meta| (meta.dev(), meta.ino()))
}

/// The most of a run's end its trim takes ([`trim_used`]), over all its build directories: the
/// verdict is decided and said by then, and what waits on the run's exit (the landing's push,
/// whoever awaits the run) waits no longer than this for it. What a trim had moved aside when
/// its time ran out stays staged in the build directory, and the next run to end there finishes
/// it.
const TRIM_WITHIN: std::time::Duration = std::time::Duration::from_secs(20);

/// TRIM, AT THE RUN'S END, EACH BUILD DIRECTORY IT KEPT ITS RECORD IN (`kept`, [`keep_used`];
/// `atpkg::build_cache::trim_run_end`): a trim an earlier run left staged there is finished
/// (nothing else finishes one: the cache's pass never looks inside a build directory for a
/// stage), and, where this run's record retired units, the units no kept run uses go through the
/// cache's own fences (cargo's locks held, the process check, custody, staged aside, then
/// removed). All of it is held to [`TRIM_WITHIN`] and stops once an interrupt is ending the run
/// ([`crate::stopping`]). One line for the run's log for each directory there is something to
/// say of: what went and the bytes freed; that the trim stopped with what it took still staged,
/// which the next run to end there finishes; or why nothing retired went (a build holds the
/// directory's locks, a process uses it, nothing retired met the rule). Retired units a trim
/// did not take are not retired again by the next run's record
/// (`atpkg::build_cache::stale::Kept::retired` is computed at each end): they wait out the stale
/// window like any unit no kept run names, and go in a pass of the cache (the main lane's) or in
/// a later trim that retires something there (a side lane's, whose record no pass reads). A
/// directory is trimmed only while the directory its record was written in still stands at its
/// path ([`KeptDir::bound`]: the same device and inode, no link), and the main lane's only
/// while it still resolves to that path; one replaced under the run is left alone, and so is
/// one whose record was not written.
#[must_use]
pub fn trim_used(ctx: &Ctx, kept: &[KeptDir]) -> Vec<String> {
    let until = std::time::Instant::now() + TRIM_WITHIN;
    let within = || !crate::stopping() && std::time::Instant::now() < until;
    trim_used_with(ctx, kept, &|dir| {
        atpkg::build_cache::trim_run_end(dir, std::time::SystemTime::now(), &within)
    })
}

/// [`trim_used`], each directory trimmed by `trim`.
fn trim_used_with(
    ctx: &Ctx,
    kept: &[KeptDir],
    trim: &dyn Fn(&Path) -> atpkg::build_cache::Trimmed,
) -> Vec<String> {
    use atpkg::build_cache::Unused;
    let mut said = Vec::new();
    for one in kept {
        let Some(bound) = one.bound else {
            continue;
        };
        let dir = one.dir.as_path();
        let resolves = one.of != Dir::Main
            || main_leaf(ctx)
                .ok()
                .flatten()
                .is_some_and(|leaf| leaf.path().is_ok_and(|path| path == dir));
        if !resolves || identity(dir) != Some(bound) {
            said.push(format!(
                "disk: {} was not trimmed (the build directory was replaced under the run)",
                dir.display()
            ));
            continue;
        }
        let trimmed = trim(dir);
        // What went: the units the trim took whole, the stages an earlier trim left that it
        // finished, and the stages that stay for the next trim (stopped, or holding an entry
        // that would not go).
        let (mut units, mut finished, mut staged) = (0_usize, 0_usize, 0_usize);
        for (_, why) in &trimmed.went {
            match why {
                Unused::Stale { superseded, .. } => units += *superseded,
                Unused::Unfinished => finished += 1,
                Unused::Stayed { .. } | Unused::Revoked { .. } | Unused::Refused { .. } => {
                    staged += 1;
                }
                _ => {}
            }
        }
        let why = trimmed
            .kept
            .as_deref()
            .unwrap_or("its removal did not finish");
        let freed = atpkg::admit_disk::size(trimmed.freed);
        if units > 0 {
            said.push(format!(
                "disk: trimmed {units} unit(s) no kept run uses from {}, {freed} freed",
                dir.display()
            ));
            continue;
        }
        if finished > 0 {
            said.push(format!(
                "disk: finished {finished} trim(s) an earlier run left staged in {}, {freed} freed",
                dir.display()
            ));
        }
        if staged > 0 {
            said.push(format!(
                "disk: the trim of {} stopped with what it had moved aside still staged there \
                 ({why}); the next run to end there finishes it",
                dir.display()
            ));
        } else if one.retired > 0 {
            said.push(format!(
                "disk: {} retired unit(s) in {} were not trimmed ({why}); they wait out the stale \
                 window like any unit no kept run names",
                one.retired,
                dir.display()
            ));
        }
    }
    said
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
