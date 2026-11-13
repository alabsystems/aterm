// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! An empty reflog message still has a real committer tail and both object endpoints.

use std::path::{Path, PathBuf};

use aterm_uds::holders::PathScan;

use super::super::super::fence::Fence;
use super::super::{Git as _, own_entry, retire_on};
use super::{Gate, GitCli, gate, git, plan};

const REFERENCE: &str = "refs/heads/no-message-control";

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
    assert!(common.starts_with(admin.join("modules")));
    let witness = g.checkout().join("vendor/astream");
    let pin = read(&sub, &["rev-parse", "HEAD"]);
    assert_eq!(read(&witness, &["rev-parse", "HEAD"]), pin);
    Module {
        sub,
        common,
        admin,
        witness,
        pin,
    }
}

fn clean(g: &Gate, m: &Module) {
    assert_eq!(read(&m.sub, &["rev-parse", "HEAD"]), m.pin);
    assert_eq!(
        read(
            &g.snapshot(),
            &["status", "--porcelain", "--ignore-submodules=none"]
        ),
        ""
    );
    assert!(
        read(&m.sub, &["for-each-ref", "--format=%(objectname)"])
            .lines()
            .all(|oid| oid == m.pin)
    );
    assert_eq!(
        read(&m.sub, &["worktree", "list", "--porcelain"])
            .lines()
            .filter(|line| line.starts_with("worktree "))
            .count(),
        1,
        "no foreign registry can supply the retention verdict",
    );
}

fn log(m: &Module) -> PathBuf {
    m.common.join("logs").join(REFERENCE)
}

fn valid_no_message(bytes: &[u8], records: usize) {
    assert!(bytes.ends_with(b"\n") && !bytes.contains(&b'\t'));
    assert_eq!(bytes.iter().filter(|byte| **byte == b'\n').count(), records);
    for record in bytes.split_inclusive(|byte| *byte == b'\n') {
        let row = record.strip_suffix(b"\n").unwrap();
        let mut fields = row.splitn(3, |byte| *byte == b' ');
        for _ in 0..2 {
            let oid = fields.next().unwrap();
            assert!(matches!(oid.len(), 40 | 64) && oid.iter().all(u8::is_ascii_hexdigit));
        }
        assert!(
            !fields.next().unwrap().is_empty(),
            "Git wrote a committer/timestamp tail"
        );
    }
}

fn kept(g: &Gate, m: &Module, expected: &[u8]) {
    let planned = plan(&g.roots, g.now, &GitCli);
    let clear = |_: &[PathBuf], _: &[u32]| Ok(PathScan::default());
    let done = retire_on(&g.roots, g.now, &GitCli, &Fence::new(&clear));
    assert!(
        planned
            .iter()
            .any(|row| row.path == g.snapshot() && !row.goes()),
        "{planned:#?}"
    );
    assert!(
        done.iter()
            .any(|(row, result)| row.path == g.snapshot() && result.is_err()),
        "{done:#?}"
    );
    assert!(
        g.snapshot().join("Cargo.toml").is_file()
            && g.cells().is_dir()
            && m.admin.is_dir()
            && m.common.is_dir(),
        "{done:#?}"
    );
    assert_eq!(std::fs::read(log(m)).unwrap(), expected);
    assert_eq!(read(&m.sub, &["rev-parse", REFERENCE]), m.pin);
    clean(g, m);
}

#[test]
fn a_canonical_private_ref_with_a_git_written_empty_message_reflog_retires() {
    let g = gate();
    let m = module(&g);
    // No -m: Git itself writes a complete reflog record without a message separator TAB.
    git(
        &m.sub,
        &["update-ref", "--create-reflog", REFERENCE, &m.pin],
    );
    valid_no_message(&std::fs::read(log(&m)).unwrap(), 1);
    clean(&g, &m);

    let planned = plan(&g.roots, g.now, &GitCli);
    let clear = |_: &[PathBuf], _: &[u32]| Ok(PathScan::default());
    let done = retire_on(&g.roots, g.now, &GitCli, &Fence::new(&clear));
    assert!(
        done.iter().all(|(_, result)| result.is_ok()),
        "plan={planned:#?}; retire={done:#?}"
    );
    assert!(planned.iter().all(|row| row.goes()), "{planned:#?}");
    assert!(
        !g.snapshot().exists() && !g.cells().exists() && !m.admin.exists() && !m.common.exists()
    );
    assert!(!g.has_worktree(&g.snapshot()));
}

#[test]
fn an_empty_message_reflog_old_endpoint_preserves_otherwise_unreferenced_private_source() {
    let g = gate();
    let m = module(&g);
    let payload = "// source held only by old and new reflog endpoints\n";
    std::fs::write(m.sub.join("lib.rs"), payload).unwrap();
    git(&m.sub, &["add", "lib.rs"]);
    let tree = read(&m.sub, &["write-tree"]);
    let blob = read(&m.sub, &["rev-parse", ":lib.rs"]);
    // No parent or ref update: this orphan commit has no path from the published pin.
    let unique = read(
        &m.sub,
        &[
            "-c",
            "user.name=t",
            "-c",
            "user.email=t@example.invalid",
            "-c",
            "commit.gpgsign=false",
            "commit-tree",
            &tree,
            "-m",
            "private reflog source",
        ],
    );
    git(
        &m.sub,
        &[
            "restore",
            "--source=HEAD",
            "--staged",
            "--worktree",
            "--",
            "lib.rs",
        ],
    );
    git(
        &m.sub,
        &[
            "-c",
            "core.logAllRefUpdates=false",
            "update-ref",
            REFERENCE,
            &unique,
        ],
    );
    assert!(
        !log(&m).exists(),
        "the orphan must not also enter a new-OID reflog field"
    );
    git(
        &m.sub,
        &["update-ref", "--create-reflog", REFERENCE, &m.pin, &unique],
    );
    let expected = std::fs::read(log(&m)).unwrap();
    valid_no_message(&expected, 1);
    let mut fields = expected.splitn(3, |byte| *byte == b' ');
    assert_eq!(fields.next().unwrap(), unique.as_bytes());
    assert_eq!(fields.next().unwrap(), m.pin.as_bytes());
    clean(&g, &m);
    assert!(
        !read(&m.sub, &["rev-list", "--all"])
            .lines()
            .any(|oid| oid == unique)
    );
    assert!(
        read(&m.sub, &["rev-list", "--all", "--reflog"])
            .lines()
            .any(|oid| oid == unique)
    );
    for oid in [&unique, &blob] {
        let absent = GitCli.run(&m.witness, &["cat-file", "-e", oid]);
        assert!(absent.code.is_some(), "Git must run: {}", absent.err);
        assert_ne!(
            absent.code,
            Some(0),
            "the canonical store must not hold {oid}"
        );
    }

    kept(&g, &m, &expected);
    let saved = GitCli.run(&m.sub, &["show", &format!("{unique}:lib.rs")]);
    assert_eq!(saved.code, Some(0), "{}", saved.err);
    assert_eq!(saved.out, payload);
    assert_eq!(read(&m.sub, &["cat-file", "-t", &blob]), "blob");
}

#[test]
fn two_valid_reflog_oids_with_an_empty_committer_tail_keep_the_snapshot() {
    let g = gate();
    let m = module(&g);
    git(
        &m.sub,
        &["update-ref", "--create-reflog", REFERENCE, &m.pin],
    );
    let expected = format!("{} {} \n", m.pin, m.pin).into_bytes();
    std::fs::write(log(&m), &expected).unwrap();
    clean(&g, &m);

    kept(&g, &m, &expected);
}
