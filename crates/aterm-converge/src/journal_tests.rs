// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

use super::*;
use crate::engine::tests::{Scripted, change, ok};
use crate::{Item, Mode, Outcome, Probe, run};
use std::sync::{Arc, Mutex};

fn lines(path: &Path) -> Vec<aterm_json::Value> {
    match fs::read_to_string(path) {
        Ok(text) => text
            .lines()
            .map(|l| aterm_json::from_str(l).unwrap())
            .collect(),
        Err(e) if e.kind() == io::ErrorKind::NotFound => Vec::new(),
        Err(e) => panic!("{}: {e}", path.display()),
    }
}

struct Peeks {
    inner: Scripted,
    journal: PathBuf,
    seen: Arc<Mutex<Vec<String>>>,
}

impl Item for Peeks {
    fn id(&self) -> String {
        self.inner.id()
    }
    fn family(&self) -> &'static str {
        self.inner.family()
    }
    fn layer(&self) -> crate::Layer {
        self.inner.layer()
    }
    fn class(&self) -> crate::Class {
        self.inner.class()
    }
    fn title(&self) -> String {
        self.inner.title()
    }
    fn probe(&self) -> Probe {
        self.inner.probe()
    }
    fn apply(&self) -> Result<(), String> {
        let text = fs::read_to_string(&self.journal).unwrap_or_default();
        self.seen.lock().unwrap().push(text);
        self.inner.apply()
    }
    fn explain(&self, probe: &Probe) -> crate::Explain {
        self.inner.explain(probe)
    }
    fn revert(&self) -> Option<String> {
        self.inner.revert()
    }
}

/// Each change's line is on disk before its apply runs, and an item the run ended failed (an
/// apply that failed, or one whose read-back still reads the old value) gets a line naming
/// why after its change's line.
/// NEGATIVE CONTROL: an engine that records no failure leaves the journal without the
/// `failed` lines.
#[test]
fn the_line_is_on_disk_before_the_apply_runs_and_a_failed_apply_keeps_it() {
    let scratch = aterm_tempfile::tempdir().unwrap();
    let mut journal = FileJournal::under(scratch.path());
    let seen = Arc::new(Mutex::new(Vec::new()));
    let items: Vec<Box<dyn Item>> = vec![
        Box::new(Peeks {
            inner: Scripted {
                id: "base.shell.rc",
                ..Scripted::new(vec![change(), ok()], Ok(()))
            },
            journal: journal.path(),
            seen: Arc::clone(&seen),
        }),
        Box::new(Peeks {
            inner: Scripted {
                id: "base.clt",
                ..Scripted::new(vec![change()], Err("softwareupdate said no".into()))
            },
            journal: journal.path(),
            seen: Arc::clone(&seen),
        }),
        Box::new(Scripted {
            id: "sync.fabric",
            ..Scripted::new(vec![change(), change()], Ok(()))
        }),
    ];
    let rows = run(&items, Mode::Apply, &mut journal).unwrap();
    assert_eq!(rows[0].outcome, Outcome::Changed);
    assert_eq!(rows[1].outcome, Outcome::Failed);
    assert_eq!(rows[2].outcome, Outcome::Failed);
    let seen = seen.lock().unwrap();
    assert!(seen[0].contains("\"id\":\"base.shell.rc\""), "{}", seen[0]);
    assert!(seen[1].contains("\"id\":\"base.clt\""), "{}", seen[1]);
    let kept = lines(&journal.path());
    let ids: Vec<&str> = kept.iter().filter_map(|line| line["id"].as_str()).collect();
    assert_eq!(
        ids,
        [
            "base.shell.rc",
            "base.clt",
            "base.clt",
            "sync.fabric",
            "sync.fabric"
        ],
        "one line per change, and one per failure after it: {kept:?}"
    );
    for line in [&kept[0], &kept[1], &kept[3]] {
        assert_eq!(line["from"].as_str(), Some("a"));
        assert_eq!(line["to"].as_str(), Some("b"));
        assert_eq!(line["revert"].as_str(), Some("undo"));
        assert!(line["failed"].is_null(), "{line:?}");
    }
    assert_eq!(kept[2]["failed"].as_str(), Some("softwareupdate said no"));
    assert_eq!(
        kept[4]["failed"].as_str(),
        rows[2].why.as_deref(),
        "a read-back that still reads the old value: {kept:?}"
    );
    for line in &kept {
        assert!(line["time"].as_u64().is_some_and(|t| t > 0), "{line:?}");
    }
    for line in [&kept[2], &kept[4]] {
        assert!(line["from"].is_null() && line["to"].is_null(), "{line:?}");
    }
}

