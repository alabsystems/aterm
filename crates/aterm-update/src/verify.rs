// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0

//! Authenticity verification of a candidate `.app`, run both at stage time and
//! again at apply time (TOCTOU defence — the staged copy sits on disk between the
//! two). Mirrors the checks `apps/aterm-mac/notarize.sh` performs, in order of
//! cheapest/most-local first, and fails CLOSED: any error, non-zero exit, or
//! unparseable output is a rejection.

use std::path::Path;
use std::process::Command;
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
/// broken".
///
/// The headroom survived the batteries-included seed (§9.1), which is worth
/// recording because it looks like it should not have: the sealed
/// `Contents/Resources/toolchain-seed` puts the bundle at ~650-850 MB, and
/// `codesign --verify --deep --strict` really does hash every sealed resource.
/// Measured on an 851 MB seeded bundle: 0.09 s wall (0.28 s CPU) — codesign
/// hashes at roughly 3 GB/s and parallelizes across files, so the seed's
/// marginal cost is ~0.06 s. `spctl` is 0.14-0.21 s there and *slower* on the
/// small notarized app (~0.44 s), because certificate and ticket evaluation
/// dominate it, not hashing. Even a pre-SHA-NI Intel Mac doing software SHA-256
/// lands 1-2 s wall. Reaching 30 s would need sustained reads under ~27 MB/s
/// across the whole payload — below any internal SSD and below a spinning disk —
/// and in that regime the download and `ditto` extract dominate long before
/// verification does. `spctl`'s network reach, not payload size, remains the
/// reason this ceiling exists.
const HELPER_TIMEOUT: Duration = Duration::from_secs(30);

/// The CEILING on the child-exit poll interval (see [`HELPER_POLL_MIN`]).
const HELPER_POLL: Duration = Duration::from_millis(25);

/// The first child-exit poll interval, doubled up to [`HELPER_POLL`].
///
/// A fixed 25 ms tick made every helper cost `25ms * ceil(runtime / 25ms)` with a
/// hard 25 ms floor, because the first `try_wait` runs microseconds after `spawn`
/// and therefore always sleeps a whole tick. The helpers are local and fast —
/// measured against the installed bundle, `PlistBuddy -c Print:CFBundleVersion`
/// is 6 ms and `codesign --verify --deep --strict` 35-70 ms — so one
/// `verified_bundle_identity()` (one codesign + two PlistBuddy) rounded ~50-80 ms
/// of real work up to 75-125 ms wall, tens of milliseconds of pure sleep on the
/// launch/apply critical path. Backing off from 1 ms removes essentially all of
/// that floor while keeping the steady-state tick (and the syscall rate for a
/// genuinely slow child) exactly where it was.
const HELPER_POLL_MIN: Duration = Duration::from_millis(1);

/// The AGGREGATE ceiling on one apply's verification work — see [`ApplyBudget`].
///
/// [`HELPER_TIMEOUT`] bounds ONE helper. It does not bound an apply, and an apply
/// is not one helper: a staged swap runs sixteen of them in series (three
/// `verified_bundle_identity` calls at five each, plus the start probe), because
/// the staged, the fixed and the installed bundle are each re-verified at the
/// point of use. Every one of those is fail-closed, so timeouts cannot actually
/// accumulate — the first one aborts the apply — but "cannot accumulate" is a
/// property of the CALLERS, argued across four files, and nothing stated the
/// ceiling as a single number a reader could check.
///
/// 8s is that number. Measured against the real signed bundle the whole sequence
/// is ~1.1s wall (codesign -R 0.101s, codesign -d 0.050s, spctl 0.193s,
/// PlistBuddy 0.004s, the start probe 0.026s), so 8s is ~7x headroom over a
/// healthy machine and still short enough that a user who double-clicked does not
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

impl Drop for ApplyBudget {
    fn drop(&mut self) {
        APPLY_DEADLINE.with(|d| d.set(self.0));
    }
}

