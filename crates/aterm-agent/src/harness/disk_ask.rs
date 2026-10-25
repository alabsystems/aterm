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
//!   ([`rank`]);
//! - only the elected session's own host types, and only while the lease still names it
//!   ([`claim`], then [`typed`]);
//! - a session that frees nothing within [`ANSWER_MS`] of its line or ends hands the ask to one
//!   other session, as does one that never comes to a turn break within [`DELIVER_MS`] or may not
//!   be asked ([`release`]); after [`MAX_ASKS`] lines or [`MAX_PICKS`] elections the episode is
//!   out ([`Said::Out`]) and the owner's disk line says so;
//! - a pick whose host ended is adopted by the host that took over its session (a seamless
//!   update's successor, a new [`HostId`]), and other hosts give it [`ADOPT_MS`] to do so, so an
//!   update hands nothing off;
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
/// then once a minute (the window's `DISK_FIRST_LOOK` and `DISK_TICK`, held to this there), so it
/// adopts the pick before another host's look gives it away. A host that ended with no successor
/// hands the ask on this much later.
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
/// one its host released ([`Miss::Released`]: it may not be asked) costs none, as no session is
/// elected twice.
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
    /// whether or not it is still supervised.
    pub barred: &'a [String],
    /// Every live session of this host: an asked one that leaves them hands the ask on.
    pub live: &'a [String],
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
}

