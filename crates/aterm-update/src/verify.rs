// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0

//! Authenticity verification of a candidate `.app`, run both at stage time and
//! again at apply time (TOCTOU defence — the staged copy sits on disk between the
//! two). It asks the same questions the CUTTER asks at notarize time —
//! `crates/aterm-release/src/sign.rs` (codesign verification, `stapler
//! validate`, `spctl` assessment) — in order of cheapest/most-local first, and
//! fails CLOSED: any error, non-zero exit, or unparseable output is a
//! rejection.

use aterm_uds::deadline::RunError;
use aterm_uds::spawnfd::Command;
use std::path::Path;
use std::time::{Duration, Instant};

/// Wall-clock ceiling for one verification helper.
///
/// Every helper here is an external process on the APPLY path, and the apply path
/// runs at the very top of `main()` before the window exists — so a helper that
/// never returns is a terminal that never starts. `spctl` is the one that makes
/// this concrete rather than theoretical: Gatekeeper assessment can reach out to
/// Apple's notarization service, and on a captive-portal / half-open network that
/// call can hang far past any useful bound. `codesign` and `PlistBuddy` are local
/// and finish in tens of milliseconds, but they share the ceiling because a hung
/// child is a hung child.
///
/// 30s is far above any healthy run and far below "the user thinks the app is
/// broken". Bundle size does not threaten it: on an 851 MB seeded bundle
/// `codesign --verify --deep --strict` measured 0.09 s wall and `spctl` 0.14-0.21 s;
/// reaching 30 s would need reads under ~27 MB/s across the payload.
const HELPER_TIMEOUT: Duration = Duration::from_secs(30);

/// How long a timed-out helper is given to be reaped after its SIGKILL.
const HELPER_REAP: Duration = Duration::from_millis(250);

/// The AGGREGATE ceiling on one apply's verification work — see [`ApplyBudget`].
///
/// [`HELPER_TIMEOUT`] bounds ONE helper. It does not bound an apply, and an apply
/// is not one helper: a staged swap runs eleven of them (two
/// `verified_bundle_identity` calls at five each — the staged and the installed
/// bundle, each re-verified at the point of use; a cross-volume install adds the
/// copied one — plus the start probe). Every one of those is fail-closed, so
/// timeouts cannot actually accumulate — the first one aborts the apply — but that
/// is a property of the callers, argued across four files; this states the
/// ceiling as one number.
///
/// 8s is that number. Measured against the real signed bundle the whole sequence
/// is well under a second (one bundle's five helpers run at once,
/// [`check_bundle`]: codesign -R 0.074s, codesign -d 0.017s, spctl 0.080s,
/// PlistBuddy 0.003s — and the boot apply runs the staged and installed
/// bundles' ten at once, [`check_bundles`]; the start probe ~0.008s once the
/// stage ran it, [`probe_staged_bundle_starts`]), so 8s is many times the headroom of a healthy
/// machine and still short enough that a user who double-clicked does not
/// conclude the app is broken. Expiry defers the apply: the window opens on the
/// build already installed, and the next launch tries again.
pub const APPLY_BUDGET: Duration = Duration::from_secs(8);

thread_local! {
    /// The wall-clock instant this thread's apply must be finished verifying by,
    /// or `None` outside an apply. Thread-local rather than threaded through ten
    /// call sites because an apply IS one thread — it runs at the top of `main`,
    /// before any window or worker exists.
    static APPLY_DEADLINE: std::cell::Cell<Option<Instant>> =
        const { std::cell::Cell::new(None) };
}

/// Opens an aggregate verification budget for the current thread, restoring the
/// previous one on drop.
///
/// While it is live every helper spawned by this module is bounded by whichever
/// is SOONER: its own [`HELPER_TIMEOUT`], or what remains of the budget. That is
/// what turns "each of sixteen helpers is individually bounded" into "the apply
/// is bounded", which is the property the launch path actually needs — the apply
/// runs before the window exists, so its total latency is time the user spends
/// looking at a bouncing Dock icon.
pub struct ApplyBudget(Option<Instant>);

impl ApplyBudget {
    /// Start a budget of `limit`, replacing any budget already in force.
    pub fn start(limit: Duration) -> Self {
        Self(APPLY_DEADLINE.with(|d| d.replace(Some(Instant::now() + limit))))
    }

    /// Push the deadline out by `elapsed`, for work that is legitimately slow and
    /// separately bounded — a cross-volume `ditto` of a several-hundred-megabyte
    /// bundle, whose duration is a property of the disk, not of a wedged helper.
    /// Without this, one slow copy would spend the whole budget and fail the
    /// verification that follows it on a machine that is behaving perfectly.
    pub fn extend(elapsed: Duration) {
        APPLY_DEADLINE.with(|d| {
            if let Some(deadline) = d.get() {
                d.set(Some(deadline + elapsed));
            }
        });
    }
}

/// The deadline this thread's verification must finish by, or `None` outside a
/// budget — for the tests that pin a lane to running under one.
#[cfg(test)]
pub(crate) fn current_budget_deadline() -> Option<Instant> {
    APPLY_DEADLINE.with(std::cell::Cell::get)
}

impl Drop for ApplyBudget {
    fn drop(&mut self) {
        APPLY_DEADLINE.with(|d| d.set(self.0));
    }
}

/// How much of a child's stderr [`status_bounded_with_stderr_observed`] keeps: the LAST
/// bytes, because a diagnostic is the last thing a failing copy prints.
const STDERR_KEEP: usize = 512;

/// Failure of an owned helper, including whether its absence was actually
/// observed. A kill request alone is not an exit/reap witness.
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

