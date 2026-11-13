// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Real [`crate::release::Backend`] implementation for `targo --unverified ship release`.

#[cfg(unix)]
use aterm_uds::spawnfd::Tether as _;
use aterm_uds::spawnfd::{Command, Stdio, command};
use std::collections::BTreeSet;
#[cfg(unix)]
use std::io::Read as _;
use std::io::Write as _;
use std::path::{Path, PathBuf};

use crate::channel::source_attestation_findings;
use crate::gates::{Outcome, Precheck, PrecheckGate};
use crate::interrupt;
use crate::ledger::{BYTE_EXACT, GitCli, GitRunner};
use crate::line::{Fix, Hop, Line};
use crate::provision::{AskWords, Finding};
use crate::release::{
    AfterCut, Answer, Artifact, Backend, Branch, BranchStop, Branches, BuildWork, Built, Candidate,
    CutJournalUnread, CutState, Discarded, EngineNow, EngineRows, Facts, InputFinding,
    PUBLISH_GATE_ENV, Preflight, Prover, PublishGate, RELEASE_COMMAND, Ran, Refusal, Reservation,
    Side, SignedDiscard, SourceState, StageTarget, Step, SupersedeResult,
};

#[path = "release_scanner_tools.rs"]
mod scanner_tools;

#[path = "release_export_remedy.rs"]
mod export_remedy;

#[cfg(not(test))]
#[path = "release_cutter_probe.rs"]
mod cutter_probe;
#[cfg(not(test))]
pub(crate) use cutter_probe::step_one_probe_if_asked;

fn preflight_cache(repo: &Path) -> Result<PathBuf, String> {
    let dir = crate::snapshot::CacheRoot::for_repo(repo)?.preflight();
    std::fs::create_dir_all(&dir).map_err(|e| format!("create {}: {e}", dir.display()))?;
    Ok(dir)
}

/// The variable `publish/pre-promote` reads for the running release's own cutter: set on `pub
/// publish`, whose engine passes it through to the hook. `publish/transforms.sh` reads it too: a
/// `pub stage` gets it only from [`hand_transforms_cutter`].
pub const RELEASE_CUTTER_ENV: &str = "ATERM_RELEASE_CUTTER";

fn local_git() -> Result<Command, Refusal> {
    crate::ledger::git_command().map_err(|why| {
        Refusal::new(
            why.to_string(),
            "repair the managed Git installation and retry",
        )
        .could_not_run()
    })
}

/// The moment the cutter [`RELEASE_CUTTER_ENV`] names was judged from, with the pid and start of
/// the process that hands it on, set and removed with it ([`hand_this_cutter`]), so the handed
/// cutter judges itself from that moment too while that process is its live ancestor
/// ([`crate::cutter::handed_since`]). [`Real::scrubbed`] removes both from every child it builds:
/// a child has them only from [`hand_this_cutter`].
pub const RELEASE_CUTTER_SINCE_ENV: &str = "ATERM_RELEASE_CUTTER_SINCE";

#[cfg(unix)]
const TOOLCHAIN_LEASE_FD_ENV: &str = "ATERM_RELEASE_TOOLCHAIN_LEASE_FD";
#[cfg(unix)]
const TOOLCHAIN_LEASE_LEAF_ENV: &str = "ATERM_RELEASE_TOOLCHAIN_LEASE_LEAF";
#[cfg(unix)]
const LINE_FD_ENV: &str = "ATERM_RELEASE_LINE_FD";
#[cfg(unix)]
const LINE_WRITER_FD_ENV: &str = "ATERM_RELEASE_LINE_WRITER_FD";
#[cfg(unix)]
const LINE_LEAF_ENV: &str = "ATERM_RELEASE_LINE_LEAF";

/// The cutter a `pub stage` of `commit` hands its `publish/transforms.sh`: `exe`, when this
/// process is `commit`'s own build (`built`, [`crate::cutter::running`]: built from that commit's
/// clean, tracked cutter sources, lockfile, manifests, cargo config and toolchain pin, and the
/// links inside its checkout they are read through, none of them changed after the moment its
/// start judgment dated them from ([`crate::cutter::Since`]) or read by way of a place outside the
/// checkout, and none of those build inputs removed since), so the stage runs `commit`'s rules
/// with no build; else why not.
pub(crate) fn transforms_cutter(
    built: Result<&str, &crate::cutter::NotOwn>,
    exe: std::io::Result<PathBuf>,
    commit: &str,
) -> Result<PathBuf, String> {
    match built {
        Ok(built) if built == commit => {
            exe.map_err(|e| format!("this cutter cannot name its own executable ({e})"))
        }
        Ok(built) => Err(format!(
            "this cutter is the build of {}, not {}",
            short(built),
            short(commit)
        )),
        Err(not) => Err(format!("this cutter is no commit's build: {}", not.why)),
    }
}

/// Hand `stage`, a `pub stage` of `commit`, this cutter for its transforms
/// ([`transforms_cutter`], [`hand_this_cutter`]), and otherwise no cutter at all, whatever this
/// process inherited; the line that says which.
fn hand_transforms_cutter(stage: &mut Command, commit: &str) -> String {
    match transforms_cutter(crate::cutter::running(), std::env::current_exe(), commit) {
        Ok(exe) => {
            hand_this_cutter(stage, &exe, crate::cutter::reference());
            format!(
                "== transforms: by this cutter ({}), the build of {} itself",
                exe.display(),
                short(commit)
            )
        }
        Err(why) => {
            stage.env_remove(RELEASE_CUTTER_ENV);
            stage.env_remove(RELEASE_CUTTER_SINCE_ENV);
            format!(
                "== transforms: {why}; {}'s publish/transforms.sh builds its own",
                short(commit)
            )
        }
    }
}

/// Hand `cmd` this running cutter, whose executable is `exe`: [`RELEASE_CUTTER_ENV`] names it,
/// and [`RELEASE_CUTTER_SINCE_ENV`] carries `since`, the moment its start judgment dated its
/// inputs from ([`crate::cutter::reference`]), handed on by this process
/// ([`crate::cutter::hand_value`]), or is removed when there is none or this process's start is
/// unread. A frozen cutter also hands the record it was proved by
/// ([`crate::release_tools::proof_for_this_image`]), so a child running this same image proves
/// itself the same way; any other image named by it fails that proof closed.
fn hand_this_cutter(cmd: &mut Command, exe: &Path, since: Option<std::time::SystemTime>) {
    hand_cutter_with_proof(
        cmd,
        exe,
        since,
        crate::release_tools::proof_for_this_image(),
    );
}

fn hand_cutter_with_proof(
    cmd: &mut Command,
    exe: &Path,
    since: Option<std::time::SystemTime>,
    proof: Option<std::ffi::OsString>,
) {
    cmd.env(RELEASE_CUTTER_ENV, exe);
    match since.and_then(crate::cutter::hand_value) {
        Some(since) => cmd.env(RELEASE_CUTTER_SINCE_ENV, since),
        None => cmd.env_remove(RELEASE_CUTTER_SINCE_ENV),
    };
    match proof {
        Some(record) => cmd.env(crate::release_tools::RECORD_ENV, record),
        None => cmd.env_remove(crate::release_tools::RECORD_ENV),
    };
}

fn detach_process(cmd: &mut Command) {
    #[cfg(unix)]
    // Detached step groups receive the cutter's forwarded stops. They must not inherit a
    // background shell's ignored keys, which would disable those stops before verify starts.
    cmd.setsid(true).default_signals();
}

fn step_process_group(cmd: &mut Command, prefreeze: bool) {
    if prefreeze {
        // Preparation already leads a cancellable group. Detaching targo again would
        // let its compiler outlive the supervisor's positive wait on the worker.
        #[cfg(unix)]
        cmd.setsid(false).default_signals();
    } else {
        detach_process(cmd);
    }
}

fn detach(cmd: &mut Command) {
    crate::credential_wait::ask_no_one(cmd);
    detach_process(cmd);
}

/// Run the anonymous appcast transport through a release step's child launcher. Keeping this
/// adapter below the shared transport means every retry is still a noted, interruptible release
/// child rather than a curl started behind the step supervisor.
fn appcast_bytes_with<L>(
    url: &str,
    cap: u64,
    launch: L,
    fetch: impl FnOnce(&str, u64, &dyn aterm_update_core::github_rest::Spawn) -> Result<Vec<u8>, String>,
) -> Result<Vec<u8>, String>
where
    L: crate::site::Launch + Send + Sync,
{
    let spawn = crate::github::Launched(launch);
    fetch(url, cap, &spawn)
}

fn appcast_bytes<L: crate::site::Launch + Send + Sync>(
    url: &str,
    cap: u64,
    launch: L,
) -> Result<Vec<u8>, String> {
    appcast_bytes_with(
        url,
        cap,
        launch,
        aterm_update_core::download_bytes_uncached_via,
    )
}

fn note_child_in(release: &Path, step: Step, pid: u32) -> PathBuf {
    let started = started_of(pid).unwrap_or_default();
    let note = crate::release::child_note(release, pid);
    let _ = std::fs::create_dir_all(release);
    let _ = crate::durable::write_fresh(
        &note,
        format!("pid {pid}\nstarted {started}\nstep {}\n", step.name()),
    );
    note
}

/// The release's child rules for a step that runs in this process — the after-cut step's site sync and its GitHub reads: every child it starts, git's and the REST lane's curl included ([`crate::github::Launched`]), is started as [`Real::spawn_forwarded`] starts a step's (none once interrupted, a session of its own with git's prompt off, the interrupt handed to it, noted while it runs).
#[derive(Clone, Debug)]
pub(crate) struct StepChildren {
    /// Where the running child is noted, and under which step; `None` on a dry run.
    pub(crate) noted: Option<(PathBuf, Step)>,
}

impl crate::site::Launch for StepChildren {
    fn interrupted(&self) -> Option<i32> {
        interrupt::caught()
    }

    fn spawn(
        &self,
        cmd: &mut aterm_uds::spawnfd::Command,
    ) -> std::io::Result<(aterm_uds::spawnfd::Child, crate::site::Guard)> {
        if interrupt::caught().is_some() {
            return Err(std::io::Error::new(
                std::io::ErrorKind::Interrupted,
                "not started: the release was interrupted",
            ));
        }
        detach(cmd);
        let child = cmd.spawn()?;
        let running = interrupt::Running::enter(child.id());
        let note = self
            .noted
            .as_ref()
            .map(|(release, step)| note_child_in(release, *step, child.id()));
        Ok((
            child,
            Box::new(Forwarded {
                _running: running,
                meter: None,
                note,
            }),
        ))
    }
}

pub(crate) fn started_of(pid: u32) -> Option<String> {
    let out = {
        command(aterm_uds::os_path!("ps"))
            .args(["-p", &pid.to_string(), "-o", "lstart="])
            .output()
            .ok()?
    };
    let s = String::from_utf8_lossy(&out.stdout).trim().to_string();
    (out.status.success() && !s.is_empty()).then_some(s)
}

/// The release state below HOME: its logs, its build times and its kept build-inputs seal
/// (`inputs-seal/`, [`crate::build_inputs::arm`]).
const RELEASE_STATE: &str = ".local/state/aterm-release";

/// The commit step's last lines a dry run's refusal carries, since its logs do not outlive it
/// ([`Real::commit_log_refusal`]).
const COMMIT_LOG_TAIL: usize = 20;

/// The release state's directory: [`RELEASE_STATE`] below `home`, or below a Cargo test binary's
/// own root ([`aterm_uds::test_binary::scratch_root`]), so that no test writes a run's logs, its
/// step times, its disk footprints or a kept seal into the person's, which the next real release
/// reads.
pub(crate) fn release_state(home: &Path) -> PathBuf {
    aterm_uds::test_binary::scratch_root()
        .unwrap_or(home)
        .join(RELEASE_STATE)
}

/// A dry run's diagnostic files are still file-event churn, even though it builds nothing.
fn dry_log_dir(temp: &Path, pid: u32, nth: u32) -> PathBuf {
    aterm_tempfile::quiet_in(temp).join(format!("aterm-release-dry-{pid}-{nth}.noindex"))
}

/// One cache identity for cutter preparation, its supervisor and its readers.
pub(crate) fn cutter_cache_dir(release: &Path) -> PathBuf {
    release.join(".ubd/cutter-cache")
}

type VerifyHalf = fn(
    &Real,
    Step,
    &Candidate,
    &Prover,
    &crate::check_record::VerifyRun,
    Option<&Artifact>,
    &BranchStop,
) -> Result<Ran, Refusal>;
type BuildRun =
    fn(&Real, &Candidate, &Reservation, &Prover, &Path, &BranchStop) -> Result<Artifact, Refusal>;
type StageFixturesRun = fn(
    &Real,
    Step,
    &Candidate,
    &Reservation,
    &Prover,
    &Path,
    Option<&BranchStop>,
) -> Result<String, Refusal>;

#[derive(Clone, Debug, PartialEq, Eq)]
struct PrefrozenStep1 {
    commit: String,
    seal: String,
}

fn public_release_exists(slug: &str, tag: &str) -> Result<bool, String> {
    crate::publish::release_objects_by_tag(slug, tag)
        .map(|objects| !objects.is_empty())
        .map_err(|error| error.to_string())
}

/// The real backend.
pub struct Real {
    prefreeze_claim: Option<aterm_capacity::Claim>,
    prefrozen_step1: Option<PrefrozenStep1>,
    pub(crate) build_inputs: fn() -> Result<String, String>,
    /// Keeps this process's build-inputs seal in the release state, its reports also in the
    /// named log ([`crate::build_inputs::arm`]).
    pub(crate) arm_inputs: fn(&Path, Option<&Path>),
    /// Arm the real state with a pre-freeze's candidate seal for step 1's mandatory recheck.
    arm_prefrozen_inputs: fn(&Path, Option<&Path>, Option<&Path>),
    /// The kept seal each child is handed ([`crate::build_inputs::handed_path`]).
    pub(crate) handed_inputs: fn() -> Option<PathBuf>,
    /// Locate the exact kept seal produced by a pre-freeze. The real cutter rechecks it before
    /// adoption; this boundary only authenticates the record and selects its handoff path.
    prefrozen_inputs: fn(&Path, &str) -> Option<PathBuf>,
    /// Injectable boundary for the tree-half run. Production and tests both enter it through
    /// [`RealBranches::verify`], so the handoff completion around that call is mutation-covered.
    verify_half_op: VerifyHalf,
    /// Injectable build/fixture boundaries. The production [`Real::build_branch`] owns their
    /// ordering; tests replace only the effects, not that orchestration.
    build_run_op: BuildRun,
    await_fixture_index_op:
        fn(&Real, &Candidate, &Prover, &TreeIndexHandoff, &BranchStop) -> Result<(), Refusal>,
    stage_fixtures_run_op: StageFixturesRun,
    repo: PathBuf,
    home: PathBuf,
    /// The publication engine this release is named (`--engine`, else the source a release that
    /// execed this cutter handed it ([`ENGINE_SOURCE_ENV`]), else `$PUBLICATION_ENGINE`, else
    /// `~/publication`): it names the engine's origin and committer, and nothing here runs, syncs
    /// or writes it, since other processes move it ([`aterm_verify::engine::engine_copy`]).
    engine_source: PathBuf,
    /// The engine this release runs, syncs, reads and hands every child as `PUBLICATION_ENGINE`:
    /// its own copy ([`aterm_verify::engine::engine_copy`], made from `engine_source`'s origin by
    /// [`Real::engine_copy`], at a real run's step 1 where the clone has none
    /// ([`Real::engine_first_copy`])); in a dry run, which never makes or moves one,
    /// `engine_source` until a real run has made it.
    engine: PathBuf,
    /// Captured once, when the release starts, and handed with `engine_source` to its own frozen
    /// cutter on exec ([`hand_engine_capture`], [`engine_authority`]); a missing engine defers
    /// refusal until an engine operation.
    engine_repository: Result<agh::Slug, String>,
    engine_url: fn(&Path) -> Result<String, String>,
    /// The main of the engine's origin, fetched at step 1 ([`Backend::fetch`]): what the release
    /// judges of the engine (its ledger, its code) until it has synced its copy
    /// ([`Real::engine_text`]), which may be a release behind or not made yet; a dry run, which
    /// syncs none, judges it throughout.
    origin_main: Option<aterm_verify::engine::OriginMain>,
    /// This run brought its copy to the origin's main ([`Real::engine_sync`]): what it judges of
    /// the engine is the copy's from then on, whose ledger this run's own `pub publish` writes.
    engine_synced: std::sync::atomic::AtomicBool,
    state: PathBuf,
    logs: PathBuf,
    /// `<git common dir>/aterm-release`: the journal, the run claim, the child's note.
    release: PathBuf,
    /// A dry run's logs are scratch, removed when it ends.
    scratch_logs: bool,
    verbose: bool,
    origin_tags: String,
    source_origin: fn(&Path) -> Result<crate::github_remote::Remote, String>,
    /// Typed staging-repository read. Tests replace only the transport result while exercising
    /// the production retired-line selection and version judgment.
    staging_file: fn(&str, &str, &str) -> Result<Vec<u8>, String>,
    /// Typed public release-object read, separated from historical channel selection so tests
    /// exercise every channel a retired line could have published to without network access.
    release_exists: fn(&str, &str) -> Result<bool, String>,
    prefetched_origin: Option<crate::github_remote::FetchedOrigin>,
    /// The step the run is at ([`Backend::entered`]): what a child is noted under.
    step: Step,
    /// This process's arguments (after the program) with every path flag's value
    /// anchored and no `--fix` ([`crate::release::ReleaseOptions::exec_args`]): what the
    /// frozen cutter receives when it replaces this image, from the clone's root.
    argv: Vec<std::ffi::OsString>,
    source_signing: Option<crate::source_signing::Tools>,
    prerequisites: Option<crate::managed_prerequisites::Tools>,
    prerequisite_acquire: crate::managed_prerequisites::Acquire,
    prepared_source_helper: Option<PathBuf>,
    scanner_tools: Option<scanner_tools::Report>,
    scanner_probe: scanner_tools::Probe,
    /// How a step builds a dev binary of this checkout ([`Real::build_checkout_bin`]; a test's
    /// stand-in answers cargo's JSON messages itself).
    checkout_build: CheckoutBuild,
    /// Production uses the configured native origin. Tests route the same typed
    /// boundary to an explicit local-bare authority, including scratch copies.
    engine_origin: fn(&Path, &Path) -> Result<crate::github_remote::Remote, String>,
    /// The caller's [`ENGINE_AGH_ENV`], read once when the backend is made: the engine's agh
    /// when set ([`Real::engine_agh`]). A test sets it, so a cut run with `PUBLISH_AGH` does
    /// not hand the test's engine stand-in the release's agh.
    engine_agh_explicit: Option<std::ffi::OsString>,
    /// The release's one disk claim ([`Backend::claim_disk`]), named to every child it starts.
    disk_claim: Option<atpkg::admit_disk::Whole>,
    /// The one Release-rank line marker, retained across the in-place cutter exec.
    line_priority: Option<aterm_capacity::Claim>,
    fleet_line: Option<crate::line_claim::Claim>,
    fleet_holder: Option<(String, String)>,
    /// The qualify graph's one pool ([`Backend::qualify_begin`]), handed to every child it starts.
    pool: Option<Pool>,
    warming: Option<WarmingVerifier>,
    warm_verifier: Option<PathBuf>,
    automatic_prefreeze: Option<aterm_uds::spawnfd::Child>,
}

struct WarmingVerifier {
    commit: String,
    child: aterm_uds::spawnfd::Child,
    _forwarded: Forwarded,
    stdout: PathBuf,
    stderr: PathBuf,
}

/// The packaging child started at bundle, owned across READY and joined by step 7's sign.
struct PreparingSign {
    child: Option<aterm_uds::spawnfd::Child>,
    _forwarded: Forwarded,
    stop: BranchStop,
    cmd: Command,
    log: PathBuf,
    file: std::fs::File,
    from: u64,
    started: Option<u64>,
}

impl crate::release::PendingSign for PreparingSign {
    fn stop(&self) -> BranchStop {
        self.stop.clone()
    }

    fn finish(mut self: Box<Self>) -> Ran {
        let mut child = self.child.take().expect("one packaging child");
        let waited = wait_accounted(&mut child, &mut self._forwarded);
        let code = waited
            .as_ref()
            .map_or(-1, |(status, _)| status.code().unwrap_or(-1));
        let usage = waited.ok().and_then(|(_, usage)| usage);
        let _ = writeln!(
            self.file,
            "{}",
            step_child_event(Step::Bundle, &self.cmd, self.started, now_ms(), code, usage,)
        );
        Ran {
            code,
            detail: tail_since(&self.log, 8, self.from),
        }
    }
}

impl Drop for PreparingSign {
    fn drop(&mut self) {
        if let Some(mut child) = self.child.take() {
            self.stop.fire();
            let _ = wait_accounted(&mut child, &mut self._forwarded);
        }
    }
}

impl Drop for WarmingVerifier {
    fn drop(&mut self) {
        if !self.child.is_reaped() {
            aterm_uds::deadline::settle_child(&mut self.child);
        }
    }
}

/// One Release core claim partitioned into isolated build, tree and export allocations.
/// The engine's gate receives its allocation as `CARGO_BUILD_JOBS`.
struct Pool {
    _claim: aterm_capacity::Claim,
    jobs: aterm_capacity::BranchShares,
    slots: u32,
    export: u32,
}

/// Completion follows the branch's drained child groups, including an unwinding branch. Return
/// admission-barrier tokens before recovering the share: a token outside its FIFO still belongs
/// to that branch, and reminting first would duplicate it.
struct FinishShare<'pool, 'barrier> {
    pool: Option<&'pool Pool>,
    branch: usize,
    admission_barrier:
        &'barrier std::sync::Mutex<Option<aterm_capacity::InitialReservation<'pool>>>,
}

/// Establish a real allocation before a zero-token branch can reserve memory. Return the
/// probe token before starting clients, which charge their own implicit tokens.
fn start_with_share<T>(
    pool: &aterm_capacity::Jobserver,
    stopped: &dyn Fn() -> bool,
    start: impl FnOnce() -> T,
) -> std::io::Result<T> {
    drop(pool.acquire(stopped)?);
    Ok(start())
}

impl Drop for FinishShare<'_, '_> {
    fn drop(&mut self) {
        self.admission_barrier
            .lock()
            .unwrap_or_else(|error| error.into_inner())
            .take();
        if let Some(pool) = self.pool {
            pool.jobs.finish(self.branch);
        }
    }
}

/// The qualify graph's pool at `path`: one Release core claim of every core, as `admit` answers
/// it, isolated FIFOs only for the compiler branches in `branches`, and the export's reservation
/// where export runs.
fn qualify_pool(
    path: &Path,
    branches: &[Branch],
    admit: impl FnOnce(aterm_capacity::Need) -> Result<aterm_capacity::Claim, aterm_capacity::Error>,
) -> Result<Pool, Refusal> {
    let claim = admit(
        aterm_capacity::Need::new("release qualify pool")
            .rank(aterm_capacity::Rank::Release)
            .cores(aterm_capacity::cores()),
    )
    .map_err(|e| {
        Refusal::new(
            format!("the qualify graph's core claim was not admitted: {e} — nothing ran"),
            format!("run {RELEASE_COMMAND} again once the ledger admits it"),
        )
        .could_not_run()
    })?;
    let slots = claim.cores().max(1);
    // A run that died leaves its FIFO behind, and mkfifo never replaces one.
    let _ = std::fs::remove_file(path);
    let _ = std::fs::remove_file(path.with_extension("tree.jobs"));
    let active = [
        branches.contains(&Branch::Build),
        branches.contains(&Branch::Tree),
    ];
    let export = export_share(
        slots,
        branches.contains(&Branch::Export),
        active[0] || active[1],
    );
    let jobs = aterm_capacity::BranchShares::new(path, slots, export, active).map_err(|e| {
        Refusal::new(
            format!(
                "cannot make the qualify graph's jobserver at {}: {e} — nothing ran",
                path.display()
            ),
            format!("fix what it names, then {RELEASE_COMMAND}"),
        )
        .could_not_run()
    })?;
    Ok(Pool {
        _claim: claim,
        jobs,
        slots,
        export,
    })
}

/// The export's reservation: the whole grant when it is the only branch, otherwise a quarter of
/// the grant, at least one. On a one-slot host the export runs first, then a compiler branch,
/// keeping the same total allocation.
pub(crate) fn export_share(slots: u32, export: bool, compiler: bool) -> u32 {
    if !export {
        return 0;
    }
    if !compiler {
        return slots;
    }
    if slots == 1 {
        return 1;
    }
    (slots / 4).clamp(1, slots - 1)
}

/// The real backend's view the qualify graph's branches run on ([`Backend::branches`]).
struct RealBranches<'a> {
    real: &'a Real,
    /// Every non-export token, held before either compiler branch starts. The build can reserve
    /// its slice memory and the tree can request lint memory without either starting Cargo. Held
    /// lint restores the private shares; queued lint transfers the whole allocation to build.
    admission_barrier: std::sync::Mutex<Option<aterm_capacity::InitialReservation<'a>>>,
    tree_index: TreeIndexHandoff,
}

/// The tree branch's one-way handoff to the fixture consumer. The index file itself carries the
/// candidate, compiler and executable content identity; these bits distinguish "not written yet"
/// from "the tree ended without usable evidence" without making the build compile beside it.
#[derive(Default)]
struct TreeIndexHandoff {
    expected: std::sync::atomic::AtomicBool,
    completion: std::sync::atomic::AtomicU8,
    bundle_finished: std::sync::atomic::AtomicBool,
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
#[repr(u8)]
enum TreeIndexCompletion {
    #[default]
    Pending = 0,
    /// The tree ended without a red; an absent index may use the bounded fallback compile.
    Compile = 1,
    /// The tree itself went red (or panicked). No fixture work may start on its behalf.
    Stopped = 2,
}

impl TreeIndexHandoff {
    fn expect(&self) {
        self.expected
            .store(true, std::sync::atomic::Ordering::SeqCst);
    }

    fn finish(&self, completion: TreeIndexCompletion) {
        debug_assert_ne!(completion, TreeIndexCompletion::Pending);
        self.completion
            .store(completion as u8, std::sync::atomic::Ordering::SeqCst);
    }

    fn expected(&self) -> bool {
        self.expected.load(std::sync::atomic::Ordering::SeqCst)
    }

    fn completion(&self) -> TreeIndexCompletion {
        match self.completion.load(std::sync::atomic::Ordering::SeqCst) {
            1 => TreeIndexCompletion::Compile,
            2 => TreeIndexCompletion::Stopped,
            _ => TreeIndexCompletion::Pending,
        }
    }

    fn bundle_finished(&self) {
        self.bundle_finished
            .store(true, std::sync::atomic::Ordering::SeqCst);
    }

    fn bundle_is_finished(&self) -> bool {
        self.bundle_finished
            .load(std::sync::atomic::Ordering::SeqCst)
    }
}

/// Run the optional product build and publish its completion to tree admission. The handoff
/// belongs to the operation which produces `artifact`, so an all-Fresh build cannot complete
/// between observations without releasing the tree branch. A failed or absent build publishes
/// nothing.
fn build_artifact_with_handoff<T, E>(
    bundle: bool,
    tree_index: &TreeIndexHandoff,
    build: impl FnOnce() -> Result<T, E>,
) -> Option<Result<T, E>> {
    let artifact = bundle.then(build);
    if artifact.as_ref().is_some_and(Result::is_ok) {
        tree_index.bundle_finished();
    }
    artifact
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum TreeIndexAnswer {
    Ready,
    Compile,
    Stopped,
}

/// Wait for the tree compile's atomically replaced index. `pause` is injected so the ordering
/// control is deterministic; production yields briefly while the verifier child compiles.
fn await_tree_index(
    handoff: &TreeIndexHandoff,
    mut ready: impl FnMut() -> bool,
    mut stopped: impl FnMut() -> bool,
    mut pause: impl FnMut(),
) -> TreeIndexAnswer {
    if !handoff.expected() {
        return TreeIndexAnswer::Compile;
    }
    loop {
        if stopped() {
            return TreeIndexAnswer::Stopped;
        }
        match handoff.completion() {
            TreeIndexCompletion::Stopped => return TreeIndexAnswer::Stopped,
            TreeIndexCompletion::Compile => {
                return if ready() {
                    TreeIndexAnswer::Ready
                } else {
                    TreeIndexAnswer::Compile
                };
            }
            TreeIndexCompletion::Pending if ready() => return TreeIndexAnswer::Ready,
            TreeIndexCompletion::Pending => {}
        }
        pause();
    }
}

/// Publish the tree's outcome even if its verifier path panics. A panic is a red in the qualify
/// graph, so the conservative default forbids fallback fixture work.
struct FinishTreeIndex<'a> {
    handoff: &'a TreeIndexHandoff,
    completion: TreeIndexCompletion,
}

impl<'a> FinishTreeIndex<'a> {
    fn new(handoff: &'a TreeIndexHandoff) -> Self {
        Self {
            handoff,
            completion: TreeIndexCompletion::Stopped,
        }
    }

    fn observed(&mut self, result: &Result<Ran, Refusal>, stop: &BranchStop) {
        self.completion = if stop.fired()
            || interrupt::caught().is_some()
            || matches!(result, Ok(ran) if ran.code == 1)
            || matches!(result, Err(refusal) if !refusal.could_not_run)
        {
            TreeIndexCompletion::Stopped
        } else {
            TreeIndexCompletion::Compile
        };
    }
}

impl Drop for FinishTreeIndex<'_> {
    fn drop(&mut self) {
        self.handoff.finish(self.completion);
    }
}

fn with_tree_index_completion(
    handoff: &TreeIndexHandoff,
    stop: &BranchStop,
    run: impl FnOnce() -> Result<Ran, Refusal>,
) -> Result<Ran, Refusal> {
    let mut finished = FinishTreeIndex::new(handoff);
    let result = run();
    finished.observed(&result, stop);
    result
}

fn release_admission_barrier_to_build(
    barrier: &std::sync::Mutex<Option<aterm_capacity::InitialReservation<'_>>>,
) {
    if let Some(reservation) = barrier
        .lock()
        .unwrap_or_else(|error| error.into_inner())
        .take()
    {
        reservation
            .release_to_build()
            .expect("an unchanged qualify admission reservation transfers to build");
    }
}

impl Branches for RealBranches<'_> {
    fn keep_build_on_test_red(&self, c: &Candidate) -> bool {
        (self.real.build_inputs)()
            .and_then(|seal| {
                crate::check_record::keep_build_on_test_failure(&self.real.git(), &c.commit, &seal)
            })
            .unwrap_or(false)
    }
    fn say(&self, line: &str) {
        say_to(&self.real.transcript(), line);
    }

    fn now(&self) -> u64 {
        now()
    }

    fn interrupted(&self) -> bool {
        interrupt::caught().is_some()
    }

    fn branch_log(&self, branch: Branch) -> PathBuf {
        self.real.side_log(Step::Qualify, branch.name())
    }

    fn expect_tree_index(&self) {
        self.tree_index.expect();
    }

    fn build_admitted(&self, ended: &dyn Fn() -> bool) {
        let _timing = crate::cut_timing::Timer::parallel_parent()("qualify slice memory barrier")
            .with_log(Some(&self.real.side_log(Step::Qualify, "admission")));
        // With no initial build token, the slice branch cannot finish. Tree must run first even
        // when its parent holds memory, so waiting for the parent while retaining every token
        // would deadlock.
        if self
            .real
            .pool
            .as_ref()
            .is_some_and(|pool| pool.slots - pool.export < 2)
        {
            return;
        }
        let me = std::process::id();
        await_slice_memory_with_barrier(
            &self.admission_barrier,
            &|| ended() || interrupt::caught().is_some() || self.tree_index.bundle_is_finished(),
            || slices_hold_memory(me),
            || std::thread::sleep(std::time::Duration::from_secs(1)),
        );
    }

    fn tree_admitted(&self, ended: &dyn Fn() -> bool) -> bool {
        let _timing = crate::cut_timing::Timer::parallel_parent()("qualify tree memory barrier")
            .with_log(Some(&self.real.side_log(Step::Qualify, "admission")));
        let me = std::process::id();
        await_tree_memory_with_barrier(
            &self.admission_barrier,
            &|| ended() || interrupt::caught().is_some(),
            || tree_lint_claim(me),
            || std::thread::sleep(std::time::Duration::from_secs(1)),
        )
    }

    fn release_admission_barrier(&self) {
        self.admission_barrier
            .lock()
            .unwrap_or_else(|error| error.into_inner())
            .take();
    }

    fn export(
        &self,
        c: &Candidate,
        prover: &Prover,
        target: StageTarget,
        stop: &BranchStop,
    ) -> Result<Ran, Refusal> {
        let _finished = FinishShare {
            pool: self.real.pool.as_ref(),
            branch: 2,
            admission_barrier: &self.admission_barrier,
        };
        let jobs = self
            .real
            .pool
            .as_ref()
            .map(|pool| pool.export)
            .filter(|n| *n > 0);
        self.real
            .export_run(c, prover, target, Step::Qualify, Some((stop, jobs)))
    }

    fn build(
        &self,
        c: &Candidate,
        reserved: &Reservation,
        prover: &Prover,
        work: BuildWork,
        stop: &BranchStop,
    ) -> Built {
        let _finished = FinishShare {
            pool: self.real.pool.as_ref(),
            branch: 0,
            admission_barrier: &self.admission_barrier,
        };
        let start = || {
            self.real
                .build_branch(c, reserved, prover, work, &self.tree_index, stop)
        };
        if let Some(pool) = self
            .real
            .pool
            .as_ref()
            .filter(|pool| pool.slots - pool.export < 2 && (work.bundle || work.fixtures))
        {
            return start_with_share(
                pool.jobs.pool(false),
                &|| stop.fired() || interrupt::caught().is_some(),
                start,
            )
            .unwrap_or_else(|error| {
                let why = Refusal::new(
                    format!(
                        "build's initial allocation wait ended before memory admission: {error}"
                    ),
                    format!("run {RELEASE_COMMAND} again"),
                )
                .could_not_run();
                Built {
                    artifact: work.bundle.then(|| Err(why.clone())),
                    fixtures: work.fixtures.then_some(Err(why)),
                }
            });
        }
        start()
    }

    fn verify(
        &self,
        step: Step,
        c: &Candidate,
        prover: &Prover,
        run: &crate::check_record::VerifyRun,
        artifact: Option<&Artifact>,
        stop: &BranchStop,
    ) -> Result<Ran, Refusal> {
        if run.half == crate::check_record::Half::Tree {
            let _finished = FinishShare {
                pool: self.real.pool.as_ref(),
                branch: 1,
                admission_barrier: &self.admission_barrier,
            };
            with_tree_index_completion(&self.tree_index, stop, || {
                (self.real.verify_half_op)(self.real, step, c, prover, run, artifact, stop)
            })
        } else {
            (self.real.verify_half_op)(self.real, step, c, prover, run, artifact, stop)
        }
    }

    fn side_log(&self, side: Side) -> PathBuf {
        match side {
            Side::Source => self.real.step_log(Step::Publish),
            Side::Sign => self.real.side_log(Step::Publish, side.name()),
        }
    }

    fn publish_gate(&self) -> PublishGate {
        PublishGate::at(self.real.logs.join(format!(
            "{:02}-{}.gate",
            Step::Publish.number(),
            Step::Publish.name()
        )))
    }

    fn source(
        &self,
        prover: &Prover,
        gate: &PublishGate,
        stop: &BranchStop,
    ) -> Result<Ran, Refusal> {
        let mut cmd = self.real.publish_command(prover)?;
        cmd.env(PUBLISH_GATE_ENV, gate.path());
        Ok(self.real.logged_in(
            Logged {
                step: Step::Publish,
                log: self.side_log(Side::Source),
                stream: self.real.verbose,
                // The engine's KeyboardInterrupt ends its whole tree before its push.
                stop: Some((stop, interrupt::SIGINT)),
                push: None,
                arrivals: true,
                settles: false,
            },
            &mut cmd,
        ))
    }

    fn sign(&self, args: &[String], prover: &Prover, stop: &BranchStop) -> Ran {
        match self.real.cut_command(args, prover) {
            Ok(mut cmd) => self.real.logged_in(
                Logged {
                    step: Step::Publish,
                    log: self.side_log(Side::Sign),
                    stream: false,
                    stop: Some((stop, interrupt::SIGTERM)),
                    push: None,
                    arrivals: false,
                    settles: false,
                },
                &mut cmd,
            ),
            Err(why) => Ran {
                code: -1,
                detail: vec![format!("not started: {why}")],
            },
        }
    }
}

/// The slices' one shared, serial or cold-learning parent must hold before the tree can compete
/// for memory. A build that fails or serializes on a smaller machine releases the wait by ending.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum SliceMemory {
    Held,
    /// A serial parent is held: either the continuous cold-learning exclusion, or a calibrated
    /// parent after the shared plan degraded. Let build finish both slices before tree starts.
    Serial,
    /// The held parent has the correct shape but is smaller than the current complete envelope.
    StaleBudget,
    Pending,
    Unavailable,
}

/// How slice admission lets the tree proceed. A serialized build has already received every
/// non-export token and ended before this result is returned. `Ended` means the build ended
/// before a memory state could be observed, so its caller must release any remaining barrier.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum SliceAdmission {
    Concurrent,
    Serialized,
    Ended,
}

/// Wait for the slice parent without making process inspection a liveness dependency. If the
/// resource host cannot be read, release the token barrier and let the build finish before tree
/// starts. `pause` is injected so the failure path has a deterministic negative control.
fn await_slice_memory(
    ended: &dyn Fn() -> bool,
    mut observe: impl FnMut() -> SliceMemory,
    mut release: impl FnMut(),
    mut pause: impl FnMut(),
) -> SliceAdmission {
    while !ended() {
        let observe_reason = observe();
        match observe_reason {
            SliceMemory::Held => return SliceAdmission::Concurrent,
            SliceMemory::Pending => pause(),
            SliceMemory::Serial | SliceMemory::Unavailable | SliceMemory::StaleBudget => {
                if matches!(observe_reason, SliceMemory::StaleBudget) {
                    eprintln!(
                        "qualify: held slice budget is smaller than the current complete envelope; waiting for build with tree compiler tokens withheld"
                    );
                }
                release();
                while !ended() {
                    pause();
                }
                return SliceAdmission::Serialized;
            }
        }
    }
    SliceAdmission::Ended
}

/// Production binding from the exact slice-memory observation to the private-FIFO transfer.
/// Keeping the reservation in this seam makes the serial escape's token effect testable without
/// constructing a release backend.
pub(crate) fn await_slice_memory_with_barrier(
    barrier: &std::sync::Mutex<Option<aterm_capacity::InitialReservation<'_>>>,
    ended: &dyn Fn() -> bool,
    observe: impl FnMut() -> SliceMemory,
    pause: impl FnMut(),
) -> SliceAdmission {
    await_slice_memory(
        ended,
        observe,
        || release_admission_barrier_to_build(barrier),
        pause,
    )
}

pub(crate) fn slices_hold_memory(me: u32) -> SliceMemory {
    let Some(ledger) = aterm_capacity::Ledger::user() else {
        return SliceMemory::Unavailable;
    };
    let Ok(records) = ledger.list() else {
        return SliceMemory::Unavailable;
    };
    let Some(procs) = interrupt::listing() else {
        return SliceMemory::Unavailable;
    };
    slices_hold_memory_in(
        me,
        crate::slice_units::live_shared_floor(2),
        &records,
        &procs,
    )
}

/// How the build branch has reserved the slice memory which the tree must fit around. A complete
/// unit record has one shared parent. With incomplete history one cold parent stays ALONE across
/// both serial slices. A calibrated shared plan can instead degrade to its smaller serial parent.
/// An exact but invalid row authorizes neither schedule.
fn slices_hold_memory_in(
    me: u32,
    shared_minimum: u64,
    records: &[aterm_capacity::Record],
    procs: &[interrupt::Proc],
) -> SliceMemory {
    #[derive(Clone, Copy, PartialEq, Eq)]
    enum ClaimState {
        Held,
        Waiting,
        Missing,
        Invalid,
    }

    let parent: std::collections::BTreeMap<i32, i32> =
        procs.iter().map(|p| (p.pid, p.parent)).collect();
    let self_or_descendant = |pid: u32| {
        if pid == me {
            return true;
        }
        let mut at = i32::try_from(pid).unwrap_or(0);
        for _ in 0..64 {
            match parent.get(&at) {
                Some(&up) if u32::try_from(up).is_ok_and(|up| up == me) => return true,
                Some(&up) if up > 1 => at = up,
                _ => return false,
            }
        }
        false
    };
    let descendant = |what: &str, minimum: u64, exclusive: bool| {
        let mut exact = false;
        let mut valid = false;
        for record in records
            .iter()
            .filter(|record| record.need.what == what && self_or_descendant(record.pid))
        {
            exact = true;
            if record.accounting != aterm_capacity::Accounting::Holder
                || record.need.mem < minimum
                || (exclusive && record.need.mem != minimum)
                || record.need.margin != 1
                || record.need.rank != aterm_capacity::Rank::Release
                || record.need.alone != exclusive
                || record.held.is_some_and(|held| held.alone != exclusive)
            {
                continue;
            }
            valid = true;
            if record.held.is_some() {
                return ClaimState::Held;
            }
        }
        if valid {
            ClaimState::Waiting
        } else if exact {
            ClaimState::Invalid
        } else {
            ClaimState::Missing
        }
    };

    const SHARED: &str = "release slice memory budget";
    let serial_minimum = crate::slice_units::serial_parent_bytes(
        shared_minimum.saturating_sub(2 * crate::slice_units::DRIVER),
    );
    match descendant(crate::slice_units::SERIAL_PARENT, serial_minimum, false) {
        ClaimState::Held => return SliceMemory::Serial,
        ClaimState::Waiting => return SliceMemory::Pending,
        ClaimState::Invalid => {
            return if descendant(
                crate::slice_units::SERIAL_PARENT,
                crate::slice_units::serial_parent_bytes(crate::slice_units::MIN_BUDGET),
                false,
            ) == ClaimState::Held
            {
                SliceMemory::StaleBudget
            } else {
                SliceMemory::Pending
            };
        }
        ClaimState::Missing => {}
    }
    match descendant(
        crate::slice_units::COLD_PARENT,
        crate::slice_units::DRIVER,
        true,
    ) {
        ClaimState::Held => return SliceMemory::Serial,
        ClaimState::Waiting | ClaimState::Invalid => return SliceMemory::Pending,
        ClaimState::Missing => {}
    }
    match descendant(SHARED, shared_minimum, false) {
        ClaimState::Held => return SliceMemory::Held,
        ClaimState::Waiting => return SliceMemory::Pending,
        ClaimState::Missing => {}
        ClaimState::Invalid => {
            // Let the build's wrapper revalidate its admitted envelope. Keeping both FIFOs
            // empty would prevent that check and strand the build behind its own barrier.
            return if descendant(SHARED, crate::slice_units::MIN_SHARED, false) == ClaimState::Held
            {
                SliceMemory::StaleBudget
            } else {
                SliceMemory::Pending
            };
        }
    }

    SliceMemory::Pending
}

/// The pooled release tree's whole-stage Tippy reservation. Branded Tippy deliberately removes
/// outer compiler wrappers, so a driver-only claim would have no unit claims beneath it. The
/// exact workspace label distinguishes this reservation from a later narrow lint, another
/// release's row and queued lookalikes; the claim must reserve memory, cover its compiler
/// descendants and carry Release rank. Its size is the verifier's
/// ([`aterm_verify::compile_width::lint_claim`]: a measured peak, or the machine's memory alone
/// where none is kept), so no figure is repeated here.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct TreeLintClaimShape {
    pub(crate) label: String,
}

pub(crate) fn tree_lint_claim_shapes() -> Option<[TreeLintClaimShape; 1]> {
    let stage = aterm_verify::plan::declared(&aterm_verify::Scope::workspace())
        .into_iter()
        .find(|stage| stage.id == aterm_verify::plan::StageId::Tippy)?;
    Some([TreeLintClaimShape {
        label: format!("verify {}", stage.title),
    }])
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum TreeLintObservation {
    Claim(TreeLintClaim),
    Missing,
    Unavailable,
}

/// Wait for the tree's early claim. Once the slice parent is known held, an unread detector can
/// safely release the CPU barrier: the private slice budget is already protected from tree
/// claims. An immediate verifier admission failure ends the tree and reaches `ended`.
fn await_tree_memory(
    ended: &dyn Fn() -> bool,
    mut observe: impl FnMut() -> TreeLintObservation,
    mut serialize: impl FnMut(),
    mut pause: impl FnMut(),
) -> bool {
    while !ended() {
        match observe() {
            TreeLintObservation::Claim(TreeLintClaim::Held) | TreeLintObservation::Unavailable => {
                return true;
            }
            TreeLintObservation::Claim(TreeLintClaim::Waiting) => {
                serialize();
                return true;
            }
            TreeLintObservation::Claim(TreeLintClaim::Invalid) => {
                // A malformed Tippy claim may reserve less than the compile can use. Keep build's
                // private budget and stop the tree; reminting tokens to that claim after build
                // drains would otherwise let an unbounded cold compile start.
                serialize();
                return false;
            }
            TreeLintObservation::Missing => pause(),
        }
    }
    true
}

/// Production binding from a queued exact Tippy claim to the private-FIFO transfer.
fn await_tree_memory_with_barrier(
    barrier: &std::sync::Mutex<Option<aterm_capacity::InitialReservation<'_>>>,
    ended: &dyn Fn() -> bool,
    observe: impl FnMut() -> TreeLintObservation,
    pause: impl FnMut(),
) -> bool {
    await_tree_memory(
        ended,
        observe,
        || release_admission_barrier_to_build(barrier),
        pause,
    )
}

fn tree_lint_claim(me: u32) -> TreeLintObservation {
    let Some(shapes) = tree_lint_claim_shapes() else {
        return TreeLintObservation::Unavailable;
    };
    let Some(ledger) = aterm_capacity::Ledger::user() else {
        return TreeLintObservation::Unavailable;
    };
    let Ok(records) = ledger.list() else {
        return TreeLintObservation::Unavailable;
    };
    let Some(procs) = interrupt::listing() else {
        return TreeLintObservation::Unavailable;
    };
    tree_lint_claim_in(me, &records, &procs, &shapes)
        .map(TreeLintObservation::Claim)
        .unwrap_or(TreeLintObservation::Missing)
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum TreeLintClaim {
    /// The whole-lint claim stands beside the slice parent: one tree compiler may overlap build
    /// under the build-first split.
    Held,
    /// The whole-lint claim is queued. Release CPU tokens so build can finish and
    /// free its parent; lint remains bounded and then runs serially on a smaller host.
    Waiting,
    /// A descendant published the exact production label but not its memory shape, rank,
    /// coverage or accounting. It cannot open concurrent tree work: transfer the bounded CPU
    /// allocation to build and stop the tree before those tokens can be reminted to it.
    Invalid,
}

fn tree_lint_claim_in(
    me: u32,
    records: &[aterm_capacity::Record],
    procs: &[interrupt::Proc],
    shapes: &[TreeLintClaimShape],
) -> Option<TreeLintClaim> {
    let parent: std::collections::BTreeMap<i32, i32> = procs
        .iter()
        .map(|process| (process.pid, process.parent))
        .collect();
    let descends = |pid: u32| {
        let mut at = i32::try_from(pid).unwrap_or(0);
        for _ in 0..64 {
            match parent.get(&at) {
                Some(&up) if u32::try_from(up).is_ok_and(|up| up == me) => return true,
                Some(&up) if up > 1 => at = up,
                _ => return false,
            }
        }
        false
    };
    let mut outcome = None;
    for record in records.iter().filter(|record| descends(record.pid)) {
        let Some(this) = classify_tree_lint_claim(record, shapes) else {
            continue;
        };
        // Duplicate live rows can briefly coexist while a process hands a claim on. Their file
        // order is not policy: a held valid row dominates a queued one, and either dominates a
        // malformed row with the same production label.
        outcome = Some(stronger_tree_lint_claim(outcome, this));
    }
    outcome
}

pub(crate) fn classify_tree_lint_claim(
    record: &aterm_capacity::Record,
    shapes: &[TreeLintClaimShape],
) -> Option<TreeLintClaim> {
    shapes
        .iter()
        .find(|shape| record.need.what == shape.label)?;
    Some(
        if record.accounting == aterm_capacity::Accounting::Holder
            && record.need.mem > 0
            && record.need.rank == aterm_capacity::Rank::Release
            && record.need.covers
        {
            if record.held.is_some() {
                TreeLintClaim::Held
            } else {
                TreeLintClaim::Waiting
            }
        } else {
            TreeLintClaim::Invalid
        },
    )
}

pub(crate) const fn stronger_tree_lint_claim(
    current: Option<TreeLintClaim>,
    this: TreeLintClaim,
) -> TreeLintClaim {
    match (current, this) {
        (Some(TreeLintClaim::Held), _) | (_, TreeLintClaim::Held) => TreeLintClaim::Held,
        (Some(TreeLintClaim::Waiting), _) | (_, TreeLintClaim::Waiting) => TreeLintClaim::Waiting,
        _ => TreeLintClaim::Invalid,
    }
}

struct Forwarded {
    _running: interrupt::Running,
    meter: Option<aterm_verify::exec::UsageMeter>,
    note: Option<PathBuf>,
}

impl Drop for Forwarded {
    fn drop(&mut self) {
        if let Some(note) = &self.note {
            let _ = std::fs::remove_file(note);
        }
    }
}

impl Drop for Real {
    fn drop(&mut self) {
        if let Some(mut child) = self.automatic_prefreeze.take() {
            // The clone's singleton supervisor owns preparation between cuts too.
            // Reap a finished supervisor; a live one is reparented on this image's
            // exit and stops at the next release-line claim.
            let _ = child.try_wait();
            self.say(&format!(
                "release-event: {}",
                aterm_json::json!({
                    "schema":1,"event":"prefreeze-handed-on","timing_scope":"next-cut",
                })
            ));
        }
        drop(self.warming.take());
        if self.scratch_logs {
            let _ = std::fs::remove_dir_all(&self.logs);
        }
    }
}

impl Real {
    /// The backend for the clone at `repo`.
    /// # Errors
    /// No HOME, or a log directory that cannot be made.
    pub fn new(
        repo: &Path,
        engine: Option<&Path>,
        dry: bool,
        verbose: bool,
    ) -> Result<Self, String> {
        let _timing = crate::cut_timing::Timer::new("release-backend-init");
        let home = std::env::var_os("HOME")
            .filter(|h| !h.is_empty())
            .map(PathBuf::from)
            .ok_or("HOME is not set")?;
        // A release's own frozen cutter takes the source the release that execed it named
        // (ENGINE_SOURCE_ENV), not the copy it is also handed as PUBLICATION_ENGINE.
        let source = engine.map(Path::to_path_buf).unwrap_or_else(|| {
            [ENGINE_SOURCE_ENV, "PUBLICATION_ENGINE"]
                .into_iter()
                .find_map(|name| std::env::var_os(name).filter(|v| !v.is_empty()))
                .map_or_else(|| home.join("publication"), PathBuf::from)
        });
        let engine_source = std::env::current_dir().map_or_else(
            |_| source.clone(),
            |cwd| crate::release::anchor(&source, &cwd),
        );
        let engine_repository = engine_authority(
            crate::github_remote::origin_slug(&engine_source),
            std::env::var_os(ENGINE_REPOSITORY_ENV),
            std::env::var_os(ENGINE_REPOSITORY_WHY_ENV),
        );
        let release = aterm_verify::engine::release_dir(repo);
        let copy = aterm_verify::engine::engine_copy(&release);
        let engine = if dry && !copy.join(".git").exists() {
            engine_source.clone()
        } else {
            copy
        };
        let state = release_state(&home);
        let started = now();
        let offset = i64::try_from(started)
            .ok()
            .and_then(aterm_uds::zone::local_offset_at)
            .unwrap_or(0);
        let stamp = local_stamp(started, offset);
        let logs = if dry {
            static NTH: std::sync::atomic::AtomicU32 = std::sync::atomic::AtomicU32::new(0);
            let nth = NTH.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
            dry_log_dir(&std::env::temp_dir(), std::process::id(), nth)
        } else {
            state
                .join(aterm_tempfile::QUIET)
                .join("logs.noindex")
                .join(format!("{stamp}-{}", std::process::id()))
        };
        std::fs::create_dir_all(&logs).map_err(|e| format!("create {}: {e}", logs.display()))?;
        Ok(Self {
            prefreeze_claim: None,
            prefrozen_step1: None,
            automatic_prefreeze: None,
            build_inputs: crate::build_inputs::current,
            arm_inputs: crate::build_inputs::arm,
            arm_prefrozen_inputs: crate::build_inputs::arm_prefrozen,
            handed_inputs: crate::build_inputs::handed_path,
            prefrozen_inputs: crate::build_inputs::prefrozen_seal,
            verify_half_op: Real::verify_half,
            build_run_op: Real::build_run,
            await_fixture_index_op: Real::await_fixture_index,
            stage_fixtures_run_op: Real::stage_fixtures_run_with_stop,
            repo: repo.to_path_buf(),
            home,
            engine_source,
            engine,
            engine_repository,
            engine_url: engine_origin_url,
            origin_main: None,
            engine_synced: std::sync::atomic::AtomicBool::new(false),
            state,
            logs,
            release,
            scratch_logs: dry,
            verbose,
            origin_tags: String::new(),
            source_origin: crate::github_remote::Remote::origin,
            staging_file: crate::github_remote::repository_file,
            release_exists: public_release_exists,
            prefetched_origin: None,
            step: Step::Checks,
            argv: std::env::args_os().skip(1).collect(),
            source_signing: None,
            prerequisites: None,
            prerequisite_acquire: crate::managed_prerequisites::Tools::acquire,
            prepared_source_helper: None,
            scanner_tools: None,
            scanner_probe: scanner_tools::run,
            checkout_build: Real::build_checkout_bin,
            engine_origin: |repo, _| crate::github_remote::Remote::origin(repo),
            engine_agh_explicit: std::env::var_os(ENGINE_AGH_ENV),
            disk_claim: None,
            line_priority: None,
            fleet_line: None,
            fleet_holder: None,
            pool: None,
            warming: None,
            warm_verifier: None,
        })
    }

    fn git(&self) -> GitCli {
        GitCli::new(&self.repo)
    }

    fn git_out(&self, args: &[&str]) -> Option<String> {
        let out = self.git().git(args).ok()?;
        out.success().then(|| out.stdout_utf8().trim().to_string())
    }

    /// Admit complete Git output as unquoted NUL-delimited relative paths. GitCli keeps
    /// its existing process/watch semantics; its child capture is not made bounded here.
    fn git_paths(&self, args: &[&str]) -> Result<Vec<String>, String> {
        let out = self
            .git()
            .git(args)
            .map_err(|why| format!("could not enumerate Git paths: {why}"))?;
        if !out.success() {
            return Err(format!("Git path enumeration exited {}", out.status));
        }
        nested_locks::paths(&out.stdout)
    }

    fn engine_git(&self, args: &[&str]) -> Option<String> {
        let mut cmd = local_git().ok()?;
        cmd.args(["-c", "protocol.allow=never"])
            .arg("-C")
            .arg(&self.engine)
            .args(args);
        let out = self.output_forwarded(&mut cmd).ok()?;
        out.status
            .success()
            .then(|| String::from_utf8_lossy(&out.stdout).into_owned())
    }

    fn engine_git_code(&self, args: &[&str]) -> Result<(i32, std::process::Output), String> {
        let e = self.engine.display();
        let mut cmd = crate::ledger::git_command().map_err(|why| why.to_string())?;
        cmd.args(["-c", "protocol.allow=never"])
            .arg("-C")
            .arg(&self.engine)
            .args(args);
        let out = self
            .output_forwarded(&mut cmd)
            .map_err(|err| format!("git -C {e} {}: {err}", args.join(" ")))?;
        let code = out
            .status
            .code()
            .ok_or_else(|| format!("git -C {e} {} was ended by a signal", args.join(" ")))?;
        Ok((code, out))
    }

    fn engine_commit(&self, rev: &str) -> Result<Option<String>, String> {
        let e = self.engine.display();
        let spec = format!("{rev}^{{commit}}");
        let (code, out) = self.engine_git_code(&["rev-parse", "--verify", "-q", &spec])?;
        let sha = String::from_utf8_lossy(&out.stdout).trim().to_string();
        match code {
            0 if !sha.is_empty() => Ok(Some(sha)),
            1 if sha.is_empty() => Ok(None),
            _ => Err(format!(
                "git -C {e} rev-parse {spec} exited {code}: {}",
                String::from_utf8_lossy(&out.stderr).trim()
            )),
        }
    }

    fn spawn_forwarded(
        &self,
        cmd: &mut Command,
    ) -> std::io::Result<(aterm_uds::spawnfd::Child, Forwarded)> {
        self.spawn_stoppable(self.step, cmd, None)
    }

    fn start_preparing_sign(&self, mut cmd: Command) -> Result<PreparingSign, String> {
        let log = self
            .step_log(Step::Bundle)
            .with_file_name("05-sign-prepare.log");
        let mut file = crate::durable::open_append(&log, true).map_err(|e| e.to_string())?;
        let from = file.metadata().map_err(|e| e.to_string())?.len();
        writeln!(file, "$ {cmd:?}").map_err(|e| e.to_string())?;
        cmd.stdin(Stdio::null())
            .stdout(file.try_clone().map_err(|e| e.to_string())?)
            .stderr(file.try_clone().map_err(|e| e.to_string())?);
        let stop = BranchStop::default();
        let started = now_ms();
        let (child, forwarded) = self
            .spawn_stoppable(Step::Bundle, &mut cmd, Some((&stop, interrupt::SIGTERM)))
            .map_err(|e| e.to_string())?;
        cmd.stdout(Stdio::null()).stderr(Stdio::null());
        Ok(PreparingSign {
            child: Some(child),
            _forwarded: forwarded,
            stop,
            cmd,
            log,
            file,
            from,
            started,
        })
    }

    /// [`Real::spawn_forwarded`] for a child of `step` (noted as that step's while it runs), which
    /// a branch's `stop` ends too (sent its signal, then `SIGKILL`); none starts once that stop
    /// fired.
    fn spawn_stoppable(
        &self,
        step: Step,
        cmd: &mut Command,
        stop: Option<(&BranchStop, i32)>,
    ) -> std::io::Result<(aterm_uds::spawnfd::Child, Forwarded)> {
        if interrupt::caught().is_some() {
            return Err(std::io::Error::new(
                std::io::ErrorKind::Interrupted,
                "not started: the release was interrupted",
            ));
        }
        if stop.is_some_and(|(stop, _)| stop.fired()) {
            return Err(std::io::Error::new(
                std::io::ErrorKind::Interrupted,
                "not started: another branch went red",
            ));
        }
        self.spawn_noted(step, cmd, |group| match stop {
            Some((stop, sig)) => interrupt::Running::enter_stoppable(group, stop.clone(), sig),
            None => interrupt::Running::enter(group),
        })
    }

    /// [`Real::spawn_stoppable`] for a child that settles this process's own stop
    /// ([`interrupt::stop_self`]: the fleet line lost), started after it: it starts unless a
    /// signal from elsewhere was caught, and its group is forwarded only such a signal
    /// ([`interrupt::Running::settling`]), never the stop it settles.
    fn spawn_settling(
        &self,
        step: Step,
        cmd: &mut Command,
    ) -> std::io::Result<(aterm_uds::spawnfd::Child, Forwarded)> {
        if interrupt::caught_foreign() {
            return Err(std::io::Error::new(
                std::io::ErrorKind::Interrupted,
                "not started: the release was interrupted",
            ));
        }
        self.spawn_noted(step, cmd, interrupt::Running::settling)
    }

    /// Start `cmd` as `step`'s child, leading a session of its own (inside preparation's group
    /// while a prefreeze claim is held, [`step_process_group`]), noted while it runs, and its
    /// group forwarded caught signals by the guard `forward` starts for that group.
    fn spawn_noted(
        &self,
        step: Step,
        cmd: &mut Command,
        forward: impl FnOnce(u32) -> interrupt::Running,
    ) -> std::io::Result<(aterm_uds::spawnfd::Child, Forwarded)> {
        self.finalize_spawn_environment(cmd)
            .map_err(std::io::Error::other)?;
        let prefreeze = self.prefreeze_claim.is_some();
        step_process_group(cmd, prefreeze);
        let child = cmd.spawn()?;
        let group = if prefreeze {
            std::process::id()
        } else {
            child.id()
        };
        let running = forward(group);
        let note = self.note_child(step, child.id());
        let meter = Some(aterm_verify::exec::UsageMeter::start(child.id()));
        Ok((
            child,
            Forwarded {
                _running: running,
                meter,
                note,
            },
        ))
    }

    fn output_forwarded(&self, cmd: &mut Command) -> std::io::Result<std::process::Output> {
        cmd.stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped());
        let (child, forwarded) = self.spawn_forwarded(cmd)?;
        let out = child.wait_with_output();
        drop(forwarded);
        out
    }

    /// [`Real::output_forwarded`] for a child of `step` that does a step's work, with the
    /// `step-child-finished` record of its sampled descendants ([`step_child_event`]).
    fn output_accounted(
        &self,
        step: Step,
        cmd: &mut Command,
    ) -> std::io::Result<(std::process::Output, String)> {
        self.output_accounted_with_stop(step, cmd, None)
    }

    fn output_accounted_with_stop(
        &self,
        step: Step,
        cmd: &mut Command,
        stop: Option<&BranchStop>,
    ) -> std::io::Result<(std::process::Output, String)> {
        cmd.stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped());
        let started_at_unix_ms = now_ms();
        let (mut child, mut forwarded) =
            self.spawn_stoppable(step, cmd, stop.map(|stop| (stop, interrupt::SIGTERM)))?;
        let drain = |pipe: Option<Box<dyn std::io::Read + Send>>| {
            std::thread::spawn(move || {
                let mut bytes = Vec::new();
                if let Some(mut pipe) = pipe {
                    let _ = pipe.read_to_end(&mut bytes);
                }
                bytes
            })
        };
        let stdout = drain(child.stdout.take().map(|p| Box::new(p) as _));
        let stderr = drain(child.stderr.take().map(|p| Box::new(p) as _));
        let waited = wait_accounted(&mut child, &mut forwarded);
        let ended_at_unix_ms = now_ms();
        drop(forwarded);
        let (status, usage) = waited?;
        let output = std::process::Output {
            status,
            stdout: stdout.join().unwrap_or_default(),
            stderr: stderr.join().unwrap_or_default(),
        };
        let record = step_child_event(
            step,
            cmd,
            started_at_unix_ms,
            ended_at_unix_ms,
            status.code().unwrap_or(-1),
            usage,
        );
        Ok((output, record))
    }

    fn slug(&self) -> String {
        crate::publish::workspace_channel_slug(&self.repo)
            .unwrap_or_else(|_| "alabsystems/aterm".to_string())
    }

    fn scrubbed(&self, program: impl AsRef<std::ffi::OsStr>) -> Command {
        self.scrubbed_command(publication_command(&self.engine, program))
    }

    /// The image [`Backend::exec_cutter`] execs in place: the frozen `cutter` with this release's
    /// arguments, its record, and the engine source and repository it captured
    /// ([`hand_engine_capture`]), without Trust's `targo` handovers; the exec adds the freezer's
    /// envelope ([`crate::release_tools::Recovery::hand`]) and its descriptor guards.
    fn cutter_command(&self, cutter: &Path) -> Command {
        let mut command = self.scrubbed(cutter);
        command.args(&self.argv).current_dir(&self.repo).env(
            crate::release_tools::RECORD_ENV,
            crate::release_tools::cutter_record_path(&self.release),
        );
        hand_engine_capture(&mut command, &self.engine_source, &self.engine_repository);
        for name in std::env::vars_os().map(|(name, _)| name).filter(|name| {
            name.to_str()
                .is_some_and(|name| name.starts_with("TRUST_TARGO_"))
        }) {
            command.env_remove(name);
        }
        command
    }

    /// The exact command used at the cutter handover, including the step-1 seal selected with
    /// the pre-frozen cutter. Keeping this boundary shared with the adoption test prevents the
    /// exec path from silently dropping the prepared record.
    fn cutter_command_with_inputs(
        &self,
        cutter: &Path,
        prefrozen_inputs: Option<PathBuf>,
    ) -> Command {
        let mut command = self.cutter_command(cutter);
        let inputs = prefrozen_inputs.or_else(|| (self.handed_inputs)());
        hand_inputs_seal(&mut command, inputs);
        command
    }

    fn scrubbed_command(&self, mut cmd: Command) -> Command {
        if let Some(tools) = &self.source_signing {
            tools.apply(&mut cmd);
        }
        hand_inputs_seal(&mut cmd, (self.handed_inputs)());
        if let Some(claim) = &self.disk_claim {
            cmd.env(aterm_capacity::DISK_COVER_ENV, claim.marker());
        }
        cmd
    }

    /// Hand the build or tree its private FIFO. Bundle checks reuse tree's FIFO after qualify,
    /// when the completed branches have returned the entire grant there.
    fn hand_pool(&self, cmd: &mut Command, tree: bool) -> Result<(), Refusal> {
        let Some(pool) = &self.pool else {
            return Ok(());
        };
        let slots = pool.slots;
        pool.jobs.hand_to(cmd, tree).map_err(|e| {
            Refusal::new(
                format!(
                    "the qualify graph's jobserver cannot hand a branch share of its {slots} \
                     slots: {e} — nothing ran"
                ),
                format!("run {RELEASE_COMMAND} again"),
            )
            .could_not_run()
        })
    }

    /// The engine's operation with the export guards' scanner tools this run prepared
    /// ([`Real::prepare_scanner_tools`]): a stage's and a publish's, no other child's.
    fn with_scanners(&self, mut cmd: Command) -> Command {
        if let Some(tools) = &self.scanner_tools {
            tools.apply(&mut cmd);
        }
        cmd
    }

    fn prepare_prerequisites(&mut self) -> Result<(), Refusal> {
        if self.prerequisites.is_none() {
            self.prerequisites = Some(
                (self.prerequisite_acquire)(
                    crate::managed_prerequisites::PUBLICATION,
                    "release publication operation",
                )
                .map_err(prerequisite_refusal)?,
            );
        }
        Ok(())
    }

    /// Sync may select a newer engine than step 1 observed. Recheck only that mutable rule.
    fn engine_readiness(&self) -> Result<(), Refusal> {
        let seam = crate::cut_timing::measure("commit-gate engine readiness", || {
            self.engine_text("bin/pub")
                .is_some_and(|t| t.contains("run_pre_promote_hook"))
        });
        if seam {
            return Ok(());
        }
        Err(Refusal::new(
            format!(
                "the publication engine's main ({}'s origin) has no pre-promote seam \
                 (`run_pre_promote_hook` in its bin/pub): `pub publish` would not run \
                 publish/pre-promote, and a publish by hand would promote R's attestation \
                 before it is verified",
                self.engine_source.display()
            ),
            "name a publication engine whose origin's main carries the seam (on the \
             engine's main since 2026-09-29) with --engine; the release syncs its own copy \
             of it",
        ))
    }

    fn apply_prerequisites(&self, cmd: &mut Command) -> Result<(), String> {
        if let Some(tools) = &self.prerequisites {
            tools.apply(cmd)?;
        }
        Ok(())
    }

    /// Apply every environment mutation made at the last boundary before a release child
    /// spawns. Content evidence calls this same finalizer on a one-use command mirror.
    fn finalize_spawn_environment(&self, cmd: &mut Command) -> Result<(), String> {
        self.apply_prerequisites(cmd)?;
        crate::credential_wait::ask_no_one(cmd);
        Ok(())
    }

    fn engine_prefix_len(&self) -> usize {
        self.prerequisites
            .as_ref()
            .map_or(0, crate::managed_prerequisites::Tools::engine_prefix_len)
    }

    fn child(&self, program: impl AsRef<std::ffi::OsStr>, prover: Option<&Prover>) -> Command {
        self.configure_child(self.scrubbed(program), prover)
    }

    /// The release's own tree fills idle cores behind its slices. Tree verification is reached
    /// only through the qualify graph, but its content plan is made before that graph installs
    /// the pool. Select the invocation from the half alone so the pre-pool plan predicts the exact
    /// child that later runs. A global Background claim would be cancellable by this release.
    fn verifier_child(
        &self,
        program: &Path,
        prover: Option<&Prover>,
        half: crate::check_record::Half,
    ) -> Command {
        let lower = half == crate::check_record::Half::Tree;
        let (program, prefix) = crate::qualify_pilot::invocation(program, lower);
        let mut command = self.child(program, prover);
        command.args(prefix);
        command
    }

    fn configure_child(&self, mut cmd: Command, prover: Option<&Prover>) -> Command {
        cmd.current_dir(&self.repo).stdin(Stdio::null());
        if let Some(claim) = &self.prefreeze_claim {
            configure_prefreeze_child(&mut cmd, &claim.record().cover_marker(), claim.cores());
        }
        if let Some(p) = prover {
            cmd.env("TRUST_STAGE2_BIN", &p.bin);
        }
        cmd
    }

    /// Resolve every edited lock once, refusing any additional edit the resolver needs.
    fn validate_bumped_locks(&self, prover: &Prover, locks: &[String]) -> Result<(), String> {
        for lock in locks {
            let manifest = self.repo.join(lock).with_file_name("Cargo.toml");
            let mut cmd = self.child(prover.bin.join("targo"), Some(prover));
            cmd.args([
                "metadata",
                "--locked",
                "--offline",
                "--format-version",
                "1",
                "--manifest-path",
            ])
            .arg(manifest);
            let (output, accounting) = self
                .output_accounted(Step::Commit, &mut cmd)
                .map_err(|e| format!("validate {lock}: {e}"))?;
            if let Ok(mut log) = crate::durable::open_append(&self.step_log(Step::Commit), true) {
                let _ = writeln!(log, "lock-bump-validation: {lock}\n{accounting}");
                let _ = log.write_all(&output.stderr);
            }
            if !output.status.success() {
                return Err(format!(
                    "locked metadata refused {lock}: {}",
                    String::from_utf8_lossy(&output.stderr)
                ));
            }
        }
        Ok(())
    }

    fn engine_child(&self, engine: &Path, prover: Option<&Prover>) -> Result<Command, Refusal> {
        let tools = self.prerequisites.as_ref().ok_or_else(|| {
            prerequisite_refusal("publication prerequisites were not acquired".into())
        })?;
        let mut cmd = tools
            .engine_command(&engine.join("bin/pub"))
            .map_err(prerequisite_refusal)?;
        publication_environment(engine, &mut cmd);
        Ok(self.configure_child(self.scrubbed_command(cmd), prover))
    }

    fn tap_command(&self, site_tag: &str) -> Result<Command, Refusal> {
        let version = site_tag.strip_prefix('v').ok_or_else(|| {
            Refusal::new(
                "the validated site cohort has no version tag",
                "read the after-cut log",
            )
        })?;
        crate::publish::release_version_from_workspace(version).map_err(|why| {
            Refusal::new(
                format!("invalid site cohort for the tap: {why}"),
                "read the after-cut log",
            )
        })?;
        let mut command = self.engine_child(&self.engine, None)?;
        command.args(["tap", "--bump", "--expect-version", version]);
        Ok(command)
    }

    fn align_tap(&self, site_tag: &str) -> Result<String, Refusal> {
        let mut command = self.tap_command(site_tag)?;
        let ran = self.logged_in(
            Logged {
                step: Step::AfterCut,
                log: self.side_log(Step::AfterCut, "tap"),
                stream: self.verbose,
                stop: None,
                push: None,
                arrivals: false,
                settles: false,
            },
            &mut command,
        );
        if ran.code != 0 {
            return Err(Refusal::new(
                format!(
                    "canonical tap alignment for {site_tag} did not finish (exit {})",
                    ran.code
                ),
                format!("{RELEASE_COMMAND} again after resolving the after-cut log's refusal"),
            )
            .detail(ran.detail));
        }
        Ok(format!("the canonical tap is aligned to {site_tag}"))
    }

    /// This cutter again with `args` (the build and cut steps): [`Real::cut_command_of`] this
    /// process's executable.
    fn cut_command(&self, args: &[String], prover: &Prover) -> Result<Command, String> {
        self.cut_command_of(std::env::current_exe(), args, prover)
    }

    /// The cutter at `exe`, this process's executable, with `args`: a [`Real::child`] with
    /// `prover`'s bin, handed itself ([`hand_this_cutter`]) so its own start judgment dates its
    /// inputs from the moment this one's did, not from its link: it is a child of this process,
    /// not of a `targo`, and the cut gate in it reads that judgment. When this process cannot
    /// name its executable, why, and no command: an `aterm-release` found on `PATH` is no cutter
    /// this process knows to be itself.
    fn cut_command_of(
        &self,
        exe: std::io::Result<PathBuf>,
        args: &[String],
        prover: &Prover,
    ) -> Result<Command, String> {
        let exe = exe.map_err(|e| format!("this cutter cannot name its own executable ({e})"))?;
        let mut cmd = self.child(&exe, Some(prover));
        cmd.args(args);
        hand_this_cutter(&mut cmd, &exe, crate::cutter::reference());
        Ok(cmd)
    }

    fn publish_command(&self, prover: &Prover) -> Result<Command, Refusal> {
        let mut cmd = self.with_scanners(self.engine_child(&self.engine, Some(prover))?);
        cmd.args(["publish", "aterm", "--keep-draft", "--draft-state"])
            .arg(crate::publish::draft_journal_path(&self.repo))
            .args([
                "--skip-site",
                "--release-remote",
                "https://github.com/alabsystems/aterm.git",
                "--repo-path",
            ])
            .arg(&self.repo);
        if let Ok(exe) = std::env::current_exe() {
            hand_this_cutter(&mut cmd, &exe, crate::cutter::reference());
        }
        Ok(cmd)
    }

    /// Run `cmd` as a step's child, through [`Real::spawn_forwarded`] — its output into `step`'s log (and, as it arrives, to the terminal under `--verbose`).
    pub(crate) fn logged(&self, step: Step, cmd: &mut Command) -> Ran {
        let stream = self.verbose;
        self.logged_to(step, cmd, stream)
    }

    /// [`Real::logged`] for a child that settles this process's own stop
    /// ([`interrupt::stop_self`]: the fleet line lost): that stop neither refuses it nor is
    /// forwarded to it; a signal from elsewhere does both ([`Real::spawn_settling`]).
    fn logged_settling(&self, step: Step, cmd: &mut Command) -> Ran {
        self.logged_in(
            Logged {
                step,
                log: self.step_log(step),
                stream: self.verbose,
                stop: None,
                push: None,
                arrivals: false,
                settles: true,
            },
            cmd,
        )
    }

    fn logged_to(&self, step: Step, cmd: &mut Command, stream: bool) -> Ran {
        self.logged_in(
            Logged {
                step,
                log: self.step_log(step),
                stream,
                stop: None,
                push: None,
                arrivals: step == Step::Publish,
                settles: false,
            },
            cmd,
        )
    }

    /// Run `cmd` as a child of `at.step` into `at.log`, its output streamed when `at.stream`,
    /// ended by `at.stop` too, and its lines read by `at.push`.
    fn logged_in(&self, at: Logged<'_>, cmd: &mut Command) -> Ran {
        #[cfg(test)]
        let ran = self.logged_in_with_drain(at, cmd, LOG_DRAIN_AFTER_EXIT, None);
        #[cfg(not(test))]
        let ran = self.logged_in_with_drain(at, cmd, LOG_DRAIN_AFTER_EXIT);
        ran
    }

    fn logged_in_with_drain(
        &self,
        at: Logged<'_>,
        cmd: &mut Command,
        drain: std::time::Duration,
        #[cfg(test)] pump_active: Option<std::sync::Arc<std::sync::atomic::AtomicBool>>,
    ) -> Ran {
        let Logged {
            step,
            log,
            stream,
            stop,
            push,
            arrivals,
            settles,
        } = at;
        let mut file = match crate::durable::open_append(&log, true) {
            Ok(f) => f,
            Err(e) => {
                return Ran {
                    code: -1,
                    detail: vec![format!("cannot open {}: {e}", log.display())],
                };
            }
        };
        let from = match file.metadata() {
            Ok(metadata) => metadata.len(),
            Err(error) => {
                return Ran {
                    code: -1,
                    detail: vec![format!("cannot size {}: {error}", log.display())],
                };
            }
        };
        let _ = writeln!(file, "$ {cmd:?}");
        let not_started = || Ran {
            code: 130,
            detail: vec!["not started: the release was interrupted".into()],
        };
        let interrupted = if settles {
            interrupt::caught_foreign()
        } else {
            interrupt::caught().is_some()
        };
        if interrupted {
            return not_started();
        }
        cmd.stdin(Stdio::null());
        // The publish's lines are always read as they arrive: when each came is the only split
        // of the engine's time (`publish-line` records, A1), whether or not they echo.
        let arrivals = (arrivals && !self.scratch_logs)
            .then(|| crate::durable::open_append(&self.side_log(step, "arrivals"), true).ok())
            .flatten();
        let pipe = if stream || arrivals.is_some() || push.is_some() {
            let made = LoggedPipe::new();
            match made {
                Ok((pipe, out, err)) => {
                    cmd.stdout(out).stderr(err);
                    Some(pipe)
                }
                Err(e) => {
                    let _ = writeln!(file, "pipe: {e}");
                    return Ran {
                        code: -1,
                        detail: vec![format!("cannot make a pipe: {e}")],
                    };
                }
            }
        } else {
            let (Ok(out), Ok(err)) = (file.try_clone(), file.try_clone()) else {
                return Ran {
                    code: -1,
                    detail: vec![format!("cannot write {}", log.display())],
                };
            };
            cmd.stdout(out).stderr(err);
            None
        };
        let started_at_unix_ms = now_ms();
        let spawned = if settles {
            self.spawn_settling(step, cmd)
        } else {
            self.spawn_stoppable(step, cmd, stop)
        };
        cmd.stdout(Stdio::null()).stderr(Stdio::null());
        let (mut child, mut forwarded) = match spawned {
            Ok(spawned) => spawned,
            Err(e) if e.kind() == std::io::ErrorKind::Interrupted => {
                return Ran {
                    code: 130,
                    detail: vec![e.to_string()],
                };
            }
            Err(e) => {
                let _ = writeln!(file, "spawn: {e}");
                return Ran {
                    code: -1,
                    detail: vec![format!("spawn: {e}")],
                };
            }
        };
        let pumped = pipe.and_then(|pipe| {
            let mut log = file.try_clone().ok()?;
            let (done, finished) = std::sync::mpsc::channel::<()>();
            let mut arrivals = arrivals.map(Arrivals::new);
            let mut push = push;
            #[cfg(test)]
            let pump_active = PumpActive::new(pump_active);
            let LoggedPipe {
                mut reader,
                #[cfg(unix)]
                cancel_reader,
                #[cfg(unix)]
                cancel_writer,
            } = pipe;
            let thread = std::thread::Builder::new()
                .name("release-output-pump".into())
                .spawn(move || {
                    #[cfg(test)]
                    let _pump_active = pump_active;
                    let mut buf = [0u8; 8192];
                    loop {
                        #[cfg(unix)]
                        {
                            use std::os::fd::AsFd as _;

                            match aterm_uds::ready::either(reader.as_fd(), cancel_reader.as_fd()) {
                                Ok(ready) if !ready.second => {}
                                Ok(_) | Err(_) => break,
                            }
                        }
                        let Ok(n) = std::io::Read::read(&mut reader, &mut buf) else {
                            break;
                        };
                        if n == 0 {
                            break;
                        }
                        let _ = log.write_all(&buf[..n]);
                        if stream {
                            let _ = std::io::stdout().write_all(&buf[..n]);
                        }
                        if let Some(arrivals) = &mut arrivals {
                            arrivals.read(&buf[..n]);
                        }
                        if let Some(push) = &mut push {
                            push.read(&buf[..n]);
                        }
                    }
                    let _ = done.send(());
                })
                .ok()?;
            Some(OutputPump {
                finished,
                thread,
                #[cfg(unix)]
                cancel: cancel_writer,
            })
        });
        let waited = wait_accounted(&mut child, &mut forwarded);
        let ended_at_unix_ms = now_ms();
        let code = waited
            .as_ref()
            .map_or(-1, |(status, _)| status.code().unwrap_or(-1));
        if let Some(pumped) = pumped {
            let _drain = crate::cut_timing::Timer::start(&log, "step child output drain");
            pumped.finish(drain);
        }
        drop(forwarded);
        let usage = waited.ok().and_then(|(_, usage)| usage);
        let _ = writeln!(
            file,
            "{}",
            step_child_event(step, cmd, started_at_unix_ms, ended_at_unix_ms, code, usage)
        );
        Ran {
            code,
            detail: tail_since(&log, 8, from),
        }
    }

    fn note_child(&self, step: Step, pid: u32) -> Option<PathBuf> {
        (!self.scratch_logs).then(|| note_child_in(&self.release, step, pid))
    }

    fn step_log(&self, step: Step) -> PathBuf {
        self.logs
            .join(format!("{:02}-{}.log", step.number(), step.name()))
    }

    /// A refusal of the bump's edit whose cause the commit step's log holds; `undone` says the
    /// edit was rolled back. A run that keeps its logs names that log. A dry run's logs are
    /// scratch, which `Drop` removes when the run ends, so its refusal carries the log's last
    /// [`COMMIT_LOG_TAIL`] lines instead, which a dry refusal prints above its fix line, or says
    /// that the reason it gives is all it recorded.
    fn commit_log_refusal(&self, why: impl Into<String>, undone: bool) -> Refusal {
        let undone = if undone { "; the bump is undone" } else { "" };
        if !self.scratch_logs {
            return Refusal::new(why, format!("read the commit step's log{undone}"));
        }
        let text = crate::durable::read_text(&self.step_log(Step::Commit)).unwrap_or_default();
        let mut tail: Vec<String> = text
            .lines()
            .rev()
            .take(COMMIT_LOG_TAIL)
            .map(str::to_owned)
            .collect();
        tail.reverse();
        let remedy = if tail.is_empty() {
            format!("the reason above is all this dry run recorded{undone}")
        } else {
            format!("read the commit step's output above; a dry run keeps no log{undone}")
        };
        Refusal::new(why, remedy).detail(tail)
    }

    /// The run's own log: what the cutter says ([`Backend::say`]), beside each step's log.
    fn transcript(&self) -> PathBuf {
        self.logs.join(crate::release_timeline::TRANSCRIPT)
    }

    fn side_log(&self, step: Step, what: &str) -> PathBuf {
        self.logs
            .join(format!("{:02}-{}-{what}.log", step.number(), step.name()))
    }

    fn engine_remote(&self, repo: &Path) -> Result<crate::github_remote::Remote, String> {
        let expected = self.engine_repository.as_ref().map_err(|why| {
            format!(
                "the publication engine had no admitted repository when this release started: {why}"
            )
        })?;
        if &crate::github_remote::origin_slug(&self.engine_source)? != expected {
            return Err(
                "the publication engine source differs from this release's captured authority"
                    .into(),
            );
        }
        let native = (self.engine_origin)(repo, &self.engine_source)?;
        if native.repository() != expected
            || &crate::github_remote::origin_slug(&self.engine_source)? != expected
        {
            return Err(
                "the native engine repository differs from this release's captured authority"
                    .into(),
            );
        }
        Ok(native)
    }

    /// Bring this release's engine to its origin's main: made first where it has none, or pointed
    /// at the origin `engine_source` names ([`Real::engine_copy`]), then fast-forwarded with the
    /// audit rows its stages appended committed and pushed ([`Real::engine_fast_forward`]).
    fn engine_sync(&self) -> Result<(), Refusal> {
        self.engine_copy()?;
        self.engine_fast_forward()?;
        self.engine_synced
            .store(true, std::sync::atomic::Ordering::Release);
        Ok(())
    }

    /// The origin `engine_source` names (`git remote get-url origin`), or why it names none.
    fn source_origin(&self) -> Result<String, Refusal> {
        let s = self.engine_source.display().to_string();
        if !self.engine_source.join(".git").exists() {
            return Err(Refusal::new(
                format!("no publication engine at {s}"),
                "clone it there, or name one: --engine <path> (or PUBLICATION_ENGINE)",
            ));
        }
        (self.engine_url)(&self.engine_source).map_err(|_| {
            Refusal::new(
                format!("the publication engine {s} names no origin"),
                format!("git -C {s} remote add origin <the engine's repository>"),
            )
        })
    }

    /// The main of the engine's `origin` ([`aterm_verify::engine::OriginMain`]), or why it cannot be
    /// read.
    fn read_origin_main(&self, origin: &str) -> Result<aterm_verify::engine::OriginMain, Refusal> {
        aterm_verify::engine::OriginMain::fetch(
            origin,
            &self.engine_source,
            &|cmd| self.output_forwarded(cmd),
            &|at, _| self.engine_remote(at)?.refresh_main(),
        )
        .map_err(|why| {
            Refusal::new(
                format!("cannot read the publication engine's origin ({origin}): {why}"),
                format!("check the network: git ls-remote {origin}, then {RELEASE_COMMAND} again"),
            )
            .could_not_run()
        })
    }

    /// Step 1's read of the engine's origin main ([`Real::origin_main`]), or why it could not
    /// be read. An engine that names no origin is step 1's own refusal
    /// ([`Real::engine_unready`]), so it reads nothing here.
    fn fetch_engine_origin(&mut self) -> Result<(), Refusal> {
        if let Ok(engine_origin) = self.source_origin() {
            self.origin_main = Some(self.read_origin_main(&engine_origin)?);
        }
        Ok(())
    }

    /// A real run's first engine: where this clone holds no copy of its own yet, make it
    /// ([`Real::engine_copy`]) as step 1 fetches the engine's origin, so nothing step 1 asks
    /// after that fetch meets a checkout that is not there: its input judgment
    /// ([`Backend::input_checks`]) seals the HEAD of the engine it hands its verifier, a step
    /// before the sync. A copy that is there is the sync's to point and fast-forward
    /// ([`Real::engine_sync`]); a dry run makes none and keeps the engine it was named; an
    /// engine that names no origin is step 1's own refusal ([`Real::engine_unready`]).
    ///
    /// # Errors
    /// The copy could not be made: [`Real::engine_copy`]'s refusal, as COULD NOT RUN.
    fn engine_first_copy(&self) -> Result<(), Refusal> {
        if self.scratch_logs || self.engine.join(".git").exists() || self.source_origin().is_err() {
            return Ok(());
        }
        self.engine_copy().map_err(Refusal::could_not_run)
    }

    /// Refresh source refs before admitting `--fix`, without reading the engine or roster.
    /// Fleet discovery and step 1 consume this same snapshot rather than fetching it again.
    fn prefetch_source(&mut self) -> Result<(), Refusal> {
        if self.prefetched_origin.is_none() {
            self.prefetched_origin = Some(
                (self.source_origin)(&self.repo)
                    .and_then(|remote| remote.fetch_origin())
                    .map_err(origin_refresh_refusal)?,
            );
        }
        Ok(())
    }

    /// Step 1's reads of both origins: aterm's main and tags through `aterm` (agh's fetch, in
    /// `Backend::fetch`), then the engine's main ([`Real::fetch_engine_origin`]), which step 1
    /// judges the ledger and `bin/pub` on until step 2 syncs the release's engine, and a real
    /// run's first copy of that engine ([`Real::engine_first_copy`]); aterm's main.
    fn fetch_origins(
        &mut self,
        aterm: impl FnOnce(&Path) -> Result<crate::github_remote::FetchedOrigin, String>,
    ) -> Result<String, Refusal> {
        let origin = aterm(&self.repo).map_err(origin_refresh_refusal)?;
        self.origin_tags = origin.tags;
        if !self.scratch_logs && self.branch_is_main() {
            crate::bump_claim::recover(
                &self.git(),
                &self.release.join("bump-claim.json"),
                &origin.main,
                false,
            )
            .map_err(|why| cut_refusal("owned bump recovery", &why))?;
        }
        self.fetch_engine_origin()?;
        self.engine_first_copy()?;
        Ok(origin.main)
    }

    /// `path` of the engine as this run judges it: its own copy's once it has synced it, else
    /// the origin's main fetched at step 1; `None` where neither holds it.
    fn engine_text(&self, path: &str) -> Option<String> {
        if self
            .engine_synced
            .load(std::sync::atomic::Ordering::Acquire)
        {
            return crate::durable::read_text(&self.engine.join(path)).ok();
        }
        self.origin_main
            .as_ref()?
            .text(path, &|cmd| self.output_forwarded(cmd))
            .ok()
    }

    /// The release's own engine ([`aterm_verify::engine::engine_copy`]): cloned from the origin
    /// `engine_source` names where there is none yet (into a name of its own, then renamed into
    /// place, so an interrupted clone is never taken for one), or where the one there was made
    /// from another origin and holds nothing that origin lacks (one that does is refused, never
    /// delivered to this origin), and given `engine_source`'s committer, which its audit-row and
    /// ledger commits are made as.
    /// Nothing is read from `engine_source` but its origin and committer. Never in a dry run, and
    /// nothing to do where the release was named its own copy (by hand, or as the frozen cutter of
    /// a release older than the engine handover, which names it only the copy, as
    /// `PUBLICATION_ENGINE`); R's own cutter is handed the release's source
    /// ([`ENGINE_SOURCE_ENV`]) and makes the copy itself.
    fn engine_copy(&self) -> Result<(), Refusal> {
        let e = self.engine.display().to_string();
        if self.scratch_logs {
            return Err(Refusal::new(
                format!("a dry run makes and moves no publication engine, so {e} was not synced"),
                format!("{RELEASE_COMMAND} without --dry-run"),
            ));
        }
        if self.engine_is_its_source() {
            return Ok(());
        }
        let origin = self.source_origin()?;
        let source_native = self.engine_remote(&self.engine_source).map_err(|why| {
            Refusal::new(
                "cannot admit the publication engine's captured repository",
                "inspect the native routing or source-state refusal, then re-run",
            )
            .detail([why])
        })?;
        if self.engine.join(".git").exists()
            && crate::github_remote::origin_slug(&self.engine)
                .is_ok_and(|slug| &slug != source_native.repository())
        {
            // A copy of another origin: what it holds is that origin's, never this one's to
            // deliver, so it is made again from this origin, or kept where it holds anything
            // its own origin lacks.
            let was = self
                .engine_git(&["remote", "get-url", "origin"])
                .unwrap_or_default();
            let holds = self
                .engine_git(&["status", "--porcelain"])
                .is_none_or(|out| !out.trim().is_empty())
                || self
                    .engine_git(&["rev-list", "refs/remotes/origin/main..HEAD"])
                    .is_none_or(|out| !out.trim().is_empty());
            if holds {
                return Err(Refusal::new(
                    format!(
                        "the release's engine {e} was made from {}, and holds changes or commits \
                         that origin lacks; the engine it is named now names {origin}",
                        was.trim()
                    ),
                    copy_remedy(
                        &e,
                        &format!("status, and git -C {e} log --stat origin/main..HEAD"),
                    ),
                ));
            }
            if std::fs::remove_dir_all(&self.engine).is_err() {
                return Err(Refusal::new(
                    format!(
                        "could not remove the release's engine {e}, made from {}",
                        was.trim()
                    ),
                    format!(
                        "remove {e} (it holds nothing its origin lacks), then {RELEASE_COMMAND} again"
                    ),
                ));
            }
        }
        if !self.engine.join(".git").exists() {
            let made = self
                .engine
                .with_file_name(format!("engine.new-{}", std::process::id()));
            // A clone a killed run left: the run claim makes this run the clone's only one.
            if let Some(parent) = made.parent()
                && let Ok(entries) = std::fs::read_dir(parent)
            {
                for entry in entries.flatten() {
                    if entry
                        .file_name()
                        .to_string_lossy()
                        .starts_with("engine.new-")
                    {
                        let _ = std::fs::remove_dir_all(entry.path());
                    }
                }
            }
            let Some(parent) = made.parent() else {
                return Err(Refusal::new(
                    format!("{e} names no directory to clone the publication engine into"),
                    "release from a clone with a git directory",
                ));
            };
            std::fs::create_dir_all(parent).map_err(|error| {
                Refusal::new(
                    format!("cannot prepare the owned engine directory: {error}"),
                    "check the release directory's ownership and writable space",
                )
            })?;
            let cloned = source_native.copy_main(&made);
            if let Err(why) = cloned {
                let _ = std::fs::remove_dir_all(&made);
                return Err(Refusal::new(
                    "cannot create the release-owned engine through native agh",
                    "inspect the native routing or source-state refusal, then re-run",
                )
                .detail([why]));
            }
            if std::fs::rename(&made, &self.engine).is_err() {
                let _ = std::fs::remove_dir_all(&made);
                return Err(Refusal::new(
                    format!("could not place the native publication engine copy into {e}"),
                    format!(
                        "check that {e} is absent and its parent directory is writable, then \
                         {RELEASE_COMMAND} again"
                    ),
                ));
            }
        }
        for key in ["user.name", "user.email"] {
            let mut cmd = local_git()?;
            cmd.arg("-C")
                .arg(&self.engine_source)
                .args(["config", "--get", key]);
            let value = self
                .output_forwarded(&mut cmd)
                .ok()
                .filter(|out| out.status.success())
                .map(|out| String::from_utf8_lossy(&out.stdout).trim().to_string());
            if let Some(value) = value.filter(|v| !v.is_empty())
                && self.engine_git(&["config", key, &value]).is_none()
            {
                return Err(Refusal::new(
                    format!("could not give the publication engine {e} its committer ({key})"),
                    format!("git -C {e} config {key} '{value}'"),
                ));
            }
        }
        Ok(())
    }

    /// The release was named its own copy as its engine.
    fn engine_is_its_source(&self) -> bool {
        match (
            self.engine.canonicalize(),
            self.engine_source.canonicalize(),
        ) {
            (Ok(engine), Ok(source)) => engine == source,
            _ => false,
        }
    }

    fn refreshed_engine(&self) -> Result<(crate::github_remote::Remote, String), Refusal> {
        let e = self.engine.display().to_string();
        let native = self.engine_remote(&self.engine).map_err(|why| {
            Refusal::new(
                format!("cannot fetch the engine's origin ({e})"),
                "check the native routing or source-state refusal, then re-run",
            )
            .detail([why])
        })?;
        let tip = native.refresh_main().map_err(|why| {
            Refusal::new(
                format!("cannot fetch the engine's origin ({e})"),
                "check the native routing or source-state refusal, then re-run",
            )
            .detail([why])
        })?;
        Ok((native, tip))
    }

    /// Stage admits only a fast-forward of the prepared engine. Recovery of unpushed records
    /// belongs to sync; merging a rewritten origin here would conceal a lost admission.
    fn engine_admit_stage(&self) -> Result<(), Refusal> {
        let (_, tip) = self.refreshed_engine()?;
        if self
            .engine_git(&["merge-base", "--is-ancestor", "HEAD", &tip])
            .is_none()
        {
            return Err(Refusal::new(
                "the publication engine's origin/main is not a fast-forward of the prepared engine; stage was not started",
                format!(
                    "inspect the engine's rewritten history, then {RELEASE_COMMAND} again with the same candidate"
                ),
            ));
        }
        if self.engine_dirty()
            || self
                .engine_git(&["merge", "--ff-only", "-q", &tip])
                .is_none()
        {
            return Err(Refusal::new(
                "the publication engine could not fast-forward to the admitted origin/main; stage was not started",
                format!(
                    "inspect {} and re-run {RELEASE_COMMAND} with the same candidate",
                    self.engine.display()
                ),
            ));
        }
        Ok(())
    }

    fn engine_fast_forward(&self) -> Result<(), Refusal> {
        let e = self.engine.display().to_string();
        let (native, tip) = self.refreshed_engine()?;
        if self.engine_dirty() {
            return Err(Refusal::new(
                format!("the release's engine {e} has local changes besides its audit log"),
                copy_remedy(&e, "status --short"),
            ));
        }
        let rows: Vec<String> = self
            .engine_git(&["diff", "verification-log.jsonl"])
            .unwrap_or_default()
            .lines()
            .filter_map(|l| l.strip_prefix('+'))
            .filter(|l| l.starts_with('{'))
            .map(str::to_string)
            .collect();
        let kept = self.logs.join("audit-rows.jsonl");
        if !rows.is_empty()
            && let Ok(mut f) = crate::durable::open_append(&kept, true)
        {
            let _ = writeln!(f, "{}", rows.join("\n"));
        }
        let _ = self.engine_git(&["checkout", "-q", "--", "verification-log.jsonl"]);
        // A refusal below puts the rows back in the audit log, where the next sync finds them.
        let put_back = |refusal: Refusal| {
            if !rows.is_empty() {
                let path = self.engine.join("verification-log.jsonl");
                let text = crate::durable::read_text(&path).unwrap_or_default();
                let _ = crate::durable::write_fresh(&path, audit_rows_merged(&text, &rows));
            }
            Err(refusal)
        };
        // Commits its origin lacks are records whose push failed (an audit row's, a ledger row
        // `pub publish` made): nothing else writes this engine, so nothing else delivers them.
        let ahead = self
            .engine_git(&["rev-list", "origin/main..HEAD"])
            .is_none_or(|out| !out.trim().is_empty());
        if ahead {
            let code: Vec<String> = self
                .engine_git(&["diff", "--no-renames", "--name-only", "origin/main...HEAD"])
                .map_or_else(
                    || vec!["(unread)".to_string()],
                    |out| {
                        out.lines()
                            .filter(|p| !engine_data_plane(p))
                            .map(str::to_string)
                            .collect()
                    },
                );
            if !code.is_empty() {
                return put_back(Refusal::new(
                    format!(
                        "the release's engine {e} holds commits its origin's main lacks that \
                         change more than its records ({}); its audit rows are kept in {}",
                        code.join(", "),
                        kept.display()
                    ),
                    copy_remedy(&e, "log --stat origin/main..HEAD"),
                ));
            }
            let behind = self
                .engine_git(&["merge-base", "--is-ancestor", "origin/main", "HEAD"])
                .is_none();
            if behind
                && self
                    .engine_git(&["merge", "--no-edit", "-q", "origin/main"])
                    .is_none()
            {
                let _ = self.engine_git(&["merge", "--abort"]);
                return put_back(Refusal::new(
                    format!(
                        "the records the release's engine {e} holds unpushed do not merge with \
                         its origin's main (its audit rows are kept in {})",
                        kept.display()
                    ),
                    copy_remedy(&e, "log -p origin/main..HEAD"),
                ));
            }
        } else if self
            .engine_git(&["merge", "--ff-only", "-q", "origin/main"])
            .is_none()
        {
            return put_back(Refusal::new(
                format!(
                    "the engine {e} could not fast-forward to its origin/main (its audit rows are \
                     kept in {})",
                    kept.display()
                ),
                format!("read git -C {e} status, then {RELEASE_COMMAND} again"),
            ));
        }
        if rows.is_empty() {
            if !ahead {
                return Ok(());
            }
            let written = self.engine_commit("HEAD").and_then(|head| {
                let head = head.ok_or("the engine has no records commit")?;
                match native.write_ref("refs/heads/main", Some(&tip), &head)? {
                    crate::ledger::RefWrite::Written => Ok(()),
                    crate::ledger::RefWrite::Transient(why) => Err(why),
                    crate::ledger::RefWrite::Conflict => {
                        Err("the engine main moved before its records could land".into())
                    }
                }
            });
            return written.map_err(|why| {
                Refusal::new(
                    format!("could not push the records the engine {e} holds to its origin's main"),
                    format!("{RELEASE_COMMAND} again (they stay in {e} until they land)"),
                )
                .detail([why])
            });
        }
        let path = self.engine.join("verification-log.jsonl");
        let text = crate::durable::read_text(&path).unwrap_or_default();
        let merged = audit_rows_merged(&text, &rows);
        let recorded = crate::durable::write_fresh(&path, merged).is_ok()
            && self
                .engine_git(&[
                    "commit",
                    "-q",
                    "-m",
                    "chore(audit): record stage verification rows",
                    "--",
                    "verification-log.jsonl",
                ])
                .is_some();
        let published = if recorded {
            self.engine_commit("HEAD").and_then(|head| {
                let head = head.ok_or("the engine has no audit commit")?;
                match native.write_ref("refs/heads/main", Some(&tip), &head)? {
                    crate::ledger::RefWrite::Written => Ok(()),
                    crate::ledger::RefWrite::Transient(why) => Err(why),
                    crate::ledger::RefWrite::Conflict => {
                        Err("the engine's main changed before the audit commit could land".into())
                    }
                }
            })
        } else {
            Err("the local audit commit could not be recorded".into())
        };
        published.map_err(|why| {
            Refusal::new(
                "could not record the engine's audit rows (commit and push of \
                 verification-log.jsonl)",
                format!(
                    "{RELEASE_COMMAND} again (the rows are kept in {})",
                    kept.display()
                ),
            )
            .detail([why])
        })
    }

    fn engine_dirty(&self) -> bool {
        self.engine_git(&["status", "--porcelain"])
            .unwrap_or_default()
            .lines()
            .any(|l| !l.ends_with(" verification-log.jsonl"))
    }

    fn submodule_sync(&self, step: Step) -> Result<(), Refusal> {
        let mut cmd = local_git()?;
        cmd.args(["submodule", "update", "--init", "vendor/astream"])
            .current_dir(&self.repo);
        let ran = self.logged(step, &mut cmd);
        if ran.code != 0 {
            return Err(Refusal::new(
                "git submodule update --init vendor/astream failed — the workspace does not \
                 resolve without it",
                "check access to the astream repository, then re-run",
            )
            .detail(ran.detail));
        }
        let origin = crate::publish::native_origin(&self.repo)
            .map_err(|why| cut_refusal("submodule origin", &why.to_string()))?;
        crate::gates::submodules_on_declared_branch(&self.git(), &origin).map_err(|e| match e {
            crate::gates::PinRefusal::Unread(why) => Refusal::new(
                why,
                "the fetch was retried and the pin is not in question: re-run once the network \
                 and the forge answer",
            ),
            crate::gates::PinRefusal::Off(why) => Refusal::new(
                why,
                "land the astream change on astream's main and bump vendor/astream to that \
                 commit (vendor/README-astream.md), then re-run",
            ),
        })
    }

    fn dirty(&self) -> Result<Vec<String>, String> {
        crate::gates::dirty_paths(&self.repo).map_err(|e| e.to_string())
    }

    fn off_tree(&self, commit: &str) -> Option<Vec<String>> {
        let want = self.git_out(&["rev-parse", &format!("{commit}^{{tree}}")]);
        let head = self.git_out(&["rev-parse", "HEAD^{tree}"]);
        if want.is_none() || want != head {
            return Some(vec![format!(
                "HEAD's tree is {}, {}'s is {}",
                head.as_deref().map_or("unresolved", short),
                short(commit),
                want.as_deref().map_or("unresolved", short)
            )]);
        }
        match self.dirty() {
            Ok(dirty) if dirty.is_empty() => None,
            Ok(dirty) => Some(dirty.into_iter().take(5).collect()),
            Err(why) => Some(vec![format!(
                "the clean-tree judge could not answer: {why}"
            )]),
        }
    }

    fn main_to(&mut self, rev: &str) -> Result<(), Refusal> {
        let _timing = crate::cut_timing::Timer::start(&self.step_log(self.step), "source-checkout");
        let head = self.head();
        let ancestor = |s: &mut Self, a: &str, b: &str| s.is_ancestor(a, b);
        let mut cmd = local_git()?;
        cmd.current_dir(&self.repo);
        if ancestor(self, &head, rev) {
            cmd.args(["merge", "-q", "--ff-only", rev]);
        } else if ancestor(self, &head, "origin/main") {
            cmd.args(["reset", "-q", "--keep", rev]);
        } else {
            return Err(Refusal::new(
                "this clone's main has commits origin/main does not",
                "push them, or release from a fresh clone",
            ));
        }
        let ran = self.logged(Step::Commit, &mut cmd);
        if ran.code == 0 {
            Ok(())
        } else {
            Err(Refusal::new(
                format!("cannot move this clone's main to {}", short(rev)),
                format!("git -C {} status; then re-run", self.repo.display()),
            )
            .detail(ran.detail))
        }
    }

    fn no_engine(&self) -> Option<Refusal> {
        let e = self.engine.display().to_string();
        (!self.engine.join(".git").exists() || !self.engine.join("bin/pub").is_file()).then(|| {
            Refusal::new(
                format!("no publication engine at {e} (bin/pub)"),
                "clone it there, or name one: --engine <path> (or PUBLICATION_ENGINE)",
            )
        })
    }

    /// Step 1's ask of the engines: `engine_source` is an engine checkout that names an origin,
    /// and the release's own copy, where it has one, is on main with no change but its audit log.
    /// `engine_source`'s branch and changes are its users', not the release's.
    fn engine_unready(&self) -> Option<Refusal> {
        if let Err(why) = self.source_origin() {
            return Some(why);
        }
        if self.engine != aterm_verify::engine::engine_copy(&self.release)
            || !self.engine.join(".git").exists()
        {
            return None;
        }
        let e = self.engine.display().to_string();
        if self
            .engine_git(&["rev-parse", "--abbrev-ref", "HEAD"])
            .is_none_or(|b| b.trim() != "main")
        {
            return Some(Refusal::new(
                format!(
                    "the release's engine {e} is not on main — stage and publish run from its main"
                ),
                copy_remedy(&e, "status"),
            ));
        }
        self.engine_dirty().then(|| {
            Refusal::new(
                format!("the release's engine {e} has local changes besides its audit log"),
                copy_remedy(&e, "status --short"),
            )
        })
    }

    fn scratch_engine(&self, scratch: &Scratch) -> Result<PathBuf, Refusal> {
        if let Some(why) = self.no_engine() {
            return Err(why);
        }
        let e = self.engine.display().to_string();
        // The origin a real run would sync its engine to: the one the named engine names.
        let origin = self.source_origin()?;
        let copy = scratch.dir.join("engine");
        let _ = std::fs::remove_dir_all(&copy);
        let copy_s = copy.display().to_string();
        let ran = |cmd: &mut Command| self.output_forwarded(cmd).is_ok_and(|o| o.status.success());
        let in_copy = |args: &[&str]| {
            let mut cmd = local_git()?;
            cmd.args(["-c", "protocol.allow=never"])
                .arg("-C")
                .arg(&copy)
                .args(args);
            Ok::<_, Refusal>(cmd)
        };
        let mut clone = local_git()?;
        clone
            .args([
                "-c",
                "protocol.allow=never",
                "-c",
                "protocol.file.allow=always",
                "clone",
            ])
            .args(BYTE_EXACT)
            .args(["--quiet", "--shared", "--no-checkout", &e, &copy_s]);
        if !ran(&mut clone) {
            return Err(Refusal::new(
                format!("git could not copy the engine {e} into {copy_s}"),
                "fix what keeps it from being written, then run it again",
            ));
        }
        if !ran(&mut in_copy(&["remote", "set-url", "origin", &origin])?) {
            return Err(Refusal::new(
                format!("cannot bind the scratch engine to its origin ({origin})"),
                "read the local clone refusal, then re-run",
            ));
        }
        let tip = self
            .engine_remote(&copy)
            .and_then(|native| native.refresh_main())
            .map_err(|why| {
                Refusal::new(
                    format!("cannot fetch the main of the engine's origin ({origin})"),
                    "check the native routing or source-state refusal, then re-run",
                )
                .detail([why])
            })?;
        if !ran(&mut in_copy(&["checkout", "--quiet", "-B", "main", &tip])?) {
            return Err(Refusal::new(
                format!("git could not check the engine's origin main out in {copy_s}"),
                "fix what keeps it from being written, then run it again",
            ));
        }
        Ok(copy)
    }

    /// `targo` with `args` in this checkout, as a step builds a dev binary of it (`what`) to hand a
    /// child: [`Real::child`], its output kept in the step's log, its stdout (cargo's JSON
    /// messages) the answer.
    fn build_checkout_bin(
        &self,
        what: &str,
        args: &[std::ffi::OsString],
        plan: &aterm_verify::NativeBuildPlan,
    ) -> Result<Vec<u8>, String> {
        use aterm_verify::provision::Host as _;
        let targo = aterm_verify::provision::System::new(&self.repo).targo()?;
        let mut cmd = self.child(targo, None);
        cmd.args(args);
        plan.apply_to_command(&mut cmd).map_err(|e| e.to_string())?;
        let out = self.output_forwarded(&mut cmd).map_err(|e| e.to_string())?;
        if let Ok(mut log) = crate::durable::open_append(&self.step_log(self.step), true) {
            let _ = log.write_all(&out.stdout);
            let _ = log.write_all(&out.stderr);
        }
        if !out.status.success() {
            return Err(format!(
                "{what} build exited {}: {}",
                out.status,
                String::from_utf8_lossy(&out.stderr)
            ));
        }
        Ok(out.stdout)
    }

    /// Build one of R's frozen tools with the already leased prover into verify's shared main
    /// build directory, and return the exact executable cargo reported.
    fn build_frozen_tool(
        &self,
        prover: &Prover,
        bin: &crate::checkout_bin::Bin,
        profile: &str,
    ) -> Result<(PathBuf, Vec<u8>), String> {
        let _phase =
            crate::cut_timing::Timer::start(&self.step_log(self.step), "frozen-tool-build");
        let plan = verify_plan(&self.repo)
            .map_err(|e| format!("select {} build directory: {e}", bin.what))?;
        let layout = plan.build_dir();
        let args = Self::frozen_tool_args(&self.repo, bin, profile, layout);
        let mut command = self.child(prover.bin.join("targo"), Some(prover));
        command
            .args(&args)
            .arg("--target-dir")
            .arg(plan.target())
            .current_dir(&self.repo);
        // Keep Cargo's actual dirty-unit reasons beside the artifact counts. A
        // later cut must distinguish a source delta from cache invalidation.
        command.env("CARGO_LOG", "targo::compiler::fingerprint=info");
        plan.apply_to_command(&mut command)
            .map_err(|e| format!("configure {} build environment: {e}", bin.what))?;
        plan.prepare()
            .map_err(|e| format!("prepare {} build directory: {e}", bin.what))?;
        let (output, accounting) = self
            .output_accounted(self.step, &mut command)
            .map_err(|e| format!("{} build: {e}", bin.what))?;
        if let Ok(mut log) = crate::durable::open_append(&self.step_log(self.step), true) {
            let _ = log.write_all(&output.stdout);
            let _ = log.write_all(&output.stderr);
            let _ = writeln!(log, "{accounting}");
            let _ = writeln!(
                log,
                "{}",
                frozen_build_event(bin.package, profile, layout, &output.stdout)
            );
        }
        if !output.status.success() {
            return Err(format!(
                "{} build exited {}: {}",
                bin.what,
                output.status,
                String::from_utf8_lossy(&output.stderr)
            ));
        }
        let path = crate::checkout_bin::built(&self.repo, bin, &output.stdout)?;
        Ok((path, output.stdout))
    }

    /// The exact locked-targo line for one frozen tool. The verifier uses the main test graph;
    /// the gate-profile cutter does not widen its features.
    fn frozen_tool_args(
        repo: &Path,
        bin: &crate::checkout_bin::Bin,
        profile: &str,
        layout: Option<&str>,
    ) -> Vec<std::ffi::OsString> {
        let mut args: Vec<String> = [
            "--unverified",
            "build",
            "--locked",
            "--profile",
            profile,
            "-p",
            bin.package,
            "--bin",
            bin.package,
            "--message-format=json",
        ]
        .into_iter()
        .map(str::to_owned)
        .collect();
        if bin.package == FROZEN_VERIFY.package {
            args = aterm_toolchain::build_cache::with_test_graph(&args);
        }
        if let Some(layout) = layout {
            args.splice(0..0, ["--config".to_owned(), layout.to_owned()]);
        }
        let mut args: Vec<std::ffi::OsString> = args.into_iter().map(Into::into).collect();
        args.push("--manifest-path".into());
        args.push(repo.join("Cargo.toml").into_os_string());
        args
    }

    /// R's two immutable executable copies, with the record published only after the second
    /// exact clean-R check.
    fn frozen_tools(
        &self,
        commit: &str,
        prover: &Prover,
    ) -> Result<crate::release_tools::ToolSet, String> {
        if let Ok(recorded) = crate::release_tools::recorded(&self.release, commit) {
            crate::release_tools::clean_at(&self.repo, commit)?;
            return Ok(recorded);
        }
        crate::release_tools::clean_at(&self.repo, commit)?;
        let cutter = self.frozen_cutter(commit, prover)?.tool.path;
        let verify = match &self.warm_verifier {
            Some(path) => path.clone(),
            None => self.build_frozen_tool(prover, &FROZEN_VERIFY, "test")?.0,
        };
        let tools = crate::release_tools::stage(&self.release, commit, &cutter, &verify)?;
        crate::release_tools::clean_at(&self.repo, commit)?;
        crate::release_tools::publish(&self.release, &tools)?;
        Ok(tools)
    }

    fn cutter_retry(&self) -> String {
        let args = self
            .argv
            .iter()
            .map(|arg| crate::release::shell_word(&arg.to_string_lossy()))
            .collect::<Vec<_>>()
            .join(" ");
        format!(
            "from {} run `targo --unverified ship {args}`",
            crate::release::shell_word(&self.repo.to_string_lossy())
        )
    }

    fn frozen_cutter(
        &self,
        commit: &str,
        prover: &Prover,
    ) -> Result<crate::release_tools::Cutter, String> {
        self.frozen_cutter_with_inputs(commit, prover)
            .map(|(cutter, _)| cutter)
    }

    /// Select the frozen cutter and, for the exact pre-freeze path, the kept build-input seal
    /// which the exec receiver must recheck and adopt at step 1.
    fn frozen_cutter_with_inputs(
        &self,
        commit: &str,
        prover: &Prover,
    ) -> Result<(crate::release_tools::Cutter, Option<PathBuf>), String> {
        // A dry run leaves the caller's HEAD alone. The compiler checkout below is still
        // checked clean at the exact selected commit before and after the frozen build.
        if !self.scratch_logs {
            crate::release_tools::clean_at(&self.repo, commit)?;
        }
        if crate::release_tools::Recovery::read(commit) != crate::release_tools::Recovery::Refreeze
            && let Ok(cutter) = crate::release_tools::recorded_cutter(&self.release, commit)
        {
            let inputs = self.prepared_step1_handoff(commit, &cutter).unwrap_or(None);
            return Ok((cutter, inputs));
        }
        let cache = cutter_cache_dir(&self.release);
        std::fs::create_dir_all(&cache).map_err(|e| e.to_string())?;
        #[cfg(unix)]
        {
            // The new line makes the clone's old supervisor settle its compiler.
            // Join that handoff before competing for its workspace; a live
            // background owner is cancellation in progress, not a cut refusal.
            let mut probe = aterm_uds::deadline::ProcessProgressProbe::default();
            wait_prefreeze(
                || {
                    crate::release::live_holder(&cache.join("prefreeze-running"), &mut started_of)
                        .map(|holder| {
                            let progress = probe.read_descendants(holder.pid);
                            (holder.pid, holder.started, progress)
                        })
                },
                std::time::Instant::now,
                || std::thread::sleep(std::time::Duration::from_millis(500)),
                || interrupt::caught().is_some(),
                |message| eprintln!("{message}"),
            )?;
        }
        let pid = std::process::id();
        let started = started_of(pid).ok_or("cannot identify cutter workspace holder")?;
        let _lease = crate::release::RunClaim::take(
            &cache.join("workspace-running"),
            (pid, &started),
            &self.logs,
            &mut started_of,
        )
        .map_err(|why| format!("cutter workspace unavailable: {why:?}"))?;
        if crate::release_tools::Recovery::read(commit) != crate::release_tools::Recovery::Refreeze
            && let Ok(prepared) = crate::release_tools::recorded_cutter(&cache, commit)
            && self.cached_input_record(&cache.join("state"), &prepared)
            && let Some(prefrozen_inputs) = self.prepared_step1_handoff(commit, &prepared)
        {
            // Preparation's proof is rooted in cutter-cache, so it cannot simply become the
            // release root's proof. Copy and prove its immutable executable under this root,
            // retain the prepared input/built-from bindings, and hand its kept input seal to
            // the cutter for step 1's mandatory status recheck. Publish only after the second
            // clean-R check. This is the exact-record fast path; no compiler checkout or Cargo
            // freshness pass belongs to the launcher.
            let log = self.step_log(self.step);
            let mut cutter = crate::release_tools::stage_cutter(
                &self.release,
                commit,
                &prepared.tool.path,
                &log,
                prepared.inputs.clone(),
            )?;
            cutter.built_from.clone_from(&prepared.built_from);
            if !self.scratch_logs {
                crate::release_tools::clean_at(&self.repo, commit)?;
            }
            crate::release_tools::publish_cutter(&self.release, &cutter)?;
            let event = aterm_json::json!({
                "schema": 1, "event": "cutter-prefreeze-adopted", "commit": commit,
                "built_from": &cutter.built_from, "inputs": &cutter.inputs,
                "step_1_inputs": if prefrozen_inputs.is_some() {
                    "adopted-and-handed"
                } else {
                    "not-applicable"
                },
            });
            if let Ok(mut log) = crate::durable::open_append(&log, true) {
                let _ = writeln!(log, "release-event: {event}");
            }
            println!("release-event: {event}");
            return Ok((cutter, prefrozen_inputs));
        }
        let compiler = self.cutter_workspace_at(&cache, commit)?;
        let built = compiler.frozen_cutter_here(commit, prover)?;
        let mut cutter = crate::release_tools::stage_cutter(
            &self.release,
            commit,
            &built.tool.path,
            &self.step_log(self.step),
            built.inputs,
        )?;
        cutter.built_from = built.built_from;
        if !self.scratch_logs {
            crate::release_tools::clean_at(&self.repo, commit)?;
        }
        crate::release_tools::publish_cutter(&self.release, &cutter)?;
        Ok((cutter, None))
    }

    fn cached_input_record(&self, state: &Path, cutter: &crate::release_tools::Cutter) -> bool {
        if cfg!(target_os = "macos") {
            cutter
                .inputs
                .as_ref()
                .is_some_and(|inputs| (self.prefrozen_inputs)(state, &inputs.seal).is_some())
        } else {
            cutter.inputs.is_none()
        }
    }

    /// An outer `None` means step 1 did not adopt the cache evidence for this exact cutter. The
    /// inner option is the real state's rechecked seal handed on macOS, or no seal where the
    /// platform has no universal build-input identity.
    fn prepared_step1_handoff(
        &self,
        commit: &str,
        cutter: &crate::release_tools::Cutter,
    ) -> Option<Option<PathBuf>> {
        if cfg!(target_os = "macos") {
            let inputs = cutter.inputs.as_ref()?;
            let adopted = self.prefrozen_step1.as_ref()?;
            if adopted.commit != commit || adopted.seal != inputs.seal {
                return None;
            }
            (self.handed_inputs)().map(Some)
        } else {
            cutter.inputs.is_none().then_some(None)
        }
    }

    /// One stable compiler checkout for this clone, shared by preparation and freeze 1.
    /// Its own proof never replaces the proof of the cutter running the current cut.
    fn cutter_workspace_at(&self, cache: &Path, commit: &str) -> Result<Real, String> {
        let key = aterm_codec::hex::encode_lower(&aterm_digest::Sha256::digest(
            cache.as_os_str().as_encoded_bytes(),
        ));
        let source = self
            .state
            .join(".ubd")
            .join(format!("cutter-{key}.noindex"));
        std::fs::create_dir_all(source.parent().ok_or("no cutter source parent")?)
            .map_err(|e| e.to_string())?;
        // Only the exclusive workspace holder changes this regenerable checkout. An
        // interrupted Git transaction may leave locks or a partially populated index;
        // reconstruct those sources, retaining the separately placed compiler cache.
        let synchronized = cache.join("source-ready");
        if source.exists() && !synchronized.exists() {
            std::fs::remove_dir_all(&source).map_err(|e| e.to_string())?;
        }
        match std::fs::remove_file(&synchronized) {
            Ok(()) => {}
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
            Err(error) => return Err(error.to_string()),
        }
        if !source.exists() {
            crate::ledger::git_ok(
                &self.git(),
                &[
                    "clone",
                    "--quiet",
                    "--shared",
                    "--no-checkout",
                    self.repo.to_str().ok_or("non-UTF8 repository")?,
                    source.to_str().ok_or("non-UTF8 compiler checkout")?,
                ],
            )
            .map_err(|e| e.to_string())?;
        }
        let git = GitCli::new(&source);
        let run = |args: &[&str]| {
            crate::ledger::git_ok(&git, args)
                .map(|_| ())
                .map_err(|e| e.to_string())
        };
        run(&[
            "fetch",
            "--quiet",
            self.repo.to_str().ok_or("non-UTF8 repository")?,
            commit,
        ])?;
        run(&["checkout", "--quiet", "--detach", commit])?;
        if source.join(".gitmodules").exists() {
            run(&[
                "config",
                "submodule.vendor/astream.url",
                self.repo
                    .join("vendor/astream")
                    .to_str()
                    .ok_or("non-UTF8 submodule")?,
            ])?;
            run(&[
                "-c",
                "protocol.file.allow=always",
                "submodule",
                "update",
                "--init",
                "vendor/astream",
            ])?;
        }
        crate::release_tools::clean_at(&source, commit)?;
        crate::durable::write_atomic_durable(&synchronized, commit.as_bytes(), 0o600)
            .map_err(|e| e.to_string())?;
        let mut compiler = Real::new(&source, Some(&self.engine_source), true, false)?;
        let _ = std::fs::remove_dir_all(&compiler.logs);
        compiler.release = cache.to_owned();
        compiler.state = cache.join("state");
        compiler.build_inputs = self.build_inputs;
        compiler.arm_inputs = self.arm_inputs;
        compiler.arm_prefrozen_inputs = self.arm_prefrozen_inputs;
        compiler.prefrozen_inputs = self.prefrozen_inputs;
        compiler.logs = self.logs.clone();
        compiler.scratch_logs = false;
        compiler.step = self.step;
        Ok(compiler)
    }

    fn frozen_cutter_here(
        &self,
        commit: &str,
        prover: &Prover,
    ) -> Result<crate::release_tools::Cutter, String> {
        let log = self.step_log(self.step);
        {
            let _phase = crate::cut_timing::Timer::start(&log, "cutter-source-check");
            crate::release_tools::clean_at(&self.repo, commit)?;
        }
        // The build supplies both the authoritative freshness decision and the selected
        // graph. Preparation and freeze 1 share these persistent gate units.
        let seal = if cfg!(target_os = "macos") {
            let _timing = crate::cut_timing::Timer::start(&log, "cutter-inputs-seal");
            (self.arm_inputs)(&self.state, Some(&self.transcript()));
            Some((self.build_inputs)()?)
        } else {
            None
        };
        let (source, output) = self.build_frozen_tool(prover, &FROZEN_CUTTER, "gate")?;
        let observation = seal.as_deref().and_then(|seal| {
            let _timing = crate::cut_timing::Timer::start(&log, "cutter-input-graph");
            match crate::cutter_inputs::capture(&self.repo, &output, seal) {
                Ok(observation) => Some(observation),
                Err(why) => {
                    if let Ok(mut log) = crate::durable::open_append(&log, true) {
                        let _ = writeln!(log, "cutter adoption unavailable: {why}");
                    }
                    None
                }
            }
        });
        let adopted = observation
            .as_ref()
            .map(|inputs| {
                crate::release_tools::adopt_cutter(&self.release, commit, &source, inputs)
            })
            .transpose()?
            .flatten();
        let cutter = if let Some(cutter) = adopted {
            let event = aterm_json::json!({
                "schema": 1, "event": "cutter-adopted", "commit": commit,
                "built_from": &cutter.built_from, "inputs": &cutter.inputs,
            });
            if let Ok(mut log) = crate::durable::open_append(&log, true) {
                let _ = writeln!(log, "release-event: {event}");
            }
            println!("release-event: {event}");
            cutter
        } else {
            crate::release_tools::stage_cutter(
                &self.release,
                commit,
                &source,
                &log,
                observation.map(|observation| observation.inputs),
            )?
        };
        {
            let _phase = crate::cut_timing::Timer::start(&log, "cutter-source-recheck");
            crate::release_tools::clean_at(&self.repo, commit)?;
        }
        {
            let _phase = crate::cut_timing::Timer::start(&log, "cutter-record-publish");
            crate::release_tools::publish_cutter(&self.release, &cutter)?;
        }
        Ok(cutter)
    }

    fn finish_tools(&mut self) -> Result<(), String> {
        let commit = self
            .warming
            .as_ref()
            .map(|warming| warming.commit.clone())
            .unwrap_or_else(|| self.head());
        if let Some(mut warming) = self.warming.take() {
            crate::release_tools::clean_at(&self.repo, &warming.commit)?;
            let status = warming.child.wait().map_err(|e| e.to_string())?;
            crate::release_tools::clean_at(&self.repo, &warming.commit)?;
            let stdout = std::fs::read(&warming.stdout).map_err(|e| e.to_string())?;
            let stderr = std::fs::read(&warming.stderr).map_err(|e| e.to_string())?;
            if !status.success() {
                return Err(format!(
                    "verifier build exited {status}: {}",
                    String::from_utf8_lossy(&stderr)
                ));
            }
            self.warm_verifier = Some(crate::checkout_bin::built(
                &self.repo,
                &FROZEN_VERIFY,
                &stdout,
            )?);
        }
        let prover = self.prover(None)?;
        self.frozen_tools(&commit, &prover)?;
        Ok(())
    }

    #[cfg(unix)]
    fn hand_exec_guards(&self, command: &mut Command, prover: &Prover) -> Result<(), String> {
        use std::os::fd::AsRawFd as _;

        let mut guards = Vec::new();
        if let Some(lease) = crate::gates::offer_toolchain_lease(&prover.bin)? {
            command
                .env(
                    TOOLCHAIN_LEASE_FD_ENV,
                    lease.descriptor.as_raw_fd().to_string(),
                )
                .env(TOOLCHAIN_LEASE_LEAF_ENV, &lease.leaf);
            guards.push(lease.descriptor);
        }
        let claim = self
            .line_priority
            .as_ref()
            .ok_or("the release-line claim is absent at the cutter handover")?;
        let descriptor = claim
            .descriptor()
            .map_err(|e| format!("duplicate release-line claim: {e}"))?;
        let leaf = claim
            .path()
            .file_name()
            .and_then(std::ffi::OsStr::to_str)
            .filter(|leaf| !leaf.is_empty() && !leaf.contains('/'))
            .ok_or("the release-line claim has no bounded UTF-8 leaf")?;
        command
            .env(LINE_FD_ENV, descriptor.as_raw_fd().to_string())
            .env(LINE_LEAF_ENV, leaf);
        guards.push(descriptor);
        let cohort = aterm_capacity::Ledger::user()
            .ok_or("no account ledger exists for the release-line claim")?
            .cohort_aware()
            .map_err(|e| format!("validate the release-line ledger: {e}"))?;
        if cohort {
            let writer = claim
                .cohort_writer_descriptor()
                .map_err(|e| format!("duplicate release-line writer: {e}"))?;
            command.env(LINE_WRITER_FD_ENV, writer.as_raw_fd().to_string());
            guards.push(writer);
        } else {
            command.env_remove(LINE_WRITER_FD_ENV);
        }
        command
            .inherit_exec_guards(guards)
            .map_err(|e| format!("arm release handover descriptors: {e}"))?;
        Ok(())
    }

    /// The agh the engine's dry run reads GitHub through ([`ENGINE_AGH_ENV`]): the caller's own,
    /// or this checkout's dev client, built in the gate profile ([`crate::checkout_bin`]). The
    /// engine sends every GitHub read through agh, which routes each owner to the token its vault
    /// holds for it and reads the public tier anonymously when the engine asks; with no
    /// `PUBLISH_AGH` it runs the `agh` on PATH, which is the installed app's link to `aterm`,
    /// resolved to `aterm` before it runs (`aterm help: unknown topic 'repo'`) and as old as that
    /// install.
    fn engine_agh(&self) -> Result<PathBuf, Refusal> {
        self.engine_agh_with_jobs(None)
    }

    fn engine_agh_with_jobs(&self, jobs: Option<u32>) -> Result<PathBuf, Refusal> {
        let explicit = self.engine_agh_explicit.as_deref();
        crate::checkout_bin::prepare_with_jobs(
            &self.repo,
            &ENGINE_AGH,
            explicit,
            &verify_environment(),
            jobs,
            |args, plan| {
                println!("export guards: building this checkout's agh, the engine's GitHub client");
                (self.checkout_build)(self, ENGINE_AGH.what, args, plan)
            },
        )
        .map_err(|why| {
            Refusal::new(
                format!(
                    "the engine reads GitHub only through agh, and no agh it can run is here: \
                     {why}"
                ),
                format!(
                    "fix this checkout's agh build (targo --unverified build -p agh --bin agh), \
                     or set {ENGINE_AGH_ENV} to a qualified agh's absolute path, then run it \
                     again"
                ),
            )
        })
    }

    fn metadata_toolchain(&self) -> Option<Refusal> {
        let dir = match rustup_dir() {
            Ok(dir) => dir,
            Err(absent) => {
                return Some(Refusal::new(
                    format!(
                        "{} — the engine's cargo-metadata guard runs `rustup run trust targo \
                         metadata`",
                        absent.why
                    ),
                    absent.fix,
                ));
            }
        };
        let found = self
            .output_forwarded(
                command(dir.join("rustup"))
                    .args(["which", "--toolchain", "trust", "targo"])
                    .env("PATH", path_with(&dir)),
            )
            .is_ok_and(|o| o.status.success());
        (!found).then(|| {
            Refusal::new(
                "rustup has no `trust` toolchain with a targo — the engine's cargo-metadata \
                 guard runs `rustup run trust targo metadata`",
                "aterm pkg doctor, then aterm pkg repair (atpkg keeps ~/.rustup/toolchains/trust)",
            )
        })
    }

    /// The export guards, `ship check --export`: the engine's own dry run, `pub stage aterm --dry-run --no-check --commit R --repo-path <scratch clone>`, every export guard `pub stage` runs (forbidden content, private references, gitleaks, the cargo metadata closure, README versions, dead links) in one pass, over a scratch clone of R (never this checkout's `publish/.out`, where a real stage's attestation waits for `pub publish`) and a [`Real::scratch_engine`] copy. The engine reads the public tier as the public does ([`public_reads`]), so those reads need no SSH key.
    pub(crate) fn export_gate(&mut self, scratch: &Scratch, r: &str) -> (Outcome, Option<PathBuf>) {
        self.export_gate_with_check(scratch, r, None)
    }

    fn export_gate_with_check(
        &mut self,
        scratch: &Scratch,
        r: &str,
        check: Option<u32>,
    ) -> (Outcome, Option<PathBuf>) {
        self.scanner_tools = None;
        let not_run = |why: Refusal| (Outcome::CouldNotRun(why), None);
        if let Err(why) = aterm_census::export::mainline::require(r, |args| {
            let mut git =
                crate::ledger::object_git(&self.repo).map_err(|error| error.to_string())?;
            git.args(args);
            aterm_uds::deadline::output_within(&mut git, aterm_census::export::mainline::READ_WAIT)
                .map(|out| {
                    (
                        out.status.code(),
                        String::from_utf8_lossy(&out.stderr).into_owned(),
                    )
                })
                .map_err(|error| error.to_string())
        }) {
            return not_run(Refusal::new(why.why(r), why.remedy()));
        }
        if let Err(why) = self.prepare_prerequisites() {
            return not_run(why);
        }
        let parent_count = std::env::var_os("GIT_CONFIG_COUNT");
        let parent_parameters = std::env::var_os("GIT_CONFIG_PARAMETERS");
        // Validate the count before asking git to interpret the inherited command config.
        if let Err(why) = public_reads(parent_count.as_deref(), &[], parent_parameters.as_deref()) {
            return not_run(why);
        }
        let keys = match public_header_keys(public_config_git(&self.repo)) {
            Ok(keys) => keys,
            Err(why) => return not_run(why),
        };
        let reads = match public_reads(parent_count.as_deref(), &keys, parent_parameters.as_deref())
        {
            Ok(reads) => reads,
            Err(why) => return not_run(why),
        };
        if let Some(why) = self.no_engine() {
            return not_run(why);
        }
        // Prepare the client before fetching the engine so compilation cannot age that snapshot.
        let agh = match self.engine_agh() {
            Ok(agh) => agh,
            Err(why) => return not_run(why),
        };
        let engine = match self.scratch_engine(scratch) {
            Ok(engine) => engine,
            Err(why) => return not_run(why),
        };
        let root = scratch.dir.join("dry-run").join("aterm");
        let log = scratch.dir.join("dry-run.log");
        if let Err(why) = self.dry_clone(&scratch.dir, &root, r) {
            return not_run(Refusal::new(
                format!(
                    "the engine's dry run could not lay {} out, so no export guard has run: \
                     {why}",
                    short(r)
                ),
                format!(
                    "fix what that names, then run it again (git's own words: {})",
                    scratch.dir.join("dry-run-clone.log").display()
                ),
            ));
        }
        let toolchain = self.metadata_toolchain();
        let pubbin = engine.join("bin/pub");
        let command = match check {
            None => self.dry_run_command(&engine, r, &root, &reads, &agh),
            Some(_) => self.local_stage_command(&engine, r, &root, &reads, &agh, check),
        };
        let (mut stage, handed) = match command {
            Ok(command) => command,
            Err(why) => return not_run(why),
        };
        let scanners = match scanner_tools::prepare(
            &stage,
            self.engine_prefix_len(),
            &scratch.dir.join("scanner-tools.json"),
            &[&self.repo, &root],
            |cmd| {
                self.apply_prerequisites(cmd)?;
                (self.scanner_probe)(cmd)
            },
        ) {
            Ok(report) => report,
            Err(why) => return not_run(scanner_refusal(why)),
        };
        scanners.apply(&mut stage);
        let (code, text) = match self.output_accounted(self.step, &mut stage) {
            Ok((o, record)) => {
                if let Ok(mut file) = crate::durable::open_append(&self.step_log(self.step), true) {
                    let _ = writeln!(file, "{record}");
                }
                (
                    o.status.code().unwrap_or(-1),
                    format!(
                        "{}{}",
                        String::from_utf8_lossy(&o.stdout),
                        String::from_utf8_lossy(&o.stderr)
                    ),
                )
            }
            Err(e) => (-1, format!("cannot run {}: {e}", pubbin.display())),
        };
        let _ = crate::durable::write_fresh(&log, format!("{handed}\n{text}"));
        let mut run = EngineRun::read(code, &text);
        let out = root.join("publish/.out");
        if check.is_some() && code == 0 {
            let attestation =
                crate::gates::read_bounded_string(&out.join("attestation.json"), 16 << 20);
            if !attestation
                .as_ref()
                .is_ok_and(|text| attestation_qualifies(text, r))
            {
                return not_run(Refusal::new(
                    "the full local export returned success without a complete passing attestation",
                    format!(
                        "read {} before measuring this candidate again",
                        log.display()
                    ),
                ));
            }
            if let Ok(text) = attestation {
                let _ = crate::durable::write_fresh(&scratch.dir.join("attestation.json"), text);
            }
        }
        let export = out.join("export");
        for (name, at) in &mut run.failed {
            if let Some(report) = gitleaks_report(name) {
                let json = crate::durable::read_text(Path::new(report)).unwrap_or_default();
                *at = gitleaks_locations(&json, &export);
            }
        }
        let reports = scratch.dir.join("reports");
        keep_reports(&out, &reports);
        let judged = !run.passed.is_empty() || !run.failed.is_empty();
        let left = (judged && export.is_dir()).then_some(export);
        (
            export_outcome(&run, r, toolchain.as_ref(), &log, &reports),
            left,
        )
    }

    /// The engine's dry run of `r` over the clone at `root`, `pub stage aterm --dry-run
    /// --no-check --commit R --repo-path <root>` from the scratch `engine`: [`Real::engine_child`],
    /// with this rustup's directory first on PATH, `reads` ([`public_reads`]) and `agh` as its
    /// GitHub client ([`ENGINE_AGH_ENV`], [`Real::engine_agh`]) in its environment, handed this
    /// cutter for its transforms only as `r`'s own build ([`hand_transforms_cutter`], whose line
    /// it returns beside it).
    fn dry_run_command(
        &self,
        engine: &Path,
        r: &str,
        root: &Path,
        reads: &[(String, std::ffi::OsString)],
        agh: &Path,
    ) -> Result<(Command, String), Refusal> {
        self.local_stage_command(engine, r, root, reads, agh, None)
    }

    fn local_stage_command(
        &self,
        engine: &Path,
        r: &str,
        root: &Path,
        reads: &[(String, std::ffi::OsString)],
        agh: &Path,
        check: Option<u32>,
    ) -> Result<(Command, String), Refusal> {
        let mut stage = self.engine_child(engine, None)?;
        stage
            .env("PUBLICATION_ENGINE", engine)
            .env(ENGINE_AGH_ENV, agh)
            .env(
                "PATH",
                rustup_dir().map_or_else(|_| publication_path(), |dir| path_with(&dir)),
            )
            .envs(reads.iter().map(|(key, value)| (key, value)))
            .args(["stage", "aterm", "--dry-run"]);
        if let Some(jobs) = check {
            stage.env("CARGO_BUILD_JOBS", jobs.max(1).to_string());
        } else {
            stage.arg("--no-check");
        }
        stage.args(["--commit", r, "--repo-path"]).arg(root);
        let handed = hand_transforms_cutter(&mut stage, r);
        Ok((stage, handed))
    }

    fn dry_clone(&self, base: &Path, root: &Path, r: &str) -> Result<(), String> {
        let log = base.join("dry-run-clone.log");
        let _ = crate::durable::write_fresh(&log, "");
        let dry_run = base.join("dry-run");
        let _ = std::fs::remove_dir_all(&dry_run);
        if std::fs::symlink_metadata(&dry_run).is_ok() {
            return Err(format!(
                "the last run's clone at {} cannot be removed",
                dry_run.display()
            ));
        }
        let origin = self
            .git_out(&["remote", "get-url", "origin"])
            .ok_or("this checkout names no origin")?;
        let git = |dir: &Path, args: &[&str]| -> Result<bool, String> {
            let out = self.output_forwarded(
                crate::ledger::git_command()
                    .map_err(|why| why.to_string())?
                    .arg("-C")
                    .arg(dir)
                    .args(args),
            );
            if let Ok(o) = &out
                && let Ok(mut f) = crate::durable::open_append(&log, false)
            {
                let _ = f.write_all(&o.stderr);
            }
            Ok(out.is_ok_and(|o| o.status.success()))
        };
        let clone = |from: &str, into: &str| {
            let args = [
                &["clone"][..],
                &BYTE_EXACT,
                &["--quiet", "--shared", "--no-checkout", from, into],
            ];
            git(base, &args.concat())
        };
        let (repo, root_s) = (self.repo.display().to_string(), root.display().to_string());
        if !clone(&repo, &root_s)?
            || !git(root, &["remote", "set-url", "origin", &origin])?
            || !git(
                root,
                &[
                    "-c",
                    "submodule.recurse=false",
                    "checkout",
                    "--quiet",
                    "--detach",
                    r,
                ],
            )?
        {
            return Err(format!(
                "git could not clone this checkout at {} into {root_s}",
                short(r)
            ));
        }
        for (rev, path) in
            crate::gates::gitlinks(&self.git_out(&["ls-tree", "-r", r]).unwrap_or_default())
        {
            let sub = self.repo.join(&path);
            let own = crate::ledger::git_command()
                .map_err(|why| why.to_string())?
                .arg("-C")
                .arg(&sub)
                .args(["rev-parse", "--show-toplevel"])
                .output()
                .ok()
                .map(|o| PathBuf::from(String::from_utf8_lossy(&o.stdout).trim()));
            let is_own = own.and_then(|p| p.canonicalize().ok()) == sub.canonicalize().ok();
            if !is_own || !git(&sub, &["cat-file", "-e", &format!("{rev}^{{commit}}")])? {
                return Err(format!(
                    "the submodule {path} is not initialized at its gitlink {} in this checkout, \
                     and the stage writes it from there (git submodule update --init {path})",
                    short(&rev)
                ));
            }
            let dest = root.join(&path);
            let _ = std::fs::remove_dir_all(&dest);
            let (sub_s, dest_s) = (sub.display().to_string(), dest.display().to_string());
            if !clone(&sub_s, &dest_s)? || !git(&dest, &["checkout", "--quiet", "--detach", &rev])?
            {
                return Err(format!(
                    "git could not clone the submodule {path} at {} into the scratch clone",
                    short(&rev)
                ));
            }
        }
        Ok(())
    }

    /// THE WASM32 ENGINE: the site's /terminal engine type-checked on the public toolchain pin, with R's own `publish/config.sh` wasm32 clause (read out of its `CHECK_CMD_DEFAULT`, never restated), over the export the export guards judged — else a `git archive` of R without `.cargo/` — copied outside every checkout, in a scrubbed environment. v0.91.0 was refused there by the site build AFTER its promote; `pub stage`'s public-clone gate runs the same clause.
    pub(crate) fn wasm32_gate(
        &mut self,
        scratch: &Scratch,
        r: &str,
        export: Option<&Path>,
        tools: Result<&Wasm32Tools, &aterm_uds::host::Absent>,
    ) -> Outcome {
        let config = self
            .git_out(&["show", &format!("{r}:publish/config.sh")])
            .unwrap_or_default();
        let shape = match wasm_clause(&config) {
            Ok(shape) => shape,
            Err(why) => return Outcome::Refuse(why),
        };
        let not_run = |why: String, fix: String| Outcome::CouldNotRun(Refusal::new(why, fix));
        let Wasm32Tools { rustup, path } = match tools {
            Ok(tools) => tools,
            Err(absent) => {
                return not_run(
                    format!(
                        "{} — the wasm32 pin is a stock channel rustup installs",
                        absent.why
                    ),
                    absent.fix.clone(),
                );
            }
        };
        let src = scratch.dir.join("wasm-src");
        let log = scratch.dir.join("wasm-check.log");
        let _ = std::fs::remove_dir_all(&src);
        let laid = std::fs::create_dir_all(&src).is_ok()
            && match export {
                Some(export) => self
                    .output_forwarded(command("/bin/cp").arg("-R").arg(export.join(".")).arg(&src))
                    .is_ok_and(|o| o.status.success()),
                None => self.archive_into(r, &src),
            };
        if !laid {
            return not_run(
                format!("could not lay {} out in {}", short(r), src.display()),
                "fix what keeps it from being written, then run it again".into(),
            );
        }
        let mut env: Vec<(String, std::ffi::OsString)> = vec![
            (
                "CARGO_TARGET_DIR".into(),
                scratch.cache.join("wasm-target").into_os_string(),
            ),
            (
                "TERM".into(),
                std::env::var_os("TERM").unwrap_or_else(|| "dumb".into()),
            ),
            ("PATH".into(), path.clone()),
        ];
        for keep in [
            "HOME",
            "LANG",
            "LC_ALL",
            "LC_CTYPE",
            "TZ",
            "TMPDIR",
            "RUSTUP_HOME",
            "CARGO_HOME",
            "SSL_CERT_FILE",
            "SSL_CERT_DIR",
        ] {
            if let Some(v) = std::env::var_os(keep) {
                env.push((keep.into(), v));
            }
        }
        let scrubbed = |program: &Path| {
            let mut cmd = command(program);
            cmd.env_clear().envs(env.iter().cloned()).current_dir(&src);
            cmd
        };
        let add = self.output_forwarded(scrubbed(&rustup.join("rustup")).args([
            "target",
            "add",
            "--toolchain",
            &shape.pin,
            "wasm32-unknown-unknown",
        ]));
        let mut text = String::new();
        let added = match &add {
            Ok(o) => {
                text.push_str(&String::from_utf8_lossy(&o.stdout));
                text.push_str(&String::from_utf8_lossy(&o.stderr));
                o.status.success()
            }
            Err(e) => {
                text.push_str(&format!("cannot run rustup: {e}\n"));
                false
            }
        };
        if !added {
            let _ = crate::durable::write_fresh(&log, &text);
            return not_run(
                format!(
                    "rustup could not add wasm32-unknown-unknown to the wasm32 pin {}",
                    shape.pin
                ),
                format!(
                    "rustup toolchain install {} --target wasm32-unknown-unknown (full log: {})",
                    shape.pin,
                    log.display()
                ),
            );
        }
        let checked =
            self.output_forwarded(scrubbed(Path::new("/bin/sh")).args(["-c", &shape.clause]));
        let ok = match &checked {
            Ok(o) => {
                text.push_str(&String::from_utf8_lossy(&o.stdout));
                text.push_str(&String::from_utf8_lossy(&o.stderr));
                o.status.success()
            }
            Err(e) => {
                text.push_str(&format!("cannot run sh: {e}\n"));
                false
            }
        };
        let _ = crate::durable::write_fresh(&log, &text);
        if ok {
            return Outcome::Pass(format!(
                "wasm32 engine: aterm-wasm compiles for wasm32-unknown-unknown on {}",
                shape.pin
            ));
        }
        if checked.is_err()
            || text.contains("can't find crate for `core`")
            || text.contains("target may not be installed")
        {
            return not_run(
                format!(
                    "the wasm32 clause did not run on {} (its target is missing)",
                    shape.pin
                ),
                format!(
                    "rustup target add --toolchain {} wasm32-unknown-unknown (full log: {})",
                    shape.pin,
                    log.display()
                ),
            );
        }
        Outcome::Refuse(
            Refusal::new(
                format!(
                    "aterm-wasm does NOT compile for wasm32-unknown-unknown on the wasm32 pin {} \
                     — `pub stage` refuses this tree, and the site could not build its /terminal \
                     engine from it",
                    shape.pin
                ),
                format!(
                    "fix the source — a cfg(not(target_os = \"macos\")) arm no host build \
                     compiles is the usual culprit; full log: {}",
                    log.display()
                ),
            )
            .detail(
                text.lines()
                    .filter(|l| l.starts_with("error"))
                    .take(6)
                    .map(str::to_string),
            ),
        )
    }

    fn archive_into(&self, r: &str, dest: &Path) -> bool {
        let script = format!(
            "command set -o pipefail 2>/dev/null; git {} -C \"$1\" archive \"$2\" | tar -x -C \"$3\"",
            BYTE_EXACT.join(" ")
        );
        let tar = |repo: &Path, rev: &str, into: &Path| -> bool {
            let mut pipe = command("/bin/sh");
            pipe.args(["-c", &script, "archive"])
                .arg(repo)
                .arg(rev)
                .arg(into);
            self.output_forwarded(&mut pipe)
                .is_ok_and(|o| o.status.success())
        };
        if !tar(&self.repo, r, dest) {
            return false;
        }
        let _ = std::fs::remove_dir_all(dest.join(".cargo"));
        crate::gates::gitlinks(&self.git_out(&["ls-tree", "-r", r]).unwrap_or_default())
            .into_iter()
            .all(|(rev, path)| {
                let into = dest.join(&path);
                std::fs::create_dir_all(&into).is_ok() && tar(&self.repo.join(&path), &rev, &into)
            })
    }

    /// `pub stage` of R (to the staging tier, or `--dry-run` without its push) on an engine
    /// fast-forwarded to its origin immediately before admission, with this run's scanner
    /// tools, then the engine synced again. In step
    /// 4's graph (`branch`: its stop and the export's share of the pool as `CARGO_BUILD_JOBS`)
    /// it logs as the export branch, its push begun read from its lines ([`PushWatch`]), and an
    /// engine that cannot be synced after it is COULD NOT RUN, no red of R; outside it (step 7's
    /// stage again) it logs as `step`'s child. A local check must leave a complete passing
    /// attestation of R.
    fn export_run(
        &self,
        c: &Candidate,
        prover: &Prover,
        target: StageTarget,
        step: Step,
        branch: Option<(&BranchStop, Option<u32>)>,
    ) -> Result<Ran, Refusal> {
        self.engine_admit_stage().map_err(|refusal| match branch {
            Some(_) => refusal.could_not_run(),
            None => refusal,
        })?;
        let (cmd, handed) = self.stage_command(&c.commit, prover, target)?;
        let mut cmd = self.with_scanners(cmd);
        if let Some((_, Some(jobs))) = branch {
            cmd.env("CARGO_BUILD_JOBS", jobs.to_string());
        }
        let log = match branch {
            Some(_) => self.side_log(step, Branch::Export.name()),
            None => self.step_log(step),
        };
        if let Ok(mut file) = crate::durable::open_append(&log, true) {
            let _ = writeln!(file, "{handed}");
        }
        let push = branch
            .filter(|_| target == StageTarget::Remote && !self.scratch_logs)
            .map(|(stop, _)| PushWatch {
                stop: stop.clone(),
                transcript: self.transcript(),
                line: Vec::new(),
                seen: false,
            });
        let ran = self.logged_in(
            Logged {
                step,
                log,
                stream: self.verbose,
                // The engine's KeyboardInterrupt ends its gate's whole tree.
                stop: branch.map(|(stop, _)| (stop, interrupt::SIGINT)),
                push,
                arrivals: false,
                settles: false,
            },
            &mut cmd,
        );
        if target == StageTarget::Local && ran.code == 0 {
            let path = self.repo.join("publish/.out/attestation.json");
            let qualified = crate::gates::read_bounded_string(&path, 16 << 20)
                .is_ok_and(|text| attestation_qualifies(&text, &c.commit));
            if !qualified {
                return Err(Refusal::new(
                    "the local export check returned success without a complete passing attestation for this candidate",
                    format!(
                        "read {} and the stage log before qualifying again",
                        path.display()
                    ),
                ));
            }
        }
        self.engine_sync().map_err(|refusal| match branch {
            Some(_) => refusal.could_not_run(),
            None => refusal,
        })?;
        if ran.code != 0
            && EngineRun::read(ran.code, &ran.detail.join("\n"))
                .refusals
                .iter()
                .any(|why| {
                    (why.starts_with("publication engine HEAD ")
                        && why.contains("does not exactly match queried origin/main"))
                        || why.starts_with("publication engine origin/main changed during stage")
                })
        {
            return Err(Refusal::new(
                "publication engine origin race: the engine moved after stage admission; no aterm tree failure was established",
                format!("run {RELEASE_COMMAND} again with the same candidate; sync refreshes the engine"),
            )
            .could_not_run()
            .detail(ran.detail));
        }
        Ok(ran)
    }

    fn prepare_source_signing(&mut self) -> Result<(), Refusal> {
        if self.source_signing.is_none() {
            self.prepare_prerequisites()?;
            self.source_signing = Some(crate::source_signing::Tools::probe_held(
                &self.repo, &self.home, i64::try_from(now()).unwrap_or(i64::MAX), self.prepared_source_helper.as_deref(),
                self.prerequisites.as_ref().ok_or_else(|| prerequisite_refusal("publication prerequisites were not acquired".into()))?.clone(),
            ).map_err(|why| Refusal::new(format!("source signing preflight: {why}"), "restore the source signing tools and the complete registered release ring, then retry"))?);
        }
        Ok(())
    }

    fn prepare_scanner_tools(&mut self, prover: &Prover) -> Result<(), Refusal> {
        self.scanner_tools = None;
        self.prepare_prerequisites()?;
        let publication = self.engine_child(&self.engine, Some(prover))?;
        self.scanner_tools = Some(
            scanner_tools::prepare(
                &publication,
                self.engine_prefix_len(),
                &self.logs.join("scanner-tools.json"),
                &[&self.repo],
                |cmd| {
                    self.apply_prerequisites(cmd)?;
                    (self.scanner_probe)(cmd)
                },
            )
            .map_err(scanner_refusal)?,
        );
        Ok(())
    }

    /// The stage step's `pub stage aterm --commit <commit> --repo-path <repo>`, a [`Real::child`]
    /// with `prover`'s bin, handed this cutter for its transforms only as `commit`'s own build
    /// ([`hand_transforms_cutter`], whose line it returns beside it).
    fn stage_command(
        &self,
        commit: &str,
        prover: &Prover,
        target: StageTarget,
    ) -> Result<(Command, String), Refusal> {
        let mut cmd = self.engine_child(&self.engine, Some(prover))?;
        cmd.args(["stage", "aterm", "--commit", commit, "--repo-path"])
            .arg(&self.repo);
        if target == StageTarget::Local {
            cmd.arg("--dry-run");
        }
        let handed = hand_transforms_cutter(&mut cmd, commit);
        Ok((cmd, handed))
    }

    fn captured(
        &self,
        step: Step,
        prover: &Prover,
        log: &Path,
        cmd: &mut Command,
    ) -> std::io::Result<std::process::Output> {
        self.captured_with_stop(step, prover, log, cmd, None)
    }

    fn captured_with_stop(
        &self,
        step: Step,
        prover: &Prover,
        log: &Path,
        cmd: &mut Command,
        stop: Option<&BranchStop>,
    ) -> std::io::Result<std::process::Output> {
        self.captured_with_jobs(step, prover, log, cmd, stop, None)
    }

    fn captured_with_jobs(
        &self,
        step: Step,
        prover: &Prover,
        log: &Path,
        cmd: &mut Command,
        stop: Option<&BranchStop>,
        jobs: Option<&aterm_capacity::BranchShares>,
    ) -> std::io::Result<std::process::Output> {
        scrub_fixture_command(cmd);
        if step == Step::Qualify
            && let Some(jobs) = jobs
        {
            jobs.hand_to(cmd, false)?;
        }
        cmd.env("TRUST_STAGE2_BIN", &prover.bin);
        let (out, record) = self.output_accounted_with_stop(step, cmd, stop)?;
        append_output(log, &out);
        if let Ok(mut file) = crate::durable::open_append(log, true) {
            let _ = writeln!(file, "{record}");
        }
        Ok(out)
    }

    fn stage_fixtures_run(
        &self,
        step: Step,
        c: &Candidate,
        reserved: &Reservation,
        prover: &Prover,
        log: &Path,
    ) -> Result<String, Refusal> {
        self.stage_fixtures_run_with_stop(step, c, reserved, prover, log, None)
    }

    /// [`Self::stage_fixtures_run`], with the qualify build branch's stop attached to every fallback
    /// compiler or generator child. A graph verdict which stops Build therefore ends fixture work
    /// already in flight. An eligible test-execution red deliberately retains Build and does not
    /// fire this stop.
    fn stage_fixtures_run_with_stop(
        &self,
        step: Step,
        c: &Candidate,
        reserved: &Reservation,
        prover: &Prover,
        log: &Path,
        stop: Option<&BranchStop>,
    ) -> Result<String, Refusal> {
        // The generator compiles the clone's working tree, so that tree must be R's.
        if let Some(off) = self.off_tree(&c.commit) {
            return Err(Refusal::new(
                format!(
                    "the clone is not at {}'s tree and clean, so the handoff fixtures would not be its",
                    short(&c.commit)
                ),
                format!("restore the clean candidate tree, then {RELEASE_COMMAND}"),
            )
            .detail(off));
        }
        let pins = fixture_pins(c, reserved);
        let root = fixture_root(&self.repo)?;
        let mut run = |cmd: &mut Command| {
            self.captured_with_jobs(
                step,
                prover,
                log,
                cmd,
                stop,
                self.pool.as_ref().map(|pool| &pool.jobs),
            )
        };
        crate::fixtures::stage(
            &prover.bin.join("targo"),
            &self.repo,
            &root,
            &pins,
            &mut run,
        )
        .map(|staged| staged.words(&pins))
        .map_err(|e| fixture_refusal(c, &e.to_string()))
    }

    /// When this graph runs the tree half, consume the executable index that compile publishes
    /// before [`Self::stage_fixtures_run`] may choose its fallback compile. A stopped sibling starts no
    /// new fixture work; a tree that ended without usable evidence permits the bounded fallback.
    fn await_fixture_index(
        &self,
        c: &Candidate,
        prover: &Prover,
        handoff: &TreeIndexHandoff,
        stop: &BranchStop,
    ) -> Result<(), Refusal> {
        if !handoff.expected() {
            return Ok(());
        }
        let _timing = crate::cut_timing::Timer::parallel_parent()("qualify fixture index wait")
            .with_log(Some(&self.side_log(Step::Qualify, "build-fixtures")));
        let index = crate::fixtures::GeneratorIndex::new(&prover.bin.join("targo"), &self.repo)
            .map_err(|e| fixture_refusal(c, &e.to_string()))?;
        match await_tree_index(
            handoff,
            || index.ready(),
            || stop.fired() || interrupt::caught().is_some(),
            || std::thread::sleep(std::time::Duration::from_millis(250)),
        ) {
            TreeIndexAnswer::Ready | TreeIndexAnswer::Compile => Ok(()),
            TreeIndexAnswer::Stopped => Err(Refusal::new(
                "the handoff fixtures stopped before the tree compile supplied their executable",
                format!("run {RELEASE_COMMAND} again; the other branch's verdict stands"),
            )
            .could_not_run()),
        }
    }

    /// `verify release` of `run`'s half in the clone at R ([`Branches::verify`]): step 4's tree
    /// branch or early bundle-context AppKit gate, and step 5's artifact-dependent bundle checks,
    /// each into its own log and events; records are kept only where R's tree stayed clean and the
    /// seal held throughout.
    fn verify_half(
        &self,
        step: Step,
        c: &Candidate,
        prover: &Prover,
        run: &crate::check_record::VerifyRun,
        artifact: Option<&Artifact>,
        stop: &BranchStop,
    ) -> Result<Ran, Refusal> {
        let r = short(&c.commit).to_string();
        let fix = format!(
            "put the clone back at {r} and clean (git -C {} status), then {RELEASE_COMMAND}",
            self.repo.display()
        );
        if let Some(off) = self.off_tree(&c.commit) {
            return Err(Refusal::new(
                format!("the clone is not at {r}'s tree and clean, so `verify release` would not judge it"),
                fix,
            )
            .detail(off)
            .could_not_run());
        }
        let unsealed = |error: String| {
            Refusal::new(
                format!("{error} — nothing was recorded"),
                "restore the release build inputs and retry",
            )
            .could_not_run()
        };
        let seal = (self.build_inputs)().map_err(unsealed)?;
        let tree = self
            .git_out(&["rev-parse", &format!("{}^{{tree}}", c.commit)])
            .ok_or_else(|| {
                Refusal::new(
                    format!("git cannot name {r}'s tree — nothing ran"),
                    fix.clone(),
                )
                .could_not_run()
            })?;
        let exact = |name: &str| run.checks.len() == 1 && run.checks[0] == name;
        let log = match (step, run.half, artifact) {
            (Step::Qualify, crate::check_record::Half::Tree, None) => {
                self.side_log(Step::Qualify, Branch::Tree.name())
            }
            (Step::Qualify, crate::check_record::Half::Bundle, None) if exact("appkit") => {
                self.side_log(Step::Qualify, "appkit")
            }
            (Step::Bundle, crate::check_record::Half::Bundle, Some(_)) if exact("measure") => {
                self.step_log(Step::Bundle)
            }
            (_, half, _) => {
                return Err(Refusal::new(
                    format!(
                        "the {half:?} release selection {:?} cannot run in step {} with this artifact",
                        run.checks,
                        step.name(),
                    ),
                    format!("run {RELEASE_COMMAND} again with the selected cutter"),
                )
                .could_not_run());
            }
        };
        let early_appkit = step == Step::Qualify
            && run.half == crate::check_record::Half::Bundle
            && artifact.is_none()
            && exact("appkit");
        let side = |what: &str| log.with_extension(what);
        let cannot = |path: &Path, e: std::io::Error| {
            Refusal::new(
                format!("write {}: {e} — nothing ran", path.display()),
                format!("check {}, then {RELEASE_COMMAND}", self.logs.display()),
            )
            .could_not_run()
        };
        let frozen = crate::release_tools::recorded(&self.release, &c.commit).map_err(|why| {
            Refusal::new(
                format!("R's recorded verifier is unavailable ({why}) — nothing ran"),
                format!("run {RELEASE_COMMAND} again to rebuild and prove R's tools"),
            )
            .could_not_run()
        })?;
        let mut cmd = self.verifier_child(&frozen.verify.path, Some(prover), run.half);
        self.hand_pool(&mut cmd, true)?;
        // `spawn_stoppable` applies the signed prerequisite environment at the last boundary.
        // Build an identical one-use mirror and finalize that mutation now, so content evidence
        // binds the effective verifier environment without applying PATH prepends twice to the
        // command which will actually spawn.
        let admission = if early_appkit {
            crate::check_content::Admission::Now
        } else {
            crate::check_content::Admission::WaitDefault
        };
        let content_context = self.check_content_context(
            &frozen.verify.path,
            &frozen.set_sha256,
            &seal,
            prover,
            run.half,
            admission,
        );
        let content_plan = content_context
            .as_ref()
            .map_err(|why| why.clone())
            .and_then(|context| {
                let appkit =
                    early_appkit.then_some(crate::check_content::AppkitContext { context });
                crate::check_content::plan(&self.git(), &c.commit, context, appkit)
            })
            .ok();
        let item_context = crate::check_content::item_context(&frozen.set_sha256, &seal);
        cmd.arg("release").arg(run.half.flag());
        if early_appkit {
            cmd.args(["--admit", "now"]);
        }
        if run
            .checks
            .iter()
            .any(|check| matches!(check.as_str(), "test" | "lint"))
        {
            cmd.arg("--carry-context").arg(&item_context);
        } else if early_appkit && let Ok(context) = &content_context {
            cmd.arg("--carry-context").arg(context);
        }
        if run.strict_subset() {
            cmd.arg("--checks").arg(run.checks.join(","));
        }
        if !run.passed_binaries.is_empty() {
            let path = side("passed-binaries");
            let mut proofs = aterm_verify::testrun::Passed::new();
            for binary in &run.passed_binaries {
                if let Some(proof) = binary.proof() {
                    proofs.insert(proof.spec.clone(), proof);
                }
            }
            let text = aterm_verify::testrun::encode_passed(&proofs).map_err(|e| {
                Refusal::new(
                    format!("cannot encode prior test-binary content proofs: {e}"),
                    format!("remove {}, then {RELEASE_COMMAND}", path.display()),
                )
                .could_not_run()
            })?;
            crate::durable::write_atomic_durable(&path, text.as_bytes(), 0o600)
                .map_err(|e| cannot(&path, e))?;
            cmd.arg("--passed-binaries").arg(&path);
        }
        if !run.passed_lint_packages.is_empty() {
            let path = side("passed-lint-packages");
            let mut proofs = aterm_verify::lint_content::Passed::new();
            for package in &run.passed_lint_packages {
                if let Some(proof) = package.proof() {
                    proofs.insert(proof.spec.clone(), proof);
                }
            }
            let text = aterm_verify::lint_content::encode_passed(&proofs).map_err(|error| {
                Refusal::new(
                    format!("cannot encode prior lint-package content proofs: {error}"),
                    format!("remove {}, then {RELEASE_COMMAND}", path.display()),
                )
                .could_not_run()
            })?;
            crate::durable::write_atomic_durable(&path, text.as_bytes(), 0o600)
                .map_err(|error| cannot(&path, error))?;
            cmd.arg("--passed-lint-packages").arg(&path);
        }
        if !run.passed_appkit_groups.is_empty() && content_context.is_ok() {
            let path = side("passed-appkit-groups");
            let proofs: aterm_verify::appkit_content::Passed = run
                .passed_appkit_groups
                .iter()
                .cloned()
                .map(|proof| (proof.group.clone(), proof))
                .collect();
            let text = aterm_verify::appkit_content::encode_passed(&proofs).map_err(|error| {
                Refusal::new(
                    format!("cannot encode prior AppKit executable-group proofs: {error}"),
                    format!("remove {}, then {RELEASE_COMMAND}", path.display()),
                )
                .could_not_run()
            })?;
            crate::durable::write_atomic_durable(&path, text.as_bytes(), 0o600)
                .map_err(|error| cannot(&path, error))?;
            cmd.arg("--passed-appkit-groups").arg(&path);
        }
        // The records are read from verify's own events file, which no child and no other writer
        // shares, never from its output; it starts empty, so it holds this run's alone.
        let events = side("events");
        crate::durable::write_atomic_durable(&events, b"", 0o600)
            .map_err(|e| cannot(&events, e))?;
        cmd.arg("--events").arg(&events);
        // `--app` goes with the check that reads it, and verify refuses it beside none.
        let artifact = artifact.filter(|_| run.half == crate::check_record::Half::Bundle);
        if run.reads_the_bundle() {
            let Some(artifact) = artifact else {
                return Err(Refusal::new(
                    "`verify release` would run measure with no bundle to judge — nothing ran",
                    format!("{RELEASE_COMMAND} (step 4 builds it)"),
                )
                .could_not_run());
            };
            cmd.arg("--app").arg(&artifact.app);
        }
        let root = self.release.join(crate::check_record::DIR);
        let run_id = self
            .logs
            .file_name()
            .map(|n| n.to_string_lossy().into_owned())
            .unwrap_or_default();
        let facts = crate::check_record::RunFacts {
            commit: &c.commit,
            tree: &tree,
            seal: &seal,
            executable: artifact.map(|a| a.macho_sha256.as_str()),
            log: &log,
            run: &run_id,
            asked: &run.checks,
            content: content_plan.as_ref(),
        };
        // From here until every FAIL it shows is kept, the run reads as a FAIL of R's tree.
        crate::check_record::begin_run(&root, &facts).map_err(|e| {
            Refusal::new(
                format!("cannot record that `verify release` begins: {e} — nothing ran"),
                format!("fix what it names, then {RELEASE_COMMAND}"),
            )
            .could_not_run()
        })?;
        let mut ran = self.logged_in(
            Logged {
                step,
                log: log.clone(),
                // The bundle half runs alone; the tree's output would interleave the build's.
                stream: run.half == crate::check_record::Half::Bundle || self.verbose,
                stop: Some((stop, interrupt::SIGTERM)),
                push: None,
                arrivals: false,
                settles: false,
            },
            &mut cmd,
        );
        // Records are kept only for R's tree, clean throughout, under the seal the run began
        // with: either moving makes the run COULD NOT RUN, and it keeps nothing.
        let abandoned = |refusal: Refusal| match crate::check_record::abandon_run(&root, &facts) {
            Ok(()) => refusal,
            Err(e) => refusal.detail([format!("its begun record stays: {e}")]),
        };
        if let Some(off) = self.off_tree(&c.commit) {
            return Err(abandoned(
                Refusal::new(
                    format!(
                        "the clone moved during `verify release`, so its records are not {r}'s — \
                         nothing was recorded"
                    ),
                    fix,
                )
                .detail(off)
                .could_not_run(),
            ));
        }
        match (self.build_inputs)() {
            Ok(now) if now == seal => {}
            Ok(_) => {
                return Err(abandoned(unsealed(
                    "compiler or SDK bytes changed during release verification".into(),
                )));
            }
            Err(e) => return Err(abandoned(unsealed(e))),
        }
        let log_sha256 = crate::artifact::file_sha256(&log).unwrap_or_else(|e| {
            ran.detail.push(format!("its log could not be hashed: {e}"));
            String::new()
        });
        let read = crate::check_record::events(&events)
            .map_err(|e| format!("read {}: {e}", events.display()));
        if let Ok(events) = &read
            && let Err(why) = crate::check_record::keep_harnesses(&root, &tree, &seal, events)
        {
            ran.detail.push(why);
        }
        let written = crate::check_record::write_run(
            &root,
            &facts,
            crate::check_record::RunEnd {
                exit: ran.code,
                log_sha256: &log_sha256,
                events: read.as_ref().map_err(String::as_str),
            },
        );
        match written {
            Ok(lines) => ran
                .detail
                .extend(lines.into_iter().map(|line| format!("record: {line}"))),
            Err(e) => ran
                .detail
                .push(format!("its records were not all written: {e}")),
        }
        if let Some(artifact) = artifact
            && read.as_ref().is_ok_and(|events| {
                events.whole()
                    && events.finished.iter().any(|e| {
                        e["check"].as_str() == Some("measure")
                            && matches!(e["outcome"].as_str(), Some("ok" | "FAIL"))
                    })
            })
            && let Ok(content) = crate::artifact::content_digest(&artifact.app)
            && let Ok(Some(product)) =
                crate::check_record::product_key(&root, &tree, &seal, &content)
            && let Err(why) = crate::check_record::remember_product(&root, &facts, &product)
        {
            ran.detail.push(why);
        }
        Ok(ran)
    }

    /// The build branch: `ship build` of R under `reserved` (when `work.bundle`), then the
    /// release's own handoff fixtures (when `work.fixtures`) — after the slices, so the
    /// generator's compile never adds to their memory peak — each into the branch's log.
    fn build_branch(
        &self,
        c: &Candidate,
        reserved: &Reservation,
        prover: &Prover,
        work: BuildWork,
        tree_index: &TreeIndexHandoff,
        stop: &BranchStop,
    ) -> Built {
        let log = self.side_log(Step::Qualify, Branch::Build.name());
        let artifact = build_artifact_with_handoff(work.bundle, tree_index, || {
            (self.build_run_op)(self, c, reserved, prover, &log, stop)
        });
        let built = artifact.as_ref().is_none_or(Result::is_ok);
        let fixtures = (work.fixtures && built && !stop.fired()).then(|| {
            let log = self.side_log(Step::Qualify, "build-fixtures");
            if tree_index.expected()
                && let Some(pool) = &self.pool
            {
                pool.jobs.build_compilers_drained();
            }
            (self.await_fixture_index_op)(self, c, prover, tree_index, stop).and_then(|()| {
                let _share = {
                    let _timing =
                        crate::cut_timing::Timer::parallel_parent()("qualify fixture core wait")
                            .with_log(Some(&log));
                    self.pool
                        .as_ref()
                        .filter(|_| tree_index.expected())
                        .map(|pool| {
                            pool.jobs
                                .fixture_share(&|| stop.fired() || interrupt::caught().is_some())
                        })
                        .transpose()
                        .map_err(|error| {
                            let refusal = fixture_refusal(c, &error.to_string());
                            if error.kind() == std::io::ErrorKind::Interrupted {
                                refusal.could_not_run()
                            } else {
                                refusal
                            }
                        })?
                };
                (self.stage_fixtures_run_op)(
                    self,
                    Step::Qualify,
                    c,
                    reserved,
                    prover,
                    &log,
                    Some(stop),
                )
            })
        });
        Built { artifact, fixtures }
    }

    fn build_run(
        &self,
        c: &Candidate,
        reserved: &Reservation,
        prover: &Prover,
        log: &Path,
        stop: &BranchStop,
    ) -> Result<Artifact, Refusal> {
        let mut cmd = self
            .cut_command(&["build".to_string()], prover)
            .map_err(|why| {
                Refusal::new(
                    format!("the build was not started: {why}"),
                    format!("run {RELEASE_COMMAND} again from the clone (nothing was claimed)"),
                )
            })?;
        self.hand_pool(&mut cmd, false)?;
        // A killed cutter cannot forward a stop. Tie the build worker to it; the
        // worker similarly ties each Cargo group to its own lifetime.
        cmd.tether()
            .env(crate::buildplan::SLICE_LOGS_ENV, &self.logs);
        let ran = self.logged_in(
            Logged {
                step: Step::Qualify,
                log: log.to_path_buf(),
                stream: self.verbose,
                stop: Some((stop, interrupt::SIGTERM)),
                push: None,
                arrivals: false,
                settles: false,
            },
            &mut cmd,
        );
        if ran.code != 0 {
            return Err(Refusal::new(
                format!("the build did not finish (ship build exited {})", ran.code),
                format!(
                    "read its log; for a red build, {} (nothing was claimed)",
                    crate::release::fix_remedy(&c.commit)
                ),
            )
            .detail(ran.detail));
        }
        let identity = crate::artifact::Identity {
            version: c.version.clone(),
            build: reserved.build,
            commit: reserved.commit.clone(),
        };
        let found = crate::artifact::compilers()
            .map(|compilers| crate::artifact::key(&identity, &compilers))
            .and_then(|key| {
                let store = crate::artifact::store(&self.repo)?;
                crate::artifact::read(&store, &key).map(|c| c.map(|c| (store, c)))
            });
        match found {
            Ok(Some((store, complete))) if complete.trustc == prover.commit => Ok(Artifact {
                app: complete.app(&store),
                key: complete.key,
                macho_sha256: complete.macho_sha256,
                build: complete.build,
            }),
            Ok(Some((_, complete))) => Err(Refusal::new(
                format!(
                    "the build ran with trustc {}, not this release's prover {}",
                    complete.trustc, prover.commit
                ),
                format!("{RELEASE_COMMAND} (it builds with the release's prover)"),
            )),
            Ok(None) => Err(Refusal::new(
                "the build ended, and the release cache holds no COMPLETE bundle for it",
                format!("read its log, then {RELEASE_COMMAND}"),
            )
            .detail(ran.detail)),
            Err(e) => Err(Refusal::new(
                format!("the built bundle cannot be read back: {e}"),
                format!("read its log, then {RELEASE_COMMAND}"),
            )),
        }
    }

    fn release_json(&self, slug: &str, tag: &str) -> Result<aterm_json::Value, String> {
        self.github_read(&format!("repos/{slug}/releases/tags/{tag}"))
            .map_err(|e| format!("could not read {slug} {tag} from GitHub's API ({e})"))
    }

    fn pointer_problems(
        &self,
        slug: &str,
        tag: &str,
        version: &str,
        release: &aterm_json::Value,
    ) -> Result<Vec<String>, String> {
        let want = format!("https://github.com/{slug}/releases/download/{tag}/aterm-appcast.toml");
        let spawn: std::sync::Arc<dyn aterm_update_core::github_rest::Spawn> =
            std::sync::Arc::new(crate::github::Launched(self.step_children()));
        let problems = settled_pointer_problems(
            &latest_pointer_urls(slug, tag, release),
            tag,
            &mut |ptr| {
                aterm_update_core::head_no_redirect(ptr)
                    .map(|hop| (hop.code, hop.location))
                    .map_err(|e| format!("{ptr} could not be read: {e:?}"))
            },
            &mut || {
                crate::github::pages_via(
                    Some(&spawn),
                    &format!("repos/{slug}/releases?per_page=100"),
                    100,
                )
                .map(aterm_json::Value::Array)
                .map_err(|e| format!("the release list for {slug} could not be read ({e})"))
            },
            &mut |pause| {
                crate::cut_timing::measure("after-cut pointer propagation wait", || {
                    std::thread::sleep(pause);
                });
            },
        )?;
        if !problems.is_empty() {
            return Ok(problems);
        }
        let served = appcast_bytes(&want, atpkg::net::MANIFEST_CAP, self.step_children())
            .map_err(|e| format!("the appcast at {want} could not be fetched anonymously ({e})"))?;
        let text = String::from_utf8_lossy(&served);
        Ok(appcast_problems(&text, slug, tag, version))
    }

    fn site_tag(&self, slug: &str) -> Result<String, String> {
        let list = self
            .github_read(&format!("repos/{slug}/releases?per_page=30"))
            .map_err(|e| {
                format!("the release list for {slug} could not be read from GitHub's API ({e})")
            })?;
        let catalog = crate::site::releases::parse_releases(&list.to_string())
            .map_err(|e| format!("the release list for {slug} is not release JSON ({e})"))?;
        crate::site::releases::elect_mac(&catalog, slug, &mut Vec::new())
            .map(|(mac, _)| mac.tag)
            .map_err(|e| format!("the site could not tell which release to link on {slug} ({e})"))
    }

    fn step_children(&self) -> StepChildren {
        StepChildren {
            noted: (!self.scratch_logs).then(|| (self.release.clone(), self.step)),
        }
    }

    fn github_read(&self, path: &str) -> Result<aterm_json::Value, String> {
        let spawn: std::sync::Arc<dyn aterm_update_core::github_rest::Spawn> =
            std::sync::Arc::new(crate::github::Launched(self.step_children()));
        crate::github::get_via(Some(&spawn), path).map_err(|e| e.to_string())
    }

    /// [`Real::github_read`], `None` for GitHub's 404 (an answer, never retried).
    fn github_read_optional(&self, path: &str) -> Result<Option<aterm_json::Value>, String> {
        let spawn: std::sync::Arc<dyn aterm_update_core::github_rest::Spawn> =
            std::sync::Arc::new(crate::github::Launched(self.step_children()));
        crate::github::get_optional_via(Some(&spawn), path).map_err(|e| e.to_string())
    }

    fn site(&self, slug: &str) -> Result<AfterCut, Refusal> {
        if interrupt::caught().is_some() {
            return Err(Refusal::new(
                "not started: the release was interrupted",
                format!("{RELEASE_COMMAND} again"),
            ));
        }
        let log = self.side_log(Step::AfterCut, "site");
        let file = crate::durable::open_append(&log, true).map_err(|e| {
            Refusal::new(
                format!("cannot open {}: {e}", log.display()),
                format!("{RELEASE_COMMAND} again once it is writable"),
            )
            .could_not_run()
        })?;
        let mut out = StepLog {
            file,
            echo: self.verbose,
        };
        let _ = writeln!(out, "$ ship site --latest ({slug}, in process)");
        let env = crate::site::Env::from_process(&self.repo, Some(slug)).map_err(|e| {
            Refusal::new(
                format!("the site settings cannot be read: {e}"),
                format!("fix publish/site.toml, then {RELEASE_COMMAND}"),
            )
        })?;
        let mut world = crate::site::Real {
            unset: std::env::vars_os()
                .filter_map(|(k, _)| k.into_string().ok())
                .filter(|k| clears(k) || launcher_env(k))
                .collect(),
            launch: std::rc::Rc::new(self.step_children()),
            curl: std::sync::Arc::new(crate::github::Launched(self.step_children())),
            reader: aterm_update_core::release_asset::Reader::anonymous(),
        };
        let opts = crate::site::Options {
            latest: true,
            ..crate::site::Options::default()
        };
        let ended = crate::site::sync(&opts, &env, &mut world, &mut out);
        match &ended {
            Ok(crate::site::Outcome::NoCheckout(line)) => {
                let _ = writeln!(out, "ship site: {line}");
            }
            Err(failure) => {
                let _ = writeln!(out, "ship site: FAIL: {failure}");
            }
            Ok(_) => {}
        }
        site_outcome(&ended, tail(&log, 8))
    }
}

struct StepLog {
    file: std::fs::File,
    echo: bool,
}

impl std::io::Write for StepLog {
    fn write(&mut self, buf: &[u8]) -> std::io::Result<usize> {
        if self.echo {
            let _ = std::io::stdout().write_all(buf);
        }
        self.file.write_all(buf)?;
        Ok(buf.len())
    }

    fn flush(&mut self) -> std::io::Result<()> {
        if self.echo {
            let _ = std::io::stdout().flush();
        }
        self.file.flush()
    }
}

/// The after-cut step's refusal of a published release whose shape is wrong.
#[must_use]
pub fn shape_refusal(slug: &str, tag: &str, problems: Vec<String>) -> Refusal {
    Refusal::new(
        format!(
            "{slug} {tag} does NOT carry the one-lean-download shape ({} problem(s)) — the site \
             was NOT touched, so its download stays on the last release that passed",
            problems.len()
        ),
        format!(
            "a published release is never re-cut, so this one ends here: ship its successor \
             from current main with {RELEASE_COMMAND} --forget (this clone's journal of \
             {tag}; nothing public changes), then {RELEASE_COMMAND} --bump, or a bare \
             {RELEASE_COMMAND} where origin/main already declares a version no release has \
             used (docs/RELEASING.md, Resume, recut, abandon)"
        ),
    )
    .detail(problems)
}

/// What `ship site --latest` ending with `ended` means for the after-cut step; `detail` is the end of its transcript.
/// # Errors
/// Any result other than a deployed website verified live, or a failure of the sync.
pub fn site_outcome(
    ended: &Result<crate::site::Outcome, crate::site::Failure>,
    detail: Vec<String>,
) -> Result<AfterCut, Refusal> {
    use crate::site::Outcome;
    let note = |s: &str| Ok(AfterCut { note: s.into() });
    match ended {
        Ok(Outcome::Synced { deployed: true }) => {
            note("alab.systems brought to the release and deployed (ship site --latest)")
        }
        Ok(Outcome::Synced { deployed: false }) => {
            note("alab.systems already shows the release (ship site --latest)")
        }
        Ok(outcome @ (Outcome::LiveUnchecked { .. } | Outcome::NoDeploy
            | Outcome::NotDeployed | Outcome::Skipped | Outcome::Deferred
            | Outcome::Planned { .. } | Outcome::NoCheckout(_))) => Err(Refusal::new(
                format!("the application is published, but the website is not deployed and verified: {outcome:?}"),
                format!("run {} with its configured checkout and deployment credential, then resume {RELEASE_COMMAND}; the application will not be rebuilt or republished", crate::site::DEPLOY_LATER),
            ).detail(detail)),
        Ok(Outcome::LiveDisagrees { .. }) => Err(Refusal::new(
            "the site is deployed, but the live site does not agree yet (ship site exited 4)",
            format!("{RELEASE_COMMAND} again in a few minutes"),
        )
        .detail(detail)),
        Err(crate::site::Failure::CouldNotRun { why }) => Err(Refusal::new(
            format!("the release is well shaped, but the site sync could not run: {why}"),
            format!("retry {RELEASE_COMMAND} when the transport answers"),
        )
        .detail(detail)
        .could_not_run()),
        Err(why) => Err(Refusal::new(
            format!("the release is well shaped, but the site sync failed: {why}"),
            format!(
                "read its log (a failed push leaves its site commit on this machine), then \
                 {RELEASE_COMMAND}"
            ),
        )
        .detail(detail)),
    }
}

/// PUBLIC-SHAPE-BEFORE-WRITES: no distribution writer starts without the
/// public cohort's shape verdict. Both writers join, including on a refusal.
fn distribute_after_shape(
    log: &Path,
    shape: Result<(), Refusal>,
    site: impl FnOnce() -> Result<AfterCut, Refusal> + Send,
    tap: impl FnOnce() -> Result<String, Refusal> + Send,
) -> Result<(AfterCut, String), Refusal> {
    shape?;
    let (site, tap) = crate::cut_timing::independent_logged(
        Some(log),
        ("after-cut site", site),
        ("after-cut tap", tap),
    );
    Ok((site?, tap?))
}

fn built_answer(repo: &Path, artifact: Option<&Artifact>, prover: &Prover) -> Answer {
    let Some(a) = artifact else {
        return Answer::no("no bundle is built for it yet");
    };
    let read = crate::artifact::store(repo).and_then(|store| crate::artifact::read(&store, &a.key));
    match read {
        Ok(Some(complete)) if complete.macho_sha256 != a.macho_sha256 => Answer::no(format!(
            "the release cache's bundle {} is not the journal's (executable {}, journaled {})",
            short(&a.key),
            short(&complete.macho_sha256),
            short(&a.macho_sha256)
        )),
        Ok(Some(complete)) if complete.trustc != prover.commit => Answer::no(format!(
            "its bundle was built by trustc {}, and this release's prover is trustc {}",
            short(&complete.trustc),
            short(&prover.commit)
        )),
        Ok(Some(complete)) => match crate::build_inputs::current() {
            Ok(inputs) if inputs == complete.inputs => Answer::yes(format!(
                "built once: {} (executable sha256 {})",
                a.app.display(),
                short(&a.macho_sha256)
            )),
            Ok(_) => Answer::no("the compiler or SDK bytes changed since this bundle was built"),
            Err(error) => Answer::no(format!(
                "the bundle's build inputs cannot be checked: {error}"
            )),
        },
        Ok(None) => Answer::no(format!(
            "the release cache holds no COMPLETE bundle {}",
            short(&a.key)
        )),
        Err(e) => Answer::no(format!("the release cache's bundle is not usable: {e}")),
    }
}

fn now() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(0, |d| d.as_secs())
}

fn now_ms() -> Option<u64> {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .ok()
        .and_then(|d| u64::try_from(d.as_millis()).ok())
}

/// A child that exits normally gets this long for every inherited pipe writer to close and for
/// its already-written output to reach the log. A descendant that keeps a writer is not allowed
/// to keep the pump thread alive past the step boundary.
const LOG_DRAIN_AFTER_EXIT: std::time::Duration = std::time::Duration::from_secs(5);

struct LoggedPipe {
    reader: std::io::PipeReader,
    #[cfg(unix)]
    cancel_reader: std::io::PipeReader,
    #[cfg(unix)]
    cancel_writer: std::io::PipeWriter,
}

impl LoggedPipe {
    fn new() -> std::io::Result<(Self, std::io::PipeWriter, std::io::PipeWriter)> {
        let (reader, writer) = std::io::pipe()?;
        let stderr = writer.try_clone()?;
        #[cfg(unix)]
        let (cancel_reader, cancel_writer) = std::io::pipe()?;
        Ok((
            Self {
                reader,
                #[cfg(unix)]
                cancel_reader,
                #[cfg(unix)]
                cancel_writer,
            },
            writer,
            stderr,
        ))
    }
}

/// The one reader of a logged child's shared stdout/stderr pipe. Its owner first gives it the
/// drain grace, then wakes it through `cancel` and joins it, so no output thread survives a step.
struct OutputPump {
    finished: std::sync::mpsc::Receiver<()>,
    thread: std::thread::JoinHandle<()>,
    #[cfg(unix)]
    cancel: std::io::PipeWriter,
}

#[cfg(test)]
struct PumpActive(Option<std::sync::Arc<std::sync::atomic::AtomicBool>>);

#[cfg(test)]
impl PumpActive {
    fn new(active: Option<std::sync::Arc<std::sync::atomic::AtomicBool>>) -> Self {
        if let Some(active) = &active {
            active.store(true, std::sync::atomic::Ordering::SeqCst);
        }
        Self(active)
    }
}

#[cfg(test)]
impl Drop for PumpActive {
    fn drop(&mut self) {
        if let Some(active) = &self.0 {
            active.store(false, std::sync::atomic::Ordering::SeqCst);
        }
    }
}

impl OutputPump {
    fn finish(self, drain: std::time::Duration) {
        let _ = self.finished.recv_timeout(drain);
        #[cfg(unix)]
        drop(self.cancel);
        let _ = self.thread.join();
    }
}

/// How [`Real::logged_in`] runs a step's child.
struct Logged<'a> {
    step: Step,
    log: PathBuf,
    stream: bool,
    /// A qualify branch's stop, and the signal it sends the child's group.
    stop: Option<(&'a BranchStop, i32)>,
    /// The export's watch for its staging push.
    push: Option<PushWatch>,
    /// Its lines' arrivals are kept as `publish-line` records ([`Arrivals`]): the engine's
    /// publish.
    arrivals: bool,
    /// It settles this process's own stop ([`interrupt::stop_self`], the fleet line lost):
    /// it starts unless a signal from elsewhere was caught ([`interrupt::caught_foreign`]),
    /// and only such a signal is forwarded to it ([`Real::spawn_settling`]).
    settles: bool,
}

/// The engine's line that its export's public-clone check passed: what `pub stage` prints right
/// before it pushes the snapshot to the staging tier (`swap_staging_main`), with nothing between.
const PUSH_BEGINS: &str = "== PASSED";
const PUSH_BEGINS_AFTER: &str = " under anonymous git: ";

/// Reads an export's output for the line its staging push begins after ([`PUSH_BEGINS`]): once
/// it comes, the branch's stop can no longer end it ([`BranchStop::push`]), and the release's
/// transcript says so (a `qualify-branch` `push-began` event). The engine runs unbuffered, so
/// the line comes as it is printed.
struct PushWatch {
    stop: BranchStop,
    transcript: PathBuf,
    line: Vec<u8>,
    seen: bool,
}

impl PushWatch {
    fn read(&mut self, bytes: &[u8]) {
        for &b in bytes {
            if b != b'\n' {
                if self.line.len() < 4096 {
                    self.line.push(b);
                }
                continue;
            }
            let line = String::from_utf8_lossy(&self.line).into_owned();
            self.line.clear();
            if !self.seen && line.starts_with(PUSH_BEGINS) && line.contains(PUSH_BEGINS_AFTER) {
                self.seen = true;
                let (stop, transcript) = (&self.stop, &self.transcript);
                // The push and its event in the graph's one order: no red is told between them.
                stop.in_order(|| {
                    if stop.push() {
                        let at = now();
                        say_to(
                            transcript,
                            &format!(
                                "release-event: {{\"schema\":1,\"event\":\"qualify-branch\",\
                                 \"branch\":\"export\",\"phase\":\"push-began\",\
                                 \"at_unix_s\":{at},\"seconds\":0}}"
                            ),
                        );
                    }
                });
            }
        }
    }
}

/// Say `line` on this process's terminal and in the run's transcript, in one write each.
fn say_to(transcript: &Path, line: &str) {
    let whole = format!("{line}\n");
    let mut out = std::io::stdout().lock();
    let _ = out.write_all(whole.as_bytes());
    let _ = out.flush();
    if let Ok(mut f) = crate::durable::open_append(transcript, true) {
        let _ = f.write_all(whole.as_bytes());
    }
}

/// When each line of a step's output arrived, as `publish-line` records in a side log: the
/// engine stamps none of its own, and promote's sections reach the log only once it ends.
struct Arrivals {
    file: std::fs::File,
    line: Vec<u8>,
}

impl Arrivals {
    /// The head of a line kept: enough to name the engine's section, never its whole output.
    const HEAD: usize = 200;

    fn new(file: std::fs::File) -> Self {
        Self {
            file,
            line: Vec::new(),
        }
    }

    fn read(&mut self, bytes: &[u8]) {
        let mut rest = bytes;
        while let Some(pos) = rest.iter().position(|&b| b == b'\n') {
            let text = if self.line.is_empty() {
                String::from_utf8_lossy(&rest[..pos.min(Self::HEAD)])
            } else {
                let take = Self::HEAD.saturating_sub(self.line.len()).min(pos);
                self.line.extend_from_slice(&rest[..take]);
                String::from_utf8_lossy(&self.line)
            };
            let _ = writeln!(
                self.file,
                "release-event: {}",
                aterm_json::Value::Object(aterm_json::Map::from([
                    ("schema".to_string(), 1.into()),
                    ("event".to_string(), "publish-line".into()),
                    (
                        "at_unix_ms".to_string(),
                        now_ms().map_or(aterm_json::Value::Null, Into::into),
                    ),
                    ("line".to_string(), text.into_owned().into()),
                ]))
            );
            self.line.clear();
            rest = &rest[pos + 1..];
        }
        let take = Self::HEAD.saturating_sub(self.line.len()).min(rest.len());
        self.line.extend_from_slice(&rest[..take]);
    }
}

/// Wait ownership stays here. The observer started at spawn samples descendants across their
/// groups and sessions, using verify's CPU, starvation and simultaneous-footprint accounting.
fn wait_accounted(
    child: &mut aterm_uds::spawnfd::Child,
    forwarded: &mut Forwarded,
) -> std::io::Result<(
    std::process::ExitStatus,
    Option<aterm_verify::exec::TreeUsage>,
)> {
    let status = child.wait()?;
    let usage = forwarded.meter.take().and_then(|meter| meter.finish());
    Ok((status, usage))
}

/// Cargo's artifact units, including build-script executables, counted once per reported
/// artifact. Build-script execution messages are not compiler units. Missing freshness is
/// unknown rather than evidence that a unit compiled.
fn frozen_build_event(package: &str, profile: &str, layout: Option<&str>, output: &[u8]) -> String {
    let (mut fresh, mut compiled, mut unknown) = (0u64, 0u64, 0u64);
    for line in output.split(|byte| *byte == b'\n') {
        let Ok(row) = aterm_json::from_slice::<aterm_json::Value>(line) else {
            continue;
        };
        if row["reason"].as_str() != Some("compiler-artifact") {
            continue;
        }
        match row["fresh"].as_bool() {
            Some(true) => fresh += 1,
            Some(false) => compiled += 1,
            None => unknown += 1,
        }
    }
    format!(
        "release-event: {}",
        aterm_json::json!({
            "schema": 1,
            "event": "frozen-tool-compile",
            "package": package,
            "profile": profile,
            "build_layout": layout,
            "fresh_units": fresh,
            "compiled_units": compiled,
            "unknown_units": unknown,
        })
    )
}

/// The `step-child-finished` record a step's child leaves in its step log: when it ran, how it
/// exited and what its descendants were observed using. Planning data, never a verdict,
/// so it carries no `outcome` (a retried child's exit is not the run's first red).
fn step_child_event(
    step: Step,
    cmd: &Command,
    started_at_unix_ms: Option<u64>,
    ended_at_unix_ms: Option<u64>,
    exit_code: i32,
    usage: Option<aterm_verify::exec::TreeUsage>,
) -> String {
    let mut program = Path::new(cmd.get_program())
        .file_name()
        .unwrap_or(cmd.get_program())
        .to_string_lossy()
        .into_owned();
    for arg in cmd.get_args().take(3) {
        program.push(' ');
        program.push_str(&arg.to_string_lossy());
    }
    let child_cpu = usage.map_or(aterm_json::Value::Null, |u| {
        aterm_json::Value::Object(aterm_json::Map::from([
            ("scope".to_string(), "sampled-descendant-processes".into()),
            ("observed_s".to_string(), u.cpu.as_secs_f64().into()),
            ("processes".to_string(), (u.processes as u64).into()),
        ]))
    });
    let peak_bytes = usage
        .map(|u| u.peak_bytes)
        .filter(|b| *b > 0)
        .map_or(aterm_json::Value::Null, Into::into);
    format!(
        "release-event: {}",
        aterm_json::Value::Object(aterm_json::Map::from([
            ("schema".to_string(), 1.into()),
            ("event".to_string(), "step-child-finished".into()),
            ("step".to_string(), step.name().into()),
            ("program".to_string(), program.into()),
            (
                "started_at_unix_ms".to_string(),
                started_at_unix_ms.map_or(aterm_json::Value::Null, Into::into),
            ),
            (
                "ended_at_unix_ms".to_string(),
                ended_at_unix_ms.map_or(aterm_json::Value::Null, Into::into),
            ),
            ("exit_code".to_string(), exit_code.into()),
            ("child_cpu".to_string(), child_cpu),
            (
                "starved_s".to_string(),
                aterm_json::json!(usage.map(|u| u.starved.as_secs_f64()))
            ),
            (
                "runnable_s".to_string(),
                aterm_json::json!(usage.map(|u| u.runnable.as_secs_f64()))
            ),
            ("peak_bytes".to_string(), peak_bytes),
        ]))
    )
}

fn short(sha: &str) -> &str {
    sha.get(..9).unwrap_or(sha)
}

fn local_stamp(unix: u64, offset: i64) -> String {
    let local = i64::try_from(unix)
        .unwrap_or(i64::MAX)
        .saturating_add(offset);
    let (y, m, d) = aterm_types::rfc3339::civil_from_days(local.div_euclid(86_400));
    let s = local.rem_euclid(86_400);
    format!(
        "{y:04}{m:02}{d:02}T{:02}{:02}{:02}",
        s / 3600,
        s % 3600 / 60,
        s % 60
    )
}

/// The variables cleared from every child, so every step builds what the operator's shell would: each `RUST*` and `CARGO*` (target-directory overrides included) but the inert knobs (job count, incremental, logging, backtraces, cargo's terminal output), and the C toolchain's (the `cc` crate's names, also as `<NAME>_<target>`, `TARGET_<NAME>`, `HOST_<NAME>`).
pub(crate) use aterm_toolchain::release_env::{clears, launcher_env};

fn scrub(cmd: &mut Command) {
    scrub_names(cmd, std::env::vars_os().map(|(name, _)| name));
}

/// The engine source a release named when it started (`--engine`, else `PUBLICATION_ENGINE`, else
/// `~/publication`), handed to the frozen cutter it execs in place ([`Backend::exec_cutter`]).
/// That image is also named the release's own copy as `PUBLICATION_ENGINE`, the engine a cutter
/// older than this handover syncs in place. A cutter that reads it takes the same source as the
/// release that execed it, so its own sync makes the copy where none exists yet and makes a
/// clean copy of another origin again from that source ([`Real::engine_copy`]). The first exec
/// comes before step 1 and the release's sync (a bump's second hop comes after it), so engine
/// discovery belongs to R's own cutter.
const ENGINE_SOURCE_ENV: &str = "ATERM_RELEASE_ENGINE_SOURCE";

/// The engine repository (`OWNER/REPO`) a release captured when it started, handed beside
/// [`ENGINE_SOURCE_ENV`]: the authority the exec'd image holds to ([`engine_authority`]), or
/// [`ENGINE_REPOSITORY_NONE`] where the release admitted none.
const ENGINE_REPOSITORY_ENV: &str = "ATERM_RELEASE_ENGINE_REPOSITORY";

/// [`ENGINE_REPOSITORY_ENV`] where the release admitted no engine repository: the image refuses
/// as the release would, with its reason ([`ENGINE_REPOSITORY_WHY_ENV`]), and hands the same to
/// the next image it execs, however many hops follow.
const ENGINE_REPOSITORY_NONE: &str = "none";

/// Why a release that admitted no engine repository ([`ENGINE_REPOSITORY_NONE`]) refused it: the
/// capture's own refusal, which the exec'd image refuses with in turn, unchanged.
const ENGINE_REPOSITORY_WHY_ENV: &str = "ATERM_RELEASE_ENGINE_REPOSITORY_WHY";

/// Hand `cmd`, a release's own frozen cutter, the engine `source` the release named and the
/// repository it `captured` ([`ENGINE_SOURCE_ENV`], [`ENGINE_REPOSITORY_ENV`]), or why it
/// admitted none ([`ENGINE_REPOSITORY_WHY_ENV`]).
fn hand_engine_capture(cmd: &mut Command, source: &Path, captured: &Result<agh::Slug, String>) {
    cmd.env(ENGINE_SOURCE_ENV, source);
    match captured {
        Ok(slug) => cmd.env(ENGINE_REPOSITORY_ENV, slug.to_string()),
        Err(why) => cmd
            .env(ENGINE_REPOSITORY_ENV, ENGINE_REPOSITORY_NONE)
            .env(ENGINE_REPOSITORY_WHY_ENV, why),
    };
}

/// The engine repository this release holds to: the one its engine source names (`named`),
/// unless it was `handed` one ([`ENGINE_REPOSITORY_ENV`]) by the release that execed it, which it
/// then holds to, accepting only a source naming that one, and refusing where that release
/// admitted none ([`ENGINE_REPOSITORY_NONE`]), with the reason it was handed (`why`,
/// [`ENGINE_REPOSITORY_WHY_ENV`]). A handover can only refuse, never widen: every engine
/// operation also checks the source's own name against it ([`Real::engine_remote`]).
fn engine_authority(
    named: Result<agh::Slug, String>,
    handed: Option<std::ffi::OsString>,
    why: Option<std::ffi::OsString>,
) -> Result<agh::Slug, String> {
    let Some(handed) = handed.filter(|handed| !handed.is_empty()) else {
        return named;
    };
    if handed == ENGINE_REPOSITORY_NONE {
        return Err(why.filter(|why| !why.is_empty()).map_or_else(
            || {
                "the release that started this cutter admitted no publication engine repository"
                    .into()
            },
            |why| why.to_string_lossy().into_owned(),
        ));
    }
    let handed = handed
        .to_str()
        .and_then(|handed| agh::Slug::parse(handed).ok())
        .ok_or_else(|| {
            format!(
                "{ENGINE_REPOSITORY_ENV} names no OWNER/REPO: {}",
                handed.to_string_lossy()
            )
        })?;
    match named {
        Ok(named) if named == handed => Ok(handed),
        Ok(named) => Err(format!(
            "the release that started this cutter captured {handed} as its publication engine, \
             and the engine it handed it names {named}"
        )),
        Err(why) => Err(format!(
            "the release that started this cutter captured {handed} as its publication engine, \
             and the engine it handed it names no repository: {why}"
        )),
    }
}

/// A direct generator has an explicit runtime manifest from the checked artifact.
/// Keep that value after removing the launcher's inherited Cargo environment.
fn scrub_fixture_command(cmd: &mut Command) {
    let manifest = cmd
        .get_envs()
        .find(|(name, _)| *name == "CARGO_MANIFEST_DIR")
        .and_then(|(_, value)| value.map(std::ffi::OsStr::to_owned));
    scrub(cmd);
    if let Some(manifest) = manifest {
        cmd.env("CARGO_MANIFEST_DIR", manifest);
    }
}

/// Remove from `cmd` each of `inherited` (the names of this process's environment) that a step
/// runs without: what [`clears`] and [`launcher_env`] name, the cutter this process may have
/// been handed ([`RELEASE_CUTTER_ENV`], [`RELEASE_CUTTER_SINCE_ENV`]), which a child has only
/// from [`hand_this_cutter`], step 7's gate ([`PUBLISH_GATE_ENV`]), which only the source's
/// `pub publish` is handed ([`Branches::source`]), the build-inputs seal it may have been handed
/// ([`crate::build_inputs::SEAL_ENV`]), which a child has only from [`hand_inputs_seal`], a
/// disk claim it may have been named ([`aterm_capacity::DISK_COVER_ENV`]), which a child has
/// only from this release's own claim ([`Real::scrubbed`]), and the engine source and repository
/// a release captured ([`ENGINE_SOURCE_ENV`], [`ENGINE_REPOSITORY_ENV`],
/// [`ENGINE_REPOSITORY_WHY_ENV`]), which only its own frozen cutter is handed
/// ([`hand_engine_capture`]).
fn scrub_names(cmd: &mut Command, inherited: impl IntoIterator<Item = std::ffi::OsString>) {
    for name in inherited {
        if let Some(name) = name.to_str()
            && (clears(name)
                || launcher_env(name)
                || name == RELEASE_CUTTER_ENV
                || name == RELEASE_CUTTER_SINCE_ENV
                || name == crate::release_tools::RECORD_ENV
                || name == crate::release_tools::FREEZER_ENV
                || name == crate::release_tools::CONTRACT_ENV
                || name == crate::release_tools::REFREEZE_ENV
                || name == crate::cut_timing::EXEC_PID_ENV
                || name == PUBLISH_GATE_ENV
                || name == crate::build_inputs::SEAL_ENV
                || name == aterm_capacity::DISK_COVER_ENV
                || name == ENGINE_SOURCE_ENV
                || name == ENGINE_REPOSITORY_ENV
                || name == ENGINE_REPOSITORY_WHY_ENV)
        {
            cmd.env_remove(name);
        }
    }
    cmd.env(aterm_capacity::RELEASE_RUN_ENV, "1");
}

/// Hand `cmd`, scrubbed ([`scrub_names`]), this process's kept build-inputs seal
/// ([`crate::build_inputs::handed_path`]), so the child adopts it after a recheck by stat
/// instead of hashing the inputs again; with none kept, the child has none.
fn hand_inputs_seal(cmd: &mut Command, kept: Option<PathBuf>) {
    if let Some(path) = kept {
        cmd.env(crate::build_inputs::SEAL_ENV, path);
    }
}

fn fixture_pins(c: &Candidate, reserved: &Reservation) -> crate::fixtures::Pins {
    crate::fixtures::Pins {
        version: c.version.clone(),
        build: reserved.build,
        commit: reserved.commit.clone(),
    }
}

fn fixture_root(repo: &Path) -> Result<PathBuf, Refusal> {
    crate::snapshot::CacheRoot::for_repo(repo)
        .map(|cache| cache.handoff_fixtures())
        .map_err(|e| {
            Refusal::new(
                format!("the release cache: {e}"),
                format!("run {RELEASE_COMMAND} again once it is readable"),
            )
            .could_not_run()
        })
}

fn fixture_refusal(c: &Candidate, text: &str) -> Refusal {
    Refusal::new(
        text.lines().next().unwrap_or("").to_string(),
        format!(
            "fix it (targo --unverified test -p aterm-gui --lib -- {} with the pins {} \
             names): {}",
            crate::fixtures::generator_args().join(" "),
            crate::gates::HANDOFF_FIXTURE_README,
            crate::release::fix_remedy(&c.commit)
        ),
    )
    .detail(text.lines().skip(1).take(8).map(str::to_string))
}

fn cut_refusal(gate: &str, why: &str) -> Refusal {
    let first = why.lines().next().unwrap_or("");
    let fix = why
        .lines()
        .find_map(|l| l.trim().strip_prefix("fix:"))
        .map_or_else(
            || "the cut's own remedy, above".to_string(),
            |f| f.trim().to_string(),
        );
    Refusal::new(format!("{gate} — {first}"), fix).detail(
        why.lines()
            .skip(1)
            .filter(|l| !l.trim().starts_with("fix:"))
            .map(str::to_string),
    )
}

/// The private bump's construction, not its editor, failed at `gate`: nothing was judged, so
/// the dry run could not run; `remedy` says what to restore or wait for.
fn private_bump_unavailable(gate: &str, why: &str, remedy: String) -> Refusal {
    let mut refusal = cut_refusal(gate, why).could_not_run();
    refusal.remedy = remedy;
    refusal
}

/// What restores the private checkout `place` when the private bump's own cleanup failed: a
/// command that runs as pasted, naming `place` as one shell word.
fn private_bump_cleanup_remedy(place: &Path) -> String {
    format!(
        "restore the private checkout before the next dry run: git -C {} restore \
         --source=HEAD --staged --worktree -- .",
        crate::release::shell_word(&place.to_string_lossy())
    )
}

fn append_output(log: &Path, out: &std::process::Output) {
    if let Ok(mut f) = crate::durable::open_append(log, true) {
        let _ = f.write_all(&out.stdout);
        let _ = f.write_all(&out.stderr);
    }
}

fn tail(path: &Path, n: usize) -> Vec<String> {
    tail_since(path, n, 0)
}

/// A child's verdict cannot inherit a previous invocation's failure from an appended log.
fn tail_since(path: &Path, n: usize, from: u64) -> Vec<String> {
    let text = crate::durable::read_text(path).unwrap_or_default();
    let text = usize::try_from(from)
        .ok()
        .and_then(|offset| text.get(offset..))
        .unwrap_or_default();
    let mut out: Vec<String> = text.lines().rev().take(n).map(str::to_string).collect();
    out.reverse();
    out
}

/// What to do about the release's own engine `e` in a state no release leaves it in: nothing
/// but a release writes it, so whatever made the state is read (`git -C e <read>`) and kept where
/// it is wanted before the copy goes; the next release clones it again.
fn engine_origin_url(repo: &Path) -> Result<String, String> {
    let out = crate::ledger::git_ok(&GitCli::new(repo), &["remote", "get-url", "origin"])
        .map_err(|e| e.to_string())?;
    let url = out.stdout_utf8().trim().to_string();
    if url.is_empty() {
        Err("engine has no origin URL".into())
    } else {
        Ok(url)
    }
}

fn copy_remedy(e: &str, read: &str) -> String {
    format!(
        "nothing but a release writes it: read git -C {e} {read}, land what is wanted on the \
         engine's main, then remove {e} (the next release clones it again) and {RELEASE_COMMAND} \
         again"
    )
}

/// A path of the engine's records rather than its code (`bin/pub`'s `ENGINE_DATA_PLANE` and
/// `ENGINE_DATA_PLANE_PREFIXES`): its ledger, its audit log, its handoff records.
fn engine_data_plane(path: &str) -> bool {
    matches!(path, "mappings.json" | "verification-log.jsonl") || path.starts_with("handoff/")
}

/// A changed engine path that is engine code, as the engine's own stage guard judges a move of
/// its HEAD (`bin/pub`'s `engine_code_drift`): neither its records ([`engine_data_plane`]), its
/// Rust port (`RUST_PORT_FILES`, `crates/`), which no `bin/pub` verb a release runs reads, nor
/// its prose (`docs/`, a top-level `*.md`), which `bin/pub` never opens. A `*.md` anywhere else
/// (`bin/`, `registry/`) is code. Read from `git diff --no-renames --name-only`, so a rename
/// lists the path it left as well as the one it took.
fn engine_code(path: &str) -> bool {
    let port = matches!(path, "Cargo.toml" | "Cargo.lock" | "rust-toolchain.toml")
        || path.starts_with("crates/");
    let prose = path.starts_with("docs/") || (!path.contains('/') && path.ends_with(".md"));
    !path.is_empty() && !engine_data_plane(path) && !port && !prose
}

/// Each kept audit row put back in time order (the pub-sync pattern).
#[must_use]
pub fn audit_rows_merged(text: &str, kept: &[String]) -> String {
    if kept.iter().all(|row| text.lines().any(|line| line == row)) {
        if !text.is_empty()
            && !text.contains('\r')
            && text.ends_with('\n')
            && !text.ends_with("\n\n")
        {
            return text.to_string();
        }
        let mut out = String::with_capacity(text.len().max(1));
        for (idx, row) in text.lines().enumerate() {
            if idx > 0 {
                out.push('\n');
            }
            out.push_str(row);
        }
        out.push('\n');
        return out;
    }
    let at = |row: &str| -> String {
        aterm_json::from_str::<aterm_json::Value>(row)
            .ok()
            .and_then(|v| {
                v.get("at")
                    .and_then(aterm_json::Value::as_str)
                    .map(str::to_string)
            })
            .unwrap_or_default()
    };
    let mut rows: Vec<(String, &str)> = text.lines().map(|r| (at(r), r)).collect();
    for row in kept {
        if rows.iter().any(|(_, r)| *r == row.as_str()) {
            continue;
        }
        let when = at(row);
        let i = rows
            .iter()
            .position(|(row_at, _)| row_at > &when)
            .unwrap_or(rows.len());
        rows.insert(i, (when, row.as_str()));
    }
    let cap: usize = rows.iter().map(|(_, r)| r.len() + 1).sum::<usize>().max(1);
    let mut out = String::with_capacity(cap);
    for (idx, (_, row)) in rows.iter().enumerate() {
        if idx > 0 {
            out.push('\n');
        }
        out.push_str(row);
    }
    out.push('\n');
    out
}

/// Where one `ship check` lays its sources out: its own `runs/<pid>-<n>` inside the [`preflight_cache`], so no two runs share one and none waits for another.
pub(crate) struct Scratch {
    pub(crate) dir: PathBuf,
    pub(crate) cache: PathBuf,
}

/// Candidate-bound export client prepared before qualify's branches start.
pub(crate) struct LocalExportClient {
    path: PathBuf,
    source: String,
    sha256: String,
    jobs: u32,
    wall_s: f64,
}

impl LocalExportClient {
    pub(crate) fn record(&self) -> aterm_json::Value {
        aterm_json::json!({
            "kind":"qualify-export-client", "source":self.source,
            "path":self.path, "sha256":self.sha256,
            "requested_jobs":self.jobs, "wall_s":self.wall_s,
        })
    }

    fn validate(&self, source: &str) -> Result<(), String> {
        if source != self.source {
            return Err("the prepared export client belongs to another candidate".into());
        }
        let (sha256, _) = crate::dmg::sha256_and_size(&self.path)?;
        if sha256 != self.sha256 {
            return Err("the prepared export client changed before use".into());
        }
        Ok(())
    }
}

/// Match qualify preparation: build the engine client before any graph branch starts.
/// The caller's measured CPU allocation bounds this build, including with a cold target.
pub(crate) fn prepare_local_export_client(
    repo: &Path,
    root: &Path,
    source: &str,
    jobs: u32,
) -> Result<LocalExportClient, String> {
    std::fs::create_dir_all(root).map_err(|error| error.to_string())?;
    let started = std::time::Instant::now();
    let _phase = crate::cut_timing::Timer::start(
        &root.join("gate.log"),
        "qualify export client preparation",
    );
    let mut real = Real::new(repo, None, true, false)?;
    real.logs = root.to_owned();
    real.scratch_logs = false;
    real.step = Step::Qualify;
    let path = real
        .engine_agh_with_jobs(Some(jobs))
        .map_err(|why| why.why)?;
    let (sha256, _) = crate::dmg::sha256_and_size(&path)?;
    let client = LocalExportClient {
        path,
        source: source.to_owned(),
        sha256,
        jobs,
        wall_s: started.elapsed().as_secs_f64(),
    };
    crate::durable::write_fresh(&root.join("result.json"), client.record().to_string())
        .map_err(|error| error.to_string())?;
    Ok(client)
}

/// Run qualify's serial AppKit prefix through its production bundle-half path. Every take
/// asks for the complete check, with no carried groups and no built-bundle input.
pub(crate) fn measure_local_appkit(
    repo: &Path,
    root: &Path,
    commit: &str,
) -> Result<aterm_json::Value, String> {
    std::fs::create_dir_all(root).map_err(|error| error.to_string())?;
    let started = std::time::Instant::now();
    let started_at_unix_ms = crate::cut_timing::epoch_ms();
    let _phase = crate::cut_timing::Timer::parallel_parent()("qualify serial AppKit prefix")
        .with_log(Some(&root.join("gate.log")));
    let mut real = Real::new(repo, None, true, false)?;
    real.logs = root.to_owned();
    real.scratch_logs = false;
    real.step = Step::Qualify;
    let prover = real.prover(None)?;
    let candidate = Candidate {
        commit: commit.to_owned(),
        version: real
            .version_at(commit)
            .ok_or("the AppKit candidate has no release version")?,
        why: "local full-gate measurement".to_owned(),
        bump_from: None,
        carry: Vec::new(),
    };
    let run = crate::check_record::VerifyRun {
        half: crate::check_record::Half::Bundle,
        checks: vec!["appkit".to_owned()],
        passed_binaries: Vec::new(),
        passed_lint_packages: Vec::new(),
        passed_appkit_groups: Vec::new(),
    };
    let frozen = crate::release_tools::recorded(&real.release, commit)?;
    let ran = real.verify_half(
        Step::Qualify,
        &candidate,
        &prover,
        &run,
        None,
        &BranchStop::default(),
    );
    let success = ran.as_ref().is_ok_and(|ran| ran.code == 0);
    let result = aterm_json::json!({
        "kind":"real-serial-appkit", "commit":commit, "success":success,
        "wall_s":started.elapsed().as_secs_f64(),
        "started_at_unix_ms":started_at_unix_ms,
        "ended_at_unix_ms":crate::cut_timing::epoch_ms(),
        "verifier":frozen.verify.path, "verifier_sha256":frozen.verify.sha256,
        "outcome":format!("{ran:?}"),
    });
    crate::durable::write_fresh(&root.join("result.json"), result.to_string())
        .map_err(|error| error.to_string())?;
    if !success {
        return Err(format!("the serial AppKit prefix did not pass: {result}"));
    }
    Ok(result)
}

/// Run all local export guards with a candidate-bound client prepared within the CPU budget.
pub(crate) fn measure_local_export(
    repo: &Path,
    root: &Path,
    commit: &str,
    jobs: u32,
    prepared: Option<&LocalExportClient>,
) -> Result<aterm_json::Value, String> {
    std::fs::create_dir_all(root).map_err(|error| error.to_string())?;
    let started = std::time::Instant::now();
    let _phase = crate::cut_timing::Timer::parallel_parent()("qualify local full export")
        .with_log(Some(&root.join("gate.log")));
    let mut real = Real::new(repo, None, true, false)?;
    real.logs = root.to_owned();
    real.scratch_logs = false;
    real.step = Step::Qualify;
    let owned;
    let client = if let Some(client) = prepared {
        client
    } else {
        owned = prepare_local_export_client(repo, &root.join("client"), commit, jobs)?;
        &owned
    };
    client.validate(commit)?;
    real.engine_agh_explicit = Some(client.path.as_os_str().to_owned());
    let scratch = Scratch {
        dir: root.to_owned(),
        cache: root.join("cache"),
    };
    let (outcome, _) = real.export_gate_with_check(&scratch, commit, Some(jobs));
    let success = matches!(outcome, Outcome::Pass(_));
    let result = aterm_json::json!({
        "kind":"local-full-export",
        "success":success,
        "wall_s":started.elapsed().as_secs_f64(),
        "outcome":format!("{outcome:?}"),
        "requested_jobs":jobs,
        "staging_push":false,
    });
    crate::durable::write_fresh(&root.join("result.json"), result.to_string())
        .map_err(|error| error.to_string())?;
    Ok(result)
}

/// The real fixture producer after both slice clients have drained. Keep the same index
/// dependency as the cut, with fresh output for every take instead of carrying a prior stage.
pub(crate) fn measure_local_fixtures(
    repo: &Path,
    root: &Path,
    commit: &str,
    tree_ended: &std::sync::atomic::AtomicBool,
    jobs: Option<&aterm_capacity::BranchShares>,
) -> Result<aterm_json::Value, String> {
    std::fs::create_dir_all(root).map_err(|error| error.to_string())?;
    let _phase = crate::cut_timing::Timer::parallel_parent()("qualify real fixtures")
        .with_log(Some(&root.join("gate.log")));
    let mut real = Real::new(repo, None, true, false)?;
    real.logs = root.to_owned();
    real.scratch_logs = false;
    real.step = Step::Qualify;
    let prover = real.prover(None)?;
    let targo = prover.bin.join("targo");
    let layout = diagnostic_verify_layout(repo).map_err(|error| error.to_string())?;
    let index = crate::fixtures::GeneratorIndex::from_plan(&targo, repo, layout.clone())
        .map_err(|error| error.to_string())?;
    let waiting = std::time::Instant::now();
    let handoff = TreeIndexHandoff::default();
    handoff.expect();
    let answer = {
        let _timing = crate::cut_timing::Timer::parallel_parent()("qualify fixture index wait")
            .with_log(Some(&root.join("fixtures.log")));
        await_tree_index(
            &handoff,
            || index.ready(),
            || tree_ended.load(std::sync::atomic::Ordering::SeqCst) && !index.ready(),
            || std::thread::sleep(std::time::Duration::from_millis(250)),
        )
    };
    let index_wait_s = waiting.elapsed().as_secs_f64();
    if answer != TreeIndexAnswer::Ready {
        return Err("the full tree ended without the fixture executable index".into());
    }
    let waiting = std::time::Instant::now();
    let _share = {
        let _timing = crate::cut_timing::Timer::parallel_parent()("qualify fixture core wait")
            .with_log(Some(&root.join("fixtures.log")));
        jobs.map(|jobs| jobs.fixture_share(&|| interrupt::caught().is_some()))
            .transpose()
            .map_err(|error| error.to_string())?
    };
    let core_wait_s = waiting.elapsed().as_secs_f64();
    // The producer derives aged desk timestamps from SOURCE_DATE_EPOCH. Use one real,
    // deterministic epoch for every take of this candidate, as a cut's reservation does.
    let build = crate::snapshot::git_text(repo, &["show", "-s", "--format=%ct", commit])?
        .trim()
        .parse::<u64>()
        .map_err(|error| format!("candidate fixture epoch is invalid: {error}"))?;
    let pins = crate::fixtures::Pins {
        version: real
            .version_at(commit)
            .ok_or("the fixture candidate has no release version")?,
        build,
        commit: commit.to_owned(),
    };
    let started = std::time::Instant::now();
    let outcome = crate::fixtures::stage_with_layout(
        &targo,
        repo,
        &root.join("stage"),
        &pins,
        Some(&layout),
        &mut |cmd| {
            real.captured_with_jobs(
                Step::Qualify,
                &prover,
                &root.join("fixtures.log"),
                cmd,
                None,
                jobs,
            )
        },
    );
    let result = aterm_json::json!({
        "kind":"real-handoff-fixtures", "index_wait_s":index_wait_s, "core_wait_s":core_wait_s,
        "build":build,
        "execution_s":started.elapsed().as_secs_f64(),
        "success":outcome.is_ok(), "outcome":format!("{outcome:?}"),
    });
    crate::durable::write_fresh(&root.join("result.json"), result.to_string())
        .map_err(|error| error.to_string())?;
    if outcome.is_err() {
        eprintln!("qualify-pilot: fixture phase failed; draining the tree: {result}");
        return Err(format!("fixture stage did not pass: {result}"));
    }
    Ok(result)
}

impl Scratch {
    /// A run's own directory, the runs whose process is gone removed first.
    pub(crate) fn read_only(repo: &Path) -> Result<Self, String> {
        static NTH: std::sync::atomic::AtomicU32 = std::sync::atomic::AtomicU32::new(0);
        let cache = preflight_cache(repo)?;
        let runs = cache.join("runs");
        let me = std::process::id();
        for entry in std::fs::read_dir(&runs).into_iter().flatten().flatten() {
            let pid = entry
                .file_name()
                .to_str()
                .and_then(|n| n.split('-').next())
                .filter(|p| p.bytes().all(|b| b.is_ascii_digit()))
                .and_then(|p| p.parse::<u32>().ok());
            if pid.is_some_and(|pid| pid != me && started_of(pid).is_none()) {
                let _ = std::fs::remove_dir_all(entry.path());
            }
        }
        let nth = NTH.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
        let dir = runs.join(format!("{me}-{nth}"));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).map_err(|e| format!("create {}: {e}", dir.display()))?;
        Ok(Self { dir, cache })
    }
}

impl Drop for Scratch {
    fn drop(&mut self) {
        for sources in ["dry-run", "engine", "wasm-src"] {
            let _ = std::fs::remove_dir_all(self.dir.join(sources));
        }
    }
}

/// Where the wasm32 gate finds rustup, and the PATH its clause runs under.
pub(crate) struct Wasm32Tools {
    /// The directory holding `rustup`.
    pub(crate) rustup: PathBuf,
    /// The clause's PATH: `rustup`'s directory, then this process's, so the clause's bare
    /// `rustup` is the row's.
    pub(crate) path: std::ffi::OsString,
}

impl Wasm32Tools {
    /// This machine's: the rustup the `rustup` row of `aterm_uds::host::TOOLS` resolves.
    pub(crate) fn find() -> Result<Self, aterm_uds::host::Absent> {
        rustup_dir().map(|rustup| Self {
            path: path_with(&rustup),
            rustup,
        })
    }
}

/// The directory of the rustup the `rustup` row resolves.
fn rustup_dir() -> Result<PathBuf, aterm_uds::host::Absent> {
    let rustup = aterm_uds::host::tool("rustup").locate()?;
    Ok(rustup.parent().map_or_else(PathBuf::new, Path::to_path_buf))
}

/// How [`Real`] builds a dev binary of its checkout: `targo` with the arguments it is handed (and
/// what the binary is called in its error), answering cargo's JSON messages
/// ([`Real::build_checkout_bin`]).
type CheckoutBuild = fn(
    &Real,
    &str,
    &[std::ffi::OsString],
    &aterm_verify::NativeBuildPlan,
) -> Result<Vec<u8>, String>;

/// The variable the publication engine's GitHub transport (`bin/pub_github.py`) reads for the
/// agh it runs, an absolute path; with none it runs the `agh` on PATH.
const ENGINE_AGH_ENV: &str = "PUBLISH_AGH";

/// The engine's GitHub client, `crates/agh`'s dev shim, as [`crate::checkout_bin`] builds it.
const ENGINE_AGH: crate::checkout_bin::Bin = crate::checkout_bin::Bin {
    package: "agh",
    env: ENGINE_AGH_ENV,
    what: "agh",
};

const FROZEN_CUTTER: crate::checkout_bin::Bin = crate::checkout_bin::Bin {
    package: "aterm-release",
    env: "ATERM_RELEASE_FROZEN_CUTTER",
    what: "release cutter",
};

const FROZEN_VERIFY: crate::checkout_bin::Bin = crate::checkout_bin::Bin {
    package: "aterm-verify",
    env: "ATERM_RELEASE_FROZEN_VERIFY",
    what: "release verifier",
};

/// Git config for the export engine's public reads, as `(key, value)`. Only the engine's
/// `git@github.com:alabsystems/<name>.git` release URLs are rewritten to HTTPS; private origins
/// retain their transport. An empty public credential helper clears inherited helpers, and the
/// following `quit=1` helper refuses helper/askpass credentials. An owner-scoped empty HTTP
/// header clears broad inherited headers; [`public_reads`] also clears exact more-specific
/// public header keys discovered by [`public_header_keys`]. Private headers retain their values.
/// The run is `pub stage --dry-run`; its staging push remains in the engine's non-dry branch.
const PUBLIC_READS: [(&str, &str); 4] = [
    (
        "url.https://github.com/alabsystems/.insteadOf",
        "git@github.com:alabsystems/",
    ),
    ("credential.https://github.com/alabsystems.helper", ""),
    (
        "credential.https://github.com/alabsystems.helper",
        "!f() { echo quit=1; }; f",
    ),
    ("http.https://github.com/alabsystems/.extraHeader", ""),
];

/// Git's config context for the export run. Tests default to the existing hermetic fixture
/// command, so an export stub cannot inspect the person's config; a config fixture injects its
/// owned context directly into [`public_header_keys`].
fn public_config_git(repo: &Path) -> Command {
    #[cfg(not(test))]
    let mut git = command(aterm_uds::os_path!("git"));
    #[cfg(test)]
    let mut git = crate::ledger::testing::git();
    git.arg("-C").arg(repo);
    git
}

/// Discover header key names, never values, in this exact Git command's config context.
/// Conditional includes can add headers when the engine changes repositories, so their
/// directives refuse discovery even when their condition is inactive here.
/// # Errors
/// Git cannot read the config, does not finish within the hang detector, emits non-UTF-8 key
/// names, fills the bounded key-list capture, or a conditional include makes future
/// scratch-repository config context-dependent.
/// Diagnostics contain neither Git's output nor a config key that could hold userinfo.
fn public_header_keys(mut git: Command) -> Result<Vec<String>, Refusal> {
    let refused = || {
        Refusal::new(
            "git could not list HTTP header key names for the anonymous public export read",
            "fix the Git configuration in this checkout, then run the export check again",
        )
    };
    git.args([
        "config",
        "--null",
        "--name-only",
        "--get-regexp",
        "^(http(\\..*)?\\.extraheader|includeif\\..*\\.path)$",
    ])
    .stdin(Stdio::null());
    let out = aterm_uds::deadline::output_within(&mut git, std::time::Duration::from_secs(60))
        .map_err(|_| refused())?;
    if out.status.code() == Some(1) && out.stdout.is_empty() {
        return Ok(Vec::new());
    }
    if !out.status.success() || out.stdout.len() >= aterm_uds::deadline::OUTPUT_KEEP {
        return Err(refused());
    }
    let keys = std::str::from_utf8(&out.stdout).map_err(|_| refused())?;
    let keys: Vec<String> = keys
        .split('\0')
        .filter(|key| !key.is_empty())
        .map(str::to_owned)
        .collect();
    if keys
        .iter()
        .any(|key| key.starts_with("includeif.") && key.ends_with(".path"))
    {
        return Err(Refusal::new(
            "Git configuration has a conditional include, so anonymous export reads cannot prove the settings of future scratch repositories",
            "run the export check with a Git configuration context that has no conditional includes; ordinary unconditional includes remain supported",
        ));
    }
    Ok(keys)
}

/// Select exact ordinary public URL header keys for an override. The owner-wide reset covers
/// broad headers; more-specific public scopes need their own reset. Git normalizes more URL
/// syntax than this bounded selector proves, so an ambiguous GitHub scope refuses the check.
/// # Errors
/// An HTTPS host uses wildcard or escaped syntax, a GitHub port is noncanonical, or a GitHub
/// path uses percent escapes, dot segments, repeated separators, a query, fragment or backslash.
/// No key or credential is echoed.
fn public_header_key(key: &str) -> Result<Option<&str>, Refusal> {
    let Some(scope) = key
        .strip_prefix("http.")
        .and_then(|key| key.strip_suffix(".extraheader"))
    else {
        return Ok(None);
    };
    let Some((scheme, rest)) = scope.split_once("://") else {
        return Ok(None);
    };
    if !scheme.eq_ignore_ascii_case("https") {
        return Ok(None);
    }
    let (authority, path) = rest.split_once('/').unwrap_or((rest, ""));
    let host = authority
        .rsplit_once('@')
        .map_or(authority, |(_, host)| host);
    let host = host.strip_suffix(":443").unwrap_or(host);
    let ambiguous = || {
        Refusal::new(
            "a GitHub HTTP header scope uses URL syntax this anonymous-read guard cannot prove",
            "use ordinary HTTPS GitHub header scopes without wildcards, escapes or dot segments, then run the export check again",
        )
    };
    if host.contains(['*', '?', '[', '%']) {
        return Err(ambiguous());
    }
    if let Some((name, port)) = host.rsplit_once(':')
        && name
            .strip_suffix('.')
            .unwrap_or(name)
            .eq_ignore_ascii_case("github.com")
        && (port.is_empty() || port.starts_with('0') || !port.bytes().all(|b| b.is_ascii_digit()))
    {
        return Err(ambiguous());
    }
    if host
        .strip_suffix('.')
        .is_some_and(|name| name.eq_ignore_ascii_case("github.com"))
    {
        return Err(ambiguous());
    }
    if !host.eq_ignore_ascii_case("github.com") {
        return Ok(None);
    }
    if path.contains(['%', '?', '#', '\\'])
        || path.contains("//")
        || path.split('/').any(|part| matches!(part, "." | ".."))
    {
        return Err(ambiguous());
    }
    Ok((path == "alabsystems" || path.starts_with("alabsystems/")).then_some(key))
}

/// The environment that carries [`PUBLIC_READS`] to the engine: each as `GIT_CONFIG_KEY_<n>` and `GIT_CONFIG_VALUE_<n>`, numbered after the caller's own `parent_count` entries (its `GIT_CONFIG_COUNT`, which git reads as 0 when empty), so those still apply; the `GIT_CONFIG_COUNT` that covers both; and `GIT_TERMINAL_PROMPT=0`, so a credential no helper gives fails at once instead of waiting on a terminal. Git reads `GIT_CONFIG_PARAMETERS` after numbered entries, so its inherited bytes are preserved and the policy appended as Git-quoted `key=value` arguments there too. This keeps private caller config while ensuring the public policy follows it at both command-config precedences.
/// # Errors
/// `parent_count` is not plain decimal digits, adding the entries would overflow it, or an
/// inherited header scope uses URL syntax [`public_header_key`] cannot prove. Git refuses most
/// malformed counts (`bogus count in GIT_CONFIG_COUNT`); signs and leading blanks are refused
/// too, so no entry is numbered by a guess. More-specific public header keys get an exact empty
/// override after the base policy; global and private header keys retain their values.
fn public_reads(
    parent_count: Option<&std::ffi::OsStr>,
    header_keys: &[String],
    parent_parameters: Option<&std::ffi::OsStr>,
) -> Result<Vec<(String, std::ffi::OsString)>, Refusal> {
    let text = parent_count.map(std::ffi::OsStr::to_string_lossy);
    let base = match text.as_deref() {
        None | Some("") => Some(0),
        Some(n) if n.bytes().all(|b| b.is_ascii_digit()) => n.parse::<usize>().ok(),
        Some(_) => None,
    };
    let mut policy: Vec<(&str, &str)> = PUBLIC_READS.to_vec();
    let mut seen = std::collections::BTreeSet::new();
    for key in header_keys {
        if let Some(key) = public_header_key(key)?
            && seen.insert(key)
        {
            policy.push((key, ""));
        }
    }
    let Some((base, count)) = base.and_then(|base| Some((base, base.checked_add(policy.len())?)))
    else {
        return Err(Refusal::new(
            format!(
                "this environment's GIT_CONFIG_COUNT is {:?}, not a plain count of \
                 GIT_CONFIG_KEY_<n> entries — the engine's dry run reads the public tier through \
                 entries numbered after the caller's own",
                text.as_deref().unwrap_or_default()
            ),
            "unset GIT_CONFIG_COUNT, or set it to the number of GIT_CONFIG_KEY_<n> entries, \
             then run it again",
        ));
    };
    let mut parameters = parent_parameters.unwrap_or_default().to_os_string();
    for (key, value) in &policy {
        if !parameters.is_empty() {
            parameters.push(" ");
        }
        parameters.push("'");
        parameters.push(format!("{key}={value}").replace('\'', "'\\''"));
        parameters.push("'");
    }
    let mut env: Vec<(String, std::ffi::OsString)> = policy
        .into_iter()
        .zip(base..)
        .flat_map(|((key, value), n)| {
            [
                (format!("GIT_CONFIG_KEY_{n}"), key.into()),
                (format!("GIT_CONFIG_VALUE_{n}"), value.into()),
            ]
        })
        .collect();
    env.push(("GIT_CONFIG_COUNT".to_string(), count.to_string().into()));
    env.push(("GIT_CONFIG_PARAMETERS".to_string(), parameters));
    env.push(("GIT_TERMINAL_PROMPT".to_string(), "0".into()));
    Ok(env)
}

fn origin_refresh_refusal(why: String) -> Refusal {
    Refusal::new(
        "cannot refresh origin through agh",
        "check the reported native routing or source-state refusal, then re-run",
    )
    .detail([why])
    .could_not_run()
}

fn scanner_refusal(why: String) -> Refusal {
    let mut lines = why.lines();
    Refusal::new(
        format!(
            "publication scanner preflight: {}; no downstream work started by this operation",
            lines.next().unwrap_or("engine refused")
        ),
        "restore the publication engine's scanner prerequisites and rerun preparation",
    )
    .detail(
        lines
            .filter(|line| !line.is_empty())
            .take(20)
            .map(str::to_owned),
    )
    .could_not_run()
}

fn prerequisite_refusal(why: String) -> Refusal {
    let remedy = if atpkg::prerequisite::freshness_transport(&why).is_some() {
        "retry when the signed channel transport answers; reinstalling the local packages cannot answer which head is current"
    } else {
        crate::managed_prerequisites::REMEDY
    };
    Refusal::new(format!("signed publication prerequisites: {why}"), remedy).could_not_run()
}
/// What the engine reads to stage append-only: a child of the current staging commit, pushed
/// without force. Its GitHub transport refuses the legacy orphan-snapshot replacement (no forced
/// write exists there), and its rule for a release caller that stages is to set this for the
/// whole run and every resume (`AGENTS.md` "The verb chain", `docs/GITHUB_TRANSPORT.md`).
const STAGE_APPEND_ONLY_ENV: &str = "PUBLISH_STAGE_APPEND_ONLY";

fn publication_command(engine: &Path, program: impl AsRef<std::ffi::OsStr>) -> Command {
    let mut cmd = command(program);
    publication_environment(engine, &mut cmd);
    cmd
}

fn publication_environment(engine: &Path, cmd: &mut Command) {
    scrub(cmd);
    cmd.env("PUBLICATION_ENGINE", engine);
    cmd.env(STAGE_APPEND_ONLY_ENV, "1");
    cmd.env("PATH", publication_path());
    cmd.env_remove(scanner_tools::ENV);
}

/// Why the engine's launcher did not start ([`engine_bootstrap`]).
#[cfg(unix)]
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) enum Bootstrap {
    /// The signed package store could not admit the publication prerequisites.
    Prerequisites(String),
    /// Anything else: what the launcher said, or why it could not run.
    Failed(String),
}

#[cfg(unix)]
impl std::fmt::Display for Bootstrap {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Prerequisites(why) | Self::Failed(why) => f.write_str(why),
        }
    }
}

/// Check the engine's launcher and guarded tap contract without starting a stage, using the
/// publication environment every release step runs the engine in ([`publication_command`]).
///
/// # Errors
/// Signed package admission or the engine contract refuses, or the interpreter
/// cannot start, exceeds the one-minute hang detector, or exits unsuccessfully.
#[cfg(unix)]
pub(crate) fn engine_bootstrap(repo: &Path, engine: &Path) -> Result<(), Bootstrap> {
    engine_bootstrap_with(repo, engine, crate::managed_prerequisites::Tools::acquire)
}

#[cfg(unix)]
fn engine_bootstrap_with(
    repo: &Path,
    engine: &Path,
    acquire: crate::managed_prerequisites::Acquire,
) -> Result<(), Bootstrap> {
    let held = acquire(
        crate::managed_prerequisites::PUBLICATION,
        "publication launcher audit",
    )
    .map_err(Bootstrap::Prerequisites)?;
    let mut cmd = held
        .engine_command(&engine.join("bin/pub"))
        .map_err(Bootstrap::Prerequisites)?;
    publication_environment(engine, &mut cmd);
    cmd.arg("--help")
        .current_dir(repo)
        .stdin(Stdio::null())
        .env("PYTHONDONTWRITEBYTECODE", "1");
    held.apply(&mut cmd).map_err(Bootstrap::Prerequisites)?;
    let out = aterm_uds::deadline::output_within(&mut cmd, std::time::Duration::from_secs(60))
        .map_err(|e| {
            Bootstrap::Failed(format!(
                "publication engine {} --help cannot run: {e}",
                engine.display()
            ))
        })?;
    if out.status.success() {
        return tap_bootstrap(repo, engine, &held);
    }
    let stderr = String::from_utf8_lossy(&out.stderr);
    let said = stderr
        .lines()
        .map(str::trim)
        .rfind(|l| !l.is_empty())
        .unwrap_or("");
    Err(Bootstrap::Failed(format!(
        "publication engine {} --help exited unsuccessfully ({}){}; check its launcher's \
         runtime on the publication PATH",
        engine.display(),
        out.status,
        if said.is_empty() {
            String::new()
        } else {
            format!(": {said}")
        }
    )))
}

/// The exact cohort guard is required before the cold release work or claim.
/// Argparse help exits before the canonical helper constructs its GitHub client.
#[cfg(unix)]
fn tap_bootstrap(
    repo: &Path,
    engine: &Path,
    held: &crate::managed_prerequisites::Tools,
) -> Result<(), Bootstrap> {
    let mut command = held
        .engine_command(&engine.join("bin/pub"))
        .map_err(Bootstrap::Prerequisites)?;
    publication_environment(engine, &mut command);
    command
        .args(["tap", "--help"])
        .current_dir(repo)
        .stdin(Stdio::null());
    held.apply(&mut command).map_err(Bootstrap::Prerequisites)?;
    let out = aterm_uds::deadline::output_until_complete_stdout(
        &mut command,
        std::time::Instant::now() + std::time::Duration::from_secs(60),
    )
    .map_err(|why| Bootstrap::Failed(format!("publication tap capability cannot run: {why}")))?;
    if !out.status.success()
        || !out
            .stdout
            .split(u8::is_ascii_whitespace)
            .any(|word| word == b"--expect-version")
    {
        return Err(Bootstrap::Failed(format!(
            "publication engine {} lacks the guarded tap --expect-version contract; use a current engine before starting a release",
            engine.display(),
        )));
    }
    Ok(())
}

/// Where a `verify` the release runs in the clone `repo` builds: the physical target its children
/// are handed and the `--config` naming its build directory, read as that `verify` reads them,
/// from the environment a release child has (the caller's home and the publication PATH, every
/// Cargo layout variable removed, as `Real::child` does) and the clone by its physical path, as
/// verify reads its root; so what the release builds beside its `verify` runs, and the
/// directories its disk claim keeps (`crate::release_disk::own_dirs`), are the ones they choose.
///
/// # Errors
/// Cargo's target configuration cannot be resolved to a concrete selected directory.
pub(crate) fn verify_plan(repo: &Path) -> std::io::Result<aterm_verify::NativeBuildPlan> {
    let env = verify_environment();
    let repo = aterm_verify::physical(repo);
    aterm_verify::NativeBuildPlan::capture(&env, &repo)
}

/// A fixture compiler's actual source/configuration root and a separate stable generated
/// artifact root. Selecting the plan is read-only; its producer prepares it before compiling.
pub(crate) fn verify_plan_for_default_target(
    repo: &Path,
    target_root: &Path,
) -> aterm_verify::NativeBuildPlan {
    aterm_verify::NativeBuildPlan::for_default_target(&verify_environment(), repo, target_root)
}

/// The release compiler policy, with caller Cargo layout overrides excluded as in `Real::child`.
pub(crate) fn verify_environment() -> aterm_verify::EnvSnapshot {
    aterm_verify::EnvSnapshot {
        home: std::env::var_os("HOME")
            .map(PathBuf::from)
            .unwrap_or_default(),
        path: publication_path(),
        ..Default::default()
    }
}

/// The diagnostic verifier inherits explicit Cargo layout variables; its fixture reader must
/// inherit that same layout rather than the production child's scrubbed defaults.
pub(crate) fn diagnostic_verify_layout(
    repo: &Path,
) -> std::io::Result<crate::fixtures::CompileLayout> {
    let env = aterm_verify::EnvSnapshot::capture();
    diagnostic_verify_layout_from_env(repo, &env)
}

fn diagnostic_verify_layout_from_env(
    repo: &Path,
    env: &aterm_verify::EnvSnapshot,
) -> std::io::Result<crate::fixtures::CompileLayout> {
    aterm_verify::NativeBuildPlan::capture(env, repo)
}

/// The remaining publication PATH supplies ordinary tools and the pinned rustup.
/// Signed prerequisites are applied separately, under held build leases immediately
/// before execution; workspace scanner/interpreter caches never authorize them.
fn publication_path() -> std::ffi::OsString {
    let path = std::env::var_os("PATH").unwrap_or_default();
    let path = prepend_path(&atpkg::abrew::prefix::Prefix::resolve().bin(), &path);
    if let Some(tool) = aterm_uds::host::tool("rustup").find()
        && let Some(dir) = tool.parent()
    {
        prepend_path(dir, &path)
    } else {
        path
    }
}

fn prepend_path(dir: &Path, path: &std::ffi::OsStr) -> std::ffi::OsString {
    let dirs =
        std::iter::once(dir.to_path_buf()).chain(std::env::split_paths(path).filter(|d| d != dir));
    std::env::join_paths(dirs).unwrap_or_else(|_| path.to_owned())
}

/// `dir`, then the publication PATH without it.
fn path_with(dir: &Path) -> std::ffi::OsString {
    prepend_path(dir, &publication_path())
}

fn keep_reports(out: &Path, to: &Path) {
    let _ = std::fs::remove_dir_all(to);
    if std::fs::create_dir_all(to).is_err() {
        return;
    }
    for entry in std::fs::read_dir(out).into_iter().flatten().flatten() {
        if entry.file_type().is_ok_and(|t| t.is_file()) {
            let name = entry.file_name();
            let _ = crate::durable::copy_regular_fresh(out.join(&name), to.join(&name));
        }
    }
}

fn gitleaks_report(guard: &str) -> Option<&str> {
    guard
        .strip_prefix("gitleaks findings (see ")?
        .strip_suffix(')')
}

/// Up to six gitleaks findings as `<path in the export>:<line> <rule>` — never the matched text or the secret.
#[must_use]
pub fn gitleaks_locations(report: &str, export: &Path) -> Vec<String> {
    let Ok(aterm_json::Value::Array(findings)) = aterm_json::from_str(report) else {
        return Vec::new();
    };
    findings
        .iter()
        .take(6)
        .map(|f| {
            let file = f
                .get("File")
                .and_then(aterm_json::Value::as_str)
                .unwrap_or("?");
            let file = Path::new(file)
                .strip_prefix(export)
                .map_or_else(|_| file.to_string(), |p| p.display().to_string());
            let line = f
                .get("StartLine")
                .and_then(aterm_json::Value::as_u64)
                .unwrap_or(0);
            let rule = f
                .get("RuleID")
                .and_then(aterm_json::Value::as_str)
                .unwrap_or("?");
            format!("{file}:{line} {rule}")
        })
        .collect()
}

/// What the engine's dry run printed, read in the engine's own words: `== guard ok: …` per passing guard, `== GUARD FAIL: …` per failing one (its indented lines its locations), `FAIL: …` per refusal.
#[derive(Debug, Default, Clone, PartialEq, Eq)]
pub struct EngineRun {
    pub code: i32,
    pub passed: Vec<String>,
    /// Each failing guard and up to six of its location lines.
    pub failed: Vec<(String, Vec<String>)>,
    pub refusals: Vec<String>,
    /// The run's last three lines, for a run that said nothing else.
    pub tail: Vec<String>,
}

impl EngineRun {
    /// The run that exited `code` and printed `text`.
    #[must_use]
    pub fn read(code: i32, text: &str) -> Self {
        let lines: Vec<&str> = text.lines().collect();
        let mut run = Self {
            code,
            tail: lines[lines.len().saturating_sub(3)..]
                .iter()
                .map(|l| (*l).to_string())
                .collect(),
            ..Self::default()
        };
        for (i, line) in lines.iter().enumerate() {
            if let Some(what) = line.strip_prefix("== guard ok: ") {
                run.passed.push(what.to_string());
            } else if let Some(what) = line.strip_prefix("== GUARD FAIL: ") {
                let at = lines[i + 1..]
                    .iter()
                    .take_while(|d| d.starts_with("  "))
                    .take(6)
                    .map(|d| d.trim().to_string())
                    .collect();
                run.failed.push((what.to_string(), at));
            } else if let Some(why) = line.strip_prefix("FAIL: ") {
                run.refusals.push(why.to_string());
            }
        }
        run
    }
}

/// Why the engine refused, read from its `FAIL:` line: the commit's tree, or the machine's — something that decided nothing about the tree.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum EngineCause {
    /// The tree's own (its manifest, its config, its transforms, its version): a refusal of the commit.
    Tree,
    /// gitleaks is not on PATH: the engine stops at that fail-closed guard.
    NoGitleaks,
    /// gitleaks crashed or timed out.
    GitleaksBroke,
    /// A remote (the origins, the staging repository, the public tags) could not be read, or moved while it was.
    Network,
    /// agh, the engine's GitHub transport, did not answer a read: no token it routes for the
    /// owner can read the repository, an anonymous read found it private, or the agh is not one
    /// the engine can use.
    Transport,
    /// The engine's own checkout, records or lock.
    Engine,
    /// The commit is not on its origin's main, or the source clone is not one the engine stages.
    Source,
}

const MACHINE_REFUSALS: [(&str, EngineCause); 19] = [
    ("GitHub transport", EngineCause::Transport),
    ("Agh ", EngineCause::Transport),
    ("gitleaks not installed", EngineCause::NoGitleaks),
    ("gitleaks crashed", EngineCause::GitleaksBroke),
    ("release notes: cannot ", EngineCause::Network),
    ("cannot query ", EngineCause::Network),
    ("cannot fetch ", EngineCause::Network),
    (" fetch failed", EngineCause::Network),
    ("fetch for pin failed", EngineCause::Network),
    ("while fetching", EngineCause::Network),
    ("publication engine ", EngineCause::Engine),
    ("another publication operation", EngineCause::Engine),
    ("mappings.json", EngineCause::Engine),
    ("mapping history", EngineCause::Engine),
    ("append-only violation", EngineCause::Engine),
    ("--commit ", EngineCause::Source),
    ("--repo-path", EngineCause::Source),
    ("source override", EngineCause::Source),
    ("source origin", EngineCause::Source),
];

impl EngineCause {
    /// The cause of the engine's refusal `line` (its `FAIL:` line, without the prefix).
    #[must_use]
    pub fn of(line: &str) -> Self {
        MACHINE_REFUSALS
            .iter()
            .find(|(words, _)| line.contains(words))
            .map_or(Self::Tree, |(_, cause)| *cause)
    }

    fn remedy(self) -> String {
        match self {
            Self::Tree => "fix the source the refusal names (never the export)".into(),
            Self::NoGitleaks => aterm_uds::host::tool("gitleaks").remedy(),
            Self::GitleaksBroke => {
                "run `gitleaks version` to see why it cannot scan, then run it again".into()
            }
            Self::Network => "check the network and access to the remote it names (ssh for a \
                              git@ remote; an alabsystems one is read anonymously over HTTPS, \
                              so it must be public), then run it again"
                .into(),
            Self::Transport => format!(
                "the engine reads GitHub through agh: an agh that lacks the verb the engine \
                 runs (`no help for: repo fetch`, `unknown topic 'repo'`) is older than the \
                 engine, so unset {ENGINE_AGH_ENV} (this checkout's agh is then built) or point \
                 it at a current one; else `agh whoami --for OWNER/REPO` names the identity agh \
                 routes for the repository it names, and a private one reads only with a vault \
                 token that can read it (`agh auth add`, or `aterm secrets sync`); an \
                 alabsystems one is read anonymously, so it must be public; then run it again"
            ),
            Self::Engine => {
                "fix the publication engine's checkout or records it names, then run it again"
                    .into()
            }
            Self::Source => "push the commit to origin/main (the engine stages only mainline \
                             commits), or fix the source clone it names, then run it again"
                .into(),
        }
    }
}

/// The export guards' [`Outcome`] from the engine's dry run of `r` (`toolchain`: why the cargo-metadata guard cannot run here, if it cannot; `log`: the whole run; `reports`: the engine's report files).
#[must_use]
pub fn export_outcome(
    run: &EngineRun,
    r: &str,
    toolchain: Option<&Refusal>,
    log: &Path,
    reports: &Path,
) -> Outcome {
    let whole = format!("the whole run: {}", log.display());
    let heard = (!run.passed.is_empty())
        .then(|| format!("the guards that passed first: {}", run.passed.join("; ")));
    let machine_guard =
        |name: &str| toolchain.is_some() && name.starts_with("targo metadata failed");
    let findings: Vec<&(String, Vec<String>)> = run
        .failed
        .iter()
        .filter(|(name, _)| !machine_guard(name))
        .collect();
    let refusals: Vec<(&str, EngineCause)> = run
        .refusals
        .iter()
        .map(String::as_str)
        .filter(|l| !l.starts_with("guards failed"))
        .map(|l| (l, EngineCause::of(l)))
        .collect();
    let tree: Vec<&str> = refusals
        .iter()
        .filter(|(_, cause)| *cause == EngineCause::Tree)
        .map(|(line, _)| *line)
        .collect();
    let first_tree = tree
        .iter()
        .find(|l| **l != "transforms failed")
        .or(tree.first())
        .copied();
    let machine = refusals
        .iter()
        .find(|(_, cause)| *cause != EngineCause::Tree)
        .copied();
    if !findings.is_empty() || first_tree.is_some() {
        let why = if findings.is_empty() {
            format!(
                "the engine refuses to stage {}: {}",
                short(r),
                first_tree.unwrap_or_default()
            )
        } else {
            let names: Vec<&str> = findings.iter().map(|(name, _)| name.as_str()).collect();
            format!(
                "the engine's export guards refuse {}: {}",
                short(r),
                names.join("; ")
            )
        };
        let guards = findings.iter().flat_map(|(name, at)| {
            std::iter::once(format!("GUARD FAIL: {name}"))
                .chain(at.iter().map(|l| format!("  {l}")))
        });
        let others = tree
            .iter()
            .filter(|l| !findings.is_empty() || Some(**l) != first_tree)
            .take(3)
            .map(|l| format!("FAIL: {l}"));
        let stopped = machine.map(|(line, cause)| {
            format!(
                "then the engine stopped, judging nothing more: {line} (fix: {})",
                cause.remedy()
            )
        });
        return Outcome::Refuse(
            Refusal::new(why, export_remedy::refusal_fix(&findings)).detail(
                guards
                    .chain(others)
                    .chain(stopped)
                    .chain(std::iter::once(format!(
                        "the engine's reports: {}",
                        reports.display()
                    )))
                    .chain(std::iter::once(whole)),
            ),
        );
    }
    if let Some((line, cause)) = machine {
        return Outcome::CouldNotRun(
            Refusal::new(
                format!("the engine could not judge {}: {line}", short(r)),
                cause.remedy(),
            )
            .detail(heard.into_iter().chain(std::iter::once(whole))),
        );
    }
    if let Some(toolchain) = toolchain
        && run.failed.iter().any(|(name, _)| machine_guard(name))
    {
        return Outcome::CouldNotRun(
            toolchain
                .clone()
                .detail(heard.into_iter().chain(std::iter::once(whole))),
        );
    }
    if run.code == 0 && !run.passed.is_empty() {
        return Outcome::Pass(format!(
            "export guards: the engine's dry run of {} passed {} guard(s): {}",
            short(r),
            run.passed.len(),
            run.passed.join("; ")
        ));
    }
    Outcome::CouldNotRun(
        Refusal::new(
            format!(
                "the engine's dry run of {} ended (exit {}) without a guard verdict",
                short(r),
                run.code
            ),
            "read the whole run, and teach this gate the engine's words if they changed",
        )
        .detail(run.tail.iter().cloned().chain(std::iter::once(whole))),
    )
}

/// The wasm32 clause of `publish/config.sh`'s `CHECK_CMD_DEFAULT` and the stock channel it names: one reader, aterm-toolchain's (`aterm_toolchain::rustup::wasm_clause`), which also holds the checkout's stock pin to that channel for the dev lanes and the `verify provision` that acquires it.
pub use aterm_toolchain::rustup::WasmClause;

/// Read the wasm32 clause out of `config` (the engine's own line shape, `CHECK_CMD_DEFAULT="…"`), with every refusal the stage would meet for it (`aterm_toolchain::rustup::wasm_clause`, as a release refusal).
/// # Errors
/// No clause; no stock channel; no target-add before it; no `RUSTC` naming the pin's own rustc.
pub fn wasm_clause(config: &str) -> Result<WasmClause, Refusal> {
    aterm_toolchain::rustup::wasm_clause(config).map_err(|e| Refusal::new(e.why, e.remedy))
}

/// Where step 7 reads the release it judges: the private draft `cut --sign` journaled, by id
/// (GitHub's lookup by tag answers only a published release, so it 404s on every draft), else
/// the published release of `tag`.
fn source_release_path(slug: &str, tag: &str, draft: Option<u64>) -> String {
    match draft {
        Some(id) => format!("repos/{slug}/releases/{id}"),
        None => format!("repos/{slug}/releases/tags/{tag}"),
    }
}

/// Step 7's pending state where no private draft id is journaled and GitHub's lookup of `tag`
/// answers 404: no release, or a draft (that lookup serves none). The sign makes or adopts the
/// draft and journals its id; the source signing then attaches R's pair to it.
pub(crate) fn unjournaled_source(tag: &str) -> String {
    format!(
        "no private draft of {tag} is journaled, and GitHub serves no published release of it \
         (its lookup by tag never serves a draft)"
    )
}

/// Step 7's judgment of the release it read for R's source publication (`version` at `repo`): the
/// private draft `draft` the clone journals, read by its id, else R's published release, read by
/// its tag. Attested when the pair stands; pending when the journaled draft lacks only whole
/// assets of it (the source signing attaches them); else refused, with the sign-release form
/// that reaches that release.
pub(crate) fn judge_source_release(
    release: &aterm_json::Value,
    slug: &str,
    version: &str,
    repo: &Path,
    draft: Option<u64>,
) -> Result<SourceState, Refusal> {
    let tag = format!("v{version}");
    if let Some((why, fix)) = source_binding_problem(release, slug, &tag, draft) {
        return Err(Refusal::new(
            why,
            format!("{fix}, then {RELEASE_COMMAND} (nothing was claimed)"),
        ));
    }
    let findings = source_attestation_findings(release);
    if findings.is_empty() {
        return Ok(SourceState::Attested);
    }
    if let Some(id) = draft
        && findings
            .iter()
            .all(|f| matches!(f, crate::channel::AttestationFinding::Missing(_)))
    {
        return Ok(SourceState::Pending(format!(
            "private draft {id} lacks its source attestation: {}",
            crate::channel::attestation_words(slug, &findings)
        )));
    }
    Err(Refusal::new(
        format!(
            "the source publication of {tag} is incomplete: {}",
            crate::channel::attestation_words(slug, &findings)
        ),
        format!(
            "finish the source signing ({}), then {RELEASE_COMMAND} (nothing was claimed)",
            crate::publish::sign_release_command(
                repo,
                version,
                draft
                    .map(|_| crate::publish::draft_journal_path(repo))
                    .as_deref()
            )
        ),
    ))
}

/// Why the release read for `tag` is not the one this cut publishes, and its remedy: another
/// release than the journaled draft, or a draft GitHub no longer binds to `tag` (it names one
/// `untagged-…` after an edit that names no tag).
fn source_binding_problem(
    release: &aterm_json::Value,
    slug: &str,
    tag: &str,
    draft: Option<u64>,
) -> Option<(String, String)> {
    let id = release.get("id").and_then(aterm_json::Value::as_u64);
    if let Some(want) = draft
        && id != Some(want)
    {
        let read = id.map_or_else(|| "no id".to_string(), |id| id.to_string());
        return Some((
            format!(
                "{slug}'s release read for {tag} is {read}, not the private draft {want} dist/draft-state.toml journals"
            ),
            format!(
                "withdraw the journaled draft with its reservation ({RELEASE_COMMAND} --forget), \
                 then {RELEASE_COMMAND}"
            ),
        ));
    }
    let bound = release
        .get("tag_name")
        .and_then(aterm_json::Value::as_str)
        .unwrap_or("");
    if bound == tag {
        return None;
    }
    let id = id.map_or_else(|| "<id>".to_string(), |id| id.to_string());
    Some((
        format!(
            "{slug} release {id} is bound to `{bound}`, not {tag}: GitHub drops a draft's tag when an edit names none"
        ),
        format!("bind it back (agh api repos/{slug}/releases/{id} -X PATCH -f tag_name={tag})"),
    ))
}

/// The lean-download shape of a published release on `slug`, from what GitHub serves (its release JSON, never the files the cut wrote): the problems, empty when it carries the shape.
#[must_use]
pub fn release_shape(release: &aterm_json::Value, slug: &str, version: &str) -> Vec<String> {
    const MAX_DMG_BYTES: u64 = 200 * 1000 * 1000;
    let mut assets: std::collections::BTreeMap<&str, Option<u64>> = Default::default();
    for a in release
        .get("assets")
        .and_then(aterm_json::Value::as_array)
        .into_iter()
        .flatten()
    {
        let name = a
            .get("name")
            .and_then(aterm_json::Value::as_str)
            .unwrap_or("");
        assets.insert(name, a.get("size").and_then(aterm_json::Value::as_u64));
    }
    let mut bad = Vec::new();
    let dmg = format!("aterm-{version}.dmg");
    let zip = format!("aterm-{version}-mac.zip");
    match assets.get(dmg.as_str()) {
        None => bad.push(format!(
            "{dmg} is missing — the download the site links and the updater binds"
        )),
        Some(None) => bad.push(format!(
            "{dmg} carries no size in the release JSON — cannot prove it is lean"
        )),
        Some(Some(n)) if *n > MAX_DMG_BYTES => bad.push(format!(
            "{dmg} is {n} bytes — over the 200 MB lean ceiling: a seeded image"
        )),
        Some(Some(_)) => {}
    }
    if !assets.contains_key(zip.as_str()) {
        bad.push(format!(
            "{zip} is missing — the updater, the Homebrew cask and install.sh all consume it"
        ));
    }
    for (twin, source) in [("aterm.dmg", &dmg), ("aterm-mac.zip", &zip)] {
        match (assets.get(twin), assets.get(source.as_str())) {
            (None, _) => bad.push(format!(
                "{twin} is missing — the permanent releases/latest/download name"
            )),
            (Some(Some(a)), Some(Some(b))) if a != b => bad.push(format!(
                "{twin} is {a} bytes but {source} is {b} — a twin must be a byte copy of the \
                 container it aliases"
            )),
            _ => {}
        }
    }
    for name in [dmg.as_str(), zip.as_str(), "aterm.dmg", "aterm-mac.zip"] {
        if !assets
            .keys()
            .any(|k| k.strip_prefix(name).is_some_and(|s| s == ".sha256"))
        {
            bad.push(format!(
                "{name}.sha256 is missing — `shasum -a 256 -c` matches on the embedded name, so each \
                 container and twin needs its own sidecar"
            ));
        }
    }
    if !assets.contains_key("aterm-appcast.toml") {
        bad.push(
            "aterm-appcast.toml is missing — installed copies read it to find this release".into(),
        );
    }
    bad.extend(
        source_attestation_findings(release)
            .iter()
            .map(|finding| finding.words(slug)),
    );
    let retired: Vec<&str> = assets
        .keys()
        .copied()
        .filter(|n| {
            n.ends_with("-x86_64.dmg") || n.ends_with("-lite.dmg") || *n == "aterm-offline.dmg"
        })
        .collect();
    if !retired.is_empty() {
        bad.push(format!(
            "retired macOS asset(s) present: {} — there is exactly ONE macOS DMG",
            retired.join(", ")
        ));
    }
    let body = release
        .get("body")
        .and_then(aterm_json::Value::as_str)
        .unwrap_or("");
    let said: Vec<&str> = [
        "batteries-included",
        "batteries included",
        "offline alab toolchain seed",
        "toolchain seed",
        "air-gapped",
    ]
    .into_iter()
    .filter(|w| {
        body.as_bytes()
            .windows(w.len())
            .any(|win| win.eq_ignore_ascii_case(w.as_bytes()))
    })
    .collect();
    if !said.is_empty() {
        bad.push(format!(
            "the release body still describes a seeded download ({}) — the lean template says \
             the toolchain installs itself on first launch",
            said.join(", ")
        ));
    }
    bad.extend(linux_inventory(&assets, version));
    bad
}

fn linux_inventory(
    assets: &std::collections::BTreeMap<&str, Option<u64>>,
    version: &str,
) -> Vec<String> {
    const ARCHES: [&str; 2] = ["x86_64", "aarch64"];
    let mut bad = Vec::new();
    let mut native = Vec::new();
    let (mut tars, mut sides) = (Vec::new(), Vec::new());
    for (&name, size) in assets {
        let Some(rest) = name.strip_prefix("aterm-") else {
            continue;
        };
        let Some((named, tail)) = rest.split_once("-linux-") else {
            continue;
        };
        let (arch, kind) = if let Some(a) = tail.strip_suffix(".tar.gz.sha256") {
            (a, "side")
        } else if let Some(a) = tail.strip_suffix(".tar.gz") {
            (a, "tar")
        } else {
            (tail, "native")
        };
        if named != version {
            bad.push(format!(
                "Linux asset {name} names another version — this release is {version}"
            ));
            continue;
        };
        if !ARCHES.contains(&arch) {
            bad.push(format!(
                "Linux asset {name} is named for an unsupported or noncanonical architecture — \
                 closed table (x86_64, aarch64)"
            ));
            continue;
        }
        match kind {
            "side" => sides.push(arch),
            "tar" => tars.push(arch),
            _ => {
                native.push(arch);
                if !size.is_some_and(|n| (64..=512 * 1024 * 1024).contains(&n)) {
                    bad.push(format!(
                        "{name} has invalid native ELF size {size:?} (expected 64..536870912 bytes)"
                    ));
                }
            }
        }
    }
    for arch in tars.iter().filter(|a| !sides.contains(a)) {
        bad.push(format!(
            "aterm-{version}-linux-{arch}.tar.gz.sha256 is missing — the historical manual \
             download requires its transport checksum"
        ));
    }
    for arch in sides.iter().filter(|a| !tars.contains(a)) {
        bad.push(format!(
            "aterm-{version}-linux-{arch}.tar.gz.sha256 is present but its tarball is not — the \
             historical manual-download pair is incomplete"
        ));
    }
    if !native.is_empty() {
        for proof in [
            "aterm-appcast.toml",
            "aterm-appcast.toml.sig",
            "aterm-machines.toml",
            "aterm-machines.toml.sig",
        ] {
            if !assets.get(proof).is_some_and(|s| s.is_some_and(|n| n > 0)) {
                bad.push(format!(
                    "Linux native proof object {proof} is missing or empty — all four \
                     authenticated appcast/roster objects are required"
                ));
            }
        }
    }
    bad
}

fn pointer_verdict(read: Result<Vec<String>, String>) -> Result<Vec<String>, Refusal> {
    read.map_err(|why| {
        Refusal::new(
            format!("{why}, so the release pointer's shape is unknown"),
            format!("{RELEASE_COMMAND} again when GitHub answers"),
        )
        .could_not_run()
    })
}

/// The page and every published asset share one propagation budget, including the appcast and
/// permanent lean-download twins. A tag URL fetched after this gate is no longer a latest read.
fn latest_pointer_urls(
    slug: &str,
    tag: &str,
    release: &aterm_json::Value,
) -> Vec<(String, String)> {
    let base = format!("https://github.com/{slug}/releases");
    let assets: std::collections::BTreeSet<&str> = release
        .get("assets")
        .and_then(aterm_json::Value::as_array)
        .into_iter()
        .flatten()
        .filter_map(|asset| asset.get("name").and_then(aterm_json::Value::as_str))
        .chain(["aterm-appcast.toml", "aterm.dmg", "aterm-mac.zip"])
        .collect();
    std::iter::once((format!("{base}/latest"), format!("{base}/tag/{tag}")))
        .chain(assets.into_iter().map(|asset| {
            (
                format!("{base}/latest/download/{asset}"),
                format!("{base}/download/{tag}/{asset}"),
            )
        }))
        .collect()
}

/// Latest redirects alone cannot prove supersession. Only a higher canonical version in the
/// releases list, published and neither draft nor prerelease, can supply that refusal.
fn newer_published_tag(list: &aterm_json::Value, tag: &str) -> Result<Option<String>, String> {
    let cut = crate::site::releases::version_key(tag)
        .ok_or_else(|| format!("the cut's tag {tag} is not a canonical release version"))?;
    let releases = list
        .as_array()
        .ok_or_else(|| "GitHub's release list is not an array".to_string())?;
    Ok(releases
        .iter()
        .filter(|release| {
            release.get("draft").and_then(aterm_json::Value::as_bool) == Some(false)
                && release
                    .get("prerelease")
                    .and_then(aterm_json::Value::as_bool)
                    == Some(false)
                && release
                    .get("published_at")
                    .and_then(aterm_json::Value::as_str)
                    .is_some_and(|date| !date.is_empty())
        })
        .filter_map(|release| release.get("tag_name").and_then(aterm_json::Value::as_str))
        .filter_map(|tag| crate::site::releases::version_key(tag).map(|key| (key, tag)))
        .filter(|(key, _)| *key > cut)
        .max_by_key(|(key, _)| *key)
        .map(|(_, tag)| tag.to_string()))
}

type PointerHop = (u16, Option<String>);

/// All after-cut latest reads use the same 30-probe bound. Re-read only pending surfaces, and
/// re-read the release list each round so a real successor ends the wait immediately. A stale
/// 302 or 404 at the bound is unfinished propagation (retryable), never a release-shape defect.
fn settled_pointer_problems(
    pointers: &[(String, String)],
    tag: &str,
    read: &mut dyn FnMut(&str) -> Result<PointerHop, String>,
    releases: &mut dyn FnMut() -> Result<aterm_json::Value, String>,
    sleep: &mut dyn FnMut(std::time::Duration),
) -> Result<Vec<String>, String> {
    let mut pending: Vec<_> = pointers.iter().map(|(ptr, want)| (ptr, want)).collect();
    for attempt in 1..=crate::publish::POINTER_PROBE_ATTEMPTS {
        if let Some(newer) = newer_published_tag(&releases()?, tag)? {
            return Ok(vec![format!(
                "another release holds GitHub's latest: the releases list proves {newer} \
                 is a newer published non-prerelease than {tag}; the cut owns it: while \
                 {tag}'s cut is open, `{RELEASE_COMMAND}` resumes it and re-sends its head \
                 PATCH (never re-point it by hand)"
            )]);
        }
        let mut stale = Vec::new();
        for (ptr, want) in pending {
            let (code, location) = read(ptr)?;
            match check_pointer_hop(ptr, want, tag, code, location.as_deref()) {
                Ok(()) => {}
                Err(why) if matches!(code, 302 | 404) => stale.push(((ptr, want), why)),
                Err(why) => return Err(why),
            }
        }
        if stale.is_empty() {
            return Ok(Vec::new());
        }
        if attempt == crate::publish::POINTER_PROBE_ATTEMPTS {
            return Err(stale
                .into_iter()
                .map(|(_, why)| why)
                .collect::<Vec<_>>()
                .join("; "));
        }
        sleep(crate::publish::POINTER_PROBE_DELAY);
        pending = stale.into_iter().map(|(pointer, _)| pointer).collect();
    }
    unreachable!("the propagation bound is nonzero")
}

fn check_pointer_hop(
    ptr: &str,
    want: &str,
    tag: &str,
    code: u16,
    location: Option<&str>,
) -> Result<(), String> {
    match code {
        302 if location == Some(want) => Ok(()),
        302 | 404 => Err(format!(
            "{ptr} answers HTTP {code} to {}, not {want} — GitHub's latest propagation did \
             not finish within {} probes; the releases list proves no newer published \
             non-prerelease than {tag}",
            location.unwrap_or("<no Location>"),
            crate::publish::POINTER_PROBE_ATTEMPTS,
        )),
        code => Err(format!(
            "{ptr} answers HTTP {code}, not a release pointer's 302 or 404"
        )),
    }
}

/// The appcast served at the pointer's target: it names `version`, and its `url` is exactly the tag-specific download URL of its own `dmg` (the credential-less updater refuses any other).
#[must_use]
pub fn appcast_problems(text: &str, slug: &str, tag: &str, version: &str) -> Vec<String> {
    let field = |key: &str| {
        text.lines().find_map(|l| {
            l.strip_prefix(key)?
                .strip_prefix(" = \"")?
                .strip_suffix('"')
        })
    };
    if field("version") != Some(version) {
        return vec![format!(
            "the appcast served for {tag} does not name version {version}"
        )];
    }
    let dmg = field("dmg").unwrap_or_default();
    let want = format!("https://github.com/{slug}/releases/download/{tag}/{dmg}");
    match field("url") {
        Some(url) if !dmg.is_empty() && url == want => Vec::new(),
        url => vec![format!(
            "the appcast's url is '{}', not {want} — the credential-less updater refuses a \
             manifest whose url does not bind it to its tag and container",
            url.unwrap_or("<absent>")
        )],
    }
}

fn hms_local(epoch: u64, offset_at: impl FnOnce(i64) -> Option<i64>) -> String {
    let unix = i64::try_from(epoch).unwrap_or(i64::MAX);
    let offset = offset_at(unix).unwrap_or(0);
    let local = unix.saturating_add(offset).rem_euclid(86_400);
    format!(
        "{:02}:{:02}:{:02}",
        local / 3600,
        local % 3600 / 60,
        local % 60
    )
}

#[derive(Debug)]
enum SupersedeCheck {
    Present(String),
    Unavailable(Refusal),
}

const SUPERSEDE_REASON: &str =
    "explicit --supersede: the unpublished line cannot finish with its own tooling";

fn supersession_record(previous: Line, replacement: Line) -> crate::line::Supersession {
    crate::line::Supersession {
        previous,
        replacement,
        reason: SUPERSEDE_REASON.into(),
    }
}

/// Discard a retirement that established the requested previous line: it says nothing
/// about whether this request retired that line in turn.
fn retirement_after_previous(
    requested: &crate::line::Supersession,
    reread: Result<Option<crate::line::Supersession>, String>,
) -> Result<Option<crate::line::Supersession>, String> {
    reread.map(|record| record.filter(|record| record.replacement.base != requested.previous.base))
}

/// Classify one authoritative reread by the complete durable record. A competing record carries
/// its replacement so the clone can converge immediately rather than restoring a retired line.
fn classify_supersession_reread(
    requested: &crate::line::Supersession,
    reread: Result<Option<crate::line::Supersession>, String>,
    retry_absent: bool,
) -> SupersedeResult {
    match retirement_after_previous(requested, reread) {
        Ok(Some(record)) if record == *requested => {
            SupersedeResult::Committed("the exact earlier supersession is durable".into())
        }
        Ok(Some(record)) => SupersedeResult::Refused {
            why: "a different durable supersession won".into(),
            retry: false,
            winner: Some(record.replacement),
        },
        Ok(None) => SupersedeResult::Refused {
            why: "an authoritative source read proved the earlier write absent".into(),
            retry: retry_absent,
            winner: None,
        },
        Err(why) => SupersedeResult::Unknown(why),
    }
}

/// A stopped claim is not evidence that its source write is absent: another cutter may have
/// adopted an aged claim and committed a supersession before the old holder observed the loss.
/// Preserve the claim's refusal when the source names no retirement of the requested previous line.
fn classify_claim_stop(
    requested: &crate::line::Supersession,
    why: String,
    reread: Result<Option<crate::line::Supersession>, String>,
) -> SupersedeResult {
    match retirement_after_previous(requested, reread) {
        Ok(None) => SupersedeResult::Refused {
            why,
            retry: false,
            winner: None,
        },
        reread => classify_supersession_reread(requested, reread, false),
    }
}

/// Bind the public-surface scan to the exact source reread required after claim acquisition.
/// Keeping this transition in one resolver prevents the scan's stale answer from becoming a
/// rollback verdict on its own.
fn settle_post_claim_public_state(
    scanned: Result<(), SupersedeCheck>,
    previous: &Line,
    classify: impl FnOnce(String) -> SupersedeResult,
) -> Result<(), SupersedeResult> {
    scanned.map_err(|error| {
        let refusal = error.refusal(Some(previous));
        classify(format!("{} — {}", refusal.why, refusal.remedy))
    })
}

fn staged_ahead_of_version(staged: &str, requested: &str) -> Result<bool, SupersedeCheck> {
    let numbers = |version: &str| {
        version
            .split('.')
            .map(str::parse::<u64>)
            .collect::<Result<Vec<_>, _>>()
            .map_err(|error| {
                SupersedeCheck::Unavailable(Refusal::new(
                    error.to_string(),
                    format!("retry the same {RELEASE_COMMAND} --commit ... --supersede command"),
                ))
            })
    };
    Ok(numbers(staged)? > numbers(requested)?)
}

impl SupersedeCheck {
    fn refusal(self, previous: Option<&Line>) -> Refusal {
        match self {
            Self::Present(why) => Refusal::new(
                why,
                previous.map_or_else(
                    || {
                        "resume the existing line, then carry its repair with ship release --fix"
                            .into()
                    },
                    |line| crate::release::fix_remedy(line.head()),
                ),
            ),
            Self::Unavailable(refusal) => refusal,
        }
    }
}

impl From<String> for SupersedeCheck {
    fn from(why: String) -> Self {
        Self::Unavailable(
            Refusal::new(
                why,
                format!("retry the same {RELEASE_COMMAND} --commit ... --supersede command"),
            )
            .could_not_run(),
        )
    }
}

fn line_commits(line: &Line) -> impl Iterator<Item = &str> {
    std::iter::once(line.base.as_str()).chain(line.hops.iter().map(|hop| hop.commit.as_str()))
}

impl Real {
    /// Check every staging destination any tree on the retired line declared. A hop may change
    /// publication policy before qualify stages it, so neither the base nor head alone bounds
    /// this append-only surface. The same version can be appended again after retirement.
    /// A later version still prevents staging from going backwards.
    fn supersede_staging_state(
        &mut self,
        previous: &Line,
        line: &Line,
    ) -> Result<(), SupersedeCheck> {
        let mut repositories = BTreeSet::new();
        for commit in line_commits(previous) {
            let config = self
                .git_out(&["show", &format!("{commit}:publish/config.sh")])
                .ok_or_else(|| {
                    SupersedeCheck::from(format!(
                        "cannot read {commit}'s staging remote declaration"
                    ))
                })?;
            let staging = config
                .lines()
                .find_map(|row| {
                    row.strip_prefix("STAGING_REMOTE_DEFAULT=\"")
                        .and_then(|value| value.strip_suffix('"'))
                })
                .ok_or_else(|| {
                    SupersedeCheck::from(format!("{commit} has no staging remote declaration"))
                })?;
            repositories.insert(
                crate::publish::github_slug_from_remote_url(staging)
                    .map_err(|error| SupersedeCheck::from(error.to_string()))?,
            );
        }
        for staging in repositories {
            let staged = (self.staging_file)(&staging, "main", "Cargo.toml")
                .map_err(SupersedeCheck::from)?;
            let staged = std::str::from_utf8(&staged)
                .map_err(|error| SupersedeCheck::from(error.to_string()))?;
            let version = crate::publish::workspace_version(staged)
                .map_err(|error| SupersedeCheck::from(error.to_string()))?;
            if staged_ahead_of_version(&version, &line.version)? {
                return Err(SupersedeCheck::Present(format!(
                    "staging has advanced to v{version}, beyond v{}",
                    line.version
                )));
            }
        }
        Ok(())
    }

    /// Check every public release channel any tree on the retired line declared. Qualify and a
    /// hop may have addressed different repositories before the line is retired.
    fn supersede_release_state(&self, previous: &Line, tag: &str) -> Result<(), SupersedeCheck> {
        let mut channels = BTreeSet::new();
        for commit in line_commits(previous) {
            let manifest = self
                .git_out(&["show", &format!("{commit}:Cargo.toml")])
                .ok_or_else(|| {
                    SupersedeCheck::from(format!("cannot read {commit}'s release channel"))
                })?;
            channels.insert(
                crate::publish::channel_slug(&manifest)
                    .map_err(|error| SupersedeCheck::from(error.to_string()))?,
            );
        }
        for channel in channels {
            let released = (self.release_exists)(&channel, tag).map_err(SupersedeCheck::from)?;
            crate::line::require_unpublished([false, false, false, released])
                .map_err(SupersedeCheck::Present)?;
        }
        Ok(())
    }

    /// A source record is authoritative only if its replacement still verifies as a release
    /// line in this fetched repository. Never hand an unproved winner to local HEAD or journals.
    fn verify_supersede_result(&self, result: SupersedeResult) -> SupersedeResult {
        let SupersedeResult::Refused {
            why,
            retry,
            winner: Some(winner),
        } = result
        else {
            return result;
        };
        match crate::line::verify(&self.git(), &winner) {
            Ok(_) => SupersedeResult::Refused {
                why,
                retry,
                winner: Some(winner),
            },
            Err(error) => SupersedeResult::Unknown(format!(
                "the durable competing supersession names an invalid replacement: {error}"
            )),
        }
    }

    /// Resolve every stop after claim acquisition against source before the caller may restore
    /// the retired local line. A slow public-surface scan can outlive an adopted claim too.
    fn classify_after_claim_stop(
        &mut self,
        requested: &crate::line::Supersession,
        why: String,
    ) -> SupersedeResult {
        self.prefetched_origin = None;
        if let Err(refusal) = self.prefetch_source() {
            return SupersedeResult::Unknown(format!(
                "{why}; exact supersession reread unavailable: {} — {}",
                refusal.why, refusal.remedy
            ));
        }
        let reread = crate::line::supersession(&self.git(), &requested.replacement.version);
        self.verify_supersede_result(classify_claim_stop(requested, why, reread))
    }

    fn supersede_public_state_after_staging(
        &mut self,
        previous: &Line,
        line: &Line,
    ) -> Result<(), SupersedeCheck> {
        if self.origin_main.is_none() {
            self.fetch_engine_origin()
                .map_err(SupersedeCheck::Unavailable)?;
        }
        // Retirement must also recover a hop whose content proof fails, such as one an older,
        // tip-bound cutter admitted. supersede verifies the replacement and the old head's
        // ancestry, never the old line. Publication
        // checks below still cover every commit of the retired line.
        let ledger = self
            .git_out(&["show", "origin/main:RELEASES.ledger"])
            .ok_or_else(|| {
                SupersedeCheck::from("cannot read origin/main's release ledger".to_string())
            })?;
        let mappings = self
            .engine_text("mappings.json")
            .ok_or_else(|| SupersedeCheck::from("cannot read publication mappings".to_string()))?;
        let _: aterm_json::Value = aterm_json::from_str(&mappings).map_err(|error| {
            SupersedeCheck::from(format!("invalid publication mappings: {error}"))
        })?;
        let tag = format!("v{}", line.version);
        let rows = self.engine_rows(Some(previous.head()));
        let published = rows.has
            || rows
                .newest
                .as_deref()
                .is_some_and(|sha| self.version_at(sha).as_deref() == Some(line.version.as_str()))
            || std::iter::once(previous.base.as_str())
                .chain(previous.hops.iter().map(|hop| hop.commit.as_str()))
                .any(|commit| self.engine_rows(Some(commit)).has);
        let tagged = self
            .origin_tag_now(&tag)
            .map_err(SupersedeCheck::from)?
            .is_some();
        let ledgered = ledger
            .lines()
            .any(|row| row.split_whitespace().nth(1) == Some(line.version.as_str()))
            || self
                .cut_journal()
                .is_some_and(|cut| cut.version == line.version);
        crate::line::require_unpublished([tagged, ledgered, published, false])
            .map_err(SupersedeCheck::Present)?;
        self.supersede_release_state(previous, &tag)
    }

    fn supersede_public_state(
        &mut self,
        previous: &Line,
        line: &Line,
    ) -> Result<(), SupersedeCheck> {
        self.supersede_staging_state(previous, line)?;
        self.supersede_public_state_after_staging(previous, line)
    }
    /// The head immediately before `commit` on this release's line. A base has none: content
    /// verdicts cross only a fix hop, never into a newly selected line.
    fn previous_line_head(&self, commit: &str) -> Option<String> {
        let journal =
            crate::release::Journal::load(&self.release.join(crate::release::JOURNAL_FILE))
                .ok()
                .flatten()?;
        let line = journal.line();
        let at = line.hops.iter().position(|hop| hop.commit == commit)?;
        Some(if at == 0 {
            line.base
        } else {
            line.hops[at - 1].commit.clone()
        })
    }

    /// The finalized verifier command context used both before and inside qualify. The actual
    /// child receives the same prerequisite mutation once in `spawn_stoppable`; this mirror
    /// receives it once before hashing, so evidence and execution cannot diverge at that seam.
    fn check_content_context(
        &self,
        verifier: &Path,
        frozen_tools_sha256: &str,
        seal: &str,
        prover: &Prover,
        half: crate::check_record::Half,
        admission: crate::check_content::Admission,
    ) -> Result<String, String> {
        let mut command = self.verifier_child(verifier, Some(prover), half);
        self.hand_pool(&mut command, true)
            .map_err(|refusal| refusal.why)?;
        self.finalize_spawn_environment(&mut command)?;
        let half = match half {
            crate::check_record::Half::Tree => crate::check_content::ReleaseHalf::Tree,
            crate::check_record::Half::Bundle => crate::check_content::ReleaseHalf::Bundle,
        };
        Ok(crate::check_content::context(
            frozen_tools_sha256,
            seal,
            &command,
            half,
            admission,
        ))
    }

    /// The declared content keys of `commit` under the verifier and build inputs this run uses.
    fn check_content_plan(
        &self,
        commit: &str,
        seal: &str,
        prover: &Prover,
    ) -> Result<crate::check_content::Plan, String> {
        let frozen = crate::release_tools::recorded(&self.release, commit)?;
        let context = self.check_content_context(
            &frozen.verify.path,
            &frozen.set_sha256,
            seal,
            prover,
            crate::check_record::Half::Tree,
            crate::check_content::Admission::WaitDefault,
        )?;
        let appkit_context = self.check_content_context(
            &frozen.verify.path,
            &frozen.set_sha256,
            seal,
            prover,
            crate::check_record::Half::Bundle,
            crate::check_content::Admission::Now,
        )?;
        let git = self.git();
        crate::check_content::plan(
            &git,
            commit,
            &context,
            Some(crate::check_content::AppkitContext {
                context: &appkit_context,
            }),
        )
    }

    /// Materialize equal OK records from the immediately preceding line head. A failure to
    /// establish content evidence only makes checks run; it is never a release verdict.
    fn carry_hop_checks(
        &self,
        commit: &str,
        seal: &str,
        plan: &crate::check_content::Plan,
    ) -> Result<crate::check_record::HopCarry, String> {
        let previous = self
            .previous_line_head(commit)
            .ok_or_else(|| "the candidate is the line base, not a fix hop".to_string())?;
        let git = self.git();
        let root = crate::check_record::root(&git)?;
        let tree = crate::check_record::tree_of(&git, commit)?;
        let previous_tree = crate::check_record::tree_of(&git, &previous)?;
        crate::check_record::carry_hop(&root, commit, &tree, &previous, &previous_tree, seal, plan)
    }
}

impl Real {
    /// Apply exactly the edits owned by a release bump, before either commit writer runs.
    fn edit_bump(
        &mut self,
        from: &str,
        to: &str,
        prover: &Prover,
        origin: &dyn crate::ledger::Origin,
    ) -> Result<(String, Vec<String>), Refusal> {
        let edit = self.engine.join("bin/pub-bump-edit");
        if !edit.is_file() {
            return Err(Refusal::new(
                format!(
                    "the release's engine {} has no bin/pub-bump-edit at its origin's main",
                    self.engine.display()
                ),
                format!(
                    "land bin/pub-bump-edit on the engine's main, then {RELEASE_COMMAND} again \
                     (its sync step brings the release's engine to it)"
                ),
            ));
        }
        let undo = |s: &mut Self| {
            let _ = s.git().git(&["checkout", "-q", "--", "."]);
        };
        let mut cmd = self.scrubbed(&edit);
        cmd.arg(&self.repo).args([to, from]).current_dir(&self.repo);
        let edit_timing =
            crate::cut_timing::Timer::start(&self.step_log(self.step), "bump-version-edit");
        if self.logged(Step::Commit, &mut cmd).code != 0 {
            return Err(Refusal::new(
                "bin/pub-bump-edit could not find the version line",
                "read Cargo.toml's [workspace.package]",
            ));
        }
        drop(edit_timing);
        // A nested workspace (a recorder run read-only in the Linux lane)
        // locks aterm's path crates at the workspace's version too: each
        // moves with the root, or its `--locked` run fails until relocked.
        let nested = self.git_paths(&["ls-files", "-z", "--", "Cargo.lock", "*/Cargo.lock"]);
        let lock_timing =
            crate::cut_timing::Timer::start(&self.step_log(self.step), "bump-lockfiles");
        let followed = match bump_nested_locks(&self.repo, nested, from, to) {
            Ok(followed) => followed,
            Err(why) => {
                undo(self);
                return Err(self.commit_log_refusal(why, false));
            }
        };
        if let Err(why) = self.validate_bumped_locks(prover, &followed) {
            undo(self);
            return Err(self.commit_log_refusal(why, true));
        }
        drop(lock_timing);
        let inspected = [
            &["diff", "--name-only", "-z", "HEAD"][..],
            &["ls-files", "--others", "--exclude-standard", "-z"][..],
        ]
        .iter()
        .map(|args| self.git_paths(args))
        .collect::<Result<Vec<_>, _>>();
        let inspected = match inspected {
            Ok(inspected) => inspected,
            Err(why) => {
                undo(self);
                return Err(self.commit_log_refusal(why, false));
            }
        };
        let moved: Vec<String> = inspected
            .into_iter()
            .flatten()
            .filter(|p| p != "Cargo.toml" && p != "Cargo.lock" && !followed.contains(p))
            .collect();
        let Some(diff) = self.git_out(&["diff", "-U0"]) else {
            undo(self);
            return Err(self.commit_log_refusal(
                "could not inspect the bump's diff; the bump is undone",
                false,
            ));
        };
        let stray = diff.lines().any(|l| {
            (l.starts_with('+') || l.starts_with('-'))
                && !l.starts_with("+++")
                && !l.starts_with("---")
                && !l.contains("version")
        });
        if !moved.is_empty() || stray {
            undo(self);
            return Err(Refusal::new(
                "the bump moved more than version lines; it is undone",
                "bump by hand and read the diff",
            )
            .detail(moved));
        }
        // The bump opens a release window, so it also carries the harness versions the
        // window owes (`ship harness-bump`'s writes), after the version-only check above
        // judged the edit's own changes.
        let harness_timing =
            crate::cut_timing::Timer::start(&self.step_log(self.step), "bump-harness-versions");
        let owed = match crate::harness_versions::apply(&self.repo, origin) {
            Ok(applied) => applied.map(|(_, owed)| owed).unwrap_or_default(),
            Err(e) => {
                undo(self);
                return Err(self.commit_log_refusal(
                    format!("the harness versions could not be bumped ({e}); the bump is undone"),
                    false,
                ));
            }
        };
        drop(harness_timing);
        let msg = if owed.is_empty() {
            format!("release: {to} — the version and the lockfile move together")
        } else {
            let names: Vec<String> = owed
                .iter()
                .map(|o| format!("alab {} harness {}.{}", o.harness.cli(), o.to.0, o.to.1))
                .collect();
            format!(
                "release: {to} — the version, the lockfile and the harness versions move \
             together ({})",
                names.join(", ")
            )
        };
        let mut declared = vec!["Cargo.toml".into(), "Cargo.lock".into()];
        declared.extend(followed);
        declared.extend(crate::harness_versions::written(&owed));
        Ok((msg, declared))
    }

    /// The bump `c` names, committed in this release's private input checkout and on no ref.
    /// The editor's own refusals ([`Real::edit_bump`]) are verdicts on the tree; a failure of
    /// the construction around it (the checkout, its path, a git step, the cleanup) judged
    /// nothing, so it could not run, with a remedy naming that checkout.
    fn preview_bump_from_origin(
        &mut self,
        c: &Candidate,
        prover: &Prover,
        origin: &dyn crate::ledger::Origin,
    ) -> Result<String, Refusal> {
        let checkout = crate::input_record::directory(&self.release);
        let place = checkout.display().to_string();
        let plumbing = |gate: &str, why: &str| {
            private_bump_unavailable(
                gate,
                why,
                format!(
                    "read the error above, then retry the dry run; no ref changed and the \
                     private checkout {place} keeps no edit"
                ),
            )
        };
        let (root, _claim) = self.candidate_checkout(&c.commit).map_err(|why| {
            private_bump_unavailable(
                "private bump checkout",
                &why,
                format!(
                    "retry once no other release-input judgment of this clone runs (ship \
                     input-check, another dry run); if the error is about the private \
                     checkout {place} itself, inspect it first"
                ),
            )
        })?;
        let root = root
            .canonicalize()
            .map_err(|e| plumbing("private bump path", &e.to_string()))?;
        let original = std::mem::replace(&mut self.repo, root);
        let result = (|| {
            let (message, paths) = self.edit_bump(
                c.bump_from.as_deref().expect("pending bump"),
                &c.version,
                prover,
                origin,
            )?;
            let git = self.git();
            let mut add = vec!["add", "--"];
            add.extend(paths.iter().map(String::as_str));
            crate::ledger::git_ok(&git, &add)
                .map_err(|e| plumbing("private bump index", &e.to_string()))?;
            let tree = crate::ledger::git_ok(&git, &["write-tree"])
                .map_err(|e| plumbing("private bump tree", &e.to_string()))?
                .stdout_utf8();
            let date = crate::ledger::git_ok(&git, &["show", "-s", "--format=%ct", &c.commit])
                .map_err(|e| plumbing("private bump date", &e.to_string()))?
                .stdout_utf8();
            let date = format!("{} +0000", date.trim());
            let out = git
                .git_env(
                    &["commit-tree", tree.trim(), "-p", &c.commit, "-m", &message],
                    &[
                        ("GIT_AUTHOR_NAME", "Release preview"),
                        ("GIT_AUTHOR_EMAIL", "preview@localhost"),
                        ("GIT_COMMITTER_NAME", "Release preview"),
                        ("GIT_COMMITTER_EMAIL", "preview@localhost"),
                        ("GIT_AUTHOR_DATE", &date),
                        ("GIT_COMMITTER_DATE", &date),
                    ],
                )
                .map_err(|e| plumbing("private bump commit", &e.to_string()))?;
            if !out.success() {
                return Err(plumbing("private bump commit", &out.stderr_utf8()));
            }
            Ok(out.stdout_utf8().trim().to_owned())
        })();
        // Only our private, claimed checkout was edited. Restore its index as well as files,
        // including when an editor/validator refused part way through.
        let cleaned = crate::ledger::git_ok(
            &self.git(),
            &[
                "restore",
                "--source=HEAD",
                "--staged",
                "--worktree",
                "--",
                ".",
            ],
        );
        self.repo = original;
        cleaned.map_err(|why| {
            private_bump_unavailable(
                "private bump cleanup",
                &why.to_string(),
                private_bump_cleanup_remedy(&checkout),
            )
        })?;
        result
    }

    /// The stable R-tree checkout shared by dry commit gates and release-input. Its lock
    /// spans each reader, so another invocation cannot change the tree under that reader.
    fn candidate_checkout(
        &self,
        commit: &str,
    ) -> Result<(PathBuf, aterm_uds::file_lock::LockFile), String> {
        let claim = crate::input_record::claim(&self.release)?;
        let root = crate::input_record::directory(&self.release);
        crate::input_record::checkout(&self.git(), &root, commit)?;
        if root.join(".gitmodules").exists() {
            crate::ledger::git_ok(
                &GitCli::new(&root),
                &["submodule", "update", "--init", "vendor/astream"],
            )
            .map_err(|e| e.to_string())?;
        }
        Ok((root, claim))
    }

    fn commit_gates_from_origin(
        &mut self,
        c: &Candidate,
        dry: bool,
        origin: crate::ledger::Result<crate::github_remote::Remote>,
    ) -> Preflight {
        let _timing =
            crate::cut_timing::Timer::start(&self.step_log(self.step), "commit-preflight");
        let mut blocks: Vec<Refusal> = Vec::new();
        let transcript = self.transcript();
        let log = self.step_log(self.step);
        let notice = CommitGateNotice {
            candidate: c,
            step: self.step,
            gate_dry: dry,
            log: &log,
            transcript: &transcript,
        };
        let mut report = |gate: &str, refusal: &Refusal| notice.emit(gate, refusal);
        let git = self.git();
        // Bracket remote readers, but never pay for an inputs seal before a cheap red.
        let tags = (!dry).then(|| {
            crate::cut_timing::measure("commit-gate tags", || {
                origin
                    .as_ref()
                    .map_err(ToString::to_string)?
                    .tag_fingerprint()
            })
        });
        // Each gate's refusal, or what it says the run must show once the preflight passes.
        let asked = || {
            origin
                .as_ref()
                .map(|origin| origin as &dyn crate::ledger::Origin)
        };
        let (commit, version) = (c.commit.as_str(), c.version.as_str());
        let mut notes = Vec::new();
        let _ = check_commit_gate("changelog", &mut blocks, &mut report, || {
            crate::cut_timing::measure("commit-gate changelog", || {
                origin_gate("changelog", asked(), &|origin| {
                    let catalog = origin.tag_catalog().map_err(crate::ledger::Error::new)?;
                    crate::gates::changelog_gate_at(&git, commit, version, &catalog).map(|_| ())
                })
            })
        });
        let harness = check_commit_gate("harness versions", &mut blocks, &mut report, || {
            crate::cut_timing::measure("commit-gate harness versions", || {
                origin_gate("harness versions", asked(), &|origin| {
                    // A bump still to be written (the real launcher's base preflight) owes its
                    // harness versions to its own commit, which the preflight names; R is held
                    // to the strict gate.
                    match c.bump_from.as_deref() {
                        Some(from) => crate::gates::harness_versions_before_bump_at(
                            &git, origin, commit, from, version,
                        ),
                        None => {
                            crate::gates::harness_versions_gate_at(&git, origin, commit, version)
                                .map(|_| None)
                        }
                    }
                })
            })
        });
        notes.extend(harness.flatten());
        let _ = check_commit_gate("handoff fixtures", &mut blocks, &mut report, || {
            crate::cut_timing::measure("commit-gate handoff fixtures", || {
                origin_gate("handoff fixtures", asked(), &|origin| {
                    crate::gates::handoff_fixture_gate_at(&git, origin, commit, version).map(|_| ())
                })
            })
        });
        let _ = check_commit_gate("handoff policy", &mut blocks, &mut report, || {
            crate::cut_timing::measure("commit-gate handoff policy", || {
                origin_gate("handoff policy", asked(), &|origin| {
                    crate::gates::handoff_policy_gate_at(&git, origin, commit, version).map(|_| ())
                })
            })
        });
        let checkout = if dry {
            let Some(checkout) =
                check_commit_gate("R-tree locked metadata", &mut blocks, &mut report, || {
                    self.candidate_checkout(commit)
                        .map_err(|why| cut_refusal("R-tree locked metadata", &why).could_not_run())
                })
            else {
                return Preflight {
                    notes,
                    found: blocks,
                };
            };
            Some(checkout)
        } else {
            None
        };
        let metadata_root = checkout
            .as_ref()
            .map_or(self.repo.as_path(), |(root, _)| root.as_path());
        let _ = check_commit_gate("locked metadata", &mut blocks, &mut report, || {
            crate::cut_timing::measure("commit-gate locked metadata", || {
                crate::gates::locked_metadata_gate(metadata_root)
            })
            .map_err(|error| {
                let refusal = cut_refusal("locked metadata", &error.to_string());
                match error {
                    crate::gates::LockedMetadataError::Unavailable(_) => refusal.could_not_run(),
                    crate::gates::LockedMetadataError::Lockfile(_) => {
                        refusal.failing("locked metadata")
                    }
                }
            })
        });
        let _ = check_commit_gate("prerequisites", &mut blocks, &mut report, || {
            crate::cut_timing::measure("commit-gate prerequisites", || self.prepare_prerequisites())
        });
        let _ = check_commit_gate("engine readiness", &mut blocks, &mut report, || {
            self.engine_readiness()
        });
        if blocks.is_empty()
            && let Some(tags) = tags
        {
            let record = (|| {
                let tags = tags?;
                let after = origin
                    .as_ref()
                    .map_err(ToString::to_string)?
                    .tag_fingerprint()?;
                if tags != after {
                    return Err(
                        "advertised release tags changed while qualify judged fixtures/policy"
                            .into(),
                    );
                }
                let inputs = crate::build_inputs::current()?;
                crate::gates::CommitGateRecord::new(&c.commit, &c.version, inputs, tags)
                    .save(&git)
                    .map_err(|e| e.to_string())
            })();
            if let Err(why) = record {
                let _ =
                    check_commit_gate::<()>("commit gate record", &mut blocks, &mut report, || {
                        Err(cut_refusal("commit gate record", &why).could_not_run())
                    });
            }
        }
        Preflight {
            notes,
            found: blocks,
        }
    }
}

/// Evaluate one actual gate before reporting its typed refusal and retaining it for the
/// unchanged aggregate. Taking the evaluator, rather than its result, makes the order explicit.
pub(crate) fn check_commit_gate<T>(
    gate: &str,
    blocks: &mut Vec<Refusal>,
    report: &mut impl FnMut(&str, &Refusal),
    evaluate: impl FnOnce() -> Result<T, Refusal>,
) -> Option<T> {
    match evaluate() {
        Ok(value) => Some(value),
        Err(refusal) => {
            report(gate, &refusal);
            blocks.push(refusal);
            None
        }
    }
}

/// R's commit-only gate `gate`, which reads origin in `check`. Its refusal is R's red
/// ([`Refusal::failing`]) when every read of origin answered; with no origin, or a read of it
/// that failed while the gate judged, the gate could not ask: COULD NOT RUN, deciding nothing of
/// R's source, so no repair worker opens on a remote's failure.
fn origin_gate<T>(
    gate: &str,
    origin: Result<&dyn crate::ledger::Origin, &crate::ledger::Error>,
    check: &dyn Fn(&dyn crate::ledger::Origin) -> crate::ledger::Result<T>,
) -> Result<T, Refusal> {
    let origin = origin.map_err(|error| cut_refusal(gate, &error.to_string()).could_not_run())?;
    let asked = Asked {
        origin,
        failed: std::cell::Cell::new(false),
    };
    check(&asked).map_err(|error| {
        let refusal = cut_refusal(gate, &error.to_string());
        if asked.failed.get() {
            refusal.could_not_run()
        } else {
            refusal.failing(gate)
        }
    })
}

/// The origin one gate asks ([`origin_gate`]): each call passes through, and `failed` keeps
/// whether any of them failed.
struct Asked<'a> {
    origin: &'a dyn crate::ledger::Origin,
    failed: std::cell::Cell<bool>,
}

impl Asked<'_> {
    fn noted<T>(&self, answer: Result<T, String>) -> Result<T, String> {
        if answer.is_err() {
            self.failed.set(true);
        }
        answer
    }
}

impl crate::ledger::Origin for Asked<'_> {
    fn refresh_main(&self) -> Result<String, String> {
        self.noted(self.origin.refresh_main())
    }
    fn tag_catalog(&self) -> Result<String, String> {
        self.noted(self.origin.tag_catalog())
    }
    fn tag_commit(&self, tag: &str) -> Result<Option<String>, String> {
        self.noted(self.origin.tag_commit(tag))
    }
    fn first_tag_commit(&self, tags: &[String]) -> Result<Option<(String, String)>, String> {
        self.noted(self.origin.first_tag_commit(tags))
    }
    fn submodule_tip(&self, path: &Path, reference: &str) -> Result<String, String> {
        self.noted(self.origin.submodule_tip(path, reference))
    }
    fn get(&self, reference: &str) -> Result<Option<String>, String> {
        self.noted(self.origin.get(reference))
    }
    fn publisher_controls(&self) -> Result<(Option<String>, Option<String>), String> {
        self.noted(self.origin.publisher_controls())
    }
    fn import(&self, oid: &str) -> Result<(), String> {
        self.noted(self.origin.import(oid))
    }
    fn peel(&self, oid: &str) -> Result<Option<String>, String> {
        self.noted(self.origin.peel(oid))
    }
    fn tag_object(&self, oid: &str) -> Result<Vec<u8>, String> {
        self.noted(self.origin.tag_object(oid))
    }
    fn write(
        &self,
        reference: &str,
        before: Option<&str>,
        after: &str,
    ) -> Result<crate::ledger::RefWrite, String> {
        self.noted(self.origin.write(reference, before, after))
    }
    fn delete(
        &self,
        reference: &str,
        expected_oid: &str,
    ) -> Result<crate::ledger::RefWrite, String> {
        self.noted(self.origin.delete(reference, expected_oid))
    }
    fn control_transaction(
        &self,
        transaction: crate::ledger::ControlRefTransaction,
    ) -> Result<crate::ledger::RefWrite, String> {
        self.noted(self.origin.control_transaction(transaction))
    }
}

/// Informational context for a finding, never a stage completion or release permission.
/// `gate_dry` is the backend gate mode; real prefreeze also uses that mode.
pub(crate) struct CommitGateNotice<'a> {
    pub(crate) candidate: &'a Candidate,
    pub(crate) step: Step,
    pub(crate) gate_dry: bool,
    pub(crate) log: &'a Path,
    pub(crate) transcript: &'a Path,
}

fn commit_gate_text(text: &str, chars: usize) -> (&str, bool) {
    text.char_indices()
        .nth(chars)
        .map_or((text, false), |(end, _)| (&text[..end], true))
}

impl CommitGateNotice<'_> {
    pub(crate) fn emit(&self, gate: &str, refusal: &Refusal) {
        let event = self.event(gate, refusal);
        // Same best-effort terminal/transcript path as existing release events. Reporting
        // failure must not change the gate's verdict, aggregation or dry-run return behavior.
        say_to(self.transcript, &format!("release-event: {event}"));
    }

    pub(crate) fn event(&self, gate: &str, refusal: &Refusal) -> aterm_json::Value {
        let (detail, detail_cut) = commit_gate_text(&refusal.why, 2048);
        let (remedy, remedy_cut) = commit_gate_text(&refusal.remedy, 1024);
        let mut lines_cut = refusal.detail.len() > 4;
        let lines: Vec<_> = refusal
            .detail
            .iter()
            .take(4)
            .map(|line| {
                let (line, cut) = commit_gate_text(line, 512);
                lines_cut |= cut;
                line
            })
            .collect();
        let (source, source_cut) = commit_gate_text(&self.candidate.commit, 128);
        let (version, version_cut) = commit_gate_text(&self.candidate.version, 128);
        let (gate, gate_cut) = commit_gate_text(gate, 128);
        let log = self
            .log
            .to_str()
            .filter(|path| !commit_gate_text(path, 2048).1);
        aterm_json::json!({
            "schema": 1, "event": "commit-gate-failed", "terminal": false,
            "stage": self.step.name(), "gate": gate, "gate_dry": self.gate_dry,
            "source": source, "version": version, "at_unix_s": now(), "log": log,
            "outcome": if refusal.could_not_run { "NOT RUN" } else { "FAIL" },
            "detail": detail, "remedy": remedy, "refusal_detail": lines,
            "detail_truncated": detail_cut || remedy_cut || lines_cut,
            "context_truncated": source_cut || version_cut || gate_cut,
            "log_unavailable": log.is_none(),
        })
    }
}

fn cached_prefrozen_seal(real: &Real, commit: &str) -> Option<(String, PathBuf)> {
    if !cfg!(target_os = "macos") {
        return None;
    }
    let cache = cutter_cache_dir(&real.release);
    let cutter = crate::release_tools::recorded_cutter(&cache, commit).ok()?;
    let seal = cutter.inputs?.seal;
    let path = (real.prefrozen_inputs)(&cache.join("state"), &seal)?;
    Some((seal, path))
}

impl Backend for Real {
    fn main_lane(&mut self, base: &str) -> Option<String> {
        Some(
            match self.git_out(&["rev-parse", &format!("{base}^{{tree}}")]) {
                Some(tree) => crate::main_lane::observe(&self.repo, base, &tree, now),
                None => format!("main lane: base {base}: tree unavailable (advisory)"),
            },
        )
    }
    fn prepare_sign(
        &mut self,
        args: &[String],
        prover: &Prover,
    ) -> Result<Box<dyn crate::release::PendingSign>, Refusal> {
        let start = || -> Result<PreparingSign, String> {
            self.start_preparing_sign(self.cut_command(args, prover)?)
        };
        start()
            .map(|pending| Box::new(pending) as Box<dyn crate::release::PendingSign>)
            .map_err(|why| {
                Refusal::new(
                    format!("early signer did not start: {why}"),
                    "read the packaging log, then resume the release",
                )
                .could_not_run()
            })
    }
    fn now(&mut self) -> u64 {
        now()
    }

    fn hms(&mut self, epoch: u64) -> String {
        hms_local(epoch, aterm_uds::zone::local_offset_at)
    }

    fn sleep(&mut self, secs: u64) {
        for _ in 0..secs {
            if interrupt::caught().is_some() {
                return;
            }
            std::thread::sleep(std::time::Duration::from_secs(1));
        }
    }

    fn say(&mut self, line: &str) {
        say_to(&self.transcript(), line);
    }

    fn interrupted(&mut self) -> Option<i32> {
        interrupt::caught()
    }

    fn log_path(&mut self, step: Step) -> PathBuf {
        self.step_log(step)
    }

    fn me(&mut self) -> (u32, String) {
        let pid = std::process::id();
        (pid, self.started_of(pid).unwrap_or_default())
    }

    fn started_of(&mut self, pid: u32) -> Option<String> {
        started_of(pid)
    }

    fn entered(&mut self, step: Step) {
        self.step = step;
        // What the release wrote so far is no longer free, so a peer's wait counts only the rest
        // of its claim as reserved.
        if let Some(claim) = &mut self.disk_claim
            && let Some(bytes) = crate::release_timeline::Timeline::of_logs(&self.logs)
                .ok()
                .and_then(|t| t.output_bytes())
            && let Err(error) = claim.wrote(bytes)
        {
            self.say(&format!(
                "        disk: cannot note what the release wrote: {error}"
            ));
        }
        if !self.scratch_logs
            && crate::release::read_holder(&self.release.join(crate::release::RUNNING_FILE))
                .is_some_and(|holder| holder.pid == std::process::id())
            && let Err(error) = crate::durable::write_atomic_durable(
                &self.release.join("latest-run"),
                self.logs.to_string_lossy().as_bytes(),
                0o600,
            )
        {
            self.say(&format!("release status: cannot record log path: {error}"));
        }
    }

    fn start_prefreeze(&mut self) {
        #[cfg(unix)]
        {
            let start = || -> Result<aterm_uds::spawnfd::Child, String> {
                let line = self
                    .line_priority
                    .as_ref()
                    .ok_or("release line claim absent")?;
                let log = crate::durable::open_append(&self.logs.join("prefreeze.log"), true)
                    .map_err(|e| e.to_string())?;
                let mut cmd = command(std::env::current_exe().map_err(|e| e.to_string())?);
                cmd.args(["release", "--prefreeze"])
                    .current_dir(&self.repo)
                    .env(AUTO_PREFREEZE, line.record().cover_marker())
                    .env(aterm_capacity::RELEASE_RUN_ENV, "1")
                    .env_remove("TRUST_STAGE2_BIN")
                    .env_remove(crate::build_inputs::SEAL_ENV)
                    .stdin(Stdio::null())
                    .stdout(log.try_clone().map_err(|e| e.to_string())?)
                    .stderr(log)
                    .setsid(true)
                    .default_signals()
                    .enter_background_band();
                cmd.spawn().map_err(|e| e.to_string())
            };
            match start() {
                Ok(child) => {
                    self.say(&format!(
                        "release-event: {}",
                        aterm_json::json!({
                            "schema":1,"event":"prefreeze-started","pid":child.id(),
                            "timing_scope":"next-cut",
                        })
                    ));
                    // Do not poll/reap this child inside a step: RUSAGE_CHILDREN would
                    // charge its preparation to whichever step happened to reap it.
                    self.automatic_prefreeze = Some(child);
                }
                Err(why) => self.say(&format!(
                    "release-event: {}",
                    aterm_json::json!({
                        "schema":1,"event":"prefreeze-unavailable","why":why,
                    })
                )),
            }
        }
    }

    fn fleet_line(&mut self, line: &Line, continued: bool) -> Result<Line, String> {
        let prefix =
            crate::line::recorded_prefix(&self.git(), line).map_err(|error| error.to_string())?;
        if let Some(claim) = &mut self.fleet_line
            && claim.line == prefix
        {
            claim.audit()?;
            return Ok(line.clone());
        }
        let take = if continued {
            crate::line_claim::Take::Continue
        } else {
            crate::line_claim::Take::Contend
        };
        let claim = crate::line_claim::Claim::take(&self.repo, &prefix, &self.logs, take)?;
        let selected = if claim.line == prefix {
            line.clone()
        } else {
            claim.line.clone()
        };
        if let Some(holder) = &claim.previous_holder {
            self.fleet_holder = Some((claim.line.head().to_string(), holder.clone()));
        }
        self.fleet_line = Some(claim);
        Ok(selected)
    }

    fn fleet_supersession(
        &mut self,
        version: &str,
    ) -> Result<Option<crate::line::Supersession>, Refusal> {
        crate::line::supersession(&self.git(), version)
            .map_err(|e| Refusal::new(e, "retry the current line read").could_not_run())
    }

    fn preflight_supersede(
        &mut self,
        line: &Line,
        expected_previous: Option<&Line>,
    ) -> Result<Line, Refusal> {
        if let Some(expected) = expected_previous {
            self.supersede_staging_state(expected, line)
                .map_err(|error| error.refusal(Some(expected)))?;
        }
        self.prefetched_origin = None;
        self.prefetch_source()?;
        let previous = crate::line_claim::existing(&self.repo, &line.version)
            .map_err(|why| Refusal::new(why, "retry the current fleet line read").could_not_run())?
            .ok_or_else(|| {
                Refusal::new(
                    "no existing fleet line to supersede",
                    "resume the existing line, or drop --supersede",
                )
            })?;
        if expected_previous.is_some_and(|expected| *expected != previous) {
            return Err(Refusal::new(
                "the fleet line advanced after this clone's release journal",
                "resume that line and carry the fix instead of superseding its new hop",
            ));
        }
        if expected_previous.is_none() {
            self.supersede_staging_state(&previous, line)
                .map_err(|error| error.refusal(Some(&previous)))?;
        }
        if crate::line::supersession(&self.git(), &line.version)
            .map_err(|why| Refusal::new(why, "retry the current line read").could_not_run())?
            .is_some_and(|record| record.replacement.base != previous.base)
        {
            return Err(Refusal::new(
                "the line already has a durable supersession",
                "resume the current fleet line",
            ));
        }
        self.origin_main = None;
        self.supersede_public_state_after_staging(&previous, line)
            .map_err(|error| error.refusal(Some(&previous)))?;
        Ok(previous)
    }

    fn classify_supersede(&mut self, previous: &Line, line: &Line) -> SupersedeResult {
        let requested = supersession_record(previous.clone(), line.clone());
        self.prefetched_origin = None;
        if let Err(refusal) = self.prefetch_source() {
            return SupersedeResult::Unknown(format!("{} — {}", refusal.why, refusal.remedy));
        }
        self.verify_supersede_result(classify_supersession_reread(
            &requested,
            crate::line::supersession(&self.git(), &line.version),
            true,
        ))
    }

    fn supersede_line(&mut self, expected_previous: &Line, line: &Line) -> SupersedeResult {
        let requested = supersession_record(expected_previous.clone(), line.clone());
        let refused = |why: String| SupersedeResult::Refused {
            why,
            retry: false,
            winner: None,
        };
        self.prefetched_origin = None;
        if let Err(error) = self.prefetch_source() {
            return refused(error.why);
        }
        let previous = match crate::line_claim::existing(&self.repo, &line.version) {
            Ok(Some(previous)) => previous,
            Ok(None) => return refused("no existing fleet line to supersede".into()),
            Err(why) => return refused(why),
        };
        let mut claim = match crate::line_claim::Claim::take(
            &self.repo,
            &previous,
            &self.logs,
            crate::line_claim::Take::Supersede,
        ) {
            Ok(claim) => claim,
            Err(why) => return self.classify_after_claim_stop(&requested, why),
        };
        let previous = claim.line.clone();
        self.prefetched_origin = None;
        if let Err(error) = self.prefetch_source() {
            return self.classify_after_claim_stop(&requested, error.why);
        }
        match crate::line::supersession(&self.git(), &line.version) {
            Ok(Some(record)) if record.replacement.base != expected_previous.base => {
                let result = self.verify_supersede_result(classify_supersession_reread(
                    &requested,
                    Ok(Some(record)),
                    false,
                ));
                if matches!(result, SupersedeResult::Committed(_)) {
                    self.fleet_line = Some(claim);
                }
                return result;
            }
            Ok(_) => {}
            Err(why) => return self.classify_after_claim_stop(&requested, why),
        }
        if previous != *expected_previous {
            return self.classify_after_claim_stop(
                &requested,
                "the fleet line advanced while the replacement took its claim".into(),
            );
        }
        if let Err(why) = claim.audit() {
            return self.classify_after_claim_stop(&requested, why);
        }
        self.origin_main = None;
        let public_state = self.supersede_public_state(&previous, line);
        if let Err(result) = settle_post_claim_public_state(public_state, &previous, |why| {
            self.classify_after_claim_stop(&requested, why)
        }) {
            return result;
        }
        if let Err(why) = claim.audit() {
            return self.classify_after_claim_stop(&requested, why);
        }
        let record = supersession_record(previous, line.clone());
        let origin = match crate::github_remote::Remote::origin(&self.repo) {
            Ok(origin) => origin,
            Err(why) => return self.classify_after_claim_stop(&requested, why),
        };
        let recorded = match crate::line::supersede(&self.git(), &origin, &record) {
            Ok(merge) => format!("recorded on main at {merge}"),
            Err(write_error) => {
                self.prefetched_origin = None;
                if let Err(refusal) = self.prefetch_source() {
                    return SupersedeResult::Unknown(format!(
                        "{write_error}; exact reread unavailable: {} — {}",
                        refusal.why, refusal.remedy
                    ));
                }
                match self.verify_supersede_result(classify_supersession_reread(
                    &record,
                    crate::line::supersession(&self.git(), &line.version),
                    false,
                )) {
                    SupersedeResult::Committed(_) => {
                        "recorded on main (confirmed by an exact reread)".into()
                    }
                    SupersedeResult::Refused { why, retry, winner } => {
                        return SupersedeResult::Refused {
                            why: format!("{write_error}; {why}"),
                            retry,
                            winner,
                        };
                    }
                    SupersedeResult::Unknown(read_error) => {
                        return SupersedeResult::Unknown(format!(
                            "{write_error}; exact reread failed: {read_error}"
                        ));
                    }
                }
            }
        };
        let mut warnings = Vec::new();
        if let Err(why) = claim.boundary(line, "commit", &self.times()) {
            warnings.push(format!("claim boundary will retry: {why}"));
        }
        if let Err(why) = claim.audit() {
            warnings.push(format!("claim audit will retry: {why}"));
        }
        self.fleet_line = Some(claim);
        let warnings = if warnings.is_empty() {
            String::new()
        } else {
            format!("; {}", warnings.join("; "))
        };
        SupersedeResult::Committed(format!(
            "SUPERSEDED v{} line {} (base {}) with {}: {}; {recorded}{warnings}",
            line.version,
            record.previous.head(),
            record.previous.base,
            line.base,
            record.reason
        ))
    }

    fn supersession_completed(&mut self) {
        self.argv.retain(|arg| arg != "--supersede");
    }

    fn fleet_existing(&mut self, version: &str) -> Result<Option<Line>, Refusal> {
        let retry = || "retry the fleet line read";
        let snapshot = crate::line_claim::existing_with_holder(&self.repo, version)
            .map_err(|why| Refusal::new(why, retry()).could_not_run())?;
        self.fleet_holder = snapshot.holder;
        let mut line = snapshot.line;
        // Source discovery must see durable supersessions even when the independent claim
        // read was stale. It reads no engine or roster, and step 1 consumes the same result.
        self.prefetch_source()?;
        line = crate::line_claim::current(&self.git(), version, line)
            .map_err(|why| Refusal::new(why, retry()).could_not_run())?;
        if let Some(line) = &line {
            crate::line::verify(&self.git(), line)
                .map_err(|error| Refusal::new(error.to_string(), retry()).could_not_run())?;
        }
        Ok(line)
    }

    fn fleet_holder(&mut self, head: &str) -> Option<String> {
        self.fleet_holder
            .as_ref()
            .filter(|(h, _)| h == head)
            .map(|(_, holder)| holder.clone())
    }

    fn main_past_line(&mut self, head: &str, main: &str) -> Option<(usize, usize)> {
        let range = format!("{head}..{main}");
        let count = |args: &[&str]| {
            let out = self.git().git_read(args).ok()?;
            out.success()
                .then(|| out.stdout_utf8().trim().parse::<usize>().ok())
                .flatten()
        };
        Some((
            count(&["rev-list", "--count", &range])?,
            count(&[
                "rev-list",
                "--count",
                &range,
                "--",
                "crates/aterm-release",
                "crates/aterm-verify",
            ])?,
        ))
    }

    fn fleet_boundary(&mut self, line: &Line, step: Step) -> Result<(), String> {
        let history = self.times();
        let prefix =
            crate::line::recorded_prefix(&self.git(), line).map_err(|error| error.to_string())?;
        if let Some(claim) = &mut self.fleet_line {
            claim.boundary(&prefix, step.name(), &history)?;
        }
        Ok(())
    }

    fn fleet_audit(&mut self) -> Result<(), String> {
        self.fleet_line
            .as_mut()
            .ok_or("no fleet line claim")?
            .audit()
    }

    fn observe_operational(&mut self) {
        #[cfg(unix)]
        let observation = aterm_agent::operational::observe();
        #[cfg(not(unix))]
        let observation = aterm_json::Value::Object(aterm_json::Map::from([
            ("state".to_string(), "unknown".into()),
            (
                "reason".to_string(),
                "operational observation is unavailable on this platform".into(),
            ),
        ]));
        let record = aterm_json::Value::Object(aterm_json::Map::from([
            ("schema".to_string(), 1.into()),
            (
                "attempt".to_string(),
                self.logs.to_string_lossy().into_owned().into(),
            ),
            ("observed_at".to_string(), now().into()),
            ("observation".to_string(), observation),
        ]));
        match aterm_json::to_vec(&record)
            .map_err(|e| e.to_string())
            .and_then(|bytes| {
                crate::durable::write_atomic_durable(
                    &self.logs.join("operational-status.json"),
                    &bytes,
                    0o600,
                )
                .map_err(|e| e.to_string())
            }) {
            Ok(()) => self.say(&format!(
                "        operational status: {}",
                self.logs.join("operational-status.json").display()
            )),
            Err(error) => self.say(&format!(
                "        operational status could not be recorded: {error}"
            )),
        }
    }

    fn release_dir(&mut self) -> PathBuf {
        self.release.clone()
    }

    fn times(&mut self) -> String {
        crate::durable::read_text(&self.state.join("steps.tsv")).unwrap_or_default()
    }

    fn record_time(&mut self, version: &str, row: &str, secs: u64) {
        let _ = std::fs::create_dir_all(&self.state);
        let mut open = std::fs::OpenOptions::new();
        open.create(true).append(true);
        #[cfg(unix)]
        {
            use std::os::unix::fs::OpenOptionsExt as _;
            open.custom_flags(aterm_uds::atomic_file::LOCK_OPEN_FLAGS);
        }
        if let Ok(mut f) = open.open(self.state.join("steps.tsv"))
            && f.metadata().is_ok_and(|m| m.is_file())
        {
            let _ = writeln!(f, "{}\t{version}\t{row}\t{secs}", now());
        }
    }

    fn prover(&mut self, journaled: Option<&str>) -> Result<Prover, String> {
        let _phase =
            crate::cut_timing::Timer::start(&self.step_log(self.step), "prover-resolution");
        let explicit = std::env::var_os("TRUST_STAGE2_BIN").filter(|v| !v.is_empty());
        let (cand, source) = if let Some(dir) = explicit {
            (PathBuf::from(dir), "TRUST_STAGE2_BIN".to_string())
        } else if let Some(j) = journaled
            && Path::new(j).join("trustc").is_file()
            && Path::new(j).join("targo").is_file()
        {
            (
                PathBuf::from(j),
                "the prover this release started with".to_string(),
            )
        } else {
            let found = crate::gates::trust_stage2_bin().map_err(|e| e.to_string())?;
            let mut source = "the cut's own toolchain resolution".to_string();
            if let Some(j) = journaled {
                source.push_str(&format!(
                    " — the prover this release started with ({j}) is gone; resolving a replacement"
                ));
            }
            (found, source)
        };
        let bin = cand
            .canonicalize()
            .map_err(|e| format!("{source} ({}) is not a directory: {e}", cand.display()))?;
        if !bin.join("trustc").is_file() || !bin.join("targo").is_file() {
            return Err(format!(
                "{source} ({}) holds no trustc and targo",
                bin.display()
            ));
        }
        // Hold the resolved store entry before executing any of its tools.
        if !self.scratch_logs {
            crate::gates::lease_toolchain_checked(&bin)?;
        }
        let identity = crate::gates::toolchain_at(&bin).map_err(|e| e.to_string())?;
        let commit = identity.rsplit(' ').next().unwrap_or("unknown").to_string();
        if commit == "unknown" || commit.is_empty() {
            return Err(format!(
                "{}/trustc names no commit-hash in `trustc -vV`, so no receipt can tell it from \
                 another prover",
                bin.display()
            ));
        }
        crate::gates::pin_trust_stage2_bin(&bin).map_err(|e| e.to_string())?;
        Ok(Prover {
            bin,
            commit,
            source,
        })
    }

    fn lease(&mut self, prover: &Prover) -> Result<(), String> {
        crate::gates::lease_toolchain_checked(&prover.bin)
    }

    /// The release's one build-inputs seal: kept in the release state and handed to every child
    /// ([`hand_inputs_seal`]), taken here or adopted from the exact pre-freeze record. Adoption
    /// rechecks every input before [`PrefrozenStep1`] can authorize cached cutter selection. A
    /// dry run never comes here, and seals lazily, keeping none. Off macOS a release has no
    /// build-inputs identity to seal.
    fn seal_inputs(&mut self) -> Result<(), String> {
        let log = self.step_log(self.step);
        let _phase = crate::cut_timing::Timer::start(&log, "inputs-seal");
        self.prefrozen_step1 = None;
        if cfg!(target_os = "macos") {
            let commit = self.head();
            let prepared = cached_prefrozen_seal(self, &commit);
            if let Some((_, path)) = &prepared {
                (self.arm_prefrozen_inputs)(&self.state, Some(&self.transcript()), Some(path));
            } else {
                (self.arm_inputs)(&self.state, Some(&self.transcript()));
            }
            let current = (self.build_inputs)()?;
            if let Some((seal, _)) = prepared {
                let adopted = current == seal;
                if adopted {
                    self.prefrozen_step1 = Some(PrefrozenStep1 {
                        commit: commit.clone(),
                        seal: seal.clone(),
                    });
                }
                let event = aterm_json::json!({
                    "schema": 1,
                    "event": if adopted {
                        "prefreeze-step-1-inputs-adopted"
                    } else {
                        "prefreeze-step-1-inputs-rejected"
                    },
                    "commit": commit,
                    "prepared": seal,
                    "current": current,
                });
                if let Ok(mut file) = crate::durable::open_append(&log, true) {
                    let _ = writeln!(file, "release-event: {event}");
                }
                println!("release-event: {event}");
            }
        }
        Ok(())
    }

    fn cleared_env(&mut self) -> Vec<String> {
        let mut names: Vec<String> = std::env::vars_os()
            .filter_map(|(k, _)| k.into_string().ok())
            .filter(|k| clears(k) && !launcher_env(k))
            .collect();
        names.sort_unstable();
        names
    }

    fn machine(&mut self, creds: &Path, explicit: bool) -> (String, Vec<Refusal>) {
        let mut out = Vec::new();
        if let Err(why) = crate::source_signing::Tools::authority(
            &self.home,
            i64::try_from(now()).unwrap_or(i64::MAX),
        ) {
            out.push(Refusal::new(
                format!("source signing ring: {why}"),
                "run targo --unverified ship provision --id <existing signer id> to restore the complete canonical ring, then retry",
            ));
        }
        if explicit && !creds.is_file() {
            out.push(Refusal::new(
                format!("--release-credentials {} is not a file", creds.display()),
                "name the signing profile ship provision wrote (docs/RELEASE-KEYS.md), or \
                 drop the flag for ~/.aterm/release-ring/release-credentials.toml",
            ));
        }
        let field = |path: &Path, key: &str| {
            crate::durable::read_text(path).ok().and_then(|t| {
                t.lines().find_map(|l| {
                    let (k, v) = l.split_once('=')?;
                    (k.trim() == key).then(|| v.trim().trim_matches('"').to_string())
                })
            })
        };
        let id = field(
            &self.home.join(atpkg_keys::provision::MACHINE_PUB_REL),
            "id",
        )
        .or_else(|| field(creds, "machine_id"))
        .unwrap_or_default();
        if let (Some(free), _) = self.disk()
            && free < crate::gates::MIN_FREE_DISK_GIB
        {
            out.push(Refusal::new(
                format!(
                    "only {free} GiB free on {} — the cut needs {}",
                    self.repo.display(),
                    crate::gates::MIN_FREE_DISK_GIB
                ),
                "free space, then re-run",
            ));
        }
        out.extend(self.engine_unready());
        match self.dirty() {
            Ok(dirty) if dirty.is_empty() => {}
            Ok(dirty) => out.push(
                Refusal::new(
                    format!(
                        "this clone ({}) is not clean — a release needs its own clean checkout",
                        self.repo.display()
                    ),
                    "commit or remove what it lists: a release runs in a clone of its own",
                )
                .detail(dirty.into_iter().take(5)),
            ),
            Err(why) => out.push(Refusal::new(
                format!("this clone's state cannot be read: {why}"),
                "run git status there, then re-run",
            )),
        }
        (id, out)
    }

    fn fetch(&mut self) -> Result<String, Refusal> {
        let prefetched = self.prefetched_origin.take();
        let source_origin = self.source_origin;
        let dry = self.scratch_logs;
        self.fetch_origins(|repo| {
            prefetched.map_or_else(
                || {
                    source_origin(repo).and_then(|remote| {
                        if dry {
                            remote.observe_origin()
                        } else {
                            remote.fetch_origin()
                        }
                    })
                },
                Ok,
            )
        })
    }

    fn prepare_clone(&mut self) -> Result<(), Refusal> {
        let dirty = self.dirty().map_err(|why| {
            Refusal::new(
                format!("cannot read the release clone: {why}"),
                "restore a clean release clone and retry",
            )
        })?;
        if dirty.is_empty() {
            Ok(())
        } else {
            Err(Refusal::new(
                "the release clone is not clean",
                "commit or remove the listed changes before selecting a candidate",
            )
            .detail(dirty.into_iter().take(5)))
        }
    }

    fn head(&mut self) -> String {
        self.git_out(&["rev-parse", "HEAD"]).unwrap_or_default()
    }

    fn input_checks(
        &mut self,
        base: &str,
        tip: &str,
        prover: &Prover,
    ) -> Result<String, InputFinding> {
        let unavailable = |why: String| {
            InputFinding::Unanswered(format!("release input could not be judged: {why}"))
        };
        // Both a dry preflight and its eventual cut use this one owned checkout. A temporary
        // dry-run path followed by the release clone changes every compiler path input even
        // when the package bytes are equal. Never reset or clean it: refuse unexpected edits.
        let root = crate::input_record::directory(&self.release);
        let _checkout_claim = crate::input_record::claim(&self.release).map_err(unavailable)?;
        crate::input_record::checkout(&self.git(), &root, tip).map_err(unavailable)?;
        let root = root.as_path();
        let initialized = crate::ledger::GitCli::new(root).git(&[
            "submodule",
            "update",
            "--init",
            "vendor/astream",
        ]);
        (|| match initialized {
            Ok(out) if out.success() => {
                let input = aterm_verify::release_input::discover(
                    root,
                    &std::env::var_os("PATH").unwrap_or_default(),
                    base,
                    tip,
                )
                .map_err(unavailable)?;
                if let Some(note) = input.no_work_note() {
                    return Ok(note);
                }
                let environment = verify_environment();
                let plan =
                    aterm_verify::NativeBuildPlan::capture(&environment, root).map_err(|e| {
                        unavailable(format!("select release-input build directory: {e}"))
                    })?;
                let mut bootstrap = aterm_verify::native_verifier::build_command(
                    root,
                    &prover.bin.join("targo"),
                    &plan,
                )
                .map_err(unavailable)?;
                publication_environment(&self.engine, &mut bootstrap);
                let mut bootstrap =
                    self.configure_child(self.scrubbed_command(bootstrap), Some(prover));
                bootstrap.current_dir(root);
                crate::input_record::detach_launcher(&mut bootstrap);
                plan.apply_to_command(&mut bootstrap).map_err(|e| {
                    unavailable(format!("configure release-input build environment: {e}"))
                })?;
                let (built, accounting) = self
                    .output_accounted(Step::Checks, &mut bootstrap)
                    .map_err(|e| unavailable(e.to_string()))?;
                if let Ok(mut log) = crate::durable::open_append(&self.step_log(Step::Checks), true)
                {
                    let _ = log.write_all(&built.stdout);
                    let _ = log.write_all(&built.stderr);
                    let _ = writeln!(log, "{accounting}");
                }
                if !built.status.success() {
                    return Err(unavailable(format!(
                        "release-input verifier bootstrap exited {}: {}",
                        built.status,
                        String::from_utf8_lossy(&built.stderr)
                    )));
                }
                let verifier = aterm_verify::native_verifier::built(root, &built.stdout)
                    .map_err(unavailable)?;
                let runtime = || -> Result<Command, InputFinding> {
                    let mut command =
                        aterm_verify::native_verifier::command(&verifier, root, &environment)
                            .map_err(unavailable)?;
                    publication_environment(&self.engine, &mut command);
                    let mut command =
                        self.configure_child(self.scrubbed_command(command), Some(prover));
                    aterm_verify::native_verifier::configure_runtime(
                        &mut command,
                        root,
                        &environment,
                    );
                    command
                        .current_dir(root)
                        .args(["release-input", "--base", base]);
                    crate::input_record::detach_launcher(&mut command);
                    Ok(command)
                };
                let mut child = runtime()?;
                let seal = (self.build_inputs)().map_err(unavailable)?;
                let scanner = atpkg::prerequisite::configured(
                    &[atpkg::prerequisite::Prerequisite::Gitleaks],
                    "release input records",
                )
                .map_err(unavailable)?;
                let scanner = scanner
                    .tool(atpkg::prerequisite::Prerequisite::Gitleaks)
                    .ok_or_else(|| unavailable("no signed scanner was acquired".into()))?;
                scanner.validate().map_err(unavailable)?;
                let input_tools = format!("{seal}\ngitleaks {}", scanner.tree_root());
                // The handoff filename is a capability for reading this already-bound seal.
                // It differs between a dry run and a real run, without changing any input.
                let mut identity_child = runtime()?;
                self.finalize_spawn_environment(&mut identity_child)
                    .map_err(unavailable)?;
                identity_child.env_remove(crate::build_inputs::SEAL_ENV);
                let carry_context =
                    crate::input_record::context(root, &self.engine, &input_tools, &identity_child)
                        .map_err(unavailable)?;
                child.args(["--carry-context", &carry_context]);
                identity_child.args(["--carry-context", &carry_context]);
                let git = crate::ledger::GitCli::new(root);
                let identity = || {
                    crate::input_record::identity(
                        &git,
                        root,
                        &self.engine,
                        tip,
                        base,
                        &input_tools,
                        &identity_child,
                    )
                };
                let (tree, input_seal) = identity().map_err(unavailable)?;
                let records = self
                    .release
                    .join(crate::check_record::DIR)
                    .join("release-input");
                let description = crate::input_record::describe(&identity_child, &input_tools)
                    .map_err(unavailable)?;
                crate::durable::write_atomic_durable(
                    &records
                        .join(&tree)
                        .join(crate::check_record::seal_dir(&input_seal))
                        .join("inputs.json"),
                    description.as_bytes(),
                    0o600,
                )
                .map_err(|e| unavailable(e.to_string()))?;
                let _input_claim = crate::input_record::claim(
                    &records
                        .join(&tree)
                        .join(crate::check_record::seal_dir(&input_seal)),
                )
                .map_err(unavailable)?;
                let missing = crate::check_record::input_missing(&records, &tree, &input_seal)
                    .map_err(|why| match why {
                        crate::check_record::InputError::Failed(why) => InputFinding::Failed(why),
                        crate::check_record::InputError::Unavailable(why) => unavailable(why),
                    })?;
                if missing.is_empty() {
                    crate::input_record::attempted(&records, base, &tree, &input_seal)
                        .map_err(unavailable)?;
                    return Ok(format!(
                        "release input: reused {} checks for tree {tree}; 0 checks run",
                        aterm_verify::release_input::CHECKS.len()
                    ));
                }
                let run_id = self
                    .logs
                    .file_name()
                    .ok_or_else(|| unavailable("no run name".into()))?
                    .to_string_lossy();
                let evidence = records
                    .join(&tree)
                    .join(crate::check_record::seal_dir(&input_seal));
                let events = evidence.join(format!("{run_id}.events.jsonl"));
                let retained_log = evidence.join(format!("{run_id}.log"));
                let items =
                    crate::input_record::previous_items(&records, base).map_err(unavailable)?;
                let item_file = evidence.join(format!("{run_id}.inputs.json"));
                crate::durable::write_atomic_durable(
                    &item_file,
                    aterm_json::to_string(&items)
                        .map_err(|e| unavailable(e.to_string()))?
                        .as_bytes(),
                    0o600,
                )
                .map_err(|e| unavailable(e.to_string()))?;
                child.arg("--passed-input-items").arg(item_file);
                crate::durable::write_atomic_durable(&events, b"", 0o600)
                    .map_err(|e| unavailable(e.to_string()))?;
                child
                    .arg("--checks")
                    .arg(missing.join(","))
                    .arg("--events")
                    .arg(&events);
                let log_path = self.step_log(Step::Checks);
                let facts = crate::check_record::RunFacts {
                    commit: tip,
                    tree: &tree,
                    seal: &input_seal,
                    executable: None,
                    log: &retained_log,
                    run: &run_id,
                    asked: &missing,
                    content: None,
                };
                crate::check_record::begin_run(&records, &facts).map_err(unavailable)?;
                crate::input_record::attempted(&records, base, &tree, &input_seal)
                    .map_err(unavailable)?;
                let (out, record) = self
                    .output_accounted(Step::Checks, &mut child)
                    .map_err(|e| unavailable(e.to_string()))?;
                let mut log = crate::durable::open_append(&self.step_log(Step::Checks), true)
                    .map_err(|e| unavailable(e.to_string()))?;
                log.write_all(&out.stdout)
                    .map_err(|e| unavailable(e.to_string()))?;
                log.write_all(&out.stderr)
                    .map_err(|e| unavailable(e.to_string()))?;
                writeln!(log, "{record}").map_err(|e| unavailable(e.to_string()))?;
                crate::durable::sync_all(&log).map_err(|e| unavailable(e.to_string()))?;
                std::fs::copy(&log_path, &retained_log).map_err(|e| unavailable(e.to_string()))?;
                let same_scanner = atpkg::prerequisite::configured(
                    &[atpkg::prerequisite::Prerequisite::Gitleaks],
                    "release input records",
                )
                .is_ok_and(|held| {
                    held.tool(atpkg::prerequisite::Prerequisite::Gitleaks)
                        .is_some_and(|now| {
                            now.tree_root() == scanner.tree_root() && now.validate().is_ok()
                        })
                });
                if !(self.build_inputs)().is_ok_and(|now| now == seal)
                    || !identity().is_ok_and(|now| now == (tree.clone(), input_seal.clone()))
                    || !same_scanner
                {
                    crate::check_record::abandon_run(&records, &facts).map_err(unavailable)?;
                    return Err(unavailable(
                        "inputs changed during release-input; no verdict retained".into(),
                    ));
                }
                let read = crate::check_record::events(&events).map_err(|e| e.to_string());
                crate::check_record::write_run(
                    &records,
                    &facts,
                    crate::check_record::RunEnd {
                        exit: out.status.code().unwrap_or(-1),
                        log_sha256: &crate::artifact::file_sha256(&retained_log)
                            .map_err(unavailable)?,
                        events: read.as_ref().map_err(String::as_str),
                    },
                )
                .map_err(unavailable)?;
                let text = format!(
                    "{}\n{}",
                    String::from_utf8_lossy(&out.stdout),
                    String::from_utf8_lossy(&out.stderr)
                );
                if out.status.success() {
                    Ok(text
                        .lines()
                        .rev()
                        .find(|line| line.starts_with("release input:"))
                        .unwrap_or("release input passed")
                        .to_string())
                } else if out.status.code() == Some(aterm_verify::exit::FAILED) {
                    Err(InputFinding::Failed(text))
                } else {
                    Err(InputFinding::Unanswered(text))
                }
            }
            Ok(out) => Err(unavailable(out.stderr_utf8())),
            Err(why) => Err(unavailable(why.to_string())),
        })()
    }

    fn branch_is_main(&mut self) -> bool {
        self.git_out(&["rev-parse", "--abbrev-ref", "HEAD"])
            .as_deref()
            == Some("main")
    }

    fn rev(&mut self, rev: &str) -> Option<String> {
        self.git_out(&["rev-parse", "--verify", "-q", rev])
    }

    fn version_at(&mut self, rev: &str) -> Option<String> {
        let manifest = self.git_out(&["show", &format!("{rev}:Cargo.toml")])?;
        crate::publish::workspace_version(&manifest).ok()
    }

    fn is_ancestor(&mut self, a: &str, b: &str) -> bool {
        !a.is_empty()
            && self
                .git()
                .git(&["merge-base", "--is-ancestor", a, b])
                .is_ok_and(|o| o.success())
    }

    fn origin_tag(&mut self, tag: &str) -> Option<String> {
        let want = format!("refs/tags/{tag}");
        let peeled = format!("{want}^{{}}");
        let mut plain = None;
        for line in self.origin_tags.lines() {
            let Some((sha, name)) = line.split_once('\t') else {
                continue;
            };
            if name == peeled {
                return Some(sha.to_string());
            }
            if name == want {
                plain = Some(sha.to_string());
            }
        }
        plain
    }

    fn origin_tag_now(&mut self, tag: &str) -> Result<Option<String>, String> {
        crate::github_remote::Remote::origin(&self.repo)?.tag(tag)
    }

    fn local_tag(&mut self, tag: &str) -> bool {
        self.rev(&format!("refs/tags/{tag}")).is_some()
    }

    fn ledger_versions(&mut self, rev: &str) -> Vec<String> {
        self.git_out(&["show", &format!("{rev}:RELEASES.ledger")])
            .unwrap_or_default()
            .lines()
            .filter(|l| l.starts_with(|c: char| c.is_ascii_digit()))
            .filter_map(|l| l.split_whitespace().nth(1).map(str::to_string))
            .collect()
    }

    fn engine_rows(&mut self, commit: Option<&str>) -> EngineRows {
        let text = self.engine_text("mappings.json").unwrap_or_default();
        if self
            .engine_synced
            .load(std::sync::atomic::Ordering::Acquire)
        {
            return engine_rows_in(&text, commit);
        }
        // Before its sync delivers them, the rows the release's engine holds unpushed are as
        // recorded as the origin's: a publish whose ledger push failed published its commit. Only
        // a copy naming the repository this release captured holds that repository's rows; one
        // naming another holds the other's, which its sync keeps there and never delivers here.
        let copy = aterm_verify::engine::engine_copy(&self.release);
        let captured = self.engine_repository.as_ref().is_ok_and(|captured| {
            crate::github_remote::origin_slug(&copy).is_ok_and(|named| &named == captured)
        });
        let unpushed = captured
            .then(|| crate::gates::unpushed_ledger(&copy, &|cmd| self.output_forwarded(cmd)))
            .flatten();
        let text = unpushed.map_or_else(
            || text.clone(),
            |unpushed| crate::gates::ledger_with_unpushed(&text, &unpushed),
        );
        engine_rows_in(&text, commit)
    }

    fn engine_now(&mut self, staged: &str) -> Result<EngineNow, String> {
        let e = self.engine.display().to_string();
        let head = self.engine_commit("HEAD")?;
        let origin = self.engine_commit("origin/main")?;
        let (Some(head), Some(origin)) = (head, origin) else {
            return Ok(EngineNow::Off(format!(
                "the engine {e} has no HEAD or no origin/main commit — pub stage and pub \
                 publish refuse such an engine"
            )));
        };
        if head != origin {
            return Ok(EngineNow::Off(format!(
                "the engine {e} is at {}, not its origin/main {} — pub stage and pub publish \
                 refuse such an engine",
                short(&head),
                short(&origin)
            )));
        }
        let Some(staged) = self.engine_commit(staged)? else {
            return Ok(EngineNow::Moved(format!(
                "the engine no longer holds the commit that staged ({}); its origin/main is {}",
                short(staged),
                short(&head)
            )));
        };
        if staged == head {
            return Ok(EngineNow::Same(format!(
                "the engine is the one that staged ({}, its origin/main)",
                short(&head)
            )));
        }
        let (from, to) = (staged.as_str(), head.as_str());
        let (code, out) = self.engine_git_code(&["merge-base", "--is-ancestor", from, to])?;
        match code {
            0 => {}
            1 => {
                return Ok(EngineNow::Moved(format!(
                    "the engine's origin/main {} does not contain the commit that staged ({}): \
                     its history was rewritten",
                    short(to),
                    short(from)
                )));
            }
            _ => {
                return Err(format!(
                    "git -C {e} merge-base --is-ancestor {} {} exited {code}: {}",
                    short(from),
                    short(to),
                    String::from_utf8_lossy(&out.stderr).trim()
                ));
            }
        }
        let (code, out) =
            self.engine_git_code(&["diff", "--no-renames", "--name-only", from, to])?;
        if code != 0 {
            return Err(format!(
                "git -C {e} diff {}..{} exited {code}: {}",
                short(from),
                short(to),
                String::from_utf8_lossy(&out.stderr).trim()
            ));
        }
        let listed = String::from_utf8_lossy(&out.stdout);
        let moved: Vec<&str> = listed.lines().filter(|p| engine_code(p)).collect();
        if moved.is_empty() {
            return Ok(EngineNow::Same(format!(
                "the engine at its origin/main {} has the code that staged ({}; only its \
                 records, Rust port or prose moved)",
                short(to),
                short(from)
            )));
        }
        let more = moved
            .len()
            .checked_sub(5)
            .filter(|n| *n > 0)
            .map_or_else(String::new, |n| format!(" and {n} more"));
        Ok(EngineNow::Moved(format!(
            "the engine's code moved since it staged: {}..{} (its origin/main) changed {}{more}",
            short(from),
            short(to),
            moved[..moved.len().min(5)].join(", ")
        )))
    }

    fn candidate_missing(&mut self) -> Result<Option<String>, String> {
        let path = self.repo.join("publish/.out/attestation.json");
        let text = crate::durable::read_text(&path)
            .map_err(|e| format!("cannot read {}: {e}", path.display()))?;
        let Some((attested, tree)) = attested_candidate(&text) else {
            return Ok(Some(format!(
                "{} names no qualified candidate (staging.attested_rev, export.tree)",
                path.display()
            )));
        };
        let reference = format!("refs/publication/candidates/{attested}");
        let out = self
            .git()
            .git(&["rev-parse", "--verify", "--quiet", &reference])
            .map_err(|e| e.to_string())?;
        if out.status == 1 {
            return Ok(Some(
                "the qualified staging candidate is not retained locally".into(),
            ));
        }
        if !out.success() {
            return Err(format!(
                "cannot read retained candidate: {}",
                String::from_utf8_lossy(&out.stderr).trim()
            ));
        }
        let retained_tree = self
            .git()
            .git(&["rev-parse", &format!("{reference}^{{tree}}")])
            .map_err(|e| e.to_string())?;
        if !retained_tree.success() {
            return Err("cannot read the retained candidate's tree".into());
        }
        Ok(
            (out.stdout_utf8().trim() != attested || retained_tree.stdout_utf8().trim() != tree)
                .then(|| {
                    "the retained candidate does not match its attested commit and tree".into()
                }),
        )
    }

    fn cut_journal_read(&mut self) -> Result<Option<CutState>, CutJournalUnread> {
        let header =
            crate::publish::JournalHeader::read_classified(&self.repo.join("dist/cut-state.toml"))
                .map_err(|unread| match unread {
                    crate::publish::JournalUnread::Read(e) => CutJournalUnread::Read(e.to_string()),
                    crate::publish::JournalUnread::Invalid(e) => {
                        CutJournalUnread::Invalid(e.to_string())
                    }
                })?;
        Ok(header.map(|header| CutState {
            finished: header.finished(),
            version: header.version,
            build: header.build_number,
        }))
    }

    fn is_dir(&mut self, path: &Path) -> bool {
        path.is_dir()
    }

    fn tree_has(&mut self, rev: &str, path: &str) -> Result<bool, Refusal> {
        Ok(crate::line::tree_has(&self.git(), rev, path)?)
    }

    fn fix_of(&mut self, line: &Line, rev: &str) -> Result<crate::line::Admitted, Refusal> {
        self.prefetch_source()?;
        let git = self.git();
        crate::line::git_ready(&git)?;
        Ok(crate::line::admit(&git, line, rev)?)
    }

    fn carry(&mut self, line: &Line, fixes: &[Fix]) -> Result<(Vec<Hop>, String), Refusal> {
        let git = self.git();
        crate::line::git_ready(&git)?;
        let fixes = crate::line::order(&git, fixes.to_vec())?;
        let hops = crate::line::carry(&git, line, &fixes)?;
        Ok((
            hops,
            "constructed locally; the frozen head judges before recording on main".into(),
        ))
    }

    fn record_line(&mut self, line: &Line) -> Result<String, Refusal> {
        let git = self.git();
        let head = line.head();
        let named: Vec<&str> = line.hops.iter().map(|hop| short(&hop.carries)).collect();
        let origin = crate::publish::native_origin(&self.repo).map_err(|why| {
            Refusal::new(
                "cannot bind the native source origin to this workspace",
                "restore the configured repository matching Cargo.toml before rerunning",
            )
            .detail([why.to_string()])
        })?;
        // A hop is the same commit however often it is made, so naming the fixes again makes
        // these hops and records them; a bare re-run resumes the journal, which names none yet.
        let recorded = crate::line::record(
            &git,
            &origin,
            &line.version,
            head,
            crate::line::MAX_RECORD_ATTEMPTS,
        )
        .map_err(|stop| {
            let first = if stop.reason == crate::line::Reason::Unreachable {
                "check the network and your git credentials, then run "
            } else {
                "run "
            };
            crate::line::Stop {
                remedy: format!(
                    "{first}{RELEASE_COMMAND} --fix {} again: it makes the same hop commits and \
                     records them on the tip it finds",
                    named.join(" ")
                ),
                ..stop
            }
        })?;
        let note = match recorded {
            crate::line::Recorded::Already { tip } => {
                format!("origin/main ({}) holds the head already", short(&tip))
            }
            crate::line::Recorded::Pushed { merge, attempt } => format!(
                "recorded on main by the merge {} (main's tree unchanged; push {attempt})",
                short(&merge)
            ),
        };
        Ok(note)
    }

    fn verify_line(&mut self, line: &Line) -> Result<String, Refusal> {
        Ok(crate::line::verify(&self.git(), line)?)
    }

    fn failure_hints(&mut self, line: &Line, evidence: &str, logs: &[String]) -> Vec<String> {
        use std::io::{BufRead, BufReader};
        // A qualify can outlive several main landings. Refresh only source tracking refs;
        // never select a new candidate or touch the publication engine for a hint.
        let refresh = (self.source_origin)(&self.repo).and_then(|origin| origin.fetch_origin());
        let mut evidence = evidence.to_owned();
        let paths = logs.iter().map(PathBuf::from).chain(
            ["tree", "build", "build-fixtures"].map(|branch| self.side_log(Step::Qualify, branch)),
        );
        let mut seen = std::collections::BTreeSet::new();
        let mut unread = Vec::new();
        for path in paths.filter(|path| seen.insert(path.clone())) {
            match std::fs::File::open(&path) {
                Ok(file) => {
                    for row in BufReader::new(file).lines() {
                        match row {
                            Ok(row)
                                if row.contains("FAIL")
                                    || row.contains("panicked at")
                                    || row.contains("error")
                                    || row.trim_start().starts_with("--> ") =>
                            {
                                evidence.push('\n');
                                evidence.push_str(&row);
                            }
                            Ok(_) => {}
                            Err(error) => {
                                unread.push(format!("hint evidence {}: {error}", path.display()));
                                break;
                            }
                        }
                    }
                }
                Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
                Err(error) => unread.push(format!("hint evidence {}: {error}", path.display())),
            }
        }
        let mut hints = crate::line::failure_hints(&self.git(), line, &evidence)
            .unwrap_or_else(|stop| vec![format!("main fix hints unavailable: {}", stop.why)]);
        if let Err(why) = refresh {
            hints.insert(
                0,
                format!("main hint refresh failed; using last fetched origin/main: {why}"),
            );
        }
        hints.extend(unread);
        hints
    }

    fn verify_pending_line(&mut self, line: &Line) -> Result<String, Refusal> {
        Ok(crate::line::verify_pending(&self.git(), line)?)
    }

    fn sync(&mut self) -> Result<String, Refusal> {
        let _timing =
            crate::cut_timing::Timer::start(&self.step_log(self.step), "engine-and-submodule-sync");
        self.engine_sync()?;
        self.engine_readiness()?;
        self.submodule_sync(Step::Sync)?;
        let head = self
            .engine_git(&["rev-parse", "--short", "HEAD"])
            .unwrap_or_default();
        Ok(format!(
            "the release's engine {} at {}, its origin/main; vendor/astream at its gitlink",
            self.engine.display(),
            head.trim()
        ))
    }

    fn preview_bump(&mut self, c: &Candidate, prover: &Prover) -> Result<String, Refusal> {
        let origin = crate::publish::native_origin(&self.repo).map_err(|e| {
            private_bump_unavailable(
                "private bump origin",
                &e.to_string(),
                "restore the configured repository matching Cargo.toml, then retry the dry run"
                    .into(),
            )
        })?;
        self.preview_bump_from_origin(c, prover, &origin)
    }

    fn bump(&mut self, from: &str, to: &str, prover: &Prover) -> Result<String, Refusal> {
        let _timing = crate::cut_timing::Timer::start(&self.step_log(self.step), "version-bump");
        let origin = crate::publish::native_origin(&self.repo).map_err(|why| {
            Refusal::new(
                "cannot bind the native source origin to this workspace",
                "restore the configured repository matching Cargo.toml before rerunning",
            )
            .detail([why.to_string()])
        })?;
        let undo = |s: &mut Self| {
            let _ = s.git().git(&["checkout", "-q", "--", "."]);
        };
        let journal = self.release.join("bump-claim.json");
        let tip = self
            .git_out(&["rev-parse", "origin/main"])
            .unwrap_or_default();
        let git = GitCli::new(&self.repo);
        let log = self.step_log(self.step);
        let bumped = crate::bump_claim::push(
            &git,
            &journal,
            tip,
            |tip| {
                self.main_to(tip)?;
                {
                    let _phase = crate::cut_timing::Timer::start(
                        &self.step_log(self.step),
                        "bump-submodule-sync",
                    );
                    self.submodule_sync(Step::Commit)?;
                }
                let cur = self.version_at("HEAD").unwrap_or_default();
                if cur != from {
                    return Err(Refusal::new(
                        format!(
                            "origin/main's version moved to {cur} while this release bumped {from}"
                        ),
                        format!(
                            "{RELEASE_COMMAND} again (without --bump when {cur} is unreleased)"
                        ),
                    ));
                }
                let (msg, declared) = self.edit_bump(from, to, prover, &origin)?;
                let mut commit = local_git()?;
                commit
                    .args(["commit", "-q", "-m", &msg, "--"])
                    .args(&declared)
                    .current_dir(&self.repo);
                let commit_timing =
                    crate::cut_timing::Timer::start(&self.step_log(self.step), "bump-git-commit");
                if self.logged(Step::Commit, &mut commit).code != 0 {
                    undo(self);
                    return Err(Refusal::new(
                        "could not commit the bump; it is undone",
                        "read the commit step's log",
                    ));
                }
                drop(commit_timing);
                let bumped = self.head();
                crate::bump_claim::record(&self.git(), &journal, &msg, declared)
                    .map_err(|why| cut_refusal("bump custody journal", &why))?;
                Ok(bumped)
            },
            |tip, bumped| {
                let _timing = crate::cut_timing::Timer::start(&log, "bump-native-ref-write");
                origin.write_ref("refs/heads/main", Some(tip), bumped)
            },
            || origin.refresh_main(),
        )?;
        self.submodule_sync(Step::Commit)?;
        Ok(bumped)
    }

    fn validation_tools(&mut self) -> Result<String, Refusal> {
        if self.warming.is_some() {
            self.finish_tools().map_err(|why| {
                Refusal::new(
                    format!("cannot finish the candidate's frozen tools: {why}"),
                    "restore the locked build and retry the release",
                )
            })?;
        }
        self.prepare_prerequisites()?;
        let held = self.prerequisites.as_ref().ok_or_else(|| {
            prerequisite_refusal("publication prerequisites were not acquired".into())
        })?;
        #[cfg(unix)]
        tap_bootstrap(&self.repo, &self.engine, held).map_err(|why| {
            Refusal::new(
                why.to_string(),
                "update the publication engine before starting the release",
            )
            .could_not_run()
        })?;
        #[cfg(not(unix))]
        let _ = held;
        let machine = aterm_verify::provision::this_machine(&self.repo).ok_or_else(|| {
            Refusal::new("cannot resolve validation tool cache", "set HOME and retry")
                .could_not_run()
        })?;
        let system = aterm_verify::provision::System::new(&self.repo);
        let mut lines = Vec::new();
        let code = aterm_verify::provision::run(
            &aterm_verify::provision::Options {
                release: true,
                ..Default::default()
            },
            &machine,
            &system,
            &mut |line| {
                println!("validation tools: {line}");
                lines.push(line.to_string());
            },
        );
        if code != 0 {
            return Err(Refusal::new(
                "validation prerequisites are unavailable; no product build or staging started",
                "run targo --unverified verify provision --release and retry the release",
            )
            .detail(lines)
            .could_not_run());
        }
        let explicit = std::env::var_os("ATPKG_KEYS");
        let helper = crate::source_signing::prepare_helper(
            &self.repo,
            explicit.as_deref(),
            &verify_environment(),
            |args, plan| {
                println!("validation tools: building this checkout's source-signing helper");
                (self.checkout_build)(self, "source-signing helper", args, plan)
            },
        )
        .map_err(|why| {
            Refusal::new(
                why,
                "restore the source-signing helper build and retry the release",
            )
        })?;
        self.prepared_source_helper = Some(helper);
        Ok("all pinned prerequisites and the source-signing helper are ready".into())
    }

    fn move_to(&mut self, commit: &str) -> Result<(), Refusal> {
        self.main_to(commit)?;
        self.submodule_sync(Step::Commit)?;
        match self.dirty() {
            Ok(d) if d.is_empty() => Ok(()),
            Ok(d) => Err(Refusal::new(
                format!("this clone is not clean at {}", short(commit)),
                "clean it (a submodule off its gitlink?), then re-run",
            )
            .detail(d.into_iter().take(5))),
            Err(why) => Err(Refusal::new(why, "run git status there, then re-run")),
        }
    }

    fn rewind_selection(&mut self, line: &Line) -> Result<(), Refusal> {
        if !self.branch_is_main() {
            return Err(Refusal::new(
                "the pending line is not checked out on main",
                "keep its journal; inspect the checkout and restore main to the recorded prefix",
            ));
        }
        crate::release_tools::clean_at(&self.repo, line.head())
            .map_err(|why| Refusal::new(why, "restore the selected clone and retry"))?;
        crate::line::verify_pending(&self.git(), line)?;
        let prefix = crate::line::recorded_prefix(&self.git(), line)?;
        // verify_pending proved every discarded hop is reproducible from its landed fix,
        // author/committer metadata and predecessor. A dirty or unrelated head never gets here;
        // the caller retains or explicitly removes the journal only after this reset succeeds.
        // The reset restores, it starts no work: the release's own stop (its fleet line lost,
        // which a refusal settles here) leaves it free to run; a person's signal refuses it.
        let mut cmd = local_git()?;
        cmd.current_dir(&self.repo)
            .args(["reset", "-q", "--keep", prefix.head()]);
        let ran = self.logged_settling(Step::Commit, &mut cmd);
        if ran.code != 0 {
            return Err(Refusal::new(
                "cannot rewind the locally generated candidate",
                "inspect the selection log and retry",
            )
            .detail(ran.detail));
        }
        Ok(())
    }

    fn own_cutter(&mut self, commit: &str) -> Result<String, crate::cutter::NotOwn> {
        crate::gates::own_frozen_cutter(commit).map_err(|mut not| {
            not.fix = self.cutter_retry();
            not
        })
    }

    fn frozen_commit(&mut self) -> Option<String> {
        match crate::cutter::running_stamp().ok()? {
            crate::cutter::Stamp::Proved(commit) => Some(commit.clone()),
            crate::cutter::Stamp::Built(_) => None,
        }
    }

    fn cutter_recovery(&mut self, commit: &str) -> crate::release_tools::Recovery {
        crate::release_tools::Recovery::read(commit)
    }

    fn exec_cutter(&mut self, commit: &str, prover: &Prover) -> Result<(), Refusal> {
        let retry = self.cutter_retry();
        let cannot = |why: String| {
            Refusal::new(
                format!("cannot exec R's frozen cutter ({why})"),
                retry.clone(),
            )
        };
        if interrupt::caught().is_some() {
            return Err(cannot("not started: the release was interrupted".into()));
        }
        let (cutter, prefrozen_inputs) = self
            .frozen_cutter_with_inputs(commit, prover)
            .map_err(cannot)?;
        let mut command = self.cutter_command_with_inputs(&cutter.tool.path, prefrozen_inputs);
        // The writer's identity is independent of the target it just built. A failed legacy
        // proof cannot supply it; the explicit re-freeze marker still bounds that repair.
        let freezer =
            crate::cutter::running_stamp().map_or("unproved", crate::cutter::Stamp::commit);
        crate::release_tools::Recovery::read(commit).hand(
            &mut command,
            commit,
            freezer,
            std::env::var_os(crate::release_tools::REFREEZE_ENV).as_deref(),
        );
        #[cfg(unix)]
        self.hand_exec_guards(&mut command, prover)
            .map_err(cannot)?;
        #[cfg(not(unix))]
        return Err(cannot("descriptor handover requires Unix".into()));
        // The fleet claim's sampler and publisher are Real's only persistent worker threads.
        // Join both before blocking this execing thread: a process-directed stop can then only
        // become pending on the thread whose mask the frozen image inherits. The same pid/start
        // reclaims the fleet line when the flow resumes in the new image; only the reader for an
        // already-running pre-B1 handed parent admits the old child-process boundary.
        drop(self.fleet_line.take());
        #[cfg(unix)]
        let _signals = interrupt::block_for_exec(&mut command).map_err(cannot)?;
        if interrupt::caught().is_some() {
            return Err(cannot("not started: the release was interrupted".into()));
        }
        let _timing = crate::cut_timing::Timer::start(&self.step_log(self.step), "cutter-exec");
        command.env(
            crate::cut_timing::EXEC_PID_ENV,
            std::process::id().to_string(),
        );
        aterm_verify::remote_host::release_before_exec().map_err(cannot)?;
        let error = command.exec();
        Err(cannot(error.to_string()))
    }

    fn warm_tools(&mut self, commit: &str, prover: &Prover) -> Result<(), Refusal> {
        if crate::release_tools::recorded(&self.release, commit).is_ok() {
            return Ok(());
        }
        let stdout = self.logs.join("verifier-build.jsonl");
        let stderr = self.logs.join("verifier-build.log");
        let start = || -> Result<WarmingVerifier, String> {
            let mut command = self.child(prover.bin.join("targo"), Some(prover));
            let plan = verify_plan(&self.repo)
                .map_err(|e| format!("select verifier build directory: {e}"))?;
            command
                .args(Self::frozen_tool_args(
                    &self.repo,
                    &FROZEN_VERIFY,
                    "test",
                    plan.build_dir(),
                ))
                .arg("--target-dir")
                .arg(plan.target())
                .current_dir(&self.repo)
                .stdin(Stdio::null())
                .stdout(std::fs::File::create(&stdout).map_err(|e| e.to_string())?)
                .stderr(std::fs::File::create(&stderr).map_err(|e| e.to_string())?);
            plan.apply_to_command(&mut command)
                .map_err(|e| format!("configure verifier build environment: {e}"))?;
            plan.prepare()
                .map_err(|e| format!("prepare verifier build directory: {e}"))?;
            let (child, forwarded) = self
                .spawn_forwarded(&mut command)
                .map_err(|e| e.to_string())?;
            Ok(WarmingVerifier {
                commit: commit.to_owned(),
                child,
                _forwarded: forwarded,
                stdout,
                stderr,
            })
        };
        self.warming =
            Some(start().map_err(|why| Refusal::new(why, "restore the verifier build and retry"))?);
        Ok(())
    }

    fn stop_tools(&mut self) {
        drop(self.warming.take());
    }

    fn facts(&mut self, c: &Candidate, prover: &Prover, artifact: Option<&Artifact>) -> Facts {
        let git = self.git();
        let built = built_answer(&self.repo, artifact, prover);
        // The tree's records need no bundle; the bundle's need its executable. The seal is the
        // run's, rechecked by stat, and read only once a record of this tree is there.
        let executable = artifact
            .filter(|_| built.yes)
            .map(|a| a.macho_sha256.as_str());
        // Bundle reuse is content evidence only; tree checks still judge the new tree.
        if let Some(artifact) = artifact.filter(|_| built.yes) {
            let reused = (|| -> Result<bool, String> {
                let root = crate::check_record::root(&git)?;
                let tree = crate::check_record::tree_of(&git, &c.commit)?;
                let seal = (self.build_inputs)()?;
                let content = crate::artifact::content_digest(&artifact.app)?;
                let Some(product) =
                    crate::check_record::product_key(&root, &tree, &seal, &content)?
                else {
                    return Ok(false);
                };
                let facts = crate::check_record::RunFacts {
                    commit: &c.commit,
                    tree: &tree,
                    seal: &seal,
                    executable: Some(&artifact.macho_sha256),
                    log: &self.logs,
                    run: "product-reuse",
                    asked: &[],
                    content: None,
                };
                crate::check_record::reuse_product(&root, &facts, &product)
            })();
            if matches!(reused, Ok(true)) {
                println!(
                    "    measure: product U, linked harnesses and seal unchanged; using recorded verdict"
                );
            }
        }
        let content_error = if self.previous_line_head(&c.commit).is_none() {
            None
        } else {
            match (self.build_inputs)().and_then(|seal| {
                let plan = self.check_content_plan(&c.commit, &seal, prover)?;
                self.carry_hop_checks(&c.commit, &seal, &plan)
            }) {
                Ok(carried) => {
                    for line in &carried.lines {
                        println!("    content record: {line}");
                    }
                    None
                }
                Err(why) => Some(why),
            }
        };
        if let Some(why) = content_error {
            println!("    content record: no cross-hop carry ({why})");
        }
        let checks = crate::check_record::checks(&git, &c.commit, executable, self.build_inputs)
            .unwrap_or_else(|e| {
                crate::check_record::Checks::unanswered(format!(
                    "the check records' judge did not answer: {e}"
                ))
            });
        let attestation =
            crate::durable::read_text(&self.repo.join("publish/.out/attestation.json")).ok();
        let (attested, staged_by) = attestation.as_deref().map_or((false, None), |t| {
            attestation_promotion_and_engine(t, &c.commit)
        });
        let fixtures = match crate::fixtures::on_commit(&git, "origin/main", &c.version) {
            Ok(Some(found)) => Answer::yes(format!(
                "v{}'s handoff fixtures are on main ({} desks)",
                c.version,
                found.desks.len()
            )),
            Ok(None) => Answer::no(format!("v{}'s handoff fixtures are not on main", c.version)),
            Err(e) => Answer::no(e.to_string()),
        };
        Facts {
            cut: self.cut_journal(),
            engine: self.engine_rows(Some(&c.commit)),
            attested,
            staged_by,
            built,
            checks,
            fixtures,
        }
    }

    fn disk(&mut self) -> (Option<u64>, bool) {
        let free = command(aterm_uds::os_path!("df"))
            .arg("-Pk")
            .arg(&self.repo)
            .output()
            .ok()
            .and_then(|o| {
                String::from_utf8_lossy(&o.stdout)
                    .lines()
                    .nth(1)
                    .and_then(|l| l.split_whitespace().nth(3))
                    .filter(|f| f.bytes().all(|b| b.is_ascii_digit()))
                    .and_then(|f| f.parse::<u64>().ok())
            })
            .map(|kib| kib / (1024 * 1024));
        let warm = self.repo.join("target/debug").is_dir();
        (free, warm)
    }

    fn disk_record(&mut self) -> aterm_capacity::Footprints {
        crate::release_disk::load(&crate::release_disk::record_in(&self.state))
    }

    fn claim_disk(&mut self, bytes: u64, why: &str) -> Result<(), Refusal> {
        let repo = self.repo.clone();
        self.disk_claim =
            crate::release_disk::claim(&repo, bytes, why, &mut |line| self.say(line))?;
        Ok(())
    }

    fn let_disk_go(&mut self, passed: bool) {
        let Some(claim) = self.disk_claim.take() else {
            return;
        };
        drop(claim);
        let wrote = crate::release_timeline::Timeline::of_logs(&self.logs)
            .ok()
            .and_then(|t| t.output_bytes())
            .filter(|&bytes| bytes > 0);
        let kept = match (passed, wrote) {
            (true, Some(bytes)) => {
                match crate::release_disk::keep(&crate::release_disk::record_in(&self.state), bytes)
                {
                    Ok(()) => format!(
                        "; this run wrote {} MiB, kept to size the next claim",
                        bytes >> 20
                    ),
                    Err(error) => format!("; what this run wrote could not be kept: {error}"),
                }
            }
            _ => String::new(),
        };
        self.say(&format!("        disk: the claim ended{kept}"));
    }

    fn can_check_commit(&mut self, c: &Candidate) -> bool {
        crate::gates::own_cutter(&self.git(), &c.commit).is_ok()
    }

    fn commit_gates(&mut self, c: &Candidate, dry: bool) -> Preflight {
        let origin = crate::publish::native_origin(&self.repo);
        self.commit_gates_from_origin(c, dry, origin)
    }
    fn cut_gates(
        &mut self,
        c: &Candidate,
        creds: &Path,
        kind: crate::publish::CutKind,
    ) -> Vec<Refusal> {
        let checkout = if self.scratch_logs {
            match self.candidate_checkout(&c.commit) {
                Ok(checkout) => Some(checkout),
                Err(why) => return vec![cut_refusal("R-tree cut preflight", &why).could_not_run()],
            }
        } else {
            None
        };
        let root = checkout
            .as_ref()
            .map_or(self.repo.as_path(), |(root, _)| root.as_path());
        crate::publish::pre_cut_gates(root, &c.version, creds, kind)
            .into_iter()
            .map(|(gate, why)| cut_refusal(gate, &why))
            .collect()
    }

    fn head_gates(&mut self, c: &Candidate) -> Vec<Refusal> {
        let mut found = Vec::new();
        let git = self.git();
        let origin = match crate::publish::native_origin(&self.repo) {
            Ok(origin) => origin,
            Err(error) => return vec![cut_refusal("native origin", &error.to_string())],
        };
        let (commit, version) = (c.commit.as_str(), c.version.as_str());
        if let Err(e) = crate::gates::handoff_fixture_gate_at(&git, &origin, commit, version) {
            found.push(cut_refusal("handoff fixtures", &e.to_string()));
        }
        if let Err(e) = crate::gates::handoff_policy_gate_at(&git, &origin, commit, version) {
            found.push(cut_refusal("handoff policy", &e.to_string()));
        }
        found
    }

    fn land_fixtures(
        &mut self,
        c: &Candidate,
        reserved: &Reservation,
        prover: &Prover,
    ) -> Result<String, Refusal> {
        self.land_fixtures_shared(c, reserved, prover)
    }
    fn audit(&mut self, machine_id: &str, profile: Option<&Path>, dry: bool) -> Vec<Finding> {
        crate::provision::audit(&self.repo, machine_id, profile, dry)
    }

    fn ask(&mut self, words: &AskWords) -> Result<String, String> {
        let line = format!(
            "{} tag=release {} -- {} -- {}",
            words.key, words.title, words.reason, words.step
        );
        notice(&["notice", "ask"], &line)
    }

    fn resolve(&mut self, key: &str) -> Result<String, String> {
        notice(&["notice", "done"], &format!("{key} ok"))
    }

    fn withdraw(&mut self, key: &str) -> Result<String, String> {
        notice(&["notice", "done"], &format!("{key} withdraw"))
    }

    fn repair(&mut self, failure: &aterm_release::repair::Failure) -> Vec<String> {
        let record = self.release.join(aterm_release::repair::RECORD_FILE);
        aterm_release::repair::open(
            &record,
            &self.repo,
            failure,
            &mut aterm_release::repair::Ctl,
        )
        .lines(&record)
    }

    fn qualify_slice_memory(&mut self) -> Result<crate::buildplan::SliceMemoryForecast, String> {
        crate::buildplan::slice_memory_forecast()
    }

    fn origin_tail(&mut self) -> Option<u64> {
        let text = self.git_out(&["show", "origin/main:RELEASES.ledger"])?;
        crate::ledger::tail(&text).ok().map(|r| r.build)
    }

    fn reserve(&mut self, c: &Candidate) -> Result<Reservation, Refusal> {
        let date = crate::changelog::today_la().map_err(|e| {
            Refusal::new(
                format!("cannot date the changelog's roll: {e}"),
                "check this machine's clock",
            )
        })?;
        let plan = crate::ledger::ClaimPlan {
            version: &c.version,
            now: crate::publish::unix_now(),
            allow_existing_section: false,
            max_attempts: crate::ledger::MAX_CLAIM_ATTEMPTS,
            source: &c.commit,
        };
        let version = c.version.clone();
        let origin = crate::publish::native_origin(&self.repo).map_err(|why| {
            Refusal::new(
                "cannot bind the native source origin to this workspace",
                "restore the configured repository matching Cargo.toml before rerunning",
            )
            .detail([why.to_string()])
        })?;
        crate::ledger::reserve(&self.git(), &origin, &plan, &|source, main| {
            crate::changelog::claim_changelogs(source, main, &version, &date)
        })
        .map(|r| Reservation {
            build: r.build,
            commit: r.commit,
            line: r.line,
        })
        .map_err(|e| {
            Refusal::new(
                format!("could not reserve the release's identity: {e}"),
                format!("fix what it names, then {RELEASE_COMMAND} (nothing was claimed)"),
            )
        })
    }

    fn stage(
        &mut self,
        c: &Candidate,
        prover: &Prover,
        target: StageTarget,
    ) -> Result<Ran, Refusal> {
        let _timing = crate::cut_timing::Timer::start(
            &self.side_log(Step::Qualify, "serial-stage"),
            "qualify serial export revalidation and staging",
        );
        self.scanner_tools = None;
        self.engine_sync()?;
        self.prepare_scanner_tools(prover)?;
        self.export_run(c, prover, target, Step::Publish, None)
    }

    fn qualify_begin(&mut self, prover: &Prover, branches: &[Branch]) -> Result<String, Refusal> {
        let _timing = crate::cut_timing::Timer::start(
            &self.side_log(Step::Qualify, "preparation"),
            "qualify preparation",
        );
        self.pool = None;
        if branches.contains(&Branch::Export) {
            // The export's engine and its scanner tools are made ready here, before any branch
            // starts, so every branch runs on `&self`.
            self.scanner_tools = None;
            self.engine_sync()?;
            self.prepare_scanner_tools(prover)?;
        }
        let path = self.logs.join("qualify.jobs");
        let _pool_timing = crate::cut_timing::Timer::start(
            &self.side_log(Step::Qualify, "admission"),
            "qualify core pool admission",
        );
        let pool = qualify_pool(&path, branches, |need| {
            aterm_capacity::admit(need, aterm_capacity::Admit::Wait)
        })?;
        let words = format!(
            "{} cores in one Release claim, isolated branch FIFOs (build: {}; tree: {}){}",
            pool.slots,
            pool.jobs.pool(false).path().display(),
            pool.jobs.pool(true).path().display(),
            if pool.export > 0 {
                format!(
                    ", {} of them the export's while it runs (CARGO_BUILD_JOBS={})",
                    pool.export, pool.export
                )
            } else {
                String::new()
            }
        );
        self.pool = Some(pool);
        Ok(words)
    }

    fn qualify_end(&mut self) {
        self.pool = None;
    }

    fn branches(&mut self) -> Box<dyn Branches + '_> {
        let real = &*self;
        let admission_barrier = real.pool.as_ref().map(|pool| {
            pool.jobs
                .reserve_initial()
                .expect("new qualify branch pools own every non-export token before branches start")
        });
        Box::new(RealBranches {
            real,
            admission_barrier: std::sync::Mutex::new(admission_barrier),
            tree_index: TreeIndexHandoff::default(),
        })
    }

    fn publish_begin(&mut self, prover: &Prover) -> Result<(), Refusal> {
        let _timing =
            crate::cut_timing::Timer::start(&self.step_log(Step::Publish), "publish preparation");
        self.scanner_tools = None;
        self.engine_sync()?;
        self.prepare_scanner_tools(prover)?;
        self.prepare_source_signing()
    }

    fn source_publication(&mut self, c: &Candidate, build: u64) -> Result<SourceState, Refusal> {
        let _timing = crate::cut_timing::Timer::start(
            &self.step_log(Step::Publish),
            "source publication binding",
        );
        let slug = self.slug();
        let tag = format!("v{}", c.version);
        let draft = crate::publish::journaled_draft(&self.repo.join("dist"), &c.version, build)
            .map_err(|why| {
                Refusal::new(
                    format!("the private draft journal of {tag} cannot be read: {why}"),
                    format!("run {RELEASE_COMMAND} again once dist/ is readable"),
                )
                .could_not_run()
            })?;
        let unread = |e: String| {
            Refusal::new(
                format!("could not read {slug} {tag} from GitHub's API ({e})"),
                format!("run {RELEASE_COMMAND} again once the channel answers"),
            )
            .could_not_run()
        };
        let path = source_release_path(&slug, &tag, draft);
        let release = match draft {
            // An id never comes back: a journaled draft GitHub does not answer was deleted, and
            // only a person can say so for certain (a retry reads the same 404).
            Some(id) => self
                .github_read_optional(&path)
                .map_err(unread)?
                .ok_or_else(|| {
                    Refusal::new(
                        format!(
                            "GitHub answers no release {id}, the private draft of {tag} \
                         dist/draft-state.toml journals"
                        ),
                        format!(
                            "once {slug}'s releases page shows no draft of {tag} this clone made, \
                         remove dist/draft-state.toml and dist/draft-bytes.toml, then \
                         {RELEASE_COMMAND} (its sign drafts afresh; nothing was claimed)"
                        ),
                    )
                })?,
            None => match self.github_read_optional(&path).map_err(unread)? {
                Some(release) => release,
                None => return Ok(SourceState::Pending(unjournaled_source(&tag))),
            },
        };
        judge_source_release(&release, &slug, &c.version, &self.repo, draft)
    }

    fn attest_draft(
        &mut self,
        c: &Candidate,
        build: u64,
        prover: &Prover,
    ) -> Result<String, Refusal> {
        let tag = format!("v{}", c.version);
        let id = crate::publish::journaled_draft(&self.repo.join("dist"), &c.version, build)
            .map_err(|why| {
                Refusal::new(
                    format!("the private draft journal of {tag} cannot be read: {why}"),
                    format!("run {RELEASE_COMMAND} again once dist/ is readable"),
                )
                .could_not_run()
            })?
            .ok_or_else(|| {
                Refusal::new(
                    format!(
                        "cut --sign passed, and dist/draft-state.toml names no private draft of \
                         {tag}: the source signing has no draft to attach R's attestation to, \
                         and nothing ran"
                    ),
                    format!("read the sign's log, then {RELEASE_COMMAND}"),
                )
            })?;
        self.engine_sync()?;
        self.prepare_source_signing()?;
        let state = crate::publish::draft_journal_path(&self.repo);
        let shown = crate::publish::sign_release_command(&self.repo, &c.version, Some(&state));
        let mut cmd = self.engine_child(&self.engine, Some(prover))?;
        cmd.args(crate::publish::sign_release_args(
            &self.repo,
            &c.version,
            Some(&state),
        ));
        if let Ok(mut log) = crate::durable::open_append(&self.step_log(Step::Publish), true) {
            let _ = writeln!(log, "$ {shown}");
        }
        let ran = self.logged(Step::Publish, &mut cmd);
        if ran.code == 0 {
            return Ok(format!(
                "{shown} attached R's source attestation to private draft {id}"
            ));
        }
        let refusal = Refusal::new(
            format!(
                "the source signing of {tag} onto private draft {id} exited {}: {}",
                ran.code,
                ran.detail.last().cloned().unwrap_or_default()
            ),
            format!(
                "read step 7's log; {RELEASE_COMMAND} signs alone and runs it again (or finish \
                 it by hand: {shown}); nothing was claimed"
            ),
        )
        .detail(ran.detail);
        Err(if ran.code == 130 {
            refusal.could_not_run()
        } else {
            refusal
        })
    }

    fn discard_signed(&mut self, version: &str, build: u64, reason: SignedDiscard) -> Discarded {
        crate::publish::discard_signed(&self.repo.join("dist"), version, build, reason)
    }
    fn cut(
        &mut self,
        args: &[String],
        prover: &Prover,
        candidate: &Candidate,
        reserved: Option<&Reservation>,
    ) -> Ran {
        let log = self.step_log(Step::Cut);
        let _wrapper = crate::cut_timing::Timer::start(&log, "cut wrapper total");
        match self.cut_command(args, prover) {
            Ok(mut cmd) => {
                let real = &*self;
                with_reader_prebuild(
                    Some(&log),
                    move |stop| {
                        let started = std::time::Instant::now();
                        let log = real.logs.join("main-reader-prebuild.log");
                        // Retain the proposed fixture tree's executable index, never
                        // a verdict. After-cut judges here if main's intervening
                        // commits leave the fixture edit untouched; otherwise it
                        // compiles and judges the new tip.
                        let result = reserved
                            .ok_or_else(|| "no reservation for fixture prebuild".to_string())
                            .and_then(|reserved| {
                                let pins = fixture_pins(candidate, reserved);
                                let root =
                                    fixture_root(&real.repo).map_err(|why| format!("{why:?}"))?;
                                let staged = crate::fixtures::staged(&root, &pins)
                                    .ok_or_else(|| "no staged fixtures for prebuild".to_string())?;
                                crate::fixtures::MainReader::new(&real.repo)
                                    .map_err(|e| e.to_string())
                                    .and_then(|mut reader| {
                                        reader
                                            .prebuild_fixtures(
                                                &prover.bin.join("targo"),
                                                "origin/main",
                                                &staged,
                                                &pins,
                                                &mut |cmd| {
                                                    with_reader_cargo(
                                                        real.pool
                                                            .as_ref()
                                                            .map(|pool| pool.jobs.pool(true)),
                                                        stop,
                                                        cmd,
                                                        |cmd| {
                                                            real.captured_with_stop(
                                                                Step::Cut,
                                                                prover,
                                                                &log,
                                                                cmd,
                                                                Some(stop),
                                                            )
                                                        },
                                                    )
                                                },
                                            )
                                            .map_err(|e| e.to_string())
                                    })
                            });
                        if let Ok(mut file) = crate::durable::open_append(&log, true) {
                            let _ = writeln!(
                                file,
                                "prebuild {:.3}s: {result:?}; no verdict",
                                started.elapsed().as_secs_f64()
                            );
                        }
                    },
                    || real.logged(Step::Cut, &mut cmd),
                )
            }
            Err(why) => Ran {
                code: -1,
                detail: vec![format!("not started: {why}")],
            },
        }
    }

    fn after_cut(&mut self, version: &str) -> Result<AfterCut, Refusal> {
        self.after_cut_shared(version)
    }

    fn after_cut_with_fixtures(
        &mut self,
        c: &Candidate,
        reserved: &Reservation,
        prover: &Prover,
    ) -> (Result<AfterCut, Refusal>, Result<String, Refusal>) {
        // FIXTURES-BEFORE-READER: land_fixtures_shared validates or stages the
        // reserved fixture set before starting the main reader. It reads no
        // site or tap state; step 8 has already made the release public.
        let log = self.step_log(Step::AfterCut);
        crate::cut_timing::independent_logged(
            Some(&log),
            ("after-cut distribution", || {
                self.after_cut_shared(&c.version)
            }),
            ("after-cut main reader", || {
                self.land_fixtures_shared(c, reserved, prover)
            }),
        )
    }
    fn latest(&mut self) -> Result<Option<String>, String> {
        let slug = self.slug();
        let release = self.github_read(&format!("repos/{slug}/releases/latest"))?;
        Ok(release["tag_name"].as_str().map(str::to_string))
    }
}

impl Real {
    fn land_fixtures_shared(
        &self,
        c: &Candidate,
        reserved: &Reservation,
        prover: &Prover,
    ) -> Result<String, Refusal> {
        let pins = fixture_pins(c, reserved);
        let again = || {
            format!(
                "run {RELEASE_COMMAND} again, which lands them on the tip it finds (or add them \
                 by hand: {}, \"By hand\")",
                crate::gates::HANDOFF_FIXTURE_README
            )
        };
        let git = self.git();
        let fetched = crate::publish::native_origin(&self.repo)
            .and_then(|native| native.refresh_main().map_err(crate::ledger::Error::new));
        if fetched.is_ok()
            && let Ok(Some(found)) = crate::fixtures::on_commit(&git, "origin/main", &c.version)
        {
            return Ok(crate::fixtures::Landed::Already {
                tip: self
                    .git_out(&["rev-parse", "origin/main"])
                    .unwrap_or_default(),
                desks: found.desks,
            }
            .words(&pins));
        }
        let root = fixture_root(&self.repo)?;
        let mut words = Vec::new();
        let staged = match crate::fixtures::staged(&root, &pins) {
            Some(staged) => staged,
            None => {
                let log = self.side_log(self.step, "fixtures");
                words.push(self.stage_fixtures_run(self.step, c, reserved, prover, &log)?);
                crate::fixtures::staged(&root, &pins).ok_or_else(|| {
                    Refusal::new(
                        format!(
                            "v{}'s handoff fixtures were written and are gone",
                            c.version
                        ),
                        again(),
                    )
                })?
            }
        };
        let targo = prover.bin.join("targo");
        let mut reader = crate::fixtures::MainReader::new(&self.repo);
        let log = self.side_log(self.step, "fixtures");
        let landed = crate::fixtures::land(
            &git,
            &crate::publish::native_origin(&self.repo).map_err(|why| {
                Refusal::new(
                    "cannot bind fixture landing to this workspace's native origin",
                    "restore the configured repository matching Cargo.toml before rerunning",
                )
                .detail([why.to_string()])
            })?,
            &staged,
            &pins,
            &mut |preview| match &mut reader {
                Ok(reader) => reader.read(&targo, preview, &pins, &mut |cmd| {
                    self.captured(self.step, prover, &log, cmd)
                }),
                Err(why) => crate::fixtures::Reading {
                    checked: None,
                    answers: Err(why.to_string()),
                    checks: format!("COULD NOT RUN: {why}"),
                },
            },
        )
        .map_err(|e| Refusal::new(e.to_string(), again()))?;
        words.push(landed.words(&pins));
        Ok(words.join("; "))
    }

    fn after_cut_shared(&self, version: &str) -> Result<AfterCut, Refusal> {
        let log = self.step_log(Step::AfterCut);
        let shape_timing = crate::cut_timing::Timer::start(&log, "after-cut public shape");
        let slug = self.slug();
        let tag = format!("v{version}");
        let read = |s: &Self, t: &str| s.release_json(&slug, t);
        let (release, site_tag) = crate::cut_timing::independent_logged(
            Some(&log),
            ("after-cut release and pointers", || {
                let release =
                    crate::cut_timing::measure("after-cut release JSON", || read(self, &tag))?;
                let pointer = crate::cut_timing::measure("after-cut latest pointer", || {
                    self.pointer_problems(&slug, &tag, version, &release)
                });
                Ok::<_, String>((release, pointer))
            }),
            ("after-cut site election", || self.site_tag(&slug)),
        );
        let (mut problems, pointer) = match release {
            Ok((rel, pointer)) => (release_shape(&rel, &slug, version), pointer),
            Err(why) => {
                return Err(Refusal::new(
                    why,
                    format!("{RELEASE_COMMAND} again when GitHub answers"),
                )
                .could_not_run());
            }
        };
        problems.extend(pointer_verdict(pointer)?);
        let site_tag = match site_tag {
            Ok(t) => t,
            Err(why) => {
                return Err(Refusal::new(
                    format!(
                        "{why}, so the release the site would link is unknown and cannot be \
                         shape-checked — the site was NOT touched"
                    ),
                    format!("{RELEASE_COMMAND} again when GitHub answers"),
                )
                .could_not_run());
            }
        };
        if site_tag != tag {
            match crate::cut_timing::measure("after-cut elected release JSON", || {
                read(self, &site_tag)
            }) {
                Ok(rel) => {
                    let v = site_tag.strip_prefix('v').unwrap_or(&site_tag);
                    problems.extend(
                        release_shape(&rel, &slug, v).into_iter().map(|p| {
                            format!("{site_tag} (the release alab.systems would link): {p}")
                        }),
                    );
                }
                Err(why) => {
                    return Err(Refusal::new(
                        why,
                        format!("{RELEASE_COMMAND} again when GitHub answers"),
                    )
                    .could_not_run());
                }
            }
        }
        let shape = if problems.is_empty() {
            Ok(())
        } else {
            Err(shape_refusal(&slug, &tag, problems))
        };
        drop(shape_timing);
        // PUBLIC-BEFORE-DISTRIBUTION: both consume the validated public cohort;
        // neither consumes the other's output. Join before recording completion.
        let (site, tap) = distribute_after_shape(
            &log,
            shape,
            || self.site(&slug),
            || self.align_tap(&site_tag),
        )?;
        Ok(AfterCut {
            note: format!(
                "{tag} carries the lean shape as GitHub serves it; {}; {tap}",
                site.note,
            ),
        })
    }
}

fn with_reader_cargo<T>(
    pool: Option<&aterm_capacity::Jobserver>,
    stop: &BranchStop,
    cmd: &mut Command,
    run: impl FnOnce(&mut Command) -> std::io::Result<T>,
) -> std::io::Result<T> {
    // This child is Cargo itself, so attach the GNU client and retain its
    // implicit token through capture and its pipe drains.
    let _implicit = pool
        .map(|pool| pool.attach_cargo(cmd, &|| stop.fired() || interrupt::caught().is_some()))
        .transpose()?;
    run(cmd)
}

fn with_reader_prebuild(
    log: Option<&Path>,
    prebuild: impl FnOnce(&BranchStop) + Send,
    cut: impl FnOnce() -> Ran,
) -> Ran {
    let stop = BranchStop::default();
    let timing = crate::cut_timing::Timer::parallel_parent();
    std::thread::scope(|scope| {
        let worker = scope.spawn(|| {
            let _prebuild = timing("cut fixture prebuild").with_log(log);
            prebuild(&stop)
        });
        let verdict = {
            let _child = timing("cut child and drains").with_log(log);
            cut()
        };
        if verdict.code != 0 {
            stop.fire();
        }
        let _join =
            log.map(|log| crate::cut_timing::Timer::start(log, "cut fixture prebuild join"));
        worker
            .join()
            .unwrap_or_else(|panic| std::panic::resume_unwind(panic));
        verdict
    })
}

fn notice(verb: &[&str], line: &str) -> Result<String, String> {
    let out = {
        command("aterm")
            .arg("ctl")
            .args(verb)
            .args(line.split(' '))
            .stdin(Stdio::null())
            .output()
    };
    match out {
        Ok(o) => {
            let reply = String::from_utf8_lossy(&o.stdout).trim().to_string();
            if o.status.success() && reply.starts_with("OK") {
                Ok(reply)
            } else {
                Err(format!(
                    "{}{}",
                    reply,
                    String::from_utf8_lossy(&o.stderr).trim()
                ))
            }
        }
        Err(e) => Err(format!("no aterm to ask ({e})")),
    }
}

/// The engine's ledger for one commit: its newest aterm row, whether `commit` has one, and that row's `release_rev`.
#[must_use]
pub fn engine_rows_in(mappings: &str, commit: Option<&str>) -> EngineRows {
    let Ok(value) = aterm_json::from_str::<aterm_json::Value>(mappings) else {
        return EngineRows::default();
    };
    let Some(rows) = value
        .get("mappings")
        .and_then(|m| m.get("aterm"))
        .and_then(aterm_json::Value::as_object)
    else {
        return EngineRows::default();
    };
    fn at(row: &aterm_json::Value) -> &str {
        row.get("verified_at")
            .and_then(aterm_json::Value::as_str)
            .unwrap_or("")
    }
    let revoked = crate::gates::revoked_rows(&value, "aterm");
    let live = |sha: &str| !revoked.is_some_and(|r| r.contains_key(sha));
    let newest = rows
        .iter()
        .filter(|(sha, _)| live(sha))
        .max_by_key(|(_, row)| at(row))
        .map(|(sha, _)| sha.clone());
    let mine = commit.filter(|c| live(c)).and_then(|c| rows.get(c));
    EngineRows {
        newest,
        has: mine.is_some(),
        rev: mine
            .and_then(|row| row.get("release_rev"))
            .and_then(aterm_json::Value::as_str)
            .map(str::to_string),
    }
}

/// A checked export of `commit`, including a full public clone check, its candidate tree and the checking engine.
#[must_use]
pub fn attestation_qualifies(json: &str, commit: &str) -> bool {
    let Ok(a) = aterm_json::from_str::<aterm_json::Value>(json) else {
        return false;
    };
    attestation_qualifies_in(&a, commit)
}

fn attestation_qualifies_in(a: &aterm_json::Value, commit: &str) -> bool {
    let source = a
        .get("source")
        .and_then(|s| s.get("commit"))
        .and_then(aterm_json::Value::as_str);
    let gate = a
        .get("guards")
        .and_then(|g| g.get("public_clone"))
        .and_then(aterm_json::Value::as_str);
    let mode = a.get("release_mode").and_then(aterm_json::Value::as_str);
    source == Some(commit)
        && (gate == Some("pass") || (gate == Some("skipped") && mode == Some("source-only")))
        && attested_engine_in(a).is_some()
        && a.get("staging")
            .and_then(|s| s.get("candidate_rev"))
            .and_then(aterm_json::Value::as_str)
            .is_some_and(|s| !s.is_empty())
        && a.get("export")
            .and_then(|e| e.get("tree"))
            .and_then(aterm_json::Value::as_str)
            .is_some_and(|s| !s.is_empty())
}

/// A checked export whose candidate was also attested on the remote staging tier.
#[cfg(test)]
#[must_use]
pub fn attestation_promotes(json: &str, commit: &str) -> bool {
    let Ok(a) = aterm_json::from_str::<aterm_json::Value>(json) else {
        return false;
    };
    attestation_promotes_in(&a, commit)
}

fn attestation_promotes_in(a: &aterm_json::Value, commit: &str) -> bool {
    if !attestation_qualifies_in(a, commit) {
        return false;
    }
    let stage = &a["staging"];
    stage["attested_rev"]
        .as_str()
        .is_some_and(|attested| Some(attested) == stage["candidate_rev"].as_str())
}

pub(crate) fn attestation_promotion_and_engine(json: &str, commit: &str) -> (bool, Option<String>) {
    let Ok(a) = aterm_json::from_str::<aterm_json::Value>(json) else {
        return (false, None);
    };
    if !attestation_promotes_in(&a, commit) {
        return (false, None);
    }
    (true, attested_engine_in(&a).map(str::to_string))
}

fn attested_engine_in(a: &aterm_json::Value) -> Option<&str> {
    a.get("engine")?
        .get("commit")?
        .as_str()
        .filter(|c| !c.is_empty())
}

/// The engine commit an attestation names (`engine.commit`): the engine that staged it.
#[cfg(test)]
#[must_use]
pub fn attested_engine(json: &str) -> Option<String> {
    let a = aterm_json::from_str::<aterm_json::Value>(json).ok()?;
    attested_engine_in(&a).map(str::to_string)
}

/// The immutable qualified commit and exported tree named by an attestation.
#[must_use]
pub fn attested_candidate(json: &str) -> Option<(String, String)> {
    let a = aterm_json::from_str::<aterm_json::Value>(json).ok()?;
    let revision = a.get("staging")?.get("attested_rev")?.as_str()?;
    let tree = a.get("export")?.get("tree")?.as_str()?;
    (!revision.is_empty() && !tree.is_empty()).then(|| (revision.into(), tree.into()))
}

#[cfg(unix)]
fn inherited_fd_number(name: &str, value: &std::ffi::OsStr) -> Result<std::os::fd::RawFd, String> {
    let text = value
        .to_str()
        .ok_or_else(|| format!("{name} is not a descriptor number"))?;
    let raw: std::os::fd::RawFd = text
        .parse()
        .map_err(|_| format!("{name} is not a descriptor number"))?;
    if raw < 3 {
        return Err(format!("{name} names a standard descriptor"));
    }
    Ok(raw)
}

#[cfg(unix)]
fn unique_inherited_fds(offers: [(&str, Option<std::os::fd::RawFd>); 3]) -> Result<(), String> {
    let mut seen = std::collections::BTreeMap::new();
    for (name, raw) in offers {
        let Some(raw) = raw else {
            continue;
        };
        if let Some(first) = seen.insert(raw, name) {
            return Err(format!(
                "{name} duplicates {first}'s inherited descriptor offer"
            ));
        }
    }
    Ok(())
}

#[cfg(unix)]
fn inherited_fd_offers(
    lease: Option<&std::ffi::OsStr>,
    line: Option<&std::ffi::OsStr>,
    writer: Option<&std::ffi::OsStr>,
) -> Result<[Option<std::os::fd::RawFd>; 3], String> {
    let lease = lease
        .map(|value| inherited_fd_number(TOOLCHAIN_LEASE_FD_ENV, value))
        .transpose()?;
    let line = line
        .map(|value| inherited_fd_number(LINE_FD_ENV, value))
        .transpose()?;
    let writer = writer
        .map(|value| inherited_fd_number(LINE_WRITER_FD_ENV, value))
        .transpose()?;
    // Validate the complete offer before any raw number becomes an OwnedFd. Otherwise one
    // malicious or stale number repeated in two variables would create two Rust owners.
    unique_inherited_fds([
        (TOOLCHAIN_LEASE_FD_ENV, lease),
        (LINE_FD_ENV, line),
        (LINE_WRITER_FD_ENV, writer),
    ])?;
    Ok([lease, line, writer])
}

#[cfg(unix)]
fn inherited_fd(name: &str, raw: std::os::fd::RawFd) -> Result<std::os::fd::OwnedFd, String> {
    use std::os::fd::FromRawFd as _;

    unsafe extern "C" {
        fn fcntl(fd: std::os::fd::RawFd, command: i32, ...) -> i32;
    }
    const F_GETFD: i32 = 1;
    const FD_CLOEXEC: i32 = 1;

    // SAFETY: F_GETFD only observes this startup offer. No threads exist yet that can close it.
    let flags = unsafe { fcntl(raw, F_GETFD) };
    if flags < 0 || flags & FD_CLOEXEC != 0 {
        return Err(format!("{name} is not a live inherited descriptor offer"));
    }
    // SAFETY: this process image received exclusive ownership of the explicitly inherited slot.
    Ok(unsafe { std::os::fd::OwnedFd::from_raw_fd(raw) })
}

#[cfg(unix)]
fn take_handover_env(name: &str) -> Option<std::ffi::OsString> {
    let value = std::env::var_os(name);
    // SAFETY: release startup is single-threaded, and these private variables are consumed once.
    unsafe { std::env::remove_var(name) };
    value
}

/// Adopt the two original resource descriptions handed through the native in-place exec.
/// Paths are derived from the receiver's selected store and account ledger; only bounded leaf
/// names and exact descriptor numbers cross the image boundary.
#[cfg(unix)]
fn adopt_exec_handoffs() -> Result<Option<aterm_capacity::Claim>, String> {
    let resource_offer = [
        TOOLCHAIN_LEASE_FD_ENV,
        TOOLCHAIN_LEASE_LEAF_ENV,
        LINE_FD_ENV,
        LINE_WRITER_FD_ENV,
        LINE_LEAF_ENV,
    ]
    .into_iter()
    .any(|name| std::env::var_os(name).is_some());
    if resource_offer && !interrupt::exec_handoff_installed() {
        return Err(
            "inherited resource offers arrived without a proved signal-mask handoff".into(),
        );
    }
    let lease_fd = take_handover_env(TOOLCHAIN_LEASE_FD_ENV);
    let lease_leaf = take_handover_env(TOOLCHAIN_LEASE_LEAF_ENV);
    let line_fd = take_handover_env(LINE_FD_ENV);
    let writer_fd = take_handover_env(LINE_WRITER_FD_ENV);
    let line_leaf = take_handover_env(LINE_LEAF_ENV);
    let [lease_fd, line_fd, writer_fd] = inherited_fd_offers(
        lease_fd.as_deref(),
        line_fd.as_deref(),
        writer_fd.as_deref(),
    )?;
    match (lease_fd, lease_leaf) {
        (None, None) => {}
        (Some(fd), Some(leaf)) => {
            let fd = inherited_fd(TOOLCHAIN_LEASE_FD_ENV, fd)?;
            let leaf = leaf
                .to_str()
                .filter(|leaf| !leaf.is_empty() && !leaf.contains('/'))
                .ok_or("the inherited toolchain lease leaf is invalid")?;
            let toolchain = crate::gates::trust_stage2_bin().map_err(|e| e.to_string())?;
            // SAFETY: inherited_fd established exclusive startup ownership; the receiver chose
            // the pinned toolchain independently, and adoption consumes the OwnedFd on every
            // answer, including a check after the lock boundary.
            unsafe { crate::gates::adopt_toolchain_lease(fd, &toolchain, leaf) }?;
        }
        _ => return Err("the inherited toolchain lease handover is incomplete".into()),
    }

    let (fd, leaf) = match (line_fd, line_leaf) {
        (Some(fd), Some(leaf)) => (fd, leaf),
        (None, None) if writer_fd.is_none() => return Ok(None),
        _ => return Err("the inherited release-line handover is incomplete".into()),
    };
    let fd = inherited_fd(LINE_FD_ENV, fd)?;
    let writer = writer_fd
        .map(|value| inherited_fd(LINE_WRITER_FD_ENV, value))
        .transpose()?;
    let leaf = leaf
        .to_str()
        .filter(|leaf| {
            leaf.len() <= 128
                && !leaf.is_empty()
                && !leaf.contains('/')
                && !leaf.contains("..")
                && leaf.ends_with(".claim")
        })
        .ok_or("the inherited release-line leaf is invalid")?;
    let ledger = aterm_capacity::Ledger::user()
        .ok_or("no account ledger exists for the inherited release-line claim")?;
    let path = ledger.dir().join(leaf);
    let claim = if let Some(writer) = writer {
        aterm_capacity::Claim::adopt_cohort(fd, writer, &path)
    } else {
        aterm_capacity::Claim::adopt(fd, &path)
    }
    .map_err(|e| format!("adopt inherited release-line claim: {e}"))?;
    Ok(Some(claim))
}

/// Preparation without publication work. Standalone callers use local origin/main;
/// the automatic worker refreshes it under admission before preparing its graph.
fn prefreeze(repo: &Path, opts: &crate::release::ReleaseOptions) -> Result<(), String> {
    #[cfg(not(unix))]
    {
        let _ = (repo, opts);
        Err("pre-freeze requires Unix process groups".into())
    }
    #[cfg(unix)]
    {
        if std::env::var_os(PREFREEZE_OBSERVER).is_some() {
            if std::env::var_os(AUTO_PREFREEZE).is_some()
                || std::env::var_os(PREFREEZE_WORKER).is_some()
            {
                return Err("the main observer cannot also be a pre-freeze worker".into());
            }
            let target = crate::github_remote::Remote::origin(repo)?.refresh_main()?;
            println!("{target}");
            return Ok(());
        }
        if let Ok(line) = std::env::var(AUTO_PREFREEZE) {
            return supervise_prefreeze(repo, &line);
        }
        // The capacity guard cancels its own dedicated group. The parent waits, so a
        // targo launcher never exits leaving an unowned compiler behind.
        // SAFETY: getpgrp only reads this process's group.
        if unsafe { libc::getpgrp() } != std::process::id() as i32 {
            let mut cmd = command(std::env::current_exe().map_err(|e| e.to_string())?);
            cmd.args(std::env::args_os().skip(1))
                .setsid(true)
                .default_signals()
                .tether();
            let status = cmd
                .spawn()
                .and_then(|mut child| child.wait())
                .map_err(|e| e.to_string())?;
            return if status.success() {
                Ok(())
            } else {
                Err(format!("worker exited {status}"))
            };
        }
        let mut real = Real::new(repo, opts.engine.as_deref(), false, opts.verbose)?;
        let tip = real
            .git_out(&["rev-parse", "origin/main"])
            .ok_or("cannot resolve local origin/main")?;
        let cutter = prepare_cached_cutter(&mut real, tip.trim())?;
        println!(
            "prefrozen: {} {}",
            cutter.commit,
            cutter.tool.path.display()
        );
        Ok(())
    }
}

#[cfg(unix)]
const AUTO_PREFREEZE: &str = "ATERM_RELEASE_AUTO_PREFREEZE";

/// Join cancellation using worker progress, never the supervisor's polling CPU.
#[cfg(unix)]
fn wait_prefreeze(
    mut holder: impl FnMut() -> Option<(u32, String, Option<u64>)>,
    mut now: impl FnMut() -> std::time::Instant,
    mut tick: impl FnMut(),
    mut interrupted: impl FnMut() -> bool,
    mut log: impl FnMut(&str),
) -> Result<(), String> {
    let began = now();
    let mut identity = None;
    let mut evidence = None;
    let mut last = began;
    while let Some((pid, started, progress)) = holder() {
        let at = now();
        if identity.as_ref() != Some(&(pid, started.clone())) {
            log(&format!(
                "waiting for cutter preparation pid {pid} to yield; hang detector: 60 s without descendant CPU/I/O progress"
            ));
            identity = Some((pid, started));
            evidence = None;
            last = at;
        }
        if interrupted() {
            return Err(format!(
                "interrupted while settling cutter preparation pid {pid}"
            ));
        }
        if progress.is_none() || evidence != progress {
            last = at;
        }
        evidence = progress;
        if at.saturating_duration_since(last) >= std::time::Duration::from_secs(60) {
            return Err(format!(
                "cutter preparation pid {pid} made no descendant CPU/I/O progress for 60 s while yielding"
            ));
        }
        tick();
    }
    if identity.is_some() {
        log(&format!(
            "cutter preparation yielded after {:.3} s; continuing frozen cutter preparation",
            now().saturating_duration_since(began).as_secs_f64()
        ));
    }
    Ok(())
}

#[cfg(unix)]
const PREFREEZE_WORKER: &str = "ATERM_RELEASE_PREFREEZE_WORKER";

#[cfg(unix)]
const PREFREEZE_OBSERVER: &str = "ATERM_RELEASE_PREFREEZE_OBSERVER";

#[cfg(unix)]
fn prepare_cached_cutter(
    real: &mut Real,
    tip: &str,
) -> Result<crate::release_tools::Cutter, String> {
    let tip = if let Some(target) = std::env::var_os(PREFREEZE_WORKER) {
        // The supervisor observed and imported this exact main through the typed native
        // boundary before it created a worker. Do not race that attribution with another
        // refresh after Real::new has already made a run directory.
        let target = target
            .into_string()
            .map_err(|_| "the pre-freeze target is not UTF-8")?;
        if !aterm_codec::hex::is_lower(&target, 40) {
            return Err("the pre-freeze target is not one full lowercase commit".into());
        }
        target
    } else {
        tip.to_owned()
    };
    let cache = cutter_cache_dir(&real.release);
    // The atomically published record binds an immutable, content-addressed executable.
    // Rechecking that binding and kept input seal needs no compiler checkout or lease.
    if let Some(cutter) = crate::release_tools::recorded_cutter(&cache, &tip)
        .ok()
        .filter(|cutter| real.cached_input_record(&cache.join("state"), cutter))
    {
        return Ok(cutter);
    }
    real.prefreeze_claim = Some(prefreeze_admission()?);
    std::fs::create_dir_all(&cache).map_err(|e| e.to_string())?;
    let pid = std::process::id();
    let started = started_of(pid).ok_or("cannot identify pre-freeze holder")?;
    let _lease = crate::release::RunClaim::take(
        &cache.join("workspace-running"),
        (pid, &started),
        &real.logs,
        &mut started_of,
    )
    .map_err(|why| format!("cutter workspace unavailable: {why:?}"))?;
    if let Some(cutter) = crate::release_tools::recorded_cutter(&cache, &tip)
        .ok()
        .filter(|cutter| real.cached_input_record(&cache.join("state"), cutter))
    {
        return Ok(cutter);
    }
    let mut compiler = real.cutter_workspace_at(&cache, &tip)?;
    compiler.prefreeze_claim = real.prefreeze_claim.take();
    // This standalone worker has not selected a prover yet. Resolve from the new
    // source's lock, independently of the ongoing cut's working directory and seal.
    std::env::set_current_dir(&compiler.repo).map_err(|e| e.to_string())?;
    crate::gates::arm_toolchain_lease();
    let prover = compiler.prover(None)?;
    let cutter = compiler.frozen_cutter_here(&tip, &prover)?;
    if !compiler.cached_input_record(&compiler.state, &cutter) {
        return Err("the pre-freeze produced no adoptable step-1 input seal".into());
    }
    Ok(cutter)
}

/// The supervisor is a separate, unreaped child until the run ends. Its reaped compiler
/// children therefore never enter the current cut's step counters.
#[cfg(unix)]
fn supervise_prefreeze(repo: &Path, line: &str) -> Result<(), String> {
    let ledger = aterm_capacity::Ledger::user().ok_or("capacity ledger unavailable")?;
    let executable = std::env::current_exe().map_err(|e| e.to_string())?;
    supervise_prefreeze_in(repo, line, &ledger, &executable)
}

#[cfg(unix)]
fn supervise_prefreeze_in(
    repo: &Path,
    line: &str,
    ledger: &aterm_capacity::Ledger,
    executable: &Path,
) -> Result<(), String> {
    interrupt::install();
    let cache = cutter_cache_dir(&aterm_verify::engine::release_dir(repo));
    std::fs::create_dir_all(&cache).map_err(|e| e.to_string())?;
    let pid = std::process::id();
    let started = started_of(pid).ok_or("cannot identify pre-freeze supervisor")?;
    let _owner = crate::release::RunClaim::take(
        &cache.join("prefreeze-running"),
        (pid, &started),
        &cache,
        &mut started_of,
    )
    .map_err(|why| format!("pre-freeze supervisor unavailable: {why:?}"))?;
    let mut runtime = LivePrefreezeSupervisor {
        repo,
        ledger,
        executable,
        cache: &cache,
    };
    supervise_prefreeze_loop(line, &mut runtime)
}

#[cfg(unix)]
#[derive(Debug)]
enum PrefreezeEvent {
    Started {
        target: String,
        pid: u32,
    },
    Finished {
        target: String,
        success: bool,
        exit_code: Option<i32>,
        signal: Option<i32>,
        status: String,
    },
    ObservationFailed {
        why: String,
    },
    YieldRequested {
        target: String,
        pid: u32,
        reason: String,
    },
    Yielded {
        target: String,
        reason: String,
        settle_s: f64,
    },
}

#[cfg(unix)]
#[derive(Clone, Debug)]
struct PrefreezeExit {
    success: bool,
    exit_code: Option<i32>,
    signal: Option<i32>,
    status: String,
}

#[cfg(unix)]
trait PrefreezeSupervisorRuntime {
    type Worker;
    type Observation;

    fn now(&mut self) -> std::time::Instant;
    fn interrupted(&mut self) -> bool;
    fn rows(&mut self) -> Result<Vec<aterm_capacity::Record>, String>;
    fn start_observation(&mut self) -> Result<Self::Observation, String>;
    fn poll_observation(
        &mut self,
        observation: &mut Self::Observation,
    ) -> Result<Option<String>, String>;
    fn settle_observation(&mut self, observation: &mut Self::Observation);
    fn prepared(&mut self, target: &str) -> bool;
    fn spawn(&mut self, target: &str, covered: bool) -> Result<Self::Worker, String>;
    fn worker_pid(&self, worker: &Self::Worker) -> u32;
    fn poll(&mut self, worker: &mut Self::Worker) -> Result<Option<PrefreezeExit>, String>;
    fn settle(&mut self, worker: &mut Self::Worker);
    fn event(&mut self, event: PrefreezeEvent);
    fn wait(&mut self);
}

#[cfg(unix)]
fn settle_main_observation<R: PrefreezeSupervisorRuntime>(
    runtime: &mut R,
    observation: &mut Option<R::Observation>,
) {
    if let Some(mut observation) = observation.take() {
        runtime.settle_observation(&mut observation);
    }
}

#[cfg(unix)]
struct ActivePrefreeze<W> {
    worker: W,
    target: String,
    covered: bool,
}

#[cfg(unix)]
fn settle_prefreeze<R: PrefreezeSupervisorRuntime>(
    runtime: &mut R,
    active: &mut Option<ActivePrefreeze<R::Worker>>,
    reason: &str,
) {
    let Some(mut active) = active.take() else {
        return;
    };
    let pid = runtime.worker_pid(&active.worker);
    if let Ok(Some(exit)) = runtime.poll(&mut active.worker) {
        runtime.event(PrefreezeEvent::Finished {
            target: active.target,
            success: exit.success,
            exit_code: exit.exit_code,
            signal: exit.signal,
            status: exit.status,
        });
        return;
    }
    runtime.event(PrefreezeEvent::YieldRequested {
        target: active.target.clone(),
        pid,
        reason: reason.to_owned(),
    });
    let began = runtime.now();
    runtime.settle(&mut active.worker);
    let settle_s = runtime.now().saturating_duration_since(began).as_secs_f64();
    runtime.event(PrefreezeEvent::Yielded {
        target: active.target,
        reason: reason.to_owned(),
        settle_s,
    });
}

/// Follow authenticated main observations, starting at most one preparation for each observed
/// head. A completed, failed, killed or yielded attempt remains the stated outcome for that head;
/// only a different main object can create another worker and therefore another run directory.
#[cfg(unix)]
fn supervise_prefreeze_loop<R: PrefreezeSupervisorRuntime>(
    line: &str,
    runtime: &mut R,
) -> Result<(), String> {
    let mut active: Option<ActivePrefreeze<R::Worker>> = None;
    let mut observation: Option<R::Observation> = None;
    let mut attempted = None;
    let mut next = runtime.now();
    loop {
        if runtime.interrupted() {
            settle_prefreeze(runtime, &mut active, "supervisor exit");
            settle_main_observation(runtime, &mut observation);
            return Ok(());
        }
        let rows = match runtime.rows() {
            Ok(rows) => rows,
            Err(why) => {
                settle_prefreeze(runtime, &mut active, "capacity ledger read failed");
                settle_main_observation(runtime, &mut observation);
                return Err(why);
            }
        };

        // Reap before attributing any simultaneous traffic row. A worker which has already
        // exited (including by a capacity-guard signal) finished with that status; it was not
        // yielded by a row the supervisor happened to observe in the same tick.
        if let Some(worker) = active.as_mut() {
            match runtime.poll(&mut worker.worker) {
                Ok(Some(exit)) => {
                    let Some(finished) = active.take() else {
                        return Err("the finished pre-freeze worker was lost".into());
                    };
                    runtime.event(PrefreezeEvent::Finished {
                        target: finished.target,
                        success: exit.success,
                        exit_code: exit.exit_code,
                        signal: exit.signal,
                        status: exit.status,
                    });
                    next = runtime.now() + PREFREEZE_REFRESH;
                }
                Ok(None) => {}
                Err(why) => {
                    settle_prefreeze(runtime, &mut active, "worker status read failed");
                    return Err(why);
                }
            }
        }

        let mut observed = None;
        if let Some(refresh) = observation.as_mut() {
            match runtime.poll_observation(refresh) {
                Ok(Some(target)) => {
                    observation.take();
                    observed = Some(target);
                }
                Ok(None) => {}
                Err(why) => {
                    observation.take();
                    runtime.event(PrefreezeEvent::ObservationFailed { why });
                    next = runtime.now() + PREFREEZE_REFRESH;
                }
            }
        }

        let covered = rows.iter().any(|row| row.cover_marker() == line);
        if active.as_ref().is_some_and(|worker| worker.covered) && !covered {
            // A worker admitted beside the old line must not retain Release rank
            // between cuts. The event is terminal for this head; another head gets
            // ordinary Background admission on its first and only attempt.
            settle_prefreeze(runtime, &mut active, "old line ended");
            next = runtime.now();
        }
        let own = active
            .as_ref()
            .map(|worker| runtime.worker_pid(&worker.worker));
        let state = prefreeze_traffic(&rows, line, own);
        if state != PrefreezeTraffic::Free {
            let reason = if state == PrefreezeTraffic::End {
                "next line claimed"
            } else {
                "Release stage or timing claim"
            };
            settle_prefreeze(runtime, &mut active, reason);
            settle_main_observation(runtime, &mut observation);
            if state == PrefreezeTraffic::End {
                return Ok(());
            }
            next = runtime.now();
        } else if active.is_none() {
            if let Some(target) = observed {
                if attempted.as_deref() == Some(target.as_str()) || runtime.prepared(&target) {
                    attempted = Some(target);
                    next = runtime.now() + PREFREEZE_REFRESH;
                } else {
                    // Record validation can take time. The worker's admission remains the final
                    // authority, but this last read avoids creating its Real/log directory for
                    // known traffic which arrived after the authenticated observation.
                    let latest = runtime.rows()?;
                    let latest_covered = latest.iter().any(|row| row.cover_marker() == line);
                    match prefreeze_traffic(&latest, line, None) {
                        PrefreezeTraffic::End => return Ok(()),
                        PrefreezeTraffic::Yield => {
                            next = runtime.now();
                        }
                        PrefreezeTraffic::Free => {
                            let worker = runtime.spawn(&target, latest_covered)?;
                            let pid = runtime.worker_pid(&worker);
                            runtime.event(PrefreezeEvent::Started {
                                target: target.clone(),
                                pid,
                            });
                            attempted = Some(target.clone());
                            active = Some(ActivePrefreeze {
                                worker,
                                target,
                                covered: latest_covered,
                            });
                        }
                    }
                }
            } else if observation.is_none() && prefreeze_due(runtime.now(), next) {
                match runtime.start_observation() {
                    Ok(refresh) => observation = Some(refresh),
                    Err(why) => {
                        runtime.event(PrefreezeEvent::ObservationFailed { why });
                        next = runtime.now() + PREFREEZE_REFRESH;
                    }
                }
            }
        }
        runtime.wait();
    }
}

#[cfg(unix)]
struct LivePrefreezeWorker(Option<aterm_uds::spawnfd::Child>);

#[cfg(unix)]
impl Drop for LivePrefreezeWorker {
    fn drop(&mut self) {
        if let Some(mut child) = self.0.take() {
            aterm_uds::deadline::settle_child(&mut child);
        }
    }
}

#[cfg(unix)]
struct LiveMainObservation {
    child: Option<aterm_uds::spawnfd::Child>,
    stdout: Option<std::thread::JoinHandle<Vec<u8>>>,
    stderr: Option<std::thread::JoinHandle<Vec<u8>>>,
}

#[cfg(unix)]
impl LiveMainObservation {
    fn drain(pipe: Option<Box<dyn std::io::Read + Send>>) -> std::thread::JoinHandle<Vec<u8>> {
        std::thread::spawn(move || {
            let mut bytes = Vec::new();
            if let Some(mut pipe) = pipe {
                let _ = pipe.read_to_end(&mut bytes);
            }
            bytes
        })
    }

    fn join(&mut self) -> Result<(Vec<u8>, Vec<u8>), String> {
        let join = |handle: &mut Option<std::thread::JoinHandle<Vec<u8>>>| {
            handle
                .take()
                .ok_or("the main observer lost an output drain")?
                .join()
                .map_err(|_| "the main observer's output drain panicked".to_owned())
        };
        Ok((join(&mut self.stdout)?, join(&mut self.stderr)?))
    }

    fn settle(&mut self) {
        if let Some(mut child) = self.child.take() {
            aterm_uds::deadline::settle_child(&mut child);
        }
        if let Some(handle) = self.stdout.take() {
            let _ = handle.join();
        }
        if let Some(handle) = self.stderr.take() {
            let _ = handle.join();
        }
    }
}

#[cfg(unix)]
impl Drop for LiveMainObservation {
    fn drop(&mut self) {
        self.settle();
    }
}

#[cfg(unix)]
struct LivePrefreezeSupervisor<'a> {
    repo: &'a Path,
    ledger: &'a aterm_capacity::Ledger,
    executable: &'a Path,
    cache: &'a Path,
}

#[cfg(unix)]
impl PrefreezeSupervisorRuntime for LivePrefreezeSupervisor<'_> {
    type Worker = LivePrefreezeWorker;
    type Observation = LiveMainObservation;

    fn now(&mut self) -> std::time::Instant {
        std::time::Instant::now()
    }

    fn interrupted(&mut self) -> bool {
        interrupt::caught().is_some()
    }

    fn rows(&mut self) -> Result<Vec<aterm_capacity::Record>, String> {
        self.ledger.list().map_err(|error| error.to_string())
    }

    fn start_observation(&mut self) -> Result<Self::Observation, String> {
        let mut cmd = command(self.executable);
        cmd.args(["release", "--prefreeze"])
            .current_dir(self.repo)
            .env_remove(AUTO_PREFREEZE)
            .env_remove(PREFREEZE_WORKER)
            .env(PREFREEZE_OBSERVER, "1")
            .env_remove(aterm_capacity::RELEASE_RUN_ENV)
            .setsid(true)
            .default_signals()
            .enter_background_band()
            .tether()
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped());
        let mut child = cmd.spawn().map_err(|error| error.to_string())?;
        let stdout =
            LiveMainObservation::drain(child.stdout.take().map(|pipe| Box::new(pipe) as Box<_>));
        let stderr =
            LiveMainObservation::drain(child.stderr.take().map(|pipe| Box::new(pipe) as Box<_>));
        Ok(LiveMainObservation {
            child: Some(child),
            stdout: Some(stdout),
            stderr: Some(stderr),
        })
    }

    fn poll_observation(
        &mut self,
        observation: &mut Self::Observation,
    ) -> Result<Option<String>, String> {
        let Some(status) = observation
            .child
            .as_mut()
            .ok_or("the active main observer lost its child")?
            .try_wait()
            .map_err(|error| error.to_string())?
        else {
            return Ok(None);
        };
        let _ = observation.child.take();
        let (stdout, stderr) = observation.join()?;
        if !status.success() {
            return Err(format!(
                "authenticated main observer exited {status}: {}",
                String::from_utf8_lossy(&stderr).trim()
            ));
        }
        let target = String::from_utf8(stdout)
            .map_err(|_| "authenticated main observer wrote non-UTF8 output")?;
        let target = target.trim();
        if !aterm_codec::hex::is_lower(target, 40) {
            return Err("authenticated main observer returned no full lowercase commit".into());
        }
        Ok(Some(target.to_owned()))
    }

    fn settle_observation(&mut self, observation: &mut Self::Observation) {
        observation.settle();
    }

    fn prepared(&mut self, target: &str) -> bool {
        let Ok(cutter) = crate::release_tools::recorded_cutter(self.cache, target) else {
            return false;
        };
        if cfg!(target_os = "macos") {
            cutter.inputs.as_ref().is_some_and(|inputs| {
                crate::build_inputs::prefrozen_seal(&self.cache.join("state"), &inputs.seal)
                    .is_some()
            })
        } else {
            cutter.inputs.is_none()
        }
    }

    fn spawn(&mut self, target: &str, covered: bool) -> Result<Self::Worker, String> {
        let mut cmd = command(self.executable);
        cmd.args(["release", "--prefreeze"])
            .current_dir(self.repo)
            .env_remove(AUTO_PREFREEZE)
            .env_remove(PREFREEZE_OBSERVER)
            .env(PREFREEZE_WORKER, target)
            .setsid(true)
            .default_signals()
            .enter_background_band()
            .tether();
        if covered {
            // The original line permits preparation beside its untimed tail.
            cmd.env(aterm_capacity::RELEASE_RUN_ENV, "1");
        } else {
            // Between cuts this is ordinary cancellable Background work.
            cmd.env_remove(aterm_capacity::RELEASE_RUN_ENV);
        }
        cmd.spawn()
            .map(|child| LivePrefreezeWorker(Some(child)))
            .map_err(|error| error.to_string())
    }

    fn worker_pid(&self, worker: &Self::Worker) -> u32 {
        worker
            .0
            .as_ref()
            .expect("an active pre-freeze worker owns its child")
            .id()
    }

    fn poll(&mut self, worker: &mut Self::Worker) -> Result<Option<PrefreezeExit>, String> {
        use std::os::unix::process::ExitStatusExt as _;

        let status = worker
            .0
            .as_mut()
            .ok_or("the active pre-freeze worker lost its child")?
            .try_wait()
            .map_err(|error| error.to_string())?;
        let exit = status.map(|status| PrefreezeExit {
            success: status.success(),
            exit_code: status.code(),
            signal: status.signal(),
            status: status.to_string(),
        });
        if exit.is_some() {
            let _ = worker.0.take();
        }
        Ok(exit)
    }

    fn settle(&mut self, worker: &mut Self::Worker) {
        if let Some(mut child) = worker.0.take() {
            aterm_uds::deadline::settle_child(&mut child);
        }
    }

    fn event(&mut self, event: PrefreezeEvent) {
        let event = match event {
            PrefreezeEvent::Started { target, pid } => aterm_json::json!({
                "schema":1,"event":"prefreeze-attempt-started","target":target,"worker_pid":pid,
            }),
            PrefreezeEvent::Finished {
                target,
                success,
                exit_code,
                signal,
                status,
            } => aterm_json::json!({
                "schema":1,"event":"prefreeze-finished","target":target,
                "success":success,"exit_code":exit_code,"signal":signal,"status":status,
            }),
            PrefreezeEvent::ObservationFailed { why } => aterm_json::json!({
                "schema":1,"event":"prefreeze-observation-failed","why":why,
            }),
            PrefreezeEvent::YieldRequested {
                target,
                pid,
                reason,
            } => aterm_json::json!({
                "schema":1,"event":"prefreeze-yield-requested","target":target,
                "worker_pid":pid,"reason":reason,
            }),
            PrefreezeEvent::Yielded {
                target,
                reason,
                settle_s,
            } => aterm_json::json!({
                "schema":1,"event":"prefreeze-yielded","target":target,
                "reason":reason,"settle_s":settle_s,
            }),
        };
        println!("release-event: {event}");
    }

    fn wait(&mut self) {
        std::thread::sleep(std::time::Duration::from_millis(20));
    }
}

#[cfg(unix)]
const PREFREEZE_REFRESH: std::time::Duration = std::time::Duration::from_secs(30);

#[cfg(unix)]
fn prefreeze_due(now: std::time::Instant, next: std::time::Instant) -> bool {
    now >= next
}

#[cfg(unix)]
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum PrefreezeTraffic {
    Free,
    Yield,
    End,
}

#[cfg(unix)]
fn prefreeze_traffic(
    rows: &[aterm_capacity::Record],
    line: &str,
    worker: Option<u32>,
) -> PrefreezeTraffic {
    let owner = rows.iter().find(|row| row.cover_marker() == line);
    let mut result = PrefreezeTraffic::Free;
    for row in rows {
        if row.cover_marker() == line || Some(row.pid) == worker {
            continue;
        }
        if row.need.rank == aterm_capacity::Rank::Release {
            if row.need.what == owner.map_or("release line", |row| row.need.what.as_str())
                && row.need.mem == 0
                && row.need.cores == 0
                && row.need.disk == 0
                && row.need.quiet.is_none()
            {
                return PrefreezeTraffic::End;
            }
            result = PrefreezeTraffic::Yield;
        } else if row.need.quiet.is_some() {
            result = PrefreezeTraffic::Yield;
        }
    }
    result
}

/// Admit before source synchronization, so Git's work excludes windowed/timed checks too.
#[cfg(unix)]
fn prefreeze_admission() -> Result<aterm_capacity::Claim, String> {
    if std::env::var_os(aterm_capacity::RELEASE_RUN_ENV).is_some()
        && std::env::var_os(PREFREEZE_WORKER).is_none()
    {
        return Err("pre-freeze must run outside a cut".into());
    }
    // Darwin's published PRIO_DARWIN_PROCESS and PRIO_DARWIN_BG apply the
    // process-wide background band, inherited by the compiler's descendants.
    #[cfg(target_os = "macos")]
    // SAFETY: constant selectors change only this process's scheduling policy.
    if unsafe { libc::setpriority(4, 0, 0x1000) } != 0 {
        return Err(std::io::Error::last_os_error().to_string());
    }
    interrupt::install();
    // The quiet resource excludes windowed/timed checks. Its load ceiling is
    // deliberately unbounded: this is exclusion, not a quiet-machine measurement.
    aterm_capacity::admit(prefreeze_need(), aterm_capacity::Admit::Preemptible)
        .map_err(|e| e.to_string())
}

#[cfg(unix)]
fn prefreeze_need() -> aterm_capacity::Need {
    aterm_capacity::Need::new("release cutter pre-freeze")
        .rank(aterm_capacity::Rank::Background)
        .mem_gib(26)
        .cores(aterm_capacity::cores())
        .covers()
        .quiet(f64::MAX)
}

fn configure_prefreeze_child(cmd: &mut Command, cover: &str, cores: u32) {
    cmd.env_remove(aterm_capacity::RELEASE_RUN_ENV)
        .env(aterm_capacity::COVER_ENV, cover)
        .env("BUILD_SLOT_HELD", "1")
        .env("CARGO_BUILD_JOBS", cores.to_string());
}

/// Run a release in the clone at `repo`; the exit code.
#[must_use]
pub fn run(repo: &Path, opts: &crate::release::ReleaseOptions) -> i32 {
    if opts.prefreeze {
        return match prefreeze(repo, opts) {
            Ok(()) => 0,
            Err(why) => {
                eprintln!(
                    "release-event: {}",
                    aterm_json::json!({
                        "schema":1,"event":"prefreeze-unavailable","why":why,
                    })
                );
                eprintln!("release prefreeze: {why}");
                2
            }
        };
    }
    #[cfg(unix)]
    let inherited_line = match adopt_exec_handoffs() {
        Ok(line) => line,
        Err(why) => {
            eprintln!("release handover: {why}");
            return 2;
        }
    };
    #[cfg(not(unix))]
    let inherited_line = None;
    // Retirement is cleanup: `retire` keeps a target it cannot certify clear, so one it could not
    // retire is stated once Real exists and the release goes on.
    let retirement = matches!(
        crate::cutter::running_stamp(),
        Ok(crate::cutter::Stamp::Proved(_))
    )
    .then(|| crate::release_target::retire(repo));
    interrupt::install();
    // Dry judgments need the same clone-local compiler handoff as a cut. This
    // capacity claim settles the old supervisor; it reserves no publication state.
    let line = if inherited_line.is_none() {
        match aterm_capacity::admit(
            aterm_capacity::Need::new("release line").rank(aterm_capacity::Rank::Release),
            aterm_capacity::Admit::Wait,
        ) {
            Ok(claim) => Some(claim),
            Err(error) => {
                eprintln!("release line: {error}");
                return 2;
            }
        }
    } else {
        inherited_line
    };
    if !opts.dry_run {
        crate::gates::arm_toolchain_lease();
    }
    let opts = match std::env::current_dir() {
        Ok(cwd) => opts.anchored(&cwd),
        Err(_) => opts.clone(),
    };
    let mut real = match Real::new(repo, opts.engine.as_deref(), opts.dry_run, opts.verbose) {
        Ok(real) => real,
        Err(why) => {
            eprintln!("release: {why}");
            return 2;
        }
    };
    match retirement {
        Some(Ok(entries)) => {
            real.say(&format!(
                "release: retired the legacy in-tree target; deps entries remaining: {entries}"
            ));
            real.say(&format!(
                "release-event: {}",
                aterm_json::json!({
                    "schema": 1,
                    "event": "target-retired",
                    "deps_entries": entries,
                })
            ));
        }
        Some(Err(why)) => real.say(&format!(
            "release: kept the legacy in-tree target ({why}); the release goes on, and a later \
             cut retires it"
        )),
        None => {}
    }
    real.line_priority = line;
    real.argv = opts.dispatch_args(std::env::args_os().skip(1).collect());
    let home = real.home.clone();
    crate::release::run_with(&mut real, &opts, &home).code
}

/// What `ship check` asks: the commit, engine, selected sections and optional stable compile.
/// With no section named, it asks cut, export and wasm32; compat is explicit.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct CheckOptions {
    pub commit: Option<String>,
    pub engine: Option<PathBuf>,
    pub cut: bool,
    pub export: bool,
    pub wasm32: bool,
    /// The exact stable x86_64 compile check used before staging.
    pub compat: bool,
    /// With `--cut`, ask only commit gates; bundle provenance belongs to the cut.
    pub source: bool,
}

impl CheckOptions {
    /// The sections asked: `(cut, export, wasm32)`.
    #[must_use]
    pub const fn sections(&self) -> (bool, bool, bool) {
        if self.cut || self.export || self.wasm32 || self.compat {
            (self.cut, self.export, self.wasm32)
        } else {
            (true, true, true)
        }
    }
}

/// The release-input instrument, without entering any publishing step.
pub fn input_check(repo: &Path, base: &str) -> Result<(), String> {
    let mut real = Real::new(repo, None, true, true)?;
    if !real.dirty()?.is_empty() {
        return Err("input-check requires a clean committed checkout".into());
    }
    let tip = real.head();
    let prover = real.prover(None)?;
    let start = std::time::Instant::now();
    let result = real.input_checks(base, &tip, &prover);
    println!(
        "release input wall: {:.3}s; records {}",
        start.elapsed().as_secs_f64(),
        real.release
            .join(crate::check_record::DIR)
            .join("release-input")
            .display()
    );
    match result {
        Ok(note) => {
            println!("{note}");
            Ok(())
        }
        Err(found) => Err(found.why().to_owned()),
    }
}

/// `targo --unverified ship check`: would a release of the commit pass — the cut's pre-claim gates that read only the commit and this clone's records ([`crate::gates::precheck`]), the publication engine's export guards ([`Real::export_gate`]) and the wasm32 engine ([`Real::wasm32_gate`]), each line printed as its gate ends, then the verdict.
#[must_use]
pub fn check(repo: &Path, opts: &CheckOptions) -> i32 {
    interrupt::install();
    let found = match checked(repo, opts) {
        Ok(found) => found,
        Err(e) => {
            eprintln!("aterm-release check: {e}");
            return 2;
        }
    };
    println!("{}", found.verdict());
    if interrupt::caught().is_some() {
        return 130;
    }
    found.exit_code()
}

/// Run [`check`]'s sections for the commit `opts` names.
/// # Errors
/// The commit names no commit, or its `Cargo.toml` carries no release version.
fn checked(repo: &Path, opts: &CheckOptions) -> Result<Precheck, String> {
    checked_with(repo, opts, |_| {})
}

/// [`checked`], with `adjust` applied to the engine's backend before its gates run (a test's
/// stand-ins: how it acquires its prerequisites, how it builds this checkout's binaries).
fn checked_with(
    repo: &Path,
    opts: &CheckOptions,
    adjust: impl FnOnce(&mut Real),
) -> Result<Precheck, String> {
    let rev = opts.commit.as_deref().unwrap_or("HEAD");
    let git = GitCli::new(repo);
    let (cut, export, wasm32) = opts.sections();
    let found = if cut {
        let native = crate::publish::native_origin(repo);
        let origin = native
            .as_ref()
            .map(|origin| origin as &dyn crate::ledger::Origin)
            .map_err(|error| crate::ledger::Error::new(error.to_string()));
        if opts.source {
            crate::gates::precheck_source(&git, origin, rev)
        } else {
            crate::gates::precheck(&git, origin, rev)
        }
    } else {
        Precheck::of(&git, rev)
    };
    let mut found = found.map_err(|e| e.to_string())?;
    print!("{}", found.header());
    for gate in &found.gates {
        print!("{}", Precheck::gate_lines(gate));
    }
    let add = |found: &mut Precheck, name: &'static str, outcome: Outcome| {
        let gate = PrecheckGate { name, outcome };
        print!("{}", Precheck::gate_lines(&gate));
        let _ = std::io::stdout().flush();
        found.gates.push(gate);
    };
    if opts.compat {
        std::fs::create_dir_all(repo.join("dist")).map_err(|e| e.to_string())?;
        let outcome = match crate::buildplan::compat_available().map(|()| {
            crate::buildplan::compat_check(repo, &found.commit, &repo.join("dist/watch-compat.log"))
        }) {
            Ok(Ok(why)) => Outcome::Pass(why),
            Ok(Err(why)) => Outcome::Refuse(Refusal::new(why, "fix the stable compat slice")),
            Err(why) => {
                Outcome::CouldNotRun(Refusal::new(why, "provide the compat compiler and std"))
            }
        };
        add(&mut found, "stable-x86_64", outcome);
    }
    if !(export || wasm32) {
        return Ok(found);
    }
    let commit = found.commit.clone();
    let ready = Real::new(repo, opts.engine.as_deref(), true, false)
        .map(|mut real| {
            adjust(&mut real);
            real
        })
        .and_then(|real| Ok((real, Scratch::read_only(repo)?)))
        .map_err(|why| {
            Refusal::new(
                format!("the engine's gates have no place to run: {why}"),
                "fix what that names, then run it again",
            )
        });
    match ready {
        Ok((mut real, scratch)) => {
            let mut left = None;
            if export {
                let (outcome, export) = real.export_gate(&scratch, &commit);
                left = export;
                add(&mut found, EXPORT_GATE, outcome);
            }
            if wasm32 {
                let tools = Wasm32Tools::find();
                let outcome = real.wasm32_gate(&scratch, &commit, left.as_deref(), tools.as_ref());
                add(&mut found, WASM32_GATE, outcome);
            }
        }
        Err(why) => {
            for (asked, name) in [(export, EXPORT_GATE), (wasm32, WASM32_GATE)] {
                if asked {
                    add(&mut found, name, Outcome::CouldNotRun(why.clone()));
                }
            }
        }
    }
    Ok(found)
}

#[path = "release_steps_nested_locks.rs"]
mod nested_locks;

/// Move admitted nested locks through pinned Unix directory handles, staging every update
/// before publication. Unknown evidence refuses; renames are not a multi-file transaction.
fn bump_nested_locks(
    repo: &Path,
    listing: Result<Vec<String>, String>,
    from: &str,
    to: &str,
) -> Result<Vec<String>, String> {
    nested_locks::bump(repo, listing, from, to)
}

/// `lock` (a nested workspace's Cargo.lock) with every path package at the
/// workspace's version `from` moved to `to`, as `targo update` there rewrites
/// it after a bump; `None` when no line moves. A registry package carries a
/// `source` line and keeps its version, whatever it is.
fn lock_follows_bump(lock: &str, from: &str, to: &str) -> Option<String> {
    let mut lines: Vec<std::borrow::Cow<'_, str>> = lock
        .split_inclusive('\n')
        .map(std::borrow::Cow::Borrowed)
        .collect();
    let mut moved = false;
    let mut name = None;
    let mut renamed = Vec::new();
    let mut version_line: Option<usize> = None;
    let mut sourced = false;
    for i in 0..=lines.len() {
        let line = lines.get(i).map_or("", |l| l.trim_end());
        if i == lines.len() || line.starts_with('[') {
            if let (Some(at), false) = (version_line.take(), sourced) {
                let ending = &lines[at][lines[at].trim_end().len()..];
                lines[at] = std::borrow::Cow::Owned(format!("version = \"{to}\"{ending}"));
                moved = true;
                if let Some(name) = name.take() {
                    renamed.push(name);
                }
            }
            name = None;
            sourced = false;
        } else if let Some(value) = line
            .strip_prefix("name = \"")
            .and_then(|s| s.strip_suffix('"'))
        {
            name = Some(value.to_owned());
        } else if line
            .strip_prefix("version = \"")
            .and_then(|s| s.strip_suffix('"'))
            == Some(from)
        {
            version_line = Some(i);
        } else if line.starts_with("source = ") {
            sourced = true;
        }
    }
    // Cargo disambiguates equal package names by version in dependency edges.
    // A sourced edge includes its source suffix and must retain its old version.
    for line in &mut lines {
        let trimmed = line.trim_start();
        for name in &renamed {
            if trimmed.trim_end() == format!("\"{name} {from}\",") {
                *line = std::borrow::Cow::Owned(line.replacen(
                    &format!("\"{name} {from}\""),
                    &format!("\"{name} {to}\""),
                    1,
                ));
                break;
            }
        }
    }
    moved.then(|| lines.concat())
}

/// `ship check`'s name for the engine's dry run.
pub const EXPORT_GATE: &str = "export guards";
/// `ship check`'s name for the wasm32 compile.
pub const WASM32_GATE: &str = "wasm32 engine";

#[cfg(test)]
#[path = "release_steps_public_reads_http_tests.rs"]
mod public_reads_http_tests;

#[cfg(test)]
#[path = "release_steps_engine_tests.rs"]
mod engine_tests;

#[cfg(test)]
#[path = "release_fix_tests.rs"]
mod fix_tests;

#[cfg(test)]
#[allow(
    dead_code,
    reason = "only the integration harness supplies the offline controller"
)]
pub(crate) fn measure_bump_launch_to_qualify(controller: fn() -> std::time::Duration) {
    tests::measure_bump_launch_to_qualify(controller);
}

#[cfg(test)]
#[path = "release_commit_gate_measure.rs"]
mod commit_gate_measure;

#[cfg(test)]
#[path = "release_dry_tools_tests.rs"]
#[allow(
    dead_code,
    reason = "the integration controller supplies the frozen-source fixture; the binary's lint-only test target does not mount it"
)]
pub(crate) mod dry_tools_tests;

#[cfg(test)]
#[path = "release_steps_tests.rs"]
mod tests;
