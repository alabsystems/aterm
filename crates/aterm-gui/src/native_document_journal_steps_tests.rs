// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! The draft journal as astream steps (format 2): what an append costs, what a save and a
//! compaction cost, and the one-time migration of a format-1 journal.

use super::tests::{
    journaled_edit, open_as_the_app_does, preserved_images, snapshots, test_root, type_and_append,
};
use super::*;
use crate::document_store::TextEdit;
use crate::native_document_io::{encode_journal, encode_journal_v1};

/// A file's length.
fn len_of(path: &Path) -> u64 {
    fs::metadata(path).unwrap().len()
}

/// What durable flushes a publication by rename asks for: the image, and on Unix its folder.
fn rewrite_flushes() -> Vec<Flush> {
    if cfg!(unix) {
        vec![Flush::Durable, Flush::Durable]
    } else {
        vec![Flush::Durable]
    }
}

#[test]
fn appending_does_not_change_a_preserved_hard_link() {
    let (root, mut journals, mut store, id, _disk, path, key) = journaled_edit("preserved-append");
    let before = fs::read(&path).unwrap();
    with_journal_lock(&path, JournalLockPatience::Worker, || {
        preserve_journal_locked(&path, JournalImageGeneration::of(&before))
    })
    .unwrap();
    assert_eq!(preserved_images(&root), vec![before.clone()]);
    let recording = aterm_durable::seam::Recording::start();
    let (_, completion) = type_and_append(&mut journals, &mut store, id, "new edit");
    assert!(matches!(completion, JournalCompletion::Durable { .. }));
    assert_eq!(
        recording.take(),
        rewrite_flushes(),
        "a shared image needs replacement"
    );
    drop(recording);
    assert_eq!(
        preserved_images(&root),
        vec![before],
        "the saved image must remain immutable"
    );
    let recovered = recover_journal_for(key, &fs::read(&path).unwrap()).unwrap();
    assert_eq!(recovered.text, store.snapshot(id).unwrap().text.as_ref());
    let _ = fs::remove_dir_all(root);
}

/// An append writes its frames in place and durably flushes the file before acknowledgement.
/// A compaction or save also flushes its renamed image's folder. Skipping the durable flush
/// or substituting an ordering barrier fails here.
#[test]
fn appends_compactions_and_saves_are_durable_before_acknowledgement() {
    let (root, mut journals, mut store, id, _disk, path, key) = journaled_edit("append-barrier");
    let recording = aterm_durable::seam::Recording::start();
    let mut compactions = 0;
    let mut typed = String::new();
    for n in 0..=COMPACT_RECORDS {
        let before = len_of(&path);
        let letter = char::from(b'a' + (n % 26) as u8).to_string();
        let (plan, completion) = type_and_append(&mut journals, &mut store, id, &letter);
        typed.push_str(&letter);
        assert!(
            matches!(completion, JournalCompletion::Durable { .. }),
            "edit {n}: {completion:?}"
        );
        let flushes = recording.take();
        if len_of(&path) > before {
            assert_eq!(flushes, [Flush::Durable], "edit {n}: durable append");
            let grown = len_of(&path) - before;
            let frame = (JOURNAL_FRAME_HEAD + plan.bytes.len()) as u64;
            assert_eq!(grown, frame, "edit {n}: one frame of the record, in place");
        } else {
            assert_eq!(
                flushes,
                rewrite_flushes(),
                "edit {n}: compaction is a durable rename"
            );
            compactions += 1;
        }
    }
    assert_eq!(
        compactions, 1,
        "the bound is passed once in {COMPACT_RECORDS} records"
    );

    let head = store.snapshot(id).unwrap();
    journals
        .request_checkpoint(
            crate::native_document_io::DurableCheckpoint {
                document: id,
                seq: head.seq,
                source: crate::native_document_io::DurableSource::AtomicFile,
            },
            head.text.clone(),
        )
        .unwrap();
    let Some(JournalEffect::Rewrite { path: at, plan }) = journals.next_effect(id).unwrap() else {
        panic!("a save's checkpoint is planned");
    };
    let result = execute_journal_rewrite(&at, &plan, JournalLockPatience::Worker);
    assert!(matches!(
        journals.complete_rewrite(id, plan.generation, result),
        JournalCompletion::Durable { .. }
    ));
    assert_eq!(recording.take(), rewrite_flushes(), "a save's checkpoint");
    let (_, completion) = type_and_append(&mut journals, &mut store, id, "!");
    assert!(matches!(completion, JournalCompletion::Durable { .. }));
    assert_eq!(recording.take(), [Flush::Durable], "and appends after it");
    drop(recording);

    let recovered = recover_journal_for(key, &fs::read(&path).unwrap()).unwrap();
    assert_eq!(recovered.text, format!("disk+{typed}!"));
    let _ = fs::remove_dir_all(root);
}

