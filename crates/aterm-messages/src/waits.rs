// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! THE WAITS A PERSON SITS THROUGH INSIDE A SESSION (design rulings 231–234):
//! a large paste on its way to a program, and the scrollback rewrap that
//! follows a width change. Both are real waits — the person's keys queue
//! behind the paste (Ctrl-C included: paste isolation keeps submission order),
//! and the history is gone from scrollback and search until the rewrap
//! re-attaches it — and neither said why.
//!
//! The policy is here, once, for every platform: which of them takes a row
//! ([`Waiter`]: who waits), the words, the rows, and one clockless state
//! machine ([`SessionWait`]) a host drives with a sample a few times a second
//! ([`WAIT_SAMPLE`]). The host only measures (bytes a writer has handed the
//! program, lines a worker has rewrapped), says whether the person is looking
//! or asking, and performs what a step returns. No clock is read here: the
//! host passes how long the work has run, and the row's grace counts from its
//! start ([`crate::PROGRESS_GRACE`]), so quick work never reaches the glass.

use crate::center::EchoKind;
use crate::model::{Amount, Glyph, Hold, Intent, Message, Meter, Severity, Unit, tags};
use crate::progress::Waiter;
use crate::{Duration, PROGRESS_GRACE, REVEAL_DEFER_MAX, REVEAL_MIN_LEFT};

/// A paste this large or larger is watched; a smaller one never takes a row
/// (it is delivered in a few `write(2)`s, and a row for it would only ever
/// sit behind the grace). 64 KiB is eight of the tty's 8 KiB input buffers,
/// and at the line editors' measured pace the first size that can outlast the
/// grace.
pub const LARGE_PASTE_BYTES: u64 = 64 * 1024;

/// How often a host samples a watched wait while its row is on the glass: a
/// few times a second, never per byte or per line.
pub const WAIT_SAMPLE: Duration = Duration::from_millis(250);

/// How often it samples a wait with no row (its session off screen, or a
/// rewrap nobody asked after): only to notice the row becoming wanted, or the
/// work ending.
pub const WAIT_SAMPLE_IDLE: Duration = Duration::from_secs(1);

/// A watched row's staleness cap. Every sample restates it (a restate that
/// changes nothing still re-arms the cap), so it lapses only when the host
/// stops sampling.
pub const STALE_WAIT: Duration = Duration::from_secs(30);

/// The paste row's words while it runs: `Pasting`, and its finished words.
pub const PASTING: &str = "Pasting";

/// How long a paste's program may take none of it before the row says so
/// (round 18, day four, D8: `Pasting 12 MB 0%` stood 37 s with no word that
/// it waited on the program, which had stopped reading). A few of the
/// paste's own samples: a program that reads in bursts is never called
/// stopped between two of them.
pub(crate) const PASTE_UNREAD_AFTER: Duration = Duration::from_secs(3);
/// The rewrap row's title before it knows how many lines it rewraps
/// ([`rewrap_title`] says them once it does).
pub const REWRAPPING: &str = "Rewrapping scrollback";
/// The rewrap row's finished words.
pub const REWRAPPED: &str = "Scrollback rewrapped";

/// Which wait a [`SessionWait`] watches.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum WaitKind {
    /// A large paste going to the session's program.
    Paste,
    /// The session's scrollback being rewrapped after a width change.
    Rewrap,
}

impl WaitKind {
    /// Whether the wait takes a row: its WAITER decides (ruling 220). A
    /// paste's person is waiting while its session is on screen; a rewrap's
    /// only once they scroll into the missing history or search it.
    #[must_use]
    pub const fn waiter(asked: bool) -> Waiter {
        Waiter::of(asked)
    }
}

/// The row's supersede key: one per session and wait.
#[must_use]
pub fn wait_key(kind: WaitKind, session: u64) -> String {
    match kind {
        WaitKind::Paste => format!("session.paste.{session}"),
        WaitKind::Rewrap => format!("session.rewrap.{session}"),
    }
}

/// Whether `key` is a session wait's row key ([`wait_key`]).
#[must_use]
pub fn is_wait_key(key: &str) -> bool {
    key.starts_with("session.paste.") || key.starts_with("session.rewrap.")
}