#[test]
fn an_item_already_as_wanted_and_a_plan_write_nothing() {
    let scratch = aterm_tempfile::tempdir().unwrap();
    let mut journal = FileJournal::under(scratch.path());
    let items: Vec<Box<dyn Item>> = vec![Box::new(Scripted::new(vec![ok()], Ok(())))];
    run(&items, Mode::Apply, &mut journal).unwrap();
    let planned: Vec<Box<dyn Item>> = vec![Box::new(Scripted::new(vec![change()], Ok(())))];
    run(&planned, Mode::Plan, &mut journal).unwrap();
    assert!(!scratch.path().join(FileJournal::DIR).exists());
}

#[test]
fn the_journal_rotates_within_its_bound_and_keeps_one_rotation() {
    let scratch = aterm_tempfile::tempdir().unwrap();
    let bound = 400;
    let mut journal = FileJournal::bounded(scratch.path(), bound);
    for n in 0..40 {
        journal
            .before_change(&format!("item.{n}"), "from", "to", None)
            .unwrap();
        for path in [journal.path(), journal.rotated()] {
            let len = fs::metadata(&path).map_or(0, |m| m.len());
            assert!(len <= bound, "{} holds {len} bytes", path.display());
        }
    }
    let names: Vec<String> = fs::read_dir(scratch.path().join(FileJournal::DIR))
        .unwrap()
        .map(|e| e.unwrap().file_name().to_string_lossy().into_owned())
        .collect();
    let mut sorted = names.clone();
    sorted.sort();
    assert_eq!(
        sorted,
        [FileJournal::ROTATED, FileJournal::LOCK, FileJournal::FILE]
    );
    let current = lines(&journal.path());
    assert_eq!(current.last().unwrap()["id"].as_str(), Some("item.39"));
    let older = lines(&journal.rotated());
    let first_kept = current[0]["id"].as_str().unwrap().to_owned();
    let last_rotated = older.last().unwrap()["id"].as_str().unwrap();
    let number = |id: &str| id.trim_start_matches("item.").parse::<u32>().unwrap();
    assert_eq!(number(last_rotated) + 1, number(&first_kept));
}

#[test]
fn a_line_over_the_bound_is_refused_and_its_change_not_made() {
    let scratch = aterm_tempfile::tempdir().unwrap();
    let mut journal = FileJournal::bounded(scratch.path(), 64);
    let item = Scripted {
        id: "a.long.enough.id.to.carry.the.line.past.sixty.four.bytes",
        ..Scripted::new(vec![change(), ok()], Ok(()))
    };
    let applies = Arc::clone(&item.applies);
    let items: Vec<Box<dyn Item>> = vec![Box::new(item)];
    let rows = run(&items, Mode::Apply, &mut journal).unwrap();
    assert_eq!(rows[0].outcome, Outcome::Failed);
    assert!(rows[0].why.as_deref().unwrap().contains("bound"));
    assert_eq!(applies.load(std::sync::atomic::Ordering::SeqCst), 0);
}

