// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! The owner identity on a draft journal and the launch sweep
//! (ATERM-KI-1015-12): a process's lease, the owner set on each image, and a
//! sweep that takes only a clean journal nobody holds.

use super::super::{DraftJournalHost, encode_journal};
use super::*;
use crate::document_store::DocumentSnapshot;
use crate::native_document_io::JournalRecord;

/// A private draft directory for one test, removed when it drops.
struct Root {
    path: PathBuf,
    _scratch: aterm_tempfile::TempDir,
}

impl Root {
    fn new(name: &str) -> Self {
        let scratch = aterm_tempfile::Builder::new()
            .prefix(&format!("aterm-journal-owner-{name}-"))
            .tempdir()
            .expect("private journal test directory");
        Self {
            path: scratch.path().to_path_buf(),
            _scratch: scratch,
        }
    }

    /// A store over this directory: one owner, as a running aterm is.
    fn host(&self) -> DraftJournalHost {
        DraftJournalHost::new(self.path.clone()).expect("journal host")
    }
}

/// `text` as opened from disk, under its own document id.
fn snapshot(text: &str) -> (crate::document_store::DocumentStore, DocumentSnapshot) {
    let mut store = crate::test_document_store();
    let id = store.open(format!("file:///tmp/{}.md", text.len()), text.to_string());
    let snapshot = store.snapshot(id).expect("snapshot");
    (store, snapshot)
}

/// Open `uri` (`disk` on disk) in `host` and leave it clean: the image is the
/// file's snapshot alone. Returns the journal's path.
fn open_clean(host: &DraftJournalHost, uri: &str, disk: &str) -> PathBuf {
    let (_store, disk) = snapshot(disk);
    let decision = host
        .inspect_open(uri, disk.text.as_bytes())
        .expect("inspect");
    host.initialize(decision, &disk, &disk)
        .expect("initialize")
        .path
}

/// Open `uri` in `host` with an unsaved draft over `disk`.
fn open_with_draft(host: &DraftJournalHost, uri: &str, disk: &str, draft: &str) -> PathBuf {
    let (mut store, disk_snapshot) = snapshot(disk);
    let decision = host
        .inspect_open(uri, disk_snapshot.text.as_bytes())
        .expect("inspect");
    let outcome = store.transact(
        disk_snapshot.id,
        disk_snapshot.seq,
        vec![crate::document_store::TextEdit {
            range: 0..disk.len(),
            insert: draft.to_string(),
        }],
    );
    assert!(matches!(
        outcome,
        crate::document_store::DocumentTxnOutcome::Committed { .. }
    ));
    let current = store.snapshot(disk_snapshot.id).expect("draft");
    host.initialize(decision, &disk_snapshot, &current)
        .expect("initialize")
        .path
}

fn mark(path: &Path) -> OwnerMark {
    read_mark(path, &sys::open_regular(path).expect("image"))
}

/// The owners the image at `path` names on its own bytes.
fn named(path: &Path) -> Option<BTreeSet<JournalOwnerId>> {
    let bytes = fs::read(path).expect("image");
    mark(path).owners_of(ContentFingerprint::of(&bytes))
}

/// The sweep's look at the keyed journal at `path`, in a draft root.
fn candidate(path: &Path) -> Option<(Candidate, ContentFingerprint)> {
    let name = path.file_name().unwrap().to_str().unwrap();
    let key = keyed_image_key(name).expect("a keyed journal");
    sweep_candidate(path, key, &path.parent().unwrap().join(OWNERS_DIR))
}

fn exists(path: &Path) -> bool {
    fs::symlink_metadata(path).is_ok()
}

