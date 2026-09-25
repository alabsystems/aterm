// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! THE STATE DIR — the facts a bridge must not lose across a `kill -9`.
//!
//! Every one is a small file written with the same discipline: write a temp,
//! `fsync` it, `rename` over the target. A half-written producer sequence is
//! worse than no file at all — the broker would dedup live records away — so
//! nothing here is ever updated in place.
//!
//! | file | what it holds | why losing it is not fatal |
//! |---|---|---|
//! | `node` | this node's `n-<16 hex>` id | a new id is a new node: it loses its mail lane, so this one IS load-bearing |
//! | `inc` | the incarnation | the bus's own last presence row is consulted too — `max(local, Last) + 1` |
//! | `seq` | the producer sequence, written BEFORE each publish | a lost `seq` still can't dup: the incarnation moved, and every sequence of `inc+1` is above `inc`'s reserved top |
//! | `seen/<sid>` | the endpoint's handled watermark, one file per session ever delivered to (see [`StateDir::seen_off`]) | the refill starts lower and the ring dedups on `off=` |
//! | `acked` | the highest offset this bridge has a `PublishAck` for on its OWN lanes | the self-lane check gets more conservative, never less |
//! | `pins` | the TOFU `sid → node` map, newest [`PINS_KEEP`] | a re-pin on next sight, which is the TOFU rule anyway |
//! | `halt-acked` | the highest fleet-halt offset this node has answered | the node re-acks a barrier it already answered: one extra retained row, never a new subject |
//! | `sent/<sid>.<id>` | the producer sequence reserved for one queued post, and whether a `key=` table entry chose it | a fresh sequence is a second copy — this one IS load-bearing, and it is written BEFORE the publish |
//! | `keys/<sid>` | `post key=` → the producer sequence it reserved, newest [`KEYS_KEEP`] per session | a re-post under a lost key is a second record; the bound is stated and the newest keys are the ones a retry names |
//! | `acks/<sid>.<rid>` | the producer sequence reserved for one OWED receipt (R8), written BEFORE the publish, removed at retirement | a fresh sequence is a second `ack` on the sender's lane after a crash between the publish and the retirement |
//! | `deadlines` | every `ask`/`task` this node published with `dl=` and has not seen answered, newest [`DEADLINES_KEEP`] | a lost table records no `expired` for those asks — the bus is checked before any verdict is published, so it can never record a false one |
//! | `topics/<sid>` | one `<topic> <next>` line per broadcast opt-in: the offset the topic resumes from (see [`StateDir::topics`]) | the topic is re-resolved from the session's `since=` at the next attach — `head` skips whatever was published while the bridge was down, which is why this one is written on every step |

use std::collections::BTreeMap;
use std::io::{self, Write};
use std::path::{Path, PathBuf};

/// One outstanding deadline: an `ask` or `task` this node published with
/// `dl=`, and has not yet seen an `answer`, `report` or `ack` for.
///
/// §6.4 / R8: "the ASKER's own bridge" records `expired re=<off> dl=<ms>` when
/// the deadline passes — the broker holds no timers. This is the list that
/// bridge arms toward the earliest deadline. `at` is the ABSOLUTE deadline
/// (the publish clock plus `dl`), so a relaunched bridge does not restart it.
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
    /// The principal whose reply settles it — the node (or `p/` principal)
    /// owning the lane the ask went to ([`Asked`]). `None` for a line written
    /// before round 16's review, which any reply settles, as it always did.
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

    /// Parse one back. TOTAL, and partial is `None`: a half line is not a
    /// deadline this bridge should publish a verdict for.
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

/// One `ask`/`task` this node published, and the principal whose REPLY it is:
/// the node that owns the lane it went to (`/f/<F>/in/<node>/<sid>/…`), or the
/// principal of a `p/` lane (`/f/<F>/in/p/<principal>/…`). The broker's grant
/// binds a reply's `<src>` segment to its publisher, so this is what tells the
/// recipient's receipt from anybody else's record that merely names the same
/// offset (round 16 review: a third node's `ack re=<off>` settled the asker's
/// `--wait-ack` and its deadline).
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

    /// Parse one back — TOTAL, a partial line is `None`.
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

