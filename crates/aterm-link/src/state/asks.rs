// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! THE ASKS JOURNAL: one append-only file per state dir, [`FILE`], in place of the two tables a
//! build before it rewrote whole on every ask ([`ASKED`], each ask's recipient, and
//! [`DEADLINES`], each deadline owed a verdict).
//!
//! An ask cost the state dir those two replaces between its landing and the `outbox sent` that
//! releases its sender (`Bridge::landing_line`), 0.23-0.35 ms of an agent's ask (B11); each answer
//! that settled a deadline, and each sweep that changed one, cost the deadline table's replace
//! again. Here the ask's recipient and its deadline are one append of two records, and a
//! deadline settled is one record ([`Frame::DeadlineGone`]).
//!
//! The records are the post journal's ([`super::journal`]: an [`astream_wire::steps`] record a
//! fact, a body of ASCII text, read to its first damage, salvaged past it), in a file of their
//! own: a build of the post journal reads `journal/posts` strictly and refuses a record it does
//! not know, so these could not ride in it. The state dir keeps an [`Index`] of what the file
//! holds, trimmed to the tables' caps as it is replayed, and rewrites the file from it at the
//! post journal's size bound ([`super::journal::COMPACT_AT`]) and after a write the disk refused.
//!
//! A SETTLED DEADLINE IS REMEMBERED: its record stays in the index ([`Index::gone`], as many as
//! [`DEADLINES_KEEP`]), and is written again at every rewrite, so a retry of an ask's post at its
//! pin, by this bridge or by the next one after a kill, arms no deadline the ask's answer, or the
//! sweep's `expired`, already settled (item F4's residual: a bridge held that only in memory). A
//! retry after an answer the same bridge delivered with no deadline standing writes that settle
//! instead of arming (`Bridge::settle_a_retry_answered_ahead`); a kill between the two arms it,
//! and the sweep reads the answer back from the bus. A settle by this node's `expired` names in its
//! record's tail what the bridge judged that `expired` to be ([`Verdict`], ATERM-KI-1115-7889: the
//! ask's verdict, or moot behind its answer), so the next bridge hands no moot one and marks an
//! answer after a standing one late; a build before it reads the body alone.
//!
//! AN ASK NAMES THE POST WHOSE LANDING IT IS (ATERM-KI-1116-7314): a recipient or a deadline a
//! landing notes carries the post's in-flight sequence in its record's tail (`seq=<n>`,
//! [`Frame::AskedAt`], [`Frame::DeadlineAt`]), so an attach that lands the post again elsewhere
//! (`Bridge::note_relanded_ask`) finds the first landing's deadline, or its settle while its
//! recipient stands, where a log move did not forget it, and arms no second one. Every build
//! before it reads the body alone (its tail parse reads a settle's verdict and nothing else),
//! and a rewrite of its own writes the rows with no sequence, which this build reads as naming
//! no landing.
//!
//! FOR A BUILD BEFORE THE JOURNAL, which reads [`ASKED`] and [`DEADLINES`] and nothing here: a
//! stop writes the index back as those two files ([`super::StateDir::hand_asks_to_files`]),
//! each opened by a line that names the hand-off ([`legacy_text`], `handed=<n>`) and that such a
//! build's parse of a row skips, and records that hand-off ([`Frame::Handed`]). The files are
//! never unlinked, so a rollback, or a dev bundle sharing the dir, finds its deadlines where it
//! looks.
//!
//! WHAT SUCH A BUILD LEAVES COMES BACK, ONCE ([`Index::take_in`]). It rewrites a table whole and
//! names no hand-off, so a table with no line is that build's word, later than the journal's last
//! hand-off: it read that hand-off's rows and wrote what it holds now. Where the journal has noted
//! nothing since, the table replaces what the journal says of it, so what that build settled or
//! forgot on a move of the log stays gone and what it asked on the new log stands. The journal
//! records what it took ([`Frame::Took`], each table's digest), and a table with that digest is
//! read as already said: no open takes it in again, however long ago a settle aged out of
//! [`Index::gone`].

use std::collections::BTreeMap;
use std::io;

use super::journal::{self, number, Record, Replay};
use super::{Asked, Deadline, ASKED_KEEP, DEADLINES_KEEP};