/// A paste's size in words, decimal like every download dialog, with one
/// decimal below 10 MB where the paste sizes live: `64 KB`, `812 KB`,
/// `4.2 MB`, `16 MB`. A figure that would round to 1000 of a unit is the next
/// unit up.
#[must_use]
pub fn size_words(bytes: u64) -> String {
    const KB: u64 = 1_000;
    const MB: u64 = 1_000_000;
    const GB: u64 = 1_000_000_000;
    // Tenths of the unit, rounded half up, in integers.
    let tenths = |unit: u64| (bytes.saturating_mul(10) + unit / 2) / unit;
    if bytes >= GB - MB / 2 {
        let t = tenths(GB);
        format!("{}.{} GB", t / 10, t % 10)
    } else if bytes >= 10 * MB - MB / 20 {
        format!("{} MB", (bytes + MB / 2) / MB)
    } else if bytes >= MB - KB / 2 {
        let t = tenths(MB);
        format!("{}.{} MB", t / 10, t % 10)
    } else if bytes >= KB {
        format!("{} KB", (bytes + KB / 2) / KB)
    } else {
        format!("{bytes} B")
    }
}

/// `done / total` bytes of a paste as stats, `done` right-aligned to the
/// total's width, so the row lays out once and the digits tick in place:
/// `1.1 MB / 4.2 MB`, `812 KB / 4.2 MB`.
#[must_use]
pub fn paste_stats(done: u64, total: u64) -> String {
    let (done, total) = (size_words(done.min(total)), size_words(total));
    let width = total.chars().count();
    format!("{done:>width$} / {total}")
}

/// A count of lines in a few characters: `950`, `12K`, `340K`, `1.2M`,
/// `12M`.
#[must_use]
pub fn lines_words(n: u64) -> String {
    const K: u64 = 1_000;
    const M: u64 = 1_000_000;
    if n >= 10 * M - M / 20 {
        format!("{}M", (n + M / 2) / M)
    } else if n >= M - K / 2 {
        let t = (n.saturating_mul(10) + M / 2) / M;
        format!("{}.{}M", t / 10, t % 10)
    } else if n >= 10 * K {
        format!("{}K", (n + K / 2) / K)
    } else if n >= K {
        let t = (n.saturating_mul(10) + K / 2) / K;
        format!("{}.{}K", t / 10, t % 10)
    } else {
        n.to_string()
    }
}

/// A rewrap's stats: `1.2M of 3.4M lines`, `done` right-aligned to the
/// total's width. The glass never paints them (the title states the total,
/// ruling 246), so they need no short form: the description and Details
/// keep them whole.
#[must_use]
pub fn rewrap_stats(done: u64, total: u64) -> String {
    let (done, total) = (lines_words(done.min(total)), lines_words(total));
    let width = total.chars().count();
    format!("{done:>width$} of {total} lines")
}

/// The paste row: `Pasting 4.2 MB`, filled by the bytes the program has
/// taken, `1.1 MB / 4.2 MB`, the engine's ETA from their rate, and ONE
/// capsule, `Stop paste`. Revealed `grace_left` after it is posted — the
/// grace counts from the paste's start, not from the post — so a paste that
/// ends inside [`PROGRESS_GRACE`] never reaches the glass. It ends
/// `Pasted 4.2 MB`.
#[must_use]
pub fn paste_row(
    session: u64,
    series: u64,
    done: u64,
    total: u64,
    grace_left: Duration,
) -> Message {
    let size = size_words(total);
    let done = done.min(total);
    Message::new(tags::SESSION, Severity::Info, format!("{PASTING} {size}"))
        .key(&wait_key(WaitKind::Paste, session))
        .glyph(Glyph::or_fallback('\u{21e3}'))
        .hold(Hold::Live {
            stale_after: STALE_WAIT,
        })
        .meter(Meter {
            stats: paste_stats(done, total),
            amount: Some(Amount {
                series,
                done,
                total,
                unit: Unit::Bytes,
            }),
            ..Meter::default()
        })
        .action(Intent::StopPaste { session })
        .no_excerpt()
        .finished_as(format!("Pasted {size}"))
        .reveal_after(grace_left)
}

