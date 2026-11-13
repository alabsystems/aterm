// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! CLAIMS HOLD WHEN A MAC'S CLOCK IS WRONG (docs/DESIGN-fluid-swarm-2026-10-28.md F22). Every
//! claims path places days by the local clock, so a Mac whose clock is a day or more off writes
//! and reads tables its peers never read or write. Nothing here moves a clock or a table: each Mac
//! learns how far its clock stands from each peer's, and a Mac that a strict majority of its peers
//! read as off refuses what it cannot do safely.
//!
//! - **The mark.** Each repair round already trades its table-ID list. While the list has room
//!   below the bound every receiver since 0.145 enforces, it carries one more 32-byte entry, a
//!   *clock mark*: [`MARK_PREFIX`], the sender's wall time in ms (little-endian), then 8 zero
//!   bytes ([`mark`]). Builds before F22 read it as a table they do not hold. A receiver takes the
//!   marks out ([`split`]) before anything else reads the list.
//! - **A reading.** A mark built after this side began its round and received at wall time `recv`
//!   a round span `span` after that bounds this Mac's offset from the peer, `my − peer`, to
//!   `[recv − peer − span, recv − peer]`, whatever the two directions' delays
//!   ([`Reading::from_mark`]). A peer from before F22 sends no mark; the claims tables it offers
//!   lie in its window, which bounds its clock to a day ([`Reading::from_offers`]).
//! - **Classes.** A reading is Ahead, Behind, Agree or Undecided against [`T_MS`]; leaving Off
//!   needs Agree within half of it ([`Reading::class`]).
//! - **Standing.** [`standing`] reduces the latest fresh reading of each other current member to
//!   Unknown (none, unless it was Off: silence keeps Off), Off (a strict majority in one
//!   direction), Agrees (a strict majority agree) or Split (anything else), names the peers it
//!   reads as skewed, and, on a narrower line ([`HOLD_T_MS`]) and for a day ([`HOLD_MS`]), the
//!   peers whose home accounts it holds for them. This Mac never votes.
//! - **The store.** The Commons runtime keeps each peer's latest reading and the last standing
//!   in `clock.state` beside the ring states ([`State`], [`Clocks`]); every process judges from
//!   that file and its own clock. A file that is missing or damaged reads as no readings.

use astream_wire::sync::Digest;
use std::{
    collections::{BTreeMap, BTreeSet},
    path::{Path, PathBuf},
    time::{Duration, Instant},
};

/// A mark's first 16 bytes: no table digest has them but by chance.
pub const MARK_PREFIX: &[u8; 16] = b"aterm/clock/mk1\0";
/// How far apart two clocks may read and still agree: below every claims margin (a lease's
/// presence, a session's freshness, a row's renewal) and far above LAN delay plus NTP error.
pub const T_MS: i64 = 10 * 60 * 1000;
/// A round longer than this gives no reading: its bound would be too loose to class.
pub const MAX_SPAN: Duration = Duration::from_secs(60);
/// A reading older than this counts for nothing toward a standing, nor names a peer skewed.
pub const FRESH_MS: u64 = 30 * 60 * 1000;
/// How long past its reading a peer is held for ([`Judgement::held`]): a day, as long as a home
/// claim row it wrote would last (the harness's claims horizon). A Mac its peers last read Off
/// stays Off through any silence ([`standing`]), writing no claim; a hold that ended with the
/// standing's freshness would free its home accounts half an hour after the last round.
pub const HOLD_MS: u64 = 24 * 60 * 60 * 1000;
/// How far apart two clocks may read and leave no hold ([`Reading::holds`]): a quarter of the
/// tolerance, half the line a Mac leaves Off by ([`Reading::class`]). A Mac its peers read Off
/// stands more than `T/2` from a strict majority of them, and each such peer's reading of it from
/// that round puts it outside `±T/2` too, the round's span lying inside both bounds; the margin
/// below `T/2` keeps holding a peer whose clock stands within `T/4` of that majority's but falls
/// on the other side of the line.
pub const HOLD_T_MS: i64 = T_MS / 4;
/// How many periods around this clock's a pre-F22 peer's offered claims tables are matched in.
pub(crate) const BAND: u64 = 8;
/// The store's file, in the claim directory beside the ring states.
pub const STATE_NAME: &str = "clock.state";
const STATE_MAGIC: &[u8; 5] = b"ATCK1";
/// The most readings the store keeps: one per member a roster can hold.
pub const MAX_READINGS: usize = aterm_secrets::commons::roster::MAX_MEMBERS;
const ENTRY_LEN: usize = 16 + 8 + 8 + 8 + 1;
const STATE_MAX: usize = STATE_MAGIC.len() + 2 + 1 + MAX_READINGS * ENTRY_LEN;