/// The journal's name in [`super::journal::DIR`].
pub(crate) const NAME: &str = "asks";
/// The journal, relative to the state dir.
pub(crate) const FILE: &str = "journal/asks";
/// The asked table a build before the journal reads, relative to the state dir: one
/// [`Asked::render`] line an ask.
pub(crate) const ASKED: &str = "asked";
/// The deadline table a build before the journal reads, relative to the state dir: one
/// [`Deadline::render`] line a deadline.
pub(crate) const DEADLINES: &str = "deadlines";
/// How the line that opens a table a stop handed back names its hand-off: `handed=<n>`.
const MARK: &str = "handed=";

/// One fact the journal records.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) enum Frame {
    /// An ask this node published, and the principal whose reply it is.
    Asked(Asked),
    /// A deadline armed for the ask at its offset; a later one at the same offset replaces it.
    Deadline(Deadline),
    /// The deadline of `sid`'s ask at `off` is settled: answered, expired, or its asker gone.
    DeadlineGone {
        /// The ask's offset.
        off: u64,
        /// Its asker.
        sid: String,
        /// What this node's `expired` of the ask is, where it published one and judged it
        /// ([`Verdict`]): written in the record's tail (`expired=<e>`, `moot=<e> by=<a>`), which
        /// a build before ATERM-KI-1115-7889 skips. `None` for any other settle, and for a
        /// verdict not judged, which the bridge judges again at its delivery.
        how: Option<Verdict>,
    },
    /// A stop wrote the index back as the tables a build before the journal reads, as hand-off
    /// `n` ([`super::StateDir::hand_asks_to_files`]).
    Handed(u64),
    /// An open took the tables in as they stood ([`Index::take_in`]).
    Took(Took),
    /// [`Frame::Asked`] noted at a landing of the post in flight at sequence `seq`
    /// ([`Index::landings`], ATERM-KI-1116-7314): its record [`Frame::Asked`]'s, the sequence in
    /// its tail (`seq=<n>`), which every build before it skips, reading the record as the
    /// recipient alone.
    AskedAt {
        /// The ask's recipient.
        asked: Asked,
        /// The in-flight sequence whose landing noted it.
        seq: u64,
    },
    /// [`Frame::Deadline`] noted at a landing of the post in flight at sequence `seq`, as
    /// [`Frame::AskedAt`] is.
    DeadlineAt {
        /// The deadline.
        deadline: Deadline,
        /// The in-flight sequence whose landing noted it.
        seq: u64,
    },
}

/// A landing's sequence as a record's tail words ([`Frame::AskedAt`], [`Frame::DeadlineAt`]).
fn seq_words(seq: u64) -> String {
    format!("seq={seq}")
}

/// The sequence a record's tail names (`seq=<n>`, alone), `None` for any other tail.
fn seq_of_words(tail: &[u8]) -> Option<u64> {
    let text = std::str::from_utf8(tail).ok()?;
    let mut words = text.split_ascii_whitespace();
    let seq = number(words.next()?.strip_prefix("seq=")?)?;
    words.next().is_none().then_some(seq)
}

/// WHAT THIS NODE'S `expired` OF AN ASK IS (ATERM-KI-1115-7889), as the bridge judged it once
/// it was published (`Bridge::answered_below`): the ask's verdict is the first record on its
/// asker's lane that settles it, so an `expired` above an answer says nothing.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Verdict {
    /// The `expired` at `at` is the verdict: no answer lies below it, and an answer after it is
    /// late.
    Expired {
        /// The `expired`'s offset.
        at: u64,
    },
    /// The `expired` at `at` came after the answer at `by`, which is the verdict: it is never
    /// handed.
    Moot {
        /// The `expired`'s offset.
        at: u64,
        /// The answer's.
        by: u64,
    },
    /// The `expired` at `at` whose fence could not be read: judged at its delivery. Never
    /// journaled ([`Verdict::journaled`]).
    Unjudged {
        /// The `expired`'s offset.
        at: u64,
    },
}

impl Verdict {
    /// The `expired`'s offset.
    pub(crate) const fn at(self) -> u64 {
        match self {
            Self::Expired { at } | Self::Moot { at, .. } | Self::Unjudged { at } => at,
        }
    }

    /// This verdict as the journal keeps it: `None` for [`Verdict::Unjudged`], which a later
    /// bridge judges again.
    pub(crate) const fn journaled(self) -> Option<Self> {
        match self {
            Self::Unjudged { .. } => None,
            judged => Some(judged),
        }
    }

