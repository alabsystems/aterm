// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Tests for draft journal owner identity and the launch sweep.

use super::super::DraftJournalHost;
use super::*;
use crate::document_store::DocumentSnapshot;
use crate::native_document_io::JournalRecord;
use crate::native_document_io::encode_journal;

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

    fn host(&self) -> DraftJournalHost {
        DraftJournalHost::new(self.path.clone()).expect("journal host")
    }
}

fn snapshot(text: &str) -> (crate::document_store::DocumentStore, DocumentSnapshot) {
    let mut store = crate::test_document_store();
    let id = store.open(format!("file:///tmp/{}.md", text.len()), text.to_string());
    let snapshot = store.snapshot(id).expect("snapshot");
    (store, snapshot)
}

fn open_clean(host: &DraftJournalHost, uri: &str, disk: &str) -> PathBuf {
    let (_store, disk) = snapshot(disk);
    let decision = host
        .inspect_open(uri, disk.text.as_bytes())
        .expect("inspect");
    // Sweep fixtures deliberately have no per-open registration: only the owner lease
    // and on-disk membership protect them, as after an interrupted acquisition.
    host.initialize_owned(decision, &disk, &disk, None)
        .expect("initialize")
        .path
}

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
    host.initialize_owned(decision, &disk_snapshot, &current, None)
        .expect("initialize")
        .path
}

fn mark(path: &Path) -> OwnerMark {
    read_mark(path, &sys::open_regular(path).expect("image"))
}

fn named(path: &Path) -> Option<BTreeSet<JournalOwnerId>> {
    let bytes = fs::read(path).expect("image");
    mark(path).owners_of(ContentFingerprint::of(&bytes))
}

fn candidate(path: &Path) -> Option<(Candidate, ContentFingerprint)> {
    let name = path.file_name().unwrap().to_str().unwrap();
    let key = keyed_image_key(name).expect("a keyed journal");
    sweep_candidate(path, key, &path.parent().unwrap().join(OWNERS_DIR))
}

fn exists(path: &Path) -> bool {
    fs::symlink_metadata(path).is_ok()
}

#[test]
fn a_clean_prefix_does_not_authorize_sweeping_an_uninterpreted_tail() {
    let root = Root::new("torn-prefix");
    let gone = root.host();
    let path = open_clean(&gone, "file:///tmp/torn-prefix.md", "baseline");
    let mut bytes = fs::read(&path).unwrap();
    bytes.push(bytes[8]);
    let (read, tail, _) =
        recover_journal_image_for(gone.key_for("file:///tmp/torn-prefix.md"), &bytes).unwrap();
    assert!(holds_no_draft(&read), "the interpreted prefix is clean");
    assert!(
        tail.unwrap().byte_len() < bytes.len() as u64,
        "the whole image is not"
    );
    fs::write(&path, &bytes).unwrap();
    // Give the candidate a matching mark: otherwise the fingerprint check alone keeps it,
    // and the decoder's incomplete-image check would not be exercised.
    write_mark(
        &path,
        &sys::open_regular(&path).unwrap(),
        ContentFingerprint::of(&bytes),
        &BTreeSet::from([gone.owner_id().unwrap()]),
    )
    .unwrap();
    finish_host(gone);
    let sweep = sweep_unheld_journals(&root.path);
    assert_eq!(sweep.removed, 0, "{sweep:?}");
    assert_eq!(fs::read(&path).unwrap(), bytes);
}

