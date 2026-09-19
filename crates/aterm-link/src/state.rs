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
//! | `feeding` | the ONE `term/in` whose feed is in flight, and its idempotency key | losing it IS the silent loss §6.5 names — a keystroke that may or may not have been typed and nothing that can tell |
//! | `term-off` | the drive-face offset already handled | the face resumes at the head, which is A3's silent loss for the records in between and never worse |
//! | `sent/<sid>.<id>` | the producer sequence reserved for one queued post, and whether a `key=` table entry chose it | a fresh sequence is a second copy — this one IS load-bearing, and it is written BEFORE the publish |
//! | `keys/<sid>` | `post key=` → the producer sequence it reserved, newest [`KEYS_KEEP`] per session | a re-post under a lost key is a second record; the bound is stated and the newest keys are the ones a retry names |
//! | `acks/<sid>.<rid>` | the producer sequence reserved for one OWED receipt (R8), written BEFORE the publish, removed at retirement | a fresh sequence is a second `ack` on the sender's lane after a crash between the publish and the retirement |
//! | `deadlines` | every `ask`/`task` this node published with `dl=` and has not seen answered, newest [`DEADLINES_KEEP`] | a lost table records no `expired` for those asks — the bus is checked before any verdict is published, so it can never record a false one |

use std::collections::BTreeMap;
use std::io::{self, Write};
use std::path::{Path, PathBuf};

/// One `term/in` record the bridge is part-way through feeding to a PTY.
///
/// The three fields are the whole of what a replay needs: WHICH record (the bus
/// offset — the record itself stays on the log, so the bytes are never copied
/// into the state dir), WHICH session, and under WHICH key. The key carries the
/// epoch, so a key minted for a session that has since relaunched is refused by
/// the endpoint (`ERR epoch`) rather than typed into its successor — the fence
/// is structural, not a check the replay has to remember to make.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FeedIntent {
    /// The bus offset of the `term/in` record being applied.
    pub off: u64,
    /// The session it is being applied to.
    pub sid: String,
    /// The `<epoch>:<producer>:<seq>` key it was stamped with.
    pub key: String,
    /// How many times the feed has been ATTEMPTED, journalled before each one.
    ///
    /// A BACKSTOP ON THE RETRY LOOP, NOT THE BUDGET. A retry under the same key
    /// cannot duplicate — that is the whole of A6's mark — so retrying an
    /// attempt whose outcome was never obtained is safe; retrying it FOREVER is
    /// not. What bounds the ordinary case is `first_at` and the bridge's
    /// `FEED_BUDGET`, because a COUNT of retries means whatever period the
    /// scheduler happens to run them at and that period has already moved once.
    /// This one bounds the case a wall clock cannot: a bridge that dies inside
    /// the feed and relaunches into it, where no time need pass at all.
    pub tries: u32,
    /// When the FIRST attempt on this record was journalled, in `now_ms()`.
    ///
    /// The budget is a DURATION and this is where it starts. It survives the
    /// restarts the retry is meant to survive, so a crash loop cannot buy
    /// itself a fresh budget. `0` is "a journal line written before this field
    /// existed": the next attempt stamps it with the clock rather than reading
    /// an epoch-zero start as an instantly exhausted budget.
    pub first_at: u64,
    /// Whether any attempt on this record ever ended in genuine DOUBT — an
    /// aterm that did not answer, where the bytes may have reached the PTY.
    ///
    /// It decides which verdict exhausting the budget publishes, and it is
    /// STICKY because doubt is: a later `ERR busy` says the endpoint refused
    /// that attempt before claiming the mark, and says nothing whatever about
    /// an earlier attempt whose reply never came. Missing from an old journal
    /// line it reads as `true`, which is the conservative direction.
    pub doubt: bool,
}

impl FeedIntent {
    /// The one line this intent is stored as.
    #[must_use]
    pub fn render(&self) -> String {
        format!(
            "off={} sid={} key={} tries={} first={} doubt={}",
            self.off,
            self.sid,
            self.key,
            self.tries,
            self.first_at,
            u8::from(self.doubt)
        )
    }

