// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Pure bridge-half bus mirror types: `[fabric] log` modes, take/pin slots, landing replies, and rate bucket.

use std::fmt::Write as _;

use aterm_slog::BusCursor;

/// `[fabric] log`: whether the bridge mirrors the session logs.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum LogMode {
    /// Mirror the bus class while the broker retains this node's `ev` faces (`[fabric] log = "auto"`).
    #[default]
    Auto,
    /// Opt out of mirroring (`[fabric] log = "off"`).
    Off,
    /// Mirror the bus class of every session log onto its `ev` face (`[fabric] log = "turns"`).
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

/// Maximum slots taken in one batch.
pub const BATCH_MAX: usize = 32;
/// Sustained records per second of the rate bucket before broker retention.
pub const RATE_PER_SEC: u64 = 10;
/// The rate bucket's burst.
pub const RATE_BURST: u64 = 2048;
/// Maximum mirrored body bytes on a broker that retains nothing (`mirror=capped`).
pub const TOTAL_MAX_BYTES: u64 = 256 * 1024 * 1024;
/// Per-node mirrored body byte budget in the broker's retention window.
pub const NODE_WINDOW_BYTES: u64 = 64 * 1024 * 1024;
/// Maximum entries kept in the pin's window ledger ([`Pin::win`]).
pub const WINDOW_LEDGER_MAX: usize = 64;
/// Lease step in bytes by which `logfloor` is raised ahead of a batch.
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

/// Parse a `log-take` reply header and body into slots and whether more remain.
///
/// # Errors
/// Missing count in header, malformed slot line, or mismatched slot count.
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

/// Durable `logpin` state: in-flight batch, running byte total, alias key, and window ledger.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Pin {
    /// The incarnation the sequences were reserved under.
    pub inc: u64,
    /// Body bytes this node mirrored, all batches.
    pub total: u64,
    /// The node's alias key (`None`: a pin written before it, or none minted).
    pub key: Option<[u8; 16]>,
    /// Per landed batch (oldest first), its highest offset and body bytes.
    pub win: Vec<(u64, u64)>,
    /// The batch.
    pub slots: Vec<PinSlot>,
}

/// Mint a fresh 16-byte alias key from the entropy helper, or `None` on failure.
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
    /// Record a landed batch of `bytes` at highest offset `hi`, merging smallest neighbours past [`WINDOW_LEDGER_MAX`].
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

    /// Body bytes in the ledger whose highest offset is at or above `oldest`.
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

    /// Whether `bytes` begin as an older build's `v=1` pin.
    #[must_use]
    pub fn by_an_older_build(bytes: &[u8]) -> bool {
        bytes.starts_with(OLDER_PIN_HEADER.as_bytes())
    }

    /// Whether `bytes` begin as this build's `v=2` pin.
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

    /// Parse a rendered `logpin` text.
    ///
    /// # Errors
    /// Unknown version header, malformed line, count mismatch, or missing trailing newline.
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

/// Last cursor of each session in each seq space of `slots`, in first-appearance order.
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
    /// Closed session's tail is all on the bus and forgotten; its `ev` face may be retired.
    Done,
    /// An update's export holds the cursor: keep the slots, land again later.
    Fenced,
    /// A seamless successor has not adopted the session yet: keep them.
    Adopting,
    /// No such session after adoption; nothing more is owed.
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

/// Parse a `log-landed` reply into per-pair answers, or `None` on `OK stale=1`.
///
/// # Errors
/// Reply is an `ERR` or its word count does not match `pairs`.
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

/// Rate bucket of [`RATE_PER_SEC`] sustained records/s and [`RATE_BURST`] burst.
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
