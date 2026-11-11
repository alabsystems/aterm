// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! The durable tagged log.

use std::collections::{BTreeMap, BTreeSet, VecDeque};
use std::fmt;

use crate::center::Outcome;
use crate::model::{Glyph, Intent, Message, MessageId, Origin, Severity, Tag, WallStamp};
use crate::text::clip;
use crate::{
    DETAIL_LINE_CAP, DETAIL_LINES_CAP, Instant, KEY_CAP, LOG_CAP, MAX_ACTIONS, PENDING_PERSIST_CAP,
    TITLE_CAP, WIRE_LOG_SHARE,
};

/// Maximum byte length the decoder accepts for a single encoded log line.
pub const MAX_LINE_BYTES: usize = 64 * 1024;

const ID_CEILING: u64 = 1 << 62;

const US: char = '\u{1f}';

/// How a record left the glass.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Retired {
    /// Its hold elapsed on glass.
    Folded,
    /// A live row went silent past its cap.
    Stale,
    /// A queued row never reached the glass in time.
    Unseen,
    /// A post with the same key replaced it.
    Superseded {
        /// The replacing message.
        by: MessageId,
    },
    /// The reporter resolved it.
    Resolved(Outcome),
    /// A person dismissed it.
    Dismissed,
    /// A person answered a decision row with this capsule.
    Answered {
        /// The capsule's full label.
        label: String,
    },
    /// The live set was full and this row ranked lowest.
    Evicted,
    /// It went to the successor process in the handoff carry.
    Carried,
    /// The reporter withdrew it with no outcome to claim.
    Withdrawn,
    /// A record: posted [`crate::model::Hold::LogOnly`], never on the glass.
    Recorded,
    /// aterm quit while the row was still open.
    Quit,
}

impl Retired {
    /// The wire's `state=` word: `folded` … `resolved-ok` / `resolved-warn`.
    #[must_use]
    pub const fn as_word(&self) -> &'static str {
        match self {
            Self::Folded => "folded",
            Self::Stale => "stale",
            Self::Unseen => "unseen",
            Self::Superseded { .. } => "superseded",
            Self::Resolved(Outcome::Ok) => "resolved-ok",
            Self::Resolved(Outcome::Warn) => "resolved-warn",
            Self::Dismissed => "dismissed",
            Self::Answered { .. } => "answered",
            Self::Evicted => "evicted",
            Self::Carried => "carried",
            Self::Withdrawn => "withdrawn",
            Self::Recorded => "recorded",
            Self::Quit => "quit",
        }
    }

    /// The codec form: the word, plus `:<id>` / `:<label>` payloads.
    #[must_use]
    pub fn encode(&self) -> String {
        match self {
            Self::Superseded { by } => format!("superseded:{by}"),
            Self::Answered { label } => format!("answered:{label}"),
            Self::Resolved(Outcome::Ok) => "resolved:ok".to_string(),
            Self::Resolved(Outcome::Warn) => "resolved:warn".to_string(),
            other => other.as_word().to_string(),
        }
    }

    /// The inverse of [`Retired::encode`]; unknown ⇒ `None`.
    #[must_use]
    pub fn decode(s: &str) -> Option<Self> {
        match s.split_once(':') {
            None => match s {
                "folded" => Some(Self::Folded),
                "stale" => Some(Self::Stale),
                "unseen" => Some(Self::Unseen),
                "dismissed" => Some(Self::Dismissed),
                "evicted" => Some(Self::Evicted),
                "carried" => Some(Self::Carried),
                "withdrawn" => Some(Self::Withdrawn),
                "recorded" => Some(Self::Recorded),
                "quit" => Some(Self::Quit),
                _ => None,
            },
            Some((kind, payload)) => match (kind, payload) {
                ("superseded", id) => {
                    if id.is_empty() || !id.bytes().all(|b| b.is_ascii_digit()) {
                        return None;
                    }
                    Some(Self::Superseded {
                        by: MessageId::from_raw(id.parse().ok()?)?,
                    })
                }
                ("resolved", "ok") => Some(Self::Resolved(Outcome::Ok)),
                ("resolved", "warn") => Some(Self::Resolved(Outcome::Warn)),
                ("answered", label) => Some(Self::Answered {
                    label: label.to_string(),
                }),
                _ => None,
            },
        }
    }
}

/// Whether a record is still live.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum LogState {
    /// Unretired.
    Posted,
    /// Retired, and how.
    Retired(Retired),
}

/// One message as the log keeps it.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct LogRecord {
    /// The id.
    pub id: MessageId,
    /// The wall clock at ingress.
    pub stamp: WallStamp,
    /// The reporter family.
    pub tag: Tag,
    /// The severity: the live row's while it lives, the one it retired under once retired.
    pub severity: Severity,
    /// The severity it was posted at, its `posted` line's.
    pub posted_severity: Severity,
    /// The glyph, as the severity.
    pub glyph: Glyph,
    /// The title (final words once retired).
    pub title: String,
    /// The detail lines (final words once retired).
    pub detail: Vec<String>,
    /// The authored intents, so the page can re-offer them.
    pub actions: Vec<Intent>,
    /// The supersede key.
    pub key: Option<String>,
    /// Where it came from.
    pub origin: Origin,
    /// Duplicate posts folded into this record (1 = posted once).
    pub repeats: u32,
    /// Live or retired.
    pub state: LogState,
    /// The wall clock when it retired.
    pub retired_unix_ms: Option<u64>,
    /// When it retired, in this process only.
    pub retired_at: Option<Instant>,
    /// The last capsule pressed, and when.
    pub last_action: Option<(String, u64)>,
    /// An entry of the digest ([`crate::need::Need::Later`]).
    pub later: bool,
}