/// The reproduction and the rule together. Before the owner identity, every
/// document ever opened left its journal in `drafts/` for good: here five
/// opens leave five images. The sweep takes exactly the one that is clean and
/// held by nobody, with its lock file, and keeps every other kind: a clean
/// journal a live process holds (closed or not, it is that process's until it
/// exits), an unsaved draft whose owner is gone, an image an older aterm wrote
/// (no owner set: its holders cannot be named), a preserved copy and a legacy
/// journal.
#[test]
fn a_launch_sweep_takes_only_a_clean_journal_nobody_holds() {
    let root = Root::new("sweep-rule");
    let gone = root.host();
    let clean_unheld = open_clean(&gone, "file:///tmp/a.md", "alpha");
    let draft_unheld = open_with_draft(&gone, "file:///tmp/b.md", "beta", "beta, edited");
    let live = root.host();
    let clean_held = open_clean(&live, "file:///tmp/c.md", "gamma");

    // An older aterm's clean image: the same record, written without a mark.
    let older = live.path_for(live.key_for("file:///tmp/d.md"));
    let (_store, disk) = snapshot("delta");
    let key = live.key_for("file:///tmp/d.md");
    fs::write(
        &older,
        encode_journal(&[JournalRecord::snapshot_for(key, &disk)]).unwrap(),
    )
    .unwrap();
    // A clean image whose mark names a gone owner on OTHER bytes (what a
    // Windows replace leaves when it carries a replaced file's stream): it
    // names nobody, so it stays.
    let stale = open_clean(&live, "file:///tmp/e.md", "epsilon");
    let stale_bytes = fs::read(&stale).unwrap();
    write_mark(
        &stale,
        &sys::open_regular(&stale).unwrap(),
        ContentFingerprint(ContentFingerprint::of(&stale_bytes).0 ^ 1),
        &BTreeSet::from([gone.owner_id().unwrap()]),
    )
    .unwrap();
    let preserved = root.path.join("0123456789abcdef.mac.preserved-1.atdj");
    fs::copy(&clean_unheld, &preserved).unwrap();
    let legacy = root.path.join("0123456789abcdef.atdj");
    fs::copy(&clean_unheld, &legacy).unwrap();
    let images = || {
        fs::read_dir(&root.path)
            .unwrap()
            .flatten()
            .filter(|entry| entry.file_name().to_string_lossy().ends_with(".mac.atdj"))
            .count()
    };
    assert_eq!(images(), 5, "every open left its journal");

    // The first host's process exits.
    drop(gone);
    let sweep = sweep_unheld_journals(&root.path);
    assert_eq!(sweep.removed, 1, "{sweep:?}");
    assert_eq!(sweep.kept, 4, "{sweep:?}");
    assert!(
        !exists(&clean_unheld),
        "the clean journal nobody holds goes"
    );
    assert!(
        !exists(&journal_lock_path(&clean_unheld).unwrap()),
        "with its lock file"
    );
    assert_eq!(
        sweep.leftovers, 0,
        "that lock file went with its image, never taken and dropped again as a leftover"
    );
    for kept in [
        &draft_unheld,
        &clean_held,
        &older,
        &stale,
        &preserved,
        &legacy,
    ] {
        assert!(exists(kept), "{} is kept", kept.display());
    }
    assert_eq!(sweep.dead_owners, 1, "the gone owner's lease goes");

    // A second sweep finds nothing more while the live host holds its journal.
    assert_eq!(sweep_unheld_journals(&root.path).removed, 0);
    // Once it exits too, its clean journal goes; the draft never does.
    drop(live);
    assert_eq!(sweep_unheld_journals(&root.path).removed, 1);
    assert!(!exists(&clean_held));
    assert!(exists(&draft_unheld) && exists(&older) && exists(&stale));
}

