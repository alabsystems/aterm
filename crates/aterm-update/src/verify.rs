// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0

//! Authenticity verification of a candidate `.app` at stage time and apply time.

use aterm_uds::deadline::RunError;
use aterm_uds::spawnfd::Command;
use std::path::Path;
use std::time::{Duration, Instant};

#[cfg(not(test))]
const HELPER_TIMEOUT: Duration = Duration::from_secs(30);
/// A unit test's helper is a real program that finishes, so one a loaded machine slows is waited
/// for to the hang detector; a test that needs the ceiling to fire opens an [`ApplyBudget`].
#[cfg(test)]
const HELPER_TIMEOUT: Duration = aterm_spec::wait::MUST_HAPPEN;

const HELPER_REAP: Duration = Duration::from_millis(250);

/// The AGGREGATE ceiling on one apply's verification work — see [`ApplyBudget`].
pub const APPLY_BUDGET: Duration = Duration::from_secs(8);

thread_local! {
    static APPLY_DEADLINE: std::cell::Cell<Option<Instant>> =
        const { std::cell::Cell::new(None) };
}

/// Opens an aggregate verification budget for the current thread.
pub struct ApplyBudget(Option<Instant>);

impl ApplyBudget {
    /// Start a budget of `limit`, replacing any budget already in force.
    pub fn start(limit: Duration) -> Self {
        Self(APPLY_DEADLINE.with(|d| d.replace(Some(Instant::now() + limit))))
    }

    /// Push the deadline out by `elapsed`.
    pub fn extend(elapsed: Duration) {
        APPLY_DEADLINE.with(|d| {
            if let Some(deadline) = d.get() {
                d.set(Some(deadline + elapsed));
            }
        });
    }
}

/// The deadline this thread's verification must finish by, or `None` outside a budget.
#[cfg(test)]
pub(crate) fn current_budget_deadline() -> Option<Instant> {
    APPLY_DEADLINE.with(std::cell::Cell::get)
}

impl Drop for ApplyBudget {
    fn drop(&mut self) {
        APPLY_DEADLINE.with(|d| d.set(self.0));
    }
}

const STDERR_KEEP: usize = 512;

/// Failure of an owned helper, including whether its absence was actually observed.
#[derive(Debug)]
pub(crate) struct HelperFailure {
    pub message: String,
    pub writer_stopped: bool,
}

trait HelperChild {
    fn request_kill(&mut self) -> std::io::Result<()>;
    fn observe_exit(&mut self) -> std::io::Result<Option<std::process::ExitStatus>>;
}

impl HelperChild for aterm_uds::spawnfd::Child {
    fn request_kill(&mut self) -> std::io::Result<()> {
        self.kill()
    }
    fn observe_exit(&mut self) -> std::io::Result<Option<std::process::ExitStatus>> {
        self.try_wait()
    }
}

fn stop_helper_before(child: &mut impl HelperChild, deadline: Instant) -> bool {
    let _ = child.request_kill();
    loop {
        match child.observe_exit() {
            Ok(Some(_)) => return true,
            Err(_) => return false,
            Ok(None) => {
                let remaining = deadline.saturating_duration_since(Instant::now());
                if remaining.is_zero() {
                    return false;
                }
                std::thread::sleep(Duration::from_millis(5).min(remaining));
            }
        }
    }
}

/// Bounded run of a child whose stdout is worth nothing but whose STDERR is.
pub(crate) fn status_bounded_with_stderr_observed(
    cmd: &mut Command,
    what: &str,
    limit: Duration,
) -> Result<(std::process::ExitStatus, String), HelperFailure> {
    use aterm_uds::spawnfd::Stdio;
    use std::io::Read;
    let deadline = Instant::now() + limit;
    let cleanup_reserve = Duration::from_millis(250).min(limit / 4);
    let work_deadline = deadline - cleanup_reserve;
    let cleanup_deadline = || deadline.min(Instant::now() + cleanup_reserve);
    let mut child = cmd
        .stdin(Stdio::null())
        .stderr(Stdio::piped())
        .spawn()
        .map_err(|error| HelperFailure {
            message: spawn_refusal(what, &error),
            writer_stopped: true,
        })?;
    let drain = if let Some(mut stream) = child.stderr.take() {
        let (tx, rx) = std::sync::mpsc::sync_channel(1);
        let spawned = std::thread::Builder::new()
            .name("update-helper-stderr".to_string())
            .spawn(move || {
                let mut tail: Vec<u8> = Vec::new();
                let mut chunk = [0u8; 4096];
                while let Ok(n) = stream.read(&mut chunk) {
                    if n == 0 {
                        break;
                    }
                    tail.extend_from_slice(&chunk[..n]);
                    if tail.len() > STDERR_KEEP {
                        let cut = tail.len() - STDERR_KEEP;
                        tail.drain(..cut);
                    }
                }
                let _ = tx.send(String::from_utf8_lossy(&tail).trim().to_string());
            });
        if let Err(error) = spawned {
            return Err(HelperFailure {
                message: spawn_refusal(&format!("{what} stderr drain"), &error),
                writer_stopped: stop_helper_before(&mut child, cleanup_deadline()),
            });
        }
        Some(rx)
    } else {
        None
    };
    let mut exit = aterm_uds::deadline::ExitWait::new(&child);
    let status = loop {
        let message = match child.try_wait() {
            Ok(Some(status)) => break status,
            Ok(None) => {
                if Instant::now() < work_deadline {
                    exit.wait(work_deadline.saturating_duration_since(Instant::now()));
                    continue;
                }
                mark_passing(format!(
                    "{what} did not finish within {}s; treating as a failure",
                    limit.as_secs()
                ))
            }
            Err(error) => format!("wait for {what}: {error}"),
        };
        return Err(HelperFailure {
            message,
            writer_stopped: stop_helper_before(&mut child, cleanup_deadline()),
        });
    };
    let stderr = match drain {
        Some(rx) => rx
            .recv_timeout(deadline.saturating_duration_since(Instant::now()))
            .map_err(|error| HelperFailure {
                message: format!("{what}: stderr did not finish within the helper budget: {error}"),
                writer_stopped: true,
            })?,
        None => String::new(),
    };
    Ok((status, stderr))
}

/// The last non-empty line of a child's stderr, for an error message.
pub(crate) fn last_stderr_line(stderr: &str) -> Option<&str> {
    stderr.lines().rev().map(str::trim).find(|l| !l.is_empty())
}

/// Prove the candidate can actually start, by starting it.
#[cfg(target_os = "macos")]
pub fn probe_bundle_starts(app: &Path, expected_build: u64) -> Result<(), String> {
    probe_starts(app, expected_build, |exe| {
        output_bounded(
            aterm_uds::spawnfd::command(exe).arg("--version"),
            START_PROBE,
        )
    })
}

/// [`probe_bundle_starts`] at stage time, on the checker thread, before the ready marker.
#[cfg(target_os = "macos")]
pub(crate) fn probe_staged_bundle_starts(app: &Path, expected_build: u64) -> Result<(), String> {
    #[cfg(test)]
    if let Some(errno) = STAGE_PROBE_SPAWN_REFUSAL.with(std::cell::Cell::take) {
        return Err(spawn_refusal(
            START_PROBE,
            &std::io::Error::from_raw_os_error(errno),
        ));
    }
    #[cfg(test)]
    if STAGE_PROBE_TIMED_OUT.with(std::cell::Cell::take) {
        return Err(timed_out(START_PROBE, false));
    }
    probe_bundle_starts(app, expected_build)
}

#[cfg(all(test, target_os = "macos"))]
thread_local! {
    /// The `errno` the next stage probe on this thread is refused its spawn with.
    pub(crate) static STAGE_PROBE_SPAWN_REFUSAL: std::cell::Cell<Option<i32>> =
        const { std::cell::Cell::new(None) };
    /// Whether the next stage probe on this thread reports a helper timeout ([`timed_out`]).
    pub(crate) static STAGE_PROBE_TIMED_OUT: std::cell::Cell<bool> =
        const { std::cell::Cell::new(false) };
}

/// What the start probe is called in its refusals.
#[cfg(target_os = "macos")]
const START_PROBE: &str = "candidate --version probe";

/// How the refusal of a start probe whose candidate a signal ended begins: a verdict on the
/// build whose candidate nonetheless stays where it ran, since no exit of its own showed
/// macOS done assessing its first exec (`install::probe_left_unproven`).
pub(crate) const START_PROBE_SIGNALLED: &str =
    "the staged bundle was ended by a signal before it exited";

/// Non-macOS: there is no bundle to start.
#[cfg(not(target_os = "macos"))]
pub(crate) fn probe_staged_bundle_starts(_app: &Path, _expected_build: u64) -> Result<(), String> {
    Ok(())
}

