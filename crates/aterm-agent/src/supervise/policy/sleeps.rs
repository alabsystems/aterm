// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! THE SLEEPS OF THE MACHINE A LOOP KNOWS OF, on the wall clock: what a
//! turn's done row counts that was no work.
//!
//! Claude Code times a turn on the wall clock (`Date.now() - turnStartTime -
//! totalPausedMs`, measured on 2.1.303: `docs/measured/real-claude-lab-2026-10-15.md`,
//! 2026-10-19), which runs on through a sleep of the machine, so the done row
//! of a turn a sleep fell in counts the night as work: a spend limit's try
//! answered at once, the Mac asleep 30 minutes from the Enter, drew `✻ Baked
//! for 30m 3s`, and read whole it started the limit's ladder over. ANY turn's
//! done row is read less the sleep that can lie inside its span
//! ([`SleepBook::in_turn`]) — the act's, a person's, one a read saw run before
//! the sleep — and the reading takes the larger of that and the loop's own
//! measure, which a sleep stops (`TurnEndReading::of_slept`).
//!
//! Where a loop learns of a sleep, each a [`Sleep`] in its book:
//!
//! * ITS OWN WATCH: the wall clock ran ahead of the awake clock between two of
//!   its reads, and the machine's own count of its sleep since boot
//!   (`aterm_uds::process::asleep_since_boot`, through the loop's clock door)
//!   says how much of that was sleep — a wall clock stepped forward is none.
//!   The sleep lies between the two reads.
//! * THE LEDGER: every act's row records that count and the boot it counts
//!   in ([`SleepStamp`], `approvals::SLEPT_MS`), so a loop that follows, its
//!   own count read at its first look, knows how long the machine slept since
//!   the last act it never watched ([`SleepBook::take_up`]).
//! * THE SYSTEM'S RECORD of its last sleep (macOS `kern.sleeptime`,
//!   `kern.waketime`): placed exactly, for a sleep before the loop's first
//!   look that nothing else places — and its wake's settle placed from the
//!   wake it records, as the loop that measured the wake placed it.
//!
//! WHERE NOTHING SAYS — an act's row an older build wrote (no count), a count
//! on another boot, the sleeps before the system's last one where no ledger
//! counts them, a machine that keeps no count (Windows) — the done row is
//! read as before this book: whole, but for the sleeps the loop measured by
//! its clocks and the system's last. That is data that does not exist; it
//! ages out with those books. The loop says so once, where a turn's done row
//! reaching back past its first look is read so (`SLEEP …` in the journal,
//! [`SinceAct::unknown`]).
//!
//! A turn begins and ends awake, so a sleep placed exactly (the system's
//! record) lies wholly inside a turn or wholly outside it: one that ended
//! before a person's Enter is no part of their turn, however far back the
//! minute the done row names reaches. Where the book knows a sleep's length
//! but not its place (between two reads, or since an act), the reading
//! takes the most of it the turn's span can hold for the turn's work: less
//! work, the reading that types later, never sooner — a limit's try's rung
//! kept, a short turn's back-off, a person's own exchange left to them — and
//! never more than the sleep itself. Whether a person's keystroke is inside
//! the turn is read against the least of it that must lie there
//! ([`InTurn`]): the turn at its longest, so a person's turn is never read
//! as nobody's for a sleep that may lie outside it.

use std::collections::VecDeque;
use std::time::Duration;

/// How many sleeps a book keeps, the newest: a turn's span reaches back a day
/// or two at most, and a machine sleeps a few times a day.
pub const SLEEPS_KEPT: usize = 64;

/// A sleep shorter than this is not written in a book from a count alone
/// ([`SleepBook::take_up`]'s rest): the counts are read at different moments,
/// and a minute is under every measure a turn's work is judged by (the
/// two-minute slack of an act's turn, the five minutes that end a limit's
/// episode).
pub const SLEEP_FLOOR: Duration = Duration::from_secs(60);

/// How finely Claude Code's done row shows a turn's span of `work` (`✻
/// Baked for 30m 3s`, `✻ Crunched for 2h 58m`): to the second, and from an
/// hour on to the minute. The span is the row's, give or take that much
/// ([`SleepBook::in_turn`]).
#[must_use]
pub fn done_row_step(work: Duration) -> Duration {
    if work >= Duration::from_secs(3600) {
        Duration::from_secs(60)
    } else {
        Duration::from_secs(1)
    }
}