/// Why a pick did not free the volume: what hands the ask on.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum Miss {
    /// It came to no turn break within [`DELIVER_MS`], left its host's supervision, or its host
    /// ended and no host adopted it within [`ADOPT_MS`], before its line.
    Undelivered,
    /// Its host found it may not be asked ([`Look::barred`], or at its turn break its worker's
    /// finding: its task finished, landed or exited, no task yet, its Autopilot Off), before its
    /// line.
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
    /// No more asks this episode; `last`: why the last pick missed (`None`: nobody was nominated).
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
    /// The sessions elected this episode, in order: none is elected twice.
    pub picks: Vec<Pick>,
    /// Of [`Self::picks`], those their hosts released ([`Miss::Released`]).
    pub released: u32,
    /// Lines typed this episode.
    pub asks: u32,
    /// The sessions that took a line this episode, in order.
    pub asked: Vec<String>,
    /// The volume's last line (unix ms), across episodes ([`COOLDOWN_MS`]).
    pub last_ask_ms: Option<i64>,
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
    /// No more asks this episode: the owner's line says so.
    Out {
        last: Option<Miss>,
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
            Said::Handoff { from, .. } => &from.sid,
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
            Said::Out { last: None } => {
                "no host had a session to ask: the owner's disk line says so".to_owned()
            }
            Said::Out { last: Some(why) } => format!(
                "the agents asked did not free it (last: {}): no more are asked; the owner's disk \
                 line says so",
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
            let pick = adopt(entry, pick, look, look.supervised);
            let here = pick.host == *look.host;
            let barred = here && look.barred.contains(&pick.sid);
            #[cfg(any(test, debug_assertions))]
            let barred = barred && !fault::armed(fault::Fault::KeepBarred);
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
            let pick = adopt(entry, pick, look, look.live);
            let best = best_free.max(look.free);
            let rose = best.saturating_sub(free_at);
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

/// `pick`, moved to this look's host when its own host ended and this host has its session among
/// `held` (its supervised sessions while the line is owed, its live ones once it went): a
/// seamless update's successor adopts its sessions under the same sids as a new [`HostId`], so the
/// pick moves to it, in the phase and in [`Entry::picks`], and its line is typed or its answer
/// judged there, instead of the ask being handed on as though its session had gone and the
/// session kept out of the episode.
fn adopt(entry: &mut Entry, pick: Pick, look: &Look<'_>, held: &[String]) -> Pick {
    if pick.host == *look.host || !held.contains(&pick.sid) || (look.alive)(&pick.host) {
        return pick;
    }
    #[cfg(any(test, debug_assertions))]
    if fault::armed(fault::Fault::NoAdopt) {
        return pick;
    }
    let moved = Pick {
        host: look.host.clone(),
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
    entry.asks = 0;
    entry.asked.clear();
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

/// This host's best seat not yet picked this episode, in place of its last nominee.
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
    nominees.retain(|n| n.host != *look.host);
    if let Some(s) = best {
        nominees.push(Nominee {
            host: look.host.clone(),
            sid: s.sid,
            held: s.held,
            at_break: s.at_break,
            seen_ms: look.now_ms,
        });
    }
    if nominees.len() > MAX_NOMINEES {
        nominees.sort_by_key(|n| std::cmp::Reverse(n.seen_ms));
        nominees.truncate(MAX_NOMINEES);
    }
}

fn elect(entry: &mut Entry, look: &Look<'_>, after: Option<Miss>) -> Said {
    let now = look.now_ms;
    let best = match &entry.phase {
        Phase::Nominating { nominees, .. } => nominees
            .iter()
            .filter(|n| !entry.picks.iter().any(|p| p.sid == n.sid))
            .filter(|n| n.host == *look.host || (look.alive)(&n.host))
            .max_by(|a, b| {
                rank((a.held, a.at_break, &a.sid), (b.held, b.at_break, &b.sid))
                    .then_with(|| b.host.cmp(&a.host))
            })
            .cloned(),
        _ => None,
    };
    let Some(n) = best else {
        entry.phase = Phase::Out {
            at_ms: now,
            last: after,
        };
        return Said::Out { last: after };
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

fn hand_off(entry: &mut Entry, now: i64, from: Pick, why: Miss) -> Said {
    if why == Miss::Released {
        entry.released = entry.released.saturating_add(1);
    }
    let picks = u32::try_from(entry.picks.len())
        .unwrap_or(u32::MAX)
        .saturating_sub(entry.released);
    if entry.asks >= MAX_ASKS || picks >= MAX_PICKS {
        entry.phase = Phase::Out {
            at_ms: now,
            last: Some(why),
        };
        return Said::Out { last: Some(why) };
    }
    entry.phase = Phase::Nominating {
        until_ms: now.saturating_add(NOMINATE_MS),
        nominees: Vec::new(),
        after: Some(why),
    };
    Said::Handoff { from, why }
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

/// What the owner's disk line says of volume `entry`'s ask: who was asked and what came of it.
/// `None` before any line of the episode, unless it ran out with nobody to ask.
#[must_use]
pub fn owner_line(entry: &Entry) -> Option<String> {
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
/// carries nothing planted into an agent's prompt.
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
    let holders: Vec<String> = l.holders.iter().take(2).map(|h| holder(h, l)).collect();
    let mut keep = holders.len();
    loop {
        let named = if keep == 0 {
            " aterm's report names no holder it can size: `aterm harness disk` lists them."
                .to_owned()
        } else {
            format!(" Largest left: {}.", holders[..keep].join("; "))
        };
        let text = format!("{HEAD}{fact}{named} {SAFE} {CONFIRM} (rule {RULE_DISK_FREE})");
        if keep == 0 || text.chars().count() <= LINE_CAP {
            return text;
        }
        keep -= 1;
    }
}

fn holder(h: &Hog, l: &Line<'_>) -> String {
    let mine = l
        .checkout
        .zip(h.serves.as_deref())
        .is_some_and(|(c, s)| c == s);
    let place = match h.serves.as_deref() {
        Some(s) if s != h.path => format!("{}'s builds", shown_path(s, l.home)),
        _ => shown_path(&h.path, l.home),
    };
    format!(
        "{place} {}{}",
        human_bytes(h.bytes),
        if mine { " (this tab's)" } else { "" }
    )
}

fn shown_path(p: &Path, home: Option<&Path>) -> String {
    let text = match home.and_then(|h| p.strip_prefix(h).ok()) {
        Some(rel) if !rel.as_os_str().is_empty() => format!("~/{}", rel.display()),
        _ => p.display().to_string(),
    };
    plain(&text).map_or_else(|| "a build directory".to_owned(), |t| clip(&t))
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
        Said::Handoff { why, .. } | Said::Out { last: Some(why) } => {
            o.insert("why".to_owned(), Value::from(why.word().to_owned()));
        }
        Said::Out { last: None } => {
            o.insert("why".to_owned(), Value::from("no-agent".to_owned()));
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
                                "@{} held={} at_break={} (host pid {})",
                                n.sid,
                                human_bytes(n.held),
                                u8::from(n.at_break),
                                n.host.pid
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
                "asked @{} at {}: free {} then, {} at best since; it needs +{} by {}",
                pick.sid,
                at(*typed_ms),
                human_bytes(*free_at),
                human_bytes(*best_free),
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
        out.push(format!("{RULE_DISK_FREE} volume={} {open}: {state}", e.key));
        out.push(format!("  lines {}/{MAX_ASKS}{picks}; {cool}", e.asks));
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