impl LogRecord {
    /// The record a post makes.
    #[must_use]
    pub fn from_posted(id: MessageId, stamp: WallStamp, msg: &Message) -> Self {
        Self {
            id,
            stamp,
            tag: msg.tag.clone(),
            severity: msg.severity,
            posted_severity: msg.severity,
            glyph: msg.glyph,
            title: msg.title.clone(),
            detail: msg.detail.clone(),
            actions: msg.actions.clone(),
            key: msg.key.clone(),
            origin: msg.origin,
            repeats: 1,
            state: LogState::Posted,
            retired_unix_ms: None,
            retired_at: None,
            last_action: None,
            later: matches!(msg.need, crate::need::Need::Later(_)),
        }
    }

    /// `true` while unretired.
    #[must_use]
    pub fn is_live(&self) -> bool {
        self.state == LogState::Posted
    }

    /// `true` for a digest entry the owner or the digest itself filed.
    #[must_use]
    pub(crate) const fn filed_exception(&self) -> bool {
        self.later
            && matches!(
                self.retired(),
                Some(
                    Retired::Dismissed
                        | Retired::Answered { .. }
                        | Retired::Unseen
                        | Retired::Evicted
                )
            )
    }

    /// The ring keeps a filed exception for [`crate::need::FILED_KEEP`] after its filing
    /// (not its posting): the fact's quiet span counts from the dismissal, so
    /// [`LOG_CAP`] later records must not push the dismissal out while it holds.
    #[must_use]
    fn kept_as_filing(&self, since_ms: u64) -> bool {
        self.filed_exception() && self.retired_unix_ms.is_some_and(|t| t >= since_ms)
    }

    /// The wire's record ([`Origin::wire_owned`]): it counts against the ring's [`WIRE_LOG_SHARE`].
    #[must_use]
    pub fn wire_owned(&self) -> bool {
        self.origin.wire_owned(self.key.as_deref())
    }

    /// How it retired, if it has.
    #[must_use]
    pub const fn retired(&self) -> Option<&Retired> {
        match &self.state {
            LogState::Posted => None,
            LogState::Retired(how) => Some(how),
        }
    }

    /// Whether this retired record still offers `intent`.
    #[must_use]
    pub fn still_offers(&self, intent: &Intent) -> bool {
        !intent.ends_with_row()
            && !matches!(self.retired(), Some(Retired::Answered { label }) if label == intent.label())
    }
}

/// One line of the file.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum LogLine {
    /// A message was posted (its words at post).
    Posted(LogRecord),
    /// A message retired, with its final words.
    Retired {
        /// The id.
        id: MessageId,
        /// How.
        how: Retired,
        /// The wall clock.
        unix_ms: u64,
        /// The final title.
        title: String,
        /// The final detail lines.
        detail: Vec<String>,
        /// Duplicate posts folded in.
        repeats: u32,
        /// The outcome's mark when the retirement changed it.
        mark: Option<(Severity, Glyph)>,
    },
    /// A capsule was pressed.
    Acted {
        /// The id.
        id: MessageId,
        /// The wall clock.
        unix_ms: u64,
        /// The capsule's full label.
        label: String,
    },
    /// This many pending lines were dropped before this one — the file is honest about a gap.
    Dropped {
        /// How many.
        count: u32,
    },
}

/// Which file a line lands in.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Shelf {
    /// aterm's own record: `messages.log`.
    Host,
    /// The wire's: `messages.wire.log`.
    Wire,
}

/// Why a line did not decode; the loader skips it.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum CodecError {
    /// The line does not start with `m1`.
    NotM1,
    /// Longer than [`MAX_LINE_BYTES`].
    TooLong,
    /// A kind this build does not know.
    UnknownKind,
    /// A required field is absent.
    MissingField(&'static str),
    /// A field did not parse.
    BadField(&'static str),
    /// No `end=` terminator: the line was cut short mid-append.
    Truncated,
}

impl fmt::Display for CodecError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::NotM1 => f.write_str("not an m1 line"),
            Self::TooLong => f.write_str("line over the size cap"),
            Self::UnknownKind => f.write_str("unknown record kind"),
            Self::MissingField(k) => write!(f, "missing field {k}"),
            Self::BadField(k) => write!(f, "bad field {k}"),
            Self::Truncated => f.write_str("line cut short (no terminator)"),
        }
    }
}

impl std::error::Error for CodecError {}

impl LogLine {
    /// The id this line is about, if any.
    #[must_use]
    pub const fn id(&self) -> Option<MessageId> {
        match self {
            Self::Posted(rec) => Some(rec.id),
            Self::Retired { id, .. } | Self::Acted { id, .. } => Some(*id),
            Self::Dropped { .. } => None,
        }
    }

