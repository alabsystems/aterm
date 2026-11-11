// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

#[test]
fn explicit_cell_selection_preserves_default_and_refuses_unknown() {
    assert_eq!(super::selected_cells(None).unwrap().len(), 4);
    assert_eq!(super::selected_cells(Some("mac")).unwrap()[0].key, "mac");
    assert_eq!(super::selected_cells(Some("mac")).unwrap().len(), 1);
    assert!(super::selected_cells(Some("native-guess")).is_err());
}
use std::ffi::OsStr;

use super::*;

#[test]
fn a_supplied_dump_is_found_by_its_triple() {
    let dir = aterm_tempfile::tempdir().expect("tempdir");
    assert_eq!(supplied_doc(Some(dir.path()), &CELLS[0]), None);
    let name = format!("libc-{REFERENCE_VERSION}-aarch64-apple-darwin.json");
    std::fs::write(dir.path().join(&name), "{}").expect("write");
    assert_eq!(
        supplied_doc(Some(dir.path()), &CELLS[0]),
        Some(dir.path().join(name))
    );
    assert_eq!(supplied_doc(Some(dir.path()), &CELLS[1]), None);
    assert_eq!(supplied_doc(None, &CELLS[0]), None);
}

/// A supplied dump that is a symlink to a real one is read, as `Path::is_file` follows it.
#[cfg(unix)]
#[test]
fn a_supplied_dump_reached_through_a_symlink_is_found() {
    let dir = aterm_tempfile::tempdir().expect("tempdir");
    let real = dir.path().join("dump.json");
    std::fs::write(&real, "{}").expect("write");
    let docs = dir.path().join("docs");
    std::fs::create_dir(&docs).expect("docs");
    let name = format!("libc-{REFERENCE_VERSION}-aarch64-apple-darwin.json");
    std::os::unix::fs::symlink(&real, docs.join(&name)).expect("symlink");
    assert_eq!(supplied_doc(Some(&docs), &CELLS[0]), Some(docs.join(name)));
}

#[test]
fn each_cell_writes_its_shim_and_its_oracle_module() {
    let [shim, oracle] = outputs(Path::new("/r"), &CELLS[2]);
    assert_eq!(
        shim,
        Path::new("/r/crates/aterm-libc/src/linux_gnu_x86_64.rs")
    );
    assert_eq!(
        oracle,
        Path::new("/r/libc-oracle/conformance/src/cell_linux.rs")
    );
}

/// A stock command never runs from the checkout and always names its
/// triple; a Trust one runs from the crate and names none.
#[test]
fn a_cross_cell_compiles_from_root_for_its_triple_and_the_host_from_the_crate() {
    let stock = Stock {
        name: "stable".into(),
        cargo: "/t/bin/cargo".into(),
        rustc: "/t/bin/rustc".into(),
        rustdoc: "/t/bin/rustdoc".into(),
        rustlib: "/t/lib/rustlib".into(),
    };
    let command = Compiler::Stock(&stock).cargo(
        &CELLS[2],
        Path::new("/w/reference"),
        Path::new("/w/target"),
        &["check"],
        &[],
    );
    let args: Vec<&OsStr> = command.get_args().collect();
    let manifest = Path::new("/w/reference").join("Cargo.toml");
    assert_eq!(
        args,
        [
            "check",
            "--target",
            "x86_64-unknown-linux-gnu",
            "--manifest-path",
            manifest.to_str().expect("UTF-8"),
            "--offline",
            "--target-dir",
            "/w/target",
        ]
    );
    assert_eq!(command.get_current_dir(), Some(Path::new("/")));
    let trust = Trust {
        formatter: Some("/s/trustfmt".into()),
        targo: "/s/targo".into(),
        trustc: "/s/trustc".into(),
        trustdoc: "/s/trustdoc".into(),
        host: "aarch64-apple-darwin".into(),
    };
    let command = Compiler::Trust(&trust).cargo(
        &CELLS[0],
        Path::new("/w/reference"),
        Path::new("/w/target"),
        &["rustdoc", "-p", "libc"],
        &["--output-format", "json"],
    );
    let args: Vec<&OsStr> = command.get_args().collect();
    assert_eq!(
        args,
        [
            "--unverified",
            "rustdoc",
            "-p",
            "libc",
            "--offline",
            "--target-dir",
            "/w/target",
            "--",
            "--output-format",
            "json",
        ]
    );
    assert_eq!(command.get_current_dir(), Some(Path::new("/w/reference")));
    assert_eq!(trust.trustfmt(), Path::new("/s/trustfmt"));
}