/// A journal stays its owner's while the owner lives, however long ago the
/// document closed: a Suspended document reopened in that process appends to
/// it without initializing it again. The store's own appends and checkpoints
/// carry its owner, not only the open's first image.
#[test]
fn a_live_stores_writes_name_it_and_are_never_swept() {
    let root = Root::new("live-store");
    let mut store = super::super::DocumentJournalStore::for_test(root.path.clone()).unwrap();
    let owner = store.host.owner_id().expect("an owner identity");
    let (mut documents, disk) = snapshot("base");
    let uri = "file:///tmp/live-store.md";
    let decision = store.inspect_open(uri, disk.text.as_bytes()).unwrap();
    let initialized = store.initialize(decision, &disk, &disk).unwrap();
    assert_eq!(named(&initialized.path), Some(BTreeSet::from([owner])));
    let outcome = documents.transact(
        disk.id,
        disk.seq,
        vec![crate::document_store::TextEdit {
            range: 4..4,
            insert: "!".to_string(),
        }],
    );
    assert!(matches!(
        outcome,
        crate::document_store::DocumentTxnOutcome::Committed { .. }
    ));
    let edited = documents.snapshot(disk.id).unwrap();
    store.observe_commit(&edited).unwrap();
    let Some(super::super::JournalEffect::Append { path, key, plan }) =
        store.next_effect(disk.id).unwrap()
    else {
        panic!("an append is planned");
    };
    assert_eq!(plan.owner, Some(owner), "the plan carries the owner");
    let result =
        super::super::execute_journal_append(&path, key, &plan, JournalLockPatience::Worker);
    assert!(matches!(
        store.complete_append(disk.id, plan.generation, result),
        super::super::JournalCompletion::Durable { .. }
    ));
    assert_eq!(named(&path), Some(BTreeSet::from([owner])));
    // Saved back to the file: clean again, and still held.
    store
        .request_checkpoint(
            crate::native_document_io::DurableCheckpoint {
                document: disk.id,
                seq: edited.seq,
                source: crate::native_document_io::DurableSource::AtomicFile,
            },
            edited.text.clone(),
        )
        .unwrap();
    let Some(super::super::JournalEffect::Rewrite { path, plan }) =
        store.next_effect(disk.id).unwrap()
    else {
        panic!("a checkpoint is planned");
    };
    let result = super::super::execute_journal_rewrite(&path, &plan, JournalLockPatience::Worker);
    assert!(matches!(
        store.complete_rewrite(disk.id, plan.generation, result),
        super::super::JournalCompletion::Durable { .. }
    ));
    assert_eq!(named(&path), Some(BTreeSet::from([owner])));
    assert_eq!(
        sweep_unheld_journals(&root.path).removed,
        0,
        "its owner lives"
    );
    drop(store);
    assert_eq!(
        sweep_unheld_journals(&root.path).removed,
        1,
        "its owner is gone"
    );
}

/// A process appends to an image while its bytes are the ones it last made
/// durable, so one that republishes those same bytes carries the claim of
/// whoever held them. Two live opens of one clean document: the second names
/// both, and the image stays until both are gone.
#[test]
fn an_identical_republish_keeps_the_live_holders_claim() {
    let root = Root::new("identical-marked");
    let first = root.host();
    let second = root.host();
    let uri = "file:///tmp/shared.md";
    let path = open_clean(&first, uri, "shared");
    assert_eq!(
        open_clean(&second, uri, "shared"),
        path,
        "one journal for one document"
    );
    let both = BTreeSet::from([first.owner_id().unwrap(), second.owner_id().unwrap()]);
    assert_eq!(named(&path), Some(both));
    drop(second);
    assert_eq!(
        sweep_unheld_journals(&root.path).removed,
        0,
        "the first holder still appends to these bytes"
    );
    drop(first);
    assert_eq!(sweep_unheld_journals(&root.path).removed, 1);
}

/// An older aterm names nobody on what it writes. When this one republishes
/// those very bytes, that older process may still be appending to them, so the
/// republished image stays unmarked: never swept.
#[test]
fn an_identical_republish_over_an_older_aterms_image_stays_unmarked() {
    let root = Root::new("identical-unmarked");
    let host = root.host();
    let uri = "file:///tmp/older.md";
    let path = host.path_for(host.key_for(uri));
    let (_store, disk) = snapshot("older");
    fs::write(
        &path,
        encode_journal(&[JournalRecord::snapshot_for(host.key_for(uri), &disk)]).unwrap(),
    )
    .unwrap();
    assert_eq!(open_clean(&host, uri, "older"), path);
    assert_eq!(mark(&path), OwnerMark::Unmarked);
    drop(host);
    assert_eq!(sweep_unheld_journals(&root.path).removed, 0);
    assert!(exists(&path));
}