    /// One line, no trailing newline.
    #[must_use]
    pub fn encode(&self) -> String {
        let mut out = String::from("m1");
        let mut field = |k: &str, v: &str| {
            out.push('\t');
            out.push_str(k);
            out.push('=');
            escape_into(&mut out, v);
        };
        match self {
            Self::Posted(rec) => {
                field("kind", "posted");
                field("id", &rec.id.to_string());
                field("t", &rec.stamp.unix_ms.to_string());
                field("tag", rec.tag.as_str());
                field("sev", rec.severity.as_str());
                field("glyph", rec.glyph.ch().encode_utf8(&mut [0; 4]));
                field("title", &rec.title);
                field("detail", &join_us(&rec.detail));
                let actions: Vec<String> = rec.actions.iter().map(Intent::encode).collect();
                field("actions", &join_us(&actions));
                if let Some(key) = &rec.key {
                    field("key", key);
                }
                field("origin", rec.origin.as_str());
                if rec.later {
                    field("need", "later");
                }
            }
            Self::Retired {
                id,
                how,
                unix_ms,
                title,
                detail,
                repeats,
                mark,
            } => {
                field("kind", "retired");
                field("id", &id.to_string());
                if *how == Retired::Recorded {
                    field("how", Retired::Folded.as_word());
                    field("rec", "1");
                } else {
                    field("how", &how.encode());
                }
                field("t", &unix_ms.to_string());
                field("title", title);
                field("detail", &join_us(detail));
                field("rep", &repeats.to_string());
                if let Some((sev, glyph)) = mark {
                    field("sev", sev.as_str());
                    field("glyph", glyph.ch().encode_utf8(&mut [0; 4]));
                }
            }
            Self::Acted { id, unix_ms, label } => {
                field("kind", "acted");
                field("id", &id.to_string());
                field("t", &unix_ms.to_string());
                field("label", label);
            }
            Self::Dropped { count } => {
                field("kind", "dropped");
                field("n", &count.to_string());
            }
        }
        field("end", "");
        out
    }

    /// One line back into a record; forward-compatible (unknown `key=value` fields are ignored).
    /// # Errors
    /// [`CodecError`] for a line that is not `m1`, is over the size cap, names an unknown kind,
    /// lacks a required field, carries a bare token with no `=`, or has no `end=` terminator of its
    /// own (it was cut short, perhaps with the next line glued on).
    pub fn decode(line: &str) -> Result<Self, CodecError> {
        let line = line.strip_suffix('\n').unwrap_or(line);
        let line = line.strip_suffix('\r').unwrap_or(line);
        if line.len() > MAX_LINE_BYTES {
            return Err(CodecError::TooLong);
        }
        let mut tokens = line.split('\t');
        if tokens.next() != Some("m1") {
            return Err(CodecError::NotM1);
        }
        let fields = Fields::parse(tokens);
        if fields.pairs.iter().filter(|(k, _)| k == "kind").count() > 1 {
            return Err(CodecError::Truncated);
        }
        let decoded = match fields.get("kind") {
            Some("posted") => decode_posted(&fields)?,
            Some("retired") => Self::Retired {
                id: fields.id()?,
                how: fields.how()?,
                unix_ms: fields.num("t")?,
                title: clip(&fields.need("title")?, TITLE_CAP),
                detail: capped_detail(&fields.get_or("detail")),
                repeats: fields.num("rep")?,
                mark: decode_mark(&fields),
            },
            Some("acted") => Self::Acted {
                id: fields.id()?,
                unix_ms: fields.num("t")?,
                label: clip(&fields.need("label")?, TITLE_CAP),
            },
            Some("dropped") => Self::Dropped {
                count: fields.num("n")?,
            },
            _ => return Err(CodecError::UnknownKind),
        };
        if fields.bare || fields.get("end").is_none() {
            return Err(CodecError::Truncated);
        }
        Ok(decoded)
    }
}

fn decode_mark(fields: &Fields) -> Option<(Severity, Glyph)> {
    let severity = Severity::parse(fields.get("sev")?)?;
    let glyph = Glyph::or_fallback(fields.get("glyph")?.chars().next()?);
    Some((severity, glyph))
}

fn decode_posted(fields: &Fields) -> Result<LogLine, CodecError> {
    let tag = Tag::try_new(&fields.need("tag")?).map_err(|_| CodecError::BadField("tag"))?;
    let severity = Severity::parse(&fields.need("sev")?).ok_or(CodecError::BadField("sev"))?;
    let glyph = fields
        .get("glyph")
        .and_then(|g| g.chars().next())
        .map_or(Glyph::FALLBACK, Glyph::or_fallback);
    let actions = split_us(&fields.get_or("actions"))
        .iter()
        .filter_map(|a| Intent::decode(a))
        .filter(|i| *i != Intent::Details)
        .take(MAX_ACTIONS)
        .collect();
    let origin = Origin::parse(&fields.need("origin")?).ok_or(CodecError::BadField("origin"))?;
    Ok(LogLine::Posted(LogRecord {
        id: fields.id()?,
        stamp: WallStamp {
            unix_ms: fields.num("t")?,
        },
        tag,
        severity,
        posted_severity: severity,
        glyph,
        title: clip(&fields.need("title")?, TITLE_CAP),
        detail: capped_detail(&fields.get_or("detail")),
        actions,
        key: fields
            .get("key")
            .map(|k| clip(k, KEY_CAP))
            .filter(|k| !k.is_empty()),
        origin,
        repeats: 1,
        state: LogState::Posted,
        retired_unix_ms: None,
        retired_at: None,
        last_action: None,
        later: fields.get("need") == Some("later"),
    }))
}