pub(crate) fn claim_metadata() -> (&'static str, usize) {
    (STATE_NAME, STATE_MAX)
}

/// The clock mark of wall time `wall_ms`: its one spelling.
#[must_use]
pub fn mark(wall_ms: u64) -> Digest {
    let mut id = [0; 32];
    id[..16].copy_from_slice(MARK_PREFIX);
    id[16..24].copy_from_slice(&wall_ms.to_le_bytes());
    id
}

/// The wall time `id` marks, where it is a mark in its one spelling; anything else is an ordinary
/// table ID.
#[must_use]
pub fn read_mark(id: &Digest) -> Option<u64> {
    (id[..16] == MARK_PREFIX[..] && id[24..] == [0; 8])
        .then(|| u64::from_le_bytes(id[16..24].try_into().expect("eight bytes")))
}

/// What marks a peer's list carried.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Heard {
    /// None: a peer from before F22, or one whose list had no room.
    Nothing,
    /// One, at this wall time.
    One(u64),
    /// More than one: no reading.
    Several,
}

/// `remote` without its marks, and what marks it carried. No mark is ever a table: never common,
/// never selected, never adopted.
#[must_use]
pub fn split(remote: &BTreeSet<Digest>) -> (BTreeSet<Digest>, Heard) {
    split_owned(remote.clone())
}

pub(crate) fn split_owned(mut remote: BTreeSet<Digest>) -> (BTreeSet<Digest>, Heard) {
    let mut heard = Heard::Nothing;
    remote.retain(|id| match read_mark(id) {
        Some(ms) => {
            heard = match heard {
                Heard::Nothing => Heard::One(ms),
                _ => Heard::Several,
            };
            false
        }
        None => true,
    });
    (remote, heard)
}

/// What a reading was taken from.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Source {
    /// A peer's mark.
    Mark,
    /// The claims tables a peer from before F22 offered: day-coarse.
    Ring,
}

/// One bound on this Mac's offset from one peer: `my − peer` lies in `[lo_ms, hi_ms]`, as read at
/// this Mac's wall time `at_ms`.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Reading {
    pub lo_ms: i64,
    pub hi_ms: i64,
    pub at_ms: u64,
    pub source: Source,
}

/// How a reading stands against the tolerance.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Class {
    /// This Mac's clock is ahead of the peer's by more than the tolerance.
    Ahead,
    /// This Mac's clock is behind the peer's by more than the tolerance.
    Behind,
    /// Within the tolerance both ways.
    Agree,
    /// Neither: the bound straddles the tolerance. It does not count.
    Undecided,
}

fn ms(value: u64) -> i64 {
    i64::try_from(value).unwrap_or(i64::MAX)
}

