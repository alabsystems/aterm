// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

use super::{Input, Scenario, control_refused, dimensions, finish, record_row, run};

#[cfg(unix)]
#[test]
fn a_closed_progress_reader_preserves_the_measurement_and_the_run() {
    let (mut progress, reader) = std::os::unix::net::UnixStream::pair().unwrap();
    drop(reader);
    let mut records = Vec::new();
    record_row(&mut records, &mut progress, "completed\n").unwrap();
    assert_eq!(records, b"completed\n");
}

#[test]
fn an_unwritable_journal_does_not_announce_an_unrecorded_row() {
    let mut storage = [];
    let mut records = std::io::Cursor::new(&mut storage[..]);
    let mut progress = Vec::new();
    assert!(record_row(&mut records, &mut progress, "completed\n").is_err());
    assert!(progress.is_empty());
}

#[test]
fn a_progress_write_error_keeps_the_record_but_fails_the_write() {
    let mut records = Vec::new();
    let mut storage = [];
    let mut progress = std::io::Cursor::new(&mut storage[..]);
    assert!(record_row(&mut records, &mut progress, "completed\n").is_err());
    assert_eq!(records, b"completed\n");
}

#[cfg(unix)]
#[test]
fn an_unrepresentable_native_input_path_is_an_error_instead_of_a_panic() {
    use std::os::unix::ffi::OsStringExt as _;
    let path = std::path::PathBuf::from(std::ffi::OsString::from_vec(b"runner\xff".to_vec()));
    let input = Input {
        path: &path,
        sha256: "0".repeat(64),
    };
    assert!(input.record().is_err());
}

#[test]
fn replacing_input_bytes_without_changing_length_invalidates_the_identity() {
    let scratch = aterm_tempfile::tempdir().unwrap();
    let path = scratch.path().join("runner");
    std::fs::write(&path, b"before").unwrap();
    let input = Input::read(&path).unwrap();
    assert!(input.recheck().is_ok());
    std::fs::write(&path, b"after!").unwrap();
    assert!(input.recheck().is_err());
    std::fs::remove_file(&path).unwrap();
    assert!(input.recheck().is_err());
}

#[test]
fn a_receipt_write_failure_keeps_the_original_failure() {
    let scratch = aterm_tempfile::tempdir().unwrap();
    let output = scratch.path().join("not-a-directory");
    std::fs::write(&output, b"occupied").unwrap();
    let result = finish(&output, Err(crate::Fail::exit(7, "original cause")));
    let fail = result.err().unwrap();
    assert_eq!(fail.code, 7);
    assert!(fail.msg.unwrap().contains("original cause"));
}

#[test]
fn a_runner_that_cannot_start_leaves_a_failed_result_receipt() {
    let scratch = aterm_tempfile::tempdir().unwrap();
    let runner = scratch.path().join("not-executable");
    std::fs::write(&runner, b"not an executable\n").unwrap();
    let after = scratch.path().join("candidate");
    std::fs::write(&after, b"candidate bytes\n").unwrap();
    let app = scratch.path().join("app");
    std::fs::write(&app, b"shared app bytes\n").unwrap();
    let output = scratch.path().join("results");
    let args = [
        "--before".into(),
        runner.to_string_lossy().into_owned(),
        "--after".into(),
        after.to_string_lossy().into_owned(),
        "--aterm".into(),
        app.to_string_lossy().into_owned(),
        "--output".into(),
        output.to_string_lossy().into_owned(),
        "--samples".into(),
        "1".into(),
    ];
    assert!(run(&args).is_err());
    let text = std::fs::read_to_string(output.join("result.json")).unwrap();
    let receipt: aterm_json::Value = aterm_json::from_str(&text).unwrap();
    assert_eq!(receipt["state"].as_str(), Some("failed"));
    let inputs: aterm_json::Value =
        aterm_json::from_str(&std::fs::read_to_string(output.join("inputs.json")).unwrap())
            .unwrap();
    assert_eq!(inputs["samples"].as_u64(), Some(1));
    assert_eq!(inputs["schema"].as_u64(), Some(2));
    let observer = std::env::current_exe().unwrap();
    for (arm, path) in [
        ("before", &runner),
        ("after", &after),
        ("aterm", &app),
        ("observer", &observer),
    ] {
        let canonical_path = std::fs::canonicalize(path).unwrap();
        assert_eq!(inputs[arm]["path"].as_str(), canonical_path.to_str());
        assert_eq!(
            inputs[arm]["sha256"].as_str(),
            Some(aterm_update_core::sha256_file(path).unwrap().as_str())
        );
    }
    assert!(
        receipt["detail"]
            .as_str()
            .unwrap()
            .contains("warmup-before-0")
    );
}

#[test]
fn a_fifo_capture_is_refused_without_waiting_for_a_writer() {
    let scratch = aterm_tempfile::tempdir().unwrap();
    let image = scratch.path().join("frame.png");
    crate::sys::make_fifo(&image).unwrap();
    let (done, completed) = std::sync::mpsc::sync_channel(1);
    let worker = std::thread::spawn(move || {
        done.send(dimensions(&image)).unwrap();
    });
    let result = completed
        .recv_timeout(std::time::Duration::from_secs(60))
        .expect("a capture FIFO must be refused without waiting for a writer");
    worker.join().unwrap();
    assert!(result.is_err());
}

#[test]
fn image_validation_distinguishes_absent_valid_and_corrupt_files() {
    let scratch = aterm_tempfile::tempdir().unwrap();
    let image = scratch.path().join("frame.png");
    assert_eq!(dimensions(&image).unwrap(), None);
    {
        let mut encoder = aterm_png::Encoder::new(std::fs::File::create(&image).unwrap(), 1, 1);
        encoder.set_color(aterm_png::ColorType::Rgb);
        encoder.set_depth(aterm_png::BitDepth::Eight);
        let mut writer = encoder.write_header().unwrap();
        writer.write_image_data(&[1, 2, 3]).unwrap();
        writer.finish().unwrap();
    }
    assert_eq!(dimensions(&image).unwrap(), Some((1, 1)));
    std::fs::write(&image, b"not a PNG").unwrap();
    assert!(dimensions(&image).is_err());
}

#[test]
fn a_failed_fixture_is_not_an_expected_control_refusal() {
    for reply in ["ERR refused", "aterm-ctl: ERR refused"] {
        assert!(control_refused(
            &format!("ctl spawn split=h -> {reply}\n"),
            "spawn split=h"
        ));
    }
    assert!(!control_refused(
        "ctl spawn split=h -> OK payload=ERR refused\n",
        "spawn split=h"
    ));
    assert!(!control_refused(
        "ctl spawn split=v -> aterm-ctl: ERR refused\n",
        "spawn split=h"
    ));
    assert!(!Scenario::PreRefusal.valid(Some(1), None, false));
    assert!(Scenario::PreRefusal.valid(Some(1), None, true));
    assert!(!Scenario::PostRefusal.valid(Some(1), Some((794, 462)), false));
    assert!(Scenario::PostRefusal.valid(Some(1), Some((794, 462)), true));
    for code in [
        Some(0),
        Some(2),
        Some(7),
        Some(101),
        Some(126),
        Some(127),
        Some(130),
        None,
    ] {
        assert!(!Scenario::PreRefusal.valid(code, None, true), "{code:?}");
        assert!(
            !Scenario::PostRefusal.valid(code, Some((794, 462)), true),
            "{code:?}"
        );
    }
    assert!(!Scenario::NativeOnly.valid(Some(1), None, false));
    assert!(Scenario::NativeOnly.valid(Some(0), Some((794, 462)), false));
}