#[test]
fn a_launch_sweep_takes_only_a_clean_journal_nobody_holds() {
    let root = Root::new("sweep-rule");
    let gone = root.host();
    let clean_unheld = open_clean(&gone, "file:///tmp/a.md", "alpha");
    let draft_unheld = open_with_draft(&gone, "file:///tmp/b.md", "beta", "beta, edited");
    let live = root.host();
    let clean_held = open_clean(&live, "file:///tmp/c.md", "gamma");

    let older = live.path_for(live.key_for("file:///tmp/d.md"));
    let (_store, disk) = snapshot("delta");
    let key = live.key_for("file:///tmp/d.md");
    fs::write(
        &older,
        encode_journal(&[JournalRecord::snapshot_for(key, &disk)]).unwrap(),
    )
    .unwrap();
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

    finish_host(gone);
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

    assert_eq!(sweep_unheld_journals(&root.path).removed, 0);
    finish_host(live);
    assert_eq!(sweep_unheld_journals(&root.path).removed, 1);
    assert!(!exists(&clean_held));
    assert!(exists(&draft_unheld) && exists(&older) && exists(&stale));
}

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
    drop(plan);
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
    drop(plan);
    let done = store.host.owner.as_ref().unwrap().finished.clone();
    drop(store);
    wait_finished(done);
    assert!(
        !exists(&path),
        "the final registration retired the clean journal"
    );
}

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
    finish_host(second);
    assert_eq!(
        sweep_unheld_journals(&root.path).removed,
        0,
        "the first holder still appends to these bytes"
    );
    finish_host(first);
    assert_eq!(sweep_unheld_journals(&root.path).removed, 1);
}

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
    finish_host(host);
    assert_eq!(sweep_unheld_journals(&root.path).removed, 0);
    assert!(exists(&path));
}

