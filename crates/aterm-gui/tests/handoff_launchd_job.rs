// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! SEAMLESS-UPDATE REGRESSION GUARD: the SURVIVOR of an overlap handoff must
//! still be a live launchd APPLICATION job.
//!
//! # The defect this exists to catch
//!
//! `App::apply_staged_update_now` hands every live PTY to a successor and the
//! outgoing process then `_exit(0)`s inside `seamless::commit_and_exit`. On
//! macOS that outgoing process IS the process of the launchd job
//! `application.com.aterm.aterm.<hex>.<hex>` that LaunchServices created for
//! this app instance, and today's successor is a `fork`+`exec` CHILD of it
//! (`std::process::Command::spawn` in `app_update_handoff::run_handoff_worker`).
//! When the job's process exits, launchd tears the JOB down. The successor
//! keeps running — re-parented to pid 1 — while holding a bootstrap (XPC)
//! context that belongs to a job which no longer exists.
//!
//! Nothing about the terminal looks wrong afterwards: the shell survived, the
//! screen is intact, the window is there. The breakage is silent until the
//! survivor asks for something that needs the app's own XPC domain, and then it
//! fails in a way that reads as an unrelated bug: `hdiutil` (the update lane's
//! own DMG attach) returns `ENXIO` / "Device not configured", so the process
//! that just applied an update can never apply the next one. User notifications
//! and LaunchServices opens are in the same class.
//!
//! # Why the assertion is on launchd, not on the symptom
//!
//! `hdiutil` failing is one downstream consequence of one upstream fact. A test
//! that asserted "the survivor can attach a DMG" would need a DMG, would be
//! slow, and would still pass on a machine where some *other* XPC service
//! happened to answer. The upstream fact is exact, cheap, and observable:
//! `launchctl print gui/<uid>` lists the app-instance jobs of the user's GUI
//! domain, and the survivor's pid must be one of them. A pid that is alive but
//! absent from that table is precisely the orphan state described above.
//!
//! The label must be an `application.<bundle-id>.*` job, not merely any job
//! mentioning the bundle id: only the LaunchServices-created application job
//! carries the per-instance bootstrap subset the app's frameworks resolve
//! against.
//!
//! # Status
//!
//! [`survivor_is_a_live_launchd_application_job`] is the end-to-end guard. It is
//! `#[ignore]`d because it drives a real LaunchServices launch and a real
//! update handoff of a built `aterm.app`, and it FAILS today — it is the
//! reproducer for the defect above, and becomes the standing regression guard
//! the moment the successor is launched as its own launchd job. Run it with:
//!
//! ```text
//! ATERM_HANDOFF_E2E_APP=/path/to/aterm.app \
//!   cargo test -p aterm-gui --test handoff_launchd_job -- --ignored --nocapture
//! ```
//!
//! The pure tests around [`running_application_job`] DO run in the gate: they
//! pin the reading of the `launchctl` table, which is the part of the guard
//! that could silently rot into a vacuous pass (a parser that matches nothing
//! would make the e2e assertion unfalsifiable).

#![cfg(target_os = "macos")]

use std::collections::BTreeSet;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::time::{Duration, Instant};

/// The shipped `CFBundleIdentifier` (`aterm-release`'s bundle stamper default).
const BUNDLE_ID: &str = "com.aterm.aterm";

/// The launchd label prefix LaunchServices gives one running INSTANCE of
/// `bundle_id`. The trailing dot is load-bearing: without it a neighbouring
/// bundle whose id merely starts with ours (`com.aterm.aterm-helper`) would
/// satisfy the guard.
fn application_job_prefix(bundle_id: &str) -> String {
    format!("application.{bundle_id}.")
}

/// Every RUNNING `application.<bundle_id>.*` job in a `launchctl print
/// gui/<uid>` printout, as `(pid, label)`.
///
/// A service row of that printout is exactly three whitespace-separated fields
/// — pid, last exit status, label — so requiring exactly three is what keeps
/// the surrounding key/value prose out of the answer. A job that is loaded but
/// has no process prints `-` for its pid and is deliberately dropped here: the
/// property under test is not "a label exists", it is "a live process is that
/// job", which is what owns the bootstrap domain.
fn running_application_jobs(printout: &str, bundle_id: &str) -> Vec<(i32, String)> {
    let prefix = application_job_prefix(bundle_id);
    printout
        .lines()
        .filter_map(|line| {
            let mut fields = line.split_whitespace();
            let pid = fields.next()?;
            let _last_exit_status = fields.next()?;
            let label = fields.next()?;
            if fields.next().is_some() || !label.starts_with(&prefix) {
                return None;
            }
            Some((pid.parse::<i32>().ok()?, label.to_string()))
        })
        .collect()
}