/// A draft larger than the compaction bound still appends in place: the bound counts the deltas
/// after the image's last snapshot, not the snapshot, whose record here spans two frames.
#[test]
fn a_draft_larger_than_the_compaction_bound_appends_in_place() {
    const URI: &str = "file:///tmp/seventeen-mib.md";
    let root = test_root("seventeen-mib");
    let mut journals = DocumentJournalStore::for_test(root.clone()).unwrap();
    let mut store = crate::test_document_store();
    let disk = "x".repeat(usize::try_from(COMPACT_BYTES).unwrap() + 1024 * 1024);
    let (_, id, initialized) = open_as_the_app_does(&mut journals, &mut store, URI, &disk);
    assert!(initialized.tail.byte_len() > COMPACT_BYTES);
    let recording = aterm_durable::seam::Recording::start();
    for typed in ["a", "b"] {
        let (_, completion) = type_and_append(&mut journals, &mut store, id, typed);
        assert!(matches!(completion, JournalCompletion::Durable { .. }));
        assert_eq!(recording.take(), [Flush::Durable], "{typed}: in place");
    }
    drop(recording);
    let recovered =
        recover_journal_for(initialized.key, &fs::read(&initialized.path).unwrap()).unwrap();
    assert_eq!(recovered.text, format!("{disk}ab"));
    let _ = fs::remove_dir_all(root);
}

/// The judge's count: a committed edit to a 4 MiB document costs one full file flush and about a
/// record's bytes, where a whole-journal rewrite cost two full flushes and the document's size.
#[test]
fn an_edit_to_a_4_mib_draft_writes_a_record_and_durably_flushes_once() {
    const URI: &str = "file:///tmp/four-mib.md";
    const EDITS: usize = 8;
    let root = test_root("four-mib");
    let mut journals = DocumentJournalStore::for_test(root.clone()).unwrap();
    let mut store = crate::test_document_store();
    let disk = "0123456789abcdef\n".repeat(4 * 1024 * 1024 / 17);
    let (_, id, initialized) = open_as_the_app_does(&mut journals, &mut store, URI, &disk);
    let recording = aterm_durable::seam::Recording::start();
    let tally = astream_sys::flush_tally();
    let start = len_of(&initialized.path);
    for n in 0..EDITS {
        let (_, completion) = type_and_append(&mut journals, &mut store, id, &format!("{n}"));
        assert!(matches!(completion, JournalCompletion::Durable { .. }));
    }
    let flushes = recording.take();
    drop(recording);
    let after = astream_sys::flush_tally();
    let written = len_of(&initialized.path) - start;
    assert_eq!(
        flushes,
        vec![Flush::Durable; EDITS],
        "durable before acknowledgement"
    );
    assert!(
        written < (EDITS * 256) as u64,
        "{written} bytes for {EDITS} edits to a {} byte draft",
        disk.len()
    );
    eprintln!(
        "F08 4 MiB draft ({} bytes): {EDITS} edits wrote {written} bytes ({} per edit); this \
         thread asked for {:?}; process tally full +{} barrier +{}{}",
        disk.len(),
        written / EDITS as u64,
        flushes,
        after.full - tally.full,
        after.barrier - tally.barrier,
        aterm_durable::durable_flushes_note(),
    );
    let recovered =
        recover_journal_for(initialized.key, &fs::read(&initialized.path).unwrap()).unwrap();
    assert_eq!(recovered.text, store.snapshot(id).unwrap().text.as_ref());
    let _ = fs::remove_dir_all(root);
}

