// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! The actual debug executable answers compile facts before creating any application state.

use std::time::Duration;

#[test]
fn compile_facts_are_headless_and_match_the_actual_executable_section() {
    let tmp = aterm_tempfile::tempdir().unwrap();
    let home = tmp.path().join("must-not-exist");
    let executable = aterm_tempfile::bin_exe("aterm");
    let mut command = aterm_uds::spawnfd::command(&executable);
    command
        .arg(aterm_stamp::facts::ARG)
        .env("HOME", &home)
        .env("XDG_CONFIG_HOME", &home)
        .env("XDG_STATE_HOME", &home)
        .env("XDG_RUNTIME_DIR", &home)
        .env(aterm_types::world::MARKER, &home);
    let output = aterm_uds::deadline::output_while_process_progressing(
        &mut command,
        Duration::from_secs(60),
        "the executable's headless compile-facts reply",
    )
    .unwrap();
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    assert!(output.stderr.is_empty());
    let facts = aterm_stamp::facts::decode(&output.stdout).unwrap();
    assert!(facts.debug_assertions);
    assert_eq!(facts.target_arch, std::env::consts::ARCH);
    assert!(
        !home.exists(),
        "compile facts must precede every state/setup lane"
    );
    #[cfg(target_os = "macos")]
    {
        let records =
            aterm_stamp::facts::macho_records(&mut std::fs::File::open(&executable).unwrap())
                .unwrap();
        let active = records
            .iter()
            .find(|record| {
                aterm_stamp::facts::decode(record.as_slice())
                    .unwrap()
                    .target_arch
                    == std::env::consts::ARCH
            })
            .unwrap();
        assert_eq!(
            output.stdout.as_slice(),
            active,
            "the reply is the exact immutable section used by offline admission"
        );
    }
    command.arg("unexpected");
    let refused = aterm_uds::deadline::output_while_process_progressing(
        &mut command,
        Duration::from_secs(60),
        "the executable's malformed compile-facts rejection",
    )
    .unwrap();
    assert_eq!(refused.status.code(), Some(2));
    assert!(refused.stdout.is_empty());
    assert!(
        !home.exists(),
        "malformed build-facts requests must also stop before startup"
    );
}