/// The rewrap row's title: the size of the whole wait, said once — `Rewrapping
/// 3.4M lines` (design ruling 246: a percent row's stats carry only the job's
/// size, and here the title already does) — or [`REWRAPPING`] before the
/// count is known.
#[must_use]
pub fn rewrap_title(total: u64) -> String {
    match total {
        0 => REWRAPPING.to_string(),
        1 => "Rewrapping 1 line".to_string(),
        n => format!("Rewrapping {} lines", lines_words(n)),
    }
}

/// The rewrap row: `Rewrapping 3.4M lines`, filled by the lines rewrapped of
/// the lines detached, and the ETA. Its stats (`1.2M of 3.4M lines`) ride the
/// description and Details; the glass paints none, since the title states the
/// total (ruling 246). No capsule: there is nothing to decide (a stopped
/// rewrap would lose the history). It ends `Scrollback rewrapped`.
#[must_use]
pub fn rewrap_row(session: u64, done: u64, total: u64, grace_left: Duration) -> Message {
    let done = done.min(total);
    Message::new(tags::SESSION, Severity::Info, rewrap_title(total))
        .key(&wait_key(WaitKind::Rewrap, session))
        .glyph(Glyph::or_fallback('\u{21bb}'))
        .hold(Hold::Live {
            stale_after: STALE_WAIT,
        })
        .meter(Meter {
            stats: rewrap_stats(done, total),
            amount: Some(Amount {
                series: Amount::series_of(&wait_key(WaitKind::Rewrap, session)),
                done,
                total: total.max(1),
                unit: Unit::Items,
            }),
            ..Meter::default()
        })
        .no_excerpt()
        .finished_as(REWRAPPED)
        .reveal_after(grace_left)
}

/// How a watched wait ended.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum WaitEnd {
    /// Delivered: the paste's bytes all went, the history re-attached.
    Done,
    /// The person stopped it (`Stop paste`).
    Stopped,
    /// Its session went away.
    Gone,
    /// It failed: the history could not be rewrapped and was lost.
    Failed,
}

impl WaitEnd {
    /// The row's echo: Complete for delivered work (its finished words);
    /// a fade for a stop the person pressed and a session that is gone
    /// (there is nothing left to claim); the warn flash for a failure.
    #[must_use]
    pub const fn echo(self) -> EchoKind {
        match self {
            Self::Done => EchoKind::Complete,
            Self::Stopped | Self::Gone => EchoKind::Vanish,
            Self::Failed => EchoKind::Fault,
        }
    }
}

/// One sample of a watched wait, measured by the host.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct WaitSample {
    /// Bytes delivered, or lines rewrapped.
    pub done: u64,
    /// Bytes of the paste, or lines detached.
    pub total: u64,
    /// How long the work has run.
    pub elapsed: Duration,
    /// Whether the person waits on it now: the paste's session is on
    /// screen; the person scrolled into the rewrap's missing history or
    /// searched it.
    pub asked: bool,
    /// `Some` once it is over.
    pub end: Option<WaitEnd>,
}

/// What the host does with a sample.
#[derive(Clone, Debug, PartialEq)]
pub enum WaitStep {
    /// Nothing.
    Idle,
    /// Post this row (it takes its grace from the work's start).
    Post(Message),
    /// Restate the posted row with these words.
    Restate(Message),
    /// End the posted row with this echo.
    End(EchoKind),
}

/// One session's watched wait, clockless: [`SessionWait::step`] turns each
/// sample into the one thing to do. A row is posted once the person waits
/// ([`WaitKind::waiter`]), restated on every later sample (its fill, stats and
/// ETA; the restate re-arms its staleness cap) and ended with the end's echo.
/// Once up it stays up until the work ends — a person who switched tabs is
/// still waiting for what they started, and a row that came and went with
/// every tab switch would be motion for nothing. A wait that ends with no row
/// posted ends silently.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct SessionWait {
    kind: WaitKind,
    session: u64,
    series: u64,
    posted: bool,
    /// The person waits on it but its row is held back: the work projects
    /// to end before it could be read (ruling 265). Sampled at the row's
    /// pace, so the row comes the moment the projection says it should.
    held: bool,
    /// The first sample this wait saw — `done` and the elapsed time — the
    /// base its own pace is measured from (a rewrap is watched from the ask,
    /// long after its work began).
    first: Option<(u64, Duration)>,
    /// The last sample whose `done` moved: its `done` and elapsed time —
    /// how long the program has taken none of a paste
    /// ([`PASTE_UNREAD_AFTER`]).
    moved: Option<(u64, Duration)>,
}