/// The sweep may take a clean journal nobody holds between an open's inspection
/// and its initialization. The open decided the same with or without it, so it
/// publishes over the absence instead of refusing as a changed journal does.
#[test]
fn a_clean_journal_swept_after_the_inspection_still_opens() {
    let root = Root::new("swept-mid-open");
    let uri = "file:///tmp/mid-open.md";
    let gone = root.host();
    let path = open_clean(&gone, uri, "text");
    drop(gone);
    let host = root.host();
    let (_store, disk) = snapshot("text");
    let decision = host.inspect_open(uri, disk.text.as_bytes()).unwrap();
    assert_eq!(sweep_unheld_journals(&root.path).removed, 1);
    assert!(!exists(&path));
    let initialized = host
        .initialize(decision, &disk, &disk)
        .expect("the open publishes over the swept journal");
    assert!(exists(&initialized.path));

    // A journal that CHANGED since the inspection still refuses, as before.
    let decision = host.inspect_open(uri, disk.text.as_bytes()).unwrap();
    let (_other, draft) = snapshot("other text");
    fs::write(
        &path,
        encode_journal(&[
            JournalRecord::snapshot_for(decision.key, &disk),
            JournalRecord::snapshot_for(decision.key, &draft),
        ])
        .unwrap(),
    )
    .unwrap();
    let refused = host.initialize(decision, &disk, &disk).unwrap_err();
    assert!(
        refused.contains("changed after recovery inspection"),
        "{refused}"
    );
}

/// A re-seat that finds its own bytes still in place, under an owner set that
/// no longer names it (another process wrote those same bytes since), joins
/// that set: the image then names every live process that can write it.
#[test]
fn a_reseat_that_finds_its_own_image_joins_its_owner_set() {
    let root = Root::new("reseat-adopts");
    let mut store = super::super::DocumentJournalStore::for_test(root.path.clone()).unwrap();
    let owner = store.host.owner_id().unwrap();
    let (_documents, disk) = snapshot("seat");
    let uri = "file:///tmp/reseat.md";
    let decision = store.inspect_open(uri, disk.text.as_bytes()).unwrap();
    let path = store.initialize(decision, &disk, &disk).unwrap().path;
    // Another owner, gone now, wrote these same bytes over it.
    let other = root.host();
    let other_id = other.owner_id().unwrap();
    drop(other);
    write_mark(
        &path,
        &sys::open_regular(&path).unwrap(),
        ContentFingerprint::of(&fs::read(&path).unwrap()),
        &BTreeSet::from([other_id]),
    )
    .unwrap();
    assert_eq!(store.owe_reseat_all(), vec![disk.id]);
    let Some(super::super::JournalEffect::Reseat { path: seat, plan }) =
        store.next_effect(disk.id).unwrap()
    else {
        panic!("a re-seat is planned");
    };
    assert_eq!(
        super::super::execute_journal_reseat(&seat, &plan, JournalLockPatience::Worker),
        super::super::JournalReseatResult::Unchanged
    );
    assert_eq!(named(&path), Some(BTreeSet::from([owner])));
    assert_eq!(sweep_unheld_journals(&root.path).removed, 0);
    drop(store);
    assert_eq!(sweep_unheld_journals(&root.path).removed, 1);
}

