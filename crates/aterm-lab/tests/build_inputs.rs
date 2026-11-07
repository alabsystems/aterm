// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Exercise the real build script's Cargo directives: the lab reads identity at run time,
//! so neither git state nor another crate belongs in its build inputs.

mod build_script {
    include!("../build.rs");

    pub(super) fn emit() {
        main();
    }
}

#[test]
fn the_lab_build_script_watches_only_itself() {
    if std::env::var_os("ATERM_LAB_BUILD_INPUT_PROBE").is_some() {
        build_script::emit();
        return;
    }
    let mut command = aterm_uds::spawnfd::command(std::env::current_exe().unwrap());
    command
        .args([
            "--exact",
            "the_lab_build_script_watches_only_itself",
            "--nocapture",
        ])
        .env("ATERM_LAB_BUILD_INPUT_PROBE", "1")
        .env("PROFILE", "debug")
        .env("OPT_LEVEL", "0")
        .env_remove(aterm_stamp::input::COMMIT)
        .current_dir(aterm_tempfile::manifest_dir());
    let output =
        aterm_uds::deadline::output_within(&mut command, aterm_spec::wait::MUST_HAPPEN).unwrap();
    assert!(output.status.success(), "{output:?}");
    let stdout = String::from_utf8(output.stdout).unwrap();
    let inputs: Vec<_> = stdout
        .lines()
        .filter_map(|line| line.split_once("cargo:rerun-if-").map(|(_, rest)| rest))
        .collect();
    assert_eq!(inputs, ["changed=build.rs"], "{stdout}");
    assert!(stdout.contains("cargo:rustc-env=ATERM_LAB_PROFILE=debug opt-level=0"));
}
