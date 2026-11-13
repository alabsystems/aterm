// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0

//! Authenticity verification of a candidate `.app` at stage time and apply time.

use aterm_uds::deadline::RunError;
use aterm_uds::spawnfd::Command;
use std::path::Path;
use std::time::{Duration, Instant};

const HELPER_TIMEOUT: Duration = Duration::from_secs(60);

const HELPER_REAP: Duration = Duration::from_millis(250);

/// The aggregate apply window matches the ordinary native-helper window.
/// In-session authentication runs before readers park, so it has no shorter UI latency cap.
pub const APPLY_BUDGET: Duration = HELPER_TIMEOUT;

thread_local! {
    static APPLY_DEADLINE: std::cell::Cell<Option<Instant>> =
        const { std::cell::Cell::new(None) };
}

/// Opens an aggregate verification budget for the current thread.
pub struct ApplyBudget(Option<Instant>);

impl ApplyBudget {
    /// Start a budget of `limit`, replacing any budget already in force.
    pub fn start(limit: Duration) -> Self {
        Self::start_at(limit, Instant::now())
    }

    fn start_at(limit: Duration, started: Instant) -> Self {
        Self(APPLY_DEADLINE.with(|d| d.replace(Some(crate::instant_after(started, limit)))))
    }

    /// Push the deadline out by `elapsed`.
    pub fn extend(elapsed: Duration) {
        APPLY_DEADLINE.with(|d| {
            if let Some(deadline) = d.get() {
                d.set(Some(crate::instant_after(deadline, elapsed)));
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

#[cfg(test)]
thread_local! {
    /// The pid of the helper [`status_bounded_with_stderr_observed`] last started on this thread,
    /// recorded as it starts, so a test can see that helper gone once the call returns.
    static HELPER_STARTED: std::cell::Cell<Option<u32>> = const { std::cell::Cell::new(None) };
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
    let deadline = crate::instant_after(Instant::now(), limit);
    let cleanup_reserve = Duration::from_millis(250).min(limit / 4);
    let work_deadline = deadline.checked_sub(cleanup_reserve).unwrap_or(deadline);
    let cleanup_deadline = || deadline.min(crate::instant_after(Instant::now(), cleanup_reserve));
    let mut child = cmd
        .stdin(Stdio::null())
        .stderr(Stdio::piped())
        .spawn()
        .map_err(|error| HelperFailure {
            message: spawn_refusal(what, &error),
            writer_stopped: true,
        })?;
    #[cfg(test)]
    HELPER_STARTED.with(|started| started.set(Some(child.id())));
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
    helper_deadline_started(Instant::now())
}

fn helper_deadline_started(started: Instant) -> (Instant, bool) {
    let own = started + HELPER_TIMEOUT;
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
    output_bounded_started(cmd, what, Instant::now())
}

fn output_bounded_started(
    cmd: &mut Command,
    what: &str,
    started: Instant,
) -> Result<std::process::Output, String> {
    let (deadline, bound_by_budget) = helper_deadline_started(started);
    let timeout = timed_out(what, bound_by_budget);
    if Instant::now() >= deadline {
        return Err(timeout);
    }
    output_until(cmd, what, || (deadline, timeout))
}

/// [`output_bounded`] with an explicit `limit` instead of the helper timeout and the apply budget.
pub(crate) fn output_within(
    cmd: &mut Command,
    what: &str,
    limit: Duration,
) -> Result<std::process::Output, String> {
    output_until(cmd, what, || {
        (
            crate::instant_after(Instant::now(), limit),
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
        RunError::Capture(e) => mark_passing(format!("capture output from started {what}: {e}")),
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
            if kind.is_file() {
                continue;
            }
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

/// Read the installed plist through the same open file whose identity the cache observed.
pub(crate) fn bundle_build_number_from_file(file: std::fs::File) -> Result<u64, String> {
    let mut cmd = aterm_uds::spawnfd::command("/usr/libexec/PlistBuddy");
    cmd.inherit_guards(vec![file.into()])
        .map_err(|error| spawn_refusal("PlistBuddy file guard", &error))?;
    let fd = cmd
        .guard_targets()
        .next()
        .ok_or_else(|| "PlistBuddy file guard is missing".to_string())?;
    cmd.arg("-c")
        .arg("Print :CFBundleVersion")
        .arg(format!("/dev/fd/{fd}"));
    build_number_from(output_bounded(&mut cmd, "PlistBuddy CFBundleVersion")?)
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
    crate::parse_uint::<u64>(text)
        .ok_or_else(|| format!("CFBundleVersion {text:?} is not an integer"))
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
#[path = "verify_tests.rs"]
mod tests;
#[test]
fn capture_failure_after_start_is_an_unanswered_helper_not_spawn_refusal() {
    let refusal = run_refusal(
        "codesign",
        RunError::Capture(std::io::Error::new(
            std::io::ErrorKind::NotFound,
            "capture setup failed after dispatch",
        )),
        String::new(),
    );
    assert!(refusal.contains("capture output from started codesign"));
    assert!(!refusal.contains("spawn codesign"));
    assert!(crate::is_passing_refusal(&refusal));
}
