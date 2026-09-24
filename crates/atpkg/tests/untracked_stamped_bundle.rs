// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0

//! THE STAMPED-BUNDLE SHAPE: a clean executable inside an `.app` whose DIRECTORY carries
//! `com.apple.provenance`, run through both untracked lanes by the real code.
//!
//! # Why this file exists
//!
//! A self-updated `aterm.app` whose last update was placed by a tracked v0.86.0–v0.89.0
//! updater — a stamped app's own updater is tracked — has clean files and a stamped `.app` DIRECTORY: 8e7868f52 (first in v0.86.0)
//! made the updater write the files clean, and the renames that placed the bundle still
//! stamped its root until 15ec68a85 (first in v0.90.0) turned them into an exchange. A
//! plain-rename fallback, or a tracked process moving the bundle, still produces it.
//! This Mac's `/Applications/aterm.app` had exactly that shape on 2026-09-22 — `xattr`
//! on the bundle printed `com.apple.provenance`, on `Contents/MacOS/aterm` nothing —
//! until the 0.90 updater placed a clean 0.91. A launchd job running a clean executable
//! in place from such a bundle writes STAMPED files (the full measured table is on
//! `atpkg::provenance::taint_candidates`; 15ec68a85 measured the same on m16).
//!
//! Until 2026-09-23 `stage_helper::plan_for_exe` read the tag off the executable alone,
//! planned this shape `ExecOriginal`, and the job ran in place and laid tagged files. On
//! 2026-09-22 this Mac's store held eight `<build>.tracked-install` sidecars, every one
//! reading "the untracked lane ran, but the first file it laid still carried
//! com.apple.provenance", the Trust toolchain among them. The sidecars do not name the
//! plan, so tying each to this shape is an inference — one that fits the mechanism, the
//! app's shape and their dates (2026-09-18 to 2026-09-20). The other bundle test, in
//! `untracked_stage.rs`, never saw it: its fake bundle is made entirely by the test
//! process, so its executable is stamped too and the plan passed for the executable's
//! sake.
//!
//! # Why a file of its own
//!
//! The bundle replica is one directory per PROCESS (`stage_helper::process_replica_dir`),
//! and the wrapper reuses it whenever it already holds an executable of the helper's
//! name. In production one process only ever runs its own bundle. In a test binary
//! whose other tests replicate a different fake bundle, this test's job could run THAT
//! replica and pass for the wrong reason. Its own binary is its own process.
//!
//! macOS only: the tag and launchd exist nowhere else. A test process that is NOT
//! provenance-tracked cannot make a stamped directory — the tag cannot be minted by
//! hand. There each test prints `NOT EXERCISED` and returns, which the harness counts as
//! a PASS; the notice shows only under `--nocapture`. The merge contract runs from a
//! tracked shell on the owner's Mac, where the tests are exercised. A process whose
//! tracking cannot be MEASURED fails rather than guessing.

#![cfg(target_os = "macos")]

use std::collections::BTreeMap;
use std::io::Write as _;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::time::{Duration, Instant};

use atpkg::install::StageSpec;
use atpkg::lay::{Executable, encode_spec, lay_untracked};
use atpkg::provenance::{carries_provenance, measure_tracked};
use atpkg::stage_helper::{HelperPlan, plan_for_exe, stage_untracked};

fn scratch(label: &str) -> PathBuf {
    let d = std::env::temp_dir().join(format!(
        "atpkg-stamped-bundle-{label}-{}",
        std::process::id()
    ));
    let _ = std::fs::remove_dir_all(&d);
    std::fs::create_dir_all(&d).unwrap();
    d
}

fn atpkg_exe() -> PathBuf {
    PathBuf::from(env!("CARGO_BIN_EXE_atpkg"))
}

