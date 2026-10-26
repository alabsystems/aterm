// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! THE ASK (`disk-free@v1`, `docs/DESIGN-disk-free-ask-2026-10-24.md`): when the volume the pass
//! removes on stays below its floor after aterm's own passes, or what is left there is not
//! aterm's to free ([`Done`]), ONE agent session on the machine is asked to free space, by one
//! fenced line at its turn break ([`line`]).
//!
//! Every host (one per aterm process) steps one machine-wide lease at the end of each disk look
//! ([`step`] inside [`transact`], under [`LOCK_FILE`] in the harness state directory), so the
//! hosts elect a session between them and every other session on the machine is left alone:
//! - an episode opens only below the floor, once aterm's freeing is [`Done`], and not within
//!   [`COOLDOWN_MS`] of the volume's last line;
//! - it gathers one nominee from each host for [`NOMINATE_MS`] and elects the session whose
//!   checkout holds the most of what the pass left, one at a turn break before one mid-turn
//!   ([`rank`]); a session held where it cannot take a line now (an unspent usage-limit wall, a
//!   box only its owner can answer, the owner's hold or a person's take-over, a draft left in its
//!   prompt: [`Look::held`], [`HeldBy`]) is never nominated, an elected one its host holds so goes
//!   back to the episode once ([`Said::Returned`], [`Entry::returned`]), and a round with nobody
//!   else to elect waits for one to be past ([`Said::Waiting`]) instead of running out; a nominee
//!   whose host ended stands [`ADOPT_MS`] from the first look that read it ended, so a seamless
//!   update's successor nominates it again before another host's look elects without it;
//! - only the elected session's own host types, and only while the lease still names it
//!   ([`claim`], then [`typed`]);
//! - a session that frees nothing within [`ANSWER_MS`] of its line or ends hands the ask to one
//!   other session, as does one that never comes to a turn break within [`DELIVER_MS`] or may not
//!   be asked ([`release`]); after [`MAX_ASKS`] lines or [`MAX_PICKS`] elections the episode is
//!   out ([`Said::Out`]) and the owner's disk line says so;
//! - a pick whose host ended is adopted by the host that took over its session (a seamless
//!   update's successor, a new [`HostId`]), before its look's pass ([`take_over`]), and other
//!   hosts give it [`ADOPT_MS`] to do so, so an update hands nothing off;
//! - the episode ends only when a look reads the volume back past the floor it is held to by
//!   [`END_MARGIN`] (and its free inodes past their line by [`END_FILES_MARGIN`]): the opening
//!   host's floor, raised by any later look of a host whose floor is higher ([`Entry::floor`]),
//!   so hosts whose floors differ, or free space hovering at one, never end and open it look
//!   after look;
//! - a settled or out episode gives way to a new one ([`COOLDOWN_MS`] after it parked and after
//!   the volume's last line) at a look that reads the volume short, so a volume that stays near
//!   its floor is still asked about, at most once a cooldown.

use std::cmp::Ordering;
use std::io;
use std::path::Path;
use std::time::Duration;

use aterm_json::{Map, Value};
use serde::{Deserialize, Serialize};

use super::super::truncate_bytes;
use super::super::usage::rfc3339_utc;
use super::{Hog, Level, ROW_TEXT_CAP, human_bytes};

/// The rule's name: its line, its ledger rows and the log say it.
pub const RULE_DISK_FREE: &str = "disk-free@v1";

/// The lease, in the harness state directory.
pub const LEASE_FILE: &str = "disk-ask.json";

/// The lock every read-modify-write of [`LEASE_FILE`] holds ([`transact`]); the data lives in
/// its own file, replaced by rename, so the lock's inode never changes under a holder.
pub const LOCK_FILE: &str = "disk-ask.lock";

/// The lease's own `kind` in the disk ledger (`<state>/disk.jsonl`).
pub const ROW_KIND: &str = "free-ask";

const SCHEMA: u64 = 1;

/// The largest lease file read.
const LEASE_MAX_BYTES: u64 = 64 * 1024;

/// How long an opened episode gathers every host's nominee before it elects: more than two disk
/// looks (one a minute), so every live host's look nominates at least once.
pub const NOMINATE_MS: i64 = 150_000;

/// How long the elected session has to come to a turn break and take its line.
pub const DELIVER_MS: i64 = 30 * 60_000;

/// After its line, how long the asked session has to free [`FREED_MIN`].
pub const ANSWER_MS: i64 = 20 * 60_000;

/// No new episode opens on a volume within this of its last line, and a settled or out
/// episode gives way to a new one only this long after it parked.
pub const COOLDOWN_MS: i64 = 6 * 3_600_000;

/// A typing host's hold on its pick ([`claim`]): no host hands the ask off for this long after
/// the claim, so a line being typed is never typed twice.
pub const TYPE_GRACE_MS: i64 = 10 * 60_000;

/// How long an owed pick whose host ended stays owed at the looks of hosts that do not
/// supervise its session, from the first such look that read its host ended
/// ([`Phase::Owed`]'s `orphan_ms`), before one of them hands it on as undelivered: a seamless
/// update's successor, which supervises the session, looks first two minutes after it starts and
/// then once a minute (the window's `DISK_FIRST_LOOK` and `DISK_TICK`, held to this there), and
/// takes the pick over as its look begins, before its pass ([`take_over`]), so it adopts the pick
/// before another host's look gives it away however long that pass runs. A host that ended with
/// no successor hands the ask on this much later.
pub const ADOPT_MS: i64 = 3 * 60_000;

/// A rise in free space of at least this, read since the line, is space the asked session freed.
pub const FREED_MIN: u64 = 1 << 30;

/// How far past the floor an episode is held to ([`Entry::floor`]) a look must read free space
/// before the episode ends: the margin aterm's own pressure pass reclaims past the floor before
/// it stops ([`super::PRESSURE_MARGIN_GIB`], `Thresholds::target`), so the ask's episode ends
/// where the pass's pressure does. It is more than [`FREED_MIN`], so a session that freed a GiB
/// settles the episode without ending it, and free space hovering a GiB either side of a floor
/// stays one episode.
pub const END_MARGIN: u64 = super::PRESSURE_MARGIN_GIB << 30;

/// How far past the inode line ([`Look::files_floor`]) a look must read free inodes, where the
/// volume says them, before an episode ends: as many again as the line itself
/// ([`super::EMERGENCY_FILES_FREE`]), so a build that creates and removes thousands of small
/// files between two looks, around the line, keeps one episode and does not end and open it
/// look after look. The line is the same in every host, so no host's is recorded.
pub const END_FILES_MARGIN: u64 = super::EMERGENCY_FILES_FREE;

/// Lines an episode: the elected session's, then at most one other's.
pub const MAX_ASKS: u32 = 2;

/// Elections an episode, lines or not: a session that never takes its line still costs one, but
/// one its host released ([`Miss::Released`]: it may not be asked) costs none, as it is not
/// elected again this episode, and neither does one its host held where it could not take a line
/// before its line ([`Said::Returned`]), as a session goes back to the episode so only once
/// ([`Entry::returned`]): a session is elected at most twice an episode.
pub const MAX_PICKS: u32 = 4;

/// Volumes the lease keeps.
pub const MAX_VOLUMES: usize = 8;

/// Nominees an open round keeps (one per host).
pub const MAX_NOMINEES: usize = 16;

/// The longest a line may be, in characters: typed text a composer holds and the fence reads
/// back, never a pasted-text placeholder (the occupancy lines' budget).
pub const LINE_CAP: usize = 620;

/// The pauses of a bounded wait for [`LOCK_FILE`] ([`transact`]): about 0.6 s in all, so a
/// contended look skips this round and the next look steps.
pub const LOCK_LADDER: [Duration; 6] = [
    Duration::from_millis(10),
    Duration::from_millis(20),
    Duration::from_millis(40),
    Duration::from_millis(80),
    Duration::from_millis(150),
    Duration::from_millis(300),
];

/// One host machine-wide: its process and that process's start ([`start_of`]), so a pid the
/// system hands out again is no one's host.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
pub struct HostId {
    pub pid: u32,
    /// The start's spelling; empty where the system does not say it. On Linux it is the boot
    /// clock's, in nanoseconds since boot ([`BOOT_START`]); elsewhere, and in a lease an older
    /// build wrote, the kernel's start record ([`super::super::restart_stamp::kernel_start`]).
    pub start: String,
}

/// How [`HostId::start`] spells a start read off the boot clock (Linux's `/proc` start ticks,
/// which [`aterm_uds::process::read_process_start`] gives as the nanoseconds since boot at which
/// that tick began): `boot:<ns since boot>`, a spelling no step of the wall clock changes, read
/// with no child process. Only equality is asked of it ([`alive_in`]).
pub const BOOT_START: &str = "boot:";

/// Process `pid`'s start as a host records it ([`HostId::start`]): on Linux off the boot clock
/// ([`BOOT_START`]); elsewhere the kernel's start record, which the kernel keeps as the process
/// began ([`super::super::restart_stamp::kernel_start`]).
#[must_use]
pub fn start_of(pid: u32) -> Option<String> {
    #[cfg(any(target_os = "linux", target_os = "android"))]
    {
        boot_start(pid)
    }
    #[cfg(not(any(target_os = "linux", target_os = "android")))]
    {
        super::super::restart_stamp::kernel_start(pid)
    }
}

/// Process `pid`'s start on the boot clock, spelled [`BOOT_START`]; `None` where it does not run
/// or the system does not say.
#[must_use]
pub fn boot_start(pid: u32) -> Option<String> {
    let start = aterm_uds::process::read_process_start(i32::try_from(pid).ok()?)?;
    Some(format!("{BOOT_START}{}", start.boot.0))
}

/// Whether a host whose start reads `start` still runs, judged in that start's own spelling: a
/// boot-clock one ([`BOOT_START`]) against `boot`, the process's boot-clock start read now; any
/// other, as an older build's lease records it (`ps -o lstart=` on Linux), by `kernel`, so a host
/// recorded before boot-clock starts is not read as ended for its spelling alone. An empty one
/// the system did not say reads as running.
fn alive_in(
    start: &str,
    boot: impl FnOnce() -> Option<String>,
    kernel: impl FnOnce() -> bool,
) -> bool {
    if start.is_empty() {
        true
    } else if start.starts_with(BOOT_START) {
        boot().as_deref() == Some(start)
    } else {
        kernel()
    }
}

impl HostId {
    /// This process, read once.
    #[must_use]
    pub fn this() -> HostId {
        static THIS: std::sync::OnceLock<HostId> = std::sync::OnceLock::new();
        THIS.get_or_init(|| {
            let pid = std::process::id();
            HostId {
                pid,
                start: start_of(pid).unwrap_or_default(),
            }
        })
        .clone()
    }

    /// Whether its process still runs: `false` only where the system says the pid ended or is
    /// another process's ([`alive_in`]). A start the system did not say reads as running, and the
    /// deadlines end what such a host left.
    #[must_use]
    pub fn alive(&self) -> bool {
        alive_in(
            &self.start,
            || boot_start(self.pid),
            || super::super::restart_stamp::alive_as(self.pid, &self.start),
        )
    }
}

/// The floor an episode is held to ([`Entry::floor`]): the free bytes below which host `host`'s
/// look read the volume short ([`Look::floor`]).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Floor {
    pub host: HostId,
    pub bytes: u64,
}

/// Why aterm's own freeing is done on a volume below its floor: what opens an episode.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum Done {
    /// A pass ran every tier it may and the volume stayed below its floor.
    Spent,
    /// `[disk] auto_free_gib = 0`, or a `[disk]` value aterm cannot read, turned removal off.
    CleanupOff,
    /// The volume is out of inodes, which no tier frees.
    Inodes,
}

impl Done {
    /// The ledger's word.
    #[must_use]
    pub const fn word(self) -> &'static str {
        match self {
            Done::Spent => "spent",
            Done::CleanupOff => "cleanup-off",
            Done::Inodes => "inodes",
        }
    }
}

/// One session of this host the ask could go to: a Claude Code the harness supervises, its
/// Autopilot not Off, working on the volume.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Seat {
    pub sid: String,
    /// What the pass's largest holders hold under this session's checkout.
    pub held: u64,
    /// The session rests at a turn break now.
    pub at_break: bool,
}

/// What one of a host's own passes freed on a volume ([`Look::reclaimed`], [`Entry::reclaimed`]):
/// space aterm reclaimed itself, which an asked session's rise never counts.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct Reclaimed {
    /// When the pass ended (unix ms).
    pub ended_ms: i64,
    /// What it freed, as its removals measured it.
    pub bytes: u64,
}

/// How long the lease keeps a pass's record ([`Entry::reclaimed`]): an asked session's answer is
/// judged until [`ANSWER_MS`] after its line, and a line whose record was lost (a full disk) is
/// recorded at its host's next look, so a pass older than this is before every line still judged.
pub const RECLAIMED_KEEP_MS: i64 = 2 * ANSWER_MS + DELIVER_MS;

/// The most passes the lease keeps a record of a volume ([`Entry::reclaimed`]); past it the two
/// oldest are kept as one, ended at the later end, so none of what they freed is ever credited to
/// a session asked between them.
pub const MAX_RECLAIMED: usize = 32;