/// A crash in the middle of an append leaves part of its record after the last whole one: the
/// next open recovers the draft as of that record, from every cut, and publishes it whole.
#[test]
fn a_torn_tail_recovers_to_the_last_whole_record() {
    let (root, mut journals, mut store, id, _disk, path, key) = journaled_edit("torn-tail");
    let whole = fs::read(&path).unwrap();
    let (_, completion) = type_and_append(&mut journals, &mut store, id, " lost");
    assert!(matches!(completion, JournalCompletion::Durable { .. }));
    let typed = fs::read(&path).unwrap();
    for cut in whole.len()..typed.len() {
        let recovered = recover_journal_for(key, &typed[..cut]).unwrap();
        assert_eq!(recovered.text, "disk+", "cut at {cut} of {}", typed.len());
    }
    drop(journals);
    let torn = typed[..whole.len() + 5].to_vec();
    fs::write(&path, &torn).unwrap();

    let host = DraftJournalHost::new(root.clone()).unwrap();
    let decision = host.inspect_open("file:///tmp/draft.md", b"disk").unwrap();
    assert_eq!(decision.recovered_text.as_deref(), Some("disk+"));
    assert_eq!(decision.notice, Some(RecoveryNotice::Recovered));
    let (mut documents, document, disk) = snapshots("disk");
    let _ = documents.transact(
        document,
        disk.seq,
        vec![TextEdit {
            range: 4..4,
            insert: "+".into(),
        }],
    );
    let head = documents.snapshot(document).unwrap();
    let initialized = host.initialize(decision, &disk, &head).unwrap();
    let published = fs::read(&path).unwrap();
    assert_eq!(
        initialized.tail.byte_len(),
        published.len() as u64,
        "nothing torn is left"
    );
    assert_eq!(recover_journal_for(key, &published).unwrap().text, "disk+");
    assert_eq!(
        preserved_images(&root),
        vec![torn],
        "keep the uninterpreted original tail"
    );
    // A complete final frame with a damaged body can have the same torn classification.
    // Preserve that original too, rather than silently throwing its bytes away.
    let mut damaged = typed;
    *damaged.last_mut().unwrap() ^= 0x80;
    fs::write(&path, &damaged).unwrap();
    let decision = host.inspect_open("file:///tmp/draft.md", b"disk").unwrap();
    assert_eq!(decision.recovered_text.as_deref(), Some("disk+"));
    host.initialize(decision, &disk, &head).unwrap();
    assert!(
        preserved_images(&root).contains(&damaged),
        "keep the damaged last frame"
    );
    let _ = fs::remove_dir_all(root);
}

/// A byte flipped inside the journal, with a whole record after it, is a fault: the open
/// recovers nothing, says the draft could not be read, and keeps the image as it is.
#[test]
fn a_flipped_byte_mid_journal_is_refused_and_the_file_kept() {
    let (root, mut journals, mut store, id, _disk, path, key) = journaled_edit("flipped-byte");
    let (_, completion) = type_and_append(&mut journals, &mut store, id, " more");
    assert!(matches!(completion, JournalCompletion::Durable { .. }));
    let mut damaged = fs::read(&path).unwrap();
    let middle = damaged.len() / 2;
    damaged[middle] ^= 0x10;
    fs::write(&path, &damaged).unwrap();
    assert!(recover_journal_for(key, &damaged).is_err());
    drop(journals);

    let host = DraftJournalHost::new(root.clone()).unwrap();
    let decision = host.inspect_open("file:///tmp/draft.md", b"disk").unwrap();
    assert_eq!(decision.notice, Some(RecoveryNotice::Corrupt));
    assert_eq!(decision.recovered_text, None);
    assert_eq!(
        fs::read(&path).unwrap(),
        damaged,
        "the inspection writes nothing"
    );
    let (_store, _, disk) = snapshots("disk");
    host.initialize(decision, &disk, &disk).unwrap();
    assert_eq!(
        preserved_images(&root),
        vec![damaged],
        "kept aside, byte for byte"
    );
    let _ = fs::remove_dir_all(root);
}

