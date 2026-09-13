// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0

//! The untracked staging lane end to end (`atpkg::stage_helper`), against the REAL
//! `atpkg` binary this build produced: the hidden verb answers through the result file
//! with the same `tree_root` the in-process lane folds over the same bytes; the whole
//! launchd round trip lays down an identical tree; a helper that cannot speak the verb
//! is detected inside the exit grace and leaves the destination empty for the caller's
//! policy to act on (refuse by default, an in-process stage under the escape hatch);
//! and — when the test process happens to be provenance-tracked, which is the case that
//! matters and the one this lane exists for — the files the job laid down are CLEAN
//! while the ones this process writes are tagged.
//!
//! macOS only: the tag and launchd exist nowhere else.

#![cfg(target_os = "macos")]

use std::collections::BTreeMap;
use std::io::Write as _;
use std::path::{Path, PathBuf};
use std::process::Command;

use atpkg::install::{StageSpec, stage_payload_spec};
use atpkg::provenance::{carries_provenance, measure_tracked};
use atpkg::stage_helper::{HIDDEN_VERB, decode_spec, encode_spec, stage_untracked};

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
/// destination is left empty, as the caller's policy requires (a refused stage removes
/// it; the escape hatch stages into it in-process).
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
