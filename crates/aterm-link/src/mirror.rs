// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! THE BUS MIRROR, BRIDGE HALF, its pure parts (`docs/DESIGN-session-log-2026-10-15.md`
//! §5.5): the `[fabric] log` mode, a take's slots as the
//! endpoint frames them, the PIN that holds one batch across a crash, the
//! landing line and its answer, the rate bucket and the node's alias key
//! (minted once, kept durably in `logkey` and copied into the pin,
//! [`Pin::key`]). The bridge's wiring (`bridge_mirror.rs`) does the I/O.
//!
//! WHAT OUTLIVES A REBOOT HAS A DURABLE COPY. The pin is atomic only, so a
//! power cut may roll it back or tear it, and two things in it no reboot ends:
//! the alias key and the node's lifetime total. The
//! key is written durably to `logkey` before the first take that names it, so
//! no alias derived from it reaches the bus before it is on the media; the
//! total has a durable FLOOR, `logfloor`, raised by [`FLOOR_LEASE_BYTES`] past
//! a batch's bytes before any byte of the batch is sent, so it is at or above
//! every byte the node ever mirrored. A pin a power cut tore is then set aside
//! like any torn file, a whole one written from the copies in its place, and
//! the mirror resumes at the same key with the floor as its total: no second
//! alias, never a total below what was counted (`StateDir::reconcile_logpin`).
//! The copies hold only while no build that keeps neither writes the pin, so
//! this build's pins are of a version an older build refuses ([`Pin`]'s
//! `v=2`). A recovered `v=1` pin is replaced durably by `v=2` before any
//! copies are made or recovered slot sent, even while its landing is owed.
//!
//! ONE BATCH IN FLIGHT. A take (`log-take`) is pinned before a byte of it is
//! sent: each slot gets a producer sequence from the bridge's reserved block,
//! and the pin (`logpin`, 0600, replaced atomically) names each slot's
//! sequence, session, cursor and body. A bridge killed at any point after the
//! pin republishes exactly the pinned bytes at exactly the pinned sequences,
//! which the broker answers with the original offsets, and a deduped answer
//! is read back and compared before it counts: a foreign record
//! at a pinned sequence is a collision, never a landing. Only when every slot
//! is on the bus does the bridge say `log-landed`, and only once every
//! session has answered for its pairs is the pin emptied. Readers dedupe by
//! the body's `(node, sid, ep, seq)`, so what the pin buys is a
//! duplicate-free bus in the ordinary case, not a correctness claim.

use std::fmt::Write as _;

use aterm_slog::BusCursor;

/// `[fabric] log`: whether the bridge mirrors the session logs.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum LogMode {
    /// THE DEFAULT (owner decision D4): the
    /// bus class is mirrored while the broker RETAINS it, its `Hello`
    /// advertising a prefix rule over this node's `ev` faces
    /// ([`crate::broker_rules::retains_ev`]), as `aterm link broker` does by
    /// default. Over any other broker nothing is asked, taken or published,
    /// and `fabric status` says `mirror=unretained`.
    #[default]
    Auto,
    /// `[fabric] log = "off"`, the opt-out: nothing is asked, taken, pinned
    /// or published; the bridge's lanes carry the bytes they did.
    Off,
    /// `[fabric] log = "turns"`: the bus class of every session log is
    /// mirrored onto its session's `ev` face over any broker; over one that
    /// retains nothing, up to [`TOTAL_MAX_BYTES`] (`mirror=capped`).
    Turns,
}

impl LogMode {
    /// The mode a `--log`/`[fabric] log` word names.
    #[must_use]
    pub fn parse(word: &str) -> Option<LogMode> {
        match word {
            "auto" => Some(LogMode::Auto),
            "off" => Some(LogMode::Off),
            "turns" => Some(LogMode::Turns),
            _ => None,
        }
    }

    /// The word.
    #[must_use]
    pub const fn word(self) -> &'static str {
        match self {
            LogMode::Auto => "auto",
            LogMode::Off => "off",
            LogMode::Turns => "turns",
        }
    }
}