/// Bounded run of a child whose stdout is worth nothing but whose STDERR is —
/// today `ditto`, whose copy can legitimately run for minutes and therefore gets
/// its own explicit `limit` rather than [`HELPER_TIMEOUT`] or the apply budget.
///
/// A timeout is an `Err`. Termination and reaping use only a short reserve from
/// the original budget; the typed internal result distinguishes an observed exit
/// from unconfirmed cleanup so callers do not reclaim a live writer's paths.
///
/// The tail of stderr rides back with the status so a failure can say WHY: a bare
/// exit status reads the same for a full disk, a corrupt archive and a read-only
/// volume.
///
/// The stream is drained on its own thread, so a child that writes more than the
/// pipe holds cannot wedge against the wait; only the tail is kept. The wait
/// ends at the child's exit as the kernel reports it
/// ([`aterm_uds::deadline::ExitWait`]). The returned string is empty when the
/// child said nothing.
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
                // A copy that outran its own ceiling says the disk was slow
                // (a cross-volume `ditto` of a large bundle on a busy volume),
                // not that the bundle is bad: passing.
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
///
/// Everything else this module checks is cryptographic — the signature, the Team ID,
/// the sealed identity, the Gatekeeper assessment. None of it establishes that the
/// binary will load: a wrong-architecture slice, a missing dylib, a Developer ID
/// Apple revoked after we verified, anything the kernel or dyld rejects before
/// `main`. That case is the one the crash-loop sentinel cannot cover, because the
/// sentinel lives inside the new build's own `main` (`install::check_boot_health`):
/// a build that never reaches `main` never counts a launch, never reaches
/// `MAX_BOOT_ATTEMPTS`, and never reverts — leaving a bricked app beside a perfectly
/// good predecessor at `aterm.app.rollback`.
///
/// So: run the candidate's own executable with `--version` before the swap and
/// require a clean exit that names the build we are about to install. `--version` is
/// the cheapest total path through the binary — it loads every linked library, then
/// prints and exits without touching config, sockets, PTYs or the updater.
///
/// Bounded like every other helper here, and fail-closed: a probe that times out,
/// crashes, or prints the wrong build refuses the apply. The machine keeps the build
/// it has, which is always a safe outcome; a bricked app is not.
///
/// The stage ran it once already ([`probe_staged_bundle_starts`]), so on the
/// same-volume path this run finds the first-exec assessment done.
#[cfg(target_os = "macos")]
pub fn probe_bundle_starts(app: &Path, expected_build: u64) -> Result<(), String> {
    probe_starts(app, expected_build, |exe| {
        output_bounded(
            aterm_uds::spawnfd::command(exe).arg("--version"),
            START_PROBE,
        )
    })
}

/// [`probe_bundle_starts`] at stage time, on the checker thread, before the ready
/// marker.
///
/// The first exec of a binary this machine has never run pays
/// syspolicyd's assessment — measured 0.75 s (0.75-0.95, n=5) for the 90 MB `aterm`
/// against ~8 ms for every exec after, and that on a cdhash syspolicyd had already
/// seen; a genuinely new release also gets the XProtect scan — and nothing else
/// here warms it: `codesign -R` and `spctl` passing do not. The verdict follows the
/// executable's inode through a rename (measured: 8 ms after renaming the bundle),
/// and the same-volume apply moves the staged inode by exactly a rename and an
/// exchange. So the assessment is paid here, off every path a person waits on,
/// instead of in the apply's own probe — before the successor's window in the
/// switch, before any window in a cold launch's boot apply. That probe still runs:
/// it is the TOCTOU check, now at the warm cost. A cross-volume install copies the
/// bundle (new inodes) and pays at its apply.
///
/// Never started before the gate passes. macOS assesses the first exec of a file
/// after the exec, by path, while the process waits at `_dyld_start`, and killing the
/// child does not end the assessment; on macOS 27.0.1 `syspolicyd` crashes (`SIGSEGV`)
/// when a Mach-O it began assessing is unlinked before the assessment finishes, which
/// strands every exec on the host at `_dyld_start`. Starting the candidate before the
/// gate (`POSIX_SPAWN_START_SUSPENDED`) and unlinking `incoming` on a gate refusal or
/// withdrawal unlinks an assessed file before it ever ran. Running the probe only
/// after the whole gate (`check_bundle`, `CFBundleVersion`, `ATermGitCommit`, and
/// `stage_withdrawn`) has passed means a refused bundle was never exec'd at all.
/// `--version` answers before any lane starts, so the candidate never reaches its
/// updater, trial counters or state roots.
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
    /// The `errno` the next stage probe on this thread is refused its spawn with, instead
    /// of starting the candidate: how a test stages a probe that runs to no answer
    /// (`EAGAIN`, the process table full just then) or to a lasting refusal (`EACCES`)
    /// without racing a deadline against the gate in front of it.
    pub(crate) static STAGE_PROBE_SPAWN_REFUSAL: std::cell::Cell<Option<i32>> =
        const { std::cell::Cell::new(None) };
    /// Whether the next stage probe on this thread reports a helper timeout
    /// ([`timed_out`]), standing in for a spawned candidate still waiting at
    /// `_dyld_start` when its ceiling expires.
    pub(crate) static STAGE_PROBE_TIMED_OUT: std::cell::Cell<bool> =
        const { std::cell::Cell::new(false) };
}

