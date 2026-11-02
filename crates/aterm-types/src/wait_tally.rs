// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! The fabric ledger's tally lines. A line is fixed-width and checksummed, so a whole-line write
//! at offset 0 replaces the last one and a read racing it is refused rather than read as half of
//! each. Two processes write one, each with its own rows: the bridge's `wait-tally` loop
//! (`aterm_link::wait_tally`) and aterm's own handlers ([`HandlerTally`]). This holds what they
//! share: the log2 [`Hist`] a row counts in, a row's text ([`put_row`]), the checksum
//! ([`sum_text`]) and the reader of a line's words ([`Words`]).

/// The log2 bands each row counts in ([`Hist::buckets`]).
pub const BUCKETS: usize = 18;

/// What a row's values measure.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Unit {
    /// Nanoseconds of `Instant`, the monotonic clock.
    Ns,
    /// A count. Band 0 is 0.
    Count,
}

impl Unit {
    /// How far a value is shifted before its band is its bit length.
    const fn shift(self) -> u32 {
        match self {
            Unit::Ns => 10,
            Unit::Count => 0,
        }
    }
}

/// One row's values since the process started.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct Hist {
    /// How many values were counted.
    pub count: u64,
    /// Their sum.
    pub total: u64,
    /// The largest value counted.
    pub max: u64,
    /// How many fell in each log2 band ([`Hist::band`]).
    pub buckets: [u32; BUCKETS],
}

impl Hist {
    /// The band `v` falls into.
    #[must_use]
    pub fn band(v: u64, unit: Unit) -> usize {
        let shifted = v >> unit.shift();
        let bits = (u64::BITS - shifted.leading_zeros()) as usize;
        bits.min(BUCKETS - 1)
    }

    /// The least value band `i` holds.
    #[must_use]
    pub fn floor(i: usize, unit: Unit) -> u64 {
        if i == 0 {
            0
        } else {
            (1u64 << (i - 1).min(63)) << unit.shift()
        }
    }

    /// Count `v`.
    pub fn add(&mut self, v: u64, unit: Unit) {
        self.count = self.count.saturating_add(1);
        self.total = self.total.saturating_add(v);
        self.max = self.max.max(v);
        let b = &mut self.buckets[Hist::band(v, unit)];
        *b = b.saturating_add(1);
    }

    /// How many values were at least band `i`'s floor.
    #[must_use]
    pub fn at_least(&self, i: usize) -> u64 {
        self.buckets
            .iter()
            .skip(i)
            .map(|&n| u64::from(n))
            .sum::<u64>()
    }

    /// The floor and (for every band but the last) the bound of the highest band holding a value.
    #[must_use]
    pub fn top(&self, unit: Unit) -> Option<(u64, Option<u64>)> {
        let i = self.buckets.iter().rposition(|&n| n > 0)?;
        let bound = (i + 1 < BUCKETS).then(|| Hist::floor(i + 1, unit));
        Some((Hist::floor(i, unit), bound))
    }

    /// What was counted between `earlier` and `self`.
    ///
    /// # Errors
    /// A count, a total or a band that went backwards.
    pub fn since(&self, earlier: &Hist) -> Result<Hist, String> {
        let back = |what: &str, then: u64, now: u64| {
            format!("its `{what}` went backwards, {then} to {now}")
        };
        let mut buckets = [0u32; BUCKETS];
        for (i, slot) in buckets.iter_mut().enumerate() {
            *slot = self.buckets[i]
                .checked_sub(earlier.buckets[i])
                .ok_or_else(|| {
                    back(
                        &format!("band {i}"),
                        u64::from(earlier.buckets[i]),
                        u64::from(self.buckets[i]),
                    )
                })?;
        }
        Ok(Hist {
            count: self
                .count
                .checked_sub(earlier.count)
                .ok_or_else(|| back("count", earlier.count, self.count))?,
            total: self
                .total
                .checked_sub(earlier.total)
                .ok_or_else(|| back("total", earlier.total, self.total))?,
            max: if self.max > earlier.max { self.max } else { 0 },
            buckets,
        })
    }
}