/// The label of the running application job whose process is `pid`, if any.
fn running_application_job(printout: &str, bundle_id: &str, pid: i32) -> Option<String> {
    running_application_jobs(printout, bundle_id)
        .into_iter()
        .find_map(|(job_pid, label)| (job_pid == pid).then_some(label))
}

fn command_stdout(program: &str, args: &[&str]) -> String {
    let output = Command::new(program)
        .args(args)
        .output()
        .unwrap_or_else(|error| panic!("run {program}: {error}"));
    String::from_utf8_lossy(&output.stdout).into_owned()
}

fn current_uid() -> u32 {
    command_stdout("/usr/bin/id", &["-u"])
        .trim()
        .parse()
        .expect("id -u prints this process's numeric uid")
}

fn launchctl_print_gui(uid: u32) -> String {
    command_stdout("/bin/launchctl", &["print", &format!("gui/{uid}")])
}

/// Liveness WITHOUT `kill(2)`: this test never owns the processes it observes
/// (they are launchd's children), so probing with a signal would be the only
/// place in the file that could disturb the very lifecycle under test.
fn pid_alive(pid: i32) -> bool {
    !command_stdout("/bin/ps", &["-p", &pid.to_string(), "-o", "pid="])
        .trim()
        .is_empty()
}

/// Every process running the bundle's executable. Matching the FULL executable
/// path (rather than a process name) keeps the successor visible across its
/// staged-swap re-exec and excludes any unrelated `aterm` on the machine.
fn instance_pids(executable: &Path) -> BTreeSet<i32> {
    command_stdout("/usr/bin/pgrep", &["-f", &executable.to_string_lossy()])
        .lines()
        .filter_map(|line| line.trim().parse::<i32>().ok())
        .collect()
}

fn wait_until(mut ready: impl FnMut() -> bool, budget: Duration, what: &str) {
    let deadline = Instant::now() + budget;
    loop {
        if ready() {
            return;
        }
        assert!(Instant::now() < deadline, "timed out waiting for {what}");
        std::thread::sleep(Duration::from_millis(100));
    }
}

/// Wait for exactly ONE instance to appear that was not in `known`.
///
/// Bounded rather than instantaneous because both interesting transitions are
/// asynchronous: a LaunchServices launch goes through the LaunchServices
/// daemon, and the overlap successor re-execs itself once for the staged swap.
fn wait_for_new_instance(
    executable: &Path,
    known: &BTreeSet<i32>,
    budget: Duration,
    what: &str,
) -> i32 {
    let deadline = Instant::now() + budget;
    loop {
        let fresh = instance_pids(executable)
            .difference(known)
            .copied()
            .collect::<Vec<_>>();
        if let [pid] = fresh[..] {
            return pid;
        }
        assert!(
            Instant::now() < deadline,
            "timed out waiting for {what}; new instances seen: {fresh:?}"
        );
        std::thread::sleep(Duration::from_millis(100));
    }
}

/// Panic-safe ownership of the survivor: a failing assertion must not leave a
/// second aterm instance (holding the fixture's PTYs) running on the machine.
struct KillOnDrop(Option<i32>);

impl Drop for KillOnDrop {
    fn drop(&mut self) {
        if let Some(pid) = self.0.take() {
            let pid = pid.to_string();
            let _ = Command::new("/bin/kill")
                .args(["-9", pid.as_str()])
                .status();
        }
    }
}

