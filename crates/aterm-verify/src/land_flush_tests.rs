// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

use super::*;
use crate::checks::Check;
use crate::land_record::Word;
use crate::land_record::tests::{BASE, NOW, T, T2, record, signer};
use crate::land_spool::{Dir, Pending};
use std::cell::{Cell, RefCell};
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
        "record flush fixture Git",
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

struct Journal {
    _scratch: aterm_tempfile::TempDir,
    root: PathBuf,
    origin: PathBuf,
    spool: Dir,
}

impl Journal {
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
        let spool = Dir::of_clone(&root).unwrap();
        Self {
            _scratch: scratch,
            root,
            origin,
            spool,
        }
    }

    fn append(&self, root: &Path, records: &[Signed], now: u64) -> Result<(), String> {
        assert_eq!(Dir::of_clone(root).unwrap().path(), self.spool.path());
        crate::judged::append_land(root, "origin", records, now, crate::judged::deadline())
            .map(drop)
    }
}

fn verdict(check: Check, tree: &str, host: &str, run: &str) -> Signed {
    let mut made = record(check, Word::Ok, tree, BASE, NOW);
    made.local.host = host.to_owned();
    made.run.id = run.to_owned();
    made.sign(Some(&signer(7))).unwrap()
}

fn end() -> Signed {
    let mut made = crate::land_run::tests::record(crate::land_run::Kind::End, 0);
    made.run.host = "another-host".to_owned();
    made.sign(Some(&signer(9))).unwrap()
}

struct Tracked<'a> {
    dir: Dir,
    listings: Cell<usize>,
    cleaned: RefCell<Vec<Signed>>,
    listing_error: bool,
    cleanup_error: Option<String>,
    stop_after_listing: Option<&'a Cell<bool>>,
    stop_during_cleanup: Option<&'a Cell<bool>>,
}

impl Tracked<'_> {
    fn new(dir: Dir) -> Self {
        Self {
            dir,
            listings: Cell::new(0),
            cleaned: RefCell::default(),
            listing_error: false,
            cleanup_error: None,
            stop_after_listing: None,
            stop_during_cleanup: None,
        }
    }
}

impl Spool for Tracked<'_> {
    fn put(&self, record: &Signed) -> Result<(), String> {
        self.dir.put(record)
    }

    fn pending(&self) -> Result<Pending, String> {
        self.listings.set(self.listings.get() + 1);
        if self.listing_error {
            return Err("controlled listing refusal".to_owned());
        }
        let pending = self.dir.pending();
        if let Some(stopped) = self.stop_after_listing {
            stopped.set(true);
        }
        pending
    }

    fn done(&self, record: &Signed) -> Result<(), String> {
        self.cleaned.borrow_mut().push(record.clone());
        if let Some(stopped) = self.stop_during_cleanup {
            stopped.set(true);
        }
        if self.cleanup_error.as_deref() == Some(record.path.as_str()) {
            Err("controlled cleanup refusal".to_owned())
        } else {
            self.dir.done(record)
        }
    }
}

#[test]
fn a_flush_publishes_existing_signed_bytes_without_recording_a_new_run() {
    let journal = Journal::new();
    let records = [
        verdict(Check::Fmt, T, "one-host", "first"),
        verdict(Check::Lint, T2, "second-host", "second"),
        end(),
    ];
    for signed in &records {
        journal.spool.put(signed).unwrap();
    }
    std::fs::write(journal.root.join("kept"), "staged\n").unwrap();
    git(&journal.root, &["add", "kept"]);
    std::fs::write(journal.root.join("kept"), "worktree\n").unwrap();
    let head = git(&journal.root, &["rev-parse", "HEAD"]);
    let index = std::fs::read(journal.root.join(".git/index")).unwrap();
    let pending = journal.spool.pending().unwrap().records;
    let spool = Tracked::new(journal.spool.clone());
    let calls = Cell::new(0);
    let result = flush(
        &journal.root,
        &spool,
        &mut |root, batch, now| {
            calls.set(calls.get() + 1);
            assert_eq!(batch, pending);
            journal.append(root, batch, now)
        },
        NOW,
        &|| false,
    )
    .unwrap();
    assert!(result.complete(), "{result:?}");
    assert_eq!(result.appended, records.len());
    assert_eq!(calls.get(), 1);
    assert_eq!(spool.listings.get(), 1);
    assert_eq!(*spool.cleaned.borrow(), pending);
    assert!(journal.spool.pending().unwrap().records.is_empty());
    for signed in &records {
        let stored = git(
            &journal.origin,
            &["show", &format!("{}:{}", crate::judged::REF, signed.path)],
        );
        assert_eq!(stored, signed.bytes);
    }
    let mut paths: Vec<_> = records.iter().map(|signed| signed.path.clone()).collect();
    paths.sort();
    let listing = git(
        &journal.origin,
        &[
            "ls-tree",
            "-r",
            "--name-only",
            crate::judged::REF,
            "--",
            "land",
        ],
    );
    assert_eq!(
        String::from_utf8(listing)
            .unwrap()
            .lines()
            .collect::<Vec<_>>(),
        paths
    );
    assert_eq!(git(&journal.root, &["rev-parse", "HEAD"]), head);
    assert_eq!(
        std::fs::read(journal.root.join(".git/index")).unwrap(),
        index
    );
    assert_eq!(
        std::fs::read(journal.root.join("kept")).unwrap(),
        b"worktree\n"
    );
    let empty = flush(
        &journal.root,
        &spool,
        &mut |_, _, _| panic!("the acknowledged snapshot is already drained"),
        NOW,
        &|| false,
    )
    .unwrap();
    assert_eq!(empty, Flushed::default());
    assert!(empty.complete());
    assert_eq!(spool.listings.get(), 2);
    assert_eq!(*spool.cleaned.borrow(), pending);
}

