// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0

//! The untracked staging lane end to end (`atpkg::stage_helper`), against the REAL
//! `atpkg` binary this build produced and the current one-binary `aterm` front door:
//! the hidden verb answers through the result file
//! with the same `tree_root` the in-process lane folds over the same bytes; the whole
//! launchd round trip lays down an identical tree; a helper that cannot speak the verb
//! is detected inside the exit grace and leaves the destination empty for the caller's
//! policy to act on (an in-process stage, recorded, by default; a refusal under
//! `ATPKG_REFUSE_TRACKED_INSTALL=1`); a tagged BUNDLE executable — the shipped app's
//! shape — serves BOTH hidden verbs from a whole-bundle fixture; and — when the test
//! process happens
//! to be provenance-tracked, which is the case that matters and the one this lane exists
//! for — the files the job laid down are CLEAN while the ones this process writes are
//! tagged.
//!
//! macOS only: the tag and launchd exist nowhere else.
//! The installed/notarized application's separate smoke is explicitly ignored by
//! default: its version is not the source under test and may predate these verbs.

#![cfg(target_os = "macos")]

use std::collections::BTreeMap;
use std::io::Write as _;
use std::path::{Path, PathBuf};
use std::process::Command;

use atpkg::install::{StageSpec, stage_payload_spec};
use atpkg::provenance::{carries_provenance, measure_tracked};
use atpkg::stage_helper::{
    HIDDEN_VERB, HelperPlan, decode_spec, encode_spec, plan_for_exe, stage_untracked,
};

// Share paint/spin's current-source RELEASE preparation instead of building a
// second freshness mechanism. The dedicated target avoids the outer test build's
// artifact lock; an explicit test override names the artifact actually exercised.
#[path = "../../aterm-conformance/tests/support/mod.rs"]
mod current_artifact;

fn current_aterm_exe() -> PathBuf {
    let root = Path::new(env!("CARGO_MANIFEST_DIR"))
        .ancestors()
        .nth(2)
        .expect("atpkg lives under the workspace crates directory");
    current_artifact::release_bin(root, &["ATERM_UNTRACKED_STAGE_BIN"])
}

