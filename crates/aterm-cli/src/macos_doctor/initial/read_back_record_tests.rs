// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

use super::*;
use aterm_types::macos_setup::{CHANGED_SINCE, Setting};

fn reading(setting: Setting, standing: Standing, note: Option<&str>) -> Observation {
    Observation {
        standing,
        now: Some("as read".into()),
        note: note.map(str::to_owned),
        ..Observation::unknown(Subject::Setting(setting))
    }
    .stamped(1_700_000_000)
}

#[test]
fn a_missing_journal_fence_is_not_an_explicit_empty_journal() {
    let scratch = aterm_tempfile::tempdir().unwrap();
    let journal = scratch.path().join("journal.jsonl");
    let dock = reading(Setting::Dock, Standing::InPlace, None);
    let saved = record(7, &[(dock.clone(), Fence(None))], &journal);
    assert_eq!(parse(&saved, 7, &journal), [(dock, Fence(None))]);
    let mut entry = saved["rows"][0].clone();
    entry.as_object_mut().unwrap().remove("journal");
    let saved = json!({"format": READ_BACK_FORMAT, "revision": 7, "rows": [entry]});
    assert!(
        parse(&saved, 7, &journal).is_empty(),
        "a missing fence supplies no evidence of an empty journal"
    );
}

#[test]
fn unusable_read_backs_do_not_acquire_the_journal_lock() {
    let journal = Journaled::new();
    let path = journal.path();
    let saved = record(7, &[(journal.dock.clone(), journal.fence())], &path);
    let lock = path.with_file_name(aterm_converge::FileJournal::LOCK);
    // The fixture's writer has finished; removing its lock file makes the next read observable.
    std::fs::remove_file(&lock).unwrap();
    assert_eq!(parse(&saved, 7, &path).len(), 1);
    assert!(
        lock.exists(),
        "CONTROL: a valid reading checks real history"
    );
    std::fs::remove_file(&lock).unwrap();

    for (field, value) in [
        ("row", json!("not-a-row")),
        (
            "row",
            json!(Subject::Setting(Setting::Secrets).setup_word()),
        ),
        ("standing", json!("not-a-standing")),
        ("standing", json!(Standing::Differs.word())),
        ("wanted", json!(7)),
        ("read_at", Value::Null),
    ] {
        let mut entry = saved["rows"][0].clone();
        entry
            .as_object_mut()
            .unwrap()
            .insert(field.to_owned(), value);
        let invalid = json!({
            "format": READ_BACK_FORMAT,
            "revision": 7,
            "rows": [entry],
        });
        assert!(parse(&invalid, 7, &path).is_empty(), "{field}: {invalid}");
        assert!(
            !lock.exists(),
            "an unusable reading acquired the journal lock: {field}"
        );
    }
}