/// Bounded status-only run of a child that produces no output worth collecting —
/// today `ditto`, whose copy can legitimately run for minutes and therefore gets
/// its own explicit `limit` rather than [`HELPER_TIMEOUT`] or the apply budget.
///
/// Fails CLOSED exactly like [`output_bounded`]: a timeout is an `Err`, the child
/// is killed and reaped, and the caller unwinds the half-made swap.
pub(crate) fn status_bounded(
    cmd: &mut Command,
    what: &str,
    limit: Duration,
) -> Result<std::process::ExitStatus, String> {
    use std::process::Stdio;
    let mut child = cmd
        .stdin(Stdio::null())
        .spawn()
        .map_err(|e| format!("spawn {what}: {e}"))?;
    let deadline = Instant::now() + limit;
    loop {
        match child.try_wait() {
            Ok(Some(status)) => return Ok(status),
            Ok(None) => {
                if Instant::now() >= deadline {
                    let _ = child.kill();
                    let _ = child.wait();
                    return Err(format!(
                        "{what} did not finish within {}s; treating as a failure",
                        limit.as_secs()
                    ));
                }
                let remaining = deadline.saturating_duration_since(Instant::now());
                std::thread::sleep(HELPER_POLL.min(remaining));
            }
            Err(e) => return Err(format!("wait for {what}: {e}")),
        }
    }
}

/// Run a verification helper with a bounded wall clock, killing it on timeout.
///
/// Fails CLOSED like every other check here: a timeout is an `Err`, i.e. a
/// rejection, never a pass. `what` names the helper for the error text.
///
/// The output of these helpers is a few hundred bytes at most, so collecting it
/// after the child exits cannot deadlock on a full pipe. Do not reuse this for a
/// child that streams volume.
/// PROVE THE CANDIDATE CAN ACTUALLY START, by starting it.
///
/// Everything else this module checks is CRYPTOGRAPHIC — the signature, the Team ID,
/// the sealed identity, the Gatekeeper assessment. None of it establishes that the
/// binary will LOAD: a wrong-architecture slice, a missing dylib, a Developer ID
/// Apple revoked after we verified, anything the kernel or dyld rejects before
/// `main`. That case is the one the crash-loop sentinel cannot cover, because the
/// sentinel lives inside the new build's own `main` (`install::check_boot_health`):
/// a build that never reaches `main` never counts a launch, never reaches
/// `MAX_BOOT_ATTEMPTS`, and never reverts — leaving a bricked app beside a perfectly
/// good predecessor at `aterm.app.rollback` (2026-08-19, named by an external
/// reviewer of the product's public claim).
///
/// So: run the candidate's own executable with `--version` before the swap and
/// require a clean exit that names the build we are about to install. `--version` is
/// the cheapest total path through the binary — it loads every linked library, then
/// prints and exits without touching config, sockets, PTYs or the updater.
///
/// Bounded like every other helper here, and FAIL-CLOSED: a probe that times out,
/// crashes, or prints the wrong build refuses the apply. The machine keeps the build
/// it has, which is always a safe outcome; a bricked app is not.
#[cfg(target_os = "macos")]
pub fn probe_bundle_starts(app: &Path, expected_build: u64) -> Result<(), String> {
    let exe = app.join("Contents/MacOS/aterm");
    if !exe.is_file() {
        return Err(format!("no executable at {}", exe.display()));
    }
    let out = output_bounded(
        Command::new(&exe)
            .arg("--version")
            .env("ATERM_NO_AUTO_UPDATE", "1"),
        "candidate --version probe",
    )?;
    if !out.status.success() {
        return Err(format!(
            "the staged bundle could not start ({}): {}",
            out.status,
            String::from_utf8_lossy(&out.stderr).trim()
        ));
    }
    // …and it must be the build we are installing. A probe that starts SOME binary
    // proves nothing about this one.
    let text = String::from_utf8_lossy(&out.stdout);
    if !text.contains(&expected_build.to_string())
        && bundle_build_number(app).ok() != Some(expected_build)
    {
        return Err(format!(
            "the staged bundle started but does not report build {expected_build}: {}",
            text.trim()
        ));
    }
    Ok(())
}