/// What a writer that is gone leaves: a publication's temporary and a lock
/// file whose journal is gone go; a live journal's lock file and anything not
/// named like a journal's stay.
#[test]
fn a_sweep_clears_the_leftovers_of_writers_that_are_gone() {
    let root = Root::new("leftovers");
    let host = root.host();
    let path = open_clean(&host, "file:///tmp/live.md", "live");
    let name = path.file_name().unwrap().to_str().unwrap().to_string();
    let temporary = root.path.join(format!(".{name}.4242-1.tmp"));
    fs::write(&temporary, b"torn").unwrap();
    let orphan_lock = root.path.join(".fedcba9876543210.mac.atdj.lock");
    fs::write(&orphan_lock, b"").unwrap();
    let legacy_lock = root.path.join(".fedcba9876543210.atdj.lock");
    fs::write(&legacy_lock, b"").unwrap();
    // A temporary whose journal is gone: taking that journal's lock to remove
    // it makes a lock file, which goes with it.
    let gone_temporary = root.path.join(".0123456789abcdef.mac.atdj.777-2.tmp");
    fs::write(&gone_temporary, b"torn").unwrap();
    let gone_lock = root.path.join(".0123456789abcdef.mac.atdj.lock");
    let secret = root.path.join(".journal-key");
    let unrelated = root.path.join(".notes.lock");
    fs::write(&unrelated, b"").unwrap();

    let sweep = sweep_unheld_journals(&root.path);
    assert_eq!(sweep.leftovers, 4, "{sweep:?}");
    assert!(!exists(&temporary) && !exists(&orphan_lock) && !exists(&legacy_lock));
    assert!(!exists(&gone_temporary) && !exists(&gone_lock));
    assert!(
        exists(&journal_lock_path(&path).unwrap()),
        "a live journal's lock stays"
    );
    assert!(exists(&secret) && exists(&unrelated));
    assert!(exists(&path));
}

/// An owner's lease is acquirable exactly when it is gone: a live host's never
/// is, and the sweep removes only the leases of owners that are gone.
#[test]
fn only_a_gone_owners_lease_is_removed() {
    let root = Root::new("leases");
    let live = root.host();
    let gone = root.host();
    let owners = root.path.join(OWNERS_DIR);
    let live_id = live.owner_id().unwrap();
    let gone_id = gone.owner_id().unwrap();
    assert!(owner_alive(&owners, live_id) && owner_alive(&owners, gone_id));
    drop(gone);
    assert!(!owner_alive(&owners, gone_id));
    assert_eq!(sweep_unheld_journals(&root.path).dead_owners, 1);
    assert!(!exists(&owners.join(gone_id.lease_name())));
    assert!(exists(&owners.join(live_id.lease_name())));
    assert!(owner_alive(&owners, live_id));
}

/// The sweep reads a journal without its lock, and session restore reopens
/// exactly the documents whose clean journals it is reading, at the same
/// moment. A live process that republished the journal since the look (a new
/// file, named with it) keeps it: the decision under the lock is made on the
/// file the look read, which is no longer the one the name names.
#[test]
fn a_journal_republished_after_the_sweeps_look_is_kept() {
    let root = Root::new("republished-mid-sweep");
    let uri = "file:///tmp/republished.md";
    let gone = root.host();
    let path = open_clean(&gone, uri, "restored");
    drop(gone);
    let (look, image) = candidate(&path).expect("clean, and nobody holds it");
    let live = root.host();
    assert_eq!(open_clean(&live, uri, "restored"), path);
    let owners = root.path.join(OWNERS_DIR);
    assert!(
        !look.settle(&owners, image, true, None),
        "the journal a live process just published is not the one the look read"
    );
    assert!(exists(&path));
    assert_eq!(
        named(&path),
        Some(BTreeSet::from([live.owner_id().unwrap()]))
    );
}

/// A live process that joined the journal's owner set in place since the
/// sweep's look (a re-seat that found its own bytes, `adopt`) keeps it: the
/// owner set is read again under the lock.
#[test]
fn an_owner_named_after_the_sweeps_look_keeps_the_journal() {
    let root = Root::new("named-mid-sweep");
    let gone = root.host();
    let path = open_clean(&gone, "file:///tmp/adopted.md", "adopted");
    drop(gone);
    let (look, image) = candidate(&path).expect("clean, and nobody holds it");
    let live = root.host();
    adopt(&path, image, live.owner_id());
    assert_eq!(
        named(&path),
        Some(BTreeSet::from([live.owner_id().unwrap()]))
    );
    assert!(!look.settle(&root.path.join(OWNERS_DIR), image, true, None));
    assert!(exists(&path));
}

