// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! The rows a Mac publishes on `metrics.log` (design §4.14), and their one codec.
//!
//! | Row | Subject | What it holds |
//! |---|---|---|
//! | [`QuotaRow`] | `shared/quota/<cli>/<account>` | an account's condition, both windows, wall, last use, sign-in, sessions and plan |
//! | [`AgyRow`] | `shared/quota/agy/<home>` | an agy home's pools: each one's share left and refill |
//! | [`PlaceRow`] | `shared/now/place/<account>` | where an account is placed here (`fill`, `borrow`, …) |
//! | [`NeedsRow`] | `shared/now/needs` | the sessions that need a person, held sessions and managers |
//! | [`UnreadRow`] | `shared/now/unread` | the account pools this Mac could not read |
//! | [`WindowRecord`] | `shared/1m/…`, `shared/1h/…` | one window's summary: origin, metric, window, revision |
//!
//! **Words and numbers only.** No row field is for an address, password, token, cookie, URL,
//! path, prompt, transcript text, command line or provider error text (§4.14 "Who can read
//! what"): every text field is a [`Word`] (`[a-z0-9_-]`), a roster [`Label`], a key
//! ([`AccountKey`], [`HomeKey`]), a session id ([`Sid`]) or a node id ([`Node`]), each checked
//! on the way in and on the way out. Those grammars refuse an email address, a URL, a path, a
//! `host:port` and a sentence; a roster label is the person's own dotted name for an account
//! (`work.max-2`), which the roster's grammar allows and this one keeps. A word the reader does
//! not know (a condition a later build adds) still reads, as itself.
//!
//! **One encoding per value.** Every row starts with a kind byte and a version byte; every
//! number after them is an unsigned LEB128 varint, every text a length byte and its bytes,
//! every `Option` a tag byte (0 none, 1 some) before its value, every list a count before
//! its entries. Decoding is strict: a different kind, an unknown version, a truncation,
//! trailing bytes, a non-minimal varint, a tag or bool byte other than 0 or 1, a text its
//! grammar refuses, a bound passed, or a body that disagrees with its subject is refused,
//! never misread. So every accepted body is the one [`Row::encode`] writes for its value,
//! and a publisher detects a change by comparing bytes.
//!
//! **A reader takes what a writer writes.** A list longer than its bound is written to its
//! bound (its first entries), and a percent or a share past its bound at the bound, so no
//! reader refuses a row a publisher made; a value within the bounds reads back as itself.
//!
//! ```text
//! QuotaRow     'Q' 1 | at | cli | account | label | condition | enrolled | ?pct5 | ?pct7
//!                    | ?five_hour_reset | ?week_reset | ?walled_until | ?last_used
//!                    | ?(signin state | t)
//!                    | ?last_start | ?last_end | sessions_7d | ?plan
//! AgyRow       'A' 1 | at | home | n | n × (id | ?remaining | ?reset_at | off)
//! PlaceRow     'P' 1 | at | account | place | since | until
//! NeedsRow     'N' 1 | at | n | n × (sid | word | harness | ?worker_of | ?since)
//!                    | n | n × (sid | hold | ?by | ?since) | n | n × (sid | workers)
//! UnreadRow    'U' 1 | at | n | n × pool
//! WindowRecord 'W' 1 | id len | astream SummaryId | revision | astream Summary (to the end)
//! ```
//!
//! Times are Unix seconds, as the harness and the journal keep them, except a window
//! record's window, which is astream's (Unix milliseconds, aligned). Percents are whole
//! points, as `aterm harness accounts` shows them, at most [`PCT_MAX`]; an agy pool's share
//! left is in basis points, at most [`REMAINING_MAX`].

use std::fmt;

use astream_wire::summary::{DecodeError, Summary, SummaryId, Window};

use crate::subject::{AccountKey, Cli, HomeKey, Metric, Node, Now, Subject, Width};

/// The kind byte of a [`QuotaRow`].
pub const KIND_QUOTA: u8 = b'Q';
/// The kind byte of an [`AgyRow`].
pub const KIND_AGY: u8 = b'A';
/// The kind byte of a [`PlaceRow`].
pub const KIND_PLACE: u8 = b'P';
/// The kind byte of a [`NeedsRow`].
pub const KIND_NEEDS: u8 = b'N';
/// The kind byte of an [`UnreadRow`].
pub const KIND_UNREAD: u8 = b'U';
/// The kind byte of a [`WindowRecord`].
pub const KIND_WINDOW: u8 = b'W';
/// The one version of every row this build writes and reads.
pub const VERSION: u8 = 1;

/// The highest percent a window reads: the harness's own bound (`accounts.rs` `PCT_MAX`), past
/// 100 because a provider can report use past its window.
pub const PCT_MAX: u16 = 1000;
/// An agy pool's whole share, in basis points.
pub const REMAINING_MAX: u16 = 10_000;
/// The most pools an [`AgyRow`] holds (agy prints four today).
pub const AGY_GROUPS_MAX: usize = 32;
/// The most entries of each list a [`NeedsRow`] holds.
pub const NEEDS_MAX: usize = 128;
/// The most pools an [`UnreadRow`] names.
pub const UNREAD_MAX: usize = 16;
/// The longest [`Word`], in bytes.
pub const WORD_MAX: usize = 32;
/// The longest [`Label`], in bytes: the harness's (`accounts.rs` `LABEL_MAX`).
pub const LABEL_MAX: usize = 64;

