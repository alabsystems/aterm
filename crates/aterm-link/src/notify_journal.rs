// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! THE NOTIFY JOURNAL: one append-only file per notify store, [`FILE`], that holds the verdicts
//! (`firing`, `fired`, `dropped`), the rate stamps a page spends, the suppressed count a drop
//! bumps, the standing values and each selector's cursor, in place of the line files a build
//! before it rewrote whole at every change (`journal`, `rate`, `suppressed`, `standing`, and a
//! `cur/<key>` file a selector).
//!
//! A change of the standing values is one step, `stand <subject> <value>` or `stand <subject>`
//! (cleared), each word percent-encoded so any subject and value is one, appended to the log's
//! store's file with its selector's cursor at the changing record in the same step, `cursor
//! <key> <off>` ([`super::Notifier::put_standing`]): a step reads whole or not at all, so no
//! standing value reaches the media ahead of the cursor it orders, by construction, and a
//! flood of `need` drops, each owing its pair on its own face, writes each change's one step
//! where the line files rewrote every value standing and replaced the cursor's file ahead of
//! it. A checkpoint that changes nothing else is a cursor's step of its own
//! ([`super::Notifier::put_cursor`]). A read takes the newest cursor per key, and a rewrite
//! writes each key's newest, so the cursors count toward the [`Bound`] as what they rewrite to.
//!
//! A verdict costs one step appended to a held descriptor and one flush of it: a page's `firing`
//! step carries the stamp it spends and is flushed in full before the page's command runs, its
//! `fired` step carries the suppressed count it clears and is ordered; a `dropped` step carries
//! the count it bumps. So a flood of drops past `--rate` writes each drop's own step, about a
//! hundred bytes, where the whole-file rewrite wrote the whole journal (up to [`JOURNAL_KEEP`]
//! lines) and the count per drop. The file is rewritten from what it holds ([`Verdicts::encode`])
//! once it has more than twice the steps that rewrite writes, never below 2 x [`JOURNAL_KEEP`]
//! steps, and more than twice its bytes ([`Bound`]), so a rewrite's steps and bytes are spread
//! over at least as many appended; the bound is what the file holds would rewrite to, at an
//! open as after a rewrite, so no restart carries a longer file's bound forward. A rewrite
//! carries the stamps the notifier holds (in order, the window's), never ones it let go.
//!
//! ONE WRITER: a notifier holds its root store, and every log's store under it, by an exclusive
//! lock for its life ([`super::Store::open`]), and a second one on the same state dir is refused
//! at its start, so no other writer appends to or renames over a file a notifier holds. An
//! append goes to the descriptor it opened once ([`StepsFile::target`]), on a file that is an
//! owned regular single-link file of the bytes it read or wrote ([`StepsFile::open`]); anything
//! else there (a file changed by hand, a planted link) is rewritten whole from what it holds.
//!
//! The steps are the post journal's ([`crate::state::journal`]: an [`astream_wire::steps`]
//! record a fact, a body of ASCII text, read to its first damage), so a cut append reads as a
//! torn tail, whose whole steps are the journal and which the next write rewrites away, and
//! damage before an intact step reads as a fault: its bytes are kept beside the file
//! ([`FAULT`]) and every whole step before and after it is read ([`journal::salvage_as`]).
//!
//! WHERE A STEP GOES: the stamps and the suppressed count are the notifier's, whatever log it
//! reads, and live in the root store's file; the verdicts are a log's and live in that log's
//! store's file (the root's, or `logs/<id>/`'s, [`super::Store::for_log`]). Reading the root's
//! log, both are one file and a verdict and what it spends or counts are one step. Reading a log
//! of its own, the verdict is appended to that log's file first and what it spends or counts to
//! the root's after it, and [`super::Notifier::restore`] finishes what a kill cut between them.
//!
//! THE LINE FILES ARE MIGRATED ONCE ([`super::Store::load`]): a store with no [`FILE`] and any
//! of the line files has them read, written as one [`FILE`], and only then renamed aside
//! (`<name>.migrated`); nothing reads them after. A [`STANDING`] file and a cursor's file
//! (`cur/<key>`, [`CURSORS`]) are migrated wherever one is found, beside a [`FILE`] too (a
//! build that kept the verdicts here and the standing values or the cursors there wrote it),
//! since only a build before their steps writes one: it is the newest. A cursor's file moves
//! its key's cursor forward only, never back, since a cursor only moves forward: one a power
//! loss revives after its rename aside never sets a newer step's cursor back.

use std::collections::{BTreeMap, VecDeque};
use std::io::{self, Write as _};
use std::path::Path;

use aterm_durable::Flush;

use super::{flush_dir, note_bytes, note_flush, warn, Store, JOURNAL_KEEP};
use crate::state::journal::{self, End, Record, Replay};

fn number(word: &str) -> Option<u64> {
    crate::parse_uint(word)
}

/// The journal in a notify store's root.
pub const FILE: &str = "journal.steps";

/// Where a load keeps the bytes of a journal that does not read whole, beside it.
pub(super) const FAULT: &str = "journal.steps.fault";

/// The line files a build before the journal wrote, migrated once ([`Store::load`]).
pub(super) const LINE_FILES: [&str; 3] = ["journal", "rate", "suppressed"];

/// The line file of the standing values a build before them in the journal wrote, migrated
/// wherever there is one ([`Store::load`]).
pub(super) const STANDING: &str = "standing";

/// The directory of the cursor files a build before the cursors were steps wrote, one
/// `cur/<key>` a selector, migrated wherever there is one ([`Store::load`]).
pub(super) const CURSORS: &str = "cur";

/// What a migrated line file is renamed to, after its name.
pub(super) const SET_ASIDE: &str = ".migrated";

/// The most a journal may hold for a load to read it.
const READ_MAX: usize = 64 << 20;

/// The stamps one step of a rewrite carries.
const STAMPS_PER_STEP: usize = 1024;

/// A change of one subject's standing value ([`Verdicts::standing`]): its subject, and the
/// value it now stands at (made its newest), or none (cleared).
pub(super) type Stand = (String, Option<String>);