    /// Its tail words (`expired=<e>`, `moot=<e> by=<a>`); none for [`Verdict::Unjudged`].
    fn words(self) -> String {
        match self {
            Self::Expired { at } => format!("expired={at}"),
            Self::Moot { at, by } => format!("moot={at} by={by}"),
            Self::Unjudged { .. } => String::new(),
        }
    }

    /// The verdict a record's tail names, `None` for a tail this build cannot read.
    fn of_words(tail: &[u8]) -> Option<Self> {
        let text = std::str::from_utf8(tail).ok()?;
        let mut words = text.split_ascii_whitespace();
        let first = words.next()?;
        let verdict = if let Some(at) = first.strip_prefix("expired=") {
            Self::Expired { at: number(at)? }
        } else {
            let at = number(first.strip_prefix("moot=")?)?;
            let by = number(words.next()?.strip_prefix("by=")?)?;
            Self::Moot { at, by }
        };
        words.next().is_none().then_some(verdict)
    }
}

/// The tables as an open took them in: each one's [`digest`], that of no text where there is no
/// file.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct Took {
    /// The digest of [`ASKED`].
    pub(crate) asked: u64,
    /// The digest of [`DEADLINES`].
    pub(crate) deadlines: u64,
}

/// What names a table's text in [`Took`]. A stored hash, so one no build changes.
fn digest(text: &str) -> u64 {
    aterm_hash::fnv1a_64(text.as_bytes())
}

/// How an open reads a table it does not already hold ([`Index::word_of`]).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Word {
    /// Its rows, but a deadline the journal remembers settled, are written over the journal's,
    /// and the journal keeps the rest: it cannot tell what the table's writer saw.
    Fills,
    /// It is the whole of what its writer holds, and that writer saw all the journal says.
    Replaces,
}

/// Whether `word` is one word a body can carry: printable ASCII and no space.
fn is_word(word: &str) -> bool {
    !word.is_empty() && word.bytes().all(|b| b.is_ascii_graphic())
}

impl Frame {
    /// The table file a build before the journal wrote this fact into: a test's refusal of that
    /// file's write refuses this record (`StateDir::refuse_writes_of`), as it refused that build's
    /// replace of the table.
    #[cfg(test)]
    pub(crate) const fn written_file(&self) -> Option<&'static str> {
        match self {
            Self::Asked(_) | Self::AskedAt { .. } => Some(ASKED),
            Self::Deadline(_) | Self::DeadlineAt { .. } | Self::DeadlineGone { .. } => {
                Some(DEADLINES)
            }
            Self::Handed(_) | Self::Took(_) => None,
        }
    }

    /// Whether its record, body and tail, reads back as itself ([`journal::encode`] refuses one
    /// that does not): only such a fact enters the index, which every rewrite writes whole.
    pub(crate) fn carried(&self) -> bool {
        Self::of_record(self.body().as_bytes(), self.tail().as_bytes()).as_ref() == Some(self)
    }

    /// Parse a body back, `None` for anything this build did not write.
    fn parse(body: &[u8]) -> Option<Self> {
        let text = std::str::from_utf8(body).ok()?;
        let (head, rest) = text.split_once(' ')?;
        let mut words = rest.split(' ');
        let frame = match head {
            "asked" => {
                let off = number(words.next()?)?;
                let to = words.next().filter(|w| is_word(w))?;
                Self::Asked(Asked {
                    off,
                    to: to.to_string(),
                })
            }
            "deadline" => {
                if !rest.split(' ').all(is_word) {
                    return None;
                }
                return Deadline::parse(rest).map(Self::Deadline);
            }
            "gone" => {
                let off = number(words.next()?)?;
                let sid = words.next().filter(|w| is_word(w))?;
                Self::DeadlineGone {
                    off,
                    sid: sid.to_string(),
                    how: None,
                }
            }
            "handed" => Self::Handed(number(words.next()?)?),
            "took" => {
                let asked = number(words.next()?)?;
                let deadlines = number(words.next()?)?;
                Self::Took(Took { asked, deadlines })
            }
            _ => return None,
        };
        words.next().is_none().then_some(frame)
    }
}

impl Record for Frame {
    const JOURNAL: &'static str = "asks journal";