/// Non-macOS: there is no bundle to start.
#[cfg(not(target_os = "macos"))]
pub fn probe_bundle_starts(_app: &Path, _expected_build: u64) -> Result<(), String> {
    Ok(())
}

fn output_bounded(cmd: &mut Command, what: &str) -> Result<std::process::Output, String> {
    use std::process::Stdio;
    let mut child = cmd
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .map_err(|e| format!("spawn {what}: {e}"))?;
    // Whichever ceiling binds first: this helper's own, or what is left of the
    // apply's aggregate budget. Outside an apply there is no budget and the
    // helper timeout stands alone, exactly as before.
    let own = Instant::now() + HELPER_TIMEOUT;
    let budget = APPLY_DEADLINE.with(|d| d.get());
    let (deadline, bound_by_budget) = match budget {
        Some(b) if b < own => (b, true),
        _ => (own, false),
    };
    let mut poll = HELPER_POLL_MIN;
    loop {
        match child.try_wait() {
            Ok(Some(_)) => break,
            Ok(None) => {
                if Instant::now() >= deadline {
                    // Reap it so the timed-out helper cannot linger as a zombie
                    // holding the bundle open across the swap.
                    let _ = child.kill();
                    let _ = child.wait();
                    return Err(if bound_by_budget {
                        format!(
                            "{what} ran past this apply's verification budget; \
                             treating as a rejection"
                        )
                    } else {
                        format!(
                            "{what} did not finish within {}s; treating as a rejection",
                            HELPER_TIMEOUT.as_secs()
                        )
                    });
                }
                // Clamp the sleep to the remaining budget so the 30s ceiling
                // stays exact, then back off toward the steady-state tick.
                let remaining = deadline.saturating_duration_since(Instant::now());
                std::thread::sleep(poll.min(remaining));
                poll = (poll * 2).min(HELPER_POLL);
            }
            Err(e) => return Err(format!("wait for {what}: {e}")),
        }
    }
    child
        .wait_with_output()
        .map_err(|e| format!("collect {what} output: {e}"))
}

/// Full apply-/stage-time gate: structural codesign seal, Team-ID pin, and
/// Gatekeeper/notarization acceptance. `expected_team` is [`crate::PINNED_TEAM_ID`].
/// Returns `Ok(())` only if every check passes.
pub fn verify_bundle(app: &Path, expected_team: &str) -> Result<(), String> {
    codesign_verify(app, expected_team)?;
    let team = team_id(app)?;
    if team != expected_team {
        return Err(format!(
            "Team ID mismatch: bundle is signed by {team:?}, expected {expected_team:?}"
        ));
    }
    spctl_assess(app)?;
    Ok(())
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
        codesign_structural(app)
    } else {
        verify_bundle(app, expected_team)
    }
}