/// A launch keeps what it read back settled of the private rows, and the next launch reads
/// it back exactly, its time included, under the same revision while the journal holds no
/// change written since each row's read started. Anything else is no reading: a row that
/// reads otherwise, an undated one, a row read in the background (the vault), a newer plan,
/// a change to such a row some pass journaled since (before it made it), a journal replaced
/// with no rotation of it kept, an other format, or a torn record. A failure line journaled
/// since changes nothing. Read of the real file.
#[test]
fn the_last_read_back_holds_only_under_its_revision_while_nothing_changed() {
    use std::io::Write as _;
    let dock = reading(Setting::Dock, Standing::InPlace, None);
    let caps = reading(Setting::CapsLock, Standing::Differs, Some(CHANGED_SINCE));
    let trackpad = reading(Setting::TrackpadScrolling, Standing::AtLogin, None);
    let spotlight = reading(Setting::Spotlight, Standing::InPlace, None);
    let drifted = reading(Setting::Desktop, Standing::Differs, None);
    let undated = Observation {
        standing: Standing::InPlace,
        ..Observation::unknown(Subject::Setting(Setting::Widgets))
    };
    let vault = reading(Setting::Secrets, Standing::InPlace, None);
    let scratch = aterm_tempfile::tempdir().unwrap();
    let journal = scratch.path().join("journal.jsonl");
    let append = |line: &str| {
        let mut file = std::fs::OpenOptions::new()
            .create(true)
            .append(true)
            .open(&journal)
            .unwrap();
        file.write_all(line.as_bytes()).unwrap();
    };
    append("{\"time\":1,\"id\":\"init.dock@alice\",\"from\":\"a\",\"to\":\"b\"}\n");
    let fence = Fence(JournalMark::of(&journal).unwrap());
    let current = |rows: &[(Observation, Fence)]| -> Vec<(Observation, Fence)> {
        rows.iter()
            .map(|(row, _)| (row.clone(), Fence(JournalMark::of(&journal).unwrap())))
            .collect()
    };
    let fenced = |rows: &[Observation]| -> Vec<(Observation, Fence)> {
        rows.iter().cloned().map(|seen| (seen, fence)).collect()
    };
    let written = record(
        7,
        &fenced(&[
            dock.clone(),
            caps.clone(),
            trackpad.clone(),
            spotlight.clone(),
            drifted,
            undated,
            vault,
        ]),
        &journal,
    );
    let kept = fenced(&[dock, caps, trackpad, spotlight]);
    assert_eq!(parse(&written, 7, &journal), current(&kept));
    assert!(
        parse(&written, 8, &journal).is_empty(),
        "another plan's revision"
    );
    append("{\"time\":2,\"id\":\"repos\",\"failed\":\"offline\"}\n");
    assert_eq!(
        parse(&written, 7, &journal),
        current(&kept),
        "a failure changed nothing"
    );
    for format in [READ_BACK_FORMAT - 1, READ_BACK_FORMAT + 1] {
        let other = json!({
            "format": format,
            "revision": 7,
            "rows": written["rows"].clone(),
        });
        assert!(parse(&other, 7, &journal).is_empty(), "format {format}");
    }
    assert!(
        parse(&json!({"rows": 1}), 7, &journal).is_empty(),
        "a torn record"
    );
    append("{\"time\":3,\"id\":\"init.dock@alice\",\"from\":\"b\",\"to\":\"a\"}\n");
    assert!(
        parse(&written, 7, &journal).is_empty(),
        "a change journaled since"
    );

    // Another file where the journal was, and the fenced one not kept as its rotation
    // (`FileJournal::ROTATED`) beside it: no promise.
    let rotated = scratch.path().join("journal.1.jsonl");
    let now = Fence(JournalMark::of(&journal).unwrap());
    let kept: Vec<_> = kept.into_iter().map(|(seen, _)| (seen, now)).collect();
    let fresh = record(7, &kept, &journal);
    assert_eq!(parse(&fresh, 7, &journal), current(&kept));
    std::fs::rename(&journal, &rotated).unwrap();
    append("{\"time\":4,\"id\":\"repos\",\"failed\":\"offline\"}\n");
    assert!(parse(&fresh, 7, &journal).is_empty(), "a replaced journal");

    // With no journal then, a record holds while nothing but failures was journaled since.
    std::fs::remove_file(&journal).unwrap();
    let kept: Vec<_> = kept
        .into_iter()
        .map(|(seen, _)| (seen, Fence(None)))
        .collect();
    let none = record(7, &kept, &journal);
    assert_eq!(parse(&none, 7, &journal), current(&kept));
    append("{\"time\":5,\"id\":\"repos\",\"failed\":\"offline\"}\n");
    assert_eq!(parse(&none, 7, &journal), current(&kept));
    append("{\"time\":6,\"id\":\"init.dock@alice\",\"from\":\"a\",\"to\":\"b\"}\n");
    assert!(parse(&none, 7, &journal).is_empty());
}