/// Why bytes are not a row, or not the row their subject names.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum RowError {
    /// The bytes end inside a field.
    Truncated,
    /// Bytes after the last field.
    Trailing,
    /// The kind byte names another row.
    Kind {
        /// The row the subject or caller wants.
        expected: u8,
        /// The kind byte found.
        found: u8,
    },
    /// A version this build does not read.
    Version(u8),
    /// A varint that is not minimal or does not fit 64 bits.
    Varint,
    /// A bool or an option tag other than 0 or 1.
    Tag,
    /// A text field its grammar refuses.
    Text(&'static str),
    /// A number past its bound.
    Bound(&'static str),
    /// A list longer than its bound.
    TooMany(&'static str),
    /// The body names another Mac, CLI, account, home, metric or width than its subject.
    Mismatch(&'static str),
    /// A subject whose rows this crate has no codec for: presence (aterm-mesh's), raw samples
    /// and the journal (their sources' own encodings).
    NoCodec,
    /// astream refused the summary or its identity.
    Summary(DecodeError),
}

impl fmt::Display for RowError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            RowError::Truncated => f.write_str("the row ends inside a field"),
            RowError::Trailing => f.write_str("bytes after the row's last field"),
            RowError::Kind { expected, found } => write!(
                f,
                "a {:?} row where a {:?} row belongs",
                char::from(*found),
                char::from(*expected)
            ),
            RowError::Version(v) => write!(f, "row version {v}, this build reads {VERSION}"),
            RowError::Varint => f.write_str("a number that is not a minimal 64-bit varint"),
            RowError::Tag => f.write_str("a flag byte other than 0 or 1"),
            RowError::Text(what) => write!(f, "{what} is not in its grammar"),
            RowError::Bound(what) => write!(f, "{what} is past its bound"),
            RowError::TooMany(what) => write!(f, "too many {what}"),
            RowError::Mismatch(what) => write!(f, "the row's {what} is not its subject's"),
            RowError::NoCodec => f.write_str("no row codec for this subject here"),
            RowError::Summary(e) => write!(f, "the summary: {e}"),
        }
    }
}

impl std::error::Error for RowError {}

// ---------------------------------------------------------------------------
// Text fields
// ---------------------------------------------------------------------------

/// A word from one of the harness's vocabularies (a condition, a sign-in state, a plan, a
/// place, a need, a hold, a pool, an agy bucket id): 1..=[`WORD_MAX`] bytes of `[a-z0-9_-]`,
/// starting with a letter or digit. It cannot spell an address, a URL, a path or a sentence.
#[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct Word(Box<str>);

impl Word {
    /// `word`, when the grammar takes it.
    #[must_use]
    pub fn new(word: &str) -> Option<Word> {
        let b = word.as_bytes();
        let first_ok = b
            .first()
            .is_some_and(|c| c.is_ascii_lowercase() || c.is_ascii_digit());
        let rest_ok = b
            .iter()
            .all(|&c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == b'-' || c == b'_');
        (first_ok && rest_ok && b.len() <= WORD_MAX).then(|| Word(word.into()))
    }

    /// The word.
    #[must_use]
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl fmt::Display for Word {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.0)
    }
}

/// An account's roster label, by the harness's grammar (`accounts.rs` `label_refusal`):
/// `[a-z0-9][a-z0-9._-]{0,63}`. It has no `@`, so it is never an email address.
#[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct Label(Box<str>);

impl Label {
    /// `label`, when the grammar takes it.
    #[must_use]
    pub fn new(label: &str) -> Option<Label> {
        let b = label.as_bytes();
        let first_ok = b
            .first()
            .is_some_and(|c| c.is_ascii_lowercase() || c.is_ascii_digit());
        let rest_ok = b.iter().all(|&c| {
            c.is_ascii_lowercase() || c.is_ascii_digit() || c == b'.' || c == b'_' || c == b'-'
        });
        (first_ok && rest_ok && b.len() <= LABEL_MAX).then(|| Label(label.into()))
    }

    /// The label.
    #[must_use]
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl fmt::Display for Label {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.0)
    }
}

/// A minted session id: `s-` and 20 lowercase hex digits (`aterm-session` `id::is_minted_sid`).
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct Sid([u8; 10]);

impl Sid {
    /// `s-<20 lowercase hex>`; anything else is `None`.
    #[must_use]
    pub fn parse(word: &str) -> Option<Sid> {
        crate::subject::parse_hex::<10>(word.strip_prefix("s-")?).map(Sid)
    }

    #[must_use]
    const fn to_ascii_bytes(self) -> [u8; 22] {
        crate::subject::format_hex_lower::<10, 22>(b"s-", &self.0)
    }
}

impl fmt::Display for Sid {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let buf = self.to_ascii_bytes();
        f.write_str(std::str::from_utf8(&buf).unwrap_or_default())
    }
}

// ---------------------------------------------------------------------------
// The codec
// ---------------------------------------------------------------------------

/// The writer: a kind and version, then fields.
struct Out(Vec<u8>);

impl Out {
    fn with_capacity(kind: u8, cap: usize) -> Out {
        let mut v = Vec::with_capacity(cap.max(2));
        v.push(kind);
        v.push(VERSION);
        Out(v)
    }

