// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Valid implicit Git Booleans do not weaken private-history custody.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use aterm_uds::holders::PathScan;

use super::super::super::fence::Fence;
use super::super::{Git as _, own_entry, retire_on};
use super::{Gate, GitCli, for_git, gate, git, plan};

fn read(dir: &Path, args: &[&str]) -> String {
    let answer = GitCli.run(dir, args);
    assert_eq!(answer.code, Some(0), "git {args:?}: {}", answer.err);
    answer.out.trim().to_owned()
}

struct Module {
    sub: PathBuf,
    common: PathBuf,
    admin: PathBuf,
    witness: PathBuf,
    witness_common: PathBuf,
    pin: String,
}

fn module(g: &Gate) -> Module {
    let snapshot = g.snapshot();
    let sub = snapshot.join("vendor/astream");
    let common = PathBuf::from(read(
        &sub,
        &["rev-parse", "--path-format=absolute", "--git-common-dir"],
    ));
    let admin = own_entry(
        &g.checkout(),
        &snapshot,
        &std::fs::read(snapshot.join(".git")).unwrap(),
    )
    .unwrap();
    let witness = g.checkout().join("vendor/astream");
    let witness_common = PathBuf::from(read(
        &witness,
        &["rev-parse", "--path-format=absolute", "--git-common-dir"],
    ));
    let pin = read(&sub, &["rev-parse", "HEAD"]);
    assert!(common.starts_with(admin.join("modules")));
    assert_eq!(read(&witness, &["rev-parse", "HEAD"]), pin);
    Module {
        sub,
        common,
        admin,
        witness,
        witness_common,
        pin,
    }
}

fn append_setting(path: &Path, setting: &str) {
    let mut bytes = std::fs::read(path).unwrap();
    bytes.extend_from_slice(b"\n");
    bytes.extend_from_slice(setting.as_bytes());
    std::fs::write(path, bytes).unwrap();
}

/// Snapshot every byte in the small fixture's independent witness, not just its config.
fn witness_files(root: &Path) -> BTreeMap<PathBuf, Vec<u8>> {
    let mut files = BTreeMap::new();
    let mut pending = vec![root.to_path_buf()];
    while let Some(directory) = pending.pop() {
        for entry in std::fs::read_dir(directory).unwrap() {
            let entry = entry.unwrap();
            let path = entry.path();
            let kind = entry.file_type().unwrap();
            if kind.is_dir() {
                pending.push(path);
            } else {
                assert!(kind.is_file(), "fixture witness must have physical files");
                files.insert(
                    path.strip_prefix(root).unwrap().to_path_buf(),
                    std::fs::read(&path).unwrap(),
                );
            }
        }
    }
    files
}

fn retire_with_boolean(canonical: bool) {
    let g = gate();
    let m = module(&g);
    let common = if canonical {
        &m.witness_common
    } else {
        &m.common
    };
    let config = common.join("config");
    append_setting(&config, "[advice]\n\tdetachedHead\n");
    assert_eq!(
        read(
            &g.checkout(),
            &[
                "config",
                "--no-includes",
                "--file",
                &for_git(&config),
                "--bool",
                "--get",
                "advice.detachedHead",
            ],
        ),
        "true",
        "Git itself recognizes the setting as an implicit Boolean",
    );
    let witness = witness_files(&m.witness_common);
    let planned = plan(&g.roots, g.now, &GitCli);
    let clear = |_: &[PathBuf], _: &[u32]| Ok(PathScan::default());
    let done = retire_on(&g.roots, g.now, &GitCli, &Fence::new(&clear));
    assert!(planned.iter().all(|row| row.goes()), "{planned:#?}");
    assert!(done.iter().all(|(_, result)| result.is_ok()), "{done:#?}");
    assert!(!g.snapshot().exists() && !g.cells().exists());
    assert!(!m.admin.exists() && !m.common.exists());
    assert!(!g.has_worktree(&g.snapshot()));
    assert_eq!(witness_files(&m.witness_common), witness);
}

#[test]
fn an_implicit_boolean_in_private_config_allows_retirement() {
    retire_with_boolean(false);
}

#[test]
fn an_implicit_boolean_in_canonical_config_allows_retirement() {
    retire_with_boolean(true);
}