/// One verdict and what it spends or counts, one change of the standing values with the
/// cursor it orders, a cursor, or what a rewrite carries of the stamps, the count, the standing
/// values and the cursors. At least one of the five.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub(super) struct Step {
    /// A verdict line, its first word the record's offset: `<off> firing|fired|dropped …`.
    pub(super) verdict: Option<String>,
    /// Rate stamps spent.
    pub(super) spent: Vec<u64>,
    /// The suppressed count and the `t=` of the newest drop it counts.
    pub(super) suppressed: Option<(u64, u64)>,
    /// Changes of the standing values, applied in order: `stand <subject> <value>` sets one
    /// (and makes it the newest), `stand <subject>` clears it.
    pub(super) stand: Vec<Stand>,
    /// Selectors' cursors, each `cursor <key> <off>`: the key's cursor is now `off`.
    pub(super) cursor: Vec<(String, u64)>,
}

impl Step {
    /// Whether it says nothing (no record is written for it).
    pub(super) fn is_empty(&self) -> bool {
        self.verdict.is_none()
            && self.spent.is_empty()
            && self.suppressed.is_none()
            && self.stand.is_empty()
            && self.cursor.is_empty()
    }

    fn parse(body: &[u8]) -> Option<Self> {
        let text = std::str::from_utf8(body).ok()?;
        let mut step = Self::default();
        let mut rank = 0u8;
        for line in text.split('\n') {
            let (head, rest) = line.split_once(' ')?;
            let at = match head {
                "verdict" if is_line(rest) => {
                    step.verdict = Some(rest.to_string());
                    1
                }
                "spent" => {
                    step.spent = rest.split(',').map(number).collect::<Option<_>>()?;
                    2
                }
                "suppressed" => {
                    let (n, mark) = rest.split_once(' ')?;
                    step.suppressed = Some((number(n)?, number(mark)?));
                    3
                }
                "stand" => {
                    let (subject, value) = match rest.split_once(' ') {
                        Some((subject, value)) => (subject, Some(value)),
                        None => (rest, None),
                    };
                    let value = match value {
                        Some(value) => Some(unword(value)?),
                        None => None,
                    };
                    step.stand.push((unword(subject)?, value));
                    4
                }
                "cursor" => {
                    let (key, off) = rest.split_once(' ')?;
                    step.cursor.push((unword(key)?, number(off)?));
                    5
                }
                _ => return None,
            };
            if at < rank || (at == rank && at < 4) {
                return None;
            }
            rank = at;
        }
        (!step.is_empty()).then_some(step)
    }
}

/// Whether `line` can be a verdict's body: printable ASCII or spaces, and not empty.
fn is_line(line: &str) -> bool {
    !line.is_empty() && line.bytes().all(|b| b == b' ' || b.is_ascii_graphic())
}

/// A standing value's subject or value as one word of a `stand` line: percent-encoded
/// ([`crate::pct::encode`]: every byte that is not ASCII-graphic, and `%`), so any subject
/// and value the notifier holds is a step, and the body stays ASCII text (the record codec's
/// rule, [`journal::Record`]). Empty, it is no word, and the step does not read back.
fn word(text: &str) -> String {
    crate::pct::encode(text)
}

/// [`word`]'s inverse: a word of ASCII-graphic bytes, not empty, decoded; None for any other.
fn unword(word: &str) -> Option<String> {
    (!word.is_empty() && word.bytes().all(|b| b.is_ascii_graphic()))
        .then(|| crate::pct::decode(word))
}

/// The standing values a line-format `standing` file says, as the build that wrote it read
/// them: `<subject> <value>` a line, split at its first space, neither empty and no space in
/// the value, the oldest first; a line that is not that is skipped.
pub(super) fn parse_standing(text: &str) -> Vec<(String, String)> {
    text.lines()
        .filter_map(|l| {
            let (subject, value) = l.split_once(' ')?;
            (!subject.is_empty() && !value.is_empty() && !value.contains(' '))
                .then(|| (subject.to_string(), value.to_string()))
        })
        .collect()
}

impl Record for Step {
    const JOURNAL: &'static str = "notify journal";

    fn body(&self) -> String {
        let mut lines = Vec::new();
        if let Some(verdict) = &self.verdict {
            lines.push(format!("verdict {verdict}"));
        }
        if !self.spent.is_empty() {
            let spent: Vec<String> = self.spent.iter().map(u64::to_string).collect();
            lines.push(format!("spent {}", spent.join(",")));
        }
        if let Some((n, mark)) = self.suppressed {
            lines.push(format!("suppressed {n} {mark}"));
        }
        for (subject, value) in &self.stand {
            lines.push(match value {
                Some(value) => format!("stand {} {}", word(subject), word(value)),
                None => format!("stand {}", word(subject)),
            });
        }
        for (key, off) in &self.cursor {
            lines.push(format!("cursor {} {off}", word(key)));
        }
        lines.join("\n")
    }

    fn tail(&self) -> String {
        String::new()
    }

    fn of_record(body: &[u8], _tail: &[u8]) -> Option<Self> {
        Self::parse(body)
    }
}

/// The offset a verdict line names: its first word.
pub(super) fn offset_of(line: &str) -> Option<u64> {
    line.split_whitespace().next().and_then(crate::parse_uint)
}

/// Note `line` in `lines`, replacing the verdict at its offset and keeping the newest
/// [`JOURNAL_KEEP`]: the line is the newest.
pub(super) fn note(lines: &mut VecDeque<String>, line: String) {
    if let Some(off) = offset_of(&line) {
        if let Some(at) = lines.iter().position(|l| offset_of(l) == Some(off)) {
            lines.remove(at);
        }
    }
    lines.push_back(line);
    while lines.len() > JOURNAL_KEEP {
        lines.pop_front();
    }
}