    /// Parse one back. TOTAL: a truncated, reordered or hand-edited line reads as
    /// `None` — "nothing was in flight" — rather than as a panic or a partly
    /// filled intent. `None` is the safe direction: it loses a replay the bridge
    /// might have made, where a half-parsed one could feed the wrong session.
    #[must_use]
    pub fn parse(line: &str) -> Option<Self> {
        let (mut off, mut sid, mut key) = (None, None, None);
        // A line written before `tries=` existed reads as one attempt already
        // made, which is the conservative direction: it can only shorten the
        // budget, never extend it.
        let mut tries = 1;
        // A line written before `first=` existed starts its budget at the next
        // attempt rather than at the epoch, and one written before `doubt=`
        // existed is assumed doubtful — the direction that publishes `in-doubt`
        // over `refused`, which is the one an operator must not be talked out
        // of by a missing field.
        let mut first_at = 0;
        let mut doubt = true;
        for tok in line.split_whitespace() {
            match tok.split_once('=') {
                Some(("off", v)) => off = v.parse().ok(),
                Some(("sid", v)) => sid = Some(v.to_string()),
                Some(("key", v)) => key = Some(v.to_string()),
                Some(("tries", v)) => tries = v.parse().unwrap_or(1),
                Some(("first", v)) => first_at = v.parse().unwrap_or(0),
                Some(("doubt", v)) => doubt = v != "0",
                _ => {}
            }
        }
        // A key is `<epoch>:<producer>:<seq>` and a sid is a principal; neither
        // may be empty, or the "replay" would be a differently-shaped request.
        let (off, sid, key) = (off?, sid?, key?);
        if sid.is_empty() || key.split(':').count() != 3 {
            return None;
        }
        Some(Self {
            off,
            sid,
            key,
            tries,
            first_at,
            doubt,
        })
    }
}

