// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! The state dir: the facts a bridge must not lose across a `kill -9`.
//!
//! Every file is replaced whole: a temp of the writer's own (`create_temp_beside`),
//! then a `rename` over the target, so a reader sees the old value or the new one,
//! never a truncation. A half-written producer sequence is worse than none (the
//! broker would dedup live records away), so nothing is updated in place — but
//! `acked`, a set only ever grown, which is APPENDED to, one offset per
//! `write`, and whose reader takes no unterminated tail as an offset
//! ([`AckedLog`]).
//!
//! ## Atomic and durable writes
//!
//! The failure model is a `kill -9` of the bridge (or a crash of aterm), and the
//! atomic replace alone survives it: the bytes and the rename are in the kernel once
//! the calls return. The exactly-once rungs test it with a real `SIGKILL` at named
//! points.
//!
//! A power loss or an OS crash is survived only by a durable write: the temp flushed
//! before the rename, the directory after it (a flushed file whose entry is not may
//! be gone after the boot). On macOS each flush is `F_FULLFSYNC`, a whole-drive flush
//! of several milliseconds, so it is spent only where a power loss would outlive
//! what the file pairs with:
//!
//! - Durable: `node`, `inc`, `pins`, `halts`, `rewound-log`. The node id is the mail
//!   address and what its caps were minted for; the incarnation lease's top fences
//!   every incarnation and sequence the next boot uses (written once per lease of
//!   [`INC_LEASE`], not once per attach: see the next section); a pin is the T15
//!   first sight of a remote sid, which outlives this host's reboot; a standing halt
//!   is a human's, and the bus row it stands in for may be gone; a rewound identity
//!   stays refused even if its head grows again, and releasing it first flushes the
//!   replacement log's witness. Each is written once, or when its set changes, never
//!   per message.
//! - Atomic only: everything else (`logpin`, the bus mirror's one batch in
//!   flight, written only by a bridge with `[fabric] log = "turns"`, among it). A power loss ends what those files pair with
//!   anyway: the outbox, the owed receipts and the inbox ring live in aterm's memory
//!   (a seamless update carries them, a reboot does not), and aterm mints fresh sids
//!   at every launch, so every file serving a session (`seen/`, `sent/`, `acks/`,
//!   `keys/`, `topics/`, `unhandled/`, `asked`, `deadlines`, `acked`, `inflight/`)
//!   is orphaned by a reboot, flushed or not. The not-hosted gate runs before the
//!   self-lane check, so a stale `acked` judges no record of a session that is gone.
//!   `seq` is fenced by the durable `inc`: the first attach after the boot takes
//!   `inc + 1`, and every sequence of it is above anything a rolled-back `seq` could
//!   name. An attach trusts `inc-lease` only in the boot that wrote it, so no
//!   rolled-back one decides an incarnation. The doctor's `publish_exited`, which
//!   does not roll the incarnation, reads it from any boot ([`StateDir::incarnation`]):
//!   a rolled-back or lost lease names an incarnation at or below the one used, never
//!   above it. It meets a rolled-back `seq` as a deduped publish and refuses
//!   (fail-closed) until a bridge has attached once. A rolled-back `halt-acked`
//!   costs one re-ack, and a rolled-back `bus-log` is a lower high-water, which the
//!   halts do not rest on.
//!
//! ## The incarnation lease
//!
//! An attach takes a fresh incarnation, and the incarnation is the fence on
//! everything the node did before it: every sequence of incarnation n+1 is above
//! incarnation n's reserved top (the will's number), so a rolled-back `seq` reuses
//! nothing, and a predecessor's will that fires late, from a connection still half
//! open, is suppressed by the broker's fence (`Bridge::bring_presence_up`). The
//! fence asks one thing of every attach: that the incarnation it takes is above
//! every incarnation this node ever PRESENTED to a broker — registered a will
//! under, or published under.
//!
//! The local disk has to answer that on its own. The bus's node row is a second
//! opinion (`max(local, bus) + 1`), not a sufficient one: a registered will is a
//! hidden `/a/will` record that no `Last` read returns, and a remote broker keeps it
//! through this host's power loss, on a connection that may still be half open
//! when the host is back. An incarnation whose will is registered and whose `live`
//! row has not landed is invisible on the bus; reusing it lets the old will fire,
//! unfenced, over the new incarnation's `live`, and makes the new one's own will a
//! duplicate that never fires.
//!
//! So `inc` was flushed — temp and directory, two `F_FULLFSYNC`s, 8–12 ms measured
//! in place — before every will, on the attach's critical path. It is still
//! flushed before any incarnation it covers is presented, but as a LEASE: `inc`
//! holds the TOP of a block of [`INC_LEASE`] incarnations, and `inc-lease` (atomic,
//! never flushed) names the one the last attach took, the top it took it under, the
//! very `inc` file that top was flushed into ([`IncFile`]), and the BOOT it was
//! written in ([`crate::durability::this_boot`]). An attach in that same boot, on that
//! same file, trusts `inc-lease` exactly, because the page cache holding it has run
//! throughout (a `kill -9`, a crash of aterm, an update's handoff and a sleep all
//! keep it), and takes the next incarnation of the lease with no flush. An attach
//! in any other boot — or one that cannot name its boot, or whose state dir is off
//! the boot volume, where a disk can vanish while the kernel runs (astream's weak
//! tiers draw the same line: `store.rs`, `Volume`), or a copy of the dir, whose
//! `inc` nobody flushed — trusts only `inc`, takes the incarnation above the whole
//! lease, and leases afresh, flushed, before it is presented.
//! [`StateDir::take_incarnation`] is the one writer.
//!
//! WHY IT HOLDS — every presented incarnation is at or below the `inc` the media
//! holds, and that only grows:
//!
//! - `inc-lease` names a top only after the flushed write of that top returned, and
//!   an attach that takes an incarnation without a flush takes one at or below the
//!   top a lease of THIS boot named.
//! - `inc-lease` is trusted only while the top it names is the top `inc` holds. A
//!   flushed write a `kill -9` cut between its rename and its directory flush (a top
//!   in the page cache that may not be on the media), or an older build that took
//!   `inc + 1` and wrote that, leaves the two apart, and the attach leases afresh.
//! - `inc-lease` is trusted only while the `inc` file and the directory it names are
//!   the ones there now: a copy of the dir (`cp -R`, a Finder duplicate, `rsync`, a
//!   backup restored) carries this boot's lease and its top beside an `inc`, in a
//!   directory, that nobody flushed, and leases afresh at its first attach.
//! - `seq`'s incarnation counts as taken: an older build's doctor publishes under
//!   whatever `inc` holds, a lease's unused top included. This build's doctor never
//!   does ([`StateDir::incarnation`]).
//!
//! A POWER LOSS at any step: the next boot trusts only `inc`, whose media value is
//! at least every top a lease ever named, so it takes an incarnation above
//! everything presented, whichever versions of `inc-lease` and `seq` the media kept.
//! A KILL at any step of one boot: until `inc-lease` names the new incarnation,
//! nothing of it was presented, and the next attach takes it again or (a top already
//! renamed in) leases past it; once it does, the next attach takes the one above.
//! The tests walk every step against a simulated power loss
//! (`the_incarnation_lease_refines_its_model`), bound to the model
//! `aterm-models-session` states (`incarnation_lease_model`); the copy, the doctor
//! and the shipped trust decision have their own
//! (`a_copied_state_dir_flushes_its_own_inc_before_trusting_a_lease`,
//! `the_doctor_never_answers_a_lost_leases_unused_top`,
//! `the_shipped_trust_decision_leases_a_dir_on_the_boot_volume`).
//!
//! NOT an overlap of the flush with the will's and the `live` row's round trips,
//! joined before the first non-presence sequence. That lets the will reach the
//! broker before `inc` is on the media, and a power loss in that window leaves a
//! registered `/a/will` (on a remote broker, with its connection half open; on a
//! local one, fired at the broker's next open, but only enqueued, so the attach's
//! `Last` read can be served first) beside an `inc` that never moved: the next
//! attach takes the same incarnation. The model's `Buggy = 4` is that overlap.
//!
//! Offsets name records in one log. `seen/`, `topics/`, `unhandled/`, `acked`,
//! `halt-acked`, `inbox-owed`, `deadlines`, `asked` and the `original-off` of `keys/`
//! and `sent/` hold bus offsets, each meaningless on another log, as is an `inflight/` record's
//! witnessed reference into one. The attach that finds another log ([`LogWitness`])
//! forgets those offsets before using them ([`StateDir::forget_log_offsets`]); a
//! reference witnessed on an unknown log stays uncertain. The rest of the dir is the
//! node's own and outlives any log.
//!
//! One state dir is one bridge. The dir is the node (its id, producer id and
//! sequence), so a second bridge on it would publish as the same producer from a
//! second counter: the two would dedup each other's records away and refuse each
//! other's own-lane mail as forged. [`StateDir::lock_bridge`] is the exclusive hold a
//! bridge keeps for its whole life.
//!
//! | file | write | what it holds | why losing it is not fatal |
//! |---|---|---|---|
//! | `lock` | — | nothing: the bridge's `flock` ([`StateDir::lock_bridge`]) | it is recreated empty; only the lock means anything |
//! | `pid` | — | the pid of the lock's holder | it names the holder in a refusal, nothing else |
//! | `node` | durable | this node's `n-<16 hex>` id | load-bearing: a new id is a new node, without its mail lane |
//! | `inc` | durable | the top of the incarnation LEASE: at or above every incarnation this node presented, flushed before the first of them is ([`StateDir::take_incarnation`]) | the bus's last presence row counts too: `max(local, Last) + 1` |
//! | `inc-lease` | atomic | `used=<inc> top=<inc> boot=<id> file=<dev>:<dir>:<ino>`: the incarnation the last attach took, the `inc` it took it under, the boot that wrote it, and the `inc` file that top was flushed into ([`IncLease`]) | the next attach leases afresh above `inc`: one flush, and the lease's unused rest burned; the doctor answers `seq`'s incarnation |
//! | `seq` | atomic | the last producer sequence of the newest reserved BLOCK, written before any number of the block is used ([`StateDir::reserve_sequence_block`]) | a `kill -9` skips the block's unused rest and reuses none; after a power loss, no dup either: the incarnation moved, and every sequence of `inc+1` is above `inc`'s reserved top |
//! | `seen/<sid>` | atomic | where the session's addressed refill starts after: the endpoint's reported floor minus one ([`StateDir::seen_off`]) | the refill starts lower and the ring dedups on `off=` |
//! | `acked` | append | the offsets with a `PublishAck` on this node's own lanes, each ended by its comma, one appended per own-lane publish, in one `write`, and compacted to the newest `keep` at twice that ([`AckedLog`]) | the self-lane check gets more conservative, never less |
//! | `inflight/<seq>` | atomic | one record bound for this node's own lanes and the log it was decided on, from before its publish until `acked` holds its offset ([`StateDir::in_flight`]) | the attach cannot re-learn an offset a kill left out of `acked`, and the self-lane check accuses that one record of ours |
//! | `pins` | durable | the TOFU `sid → node` map, newest [`PINS_KEEP`] | a re-pin on next sight, the TOFU rule anyway |
//! | `halt-acked` | atomic | the highest fleet-halt offset this node has answered | a re-ack of an answered barrier: one extra retained row |
//! | `inbox-owed` | atomic | the inbox-group commit a bridge owed when it exited with no broker connection to make it on, every record at or below it accounted for ([`StateDir::inbox_owed`]); removed once the next attach commits it | the group offers those records again, and an endpoint that no longer remembers their offsets (an update's successor) lists them as new |
//! | `bus-log` | atomic | the identity of the log this node reads and the highest broker head seen there ([`LogWitness`]) | a replaced log is judged by its head alone until the next attach records the identity; the halts do not rest on it |
//! | `rewound-log` | durable | one known identity whose head this node saw go backwards | regrowth cannot make its reused offsets trustworthy; only a different known log releases the refusal |
//! | `halts` | durable | the fleet halts this node holds in force ([`StandingHalt`]) | the attach re-reads them from the bus; only one whose row the bus lost goes with it |
//! | `sent/<sid>.<id>` | atomic | the sequence reserved for one queued post, its publication identity, and whether a `key=` entry chose it, one line an older bridge reads too | load-bearing: a fresh sequence is a second copy |
//! | `keys/<sid>` | atomic | `post key=` → its reserved sequence, newest [`KEYS_KEEP`] per session | a re-post under a lost key is a second record |
//! | `acks/<sid>.<rid>` | atomic | the sequence reserved for one owed receipt (R8), removed at retirement | a fresh sequence is a second `ack` after a crash between publish and retirement |
//! | `asked` | atomic | the recipient of each `ask`/`task` this node published, newest [`ASKED_KEEP`] | a reply to a forgotten ask is judged without its recipient (a deadline's `to` still counts) |
//! | `deadlines` | atomic | each `ask`/`task` published with `dl=` and not yet answered, newest [`DEADLINES_KEEP`] | no `expired` for those asks; the bus is checked before any verdict, so never a false one |
//! | `topics/<sid>` | atomic | `<topic> <next>` per broadcast opt-in ([`StateDir::topics`]) | the topic restarts from the session's `since=`, and `head` skips what was published meanwhile, so it is written every step |
//! | `unhandled/<sid>` | atomic | `<topic> <off>`: per topic, the lowest offset of a broadcast the endpoint took as new and `inbox seen` has not passed, written before the cursor moves past it ([`StateDir::topic_floors`]) | a successor that keeps the sid (a seamless update, a PTY keeper's recovery) is not offered those broadcasts again, though they are gone from its inbox |

use std::collections::BTreeMap;
use std::io::{self, Write};
use std::path::{Path, PathBuf};

/// One outstanding deadline: an `ask` or `task` this node published with `dl=` and
/// has not yet seen an `answer`, `report` or `ack` for.
///
/// The asker's own bridge records `expired re=<off> dl=<ms>` when it passes (§6.4,
/// R8; the broker holds no timers). `at` is absolute (the publish clock plus `dl`),
/// so a relaunched bridge does not restart it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Deadline {
    /// The bus offset of the ask (what a reply carries back as `re=`).
    pub off: u64,
    /// The asking session.
    pub sid: String,
    /// When it expires, in `now_ms()`.
    pub at: u64,
    /// The advisory `dl=` the ask carried, echoed onto the verdict.
    pub dl: u64,
    /// The principal whose reply settles it: the node (or `p/` principal) owning the
    /// lane the ask went to ([`Asked`]). `None` for a line without `to=`, which any
    /// reply settles.
    pub to: Option<String>,
}

impl Deadline {
    /// The one line this deadline is stored as.
    #[must_use]
    pub fn render(&self) -> String {
        let mut line = format!(
            "off={} sid={} at={} dl={}",
            self.off, self.sid, self.at, self.dl
        );
        if let Some(to) = &self.to {
            line.push_str(&format!(" to={to}"));
        }
        line
    }

    /// Parse one back; a partial line is `None`, never a deadline to publish a verdict on.
    #[must_use]
    pub fn parse(line: &str) -> Option<Self> {
        let (mut off, mut sid, mut at, mut dl, mut to) = (None, None, None, None, None);
        for tok in line.split_whitespace() {
            match tok.split_once('=') {
                Some(("off", v)) => off = v.parse().ok(),
                Some(("sid", v)) => sid = Some(v.to_string()),
                Some(("at", v)) => at = v.parse().ok(),
                Some(("dl", v)) => dl = v.parse().ok(),
                Some(("to", v)) => to = Some(v.to_string()),
                _ => {}
            }
        }
        let sid: String = sid?;
        if sid.is_empty() {
            return None;
        }
        // A `to=` that is present and empty is a damaged line, not an old one.
        if to.as_deref() == Some("") {
            return None;
        }
        Some(Self {
            off: off?,
            sid,
            at: at?,
            dl: dl?,
            to,
        })
    }
}

/// One `ask`/`task` this node published, and the principal whose reply it is: the
/// node owning the lane it went to (`/f/<F>/in/<node>/<sid>/…`) or the principal of a
/// `p/` lane (`/f/<F>/in/p/<principal>/…`). The broker binds a reply's `<src>`
/// segment to its publisher, so this tells the recipient's receipt from a third
/// node's record naming the same offset.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Asked {
    /// The bus offset of the ask.
    pub off: u64,
    /// The principal whose reply it is.
    pub to: String,
}

impl Asked {
    /// The one line this entry is stored as: `<off> <to>`.
    #[must_use]
    pub fn render(&self) -> String {
        format!("{} {}", self.off, self.to)
    }

    /// Parse one back; a partial line is `None`.
    #[must_use]
    pub fn parse(line: &str) -> Option<Self> {
        let mut toks = line.split_whitespace();
        let off = toks.next()?.parse().ok()?;
        let to = toks.next()?.to_string();
        toks.next().is_none().then_some(Self { off, to })
    }
}

/// How many published asks the node remembers the recipient of, newest last.
/// A `post --wait-ack` parks for at most 600 s, and a deadline carries its
/// own [`Deadline::to`]: this only has to outlive the waits, with room.
pub const ASKED_KEEP: usize = 1024;

/// How many `post key=` reservations one session keeps, newest last: R7's bound on
/// the broker's dedup window. A re-post under an evicted key is a new record.
pub const KEYS_KEEP: usize = 4096;

/// How many outstanding deadlines the node keeps, newest offset last.
pub const DEADLINES_KEEP: usize = 4096;

/// How many TOFU pins the table keeps, oldest first sight out.
///
/// `pins` gains a line per remote sid ever addressed by a bare `@s-<sid>`, sids are
/// never reused, and `Bridge::pinned_node_for` re-reads the file on every such post.
/// Eviction is by first sight, not use: recency would cost a durable rewrite per
/// post, and a re-pin on next sight is the TOFU rule. An evicted pin costs the T15
/// protection for a sid not among the last [`PINS_KEEP`] fresh ones. Deleting
/// `<state>/pins` is safe.
pub const PINS_KEEP: usize = 4096;

/// One session's position on one broadcast topic: the offset it resumes from after a
/// bridge restart.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct TopicCursor {
    /// The next offset this topic may deliver from.
    pub next: u64,
}

impl TopicCursor {
    /// Parse the state file's `<topic> <next>` line; `None` for anything else, which
    /// the caller reports. A third number (a 0.90.x bridge's add serial) is read past.
    ///
    /// The topic is held to the grammar ([`crate::subject::is_topic`]) like every
    /// other way a topic enters the bridge: the catch-up builds a `Fetch` filter from
    /// it, and no broadcast under any other name can be delivered anyway.
    #[must_use]
    pub fn parse(line: &str) -> Option<(String, TopicCursor)> {
        let mut words = line.split_whitespace();
        let topic = words.next().filter(|t| crate::subject::is_topic(t))?;
        // Digits only: `u64::from_str` would accept `+5`.
        let number = |w: &str| {
            (!w.is_empty() && w.bytes().all(|b| b.is_ascii_digit()))
                .then(|| w.parse::<u64>().ok())?
        };
        let next = number(words.next()?)?;
        if let Some(extra) = words.next() {
            number(extra)?;
        }
        if words.next().is_some() {
            return None;
        }
        Some((topic.to_string(), TopicCursor { next }))
    }
}

/// What this node knows of the bus log it reads: the `bus-log` file.
///
/// Every offset in this dir is an offset on one log and means nothing on another:
/// a log deleted and recreated, truncated by a repair, restored from a backup, or
/// another host's broker gives the same numbers to different records.
/// [`LogWitness::replaced_by`] is the one judgement of "this is not the log those
/// offsets index", from two kinds of evidence:
///
/// * The log's identity, which astream answers every `Hello` with
///   ([`astream_broker::LogIdentity`]: a name for the log and the generation of its
///   offsets). Any difference is another log, whatever its head. A broker that
///   predates identity answers none, which says nothing either way; the identity
///   seen last stays the witness.
/// * The high-water: offsets on one log only grow, so a head below the highest this
///   node saw there is another log (or the same one rolled back under its own
///   identity, which reuses offsets just the same). It is the only evidence against
///   a broker without identity, and it catches only a shorter log.
///
/// Every offset in the dir rests on the verdict ([`StateDir::forget_log_offsets`]),
/// and so does the reporting; the halts do not ([`StandingHalt`]). Unknown tokens,
/// and an `id=` that does not parse, are read past, so a newer bridge's line still
/// parses.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct LogWitness {
    /// The highest broker head (one past the newest offset) this node has seen
    /// on the log it reads.
    pub high: u64,
    /// The identity of that log, once a broker that answers one was read.
    pub id: Option<astream_broker::LogIdentity>,
}

impl LogWitness {
    /// The one line this is stored as: `[id=<identity> ]high=<n>`.
    #[must_use]
    pub fn render(&self) -> String {
        match self.id {
            Some(id) => format!("id={id} high={}", self.high),
            None => format!("high={}", self.high),
        }
    }

    /// Parse one back: `None` without a readable `high=`.
    #[must_use]
    pub fn parse(line: &str) -> Option<Self> {
        let high = line
            .split_whitespace()
            .find_map(|tok| tok.strip_prefix("high="))
            .filter(|v| !v.is_empty() && v.bytes().all(|b| b.is_ascii_digit()))?
            .parse()
            .ok()?;
        let id = line
            .split_whitespace()
            .find_map(|tok| tok.strip_prefix("id="))
            .and_then(astream_broker::LogIdentity::parse);
        Some(Self { high, id })
    }

    /// Whether a broker answering `head` under `id` is provably not the log this
    /// witness saw: another identity, or a head below one this node already read
    /// there.
    #[must_use]
    pub fn replaced_by(&self, head: u64, id: Option<astream_broker::LogIdentity>) -> bool {
        head < self.high || matches!((self.id, id), (Some(was), Some(now)) if was != now)
    }

    /// WHERE THE LOG THIS WITNESS SAW CONTINUES on the log a broker answers as
    /// `id`, with the `lineage` its same `Hello` told
    /// ([`astream_broker::Client::log_lineage`]): `Some(k)` — every offset below
    /// `k` names the same record on both, so an offset this dir holds resumes
    /// at the lower of itself and `k` ([`StateDir::forget_log_offsets`]'s
    /// `keep_below`) instead of being forgotten; `None` — another log as far as
    /// anything can tell, and every offset is forgotten, as before lineage.
    ///
    /// A moved GENERATION of the same log continues when astream recorded
    /// where the generations agree ([`astream_broker::LogIdentity::continues`]):
    /// after a power loss under a weak tier, a torn batch cut, a repair's cut.
    /// It never continues across another `id`, without a witnessed identity
    /// (a witness written before identity, a broker before it), without
    /// lineage (a broker before it, or a log found shorter, re-based or foreign,
    /// which astream records none for), or past the newest
    /// [`astream_broker::Lineage::MAX`] steps. The same identity is
    /// `Some(u64::MAX)` — nothing to clamp — and a head below the high-water
    /// under it is the rewound log a bridge refuses, never one it continues.
    #[must_use]
    pub fn continues(
        &self,
        id: Option<astream_broker::LogIdentity>,
        lineage: Option<&astream_broker::Lineage>,
    ) -> Option<u64> {
        let (was, now) = (self.id?, id?);
        if was == now {
            return Some(u64::MAX);
        }
        now.continues(was, lineage?)
    }
}

/// One fleet halt this node holds in force ([`StateDir::standing_halts`]).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct StandingHalt {
    /// The `reason=` the sessions are held under: the human's own words, the
    /// pct token the bridge passed to `hold` (it holds no space).
    pub reason: String,
    /// `true` when the face this node reads carries no row for it — it was
    /// read on a log since replaced — so it stands on this file alone, and
    /// only its human's own row on this log lifts it.
    pub kept: bool,
}

impl StandingHalt {
    /// `<fleet> <human> <reason> witnessed|kept`.
    #[must_use]
    pub fn render(&self, fleet: &str, human: &str) -> String {
        let how = if self.kept { "kept" } else { "witnessed" };
        format!("{fleet} {human} {} {how}", self.reason)
    }