/// A format-1 journal an earlier aterm left under the keyed name.
fn write_keyed_v1_journal(
    journals: &DocumentJournalStore,
    uri: &str,
    disk: &str,
    typed: &[&str],
) -> (PathBuf, Vec<u8>) {
    let key = journals.key_for(uri);
    let mut store = crate::test_document_store();
    let id = store.open(uri.to_string(), disk.to_string());
    let base = store.snapshot(id).unwrap();
    let mut records = vec![JournalRecord::snapshot_for(key, &base)];
    let (mut seq, mut text) = (base.seq, disk.to_string());
    for next in typed {
        let after = Seq(seq.0 + 1);
        let longer = format!("{text}{next}");
        records.push(
            JournalRecord::delta_for(key, seq, after, vec![single_edit(&text, &longer)]).unwrap(),
        );
        (seq, text) = (after, longer);
    }
    let path = journals.path_for_uri(uri);
    let image = encode_journal_v1(&records).unwrap();
    fs::create_dir_all(path.parent().unwrap()).unwrap();
    fs::write(&path, &image).unwrap();
    (path, image)
}

/// A format-1 journal is read through its own decoder at the open, and the open republishes
/// its draft in format 1: an update from an aterm before format 2 opens the document in the
/// successor while the outgoing aterm still reads the journal back (its Commit check of the
/// carried draft, and on a rollback its re-seat), through format 1's decoder alone. The first
/// write after the open publishes format 2 whole, with the identical text and baseline, and
/// appends in place after it. A format-1 file that does not decode is kept untouched.
#[test]
fn a_format_1_journal_stays_format_1_until_its_first_write() {
    const URI: &str = "file:///tmp/format-1.md";
    let root = test_root("format-1-migrates");
    let mut journals = DocumentJournalStore::for_test(root.clone()).unwrap();
    let (path, v1) = write_keyed_v1_journal(&journals, URI, "disk", &[" and", " a λ draft"]);
    let key = journals.key_for(URI);
    let read_v1 = recover_journal_for(key, &v1).unwrap();
    assert_eq!(read_v1.text, "disk and a λ draft");

    let recording = aterm_durable::seam::Recording::start();
    let mut store = crate::test_document_store();
    let (decision, id, initialized) = open_as_the_app_does(&mut journals, &mut store, URI, "disk");
    assert_eq!(
        recording.take(),
        rewrite_flushes(),
        "the open's image is durable before it is the journal"
    );
    assert_eq!(
        decision.recovered_text.as_deref(),
        Some(read_v1.text.as_str())
    );
    assert_eq!(initialized.notice, Some(RecoveryNotice::Recovered));
    assert_eq!(initialized.path, path);
    let opened = fs::read(&path).unwrap();
    let outgoing = crate::native_document_io::recover_journal_v1_for(key, &opened)
        .expect("an aterm before format 2 reads the open's image");
    assert_eq!(outgoing.text, store.snapshot(id).unwrap().text.as_ref());
    assert_eq!(outgoing.base_content, ContentFingerprint::of(b"disk"));
    assert_eq!(
        initialized.tail.fingerprint(),
        ContentFingerprint::of(&opened)
    );

    let (_, completion) = type_and_append(&mut journals, &mut store, id, "!");
    assert!(matches!(completion, JournalCompletion::Durable { .. }));
    assert_eq!(
        recording.take(),
        rewrite_flushes(),
        "the first write publishes format 2 whole"
    );
    let migrated = fs::read(&path).unwrap();
    assert!(migrated.starts_with(b"ATDJ\0\x02S\n"), "format 2 now");
    let read_v2 = recover_journal_for(key, &migrated).unwrap();
    assert_eq!(read_v2.text, format!("{}!", read_v1.text));
    assert_eq!(read_v2.base_content, read_v1.base_content);
    let (_, completion) = type_and_append(&mut journals, &mut store, id, "?");
    assert!(matches!(completion, JournalCompletion::Durable { .. }));
    assert_eq!(
        recording.take(),
        [Flush::Durable],
        "then appends durably in place"
    );
    drop(recording);
    let reopened = recover_journal_for(key, &fs::read(&path).unwrap()).unwrap();
    assert_eq!(reopened.text, format!("{}!?", read_v1.text));

    const BROKEN: &str = "file:///tmp/format-1-broken.md";
    let (broken, mut image) = write_keyed_v1_journal(&journals, BROKEN, "disk", &[" draft"]);
    let last = image.len() - 1;
    image[last] ^= 0x01;
    fs::write(&broken, &image).unwrap();
    let (decision, _, _) = open_as_the_app_does(&mut journals, &mut store, BROKEN, "disk");
    assert_eq!(decision.notice, Some(RecoveryNotice::Corrupt));
    assert_eq!(decision.recovered_text, None);
    assert_eq!(preserved_images(&root), vec![image], "kept aside untouched");
    let _ = fs::remove_dir_all(root);
}