struct Fields {
    pairs: Vec<(String, String)>,
    bare: bool,
}

impl Fields {
    fn parse<'a>(tokens: impl Iterator<Item = &'a str>) -> Self {
        let mut pairs = Vec::new();
        let mut bare = false;
        for token in tokens {
            if let Some((k, v)) = token.split_once('=') {
                pairs.push((k.to_string(), unescape(v)));
            } else {
                bare = true;
            }
        }
        Self { pairs, bare }
    }

    fn get(&self, key: &str) -> Option<&str> {
        self.pairs
            .iter()
            .rfind(|(k, _)| k == key)
            .map(|(_, v)| v.as_str())
    }

    fn get_or(&self, key: &str) -> String {
        self.get(key).unwrap_or_default().to_string()
    }

    fn need(&self, key: &'static str) -> Result<String, CodecError> {
        self.get(key)
            .map(str::to_string)
            .ok_or(CodecError::MissingField(key))
    }

    fn num<T: std::str::FromStr>(&self, key: &'static str) -> Result<T, CodecError> {
        let s = self.need(key)?;
        if s.is_empty() || !s.bytes().all(|b| b.is_ascii_digit()) {
            return Err(CodecError::BadField(key));
        }
        s.parse().map_err(|_| CodecError::BadField(key))
    }

    fn id(&self) -> Result<MessageId, CodecError> {
        MessageId::from_raw(self.num("id")?).ok_or(CodecError::BadField("id"))
    }

    fn how(&self) -> Result<Retired, CodecError> {
        let how = Retired::decode(&self.need("how")?).ok_or(CodecError::BadField("how"))?;
        Ok(match how {
            Retired::Answered { label } => Retired::Answered {
                label: clip(&label, TITLE_CAP),
            },
            Retired::Folded if self.get("rec") == Some("1") => Retired::Recorded,
            other => other,
        })
    }
}

fn capped_detail(joined: &str) -> Vec<String> {
    split_us(joined)
        .iter()
        .map(|l| clip(l, DETAIL_LINE_CAP))
        .filter(|l| !l.is_empty())
        .take(DETAIL_LINES_CAP)
        .collect()
}

/// Lines joined by US.
pub(crate) fn join_us(lines: &[String]) -> String {
    let mut out = String::new();
    for (i, line) in lines.iter().enumerate() {
        if i > 0 {
            out.push(US);
        }
        out.extend(line.chars().filter(|c| *c != US));
    }
    out
}

/// The inverse of [`join_us`]: a value's lines, split at US (none for an empty value).
pub(crate) fn split_us(s: &str) -> Vec<String> {
    if s.is_empty() {
        return Vec::new();
    }
    s.split(US).map(str::to_string).collect()
}

/// `\`→`\\`, TAB→`\t`, LF→`\n`, CR→`\r`, US→`\u`.
pub(crate) fn escape_into(out: &mut String, v: &str) {
    for c in v.chars() {
        match c {
            '\\' => out.push_str("\\\\"),
            '\t' => out.push_str("\\t"),
            '\n' => out.push_str("\\n"),
            '\r' => out.push_str("\\r"),
            US => out.push_str("\\u"),
            other => out.push(other),
        }
    }
}

/// The inverse of [`escape_into`]; an unknown escape keeps both characters.
pub(crate) fn unescape(v: &str) -> String {
    let mut out = String::with_capacity(v.len());
    let mut chars = v.chars();
    while let Some(c) = chars.next() {
        if c != '\\' {
            out.push(c);
            continue;
        }
        match chars.next() {
            Some('\\') | None => out.push('\\'),
            Some('t') => out.push('\t'),
            Some('n') => out.push('\n'),
            Some('r') => out.push('\r'),
            Some('u') => out.push(US),
            Some(other) => {
                out.push('\\');
                out.push(other);
            }
        }
    }
    out
}

/// A retiring row's final words, as the Retired line carries them.
#[derive(Clone, Copy, Debug)]
pub(crate) struct FinalWords<'a> {
    /// The final title.
    pub(crate) title: &'a str,
    /// The final detail lines.
    pub(crate) detail: &'a [String],
    /// Duplicate posts folded in.
    pub(crate) repeats: u32,
    /// The outcome's mark, when the retirement changed it.
    pub(crate) mark: Option<(Severity, Glyph)>,
}