/// Review finding 3: each row is fenced by the journal as its read started, never as the
/// record is written, so a change any aterm process journals between a row's read and the
/// write (a terminal `aterm init` setting the Dock while this window held its reading) keeps
/// that reading out of the record; a row read after the change, fenced after it, is kept.
/// Before, the record took the journal as it was written, after the change, and the next
/// launch carried the earlier reading as set up.
/// NEGATIVE CONTROL: with no change between read and write, the row is kept and read back.
#[test]
fn a_change_between_a_rows_read_and_the_write_keeps_that_reading_out() {
    use std::io::Write as _;
    let scratch = aterm_tempfile::tempdir().unwrap();
    let journal = scratch.path().join("journal.jsonl");
    let append = |line: &str| {
        let mut file = std::fs::OpenOptions::new()
            .create(true)
            .append(true)
            .open(&journal)
            .unwrap();
        file.write_all(line.as_bytes()).unwrap();
    };
    append("{\"time\":1,\"id\":\"repos\",\"failed\":\"offline\"}\n");
    let dock = reading(Setting::Dock, Standing::InPlace, None);
    let caps = reading(Setting::CapsLock, Standing::InPlace, None);
    let dock_read = Fence(JournalMark::of(&journal).unwrap());
    let unchanged = record(7, &[(dock.clone(), dock_read)], &journal);
    assert_eq!(
        parse(&unchanged, 7, &journal),
        [(dock.clone(), dock_read)],
        "CONTROL: nothing changed between read and write"
    );
    append("{\"time\":2,\"id\":\"init.dock@alice\",\"from\":\"a\",\"to\":\"b\"}\n");
    let caps_read = Fence(JournalMark::of(&journal).unwrap());
    let written = record(7, &[(dock, dock_read), (caps.clone(), caps_read)], &journal);
    assert_eq!(
        parse(&written, 7, &journal),
        [(caps, caps_read)],
        "the Dock read before the change is not kept; Caps Lock read after it is"
    );
}

/// A change journal as the real writer keeps it under a scratch state directory, and the Dock
/// read back settled.
struct Journaled {
    _scratch: aterm_tempfile::TempDir,
    writer: aterm_converge::FileJournal,
    dock: Observation,
}

impl Journaled {
    /// One whose journal already holds a Dock change, so a fence taken next marks a file.
    fn new() -> Self {
        let scratch = aterm_tempfile::tempdir().unwrap();
        let mut this = Self {
            writer: aterm_converge::FileJournal::under(scratch.path()),
            _scratch: scratch,
            dock: reading(Setting::Dock, Standing::InPlace, None),
        };
        this.change("init.dock@alice");
        this
    }

    fn path(&self) -> PathBuf {
        self.writer.path()
    }

    /// The journal as it stands: what a read starting now is fenced by.
    fn fence(&self) -> Fence {
        Fence(JournalMark::of(&self.path()).unwrap())
    }

    /// The writer's own change line for item `id`.
    fn change(&mut self, id: &str) {
        use aterm_converge::Journal as _;
        self.writer.before_change(id, "a", "b", None).unwrap();
    }

    /// `line`, appended to the journal as it is.
    fn raw(&self, line: &str) {
        use std::io::Write as _;
        std::fs::OpenOptions::new()
            .create(true)
            .append(true)
            .open(self.path())
            .unwrap()
            .write_all(line.as_bytes())
            .unwrap();
    }

    /// An older writer's rotation with no retained-history update: the journal becomes
    /// the one rotation kept, and the next line starts a new file. Modern rotations are
    /// exercised through the real writer in the no-intervening-check regression.
    fn rotate(&self) {
        std::fs::rename(self.path(), self.writer.rotated()).unwrap();
    }