fn checkout(root: &Path) -> (Vec<u8>, Vec<u8>, Vec<u8>) {
    let index = String::from_utf8(git(
        root,
        &["rev-parse", "--path-format=absolute", "--git-path", "index"],
    ))
    .unwrap();
    (
        git(root, &["rev-parse", "HEAD"]),
        std::fs::read(index.trim()).unwrap(),
        std::fs::read(root.join("kept")).unwrap(),
    )
}

#[test]
fn a_linked_worktree_flushes_its_common_spool_and_leaves_another_clone_untouched() {
    let journal = Journal::new();
    let unrelated = Journal::new();
    let linked = journal._scratch.path().join("linked");
    git(
        &journal.root,
        &[
            "worktree",
            "add",
            "--quiet",
            "--detach",
            linked.to_str().unwrap(),
            "HEAD",
        ],
    );
    let shared = Dir::of_clone(&linked).unwrap();
    assert_eq!(shared.path(), journal.spool.path());
    journal.spool.put(&end()).unwrap();
    unrelated
        .spool
        .put(&verdict(Check::Fmt, T2, "other", "unrelated"))
        .unwrap();
    let before = (
        checkout(&journal.root),
        checkout(&linked),
        checkout(&unrelated.root),
        unrelated.spool.pending().unwrap(),
        git(&unrelated.origin, &["for-each-ref"]),
    );
    let result = flush(
        &linked,
        &shared,
        &mut |root, records, now| journal.append(root, records, now),
        NOW,
        &|| false,
    )
    .unwrap();
    assert!(result.complete());
    assert_eq!(result.appended, 1);
    assert!(journal.spool.pending().unwrap().records.is_empty());
    assert_eq!(
        (
            checkout(&journal.root),
            checkout(&linked),
            checkout(&unrelated.root),
            unrelated.spool.pending().unwrap(),
            git(&unrelated.origin, &["for-each-ref"]),
        ),
        before
    );
}

#[test]
fn an_absent_or_empty_spool_needs_no_append_or_new_record() {
    for exists in [false, true] {
        let scratch = aterm_tempfile::tempdir().unwrap();
        let dir = Dir::at(scratch.path().join("spool"));
        if exists {
            std::fs::create_dir(dir.path()).unwrap();
        }
        let spool = Tracked::new(dir.clone());
        let result = flush(
            scratch.path(),
            &spool,
            &mut |_, _, _| panic!("no remote work"),
            NOW,
            &|| false,
        )
        .unwrap();
        assert_eq!(result, Flushed::default());
        assert!(result.complete());
        assert_eq!(spool.listings.get(), 1);
        assert!(spool.cleaned.borrow().is_empty());
        assert_eq!(dir.path().exists(), exists);
        assert!(dir.pending().unwrap().records.is_empty());
    }
}

#[test]
fn a_spool_listing_failure_is_not_an_empty_success() {
    let scratch = aterm_tempfile::tempdir().unwrap();
    let mut spool = Tracked::new(Dir::at(scratch.path().join("spool")));
    spool.listing_error = true;
    assert!(
        flush(
            scratch.path(),
            &spool,
            &mut |_, _, _| panic!("no append"),
            NOW,
            &|| false,
        )
        .is_err()
    );
    assert_eq!(spool.listings.get(), 1);
    assert!(spool.cleaned.borrow().is_empty());
}

