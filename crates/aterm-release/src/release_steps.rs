// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Real [`crate::release::Backend`] implementation for `targo --unverified ship release`.

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

fn preflight_cache(repo: &Path) -> Result<PathBuf, String> {
    let dir = crate::snapshot::CacheRoot::for_repo(repo)?.preflight();
    std::fs::create_dir_all(&dir).map_err(|e| format!("create {}: {e}", dir.display()))?;
    Ok(dir)
}

/// The variable `publish/pre-promote` reads for the running release's own cutter: set on `pub
/// publish`, whose engine passes it through to the hook. `publish/transforms.sh` reads it too: a
/// `pub stage` gets it only from [`hand_transforms_cutter`].
pub const RELEASE_CUTTER_ENV: &str = "ATERM_RELEASE_CUTTER";

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
/// unread.
fn hand_this_cutter(cmd: &mut Command, exe: &Path, since: Option<std::time::SystemTime>) {
    cmd.env(RELEASE_CUTTER_ENV, exe);
    match since.and_then(crate::cutter::hand_value) {
        Some(since) => cmd.env(RELEASE_CUTTER_SINCE_ENV, since),
        None => cmd.env_remove(RELEASE_CUTTER_SINCE_ENV),
    };
}

fn detach(cmd: &mut Command) {
    crate::credential_wait::ask_no_one(cmd);
    #[cfg(unix)]
    cmd.setsid(true);
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
        crate::credential_wait::ask_no_one(cmd).setsid(true);
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

/// The real backend.
pub struct Real {
    pub(crate) build_inputs: fn() -> Result<String, String>,
    /// Keeps this process's build-inputs seal in the release state, its reports also in the
    /// named log ([`crate::build_inputs::arm`]).
    pub(crate) arm_inputs: fn(&Path, Option<&Path>),
    /// The kept seal each child is handed ([`crate::build_inputs::handed_path`]).
    pub(crate) handed_inputs: fn() -> Option<PathBuf>,
    repo: PathBuf,
    home: PathBuf,
    /// The publication engine this release is named (`--engine`, else `$PUBLICATION_ENGINE`, else
    /// `~/publication`): it names the engine's origin and committer, and nothing here runs, syncs
    /// or writes it, since other processes move it ([`crate::gates::engine_copy`]).
    engine_source: PathBuf,
    /// The engine this release runs, syncs, reads and hands every child as `PUBLICATION_ENGINE`:
    /// its own copy ([`crate::gates::engine_copy`], made from `engine_source`'s origin by
    /// [`Real::engine_copy`]); in a dry run, which never makes or moves one, `engine_source` until a
    /// real run has made it.
    engine: PathBuf,
    /// The main of the engine's origin, fetched at step 1 ([`Backend::fetch`]): what the release
    /// judges of the engine (its ledger, its code) until it has synced its copy
    /// ([`Real::engine_text`]), which may be a release behind or not made yet; a dry run, which
    /// syncs none, judges it throughout.
    origin_main: Option<crate::gates::OriginMain>,
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
fn export_share(slots: u32, export: bool) -> u32 {
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

    fn tree_admitted(&self, ended: &dyn Fn() -> bool) {
        let me = std::process::id();
        let began = std::time::Instant::now();
        while !ended() && began.elapsed() < crate::release::TREE_HEAD_START {
            if interrupt::caught().is_some() || tree_holds_memory(me) {
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
        self.real.build_branch(c, reserved, prover, work, stop)
    }

    fn verify(
        &self,
        c: &Candidate,
        prover: &Prover,
        run: &crate::check_record::VerifyRun,
        artifact: Option<&Artifact>,
        stop: &BranchStop,
    ) -> Result<Ran, Refusal> {
        self.real.verify_half(c, prover, run, artifact, stop)
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

/// Whether the user's ledger holds an admitted memory claim of `verify release` under this
/// process `me`: the tree branch's, the only `verify` the release runs while its build waits.
fn tree_holds_memory(me: u32) -> bool {
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
    records.iter().any(|r| {
        r.held.is_some() && r.need.what.starts_with("verify ") && r.need.mem > 0 && descends(r.pid)
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
        let home = std::env::var_os("HOME")
            .filter(|h| !h.is_empty())
            .map(PathBuf::from)
            .ok_or("HOME is not set")?;
        let source = engine.map(Path::to_path_buf).unwrap_or_else(|| {
            std::env::var_os("PUBLICATION_ENGINE")
                .filter(|v| !v.is_empty())
                .map_or_else(|| home.join("publication"), PathBuf::from)
        });
        let engine_source = std::env::current_dir().map_or_else(
            |_| source.clone(),
            |cwd| crate::release::anchor(&source, &cwd),
        );
        let release = crate::gates::release_dir(repo);
        let copy = crate::gates::engine_copy(&release);
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
            std::env::temp_dir().join(format!("aterm-release-dry-{}-{nth}", std::process::id()))
        } else {
            state
                .join("logs")
                .join(format!("{stamp}-{}", std::process::id()))
        };
        std::fs::create_dir_all(&logs).map_err(|e| format!("create {}: {e}", logs.display()))?;
        Ok(Self {
            build_inputs: crate::build_inputs::current,
            arm_inputs: crate::build_inputs::arm,
            handed_inputs: crate::build_inputs::handed_path,
            repo: repo.to_path_buf(),
            home,
            engine_source,
            engine,
            origin_main: None,
            engine_synced: std::sync::atomic::AtomicBool::new(false),
            state,
            logs,
            release,
            scratch_logs: dry,
            verbose,
            origin_tags: String::new(),
            step: Step::Checks,
            argv: std::env::args_os().skip(1).collect(),
            source_signing: None,
            prerequisites: None,
            prerequisite_acquire: crate::managed_prerequisites::Tools::acquire,
            prepared_source_helper: None,
            scanner_tools: None,
            scanner_probe: scanner_tools::run,
            checkout_build: Real::build_checkout_bin,
            engine_agh_explicit: std::env::var_os(ENGINE_AGH_ENV),
            disk_claim: None,
            line_priority: None,
            fleet_line: None,
            pool: None,
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

    fn remote_git(&self, args: &[&str]) -> std::io::Result<std::process::Output> {
        let mut cmd = command(aterm_uds::os_path!("git"));
        cmd.arg("-C").arg(&self.repo).args(args);
        self.output_forwarded(&mut cmd)
    }

    fn engine_git(&self, args: &[&str]) -> Option<String> {
        let mut cmd = command(aterm_uds::os_path!("git"));
        cmd.arg("-C").arg(&self.engine).args(args);
        let out = self.output_forwarded(&mut cmd).ok()?;
        out.status
            .success()
            .then(|| String::from_utf8_lossy(&out.stdout).into_owned())
    }

    fn engine_git_code(&self, args: &[&str]) -> Result<(i32, std::process::Output), String> {
        let e = self.engine.display();
        let mut cmd = command(aterm_uds::os_path!("git"));
        cmd.arg("-C").arg(&self.engine).args(args);
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
        self.apply_prerequisites(cmd)
            .map_err(std::io::Error::other)?;
        detach(cmd);
        let child = cmd.spawn()?;
        let running = match stop {
            Some((stop, sig)) => interrupt::Running::enter_stoppable(child.id(), stop.clone(), sig),
            None => interrupt::Running::enter(child.id()),
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
        cmd.stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped());
        let started_at_unix_ms = now_ms();
        let (mut child, forwarded) = self.spawn_forwarded(cmd)?;
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

    fn engine_prefix_len(&self) -> usize {
        self.prerequisites
            .as_ref()
            .map_or(0, crate::managed_prerequisites::Tools::engine_prefix_len)
    }

    fn child(&self, program: impl AsRef<std::ffi::OsStr>, prover: Option<&Prover>) -> Command {
        self.configure_child(self.scrubbed(program), prover)
    }

    fn configure_child(&self, mut cmd: Command, prover: Option<&Prover>) -> Command {
        cmd.current_dir(&self.repo).stdin(Stdio::null());
        if let Some(p) = prover {
            cmd.env("TRUST_STAGE2_BIN", &p.bin);
        }
        cmd
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
        let ran = self.logged(Step::AfterCut, &mut command);
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
        cmd.args(["publish", "aterm", "--skip-site", "--repo-path"])
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
            detail: tail(&log, 8),
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
        let mut cmd = command(aterm_uds::os_path!("git"));
        cmd.arg("-C")
            .arg(&self.engine_source)
            .args(["remote", "get-url", "origin"]);
        self.output_forwarded(&mut cmd)
            .ok()
            .filter(|out| out.status.success())
            .map(|out| String::from_utf8_lossy(&out.stdout).trim().to_string())
            .filter(|url| !url.is_empty())
            .ok_or_else(|| {
                Refusal::new(
                    format!("the publication engine {s} names no origin"),
                    format!("git -C {s} remote add origin <the engine's repository>"),
                )
            })
    }

    /// The main of the engine's `origin` ([`crate::gates::OriginMain`]), or why it cannot be
    /// read.
    fn read_origin_main(&self, origin: &str) -> Result<crate::gates::OriginMain, Refusal> {
        crate::gates::OriginMain::fetch(origin, &|cmd| self.output_forwarded(cmd)).map_err(|why| {
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

    /// Step 1's reads of both origins: aterm's main and tags through `aterm` (agh's fetch, in
    /// `Backend::fetch`), then the engine's main ([`Real::fetch_engine_origin`]), which step 1
    /// judges the ledger and `bin/pub` on until step 2 syncs the release's engine; aterm's main.
    fn fetch_origins(
        &mut self,
        aterm: impl FnOnce(&Path) -> Result<crate::github_remote::FetchedOrigin, String>,
    ) -> Result<String, Refusal> {
        let origin = aterm(&self.repo).map_err(|why| {
            Refusal::new(
                "cannot refresh origin through agh",
                "check the reported native routing or source-state refusal, then re-run",
            )
            .detail([why])
        })?;
        self.origin_tags = origin.tags;
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

    /// The release's own engine ([`crate::gates::engine_copy`]): cloned from the origin
    /// `engine_source` names where there is none yet (into a name of its own, then renamed into
    /// place, so an interrupted clone is never taken for one), or where the one there was made
    /// from another origin and holds nothing that origin lacks (one that does is refused, never
    /// delivered to this origin), and given `engine_source`'s committer, which its audit-row and
    /// ledger commits are made as.
    /// Nothing is read from `engine_source` but its origin and committer. Never in a dry run, and
    /// nothing to do where the release was named its own copy (the re-run as R's own cutter is
    /// handed it as `PUBLICATION_ENGINE`).
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
        let git = |dir: &Path, args: &[&str]| {
            let mut cmd = command(aterm_uds::os_path!("git"));
            cmd.arg("-C").arg(dir).args(args);
            self.output_forwarded(&mut cmd)
                .is_ok_and(|out| out.status.success())
        };
        if self.engine.join(".git").exists()
            && self
                .engine_git(&["remote", "get-url", "origin"])
                .is_none_or(|url| url.trim() != origin)
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
            let made_s = made.display().to_string();
            let cloned = std::fs::create_dir_all(parent).is_ok()
                && git(
                    parent,
                    &[
                        &["clone"],
                        &BYTE_EXACT[..],
                        &["--quiet", "--no-checkout", &origin, &made_s],
                    ]
                    .concat(),
                )
                && git(
                    &made,
                    &[
                        "checkout",
                        "--quiet",
                        "--track",
                        "-B",
                        "main",
                        "origin/main",
                    ],
                );
            if !cloned || std::fs::rename(&made, &self.engine).is_err() {
                let _ = std::fs::remove_dir_all(&made);
                return Err(Refusal::new(
                    format!("could not clone the publication engine's origin ({origin}) into {e}"),
                    format!(
                        "check the network (git ls-remote {origin}) and that nothing else is at \
                         {e}, then {RELEASE_COMMAND} again"
                    ),
                ));
            }
        }
        for key in ["user.name", "user.email"] {
            let mut cmd = command(aterm_uds::os_path!("git"));
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

    fn engine_fast_forward(&self) -> Result<(), Refusal> {
        let e = self.engine.display().to_string();
        if self.engine_git(&["fetch", "-q", "origin"]).is_none() {
            return Err(Refusal::new(
                format!("cannot fetch the engine's origin ({e})"),
                format!("check the network and git -C {e} remote -v"),
            ));
        }
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
                .engine_git(&["diff", "--name-only", "origin/main...HEAD"])
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
            return if !ahead
                || self
                    .engine_git(&["push", "-q", "origin", "HEAD:main"])
                    .is_some()
            {
                Ok(())
            } else {
                Err(Refusal::new(
                    format!("could not push the records the engine {e} holds to its origin's main"),
                    format!("{RELEASE_COMMAND} again (they stay in {e} until they land)"),
                ))
            };
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
                .is_some()
            && self
                .engine_git(&["push", "-q", "origin", "HEAD:main"])
                .is_some();
        if recorded {
            Ok(())
        } else {
            Err(Refusal::new(
                "could not record the engine's audit rows (commit and push of \
                 verification-log.jsonl)",
                format!(
                    "{RELEASE_COMMAND} again (the rows are kept in {})",
                    kept.display()
                ),
            ))
        }
    }

    fn engine_dirty(&self) -> bool {
        self.engine_git(&["status", "--porcelain"])
            .unwrap_or_default()
            .lines()
            .any(|l| !l.ends_with(" verification-log.jsonl"))
    }

    fn submodule_sync(&self, step: Step) -> Result<(), Refusal> {
        let mut cmd = command(aterm_uds::os_path!("git"));
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
        crate::gates::submodules_on_declared_branch(&self.git()).map_err(|e| match e {
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
        let head = self.head();
        let ancestor = |s: &mut Self, a: &str, b: &str| s.is_ancestor(a, b);
        let mut cmd = command(aterm_uds::os_path!("git"));
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
        let s = self.engine_source.display().to_string();
        if self.engine_source.join(".git").exists() && !self.engine_source.join("bin/pub").is_file()
        {
            return Some(Refusal::new(
                format!("no publication engine at {s} (bin/pub)"),
                "clone it there, or name one: --engine <path> (or PUBLICATION_ENGINE)",
            ));
        }
        if let Err(why) = self.source_origin() {
            return Some(why);
        }
        if self.engine != crate::gates::engine_copy(&self.release)
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
            let mut cmd = command(aterm_uds::os_path!("git"));
            cmd.arg("-C").arg(&copy).args(args);
            cmd
        };
        let mut clone = command(aterm_uds::os_path!("git"));
        clone.arg("clone").args(BYTE_EXACT).args([
            "--quiet",
            "--shared",
            "--no-checkout",
            &e,
            &copy_s,
        ]);
        if !ran(&mut clone) {
            return Err(Refusal::new(
                format!("git could not copy the engine {e} into {copy_s}"),
                "fix what keeps it from being written, then run it again",
            ));
        }
        if !ran(&mut in_copy(&["remote", "set-url", "origin", &origin]))
            || !ran(&mut in_copy(&[
                "fetch",
                "--quiet",
                "--no-tags",
                "origin",
                "refs/heads/main",
            ]))
        {
            return Err(Refusal::new(
                format!("cannot fetch the main of the engine's origin ({origin})"),
                format!("check the network: git -C {e} ls-remote origin"),
            ));
        }
        if !ran(&mut in_copy(&[
            "checkout",
            "--quiet",
            "-B",
            "main",
            "FETCH_HEAD",
        ])) {
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
    ) -> Result<PathBuf, String> {
        let layout = verify_layout(&self.repo).1;
        let args = Self::frozen_tool_args(&self.repo, bin, profile, layout.as_deref());
        let mut command = self.child(prover.bin.join("targo"), Some(prover));
        command.args(&args).current_dir(&self.repo);
        let output = self
            .output_forwarded(&mut command)
            .map_err(|e| format!("{} build: {e}", bin.what))?;
        if let Ok(mut log) = crate::durable::open_append(&self.step_log(self.step), true) {
            let _ = log.write_all(&output.stdout);
            let _ = log.write_all(&output.stderr);
        }
        if !output.status.success() {
            return Err(format!(
                "{} build exited {}: {}",
                bin.what,
                output.status,
                String::from_utf8_lossy(&output.stderr)
            ));
        }
        crate::checkout_bin::built(&self.repo, bin, &output.stdout)
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
        let cutter = self.build_frozen_tool(prover, &FROZEN_CUTTER, "gate")?;
        let verify = self.build_frozen_tool(prover, &FROZEN_VERIFY, "test")?;
        let tools = crate::release_tools::stage(&self.release, commit, &cutter, &verify)?;
        crate::release_tools::clean_at(&self.repo, commit)?;
        crate::release_tools::publish(&self.release, &tools)?;
        Ok(tools)
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
            let mut git = crate::ledger::object_git(&self.repo);
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
        let git = |dir: &Path, args: &[&str]| -> bool {
            let out = self.output_forwarded(
                command(aterm_uds::os_path!("git"))
                    .arg("-C")
                    .arg(dir)
                    .args(args),
            );
            if let Ok(o) = &out
                && let Ok(mut f) = crate::durable::open_append(&log, false)
            {
                let _ = f.write_all(&o.stderr);
            }
            out.is_ok_and(|o| o.status.success())
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
        if !clone(&repo, &root_s)
            || !git(root, &["remote", "set-url", "origin", &origin])
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
            )
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
            let own = command(aterm_uds::os_path!("git"))
                .arg("-C")
                .arg(&sub)
                .args(["rev-parse", "--show-toplevel"])
                .output()
                .ok()
                .map(|o| PathBuf::from(String::from_utf8_lossy(&o.stdout).trim()));
            let is_own = own.and_then(|p| p.canonicalize().ok()) == sub.canonicalize().ok();
            if !is_own || !git(&sub, &["cat-file", "-e", &format!("{rev}^{{commit}}")]) {
                return Err(format!(
                    "the submodule {path} is not initialized at its gitlink {} in this checkout, \
                     and the stage writes it from there (git submodule update --init {path})",
                    short(&rev)
                ));
            }
            let dest = root.join(&path);
            let _ = std::fs::remove_dir_all(&dest);
            let (sub_s, dest_s) = (sub.display().to_string(), dest.display().to_string());
            if !clone(&sub_s, &dest_s) || !git(&dest, &["checkout", "--quiet", "--detach", &rev]) {
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
    /// its caller synced, with this run's scanner tools, then the engine synced again. In step
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
        scrub_fixture_command(cmd);
        cmd.env("TRUST_STAGE2_BIN", &prover.bin);
        let (out, record) = self.output_accounted(step, cmd)?;
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
        let mut run = |cmd: &mut Command| self.captured(step, prover, log, cmd);
        crate::fixtures::stage(
            &prover.bin.join("targo"),
            &self.repo,
            &root,
            &pins,
            &mut run,
        )
        .map(|staged| staged.words(&pins))
        .map_err(|e| {
            let text = e.to_string();
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
        })
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
        let mut cmd = self.child(&frozen.verify.path, Some(prover));
        self.hand_pool(&mut cmd, true)?;
        cmd.arg("release").arg(run.half.flag());
        if run.strict_subset() {
            cmd.arg("--checks").arg(run.checks.join(","));
        }
        if !run.passed_binaries.is_empty() {
            let path = side("passed-binaries");
            let mut text = String::new();
            for (spec, sha256) in &run.passed_binaries {
                text.push_str(sha256);
                text.push(' ');
                text.push_str(spec);
                text.push('\n');
            }
            crate::durable::write_atomic_durable(&path, text.as_bytes(), 0o600)
                .map_err(|e| cannot(&path, e))?;
            cmd.arg("--passed-binaries").arg(&path);
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
        stop: &BranchStop,
    ) -> Built {
        let log = self.side_log(Step::Qualify, Branch::Build.name());
        let artifact = work
            .bundle
            .then(|| self.build_run(c, reserved, prover, &log, stop));
        let built = artifact.as_ref().is_none_or(Result::is_ok);
        let fixtures = (work.fixtures && built && !stop.fired()).then(|| {
            let log = self.side_log(Step::Qualify, "build-fixtures");
            self.stage_fixtures_run(Step::Qualify, c, reserved, prover, &log)
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
        let served = self.output_forwarded(command(aterm_uds::os_path!("curl")).args([
            "-fsSL",
            "--proto-redir",
            "=https",
            "--max-time",
            "60",
            "--",
            &want,
        ]));
        let text = match served {
            Ok(o) if o.status.success() => String::from_utf8_lossy(&o.stdout).into_owned(),
            Ok(o) => {
                return Err(format!(
                    "the appcast at {want} could not be fetched anonymously ({})",
                    String::from_utf8_lossy(&o.stderr).trim()
                ));
            }
            Err(e) => return Err(format!("cannot run curl: {e}")),
        };
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

    fn site(&mut self, slug: &str) -> Result<AfterCut, Refusal> {
        if interrupt::caught().is_some() {
            return Err(Refusal::new(
                "not started: the release was interrupted",
                format!("{RELEASE_COMMAND} again"),
            ));
        }
        let log = self.log_path(Step::AfterCut);
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
/// ([`crate::build_inputs::SEAL_ENV`]), which a child has only from [`hand_inputs_seal`], and a
/// disk claim it may have been named ([`aterm_capacity::DISK_COVER_ENV`]), which a child has
/// only from this release's own claim ([`Real::scrubbed`]).
fn scrub_names(cmd: &mut Command, inherited: impl IntoIterator<Item = std::ffi::OsString>) {
    for name in inherited {
        if let Some(name) = name.to_str()
            && (clears(name)
                || launcher_env(name)
                || name == RELEASE_CUTTER_ENV
                || name == RELEASE_CUTTER_SINCE_ENV
                || name == PUBLISH_GATE_ENV
                || name == crate::build_inputs::SEAL_ENV
                || name == aterm_capacity::DISK_COVER_ENV)
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
    let text = crate::durable::read_text(path).unwrap_or_default();
    let mut out: Vec<String> = text.lines().rev().take(n).map(str::to_string).collect();
    out.reverse();
    out
}

/// What to do about the release's own engine `e` in a state no release leaves it in: nothing
/// but a release writes it, so whatever made the state is read (`git -C e <read>`) and kept where
/// it is wanted before the copy goes; the next release clones it again.
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
    Refusal::new(
        format!("signed publication prerequisites: {why}"),
        crate::managed_prerequisites::REMEDY,
    )
    .could_not_run()
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
            Refusal::new(
                why,
                "fix the SOURCE, never the export — patterns: the engine's \
                 baseline/forbidden-content.txt + publish/forbidden-extra.txt",
            )
            .detail(
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

impl Backend for Real {
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

    fn fleet_line(&mut self, line: &Line, continued: bool) -> Result<Line, String> {
        let claim = crate::line_claim::Claim::take(&self.repo, line, &self.logs, continued)?;
        let selected = claim.line.clone();
        self.fleet_line = Some(claim);
        Ok(selected)
    }

    fn fleet_existing(&mut self, version: &str) -> Result<Option<Line>, String> {
        let line = crate::line_claim::existing(&self.repo, version)?;
        if let Some(line) = &line {
            self.fetch().map_err(|r| r.why)?;
            crate::line::verify(&self.git(), line).map_err(|e| e.to_string())?;
        }
        Ok(line)
    }

    fn fleet_boundary(&mut self, line: &Line, step: Step) -> Result<(), String> {
        let history = self.times();
        if let Some(claim) = &mut self.fleet_line {
            claim.boundary(line, step.name(), &history)?;
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
        self.fetch_origins(|repo| {
            crate::github_remote::Remote::origin(repo).and_then(|remote| remote.fetch_origin())
        })
    }

    fn head(&mut self) -> String {
        self.git_out(&["rev-parse", "HEAD"]).unwrap_or_default()
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
        // recorded as the origin's: a publish whose ledger push failed published its commit.
        let copy = crate::gates::engine_copy(&self.release);
        let text = crate::gates::unpushed_ledger(&copy, &|cmd| self.output_forwarded(cmd))
            .map_or_else(
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
        let (code, out) = self.engine_git_code(&[
            "diff",
            "--quiet",
            from,
            to,
            "--",
            ".",
            ":(exclude)verification-log.jsonl",
            ":(exclude)mappings.json",
            ":(exclude)handoff/",
        ])?;
        match code {
            0 => Ok(EngineNow::Same(format!(
                "the engine at its origin/main {} has the code that staged ({}; only its data \
                 plane moved)",
                short(to),
                short(from)
            ))),
            1 => Ok(EngineNow::Moved(format!(
                "the engine's code moved since it staged: {}..{} (its origin/main)",
                short(from),
                short(to)
            ))),
            _ => Err(format!(
                "git -C {e} diff {}..{} exited {code}: {}",
                short(from),
                short(to),
                String::from_utf8_lossy(&out.stderr).trim()
            )),
        }
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
        let git = self.git();
        crate::line::git_ready(&git)?;
        Ok(crate::line::admit(&git, line, rev)?)
    }

    fn carry(&mut self, line: &Line, fixes: &[Fix]) -> Result<(Vec<Hop>, String), Refusal> {
        let git = self.git();
        crate::line::git_ready(&git)?;
        let fixes = crate::line::order(&git, fixes.to_vec())?;
        let hops = crate::line::carry(&git, line, &fixes)?;
        let head = hops.last().map_or(line.head(), |hop| hop.commit.as_str());
        let named: Vec<&str> = fixes.iter().map(|fix| short(&fix.commit)).collect();
        // A hop is the same commit however often it is made, so naming the fixes again makes
        // these hops and records them; a bare re-run resumes the journal, which names none yet.
        let recorded = crate::line::record(
            &git,
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
        Ok((hops, note))
    }

    fn verify_line(&mut self, line: &Line) -> Result<String, Refusal> {
        Ok(crate::line::verify(&self.git(), line)?)
    }

    fn sync(&mut self) -> Result<String, Refusal> {
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
        for attempt in 1..=3 {
            let tip = self
                .git_out(&["rev-parse", "origin/main"])
                .unwrap_or_default();
            self.main_to(&tip)?;
            self.submodule_sync(Step::Commit)?;
            let cur = self.version_at("HEAD").unwrap_or_default();
            if cur != from {
                return Err(Refusal::new(
                    format!(
                        "origin/main's version moved to {cur} while this release bumped {from}"
                    ),
                    format!("{RELEASE_COMMAND} again (without --bump when {cur} is unreleased)"),
                ));
            }
            let mut cmd = self.scrubbed(&edit);
            cmd.arg(&self.repo).args([to, &cur]).current_dir(&self.repo);
            if self.logged(Step::Commit, &mut cmd).code != 0 {
                return Err(Refusal::new(
                    "bin/pub-bump-edit could not find the version line",
                    "read Cargo.toml's [workspace.package]",
                ));
            }
            let targo = prover.bin.join("targo");
            let mut offline = self.child(&targo, Some(prover));
            offline.args(["update", "--workspace", "--offline", "-q"]);
            if self.logged(Step::Commit, &mut offline).code != 0 {
                let mut online = self.child(&targo, Some(prover));
                online.args(["update", "--workspace", "-q"]);
                if self.logged(Step::Commit, &mut online).code != 0 {
                    undo(self);
                    return Err(Refusal::new(
                        "the lockfile refresh failed (targo update --workspace); the bump is undone",
                        "read the commit step's log",
                    ));
                }
            }
            // A nested workspace (a recorder run read-only in the Linux lane)
            // locks aterm's path crates at the workspace's version too: each
            // moves with the root, or its `--locked` run fails until relocked.
            let nested = self.git_paths(&["ls-files", "-z", "--", "*/Cargo.lock"]);
            let followed = match bump_nested_locks(&self.repo, nested, from, to) {
                Ok(followed) => followed,
                Err(why) => {
                    undo(self);
                    return Err(Refusal::new(why, "read the commit step's log"));
                }
            };
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
            let owed = match crate::harness_versions::apply(&self.repo) {
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
            let mut commit = command(aterm_uds::os_path!("git"));
            commit
                .args(["commit", "-q", "-m", &msg, "--", "Cargo.toml", "Cargo.lock"])
                .args(&followed)
                .args(crate::harness_versions::written(&owed))
                .current_dir(&self.repo);
            if self.logged(Step::Commit, &mut commit).code != 0 {
                undo(self);
                return Err(Refusal::new(
                    "could not commit the bump; it is undone",
                    "read the commit step's log",
                ));
            }
            let mut push = command(aterm_uds::os_path!("git"));
            push.args(["push", "-q", "origin", "HEAD:main"])
                .current_dir(&self.repo);
            if self.logged(Step::Commit, &mut push).code == 0 {
                return Ok(self.head());
            }
            if attempt == 3 {
                return Err(Refusal::new(
                    "main moved under the bump three times (each push lost the race)",
                    format!("{RELEASE_COMMAND} --bump again"),
                ));
            }
            let _ = self.remote_git(&["fetch", "-q", "origin", "main"]);
            let _ = self.git().git(&["reset", "-q", "--keep", "origin/main"]);
        }
        Err(Refusal::new(
            "the bump did not land",
            "read the commit step's log",
        ))
    }

    fn validation_tools(&mut self) -> Result<String, Refusal> {
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

    fn own_cutter(&mut self, commit: &str) -> Result<String, crate::cutter::NotOwn> {
        crate::gates::own_frozen_cutter(commit)
    }

    fn exec_cutter(&mut self, commit: &str, prover: &Prover) -> Result<(), Refusal> {
        let cannot = |why: String| {
            Refusal::new(
                format!("cannot exec R's frozen cutter ({why})"),
                format!("run {RELEASE_COMMAND} again from the clone"),
            )
        };
        if interrupt::caught().is_some() {
            return Err(cannot("not started: the release was interrupted".into()));
        }
        let tools = self.frozen_tools(commit, prover).map_err(cannot)?;
        let mut command = self.scrubbed(&tools.cutter.path);
        command.args(&self.argv).current_dir(&self.repo).env(
            crate::release_tools::RECORD_ENV,
            crate::release_tools::record_path(&self.release),
        );
        for name in std::env::vars_os().map(|(name, _)| name).filter(|name| {
            name.to_str()
                .is_some_and(|name| name.starts_with("TRUST_TARGO_"))
        }) {
            command.env_remove(name);
        }
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
        let error = command.exec();
        Err(cannot(error.to_string()))
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
                };
                crate::check_record::reuse_product(&root, &facts, &product)
            })();
            if matches!(reused, Ok(true)) {
                println!(
                    "    measure: product U, linked harnesses and seal unchanged; using recorded verdict"
                );
            }
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
                if dry && c.bump_from.is_some() {
                    crate::gates::harness_versions_bump_writes_at(&git, commit, version)
                } else {
                    crate::gates::harness_versions_gate_at(&git, commit, version)
                }
                .map(|_| ()),
            ),
            (
                "handoff fixtures",
                crate::gates::handoff_fixture_gate_at(&git, commit, version).map(|_| ()),
            ),
            (
                "handoff policy",
                crate::gates::handoff_policy_gate_at(&git, commit, version).map(|_| ()),
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
        let (commit, version) = (c.commit.as_str(), c.version.as_str());
        if let Err(e) = crate::gates::handoff_fixture_gate_at(&git, commit, version) {
            found.push(cut_refusal("handoff fixtures", &e.to_string()));
        }
        if let Err(e) = crate::gates::handoff_policy_gate_at(&git, commit, version) {
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
        let pins = fixture_pins(c, reserved);
        let again = || {
            format!(
                "run {RELEASE_COMMAND} again, which lands them on the tip it finds (or add them \
                 by hand: {}, \"By hand\")",
                crate::gates::HANDOFF_FIXTURE_README
            )
        };
        let git = self.git();
        let fetched = crate::ledger::git_ok(&git, &["fetch", "origin", "main"]);
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
                let log = self.step_log(self.step);
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
        let log = self.step_log(self.step);
        let landed = crate::fixtures::land(
            &git,
            &staged,
            &pins,
            crate::ledger::MAX_CLAIM_ATTEMPTS,
            &mut |preview| match &mut reader {
                Ok(reader) => reader.read(&targo, preview, &pins, &mut |cmd| {
                    self.captured(self.step, prover, &log, cmd)
                }),
                Err(why) => crate::fixtures::Reading {
                    answers: Err(why.to_string()),
                    checks: format!("COULD NOT RUN: {why}"),
                },
            },
        )
        .map_err(|e| Refusal::new(e.to_string(), again()))?;
        words.push(landed.words(&pins));
        Ok(words.join("; "))
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
        crate::ledger::reserve(&self.git(), &plan, &|source, main| {
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
    fn cut(&mut self, args: &[String], prover: &Prover) -> Ran {
        match self.cut_command(args, prover) {
            Ok(mut cmd) => self.logged(Step::Cut, &mut cmd),
            Err(why) => Ran {
                code: -1,
                detail: vec![format!("not started: {why}")],
            },
        }
    }

    fn after_cut(&mut self, version: &str) -> Result<AfterCut, Refusal> {
        let slug = self.slug();
        let tag = format!("v{version}");
        let read = |s: &Self, t: &str| s.release_json(&slug, t);
        let mut problems = match read(self, &tag) {
            Ok(rel) => release_shape(&rel, version),
            Err(why) => {
                return Err(Refusal::new(
                    why,
                    format!("{RELEASE_COMMAND} again when GitHub answers"),
                )
                .could_not_run());
            }
        };
        problems.extend(pointer_verdict(
            self.pointer_problems(&slug, &tag, version),
        )?);
        let site_tag = match self.site_tag(&slug) {
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
            match read(self, &site_tag) {
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
        if !problems.is_empty() {
            return Err(shape_refusal(&slug, &tag, problems));
        }
        let site = self.site(&slug)?;
        let tap = self.align_tap(&site_tag)?;
        Ok(AfterCut {
            note: format!(
                "{tag} carries the lean shape as GitHub serves it; {}; {tap}",
                site.note,
            ),
        })
    }

    fn latest(&mut self) -> Result<String, String> {
        let slug = self.slug();
        let release = self.github_read(&format!("repos/{slug}/releases/latest"))?;
        release["tag_name"]
            .as_str()
            .map(str::to_string)
            .ok_or_else(|| format!("{slug}'s latest release names no tag_name"))
    }
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

/// Run a release in the clone at `repo`; the exit code.
#[must_use]
pub fn run(repo: &Path, opts: &crate::release::ReleaseOptions) -> i32 {
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
    let retired_deps = if matches!(
        crate::cutter::running_stamp(),
        Ok(crate::cutter::Stamp::Proved(_))
    ) {
        match crate::release_target::retire(repo) {
            Ok(entries) => Some(entries),
            Err(why) => {
                eprintln!("release target retirement: {why}");
                return 2;
            }
        }
    } else {
        None
    };
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
    if let Some(entries) = retired_deps {
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
    real.line_priority = line;
    real.argv = opts.exec_args(std::env::args_os().skip(1).collect());
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
        if opts.source {
            crate::gates::precheck_source(&git, rev)
        } else {
            crate::gates::precheck(&git, rev)
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
    let mut version_line: Option<usize> = None;
    let mut sourced = false;
    for i in 0..=lines.len() {
        let line = lines.get(i).map_or("", |l| l.trim_end());
        if i == lines.len() || line.starts_with('[') {
            if let (Some(at), false) = (version_line.take(), sourced) {
                let ending = &lines[at][lines[at].trim_end().len()..];
                lines[at] = std::borrow::Cow::Owned(format!("version = \"{to}\"{ending}"));
                moved = true;
            }
            sourced = false;
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
mod tests {
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

        // Asked for by a spelling no child is handed: a `.` component, and a doubled separator
        // too where `TMPDIR` holds one (as `$TMPDIR/.ubd/` does where `$TMPDIR` ends in a
        // separator).
        let path = dir.path().join(".").join("qualify.jobs");
        let pool = qualify_pool(&path, true, |need| {
            aterm_capacity::admit(need, aterm_capacity::Admit::Now)
        })
        .unwrap();
        let (slots, export) = (pool.jobs.slots(), pool.export);
        assert_eq!(export, export_share(slots, true));
        let fifo = pool.jobs.path().as_os_str().to_owned();
        assert_eq!(
            Path::new(&fifo),
            path,
            "the pool's FIFO is the one asked for"
        );
        real.pool = Some(pool);

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
        let log = std::fs::read_to_string(real.step_log(Step::AfterCut)).unwrap();
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
            .args(["-c", &alias, "fd-proof"])
            .output()
            .unwrap();
        assert!(
            out.status.success(),
            "object git inherited fd {fd}: {}",
            String::from_utf8_lossy(&out.stderr)
        );
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
            git(&engine, &["push", "-q", "--force", "origin", "main"]);
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
            let found = checked_with(&repo, &opts(commit, &engine), |real| {
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
                vec!["push", "-q", "origin", "main"],
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
        let lf = |tree: &Path, notes: &str, with_sub: bool| {
            let read = |rel: &str| fs::read_to_string(tree.join(rel)).unwrap();
            assert_eq!(read("bin/pub"), "#!/bin/sh\n", "{}", tree.display());
            assert_eq!(read("notes.txt"), notes, "{}", tree.display());
            if with_sub {
                assert_eq!(read("vendor/sub/s.sh"), "#!/bin/sh\n", "{}", tree.display());
            }
        };
        let real = Real::new(&repo, Some(&repo), true, false).unwrap();
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
        let place = |commit: &str| {
            let source = crate::gates::PublishedSource {
                commit: commit.to_string(),
                verified_at: "2026-10-21T00:00:00+00:00".to_string(),
            };
            crate::gates::place_published(&GitCli::new(&repo), &repo, &source).unwrap()
        };
        lf(&place(&r).tree, "one\n", true);
        write_crlf_fixture_file(&repo.join("notes.txt"), "one\ntwo\n");
        run_crlf_git(&repo, &["commit", "-q", "-am", "two"]);
        run_crlf_git(&repo, &["push", "-q", "origin", "main"]);
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
        let reserved = crate::ledger::reserve(&cut, &plan, &rolls).unwrap();
        crate::ledger::claim(&cut, &moved.tree, &plan, &reserved, &rolls).unwrap();
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
        // A real run's backend (a dry one makes no engine to stage from), its logs its own.
        let backend = |engine: &Path, logs: &str| {
            let mut real = Real::new(&repo, Some(engine), false, false).unwrap();
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
            if mode == "control" {
                // SAFETY: SIG_DFL is a disposition, no handler of ours.
                unsafe {
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
            let bound = std::time::Instant::now() + std::time::Duration::from_secs(60);
            while interrupt::caught().is_none() {
                assert!(std::time::Instant::now() < bound, "SIGTERM never caught");
                std::thread::sleep(std::time::Duration::from_millis(10));
            }
            println!("PROBE term={}", interrupt::caught().unwrap_or(0));
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
            let out = cmd
                .args([
                    "--exact",
                    "release_steps::tests::an_inherited_sig_ign_is_kept_and_the_others_are_caught",
                    "--nocapture",
                    "--test-threads=1",
                ])
                .env(PROBE, mode)
                .stdin(Stdio::null())
                .output()
                .unwrap();
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
    }
}