/// What the start probe is called in its refusals.
#[cfg(target_os = "macos")]
const START_PROBE: &str = "candidate --version probe";

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
    // `--version` answers before any lane starts, so the candidate never reaches
    // its updater: no switch is needed to keep the probe inert.
    let out = run(&exe)?;
    if !out.status.success() {
        return Err(format!(
            "the staged bundle could not start ({}): {}",
            out.status,
            crate::quote_tool_output(String::from_utf8_lossy(&out.stderr).trim())
        ));
    }
    // …and it must be the build we are installing. A probe that starts SOME binary
    // proves nothing about this one. A current candidate prints `build: <N>`; the
    // bundle's own `CFBundleVersion`, which the codesign seal ties to the binary
    // that just ran, answers for an older candidate (a yank, a rollback sibling)
    // whose identity line has no build number.
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

/// Whichever ceiling binds first: a helper's own [`HELPER_TIMEOUT`], or what is left of
/// the apply's aggregate budget; `true` when the budget is the one that binds. Outside an
/// apply there is no budget and the helper timeout stands alone.
fn helper_deadline() -> (Instant, bool) {
    let own = Instant::now() + HELPER_TIMEOUT;
    match APPLY_DEADLINE.with(|d| d.get()) {
        Some(b) if b < own => (b, true),
        _ => (own, false),
    }
}

/// The rejection a helper that outran [`helper_deadline`] reports.
///
/// The apply refuses — nothing is swapped past a helper that did not answer —
/// and the reason carries [`crate::PASSING_REFUSAL_KEY`], because a deadline
/// missed says the machine was slow at that moment (a loaded desk, a cold page
/// cache, `spctl` waiting on the network), not that the bundle is bad. Its
/// readers retry it on the transient schedule instead of converging the build as
/// structural or asking a person to reinstall.
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

/// `message`, marked as a passing refusal ([`crate::PASSING_REFUSAL_KEY`]). The one
/// spelling, so every cause reads the same in the log and the key is never
/// reworded at one site and missed at another.
pub(crate) fn mark_passing(message: String) -> String {
    format!("{message} ({})", crate::PASSING_REFUSAL_KEY)
}

/// Whether an I/O error from starting a helper, or from waiting on a lock, is a
/// fact about this MOMENT rather than about the bundle or the machine's setup.
///
/// * `TimedOut` — `FileLock::acquire_within`'s answer when another process held
///   the lock past the bounded wait; the holder lets go or is reaped.
/// * `EAGAIN` / `ENOMEM` — `fork`/`posix_spawn` refused for the process table or
///   memory at that instant (Rust reports `EAGAIN` as `WouldBlock`, `ENOMEM` as
///   `OutOfMemory`).
/// * `EMFILE` / `ENFILE` — no descriptor for the helper's pipes just then.
///
/// Everything else keeps its verdict: `ENOENT` (no `/usr/bin/codesign`), `EACCES`,
/// `ENOLCK`, an unsupported filesystem — those come back the same every time.
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

/// The refusal for an I/O error while starting helper `what`, marked passing when
/// the error is a moment ([`io_error_is_passing`]).
pub(crate) fn spawn_refusal(what: &str, error: &std::io::Error) -> String {
    let message = format!("spawn {what}: {error}");
    if io_error_is_passing(error) {
        mark_passing(message)
    } else {
        message
    }
}

/// The refusal a bounded wait on the apply lock reports, under `what` (the lane's
/// own prefix: "lock", "pre-verify lock", "health lock", "publish lock").
///
/// A lock another process held past the wait is PASSING: the holder is a sibling
/// launch mid-swap, a stage publishing, or a process stopped under a debugger —
/// each lets go or is reaped, and the next attempt takes the lock. Any OTHER lock error
/// (`ENOLCK`, an unsupported filesystem) is not a moment and keeps its verdict.
pub(crate) fn lock_wait_refusal(what: &str, error: &std::io::Error) -> String {
    let message = format!("{what}: {error}");
    if io_error_is_passing(error) {
        mark_passing(message)
    } else {
        message
    }
}

/// Run a verification helper with a bounded wall clock, killing it on timeout.
///
/// Fails CLOSED like every other check here: a timeout is an `Err`, i.e. a
/// rejection, never a pass. `what` names the helper for the error text.
///
/// Both streams are drained while the child runs and their tails kept
/// ([`aterm_uds::deadline::outputs_until`]), so a chatty child cannot wedge on a
/// full pipe; these helpers print a few hundred bytes at most.
fn output_bounded(cmd: &mut Command, what: &str) -> Result<std::process::Output, String> {
    output_until(cmd, what, || {
        let (deadline, bound_by_budget) = helper_deadline();
        (deadline, timed_out(what, bound_by_budget))
    })
}

/// [`output_bounded`] with an explicit `limit` instead of the helper timeout and
/// the apply budget: for a helper on the CHECK thread whose legitimate run is
/// longer than a verification helper's, but must still end — `hdiutil attach`
/// verifies a whole image's checksums; `hdiutil detach -force` of a wedged mount
/// can wait on the disk arbitration daemon indefinitely. A timeout is an `Err`
/// and the child is killed and reaped.
///
/// A limit run out is PASSING ([`mark_passing`]), like
/// [`timed_out`]: a helper that did not finish says the machine was slow at that
/// moment, not that the image is bad, and the stage path retries it rather than
/// booking a backoff against a healthy build.
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

/// Run `cmd` to its output by the deadline `bound` names, killing it there and
/// reaping it within [`HELPER_REAP`]; `bound` also gives the timeout's error text.
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

/// What one helper run gave: its output, or why there is none.
type Run = Result<std::process::Output, RunError>;