/// The most slots one batch takes: half the publisher's pipeline window, so
/// a batch never fills it and another publish never waits behind it in line.
pub const BATCH_MAX: usize = 32;
/// The rate bucket's sustained records a second, before retention exists in
/// the broker (§8.1): past it records wait in their rings and become exact
/// covers if evicted.
pub const RATE_PER_SEC: u64 = 10;
/// The rate bucket's burst.
pub const RATE_BURST: u64 = 2048;
/// THE NODE'S TOTAL of mirrored body bytes on a broker that retains nothing:
/// past it the mirror stops, said in `fabric status`
/// (`mirror=capped`). 256 MiB of bodies is about two million records, some
/// two hundred days of a busy agent node at the sustained rate's typical use.
/// A lifetime gate, so it holds only where nothing is ever given up: on a
/// broker whose `Hello` advertises a prefix rule over this node's `ev` faces
/// ([`crate::broker_rules::retains_ev`]) the [`NODE_WINDOW_BYTES`] window
/// budget replaces it.
pub const TOTAL_MAX_BYTES: u64 = 256 * 1024 * 1024;
/// THE NODE'S WINDOW BUDGET on a retaining broker (§5.5's "64 MiB per node"): the mirrored body bytes this
/// node landed at offsets at or above the broker's `oldest=` for its `ev`
/// faces are at most this, held by the producer. The broker's bulk stream
/// keeps 256 MiB ([`crate::broker_rules::EV_BYTES`]): four nodes at their
/// full share. Past it the mirror WAITS (`mirror=budget`), asks `oldest=`
/// again at its retry cadence, and goes on once the broker's cut has passed
/// enough of it; records wait in their rings and become exact covers if
/// evicted, as past [`RATE_PER_SEC`]. It never stops for good.
pub const NODE_WINDOW_BYTES: u64 = 64 * 1024 * 1024;
/// The most entries the pin's window ledger keeps ([`Pin::win`]): past it the
/// two NEIGHBOURS WHOSE BYTES SUM LEAST are merged into one (the higher
/// offset, the bytes summed, [`Pin::note_window`]). A merge only ever counts
/// bytes in the window longer, never shorter, and by at most one entry's
/// bytes: of 64 neighbour pairs, 32 are disjoint, so the cheapest sums to at
/// most 1/32 of the ledger, which the budget keeps near [`NODE_WINDOW_BYTES`]
/// (2 MiB of a full window). So a node at its budget goes on once the cut has
/// passed that much more than its exact share, whatever its batch sizes.
pub const WINDOW_LEDGER_MAX: usize = 64;
/// THE FLOOR'S LEASE: how far past a batch's
/// bytes `logfloor` is raised, durably, when the batch would pass it — so the
/// floor costs one durable write per MiB of bodies (some eight thousand
/// records, minutes at the sustained rate). The price is paid at each reboot
/// after an unclean stop, which cannot tell the newest pin from an older
/// one a power cut brought back: the total resumes at the floor, at most
/// this much AND ONE BATCH'S BODIES ([`BATCH_MAX`]) more than was mirrored
/// — the batch in flight is in the floor, and when it lands again its bytes
/// are counted once more — so some 256 such reboots in a node's life bring
/// [`TOTAL_MAX_BYTES`] nearer by what they did not send. Never a byte less.
pub const FLOOR_LEASE_BYTES: u64 = 1024 * 1024;

/// One slot of a take: its session, its cursor and its `v=2` body.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct TakeSlot {
    /// The session's sid.
    pub sid: String,
    /// Where the session lands once this slot and those before it are on the bus.
    pub at: BusCursor,
    /// The body.
    pub body: String,
}