/// A migration (the first write after an open of a format-1 journal) that died after its
/// format-2 image was written and before it went over the format-1 journal leaves both on
/// disk: the format-1 journal under its name, the image in the writer's temporary. The next
/// open recovers the draft once, from the journal, and the stray temporary adds nothing to
/// it; after that open, another recovers it once again.
#[test]
fn a_crash_between_writing_format_2_and_retiring_format_1_recovers_the_draft_once() {
    const URI: &str = "file:///tmp/format-1-crash.md";
    let root = test_root("format-1-crash");
    let mut journals = DocumentJournalStore::for_test(root.clone()).unwrap();
    let (path, _) = write_keyed_v1_journal(&journals, URI, "disk", &[" draft"]);
    let key = journals.key_for(URI);
    let mut store = crate::test_document_store();
    let id = store.open(URI.to_string(), "disk".to_string());
    let base = store.snapshot(id).unwrap();
    let draft = synthetic_snapshot(id, Seq(base.seq.0 + 1), Arc::from("disk draft"));
    let written = encode_journal(&[
        JournalRecord::snapshot_for(key, &base),
        JournalRecord::snapshot_for(key, &draft),
    ])
    .unwrap();
    let name = path.file_name().unwrap().to_str().unwrap();
    let temporary = root.join(format!(".{name}.{}-1.tmp", std::process::id()));
    fs::write(&temporary, &written).unwrap();

    let mut store = crate::test_document_store();
    let (decision, id, _) = open_as_the_app_does(&mut journals, &mut store, URI, "disk");
    assert_eq!(decision.recovered_text.as_deref(), Some("disk draft"));
    assert_eq!(store.snapshot(id).unwrap().text.as_ref(), "disk draft");
    let image = fs::read(&path).unwrap();
    let recovered = recover_journal_for(key, &image).unwrap();
    assert_eq!(
        (recovered.text.as_str(), recovered.record_count),
        ("disk draft", 2)
    );
    drop(journals);

    let journals = DocumentJournalStore::for_test(root.clone()).unwrap();
    let reopened = journals.inspect_open(URI, b"disk").unwrap();
    assert_eq!(reopened.recovered_text.as_deref(), Some("disk draft"));
    assert_eq!(reopened.notice, Some(RecoveryNotice::Recovered));
    assert!(preserved_images(&root).is_empty(), "nothing was set aside");
    let _ = fs::remove_dir_all(root);
}