impl Reading {
    /// The reading a mark of the peer's wall time `peer_ms` gives, received at this Mac's wall
    /// time `recv_ms` a round span `span` after this side began the round: `my − peer ∈ [recv −
    /// peer − span, recv − peer]`. A span over [`MAX_SPAN`] gives none.
    #[must_use]
    #[cfg_attr(
        any(test, debug_assertions),
        aterm_spec_anchor::refines(
            machine = "DbClaimClock",
            action = "HearB",
            project = "aterm_db::clock::tests::claim_clock_transition"
        )
    )]
    #[cfg_attr(
        any(test, debug_assertions),
        aterm_spec_anchor::refines(
            machine = "DbClaimClock",
            action = "HearC",
            project = "aterm_db::clock::tests::claim_clock_transition"
        )
    )]
    #[cfg_attr(
        any(test, debug_assertions),
        aterm_spec_anchor::refines(
            machine = "DbClaimClock",
            action = "HearL",
            project = "aterm_db::clock::tests::claim_clock_transition"
        )
    )]
    pub fn from_mark(peer_ms: u64, recv_ms: u64, span: Duration) -> Option<Self> {
        if span > MAX_SPAN {
            return None;
        }
        let hi = ms(recv_ms).saturating_sub(ms(peer_ms));
        // Rounded up: the bound holds for the whole span, not its whole milliseconds.
        let span = span.as_nanos().div_ceil(1_000_000);
        let span = i64::try_from(span).unwrap_or(i64::MAX);
        Some(Self {
            lo_ms: hi.saturating_sub(span),
            hi_ms: hi,
            at_ms: recv_ms,
            source: Source::Mark,
        })
    }

    /// The reading the claims tables a peer from before F22 offered give: their periods `offered`
    /// (of `period_s` seconds) lie in its window, so its current period lies in `[max − 1, min +
    /// 1]` and `my − peer ∈ (recv − (min + 2)·P, recv − (max − 1)·P]`. Offers spanning more than
    /// a window (or none) give none.
    #[must_use]
    pub fn from_offers(offered: &BTreeSet<u64>, recv_ms: u64, period_s: u64) -> Option<Self> {
        let (&min, &max) = (offered.first()?, offered.last()?);
        if max - min > 2 {
            return None;
        }
        let period = i128::from(period_s) * 1000;
        let recv = i128::from(recv_ms);
        let clamp = |v: i128| i64::try_from(v).unwrap_or(if v < 0 { i64::MIN } else { i64::MAX });
        Some(Self {
            lo_ms: clamp(recv - (i128::from(min) + 2) * period),
            hi_ms: clamp(recv - (i128::from(max) - 1) * period),
            at_ms: recv_ms,
            source: Source::Ring,
        })
    }

    /// This reading's class while the last standing was `last`: against [`T_MS`], or half of it
    /// both ways while `last` was Off, so a Mac leaves Off only once its peers read it within
    /// half the tolerance, and an offset near the tolerance cannot toggle it.
    #[must_use]
    pub fn class(&self, last: Standing) -> Class {
        let t = if matches!(last, Standing::Off { .. }) {
            T_MS / 2
        } else {
            T_MS
        };
        if self.lo_ms > t {
            Class::Ahead
        } else if self.hi_ms < -t {
            Class::Behind
        } else if self.lo_ms >= -t && self.hi_ms <= t {
            Class::Agree
        } else {
            Class::Undecided
        }
    }

    /// Whether this reading names its peer for holds: it does not agree within [`HOLD_T_MS`], an
    /// undecided bound included, whatever the last standing. A narrower line than any class, so
    /// every peer whose reading counted against an Off Mac names it.
    #[must_use]
    pub const fn holds(&self) -> bool {
        !(self.lo_ms >= -HOLD_T_MS && self.hi_ms <= HOLD_T_MS)
    }

    /// Whether this reading still holds its peer at `now_ms` ([`Judgement::held`]): taken no more
    /// than [`HOLD_MS`] ago and not in the future, and it names the peer for holds.
    #[must_use]
    pub fn holding(&self, now_ms: u64) -> bool {
        self.holds()
            && now_ms
                .checked_sub(self.at_ms)
                .is_some_and(|age| age <= HOLD_MS)
    }

    /// Whether this reading still counts at `now_ms`: taken no more than [`FRESH_MS`] ago, and
    /// not in the future (a clock set back since reads it as no reading).
    #[must_use]
    #[cfg_attr(
        any(test, debug_assertions),
        aterm_spec_anchor::refines(
            machine = "DbClaimClock",
            action = "ForgetB",
            project = "aterm_db::clock::tests::claim_clock_transition"
        )
    )]
    #[cfg_attr(
        any(test, debug_assertions),
        aterm_spec_anchor::refines(
            machine = "DbClaimClock",
            action = "ForgetC",
            project = "aterm_db::clock::tests::claim_clock_transition"
        )
    )]
    #[cfg_attr(
        any(test, debug_assertions),
        aterm_spec_anchor::refines(
            machine = "DbClaimClock",
            action = "ForgetL",
            project = "aterm_db::clock::tests::claim_clock_transition"
        )
    )]
    pub fn fresh(&self, now_ms: u64) -> bool {
        now_ms
            .checked_sub(self.at_ms)
            .is_some_and(|age| age <= FRESH_MS)
    }
}