#[test]
fn a_clean_journal_swept_after_the_inspection_still_opens() {
    let root = Root::new("swept-mid-open");
    let uri = "file:///tmp/mid-open.md";
    let gone = root.host();
    let path = open_clean(&gone, uri, "text");
    finish_host(gone);
    let host = root.host();
    let (_store, disk) = snapshot("text");
    let decision = host.inspect_open(uri, disk.text.as_bytes()).unwrap();
    assert_eq!(sweep_unheld_journals(&root.path).removed, 1);
    assert!(!exists(&path));
    let initialized = host
        .initialize(decision, &disk, &disk)
        .expect("the open publishes over the swept journal");
    assert!(exists(&initialized.path));

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

#[test]
fn a_reseat_that_finds_its_own_image_joins_its_owner_set() {
    let root = Root::new("reseat-adopts");
    let mut store = super::super::DocumentJournalStore::for_test(root.path.clone()).unwrap();
    let owner = store.host.owner_id().unwrap();
    let (_documents, disk) = snapshot("seat");
    let uri = "file:///tmp/reseat.md";
    let decision = store.inspect_open(uri, disk.text.as_bytes()).unwrap();
    let path = store.initialize(decision, &disk, &disk).unwrap().path;
    let other = root.host();
    let other_id = other.owner_id().unwrap();
    finish_host(other);
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
    drop(plan);
    let done = store.host.owner.as_ref().unwrap().finished.clone();
    drop(store);
    wait_finished(done);
    assert!(!exists(&path));
}

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

#[test]
fn only_a_gone_owners_lease_is_removed() {
    let root = Root::new("leases");
    let live = root.host();
    let gone = root.host();
    let owners = root.path.join(OWNERS_DIR);
    let live_id = live.owner_id().unwrap();
    let gone_id = gone.owner_id().unwrap();
    assert!(owner_alive(&owners, live_id) && owner_alive(&owners, gone_id));
    finish_host(gone);
    assert!(!owner_alive(&owners, gone_id));
    assert_eq!(sweep_unheld_journals(&root.path).dead_owners, 1);
    assert!(!exists(&owners.join(gone_id.lease_name())));
    assert!(exists(&owners.join(live_id.lease_name())));
    assert!(owner_alive(&owners, live_id));
}

#[test]
fn a_journal_republished_after_the_sweeps_look_is_kept() {
    let root = Root::new("republished-mid-sweep");
    let uri = "file:///tmp/republished.md";
    let gone = root.host();
    let path = open_clean(&gone, uri, "restored");
    finish_host(gone);
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

#[test]
fn an_owner_named_after_the_sweeps_look_keeps_the_journal() {
    let root = Root::new("named-mid-sweep");
    let gone = root.host();
    let path = open_clean(&gone, "file:///tmp/adopted.md", "adopted");
    finish_host(gone);
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

#[test]
fn a_lock_removed_while_a_writer_waits_on_it_is_never_taken_as_held() {
    let root = Root::new("lock-removed");
    let gone = root.host();
    let path = open_clean(&gone, "file:///tmp/waiter.md", "waiter");
    finish_host(gone);
    let lock = journal_lock_path(&path).unwrap();
    let swept = root.path.clone();
    super::super::between_lock_open_and_take_for_test(move || {
        assert_eq!(sweep_unheld_journals(&swept).removed, 1);
    });
    let inside = with_journal_lock(&path, JournalLockPatience::Worker, || {
        // The lock this writer holds is the one its name names, so a second taker is refused while
        // it is in.
        Ok((
            exists(&lock),
            with_journal_lock(&path, JournalLockPatience::Sweep, || Ok(())).is_err(),
        ))
    });
    assert_eq!(inside, Ok((true, true)));
    assert!(!exists(&path), "the sweep took the journal");
}

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
    fence(&first.host);
    assert!(exists(&path), "the second process still holds it");
    assert_eq!(
        named(&path),
        Some(BTreeSet::from([first.host.owner_id().unwrap(), second_id]))
    );
    // Its pinned inode was replaced by the second process. It cannot retarget that
    // process's identical image, so its old mark conservatively lives until lease exit.
    let done = first.host.owner.as_ref().unwrap().finished.clone();
    drop(first);
    wait_finished(done);
    second.forget(&[disk.id]);
    fence(&second.host);
    assert!(!exists(&path), "nobody holds it now");
    assert!(!exists(&journal_lock_path(&path).unwrap()));

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
    fence(&drafting.host);
    assert!(exists(&draft), "a draft is never taken");
    assert_eq!(named(&draft), Some(BTreeSet::from([drafting_id])));
}

#[test]
fn a_release_leaves_a_journal_reopened_since_alone() {
    let root = Root::new("release-reopened");
    let host = root.host();
    let uri = "file:///tmp/reopened.md";
    let (_documents, disk) = snapshot("reopened");
    let prepare = || {
        let decision = host.inspect_open(uri, disk.text.as_bytes()).unwrap();
        host.initialize(decision, &disk, &disk).unwrap()
    };
    let old = prepare();
    let reopened = prepare();
    let path = reopened.path.clone();
    let gate = Gate::hold(&host);
    drop(old); // queued after the identical reopen acquired its own registration
    gate.release();
    fence(&host);
    assert!(exists(&path));
    assert_eq!(
        named(&path),
        Some(BTreeSet::from([host.owner_id().unwrap()]))
    );
    drop(reopened);
    fence(&host);
    assert!(!exists(&path));
}

const HANG: std::time::Duration = std::time::Duration::from_secs(60);

fn fence(host: &DraftJournalHost) {
    host.owner.as_ref().unwrap().run(|_| ()).unwrap();
}

fn wait_finished(done: Arc<std::sync::Mutex<mpsc::Receiver<()>>>) {
    done.lock()
        .unwrap()
        .recv_timeout(HANG)
        .expect("custody worker finished");
}

fn finish_host(host: DraftJournalHost) {
    let done = host.owner.as_ref().unwrap().finished.clone();
    drop(host);
    wait_finished(done);
}

struct Gate(mpsc::Sender<()>);
impl Gate {
    fn hold(host: &DraftJournalHost) -> Self {
        let (entered, ready) = mpsc::channel();
        let (resume, wait) = mpsc::channel();
        host.owner
            .as_ref()
            .unwrap()
            .sender
            .send(Message::Run(Box::new(move |_| {
                entered.send(()).unwrap();
                wait.recv_timeout(HANG).expect("worker gate released");
            })))
            .unwrap();
        ready.recv_timeout(HANG).expect("worker at gate");
        Self(resume)
    }
    fn release(self) {
        self.0.send(()).unwrap();
    }
}

#[test]
fn ui_retirement_and_cancel_only_enqueue_while_the_worker_is_blocked() {
    let root = Root::new("responsive-release");
    let mut store = super::super::DocumentJournalStore::for_test(root.path.clone()).unwrap();
    let (_documents, disk) = snapshot("responsive");
    let decision = store
        .inspect_open("file:///tmp/responsive.md", disk.text.as_bytes())
        .unwrap();
    let initialized = store.initialize(decision, &disk, &disk).unwrap();
    let gate = Gate::hold(&store.host);
    // These run on the caller while every worker filesystem operation is barred.
    FORBID_FS.with(|forbidden| forbidden.set(true));
    assert!(store.forget(&[disk.id]));
    drop(initialized);
    FORBID_FS.with(|forbidden| forbidden.set(false));
    assert!(store.entries.is_empty());
    gate.release();
    fence(&store.host);
    assert!(!exists(&store.path_for_uri("file:///tmp/responsive.md")));

    let host = &store.host;
    let decision = host
        .inspect_open("file:///tmp/canceled.md", disk.text.as_bytes())
        .unwrap();
    let prepared = host.initialize(decision, &disk, &disk).unwrap();
    let path = prepared.path.clone();
    let clone = prepared.clone();
    let gate = Gate::hold(host);
    FORBID_FS.with(|forbidden| forbidden.set(true));
    drop(prepared);
    drop(clone);
    FORBID_FS.with(|forbidden| forbidden.set(false));
    gate.release();
    fence(host);
    assert!(!exists(&path), "one shared cancellation authority");
}

#[test]
fn same_inode_registrations_and_single_transfer_protect_each_other() {
    let root = Root::new("same-inode");
    let host = root.host();
    let uri = "file:///tmp/same.md";
    let (_documents, disk) = snapshot("same");
    let decision = host.inspect_open(uri, disk.text.as_bytes()).unwrap();
    let initialized = host.initialize(decision, &disk, &disk).unwrap();
    let path = initialized.path.clone();
    let first = initialized.take_registration().unwrap().unwrap();
    assert!(initialized.clone().take_registration().is_err());
    let other_host = host.clone();
    let other_path = path.clone();
    let second = host
        .owner
        .as_ref()
        .unwrap()
        .run(move |custody| {
            with_journal_lock(&other_path, JournalLockPatience::Worker, || {
                custody.register(
                    other_host.owner.as_ref().unwrap(),
                    &other_path,
                    other_host.key_for(uri),
                )
            })
            .unwrap()
        })
        .unwrap();
    drop(first);
    fence(&host);
    assert!(exists(&path));
    drop(second);
    fence(&host);
    assert!(!exists(&path));
}

#[test]
fn same_byte_replacement_never_inherits_a_retired_registration() {
    let root = Root::new("same-bytes");
    let host = root.host();
    let (_documents, disk) = snapshot("identical");
    let decision = host
        .inspect_open("file:///tmp/identical.md", disk.text.as_bytes())
        .unwrap();
    let prepared = host.initialize(decision, &disk, &disk).unwrap();
    let path = prepared.path.clone();
    let before = sys::stamp_at(&path).unwrap();
    let bytes = fs::read(&path).unwrap();
    let replacement = root.path.join("replacement");
    fs::write(&replacement, &bytes).unwrap();
    write_mark(
        &replacement,
        &sys::open_regular(&replacement).unwrap(),
        ContentFingerprint::of(&bytes),
        &BTreeSet::from([host.owner_id().unwrap()]),
    )
    .unwrap();
    fs::rename(&replacement, &path).unwrap();
    assert!(!before.same_file(&sys::stamp_at(&path).unwrap()));
    drop(prepared);
    fence(&host);
    assert_eq!(fs::read(&path).unwrap(), bytes);
    assert_eq!(
        named(&path),
        Some(BTreeSet::from([host.owner_id().unwrap()]))
    );
}

#[test]
fn an_undelivered_initialization_result_releases_its_provisional_registration() {
    let root = Root::new("undelivered");
    let host = root.host();
    let (_documents, disk) = snapshot("undelivered");
    let uri = "file:///tmp/undelivered.md";
    let decision = host.inspect_open(uri, disk.text.as_bytes()).unwrap();
    let path = decision.path.clone();
    let worker_host = host.clone();
    let gate = Gate::hold(&host);
    let (sender, receiver) = mpsc::channel();
    host.owner
        .as_ref()
        .unwrap()
        .sender
        .send(Message::Run(Box::new(move |custody| {
            let result = worker_host.initialize_owned(decision, &disk, &disk, Some(custody));
            let _ = sender.send(result);
        })))
        .unwrap();
    drop(receiver); // canceled before initialization and before delivery
    gate.release();
    fence(&host); // the failed delivery enqueues release behind this fence
    fence(&host);
    assert!(!exists(&path));
}

#[test]
fn dirty_inflight_and_failed_entries_retain_their_reducer_and_write_custody() {
    use super::super::{
        DocumentJournalStore, JournalCompletion, JournalEffect, execute_journal_append,
        execute_journal_rewrite,
    };
    let root = Root::new("busy-retire");
    let mut journals = DocumentJournalStore::for_test(root.path.clone()).unwrap();
    let (mut documents, disk) = snapshot("base");
    let uri = "file:///tmp/busy.md";
    let decision = journals.inspect_open(uri, disk.text.as_bytes()).unwrap();
    let path = journals.initialize(decision, &disk, &disk).unwrap().path;
    documents.transact(
        disk.id,
        disk.seq,
        vec![crate::document_store::TextEdit {
            range: 4..4,
            insert: "!".into(),
        }],
    );
    let edited = documents.snapshot(disk.id).unwrap();
    journals.observe_commit(&edited).unwrap();
    assert!(!journals.forget(&[disk.id]), "dirty head retains reducer");
    let Some(JournalEffect::Append {
        path: at,
        key,
        plan,
    }) = journals.next_effect(disk.id).unwrap()
    else {
        panic!("append")
    };
    let gate = Gate::hold(&journals.host);
    let (send, receive) = mpsc::channel();
    let generation = plan.generation;
    std::thread::spawn(move || {
        let result = execute_journal_append(&at, key, &plan, JournalLockPatience::Worker);
        send.send(result).unwrap();
    });
    FORBID_FS.with(|forbidden| forbidden.set(true));
    assert!(
        !journals.forget(&[disk.id]),
        "inflight write retains reducer"
    );
    FORBID_FS.with(|forbidden| forbidden.set(false));
    gate.release();
    let result = receive.recv_timeout(HANG).unwrap();
    assert!(matches!(
        journals.complete_append(disk.id, generation, result),
        JournalCompletion::Durable { .. }
    ));
    journals
        .request_checkpoint(
            crate::native_document_io::DurableCheckpoint {
                document: disk.id,
                seq: edited.seq,
                source: crate::native_document_io::DurableSource::AtomicFile,
            },
            edited.text.clone(),
        )
        .unwrap();
    let Some(JournalEffect::Rewrite { path: at, plan }) = journals.next_effect(disk.id).unwrap()
    else {
        panic!("rewrite")
    };
    assert!(!journals.forget(&[disk.id]));
    let bad = super::super::JournalRewriteResult::Failed("injected failure".into());
    journals.complete_rewrite(disk.id, plan.generation, bad);
    assert!(!journals.forget(&[disk.id]), "failed checkpoint stays owed");
    assert_eq!(journals.owed_documents(), vec![disk.id]);
    drop(plan);
    let Some(JournalEffect::Rewrite { path: _, plan }) = journals.next_effect(disk.id).unwrap()
    else {
        panic!("checkpoint retry")
    };
    let result = execute_journal_rewrite(&at, &plan, JournalLockPatience::Worker);
    assert!(matches!(
        journals.complete_rewrite(disk.id, plan.generation, result),
        JournalCompletion::Durable { .. }
    ));
    drop(plan);
    assert!(
        journals.forget(&[disk.id]),
        "production settled transition permits release"
    );
    fence(&journals.host);
    assert!(
        !exists(&path),
        "checkpoint refreshed the exact pinned image"
    );
}

#[test]
fn asynchronous_release_keeps_uncertain_or_aliased_images() {
    for mutation in [
        "unknown",
        "unreadable-mark",
        "torn",
        "dirty",
        "hardlink",
        "symlink",
        "append",
    ] {
        let root = Root::new(mutation);
        let host = root.host();
        let (_documents, disk) = snapshot("safe");
        let uri = "file:///tmp/safe.md";
        let decision = host.inspect_open(uri, disk.text.as_bytes()).unwrap();
        let prepared = host.initialize(decision, &disk, &disk).unwrap();
        let path = prepared.path.clone();
        let bytes = fs::read(&path).unwrap();
        let alias = root.path.join("alias");
        match mutation {
            "unknown" => {
                sys::write_mark(&path, &sys::open_regular(&path).unwrap(), UNKNOWN_HOLDERS).unwrap()
            }
            "unreadable-mark" => {
                sys::write_mark(&path, &sys::open_regular(&path).unwrap(), b"invalid").unwrap()
            }
            "hardlink" => fs::hard_link(&path, &alias).unwrap(),
            "symlink" => {
                fs::rename(&path, &alias).unwrap();
                #[cfg(unix)]
                std::os::unix::fs::symlink(&alias, &path).unwrap();
                #[cfg(windows)]
                std::os::windows::fs::symlink_file(&alias, &path).unwrap();
            }
            "dirty" => {
                let (_documents, dirty) = snapshot("unsaved draft");
                let bytes = encode_journal(&[
                    JournalRecord::snapshot_for(host.key_for(uri), &disk),
                    JournalRecord::snapshot_for(host.key_for(uri), &dirty),
                ])
                .unwrap();
                fs::write(&path, &bytes).unwrap();
            }
            _ => {
                let mut changed = bytes.clone();
                changed.push(0x12);
                fs::write(&path, &changed).unwrap();
            }
        }
        let expected = fs::read(&path).unwrap();
        drop(prepared);
        fence(&host);
        assert!(exists(&path), "{mutation}");
        assert_eq!(fs::read(&path).unwrap(), expected, "{mutation}");
    }
}

#[test]
fn a_panicked_worker_keeps_active_custody_and_rejects_work_until_logical_owner_exit() {
    for image in ["clean", "dirty", "inflight"] {
        let root = Root::new(&format!("panic-{image}"));
        let host = root.host();
        let (mut documents, disk) = snapshot("base");
        let uri = "file:///tmp/panic.md";
        let decision = host.inspect_open(uri, disk.text.as_bytes()).unwrap();
        if image == "dirty" {
            let _ = documents.transact(
                disk.id,
                disk.seq,
                vec![crate::document_store::TextEdit {
                    range: 4..4,
                    insert: ", unsaved".into(),
                }],
            );
        }
        let current = documents.snapshot(disk.id).unwrap();
        let prepared = host.initialize(decision, &disk, &current).unwrap();
        let path = prepared.path.clone();
        let before = sys::stamp_at(&path).unwrap();
        let worker_path = path.clone();
        let owner = host.owner.as_ref().unwrap();
        let failure: Result<(), String> = owner.run(move |_| {
            if image == "inflight" {
                // Interrupt an in-place mutation while its real production journal lock
                // is held, before a writer could refresh its registration's stamp.
                with_journal_lock(
                    &worker_path,
                    JournalLockPatience::Worker,
                    || -> Result<(), String> {
                        let file = super::super::open_journal_image(&worker_path).unwrap();
                        super::super::write_at(&file, &[0x12], before.len()).unwrap();
                        panic!("injected journal mutation panic");
                    },
                )
                .unwrap();
            }
            panic!("injected journal job panic");
        });
        assert!(failure.is_err(), "the panicked call returns an error");
        let executed = Arc::new(std::sync::atomic::AtomicBool::new(false));
        let attempted = executed.clone();
        assert!(
            owner
                .run(move |_| attempted.store(true, std::sync::atomic::Ordering::SeqCst))
                .is_err()
        );
        assert!(
            !executed.load(std::sync::atomic::Ordering::SeqCst),
            "uncertain state is never reused"
        );
        let owners = root.path.join(OWNERS_DIR);
        let id = host.owner_id().unwrap();
        assert!(
            owner_alive(&owners, id),
            "{image}: live handles still own the lease"
        );
        assert!(before.same_file(&sys::stamp_at(&path).unwrap()));
        let bytes = fs::read(&path).unwrap();
        let sweep = sweep_unheld_journals(&root.path);
        assert_eq!(
            sweep.removed, 0,
            "{image}: external sweep cannot retire an active registration"
        );
        assert_eq!(fs::read(&path).unwrap(), bytes);
        FORBID_FS.with(|forbidden| forbidden.set(true));
        drop(prepared); // rejected release only enqueues; uncertain pins remain on worker
        FORBID_FS.with(|forbidden| forbidden.set(false));
        assert!(
            owner_alive(&owners, id),
            "logical host lifetime still protects custody"
        );
        finish_host(host);
        assert!(!owner_alive(&owners, id));
        let sweep = sweep_unheld_journals(&root.path);
        if image == "clean" {
            assert_eq!(
                sweep.removed, 1,
                "negative control: the sweep can retire this exact clean image after owner exit"
            );
            assert!(!exists(&path));
        } else {
            assert_eq!(
                sweep.removed, 0,
                "{image}: failed writes/drafts remain recoverable after exit"
            );
            assert_eq!(fs::read(&path).unwrap(), bytes);
        }
    }
}

#[test]
fn a_clean_release_keeps_an_owner_whose_lease_cannot_be_probed() {
    let root = Root::new("unknown-lease");
    let host = root.host();
    let (_documents, disk) = snapshot("held");
    let decision = host
        .inspect_open("file:///tmp/held.md", disk.text.as_bytes())
        .unwrap();
    let prepared = host.initialize(decision, &disk, &disk).unwrap();
    let path = prepared.path.clone();
    let unknown = JournalOwnerId::from_raw(host.owner_id().unwrap().raw() ^ 1)
        .unwrap_or_else(|| JournalOwnerId::from_raw(2).unwrap());
    fs::create_dir(root.path.join(OWNERS_DIR).join(unknown.lease_name())).unwrap();
    let bytes = fs::read(&path).unwrap();
    write_mark(
        &path,
        &sys::open_regular(&path).unwrap(),
        ContentFingerprint::of(&bytes),
        &BTreeSet::from([host.owner_id().unwrap(), unknown]),
    )
    .unwrap();
    drop(prepared);
    fence(&host);
    assert!(
        exists(&path),
        "an unreadable/nonregular lease is not a dead owner"
    );
    assert!(named(&path).is_some_and(|owners| owners.contains(&unknown)));
}

#[test]
fn a_canceled_recovered_draft_keeps_its_full_durable_image() {
    let root = Root::new("canceled-draft");
    let host = root.host();
    let (mut documents, disk) = snapshot("baseline");
    let uri = "file:///tmp/canceled-draft.md";
    let decision = host.inspect_open(uri, disk.text.as_bytes()).unwrap();
    let _ = documents.transact(
        disk.id,
        disk.seq,
        vec![crate::document_store::TextEdit {
            range: 8..8,
            insert: ", unsaved".into(),
        }],
    );
    let draft = documents.snapshot(disk.id).unwrap();
    let prepared = host.initialize(decision, &disk, &draft).unwrap();
    let path = prepared.path.clone();
    let bytes = fs::read(&path).unwrap();
    drop(prepared);
    fence(&host);
    assert_eq!(fs::read(&path).unwrap(), bytes);
    let read = recover_journal_image_for(host.key_for(uri), &bytes)
        .unwrap()
        .0;
    assert_eq!(read.text, draft.text.as_ref());
    assert!(!holds_no_draft(&read));
}

#[test]
fn privacy_purge_orders_after_a_queued_release_and_leaves_no_recreated_lock() {
    let root = Root::new("purge-ready-release");
    let mut store = super::super::DocumentJournalStore::for_test(root.path.clone()).unwrap();
    let (_documents, disk) = snapshot("purge");
    let uri = "file:///tmp/purge.md";
    let decision = store.inspect_open(uri, disk.text.as_bytes()).unwrap();
    let path = store.initialize(decision, &disk, &disk).unwrap().path;
    let gate = Gate::hold(&store.host);
    FORBID_FS.with(|forbidden| forbidden.set(true));
    assert!(store.forget(&[disk.id]));
    FORBID_FS.with(|forbidden| forbidden.set(false));
    let host = store.host.clone();
    let (queued, observed) = mpsc::channel();
    let (completed, done) = mpsc::channel();
    let worker = std::thread::spawn(move || {
        ENQUEUED.with(|slot| *slot.borrow_mut() = Some(queued));
        completed.send(host.purge(uri)).unwrap();
    });
    observed
        .recv_timeout(HANG)
        .expect("actual purge queued behind release");
    assert!(matches!(done.try_recv(), Err(mpsc::TryRecvError::Empty)));
    gate.release();
    done.recv_timeout(HANG)
        .expect("purge acknowledged")
        .unwrap();
    worker.join().unwrap();
    assert!(!exists(&path));
    assert!(!exists(&journal_lock_path(&path).unwrap()));
}

#[test]
fn privacy_purge_revokes_stale_plans_and_receipts_without_banning_a_fresh_open() {
    use super::super::{
        DocumentJournalStore, JournalAppendResult, JournalEffect, JournalReseatResult,
        JournalRewriteResult, execute_journal_append, execute_journal_reseat,
        execute_journal_rewrite,
    };
    use crate::native_document_io::{DurableCheckpoint, DurableSource};
    for kind in ["append", "rewrite", "reseat"] {
        let root = Root::new(kind);
        let mut store = DocumentJournalStore::for_test(root.path.clone()).unwrap();
        let (mut documents, disk) = snapshot("base");
        let uri = "file:///tmp/purged.md";
        let decision = store.inspect_open(uri, disk.text.as_bytes()).unwrap();
        let path = store.initialize(decision, &disk, &disk).unwrap().path;
        if kind == "reseat" {
            store.owe_reseat_all();
        } else {
            let _ = documents.transact(
                disk.id,
                disk.seq,
                vec![crate::document_store::TextEdit {
                    range: 0..0,
                    insert: "new ".into(),
                }],
            );
            let head = documents.snapshot(disk.id).unwrap();
            store.observe_commit(&head).unwrap();
            if kind == "rewrite" {
                store
                    .request_checkpoint(
                        DurableCheckpoint {
                            document: disk.id,
                            seq: head.seq,
                            source: DurableSource::AtomicFile,
                        },
                        head.text,
                    )
                    .unwrap();
            }
        }
        let effect = store.next_effect(disk.id).unwrap().unwrap();
        let host = store.host.clone();
        FORBID_FS.with(|forbidden| forbidden.set(true));
        store.decline(disk.id, uri).unwrap();
        FORBID_FS.with(|forbidden| forbidden.set(false));
        assert!(!exists(&path));
        assert!(!exists(&journal_lock_path(&path).unwrap()));
        let run_stale = || match &effect {
            JournalEffect::Append { path, key, plan } => {
                assert!(
                    matches!(execute_journal_append(path, *key, plan, JournalLockPatience::Worker),
                    JournalAppendResult::Failed { message, .. } if message.contains("revoked"))
                );
            }
            JournalEffect::Rewrite { path, plan } => {
                assert!(
                    matches!(execute_journal_rewrite(path, plan, JournalLockPatience::Worker),
                    JournalRewriteResult::Failed(message) if message.contains("revoked"))
                );
            }
            JournalEffect::Reseat { path, plan } => {
                assert!(
                    matches!(execute_journal_reseat(path, plan, JournalLockPatience::Worker),
                    JournalReseatResult::Failed(message) if message.contains("revoked"))
                );
            }
        };
        run_stale();
        assert!(!exists(&path));
        assert!(!exists(&journal_lock_path(&path).unwrap()));
        let decision = host.inspect_open(uri, disk.text.as_bytes()).unwrap();
        let fresh = host.initialize(decision, &disk, &disk).unwrap();
        let bytes = fs::read(&path).unwrap();
        run_stale();
        drop(effect);
        fence(&host);
        assert_eq!(fs::read(&path).unwrap(), bytes);
        assert!(fresh.take_registration().unwrap().is_some());
        // The transferred handle above is dropped once, so the fresh clean open retires.
        fence(&host);
        assert!(!exists(&path));
        assert!(!exists(&journal_lock_path(&path).unwrap()));
    }

    let root = Root::new("purged-receipt");
    let host = root.host();
    let (_documents, disk) = snapshot("receipt");
    let uri = "file:///tmp/receipt.md";
    let decision = host.inspect_open(uri, disk.text.as_bytes()).unwrap();
    let stale = host.initialize(decision, &disk, &disk).unwrap();
    let clone = stale.clone();
    let path = stale.path.clone();
    host.purge(uri).unwrap();
    let decision = host.inspect_open(uri, disk.text.as_bytes()).unwrap();
    let fresh = host.initialize(decision, &disk, &disk).unwrap();
    let bytes = fs::read(&path).unwrap();
    let gate = Gate::hold(&host);
    FORBID_FS.with(|forbidden| forbidden.set(true));
    assert!(
        stale.take_registration().is_err(),
        "revoked receipt cannot adopt"
    );
    drop(stale);
    drop(clone);
    FORBID_FS.with(|forbidden| forbidden.set(false));
    gate.release();
    fence(&host);
    assert_eq!(fs::read(&path).unwrap(), bytes);
    drop(fresh);
    fence(&host);
    assert!(!exists(&path));
}