/// The refusal for helper `what` that gave no output: `timeout` for one past its
/// deadline — killed, and a rejection ([`timed_out`]) whether or not its reap was
/// seen within [`HELPER_REAP`]: a helper still wedged in the kernel past SIGKILL is
/// left to exit on its own rather than parking this thread on it, which is the
/// whole point of the bound, and nothing is swapped past a helper that may still
/// hold a file.
fn run_refusal(what: &str, error: RunError, timeout: String) -> String {
    match error {
        RunError::Spawn(e) => spawn_refusal(what, &e),
        // Waiting on a child that ran is no verdict on what it read (as for
        // codesign: [`codesign_refusal`]).
        RunError::Wait(e) => mark_passing(format!("wait for {what}: {e}")),
        RunError::TimedOut => timeout,
    }
}

/// A bundle's policy gate and its sealed reads, with every helper started at once.
///
/// Started together and reaped together under one deadline, on this thread with
/// no other started ([`aterm_uds::deadline::outputs_until`]), the helpers (`codesign -R`,
/// `codesign -d`, `spctl`, and `PlistBuddy` for each read) cost the slowest
/// of them (~80 ms vs ~185 ms serial on a quiet machine).
///
/// Each field is what its own helper says, and
/// every caller reads them in the serial order — [`Self::policy`] first — so the
/// first refusal it meets is the one the serial gate would have returned, wording
/// and passing classification included. A read that ran beside the gate instead of
/// after it read the same bytes: nothing writes the bundle while it is verified (a
/// private staging parent and the apply lock), and an Info.plist value is still
/// sealed evidence only once [`Self::policy`] is `Ok`. A helper that outlives the
/// deadline is killed and refused ([`timed_out`]) like a serial one.
#[derive(Debug)]
pub struct BundleChecks {
    /// [`verify_bundle_policy`]'s verdict.
    pub policy: Result<(), String>,
    /// [`bundle_build_number`]'s answer.
    pub build: Result<u64, String>,
    /// The sealed `ATermGitCommit` ([`git_commit_from`]).
    pub commit: Result<String, String>,
    /// [`team_id`]'s answer when a Team ID is pinned (the gate reads it); `None` in
    /// the structural tier.
    pub team: Option<Result<String, String>>,
    /// [`bundle_short_version`]'s answer, when asked for.
    pub short_version: Option<Result<String, String>>,
}

/// [`BundleChecks`] of `app` under `expected_team` (the tier [`verify_bundle_policy`]
/// takes), reading `CFBundleShortVersionString` too when `short_version`.
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

/// [`check_bundle`] of every one of `apps`, in order, with ALL their helpers started
/// at once and reaped under one deadline — the apply's staged and installed bundles
/// cost the slower of the two checks, not their sum. Each bundle's answers are its own
/// helpers', composed exactly as [`check_bundle`] composes them.
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

/// The answer for a check that never ran — never returned in practice: the gates
/// answer every bundle they are given.
fn not_run<T>() -> Result<T, String> {
    Err("the bundle check did not run".to_string())
}

/// One run of the policy gate beside `PlistBuddy` reads of `keys`.
struct Gate {
    policy: Result<(), String>,
    team: Option<Result<String, String>>,
    /// Each key's `PlistBuddy` output, in `keys`' order.
    reads: Vec<Result<std::process::Output, String>>,
}

/// The helpers one bundle's [`Gate`] runs: the policy gate — `codesign -R`,
/// `codesign -d` (the team) and `spctl` for a `pinned` Team ID, else the structural
/// `codesign --verify` — and a `PlistBuddy` read of each key.
struct GateHelpers {
    seal: Command,
    display: Option<Command>,
    spctl: Option<Command>,
    plist: Vec<Command>,
}

impl GateHelpers {
    /// `app`'s helpers, or the gate's answer when a pin cannot make a requirement or
    /// the bundle holds an entry no helper could read to an answer
    /// ([`unanswerable_entry`], walked until `deadline`) — refused before anything runs.
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

    /// The helpers in the order [`compose_gate`] reads their runs.
    fn commands(&mut self) -> impl Iterator<Item = &mut Command> {
        std::iter::once(&mut self.seal)
            .chain(self.display.as_mut())
            .chain(self.spctl.as_mut())
            .chain(self.plist.iter_mut())
    }
}

/// Why no helper could read `app` to an answer, or `None` when nothing found stops
/// one: an entry that is not a directory or a regular file (a FIFO, a socket, a
/// device), a symlink to one, or a symlink to a directory outside the bundle. codesign
/// opens what such an entry names and waits there — a FIFO no writer opens holds it for
/// good. Measured with `/usr/bin/codesign --verify --deep --strict`: a FIFO at
/// `Contents/_CodeSignature/CodeResources` or at the main executable, a symlink to one
/// in place of a sealed resource, and `Contents`, `Contents/MacOS` or
/// `Contents/_CodeSignature` linked to a directory outside holding one each held it
/// past every deadline. A run past its deadline is a PASSING refusal ([`timed_out`]), so
/// a bundle tampered this way would be kept and re-verified as a stage, and as the
/// installed bundle would never ask for a reinstall. A signed bundle holds none of
/// these (where codesign does answer, it says "unsupported resource found" or, under
/// `--strict`, "invalid destination for symbolic link in bundle"), so this is a
/// verdict, and the entry's name, which the bundle chose, is quoted
/// ([`crate::quote_tool_output`]).
///
/// Checks `deadline` between directories and entries. The filesystem operations are
/// `lstat` (`DirEntry::file_type`), one `stat` of a symlink's target and, for a
/// directory, its real path; each can still block in the filesystem. Never descends
/// through a symlink (a directory inside the bundle is walked under its own name).
/// What it did not reach, or could not list or resolve, is codesign's to judge.
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
    // The bundle's tree as its links resolve: codesign follows `app` itself.
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
                // A dangling link is codesign's verdict to give.
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