/// What one host's disk look read of one volume ([`step`]).
pub struct Look<'a> {
    /// Unix ms.
    pub now_ms: i64,
    pub host: &'a HostId,
    /// The volume ([`aterm_sysprobe::volumes::VolumeKey::spelled`]).
    pub key: &'a str,
    pub level: Level,
    pub free: u64,
    /// The free bytes below which this host's [`Self::level`] reads the volume short: its floor,
    /// or with the floor at 0 what writes need ([`super::Thresholds::short_below`]).
    pub floor: u64,
    /// The volume's free inodes; `None` where it does not say them.
    pub files_free: Option<u64>,
    /// The free inodes below which [`Self::level`] reads an emergency
    /// ([`super::Thresholds::files_emergency`]).
    pub files_floor: u64,
    /// `None`: aterm's own passes may still free space here.
    pub done: Option<Done>,
    /// This host's sessions the ask could go to.
    pub seats: &'a [Seat],
    /// This host's supervised sessions, Autopilot not Off: an elected one that leaves them
    /// before its line, and is not [`Self::barred`], hands the ask on as undelivered.
    pub supervised: &'a [String],
    /// Those of this host's live sessions the seat rule bars by a fact that lasts past this
    /// look (their task is done or exited, their worker found them unfit at their last turn
    /// break, their agent is no longer Claude Code, or their directory is on another volume):
    /// an elected one among them is released ([`Miss::Released`]), so it costs no election,
    /// whether or not it is still supervised, and whether it was this host's pick or that of a
    /// host this one replaced (a seamless update's successor adopts it first).
    pub barred: &'a [String],
    /// Every live session of this host: an asked one that leaves them hands the ask on.
    pub live: &'a [String],
    /// This host's sessions the seat rule would seat but for what holds them now and passes, each
    /// with what holds it ([`HeldBy`]): an unspent usage-limit wall, a box escalated to their
    /// owner, the owner's hold or a person's take-over, a draft left standing in the composer, the
    /// loop's credits hold, a sign-in wall, or another wall its loop waits at.
    /// None of them is in [`Self::seats`]; an elected one among them goes back to the episode at
    /// no election's cost ([`Said::Returned`]), as it could not take its line before its
    /// deadline, and is nominated again once past; a round with nobody else to elect waits for one
    /// of them to be past ([`Said::Waiting`], [`Entry::held`]) instead of running out.
    pub held: &'a [(String, HeldBy)],
    /// What this host's own passes freed on the volume since its last step the lease took, each
    /// with when it ended: recorded in an open episode ([`Entry::reclaimed`]), so an asked
    /// session's rise counts only space aterm did not free itself, whichever host's pass freed it.
    pub reclaimed: &'a [Reclaimed],
    /// Another instance's pass holds the pass lock as this host looks: it may be freeing space
    /// that no host has recorded yet, so this look settles no asked episode.
    pub passing: bool,
    /// Whether another host's process still runs ([`HostId::alive`]).
    pub alive: &'a dyn Fn(&HostId) -> bool,
}

/// A session the lease elected, on its host.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Pick {
    pub host: HostId,
    pub sid: String,
}

/// One host's best seat in an open round.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Nominee {
    pub host: HostId,
    pub sid: String,
    pub held: u64,
    pub at_break: bool,
    /// Unix ms.
    pub seen_ms: i64,
    /// When a look of another host that does not run this session first read its host ended
    /// ([`stands`]): the nominee stands [`ADOPT_MS`] from then, so a seamless update's
    /// successor nominates the session again before the round elects without it. `None` while
    /// its host runs, and in a lease an older build wrote.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub orphan_ms: Option<i64>,
}

/// What holds a session where it cannot take a line now ([`Look::held`], [`Held::why`]): each
/// passes, so the session is held, not barred.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize)]
pub enum HeldBy {
    /// An unspent usage-limit wall: it passes at the limit's reset.
    #[serde(rename = "limited")]
    Limit,
    /// A box its loop escalated to its owner: it passes once the owner answers it.
    #[serde(rename = "box")]
    Box,
    /// The owner's hold or a person's take-over (`status hold=1`): it passes once lifted.
    #[serde(rename = "hold")]
    Hold,
    /// A draft left standing in its composer: it passes once the draft is sent or cleared.
    #[serde(rename = "draft")]
    Draft,
    /// Its loop's credits hold (`credits-hold@v1`, `[harness] usage_credits = false`): usage
    /// credits would carry its next turn, so the loop types nothing until its usage limit resets.
    #[serde(rename = "credits")]
    Credits,
    /// A sign-in wall (`wall:auth`): its login is gone, and only a person's browser brings it back.
    #[serde(rename = "sign-in")]
    SignIn,
    /// Another wall that is no usage limit, which its loop waits at (an API error, an overload,
    /// a full context, the memory banner): it passes once the wall does.
    #[serde(rename = "wall")]
    Wall,
}

impl HeldBy {
    /// Every hold, in order.
    pub const ALL: [HeldBy; 7] = [
        HeldBy::Limit,
        HeldBy::Box,
        HeldBy::Hold,
        HeldBy::Draft,
        HeldBy::Credits,
        HeldBy::SignIn,
        HeldBy::Wall,
    ];

    /// The word a look, the lease's record and a turn break's `wait:<why>` say.
    #[must_use]
    pub const fn word(self) -> &'static str {
        match self {
            HeldBy::Limit => "limited",
            HeldBy::Box => "box",
            HeldBy::Hold => "hold",
            HeldBy::Draft => "draft",
            HeldBy::Credits => "credits",
            HeldBy::SignIn => "sign-in",
            HeldBy::Wall => "wall",
        }
    }

    /// The hold `word` names ([`Self::word`]); `None` for any other word.
    #[must_use]
    pub fn from_word(word: &str) -> Option<HeldBy> {
        HeldBy::ALL.into_iter().find(|h| h.word() == word)
    }

    /// Where it holds a session, as the owner reads it on their own line (`you`) or anyone reads
    /// it elsewhere.
    const fn at(self, you: bool) -> &'static str {
        match (self, you) {
            (HeldBy::Limit, _) => "at a usage limit",
            (HeldBy::Box, true) => "at a box only you can answer",
            (HeldBy::Box, false) => "at a box only its owner can answer",
            (HeldBy::Hold, true) => "on hold (yours, or a person's take-over)",
            (HeldBy::Hold, false) => "on hold (its owner's, or a person's take-over)",
            (HeldBy::Draft, _) => "with a draft left in its prompt",
            (HeldBy::Credits, _) => "waiting out its usage limit rather than spend usage credits",
            (HeldBy::SignIn, true) => "at a sign-in only you can do",
            (HeldBy::SignIn, false) => "at a sign-in only its owner can do",
            (HeldBy::Wall, _) => "at an error its loop waits out",
        }
    }

    /// What ends it.
    const fn past(self) -> &'static str {
        match self {
            HeldBy::Limit | HeldBy::Box | HeldBy::Wall => "it is past",
            HeldBy::Hold => "its hold is lifted",
            HeldBy::Draft => "its draft is sent or cleared",
            HeldBy::Credits => "its usage limit resets",
            HeldBy::SignIn => "it is signed in again",
        }
    }
}

/// `phrases`, each said once, joined as a person reads a choice: `a`, `a or b`, `a, b or c`.
fn either(phrases: impl IntoIterator<Item = &'static str>) -> String {
    let mut said: Vec<&str> = Vec::new();
    for p in phrases {
        if !said.contains(&p) {
            said.push(p);
        }
    }
    match said.split_last() {
        None => String::new(),
        Some((last, [])) => (*last).to_owned(),
        Some((last, rest)) => format!("{} or {last}", rest.join(", ")),
    }
}

/// Where `why` holds a session ([`HeldBy::at`]), every hold said once: `you`, as the owner reads
/// it on their own line.
fn held_at(why: &[HeldBy], you: bool) -> String {
    either(why.iter().map(|h| h.at(you)))
}

/// What ends the holds `why` ([`HeldBy::past`]), each said once.
fn held_past(why: &[HeldBy]) -> String {
    either(why.iter().map(|h| h.past()))
}

/// The hold a lease records ([`Held::why`]): a word this build does not know reads as none.
fn held_by_word<'de, D: serde::Deserializer<'de>>(d: D) -> Result<Option<HeldBy>, D::Error> {
    Ok(Option::<String>::deserialize(d)?
        .as_deref()
        .and_then(HeldBy::from_word))
}

/// One of a host's sessions held where it cannot take a line now ([`Look::held`]), as that host's
/// last look read it ([`Entry::held`]).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Held {
    pub host: HostId,
    pub sid: String,
    /// What holds it, as its host read it; `None` in a lease an older build wrote, which a reader
    /// says as a usage limit or a box ([`HeldBy::Limit`], [`HeldBy::Box`]), the holds that build
    /// knew.
    #[serde(
        default,
        skip_serializing_if = "Option::is_none",
        deserialize_with = "held_by_word"
    )]
    pub why: Option<HeldBy>,
    /// When a look of another host that does not run this session first read its host ended
    /// ([`stands`]), as for a [`Nominee`].
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub orphan_ms: Option<i64>,
}

impl Held {
    /// What holds it, as a reader says it: a record an older build wrote, with no hold, is said as
    /// either hold that build knew.
    fn holds(&self) -> &'static [HeldBy] {
        match self.why {
            Some(HeldBy::Limit) => &[HeldBy::Limit],
            Some(HeldBy::Box) => &[HeldBy::Box],
            Some(HeldBy::Hold) => &[HeldBy::Hold],
            Some(HeldBy::Draft) => &[HeldBy::Draft],
            Some(HeldBy::Credits) => &[HeldBy::Credits],
            Some(HeldBy::SignIn) => &[HeldBy::SignIn],
            Some(HeldBy::Wall) => &[HeldBy::Wall],
            None => &[HeldBy::Limit, HeldBy::Box],
        }
    }
}

/// What `held` (a look's [`Look::held`]) says holds session `sid`; `None`: it is not held there.
fn holds(held: &[(String, HeldBy)], sid: &str) -> Option<HeldBy> {
    held.iter().find(|(s, _)| s == sid).map(|(_, why)| *why)
}

/// Why a pick did not free the volume: what hands the ask on.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum Miss {
    /// It came to no turn break within [`DELIVER_MS`], left its host's supervision, or its host
    /// ended and no host adopted it within [`ADOPT_MS`], before its line.
    Undelivered,
    /// Its host found it may not be asked ([`Look::barred`], or at its turn break its worker's
    /// finding: its task finished, landed or exited, no task yet, its Autopilot Off), or held it
    /// where it cannot take a line now ([`Look::held`]: an unspent usage-limit wall, a box only
    /// its owner can answer, the owner's hold or a person's take-over, a draft left in its prompt)
    /// after it went back to the episode once ([`Entry::returned`]), before its line. A first
    /// such hold returns it instead ([`Said::Returned`]).
    Released,
    /// Its session ended after its line.
    Ended,
    /// Free space did not rise [`FREED_MIN`] within [`ANSWER_MS`] of its line.
    NothingFreed,
}

impl Miss {
    /// The ledger's word.
    #[must_use]
    pub const fn word(self) -> &'static str {
        match self {
            Miss::Undelivered => "undelivered",
            Miss::Released => "released",
            Miss::Ended => "ended",
            Miss::NothingFreed => "nothing-freed",
        }
    }
}

/// Where a volume's episode stands.
#[derive(Debug, Clone, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(tag = "state", rename_all = "kebab-case")]
pub enum Phase {
    /// No episode.
    #[default]
    Idle,
    /// Gathering one nominee from each host until `until_ms`; `after`: why the last pick missed.
    Nominating {
        until_ms: i64,
        nominees: Vec<Nominee>,
        after: Option<Miss>,
    },
    /// Elected, its line not typed yet; its host types it at a turn break before `until_ms`.
    Owed {
        pick: Pick,
        elected_ms: i64,
        until_ms: i64,
        /// When a look of a host that does not supervise the pick's session first read the
        /// pick's host ended: such looks hand it on only [`ADOPT_MS`] later, so a successor
        /// adopts it first. `None` while its host runs, and in a lease an older build wrote.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        orphan_ms: Option<i64>,
    },
    /// Its line went at `typed_ms`; `free_at` the free figure then, `best_free` the most read since.
    Asked {
        pick: Pick,
        typed_ms: i64,
        until_ms: i64,
        free_at: u64,
        best_free: u64,
    },
    /// The asked session freed `freed` bytes: nobody else is asked this episode.
    Settled { at_ms: i64, sid: String, freed: u64 },
    /// No more asks this episode; `last`: why the last pick missed (`None`: nobody was nominated,
    /// and nobody was held: [`Said::Waiting`]).
    Out { at_ms: i64, last: Option<Miss> },
}

impl Phase {
    /// The reader's word.
    #[must_use]
    pub const fn word(&self) -> &'static str {
        match self {
            Phase::Idle => "idle",
            Phase::Nominating { .. } => "nominating",
            Phase::Owed { .. } => "owed",
            Phase::Asked { .. } => "asked",
            Phase::Settled { .. } => "settled",
            Phase::Out { .. } => "out",
        }
    }
}