    /// Parse one back as `(fleet, human, halt)`; a partial line, or a "human" that is
    /// not an `h-` principal, is `None`.
    #[must_use]
    pub fn parse(line: &str) -> Option<(String, String, Self)> {
        let mut toks = line.split_whitespace();
        let (fleet, human, reason, how) = (toks.next()?, toks.next()?, toks.next()?, toks.next()?);
        if toks.next().is_some() || !human.starts_with("h-") {
            return None;
        }
        let kept = match how {
            "kept" => true,
            "witnessed" => false,
            _ => return None,
        };
        Some((
            fleet.to_string(),
            human.to_string(),
            Self {
                reason: reason.to_string(),
                kept,
            },
        ))
    }
}

/// One state-file line, bounded and stripped of control characters for a
/// report: the file is on disk and anything that can write the state directory
/// can put an escape sequence in it, and this line goes to a terminal.
fn safe_line(line: &str) -> String {
    line.chars().filter(|c| !c.is_control()).take(120).collect()
}

/// Flush a directory, so the entries a rename put in it survive a power loss.
///
/// Tolerates what astream's log does (`store.rs`'s `sync_parent_dir`): a filesystem
/// that cannot flush a directory handle (`Unsupported`, `InvalidInput`), a
/// directory this process may write but not open (`PermissionDenied`), one already
/// gone (`NotFound`). The file's bytes are flushed already, and refusing here would
/// refuse a state dir that works.
pub(crate) fn sync_dir(dir: &Path) -> io::Result<()> {
    let dir = if dir.as_os_str().is_empty() {
        Path::new(".")
    } else {
        dir
    };
    #[expect(
        clippy::disallowed_methods,
        reason = "aterm-link's flushes are the Ordered-tier work's to classify; the full flush is kept until then"
    )]
    match std::fs::File::open(dir).and_then(|d| d.sync_all()) {
        Err(e)
            if !matches!(
                e.kind(),
                io::ErrorKind::Unsupported
                    | io::ErrorKind::InvalidInput
                    | io::ErrorKind::PermissionDenied
                    | io::ErrorKind::NotFound
            ) =>
        {
            Err(e)
        }
        _ => Ok(()),
    }
}

/// The reserved top of an incarnation's sequence space: the will's sequence, so no
/// ordinary publish of that incarnation can collide with it (§7).
pub const WILL_SEQ_LOW: u64 = 0xFFFF_FFFF;

/// The highest incarnation a node may run under: the last whose will,
/// `(inc << 32) | WILL_SEQ_LOW`, sits below astream's reserved ack half
/// ([`astream_broker::ACK_SEQ_BASE`]), where a will is refused (`SeqReserved`) and an
/// ordinary publish is the caller's to keep out (`Client::publish`'s contract).
/// Nothing above it is adopted, persisted or reserved under.
pub const INC_MAX: u64 = (astream_broker::ACK_SEQ_BASE >> 32) - 1;
// The will's sequence of INC_MAX is below the reserved half and the next
// incarnation's is not. `+`, which is `|` here: `inc << 32` has no low bits.
const _: () = assert!((INC_MAX << 32) + WILL_SEQ_LOW < astream_broker::ACK_SEQ_BASE);
const _: () = assert!(((INC_MAX + 1) << 32) + WILL_SEQ_LOW >= astream_broker::ACK_SEQ_BASE);

/// How many incarnations one flushed write of `inc` covers (the module doc's "The
/// incarnation lease"): the attaches of one boot pay one flush per lease instead of
/// one each, and a boot that cannot trust the last lease burns at most this many of
/// [`INC_MAX`]'s 2^31 − 1 — a whole lease burned at every boot would last 33 million
/// boots. The bridge's sequence block (`SEQ_BLOCK`) makes the same trade one level
/// down, and for the same reason: a number nobody reads back until the next start
/// is written once per block, and a crash burns the block's unused rest.
pub const INC_LEASE: u64 = 64;
const _: () = assert!(INC_LEASE >= 1 && INC_LEASE <= INC_MAX);

/// What `inc-lease` holds: the incarnation the last attach took (`used`), the top of
/// the lease it took it under — what `inc` held, FLUSHED, when this was written —
/// the boot that wrote it, `None` where the kernel names none
/// ([`crate::durability::this_boot`]) or the state dir is off the boot volume, and the
/// very `inc` file that flush went into ([`IncFile`]). The module doc's "The
/// incarnation lease" is the argument; [`StateDir::take_incarnation`] is the one
/// writer.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct IncLease {
    /// The incarnation the last attach took.
    pub used: u64,
    /// The lease's top, flushed into `inc` before `used` was presented.
    pub top: u64,
    /// The boot that wrote it, the one boot whose page cache vouches for it.
    pub boot: Option<[u8; 16]>,
    /// The `inc` the top was flushed into, `None` where the platform names no inode.
    pub file: Option<IncFile>,
}

/// WHICH `inc` A LEASE VOUCHES FOR: its device, the state dir's inode and `inc`'s own
/// inode, as the flushed write left them ([`StateDir::inc_file`]).
///
/// A lease's top was flushed into ONE file, renamed into ONE directory whose entry was
/// then flushed. A copy of the state dir in the same boot — `cp -R`, a Finder
/// duplicate, `rsync`, a backup restored, or `cp -al`'s hard links — carries the same
/// lease and the same top in a directory, and files, that nobody flushed: trusted on
/// boot and top alone, the copy would present a whole lease of incarnations with no
/// flushed `inc` under them, and after a power loss take one of them again. A copy's
/// directory is another inode, and a restored `inc` is another file, so the lease
/// names both, and is trusted only where both are still the ones it names
/// ([`plan_incarnation`]). A flushed write renames a NEW file in, so the lease it
/// writes names the new one.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct IncFile {
    /// The device both live on.
    pub dev: u64,
    /// The state dir's inode.
    pub dir: u64,
    /// `inc`'s inode.
    pub ino: u64,
}

impl IncLease {
    /// The one line it is stored as.
    pub(crate) fn render(&self) -> String {
        let boot = self
            .boot
            .map_or_else(|| "-".to_string(), |b| aterm_codec::hex::encode_lower(&b));
        let file = self.file.map_or_else(
            || "-".to_string(),
            |f| format!("{}:{}:{}", f.dev, f.dir, f.ino),
        );
        format!(
            "used={} top={} boot={boot} file={file}",
            self.used, self.top
        )
    }

    /// Parse one back: all four fields once each, `used` at or below `top`, a boot that
    /// is `-` or a boot id, and a file that is `-` or `<dev>:<dir>:<ino>`. Anything
    /// else — a torn or hand-edited file — is `None`, which the next attach reads as no
    /// lease at all: the direction that flushes, never the one that trusts.
    pub(crate) fn parse(text: &str) -> Option<Self> {
        let (mut used, mut top, mut boot, mut file) = (None, None, None, None);
        for tok in text.split_whitespace() {
            match tok.split_once('=')? {
                ("used", v) if used.is_none() => used = Some(v.parse().ok()?),
                ("top", v) if top.is_none() => top = Some(v.parse().ok()?),
                ("boot", "-") if boot.is_none() => boot = Some(None),
                ("boot", v) if boot.is_none() => {
                    boot = Some(Some(astream_sys::parse_boot_id(v.as_bytes())?));
                }
                ("file", "-") if file.is_none() => file = Some(None),
                ("file", v) if file.is_none() => {
                    let mut ids = v.split(':').map(|n| n.parse::<u64>().ok());
                    let (dev, dir, ino) = (ids.next()??, ids.next()??, ids.next()??);
                    if ids.next().is_some() {
                        return None;
                    }
                    file = Some(Some(IncFile { dev, dir, ino }));
                }
                _ => return None,
            }
        }
        let (used, top, boot, file) = (used?, top?, boot?, file?);
        (used <= top).then_some(Self {
            used,
            top,
            boot,
            file,
        })
    }
}

/// What one attach does about its incarnation ([`plan_incarnation`]).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct IncPlan {
    /// The incarnation it takes.
    pub next: u64,
    /// The lease top `inc-lease` will name, flushed into `inc` first when `flush`.
    pub top: u64,
    /// Whether `inc` is flushed before `next` may be presented.
    pub flush: bool,
}

/// One past both the state dir's and the bus's incarnation, or `None` past
/// [`INC_MAX`] — checked rather than wrapped, so a corrupt `inc` file is a named
/// refusal and not an incarnation that collides with an old one.
pub(crate) fn next_incarnation(local: u64, bus: u64) -> Option<u64> {
    local.max(bus).checked_add(1).filter(|inc| *inc <= INC_MAX)
}

/// THE ATTACH'S DECISION, from what the disk and the bus say: `top` is what `inc`
/// holds and `file` which file holds it ([`IncFile`]), `lease` what `inc-lease`
/// holds, `seq_inc` the incarnation `seq`'s number belongs to, `boot` this boot when
/// the page cache can vouch for the state dir (the kernel names the boot and the dir
/// is on the boot volume), and `bus` the newest `inc=` on the node's own presence
/// face. `None` past [`INC_MAX`].
///
/// The lease is TRUSTED only when this boot wrote it, the top it names is the top
/// `inc` holds, and the `inc` it names is this very file in this very directory;
/// then what it says was taken is exactly what was taken. Otherwise everything up to
/// `inc` counts as taken. `seq`'s incarnation counts either way. The incarnation
/// taken is one past all of that and the bus; it is covered with no flush when a
/// trusted lease's top is at or above it, and leased afresh — one flush of `inc`
/// covering `span` incarnations ([`INC_LEASE`]) — when not. A boot the page cache
/// cannot vouch for gets a lease of one (`inc` is then exactly the incarnation taken,
/// as the file always was), since no later attach could trust the rest of a longer
/// one.
pub(crate) fn plan_incarnation(
    top: u64,
    file: Option<IncFile>,
    lease: Option<IncLease>,
    seq_inc: u64,
    boot: Option<[u8; 16]>,
    bus: u64,
    span: u64,
) -> Option<IncPlan> {
    let trusted = lease.filter(|l| {
        boot.is_some() && l.boot == boot && l.top == top && file.is_some() && l.file == file
    });
    let taken = trusted.map_or(top, |l| l.used).max(seq_inc);
    let next = next_incarnation(taken, bus)?;
    if trusted.is_some() && next <= top {
        return Some(IncPlan {
            next,
            top,
            flush: false,
        });
    }
    let span = if boot.is_some() { span.max(1) } else { 1 };
    Some(IncPlan {
        next,
        top: next.saturating_add(span - 1).min(INC_MAX),
        flush: true,
    })
}

/// Whether `dir` shares a device with the boot volume: the device that loses power
/// only together with the kernel, so a page cache that has run throughout this boot
/// still holds every write made to it. astream's weak tiers ask the same question of
/// their log's directory (`astream-broker`'s `store.rs`, `on_boot_volume`). Off it, a
/// removable or network disk can vanish while the kernel runs and take an unflushed
/// write with it, so no lease written there is trusted.
#[cfg(unix)]
fn on_boot_volume(dir: &Path) -> bool {
    use std::os::unix::fs::MetadataExt;
    let Ok(here) = std::fs::metadata(dir) else {
        return false;
    };
    // APFS splits the boot volume into the read-only system volume (`/`) and the
    // data volume (`/System/Volumes/Data`, where `~` lives); both sit on the one
    // internal device.
    let roots: &[&str] = if cfg!(target_vendor = "apple") {
        &["/", "/System/Volumes/Data"]
    } else {
        &["/"]
    };
    roots
        .iter()
        .any(|r| std::fs::metadata(r).is_ok_and(|m| m.dev() == here.dev()))
}

/// No device to compare: never the boot volume, so never a trusted lease.
#[cfg(not(unix))]
fn on_boot_volume(_dir: &Path) -> bool {
    false
}

/// The points of [`StateDir::take_incarnation`] a test can kill it at
/// ([`IncSeam::kill_at`]). A kill anywhere else is a kill before the take, or one
/// after it returned and before the will went out, which the walk stands in
/// without help.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum IncStep {
    /// `inc`'s flushed write has renamed its temp over `inc` and not yet flushed the
    /// directory: the new top is in the page cache, and maybe not on the media.
    Renamed,
    /// `inc`'s flushed write returned; `inc-lease` is not yet written.
    Flushed,
}

/// TEST-ONLY: what [`StateDir::take_incarnation`] is told instead of asking, where it
/// is killed, and which defect it carries — the incarnation lease's conformance walk
/// (`the_incarnation_lease_refines_its_model`).
#[cfg(test)]
#[derive(Clone, Copy, Debug, Default)]
pub(crate) struct IncSeam {
    /// This boot, as the walk stands it in: `Some(None)` is a boot the kernel cannot
    /// name (or a dir off the boot volume); `None` asks the kernel.
    pub boot: Option<Option<[u8; 16]>>,
    /// The step the next take dies at.
    pub kill_at: Option<IncStep>,
    /// One defect, the walk's negative controls (the model's `Buggy`): 0 none; 1 a
    /// lease trusted across boots; 2 a lease trusted whose top is not `inc`'s; 3 the
    /// lease written before its top is flushed (naming the `inc` file it found, the
    /// one it can name before the rename).
    pub mutant: u8,
    /// The lease a flush covers, short enough for the walk to run one out (the
    /// model's `Span`); `None` is [`INC_LEASE`].
    pub span: Option<u64>,
    /// The walk stands in the media (a power loss is its model's step), so a durable
    /// write skips its two flushes' system calls ([`StateDir::flushes`]).
    pub simulated_media: bool,
}

/// The bridge's state directory.
pub struct StateDir {
    root: PathBuf,
    /// TEST-ONLY: what this dir's writers did to the filesystem ([`FileOps`]).
    #[cfg(test)]
    ops: std::sync::Mutex<FileOps>,
    /// TEST-ONLY: the incarnation take's boot, kill point and defect ([`IncSeam`]).
    #[cfg(test)]
    inc_seam: std::sync::Mutex<IncSeam>,
}

/// The filesystem operations one [`StateDir`] made, by kind (counted only in a
/// test build, [`StateDir::ops`]), so a test states what a post COSTS as a count
/// — deterministic for a given build — rather than as a time this shared machine
/// would make into noise.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub(crate) struct FileOps {
    /// Reservation and journal reads ([`StateDir::read_reservation`]), hits and
    /// misses alike.
    pub reads: u64,
    /// Atomic replaces: a fresh temp written, then renamed over the target.
    pub replaces: u64,
    /// Renames, the replaces' own included.
    pub renames: u64,
    /// `fsync`s (`F_FULLFSYNC` on macOS): a durable replace flushes its file
    /// and its directory.
    pub fsyncs: u64,
    /// Unlinks of a journal entry or a reservation.
    pub unlinks: u64,
    /// Appends to an append-only file.
    pub appends: u64,
}

#[cfg(test)]
impl std::ops::Sub for FileOps {
    type Output = Self;

    fn sub(self, before: Self) -> Self {
        Self {
            reads: self.reads - before.reads,
            replaces: self.replaces - before.replaces,
            renames: self.renames - before.renames,
            fsyncs: self.fsyncs - before.fsyncs,
            unlinks: self.unlinks - before.unlinks,
            appends: self.appends - before.appends,
        }
    }
}

/// The original publication's face and semantic digest, persisted before send.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct PublishIdentity {
    pub subject: String,
    pub digest: String,
}

/// A sequence reservation. A file without an identity needs independent evidence
/// before a deduped offset is accepted.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct PublishPin {
    pub seq: u64,
    pub via_key: bool,
    pub identity: Option<PublishIdentity>,
    /// A key's previous winner; the attempt itself may carry different bytes.
    pub original: Option<PublishIdentity>,
    /// The key winner's landed offset on the witnessed log, distinct from a
    /// later attempt's offset. Forgotten when that log is replaced.
    pub original_off: Option<u64>,
    /// The original post within this session's key file, before any publish.
    pub owner: Option<u64>,
}

impl PublishPin {
    pub(crate) fn parse(text: &str) -> io::Result<Self> {
        let bad = || {
            io::Error::new(
                io::ErrorKind::InvalidData,
                "invalid publication reservation",
            )
        };
        let mut words = text.split_whitespace();
        let seq = words.next().and_then(|s| s.parse().ok()).ok_or_else(bad)?;
        let mut tail: Vec<&str> = words.collect();
        let via_key = tail.first() == Some(&"key");
        if via_key {
            tail.remove(0);
        }
        let (mut identity, mut original, mut original_off, mut owner) = (None, None, None, None);
        let mut parts = tail.as_slice();
        while !parts.is_empty() {
            if parts[0] == "owner" && owner.is_none() {
                owner = Some(
                    parts
                        .get(1)
                        .and_then(|s| s.parse::<u64>().ok())
                        .filter(|id| *id > 0)
                        .ok_or_else(bad)?,
                );
                parts = &parts[2..];
                continue;
            }
            if parts[0] == "original-off" && original_off.is_none() {
                original_off = Some(parts.get(1).and_then(|s| s.parse().ok()).ok_or_else(bad)?);
                parts = &parts[2..];
                continue;
            }
            if parts.len() < 3 {
                return Err(bad());
            }
            let (label, subject, digest) = (parts[0], parts[1], parts[2]);
            if !is_face(subject) || !aterm_codec::hex::is_lower(digest, 64) {
                return Err(bad());
            }
            let slot = match label {
                "identity" if identity.is_none() => &mut identity,
                "original" if original.is_none() && via_key => &mut original,
                _ => return Err(bad()),
            };
            *slot = Some(PublishIdentity {
                subject: subject.to_string(),
                digest: digest.to_string(),
            });
            parts = &parts[3..];
        }
        if (original.is_some() || original_off.is_some()) && identity.is_none() {
            return Err(bad());
        }
        Ok(Self {
            seq,
            via_key,
            identity,
            original,
            original_off,
            owner,
        })
    }

    fn render(&self) -> String {
        let mut line = self.seq.to_string();
        if self.via_key {
            line.push_str(" key");
        }
        if let Some(identity) = &self.identity {
            line.push_str(&format!(
                " identity {} {}",
                identity.subject, identity.digest
            ));
        }
        if let Some(original) = &self.original {
            line.push_str(&format!(
                " original {} {}",
                original.subject, original.digest
            ));
        }
        if let Some(off) = self.original_off {
            line.push_str(&format!(" original-off {off}"));
        }
        if let Some(owner) = self.owner {
            line.push_str(&format!(" owner {owner}"));
        }
        line
    }
}

/// A face a reservation may publish on: absolute, printable, no wildcard.
fn is_face(subject: &str) -> bool {
    subject.starts_with('/')
        && subject
            .bytes()
            .all(|b| b.is_ascii_graphic() && !matches!(b, b'*' | b'>'))
}

/// One record in flight onto this node's own lanes ([`StateDir::in_flight`]): its
/// reservation, face, exact bytes and the log it was decided on. A relaunched
/// bridge publishes it again at the same sequence, learning its offset from the
/// broker's dedup before the self-lane check judges it; the log says when it must
/// not.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct InFlight {
    pub pin: PublishPin,
    pub subject: String,
    pub body: Vec<u8>,
    /// The log this body was prepared on, the only one where a `re=` in it points;
    /// `None` for an entry written without a witness.
    pub log: Option<astream_broker::LogIdentity>,
}

impl InFlight {
    /// The pin, face, body in lower hex, then an optional `log=<identity>`.
    fn render(&self) -> String {
        let mut text = format!(
            "{}\n{}\n{}",
            self.pin.render(),
            self.subject,
            aterm_codec::hex::encode_lower(&self.body)
        );
        if let Some(log) = self.log {
            text.push_str(&format!("\nlog={log}"));
        }
        text
    }

    /// Parse the three-line form, or the four with `log=`; a `log=` line that does
    /// not parse is damage, never a missing witness.
    pub(crate) fn parse(seq: u64, text: &str) -> Option<Self> {
        let mut lines = text.lines();
        let pin = PublishPin::parse(lines.next()?).ok()?;
        let subject = lines.next()?;
        let body = aterm_codec::hex::decode_vec(lines.next()?.as_bytes())?;
        let log = match lines.next() {
            Some(line) => Some(astream_broker::LogIdentity::parse(
                line.strip_prefix("log=")?,
            )?),
            None => None,
        };
        (lines.next().is_none() && pin.seq == seq && is_face(subject)).then(|| Self {
            pin,
            subject: subject.to_string(),
            body,
            log,
        })
    }
}

/// The acked self-lane log as read back ([`StateDir::self_acked_log`]).
///
/// AN APPEND LOG, NOT A SET REWRITTEN PER PUBLISH. The file used to be the
/// whole set, replaced for every own-lane publish: at the bridge's steady
/// state that is thousands of offsets, tens of KiB written and renamed for
/// one record, between its durable ack and its delivery. Now each offset ends
/// at its comma, the newest appended last in a `write` of its own, and the
/// bridge compacts the log to the newest set when it reaches twice that size
/// ([`StateDir::set_self_acked`]). Repeats are harmless: it is read as a set.
#[derive(Debug, Default, PartialEq, Eq)]
pub(crate) struct AckedLog {
    /// Every offset the file holds whole.
    pub offsets: std::collections::BTreeSet<u64>,
    /// How many entries the file holds, repeats included: the size an append
    /// grows and a compaction resets.
    pub entries: usize,
    /// Whether the file ends at a terminator (or is absent), so an append may
    /// go on after its last byte. `false` for a torn tail and for the
    /// newline-ended line an older bridge wrote: the next write replaces the
    /// file whole.
    pub appendable: bool,
}

impl AckedLog {
    /// Read the log. Only a number ended by its comma is an offset — and,
    /// for a file an older bridge wrote, the last number of its one line,
    /// ended by the newline. Anything after the last terminator is a torn
    /// append and is NOT an offset of ours: a prefix of a number names some
    /// other record, and vouching for it would let a forgery through the
    /// self-lane check.
    fn parse(text: &str) -> Self {
        let mut log = Self {
            appendable: text.is_empty() || text.ends_with(','),
            ..Self::default()
        };
        let mut pieces: Vec<&str> = text.split(',').collect();
        let tail = pieces.pop().unwrap_or_default();
        for piece in pieces {
            log.entries += 1;
            if let Ok(off) = piece.trim().parse() {
                log.offsets.insert(off);
            }
        }
        if tail.ends_with('\n') {
            if let Ok(off) = tail.trim().parse() {
                log.entries += 1;
                log.offsets.insert(off);
            }
        }
        log
    }

    /// `offsets`, each ended by its comma: what an append writes, and what a
    /// compaction writes whole.
    fn render(offsets: impl Iterator<Item = u64>) -> String {
        let mut text = String::new();
        for off in offsets {
            text.push_str(&off.to_string());
            text.push(',');
        }
        text
    }
}

/// [`StateDir::lock_bridge`]'s hold, released by `LOCK_UN` before the close
/// (`clippy.toml`'s flock release rule): a child forked while it is held keeps the
/// descriptor until it execs, so a release by the close alone would leave the lock
/// taken on that copy.
pub struct BridgeLock(std::fs::File);

impl Drop for BridgeLock {
    fn drop(&mut self) {
        let _ = self.0.unlock();
    }
}

impl StateDir {
    /// Open (creating) a state directory.
    ///
    /// # Errors
    ///
    /// If the directory cannot be created.
    pub fn open(root: impl Into<PathBuf>) -> io::Result<Self> {
        let root = root.into();
        std::fs::create_dir_all(root.join("seen"))?;
        std::fs::create_dir_all(root.join("sent"))?;
        Ok(Self::at(root))
    }

    fn at(root: PathBuf) -> Self {
        Self {
            root,
            #[cfg(test)]
            ops: std::sync::Mutex::default(),
            #[cfg(test)]
            inc_seam: std::sync::Mutex::default(),
        }
    }

    /// TEST-ONLY: every filesystem operation this dir has counted so far.
    #[cfg(test)]
    pub(crate) fn ops(&self) -> FileOps {
        *self.ops.lock().unwrap_or_else(|e| e.into_inner())
    }

    /// TEST-ONLY: stand in the incarnation take's boot, kill point and defect.
    #[cfg(test)]
    pub(crate) fn set_inc_seam(&self, seam: IncSeam) {
        *self.inc_seam.lock().unwrap_or_else(|e| e.into_inner()) = seam;
    }