/// `codesign --verify --deep --strict` with NO designated requirement: proves the
/// signature seal is intact and nothing in the bundle changed since signing, but does
/// not constrain the signer (an ad-hoc signature passes). The default-tier check.
fn codesign_structural(app: &Path) -> Result<(), String> {
    let out = output_bounded(
        Command::new("/usr/bin/codesign")
            .args(["--verify", "--deep", "--strict", "--verbose=2"])
            .arg(app),
        "codesign --verify (structural)",
    )?;
    if out.status.success() {
        Ok(())
    } else {
        Err(format!(
            "codesign --verify (structural) failed: {}",
            String::from_utf8_lossy(&out.stderr).trim()
        ))
    }
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
    let plist = app.join("Contents/Info.plist");
    let out = output_bounded(
        Command::new("/usr/libexec/PlistBuddy")
            .args(["-c", "Print :CFBundleVersion"])
            .arg(&plist),
        "PlistBuddy CFBundleVersion",
    )?;
    if !out.status.success() {
        return Err(format!(
            "read CFBundleVersion: {}",
            String::from_utf8_lossy(&out.stderr).trim()
        ));
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
    let plist = app.join("Contents/Info.plist");
    let out = output_bounded(
        Command::new("/usr/libexec/PlistBuddy")
            .args(["-c", "Print :CFBundleShortVersionString"])
            .arg(&plist),
        "PlistBuddy CFBundleShortVersionString",
    )?;
    if !out.status.success() {
        return Err(format!(
            "read CFBundleShortVersionString: {}",
            String::from_utf8_lossy(&out.stderr).trim()
        ));
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

/// Read the codesign-sealed source provenance from `ATermGitCommit` in Info.plist.
/// Callers compare it through [`crate::commit_matches`]; this reader only enforces a
/// small, valid UTF-8 value so process output cannot become an allocation surface.
pub fn bundle_git_commit(app: &Path) -> Result<String, String> {
    let plist = app.join("Contents/Info.plist");
    let out = output_bounded(
        Command::new("/usr/libexec/PlistBuddy")
            .args(["-c", "Print :ATermGitCommit"])
            .arg(&plist),
        "PlistBuddy ATermGitCommit",
    )?;
    if !out.status.success() {
        return Err(format!(
            "read ATermGitCommit: {}",
            String::from_utf8_lossy(&out.stderr).trim()
        ));
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

/// `codesign --verify --deep --strict` PLUS a `-R` designated requirement that pins
/// the **Apple anchor + Developer-ID chain + our Team ID**. The requirement makes
/// codesign itself reject anything not signed by a genuine Apple-issued Developer-ID
/// Application cert whose Team (OU) equals `expected_team` — the authenticity anchor
/// no longer rests solely on `spctl`/Gatekeeper, which a Gatekeeper-disabled machine
/// turns into a no-op (F1). Without this, `--verify` only proves the seal is
/// internally consistent, so a self-signed bundle with a spoofed `TeamIdentifier`
/// string would pass. Fails CLOSED on any codesign error.
fn codesign_verify(app: &Path, expected_team: &str) -> Result<(), String> {
    // The team is our compiled-in pin (10 alnum chars). Refuse to build a requirement
    // from a non-alphanumeric value — both a fail-closed guard for an unset/garbage pin
    // and a defense against injecting `"`/metacharacters into the requirement text.
    if expected_team.is_empty() || !expected_team.bytes().all(|b| b.is_ascii_alphanumeric()) {
        return Err(format!(
            "refusing to verify: pinned team id {expected_team:?} is not alphanumeric"
        ));
    }
    // Apple's Developer-ID designated requirement, team-pinned. Leading `=` marks the
    // argument as inline requirement source text (not a file). The two marker OIDs are
    // the Developer-ID intermediate CA (…6.2.6) and the Developer-ID Application leaf
    // (…6.1.13); `anchor apple generic` requires the chain to Apple's root.
    let req = format!(
        "=anchor apple generic \
         and certificate 1[field.1.2.840.113635.100.6.2.6] exists \
         and certificate leaf[field.1.2.840.113635.100.6.1.13] exists \
         and certificate leaf[subject.OU] = \"{expected_team}\""
    );
    let out = output_bounded(
        Command::new("/usr/bin/codesign")
            .args(["--verify", "--deep", "--strict", "--verbose=2", "-R", &req])
            .arg(app),
        "codesign --verify (team-pinned)",
    )?;
    if out.status.success() {
        Ok(())
    } else {
        Err(format!(
            "codesign --verify (team-pinned requirement) failed: {}",
            String::from_utf8_lossy(&out.stderr).trim()
        ))
    }
}

/// Extract the signing **Team Identifier** from a bundle. `codesign -dv` writes its
/// descriptive output to **stderr**, hence the merge; we scan for the
/// `TeamIdentifier=...` line. A `not set` value (ad-hoc / unsigned) is rejected.
pub fn team_id(app: &Path) -> Result<String, String> {
    let out = output_bounded(
        Command::new("/usr/bin/codesign")
            .args(["-d", "--verbose=4"])
            .arg(app),
        "codesign -d (team id)",
    )?;
    // -dv prints to stderr regardless of success; combine both streams.
    let text = format!(
        "{}{}",
        String::from_utf8_lossy(&out.stdout),
        String::from_utf8_lossy(&out.stderr)
    );
    parse_team_id(&text)
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
fn spctl_assess(app: &Path) -> Result<(), String> {
    let out = output_bounded(
        Command::new("/usr/sbin/spctl")
            .args(["-a", "-t", "exec", "-vvv"])
            .arg(app),
        "spctl assessment",
    )?;
    if out.status.success() {
        Ok(())
    } else {
        Err(format!(
            "spctl assessment rejected the bundle: {}",
            String::from_utf8_lossy(&out.stderr).trim()
        ))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The aggregate budget must bind BEFORE a helper's own ceiling, or sixteen
    /// serial helpers are only bounded by an argument about their callers rather
    /// than by a number.
    #[cfg(unix)]
    #[test]
    fn the_apply_budget_binds_before_the_helper_timeout() {
        let _budget = ApplyBudget::start(Duration::from_millis(150));
        let started = Instant::now();
        let outcome = output_bounded(
            Command::new("/bin/sleep").arg("30"),
            "a helper that never returns",
        );
        let waited = started.elapsed();
        let error = outcome.expect_err("a helper past the budget must be a rejection");
        assert!(
            error.contains("verification budget"),
            "the rejection must name the budget, not the per-helper ceiling: {error}"
        );
        // Bounded by the budget (150ms), nowhere near HELPER_TIMEOUT (30s).
        assert!(
            waited < Duration::from_secs(5),
            "waited {waited:?}, so the budget did not bind"
        );
    }

    /// Outside an apply there is no budget, and the per-helper ceiling stands
    /// alone exactly as before — the budget must not leak into ordinary use.
    #[cfg(unix)]
    #[test]
    fn without_a_budget_a_helper_keeps_its_own_ceiling() {
        assert!(APPLY_DEADLINE.with(|d| d.get()).is_none());
        let out = output_bounded(Command::new("/bin/echo").arg("hi"), "echo")
            .expect("a fast helper still runs");
        assert!(out.status.success());
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

    /// `ditto` was the one child on the apply path with no ceiling at all.
    #[cfg(unix)]
    #[test]
    fn status_bounded_kills_a_child_that_overruns() {
        let started = Instant::now();
        let error = status_bounded(
            Command::new("/bin/sleep").arg("30"),
            "a copy that never finishes",
            Duration::from_millis(150),
        )
        .expect_err("an overrunning copy must fail, not hang");
        assert!(error.contains("did not finish"), "{error}");
        assert!(started.elapsed() < Duration::from_secs(5));
    }

    #[test]
    fn team_id_parsed_from_codesign_output() {
        let sample = "Executable=/x\nIdentifier=com.aterm.aterm\nTeamIdentifier=ABCDE12345\nSealed Resources\n";
        assert_eq!(parse_team_id(sample).unwrap(), "ABCDE12345");
    }

    #[test]
    fn team_id_fails_closed_on_adhoc_or_missing() {
        // ad-hoc / unsigned reports "not set".
        assert!(parse_team_id("TeamIdentifier=not set\n").is_err());
        // no line at all → reject (not silently accept).
        assert!(parse_team_id("Identifier=com.aterm.aterm\n").is_err());
        assert!(parse_team_id("TeamIdentifier=\n").is_err());
    }
}