/// One volume's lease.
#[derive(Debug, Clone, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(default)]
pub struct Entry {
    pub key: String,
    /// When the episode opened (unix ms); `None` between episodes.
    pub episode_ms: Option<i64>,
    /// Why it opened.
    pub done: Option<Done>,
    /// The floor the episode is held to: the opening host's, replaced at every later look that
    /// does not end the episode by that host's own floor, unless this one is another live
    /// host's at least as high. A look ends the episode only once it reads free space at or
    /// above this (while its host lives) and its own floor, plus [`END_MARGIN`]; after any look
    /// that steps the episode it is what that look held it to. `None` between episodes, and in a
    /// lease written before floors were recorded: a look then holds the episode to its own floor
    /// and records it.
    pub floor: Option<Floor>,
    pub phase: Phase,
    /// The sessions elected this episode, in order: none is elected again while it is here, and
    /// one leaves it only as it goes back to the episode ([`Self::returned`]).
    pub picks: Vec<Pick>,
    /// Of [`Self::picks`], those their hosts released ([`Miss::Released`]).
    pub released: u32,
    /// The sessions elected this episode that their hosts held where they could not take a line
    /// before their line ([`Look::held`]: a usage-limit wall, a box only their owner can answer,
    /// the owner's hold or a person's take-over, a draft left in their prompt), in order: each
    /// went back to the episode ([`Said::Returned`]), leaving [`Self::picks`] at no election's
    /// cost, so it is nominated again once past and a round with nobody else to elect waits for
    /// it. A session goes back so once an episode: held again after its next election, it is
    /// released ([`Miss::Released`]) and stays a pick, so a session whose turns keep stopping at
    /// boxes does not keep the ask from every other session. None took a line: a host records a line
    /// whose record was lost before its look steps the lease ([`typed_unrecorded`]), so a pick
    /// goes back only while it is owed its line.
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub returned: Vec<String>,
    /// Lines typed this episode.
    pub asks: u32,
    /// The sessions that took a line this episode, in order.
    pub asked: Vec<String>,
    /// The volume's last line (unix ms), across episodes ([`COOLDOWN_MS`]).
    pub last_ask_ms: Option<i64>,
    /// Every host's sessions held where they cannot take a line now ([`Look::held`]), as each
    /// host's last look that stepped this volume's open episode read them: what a round with
    /// nobody else to elect waits on ([`Said::Waiting`]). A host's are replaced at each of its
    /// looks; another host's stand while it runs, and for [`ADOPT_MS`] after a look first read it
    /// ended ([`stands`]), unless a host that runs the session took the record over as its look
    /// began ([`take_over`]). They are kept only while an episode is open: no look steps a volume
    /// read clear with none open, so a record kept past its episode would go stale there. An
    /// episode starts with the opening look's alone, and every live host's look records its own
    /// within the round's [`NOMINATE_MS`].
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub held: Vec<Held>,
    /// What aterm's own passes freed on the volume while the episode was open, as each host's
    /// look reported its own ([`Look::reclaimed`]), for [`RECLAIMED_KEEP_MS`] and at most
    /// [`MAX_RECLAIMED`]: an asked session's rise is measured past every pass that ended at or
    /// after its line ([`aterm_since`]), so the cleanup pass that keeps running while the volume
    /// is short is never credited to the session asked. Kept only while an episode is open.
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub reclaimed: Vec<Reclaimed>,
}

impl Entry {
    fn close(&mut self) {
        *self = Entry {
            key: std::mem::take(&mut self.key),
            last_ask_ms: self.last_ask_ms,
            ..Entry::default()
        };
    }
}

/// The machine-wide lease ([`LEASE_FILE`]).
#[derive(Debug, Clone, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(default)]
pub struct Lease {
    pub schema: u64,
    pub volumes: Vec<Entry>,
}

impl Lease {
    /// The lease `text` holds; an empty one where it is not this schema's.
    #[must_use]
    pub fn decode(text: &str) -> Lease {
        aterm_json::from_str::<Lease>(text)
            .ok()
            .filter(|l| l.schema == SCHEMA)
            .map(|mut l| {
                l.volumes.truncate(MAX_VOLUMES);
                l
            })
            .unwrap_or_default()
    }

    /// The lease as its file holds it.
    #[must_use]
    pub fn encode(&self) -> String {
        let mut out = self.clone();
        out.schema = SCHEMA;
        aterm_json::to_string(&out).unwrap_or_else(|_| "{}".to_owned())
    }

    /// Volume `key`'s entry.
    #[must_use]
    pub fn entry(&self, key: &str) -> Option<&Entry> {
        self.volumes.iter().find(|e| e.key == key)
    }

    fn entry_at(&mut self, key: &str) -> Option<&mut Entry> {
        self.volumes.iter_mut().find(|e| e.key == key)
    }

    /// Volume `key`'s entry, made where there is none: past [`MAX_VOLUMES`] an idle entry goes
    /// first, the one whose last line is oldest.
    fn entry_mut(&mut self, key: &str) -> &mut Entry {
        let at = match self.volumes.iter().position(|e| e.key == key) {
            Some(at) => at,
            None => {
                if self.volumes.len() >= MAX_VOLUMES {
                    let drop = self
                        .volumes
                        .iter()
                        .enumerate()
                        .min_by_key(|(_, e)| (e.episode_ms.is_some(), e.last_ask_ms))
                        .map_or(0, |(i, _)| i);
                    self.volumes.remove(drop);
                }
                self.volumes.push(Entry {
                    key: key.to_owned(),
                    ..Entry::default()
                });
                self.volumes.len() - 1
            }
        };
        &mut self.volumes[at]
    }
}

/// What one step decided ([`step`], [`release`]): one closed word ([`Said::word`]) and its facts.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Said {
    /// At or above the floor with no episode, or the volume unread.
    Quiet,
    /// Below the floor, and aterm's own passes may still free space here.
    Freeing,
    /// Would open, but the volume's last line was within [`COOLDOWN_MS`].
    Cooldown {
        last_ms: i64,
    },
    /// An episode opened.
    Opened {
        done: Done,
    },
    /// A round gathers nominees; `seat`: this host's.
    Nominating {
        until_ms: i64,
        seat: Option<String>,
    },
    Elected {
        pick: Pick,
        held: u64,
    },
    /// The elected session's line is still to be typed.
    Owed {
        pick: Pick,
        until_ms: i64,
    },
    /// Its line went; `rose`: what free space rose since.
    Asked {
        pick: Pick,
        until_ms: i64,
        rose: u64,
    },
    Freed {
        sid: String,
        freed: u64,
    },
    /// The pick missed; a new round elects another session.
    Handoff {
        from: Pick,
        why: Miss,
    },
    /// Its host held the pick where it cannot take a line before its line ([`Look::held`]: a
    /// usage-limit wall, a box only its owner can answer, the owner's hold or a person's
    /// take-over, a draft left in its prompt), `why` saying which: it goes back to the episode at
    /// no election's cost ([`Entry::returned`]), and a new round elects another session, or it
    /// once past.
    Returned {
        from: Pick,
        why: HeldBy,
    },
    /// The round's time is over, but every session it could elect is held where it cannot take a
    /// line now ([`Look::held`]), `why` saying by what, each hold once: it waits for one to be
    /// past, and elects at the first look after.
    Waiting {
        held: Vec<String>,
        why: Vec<HeldBy>,
    },
    /// No more asks this episode: the owner's line says so. `asked`: a line went this episode.
    Out {
        last: Option<Miss>,
        asked: bool,
    },
    /// The episode was settled by a session that freed space.
    Settled,
    /// The episode is out.
    Spent,
    /// Back past the floor the episode was held to by [`END_MARGIN`]: the episode ended.
    Ended {
        asks: u32,
    },
}

impl Said {
    /// The ledger's word.
    #[must_use]
    pub const fn word(&self) -> &'static str {
        match self {
            Said::Quiet => "quiet",
            Said::Freeing => "freeing",
            Said::Cooldown { .. } => "cooldown",
            Said::Opened { .. } => "opened",
            Said::Nominating { .. } => "nominating",
            Said::Elected { .. } => "elected",
            Said::Owed { .. } => "owed",
            Said::Asked { .. } => "asked",
            Said::Freed { .. } => "freed",
            Said::Handoff { .. } => "handoff",
            Said::Returned { .. } => "returned",
            Said::Waiting { .. } => "waiting",
            Said::Out { .. } => "out",
            Said::Settled => "settled",
            Said::Spent => "spent",
            Said::Ended { .. } => "ended",
        }
    }

    /// Whether this step moved the episode (a row the moving host journals), not a standing
    /// state (a row each host journals when its word changes).
    #[must_use]
    pub const fn moves(&self) -> bool {
        matches!(
            self,
            Said::Opened { .. }
                | Said::Elected { .. }
                | Said::Freed { .. }
                | Said::Handoff { .. }
                | Said::Returned { .. }
                | Said::Out { .. }
                | Said::Ended { .. }
        )
    }

    /// The session it names.
    #[must_use]
    pub fn sid(&self) -> &str {
        match self {
            Said::Elected { pick, .. } | Said::Owed { pick, .. } | Said::Asked { pick, .. } => {
                &pick.sid
            }
            Said::Handoff { from, .. } | Said::Returned { from, .. } => &from.sid,
            Said::Freed { sid, .. } => sid,
            Said::Nominating {
                seat: Some(sid), ..
            } => sid,
            _ => "",
        }
    }

    /// One sentence a person reads.
    #[must_use]
    pub fn describe(&self) -> String {
        match self {
            Said::Quiet => "the volume is at or above its floor".to_owned(),
            Said::Freeing => {
                "below the floor, and aterm's own passes may still free space: no agent is asked"
                    .to_owned()
            }
            Said::Cooldown { last_ms } => format!(
                "below the floor, but an agent was asked at {}: none is asked again before {}",
                rfc3339_utc(last_ms / 1000),
                rfc3339_utc(last_ms.saturating_add(COOLDOWN_MS) / 1000)
            ),
            Said::Opened { done } => format!(
                "below the floor and aterm's freeing is done ({}): one agent will be asked",
                done.word()
            ),
            Said::Nominating { until_ms, seat } => format!(
                "gathering every host's nominee until {}{}",
                rfc3339_utc(until_ms / 1000),
                seat.as_ref()
                    .map(|s| format!("; this host's is @{s}"))
                    .unwrap_or_default()
            ),
            Said::Elected { pick, held } => format!(
                "@{} (host pid {}) is asked: its checkout holds {} of what aterm could not free",
                pick.sid,
                pick.host.pid,
                human_bytes(*held)
            ),
            Said::Owed { pick, until_ms } => format!(
                "@{}'s line goes at its next turn break, by {}",
                pick.sid,
                rfc3339_utc(until_ms / 1000)
            ),
            Said::Asked {
                pick,
                until_ms,
                rose,
            } => format!(
                "@{} was asked; free space rose {} (it needs {} by {})",
                pick.sid,
                human_bytes(*rose),
                human_bytes(FREED_MIN),
                rfc3339_utc(until_ms / 1000)
            ),
            Said::Freed { sid, freed } => format!(
                "@{sid} freed {}: nobody else is asked this episode",
                human_bytes(*freed)
            ),
            Said::Handoff { from, why } => format!(
                "@{} missed ({}): one other session is asked",
                from.sid,
                why.word()
            ),
            Said::Returned { from, why } => format!(
                "@{} is {} before its line: it costs no election, and a new round asks another \
                 session, or it once {}",
                from.sid,
                why.at(false),
                why.past()
            ),
            Said::Waiting { held, why } => format!(
                "every session that could be asked is {} ({}): one is asked once {}",
                held_at(why, false),
                held.iter()
                    .map(|s| format!("@{s}"))
                    .collect::<Vec<_>>()
                    .join(", "),
                held_past(why)
            ),
            Said::Out { last: None, .. } => {
                "no host had a session to ask: the owner's disk line says so".to_owned()
            }
            Said::Out {
                last: Some(why),
                asked: true,
            } => format!(
                "the agents asked did not free it (last: {}): no more are asked; the owner's disk \
                 line says so",
                why.word()
            ),
            Said::Out {
                last: Some(why),
                asked: false,
            } => format!(
                "no session took its line (last: {}): no more are asked; the owner's disk line \
                 says so",
                why.word()
            ),
            Said::Settled => "an agent freed space this episode: nobody else is asked".to_owned(),
            Said::Spent => "this episode's asks are spent: no more agents are asked".to_owned(),
            Said::Ended { asks } => format!(
                "back {} past the floor it was held to: the episode ended after {asks} line(s)",
                human_bytes(END_MARGIN)
            ),
        }
    }
}

/// Which of two candidates `(held, at_break, sid)` the ask goes to first ([`Ordering::Greater`]:
/// `a`): the one whose checkout holds more of what the pass left, in whole GiB; then one at a turn
/// break; then more bytes; then the lower sid.
#[must_use]
pub fn rank(a: (u64, bool, &str), b: (u64, bool, &str)) -> Ordering {
    (a.0 >> 30)
        .cmp(&(b.0 >> 30))
        .then(a.1.cmp(&b.1))
        .then(a.0.cmp(&b.0))
        .then_with(|| b.2.cmp(a.2))
}