/// A `log-take` reply read: `OK <nbytes> slots=<k>[ more=1]` (or `OK 0
/// slots=0 stale=1`) and its body of `<sid> <ep>:<seq>:<ms> <body>` lines.
/// Answers the slots and whether the endpoint holds more.
///
/// # Errors
///
/// A header without its count, a line that does not read, or a count that
/// is not the lines'.
pub fn parse_take(header: &str, body: &[u8]) -> Result<(Vec<TakeSlot>, bool), String> {
    let mut toks = header.split_whitespace();
    if toks.next() != Some("OK") {
        return Err(format!("log-take answered {header:?}"));
    }
    let _bytes = toks.next();
    let mut want: Option<usize> = None;
    let mut more = false;
    for t in toks {
        if let Some(k) = t.strip_prefix("slots=") {
            want = k.parse().ok();
        } else if t == "more=1" {
            more = true;
        }
    }
    let want = want.ok_or_else(|| format!("log-take header without slots=: {header:?}"))?;
    let text = std::str::from_utf8(body).map_err(|_| "log-take body is not text".to_string())?;
    let mut slots = Vec::with_capacity(want);
    for line in text.lines() {
        let mut p = line.splitn(3, ' ');
        let (Some(sid), Some(at), Some(body)) = (p.next(), p.next(), p.next()) else {
            return Err(format!("log-take line does not read: {line:?}"));
        };
        let at = BusCursor::parse(at).ok_or_else(|| format!("log-take cursor: {line:?}"))?;
        slots.push(TakeSlot {
            sid: sid.to_string(),
            at,
            body: body.to_string(),
        });
    }
    if slots.len() != want {
        return Err(format!(
            "log-take said {want} slots and sent {}",
            slots.len()
        ));
    }
    Ok((slots, more))
}

/// One pinned slot: the producer sequence it is published at, and the take's slot.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct PinSlot {
    /// The producer sequence.
    pub pseq: u64,
    /// The session.
    pub sid: String,
    /// Its cursor.
    pub at: BusCursor,
    /// Its body, exactly as published.
    pub body: String,
}

/// THE PIN: the one batch in flight, the node's running total of mirrored
/// body bytes, the node's ALIAS KEY and its WINDOW
/// LEDGER, as `logpin` holds them.
///
/// ```text
/// logpin v=2 inc=<inc> n=<k> total=<bytes> key=<32 hex> win=<off>:<bytes>,…
/// <pseq> <sid> <ep>:<seq>:<ms> <body>        (k lines)
/// ```
///
/// `win=` is read past by a reader from
/// before it, as every word after `total=` is; a pin without it has an empty
/// ledger.
///
/// `n=0` is a bridge between batches. A pin that does not read stops the
/// mirror for this bridge's life and is never deleted — save one a power cut
/// tore, which the first bridge after the unclean stop sets aside once the
/// state dir holds its durable copies (`logkey`, `logfloor`), and the mirror
/// resumes from them. `key=` is minted once, 16 random bytes, the first time
/// a bridge of this state dir mirrors, written durably to `logkey` before any
/// take names it, and kept by every rewrite of the pin: never derived from
/// the node's caps (a renewed cap, a changed grant set, an unguarded broker's
/// empty ring), so an edge's alias on the bus is the same for the node's
/// life, and nobody holding the fleet's mint secret can recompute it.
///
/// `v=2` IS A PIN WITH ITS COPIES. A build before `logkey` and `logfloor`
/// wrote `v=1`, and reads nothing else: it refuses a `v=2` pin as one of
/// another version, stops its mirror and keeps the pin, so it never mirrors
/// past a floor it does not raise nor under a key it minted itself (the
/// bridge binary is whatever `[fabric] command` names, so an older build can
/// run on the same state dir). This build reads both, and writes `v=2` only;
/// its first `v=2` over a `v=1` pin or none is durable, so no power cut
/// brings back a pin that an older build would read. A recovered `v=1` is
/// upgraded before the copies are written or its batch sent; its own durable
/// key and total protect the interval before those copies exist.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Pin {
    /// The incarnation the sequences were reserved under.
    pub inc: u64,
    /// Body bytes this node mirrored, all batches.
    pub total: u64,
    /// The node's alias key (`None`: a pin written before it, or none minted).
    pub key: Option<[u8; 16]>,
    /// THE WINDOW LEDGER: per landed batch, oldest first, the
    /// highest offset it landed at and its body bytes. A batch counts against
    /// [`NODE_WINDOW_BYTES`] while its highest offset is at or above the
    /// broker's `oldest=` ([`Pin::window_bytes`]); at most
    /// [`WINDOW_LEDGER_MAX`] entries ([`Pin::note_window`]).
    pub win: Vec<(u64, u64)>,
    /// The batch.
    pub slots: Vec<PinSlot>,
}