/// The sweep removes a journal's lock file while it holds it. A writer that
/// opened that lock file just before, and waited, then holds a lock nobody
/// else will take — the next taker makes the name afresh — so it proves the
/// name still names what it locked, and opens it again when not: two writers
/// are never in at once.
#[test]
fn a_lock_removed_while_a_writer_waits_on_it_is_never_taken_as_held() {
    let root = Root::new("lock-removed");
    let gone = root.host();
    let path = open_clean(&gone, "file:///tmp/waiter.md", "waiter");
    drop(gone);
    let lock = journal_lock_path(&path).unwrap();
    let swept = root.path.clone();
    super::super::between_lock_open_and_take_for_test(move || {
        assert_eq!(sweep_unheld_journals(&swept).removed, 1);
    });
    let inside = with_journal_lock(&path, JournalLockPatience::Worker, || {
        // The lock this writer holds is the one its name names, so a second
        // taker is refused while it is in.
        Ok((
            exists(&lock),
            with_journal_lock(&path, JournalLockPatience::Sweep, || Ok(())).is_err(),
        ))
    });
    assert_eq!(inside, Ok((true, true)));
    assert!(!exists(&path), "the sweep took the journal");
}

/// A process that lets a document go leaves its journal's owner set: an image
/// another live process still names stays, now naming only that one; the last
/// holder's release takes a clean image with its lock file; an image holding a
/// draft never goes.
#[test]
fn a_release_takes_a_clean_journal_once_no_live_owner_is_left_on_it() {
    let root = Root::new("release");
    let uri = "file:///tmp/released.md";
    let (_documents, disk) = snapshot("released");
    let open = |store: &mut super::super::DocumentJournalStore| {
        let decision = store.inspect_open(uri, disk.text.as_bytes()).unwrap();
        store.initialize(decision, &disk, &disk).unwrap().path
    };
    let mut first = super::super::DocumentJournalStore::for_test(root.path.clone()).unwrap();
    let mut second = super::super::DocumentJournalStore::for_test(root.path.clone()).unwrap();
    let path = open(&mut first);
    assert_eq!(open(&mut second), path);
    let second_id = second.host.owner_id().unwrap();

    first.forget(&[disk.id]);
    assert!(exists(&path), "the second process still holds it");
    assert_eq!(named(&path), Some(BTreeSet::from([second_id])));
    second.forget(&[disk.id]);
    assert!(!exists(&path), "nobody holds it now");
    assert!(!exists(&journal_lock_path(&path).unwrap()));

    // An image holding a draft never goes, whoever lets it go.
    let mut drafting = super::super::DocumentJournalStore::for_test(root.path.clone()).unwrap();
    let (mut documents, base) = snapshot("base");
    let decision = drafting
        .inspect_open("file:///tmp/draft.md", base.text.as_bytes())
        .unwrap();
    let outcome = documents.transact(
        base.id,
        base.seq,
        vec![crate::document_store::TextEdit {
            range: 0..4,
            insert: "base, edited".to_string(),
        }],
    );
    assert!(matches!(
        outcome,
        crate::document_store::DocumentTxnOutcome::Committed { .. }
    ));
    let edited = documents.snapshot(base.id).unwrap();
    let draft = drafting.initialize(decision, &base, &edited).unwrap().path;
    let drafting_id = drafting.host.owner_id().unwrap();
    drafting.forget(&[base.id]);
    assert!(exists(&draft), "a draft is never taken");
    assert_eq!(named(&draft), Some(BTreeSet::from([drafting_id])));
}

/// A released journal is settled off the event loop, against the file it
/// opened there. When this process opens the same file again first, its open
/// republishes the journal as a new file it holds, which the settle leaves
/// alone.
#[test]
fn a_release_leaves_a_journal_reopened_since_alone() {
    let root = Root::new("release-reopened");
    let uri = "file:///tmp/reopened.md";
    let host = root.host();
    let path = open_clean(&host, uri, "reopened");
    let owner = host.owner_id().unwrap();
    let let_go = released(path.clone(), host.key_for(uri)).expect("the image");
    assert_eq!(open_clean(&host, uri, "reopened"), path);
    settle_releases(owner, vec![let_go]);
    assert!(exists(&path), "the reopened document's journal stays");
    assert_eq!(named(&path), Some(BTreeSet::from([owner])));
}