/// Step volume `look.key`'s lease over what one host's look read: open, nominate, elect, hand
/// off, settle, run out or end the episode, or give a parked one way to a new one. Only the
/// elected session's host types anything ([`claim`]). A look that does not read the volume
/// short steps an open episode too, unless it reads the volume clear of what the episode is
/// held to ([`ends`]), which ends it.
#[must_use]
pub fn step(lease: &mut Lease, look: &Look<'_>) -> Said {
    if look.level == Level::Unknown {
        return Said::Quiet;
    }
    let below = look.level >= Level::Short;
    let entry = if below {
        lease.entry_mut(look.key)
    } else {
        match lease.entry_at(look.key) {
            Some(e) if e.episode_ms.is_some() => {
                if ends(e, look) {
                    let asks = e.asks;
                    e.close();
                    return Said::Ended { asks };
                }
                e
            }
            _ => return Said::Quiet,
        }
    };
    if entry.episode_ms.is_some() {
        hold(entry, look);
        note_held(entry, look);
        note_reclaimed(entry, look);
    }
    let now = look.now_ms;
    match entry.phase.clone() {
        Phase::Idle if below => open(entry, look),
        Phase::Idle => Said::Quiet,
        Phase::Nominating {
            until_ms, after, ..
        } => {
            nominate(entry, look);
            if now < until_ms {
                let seat = match &entry.phase {
                    Phase::Nominating { nominees, .. } => nominees
                        .iter()
                        .find(|n| n.host == *look.host)
                        .map(|n| n.sid.clone()),
                    _ => None,
                };
                return Said::Nominating { until_ms, seat };
            }
            elect(entry, look, after)
        }
        Phase::Owed {
            pick,
            until_ms,
            orphan_ms,
            ..
        } => {
            // Adopted where this host supervises the session or its seat rule bars it: a pick
            // whose agent exited around a seamless update has left the successor's supervised
            // roster, and the successor releases it below at no election's cost.
            let pick = adopt(
                entry,
                pick,
                look.host,
                look.alive,
                &owed_adopters(look.supervised, look.barred),
            );
            let here = pick.host == *look.host;
            let barred = here && look.barred.contains(&pick.sid);
            #[cfg(any(test, debug_assertions))]
            let barred = barred && !fault::armed(fault::Fault::KeepBarred);
            // A pick its host holds where it cannot take a line (`Look::held`: a wall, a box, a
            // hold, a draft) could not take its line before its deadline: it goes back to the
            // episode at no election's cost, as it is fit again once past ([`give_back`]).
            let walled = if here {
                holds(look.held, &pick.sid)
            } else {
                None
            };
            #[cfg(any(test, debug_assertions))]
            let walled = walled.filter(|_| !fault::armed(fault::Fault::KeepHeld));
            // A pick whose host ended, at the look of a host that does not supervise its session:
            // owed on for [`ADOPT_MS`] from the first such look, so a successor adopts it first.
            let orphan = (!here && !(look.alive)(&pick.host)).then(|| orphan_ms.unwrap_or(now));
            let wait = ADOPT_MS;
            #[cfg(any(test, debug_assertions))]
            let wait = if fault::armed(fault::Fault::OrphanAtOnce) {
                0
            } else {
                wait
            };
            let gone = if here {
                !look.supervised.contains(&pick.sid)
            } else {
                orphan.is_some_and(|at| now.saturating_sub(at) >= wait)
            };
            #[cfg(any(test, debug_assertions))]
            let barred = barred && !(gone && fault::armed(fault::Fault::GoneFirst));
            // A pick its host bars is released whether or not it is still supervised: an agent
            // that exited leaves the supervised roster, and its task record says why.
            if barred {
                hand_off(entry, now, pick, Miss::Released)
            } else if let Some(why) = walled {
                give_back(entry, now, pick, why)
            } else if gone || now >= until_ms {
                hand_off(entry, now, pick, Miss::Undelivered)
            } else {
                if let Phase::Owed { orphan_ms, .. } = &mut entry.phase {
                    *orphan_ms = orphan;
                }
                Said::Owed { pick, until_ms }
            }
        }
        Phase::Asked {
            pick,
            typed_ms,
            until_ms,
            free_at,
            best_free,
        } => {
            let pick = adopt(entry, pick, look.host, look.alive, &[look.live]);
            let best = best_free.max(look.free);
            // The rise past the line's figure and past what aterm's own passes freed since, on any
            // host: the cleanup pass runs on while the volume is short, and what it reclaims is
            // not the asked session's.
            let rose = best
                .saturating_sub(free_at)
                .saturating_sub(aterm_since(entry, typed_ms));
            // Another instance's pass may be freeing space no host has recorded yet: the rise is
            // judged at a look after it, which reads its record.
            let passing = look.passing;
            #[cfg(any(test, debug_assertions))]
            let passing = passing && !fault::armed(fault::Fault::SettleWhilePassing);
            if rose >= FREED_MIN && passing {
                entry.phase = Phase::Asked {
                    pick: pick.clone(),
                    typed_ms,
                    until_ms,
                    free_at,
                    best_free: best,
                };
                return Said::Asked {
                    pick,
                    until_ms,
                    rose,
                };
            }
            if rose >= FREED_MIN {
                entry.phase = Phase::Settled {
                    at_ms: now,
                    sid: pick.sid.clone(),
                    freed: rose,
                };
                return Said::Freed {
                    sid: pick.sid,
                    freed: rose,
                };
            }
            entry.phase = Phase::Asked {
                pick: pick.clone(),
                typed_ms,
                until_ms,
                free_at,
                best_free: best,
            };
            if pick.host == *look.host && !look.live.contains(&pick.sid) {
                hand_off(entry, now, pick, Miss::Ended)
            } else if now >= until_ms {
                hand_off(entry, now, pick, Miss::NothingFreed)
            } else {
                Said::Asked {
                    pick,
                    until_ms,
                    rose,
                }
            }
        }
        Phase::Settled { at_ms, .. } | Phase::Out { at_ms, .. }
            if below && gives_way(entry, at_ms, look) =>
        {
            entry.close();
            open(entry, look)
        }
        Phase::Settled { .. } => Said::Settled,
        Phase::Out { .. } => Said::Spent,
    }
}

/// The rosters among which a host adopts an owed pick whose host ended ([`adopt`]): its
/// supervised sessions and those its seat rule bars.
fn owed_adopters<'a>(supervised: &'a [String], barred: &'a [String]) -> Vec<&'a [String]> {
    #[cfg(any(test, debug_assertions))]
    if fault::armed(fault::Fault::AdoptSupervisedOnly) {
        return vec![supervised];
    }
    vec![supervised, barred]
}

/// `pick`, moved to host `host` when its own host ended (`alive` says not) and `host` has its
/// session among `held` (its supervised sessions and those its seat rule bars while the line is
/// owed, its live ones once it went): a seamless update's successor adopts its sessions under the
/// same sids as a new [`HostId`], so the pick moves to it, in the phase and in [`Entry::picks`],
/// and its line is typed, its release made or its answer judged there, instead of the ask being
/// handed on as though its session had gone and the session kept out of the episode.
fn adopt(
    entry: &mut Entry,
    pick: Pick,
    host: &HostId,
    alive: &dyn Fn(&HostId) -> bool,
    held: &[&[String]],
) -> Pick {
    if pick.host == *host || !held.iter().any(|sids| sids.contains(&pick.sid)) || alive(&pick.host)
    {
        return pick;
    }
    #[cfg(any(test, debug_assertions))]
    if fault::armed(fault::Fault::NoAdopt) {
        return pick;
    }
    let moved = Pick {
        host: host.clone(),
        sid: pick.sid.clone(),
    };
    for p in &mut entry.picks {
        if *p == pick {
            p.clone_from(&moved);
        }
    }
    if let Phase::Owed { pick: p, .. } | Phase::Asked { pick: p, .. } = &mut entry.phase {
        p.clone_from(&moved);
    }
    moved
}

/// What a host reads of its own sessions before its look's pass ([`take_over`]): only what the
/// pass does not change. Its fields are [`Look`]'s.
pub struct Roster<'a> {
    pub host: &'a HostId,
    /// The volume ([`aterm_sysprobe::volumes::VolumeKey::spelled`]).
    pub key: &'a str,
    /// [`Look::supervised`].
    pub supervised: &'a [String],
    /// [`Look::barred`].
    pub barred: &'a [String],
    /// [`Look::live`].
    pub live: &'a [String],
    /// [`Look::held`].
    pub held: &'a [(String, HeldBy)],
    /// Whether another host's process still runs ([`HostId::alive`]).
    pub alive: &'a dyn Fn(&HostId) -> bool,
}

/// Before host `r.host`'s look's pass, take over what a host that ended left in volume `r.key`'s
/// open episode of the sessions `r.host` runs: a seamless update's successor runs them under the
/// same sids, so they move to it, and every other host's look then reads their host running. That
/// is the owed pick of a session it supervises or bars, and the asked pick of a live one
/// ([`adopt`]); the nominee of a session it supervises and bars by no lasting fact, with its
/// figures kept until its own nomination replaces it, or, where it holds that session
/// ([`Roster::held`]), a record of it held instead; and the record of a held session
/// ([`Entry::held`]), whatever this host's seat rule reads of that session before its pass. An
/// ended host's nominee of a session this host bars or does not supervise is dropped, as the step
/// after the pass would drop it: that step reads the same roster. Nothing else moves: no round
/// elects, no pick is handed on, released or given back, no floor is recorded, so the step after
/// the pass ([`step`]) judges all of that as before, and replaces what was taken over with what
/// its own reading nominates and holds, under one lock.
///
/// A look's pass can run for a minute or more (discovery up to 20 s, each profile walk up to
/// 10 s), and the step after it was a successor's first chance to adopt: another host's look
/// between the two, [`ADOPT_MS`] after it first read the ended host, handed the pick off as
/// undelivered, spending an election on the session the successor runs, or dropped its nominee
/// and elected a weaker seat. A held record is moved whatever this host reads of the session
/// before its pass, as that reading can lack what holds it: before its first pass a process
/// places no session on the volume, so it holds none, and it reads no draft as standing, and the
/// board's walls are each process's own. Dropped there, another host's look while the pass ran
/// found the round with nobody to elect or wait for and ran the episode out. Returns the sids
/// taken over.
pub fn take_over(lease: &mut Lease, r: &Roster<'_>) -> Vec<String> {
    let Some(entry) = lease.entry_at(r.key) else {
        return Vec::new();
    };
    if entry.episode_ms.is_none() {
        return Vec::new();
    }
    #[cfg(any(test, debug_assertions))]
    if fault::armed(fault::Fault::NoTakeOver) {
        return Vec::new();
    }
    // A record of a session this host runs is moved whatever its seat rule reads before the pass.
    let unread = true;
    #[cfg(any(test, debug_assertions))]
    let unread = unread && !fault::armed(fault::Fault::TakeOverByRule);
    let ended = |h: &HostId| *h != *r.host && !(r.alive)(h);
    let mut took = Vec::new();
    match entry.phase.clone() {
        Phase::Owed { pick, .. } => {
            let moved = adopt(
                entry,
                pick.clone(),
                r.host,
                r.alive,
                &owed_adopters(r.supervised, r.barred),
            );
            if moved != pick {
                if let Phase::Owed { orphan_ms, .. } = &mut entry.phase {
                    *orphan_ms = None;
                }
                took.push(moved.sid);
            }
        }
        Phase::Asked { pick, .. } => {
            let moved = adopt(entry, pick.clone(), r.host, r.alive, &[r.live]);
            if moved != pick {
                took.push(moved.sid);
            }
        }
        Phase::Idle | Phase::Nominating { .. } | Phase::Settled { .. } | Phase::Out { .. } => {}
    }
    let mut now_held: Vec<Held> = Vec::new();
    if let Phase::Nominating { nominees, .. } = &mut entry.phase {
        nominees.retain_mut(|n| {
            if !ended(&n.host) || !r.live.contains(&n.sid) {
                return true;
            }
            if !r.supervised.contains(&n.sid) || r.barred.contains(&n.sid) {
                return false;
            }
            if let Some(why) = holds(r.held, &n.sid) {
                if unread {
                    now_held.push(Held {
                        host: r.host.clone(),
                        sid: n.sid.clone(),
                        why: Some(why),
                        orphan_ms: None,
                    });
                    took.push(n.sid.clone());
                }
                return false;
            }
            n.host = r.host.clone();
            n.orphan_ms = None;
            took.push(n.sid.clone());
            true
        });
        // One nominee a host: the best of what it took over and its own.
        let best = nominees
            .iter()
            .filter(|n| n.host == *r.host)
            .max_by(|a, b| rank((a.held, a.at_break, &a.sid), (b.held, b.at_break, &b.sid)))
            .map(|n| n.sid.clone());
        nominees.retain(|n| n.host != *r.host || best.as_ref() == Some(&n.sid));
    }
    entry.held.retain_mut(|h| {
        if !ended(&h.host) || !r.live.contains(&h.sid) {
            return true;
        }
        let now = holds(r.held, &h.sid);
        if now.is_none() && !unread {
            return false;
        }
        h.host = r.host.clone();
        h.why = now.or(h.why);
        h.orphan_ms = None;
        took.push(h.sid.clone());
        true
    });
    entry.held.extend(now_held);
    let mut seen: Vec<(HostId, String)> = Vec::new();
    entry.held.retain(|h| {
        let key = (h.host.clone(), h.sid.clone());
        let first = !seen.contains(&key);
        seen.push(key);
        first
    });
    let over = entry.held.len().saturating_sub(MAX_NOMINEES);
    entry.held.drain(..over);
    took.sort();
    took.dedup();
    took
}

/// Whether `look` reads `entry`'s open episode clear of what it is held to: free space at or
/// above [`held_to`] plus [`END_MARGIN`], and free inodes, where the volume says them, at or
/// above their line plus [`END_FILES_MARGIN`].
fn ends(entry: &Entry, look: &Look<'_>) -> bool {
    #[cfg(any(test, debug_assertions))]
    if fault::armed(fault::Fault::EndAtOwnFloor) {
        return true;
    }
    let (bytes, files) = (END_MARGIN, END_FILES_MARGIN);
    #[cfg(any(test, debug_assertions))]
    let (bytes, files) = if fault::armed(fault::Fault::EndAtHeldFloor) {
        (0, 0)
    } else {
        (bytes, files)
    };
    look.free >= held_to(entry, look).saturating_add(bytes)
        && look
            .files_free
            .is_none_or(|f| f >= look.files_floor.saturating_add(files))
}

/// Whether `entry`'s parked episode (settled or out since `at_ms`) gives way, at a look that
/// reads the volume short, to a new one: [`COOLDOWN_MS`] has passed since it parked and the
/// volume's last line is outside the cooldown, and aterm's own freeing is done, so the look
/// opens the new one in the same step. A volume that stays short, or near its floor, after an
/// episode ran out or a session freed a little is so asked about again, at most once a
/// cooldown, instead of never.
fn gives_way(entry: &Entry, at_ms: i64, look: &Look<'_>) -> bool {
    #[cfg(any(test, debug_assertions))]
    if fault::armed(fault::Fault::ParkForever) {
        return false;
    }
    look.done.is_some()
        && look.now_ms.saturating_sub(at_ms) >= COOLDOWN_MS
        && entry
            .last_ask_ms
            .is_none_or(|last| !(0..COOLDOWN_MS).contains(&look.now_ms.saturating_sub(last)))
}

