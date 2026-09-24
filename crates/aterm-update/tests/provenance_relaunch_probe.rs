// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0

//! DOES A LAUNCHSERVICES LAUNCH PASS PROVENANCE TRACKING ON? — the measurement behind
//! retiring aterm-gui's provenance self-repair (`crates/aterm-gui/src/provenance_repair.rs`).
//!
//! The repair relaunched a tracked aterm from its clean bundle through LaunchServices, on
//! the premise that the new process would be untracked. This probe checks the launch lane
//! itself with a throwaway `.app` that writes one file and exits: a clean copy started by a
//! TRACKED process, and a tracked instance that relaunches itself after a clean copy has
//! been swapped in at its own path. Both write clean files (measured 2026-09-23, macOS
//! 26.6.2), so the lane does not carry the tag. aterm itself stayed tracked through that
//! lane anyway — the module doc has that half — which a probe signed like this one cannot
//! reproduce.
//!
//! IGNORED by default and macOS-only: it needs `/usr/bin/cc`, `launchctl` to build the
//! clean copies (a tracked test process tags everything it creates), and `open` to reach
//! LaunchServices. The probe app is background-only, so nothing appears on screen. Run it
//! deliberately:
//!
//! ```text
//! targo --unverified test -p aterm-update --test provenance_relaunch_probe -- --ignored --nocapture
//! ```

#![cfg(target_os = "macos")]

use std::path::{Path, PathBuf};
use std::process::Command;
use std::time::{Duration, Instant};

/// Does `path` carry `com.apple.provenance`? PANICS on a failed read, for the reason the
/// rename probe gives: "could not look" must never pass as "clean".
fn tagged(path: &Path) -> bool {
    let out = Command::new("/usr/bin/xattr")
        .arg("-l")
        .arg(path)
        .output()
        .expect("xattr runs");
    assert!(
        out.status.success(),
        "xattr could not inspect {}: {}",
        path.display(),
        String::from_utf8_lossy(&out.stderr).trim()
    );
    String::from_utf8_lossy(&out.stdout).contains("com.apple.provenance")
}

fn wait_for(path: &Path) {
    let started = Instant::now();
    while !path.exists() {
        assert!(
            started.elapsed() < Duration::from_secs(60),
            "{} never appeared",
            path.display()
        );
        std::thread::sleep(Duration::from_millis(50));
    }
}