/// What this Mac's peers say of its clock.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Standing {
    /// No fresh decisive reading: a lone Mac, or readings stale or dated in the future, unless
    /// the last standing was Off (which silence keeps). It runs as before F22.
    Unknown,
    /// A strict majority of the peers it reads agree with its clock.
    Agrees,
    /// No strict majority agrees, and none says it is off one way.
    Split,
    /// A strict majority says its clock is off, ahead or behind.
    Off { ahead: bool },
}

impl Standing {
    const fn tag(self) -> u8 {
        match self {
            Self::Unknown => 0,
            Self::Agrees => 1,
            Self::Split => 2,
            Self::Off { ahead: true } => 3,
            Self::Off { ahead: false } => 4,
        }
    }

    const fn from_tag(tag: u8) -> Option<Self> {
        Some(match tag {
            0 => Self::Unknown,
            1 => Self::Agrees,
            2 => Self::Split,
            3 => Self::Off { ahead: true },
            4 => Self::Off { ahead: false },
            _ => return None,
        })
    }

    /// The word a log line and a cause carry.
    #[must_use]
    pub const fn word(self) -> &'static str {
        match self {
            Self::Unknown => "unknown",
            Self::Agrees => "agrees",
            Self::Split => "split",
            Self::Off { .. } => "off",
        }
    }
}

/// A peer this Mac reads as skewed ([`Judgement::skewed`]) or holds for ([`Judgement::held`]).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Skewed {
    /// Its Commons machine.
    pub machine: [u8; 16],
    /// Its binding's recipient (`age1…`), which names it in the swarm roster.
    pub recipient: String,
    /// Its holds last until this wall time (ms): [`HOLD_MS`] past the reading for a held peer,
    /// [`FRESH_MS`] for a skewed one.
    pub until_ms: u64,
    /// This Mac's clock runs at most this far ahead of the peer's (ms, the reading's upper
    /// bound; zero or less where it does not run ahead): a row the peer wrote to lapse at `until`
    /// on its clock lapses no later than `until + ahead_ms` on this one.
    pub ahead_ms: i64,
}

/// What [`standing`] judges.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Judgement {
    pub standing: Standing,
    /// Under Off, the least offset every reading against this Mac shows (ms): at least this far
    /// off. Otherwise zero.
    pub by_ms: u64,
    /// The decisive readings against this Mac's clock: those Off counts under Off, every one
    /// that does not agree otherwise.
    pub against: usize,
    /// The decisive readings counted.
    pub of: usize,
    /// Off kept through silence: no fresh decisive reading, so the figures are those of the
    /// readings it last stood on, however old.
    pub silent: bool,
    /// The peers it reads as skewed, unless it is Off; by machine: each fresh decisive reading
    /// that does not agree, the Macs a warning names.
    pub skewed: Vec<Skewed>,
    /// The peers whose home accounts it holds for them, unless it is Off; by machine: each reading
    /// that still holds its peer ([`Reading::holding`]: not agreeing within [`HOLD_T_MS`], and no
    /// more than [`HOLD_MS`] old). It covers every peer [`Self::skewed`] names, and every peer an
    /// Off Mac counted against itself.
    pub held: Vec<Skewed>,
}

impl Judgement {
    /// Whether this Mac writes no claim row: its peers' majority reads its clock as off.
    #[must_use]
    pub const fn off(&self) -> bool {
        matches!(self.standing, Standing::Off { .. })
    }