/// Run `app`'s executable with `--version` through `run` (which returns its output)
/// and require a clean exit naming `expected_build`.
#[cfg(target_os = "macos")]
fn probe_starts(
    app: &Path,
    expected_build: u64,
    run: impl FnOnce(&Path) -> Result<std::process::Output, String>,
) -> Result<(), String> {
    let exe = app.join("Contents/MacOS/aterm");
    if !exe.is_file() {
        return Err(format!("no executable at {}", exe.display()));
    }
    let out = run(&exe)?;
    // A signal is how a bundle that cannot start ends (a dyld abort, a code-signing kill),
    // and it may land at `_dyld_start` while macOS still assesses the first exec: a verdict
    // whose candidate the callers keep where it ran.
    if out.status.code().is_none() {
        return Err(format!(
            "{START_PROBE_SIGNALLED} ({}); {} stays where it ran while macOS may still be \
             assessing its first exec: {}",
            out.status,
            exe.display(),
            crate::quote_tool_output(String::from_utf8_lossy(&out.stderr).trim())
        ));
    }
    if !out.status.success() {
        return Err(format!(
            "the staged bundle could not start ({}): {}",
            out.status,
            crate::quote_tool_output(String::from_utf8_lossy(&out.stderr).trim())
        ));
    }
    let text = String::from_utf8_lossy(&out.stdout);
    if !text.contains(&expected_build.to_string())
        && bundle_build_number(app).ok() != Some(expected_build)
    {
        return Err(format!(
            "the staged bundle started but does not report build {expected_build}: {}",
            crate::quote_tool_output(text.trim())
        ));
    }
    Ok(())
}

/// Non-macOS: there is no bundle to start.
#[cfg(not(target_os = "macos"))]
pub fn probe_bundle_starts(_app: &Path, _expected_build: u64) -> Result<(), String> {
    Ok(())
}

fn helper_deadline() -> (Instant, bool) {
    let own = Instant::now() + HELPER_TIMEOUT;
    match APPLY_DEADLINE.with(|d| d.get()) {
        Some(b) if b < own => (b, true),
        _ => (own, false),
    }
}

/// The rejection a helper that outran [`helper_deadline`] reports.
pub(crate) fn timed_out(what: &str, bound_by_budget: bool) -> String {
    if bound_by_budget {
        mark_passing(format!(
            "{what} ran past this apply's verification budget; treating as a rejection"
        ))
    } else {
        mark_passing(format!(
            "{what} did not finish within {}s; treating as a rejection",
            HELPER_TIMEOUT.as_secs()
        ))
    }
}

/// `message`, marked as a passing refusal ([`crate::PASSING_REFUSAL_KEY`]).
pub(crate) fn mark_passing(message: String) -> String {
    format!("{message} ({})", crate::PASSING_REFUSAL_KEY)
}

/// Whether an I/O error from starting a helper, or from waiting on a lock, is a fact about this
/// moment rather than about the bundle or the machine's setup.
pub(crate) fn io_error_is_passing(error: &std::io::Error) -> bool {
    matches!(
        error.kind(),
        std::io::ErrorKind::TimedOut
            | std::io::ErrorKind::WouldBlock
            | std::io::ErrorKind::OutOfMemory
    ) || matches!(
        error.raw_os_error(),
        Some(libc::EAGAIN | libc::ENOMEM | libc::EMFILE | libc::ENFILE)
    )
}

/// The refusal for an I/O error while starting helper `what`.
pub(crate) fn spawn_refusal(what: &str, error: &std::io::Error) -> String {
    let message = format!("spawn {what}: {error}");
    if io_error_is_passing(error) {
        mark_passing(message)
    } else {
        message
    }
}

/// The refusal a bounded wait on the apply lock reports, under `what`.
pub(crate) fn lock_wait_refusal(what: &str, error: &std::io::Error) -> String {
    let message = format!("{what}: {error}");
    if io_error_is_passing(error) {
        mark_passing(message)
    } else {
        message
    }
}

fn output_bounded(cmd: &mut Command, what: &str) -> Result<std::process::Output, String> {
    output_until(cmd, what, || {
        let (deadline, bound_by_budget) = helper_deadline();
        (deadline, timed_out(what, bound_by_budget))
    })
}

/// [`output_bounded`] with an explicit `limit` instead of the helper timeout and the apply budget.
pub(crate) fn output_within(
    cmd: &mut Command,
    what: &str,
    limit: Duration,
) -> Result<std::process::Output, String> {
    output_until(cmd, what, || {
        (
            Instant::now() + limit,
            mark_passing(format!(
                "{what} did not finish within {}s; treating as a failure",
                limit.as_secs()
            )),
        )
    })
}

fn output_until(
    cmd: &mut Command,
    what: &str,
    bound: impl FnOnce() -> (Instant, String),
) -> Result<std::process::Output, String> {
    let (deadline, timeout) = bound();
    aterm_uds::deadline::outputs_until(&mut [cmd], deadline, HELPER_REAP)
        .pop()
        .unwrap_or(Err(RunError::TimedOut))
        .map_err(|error| run_refusal(what, error, timeout))
}

type Run = Result<std::process::Output, RunError>;

fn run_refusal(what: &str, error: RunError, timeout: String) -> String {
    match error {
        RunError::Spawn(e) => spawn_refusal(what, &e),
        RunError::Wait(e) => mark_passing(format!("wait for {what}: {e}")),
        RunError::TimedOut => timeout,
    }
}

/// A bundle's policy gate and its sealed reads, with every helper started at once.
#[derive(Debug)]
pub struct BundleChecks {
    /// [`verify_bundle_policy`]'s verdict.
    pub policy: Result<(), String>,
    /// [`bundle_build_number`]'s answer.
    pub build: Result<u64, String>,
    /// The sealed `ATermGitCommit` ([`git_commit_from`]).
    pub commit: Result<String, String>,
    /// [`team_id`]'s answer when a Team ID is pinned (the gate reads it).
    pub team: Option<Result<String, String>>,
    /// [`bundle_short_version`]'s answer, when asked for.
    pub short_version: Option<Result<String, String>>,
}

/// [`BundleChecks`] of `app` under `expected_team` (the tier [`verify_bundle_policy`] takes).
pub fn check_bundle(app: &Path, expected_team: &str, short_version: bool) -> BundleChecks {
    check_bundles(&[app], expected_team, short_version)
        .pop()
        .unwrap_or_else(|| BundleChecks {
            policy: not_run(),
            build: not_run(),
            commit: not_run(),
            team: None,
            short_version: short_version.then(not_run),
        })
}

/// [`check_bundle`] of every one of `apps`, in order.
pub fn check_bundles(
    apps: &[&Path],
    expected_team: &str,
    short_version: bool,
) -> Vec<BundleChecks> {
    const BUILD: &str = "CFBundleVersion";
    const COMMIT: &str = "ATermGitCommit";
    const VERSION: &str = "CFBundleShortVersionString";
    let pinned = (!expected_team.is_empty()).then_some(expected_team);
    let keys: &[&str] = if short_version {
        &[BUILD, COMMIT, VERSION]
    } else {
        &[BUILD, COMMIT]
    };
    gates_and_reads(apps, pinned, keys)
        .into_iter()
        .map(|gate| {
            let mut reads = gate.reads.into_iter();
            let mut next = || {
                reads
                    .next()
                    .unwrap_or_else(|| Err("the read did not run".to_string()))
            };
            BundleChecks {
                policy: gate.policy,
                build: next().and_then(build_number_from),
                commit: next().and_then(git_commit_from),
                team: gate.team,
                short_version: short_version.then(|| next().and_then(short_version_from)),
            }
        })
        .collect()
}

fn not_run<T>() -> Result<T, String> {
    Err("the bundle check did not run".to_string())
}

struct Gate {
    policy: Result<(), String>,
    team: Option<Result<String, String>>,
    /// Each key's `PlistBuddy` output, in `keys`' order.
    reads: Vec<Result<std::process::Output, String>>,
}

struct GateHelpers {
    seal: Command,
    display: Option<Command>,
    spctl: Option<Command>,
    plist: Vec<Command>,
}

impl GateHelpers {
    fn of(
        app: &Path,
        pinned: Option<&str>,
        keys: &[&str],
        deadline: Instant,
    ) -> Result<Self, Gate> {
        use aterm_update_core::codesign::developer_id_command;
        let refused = |refusal: String| Gate {
            policy: Err(refusal.clone()),
            team: None,
            reads: keys.iter().map(|_| Err(refusal.clone())).collect(),
        };
        let seal = match pinned {
            Some(team) => developer_id_command(app, team, true)
                .map_err(|error| refused(codesign_refusal(error, app, false)))?,
            None => structural_command(app),
        };
        if let Some(refusal) = unanswerable_entry(app, deadline) {
            return Err(refused(refusal));
        }
        Ok(Self {
            seal,
            display: pinned.map(|_| display_command(app)),
            spctl: pinned.map(|_| spctl_command(app)),
            plist: keys.iter().map(|key| plist_command(app, key)).collect(),
        })
    }

    fn commands(&mut self) -> impl Iterator<Item = &mut Command> {
        std::iter::once(&mut self.seal)
            .chain(self.display.as_mut())
            .chain(self.spctl.as_mut())
            .chain(self.plist.iter_mut())
    }
}