/// ONE SLEEP OF THE MACHINE: `len` long, lying somewhere inside the wall
/// window `from_ms..to_ms` (unix milliseconds) — exactly there when the
/// window is as long as the sleep.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Sleep {
    /// How long the machine slept.
    pub len: Duration,
    /// The window it lies in: from…
    pub from_ms: i64,
    /// …to.
    pub to_ms: i64,
}

impl Sleep {
    /// PLACED EXACTLY: its window is no longer than the sleep (the system's
    /// record of it, a watch with no awake time between its two reads). It
    /// is one sleep, and a turn begins and ends awake, so it lies wholly
    /// inside a turn's span or wholly outside it.
    #[must_use]
    pub fn placed(&self) -> bool {
        self.to_ms.saturating_sub(self.from_ms) <= millis(self.len)
    }

    /// The most of this sleep that can lie inside `start_ms..end_ms`, every
    /// moment a turn's span can reach: placed exactly, all of it where its
    /// window lies within the span, else none (it lies outside the turn: a
    /// sleep that ended before the Enter is no part of the turn, however far
    /// the done row's minute reaches back into it); else no more than the
    /// sleep, nor than the stretch of its window inside the span.
    #[must_use]
    pub fn most_inside(&self, start_ms: i64, end_ms: i64) -> Duration {
        if self.placed() {
            return if start_ms <= self.from_ms && self.to_ms <= end_ms {
                self.len
            } else {
                Duration::ZERO
            };
        }
        self.len.min(ms(self.overlap(start_ms, end_ms)))
    }

    /// The least of this sleep that must lie inside `start_ms..end_ms`, the
    /// moments every span a turn can have holds: what the stretch of its
    /// window outside them cannot hold — none, where that stretch is as long
    /// as the sleep (a count the ledger cannot place, somewhere in hours).
    #[must_use]
    pub fn least_inside(&self, start_ms: i64, end_ms: i64) -> Duration {
        let window = self.to_ms.saturating_sub(self.from_ms);
        let outside = window.saturating_sub(self.overlap(start_ms, end_ms));
        self.len.saturating_sub(ms(outside))
    }

    /// How much of its window lies inside `start_ms..end_ms`, in ms.
    fn overlap(&self, start_ms: i64, end_ms: i64) -> i64 {
        let (lo, hi) = (self.from_ms.max(start_ms), self.to_ms.min(end_ms));
        hi.saturating_sub(lo).max(0)
    }
}

/// THE SLEEP INSIDE A TURN ([`SleepBook::in_turn`]): the most of the sleeps
/// known that can lie inside it, and the least that must. The done row's
/// span less the most is the turn's work at its least, for the measures a
/// turn's work is judged by — a limit's try's rung kept, a person's own
/// exchange left to them, the short streak's back-off — which then type
/// later, never sooner. Less the least, it is the turn's work at its most,
/// for whether a person's keystroke is inside the turn
/// (`TurnEndReading::person_in_turn`): a turn whose work a sleep's unknown
/// place may have shortened is still theirs, never nobody's, which a loop
/// would type into.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct InTurn {
    /// The most that can lie inside the turn.
    pub most: Duration,
    /// The least that must, never more than [`Self::most`].
    pub least: Duration,
}

/// THE MACHINE'S COUNT OF ITS SLEEP at a moment: how long it slept since it
/// booted, and the boot it counts in (`aterm_uds::process::asleep_since_boot`,
/// `astream_sys::boot_session`). Two counts compare only on the same named
/// boot: the count starts over at a boot.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SleepStamp {
    /// The sleep since boot.
    pub asleep: Duration,
    /// The boot ([`Boot`]: the kernel's boot id, `astream_sys::boot_session`,
    /// the reading aterm-link's broker opens its log in); `None` where the
    /// kernel names none, or names the all-zero id a damaged answer reads as,
    /// and then the count compares with no other process's.
    pub boot: Option<Boot>,
}