/// The bounded ring of records plus the lines waiting for the host to append.
#[derive(Clone, Debug, Default)]
pub struct MessageLog {
    ring: VecDeque<LogRecord>,
    /// How many of [`Self::ring`]'s records are the wire's
    /// ([`LogRecord::wire_owned`], fixed for a record's life: its origin and
    /// key never change in the ring), kept beside it so an eviction does not
    /// count the whole ring.
    wire_records: usize,
    next_id: u64,
    pending: VecDeque<(LogLine, Shelf)>,
    dropped: u32,
    /// Replay only: a file id another writer posted again, mapped to the fresh ids its
    /// later records were loaded under, oldest first, so no two records in the ring
    /// share an id.
    remapped: BTreeMap<u64, Vec<MessageId>>,
    /// The fresh ids [`Self::remapped`] handed out: a record under one of
    /// them is not the file's record of that number.
    minted_at_replay: BTreeSet<u64>,
}

impl MessageLog {
    /// A log with no records; the first id minted is [`MessageId::FIRST`].
    #[must_use]
    pub fn empty() -> Self {
        Self::default()
    }

    /// Load one line from the file.
    pub fn replay(&mut self, line: LogLine) {
        if let Some(id) = line.id() {
            self.raise(id);
        }
        match line {
            LogLine::Posted(mut rec) => {
                let file_id = rec.id;
                let held = self.get(file_id).is_some();
                let candidates = self.candidates_given(file_id, held);
                if candidates.iter().any(|id| {
                    self.get(*id).is_some_and(|r| {
                        !Self::closed_by_a_line(r)
                            && r.stamp == rec.stamp
                            && r.tag == rec.tag
                            && r.title == rec.title
                    })
                }) {
                    return;
                }
                if held || !candidates.is_empty() {
                    let fresh = self.next_id();
                    self.raise(fresh);
                    self.minted_at_replay.insert(fresh.raw());
                    self.remapped.entry(file_id.raw()).or_default().push(fresh);
                    rec.id = fresh;
                }
                if !rec.later {
                    rec.state = LogState::Retired(Retired::Stale);
                }
                rec.retired_at = None;
                self.push_record(rec);
            }
            LogLine::Retired {
                id,
                how,
                unix_ms,
                title,
                detail,
                repeats,
                mark,
            } => {
                let target = self.replay_target(id, Some(&title));
                if let Some(rec) = target.and_then(|id| self.get_mut(id)) {
                    rec.state = LogState::Retired(how);
                    rec.retired_unix_ms = Some(unix_ms);
                    rec.retired_at = None;
                    rec.title = title;
                    rec.detail = detail;
                    rec.repeats = repeats;
                    if let Some((severity, glyph)) = mark {
                        rec.severity = severity;
                        rec.glyph = glyph;
                    }
                }
            }
            LogLine::Acted { id, unix_ms, label } => {
                let target = self.replay_target(id, None);
                if let Some(rec) = target.and_then(|id| self.get_mut(id)) {
                    rec.last_action = Some((label, unix_ms));
                }
            }
            LogLine::Dropped { .. } => {}
        }
    }

    /// Replay a file's lines in order after advancing the id counter past all ids in the batch.
    pub fn replay_all(&mut self, lines: impl IntoIterator<Item = LogLine>) {
        let lines: Vec<LogLine> = lines.into_iter().collect();
        if let Some(top) = lines
            .iter()
            .filter_map(LogLine::id)
            .filter(|id| id.next().raw() < ID_CEILING)
            .max()
        {
            self.raise(top);
        }
        for line in lines {
            self.replay(line);
        }
    }

    /// The number the file gave the record under `id`.
    #[must_use]
    pub fn file_id_of(&self, id: MessageId) -> Option<u64> {
        if !self.minted_at_replay.contains(&id.raw()) {
            return Some(id.raw());
        }
        self.remapped
            .iter()
            .find(|(_, fresh)| fresh.contains(&id))
            .map(|(file, _)| *file)
    }

    fn replay_candidates(&self, file_id: MessageId) -> Vec<MessageId> {
        self.candidates_given(file_id, self.get(file_id).is_some())
    }

    fn candidates_given(&self, file_id: MessageId, held: bool) -> Vec<MessageId> {
        let own = (held && !self.minted_at_replay.contains(&file_id.raw())).then_some(file_id);
        own.into_iter()
            .chain(
                self.remapped
                    .get(&file_id.raw())
                    .into_iter()
                    .flatten()
                    .copied(),
            )
            .collect()
    }

    fn replay_target(&self, file_id: MessageId, title: Option<&str>) -> Option<MessageId> {
        let candidates = self.replay_candidates(file_id);
        let open = |id: &MessageId| self.get(*id).is_some_and(|r| !Self::closed_by_a_line(r));
        title
            .and_then(|title| {
                candidates.iter().rev().find(|id| {
                    open(id)
                        && self
                            .get(**id)
                            .is_some_and(|r| Self::final_words_of(&r.title, title))
                })
            })
            .or_else(|| candidates.iter().rev().find(|id| open(id)))
            .or_else(|| candidates.last())
            .copied()
    }

    fn final_words_of(posted: &str, final_title: &str) -> bool {
        posted == final_title
            || [
                crate::words::finished_form(posted),
                crate::words::done_form(posted),
                crate::words::stopped_form(posted),
                crate::words::ended_form(posted),
            ]
            .iter()
            .flatten()
            .any(|w| w == final_title)
    }

    fn closed_by_a_line(rec: &LogRecord) -> bool {
        matches!(&rec.state, LogState::Retired(how) if *how != Retired::Stale)
    }