    /// Why this Mac writes no claim row, in the words a refusal carries.
    #[must_use]
    pub fn refusal(&self) -> Option<String> {
        let Standing::Off { ahead } = self.standing else {
            return None;
        };
        let way = if ahead { "ahead" } else { "behind" };
        Some(if self.silent {
            format!(
                "this Mac's clock read at least {} {way} of {} of the {} Macs it last heard; it \
                 writes no account claim until a peer reads the clock agreeing",
                span_words(self.by_ms),
                self.against,
                self.of,
            )
        } else {
            format!(
                "this Mac's clock reads at least {} {way} of {} of the {} Macs it hears; it \
                 writes no account claim until the clock agrees",
                span_words(self.by_ms),
                self.against,
                self.of,
            )
        })
    }
}

/// `ms` in days, hours and minutes, the two largest units that are not zero ("2 d 3 h", "45
/// min").
#[must_use]
pub fn span_words(ms: u64) -> String {
    let minutes = ms / 60_000;
    let (d, h, m) = (minutes / 1440, minutes / 60 % 24, minutes % 60);
    match (d, h, m) {
        (0, 0, m) => format!("{m} min"),
        (0, h, 0) => format!("{h} h"),
        (0, h, m) => format!("{h} h {m} min"),
        (d, 0, _) => format!("{d} d"),
        (d, h, _) => format!("{d} d {h} h"),
    }
}

/// This Mac's standing from `readings` (by peer machine) at its wall time `now_ms`, with `last`
/// the standing the store last wrote (hysteresis, [`Reading::class`]). It counts each current
/// member of `members` (machine to recipient) other than `me` once, by its reading where fresh
/// and decisive: none is Unknown (but Off stays Off while another member exists: silence, a
/// sleep or a clock set since, clears nothing, and only a fresh reading moves it), a strict
/// majority one way is Off, a strict majority that agrees is Agrees, anything else is Split.
/// Unless Off, it names each member whose decisive reading does not agree as skewed, and holds
/// for each member whose reading still holds it ([`Reading::holding`]). A pure function: every
/// process judges the same file alike on its own clock.
#[must_use]
#[cfg_attr(
    any(test, debug_assertions),
    aterm_spec_anchor::refines(
        machine = "DbClaimClock",
        action = "Judge",
        project = "aterm_db::clock::tests::claim_clock_transition"
    )
)]
pub fn standing(
    readings: &BTreeMap<[u8; 16], Reading>,
    members: &BTreeMap<[u8; 16], String>,
    me: &[u8; 16],
    last: Standing,
    now_ms: u64,
) -> Judgement {
    let counted: Vec<([u8; 16], &Reading, Class)> = members
        .keys()
        .filter(|machine| *machine != me)
        .filter_map(|machine| {
            let reading = readings.get(machine).filter(|r| r.fresh(now_ms))?;
            let class = reading.class(last);
            (class != Class::Undecided).then_some((*machine, reading, class))
        })
        .collect();
    let n = counted.len();
    let count = |class: Class| counted.iter().filter(|(_, _, c)| *c == class).count();
    let (a, b, g) = (
        count(Class::Ahead),
        count(Class::Behind),
        count(Class::Agree),
    );
    let least = |class: Class, offset: fn(&Reading) -> i64| {
        counted
            .iter()
            .filter(|(_, _, c)| *c == class)
            .map(|(_, r, _)| offset(r).max(0).unsigned_abs())
            .min()
            .unwrap_or(0)
    };
    let peers = || members.keys().filter(move |machine| *machine != me);
    let (standing, by_ms, against, of, silent) = if n == 0 {
        match last {
            Standing::Off { ahead } if peers().next().is_some() => {
                // What it last stood on, however old: the figures the refusal says.
                let class = if ahead { Class::Ahead } else { Class::Behind };
                let mut against = 0usize;
                let mut of = 0usize;
                let mut by: Option<u64> = None;
                for r in peers().filter_map(|m| readings.get(m)) {
                    let c = r.class(last);
                    if c != Class::Undecided {
                        of += 1;
                    }
                    if c == class {
                        against += 1;
                        let o = if ahead {
                            r.lo_ms
                        } else {
                            r.hi_ms.saturating_neg()
                        };
                        let abs = o.max(0).unsigned_abs();
                        by = Some(by.map_or(abs, |prev| prev.min(abs)));
                    }
                }
                (last, by.unwrap_or(0), against, of, true)
            }
            _ => (Standing::Unknown, 0, 0, 0, false),
        }
    } else if 2 * a > n {
        (
            Standing::Off { ahead: true },
            least(Class::Ahead, |r| r.lo_ms),
            a,
            n,
            false,
        )
    } else if 2 * b > n {
        (
            Standing::Off { ahead: false },
            least(Class::Behind, |r| r.hi_ms.saturating_neg()),
            b,
            n,
            false,
        )
    } else if 2 * g > n {
        (Standing::Agrees, 0, a + b, n, false)
    } else {
        (Standing::Split, 0, a + b, n, false)
    };
    let named = |machine: &[u8; 16], reading: &Reading, lasts: u64| Skewed {
        machine: *machine,
        recipient: members[machine].clone(),
        until_ms: reading.at_ms.saturating_add(lasts),
        ahead_ms: reading.hi_ms,
    };
    let off = matches!(standing, Standing::Off { .. });
    let skewed = if off {
        Vec::new()
    } else {
        counted
            .iter()
            .filter(|(_, _, class)| *class != Class::Agree)
            .map(|(machine, reading, _)| named(machine, reading, FRESH_MS))
            .collect()
    };
    let held = if off {
        Vec::new()
    } else {
        peers()
            .filter_map(|machine| {
                let reading = readings.get(machine).filter(|r| r.holding(now_ms))?;
                Some(named(machine, reading, HOLD_MS))
            })
            .collect()
    };
    Judgement {
        standing,
        by_ms,
        against,
        of,
        silent,
        skewed,
        held,
    }
}