/// Non-regular journal/lock/rotated paths and symlinked journal or state directories refuse
/// every record before any item applies.
/// NEGATIVE CONTROL: following a symlinked `state_dir`, `converge` folder, lock, or rotated
/// file would create or overwrite targets outside `<state_dir>/converge`.
#[test]
fn a_journal_that_is_not_a_regular_file_refuses_and_nothing_is_applied() {
    let scratch = aterm_tempfile::tempdir().unwrap();
    let mut journal = FileJournal::under(scratch.path());
    fs::create_dir_all(journal.path()).unwrap();
    let item = Scripted::new(vec![change(), ok()], Ok(()));
    let applies = Arc::clone(&item.applies);
    let items: Vec<Box<dyn Item>> = vec![Box::new(item)];
    let rows = run(&items, Mode::Apply, &mut journal).unwrap();
    assert_eq!(rows[0].outcome, Outcome::Failed);
    assert!(
        rows[0]
            .why
            .as_deref()
            .unwrap()
            .contains("not a regular file")
    );
    assert_eq!(applies.load(std::sync::atomic::Ordering::SeqCst), 0);

    #[cfg(unix)]
    {
        let lock_scratch = aterm_tempfile::tempdir().unwrap();
        let mut lock_journal = FileJournal::under(lock_scratch.path());
        let lock_dir = lock_scratch.path().join(FileJournal::DIR);
        fs::create_dir_all(&lock_dir).unwrap();
        let decoy = lock_scratch.path().join("decoy");
        std::os::unix::fs::symlink(&decoy, lock_dir.join(FileJournal::LOCK)).unwrap();
        let item = Scripted::new(vec![change(), ok()], Ok(()));
        let applies = Arc::clone(&item.applies);
        let items: Vec<Box<dyn Item>> = vec![Box::new(item)];
        let rows = run(&items, Mode::Apply, &mut lock_journal).unwrap();
        assert_eq!(rows[0].outcome, Outcome::Failed);
        assert!(
            rows[0]
                .why
                .as_deref()
                .unwrap()
                .contains("not a regular file"),
            "{rows:?}"
        );
        assert_eq!(applies.load(std::sync::atomic::Ordering::SeqCst), 0);
        assert!(
            !decoy.exists(),
            "symlinked lock target must never be created"
        );

        let dir_scratch = aterm_tempfile::tempdir().unwrap();
        let dir_target = dir_scratch.path().join("elsewhere");
        fs::create_dir_all(&dir_target).unwrap();
        std::os::unix::fs::symlink(&dir_target, dir_scratch.path().join(FileJournal::DIR)).unwrap();
        let mut dir_journal = FileJournal::under(dir_scratch.path());
        let err = dir_journal
            .before_change("item.0", "a", "b", None)
            .unwrap_err();
        assert!(err.contains("not a directory"), "{err}");

        let state_scratch = aterm_tempfile::tempdir().unwrap();
        let state_target = state_scratch.path().join("state-elsewhere");
        fs::create_dir_all(&state_target).unwrap();
        let state_link = state_scratch.path().join("state-link");
        std::os::unix::fs::symlink(&state_target, &state_link).unwrap();
        let mut state_journal = FileJournal::under(&state_link);
        let err = state_journal
            .before_change("item.0", "a", "b", None)
            .unwrap_err();
        assert!(err.contains("not a directory"), "{err}");
        assert!(
            !state_target.join(FileJournal::DIR).exists(),
            "symlinked state_dir must never have converge created inside its target"
        );

        use std::os::unix::fs::PermissionsExt as _;
        let mode_scratch = aterm_tempfile::tempdir().unwrap();
        let mode_dir = mode_scratch.path().join(FileJournal::DIR);
        fs::create_dir_all(&mode_dir).unwrap();
        fs::set_permissions(&mode_dir, fs::Permissions::from_mode(0o755)).unwrap();
        let mode_journal_file = mode_dir.join(FileJournal::FILE);
        let mode_lock_file = mode_dir.join(FileJournal::LOCK);
        fs::write(&mode_journal_file, b"").unwrap();
        fs::write(&mode_lock_file, b"").unwrap();
        fs::set_permissions(&mode_journal_file, fs::Permissions::from_mode(0o644)).unwrap();
        fs::set_permissions(&mode_lock_file, fs::Permissions::from_mode(0o644)).unwrap();
        let mut mode_journal = FileJournal::under(mode_scratch.path());
        mode_journal
            .before_change("item.0", "a", "b", None)
            .unwrap();
        assert_eq!(
            fs::symlink_metadata(&mode_dir)
                .unwrap()
                .permissions()
                .mode()
                & 0o777,
            0o700
        );
        for f in [&mode_journal_file, &mode_lock_file] {
            assert_eq!(
                fs::symlink_metadata(f).unwrap().permissions().mode() & 0o777,
                0o600,
                "{}",
                f.display()
            );
        }

        let rot_scratch = aterm_tempfile::tempdir().unwrap();
        let mut rot_journal = FileJournal::bounded(rot_scratch.path(), 128);
        rot_journal.before_change("item.0", "a", "b", None).unwrap();
        let before_lines = lines(&rot_journal.path());
        assert_eq!(before_lines.len(), 1);
        let target_outside = rot_scratch.path().join("outside");
        fs::write(&target_outside, b"keep").unwrap();
        std::os::unix::fs::symlink(&target_outside, rot_journal.rotated()).unwrap();
        let item = Scripted {
            id: "item.1.with.enough.text.to.force.rotation",
            ..Scripted::new(vec![change(), ok()], Ok(()))
        };
        let applies = Arc::clone(&item.applies);
        let items: Vec<Box<dyn Item>> = vec![Box::new(item)];
        let rows = run(&items, Mode::Apply, &mut rot_journal).unwrap();
        assert_eq!(rows[0].outcome, Outcome::Failed);
        assert!(
            rows[0]
                .why
                .as_deref()
                .unwrap()
                .contains("not a regular file"),
            "{rows:?}"
        );
        assert_eq!(applies.load(std::sync::atomic::Ordering::SeqCst), 0);
        assert!(
            fs::symlink_metadata(rot_journal.rotated())
                .unwrap()
                .file_type()
                .is_symlink(),
            "rotation must refuse a planted symlink without replacing it"
        );
        assert_eq!(lines(&rot_journal.path()), before_lines);
    }
}