#[test]
fn running_application_job_reads_the_launchctl_table() {
    // Shaped exactly like a `launchctl print gui/<uid>` service block: three
    // fields per row, `-` for a job with no live process.
    let printout = "gui/501 = {\n\
         \tservices = {\n\
         \t    4711     0\tapplication.com.aterm.aterm.0x10a5f.0x10a5f\n\
         \t       -     0\tapplication.com.aterm.aterm.0x9001.0x9001\n\
         \t    4712     0\tcom.aterm.aterm\n\
         \t    4713     0\tapplication.com.aterm.aterm-helper.0x1.0x1\n\
         \t    4714     0\tcom.apple.Finder\n\
         \t}\n\
         }\n";

    assert_eq!(
        running_application_job(printout, BUNDLE_ID, 4711).as_deref(),
        Some("application.com.aterm.aterm.0x10a5f.0x10a5f"),
        "a live app instance is found by its pid"
    );

    // THE DEFECT'S SIGNATURE: the survivor is alive but its pid is in no row.
    assert_eq!(
        running_application_job(printout, BUNDLE_ID, 9999),
        None,
        "a pid absent from the table is an orphan, not a job"
    );

    // A plain `com.aterm.aterm` job (a LaunchAgent, say) is NOT the
    // LaunchServices application job and does not carry the app's XPC domain.
    assert_eq!(running_application_job(printout, BUNDLE_ID, 4712), None);

    // A neighbouring bundle id that merely starts with ours must not satisfy
    // the guard — this is why the prefix keeps its trailing dot.
    assert_eq!(running_application_job(printout, BUNDLE_ID, 4713), None);

    // A loaded-but-dead job contributes no row at all: "the label exists" was
    // never the property.
    let labels = running_application_jobs(printout, BUNDLE_ID)
        .into_iter()
        .map(|(_, label)| label)
        .collect::<Vec<_>>();
    assert_eq!(labels, vec!["application.com.aterm.aterm.0x10a5f.0x10a5f"]);
}

#[test]
#[ignore = "live LaunchServices/launchd e2e (needs ATERM_HANDOFF_E2E_APP); \
            FAILS until the overlap successor is launched as its own launchd job"]
fn survivor_is_a_live_launchd_application_job() {
    let app = PathBuf::from(
        std::env::var_os("ATERM_HANDOFF_E2E_APP")
            .expect("set ATERM_HANDOFF_E2E_APP to a built aterm.app"),
    );
    let executable = app.join("Contents/MacOS/aterm");
    let ctl = app.join("Contents/MacOS/aterm-ctl");
    assert!(
        executable.is_file(),
        "{} is not a built app bundle",
        app.display()
    );
    assert!(ctl.exists(), "{} has no aterm-ctl alias", app.display());
    let uid = current_uid();

    // `open -n` is the fixture's own load-bearing detail: it is what makes
    // LaunchServices mint a fresh `application.com.aterm.aterm.*` job, which is
    // the state the survivor must still be in at the end. `--env` (macOS 13+)
    // carries the QA seam that re-execs the SAME binary through the FULL
    // overlap handoff, so no staged release is needed to exercise the protocol.
    let mut before = instance_pids(&executable);
    let opened = Command::new("/usr/bin/open")
        .arg("-n")
        .args(["--env", "ATERM_DEBUG_SEAMLESS_REEXEC=1"])
        .arg("-a")
        .arg(&app)
        .status()
        .expect("run /usr/bin/open");
    assert!(opened.success(), "open -n failed for {}", app.display());

    let original = wait_for_new_instance(
        &executable,
        &before,
        Duration::from_secs(30),
        "the LaunchServices-launched instance",
    );
    let mut owned = KillOnDrop(Some(original));
    assert!(
        running_application_job(&launchctl_print_gui(uid), BUNDLE_ID, original).is_some(),
        "the freshly opened instance is not a launchd application job — the \
         FIXTURE is broken, not the handoff"
    );
    // The seamless lane refuses to start without a live PTY, so wait for the
    // first window's shell to exist before asking for an apply.
    wait_until(
        || {
            !command_stdout("/usr/bin/pgrep", &["-P", &original.to_string()])
                .trim()
                .is_empty()
        },
        Duration::from_secs(30),
        "the first session's shell",
    );

    // Drive the handoff through the same verb the GUI's [Relaunch] nudge uses.
    let applied = Command::new(&ctl)
        .args(["update", "apply"])
        .status()
        .expect("run aterm-ctl update apply");
    assert!(applied.success(), "aterm-ctl update apply failed");

    // The handoff has resolved once the outgoing process is gone: it only
    // `_exit`s after the atomic Commit write, so its absence proves the
    // successor was admitted rather than rejected and reaped.
    wait_until(
        || !pid_alive(original),
        Duration::from_secs(60),
        "the outgoing instance to exit after Commit",
    );
    before.insert(original);
    let survivor = wait_for_new_instance(
        &executable,
        &before,
        Duration::from_secs(60),
        "the handoff survivor",
    );
    owned.0 = Some(survivor);

    let printout = launchctl_print_gui(uid);
    assert!(
        running_application_job(&printout, BUNDLE_ID, survivor).is_some(),
        "survivor {survivor} is alive but is not an `{}*` job in gui/{uid}: the \
         handoff left a pid-1 orphan holding a dead job's bootstrap context, so \
         every XPC-backed framework call it makes (hdiutil attach, user \
         notifications, LaunchServices opens) will fail",
        application_job_prefix(BUNDLE_ID)
    );
}
