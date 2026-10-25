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
//! - the episode ends when the volume is back at or above its floor.

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

/// No new episode opens on a volume within this of its last line.
pub const COOLDOWN_MS: i64 = 6 * 3_600_000;

/// A typing host's hold on its pick ([`claim`]): no host hands the ask off for this long after
/// the claim, so a line being typed is never typed twice.
pub const TYPE_GRACE_MS: i64 = 10 * 60_000;

/// A rise in free space of at least this, read since the line, is space the asked session freed.
pub const FREED_MIN: u64 = 1 << 30;

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

/// One host machine-wide: its process and that process's start
/// ([`super::super::restart_stamp::kernel_start`]), so a pid the system hands out again is no
/// one's host.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
pub struct HostId {
    pub pid: u32,
    /// The start's spelling; empty where the system does not say it.
    pub start: String,
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
                start: super::super::restart_stamp::kernel_start(pid).unwrap_or_default(),
            }
        })
        .clone()
    }

    /// Whether its process still runs: `false` only where the system says the pid ended or is
    /// another process's. A start the system did not say reads as running, and the deadlines end
    /// what such a host left.
    #[must_use]
    pub fn alive(&self) -> bool {
        self.start.is_empty() || super::super::restart_stamp::alive_as(self.pid, &self.start)
    }
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
    /// `None`: aterm's own passes may still free space here.
    pub done: Option<Done>,
    /// This host's sessions the ask could go to.
    pub seats: &'a [Seat],
    /// This host's supervised sessions, Autopilot not Off: an elected one that leaves them
    /// before its line hands the ask on.
    pub supervised: &'a [String],
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
    /// It came to no turn break within [`DELIVER_MS`], left its host's seats, or its host ended,
    /// before its line.
    Undelivered,
    /// Its host found it may not be asked at its turn break (its task finished or landed, no
    /// task yet), before its line.
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
    /// Back at or above the floor: the episode ended.
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
            Said::Ended { asks } => {
                format!("back at or above the floor: the episode ended after {asks} line(s)")
            }
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
/// off, settle, run out or end the episode. Only the elected session's host types anything
/// ([`claim`]).
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
                let asks = e.asks;
                e.close();
                return Said::Ended { asks };
            }
            _ => return Said::Quiet,
        }
    };
    let now = look.now_ms;
    match entry.phase.clone() {
        Phase::Idle => open(entry, look),
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
        Phase::Owed { pick, until_ms, .. } => {
            let gone = if pick.host == *look.host {
                !look.supervised.contains(&pick.sid)
            } else {
                !(look.alive)(&pick.host)
            };
            if gone || now >= until_ms {
                hand_off(entry, now, pick, Miss::Undelivered)
            } else {
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
        Phase::Settled { .. } => Said::Settled,
        Phase::Out { .. } => Said::Spent,
    }
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
/// the volume's cooldown starts, and the lease's pick (when it is this one) is asked. `false`:
/// the lease had moved on, so the line is counted for the cooldown alone.
pub fn typed(
    lease: &mut Lease,
    key: &str,
    host: &HostId,
    sid: &str,
    now_ms: i64,
    free: u64,
) -> bool {
    let entry = lease.entry_mut(key);
    entry.last_ask_ms = Some(now_ms);
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
        _ => false,
    }
}

/// [`typed`], for the line host `host` last typed on volume `key` (session `sid`, at `at_ms`,
/// `free` bytes free then), where the lease may lack its record because writing it failed (a
/// full disk): recorded only when the volume's last line is older. `true`: it was.
pub fn typed_unrecorded(
    lease: &mut Lease,
    key: &str,
    host: &HostId,
    sid: &str,
    at_ms: i64,
    free: u64,
) -> bool {
    if lease
        .entry(key)
        .and_then(|e| e.last_ask_ms)
        .is_some_and(|last| last >= at_ms)
    {
        return false;
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
    /// The line it is below: its floor, or with `emergency` what writes need.
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

/// The `disk-free@v1` line: the mark, the volume and how full it is, why aterm is done, the
/// largest holders the pass left, what is safe to remove and how to confirm, then
/// `(rule disk-free@v1)`, in at most [`LINE_CAP`] characters. A holder's path is said only in
/// plain characters (letters, digits, `/._-~+@:,=` and spaces) and clipped; any other is said as
/// "a build directory", so a folder's name carries nothing planted into an agent's prompt.
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
        (_, false) => format!("below its {} floor", human_bytes(l.floor)),
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

/// What `aterm harness disk why` prints: one head line per volume the lease holds (its phase,
/// what it waits on, its picks, lines and cooldown at `now_ms`), then `rows` (this rule's ledger
/// rows, oldest first) as [`row_line`] says them.
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
                    "episode since {}{}",
                    at(ms),
                    e.done
                        .map(|d| format!(" ({})", d.word()))
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
            Phase::Owed { pick, until_ms, .. } => format!(
                "owed @{} (host pid {}): its line goes at its next turn break, by {}",
                pick.sid,
                pick.host.pid,
                at(*until_ms)
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
                "settled at {}: @{sid} freed {}",
                at(*at_ms),
                human_bytes(*freed)
            ),
            Phase::Out { at_ms, last } => format!(
                "out at {} ({}): no more agents are asked this episode",
                at(*at_ms),
                last.map_or("no-agent", Miss::word)
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

/// A planted defect for the cross-host claim's negative control
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