/// What a journal's steps say: its verdicts (the newest per offset, at most [`JOURNAL_KEEP`],
/// oldest first), the stamps spent, in the order spent, the suppressed count with its mark, the
/// standing values (the newest per subject, the oldest set first: the notifier bounds them,
/// [`super::STANDING_KEEP`], and writes what its bound drops as a clear), and each selector's
/// cursor (the newest per key: one per selector the store was read for).
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub(super) struct Verdicts {
    pub(super) lines: VecDeque<String>,
    pub(super) stamps: Vec<u64>,
    pub(super) suppressed: (u64, u64),
    pub(super) standing: Vec<(String, String)>,
    pub(super) cursors: BTreeMap<String, u64>,
}

impl Replay for Verdicts {
    type Record = Step;

    fn apply(&mut self, step: Step) {
        if let Some(line) = step.verdict {
            note(&mut self.lines, line);
        }
        for t in step.spent {
            note_stamp(&mut self.stamps, t);
        }
        if let Some(suppressed) = step.suppressed {
            self.suppressed = suppressed;
        }
        for (subject, value) in step.stand {
            note_stand(&mut self.standing, subject, value);
        }
        self.cursors.extend(step.cursor);
    }
}

/// Apply one change of the standing values to `standing`: `subject` cleared, then, where it
/// has a value, set to it as the newest.
pub(super) fn note_stand(
    standing: &mut Vec<(String, String)>,
    subject: String,
    value: Option<String>,
) {
    standing.retain(|(s, _)| *s != subject);
    if let Some(value) = value {
        standing.push((subject, value));
    }
}

/// Note stamp `t` in `stamps`, which are in order, once: an append where it is the newest, as a
/// page's stamp is, in its place where it is older (one [`super::Notifier::restore`] spends).
pub(super) fn note_stamp(stamps: &mut Vec<u64>, t: u64) {
    if let Err(at) = stamps.binary_search(&t) {
        stamps.insert(at, t);
    }
}

impl Verdicts {
    /// Keep the newest `limit` stamps, by their value.
    pub(super) fn keep_stamps(&mut self, limit: usize) {
        let over = self.stamps.len().saturating_sub(limit);
        self.stamps.drain(..over);
    }

    /// The steps a journal holding exactly this opens with: a verdict a step, oldest first, then
    /// the stamps, [`STAMPS_PER_STEP`] a step, the count with the last (its own step where no
    /// stamp is spent and it is not nothing), then a standing value a step, the oldest set first,
    /// then the cursors, each key's newest, in one step.
    fn steps(&self) -> Vec<Step> {
        let mut steps: Vec<Step> = self
            .lines
            .iter()
            .map(|line| Step {
                verdict: Some(line.clone()),
                ..Step::default()
            })
            .collect();
        steps.extend(self.stamps.chunks(STAMPS_PER_STEP).map(|chunk| Step {
            spent: chunk.to_vec(),
            ..Step::default()
        }));
        if self.suppressed != (0, 0) {
            match steps.last_mut().filter(|s| !s.spent.is_empty()) {
                Some(last) => last.suppressed = Some(self.suppressed),
                None => steps.push(Step {
                    suppressed: Some(self.suppressed),
                    ..Step::default()
                }),
            }
        }
        steps.extend(self.standing.iter().map(|(subject, value)| Step {
            stand: vec![(subject.clone(), Some(value.clone()))],
            ..Step::default()
        }));
        if !self.cursors.is_empty() {
            steps.push(Step {
                cursor: self
                    .cursors
                    .iter()
                    .map(|(key, &off)| (key.clone(), off))
                    .collect(),
                ..Step::default()
            });
        }
        steps
    }

    /// The journal that holds exactly this, its steps from 0: the bytes and how many steps.
    ///
    /// # Errors
    /// A step that would not read back ([`journal::encode`]): a verdict line no body carries.
    pub(super) fn encode(&self) -> io::Result<(Vec<u8>, u64)> {
        encode_from(0, &self.steps())
    }
}

/// The records of `steps` from `step`: the bytes and the step after the last.
///
/// # Errors
/// A step that would not read back ([`journal::encode`]).
pub(super) fn encode_from(mut step: u64, steps: &[Step]) -> io::Result<(Vec<u8>, u64)> {
    let mut bytes = Vec::new();
    for record in steps {
        bytes.extend_from_slice(&journal::encode(step, record)?);
        step += 1;
    }
    Ok((bytes, step))
}

/// Read a journal's bytes ([`journal::read_as`]).
#[must_use]
pub(super) fn read(bytes: &[u8]) -> journal::Read<Verdicts> {
    journal::read_as(bytes)
}

/// One store's journal as this notifier holds it: what the file holds and where an append goes.
#[derive(Debug)]
pub(super) struct StepsFile {
    /// The store's root, whose [`FILE`] it is.
    store: Store,
    /// What the file holds, as its steps say: a rewrite writes it whole.
    pub(super) held: Verdicts,
    /// The descriptor appends go to, once one was made.
    file: Option<std::fs::File>,
    /// The bytes the file holds, the step its next record carries, and its steps.
    len: u64,
    next_step: u64,
    pub(super) steps: u64,
    /// The file is to be rewritten whole before anything is appended: there is none, its tail
    /// was torn or damaged, or an append to it failed part-way.
    whole: bool,
    /// The steps and the bytes past both of which the next write rewrites it ([`Bound`]).
    pub(super) bound: Bound,
    /// A test's mutant: every write rewrites the file whole, as the line files were.
    #[cfg(test)]
    pub(super) rewrite_always: bool,
}

/// The steps below which no rewrite comes: twice [`JOURNAL_KEEP`].
const COMPACT_FLOOR: u64 = 2 * JOURNAL_KEEP as u64;

/// When a journal is rewritten from what it holds: once its steps pass twice what that rewrite
/// writes, and never below [`COMPACT_FLOOR`], and its bytes pass twice what it writes, so a
/// rewrite's steps and bytes are each spread over at least as many appended. Both are what the
/// file holds now would rewrite to, at a load as after a rewrite ([`Bound::of`]): never the
/// file's own length, which an open after a long run would carry forward doubled.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) struct Bound {
    pub(super) steps: u64,
    pub(super) bytes: u64,
}