    /// TEST-ONLY: the incarnation take's stand-ins as they are now.
    #[cfg(test)]
    fn inc_seam(&self) -> IncSeam {
        *self.inc_seam.lock().unwrap_or_else(|e| e.into_inner())
    }

    /// Die here if a test armed this step ([`IncSeam::kill_at`]): the take stops with
    /// every file as the kernel has it, which is what a `kill -9` leaves. Nothing at
    /// all outside a test build.
    fn kill_point(&self, _step: IncStep) -> io::Result<()> {
        #[cfg(test)]
        if self.inc_seam().kill_at == Some(_step) {
            return Err(io::Error::other(format!("killed at {_step:?}")));
        }
        Ok(())
    }

    /// Count one operation ([`FileOps`]); nothing at all outside a test build.
    fn count(&self, _note: impl FnOnce(&mut FileOps)) {
        #[cfg(test)]
        _note(&mut self.ops.lock().unwrap_or_else(|e| e.into_inner()));
    }

    /// Unlink `name`, counted ([`FileOps::unlinks`]).
    fn unlink(&self, name: &str) -> io::Result<()> {
        self.count(|ops| ops.unlinks += 1);
        std::fs::remove_file(self.path(name))
    }

    /// A state directory to read, creating nothing: `aterm fabric`'s report must not
    /// make a dir that a command names and that does not exist. A missing file reads
    /// as "nothing known".
    #[must_use]
    pub fn peek(root: impl Into<PathBuf>) -> Self {
        Self::at(root.into())
    }

    fn path(&self, name: &str) -> PathBuf {
        self.root.join(name)
    }

    /// Read one file's trimmed contents.
    fn read(&self, name: &str) -> Option<String> {
        std::fs::read_to_string(self.path(name))
            .ok()
            .map(|s| s.trim().to_string())
            .filter(|s| !s.is_empty())
    }

    fn read_reservation(&self, name: &str) -> io::Result<Option<String>> {
        self.count(|ops| ops.reads += 1);
        match std::fs::read_to_string(self.path(name)) {
            Ok(text) => Ok(Some(text)),
            Err(e) if e.kind() == io::ErrorKind::NotFound => {
                match std::fs::symlink_metadata(self.path(name)) {
                    Err(missing) if missing.kind() == io::ErrorKind::NotFound => Ok(None),
                    Err(other) => Err(other),
                    Ok(_) => Err(e),
                }
            }
            Err(e) => Err(e),
        }
    }

    /// THE BUS MIRROR'S PIN (`logpin`, session log phase 4,
    /// [`crate::mirror::Pin`]): the one batch in flight and the node's total
    /// of mirrored bytes. Absent on a node that never mirrored; a bridge with
    /// `[fabric] log = "off"` never reads or writes it. Read with the
    /// missing-versus-damaged rule every reservation is.
    ///
    /// # Errors
    ///
    /// A pin that exists and cannot be read.
    pub(crate) fn logpin(&self) -> io::Result<Option<String>> {
        self.read_reservation("logpin")
    }

    /// Replace `logpin` whole, atomically, 0600 (it holds record bytes, as
    /// `inflight/` does). Not flushed: a power loss ends what it pairs with
    /// (the endpoint's rings), and losing it costs at most one batch taken
    /// again at fresh sequences — a second copy each reader dedupes.
    ///
    /// # Errors
    ///
    /// The write's.
    pub(crate) fn set_logpin(&self, text: &str) -> io::Result<()> {
        self.replace_bytes("logpin", text.as_bytes(), false, 0o600)
    }

    pub(crate) fn post_pin(&self, sid: &str, id: u64) -> io::Result<Option<PublishPin>> {
        self.read_reservation(&format!("sent/{sid}.{id}"))?
            .map(|s| PublishPin::parse(&s))
            .transpose()
    }

    pub(crate) fn set_post_pin(&self, sid: &str, id: u64, pin: &PublishPin) -> io::Result<()> {
        self.write(&format!("sent/{sid}.{id}"), &pin.render())
    }

    pub(crate) fn receipt_pin(&self, sid: &str, rid: u64) -> io::Result<Option<PublishPin>> {
        self.read_reservation(&format!("acks/{sid}.{rid}"))?
            .map(|s| PublishPin::parse(&s))
            .transpose()
    }

    pub(crate) fn set_receipt_pin(&self, sid: &str, rid: u64, pin: &PublishPin) -> io::Result<()> {
        self.write(&format!("acks/{sid}.{rid}"), &pin.render())
    }

    pub(crate) fn key_pin(&self, sid: &str, key: &str) -> io::Result<Option<PublishPin>> {
        let text = self
            .read_reservation(&format!("keys/{sid}"))?
            .unwrap_or_default();
        text.lines()
            .find(|line| line.split_whitespace().next() == Some(key))
            .map(|line| {
                let rest = line.strip_prefix(key).unwrap_or_default().trim();
                PublishPin::parse(rest)
            })
            .transpose()
    }

    pub(crate) fn set_key_pin(
        &self,
        sid: &str,
        key: &str,
        pin: &PublishPin,
    ) -> io::Result<PublishPin> {
        if let Some(have) = self.key_pin(sid, key)? {
            return Ok(have);
        }
        let text = self
            .read_reservation(&format!("keys/{sid}"))?
            .unwrap_or_default();
        let mut lines: Vec<String> = text.lines().map(str::to_string).collect();
        lines.push(format!("{key} {}", pin.render()));
        let over = lines.len().saturating_sub(KEYS_KEEP);
        self.write(&format!("keys/{sid}"), &lines[over..].join("\n"))?;
        Ok(pin.clone())
    }

    /// Reconcile a retained key to the bytes actually found/appended, before
    /// retiring the post. Another reservation or an evicted key is left alone.
    pub(crate) fn reconcile_key(
        &self,
        sid: &str,
        key: &str,
        seq: u64,
        identity: &PublishIdentity,
        off: u64,
        owner: Option<u64>,
    ) -> io::Result<()> {
        let have = match self.key_pin(sid, key)? {
            Some(have) if have.seq == seq => have,
            _ => return Ok(()),
        };
        let owner = owner.or(have.owner);
        if have.identity.as_ref() == Some(identity)
            && have.original_off == Some(off)
            && have.owner == owner
        {
            return Ok(());
        }
        let next = PublishPin {
            seq,
            via_key: false,
            identity: Some(identity.clone()),
            original: None,
            original_off: Some(off),
            owner,
        };
        let text = self
            .read_reservation(&format!("keys/{sid}"))?
            .unwrap_or_default();
        let lines: Vec<String> = text
            .lines()
            .map(|line| {
                if line.split_whitespace().next() == Some(key) {
                    format!("{key} {}", next.render())
                } else {
                    line.to_string()
                }
            })
            .collect();
        self.write(&format!("keys/{sid}"), &lines.join("\n"))
    }

    /// The `keys/` and `sent/` rewrites a log switch needs: each reservation keeps
    /// its ownership and identities and loses only `original-off`, an offset on the
    /// old log — on a log that continues the old one below `keep_below`
    /// ([`StateDir::forget_log_offsets`]), only one at or above it. Reads every
    /// reservation and writes none; the caller writes them before the witness
    /// advances, and a partial rewrite is safe to repeat.
    fn publication_offset_resets(
        &self,
        keep_below: Option<u64>,
    ) -> io::Result<Vec<(String, String)>> {
        // Take `original-off` when it names a record this log does not keep.
        let reset = |pin: &mut PublishPin| -> bool {
            let stale = pin
                .original_off
                .is_some_and(|off| keep_below.is_none_or(|f| off >= f));
            stale && pin.original_off.take().is_some()
        };
        let mut rewrites = Vec::new();
        for directory in ["keys", "sent"] {
            let path = self.path(directory);
            let entries = match std::fs::read_dir(&path) {
                Ok(entries) => entries,
                Err(e)
                    if e.kind() == io::ErrorKind::NotFound
                        && std::fs::symlink_metadata(&path)
                            .is_err_and(|e| e.kind() == io::ErrorKind::NotFound) =>
                {
                    continue;
                }
                Err(e) => return Err(e),
            };
            for entry in entries {
                let entry = entry?;
                let name = entry.file_name();
                let name = name.to_str().ok_or_else(|| {
                    io::Error::new(io::ErrorKind::InvalidData, "invalid publication filename")
                })?;
                // A crash can leave an atomic write's hidden temp; it is not
                // the committed reservation at this name.
                if name.starts_with('.') {
                    continue;
                }
                if !entry.file_type()?.is_file() {
                    return Err(io::Error::new(
                        io::ErrorKind::InvalidData,
                        "publication reservation is not a regular file",
                    ));
                }
                let text = std::fs::read_to_string(entry.path())?;
                let next = if directory == "keys" {
                    let mut changed = false;
                    let mut lines = Vec::new();
                    for line in text.lines() {
                        if line.trim().is_empty() {
                            lines.push(line.to_string());
                            continue;
                        }
                        let (key, reservation) = line
                            .split_once(char::is_whitespace)
                            .filter(|(key, _)| !key.is_empty())
                            .ok_or_else(|| {
                                io::Error::new(
                                    io::ErrorKind::InvalidData,
                                    "invalid publication key",
                                )
                            })?;
                        let mut pin = PublishPin::parse(reservation)?;
                        if reset(&mut pin) {
                            changed = true;
                            lines.push(format!("{key} {}", pin.render()));
                        } else {
                            lines.push(line.to_string());
                        }
                    }
                    changed.then(|| lines.join("\n"))
                } else {
                    let mut pin = PublishPin::parse(&text)?;
                    reset(&mut pin).then(|| pin.render())
                };
                if let Some(next) = next {
                    rewrites.push((format!("{directory}/{name}"), next));
                }
            }
        }
        Ok(rewrites)
    }

    /// Replace one file atomically: temp, rename, so a reader sees the old value or
    /// the new one, never a truncation, and a `kill -9` at any point leaves one of
    /// the two. Nothing is flushed; the module doc says which files must survive a
    /// power loss ([`StateDir::write_durable`]) and why the rest need not.
    ///
    /// The temp is this write's own fresh `O_EXCL` name (`create_temp_beside`), never
    /// a shared `<name>.tmp` that a second writer (a CLI beside the bridge, `fabric
    /// on`) could truncate mid-write and rename over the target. Nothing planted at
    /// the name is written through.
    ///
    /// # Errors
    ///
    /// Any I/O failure; a bridge that cannot persist a sequence reservation must not
    /// publish.
    fn write(&self, name: &str, value: &str) -> io::Result<()> {
        self.replace(name, value, false, 0o666)
    }

    /// [`StateDir::write`], made durable across a power loss or an OS crash: the temp
    /// is flushed before the rename and the directory after it, as astream's log does
    /// (`store.rs`'s `sync_parent_dir`; a flushed file whose entry is not may be gone
    /// after the boot). Several milliseconds on macOS (`F_FULLFSYNC`), so only for
    /// the files the module doc lists as durable, never one written per message.
    ///
    /// # Errors
    ///
    /// [`StateDir::write`]'s, and a directory flush failure [`sync_dir`] does not
    /// tolerate.
    fn write_durable(&self, name: &str, value: &str) -> io::Result<()> {
        self.replace(name, value, true, 0o666)
    }

    /// The one replace behind [`StateDir::write`] and [`StateDir::write_durable`]:
    /// `value` and its newline. `mode` is subject to the umask: 0o666
    /// (`File::create`'s) for every file but the ones holding a record's bytes,
    /// 0o600 ([`StateDir::set_in_flight`]).
    fn replace(&self, name: &str, value: &str, durable: bool, mode: u32) -> io::Result<()> {
        let mut bytes = Vec::with_capacity(value.len() + 1);
        bytes.extend_from_slice(value.as_bytes());
        bytes.push(b'\n');
        self.replace_bytes(name, &bytes, durable, mode)
    }