/// The policy gate and the reads of `keys` for each of `apps` ([`GateHelpers`]), every
/// helper of every bundle started at once under one [`helper_deadline`]. Each verdict
/// composes its own bundle's answers in [`verify_bundle`]'s serial order: the seal,
/// then the team (and its pin), then Gatekeeper.
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

/// One bundle's (`app`'s) [`Gate`] from its helpers' runs, taken from `runs` in
/// [`GateHelpers::commands`]' order.
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

/// [`gates_and_reads`] of one bundle.
fn gate_and_read(app: &Path, pinned: Option<&str>, keys: &[&str]) -> Gate {
    gates_and_reads(&[app], pinned, keys)
        .pop()
        .unwrap_or_else(|| Gate {
            policy: not_run(),
            team: None,
            reads: Vec::new(),
        })
}

/// Full apply-/stage-time gate: structural codesign seal, Team-ID pin, and
/// Gatekeeper/notarization acceptance. `expected_team` is [`crate::PINNED_TEAM_ID`].
/// Returns `Ok(())` only if every check passes. The three helpers run at once
/// ([`check_bundle`] says why the verdict is the serial one).
pub fn verify_bundle(app: &Path, expected_team: &str) -> Result<(), String> {
    gate_and_read(app, Some(expected_team), &[]).policy
}

/// Tiered bundle-authenticity gate (see the crate trust model). When a Team ID is
/// pinned, run the full Developer-ID [`verify_bundle`] (Tier APPLE). Otherwise run only
/// the **structural** `codesign --verify` (Tier REPO): the seal must be intact — which
/// also requires the bundle be at least ad-hoc signed, as arm64 needs to run — but the
/// signer is unconstrained, because authenticity in that tier comes from the
/// authenticated private repo + sha256 (and, if configured, the signed manifest checked
/// in `github.rs`), not from the Apple chain. Fails CLOSED on any codesign error.
pub fn verify_bundle_policy(app: &Path, expected_team: &str) -> Result<(), String> {
    if expected_team.is_empty() {
        gate_and_read(app, None, &[]).policy
    } else {
        verify_bundle(app, expected_team)
    }
}

/// The structural lane's name in its refusals.
const STRUCTURAL: &str = "codesign --verify (structural)";

/// `codesign --verify --deep --strict` with NO designated requirement: proves the
/// signature seal is intact and nothing in the bundle changed since signing, but does
/// not constrain the signer (an ad-hoc signature passes). The default-tier check.
fn structural_command(app: &Path) -> Command {
    let mut cmd = aterm_uds::spawnfd::command("/usr/bin/codesign");
    cmd.args(["--verify", "--deep", "--strict", "--verbose=2"])
        .arg(app);
    cmd
}

/// What one ended [`structural_command`] run on `app` says.
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

/// `PlistBuddy -c "Print :<key>"` of `app`'s `Contents/Info.plist`.
fn plist_command(app: &Path, key: &str) -> Command {
    let mut cmd = aterm_uds::spawnfd::command("/usr/libexec/PlistBuddy");
    cmd.arg("-c")
        .arg(format!("Print :{key}"))
        .arg(app.join("Contents/Info.plist"));
    cmd
}

/// Read `CFBundleVersion` (the monotonic build number) from a staged `.app`'s
/// `Contents/Info.plist`. This binds the UNAUTHENTICATED manifest `build_number` to
/// the number actually baked into the signed bundle, closing a downgrade/replay hole:
/// without it, a repo-write adversary could publish a manifest claiming a huge
/// `build_number` that points at an OLD but genuinely-signed DMG (whose real sha256
/// they also list), passing every signature/Team-ID/notarization check while
/// re-installing since-patched code. Fails CLOSED — any error or unparseable value is
/// a rejection. The plist value is signed-in (codesign seals `Contents/`), so reading
/// it after `verify_bundle` means it can't have been tampered post-signing.
pub fn bundle_build_number(app: &Path) -> Result<u64, String> {
    build_number_from(output_bounded(
        &mut plist_command(app, "CFBundleVersion"),
        "PlistBuddy CFBundleVersion",
    )?)
}

/// The refusal for a `PlistBuddy` read of `key` that did not succeed: its words
/// (quoted, [`crate::quote_tool_output`]), marked passing ([`mark_passing`]) when a signal ended the read. A read killed on
/// a loaded desk said nothing about the plist, as a killed `codesign` or `spctl`
/// says nothing about the bundle ([`codesign_exit_refusal`], [`spctl_verdict`]);
/// as a verdict it would discard a healthy stage or a predecessor's revert. Every
/// read that ran to its exit keeps its verdict.
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

/// What one `PlistBuddy` read of `CFBundleVersion` says ([`bundle_build_number`]).
fn build_number_from(out: std::process::Output) -> Result<u64, String> {
    if !out.status.success() {
        return Err(plist_read_refusal("CFBundleVersion", &out));
    }
    let text = String::from_utf8_lossy(&out.stdout);
    let text = text.trim();
    text.parse::<u64>()
        .map_err(|_| format!("CFBundleVersion {text:?} is not an integer"))
}

/// Read the marketing version (`CFBundleShortVersionString`) from a bundle's sealed
/// `Contents/Info.plist` — display only (the update screen names the build it is about
/// to activate); authority is always the build number and the sealed commit.
pub fn bundle_short_version(app: &Path) -> Result<String, String> {
    short_version_from(output_bounded(
        &mut plist_command(app, "CFBundleShortVersionString"),
        "PlistBuddy CFBundleShortVersionString",
    )?)
}

/// What one `PlistBuddy` read of `CFBundleShortVersionString` says
/// ([`bundle_short_version`]).
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

