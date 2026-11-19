// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! This Mac's format-2 log pass (tracker FL-148F): a Mac with no mark does nothing and reads
//! nothing past its mark, a crash at every step of a pass that prunes or sheds recovers, and
//! two seated Macs share, prune and shed through their vault server and end with the same
//! membership.

use super::*;
use crate::client::{
    MemberEntry, MemberWritten, OnBusy, load_records, member_set, save_records, sync,
    two_log_macs_for_test,
};
use crate::crossing::{FORMAT_TWO, Faults, inject};
use crate::fsx;
use crate::membership_log::Record;
use crate::membership_log::checkpoint::DAY_MS;
use crate::membership_log::stored::KEPT_MAGIC;
use crate::membership_log::{PassingVault, RecordBudgets, Store, encode_records, passing_vault};
use crate::records_storage::Step;
use crate::records_storage::rewrite::{self, CUT, Cut, DIES, Dies, Piece};
use std::collections::BTreeMap;
use std::path::Path;

/// From now on `paths`'s build reads format 2 and its clock reads `now`.
fn reads_two_at(paths: &Paths, now: u64) {
    inject(
        super::super::format_mark(paths).path(),
        Faults {
            follows: true,
            now_ms: Some(now),
            ..Faults::default()
        },
    );
}

/// Every file under `dir`, by its path from `dir`, with its bytes.
fn tree(dir: &Path) -> BTreeMap<String, Vec<u8>> {
    let mut out = BTreeMap::new();
    let mut stack = vec![dir.to_path_buf()];
    while let Some(at) = stack.pop() {
        for entry in std::fs::read_dir(&at).unwrap() {
            let path = entry.unwrap().path();
            if path.is_dir() {
                stack.push(path);
            } else {
                let name = path.strip_prefix(dir).unwrap().display().to_string();
                out.insert(name, std::fs::read(&path).unwrap());
            }
        }
    }
    out
}

/// A Mac at `root/name` that holds `records` and reads them at format 2: its node follows and
/// its clock reads the vault's pass time; its mark names format 2.
fn marked(root: &Path, name: &str, world: &PassingVault, records: &[Record]) -> Paths {
    let paths = Paths::for_home(&root.join(name));
    fsx::ensure_private_dir(&paths.dir).unwrap();
    reads_two_at(&paths, world.now_ms);
    super::super::format_mark(&paths)
        .store(FORMAT_TWO)
        .expect("the mark");
    save_records(&paths, &encode_records(records)).expect("the records");
    assert!(load_records(&paths).unwrap().reading().admits_two());
    paths
}

/// What `store` holds, whatever order and file it was read from.
fn held(store: &Store) -> (Option<[u8; 32]>, Option<Vec<u8>>, Vec<Vec<u8>>) {
    let mut records: Vec<Vec<u8>> = store
        .log()
        .map(|log| {
            log.records()
                .map(|record| record.wire_bytes().unwrap().to_vec())
                .collect()
        })
        .unwrap_or_default();
    records.sort();
    (store.heads_digest(), store.kept_frame().unwrap(), records)
}

/// A pass on a Mac with no format mark does nothing and reads nothing new: on a Mac whose
/// build reads no format 2 (every running build) it answers before it reads a file, so a
/// records file that does not read changes nothing; on one whose build reads format 2 but
/// whose mark names none it reads the mark alone. Every file of the Mac is byte for byte what
/// it was, no journal record is kept, and status and the survey say nothing. NEGATIVE
/// CONTROL (observed): with `log_pass::run`'s gate cut, the first pass takes this Mac's state
/// and author locks, and `assert_eq!(tree(&paths.dir), before)` fails on `.lock` and
/// `.records.author.lock`.
#[test]
fn vault_checkpoint_a_pass_on_an_unmarked_mac_does_nothing_and_reads_nothing_new() {
    let root = aterm_tempfile::tempdir().unwrap();
    let world = passing_vault();
    let paths = Paths::for_home(&root.path().join("dark"));
    fsx::ensure_private_dir(&paths.dir).unwrap();
    fsx::write_atomic(
        &paths.records_file(),
        &encode_records(&world.records),
        0o600,
    )
    .unwrap();
    let before = tree(&paths.dir);
    assert!(matches!(super::run(&paths), Ok(None)));
    assert_eq!(tree(&paths.dir), before);
    // A file no reader reads: the pass never opens it.
    fsx::write_atomic(&paths.records_file(), b"not records", 0o600).unwrap();
    let before = tree(&paths.dir);
    assert!(matches!(super::run(&paths), Ok(None)), "the build's gate");
    reads_two_at(&paths, world.now_ms);
    assert!(matches!(super::run(&paths), Ok(None)), "the mark's gate");
    inject(super::super::format_mark(&paths).path(), Faults::default());
    assert_eq!(tree(&paths.dir), before);
    assert!(!paths.dir.join(super::LOG_PASS_FILE).exists());
    assert_eq!(super::line(&paths, |_| String::new()), None);
    assert_eq!(super::row(&paths), None);
}