/// A fresh alias key: 16 bytes from the one audited entropy helper, `None`
/// when it fails (the mirror then names every edge `-`, never its sid).
#[must_use]
pub fn mint_alias_key() -> Option<[u8; 16]> {
    let bytes = crate::enable::random_bytes(16).ok()?;
    let mut key = [0u8; 16];
    key.copy_from_slice(bytes.get(..16)?);
    Some(key)
}

/// The head of every pin this build writes ([`Pin::render`]).
pub const PIN_HEADER: &str = "logpin v=2";

/// The head of a pin a build before the durable copies wrote: it reads
/// nothing else ([`Pin`]'s `v=2`).
const OLDER_PIN_HEADER: &str = "logpin v=1";

impl Pin {
    /// A batch of `bytes` landed, the highest of it at `hi`: one ledger
    /// entry; while it holds more than [`WINDOW_LEDGER_MAX`], the two
    /// neighbours whose bytes sum least (the oldest such pair on a tie) are
    /// merged under the higher offset. Merging the two OLDEST instead piled
    /// every batch but the newest few into one entry keyed near the window's
    /// top, which the cut passed only once it had passed nearly all of them:
    /// a node at its budget then waited for days its oldest batches had
    /// already aged out of.
    pub fn note_window(&mut self, hi: u64, bytes: u64) {
        self.win.push((hi, bytes));
        while self.win.len() > WINDOW_LEDGER_MAX {
            let Some(i) = (0..self.win.len() - 1)
                .min_by_key(|&i| self.win[i].1.saturating_add(self.win[i + 1].1))
            else {
                return;
            };
            let (a, b) = (self.win[i], self.win[i + 1]);
            self.win[i + 1] = (a.0.max(b.0), a.1.saturating_add(b.1));
            self.win.remove(i);
        }
    }

    /// The body bytes the ledger counts in the broker's window from `oldest`:
    /// every batch whose highest offset is at or above it.
    #[must_use]
    pub fn window_bytes(&self, oldest: u64) -> u64 {
        self.win
            .iter()
            .filter(|(hi, _)| *hi >= oldest)
            .fold(0u64, |sum, (_, bytes)| sum.saturating_add(*bytes))
    }

    /// Forget the batches the broker's cut at `oldest` gave up whole.
    pub fn prune_window(&mut self, oldest: u64) {
        self.win.retain(|(hi, _)| *hi >= oldest);
    }

    /// Whether `bytes`, whole or cut short, begin as an OLDER BUILD'S pin
    /// (`v=1`): one written by a build that keeps neither `logkey` nor
    /// `logfloor`, so those copies need not cover what it holds. A prefix too
    /// short to name its version is not one: this build wrote no `v=1`, and
    /// once it pinned, no older build writes over its pin.
    #[must_use]
    pub fn by_an_older_build(bytes: &[u8]) -> bool {
        bytes.starts_with(OLDER_PIN_HEADER.as_bytes())
    }

    /// Whether `bytes` begin as a pin of THIS build (`v=2`), which an older
    /// build refuses: what a rewrite of the pin may replace atomically, since
    /// no power cut can then bring back a version an older build reads.
    #[must_use]
    pub fn by_this_build(bytes: &[u8]) -> bool {
        bytes
            .strip_prefix(PIN_HEADER.as_bytes())
            .is_some_and(|rest| rest.starts_with(b" "))
    }

    /// The file's text.
    #[must_use]
    pub fn render(&self) -> String {
        let key = self.key.map_or_else(String::new, |k| {
            format!(" key={}", aterm_codec::hex::encode_lower(&k))
        });
        let mut win = String::new();
        for (i, (hi, bytes)) in self.win.iter().enumerate() {
            let _ = write!(win, "{}{hi}:{bytes}", if i == 0 { " win=" } else { "," });
        }
        let mut out = format!(
            "{PIN_HEADER} inc={} n={} total={}{key}{win}\n",
            self.inc,
            self.slots.len(),
            self.total
        );
        for s in &self.slots {
            let _ = writeln!(out, "{} {} {} {}", s.pseq, s.sid, s.at.word(), s.body);
        }
        out
    }