/// One outstanding deadline: an `ask` or `task` this node published with
/// `dl=`, and has not yet seen an `answer`, `report` or `ack` for.
///
/// §6.4 / R8: "the ASKER's own bridge" records `expired re=<off> dl=<ms>` when
/// the deadline passes — the broker holds no timers. This is the list that
/// bridge checks on its tick. `at` is the ABSOLUTE deadline (the publish clock
/// plus `dl`), so a relaunched bridge does not restart the clock.
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
/// `acked` at `SELF_ACK_KEEP`, notify's journal at `JOURNAL_KEEP`, the mirror's
/// window at `DEDUP_KEEP`, `RETIRED_KEEP`, `RING_CAP` — and `mirror.rs`'s header
/// puts the discipline in four words: "Nothing durable and unbounded". `pins`
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

    /// The highest DRIVE-FACE offset this bridge has finished handling.
    ///
    /// ## Why the head-resume was a silent loss, and why a replay is now safe
    ///
    /// A3 subscribed `/f/<F>/term/<node>/>` from the log's HEAD on every attach,
    /// and argued it: "`feed-bin` is not idempotent (§6.5), so replaying the
    /// drive face from zero would retype every command the session was ever
    /// driven with". That was true for A3 and is false as of A6. The key
    /// `on_term_record` mints is `{epoch}:{producer}:{off+1}` — the producer id
    /// is derived from the node id the state dir mints ONCE and keeps forever, so
    /// it is identical across bridge restarts, and the endpoint's mark is a
    /// monotone per-`(Realm::Bridge, producer)` high-water that answers `dup`
    /// and writes NOTHING below its water. After an aterm restart the epoch
    /// differs and the record is refused before a claim is made. A replay from a
    /// lower offset can no longer retype anything.
    ///
    /// Meanwhile the loss the head-resume caused was unmitigated and completely
    /// silent: a `term/in` published while the bridge was between incarnations —
    /// a broker hiccup, the supervisor's relaunch window, a back-off of up to 30 s
    /// after a crash loop — was never seen, never fed, never refused, and named
    /// by no `ev` at all. The doc on this very file called that out as A3's
    /// defect while the code still did it.
    ///
    /// So the face resumes from HERE, and the cursor is written after each record
    /// is handled. Losing the file costs a resume at the head — exactly A3's
    /// behaviour, never worse.
    #[must_use]
    pub fn term_off(&self) -> Option<u64> {
        self.read("term-off").and_then(|s| s.parse().ok())
    }

    /// Record the drive-face offset this bridge has finished handling. MONOTONE,
    /// for the same reason every other watermark here is.
    ///
    /// # Errors
    ///
    /// Any I/O failure.
    pub fn set_term_off(&self, off: u64) -> io::Result<()> {
        if self.term_off().is_some_and(|have| have >= off) {
            return Ok(());
        }
        self.write("term-off", &off.to_string())
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

    /// The `term/in` this bridge is FEEDING right now, if it is feeding one.
    ///
    /// ## Why this file exists (§6.5's last DESIGNED row, the bridge half)
    ///
    /// `feed-bin` reaches a PTY. A bridge that dies between "wrote the bytes"
    /// and "recorded that it wrote them" has two bad options, and A3 took the
    /// second: replay, and the keystroke is typed twice; do not replay, and it
    /// is lost. A3 subscribes the drive face from the HEAD on every attach, so
    /// the record is never seen again — a silent loss, which is the failure the
    /// design says is not acceptable either.
    ///
    /// A6 built the endpoint half: `feed-bin … id=<epoch>:<producer>:<seq>` keeps
    /// a per-session, per-producer high-water mark and answers an already-consumed
    /// sequence `OK dup=1` **without writing**, or `ERR in-doubt seq=<n>` when the
    /// outcome of that exact attempt is genuinely unknown. That turns a replay
    /// from a gamble into a QUESTION — but only for a bridge that can still ask
    /// it, which means one that wrote down what it was about to do BEFORE it did
    /// it, and under which key.
    ///
    /// This is that record. ONE ENTRY, because at most one feed is ever
    /// UNRESOLVED: written before the verb, and cleared once the outcome is on
    /// the bus as an `ev`.
    ///
    /// "Unresolved" and not "in flight", because the entry deliberately outlives
    /// the verb. `Bridge::record_feed_outcome` keeps it whenever the verdict is
    /// not final — `ERR busy`, `ERR halted`, `ERR rate`, or an aterm that did
    /// not answer — so the same record can be asked again under the same key.
    /// The single slot is therefore a rule the bridge has to KEEP rather than a
    /// fact it gets for free: `Bridge::settle_pending_feed_before` resolves or
    /// publishes-and-retires a pending entry before a new drive record may take
    /// the slot, so no offset ever leaves this file without a verdict on the
    /// bus. A3 overwrote it, which lost the keystroke silently.
    #[must_use]
    pub fn feed_intent(&self) -> Option<FeedIntent> {
        FeedIntent::parse(&self.read("feeding")?)
    }

    /// Write down the feed about to be attempted, DURABLY, before the verb.
    ///
    /// # Errors
    ///
    /// Any I/O failure — and the caller must NOT feed if this fails, because an
    /// unjournalled feed is exactly the in-doubt window this file closes.
    pub fn set_feed_intent(&self, intent: &FeedIntent) -> io::Result<()> {
        self.write("feeding", &intent.render())
    }

    /// Forget a resolved feed. Called only after its outcome has been RECORDED,
    /// so the file outlives every moment in which it could still be needed.
    pub fn clear_feed_intent(&self) {
        let _ = std::fs::remove_file(self.path("feeding"));
    }

    /// The directory itself, for a caller writing something this type does not
    /// model (the wake socket, `glance.json`).
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

    /// THE FEED INTENT IS ONE ENTRY, IT SURVIVES A REOPEN, AND A DAMAGED LINE
    /// READS AS NOTHING. Losing it loses a replay; MIS-reading it would feed a
    /// session the bridge was never asked to feed, so every partial parse
    /// answers `None`.
    #[test]
    fn a_feed_intent_round_trips_and_a_damaged_one_reads_as_nothing() {
        let dir = scratch("feeding");
        let st = StateDir::open(&dir).expect("open");
        assert_eq!(st.feed_intent(), None);
        let intent = FeedIntent {
            off: 4_211,
            sid: "s-abcdef0123456789".to_string(),
            key: "0123456789abcdef0123456789abcdef:77:4212".to_string(),
            tries: 3,
            first_at: 1_700_000_000_123,
            doubt: true,
        };
        st.set_feed_intent(&intent).expect("journal");
        assert_eq!(
            StateDir::open(&dir).expect("reopen").feed_intent(),
            Some(intent.clone())
        );
        st.clear_feed_intent();
        assert_eq!(st.feed_intent(), None);

        for damaged in [
            "",
            "off=1 sid=s-a",             // no key
            "off=1 key=a:b:c",           // no sid
            "sid=s-a key=a:b:c",         // no offset
            "off=nan sid=s-a key=a:b:c", // an offset that is not one
            "off=1 sid= key=a:b:c",      // an empty sid
            "off=1 sid=s-a key=a:b",     // a two-part key is not a key
            "off=1 sid=s-a key=a:b:c:d", // nor a four-part one
        ] {
            assert_eq!(FeedIntent::parse(damaged), None, "{damaged:?}");
        }
        // A line from before `tries=` existed counts as one attempt already
        // made: the budget can only shrink on a format change, never grow.
        assert_eq!(
            FeedIntent::parse("off=1 sid=s-a key=a:b:c").map(|i| i.tries),
            Some(1)
        );
        // AND THE TWO FIELDS THE DURATION BUDGET ADDED READ CONSERVATIVELY when
        // they are absent: an unknown start is `0`, which
        // `Bridge::resolve_pending_feed` stamps with the clock rather than
        // treating as a budget spent in 1970, and an unknown history is DOUBT,
        // which publishes `in-doubt` rather than talking an operator out of one.
        let legacy = FeedIntent::parse("off=1 sid=s-a key=a:b:c tries=2").expect("parses");
        assert_eq!(legacy.first_at, 0);
        assert!(legacy.doubt);
        // A round trip carries both.
        let fresh = FeedIntent {
            off: 9,
            sid: "s-a".to_string(),
            key: "a:b:c".to_string(),
            tries: 1,
            first_at: 42,
            doubt: false,
        };
        assert_eq!(FeedIntent::parse(&fresh.render()), Some(fresh));
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
