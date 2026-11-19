// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

use std::time::Instant;

fn request(directory: &std::path::Path) {
    std::fs::write(directory.join("driver-request.json"), aterm_json::json!({
        "schema":"aterm-runtime-driver-request-v1", "nonce":"0123456789abcdef0123456789abcdef",
        "directory":directory.display().to_string(), "label":"systems.alab.aterm.runtime.standalone"
    }).to_string()).unwrap();
}

#[test]
fn private_driver_runs_by_executable_path_without_cargo_runtime_context() {
    let tmp = aterm_tempfile::tempdir().unwrap();
    request(tmp.path());
    assert!(!tmp.path().join("driver-binding.json").exists());
    let mut command = aterm_uds::spawnfd::command(aterm_tempfile::bin_exe("aterm-verify"));
    command
        .args(["--runtime-capture", "run"])
        .arg(tmp.path())
        .args(["60", "test -f driver-binding.json || exit 7; printf standalone; printf diagnostics >&2; exit 1"])
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
    let binding: aterm_json::Value =
        aterm_json::from_slice(&std::fs::read(tmp.path().join("driver-binding.json")).unwrap())
            .unwrap();
    let request: aterm_json::Value =
        aterm_json::from_slice(&std::fs::read(tmp.path().join("driver-request.json")).unwrap())
            .unwrap();
    assert_eq!(
        binding["schema"].as_str(),
        Some("aterm-runtime-driver-binding-v1")
    );
    assert_eq!(binding["nonce"], request["nonce"]);
    assert_eq!(binding["directory"], request["directory"]);
    let (uid, label) = binding["target"]
        .as_str()
        .unwrap()
        .strip_prefix("gui/")
        .unwrap()
        .split_once('/')
        .unwrap();
    assert!(uid.parse::<u32>().is_ok());
    assert_eq!(Some(label), request["label"].as_str());
    assert!(aterm_uds::process::Identity::parse(binding["identity"].as_str().unwrap()).is_some());
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
    request(tmp.path());
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