/// What `clock.state` holds: the last standing the runtime wrote and each peer's latest reading.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct State {
    pub last: Standing,
    pub readings: BTreeMap<[u8; 16], Reading>,
}

impl Default for State {
    fn default() -> Self {
        Self {
            last: Standing::Unknown,
            readings: BTreeMap::new(),
        }
    }
}

impl State {
    /// Its one spelling: [`STATE_MAGIC`], the count (u16), the last standing, then each entry by
    /// machine (machine, at, lo, hi, source), every integer little-endian.
    #[must_use]
    pub fn encode(&self) -> Vec<u8> {
        let count_usize = self.readings.len().min(MAX_READINGS);
        let mut bytes = Vec::with_capacity(STATE_MAGIC.len() + 2 + 1 + count_usize * ENTRY_LEN);
        bytes.extend_from_slice(STATE_MAGIC);
        let count = u16::try_from(count_usize).expect("bounded count");
        bytes.extend_from_slice(&count.to_le_bytes());
        bytes.push(self.last.tag());
        for (machine, reading) in self.readings.iter().take(MAX_READINGS) {
            bytes.extend_from_slice(machine);
            bytes.extend_from_slice(&reading.at_ms.to_le_bytes());
            bytes.extend_from_slice(&reading.lo_ms.to_le_bytes());
            bytes.extend_from_slice(&reading.hi_ms.to_le_bytes());
            bytes.push(match reading.source {
                Source::Mark => 0,
                Source::Ring => 1,
            });
        }
        bytes
    }

    /// The state `bytes` spell, where they spell one in its one spelling.
    #[must_use]
    pub fn decode(bytes: &[u8]) -> Option<Self> {
        let rest = bytes.strip_prefix(STATE_MAGIC)?;
        let (count, rest) = rest.split_first_chunk::<2>()?;
        let count = usize::from(u16::from_le_bytes(*count));
        let (&[tag], rest) = rest.split_first_chunk::<1>()?;
        if count > MAX_READINGS || rest.len() != count * ENTRY_LEN {
            return None;
        }
        let last = Standing::from_tag(tag)?;
        let mut readings = BTreeMap::new();
        let mut prev_machine: Option<[u8; 16]> = None;
        for entry in rest.as_chunks::<ENTRY_LEN>().0 {
            let machine: [u8; 16] = entry[..16].try_into().expect("sixteen");
            // One spelling: sorted, distinct machines.
            if prev_machine.is_some_and(|prev| prev >= machine) {
                return None;
            }
            prev_machine = Some(machine);
            let word = |at: usize| -> [u8; 8] { entry[at..at + 8].try_into().expect("eight") };
            let reading = Reading {
                at_ms: u64::from_le_bytes(word(16)),
                lo_ms: i64::from_le_bytes(word(24)),
                hi_ms: i64::from_le_bytes(word(32)),
                source: match entry[40] {
                    0 => Source::Mark,
                    1 => Source::Ring,
                    _ => return None,
                },
            };
            if reading.lo_ms > reading.hi_ms {
                return None;
            }
            readings.insert(machine, reading);
        }
        Some(Self { last, readings })
    }

