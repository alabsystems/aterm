// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! `<state_dir>/converge/journal.ndjson`: one line per change, flushed to stable media before
//! applying.

use crate::engine::Journal;
use serde::Serialize;
use std::fs::{self, File, OpenOptions};
use std::io::{self, Write};
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

/// Bounded NDJSON change journal (`{"time", "id", "from", "to", "revert"}`) with single-rotation
/// locking.
#[derive(Debug, Clone)]
pub struct FileJournal {
    dir: PathBuf,
    bound: u64,
    wait: Duration,
}

#[derive(Serialize)]
struct Line<'a> {
    time: u64,
    id: &'a str,
    from: &'a str,
    to: &'a str,
    #[serde(skip_serializing_if = "Option::is_none")]
    revert: Option<&'a str>,
}

impl FileJournal {
    /// The folder under the state directory.
    pub const DIR: &'static str = "converge";
    /// The journal.
    pub const FILE: &'static str = "journal.ndjson";
    /// The one rotation kept.
    pub const ROTATED: &'static str = "journal.1.ndjson";
    /// The most bytes the journal holds before it rotates: 1 MiB.
    pub const BOUND: u64 = 1 << 20;
    /// The advisory lock each append holds, beside the journal.
    pub const LOCK: &'static str = "journal.lock";
    /// Maximum lock wait before refusing an append.
    pub const WAIT: Duration = Duration::from_secs(60);

    /// Returns a journal rooted at `<state_dir>/converge`.
    #[must_use]
    pub fn under(state_dir: &Path) -> Self {
        Self {
            dir: state_dir.join(Self::DIR),
            bound: Self::BOUND,
            wait: Self::WAIT,
        }
    }

    /// The journal file.
    #[must_use]
    pub fn path(&self) -> PathBuf {
        self.dir.join(Self::FILE)
    }

    /// The rotation kept beside it.
    #[must_use]
    pub fn rotated(&self) -> PathBuf {
        self.dir.join(Self::ROTATED)
    }

    /// [`FileJournal::under`] with a small bound, so a test rotates in a few lines.
    #[cfg(test)]
    fn bounded(state_dir: &Path, bound: u64) -> Self {
        Self {
            bound,
            ..Self::under(state_dir)
        }
    }

    fn append(&self, line: &[u8]) -> Result<(), String> {
        let len = u64::try_from(line.len()).unwrap_or(u64::MAX);
        if len > self.bound {
            return Err(format!(
                "a {len}-byte line is over the journal's {}-byte bound",
                self.bound
            ));
        }
        let path = self.path();
        let shown = |e: io::Error, at: &Path| format!("{}: {e}", at.display());
        make_dir(&self.dir).map_err(|e| shown(e, &self.dir))?;
        let lock_path = self.dir.join(Self::LOCK);
        let _held = Held::take(&lock_path, self.wait).map_err(|e| shown(e, &lock_path))?;
        let held = match fs::symlink_metadata(&path) {
            Ok(meta) if meta.is_file() => meta.len(),
            Ok(_) => return Err(format!("{}: not a regular file", path.display())),
            Err(e) if e.kind() == io::ErrorKind::NotFound => 0,
            Err(e) => return Err(shown(e, &path)),
        };
        if held > 0 && held.saturating_add(len) > self.bound {
            fs::rename(&path, self.rotated()).map_err(|e| {
                format!(
                    "{}: moving it to {}: {e}",
                    path.display(),
                    self.rotated().display()
                )
            })?;
        }
        let fresh = held == 0 || held.saturating_add(len) > self.bound;
        let mut file = open_append(&path).map_err(|e| shown(e, &path))?;
        file.write_all(line).map_err(|e| shown(e, &path))?;
        aterm_durable::durable_data(&file).map_err(|e| shown(e, &path))?;
        if fresh {
            aterm_durable::durable_dir(&self.dir).map_err(|e| shown(e, &self.dir))?;
        }
        Ok(())
    }
}

impl Journal for FileJournal {
    fn before_change(
        &mut self,
        id: &str,
        from: &str,
        to: &str,
        revert: Option<&str>,
    ) -> Result<(), String> {
        let time = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map_or(0, |d| d.as_secs());
        let mut line = aterm_json::to_string(&Line {
            time,
            id,
            from,
            to,
            revert,
        })
        .map_err(|e| e.to_string())?;
        line.push('\n');
        self.append(line.as_bytes())
    }
}

fn make_dir(dir: &Path) -> io::Result<()> {
    let mut builder = fs::DirBuilder::new();
    builder.recursive(true);
    #[cfg(unix)]
    std::os::unix::fs::DirBuilderExt::mode(&mut builder, 0o700);
    builder.create(dir)
}

struct Held(File);

impl Held {
    fn take(path: &Path, wait: Duration) -> io::Result<Self> {
        let mut options = OpenOptions::new();
        options.read(true).write(true).create(true).truncate(false);
        #[cfg(unix)]
        std::os::unix::fs::OpenOptionsExt::mode(&mut options, 0o600);
        let file: std::fs::File = options.open(path)?;
        let deadline = Instant::now() + wait;
        loop {
            #[expect(
                clippy::disallowed_methods,
                reason = "the guard's own take: its Drop releases by LOCK_UN"
            )]
            let taken = file.try_lock();
            match taken {
                Ok(()) => return Ok(Self(file)),
                Err(std::fs::TryLockError::WouldBlock) if Instant::now() < deadline => {
                    std::thread::sleep(Duration::from_millis(5));
                }
                Err(std::fs::TryLockError::WouldBlock) => {
                    return Err(io::Error::new(
                        io::ErrorKind::TimedOut,
                        format!(
                            "another run held the journal for more than {}s",
                            wait.as_secs()
                        ),
                    ));
                }
                Err(std::fs::TryLockError::Error(e)) => return Err(e),
            }
        }
    }
}

impl Drop for Held {
    fn drop(&mut self) {
        let _ = self.0.unlock();
    }
}

fn open_append(path: &Path) -> io::Result<File> {
    let mut options = OpenOptions::new();
    options.append(true).create(true);
    #[cfg(unix)]
    std::os::unix::fs::OpenOptionsExt::mode(&mut options, 0o600);
    options.open(path)
}

#[cfg(test)]
mod tests {
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
        ];
        let rows = run(&items, Mode::Apply, &mut journal).unwrap();
        assert_eq!(rows[0].outcome, Outcome::Changed);
        assert_eq!(rows[1].outcome, Outcome::Failed);
        let seen = seen.lock().unwrap();
        assert!(seen[0].contains("\"id\":\"base.shell.rc\""), "{}", seen[0]);
        assert!(seen[1].contains("\"id\":\"base.clt\""), "{}", seen[1]);
        let kept = lines(&journal.path());
        assert_eq!(kept.len(), 2, "one line per change, the failed one too");
        for (line, id) in kept.iter().zip(["base.shell.rc", "base.clt"]) {
            assert_eq!(line["id"].as_str(), Some(id));
            assert_eq!(line["from"].as_str(), Some("a"));
            assert_eq!(line["to"].as_str(), Some("b"));
            assert_eq!(line["revert"].as_str(), Some("undo"));
            assert!(line["time"].as_u64().is_some_and(|t| t > 0));
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
}