impl Bound {
    /// The bound of a file whose rewrite writes `steps` steps of `bytes` bytes.
    fn after(steps: u64, bytes: u64) -> Self {
        Self {
            steps: COMPACT_FLOOR.max(steps.saturating_mul(2)),
            bytes: bytes.saturating_mul(2),
        }
    }

    /// The bound of a file holding `held`: what its rewrite would write.
    fn of(held: &Verdicts) -> Self {
        held.encode().map_or(Self::after(0, 0), |(bytes, steps)| {
            Self::after(steps, bytes.len() as u64)
        })
    }
}

#[cfg(test)]
thread_local! {
    /// A test's mutant: an open bounds the file by twice its own steps and bytes, not by what
    /// it holds would rewrite to ([`Bound::of`]).
    pub(super) static OPEN_BOUND_FROM_FILE: std::cell::Cell<bool> =
        const { std::cell::Cell::new(false) };
}

impl StepsFile {
    fn new(store: &Store, held: Verdicts, len: u64, steps: u64, whole: bool) -> Self {
        #[cfg(test)]
        let from_file = OPEN_BOUND_FROM_FILE.with(std::cell::Cell::get);
        #[cfg(not(test))]
        let from_file = false;
        Self {
            store: store.clone(),
            bound: if from_file {
                Bound::after(steps, len)
            } else {
                Bound::of(&held)
            },
            held,
            file: None,
            len,
            next_step: steps,
            steps,
            whole,
            #[cfg(test)]
            rewrite_always: false,
        }
    }

    pub(super) fn same_device(&self, other: &Self) -> bool {
        self.store
            .directory
            .file
            .metadata()
            .ok()
            .zip(other.store.directory.file.metadata().ok())
            .is_some_and(|(a, b)| super::file_id(&a).0 == super::file_id(&b).0)
    }

    /// Put `steps` in the file, flushed by `flush`: appended to it in one write, or, where the
    /// file is to be rewritten whole, would pass its [`Bound`], or is not one this notifier
    /// appends to ([`StepsFile::open`]), the file replaced by one that
    /// holds what it held and them ([`Verdicts::encode`]), its directory flushed by `flush`.
    /// `stamps` are the stamps a rewrite carries, where the caller holds them (the notifier's,
    /// in order); otherwise it carries the ones the file held. Nothing is held as written until
    /// the write returns: refused, the file is rewritten whole at the next write, from what it
    /// held before.
    ///
    /// # Errors
    /// The write or its flush, or a step that would not read back.
    pub(super) fn write(
        &mut self,
        steps: &[Step],
        flush: Flush,
        stamps: Option<&[u64]>,
    ) -> io::Result<()> {
        if steps.is_empty() {
            return Ok(());
        }
        let written = self.put(steps, flush, stamps);
        if written.is_err() {
            self.whole = true;
            self.file = None;
        }
        written
    }

    fn put(&mut self, steps: &[Step], flush: Flush, stamps: Option<&[u64]>) -> io::Result<()> {
        let count = steps.len() as u64;
        #[cfg(test)]
        let always = self.rewrite_always;
        #[cfg(not(test))]
        let always = false;
        let (bytes, next) = encode_from(self.next_step, steps)?;
        let past = self.steps.saturating_add(count) > self.bound.steps
            && self.len.saturating_add(bytes.len() as u64) > self.bound.bytes;
        if !(always || self.whole || past) {
            super::test_fault(FILE)?;
            if let Some(file) = self.target()? {
                file.write_all(&bytes)?;
                note_bytes(bytes.len());
                flush_file(file, flush)?;
                for step in steps {
                    self.held.apply(step.clone());
                }
                self.len += bytes.len() as u64;
                self.next_step = next;
                self.steps += count;
                return Ok(());
            }
        }
        let mut held = self.held.clone();
        for step in steps {
            held.apply(step.clone());
        }
        if let Some(stamps) = stamps {
            held.stamps = stamps.to_vec();
            held.stamps.sort_unstable();
            held.stamps.dedup();
        }
        let (bytes, next) = held.encode()?;
        self.file = None;
        let dir = self.store.put_bytes(FILE, &bytes)?;
        flush_dir(&dir, flush)?;
        self.held = held;
        self.len = bytes.len() as u64;
        self.next_step = next;
        self.steps = next;
        self.whole = false;
        self.bound = Bound::after(next, self.len);
        Ok(())
    }

    /// The descriptor an append goes to: the one held, or one opened now ([`StepsFile::open`]).
    /// None where there is none to open: the file is then rewritten whole. The one held stays
    /// the file and its bytes between appends, since this notifier is its store's one writer
    /// ([`super::Store::open`]); an append that failed part-way lets it go ([`StepsFile::write`]).
    ///
    /// # Errors
    /// The file's open or its `stat`.
    fn target(&mut self) -> io::Result<Option<&mut std::fs::File>> {
        if self.file.is_none() {
            self.file = self.open()?;
        }
        Ok(self.file.as_mut())
    }

    /// Open the file to append to it: an owned regular single-link file of the bytes this
    /// notifier holds it to, which it wrote or read. None where it is not that (a file changed
    /// by hand, a link planted in its place), which is then rewritten whole.
    ///
    /// # Errors
    /// The open or its `stat`.
    fn open(&self) -> io::Result<Option<std::fs::File>> {
        use std::os::unix::fs::MetadataExt as _;
        let file = match self.store.directory.open_file(
            std::ffi::OsStr::new(FILE),
            aterm_dirfd::OFlags::WRONLY | aterm_dirfd::OFlags::APPEND,
            0,
        ) {
            Ok(file) => file,
            Err(e) if e.kind() == io::ErrorKind::NotFound => return Ok(None),
            Err(e) if e.raw_os_error() == Some(libc::ELOOP) => return Ok(None),
            Err(e) => return Err(e),
        };
        let meta = file.metadata()?;
        let ours = meta.is_file() && meta.uid() == aterm_uds::peer::our_uid() && meta.nlink() == 1;
        Ok((ours && meta.len() == self.len).then_some(file))
    }
}