    fn varint(&mut self, mut v: u64) {
        loop {
            let low = u8::try_from(v & 0x7f).unwrap_or(0);
            v >>= 7;
            if v == 0 {
                self.0.push(low);
                return;
            }
            self.0.push(low | 0x80);
        }
    }

    fn flag(&mut self, b: bool) {
        self.0.push(u8::from(b));
    }

    fn opt(&mut self, v: Option<u64>) {
        self.flag(v.is_some());
        if let Some(v) = v {
            self.varint(v);
        }
    }

    /// A text of at most 255 bytes: every text type here is shorter.
    fn text(&mut self, s: &str) {
        self.bytes(s.as_bytes());
    }

    fn bytes(&mut self, b: &[u8]) {
        let len = u8::try_from(b.len()).unwrap_or(u8::MAX);
        self.0.push(len);
        self.0
            .extend_from_slice(b.get(..usize::from(len)).unwrap_or_default());
    }

    fn opt_text(&mut self, s: Option<&str>) {
        self.flag(s.is_some());
        if let Some(s) = s {
            self.text(s);
        }
    }

    fn sid(&mut self, sid: Sid) {
        self.bytes(&sid.to_ascii_bytes());
    }

    fn opt_sid(&mut self, sid: Option<Sid>) {
        self.flag(sid.is_some());
        if let Some(sid) = sid {
            self.sid(sid);
        }
    }

    fn opt_node(&mut self, node: Option<Node>) {
        self.flag(node.is_some());
        if let Some(node) = node {
            self.bytes(&node.to_ascii_bytes());
        }
    }

    fn account(&mut self, account: AccountKey) {
        self.bytes(&account.to_ascii_bytes());
    }

    fn home(&mut self, home: HomeKey) {
        match home {
            HomeKey::Home => self.bytes(b"home"),
            HomeKey::Hashed(h) => {
                let bytes = h.to_be_bytes();
                let buf = crate::subject::format_hex_lower::<8, 17>(b"h", &bytes);
                self.bytes(&buf);
            }
        }
    }

    /// A percent, at most [`PCT_MAX`].
    fn pct(&mut self, p: Option<u16>) {
        self.opt(p.map(|p| u64::from(p.min(PCT_MAX))));
    }

    /// The count of a list written to its bound: the entries to write.
    fn count<T>(&mut self, list: &[T], max: usize) -> usize {
        let n = list.len().min(max);
        self.varint(u64::try_from(n).unwrap_or(u64::MAX));
        n
    }
}

/// The strict reader.
struct In<'a>(&'a [u8]);

impl<'a> In<'a> {
    fn head(bytes: &'a [u8], kind: u8) -> Result<In<'a>, RowError> {
        let mut r = In(bytes);
        let found = r.byte()?;
        if found != kind {
            return Err(RowError::Kind {
                expected: kind,
                found,
            });
        }
        let version = r.byte()?;
        if version != VERSION {
            return Err(RowError::Version(version));
        }
        Ok(r)
    }

    fn byte(&mut self) -> Result<u8, RowError> {
        let (&b, rest) = self.0.split_first().ok_or(RowError::Truncated)?;
        self.0 = rest;
        Ok(b)
    }

    fn take(&mut self, n: usize) -> Result<&'a [u8], RowError> {
        let (head, rest) = self.0.split_at_checked(n).ok_or(RowError::Truncated)?;
        self.0 = rest;
        Ok(head)
    }

    fn varint(&mut self) -> Result<u64, RowError> {
        let mut v: u64 = 0;
        for i in 0..10u32 {
            let b = self.byte()?;
            let low = u64::from(b & 0x7f);
            let shift = i * 7;
            if i == 9 && low > 1 {
                return Err(RowError::Varint);
            }
            v |= low << shift;
            if b & 0x80 == 0 {
                // Minimal: no zero continuation byte after the first.
                if i > 0 && b == 0 {
                    return Err(RowError::Varint);
                }
                return Ok(v);
            }
        }
        Err(RowError::Varint)
    }

    fn flag(&mut self) -> Result<bool, RowError> {
        match self.byte()? {
            0 => Ok(false),
            1 => Ok(true),
            _ => Err(RowError::Tag),
        }
    }

    fn opt(&mut self) -> Result<Option<u64>, RowError> {
        if self.flag()? {
            Ok(Some(self.varint()?))
        } else {
            Ok(None)
        }
    }

    fn text(&mut self) -> Result<&'a str, RowError> {
        let len = self.byte()?;
        let bytes = self.take(usize::from(len))?;
        std::str::from_utf8(bytes).map_err(|_| RowError::Text("a text field"))
    }

    fn word(&mut self, what: &'static str) -> Result<Word, RowError> {
        Word::new(self.text()?).ok_or(RowError::Text(what))
    }

    fn opt_word(&mut self, what: &'static str) -> Result<Option<Word>, RowError> {
        if self.flag()? {
            self.word(what).map(Some)
        } else {
            Ok(None)
        }
    }

    fn sid(&mut self, what: &'static str) -> Result<Sid, RowError> {
        Sid::parse(self.text()?).ok_or(RowError::Text(what))
    }

    fn opt_sid(&mut self, what: &'static str) -> Result<Option<Sid>, RowError> {
        if self.flag()? {
            self.sid(what).map(Some)
        } else {
            Ok(None)
        }
    }

    fn account(&mut self) -> Result<AccountKey, RowError> {
        AccountKey::parse(self.text()?).ok_or(RowError::Text("the account key"))
    }

    fn pct(&mut self, what: &'static str) -> Result<Option<u16>, RowError> {
        match self.opt()? {
            None => Ok(None),
            Some(v) => u16::try_from(v)
                .ok()
                .filter(|&p| p <= PCT_MAX)
                .map(Some)
                .ok_or(RowError::Bound(what)),
        }
    }

    fn u32(&mut self, what: &'static str) -> Result<u32, RowError> {
        u32::try_from(self.varint()?).map_err(|_| RowError::Bound(what))
    }

    fn count(&mut self, max: usize, what: &'static str) -> Result<usize, RowError> {
        usize::try_from(self.varint()?)
            .ok()
            .filter(|&n| n <= max)
            .ok_or(RowError::TooMany(what))
    }

    fn finish(self) -> Result<(), RowError> {
        if self.0.is_empty() {
            Ok(())
        } else {
            Err(RowError::Trailing)
        }
    }
}