    /// Whether the Dock reading fenced by `fence` is both written into a record now and read
    /// back from it: the two places its fence is checked.
    fn keeps(&self, fence: Fence) -> bool {
        let journal = self.path();
        let rows = [(self.dock.clone(), fence)];
        let written = record(7, &rows, &journal);
        let read = parse(&written, 7, &journal);
        assert!(
            read.is_empty() || read == [(self.dock.clone(), self.fence())],
            "a kept row is read back as it was written: {read:?}"
        );
        !read.is_empty()
    }
}

/// The sync lane journals a change on most of its passes and the Repositories stage on its
/// refreshes, into the journal that fences the private rows' kept readings, so on a Mac whose
/// vault is joined a change line landed within a minute of every read and no reading reached
/// the next launch: each such row read `unread:` after every relaunch. Those items write no
/// privately read row, so their change lines leave a kept reading standing.
/// NEGATIVE CONTROL: with `leaves_private_rows` counting every change line, as before, the Dock
/// reading is dropped at the first `sync.requests` line; and a Dock change beside them still
/// drops it here.
#[test]
fn a_sync_or_repositories_change_since_a_rows_read_leaves_its_reading_kept() {
    let mut journaled = Journaled::new();
    let fence = journaled.fence();
    assert!(journaled.keeps(fence), "CONTROL: nothing journaled since");
    journaled.change("sync.requests");
    journaled.change("init.github.native@alice");
    journaled.change("sync.clone");
    assert!(
        journaled.keeps(fence),
        "changes of the sync lane and the Repositories stage"
    );
    assert_ne!(journaled.fence(), fence, "those lines were journaled");
    journaled.change("init.dock@alice");
    assert!(!journaled.keeps(fence), "a Dock change among them");
}

/// Only an id known to write no privately read row is passed over: a change under an id this
/// build does not know, under an id that only looks like the sync lane's or the Repositories
/// stage's, a change line that names no item, and a line that cannot be read each keep the
/// reading out, as any change did before.
/// NEGATIVE CONTROL: each case holds the reading before its line is appended; with
/// `leaves_private_rows` passing every change line that names an item, the first case is kept.
#[test]
fn a_change_under_any_other_id_or_none_keeps_the_reading_out() {
    for line in [
        "{\"time\":2,\"id\":\"init.future@alice\",\"from\":\"a\",\"to\":\"b\"}\n",
        "{\"time\":2,\"id\":\"sync.dock\",\"from\":\"a\",\"to\":\"b\"}\n",
        "{\"time\":2,\"id\":\"sync.requests@alice\",\"from\":\"a\",\"to\":\"b\"}\n",
        "{\"time\":2,\"id\":\"init.github\",\"from\":\"a\",\"to\":\"b\"}\n",
        "{\"time\":2,\"from\":\"a\",\"to\":\"b\"}\n",
        "{\"time\":2,\"id\":7,\"from\":\"a\",\"to\":\"b\"}\n",
        "{\"time\":2,\"failed\":\"offline\"}\n",
        "{\"time\":2,\"id\":7,\"failed\":\"offline\"}\n",
        "{\"time\":2,\"id\":\"sync.requests\",\"from\":\"a\"\n",
        "sync.requests\n",
    ] {
        let mut journaled = Journaled::new();
        let fence = journaled.fence();
        journaled.change("sync.requests");
        assert!(journaled.keeps(fence), "CONTROL: before {line}");
        journaled.raw(line);
        assert!(!journaled.keeps(fence), "{line}");
    }
}

/// The journal rotates every few days on a Mac whose sync lane runs, and a rotation dropped
/// every kept reading. The rotated file is the fenced one, which nothing appends to any
/// longer, so its lines past the fence and every line of the journal that took its place are
/// the lines since, judged alike: sync changes alone keep the reading, and a Dock change in
/// either file drops it.
/// NEGATIVE CONTROL: with `no_change_since` promising nothing for a journal that is another
/// file than the fenced one, as before, the first reading is dropped.
#[test]
fn a_rotation_with_only_sync_changes_since_keeps_the_reading() {
    let mut journaled = Journaled::new();
    let fence = journaled.fence();
    journaled.change("sync.requests");
    journaled.rotate();
    journaled.change("sync.identity");
    assert_ne!(journaled.fence(), fence, "another file is the journal now");
    assert!(journaled.keeps(fence), "sync changes on both sides");
    journaled.change("init.dock@alice");
    assert!(!journaled.keeps(fence), "a Dock change after the rotation");

    let mut journaled = Journaled::new();
    let fence = journaled.fence();
    journaled.change("init.dock@alice");
    journaled.rotate();
    journaled.change("sync.identity");
    assert!(!journaled.keeps(fence), "a Dock change before the rotation");
}