#[test]
fn each_line_is_flushed_durable_before_its_change_and_a_new_entry_with_its_folder() {
    use aterm_durable::{Flush, seam::Recording};
    let scratch = aterm_tempfile::tempdir().unwrap();
    let bound = 200;
    let mut journal = FileJournal::bounded(scratch.path(), bound);
    let recording = Recording::start();
    journal.before_change("item.0", "from", "to", None).unwrap();
    assert_eq!(recording.take(), [Flush::Durable, Flush::Durable]);
    journal.before_change("item.1", "from", "to", None).unwrap();
    assert_eq!(recording.take(), [Flush::Durable]);
    let mut flushes = Vec::new();
    while fs::metadata(journal.rotated()).is_err() {
        journal.before_change("item.n", "from", "to", None).unwrap();
        flushes = recording.take();
    }
    assert_eq!(flushes, [Flush::Durable, Flush::Durable]);
}

#[test]
fn a_line_waits_for_another_writers_lock_and_a_hold_past_the_wait_refuses_its_change() {
    let scratch = aterm_tempfile::tempdir().unwrap();
    let mut journal = FileJournal {
        wait: Duration::ZERO,
        ..FileJournal::under(scratch.path())
    };
    journal.before_change("item.0", "a", "b", None).unwrap();
    let other = Held::take(
        &scratch
            .path()
            .join(FileJournal::DIR)
            .join(FileJournal::LOCK),
        Duration::ZERO,
    )
    .unwrap();
    let item = Scripted::new(vec![change(), ok()], Ok(()));
    let applies = Arc::clone(&item.applies);
    let items: Vec<Box<dyn Item>> = vec![Box::new(item)];
    let rows = run(&items, Mode::Apply, &mut journal).unwrap();
    assert_eq!(rows[0].outcome, Outcome::Failed);
    let why = rows[0].why.as_deref().unwrap();
    assert!(why.contains("another run held the journal"), "{why}");
    assert_eq!(applies.load(std::sync::atomic::Ordering::SeqCst), 0);
    assert_eq!(lines(&journal.path()).len(), 1);
    drop(other);
    journal.before_change("item.1", "a", "b", None).unwrap();
    assert_eq!(lines(&journal.path()).len(), 2);
}

#[test]
fn writers_that_cross_the_bound_together_rotate_once_and_none_is_refused() {
    let scratch = aterm_tempfile::tempdir().unwrap();
    let bound: u64 = 400;
    let writers: Vec<_> = (0..4)
        .map(|writer| {
            let dir = scratch.path().to_path_buf();
            std::thread::spawn(move || {
                let mut journal = FileJournal::bounded(&dir, bound);
                let mut rotations = Vec::new();
                for n in 0..64 {
                    let id = format!("writer.{writer}.item.{n:02}");
                    journal
                        .before_change(&id, "from", "to", None)
                        .unwrap_or_else(|why| panic!("{id} refused: {why}"));
                    if let Ok(meta) = fs::metadata(journal.rotated()) {
                        rotations.push(meta.len());
                    }
                }
                rotations
            })
        })
        .collect();
    let rotations: Vec<u64> = writers
        .into_iter()
        .flat_map(|w| w.join().unwrap())
        .collect();
    let dir = scratch.path().join(FileJournal::DIR);
    let text = fs::read_to_string(dir.join(FileJournal::FILE)).unwrap()
        + &fs::read_to_string(dir.join(FileJournal::ROTATED)).unwrap();
    let mut lengths: Vec<u64> = text
        .lines()
        .map(|l| u64::try_from(l.len()).unwrap() + 1)
        .collect();
    lengths.dedup();
    let [line] = lengths[..] else {
        panic!("lines of more than one length, or torn: {text}");
    };
    assert!(!rotations.is_empty(), "the run never rotated");
    for len in rotations {
        assert!(
            len + line > bound,
            "a {len}-byte rotation of {line}-byte lines: a later rename moved a fresh \
             journal over the one rotated"
        );
    }
    for name in [FileJournal::FILE, FileJournal::ROTATED] {
        assert!(!lines(&dir.join(name)).is_empty(), "{name}");
    }
}