impl SessionWait {
    /// A paste into `session`; `series` names this paste (a new paste is a
    /// new estimator series).
    #[must_use]
    pub const fn paste(session: u64, series: u64) -> Self {
        Self {
            kind: WaitKind::Paste,
            session,
            series,
            posted: false,
            held: false,
            first: None,
            moved: None,
        }
    }

    /// A rewrap of `session`'s scrollback.
    #[must_use]
    pub const fn rewrap(session: u64) -> Self {
        Self {
            kind: WaitKind::Rewrap,
            session,
            series: 0,
            posted: false,
            held: false,
            first: None,
            moved: None,
        }
    }

    /// Which wait this is.
    #[must_use]
    pub const fn kind(&self) -> WaitKind {
        self.kind
    }

    /// Whether its row is posted (possibly still inside its grace).
    #[must_use]
    pub const fn posted(&self) -> bool {
        self.posted
    }

    /// The row's words for `s`. A paste whose program has taken none of it
    /// for [`PASTE_UNREAD_AFTER`] says so, and for how long, as its excerpt
    /// — what it waits on, so the person knows the wait is the program's and
    /// that `Stop paste` ends it.
    #[must_use]
    pub fn row(&self, s: &WaitSample) -> Message {
        let grace_left = PROGRESS_GRACE.saturating_sub(s.elapsed);
        match self.kind {
            WaitKind::Paste => {
                let row = paste_row(self.session, self.series, s.done, s.total, grace_left);
                match self.unread_for(s) {
                    Some(stuck) => {
                        let mut row = row.line(unread_words(stuck));
                        row.excerpt = true;
                        row
                    }
                    None => row,
                }
            }
            WaitKind::Rewrap => rewrap_row(self.session, s.done, s.total, grace_left),
        }
    }

    /// How long the program has taken none of this paste, once that is
    /// [`PASTE_UNREAD_AFTER`] or more and some of it is still unsent.
    fn unread_for(&self, s: &WaitSample) -> Option<Duration> {
        let (done, at) = self.moved?;
        let stuck = s.elapsed.saturating_sub(at);
        (s.done <= done && s.done < s.total && stuck >= PASTE_UNREAD_AFTER).then_some(stuck)
    }

    /// The one thing to do with sample `s`.
    pub fn step(&mut self, s: &WaitSample) -> WaitStep {
        let first = *self.first.get_or_insert((s.done, s.elapsed));
        if self.moved.is_none_or(|(done, _)| s.done > done) {
            self.moved = Some((s.done, s.elapsed));
        }
        if let Some(end) = s.end {
            return if std::mem::take(&mut self.posted) {
                WaitStep::End(end.echo())
            } else {
                WaitStep::Idle
            };
        }
        if self.posted {
            WaitStep::Restate(self.row(s))
        } else if WaitKind::waiter(s.asked).takes_row(false) {
            self.held = ends_unread(first, s);
            if self.held {
                return WaitStep::Idle;
            }
            self.posted = true;
            WaitStep::Post(self.row(s))
        } else {
            WaitStep::Idle
        }
    }

    /// The next sample falls due this long after one: [`WAIT_SAMPLE`] while
    /// a row is up, [`WAIT_SAMPLE_IDLE`] while none is.
    #[must_use]
    pub const fn sample_every(&self) -> Duration {
        if self.posted || self.held {
            WAIT_SAMPLE
        } else {
            WAIT_SAMPLE_IDLE
        }
    }
}

