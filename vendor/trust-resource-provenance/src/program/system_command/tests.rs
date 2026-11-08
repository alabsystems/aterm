use std::os::unix::fs::PermissionsExt;
use std::time::Duration;

use super::*;

fn deadline() -> Instant {
    Instant::now() + Duration::from_secs(30)
}

#[test]
fn actual_native_system_command_holds_original_image_without_an_sdk() {
    const NAME: &str = "program::system_command::tests::actual_native_system_command_holds_original_image_without_an_sdk";
    const ACTOR: &str = "TRUST_SYSTEM_COMMAND_ORIGINAL_ACTOR";
    if std::env::var_os(ACTOR).as_deref() != Some(std::ffi::OsStr::new("1")) {
        // Other native-program fixtures deliberately mutate current_exe's
        // parent. Select an actual loaded child in its own stable private root;
        // keep the production directory-identity checks fully strict.
        let temp = tempfile::tempdir().unwrap();
        let root = std::fs::canonicalize(temp.path()).unwrap();
        let executable = root.join("native-system-command-test");
        std::fs::copy(std::env::current_exe().unwrap(), &executable).unwrap();
        std::fs::set_permissions(&executable, std::fs::Permissions::from_mode(0o555)).unwrap();
        let output = std::process::Command::new(&executable)
            .args(["--exact", NAME, "--nocapture"])
            .env(ACTOR, "1")
            .output()
            .unwrap();
        assert!(output.status.success(), "actual native command actor failed: {output:?}");
        assert!(
            String::from_utf8_lossy(&output.stdout)
                .contains("native-system-command-original-checked")
        );
        return;
    }
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
    let temp = tempfile::tempdir().unwrap();
    let root = std::fs::canonicalize(temp.path()).unwrap();
    let path = root.join("native-command");
    std::fs::copy(std::env::current_exe().unwrap(), &path).unwrap();
    std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o700)).unwrap();
    let original = HeldSystemCommand::open(&path, deadline()).unwrap();
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