    /// [`StateDir::replace`] of exactly `bytes`, in ONE `write`: the temp is this
    /// write's own, so nothing can read it half written, and a second `write`
    /// for the newline was one more system call on every replace.
    fn replace_bytes(&self, name: &str, bytes: &[u8], durable: bool, mode: u32) -> io::Result<()> {
        let target = self.path(name);
        let parent = target.parent().unwrap_or(&self.root);
        std::fs::create_dir_all(parent)?;
        let (tmp, mut f) = crate::enable::create_temp_beside(&target, mode)?;
        self.count(|ops| {
            ops.replaces += 1;
            ops.renames += 1;
            ops.fsyncs += 2 * u64::from(durable);
        });
        let flush = durable && self.flushes();
        let staged = (|| {
            f.write_all(bytes)?;
            if flush {
                #[expect(
                    clippy::disallowed_methods,
                    reason = "aterm-link's flushes are the Ordered-tier work's to classify; the full flush is kept until then"
                )]
                f.sync_all()?;
            }
            drop(f);
            std::fs::rename(&tmp, &target)
        })();
        if staged.is_err() {
            let _ = std::fs::remove_file(&tmp);
            return staged;
        }
        if durable {
            self.kill_point(IncStep::Renamed)?;
            if flush {
                sync_dir(parent)?;
            }
        }
        Ok(())
    }

    /// Whether a durable write really flushes: always, but in the incarnation lease's
    /// walk ([`IncSeam::simulated_media`]), which stands in the media itself and reads
    /// nothing a flush would change — so thousands of takes run at file speed, not at
    /// `F_FULLFSYNC`'s. The flushes are still counted ([`FileOps::fsyncs`]).
    fn flushes(&self) -> bool {
        #[cfg(test)]
        if self.inc_seam().simulated_media {
            return false;
        }
        true
    }

    /// Take this state dir for one bridge's life: an exclusive `flock` on
    /// `<root>/lock`, released when the [`BridgeLock`] drops or the process dies.
    ///
    /// [`StateDir::open`] does not take it: `fabric on` (provisioning the node id)
    /// and `aterm fabric` (reading) run beside a bridge and never publish as the
    /// node. The lock orders the writers that do: a bridge and the doctor's
    /// `publish_exited`.
    ///
    /// Opened with `enable::LOCK_OPEN_FLAGS`, so a link planted at the name is
    /// refused and a FIFO is not waited on. A filesystem without advisory locking
    /// (`Unsupported`) runs unguarded rather than not at all, as astream's own log
    /// lock does.
    ///
    /// # Errors
    ///
    /// [`io::ErrorKind::WouldBlock`] when another process holds it (the message names
    /// its pid when the `pid` file does); otherwise the open's or the lock's error.
    pub fn lock_bridge(&self) -> io::Result<BridgeLock> {
        use std::os::unix::fs::OpenOptionsExt;
        let path = self.path("lock");
        let file = std::fs::OpenOptions::new()
            .read(true)
            .write(true)
            .create(true)
            .truncate(false)
            .mode(0o600)
            .custom_flags(crate::enable::LOCK_OPEN_FLAGS)
            .open(&path)?;
        if !file.metadata()?.is_file() {
            return Err(io::Error::new(
                io::ErrorKind::InvalidData,
                format!("the bridge lock {} is not a regular file", path.display()),
            ));
        }
        #[expect(
            clippy::disallowed_methods,
            reason = "the guard's own take: BridgeLock's Drop releases by LOCK_UN"
        )]
        let taken = file.try_lock();
        match taken {
            Ok(()) => Ok(BridgeLock(file)),
            Err(std::fs::TryLockError::Error(e)) if e.kind() == io::ErrorKind::Unsupported => {
                Ok(BridgeLock(file))
            }
            Err(std::fs::TryLockError::Error(e)) => Err(e),
            Err(std::fs::TryLockError::WouldBlock) => Err(io::Error::new(
                io::ErrorKind::WouldBlock,
                format!(
                    "the state dir {} is held by another bridge{}",
                    self.root.display(),
                    self.holder()
                        .map_or_else(String::new, |pid| format!(" (pid {pid})"))
                ),
            )),
        }
    }

    /// The pid the `pid` file names: the holder of [`StateDir::lock_bridge`], which
    /// writes it once it holds the lock. For messages only; the lock is the fact.
    #[must_use]
    pub fn holder(&self) -> Option<u32> {
        self.read("pid").and_then(|s| s.parse().ok())
    }

    /// This node's id, minted once and kept: the id is the mail address, so a new one
    /// would orphan the node's inbox lane.
    ///
    /// Durable, and so is the state dir's entry in its parent: the mint is usually the
    /// dir's first write, and an id flushed into a directory a power loss takes back
    /// leaves the node with no mail lane and no caps.
    ///
    /// # Errors
    ///
    /// If a freshly minted id cannot be persisted: the bridge refuses to start rather
    /// than run under an id that will not survive a restart.
    pub fn node_id(&self, mint: impl FnOnce() -> String) -> io::Result<String> {
        if let Some(id) = self.node() {
            return Ok(id);
        }
        let id = mint();
        self.write_durable("node", &id)?;
        sync_dir(self.root.parent().unwrap_or(Path::new("")))?;
        Ok(id)
    }

    /// The node id this dir records, if it records one — read, never minted.
    #[must_use]
    pub fn node(&self) -> Option<String> {
        self.read("node")
    }

    /// The incarnation this node last used, as far as the local disk knows — for the
    /// doctor's `publish_exited`, which publishes INSIDE it rather than rolling a new
    /// one: the one `inc-lease` says the last attach took, or `seq`'s incarnation where
    /// that is higher. A bridge reserves its first block under its incarnation after
    /// the take, so `seq`'s is never above the one taken, and it is what answers for an
    /// older build's dir, which keeps no lease (its attach reserved under the
    /// incarnation `inc` held); 0 where neither file says anything.
    ///
    /// NEVER WHAT `inc` HOLDS, which is the top of a lease: a doctor that published
    /// under it would raise the node's high water past the will of the incarnation
    /// really in use, and fence it — the dead node's row would stay `live`, and the next
    /// attach would never witness that incarnation dead. Not even with no whole lease
    /// left: a power loss can take the dir's FIRST lease, which has no older version on
    /// the media to fall back on, and `inc` alone cannot tell a lease's top from an
    /// older build's exact incarnation. So every answer is at or below the incarnation
    /// used, read from any boot's files: a stale lease, a rolled-back `seq` or nothing
    /// at all names one at or below it, where the doctor's sequences either dedup (it
    /// refuses, fail-closed, as after any rolled-back `seq`) or are fresh numbers below
    /// that incarnation's will, which fence nothing.
    #[must_use]
    pub fn incarnation(&self) -> u64 {
        let used = self.lease().map_or(0, |lease| lease.used);
        used.max(self.sequence() >> 32)
    }

    /// What `inc` holds: the top of the incarnation lease, at or above every
    /// incarnation this node has presented to a broker (the module doc's "The
    /// incarnation lease").
    fn incarnation_top(&self) -> u64 {
        self.read("inc").and_then(|s| s.parse().ok()).unwrap_or(0)
    }

    /// What `inc-lease` holds, if it holds a whole one ([`IncLease::parse`]).
    pub(crate) fn lease(&self) -> Option<IncLease> {
        self.read("inc-lease").and_then(|s| IncLease::parse(&s))
    }

    /// Which `inc` this dir holds now ([`IncFile`]): `None` where it holds none, where
    /// `inc` is not a regular file of the dir's own device (a link planted at the name
    /// is another file), or where the platform names no inode — and then no lease is
    /// trusted.
    #[cfg(unix)]
    pub(crate) fn inc_file(&self) -> Option<IncFile> {
        use std::os::unix::fs::MetadataExt;
        let dir = std::fs::metadata(&self.root).ok()?;
        let inc = std::fs::symlink_metadata(self.path("inc")).ok()?;
        (inc.is_file() && inc.dev() == dir.dev()).then(|| IncFile {
            dev: dir.dev(),
            dir: dir.ino(),
            ino: inc.ino(),
        })
    }

    /// No inode to name: never a trusted lease (nor a boot to trust it in:
    /// [`on_boot_volume`]).
    #[cfg(not(unix))]
    pub(crate) fn inc_file(&self) -> Option<IncFile> {
        None
    }

    /// This boot, when the page cache can vouch for this state dir: the kernel names
    /// it and the dir is on the boot volume ([`on_boot_volume`]). `None` otherwise,
    /// which trusts no lease. The boot is the broker's own
    /// ([`crate::durability::this_boot`]), so the lease and the log's unclean verdict
    /// never judge the page cache by different boots.
    pub(crate) fn trusted_boot(&self) -> Option<[u8; 16]> {
        #[cfg(test)]
        if let Some(boot) = self.inc_seam().boot {
            return boot;
        }
        crate::durability::this_boot().filter(|_| on_boot_volume(&self.root))
    }

    /// TAKE THE INCARNATION AN ATTACH RUNS UNDER, before its will is registered: one
    /// past what the disk says this node has taken and past `bus`, the newest `inc=`
    /// on the node's own presence face, and covered by a FLUSHED `inc` before it is
    /// answered (the module doc's "The incarnation lease": [`plan_incarnation`]
    /// decides, and this writes).
    ///
    /// THE ORDER IS THE ARGUMENT. `inc`, when the lease must grow, is flushed first;
    /// `inc-lease`, which names the new top, the file it was flushed into and the
    /// incarnation taken, is written only after that flush returned; the incarnation
    /// is answered only after both. So `inc-lease` never names a top the media may not
    /// hold, and nothing of an incarnation reaches a broker before a flushed `inc`
    /// covers it. In the boot that wrote it, a lease with room answers with one
    /// unflushed write (the attach's steady state); in any other, or in a copy of the
    /// dir, `inc` alone is trusted and the lease starts again above it.
    ///
    /// A read-plan-write of two files, so only the holder of
    /// [`StateDir::lock_bridge`] calls it.
    ///
    /// # Errors
    ///
    /// The incarnation space is exhausted (a corrupt `inc` or `seq`, or a bus row at
    /// [`INC_MAX`]: a named refusal, never a wrapped incarnation), or a write failed —
    /// and then the attach must not register a will.
    pub fn take_incarnation(&self, bus: u64) -> io::Result<u64> {
        let (top, file) = (self.incarnation_top(), self.inc_file());
        let boot = self.trusted_boot();
        let (lease, seq_inc, span) = (self.lease(), self.sequence() >> 32, INC_LEASE);
        #[cfg(test)]
        let (lease, span) = {
            let seam = self.inc_seam();
            let lease = match seam.mutant {
                1 => lease.map(|l| IncLease { boot, ..l }),
                2 => lease.map(|l| IncLease { top, ..l }),
                _ => lease,
            };
            (lease, seam.span.unwrap_or(span))
        };
        let plan =
            plan_incarnation(top, file, lease, seq_inc, boot, bus, span).ok_or_else(|| {
                io::Error::other(format!(
                    "this node's incarnation space is exhausted (inc {} of {INC_MAX})",
                    top.max(seq_inc).max(bus)
                ))
            })?;
        let leased = |file| IncLease {
            used: plan.next,
            top: plan.top,
            boot,
            file,
        };
        #[cfg(test)]
        if plan.flush && self.inc_seam().mutant == 3 {
            self.write("inc-lease", &leased(file).render())?;
        }
        // A flushed write renames a NEW file in: the lease names that one, read back
        // after the flush returned, so a copy of it is never the file a lease vouches
        // for. A lease with room keeps the file it was trusted on, which is `file`.
        let file = if plan.flush {
            self.write_durable("inc", &plan.top.to_string())?;
            self.kill_point(IncStep::Flushed)?;
            self.inc_file()
        } else {
            file
        };
        self.write("inc-lease", &leased(file).render())?;
        Ok(plan.next)
    }

    /// The highest producer sequence this bridge has reserved (not necessarily
    /// published) — the LAST number of the newest block
    /// ([`StateDir::reserve_sequence_block`]). It is written before any number of the
    /// block is used, so a crash burns the block's unused rest rather than risk a
    /// duplicate (the trade `asb pub --seq-file` makes).
    #[must_use]
    pub fn sequence(&self) -> u64 {
        self.read("seq").and_then(|s| s.parse().ok()).unwrap_or(0)
    }

    /// Reserve the next producer sequence of incarnation `inc` and persist it before
    /// it is used: a block of one ([`StateDir::reserve_sequence_block`]), for the
    /// doctor's `publish_exited`, which publishes a handful of rows and holds the
    /// bridge's lock while it does.
    ///
    /// # Errors
    ///
    /// The incarnation's space is exhausted or the write failed; the caller must not
    /// publish either way.
    pub fn reserve_next_sequence(&self, inc: u64) -> io::Result<u64> {
        self.reserve_sequence_block(inc, 1)
            .map(|block| *block.start())
    }

    /// Reserve a BLOCK of up to `want` producer sequences of incarnation `inc` —
    /// the numbers just above both the file and `inc << 32` — and persist the
    /// block's LAST number before answering it: the one reservation, for the
    /// bridge (which hands the block out from memory, `Bridge::next_seq`) and the
    /// doctor's `publish_exited` alike.
    ///
    /// NEVER A NUMBER TWICE, and the argument is the file alone. The file only
    /// ever grows — every call writes a number above what it read — and it holds
    /// the last number of every block handed out, written (temp + rename) before
    /// the block's first number reaches anybody. So every number ever handed out
    /// is at or below the file, and the next call, in this process or the next,
    /// starts above it. A `kill -9` part-way through a block skips the block's
    /// unused rest — a burned number, which costs nothing — and reuses none. A
    /// power loss that rolls the file back is fenced by the DURABLE `inc`, as it
    /// always was (the module doc): the next attach takes `inc + 1`, whose every
    /// number is above anything a rolled-back file could name.
    ///
    /// Clamped below the incarnation's reserved top ([`WILL_SEQ_LOW`], the will's
    /// own number), so a block near the top is shorter rather than refused; one
    /// that would start at the top is refused, as is an incarnation past
    /// [`INC_MAX`]. A read-add-write of one file, so only the holder of
    /// [`StateDir::lock_bridge`] calls it.
    ///
    /// # Errors
    ///
    /// The incarnation's space is exhausted or the write failed; the caller must not
    /// publish either way.
    pub fn reserve_sequence_block(
        &self,
        inc: u64,
        want: u64,
    ) -> io::Result<std::ops::RangeInclusive<u64>> {
        if inc > INC_MAX {
            return Err(io::Error::other(format!(
                "incarnation {inc} is past this node's incarnation space ({INC_MAX})"
            )));
        }
        let base = inc << 32;
        let top = base | WILL_SEQ_LOW;
        let first = self
            .sequence()
            .max(base)
            .checked_add(1)
            .filter(|n| *n < top)
            .ok_or_else(|| io::Error::other("this incarnation's sequence space is exhausted"))?;
        let last = first.saturating_add(want.max(1) - 1).min(top - 1);
        self.write("seq", &last.to_string())?;
        Ok(first..=last)
    }

    /// Where one session's addressed refill starts after: the `floor=` of the
    /// `EVENT <local> inbox-seen` digest, minus one. The floor is the lowest offset of
    /// an addressed row the agent has not handled, else one past the highest
    /// delivered, so an acknowledged broadcast never moves it. The refill (§6.2)
    /// starts at `+1`.
    ///
    /// One file per session ever delivered to, and no sweep removes them: "departed"
    /// is not knowable here (a seamless update's sid is off aterm's roster between
    /// the re-exec and adoption), and a sweep in that window would refill a live
    /// session from `REFILL_FLOOR_SPAN`, re-offering thousands of old offsets.
    #[must_use]
    pub fn seen_off(&self, sid: &str) -> Option<u64> {
        self.read(&format!("seen/{sid}"))
            .and_then(|s| s.parse().ok())
    }

    /// Record a session's refill watermark ([`Self::seen_off`]). Monotone: moving it
    /// back would re-deliver rows the agent already handled.
    ///
    /// # Errors
    ///
    /// Any I/O failure.
    pub fn set_seen_off(&self, sid: &str, off: u64) -> io::Result<()> {
        if self.seen_off(sid).is_some_and(|have| have >= off) {
            return Ok(());
        }
        self.write(&format!("seen/{sid}"), &off.to_string())
    }

    /// Every offset this bridge holds a `PublishAck` for on its own lanes.
    ///
    /// For the self-lane check (§6.2): a record on this node's lane, under its
    /// `<src>`, at an offset not here is a forgery by a co-holder of the node cap. A
    /// set, not a high-water mark: other producers' offsets interleave with ours, and
    /// a watermark would vouch for every record below it.
    ///
    /// Every offset the file holds, older ones included: the bridge keeps the
    /// newest `keep` of them ([`AckedLog`]).
    #[must_use]
    pub fn self_acked(&self) -> std::collections::BTreeSet<u64> {
        self.self_acked_log().offsets
    }

    /// The `acked` file as it stands: its offsets, how many entries it holds, and
    /// whether an append may go on after its last byte ([`AckedLog`]).
    ///
    /// A missing file is the empty log, appendable. A file this process cannot
    /// read is the empty set too — the self-lane check gets more conservative,
    /// never less — and NOT appendable, so the next write replaces it whole.
    pub(crate) fn self_acked_log(&self) -> AckedLog {
        match std::fs::read_to_string(self.path("acked")) {
            Ok(text) => AckedLog::parse(&text),
            Err(e) if e.kind() == io::ErrorKind::NotFound => AckedLog {
                appendable: true,
                ..AckedLog::default()
            },
            Err(_) => AckedLog::default(),
        }
    }

    /// Replace the acked self-lane set with the newest `keep` of `set`: the
    /// COMPACTION of the append log ([`StateDir::append_self_acked`]), and the
    /// write that follows any failed or torn one. The bridge's in-memory copy is
    /// the set, so it is written without reading the file back.
    ///
    /// Each offset ends at its comma and nothing follows the last: an append then
    /// goes on after the last byte, and an older bridge's reader (`split(',')`
    /// after a trim) reads the same set.
    ///
    /// # Errors
    ///
    /// Any I/O failure.
    pub fn set_self_acked(
        &self,
        set: &std::collections::BTreeSet<u64>,
        keep: usize,
    ) -> io::Result<()> {
        let over = set.len().saturating_sub(keep);
        self.replace_bytes(
            "acked",
            AckedLog::render(set.iter().skip(over).copied()).as_bytes(),
            false,
            0o666,
        )
    }

    /// Append `off` to the acked self-lane log, ended by its comma, in ONE
    /// `write` to a file opened `O_APPEND`: a `kill -9` lands all of the write
    /// or none of it, so the file always ends at a terminator. What a power
    /// loss or a full disk can tear is the tail, and a number without its comma
    /// is never read as an offset ([`AckedLog::parse`]); a failed append is
    /// followed by a whole replace ([`StateDir::set_self_acked`]), never by
    /// another append onto the fragment, which could join it to a new number.
    ///
    /// Opened with `enable::LOCK_OPEN_FLAGS`: a link planted at the name is
    /// refused, not written through, and a FIFO is not waited on. The opened
    /// handle must name a regular file owned by this uid with one link before
    /// any append; refused occupants are neither written nor removed.
    ///
    /// # Errors
    ///
    /// Any I/O failure: the caller keeps the offset's in-flight entry and
    /// writes the set whole next time.
    pub(crate) fn append_self_acked(&self, off: u64) -> io::Result<()> {
        use std::os::unix::fs::{MetadataExt, OpenOptionsExt};
        self.count(|ops| ops.appends += 1);
        let mut file = std::fs::OpenOptions::new()
            .append(true)
            .create(true)
            .mode(0o666)
            .custom_flags(crate::enable::LOCK_OPEN_FLAGS)
            .open(self.path("acked"))?;
        let metadata = file.metadata()?;
        if !metadata.is_file()
            || metadata.uid() != aterm_uds::peer::our_uid()
            || metadata.nlink() != 1
        {
            return Err(io::Error::new(
                io::ErrorKind::InvalidData,
                "the acked log is not an owned regular single-link file",
            ));
        }
        file.write_all(AckedLog::render(std::iter::once(off)).as_bytes())
    }

    /// Every record in flight onto this node's own lanes, by sequence: written before
    /// its publish ([`StateDir::set_in_flight`]) and cleared once `acked` holds its
    /// offset, so its offset may be unknown (a kill between the `PublishAck` and the
    /// `acked` write, or before any answer). The attach publishes each again at its
    /// sequence before anything is judged, and the broker's dedup hands back the
    /// offset.
    ///
    /// A missing dir is nothing pending. Any listing, read or parse failure leaves a
    /// publication's fate unknown and stops the attach before any own row is judged;
    /// nothing is discarded, so a repaired journal is retried at the same sequence.
    ///
    /// # Errors
    ///
    /// Any directory or entry I/O error, or a malformed numeric entry.
    pub(crate) fn in_flight(&self) -> io::Result<Vec<(u64, InFlight)>> {
        let dir = self.path("inflight");
        let entries = match std::fs::read_dir(&dir) {
            Ok(entries) => entries,
            Err(e)
                if e.kind() == io::ErrorKind::NotFound
                    && std::fs::symlink_metadata(&dir)
                        .is_err_and(|e| e.kind() == io::ErrorKind::NotFound) =>
            {
                return Ok(Vec::new());
            }
            Err(e) => return Err(e),
        };
        let mut out = Vec::new();
        for entry in entries {
            let entry = entry?;
            // Digits only: a writer's temp (`.<seq>.tmp.…`) is not an entry.
            let Some(name) = entry.file_name().to_str().map(str::to_string) else {
                continue;
            };
            if name.is_empty() || !name.bytes().all(|b| b.is_ascii_digit()) {
                continue;
            }
            let bad = || {
                io::Error::new(
                    io::ErrorKind::InvalidData,
                    format!("{}: malformed in-flight entry", entry.path().display()),
                )
            };
            let seq = name.parse().map_err(|_| bad())?;
            let text = self
                .read_reservation(&format!("inflight/{name}"))?
                .ok_or_else(|| {
                    io::Error::new(
                        io::ErrorKind::NotFound,
                        format!(
                            "{}: in-flight entry disappeared during recovery",
                            entry.path().display()
                        ),
                    )
                })?;
            let record = InFlight::parse(seq, &text).ok_or_else(bad)?;
            out.push((seq, record));
        }
        out.sort_by_key(|(seq, _)| *seq);
        Ok(out)
    }

    /// Record one own-lane record as in flight, before it is published.
    ///
    /// Mode 0o600: this is the one file here that holds a record's body (the
    /// reservations hold only its identity), from the publish to the `acked` write,
    /// or until the next attach when the publish's fate is unknown.
    ///
    /// # Errors
    ///
    /// Any I/O failure; the caller must not publish then.
    pub(crate) fn set_in_flight(&self, entry: &InFlight) -> io::Result<()> {
        self.replace(
            &format!("inflight/{}", entry.pin.seq),
            &entry.render(),
            false,
            0o600,
        )
    }

    /// Forget the in-flight record at `seq`: it landed and `acked` holds its offset,
    /// or it provably did not land.
    pub(crate) fn clear_in_flight(&self, seq: u64) {
        let _ = self.unlink(&format!("inflight/{seq}"));
    }

    /// The highest fleet-halt offset this node has published an `ack` for, so a
    /// reconnect does not re-answer the halt row its `Last` read returns again, nor a
    /// row that arrives both on a `Last` page and on the fleet tail. `None` is
    /// "answered nothing": it re-acks rather than skip a barrier.
    #[must_use]
    pub fn halt_acked(&self) -> Option<u64> {
        self.read("halt-acked").and_then(|s| s.parse().ok())
    }

    /// Record the halt offset this node has answered. Monotone, like `seen`: an
    /// out-of-order replay must not make the node re-answer the history.
    ///
    /// # Errors
    ///
    /// Any I/O failure.
    pub fn set_halt_acked(&self, off: u64) -> io::Result<()> {
        if self.halt_acked().is_some_and(|have| have >= off) {
            return Ok(());
        }
        self.write("halt-acked", &off.to_string())
    }

    /// The inbox-group commit a bridge OWED when it exited with no broker
    /// connection to make it on (`Bridge::hand_over_inbox_commit`): every
    /// record at or below it was accounted for — delivered, or refused with
    /// its verdict answered on the bus — so the next bridge commits it before
    /// its group subscription reads the cursor, rather than have the group offer
    /// a burst again to an endpoint that may no longer remember taking it.
    /// `None`: nothing was handed over.
    #[must_use]
    pub fn inbox_owed(&self) -> Option<u64> {
        self.read("inbox-owed").and_then(|s| s.parse().ok())
    }

    /// Hand over the inbox-group commit through `off` ([`Self::inbox_owed`]).
    /// Monotone: the group commit itself never moves back, and a lower offset
    /// handed over after a higher one names records already covered.
    ///
    /// # Errors
    ///
    /// Any I/O failure.
    pub fn set_inbox_owed(&self, off: u64) -> io::Result<()> {
        if self.inbox_owed().is_some_and(|have| have >= off) {
            return Ok(());
        }
        self.write("inbox-owed", &off.to_string())
    }

    /// The inbox commit handed over has been made: forget it.
    ///
    /// # Errors
    ///
    /// Any I/O failure other than the file already being gone.
    pub fn clear_inbox_owed(&self) -> io::Result<()> {
        match std::fs::remove_file(self.path("inbox-owed")) {
            Err(e) if e.kind() != io::ErrorKind::NotFound => Err(e),
            _ => Ok(()),
        }
    }

    /// Forget the halt watermark: it names an offset on a log this node no
    /// longer reads ([`LogWitness::replaced_by`]), and on the log it does, a
    /// halt at or below that number would never be answered.
    ///
    /// # Errors
    ///
    /// Any I/O failure other than the file already being gone.
    pub fn clear_halt_acked(&self) -> io::Result<()> {
        match std::fs::remove_file(self.path("halt-acked")) {
            Err(e) if e.kind() != io::ErrorKind::NotFound => Err(e),
            _ => Ok(()),
        }
    }

    /// Forget every offset this dir holds that the log this node reads now does
    /// not vouch for ([`LogWitness::replaced_by`]), and put each back where a
    /// reader of this log, whose head is `head`, resumes.
    ///
    /// `keep_below` is where the old log CONTINUES on this one
    /// ([`LogWitness::continues`]): `Some(f)` — a moved generation of the same
    /// log, every offset below `f` naming the same record on both — CLAMPS
    /// instead of forgetting: what names a record below `f` is kept, what names
    /// one at or above it (a record the loss took, whose offset this log now
    /// gives to another) goes, and every cursor resumes at the lower of itself
    /// and `f`, so nothing posted after the fork is skipped and nothing below it
    /// re-read. `None` — another log — forgets every offset:
    ///
    /// * `seen/*`: forgotten, so a refill starts at its bounded floor below this
    ///   log's head, not past it, where the old watermark would answer "nothing to
    ///   refill" over rows the endpoint lost. Clamped: one at or above `f` falls
    ///   to `f - 1`, so the refill starts at the fork — the new records there are
    ///   delivered, the ones below it are not offered again;
    /// * every `topics/*` cursor resumes at `head`, as a `since=head` opt-in made
    ///   now does, not at an old offset that would skip this log's broadcasts or
    ///   replay a longer log's history as new mail. Clamped: at `min(next, f)`;
    /// * `unhandled/*`: forgotten: a refill floor names a record of the old log,
    ///   and rewinding a cursor to it would offer this log's history as new mail.
    ///   Clamped: only the floors at or above `f` go;
    /// * `halt-acked` goes ALWAYS, clamped or not, so a halt here is answered,
    ///   never skipped. It stores the HALT ROW's offset, which can sit below `f`
    ///   while the ack it stands for sat above it and was lost: a remote bridge
    ///   would never re-ack. Clearing costs one redundant ack row;
    /// * `inbox-owed`: forgotten: committed on this log, it would carry the group
    ///   past records nobody delivered. Clamped: kept below `f`, where the records
    ///   it covers are the same ones;
    /// * `acked` keeps one member, `head - 1`, clamped or not: the self-lane
    ///   window then starts at this log's head, so a record of this node's lane
    ///   already on this log reads "cannot tell" (this node never held an ack for
    ///   it here), not a forgery, and every later record is judged as before.
    ///   Vouching for the old acks below `f` would buy nothing — nothing below the
    ///   fork is offered again — and would put a record a crash left between
    ///   `f` and `head` inside the window, as an accusation;
    /// * `deadlines` and `asked`: forgotten: no reply to an ask made on the old
    ///   log can arrive on this one, and an `expired` here would name another
    ///   record. Clamped: an ask below `f` is still that ask, and kept;
    /// * `keys/` and `sent/` lose only `original-off`: a previous winner's
    ///   position is no evidence about a publication on this log. Clamped: only
    ///   one at or above `f`;
    /// * an `inflight/*` record carrying a `re=` and witnessed on a different known
    ///   log goes — clamped, only one whose `re=` is at or above `f`, and one below
    ///   it is re-witnessed onto `log`, where the attach then replays it — returned as
    ///   `(sequence, face)` for the attach to report. One with an unknown witness
    ///   or no `re=` stays: absence is not proof that a pending body belongs to
    ///   another log. That is a body this node DECIDED to publish and holds no
    ///   proof of, so it is published on this log — which is why an entry ends in
    ///   the step that writes its record's offset to `acked`, and never outlives
    ///   the record's delivery: one kept past it would publish a delivered record
    ///   here a second time.
    ///
    /// Node-scoped facts stay (`node`, `inc`, `seq`, `pins`, `acks/`, and the
    /// sequences, identities and key ownership in `sent/` and `keys/`), and so do
    /// the halts this node holds in force (`halts`): only their human's own row
    /// lifts them, never the absence of one.
    ///
    /// Idempotent — a clamp applied twice clamps to the same `f` — and run
    /// before the witness is re-based ([`StateDir::set_log_witness`]): a crash in
    /// between re-judges the log at the next attach and forgets (or clamps)
    /// again.
    ///
    /// # Errors
    ///
    /// The first I/O failure, including an in-flight journal that cannot be read
    /// whole. Everything is read before the first change; what was forgotten before
    /// a later write failure stays forgotten. The caller must have the endpoint's
    /// acknowledgement of the new log (`Bridge::tell_endpoint_log`) before this
    /// reset may drop an old referenced publication.
    pub fn forget_log_offsets(
        &self,
        head: u64,
        log: Option<astream_broker::LogIdentity>,
        keep_below: Option<u64>,
    ) -> io::Result<Vec<(u64, String)>> {
        // A fork above the head cannot be: the log grew from it. Held to the
        // head, so a cursor never resumes past what this log holds.
        let keep_below = keep_below.map(|f| f.min(head));
        // Whether an offset of the old log still names its record here.
        let kept = |off: u64| keep_below.is_some_and(|f| off < f);
        // Enumerate and read everything before the first removal. An absent
        // directory is empty; a failed listing, entry or topic read is
        // uncertainty and must not let the witness move onto the new log.
        let entries = |name: &str| -> io::Result<Vec<std::fs::DirEntry>> {
            let path = self.path(name);
            match std::fs::read_dir(&path) {
                Ok(dir) => dir.collect(),
                Err(e) if e.kind() == io::ErrorKind::NotFound => {
                    match std::fs::symlink_metadata(&path) {
                        Err(missing) if missing.kind() == io::ErrorKind::NotFound => Ok(Vec::new()),
                        Err(other) => Err(other),
                        Ok(_) => Err(e),
                    }
                }
                Err(e) => Err(e),
            }
        };
        // `seen/*`: removed, or (clamped) rewritten at the fork. A watermark
        // that does not parse names nothing anyone can trust, and goes.
        let mut seen_removed = Vec::new();
        let mut seen_clamped = Vec::new();
        for entry in entries("seen")? {
            let name = entry.file_name();
            let sid = name.to_str().filter(|n| !n.contains('.'));
            let off = sid.and_then(|sid| self.seen_off(sid));
            match (sid, off, keep_below) {
                (Some(_), Some(off), Some(_)) if kept(off) => {}
                (Some(sid), Some(_), Some(f)) if f > 0 => {
                    seen_clamped.push((sid.to_string(), f - 1));
                }
                _ => seen_removed.push(entry.path()),
            }
        }
        // `unhandled/*`: removed, or (clamped) the floors below the fork kept.
        let mut unhandled_removed = Vec::new();
        let mut unhandled_kept = Vec::new();
        for entry in entries("unhandled")? {
            let name = entry.file_name();
            let floors = match name.to_str().filter(|n| !n.contains('.')) {
                Some(sid) => {
                    // Uncertainty is not permission to drop a retained refill
                    // floor, including when the caller must forget this log.
                    let floors = self.topic_floors(sid)?;
                    keep_below.map(|_| (sid.to_string(), floors))
                }
                None => None,
            };
            match floors {
                Some((sid, floors)) => {
                    let below: BTreeMap<String, u64> =
                        floors.into_iter().filter(|(_, off)| kept(*off)).collect();
                    unhandled_kept.push((sid, below));
                }
                None => unhandled_removed.push(entry.path()),
            }
        }
        let mut topics = Vec::new();
        for entry in entries("topics")? {
            let name = entry.file_name();
            let Some(sid) = name.to_str().filter(|n| !n.contains('.')) else {
                continue;
            };
            let body = std::fs::read_to_string(entry.path())?;
            let rebased: BTreeMap<String, TopicCursor> = self
                .parse_topics(sid, &body)
                .into_iter()
                .map(|(topic, cursor)| {
                    let next = match keep_below {
                        Some(f) => cursor.next.min(f),
                        None => head,
                    };
                    (topic, TopicCursor { next })
                })
                .collect();
            topics.push((sid.to_string(), rebased));
        }
        let publication_resets = self.publication_offset_resets(keep_below)?;
        let in_flight = self.in_flight()?;
        let owed = self.inbox_owed();
        let deadlines: Vec<Deadline> = self
            .deadlines()
            .into_iter()
            .filter(|d| kept(d.off))
            .collect();
        let asked: Vec<Asked> = self.asked().into_iter().filter(|a| kept(a.off)).collect();
        for path in seen_removed.into_iter().chain(unhandled_removed) {
            match std::fs::remove_file(path) {
                Err(e) if e.kind() != io::ErrorKind::NotFound => return Err(e),
                _ => {}
            }
        }
        for (sid, off) in seen_clamped {
            // Not `set_seen_off`, which only rises: the clamp moves it down.
            self.write(&format!("seen/{sid}"), &off.to_string())?;
        }
        for (sid, floors) in unhandled_kept {
            self.set_topic_floors(&sid, &floors)?;
        }
        for (sid, rebased) in topics {
            self.set_topics(&sid, &rebased)?;
        }
        for (name, text) in publication_resets {
            self.write(&name, &text)?;
        }
        self.set_self_acked(&head.checked_sub(1).into_iter().collect(), 1)?;
        self.clear_halt_acked()?;
        if !owed.is_some_and(kept) {
            self.clear_inbox_owed()?;
        }
        self.set_deadlines(&deadlines)?;
        self.set_asked(&asked)?;
        let mut dropped = Vec::new();
        for (seq, entry) in in_flight {
            let different_log = matches!((entry.log, log), (Some(was), Some(now)) if was != now);
            let re = crate::body::Body::decode(&entry.body).0.re;
            if !different_log || re.is_none() {
                continue;
            }
            // A REPLY BELOW THE FORK IS RE-WITNESSED ONTO THIS LOG: its `re=`
            // names the same record here, and the journal's witness is what
            // the attach's replay goes by ([`crate::bridge`]'s
            // `settle_in_flight`, which replays a reference only on the log it
            // names). Kept under the old witness it would survive this clamp
            // only to be dropped there, unpublished. Rewritten before the
            // bridge's own witness moves, so a crash in between finds it
            // already on this log and keeps it: the clamp stays idempotent.
            if re.is_some_and(kept) {
                self.set_in_flight(&InFlight { log, ..entry })?;
                continue;
            }
            match std::fs::remove_file(self.path(&format!("inflight/{seq}"))) {
                Err(e) if e.kind() != io::ErrorKind::NotFound => return Err(e),
                _ => dropped.push((seq, entry.subject)),
            }
        }
        Ok(dropped)
    }

    /// What this node knows of the bus log it reads ([`LogWitness`]), or
    /// `None` before its first attach recorded anything.
    #[must_use]
    pub fn log_witness(&self) -> Option<LogWitness> {
        self.read("bus-log").and_then(|s| LogWitness::parse(&s))
    }

    /// Record what this node knows of the bus log it reads. Not monotone: the attach
    /// that finds the log replaced re-bases it onto the new log.
    ///
    /// # Errors
    ///
    /// Any I/O failure.
    pub fn set_log_witness(&self, witness: &LogWitness) -> io::Result<()> {
        self.write("bus-log", &witness.render())
    }

    /// The known log identity this node saw rewind (reuse its offsets), which stays
    /// refused. Only a missing file is no refusal: an unreadable or damaged one is
    /// an error, never read as absent.
    pub(crate) fn rewound_log(&self) -> io::Result<Option<astream_broker::LogIdentity>> {
        use std::io::Read;
        use std::os::unix::fs::OpenOptionsExt;

        let path = self.path("rewound-log");
        match std::fs::symlink_metadata(&path) {
            Err(e) if e.kind() == io::ErrorKind::NotFound => return Ok(None),
            Err(e) => return Err(e),
            Ok(meta) if !meta.is_file() => {
                return Err(io::Error::new(
                    io::ErrorKind::InvalidData,
                    "the rewound log fact is not a regular file",
                ));
            }
            Ok(_) => {}
        }
        let file = std::fs::OpenOptions::new()
            .read(true)
            .custom_flags(crate::enable::LOCK_OPEN_FLAGS)
            .open(path)?;
        let mut text = String::new();
        file.take(128).read_to_string(&mut text)?;
        if text.len() < 128 {
            if let Some(log) = astream_broker::LogIdentity::parse(text.trim()) {
                return Ok(Some(log));
            }
        }
        Err(io::Error::new(
            io::ErrorKind::InvalidData,
            "the rewound log fact has no valid identity",
        ))
    }

    /// Record a rewound identity durably, so its refusal outlives a bridge or host
    /// restart.
    pub(crate) fn set_rewound_log(&self, log: astream_broker::LogIdentity) -> io::Result<()> {
        self.write_durable("rewound-log", &log.to_string())
    }

    /// Release the refusal, once the caller has fenced the endpoint on a different
    /// known log. The new witness is flushed before the refusal is removed, so a
    /// restart cannot find the old witness without its refusal.
    pub(crate) fn clear_rewound_log(&self, witness: &LogWitness) -> io::Result<()> {
        self.write_durable("bus-log", &witness.render())?;
        match std::fs::remove_file(self.path("rewound-log")) {
            Err(e) if e.kind() != io::ErrorKind::NotFound => return Err(e),
            _ => {}
        }
        sync_dir(&self.root)
    }

    /// The fleet halts of `fleet` this node holds in force: `human → StandingHalt`.
    /// The bus is the authority (every attach re-reads `Last{/f/<F>/fleet/*/halt}`);
    /// this stands in for a row the bus has lost: a human whose halt this node held
    /// and whose row is not on the face it reads now is still halting, until that
    /// human's own row on this log says otherwise.
    ///
    /// A line for another fleet is skipped; a damaged one is reported and read as
    /// nothing, leaving the halts to the bus alone.
    #[must_use]
    pub fn standing_halts(&self, fleet: &str) -> BTreeMap<String, StandingHalt> {
        let Some(body) = self.read("halts") else {
            return BTreeMap::new();
        };
        let mut out = BTreeMap::new();
        for line in body.lines() {
            match StandingHalt::parse(line) {
                Some((f, human, halt)) if f == fleet => {
                    out.insert(human, halt);
                }
                Some(_) => {}
                None => eprintln!(
                    "aterm-link: {}: `{}` is not a standing halt; it is not kept",
                    self.path("halts").display(),
                    safe_line(line)
                ),
            }
        }
        out
    }

    /// Replace `fleet`'s standing halts ([`StateDir::standing_halts`]).
    ///
    /// # Errors
    ///
    /// Any I/O failure.
    pub fn set_standing_halts(
        &self,
        fleet: &str,
        halts: &BTreeMap<String, StandingHalt>,
    ) -> io::Result<()> {
        let lines: Vec<String> = halts
            .iter()
            .map(|(human, halt)| halt.render(fleet, human))
            .collect();
        // Durable: the halt is a human's, and the row this file stands in for
        // may be on no log this node can still read.
        self.write_durable("halts", &lines.join("\n"))
    }

    /// The TOFU `sid → node` pins (§6.1): the first node seen advertising a sid owns
    /// it, and a second node claiming it is a conflict, never a route.
    #[must_use]
    pub fn pins(&self) -> BTreeMap<String, String> {
        self.pin_lines()
            .into_iter()
            .filter_map(|l| {
                l.split_once(' ')
                    .map(|(a, b)| (a.to_string(), b.to_string()))
            })
            .collect()
    }

    /// The pin file's lines in first-sight order, the order [`PINS_KEEP`] evicts in.
    fn pin_lines(&self) -> Vec<String> {
        self.read("pins")
            .map(|s| {
                s.lines()
                    .filter(|l| l.contains(' '))
                    .map(str::to_string)
                    .collect()
            })
            .unwrap_or_default()
    }

    /// Pin `sid` to `node` unless it is pinned already. Answers the node it is pinned
    /// to, which is not necessarily `node`: a caller that assumed it was would turn a
    /// conflict into a route.
    ///
    /// Bounded at [`PINS_KEEP`], oldest first sight out; a new pin is appended, so the
    /// file stays in first-sight order.
    ///
    /// # Errors
    ///
    /// Any I/O failure.
    pub fn pin(&self, sid: &str, node: &str) -> io::Result<String> {
        let mut lines = self.pin_lines();
        if let Some(have) = lines
            .iter()
            .filter_map(|l| l.split_once(' '))
            .find(|(s, _)| *s == sid)
            .map(|(_, n)| n.to_string())
        {
            return Ok(have);
        }
        lines.push(format!("{sid} {node}"));
        let over = lines.len().saturating_sub(PINS_KEEP);
        // Durable: a remote sid outlives this host's reboot, and a first sight
        // the boot took back is one a second claimant may then win.
        self.write_durable("pins", &lines[over..].join("\n"))?;
        Ok(node.to_string())
    }

    /// One session's broadcast cursors: `topic → the next offset it may deliver from`.
    ///
    /// The opt-in lives in aterm, the cursor here. The session owns the set (`topic
    /// add`/`topic drop`) and answers `since=head|@<off>`, an intent, not a position;
    /// the bridge resolves it once, against the broker's head when it first learns
    /// the entry, and writes the offset here, so a restarted bridge resumes each topic
    /// instead of re-reading `head` and losing what was published while it was down.
    /// One file per session ([`StateDir::forget_topics`]).
    #[must_use]
    pub fn topics(&self, sid: &str) -> BTreeMap<String, TopicCursor> {
        self.read(&format!("topics/{sid}"))
            .map_or_else(BTreeMap::new, |body| self.parse_topics(sid, &body))
    }

    /// The cursor parse shared by [`Self::topics`] and [`Self::forget_log_offsets`].
    fn parse_topics(&self, sid: &str, body: &str) -> BTreeMap<String, TopicCursor> {
        let mut out = BTreeMap::new();
        for line in body.trim().lines() {
            match TopicCursor::parse(line) {
                Some((topic, cursor)) => {
                    out.insert(topic, cursor);
                }
                // Reported: a dropped cursor is a dropped delivery guarantee,
                // and the session has no other way to learn it.
                None => eprintln!(
                    "aterm-link: {}: `{}` is not a broadcast cursor; that topic \
                     resumes from the bus head; what it missed is lost",
                    self.path(&format!("topics/{sid}")).display(),
                    safe_line(line)
                ),
            }
        }
        out
    }

    /// Replace one session's broadcast cursors.
    ///
    /// # Errors
    ///
    /// Any I/O failure.
    pub fn set_topics(&self, sid: &str, cursors: &BTreeMap<String, TopicCursor>) -> io::Result<()> {
        let body: Vec<String> = cursors
            .iter()
            .map(|(t, c)| format!("{t} {}", c.next))
            .collect();
        self.write(&format!("topics/{sid}"), &body.join("\n"))
    }

    /// Forget the broadcast cursors of a session gone from the roster, and the
    /// refill floors that rest on them ([`Self::topic_floors`]).
    pub fn forget_topics(&self, sid: &str) {
        let _ = std::fs::remove_file(self.path(&format!("topics/{sid}")));
        let _ = std::fs::remove_file(self.path(&format!("unhandled/{sid}")));
    }

    /// One session's broadcast refill floors: per topic, the lowest offset of a
    /// broadcast its endpoint took as a new row (`deliver` answered `OK <id>`) that
    /// `inbox seen` has not yet passed. A topic with nothing unhandled has no line.
    ///
    /// The topic cursor ([`Self::topics`]) moves past a broadcast on the endpoint's
    /// `OK`, since a cursor that waited for the agent would re-offer every unread
    /// broadcast on every bridge restart. But the ring holding the row lives in
    /// aterm's memory, and a relaunch that keeps the sid (a seamless update, a PTY
    /// keeper's recovery) starts an empty one. A bridge that loads a session's
    /// cursors from disk rewinds each to its floor here, and the catch-up offers the
    /// stretch again: a fresh ring takes the rows as new, and a ring that still holds
    /// them (a bridge-only restart) dedups them on `off=`.
    ///
    /// Written before the cursor moves past the row, so a kill between the two
    /// leaves a floor at or above the cursor, which rewinds nothing. A missing file
    /// is empty; an unreadable or damaged one is uncertainty and never authorizes
    /// advancing past an unread row.
    ///
    /// # Errors
    ///
    /// An unreadable directory or entry, a non-regular entry (a dangling symlink
    /// included), an empty file, or any line that is not one unique topic and offset.
    pub fn topic_floors(&self, sid: &str) -> io::Result<BTreeMap<String, u64>> {
        let dir = self.path("unhandled");
        match std::fs::symlink_metadata(&dir) {
            Err(e) if e.kind() == io::ErrorKind::NotFound => return Ok(BTreeMap::new()),
            Err(e) => return Err(e),
            Ok(meta) if !meta.is_dir() => {
                return Err(io::Error::new(
                    io::ErrorKind::InvalidData,
                    "refill floor parent is not a directory",
                ));
            }
            Ok(_) => {}
        }
        let path = dir.join(sid);
        match std::fs::symlink_metadata(&path) {
            Err(e) if e.kind() == io::ErrorKind::NotFound => return Ok(BTreeMap::new()),
            Err(e) => return Err(e),
            Ok(meta) if !meta.is_file() => {
                return Err(io::Error::new(
                    io::ErrorKind::InvalidData,
                    "refill floor is not a regular file",
                ));
            }
            Ok(_) => {}
        }
        let body = std::fs::read_to_string(&path)?;
        if body.trim().is_empty() {
            return Err(io::Error::new(
                io::ErrorKind::InvalidData,
                "empty broadcast refill floor file",
            ));
        }
        let mut out = BTreeMap::new();
        for line in body.lines() {
            let mut words = line.split_whitespace();
            let topic = words.next().filter(|t| crate::subject::is_topic(t));
            let off = words
                .next()
                .filter(|w| !w.is_empty() && w.bytes().all(|b| b.is_ascii_digit()))
                .and_then(|w| w.parse::<u64>().ok());
            match (topic, off, words.next()) {
                (Some(topic), Some(off), None) => {
                    if out.insert(topic.to_string(), off).is_some() {
                        return Err(io::Error::new(
                            io::ErrorKind::InvalidData,
                            "duplicate broadcast refill topic",
                        ));
                    }
                }
                _ => {
                    return Err(io::Error::new(
                        io::ErrorKind::InvalidData,
                        format!(
                            "{}: `{}` is not a broadcast refill floor",
                            path.display(),
                            safe_line(line)
                        ),
                    ));
                }
            }
        }
        Ok(out)
    }

    /// Replace one session's broadcast refill floors ([`Self::topic_floors`]);
    /// an empty set removes the file.
    ///
    /// # Errors
    ///
    /// Any I/O failure.
    pub fn set_topic_floors(&self, sid: &str, floors: &BTreeMap<String, u64>) -> io::Result<()> {
        let name = format!("unhandled/{sid}");
        if floors.is_empty() {
            return match std::fs::remove_file(self.path(&name)) {
                Err(e) if e.kind() != io::ErrorKind::NotFound => Err(e),
                _ => Ok(()),
            };
        }
        let body: Vec<String> = floors.iter().map(|(t, off)| format!("{t} {off}")).collect();
        self.write(&name, &body.join("\n"))
    }

    /// The producer sequence this bridge reserved for one outbound post, and whether
    /// a `post key=` entry an earlier post reserved chose it ([`StateDir::key_seq`]).
    ///
    /// This makes the outbound half exactly once. `outbox` is a peek, so a bridge that
    /// dies between `Publish` and `outbox sent` re-reads the post on restart, and a
    /// fresh sequence would be a second record (the broker dedups on `(producer_id,
    /// producer_seq)`). At the pinned sequence the retry is byte-identical and the
    /// broker answers `deduped` with the original offset, the one the sender's
    /// `--wait` was promised. The `key` flag makes the retirement say `dup=1`, after a
    /// relaunch too.
    #[must_use]
    pub fn post_seq(&self, sid: &str, id: u64) -> Option<(u64, bool)> {
        self.post_pin(sid, id)
            .ok()
            .flatten()
            .map(|pin| (pin.seq, pin.via_key))
    }

    /// Reserve a sequence for one outbound post before it is published. `via_key`
    /// says a `post key=` entry chose it ([`StateDir::post_seq`]).
    ///
    /// # Errors
    ///
    /// Any I/O failure; the caller must not publish then.
    pub fn set_post_seq(&self, sid: &str, id: u64, seq: u64, via_key: bool) -> io::Result<()> {
        let line = if via_key {
            format!("{seq} key")
        } else {
            seq.to_string()
        };
        self.write(&format!("sent/{sid}.{id}"), &line)
    }

    /// Forget a retired post's sequence; called only once the endpoint has
    /// acknowledged the retirement.
    pub fn clear_post_seq(&self, sid: &str, id: u64) {
        let _ = self.unlink(&format!("sent/{sid}.{id}"));
    }

    /// The producer sequence this bridge reserved for one owed receipt, if any: the
    /// `receipt sid=<sid> rid=<rid> …` line an endpoint's `outbox` peek lists from a
    /// session's `inbox seen <id> handled|refused|deferred` until the bridge retires
    /// it (R8).
    ///
    /// [`StateDir::post_seq`]'s rule for receipts: republished after a crash at this
    /// sequence, the `ack` is `deduped` rather than a second one on the sender's lane.
    #[must_use]
    pub fn receipt_seq(&self, sid: &str, rid: u64) -> Option<u64> {
        self.receipt_pin(sid, rid).ok().flatten().map(|pin| pin.seq)
    }

    /// Reserve a sequence for one owed receipt before it is published
    /// ([`StateDir::receipt_seq`]).
    ///
    /// # Errors
    ///
    /// Any I/O failure; the caller must not publish then.
    pub fn set_receipt_seq(&self, sid: &str, rid: u64, seq: u64) -> io::Result<()> {
        self.write(&format!("acks/{sid}.{rid}"), &seq.to_string())
    }

    /// Forget a retired receipt's sequence. Called only after the endpoint has
    /// taken the retirement.
    pub fn clear_receipt_seq(&self, sid: &str, rid: u64) {
        let _ = std::fs::remove_file(self.path(&format!("acks/{sid}.{rid}")));
    }

    /// The producer sequence a session's `post key=<key>` reserved, if any post under
    /// that key did.
    ///
    /// This makes `post key=` exactly once (R7, §6.5). [`StateDir::post_seq`] pins a
    /// sequence to a post id, which survives a bridge crash but not a re-post: after
    /// `ERR timeout` the sender cannot tell landed from lost, and posting again mints
    /// a new id. This table pins the sequence to the caller's key, so a re-post
    /// publishes at the same `(producer_id, producer_seq)` and the broker's dedup
    /// (rebuilt from the log on a broker restart) answers `deduped` with the original
    /// offset. The endpoint's `outbox` carries the key on every drain, so a
    /// relaunched bridge reads it again. First wins, like a pin; one file per
    /// session, newest last, bounded at [`KEYS_KEEP`].
    #[must_use]
    pub fn key_seq(&self, sid: &str, key: &str) -> Option<u64> {
        self.key_pin(sid, key).ok().flatten().map(|pin| pin.seq)
    }

    /// The key file's lines in reservation order, the order [`KEYS_KEEP`] evicts in.
    #[cfg(test)]
    fn key_lines(&self, sid: &str) -> Vec<String> {
        self.read(&format!("keys/{sid}"))
            .map(|s| {
                s.lines()
                    .filter(|l| l.contains(' '))
                    .map(str::to_string)
                    .collect()
            })
            .unwrap_or_default()
    }

    /// Reserve `seq` for `key` before the publish, unless the key holds one already
    /// (first wins). Answers the sequence the key holds afterwards.
    ///
    /// # Errors
    ///
    /// Any I/O failure; the caller must not publish then.
    pub fn set_key_seq(&self, sid: &str, key: &str, seq: u64) -> io::Result<u64> {
        self.set_key_pin(
            sid,
            key,
            &PublishPin {
                seq,
                via_key: false,
                identity: None,
                original: None,
                original_off: None,
                owner: None,
            },
        )
        .map(|pin| pin.seq)
    }

    /// Forget `key`'s reservation if it is `seq` (that pair only): `seq` turned out
    /// to be another writer's, and a re-post under the key must reserve its own
    /// number, not dedup onto a record that is not its post.
    ///
    /// # Errors
    ///
    /// Any I/O failure.
    pub fn forget_key_seq(&self, sid: &str, key: &str, seq: u64) -> io::Result<()> {
        if !self.key_pin(sid, key)?.is_some_and(|pin| pin.seq == seq) {
            return Ok(());
        }
        let text = self
            .read_reservation(&format!("keys/{sid}"))?
            .unwrap_or_default();
        let kept: Vec<&str> = text
            .lines()
            .filter(|l| l.split_whitespace().next() != Some(key))
            .collect();
        self.write(&format!("keys/{sid}"), &kept.join("\n"))
    }

    /// Every deadline this node still owes a verdict on, oldest offset first.
    #[must_use]
    pub fn deadlines(&self) -> Vec<Deadline> {
        self.read("deadlines")
            .map(|s| s.lines().filter_map(Deadline::parse).collect())
            .unwrap_or_default()
    }

    /// Every ask this node remembers the recipient of, oldest offset first.
    #[must_use]
    pub fn asked(&self) -> Vec<Asked> {
        self.read("asked")
            .map(|s| s.lines().filter_map(Asked::parse).collect())
            .unwrap_or_default()
    }

    /// Replace the asked table, keeping the newest [`ASKED_KEEP`] by position.
    ///
    /// # Errors
    ///
    /// Any I/O failure.
    pub fn set_asked(&self, list: &[Asked]) -> io::Result<()> {
        let over = list.len().saturating_sub(ASKED_KEEP);
        let lines: Vec<String> = list[over..].iter().map(Asked::render).collect();
        self.write("asked", &lines.join("\n"))
    }

    /// Replace the deadline table, keeping the newest [`DEADLINES_KEEP`] by position.
    /// Written whole: it is small and changes once per `dl=` ask and per reply.
    ///
    /// # Errors
    ///
    /// Any I/O failure.
    pub fn set_deadlines(&self, list: &[Deadline]) -> io::Result<()> {
        let over = list.len().saturating_sub(DEADLINES_KEEP);
        let lines: Vec<String> = list[over..].iter().map(Deadline::render).collect();
        self.write("deadlines", &lines.join("\n"))
    }

    /// The directory itself, for what this type does not model: the `pid` file and
    /// the bridge's fault and test markers.
    #[must_use]
    pub fn root(&self) -> &Path {
        &self.root
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn scratch(tag: &str) -> PathBuf {
        let p = std::env::temp_dir().join(format!("atlink-state-{}-{tag}", std::process::id()));
        let _ = std::fs::remove_dir_all(&p);
        p
    }

    /// A BLOCK IS ONE WRITE, AND ITS LAST NUMBER IS ON DISK BEFORE ITS FIRST IS
    /// ANSWERED. The file holds the block's end the moment the call returns, so
    /// a reopen — the next process, after a `kill -9` that used part of the
    /// block — starts above every number the block could have handed out; a
    /// block of one is the old one-number reservation; and a block that would
    /// cross the incarnation's reserved top (the will's own number) is cut
    /// short below it, then refused once nothing is left.
    #[test]
    fn a_sequence_block_is_on_disk_before_use_and_never_reaches_the_wills_number() {
        let dir = scratch("seqblock");
        let st = StateDir::open(&dir).expect("open");
        let block = st.reserve_sequence_block(3, 64).expect("a block");
        assert_eq!(block, (3 << 32) + 1..=(3 << 32) + 64);
        assert_eq!(
            st.sequence(),
            *block.end(),
            "the END is what the file holds"
        );
        let reopened = StateDir::open(&dir).expect("reopen");
        assert_eq!(
            reopened.reserve_next_sequence(3).expect("one more"),
            block.end() + 1,
            "a reopen starts above the whole block, however little of it was used"
        );
        // A later incarnation starts above its own base, not above the file.
        assert_eq!(
            reopened.reserve_sequence_block(4, 2).expect("inc 4"),
            (4 << 32) + 1..=(4 << 32) + 2
        );
        // Near the top: cut short below the will's number, then refused.
        std::fs::write(
            dir.join("seq"),
            ((5u64 << 32) | (WILL_SEQ_LOW - 3)).to_string(),
        )
        .expect("seed near the top");
        let short = reopened
            .reserve_sequence_block(5, 64)
            .expect("a short block");
        assert_eq!(
            short,
            ((5 << 32) | (WILL_SEQ_LOW - 2))..=((5 << 32) | (WILL_SEQ_LOW - 1))
        );
        assert!(
            reopened.reserve_sequence_block(5, 64).is_err(),
            "nothing below the will's number is left"
        );
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// The node id (the node's mail address) is minted once and read back on reopen.
    #[test]
    fn the_node_id_is_minted_once_and_survives_reopen() {
        let dir = scratch("node");
        let first = StateDir::open(&dir)
            .expect("open")
            .node_id(|| "n-0123456789abcdef".to_string())
            .expect("mint");
        let second = StateDir::open(&dir)
            .expect("reopen")
            .node_id(|| panic!("must not re-mint"))
            .expect("read back");
        assert_eq!(first, second);
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// A cursor line is `<topic> <next>` (a third number is read past); any other line
    /// is dropped and reported while its neighbours survive.
    #[test]
    fn a_topic_cursor_line_parses_or_is_dropped_beside_its_neighbours() {
        let c = |next| TopicCursor { next };
        assert_eq!(
            TopicCursor::parse("build.failed 42"),
            Some(("build.failed".to_string(), c(42)))
        );
        assert_eq!(
            TopicCursor::parse("old.topic 3 7"),
            Some(("old.topic".to_string(), c(3)))
        );
        for bad in [
            "",
            "onlytopic",
            "t x",
            "t 1 y",
            "t 1 2 3",
            "t +5",
            "T 5",
            "a/b 5",
            "a* 5",
            "> 5",
        ] {
            assert_eq!(TopicCursor::parse(bad), None, "{bad:?}");
        }
        let dir = scratch("topics");
        let st = StateDir::open(&dir).expect("open");
        let mut good = BTreeMap::new();
        good.insert("a".to_string(), c(10));
        good.insert("b".to_string(), c(20));
        st.set_topics("s-x", &good).expect("write");
        assert_eq!(st.topics("s-x"), good);
        std::fs::write(
            st.path("topics/s-x"),
            "a 10\nthis is not a cursor \x1b[2J\nb 20\n",
        )
        .expect("corrupt one line");
        assert_eq!(st.topics("s-x"), good, "the neighbours survive");
        st.forget_topics("s-x");
        assert!(st.topics("s-x").is_empty());
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// A session's broadcast refill floors round-trip, an empty set removes
    /// the file, a damaged line refuses the whole read until repaired, and a
    /// floor goes with its session's cursors and with the log its offsets index.
    #[test]
    fn a_broadcast_refill_floor_round_trips_and_goes_with_its_cursors_and_its_log() {
        let dir = scratch("unhandled");
        let st = StateDir::open(&dir).expect("open");
        let mut floors = BTreeMap::new();
        floors.insert("a".to_string(), 10);
        floors.insert("b.c".to_string(), 7);
        st.set_topic_floors("s-x", &floors).expect("write");
        assert_eq!(st.topic_floors("s-x").unwrap(), floors);
        std::fs::write(
            st.path("unhandled/s-x"),
            "a 10\nb.c +7\nB 3\nb.c 7\nd 1 2\n",
        )
        .expect("damage some lines");
        assert!(
            st.topic_floors("s-x").is_err(),
            "damaged state is not an empty floor"
        );
        st.set_topic_floors("s-x", &floors).expect("repair");
        assert_eq!(st.topic_floors("s-x").unwrap(), floors);

        st.set_topic_floors("s-x", &BTreeMap::new())
            .expect("an empty set");
        assert!(
            !st.path("unhandled/s-x").exists(),
            "an empty set is no file"
        );
        assert!(st.set_topic_floors("s-x", &BTreeMap::new()).is_ok());

        st.set_topic_floors("s-x", &floors).expect("write");
        st.forget_topics("s-x");
        assert!(
            st.topic_floors("s-x").unwrap().is_empty(),
            "a departed session's floors go"
        );

        st.set_topic_floors("s-y", &floors).expect("write");
        st.forget_log_offsets(100, None, None).expect("re-base");
        assert!(
            st.topic_floors("s-y").unwrap().is_empty(),
            "a floor names a record of the old log"
        );
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// The `seen` watermark never moves backwards.
    #[test]
    fn the_seen_watermark_never_goes_backwards() {
        let dir = scratch("seen");
        let st = StateDir::open(&dir).expect("open");
        st.set_seen_off("s-a", 42).expect("write");
        st.set_seen_off("s-a", 7).expect("write");
        assert_eq!(st.seen_off("s-a"), Some(42));
        st.set_seen_off("s-a", 43).expect("write");
        assert_eq!(st.seen_off("s-a"), Some(43));
        assert_eq!(st.seen_off("s-unknown"), None);
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// The self-lane acked set is a set, not a watermark, and bounded; the halt-ack
    /// watermark never moves backwards.
    #[test]
    fn the_self_acked_set_is_a_set_and_is_bounded() {
        let dir = scratch("acked");
        let st = StateDir::open(&dir).expect("open");
        let mut set = std::collections::BTreeSet::new();
        for off in [10u64, 30, 20] {
            set.insert(off);
            st.set_self_acked(&set, 3).expect("write");
        }
        assert_eq!(st.self_acked(), [10, 20, 30].into_iter().collect());
        assert_eq!(st.halt_acked(), None);
        st.set_halt_acked(91).expect("write");
        st.set_halt_acked(7).expect("write");
        assert_eq!(st.halt_acked(), Some(91));
        st.set_halt_acked(92).expect("write");
        assert_eq!(st.halt_acked(), Some(92));
        set.insert(40);
        st.set_self_acked(&set, 3).expect("write");
        assert_eq!(
            st.self_acked(),
            [20, 30, 40].into_iter().collect(),
            "the oldest is dropped, and 10 is now unknown rather than proven ours"
        );
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// A pin is first-wins and answers the pinned node, not the asked-for one.
    #[test]
    fn a_pin_is_first_wins_and_reports_the_pinned_node() {
        let dir = scratch("pins");
        let st = StateDir::open(&dir).expect("open");
        assert_eq!(st.pin("s-a", "n-one").expect("pin"), "n-one");
        assert_eq!(
            st.pin("s-a", "n-two").expect("pin"),
            "n-one",
            "a second node claiming a pinned sid is a conflict, not a re-pin"
        );
        assert_eq!(
            StateDir::open(&dir).expect("reopen").pins().get("s-a"),
            Some(&"n-one".to_string())
        );
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// A post's sequence is pinned to the post and survives a reopen
    /// ([`StateDir::post_seq`]).
    #[test]
    fn a_posts_sequence_is_pinned_to_the_post_and_survives_a_reopen() {
        let dir = scratch("postseq");
        let st = StateDir::open(&dir).expect("open");
        assert_eq!(st.post_seq("s-a", 1), None);
        st.set_post_seq("s-a", 1, 0x0000_0002_0000_0007, false)
            .expect("reserve");
        assert_eq!(
            StateDir::open(&dir).expect("reopen").post_seq("s-a", 1),
            Some((0x0000_0002_0000_0007, false))
        );
        // Another post has its own, and a retirement forgets one.
        assert_eq!(st.post_seq("s-a", 2), None);
        st.clear_post_seq("s-a", 1);
        assert_eq!(st.post_seq("s-a", 1), None);
        // The `key=` marker survives too, so a relaunched bridge's retirement still
        // says `dup=1`.
        st.set_post_seq("s-a", 3, 0x0000_0002_0000_0009, true)
            .expect("reserve via key");
        assert_eq!(
            StateDir::open(&dir).expect("reopen").post_seq("s-a", 3),
            Some((0x0000_0002_0000_0009, true))
        );
        // A file without the marker reads as a plain pin.
        std::fs::write(dir.join("sent/s-a.4"), "17\n").expect("write");
        assert_eq!(st.post_seq("s-a", 4), Some((17, false)));
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// An owed receipt's sequence is pinned to `(sid, rid)`, survives a reopen, and
    /// is forgotten at retirement.
    #[test]
    fn a_receipt_pins_its_sequence_until_it_is_retired() {
        let dir = scratch("acks");
        let st = StateDir::open(&dir).expect("open");
        assert_eq!(st.receipt_seq("s-a", 1), None);
        st.set_receipt_seq("s-a", 1, 77).expect("reserve");
        st.set_receipt_seq("s-a", 2, 78).expect("reserve");
        st.set_receipt_seq("s-b", 1, 79).expect("reserve");
        let again = StateDir::open(&dir).expect("reopen");
        assert_eq!(again.receipt_seq("s-a", 1), Some(77), "survives a reopen");
        assert_eq!(again.receipt_seq("s-a", 2), Some(78));
        assert_eq!(again.receipt_seq("s-b", 1), Some(79), "per session");
        again.clear_receipt_seq("s-a", 1);
        assert_eq!(again.receipt_seq("s-a", 1), None, "retired");
        assert_eq!(again.receipt_seq("s-a", 2), Some(78), "the others stand");
        assert!(
            std::fs::read_to_string(dir.join("acks/s-a.2"))
                .expect("the pin file")
                .trim()
                .parse::<u64>()
                .is_ok(),
            "a pin holds the number and nothing else"
        );
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn publication_identity_survives_pins_and_malformed_reservations_are_not_absent() {
        let dir = scratch("identities");
        let st = StateDir::open(&dir).unwrap();
        let pin = PublishPin {
            seq: 17,
            via_key: true,
            identity: Some(PublishIdentity {
                subject: "/f/f/in/n-a/s-b/n-c/ask".into(),
                digest: "a".repeat(64),
            }),
            original: Some(PublishIdentity {
                subject: "/f/f/in/n-d/s-e/n-c/note".into(),
                digest: "b".repeat(64),
            }),
            original_off: Some(0),
            owner: Some(1),
        };
        st.set_post_pin("s-a", 1, &pin).unwrap();
        assert_eq!(
            StateDir::open(&dir).unwrap().post_pin("s-a", 1).unwrap(),
            Some(pin.clone())
        );
        for damaged in [
            "",
            "17 identity /f/f/x nope",
            "17 identity /f/f/x",
            "17 garbage",
            "17 owner 1 owner 2",
            "17 original-off nope",
        ] {
            st.write("sent/s-a.1", damaged).unwrap();
            assert_eq!(
                st.post_pin("s-a", 1).unwrap_err().kind(),
                io::ErrorKind::InvalidData
            );
        }
        for damaged in [
            "key-only",
            "key-only garbage",
            "key-only 17 identity /f/f/x bad",
        ] {
            st.write("keys/s-a", damaged).unwrap();
            assert_eq!(
                st.key_pin("s-a", "key-only").unwrap_err().kind(),
                io::ErrorKind::InvalidData
            );
        }
        st.write("keys/s-a", "key-only\t17").unwrap();
        assert_eq!(st.key_pin("s-a", "key-only").unwrap().unwrap().seq, 17);
        st.forget_key_seq("s-a", "key-only", 17).unwrap();
        assert!(st.key_pin("s-a", "key-only").unwrap().is_none());
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// The in-flight journal round-trips the exact bytes, owner-only, in sequence
    /// order; a damaged entry blocks recovery rather than reading as absent or being
    /// discarded, and a writer's temp beside the entries is not one.
    #[test]
    fn in_flight_entries_round_trip_in_order_and_a_damaged_one_is_not_absent() {
        let dir = scratch("inflight");
        let st = StateDir::open(&dir).unwrap();
        assert!(
            st.in_flight().unwrap().is_empty(),
            "no dir is nothing in flight"
        );
        let entry = |seq: u64, body: &[u8]| InFlight {
            pin: PublishPin {
                seq,
                via_key: false,
                identity: Some(PublishIdentity {
                    subject: "/f/f/in/n-a/s-b/n-a/note".into(),
                    digest: "c".repeat(64),
                }),
                original: None,
                original_off: None,
                owner: Some(3),
            },
            subject: "/f/f/in/n-a/s-b/n-a/note".into(),
            body: body.to_vec(),
            log: None,
        };
        let late = InFlight {
            log: Some(astream_broker::LogIdentity {
                id: [0xaa; 16],
                generation: 2,
            }),
            ..entry(0x0000_0002_0000_0009, b"v=1 t=7 text=two%0Alines\n\x00\xff")
        };
        let early = entry(0x0000_0002_0000_0002, b"");
        st.set_in_flight(&late).unwrap();
        st.set_in_flight(&early).unwrap();
        let mode = std::os::unix::fs::PermissionsExt::mode(
            &std::fs::metadata(dir.join(format!("inflight/{}", late.pin.seq)))
                .unwrap()
                .permissions(),
        );
        assert_eq!(
            mode & 0o077,
            0,
            "a record's bytes are the owner's: {mode:o}"
        );
        std::fs::write(dir.join("inflight/.9.tmp.1.abcd"), "junk").unwrap();
        assert_eq!(
            StateDir::open(&dir).unwrap().in_flight().unwrap(),
            vec![(early.pin.seq, early.clone()), (late.pin.seq, late.clone())]
        );
        for damaged in [
            "",
            "8589934601",
            "8589934601\n/f/f/x",
            "8589934601\n/f/f/x\nnot-hex",
            "8589934601\n/f/f/*\n00",
            "8589934601\n/f/f/x\n00\nextra",
            "8589934601\n/f/f/x\n00\nlog=",
            "8589934601\n/f/f/x\n00\nlog=AA.0",
            "8589934601\n/f/f/x\n00\nlog=aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa.0\nextra",
            "8589934600\n/f/f/x\n00",
        ] {
            st.write(&format!("inflight/{}", late.pin.seq), damaged)
                .unwrap();
            assert_eq!(
                st.in_flight().unwrap_err().kind(),
                io::ErrorKind::InvalidData,
                "{damaged:?} must block recovery"
            );
            assert!(
                dir.join(format!("inflight/{}", late.pin.seq)).exists(),
                "uncertain publication fate must not discard its journal"
            );
        }
        st.clear_in_flight(late.pin.seq);
        st.clear_in_flight(early.pin.seq);
        assert!(st.in_flight().unwrap().is_empty());
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// **THE ACKED LOG IS APPENDED, READS AN OLDER BRIDGE'S FILE, AND NEVER
    /// TAKES A TORN TAIL FOR AN OFFSET.** Appends land after the last
    /// terminator; the one line an older bridge wrote reads whole, newline and
    /// all, but is not appendable, so the next write replaces it; a number
    /// without its comma is a prefix of some other offset and is dropped, and
    /// the file is then not appendable either; a compaction keeps the newest
    /// set in the format an older bridge's `split(',')` reads the same; and a
    /// link planted at the name is refused, not written through.
    #[test]
    fn the_acked_log_is_appended_reads_an_older_file_and_never_takes_a_torn_tail() {
        let dir = scratch("acked-log");
        let st = StateDir::open(&dir).unwrap();
        let set = |offs: &[u64]| {
            offs.iter()
                .copied()
                .collect::<std::collections::BTreeSet<_>>()
        };
        assert_eq!(
            st.self_acked_log(),
            AckedLog {
                appendable: true,
                ..AckedLog::default()
            },
            "no file is the empty log, appendable"
        );
        for off in [5, 7, 9, 7] {
            st.append_self_acked(off).unwrap();
        }
        assert_eq!(
            std::fs::read_to_string(dir.join("acked")).unwrap(),
            "5,7,9,7,"
        );
        assert_eq!(
            st.self_acked_log(),
            AckedLog {
                offsets: set(&[5, 7, 9]),
                entries: 4,
                appendable: true,
            }
        );
        let older = |text: &str| {
            std::fs::write(dir.join("acked"), text).unwrap();
            st.self_acked_log()
        };
        assert_eq!(
            older("1,2,3\n"),
            AckedLog {
                offsets: set(&[1, 2, 3]),
                entries: 3,
                appendable: false,
            },
            "an older bridge's line"
        );
        assert_eq!(
            older("5,7,45"),
            AckedLog {
                offsets: set(&[5, 7]),
                entries: 2,
                appendable: false,
            },
            "45 may be the torn head of 456: it is no offset of ours"
        );
        assert_eq!(older("").offsets, set(&[]));
        st.set_self_acked(&set(&[20, 30, 40]), 2).unwrap();
        let text = std::fs::read_to_string(dir.join("acked")).unwrap();
        assert_eq!(text, "30,40,", "the newest two, each ended by its comma");
        let older_reader: std::collections::BTreeSet<u64> = text
            .trim()
            .split(',')
            .filter_map(|n| n.parse().ok())
            .collect();
        assert_eq!(
            older_reader,
            set(&[30, 40]),
            "an older bridge reads the same set"
        );
        assert!(st.self_acked_log().appendable);
        std::fs::remove_file(dir.join("acked")).unwrap();
        let elsewhere = dir.join("elsewhere");
        std::fs::write(&elsewhere, "").unwrap();
        std::os::unix::fs::symlink(&elsewhere, dir.join("acked")).unwrap();
        assert!(
            st.append_self_acked(1).is_err(),
            "a planted link is refused"
        );
        assert_eq!(std::fs::read_to_string(&elsewhere).unwrap(), "");
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// An append admits its opened file before it writes: ordinary state is
    /// appendable, but a second name for another owned file is not our log.
    #[test]
    fn an_acked_append_preserves_a_file_with_another_name() {
        let scratch = aterm_tempfile::TempDir::new().expect("exclusive state scratch");
        let root = std::fs::canonicalize(scratch.path()).expect("canonical state scratch");
        let ordinary = StateDir::open(root.join("ordinary")).expect("ordinary state");
        ordinary
            .append_self_acked(5)
            .expect("first ordinary append");
        ordinary
            .append_self_acked(7)
            .expect("second ordinary append");
        assert_eq!(
            std::fs::read(ordinary.path("acked")).expect("ordinary log"),
            b"5,7,",
            "ordinary append controls precede the occupied-file case"
        );

        let linked = StateDir::open(root.join("linked")).expect("linked state");
        let other = root.join("other-owned-file");
        const SENTINEL: &[u8] = b"the other file owns these bytes\n";
        std::fs::write(&other, SENTINEL).expect("owned sentinel");
        std::fs::hard_link(&other, linked.path("acked")).expect("owned hardlink");
        let result = linked.append_self_acked(42);
        let other_bytes = std::fs::read(&other).expect("other file remains readable");
        let state_bytes = std::fs::read(linked.path("acked")).expect("state name remains readable");
        assert!(
            result.is_err() && other_bytes == SENTINEL && state_bytes == SENTINEL,
            "ACKED_APPEND_REQUIRES_OWNED_SINGLE_LINK_FILE: result={result:?}, other={other_bytes:?}, state={state_bytes:?}"
        );
    }

    /// A re-base drops an in-flight record carrying a `re=` only when both log
    /// identities prove it was prepared on another log; an unknown witness or a
    /// record without `re=` stays, and a damaged journal blocks every reset mutation.
    #[test]
    fn a_re_base_drops_proven_old_references_and_keeps_uncertain_or_unrelated_mail() {
        use astream_broker::LogIdentity;
        let (a, b) = (
            LogIdentity {
                id: [0xaa; 16],
                generation: 0,
            },
            LogIdentity {
                id: [0xbb; 16],
                generation: 0,
            },
        );
        let dir = scratch("inflight-rebase");
        let st = StateDir::open(&dir).unwrap();
        let face = "/f/f/in/n-a/s-b/n-a/expired";
        let entry = |seq: u64, log: Option<LogIdentity>| InFlight {
            pin: PublishPin {
                seq,
                via_key: false,
                identity: None,
                original: None,
                original_off: None,
                owner: None,
            },
            subject: face.into(),
            body: format!("v=1 t=1 re={seq}").into_bytes(),
            log,
        };
        st.set_in_flight(&entry(1, Some(a))).unwrap();
        st.set_in_flight(&entry(2, None)).unwrap();
        st.set_in_flight(&entry(3, Some(b))).unwrap();
        let note = InFlight {
            body: b"v=1 t=1 text=still-owed".to_vec(),
            ..entry(4, Some(a))
        };
        st.set_in_flight(&note).unwrap();
        assert_eq!(
            st.forget_log_offsets(100, Some(b), None).unwrap(),
            vec![(1, face.to_string())]
        );
        let kept = vec![(2, entry(2, None)), (3, entry(3, Some(b))), (4, note)];
        assert_eq!(st.in_flight().unwrap(), kept);
        assert!(
            st.forget_log_offsets(100, Some(b), None)
                .unwrap()
                .is_empty(),
            "idempotent"
        );
        assert!(
            st.forget_log_offsets(7, None, None).unwrap().is_empty(),
            "no identity proves no mismatch"
        );
        assert_eq!(st.in_flight().unwrap(), kept, "uncertainty is retained");
        st.set_seen_off("s-b", 44).unwrap();
        st.write("inflight/5", "damaged").unwrap();
        assert_eq!(
            st.forget_log_offsets(7, Some(b), None).unwrap_err().kind(),
            io::ErrorKind::InvalidData
        );
        assert!(dir.join("inflight/5").exists(), "damage is kept for repair");
        assert_eq!(st.seen_off("s-b"), Some(44), "preflight changed nothing");
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// A MOVED GENERATION CLAMPS INSTEAD OF FORGETTING
    /// ([`StateDir::forget_log_offsets`] with `keep_below = Some(f)`): every
    /// offset below the fork `f` still names its record, so whatever names one
    /// below it stays, whatever names one at or above it goes, and every cursor
    /// resumes at the lower of itself and `f` — `seen/` at `f - 1`, a topic at
    /// `f`; an in-flight reply below it is re-witnessed onto the new
    /// generation, where the attach replays it. `halt-acked` goes either way (it
    /// can name a halt row below the fork whose ack the loss took above it), and
    /// the `acked` window re-bases at the head either way. A clamp applied twice
    /// clamps to the same. NEGATIVE
    /// CONTROL on a twin dir: `None` — another log — forgets every one of them.
    #[test]
    fn a_moved_generation_clamps_the_offsets_it_continues_and_forgets_the_rest() {
        use astream_broker::LogIdentity;
        let old = LogIdentity {
            id: [0xaa; 16],
            generation: 0,
        };
        let new = LogIdentity {
            generation: 1,
            ..old
        };
        let pin = |original_off: Option<u64>| PublishPin {
            seq: 1,
            via_key: true,
            identity: original_off.map(|_| PublishIdentity {
                subject: "/f/f/in/n-a/s-b/n-c/ask".into(),
                digest: "a".repeat(64),
            }),
            original: None,
            original_off,
            owner: None,
        };
        let in_flight = |seq: u64, re: u64| InFlight {
            pin: PublishPin { seq, ..pin(None) },
            subject: "/f/f/in/n-a/s-b/n-a/answer".into(),
            body: format!("v=1 t=1 re={re}").into_bytes(),
            log: Some(old),
        };
        let deadline = |off| Deadline {
            off,
            sid: "s-x".into(),
            at: 9_000,
            dl: 60,
            to: None,
        };
        let asked = |off| Asked {
            off,
            to: "n-b".into(),
        };
        let cursors = |pairs: &[(&str, u64)]| -> BTreeMap<String, TopicCursor> {
            pairs
                .iter()
                .map(|(t, next)| ((*t).to_string(), TopicCursor { next: *next }))
                .collect()
        };
        let floors = |pairs: &[(&str, u64)]| -> BTreeMap<String, u64> {
            pairs
                .iter()
                .map(|(t, off)| ((*t).to_string(), *off))
                .collect()
        };
        let fill = |st: &StateDir| {
            st.set_seen_off("s-low", 3).unwrap();
            st.set_seen_off("s-high", 9).unwrap();
            st.set_topics("s-x", &cursors(&[("a", 2), ("b", 8)]))
                .unwrap();
            st.set_topic_floors("s-x", &floors(&[("a", 1), ("b", 7)]))
                .unwrap();
            st.set_topic_floors("s-y", &floors(&[("c", 6)])).unwrap();
            st.set_halt_acked(2).unwrap();
            st.set_inbox_owed(4).unwrap();
            st.set_deadlines(&[deadline(3), deadline(6)]).unwrap();
            st.set_asked(&[asked(3), asked(6)]).unwrap();
            st.set_key_pin("s-x", "low", &pin(Some(1))).unwrap();
            st.set_key_pin("s-x", "high", &pin(Some(6))).unwrap();
            st.set_in_flight(&in_flight(1, 2)).unwrap();
            st.set_in_flight(&in_flight(2, 7)).unwrap();
            st.set_self_acked(&[1, 6].into_iter().collect(), 8).unwrap();
        };
        let answer = |seq: u64| (seq, "/f/f/in/n-a/s-b/n-a/answer".to_string());

        let dir = scratch("clamp");
        let st = StateDir::open(&dir).unwrap();
        fill(&st);
        assert_eq!(
            st.forget_log_offsets(12, Some(new), Some(5)).unwrap(),
            vec![answer(2)],
            "only the in-flight reply to a record the loss took goes"
        );
        let clamped = |st: &StateDir| {
            assert_eq!(st.seen_off("s-low"), Some(3), "below the fork: kept");
            assert_eq!(st.seen_off("s-high"), Some(4), "at the fork");
            assert_eq!(st.topics("s-x"), cursors(&[("a", 2), ("b", 5)]));
            assert_eq!(st.topic_floors("s-x").unwrap(), floors(&[("a", 1)]));
            assert!(st.topic_floors("s-y").unwrap().is_empty());
            assert_eq!(st.halt_acked(), None, "cleared, clamped or not");
            assert_eq!(
                st.inbox_owed(),
                Some(4),
                "the commit owed is below the fork"
            );
            assert_eq!(st.deadlines(), [deadline(3)]);
            assert_eq!(st.asked(), [asked(3)]);
            assert_eq!(
                st.key_pin("s-x", "low").unwrap().unwrap().original_off,
                Some(1)
            );
            assert_eq!(
                st.key_pin("s-x", "high").unwrap().unwrap().original_off,
                None
            );
            assert_eq!(
                st.in_flight().unwrap(),
                vec![(
                    1,
                    InFlight {
                        log: Some(new),
                        ..in_flight(1, 2)
                    }
                )],
                "the reply below the fork is kept, RE-WITNESSED onto the new generation: the \
                 attach replays a reference only on the log its witness names"
            );
            assert_eq!(
                st.self_acked_log().offsets,
                [11].into_iter().collect(),
                "the window re-bases at the head"
            );
        };
        clamped(&st);
        assert!(
            st.forget_log_offsets(12, Some(new), Some(5))
                .unwrap()
                .is_empty(),
            "idempotent"
        );
        clamped(&st);
        // A fork above the head cannot be (the log grew from it): held to the head.
        st.set_topics("s-z", &cursors(&[("d", 30)])).unwrap();
        st.forget_log_offsets(12, Some(new), Some(40)).unwrap();
        assert_eq!(st.topics("s-z"), cursors(&[("d", 12)]));

        // NEGATIVE CONTROL: another log forgets every offset.
        let twin = scratch("clamp-ctl");
        let st = StateDir::open(&twin).unwrap();
        fill(&st);
        assert_eq!(
            st.forget_log_offsets(12, Some(new), None).unwrap(),
            vec![answer(1), answer(2)]
        );
        assert_eq!((st.seen_off("s-low"), st.seen_off("s-high")), (None, None));
        assert_eq!(st.topics("s-x"), cursors(&[("a", 12), ("b", 12)]));
        assert!(st.topic_floors("s-x").unwrap().is_empty());
        assert_eq!(st.halt_acked(), None);
        assert_eq!(st.inbox_owed(), None);
        assert!(st.deadlines().is_empty() && st.asked().is_empty());
        assert_eq!(
            st.key_pin("s-x", "low").unwrap().unwrap().original_off,
            None
        );
        assert!(st.in_flight().unwrap().is_empty());
        let _ = std::fs::remove_dir_all(&dir);
        let _ = std::fs::remove_dir_all(&twin);
    }

    /// A `post key=` pins its sequence to the key, first wins, and the table is
    /// bounded at [`KEYS_KEEP`]: the 4097th key evicts the oldest, and a re-post under
    /// an evicted key is a new record.
    #[test]
    fn a_key_pins_its_sequence_first_wins_and_the_4097th_evicts_the_oldest() {
        let dir = scratch("keys");
        let st = StateDir::open(&dir).expect("open");
        assert_eq!(st.key_seq("s-a", "k-1"), None);
        assert_eq!(st.set_key_seq("s-a", "k-1", 41).expect("reserve"), 41);
        assert_eq!(
            StateDir::open(&dir).expect("reopen").key_seq("s-a", "k-1"),
            Some(41),
            "the reservation survives a reopen"
        );
        // First wins: a second reservation answers the sequence the key holds.
        assert_eq!(st.set_key_seq("s-a", "k-1", 99).expect("re-reserve"), 41);
        assert_eq!(st.key_seq("s-a", "k-1"), Some(41));
        // Per session: another session's identical key is its own reservation.
        assert_eq!(st.key_seq("s-b", "k-1"), None);
        assert_eq!(st.set_key_seq("s-b", "k-1", 7).expect("reserve"), 7);
        assert_eq!(st.key_seq("s-a", "k-1"), Some(41));

        // The bound: seed a full table as one file (a line at a time is quadratic),
        // then add one more.
        let full: Vec<String> = (0..KEYS_KEEP).map(|n| format!("k-{n} {n}")).collect();
        st.write("keys/s-c", &full.join("\n"))
            .expect("seed a full table");
        assert_eq!(st.key_seq("s-c", "k-0"), Some(0));
        assert_eq!(
            st.key_seq("s-c", &format!("k-{}", KEYS_KEEP - 1)),
            Some(KEYS_KEEP as u64 - 1)
        );
        assert_eq!(
            st.set_key_seq("s-c", "k-newest", 1_000_000)
                .expect("the 4097th"),
            1_000_000
        );
        assert_eq!(st.key_seq("s-c", "k-0"), None, "the oldest key is evicted");
        assert_eq!(st.key_seq("s-c", "k-1"), Some(1), "the next-oldest stands");
        assert_eq!(st.key_seq("s-c", "k-newest"), Some(1_000_000));
        assert_eq!(st.key_lines("s-c").len(), KEYS_KEEP, "the table is capped");
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// The deadline table round-trips and is bounded; a damaged line reads as nothing
    /// (a half-parsed deadline would be a verdict for the wrong ask).
    #[test]
    fn deadlines_round_trip_are_bounded_and_a_damaged_line_reads_as_nothing() {
        let dir = scratch("deadlines");
        let st = StateDir::open(&dir).expect("open");
        assert!(st.deadlines().is_empty());
        let one = Deadline {
            off: 90_312,
            sid: "s-abcdef0123456789".to_string(),
            at: 1_700_000_240_123,
            dl: 240_000,
            to: Some("n-0123456789abcdef".to_string()),
        };
        st.set_deadlines(std::slice::from_ref(&one)).expect("write");
        assert_eq!(
            StateDir::open(&dir).expect("reopen").deadlines(),
            vec![one.clone()]
        );
        st.set_deadlines(&[]).expect("clear");
        assert!(st.deadlines().is_empty());
        for damaged in [
            "",
            "off=1 sid=s-a at=2",          // no dl
            "off=1 sid=s-a dl=3",          // no at
            "sid=s-a at=2 dl=3",           // no offset
            "off=1 at=2 dl=3",             // no sid
            "off=1 sid= at=2 dl=3",        // an empty sid
            "off=x sid=s-a at=2 dl=3",     // an offset that is not one
            "off=1 sid=s-a at=2 dl=3 to=", // an empty recipient
        ] {
            assert_eq!(Deadline::parse(damaged), None, "{damaged:?}");
        }
        assert_eq!(Deadline::parse(&one.render()), Some(one.clone()));
        // A line from before `to=` reads, with no recipient to hold replies to.
        assert_eq!(
            Deadline::parse("off=90312 sid=s-abcdef0123456789 at=1700000240123 dl=240000"),
            Some(Deadline { to: None, ..one })
        );
        // Bounded: the newest by position survive.
        let many: Vec<Deadline> = (0..DEADLINES_KEEP as u64 + 3)
            .map(|n| Deadline {
                off: n,
                sid: "s-a".to_string(),
                at: n,
                dl: 1,
                to: None,
            })
            .collect();
        st.set_deadlines(&many).expect("write many");
        let kept = st.deadlines();
        assert_eq!(kept.len(), DEADLINES_KEEP);
        assert_eq!(kept[0].off, 3, "the three oldest are gone");
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// The asked table round-trips and is bounded; a damaged line reads as nothing (a
    /// misread recipient would hold a real receipt as a stray).
    #[test]
    fn the_asked_table_round_trips_and_is_bounded() {
        let dir = scratch("asked");
        let st = StateDir::open(&dir).expect("open");
        assert!(st.asked().is_empty());
        let two = vec![
            Asked {
                off: 7,
                to: "n-0123456789abcdef".to_string(),
            },
            Asked {
                off: 9,
                to: "h-andrew".to_string(),
            },
        ];
        st.set_asked(&two).expect("write");
        assert_eq!(StateDir::open(&dir).expect("reopen").asked(), two);
        for damaged in ["", "7", "x n-a", "7 n-a extra"] {
            assert_eq!(Asked::parse(damaged), None, "{damaged:?}");
        }
        let many: Vec<Asked> = (0..ASKED_KEEP as u64 + 2)
            .map(|off| Asked {
                off,
                to: "n-a".to_string(),
            })
            .collect();
        st.set_asked(&many).expect("write many");
        let kept = st.asked();
        assert_eq!(kept.len(), ASKED_KEEP);
        assert_eq!(kept[0].off, 2, "the two oldest are gone");
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// The pin table is bounded, oldest first sight out, without disturbing what it
    /// keeps.
    #[test]
    fn the_pin_table_is_bounded_and_evicts_the_oldest_first_sight() {
        let dir = scratch("pins-bound");
        let st = StateDir::open(&dir).expect("open");
        // A full table, written as one file: filling it via `pin` would be
        // quadratic.
        let full: Vec<String> = (0..PINS_KEEP)
            .map(|n| format!("s-{n:016x} n-{:016x}", n % 3))
            .collect();
        st.write("pins", &full.join("\n"))
            .expect("seed a full table");
        assert_eq!(st.pins().len(), PINS_KEEP);
        for n in PINS_KEEP..PINS_KEEP + 16 {
            st.pin(&format!("s-{n:016x}"), &format!("n-{:016x}", n % 3))
                .expect("pin");
        }
        let pins = st.pins();
        assert_eq!(pins.len(), PINS_KEEP, "the table is capped");
        // The sixteen oldest first sights are gone; everything newer stands.
        for n in 0..16 {
            assert!(!pins.contains_key(&format!("s-{n:016x}")), "{n} evicted");
        }
        for n in 16..PINS_KEEP + 16 {
            assert_eq!(
                pins.get(&format!("s-{n:016x}")),
                Some(&format!("n-{:016x}", n % 3)),
                "{n} kept, with its node"
            );
        }
        // First-wins holds across the bound: re-pinning a survivor writes nothing,
        // so asking cannot provoke an eviction.
        let survivor = format!("s-{:016x}", PINS_KEEP);
        assert_eq!(
            st.pin(&survivor, "n-rogue").expect("re-pin"),
            format!("n-{:016x}", PINS_KEEP % 3)
        );
        assert_eq!(st.pins().len(), PINS_KEEP);
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// A second take of the bridge lock is `WouldBlock` naming the `pid` file's pid,
    /// and the dir is free once the hold drops. `flock` conflicts between two open
    /// file descriptions in one process, so this needs no second process.
    #[test]
    fn the_bridge_lock_has_one_holder_until_it_drops() {
        let dir = scratch("lock");
        let st = StateDir::open(&dir).expect("open");
        let held = st.lock_bridge().expect("the first take");
        std::fs::write(dir.join("pid"), "4242\n").expect("pid");
        let refused = st.lock_bridge().err().expect("a second take is refused");
        assert_eq!(refused.kind(), io::ErrorKind::WouldBlock);
        assert!(refused.to_string().contains("(pid 4242)"), "{refused}");
        drop(held);
        let _again = st.lock_bridge().expect("free once the holder let go");
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// A missing directory is empty; a dangling directory entry is unreadable
    /// state and cannot authorize finishing a replacement log's reset.
    #[cfg(unix)]
    #[test]
    fn a_log_switch_refuses_a_dangling_offset_directory() {
        for blocked in ["seen", "topics"] {
            let dir = scratch("dangling-offsets");
            let st = StateDir::open(&dir).unwrap();
            let area = dir.join(blocked);
            if area.is_dir() {
                std::fs::remove_dir(&area).unwrap();
            }
            std::os::unix::fs::symlink(dir.join("missing"), &area).unwrap();
            assert!(
                st.forget_log_offsets(20, None, None).is_err(),
                "unreadable {blocked} was empty"
            );
            assert!(area.is_symlink(), "the unreadable directory entry stays");
            std::fs::remove_file(&area).unwrap();
            assert!(
                st.forget_log_offsets(20, None, None).is_ok(),
                "an absent directory is empty"
            );
            let _ = std::fs::remove_dir_all(&dir);
        }
    }

    /// The log witness round-trips with and without an identity, reads past a
    /// token (or an `id=`) it cannot parse, and names another log by a different
    /// identity at any head or by a head below its high-water. An identity missing
    /// on either side says nothing.
    #[test]
    fn the_log_witness_round_trips_and_names_another_log_by_identity_or_head() {
        use astream_broker::LogIdentity;
        let a = LogIdentity {
            id: [0xaa; 16],
            generation: 0,
        };
        let a_rewritten = LogIdentity { generation: 1, ..a };
        let b = LogIdentity {
            id: [0xbb; 16],
            generation: 0,
        };
        let dir = scratch("witness");
        let st = StateDir::open(&dir).expect("open");
        assert_eq!(st.log_witness(), None, "nothing seen yet");
        st.set_log_witness(&LogWitness {
            high: 500,
            id: None,
        })
        .expect("write");
        let w = st.log_witness().expect("read back");
        assert_eq!(
            w,
            LogWitness {
                high: 500,
                id: None
            }
        );
        assert!(w.replaced_by(499, None) && w.replaced_by(499, Some(a)));
        assert!(!w.replaced_by(500, None) && !w.replaced_by(10_000, Some(a)));
        let with_id = LogWitness {
            high: 500,
            id: Some(a),
        };
        st.set_log_witness(&with_id).expect("write");
        assert_eq!(st.log_witness(), Some(with_id));
        // Another identity is another log, longer or not, and so is the same
        // log's next generation, whose offsets went to other records.
        assert!(with_id.replaced_by(10_000, Some(b)));
        assert!(with_id.replaced_by(10_000, Some(a_rewritten)));
        assert!(!with_id.replaced_by(10_000, Some(a)));
        assert!(
            !with_id.replaced_by(10_000, None),
            "no identity says nothing"
        );
        assert!(
            with_id.replaced_by(499, Some(a)),
            "a head below is evidence too"
        );
        assert_eq!(
            LogWitness::parse("id=00ff high=7"),
            Some(LogWitness { high: 7, id: None })
        );
        assert_eq!(LogWitness::parse("high=+7"), None);
        assert_eq!(LogWitness::parse("id=00ff"), None);
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// Standing halts round-trip per fleet with their kept/witnessed flag, a damaged
    /// line is no halt, and clearing the halt watermark is idempotent.
    #[test]
    fn standing_halts_round_trip_per_fleet_and_a_damaged_line_is_nothing() {
        let dir = scratch("halts");
        let st = StateDir::open(&dir).expect("open");
        assert!(st.standing_halts("f").is_empty());
        let halts = BTreeMap::from([
            (
                "h-a".to_string(),
                StandingHalt {
                    reason: "main%20broken".to_string(),
                    kept: false,
                },
            ),
            (
                "h-b".to_string(),
                StandingHalt {
                    reason: "fleet-halt".to_string(),
                    kept: true,
                },
            ),
        ]);
        st.set_standing_halts("f", &halts).expect("write");
        assert_eq!(st.standing_halts("f"), halts);
        assert!(st.standing_halts("g").is_empty(), "another fleet's halts");
        std::fs::write(
            dir.join("halts"),
            "f h-a r witnessed\nf h-b r maybe\nf n-node r kept\nf h-c\n",
        )
        .expect("write");
        assert_eq!(
            st.standing_halts("f").into_keys().collect::<Vec<_>>(),
            ["h-a"]
        );
        st.set_standing_halts("f", &BTreeMap::new()).expect("empty");
        assert!(st.standing_halts("f").is_empty());

        st.set_halt_acked(9).expect("ack");
        st.clear_halt_acked().expect("clear");
        assert_eq!(st.halt_acked(), None);
        st.clear_halt_acked().expect("clearing nothing is no error");
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// A blank or garbage file reads as "nothing known", never a panic.
    #[test]
    fn a_corrupt_file_reads_as_unknown() {
        let dir = scratch("corrupt");
        let st = StateDir::open(&dir).expect("open");
        std::fs::write(dir.join("seq"), "   \n").expect("write");
        assert_eq!(st.sequence(), 0);
        std::fs::write(dir.join("inc"), "not-a-number").expect("write");
        assert_eq!(st.incarnation(), 0);
        let _ = std::fs::remove_dir_all(&dir);
    }

    // -----------------------------------------------------------------------
    // the incarnation lease
    // -----------------------------------------------------------------------

    /// The boot id standing in for boot number `b` (never the all-zero "unknown").
    fn boot_id(b: u8) -> [u8; 16] {
        [b; 16]
    }

    /// A seam that names this boot, kills nowhere and carries no defect.
    fn in_boot(b: u8) -> IncSeam {
        IncSeam {
            boot: Some(Some(boot_id(b))),
            ..IncSeam::default()
        }
    }

    /// The `inc` file standing in for file number `n` of one dir.
    fn inc_file_id(n: u64) -> Option<IncFile> {
        Some(IncFile {
            dev: 1,
            dir: 2,
            ino: n,
        })
    }

    /// `inc-lease` RENDERS AND PARSES BACK, and anything but a whole lease is no lease:
    /// a torn, hand-edited or duplicated field, `used` above `top`, a boot that is not a
    /// boot id (the all-zero one included), or a file that is not `<dev>:<dir>:<ino>`
    /// reads as `None`, which flushes.
    #[test]
    fn an_incarnation_lease_round_trips_and_anything_torn_is_no_lease() {
        for lease in [
            IncLease {
                used: 3,
                top: 66,
                boot: Some(boot_id(7)),
                file: Some(IncFile {
                    dev: u64::MAX,
                    dir: 0,
                    ino: 12_345,
                }),
            },
            IncLease {
                used: 5,
                top: 5,
                boot: None,
                file: None,
            },
        ] {
            assert_eq!(IncLease::parse(&lease.render()), Some(lease), "{lease:?}");
        }
        let id = aterm_codec::hex::encode_lower(&boot_id(7));
        for torn in [
            String::new(),
            "used=3 top=66".to_string(),
            format!("used=3 boot={id} file=1:2:3"),
            format!("used=3 top=66 boot={id}"),
            format!("used=3 top=66 boot={} file=1:2:3", &id[..31]),
            format!("used=3 top=66 boot={} file=1:2:3", "0".repeat(32)),
            format!("used=67 top=66 boot={id} file=1:2:3"),
            format!("used=3 used=4 top=66 boot={id} file=1:2:3"),
            format!("used=3 top=66 boot={id} file=1:2:3 extra=1"),
            format!("used=x top=66 boot={id} file=1:2:3"),
            format!("used=3 top=66 boot={id} file=1:2"),
            format!("used=3 top=66 boot={id} file=1:2:3:4"),
            format!("used=3 top=66 boot={id} file=1::3"),
            format!("used=3 top=66 boot={id} file=1:2:3 file=-"),
        ] {
            assert_eq!(IncLease::parse(&torn), None, "{torn:?}");
        }
    }

    /// THE PLAN, case by case: a lease this boot wrote, whose top is `inc`'s and whose
    /// file is the `inc` there now, answers with no flush while it has room and leases
    /// afresh above itself once it runs out; any other boot's lease, a lease whose top is
    /// not `inc`'s, a lease naming another file (a copy's) or none, no lease, or a boot
    /// nobody can name takes the incarnation above `inc` and flushes; `seq`'s incarnation
    /// and the bus's both count; a boot nobody can name leases one; the space ends at
    /// `INC_MAX`.
    #[test]
    fn the_incarnation_plan_trusts_only_this_boots_lease_and_counts_seq_and_the_bus() {
        let (b1, b2) = (Some(boot_id(1)), Some(boot_id(2)));
        let f = inc_file_id(1);
        let lease = |used, top, boot| {
            Some(IncLease {
                used,
                top,
                boot,
                file: f,
            })
        };
        let plan = |next, top, flush| Some(IncPlan { next, top, flush });
        let span = INC_LEASE;
        // This boot's lease with room: the next one, unflushed.
        assert_eq!(
            plan_incarnation(66, f, lease(3, 66, b1), 3, b1, 0, span),
            plan(4, 66, false)
        );
        // Run out: leased afresh above it.
        assert_eq!(
            plan_incarnation(66, f, lease(66, 66, b1), 66, b1, 0, span),
            plan(67, 66 + span, true)
        );
        // Another boot's lease: everything up to `inc` counts as taken.
        assert_eq!(
            plan_incarnation(66, f, lease(3, 66, b1), 3, b2, 0, span),
            plan(67, 66 + span, true)
        );
        // A top that is not `inc`'s (an interrupted flush renamed in, or an older build's
        // `inc + 1`): not trusted.
        assert_eq!(
            plan_incarnation(130, f, lease(3, 66, b1), 3, b1, 0, span),
            plan(131, 130 + span, true)
        );
        // Another `inc` file than the lease names (a copy of the dir, a restored `inc`),
        // no `inc` file at all, or a lease that names none: not trusted.
        for (file, named) in [(inc_file_id(2), f), (None, f), (f, None), (None, None)] {
            let other = Some(IncLease {
                used: 3,
                top: 66,
                boot: b1,
                file: named,
            });
            assert_eq!(
                plan_incarnation(66, file, other, 3, b1, 0, span),
                plan(67, 66 + span, true),
                "inc {file:?}, lease naming {named:?}"
            );
        }
        // No lease at all (a dir an older build kept): `inc` was the incarnation.
        assert_eq!(
            plan_incarnation(9, f, None, 9, b1, 0, span),
            plan(10, 9 + span, true)
        );
        // `seq`'s incarnation counts: an older build's doctor published under the top.
        assert_eq!(
            plan_incarnation(66, f, lease(3, 66, b1), 66, b1, 0, span),
            plan(67, 66 + span, true)
        );
        // So does the bus's: a copied dir's sibling ran ahead.
        assert_eq!(
            plan_incarnation(66, f, lease(3, 66, b1), 3, b1, 90, span),
            plan(91, 90 + span, true)
        );
        // A boot nobody can name trusts no lease and leases ONE: `inc` is then exactly
        // the incarnation taken, as the file always was.
        assert_eq!(
            plan_incarnation(66, f, lease(3, 66, None), 3, None, 0, span),
            plan(67, 67, true)
        );
        // The space ends at INC_MAX: the top is clamped, and past it is refused.
        assert_eq!(
            plan_incarnation(INC_MAX - 1, f, None, 0, b1, 0, span),
            plan(INC_MAX, INC_MAX, true)
        );
        assert_eq!(plan_incarnation(INC_MAX, f, None, 0, b1, 0, span), None);
        assert_eq!(plan_incarnation(0, None, None, 0, b1, INC_MAX, span), None);
        assert_eq!(
            plan_incarnation(0, None, None, u64::MAX >> 32, b1, 0, span),
            None
        );
    }

    /// WHAT AN ATTACH COSTS, as a count: in one boot, one lease of `INC_LEASE` attaches
    /// is ONE flushed write — two `fsync`s (`F_FULLFSYNC` on macOS, 8–12 ms measured in
    /// place on this machine) — and every other attach is one atomic replace; the attach
    /// after the lease runs out flushes again. A new boot flushes at its first attach,
    /// however much of the last lease was left, and a boot nobody can name flushes every
    /// time, as every attach did before the lease.
    ///
    /// Negative control: the old `set_incarnation` flushed on every take, `2 * INC_LEASE`
    /// fsyncs where this counts 2.
    #[test]
    fn a_lease_of_attaches_costs_one_flush_and_a_new_boot_flushes_once() {
        let dir = scratch("inclease-cost");
        let st = StateDir::open(&dir).expect("open");
        st.set_inc_seam(in_boot(1));
        let take = |n: u64| -> Vec<u64> {
            (0..n)
                .map(|_| st.take_incarnation(0).expect("take"))
                .collect()
        };
        let before = st.ops();
        let mut taken = take(3);
        // MID-LEASE, where what was taken and the lease's top differ (3 and 64): the
        // doctor's answer is the one taken (`the_doctor_never_answers_a_lost_leases_unused_top`).
        assert_eq!(
            (st.incarnation(), st.incarnation_top()),
            (3, INC_LEASE),
            "the doctor's answer is what was taken, not the top"
        );
        taken.extend(take(INC_LEASE - 3));
        let lease = st.ops() - before;
        assert_eq!(
            taken,
            (1..=INC_LEASE).collect::<Vec<_>>(),
            "one apart, from 1"
        );
        assert_eq!(
            (lease.fsyncs, lease.replaces),
            (2, INC_LEASE + 1),
            "one flushed `inc`, then one `inc-lease` replace per attach: {lease:?}"
        );
        let before = st.ops();
        assert_eq!(st.take_incarnation(0).expect("take"), INC_LEASE + 1);
        assert_eq!(
            (st.ops() - before).fsyncs,
            2,
            "the lease ran out: flushed again"
        );

        // A NEW BOOT: the lease has 63 incarnations left, and none of them is trusted.
        st.set_inc_seam(in_boot(2));
        let before = st.ops();
        assert_eq!(
            st.take_incarnation(0).expect("take"),
            2 * INC_LEASE + 1,
            "above the whole lease"
        );
        assert_eq!((st.ops() - before).fsyncs, 2);
        let before = st.ops();
        assert_eq!(st.take_incarnation(0).expect("take"), 2 * INC_LEASE + 2);
        assert_eq!((st.ops() - before).fsyncs, 0, "and then trusted again");

        // A BOOT NOBODY CAN NAME: every take flushes, and leases one.
        st.set_inc_seam(IncSeam {
            boot: Some(None),
            ..IncSeam::default()
        });
        for _ in 0..3 {
            let before = st.ops();
            let inc = st.take_incarnation(0).expect("take");
            assert_eq!((st.ops() - before).fsyncs, 2);
            assert_eq!(st.lease().map(|l| (l.used, l.top)), Some((inc, inc)));
        }
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// **THE DOCTOR NEVER PUBLISHES UNDER A LEASE'S UNUSED TOP**, not even once a power
    /// loss took the lease that named what was used. `inc` holds the top of a block of
    /// `INC_LEASE` and was flushed; `inc-lease`, which says which incarnation of the
    /// block the last attach took, was not, and the dir's FIRST lease has no older
    /// version for the media to fall back on. A doctor that answered `inc` there would
    /// reserve `(64 << 32) + 1`, raise the producer's high water above incarnation 3's
    /// will (`(3 << 32) | WILL_SEQ_LOW`) and fence it: the dead node's row would stay
    /// `live`, and the next attach would never witness incarnation 3 dead.
    ///
    /// What remains answers: `seq`'s incarnation, which a bridge reserves under after its
    /// take, so it is never above what was used. Without that either, nothing is known,
    /// and the answer is 0: the doctor's sequence is then a fresh number below every will
    /// (accepted, fencing nothing) or one used before (a dedup, which it refuses).
    ///
    /// Negative control: `incarnation()` falling back to `inc` when there is no whole
    /// lease answers 64 at the first assertion after the power loss.
    #[test]
    fn the_doctor_never_answers_a_lost_leases_unused_top() {
        let dir = scratch("inclease-doctor");
        let st = StateDir::open(&dir).expect("open");
        st.set_inc_seam(in_boot(1));
        // Boot 1: the dir's first lease, and three attaches, each reserving a block as
        // the bridge does once its will is registered.
        for want in 1..=3 {
            let inc = st.take_incarnation(0).expect("take");
            assert_eq!(inc, want);
            st.reserve_sequence_block(inc, 16).expect("a block");
        }
        // MID-LEASE the doctor's answer is what was taken, never the top: used 3, top 64.
        assert_eq!(
            (st.incarnation(), st.incarnation_top()),
            (3, INC_LEASE),
            "the doctor answers the incarnation taken, not the lease's top"
        );
        let will = |inc: u64| (inc << 32) | WILL_SEQ_LOW;

        // THE POWER LOSS: the flushed `inc` survives, the never-flushed first lease does
        // not, and `seq` was written back.
        std::fs::remove_file(dir.join("inc-lease")).expect("lose the lease");
        assert_eq!(
            st.incarnation(),
            3,
            "seq's incarnation, never the lost lease's unused top ({INC_LEASE})"
        );
        // A torn lease is no lease, and answers the same.
        std::fs::write(dir.join("inc-lease"), "used=3 top=").expect("tear the lease");
        assert_eq!(st.incarnation(), 3, "a torn lease");
        // `seq` rolled back to incarnation 1's block: at or below the one used.
        std::fs::write(dir.join("seq"), ((1u64 << 32) + 16).to_string()).expect("seq");
        assert_eq!(st.incarnation(), 1, "a rolled-back seq");
        // Both gone: nothing is known, and the doctor's sequence fences no will.
        std::fs::remove_file(dir.join("seq")).expect("lose seq");
        assert_eq!(st.incarnation(), 0, "nothing known");
        let seq = st
            .reserve_next_sequence(st.incarnation())
            .expect("a sequence");
        assert!(
            seq < will(1),
            "the doctor's sequence {seq:#x} is below every will this node registered"
        );

        // AN OLDER BUILD'S DIR (no lease ever; `inc` was the incarnation, and its attach
        // reserved under it): the doctor answers that incarnation, as it always did.
        std::fs::remove_file(dir.join("inc-lease")).expect("no lease");
        std::fs::write(dir.join("inc"), "9").expect("inc");
        std::fs::write(dir.join("seq"), ((9u64 << 32) + 5).to_string()).expect("seq");
        assert_eq!(st.incarnation(), 9, "an older build's dir");
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// **A COPIED STATE DIR FLUSHES ITS OWN `inc` BEFORE IT TRUSTS A LEASE.** A copy in
    /// the same boot (`cp -R`, a Finder duplicate, `rsync`, a backup restored) carries a
    /// lease whose boot is this one and whose top equals the copied `inc` — but that top
    /// was flushed in the ORIGINAL dir; the copy's `inc` is a new file the copy wrote
    /// unflushed. Trusted, every attach of the copy would present an incarnation no
    /// flushed `inc` covers, and a power loss would let the next boot take one of them
    /// again (the bus's newest row is one short when the last `live` had not landed),
    /// whose old will then fires unfenced over the new `live`. So the lease names the
    /// very `inc` file and directory it vouches for, and a copy's are other files.
    ///
    /// Negative control: a lease trusted on boot and top alone takes 6 in the copy with
    /// no flush.
    #[test]
    fn a_copied_state_dir_flushes_its_own_inc_before_trusting_a_lease() {
        let (a, c) = (scratch("inclease-copy-a"), scratch("inclease-copy-c"));
        let original = StateDir::open(&a).expect("open the original");
        original.set_inc_seam(in_boot(1));
        for want in 1..=5 {
            assert_eq!(original.take_incarnation(0).expect("take"), want);
        }
        // THE COPY, in the same boot: the same bytes in new files.
        let copy = StateDir::open(&c).expect("open the copy");
        for file in ["inc", "inc-lease", "seq"] {
            if a.join(file).exists() {
                std::fs::copy(a.join(file), c.join(file)).expect("copy");
            }
        }
        copy.set_inc_seam(in_boot(1));
        let before = copy.ops();
        assert_eq!(
            copy.take_incarnation(0).expect("take"),
            INC_LEASE + 1,
            "above the copied lease's whole top"
        );
        assert_eq!(
            (copy.ops() - before).fsyncs,
            2,
            "the copy's own `inc` is flushed before its first incarnation is presented"
        );
        let before = copy.ops();
        assert_eq!(copy.take_incarnation(0).expect("take"), INC_LEASE + 2);
        assert_eq!(
            (copy.ops() - before).fsyncs,
            0,
            "and then its own lease is trusted"
        );
        // The original's lease still names the file it flushed: no flush there.
        let before = original.ops();
        assert_eq!(original.take_incarnation(0).expect("take"), 6);
        assert_eq!(
            (original.ops() - before).fsyncs,
            0,
            "the original keeps its lease"
        );
        let _ = std::fs::remove_dir_all(&a);
        let _ = std::fs::remove_dir_all(&c);
    }

    /// **THE SHIPPED TRUST DECISION LEASES ON THIS MACHINE**: with no seam, the kernel's
    /// boot session and the dir's own device decide (`trusted_boot`), and on macOS — the
    /// build whose `F_FULLFSYNC` the lease exists to spare — they must vouch for a dir
    /// on the boot volume's data volume (where `~` and the temp dir live), so five
    /// attaches cost ONE flushed write. Every other lease test stands its boot in; this
    /// is the one that holds the speed-up to the path that ships.
    ///
    /// Negative control: `trusted_boot` answering `None` (`this_boot` failing, or
    /// `on_boot_volume` refusing the data volume) makes the five takes ten fsyncs.
    #[test]
    fn the_shipped_trust_decision_leases_a_dir_on_the_boot_volume() {
        let dir = scratch("inclease-shipped");
        let st = StateDir::open(&dir).expect("open");
        let boot = crate::durability::this_boot();
        let on_boot = on_boot_volume(&dir);
        if cfg!(target_vendor = "apple") {
            assert!(boot.is_some(), "macOS names its boot session");
            assert!(on_boot, "{dir:?} is on the boot volume");
        }
        let before = st.ops();
        let taken: Vec<u64> = (0..5)
            .map(|_| st.take_incarnation(0).expect("take"))
            .collect();
        assert_eq!(taken, [1, 2, 3, 4, 5]);
        let fsyncs = (st.ops() - before).fsyncs;
        eprintln!(
            "MEASURED five shipped takes: {fsyncs} fsyncs (boot {boot:?}, on boot volume {on_boot})"
        );
        let want = if boot.is_some() && on_boot { 2 } else { 10 };
        assert_eq!(fsyncs, want, "five takes on the shipped trust decision");
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// The model's page-cache files laid down in `st`'s dir: `inc` (`c_inc`), `inc-lease`
    /// (`c_used`, `c_top`, `c_lb`: none when `c_lb` is 0) and `seq` (the end of a block
    /// of incarnation `c_seq`: none when it is 0). The model is one dir whose `inc` is
    /// the file every lease was flushed into (a copy of the dir is
    /// `a_copied_state_dir_flushes_its_own_inc_before_trusting_a_lease`'s), so the lease
    /// names the `inc` laid down.
    fn lay_down(st: &StateDir, s: &aterm_spec::interp::State) {
        let v = |name: &str| u64::try_from(s[name]).expect("a model value is not negative");
        for file in ["inc", "inc-lease", "seq"] {
            let _ = std::fs::remove_file(st.path(file));
        }
        if v("c_inc") > 0 {
            std::fs::write(st.path("inc"), v("c_inc").to_string()).expect("inc");
        }
        if v("c_lb") > 0 {
            let lease = IncLease {
                used: v("c_used"),
                top: v("c_top"),
                boot: Some(boot_id(u8::try_from(v("c_lb")).expect("a small boot"))),
                file: st.inc_file(),
            };
            std::fs::write(st.path("inc-lease"), lease.render()).expect("inc-lease");
        }
        if v("c_seq") > 0 {
            std::fs::write(st.path("seq"), ((v("c_seq") << 32) + 64).to_string()).expect("seq");
        }
    }

    /// The files read back as the model's page-cache variables
    /// `(c_inc, c_used, c_top, c_lb, c_seq)`.
    fn read_back(st: &StateDir) -> [i64; 5] {
        let int = |n: u64| i64::try_from(n).expect("a small value");
        let (used, top, lb) = st.lease().map_or((0, 0, 0), |l| {
            let boot = l.boot.expect("the walk always names its boot");
            assert!(boot.iter().all(|b| *b == boot[0]), "a stand-in boot id");
            (l.used, l.top, u64::from(boot[0]))
        });
        [
            int(st.incarnation_top()),
            int(used),
            int(top),
            int(lb),
            int(st.sequence() >> 32),
        ]
    }

    /// Run the REAL take from every planned state `model` reaches, at each kill point,
    /// with `mutant`'s defect, and name every one whose files are not the model's
    /// successor (or whose answer is not the model's `pn`). A kill point the take does
    /// not reach (a take that does not flush) must complete it as the model's `Take`
    /// does; a state whose flush the model's bound cuts off is not compared.
    fn diverging_takes(model: &aterm_spec::derive::Model, mutant: u8) -> (usize, Vec<String>) {
        let span = model
            .consts
            .iter()
            .find(|(name, _)| *name == "Span")
            .map(|(_, v)| u64::try_from(*v).expect("a positive span"))
            .expect("the model declares Span");
        let dir = scratch(&format!("inclease-walk-{mutant}"));
        let st = StateDir::open(&dir).expect("open");
        let mut seen = std::collections::BTreeSet::new();
        let (mut compared, mut diverged) = (0, Vec::new());
        for s in aterm_spec::interp::reachable_states(model) {
            let key = ["boot", "c_inc", "c_used", "c_top", "c_lb", "c_seq", "pc"].map(|k| s[k]);
            if s["pc"] != 1 || !seen.insert(key) {
                continue;
            }
            let completed = ["Take", "TakeFlushed"]
                .iter()
                .find_map(|a| model.successors(a, &s).into_iter().next());
            for (kill, action) in [
                (None, None),
                (Some(IncStep::Renamed), Some("KillRenamed")),
                (Some(IncStep::Flushed), Some("KillFlushed")),
            ] {
                let killed = action.and_then(|a| model.successors(a, &s).into_iter().next());
                let (expect, dies) = match (killed, &completed) {
                    (Some(k), _) => (k, true),
                    (None, Some(c)) => (c.clone(), false),
                    (None, None) => continue, // the model's bound, not a step
                };
                if kill.is_some() && !dies && s["tr"] == 0 {
                    continue; // a flush the bound cut off is no kill point
                }
                lay_down(&st, &s);
                st.set_inc_seam(IncSeam {
                    boot: Some(Some(boot_id(u8::try_from(s["boot"]).expect("small")))),
                    kill_at: kill,
                    mutant,
                    span: Some(span),
                    simulated_media: true,
                });
                let got = st.take_incarnation(0);
                let files = read_back(&st);
                let want = ["c_inc", "c_used", "c_top", "c_lb", "c_seq"].map(|k| expect[k]);
                let answer_ok = match (&got, dies) {
                    (Err(_), true) => true,
                    (Ok(inc), false) => i64::try_from(*inc).ok() == Some(s["pn"]),
                    _ => false,
                };
                compared += 1;
                if !answer_ok || files != want {
                    diverged.push(format!(
                        "from {s:?} killed at {kill:?}: took {got:?} (model pn {}), files \
                         {files:?}, model {want:?}",
                        s["pn"]
                    ));
                }
            }
        }
        let _ = std::fs::remove_dir_all(&dir);
        (compared, diverged)
    }

    /// **THE INCARNATION LEASE REFINES ITS MODEL, at every kill point, from every state
    /// a run of kills, power losses, write-backs and older builds' attaches reaches.**
    ///
    /// `aterm-models-session`'s `IncarnationLease` proves, over its whole bounded space,
    /// that a trusted lease names a durable top, that every presented incarnation is
    /// covered on the media, and that none is presented twice (Tier-0). This runs the
    /// shipped `take_incarnation` from each distinct planned state the model reaches —
    /// the files laid down as the model's page cache, this boot stood in, killed after
    /// the flush's rename, after the flush, or not at all — and requires the files it
    /// leaves and the incarnation it answers to be the model's successor. A power loss is
    /// the model's own step (the files it leaves are the model's media), so the take is
    /// held to the model in every boot the walk reaches.
    ///
    /// Each defect the model was written to catch (`Buggy` 1–3) is the take's own mutant
    /// of the same number; the defective take refines the defective model, which the
    /// Tier-0 suite shows breaks the fence. NEGATIVE CONTROL: the committed model against
    /// each defective take diverges, so the walk is not blind to the defects.
    /// `Buggy = 4` (the will sent while the flush is in flight) is the bridge's order,
    /// not the take's: its files are the committed ones, and `bring_presence_up` calls
    /// the will only after `take_incarnation` returned.
    #[test]
    fn the_incarnation_lease_refines_its_model() {
        let committed = aterm_models_session::incarnation_lease_model();
        for buggy in 0..=3u8 {
            // A defective machine keeps going past its first duplicate, so its space is
            // walked over one power loss (every Tier-0 counterexample needs one), the
            // committed machine's over the committed two.
            let model = match buggy {
                0 => committed.clone(),
                _ => aterm_spec::interp::with_consts(
                    &committed,
                    &[("Buggy", i64::from(buggy)), ("MaxBoot", 2)],
                ),
            };
            let (compared, diverged) = diverging_takes(&model, buggy);
            eprintln!("IncarnationLease Buggy = {buggy}: {compared} takes held to the model");
            assert!(
                compared > 100,
                "Buggy = {buggy}: only {compared} takes compared"
            );
            assert!(
                diverged.is_empty(),
                "Buggy = {buggy}: the take is not the model's in {} of {compared}:\n{}",
                diverged.len(),
                diverged.join("\n")
            );
        }
        // Over one power loss and a higher bound: defect 2 shows only on the retry of a
        // run-out lease, whose committed flush lands above the committed bound.
        let roomier = aterm_spec::interp::with_consts(&committed, &[("MaxTop", 7), ("MaxBoot", 2)]);
        for mutant in 1..=3u8 {
            let (compared, diverged) = diverging_takes(&roomier, mutant);
            assert!(
                !diverged.is_empty(),
                "the take's defect {mutant} matched the committed model in all {compared} takes"
            );
        }
    }

    /// The REAL reader's resume point from a `FabricReaderResume` state: the state's
    /// lineage — the model's two slots, `(g - 1, f2)` and `(g, f1)` where they exist — is
    /// built through astream's own `Hello` parser ([`crate::durability::lineage_of`]), the
    /// witness names generation `rg` of one log, [`LogWitness::continues`] answers where
    /// it continues on generation `g`, and [`StateDir::forget_log_offsets`] clamps (or
    /// forgets) a topic cursor at `c` against head `h`. Answers the cursor it leaves.
    /// `rebase_at_head` is the NEGATIVE CONTROL: the reader before lineage, which reads
    /// no continuation and forgets.
    fn resume_through(
        st: &StateDir,
        s: &std::collections::BTreeMap<&'static str, i64>,
        top: i64,
        rebase_at_head: bool,
    ) -> i64 {
        let at = |name: &str| u64::try_from(s[name]).expect("a model offset");
        let (g, rg, h, c) = (at("g"), at("rg"), at("h"), at("c"));
        let mut steps = Vec::new();
        if g >= 2 && s["f2"] != top {
            steps.push((g - 1, at("f2")));
        }
        if g >= 1 && s["f1"] != top {
            steps.push((g, at("f1")));
        }
        let lineage = crate::durability::lineage_of(&steps).expect("a lineage astream reads");
        let log = |generation| astream_broker::LogIdentity {
            id: [0x5a; 16],
            generation,
        };
        let witness = LogWitness {
            high: c,
            id: Some(log(rg)),
        };
        let keep_below = if rebase_at_head {
            None
        } else {
            witness.continues(Some(log(g)), Some(&lineage))
        };
        let mut cursors = BTreeMap::new();
        cursors.insert("t".to_string(), TopicCursor { next: c });
        st.set_topics("s-r", &cursors).expect("write the cursor");
        st.forget_log_offsets(h, Some(log(g)), keep_below)
            .expect("forget or clamp");
        i64::try_from(st.topics("s-r")["t"].next).expect("an offset")
    }

    /// TIER-1 OF `FabricReaderResume` (`aterm-models-session`), the reader-resume law:
    /// from EVERY state the model reaches in which the reader decides where to resume, the
    /// real reader ([`resume_through`]: the real `LogWitness::continues` over the state's
    /// lineage, then the real `StateDir::forget_log_offsets`) must leave the cursor the
    /// model's `Decide` picks — the projection is the state with `r` the real cursor and
    /// the decision taken, which the model must admit as its `Decide` step — and the
    /// model's `Resume` from there keeps both laws. The real reader's answer depends only
    /// on `(g, rg, h, c, f1, f2)`, so each distinct one is run once.
    ///
    /// NEGATIVE CONTROLS. The reader that re-bases at the head (the rule before lineage)
    /// is rejected by the committed model; and the real reader matches none of the
    /// defective models (`Buggy` 1–3: resume at the head, at the newest fork only, a
    /// continuation trusted beyond the window), so the walk is not blind to the defects
    /// the registry sweep (`aterm-models`' `non_vacuity`) shows break the law. Its
    /// lineages have at most two steps; astream's own window is run end to end in
    /// `durability.rs`'s `a_reader_older_than_the_lineage_window_forgets`.
    #[test]
    fn the_reader_resume_refines_its_model() {
        use aterm_spec::interp;
        let committed = aterm_models_session::fabric_reader_resume_model();
        let top = committed
            .consts
            .iter()
            .find(|(name, _)| *name == "Top")
            .map(|(_, v)| *v)
            .expect("the model's Top");
        let dir = scratch("resume-tier1");
        let st = StateDir::open(&dir).expect("open");
        let mut real: BTreeMap<[i64; 6], (i64, i64)> = BTreeMap::new();
        // The (real, re-based) cursors for a state, each run once per distinct input.
        let mut run = |s: &std::collections::BTreeMap<&'static str, i64>| {
            let key = ["g", "rg", "h", "c", "f1", "f2"].map(|v| s[v]);
            *real.entry(key).or_insert_with(|| {
                (
                    resume_through(&st, s, top, false),
                    resume_through(&st, s, top, true),
                )
            })
        };
        // Walk `model`'s reachable deciding states; answer (compared, real diverged, re-based
        // diverged).
        let mut walk = |model: &aterm_spec::derive::Model, laws: bool| {
            let (mut compared, mut diverged, mut rebased) = (0, Vec::new(), 0);
            for state in interp::reachable_states(model) {
                let deciding = state["pc"] == 0 && state["down"] == 0 && state["rg"] < state["g"];
                if !deciding {
                    continue;
                }
                let (cursor, rebase) = run(&state);
                let decided = |r| {
                    let mut next = state.clone();
                    next.insert("r", r);
                    next.insert("pc", 1);
                    next
                };
                compared += 1;
                let taken = decided(cursor);
                if interp::admits(model, &state, &taken) == Some("Decide") {
                    let mut resumed = taken.clone();
                    assert!(model.fire("Resume", &mut resumed), "Resume from {taken:?}");
                    // A defective model's own `Resume` breaks its law: only the committed
                    // one is held to them.
                    for law in ["NoSkipInWindow", "AnotherLogOutsideWindow"]
                        .into_iter()
                        .filter(|_| laws)
                    {
                        assert!(
                            model.check_invariant(law, &resumed),
                            "{}: the real resume to {cursor} breaks {law} from {state:?}",
                            model.name
                        );
                    }
                } else {
                    diverged.push(format!("{state:?}: real {cursor}"));
                }
                if interp::admits(model, &state, &decided(rebase)) != Some("Decide") {
                    rebased += 1;
                }
            }
            (compared, diverged, rebased)
        };

        let (compared, diverged, rebased) = walk(&committed, true);
        eprintln!("FabricReaderResume: {compared} resumes held to the model");
        assert!(compared > 100, "only {compared} resumes compared");
        assert!(
            diverged.is_empty(),
            "the real reader is not the model's in {} of {compared}:\n{}",
            diverged.len(),
            diverged.join("\n")
        );
        assert!(
            rebased > 0,
            "NEGATIVE CONTROL: the reader that re-bases at the head matched the model in all \
             {compared} resumes"
        );
        for buggy in 1..=3 {
            let (compared, diverged, _) = walk(&interp::with_buggy(&committed, buggy), false);
            assert!(
                !diverged.is_empty(),
                "the real reader matched the defective model Buggy = {buggy} in all {compared} \
                 resumes"
            );
        }
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// A real clamp keeps retained floors, but uncertainty must precede every
    /// deletion or cursor rewrite. All files belong to this exclusive directory.
    #[test]
    fn a_generation_clamp_preserves_uncertain_refill_evidence() {
        let dir = aterm_tempfile::TempDir::new().expect("owned state");
        let st = StateDir::open(dir.path()).expect("open");
        let sid = "s-kept";
        let topics = BTreeMap::from([("topic".to_string(), TopicCursor { next: 9 })]);
        st.set_topics(sid, &topics).unwrap();
        st.set_seen_off(sid, 8).unwrap();
        st.set_topic_floors(
            sid,
            &BTreeMap::from([("topic".to_string(), 1), ("gone".to_string(), 7)]),
        )
        .unwrap();
        st.forget_log_offsets(9, None, Some(6)).unwrap();
        assert_eq!(
            st.topic_floors(sid).unwrap(),
            BTreeMap::from([("topic".into(), 1)])
        );
        assert_eq!(st.topics(sid)["topic"].next, 6);
        assert_eq!(st.seen_off(sid), Some(5));
        st.forget_log_offsets(9, None, None).unwrap();
        assert!(st.topic_floors(sid).unwrap().is_empty());

        let mut preserved = Vec::new();
        for keep in [Some(6), None] {
            for bytes in [b"not a floor".as_slice(), b"", b"topic 1\ntopic 2", b"\xff"] {
                st.set_topics(sid, &topics).unwrap();
                st.set_seen_off(sid, 8).unwrap();
                let floor = dir.path().join("unhandled").join(sid);
                std::fs::write(&floor, bytes).unwrap();
                let before_topics = std::fs::read(dir.path().join("topics").join(sid)).unwrap();
                let before_seen = std::fs::read(dir.path().join("seen").join(sid)).unwrap();
                let result = st.forget_log_offsets(9, None, keep);
                preserved.push(
                    result.is_err()
                        && std::fs::read(&floor).is_ok_and(|got| got == bytes)
                        && std::fs::read(dir.path().join("topics").join(sid))
                            .is_ok_and(|got| got == before_topics)
                        && std::fs::read(dir.path().join("seen").join(sid))
                            .is_ok_and(|got| got == before_seen),
                );
            }
        }
        assert!(
            preserved.into_iter().all(|kept| kept),
            "CLAMP_DROPPED_UNCERTAIN_REFILL_EVIDENCE"
        );
    }
}