/// Run `/bin/sh -c <script>` from a job LAUNCHD spawns — an untracked process whatever
/// this test process is — with `args` as `$1…`, and wait for it. `stem` names both the
/// label and the completion file, so two calls in one directory never read each other's
/// completion. The label is removed on every path.
fn launchd_sh(stem: &str, script: &str, args: &[&Path], dir: &Path) {
    let done = dir.join(format!("{stem}.done"));
    let _ = std::fs::remove_file(&done);
    let label = format!(
        "systems.alab.atpkg.test.stamped.{stem}.{}",
        std::process::id()
    );
    let wrapped = format!("d=\"$1\"; shift; ( {script} ); : > \"$d\"");
    let out = Command::new("/bin/launchctl")
        .args(["submit", "-l", &label, "--", "/bin/sh", "-c", &wrapped, "x"])
        .arg(&done)
        .args(args)
        .output()
        .expect("launchctl runs");
    assert!(
        out.status.success(),
        "launchctl submit: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    let started = Instant::now();
    while !done.exists() {
        if started.elapsed() > Duration::from_secs(60) {
            let _ = Command::new("/bin/launchctl")
                .args(["remove", &label])
                .output();
            panic!("the launchd job {stem} did not finish");
        }
        std::thread::sleep(Duration::from_millis(50));
    }
    let _ = Command::new("/bin/launchctl")
        .args(["remove", &label])
        .output();
}

/// The shape: `<d>/<name>` made by THIS process — stamped, because it is tracked — and
/// everything inside it (`Contents/`, `Info.plist`, `Contents/MacOS/atpkg`, a byte copy of
/// this build's `atpkg`) laid by a launchd job, so all of it is clean.
fn stamped_bundle_around_a_clean_helper(d: &Path, name: &str) -> (PathBuf, PathBuf) {
    let bundle = d.join(name);
    std::fs::create_dir(&bundle).unwrap();
    launchd_sh(
        &format!("lay-{}", name.replace('.', "-")),
        "mkdir -p \"$1/Contents/MacOS\" && printf '<plist/>' > \"$1/Contents/Info.plist\" \
         && cat \"$2\" > \"$1/Contents/MacOS/atpkg\" && chmod 755 \"$1/Contents/MacOS/atpkg\"",
        &[&bundle, &atpkg_exe()],
        d,
    );
    let exe = bundle.join("Contents").join("MacOS").join("atpkg");
    assert!(exe.is_file(), "the launchd job laid {}", exe.display());
    (bundle, exe)
}

/// Whether this test process is provenance-tracked, which is what lets it make a stamped
/// directory at all. `false` prints the NOT EXERCISED notice (see the module doc). A
/// process whose tracking cannot be measured FAILS: "could not look" is not "untracked".
fn exercised(d: &Path) -> bool {
    let tracked = measure_tracked(d).expect(
        "whether this process is provenance-tracked could not be measured (the probe file \
         could not be written or inspected) — that is a failure, not an untracked host",
    );
    if !tracked {
        eprintln!(
            "NOTICE: NOT EXERCISED — this test process is not provenance-tracked, so it \
             cannot make a stamped bundle directory (the tag cannot be minted by hand). \
             Run it from a tracked shell to exercise the stamped-bundle law."
        );
    }
    tracked
}

/// The helper `name`d executable run IN PLACE by launchd on the laying verb, laying one
/// file at `laid`: what the lane's `ExecOriginal` plan did. Returns once the job is done.
fn run_in_place_from_launchd(exe: &Path, laid: &Path, d: &Path, stem: &str) {
    let spec_dir = d.join(format!("{stem}-spec"));
    std::fs::create_dir_all(&spec_dir).unwrap();
    let spec_file = spec_dir.join("spec");
    std::fs::write(
        &spec_file,
        encode_spec(&[Executable::new(laid, b"#!/bin/sh\necho laid\n".to_vec())]),
    )
    .unwrap();
    launchd_sh(
        stem,
        &format!("\"$1\" {} \"$2\"", atpkg::lay::HIDDEN_VERB),
        &[exe, &spec_file],
        d,
    );
    assert!(
        laid.is_file(),
        "the in-place run laid {} (result: {:?})",
        laid.display(),
        std::fs::read_to_string(spec_dir.join("result")).ok()
    );
}

/// Remove the bundle replica THIS process left. The lane builds it once per process under
/// the configured prefix's `staging/.lanes` (or the temp dir without one), named
/// `replica-<pid>-…`; the next pass's dead-pid sweep would reclaim it, but a test should
/// not leave 18 MB in the owner's store for that.
fn remove_this_process_replicas() {
    let prefix = format!("replica-{}-", std::process::id());
    let mut dirs = vec![std::env::temp_dir()];
    if let Some(layout) = atpkg::store::resolve_configured() {
        dirs.push(layout.prefix.join("staging").join(".lanes"));
    }
    for dir in dirs {
        for e in std::fs::read_dir(&dir).into_iter().flatten().flatten() {
            if e.file_name().to_string_lossy().starts_with(&prefix) {
                let _ = std::fs::remove_dir_all(e.path());
            }
        }
    }
}

/// One raw USTAR header + padded body.
fn tar_entry(name: &str, typeflag: u8, content: &[u8], mode: u32) -> Vec<u8> {
    let mut h = [0u8; 512];
    let nb = name.as_bytes();
    h[..nb.len()].copy_from_slice(nb);
    h[100..108].copy_from_slice(format!("{mode:07o}\0").as_bytes());
    h[108..116].copy_from_slice(b"0000000\0");
    h[116..124].copy_from_slice(b"0000000\0");
    h[124..136].copy_from_slice(format!("{:011o}\0", content.len()).as_bytes());
    h[136..148].copy_from_slice(b"00000000000\0");
    h[148..156].copy_from_slice(b"        ");
    h[156] = typeflag;
    h[257..263].copy_from_slice(b"ustar\0");
    h[263..265].copy_from_slice(b"00");
    let sum: u32 = h.iter().map(|&b| u32::from(b)).sum();
    h[148..156].copy_from_slice(format!("{sum:06o}\0 ").as_bytes());
    let mut out = h.to_vec();
    out.extend_from_slice(content);
    out.resize(out.len() + (512 - content.len() % 512) % 512, 0);
    out
}

/// A release-bundle-shaped `.tar.zst` holding one executable, `bin/tool`.
fn one_tool_archive(dir: &Path) -> PathBuf {
    let mut tar = Vec::new();
    tar.extend(tar_entry("bin/", b'5', b"", 0o755));
    tar.extend(tar_entry(
        "bin/tool",
        b'0',
        b"#!/bin/sh\necho tool\n",
        0o755,
    ));
    tar.resize(tar.len() + 1024, 0);
    let path = dir.join("tool-1.tar.zst");
    let f = std::fs::File::create(&path).unwrap();
    let mut enc = zstd::Encoder::new(f, 0).unwrap();
    enc.write_all(&tar).unwrap();
    enc.finish().unwrap();
    path
}

fn stage_spec() -> StageSpec {
    StageSpec {
        payload: String::new(),
        entry: String::new(),
        strip_components: 0,
        links: BTreeMap::new(),
        size_cap: 1 << 20,
    }
}

/// A clean executable inside a STAMPED `.app` must be run from a clean replica of its
/// bundle, never in place — and both lanes, run by the real code, must then lay clean
/// files. The in-place run is kept as the negative control: it is what proves the
/// fixture reproduces the failing shape on this macOS, so a clean lane result is not
/// vacuous.
#[test]
fn a_clean_helper_inside_a_stamped_bundle_runs_from_a_replica_and_lays_clean_files() {
    let d = scratch("law");
    if !exercised(&d) {
        let _ = std::fs::remove_dir_all(&d);
        return;
    }
    let (bundle, exe) = stamped_bundle_around_a_clean_helper(&d, "stamped.app");

    // THE SHAPE: stamp on the bundle directory, nothing on the executable or inside.
    assert!(
        carries_provenance(&bundle),
        "a directory this tracked process made carries the tag: {}",
        bundle.display()
    );
    assert!(
        !carries_provenance(&exe),
        "the helper a launchd job laid is clean: {}",
        exe.display()
    );
    assert!(
        !carries_provenance(&bundle.join("Contents")),
        "Contents/ was laid by the launchd job and is clean"
    );

    // THE NEGATIVE CONTROL: the same clean executable, run IN PLACE by launchd, lays a
    // STAMPED file. This is what `ExecOriginal` did with this shape, and it is the
    // measurement that makes the rest of the test mean something.
    let body = b"#!/bin/sh\necho laid\n".to_vec();
    let in_place = d.join("laid-in-place");
    run_in_place_from_launchd(&exe, &in_place, &d, "in-place");
    assert!(
        carries_provenance(&in_place),
        "PREMISE: a clean executable run in place from a stamped `.app` writes stamped \
         files. If this fails, this macOS no longer charges the process to its bundle, the \
         bundle arm of `plan_for_exe` is not needed for this shape, and the clean lane \
         results below prove nothing"
    );

    // THE PLAN: the bundle's stamp taints its executable.
    assert_eq!(
        plan_for_exe(&exe).expect("the helper and its bundle can be inspected"),
        HelperPlan::CopyBundleThenExec {
            bundle: bundle.clone()
        },
        "a clean executable inside a stamped `.app` must be run from a replica of the \
         bundle, never in place"
    );

    // THE LANES, end to end. The laying lane measures its own witness and answers Err
    // when it comes back stamped.
    let laid = d.join("laid-by-the-lane");
    lay_untracked(&exe, &[Executable::new(&laid, body.clone())], &d)
        .unwrap_or_else(|why| panic!("the laying lane must lay clean files: {why}"));
    assert_eq!(std::fs::read(&laid).unwrap(), body);
    assert!(
        !carries_provenance(&laid),
        "the file the laying lane laid is clean"
    );

    // The staging lane is the one that wrote the eight `tracked-install` records.
    let archive = one_tool_archive(&d);
    let dest = d.join("incoming");
    std::fs::create_dir_all(&dest).unwrap();
    let staged = stage_untracked(&exe, &stage_spec(), &archive, &dest, &d)
        .unwrap_or_else(|why| panic!("the staging lane must run: {why}"));
    assert!(
        !staged.witness_tagged,
        "the staging lane's witness is clean — this is the reading that was `true` eight \
         times on this Mac"
    );
    assert_eq!(
        std::fs::read(dest.join("bin/tool")).unwrap(),
        b"#!/bin/sh\necho tool\n"
    );
    assert!(
        !carries_provenance(&dest.join("bin/tool")),
        "the staged tool is clean"
    );
    remove_this_process_replicas();
    let _ = std::fs::remove_dir_all(&d);
}

/// The other shapes the kernel charges, at the PLAN level: a case variant of `.app` and a
/// non-`.app` wrapper around `Contents/MacOS/` (measured 2026-09-23; the table is on
/// `atpkg::provenance::taint_candidates`). Each keeps its own in-place negative control.
/// No lane runs here — the replica is one per process and reused by executable name, so
/// running a second bundle's lane in this binary could reuse the first test's replica;
/// the plan is the part that differs by shape, and the lanes are exercised above.
#[test]
fn a_case_variant_app_and_an_xpc_wrapper_are_planned_as_tainted_too() {
    let d = scratch("shapes");
    if !exercised(&d) {
        let _ = std::fs::remove_dir_all(&d);
        return;
    }
    for name in ["Stamped.APP", "Stamped.xpc"] {
        let (bundle, exe) = stamped_bundle_around_a_clean_helper(&d, name);
        assert!(carries_provenance(&bundle), "{name} is stamped");
        assert!(
            !carries_provenance(&exe),
            "the helper inside {name} is clean"
        );
        let laid = d.join(format!("laid-in-place-{}", name.replace('.', "-")));
        run_in_place_from_launchd(
            &exe,
            &laid,
            &d,
            &format!("in-place-{}", name.replace('.', "-")),
        );
        assert!(
            carries_provenance(&laid),
            "PREMISE: run in place from a stamped {name}, the clean helper writes stamped \
             files — without that, the plan assertion below proves nothing"
        );
        assert_eq!(
            plan_for_exe(&exe).expect("the helper and its bundle can be inspected"),
            HelperPlan::CopyBundleThenExec {
                bundle: bundle.clone()
            },
            "a clean helper inside a stamped {name} must be run from a replica, never in \
             place"
        );
    }
    let _ = std::fs::remove_dir_all(&d);
}