#[test]
fn unreadable_files_and_both_invalid_record_kinds_remain_incomplete() {
    let scratch = aterm_tempfile::tempdir().unwrap();
    let dir = Dir::at(scratch.path().join("spool"));
    let bad_verdict = Signed {
        bytes: b"not a verdict".to_vec(),
        ..verdict(Check::Fmt, T, "host", "run")
    };
    let bad_run = Signed {
        bytes: b"not a run".to_vec(),
        ..end()
    };
    dir.put(&bad_verdict).unwrap();
    dir.put(&bad_run).unwrap();
    let oversized = verdict(Check::Test, T, "host", "large");
    let file = dir
        .path()
        .join(crate::land_spool::file_of(&oversized.path).unwrap());
    std::fs::write(
        &file,
        vec![b' '; usize::try_from(crate::land_record::MAX_BYTES).unwrap() + 1],
    )
    .unwrap();
    let before = dir.pending().unwrap();
    let spool = Tracked::new(dir.clone());
    let result = flush(
        scratch.path(),
        &spool,
        &mut |_, _, _| panic!("no valid records"),
        NOW,
        &|| false,
    )
    .unwrap();
    assert_eq!(result.appended, 0);
    assert_eq!(result.invalid.len(), 3);
    assert!(!result.complete());
    assert!(spool.cleaned.borrow().is_empty());
    assert_eq!(dir.pending().unwrap(), before);
    assert_eq!(spool.listings.get(), 1);
}

#[test]
fn valid_toml_with_tampered_signatures_is_not_published() {
    for signed in [verdict(Check::Fmt, T, "signed-host", "run"), end()] {
        let scratch = aterm_tempfile::tempdir().unwrap();
        let dir = Dir::at(scratch.path().join("spool"));
        assert!(super::super::unreadable(&signed).is_none());
        let text = String::from_utf8(signed.bytes.clone()).unwrap();
        let tampered = text
            .replace("signed-host", "tampered-host")
            .replace("another-host", "tampered-host");
        assert_ne!(text, tampered);
        aterm_toml::from_str::<aterm_toml::Value>(&tampered).unwrap();
        let tampered = Signed {
            bytes: tampered.into_bytes(),
            ..signed
        };
        dir.put(&tampered).unwrap();
        let result = flush(
            scratch.path(),
            &dir,
            &mut |_, _, _| panic!("invalid signatures cannot be published"),
            NOW,
            &|| false,
        )
        .unwrap();
        assert_eq!(result.appended, 0);
        assert_eq!(result.invalid.len(), 1);
        assert!(!result.complete());
        assert_eq!(dir.pending().unwrap().records, [tampered]);
    }
}

#[test]
fn a_mixed_snapshot_publishes_only_valid_records_and_reports_the_remainder() {
    let journal = Journal::new();
    let valid = verdict(Check::Fmt, T, "host", "run");
    let bad = Signed {
        bytes: b"not a run".to_vec(),
        ..end()
    };
    journal.spool.put(&valid).unwrap();
    journal.spool.put(&bad).unwrap();
    let spool = Tracked::new(journal.spool.clone());
    let result = flush(
        &journal.root,
        &spool,
        &mut |root, batch, now| {
            assert_eq!(batch, std::slice::from_ref(&valid));
            journal.append(root, batch, now)
        },
        NOW,
        &|| false,
    )
    .unwrap();
    assert_eq!(result.appended, 1);
    assert_eq!(result.invalid.len(), 1);
    assert!(result.cleanup.is_empty());
    assert!(!result.complete());
    assert_eq!(journal.spool.pending().unwrap().records, [bad]);
    assert_eq!(
        spool.cleaned.borrow().as_slice(),
        std::slice::from_ref(&valid)
    );
}

#[test]
fn cleanup_refusal_is_reported_after_acknowledgment_without_skipping_other_records() {
    let scratch = aterm_tempfile::tempdir().unwrap();
    let dir = Dir::at(scratch.path().join("spool"));
    for signed in [verdict(Check::Fmt, T, "host", "run"), end()] {
        dir.put(&signed).unwrap();
    }
    let before = dir.pending().unwrap().records;
    let mut spool = Tracked::new(dir.clone());
    spool.cleanup_error = Some(before[0].path.clone());
    let result = flush(scratch.path(), &spool, &mut |_, _, _| Ok(()), NOW, &|| {
        false
    })
    .unwrap();
    assert_eq!(result.appended, before.len());
    assert_eq!(result.cleanup.len(), 1);
    assert!(!result.complete());
    assert_eq!(*spool.cleaned.borrow(), before);
    assert_eq!(dir.pending().unwrap().records, [before[0].clone()]);
}