fn unanswerable_entry(app: &Path, deadline: Instant) -> Option<String> {
    const SPECIAL: &str = "not a directory, a file or a symlink to one";
    const OUTSIDE: &str = "a symlink to a directory outside the bundle";
    let special = |kind: std::fs::FileType| !(kind.is_dir() || kind.is_file());
    let refusal = |entry: &Path, what: &str| {
        format!(
            "refusing {}: it holds {} — {what}, which no signed bundle holds and codesign \
             can wait on for good",
            app.display(),
            crate::quote_tool_output(&format!("{:?}", entry.display().to_string()))
        )
    };
    let root = std::fs::canonicalize(app).ok()?;
    if std::fs::metadata(&root).is_ok_and(|metadata| special(metadata.file_type())) {
        return Some(refusal(Path::new("."), SPECIAL));
    }
    let mut dirs = vec![root.clone()];
    while let Some(dir) = dirs.pop() {
        if Instant::now() >= deadline {
            return None;
        }
        let Ok(entries) = std::fs::read_dir(&dir) else {
            continue;
        };
        for entry in entries {
            if Instant::now() >= deadline {
                return None;
            }
            let Ok(entry) = entry else {
                continue;
            };
            let Ok(kind) = entry.file_type() else {
                continue;
            };
            let path = entry.path();
            if kind.is_dir() {
                dirs.push(path);
                continue;
            }
            let within = path.strip_prefix(&root).unwrap_or(&path);
            let target = if kind.is_symlink() {
                let Ok(target) = std::fs::metadata(&path) else {
                    continue;
                };
                if target.is_dir() {
                    if std::fs::canonicalize(&path).is_ok_and(|real| !real.starts_with(&root)) {
                        return Some(refusal(within, OUTSIDE));
                    }
                    continue;
                }
                target.file_type()
            } else {
                kind
            };
            if special(target) {
                return Some(refusal(within, SPECIAL));
            }
        }
    }
    None
}

fn gates_and_reads(apps: &[&Path], pinned: Option<&str>, keys: &[&str]) -> Vec<Gate> {
    let (deadline, bound_by_budget) = helper_deadline();
    let mut plans: Vec<Result<GateHelpers, Gate>> = apps
        .iter()
        .map(|app| GateHelpers::of(app, pinned, keys, deadline))
        .collect();
    let runs = {
        let mut cmds: Vec<&mut Command> = plans
            .iter_mut()
            .filter_map(|plan| plan.as_mut().ok())
            .flat_map(GateHelpers::commands)
            .collect();
        aterm_uds::deadline::outputs_until(&mut cmds, deadline, HELPER_REAP)
    };
    let mut runs = runs.into_iter();
    plans
        .into_iter()
        .zip(apps)
        .map(|(plan, app)| match plan {
            Ok(_) => compose_gate(app, pinned, keys, &mut runs, bound_by_budget),
            Err(refused) => refused,
        })
        .collect()
}

fn compose_gate(
    app: &Path,
    pinned: Option<&str>,
    keys: &[&str],
    runs: &mut impl Iterator<Item = Run>,
    bound_by_budget: bool,
) -> Gate {
    use aterm_update_core::codesign::developer_id_verdict;
    const TEAM: &str = "codesign -d (team id)";
    const SPCTL: &str = "spctl assessment";
    let mut next = || runs.next().unwrap_or(Err(RunError::TimedOut));
    let output = |run: Run, what: &str| {
        run.map_err(|error| run_refusal(what, error, timed_out(what, bound_by_budget)))
    };
    let seal_run = next();
    let (policy, team) = match pinned {
        Some(expected_team) => {
            let seal = developer_id_verdict(seal_run)
                .map_err(|error| codesign_refusal(error, app, bound_by_budget));
            let team = output(next(), TEAM)
                .and_then(|out| display_from(TEAM, app, &out))
                .and_then(|text| parse_team_id(&text));
            let spctl = output(next(), SPCTL).and_then(|out| spctl_from(&out));
            let policy = seal
                .and_then(|()| match &team {
                    Ok(team) if team == expected_team => Ok(()),
                    Ok(team) => Err(format!(
                        "Team ID mismatch: bundle is signed by {team:?}, expected \
                         {expected_team:?}"
                    )),
                    Err(error) => Err(error.clone()),
                })
                .and(spctl);
            (policy, Some(team))
        }
        None => (
            output(seal_run, STRUCTURAL).and_then(|out| structural_from(app, &out)),
            None,
        ),
    };
    let reads = keys
        .iter()
        .map(|key| output(next(), &format!("PlistBuddy {key}")))
        .collect();
    Gate {
        policy,
        team,
        reads,
    }
}

fn gate_and_read(app: &Path, pinned: Option<&str>, keys: &[&str]) -> Gate {
    gates_and_reads(&[app], pinned, keys)
        .pop()
        .unwrap_or_else(|| Gate {
            policy: not_run(),
            team: None,
            reads: Vec::new(),
        })
}

/// Full apply-/stage-time gate.
pub fn verify_bundle(app: &Path, expected_team: &str) -> Result<(), String> {
    gate_and_read(app, Some(expected_team), &[]).policy
}

/// Tiered bundle-authenticity gate (see the crate trust model).
pub fn verify_bundle_policy(app: &Path, expected_team: &str) -> Result<(), String> {
    if expected_team.is_empty() {
        gate_and_read(app, None, &[]).policy
    } else {
        verify_bundle(app, expected_team)
    }
}

const STRUCTURAL: &str = "codesign --verify (structural)";

fn structural_command(app: &Path) -> Command {
    let mut cmd = aterm_uds::spawnfd::command("/usr/bin/codesign");
    cmd.args(["--verify", "--deep", "--strict", "--verbose=2"])
        .arg(app);
    cmd
}

fn structural_from(app: &Path, out: &std::process::Output) -> Result<(), String> {
    if out.status.success() {
        Ok(())
    } else {
        Err(codesign_exit_refusal(
            STRUCTURAL,
            app,
            aterm_update_core::codesign::CodesignError::Refused {
                code: out.status.code(),
                stderr: String::from_utf8_lossy(&out.stderr).into_owned(),
            },
        ))
    }
}

fn plist_command(app: &Path, key: &str) -> Command {
    let mut cmd = aterm_uds::spawnfd::command("/usr/libexec/PlistBuddy");
    cmd.arg("-c")
        .arg(format!("Print :{key}"))
        .arg(app.join("Contents/Info.plist"));
    cmd
}

/// Read `CFBundleVersion` from a staged `.app`'s `Contents/Info.plist`.
pub fn bundle_build_number(app: &Path) -> Result<u64, String> {
    build_number_from(output_bounded(
        &mut plist_command(app, "CFBundleVersion"),
        "PlistBuddy CFBundleVersion",
    )?)
}

fn plist_read_refusal(key: &str, out: &std::process::Output) -> String {
    let refusal = format!(
        "read {key}: {}",
        crate::quote_tool_output(String::from_utf8_lossy(&out.stderr).trim())
    );
    if out.status.code().is_none() {
        mark_passing(refusal)
    } else {
        refusal
    }
}

fn build_number_from(out: std::process::Output) -> Result<u64, String> {
    if !out.status.success() {
        return Err(plist_read_refusal("CFBundleVersion", &out));
    }
    let text = String::from_utf8_lossy(&out.stdout);
    let text = text.trim();
    text.parse::<u64>()
        .map_err(|_| format!("CFBundleVersion {text:?} is not an integer"))
}

/// Read `CFBundleShortVersionString` from a bundle's sealed `Contents/Info.plist`.
pub fn bundle_short_version(app: &Path) -> Result<String, String> {
    short_version_from(output_bounded(
        &mut plist_command(app, "CFBundleShortVersionString"),
        "PlistBuddy CFBundleShortVersionString",
    )?)
}

fn short_version_from(out: std::process::Output) -> Result<String, String> {
    if !out.status.success() {
        return Err(plist_read_refusal("CFBundleShortVersionString", &out));
    }
    if out.stdout.len() > 64 {
        return Err("CFBundleShortVersionString exceeds 64 bytes".to_string());
    }
    let version = String::from_utf8_lossy(&out.stdout).trim().to_string();
    if version.is_empty() {
        return Err("CFBundleShortVersionString is empty".to_string());
    }
    Ok(version)
}

fn git_commit_from(out: std::process::Output) -> Result<String, String> {
    if !out.status.success() {
        return Err(plist_read_refusal("ATermGitCommit", &out));
    }
    if out.stdout.len() > 256 {
        return Err("ATermGitCommit exceeds 256 bytes".to_string());
    }
    let commit = String::from_utf8(out.stdout)
        .map_err(|_| "ATermGitCommit is not valid UTF-8".to_string())?;
    let commit = commit.trim();
    if commit.is_empty() {
        return Err("ATermGitCommit is empty".to_string());
    }
    Ok(commit.to_string())
}

/// The refusal a failed team-pinned `codesign --verify` reports.
pub(crate) fn codesign_refusal(
    error: aterm_update_core::codesign::CodesignError,
    app: &Path,
    bound_by_budget: bool,
) -> String {
    use aterm_update_core::codesign::CodesignError;
    const WHAT: &str = "codesign --verify (team-pinned)";
    match error {
        CodesignError::InvalidTeam(team) => {
            format!("refusing to verify: pinned team id {team:?} is not alphanumeric")
        }
        refused @ CodesignError::Refused { .. } => {
            codesign_exit_refusal("codesign --verify (team-pinned requirement)", app, refused)
        }
        CodesignError::TimedOut => timed_out(WHAT, bound_by_budget),
        CodesignError::Spawn(e) => spawn_refusal(WHAT, &e),
        CodesignError::Wait(e) => mark_passing(format!("wait for {WHAT}: {e}")),
        e @ CodesignError::Unsupported => format!("{WHAT}: {e}"),
    }
}

