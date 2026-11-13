// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

use super::*;

fn strings(flags: &[&str]) -> Vec<String> {
    flags.iter().map(|s| (*s).to_string()).collect()
}

#[test]
fn every_environment_source_of_rustflags_is_named() {
    for name in [
        "RUSTFLAGS",
        "CARGO_ENCODED_RUSTFLAGS",
        "CARGO_BUILD_RUSTFLAGS",
        "CARGO_TARGET_AARCH64_APPLE_DARWIN_RUSTFLAGS",
        "CARGO_TARGET_X86_64_UNKNOWN_LINUX_GNU_RUSTFLAGS",
    ] {
        assert!(is_env_source(name), "{name}");
    }
    for name in [
        "RUSTDOCFLAGS",
        "CARGO_TARGET_DIR",
        "CARGO_TARGET__RUSTFLAGS",
        "CARGO_TARGET_AARCH64_APPLE_DARWIN_LINKER",
        "CARGO_BUILD_JOBS",
        "XRUSTFLAGS",
    ] {
        assert!(!is_env_source(name), "{name}");
    }
}

#[test]
fn a_flag_that_sets_a_resolved_option_is_found_in_every_spelling() {
    let flags = strings(&[
        "-Zthreads=4",
        "-C",
        "opt-level=1",
        "-Cdebug-assertions=off",
        "--codegen",
        "overflow_checks=on",
        "--codegen=opt-level=s",
        "-Clto=thin",
        "--codegen",
        "codegen_units=8",
        "--codegen=codegen-units=4",
        "-O",
        "-C",
        "target-cpu=native",
        "-Cincremental=x",
    ]);
    assert_eq!(
        overrides(&flags),
        strings(&[
            "-C opt-level=1",
            "-Cdebug-assertions=off",
            "--codegen overflow_checks=on",
            "--codegen=opt-level=s",
            "-Clto=thin",
            "--codegen codegen_units=8",
            "--codegen=codegen-units=4",
            "-O",
        ])
    );
    assert!(overrides(&strings(&["-Zthreads=4"])).is_empty());
    assert!(overrides(&strings(&["-C"])).is_empty());
}

#[test]
fn config_rustflags_are_read_from_both_tables_as_cargo_takes_them() {
    let target = r#"{"target":{"cfg(trust_verify)":{"rustflags":["-Zthreads=4"]},"x86_64-pc-windows-gnu":{"ar":"x86_64-w64-mingw32-ar","linker":"x86_64-w64-mingw32-gcc"}}}"#;
    let sources = config_sources(None, Some(target)).expect("parses");
    assert_eq!(
        sources,
        [Source {
            key: "target.cfg(trust_verify).rustflags".to_string(),
            flags: strings(&["-Zthreads=4"]),
        }]
    );
    assert_eq!(refusal(&sources), None);
    assert_eq!(
        describe(&sources),
        "rustflags from cargo config: target.cfg(trust_verify).rustflags = `-Zthreads=4`"
    );
    let build = r#"{"build":{"jobs":4,"rustflags":"-C  opt-level=1 -Zthreads=8"}}"#;
    let sources = config_sources(Some(build), Some(target)).expect("parses");
    assert_eq!(
        sources[0].flags,
        strings(&["-C", "opt-level=1", "-Zthreads=8"])
    );
    let why = refusal(&sources).expect("refused");
    assert!(
        why.contains("build.rustflags sets `-C opt-level=1`"),
        "{why}"
    );
    assert!(!why.contains("trust_verify"), "{why}");
    assert_eq!(config_sources(None, None), Ok(Vec::new()));
    assert_eq!(describe(&[]), "rustflags from cargo config: none");
    for bad in [
        r#"{"build":{"rustflags":3}}"#,
        r#"{"build":{"rustflags":["-O",3]}}"#,
        "not json",
    ] {
        assert!(config_sources(Some(bad), None).is_err(), "{bad}");
    }
}
