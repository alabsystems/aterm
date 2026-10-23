// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Policy and state machine for paste and scrollback-rewrap waits.

use crate::center::EchoKind;
use crate::model::{Amount, Glyph, Hold, Intent, Message, Meter, Severity, Unit, tags};
use crate::progress::Waiter;
use crate::{Duration, PROGRESS_GRACE, REVEAL_DEFER_MAX, REVEAL_MIN_LEFT};

/// A paste this large or larger is watched.
pub const LARGE_PASTE_BYTES: u64 = 64 * 1024;

/// How often a host samples a watched wait while its row is on the glass.
pub const WAIT_SAMPLE: Duration = Duration::from_millis(250);

/// Sampling interval for an unrevealed wait.
pub const WAIT_SAMPLE_IDLE: Duration = Duration::from_secs(1);

/// A watched row's staleness cap.
pub const STALE_WAIT: Duration = Duration::from_secs(30);

/// The paste row's words while it runs: `Pasting`, and its finished words.
pub const PASTING: &str = "Pasting";

/// How long a paste's program may take none of it before the row says so.
pub(crate) const PASTE_UNREAD_AFTER: Duration = Duration::from_secs(3);
/// Default rewrap row title before the line count is known.
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
    /// Whether the wait takes a row: its waiter decides.
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

/// Format a byte count in compact decimal units (`64 KB`, `4.2 MB`, `16 MB`).
#[must_use]
pub fn size_words(bytes: u64) -> String {
    const KB: u64 = 1_000;
    const MB: u64 = 1_000_000;
    const GB: u64 = 1_000_000_000;
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

/// Format `done / total` paste bytes with `done` right-aligned to the width of `total`.
#[must_use]
pub fn paste_stats(done: u64, total: u64) -> String {
    let (done, total) = (size_words(done.min(total)), size_words(total));
    let width = total.chars().count();
    format!("{done:>width$} / {total}")
}

/// A count of lines in a few characters: `950`, `12K`, `340K`, `1.2M`, `12M`.
#[must_use]
pub fn lines_words(n: u64) -> String {
    const K: u64 = 1_000;
    const M: u64 = 1_000_000;
    if n >= 10 * M - M / 20 {
        format!("{}M", (n + M / 2) / M)
    } else if n >= M - K / 2 {
        let t = (n.saturating_mul(10) + M / 2) / M;
        format!("{}.{}M", t / 10, t % 10)
    } else if n >= 10 * K - K / 20 {
        format!("{}K", (n + K / 2) / K)
    } else if n >= K {
        let t = (n.saturating_mul(10) + K / 2) / K;
        format!("{}.{}K", t / 10, t % 10)
    } else {
        n.to_string()
    }
}

/// A rewrap's stats: `1.2M of 3.4M lines`, `done` right-aligned to the total's width.
#[must_use]
pub fn rewrap_stats(done: u64, total: u64) -> String {
    let (done, total) = (lines_words(done.min(total)), lines_words(total));
    let width = total.chars().count();
    format!("{done:>width$} of {total} lines")
}

/// Build the progress message row for a watched paste in `session`.
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

/// Format the rewrap row title for `total` lines, or [`REWRAPPING`] when `total == 0`.
#[must_use]
pub fn rewrap_title(total: u64) -> String {
    match total {
        0 => REWRAPPING.to_string(),
        1 => "Rewrapping 1 line".to_string(),
        n => format!("Rewrapping {} lines", lines_words(n)),
    }
}

/// Build the progress message row for a scrollback rewrap in `session`.
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
    /// The row's echo: Complete for delivered work (its finished words).
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
    /// Whether the person waits on it now: the paste's session is on screen, or the person
    /// scrolled into or searched the rewrap's missing history.
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

/// One session's watched wait, clockless.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct SessionWait {
    kind: WaitKind,
    session: u64,
    series: u64,
    posted: bool,
    held: bool,
    first: Option<(u64, Duration)>,
    moved: Option<(u64, Duration)>,
}

impl SessionWait {
    /// A paste into `session`; `series` names this paste (a new paste is a new estimator series).
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

    /// The row's words for `s`.
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

    /// The next sample falls due this long after one.
    #[must_use]
    pub const fn sample_every(&self) -> Duration {
        if self.posted || self.held {
            WAIT_SAMPLE
        } else {
            WAIT_SAMPLE_IDLE
        }
    }
}

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

fn unread_words(stuck: Duration) -> String {
    format!("not read for {}", span_words(stuck))
}

fn span_words(d: Duration) -> String {
    let s = d.as_secs();
    if s < 60 {
        format!("{s} s")
    } else if s < 3600 {
        format!("{}m {}s", s / 60, s % 60)
    } else {
        format!("{}h {}m", s / 3600, s % 3600 / 60)
    }
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
        let WaitStep::Restate(row) = w.step(&sample(1_000_000, total, 39_250, true)) else {
            panic!("restated");
        };
        assert!(!row.excerpt && row.detail.is_empty(), "{row:?}");
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