fn codesign_exit_refusal(
    what: &str,
    app: &Path,
    refused: aterm_update_core::codesign::CodesignError,
) -> String {
    use aterm_update_core::codesign::CodesignError;
    let message = match &refused {
        CodesignError::Refused { code: None, stderr } => format!(
            "{what} was ended by a signal before it reached a verdict: {}",
            crate::quote_tool_output(stderr.trim())
        ),
        CodesignError::Refused {
            code: Some(_),
            stderr,
        } => format!("{what} failed: {}", crate::quote_tool_output(stderr.trim())),
        other => format!("{what}: {other}"),
    };
    if refused.judged_nothing_at(app) {
        mark_passing(message)
    } else {
        message
    }
}

/// Extract the signing Team Identifier from a bundle.
pub fn team_id(app: &Path) -> Result<String, String> {
    parse_team_id(&codesign_display(app, "codesign -d (team id)")?)
}

/// The code-signing identifier `app` is signed with (`codesign -d`'s `Identifier=`).
pub fn signing_identifier(app: &Path) -> Result<String, String> {
    parse_signing_identifier(&codesign_display(app, "codesign -d (identifier)")?)
}

fn codesign_display(app: &Path, what: &str) -> Result<String, String> {
    display_from(what, app, &output_bounded(&mut display_command(app), what)?)
}

fn display_command(app: &Path) -> Command {
    let mut cmd = aterm_uds::spawnfd::command("/usr/bin/codesign");
    cmd.args(["-d", "--verbose=4"]).arg(app);
    cmd
}

fn display_from(what: &str, app: &Path, out: &std::process::Output) -> Result<String, String> {
    // A run that ended on a moment (a signal, or a file it could not read just then) reached
    // no verdict, and its output is no evidence of a missing TeamIdentifier.
    if !out.status.success() {
        let ended = aterm_update_core::codesign::CodesignError::Refused {
            code: out.status.code(),
            stderr: String::from_utf8_lossy(&out.stderr).into_owned(),
        };
        if ended.judged_nothing_at(app) {
            return Err(codesign_exit_refusal(what, app, ended));
        }
    }
    Ok(format!(
        "{}{}",
        String::from_utf8_lossy(&out.stdout),
        String::from_utf8_lossy(&out.stderr)
    ))
}

fn parse_signing_identifier(text: &str) -> Result<String, String> {
    let identifier = text
        .lines()
        .find_map(|l| l.strip_prefix("Identifier="))
        .map(str::trim)
        .ok_or_else(|| "codesign output had no Identifier line".to_string())?;
    if identifier.is_empty() {
        return Err("codesign output had an empty Identifier".to_string());
    }
    Ok(identifier.to_string())
}

fn parse_team_id(text: &str) -> Result<String, String> {
    let team = text
        .lines()
        .find_map(|l| l.strip_prefix("TeamIdentifier="))
        .map(str::trim)
        .ok_or_else(|| "codesign output had no TeamIdentifier line".to_string())?;
    if team.is_empty() || team == "not set" {
        return Err("bundle is ad-hoc/unsigned (TeamIdentifier not set)".to_string());
    }
    Ok(team.to_string())
}

fn spctl_command(app: &Path) -> Command {
    let mut cmd = aterm_uds::spawnfd::command("/usr/sbin/spctl");
    cmd.args(["-a", "-t", "exec", "-vvv"]).arg(app);
    cmd
}

fn spctl_from(out: &std::process::Output) -> Result<(), String> {
    spctl_verdict(out.status, &String::from_utf8_lossy(&out.stderr))
}