/// Say on stderr that the line file `path` does not read (`e`) and is read as empty: to be set
/// aside with its bytes kept where `migrating`, left as it was otherwise.
fn unread(path: &Path, e: &io::Error, migrating: bool) {
    warn(&format!(
        "{} does not read ({e}); it is {}",
        path.display(),
        if migrating {
            "migrated as empty and set aside, its bytes kept"
        } else {
            "read as empty and left as it was"
        }
    ));
}

/// Finish an append to `file` by `flush`: ordered (`aterm_durable::ordered_or_fsync`), or in
/// full (a full flush the volume refuses taken as the plain one it can do,
/// `aterm_durable::refusal_accepted`).
fn flush_file(file: &std::fs::File, flush: Flush) -> io::Result<()> {
    note_flush(flush);
    match flush {
        Flush::Ordered => aterm_durable::ordered_or_fsync(file),
        Flush::Durable => aterm_durable::refusal_accepted(aterm_durable::durable(file)),
    }
}

impl Store {
    /// THE STORE'S JOURNAL, opened: what [`Store::read_steps`] reads, and the file made to hold
    /// exactly that where it does not (none there, a torn tail, damage), rewritten whole now,
    /// its directory entry ordered, so a page's first append costs what every other one does.
    /// A rewrite refused leaves it to the next write. A line-format [`STANDING`] file there is
    /// migrated: what it says replaces the journal's standing values (it is the newest: only a
    /// build before them in the journal writes it), the journal is rewritten whole with them,
    /// and only then is it set aside (`standing.migrated`). One that does not read says none
    /// ([`Store::line_standing`]): the journal's standing values stand, and it is set aside.
    /// A cursor's file there (`cur/<key>`, [`Store::line_cursors`]) is migrated the same way, in
    /// the same rewrite: its offset moves the journal's cursor of that key forward to it, never
    /// back ([`forward`]: a file a power loss revived after it was set aside never sets a newer
    /// step's cursor back), and only then is it set aside (`cur/<key>.migrated`); one that does
    /// not read says none, the journal's cursor of the key standing.
    ///
    /// # Errors
    /// [`Store::read_steps`]'s, a [`CURSORS`] directory that does not list, or the rewrite that
    /// migrates a [`STANDING`] or cursor file or its rename aside after it: then the file stays,
    /// and the next load migrates it again. A load that went on past either would run with a
    /// file the next load lays over newer steps, so a store on a full or read-only volume that
    /// still holds one refuses to open (the notifier does not start, or its switch to the log
    /// fails) until it can be migrated, as a line-file migration refused does.
    pub(super) fn load(&self) -> io::Result<StepsFile> {
        let mut file = self.read_steps()?;
        let standing = self.line_standing(true);
        let cursors: Vec<(String, Option<u64>)> = self
            .line_cursors()?
            .into_iter()
            .map(|(key, off)| {
                let off = off
                    .inspect_err(|e| unread(&self.root.join(CURSORS).join(&key), e, true))
                    .ok();
                (key, off)
            })
            .collect();
        if standing.is_some() || !cursors.is_empty() {
            if let Some(Some(standing)) = &standing {
                file.held.standing.clone_from(standing);
            }
            for (key, off) in &cursors {
                if let Some(off) = off {
                    forward(&mut file.held.cursors, key, *off);
                }
            }
            file.whole = true;
            file.put(&[], Flush::Ordered, None).inspect_err(|_| {
                file.file = None;
            })?;
            if standing.is_some() {
                self.set_aside(&[STANDING], true)?;
            }
            let names: Vec<String> = cursors
                .iter()
                .map(|(key, _)| format!("{CURSORS}/{key}"))
                .collect();
            let names: Vec<&str> = names.iter().map(String::as_str).collect();
            self.set_aside(&names, true)?;
            return Ok(file);
        }
        if file.whole {
            let _ = file.put(&[], Flush::Ordered, None).inspect_err(|_| {
                file.whole = true;
                file.file = None;
            });
        }
        Ok(file)
    }

    /// THE STORE'S JOURNAL, read now: its whole steps, and the [`StepsFile`] that appends to it.
    /// A torn last step reads as the steps before it, the file to be rewritten whole. Damage
    /// before an intact step is a fault: the file's bytes are kept as [`FAULT`] (said on
    /// stderr), every whole step before and after it is read, and the file is to be rewritten
    /// whole. A store with no journal migrates its line files ([`Store::migrate`]).
    ///
    /// # Errors
    /// The journal does not read (not a regular file, too long, unreadable), or the
    /// migration's write.
    fn read_steps(&self) -> io::Result<StepsFile> {
        let path = self.root.join(FILE);
        let bytes = match self.read_bytes(FILE, READ_MAX) {
            Ok(bytes) => bytes,
            Err(e) if e.kind() == io::ErrorKind::NotFound => match self.metadata(FILE) {
                Err(missing) if missing.kind() == io::ErrorKind::NotFound => {
                    return self.migrate();
                }
                Err(other) => return Err(other),
                Ok(_) => return Err(e),
            },
            Err(e) => return Err(e),
        };
        let _ = self.set_aside(&LINE_FILES, false);
        let read = read(&bytes);
        Ok(match read.end {
            End::Clean => StepsFile::new(self, read.index, read.valid_len, read.next_step, false),
            End::Cut | End::Torn => {
                StepsFile::new(self, read.index, read.valid_len, read.next_step, true)
            }
            End::Fault { at, why } => {
                let name = self.free_name(FAULT)?;
                let kept = self.put_bytes(&name, &bytes).map(|_| ());
                warn(&format!(
                    "{} does not read whole ({why} at byte {at}){}; it is read from its whole \
                     steps and written again from them",
                    path.display(),
                    match kept {
                        Ok(()) => format!(
                            ": its bytes are kept as {}",
                            self.root.join(&name).display()
                        ),
                        Err(e) => format!(", and its bytes could not be kept ({e})"),
                    }
                ));
                StepsFile::new(self, journal::salvage_as(&bytes), 0, 0, true)
            }
        })
    }