/// A rotation answers only for the file the fence names: where the one kept beside the journal
/// is another file (the journal rotated twice, so the fenced one is gone), is shorter than the
/// fence, or is missing, the lines since cannot all be read and the reading is not kept; nor is
/// a reading fenced by no journal once a rotation is kept, whose earlier lines may be gone.
/// NEGATIVE CONTROL: with the fenced file as the rotation the reading is kept (the control
/// here); with `no_change_past` not comparing the file's inode, the first case is kept too,
/// since the other file holds as long a first line and sync changes after it.
#[test]
fn a_rotation_that_does_not_keep_the_fenced_file_keeps_the_reading_out() {
    // The fenced file moved away (alive, so its inode is not handed out again), and another
    // file is the rotation: as long a first line, then a sync change.
    let mut journaled = Journaled::new();
    let fence = journaled.fence();
    journaled.change("sync.requests");
    let away = journaled.path().with_file_name("away.ndjson");
    std::fs::rename(journaled.path(), &away).unwrap();
    journaled.change("init.dock@alice");
    assert_eq!(
        JournalMark::of(&journaled.path())
            .unwrap()
            .map(|mark| mark.len),
        fence.0.map(|mark| mark.len),
        "the other file's first line ends where the fence does"
    );
    journaled.change("sync.requests");
    journaled.rotate();
    journaled.change("sync.identity");
    assert!(!journaled.keeps(fence), "the rotation is another file");
    std::fs::rename(&away, journaled.writer.rotated()).unwrap();
    assert!(
        journaled.keeps(fence),
        "CONTROL: the fenced file as the rotation"
    );

    // The fenced file is the rotation, cut shorter than the fence.
    let cut = std::fs::OpenOptions::new()
        .write(true)
        .open(journaled.writer.rotated())
        .unwrap();
    cut.set_len(1).unwrap();
    assert!(!journaled.keeps(fence), "a rotation shorter than the fence");

    // No rotation kept at all.
    std::fs::remove_file(journaled.writer.rotated()).unwrap();
    assert!(!journaled.keeps(fence), "no rotation kept");

    // A reading fenced by no journal: every line since is the journal's while it never
    // rotated, and no longer once a rotation is kept.
    let none = Fence(None);
    assert!(
        journaled.keeps(none),
        "CONTROL: a first journal of sync lines"
    );
    journaled.rotate();
    journaled.change("sync.identity");
    assert!(
        !journaled.keeps(none),
        "a rotation since, with no file fenced"
    );
    // The same with no journal standing: the rotation alone is kept.
    std::fs::remove_file(journaled.path()).unwrap();
    assert!(
        !journaled.keeps(none),
        "a rotation kept and no journal, with no file fenced"
    );
    std::fs::remove_file(journaled.writer.rotated()).unwrap();
    assert!(journaled.keeps(none), "CONTROL: no journal and no rotation");
}

