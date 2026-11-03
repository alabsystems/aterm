// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! aterm's own side of the fabric, timed for the fabric ledger: the handler bodies the bridge's
//! verb lane and a client's `post` and `await inbox` reach ([`Row`]). A development seam
//! ([`aterm_types::wait_tally::HANDLER_SEAM`]) turns it on; a shipped binary compiles every call
//! here to nothing.
//!
//! aterm's handlers have no loop and so no idle moment for the line's write, and a write on a
//! measured path would be counted in what it measures. So the line is written by the one request
//! that cannot be measuring anything: an `await inbox … timeout=0` ([`write`]). Its reply proves
//! the file holds every span that ended before the request, which is the fence the bench needs,
//! with no polling.

pub use aterm_types::wait_tally::HandlerRow as Row;

#[cfg(any(test, debug_assertions, feature = "dev-seams"))]
pub use on::*;

#[cfg(not(any(test, debug_assertions, feature = "dev-seams")))]
pub use off::*;

#[cfg(any(test, debug_assertions, feature = "dev-seams"))]
mod on {
    use std::path::Path;
    use std::sync::{Mutex, OnceLock};
    use std::time::Instant;

    use aterm_types::wait_tally::{
        HANDLER_FILE, HANDLER_ROWS, HANDLER_SEAM, HandlerTally, Hist, Unit,
    };

    use super::Row;

    /// The moment a span began, or `None` while nothing is counted.
    pub type Stamp = Option<Instant>;

    /// The counts, and the private file they are written to.
    #[derive(Debug)]
    struct Recorder {
        out: std::fs::File,
        started: Instant,
        rows: Mutex<[Hist; HANDLER_ROWS]>,
    }

    impl Recorder {
        /// On, writing to a newly created private `path` inside a real directory.
        fn at(path: &Path) -> std::io::Result<Self> {
            if let Some(parent) = path.parent().filter(|d| !d.as_os_str().is_empty())
                && !std::fs::symlink_metadata(parent)?.file_type().is_dir()
            {
                return Err(std::io::Error::new(
                    std::io::ErrorKind::InvalidInput,
                    "handler tally parent path is not a directory",
                ));
            }
            let mut open = std::fs::OpenOptions::new();
            open.write(true).create_new(true);
            #[cfg(unix)]
            {
                use std::os::unix::fs::OpenOptionsExt as _;
                open.mode(0o600);
            }
            Ok(Self {
                out: open.open(path)?,
                started: Instant::now(),
                rows: Mutex::new([Hist::default(); HANDLER_ROWS]),
            })
        }

        fn note(&self, row: Row, started: Instant) {
            let ns = u64::try_from(started.elapsed().as_nanos()).unwrap_or(u64::MAX);
            let mut rows = self.rows.lock().unwrap_or_else(|e| e.into_inner());
            rows[row.index()].add(ns, Unit::Ns);
        }

        fn line(&self) -> String {
            let rows = *self.rows.lock().unwrap_or_else(|e| e.into_inner());
            HandlerTally {
                pid: std::process::id(),
                at_ns: u64::try_from(self.started.elapsed().as_nanos()).unwrap_or(u64::MAX),
                rows,
            }
            .render()
        }

        /// One whole line at offset 0: every line is one length ([`HandlerTally::render`]).
        fn write(&self) -> std::io::Result<()> {
            let line = self.line();
            #[cfg(unix)]
            {
                use std::os::unix::fs::FileExt as _;
                self.out.write_all_at(line.as_bytes(), 0)
            }
            #[cfg(not(unix))]
            {
                use std::io::{Seek as _, Write as _};
                let mut out = &self.out;
                out.seek(std::io::SeekFrom::Start(0))?;
                out.write_all(line.as_bytes())
            }
        }
    }