// ---------------------------------------------------------------------------
// Quota
// ---------------------------------------------------------------------------

/// A sign-in word and when it was seen: the journal book's (`in`, `out`, `ended`, `wrong`,
/// `failed`).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct SignIn {
    /// The word.
    pub state: Word,
    /// When, Unix seconds.
    pub t: u64,
}

/// An account's current row (§4.14 "Quota row"): what `aterm harness accounts` shows for it on
/// this Mac, and the journal book's facts. About 100 bytes.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct QuotaRow {
    /// When the figures were read, Unix seconds.
    pub at: u64,
    /// The CLI; never agy, whose quota is per home ([`AgyRow`]).
    pub cli: Cli,
    /// The registry's key for the account.
    pub account: AccountKey,
    /// The roster label.
    pub label: Label,
    /// The condition word (`usable`, `low`, `limited`, `signed-out`, `not-enrolled`,
    /// `unseen`), or [`QuotaRow::REMOVED`] once this Mac's roster no longer lists the account.
    pub condition: Word,
    /// Whether its login is made on this Mac.
    pub enrolled: bool,
    /// The 5-hour window's use, whole points.
    pub pct5: Option<u16>,
    /// The week's use, whole points.
    pub pct7: Option<u16>,
    /// When the 5-hour window resets, Unix seconds: the accounts store's `session_reset`.
    pub five_hour_reset: Option<u64>,
    /// When the week resets, Unix seconds.
    pub week_reset: Option<u64>,
    /// When a wall that stands ends, Unix seconds.
    pub walled_until: Option<u64>,
    /// When this Mac last ran it, Unix seconds.
    pub last_used: Option<u64>,
    /// The sign-in word.
    pub signin: Option<SignIn>,
    /// The last session start here, Unix seconds.
    pub last_start: Option<u64>,
    /// The last session end here, Unix seconds.
    pub last_end: Option<u64>,
    /// Sessions started here in the last 7 days.
    pub sessions_7d: u32,
    /// The plan as this Mac's accounts view names it: its word, then `-` and its tier where one
    /// was seen (`max-20x`, `pro`, `plus`).
    pub plan: Option<Word>,
}

impl QuotaRow {
    /// The condition of an account this Mac's roster no longer lists. Its quota class keeps
    /// each subject's last row, so the publisher ends the account's row once, as
    /// [`QuotaRow::ended`], and a reader leaves it out ([`crate::merge::Snapshots::accounts`]).
    pub const REMOVED: &'static str = "removed";

    /// Where `node` publishes it; `None` for a row whose CLI is agy.
    #[must_use]
    pub const fn subject(&self, node: Node) -> Option<Subject> {
        Subject::quota(node, self.cli, self.account)
    }

    /// Whether this Mac's roster no longer lists the account ([`QuotaRow::REMOVED`]).
    #[must_use]
    pub fn removed(&self) -> bool {
        self.condition.as_str() == Self::REMOVED
    }

    /// This row as its account's end on its Mac at `at`: the same CLI, account and label, the
    /// condition [`QuotaRow::REMOVED`], and no figure, reset, wall, use, sign-in, session or
    /// plan, so a reader that does not know the word counts nothing of it either.
    #[must_use]
    pub fn ended(&self, at: u64) -> QuotaRow {
        QuotaRow {
            at,
            cli: self.cli,
            account: self.account,
            label: self.label.clone(),
            condition: Word(Self::REMOVED.into()),
            enrolled: false,
            pct5: None,
            pct7: None,
            five_hour_reset: None,
            week_reset: None,
            walled_until: None,
            last_used: None,
            signin: None,
            last_start: None,
            last_end: None,
            sessions_7d: 0,
            plan: None,
        }
    }

    /// The canonical encoding; each percent at most [`PCT_MAX`].
    #[must_use]
    pub fn encode(&self) -> Vec<u8> {
        let mut o = Out::with_capacity(KIND_QUOTA, 128);
        o.varint(self.at);
        o.text(self.cli.word());
        o.account(self.account);
        o.text(self.label.as_str());
        o.text(self.condition.as_str());
        o.flag(self.enrolled);
        o.pct(self.pct5);
        o.pct(self.pct7);
        o.opt(self.five_hour_reset);
        o.opt(self.week_reset);
        o.opt(self.walled_until);
        o.opt(self.last_used);
        o.flag(self.signin.is_some());
        if let Some(s) = &self.signin {
            o.text(s.state.as_str());
            o.varint(s.t);
        }
        o.opt(self.last_start);
        o.opt(self.last_end);
        o.varint(u64::from(self.sessions_7d));
        o.opt_text(self.plan.as_ref().map(Word::as_str));
        o.0
    }