/// A crash in a pass, as one of its rewrites dies.
#[derive(Clone, Copy, Debug)]
enum Crash {
    /// Once the step is done.
    After(Step),
    /// At the cut ([`CUT`]).
    At(Cut),
}

/// Every crash a rewrite can suffer: after each step, at each scratch file torn, flushed whole
/// and not renamed, and half lost, with the pin's flush failed, and after each unlink.
fn crashes() -> Vec<Crash> {
    let mut crashes: Vec<Crash> = [
        Step::Intent,
        Step::Copied,
        Step::Prepared,
        Step::Pinned,
        Step::Exchanged,
    ]
    .into_iter()
    .map(Crash::After)
    .collect();
    for piece in [Piece::Intent, Piece::Data, Piece::Header, Piece::Pin] {
        crashes.extend([
            Crash::At(Cut::Torn(piece)),
            Crash::At(Cut::Flushed(piece)),
            Crash::At(Cut::Lost(piece)),
        ]);
    }
    crashes.push(Crash::At(Cut::Unflushed));
    crashes.extend((1..=8).map(|unlinks| Crash::At(Cut::Unlinked(unlinks))));
    crashes
}

/// A crash at every step of a pass that prunes, and of one that sheds alone, recovers: the
/// pass's rewrite dies, and the Mac's next pass recovers the entry under its state lock, reads
/// either what it held or what the dead pass wrote, makes the same prune or shed again, and
/// holds exactly the store the pass makes in memory, its file of a rewritten generation that
/// begins with its frame where it pruned, with no rewrite intent or scratch file left.
/// NEGATIVE CONTROL (observed): with `log_pass::run` taking no state lock (its
/// `StateLock::take` cut), nothing recovers the entry and the test fails with
/// "prunes At(Unlinked(6)): no rewrite left".
#[test]
fn vault_checkpoint_a_crash_at_every_step_of_a_pass_that_prunes_recovers() {
    let world = passing_vault();
    for (what, records, expected, prunes) in [
        ("prunes", &world.records, &world.passed, true),
        ("sheds", &world.unshared, &world.shed_alone, false),
    ] {
        for crash in crashes() {
            let root = aterm_tempfile::tempdir().unwrap();
            let paths = marked(root.path(), "mac", &world, records);
            match crash {
                Crash::After(step) => DIES.set(Some(Dies::After(step))),
                Crash::At(cut) => {
                    DIES.set(Some(Dies::AtCut));
                    CUT.set(Some(cut));
                }
            }
            let first = super::run(&paths);
            DIES.set(None);
            CUT.set(None);
            let again = super::run(&paths)
                .unwrap_or_else(|error| panic!("{what} {crash:?}: the next pass: {error}"));
            assert!(
                first.is_err() || again.is_some_and(|done| !done.did()),
                "{what} {crash:?}: a pass that did not die left nothing to do"
            );
            let store = load_records(&paths).unwrap();
            assert_eq!(held(&store), held(expected), "{what} {crash:?}");
            let left: Vec<String> = tree(&paths.dir)
                .into_keys()
                .filter(|name| name.ends_with(".rewriting") || name.ends_with(".building"))
                .collect();
            assert_eq!(
                left,
                Vec::<String>::new(),
                "{what} {crash:?}: no rewrite left"
            );
            let custody = rewrite::data_path(&paths.records_file()).unwrap();
            assert!(custody.generation >= 1, "{what} {crash:?}");
            assert_eq!(
                std::fs::read(&custody.path)
                    .unwrap()
                    .starts_with(KEPT_MAGIC),
                prunes,
                "{what} {crash:?}"
            );
            // A pass that died keeps no record; the next keeps one of what it did.
            if let Some(done) = again.filter(super::Done::did) {
                assert_eq!(super::last(&paths), Some(done), "{what} {crash:?}");
            }
            inject(super::super::format_mark(&paths).path(), Faults::default());
        }
    }
}

/// A plain `set` of `name` to `value`.
fn entry(name: &str, value: &[u8]) -> MemberEntry {
    MemberEntry {
        name: name.to_owned(),
        value: crate::SecretBytes::from(value.to_vec()),
        if_absent: false,
        anchor: crate::client::Anchor::Keep,
        fleet_reads_anchors: false,
        read_as: None,
        settles: None,
    }
}

fn set(paths: &Paths, name: &str, value: &[u8]) {
    let written = member_set(
        paths,
        &entry(name, value),
        OnBusy::Refuse(std::time::Instant::now() + aterm_spec::wait::MUST_HAPPEN),
    )
    .unwrap();
    assert!(
        matches!(written, MemberWritten::Posted | MemberWritten::SignedHere),
        "{written:?}"
    );
}