/// Writes `v` as exactly `out.len()` lowercase hex digits.
fn put_hex(out: &mut [u8], mut v: u64) {
    const DIGITS: &[u8; 16] = b"0123456789abcdef";
    for slot in out.iter_mut().rev() {
        *slot = DIGITS[(v & 0xf) as usize];
        v >>= 4;
    }
}

/// The text of one row: ` <name>=<count>,<total>,<max>,<band>.<band>…`, every number fixed-width
/// hex (16 digits, 8 for a band).
#[must_use]
pub fn row_text(name: &str, h: &Hist) -> Vec<u8> {
    let mut out = vec![b'0'; row_len(name)];
    put_row(&mut out, name, h);
    out
}

/// A row's text's length: its name's, and 214.
#[must_use]
pub const fn row_len(name: &str) -> usize {
    1 + name.len() + 1 + 3 * 17 + BUCKETS * 9 - 1
}

/// [`row_text`] written into `out`, which is exactly [`row_len`] long: the in-place rewrite of one
/// row's place in the line.
pub fn put_row(out: &mut [u8], name: &str, h: &Hist) {
    let name = name.as_bytes();
    out[0] = b' ';
    out[1..=name.len()].copy_from_slice(name);
    let mut at = 1 + name.len();
    out[at] = b'=';
    at += 1;
    for (i, v) in [h.count, h.total, h.max].into_iter().enumerate() {
        if i > 0 {
            out[at] = b',';
            at += 1;
        }
        put_hex(&mut out[at..at + 16], v);
        at += 16;
    }
    out[at] = b',';
    at += 1;
    for (i, &n) in h.buckets.iter().enumerate() {
        if i > 0 {
            out[at] = b'.';
            at += 1;
        }
        put_hex(&mut out[at..at + 8], u64::from(n));
        at += 8;
    }
    debug_assert_eq!(at, out.len());
}

/// The ` sum=<16 hex>\n` tail of a line whose body is `body`.
#[must_use]
pub fn sum_text(body: &[u8]) -> String {
    format!(" sum={:016x}\n", checksum(body))
}

/// THE LINE'S CHECKSUM: aterm-hash's word-at-a-time Fx hash, not FNV-1a. It only has to tell a torn
/// read from a whole line, and the writer pays for it at every write: over a line of about 6 KiB
/// FNV-1a's byte-serial multiply chain was most of the write's cost.
#[must_use]
pub fn checksum(body: &[u8]) -> u64 {
    aterm_hash::fx_hash_bytes(body)
}

/// A checked line's words, each read in its place.
#[derive(Debug)]
pub struct Words<'a> {
    line: &'a str,
    words: std::str::Split<'a, char>,
}

impl<'a> Words<'a> {
    /// The words of `text` after `head`, once its newline and checksum are read.
    ///
    /// # Errors
    /// A line with no newline, no checksum or the wrong one, or another format's.
    pub fn of(text: &'a str, head: &str) -> Result<Self, String> {
        let line = text
            .strip_suffix('\n')
            .ok_or_else(|| format!("an unterminated tally line: {text:?}"))?;
        let (body, sum) = line
            .rsplit_once(" sum=")
            .ok_or_else(|| format!("a tally line with no checksum: {line:?}"))?;
        let want = format!("{:016x}", checksum(body.as_bytes()));
        if sum != want {
            return Err(format!(
                "a tally line whose checksum is {sum}, not {want} (a read that raced the write, \
                 or not a tally): {line:?}"
            ));
        }
        let rest = body
            .strip_prefix(head)
            .and_then(|rest| rest.strip_prefix(' '))
            .ok_or_else(|| format!("not a `{head}` line: {line:?}"))?;
        Ok(Words {
            line,
            words: rest.split(' '),
        })
    }