fn open(entry: &mut Entry, look: &Look<'_>) -> Said {
    let Some(done) = look.done else {
        return Said::Freeing;
    };
    if let Some(last) = entry.last_ask_ms
        && (0..COOLDOWN_MS).contains(&look.now_ms.saturating_sub(last))
    {
        return Said::Cooldown { last_ms: last };
    }
    entry.episode_ms = Some(look.now_ms);
    entry.done = Some(done);
    entry.floor = Some(Floor {
        host: look.host.clone(),
        bytes: look.floor,
    });
    entry.picks.clear();
    entry.released = 0;
    entry.returned.clear();
    entry.asks = 0;
    entry.asked.clear();
    entry.held.clear();
    note_held(entry, look);
    entry.phase = Phase::Nominating {
        until_ms: look.now_ms.saturating_add(NOMINATE_MS),
        nominees: Vec::new(),
        after: None,
    };
    nominate(entry, look);
    Said::Opened { done }
}

/// The floor `entry`'s open episode is held to on `look`: the one recorded for it while its host
/// lives, or this host's own where that is higher, where none is recorded, or where it is this
/// host's.
fn held_to(entry: &Entry, look: &Look<'_>) -> u64 {
    entry
        .floor
        .as_ref()
        .filter(|f| f.host != *look.host && (look.alive)(&f.host))
        .map_or(0, |f| f.bytes)
        .max(look.floor)
}

/// A look that steps `entry`'s open episode without ending it records what it holds it to
/// ([`held_to`]): this host's floor, unless the floor recorded is another live host's at least
/// as high. So the record, and the line, the owner's note and `harness disk why` that read it,
/// say the floor the episode is held to, whether or not this look read the volume short.
fn hold(entry: &mut Entry, look: &Look<'_>) {
    let kept = entry
        .floor
        .as_ref()
        .is_some_and(|f| f.host != *look.host && f.bytes >= look.floor && (look.alive)(&f.host));
    if !kept {
        entry.floor = Some(Floor {
            host: look.host.clone(),
            bytes: look.floor,
        });
    }
}

/// This host's best seat not yet picked this episode, in place of its last nominee; another
/// host's nominee is kept while it stands ([`stands`]).
fn nominate(entry: &mut Entry, look: &Look<'_>) {
    let best = look
        .seats
        .iter()
        .filter(|s| !entry.picks.iter().any(|p| p.sid == s.sid))
        .max_by(|a, b| rank((a.held, a.at_break, &a.sid), (b.held, b.at_break, &b.sid)))
        .cloned();
    let Phase::Nominating { nominees, .. } = &mut entry.phase else {
        return;
    };
    #[cfg(any(test, debug_assertions))]
    if fault::armed(fault::Fault::DropEndedNominee) {
        nominees.retain(|n| n.host == *look.host || (look.alive)(&n.host));
    }
    nominees
        .retain_mut(|n| n.host != *look.host && stands(&n.host, &n.sid, &mut n.orphan_ms, look));
    if let Some(s) = best {
        nominees.push(Nominee {
            host: look.host.clone(),
            sid: s.sid,
            held: s.held,
            at_break: s.at_break,
            seen_ms: look.now_ms,
            orphan_ms: None,
        });
    }
    if nominees.len() > MAX_NOMINEES {
        nominees.sort_by_key(|n| std::cmp::Reverse(n.seen_ms));
        nominees.truncate(MAX_NOMINEES);
    }
}

/// Whether a record another host `host` made of its session `sid` (a nominee, a held session)
/// still stands at `look`: its host runs, or ended less than [`ADOPT_MS`] after a look first read
/// it ended (`orphan_ms`, stamped here), so a seamless update's successor, which takes the session
/// over under a new [`HostId`] and looks first two minutes after it starts, makes its own record
/// of it before the record is dropped. A session this look's host runs is its own to record, so
/// the other host's record of it stands no more.
fn stands(host: &HostId, sid: &str, orphan_ms: &mut Option<i64>, look: &Look<'_>) -> bool {
    if look.live.iter().any(|s| s == sid) {
        return false;
    }
    if (look.alive)(host) {
        *orphan_ms = None;
        return true;
    }
    let at = *orphan_ms.get_or_insert(look.now_ms);
    look.now_ms.saturating_sub(at) < ADOPT_MS
}

/// Record this look's held sessions ([`Look::held`]) in place of its host's last, and keep
/// another host's while they stand ([`stands`]); at most [`MAX_NOMINEES`], this look's last.
fn note_held(entry: &mut Entry, look: &Look<'_>) {
    entry
        .held
        .retain_mut(|h| h.host != *look.host && stands(&h.host, &h.sid, &mut h.orphan_ms, look));
    entry.held.extend(look.held.iter().map(|(sid, why)| Held {
        host: look.host.clone(),
        sid: sid.clone(),
        why: Some(*why),
        orphan_ms: None,
    }));
    let over = entry.held.len().saturating_sub(MAX_NOMINEES);
    entry.held.drain(..over);
}

/// Record what this look's host's own passes freed ([`Look::reclaimed`]) in the open episode
/// ([`Entry::reclaimed`]), oldest first, keeping [`RECLAIMED_KEEP_MS`] of them, and every one since
/// the line an asked session's answer is judged against however long ago it went, and at most
/// [`MAX_RECLAIMED`]: past it the two oldest are kept as one, ended at the later end, which credits
/// aterm, never a session asked between them, with what both freed.
fn note_reclaimed(entry: &mut Entry, look: &Look<'_>) {
    entry.reclaimed.extend(look.reclaimed.iter().copied());
    entry.reclaimed.sort_by_key(|r| r.ended_ms);
    let mut keep_from = look.now_ms.saturating_sub(RECLAIMED_KEEP_MS);
    if let Phase::Asked { typed_ms, .. } = &entry.phase {
        keep_from = keep_from.min(*typed_ms);
    }
    entry.reclaimed.retain(|r| r.ended_ms >= keep_from);
    while entry.reclaimed.len() > MAX_RECLAIMED {
        let first = entry.reclaimed.remove(0);
        let second = &mut entry.reclaimed[0];
        second.bytes = second.bytes.saturating_add(first.bytes);
    }
}

/// What aterm's own passes freed on `entry`'s volume since a line typed at `typed_ms`, as every
/// host recorded its own ([`Entry::reclaimed`]): a pass that ended at or after the line, all of
/// it, so a pass the line went in the middle of is aterm's whole, never the asked session's.
fn aterm_since(entry: &Entry, typed_ms: i64) -> u64 {
    #[cfg(any(test, debug_assertions))]
    if fault::armed(fault::Fault::CreditAterm) {
        return 0;
    }
    entry
        .reclaimed
        .iter()
        .filter(|r| r.ended_ms >= typed_ms)
        .fold(0u64, |a, r| a.saturating_add(r.bytes))
}

/// The held sessions ([`Entry::held`]) not picked this episode, or gone back to it
/// ([`Entry::returned`]) since they were: what a round with nobody else to elect waits on.
fn held_unpicked(entry: &Entry) -> Vec<String> {
    let mut held: Vec<String> = unpicked_held(entry).map(|h| h.sid.clone()).collect();
    held.sort();
    held.dedup();
    held
}

/// The records of [`held_unpicked`].
fn unpicked_held(entry: &Entry) -> impl Iterator<Item = &Held> {
    entry
        .held
        .iter()
        .filter(|h| !entry.picks.iter().any(|p| p.sid == h.sid))
}

/// What holds the sessions a round waits on ([`held_unpicked`]), each hold once, in order.
fn held_why(entry: &Entry) -> Vec<HeldBy> {
    let mut why: Vec<HeldBy> = unpicked_held(entry)
        .flat_map(|h| h.holds().iter().copied())
        .collect();
    why.sort();
    why.dedup();
    why
}

/// At a look past the round's time: elect the best nominee not yet picked; wait where that is
/// an ended host's that still stands ([`stands`]: its successor may yet nominate it), or where
/// there is none and a session not yet picked is held ([`Said::Waiting`]); otherwise run out.
fn elect(entry: &mut Entry, look: &Look<'_>, after: Option<Miss>) -> Said {
    let now = look.now_ms;
    let (best, seat) = match &entry.phase {
        Phase::Nominating { nominees, .. } => (
            nominees
                .iter()
                .filter(|n| !entry.picks.iter().any(|p| p.sid == n.sid))
                .max_by(|a, b| {
                    rank((a.held, a.at_break, &a.sid), (b.held, b.at_break, &b.sid))
                        .then_with(|| b.host.cmp(&a.host))
                })
                .cloned(),
            nominees
                .iter()
                .find(|n| n.host == *look.host)
                .map(|n| n.sid.clone()),
        ),
        _ => (None, None),
    };
    if let Some(at) = best.as_ref().and_then(|n| n.orphan_ms) {
        // The best nominee's host ended: the round waits for its successor's look, which
        // nominates the session again, or for the nominee to stand no more.
        return Said::Nominating {
            until_ms: at.saturating_add(ADOPT_MS),
            seat,
        };
    }
    let Some(n) = best else {
        let held = held_unpicked(entry);
        #[cfg(any(test, debug_assertions))]
        let held = if fault::armed(fault::Fault::OutWhileHeld) {
            Vec::new()
        } else {
            held
        };
        if !held.is_empty() {
            return Said::Waiting {
                held,
                why: held_why(entry),
            };
        }
        entry.phase = Phase::Out {
            at_ms: now,
            last: after,
        };
        return Said::Out {
            last: after,
            asked: asked_any(entry),
        };
    };
    let pick = Pick {
        host: n.host,
        sid: n.sid,
    };
    entry.picks.push(pick.clone());
    entry.phase = Phase::Owed {
        pick: pick.clone(),
        elected_ms: now,
        until_ms: now.saturating_add(DELIVER_MS),
        orphan_ms: None,
    };
    Said::Elected { pick, held: n.held }
}

/// Whether a line went this episode: one counted, or a session recorded as asked.
fn asked_any(entry: &Entry) -> bool {
    entry.asks > 0 || !entry.asked.is_empty()
}

fn hand_off(entry: &mut Entry, now: i64, from: Pick, why: Miss) -> Said {
    if why == Miss::Released {
        entry.released = entry.released.saturating_add(1);
    }
    if spent(entry) {
        return run_out(entry, now, why);
    }
    next_round(entry, now, why);
    Said::Handoff { from, why }
}

/// Owed pick `from`, which its host holds where it cannot take a line (`why`, [`Look::held`]),
/// goes back to the episode: it leaves [`Entry::picks`], so it costs no election, is nominated
/// again once past, and a round with nobody else to elect waits for it ([`held_unpicked`]). A
/// session goes back so once an episode ([`Entry::returned`]): held again after its next
/// election, it is released ([`Miss::Released`]) and stays a pick, so a session whose turns keep
/// stopping at boxes does not keep the ask from every other session.
fn give_back(entry: &mut Entry, now: i64, from: Pick, why: HeldBy) -> Said {
    #[cfg(any(test, debug_assertions))]
    if fault::armed(fault::Fault::HoldKeepsPick) {
        return hand_off(entry, now, from, Miss::Released);
    }
    if entry.returned.contains(&from.sid) {
        return hand_off(entry, now, from, Miss::Released);
    }
    if let Some(at) = entry.picks.iter().rposition(|p| *p == from) {
        entry.picks.remove(at);
    }
    entry.returned.push(from.sid.clone());
    if spent(entry) {
        return run_out(entry, now, Miss::Released);
    }
    next_round(entry, now, Miss::Released);
    Said::Returned { from, why }
}

/// Whether the episode's lines ([`MAX_ASKS`]) or elections ([`MAX_PICKS`], those released not
/// counted) are spent.
fn spent(entry: &Entry) -> bool {
    let picks = u32::try_from(entry.picks.len())
        .unwrap_or(u32::MAX)
        .saturating_sub(entry.released);
    entry.asks >= MAX_ASKS || picks >= MAX_PICKS
}

/// The episode runs out ([`Said::Out`]) after a pick missed for `why`.
fn run_out(entry: &mut Entry, now: i64, why: Miss) -> Said {
    entry.phase = Phase::Out {
        at_ms: now,
        last: Some(why),
    };
    Said::Out {
        last: Some(why),
        asked: asked_any(entry),
    }
}

/// A new round gathers nominees after a pick missed for `why`.
fn next_round(entry: &mut Entry, now: i64, why: Miss) {
    entry.phase = Phase::Nominating {
        until_ms: now.saturating_add(NOMINATE_MS),
        nominees: Vec::new(),
        after: Some(why),
    };
}

/// What a host's [`claim`] found for its session's line.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Claim {
    /// The lease names this session and its time stands: type the line now. The lease holds the
    /// pick for [`TYPE_GRACE_MS`] while it is typed.
    Yours,
    /// The lease names no line for it (handed on, ended, or another session's): type nothing.
    Gone,
}

/// Before host `host` types session `sid`'s line on volume `key`: whether the lease still names
/// it ([`Claim::Yours`], its hold extended so no host hands it off mid-typing).
#[must_use]
pub fn claim(lease: &mut Lease, key: &str, host: &HostId, sid: &str, now_ms: i64) -> Claim {
    #[cfg(any(test, debug_assertions))]
    if fault::armed(fault::Fault::ClaimBlind) {
        return Claim::Yours;
    }
    let Some(entry) = lease.entry_at(key) else {
        return Claim::Gone;
    };
    match &mut entry.phase {
        Phase::Owed {
            pick,
            elected_ms,
            until_ms,
            ..
        } if pick.host == *host && pick.sid == sid && now_ms < *until_ms => {
            let cap = elected_ms
                .saturating_add(DELIVER_MS)
                .saturating_add(TYPE_GRACE_MS);
            *until_ms = (*until_ms)
                .max(now_ms.saturating_add(TYPE_GRACE_MS))
                .min(cap);
            Claim::Yours
        }
        _ => Claim::Gone,
    }
}