/// Run `/bin/sh -c script probe args…` as an UNTRACKED launchd job and wait for `done`.
fn launchd_sh(dir: &Path, script: &str, args: &[&Path], done: &Path) {
    let label = format!(
        "systems.alab.probe.relaunch-{}-{}",
        std::process::id(),
        done.file_name().and_then(|n| n.to_str()).unwrap_or("job")
    );
    let _ = std::fs::remove_file(done);
    let out = Command::new("/bin/launchctl")
        .arg("submit")
        .args(["-l", &label])
        .arg("--")
        .arg("/bin/sh")
        .arg("-c")
        .arg(script)
        .arg("probe")
        .args(args)
        .current_dir(dir)
        .output()
        .expect("launchctl runs");
    assert!(
        out.status.success(),
        "launchctl submit: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    wait_for(done);
    let _ = Command::new("/bin/launchctl")
        .args(["remove", &label])
        .output();
}

/// `probe <out>` writes `<out>` and exits. `probe --relaunch <bundle> <out1> <out2>`
/// writes `<out1>`, waits for `<out1>.go`, asks LaunchServices (`open -n`) for a NEW
/// instance of `<bundle>` that writes `<out2>`, and stays alive until it has — the shape
/// of a predecessor holding on until its successor is up.
const PROBE_C: &str = r#"
#include <stdio.h>
#include <string.h>
#include <unistd.h>
static void put(const char *p) { FILE *f = fopen(p, "w"); if (f) { fputs("x\n", f); fclose(f); } }
static void await_path(const char *p) { for (int i = 0; i < 600 && access(p, F_OK) != 0; i++) usleep(100000); }
int main(int argc, char **argv) {
    if (argc == 5 && strcmp(argv[1], "--relaunch") == 0) {
        char go[4096];
        put(argv[3]);
        snprintf(go, sizeof go, "%s.go", argv[3]);
        await_path(go);
        char *open_argv[] = {"/usr/bin/open", "-n", "-g", argv[2], "--args", argv[4], NULL};
        pid_t pid = fork();
        if (pid == 0) { execv(open_argv[0], open_argv); _exit(127); }
        await_path(argv[4]);
        return 0;
    }
    if (argc >= 2) put(argv[argc - 1]);
    return 0;
}
"#;

fn info_plist(id: &str) -> String {
    format!(
        "<?xml version=\"1.0\" encoding=\"UTF-8\"?>\n\
         <!DOCTYPE plist PUBLIC \"-//Apple//DTD PLIST 1.0//EN\" \
         \"http://www.apple.com/DTDs/PropertyList-1.0.dtd\">\n\
         <plist version=\"1.0\"><dict>\n\
         <key>CFBundleExecutable</key><string>probe</string>\n\
         <key>CFBundleIdentifier</key><string>{id}</string>\n\
         <key>CFBundlePackageType</key><string>APPL</string>\n\
         <key>CFBundleVersion</key><string>1</string>\n\
         <key>LSBackgroundOnly</key><true/>\n\
         </dict></plist>\n"
    )
}

/// A bundle written by THIS process: tagged whenever this process is tracked.
fn tagged_bundle(dir: &Path, name: &str, exe: &Path, plist: &Path) -> PathBuf {
    let app = dir.join(format!("{name}.app"));
    std::fs::create_dir_all(app.join("Contents/MacOS")).expect("mkdir");
    std::fs::copy(exe, app.join("Contents/MacOS/probe")).expect("copy exe");
    std::fs::copy(plist, app.join("Contents/Info.plist")).expect("copy plist");
    app
}

/// A byte copy made by an UNTRACKED job (`cat`, which carries no attributes): clean.
fn clean_bundle(dir: &Path, name: &str, exe: &Path, plist: &Path) -> PathBuf {
    let app = dir.join(format!("{name}.app"));
    let done = dir.join(format!("made-{name}"));
    launchd_sh(
        dir,
        "mkdir -p \"$1/Contents/MacOS\" && cat \"$2\" > \"$1/Contents/MacOS/probe\" && \
         chmod 755 \"$1/Contents/MacOS/probe\" && cat \"$3\" > \"$1/Contents/Info.plist\" && \
         : > \"$4\"",
        &[&app, exe, plist, &done],
        &done,
    );
    assert!(
        !tagged(&app) && !tagged(&app.join("Contents/MacOS/probe")),
        "the job's copy starts clean"
    );
    app
}

fn open_new_instance(app: &Path, args: &[&Path]) {
    let status = Command::new("/usr/bin/open")
        .args(["-n", "-g"])
        .arg(app)
        .arg("--args")
        .args(args)
        .status()
        .expect("open runs");
    assert!(status.success(), "open -n {}", app.display());
}

/// THE MEASUREMENT.
#[test]
#[ignore = "measures the host's provenance behaviour; needs cc, launchctl and LaunchServices"]
fn a_launchservices_launch_from_a_clean_bundle_is_untracked_whoever_asks() {
    let dir = std::env::temp_dir().join(format!("aterm-relaunch-probe-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).expect("scratch");
    let this_process_is_tracked = {
        let probe = dir.join("probe");
        std::fs::write(&probe, b"x").expect("write");
        tagged(&probe)
    };
    if !this_process_is_tracked {
        eprintln!("untracked test process: nothing to measure — run it from a tracked shell");
        let _ = std::fs::remove_dir_all(&dir);
        return;
    }

    let source = dir.join("probe.c");
    std::fs::write(&source, PROBE_C).expect("write source");
    let exe = dir.join("probe-bin");
    let cc = Command::new("/usr/bin/cc")
        .arg("-o")
        .arg(&exe)
        .arg(&source)
        .status()
        .expect("cc runs");
    assert!(cc.success(), "cc");
    // One bundle identifier per role and run, so LaunchServices never confuses them.
    let id = |role: &str| format!("systems.alab.probe.relaunch.{role}{}", std::process::id());
    let plist = |role: &str| {
        let path = dir.join(format!("Info-{role}.plist"));
        std::fs::write(&path, info_plist(&id(role))).expect("write plist");
        path
    };
    let (control_plist, fresh_plist, live_plist) =
        (plist("control"), plist("fresh"), plist("live"));

    // (1) CONTROL: a tagged bundle is tracked, so the probe can see tracking at all.
    let control = tagged_bundle(&dir, "control", &exe, &control_plist);
    let out_control = dir.join("out-control");
    open_new_instance(&control, &[&out_control]);
    wait_for(&out_control);

    // (2) A clean bundle, started through LaunchServices by this TRACKED process.
    let fresh = clean_bundle(&dir, "fresh", &exe, &fresh_plist);
    let out_fresh = dir.join("out-fresh");
    open_new_instance(&fresh, &[&out_fresh]);
    wait_for(&out_fresh);

    // (3) THE REPAIR'S SHAPE: a tracked instance, a clean copy swapped in at its own path
    // by an untracked job, then the instance asks for a new instance of that path.
    let live = tagged_bundle(&dir, "live", &exe, &live_plist);
    let replacement = clean_bundle(&dir, "replacement", &exe, &live_plist);
    let (out_before, out_after) = (dir.join("out-before"), dir.join("out-after"));
    open_new_instance(
        &live,
        &[Path::new("--relaunch"), &live, &out_before, &out_after],
    );
    wait_for(&out_before);
    let go = dir.join("out-before.go");
    launchd_sh(
        &dir,
        "mv \"$1\" \"$1.old\" && mv \"$2\" \"$1\" && : > \"$3\"",
        &[&live, &replacement, &go],
        &go,
    );
    wait_for(&out_after);

    let (control_tagged, fresh_tagged) = (tagged(&out_control), tagged(&out_fresh));
    let (before_tagged, after_tagged) = (tagged(&out_before), tagged(&out_after));
    eprintln!(
        "this test process tracked=true\n  \
         tagged bundle, opened                      -> file tagged={control_tagged}\n  \
         clean bundle, opened by a tracked process  -> file tagged={fresh_tagged}\n  \
         tracked instance before its relaunch       -> file tagged={before_tagged}\n  \
         its relaunch from the clean swapped bundle -> file tagged={after_tagged}"
    );
    assert!(
        control_tagged && before_tagged,
        "a process started from a tagged bundle is tracked — without this the probe sees nothing"
    );
    assert!(
        !fresh_tagged,
        "a tracked process asking LaunchServices for a clean bundle got a TRACKED process: \
         the launch lane now passes tracking on"
    );
    assert!(
        !after_tagged,
        "a tracked instance relaunching itself from a clean bundle got a TRACKED successor: \
         the launch lane now passes tracking on"
    );
    let _ = std::fs::remove_dir_all(&dir);
}