/// NO FLASH (design ruling 265): whether the work in `s`, projected at the
/// pace it kept since `first` (the first sample this wait saw), ends before
/// its row could be read — inside the rest of its grace plus
/// [`REVEAL_MIN_LEFT`]. Held back so, the row is never posted and the wait
/// ends silently; past [`PROGRESS_GRACE`] plus [`REVEAL_DEFER_MAX`] it is
/// posted whatever the projection says. No pace yet projects nothing: the
/// row is posted (and the engine's reveal looks again at the grace's end).
fn ends_unread((done0, at0): (u64, Duration), s: &WaitSample) -> bool {
    if s.elapsed >= PROGRESS_GRACE + REVEAL_DEFER_MAX || s.done <= done0 || s.total <= s.done {
        return false;
    }
    let span = s.elapsed.saturating_sub(at0);
    let span = u128::from(u64::try_from(span.as_millis()).unwrap_or(u64::MAX));
    let left_ms = span * u128::from(s.total - s.done) / u128::from(s.done - done0);
    let left = Duration::from_millis(u64::try_from(left_ms).unwrap_or(u64::MAX));
    left < PROGRESS_GRACE.saturating_sub(s.elapsed) + REVEAL_MIN_LEFT
}

/// A paste's excerpt while its program takes none of it: `not read for 37
/// s` — short enough that its figure survives at 60 columns beside the
/// title, the percent and `Stop` (ruling 316, day eight E4: `program not
/// reading for 37 s` kept its figure at 100 columns only and read `program
/// not reading…` at 60; live, the longer `the program has read nothing for
/// 13…` lost it at 100). The title names the paste, so what was not read is
/// the paste, and `Stop paste` is still what ends the wait.
fn unread_words(stuck: Duration) -> String {
    format!("not read for {}", crate::strain::span_words(stuck))
}