#[test]
fn a_refused_or_unknown_append_keeps_every_byte_and_never_retries() {
    for why in ["expected ref refused", "unknown outcome: reconcile intent"] {
        let scratch = aterm_tempfile::tempdir().unwrap();
        let dir = Dir::at(scratch.path().join("spool"));
        for signed in [verdict(Check::Fmt, T, "host", "run"), end()] {
            dir.put(&signed).unwrap();
        }
        let before = dir.pending().unwrap();
        let spool = Tracked::new(dir.clone());
        let calls = Cell::new(0);
        let result = flush(
            scratch.path(),
            &spool,
            &mut |_, _, _| {
                calls.set(calls.get() + 1);
                Err(why.to_owned())
            },
            NOW,
            &|| false,
        );
        assert_eq!(result, Err(why.to_owned()));
        assert_eq!(calls.get(), 1);
        assert!(spool.cleaned.borrow().is_empty());
        assert_eq!(dir.pending().unwrap(), before);
    }
}

#[test]
fn concurrent_replacement_and_new_arrival_are_left_for_a_later_snapshot() {
    let scratch = aterm_tempfile::tempdir().unwrap();
    let dir = Dir::at(scratch.path().join("spool"));
    let original = verdict(Check::Fmt, T, "host", "first");
    let newer = verdict(Check::Fmt, T, "host", "second");
    let arrival = verdict(Check::Lint, T2, "other-host", "new-arrival");
    assert_eq!(original.path, newer.path);
    assert_ne!(original.bytes, newer.bytes);
    dir.put(&original).unwrap();
    let spool = Tracked::new(dir.clone());
    let result = flush(
        scratch.path(),
        &spool,
        &mut |_, batch, _| {
            assert_eq!(batch, std::slice::from_ref(&original));
            dir.put(&newer)?;
            dir.put(&arrival)
        },
        NOW,
        &|| false,
    )
    .unwrap();
    assert!(result.complete());
    assert_eq!(result.appended, 1);
    assert_eq!(spool.listings.get(), 1);
    let remaining = dir.pending().unwrap().records;
    assert_eq!(remaining.len(), 2);
    assert!(remaining.contains(&newer));
    assert!(remaining.contains(&arrival));
}

#[test]
fn cancellation_before_publication_does_not_append_or_cleanup() {
    for initially in [true, false] {
        let scratch = aterm_tempfile::tempdir().unwrap();
        let dir = Dir::at(scratch.path().join("spool"));
        dir.put(&verdict(Check::Fmt, T, "host", "run")).unwrap();
        let before = dir.pending().unwrap();
        let stopped = Cell::new(initially);
        let mut spool = Tracked::new(dir.clone());
        if !initially {
            spool.stop_after_listing = Some(&stopped);
        }
        assert!(
            flush(
                scratch.path(),
                &spool,
                &mut |_, _, _| panic!("cancelled before append"),
                NOW,
                &|| stopped.get(),
            )
            .is_err()
        );
        assert_eq!(spool.listings.get(), usize::from(!initially));
        assert!(spool.cleaned.borrow().is_empty());
        assert_eq!(dir.pending().unwrap(), before);
    }
}

#[test]
fn cancellation_after_acknowledgment_keeps_the_ack_and_finishes_all_cleanup() {
    for during_cleanup in [false, true] {
        let scratch = aterm_tempfile::tempdir().unwrap();
        let dir = Dir::at(scratch.path().join("spool"));
        for signed in [verdict(Check::Fmt, T, "host", "run"), end()] {
            dir.put(&signed).unwrap();
        }
        let before = dir.pending().unwrap().records;
        let stopped = Cell::new(false);
        let mut spool = Tracked::new(dir.clone());
        if during_cleanup {
            spool.stop_during_cleanup = Some(&stopped);
        }
        let result = flush(
            scratch.path(),
            &spool,
            &mut |_, _, _| {
                if !during_cleanup {
                    stopped.set(true);
                }
                Ok(())
            },
            NOW,
            &|| stopped.get(),
        )
        .unwrap();
        assert_eq!(result.appended, before.len());
        assert!(result.interrupted);
        assert!(!result.complete());
        assert_eq!(*spool.cleaned.borrow(), before);
        assert!(dir.pending().unwrap().records.is_empty());
    }
}