    /// The state in `dir` (the claim directory); none, or any other spelling, reads as no
    /// readings.
    #[must_use]
    pub fn read(dir: &Path) -> Self {
        aterm_uds::private_file::read_private(dir, STATE_NAME, STATE_MAX)
            .ok()
            .and_then(|bytes| Self::decode(&bytes))
            .unwrap_or_default()
    }

    /// Write it to `dir`, atomic and private, not durable: a lost file reads as no readings.
    ///
    /// # Errors
    /// The directory cannot be made private or the file cannot be written.
    pub fn write(&self, dir: &Path) -> std::io::Result<()> {
        aterm_uds::private_file::ensure_private_dir(dir)?;
        aterm_uds::private_file::write_private(dir, STATE_NAME, &self.encode())
    }
}

/// The directory `clock.state` lives in: the claim directory of `bootstrap`'s custody, beside the
/// ring states (`crate::table::store::claim_dir`).
pub(crate) fn dir(bootstrap: &aterm_secrets::commons::bootstrap::Bootstrap) -> Option<PathBuf> {
    crate::table::store::claim_dir_of(bootstrap).ok()
}

/// A clock set right, or a day or more ahead or behind (`Ahead`, `Behind`, `Fix`), and what a liar
/// chooses to report (`Lie`): none is code here. `claim_clock_conforms` steps each the way the
/// model does and binds what the code reads of it.
#[cfg(any(test, debug_assertions))]
#[cfg_attr(
    any(test, debug_assertions),
    aterm_spec_anchor::spec_unmodeled(
        machine = "DbClaimClock",
        action = "Ahead",
        reason = "A clock set ahead is the environment: no code chooses it. claim_clock_conforms dates the readings of honest peers by it and voids the held ones, as a clock step makes them stale."
    )
)]
#[cfg_attr(
    any(test, debug_assertions),
    aterm_spec_anchor::spec_unmodeled(
        machine = "DbClaimClock",
        action = "Behind",
        reason = "A clock set behind is the environment: no code chooses it. claim_clock_conforms dates the readings of honest peers by it and voids the held ones, as a clock step dates them in the future."
    )
)]
#[cfg_attr(
    any(test, debug_assertions),
    aterm_spec_anchor::spec_unmodeled(
        machine = "DbClaimClock",
        action = "Fix",
        reason = "A clock set right is the environment: no code chooses it. claim_clock_conforms dates the readings of honest peers by it and voids the held ones."
    )
)]
#[cfg_attr(
    any(test, debug_assertions),
    aterm_spec_anchor::spec_unmodeled(
        machine = "DbClaimClock",
        action = "Lie",
        reason = "What a lying member reports is its own choice: no code here makes it. claim_clock_conforms sends its mark at the wall time the model picks."
    )
)]
#[doc(hidden)]
pub fn db_claim_clock_environment() {}

/// What this Mac's peers say of its clock at its wall time `now_ms`: the [`standing`] of the
/// readings in `bootstrap`'s `clock.state` among `bindings` (the current members), `me` this Mac's
/// machine. No file, or a damaged one, judges Unknown.
pub(crate) fn judge(
    bootstrap: &aterm_secrets::commons::bootstrap::Bootstrap,
    bindings: &[aterm_secrets::commons::binding::Binding],
    me: &[u8; 16],
    now_ms: u64,
) -> Judgement {
    let state = dir(bootstrap)
        .map(|dir| State::read(&dir))
        .unwrap_or_default();
    standing(&state.readings, &members(bindings), me, state.last, now_ms)
}