impl SleepStamp {
    /// How long the machine slept since `then`, read on this boot: `None`
    /// on another boot, or a boot either does not name. The boots compare
    /// by their 16 bytes ([`Boot`]), so how a row or a tape spelled one never
    /// decides it.
    #[must_use]
    pub fn since(&self, then: &SleepStamp) -> Option<Duration> {
        let same = matches!((&self.boot, &then.boot), (Some(a), Some(b)) if a == b);
        same.then(|| self.asleep.saturating_sub(then.asleep))
    }
}

/// A BOOT A COUNT OF THE MACHINE'S SLEEP IS ON: the kernel's boot id, its 16
/// bytes (`astream_sys::boot_session`), and the case its text was spelled in,
/// which an act's ledger row (`approvals::BOOT`) and a tape's count
/// (`tape::core`) write it back in ([`Boot::text`]).
///
/// Two boots are one when their bytes are: the case never decides it (this
/// type's `PartialEq` reads the id alone), so a row of either case counts
/// with this loop's. The case is kept for the readers that compare the TEXT.
/// A build from 30ae1a1185 up to this one's change (v0.107.0 among them) wrote
/// the kernel's text as it came — macOS's `kern.bootsessionuuid` upper-case,
/// Linux's `boot_id` lower-case — and compares it as text: a row this build
/// wrote in the other case would read to that build, after a rollback or a
/// downgrade, as another boot, and the sleep since the act would go
/// uncounted. And a replay feeds a tape's recorded count back to the loop and
/// compares the act's row the loop writes with the recorded one byte for byte:
/// a boot written back in a case it was not read in would diverge at every
/// act, a spelling reported as a change of behaviour. So this host's boot is
/// written as this host's kernel spells it ([`Boot::kernel`]), and a boot read
/// off a row or a tape as that text spelled it ([`Boot::of`]).
#[derive(Debug, Clone, Copy)]
pub struct Boot {
    /// The id's 16 bytes, as `astream_sys::parse_boot_id` reads them.
    id: [u8; 16],
    /// Spelled lower-case (Linux's `boot_id`); upper-case otherwise.
    lower: bool,
}

/// Whether this host's kernel spells its boot id lower-case: Linux's and
/// Android's `/proc/sys/kernel/random/boot_id` do, macOS's
/// `kern.bootsessionuuid` does not. Upper-case where no kernel names a boot,
/// which writes none (`astream_sys::boot_session` reads none there).
const KERNEL_SPELLS_LOWER: bool = cfg!(any(target_os = "linux", target_os = "android"));

impl Boot {
    /// This host's boot, `id` as `astream_sys::boot_session` read it, spelled
    /// as this host's kernel spells it.
    #[must_use]
    pub const fn kernel(id: [u8; 16]) -> Self {
        Self {
            id,
            lower: KERNEL_SPELLS_LOWER,
        }
    }

    /// The boot a row's or a tape's `text` names, by astream's reader of the
    /// kernel's boot id text (`astream_sys::parse_boot_id`), its case kept
    /// (lower-case if any hex letter is): the UUID in either case. `None` for
    /// anything else, the all-zero id among them: a damaged answer must never
    /// pass as a boot, or two equal damaged answers would read as one boot,
    /// and the sleep between them as counted.
    #[must_use]
    pub fn of(text: &str) -> Option<Self> {
        let id = astream_sys::parse_boot_id(text.as_bytes())?;
        let lower = text.bytes().any(|b| matches!(b, b'a'..=b'f'));
        Some(Self { id, lower })
    }

    /// The id's 16 bytes.
    #[must_use]
    pub const fn id(&self) -> [u8; 16] {
        self.id
    }

    /// The text a row or a tape writes: the UUID's 32 hex digits hyphenated
    /// 8-4-4-4-12, the kernel's own form, in the case this boot was read in.
    #[must_use]
    pub fn text(&self) -> String {
        let [a, b, c, d, e, f, g, h, i, j, k, l, m, n, o, p] = self.id;
        let upper = format!(
            "{a:02X}{b:02X}{c:02X}{d:02X}-{e:02X}{f:02X}-{g:02X}{h:02X}-{i:02X}{j:02X}-\
             {k:02X}{l:02X}{m:02X}{n:02X}{o:02X}{p:02X}"
        );
        if self.lower {
            upper.to_ascii_lowercase()
        } else {
            upper
        }
    }
}

/// One boot is its 16 bytes, however it was spelled ([`Boot`]).
impl PartialEq for Boot {
    fn eq(&self, other: &Self) -> bool {
        self.id == other.id
    }
}