    fn body(&self) -> String {
        match self {
            Self::Asked(asked) | Self::AskedAt { asked, .. } => format!("asked {}", asked.render()),
            Self::Deadline(deadline) | Self::DeadlineAt { deadline, .. } => {
                format!("deadline {}", deadline.render())
            }
            Self::DeadlineGone { off, sid, .. } => format!("gone {off} {sid}"),
            Self::Handed(n) => format!("handed {n}"),
            Self::Took(took) => format!("took {} {}", took.asked, took.deadlines),
        }
    }

    /// A settle's verdict ([`Frame::DeadlineGone`]'s `how`), and the landing's sequence of a
    /// recipient or a deadline noted at one ([`Frame::AskedAt`], [`Frame::DeadlineAt`]); no
    /// other fact has more than its body says.
    fn tail(&self) -> String {
        match self {
            Self::DeadlineGone { how: Some(how), .. } => how.words(),
            Self::AskedAt { seq, .. } | Self::DeadlineAt { seq, .. } => seq_words(*seq),
            _ => String::new(),
        }
    }

    /// Its body, a settle's verdict where its tail names one, and a recipient's or a deadline's
    /// landing where its tail names that; a tail this build cannot read adds nothing.
    fn of_record(body: &[u8], tail: &[u8]) -> Option<Self> {
        let frame = Self::parse(body)?;
        Some(match frame {
            Self::DeadlineGone { off, sid, .. } => Self::DeadlineGone {
                off,
                sid,
                how: Verdict::of_words(tail),
            },
            Self::Asked(asked) => match seq_of_words(tail) {
                Some(seq) => Self::AskedAt { asked, seq },
                None => Self::Asked(asked),
            },
            Self::Deadline(deadline) => match seq_of_words(tail) {
                Some(seq) => Self::DeadlineAt { deadline, seq },
                None => Self::Deadline(deadline),
            },
            frame => frame,
        })
    }
}

/// What the journal holds: each ask's recipient, each deadline owed a verdict, each deadline
/// settled, the last hand-off to the tables a build before the journal reads, and what it took
/// from those tables since.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub(crate) struct Index {
    /// Each ask's recipient, by the ask's offset: at most [`ASKED_KEEP`], the lowest offsets
    /// going first, as the bridge's own table keeps them.
    pub(crate) asked: BTreeMap<u64, String>,
    /// Each deadline owed a verdict, by the ask's offset: at most [`DEADLINES_KEEP`], the lowest
    /// offsets going first.
    pub(crate) deadlines: BTreeMap<u64, Deadline>,
    /// Each deadline settled, its asker by the ask's offset, with the verdict of this node's
    /// `expired` of it where one was judged ([`Frame::DeadlineGone`]): at most
    /// [`DEADLINES_KEEP`], the lowest offsets going first.
    pub(crate) gone: BTreeMap<u64, (String, Option<Verdict>)>,
    /// The last hand-off ([`Frame::Handed`]); `None` where no stop handed the index back.
    pub(crate) handed: Option<u64>,
    /// What an open last took from the tables, where no hand-off replaced them since
    /// ([`Frame::Took`]).
    pub(crate) took: Option<Took>,
    /// A fact was recorded after the last hand-off and the last taking-in, which a build before
    /// the journal that ran since never saw.
    pub(crate) noted: bool,
    /// WHICH POST'S LANDING EACH ASK IS (ATERM-KI-1116-7314): the in-flight sequence whose
    /// landing noted the recipient or the deadline at an offset ([`Frame::AskedAt`],
    /// [`Frame::DeadlineAt`]), kept while [`Index::asked`] or [`Index::deadlines`] holds the
    /// offset, so past the ask's settle while its recipient stands, and written again with them
    /// at every rewrite. A row noted with none (a build before it, a table taken in) names none,
    /// and takes out what it replaces.
    pub(crate) landings: BTreeMap<u64, u64>,
}

impl Replay for Index {
    type Record = Frame;