    /// The records, oldest first.
    #[must_use = "consume the iterator to read the log records"]
    pub fn records(&self) -> impl DoubleEndedIterator<Item = &LogRecord> {
        self.ring.iter()
    }

    /// One record by id.
    #[must_use]
    pub fn get(&self, id: MessageId) -> Option<&LogRecord> {
        self.ring.iter().rev().find(|r| r.id == id)
    }

    fn get_mut(&mut self, id: MessageId) -> Option<&mut LogRecord> {
        self.ring.iter_mut().rev().find(|r| r.id == id)
    }

    /// The id the next post takes.
    #[must_use]
    pub fn next_id(&self) -> MessageId {
        MessageId::from_raw(self.next_id).unwrap_or(MessageId::FIRST)
    }

    /// Records in the ring.
    #[must_use]
    pub fn len(&self) -> usize {
        self.ring.len()
    }

    /// `true` with no records.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.ring.is_empty()
    }

    /// Lines waiting for the host.
    #[must_use]
    pub fn pending_len(&self) -> usize {
        self.pending.len()
    }

    /// The lines waiting for the host, oldest first, left in place.
    #[cfg(test)]
    pub(crate) fn pending_lines(&self) -> impl Iterator<Item = &LogLine> {
        self.pending.iter().map(|(line, _)| line)
    }

    /// Take every pending line, oldest first.
    #[cfg(test)]
    pub(crate) fn drain_pending(&mut self) -> Vec<LogLine> {
        self.drain_shelved()
            .into_iter()
            .map(|(line, _)| line)
            .collect()
    }

    /// Take every pending line, oldest first, each with the file it lands in.
    pub(crate) fn drain_shelved(&mut self) -> Vec<(LogLine, Shelf)> {
        let mut out: Vec<(LogLine, Shelf)> = self.pending.drain(..).collect();
        if self.dropped > 0 {
            out.push((
                LogLine::Dropped {
                    count: self.dropped,
                },
                Shelf::Host,
            ));
            self.dropped = 0;
        }
        out
    }

    /// The latest word on a fact.
    #[must_use]
    pub(crate) fn last_word(&self, key: &str, exceptions: bool) -> Option<&LogRecord> {
        self.ring
            .iter()
            .filter(|r| r.later == exceptions && r.key.as_deref() == Some(key) && !r.is_live())
            .max_by_key(|r| (r.retired_unix_ms.unwrap_or(0), r.id))
    }

    /// The filings of `key`'s exception at or after `since_ms`.
    #[must_use]
    pub(crate) fn filings_since(&self, key: &str, since_ms: u64) -> u32 {
        let n = self
            .ring
            .iter()
            .filter(|r| {
                r.key.as_deref() == Some(key)
                    && matches!(
                        r.retired(),
                        Some(Retired::Dismissed | Retired::Answered { .. })
                    )
                    && r.later
                    && r.retired_unix_ms.is_some_and(|t| t >= since_ms)
            })
            .count();
        u32::try_from(n).unwrap_or(u32::MAX)
    }

    /// Mint the next id.
    pub(crate) fn mint(&mut self) -> MessageId {
        let id = self.next_id();
        self.next_id = id.next().raw();
        id
    }

    fn raise(&mut self, id: MessageId) {
        self.raise_to(id.next().raw());
    }

    /// Never mint below `raw` again (the carry ships the parent's next id).
    pub(crate) fn raise_to(&mut self, raw: u64) {
        if raw < ID_CEILING {
            self.next_id = self.next_id.max(raw);
        }
    }

    fn push_record(&mut self, rec: LogRecord) {
        if self.ring.len() >= LOG_CAP {
            self.evict_one(rec.stamp.unix_ms);
        }
        self.wire_records += usize::from(rec.wire_owned());
        self.ring.push_back(rec);
    }

    fn evict_one(&mut self, at_ms: u64) {
        let wire_full = self.wire_records >= WIRE_LOG_SHARE;
        let keep_since = at_ms.saturating_sub(crate::need::FILED_KEEP_MS);
        let wire_victim = || {
            let retired_wire = |r: &LogRecord| r.wire_owned() && !r.is_live();
            self.ring
                .iter()
                .position(|r| retired_wire(r) && !r.kept_as_filing(keep_since))
                .or_else(|| self.ring.iter().position(retired_wire))
        };
        let victim = wire_full
            .then(wire_victim)
            .flatten()
            .or_else(|| {
                self.ring
                    .iter()
                    .position(|r| !r.is_live() && !r.kept_as_filing(keep_since))
            })
            .or_else(|| self.ring.iter().position(|r| !r.is_live()));
        let gone = match victim {
            Some(i) => self.ring.remove(i),
            None => self.ring.pop_front(),
        };
        if gone.is_some_and(|r| r.wire_owned()) {
            self.wire_records -= 1;
        }
    }

    /// The file read again, merged into the ring.
    pub(crate) fn merge_reloaded(&mut self, disk: &MessageLog) {
        self.raise_to(disk.next_id);
        for rec in &disk.ring {
            let file_id = disk.file_id_of(rec.id);
            let claimed = |r: &LogRecord| {
                disk.get(r.id)
                    .is_some_and(|d| d.id != rec.id && d.stamp == r.stamp && d.tag == r.tag)
            };
            let same = self
                .ring
                .iter()
                .position(|r| r.stamp == rec.stamp && r.tag == rec.tag && r.id == rec.id)
                .or_else(|| {
                    self.ring.iter().position(|r| {
                        r.stamp == rec.stamp
                            && r.tag == rec.tag
                            && r.title == rec.title
                            && !claimed(r)
                    })
                })
                .or_else(|| {
                    let file_id = file_id?;
                    self.ring.iter().position(|r| {
                        r.stamp == rec.stamp
                            && r.tag == rec.tag
                            && self.file_id_of(r.id) == Some(file_id)
                            && !claimed(r)
                    })
                });
            if let Some(i) = same {
                let mine = &mut self.ring[i];
                mine.posted_severity = rec.posted_severity;
                if mine.is_live() {
                    continue;
                }
                if mine.retired() == Some(&Retired::Stale)
                    && rec.retired().is_some_and(|how| *how != Retired::Stale)
                {
                    mine.state = rec.state.clone();
                    mine.retired_unix_ms = rec.retired_unix_ms;
                    mine.title.clone_from(&rec.title);
                    mine.detail.clone_from(&rec.detail);
                    mine.repeats = rec.repeats;
                    mine.severity = rec.severity;
                    mine.glyph = rec.glyph;
                }
                if mine.last_action.is_none() {
                    mine.last_action.clone_from(&rec.last_action);
                }
                continue;
            }
            if self.ring.len() >= LOG_CAP && self.ring.front().is_some_and(|r| rec.id < r.id) {
                continue;
            }
            let mut rec = rec.clone();
            if self.get(rec.id).is_some() {
                let fresh = self.mint();
                self.minted_at_replay.insert(fresh.raw());
                rec.id = fresh;
            }
            self.raise(rec.id);
            if self.ring.len() >= LOG_CAP {
                self.evict_one(rec.stamp.unix_ms);
            }
            let at = self
                .ring
                .iter()
                .position(|r| r.id > rec.id)
                .unwrap_or(self.ring.len());
            self.wire_records += usize::from(rec.wire_owned());
            self.ring.insert(at, rec);
        }
    }

    pub(crate) fn push_pending(&mut self, line: LogLine) {
        let wire = match &line {
            LogLine::Posted(rec) => rec.wire_owned(),
            LogLine::Retired { id, .. } | LogLine::Acted { id, .. } => {
                self.get(*id).is_some_and(LogRecord::wire_owned)
            }
            LogLine::Dropped { .. } => false,
        };
        if self.pending.len() == PENDING_PERSIST_CAP {
            self.pending.pop_front();
            self.dropped = self.dropped.saturating_add(1);
        }
        let owner = if wire { Shelf::Wire } else { Shelf::Host };
        self.pending.push_back((line, owner));
    }

    /// A post: into the ring and the pending queue.
    pub(crate) fn record_posted(&mut self, rec: LogRecord) {
        self.raise(rec.id);
        self.push_pending(LogLine::Posted(rec.clone()));
        self.push_record(rec);
    }

    /// A carried row's record, live again in this process.
    pub(crate) fn adopt_carried(&mut self, rec: LogRecord) {
        self.raise(rec.id);
        let mine = self.replay_candidates(rec.id).into_iter().find(|id| {
            self.get(*id)
                .is_some_and(|r| r.stamp == rec.stamp && r.tag == rec.tag)
        });
        if mine != Some(rec.id) && self.get(rec.id).is_some() {
            let file_id = self.file_id_of(rec.id);
            let fresh = self.mint();
            self.minted_at_replay.insert(fresh.raw());
            if let Some(file_id) = file_id {
                let remaps = self.remapped.entry(file_id).or_default();
                remaps.retain(|id| *id != rec.id);
                remaps.push(fresh);
            }
            if let Some(squatter) = self.get_mut(rec.id) {
                squatter.id = fresh;
            }
        }
        match mine.and_then(|id| self.get_mut(id)) {
            Some(existing) => {
                existing.id = rec.id;
                existing.state = LogState::Posted;
                existing.retired_unix_ms = None;
                existing.retired_at = None;
                existing.title = rec.title;
                existing.detail = rec.detail;
            }
            None => self.push_record(rec),
        }
    }

    /// File record for the row posted under file id `id` at `stamp` with `tag`.
    #[must_use]
    pub fn record_of_file_id(
        &self,
        id: MessageId,
        stamp: WallStamp,
        tag: &Tag,
    ) -> Option<&LogRecord> {
        self.ring
            .iter()
            .rev()
            .find(|r| r.stamp == stamp && r.tag == *tag && self.file_id_of(r.id) == Some(id.raw()))
    }

    /// Mark for the open or `Stale` record posted under file id `id` at `stamp` with `tag`.
    pub(crate) fn open_posted_mark(
        &self,
        id: MessageId,
        stamp: WallStamp,
        tag: &Tag,
    ) -> Option<(Severity, Glyph)> {
        self.ring
            .iter()
            .find(|r| {
                r.stamp == stamp
                    && r.tag == *tag
                    && !Self::closed_by_a_line(r)
                    && self.file_id_of(r.id) == Some(id.raw())
            })
            .map(|r| (r.severity, r.glyph))
    }

    /// A live row now shows `mark` (a restate, or a seeded row over the file's Posted line).
    pub(crate) fn show_mark(&mut self, id: MessageId, mark: (Severity, Glyph)) {
        if let Some(rec) = self.get_mut(id).filter(|r| r.is_live()) {
            (rec.severity, rec.glyph) = mark;
        }
    }

    /// A retirement: the record takes its final words and the line is queued.
    pub(crate) fn record_retired(
        &mut self,
        id: MessageId,
        how: Retired,
        unix_ms: u64,
        now: Instant,
        words: FinalWords<'_>,
    ) {
        if let Some(rec) = self.get_mut(id) {
            rec.state = LogState::Retired(how.clone());
            rec.retired_unix_ms = Some(unix_ms);
            rec.retired_at = Some(now);
            rec.title = words.title.to_string();
            rec.detail = words.detail.to_vec();
            rec.repeats = words.repeats;
            if let Some((severity, glyph)) = words.mark {
                rec.severity = severity;
                rec.glyph = glyph;
            }
        }
        self.push_pending(LogLine::Retired {
            id,
            how,
            unix_ms,
            title: words.title.to_string(),
            detail: words.detail.to_vec(),
            repeats: words.repeats,
            mark: words.mark,
        });
    }

    /// The parent's ending of a carried row, adopted at Commit.
    pub(crate) fn adopt_ending(&mut self, id: MessageId, from: &LogRecord) {
        if let Some(rec) = self.get_mut(id) {
            rec.state = from.state.clone();
            rec.retired_unix_ms = from.retired_unix_ms;
            rec.retired_at = None;
            rec.title.clone_from(&from.title);
            rec.detail.clone_from(&from.detail);
            rec.repeats = from.repeats;
            rec.severity = from.severity;
            rec.glyph = from.glyph;
            if rec.last_action.is_none() {
                rec.last_action.clone_from(&from.last_action);
            }
        }
    }

    /// The cause behind `key`'s digest entry ended while nothing was open.
    pub(crate) fn resolve_filing(&mut self, key: &str, unix_ms: u64, now: Instant) -> bool {
        let Some(last) = self.last_word(key, true).filter(|r| r.filed_exception()) else {
            return false;
        };
        let (id, title, detail, repeats) = (
            last.id,
            last.title.clone(),
            last.detail.clone(),
            last.repeats,
        );
        self.record_retired(
            id,
            Retired::Resolved(Outcome::Ok),
            unix_ms,
            now,
            FinalWords {
                title: &title,
                detail: &detail,
                repeats,
                mark: None,
            },
        );
        true
    }

    /// Record that `id` no longer keeps its standing row's key quiet.
    pub(crate) fn withdraw_read(&mut self, id: MessageId, unix_ms: u64, now: Instant) -> bool {
        let Some(read) = self.get(id) else {
            return false;
        };
        let (title, detail, repeats) = (read.title.clone(), read.detail.clone(), read.repeats);
        self.record_retired(
            id,
            Retired::Withdrawn,
            unix_ms,
            now,
            FinalWords {
                title: &title,
                detail: &detail,
                repeats,
                mark: None,
            },
        );
        true
    }

    /// A record re-worded in place ([`crate::MessageCenter::reword_record`]).
    pub(crate) fn record_reworded(
        &mut self,
        id: MessageId,
        title: &str,
        detail: &[String],
    ) -> bool {
        let Some(rec) = self.get_mut(id) else {
            return false;
        };
        let (LogState::Retired(how), Some(unix_ms)) = (rec.state.clone(), rec.retired_unix_ms)
        else {
            return false;
        };
        if rec.title == title && rec.detail == detail {
            return false;
        }
        rec.title = title.to_string();
        rec.detail = detail.to_vec();
        let repeats = rec.repeats;
        self.push_pending(LogLine::Retired {
            id,
            how,
            unix_ms,
            title: title.to_string(),
            detail: detail.to_vec(),
            repeats,
            mark: None,
        });
        true
    }

    /// A retired record retired again, as `how` ([`crate::MessageCenter::supersede_record`]).
    pub(crate) fn record_retired_again(&mut self, id: MessageId, how: Retired) -> bool {
        let Some(rec) = self.get_mut(id) else {
            return false;
        };
        let (LogState::Retired(was), Some(unix_ms)) = (&rec.state, rec.retired_unix_ms) else {
            return false;
        };
        if *was == how {
            return false;
        }
        rec.state = LogState::Retired(how.clone());
        let line = LogLine::Retired {
            id,
            how,
            unix_ms,
            title: rec.title.clone(),
            detail: rec.detail.clone(),
            repeats: rec.repeats,
            mark: None,
        };
        self.push_pending(line);
        true
    }

    /// A capsule press.
    pub(crate) fn record_acted(&mut self, id: MessageId, unix_ms: u64, label: &str) {
        if let Some(rec) = self.get_mut(id) {
            rec.last_action = Some((label.to_string(), unix_ms));
        }
        self.push_pending(LogLine::Acted {
            id,
            unix_ms,
            label: label.to_string(),
        });
    }
}

#[cfg(test)]
#[path = "log_tests.rs"]
pub(crate) mod tests;