/// Host `host` typed session `sid`'s line on volume `key` at `now_ms`, `free` bytes free then:
/// the volume's cooldown starts, and the lease's pick (when it is this one) is asked. A line the
/// lease moved on from owing, a pick of the open episode yet, is still counted as one of its
/// lines ([`counted_late`]). `false`: it is no pick of the open episode, so the line is counted
/// for the cooldown alone.
pub fn typed(
    lease: &mut Lease,
    key: &str,
    host: &HostId,
    sid: &str,
    now_ms: i64,
    free: u64,
) -> bool {
    let entry = lease.entry_mut(key);
    entry.last_ask_ms = Some(entry.last_ask_ms.map_or(now_ms, |last| last.max(now_ms)));
    match &entry.phase {
        Phase::Owed { pick, .. } if pick.host == *host && pick.sid == sid => {
            let pick = pick.clone();
            entry.asks = entry.asks.saturating_add(1);
            entry.asked.push(pick.sid.clone());
            entry.phase = Phase::Asked {
                pick,
                typed_ms: now_ms,
                until_ms: now_ms.saturating_add(ANSWER_MS),
                free_at: free,
                best_free: free,
            };
            true
        }
        _ => counted_late(entry, host, sid, now_ms, free),
    }
}

/// Whether `entry`'s open episode holds host `host`'s session `sid` as a pick that took no line,
/// for a line typed at `at_ms`: one of the episode's own picks, elected before the line went.
fn uncounted(entry: &Entry, host: &HostId, sid: &str, at_ms: i64) -> bool {
    entry.episode_ms.is_some_and(|opened| at_ms >= opened)
        && entry.picks.iter().any(|p| p.host == *host && p.sid == sid)
        && !entry.asked.iter().any(|s| s == sid)
}

/// A line host `host` typed session `sid` at `at_ms` (`free` bytes free then) after the lease
/// moved on from owing it: its record could not be written (a full disk), and another host
/// handed its pick off before this host's next look recorded it ([`typed_unrecorded`]). A pick
/// of the open episode that took no line is counted as asked, so [`MAX_ASKS`] bounds the lines
/// that went, not the lines recorded in time. Where no pick elected after it took a line, the
/// episode waits on its answer again ([`Phase::Asked`]) when it gathers nominees or ran out, or
/// when it owes a later pick its line and this count reached [`MAX_ASKS`]: that later line would
/// be one past the bound, so the later pick is withdrawn untyped. A release its own host made of
/// it, the last pick, before recording is undone. `false`: it is no such pick, so the line
/// counts for the cooldown alone.
fn counted_late(entry: &mut Entry, host: &HostId, sid: &str, at_ms: i64, free: u64) -> bool {
    if !uncounted(entry, host, sid, at_ms) {
        return false;
    }
    #[cfg(any(test, debug_assertions))]
    if fault::armed(fault::Fault::CountOwedOnly) {
        return false;
    }
    let at = entry
        .picks
        .iter()
        .position(|p| p.host == *host && p.sid == sid);
    let later_line = at.is_some_and(|i| {
        entry.picks[i + 1..]
            .iter()
            .any(|p| entry.asked.contains(&p.sid))
    });
    entry.asks = entry.asks.saturating_add(1);
    entry.asked.push(sid.to_owned());
    let last = at.is_some_and(|i| i + 1 == entry.picks.len());
    let spent = entry.asks >= MAX_ASKS;
    #[cfg(any(test, debug_assertions))]
    let spent = spent && !fault::armed(fault::Fault::OwedPastTwo);
    let (back, missed) = match &entry.phase {
        Phase::Nominating { after, .. } => (true, *after),
        Phase::Out { last: why, .. } => (true, *why),
        Phase::Owed { .. } => (spent, None),
        _ => (false, None),
    };
    if later_line || !back {
        return true;
    }
    if last && missed == Some(Miss::Released) {
        entry.released = entry.released.saturating_sub(1);
    }
    entry.phase = Phase::Asked {
        pick: Pick {
            host: host.clone(),
            sid: sid.to_owned(),
        },
        typed_ms: at_ms,
        until_ms: at_ms.saturating_add(ANSWER_MS),
        free_at: free,
        best_free: free,
    };
    true
}

/// [`typed`], for the line host `host` last typed on volume `key` (session `sid`, at `at_ms`,
/// `free` bytes free then), where the lease may lack its record because writing it failed (a
/// full disk): recorded when the volume's last line is older, or when the open episode still
/// holds the session as a pick that took no line ([`counted_late`]), though a later line went. A
/// line typed before the open episode opened is one of an earlier episode's, and starts the
/// cooldown alone. `true`: it was recorded.
pub fn typed_unrecorded(
    lease: &mut Lease,
    key: &str,
    host: &HostId,
    sid: &str,
    at_ms: i64,
    free: u64,
) -> bool {
    let Some(entry) = lease.entry_at(key) else {
        typed(lease, key, host, sid, at_ms, free);
        return true;
    };
    if entry.last_ask_ms.is_some_and(|last| last >= at_ms) && !uncounted(entry, host, sid, at_ms) {
        return false;
    }
    if entry.episode_ms.is_some_and(|opened| at_ms < opened) {
        entry.last_ask_ms = Some(entry.last_ask_ms.map_or(at_ms, |last| last.max(at_ms)));
        return true;
    }
    typed(lease, key, host, sid, at_ms, free);
    true
}

/// Whether a host's look that reads volume `key` clear of its floor leaves `lease` as it is, its
/// lock untaken: no episode is open there, and the last line this host typed on the volume (at
/// `typed_ms`), if any, needs no record ([`typed_unrecorded`] would not record it). A line whose
/// record was lost (a full disk) is so recorded at the host's next look even when another host
/// has ended its episode since, so its cooldown starts and no host opens a new episode, and asks
/// another session, within it.
#[must_use]
pub fn rests(lease: &Lease, key: &str, typed_ms: Option<i64>) -> bool {
    let entry = lease.entry(key);
    let open = entry.is_some_and(|e| e.episode_ms.is_some());
    let unrecorded = typed_ms.is_some_and(|at| {
        entry
            .and_then(|e| e.last_ask_ms)
            .is_none_or(|last| last < at)
    });
    #[cfg(any(test, debug_assertions))]
    let unrecorded = unrecorded && !fault::armed(fault::Fault::RestUnrecorded);
    !open && !unrecorded
}

/// Host `host` found at its turn break that session `sid` may not be asked: the lease hands the
/// ask on ([`Miss::Released`]) when it still names it.
pub fn release(
    lease: &mut Lease,
    key: &str,
    host: &HostId,
    sid: &str,
    now_ms: i64,
) -> Option<Said> {
    let entry = lease.entry_at(key)?;
    match &entry.phase {
        Phase::Owed { pick, .. } if pick.host == *host && pick.sid == sid => {
            let pick = pick.clone();
            Some(hand_off(entry, now_ms, pick, Miss::Released))
        }
        _ => None,
    }
}

/// What a host's turn-break step found for its session's line ([`at_break`]).
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Break {
    /// It may not be asked: the lease handed the ask on ([`release`]).
    Released(Said),
    /// Type the line now ([`Claim::Yours`]).
    Yours,
    /// The lease names no line for it: type nothing.
    Gone,
}

/// At session `sid`'s turn break on host `host`: when the seat rule bars it now (`barred`: its
/// task is done or exited, or its worker finds it unfit), its line is released ([`release`]),
/// so a session that may not be nominated is never typed one; otherwise it is claimed
/// ([`claim`]).
#[must_use]
pub fn at_break(
    lease: &mut Lease,
    key: &str,
    host: &HostId,
    sid: &str,
    now_ms: i64,
    barred: bool,
) -> Break {
    #[cfg(any(test, debug_assertions))]
    let barred = barred && !fault::armed(fault::Fault::TypeBarred);
    if barred {
        return release(lease, key, host, sid, now_ms).map_or(Break::Gone, Break::Released);
    }
    match claim(lease, key, host, sid, now_ms) {
        Claim::Yours => Break::Yours,
        Claim::Gone => Break::Gone,
    }
}

/// Whether `entry`'s round waits at `now_ms` on sessions held where they cannot take a line now
/// ([`Said::Waiting`], as [`elect`] decides it): its time is over, it holds no nominee not yet
/// picked, and a session not yet picked, or gone back to the episode, is held
/// ([`held_unpicked`]).
fn waits(entry: &Entry, now_ms: i64) -> bool {
    match &entry.phase {
        Phase::Nominating {
            until_ms, nominees, ..
        } => {
            now_ms >= *until_ms
                && !nominees
                    .iter()
                    .any(|n| !entry.picks.iter().any(|p| p.sid == n.sid))
                && !held_unpicked(entry).is_empty()
        }
        _ => false,
    }
}

/// What the owner's disk line says of volume `entry`'s ask at `now_ms`: who was asked and what
/// came of it. A round that waits on held sessions ([`Said::Waiting`]) says so, after a line went
/// too, and what holds them and ends the wait ([`HeldBy`]): such a round can last days (a weekly
/// usage limit, a hold or a draft nobody comes back to), and nobody else is asked meanwhile.
/// `None` before any line of the episode, unless it ran out with nobody to ask or waits.
#[must_use]
pub fn owner_line(entry: &Entry, now_ms: i64) -> Option<String> {
    let waiting = || {
        let why = held_why(entry);
        format!(
            "{}: aterm asks one once {} (`aterm harness disk why`)",
            held_at(&why, true),
            held_past(&why)
        )
    };
    let named = |sids: &[String]| {
        sids.iter()
            .map(|s| format!("@{s}"))
            .collect::<Vec<_>>()
            .join(" and ")
    };
    let asked = named(&entry.asked);
    let they = if entry.asked.len() == 1 { "it" } else { "they" };
    match &entry.phase {
        Phase::Out { .. } if entry.asked.is_empty() => Some(
            "No agent session could be asked to free space (none supervised on this volume, or \
             none took its line): only you can free it"
                .to_owned(),
        ),
        Phase::Out { .. } => Some(format!(
            "aterm asked {asked} to free space and {they} freed under {}: no more agents are \
             asked",
            human_bytes(FREED_MIN)
        )),
        Phase::Settled { sid, freed, .. } => Some(format!(
            "@{sid} freed {} after aterm asked it; nobody else is asked",
            human_bytes(*freed)
        )),
        Phase::Asked { pick, typed_ms, .. } => Some(format!(
            "aterm asked @{} at {} to free space (one agent; `aterm harness disk why`)",
            pick.sid,
            rfc3339_utc(typed_ms / 1000)
        )),
        Phase::Nominating { .. } if waits(entry, now_ms) && entry.asked.is_empty() => Some(
            format!("Every agent session that could free space is {}", waiting()),
        ),
        Phase::Nominating { .. } if waits(entry, now_ms) => Some(format!(
            "aterm asked {asked}; {they} freed under {}, and every other agent session that \
             could free space is {}",
            human_bytes(FREED_MIN),
            waiting()
        )),
        Phase::Nominating { .. } | Phase::Owed { .. } if !entry.asked.is_empty() => Some(format!(
            "aterm asked {asked}; {they} freed under {}, so one other session is asked",
            human_bytes(FREED_MIN)
        )),
        _ => None,
    }
}

/// The guard on [`LOCK_FILE`]: its `Drop` releases by `LOCK_UN`.
pub struct LeaseLock(std::fs::File);

impl Drop for LeaseLock {
    fn drop(&mut self) {
        let _ = self.0.unlock();
    }
}

/// Take `dir`'s [`LOCK_FILE`], pausing through `ladder` while it is held: `Ok(None)` when it
/// stayed held.
///
/// # Errors
///
/// The folder could not be made, or the lock file opened or locked.
pub fn lock_within(dir: &Path, ladder: &[Duration]) -> io::Result<Option<LeaseLock>> {
    let mut builder = std::fs::DirBuilder::new();
    builder.recursive(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::DirBuilderExt as _;
        builder.mode(0o700);
    }
    builder.create(dir)?;
    let mut opts = std::fs::OpenOptions::new();
    opts.read(true).write(true).create(true).truncate(false);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt as _;
        opts.mode(0o600)
            .custom_flags(libc::O_NOFOLLOW | libc::O_CLOEXEC);
    }
    #[cfg(windows)]
    {
        use std::os::windows::fs::OpenOptionsExt as _;
        opts.custom_flags(super::win::FILE_FLAG_OPEN_REPARSE_POINT);
    }
    let file = opts.open(dir.join(LOCK_FILE))?;
    let mut pauses = ladder.iter();
    loop {
        #[expect(
            clippy::disallowed_methods,
            reason = "the lease lock's own take: LeaseLock's Drop releases by LOCK_UN"
        )]
        let taken = file.try_lock();
        match taken {
            Ok(()) => return Ok(Some(LeaseLock(file))),
            Err(std::fs::TryLockError::WouldBlock) => match pauses.next() {
                Some(pause) => std::thread::sleep(*pause),
                None => return Ok(None),
            },
            Err(std::fs::TryLockError::Error(e)) => return Err(e),
        }
    }
}

/// `dir`'s lease as its file holds it, read without the lock (a reader's view); an empty lease
/// where there is none or it cannot be read.
#[must_use]
pub fn read(dir: &Path) -> Lease {
    super::super::accounts::read_bounded(&dir.join(LEASE_FILE), LEASE_MAX_BYTES)
        .ok()
        .flatten()
        .map(|t| Lease::decode(&t))
        .unwrap_or_default()
}

