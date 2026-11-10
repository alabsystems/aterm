// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Real [`crate::release::Backend`] implementation for `targo --unverified ship release`.

#[cfg(unix)]
use aterm_uds::spawnfd::Tether as _;
use aterm_uds::spawnfd::{Command, Stdio, command};
use std::io::Write as _;
use std::path::{Path, PathBuf};

use crate::gates::{Outcome, Precheck, PrecheckGate};
use crate::interrupt;
use crate::ledger::{BYTE_EXACT, GitCli, GitRunner};
use crate::line::{Fix, Hop, Line};
use crate::provision::{AskWords, Finding};
use crate::release::{
    AfterCut, Answer, Artifact, Backend, Branch, BranchStop, Branches, BuildWork, Built, Candidate,
    CutState, EngineNow, EngineRows, Facts, PUBLISH_GATE_ENV, Prover, PublishGate, RELEASE_COMMAND,
    Ran, Refusal, Reservation, Side, StageTarget, Step,
};

#[path = "release_scanner_tools.rs"]
mod scanner_tools;

#[path = "release_export_remedy.rs"]
mod export_remedy;

#[cfg(all(debug_assertions, not(test)))]
#[path = "release_cutter_probe.rs"]
mod cutter_probe;
#[cfg(all(debug_assertions, not(test)))]
pub(crate) use cutter_probe::cutter_probe_if_asked;

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

/// The release state's directory: [`RELEASE_STATE`] below `home`, or below a Cargo test binary's
/// own root ([`aterm_uds::test_binary::scratch_root`]), so that no test writes a run's logs, its
/// step times, its disk footprints or a kept seal into the person's, which the next real release
/// reads.
pub(crate) fn release_state(home: &Path) -> PathBuf {
    aterm_uds::test_binary::scratch_root()
        .unwrap_or(home)
        .join(RELEASE_STATE)
}

