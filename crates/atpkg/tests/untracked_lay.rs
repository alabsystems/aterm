// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0

//! The untracked EXECUTABLE-laying lane end to end (`atpkg::lay`), against the REAL
//! `atpkg` binary this build produced: the launchd job lays every file byte-for-byte at
//! mode 0755; when this test process is provenance-tracked — the case the lane exists
//! for — the files it laid are CLEAN while the in-process control is tagged; and LAW m21
//! is measured, not recited: a shim exec'd from an UNTRACKED parent writes clean output
//! when the shim is clean and tagged output when the shim is tagged. A helper that cannot
//! answer is detected inside the exit grace and lays nothing.
//!
//! macOS only: the tag and launchd exist nowhere else.

#![cfg(target_os = "macos")]

use std::path::{Path, PathBuf};
use std::process::Command;
use std::time::{Duration, Instant};

use atpkg::lay::{Executable, lay_untracked, write_in_process};
use atpkg::provenance::{carries_provenance, measure_tracked};

fn scratch(label: &str) -> PathBuf {
    let d = std::env::temp_dir().join(format!(
        "atpkg-untracked-lay-{label}-{}",
        std::process::id()
    ));
    let _ = std::fs::remove_dir_all(&d);
    std::fs::create_dir_all(&d).unwrap();
    d
}

fn helper_exe() -> PathBuf {
    PathBuf::from(env!("CARGO_BIN_EXE_atpkg"))
}

fn mode_of(p: &Path) -> u32 {
    use std::os::unix::fs::PermissionsExt as _;
    std::fs::metadata(p).unwrap().permissions().mode() & 0o777
}