fn spctl_verdict(status: std::process::ExitStatus, stderr: &str) -> Result<(), String> {
    if status.success() {
        Ok(())
    } else if status.code().is_none() {
        Err(mark_passing(format!(
            "spctl assessment was ended by a signal before it reached a verdict: {}",
            crate::quote_tool_output(stderr.trim())
        )))
    } else {
        Err(format!(
            "spctl assessment rejected the bundle: {}",
            crate::quote_tool_output(stderr.trim())
        ))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The aggregate budget binds before a helper's own ceiling (the rejection names
    /// the budget), so sixteen serial helpers are bounded by a number.
    #[cfg(unix)]
    #[test]
    fn a_helper_timeout_is_a_passing_refusal() {
        let budget = {
            let _budget = ApplyBudget::start(Duration::from_millis(50));
            output_bounded(Command::new("/bin/sleep").arg("30"), "a slow helper")
                .expect_err("a helper past the budget is a rejection")
        };
        assert!(crate::is_passing_refusal(&budget), "{budget}");
        assert!(budget.contains("verification budget"), "{budget}");
        assert!(
            crate::is_passing_refusal(&timed_out("codesign", false)),
            "the helper's own ceiling is the same moment"
        );
        let copy = status_bounded_with_stderr_observed(
            Command::new("/bin/sleep").arg("30"),
            "ditto",
            Duration::from_millis(100),
        )
        .expect_err("a copy past its limit is a failure")
        .message;
        assert!(crate::is_passing_refusal(&copy), "{copy}");

        let refused = output_bounded(&mut Command::new("/usr/bin/false"), "a refusing helper")
            .expect("the helper ran to an exit");
        assert!(!refused.status.success());
        let verdict = format!(
            "codesign --verify (structural) failed: {}",
            String::from_utf8_lossy(&refused.stderr).trim()
        );
        assert!(!crate::is_passing_refusal(&verdict), "{verdict}");
    }

    #[test]
    fn only_a_moment_s_io_errors_are_passing() {
        for passing in [libc::EAGAIN, libc::ENOMEM, libc::EMFILE, libc::ENFILE] {
            let error = std::io::Error::from_raw_os_error(passing);
            assert!(io_error_is_passing(&error), "{error}");
            assert!(crate::is_passing_refusal(&spawn_refusal(
                "codesign", &error
            )));
        }
        let lock = std::io::Error::new(std::io::ErrorKind::TimedOut, "held for more than 10s");
        assert!(crate::is_passing_refusal(&lock_wait_refusal("lock", &lock)));
        for verdict in [libc::ENOENT, libc::EACCES, libc::ENOLCK, libc::EINVAL] {
            let error = std::io::Error::from_raw_os_error(verdict);
            assert!(!io_error_is_passing(&error), "{error}");
            assert!(!crate::is_passing_refusal(&lock_wait_refusal(
                "lock", &error
            )));
        }
        let missing = output_bounded(
            &mut Command::new("/nonexistent/aterm-update-helper"),
            "a missing helper",
        )
        .expect_err("a helper that does not exist cannot start");
        assert!(!crate::is_passing_refusal(&missing), "{missing}");
    }

    /// A check-thread helper ends at its own limit. The check's `hdiutil` runs
    /// hold the check lane and the machine-wide `checker.lock`, so one that never
    /// returned would park the checker: through `output_within` a child past its
    /// limit is killed, reaped within a short reserve, and reported; one that
    /// finishes is collected.
    #[cfg(unix)]
    #[test]
    fn a_check_helper_past_its_limit_is_killed_and_reported() {
        let error = output_within(
            Command::new("/bin/sleep").arg("30"),
            "sleep",
            Duration::from_millis(300),
        )
        .expect_err("a helper past its limit is a failure");
        assert!(crate::is_passing_refusal(&error), "{error}");
        let out = output_within(
            Command::new("/bin/echo").arg("hi"),
            "echo",
            aterm_spec::wait::MUST_HAPPEN,
        )
        .expect("a helper inside its limit runs");
        assert_eq!(String::from_utf8_lossy(&out.stdout).trim(), "hi");
    }

    /// The budget restores whatever was in force when it drops, so a nested or
    /// sequential apply cannot inherit a spent deadline.
    #[cfg(unix)]
    #[test]
    fn the_budget_guard_restores_the_previous_deadline() {
        assert!(APPLY_DEADLINE.with(|d| d.get()).is_none());
        {
            let _outer = ApplyBudget::start(Duration::from_secs(60));
            let outer = APPLY_DEADLINE.with(|d| d.get()).expect("outer budget");
            {
                let _inner = ApplyBudget::start(Duration::from_millis(10));
                assert!(APPLY_DEADLINE.with(|d| d.get()).expect("inner") < outer);
            }
            assert_eq!(APPLY_DEADLINE.with(|d| d.get()), Some(outer));
        }
        assert!(APPLY_DEADLINE.with(|d| d.get()).is_none());
    }

    /// A slow-but-honest copy must not spend the budget the checks after it need.
    #[cfg(unix)]
    #[test]
    fn extending_the_budget_pays_back_disk_time() {
        let _budget = ApplyBudget::start(Duration::from_secs(1));
        let before = APPLY_DEADLINE.with(|d| d.get()).expect("budget");
        ApplyBudget::extend(Duration::from_secs(30));
        let after = APPLY_DEADLINE.with(|d| d.get()).expect("budget");
        assert!(after >= before + Duration::from_secs(29));
    }

    #[test]
    fn helper_cleanup_requires_an_observed_exit_and_never_renews_the_deadline() {
        use std::os::unix::process::ExitStatusExt;
        let model = aterm_spec::ty_model! {
            BoundedHelperCleanup {
                const Buggy = 0;
                var killed = 0;
                var observed = 0;
                var stopped = 0;
                action Kill when (killed == 0) {
                    killed = 1;
                    stopped = if Buggy == 1 { 1 } else { 0 };
                }
                action ObserveExit when (killed == 1 && observed == 0) {
                    observed = 1;
                    stopped = 1;
                }
                action LoseWait when (killed == 1 && observed == 0) { observed = 2; }
                invariant StopRequiresExit: stopped == 0 || observed == 1;
            }
        };
        aterm_spec::verify::prove_and_catch_scalar(&model, "bounded helper cleanup");
        struct FakeChild {
            exit: u8,
            kill_error: bool,
            kills: usize,
            deadline: Instant,
            late: usize,
        }
        impl HelperChild for FakeChild {
            fn request_kill(&mut self) -> std::io::Result<()> {
                self.kills += 1;
                if self.kill_error {
                    Err(std::io::Error::other("kill refused"))
                } else {
                    Ok(())
                }
            }
            fn observe_exit(&mut self) -> std::io::Result<Option<std::process::ExitStatus>> {
                match self.exit {
                    1 => Ok(Some(std::process::ExitStatus::from_raw(0))),
                    2 => Err(std::io::Error::other("wait failed")),
                    _ => {
                        if Instant::now() >= self.deadline {
                            self.late += 1;
                        }
                        if self.late > 1 {
                            return Err(std::io::Error::other("still looked at past the deadline"));
                        }
                        Ok(None)
                    }
                }
            }
        }
        for exit in 0..=2 {
            for kill_error in [false, true] {
                let started = Instant::now();
                let deadline = started + Duration::from_millis(15);
                let mut child = FakeChild {
                    exit,
                    kill_error,
                    kills: 0,
                    deadline,
                    late: 0,
                };
                let stopped = stop_helper_before(&mut child, deadline);
                assert_eq!(child.kills, 1);
                assert!(
                    child.late <= 1,
                    "cleanup must not wait unboundedly after a failed kill: {} looks past \
                     its deadline",
                    child.late
                );
                assert!(
                    started.elapsed() < Duration::from_secs(60),
                    "cleanup must not wait unboundedly after a failed kill: {:?}",
                    started.elapsed()
                );
                let mut state = model.successors("Kill", &model.init_state())[0].clone();
                if exit > 0 {
                    let action = if exit == 1 { "ObserveExit" } else { "LoseWait" };
                    state = model.successors(action, &state)[0].clone();
                }
                assert_eq!(state["stopped"], i64::from(stopped));
                assert!(model.check_invariant("StopRequiresExit", &state));
                if exit != 1 {
                    state.insert("stopped", 1);
                    assert!(!model.check_invariant("StopRequiresExit", &state));
                }
            }
        }
    }

    #[test]
    fn timed_out_owned_helper_is_reaped_before_its_copy_path_is_reclaimable() {
        let staging = crate::paths::Staging::scratch("owned-helper-reap");
        let receipt = staging.root.join("child.pid");
        let failure = status_bounded_with_stderr_observed(
            Command::new("/bin/sh")
                .arg("-c")
                .arg(r#"printf '%s\n' "$$" > "$1"; exec /bin/sleep 30"#)
                .arg("owned-helper-fixture")
                .arg(&receipt),
            "owned copy fixture",
            Duration::from_secs(2),
        )
        .unwrap_err();
        assert!(crate::is_passing_refusal(&failure.message), "{failure:?}");
        assert!(failure.writer_stopped, "{failure:?}");
        let pid: i32 = std::fs::read_to_string(&receipt)
            .unwrap()
            .trim()
            .parse()
            .unwrap();
        assert_eq!(unsafe { libc::kill(pid, 0) }, -1);
        assert_eq!(
            std::io::Error::last_os_error().raw_os_error(),
            Some(libc::ESRCH)
        );
        let absent = status_bounded_with_stderr_observed(
            &mut Command::new(staging.root.join("no-such-helper")),
            "absent helper",
            Duration::from_secs(2),
        )
        .unwrap_err();
        assert!(absent.writer_stopped, "a refused spawn created no writer");
        let _ = std::fs::remove_dir_all(&staging.root);
    }

    /// The tail of the child's stderr comes back with its status; a child that floods stderr
    /// is drained past the pipe's capacity, and only its last bytes are kept.
    #[cfg(unix)]
    #[test]
    fn status_bounded_keeps_the_tail_of_stderr_without_wedging_on_a_flood() {
        let (status, stderr) = status_bounded_with_stderr_observed(
            Command::new("/bin/sh")
                .arg("-c")
                .arg("echo 'first line' >&2; echo 'ditto: No space left on device' >&2; exit 1"),
            "a copy that fails",
            Duration::from_secs(60),
        )
        .expect("the child ran");
        assert!(!status.success());
        assert_eq!(
            last_stderr_line(&stderr),
            Some("ditto: No space left on device"),
            "{stderr:?}"
        );

        let (status, stderr) = status_bounded_with_stderr_observed(
            Command::new("/bin/sh")
                .arg("-c")
                .arg("i=0; while [ $i -lt 2048 ]; do echo 'a line of stderr noise, sixty-four bytes long, give or take..' >&2; i=$((i+1)); done; echo 'the last line' >&2"),
            "a chatty copy",
            Duration::from_secs(60),
        )
        .expect("a chatty child must not wedge the poll loop");
        assert!(status.success());
        assert!(
            stderr.len() <= STDERR_KEEP,
            "only the tail is kept: {}",
            stderr.len()
        );
        assert_eq!(last_stderr_line(&stderr), Some("the last line"));

        let (status, stderr) = status_bounded_with_stderr_observed(
            &mut Command::new("/usr/bin/true"),
            "a quiet copy",
            Duration::from_secs(60),
        )
        .expect("ran");
        assert!(status.success() && stderr.is_empty() && last_stderr_line(&stderr).is_none());
    }

    #[test]
    fn signing_identifier_parsed_from_codesign_output() {
        let sample = "Executable=/x\nIdentifier=com.aterm.aterm\nTeamIdentifier=ABCDE12345\n";
        assert_eq!(parse_signing_identifier(sample).unwrap(), "com.aterm.aterm");
        let reordered = "TeamIdentifier=ABCDE12345\nIdentifier=com.aterm.aterm.dev\n";
        assert_eq!(
            parse_signing_identifier(reordered).unwrap(),
            "com.aterm.aterm.dev"
        );
        assert!(parse_signing_identifier("TeamIdentifier=ABCDE12345\n").is_err());
        assert!(parse_signing_identifier("Identifier=\n").is_err());
    }

    #[test]
    fn team_id_is_read_from_codesign_output_and_fails_closed_on_adhoc_or_missing() {
        let sample = "Executable=/x\nIdentifier=com.aterm.aterm\nTeamIdentifier=ABCDE12345\nSealed Resources\n";
        assert_eq!(parse_team_id(sample).unwrap(), "ABCDE12345");
        assert!(parse_team_id("TeamIdentifier=not set\n").is_err());
        assert!(parse_team_id("Identifier=com.aterm.aterm\n").is_err());
        assert!(parse_team_id("TeamIdentifier=\n").is_err());
    }

    #[test]
    fn verify_bundle_refuses_a_garbage_pin_and_another_signer() {
        verify_bundle(Path::new("/nonexistent"), "AB\"CD").unwrap_err();
        aterm_uds::deadline::require_trust_service();
        let refused = verify_bundle(Path::new("/bin/ls"), "ABCDE12345").unwrap_err();
        assert!(!crate::is_passing_refusal(&refused), "{refused}");
    }

    #[test]
    fn only_a_codesign_moment_is_passing() {
        use aterm_update_core::codesign::CodesignError;
        let refused = |code: Option<i32>, stderr: &str| CodesignError::Refused {
            code,
            stderr: stderr.to_string(),
        };
        for passing in [
            refused(None, ""),
            refused(None, "aterm.app: code object is not signed at all\n"),
            refused(
                Some(1),
                "aterm.app: internal error in Code Signing subsystem\n",
            ),
            refused(Some(1), "aterm.app: Permission denied\n"),
            refused(Some(1), "aterm.app: Too many open files\n"),
            refused(
                Some(1),
                "aterm.app: the code cannot be read by the verifier (file system permissions \
                 etc.)\n",
            ),
            CodesignError::Wait(std::io::Error::from_raw_os_error(libc::EINTR)),
        ] {
            let pinned = codesign_refusal(passing, Path::new("aterm.app"), false);
            assert!(crate::is_passing_refusal(&pinned), "{pinned}");
        }
        let structural = codesign_exit_refusal(
            "codesign --verify (structural)",
            Path::new("aterm.app"),
            refused(
                Some(1),
                "aterm.app: internal error in Code Signing subsystem\n",
            ),
        );
        assert!(crate::is_passing_refusal(&structural), "{structural}");

        let verdicts = [
            (
                Some(3),
                "aterm.app: code failed to satisfy specified code requirement(s)\n",
            ),
            (Some(1), "aterm.app: code object is not signed at all\n"),
            (
                Some(1),
                "aterm.app: invalid signature (code or signature have been modified)\n",
            ),
            (
                Some(1),
                "aterm.app: bundle format unrecognized, invalid, or unsuitable\n",
            ),
            (
                Some(1),
                "aterm.app: a sealed resource is missing or invalid\nfile modified: \
                 /Applications/aterm.app/Contents/Resources/aterm-handoff-policy.toml\n",
            ),
            (
                Some(1),
                "aterm.app: a sealed resource is missing or invalid\nfile added: \
                 /Applications/aterm.app/Contents/Resources/x: Permission denied\n",
            ),
            (
                Some(1),
                "aterm.app: invalid Info.plist (plist or signature have been modified)\n\
                 In architecture: arm64e\n",
            ),
            (
                Some(1),
                "aterm.app: invalid resource directory (directory or signature have been \
                 modified)\n",
            ),
            (
                Some(1),
                "aterm.app: unsealed contents present in the bundle root\n",
            ),
            (Some(1), "aterm.app: nested code is modified or invalid\n"),
            (Some(1), "aterm.app: a verdict nobody has measured yet\n"),
            (Some(1), "aterm.app: No such file or directory\n"),
            (
                Some(1),
                "aterm.app: invalid destination for symbolic link in bundle\nfile modified: \
                 /Applications/aterm.app/Contents/Resources/h: Permission denied\n",
            ),
            (
                Some(1),
                "--prepared:aterm.app/Contents/Helpers/h\naterm.app: Permission denied\n\
                 aterm.app: nested code is modified or invalid\n",
            ),
            (Some(1), ""),
            (Some(2), "codesign: unrecognized option\n"),
        ];
        for (code, stderr) in verdicts {
            let line = codesign_refusal(refused(code, stderr), Path::new("aterm.app"), false);
            assert!(!crate::is_passing_refusal(&line), "{line}");
            let structural = codesign_exit_refusal(
                "codesign --verify (structural)",
                Path::new("aterm.app"),
                refused(code, stderr),
            );
            assert!(!crate::is_passing_refusal(&structural), "{structural}");
        }
    }

    #[test]
    fn a_verdict_that_quotes_a_refusal_key_is_still_a_verdict() {
        use aterm_update_core::codesign::CodesignError;
        use std::os::unix::process::ExitStatusExt as _;
        let exited = std::process::ExitStatus::from_raw(1 << 8);
        let bundle = aterm_tempfile::Builder::new()
            .prefix("aterm-quoted-key-")
            .tempdir()
            .unwrap();
        let app = bundle.path().join("aterm.app");
        std::fs::create_dir_all(app.join("Contents/MacOS")).unwrap();
        std::fs::write(app.join("Contents/MacOS/aterm"), "").unwrap();
        for key in [
            crate::PASSING_REFUSAL_KEY,
            crate::INSTALL_LOCATION_REFUSAL_KEY,
            crate::ROSTER_UNCONFIRMED_KEY,
            crate::NOT_INSTALLED_KEY,
            crate::install::ROLLBACK_SOURCE_REFUSAL_KEY,
            crate::github::STALE_ANCHOR_KEY,
        ] {
            let said = format!("/x/Outer.app/Contents/Helpers/q: {key}.app");
            let output = |code: i32, stdout: &str| std::process::Output {
                status: std::process::ExitStatus::from_raw(code << 8),
                stdout: stdout.as_bytes().to_vec(),
                stderr: format!("{said}\n").into_bytes(),
            };
            let refusals = [
                codesign_exit_refusal(
                    "codesign --verify",
                    Path::new("Outer.app"),
                    CodesignError::Refused {
                        code: Some(1),
                        stderr: format!(
                            "Outer.app: main executable failed strict validation\n\
                             In subcomponent: {said}\n"
                        ),
                    },
                ),
                spctl_verdict(exited, &said).unwrap_err(),
                plist_read_refusal("CFBundleVersion", &output(1, "")),
                probe_starts(&app, 9_999_999, |_| Ok(output(1, ""))).unwrap_err(),
                probe_starts(&app, 9_999_999, |_| Ok(output(0, &said))).unwrap_err(),
            ];
            for refusal in refusals {
                assert!(!refusal.contains(key), "{key}: {refusal}");
                assert!(
                    refusal.contains("Helpers/q: "),
                    "the quote reads: {refusal}"
                );
                assert!(!crate::is_passing_refusal(&refusal), "{refusal}");
                assert!(!crate::is_install_location_refusal(&refusal), "{refusal}");
                assert!(
                    !crate::install::is_rollback_source_refusal(&refusal),
                    "{refusal}"
                );
                assert!(
                    !crate::github::is_stale_anchor_refusal(&refusal),
                    "{refusal}"
                );
                assert!(!crate::refusal_needs_person(&refusal), "{refusal}");
            }
        }
        let moment = codesign_exit_refusal(
            "codesign --verify",
            Path::new("Outer.app"),
            CodesignError::Refused {
                code: None,
                stderr: String::new(),
            },
        );
        assert!(crate::is_passing_refusal(&moment), "{moment}");
    }

    /// A start probe whose candidate a signal ended never saw it exit: a verdict on the
    /// build that names the signal ([`START_PROBE_SIGNALLED`]), so its candidate stays where
    /// it ran (`install::probe_left_unproven`); one that exited with a failure is a plain
    /// verdict.
    #[test]
    fn a_signal_ending_the_start_probe_is_a_verdict_that_says_so() {
        use std::os::unix::process::ExitStatusExt as _;
        let bundle = aterm_tempfile::Builder::new()
            .prefix("aterm-probe-signal-")
            .tempdir()
            .unwrap();
        let app = bundle.path().join("aterm.app");
        std::fs::create_dir_all(app.join("Contents/MacOS")).unwrap();
        std::fs::write(app.join("Contents/MacOS/aterm"), "").unwrap();
        let output = |status| std::process::Output {
            status,
            stdout: Vec::new(),
            stderr: Vec::new(),
        };
        let killed = probe_starts(&app, 1, |_| {
            Ok(output(std::process::ExitStatus::from_raw(libc::SIGKILL)))
        })
        .unwrap_err();
        assert!(!crate::is_passing_refusal(&killed), "{killed}");
        assert!(killed.starts_with(START_PROBE_SIGNALLED), "{killed}");
        let exited = probe_starts(&app, 1, |_| {
            Ok(output(std::process::ExitStatus::from_raw(1 << 8)))
        })
        .unwrap_err();
        assert!(!crate::is_passing_refusal(&exited), "{exited}");
        assert!(!exited.starts_with(START_PROBE_SIGNALLED), "{exited}");
    }

    /// A tampered bundle named like a machine fault is a verdict, measured on the real
    /// codesign through the structural lane on ad hoc signed `.app`s: a dangling symlink
    /// added after signing (codesign's whole word is `<app>: No such file or
    /// directory`); a symlink out of the bundle named `h: Permission denied`, read from
    /// stderr or both streams; and nested helpers named `h: Permission denied`,
    /// `h\nX: Permission denied` and after the passing key, with a sibling re-signed
    /// after the seal, which codesign names on `--verbose=2` progress lines before its
    /// verdict.
    #[cfg(target_os = "macos")]
    #[test]
    fn a_tampered_bundle_named_like_a_machine_fault_is_a_verdict() {
        use std::os::unix::fs::PermissionsExt as _;
        aterm_uds::deadline::require_trust_service();
        let root = aterm_tempfile::Builder::new()
            .prefix("aterm-codesign-tamper-")
            .tempdir()
            .unwrap();
        let unsigned = |name: &str| unsigned_app(root.path(), name);
        let verdict = |app: &Path, words: &str| {
            let refusal = verify_bundle_policy(app, "").expect_err(words);
            assert!(refusal.contains(words), "{refusal}");
            assert!(!crate::is_passing_refusal(&refusal), "{refusal}");
        };

        let clean = unsigned("Clean.app");
        aterm_uds::deadline::sign_ad_hoc(&clean);
        assert_eq!(verify_bundle_policy(&clean, ""), Ok(()));

        let dangling = unsigned("Dangling.app");
        aterm_uds::deadline::sign_ad_hoc(&dangling);
        std::os::unix::fs::symlink(
            "/nonexistent/aterm-dangling",
            dangling.join("Contents/Resources/dangling"),
        )
        .unwrap();
        verdict(&dangling, ": No such file or directory");

        let named = unsigned("Named.app");
        std::os::unix::fs::symlink(
            "/etc/hosts",
            named.join("Contents/Resources/h: Permission denied"),
        )
        .unwrap();
        aterm_uds::deadline::sign_ad_hoc(&named);
        verdict(&named, "invalid destination for symbolic link in bundle");
        let out = output_bounded(&mut structural_command(&named), STRUCTURAL).unwrap();
        let both = aterm_update_core::codesign::CodesignError::Refused {
            code: out.status.code(),
            stderr: format!(
                "{}{}",
                String::from_utf8_lossy(&out.stderr),
                String::from_utf8_lossy(&out.stdout)
            ),
        };
        assert!(!both.judged_nothing_at(&named), "{both:?}");

        let nested = unsigned("Nested.app");
        let helpers = nested.join("Contents/Helpers");
        std::fs::create_dir_all(&helpers).unwrap();
        for name in [
            "h: Permission denied".to_string(),
            "h\nX: Permission denied".to_string(),
            format!("q: {}", crate::PASSING_REFUSAL_KEY),
            "z".to_string(),
        ] {
            std::fs::copy("/bin/ls", helpers.join(&name)).unwrap();
            aterm_uds::deadline::sign_ad_hoc(&helpers.join(name));
        }
        aterm_uds::deadline::sign_ad_hoc(&nested);
        let resigned = output_bounded(
            aterm_uds::spawnfd::command("/usr/bin/codesign")
                .args(["--force", "--sign", "-", "--identifier", "other"])
                .arg(helpers.join("z")),
            "codesign --sign (ad hoc)",
        )
        .unwrap();
        assert!(resigned.status.success(), "{resigned:?}");
        verdict(&nested, "nested code is modified or invalid");

        let unreadable = unsigned("Unreadable.app");
        aterm_uds::deadline::sign_ad_hoc(&unreadable);
        let r = unreadable.join("Contents/Resources/r");
        std::fs::set_permissions(&r, std::fs::Permissions::from_mode(0o000)).unwrap();
        let premise = std::fs::read(&r).is_err();
        let moment = verify_bundle_policy(&unreadable, "");
        std::fs::set_permissions(&r, std::fs::Permissions::from_mode(0o644)).unwrap();
        if premise {
            let refusal = moment.expect_err("an unreadable sealed resource");
            assert!(crate::is_passing_refusal(&refusal), "{refusal}");
        }
    }

    /// A minimal unsigned `.app` at `root/name`: `/bin/ls` as its main executable `A`, an
    /// `Info.plist` naming it, and one resource, `Contents/Resources/r`.
    #[cfg(target_os = "macos")]
    fn unsigned_app(root: &Path, name: &str) -> std::path::PathBuf {
        let app = root.join(name);
        std::fs::create_dir_all(app.join("Contents/MacOS")).unwrap();
        std::fs::create_dir_all(app.join("Contents/Resources")).unwrap();
        std::fs::copy("/bin/ls", app.join("Contents/MacOS/A")).unwrap();
        std::fs::write(
            app.join("Contents/Info.plist"),
            "<?xml version=\"1.0\" encoding=\"UTF-8\"?>\n<plist version=\"1.0\"><dict>\
             <key>CFBundleExecutable</key><string>A</string>\
             <key>CFBundleIdentifier</key><string>t.a</string></dict></plist>\n",
        )
        .unwrap();
        std::fs::write(app.join("Contents/Resources/r"), "x\n").unwrap();
        app
    }

    /// A bundle codesign cannot read to an answer is a verdict, refused before any
    /// helper runs.
    #[cfg(target_os = "macos")]
    #[test]
    fn a_bundle_codesign_cannot_read_to_an_answer_is_a_verdict() {
        aterm_uds::deadline::require_trust_service();
        let root = aterm_tempfile::Builder::new()
            .prefix("aterm-codesign-fifo-")
            .tempdir()
            .unwrap();
        let fifo = |at: &Path| {
            let _ = std::fs::remove_file(at);
            let path = std::ffi::CString::new(at.as_os_str().as_encoded_bytes()).unwrap();
            // SAFETY: `path` is a valid NUL-terminated path for the call's duration.
            assert_eq!(unsafe { libc::mkfifo(path.as_ptr(), 0o644) }, 0, "{at:?}");
        };
        let signed = |name: &str| {
            let app = unsigned_app(root.path(), name);
            aterm_uds::deadline::sign_ad_hoc(&app);
            app
        };
        let clean = signed("Clean.app");
        assert_eq!(verify_bundle_policy(&clean, ""), Ok(()));
        let inside = unsigned_app(root.path(), "Inside.app");
        std::fs::create_dir(inside.join("Contents/Resources/real")).unwrap();
        std::os::unix::fs::symlink("real", inside.join("Contents/Resources/link")).unwrap();
        aterm_uds::deadline::sign_ad_hoc(&inside);
        assert_eq!(verify_bundle_policy(&inside, ""), Ok(()));

        let seal = signed("Seal.app");
        fifo(&seal.join("Contents/_CodeSignature/CodeResources"));
        let exe = signed("Exe.app");
        fifo(&exe.join("Contents/MacOS/A"));
        let outside = root.path().join("outside");
        fifo(&outside);
        let linked = signed("Linked.app");
        std::fs::remove_file(linked.join("Contents/Resources/r")).unwrap();
        std::os::unix::fs::symlink(&outside, linked.join("Contents/Resources/r")).unwrap();
        let named = signed("Named.app");
        let key = crate::PASSING_REFUSAL_KEY;
        fifo(&named.join(format!("Contents/Resources/q: {key}")));
        let moved_out = |name: &str, dir: &str, at: &str| {
            let app = signed(name);
            let out = root.path().join(format!("{name}.out"));
            std::fs::rename(app.join(dir), &out).unwrap();
            std::os::unix::fs::symlink(&out, app.join(dir)).unwrap();
            fifo(&out.join(at));
            app
        };
        let contents = moved_out("Contents.app", "Contents", "_CodeSignature/CodeResources");
        let macos = moved_out("MacOS.app", "Contents/MacOS", "A");
        let sealed = moved_out("Sealed.app", "Contents/_CodeSignature", "CodeResources");

        let _budget = ApplyBudget::start(Duration::from_secs(5));
        let verdict = |refusal: &str| {
            refusal.contains("which no signed bundle holds") && !crate::is_passing_refusal(refusal)
        };
        for (app, team) in [
            (&seal, ""),
            (&seal, "ABCDE12345"),
            (&exe, ""),
            (&linked, ""),
            (&named, ""),
            (&named, "ABCDE12345"),
            (&contents, ""),
            (&macos, ""),
            (&sealed, "ABCDE12345"),
        ] {
            let refusal = verify_bundle_policy(app, team).expect_err("a bundle it cannot read");
            assert!(verdict(&refusal), "{app:?} {team:?}: {refusal}");
        }
        let checks = check_bundle(&seal, "", true);
        assert!(
            checks.build.as_ref().is_err_and(|why| verdict(why)),
            "{:?}",
            checks.build
        );
    }

    /// A bundle check runs several helpers on the bundle, and a signal ending the
    /// team id's (`codesign -d`), Gatekeeper's (`spctl`) or a sealed read's
    /// (`PlistBuddy`) is as much a moment as one ending the first, and so is a wait
    /// on any of them that failed.
    #[cfg(unix)]
    #[test]
    fn a_signal_ending_a_bundle_check_helper_is_no_verdict() {
        use std::os::unix::process::ExitStatusExt as _;
        let killed = std::process::ExitStatus::from_raw(libc::SIGKILL);
        let rejected = std::process::ExitStatus::from_raw(3 << 8);
        let spctl = spctl_verdict(killed, "").expect_err("a killed spctl did not pass");
        assert!(crate::is_passing_refusal(&spctl), "{spctl}");
        let spctl = spctl_verdict(rejected, "aterm.app: rejected\nsource=Unnotarized")
            .expect_err("a rejection");
        assert!(!crate::is_passing_refusal(&spctl), "{spctl}");
        let plist = |status| std::process::Output {
            status,
            stdout: Vec::new(),
            stderr: b"Print: Entry Does Not Exist\n".to_vec(),
        };
        let reads = |status| {
            [
                build_number_from(plist(status)).map(|_| ()),
                short_version_from(plist(status)).map(|_| ()),
                git_commit_from(plist(status)).map(|_| ()),
            ]
        };
        for read in reads(killed) {
            let refusal = read.expect_err("a killed read did not pass");
            assert!(crate::is_passing_refusal(&refusal), "{refusal}");
        }
        let exited = std::process::ExitStatus::from_raw(1 << 8);
        for read in reads(exited) {
            let refusal = read.expect_err("a failed read");
            assert!(!crate::is_passing_refusal(&refusal), "{refusal}");
        }
        let wait = run_refusal(
            "PlistBuddy CFBundleVersion",
            RunError::Wait(std::io::Error::from_raw_os_error(libc::EINTR)),
            String::new(),
        );
        assert!(crate::is_passing_refusal(&wait), "{wait}");

        aterm_uds::deadline::require_trust_service();
        use std::os::unix::fs::PermissionsExt as _;
        let root = aterm_tempfile::Builder::new()
            .prefix("aterm-team-id-moment-")
            .tempdir()
            .unwrap();
        let exe = root.path().join("aterm.app/Contents/MacOS/aterm");
        std::fs::create_dir_all(exe.parent().unwrap()).unwrap();
        std::fs::copy("/bin/ls", &exe).unwrap();
        std::fs::write(
            root.path().join("aterm.app/Contents/Info.plist"),
            "<?xml version=\"1.0\" encoding=\"UTF-8\"?>\n<plist version=\"1.0\"><dict>\
             <key>CFBundleExecutable</key><string>aterm</string></dict></plist>\n",
        )
        .unwrap();
        std::fs::set_permissions(&exe, std::fs::Permissions::from_mode(0o000)).unwrap();
        let premise = std::fs::read(&exe).is_err();
        let unread = team_id(&root.path().join("aterm.app"));
        std::fs::set_permissions(&exe, std::fs::Permissions::from_mode(0o755)).unwrap();
        let refusal = team_id(&root.path().join("aterm.app")).expect_err("no team id");
        assert!(!crate::is_passing_refusal(&refusal), "{refusal}");
        if !premise {
            aterm_uds::host::could_not_run(
                "a mode-0 executable read anyway: root, or a file system that ignores modes",
            );
        }
        let refusal = unread.expect_err("an unreadable bundle has no team id");
        assert!(crate::is_passing_refusal(&refusal), "{refusal}");
    }

    /// An ad-hoc-signed `aterm.app` under `root` whose Info.plist carries `plist`
    /// (the `<dict>` body) and whose executable is a script exiting 0.
    #[cfg(target_os = "macos")]
    fn ad_hoc_bundle(root: &Path, plist: &str) -> std::path::PathBuf {
        use std::os::unix::fs::PermissionsExt as _;
        let app = root.join("aterm.app");
        let exe = app.join("Contents/MacOS/aterm");
        std::fs::create_dir_all(exe.parent().unwrap()).unwrap();
        std::fs::write(&exe, "#!/bin/sh\nexit 0\n").unwrap();
        std::fs::set_permissions(&exe, std::fs::Permissions::from_mode(0o755)).unwrap();
        std::fs::write(
            app.join("Contents/Info.plist"),
            format!(
                "<?xml version=\"1.0\" encoding=\"UTF-8\"?>\n<plist version=\"1.0\"><dict>\
                 <key>CFBundleExecutable</key><string>aterm</string>\
                 <key>CFBundleIdentifier</key><string>com.aterm.fixture</string>\
                 {plist}</dict></plist>\n"
            ),
        )
        .unwrap();
        aterm_uds::deadline::sign_ad_hoc(&app);
        app
    }

    /// Every helper of a bundle check answers for itself, and the verdict is the
    /// serial one.
    #[cfg(target_os = "macos")]
    #[test]
    fn each_helper_of_a_bundle_check_answers_for_itself_and_the_verdict_is_the_serial_one() {
        aterm_uds::deadline::require_trust_service();
        let root = aterm_tempfile::Builder::new()
            .prefix("aterm-check-bundle-")
            .tempdir()
            .unwrap();
        let app = ad_hoc_bundle(
            root.path(),
            "<key>CFBundleVersion</key><string>777</string>\
             <key>ATermGitCommit</key><string>0123456789abcdef0123456789abcdef01234567</string>\
             <key>CFBundleShortVersionString</key><string>1.2.3</string>",
        );
        let checks = check_bundle(&app, "", true);
        assert_eq!(checks.policy, Ok(()));
        assert_eq!(checks.build, Ok(777));
        assert_eq!(
            checks.commit.as_deref(),
            Ok("0123456789abcdef0123456789abcdef01234567")
        );
        assert_eq!(checks.short_version, Some(Ok("1.2.3".to_string())));
        assert!(checks.team.is_none(), "the structural tier reads no team");
        let pinned = check_bundle(&app, "ABCDE12345", false);
        let policy = pinned.policy.expect_err("an ad-hoc bundle fails the pin");
        assert!(
            policy.starts_with("codesign --verify (team-pinned requirement) failed: "),
            "{policy}"
        );
        assert!(!crate::is_passing_refusal(&policy), "{policy}");
        let team = pinned.team.expect("a pinned check reads the team");
        assert!(
            team.expect_err("ad hoc has no team")
                .contains("TeamIdentifier not set"),
            "the team helper ran and answered"
        );
        assert_eq!(pinned.build, Ok(777));
        assert_eq!(pinned.short_version, None, "not asked for");
        let unreadable = ad_hoc_bundle(
            &root.path().join("unreadable"),
            "<key>CFBundleVersion</key><string>seven</string>",
        );
        let checks = check_bundle(&unreadable, "", false);
        assert_eq!(checks.policy, Ok(()));
        let build = checks.build.expect_err("not an integer");
        assert!(build.contains("is not an integer"), "{build}");
        assert!(
            checks
                .commit
                .expect_err("absent")
                .starts_with("read ATermGitCommit")
        );
    }

    /// Several bundles checked at once each get their own answers. `check_bundles`
    /// starts every bundle's helpers together (the boot apply's staged and installed
    /// bundles); each bundle's answers are its own helpers', in the order given, and
    /// the same as `check_bundle` of it alone: a broken seal refuses only its own
    /// bundle, and its reads still answer.
    #[cfg(target_os = "macos")]
    #[test]
    fn several_bundles_checked_at_once_each_get_their_own_answers() {
        aterm_uds::deadline::require_trust_service();
        let root = aterm_tempfile::Builder::new()
            .prefix("aterm-check-bundles-")
            .tempdir()
            .unwrap();
        let plist = |build: u32| {
            format!(
                "<key>CFBundleVersion</key><string>{build}</string>\
                 <key>ATermGitCommit</key><string>0123456789abcdef0123456789abcdef0123456{}</string>\
                 <key>CFBundleShortVersionString</key><string>1.2.{build}</string>",
                build % 10
            )
        };
        let a = ad_hoc_bundle(&root.path().join("a"), &plist(777));
        let broken = ad_hoc_bundle(&root.path().join("broken"), &plist(779));
        std::fs::write(broken.join("Contents/MacOS/aterm"), "#!/bin/sh\nexit 1\n").unwrap();
        let b = ad_hoc_bundle(&root.path().join("b"), &plist(778));
        let apps = [a.as_path(), broken.as_path(), b.as_path()];
        let together = check_bundles(&apps, "", true);
        assert_eq!(together.len(), 3);
        for (app, checks) in apps.iter().zip(&together) {
            let alone = check_bundle(app, "", true);
            assert_eq!(checks.policy.is_ok(), alone.policy.is_ok(), "{app:?}");
            assert_eq!(checks.build, alone.build, "{app:?}");
            assert_eq!(checks.commit, alone.commit, "{app:?}");
            assert_eq!(checks.short_version, alone.short_version, "{app:?}");
        }
        assert_eq!(together[0].policy, Ok(()));
        assert_eq!(together[0].build, Ok(777));
        let seal = together[1].policy.as_ref().expect_err("a broken seal");
        assert!(!crate::is_passing_refusal(seal), "{seal}");
        assert_eq!(together[1].build, Ok(779), "its reads still answer");
        assert_eq!(together[2].policy, Ok(()));
        assert_eq!(together[2].build, Ok(778));
        assert_eq!(
            together[2].commit.as_deref(),
            Ok("0123456789abcdef0123456789abcdef01234568")
        );
        for checks in check_bundles(&[a.as_path(), b.as_path()], "AB\"CD", false) {
            assert!(
                checks.policy.is_err() && checks.build.is_err(),
                "{checks:?}"
            );
        }
    }

    /// A helper of a bundle check that never answers ends at the deadline, as a
    /// passing refusal naming the budget, and the call comes back: here every
    /// `PlistBuddy` blocks opening an Info.plist that is a FIFO nobody writes, and the
    /// budget is spent before the bundle's walk ([`unanswerable_entry`]) can refuse the
    /// FIFO, so what it did not reach is left to the helpers.
    #[cfg(target_os = "macos")]
    #[test]
    fn a_bundle_check_helper_that_never_answers_is_a_passing_refusal_at_the_budget() {
        let root = aterm_tempfile::Builder::new()
            .prefix("aterm-check-bundle-fifo-")
            .tempdir()
            .unwrap();
        let app = root.path().join("aterm.app");
        std::fs::create_dir_all(app.join("Contents")).unwrap();
        let plist = std::ffi::CString::new(
            app.join("Contents/Info.plist")
                .into_os_string()
                .into_encoded_bytes(),
        )
        .unwrap();
        // SAFETY: a NUL-terminated path that outlives the call.
        assert_eq!(unsafe { libc::mkfifo(plist.as_ptr(), 0o600) }, 0);
        let begun = Instant::now();
        let checks = {
            let _budget = ApplyBudget::start(Duration::ZERO);
            check_bundle(&app, "", false)
        };
        assert!(
            begun.elapsed() < aterm_spec::wait::MUST_HAPPEN,
            "the check did not come back"
        );
        for read in [checks.build.map(drop), checks.commit.map(drop)] {
            let refusal = read.expect_err("a read that never answered is a refusal");
            assert!(refusal.contains("verification budget"), "{refusal}");
            assert!(crate::is_passing_refusal(&refusal), "{refusal}");
        }
    }

    /// A helper that prints more than a pipe holds is read while it runs, so it
    /// ends and is collected rather than stalling against the wait until its
    /// deadline.
    #[cfg(unix)]
    #[test]
    fn a_helper_printing_more_than_a_pipe_holds_is_collected() {
        let out = output_bounded(
            Command::new("/bin/sh")
                .arg("-c")
                .arg("head -c 300000 /dev/zero; head -c 300000 /dev/zero >&2"),
            "a chatty helper",
        )
        .expect("a chatty helper runs to its exit");
        assert!(out.status.success());
        assert_eq!((out.stdout.len(), out.stderr.len()), (300_000, 300_000));
    }
}