    /// Decode a canonical encoding.
    ///
    /// # Errors
    /// [`RowError`] for anything [`QuotaRow::encode`] does not write.
    pub fn decode(bytes: &[u8]) -> Result<QuotaRow, RowError> {
        let mut r = In::head(bytes, KIND_QUOTA)?;
        let at = r.varint()?;
        let cli = Cli::parse(r.text()?)
            .filter(|c| c.has_accounts())
            .ok_or(RowError::Text("the CLI"))?;
        let account = r.account()?;
        let label = Label::new(r.text()?).ok_or(RowError::Text("the label"))?;
        let condition = r.word("the condition")?;
        let enrolled = r.flag()?;
        let pct5 = r.pct("the 5-hour percent")?;
        let pct7 = r.pct("the week percent")?;
        let five_hour_reset = r.opt()?;
        let week_reset = r.opt()?;
        let walled_until = r.opt()?;
        let last_used = r.opt()?;
        let signin = if r.flag()? {
            Some(SignIn {
                state: r.word("the sign-in word")?,
                t: r.varint()?,
            })
        } else {
            None
        };
        let last_start = r.opt()?;
        let last_end = r.opt()?;
        let sessions_7d = r.u32("sessions in 7 days")?;
        let plan = r.opt_word("the plan")?;
        r.finish()?;
        Ok(QuotaRow {
            at,
            cli,
            account,
            label,
            condition,
            enrolled,
            pct5,
            pct7,
            five_hour_reset,
            week_reset,
            walled_until,
            last_used,
            signin,
            last_start,
            last_end,
            sessions_7d,
            plan,
        })
    }
}

// ---------------------------------------------------------------------------
// agy
// ---------------------------------------------------------------------------

/// One agy pool, by its bucket id as agy names it (`gemini-weekly`, `3p-5h`, …).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct AgyGroup {
    /// The bucket id.
    pub id: Word,
    /// The share left, basis points; `None` when agy gave none.
    pub remaining: Option<u16>,
    /// When it refills, Unix seconds.
    pub reset_at: Option<u64>,
    /// Whether agy says the bucket is disabled: its share then reads full and means nothing.
    pub off: bool,
}

/// An agy home's current pools (§4.14 "agy row"), with `at` the reading's `as_of`.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct AgyRow {
    /// When agy was read, Unix seconds.
    pub at: u64,
    /// The home.
    pub home: HomeKey,
    /// Its pools, in agy's order.
    pub groups: Vec<AgyGroup>,
}

impl AgyRow {
    /// Where `node` publishes it.
    #[must_use]
    pub const fn subject(&self, node: Node) -> Subject {
        Subject::Agy {
            node,
            home: self.home,
        }
    }

    /// The canonical encoding: its first [`AGY_GROUPS_MAX`] pools, each share at most
    /// [`REMAINING_MAX`].
    #[must_use]
    pub fn encode(&self) -> Vec<u8> {
        let mut o = Out::with_capacity(KIND_AGY, 32 + self.groups.len().min(AGY_GROUPS_MAX) * 24);
        o.varint(self.at);
        o.home(self.home);
        let n = o.count(&self.groups, AGY_GROUPS_MAX);
        for g in self.groups.iter().take(n) {
            o.text(g.id.as_str());
            o.opt(g.remaining.map(|bp| u64::from(bp.min(REMAINING_MAX))));
            o.opt(g.reset_at);
            o.flag(g.off);
        }
        o.0
    }

    /// Decode a canonical encoding.
    ///
    /// # Errors
    /// [`RowError`] for anything [`AgyRow::encode`] does not write.
    pub fn decode(bytes: &[u8]) -> Result<AgyRow, RowError> {
        let mut r = In::head(bytes, KIND_AGY)?;
        let at = r.varint()?;
        let home = HomeKey::parse(r.text()?).ok_or(RowError::Text("the home key"))?;
        let n = r.count(AGY_GROUPS_MAX, "agy pools")?;
        let mut groups = Vec::with_capacity(n);
        for _ in 0..n {
            let id = r.word("an agy bucket id")?;
            let remaining = match r.opt()? {
                None => None,
                Some(v) => Some(
                    u16::try_from(v)
                        .ok()
                        .filter(|&bp| bp <= REMAINING_MAX)
                        .ok_or(RowError::Bound("an agy share"))?,
                ),
            };
            let reset_at = r.opt()?;
            let off = r.flag()?;
            groups.push(AgyGroup {
                id,
                remaining,
                reset_at,
                off,
            });
        }
        r.finish()?;
        Ok(AgyRow { at, home, groups })
    }
}

// ---------------------------------------------------------------------------
// Place
// ---------------------------------------------------------------------------

/// Where an account is placed on this Mac (the journal's `place` vocabulary: `fill`,
/// `borrow`, `stranded`, `probation`), and the span the placement holds for. A place that
/// ended is published with `until` at its end.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct PlaceRow {
    /// When it was written, Unix seconds.
    pub at: u64,
    /// The account.
    pub account: AccountKey,
    /// The place word.
    pub place: Word,
    /// Since when, Unix seconds.
    pub since: u64,
    /// Until when, Unix seconds.
    pub until: u64,
}