impl Eq for Boot {}

/// A LOOP'S FIRST LOOK AT THE MACHINE'S SLEEP: what a seed sets the
/// ledger's last act against ([`SleepBook::count_since_act`]).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FirstLook {
    /// When, unix ms.
    pub at_ms: i64,
    /// The machine's count of its sleep then.
    pub count: Option<SleepStamp>,
    /// The system's record of its last sleep then (unix ms, asleep and woke).
    pub last: Option<(i64, i64)>,
}

/// THE SLEEPS A LOOP KNOWS OF ([module docs](self)), the newest
/// [`SLEEPS_KEPT`].
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct SleepBook {
    sleeps: VecDeque<Sleep>,
}

impl SleepBook {
    /// No sleep known: every done row reads whole.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.sleeps.is_empty()
    }

    /// The sleeps known, oldest first.
    pub fn sleeps(&self) -> impl Iterator<Item = &Sleep> {
        self.sleeps.iter()
    }

    /// A sleep of `len` that lies inside `from_ms..to_ms`: the loop's watch
    /// measured it between two reads, or the ledger counts it. Nothing for
    /// no sleep, or a window that cannot hold one.
    pub fn add(&mut self, len: Duration, from_ms: i64, to_ms: i64) {
        let sleep = Sleep {
            len,
            from_ms,
            to_ms,
        };
        // The same sleep twice (a seed read again) is one sleep.
        if len.is_zero() || to_ms <= from_ms || self.sleeps.contains(&sleep) {
            return;
        }
        if self.sleeps.len() == SLEEPS_KEPT {
            self.sleeps.pop_front();
        }
        self.sleeps.push_back(sleep);
    }

    /// THE SLEEP INSIDE A TURN whose done row says `work` (its wall span) and
    /// ended between `end_lo_ms` and `end_hi_ms` (the done row's clock names
    /// a minute; with none, the read's time is both): the most of every
    /// sleep known that can lie inside some span the turn can have had, and
    /// the least that must lie inside every one ([`InTurn`]), each at most
    /// the span itself. The row shows the span to the second, and past an
    /// hour to the minute (`2h 58m`), so a span is the row's give or take
    /// that much ([`done_row_step`]). The turn's awake work is `work` less
    /// it.
    #[must_use]
    pub fn in_turn(&self, work: Duration, end_lo_ms: i64, end_hi_ms: i64) -> InTurn {
        if self.sleeps.is_empty() || work.is_zero() {
            return InTurn::default();
        }
        let step = millis(done_row_step(work));
        let span_ms = millis(work);
        let (lo, hi) = (end_lo_ms.min(end_hi_ms), end_hi_ms.max(end_lo_ms));
        // Every moment some span can reach: the longest, from the earliest end.
        let reach = (lo.saturating_sub(span_ms.saturating_add(step)), hi);
        // Every moment each span holds: the shortest, to the latest end.
        let held = (hi.saturating_sub(span_ms.saturating_sub(step)), lo);
        let (most, least) =
            self.sleeps
                .iter()
                .fold((Duration::ZERO, Duration::ZERO), |(most, least), s| {
                    let m = s.most_inside(reach.0, reach.1);
                    let l = if held.0 < held.1 {
                        s.least_inside(held.0, held.1).min(m)
                    } else {
                        Duration::ZERO
                    };
                    (most.saturating_add(m), least.saturating_add(l))
                });
        let most = most.min(work);
        InTurn {
            most,
            least: least.min(most),
        }
    }

    /// THE SLEEP BEFORE THIS LOOP'S FIRST LOOK, at `now_ms`, that no read of
    /// its own measured: what the system's record and the ledger say of it
    /// ([module docs](self)).
    ///
    /// * `last`: the system's last sleep (unix ms, asleep and woke), placed
    ///   exactly where it ended before `now_ms`;
    /// * `act`: the last act's row, its stamp (unix ms) and the count it
    ///   records (`None`: a row an older build wrote), with `now`, this
    ///   loop's own count: on the same boot, the machine slept the difference
    ///   since the act — the system's last sleep where it fell since, and the
    ///   rest, if a minute or more, somewhere between the act and that sleep
    ///   (or this look, with no such sleep).
    ///
    /// What it wrote ([`TakeUp`]), for the loop to say so.
    pub fn take_up(
        &mut self,
        now_ms: i64,
        now: Option<&SleepStamp>,
        act: Option<(i64, Option<&SleepStamp>)>,
        last: Option<(i64, i64)>,
    ) -> TakeUp {
        let placed = self.place_last(now_ms, last);
        TakeUp {
            last: placed,
            since_act: self.count_since_act(now_ms, now, act, last),
        }
    }

    /// [`Self::take_up`]'s first half, at the loop's first look: the
    /// system's last sleep, where it ended before `now_ms`. Whether it was
    /// written.
    pub fn place_last(&mut self, now_ms: i64, last: Option<(i64, i64)>) -> bool {
        let Some((slept, woke)) = before_look(now_ms, last) else {
            return false;
        };
        self.add(ms(woke - slept), slept, woke);
        true
    }

    /// [`Self::take_up`]'s second half, once the ledger is read: the sleep
    /// since its last act that the system's last sleep (`last`, as the first
    /// look read it) does not place.
    pub fn count_since_act(
        &mut self,
        now_ms: i64,
        now: Option<&SleepStamp>,
        act: Option<(i64, Option<&SleepStamp>)>,
        last: Option<(i64, i64)>,
    ) -> SinceAct {
        let Some((at_ms, stamp)) = act else {
            return SinceAct::NoAct;
        };
        // A machine that keeps no count writes none in its acts' rows
        // either: the reason is the machine's, whatever the row holds.
        let Some(now) = now else {
            return SinceAct::NoCount;
        };
        let Some(stamp) = stamp else {
            return SinceAct::Uncounted;
        };
        let Some(total) = now.since(stamp) else {
            return SinceAct::OtherBoot;
        };
        // The system's last sleep, where it fell since the act, is that much
        // of the count; the rest lies before it.
        let (placed, before) = match before_look(now_ms, last) {
            Some((slept, woke)) if slept >= at_ms => (ms(woke - slept), slept),
            _ => (Duration::ZERO, now_ms),
        };
        let rest = total.saturating_sub(placed);
        if rest >= SLEEP_FLOOR {
            self.add(rest, at_ms, before);
        }
        SinceAct::Counted(total)
    }
}