    fn apply(&mut self, frame: Frame) {
        match frame {
            Frame::Asked(Asked { off, to }) => {
                if self.asked.get(&off) != Some(&to) {
                    self.landings.remove(&off);
                }
                self.ask(off, to);
            }
            Frame::AskedAt {
                asked: Asked { off, to },
                seq,
            } => {
                self.landings.insert(off, seq);
                self.ask(off, to);
            }
            Frame::Deadline(deadline) => {
                if self.deadlines.get(&deadline.off) != Some(&deadline) {
                    self.landings.remove(&deadline.off);
                }
                self.arm(deadline);
            }
            Frame::DeadlineAt { deadline, seq } => {
                self.landings.insert(deadline.off, seq);
                self.arm(deadline);
            }
            Frame::DeadlineGone { off, sid, how } => {
                if self.deadlines.get(&off).is_some_and(|d| d.sid == sid) {
                    self.deadlines.remove(&off);
                    if !self.asked.contains_key(&off) {
                        self.landings.remove(&off);
                    }
                }
                // A settle that names no verdict keeps the one a settle of the same ask named.
                let how = how.or_else(|| {
                    self.gone
                        .get(&off)
                        .filter(|(asker, _)| *asker == sid)
                        .and_then(|(_, how)| *how)
                });
                self.gone.insert(off, (sid, how));
                while self.gone.len() > DEADLINES_KEEP {
                    self.gone.pop_first();
                }
                self.noted = true;
            }
            Frame::Handed(n) => {
                self.handed = Some(n);
                self.took = None;
                self.noted = false;
            }
            Frame::Took(took) => {
                self.took = Some(took);
                self.noted = false;
            }
        }
    }
}

impl Index {
    /// Nothing asked, no deadline, nothing settled, no hand-off, nothing taken in.
    pub(crate) const EMPTY: Self = Self {
        asked: BTreeMap::new(),
        deadlines: BTreeMap::new(),
        gone: BTreeMap::new(),
        handed: None,
        took: None,
        noted: false,
        landings: BTreeMap::new(),
    };

    /// The ask at `off` went to `to`, the lowest offsets going past [`ASKED_KEEP`], each with
    /// its landing where no deadline holds that offset.
    fn ask(&mut self, off: u64, to: String) {
        self.asked.insert(off, to);
        while self.asked.len() > ASKED_KEEP {
            if let Some((trimmed, _)) = self.asked.pop_first() {
                if !self.deadlines.contains_key(&trimmed) {
                    self.landings.remove(&trimmed);
                }
            }
        }
        self.noted = true;
    }

    /// `deadline` armed, the lowest offsets going past [`DEADLINES_KEEP`], each with its landing
    /// where no recipient holds that offset.
    fn arm(&mut self, deadline: Deadline) {
        self.gone.remove(&deadline.off);
        self.deadlines.insert(deadline.off, deadline);
        while self.deadlines.len() > DEADLINES_KEEP {
            if let Some((trimmed, _)) = self.deadlines.pop_first() {
                if !self.asked.contains_key(&trimmed) {
                    self.landings.remove(&trimmed);
                }
            }
        }
        self.noted = true;
    }

    /// Whether the deadline of `sid`'s ask at `off` is remembered settled.
    pub(crate) fn settled(&self, off: u64, sid: &str) -> bool {
        self.gone.get(&off).is_some_and(|(asker, _)| asker == sid)
    }

    /// EVERY OTHER LANDING OF THE POST IN FLIGHT AT SEQUENCE `seq` THE JOURNAL HOLDS AN ASK OF
    /// (ATERM-KI-1116-7314, [`Index::landings`]): each offset at or above `floor`, but `off`,
    /// whose recipient or deadline a landing of that sequence noted, lowest first. A keyed
    /// re-post shares its key's sequence with every re-post of the key, each published below
    /// the next one's own head, so `floor`, that head, leaves the earlier ones' out.
    pub(crate) fn landed_elsewhere(&self, seq: u64, floor: u64, off: u64) -> Vec<u64> {
        self.landings
            .range(floor..)
            .filter(|&(&at, &of)| of == seq && at != off)
            .map(|(&at, _)| at)
            .collect()
    }

    /// Every verdict of this node's `expired` the settles name, oldest ask first: `(ask, asker,
    /// verdict)`.
    pub(crate) fn verdict_rows(&self) -> Vec<(u64, String, Verdict)> {
        self.gone
            .iter()
            .filter_map(|(off, (sid, how))| how.map(|how| (*off, sid.clone(), how)))
            .collect()
    }

    /// Every ask's recipient, oldest offset first.
    pub(crate) fn asked_rows(&self) -> Vec<Asked> {
        self.asked
            .iter()
            .map(|(off, to)| Asked {
                off: *off,
                to: to.clone(),
            })
            .collect()
    }