impl PlaceRow {
    /// Where `node` publishes it.
    #[must_use]
    pub const fn subject(&self, node: Node) -> Subject {
        Subject::Now {
            node,
            now: Now::Place(self.account),
        }
    }

    /// The canonical encoding.
    #[must_use]
    pub fn encode(&self) -> Vec<u8> {
        let mut o = Out::with_capacity(KIND_PLACE, 64);
        o.varint(self.at);
        o.account(self.account);
        o.text(self.place.as_str());
        o.varint(self.since);
        o.varint(self.until);
        o.0
    }

    /// Decode a canonical encoding.
    ///
    /// # Errors
    /// [`RowError`] for anything [`PlaceRow::encode`] does not write.
    pub fn decode(bytes: &[u8]) -> Result<PlaceRow, RowError> {
        let mut r = In::head(bytes, KIND_PLACE)?;
        let row = PlaceRow {
            at: r.varint()?,
            account: r.account()?,
            place: r.word("the place")?,
            since: r.varint()?,
            until: r.varint()?,
        };
        r.finish()?;
        Ok(row)
    }

    /// Whether the placement holds at `now`.
    #[must_use]
    pub const fn live(&self, now: u64) -> bool {
        self.since <= now && now < self.until
    }
}

// ---------------------------------------------------------------------------
// Needs
// ---------------------------------------------------------------------------

/// A session that needs a person (the Fleet menu's need rows, `aterm-gui` `fleet_glance`).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct NeedEntry {
    /// The session.
    pub sid: Sid,
    /// The need word (`frozen`, `stopped`, `stalled`, `login`, `memory`, `wall`, `ask`,
    /// `approval`, `question`).
    pub word: Word,
    /// Whether the harness carries the session on by itself (`needs_by=harness`).
    pub harness: bool,
    /// The manager session it works for.
    pub worker_of: Option<Sid>,
    /// Since when, Unix seconds.
    pub since: Option<u64>,
}

/// A held session.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct HeldEntry {
    /// The session.
    pub sid: Sid,
    /// The hold word (`person`, `remote`, `local`, `bridge`).
    pub hold: Word,
    /// The Mac that holds it, when another Mac does.
    pub by: Option<Node>,
    /// Since when, Unix seconds.
    pub since: Option<u64>,
}

/// A session that manages workers.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ManagerEntry {
    /// The manager session.
    pub sid: Sid,
    /// How many workers it runs.
    pub workers: u32,
}

/// This Mac's sessions that need a person, its held sessions and its managers: what the Fleet
/// menu shows of it. Ids, words and times only; a session's title, role, ask text and hold
/// host never leave the Mac.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct NeedsRow {
    /// When it was written, Unix seconds.
    pub at: u64,
    /// Sessions that need a person.
    pub needs: Vec<NeedEntry>,
    /// Held sessions.
    pub held: Vec<HeldEntry>,
    /// Managers.
    pub managers: Vec<ManagerEntry>,
}

impl NeedsRow {
    /// Where `node` publishes it.
    #[must_use]
    pub const fn subject(node: Node) -> Subject {
        Subject::Now {
            node,
            now: Now::Needs,
        }
    }

    /// The canonical encoding: the first [`NEEDS_MAX`] entries of each list.
    #[must_use]
    pub fn encode(&self) -> Vec<u8> {
        let cap = 16
            + self.needs.len().min(NEEDS_MAX) * 56
            + self.held.len().min(NEEDS_MAX) * 56
            + self.managers.len().min(NEEDS_MAX) * 28;
        let mut o = Out::with_capacity(KIND_NEEDS, cap);
        o.varint(self.at);
        let k = o.count(&self.needs, NEEDS_MAX);
        for n in self.needs.iter().take(k) {
            o.sid(n.sid);
            o.text(n.word.as_str());
            o.flag(n.harness);
            o.opt_sid(n.worker_of);
            o.opt(n.since);
        }
        let k = o.count(&self.held, NEEDS_MAX);
        for h in self.held.iter().take(k) {
            o.sid(h.sid);
            o.text(h.hold.as_str());
            o.opt_node(h.by);
            o.opt(h.since);
        }
        let k = o.count(&self.managers, NEEDS_MAX);
        for m in self.managers.iter().take(k) {
            o.sid(m.sid);
            o.varint(u64::from(m.workers));
        }
        o.0
    }

    /// Decode a canonical encoding.
    ///
    /// # Errors
    /// [`RowError`] for anything [`NeedsRow::encode`] does not write.
    pub fn decode(bytes: &[u8]) -> Result<NeedsRow, RowError> {
        let mut r = In::head(bytes, KIND_NEEDS)?;
        let at = r.varint()?;
        let n = r.count(NEEDS_MAX, "needs")?;
        let mut needs = Vec::with_capacity(n);
        for _ in 0..n {
            needs.push(NeedEntry {
                sid: r.sid("a session id")?,
                word: r.word("a need word")?,
                harness: r.flag()?,
                worker_of: r.opt_sid("a manager's session id")?,
                since: r.opt()?,
            });
        }
        let n = r.count(NEEDS_MAX, "held sessions")?;
        let mut held = Vec::with_capacity(n);
        for _ in 0..n {
            let sid = r.sid("a session id")?;
            let hold = r.word("a hold word")?;
            let by = if r.flag()? {
                Some(Node::parse(r.text()?).ok_or(RowError::Text("the holding node"))?)
            } else {
                None
            };
            held.push(HeldEntry {
                sid,
                hold,
                by,
                since: r.opt()?,
            });
        }
        let n = r.count(NEEDS_MAX, "managers")?;
        let mut managers = Vec::with_capacity(n);
        for _ in 0..n {
            managers.push(ManagerEntry {
                sid: r.sid("a manager's session id")?,
                workers: r.u32("a manager's workers")?,
            });
        }
        r.finish()?;
        Ok(NeedsRow {
            at,
            needs,
            held,
            managers,
        })
    }
}