    /// The value of the word `key=` that must come next.
    ///
    /// # Errors
    /// Another word, or none.
    pub fn next(&mut self, key: &str) -> Result<&'a str, String> {
        let line = self.line;
        self.words
            .next()
            .and_then(|w| w.strip_prefix(key))
            .and_then(|w| w.strip_prefix('='))
            .ok_or_else(|| format!("a tally line with no `{key}=` in its place: {line:?}"))
    }

    /// The decimal value of the word `key=` that must come next.
    ///
    /// # Errors
    /// As [`Words::next`], or a value that is not a number.
    pub fn dec(&mut self, key: &str) -> Result<u64, String> {
        let line = self.line;
        let v = self.next(key)?;
        (!v.is_empty() && v.bytes().all(|b| b.is_ascii_digit()))
            .then(|| v.parse::<u64>().ok())
            .flatten()
            .ok_or_else(|| format!("a tally line whose `{key}` is not a number: {line:?}"))
    }

    /// The process the line belongs to.
    ///
    /// # Errors
    /// As [`Words::dec`], or a value no pid has.
    pub fn pid(&mut self) -> Result<u32, String> {
        let line = self.line;
        u32::try_from(self.dec("pid")?)
            .map_err(|_| format!("a tally line whose pid is not a pid: {line:?}"))
    }

    /// The row `key` that must come next.
    ///
    /// # Errors
    /// As [`Words::next`], or a value that is not a row's fixed-width fields.
    pub fn hist(&mut self, key: &str) -> Result<Hist, String> {
        let line = self.line;
        let hex = |v: &str, width: usize| -> Result<u64, String> {
            if v.len() != width {
                return Err(format!(
                    "a tally line whose `{key}` is not {width} hex digits: {line:?}"
                ));
            }
            if !v.bytes().all(|b| b.is_ascii_hexdigit()) {
                return Err(format!("a tally line whose `{key}` is not hex: {line:?}"));
            }
            u64::from_str_radix(v, 16)
                .map_err(|_| format!("a tally line whose `{key}` is not hex: {line:?}"))
        };
        let value = self.next(key)?;
        let mut fields = value.split(',');
        let mut field = |what: &str| -> Result<&str, String> {
            fields
                .next()
                .ok_or_else(|| format!("a tally line whose `{key}` has no {what}: {line:?}"))
        };
        let count = hex(field("count")?, 16)?;
        let total = hex(field("total")?, 16)?;
        let max = hex(field("max")?, 16)?;
        let bands = field("bands")?;
        if fields.next().is_some() {
            return Err(format!(
                "a tally line whose `{key}` has fields past its bands: {line:?}"
            ));
        }
        let mut buckets = [0u32; BUCKETS];
        let mut parts = bands.split('.');
        for b in &mut buckets {
            let part = parts.next().ok_or_else(|| {
                format!("a tally line whose `{key}` has fewer than {BUCKETS} bands: {line:?}")
            })?;
            *b = u32::try_from(hex(part, 8)?).unwrap_or(u32::MAX);
        }
        if parts.next().is_some() {
            return Err(format!(
                "a tally line whose `{key}` has more than {BUCKETS} bands: {line:?}"
            ));
        }
        Ok(Hist {
            count,
            total,
            max,
            buckets,
        })
    }

    /// The line must end here.
    ///
    /// # Errors
    /// Words past the fields.
    pub fn end(mut self) -> Result<(), String> {
        match self.words.next() {
            None => Ok(()),
            Some(_) => Err(format!(
                "a tally line with words past its fields: {:?}",
                self.line
            )),
        }
    }
}

/// The development seam that turns aterm's handler tally on.
pub const HANDLER_SEAM: &str = "ATERM_FABRIC_HANDLER_TALLY";

/// The file in the instance's state dir ([`crate::dirs::state_dir`]) the tally is written to.
pub const HANDLER_FILE: &str = "handler-tally";

/// What the line starts with: the format's name and version.
const HANDLER_HEAD: &str = "handler-tally v=1";

