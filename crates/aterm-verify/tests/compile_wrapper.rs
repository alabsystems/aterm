// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0

//! Cargo's wrapper dispatch precedes the ordinary verifier CLI.

#![cfg(unix)]

#[test]
fn binary_dispatches_compiler_probes_only_with_the_wrapper_marker() {
    let verify = aterm_tempfile::bin_exe("aterm-verify");
    aterm_spec::wait::first_exec(&verify, &["--help"]);
    for wrapper in [true, false] {
        let mut command = aterm_uds::spawnfd::command(&verify);
        command.args([
            "/usr/bin/true",
            "--crate-name",
            "probe",
            "--crate-type",
            "lib",
            "--print",
            "file-names",
        ]);
        if wrapper {
            command.env("ATERM_VERIFY_UNIT_WRAPPER", "1");
        } else {
            command.env_remove("ATERM_VERIFY_UNIT_WRAPPER");
        }
        let output = aterm_uds::deadline::output_until(
            &mut command,
            std::time::Instant::now() + aterm_spec::wait::MUST_HAPPEN,
        )
        .unwrap();
        assert_eq!(output.status.success(), wrapper, "{output:?}");
    }
}
