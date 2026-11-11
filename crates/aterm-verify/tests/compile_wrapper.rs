// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0

//! Cargo's wrapper dispatch precedes the ordinary verifier CLI.

#![cfg(unix)]

#[test]
fn binary_dispatches_compiler_probes_only_with_the_complete_wrapper_tuple() {
    let verify = aterm_tempfile::bin_exe("aterm-verify");
    aterm_spec::wait::first_exec(&verify, &["--help"]);
    let dir = aterm_tempfile::tempdir().unwrap();
    for wrapper in ["complete", "marker-only", "none"] {
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
        for name in [
            "ATERM_VERIFY_UNIT_WRAPPER",
            "ATERM_VERIFY_UNIT_BOOK",
            "ATERM_VERIFY_UNIT_CHECKOUT",
            "ATERM_VERIFY_UNIT_MODE",
            "ATERM_VERIFY_UNIT_PENDING",
            "ATERM_VERIFY_UNIT_RUN",
            "ATERM_VERIFY_UNIT_STAGE",
            "ATERM_VERIFY_UNIT_UNKNOWN",
        ] {
            command.env_remove(name);
        }
        match wrapper {
            "complete" => {
                command
                    .env("ATERM_VERIFY_UNIT_WRAPPER", "1")
                    .env("ATERM_VERIFY_UNIT_BOOK", dir.path().join("units.json"))
                    .env("ATERM_VERIFY_UNIT_CHECKOUT", "/fixture")
                    .env("ATERM_VERIFY_UNIT_MODE", "record")
                    .env("ATERM_VERIFY_UNIT_PENDING", dir.path().join("pending.json"))
                    .env("ATERM_VERIFY_UNIT_RUN", "probe")
                    .env("ATERM_VERIFY_UNIT_STAGE", "TestCompile unit")
                    .env("ATERM_VERIFY_UNIT_UNKNOWN", (16_u64 << 30).to_string());
            }
            "marker-only" => {
                command.env("ATERM_VERIFY_UNIT_WRAPPER", "1");
            }
            "none" => {
                command.env_remove("ATERM_VERIFY_UNIT_WRAPPER");
            }
            _ => unreachable!(),
        }
        let output = aterm_uds::deadline::output_until(
            &mut command,
            std::time::Instant::now() + aterm_spec::wait::MUST_HAPPEN,
        )
        .unwrap();
        assert_eq!(output.status.success(), wrapper == "complete", "{output:?}");
    }
}