    /// The file read back.
    ///
    /// # Errors
    ///
    /// Anything that does not read: a header of another version, a slot line
    /// that does not parse, a count that is not the lines' — and a pin that does
    /// not end at the newline every line of it ends with ([`Pin::render`]): a
    /// power cut may leave a prefix, and one cut inside its header still reads
    /// as a pin with a lower total and no key (reset), one
    /// cut inside its last slot as that slot with its body's tail gone. So
    /// what this refuses is exactly what is not PROVABLY WHOLE, the rule the
    /// state dir's sweep sets a torn file aside by (`StateDir::reconcile_logpin`).
    /// An older build's `v=1` pin reads as this build's `v=2` does.
    pub fn parse(text: &str) -> Result<Pin, String> {
        if !text.ends_with('\n') {
            return Err(if text.is_empty() {
                "an empty logpin".to_string()
            } else {
                "a logpin cut short of its last newline".to_string()
            });
        }
        let mut lines = text.lines();
        let head = lines.next().ok_or("an empty logpin")?;
        let mut toks = head.split(' ');
        if !matches!(
            (toks.next(), toks.next()),
            (Some("logpin"), Some("v=1" | "v=2"))
        ) {
            return Err(format!("a logpin of another version: {head:?}"));
        }
        let mut field = |k: &str| -> Result<u64, String> {
            toks.next()
                .and_then(|t| t.strip_prefix(k))
                .and_then(|v| v.strip_prefix('='))
                .and_then(|v| v.parse().ok())
                .ok_or_else(|| format!("a logpin header without {k}=: {head:?}"))
        };
        let inc = field("inc")?;
        let n = field("n")?;
        let total = field("total")?;
        // Past the total, a newer writer's words are read past; `key=` and
        // `win=` are read, and refused when they do not.
        let rest: Vec<&str> = toks.collect();
        let key = match rest.iter().find_map(|t| t.strip_prefix("key=")) {
            None => None,
            Some(hex) => {
                let mut key = [0u8; 16];
                if hex.len() != 32 || !aterm_codec::hex::decode_into(hex.as_bytes(), &mut key) {
                    return Err(format!("a logpin's key: {head:?}"));
                }
                Some(key)
            }
        };
        let win = match rest.iter().find_map(|t| t.strip_prefix("win=")) {
            None => Vec::new(),
            Some(list) => list
                .split(',')
                .map(|e| {
                    let (hi, bytes) = e.split_once(':')?;
                    Some((hi.parse().ok()?, bytes.parse().ok()?))
                })
                .collect::<Option<Vec<(u64, u64)>>>()
                .filter(|w| w.len() <= WINDOW_LEDGER_MAX)
                .ok_or_else(|| format!("a logpin's window ledger: {head:?}"))?,
        };
        let mut slots = Vec::new();
        for line in lines {
            let mut p = line.splitn(4, ' ');
            let (Some(pseq), Some(sid), Some(at), Some(body)) =
                (p.next(), p.next(), p.next(), p.next())
            else {
                return Err(format!("a logpin slot that does not read: {line:?}"));
            };
            slots.push(PinSlot {
                pseq: pseq
                    .parse()
                    .map_err(|_| format!("a logpin slot's sequence: {line:?}"))?,
                sid: sid.to_string(),
                at: BusCursor::parse(at)
                    .ok_or_else(|| format!("a logpin slot's cursor: {line:?}"))?,
                body: body.to_string(),
            });
        }
        if slots.len() as u64 != n {
            return Err(format!("a logpin of {n} slots holds {}", slots.len()));
        }
        Ok(Pin {
            inc,
            total,
            key,
            win,
            slots,
        })
    }
}