type VerifyHalf = fn(
    &Real,
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

/// The real backend.
pub struct Real {
    prefreeze_claim: Option<aterm_capacity::Claim>,
    pub(crate) build_inputs: fn() -> Result<String, String>,
    /// Keeps this process's build-inputs seal in the release state, its reports also in the
    /// named log ([`crate::build_inputs::arm`]).
    pub(crate) arm_inputs: fn(&Path, Option<&Path>),
    /// The kept seal each child is handed ([`crate::build_inputs::handed_path`]).
    pub(crate) handed_inputs: fn() -> Option<PathBuf>,
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
    /// [`Real::engine_copy`]); in a dry run, which never makes or moves one, `engine_source` until a
    /// real run has made it.
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
        let waited = wait_accounted(&mut child);
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
            let _ = wait_accounted(&mut child);
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

/// The qualify graph's cores: its one Release core claim, the jobserver of that many slots every
/// branch's children draw from, and the share of it the export holds while it runs (the
/// engine's gate passes `CARGO_BUILD_JOBS` alone, so its cargo cannot draw from the jobserver).
struct Pool {
    _claim: aterm_capacity::Claim,
    jobs: aterm_capacity::Jobserver,
    export: u32,
}

/// The qualify graph's pool at `path`: one Release core claim of every core, as `admit` answers
/// it, a jobserver of as many slots as it granted, and the export's share of them where `export`
/// runs ([`export_share`]).
fn qualify_pool(
    path: &Path,
    export: bool,
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
    let jobs = aterm_capacity::Jobserver::new(path, slots).map_err(|e| {
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
        export: export_share(slots, export),
    })
}

/// The export's share of the pool's `slots` while it runs: a quarter of them, at least one, and
/// never every slot (none of one slot), so the tree and the build always draw.
pub(crate) fn export_share(slots: u32, export: bool) -> u32 {
    if !export || slots < 2 {
        return 0;
    }
    (slots / 4).clamp(1, slots - 1)
}

/// The real backend's view the qualify graph's branches run on ([`Backend::branches`]), with the
/// export's share of the pool taken from it before any branch starts, given back as the export
/// ends.
struct RealBranches<'a> {
    real: &'a Real,
    export: std::sync::Mutex<Vec<aterm_capacity::Token<'a>>>,
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
        let me = std::process::id();
        while !ended() {
            if interrupt::caught().is_some()
                || slices_hold_memory(me)
                || self.tree_index.bundle_is_finished()
            {
                return;
            }
            std::thread::sleep(std::time::Duration::from_secs(1));
        }
    }

    fn export(
        &self,
        c: &Candidate,
        prover: &Prover,
        target: StageTarget,
        stop: &BranchStop,
    ) -> Result<Ran, Refusal> {
        let held = std::mem::take(&mut *self.export.lock().unwrap_or_else(|e| e.into_inner()));
        let jobs = u32::try_from(held.len()).ok().filter(|n| *n > 0);
        let ran = self
            .real
            .export_run(c, prover, target, Step::Qualify, Some((stop, jobs)));
        drop(held);
        ran
    }

    fn build(
        &self,
        c: &Candidate,
        reserved: &Reservation,
        prover: &Prover,
        work: BuildWork,
        stop: &BranchStop,
    ) -> Built {
        self.real
            .build_branch(c, reserved, prover, work, &self.tree_index, stop)
    }

    fn verify(
        &self,
        c: &Candidate,
        prover: &Prover,
        run: &crate::check_record::VerifyRun,
        artifact: Option<&Artifact>,
        stop: &BranchStop,
    ) -> Result<Ran, Refusal> {
        if run.half == crate::check_record::Half::Tree {
            with_tree_index_completion(&self.tree_index, stop, || {
                (self.real.verify_half_op)(self.real, c, prover, run, artifact, stop)
            })
        } else {
            (self.real.verify_half_op)(self.real, c, prover, run, artifact, stop)
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

/// Both slices must have their claims before the tree can compete for memory. A build
/// that fails or serializes on a smaller machine releases the wait by ending instead.
fn slices_hold_memory(me: u32) -> bool {
    let Some(records) = aterm_capacity::Ledger::user().and_then(|l| l.list().ok()) else {
        return false;
    };
    let Some(procs) = interrupt::listing() else {
        return false;
    };
    let parent: std::collections::BTreeMap<i32, i32> =
        procs.iter().map(|p| (p.pid, p.parent)).collect();
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
    ["aarch64-apple-darwin", "x86_64-apple-darwin"]
        .iter()
        .all(|triple| {
            records.iter().any(|r| {
                r.held.is_some()
                    && r.need.what == format!("release slice {triple}")
                    && r.need.mem > 0
                    && descends(r.pid)
            })
        })
}

struct Forwarded {
    _running: interrupt::Running,
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
            std::env::temp_dir().join(format!(
                "aterm-release-dry-{}-{nth}.noindex",
                std::process::id()
            ))
        } else {
            state
                .join("logs")
                .join(format!("{stamp}-{}", std::process::id()))
        };
        std::fs::create_dir_all(&logs).map_err(|e| format!("create {}: {e}", logs.display()))?;
        Ok(Self {
            prefreeze_claim: None,
            automatic_prefreeze: None,
            build_inputs: crate::build_inputs::current,
            arm_inputs: crate::build_inputs::arm,
            handed_inputs: crate::build_inputs::handed_path,
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
        let running = match stop {
            Some((stop, sig)) => interrupt::Running::enter_stoppable(group, stop.clone(), sig),
            None => interrupt::Running::enter(group),
        };
        let note = self.note_child(step, child.id());
        Ok((
            child,
            Forwarded {
                _running: running,
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
    /// `step-child-finished` record of what `wait4` booked for it ([`step_child_event`]).
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
        let (mut child, forwarded) =
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
        let waited = wait_accounted(&mut child);
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

    /// Hand step 4's pool to a branch child that adopts it ([`aterm_capacity::Jobserver::hand_to`]):
    /// `ship build`, whose slices draw from it, and the tree's `verify`, whose test binaries
    /// do. With `less_export` the child's share is the pool less the export's
    /// ([`aterm_capacity::Jobserver::hand_share_to`]: the same FIFO, by the jobserver's own path,
    /// with fewer slots), so no test binary's charge waits holding tokens on what the export
    /// holds. Nothing else is handed it: the export draws its share as `CARGO_BUILD_JOBS`, and a
    /// child outside the graph has no pool. Refused, before the child runs, only for a share
    /// the pool cannot carry, which [`export_share`] never leaves.
    fn hand_pool(&self, cmd: &mut Command, less_export: bool) -> Result<(), Refusal> {
        let Some(pool) = &self.pool else {
            return Ok(());
        };
        if !less_export {
            pool.jobs.hand_to(cmd);
            return Ok(());
        }
        let slots = pool.jobs.slots();
        let share = slots.saturating_sub(pool.export);
        pool.jobs.hand_share_to(cmd, share).map_err(|e| {
            Refusal::new(
                format!(
                    "the qualify graph's jobserver cannot hand a share of {share} of its {slots} \
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
        cmd.args([
            "publish",
            "aterm",
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

    fn logged_to(&self, step: Step, cmd: &mut Command, stream: bool) -> Ran {
        self.logged_in(
            Logged {
                step,
                log: self.step_log(step),
                stream,
                stop: None,
                push: None,
                arrivals: step == Step::Publish,
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
        if interrupt::caught().is_some() {
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
        let spawned = self.spawn_stoppable(step, cmd, stop);
        cmd.stdout(Stdio::null()).stderr(Stdio::null());
        let (mut child, forwarded) = match spawned {
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
        let waited = wait_accounted(&mut child);
        let ended_at_unix_ms = now_ms();
        let code = waited
            .as_ref()
            .map_or(-1, |(status, _)| status.code().unwrap_or(-1));
        if let Some(pumped) = pumped {
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
    /// judges the ledger and `bin/pub` on until step 2 syncs the release's engine; aterm's main.
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
    ) -> Result<Vec<u8>, String> {
        use aterm_verify::provision::Host as _;
        let targo = aterm_verify::provision::System::new(&self.repo).targo()?;
        let mut cmd = self.child(targo, None);
        cmd.args(args);
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
        let layout = verify_layout(&self.repo).1;
        let args = Self::frozen_tool_args(&self.repo, bin, profile, layout.as_deref());
        let mut command = self.child(prover.bin.join("targo"), Some(prover));
        command.args(&args).current_dir(&self.repo);
        // Keep Cargo's actual dirty-unit reasons beside the artifact counts. A
        // later cut must distinguish a source delta from cache invalidation.
        command.env("CARGO_LOG", "targo::compiler::fingerprint=info");
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
                frozen_build_event(bin.package, profile, layout.as_deref(), &output.stdout)
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
        crate::release_tools::clean_at(&self.repo, commit)?;
        if crate::release_tools::Recovery::read(commit) != crate::release_tools::Recovery::Refreeze
            && let Ok(cutter) = crate::release_tools::recorded_cutter(&self.release, commit)
        {
            return Ok(cutter);
        }
        let cache = self.release.join("cutter-cache");
        std::fs::create_dir_all(&cache).map_err(|e| e.to_string())?;
        #[cfg(unix)]
        if self.line_priority.is_some() {
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
        crate::release_tools::clean_at(&self.repo, commit)?;
        crate::release_tools::publish_cutter(&self.release, &cutter)?;
        Ok(cutter)
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
        let explicit = self.engine_agh_explicit.as_deref();
        crate::checkout_bin::prepare(&self.repo, &ENGINE_AGH, explicit, |args| {
            println!("export guards: building this checkout's agh, the engine's GitHub client");
            (self.checkout_build)(self, ENGINE_AGH.what, args)
        })
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
        let (mut stage, handed) = match self.dry_run_command(&engine, r, &root, &reads, &agh) {
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
        let mut stage = self.engine_child(engine, None)?;
        stage
            .env("PUBLICATION_ENGINE", engine)
            .env(ENGINE_AGH_ENV, agh)
            .env(
                "PATH",
                rustup_dir().map_or_else(|_| publication_path(), |dir| path_with(&dir)),
            )
            .envs(reads.iter().map(|(key, value)| (key, value)))
            .args([
                "stage",
                "aterm",
                "--dry-run",
                "--no-check",
                "--commit",
                r,
                "--repo-path",
            ])
            .arg(root);
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
        scrub_fixture_command(cmd);
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

    /// [`stage_fixtures_run`], with the qualify build branch's stop attached to every fallback
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
        let mut run = |cmd: &mut Command| self.captured_with_stop(step, prover, log, cmd, stop);
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
    /// before [`stage_fixtures_run`] may choose its fallback compile. A stopped sibling starts no
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

    /// `verify release` of `run`'s half in the clone at R ([`Branches::verify`]): the tree's in
    /// step 4's tree branch, the bundle's in step 5 on `artifact`, each into its own log and
    /// events; its records kept only where R's tree stayed clean and the seal held throughout.
    fn verify_half(
        &self,
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
        let (step, log) = match run.half {
            crate::check_record::Half::Tree => (
                Step::Qualify,
                self.side_log(Step::Qualify, Branch::Tree.name()),
            ),
            crate::check_record::Half::Bundle => (Step::Bundle, self.step_log(Step::Bundle)),
        };
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
        let content_plan = self
            .check_content_context(
                &frozen.verify.path,
                &frozen.set_sha256,
                &seal,
                prover,
                run.half,
            )
            .and_then(|context| crate::check_content::plan(&self.git(), &c.commit, &context))
            .ok();
        let item_context = crate::check_content::item_context(&frozen.set_sha256, &seal);
        cmd.arg("release").arg(run.half.flag());
        if run
            .checks
            .iter()
            .any(|check| matches!(check.as_str(), "test" | "lint"))
        {
            cmd.arg("--carry-context").arg(&item_context);
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
            (self.await_fixture_index_op)(self, c, prover, tree_index, stop).and_then(|()| {
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
        cmd.env(crate::buildplan::SLICE_LOGS_ENV, &self.logs);
        let ran = self.logged_in(
            Logged {
                step: Step::Qualify,
                log: log.to_path_buf(),
                stream: self.verbose,
                stop: Some((stop, interrupt::SIGTERM)),
                push: None,
                arrivals: false,
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
    ) -> Result<Vec<String>, String> {
        let ptr = format!("https://github.com/{slug}/releases/latest/download/aterm-appcast.toml");
        let want = format!("https://github.com/{slug}/releases/download/{tag}/aterm-appcast.toml");
        let hop = aterm_update_core::head_no_redirect(&ptr)
            .map_err(|e| format!("{ptr} could not be read: {e:?}"))?;
        if let Some(problem) =
            pointer_hop_problem(&ptr, &want, tag, hop.code, hop.location.as_deref())?
        {
            return Ok(vec![problem]);
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

/// Wait for a step's child and keep what `wait4` books for it: CPU of the child and of every
/// descendant reaped down its tree (verify's stages and the engine's children run in groups and
/// sessions of their own, which a group read of the child would miss), and the largest process's
/// resident peak. `None` usage where the host keeps no accounting; the status either way.
fn wait_accounted(
    child: &mut aterm_uds::spawnfd::Child,
) -> std::io::Result<(std::process::ExitStatus, Option<aterm_uds::spawnfd::Rusage>)> {
    match child.wait_with_rusage() {
        Ok(usage) => Ok((usage.status, Some(usage))),
        Err(e) if e.kind() == std::io::ErrorKind::Unsupported => Ok((child.wait()?, None)),
        Err(e) => Err(e),
    }
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
/// exited and what `wait4` booked for it. Planning data for `release status`, never a verdict,
/// so it carries no `outcome` (a retried child's exit is not the run's first red).
fn step_child_event(
    step: Step,
    cmd: &Command,
    started_at_unix_ms: Option<u64>,
    ended_at_unix_ms: Option<u64>,
    exit_code: i32,
    usage: Option<aterm_uds::spawnfd::Rusage>,
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
            (
                "scope".to_string(),
                "reaped-child-and-waited-descendants".into(),
            ),
            ("observed_s".to_string(), u.cpu.as_secs_f64().into()),
            ("user_s".to_string(), u.user.as_secs_f64().into()),
            ("system_s".to_string(), u.system.as_secs_f64().into()),
        ]))
    });
    let peak_bytes = usage
        .map(|u| u.maxrss)
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
type CheckoutBuild = fn(&Real, &str, &[std::ffi::OsString]) -> Result<Vec<u8>, String>;

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
pub(crate) fn verify_layout(repo: &Path) -> (PathBuf, Option<String>) {
    let env = aterm_verify::EnvSnapshot {
        home: std::env::var_os("HOME")
            .map(PathBuf::from)
            .unwrap_or_default(),
        path: publication_path(),
        ..Default::default()
    };
    let repo = aterm_verify::physical(repo);
    let target = aterm_verify::physical_target(&env, &repo);
    let layout = aterm_verify::main_build_dir(&env, &repo)
        .or_else(|| aterm_verify::fallback_build_dir(&env, &repo, &target));
    (target, layout)
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

fn source_attestation_findings(release: &aterm_json::Value) -> Vec<String> {
    crate::channel::SOURCE_ATTESTATION_ASSETS
        .into_iter()
        .filter_map(|name| {
            let count = release
                .get("assets")
                .and_then(aterm_json::Value::as_array)
                .into_iter()
                .flatten()
                .filter(|a| a.get("name").and_then(aterm_json::Value::as_str) == Some(name))
                .count();
            match count {
                1 => None,
                0 => Some(format!(
                    "{name} is missing — the signed source manifest `pub publish` attaches \
                     (`pub sign-release aterm` attaches it to a published release)"
                )),
                _ => Some(format!(
                    "{name} is duplicated — the source manifest must be unambiguous"
                )),
            }
        })
        .collect()
}

/// The lean-download shape of a published release, from what GitHub serves (its release JSON, never the files the cut wrote): the problems, empty when it carries the shape.
#[must_use]
pub fn release_shape(release: &aterm_json::Value, version: &str) -> Vec<String> {
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
    bad.extend(source_attestation_findings(release));
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

fn pointer_hop_problem(
    ptr: &str,
    want: &str,
    tag: &str,
    code: u16,
    location: Option<&str>,
) -> Result<Option<String>, String> {
    let why = match code {
        302 if location == Some(want) => return Ok(None),
        404 => "404 — no published non-prerelease release holds the pointer".to_string(),
        302 => format!(
            "302 to {}, not {want} — another release holds GitHub's latest; the cut owns \
             it: while {tag}'s cut is open, `{RELEASE_COMMAND}` resumes it and re-sends \
             its head PATCH (never re-point it by hand)",
            location.unwrap_or("<no Location>")
        ),
        code => {
            return Err(format!(
                "{ptr} answers HTTP {code}, not a release pointer's 302 or 404"
            ));
        }
    };
    Ok(Some(format!(
        "{ptr} answers {why}; every credential-less install discovers the channel head \
         from this pointer and would never see {tag}"
    )))
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

impl Real {
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
        )?;
        crate::check_content::plan(&self.git(), commit, &context)
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
        let claim = crate::line_claim::Claim::take(&self.repo, &prefix, &self.logs, continued)?;
        let selected = if claim.line == prefix {
            line.clone()
        } else {
            claim.line.clone()
        };
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

    fn supersede_line(&mut self, line: &Line) -> Result<String, String> {
        self.fetch().map_err(|e| e.why)?;
        let previous = crate::line_claim::existing(&self.repo, &line.version)?
            .ok_or("no existing fleet line to supersede")?;
        let mut claim = crate::line_claim::Claim::take(&self.repo, &previous, &self.logs, false)?;
        // Claim acquisition may adopt a hop written after the read above.
        let previous = claim.line.clone();
        self.fetch().map_err(|e| e.why)?;
        if crate::line::supersession(&self.git(), &line.version)?.is_some_and(|record| {
            record.replacement.base == line.base && record.replacement.version == line.version
        }) {
            claim.audit()?;
            self.fleet_line = Some(claim);
            return Ok(format!("supersession already recorded for {}", line.base));
        }
        crate::line::verify(&self.git(), &previous).map_err(|e| e.to_string())?;
        let ledger_text = self
            .git_out(&["show", "origin/main:RELEASES.ledger"])
            .ok_or("cannot read origin/main's release ledger")?;
        let mappings = self
            .engine_text("mappings.json")
            .ok_or("cannot read the publication engine's mappings")?;
        let _: aterm_json::Value = aterm_json::from_str(&mappings)
            .map_err(|e| format!("invalid publication mappings: {e}"))?;
        let tag = format!("v{}", line.version);
        let rows = self.engine_rows(Some(previous.head()));
        let published = rows.has
            || rows
                .newest
                .as_deref()
                .is_some_and(|sha| self.version_at(sha).as_deref() == Some(line.version.as_str()));
        let tagged = self.origin_tag(&tag).is_some();
        let ledger = ledger_text
            .lines()
            .any(|row| row.split_whitespace().nth(1) == Some(line.version.as_str()))
            || self
                .cut_journal()
                .is_some_and(|cut| cut.version == line.version);
        let published = published
            || std::iter::once(previous.base.as_str())
                .chain(previous.hops.iter().map(|hop| hop.commit.as_str()))
                .any(|commit| self.engine_rows(Some(commit)).has);
        crate::line::require_unpublished([tagged, ledger, published, false, false])?;
        let public =
            crate::publish::workspace_channel_slug(&self.repo).map_err(|e| e.to_string())?;
        let released = !crate::publish::release_objects_by_tag(&public, &tag)
            .map_err(|e| e.to_string())?
            .is_empty();
        crate::line::require_unpublished([false, false, false, released, false])?;
        let config = self
            .git_out(&["show", "origin/main:publish/config.sh"])
            .ok_or("cannot read the staging remote declaration")?;
        let staging = config
            .lines()
            .find_map(|row| {
                row.strip_prefix("STAGING_REMOTE_DEFAULT=\"")
                    .and_then(|value| value.strip_suffix('"'))
            })
            .ok_or("no staging remote declaration")?;
        let staging =
            crate::publish::github_slug_from_remote_url(staging).map_err(|e| e.to_string())?;
        let staged = crate::github_remote::repository_file(&staging, "main", "Cargo.toml")?;
        let staged = std::str::from_utf8(&staged).map_err(|e| e.to_string())?;
        let staged_version =
            crate::publish::workspace_version(staged).map_err(|e| e.to_string())?;
        let numbers = |version: &str| {
            version
                .split('.')
                .map(str::parse::<u64>)
                .collect::<Result<Vec<_>, _>>()
        };
        let staged = numbers(&staged_version).map_err(|e| e.to_string())?
            >= numbers(&line.version).map_err(|e| e.to_string())?;
        crate::line::require_unpublished([false, false, false, false, staged])?;
        claim.audit()?;
        let record = crate::line::Supersession {
            previous,
            replacement: line.clone(),
            reason: "explicit --supersede: the unpublished line cannot finish with its own tooling"
                .into(),
        };
        let origin = crate::github_remote::Remote::origin(&self.repo)?;
        let merge = crate::line::supersede(&self.git(), &origin, &record)?;
        claim.boundary(line, "commit", &self.times())?;
        claim.audit()?;
        self.fleet_line = Some(claim);
        Ok(format!(
            "SUPERSEDED v{} line {} (base {}) with {}: {}; recorded on main at {merge}",
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
        let mut line = crate::line_claim::existing(&self.repo, version)
            .map_err(|why| Refusal::new(why, retry()).could_not_run())?;
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
    /// ([`hand_inputs_seal`]), taken here or adopted from the cutter that re-ran this one. A dry
    /// run never comes here, and seals lazily, keeping none. Off macOS a release has no
    /// build-inputs identity to seal.
    fn seal_inputs(&mut self) -> Result<(), String> {
        let _phase = crate::cut_timing::Timer::start(&self.step_log(self.step), "inputs-seal");
        if cfg!(target_os = "macos") {
            (self.arm_inputs)(&self.state, Some(&self.transcript()));
            (self.build_inputs)()?;
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
        self.fetch_origins(|repo| {
            prefetched.map_or_else(
                || source_origin(repo).and_then(|remote| remote.fetch_origin()),
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

    fn input_checks(&mut self, base: &str, tip: &str, prover: &Prover) -> Result<String, Refusal> {
        let remedy = crate::release::fix_remedy(tip);
        let unavailable = |why: String| {
            Refusal::new(
                format!("release input could not be judged: {why}"),
                remedy.clone(),
            )
            .could_not_run()
        };
        // A dry run does not move the person's checkout to the fetched candidate.
        // Only that case needs a private worktree; a real cut already runs at R.
        let temporary = if self.off_tree(tip).is_some() {
            let dir = aterm_tempfile::Builder::new()
                .prefix("aterm-release-input-")
                .suffix(".noindex")
                .tempdir()
                .map_err(|e| unavailable(e.to_string()))?;
            let root = dir.path().join("input");
            self.git()
                .git(&[
                    "worktree",
                    "add",
                    "--detach",
                    root.to_str()
                        .ok_or_else(|| unavailable("non-UTF-8 scratch path".into()))?,
                    tip,
                ])
                .and_then(|o| {
                    if o.success() {
                        Ok(o)
                    } else {
                        Err(crate::ledger::Error::new(o.stderr_utf8()))
                    }
                })
                .map_err(|e| unavailable(e.to_string()))?;
            Some((dir, root))
        } else {
            None
        };
        let root = temporary
            .as_ref()
            .map_or(self.repo.as_path(), |(_, root)| root.as_path());
        let initialized = crate::ledger::GitCli::new(root).git(&[
            "submodule",
            "update",
            "--init",
            "vendor/astream",
        ]);
        let result = (|| match initialized {
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
                let mut child = self.child(prover.bin.join("targo"), Some(prover));
                if let Some(layout) = verify_layout(root).1 {
                    child.args(["--config", &layout]);
                }
                child
                    .args(["--unverified", "verify", "release-input", "--base", base])
                    .env("PUBLICATION_ENGINE", &self.engine)
                    .current_dir(root);
                crate::input_record::detach_launcher(&mut child);
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
                let mut identity_child = self.child(prover.bin.join("targo"), Some(prover));
                if let Some(layout) = verify_layout(root).1 {
                    identity_child.args(["--config", &layout]);
                }
                identity_child
                    .args(["--unverified", "verify", "release-input", "--base", base])
                    .env("PUBLICATION_ENGINE", &self.engine)
                    .current_dir(root);
                crate::input_record::detach_launcher(&mut identity_child);
                self.finalize_spawn_environment(&mut identity_child)
                    .map_err(unavailable)?;
                identity_child.env_remove(crate::build_inputs::SEAL_ENV);
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
                        crate::check_record::InputError::Failed(why) => {
                            Refusal::new(why, remedy.clone())
                        }
                        crate::check_record::InputError::Unavailable(why) => unavailable(why),
                    })?;
                if missing.is_empty() {
                    return Ok(format!(
                        "release input: reused 4 checks for tree {tree}; 0 checks run"
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
                } else {
                    let refusal = Refusal::new(text, remedy.clone());
                    Err(if out.status.code() == Some(aterm_verify::exit::FAILED) {
                        refusal
                    } else {
                        refusal.could_not_run()
                    })
                }
            }
            Ok(out) => Err(unavailable(out.stderr_utf8())),
            Err(why) => Err(unavailable(why.to_string())),
        })();
        if let Some((_, root)) = &temporary {
            let removed =
                self.git()
                    .git(&["worktree", "remove", "--force", &root.to_string_lossy()]);
            if !removed.is_ok_and(|out| out.success()) {
                return Err(unavailable(format!(
                    "cannot remove private input worktree {}",
                    root.display()
                )));
            }
        }
        result
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

    fn cut_journal(&mut self) -> Option<CutState> {
        let header =
            crate::publish::JournalHeader::read(&self.repo.join("dist/cut-state.toml")).ok()??;
        Some(CutState {
            finished: header.finished(),
            version: header.version,
            build: header.build_number,
        })
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

    fn bump(&mut self, from: &str, to: &str, prover: &Prover) -> Result<String, Refusal> {
        let _timing = crate::cut_timing::Timer::start(&self.step_log(self.step), "version-bump");
        let origin = crate::publish::native_origin(&self.repo).map_err(|why| {
            Refusal::new(
                "cannot bind the native source origin to this workspace",
                "restore the configured repository matching Cargo.toml before rerunning",
            )
            .detail([why.to_string()])
        })?;
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
                let mut cmd = self.scrubbed(&edit);
                cmd.arg(&self.repo).args([to, &cur]).current_dir(&self.repo);
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
                let nested =
                    self.git_paths(&["ls-files", "-z", "--", "Cargo.lock", "*/Cargo.lock"]);
                let lock_timing =
                    crate::cut_timing::Timer::start(&self.step_log(self.step), "bump-lockfiles");
                let followed = match bump_nested_locks(&self.repo, nested, from, to) {
                    Ok(followed) => followed,
                    Err(why) => {
                        undo(self);
                        return Err(Refusal::new(why, "read the commit step's log"));
                    }
                };
                if let Err(why) = self.validate_bumped_locks(prover, &followed) {
                    undo(self);
                    return Err(Refusal::new(
                        why,
                        "read the commit step's log; the bump is undone",
                    ));
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
                        return Err(Refusal::new(why, "read the commit step's log"));
                    }
                };
                let moved: Vec<String> = inspected
                    .into_iter()
                    .flatten()
                    .filter(|p| p != "Cargo.toml" && p != "Cargo.lock" && !followed.contains(p))
                    .collect();
                let Some(diff) = self.git_out(&["diff", "-U0"]) else {
                    undo(self);
                    return Err(Refusal::new(
                        "could not inspect the bump's diff; the bump is undone",
                        "read the commit step's log",
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
                let harness_timing = crate::cut_timing::Timer::start(
                    &self.step_log(self.step),
                    "bump-harness-versions",
                );
                let owed = match crate::harness_versions::apply(&self.repo, &origin) {
                    Ok(applied) => applied.map(|(_, owed)| owed).unwrap_or_default(),
                    Err(e) => {
                        undo(self);
                        return Err(Refusal::new(
                            format!(
                                "the harness versions could not be bumped ({e}); the bump is undone"
                            ),
                            "read the commit step's log",
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
                let mut commit = local_git()?;
                commit
                    .args(["commit", "-q", "-m", &msg, "--", "Cargo.toml", "Cargo.lock"])
                    .args(&followed)
                    .args(crate::harness_versions::written(&owed))
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
                let mut declared = vec!["Cargo.toml".into(), "Cargo.lock".into()];
                declared.extend(followed);
                declared.extend(crate::harness_versions::written(&owed));
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
        let helper =
            crate::source_signing::prepare_helper(&self.repo, explicit.as_deref(), |args| {
                println!("validation tools: building this checkout's source-signing helper");
                (self.checkout_build)(self, "source-signing helper", args)
            })
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
        crate::release_tools::clean_at(&self.repo, line.head())
            .map_err(|why| Refusal::new(why, "restore the selected clone and retry"))?;
        crate::line::verify_pending(&self.git(), line)?;
        let prefix = crate::line::recorded_prefix(&self.git(), line)?;
        // verify_pending proved every discarded hop is reproducible from its landed fix,
        // author/committer metadata and predecessor. A dirty or unrelated head never gets here.
        let mut cmd = local_git()?;
        cmd.current_dir(&self.repo)
            .args(["reset", "-q", "--keep", prefix.head()]);
        let ran = self.logged(Step::Commit, &mut cmd);
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
        let cutter = self.frozen_cutter(commit, prover).map_err(cannot)?;
        let mut command = self.cutter_command(&cutter.tool.path);
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
            let layout = verify_layout(&self.repo).1;
            command
                .args(Self::frozen_tool_args(
                    &self.repo,
                    &FROZEN_VERIFY,
                    "test",
                    layout.as_deref(),
                ))
                .current_dir(&self.repo)
                .stdin(Stdio::null())
                .stdout(std::fs::File::create(&stdout).map_err(|e| e.to_string())?)
                .stderr(std::fs::File::create(&stderr).map_err(|e| e.to_string())?);
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

    fn commit_gates(&mut self, c: &Candidate, dry: bool) -> Vec<Refusal> {
        let mut blocks: Vec<Refusal> = Vec::new();
        let git = self.git();
        let origin = crate::publish::native_origin(&self.repo);
        let sealed = if dry {
            None
        } else {
            let snapshot = (|| {
                Ok::<_, String>((
                    crate::build_inputs::current()?,
                    origin
                        .as_ref()
                        .map_err(ToString::to_string)?
                        .tag_fingerprint()?,
                ))
            })();
            match snapshot {
                Ok(record) => Some(record),
                Err(why) => return vec![cut_refusal("commit gate inputs", &why).could_not_run()],
            }
        };
        let with_origin =
            |check: &dyn Fn(&dyn crate::ledger::Origin) -> crate::ledger::Result<()>| match &origin
            {
                Ok(origin) => check(origin),
                Err(error) => Err(crate::ledger::Error::new(error.to_string())),
            };
        if dry && let Err(not) = crate::gates::own_cutter(&git, &c.commit) {
            self.say(&format!(
                "        note: R's pre-claim gates asked of this cutter, not yet R's own (a real \
                 real run executes as R's frozen cutter): {}",
                not.why
            ));
        }
        let (commit, version) = (c.commit.as_str(), c.version.as_str());
        let gates: [(&str, crate::ledger::Result<()>); 4] = [
            (
                "changelog",
                crate::gates::changelog_gate_at(&git, commit, version).map(|_| ()),
            ),
            (
                "harness versions",
                with_origin(&|origin| {
                    if dry && c.bump_from.is_some() {
                        crate::gates::harness_versions_bump_writes_at(&git, origin, commit, version)
                    } else {
                        crate::gates::harness_versions_gate_at(&git, origin, commit, version)
                    }
                    .map(|_| ())
                }),
            ),
            (
                "handoff fixtures",
                with_origin(&|origin| {
                    crate::gates::handoff_fixture_gate_at(&git, origin, commit, version).map(|_| ())
                }),
            ),
            (
                "handoff policy",
                with_origin(&|origin| {
                    crate::gates::handoff_policy_gate_at(&git, origin, commit, version).map(|_| ())
                }),
            ),
        ];
        for (name, outcome) in gates {
            if let Err(e) = outcome {
                blocks.push(cut_refusal(name, &e.to_string()));
            }
        }
        if let Err(error) = crate::gates::locked_metadata_gate(&self.repo) {
            let refusal = cut_refusal("locked metadata", &error.to_string());
            blocks.push(match error {
                crate::gates::LockedMetadataError::Unavailable(_) => refusal.could_not_run(),
                crate::gates::LockedMetadataError::Lockfile(_) => refusal,
            });
        }
        if let Err(why) = self.prepare_prerequisites() {
            blocks.push(why);
        }
        let seam = self
            .engine_text("bin/pub")
            .is_some_and(|t| t.contains("run_pre_promote_hook"));
        if !seam {
            blocks.push(Refusal::new(
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
            ));
        }
        if blocks.is_empty()
            && let Some((inputs, tags)) = sealed
        {
            let record = (|| {
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
                crate::gates::CommitGateRecord::new(&c.commit, &c.version, inputs, tags)
                    .save(&git)
                    .map_err(|e| e.to_string())
            })();
            if let Err(why) = record {
                blocks.push(cut_refusal("commit gate record", &why).could_not_run());
            }
        }
        blocks
    }

    fn cut_gates(
        &mut self,
        c: &Candidate,
        creds: &Path,
        kind: crate::publish::CutKind,
    ) -> Vec<Refusal> {
        crate::publish::pre_cut_gates(&self.repo, &c.version, creds, kind)
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
        self.scanner_tools = None;
        self.engine_sync()?;
        self.prepare_scanner_tools(prover)?;
        self.export_run(c, prover, target, Step::Publish, None)
    }

    fn qualify_begin(&mut self, prover: &Prover, export: bool) -> Result<String, Refusal> {
        self.pool = None;
        if export {
            // The export's engine and its scanner tools are made ready here, before any branch
            // starts, so every branch runs on `&self`.
            self.scanner_tools = None;
            self.engine_sync()?;
            self.prepare_scanner_tools(prover)?;
        }
        let path = self.logs.join("qualify.jobs");
        let pool = qualify_pool(&path, export, |need| {
            aterm_capacity::admit(need, aterm_capacity::Admit::Wait)
        })?;
        let words = format!(
            "{} cores in one Release claim, a jobserver of as many slots ({}) every branch's \
             children draw from{}",
            pool.jobs.slots(),
            pool.jobs.path().display(),
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
        let export = real.pool.as_ref().map_or_else(Vec::new, |pool| {
            (0..pool.export)
                .map_while(|_| pool.jobs.try_acquire().ok().flatten())
                .collect()
        });
        Box::new(RealBranches {
            real,
            export: std::sync::Mutex::new(export),
            tree_index: TreeIndexHandoff::default(),
        })
    }

    fn publish_begin(&mut self, prover: &Prover) -> Result<(), Refusal> {
        self.scanner_tools = None;
        self.engine_sync()?;
        self.prepare_scanner_tools(prover)?;
        self.prepare_source_signing()
    }

    fn source_publication(&mut self, c: &Candidate) -> Result<(), Refusal> {
        let slug = self.slug();
        let tag = format!("v{}", c.version);
        let release = self.release_json(&slug, &tag).map_err(|why| {
            Refusal::new(
                why,
                format!("run {RELEASE_COMMAND} again once the channel answers"),
            )
            .could_not_run()
        })?;
        let missing = source_attestation_findings(&release);
        if missing.is_empty() {
            return Ok(());
        }
        Err(Refusal::new(
            format!(
                "the source publication of {tag} is incomplete: {}",
                missing.join("; ")
            ),
            format!("finish pub sign-release aterm, then {RELEASE_COMMAND} (nothing was claimed)"),
        ))
    }

    fn discard_signed(&mut self, version: &str, build: u64) -> Vec<String> {
        crate::publish::discard_signed(&self.repo.join("dist"), version, build)
    }
    fn cut(
        &mut self,
        args: &[String],
        prover: &Prover,
        candidate: &Candidate,
        reserved: Option<&Reservation>,
    ) -> Ran {
        match self.cut_command(args, prover) {
            Ok(mut cmd) => {
                let real = &*self;
                with_reader_prebuild(
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
                                                        real.pool.as_ref().map(|pool| &pool.jobs),
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
        let (release, (pointer, site_tag)) = crate::cut_timing::independent_logged(
            Some(&log),
            ("after-cut release JSON", || read(self, &tag)),
            ("after-cut public readers", || {
                crate::cut_timing::independent_logged(
                    Some(&log),
                    ("after-cut latest pointer", || {
                        self.pointer_problems(&slug, &tag, version)
                    }),
                    ("after-cut site election", || self.site_tag(&slug)),
                )
            }),
        );
        let mut problems = match release {
            Ok(rel) => release_shape(&rel, version),
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
                        release_shape(&rel, v).into_iter().map(|p| {
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
    prebuild: impl FnOnce(&BranchStop) + Send,
    cut: impl FnOnce() -> Ran,
) -> Ran {
    let stop = BranchStop::default();
    std::thread::scope(|scope| {
        scope.spawn(|| prebuild(&stop));
        let verdict = cut();
        if verdict.code != 0 {
            stop.fire();
        }
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
struct PrefreezeWorker(Option<aterm_uds::spawnfd::Child>);

#[cfg(unix)]
impl PrefreezeWorker {
    fn settle(&mut self, reason: &str) {
        if let Some(mut child) = self.0.take() {
            let yielding = std::time::Instant::now();
            println!(
                "release-event: {}",
                aterm_json::json!({
                    "schema":1,"event":"prefreeze-yield-requested",
                    "worker_pid":child.id(),"reason":reason,
                })
            );
            aterm_uds::deadline::settle_child(&mut child);
            println!(
                "release-event: {}",
                aterm_json::json!({
                    "schema":1,"event":"prefreeze-yielded",
                    "settle_s":yielding.elapsed().as_secs_f64(),
                })
            );
        }
    }
}

#[cfg(unix)]
impl Drop for PrefreezeWorker {
    fn drop(&mut self) {
        self.settle("supervisor exit");
    }
}

#[cfg(unix)]
fn prepare_cached_cutter(
    real: &mut Real,
    tip: &str,
) -> Result<crate::release_tools::Cutter, String> {
    real.prefreeze_claim = Some(prefreeze_admission()?);
    let tip = if std::env::var_os(PREFREEZE_WORKER).is_some() {
        // The typed native boundary imports only the selected main object and
        // updates its tracking ref by CAS. Repeated preparation needs no tag
        // inventory and must use the source's recorded agh identity.
        (real.source_origin)(&real.repo)?.refresh_main()?
    } else {
        tip.to_owned()
    };
    let cache = real.release.join("cutter-cache");
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
    let mut compiler = real.cutter_workspace_at(&cache, &tip)?;
    compiler.prefreeze_claim = real.prefreeze_claim.take();
    // This standalone worker has not selected a prover yet. Resolve from the new
    // source's lock, independently of the ongoing cut's working directory and seal.
    std::env::set_current_dir(&compiler.repo).map_err(|e| e.to_string())?;
    crate::gates::arm_toolchain_lease();
    let prover = compiler.prover(None)?;
    compiler.frozen_cutter_here(&tip, &prover)
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
    let cache = aterm_verify::engine::release_dir(repo).join("cutter-cache");
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
    let mut worker = PrefreezeWorker(None);
    let mut worker_covered = false;
    let mut next = std::time::Instant::now();
    loop {
        if interrupt::caught().is_some() {
            return Ok(());
        }
        let rows = ledger.list().map_err(|e| e.to_string())?;
        let covered = rows.iter().any(|row| row.cover_marker() == line);
        if worker_covered && !covered {
            // A worker admitted beside the old line must not retain Release rank
            // between cuts. Settle it and let Background admission own the rest.
            worker.settle("old line ended; readmitting as Background");
            worker_covered = false;
            next = std::time::Instant::now();
        }
        let own = worker.0.as_ref().map(aterm_uds::spawnfd::Child::id);
        let state = prefreeze_traffic(&rows, line, own);
        if state != PrefreezeTraffic::Free {
            worker.settle(if state == PrefreezeTraffic::End {
                "next line claimed"
            } else {
                "Release stage or timing claim"
            });
            if state == PrefreezeTraffic::End {
                return Ok(());
            }
        } else if let Some(child) = worker.0.as_mut() {
            if let Some(status) = child.try_wait().map_err(|e| e.to_string())? {
                println!(
                    "release-event: {}",
                    aterm_json::json!({
                        "schema":1,"event":"prefreeze-finished","success":status.success(),
                    })
                );
                worker.0 = None;
                // Main can acquire cutter changes after READY. Keep following it
                // until the next cut; the worker fetches and Cargo judges the
                // delta in the same persistent checkout. Failures use the same
                // paced retry, without a request or a foreground build.
                next = std::time::Instant::now() + PREFREEZE_REFRESH;
            }
        } else if prefreeze_due(std::time::Instant::now(), next) {
            let mut cmd = command(executable);
            cmd.args(["release", "--prefreeze"])
                .current_dir(repo)
                .env_remove(AUTO_PREFREEZE)
                .env(PREFREEZE_WORKER, "1")
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
            worker.0 = Some(cmd.spawn().map_err(|e| e.to_string())?);
            worker_covered = covered;
        }
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
    let line = if !opts.dry_run && inherited_line.is_none() {
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
        Err(refusal) => Err(refusal.why),
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
mod tests {
    fn witnessed_fixture_wait(
        real: &super::Real,
        _candidate: &super::Candidate,
        _prover: &super::Prover,
        handoff: &super::TreeIndexHandoff,
        _stop: &super::BranchStop,
    ) -> Result<(), super::Refusal> {
        assert!(
            handoff.expected(),
            "the production build path kept the tree promise"
        );
        std::fs::write(real.repo.join("fixture-waited"), b"waited").unwrap();
        Ok(())
    }

    fn witnessed_fixture_stage(
        real: &super::Real,
        _step: super::Step,
        _candidate: &super::Candidate,
        _reserved: &super::Reservation,
        _prover: &super::Prover,
        _log: &std::path::Path,
        stop: Option<&super::BranchStop>,
    ) -> Result<String, super::Refusal> {
        assert!(real.repo.join("fixture-waited").is_file());
        assert!(
            stop.is_some(),
            "the production build path attaches its stop"
        );
        std::fs::write(real.repo.join("fixture-staged"), b"staged").unwrap();
        Ok("fixtures staged".into())
    }

    fn red_tree_verify(
        _real: &super::Real,
        _candidate: &super::Candidate,
        _prover: &super::Prover,
        _run: &crate::check_record::VerifyRun,
        _artifact: Option<&super::Artifact>,
        _stop: &super::BranchStop,
    ) -> Result<super::Ran, super::Refusal> {
        Err(super::Refusal::new("tree red", "fix it"))
    }

    /// The production orchestration, not only its helpers, waits before staging fixtures and
    /// closes the tree handoff before returning a red. NEGATIVE CONTROLS: removing the wait call
    /// makes the injected stage observe no marker; removing `with_tree_index_completion` leaves
    /// the production handoff Pending.
    #[test]
    fn production_build_and_tree_wiring_owns_the_fixture_handoff() {
        use super::*;

        let dir = aterm_tempfile::tempdir().unwrap();
        let repo = dir.path().join("repo");
        std::fs::create_dir(&repo).unwrap();
        let mut real = Real::new(&repo, Some(&repo.join("engine")), true, false).unwrap();
        real.await_fixture_index_op = witnessed_fixture_wait;
        real.stage_fixtures_run_op = witnessed_fixture_stage;
        let candidate = Candidate {
            commit: "11".repeat(20),
            version: "0.0.0".into(),
            why: "fixture".into(),
            bump_from: None,
            carry: Vec::new(),
        };
        let reservation = Reservation {
            build: 1,
            commit: "22".repeat(20),
            line: "1 0.0.0".into(),
        };
        let prover = Prover {
            bin: repo.join("prover/bin"),
            commit: "33".repeat(20),
            source: "fixture".into(),
        };
        let handoff = TreeIndexHandoff::default();
        handoff.expect();
        let built = real.build_branch(
            &candidate,
            &reservation,
            &prover,
            BuildWork {
                bundle: false,
                fixtures: true,
            },
            &handoff,
            &BranchStop::default(),
        );
        assert!(built.artifact.is_none());
        assert!(built.fixtures.is_some_and(|result| result.is_ok()));
        assert!(repo.join("fixture-staged").is_file());

        real.verify_half_op = red_tree_verify;
        let branches = RealBranches {
            real: &real,
            export: std::sync::Mutex::new(Vec::new()),
            tree_index: TreeIndexHandoff::default(),
        };
        branches.tree_index.expect();
        let stop = BranchStop::default();
        let run = crate::check_record::VerifyRun {
            half: crate::check_record::Half::Tree,
            checks: vec!["test".into()],
            passed_binaries: Vec::new(),
            passed_lint_packages: Vec::new(),
        };
        assert!(
            branches
                .verify(&candidate, &prover, &run, None, &stop)
                .is_err()
        );
        assert_eq!(
            branches.tree_index.completion(),
            TreeIndexCompletion::Stopped
        );
        assert!(!stop.fired(), "the graph has not classified the red yet");
    }

    /// Production obtains its artifact through the completion transition itself. NEGATIVE
    /// CONTROLS: an absent build and a failed build do not release admission; deleting the
    /// successful transition leaves `bundle_is_finished` false.
    #[test]
    fn a_successful_product_build_publishes_completion_before_returning_its_artifact() {
        use super::*;

        let completed = TreeIndexHandoff::default();
        let artifact = build_artifact_with_handoff(true, &completed, || Ok::<_, &str>(7));
        assert_eq!(artifact, Some(Ok(7)));
        assert!(completed.bundle_is_finished());

        let failed = TreeIndexHandoff::default();
        assert_eq!(
            build_artifact_with_handoff(true, &failed, || Err::<u8, _>("red")),
            Some(Err("red"))
        );
        assert!(!failed.bundle_is_finished());

        let absent = TreeIndexHandoff::default();
        assert_eq!(
            build_artifact_with_handoff(false, &absent, || Ok::<_, &str>(7)),
            None
        );
        assert!(!absent.bundle_is_finished());
    }

    /// A scheduled tree gets the opportunity to publish its exact executable index before the
    /// fixture path may compile. NEGATIVE CONTROLS: the former one-shot miss is `Compile`, not the
    /// `Ready` answer this handoff reaches; and a tree red published before the graph fires the
    /// build stop is already `Stopped`, never a fallback compile. A non-red completed tree with no
    /// index still falls back.
    #[test]
    fn fixture_index_waits_for_the_tree_compile_before_fallback() {
        use super::*;
        use std::cell::Cell;

        let handoff = TreeIndexHandoff::default();
        handoff.expect();
        let probes = Cell::new(0);
        let pauses = Cell::new(0);
        let answer = await_tree_index(
            &handoff,
            || {
                let probe = probes.get() + 1;
                probes.set(probe);
                probe == 2
            },
            || false,
            || pauses.set(pauses.get() + 1),
        );
        assert_eq!(answer, TreeIndexAnswer::Ready);
        assert_eq!((probes.get(), pauses.get()), (2, 1));
        let one_shot_miss = TreeIndexAnswer::Compile;
        assert_ne!(
            answer, one_shot_miss,
            "the former one-shot lookup compiles instead of observing the tree's handoff"
        );

        let missing = TreeIndexHandoff::default();
        missing.expect();
        let pauses = Cell::new(0);
        let answer = await_tree_index(
            &missing,
            || false,
            || false,
            || {
                pauses.set(pauses.get() + 1);
                missing.finish(TreeIndexCompletion::Compile);
            },
        );
        assert_eq!(answer, TreeIndexAnswer::Compile);
        assert_eq!(pauses.get(), 1);

        let red = TreeIndexHandoff::default();
        red.expect();
        let graph_stop = BranchStop::default();
        let refusal = with_tree_index_completion(&red, &graph_stop, || {
            Err(Refusal::new("tree tests failed", "fix the tests"))
        })
        .unwrap_err();
        assert_eq!(refusal.why, "tree tests failed");
        assert!(
            !graph_stop.fired(),
            "the graph has not classified the note yet"
        );
        assert_eq!(
            await_tree_index(&red, || false, || graph_stop.fired(), || unreachable!()),
            TreeIndexAnswer::Stopped,
            "the tree outcome closes the pre-stop race instead of authorizing fallback"
        );

        let stopped = TreeIndexHandoff::default();
        stopped.expect();
        assert_eq!(
            await_tree_index(&stopped, || true, || true, || unreachable!()),
            TreeIndexAnswer::Stopped,
            "a sibling red starts no new fixture work even when an index exists"
        );
    }

    /// NEGATIVE CONTROLS: ignoring the shape verdict starts a writer; returning
    /// immediately on the site's refusal leaves the independent tap unrun.
    #[test]
    fn public_shape_precedes_writers_and_each_writer_joins_on_refusal() {
        use super::*;
        use std::sync::atomic::{AtomicUsize, Ordering};
        let dir = aterm_tempfile::tempdir().unwrap();
        let log = dir.path().join("after-cut.log");
        let calls = AtomicUsize::new(0);
        let site = || {
            calls.fetch_add(1, Ordering::SeqCst);
            Err(Refusal::new("site refused", "retry"))
        };
        let tap = || {
            calls.fetch_add(1, Ordering::SeqCst);
            Ok("tap aligned".into())
        };
        let refused = distribute_after_shape(
            &log,
            Err(Refusal::new("bad public shape", "repair")),
            site,
            tap,
        )
        .unwrap_err();
        assert_eq!(refused.why, "bad public shape");
        assert_eq!(calls.load(Ordering::SeqCst), 0);
        let refused = distribute_after_shape(&log, Ok(()), site, tap).unwrap_err();
        assert_eq!(refused.why, "site refused");
        assert_eq!(calls.load(Ordering::SeqCst), 2);
    }

    /// Credential-free scheduling measurement. The engine child is a native
    /// no-op at the existing tap seam; reads use agh-fake's loopback server and
    /// real curl with an injected 40 ms service cost. This is not a real cut.
    #[cfg(unix)]
    #[test]
    #[ignore = "explicit loopback cut/after-cut gate scheduling measurement"]
    fn independent_cut_gate_fixture_walls() {
        use super::*;
        use aterm_update_core::github_rest::{Direct, Origins, Spawn};
        use std::time::Instant;
        struct Service;
        impl Spawn for Service {
            fn run(
                &self,
                cmd: &mut Command,
                stdin: Option<&[u8]>,
            ) -> std::io::Result<std::process::Output> {
                std::thread::sleep(std::time::Duration::from_millis(40));
                Direct.run(cmd, stdin)
            }
        }
        let fake = agh_fake::FakeGithub::start();
        let token = agh_fake::token("cut-gate-fixture");
        fake.add_token(&token, "gate-fixture-reader");
        fake.add_repo("gate-fixture/aterm", agh_fake::RepoSpec::public());
        let ports = fake.ports();
        let _route = crate::github::Fixture {
            origins: Origins::loopback(ports.api, ports.uploads, ports.web, ports.cdn),
            token: Some(token),
            spawn: Some(std::sync::Arc::new(Service)),
        }
        .enter();
        let dir = aterm_tempfile::tempdir().unwrap();
        let engine = dir.path().join("engine");
        std::fs::create_dir_all(engine.join("bin")).unwrap();
        std::os::unix::fs::symlink("/usr/bin/true", engine.join("bin/pub")).unwrap();
        let mut real = Real::new(dir.path(), Some(&engine), true, false).unwrap();
        real.prerequisites = Some(crate::managed_prerequisites::Tools::Simulated);
        let read = || {
            crate::github::get("repos/gate-fixture/aterm/releases?per_page=30").unwrap();
        };
        let tap = || {
            real.align_tap("v0.184.0").unwrap();
        };
        read();
        tap();
        for (take, parallel) in [false, true, true, false].into_iter().enumerate() {
            let started = Instant::now();
            if parallel {
                let commit = || {
                    crate::cut_timing::independent(
                        ("bundle", read),
                        ("handoff", || {
                            crate::cut_timing::independent(("fixtures", read), ("policy", read));
                        }),
                    )
                };
                let authority = || {
                    crate::cut_timing::independent(
                        ("availability", || {
                            crate::cut_timing::independent(("lock", read), ("target", read));
                        }),
                        ("floors", || {
                            crate::cut_timing::independent(
                                ("head and signature", || {
                                    crate::cut_timing::independent(
                                        ("head", read),
                                        ("signature", read),
                                    );
                                }),
                                ("roster and ledger", || {
                                    crate::cut_timing::independent(
                                        ("roster", read),
                                        ("ledger", read),
                                    );
                                }),
                            );
                        }),
                    )
                };
                crate::cut_timing::independent(("commit", commit), ("authority", authority));
                crate::cut_timing::independent(("signed bytes", read), ("published floor", read));
            } else {
                for _ in 0..11 {
                    read();
                }
            }
            println!(
                "gate-fixture step=cut take={take} parallel={parallel} wall_s={:.6}",
                started.elapsed().as_secs_f64()
            );
            let started = Instant::now();
            let distribution = || {
                // Public shape precedes both distribution writes.
                if parallel {
                    crate::cut_timing::independent(
                        ("release JSON", read),
                        ("public readers", || {
                            crate::cut_timing::independent(("pointer", read), ("election", read));
                        }),
                    );
                } else {
                    for _ in 0..3 {
                        read();
                    }
                }
                if parallel {
                    crate::cut_timing::independent(("site", read), ("tap", tap));
                } else {
                    read();
                    tap();
                }
            };
            let reader = || {
                read();
            };
            if parallel {
                crate::cut_timing::independent(("distribution", distribution), ("reader", reader));
            } else {
                distribution();
                reader();
            }
            println!(
                "gate-fixture step=after-cut take={take} parallel={parallel} wall_s={:.6}",
                started.elapsed().as_secs_f64()
            );
        }
    }

    #[test]
    fn an_interrupted_cutter_checkout_is_reconstructed_at_the_same_path() {
        use super::*;
        let temp = aterm_tempfile::tempdir().unwrap();
        let repo = temp.path().join("repo");
        std::fs::create_dir_all(&repo).unwrap();
        std::fs::write(repo.join("input"), "committed input").unwrap();
        let git = GitCli::new(&repo);
        for args in [
            vec!["init", "-q"],
            vec!["add", "."],
            vec![
                "-c",
                "user.name=Test",
                "-c",
                "user.email=test@example.invalid",
                "commit",
                "-qm",
                "fixture",
            ],
        ] {
            crate::ledger::git_ok(&git, &args).unwrap();
        }
        let commit = crate::ledger::git_ok(&git, &["rev-parse", "HEAD"])
            .unwrap()
            .stdout_utf8()
            .trim()
            .to_owned();
        let mut real = Real::new(&repo, Some(&repo), true, false).unwrap();
        real.state = temp.path().join("state");
        let cache = temp.path().join("cache");
        std::fs::create_dir_all(&cache).unwrap();
        let compiler = real.cutter_workspace_at(&cache, &commit).unwrap();
        let source = compiler.repo.clone();
        drop(compiler);
        std::fs::write(source.join(".git/index.lock"), "interrupted checkout").unwrap();
        std::fs::write(source.join("input"), "partial checkout").unwrap();
        std::fs::remove_file(cache.join("source-ready")).unwrap();
        let compiler = real.cutter_workspace_at(&cache, &commit).unwrap();
        assert_eq!(compiler.repo, source);
        assert!(!source.join(".git/index.lock").exists());
        assert_eq!(
            std::fs::read_to_string(source.join("input")).unwrap(),
            "committed input"
        );
        crate::release_tools::clean_at(&source, &commit).unwrap();
    }

    #[cfg(unix)]
    #[test]
    fn automatic_prefreeze_follows_between_cuts_and_yields_to_stages_and_windows() {
        use super::*;
        let row = |pid, need| aterm_capacity::Record {
            accounting: aterm_capacity::Accounting::default(),
            pid,
            start_ms: 1,
            seq: 1,
            checkout: String::new(),
            need,
            timing: Default::default(),
            held: None,
            credit: Vec::new(),
            wrote: None,
            next_ms: None,
        };
        let line = row(
            1,
            aterm_capacity::Need::new("release line").rank(aterm_capacity::Rank::Release),
        );
        let marker = line.cover_marker();
        let worker = row(2, prefreeze_need().rank(aterm_capacity::Rank::Release));
        assert_eq!(
            prefreeze_traffic(&[line.clone(), worker.clone()], &marker, Some(2)),
            PrefreezeTraffic::Free
        );
        let mut stage = row(
            1,
            aterm_capacity::Need::new("stage")
                .rank(aterm_capacity::Rank::Release)
                .mem_gib(1),
        );
        stage.seq = 2;
        assert_eq!(
            prefreeze_traffic(&[line.clone(), worker.clone(), stage], &marker, Some(2)),
            PrefreezeTraffic::Yield
        );
        let window = row(3, aterm_capacity::Need::new("window").quiet(f64::MAX));
        assert_eq!(
            prefreeze_traffic(&[line.clone(), worker.clone(), window], &marker, Some(2)),
            PrefreezeTraffic::Yield
        );
        let next = row(
            3,
            aterm_capacity::Need::new("release line").rank(aterm_capacity::Rank::Release),
        );
        assert_eq!(
            prefreeze_traffic(&[line, worker.clone(), next.clone()], &marker, Some(2)),
            PrefreezeTraffic::End
        );
        assert_eq!(
            prefreeze_traffic(&[next], &marker, None),
            PrefreezeTraffic::End,
            "a new cut must cancel preparation after the old cut exited"
        );
        assert_eq!(
            prefreeze_traffic(&[worker], &marker, Some(2)),
            PrefreezeTraffic::Free,
            "the old cut ending must not stop following main"
        );
        assert_eq!(
            prefreeze_traffic(&[], &marker, None),
            PrefreezeTraffic::Free
        );
        let now = std::time::Instant::now();
        let next = now + PREFREEZE_REFRESH;
        assert!(!prefreeze_due(now, next));
        assert!(prefreeze_due(next, next));
    }

    #[cfg(unix)]
    #[test]
    fn prefreeze_handoff_waits_ninety_seconds_with_progress_but_refuses_a_stall() {
        use super::*;
        use std::cell::Cell;
        use std::time::{Duration, Instant};
        for progressing in [true, false] {
            let seconds = Cell::new(0_u64);
            let began = Instant::now();
            let mut logs = Vec::new();
            let result = wait_prefreeze(
                || {
                    let elapsed = seconds.get();
                    (!progressing || elapsed < 90).then(|| {
                        (
                            4321,
                            "same birth".into(),
                            Some(if progressing { elapsed / 30 } else { 0 }),
                        )
                    })
                },
                || began + Duration::from_secs(seconds.get()),
                || seconds.set(seconds.get() + 1),
                || false,
                |message| logs.push(message.to_owned()),
            );
            if progressing {
                result.unwrap();
                assert_eq!(seconds.get(), 90);
                assert!(logs[0].contains("pid 4321"));
                assert!(logs[1].contains("yielded after 90.000 s"));
            } else {
                assert_eq!(seconds.get(), 60);
                assert!(
                    result
                        .unwrap_err()
                        .contains("pid 4321 made no descendant CPU/I/O progress")
                );
                assert_eq!(logs.len(), 1);
            }
        }
    }

    #[cfg(unix)]
    #[test]
    fn prefreeze_compiler_stays_in_the_workers_cancellation_group() {
        use super::*;
        for prefreeze in [true, false] {
            let mut cmd = command("/bin/cat");
            cmd.stdin(Stdio::piped()).stdout(Stdio::null());
            step_process_group(&mut cmd, prefreeze);
            let mut child = cmd.spawn().unwrap();
            let pid = i32::try_from(child.id()).unwrap();
            // SAFETY: these calls only read process-group identities of this process
            // and its owned, unreaped child.
            let (group, parent) = unsafe { (libc::getpgid(pid), libc::getpgrp()) };
            assert_eq!(group, if prefreeze { parent } else { pid });
            drop(child.stdin.take());
            assert!(child.wait().unwrap().success());
        }
    }

    /// Real worker and supervisor, private traffic ledger: no cut or publication.
    #[cfg(target_os = "macos")]
    #[test]
    #[ignore = "explicit real prefreeze cancellation measurement"]
    fn measure_prefreeze_yield() {
        use super::*;
        use std::time::{Duration, Instant};
        let cwd = std::env::current_dir().unwrap();
        let source = PathBuf::from(
            crate::ledger::git_ok(&GitCli::new(&cwd), &["rev-parse", "--show-toplevel"])
                .unwrap()
                .stdout_utf8()
                .trim(),
        );
        let original = GitCli::new(&source);
        let temp = aterm_tempfile::tempdir().unwrap();
        let repo = temp.path().join("repo");
        crate::ledger::git_ok(
            &original,
            &[
                "clone",
                "--quiet",
                "--shared",
                source.to_str().unwrap(),
                repo.to_str().unwrap(),
            ],
        )
        .unwrap();
        let repo = repo.canonicalize().unwrap();
        let git = GitCli::new(&repo);
        let url = crate::ledger::git_ok(&original, &["remote", "get-url", "origin"])
            .unwrap()
            .stdout_utf8();
        crate::ledger::git_ok(&git, &["remote", "set-url", "origin", url.trim()]).unwrap();
        crate::ledger::git_ok(
            &git,
            &[
                "config",
                "submodule.vendor/astream.url",
                source.join("vendor/astream").to_str().unwrap(),
            ],
        )
        .unwrap();
        crate::ledger::git_ok(
            &git,
            &[
                "-c",
                "protocol.file.allow=always",
                "submodule",
                "update",
                "--init",
                "vendor/astream",
            ],
        )
        .unwrap();
        let ledger = aterm_capacity::Ledger::at(temp.path().join("traffic"));
        let executable = aterm_tempfile::bin_exe("aterm-release");
        std::thread::scope(|scope| {
            let supervisor =
                scope.spawn(|| supervise_prefreeze_in(&repo, "old-line", &ledger, &executable));
            let mut last = Instant::now();
            let mut probe = aterm_uds::deadline::ProcessProgressProbe::default();
            let mut evidence = None;
            let compiler = loop {
                let sweep = aterm_sysprobe::processes().unwrap();
                let mut descendants = std::collections::BTreeSet::from([std::process::id()]);
                loop {
                    let before = descendants.len();
                    for process in &sweep.procs {
                        if descendants.contains(&process.ppid) {
                            descendants.insert(process.pid);
                        }
                    }
                    if descendants.len() == before {
                        break;
                    }
                }
                if let Some(compiler) = sweep.procs.iter().find(|process| {
                    descendants.contains(&process.pid)
                        && process.name == "trustc"
                        && aterm_sysprobe::kproc::argv(process.pid).is_some_and(|args| {
                            args.iter().any(|arg| arg.starts_with(b"--emit="))
                                && !args.iter().any(|arg| arg.starts_with(b"--print"))
                        })
                }) {
                    let args = aterm_sysprobe::kproc::argv(compiler.pid).unwrap();
                    let unit = args
                        .windows(2)
                        .find(|pair| pair[0] == b"--crate-name")
                        .unwrap();
                    println!(
                        "prefreeze-active-compiler unit={}",
                        String::from_utf8_lossy(&unit[1])
                    );
                    break Some(compiler.pid);
                }
                let progress = probe.read_descendants(std::process::id());
                if progress.is_none() || progress != evidence {
                    last = Instant::now();
                }
                evidence = progress;
                if supervisor.is_finished() || last.elapsed() >= Duration::from_secs(60) {
                    break None;
                }
                std::thread::sleep(Duration::from_millis(100));
            };
            let claimed = Instant::now();
            let _line = aterm_capacity::admit_with(
                &ledger,
                &aterm_capacity::Fixed::new(aterm_capacity::Reading::of_gib(48, 40, 4)),
                aterm_capacity::Need::new("release line").rank(aterm_capacity::Rank::Release),
                aterm_capacity::Admit::Wait,
                aterm_capacity::Pace::default(),
                &mut |_| {},
            )
            .unwrap();
            supervisor.join().unwrap().unwrap();
            let compiler =
                compiler.expect("no compiler appeared before preparation stopped progressing");
            println!(
                "prefreeze-yield-measure compiler_pid={compiler} claim_to_holder_removed_s={:.6}",
                claimed.elapsed().as_secs_f64()
            );
            assert!(
                !repo
                    .join(".git/aterm-release/cutter-cache/prefreeze-running")
                    .exists()
            );
            let remaining = aterm_sysprobe::kproc::record_read(compiler).unwrap();
            println!("prefreeze-yield-compiler-terminal {remaining:?}");
            assert!(
                remaining.is_none_or(|record| record.zombie),
                "a live compiler retained its build lock after yield"
            );
        });
    }

    /// Small real Cargo graph, no release, credential, service or product execution.
    /// The instrument counts compiled units, rather than interpreting elapsed time as a gain.
    #[cfg(target_os = "macos")]
    #[test]
    #[ignore = "explicit local pre-freeze timeline measurement"]
    fn measure_automatic_prefreeze_timeline() {
        use super::*;
        let temp = aterm_tempfile::tempdir().unwrap();
        let repo = temp.path().join("repo");
        std::fs::create_dir_all(repo.join("crates/aterm-release/src")).unwrap();
        std::fs::create_dir_all(repo.join("dep/src")).unwrap();
        std::fs::create_dir_all(repo.join(".cargo")).unwrap();
        std::fs::write(repo.join(".gitignore"), "/target/\n").unwrap();
        std::fs::write(
            repo.join(".cargo/config.toml"),
            "[unstable]\nchecksum-freshness=true\n",
        )
        .unwrap();
        std::fs::create_dir_all(repo.join("app/src")).unwrap();
        std::fs::write(repo.join("app/src/main.rs"), "fn main() {}\n").unwrap();
        std::fs::write(
            repo.join("app/Cargo.toml"),
            "[package]\nname='app'\nversion.workspace=true\nedition='2021'\n",
        )
        .unwrap();
        std::fs::write(repo.join("Cargo.toml"), "[workspace]\nmembers=['dep','app','crates/aterm-release']\nresolver='2'\n[workspace.package]\nversion='1.0.0'\n[profile.gate]\ninherits='dev'\n").unwrap();
        std::fs::write(repo.join("crates/aterm-release/Cargo.toml"), "[package]\nname='aterm-release'\nversion='0.0.0'\nedition='2021'\n[dependencies]\ncutter-dep={path='../../dep'}\n").unwrap();
        std::fs::write(
            repo.join("dep/Cargo.toml"),
            "[package]\nname='cutter-dep'\nversion='0.0.0'\nedition='2021'\n",
        )
        .unwrap();
        std::fs::write(
            repo.join("dep/src/lib.rs"),
            "pub fn marker() -> u32 { 1 }\n",
        )
        .unwrap();
        let program =
            "fn main() { let _ = cutter_dep::marker(); println!(\"aterm-release tool 1\"); }\n";
        std::fs::write(repo.join("crates/aterm-release/src/main.rs"), program).unwrap();
        std::fs::write(repo.join("Cargo.lock"), "version=4\n[[package]]\nname='app'\nversion='1.0.0'\n[[package]]\nname='aterm-release'\nversion='0.0.0'\ndependencies=['cutter-dep']\n[[package]]\nname='cutter-dep'\nversion='0.0.0'\n").unwrap();
        let git = GitCli::new(&repo);
        let git_run = |args: &[&str]| crate::ledger::git_ok(&git, args).unwrap();
        git_run(&["init", "-q"]);
        let commit = || {
            git_run(&["add", "."]);
            git_run(&[
                "-c",
                "user.name=Test",
                "-c",
                "user.email=test@example.invalid",
                "commit",
                "-qm",
                "fixture",
            ]);
            git_run(&["rev-parse", "HEAD"])
                .stdout_utf8()
                .trim()
                .to_owned()
        };
        let base = commit();
        let bin = crate::gates::trust_stage2_bin().unwrap();
        let prover = Prover {
            bin,
            commit: "0".repeat(40),
            source: "fixture pinned Trust".into(),
        };
        for (take, prepared) in [false, true, true, false].into_iter().enumerate() {
            git_run(&["checkout", "--quiet", "--detach", &base]);
            let state = temp.path().join(format!("take-{take}-{prepared}"));
            let mut real = Real::new(&repo, Some(&repo), true, false).unwrap();
            real.scratch_logs = false;
            real.release = state.join("release");
            real.state = state.join("state");
            real.logs = state.join("logs");
            std::fs::create_dir_all(&real.logs).unwrap();
            real.build_inputs = || Ok("f".repeat(64));
            real.arm_inputs = |_, _| {};
            let n = real.frozen_cutter(&base, &prover).unwrap();
            std::fs::write(
                repo.join("crates/aterm-release/src/main.rs"),
                format!("{program}// main moves during cut N\n"),
            )
            .unwrap();
            let moved = commit();
            if prepared {
                let compiler = real
                    .cutter_workspace_at(&real.release.join("cutter-cache"), &moved)
                    .unwrap();
                compiler.frozen_cutter_here(&moved, &prover).unwrap();
                assert_eq!(
                    crate::release_tools::recorded_cutter(&real.release, &base).unwrap(),
                    n,
                    "preparation must not overwrite the current cut's proof"
                );
            }
            std::fs::write(repo.join("notes"), "unrelated main movement\n").unwrap();
            let next = commit();
            let log = real.step_log(real.step);
            std::fs::write(&log, "").unwrap();
            let next_cutter = real.frozen_cutter(&next, &prover).unwrap();
            let compiled = |log: &Path| -> u64 {
                std::fs::read_to_string(log)
                    .unwrap()
                    .lines()
                    .filter_map(|line| line.strip_prefix("release-event: "))
                    .filter_map(|line| aterm_json::from_str::<aterm_json::Value>(line).ok())
                    .filter(|event| event["event"].as_str() == Some("frozen-tool-compile"))
                    .map(|event| event["compiled_units"].as_u64().unwrap())
                    .sum()
            };
            let first = compiled(&log);
            assert_eq!(first, if prepared { 0 } else { 1 });
            let hop = |label: &str, expected: u64| {
                let text = std::fs::read_to_string(&log).unwrap();
                assert_eq!(compiled(&log), expected, "{label}");
                println!(
                    "prefreeze-timeline take={take} prepared={prepared} hop={label} compiled={expected}"
                );
                for line in text.lines().filter(|line| {
                    line.contains("fingerprint") || line.starts_with("release-event: ")
                }) {
                    println!("{line}");
                }
            };
            hop("unchanged", first);
            let manifest = std::fs::read_to_string(repo.join("Cargo.toml")).unwrap();
            let lock = std::fs::read_to_string(repo.join("Cargo.lock")).unwrap();
            std::fs::write(repo.join("Cargo.toml"), manifest.replace("1.0.0", "2.0.0")).unwrap();
            std::fs::write(repo.join("Cargo.lock"), lock.replace("1.0.0", "2.0.0")).unwrap();
            let bump = commit();
            std::fs::write(&log, "").unwrap();
            let bumped = real.frozen_cutter(&bump, &prover).unwrap();
            assert_eq!(bumped.built_from, next_cutter.built_from);
            assert_eq!(bumped.inputs, next_cutter.inputs);
            hop("bump-only", 0);
            if prepared {
                assert_eq!(next_cutter.built_from, moved);
                std::fs::write(
                    repo.join("crates/aterm-release/src/main.rs"),
                    format!("{program}// another cutter delta\n"),
                )
                .unwrap();
                let delta = commit();
                std::fs::write(&log, "").unwrap();
                let delta_cutter = real.frozen_cutter(&delta, &prover).unwrap();
                assert_eq!(compiled(&log), 1, "unchanged dependency stays fresh");
                assert_eq!(delta_cutter.built_from, delta);
                assert_ne!(delta_cutter.inputs, next_cutter.inputs);
                hop("real-cutter-change", 1);
                std::fs::write(
                    repo.join("dep/src/lib.rs"),
                    "pub fn marker() -> u32 { 2 }\n",
                )
                .unwrap();
                let manifest = std::fs::read_to_string(repo.join("Cargo.toml")).unwrap();
                let lock = std::fs::read_to_string(repo.join("Cargo.lock")).unwrap();
                std::fs::write(repo.join("Cargo.toml"), manifest.replace("2.0.0", "3.0.0"))
                    .unwrap();
                std::fs::write(repo.join("Cargo.lock"), lock.replace("2.0.0", "3.0.0")).unwrap();
                let delta = commit();
                std::fs::write(&log, "").unwrap();
                let cutter = real.frozen_cutter(&delta, &prover).unwrap();
                assert_eq!(
                    cutter.built_from, delta,
                    "a bump carrying a real input cannot adopt another input's pre-freeze"
                );
                assert_ne!(cutter.inputs, bumped.inputs);
                hop("real-dependency-change", 2);
            }
            println!("prefreeze-timeline prepared={prepared} next_freeze_compiled_units={first}");
        }
    }

    /// Read-only native origin observations and local bump writes in a disposable
    /// clone. Deliberately stops before the native ref write or any publication.
    #[test]
    #[ignore = "explicit local bump attribution; reads configured source origin"]
    fn measure_local_bump_parts() {
        let source = std::env::current_dir().unwrap();
        let original = GitCli::new(&source);
        let url = crate::ledger::git_ok(&original, &["remote", "get-url", "origin"])
            .unwrap()
            .stdout_utf8();
        let temp = aterm_tempfile::tempdir().unwrap();
        let repo = temp.path().join("repo");
        crate::ledger::git_ok(
            &original,
            &[
                "clone",
                "--quiet",
                "--shared",
                source.to_str().unwrap(),
                repo.to_str().unwrap(),
            ],
        )
        .unwrap();
        let repo = repo.canonicalize().unwrap();
        let git = GitCli::new(&repo);
        crate::ledger::git_ok(&git, &["remote", "set-url", "origin", url.trim()]).unwrap();
        crate::ledger::git_ok(
            &git,
            &[
                "config",
                "submodule.vendor/astream.url",
                source.join("vendor/astream").to_str().unwrap(),
            ],
        )
        .unwrap();
        crate::ledger::git_ok(
            &git,
            &[
                "-c",
                "protocol.file.allow=always",
                "submodule",
                "update",
                "--init",
                "vendor/astream",
            ],
        )
        .unwrap();
        let mut real = Real::new(&repo, Some(&source), true, false).unwrap();
        let prover = real.prover(None).unwrap();
        let from = real.version_at("HEAD").unwrap();
        let to = crate::publish::bump_minor_release(&from).unwrap();
        crate::cut_timing::measure("fixture-bump-origin", || {
            crate::publish::native_origin(&repo).unwrap()
        });
        let editor = PathBuf::from(std::env::var_os("ATERM_RELEASE_BUMP_EDITOR").unwrap());
        crate::cut_timing::measure("fixture-bump-editor", || {
            let mut cmd = real.child(editor, None);
            cmd.arg(&repo).args([&to, &from]);
            assert_eq!(real.logged(Step::Commit, &mut cmd).code, 0);
        });
        crate::cut_timing::measure("fixture-bump-lockfiles", || {
            let changed = bump_nested_locks(
                &repo,
                real.git_paths(&["ls-files", "-z", "--", "Cargo.lock", "*/Cargo.lock"]),
                &from,
                &to,
            )
            .unwrap();
            real.validate_bumped_locks(&prover, &changed).unwrap();
        });
        crate::cut_timing::measure("fixture-bump-harness", || {
            // A libtest image has no credential-helper dispatch. Use the real
            // diagnostic CLI, so its native origin uses its own typed helper.
            let cli = PathBuf::from(std::env::var_os("ATERM_RELEASE_DIAGNOSTIC").unwrap());
            let mut cmd = real.child(cli, None);
            cmd.arg("harness-bump");
            assert_eq!(real.logged(Step::Commit, &mut cmd).code, 0);
        });
        crate::cut_timing::measure("fixture-bump-commit", || {
            crate::ledger::git_ok(
                &git,
                &[
                    "-c",
                    "user.name=Test",
                    "-c",
                    "user.email=test@example.invalid",
                    "-c",
                    "commit.gpgsign=false",
                    "commit",
                    "-qam",
                    "Fixture bump (never pushed)",
                ],
            )
            .unwrap();
        });
        // Keep this mutable so Real's destructor owns all its ordinary cleanup.
        real.scratch_logs = true;
    }

    /// Replay exact historical compiler graphs in one persistent private checkout.
    /// The seal is fixed fixture evidence; this measures compiler reuse, not an SDK walk.
    #[test]
    #[ignore = "explicit full cutter rebuild attribution"]
    fn measure_real_cutter_rebuilds() {
        let repo = std::env::current_dir().unwrap().canonicalize().unwrap();
        let state = PathBuf::from(std::env::var_os("ATERM_RELEASE_REBUILD_STATE").unwrap());
        assert!(state.is_absolute());
        let commits = std::env::var("ATERM_RELEASE_REBUILD_COMMITS").unwrap();
        let mut real = Real::new(&repo, Some(&repo), true, false).unwrap();
        real.state = state.join("state");
        real.release = state.join("release");
        real.logs = state.join("logs");
        real.build_inputs = || Ok("f".repeat(64));
        real.arm_inputs = |_, _| {};
        std::fs::create_dir_all(&real.logs).unwrap();
        let prover = real.prover(None).unwrap();
        for rev in commits.split(',') {
            let commit = crate::ledger::git_ok(&real.git(), &["rev-parse", rev])
                .unwrap()
                .stdout_utf8();
            let compiler = real
                .cutter_workspace_at(&real.release.join("cutter-cache"), commit.trim())
                .unwrap();
            let log = compiler.step_log(compiler.step);
            std::fs::write(&log, "").unwrap();
            let cutter = compiler.frozen_cutter_here(commit.trim(), &prover).unwrap();
            std::fs::copy(&log, state.join(format!("{rev}.log"))).unwrap();
            println!("cutter-rebuild rev={rev} built_from={}", cutter.built_from);
            for line in std::fs::read_to_string(&log)
                .unwrap()
                .lines()
                .filter(|line| line.contains("fingerprint") || line.starts_with("release-event: "))
            {
                println!("{line}");
            }
        }
    }

    #[cfg(unix)]
    #[test]
    fn prefreeze_reserves_background_build_and_timing_exclusion() {
        use super::*;
        let need = prefreeze_need();
        assert_eq!(need.rank, aterm_capacity::Rank::Background);
        assert!(need.mem > 0 && need.cores > 0 && need.covers);
        assert_eq!(need.quiet, Some(f64::MAX));
        // The ordinary cut path carries Release rank; the preparation must remove it
        // from every child, retain the admitted build cover and bound compiler jobs.
        let dir = aterm_tempfile::tempdir().unwrap();
        let real = Real::new(dir.path(), Some(dir.path()), true, false).unwrap();
        let mut cmd = real.child("targo", None);
        assert!(
            cmd.get_envs()
                .any(|(key, val)| key == aterm_capacity::RELEASE_RUN_ENV && val.is_some())
        );
        configure_prefreeze_child(&mut cmd, "fixture-cover", 3);
        let env: std::collections::BTreeMap<_, _> = cmd.get_envs().collect();
        assert_eq!(
            env[std::ffi::OsStr::new(aterm_capacity::RELEASE_RUN_ENV)],
            None
        );
        assert_eq!(
            env[std::ffi::OsStr::new(aterm_capacity::COVER_ENV)],
            Some(std::ffi::OsStr::new("fixture-cover"))
        );
        assert_eq!(
            env[std::ffi::OsStr::new("CARGO_BUILD_JOBS")],
            Some(std::ffi::OsStr::new("3"))
        );
    }

    /// Real offline resolution is the oracle. Omitting the rewrite must fail locked
    /// metadata, and an incorrect rewritten version must fail it too.
    #[cfg(unix)]
    #[test]
    fn version_only_lock_rewrite_matches_resolver_bytes() {
        use super::*;
        let scratch = aterm_tempfile::tempdir().unwrap();
        let repo = scratch.path().canonicalize().unwrap();
        for dir in ["leaf/src", "nested/src"] {
            std::fs::create_dir_all(repo.join(dir)).unwrap();
            std::fs::write(repo.join(dir).join("lib.rs"), "").unwrap();
        }
        let root = "[workspace]\nmembers = [\"leaf\"]\nexclude = [\"nested\"]\nresolver = \"2\"\n[workspace.package]\nversion = \"0.147.0\"\n";
        std::fs::write(repo.join("Cargo.toml"), root).unwrap();
        std::fs::write(
            repo.join("leaf/Cargo.toml"),
            "[package]\nname = \"bump-leaf\"\nversion.workspace = true\nedition = \"2021\"\n",
        )
        .unwrap();
        std::fs::write(repo.join("nested/Cargo.toml"), "[workspace]\n[package]\nname = \"consumer\"\nversion = \"0.0.0\"\nedition = \"2021\"\n[dependencies]\nbump-leaf = { path = \"../leaf\" }\n").unwrap();
        let targo = crate::gates::trust_stage2_bin().unwrap().join("targo");
        let run = |manifest: &Path, args: &[&str]| {
            let mut cmd = command(&targo);
            cmd.current_dir(&repo)
                .args(args)
                .arg("--manifest-path")
                .arg(manifest);
            cmd.output().unwrap()
        };
        let locks = ["Cargo.lock", "nested/Cargo.lock"];
        let mut old = Vec::new();
        for lock in locks {
            let manifest = repo.join(lock).with_file_name("Cargo.toml");
            assert!(
                run(&manifest, &["update", "--workspace", "--offline"])
                    .status
                    .success()
            );
            old.push(std::fs::read_to_string(repo.join(lock)).unwrap());
        }
        std::fs::write(repo.join("Cargo.toml"), root.replace("0.147.0", "0.148.0")).unwrap();
        let metadata = ["metadata", "--locked", "--offline", "--format-version", "1"];
        for lock in locks {
            assert!(
                !run(&repo.join(lock).with_file_name("Cargo.toml"), &metadata)
                    .status
                    .success(),
                "unmodified {lock} must be stale"
            );
        }
        assert_eq!(
            bump_nested_locks(
                &repo,
                Ok(locks.map(str::to_owned).to_vec()),
                "0.147.0",
                "0.148.0"
            )
            .unwrap(),
            locks
        );
        for (lock, original) in locks.iter().zip(&old) {
            let manifest = repo.join(lock).with_file_name("Cargo.toml");
            let expected = lock_follows_bump(original, "0.147.0", "0.148.0").unwrap();
            assert_eq!(std::fs::read_to_string(repo.join(lock)).unwrap(), expected);
            assert!(run(&manifest, &metadata).status.success());
            assert!(
                run(&manifest, &["update", "--workspace", "--offline"])
                    .status
                    .success()
            );
            assert_eq!(std::fs::read_to_string(repo.join(lock)).unwrap(), expected);
            std::fs::write(repo.join(lock), expected.replace("0.148.0", "0.149.0")).unwrap();
            assert!(
                !run(&manifest, &metadata).status.success(),
                "wrong version must be refused"
            );
        }
    }
    /// NEGATIVE CONTROLS: waiting at start blocks on sleep; dropping without stopping and
    /// reaping leaves the PID alive. No release executable, signer or credential is used.
    #[cfg(unix)]
    #[test]
    fn early_packaging_child_is_owned_until_join_or_cancel() {
        use super::*;
        use crate::release::PendingSign as _;
        let dir = aterm_tempfile::tempdir().unwrap();
        let real = Real::new(dir.path(), Some(dir.path()), true, false).unwrap();
        for cancel in [false, true] {
            let mut cmd = command("/bin/sleep");
            cmd.arg("60");
            let mut pending = real.start_preparing_sign(cmd).unwrap();
            let pid = pending.child.as_ref().unwrap().id();
            assert!(
                pending
                    .child
                    .as_mut()
                    .unwrap()
                    .try_wait()
                    .unwrap()
                    .is_none()
            );
            if cancel {
                drop(pending);
            } else {
                pending.stop().fire();
                let ran = Box::new(pending).finish();
                assert_ne!(ran.code, 0);
                assert!(
                    ran.detail
                        .iter()
                        .any(|line| line.contains("step-child-finished"))
                );
            }
            // SAFETY: signal zero only queries this test child's PID after it was reaped.
            assert_ne!(
                unsafe { libc::kill(libc::pid_t::try_from(pid).unwrap(), 0) },
                0
            );
        }
    }

    #[cfg(unix)]
    #[test]
    fn release_signal_disposition_probe() {
        let Ok(expected) = std::env::var("ATERM_RELEASE_SIGNAL_PROBE") else {
            return;
        };
        let expected = if expected == "ignored" {
            libc::SIG_IGN
        } else {
            libc::SIG_DFL
        };
        for sig in [libc::SIGINT, libc::SIGQUIT, libc::SIGTERM] {
            let mut action = std::mem::MaybeUninit::<libc::sigaction>::zeroed();
            // SAFETY: a null new action reads into a live output slot.
            assert_eq!(
                unsafe { libc::sigaction(sig, std::ptr::null(), action.as_mut_ptr()) },
                0
            );
            // SAFETY: the successful query initialized the action.
            assert_eq!(
                unsafe { action.assume_init() }.sa_sigaction,
                expected,
                "signal {sig}"
            );
        }
        println!("\nRELEASE_SIGNAL_CHILD_CHECKED");
    }

    /// Detached release steps default the signals the cutter forwards even when its shell
    /// ignored them. The ordinary child is the negative control: it observes inherited ignores.
    #[cfg(unix)]
    #[test]
    fn detached_steps_default_the_background_shells_ignored_signals() {
        use super::*;
        use std::time::Duration;
        const PARENT: &str = "ATERM_RELEASE_SIGNAL_PARENT";
        const NAME: &str =
            "release_steps::tests::detached_steps_default_the_background_shells_ignored_signals";
        const PROBE: &str = "release_steps::tests::release_signal_disposition_probe";
        if std::env::var_os(PARENT).is_none() {
            let mut parent = command("/bin/sh");
            parent
                .args(["-c", "trap '' INT QUIT TERM; exec \"$0\" \"$@\""])
                .arg(std::env::current_exe().unwrap())
                .args(["--exact", NAME, "--nocapture", "--test-threads=1"])
                .env(PARENT, "1");
            let out = aterm_uds::deadline::ends_within(&mut parent, Duration::from_secs(60));
            assert!(out.status.success(), "{out:?}");
            assert!(
                String::from_utf8_lossy(&out.stdout).contains("\nDETACHED_SIGNALS_CHECKED\n"),
                "{out:?}"
            );
            return;
        }
        for reset in [false, true] {
            let mut child = command(std::env::current_exe().unwrap());
            child
                .args(["--exact", PROBE, "--nocapture", "--test-threads=1"])
                .env(
                    "ATERM_RELEASE_SIGNAL_PROBE",
                    if reset { "default" } else { "ignored" },
                );
            if reset {
                detach(&mut child);
            }
            let out = aterm_uds::deadline::ends_within(&mut child, Duration::from_secs(60));
            assert!(out.status.success(), "{out:?}");
            assert!(
                String::from_utf8_lossy(&out.stdout).contains("\nRELEASE_SIGNAL_CHILD_CHECKED\n"),
                "reset={reset}: {out:?}"
            );
        }
        println!("\nDETACHED_SIGNALS_CHECKED");
    }

    #[test]
    fn reader_cargo_holds_its_implicit_job_through_success_and_failure() {
        let dir = aterm_tempfile::tempdir().unwrap();
        let pool = aterm_capacity::Jobserver::new(&dir.path().join("reader.jobs"), 1).unwrap();
        for fail in [false, true] {
            let stop = super::BranchStop::default();
            let mut cmd = super::command("targo");
            let result = super::with_reader_cargo(Some(&pool), &stop, &mut cmd, |cmd| {
                assert!(
                    pool.try_acquire().unwrap().is_none(),
                    "implicit job is charged"
                );
                for name in ["MAKEFLAGS", "CARGO_MAKEFLAGS"] {
                    assert!(cmd.get_envs().any(|(key, value)| {
                        key == name && value == Some(std::ffi::OsStr::new(&pool.flags()))
                    }));
                }
                if fail {
                    Err(std::io::Error::other("compile failed"))
                } else {
                    Ok(())
                }
            });
            assert_eq!(result.is_err(), fail);
            assert!(
                pool.try_acquire().unwrap().is_some(),
                "compile returned its job"
            );
        }
        let stop = super::BranchStop::default();
        stop.fire();
        let token = pool.try_acquire().unwrap().unwrap();
        let mut ran = false;
        let result =
            super::with_reader_cargo(Some(&pool), &stop, &mut super::command("targo"), |_| {
                ran = true;
                Ok(())
            });
        assert!(result.is_err());
        assert!(!ran, "cancelled warm-up never starts Cargo");
        drop(token);
    }

    #[test]
    fn reader_prebuild_overlaps_cut_and_preserves_its_verdict() {
        let (started, began) = std::sync::mpsc::channel();
        let (release, released) = std::sync::mpsc::channel();
        let stopped = std::sync::Arc::new(std::sync::Mutex::new(None));
        let warm_stop = std::sync::Arc::clone(&stopped);
        let timeout = std::time::Duration::from_secs(60);
        let verdict = super::with_reader_prebuild(
            move |stop| {
                *warm_stop.lock().unwrap() = Some(stop.clone());
                started.send(()).unwrap();
                released.recv_timeout(timeout).unwrap();
            },
            || {
                began.recv_timeout(timeout).unwrap();
                release.send(()).unwrap();
                super::Ran {
                    code: 37,
                    detail: Vec::new(),
                }
            },
        );
        assert_eq!(verdict.code, 37);
        assert!(stopped.lock().unwrap().as_ref().unwrap().fired());
    }

    #[test]
    fn a_channel_transport_failure_never_blames_installed_prerequisites() {
        let transport = "curl: (56) HTTP 504";
        let error = format!("signed channel freshness transport: {transport}");
        let refusal = prerequisite_refusal(error);
        assert!(refusal.could_not_run);
        assert!(refusal.why.contains(transport), "{}", refusal.why);
        assert!(
            refusal.remedy.contains("transport answers"),
            "{}",
            refusal.remedy
        );
        assert!(
            !refusal.remedy.contains("pkg install"),
            "negative control: a freshness failure cannot be repaired by replacing local bytes: {}",
            refusal.remedy
        );

        let local = prerequisite_refusal("gitleaks is not installed".into());
        assert!(local.remedy.contains("pkg install"), "{}", local.remedy);
    }

    #[cfg(unix)]
    #[test]
    fn a_warming_verifier_is_bound_to_the_candidate_it_started_from() {
        use super::*;
        let root = aterm_tempfile::tempdir().unwrap();
        let mut real = Real::new(root.path(), Some(root.path()), true, false).unwrap();
        let mut command = command(aterm_uds::os_path!("true"));
        command
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .process_group(0);
        let child = command.spawn().unwrap();
        let forwarded = Forwarded {
            _running: interrupt::Running::enter(child.id()),
            note: None,
        };
        let expected = "a".repeat(40);
        real.warming = Some(WarmingVerifier {
            commit: expected.clone(),
            child,
            _forwarded: forwarded,
            stdout: root.path().join("no-build-output"),
            stderr: root.path().join("no-build-errors"),
        });
        // This checkout is not the recorded candidate. Without the binding, the code instead
        // consumes build output or resolves tools, neither of which is authorized here.
        let error = real.finish_tools().unwrap_err();
        assert!(error.contains(&format!("not {expected}")), "{error}");
        assert!(real.warm_verifier.is_none());
        assert!(real.warming.is_none());
    }

    #[test]
    fn step_one_consumes_a_fleet_source_refresh_without_fetching_it_again() {
        use super::*;
        let root = aterm_tempfile::tempdir().unwrap();
        let mut real = Real::new(root.path(), Some(root.path()), true, false).unwrap();
        real.prefetched_origin = Some(crate::github_remote::FetchedOrigin {
            main: "a".repeat(40),
            tags: "fixture tags".into(),
        });
        // This directory has no source origin: trying a second refresh is a negative control.
        assert_eq!(real.fetch().unwrap(), "a".repeat(40));
        assert_eq!(real.origin_tags, "fixture tags");
        assert!(real.prefetched_origin.is_none());
    }

    /// NEGATIVE CONTROL: omitting settlement, or ending only the direct child, leaves the
    /// output pipe open. EOF proves each owned build and its inherited-pipe descendant ended.
    #[cfg(unix)]
    #[test]
    fn an_early_refusal_settles_owned_warmups_and_their_groups() {
        use super::*;
        use std::io::Read as _;
        use std::os::fd::AsFd as _;
        let temp = aterm_tempfile::tempdir().unwrap();
        let program = temp.path().join("warming-verifier");
        aterm_tempfile::stand_in(
            &program,
            "#!/bin/sh\n/bin/sh -c 'printf \"ready\\n\"; while :; do /bin/sleep 60; done' &\nwait\n",
        )
        .unwrap();
        for warming in [true, false] {
            let mut command = command(&program);
            command
                .stdin(Stdio::null())
                .stdout(Stdio::piped())
                .stderr(Stdio::null())
                .process_group(0);
            let mut child = command.spawn().unwrap();
            let mut output = child.stdout.take().unwrap();
            let end = std::time::Instant::now() + std::time::Duration::from_secs(60);
            assert!(
                !aterm_uds::ready::any_until(&[output.as_fd()], end)
                    .unwrap()
                    .is_empty()
            );
            let mut line = [0; 6];
            output.read_exact(&mut line).unwrap();
            assert_eq!(&line, b"ready\n");
            let forwarded = Forwarded {
                _running: interrupt::Running::enter(child.id()),
                note: None,
            };
            if warming {
                drop(WarmingVerifier {
                    commit: "a".repeat(40),
                    child,
                    _forwarded: forwarded,
                    stdout: temp.path().join("out"),
                    stderr: temp.path().join("err"),
                });
            } else {
                drop(forwarded);
                drop(PrefreezeWorker(Some(child)));
            }
            assert!(
                !aterm_uds::ready::any_until(&[output.as_fd()], end)
                    .unwrap()
                    .is_empty()
            );
            assert_eq!(
                output.read(&mut [0]).unwrap(),
                0,
                "no build descendant retains the pipe"
            );
        }
    }

    /// Local instrument for the launch boundary only. It never starts a release, reads a
    /// roster, or publishes. Run A/B/B/A from a clean, fixed checkout with warm units.
    #[test]
    #[ignore = "explicit local freeze-time measurement"]
    fn measure_cutter_freeze() {
        use super::*;
        let cwd = std::env::current_dir().unwrap();
        let root =
            crate::ledger::git_ok(&GitCli::new(&cwd), &["rev-parse", "--show-toplevel"]).unwrap();
        let repo = PathBuf::from(root.stdout_utf8().trim());
        let sample = std::env::var("ATERM_RELEASE_FREEZE_SAMPLE")
            .expect("A (old two-tool freeze) or B (cutter-only)");
        let temp = aterm_tempfile::tempdir().unwrap();
        let mut real = Real::new(&repo, Some(&repo), false, false).unwrap();
        real.release = std::env::var_os("ATERM_RELEASE_FREEZE_STATE")
            .map(PathBuf::from)
            .unwrap_or_else(|| temp.path().join("release"));
        assert!(real.release.is_absolute(), "fixture state must be absolute");
        if std::env::var_os("ATERM_RELEASE_FREEZE_STATE").is_some() {
            real.state = real.release.join("fixture-state");
        }
        real.logs = std::env::var_os("ATERM_RELEASE_FREEZE_LOGS")
            .map(PathBuf::from)
            .unwrap_or_else(|| temp.path().join("logs"));
        std::fs::create_dir_all(&real.logs).unwrap();
        let commit = real.head();
        let prover = real.prover(None).unwrap();
        let start = std::time::Instant::now();
        if sample == "A" {
            crate::release_tools::clean_at(&repo, &commit).unwrap();
            let phase = std::time::Instant::now();
            let cutter = real
                .build_frozen_tool(&prover, &FROZEN_CUTTER, "gate")
                .unwrap()
                .0;
            println!(
                "freeze-phase cutter_build_ms={}",
                phase.elapsed().as_millis()
            );
            let phase = std::time::Instant::now();
            let verifier = real
                .build_frozen_tool(&prover, &FROZEN_VERIFY, "test")
                .unwrap()
                .0;
            println!(
                "freeze-phase verifier_build_ms={}",
                phase.elapsed().as_millis()
            );
            let phase = std::time::Instant::now();
            let tools =
                crate::release_tools::stage(&real.release, &commit, &cutter, &verifier).unwrap();
            crate::release_tools::clean_at(&repo, &commit).unwrap();
            crate::release_tools::publish(&real.release, &tools).unwrap();
            println!(
                "freeze-phase copy_prove_publish_ms={}",
                phase.elapsed().as_millis()
            );
        } else {
            assert_eq!(sample, "B");
            let cutter = real.frozen_cutter(&commit, &prover).unwrap();
            println!("freeze-proof {}", aterm_json::to_string(&cutter).unwrap());
        }
        println!(
            "freeze-sample mode={sample} elapsed_ms={}",
            start.elapsed().as_millis()
        );
        let log = std::fs::read_to_string(real.step_log(real.step)).unwrap_or_default();
        for line in log.lines().filter(|line| {
            line.starts_with("release-event: ") || line.starts_with("cutter adoption unavailable:")
        }) {
            println!("{line}");
        }
    }

    /// Full local launch instrument: real two-freeze/bump/seal work, with the existing
    /// authority seam stopping at qualify. The caller owns a clean disposable branch;
    /// restore its base between ABBA takes. No real release authority is contacted.
    pub(super) fn measure_bump_launch_to_qualify(controller: fn() -> std::time::Duration) {
        use super::*;
        let repo = std::env::current_dir().unwrap().canonicalize().unwrap();
        let git = GitCli::new(&repo);
        let branch = crate::ledger::git_ok(&git, &["rev-parse", "--abbrev-ref", "HEAD"]).unwrap();
        assert!(branch.stdout_utf8().trim().starts_with("freeze-fixture-"));
        let base = crate::ledger::git_ok(&git, &["rev-parse", "HEAD"])
            .unwrap()
            .stdout_utf8()
            .trim()
            .to_owned();
        crate::release_tools::clean_at(&repo, &base).unwrap();
        let mode = std::env::var("ATERM_RELEASE_LAUNCH_SAMPLE").expect("before or after");
        assert!(mode == "before" || mode == "after" || mode == "prepare");
        let state = PathBuf::from(
            std::env::var_os("ATERM_RELEASE_FREEZE_STATE").expect("private fixture state"),
        );
        assert!(state.is_absolute());
        let editor =
            PathBuf::from(std::env::var_os("ATERM_RELEASE_BUMP_EDITOR").expect("pub-bump-edit"));
        assert!(editor.is_absolute());
        let preparation_start = std::time::Instant::now();
        let preparation_timing = crate::cut_timing::Timer::new("launch-fixture-preparation");
        let mut real = Real::new(&repo, Some(&repo), false, false).unwrap();
        real.state = state.join("seal");
        real.logs = state.join(format!("logs-{mode}-{}", std::process::id()));
        real.release = state.join(if mode == "prepare" { "after" } else { &mode });
        std::fs::create_dir_all(&real.logs).unwrap();
        if mode == "prepare" {
            #[cfg(unix)]
            prepare_cached_cutter(&mut real, &base).unwrap();
            return;
        }
        let prover = real.prover(None).unwrap();
        let freeze = |real: &Real, commit: &str| real.frozen_cutter(commit, &prover).unwrap();
        if mode == "after" {
            // A separate process runs the actual Background preparation, then exits.
            // The measured cut retains its default QoS and inherits no resource cover.
            // Libtest has no tether entry; this opt-in instrument owns and waits for
            // its setsid child rather than detaching it.
            let mut cmd = command(std::env::current_exe().unwrap());
            cmd.args(std::env::args_os().skip(1))
                .env("ATERM_RELEASE_LAUNCH_SAMPLE", "prepare");
            #[cfg(unix)]
            cmd.setsid(true).default_signals();
            assert!(cmd.spawn().unwrap().wait().unwrap().success());
        }
        let preparation_s = preparation_start.elapsed().as_secs_f64();
        drop(preparation_timing);
        let start = std::time::Instant::now();
        let _launch_timing = crate::cut_timing::Timer::new("launch-fixture-cut");
        freeze(&real, &base);
        let version = real.version_at("HEAD").unwrap();
        let parts: Vec<u64> = version.split('.').map(|s| s.parse().unwrap()).collect();
        assert_eq!(parts.len(), 3);
        assert_eq!(parts[2], 0);
        let next = format!("{}.{}.0", parts[0], parts[1] + 1);
        let mut edit = real.child(&editor, None);
        edit.arg(&repo).args([&next, &version]);
        assert_eq!(real.logged(Step::Commit, &mut edit).code, 0);
        let bump_start = std::time::Instant::now();
        let lock_timing = crate::cut_timing::Timer::new("launch-fixture-lockfiles");
        if mode == "before" {
            let mut lock = real.child(prover.bin.join("targo"), Some(&prover));
            lock.args(["update", "--workspace", "--offline", "-q"]);
            assert_eq!(real.logged(Step::Commit, &mut lock).code, 0);
            bump_nested_locks(
                &repo,
                real.git_paths(&["ls-files", "-z", "--", "*/Cargo.lock"]),
                &version,
                &next,
            )
            .unwrap();
        } else {
            let changed = bump_nested_locks(
                &repo,
                real.git_paths(&["ls-files", "-z", "--", "Cargo.lock", "*/Cargo.lock"]),
                &version,
                &next,
            )
            .unwrap();
            real.validate_bumped_locks(&prover, &changed).unwrap();
        }
        let locks_s = bump_start.elapsed().as_secs_f64();
        drop(lock_timing);
        crate::ledger::git_ok(
            &git,
            &[
                "-c",
                "commit.gpgsign=false",
                "-c",
                "user.name=Test",
                "-c",
                "user.email=test@example.invalid",
                "commit",
                "-qam",
                "Fixture runtime identity bump",
            ],
        )
        .unwrap();
        let bumped = real.head();
        assert_ne!(bumped, base);
        let cutter = freeze(&real, &bumped);
        if mode == "after" {
            assert!(
                cutter.inputs.is_some(),
                "the fixture needs complete graph evidence"
            );
            assert_ne!(
                cutter.built_from, bumped,
                "the bump must adopt, not stage a new proof"
            );
        }
        // This is the real seal work formerly done by step 1. The authority seam below
        // models controller checks; its wall time is explicitly not service latency.
        real.seal_inputs().unwrap();
        let controller = controller();
        println!(
            "release-event: {}",
            aterm_json::json!({
                "schema":1,"event":"launch-fixture-to-qualify","sample":mode,
                "execution_s": start.elapsed().as_secs_f64(),
                "preparation_s": preparation_s,"locks_s":locks_s,
                "controller_s": controller.as_secs_f64(),"authority":"offline-test-seam",
                "base":base,"commit":bumped,"built_from":cutter.built_from,"inputs":cutter.inputs,
            })
        );
        let log = std::fs::read_to_string(real.step_log(real.step)).unwrap();
        for line in log.lines().filter(|line| {
            line.starts_with("release-event: ") || line.starts_with("cutter adoption unavailable:")
        }) {
            println!("{line}");
        }
    }

    pub(super) fn bind_native_engine_fixture(repo: &Path, bare: &Path) {
        let git = GitCli::new(repo);
        crate::ledger::git_ok(
            &git,
            &["config", "remote.fixture.url", bare.to_str().unwrap()],
        )
        .unwrap();
        crate::ledger::git_ok(
            &git,
            &[
                "config",
                "remote.origin.url",
                "https://github.com/fixture/app.git",
            ],
        )
        .unwrap();
    }

    pub(super) fn native_engine_fixture(
        repo: &Path,
        authority: &Path,
    ) -> Result<crate::github_remote::Remote, String> {
        let bare = crate::ledger::git_ok(
            &GitCli::new(authority),
            &["config", "--get", "remote.fixture.url"],
        )
        .map_err(|why| why.to_string())?
        .stdout_utf8();
        Ok(crate::github_remote::claim_fixture(
            repo,
            Path::new(bare.trim()),
        ))
    }

    pub(super) fn fixture_engine_url(repo: &Path) -> Result<String, String> {
        let configured = engine_origin_url(repo)?;
        if !crate::publish::github_slug_from_remote_url(&configured)
            .is_ok_and(|slug| slug == "fixture/app")
        {
            return Ok(configured);
        }
        crate::ledger::git_ok(
            &GitCli::new(repo),
            &["config", "--get", "remote.fixture.url"],
        )
        .map(|out| out.stdout_utf8().trim().to_string())
        .or_else(|_| engine_origin_url(repo))
    }

    /// The native writer may canonicalize transport to SSH. Only the fixture repository
    /// maps to the local bare; a different origin keeps its identity even with stale metadata.
    #[test]
    fn fixture_engine_origin_survives_transport_normalization_but_not_retargeting() {
        let root = aterm_tempfile::tempdir().unwrap();
        let repo = root.path().join("engine");
        let bare = root.path().join("engine.git");
        crate::ledger::testing::init(&repo, &[]);
        std::fs::create_dir(&bare).unwrap();
        bind_native_engine_fixture(&repo, &bare);
        let git = GitCli::new(&repo);
        crate::ledger::git_ok(
            &git,
            &[
                "config",
                "url.git@github.com:fixture/app.git.insteadOf",
                "https://github.com/fixture/app.git",
            ],
        )
        .unwrap();
        for url in [
            "https://github.com/fixture/app.git",
            "git@github.com:fixture/app.git",
            "ssh://git@github.com/fixture/app.git",
        ] {
            crate::ledger::git_ok(&git, &["remote", "set-url", "origin", url]).unwrap();
            if url == "https://github.com/fixture/app.git" {
                assert_eq!(
                    engine_origin_url(&repo).unwrap(),
                    "git@github.com:fixture/app.git"
                );
            }
            assert_eq!(
                fixture_engine_url(&repo).unwrap(),
                bare.display().to_string()
            );
        }
        let other = "git@github.com:another/app.git";
        crate::ledger::git_ok(&git, &["remote", "set-url", "origin", other]).unwrap();
        let resolved = fixture_engine_url(&repo).unwrap();
        assert_eq!(
            crate::publish::github_slug_from_remote_url(&resolved).unwrap(),
            "another/app"
        );
    }

    #[cfg(unix)]
    #[test]
    fn engine_audit_rows_use_the_native_expected_main_and_reject_retargeting() {
        let root = aterm_tempfile::tempdir().unwrap();
        let engine = root.path().join("engine");
        let bare = root.path().join("engine.git");
        crate::ledger::testing::init(
            &engine,
            &[
                ("user.name", "fixture"),
                ("user.email", "fixture@example.invalid"),
                ("commit.gpgsign", "false"),
            ],
        );
        let git = GitCli::new(&engine);
        std::fs::write(engine.join("verification-log.jsonl"), "").unwrap();
        crate::ledger::git_ok(&git, &["add", "verification-log.jsonl"]).unwrap();
        crate::ledger::git_ok(&git, &["commit", "-qm", "initial audit"]).unwrap();
        crate::ledger::git_ok(
            &GitCli::new(root.path()),
            &[
                "clone",
                "-q",
                "--bare",
                engine.to_str().unwrap(),
                bare.to_str().unwrap(),
            ],
        )
        .unwrap();
        bind_native_engine_fixture(&engine, &bare);
        let mut real = Real::new(&engine, Some(&engine), true, false).unwrap();
        real.engine_origin = native_engine_fixture;
        std::fs::write(engine.join("verification-log.jsonl"), "{\"row\":1}\n").unwrap();
        real.engine_fast_forward().unwrap();
        let remote = GitCli::new(&bare);
        assert_eq!(
            crate::ledger::git_ok(&remote, &["show", "refs/heads/main:verification-log.jsonl"])
                .unwrap()
                .stdout_utf8(),
            "{\"row\":1}\n"
        );
        assert_eq!(real.engine_git(&["status", "--porcelain"]).unwrap(), "");
        let before = crate::ledger::rev_parse(&remote, "refs/heads/main").unwrap();
        crate::ledger::git_ok(
            &git,
            &[
                "config",
                "remote.origin.url",
                "https://github.com/foreign/app.git",
            ],
        )
        .unwrap();
        std::fs::write(
            engine.join("verification-log.jsonl"),
            "{\"row\":1}\n{\"row\":2}\n",
        )
        .unwrap();
        assert!(real.engine_fast_forward().is_err());
        assert_eq!(
            crate::ledger::rev_parse(&remote, "refs/heads/main").unwrap(),
            before
        );
        assert_eq!(
            std::fs::read_to_string(engine.join("verification-log.jsonl")).unwrap(),
            "{\"row\":1}\n{\"row\":2}\n"
        );
    }
    #[cfg(unix)]
    #[test]
    fn a_valid_retargeted_engine_adapter_cannot_replace_the_captured_authority() {
        let root = aterm_tempfile::tempdir().unwrap();
        let engine = root.path().join("engine");
        let bare = root.path().join("engine.git");
        crate::ledger::testing::init(
            &engine,
            &[
                ("user.name", "fixture"),
                ("user.email", "fixture@example.invalid"),
                ("commit.gpgsign", "false"),
            ],
        );
        let git = GitCli::new(&engine);
        std::fs::write(engine.join("verification-log.jsonl"), "").unwrap();
        crate::ledger::git_ok(&git, &["add", "verification-log.jsonl"]).unwrap();
        crate::ledger::git_ok(&git, &["commit", "-qm", "initial audit"]).unwrap();
        crate::ledger::git_ok(
            &GitCli::new(root.path()),
            &[
                "clone",
                "-q",
                "--bare",
                engine.to_str().unwrap(),
                bare.to_str().unwrap(),
            ],
        )
        .unwrap();
        bind_native_engine_fixture(&engine, &bare);
        crate::ledger::git_ok(
            &git,
            &[
                "config",
                "remote.origin.url",
                "https://github.com/earlier/engine.git",
            ],
        )
        .unwrap();
        let mut real = Real::new(&engine, Some(&engine), true, false).unwrap();
        // This new native adapter is itself valid and permitted. The common
        // production binding must still reject its different captured slug.
        crate::ledger::git_ok(
            &git,
            &[
                "config",
                "remote.origin.url",
                "https://github.com/fixture/app.git",
            ],
        )
        .unwrap();
        real.engine_origin = native_engine_fixture;
        assert!(
            native_engine_fixture(&engine, &engine)
                .unwrap()
                .repository()
                .eq_full_name("fixture/app")
        );
        let remote = GitCli::new(&bare);
        let before = crate::ledger::rev_parse(&remote, "refs/heads/main").unwrap();
        std::fs::write(engine.join("verification-log.jsonl"), "{\"row\":1}\n").unwrap();
        let error = real.engine_fast_forward().unwrap_err();
        assert!(format!("{error:?}").contains("captured authority"));
        assert_eq!(
            crate::ledger::rev_parse(&remote, "refs/heads/main").unwrap(),
            before
        );
        assert_eq!(
            git.git(&[
                "rev-parse",
                "--verify",
                "--quiet",
                "refs/remotes/origin/main"
            ])
            .unwrap()
            .status,
            1,
            "refused before the native main refresh records its snapshot"
        );
        assert_eq!(
            std::fs::read_to_string(engine.join("verification-log.jsonl")).unwrap(),
            "{\"row\":1}\n"
        );
    }

    /// THE QUALIFY POOL GOES ONLY TO THE BRANCH CHILDREN THAT ADOPT IT: with step 4's pool made
    /// (one claim of every core, a quarter of them the export's), `ship build` is handed the whole
    /// pool and the tree's verify the pool less the export's share, each as
    /// `ATERM_RELEASE_POOL=<slots>:<fifo>` with `<fifo>` the jobserver's own path of the FIFO the
    /// release asked for, spelled the same for both whatever spelling the release asked by; a
    /// plain step child (an engine's, the fixtures') gets none, and with no pool nothing is
    /// handed. NEGATIVE CONTROLS: putting the pool in every child's scrubbed environment hands
    /// the plain child it, handing verify the whole pool gives it every slot, the export's
    /// included, and handing verify the FIFO by the path the release asked for hands it a second
    /// spelling of the FIFO the build is handed.
    #[test]
    fn the_qualify_pool_goes_only_to_the_branch_children_that_adopt_it() {
        let dir = aterm_tempfile::tempdir().unwrap();
        let repo = std::env::current_dir().unwrap();
        let mut real = Real::new(&repo, None, true, false).unwrap();
        let prover = Prover {
            bin: PathBuf::from("/fixture/trust/bin"),
            commit: "0".repeat(40),
            source: "fixture".into(),
        };
        let context_outside_pool = real
            .check_content_context(
                Path::new("/fixture/verify"),
                "frozen-tools",
                "seal",
                &prover,
                crate::check_record::Half::Tree,
            )
            .unwrap();
        let mut spawn_mirror = real.verifier_child(
            Path::new("/fixture/verify"),
            Some(&prover),
            crate::check_record::Half::Tree,
        );
        real.hand_pool(&mut spawn_mirror, true).unwrap();
        real.finalize_spawn_environment(&mut spawn_mirror).unwrap();
        assert_eq!(
            spawn_mirror
                .get_envs()
                .find(|(name, _)| *name == "GIT_TERMINAL_PROMPT")
                .and_then(|(_, value)| value),
            Some(std::ffi::OsStr::new("0"))
        );
        assert_eq!(
            spawn_mirror
                .get_envs()
                .find(|(name, _)| *name == "GIT_ASKPASS")
                .and_then(|(_, value)| value),
            Some(std::ffi::OsStr::new(""))
        );
        assert_eq!(
            context_outside_pool,
            crate::check_content::context(
                "frozen-tools",
                "seal",
                &spawn_mirror,
                crate::check_content::ReleaseHalf::Tree,
            ),
            "the context uses the same environment finalizer as the verifier spawn"
        );
        // `<slots>:<fifo>`, each exactly as handed.
        let pool_of = |cmd: &Command| {
            let value = cmd
                .get_envs()
                .find(|(k, _)| *k == aterm_capacity::POOL_ENV)
                .and_then(|(_, v)| v)?;
            let value = value.to_str().expect("this test's pool value is UTF-8");
            let (slots, fifo) = value.split_once(':').expect("<slots>:<fifo>");
            Some((slots.to_owned(), std::ffi::OsString::from(fifo)))
        };
        let mut none = command("verify");
        real.hand_pool(&mut none, true).unwrap();
        assert_eq!(pool_of(&none), None, "no pool, nothing handed");
        let verifier = Path::new("/frozen/verifier");

        // Asked for by a spelling no child is handed: a `.` component, and a doubled separator
        // too where `TMPDIR` holds one (as `$TMPDIR/.ubd/` does where `$TMPDIR` ends in a
        // separator).
        let path = dir.path().join(".").join("qualify.jobs");
        // This fixture must not publish a Release into the account ledger and cancel an
        // unrelated main-lane judgment. Its four-core machine and ledger are both private.
        let ledger_path = dir.path().join("pool-ledger");
        let ledger = aterm_capacity::Ledger::at(&ledger_path);
        let machine = aterm_capacity::Fixed::new(aterm_capacity::Reading::of_gib(48, 40, 4));
        let pool = qualify_pool(&path, true, |need| {
            assert_eq!(need.rank, aterm_capacity::Rank::Release);
            aterm_capacity::admit_with(
                &ledger,
                &machine,
                need.cores(4),
                aterm_capacity::Admit::Now,
                aterm_capacity::Pace::default(),
                &mut |_| {},
            )
        })
        .unwrap();
        assert_eq!(
            pool._claim.path().parent().unwrap().canonicalize().unwrap(),
            ledger_path.canonicalize().unwrap(),
            "the synthetic Release belongs only to the fixture's ledger"
        );
        assert_eq!(ledger.list().unwrap().len(), 1);
        assert_eq!(
            pool._claim.record().need.rank,
            aterm_capacity::Rank::Release
        );
        let (slots, export) = (pool.jobs.slots(), pool.export);
        assert_eq!(slots, 4);
        assert_eq!(export, export_share(slots, true));
        let fifo = pool.jobs.path().as_os_str().to_owned();
        assert_eq!(
            Path::new(&fifo),
            path,
            "the pool's FIFO is the one asked for"
        );
        real.pool = Some(pool);

        let tree = real.verifier_child(verifier, None, crate::check_record::Half::Tree);
        if cfg!(unix) {
            assert_eq!(tree.get_program(), "/usr/bin/nice");
            assert_eq!(
                tree.get_args().collect::<Vec<_>>(),
                [
                    std::ffi::OsStr::new("-n"),
                    std::ffi::OsStr::new("10"),
                    verifier.as_os_str()
                ]
            );
        }
        // Negative control: bundle checks run after the pool's slices and keep foreground priority.
        let bundle = real.verifier_child(verifier, None, crate::check_record::Half::Bundle);
        assert_eq!(bundle.get_program(), verifier.as_os_str());
        assert_eq!(bundle.get_args().count(), 0);

        let mut actual_tree = real.verifier_child(
            Path::new("/fixture/verify"),
            Some(&prover),
            crate::check_record::Half::Tree,
        );
        real.hand_pool(&mut actual_tree, true).unwrap();
        real.finalize_spawn_environment(&mut actual_tree).unwrap();
        assert_eq!(
            context_outside_pool,
            crate::check_content::context(
                "frozen-tools",
                "seal",
                &actual_tree,
                crate::check_content::ReleaseHalf::Tree,
            ),
            "the pre-pool content plan predicts the finalized pooled tree child"
        );
        if cfg!(unix) {
            let mut omitted_wrapper = real.child("/fixture/verify", Some(&prover));
            real.hand_pool(&mut omitted_wrapper, true).unwrap();
            real.finalize_spawn_environment(&mut omitted_wrapper)
                .unwrap();
            assert_ne!(
                context_outside_pool,
                crate::check_content::context(
                    "frozen-tools",
                    "seal",
                    &omitted_wrapper,
                    crate::check_content::ReleaseHalf::Tree,
                ),
                "omitting nice's program and argv must invalidate tree content evidence"
            );
        }

        let mut build = command("ship");
        real.hand_pool(&mut build, false).unwrap();
        assert_eq!(pool_of(&build), Some((slots.to_string(), fifo.clone())));
        let mut verify = command("targo");
        real.hand_pool(&mut verify, true).unwrap();
        let share = if export > 0 { slots - export } else { slots };
        assert_eq!(
            pool_of(&verify),
            Some((share.to_string(), fifo)),
            "verify is handed the build's FIFO, spelled the same, less the export's share"
        );
        assert_eq!(
            context_outside_pool,
            real.check_content_context(
                Path::new("/fixture/verify"),
                "frozen-tools",
                "seal",
                &prover,
                crate::check_record::Half::Tree,
            )
            .unwrap(),
            "facts before qualify and the verifier inside its pool have one check context"
        );
        let plain = real.child("generator", None);
        assert_eq!(pool_of(&plain), None, "a plain child is handed no pool");
    }

    /// The export's share: a quarter of the pool, at least one slot and never all of them, and
    /// none where it does not run or the pool has one slot.
    #[test]
    fn the_export_takes_a_quarter_of_the_pool_and_never_all_of_it() {
        assert_eq!(export_share(18, true), 4);
        assert_eq!(export_share(3, true), 1);
        assert_eq!(export_share(2, true), 1);
        assert_eq!(export_share(1, true), 0);
        assert_eq!(export_share(18, false), 0);
    }

    #[test]
    fn a_direct_fixtures_runtime_manifest_survives_the_launcher_scrub() {
        let mut cmd = aterm_uds::spawnfd::command("generator");
        cmd.env("CARGO_MANIFEST_DIR", "/candidate/crates/aterm-gui");
        super::scrub_fixture_command(&mut cmd);
        assert_eq!(
            cmd.get_envs()
                .find(|(k, _)| *k == "CARGO_MANIFEST_DIR")
                .and_then(|(_, v)| v),
            Some(std::ffi::OsStr::new("/candidate/crates/aterm-gui"))
        );
        // The inherited launcher value is still stripped; only an explicit artifact
        // manifest can be restored by the fixture path.
        let mut inherited = aterm_uds::spawnfd::command("generator");
        super::scrub_names(
            &mut inherited,
            [std::ffi::OsString::from("CARGO_MANIFEST_DIR")],
        );
        assert_eq!(
            inherited
                .get_envs()
                .find(|(k, _)| *k == "CARGO_MANIFEST_DIR")
                .and_then(|(_, v)| v),
            None
        );
    }
    use super::*;

    /// Every exec resource offer owns a distinct descriptor slot before any slot becomes an
    /// `OwnedFd`. NEGATIVE CONTROL: repeat the toolchain, line, or cohort-writer number in either
    /// other variable, and validation must refuse while the original descriptor remains owned by
    /// this test.
    #[cfg(unix)]
    #[test]
    fn duplicate_exec_resource_descriptor_offers_are_refused_before_ownership() {
        use std::os::fd::AsRawFd as _;

        let held = std::fs::File::open("/dev/null").unwrap();
        let repeated = std::ffi::OsString::from(held.as_raw_fd().to_string());
        for (lease, line, writer) in [
            (Some(repeated.as_os_str()), Some(repeated.as_os_str()), None),
            (Some(repeated.as_os_str()), None, Some(repeated.as_os_str())),
            (None, Some(repeated.as_os_str()), Some(repeated.as_os_str())),
        ] {
            let why = inherited_fd_offers(lease, line, writer).unwrap_err();
            assert!(why.contains("duplicates"), "{why}");
            assert!(
                held.metadata().is_ok(),
                "refusal took ownership of the offered descriptor"
            );
        }

        let lease = std::fs::File::open("/dev/null").unwrap();
        let line = std::fs::File::open("/dev/null").unwrap();
        let writer = std::fs::File::open("/dev/null").unwrap();
        let values = [
            std::ffi::OsString::from(lease.as_raw_fd().to_string()),
            std::ffi::OsString::from(line.as_raw_fd().to_string()),
            std::ffi::OsString::from(writer.as_raw_fd().to_string()),
        ];
        assert_eq!(
            inherited_fd_offers(
                Some(values[0].as_os_str()),
                Some(values[1].as_os_str()),
                Some(values[2].as_os_str()),
            )
            .unwrap(),
            [
                Some(lease.as_raw_fd()),
                Some(line.as_raw_fd()),
                Some(writer.as_raw_fd()),
            ]
        );
    }

    #[cfg(unix)]
    #[test]
    fn publication_engine_bootstrap_requires_a_working_launcher_and_recovers() {
        use std::os::unix::fs::PermissionsExt as _;

        let dir = aterm_tempfile::tempdir().unwrap();
        let repo = dir.path().join("source");
        let engine = dir.path().join("engine");
        std::fs::create_dir_all(&repo).unwrap();
        std::fs::create_dir_all(engine.join("bin")).unwrap();
        let launcher = engine.join("bin/pub");
        std::fs::write(&launcher, "#!/bin/sh\necho 'gone' >&2\nexit 127\n").unwrap();
        std::fs::set_permissions(&launcher, std::fs::Permissions::from_mode(0o755)).unwrap();
        match engine_bootstrap_with(&repo, &engine, crate::managed_prerequisites::simulated) {
            Err(Bootstrap::Failed(why)) => {
                assert!(why.contains("(exit status: 127): gone;"), "{why}")
            }
            other => panic!("{other:?}"),
        }
        std::fs::rename(&launcher, engine.join("bin/unavailable-pub")).unwrap();
        std::fs::write(
            &launcher,
            "#!/bin/sh\ncase \"$*\" in '--help') printf 'engine-help-ready\\n';; 'tap --help') printf '%s\\n' '--expect-version VERSION';; *) exit 3;; esac\n",
        )
        .unwrap();
        std::fs::set_permissions(&launcher, std::fs::Permissions::from_mode(0o755)).unwrap();
        assert!(
            engine_bootstrap_with(&repo, &engine, crate::managed_prerequisites::simulated).is_ok()
        );
        // An old tap may have successful help while lacking the cohort guard.
        std::fs::write(
            &launcher,
            "#!/bin/sh\nprintf '%s\\n' \"$*\" >> probes\nprintf 'old-help\\n'\n",
        )
        .unwrap();
        let missing =
            engine_bootstrap_with(&repo, &engine, crate::managed_prerequisites::simulated);
        assert!(matches!(missing, Err(Bootstrap::Failed(why)) if why.contains("--expect-version")));
        assert_eq!(
            std::fs::read_to_string(repo.join("probes")).unwrap(),
            "--help\ntap --help\n"
        );
        // A healthy-looking engine cannot run when signed admission refuses.
        // No script or PATH interpreter is probed as a replacement.
        std::fs::write(&launcher, "#!/bin/sh\nprintf ran > child-ran\n").unwrap();
        let refused = engine_bootstrap_with(&repo, &engine, |wants, _| {
            assert_eq!(wants, crate::managed_prerequisites::PUBLICATION);
            Err("cpython is unavailable on this target".into())
        });
        assert!(
            matches!(refused, Err(Bootstrap::Prerequisites(why)) if why.contains("unavailable"))
        );
        assert!(!repo.join("child-ran").exists());
    }

    #[test]
    #[cfg(unix)]
    fn an_old_tap_refuses_before_validation_tool_provisioning() {
        use std::os::unix::fs::PermissionsExt as _;
        let dir = aterm_tempfile::tempdir().unwrap();
        let engine = dir.path().join("engine");
        std::fs::create_dir_all(engine.join("bin")).unwrap();
        let script = engine.join("bin/pub");
        std::fs::write(
            &script,
            "#!/bin/sh\nprintf '%s\\n' \"$*\" > tap-probe\nprintf 'legacy tap help\\n'\n",
        )
        .unwrap();
        std::fs::set_permissions(&script, std::fs::Permissions::from_mode(0o755)).unwrap();
        let mut real = Real::new(dir.path(), Some(&engine), true, false).unwrap();
        real.prerequisites = Some(crate::managed_prerequisites::Tools::Simulated);
        let refused = real.validation_tools().unwrap_err();
        assert!(refused.why.contains("--expect-version"), "{refused:?}");
        assert_eq!(
            std::fs::read_to_string(dir.path().join("tap-probe")).unwrap(),
            "tap --help\n"
        );
    }

    #[test]
    #[cfg(unix)]
    fn the_after_cut_tap_child_uses_the_validated_cohort_and_propagates_refusal() {
        use std::os::unix::fs::PermissionsExt as _;
        let dir = aterm_tempfile::tempdir().unwrap();
        let engine = dir.path().join("engine");
        std::fs::create_dir_all(engine.join("bin")).unwrap();
        let script = engine.join("bin/pub");
        let mut real = Real::new(dir.path(), Some(&engine), true, false).unwrap();
        assert!(
            real.tap_command("v0.159.0").is_err(),
            "unheld interpreter cannot run"
        );
        real.prerequisites = Some(crate::managed_prerequisites::Tools::Simulated);
        for bad in ["0.159.0", "v0.159.1", "v00.159.0", "v0.159.0 extra"] {
            assert!(real.tap_command(bad).is_err(), "{bad}");
        }
        let command = real.tap_command("v0.159.0").unwrap();
        assert_eq!(command.get_program(), script.as_os_str());
        assert_eq!(
            command.get_args().collect::<Vec<_>>(),
            ["tap", "--bump", "--expect-version", "0.159.0"].map(std::ffi::OsStr::new)
        );
        std::fs::write(
            &script,
            "#!/bin/sh\nprintf '%s\\n' \"$*\" > tap-invocation\nexit 17\n",
        )
        .unwrap();
        std::fs::set_permissions(&script, std::fs::Permissions::from_mode(0o755)).unwrap();
        let refused = real.align_tap("v0.159.0").unwrap_err();
        assert!(refused.why.contains("exit 17"), "{refused:?}");
        assert_eq!(
            std::fs::read_to_string(dir.path().join("tap-invocation")).unwrap(),
            "tap --bump --expect-version 0.159.0\n"
        );
        std::fs::write(&script, "#!/bin/sh\nprintf '%s\\n' \"$*\" > tap-invocation\nprintf 'canonical verified alignment\\n'\n").unwrap();
        assert!(real.align_tap("v0.159.0").unwrap().contains("v0.159.0"));
        let log = std::fs::read_to_string(real.side_log(Step::AfterCut, "tap")).unwrap();
        assert!(log.contains("canonical verified alignment"));
    }

    #[test]
    fn publication_admission_is_retained_and_a_refusal_never_probes_scanners() {
        static ACQUIRED: std::sync::atomic::AtomicUsize = std::sync::atomic::AtomicUsize::new(0);
        let dir = aterm_tempfile::tempdir().unwrap();
        let mut real = Real::new(dir.path(), Some(dir.path()), true, false).unwrap();
        real.prerequisite_acquire = |wants, _| {
            assert_eq!(wants, crate::managed_prerequisites::PUBLICATION);
            ACQUIRED.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
            Ok(crate::managed_prerequisites::Tools::Simulated)
        };
        real.prepare_prerequisites().unwrap();
        real.prepare_prerequisites().unwrap();
        assert_eq!(ACQUIRED.load(std::sync::atomic::Ordering::SeqCst), 1);
        real.prerequisites = None;
        real.prerequisite_acquire = |_, _| Err("selected package has a bad signature".into());
        real.scanner_probe = |_| panic!("admission must refuse before scanner execution");
        let prover = Prover {
            bin: dir.path().to_path_buf(),
            commit: "fixture".into(),
            source: "fixture".into(),
        };
        let why = real.prepare_scanner_tools(&prover).err().unwrap();
        assert!(why.why.contains("bad signature"));
        assert!(why.could_not_run);
        assert!(real.prerequisites.is_none());
        assert!(real.scanner_tools.is_none());
    }

    /// A descriptor without FD_CLOEXEC, held open as one is between a `pipe` and its
    /// `fcntl`: neither a detached step nor the snapshot's git may inherit it.
    #[cfg(unix)]
    #[test]
    fn forwarded_steps_and_object_git_exclude_an_unflagged_descriptor() {
        use std::os::fd::{AsRawFd as _, FromRawFd as _, OwnedFd};
        unsafe extern "C" {
            fn fcntl(fd: i32, cmd: i32, ...) -> i32;
        }
        let file = std::fs::File::open("/dev/null").unwrap();
        // SAFETY: F_DUPFD creates a fresh descriptor without FD_CLOEXEC.
        let fd = unsafe { fcntl(file.as_raw_fd(), 0, 200) };
        assert!(fd >= 200);
        let _held = unsafe { OwnedFd::from_raw_fd(fd) };
        let probe = aterm_spec::host::held_probe(fd);
        let control = aterm_uds::spawnfd::transparent("/bin/sh")
            .args(["-c", &probe])
            .status()
            .unwrap();
        assert_eq!(control.code(), Some(3));

        let repo = std::env::current_dir().unwrap();
        let real = Real::new(&repo, None, true, false).unwrap();
        let out = real
            .output_forwarded(command("/bin/sh").args(["-c", &probe]))
            .unwrap();
        assert!(out.status.success(), "detached step inherited fd {fd}");

        let alias = format!("alias.fd-proof=!{probe}");
        let out = crate::ledger::object_git(&repo)
            .unwrap()
            .args(["-c", &alias, "fd-proof"])
            .output()
            .unwrap();
        assert!(
            out.status.success(),
            "object git inherited fd {fd}: {}",
            String::from_utf8_lossy(&out.stderr)
        );
    }

    /// The appcast adapter hands every transport attempt to `Launched<StepChildren>`'s launch
    /// boundary. NEGATIVE CONTROL: replacing that adapter with the client's direct spawn leaves
    /// `started` at zero; starting a later retry after the launch reports an interrupt makes the
    /// second half fail.
    #[test]
    fn appcast_retries_are_release_children_and_an_interrupt_starts_no_later_attempt() {
        use super::*;
        use std::sync::Arc;
        use std::sync::atomic::{AtomicBool, AtomicU32, Ordering};

        struct Launch {
            interrupt_after_first: bool,
            interrupted: AtomicBool,
            started: Arc<AtomicU32>,
        }

        impl crate::site::Launch for Launch {
            fn interrupted(&self) -> Option<i32> {
                self.interrupted.load(Ordering::SeqCst).then_some(15)
            }

            fn spawn(
                &self,
                command: &mut aterm_uds::spawnfd::Command,
            ) -> std::io::Result<(aterm_uds::spawnfd::Child, crate::site::Guard)> {
                if self.interrupted().is_some() {
                    return Err(std::io::Error::new(
                        std::io::ErrorKind::Interrupted,
                        "not started: the release was interrupted",
                    ));
                }
                let child = command.spawn()?;
                self.started.fetch_add(1, Ordering::SeqCst);
                if self.interrupt_after_first {
                    self.interrupted.store(true, Ordering::SeqCst);
                }
                Ok((child, Box::new(())))
            }
        }

        let fetch = |_: &str,
                     _: u64,
                     spawn: &dyn aterm_update_core::github_rest::Spawn|
         -> Result<Vec<u8>, String> {
            for _ in 0..3 {
                let mut command = aterm_uds::spawnfd::command("/bin/sh");
                command.args(["-c", "printf appcast"]);
                let output = spawn
                    .run(&mut command, None)
                    .map_err(|error| error.to_string())?;
                if !output.status.success() {
                    return Err(format!("fixture child exited {}", output.status));
                }
            }
            Ok(b"appcast".to_vec())
        };

        let started = Arc::new(AtomicU32::new(0));
        assert_eq!(
            appcast_bytes_with(
                "https://example.invalid/appcast",
                1024,
                Launch {
                    interrupt_after_first: false,
                    interrupted: AtomicBool::new(false),
                    started: Arc::clone(&started),
                },
                fetch,
            ),
            Ok(b"appcast".to_vec())
        );
        assert_eq!(started.load(Ordering::SeqCst), 3);

        let started = Arc::new(AtomicU32::new(0));
        let refused = appcast_bytes_with(
            "https://example.invalid/appcast",
            1024,
            Launch {
                interrupt_after_first: true,
                interrupted: AtomicBool::new(false),
                started: Arc::clone(&started),
            },
            fetch,
        )
        .expect_err("the next retry is not started after the interrupt");
        assert!(refused.contains("not started"), "{refused}");
        assert_eq!(started.load(Ordering::SeqCst), 1);
    }

    /// The site sync's children keep the release's rules: each leads a session and group of
    /// its own (where the interrupt is forwarded, with no terminal to stop on), git's prompt
    /// is off, and a running-child note names it while it runs and is gone once it ends.
    #[cfg(unix)]
    #[test]
    fn the_site_syncs_children_keep_the_releases_rules() {
        use crate::site::Launch as _;
        let dir = aterm_tempfile::Builder::new()
            .prefix("aterm-step-children-")
            .tempdir()
            .unwrap();
        let children = StepChildren {
            noted: Some((dir.path().to_path_buf(), Step::AfterCut)),
        };
        assert_eq!(children.interrupted(), None);
        let mut sh = aterm_uds::spawnfd::command("/bin/sh");
        sh.args([
            "-c",
            "echo \"$$ $(ps -o pgid= -p $$ | tr -d ' ') ${GIT_TERMINAL_PROMPT-unset} [${GIT_ASKPASS-unset}]\"",
        ]);
        let out = crate::site::output(&children, &mut sh).unwrap();
        let said = String::from_utf8_lossy(&out.stdout);
        let fields: Vec<&str> = said.split_whitespace().collect();
        assert_eq!(fields.len(), 4, "{said}");
        assert_eq!(
            fields[0], fields[1],
            "the child leads its own group: {said}"
        );
        assert_eq!(fields[2], "0", "git's prompt is off: {said}");
        assert_eq!(fields[3], "[]", "and so is its askpass: {said}");
        assert!(
            crate::release::child_notes(dir.path()).is_empty(),
            "the note goes with the child"
        );
        let mut sleeper = aterm_uds::spawnfd::command("/bin/sh");
        sleeper
            .args(["-c", "read line"])
            .stdin(aterm_uds::spawnfd::Stdio::piped());
        let (mut child, guard) = children.spawn(&mut sleeper).unwrap();
        let note =
            std::fs::read_to_string(crate::release::child_note(dir.path(), child.id())).unwrap();
        assert!(
            note.starts_with(&format!("pid {}\n", child.id()))
                && note.ends_with("step after-cut\n"),
            "{note}"
        );
        drop(child.stdin.take());
        child.wait().unwrap();
        drop(guard);
        assert!(crate::release::child_notes(dir.path()).is_empty());
    }

    #[test]
    fn a_pointer_that_could_not_be_read_is_could_not_run() {
        let refused = pointer_verdict(Err("https://x/latest could not be read: timeout".into()))
            .expect_err("a failed read refuses");
        assert!(refused.could_not_run, "{refused:?}");
        assert_eq!(
            pointer_verdict(Ok(vec!["404".into()])).expect("shape problems pass through"),
            ["404"]
        );
    }

    #[test]
    fn only_a_404_or_a_302_elsewhere_is_the_pointers_shape() {
        let (ptr, want) = ("https://x/latest", "https://x/v0.6.0");
        assert_eq!(
            pointer_hop_problem(ptr, want, "v0.6.0", 302, Some(want)),
            Ok(None)
        );
        assert!(matches!(
            pointer_hop_problem(ptr, want, "v0.6.0", 404, None),
            Ok(Some(_))
        ));
        assert!(matches!(
            pointer_hop_problem(ptr, want, "v0.6.0", 302, Some("https://x/v0.5.0")),
            Ok(Some(_))
        ));
        for code in [500, 502, 503, 403, 200] {
            assert!(
                pointer_hop_problem(ptr, want, "v0.6.0", code, None).is_err(),
                "HTTP {code} is a read that failed"
            );
        }
    }

    fn release(assets: &[(&str, u64)], body: &str) -> aterm_json::Value {
        let assets: Vec<String> = assets
            .iter()
            .map(|(n, s)| format!("{{\"name\":\"{n}\",\"size\":{s}}}"))
            .collect();
        aterm_json::from_str(&format!(
            "{{\"tag_name\":\"v0.6.0\",\"body\":{body:?},\"assets\":[{}]}}",
            assets.join(",")
        ))
        .unwrap()
    }

    fn lean() -> Vec<(&'static str, u64)> {
        vec![
            ("aterm-0.6.0.dmg", 40_600_000),
            ("aterm-0.6.0-mac.zip", 38_000_000),
            ("aterm.dmg", 40_600_000),
            ("aterm-mac.zip", 38_000_000),
            ("aterm-0.6.0.dmg.sha256", 90),
            ("aterm-0.6.0-mac.zip.sha256", 90),
            ("aterm.dmg.sha256", 90),
            ("aterm-mac.zip.sha256", 90),
            ("aterm-appcast.toml", 900),
            ("aterm-appcast.toml.sig", 100),
            ("aterm-machines.toml", 900),
            ("aterm-machines.toml.sig", 100),
            ("SHA256SUMS", 680_000),
            ("SHA256SUMS.sig", 64),
        ]
    }

    #[test]
    fn source_publication_requires_one_manifest_and_one_signature_before_binary_assets() {
        let pair = [("SHA256SUMS", 100), ("SHA256SUMS.sig", 64)];
        assert!(source_attestation_findings(&release(&pair, "")).is_empty());
        for assets in [
            vec![],
            vec![pair[0]],
            vec![pair[1]],
            vec![pair[0], pair[0], pair[1]],
        ] {
            assert!(!source_attestation_findings(&release(&assets, "")).is_empty());
        }
    }

    #[test]
    fn a_release_is_judged_by_the_lean_shape_github_serves() {
        assert_eq!(
            release_shape(&release(&lean(), "notes"), "0.6.0"),
            Vec::<String>::new()
        );
        let mut seeded = lean();
        seeded[0].1 = 1_070_000_000;
        seeded.push(("aterm-0.6.0-x86_64.dmg", 9));
        seeded.retain(|(n, _)| *n != "aterm-mac.zip.sha256" && *n != "SHA256SUMS.sig");
        seeded[2].1 = 1;
        let bad = release_shape(&release(&seeded, "the batteries-included image"), "0.6.0");
        assert_eq!(bad.len(), 6, "{bad:#?}");
        for asset in [
            "aterm-mac.zip.sha256",
            "aterm-0.6.0-x86_64.dmg",
            "SHA256SUMS.sig",
        ] {
            assert!(bad.iter().any(|p| p.contains(asset)), "{asset}: {bad:#?}");
        }
        let mut linux = lean();
        linux.push(("aterm-0.6.0-linux-x86_64", 40_000_000));
        linux.push(("aterm-0.6.0-linux-riscv64", 40_000_000));
        linux.push(("aterm-0.5.0-linux-aarch64", 40_000_000));
        linux.push(("aterm-0.6.0-linux-aarch64.tar.gz", 1_000));
        linux.retain(|(n, _)| *n != "aterm-machines.toml.sig");
        let bad = release_shape(&release(&linux, ""), "0.6.0");
        for asset in [
            "aterm-0.6.0-linux-riscv64",
            "aterm-0.5.0-linux-aarch64",
            "aterm-0.6.0-linux-aarch64.tar.gz.sha256",
            "aterm-machines.toml.sig",
        ] {
            assert!(bad.iter().any(|p| p.contains(asset)), "{asset}: {bad:#?}");
        }
    }

    #[test]
    fn the_appcast_the_pointer_serves_names_this_version_and_binds_its_url() {
        let slug = "alabsystems/aterm";
        let good = "version = \"0.6.0\"\ndmg = \"aterm-0.6.0.dmg\"\n\
                    url = \"https://github.com/alabsystems/aterm/releases/download/v0.6.0/aterm-0.6.0.dmg\"\n";
        assert!(appcast_problems(good, slug, "v0.6.0", "0.6.0").is_empty());
        assert!(!appcast_problems(good, slug, "v0.7.0", "0.7.0").is_empty());
        let unbound = good.replace("download/v0.6.0", "download/v0.5.0");
        assert!(!appcast_problems(&unbound, slug, "v0.6.0", "0.6.0").is_empty());
    }

    #[test]
    fn the_engines_dry_run_is_read_in_its_own_words() {
        let text = "== exported 5807 of 7058 tracked files from abc\n\
                    == guard ok: no forbidden content\n\
                    == GUARD FAIL: private references (2)\n  a.rs:3\n  b.md:9\n\
                    FAIL: guards failed — nothing was pushed\n";
        let run = EngineRun::read(1, text);
        assert_eq!(run.passed, ["no forbidden content"]);
        assert_eq!(
            run.failed,
            [(
                "private references (2)".to_string(),
                vec!["a.rs:3".to_string(), "b.md:9".to_string()]
            )]
        );
        assert_eq!(run.refusals, ["guards failed — nothing was pushed"]);
        assert_eq!(EngineRun::read(0, "").tail, Vec::<String>::new());
    }

    #[test]
    fn an_engine_refusal_is_sorted_by_its_cause() {
        for line in [
            "malformed manifest line (bare '!'): '!'",
            "empty manifest",
            "captured aterm commit has no publish/manifest.txt",
            "publication policy contains a symlink, which could escape immutable inputs: x",
            "CHECK_TIMEOUT_SECONDS_DEFAULT must be a positive integer",
            "vendor/astream is not a gitlink in abc",
            "transforms failed",
            "the export does not pin the Trust toolchain the dev commit pins",
            "detected version '1.2' is not a semver string",
            "no staging remote configured",
            "staging remote must identify alabsystems/aterm-staging, found 'x'",
        ] {
            assert_eq!(EngineCause::of(line), EngineCause::Tree, "{line}");
        }
        for (line, cause) in [
            (
                "gitleaks not installed — a fail-closed guard cannot be skipped",
                EngineCause::NoGitleaks,
            ),
            (
                "gitleaks crashed/timed out — aborting rather than fail open",
                EngineCause::GitleaksBroke,
            ),
            (
                "release notes: cannot read the public tags of alabsystems/aterm: git \
                 ls-remote failed",
                EngineCause::Network,
            ),
            (
                "release notes: cannot fetch the previous public release v0.1.0",
                EngineCause::Network,
            ),
            (
                "cannot query aterm origin/main: fatal: unable to access",
                EngineCause::Network,
            ),
            (
                "astream public release main fetch failed: Host key verification failed.",
                EngineCause::Network,
            ),
            // The engine's agh transport (2026-11-05), measured on this Mac on 2026-11-06 with
            // the installed agh: a read through it is agh's to route, not the network's.
            (
                "cannot query publication engine origin/main: pub GitHub transport: Agh exited 2: \
                 aterm help: unknown topic 'repo'",
                EngineCause::Transport,
            ),
            (
                "astream public release main fetch failed: pub GitHub transport: Agh exited 4: \
                 agh: not_found exit=4",
                EngineCause::Transport,
            ),
            (
                "Agh lacks the required exact anonymous fetch capability; select the qualified \
                 client with PUBLISH_AGH",
                EngineCause::Transport,
            ),
            (
                "publication engine HEAD a does not exactly match queried origin/main b",
                EngineCause::Engine,
            ),
            (
                "another publication operation for aterm is running; cannot stage",
                EngineCause::Engine,
            ),
            (
                "--commit abc is not an ancestor of origin/main def",
                EngineCause::Source,
            ),
        ] {
            assert_eq!(EngineCause::of(line), cause, "{line}");
        }
    }

    #[test]
    fn the_export_guards_pass_refuse_or_could_not_run() {
        let (log, reports) = (Path::new("/c/dry-run.log"), Path::new("/c/reports"));
        let outcome = |code: i32, text: &str, toolchain: Option<&Refusal>| {
            export_outcome(&EngineRun::read(code, text), "r", toolchain, log, reports)
        };
        let refused = |code: i32, text: &str, toolchain: Option<&Refusal>| match outcome(
            code, text, toolchain,
        ) {
            Outcome::Refuse(r) => r,
            other => panic!("{text:?}: {other:?}"),
        };
        let not_run = |code: i32, text: &str, toolchain: Option<&Refusal>| match outcome(
            code, text, toolchain,
        ) {
            Outcome::CouldNotRun(r) => r,
            other => panic!("{text:?}: {other:?}"),
        };
        let no_toolchain = Refusal::new("no rustup", "install rustup");
        let exported = "== exported 1 of 1 tracked files from r\n";
        let ok =
            format!("{exported}== guard ok: no forbidden content\n== guard ok: gitleaks clean\n");
        assert!(matches!(outcome(0, &ok, None), Outcome::Pass(_)));
        assert!(matches!(
            outcome(0, &ok, Some(&no_toolchain)),
            Outcome::Pass(_)
        ));
        let hit =
            format!("{exported}== GUARD FAIL: forbidden content (1 hits; first 5):\n  x.rs:1\n");
        for after in [
            "FAIL: guards failed — nothing was pushed\n",
            "FAIL: gitleaks not installed — a fail-closed guard cannot be skipped\n",
            "FAIL: gitleaks crashed/timed out — aborting rather than fail open\n",
        ] {
            let r = refused(1, &format!("{hit}{after}"), None);
            assert!(r.detail.iter().any(|l| l == "  x.rs:1"), "{r:?}");
        }
        for (text, line) in [
            (
                "FAIL: malformed manifest line (bare '!'): '!'\n".to_string(),
                "malformed manifest line (bare '!'): '!'",
            ),
            (
                format!(
                    "{exported}FAIL: vendor/astream is not a gitlink in abc\nFAIL: transforms \
                     failed\n"
                ),
                "vendor/astream is not a gitlink in abc",
            ),
        ] {
            assert!(refused(1, &text, None).why.contains(line), "{text}");
        }
        for (text, line) in [
            (
                "FAIL: cannot query aterm origin/main: fatal: unable to access\n".to_string(),
                "cannot query aterm origin/main",
            ),
            (
                format!(
                    "{exported}== guard ok: no forbidden content\nFAIL: gitleaks not installed \
                     — a fail-closed guard cannot be skipped\n"
                ),
                "gitleaks not installed",
            ),
            (
                format!(
                    "{exported}== guard ok: no forbidden content\nFAIL: gitleaks crashed/timed \
                     out — aborting rather than fail open\n"
                ),
                "gitleaks crashed",
            ),
            (
                format!(
                    "{exported}FAIL: release notes: cannot read the public tags of \
                     alabsystems/aterm: git ls-remote failed\n"
                ),
                "release notes: cannot read the public tags",
            ),
        ] {
            assert!(not_run(1, &text, None).why.contains(line), "{text}");
        }
        let metadata = format!(
            "{exported}== guard ok: gitleaks clean\n== GUARD FAIL: targo metadata failed (see \
             cargo-metadata.err)\nFAIL: guards failed — nothing was pushed\n"
        );
        refused(1, &metadata, None);
        assert_eq!(
            not_run(1, &metadata, Some(&no_toolchain)).why,
            no_toolchain.why
        );
        for (code, text) in [
            (0, exported),
            (1, exported),
            (-1, "cannot run /e/bin/pub: No such file or directory"),
        ] {
            not_run(code, text, None);
        }
    }

    #[test]
    fn a_gitleaks_finding_is_named_by_place_and_rule_never_by_its_secret() {
        let report = r#"[{"File":"/x/export/a/b.rs","StartLine":7,"RuleID":"generic-api-key","Match":"key=hunter2","Secret":"hunter2"}]"#;
        assert_eq!(
            gitleaks_locations(report, Path::new("/x/export")),
            ["a/b.rs:7 generic-api-key"]
        );
        assert!(gitleaks_locations("not json", Path::new("/x")).is_empty());
        assert_eq!(
            gitleaks_report("gitleaks findings (see /o/gitleaks-adjudicated.json)"),
            Some("/o/gitleaks-adjudicated.json")
        );
        assert_eq!(gitleaks_report("private references (2)"), None);
    }

    #[test]
    fn ship_check_export_drives_a_scratch_copy_of_the_engine_at_its_origins_main() {
        let dir = aterm_tempfile::tempdir().unwrap();
        let root = dir.path().canonicalize().unwrap();
        let git = |cwd: &Path, args: &[&str]| -> String {
            let out = crate::ledger::testing::git()
                .current_dir(cwd)
                .args([
                    "-c",
                    "user.name=t",
                    "-c",
                    "user.email=t@t",
                    "-c",
                    "commit.gpgsign=false",
                ])
                .args(args)
                .output()
                .unwrap();
            assert!(
                out.status.success(),
                "git {args:?}: {}",
                String::from_utf8_lossy(&out.stderr)
            );
            String::from_utf8_lossy(&out.stdout).trim().to_string()
        };
        let (engine, engine_origin) = (root.join("publication"), root.join("publication.git"));
        let (repo, origin) = (root.join("aterm"), root.join("aterm.git"));
        for bare in [&engine_origin, &origin] {
            git(
                &root,
                &[
                    "init",
                    "-q",
                    "--bare",
                    "-b",
                    "main",
                    &bare.display().to_string(),
                ],
            );
        }
        for (dir, remote) in [(&engine, &engine_origin), (&repo, &origin)] {
            std::fs::create_dir_all(dir).unwrap();
            git(dir, &["init", "-q", "-b", "main"]);
            git(
                dir,
                &["remote", "add", "origin", &remote.display().to_string()],
            );
        }
        // The engine's GIT_* environment, as the gate hands it on from this process's own.
        let reads = public_reads(
            std::env::var_os("GIT_CONFIG_COUNT").as_deref(),
            &[],
            std::env::var_os("GIT_CONFIG_PARAMETERS").as_deref(),
        )
        .unwrap();
        let seen = root.join("seen-env");
        let scanner_report = root.join("scanner-report.json");
        std::fs::write(&scanner_report, scanner_tools::test_output().stdout).unwrap();
        let dump: String = reads
            .iter()
            .map(|(key, _)| {
                format!(
                    "printf '%s=%s\\n' {key} \"${key}\" >> '{}'\n",
                    seen.display()
                )
            })
            .collect();
        let agh = agh_standin(&repo);
        aterm_verify::standin(&agh, "exit 0");
        let stub = |say: &str, code: i32| {
            std::fs::create_dir_all(engine.join("bin")).unwrap();
            let pubbin = engine.join("bin/pub");
            std::fs::write(
                &pubbin,
                format!(
                    "#!/bin/sh\nhere=$(cd \"$(dirname \"$0\")/..\" && pwd)\n\
                     if [ \"$1\" = scanner-preflight ]; then cat '{scanner}'; exit 0; fi\n\
                     [ -n \"$PUBLISH_SCANNER_TOOLS\" ] && cmp -s \"$PUBLISH_SCANNER_TOOLS\" '{scanner}' || exit 10\n\
                     [ \"$PUBLISH_AGH\" = '{agh}' ] || exit 11\n\
                     head=$(git -C \"$here\" rev-parse HEAD) || exit 12\n\
                     remote=$(git --git-dir='{engine_origin}' rev-parse refs/heads/main) || exit 12\n\
                     [ \"$head\" = \"$remote\" ] || {{ printf 'FAIL: publication engine HEAD %s does not exactly match queried origin/main %s\\n' \"$head\" \"$remote\"; exit 1; }}\n\
                     echo '{{\"row\":1}}' \
                     >> \"$here/verification-log.jsonl\"\n[ \"$1 $2 $3 $4\" = 'stage aterm \
                     --dry-run --no-check' ] || exit 9\n{dump}printf '%s' '{say}'\nexit {code}\n",
                    scanner = scanner_report.display(),
                    agh = agh.display(),
                    engine_origin = engine_origin.display(),
                ),
            )
            .unwrap();
            let mut perms = std::fs::metadata(&pubbin).unwrap().permissions();
            std::os::unix::fs::PermissionsExt::set_mode(&mut perms, 0o755);
            std::fs::set_permissions(&pubbin, perms).unwrap();
            std::fs::write(engine.join("verification-log.jsonl"), "").unwrap();
            git(&engine, &["add", "-A"]);
            git(&engine, &["commit", "-q", "-m", "stub"]);
            git(
                &engine,
                &[
                    "push",
                    "-q",
                    "--force",
                    engine_origin.to_str().unwrap(),
                    "main",
                ],
            );
        };
        std::fs::write(
            repo.join("Cargo.toml"),
            "[workspace.package]\nversion = \"0.2.0\"\n",
        )
        .unwrap();
        git(&repo, &["add", "-A"]);
        git(&repo, &["commit", "-q", "-m", "c"]);
        git(&repo, &["push", "-q", "origin", "main"]);
        git(&repo, &["fetch", "-q", "origin"]);
        let opts = |commit: &str, engine: &Path| CheckOptions {
            commit: Some(commit.to_string()),
            engine: Some(engine.to_path_buf()),
            export: true,
            ..CheckOptions::default()
        };
        let export_built = |commit: &str, build: CheckoutBuild| {
            bind_native_engine_fixture(&engine, &engine_origin);
            let found = checked_with(&repo, &opts(commit, &engine), |real| {
                real.engine_origin = native_engine_fixture;
                real.prerequisite_acquire = crate::managed_prerequisites::simulated;
                real.checkout_build = build;
                // This test builds the checkout's agh, whatever PUBLISH_AGH its caller set.
                real.engine_agh_explicit = None;
            })
            .expect("a release");
            let gate = found.gates.last().expect("the export gate");
            assert_eq!(gate.name, EXPORT_GATE);
            (gate.outcome.clone(), found.exit_code())
        };
        let export = |commit: &str| export_built(commit, built_agh);
        let exported = "== exported 1 of 1 tracked files from r\n";
        stub(&format!("{exported}== guard ok: no forbidden content\n"), 0);
        let (outcome, code) = export("HEAD");
        assert!(matches!(outcome, Outcome::Pass(_)), "{outcome:?}");
        assert_eq!(code, 0);
        let want: String = reads
            .iter()
            .map(|(k, v)| format!("{k}={}\n", v.to_string_lossy()))
            .collect();
        assert_eq!(std::fs::read_to_string(&seen).unwrap(), want);
        assert_eq!(
            std::fs::read_to_string(engine.join("verification-log.jsonl")).unwrap(),
            ""
        );
        assert_eq!(git(&engine, &["status", "--porcelain"]), "");
        // The fixture's engine advances while this checkout prepares its GitHub client.
        // The actual stage must still receive that origin's current main, not the copy
        // fetched before preparation. No elapsed-time threshold or remote network is involved.
        let (outcome, code) = export_built("HEAD", |real, what, args| {
            let built = built_agh(real, what, args)?;
            for args in [
                vec![
                    "commit",
                    "-q",
                    "--allow-empty",
                    "-m",
                    "advance during agh build",
                ],
                vec!["push", "-q", "fixture", "main"],
            ] {
                let out = crate::ledger::testing::git()
                    .current_dir(&real.engine)
                    .args([
                        "-c",
                        "user.name=t",
                        "-c",
                        "user.email=t@t",
                        "-c",
                        "commit.gpgsign=false",
                    ])
                    .args(&args)
                    .output()
                    .map_err(|why| why.to_string())?;
                assert!(
                    out.status.success(),
                    "git {args:?}: {}",
                    String::from_utf8_lossy(&out.stderr)
                );
            }
            Ok(built)
        });
        assert!(matches!(outcome, Outcome::Pass(_)), "{outcome:?}");
        assert_eq!(code, 0);
        stub("FAIL: malformed manifest line (bare !): !\n", 1);
        git(&engine, &["reset", "-q", "--hard", "HEAD~1"]);
        let (outcome, code) = export("HEAD");
        assert!(matches!(outcome, Outcome::Refuse(_)), "{outcome:?}");
        assert_eq!(code, 1);
        stub(
            "FAIL: cannot query aterm origin/main: fatal: unable to access\n",
            1,
        );
        let (outcome, code) = export("HEAD");
        assert!(matches!(outcome, Outcome::CouldNotRun(_)), "{outcome:?}");
        assert_eq!(code, 3);
        stub(&format!("{exported}== guard ok: no forbidden content\n"), 0);
        // No agh the engine can read GitHub through: the gate stops before the dry run, and its
        // refusal names what would.
        let (outcome, code) = export_built("HEAD", broken_agh);
        match &outcome {
            Outcome::CouldNotRun(why) => assert!(
                why.why.contains("only through agh")
                    && why.why.contains("error[E0425]")
                    && why.remedy.contains("PUBLISH_AGH"),
                "{why:?}"
            ),
            other => panic!("{other:?}"),
        }
        assert_eq!(code, 3);
        std::fs::write(repo.join("new.rs"), "\n").unwrap();
        git(&repo, &["add", "-A"]);
        git(&repo, &["commit", "-q", "-m", "unpushed"]);
        let (outcome, code) = export("HEAD");
        assert!(matches!(outcome, Outcome::CouldNotRun(_)), "{outcome:?}");
        assert_eq!(code, 3);
        git(
            &repo,
            &["push", "-q", "origin", "HEAD:refs/heads/export-feature"],
        );
        git(&repo, &["fetch", "-q", "origin"]);
        let branch = checked_with(&repo, &opts("HEAD", &engine), |real| {
            real.prerequisite_acquire = |_, _| panic!("a feature commit acquires no export tools");
            real.checkout_build = |_, _, _| panic!("a feature commit builds no export tools");
        })
        .unwrap();
        assert!(matches!(
            branch.gates.last().map(|gate| &gate.outcome),
            Some(Outcome::CouldNotRun(_))
        ));
        assert_eq!(
            branch.exit_code(),
            3,
            "a feature branch push is not mainline"
        );
        let mut unread = Real::new(&repo, Some(&engine), true, false).unwrap();
        unread.prerequisite_acquire = |_, _| panic!("unread ancestry acquires no export tools");
        let (outcome, _) =
            unread.export_gate(&Scratch::read_only(&repo).unwrap(), "no-such-commit");
        assert!(matches!(outcome, Outcome::CouldNotRun(_)), "{outcome:?}");
        let found = checked_with(&repo, &opts("HEAD~1", &root.join("none")), |real| {
            real.prerequisite_acquire = crate::managed_prerequisites::simulated;
            real.checkout_build = |_, _, _| panic!("a missing engine builds no export tools");
        })
        .unwrap();
        assert!(matches!(
            found.gates.last().map(|g| &g.outcome),
            Some(Outcome::CouldNotRun(_))
        ));
        assert!(checked(&repo, &opts("no-such-commit", &engine)).is_err());
    }

    /// Where the stand-in agh of the checkout at `repo` is "built", outside the checkout.
    fn agh_standin(repo: &Path) -> PathBuf {
        repo.with_file_name("agh-build").join("agh")
    }

    /// A build of the checkout's agh that cargo answers with the stand-in's path.
    fn built_agh(real: &Real, what: &str, args: &[std::ffi::OsString]) -> Result<Vec<u8>, String> {
        let pair = |flag: &str, value: &str| args.windows(2).any(|w| w[0] == flag && w[1] == value);
        assert!(
            what == "agh" && pair("-p", "agh") && pair("--bin", "agh"),
            "{what}: {args:?}"
        );
        let manifest = real.repo.join("crates/agh/Cargo.toml");
        Ok(aterm_json::json!({
            "reason": "compiler-artifact",
            "manifest_path": manifest.to_str().unwrap(),
            "target": {"name": "agh", "kind": ["bin"]},
            "executable": agh_standin(&real.repo).to_str().unwrap(),
        })
        .to_string()
        .into_bytes())
    }

    /// A build of the checkout's agh that does not compile.
    fn broken_agh(_: &Real, what: &str, _: &[std::ffi::OsString]) -> Result<Vec<u8>, String> {
        Err(format!(
            "{what} build exited exit status: 101: error[E0425]"
        ))
    }

    /// [`PUBLIC_READS`] numbered from `base`, the way [`public_reads`] hands them to the engine.
    fn public_reads_at(base: usize) -> Vec<(String, std::ffi::OsString)> {
        let pair = |key: String, value: &str| (key, value.into());
        vec![
            pair(
                format!("GIT_CONFIG_KEY_{base}"),
                "url.https://github.com/alabsystems/.insteadOf",
            ),
            pair(
                format!("GIT_CONFIG_VALUE_{base}"),
                "git@github.com:alabsystems/",
            ),
            pair(
                format!("GIT_CONFIG_KEY_{}", base + 1),
                "credential.https://github.com/alabsystems.helper",
            ),
            pair(format!("GIT_CONFIG_VALUE_{}", base + 1), ""),
            pair(
                format!("GIT_CONFIG_KEY_{}", base + 2),
                "credential.https://github.com/alabsystems.helper",
            ),
            pair(
                format!("GIT_CONFIG_VALUE_{}", base + 2),
                "!f() { echo quit=1; }; f",
            ),
            pair(
                format!("GIT_CONFIG_KEY_{}", base + 3),
                "http.https://github.com/alabsystems/.extraHeader",
            ),
            pair(format!("GIT_CONFIG_VALUE_{}", base + 3), ""),
            pair("GIT_CONFIG_COUNT".to_string(), &(base + 4).to_string()),
            pair(
                "GIT_CONFIG_PARAMETERS".to_string(),
                concat!(
                    "'url.https://github.com/alabsystems/.insteadOf=git@github.com:alabsystems/' ",
                    "'credential.https://github.com/alabsystems.helper=' ",
                    "'credential.https://github.com/alabsystems.helper=!f() { echo quit=1; }; f' ",
                    "'http.https://github.com/alabsystems/.extraHeader='",
                ),
            ),
            pair("GIT_TERMINAL_PROMPT".to_string(), "0"),
        ]
    }

    /// The engine's dry run reads alabsystems as the public does, whatever this host's SSH keys: each `git@github.com:alabsystems/` URL over HTTPS, with no credential for `https://github.com/alabsystems` from a helper or an askpass program, numbered after the caller's own `GIT_CONFIG_*` entries, which still apply, while every other GitHub URL keeps its transport and its helper.
    #[test]
    fn the_engines_dry_run_reads_the_public_tier_anonymously_after_the_callers_git_config() {
        let reads = |count: Option<&str>| public_reads(count.map(std::ffi::OsStr::new), &[], None);
        assert_eq!(reads(None).unwrap(), public_reads_at(0));
        assert_eq!(reads(Some("")).unwrap(), public_reads_at(0));
        assert_eq!(reads(Some("3")).unwrap(), public_reads_at(3));
        for bogus in ["two", "-1", " 1", "1 ", &usize::MAX.to_string()] {
            let why = reads(Some(bogus)).unwrap_err();
            assert!(
                why.why.contains("GIT_CONFIG_COUNT") && why.why.contains(bogus),
                "{why:?}"
            );
        }

        let dir = aterm_tempfile::tempdir().unwrap();
        let root = dir.path().canonicalize().unwrap();
        let mut real = Real::new(&root, Some(&root), true, false).unwrap();
        real.prerequisites = Some(crate::managed_prerequisites::Tools::Simulated);
        let (stage, handed) = real
            .dry_run_command(
                &root.join("engine"),
                "c0ffee",
                &root.join("clone"),
                &reads(Some("1")).unwrap(),
                &root.join("agh"),
            )
            .unwrap();
        // This test process was never judged at its start, so it is no commit's build: the dry
        // run it builds is handed no cutter, not even one this process inherited.
        for name in [RELEASE_CUTTER_ENV, RELEASE_CUTTER_SINCE_ENV] {
            assert!(
                stage
                    .get_envs()
                    .any(|(k, v)| k == std::ffi::OsStr::new(name) && v.is_none()),
                "{name}: {handed}"
            );
        }
        assert!(
            handed.ends_with("c0ffee's publish/transforms.sh builds its own"),
            "{handed}"
        );
        let mut sent: Vec<(String, std::ffi::OsString)> = stage
            .get_envs()
            .filter_map(|(key, value)| {
                let key = key.to_str().filter(|k| k.starts_with("GIT_"))?;
                let value = value.map_or_else(|| "<removed>".into(), |v| v.to_os_string());
                Some((key.to_string(), value))
            })
            .collect();
        sent.sort();
        let mut want = public_reads_at(1);
        want.sort();
        assert_eq!(sent, want, "the engine's GIT_* environment");

        // What git makes of it, under a caller's own entry 0, a helper for every GitHub URL and
        // an askpass program that answers any prompt.
        let config = root.join("gitconfig");
        std::fs::write(
            &config,
            "[credential \"https://github.com\"]\n\thelper = \"!f() { echo username=fixture; \
             echo password=fixture-pass; }; f\"\n",
        )
        .unwrap();
        let askpass = root.join("askpass");
        std::fs::write(&askpass, "#!/bin/sh\necho askpass-pass\n").unwrap();
        let mut perms = std::fs::metadata(&askpass).unwrap().permissions();
        std::os::unix::fs::PermissionsExt::set_mode(&mut perms, 0o755);
        std::fs::set_permissions(&askpass, perms).unwrap();
        let git = |args: &[&str], input: Option<&str>| {
            let mut git = crate::ledger::testing::git();
            git.current_dir(&root)
                .env("GIT_CONFIG_GLOBAL", &config)
                .env("GIT_ASKPASS", &askpass)
                .env_remove("SSH_ASKPASS")
                .env(
                    "GIT_CONFIG_KEY_0",
                    "url.https://example.invalid/mirror/.insteadOf",
                )
                .env("GIT_CONFIG_VALUE_0", "fixture:")
                .envs(sent.iter().map(|(key, value)| (key, value)))
                .args(args)
                .stdin(if input.is_some() {
                    Stdio::piped()
                } else {
                    Stdio::null()
                })
                .stdout(Stdio::piped())
                .stderr(Stdio::piped());
            let mut child = git.spawn().unwrap();
            if let (Some(input), Some(mut stdin)) = (input, child.stdin.take()) {
                stdin.write_all(input.as_bytes()).unwrap();
            }
            let out = child.wait_with_output().unwrap();
            let text = |b: &[u8]| String::from_utf8_lossy(b).into_owned();
            (out.status.success(), text(&out.stdout), text(&out.stderr))
        };
        let url = |from: &str| git(&["ls-remote", "--get-url", from], None).1;
        assert_eq!(
            url("git@github.com:alabsystems/a3d.git").trim(),
            "https://github.com/alabsystems/a3d.git"
        );
        assert_eq!(
            url("git@github.com:alabsystems/publication.git").trim(),
            "git@github.com:alabsystems/publication.git",
            "a private address keeps this host's transport"
        );
        assert_eq!(
            url("fixture:x").trim(),
            "https://example.invalid/mirror/x",
            "the caller's own entry still applies"
        );
        let fill = |url: &str| git(&["credential", "fill"], Some(&format!("url={url}\n\n")));
        let (ok, out, err) = fill("https://github.com/alabsystems/publication.git");
        assert!(ok && out.contains("password=fixture-pass"), "{out}{err}");
        for public in [
            "https://github.com/alabsystems/a3d.git",
            "https://alabsystems@github.com/alabsystems/aterm.git",
        ] {
            let (ok, out, err) = fill(public);
            assert!(
                !ok && !out.contains("password=") && err.contains("told us to quit"),
                "{public}: {out}{err}"
            );
        }
    }

    #[test]
    fn the_wasm32_gate_runs_rs_own_clause_under_a_stub_rustup() {
        let dir = aterm_tempfile::tempdir().unwrap();
        let root = dir.path().canonicalize().unwrap();
        let git = |cwd: &Path, args: &[&str]| -> String {
            let out = crate::ledger::testing::git()
                .current_dir(cwd)
                .args(["-c", "user.name=t", "-c", "user.email=t@t"])
                .args(["-c", "commit.gpgsign=false"])
                .args(args)
                .output()
                .unwrap();
            assert!(out.status.success(), "git {args:?}");
            String::from_utf8_lossy(&out.stdout).trim().to_string()
        };
        let repo = root.join("aterm");
        std::fs::create_dir_all(repo.join("publish")).unwrap();
        git(&repo, &["init", "-q", "-b", "main"]);
        std::fs::write(
            repo.join("publish/config.sh"),
            "CHECK_CMD_DEFAULT=\"rustup target add --toolchain 9.9.9 wasm32-unknown-unknown && \
             RUSTC=$(rustup which --toolchain 9.9.9 rustc) rustup run 9.9.9 cargo check \
             --locked --target wasm32-unknown-unknown -p aterm-wasm\"\n",
        )
        .unwrap();
        std::fs::write(repo.join("Cargo.toml"), "[workspace]\n").unwrap();
        git(&repo, &["add", "-A"]);
        git(&repo, &["commit", "-q", "-m", "r"]);
        let r = git(&repo, &["rev-parse", "HEAD"]);
        let stub = root.join("rustup-bin");
        std::fs::create_dir_all(&stub).unwrap();
        std::fs::write(
            stub.join("rustup"),
            "#!/bin/sh\nhere=$(cd \"$(dirname \"$0\")\" && pwd)\ncase \"$1\" in\n\
             target) exit 0 ;;\n\
             which) echo \"$here/rustc\"; exit 0 ;;\n\
             run) [ -f Cargo.toml ] || { echo \"error: no tree laid out in $(pwd)\" >&2; exit 101; }\n\
             \x20 [ -f \"$here/broken\" ] && { echo 'error[E0425]: cannot find value `x`' >&2; exit 101; }\n\
             \x20 echo 'Finished'; exit 0 ;;\nesac\n\
             echo \"stub rustup: unexpected $*\" >&2; exit 2\n",
        )
        .unwrap();
        let mut perms = std::fs::metadata(stub.join("rustup"))
            .unwrap()
            .permissions();
        std::os::unix::fs::PermissionsExt::set_mode(&mut perms, 0o755);
        std::fs::set_permissions(stub.join("rustup"), perms).unwrap();
        let tools = Wasm32Tools {
            rustup: stub.clone(),
            path: std::env::join_paths([stub.as_path(), Path::new("/usr/bin"), Path::new("/bin")])
                .unwrap(),
        };
        let scratch = Scratch {
            dir: root.join("scratch"),
            cache: root.join("cache"),
        };
        std::fs::create_dir_all(&scratch.dir).unwrap();
        let export = root.join("export");
        std::fs::create_dir_all(&export).unwrap();
        std::fs::write(export.join("Cargo.toml"), "[workspace]\n").unwrap();
        let mut real = Real::new(&repo, None, true, false).expect("a backend");

        let absent = aterm_uds::host::tool("rustup")
            .locate_in(&aterm_uds::host::Lookup {
                named: None,
                tooling: Some(root.join("tooling.noindex")),
                path: None,
            })
            .unwrap_err();
        let none = real.wasm32_gate(&scratch, &r, Some(&export), Err(&absent));
        assert!(
            matches!(&none, Outcome::CouldNotRun(refused)
                if refused.why.starts_with("no rustup (looked in")
                    && refused.remedy.contains("targo --unverified verify provision")),
            "no rustup: {none:?}"
        );
        let passed = real.wasm32_gate(&scratch, &r, Some(&export), Ok(&tools));
        assert!(matches!(passed, Outcome::Pass(_)), "{passed:?}");
        std::fs::write(stub.join("broken"), "").unwrap();
        let broken = real.wasm32_gate(&scratch, &r, Some(&export), Ok(&tools));
        match &broken {
            Outcome::Refuse(why) => assert!(
                why.detail.iter().any(|l| l.starts_with("error[E0425]")),
                "the compiler's errors ride along: {why:?}"
            ),
            other => panic!("a compile that fails refuses: {other:?}"),
        }
        std::fs::remove_file(stub.join("broken")).unwrap();
        let archived = real.wasm32_gate(&scratch, &r, None, Ok(&tools));
        assert!(matches!(archived, Outcome::Pass(_)), "{archived:?}");
        assert!(scratch.dir.join("wasm-src/publish/config.sh").is_file());
    }

    const CRLF_PROBE_ENV: &str = "ATERM_RELEASE_CRLF_PROBE_DIR";
    const CRLF_PROBE: &str =
        "release_steps::tests::every_tree_the_release_writes_is_the_commits_bytes";

    fn run_crlf_git(at: &Path, args: &[&str]) -> String {
        let out = crate::ledger::testing::git()
            .arg("-C")
            .arg(at)
            .args(args)
            .output()
            .unwrap();
        assert!(
            out.status.success(),
            "git {args:?}: {}",
            String::from_utf8_lossy(&out.stderr)
        );
        String::from_utf8_lossy(&out.stdout).trim().to_string()
    }

    fn write_crlf_fixture_file(path: &Path, body: &str) {
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(path, body).unwrap();
    }

    fn setup_crlf_fixture_repos(root: &Path) -> (PathBuf, String) {
        use crate::ledger::testing;
        let ident = [
            ("user.name", "t"),
            ("user.email", "t@example.invalid"),
            ("commit.gpgsign", "false"),
        ];
        let sub = root.join("sub");
        testing::init(&sub, &ident);
        write_crlf_fixture_file(&sub.join("s.sh"), "#!/bin/sh\n");
        run_crlf_git(&sub, &["add", "-A"]);
        run_crlf_git(&sub, &["commit", "-q", "-m", "s"]);
        let (repo, origin) = (root.join("aterm"), root.join("aterm.git"));
        testing::init(&repo, &ident);
        write_crlf_fixture_file(&repo.join(".gitattributes"), "*.txt text\n");
        write_crlf_fixture_file(&repo.join("bin/pub"), "#!/bin/sh\n");
        write_crlf_fixture_file(&repo.join("notes.txt"), "one\n");
        write_crlf_fixture_file(&repo.join("RELEASES.ledger"), "1783354739 0.25.0\n");
        write_crlf_fixture_file(
            &repo.join("CHANGELOG.md"),
            "## [Unreleased]\n\n### Added\n- An entry.\n",
        );
        let sub_s = sub.display().to_string();
        let add_sub = ["submodule", "add", "-q", &sub_s, "vendor/sub"];
        run_crlf_git(
            &repo,
            &[&["-c", "protocol.file.allow=always"][..], &add_sub].concat(),
        );
        run_crlf_git(&repo, &["add", "-A"]);
        run_crlf_git(&repo, &["commit", "-q", "-m", "r"]);
        run_crlf_git(root, &["init", "-q", "--bare", "-b", "main", "aterm.git"]);
        run_crlf_git(
            &repo,
            &["remote", "add", "origin", &origin.display().to_string()],
        );
        run_crlf_git(&repo, &["push", "-q", "origin", "main"]);
        let head = run_crlf_git(&repo, &["rev-parse", "HEAD"]);
        (repo, head)
    }

    fn probe_every_tree_the_release_writes_under_a_crlf_config_if_asked() -> bool {
        use std::fs;
        let Some(root) = std::env::var_os(CRLF_PROBE_ENV).map(PathBuf::from) else {
            return false;
        };
        let (repo, r) = setup_crlf_fixture_repos(&root);
        // The configuration the parent chose is what this probe's own git reads (it was started
        // with `crate::ledger::testing::CHOSEN_GIT_ENV`), not the test binary's hermetic one.
        let host = GitCli::new(&repo);
        let read = |args: &[&str]| {
            crate::ledger::GitRunner::git(&host, args)
                .unwrap()
                .stdout_utf8()
                .trim()
                .to_owned()
        };
        assert_eq!(read(&["config", "core.autocrlf"]), "true");
        assert_eq!(
            read(&["check-attr", "eol", "--", "notes.txt"]),
            "notes.txt: eol: crlf"
        );
        let lf = |tree: &Path, notes: &str, with_sub: bool| {
            let read = |rel: &str| fs::read_to_string(tree.join(rel)).unwrap();
            assert_eq!(read("bin/pub"), "#!/bin/sh\n", "{}", tree.display());
            assert_eq!(read("notes.txt"), notes, "{}", tree.display());
            if with_sub {
                assert_eq!(read("vendor/sub/s.sh"), "#!/bin/sh\n", "{}", tree.display());
            }
        };
        bind_native_engine_fixture(&repo, &root.join("aterm.git"));
        let mut real = Real::new(&repo, Some(&repo), true, false).unwrap();
        real.engine_origin = native_engine_fixture;
        let scratch = Scratch {
            dir: root.join("scratch"),
            cache: root.join("cache"),
        };
        fs::create_dir_all(&scratch.dir).unwrap();
        lf(&real.scratch_engine(&scratch).unwrap(), "one\n", false);
        let clone = scratch.dir.join("dry-run/aterm");
        real.dry_clone(&scratch.dir, &clone, &r).unwrap();
        lf(&clone, "one\n", true);
        let archive = root.join("archive");
        fs::create_dir_all(&archive).unwrap();
        assert!(real.archive_into(&r, &archive));
        lf(&archive, "one\n", true);
        // The engine segment above uses its explicit native fixture. The
        // placement segment exercises the existing local-file route; keep its
        // authority local rather than accidentally selecting network Git.
        run_crlf_git(
            &repo,
            &[
                "remote",
                "set-url",
                "origin",
                root.join("aterm.git").to_str().unwrap(),
            ],
        );
        let place = |commit: &str| {
            let source = crate::gates::PublishedSource {
                commit: commit.to_string(),
                verified_at: "2026-10-21T00:00:00+00:00".to_string(),
            };
            let git = GitCli::new(&repo);
            let origin = crate::gates::fixture_origin::FixtureOrigin(&git);
            crate::gates::place_published(&git, &origin, &repo, &source).unwrap()
        };
        lf(&place(&r).tree, "one\n", true);
        write_crlf_fixture_file(&repo.join("notes.txt"), "one\ntwo\n");
        run_crlf_git(&repo, &["commit", "-q", "-am", "two"]);
        run_crlf_git(
            &repo,
            &[
                "push",
                "-q",
                root.join("aterm.git").to_str().unwrap(),
                "main",
            ],
        );
        let source = run_crlf_git(&repo, &["rev-parse", "HEAD"]);
        let moved = place(&source);
        assert!(moved.moved);
        lf(&moved.tree, "one\ntwo\n", true);
        let plan = crate::ledger::ClaimPlan {
            version: "0.26.0",
            now: 0,
            allow_existing_section: false,
            max_attempts: 1,
            source: &source,
        };
        let rolls = |source: &str, main: &str| {
            crate::changelog::claim_changelogs(source, main, "0.26.0", "2026-10-21")
        };
        let cut = GitCli::new(&moved.tree);
        bind_native_engine_fixture(&moved.tree, &root.join("aterm.git"));
        let native = native_engine_fixture(&moved.tree, &moved.tree).unwrap();
        let reserved = crate::ledger::reserve(&cut, &native, &plan, &rolls).unwrap();
        crate::ledger::claim(&cut, &native, &moved.tree, &plan, &reserved, &rolls).unwrap();
        let rolled = fs::read_to_string(moved.tree.join("CHANGELOG.md")).unwrap();
        assert!(
            rolled.contains("## [0.26.0]") && !rolled.contains('\r'),
            "{rolled:?}"
        );
        true
    }

    /// Every entry under `root` and its bytes, symlinks not followed: what a probe writes grows it.
    fn tree_bytes(root: &Path) -> u64 {
        let mut total = 0;
        let mut dirs = vec![root.to_path_buf()];
        while let Some(dir) = dirs.pop() {
            for entry in std::fs::read_dir(&dir).into_iter().flatten().flatten() {
                let Ok(meta) = entry.metadata() else { continue };
                total += 1 + meta.len();
                if meta.is_dir() {
                    dirs.push(entry.path());
                }
            }
        }
        total
    }

    #[test]
    fn every_tree_the_release_writes_is_the_commits_bytes() {
        if probe_every_tree_the_release_writes_under_a_crlf_config_if_asked() {
            return;
        }
        let dir = aterm_tempfile::tempdir().unwrap();
        let root = dir.path().canonicalize().unwrap();
        let config = root.join("gitconfig");
        std::fs::write(
            &config,
            "[core]\n\tautocrlf = true\n\tsafecrlf = true\n\teol = crlf\n[protocol \"file\"]\n\tallow = always\n",
        )
        .unwrap();
        std::fs::create_dir_all(root.join("xdg/git")).unwrap();
        std::fs::write(root.join("xdg/git/attributes"), "* text eol=crlf\n").unwrap();
        let work = root.join("work");
        std::fs::create_dir(&work).unwrap();
        for leaf in ["home", "tmp", "state"] {
            std::fs::create_dir(root.join(leaf)).unwrap();
        }
        let mut child = command(std::env::current_exe().unwrap());
        aterm_uds::fixture_git::hermetic(&mut child)
            .args(["--exact", "--nocapture", "--test-threads=1", CRLF_PROBE])
            .env(CRLF_PROBE_ENV, &work)
            .env("HOME", root.join("home"))
            .env("TMPDIR", root.join("tmp"))
            .env("TMP", root.join("tmp"))
            .env("TEMP", root.join("tmp"))
            .env("XDG_STATE_HOME", root.join("state"))
            .env("GIT_CONFIG_GLOBAL", &config)
            .env("XDG_CONFIG_HOME", root.join("xdg"))
            .env(crate::ledger::testing::CHOSEN_GIT_ENV, "1")
            .stdin(Stdio::null());
        // The probe is a whole release's git work: the hang detector is a minute in which its
        // fixture tree stops growing, not a minute for all of it.
        let mut grown = || tree_bytes(&work);
        let out = aterm_uds::deadline::output_while_progressing(
            &mut child,
            aterm_spec::wait::MUST_HAPPEN,
            &mut grown,
        );
        let out = match out {
            Ok(out) => out,
            Err(error) => {
                let kept = dir.keep();
                panic!(
                    "CRLF probe: {error} (its fixture tree stopped growing); retained fixture {}",
                    kept.display()
                );
            }
        };
        let stdout = String::from_utf8_lossy(&out.stdout);
        assert!(
            out.status.success() && stdout.contains("1 passed"),
            "the probe under a CRLF config: {}\n{stdout}\n{}",
            out.status,
            String::from_utf8_lossy(&out.stderr)
        );
    }

    #[test]
    fn a_refusal_outranks_a_gate_that_could_not_run() {
        let gate = |outcome: Outcome| PrecheckGate { name: "g", outcome };
        let pass = || gate(Outcome::Pass("ok".into()));
        let refuse = || gate(Outcome::Refuse(Refusal::new("no", "fix")));
        let not_run = || gate(Outcome::CouldNotRun(Refusal::new("no tool", "install")));
        for (gates, code) in [
            (vec![pass()], 0),
            (vec![pass(), not_run()], 3),
            (vec![not_run(), refuse(), pass()], 1),
            (vec![refuse(), not_run()], 1),
        ] {
            let found = Precheck {
                commit: "c".repeat(40),
                version: "0.2.0".into(),
                gates,
            };
            assert_eq!(found.exit_code(), code);
        }
    }

    #[test]
    fn the_engines_ledger_and_attestation_are_read_as_the_release_reads_them() {
        let a = "a".repeat(40);
        let b = "b".repeat(40);
        let json = format!(
            "{{\"mappings\":{{\"aterm\":{{\"{a}\":{{\"verified_at\":\"2026-10-01T00:00:00Z\",\
             \"release_rev\":\"e1\"}},\"{b}\":{{\"verified_at\":\"2026-10-02T00:00:00Z\"}}}}}}}}"
        );
        let rows = engine_rows_in(&json, Some(&a));
        assert_eq!(
            rows,
            EngineRows {
                newest: Some(b.clone()),
                has: true,
                rev: Some("e1".into())
            }
        );
        assert!(!engine_rows_in(&json, Some("c")).has);
        assert_eq!(engine_rows_in("not json", None), EngineRows::default());
        // A revoked row is no publication of its commit, nor the newest. NEGATIVE CONTROL: the
        // same rows unrevoked (above) have `a` published and `b` newest.
        let revoked = format!(
            "{}, \"revoked\":{{\"aterm\":{{\"{b}\":{{}},\"{a}\":{{}}}}}}}}",
            json.strip_suffix('}').unwrap()
        );
        assert_eq!(engine_rows_in(&revoked, Some(&a)), EngineRows::default());
        let attestation = |gate: &str, mode: &str| {
            format!(
                "{{\"source\":{{\"commit\":\"{a}\"}},\"guards\":{{\"public_clone\":\"{gate}\"}},\
                 \"release_mode\":\"{mode}\",\"engine\":{{\"commit\":\"{b}\"}},\
                 \"staging\":{{\"candidate_rev\":\"{b}\",\"attested_rev\":\"{b}\"}},\
                 \"export\":{{\"tree\":\"{a}\"}}}}"
            )
        };
        assert!(attestation_promotes(&attestation("pass", "full"), &a));
        assert!(attestation_promotes(
            &attestation("skipped", "source-only"),
            &a
        ));
        assert!(!attestation_promotes(&attestation("skipped", "full"), &a));
        assert!(!attestation_promotes(&attestation("pass", "full"), &b));
        assert_eq!(
            attested_engine(&attestation("pass", "full")).as_deref(),
            Some(b.as_str())
        );
        let engineless =
            attestation("pass", "full").replace(&format!(",\"engine\":{{\"commit\":\"{b}\"}}"), "");
        assert_eq!(attested_engine(&engineless), None);
        assert!(!attestation_promotes(&engineless, &a));
        let private = attestation("pass", "full").replace(
            &format!("\"attested_rev\":\"{b}\""),
            "\"attested_rev\":null",
        );
        assert!(attestation_qualifies(&private, &a));
        assert!(!attestation_promotes(&private, &a));
        for invalid in [
            private.replace(
                &format!("\"candidate_rev\":\"{b}\""),
                "\"candidate_rev\":null",
            ),
            private.replace(&format!("\"tree\":\"{a}\""), "\"tree\":null"),
            private.replace("\"public_clone\":\"pass\"", "\"public_clone\":\"skipped\""),
        ] {
            assert!(!attestation_qualifies(&invalid, &a), "{invalid}");
            assert!(!attestation_promotes(&invalid, &a), "{invalid}");
        }
        let mismatch = attestation("pass", "full").replace(
            &format!("\"attested_rev\":\"{b}\""),
            &format!("\"attested_rev\":\"{a}\""),
        );
        assert!(!attestation_promotes(&mismatch, &a));
    }

    #[test]
    fn the_qualified_candidate_requires_both_commit_and_tree() {
        let (s, t) = ("5".repeat(40), "6".repeat(40));
        let json =
            format!("{{\"staging\":{{\"attested_rev\":\"{s}\"}},\"export\":{{\"tree\":\"{t}\"}}}}");
        assert_eq!(attested_candidate(&json), Some((s.clone(), t.clone())));
        assert_eq!(
            attested_candidate("{\"staging\":{\"attested_rev\":\"r\"}}"),
            None
        );
        assert_eq!(attested_candidate("not json"), None);
    }

    #[test]
    fn retained_candidate_is_checked_locally_and_missing_evidence_is_not_a_pass() {
        let dir = aterm_tempfile::tempdir().unwrap();
        let git = GitCli::new(dir.path());
        let run = |args: &[&str]| {
            let out = git.git(args).unwrap();
            assert!(out.success(), "{}", String::from_utf8_lossy(&out.stderr));
            out.stdout_utf8().trim().to_string()
        };
        run(&["init", "-q", "-b", "main"]);
        run(&[
            "-c",
            "user.name=test",
            "-c",
            "user.email=test@example.invalid",
            "commit",
            "--allow-empty",
            "-qm",
            "qualified",
        ]);
        let revision = run(&["rev-parse", "HEAD"]);
        let tree = run(&["rev-parse", "HEAD^{tree}"]);
        let reference = format!("refs/publication/candidates/{revision}");
        run(&["update-ref", &reference, &revision]);
        let out = dir.path().join("publish/.out");
        std::fs::create_dir_all(&out).unwrap();
        std::fs::write(out.join("attestation.json"), format!(
            "{{\"staging\":{{\"attested_rev\":\"{revision}\"}},\"export\":{{\"tree\":\"{tree}\"}}}}"
        )).unwrap();
        let mut real = Real::new(dir.path(), Some(dir.path()), true, false).unwrap();
        assert_eq!(real.candidate_missing().unwrap(), None);
        run(&["update-ref", "-d", "refs/heads/main"]);
        assert_eq!(real.candidate_missing().unwrap(), None);
        run(&["update-ref", "-d", &reference]);
        assert!(real.candidate_missing().unwrap().is_some());
        std::fs::rename(dir.path().join(".git"), dir.path().join("missing-git")).unwrap();
        assert!(real.candidate_missing().is_err());
    }

    #[test]
    fn dry_run_machine_checks_refuse_an_incomplete_source_signing_ring() {
        let dir = aterm_tempfile::tempdir().unwrap();
        let mut real = Real::new(dir.path(), Some(dir.path()), true, false).unwrap();
        real.home = dir.path().join("empty-home");
        let (_, refusals) = real.machine(&real.home.join("profile.toml"), false);
        assert!(
            refusals
                .iter()
                .any(|r| r.why.starts_with("source signing ring:"))
        );
    }

    /// The build and cut steps run this very executable or nothing: a cutter that cannot name
    /// its own executable builds no command for an `aterm-release` found on `PATH`, which would
    /// be handed nothing of this one; one that can runs that path, handed itself. NEGATIVE
    /// CONTROL: fall back to `aterm-release` on `PATH` (as before), and a command comes back.
    #[test]
    fn a_cutter_that_cannot_name_itself_starts_no_step() {
        let dir = aterm_tempfile::tempdir().unwrap();
        let real = Real::new(
            dir.path(),
            Some(Path::new("/other/publication")),
            true,
            false,
        )
        .expect("a backend");
        let prover = Prover {
            bin: PathBuf::from("/store/trust/9192/bin"),
            commit: "321aaeda7".into(),
            source: "the store".into(),
        };
        let args = ["cut".to_string()];
        let gone = real.cut_command_of(Err(std::io::Error::other("gone")), &args, &prover);
        assert_eq!(
            gone.err().as_deref(),
            Some("this cutter cannot name its own executable (gone)")
        );
        let exe = PathBuf::from("/t/gate/aterm-release");
        let cmd = real
            .cut_command_of(Ok(exe.clone()), &args, &prover)
            .unwrap();
        assert_eq!(cmd.get_program(), exe.as_os_str());
        assert_eq!(cmd.get_args().collect::<Vec<_>>(), ["cut"]);
        assert!(cmd.get_envs().any(|kv| kv
            == (
                std::ffi::OsStr::new(RELEASE_CUTTER_ENV),
                Some(exe.as_os_str())
            )));
    }

    #[test]
    fn the_engine_the_release_judges_is_the_one_every_child_reads() {
        let dir = aterm_tempfile::tempdir().unwrap();
        let mut real = Real::new(
            dir.path(),
            Some(Path::new("/other/publication")),
            true,
            false,
        )
        .expect("a backend");
        let prover = Prover {
            bin: PathBuf::from("/store/trust/9192/bin"),
            commit: "321aaeda7".into(),
            source: "the store".into(),
        };
        let engine = (
            std::ffi::OsStr::new("PUBLICATION_ENGINE"),
            Some(std::ffi::OsStr::new("/other/publication")),
        );
        let cut = real
            .cut_command(&["cut".into(), "--mac-only".into()], &prover)
            .unwrap();
        let envs: Vec<_> = cut.get_envs().collect();
        assert!(envs.contains(&engine), "{envs:?}");
        let exe = std::env::current_exe().unwrap();
        // The cut step's cutter is this executable, handed itself, so its cut gate judges it
        // from this process's moment; never judged at its start, this test process has none.
        for handed in [
            (RELEASE_CUTTER_ENV, Some(exe.as_os_str())),
            (RELEASE_CUTTER_SINCE_ENV, None),
        ] {
            assert!(
                envs.contains(&(std::ffi::OsStr::new(handed.0), handed.1)),
                "{handed:?}: {envs:?}"
            );
        }
        assert!(
            envs.contains(&(
                std::ffi::OsStr::new("TRUST_STAGE2_BIN"),
                Some(std::ffi::OsStr::new("/store/trust/9192/bin"))
            )),
            "{envs:?}"
        );
        let stage = real.child("/other/publication/bin/pub", Some(&prover));
        assert!(stage.get_envs().any(|kv| kv == engine));
        real.prerequisites = Some(crate::managed_prerequisites::Tools::Simulated);
        let publish = real.publish_command(&prover).unwrap();
        assert_eq!(
            publish.get_program(),
            std::ffi::OsStr::new("/other/publication/bin/pub")
        );
        assert!(
            publish.get_args().any(|a| a == "--skip-site"),
            "the site follows the cut, not the promote"
        );
        let args: Vec<_> = publish.get_args().collect();
        assert!(args.windows(2).any(|pair| {
            pair == [
                "--release-remote",
                "https://github.com/alabsystems/aterm.git",
            ]
        }));
        let envs: Vec<_> = publish.get_envs().collect();
        assert!(envs.contains(&engine), "{envs:?}");
        assert!(
            envs.contains(&(
                std::ffi::OsStr::new(RELEASE_CUTTER_ENV),
                Some(exe.as_os_str())
            )),
            "{envs:?}"
        );
        // Never judged at its start, this test process has no moment to hand on with itself.
        assert!(
            envs.contains(&(std::ffi::OsStr::new(RELEASE_CUTTER_SINCE_ENV), None)),
            "{envs:?}"
        );
        let edit = real.scrubbed("/other/publication/bin/pub-bump-edit");
        assert!(edit.get_envs().any(|kv| kv == engine));
    }

    #[cfg(unix)]
    /// EVERY CHILD THE RELEASE STARTS IS NAMED ITS DISK CLAIM WHILE IT HOLDS ONE: a `verify` it
    /// runs is covered by that claim alone (`aterm_capacity::covering_disk`), walking nothing. One
    /// this process inherited is never passed on, and once the claim is let go no child is named
    /// it. NEGATIVE CONTROL: before the claim, the same child carries no marker.
    #[test]
    fn every_child_is_named_the_releases_disk_claim_while_it_holds_one() {
        let gib = 1u64 << 30;
        let dir = aterm_tempfile::tempdir().unwrap();
        let mut real = Real::new(dir.path(), Some(dir.path()), true, false).unwrap();
        // `Real::new` keeps its record below this test binary's own root (`release_state`),
        // which every test in this process shares; a record of its own keeps "a run that did
        // not pass keeps nothing" below independent of them.
        real.state = dir.path().join("state");
        let marker = |cmd: &Command| {
            cmd.get_envs()
                .find(|(k, _)| *k == std::ffi::OsStr::new(aterm_capacity::DISK_COVER_ENV))
                .map(|(_, v)| v.map(std::ffi::OsStr::to_owned))
        };
        let mut inherited = command("/bin/true");
        scrub_names(
            &mut inherited,
            [std::ffi::OsString::from(aterm_capacity::DISK_COVER_ENV)],
        );
        assert_eq!(
            marker(&inherited),
            Some(None),
            "an inherited claim is removed"
        );
        assert_eq!(
            marker(&real.child("/bin/true", None)),
            None,
            "no claim, no marker"
        );

        let mut reading = aterm_capacity::Reading::of_gib(48, 40, 14);
        reading.volume = Some(aterm_capacity::Volume {
            mount: dir.path().to_path_buf(),
            total: 400 * gib,
            free: 300 * gib,
            floor: 10 * gib,
            room: 2 * gib,
        });
        let ledger = aterm_capacity::Ledger::at(dir.path().join("ledger"));
        let whole = atpkg::admit_disk::admit_sized_on(
            crate::release_disk::sized(dir.path(), 39 * gib, "release v0.6.0", Vec::new()),
            &ledger,
            &aterm_capacity::Fixed::new(reading),
            atpkg::admit_disk::Admit::Wait,
            aterm_capacity::Pace::default(),
            &mut atpkg::admit_disk::Reclaim::of(
                Box::new(|_, _| (0, Vec::new(), Vec::new())),
                Box::new(|_, _, _| None),
                Box::new(|_, _| Vec::new()),
            ),
            &mut |_| {},
        )
        .expect("room for the claim");
        let name = std::ffi::OsString::from(whole.marker());
        real.disk_claim = Some(whole);
        assert_eq!(marker(&real.child("/bin/true", None)), Some(Some(name)));

        real.let_disk_go(false);
        assert!(real.disk_claim.is_none());
        assert_eq!(marker(&real.child("/bin/true", None)), None, "let go");
        assert!(
            !crate::release_disk::record_in(&real.state).exists(),
            "a run that did not pass keeps nothing"
        );
    }

    #[test]
    fn scanner_export_preflight_uses_the_actual_dry_run_environment() {
        let dir = aterm_tempfile::tempdir().unwrap();
        let root = dir.path().canonicalize().unwrap();
        let repo = root.join("candidate");
        let export = root.join("export-candidate");
        std::fs::create_dir(&repo).unwrap();
        std::fs::create_dir(&export).unwrap();
        let mut real = Real::new(&repo, Some(&root.join("engine")), true, false).unwrap();
        real.prerequisites = Some(crate::managed_prerequisites::Tools::Simulated);
        let (mut stage, _) = real
            .dry_run_command(
                &root.join("scratch-engine"),
                "candidate",
                &export,
                &public_reads_at(0),
                &root.join("agh"),
            )
            .unwrap();
        let report = scanner_tools::prepare(
            &stage,
            real.engine_prefix_len(),
            &real.logs.join("export-scanner-tools.json"),
            &[&repo, &export],
            |probe| {
                assert_eq!(probe.get_program(), stage.get_program());
                assert_eq!(
                    probe.get_envs().collect::<Vec<_>>(),
                    stage.get_envs().collect::<Vec<_>>()
                );
                assert_eq!(
                    probe.get_args().collect::<Vec<_>>(),
                    [std::ffi::OsStr::new("scanner-preflight")]
                );
                Ok(scanner_tools::test_output())
            },
        )
        .unwrap();
        report.apply(&mut stage);
        assert!(
            stage
                .get_envs()
                .any(|(name, value)| name == scanner_tools::ENV && value.is_some())
        );
        assert_eq!(std::fs::read_dir(repo).unwrap().count(), 0);
        assert_eq!(std::fs::read_dir(export).unwrap().count(), 0);
    }

    #[cfg(unix)]
    #[test]
    fn scanner_failures_stop_the_export_and_its_tools_reach_only_the_engine() {
        let dir = aterm_tempfile::tempdir().unwrap();
        let repo = dir.path().join("repo");
        crate::ledger::testing::init(
            &repo,
            &[
                ("user.name", "Test"),
                ("user.email", "test@example.invalid"),
                ("commit.gpgsign", "false"),
            ],
        );
        let git = GitCli::new(&repo);
        git.git(&["commit", "--allow-empty", "-qm", "candidate"])
            .unwrap();
        let candidate = Candidate {
            commit: git
                .git(&["rev-parse", "HEAD"])
                .unwrap()
                .stdout_utf8()
                .trim()
                .into(),
            version: "1.0.0".into(),
            why: "test".into(),
            bump_from: None,
            carry: Vec::new(),
        };
        let bin = dir.path().join("bin");
        std::fs::create_dir(&bin).unwrap();
        std::os::unix::fs::symlink("/usr/bin/false", bin.join("targo")).unwrap();
        let prover = Prover {
            bin: bin.clone(),
            commit: "test".into(),
            source: "test".into(),
        };
        // An engine clone with no bin/pub, synced from its origin before the export runs.
        let engine = dir.path().join("engine");
        let origin = dir.path().join("engine-origin.git");
        crate::ledger::testing::init(
            &engine,
            &[
                ("user.name", "Test"),
                ("user.email", "test@example.invalid"),
                ("commit.gpgsign", "false"),
            ],
        );
        let engine_git = GitCli::new(&engine);
        engine_git
            .git(&["commit", "--allow-empty", "-qm", "engine"])
            .unwrap();
        GitCli::new(dir.path())
            .git(&["clone", "-q", "--bare", "engine", "engine-origin.git"])
            .unwrap();
        engine_git
            .git(&["remote", "add", "origin", &origin.display().to_string()])
            .unwrap();
        bind_native_engine_fixture(&engine, &origin);
        // A real run's backend (a dry one makes no engine to stage from), its logs its own.
        let backend = |engine: &Path, logs: &str| {
            let mut real = Real::new(&repo, Some(engine), false, false).unwrap();
            real.engine_origin = native_engine_fixture;
            real.engine_url = fixture_engine_url;
            real.logs = dir.path().join(logs);
            std::fs::create_dir_all(&real.logs).unwrap();
            real
        };
        let mut real = backend(&engine, "logs");
        real.home = dir.path().join("empty-home");
        real.prerequisite_acquire = crate::managed_prerequisites::simulated;
        let mut missing = backend(&dir.path().join("missing-engine"), "logs-missing");
        let unsynced =
            Backend::stage(&mut missing, &candidate, &prover, StageTarget::Remote).unwrap_err();
        assert!(
            unsynced.why.starts_with("no publication engine at"),
            "the engine is synced before the export: {unsynced:?}"
        );
        let scanner_failure =
            Backend::stage(&mut real, &candidate, &prover, StageTarget::Remote).unwrap_err();
        assert!(
            scanner_failure
                .why
                .starts_with("publication scanner preflight:")
        );
        assert!(real.scanner_tools.is_none());
        real.scanner_probe = |_| Ok(scanner_tools::test_output());
        let ran = Backend::stage(&mut real, &candidate, &prover, StageTarget::Remote).unwrap();
        assert_ne!(ran.code, 0, "the engine has no bin/pub: {ran:?}");
        let report = real.logs.canonicalize().unwrap().join("scanner-tools.json");
        for cmd in [
            real.with_scanners(
                real.stage_command(&candidate.commit, &prover, StageTarget::Remote)
                    .unwrap()
                    .0,
            ),
            real.publish_command(&prover).unwrap(),
        ] {
            assert!(cmd.get_envs().any(
                |(name, value)| name == scanner_tools::ENV && value == Some(report.as_os_str())
            ));
        }
        // Negative control: no other child of the run is handed them (the build's, verify's);
        // each has the name removed, so none inherits one either.
        assert!(
            real.child(prover.bin.join("targo"), Some(&prover))
                .get_envs()
                .any(|(name, value)| name == scanner_tools::ENV && value.is_none())
        );
        // The stage outside the graph (step 7's, again) is that step's child, in its log; the
        // graph's export is the export branch, in its own.
        let logged = |path: PathBuf| std::fs::read_to_string(path).unwrap_or_default();
        let branch_log = real.side_log(Step::Qualify, Branch::Export.name());
        assert!(
            logged(real.step_log(Step::Publish)).contains("\"stage\""),
            "the stage again logs as step 7's child"
        );
        assert!(!branch_log.exists(), "and not as step 4's export");
        let ran = Backend::branches(&mut real)
            .export(
                &candidate,
                &prover,
                StageTarget::Remote,
                &BranchStop::default(),
            )
            .unwrap();
        assert_ne!(ran.code, 0, "{ran:?}");
        assert!(logged(branch_log).contains("\"stage\""));
        // An engine that cannot be synced after the graph's export says nothing of R: COULD NOT
        // RUN, so it stops no other branch. NEGATIVE CONTROL: the same refusal outside the graph
        // stays a refusal.
        std::fs::rename(&origin, dir.path().join("gone.git")).unwrap();
        let unsynced = Backend::branches(&mut real)
            .export(
                &candidate,
                &prover,
                StageTarget::Remote,
                &BranchStop::default(),
            )
            .unwrap_err();
        assert!(
            unsynced.why.starts_with("cannot fetch the engine's origin") && unsynced.could_not_run,
            "{unsynced:?}"
        );
        let refused =
            Backend::stage(&mut real, &candidate, &prover, StageTarget::Remote).unwrap_err();
        assert!(
            refused.why.starts_with("cannot fetch the engine's origin") && !refused.could_not_run,
            "{refused:?}"
        );
        std::fs::rename(dir.path().join("gone.git"), &origin).unwrap();
        let failures: [scanner_tools::Probe; 3] = [
            |_| Err("engine missing or probe timed out".into()),
            |_| {
                let mut out = scanner_tools::test_output();
                out.stdout = b"unsupported command help".to_vec();
                Ok(out)
            },
            |_| {
                use std::os::unix::process::ExitStatusExt as _;
                let mut out = scanner_tools::test_output();
                out.status = std::process::ExitStatus::from_raw(1 << 8);
                out.stderr = b"zstd unavailable: restore the supported managed zstd tool".to_vec();
                Ok(out)
            },
        ];
        for (index, probe) in failures.into_iter().enumerate() {
            real.scanner_probe = probe;
            let refusal =
                Backend::stage(&mut real, &candidate, &prover, StageTarget::Remote).unwrap_err();
            assert!(refusal.why.starts_with("publication scanner preflight:"));
            if index == 2 {
                assert!(
                    refusal
                        .detail
                        .iter()
                        .any(|line| line.contains("zstd unavailable")
                            && line.contains("supported managed zstd"))
                );
            }
            assert!(real.scanner_tools.is_none());
            assert!(
                real.publish_command(&prover)
                    .unwrap()
                    .get_envs()
                    .any(|(name, value)| name == scanner_tools::ENV && value.is_none())
            );
        }
    }

    /// The fixture generator compiles the clone's working tree, so a clone off the candidate's
    /// tree is refused before it runs, in the build branch as in the after-cut step.
    #[cfg(unix)]
    #[test]
    fn the_fixture_generator_refuses_a_clone_off_the_candidates_tree() {
        let dir = aterm_tempfile::tempdir().unwrap();
        let repo = dir.path().join("repo");
        crate::ledger::testing::init(
            &repo,
            &[
                ("user.name", "Test"),
                ("user.email", "test@example.invalid"),
                ("commit.gpgsign", "false"),
            ],
        );
        let git = GitCli::new(&repo);
        git.git(&["commit", "--allow-empty", "-qm", "candidate"])
            .unwrap();
        let candidate = Candidate {
            commit: git
                .git(&["rev-parse", "HEAD"])
                .unwrap()
                .stdout_utf8()
                .trim()
                .into(),
            version: "1.0.0".into(),
            why: "test".into(),
            bump_from: None,
            carry: Vec::new(),
        };
        let bin = dir.path().join("bin");
        std::fs::create_dir(&bin).unwrap();
        std::os::unix::fs::symlink("/usr/bin/false", bin.join("targo")).unwrap();
        let prover = Prover {
            bin,
            commit: "test".into(),
            source: "test".into(),
        };
        let real = Real::new(&repo, Some(&dir.path().join("missing-engine")), true, false).unwrap();
        let log = dir.path().join("fixtures.log");
        let off_tree = |refusal: &Refusal| refusal.why.contains("tree and clean");
        let clean = real.stage_fixtures_run(
            Step::Qualify,
            &candidate,
            &Reservation::default(),
            &prover,
            &log,
        );
        assert!(!clean.as_ref().is_err_and(off_tree), "{clean:?}");
        std::fs::write(repo.join("untracked"), "changed").unwrap();
        let dirty = real
            .stage_fixtures_run(
                Step::Qualify,
                &candidate,
                &Reservation::default(),
                &prover,
                &log,
            )
            .unwrap_err();
        assert!(off_tree(&dirty), "{dirty:?}");
        assert!(
            dirty.detail.iter().any(|line| line.contains("untracked")),
            "{dirty:?}"
        );
    }

    /// The stage step's `pub stage` is a child with the prover's bin, handed this cutter only as
    /// the commit's own build: this test process, never judged at its start, hands none, and
    /// removes any it inherited. NEGATIVE CONTROL: build the command without
    /// [`hand_transforms_cutter`], and neither variable is removed.
    #[test]
    fn the_stage_step_is_handed_this_cutter_only_as_the_commits_own_build() {
        let dir = aterm_tempfile::tempdir().unwrap();
        let mut real = Real::new(
            dir.path(),
            Some(Path::new("/other/publication")),
            true,
            false,
        )
        .expect("a backend");
        let prover = Prover {
            bin: PathBuf::from("/store/trust/9192/bin"),
            commit: "321aaeda7".into(),
            source: "the store".into(),
        };
        let commit = "c".repeat(40);
        real.prerequisites = Some(crate::managed_prerequisites::Tools::Simulated);
        let (stage, handed) = real
            .stage_command(&commit, &prover, StageTarget::Remote)
            .unwrap();
        assert_eq!(
            stage.get_program(),
            std::ffi::OsStr::new("/other/publication/bin/pub")
        );
        let args: Vec<_> = stage.get_args().collect();
        let repo = dir.path().as_os_str();
        assert_eq!(
            args,
            ["stage", "aterm", "--commit", commit.as_str(), "--repo-path"]
                .map(std::ffi::OsStr::new)
                .into_iter()
                .chain([repo])
                .collect::<Vec<_>>()
        );
        let local = real
            .stage_command(&commit, &prover, StageTarget::Local)
            .unwrap()
            .0;
        let local_args: Vec<_> = local.get_args().collect();
        let mut expected_local = args.clone();
        expected_local.push(std::ffi::OsStr::new("--dry-run"));
        assert_eq!(local_args, expected_local);
        assert!(!local_args.contains(&std::ffi::OsStr::new("--no-check")));
        let envs: Vec<_> = stage.get_envs().collect();
        for kv in [
            ("PUBLICATION_ENGINE", Some("/other/publication")),
            ("TRUST_STAGE2_BIN", Some("/store/trust/9192/bin")),
            (STAGE_APPEND_ONLY_ENV, Some("1")),
            (RELEASE_CUTTER_ENV, None),
            (RELEASE_CUTTER_SINCE_ENV, None),
        ] {
            let want = (std::ffi::OsStr::new(kv.0), kv.1.map(std::ffi::OsStr::new));
            assert!(envs.contains(&want), "{kv:?}: {envs:?}");
        }
        // Both stage commands stage append-only: the engine's GitHub transport has no forced
        // write, so the legacy orphan-snapshot replacement refuses at the staging push.
        let local_envs: Vec<_> = local.get_envs().collect();
        let append_only = (
            std::ffi::OsStr::new(STAGE_APPEND_ONLY_ENV),
            Some(std::ffi::OsStr::new("1")),
        );
        assert!(local_envs.contains(&append_only), "{local_envs:?}");
        assert!(
            handed.contains("no commit's build")
                && handed.ends_with("ccccccccc's publish/transforms.sh builds its own"),
            "{handed}"
        );
    }

    /// Frozen tools share verify's main build directory, and only the test-profile verifier
    /// widens to the workspace test graph. NEGATIVE CONTROL: applying those words to the gate
    /// cutter would change its compiled feature set.
    #[test]
    fn frozen_tools_use_the_main_layout_and_only_verify_uses_the_test_graph() {
        let repo = Path::new("/release");
        let layout = "build.build-dir='/cache/build.noindex'";
        let words = |args: Vec<std::ffi::OsString>| {
            args.into_iter()
                .map(|word| word.into_string().unwrap())
                .collect::<Vec<_>>()
        };
        let cutter = words(Real::frozen_tool_args(
            repo,
            &FROZEN_CUTTER,
            "gate",
            Some(layout),
        ));
        let verify = words(Real::frozen_tool_args(
            repo,
            &FROZEN_VERIFY,
            "test",
            Some(layout),
        ));
        assert_eq!(&cutter[..4], ["--config", layout, "--unverified", "build"]);
        assert!(
            !cutter
                .iter()
                .any(|word| word == aterm_toolchain::build_cache::TEST_GRAPH_FLAG)
        );
        let [config, workspace] = aterm_toolchain::build_cache::TEST_GRAPH_CONFIG;
        assert_eq!(
            &verify[..7],
            [
                "--config",
                layout,
                "--unverified",
                config,
                workspace,
                "build",
                aterm_toolchain::build_cache::TEST_GRAPH_FLAG
            ]
        );
        for (args, package, profile) in [
            (&cutter, FROZEN_CUTTER.package, "gate"),
            (&verify, FROZEN_VERIFY.package, "test"),
        ] {
            assert!(args.windows(2).any(|pair| pair == ["--profile", profile]));
            assert!(args.windows(2).any(|pair| pair == ["-p", package]));
            assert!(
                args.windows(2)
                    .any(|pair| pair == ["--manifest-path", "/release/Cargo.toml"])
            );
        }
    }

    /// A Cargo test binary's release keeps its state, its logs included, below that binary's own
    /// root ([`release_state`]), never below the person's HOME, whose logs, step times, footprints
    /// and kept seal the next real release reads (m3's logs held empty run folders this crate's
    /// tests had made). NEGATIVE CONTROL: resolve the state from HOME alone, and this run's log
    /// folder is made below the person's.
    #[test]
    fn a_test_binarys_release_keeps_its_state_below_its_own_root() {
        let dir = aterm_tempfile::tempdir().unwrap();
        let real = Real::new(dir.path(), Some(dir.path()), false, false).expect("a backend");
        let root = aterm_uds::test_binary::scratch_root().expect("a test binary's root");
        let home = PathBuf::from(std::env::var_os("HOME").expect("HOME in tests"));
        assert_eq!(real.state, root.join(RELEASE_STATE));
        assert!(
            real.logs.starts_with(&real.state) && real.logs.is_dir(),
            "{}",
            real.logs.display()
        );
        assert!(
            !real.logs.starts_with(home.join(RELEASE_STATE)),
            "{}",
            real.logs.display()
        );
    }

    /// A handed cutter carries the moment it was judged from, with this process as the one that
    /// handed it, as the cutter it hands reads it, or none: an inherited one is removed.
    /// NEGATIVE CONTROL: leave the variable alone when there is no moment, and an inherited one
    /// reaches the handed cutter.
    #[test]
    fn a_handed_cutter_carries_the_moment_it_was_judged_from() {
        let exe = Path::new("/t/gate/aterm-release");
        let at = std::time::SystemTime::UNIX_EPOCH + std::time::Duration::new(1_793_520_000, 7);
        let since = |cmd: &Command| {
            cmd.get_envs()
                .find(|(k, _)| *k == std::ffi::OsStr::new(RELEASE_CUTTER_SINCE_ENV))
                .map(|(_, v)| v.map(std::ffi::OsStr::to_os_string))
        };
        let mut cmd = command("/other/publication/bin/pub");
        hand_this_cutter(&mut cmd, exe, Some(at));
        let value = crate::cutter::hand_value(at);
        #[cfg(target_os = "macos")]
        assert!(
            value.as_deref().is_some_and(
                |v| v.starts_with(&format!("1793520000000000007 {} ", std::process::id()))
            ),
            "the moment and this process's pid, in nanoseconds: {value:?}"
        );
        assert_eq!(
            since(&cmd),
            Some(value.map(Into::into)),
            "handed by this process"
        );
        assert!(cmd.get_envs().any(|kv| kv
            == (
                std::ffi::OsStr::new(RELEASE_CUTTER_ENV),
                Some(exe.as_os_str())
            )));
        let mut cmd = command("/other/publication/bin/pub");
        hand_this_cutter(&mut cmd, exe, None);
        assert_eq!(since(&cmd), Some(None), "none, and none inherited");
    }

    /// A step runs without the cutter or the build-inputs seal this process may have been
    /// handed: a child has [`RELEASE_CUTTER_ENV`] and [`RELEASE_CUTTER_SINCE_ENV`] only from
    /// [`hand_this_cutter`] and [`crate::build_inputs::SEAL_ENV`] only from
    /// [`hand_inputs_seal`] (this process's kept seal, else none), never inherited, and keeps
    /// what the scrub does not name. NEGATIVE CONTROL: scrub only what [`clears`] and
    /// [`launcher_env`] name, and all three reach the child.
    #[test]
    fn a_step_inherits_no_handed_cutter() {
        let seal = crate::build_inputs::SEAL_ENV;
        let mut cmd = command("/usr/bin/true");
        let inherited = [
            RELEASE_CUTTER_ENV,
            RELEASE_CUTTER_SINCE_ENV,
            seal,
            "HOME",
            "RUSTFLAGS",
        ];
        scrub_names(&mut cmd, inherited.map(std::ffi::OsString::from));
        let removed: Vec<_> = cmd
            .get_envs()
            .filter(|(_, v)| v.is_none())
            .map(|(k, _)| k.to_os_string())
            .collect();
        assert_eq!(
            removed,
            [
                RELEASE_CUTTER_ENV,
                RELEASE_CUTTER_SINCE_ENV,
                seal,
                "RUSTFLAGS"
            ]
        );
        hand_this_cutter(&mut cmd, Path::new("/t/gate/aterm-release"), None);
        assert!(cmd.get_envs().any(|kv| kv
            == (
                std::ffi::OsStr::new(RELEASE_CUTTER_ENV),
                Some(std::ffi::OsStr::new("/t/gate/aterm-release"))
            )));
        let handed = |cmd: &Command| {
            cmd.get_envs()
                .find(|(k, _)| *k == std::ffi::OsStr::new(seal))
                .map(|(_, v)| v.map(std::ffi::OsStr::to_os_string))
        };
        hand_inputs_seal(&mut cmd, None);
        assert_eq!(handed(&cmd), Some(None), "none kept, none handed");
        let kept = PathBuf::from("/state/aterm-release/inputs-seal/k.toml");
        hand_inputs_seal(&mut cmd, Some(kept.clone()));
        assert_eq!(handed(&cmd), Some(Some(kept.into_os_string())));
    }

    /// A RELEASE'S OWN FROZEN CUTTER TAKES THE ENGINE SOURCE AND REPOSITORY THE RELEASE CAPTURED:
    /// the exec hands both ([`hand_engine_capture`]) beside the release's copy as
    /// `PUBLICATION_ENGINE`, the engine a cutter older than the handover syncs, never the source;
    /// no other child inherits them ([`scrub_names`]); and the new image (this test binary run with
    /// the exec's engine handover, `Real::new` reading its environment) takes the release's source
    /// and holds to its repository ([`engine_authority`]), and hands both to the next image it
    /// would exec. A source that names another repository since the capture is refused, as is a
    /// handover from a release that admitted none, with that release's own reason, and either
    /// refusal is handed on unchanged ([`ENGINE_REPOSITORY_NONE`], [`ENGINE_REPOSITORY_WHY_ENV`]),
    /// so no later hop adopts what the copy names. NEGATIVE CONTROL: the exec of a release older
    /// than the handover, which names only the copy, has the image take the copy as its source and
    /// hold to the other repository the copy names.
    #[test]
    fn a_releases_frozen_cutter_takes_the_engine_source_and_repository_it_captured() {
        const PROBE: &str = "ATERM_RELEASE_ENGINE_CAPTURE_PROBE";
        const HANDOVER: [&str; 4] = [
            "PUBLICATION_ENGINE",
            ENGINE_SOURCE_ENV,
            ENGINE_REPOSITORY_ENV,
            ENGINE_REPOSITORY_WHY_ENV,
        ];
        let named = |cmd: &Command, name: &str| {
            cmd.get_envs()
                .find(|(k, _)| *k == std::ffi::OsStr::new(name))
                .map(|(_, v)| v.map(std::ffi::OsStr::to_os_string))
        };
        if let Some(repo) = std::env::var_os(PROBE) {
            let real = Real::new(Path::new(&repo), None, true, false).unwrap();
            let next = real.cutter_command(Path::new("/usr/bin/true"));
            let next = |name: &str| {
                named(&next, name)
                    .flatten()
                    .map(|v| v.to_string_lossy().into_owned())
                    .unwrap_or_default()
            };
            println!(
                "PROBE source={} repository={} next_source={} next_repository={} next_why={}",
                real.engine_source.display(),
                real.engine_repository
                    .as_ref()
                    .map_or_else(|why| format!("refused: {why}"), ToString::to_string),
                next(ENGINE_SOURCE_ENV),
                next(ENGINE_REPOSITORY_ENV),
                next(ENGINE_REPOSITORY_WHY_ENV),
            );
            // A dry run removes its scratch logs when it drops, and `exit` runs no destructor.
            drop(real);
            std::process::exit(0);
        }
        let slug = |s: &str| agh::Slug::parse(s).unwrap();
        let handed = |s: &str| Some(std::ffi::OsString::from(s));
        assert_eq!(
            engine_authority(Ok(slug("foreign/app")), handed(""), handed("no origin")),
            Ok(slug("foreign/app"))
        );
        assert_eq!(
            engine_authority(
                Ok(slug("fixture/app")),
                handed(ENGINE_REPOSITORY_NONE),
                handed("no origin")
            ),
            Err("no origin".to_owned()),
            "a release that admitted none is refused for its own reason"
        );
        for refused in [
            engine_authority(Ok(slug("foreign/app")), handed("fixture/app"), None),
            engine_authority(Err("no origin".into()), handed("fixture/app"), None),
            engine_authority(Ok(slug("fixture/app")), handed("not one repository"), None),
            engine_authority(
                Ok(slug("fixture/app")),
                handed(ENGINE_REPOSITORY_NONE),
                None,
            ),
            engine_authority(
                Ok(slug("fixture/app")),
                handed(ENGINE_REPOSITORY_NONE),
                handed(""),
            ),
        ] {
            assert!(refused.is_err(), "{refused:?}");
        }
        let mut inherited = command("/usr/bin/true");
        let captured = &HANDOVER[1..];
        scrub_names(
            &mut inherited,
            captured.iter().map(std::ffi::OsString::from),
        );
        for name in captured {
            assert_eq!(
                named(&inherited, name),
                Some(None),
                "{name} is not inherited"
            );
        }

        let dir = aterm_tempfile::tempdir().unwrap();
        let root = dir.path().canonicalize().unwrap();
        let checkout = |name: &str, url: &str| {
            let at = root.join(name);
            crate::ledger::testing::init(&at, &[]);
            crate::ledger::git_ok(&GitCli::new(&at), &["remote", "add", "origin", url]).unwrap();
            at
        };
        let source = checkout("publication", "https://github.com/fixture/app.git");
        let copy = checkout("copy", "https://github.com/foreign/app.git");
        let moved = checkout("moved", "https://github.com/other/app.git");
        let repo = root.join("aterm");
        std::fs::create_dir(&repo).unwrap();
        let probe = |exec: &Command| {
            let mut cmd = command(std::env::current_exe().unwrap());
            cmd.args([
                "--exact",
                "release_steps::tests::a_releases_frozen_cutter_takes_the_engine_source_and_repository_it_captured",
                "--nocapture",
                "--test-threads=1",
            ])
            .env(PROBE, &repo)
            .stdin(Stdio::null());
            for name in HANDOVER {
                cmd.env_remove(name);
                if let Some(Some(value)) = named(exec, name) {
                    cmd.env(name, value);
                }
            }
            let out = cmd.output().unwrap();
            assert!(out.status.success(), "{out:?}");
            let stdout = String::from_utf8_lossy(&out.stdout).into_owned();
            stdout
                .lines()
                .find_map(|line| line.split_once("PROBE ").map(|(_, probe)| probe.to_owned()))
                .unwrap_or_else(|| panic!("no probe line: {stdout}"))
        };
        let (s, c, m) = (source.display(), copy.display(), moved.display());

        let mut real = Real::new(&repo, Some(&source), true, false).unwrap();
        assert_eq!(real.engine_repository, Ok(slug("fixture/app")));
        // A real run's engine is its copy; this dry one has none made.
        real.engine.clone_from(&copy);
        let cutter = Path::new("/usr/bin/true");
        let exec = real.cutter_command(cutter);
        assert_eq!(
            named(&exec, "PUBLICATION_ENGINE"),
            Some(Some(copy.clone().into_os_string())),
            "a cutter older than the handover is named the release's copy, never its source"
        );
        assert_eq!(
            probe(&exec),
            format!(
                "source={s} repository=fixture/app next_source={s} next_repository=fixture/app \
                 next_why="
            )
        );
        assert_eq!(
            probe(&real.scrubbed(cutter)),
            format!(
                "source={c} repository=foreign/app next_source={c} next_repository=foreign/app \
                 next_why="
            ),
            "NEGATIVE CONTROL: the exec of a release older than the handover"
        );
        real.engine_source.clone_from(&moved);
        let why = engine_authority(Ok(slug("other/app")), handed("fixture/app"), None).unwrap_err();
        assert_eq!(
            probe(&real.cutter_command(cutter)),
            format!(
                "source={m} repository=refused: {why} next_source={m} next_repository=none \
                 next_why={why}"
            ),
            "a source naming another repository since the capture"
        );
        real.engine_repository = Err("no origin".into());
        real.engine_source.clone_from(&copy);
        assert_eq!(
            probe(&real.cutter_command(cutter)),
            format!(
                "source={c} repository=refused: no origin next_source={c} next_repository=none \
                 next_why=no origin"
            ),
            "a release that admitted none, refused for its own reason"
        );
    }

    /// NEGATIVE CONTROL: leaking a launch envelope lets a nested cutter mistake the
    /// parent's proof or retry budget for its own.
    #[test]
    fn only_exec_explicitly_hands_the_freezer_envelope() {
        let names = [
            crate::release_tools::RECORD_ENV,
            crate::release_tools::FREEZER_ENV,
            crate::release_tools::CONTRACT_ENV,
            crate::release_tools::REFREEZE_ENV,
        ];
        let mut cmd = command("/usr/bin/true");
        scrub_names(&mut cmd, names.map(std::ffi::OsString::from));
        for name in names {
            assert!(
                cmd.get_envs()
                    .any(|(key, value)| key == name && value.is_none())
            );
        }
    }

    /// NEGATIVE CONTROL for the scrub above: a child that runs this frozen image (step 7's
    /// `cut --sign`, step 8's `cut`) gets back the record this process was proved by, and no
    /// part of the freezer envelope. Without it the copy falls back to a dep-info judgment no
    /// copy passes: 0.177.0 refused at step 7 on 2026-11-08 ("this aterm-release binary is no
    /// commit's cutter: cannot read …/aterm-release.d") when only the scrub ran. A cutter that
    /// was not proved hands no record.
    #[test]
    fn a_child_of_this_frozen_image_is_handed_its_proof_and_never_the_envelope() {
        use crate::release_tools::{CONTRACT_ENV, FREEZER_ENV, RECORD_ENV, REFREEZE_ENV};
        let names = [RECORD_ENV, FREEZER_ENV, CONTRACT_ENV, REFREEZE_ENV];
        let exe = Path::new("/release/tools/0a/aterm-release");
        let record = std::ffi::OsStr::new("/release/cutter.toml");

        let mut proved = command("/usr/bin/true");
        scrub_names(&mut proved, names.map(std::ffi::OsString::from));
        hand_cutter_with_proof(&mut proved, exe, None, Some(record.to_owned()));
        let envs: Vec<_> = proved.get_envs().collect();
        assert!(
            envs.contains(&(std::ffi::OsStr::new(RECORD_ENV), Some(record))),
            "{envs:?}"
        );
        assert!(
            envs.contains(&(
                std::ffi::OsStr::new(RELEASE_CUTTER_ENV),
                Some(exe.as_os_str())
            )),
            "{envs:?}"
        );
        for name in [FREEZER_ENV, CONTRACT_ENV, REFREEZE_ENV] {
            assert!(
                envs.contains(&(std::ffi::OsStr::new(name), None)),
                "{name}: {envs:?}"
            );
        }

        let mut built = command("/usr/bin/true");
        scrub_names(&mut built, names.map(std::ffi::OsString::from));
        hand_cutter_with_proof(&mut built, exe, None, None);
        assert!(
            built
                .get_envs()
                .any(|(key, value)| key == RECORD_ENV && value.is_none())
        );
    }

    /// Step 1's seal arms keeping in the release state, then takes the seal, whose failure is
    /// the step's; every child the backend starts (a step or the cut) is handed the kept seal,
    /// and none while none is kept. CONTROL: off macOS step 1 arms and
    /// takes nothing. The seal's reports go to the run's transcript, as the cutter's stderr
    /// reaches no log. NEGATIVE CONTROL: drop the hand-off from [`Real::scrubbed`], the arm
    /// from [`Real::seal_inputs`], or its transcript, and the children carry no seal, nothing
    /// is armed, or the cutter's own seal calls go uncounted.
    #[test]
    fn step_one_keeps_the_inputs_seal_and_every_child_is_handed_it() {
        thread_local! {
            static ARMED: std::cell::RefCell<Vec<(PathBuf, Option<PathBuf>)>> =
                const { std::cell::RefCell::new(Vec::new()) };
            static TAKEN: std::cell::Cell<usize> = const { std::cell::Cell::new(0) };
        }
        let dir = aterm_tempfile::tempdir().unwrap();
        let mut real = Real::new(dir.path(), Some(dir.path()), true, false).expect("a backend");
        real.arm_inputs = |state, log| {
            ARMED.with(|armed| {
                armed
                    .borrow_mut()
                    .push((state.into(), log.map(Path::to_path_buf)));
            });
        };
        real.build_inputs = || {
            TAKEN.with(|n| n.set(n.get() + 1));
            Ok("inputs".into())
        };
        let macos = cfg!(target_os = "macos");
        assert_eq!(real.seal_inputs(), Ok(()));
        assert_eq!(
            (
                ARMED.with(|armed| armed.borrow().clone()),
                TAKEN.with(std::cell::Cell::get)
            ),
            if macos {
                (
                    vec![(real.state.clone(), Some(real.logs.join("transcript.log")))],
                    1,
                )
            } else {
                (Vec::new(), 0)
            }
        );
        assert!(real.state.ends_with(RELEASE_STATE));
        real.build_inputs = || Err("build input changed while it was hashed: /sdk".into());
        assert_eq!(
            real.seal_inputs().is_err(),
            macos,
            "a seal that cannot be taken refuses step 1"
        );

        let seal = |cmd: &Command| {
            cmd.get_envs()
                .find(|(k, _)| *k == std::ffi::OsStr::new(crate::build_inputs::SEAL_ENV))
                .and_then(|(_, v)| v.map(std::ffi::OsStr::to_os_string))
        };
        let prover = Prover {
            bin: PathBuf::from("/store/trust/9192/bin"),
            commit: "321aaeda7".into(),
            source: "the store".into(),
        };
        let exe = || Ok(PathBuf::from("/t/gate/aterm-release"));
        let children = |real: &Real| {
            [
                real.child("/usr/bin/true", None),
                real.cut_command_of(exe(), &["cut".into()], &prover)
                    .unwrap(),
            ]
            .map(|cmd| seal(&cmd))
        };
        real.handed_inputs = || None;
        assert_eq!(children(&real), [None, None], "none kept, none handed");
        real.handed_inputs = || Some(PathBuf::from("/state/aterm-release/inputs-seal/k.toml"));
        let kept = Some(std::ffi::OsString::from(
            "/state/aterm-release/inputs-seal/k.toml",
        ));
        assert_eq!(children(&real), [kept.clone(), kept]);
    }

    #[test]
    fn a_stage_is_handed_this_cutter_for_its_transforms_only_as_the_commits_own_build() {
        let (commit, other) = ("c".repeat(40), "d".repeat(40));
        let exe = || Ok(PathBuf::from("/t/gate/aterm-release"));
        assert_eq!(
            transforms_cutter(Ok(commit.as_str()), exe(), &commit),
            Ok(PathBuf::from("/t/gate/aterm-release"))
        );
        let dirty = crate::cutter::NotOwn {
            why: "its checkout /w has uncommitted cutter sources (Cargo.lock)".into(),
            fix: String::new(),
        };
        for (built, words) in [
            (Ok(other.as_str()), "the build of ddddddddd, not ccccccccc"),
            (
                Err(&dirty),
                "no commit's build: its checkout /w has uncommitted",
            ),
        ] {
            let why = transforms_cutter(built, exe(), &commit).unwrap_err();
            assert!(why.contains(words), "{words}: {why}");
        }
        let unnamed = transforms_cutter(
            Ok(commit.as_str()),
            Err(std::io::Error::other("gone")),
            &commit,
        );
        assert!(
            unnamed
                .unwrap_err()
                .contains("cannot name its own executable (gone)")
        );
    }

    #[test]
    fn a_verbose_step_logs_both_streams_in_the_order_written() {
        let dir = aterm_tempfile::tempdir().unwrap();
        let mut real = Real::new(dir.path(), None, true, true).expect("a backend");
        let mut cmd = real.child("/bin/sh", None);
        cmd.args(["-c", "echo one; echo two >&2; echo three; exit 7"]);
        let ran = real.logged(Step::Sync, &mut cmd);
        assert_eq!(ran.code, 7, "{ran:?}");
        let log = std::fs::read_to_string(real.log_path(Step::Sync)).unwrap();
        let at = |needle: &str| {
            log.find(needle)
                .unwrap_or_else(|| panic!("{needle}: {log}"))
        };
        assert!(
            at("one\n") < at("two\n") && at("two\n") < at("three\n"),
            "{log}"
        );
    }

    /// The direct child exits only after its background descendant holds the shared output
    /// writer. The zero drain grace makes the step cancel immediately, and its witness must say
    /// the pump is gone before the return. NEGATIVE CONTROL: discarding the pump's join handle
    /// after the timed receive leaves the witness set until the descendant is released.
    #[cfg(unix)]
    #[test]
    fn a_logged_step_joins_its_pump_when_a_grandchild_holds_the_pipe() {
        let dir = aterm_tempfile::tempdir().unwrap();
        let go = dir.path().join("finish-after-return");
        let finished = dir.path().join("grandchild-finished");
        let token = dir.path().join("pump-token");
        std::fs::write(&token, b"pump-read-before-direct-exit\n").unwrap();
        let pump_active = std::sync::Arc::new(std::sync::atomic::AtomicBool::new(false));
        let real = Real::new(dir.path(), None, true, false).expect("a backend");
        let log = real.step_log(Step::Sync);
        let mut cmd = real.child("/bin/sh", None);
        cmd.arg("-c")
            .arg(
                "(token=$(/bin/cat \"$2\"); printf '%s\\n' \"$token\"; \
                 while [ ! -e \"$0\" ]; do /bin/sleep 0.01; done; : > \"$1\") & \
                 token=$(/bin/cat \"$2\"); \
                 while ! /usr/bin/grep -Fq \"$token\" \"$3\"; do /bin/sleep 0.01; done; exit 0",
            )
            .arg(&go)
            .arg(&finished)
            .arg(&token)
            .arg(&log);
        let ran = real.logged_in_with_drain(
            Logged {
                step: Step::Sync,
                log,
                stream: true,
                stop: None,
                push: None,
                arrivals: false,
            },
            &mut cmd,
            std::time::Duration::ZERO,
            Some(pump_active.clone()),
        );

        let active_after_return = pump_active.load(std::sync::atomic::Ordering::SeqCst);
        std::fs::write(&go, b"go").unwrap();
        aterm_spec::wait::until_within(
            aterm_spec::wait::MUST_HAPPEN,
            "the grandchild to finish after the pump assertion",
            || finished.exists().then_some(()),
        );
        assert_eq!(ran.code, 0, "{ran:?}");
        assert!(!active_after_return, "the output pump survived the return");
    }

    /// NEGATIVE CONTROLS: treating build-script executions or arbitrary JSON as artifacts
    /// overcounts compiled units; treating absent freshness as false claims an unobserved
    /// compile. A fresh build-script executable is still a compiler unit.
    #[test]
    fn frozen_compile_counts_only_artifacts_with_observed_freshness() {
        let output =
            br#"{"reason":"compiler-artifact","fresh":true,"target":{"kind":["custom-build"]}}
{"reason":"compiler-artifact","fresh":false}
{"reason":"compiler-artifact"}
{"reason":"build-script-executed","fresh":false}
{"fresh":false}
not json
"#;
        let line = frozen_build_event(
            "aterm-release",
            "gate",
            Some("build.build-dir='fixture'"),
            output,
        );
        let event: aterm_json::Value =
            aterm_json::from_str(line.strip_prefix("release-event: ").unwrap()).unwrap();
        assert_eq!(event["fresh_units"].as_u64(), Some(1));
        assert_eq!(event["compiled_units"].as_u64(), Some(1));
        assert_eq!(event["unknown_units"].as_u64(), Some(1));
        assert_eq!(
            event["build_layout"].as_str(),
            Some("build.build-dir='fixture'")
        );
    }

    /// Each step child leaves one `step-child-finished` record after its output: its exit, when
    /// it ran, and the CPU `wait4` booked for it and the descendants it reaped (here a
    /// grandchild that did all the work), with no `outcome`, so a child's exit is never a red.
    #[cfg(unix)]
    #[test]
    fn a_step_child_books_the_cpu_of_what_it_reaped_and_is_no_red() {
        let dir = aterm_tempfile::tempdir().unwrap();
        let real = Real::new(dir.path(), None, true, false).expect("a backend");
        let mut cmd = real.child("/bin/sh", None);
        cmd.args([
            "-c",
            "/bin/sh -c 'i=0; while [ $i -lt 200000 ]; do i=$((i+1)); done' & wait; echo done; exit 3",
        ]);
        let ran = real.logged(Step::Qualify, &mut cmd);
        assert_eq!(ran.code, 3, "{ran:?}");
        let log = std::fs::read_to_string(real.step_log(Step::Qualify)).unwrap();
        let line = log
            .lines()
            .find(|l| l.contains("\"step-child-finished\""))
            .unwrap_or_else(|| panic!("no record: {log}"));
        assert!(
            log.find("done\n").unwrap() < log.find(line).unwrap(),
            "the record follows the output: {log}"
        );
        let event: aterm_json::Value =
            aterm_json::from_str(line.strip_prefix("release-event: ").unwrap()).unwrap();
        assert_eq!(event["step"].as_str(), Some("qualify"), "{event}");
        assert_eq!(event["exit_code"].as_u64(), Some(3), "{event}");
        assert!(event.get("outcome").is_none(), "{event}");
        assert!(
            event["started_at_unix_ms"].as_u64() <= event["ended_at_unix_ms"].as_u64(),
            "{event}"
        );
        let cpu = event["child_cpu"]["observed_s"]
            .as_f64()
            .expect("wait4 CPU");
        assert!(cpu > 0.0, "{event}");
        let user = event["child_cpu"]["user_s"]
            .as_f64()
            .expect("wait4 user CPU");
        let system = event["child_cpu"]["system_s"]
            .as_f64()
            .expect("wait4 system CPU");
        assert!(user > 0.0 && system >= 0.0, "{event}");
        assert!((cpu - user - system).abs() < 1e-9, "{event}");
        assert!(
            event["program"].as_str().unwrap().starts_with("sh -c"),
            "{event}"
        );
    }

    /// A step child whose output the step reads (the fixture generator, the export dry run)
    /// is booked as a logged one is, and its output still comes back whole and apart.
    #[test]
    fn a_captured_step_child_is_booked_and_keeps_its_output() {
        let dir = aterm_tempfile::tempdir().unwrap();
        let real = Real::new(dir.path(), None, true, false).expect("a backend");
        let mut cmd = real.child("/bin/sh", None);
        cmd.args([
            "-c",
            "i=0; while [ $i -lt 200000 ]; do i=$((i+1)); done; echo out; echo err >&2; exit 4",
        ]);
        let (out, record) = real.output_accounted(Step::Qualify, &mut cmd).unwrap();
        assert_eq!(out.status.code(), Some(4));
        assert_eq!(
            (out.stdout.as_slice(), out.stderr.as_slice()),
            (&b"out\n"[..], &b"err\n"[..])
        );
        let event: aterm_json::Value =
            aterm_json::from_str(record.strip_prefix("release-event: ").unwrap()).unwrap();
        assert_eq!(event["step"].as_str(), Some("qualify"), "{event}");
        assert_eq!(event["exit_code"].as_u64(), Some(4), "{event}");
        assert!(event.get("outcome").is_none(), "{event}");
        let cpu = event["child_cpu"]["observed_s"]
            .as_f64()
            .expect("wait4 CPU");
        assert!(cpu > 0.0, "{event}");
        let user = event["child_cpu"]["user_s"]
            .as_f64()
            .expect("wait4 user CPU");
        let system = event["child_cpu"]["system_s"]
            .as_f64()
            .expect("wait4 system CPU");
        assert!(user > 0.0 && system >= 0.0, "{event}");
        assert!((cpu - user - system).abs() < 1e-9, "{event}");
    }

    /// The publish step's lines are stamped as they arrive, in a side log, even when they are
    /// not echoed: the split of the engine's time comes from nowhere else.
    #[cfg(unix)]
    #[test]
    fn the_publish_steps_lines_are_stamped_as_they_arrive() {
        let dir = aterm_tempfile::tempdir().unwrap();
        let mut real = Real::new(dir.path(), None, true, false).expect("a backend");
        real.scratch_logs = false;
        let mut cmd = real.child("/bin/sh", None);
        cmd.args([
            "-c",
            "echo '== first section'; echo body >&2; echo '== second'",
        ]);
        let ran = real.logged(Step::Publish, &mut cmd);
        real.scratch_logs = true;
        assert_eq!(ran.code, 0, "{ran:?}");
        let log = std::fs::read_to_string(real.step_log(Step::Publish)).unwrap();
        assert!(
            log.contains("== first section\n") && log.contains("body\n"),
            "{log}"
        );
        let arrivals = std::fs::read_to_string(real.side_log(Step::Publish, "arrivals")).unwrap();
        let lines: Vec<aterm_json::Value> = arrivals
            .lines()
            .map(|l| aterm_json::from_str(l.strip_prefix("release-event: ").unwrap()).unwrap())
            .collect();
        assert_eq!(lines.len(), 3, "{arrivals}");
        assert!(
            lines
                .iter()
                .all(|e| e["event"].as_str() == Some("publish-line")
                    && e["at_unix_ms"].as_u64().is_some())
        );
        assert!(
            lines
                .iter()
                .any(|e| e["line"].as_str() == Some("== second")),
            "{arrivals}"
        );
    }

    /// A signal the release was started ignoring (`nohup`) stays ignored, and every other
    /// one is caught.
    #[cfg(unix)]
    #[test]
    fn an_inherited_sig_ign_is_kept_and_the_others_are_caught() {
        const PROBE: &str = "ATERM_RELEASE_SIGNAL_PROBE";
        const SIG_DFL: usize = 0;
        const SIG_IGN: usize = 1;
        unsafe extern "C" {
            fn signal(sig: i32, handler: usize) -> usize;
        }
        if let Some(mode) = std::env::var_os(PROBE) {
            // SAFETY: SIG_DFL is a disposition, no handler of ours.
            unsafe {
                // SIGTERM, which the probe sends itself, at its default action whatever this
                // test binary was started with: an ignored one stays ignored.
                signal(interrupt::SIGTERM, SIG_DFL);
                if mode == "control" {
                    signal(interrupt::SIGHUP, SIG_DFL);
                }
            }
            interrupt::install();
            // SAFETY: as above; the disposition read is put straight back.
            let hup = unsafe {
                let now = signal(interrupt::SIGHUP, SIG_IGN);
                signal(interrupt::SIGHUP, now);
                now
            };
            let hup = match hup {
                SIG_IGN => "ignored",
                SIG_DFL => "default",
                _ => "caught",
            };
            println!("PROBE hup={hup}");
            interrupt::send(std::process::id().cast_signed(), interrupt::SIGTERM);
            let term = aterm_spec::wait::until_within(
                aterm_spec::wait::MUST_HAPPEN,
                "the probe's own SIGTERM caught",
                interrupt::caught,
            );
            println!("PROBE term={term}");
            std::process::exit(0);
        }
        let probe = |nohup: bool, mode: &str| {
            let exe = std::env::current_exe().unwrap();
            let mut cmd = if nohup {
                let mut cmd = command(aterm_uds::os_path!("nohup"));
                cmd.arg(exe);
                cmd
            } else {
                command(exe)
            };
            cmd.args([
                "--exact",
                "release_steps::tests::an_inherited_sig_ign_is_kept_and_the_others_are_caught",
                "--nocapture",
                "--test-threads=1",
            ])
            .env(PROBE, mode);
            // Its stdin closed, within twice the hang detector: the probe's own detector says
            // first what never came.
            let out =
                aterm_uds::deadline::output_within(&mut cmd, aterm_spec::wait::MUST_HAPPEN * 2)
                    .expect("hang detector: the probe never ended");
            assert!(out.status.success(), "{out:?}");
            String::from_utf8_lossy(&out.stdout).into_owned()
        };
        let kept = probe(true, "nohup");
        assert!(kept.contains("PROBE hup=ignored"), "{kept}");
        assert!(kept.contains("PROBE term=15"), "{kept}");
        let control = probe(false, "control");
        assert!(control.contains("PROBE hup=caught"), "{control}");
        assert!(control.contains("PROBE term=15"), "{control}");
    }

    #[test]
    fn kept_audit_rows_go_back_in_time_order() {
        let text = "{\"at\":\"1\"}\n{\"at\":\"3\"}\n";
        let kept = vec!["{\"at\":\"2\"}".to_string(), "{\"at\":\"3\"}".to_string()];
        assert_eq!(
            audit_rows_merged(text, &kept),
            "{\"at\":\"1\"}\n{\"at\":\"2\"}\n{\"at\":\"3\"}\n"
        );
        assert_eq!(local_stamp(0, 0), "19700101T000000");
        assert_eq!(local_stamp(1_792_627_200, 7200), "20261022T020000");
        assert_eq!(local_stamp(1_792_627_200, -27_000), "20261021T163000");
        let (pdt, pst, fall_back) = (-7 * 3600, -8 * 3600, 1_793_523_600);
        let zone = |t: i64| Some(if t < fall_back { pdt } else { pst });
        assert_eq!(hms_local(1_793_518_200, zone), "00:30:00");
        assert_eq!(hms_local(1_793_527_200, zone), "02:00:00");
        assert_eq!(
            hms_local(1_793_527_200, |_| Some(pdt)),
            "03:00:00",
            "NEGATIVE CONTROL: start-of-run offset shifts post-change step times by an hour"
        );
        assert!(clears("RUSTFLAGS") && clears("CARGO_TARGET_DIR") && !clears("PATH"));
        for name in [
            "CARGO",
            "CARGO_PKG_NAME",
            "TARGO_MANIFEST_DIR",
            "TRUST_TARGO_FRONTEND",
        ] {
            assert!(launcher_env(name), "{name}");
        }
        assert!(!launcher_env("CARGO_HOME") && !launcher_env("HOME"));
    }

    #[cfg(unix)]
    mod nested_lock_files {
        use super::*;

        const NESTED_LOCK_BEFORE: &str = "version = 4\n\n[[package]]\nname = \"aterm-local\"\nversion = \"0.147.0\"\n\n[[package]]\nname = \"registry\"\nversion = \"0.147.0\"\nsource = \"registry+https://example.invalid/index\"\n";
        const NESTED_LOCK_AFTER: &str = "version = 4\n\n[[package]]\nname = \"aterm-local\"\nversion = \"0.148.0\"\n\n[[package]]\nname = \"registry\"\nversion = \"0.147.0\"\nsource = \"registry+https://example.invalid/index\"\n";

        fn ordinary_nested_bump(repo: &Path) {
            let parent = repo.join("ordinary");
            std::fs::create_dir_all(&parent).unwrap();
            std::fs::write(parent.join("Cargo.lock"), NESTED_LOCK_BEFORE).unwrap();
            assert_eq!(
                bump_nested_locks(
                    repo,
                    Ok(vec!["ordinary/Cargo.lock".into()]),
                    "0.147.0",
                    "0.148.0"
                )
                .unwrap(),
                ["ordinary/Cargo.lock"]
            );
            assert_eq!(
                std::fs::read_to_string(parent.join("Cargo.lock")).unwrap(),
                NESTED_LOCK_AFTER,
                "the local package moves and the registry version stays"
            );
        }

        #[cfg(unix)]
        #[test]
        fn nested_lock_bumps_refuse_aliases_without_touching_their_payloads() {
            use std::os::unix::fs::MetadataExt as _;

            let mut outcomes = Vec::new();
            for kind in ["symlink", "hardlink"] {
                let scratch = aterm_tempfile::tempdir().unwrap();
                let home = scratch.path().canonicalize().unwrap();
                let repo = home.join("repo");
                ordinary_nested_bump(&repo);
                std::fs::create_dir(repo.join("nested")).unwrap();
                let payload = home.join("outside-repo.lock");
                let alias = repo.join("nested/Cargo.lock");
                std::fs::write(&payload, NESTED_LOCK_BEFORE).unwrap();
                if kind == "symlink" {
                    std::os::unix::fs::symlink(&payload, &alias).unwrap();
                } else {
                    std::fs::hard_link(&payload, &alias).unwrap();
                }
                let identity = |path: &Path| {
                    let meta = std::fs::symlink_metadata(path).unwrap();
                    (meta.dev(), meta.ino(), meta.nlink())
                };
                let before = identity(&alias);
                let result = bump_nested_locks(
                    &repo,
                    Ok(vec!["nested/Cargo.lock".into()]),
                    "0.147.0",
                    "0.148.0",
                );
                outcomes.push((
                    kind,
                    result.is_err(),
                    identity(&alias) == before,
                    std::fs::read_to_string(&payload).unwrap() == NESTED_LOCK_BEFORE,
                ));
            }
            assert!(
                outcomes
                    .iter()
                    .all(|(_, refused, kept, untouched)| *refused && *kept && *untouched),
                "each alias must be refused and retain its external payload: {outcomes:?}"
            );
        }

        #[test]
        fn nested_lock_refusals_preserve_every_target_before_publication() {
            let mut outcomes = Vec::new();
            for kind in ["invalid-utf8", "missing", "directory", "listing-refused"] {
                let scratch = aterm_tempfile::tempdir().unwrap();
                let repo = scratch.path().canonicalize().unwrap();
                ordinary_nested_bump(&repo);
                std::fs::create_dir(repo.join("good")).unwrap();
                std::fs::write(repo.join("good/Cargo.lock"), NESTED_LOCK_BEFORE).unwrap();
                std::fs::create_dir(repo.join("unknown")).unwrap();
                match kind {
                    "invalid-utf8" => {
                        std::fs::write(repo.join("unknown/Cargo.lock"), [0xff]).unwrap()
                    }
                    "directory" => std::fs::create_dir(repo.join("unknown/Cargo.lock")).unwrap(),
                    "missing" | "listing-refused" => {}
                    _ => unreachable!(),
                }
                let listing = if kind == "listing-refused" {
                    Err("inert git enumeration refusal".to_owned())
                } else {
                    Ok(vec!["good/Cargo.lock".into(), "unknown/Cargo.lock".into()])
                };
                let result = bump_nested_locks(&repo, listing, "0.147.0", "0.148.0");
                outcomes.push((
                    kind,
                    result.is_err(),
                    std::fs::read_to_string(repo.join("good/Cargo.lock")).unwrap()
                        == NESTED_LOCK_BEFORE,
                ));
            }
            assert!(
                outcomes
                    .iter()
                    .all(|(_, refused, unchanged)| *refused && *unchanged),
                "unknown evidence must stop the batch before any target changes: {outcomes:?}"
            );
        }
    }

    /// The bump moves a nested lock's path crates with the root's and leaves a
    /// registry crate that happens to share the old version where it was.
    #[test]
    fn a_nested_lock_follows_the_bump_in_its_path_crates_only() {
        use super::lock_follows_bump;
        let lock = "version = 4\n\n[[package]]\nname = \"aterm-font\"\nversion = \"0.147.0\"\n\
                    dependencies = [\n \"aterm-hash\",\n]\n\n[[package]]\nname = \"lookalike\"\n\
                    version = \"0.147.0\"\nsource = \"registry+https://github.com/rust-lang/crates.io-index\"\n\
                    checksum = \"00\"\n\n[[package]]\nname = \"astream-wire\"\nversion = \"0.2.0\"\n\n\
                    [[package]]\nname = \"aterm-hash\"\nversion = \"0.147.0\"\n";
        let moved = lock_follows_bump(lock, "0.147.0", "0.148.0").expect("two path crates move");
        assert_eq!(
            moved,
            lock.replacen(
                "name = \"aterm-font\"\nversion = \"0.147.0\"",
                "name = \"aterm-font\"\nversion = \"0.148.0\"",
                1
            )
            .replacen(
                "name = \"aterm-hash\"\nversion = \"0.147.0\"",
                "name = \"aterm-hash\"\nversion = \"0.148.0\"",
                1
            )
        );
        assert!(moved.contains("name = \"lookalike\"\nversion = \"0.147.0\""));
        assert_eq!(lock_follows_bump(lock, "0.107.0", "0.148.0"), None);
        let crlf = "[[package]]\r\nname = \"aterm-uds\"\r\nversion = \"0.147.0\"\r\n";
        assert_eq!(
            lock_follows_bump(crlf, "0.147.0", "0.148.0").as_deref(),
            Some("[[package]]\r\nname = \"aterm-uds\"\r\nversion = \"0.148.0\"\r\n")
        );
        let ambiguous = "[[package]]\nname = \"leaf\"\nversion = \"0.147.0\"\n\n[[package]]\nname = \"user\"\nversion = \"0.0.0\"\ndependencies = [\n \"leaf 0.147.0\",\n \"leaf 0.147.0 (registry+https://example.invalid/index)\",\n]\n";
        assert_eq!(
            lock_follows_bump(ambiguous, "0.147.0", "0.148.0").unwrap(),
            ambiguous
                .replacen("version = \"0.147.0\"", "version = \"0.148.0\"", 1)
                .replacen("\"leaf 0.147.0\"", "\"leaf 0.148.0\"", 1)
        );
    }
}