/// Reading the record and writing it each advance the fence over inspected, unrelated
/// changes. Neither path may skip a setting change. The old, stationary-fence behavior is
/// the negative control: its mark is gone after the second rotation.
#[test]
fn inspected_journal_tails_advance_the_fence_across_repeated_rotations() {
    for refresh_on_read in [false, true] {
        let mut journaled = Journaled::new();
        let original = journaled.fence();
        let mut rows = vec![(journaled.dock.clone(), original)];
        let mut value = record(7, &rows, &journaled.path());
        for _ in 0..8 {
            journaled.change("sync.requests");
            journaled.rotate();
            journaled.change("sync.clone");
            if refresh_on_read {
                rows = parse(&value, 7, &journaled.path());
                assert_eq!(rows, [(journaled.dock.clone(), journaled.fence())]);
            }
            value = record(7, &rows, &journaled.path());
            assert_eq!(
                value["rows"][0]["journal"],
                JournalMark::record(journaled.fence().0)
            );
            rows = parse(&value, 7, &journaled.path());
            assert_eq!(rows, [(journaled.dock.clone(), journaled.fence())]);
        }
        assert!(
            !no_change_since(original.0, &journaled.path()),
            "CONTROL: an unadvanced fence lost its history"
        );
        journaled.change("init.dock@alice");
        assert!(
            parse(&value, 7, &journaled.path()).is_empty(),
            "a read must not hide a setting change"
        );
        assert!(
            record(7, &rows, &journaled.path())["rows"]
                .as_array()
                .unwrap()
                .is_empty(),
            "a write must not hide a setting change"
        );
    }
}

/// The running process receives only fences it saved successfully. A repeated pass over the
/// same tail replaces no file. Refusing the write is the negative control for publication.
#[test]
fn a_saved_fence_advances_in_memory_only_after_publication() {
    let mut journaled = Journaled::new();
    let path = journaled.path().with_file_name("read-back.json");
    let original = journaled.fence();
    let mut rows = vec![(journaled.dock.clone(), original)];
    journaled.change("sync.clone");
    let blocked = path.join("no-parent");
    std::fs::write(&path, b"not a directory").unwrap();
    assert!(remember_in(&blocked, &journaled.path(), 7, &mut rows).is_err());
    assert_eq!(rows[0].1, original, "CONTROL: no published fence");
    remember_in(&path, &journaled.path(), 7, &mut rows).unwrap();
    assert_eq!(rows[0].1, journaled.fence());
    let saved = JournalMark::of(&path).unwrap();
    remember_in(&path, &journaled.path(), 7, &mut rows).unwrap();
    assert_eq!(
        JournalMark::of(&path).unwrap(),
        saved,
        "no replacement with identical bytes"
    );
    journaled.change("init.dock@alice");
    let before = rows[0].1;
    remember_in(&path, &journaled.path(), 7, &mut rows).unwrap();
    assert_eq!(
        rows[0].1, before,
        "invalidated readings never acquire a new fence"
    );
    let value: Value = aterm_json::from_slice(&std::fs::read(&path).unwrap()).unwrap();
    assert!(value["rows"].as_array().unwrap().is_empty());
}

#[test]
fn duplicate_subjects_cannot_rebind_a_rejected_readings_fence() {
    let mut journaled = Journaled::new();
    let path = journaled.path().with_file_name("read-back.json");
    let old = journaled.fence();
    let stale = journaled.dock.clone();
    journaled.change("init.dock@alice");
    let fresh = journaled.fence();
    let current = Observation {
        now: Some("new reading".to_owned()),
        ..journaled.dock.clone()
    };
    let mut valid = [(current.clone(), fresh)];
    remember_in(&path, &journaled.path(), 7, &mut valid).unwrap();
    let saved = std::fs::read(&path).unwrap();

    let mut ambiguous = [(stale, old), (current, fresh)];
    let before = ambiguous.clone();
    let result = remember_in(&path, &journaled.path(), 7, &mut ambiguous);
    assert_eq!(
        ambiguous[0].1, old,
        "the rejected stale reading must not inherit the current reading's fence"
    );
    assert!(result.is_err(), "duplicate subjects are not a snapshot");
    assert_eq!(ambiguous, before);
    assert_eq!(std::fs::read(&path).unwrap(), saved);
}