/// Run `"$1" "$2"` from a job LAUNCHD spawns — an untracked parent whatever this test
/// process is — and wait for it to finish (a `done` file touched after the command).
/// The label is removed before returning, on every path.
fn exec_from_launchd(label_stem: &str, cmd: &Path, arg: &Path, dir: &Path) {
    let done = dir.join(format!("{label_stem}.done"));
    let label = format!(
        "systems.alab.atpkg.test.{label_stem}.{}",
        std::process::id()
    );
    let out = Command::new("/bin/launchctl")
        .args(["submit", "-l", &label, "--", "/bin/sh", "-c"])
        .arg("\"$1\" \"$2\"; : > \"$3\"")
        .arg("x")
        .arg(cmd)
        .arg(arg)
        .arg(&done)
        .output()
        .expect("launchctl runs");
    assert!(
        out.status.success(),
        "launchctl submit: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    let started = Instant::now();
    while !done.exists() {
        assert!(
            started.elapsed() < Duration::from_secs(20),
            "the launchd job did not finish"
        );
        std::thread::sleep(Duration::from_millis(50));
    }
    let _ = Command::new("/bin/launchctl")
        .args(["remove", &label])
        .output();
}

/// The lane lays every file byte-for-byte at 0755, in the order given (the first file
/// down first — a marker before what it marks), and leaves no job scratch behind. And
/// the property it exists for: under a tracked test process the lane's files are clean
/// while a file this process writes is tagged.
#[test]
fn the_lane_lays_the_files_clean_and_the_in_process_control_is_tagged_when_tracked() {
    let d = scratch("lane");
    let dest = d.join("bin");
    std::fs::create_dir_all(&dest).unwrap();
    let files = vec![
        Executable::new(
            dest.join("targo"),
            "#!/bin/sh\nexec '/s/bin/targo' \"$@\"\n",
        ),
        Executable::new(
            dest.join("alab-targo"),
            "#!/bin/sh\nexec '/s/bin/targo' \"$@\"\n",
        ),
        Executable::new(dest.join("odd name"), b"\x00\xff\n".to_vec()),
    ];
    lay_untracked(&helper_exe(), &files, &d)
        .unwrap_or_else(|why| panic!("the lane must run on this macOS: {why}"));
    for f in &files {
        assert_eq!(
            std::fs::read(&f.path).unwrap(),
            f.body,
            "{}",
            f.path.display()
        );
        assert_eq!(mode_of(&f.path), 0o755, "{}", f.path.display());
    }
    let leftovers: Vec<_> = std::fs::read_dir(&d)
        .unwrap()
        .flatten()
        .map(|e| e.file_name().to_string_lossy().into_owned())
        .filter(|n| n.starts_with("lay-helper-"))
        .collect();
    assert!(
        leftovers.is_empty(),
        "job scratch left behind: {leftovers:?}"
    );

    let tracked = measure_tracked(&d).expect("a writable scratch is measurable");
    let control = Executable::new(dest.join("control"), "#!/bin/sh\n");
    write_in_process(&control).unwrap();
    eprintln!(
        "this test process is {}; in-process shim tagged={}, launchd-laid shim tagged={}",
        if tracked {
            "provenance-TRACKED"
        } else {
            "untracked"
        },
        carries_provenance(&control.path),
        carries_provenance(&files[0].path)
    );
    assert_eq!(
        carries_provenance(&control.path),
        tracked,
        "the in-process writer's file carries the tag exactly when the process is tracked"
    );
    for f in &files {
        assert!(
            !carries_provenance(&f.path),
            "the lane's file must be clean (tracked caller: {tracked}): {}",
            f.path.display()
        );
    }
    let _ = std::fs::remove_dir_all(&d);
}

/// LAW m21, measured. Two shims exec the SAME clean tool from the SAME untracked parent
/// (a launchd job); only the shim's tag differs. The clean shim's output is clean. The
/// tagged shim's output carries the tag exactly when this process — which wrote the
/// tagged shim — is tracked: that is the shim tracking what it execs, the shape every
/// tool a user ran through the old shims had.
#[test]
fn m21_a_clean_shim_exec_d_from_an_untracked_parent_writes_clean_output() {
    let d = scratch("m21");
    let store_bin = d.join("store").join("bin");
    let clean_dir = d.join("clean");
    let tagged_dir = d.join("tagged");
    let out_dir = d.join("out");
    for p in [&store_bin, &clean_dir, &tagged_dir, &out_dir] {
        std::fs::create_dir_all(p).unwrap();
    }
    let tool = store_bin.join("tool");
    let shim_body = format!("#!/bin/sh\nexec '{}' \"$@\"\n", tool.display());
    // The tool and the CLEAN shim through the lane; the TAGGED shim by this process.
    lay_untracked(
        &helper_exe(),
        &[
            Executable::new(&tool, "#!/bin/sh\nprintf hi > \"$1\"\n"),
            Executable::new(clean_dir.join("tool"), shim_body.clone()),
        ],
        &d,
    )
    .unwrap_or_else(|why| panic!("the lane must run on this macOS: {why}"));
    write_in_process(&Executable::new(tagged_dir.join("tool"), shim_body)).unwrap();

    let tracked = measure_tracked(&d).expect("measurable");
    assert!(!carries_provenance(&tool), "the tool is clean");
    assert!(
        !carries_provenance(&clean_dir.join("tool")),
        "the clean shim is clean"
    );
    assert_eq!(
        carries_provenance(&tagged_dir.join("tool")),
        tracked,
        "the control shim is tagged exactly when this process is tracked"
    );

    let out_clean = out_dir.join("via-clean-shim");
    let out_tagged = out_dir.join("via-tagged-shim");
    exec_from_launchd("clean", &clean_dir.join("tool"), &out_clean, &d);
    exec_from_launchd("tagged", &tagged_dir.join("tool"), &out_tagged, &d);
    assert_eq!(std::fs::read(&out_clean).unwrap(), b"hi");
    assert_eq!(std::fs::read(&out_tagged).unwrap(), b"hi");
    eprintln!(
        "m21 from an untracked parent: output via clean shim tagged={}, via tagged shim tagged={} \
         (this process tracked={tracked})",
        carries_provenance(&out_clean),
        carries_provenance(&out_tagged)
    );
    assert!(
        !carries_provenance(&out_clean),
        "a clean shim exec'd from an untracked parent writes clean output"
    );
    assert_eq!(
        carries_provenance(&out_tagged),
        tracked,
        "law m21: a tagged shim tracks the tool it execs (vacuous under an untracked test process)"
    );
    let _ = std::fs::remove_dir_all(&d);
}

/// A helper spelled right but unable to answer (`/usr/bin/true` under the name `atpkg`)
/// is detected inside the exit grace, and the lane laid nothing — the caller's policy
/// decides what happens next, never a quiet in-process write here.
#[test]
fn a_helper_that_cannot_answer_is_detected_and_nothing_is_laid() {
    let d = scratch("noanswer");
    let dest = d.join("bin");
    std::fs::create_dir_all(&dest).unwrap();
    let files = vec![Executable::new(dest.join("tool"), "#!/bin/sh\n")];
    let why = lay_untracked(Path::new("/usr/bin/true"), &files, &d).unwrap_err();
    assert!(why.contains("not an atpkg/aterm binary"), "{why}");
    let fake_dir = d.join("fake");
    std::fs::create_dir_all(&fake_dir).unwrap();
    let fake = fake_dir.join("atpkg");
    std::fs::copy("/usr/bin/true", &fake).unwrap();
    let started = Instant::now();
    let why = lay_untracked(&fake, &files, &d).unwrap_err();
    assert!(why.contains("exited without a result"), "{why}");
    assert!(
        started.elapsed() < Duration::from_secs(20),
        "detection took {:?}",
        started.elapsed()
    );
    assert!(
        !dest.join("tool").exists(),
        "nothing laid by a lane that failed"
    );
    let _ = std::fs::remove_dir_all(&d);
}