/// Two seated Macs at format 2 run their passes through their vault server: m7, whose log is
/// past its automatic stop, shares the checkpoint it proposes (signed version 2, asked as a
/// membership write) and sheds its own replaced `Put`; m22 hears the share, shares the same
/// body and prunes, since both seats' shares are more than half of the two; the server takes
/// m22's share and prunes from the frames; m7 hears it and prunes. Both end with the same
/// records, frame and heads, read the same value, and each keeps a journal record that status
/// and the survey read. NEGATIVE CONTROL (observed): with `Log::share_draft` waiting for room
/// alone (its `|| self.records().any(shares)` cut), m22, whose log is not past its stop, never
/// joins m7's body, and the test fails with "m22 joins m7's body", `left: None`; with the
/// server's prune cut from `server::apply_records` (its `adopt_final` answering `None`), it
/// fails with "the server prunes too".
#[test]
fn vault_checkpoint_two_seated_macs_share_prune_and_shed_through_their_server() {
    let macs = two_log_macs_for_test(&[]);
    let (m22, m7) = (&macs.m22, &macs.m7);
    let server_mark = crate::crossing::FormatMark::at(
        macs.root
            .path()
            .join("s1")
            .join(format!("{}.format", crate::ALAB_VAULT)),
    );
    inject(
        server_mark.path(),
        Faults {
            follows: true,
            ..Faults::default()
        },
    );
    let base = aterm_time::unix_millis().unwrap();
    for mac in [m7, m22] {
        reads_two_at(mac, base);
        super::super::format_mark(mac).store(FORMAT_TWO).unwrap();
    }
    // Bodies worth the shed's rewrite on either Mac (`Log::shed_worth`).
    for value in [b'1', b'2', b'3'] {
        set(m7, "k", &[value; 4000]);
    }
    sync(m22, None).expect("m22 syncs");
    let later = base + 16 * DAY_MS;
    reads_two_at(m7, later);
    reads_two_at(m22, later);
    let total = load_records(m7).unwrap().log().unwrap().total_bytes();
    // m7's log alone is past its automatic stop: m22 shares only on m7's share.
    let done = RecordBudgets::within_bytes(total + total / 8, || super::run(m7))
        .unwrap()
        .expect("a marked Mac's pass");
    let shared = done.shared.expect("m7 shares");
    assert!(done.shed > 0 && done.pruned.is_none(), "{done:?}");
    let line = super::line(m7, |_| "then".to_owned()).unwrap();
    assert!(line.starts_with("log pass: shared checkpoint "), "{line}");
    assert!(super::row(m7).unwrap().starts_with("vault_log at="));
    sync(m7, None).expect("m7 posts its share");
    sync(m22, None).expect("m22 takes it");
    let done = super::last(m22).expect("m22's pass");
    assert_eq!(done.shared, Some(shared), "m22 joins m7's body");
    done.pruned.expect("m22 prunes");
    sync(m22, None).expect("m22 posts its share");
    sync(m7, None).expect("m7 takes it");
    assert!(super::last(m7).unwrap().pruned.is_some(), "m7 prunes");
    let (a, b) = (load_records(m7).unwrap(), load_records(m22).unwrap());
    assert_eq!(held(&a), held(&b));
    assert!(a.kept_frame().unwrap().is_some());
    let server = rewrite::data_path(
        &macs
            .root
            .path()
            .join("s1")
            .join(format!("{}.records", crate::ALAB_VAULT)),
    )
    .unwrap();
    assert!(server.generation >= 1, "the server prunes too");
    assert_eq!(
        crate::client::stored(m7, "k").as_deref(),
        Some([b'3'; 4000].as_slice())
    );
    assert_eq!(
        crate::client::stored(m22, "k").as_deref(),
        Some([b'3'; 4000].as_slice())
    );
    for mac in [m7, m22] {
        inject(super::super::format_mark(mac).path(), Faults::default());
    }
    inject(server_mark.path(), Faults::default());
}

/// The journal record speaks only on a Mac that runs the pass: one whose build reads no format
/// 2, or whose mark names none, says nothing in `aterm secrets status` or the survey of a record
/// it holds (left by a build it was downgraded from, or copied in with the vault folder), and
/// reads nothing past its mark; once its build reads format 2 and its mark names it, the same
/// record is read. NEGATIVE CONTROL (observed): with the record read whatever the Mac reads
/// (`Faults::log_pass_ungated`), the unmarked Mac's status names the pass and
/// `assert_eq!(super::line(..), None)` fails.
#[test]
fn vault_checkpoint_an_unmarked_mac_says_nothing_of_a_log_pass_record_it_holds() {
    let root = aterm_tempfile::tempdir().unwrap();
    let paths = Paths::for_home(&root.path().join("dark"));
    fsx::ensure_private_dir(&paths.dir).unwrap();
    let done = super::Done {
        at_ms: 1_000,
        shed: 2,
        shared: None,
        pruned: None,
    };
    fsx::write_atomic(
        &paths.dir.join(super::LOG_PASS_FILE),
        done.encode().as_bytes(),
        0o600,
    )
    .unwrap();
    assert_eq!(super::line(&paths, |_| String::new()), None, "no format 2");
    assert_eq!(super::row(&paths), None);
    reads_two_at(&paths, 2_000);
    assert_eq!(super::last(&paths), None, "no mark");
    super::super::format_mark(&paths).store(FORMAT_TWO).unwrap();
    assert_eq!(super::last(&paths), Some(done), "a marked Mac");
    inject(super::super::format_mark(&paths).path(), Faults::default());
}