/// Edit `dir`'s lease under its lock ([`lock_within`] over `ladder`): read, `edit`, and write it
/// back (by temp file and rename) when the edit changed it. `Ok(None)`: the lock stayed held,
/// and nothing was read or written.
///
/// # Errors
///
/// The lock could not be taken, or the changed lease could not be written (a full disk): the
/// edit is then lost, and the next look steps again.
pub fn transact<R>(
    dir: &Path,
    ladder: &[Duration],
    edit: impl FnOnce(&mut Lease) -> R,
) -> io::Result<Option<R>> {
    let Some(_held) = lock_within(dir, ladder)? else {
        return Ok(None);
    };
    let mut lease = read(dir);
    let before = lease.clone();
    let out = edit(&mut lease);
    if lease != before {
        super::super::upgrade_models::write_atomic_ordered(&dir.join(LEASE_FILE), &lease.encode())?;
    }
    Ok(Some(out))
}

/// What a line says ([`line`]).
#[derive(Debug, Clone, Copy)]
pub struct Line<'a> {
    /// The volume's mount point.
    pub mount: &'a Path,
    pub total: u64,
    pub free: u64,
    /// The line it is held to: the episode's floor ([`Entry::floor`]), or with `emergency` what
    /// writes need.
    pub floor: u64,
    pub emergency: bool,
    pub done: Done,
    /// The largest holders the pass left ([`super::Report::hogs`], largest first).
    pub holders: &'a [Hog],
    /// The asked session's checkout: a holder that serves it is said to be this tab's.
    pub checkout: Option<&'a Path>,
    /// `$HOME`, for `~/…`.
    pub home: Option<&'a Path>,
}

/// The harness's mark and whose fact the line is.
const HEAD: &str = concat!(harness_mark!(), " aterm: ");

/// What is safe to remove, a constant.
const SAFE: &str = "Free what nobody uses: builds you no longer need (`targo clean` in their \
                    checkout), stale build output; never source, a checkout, .git or a build \
                    directory another session is using.";

/// How to confirm, a constant.
const CONFIRM: &str = "Check with `aterm ctl @self status` (disk=, disk_free_mb=) and `aterm \
                       harness disk`, then carry on with your task. Only this tab was asked.";

/// The longest a holder's path is said, in characters.
const HOLDER_PATH_CAP: usize = 56;

/// The `disk-free@v1` line: the mark, the volume and how full it is (below its floor, or within
/// [`END_MARGIN`] above it), why aterm is done, the largest holders the pass left, what is safe
/// to remove and how to confirm, then `(rule disk-free@v1)`, in at most [`LINE_CAP`]
/// characters. A holder's path is said only in plain characters (letters, digits, `/._-~+@:,=`
/// and spaces) and clipped; any other is said as "a build directory", so a folder's name
/// carries nothing planted into an agent's prompt. A line too long for both holders names the
/// largest alone, and one too long for that names it with its path clipped to the room left
/// ([`clip_within`]), so a line whose pass left a holder never says it names none.
#[must_use]
pub fn line(l: &Line<'_>) -> String {
    let used = l.total.saturating_sub(l.free);
    let pct = if l.total == 0 {
        100
    } else {
        u64::try_from(u128::from(used) * 100 / u128::from(l.total)).unwrap_or(100)
    };
    let why = match l.done {
        Done::Spent => "aterm has removed every build cache it may",
        Done::CleanupOff => "aterm's own cleanup is off (`[disk] auto_free_gib`)",
        Done::Inodes => "aterm's cleanup frees bytes, not inodes",
    };
    let below = match (l.done, l.emergency) {
        (Done::Inodes, _) => "and out of inodes".to_owned(),
        (_, true) => format!("below the {} writes need", human_bytes(l.floor)),
        (_, false) if l.free < l.floor => format!("below its {} floor", human_bytes(l.floor)),
        // The episode is held open until free space clears its floor by the margin.
        (_, false) => format!(
            "within {} of its {} floor",
            human_bytes(END_MARGIN),
            human_bytes(l.floor)
        ),
    };
    let fact = format!(
        "the volume this tab works on ({}) is {pct}% full, {} free, {below}, and {why}.",
        plain(&l.mount.display().to_string()).unwrap_or_else(|| "its volume".to_owned()),
        human_bytes(l.free),
    );
    let said =
        |named: &str| format!("{HEAD}{fact}{named} {SAFE} {CONFIRM} (rule {RULE_DISK_FREE})");
    let largest = |named: &[String]| format!(" Largest left: {}.", named.join("; "));
    let holders: Vec<String> = l
        .holders
        .iter()
        .take(2)
        .map(|h| holder(h, l, &shown_path(holder_path(h), l.home)))
        .collect();
    for keep in (1..=holders.len()).rev() {
        let text = said(&largest(&holders[..keep]));
        if text.chars().count() <= LINE_CAP {
            return text;
        }
    }
    // Not even the largest fits as it is usually said: it is said with its path clipped to the
    // room the line leaves it, so a line with a holder to name never says it has none, and the
    // asked tab's own keeps its mark.
    if let Some(first) = l.holders.first() {
        let bare = said(&largest(&[holder(first, l, "")])).chars().count();
        if let Some(shown) =
            shown_path_within(holder_path(first), l.home, LINE_CAP.saturating_sub(bare))
        {
            return said(&largest(&[holder(first, l, &shown)]));
        }
        return said(" `aterm harness disk` lists the largest holders left.");
    }
    said(" aterm's report names no holder it can size: `aterm harness disk` lists them.")
}

/// The path holder `h` is said by: the checkout it builds for, or its own directory.
fn holder_path(h: &Hog) -> &Path {
    h.serves.as_deref().unwrap_or(&h.path)
}

/// Holder `h` as the line says it, its path ([`holder_path`]) said as `shown`:
/// `<checkout>'s builds <size>`, or `<directory> <size>`, marked `(this tab's)` when it builds
/// for the asked session's checkout.
fn holder(h: &Hog, l: &Line<'_>, shown: &str) -> String {
    let mine = l
        .checkout
        .zip(h.serves.as_deref())
        .is_some_and(|(c, s)| c == s);
    let place = match h.serves.as_deref() {
        Some(s) if s != h.path => format!("{shown}'s builds"),
        _ => shown.to_owned(),
    };
    format!(
        "{place} {}{}",
        human_bytes(h.bytes),
        if mine { " (this tab's)" } else { "" }
    )
}

/// What a path any other character than the plain ones would spell is said as ([`plain`]).
const UNSAID_PATH: &str = "a build directory";

fn shown_path(p: &Path, home: Option<&Path>) -> String {
    said_path(p, home).map_or_else(|| UNSAID_PATH.to_owned(), |t| clip(&t))
}

/// `p` home-relative (`~/…`) and whole, when every character of it is a plain one ([`plain`]).
fn said_path(p: &Path, home: Option<&Path>) -> Option<String> {
    let text = match home.and_then(|h| p.strip_prefix(h).ok()) {
        Some(rel) if !rel.as_os_str().is_empty() => format!("~/{}", rel.display()),
        _ => p.display().to_string(),
    };
    plain(&text)
}

/// [`shown_path`] in at most `room` characters ([`clip_within`]). `None`: not even its clipped
/// form fits.
fn shown_path_within(p: &Path, home: Option<&Path>, room: usize) -> Option<String> {
    match said_path(p, home) {
        Some(text) => clip_within(&text, room),
        None => (UNSAID_PATH.chars().count() <= room).then(|| UNSAID_PATH.to_owned()),
    }
}

/// The fewest characters of a path's last component [`clip_within`] keeps.
const LAST_KEPT_MIN: usize = 3;

/// `text`, a path as [`said_path`] says it, in at most `room` characters: clipped in its middle,
/// so it keeps its leading `~/` or `/`, as many whole components after that as fit, and its last
/// component (`~/aterm/…/worktree-name`). A last component too long for that is clipped in
/// its own middle (`~/…/worktr…e-name`). `None`: not even [`LAST_KEPT_MIN`] characters of it fit.
fn clip_within(text: &str, room: usize) -> Option<String> {
    let chars: Vec<char> = text.chars().collect();
    if chars.len() <= room {
        return Some(text.to_owned());
    }
    let lead = if text.starts_with("~/") {
        2
    } else {
        usize::from(text.starts_with('/'))
    };
    let last_at = chars
        .iter()
        .rposition(|&c| c == '/')
        .map_or(0, |at| at + 1)
        .max(lead);
    let last = &chars[last_at..];
    // `…/` stands for what lies between the lead and the last component.
    let between = if last_at > lead { "…/" } else { "" };
    let marks = between.chars().count();
    if lead + marks + last.len() <= room {
        // As much of what comes first as fits, cut where a component ends.
        let fits = room - marks - last.len();
        let head = chars[..fits]
            .iter()
            .rposition(|&c| c == '/')
            .map_or(lead, |at| at + 1)
            .max(lead);
        return Some(
            chars[..head]
                .iter()
                .copied()
                .chain(between.chars())
                .chain(last.iter().copied())
                .collect(),
        );
    }
    let kept = room.checked_sub(lead + marks + 1)?;
    if kept < LAST_KEPT_MIN {
        return None;
    }
    let tail = kept / 2;
    let head = kept - tail;
    Some(
        chars[..lead]
            .iter()
            .copied()
            .chain(between.chars())
            .chain(last[..head].iter().copied())
            .chain(std::iter::once('…'))
            .chain(last[last.len() - tail..].iter().copied())
            .collect(),
    )
}

/// `text` when every character is a plain one (letters, digits, `/._-~+@:,=` and spaces).
fn plain(text: &str) -> Option<String> {
    (!text.is_empty()
        && text
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || "/._-~+@:,= ".contains(c)))
    .then(|| text.to_owned())
}

fn clip(text: &str) -> String {
    let chars: Vec<char> = text.chars().collect();
    if chars.len() <= HOLDER_PATH_CAP {
        return text.to_owned();
    }
    let head = HOLDER_PATH_CAP / 3;
    let tail = HOLDER_PATH_CAP - head - 1;
    chars[..head]
        .iter()
        .chain(std::iter::once(&'…'))
        .chain(&chars[chars.len() - tail..])
        .collect()
}

/// One `free-ask` row of the disk ledger: a step's decision on volume `key` (its word, the
/// session it names, its sentence), at `now_s`, read at `level` with `free` bytes free, by host
/// `host`.
#[must_use]
pub fn row(now_s: i64, key: &str, said: &Said, level: Level, free: u64, host: &HostId) -> String {
    let mut o = Map::new();
    o.insert("ts".to_owned(), Value::from(rfc3339_utc(now_s)));
    o.insert("sid".to_owned(), Value::from(said.sid().to_owned()));
    o.insert("kind".to_owned(), Value::from(ROW_KIND.to_owned()));
    o.insert("rule".to_owned(), Value::from(RULE_DISK_FREE.to_owned()));
    o.insert("said".to_owned(), Value::from(said.word().to_owned()));
    o.insert("volume".to_owned(), Value::from(key.to_owned()));
    o.insert("level".to_owned(), Value::from(level.as_str().to_owned()));
    o.insert("free_bytes".to_owned(), Value::from(free));
    o.insert("host_pid".to_owned(), Value::from(host.pid));
    match said {
        Said::Opened { done } => {
            o.insert("why".to_owned(), Value::from(done.word().to_owned()));
        }
        Said::Elected { pick, held } => {
            o.insert("held_bytes".to_owned(), Value::from(*held));
            o.insert("pick_pid".to_owned(), Value::from(pick.host.pid));
        }
        Said::Freed { freed, .. } => {
            o.insert("freed_bytes".to_owned(), Value::from(*freed));
        }
        Said::Handoff { why, .. } => {
            o.insert("why".to_owned(), Value::from(why.word().to_owned()));
        }
        Said::Out { last, asked } => {
            o.insert(
                "why".to_owned(),
                Value::from(last.map_or("no-agent", Miss::word).to_owned()),
            );
            o.insert("asked".to_owned(), Value::from(*asked));
        }
        Said::Waiting { held, why } => {
            o.insert("held".to_owned(), Value::from(held.join(",")));
            o.insert(
                "why".to_owned(),
                Value::from(why.iter().map(|h| h.word()).collect::<Vec<_>>().join(",")),
            );
        }
        Said::Returned { why, .. } => {
            o.insert("why".to_owned(), Value::from(why.word().to_owned()));
        }
        Said::Ended { asks } => {
            o.insert("asks".to_owned(), Value::from(*asks));
        }
        _ => {}
    }
    o.insert(
        "text".to_owned(),
        Value::from(truncate_bytes(&said.describe(), ROW_TEXT_CAP).to_owned()),
    );
    aterm_json::to_string(&Value::Object(o))
        .unwrap_or_else(|_| "{\"kind\":\"free-ask\",\"reason\":\"unserializable\"}".to_owned())
}

/// One `free-ask` row of a host's turn-break step for session `sid` on volume `key`: `step` is
/// `typed`, `wait:<why>` (the fence refused; it is tried again at its next turn break),
/// `released:<why>` (it may not be asked) or `gone` (the lease names it no more).
#[must_use]
pub fn step_row(now_s: i64, sid: &str, key: &str, step: &str, host: &HostId) -> String {
    let mut o = Map::new();
    o.insert("ts".to_owned(), Value::from(rfc3339_utc(now_s)));
    o.insert("sid".to_owned(), Value::from(sid.to_owned()));
    o.insert("kind".to_owned(), Value::from(ROW_KIND.to_owned()));
    o.insert("rule".to_owned(), Value::from(RULE_DISK_FREE.to_owned()));
    o.insert("said".to_owned(), Value::from("step".to_owned()));
    o.insert("volume".to_owned(), Value::from(key.to_owned()));
    o.insert("host_pid".to_owned(), Value::from(host.pid));
    o.insert(
        "step".to_owned(),
        Value::from(truncate_bytes(step, ROW_TEXT_CAP).to_owned()),
    );
    aterm_json::to_string(&Value::Object(o))
        .unwrap_or_else(|_| "{\"kind\":\"free-ask\",\"reason\":\"unserializable\"}".to_owned())
}