/// No launch, save or fence check runs while the real writer rotates. The index retains
/// enough evidence to keep the reading. Removing that evidence is the negative control:
/// the two remaining journal files cannot establish what happened to the original reading.
#[test]
fn a_reading_survives_many_rotations_with_no_intervening_check() {
    use aterm_converge::Journal as _;
    let mut journaled = Journaled::new();
    let original = journaled.fence();
    let _original_file = std::fs::File::open(journaled.path()).unwrap();
    let saved = record(7, &[(journaled.dock.clone(), original)], &journaled.path());
    let payload = "x".repeat(600_000);
    for _ in 0..12 {
        journaled
            .writer
            .before_change("sync.clone", &payload, "b", None)
            .unwrap();
    }
    assert_ne!(
        original.0.unwrap().inode,
        journaled.fence().0.unwrap().inode
    );
    assert_eq!(
        parse(&saved, 7, &journaled.path()),
        [(journaled.dock.clone(), journaled.fence())]
    );
    let index = journaled
        .path()
        .with_file_name(aterm_converge::FileJournal::HISTORY);
    let evidence = std::fs::read(&index).unwrap();
    assert!(
        evidence.len() < 1_000,
        "the index holds ids and sequences, not journal values"
    );
    std::fs::remove_file(&index).unwrap();
    assert!(
        parse(&saved, 7, &journaled.path()).is_empty(),
        "CONTROL: lost history proves nothing"
    );
    std::fs::write(&index, &evidence).unwrap();
    assert!(!parse(&saved, 7, &journaled.path()).is_empty());
    journaled.change("init.dock@alice");
    assert!(
        parse(&saved, 7, &journaled.path()).is_empty(),
        "a setting change survives rotation too"
    );
}

/// A reading before the first change, or before the first indexed writer on an upgraded
/// installation, needs an incarnation too. Preparing it again must write nothing.
/// Without preparation (the negative control), neither journal has a history stamp.
#[test]
fn an_initial_reading_has_history_before_any_indexed_change() {
    use aterm_converge::Journal as _;
    for legacy in [false, true] {
        let scratch = aterm_tempfile::tempdir().unwrap();
        let mut writer = aterm_converge::FileJournal::under(scratch.path());
        if legacy {
            std::fs::create_dir_all(writer.path().parent().unwrap()).unwrap();
            std::fs::write(
                writer.path(),
                b"{\"id\":\"sync.clone\",\"from\":\"a\",\"to\":\"b\"}\n",
            )
            .unwrap();
        }
        let selected = |id: &str| !aterm_machines::init::writes_no_private_row(id);
        assert!(aterm_converge::FileJournal::history_stamp_at(&writer.path(), selected).is_none());
        let dock = reading(Setting::Dock, Standing::InPlace, None);
        let old = Fence(JournalMark::of(&writer.path()).unwrap());
        let old_record = record(7, &[(dock.clone(), old)], &writer.path());
        writer.prepare_history().unwrap();
        assert_eq!(
            parse(&old_record, 7, &writer.path())[0].0,
            dock,
            "a metadata checkpoint preserves an existing readable fence"
        );
        let before = std::fs::read(writer.path()).unwrap();
        writer.prepare_history().unwrap();
        assert_eq!(
            std::fs::read(writer.path()).unwrap(),
            before,
            "preparation is idempotent"
        );
        let fence = Fence(JournalMark::of(&writer.path()).unwrap());
        assert!(fence.0.unwrap().history.is_some());
        let saved = record(7, &[(dock.clone(), fence)], &writer.path());
        for _ in 0..6 {
            writer
                .before_change("sync.clone", &"x".repeat(600_000), "b", None)
                .unwrap();
        }
        assert_eq!(parse(&saved, 7, &writer.path())[0].0, dock);
        writer
            .before_change("init.dock@alice", "a", "b", None)
            .unwrap();
        assert!(parse(&saved, 7, &writer.path()).is_empty());
    }
}
