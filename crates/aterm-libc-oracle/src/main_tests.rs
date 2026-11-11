// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

use super::*;

fn args(words: &[&str]) -> Result<Args, String> {
    parse(words.iter().map(OsString::from))
}

/// A checkout whose marker files are symlinks to regular files is still the root, as
/// `Path::is_file` follows them; one missing a marker is not.
#[cfg(unix)]
#[test]
fn a_root_whose_markers_are_symlinks_is_found() {
    let dir = aterm_tempfile::tempdir().expect("tempdir");
    let root = dir.path().join("checkout");
    let real = dir.path().join("real");
    std::fs::create_dir_all(&real).expect("real");
    for rel in ["libc-oracle/Cargo.toml", "crates/aterm-libc/src/lib.rs"] {
        let target = real.join(rel.replace('/', "-"));
        std::fs::write(&target, "").expect("write");
        let link = root.join(rel);
        std::fs::create_dir_all(link.parent().expect("parent")).expect("dirs");
        std::os::unix::fs::symlink(&target, &link).expect("symlink");
    }
    let start = root.join("crates/aterm-libc/src");
    assert_eq!(find_root(&start), Some(root.clone()));
    std::fs::remove_file(root.join("libc-oracle/Cargo.toml")).expect("remove");
    assert_eq!(find_root(&start), None);
}

/// Regen's cross cells compile on the checkout's stock pin unless `--stock` names another,
/// never on rustup's floating `stable`.
#[test]
fn regen_rides_the_stock_pin_by_default() {
    let root = aterm_tempfile::manifest_dir().join("../..");
    let stock = |words: &[&str]| {
        regen_options(root.clone(), &args(words).unwrap())
            .map(|options| options.stock)
            .ok()
    };
    assert_eq!(
        stock(&["regen"]).as_deref(),
        Some(aterm_toolchain::rustup::STOCK_PIN)
    );
    assert_eq!(
        stock(&["regen", "--stock", "1.98.1"]).as_deref(),
        Some("1.98.1")
    );
}

#[test]
fn each_verb_takes_only_its_own_flags() {
    assert!(
        args(&["--stock", "stable"]).is_err(),
        "an authoring flag is not the gate's"
    );
    assert_eq!(
        args(&[
            "regen",
            "--out",
            "/o",
            "--docs",
            "d",
            "--stock",
            "1.98.1",
            "--no-format"
        ]),
        Ok(Args {
            verb: Verb::Regen,
            out: Some("/o".into()),
            docs: Some("d".into()),
            stock: Some("1.98.1".into()),
            no_format: true,
            ..Args::default()
        })
    );
    assert!(args(&["regen", "--target-dir", "t"]).is_err());
    assert!(args(&["regen", "log.txt"]).is_err());
    assert_eq!(
        args(&["harvest", "--cell", "linux", "--write", "check.log"]),
        Ok(Args {
            verb: Verb::Harvest,
            cell: Some("linux".into()),
            write: true,
            log: Some("check.log".into()),
            ..Args::default()
        })
    );
    assert!(args(&["harvest", "check.log"]).is_err(), "no cell");
    assert!(args(&["harvest", "--cell", "mac"]).is_err(), "no log");
    assert!(args(&["harvest", "--cell", "mac", "a.log", "b.log"]).is_err());
    assert!(args(&["harvest", "--out", "/o", "--cell", "mac", "a.log"]).is_err());
}