    /// Every deadline owed a verdict, oldest offset first.
    pub(crate) fn deadline_rows(&self) -> Vec<Deadline> {
        self.deadlines.values().cloned().collect()
    }

    /// Keep only what an offset `kept` takes: a log reset's clamp
    /// ([`super::StateDir::forget_log_offsets`]). A settle kept loses its verdict where the
    /// `expired` it names, or the answer a moot one names, is past the fork
    /// (ATERM-KI-1115-7889), as the bridge forgets it in memory (`Bridge::forget_log_offsets`):
    /// restored at the next open, an offset of the old log read as one of the new would mark an
    /// answer there `late=1`, or name it the answer an `expired` is moot behind.
    pub(crate) fn retain(&mut self, kept: impl Fn(u64) -> bool) {
        self.asked.retain(|off, _| kept(*off));
        self.deadlines.retain(|off, _| kept(*off));
        self.gone.retain(|off, _| kept(*off));
        self.landings.retain(|off, _| kept(*off));
        for (_, how) in self.gone.values_mut() {
            let past = how.is_some_and(|verdict| match verdict {
                Verdict::Moot { at, by } => !kept(at) || !kept(by),
                Verdict::Expired { at } | Verdict::Unjudged { at } => !kept(at),
            });
            if past {
                *how = None;
            }
        }
    }

    /// RECONCILE THE INDEX WITH THE TABLES AS AN OPEN FINDS THEM, `None` where there is no file,
    /// and record what it took ([`Frame::Took`]): each by [`Index::word_of`]. `damaged` says the
    /// records may have lost what a table still holds. A table that [`Word::Fills`] takes every
    /// row but a deadline the index remembers settled; one that [`Word::Replaces`] stands in
    /// for what the index says of it, and a deadline table for what it remembers settled too:
    /// that memory can name an offset of a log the table's writer has left, where a settle
    /// would keep a new ask's deadline out. A deadline that writer armed again is swept, which
    /// reads the answer back from the bus. Whether anything was taken: the journal is then to be
    /// rewritten, which is what makes the taking stand.
    pub(crate) fn take_in(
        &mut self,
        asked: Option<&str>,
        deadlines: Option<&str>,
        damaged: bool,
    ) -> bool {
        let word = |index: &Self, text: Option<&str>, taken: Option<u64>| {
            text.and_then(|text| index.word_of(text, taken, damaged))
        };
        let asked_word = word(self, asked, self.took.map(|took| took.asked));
        let deadlines_word = word(self, deadlines, self.took.map(|took| took.deadlines));
        if asked_word.is_none() && deadlines_word.is_none() {
            return false;
        }
        if let (Some(word), Some(text)) = (asked_word, asked) {
            self.take_asked(text, word);
        }
        if let (Some(word), Some(text)) = (deadlines_word, deadlines) {
            self.take_deadlines(text, word);
        }
        let (recipients, armed) = (&self.asked, &self.deadlines);
        self.landings
            .retain(|off, _| recipients.contains_key(off) || armed.contains_key(off));
        self.apply(Frame::Took(Took {
            asked: digest(asked.unwrap_or_default()),
            deadlines: digest(deadlines.unwrap_or_default()),
        }));
        true
    }

    /// How an open reads a table's `text`, `None` where it holds nothing the index lacks:
    ///
    /// - DAMAGED RECORDS trust no line: the table fills in what they lost, whatever hand-off it
    ///   names, since the records of the rows it holds may be what the damage took.
    /// - A TABLE THIS INDEX TOOK (`taken`, its digest) or that a hand-off of its own wrote, or
    ///   an earlier one's, whose write a power cut undid, says nothing newer.
    /// - A LATER HAND-OFF, whose record a power cut lost, fills in.
    /// - NO HAND-OFF NAMED is a build before the journal's word. With nothing noted since the
    ///   journal's last hand-off or taking-in, that build saw everything here, so the table
    ///   replaces it. With something noted, a kill came first and that build saw less: the table
    ///   fills in, so nothing noted is lost, and a deadline that build settled stays until the
    ///   sweep reads its answer back. A move of the log that build met leaves the noted rows
    ///   behind the same way. A hand-off's table a power cut tore to nothing reads as that
    ///   build's empty one, as that build itself would read it.
    fn word_of(&self, text: &str, taken: Option<u64>, damaged: bool) -> Option<Word> {
        if damaged {
            return Some(Word::Fills);
        }
        if taken == Some(digest(text)) {
            return None;
        }
        match legacy_mark(text) {
            Some(mark) if self.handed.is_some_and(|handed| mark <= handed) => None,
            Some(_) => Some(Word::Fills),
            None if self.noted => Some(Word::Fills),
            None => Some(Word::Replaces),
        }
    }