/// One thing aterm does for the fabric, counted.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum HandlerRow {
    /// `deliver`, its whole body.
    Deliver,
    /// Of a `deliver` or an `outbox sent`, ringing the session's watchers.
    WakeWatchers,
    /// The outbox peek's body.
    Outbox,
    /// `outbox sent`'s body.
    Sent,
    /// `sessions bridge`'s body.
    Sessions,
    /// Every other bridge-only verb's body (`link`, `hello`, `log-take`, `log-landed`).
    OtherBridge,
    /// `post`, from its entry to its push-lane event and the wakes that follow it.
    Post,
    /// `inbox`, the listing.
    Inbox,
    /// `await inbox`, from its entry to its first park, or to its answer if it never parked.
    AwaitArm,
    /// A parked `await inbox`, from its row's arrival in the ring to its answer.
    AwaitWake,
}

/// How many rows a handler line holds.
pub const HANDLER_ROWS: usize = 10;

impl HandlerRow {
    /// Every row, in the line's order.
    pub const ALL: [HandlerRow; HANDLER_ROWS] = [
        HandlerRow::Deliver,
        HandlerRow::WakeWatchers,
        HandlerRow::Outbox,
        HandlerRow::Sent,
        HandlerRow::Sessions,
        HandlerRow::OtherBridge,
        HandlerRow::Post,
        HandlerRow::Inbox,
        HandlerRow::AwaitArm,
        HandlerRow::AwaitWake,
    ];

    /// Its place in the line.
    #[must_use]
    pub const fn index(self) -> usize {
        self as usize
    }

    /// Its name in the line.
    #[must_use]
    pub const fn name(self) -> &'static str {
        match self {
            HandlerRow::Deliver => "deliver",
            HandlerRow::WakeWatchers => "wake_watchers",
            HandlerRow::Outbox => "outbox",
            HandlerRow::Sent => "sent",
            HandlerRow::Sessions => "sessions",
            HandlerRow::OtherBridge => "other_bridge",
            HandlerRow::Post => "post",
            HandlerRow::Inbox => "inbox",
            HandlerRow::AwaitArm => "await_arm",
            HandlerRow::AwaitWake => "await_wake",
        }
    }
}

/// aterm's handlers' time since this process started.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct HandlerTally {
    /// The aterm process the counts belong to.
    pub pid: u32,
    /// Its monotonic clock at the write, in nanoseconds since its recorder started.
    pub at_ns: u64,
    /// Each [`HandlerRow`], at its [`HandlerRow::index`].
    pub rows: [Hist; HANDLER_ROWS],
}

impl HandlerTally {
    /// One row.
    #[must_use]
    pub const fn row(&self, r: HandlerRow) -> &Hist {
        &self.rows[r.index()]
    }

    /// The line [`HandlerTally::parse`] reads, newline included. Every line is one length, so a
    /// write at offset 0 replaces the last line whole.
    #[must_use]
    pub fn render(&self) -> String {
        let mut body =
            format!("{HANDLER_HEAD} pid={:010} at={:020}", self.pid, self.at_ns).into_bytes();
        for row in HandlerRow::ALL {
            body.extend_from_slice(&row_text(row.name(), self.row(row)));
        }
        let sum = sum_text(&body);
        body.extend_from_slice(sum.as_bytes());
        String::from_utf8(body).unwrap_or_default()
    }

    /// Read a line [`HandlerTally::render`] wrote.
    ///
    /// # Errors
    /// A line that is not one.
    pub fn parse(text: &str) -> Result<HandlerTally, String> {
        let mut words = Words::of(text, HANDLER_HEAD)?;
        let pid = words.pid()?;
        let at_ns = words.dec("at")?;
        let mut rows = [Hist::default(); HANDLER_ROWS];
        for (slot, row) in rows.iter_mut().zip(HandlerRow::ALL) {
            *slot = words.hist(row.name())?;
        }
        words.end()?;
        Ok(HandlerTally { pid, at_ns, rows })
    }