/// What one `PlistBuddy` read of `ATermGitCommit` says: the codesign-sealed source
/// provenance in Info.plist ([`BundleChecks::commit`]). Callers compare it through
/// [`crate::commit_matches`]; this reader only enforces a small, valid UTF-8 value so
/// process output cannot become an allocation surface.
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

/// The refusal a failed team-pinned `codesign --verify` reports — `codesign -R` with
/// the **Apple anchor + Developer-ID chain + our Team ID** requirement, the one
/// requirement text, [`aterm_update_core::codesign`]. The requirement makes codesign
/// itself reject anything not signed by a genuine Apple-issued Developer-ID
/// Application cert whose Team (OU) equals the pin, so the authenticity anchor does
/// not rest solely on `spctl`/Gatekeeper, which a Gatekeeper-disabled machine turns
/// into a no-op.
///
/// Split out so its classification is provable without a codesign that misbehaves on
/// cue: always a rejection, and a PASSING one ([`mark_passing`]) only when codesign
/// knowably reached no verdict on the bytes of `app` ([`codesign_exit_refusal`]).
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
        // A refusal unless codesign knowably judged nothing: see
        // [`codesign_exit_refusal`], which the structural lane and the team-id read
        // share.
        refused @ CodesignError::Refused { .. } => {
            codesign_exit_refusal("codesign --verify (team-pinned requirement)", app, refused)
        }
        CodesignError::TimedOut => timed_out(WHAT, bound_by_budget),
        CodesignError::Spawn(e) => spawn_refusal(WHAT, &e),
        // Waiting on a child that ran is no verdict on what it read.
        CodesignError::Wait(e) => mark_passing(format!("wait for {WHAT}: {e}")),
        e @ CodesignError::Unsupported => format!("{WHAT}: {e}"),
    }
}

/// The refusal for a codesign run on `app` that ENDED (`refused`, a
/// [`CodesignError::Refused`]) under `what`: codesign's words, quoted
/// ([`crate::quote_tool_output`]), marked passing ([`mark_passing`]) only when
/// codesign knowably judged nothing.
///
/// A run that ended without a verdict is not one. codesign killed by a signal
/// (jetsam on a loaded desk), or exiting 1 for a file it could not read or
/// "internal error in Code Signing subsystem", says nothing about the bundle; as a
/// verdict it would ask a person to reinstall a healthy app or discard a healthy
/// stage. The rejection stands either way — nothing is swapped past a check that
/// did not pass — but its readers retry it.
///
/// Fail closed: only [`CodesignError::judged_nothing_at`] — a signal, or exit 1
/// whose first line names only a failure to read or to run on `app` itself — is
/// passing. Every other exit is a verdict, however codesign words it ("a sealed
/// resource is missing or invalid", "invalid Info.plist", a dangling symlink's "No
/// such file or directory", exit 2's usage error, exit 3's requirement, words no list
/// knows, a machine fault on a line naming a path the bundle chose), so a tampered
/// stage is never kept and re-verified and a tampered installed bundle still asks for
/// a reinstall.
///
/// [`CodesignError::Refused`]: aterm_update_core::codesign::CodesignError::Refused
/// [`CodesignError::judged_nothing_at`]: aterm_update_core::codesign::CodesignError::judged_nothing_at
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

/// Extract the signing **Team Identifier** from a bundle. `codesign -dv` writes its
/// descriptive output to **stderr**, hence the merge; we scan for the
/// `TeamIdentifier=...` line. A `not set` value (ad-hoc / unsigned) is rejected.
pub fn team_id(app: &Path) -> Result<String, String> {
    parse_team_id(&codesign_display(app, "codesign -d (team id)")?)
}

/// The code-signing identifier `app` is signed with (`codesign -d`'s
/// `Identifier=`): the identifier a code requirement's `identifier "…"` clause
/// tests. A display, not a verification — read it from a bundle a check has
/// just passed. A run that ended on a moment is a passing refusal, as for
/// [`team_id`].
pub fn signing_identifier(app: &Path) -> Result<String, String> {
    parse_signing_identifier(&codesign_display(app, "codesign -d (identifier)")?)
}

/// `codesign -d --verbose=4 <app>`'s text, both streams (it prints to stderr
/// whether or not it succeeds), bounded like every helper here.
fn codesign_display(app: &Path, what: &str) -> Result<String, String> {
    display_from(what, app, &output_bounded(&mut display_command(app), what)?)
}

/// `codesign -d --verbose=4 <app>`.
fn display_command(app: &Path) -> Command {
    let mut cmd = aterm_uds::spawnfd::command("/usr/bin/codesign");
    cmd.args(["-d", "--verbose=4"]).arg(app);
    cmd
}

