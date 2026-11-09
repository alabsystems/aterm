use std::os::unix::fs::PermissionsExt;
use std::time::Duration;

use super::*;

fn deadline() -> Instant {
    deadline_from(Instant::now())
}

const NATIVE_INPUT_HANG: Duration = Duration::from_secs(60);

fn deadline_from(started: Instant) -> Instant {
    started + NATIVE_INPUT_HANG
}

#[test]
fn actual_native_system_command_holds_original_image_without_an_sdk() {
    const NAME: &str = "program::system_command::tests::actual_native_system_command_holds_original_image_without_an_sdk";
    const ACTOR: &str = "TRUST_SYSTEM_COMMAND_ORIGINAL_ACTOR";
    const STARTED: &str = "native-system-command-original-started";
    if std::env::var_os(ACTOR).as_deref() != Some(std::ffi::OsStr::new("1")) {
        // Other native-program fixtures deliberately mutate current_exe's
        // parent. Select an actual loaded child in its own stable private root;
        // keep the production directory-identity checks fully strict.
        let temp = tempfile::tempdir().unwrap();
        let root = std::fs::canonicalize(temp.path()).unwrap();
        let source = std::env::current_exe().unwrap();
        let executable = root.join(source.file_name().unwrap());
        std::fs::copy(&source, &executable).unwrap();
        std::fs::set_permissions(&executable, std::fs::Permissions::from_mode(0o555)).unwrap();
        let mut retained = aterm_uds::deadline::KeptUntilRan::new(temp.keep());
        let mut command = aterm_uds::spawnfd::command(&executable);
        command.args(["--exact", NAME, "--nocapture"]).env(ACTOR, "1");
        let output = aterm_uds::deadline::output_within(&mut command, Duration::from_secs(60)).unwrap();
        if String::from_utf8_lossy(&output.stdout).lines().any(|line| line.ends_with(STARTED)) {
            retained.ran();
        }
        assert!(output.status.success(), "actual native command actor failed: {output:?}");
        assert!(
            String::from_utf8_lossy(&output.stdout)
                .contains("native-system-command-original-checked")
        );
        return;
    }
    use std::io::Write as _;
    println!("{STARTED}");
    std::io::stdout().flush().unwrap();
    assert!(aterm_uds::test_binary::is_test_binary(&std::env::current_exe().unwrap()), "native actor must retain Cargo test isolation");
    let original = std::fs::canonicalize(std::env::current_exe().unwrap()).unwrap();
    let held = HeldSystemCommand::open(&original, deadline()).unwrap();
    assert!(held.observation().runtime.is_empty());
    assert!(held.observation().loader_paths.is_empty());
    assert_eq!(held.observation().policy, "native-command/macho-arm64-system-only-v1");
    held.validate_loaded(process::read_identity(std::process::id()).unwrap(), deadline()).unwrap();
    // An actual change to this selected directory still invalidates the held
    // input. Isolation does not turn unrelated selected-root writes into reuse.
    std::fs::create_dir(original.parent().unwrap().join("actual-directory-change")).unwrap();
    let error = held.validate(deadline()).unwrap_err();
    assert_eq!(error.kind(), std::io::ErrorKind::InvalidData);
    assert_eq!(error.to_string(), "original native runtime directory changed");
    println!("native-system-command-original-checked");
}

#[test]
fn exact_original_input_survives_neither_byte_drift_nor_same_byte_replacement() {
    original_input_with_age(Duration::ZERO);
}

#[test]
fn required_native_input_keeps_its_original_start_without_a_latency_budget() {
    original_input_with_age(Duration::from_secs(31));
}

fn original_input_with_age(age: Duration) {
    let temp = tempfile::tempdir().unwrap();
    let root = std::fs::canonicalize(temp.path()).unwrap();
    let path = root.join("native-command");
    std::fs::copy(std::env::current_exe().unwrap(), &path).unwrap();
    std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o700)).unwrap();
    let original_deadline = deadline_from(Instant::now() - age);
    let original = HeldSystemCommand::open(&path, original_deadline).unwrap();
    let replacement = root.join("replacement");
    std::fs::copy(&path, &replacement).unwrap();
    std::fs::rename(&replacement, &path).unwrap();
    assert!(original.validate(deadline()).is_err());
    let current = HeldSystemCommand::open(&path, deadline()).unwrap();
    use std::os::unix::fs::FileExt as _;
    let write = std::fs::OpenOptions::new().write(true).open(&path).unwrap();
    write.write_all_at(b"X", 0).unwrap();
    assert!(current.validate(deadline()).is_err());
}

#[test]
fn unknown_script_and_expired_input_refuse() {
    let temp = tempfile::tempdir().unwrap();
    let path = std::fs::canonicalize(temp.path()).unwrap().join("script");
    std::fs::write(&path, b"#!/bin/sh\nexit 0\n").unwrap();
    std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o700)).unwrap();
    assert!(HeldSystemCommand::open(&path, deadline()).is_err());
    assert!(HeldSystemCommand::open(&std::env::current_exe().unwrap(), Instant::now()).is_err());
}
