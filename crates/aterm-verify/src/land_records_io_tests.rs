// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

use super::*;
use crate::checks::Check;
use crate::land_record::Word;
use crate::land_record::tests::{BASE, NOW, record};
use crate::land_spool::Dir;
use std::path::PathBuf;
use std::time::Duration;

fn git(root: &Path, args: &[&str]) -> Vec<u8> {
    let mut command = aterm_uds::fixture_git::git();
    command
        .current_dir(root)
        .args(args)
        .stdin(aterm_uds::spawnfd::Stdio::null());
    let output = aterm_uds::deadline::output_while_process_progressing_complete_with_limit(
        &mut command,
        Duration::from_secs(60),
        "record listing fixture Git",
        1024 * 1024,
    )
    .unwrap();
    assert!(
        output.status.success(),
        "{args:?}: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    output.stdout
}

struct World {
    _scratch: aterm_tempfile::TempDir,
    root: PathBuf,
    origin: PathBuf,
    tree: String,
}

#[derive(Debug, PartialEq, Eq)]
struct Checkout {
    head: Vec<u8>,
    index: Vec<u8>,
    tracked: Vec<u8>,
    untracked: Vec<u8>,
    status: Vec<u8>,
}

impl World {
    fn new() -> Self {
        let scratch = aterm_tempfile::tempdir().unwrap();
        git(scratch.path(), &["init", "--quiet", "work"]);
        git(scratch.path(), &["init", "--quiet", "--bare", "origin"]);
        let root = scratch.path().join("work");
        let origin = scratch.path().join("origin");
        for at in [&root, &origin] {
            git(at, &["config", "maintenance.auto", "false"]);
        }
        git(&root, &["config", "user.name", "Andrew Yates"]);
        git(&root, &["config", "user.email", "fixture@example.invalid"]);
        git(
            &root,
            &["remote", "add", "origin", origin.to_str().unwrap()],
        );
        std::fs::write(root.join("kept"), "base\n").unwrap();
        git(&root, &["add", "kept"]);
        git(&root, &["commit", "--quiet", "-m", "fixture"]);
        let tree = String::from_utf8(git(&root, &["rev-parse", "HEAD^{tree}"]))
            .unwrap()
            .trim()
            .to_owned();
        Self {
            _scratch: scratch,
            root,
            origin,
            tree,
        }
    }

    fn verdict(&self, check: Check) -> Signed {
        record(check, Word::Ok, &self.tree, BASE, NOW)
            .sign(None)
            .unwrap()
    }

    fn publish(&self, signed: &Signed) {
        crate::judged::append_land(
            &self.root,
            REMOTE,
            std::slice::from_ref(signed),
            NOW,
            crate::judged::deadline(),
        )
        .unwrap();
        assert_eq!(
            git(
                &self.origin,
                &["show", &format!("{}:{}", crate::judged::REF, signed.path)],
            ),
            signed.bytes
        );
    }

    fn dirty(&self) {
        std::fs::write(self.root.join("kept"), "staged\n").unwrap();
        git(&self.root, &["add", "kept"]);
        std::fs::write(self.root.join("kept"), "worktree\n").unwrap();
        std::fs::write(self.root.join("untracked"), "leave this too\n").unwrap();
    }

    fn checkout(&self) -> Checkout {
        let status = git(&self.root, &["status", "--porcelain=v1"]);
        Checkout {
            head: git(&self.root, &["rev-parse", "HEAD"]),
            index: std::fs::read(self.root.join(".git/index")).unwrap(),
            tracked: std::fs::read(self.root.join("kept")).unwrap(),
            untracked: std::fs::read(self.root.join("untracked")).unwrap(),
            status,
        }
    }

    fn listing(&self) -> (i32, Vec<String>, Vec<String>) {
        let mut lines = Vec::new();
        let mut warnings = Vec::new();
        let code = list_records(
            &self.root,
            &self.tree,
            None,
            || NOW,
            false,
            &mut |line| lines.push(line.to_owned()),
            &mut |line| warnings.push(line.to_owned()),
        );
        (code, lines, warnings)
    }
}

#[test]
fn an_unreadable_spool_keeps_journal_rows_visible_and_returns_unavailable() {
    let world = World::new();
    world.publish(&world.verdict(Check::Fmt));
    world.dirty();
    let before = world.checkout();
    assert!(!before.status.is_empty(), "the checkout is dirty");
    let spool = Dir::of_clone(&world.root).unwrap();

    let (code, complete, warnings) = world.listing();
    assert_eq!(code, exit::PASS, "{warnings:?}");
    assert!(warnings.is_empty(), "{warnings:?}");
    assert_eq!(complete.len(), 2, "{complete:?}");
    assert!(
        complete[1].starts_with("  fmt ok ") && complete[1].ends_with("(journal)"),
        "{complete:?}"
    );
    assert!(!spool.path().exists(), "listing creates no absent spool");
    assert_eq!(world.checkout(), before);

    std::fs::create_dir_all(spool.path().parent().unwrap()).unwrap();
    std::fs::write(spool.path(), b"not a spool directory\n").unwrap();
    let (code, partial, warnings) = world.listing();
    assert_eq!(code, exit::COULD_NOT_RUN, "{partial:?} {warnings:?}");
    assert_eq!(partial, complete, "the readable journal is still shown");
    assert_eq!(warnings.len(), 1, "{warnings:?}");
    assert!(warnings[0].contains("spool"), "{warnings:?}");
    assert_eq!(world.checkout(), before);
    assert_eq!(
        std::fs::read(spool.path()).unwrap(),
        b"not a spool directory\n"
    );
}

#[test]
fn an_unreadable_journal_keeps_spooled_rows_visible_and_returns_unavailable() {
    let world = World::new();
    world.publish(&world.verdict(Check::Fmt));
    let spool = Dir::of_clone(&world.root).unwrap();
    let held = world.verdict(Check::Lint);
    spool.put(&held).unwrap();
    world.dirty();

    let (code, complete, warnings) = world.listing();
    assert_eq!(code, exit::PASS, "{warnings:?}");
    assert!(warnings.is_empty(), "{warnings:?}");
    assert_eq!(complete.len(), 3, "{complete:?}");
    assert!(
        complete
            .iter()
            .any(|line| line.starts_with("  fmt ok ") && line.ends_with("(journal)")),
        "{complete:?}"
    );
    let local = complete
        .iter()
        .find(|line| line.starts_with("  lint ok ") && line.ends_with("(spooled)"))
        .expect("the local record is shown")
        .clone();
    let blocked = world.root.join("not-a-journal");
    std::fs::write(&blocked, b"not a Git repository\n").unwrap();
    git(
        &world.root,
        &["remote", "set-url", "origin", blocked.to_str().unwrap()],
    );
    let before = world.checkout();
    let (code, partial, warnings) = world.listing();
    assert_eq!(code, exit::COULD_NOT_RUN, "{partial:?} {warnings:?}");
    assert_eq!(partial.len(), 2, "{partial:?}");
    assert_eq!(partial[1], local, "the readable spool is still shown");
    assert_eq!(warnings.len(), 1, "{warnings:?}");
    assert!(warnings[0].contains("journal"), "{warnings:?}");
    assert_eq!(spool.pending().unwrap().records, [held]);
    assert_eq!(world.checkout(), before);
    assert_eq!(std::fs::read(blocked).unwrap(), b"not a Git repository\n");
}
