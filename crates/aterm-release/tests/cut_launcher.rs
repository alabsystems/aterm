// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! The cut's LAUNCHER, enforced — `tools/cut-launch.sh`.
//!
//! Every release cut on this machine is launched detached, because a cut started from a
//! shell under the installed /Applications bundle (or under any provenance-tracked
//! parent) dies post-claim on `com.apple.provenance`, with a build number burned. The
//! reflex reach is `launchctl submit`, and submit runs its job at `QOS_CLASS_UTILITY`:
//! measured 0x11 against an interactive shell's 0x21, a 50 ms timer 75.0 ms late on the
//! first tick and 25-35 ms late at the median, and the aterm instance the PAINT SMOKE
//! launches inherits the tier — 11 of 30 takes of the cutter's own smoke went red under
//! submit against 0 of 8 from a shell (docs/RELEASING.md).
//!
//! So the cut's paint evidence was being invalidated by the way the cut was started, and
//! the remedy — a LaunchAgent with `ProcessType=Interactive` — was documentation only.
//! `tools/cut-launch.sh` is that remedy as a committed, tested launcher, and this file is
//! the half of its guard that runs under `targo --unverified test -p aterm-release`:
//!
//! * the launcher exists and is executable (a cut is not a thing to improvise);
//! * the plist it generates carries `ProcessType=Interactive` and no `KeepAlive` — the
//!   two properties that, respectively, keep the smoke honest and keep a finished cut
//!   from being claimed twice;
//! * the job's executable image is `/bin/zsh` and no file of this repo, which is what
//!   keeps the launchd hop's provenance escape intact (the kernel exec'ing a TAGGED
//!   script tracks what it forwards to — measured);
//! * `tools/test-cut-launch.sh`, the behavioural half, still passes.
//!
//! Hermetic: no network, no launchd, no cut. The suite's own live lane (a real
//! LaunchAgent) is opt-in behind `ATERM_CUT_LAUNCH_LIVE=1` and is not run from here.

use std::path::{Path, PathBuf};

fn repo_root() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("../..")
}

/// The launcher is committed and executable. A `chmod -x` (or a delete) would send the
/// operator straight back to `launchctl submit`, which is the defect.
#[test]
fn the_launcher_is_committed_and_executable() {
    let launcher = repo_root().join("tools/cut-launch.sh");
    assert!(
        launcher.is_file(),
        "tools/cut-launch.sh is gone — the cut has no launcher that keeps the paint \
         smoke at interactive QoS"
    );
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let mode = launcher
            .metadata()
            .expect("stat the launcher")
            .permissions()
            .mode();
        assert!(
            mode & 0o111 != 0,
            "tools/cut-launch.sh is not executable (mode {mode:o})"
        );
    }
    let src = std::fs::read_to_string(&launcher).expect("the launcher is readable");
    assert!(
        src.contains("ProcessType"),
        "the launcher no longer names ProcessType at all"
    );
}

/// The plist the launcher writes, read as text. `Interactive` is the whole difference
/// between this launcher and the one that starves the smoke, and `KeepAlive` is the
/// behaviour that turns a finished cut into a second ledger claim.
#[cfg(unix)]
#[test]
fn the_generated_plist_is_interactive_and_never_keepalive() {
    let root = repo_root();
    let tmp = std::env::temp_dir().join(format!("cut-launch-plist-{}", std::process::id()));
    std::fs::create_dir_all(&tmp).expect("scratch dir");
    let token = tmp.join("token");
    std::fs::write(&token, "ghp_not_a_real_token\n").expect("write the stub credential");
    let plist = tmp.join("cut.plist");

    let out = std::process::Command::new("bash")
        .arg(root.join("tools/cut-launch.sh"))
        .arg("--emit-plist")
        .arg(&plist)
        .arg("--token-file")
        .arg(&token)
        .env("ATERM_CUT_STATE_DIR", tmp.join("state"))
        .current_dir(&root)
        .output()
        .expect("bash runs the launcher");
    assert!(
        out.status.success(),
        "--emit-plist failed:\n{}\n{}",
        String::from_utf8_lossy(&out.stdout),
        String::from_utf8_lossy(&out.stderr)
    );

    let xml = std::fs::read_to_string(&plist).expect("the plist was written");
    let _ = std::fs::remove_dir_all(&tmp);

    assert!(
        xml.contains("<key>ProcessType</key>") && xml.contains("<string>Interactive</string>"),
        "the cut's agent is not ProcessType=Interactive — it will run at \
         QOS_CLASS_UTILITY and starve the paint smoke:\n{xml}"
    );
    assert!(
        !xml.contains("<key>KeepAlive</key>"),
        "the cut's agent carries KeepAlive — launchd would RE-RUN the cut when it \
         exits, and a re-run cut is a second ledger claim:\n{xml}"
    );
    assert!(
        xml.contains("<string>/bin/zsh</string>"),
        "the job's executable image is not /bin/zsh:\n{xml}"
    );
    // The provenance escape: launchd must exec an UNTAGGED system binary. Every file of
    // this repo may carry com.apple.provenance, so none of them may be the job's image
    // — not as ProgramArguments[0] and not as a `--inner` re-exec.
    for line in xml.lines().filter(|l| l.contains("<string>")) {
        assert!(
            !line.contains("cut-launch.sh"),
            "the launcher re-execs ITSELF from the plist — the kernel exec'ing a tagged \
             script tracks what it forwards to, so this hands the cut the provenance tag \
             the launchd hop exists to shed:\n{line}"
        );
    }
    assert!(
        !xml.contains("ghp_not_a_real_token"),
        "the credential is IN the plist, which is world-readable and which \
         `launchctl print` echoes back:\n{xml}"
    );
}

/// The behavioural half, where a shell exists: `tools/test-cut-launch.sh` drives the
/// launcher's plist generation, its resume-spelling refusals, its token handling and the
/// job command's quoting (it runs that command against a stub toolchain). Hermetic — the
/// suite's live LaunchAgent lane is opt-in and not enabled here.
#[cfg(unix)]
#[test]
fn the_hermetic_suite_passes() {
    let suite = repo_root().join("tools/test-cut-launch.sh");
    assert!(
        suite.is_file(),
        "tools/test-cut-launch.sh is gone — the behavioural half of this guard cannot \
         vanish quietly"
    );
    let out = std::process::Command::new("bash")
        .arg(&suite)
        .current_dir(repo_root())
        .output()
        .expect("bash runs the hermetic suite");
    let stdout = String::from_utf8_lossy(&out.stdout);
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(
        out.status.success() && stdout.contains("test-cut-launch: PASS"),
        "the cut launcher's suite failed:\n--- stdout ---\n{stdout}\n--- stderr ---\n{stderr}"
    );
}
