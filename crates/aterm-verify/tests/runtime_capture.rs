// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

use std::time::Instant;

#[test]
fn private_driver_runs_by_executable_path_without_cargo_runtime_context() {
    let tmp = aterm_tempfile::tempdir().unwrap();
    let mut command = aterm_uds::spawnfd::command(aterm_tempfile::bin_exe("aterm-verify"));
    command
        .args(["--runtime-capture", "run"])
        .arg(tmp.path())
        .args(["60", "printf standalone; printf diagnostics >&2; exit 1"])
        .env_remove("CARGO_MANIFEST_DIR")
        .env_remove("CARGO_TARGET_DIR")
        .current_dir(tmp.path());
    let output = aterm_uds::deadline::output_with_input_until(
        &mut command,
        aterm_uds::secret::Secret::new(&[]),
        Instant::now() + aterm_spec::wait::MUST_HAPPEN,
        aterm_uds::deadline::MAX_COMPLETE_STDOUT_BYTES,
    )
    .unwrap();
    assert_eq!(output.status.code(), Some(1), "{output:?}");
    assert_eq!(
        std::fs::read(tmp.path().join("stdout")).unwrap(),
        b"standalone"
    );
    assert_eq!(
        std::fs::read(tmp.path().join("stderr")).unwrap(),
        b"diagnostics"
    );
    let receipt: aterm_json::Value =
        aterm_json::from_slice(&std::fs::read(tmp.path().join("capture.json")).unwrap()).unwrap();
    assert_eq!(receipt["complete"].as_bool(), Some(true));
    assert_eq!(receipt["exit"].as_i64(), Some(1));
    assert!(receipt["signal"].is_null());
    assert_eq!(receipt["stdout_bytes"].as_u64(), Some(10));
    assert_eq!(receipt["stderr_bytes"].as_u64(), Some(11));
}

#[test]
fn an_existing_completion_refuses_before_the_producer_can_launch() {
    let tmp = aterm_tempfile::tempdir().unwrap();
    let stale = b"{\"schema\":\"aterm-runtime-capture-v1\",\"complete\":true,\"exit\":0,\"signal\":null,\"stdout_bytes\":0,\"stderr_bytes\":0,\"error\":null}\n";
    std::fs::write(tmp.path().join("capture.json"), stale).unwrap();
    let mut command = aterm_uds::spawnfd::command(aterm_tempfile::bin_exe("aterm-verify"));
    command
        .args(["--runtime-capture", "run"])
        .arg(tmp.path())
        .args(["60", "printf invoked > producer-ran"])
        .env_remove("CARGO_MANIFEST_DIR")
        .env_remove("CARGO_TARGET_DIR")
        .current_dir(tmp.path());
    let output = aterm_uds::deadline::output_with_input_until(
        &mut command,
        aterm_uds::secret::Secret::new(&[]),
        Instant::now() + aterm_spec::wait::MUST_HAPPEN,
        aterm_uds::deadline::MAX_COMPLETE_STDOUT_BYTES,
    )
    .unwrap();
    assert_eq!(
        output.status.code(),
        Some(aterm_verify::exit::COULD_NOT_RUN)
    );
    assert!(!tmp.path().join("producer-ran").exists());
    assert!(!tmp.path().join("started").exists());
    assert!(!tmp.path().join("stdout").exists());
    assert_eq!(
        std::fs::read(tmp.path().join("capture.json")).unwrap(),
        stale
    );
}