/// The system's last sleep (`last`), where it is one that ended before the
/// look at `now_ms`.
fn before_look(now_ms: i64, last: Option<(i64, i64)>) -> Option<(i64, i64)> {
    last.filter(|&(slept, woke)| slept < woke && woke <= now_ms)
}

/// What [`SleepBook::take_up`] found.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct TakeUp {
    /// The system's last sleep was written.
    pub last: bool,
    /// What the ledger said of the sleep since its last act.
    pub since_act: SinceAct,
}

/// What the ledger says of the sleep since its last act ([`TakeUp`]).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SinceAct {
    /// No act on the books.
    NoAct,
    /// The act's row holds no count: an older build wrote it.
    Uncounted,
    /// Its count is another boot's, or a boot this machine does not name.
    OtherBoot,
    /// This machine keeps no count of its sleep (Windows).
    NoCount,
    /// The machine slept this long since the act.
    Counted(Duration),
}

impl SinceAct {
    /// WHY THE LEDGER COULD NOT SAY how long the machine slept since its last
    /// act, in a journal line's words: the sleep before the loop's first look
    /// is then counted by the system's record of its last sleep alone, and a
    /// turn's done row reaching back past that look is read as before this
    /// book, whole of the rest. `None` where it said, or had no act to say
    /// it of.
    #[must_use]
    pub fn unknown(self) -> Option<&'static str> {
        match self {
            Self::Uncounted => {
                Some("the last act on the books counts no sleep (an older build's row)")
            }
            Self::OtherBoot => Some("the last act on the books counts its sleep on another boot"),
            Self::NoCount => Some("this machine keeps no count of its sleep"),
            Self::NoAct | Self::Counted(_) => None,
        }
    }
}

fn ms(d: i64) -> Duration {
    Duration::from_millis(u64::try_from(d).unwrap_or(0))
}

fn millis(d: Duration) -> i64 {
    i64::try_from(d.as_millis()).unwrap_or(i64::MAX)
}

#[cfg(test)]
#[path = "sleeps_tests.rs"]
mod tests;