    fn take_asked(&mut self, text: &str, word: Word) {
        if word == Word::Replaces {
            self.asked.clear();
        }
        for row in text.lines().filter_map(Asked::parse) {
            let frame = Frame::Asked(row);
            if frame.carried() {
                self.apply(frame);
            }
        }
    }

    fn take_deadlines(&mut self, text: &str, word: Word) {
        if word == Word::Replaces {
            self.deadlines.clear();
            self.gone.clear();
        }
        for row in text.lines().filter_map(Deadline::parse) {
            if self.settled(row.off, &row.sid) {
                continue;
            }
            let frame = Frame::Deadline(row);
            if frame.carried() {
                self.apply(frame);
            }
        }
    }

    /// The journal that holds exactly this index, its steps from 0: every ask's recipient, every
    /// deadline and every deadline settled, and the last hand-off and taking-in on the side of
    /// them that keeps [`Index::noted`] as it is. Returns the bytes and the step after the last.
    ///
    /// # Errors
    /// A record that would not read back, or one past a step record's payload cap
    /// ([`journal::encode`]).
    pub(crate) fn encode(&self) -> io::Result<(Vec<u8>, u64)> {
        let asked =
            self.asked_rows()
                .into_iter()
                .map(|asked| match self.landings.get(&asked.off) {
                    Some(&seq) => Frame::AskedAt { asked, seq },
                    None => Frame::Asked(asked),
                });
        let deadlines = self.deadlines.values().cloned().map(|deadline| {
            match self.landings.get(&deadline.off) {
                Some(&seq) => Frame::DeadlineAt { deadline, seq },
                None => Frame::Deadline(deadline),
            }
        });
        let gone = self
            .gone
            .iter()
            .map(|(off, (sid, how))| Frame::DeadlineGone {
                off: *off,
                sid: sid.clone(),
                how: *how,
            });
        let rows: Vec<Frame> = asked.chain(deadlines).chain(gone).collect();
        let marks = self
            .handed
            .map(Frame::Handed)
            .into_iter()
            .chain(self.took.map(Frame::Took));
        let frames: Vec<Frame> = if self.noted {
            marks.chain(rows).collect()
        } else {
            rows.into_iter().chain(marks).collect()
        };
        encode_from(0, &frames)
    }
}

/// The records of `frames` from `step`, for one append: the bytes and the step after the last.
///
/// # Errors
/// A record that would not read back, or one past a step record's payload cap
/// ([`journal::encode`]).
pub(crate) fn encode_from(mut step: u64, frames: &[Frame]) -> io::Result<(Vec<u8>, u64)> {
    let mut bytes = Vec::new();
    for frame in frames {
        bytes.extend_from_slice(&journal::encode(step, frame)?);
        step += 1;
    }
    Ok((bytes, step))
}

/// Read the journal's bytes ([`journal::read_as`]).
#[must_use]
pub(crate) fn read(bytes: &[u8]) -> journal::Read<Index> {
    journal::read_as(bytes)
}

/// A table handed back as hand-off `handed`: the line that names it, then one line a row. A
/// build before the journal parses each line as a row and skips the first, which no row's parse
/// takes ([`Asked::parse`], [`Deadline::parse`]).
pub(crate) fn legacy_text(handed: u64, rows: impl IntoIterator<Item = String>) -> String {
    let mut lines = vec![format!("{MARK}{handed}")];
    lines.extend(rows);
    lines.join("\n")
}

/// The hand-off a table's opening line names, `None` for a table a build before the journal
/// wrote (it writes rows alone).
pub(crate) fn legacy_mark(text: &str) -> Option<u64> {
    number(text.lines().next()?.trim().strip_prefix(MARK)?)
}

#[cfg(test)]
#[path = "asks_tests.rs"]
mod tests;
