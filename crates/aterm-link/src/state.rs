// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! The state dir: the facts a bridge must not lose across a `kill -9`.
//!
//! Every file is replaced whole: a temp of the writer's own (`create_temp_beside`),
//! then a `rename` over the target, so a reader sees the old value or the new one,
//! never a truncation. A half-written producer sequence is worse than none (the
//! broker would dedup live records away), so nothing is updated in place.
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
//!   address and what its caps were minted for; the incarnation fences every
//!   sequence the next one uses; a pin is the T15 first sight of a remote sid, which
//!   outlives this host's reboot; a standing halt is a human's, and the bus row it
//!   stands in for may be gone; a rewound identity stays refused even if its head
//!   grows again, and releasing it first flushes the replacement log's witness. Each
//!   is written once, or when its set changes, never per message.
//! - Atomic only: everything else. A power loss ends what those files pair with
//!   anyway: the outbox, the owed receipts and the inbox ring live in aterm's memory
//!   (a seamless update carries them, a reboot does not), and aterm mints fresh sids
//!   at every launch, so every file serving a session (`seen/`, `sent/`, `acks/`,
//!   `keys/`, `topics/`, `unhandled/`, `asked`, `deadlines`, `acked`, `inflight/`)
//!   is orphaned by a reboot, flushed or not. The not-hosted gate runs before the
//!   self-lane check, so a stale `acked` judges no record of a session that is gone.
//!   `seq` is fenced by the durable `inc`: the first attach after the boot takes
//!   `inc + 1`, and every sequence of it is above anything a rolled-back `seq` could
//!   name. The doctor's `publish_exited`, which does not roll the incarnation, meets
//!   a rolled-back `seq` as a deduped publish and refuses (fail-closed) until a
//!   bridge has attached once. A rolled-back `halt-acked` costs one re-ack, and a
//!   rolled-back `bus-log` is a lower high-water, which the halts do not rest on.
//!
//! Offsets name records in one log. `seen/`, `topics/`, `unhandled/`, `acked`,
//! `halt-acked`, `deadlines`, `asked` and the `original-off` of `keys/` and `sent/`
//! hold bus offsets, each meaningless on another log, as is an `inflight/` record's
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
//! | `inc` | durable | the incarnation | the bus's last presence row counts too: `max(local, Last) + 1` |
//! | `seq` | atomic | the producer sequence, written before each publish | no dup: the incarnation moved, and every sequence of `inc+1` is above `inc`'s reserved top |
//! | `seen/<sid>` | atomic | where the session's addressed refill starts after: the endpoint's reported floor minus one ([`StateDir::seen_off`]) | the refill starts lower and the ring dedups on `off=` |
//! | `acked` | atomic | the newest `keep` offsets with a `PublishAck` on this node's own lanes ([`StateDir::self_acked`]) | the self-lane check gets more conservative, never less |
//! | `inflight/<seq>` | atomic | one record bound for this node's own lanes and the log it was decided on, from before its publish until `acked` holds its offset ([`StateDir::in_flight`]) | the attach cannot re-learn an offset a kill left out of `acked`, and the self-lane check accuses that one record of ours |
//! | `pins` | durable | the TOFU `sid → node` map, newest [`PINS_KEEP`] | a re-pin on next sight, the TOFU rule anyway |
//! | `halt-acked` | atomic | the highest fleet-halt offset this node has answered | a re-ack of an answered barrier: one extra retained row |
//! | `bus-log` | atomic | the identity of the log this node reads and the highest broker head seen there ([`LogWitness`]) | a replaced log is judged by its head alone until the next attach records the identity; the halts do not rest on it |
//! | `rewound-log` | durable | one known identity whose head this node saw go backwards | regrowth cannot make its reused offsets trustworthy; only a different known log releases the refusal |
//! | `halts` | durable | the fleet halts this node holds in force ([`StandingHalt`]) | the attach re-reads them from the bus; only one whose row the bus lost goes with it |
//! | `sent/<sid>.<id>` | atomic | the sequence reserved for one queued post, its publication identity, and whether a `key=` entry chose it | load-bearing: a fresh sequence is a second copy |
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
fn sync_dir(dir: &Path) -> io::Result<()> {
    let dir = if dir.as_os_str().is_empty() {
        Path::new(".")
    } else {
        dir
    };
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

/// The bridge's state directory.
pub struct StateDir {
    root: PathBuf,
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
    fn parse(text: &str) -> io::Result<Self> {
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
    fn parse(seq: u64, text: &str) -> Option<Self> {
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
        Ok(Self { root })
    }

    /// A state directory to read, creating nothing: `aterm fabric`'s report must not
    /// make a dir that a command names and that does not exist. A missing file reads
    /// as "nothing known".
    #[must_use]
    pub fn peek(root: impl Into<PathBuf>) -> Self {
        Self { root: root.into() }
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
    /// old log. Reads every reservation and writes none; the caller writes them
    /// before the witness advances, and a partial rewrite is safe to repeat.
    fn publication_offset_resets(&self) -> io::Result<Vec<(String, String)>> {
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
                        if pin.original_off.take().is_some() {
                            changed = true;
                            lines.push(format!("{key} {}", pin.render()));
                        } else {
                            lines.push(line.to_string());
                        }
                    }
                    changed.then(|| lines.join("\n"))
                } else {
                    let mut pin = PublishPin::parse(&text)?;
                    pin.original_off.take().map(|_| pin.render())
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

    /// The one replace behind [`StateDir::write`] and [`StateDir::write_durable`].
    /// `mode` is subject to the umask: 0o666 (`File::create`'s) for every file but
    /// the one holding a record's bytes, 0o600 ([`StateDir::set_in_flight`]).
    fn replace(&self, name: &str, value: &str, durable: bool, mode: u32) -> io::Result<()> {
        let target = self.path(name);
        let parent = target.parent().unwrap_or(&self.root);
        std::fs::create_dir_all(parent)?;
        let (tmp, mut f) = crate::enable::create_temp_beside(&target, mode)?;
        let staged = (|| {
            f.write_all(value.as_bytes())?;
            f.write_all(b"\n")?;
            if durable {
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
            sync_dir(parent)?;
        }
        Ok(())
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

    /// The incarnation this bridge last used, as far as the local disk knows.
    #[must_use]
    pub fn incarnation(&self) -> u64 {
        self.read("inc").and_then(|s| s.parse().ok()).unwrap_or(0)
    }

    /// Record the incarnation this run is using, durably and before any sequence of
    /// it is reserved: it fences a `seq` a power loss rolled back (the module doc).
    /// Written once per attach.
    ///
    /// # Errors
    ///
    /// Any I/O failure.
    pub fn set_incarnation(&self, inc: u64) -> io::Result<()> {
        self.write_durable("inc", &inc.to_string())
    }

    /// The highest producer sequence this bridge has reserved (not necessarily
    /// published). It is written before the publish, so a crash between the two burns
    /// a number rather than risk a duplicate (the trade `asb pub --seq-file` makes).
    #[must_use]
    pub fn sequence(&self) -> u64 {
        self.read("seq").and_then(|s| s.parse().ok()).unwrap_or(0)
    }

    /// Reserve the next producer sequence of incarnation `inc` and persist it before
    /// it is used: the one reservation, for the bridge and the doctor's
    /// `publish_exited` alike.
    ///
    /// The next number above both the file and `inc << 32`, refused at the
    /// incarnation's reserved top ([`WILL_SEQ_LOW`]) and past [`INC_MAX`]. A
    /// read-add-write of one file, so only the holder of [`StateDir::lock_bridge`]
    /// calls it.
    ///
    /// # Errors
    ///
    /// The incarnation's space is exhausted or the write failed; the caller must not
    /// publish either way.
    pub fn reserve_next_sequence(&self, inc: u64) -> io::Result<u64> {
        if inc > INC_MAX {
            return Err(io::Error::other(format!(
                "incarnation {inc} is past this node's incarnation space ({INC_MAX})"
            )));
        }
        let base = inc << 32;
        let next = self
            .sequence()
            .max(base)
            .checked_add(1)
            .filter(|n| *n < base | WILL_SEQ_LOW)
            .ok_or_else(|| io::Error::other("this incarnation's sequence space is exhausted"))?;
        self.write("seq", &next.to_string())?;
        Ok(next)
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
    #[must_use]
    pub fn self_acked(&self) -> std::collections::BTreeSet<u64> {
        self.read("acked")
            .map(|s| s.split(',').filter_map(|n| n.parse().ok()).collect())
            .unwrap_or_default()
    }

    /// Replace the acked self-lane set with the newest `keep` of `set`. The bridge's
    /// in-memory copy is the set, so it is written without reading the file back (up
    /// to `keep` offsets per publish).
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
        let joined: Vec<String> = set.iter().skip(over).map(u64::to_string).collect();
        self.write("acked", &joined.join(","))
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
        let _ = std::fs::remove_file(self.path(&format!("inflight/{seq}")));
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

    /// Forget every offset this dir holds: the log they index is not the one this
    /// node reads now ([`LogWitness::replaced_by`]), so each names nothing, and each
    /// is put back where a first run on this log, whose head is `head`, would start:
    ///
    /// * `seen/*` go, so a refill starts at its bounded floor below this log's head,
    ///   not past it, where the old watermark would answer "nothing to refill" over
    ///   rows the endpoint lost;
    /// * every `topics/*` cursor resumes at `head`, as a `since=head` opt-in made now
    ///   does, not at an old offset that would skip this log's broadcasts or replay a
    ///   longer log's history as new mail;
    /// * `unhandled/*` go: a refill floor names a record of the old log, and
    ///   rewinding a cursor to it would offer this log's history as new mail;
    /// * `halt-acked` goes, so a halt here is answered, never skipped;
    /// * `acked` keeps one member, `head - 1`: the self-lane window then starts at
    ///   this log's head, so a record of this node's lane already on this log reads
    ///   "cannot tell" (this node never held an ack for it here), not a forgery, and
    ///   every later record is judged as before;
    /// * `deadlines` and `asked` go: no reply to an ask made on the old log can
    ///   arrive on this one, and an `expired` here would name another record;
    /// * `keys/` and `sent/` lose only `original-off`: a previous winner's position
    ///   is no evidence about a publication on this log;
    /// * an `inflight/*` record carrying a `re=` and witnessed on a different known
    ///   log goes, returned as `(sequence, face)` for the attach to report. One with
    ///   an unknown witness or no `re=` stays: absence is not proof that a pending
    ///   body belongs to another log.
    ///
    /// Node-scoped facts stay (`node`, `inc`, `seq`, `pins`, `acks/`, and the
    /// sequences, identities and key ownership in `sent/` and `keys/`), and so do
    /// the halts this node holds in force (`halts`): only their human's own row
    /// lifts them, never the absence of one.
    ///
    /// Idempotent, and run before the witness is re-based
    /// ([`StateDir::set_log_witness`]): a crash in between re-judges the log at the
    /// next attach and forgets again.
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
    ) -> io::Result<Vec<(u64, String)>> {
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
        let seen = entries("seen")?;
        let unhandled = entries("unhandled")?;
        let mut topics = Vec::new();
        for entry in entries("topics")? {
            let name = entry.file_name();
            let Some(sid) = name.to_str().filter(|n| !n.contains('.')) else {
                continue;
            };
            let body = std::fs::read_to_string(entry.path())?;
            let rebased: BTreeMap<String, TopicCursor> = self
                .parse_topics(sid, &body)
                .into_keys()
                .map(|topic| (topic, TopicCursor { next: head }))
                .collect();
            topics.push((sid.to_string(), rebased));
        }
        let publication_resets = self.publication_offset_resets()?;
        let in_flight = self.in_flight()?;
        for entry in seen.into_iter().chain(unhandled) {
            match std::fs::remove_file(entry.path()) {
                Err(e) if e.kind() != io::ErrorKind::NotFound => return Err(e),
                _ => {}
            }
        }
        for (sid, rebased) in topics {
            self.set_topics(&sid, &rebased)?;
        }
        for (name, text) in publication_resets {
            self.write(&name, &text)?;
        }
        let window = head
            .checked_sub(1)
            .map_or_else(String::new, |n| n.to_string());
        self.write("acked", &window)?;
        self.clear_halt_acked()?;
        self.set_deadlines(&[])?;
        self.set_asked(&[])?;
        let mut dropped = Vec::new();
        for (seq, entry) in in_flight {
            let different_log = matches!((entry.log, log), (Some(was), Some(now)) if was != now);
            if !different_log || crate::body::Body::decode(&entry.body).0.re.is_none() {
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
                    ))
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
        let _ = std::fs::remove_file(self.path(&format!("sent/{sid}.{id}")));
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
        st.forget_log_offsets(100, None).expect("re-base");
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
            st.forget_log_offsets(100, Some(b)).unwrap(),
            vec![(1, face.to_string())]
        );
        let kept = vec![(2, entry(2, None)), (3, entry(3, Some(b))), (4, note)];
        assert_eq!(st.in_flight().unwrap(), kept);
        assert!(
            st.forget_log_offsets(100, Some(b)).unwrap().is_empty(),
            "idempotent"
        );
        assert!(
            st.forget_log_offsets(7, None).unwrap().is_empty(),
            "no identity proves no mismatch"
        );
        assert_eq!(st.in_flight().unwrap(), kept, "uncertainty is retained");
        st.set_seen_off("s-b", 44).unwrap();
        st.write("inflight/5", "damaged").unwrap();
        assert_eq!(
            st.forget_log_offsets(7, Some(b)).unwrap_err().kind(),
            io::ErrorKind::InvalidData
        );
        assert!(dir.join("inflight/5").exists(), "damage is kept for repair");
        assert_eq!(st.seen_off("s-b"), Some(44), "preflight changed nothing");
        let _ = std::fs::remove_dir_all(&dir);
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
                st.forget_log_offsets(20, None).is_err(),
                "unreadable {blocked} was empty"
            );
            assert!(area.is_symlink(), "the unreadable directory entry stays");
            std::fs::remove_file(&area).unwrap();
            assert!(
                st.forget_log_offsets(20, None).is_ok(),
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
}