    /// MIGRATE THE LINE FILES ONCE: read what the line files say ([`Store::line_verdicts`]),
    /// write it as [`FILE`], its directory entry ordered, and only then rename each aside
    /// ([`SET_ASIDE`]). A line file that does not read (not text, too long, not a regular
    /// file) is said on stderr, read as empty, as a build before the journal read it, and set
    /// aside with the rest, its bytes kept. A store with none of them has an empty journal, to
    /// be written whole.
    ///
    /// # Errors
    /// The journal's write: then nothing is set aside, and the next load migrates again.
    fn migrate(&self) -> io::Result<StepsFile> {
        let Some(held) = self.line_verdicts(true) else {
            return Ok(StepsFile::new(self, Verdicts::default(), 0, 0, true));
        };
        let (bytes, steps) = held.encode()?;
        let dir = self.put_bytes(FILE, &bytes)?;
        flush_dir(&dir, Flush::Ordered)?;
        let _ = self.set_aside(&LINE_FILES, false);
        Ok(StepsFile::new(self, held, bytes.len() as u64, steps, false))
    }

    /// `name`, or, where that is taken, the first of `name.1`, `name.2`, … that is not: where
    /// a copy is kept without replacing one kept before it.
    fn free_name(&self, name: &str) -> io::Result<String> {
        let free = |candidate: &str| match self.metadata(candidate) {
            Ok(_) => Ok(false),
            Err(error) if error.kind() == io::ErrorKind::NotFound => Ok(true),
            Err(error) => Err(error),
        };
        if free(name)? {
            return Ok(name.to_owned());
        }
        for index in 1u64.. {
            let candidate = format!("{name}.{index}");
            if free(&candidate)? {
                return Ok(candidate);
            }
        }
        Err(io::Error::new(
            io::ErrorKind::AlreadyExists,
            "notify archive names exhausted",
        ))
    }

    /// Rename each of the line files `names` there is aside ([`SET_ASIDE`]), each directory an
    /// entry moved in (the store's, or [`CURSORS`] for a cursor's file) flushed ordered: once [`FILE`] holds what they said, after the migration or at a load
    /// after a kill cut it short. One whose set-aside name is taken (a line file written after
    /// the migration, by a build before the journal) is kept beside it under the next free
    /// name, never over it; where it was not `read` into the journal, that is said on stderr:
    /// its verdicts are not the journal's. A rename refused is said, and done again at the next
    /// load.
    ///
    /// # Errors
    /// The last rename refused, once every other was tried: a caller whose file a later load
    /// would read again over newer steps ([`STANDING`]) fails on it.
    fn set_aside(&self, names: &[&str], read: bool) -> io::Result<()> {
        let mut moved = std::collections::BTreeMap::new();
        let mut refused = None;
        for &name in names {
            match self.metadata(name) {
                Ok(_) => {}
                Err(error) if error.kind() == io::ErrorKind::NotFound => continue,
                Err(error) => {
                    refused = Some(error);
                    continue;
                }
            }
            let base = format!("{name}{SET_ASIDE}");
            let result = (|| {
                let free = self.free_name(&base)?;
                let (from_parent, from_leaf) = self.directory.parent(name, false)?;
                let (to_parent, to_leaf) = self.directory.parent(&free, false)?;
                super::test_fault(&free)?;
                from_parent.rename(&from_leaf, &to_parent, &to_leaf)?;
                for directory in [from_parent, to_parent] {
                    moved.insert(super::file_id(&directory.file.metadata()?), directory);
                }
                if free != base && !read {
                    warn(&format!(
                        "{} was written after its journal and is kept as {}",
                        self.root.join(name).display(),
                        self.root.join(free).display()
                    ));
                }
                Ok::<(), io::Error>(())
            })();
            if let Err(error) = result {
                warn(&format!(
                    "could not set {} aside: {error}",
                    self.root.join(name).display()
                ));
                refused = Some(error);
            }
        }
        for directory in moved.into_values() {
            if let Err(error) = flush_dir(&directory, Flush::Ordered) {
                warn(&format!("{}: {error}", directory.path.display()));
            }
        }
        refused.map_or(Ok(()), Err)
    }

    /// Replace the store's journal by one holding `held`, its directory flushed by `flush`.
    ///
    /// # Errors
    /// The write, or a verdict line no step carries.
    pub(super) fn set_verdicts(&self, held: &Verdicts, flush: Flush) -> io::Result<()> {
        let (bytes, _) = held.encode()?;
        let dir = self.put_bytes(FILE, &bytes)?;
        flush_dir(&dir, flush)
    }
}

/// A test's reads and writes of a store's journal ([`Store::load`], [`Store::set_verdicts`]).
#[cfg(test)]
impl Store {
    /// The verdicts the store's journal holds, oldest first, as a load reads them
    /// ([`Store::load`]).
    pub(super) fn verdicts(&self) -> io::Result<Verdicts> {
        self.load().map(|file| file.held)
    }

    /// Its verdicts, oldest first; none where it does not read.
    pub(super) fn journal(&self) -> Vec<String> {
        self.verdicts()
            .map(|held| held.lines.into_iter().collect())
            .unwrap_or_default()
    }

    /// Replace it by one holding `lines` and the cursors it held, and nothing else.
    pub(super) fn set_journal(&self, lines: &[String], flush: Flush) -> io::Result<()> {
        self.set_verdicts(
            &Verdicts {
                lines: lines.iter().cloned().collect(),
                cursors: self.read_verdicts()?.cursors,
                ..Verdicts::default()
            },
            flush,
        )
    }

    /// The stamps it holds.
    pub(super) fn stamps(&self) -> Vec<u64> {
        self.verdicts().map(|held| held.stamps).unwrap_or_default()
    }

    /// The suppressed count it holds, and its mark.
    pub(super) fn suppressed(&self) -> (u64, u64) {
        self.verdicts()
            .map(|held| held.suppressed)
            .unwrap_or_default()
    }