    /// The process's recorder, on when [`HANDLER_SEAM`] is set.
    fn recorder() -> Option<&'static Recorder> {
        static RECORDER: OnceLock<Option<Recorder>> = OnceLock::new();
        RECORDER
            .get_or_init(|| {
                aterm_types::dev_seam!(HANDLER_SEAM)?;
                let dir = aterm_types::dirs::state_dir()?;
                let path = dir.join(HANDLER_FILE);
                if std::fs::symlink_metadata(&dir).is_ok_and(|m| !m.file_type().is_dir()) {
                    eprintln!(
                        "aterm: {HANDLER_SEAM} is set, but {} is not a directory; no handler tally",
                        dir.display()
                    );
                    return None;
                }
                std::fs::create_dir_all(&dir)
                    .and_then(|()| Recorder::at(&path))
                    .map_err(|e| {
                        eprintln!(
                            "aterm: {HANDLER_SEAM} is set, but {} cannot be written ({e}); no \
                             handler tally",
                            path.display()
                        );
                    })
                    .ok()
            })
            .as_ref()
    }

    /// Now, when the tally is on.
    #[must_use]
    pub fn stamp() -> Stamp {
        recorder().map(|_| Instant::now())
    }

    /// The span that began at `stamp` has ended now.
    pub fn note_since(row: Row, stamp: Stamp) {
        if let (Some(recorder), Some(started)) = (recorder(), stamp) {
            recorder.note(row, started);
        }
    }

    /// A span of the row `row` names, counted when the timer drops.
    #[must_use]
    pub fn timer(row: impl FnOnce() -> Row) -> Timer {
        Timer(recorder().map(|recorder| (recorder, row(), Instant::now())))
    }

    /// One span [`timer`] counts when it drops.
    #[derive(Debug)]
    pub struct Timer(Option<(&'static Recorder, Row, Instant)>);

    impl Drop for Timer {
        fn drop(&mut self) {
            if let Some((recorder, row, started)) = self.0 {
                recorder.note(row, started);
            }
        }
    }

    /// One `await inbox`'s two spans ([`Row::AwaitArm`], [`Row::AwaitWake`]). One with no time to
    /// wait is the tally's read point, and counts nothing.
    #[derive(Debug)]
    pub struct AwaitSpans {
        recorder: Option<&'static Recorder>,
        began: Stamp,
        parked: bool,
    }

    impl AwaitSpans {
        /// An `await inbox` with `timeout_ms` to wait has begun. `timeout_ms` of 0 writes the line
        /// ([`write`]).
        #[must_use]
        pub fn begin(timeout_ms: u64) -> Self {
            Self::on(recorder(), timeout_ms)
        }

        /// [`AwaitSpans::begin`], counted by `recorder`: the process's, or a test's own.
        fn on(recorder: Option<&'static Recorder>, timeout_ms: u64) -> Self {
            if timeout_ms == 0 {
                write_to(recorder);
                return Self {
                    recorder: None,
                    began: None,
                    parked: false,
                };
            }
            Self {
                recorder,
                began: recorder.map(|_| Instant::now()),
                parked: false,
            }
        }

        fn note(&self, row: Row, since: Stamp) {
            if let (Some(recorder), Some(started)) = (self.recorder, since) {
                recorder.note(row, started);
            }
        }

        /// The wait parks now.
        pub fn parks(&mut self) {
            if !self.parked {
                self.parked = true;
                self.note(Row::AwaitArm, self.began);
            }
        }

        /// A parked wait answers now with a row that arrived at `arrived` ([`stamp`]); that
        /// arrival is spent, so a later wait on another path's row reads no stale moment.
        pub fn answers(&self, arrived: &mut Stamp) {
            let arrived = arrived.take();
            if self.parked && self.began.is_some() {
                self.note(Row::AwaitWake, arrived);
            }
        }
    }

    impl Drop for AwaitSpans {
        fn drop(&mut self) {
            if !self.parked {
                self.note(Row::AwaitArm, self.began);
            }
        }
    }

    /// Write the line, when the tally is on.
    pub fn write() {
        write_to(recorder());
    }

    fn write_to(recorder: Option<&Recorder>) {
        if let Some(recorder) = recorder
            && let Err(e) = recorder.write()
        {
            eprintln!("aterm: the handler tally could not be written ({e})");
        }
    }

    #[cfg(test)]
    mod tests {
        use super::*;

        #[test]
        fn a_written_line_reads_back_with_what_was_counted() {
            let dir = aterm_tempfile::tempdir().expect("scratch");
            let path = dir.path().join(HANDLER_FILE);
            let recorder = Recorder::at(&path).expect("a new file");
            recorder.note(Row::Post, Instant::now());
            recorder.note(Row::Post, Instant::now());
            recorder.write().expect("written");
            let first = HandlerTally::parse(&std::fs::read_to_string(&path).expect("read"))
                .expect("parses");
            assert_eq!(first.pid, std::process::id());
            assert_eq!(first.row(Row::Post).count, 2);
            assert_eq!(first.row(Row::Deliver).count, 0);
            recorder.note(Row::Deliver, Instant::now());
            recorder.write().expect("written again");
            let second = HandlerTally::parse(&std::fs::read_to_string(&path).expect("read"))
                .expect("a later line replaces the first whole");
            let d = second.since(&first).expect("one process");
            assert_eq!(d.row(Row::Deliver).count, 1);
            assert_eq!(d.row(Row::Post).count, 0);
        }

        #[test]
        fn a_second_recorder_refuses_an_occupied_name() {
            let dir = aterm_tempfile::tempdir().expect("scratch");
            let path = dir.path().join(HANDLER_FILE);
            std::fs::write(&path, "another process's\n").expect("occupied");
            assert!(Recorder::at(&path).is_err());
            assert_eq!(
                std::fs::read_to_string(&path).expect("read"),
                "another process's\n"
            );
        }

        /// A recorder held for the process's life, as the real one is, and its file.
        fn on_file() -> (
            aterm_tempfile::TempDir,
            std::path::PathBuf,
            &'static Recorder,
        ) {
            let dir = aterm_tempfile::tempdir().expect("scratch");
            let path = dir.path().join(HANDLER_FILE);
            let recorder = Recorder::at(&path).expect("a new file");
            (dir, path, Box::leak(Box::new(recorder)))
        }

        /// Every row that counted anything, with its count.
        fn counted(recorder: &Recorder) -> Vec<(Row, u64)> {
            let rows = *recorder.rows.lock().expect("not poisoned");
            Row::ALL
                .into_iter()
                .map(|row| (row, rows[row.index()].count))
                .filter(|(_, count)| *count > 0)
                .collect()
        }

        #[test]
        fn a_wait_with_no_time_writes_every_earlier_span_and_counts_in_no_row() {
            let (_dir, path, recorder) = on_file();
            recorder.note(Row::Post, Instant::now());
            let mut spans = AwaitSpans::on(Some(recorder), 0);
            let line = HandlerTally::parse(&std::fs::read_to_string(&path).expect("the fence"))
                .expect("a whole line");
            assert_eq!(line.row(Row::Post).count, 1, "written before the answer");
            spans.parks();
            spans.answers(&mut Some(Instant::now()));
            drop(spans);
            assert_eq!(counted(recorder), [(Row::Post, 1)]);
        }

        #[test]
        fn a_parked_wait_counts_its_arm_once_and_its_answer_as_the_wake() {
            let (_dir, _path, recorder) = on_file();
            let mut spans = AwaitSpans::on(Some(recorder), 60_000);
            assert_eq!(counted(recorder), []);
            spans.parks();
            spans.parks();
            assert_eq!(
                counted(recorder),
                [(Row::AwaitArm, 1)],
                "a park again is one"
            );
            let mut arrived = Some(Instant::now());
            spans.answers(&mut arrived);
            assert_eq!(arrived, None, "the arrival is spent");
            drop(spans);
            assert_eq!(
                counted(recorder),
                [(Row::AwaitArm, 1), (Row::AwaitWake, 1)],
                "and the drop counts no second arm"
            );
        }

        #[test]
        fn a_wait_that_never_parked_spends_the_arrival_and_counts_only_its_arm() {
            let (_dir, _path, recorder) = on_file();
            let mut arrived = Some(Instant::now());
            let spans = AwaitSpans::on(Some(recorder), 60_000);
            spans.answers(&mut arrived);
            assert_eq!(
                arrived, None,
                "or a later wait would read this row's moment"
            );
            assert_eq!(counted(recorder), [], "its arm ends when it does");
            drop(spans);
            assert_eq!(counted(recorder), [(Row::AwaitArm, 1)]);

            let mut later = AwaitSpans::on(Some(recorder), 60_000);
            later.parks();
            later.answers(&mut arrived);
            assert_eq!(
                counted(recorder),
                [(Row::AwaitArm, 2)],
                "an answer with no arrival of its own has no wake to count"
            );
        }

        #[test]
        fn with_the_seam_unset_nothing_is_counted() {
            assert!(stamp().is_none());
            let _timer = timer(|| panic!("the row is named only when the tally is on"));
            let mut spans = AwaitSpans::begin(1);
            spans.parks();
            spans.answers(&mut stamp());
            write();
        }

        /// NEGATIVE CONTROL: `Recorder::at` refuses a symlinked parent directory without creating
        /// the tally file in the link target.
        #[cfg(unix)]
        #[test]
        fn recorder_refuses_symlinked_parent_directory() {
            let root = aterm_tempfile::tempdir().expect("scratch");
            let real_dir = root.path().join("real");
            std::fs::create_dir(&real_dir).expect("real dir");
            let linked_dir = root.path().join("linked");
            std::os::unix::fs::symlink(&real_dir, &linked_dir).expect("symlink dir");

            let err = Recorder::at(&linked_dir.join(HANDLER_FILE))
                .expect_err("symlinked parent must be refused");
            assert_eq!(err.kind(), std::io::ErrorKind::InvalidInput);
            assert!(!real_dir.join(HANDLER_FILE).exists());
        }
    }
}

#[cfg(not(any(test, debug_assertions, feature = "dev-seams")))]
mod off {
    use super::Row;

    /// Nothing: a shipped binary times nothing.
    #[derive(Debug, Default, Clone, Copy)]
    pub struct Stamp;

    /// Nothing.
    #[inline(always)]
    #[must_use]
    pub const fn stamp() -> Stamp {
        Stamp
    }

    /// Nothing.
    #[inline(always)]
    pub const fn note_since(_row: Row, _stamp: Stamp) {}

    /// Nothing, and `_row` is never asked.
    #[inline(always)]
    #[must_use]
    pub fn timer(_row: impl FnOnce() -> Row) -> Timer {
        Timer
    }

    /// Nothing.
    #[derive(Debug)]
    pub struct Timer;

    /// Nothing.
    #[derive(Debug)]
    pub struct AwaitSpans;

    impl AwaitSpans {
        /// Nothing.
        #[inline(always)]
        #[must_use]
        pub const fn begin(_timeout_ms: u64) -> Self {
            Self
        }

        /// Nothing.
        #[inline(always)]
        pub const fn parks(&mut self) {}

        /// Nothing.
        #[inline(always)]
        pub const fn answers(&self, _arrived: &mut Stamp) {}
    }

    /// Nothing.
    #[inline(always)]
    pub const fn write() {}
}