    /// What the handlers did between `earlier` and this tally.
    ///
    /// # Errors
    /// Two processes' tallies, or a clock or a row that went backwards.
    pub fn since(&self, earlier: &HandlerTally) -> Result<HandlerTally, String> {
        if self.pid != earlier.pid {
            return Err(format!(
                "the handler tallies are of two aterm processes, pid {} and then {}",
                earlier.pid, self.pid
            ));
        }
        let at_ns = self.at_ns.checked_sub(earlier.at_ns).ok_or_else(|| {
            format!(
                "the handler tally's clock went backwards, {} to {}",
                earlier.at_ns, self.at_ns
            )
        })?;
        let mut rows = [Hist::default(); HANDLER_ROWS];
        for (i, slot) in rows.iter_mut().enumerate() {
            *slot = self.rows[i].since(&earlier.rows[i]).map_err(|why| {
                format!("the handler tally's `{}`: {why}", HandlerRow::ALL[i].name())
            })?;
        }
        Ok(HandlerTally {
            pid: self.pid,
            at_ns,
            rows,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sample() -> HandlerTally {
        let mut t = HandlerTally {
            pid: 4242,
            at_ns: 900_000_000,
            ..HandlerTally::default()
        };
        t.rows[HandlerRow::Deliver.index()].add(120_000, Unit::Ns);
        t.rows[HandlerRow::Deliver.index()].add(40_000_000, Unit::Ns);
        t.rows[HandlerRow::Post.index()].add(9_000, Unit::Ns);
        t.rows[HandlerRow::AwaitWake.index()].add(31_000, Unit::Ns);
        t
    }

    #[test]
    fn a_handler_line_round_trips_at_one_length() {
        let t = sample();
        let line = t.render();
        assert!(
            line.starts_with("handler-tally v=1 pid=0000004242 at="),
            "{line}"
        );
        assert_eq!(HandlerTally::parse(&line), Ok(t));
        let zero = HandlerTally::default();
        let full = HandlerTally {
            pid: u32::MAX,
            at_ns: u64::MAX,
            rows: [Hist {
                count: u64::MAX,
                total: u64::MAX,
                max: u64::MAX,
                buckets: [u32::MAX; BUCKETS],
            }; HANDLER_ROWS],
        };
        assert_eq!(zero.render().len(), full.render().len());
        assert_eq!(zero.render().len(), line.len());
        assert_eq!(HandlerTally::parse(&full.render()), Ok(full));
        assert_eq!(HandlerTally::parse(&zero.render()), Ok(zero));
    }

    #[test]
    fn a_torn_or_foreign_handler_line_is_refused() {
        let old = sample().render();
        let mut later = sample();
        later.rows[HandlerRow::Sent.index()].add(300_000, Unit::Ns);
        later.at_ns += 1;
        let new = later.render();
        assert_eq!(old.len(), new.len());
        for cut in 1..old.len() {
            let torn = format!("{}{}", &new[..cut], &old[cut..]);
            if torn != old && torn != new {
                assert!(HandlerTally::parse(&torn).is_err(), "cut {cut}");
            }
        }
        for bad in [
            "",
            "handler-tally v=1\n",
            &old[..old.len() - 1],
            &old.replace("handler-tally v=1", "handler-tally v=2"),
            &old.replace("handler-tally v=1", "wait-tally v=2"),
        ] {
            assert!(HandlerTally::parse(bad).is_err(), "{bad:?}");
        }
    }

    #[test]
    fn the_difference_of_two_handler_lines_is_one_processs() {
        let earlier = sample();
        let mut later = earlier;
        later.at_ns += 5;
        later.rows[HandlerRow::Post.index()].add(1_000, Unit::Ns);
        let d = later.since(&earlier).expect("one process");
        assert_eq!(d.at_ns, 5);
        assert_eq!(d.row(HandlerRow::Post).count, 1);
        assert_eq!(d.row(HandlerRow::Post).total, 1_000);
        assert_eq!(d.row(HandlerRow::Deliver).count, 0);
        let other = HandlerTally { pid: 1, ..later };
        assert!(other.since(&earlier).is_err());
        assert!(earlier.since(&later).is_err(), "backwards");
    }

    #[test]
    fn every_row_has_its_own_name_and_place() {
        for (i, row) in HandlerRow::ALL.into_iter().enumerate() {
            assert_eq!(row.index(), i);
        }
        let mut names: Vec<&str> = HandlerRow::ALL.iter().map(|r| r.name()).collect();
        names.sort_unstable();
        names.dedup();
        assert_eq!(names.len(), HANDLER_ROWS);
    }
}