fn scratch(label: &str) -> PathBuf {
    let d = std::env::temp_dir().join(format!(
        "atpkg-untracked-stage-{label}-{}",
        std::process::id()
    ));
    let _ = std::fs::remove_dir_all(&d);
    std::fs::create_dir_all(&d).unwrap();
    d
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

/// A release-bundle-shaped `.tar.zst`: `bin/tool` (0755) and `share/doc` (0644).
fn bundle_archive(dir: &Path) -> PathBuf {
    let mut tar = Vec::new();
    tar.extend(tar_entry("bin/", b'5', b"", 0o755));
    tar.extend(tar_entry(
        "bin/tool",
        b'0',
        b"#!/bin/sh\necho tool\n",
        0o755,
    ));
    tar.extend(tar_entry("share/", b'5', b"", 0o755));
    tar.extend(tar_entry("share/doc", b'0', b"documentation\n", 0o644));
    tar.resize(tar.len() + 1024, 0);
    let path = dir.join("tool-1.tar.zst");
    let f = std::fs::File::create(&path).unwrap();
    let mut enc = zstd::Encoder::new(f, 0).unwrap();
    enc.write_all(&tar).unwrap();
    enc.finish().unwrap();
    path
}

fn spec() -> StageSpec {
    StageSpec {
        payload: String::new(),
        entry: String::new(),
        strip_components: 0,
        links: BTreeMap::new(),
        size_cap: 1 << 20,
    }
}

/// Every regular file under `dir` as `(relative path, mode & 0o777, bytes)`, sorted.
fn tree(dir: &Path) -> Vec<(String, u32, Vec<u8>)> {
    use std::os::unix::fs::PermissionsExt as _;
    fn walk(root: &Path, dir: &Path, out: &mut Vec<(String, u32, Vec<u8>)>) {
        for e in std::fs::read_dir(dir).unwrap().flatten() {
            let p = e.path();
            let meta = std::fs::symlink_metadata(&p).unwrap();
            if meta.is_dir() {
                walk(root, &p, out);
            } else if meta.is_file() {
                out.push((
                    p.strip_prefix(root).unwrap().display().to_string(),
                    meta.permissions().mode() & 0o777,
                    std::fs::read(&p).unwrap(),
                ));
            }
        }
    }
    let mut out = Vec::new();
    walk(dir, dir, &mut out);
    out.sort();
    out
}

fn helper_exe() -> PathBuf {
    PathBuf::from(env!("CARGO_BIN_EXE_atpkg"))
}

/// The hidden verb, run directly: the result file carries `ok` and the very root the
/// in-process lane folds; the spec it read decodes to what was encoded.
#[test]
fn the_hidden_verb_answers_with_the_in_process_root() {
    let d = scratch("verb");
    let archive = bundle_archive(&d);
    let reference = d.join("reference");
    std::fs::create_dir_all(&reference).unwrap();
    let expected = stage_payload_spec(&spec(), &archive, &reference).unwrap();

    let job = d.join("job");
    std::fs::create_dir_all(&job).unwrap();
    let dest = d.join("dest");
    std::fs::create_dir_all(&dest).unwrap();
    let spec_text = encode_spec(&spec(), &archive, &dest);
    let (back, a, de) = decode_spec(&spec_text).unwrap();
    assert_eq!((back, a, de), (spec(), archive.clone(), dest.clone()));
    let spec_path = job.join("spec");
    std::fs::write(&spec_path, &spec_text).unwrap();

    let out = Command::new(helper_exe())
        .arg(HIDDEN_VERB)
        .arg(&spec_path)
        .output()
        .expect("the atpkg binary runs");
    assert!(
        out.status.success(),
        "stderr: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    let result = std::fs::read_to_string(job.join("result")).unwrap();
    assert_eq!(result, format!("ok\n{expected}\n"));
    assert_eq!(
        tree(&dest),
        tree(&reference),
        "the helper lays the same tree"
    );
    let _ = std::fs::remove_dir_all(&d);
}

/// The whole lane: submit → wait → read back. Same root, same tree. And the property the
/// lane exists for, asserted whenever this process is in a position to show it: a
/// tracked test process writes tagged files, the launchd job writes clean ones.
#[test]
fn the_launchd_lane_lays_an_identical_tree_and_a_clean_one_when_the_caller_is_tracked() {
    let d = scratch("lane");
    let archive = bundle_archive(&d);
    let reference = d.join("reference");
    std::fs::create_dir_all(&reference).unwrap();
    let expected = stage_payload_spec(&spec(), &archive, &reference).unwrap();

    let dest = d.join("incoming");
    std::fs::create_dir_all(&dest).unwrap();
    let staged = stage_untracked(&helper_exe(), &spec(), &archive, &dest, &d)
        .unwrap_or_else(|why| panic!("the lane must run on this macOS: {why}"));
    assert_eq!(staged.root, expected);
    // The job removed its own label (2026-09-13): soon nothing of this pid's is
    // registered. "Soon", not "now": the other tests in this binary run their own jobs
    // on other threads at the same time, and a job of theirs still RUNNING is not a
    // leak — a leak is a label that stays once every job has answered.
    let mine = format!("systems.alab.atpkg.stage-helper-{}-", std::process::id());
    let registered = || -> Vec<String> {
        let listed = Command::new("/bin/launchctl").arg("list").output().unwrap();
        String::from_utf8_lossy(&listed.stdout)
            .lines()
            .filter(|l| l.contains(&mine))
            .map(str::to_string)
            .collect()
    };
    let waited = std::time::Instant::now();
    let mut leaked = registered();
    while !leaked.is_empty() && waited.elapsed() < std::time::Duration::from_secs(20) {
        std::thread::sleep(std::time::Duration::from_millis(200));
        leaked = registered();
    }
    assert!(leaked.is_empty(), "labels still registered: {leaked:?}");
    assert!(
        !staged.witness_tagged,
        "the lane measures its own outcome, and it must be clean"
    );
    assert_eq!(tree(&dest), tree(&reference));
    // The job's scratch is gone once the lane has answered.
    let leftovers: Vec<_> = std::fs::read_dir(&d)
        .unwrap()
        .flatten()
        .map(|e| e.file_name().to_string_lossy().into_owned())
        .filter(|n| n.starts_with("stage-helper-"))
        .collect();
    assert!(
        leftovers.is_empty(),
        "job scratch left behind: {leftovers:?}"
    );

    let tracked = measure_tracked(&d).expect("a writable scratch is measurable");
    let by_this_process = reference.join("bin/tool");
    let by_the_job = dest.join("bin/tool");
    eprintln!(
        "this test process is {}; in-process file tagged={}, launchd-laid file tagged={}",
        if tracked {
            "provenance-TRACKED"
        } else {
            "untracked"
        },
        carries_provenance(&by_this_process),
        carries_provenance(&by_the_job)
    );
    assert_eq!(
        carries_provenance(&by_this_process),
        tracked,
        "the in-process lane's files carry the tag exactly when the process is tracked"
    );
    // The measured property. Under an untracked test process both are clean and this
    // is vacuous; under a tracked one (an agent's shell, a shell inside aterm.app) it is
    // the whole point.
    assert!(
        !carries_provenance(&by_the_job),
        "the launchd job's files must be clean (tracked caller: {tracked})"
    );
    let _ = std::fs::remove_dir_all(&d);
}

/// A helper that is not an atpkg/aterm binary is refused up front; one that is spelled
/// right but answers nothing is detected inside the exit grace — and either way the
/// destination is left empty, as the caller's policy requires (the default stages into
/// it in-process and records the fact; a refusal removes it).
#[test]
fn a_helper_that_cannot_answer_is_detected_and_the_destination_is_left_empty() {
    let d = scratch("noanswer");
    let archive = bundle_archive(&d);
    let dest = d.join("incoming");
    std::fs::create_dir_all(&dest).unwrap();

    // Wrong spelling: refused before anything is submitted.
    let why =
        stage_untracked(Path::new("/usr/bin/true"), &spec(), &archive, &dest, &d).unwrap_err();
    assert!(why.contains("not an atpkg/aterm binary"), "{why}");

    // Right spelling, silent binary: `/usr/bin/true` copied under the name `atpkg`
    // exits 0 without a result. Detected within the grace, destination emptied.
    let fake_dir = d.join("fake");
    std::fs::create_dir_all(&fake_dir).unwrap();
    let fake = fake_dir.join("atpkg");
    std::fs::copy("/usr/bin/true", &fake).unwrap();
    let started = std::time::Instant::now();
    let why = stage_untracked(&fake, &spec(), &archive, &dest, &d).unwrap_err();
    assert!(why.contains("exited without a result"), "{why}");
    assert!(why.contains("exit status 0"), "the fate is named: {why}");
    assert!(
        started.elapsed() < std::time::Duration::from_secs(20),
        "detection took {:?}",
        started.elapsed()
    );
    assert_eq!(
        std::fs::read_dir(&dest).unwrap().count(),
        0,
        "dest emptied for the caller's policy"
    );
    let _ = std::fs::remove_dir_all(&d);
}

/// `cat "$1" > "$2"` from a job LAUNCHD spawns — an untracked process whatever this test
/// process is — so the copy is clean (measured law; `crate::provenance`). Waits for a
/// `done` file, removes the label on every path.
fn launchd_cat(src: &Path, dst: &Path, dir: &Path) {
    let done = dir.join("cat.done");
    let label = format!("systems.alab.atpkg.test.cat.{}", std::process::id());
    let out = Command::new("/bin/launchctl")
        .args(["submit", "-l", &label, "--", "/bin/sh", "-c"])
        .arg("cat \"$1\" > \"$2\" && chmod 755 \"$2\"; : > \"$3\"")
        .arg("x")
        .arg(src)
        .arg(dst)
        .arg(&done)
        .output()
        .expect("launchctl runs");
    assert!(
        out.status.success(),
        "launchctl submit: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    let started = std::time::Instant::now();
    while !done.exists() {
        assert!(
            started.elapsed() < std::time::Duration::from_secs(60),
            "the launchd cat did not finish"
        );
        std::thread::sleep(std::time::Duration::from_millis(50));
    }
    let _ = Command::new("/bin/launchctl")
        .args(["remove", &label])
        .output();
}

/// HOW the job runs the helper is a measurement of the binary, and both ways lay clean
/// files. The test binary built from a tracked shell carries the tag and is COPIED; a
/// clean byte copy of it (made by a launchd job) is run IN PLACE — and that in-place run
/// lays the same tree, clean. Under an untracked shell the binary is already clean, both
/// plans read `ExecOriginal`, and the tagged half is vacuous: the tag cannot be minted by
/// hand, so this test says which case it exercised.
#[test]
fn the_helper_runs_in_place_when_clean_and_is_copied_when_tagged() {
    let d = scratch("plan");
    let exe = helper_exe();
    let tagged = carries_provenance(&exe);
    let plan = plan_for_exe(&exe).expect("the test binary can be inspected");
    assert_eq!(
        plan,
        if tagged {
            HelperPlan::CopyThenExec
        } else {
            HelperPlan::ExecOriginal
        },
        "the plan follows the measured tag ({})",
        exe.display()
    );

    let clean_dir = d.join("clean");
    std::fs::create_dir_all(&clean_dir).unwrap();
    let clean = clean_dir.join("atpkg");
    launchd_cat(&exe, &clean, &d);
    assert!(
        !carries_provenance(&clean),
        "a byte copy made by a launchd job is clean"
    );
    assert_eq!(plan_for_exe(&clean).unwrap(), HelperPlan::ExecOriginal);

    let archive = bundle_archive(&d);
    let reference = d.join("reference");
    std::fs::create_dir_all(&reference).unwrap();
    let expected = stage_payload_spec(&spec(), &archive, &reference).unwrap();
    let dest = d.join("incoming");
    std::fs::create_dir_all(&dest).unwrap();
    let staged = stage_untracked(&clean, &spec(), &archive, &dest, &d)
        .unwrap_or_else(|why| panic!("the in-place lane must run on this macOS: {why}"));
    assert_eq!(staged.root, expected);
    assert!(!staged.witness_tagged);
    assert_eq!(tree(&dest), tree(&reference));
    assert!(
        !carries_provenance(&dest.join("bin/tool")),
        "a clean helper run in place by launchd writes clean files"
    );
    eprintln!(
        "test binary tagged={tagged} → plan {plan:?}; clean copy → ExecOriginal, laid clean \
         (in-process reference tagged={})",
        carries_provenance(&reference.join("bin/tool"))
    );
    let _ = std::fs::remove_dir_all(&d);
}

/// A helper the kernel will not run is reported by its SIGNAL, not as "exited without a
/// result": a fake `atpkg` that SIGKILLs itself stands in for the copied bundle binary
/// the old lane died on (`aterm.app`'s own executable, killed at exec, empty stderr —
/// 2026-09-13). The destination is left empty for the caller's policy.
#[test]
fn a_helper_killed_at_exec_is_reported_by_its_signal() {
    use std::os::unix::fs::PermissionsExt as _;
    let d = scratch("killed");
    let archive = bundle_archive(&d);
    let dest = d.join("incoming");
    std::fs::create_dir_all(&dest).unwrap();
    let fake_dir = d.join("fake");
    std::fs::create_dir_all(&fake_dir).unwrap();
    let fake = fake_dir.join("atpkg");
    std::fs::write(&fake, "#!/bin/sh\nkill -9 $$\n").unwrap();
    std::fs::set_permissions(&fake, std::fs::Permissions::from_mode(0o755)).unwrap();
    let started = std::time::Instant::now();
    let why = stage_untracked(&fake, &spec(), &archive, &dest, &d).unwrap_err();
    assert!(why.contains("signal 9 (SIGKILL)"), "{why}");
    // The cause the lane KNOWS is named, and named as a candidate: this fixture kills
    // itself with `kill -9`, which is exactly one of the other causes the report must
    // leave open, so a report that called it a code-signature kill would be wrong here.
    assert!(why.contains("code-signature kill"), "{why}");
    assert!(why.contains("records the signal, not the reason"), "{why}");
    assert!(!why.contains("exited without a result"), "{why}");
    assert!(
        started.elapsed() < std::time::Duration::from_secs(20),
        "detection took {:?}",
        started.elapsed()
    );
    assert_eq!(std::fs::read_dir(&dest).unwrap().count(), 0);
    let _ = std::fs::remove_dir_all(&d);
}

/// A label left by a parent that died before its `Drop` — the shape two labels from one
/// dead test pid had on 2026-09-12/13 — is swept by the next job any process prepares.
/// The orphan is minted here with a pid that does not exist; the lane's next `prepare`
/// removes it.
#[test]
fn a_label_whose_owning_pid_is_dead_is_swept_by_the_next_job() {
    let d = scratch("sweep");
    // A pid nothing runs under. macOS pids stay below 99999, but under a full test
    // sweep's process churn a fixed 99998 WAS alive once (2026-09-18: the one red in an
    // otherwise green tree), so the fixture walks down from there to a pid `ps` cannot
    // find rather than asserting a single number's luck.
    let dead_pid = (99_990u32..=99_998)
        .rev()
        .find(|pid| {
            !Command::new("/bin/ps")
                .args(["-p", &pid.to_string()])
                .output()
                .unwrap()
                .status
                .success()
        })
        .expect("one of pids 99990..=99998 is dead");
    let orphan = format!("systems.alab.atpkg.stage-helper-{dead_pid}-0-deadbeef");
    let out = Command::new("/bin/launchctl")
        .args(["submit", "-l", &orphan, "--", "/usr/bin/true"])
        .output()
        .unwrap();
    assert!(
        out.status.success(),
        "launchctl submit: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    let listed = || -> bool {
        let out = Command::new("/bin/launchctl").arg("list").output().unwrap();
        String::from_utf8_lossy(&out.stdout).contains(&orphan)
    };
    // LAUNCHD IS ASYNCHRONOUS AT BOTH ENDS. `launchctl submit` returns once launchd has
    // taken the request, not once `launchctl list` shows the job, and `remove` likewise
    // returns before the label is gone. Measured 2026-09-17 inside the merge gate on a
    // loaded 4-core Intel Mac: this case failed at "the orphan is registered before the
    // sweep" — the submit had succeeded and the list simply had not caught up — while the
    // same test passed twice on the quiet machine. So both ends are bounded waits, and a
    // timeout says which end it was rather than blaming the sweep.
    let wait_listed = |want: bool, why: &str| {
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(60);
        while listed() != want {
            assert!(std::time::Instant::now() < deadline, "{why}");
            std::thread::sleep(std::time::Duration::from_millis(50));
        }
    };
    wait_listed(
        true,
        "launchd never listed the orphan label within 60 s, though `submit` reported \
         success — the fixture never stood, so this run says nothing about the sweep",
    );

    // Any lane call prepares a job, and preparing sweeps. A silent helper keeps it short.
    let archive = bundle_archive(&d);
    let dest = d.join("incoming");
    std::fs::create_dir_all(&dest).unwrap();
    let fake_dir = d.join("fake");
    std::fs::create_dir_all(&fake_dir).unwrap();
    let fake = fake_dir.join("atpkg");
    std::fs::copy("/usr/bin/true", &fake).unwrap();
    let _ = stage_untracked(&fake, &spec(), &archive, &dest, &d);
    wait_listed(
        false,
        "the orphan label was still listed 60 s after the next job's prepare: the sweep \
         did not remove it",
    );
    // Belt and braces for a failed assertion above: never leave it behind.
    let _ = Command::new("/bin/launchctl")
        .args(["remove", &orphan])
        .output();
    let _ = std::fs::remove_dir_all(&d);
}

/// A tagged BUNDLE executable — the shipped app's shape, `<bundle>.app/Contents/MacOS/
/// <exe>` beside an `Info.plist` — is run from a copy of its WHOLE bundle, made by the
/// job: a fixture `.app` around the CURRENT one-binary executable. Its name is
/// `aterm`, so the atpkg argv0 alias cannot hide a missing front-door dispatch of
/// `__stage-payload` or `__lay-files` (the installed 0.81 app lacks the latter).
/// Both verbs must actually answer. The copy carries the tag exactly
/// when this process is tracked (it is what wrote the copy), and then the plan reads
/// `CopyBundleThenExec` and the tree the lane stages is CLEAN; under an untracked shell
/// the copy is clean, the plan reads `ExecOriginal`, and the in-place half is what runs —
/// the tag cannot be minted by hand, so this test says which case it exercised. Either
/// way the lane stages the identical tree. Before 2026-09-14 the tracked case was
/// REFUSED, which on a self-updated app (its bundle tagged by its own updater) was every
/// install.
#[test]
fn the_current_one_binary_bundle_serves_both_hidden_verbs() {
    let current = current_aterm_exe();
    let d = scratch("bundle-copy");
    let bundle = d.join("fake.app");
    let contents = bundle.join("Contents");
    std::fs::create_dir_all(contents.join("MacOS")).unwrap();
    std::fs::write(contents.join("Info.plist"), b"<plist/>").unwrap();
    let exe = contents.join("MacOS").join("aterm");
    std::fs::copy(&current, &exe).unwrap();
    std::os::unix::fs::symlink("aterm", contents.join("MacOS").join("atpkg")).unwrap();
    let tracked = measure_tracked(&d).unwrap_or(false);
    let tagged = carries_provenance(&exe);
    let plan = plan_for_exe(&exe).expect("the fake bundle's binary can be inspected");
    assert_eq!(
        plan,
        if tagged {
            HelperPlan::CopyBundleThenExec {
                bundle: bundle.clone(),
            }
        } else {
            HelperPlan::ExecOriginal
        },
        "the plan follows the measured tag and the bundle shape ({})",
        exe.display()
    );

    let archive = bundle_archive(&d);
    let reference = d.join("reference");
    std::fs::create_dir_all(&reference).unwrap();
    let expected = stage_payload_spec(&spec(), &archive, &reference).unwrap();
    let dest = d.join("incoming");
    std::fs::create_dir_all(&dest).unwrap();
    let staged = stage_untracked(&exe, &spec(), &archive, &dest, &d)
        .unwrap_or_else(|why| panic!("the bundle lane must run on this macOS: {why}"));
    assert_eq!(staged.root, expected);
    assert!(!staged.witness_tagged, "what the job laid is clean");
    assert_eq!(tree(&dest), tree(&reference));
    assert!(
        !carries_provenance(&dest.join("bin/tool")),
        "a clean bundle copy run by launchd writes clean files"
    );
    let laid = d.join("laid-by-current-front-door");
    let body = b"#!/bin/sh\necho current-front-door\n";
    atpkg::lay::lay_untracked(
        &exe,
        &[atpkg::lay::Executable::new(&laid, body.to_vec())],
        &d,
    )
    .unwrap_or_else(|why| panic!("the current aterm front door must serve __lay-files: {why}"));
    assert_eq!(std::fs::read(&laid).unwrap(), body);
    assert!(
        !carries_provenance(&laid),
        "the current front door lays clean files through the same bundle lane"
    );
    if tracked {
        assert!(tagged, "a copy this tracked process made carries the tag");
        assert!(
            carries_provenance(&reference.join("bin/tool")),
            "the in-process reference is tagged when this process is tracked"
        );
    }
    eprintln!(
        "current artifact={}, test process tracked={tracked}, bundle binary tagged={tagged} → plan {plan:?}; \
         laid clean (in-process reference tagged={})",
        current.display(),
        carries_provenance(&reference.join("bin/tool"))
    );
    let _ = std::fs::remove_dir_all(&d);
}

/// The shipped app itself, when it is installed on this Mac and carries the tag — which a
/// self-updated or browser-downloaded `aterm.app` does — is the helper: the lane runs its
/// hidden verb from a copy of the whole notarized bundle (the copy keeps its seal; a lone
/// copy of the executable is killed at exec) and the file it lays is CLEAN. This is the
/// measurement of 2026-09-14 kept as an OPT-IN installed-artifact smoke, separate
/// from the current-source regression above. Run with:
/// ```sh
/// targo --unverified test -p atpkg --test untracked_stage \
///   the_shipped_app_bundle_when_tagged_lays_clean_files_from_a_whole_bundle_copy \
///   -- --ignored --exact --nocapture
/// ```
/// The installed version may predate the hidden verbs, in which case this smoke
/// truthfully fails for that artifact.
#[test]
#[ignore = "installed tagged/notarized artifact smoke; current-source coverage runs above"]
fn the_shipped_app_bundle_when_tagged_lays_clean_files_from_a_whole_bundle_copy() {
    let app = Path::new("/Applications/aterm.app/Contents/MacOS/aterm");
    assert!(
        app.is_file(),
        "installed-artifact smoke requires {}",
        app.display()
    );
    assert!(
        carries_provenance(app),
        "installed-artifact smoke requires a tagged bundle at {}; the tag cannot be minted by hand",
        app.display()
    );
    let d = scratch("shipped-app");
    assert_eq!(
        plan_for_exe(app).unwrap(),
        HelperPlan::CopyBundleThenExec {
            bundle: PathBuf::from("/Applications/aterm.app"),
        }
    );
    let laid = d.join("laid-by-the-app-copy");
    let files = vec![atpkg::lay::Executable::new(&laid, "#!/bin/sh\necho laid\n")];
    atpkg::lay::lay_untracked(app, &files, &d).unwrap_or_else(|why| {
        panic!("the shipped app must serve the lane from a whole-bundle copy: {why}")
    });
    assert_eq!(std::fs::read(&laid).unwrap(), b"#!/bin/sh\necho laid\n");
    assert!(
        !carries_provenance(&laid),
        "a file laid by the untracked copy of the tagged app is clean"
    );
    eprintln!(
        "the tagged shipped app ({}) laid a clean file through a whole-bundle copy",
        app.display()
    );
    let _ = std::fs::remove_dir_all(&d);
}