/// The ledger rows that are this rule's (`kind: free-ask`), as one line each a person reads:
/// `<ts> <said> [@<sid>] <volume> — <text>` (a step: `<ts> step @<sid> <step>`).
#[must_use]
pub fn row_line(row: &str) -> Option<String> {
    let v: Value = aterm_json::from_str(row).ok()?;
    if v.get("kind").and_then(Value::as_str) != Some(ROW_KIND) {
        return None;
    }
    let s = |k: &str| v.get(k).and_then(Value::as_str).unwrap_or("");
    let sid = match s("sid") {
        "" => String::new(),
        sid => format!(" @{sid}"),
    };
    Some(if s("said") == "step" {
        format!("{} step{sid} {} ({})", s("ts"), s("step"), s("volume"))
    } else {
        format!(
            "{} {}{sid} {} — {}",
            s("ts"),
            s("said"),
            s("volume"),
            s("text")
        )
    })
}

/// What `aterm harness disk why` prints: one head line per volume the lease holds (the floor
/// its episode is held to, its phase, what it waits on, its picks, lines and cooldown at
/// `now_ms`), then `rows` (this rule's ledger rows, oldest first) as [`row_line`] says them.
#[must_use]
pub fn why_lines(lease: &Lease, rows: &[String], now_ms: i64) -> Vec<String> {
    let at = |ms: i64| rfc3339_utc(ms / 1000);
    let mut out = Vec::new();
    if lease.volumes.is_empty() {
        out.push(format!(
            "{RULE_DISK_FREE}: no volume has had an episode (the lease is empty)"
        ));
    }
    for e in &lease.volumes {
        let open = e.episode_ms.map_or_else(
            || "no episode".to_owned(),
            |ms| {
                format!(
                    "episode since {}{}{}",
                    at(ms),
                    e.done
                        .map(|d| format!(" ({})", d.word()))
                        .unwrap_or_default(),
                    e.floor
                        .as_ref()
                        .map(|f| format!(
                            ", held to {} (host pid {}) until free space is {} past it",
                            human_bytes(f.bytes),
                            f.host.pid,
                            human_bytes(END_MARGIN)
                        ))
                        .unwrap_or_default()
                )
            },
        );
        let state = match &e.phase {
            Phase::Idle => "idle".to_owned(),
            Phase::Nominating {
                until_ms,
                nominees,
                after,
            } => format!(
                "nominating until {}{}: {}",
                at(*until_ms),
                after
                    .map(|m| format!(" (after {})", m.word()))
                    .unwrap_or_default(),
                if nominees.is_empty() {
                    "no nominee yet".to_owned()
                } else {
                    nominees
                        .iter()
                        .map(|n| {
                            format!(
                                "@{} held={} at_break={} (host pid {}{})",
                                n.sid,
                                human_bytes(n.held),
                                u8::from(n.at_break),
                                n.host.pid,
                                n.orphan_ms
                                    .map(|ms| format!(
                                        ", read ended at {}: stands until {} for its successor",
                                        at(ms),
                                        at(ms.saturating_add(ADOPT_MS))
                                    ))
                                    .unwrap_or_default()
                            )
                        })
                        .collect::<Vec<_>>()
                        .join(", ")
                }
            ),
            Phase::Owed {
                pick,
                until_ms,
                orphan_ms,
                ..
            } => format!(
                "owed @{} (host pid {}): its line goes at its next turn break, by {}{}",
                pick.sid,
                pick.host.pid,
                at(*until_ms),
                orphan_ms
                    .map(|ms| format!(
                        "; its host was read ended at {}: handed on from {} unless a host that \
                         supervises it adopts it",
                        at(ms),
                        at(ms.saturating_add(ADOPT_MS))
                    ))
                    .unwrap_or_default()
            ),
            Phase::Asked {
                pick,
                typed_ms,
                until_ms,
                free_at,
                best_free,
            } => format!(
                "asked @{} at {}: free {} then, {} at best since, {} of it freed by aterm's own \
                 passes, which is not the session's; it needs +{} more by {}",
                pick.sid,
                at(*typed_ms),
                human_bytes(*free_at),
                human_bytes(*best_free),
                human_bytes(aterm_since(e, *typed_ms)),
                human_bytes(FREED_MIN),
                at(*until_ms)
            ),
            Phase::Settled { at_ms, sid, freed } => format!(
                "settled at {}: @{sid} freed {}; a look that reads the volume short from {} \
                 opens a new episode",
                at(*at_ms),
                human_bytes(*freed),
                at(at_ms.saturating_add(COOLDOWN_MS))
            ),
            Phase::Out { at_ms, last } => format!(
                "out at {} ({}): no more agents are asked this episode; a look that reads the \
                 volume short from {} opens a new one",
                at(*at_ms),
                last.map_or("no-agent", Miss::word),
                at(at_ms.saturating_add(COOLDOWN_MS))
            ),
        };
        let cool = match e.last_ask_ms {
            Some(last) if (0..COOLDOWN_MS).contains(&now_ms.saturating_sub(last)) => format!(
                "last line {}, no new episode before {}",
                at(last),
                at(last.saturating_add(COOLDOWN_MS))
            ),
            Some(last) => format!("last line {}, cooldown over", at(last)),
            None => "no line yet".to_owned(),
        };
        let picks = if e.picks.is_empty() {
            String::new()
        } else {
            format!(
                "; picked {}{}",
                e.picks
                    .iter()
                    .map(|p| format!("@{}", p.sid))
                    .collect::<Vec<_>>()
                    .join(", "),
                if e.released > 0 {
                    format!(" ({} released, as they may not be asked)", e.released)
                } else {
                    String::new()
                }
            )
        };
        let returned = if e.returned.is_empty() {
            String::new()
        } else {
            format!(
                "; back in the episode, held where they could not take their line: {}",
                e.returned
                    .iter()
                    .map(|s| format!("@{s}"))
                    .collect::<Vec<_>>()
                    .join(", ")
            )
        };
        out.push(format!("{RULE_DISK_FREE} volume={} {open}: {state}", e.key));
        out.push(format!(
            "  lines {}/{MAX_ASKS}{picks}{returned}; {cool}",
            e.asks
        ));
        if e.episode_ms.is_some() && !e.held.is_empty() {
            out.push(format!(
                "  held where they cannot take a line now, so not nominated until that passes: \
                 {}",
                e.held
                    .iter()
                    .map(|h| format!(
                        "@{} (host pid {}) {}",
                        h.sid,
                        h.host.pid,
                        held_at(h.holds(), false)
                    ))
                    .collect::<Vec<_>>()
                    .join(", ")
            ));
        }
    }
    if rows.is_empty() {
        out.push("no free-ask rows in the disk ledger yet".to_owned());
    } else {
        out.push("recent decisions (the disk ledger's free-ask rows):".to_owned());
        out.extend(
            rows.iter()
                .filter_map(|r| row_line(r))
                .map(|l| format!("  {l}")),
        );
    }
    out
}

/// The planted defects of the conformance test's negative controls
/// (`tests/conformance_disk_free_ask.rs`): proof and test builds only.
#[cfg(any(test, debug_assertions))]
pub mod fault {
    use std::cell::Cell;

    /// A planted defect.
    #[derive(Debug, Clone, Copy, PartialEq, Eq)]
    pub enum Fault {
        /// [`super::claim`] answers `Yours` whatever the lease says: a host types on what it
        /// believed when it was elected.
        ClaimBlind,
        /// [`super::step`] ends an open episode at any look that does not read the volume short
        /// by its own floor, as before floors were recorded: a host with a lower floor ends
        /// another's episode, and free space at a floor ends and opens it look after look.
        EndAtOwnFloor,
        /// [`super::step`] ends an open episode at the floor it is held to, with no margin past
        /// it: free space hovering at that floor ends and opens it look after look.
        EndAtHeldFloor,
        /// [`super::step`] keeps a settled or out episode parked however long ago it parked, as
        /// before parked episodes gave way: a volume near its floor is never asked about again.
        ParkForever,
        /// [`super::step`] keeps an owed pick its host's seat rule bars owed, as the look did
        /// when it only dropped the pick's line from its board.
        KeepBarred,
        /// [`super::step`] hands off an owed pick that left its host's supervision as
        /// undelivered even when its host bars it, as the look did when it asked whether the pick
        /// was gone first: an agent that exited cost an election.
        GoneFirst,
        /// [`super::at_break`] claims a line the seat rule bars, as the turn break did when it
        /// read only the transcript.
        TypeBarred,
        /// [`super::step`] hands off a pick whose host ended though this look's host supervises
        /// its session, as the look did before a successor adopted its picks: a seamless
        /// update's successor lost its session's line and the session's place in the episode.
        NoAdopt,
        /// [`super::typed`] counts a line only while the lease still owes it, as before late
        /// records were counted: a line whose record was lost and whose pick was handed off is
        /// never one of the episode's lines, so two more sessions are asked after it.
        CountOwedOnly,
        /// [`super::step`] hands off an owed pick whose host ended at the first look of a host
        /// that does not supervise its session, as before such picks waited [`super::ADOPT_MS`]:
        /// any other host's look between a seamless update and its successor's first look gave
        /// the successor's session away.
        OrphanAtOnce,
        /// [`super::typed`] counts a lost line late but leaves a pick elected after its hand-off
        /// owed when that count reaches [`super::MAX_ASKS`], as before such picks were
        /// withdrawn: the later pick takes a line past the bound.
        OwedPastTwo,
        /// [`super::rests`] lets a look that reads the volume clear rest with its host's line
        /// unrecorded, as the window's look did before it recorded such a line first: where
        /// another host ended the episode before that look, the line's cooldown never started,
        /// and a new episode asked another session within it.
        RestUnrecorded,
        /// [`super::step`] adopts an ended host's owed pick only where the looking host
        /// supervises its session, as before it adopted one its seat rule bars: a pick whose
        /// agent exited around a seamless update was left owed by its successor, which bars it,
        /// and handed off as undelivered once [`super::ADOPT_MS`] was over, spending an election.
        AdoptSupervisedOnly,
        /// [`super::step`] keeps an owed pick its host holds at a usage-limit wall or a box only
        /// its owner can answer ([`super::Look::held`]) owed, as the look did before such picks
        /// were released: the pick waited out its delivery window and was handed off as
        /// undelivered, spending an election, with no line typed.
        KeepHeld,
        /// [`super::step`] runs a round out when nobody but held sessions is left to elect, as
        /// before such rounds waited ([`super::Said::Waiting`]): the episode parked for its
        /// cooldown while a session that could free space was a usage limit's reset away.
        OutWhileHeld,
        /// [`super::step`] drops the nominee of a host read ended at once, as before such
        /// nominees stood [`super::ADOPT_MS`]: a seamless update mid-round left the other host's
        /// look to elect a weaker seat, or nobody, before the successor nominated the session.
        DropEndedNominee,
        /// [`super::step`] releases an owed pick its host holds at a wall or a box and keeps it a
        /// pick, as before such picks went back to the episode ([`super::Said::Returned`]): the
        /// session was never nominated or waited for again that episode, so a round whose only
        /// seat it was ran out, and parked for its cooldown, though it was past minutes later.
        HoldKeepsPick,
        /// [`super::take_over`] takes nothing over, as before a look took over its sessions
        /// before its pass: a seamless update's successor first adopted its session's pick at the
        /// step after its first look's pass, and another host's look while that pass ran, past
        /// [`super::ADOPT_MS`], handed the pick off as undelivered, or dropped its nominee and
        /// elected a weaker seat.
        NoTakeOver,
        /// [`super::take_over`] drops an ended host's record of a held session that the
        /// successor's seat rule does not read held before its pass, and its nominee of a session
        /// that rule holds, as the first take-over did: a round that waited on that record alone
        /// found nobody to elect or wait for at another host's look while the successor's pass
        /// ran, and ran the episode out.
        TakeOverByRule,
        /// [`super::step`] counts none of what aterm's own passes freed since an asked session's
        /// line ([`super::Entry::reclaimed`]), as before the lease recorded them: the cleanup
        /// pass that runs on while the volume is short settled the episode in the session's name,
        /// and nobody else was asked for six hours.
        CreditAterm,
        /// [`super::step`] settles an asked episode while another instance's pass holds the pass
        /// lock ([`super::Look::passing`]): space that pass freed, which no host has recorded yet,
        /// is credited to the asked session.
        SettleWhilePassing,
    }

    thread_local! {
        static ARMED: Cell<Option<Fault>> = const { Cell::new(None) };
    }

    /// Run `f` with `fault` armed on this thread, disarmed after it even on unwind.
    pub fn with<T>(fault: Fault, f: impl FnOnce() -> T) -> T {
        struct Disarm;
        impl Drop for Disarm {
            fn drop(&mut self) {
                ARMED.with(|a| a.set(None));
            }
        }
        ARMED.with(|a| a.set(Some(fault)));
        let _disarm = Disarm;
        f()
    }

    /// Whether `fault` is armed on this thread.
    #[must_use]
    pub fn armed(fault: Fault) -> bool {
        ARMED.with(|a| a.get() == Some(fault))
    }
}

#[cfg(test)]
#[path = "disk_ask_tests.rs"]
mod tests;