    /// The standing values it holds, oldest first, read and never written
    /// ([`Store::read_verdicts`]); none where it does not read.
    pub(super) fn standing(&self) -> Vec<(String, String)> {
        self.read_verdicts()
            .map(|held| held.standing)
            .unwrap_or_default()
    }

    /// Replace its standing values by `rows`, keeping everything else it holds.
    pub(super) fn set_standing(&self, rows: &[(String, String)]) -> io::Result<()> {
        let mut held = self.read_verdicts()?;
        held.standing = rows.to_vec();
        self.set_verdicts(&held, Flush::Ordered)
    }
}

/// The verdicts `<state>/notify/`'s journal holds, oldest first, read and never written: what
/// a reader outside the notifier (a test, the fabric bench) takes `aterm link notify` to have
/// judged. A journal that is not there, or does not read, holds none; a torn last step reads as
/// the steps before it.
#[must_use]
pub fn journal_lines(state_dir: &Path) -> Vec<String> {
    read_only(state_dir).lines.into_iter().collect()
}

/// The suppressed count `<state>/notify/`'s journal holds ([`journal_lines`]'s reading).
#[must_use]
pub fn suppressed_count(state_dir: &Path) -> u64 {
    read_only(state_dir).suppressed.0
}

/// Selector `key`'s cursor in `<state>/notify/` (a key is [`super::Sel::key`]), read and never
/// written: what a reader outside the notifier (a test) takes `aterm link notify` to have
/// passed, as an open would read it ([`Store::read_cursor`]). None where it holds none or
/// does not read.
#[must_use]
pub fn cursor_of(state_dir: &Path, key: &str) -> Option<u64> {
    Store::<super::ReadOnly>::existing(state_dir.join("notify"))
        .ok()?
        .read_cursor(key)
}

/// A test's rewind of selector `key`'s cursor in `<state>/notify/` (a key is
/// [`super::Sel::key`]) to `off`, back or forward: one step of its journal, ordered, the store
/// as it would stand had its cursor never passed `off`. No notifier may hold the store
/// ([`super::Store::open`]); a cursor's file cannot set it back ([`forward`]).
///
/// # Errors
/// The store's open ([`Store::load`]) or the step's write.
#[cfg(any(test, debug_assertions))]
pub fn rewind_cursor(state_dir: &Path, key: &str, off: u64) -> io::Result<()> {
    Store::open(
        state_dir
            .to_str()
            .ok_or_else(|| io::Error::new(io::ErrorKind::InvalidInput, "non-UTF-8 notify state"))?,
    )?
    .load()?
    .write(
        &[Step {
            cursor: vec![(key.to_string(), off)],
            ..Step::default()
        }],
        Flush::Ordered,
        None,
    )
}

/// Move `key`'s cursor in `cursors` forward to `off`, never back: a selector's cursor only
/// moves forward in its store ([`super::Notifier::put_cursor`]), so a cursor's file a power
/// loss revives after its migration ([`Store::load`]), its rename aside lost with its
/// directory's entry, never sets the journal's newer cursor back.
fn forward(cursors: &mut BTreeMap<String, u64>, key: &str, off: u64) {
    let at = cursors.entry(key.to_string()).or_insert(off);
    *at = (*at).max(off);
}

fn read_only(state_dir: &Path) -> Verdicts {
    Store::<super::ReadOnly>::existing(state_dir.join("notify"))
        .and_then(|store| store.read_bytes(FILE, READ_MAX))
        .map(|bytes| read(&bytes).index)
        .unwrap_or_default()
}

#[cfg(test)]
#[path = "notify_journal_tests.rs"]
pub(super) mod tests;

impl<Access> Store<Access> {
    /// What the line files a build before the journal wrote say: `journal` (a verdict a line),
    /// `rate` (the stamps, comma-joined) and `suppressed` (`<n> t=<mark>`). None where there is
    /// none of them. A line file that does not read (not text, too long, not a regular file) is
    /// read as empty, as a build before the journal read it, and said on stderr: to be set
    /// aside with its bytes kept where `migrating` ([`Store::migrate`]), left as it was
    /// otherwise ([`Store::read_verdicts`]).
    fn line_verdicts(&self, migrating: bool) -> Option<Verdicts> {
        let mut present = false;
        let [journal, rate, suppressed] = LINE_FILES.map(|name| match self.carry_text(name) {
            Ok(text) => {
                present |= text.is_some();
                text
            }
            Err(e) => {
                present = true;
                unread(&self.root.join(name), &e, migrating);
                None
            }
        });
        if !present {
            return None;
        }
        let mut held = Verdicts::default();
        for line in journal.as_deref().unwrap_or_default().lines() {
            if is_line(line) {
                note(&mut held.lines, line.to_string());
            }
        }
        for t in rate.as_deref().unwrap_or_default().split(',') {
            if let Some(t) = crate::parse_uint::<u64>(t.trim()) {
                note_stamp(&mut held.stamps, t);
            }
        }
        let text = suppressed.unwrap_or_default();
        let mut words = text.split_whitespace();
        let n = words.next().and_then(crate::parse_uint).unwrap_or(0);
        let mark = words
            .find_map(|w| w.strip_prefix("t="))
            .and_then(crate::parse_uint)
            .unwrap_or(0);
        held.suppressed = (n, mark);
        Some(held)
    }

    /// What a line-format [`STANDING`] file says ([`parse_standing`]): `None` where there is
    /// none, `Some(None)` where there is one that does not read. That one is said on stderr, as
    /// [`Store::line_verdicts`] says a line file, to be set aside with its bytes kept where
    /// `migrating` ([`Store::load`]) and left as it was otherwise ([`Store::read_verdicts`]),
    /// and it says no standing values: the journal's stand, which a line file that reads
    /// replaces.
    fn line_standing(&self, migrating: bool) -> Option<Option<Vec<(String, String)>>> {
        match self.carry_text(STANDING) {
            Ok(text) => text.as_deref().map(|text| Some(parse_standing(text))),
            Err(e) => {
                unread(&self.root.join(STANDING), &e, migrating);
                Some(None)
            }
        }
    }