/// Whether a paste of `bytes` is watched at all ([`LARGE_PASTE_BYTES`]).
#[must_use]
pub const fn paste_is_watched(bytes: u64) -> bool {
    bytes >= LARGE_PASTE_BYTES
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::text::{glass_title_fault, title_words};

    fn sample(done: u64, total: u64, ms: u64, asked: bool) -> WaitSample {
        WaitSample {
            done,
            total,
            elapsed: Duration::from_millis(ms),
            asked,
            end: None,
        }
    }

    /// A PASTE THE PROGRAM IS NOT READING SAYS SO (round 18, day four, D8:
    /// `Pasting 12 MB 0%` for 37 s, no word of what it waited on). Once the
    /// program has taken none of it for [`PASTE_UNREAD_AFTER`], the row's
    /// excerpt names the program and how long; the words go as soon as it
    /// reads again. NEGATIVE CONTROL: a paste that keeps moving, and one
    /// stuck for less than the bound, carry no excerpt.
    #[test]
    fn a_paste_the_program_does_not_read_says_what_it_waits_on() {
        let mut w = SessionWait::paste(3, 1);
        let total = 12_000_000;
        let WaitStep::Post(row) = w.step(&sample(0, total, 2_000, true)) else {
            panic!("posted");
        };
        assert!(!row.excerpt && row.detail.is_empty(), "{row:?}");
        let WaitStep::Restate(row) = w.step(&sample(0, total, 4_900, true)) else {
            panic!("restated");
        };
        assert!(!row.excerpt, "under the bound: {row:?}");
        let WaitStep::Restate(row) = w.step(&sample(0, total, 39_000, true)) else {
            panic!("restated");
        };
        assert!(row.excerpt);
        assert_eq!(row.detail, ["not read for 37 s"]);
        assert_eq!(row.title, "Pasting 12 MB", "the title is the job's");
        // Ruling 316 (day eight E4): the figure survives at 60 columns,
        // beside the percent and the capsule, on the widest paste title and
        // the widest span words; `program not reading for 37 s` lost it.
        let figure_at = |row: &Message, cols: usize| {
            let now = crate::Instant::now();
            let mut c = crate::center::MessageCenter::new(crate::log::MessageLog::empty(), now);
            let _ = c.post(row.clone(), crate::model::WallStamp { unix_ms: 1_000 }, now);
            let _ = c.settle(now, true);
            c.commit_rows(now, 3);
            let p = c.presentation(
                cols,
                &crate::text::char_width,
                None,
                crate::glass::Links::Painted,
            );
            p.rows
                .first()
                .and_then(|r| r.detail.clone())
                .map(|(_, d)| d)
        };
        let mut big = SessionWait::paste(3, 9);
        let _ = big.step(&sample(0, 999_000_000, 2_000, true));
        let WaitStep::Restate(wide) = big.step(&sample(0, 999_000_000, 3_601_000, true)) else {
            panic!("restated");
        };
        assert_eq!(wide.title, "Pasting 999 MB");
        assert_eq!(
            figure_at(&wide, 60).as_deref(),
            Some("not read for 59m 59s")
        );
        let mut old = row.clone();
        old.detail = vec!["program not reading for 37 s".to_string()];
        assert_ne!(
            figure_at(&old, 60).as_deref(),
            Some("program not reading for 37 s"),
            "the control: the old words are cut at 60"
        );
        // It reads again: the words go at once.
        let WaitStep::Restate(row) = w.step(&sample(1_000_000, total, 39_250, true)) else {
            panic!("restated");
        };
        assert!(!row.excerpt && row.detail.is_empty(), "{row:?}");
        // A steady paste never says it.
        let mut steady = SessionWait::paste(3, 2);
        for i in 0..40u64 {
            let step = steady.step(&sample(i * 100_000, total, 2_000 + i * 250, true));
            if let WaitStep::Post(row) | WaitStep::Restate(row) = step {
                assert!(!row.excerpt, "{i}: {row:?}");
            }
        }
    }

    #[test]
    fn the_words_are_terse_and_tick_in_place() {
        assert_eq!(size_words(65_536), "66 KB");
        assert_eq!(size_words(812_000), "812 KB");
        assert_eq!(size_words(4_200_000), "4.2 MB");
        assert_eq!(size_words(999_700), "1.0 MB");
        assert_eq!(size_words(16 * 1024 * 1024), "17 MB");
        assert_eq!(paste_stats(1_100_000, 4_200_000), "1.1 MB / 4.2 MB");
        assert_eq!(paste_stats(812_000, 4_200_000), "812 KB / 4.2 MB");
        assert_eq!(paste_stats(0, 4_200_000), "   0 B / 4.2 MB");
        assert_eq!(lines_words(950), "950");
        assert_eq!(lines_words(12_345), "12K");
        assert_eq!(lines_words(1_234), "1.2K");
        assert_eq!(lines_words(1_200_000), "1.2M");
        assert_eq!(lines_words(3_400_000), "3.4M");
        assert_eq!(lines_words(12_000_000), "12M");
        assert_eq!(rewrap_stats(1_200_000, 3_400_000), "1.2M of 3.4M lines");
        for title in ["Pasting 4.2 MB", "Pasted 4.2 MB", REWRAPPING, REWRAPPED] {
            assert_eq!(glass_title_fault(title), None, "{title}");
            assert!(title_words(title) <= 3, "{title}");
        }
    }

    /// The paste row: progress with a fill, the ETA's amount in bytes, the
    /// one capsule, the grace from the paste's start, its finished words.
    #[test]
    fn the_paste_row_is_progress_with_one_stop() {
        let row = paste_row(7, 11, 1_100_000, 4_200_000, Duration::from_millis(500));
        assert_eq!(row.title, "Pasting 4.2 MB");
        assert_eq!(row.finished_title(), "Pasted 4.2 MB");
        assert_eq!(row.actions, [Intent::StopPaste { session: 7 }]);
        assert!(Intent::StopPaste { session: 7 }.is_consequential());
        assert!(!Intent::StopPaste { session: 7 }.closes_row());
        assert_eq!(Intent::StopPaste { session: 7 }.label(), "Stop paste");
        assert_eq!(row.reveal_after, Some(Duration::from_millis(500)));
        assert_eq!(row.key.as_deref(), Some("session.paste.7"));
        assert!(is_wait_key("session.paste.7") && is_wait_key(&wait_key(WaitKind::Rewrap, 7)));
        assert!(!is_wait_key("session.strain"));
        let meter = row.meter.expect("a meter");
        assert_eq!(meter.fill_permille, Some(262));
        assert_eq!(meter.stats, "1.1 MB / 4.2 MB");
        assert_eq!(meter.amount.map(|a| a.unit), Some(Unit::Bytes));
        assert!(!meter.busy);
        assert!(matches!(row.hold, Hold::Live { .. }));
    }

    /// Only a large paste is watched.
    #[test]
    fn a_small_paste_is_never_watched() {
        assert!(!paste_is_watched(LARGE_PASTE_BYTES - 1));
        assert!(!paste_is_watched(4096));
        assert!(paste_is_watched(LARGE_PASTE_BYTES));
    }

    /// The lifecycle: nothing while nobody waits, posted once the person
    /// does (with what is left of the grace), restated each sample — still
    /// when they look away — and ended once with the end's echo; a wait
    /// nobody waited on ends silently.
    #[test]
    fn a_session_wait_posts_restates_and_ends_in_its_echo() {
        let mut w = SessionWait::paste(3, 1);
        assert_eq!(w.sample_every(), WAIT_SAMPLE_IDLE);
        assert_eq!(
            w.step(&sample(0, 4_000_000, 100, false)),
            WaitStep::Idle,
            "off screen"
        );
        let WaitStep::Post(row) = w.step(&sample(0, 4_000_000, 1_500, true)) else {
            panic!("posted");
        };
        assert_eq!(
            row.reveal_after,
            Some(Duration::from_millis(500)),
            "the grace counts from the paste's start"
        );
        assert_eq!(w.sample_every(), WAIT_SAMPLE);
        assert!(matches!(
            w.step(&sample(1_000_000, 4_000_000, 1_600, true)),
            WaitStep::Restate(_)
        ));
        let WaitStep::Restate(away) = w.step(&sample(2_000_000, 4_000_000, 5_000, false)) else {
            panic!("a row once up stays up");
        };
        assert_eq!(away.meter.and_then(|m| m.fill_permille), Some(500));
        let done = WaitSample {
            end: Some(WaitEnd::Done),
            ..sample(4_000_000, 4_000_000, 6_000, true)
        };
        assert_eq!(w.step(&done), WaitStep::End(EchoKind::Complete));
        assert_eq!(w.step(&done), WaitStep::Idle, "ended once");
        // Nobody waited: no row, and the end is silent.
        let mut quiet = SessionWait::rewrap(4);
        assert_eq!(
            quiet.step(&sample(10, 1_000_000, 100, false)),
            WaitStep::Idle
        );
        let gone = WaitSample {
            end: Some(WaitEnd::Done),
            ..sample(1_000_000, 1_000_000, 9_000, false)
        };
        assert_eq!(quiet.step(&gone), WaitStep::Idle);
        assert_eq!(WaitEnd::Stopped.echo(), EchoKind::Vanish);
        assert_eq!(WaitEnd::Gone.echo(), EchoKind::Vanish);
        assert_eq!(WaitEnd::Failed.echo(), EchoKind::Fault);
    }

    /// The rewrap row: filled by lines, no capsule, its own words.
    #[test]
    fn the_rewrap_row_counts_lines_and_decides_nothing() {
        let mut w = SessionWait::rewrap(9);
        let WaitStep::Post(row) = w.step(&sample(1_200_000, 3_400_000, 3_000, true)) else {
            panic!("posted");
        };
        assert_eq!(
            row.title, "Rewrapping 3.4M lines",
            "the wait's size, said once"
        );
        assert_eq!(rewrap_title(0), REWRAPPING);
        assert_eq!(rewrap_title(1), "Rewrapping 1 line");
        assert_eq!(row.finished_title(), REWRAPPED);
        assert!(row.actions.is_empty());
        // The glass paints no stats beside a title that states the total
        // (ruling 246); the description keeps them.
        assert_eq!(
            crate::glass::job_stats(&row.title, "1.2M of 3.4M lines"),
            None
        );
        assert_eq!(row.reveal_after, Some(Duration::ZERO));
        let meter = row.meter.expect("a meter");
        assert_eq!(meter.stats, "1.2M of 3.4M lines");
        assert_eq!(meter.fill_permille, Some(353));
        assert_eq!(meter.amount.map(|a| a.unit), Some(Unit::Items));
    }
}