/// What one ended [`display_command`] run (`what`) on `app` printed, or its refusal.
fn display_from(what: &str, app: &Path, out: &std::process::Output) -> Result<String, String> {
    // A run that ended on a MOMENT — a signal, or a file it could not read just then —
    // reached no verdict, and its output is no evidence of a missing TeamIdentifier.
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

/// Pure parser for the `Identifier=` line of `codesign -dv` output. Fails closed
/// on a missing or empty identifier.
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

/// Pure parser for the `TeamIdentifier=` line of `codesign -dv` output, split out
/// so the pin-matching logic is unit-testable without invoking codesign. Rejects a
/// missing or `not set` (ad-hoc/unsigned) identifier — fail closed.
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

/// `spctl -a -t exec` — Gatekeeper/notarization acceptance for a *runnable app*
/// (`-t exec`, not the DMG-install `-t install`). Reads the stapled notarization
/// ticket from the bundle, so it succeeds offline.
fn spctl_command(app: &Path) -> Command {
    let mut cmd = aterm_uds::spawnfd::command("/usr/sbin/spctl");
    cmd.args(["-a", "-t", "exec", "-vvv"]).arg(app);
    cmd
}

/// What one ended [`spctl_command`] run says ([`spctl_verdict`]).
fn spctl_from(out: &std::process::Output) -> Result<(), String> {
    spctl_verdict(out.status, &String::from_utf8_lossy(&out.stderr))
}

/// What one ended `spctl` run says: a pass, its rejection, or — when a signal ended
/// it — no verdict at all, a passing refusal (as liable to jetsam on a loaded desk
/// as `codesign`).
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
    ///
    /// A helper that did not finish is a passing refusal: still a rejection — nothing is
    /// swapped past it — but one its readers retry on the transient schedule, never a
    /// structural verdict and never a person's. Both deadlines that bind a helper say so:
    /// the apply's budget and the helper's own ceiling, and a copy's own `limit` as well.
    ///
    /// Control: a helper that ran and refused is a verdict and carries no key.
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

        // CONTROL: `false` ran and refused — the verdict shape, with no key.
        let refused = output_bounded(&mut Command::new("/usr/bin/false"), "a refusing helper")
            .expect("the helper ran to an exit");
        assert!(!refused.status.success());
        let verdict = format!(
            "codesign --verify (structural) failed: {}",
            String::from_utf8_lossy(&refused.stderr).trim()
        );
        assert!(!crate::is_passing_refusal(&verdict), "{verdict}");
    }

    /// Which I/O errors are a moment, by errno. The kernel refusing a helper for
    /// the process table, memory or descriptors just then — and a lock held past
    /// its wait — pass; a missing helper, a permission, a filesystem without locks
    /// come back the same every time and keep their verdict. The spawn itself is
    /// driven for the missing-binary case, so the classification is read off a
    /// real `spawn` error, not only a constructed one.
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
    fn a_check_helper_past_its_limit_is_killed_and_reported_promptly() {
        let error = output_within(
            Command::new("/bin/sleep").arg("30"),
            "sleep",
            Duration::from_millis(300),
        )
        .expect_err("a helper past its limit is a failure");
        // A limit run out is the machine's moment, not a verdict on the image:
        // the stage path retries it instead of booking
        // a backoff against a healthy build.
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
        /// A child that never exits (`exit` 0) counts the looks at it made once
        /// the cleanup's `deadline` had passed. One such look is the loop's own
        /// last (a poll that woke past the deadline, then saw it gone); a second
        /// is a cleanup that renewed its deadline, and the fake answers it with
        /// an error so a cleanup that would wait forever ends, and fails.
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
                // The cleanup ends at its deadline, never a renewed one: at most
                // the one look that found it passed — read off the calls, not a
                // clock, so a loaded machine cannot fail it — and inside a
                // minute's hang detector.
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
                    // Negative control: treating kill/wait without an observed exit as stopped violates the invariant.
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
        // Signal zero only queries this fixture's recorded process; it sends no signal.
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

    /// The TAIL of the child's stderr comes back with its status. A
    /// child that floods stderr is drained past the pipe's capacity rather than
    /// wedged against the poll loop, and only its last bytes are kept.
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

        // A flood: 128 KiB of stderr, past the 64 KiB pipe, then a clean exit.
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

        // Silence is an empty tail, not a failure.
        let (status, stderr) = status_bounded_with_stderr_observed(
            &mut Command::new("/usr/bin/true"),
            "a quiet copy",
            Duration::from_secs(60),
        )
        .expect("ran");
        assert!(status.success() && stderr.is_empty() && last_stderr_line(&stderr).is_none());
    }

    /// The signing identifier is the `Identifier=` line, never the
    /// `TeamIdentifier=` one that also ends in "Identifier=", and its absence
    /// fails closed.
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
        // ad-hoc / unsigned reports "not set".
        assert!(parse_team_id("TeamIdentifier=not set\n").is_err());
        // no line at all → reject (not silently accept).
        assert!(parse_team_id("Identifier=com.aterm.aterm\n").is_err());
        assert!(parse_team_id("TeamIdentifier=\n").is_err());
    }

    /// A garbage pin (one that could escape the requirement string) is refused before
    /// anything runs, and a bundle of another signer is a verdict.
    #[test]
    fn verify_bundle_refuses_a_garbage_pin_and_another_signer() {
        verify_bundle(Path::new("/nonexistent"), "AB\"CD").unwrap_err();
        // Judging Apple's `/bin/ls` waits on trustd.
        aterm_uds::deadline::require_trust_service();
        let refused = verify_bundle(Path::new("/bin/ls"), "ABCDE12345").unwrap_err();
        // A real verdict (exit 3, another signer) carries no passing key.
        assert!(!crate::is_passing_refusal(&refused), "{refused}");
    }

    /// A codesign run that ended without a verdict — a signal, a
    /// wait that failed, or exit 1 naming a fact about this machine (a file it could
    /// not read, its own internal error) — is a passing refusal on both codesign lanes.
    /// Every other exit is a verdict, worded as codesign words it and with no key:
    /// the signature faults, exit 3's requirement, exit 2's usage error, codesign's
    /// bundle-seal verdicts — and a line nobody measured, which must stay a verdict too
    /// ([`aterm_update_core::codesign::CodesignError::judged_nothing_at`] fails closed).
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

        // CONTROL: a verdict on the bytes stays a verdict, on either lane — the
        // tamper verdicts measured on a modified ad hoc signed bundle included.
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
            // Measured: a dangling symlink added to a sealed bundle.
            (Some(1), "aterm.app: No such file or directory\n"),
            // Measured: a symlink out of the bundle named after a machine fault, and
            // `--verbose=2` progress lines naming nested helpers the bundle called so.
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

    /// A tool cannot speak for the refusal that quotes it: a helper's output that
    /// echoes a refusal key (a nested path or a file the bundle named after it) leaves
    /// a verdict a verdict, and still reads the text it quoted. Every door that quotes
    /// codesign, spctl, PlistBuddy or the candidate's `--version`. Control: a moment
    /// stays a moment.
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

    /// A tampered bundle named like a machine fault is a verdict, measured on the real
    /// codesign through the structural lane on ad hoc signed `.app`s: a dangling symlink
    /// added after signing (codesign's whole word is `<app>: No such file or
    /// directory`); a symlink out of the bundle named `h: Permission denied`, read from
    /// stderr or both streams; and nested helpers named `h: Permission denied`,
    /// `h\nX: Permission denied` and after the passing key, with a sibling re-signed
    /// after the seal, which codesign names on `--verbose=2` progress lines before its
    /// verdict. Controls: the untouched bundle verifies, and a sealed resource this user
    /// cannot read is still the passing moment it was.
    #[cfg(target_os = "macos")]
    #[test]
    fn a_tampered_bundle_named_like_a_machine_fault_is_a_verdict() {
        use std::os::unix::fs::PermissionsExt as _;
        // Signing and judging a bundle wait on trustd.
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
        // codesign writes the `file modified: …/h: Permission denied` detail to STDOUT;
        // a reader handed both streams still reads the first line's verdict.
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
        // Root reads a mode-000 file, and then the premise does not hold.
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
    /// helper runs. On the real codesign, a FIFO at the seal
    /// (`_CodeSignature/CodeResources`) or at the main executable, a symlink to one in
    /// place of a sealed resource, or a FIFO behind `Contents`, `Contents/MacOS` or
    /// `Contents/_CodeSignature` linked out of the bundle holds `codesign --verify`
    /// until its deadline, and a deadline run out alone is the passing moment of a slow
    /// machine: a tampered stage kept and re-verified at every check, a tampered install
    /// that never asks for a reinstall. Each is a verdict in both tiers and for the
    /// sealed reads beside the gate, inside an apply's budget (where a codesign let run
    /// would answer with the budget's passing refusal), and one named after the passing
    /// key is still one. Control: the untouched bundle verifies, and so does one whose
    /// sealed symlink names a directory inside it.
    #[cfg(target_os = "macos")]
    #[test]
    fn a_bundle_codesign_cannot_read_to_an_answer_is_a_verdict() {
        // Signing and judging a bundle wait on trustd.
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
        // A directory of the bundle moved out and linked back, a FIFO where codesign looks.
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
    /// on any of them that failed. CONTROL: an ended `spctl` or `PlistBuddy` that ran
    /// to its exit is still the rejection it reports.
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

        // The team-id helper, through the real `codesign -d` on a bundle it cannot
        // read: the run ends on a moment, not on the missing line it then prints.
        // `codesign -d` of Apple's `/bin/ls` waits on trustd.
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
        // Root reads a mode-000 file, and then the premise does not hold.
        let premise = std::fs::read(&exe).is_err();
        let unread = team_id(&root.path().join("aterm.app"));
        std::fs::set_permissions(&exe, std::fs::Permissions::from_mode(0o755)).unwrap();
        if premise {
            let refusal = unread.expect_err("an unreadable bundle has no team id");
            assert!(crate::is_passing_refusal(&refusal), "{refusal}");
        }
        // CONTROL: readable, the platform-signed executable carries no Developer ID
        // team (measured: `TeamIdentifier=not set`), the verdict it always was.
        let refusal = team_id(&root.path().join("aterm.app")).expect_err("no team id");
        assert!(!crate::is_passing_refusal(&refusal), "{refusal}");
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
    /// serial one. The helpers run at once, so each field is its own helper's
    /// answer whatever the others said; the policy still reports the first refusal
    /// in the serial order — the seal before the team before Gatekeeper — and a
    /// read that failed refuses only itself.
    #[cfg(target_os = "macos")]
    #[test]
    fn each_helper_of_a_bundle_check_answers_for_itself_and_the_verdict_is_the_serial_one() {
        // Signing and judging a bundle wait on trustd.
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
        // The structural tier: the seal holds, and every read answers.
        let checks = check_bundle(&app, "", true);
        assert_eq!(checks.policy, Ok(()));
        assert_eq!(checks.build, Ok(777));
        assert_eq!(
            checks.commit.as_deref(),
            Ok("0123456789abcdef0123456789abcdef01234567")
        );
        assert_eq!(checks.short_version, Some(Ok("1.2.3".to_string())));
        assert!(checks.team.is_none(), "the structural tier reads no team");
        // Pinned, the same bundle fails all three gate helpers at once: the verdict
        // is the seal's, as the serial gate's was, while the team's own answer and
        // the reads stand beside it.
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
        // A read that fails refuses itself, not the gate.
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
    /// bundle, and its reads still answer. A pin that cannot make a requirement is
    /// refused for each bundle before anything runs. The negative control is any
    /// slip in slicing the shared runs (another bundle's answers, or the order
    /// reversed): the builds 777/778 and the broken seal's place say which is which.
    #[cfg(target_os = "macos")]
    #[test]
    fn several_bundles_checked_at_once_each_get_their_own_answers() {
        // Signing and judging a bundle wait on trustd.
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
        // Modified after signing: the seal no longer holds.
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
        // A pin that cannot make a requirement: every bundle refused, nothing run.
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