/// Each session's last cursor IN EACH SEQ SPACE of `slots`, in the order
/// they first appear: what one `log-landed` names. A take can hold two spaces
/// of one session (an uncarried update's old-space `why=reset` cover, then
/// the new space's records); each lands, or the old space's cover
/// is owed again on every take.
#[must_use]
pub fn landed_pairs(slots: &[PinSlot]) -> Vec<(String, BusCursor)> {
    let mut out: Vec<(String, BusCursor)> = Vec::new();
    for s in slots {
        match out
            .iter_mut()
            .find(|(sid, at)| *sid == s.sid && at.ep == s.at.ep)
        {
            Some(slot) => slot.1 = s.at,
            None => out.push((s.sid.clone(), s.at)),
        }
    }
    out
}

/// The `log-landed` request for `pairs`.
#[must_use]
pub fn landed_line(pairs: &[(String, BusCursor)]) -> String {
    let mut out = String::from("log-landed");
    for (sid, at) in pairs {
        let _ = write!(out, " {sid} {}", at.word());
    }
    out
}

/// What the endpoint answered for one pair of a `log-landed`.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum LandedWord {
    /// The cursor moved.
    Ok,
    /// Already there, or another seq space: on the bus already.
    Stale,
    /// A closed session's tail is all on the bus and forgotten: its `ev` face
    /// may be retired.
    Done,
    /// An update's export holds the cursor: keep the slots, land again later.
    Fenced,
    /// A seamless successor has not adopted the session yet: keep them.
    Adopting,
    /// No such session (after adoption): the slots are on the bus; nothing
    /// more is owed.
    Gone,
    /// Past anything the session's log numbered: nothing to land on.
    Ahead,
}

impl LandedWord {
    /// Whether the pair is settled: nothing more is owed for its slots.
    #[must_use]
    pub const fn settled(self) -> bool {
        !matches!(self, LandedWord::Fenced | LandedWord::Adopting)
    }
}

/// A `log-landed` reply read: one word a pair, or `OK stale=1` from a lane
/// that no longer owns the link (`None`: nothing landed, keep the pin).
///
/// # Errors
///
/// An `ERR`, or a word count that is not the pairs'.
pub fn parse_landed(reply: &str, pairs: usize) -> Result<Option<Vec<LandedWord>>, String> {
    let rest = reply
        .strip_prefix("OK")
        .ok_or_else(|| format!("log-landed answered {reply:?}"))?;
    if rest.trim() == "stale=1" {
        return Ok(None);
    }
    let words: Vec<LandedWord> = rest
        .split_whitespace()
        .map(|w| match w {
            "ok" => Ok(LandedWord::Ok),
            "stale" => Ok(LandedWord::Stale),
            "done" => Ok(LandedWord::Done),
            "fenced" => Ok(LandedWord::Fenced),
            "adopting" => Ok(LandedWord::Adopting),
            "gone" => Ok(LandedWord::Gone),
            "ahead" => Ok(LandedWord::Ahead),
            other => Err(format!("log-landed word {other:?}")),
        })
        .collect::<Result<_, _>>()?;
    if words.len() != pairs {
        return Err(format!(
            "log-landed answered {} words for {pairs} pairs",
            words.len()
        ));
    }
    Ok(Some(words))
}

/// THE RATE BUCKET (§8.1): [`RATE_PER_SEC`] records a second sustained, a
/// [`RATE_BURST`] burst, on a clock the caller passes (milliseconds), so a
/// test steps it.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Bucket {
    /// Thousandths of a record.
    milli: u64,
    at_ms: u64,
}

impl Bucket {
    /// A full bucket at `now_ms`.
    #[must_use]
    pub const fn full(now_ms: u64) -> Bucket {
        Bucket {
            milli: RATE_BURST * 1000,
            at_ms: now_ms,
        }
    }

    /// Refill to `now_ms` and answer how many whole records may go.
    pub fn room(&mut self, now_ms: u64) -> u64 {
        let elapsed = now_ms.saturating_sub(self.at_ms);
        self.at_ms = self.at_ms.max(now_ms);
        self.milli = self
            .milli
            .saturating_add(elapsed.saturating_mul(RATE_PER_SEC))
            .min(RATE_BURST * 1000);
        self.milli / 1000
    }

    /// Spend `n` records.
    pub fn spend(&mut self, n: u64) {
        self.milli = self.milli.saturating_sub(n.saturating_mul(1000));
    }
}

#[cfg(test)]
#[path = "mirror_tests.rs"]
mod tests;