// ---------------------------------------------------------------------------
// Unread pools
// ---------------------------------------------------------------------------

/// The account pools this Mac could not read on its last pass (`survey`'s
/// `accounts_unread=`): a view says their rows are as old as they are, not that they are gone.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct UnreadRow {
    /// When it was written, Unix seconds.
    pub at: u64,
    /// The pools, by CLI word.
    pub pools: Vec<Word>,
}

impl UnreadRow {
    /// Where `node` publishes it.
    #[must_use]
    pub const fn subject(node: Node) -> Subject {
        Subject::Now {
            node,
            now: Now::Unread,
        }
    }

    /// The canonical encoding: its first [`UNREAD_MAX`] pools.
    #[must_use]
    pub fn encode(&self) -> Vec<u8> {
        let mut o = Out::with_capacity(KIND_UNREAD, 16 + self.pools.len().min(UNREAD_MAX) * 8);
        o.varint(self.at);
        let n = o.count(&self.pools, UNREAD_MAX);
        for p in self.pools.iter().take(n) {
            o.text(p.as_str());
        }
        o.0
    }

    /// Decode a canonical encoding.
    ///
    /// # Errors
    /// [`RowError`] for anything [`UnreadRow::encode`] does not write.
    pub fn decode(bytes: &[u8]) -> Result<UnreadRow, RowError> {
        let mut r = In::head(bytes, KIND_UNREAD)?;
        let at = r.varint()?;
        let n = r.count(UNREAD_MAX, "unread pools")?;
        let mut pools = Vec::with_capacity(n);
        for _ in 0..n {
            pools.push(r.word("a pool")?);
        }
        r.finish()?;
        Ok(UnreadRow { at, pools })
    }
}

// ---------------------------------------------------------------------------
// Window records
// ---------------------------------------------------------------------------

/// One closed window's summary (§4.14 "Counters"): its origin, metric, window (start and
/// width) and revision, then astream's mergeable summary of every sample the origin recorded
/// in it, by its own clock.
///
/// A sample that arrives after its window closed publishes the window again at `revision +
/// 1`, holding every sample so far; a reader keeps the highest revision for each (origin,
/// metric, window) and adds across origins ([`crate::merge::Counters`]), so a relayed
/// duplicate or a late copy changes nothing.
///
/// Built only by [`WindowRecord::new`] and the decoder, so every record names a metric this
/// build names, on a window of its width, and every reader takes its encoding.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct WindowRecord {
    pub(crate) node: Node,
    pub(crate) metric: Metric,
    pub(crate) window: Window,
    pub(crate) revision: u64,
    pub(crate) summary: Summary,
}

impl WindowRecord {
    /// A record of `metric`'s window starting at `start_ms`; `None` when this build does not
    /// name `metric` ([`Metric::ALL`]), or `start_ms` is not the start of a window of its
    /// width.
    #[must_use]
    pub fn new(
        node: Node,
        metric: Metric,
        start_ms: u64,
        revision: u64,
        summary: Summary,
    ) -> Option<WindowRecord> {
        if !metric.is_named() {
            return None;
        }
        let window = Window::starting(start_ms, metric.width().window_len())?;
        Some(WindowRecord {
            node,
            metric,
            window,
            revision,
            summary,
        })
    }

    /// The Mac whose samples it summarizes, and its only writer.
    #[must_use]
    pub const fn node(&self) -> Node {
        self.node
    }

    /// What it measures.
    #[must_use]
    pub const fn metric(&self) -> Metric {
        self.metric
    }

    /// The window, whose length is the metric's width.
    #[must_use]
    pub const fn window(&self) -> Window {
        self.window
    }

    /// 0 for the first version, one more for each republish.
    #[must_use]
    pub const fn revision(&self) -> u64 {
        self.revision
    }

    /// The samples.
    #[must_use]
    pub const fn summary(&self) -> &Summary {
        &self.summary
    }

    /// Where its node publishes it.
    #[must_use]
    pub const fn subject(&self) -> Subject {
        Subject::Window {
            node: self.node,
            metric: self.metric,
        }
    }

    /// astream's identity of the row: origin, metric, window.
    #[must_use]
    pub fn id(&self) -> SummaryId {
        SummaryId {
            origin: self.node.origin(),
            metric: self.metric.metric_name(),
            window: self.window,
        }
    }