/// How many `post key=` reservations one session keeps, newest last.
///
/// The bound R7 states for the broker's own dedup window, applied to the table
/// that rides on it. The 4097th key evicts the oldest, and a re-post under an
/// evicted key is a NEW record — stated, and pinned by a test, rather than
/// "never".
pub const KEYS_KEEP: usize = 4096;

/// How many outstanding deadlines the node keeps, newest offset last.
pub const DEADLINES_KEEP: usize = 4096;

/// How many TOFU pins the table keeps, oldest-first-out.
///
/// THE ONE DURABLE STRUCTURE HERE THAT HAD NO BOUND. Every sibling states one —
/// `acked` at `SELF_ACK_KEEP`, notify's journal at `JOURNAL_KEEP`, `RETIRED_KEEP`,
/// `RING_CAP` — the discipline in four words: nothing durable and unbounded. `pins`
/// grew one line per REMOTE sid ever addressed by a bare `@s-<sid>`, sids are
/// per-session and never reused, and the node id that owns the file is minted
/// once and kept forever; meanwhile `Bridge::pinned_node_for` re-reads and
/// re-parses the whole file on every such post and `pin` rewrites and `fsync`s
/// it whenever a new sid appears. A busy orchestrator reached six figures of
/// lines in a month, with §11.2's `aterm-link pin` override unimplemented and
/// no operator undo but deleting the file by hand.
///
/// EVICTION IS BY FIRST SIGHT, NOT BY USE, and the file is kept in first-sight
/// order to make that possible. Recency would mean a durable rewrite per post
/// — trading an unbounded file for an unbounded write rate — where the eviction
/// order the file already has costs nothing: a re-pin on next sight IS the TOFU
/// rule, and it is the answer this table's own row has always given for losing
/// the file entirely. What an evicted pin costs is the T15 protection for a sid
/// that has not been addressed in the last [`PINS_KEEP`] fresh ones; four
/// thousand distinct newer peers is a long way past the sid churn the bound
/// exists to survive.
pub const PINS_KEEP: usize = 4096;

/// ONE SESSION'S POSITION ON ONE BROADCAST TOPIC: the offset it resumes from —
/// the delivery guarantee across a bridge restart.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct TopicCursor {
    /// The next offset this topic may deliver from.
    pub next: u64,
}