fn kept(g: &Gate, m: &Module) {
    let gitfile = std::fs::read(g.snapshot().join(".git")).unwrap();
    let source = std::fs::read(g.snapshot().join("Cargo.toml")).unwrap();
    let config = std::fs::read(m.common.join("config")).unwrap();
    let refs = read(
        &m.sub,
        &["for-each-ref", "--format=%(refname)%00%(objectname)"],
    );
    let witness = witness_files(&m.witness_common);
    let planned = plan(&g.roots, g.now, &GitCli);
    let clear = |_: &[PathBuf], _: &[u32]| Ok(PathScan::default());
    let done = retire_on(&g.roots, g.now, &GitCli, &Fence::new(&clear));
    assert!(
        planned
            .iter()
            .any(|row| row.path == g.snapshot() && !row.goes()),
        "{planned:#?}",
    );
    assert!(
        done.iter()
            .any(|(row, result)| row.path == g.snapshot() && result.is_err()),
        "{done:#?}",
    );
    assert!(g.cells().is_dir() && m.admin.is_dir() && m.common.is_dir());
    assert!(g.has_worktree(&g.snapshot()));
    assert_eq!(std::fs::read(g.snapshot().join(".git")).unwrap(), gitfile);
    assert_eq!(
        std::fs::read(g.snapshot().join("Cargo.toml")).unwrap(),
        source
    );
    assert_eq!(std::fs::read(m.common.join("config")).unwrap(), config);
    assert_eq!(
        read(
            &m.sub,
            &["for-each-ref", "--format=%(refname)%00%(objectname)"]
        ),
        refs,
    );
    assert_eq!(read(&m.sub, &["rev-parse", "HEAD"]), m.pin);
    assert_eq!(witness_files(&m.witness_common), witness);
}

#[test]
fn an_implicit_boolean_does_not_hide_unique_private_history() {
    let g = gate();
    let m = module(&g);
    git(&m.sub, &["checkout", "-qb", "retained-with-boolean"]);
    let payload = "// unique private history remains recoverable\n";
    std::fs::write(m.sub.join("lib.rs"), payload).unwrap();
    git(&m.sub, &["add", "lib.rs"]);
    git(&m.sub, &["commit", "-qm", "retain private source"]);
    let unique = read(&m.sub, &["rev-parse", "HEAD"]);
    let blob = read(&m.sub, &["rev-parse", "HEAD:lib.rs"]);
    git(&m.sub, &["checkout", "-q", "--detach", &m.pin]);
    for oid in [&unique, &blob] {
        let absent = GitCli.run(&m.witness, &["cat-file", "-e", oid]);
        assert!(absent.code.is_some(), "Git must run: {}", absent.err);
        assert_ne!(absent.code, Some(0), "the witness must not hold {oid}");
    }
    for common in [&m.common, &m.witness_common] {
        append_setting(&common.join("config"), "[advice]\n\tdetachedHead\n");
    }
    assert_eq!(
        read(
            &g.snapshot(),
            &["status", "--porcelain", "--ignore-submodules=none"],
        ),
        "",
    );

    kept(&g, &m);
    assert_eq!(
        read(&m.sub, &["rev-parse", "refs/heads/retained-with-boolean"]),
        unique,
    );
    assert_eq!(
        read(&m.sub, &["show", &format!("{unique}:lib.rs")]),
        payload.trim(),
    );
    assert_eq!(read(&m.sub, &["cat-file", "-t", &blob]), "blob");
}

fn restricted_witness_setting_keeps_source(setting: &str) {
    let g = gate();
    let m = module(&g);
    // The private config stays valid, so the snapshot's own status and refs remain readable.
    append_setting(&m.witness_common.join("config"), setting);
    kept(&g, &m);
}

#[test]
fn a_valueless_include_cannot_authorize_retirement() {
    restricted_witness_setting_keeps_source("[include]\n\tpath\n");
}

#[test]
fn a_valueless_conditional_include_cannot_authorize_retirement() {
    restricted_witness_setting_keeps_source("[includeIf \"gitdir:never-matches\"]\n\tpath\n");
}

#[test]
fn a_valueless_witness_fsck_setting_cannot_authorize_retirement() {
    restricted_witness_setting_keeps_source("[fsck]\n\tmissingEmail\n");
}

#[test]
fn a_valueless_unsupported_extension_cannot_authorize_retirement() {
    restricted_witness_setting_keeps_source("[extensions]\n\tworktreeConfig\n");
}

#[test]
fn a_valueless_object_format_cannot_authorize_retirement() {
    restricted_witness_setting_keeps_source("[extensions]\n\tobjectFormat\n");
}

#[test]
fn a_valueless_ref_format_cannot_authorize_retirement() {
    restricted_witness_setting_keeps_source("[extensions]\n\trefStorage\n");
}

#[test]
fn a_valueless_numeric_format_cannot_authorize_retirement() {
    restricted_witness_setting_keeps_source("[core]\n\trepositoryFormatVersion\n");
}
