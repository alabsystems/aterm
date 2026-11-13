// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

use super::*;

fn frame(lit: usize) -> Frame {
    let mut rgba = [0, 0, 0, 255].repeat(6);
    rgba[4 * lit..4 * lit + 3].copy_from_slice(&[255, 255, 255]);
    Frame {
        width: 3,
        height: 2,
        rgba,
    }
}

fn recording() -> Recording {
    Recording {
        module: "aterm-gpu-web".to_string(),
        browser: "HeadlessChrome/1".to_string(),
        adapter: "ANGLE (test)".to_string(),
        frames: vec![
            ("parity-0".to_string(), frame(0)),
            ("parity-1".to_string(), frame(1)),
            ("diff-0".to_string(), frame(2)),
            ("diff-1".to_string(), frame(3)),
        ],
    }
}

fn run_of(rec: &Recording) -> Vec<(String, &Frame)> {
    rec.frames.iter().map(|(n, f)| (n.clone(), f)).collect()
}

/// A laid site carries the committed recording, which reads back as the checkout's own: a
/// drive handed only the site holds its frames to it. NEGATIVE CONTROL: before the lay, the
/// site has none to read.
#[test]
fn a_laid_site_carries_the_committed_recording() {
    let root = aterm_tempfile::manifest_dir().join("../..");
    let site = aterm_tempfile::tempdir().expect("tempdir");
    assert!(read(&site.path().join(IN_SITE)).is_err());
    lay(&root, site.path()).expect("lay");
    let committed = read(&checkout_dir(&root)).expect("the committed recording");
    assert_eq!(
        read(&site.path().join(IN_SITE)).expect("the site's copy"),
        committed
    );
}

#[test]
fn a_recording_round_trips_and_a_tampered_png_is_refused() {
    let dir = aterm_tempfile::tempdir().expect("tempdir");
    let rec = recording();
    write(dir.path(), &rec).expect("write");
    assert_eq!(read(dir.path()).expect("read"), rec);
    let mut other = frame(2);
    other.rgba[0] = 7;
    std::fs::write(dir.path().join("parity-1.png"), png(&other).expect("png")).expect("plant");
    let err = read(dir.path()).expect_err("a tampered frame");
    assert!(err.contains("do not hash"), "{err}");
}

#[test]
fn the_recording_s_adapter_is_held_byte_for_byte_another_within_the_tolerance() {
    let rec = recording();
    let v = judge(&rec, &rec.adapter, &run_of(&rec));
    assert!(v.failures.is_empty(), "{:?}", v.failures);
    assert!(v.lines[0].contains("byte-identical"), "{:?}", v.lines);
    let mut off = recording();
    off.frames[2].1.rgba[1] = 1;
    let v = judge(&rec, &rec.adapter, &run_of(&off));
    assert_eq!(v.failures.len(), 1, "{:?}", v.failures);
    assert!(
        v.failures[0].starts_with("diff-0: differs"),
        "{:?}",
        v.failures
    );
    let v = judge(&rec, "SwiftShader", &run_of(&off));
    assert!(v.failures.is_empty(), "{:?}", v.failures);
    assert!(v.lines[0].contains("within 8"), "{:?}", v.lines);
}

#[test]
fn a_stalled_page_trips_the_control_and_a_missing_frame_is_named() {
    let rec = recording();
    let mut stalled = recording();
    stalled.frames[1].1 = frame(0);
    let v = judge(&rec, &rec.adapter, &run_of(&stalled));
    assert!(
        v.failures
            .iter()
            .any(|f| f.starts_with("NEGATIVE CONTROL: parity-1")),
        "{:?}",
        v.failures
    );
    let short: Vec<(String, &Frame)> = run_of(&rec).into_iter().take(3).collect();
    let v = judge(&rec, &rec.adapter, &short);
    assert!(
        v.failures[0].contains("the recording holds"),
        "{:?}",
        v.failures
    );
}