    /// The canonical encoding.
    #[must_use]
    pub fn encode(&self) -> Vec<u8> {
        // A node's origin and a compiled metric name keep the identity far under 128 bytes
        // (`every_row_reads_back_as_itself_at_its_subject`), so its length prefix is 1 byte.
        let mut o = Out::with_capacity(KIND_WINDOW, 128);
        o.0.push(0);
        let id_start = o.0.len();
        o.0.push(astream_wire::summary::KIND_ID);
        o.0.push(astream_wire::summary::SUMMARY_ID_VERSION);
        o.bytes(&self.node.to_ascii_bytes());
        o.text(self.metric.as_str());
        o.0.push(match self.window.window_len() {
            astream_wire::summary::WindowLen::Minute => 1,
            astream_wire::summary::WindowLen::Hour => 2,
            astream_wire::summary::WindowLen::Day => 3,
            astream_wire::summary::WindowLen::Week => 4,
        });
        o.varint(self.window.index());
        o.0[id_start - 1] = u8::try_from(o.0.len() - id_start).unwrap_or(0);
        o.varint(self.revision);
        self.summary.encode_into(&mut o.0);
        o.0
    }

    /// Decode a canonical encoding.
    ///
    /// # Errors
    /// [`RowError`] for anything [`WindowRecord::encode`] does not write: an identity whose
    /// origin is not a node id, whose metric this build does not name, or whose window is not
    /// the metric's width; or a summary astream refuses.
    pub fn decode(bytes: &[u8]) -> Result<WindowRecord, RowError> {
        let mut r = In::head(bytes, KIND_WINDOW)?;
        let id_len = r.count(usize::from(u8::MAX), "identity bytes")?;
        let id = SummaryId::decode(r.take(id_len)?).map_err(RowError::Summary)?;
        let revision = r.varint()?;
        let summary = Summary::decode(r.0).map_err(RowError::Summary)?;
        let node = Node::parse(id.origin.as_str()).ok_or(RowError::Text("the origin"))?;
        let metric = Metric::parse(id.metric.as_str()).ok_or(RowError::Text("the metric"))?;
        if Width::of(id.window.window_len()) != Some(metric.width()) {
            return Err(RowError::Mismatch("window width"));
        }
        Ok(WindowRecord {
            node,
            metric,
            window: id.window,
            revision,
            summary,
        })
    }
}

// ---------------------------------------------------------------------------
// Any row, read at its subject
// ---------------------------------------------------------------------------

/// Any row this crate reads, as decoded at its subject.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Row {
    /// An account's current row.
    Quota(QuotaRow),
    /// An agy home's pools.
    Agy(AgyRow),
    /// An account's place.
    Place(PlaceRow),
    /// The Mac's needs.
    Needs(NeedsRow),
    /// The pools the Mac could not read.
    Unread(UnreadRow),
    /// A window's summary.
    Window(WindowRecord),
}

impl Row {
    /// The canonical encoding.
    #[must_use]
    pub fn encode(&self) -> Vec<u8> {
        match self {
            Row::Quota(r) => r.encode(),
            Row::Agy(r) => r.encode(),
            Row::Place(r) => r.encode(),
            Row::Needs(r) => r.encode(),
            Row::Unread(r) => r.encode(),
            Row::Window(r) => r.encode(),
        }
    }

    /// Decode `body` as the row `subject` holds, and refuse one that names another Mac, CLI,
    /// account, home or metric than its subject: the subject is what the relay checked its
    /// origin's signature on, so the body may not speak for anything else.
    ///
    /// # Errors
    /// [`RowError::NoCodec`] for presence, raw samples and the journal; otherwise what the
    /// row's own decoder refuses, or [`RowError::Mismatch`].
    pub fn decode_at(subject: &Subject, body: &[u8]) -> Result<Row, RowError> {
        match *subject {
            Subject::Quota { cli, account, .. } => {
                let row = QuotaRow::decode(body)?;
                if row.cli != cli {
                    return Err(RowError::Mismatch("CLI"));
                }
                if row.account != account {
                    return Err(RowError::Mismatch("account"));
                }
                Ok(Row::Quota(row))
            }
            Subject::Agy { home, .. } => {
                let row = AgyRow::decode(body)?;
                if row.home != home {
                    return Err(RowError::Mismatch("home"));
                }
                Ok(Row::Agy(row))
            }
            Subject::Now { now, .. } => match now {
                Now::Needs => NeedsRow::decode(body).map(Row::Needs),
                Now::Unread => UnreadRow::decode(body).map(Row::Unread),
                Now::Place(account) => {
                    let row = PlaceRow::decode(body)?;
                    if row.account != account {
                        return Err(RowError::Mismatch("account"));
                    }
                    Ok(Row::Place(row))
                }
                Now::Presence => Err(RowError::NoCodec),
            },
            Subject::Window { node, metric } => {
                let row = WindowRecord::decode(body)?;
                if row.node != node {
                    return Err(RowError::Mismatch("origin"));
                }
                if row.metric != metric {
                    return Err(RowError::Mismatch("metric"));
                }
                Ok(Row::Window(row))
            }
            Subject::Raw { .. } | Subject::Journal { .. } => Err(RowError::NoCodec),
        }
    }

    /// Where `node` publishes it; `None` for a quota row whose CLI is agy.
    #[must_use]
    pub fn subject(&self, node: Node) -> Option<Subject> {
        match self {
            Row::Quota(r) => r.subject(node),
            Row::Agy(r) => Some(r.subject(node)),
            Row::Place(r) => Some(r.subject(node)),
            Row::Needs(_) => Some(NeedsRow::subject(node)),
            Row::Unread(_) => Some(UnreadRow::subject(node)),
            Row::Window(r) => (r.node == node).then(|| r.subject()),
        }
    }
}