impl TopicCursor {
    /// `<topic> <next>` — the state file's line. `None` for anything else,
    /// which the caller REPORTS rather than swallows. A 0.90.x bridge wrote a
    /// third field (that release's add serial); it is read past.
    #[must_use]
    pub fn parse(line: &str) -> Option<(String, TopicCursor)> {
        let mut words = line.split_whitespace();
        let topic = words.next()?;
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

/// One state-file line, bounded and stripped of control characters for a
/// report: the file is on disk and anything that can write the state directory
/// can put an escape sequence in it, and this line goes to a terminal.
fn safe_line(line: &str) -> String {
    line.chars().filter(|c| !c.is_control()).take(120).collect()
}

/// The bridge's durable state.
pub struct StateDir {
    root: PathBuf,
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

    /// Write one file DURABLY: temp, fsync, rename. The rename is what makes a
    /// reader see either the old value or the new one, never a truncation.
    ///
    /// # Errors
    ///
    /// Any I/O failure along the way — and every caller treats it as fatal,
    /// because a bridge that cannot persist its sequence must not publish.
    fn write(&self, name: &str, value: &str) -> io::Result<()> {
        let target = self.path(name);
        let tmp = target.with_extension("tmp");
        if let Some(parent) = tmp.parent() {
            std::fs::create_dir_all(parent)?;
        }
        {
            let mut f = std::fs::File::create(&tmp)?;
            f.write_all(value.as_bytes())?;
            f.write_all(b"\n")?;
            f.sync_all()?;
        }
        std::fs::rename(&tmp, &target)
    }

    /// This node's id, minted once and kept forever. The id IS the mail address:
    /// a node that mints a new one on every start has no inbox lane anybody can
    /// write to twice.
    ///
    /// # Errors
    ///
    /// If a freshly minted id cannot be persisted — better to refuse to start
    /// than to run under an id that will not survive a restart.
    pub fn node_id(&self, mint: impl FnOnce() -> String) -> io::Result<String> {
        if let Some(id) = self.read("node") {
            return Ok(id);
        }
        let id = mint();
        self.write("node", &id)?;
        Ok(id)
    }

    /// The incarnation this bridge last used, as far as the LOCAL disk knows.
    #[must_use]
    pub fn incarnation(&self) -> u64 {
        self.read("inc").and_then(|s| s.parse().ok()).unwrap_or(0)
    }

    /// Record the incarnation this run is using.
    ///
    /// # Errors
    ///
    /// Any I/O failure.
    pub fn set_incarnation(&self, inc: u64) -> io::Result<()> {
        self.write("inc", &inc.to_string())
    }

    /// The highest producer sequence this bridge has ever RESERVED (not
    /// necessarily published). Written before the publish, so a crash between
    /// the two BURNS a sequence number rather than risking a duplicate — the
    /// same trade `asb pub --seq-file` makes, and for the same reason.
    #[must_use]
    pub fn sequence(&self) -> u64 {
        self.read("seq").and_then(|s| s.parse().ok()).unwrap_or(0)
    }

    /// Reserve and persist the next sequence.
    ///
    /// # Errors
    ///
    /// Any I/O failure — and the caller must NOT publish if this fails.
    pub fn reserve_sequence(&self, next: u64) -> io::Result<()> {
        self.write("seq", &next.to_string())
    }

    /// The endpoint's `seen=` watermark for one session, persisted from the
    /// `EVENT <local> inbox-seen` digest. The refill (§6.2) starts at `+1`.
    ///
    /// ONE SMALL FILE PER SESSION EVER DELIVERED TO, AND NOTHING PRUNES THEM —
    /// stated rather than fixed, because the fix is worse than the growth.
    /// Sids are per-session and never reused, so the directory grows with
    /// sessions that have received mail (not with traffic), at one inode of a
    /// few bytes each. Deleting a departed session's file would be free ONLY if
    /// "departed" were knowable here, and it is not: the sid a seamless update
    /// preserves is absent from aterm's roster for the whole window between the
    /// re-exec and adoption, and a sweep that ran in that window would delete
    /// the watermark of a live session and refill it from
    /// `REFILL_FLOOR_SPAN` — re-offering thousands of old offsets — which is
    /// the exact case §6.2's refill exists to serve. `pins` is bounded
    /// ([`PINS_KEEP`]) because eviction there costs a re-pin; here it would
    /// cost the guarantee.
    #[must_use]
    pub fn seen_off(&self, sid: &str) -> Option<u64> {
        self.read(&format!("seen/{sid}"))
            .and_then(|s| s.parse().ok())
    }

    /// Record a session's `seen=` watermark. MONOTONE: an out-of-order event
    /// never moves it backwards, because moving it back would re-deliver rows
    /// the agent already handled.
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

    /// Every offset this bridge holds a `PublishAck` for on its OWN lanes.
    ///
    /// The self-lane check (§6.2) asks the opposite question: a record on this
    /// node's lane, under this node's `<src>`, at an offset that is NOT here is a
    /// forgery by a co-holder of the node cap. A SET rather than a high-water
    /// mark, because offsets from other producers interleave with ours — a
    /// watermark would call every one of our own records below it genuine.
    #[must_use]
    pub fn self_acked(&self) -> std::collections::BTreeSet<u64> {
        self.read("acked")
            .map(|s| s.split(',').filter_map(|n| n.parse().ok()).collect())
            .unwrap_or_default()
    }

    /// Record one more acked self-lane offset, keeping the newest `keep`.
    ///
    /// # Errors
    ///
    /// Any I/O failure.
    pub fn note_self_acked(&self, off: u64, keep: usize) -> io::Result<()> {
        let mut set = self.self_acked();
        set.insert(off);
        while set.len() > keep {
            let Some(oldest) = set.iter().next().copied() else {
                break;
            };
            set.remove(&oldest);
        }
        let joined: Vec<String> = set.iter().map(u64::to_string).collect();
        self.write("acked", &joined.join(","))
    }

    /// The highest fleet-halt offset this node has published an `ack` for.
    ///
    /// The fleet face resubscribes from offset 0 on every reconnect, so without
    /// this the node re-answers every halt in the fleet's history at every
    /// reconnect. `None` is "answered nothing", which is the conservative
    /// direction: it re-acks rather than skipping a barrier it never answered.
    #[must_use]
    pub fn halt_acked(&self) -> Option<u64> {
        self.read("halt-acked").and_then(|s| s.parse().ok())
    }

    /// Record the halt offset this node has now answered. MONOTONE, for the same
    /// reason the `seen` watermark is: an out-of-order replay must not make the
    /// node re-answer the whole history.
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

    /// The TOFU `sid → node` pins (§6.1): the first node seen advertising a sid
    /// owns it, and a second node claiming it is a CONFLICT, never a route.
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

    /// The pin file's lines IN FIRST-SIGHT ORDER, which is the order
    /// [`PINS_KEEP`] evicts in. [`StateDir::pins`] is the same content as a map
    /// for lookups; this is the same content as a queue for the bound.
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

    /// Pin `sid` to `node` if it is not pinned yet. Answers the node it is pinned
    /// to — which is NOT necessarily `node`, and a caller that assumed it was
    /// would turn a conflict into a route.
    ///
    /// BOUNDED AT [`PINS_KEEP`], oldest first sight out. The new pin is
    /// APPENDED, so the file stays in the order the bound is expressed in.
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
        self.write("pins", &lines[over..].join("\n"))?;
        Ok(node.to_string())
    }

    /// One session's BROADCAST CURSORS: `topic → the next offset that topic may
    /// deliver from`.
    ///
    /// THE OPT-IN LIVES IN ATERM; THE CURSOR LIVES HERE, and the split is the
    /// point. The session owns the set (`topic add`/`topic drop`) and answers
    /// `since=head|@<off>`, which is an INTENT, not a position — resolving
    /// `head` needs the bus. The bridge resolves it ONCE, against the broker's
    /// head at the moment it first learns the entry, and writes the resolved
    /// offset here. A bridge that restarts therefore RESUMES each topic from
    /// where the last one left off instead of re-reading `head` and silently
    /// swallowing every broadcast published while it was down — which is the
    /// failure the persistence exists to prevent, and it is invisible from the
    /// session, which asked for a topic and simply never heard about it.
    ///
    /// Keyed by sid, one file per session, so a session that exits takes its
    /// cursors with it ([`StateDir::forget_topics`]).
    #[must_use]
    pub fn topics(&self, sid: &str) -> BTreeMap<String, TopicCursor> {
        let Some(body) = self.read(&format!("topics/{sid}")) else {
            return BTreeMap::new();
        };
        let mut out = BTreeMap::new();
        for line in body.lines() {
            match TopicCursor::parse(line) {
                Some((topic, cursor)) => {
                    out.insert(topic, cursor);
                }
                // Said out loud, naming the file and the fallback: a dropped
                // cursor is a dropped delivery guarantee, and the session it
                // belonged to has no other way to learn it.
                None => eprintln!(
                    "aterm-link: {}: `{}` is not a broadcast cursor; that topic \
                     resumes from the bus head and whatever it missed is lost",
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

    /// Forget a session's broadcast cursors — it is gone from the roster.
    pub fn forget_topics(&self, sid: &str) {
        let _ = std::fs::remove_file(self.path(&format!("topics/{sid}")));
    }

    /// The producer sequence this bridge RESERVED for one outbound post, if it
    /// has reserved one.
    ///
    /// THIS IS WHAT MAKES THE OUTBOUND HALF EXACTLY-ONCE. `outbox` is a peek, so
    /// a bridge that dies between `Publish` and `outbox sent` re-reads the same
    /// post on restart — and republishing it under a FRESH sequence would put a
    /// second copy on the bus, because the broker's dedup key is
    /// `(producer_id, producer_seq)` and a new number is a new record. Pinning
    /// the sequence to the POST rather than to a counter makes the retry
    /// byte-identical, so the broker answers `deduped` and hands back the
    /// ORIGINAL offset — which is the offset the sender's `--wait` was promised.
    ///
    /// The second half is WHERE THE NUMBER CAME FROM: `true` when a `post key=`
    /// table entry an EARLIER post reserved chose it (see [`StateDir::key_seq`]),
    /// which is what makes the retirement say `dup=1`. It is persisted beside
    /// the sequence so a bridge that dies between the publish and the
    /// retirement answers the same `dup=1` after its relaunch that it would have
    /// answered before.
    #[must_use]
    pub fn post_seq(&self, sid: &str, id: u64) -> Option<(u64, bool)> {
        let line = self.read(&format!("sent/{sid}.{id}"))?;
        let mut toks = line.split_whitespace();
        let seq = toks.next()?.parse().ok()?;
        Some((seq, toks.next() == Some("key")))
    }

    /// Reserve a sequence for one outbound post, durably, BEFORE it is published.
    /// `via_key` says a `post key=` entry chose it (see [`StateDir::post_seq`]).
    ///
    /// # Errors
    ///
    /// Any I/O failure — and the caller must not publish if this fails.
    pub fn set_post_seq(&self, sid: &str, id: u64, seq: u64, via_key: bool) -> io::Result<()> {
        let line = if via_key {
            format!("{seq} key")
        } else {
            seq.to_string()
        };
        self.write(&format!("sent/{sid}.{id}"), &line)
    }

    /// Forget a retired post's sequence. Called only after the endpoint has
    /// acknowledged the retirement, so the file outlives every moment in which
    /// it could still be needed.
    pub fn clear_post_seq(&self, sid: &str, id: u64) {
        let _ = std::fs::remove_file(self.path(&format!("sent/{sid}.{id}")));
    }

    /// The producer sequence this bridge RESERVED for one OWED RECEIPT — the
    /// `receipt sid=<sid> rid=<rid> …` line an endpoint lists on its `outbox`
    /// peek from a session's `inbox seen <id> handled|refused|deferred` until
    /// the bridge retires it (R8) — if it has reserved one.
    ///
    /// [`StateDir::post_seq`]'s rule, for the other thing the peek carries out.
    /// A bridge that dies between publishing the `ack` and the endpoint's
    /// retirement re-reads the same receipt after its relaunch, and
    /// republishing it at a FRESH sequence would put a second `ack` on the
    /// sender's lane; at THIS sequence the broker answers `deduped` and
    /// appends nothing. One file per receipt, holding only the number, removed
    /// once the endpoint has taken the retirement.
    #[must_use]
    pub fn receipt_seq(&self, sid: &str, rid: u64) -> Option<u64> {
        self.read(&format!("acks/{sid}.{rid}"))?.parse().ok()
    }

    /// Reserve a sequence for one owed receipt, durably, BEFORE it is
    /// published. See [`StateDir::receipt_seq`].
    ///
    /// # Errors
    ///
    /// Any I/O failure — and the caller must not publish if this fails.
    pub fn set_receipt_seq(&self, sid: &str, rid: u64, seq: u64) -> io::Result<()> {
        self.write(&format!("acks/{sid}.{rid}"), &seq.to_string())
    }

    /// Forget a retired receipt's sequence. Called only after the endpoint has
    /// taken the retirement.
    pub fn clear_receipt_seq(&self, sid: &str, rid: u64) {
        let _ = std::fs::remove_file(self.path(&format!("acks/{sid}.{rid}")));
    }

    /// The producer sequence a session's `post key=<key>` RESERVED, if any post
    /// under that key ever did.
    ///
    /// ## What makes `post key=` exactly once (R7, §6.5)
    ///
    /// [`StateDir::post_seq`] pins a sequence to a POST ID, which survives a
    /// bridge crash but not a RE-POST: `ERR timeout` leaves the sender holding
    /// a post id it cannot tell landed from lost, and posting again mints a new
    /// id — a new sequence, a second record. This table pins the sequence to
    /// the CALLER'S key instead. A re-post under the same key finds it here,
    /// publishes at the same `(producer_id, producer_seq)`, and the broker's own
    /// dedup — rebuilt from the log on a broker restart — answers `deduped` with
    /// the ORIGINAL offset and appends nothing. The endpoint's `outbox` carries
    /// the key on every drain, so a relaunched bridge reads the same key from
    /// the same queued row.
    ///
    /// FIRST WINS, like a pin: the sequence a key reserved is the sequence it
    /// keeps, because a later number would be a later record. One file per
    /// session, newest last, bounded at [`KEYS_KEEP`].
    #[must_use]
    pub fn key_seq(&self, sid: &str, key: &str) -> Option<u64> {
        self.key_lines(sid)
            .iter()
            .filter_map(|l| l.split_once(' '))
            .find(|(k, _)| *k == key)
            .and_then(|(_, seq)| seq.parse().ok())
    }

    /// The key file's lines IN RESERVATION ORDER, which is the order
    /// [`KEYS_KEEP`] evicts in.
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

    /// Reserve `seq` for `key`, durably, BEFORE the publish — and only if the
    /// key has none yet (first wins; see [`StateDir::key_seq`]). Answers the
    /// sequence the key holds afterwards, which is `seq` unless it already had
    /// one.
    ///
    /// # Errors
    ///
    /// Any I/O failure — and the caller must not publish if this fails.
    pub fn set_key_seq(&self, sid: &str, key: &str, seq: u64) -> io::Result<u64> {
        let mut lines = self.key_lines(sid);
        if let Some(have) = lines
            .iter()
            .filter_map(|l| l.split_once(' '))
            .find(|(k, _)| *k == key)
            .and_then(|(_, s)| s.parse().ok())
        {
            return Ok(have);
        }
        lines.push(format!("{key} {seq}"));
        let over = lines.len().saturating_sub(KEYS_KEEP);
        self.write(&format!("keys/{sid}"), &lines[over..].join("\n"))?;
        Ok(seq)
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

    /// Replace the deadline table, keeping the newest [`DEADLINES_KEEP`] by
    /// offset. Written whole because it is small and changes rarely: once per
    /// ask with a `dl=`, once per reply to one.
    ///
    /// # Errors
    ///
    /// Any I/O failure.
    pub fn set_deadlines(&self, list: &[Deadline]) -> io::Result<()> {
        let over = list.len().saturating_sub(DEADLINES_KEEP);
        let lines: Vec<String> = list[over..].iter().map(Deadline::render).collect();
        self.write("deadlines", &lines.join("\n"))
    }

    /// The directory itself, for a caller writing something this type does not
    /// model — the `pid` file `Bridge::new` writes, and the fault markers
    /// `Bridge::run` looks for under it.
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

    /// The node id is minted ONCE. It is the node's mail address, so a second
    /// open must read the same one back — a fresh id would silently abandon
    /// every message already on the old lane.
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

    /// A cursor line is `<topic> <next>` (a 0.90.x third field is read past);
    /// anything else is `None` — and a `None` in the file is DROPPED AND
    /// REPORTED, never repaired, while the lines beside it survive.
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
        for bad in ["", "onlytopic", "t x", "t 1 y", "t 1 2 3", "t +5"] {
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

    /// The `seen` watermark is MONOTONE. An out-of-order digest event must not
    /// walk it back and re-deliver rows the agent already handled.
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

    /// The self-lane acked set is a SET, not a watermark, and it is bounded.
    /// A watermark would declare every one of our own records below it genuine,
    /// which is exactly the forgery the check exists to catch.
    #[test]
    fn the_self_acked_set_is_a_set_and_is_bounded() {
        let dir = scratch("acked");
        let st = StateDir::open(&dir).expect("open");
        for off in [10u64, 30, 20] {
            st.note_self_acked(off, 3).expect("write");
        }
        assert_eq!(st.self_acked(), [10, 20, 30].into_iter().collect());
        // The halt-ack watermark is monotone for the same reason `seen` is: a
        // reconnect replays the fleet face from zero, and a watermark that
        // walked backwards would re-answer every barrier in the log.
        assert_eq!(st.halt_acked(), None);
        st.set_halt_acked(91).expect("write");
        st.set_halt_acked(7).expect("write");
        assert_eq!(st.halt_acked(), Some(91));
        st.set_halt_acked(92).expect("write");
        assert_eq!(st.halt_acked(), Some(92));
        st.note_self_acked(40, 3).expect("write");
        assert_eq!(
            st.self_acked(),
            [20, 30, 40].into_iter().collect(),
            "the oldest is dropped, and 10 is now unknown rather than proven ours"
        );
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// A pin is FIRST-WINS and it reports the pinned node, not the asked-for one.
    /// A caller that read back its own argument would turn a second node's claim
    /// into a route into that node's read lane.
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

    /// A POST'S SEQUENCE IS PINNED TO THE POST, and it survives a reopen. A
    /// bridge that came back and picked a fresh number would publish a second
    /// copy of a message the bus already holds — the broker's dedup key is
    /// `(producer_id, producer_seq)`, so a new number IS a new record.
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
        // A DIFFERENT post keeps its own, and a retirement forgets one.
        assert_eq!(st.post_seq("s-a", 2), None);
        st.clear_post_seq("s-a", 1);
        assert_eq!(st.post_seq("s-a", 1), None);
        // WHERE THE NUMBER CAME FROM survives with it: a sequence a `key=`
        // entry chose reads back as one, so the relaunched bridge's retirement
        // says `dup=1` exactly as the dead one's would have.
        st.set_post_seq("s-a", 3, 0x0000_0002_0000_0009, true)
            .expect("reserve via key");
        assert_eq!(
            StateDir::open(&dir).expect("reopen").post_seq("s-a", 3),
            Some((0x0000_0002_0000_0009, true))
        );
        // And a file from before the marker existed reads as a plain pin.
        std::fs::write(dir.join("sent/s-a.4"), "17\n").expect("write");
        assert_eq!(st.post_seq("s-a", 4), Some((17, false)));
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// **AN OWED RECEIPT'S SEQUENCE IS PINNED TO `(sid, rid)`, SURVIVES A
    /// REOPEN, AND IS FORGOTTEN AT RETIREMENT.** The durable half of a receipt
    /// being exactly once: a bridge that dies between the `ack` publish and the
    /// endpoint's retirement republishes under the SAME sequence after its
    /// relaunch, which the broker dedups.
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

    /// **A `post key=` PINS ITS SEQUENCE TO THE KEY, FIRST WINS, AND THE TABLE
    /// IS BOUNDED AT [`KEYS_KEEP`] — the 4097th key evicts the oldest.** A
    /// re-post under a key the table still holds is byte-identical on the wire
    /// (`(producer_id, producer_seq)`), so the broker dedups it; one under an
    /// evicted key is a new record, and this test states where that line is.
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
        // FIRST WINS: reserving again under the same key answers the sequence
        // the key already holds and writes nothing new.
        assert_eq!(st.set_key_seq("s-a", "k-1", 99).expect("re-reserve"), 41);
        assert_eq!(st.key_seq("s-a", "k-1"), Some(41));
        // PER SESSION: another session's identical key is its own reservation.
        assert_eq!(st.key_seq("s-b", "k-1"), None);
        assert_eq!(st.set_key_seq("s-b", "k-1", 7).expect("reserve"), 7);
        assert_eq!(st.key_seq("s-a", "k-1"), Some(41));

        // THE BOUND. Seed a full table as one file (the write is whole-file, so
        // filling it a line at a time would be quadratic), then add one more.
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

    /// THE DEADLINE TABLE ROUND-TRIPS, IS BOUNDED, AND A DAMAGED LINE READS AS
    /// NOTHING — a half-parsed deadline would be a verdict published for the
    /// wrong ask.
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
        // BOUNDED: the newest by position survive.
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

    /// THE ASKED TABLE ROUND-TRIPS, IS BOUNDED, AND A DAMAGED LINE READS AS
    /// NOTHING — a misread recipient would hold a real receipt as a stray.
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

    /// THE PIN TABLE IS BOUNDED, oldest first sight out, and the bound does not
    /// disturb what is inside it.
    ///
    /// It was the one durable structure in the crate with no cap and no prune,
    /// growing one line per remote sid ever addressed while `pinned_node_for`
    /// re-read and re-parsed the whole file before every post — and §11.2's
    /// `aterm-link pin` override is unimplemented, so an operator's only remedy
    /// was deleting the file by hand.
    #[test]
    fn the_pin_table_is_bounded_and_evicts_the_oldest_first_sight() {
        let dir = scratch("pins-bound");
        let st = StateDir::open(&dir).expect("open");
        // A FULL TABLE, WRITTEN AS ONE FILE. `pin` rewrites the whole file per
        // call, so filling it a line at a time would be quadratic — the very
        // cost the bound exists to stop growing.
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
        // The sixteen oldest first sights are gone; everything newer stands,
        // and a re-pin on next sight is the TOFU rule anyway.
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
        // AND FIRST-WINS STILL HOLDS ACROSS THE BOUND: re-pinning a surviving
        // sid to another node answers the node it is pinned to and writes
        // nothing, so no eviction can be provoked by asking.
        let survivor = format!("s-{:016x}", PINS_KEEP);
        assert_eq!(
            st.pin(&survivor, "n-rogue").expect("re-pin"),
            format!("n-{:016x}", PINS_KEEP % 3)
        );
        assert_eq!(st.pins().len(), PINS_KEEP);
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// A missing or garbage file reads as "nothing known" rather than a panic or
    /// a restart at 1 — an empty `seq` that restarted at 1 would have the broker
    /// dedup live records away.
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