/// Each of `bindings`' machines and the recipient that names it in the swarm roster, as
/// [`standing`] takes the members.
pub(crate) fn members<'a>(
    bindings: impl IntoIterator<Item = &'a aterm_secrets::commons::binding::Binding>,
) -> BTreeMap<[u8; 16], String> {
    bindings
        .into_iter()
        .map(|binding| (*binding.machine(), binding.recipient().to_bech32()))
        .collect()
}

/// The Commons runtime's store of readings: each peer's latest, the last standing, and when the
/// file was last written. It writes when a reading's class or the standing changes, or every
/// `keep` once some reading changed.
#[derive(Debug)]
pub(crate) struct Clocks {
    state: State,
    /// The standing and classes the file last carried, none before the first write.
    written: Option<(Standing, BTreeMap<[u8; 16], Class>)>,
    /// When the round behind each reading this process took authenticated: a reading replaces
    /// one taken in a later round never, whatever wall time each was taken at. A reading from the
    /// file, taken by an earlier process, gives way to any this one takes.
    heard: BTreeMap<[u8; 16], Instant>,
    changed: bool,
    kept: Option<Instant>,
}

impl Clocks {
    /// The store as `dir` last kept it.
    pub(crate) fn open(dir: &Path) -> Self {
        Self {
            state: State::read(dir),
            written: None,
            heard: BTreeMap::new(),
            changed: false,
            kept: None,
        }
    }

    /// Take `reading` of `machine`, read in a round authenticated at `at`, unless a later round's
    /// is held: rounds are ordered by the steady clock, so a reading taken after this Mac's clock
    /// was set back replaces the ones taken before, which that clock dates in the future.
    pub(crate) fn observe(&mut self, machine: [u8; 16], reading: Reading, at: Instant) {
        if self.heard.get(&machine).is_some_and(|held| *held > at) {
            return;
        }
        self.heard.insert(machine, at);
        if self.state.readings.get(&machine) != Some(&reading) {
            self.state.readings.insert(machine, reading);
            self.changed = true;
        }
    }

    /// Whether a reading changed and `keep` passed since the last write at `now`: the store
    /// writes then though no class moved.
    pub(crate) fn due(&self, now: Instant, keep: Duration) -> bool {
        self.changed
            && self
                .kept
                .is_none_or(|kept| now.saturating_duration_since(kept) >= keep)
    }

    /// Judge the readings of `members` (others dropped) at `now_ms` and write `dir`'s file when a
    /// class or the standing changed since the last write, or `keep` passed since it once a
    /// reading changed. Whether it wrote.
    pub(crate) fn tick(
        &mut self,
        dir: &Path,
        members: &BTreeMap<[u8; 16], String>,
        me: &[u8; 16],
        now_ms: u64,
        now: Instant,
        keep: Duration,
    ) -> bool {
        let before = self.state.readings.len();
        self.state
            .readings
            .retain(|machine, _| members.contains_key(machine) && machine != me);
        self.changed |= self.state.readings.len() != before;
        let judged = standing(&self.state.readings, members, me, self.state.last, now_ms);
        let moved = self.written.as_ref().is_none_or(|(standing, written)| {
            *standing != judged.standing
                || written.len() != self.state.readings.len()
                || self.state.readings.iter().any(|(machine, reading)| {
                    written.get(machine) != Some(&reading.class(self.state.last))
                })
        });
        if !moved && !self.due(now, keep) {
            return false;
        }
        let classes: BTreeMap<[u8; 16], Class> = self
            .state
            .readings
            .iter()
            .map(|(machine, reading)| (*machine, reading.class(self.state.last)))
            .collect();
        self.state.last = judged.standing;
        self.written = Some((judged.standing, classes));
        self.changed = false;
        self.kept = Some(now);
        // A write that fails is tried again at the next change or `keep`, never every tick.
        self.state.write(dir).is_ok()
    }

    #[cfg(test)]
    pub(crate) const fn state(&self) -> &State {
        &self.state
    }
}

#[cfg(test)]
mod tests;