    /// The cursor files a build before the cursors were steps wrote, `cur/<key>` a selector
    /// ([`CURSORS`]), by key, with the offset each says, or why it does not read (not text, too
    /// long, not a regular file, not an offset): a load migrates that one as none, said on
    /// stderr, and sets it aside with its bytes kept ([`Store::load`]); a read-only read
    /// refuses it, as an incomplete read ([`Store::read_verdicts`]), or reads it as none
    /// ([`Store::cursor`]). A temporary a replace left (a name that starts with `.` or ends
    /// `.tmp`) and a file set aside before ([`SET_ASIDE`]) are no cursor. Empty where there is
    /// no [`CURSORS`].
    ///
    /// # Errors
    /// [`CURSORS`] is there and does not list (not a directory, unreadable), or an entry of it.
    pub(super) fn line_cursors(&self) -> io::Result<Vec<(String, io::Result<u64>)>> {
        let directory = match self.directory.child(std::ffi::OsStr::new(CURSORS), false) {
            Ok(directory) => directory,
            Err(error) if error.kind() == io::ErrorKind::NotFound => match self.metadata(CURSORS) {
                Err(missing) if missing.kind() == io::ErrorKind::NotFound => return Ok(Vec::new()),
                Err(other) => return Err(other),
                Ok(_) => return Err(error),
            },
            Err(error) => return Err(error),
        };
        let mut cursors = Vec::new();
        for name in directory.names()? {
            let Some(key) = name.to_str().filter(|key| {
                !key.starts_with('.')
                    && !key.ends_with(".tmp")
                    && !key.ends_with(SET_ASIDE)
                    && !key.contains(&format!("{SET_ASIDE}."))
            }) else {
                continue;
            };
            let off = directory
                .read(&name, 4096)
                .and_then(|bytes| {
                    String::from_utf8(bytes)
                        .map_err(|error| io::Error::new(io::ErrorKind::InvalidData, error))
                })
                .and_then(|text| {
                    crate::parse_uint::<u64>(text.trim()).ok_or_else(|| {
                        io::Error::new(io::ErrorKind::InvalidData, "not a notifier cursor")
                    })
                });
            cursors.push((key.to_owned(), off));
        }
        cursors.sort_by(|a, b| a.0.cmp(&b.0));
        Ok(cursors)
    }

    /// The verdicts the store's journal holds, read and never written: its whole steps (a
    /// torn last step reads as the steps before it), or, in a store with no journal, what its
    /// line files say ([`Store::line_verdicts`]); a store with neither holds none. A
    /// line-format [`STANDING`] file there that reads says its standing values, as a load
    /// migrates it ([`Store::load`]), and a cursor's file there moves its key's cursor forward
    /// to what it says ([`Store::line_cursors`]). A line file that does not read is read as empty (a
    /// [`STANDING`] one as saying none, the journal's standing values kept), as a load reads
    /// it, and left as it was, so an old store's unreadable line file fails no carry from it;
    /// a cursor's file that does not read is refused, since a carry without it would start
    /// that selector over. What [`Store::carry`] carries from a store it must not change,
    /// which may be the live one.
    ///
    /// # Errors
    /// The journal does not read, or reads as a fault (damage before an intact step), or a
    /// cursor's file there does not read, or its [`CURSORS`] directory does not list: an
    /// incomplete read.
    pub(super) fn read_verdicts(&self) -> io::Result<Verdicts> {
        let mut held = self.read_journal(false)?;
        if let Some(Some(standing)) = self.line_standing(false) {
            held.standing = standing;
        }
        for (key, off) in self.line_cursors()? {
            forward(&mut held.cursors, &key, off?);
        }
        Ok(held)
    }

    /// `key`'s cursor as the store holds it, read and never written, as a load would read and
    /// migrate it ([`Store::load`]): the journal's, from its whole steps (a torn last step
    /// read as the steps before it, damage before an intact step read around, as a load
    /// salvages it, so a damaged journal never reads as a selector with no cursor), moved
    /// forward to a cursor's file there that reads (`cur/<key>`), never back. None where
    /// neither reads.
    pub(super) fn read_cursor(&self, key: &str) -> Option<u64> {
        let mut cursors = self
            .read_journal(true)
            .map(|held| held.cursors)
            .unwrap_or_default();
        if let Some(off) = self
            .line_cursors()
            .ok()
            .and_then(|cursors| cursors.into_iter().find(|(k, _)| k == key))
            .and_then(|(_, off)| off.ok())
        {
            forward(&mut cursors, key, off);
        }
        cursors.get(key).copied()
    }

    /// [`Store::read_verdicts`] but for a line-format [`STANDING`] file and the cursors' files.
    /// Damage before an intact step is refused, an incomplete read, unless `salvage`: then the
    /// whole steps before and after it are read, as a load reads them ([`Store::read_steps`]).
    fn read_journal(&self, salvage: bool) -> io::Result<Verdicts> {
        let path = self.root.join(FILE);
        let bytes = match self.read_bytes(FILE, READ_MAX) {
            Ok(bytes) => bytes,
            Err(e) if e.kind() == io::ErrorKind::NotFound => {
                return match self.metadata(FILE) {
                    Err(missing) if missing.kind() == io::ErrorKind::NotFound => {
                        Ok(self.line_verdicts(false).unwrap_or_default())
                    }
                    Err(other) => Err(other),
                    Ok(_) => Err(e),
                };
            }
            Err(e) => return Err(e),
        };
        let read = read(&bytes);
        match read.end {
            End::Fault { .. } if salvage => Ok(journal::salvage_as(&bytes)),
            End::Fault { at, why } => Err(io::Error::new(
                io::ErrorKind::InvalidData,
                format!("{}: {why} at byte {at}", path.display()),
            )),
            End::Clean | End::Cut | End::Torn => Ok(read.index),
        }
    }
}
