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
        let (kind, payload) = s.split_once(':').unwrap_or((s, ""));
        match (kind, payload) {
            ("folded", "") => Some(Self::Folded),
            ("stale", "") => Some(Self::Stale),
            ("unseen", "") => Some(Self::Unseen),
            ("superseded", id) => Some(Self::Superseded {
                by: MessageId::from_raw(id.parse().ok()?)?,
            }),
            ("resolved", "ok") => Some(Self::Resolved(Outcome::Ok)),
            ("resolved", "warn") => Some(Self::Resolved(Outcome::Warn)),
            ("dismissed", "") => Some(Self::Dismissed),
            ("answered", label) => Some(Self::Answered {
                label: label.to_string(),
            }),
            ("evicted", "") => Some(Self::Evicted),
            ("carried", "") => Some(Self::Carried),
            ("withdrawn", "") => Some(Self::Withdrawn),
            ("recorded", "") => Some(Self::Recorded),
            ("quit", "") => Some(Self::Quit),
            _ => None,
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
            .find(|(k, _)| k == key)
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
        self.need(key)?
            .parse()
            .map_err(|_| CodecError::BadField(key))
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
pub(crate) mod tests {
    use super::*;
    use crate::model::{Decision, tags};

    /// What the decoder hands back for `line`: every word clipped to the model's caps and control-free, empty detail lines dropped.
    pub(crate) fn as_decoded(line: &LogLine) -> LogLine {
        let lines = |d: &[String]| -> Vec<String> {
            d.iter()
                .map(|l| clip(l, DETAIL_LINE_CAP))
                .filter(|l| !l.is_empty())
                .take(DETAIL_LINES_CAP)
                .collect()
        };
        match line {
            LogLine::Posted(r) => LogLine::Posted(LogRecord {
                title: clip(&r.title, TITLE_CAP),
                detail: lines(&r.detail),
                key: r
                    .key
                    .as_deref()
                    .map(|k| clip(k, KEY_CAP))
                    .filter(|k| !k.is_empty()),
                ..r.clone()
            }),
            LogLine::Retired {
                id,
                how,
                unix_ms,
                title,
                detail,
                repeats,
                mark,
            } => LogLine::Retired {
                id: *id,
                how: match how {
                    Retired::Answered { label } => Retired::Answered {
                        label: clip(label, TITLE_CAP),
                    },
                    other => other.clone(),
                },
                unix_ms: *unix_ms,
                title: clip(title, TITLE_CAP),
                detail: lines(detail),
                repeats: *repeats,
                mark: *mark,
            },
            LogLine::Acted { id, unix_ms, label } => LogLine::Acted {
                id: *id,
                unix_ms: *unix_ms,
                label: clip(label, TITLE_CAP),
            },
            other => other.clone(),
        }
    }

    fn posted(id: u64) -> LogRecord {
        LogRecord {
            id: MessageId::from_raw(id).unwrap(),
            stamp: WallStamp {
                unix_ms: 1_758_470_000_000 + id,
            },
            tag: tags::CRASH,
            severity: Severity::Error,
            posted_severity: Severity::Error,
            glyph: Glyph::new('\u{26a0}').unwrap(),
            title: "tab\there \\ back\nnew\rcr\u{1f}us \u{00b7} end".into(),
            detail: vec!["line one\twith tab".into(), String::new(), "three\\".into()],
            actions: vec![
                Intent::OpenPath {
                    path: "/a b/c\t.log".into(),
                },
                Intent::NotNow {
                    decision: Decision::FileAccess,
                },
            ],
            key: Some("crash.last=\tk".into()),
            origin: Origin::Wire,
            repeats: 1,
            state: LogState::Posted,
            retired_unix_ms: None,
            retired_at: None,
            last_action: None,
            later: false,
        }
    }

    #[test]
    fn codec_round_trips_every_field_and_skips_junk() {
        let lines = vec![
            LogLine::Posted(posted(7)),
            LogLine::Retired {
                id: MessageId::from_raw(7).unwrap(),
                how: Retired::Superseded {
                    by: MessageId::from_raw(9).unwrap(),
                },
                unix_ms: 5,
                title: "final\twords \u{1f} us".into(),
                detail: vec!["a".into(), "b\\c".into()],
                repeats: 3,
                mark: None,
            },
            LogLine::Retired {
                id: MessageId::from_raw(8).unwrap(),
                how: Retired::Answered {
                    label: "Not now".into(),
                },
                unix_ms: 6,
                title: "t".into(),
                detail: vec![],
                repeats: 1,
                mark: None,
            },
            LogLine::Retired {
                id: MessageId::from_raw(8).unwrap(),
                how: Retired::Resolved(Outcome::Warn),
                unix_ms: 6,
                title: "t".into(),
                detail: vec![],
                repeats: 1,
                mark: Some((Severity::Warn, Glyph::or_fallback('\u{26a0}'))),
            },
            LogLine::Acted {
                id: MessageId::from_raw(7).unwrap(),
                unix_ms: 9,
                label: "Open log".into(),
            },
            LogLine::Dropped { count: 12 },
        ];
        for line in &lines {
            let enc = line.encode();
            assert!(enc.starts_with("m1\t") && enc.ends_with("\tend="), "{enc}");
            assert!(
                !enc.contains('\n') && !enc.contains('\r') && !enc.contains('\u{1f}'),
                "{enc}"
            );
            let back = LogLine::decode(&enc).unwrap_or_else(|e| panic!("{e}: {enc}"));
            assert_eq!(back, as_decoded(line), "{enc}");
            assert_eq!(
                LogLine::decode(&format!("{enc}\n")).unwrap(),
                as_decoded(line),
                "a trailing newline is fine"
            );
            assert_eq!(
                LogLine::decode(&back.encode()).unwrap(),
                back,
                "what the decoder hands back is the identity from then on"
            );
        }
        let enc = lines[0].encode();
        assert!(
            enc.contains("\\t") && enc.contains("\\n") && enc.contains("\\r"),
            "{enc}"
        );
        match LogLine::decode(&enc).unwrap() {
            LogLine::Posted(rec) => {
                assert_eq!(rec.origin, Origin::Wire);
                assert_eq!(rec.key.as_deref(), Some("crash.last=k"));
                assert_eq!(rec.actions.len(), 2);
                assert_eq!(rec.title, "tabhere \\ backnewcrus \u{00b7} end");
                assert_eq!(
                    rec.detail,
                    vec!["line onewith tab".to_string(), "three\\".into()]
                );
            }
            other => panic!("{other:?}"),
        }
        for how in [
            Retired::Folded,
            Retired::Stale,
            Retired::Unseen,
            Retired::Dismissed,
            Retired::Evicted,
            Retired::Carried,
            Retired::Withdrawn,
            Retired::Quit,
            Retired::Resolved(Outcome::Ok),
        ] {
            assert_eq!(Retired::decode(&how.encode()), Some(how));
        }
        let mut us_inside = posted(5);
        us_inside.detail = vec!["b\u{1f}c".into()];
        match LogLine::decode(&LogLine::Posted(us_inside).encode()).unwrap() {
            LogLine::Posted(rec) => assert_eq!(rec.detail, vec!["bc".to_string()]),
            other => panic!("{other:?}"),
        }
        assert_eq!(LogLine::decode("hello"), Err(CodecError::NotM1));
        assert_eq!(
            LogLine::decode("m2\tkind=posted\tid=1"),
            Err(CodecError::NotM1)
        );
        let full = LogLine::Posted(posted(3)).encode();
        let cut = &full[..full.len() / 2];
        assert_eq!(
            LogLine::decode(cut),
            Err(CodecError::MissingField("origin")),
            "{cut}"
        );
        let retired = lines[1].encode();
        let cut = &retired[..retired.len() - 2];
        assert_eq!(LogLine::decode(cut), Err(CodecError::Truncated), "{cut}");
        assert_eq!(
            LogLine::decode(full.trim_end_matches("\tend=")),
            Err(CodecError::Truncated),
            "a complete line with no terminator was cut inside its last value"
        );
        assert_eq!(
            LogLine::decode("m1\tkind=teleport\tid=1"),
            Err(CodecError::UnknownKind)
        );
        assert_eq!(
            LogLine::decode(
                "m1\tkind=posted\tid=0\tt=1\ttag=crash\tsev=error\ttitle=x\torigin=host"
            ),
            Err(CodecError::BadField("id"))
        );
        assert_eq!(
            LogLine::decode("m1\tkind=posted\tid=1\tt=1\ttag=crash\tsev=error\ttitle=x\tend="),
            Err(CodecError::MissingField("origin")),
            "a required field is named before the terminator is asked for"
        );
        let long = format!("m1\tkind=dropped\tn=1\tpad={}", "x".repeat(MAX_LINE_BYTES));
        assert_eq!(LogLine::decode(&long), Err(CodecError::TooLong));
        let novel = "m1\tkind=posted\tid=4\tt=1\ttag=system\tsev=info\tglyph=\u{2699}\ttitle=x\tactions=teleport:home\u{1f}new-window\tfuture=yes\torigin=host\tend=";
        match LogLine::decode(novel).unwrap() {
            LogLine::Posted(rec) => {
                assert_eq!(rec.glyph, Glyph::FALLBACK);
                assert_eq!(rec.actions, vec![Intent::NewWindow]);
                assert_eq!(rec.detail, Vec::<String>::new());
                assert_eq!(rec.key, None);
                assert_eq!(rec.origin, Origin::Host);
            }
            other => panic!("{other:?}"),
        }

        let mut log = MessageLog::empty();
        for line in &lines {
            log.replay(line.clone());
        }
        log.replay(LogLine::Posted(posted(40)));
        assert_eq!(
            log.len(),
            2,
            "ids 7 and 40 (8's Retired had no Posted to merge into)"
        );
        let seven = log.get(MessageId::from_raw(7).unwrap()).unwrap();
        assert_eq!(
            seven.state,
            LogState::Retired(Retired::Superseded {
                by: MessageId::from_raw(9).unwrap()
            })
        );
        assert_eq!(
            seven.title, "final\twords \u{1f} us",
            "an in-memory line replays as given: the clipping is the DECODER's (the file ingress)"
        );
        assert_eq!(seven.repeats, 3);
        assert_eq!(seven.retired_at, None, "loaded from disk: not this process");
        assert_eq!(seven.last_action, Some(("Open log".to_string(), 9)));
        let forty = log.get(MessageId::from_raw(40).unwrap()).unwrap();
        assert_eq!(
            forty.state,
            LogState::Retired(Retired::Stale),
            "nobody folded it: stale"
        );
        assert_eq!(log.next_id().raw(), 41);
        log.replay(LogLine::Posted(posted(40)));
        assert_eq!(log.len(), 2, "deduped by id");
    }

    #[test]
    fn the_vault_sync_press_reads_back_and_an_older_build_keeps_the_rest() {
        let mut rec = posted(5);
        rec.tag = tags::PRIVACY;
        rec.title = "Joining the secrets vault failed".into();
        rec.detail = vec![
            "the secrets vault did not let this Mac in".into(),
            "Apply in System State with the vault row on and the Mac settings rows off".into(),
        ];
        rec.actions = vec![Intent::ReviewVaultSync, Intent::NewWindow];
        rec.key = Some("privacy.secrets.failed".into());
        rec.later = true;
        let line = LogLine::Posted(rec.clone()).encode();
        assert!(line.contains("review-vault-sync"), "{line}");
        assert_eq!(LogLine::decode(&line), Ok(LogLine::Posted(rec.clone())));

        let older = line.replace("review-vault-sync", "unknown-kind");
        rec.actions = vec![Intent::NewWindow];
        assert_eq!(LogLine::decode(&older), Ok(LogLine::Posted(rec)));
    }

    #[test]
    fn a_closed_records_id_posted_again_is_another_record() {
        let id = MessageId::from_raw(15).unwrap();
        let retired = |title: &str| LogLine::Retired {
            id,
            how: Retired::Folded,
            unix_ms: 9,
            title: title.into(),
            detail: vec![],
            repeats: 1,
            mark: None,
        };
        let mut log = MessageLog::empty();
        log.replay(LogLine::Posted(LogRecord {
            title: "2 keys have no effect".into(),
            ..posted(15)
        }));
        log.replay(retired("2 keys have no effect"));
        log.replay(LogLine::Posted(LogRecord {
            title: "Claude Code 2.1.283 is up to date".into(),
            ..posted(15)
        }));
        log.replay(retired("Claude Code 2.1.283 is up to date"));
        let titles: Vec<&str> = log.records().map(|r| r.title.as_str()).collect();
        assert_eq!(
            titles,
            ["2 keys have no effect", "Claude Code 2.1.283 is up to date"]
        );
        assert!(
            log.records()
                .all(|r| r.state == LogState::Retired(Retired::Folded))
        );

        let ids: Vec<u64> = log.records().map(|r| r.id.raw()).collect();
        assert_eq!(ids, [15, 16], "one id per record (ruling 270)");
        assert_eq!(log.next_id().raw(), 17);

        let mut open = MessageLog::empty();
        open.replay(LogLine::Posted(posted(15)));
        open.replay(LogLine::Posted(posted(15)));
        assert_eq!(open.len(), 1, "still open: the same record, deduped");

        let mut both = MessageLog::empty();
        both.replay(LogLine::Posted(LogRecord {
            title: "Claude Code 2.1.283 is up to date".into(),
            ..posted(15)
        }));
        both.replay(LogLine::Posted(LogRecord {
            title: "Built aterm".into(),
            ..posted(15)
        }));
        both.replay(retired("Claude Code 2.1.283 is up to date"));
        both.replay(LogLine::Acted {
            id,
            unix_ms: 10,
            label: "Copy".into(),
        });
        let words: Vec<(u64, &str, bool)> = both
            .records()
            .map(|r| {
                (
                    r.id.raw(),
                    r.title.as_str(),
                    r.state == LogState::Retired(Retired::Folded),
                )
            })
            .collect();
        assert_eq!(
            words,
            [
                (15, "Claude Code 2.1.283 is up to date", true),
                (16, "Built aterm", false)
            ]
        );
        assert_eq!(
            both.get(MessageId::from_raw(16).unwrap())
                .and_then(|r| r.last_action.clone()),
            Some(("Copy".to_string(), 10)),
            "a later line goes to the record still open"
        );
        both.replay(LogLine::Posted(LogRecord {
            title: "Paste stopped".into(),
            ..posted(16)
        }));
        let ids: Vec<u64> = both.records().map(|r| r.id.raw()).collect();
        assert_eq!(ids, [15, 16, 17]);

        let mut file = MessageLog::empty();
        file.replay_all([
            LogLine::Posted(posted(15)),
            retired("tab\there \\ back\nnew\rcr\u{1f}us \u{00b7} end"),
            LogLine::Posted(LogRecord {
                title: "Claude Code 2.1.283 is up to date".into(),
                ..posted(15)
            }),
            LogLine::Posted(posted(16)),
        ]);
        let ids: Vec<u64> = file.records().map(|r| r.id.raw()).collect();
        assert_eq!(ids, [15, 17, 16], "16 keeps its number");
    }

    #[test]
    fn a_delivered_retirement_goes_to_the_record_its_finished_words_name() {
        let id = MessageId::from_raw(15).unwrap();
        let delivered = |title: &str, unix_ms: u64| LogLine::Retired {
            id,
            how: Retired::Folded,
            unix_ms,
            title: title.into(),
            detail: vec![],
            repeats: 1,
            mark: Some((Severity::Success, Glyph::or_fallback('\u{2713}'))),
        };
        let load = |a: &str, b: &str, finals: [(&str, u64); 2]| {
            let mut log = MessageLog::empty();
            log.replay(LogLine::Posted(LogRecord {
                title: a.into(),
                ..posted(15)
            }));
            log.replay(LogLine::Posted(LogRecord {
                title: b.into(),
                ..posted(15)
            }));
            for (title, at) in finals {
                log.replay(delivered(title, at));
            }
            log.records()
                .map(|r| (r.id.raw(), r.title.clone(), r.retired_unix_ms))
                .collect::<Vec<_>>()
        };
        assert_eq!(
            load(
                "Deploying site",
                "Building docs",
                [("Deployed site", 20), ("Built docs", 30)]
            ),
            [
                (15, "Deployed site".to_string(), Some(20)),
                (16, "Built docs".to_string(), Some(30)),
            ],
            "the older record retired first keeps its own words"
        );
        assert_eq!(
            load(
                "Uploading the backup",
                "Building docs",
                [
                    ("Uploading the backup \u{2014} done", 20),
                    ("Built docs", 30)
                ]
            ),
            [
                (
                    15,
                    "Uploading the backup \u{2014} done".to_string(),
                    Some(20)
                ),
                (16, "Built docs".to_string(), Some(30)),
            ],
            "a `— done` title names its record too"
        );
        assert_eq!(
            load(
                "Deploying site",
                "Building docs",
                [("Deploying stopped", 20), ("Built docs", 30)]
            )[0],
            (15, "Deploying stopped".to_string(), Some(20)),
            "a stopped form names its record"
        );
        assert_eq!(
            load(
                "Deploying site",
                "Building docs",
                [("Site is live", 20), ("Deployed site", 30)]
            ),
            [
                (15, "Deployed site".to_string(), Some(30)),
                (16, "Site is live".to_string(), Some(20)),
            ]
        );
    }

    #[test]
    fn every_line_is_shelved_with_its_records_owner() {
        let mut log = MessageLog::empty();
        let post = |log: &mut MessageLog, origin: Origin, key: Option<&str>| {
            let id = log.mint();
            log.record_posted(LogRecord {
                id,
                origin,
                key: key.map(str::to_string),
                ..posted(1)
            });
            log.record_retired(
                id,
                Retired::Recorded,
                2,
                Instant::now(),
                FinalWords {
                    title: "t",
                    detail: &[],
                    repeats: 1,
                    mark: None,
                },
            );
            log.record_acted(id, 3, "Details");
            id
        };
        let host = post(&mut log, Origin::Host, None);
        let wire = post(&mut log, Origin::Wire, None);
        let carried = post(&mut log, Origin::Carried, Some("wire.build"));
        let shelved = log.drain_shelved();
        assert_eq!(shelved.len(), 9);
        for (line, shelf) in &shelved {
            let want = if line.id() == Some(host) {
                Shelf::Host
            } else {
                assert!(line.id() == Some(wire) || line.id() == Some(carried));
                Shelf::Wire
            };
            assert_eq!(*shelf, want, "{line:?}");
        }
        for _ in 0..(PENDING_PERSIST_CAP + 1) {
            post(&mut log, Origin::Wire, None);
        }
        let shelved = log.drain_shelved();
        let count = u32::try_from((PENDING_PERSIST_CAP + 1) * 3 - PENDING_PERSIST_CAP).unwrap();
        assert_eq!(
            shelved.last(),
            Some(&(LogLine::Dropped { count }, Shelf::Host))
        );
    }

    #[test]
    fn wire_records_never_push_aterms_own_out_of_the_ring() {
        let retire = |log: &mut MessageLog, rec: LogRecord| {
            let id = rec.id;
            log.record_posted(rec);
            log.record_retired(
                id,
                Retired::Recorded,
                0,
                Instant::now(),
                FinalWords {
                    title: "t",
                    detail: &[],
                    repeats: 1,
                    mark: None,
                },
            );
        };
        let host = |log: &mut MessageLog| {
            let id = log.mint();
            LogRecord {
                id,
                origin: Origin::Host,
                key: None,
                ..posted(1)
            }
        };
        let wire = |log: &mut MessageLog| {
            let id = log.mint();
            LogRecord {
                id,
                origin: Origin::Wire,
                key: None,
                ..posted(1)
            }
        };
        let mut log = MessageLog::empty();
        let crash = host(&mut log);
        let crash_id = crash.id;
        retire(&mut log, crash);
        for _ in 0..600 {
            let rec = wire(&mut log);
            retire(&mut log, rec);
        }
        assert_eq!(log.len(), LOG_CAP);
        assert!(log.get(crash_id).is_some(), "the host record stays");
        let mut log = MessageLog::empty();
        for _ in 0..LOG_CAP {
            let rec = host(&mut log);
            retire(&mut log, rec);
        }
        for _ in 0..600 {
            let rec = wire(&mut log);
            retire(&mut log, rec);
        }
        let wire_n = log.records().filter(|r| r.wire_owned()).count();
        assert_eq!(wire_n, WIRE_LOG_SHARE);
        assert_eq!(log.len() - wire_n, LOG_CAP - WIRE_LOG_SHARE);
        assert_eq!(log.wire_records, wire_n, "the kept count is the ring's");
        let carried = LogRecord {
            origin: Origin::Carried,
            key: Some("wire.build".into()),
            ..posted(1)
        };
        assert!(carried.wire_owned());
        let hosted = LogRecord {
            origin: Origin::Carried,
            key: Some("update.progress".into()),
            ..posted(1)
        };
        assert!(!hosted.wire_owned());
    }

    #[test]
    fn a_wire_filing_outlives_the_wires_own_churn_inside_its_share() {
        let file = |log: &mut MessageLog, rec: LogRecord, how: Retired| {
            let id = rec.id;
            let at = rec.stamp.unix_ms;
            log.record_posted(rec);
            log.record_retired(
                id,
                how,
                at,
                Instant::now(),
                FinalWords {
                    title: "t",
                    detail: &[],
                    repeats: 1,
                    mark: None,
                },
            );
            id
        };
        let next = |log: &mut MessageLog, origin: Origin, later: bool| {
            let id = log.mint();
            LogRecord {
                id,
                origin,
                key: later.then(|| format!("wire.k{}", id.raw())),
                later,
                ..posted(id.raw())
            }
        };
        let fill_with_host = |log: &mut MessageLog| {
            for _ in 0..LOG_CAP {
                let rec = next(log, Origin::Host, false);
                file(log, rec, Retired::Recorded);
            }
        };
        let mut log = MessageLog::empty();
        fill_with_host(&mut log);
        let rec = next(&mut log, Origin::Wire, true);
        let dismissed = file(&mut log, rec, Retired::Dismissed);
        for _ in 0..600 {
            let rec = next(&mut log, Origin::Wire, false);
            file(&mut log, rec, Retired::Recorded);
        }
        assert!(log.get(dismissed).is_some(), "the owner's dismissal stays");
        let wire_n = log.records().filter(|r| r.wire_owned()).count();
        assert_eq!(wire_n, WIRE_LOG_SHARE, "the share holds");
        assert_eq!(log.wire_records, wire_n);
        let mut log = MessageLog::empty();
        fill_with_host(&mut log);
        for _ in 0..600 {
            let rec = next(&mut log, Origin::Wire, true);
            file(&mut log, rec, Retired::Dismissed);
        }
        let wire_n = log.records().filter(|r| r.wire_owned()).count();
        assert_eq!(
            wire_n, WIRE_LOG_SHARE,
            "filings never take aterm's own place"
        );
        assert_eq!(log.len() - wire_n, LOG_CAP - WIRE_LOG_SHARE);
    }

    #[test]
    fn codec_reads_back_a_message_at_the_crates_own_caps() {
        for (name, unit) in [
            ("box-drawing", "\u{2500}"),
            ("backslash", "\\"),
            ("cjk", "\u{6f22}"),
            ("emoji", "\u{1f680}"),
        ] {
            let msg = Message::new(tags::CRASH, Severity::Error, &unit.repeat(TITLE_CAP))
                .lines((0..DETAIL_LINES_CAP).map(|_| unit.repeat(DETAIL_LINE_CAP)))
                .key(&unit.repeat(KEY_CAP))
                .action(Intent::OpenPath {
                    path: format!("/{}", unit.repeat(200)),
                })
                .action(Intent::OpenSystemPane {
                    pane: unit.repeat(200),
                })
                .normalized();
            let rec = LogRecord::from_posted(MessageId::FIRST, WallStamp { unix_ms: 1 }, &msg);
            let enc = LogLine::Posted(rec.clone()).encode();
            assert!(
                enc.len() <= MAX_LINE_BYTES / 2,
                "{name}: {} bytes",
                enc.len()
            );
            let back = LogLine::decode(&enc);
            assert_eq!(
                back,
                Ok(LogLine::Posted(rec)),
                "{name}: a line the encoder wrote ({} bytes) must read back whole",
                enc.len()
            );
        }
        let worst = 4 * (TITLE_CAP + DETAIL_LINES_CAP * DETAIL_LINE_CAP + KEY_CAP)
            + 2 * DETAIL_LINES_CAP
            + 8 * 1024;
        assert!(
            worst <= MAX_LINE_BYTES / 2,
            "{worst} > {}",
            MAX_LINE_BYTES / 2
        );
    }

    #[test]
    fn a_truncated_last_field_fails_to_decode() {
        let acted = LogLine::Acted {
            id: MessageId::from_raw(7).unwrap(),
            unix_ms: 9,
            label: "Open log".into(),
        };
        let enc = acted.encode();
        let cut = &enc[..enc.len() - 4];
        let back = LogLine::decode(cut);
        assert!(
            back.is_err(),
            "a cut inside the label decodes to a different label: {back:?} from {cut:?}"
        );
        let retired = LogLine::Retired {
            id: MessageId::from_raw(7).unwrap(),
            how: Retired::Folded,
            unix_ms: 5,
            title: "t".into(),
            detail: vec![],
            repeats: 12,
            mark: None,
        };
        let enc = retired.encode();
        let back = LogLine::decode(&enc[..enc.len() - 1]);
        assert!(
            back.is_err(),
            "a cut inside rep= decodes to a different repeat count: {back:?}"
        );
        let enc = LogLine::Dropped { count: 12 }.encode();
        let back = LogLine::decode(&enc[..enc.len() - 1]);
        assert!(
            back.is_err(),
            "a cut inside n= decodes to a different count: {back:?}"
        );
    }

    #[test]
    fn ids_keep_rising_after_replaying_a_max_id() {
        let mut log = MessageLog::empty();
        let line = format!(
            "m1\tkind=posted\tid={}\tt=1\ttag=crash\tsev=error\ttitle=x\torigin=host\tend=",
            u64::MAX
        );
        log.replay(LogLine::decode(&line).unwrap());
        assert_eq!(log.len(), 1, "the record itself is kept");
        let a = log.mint();
        let b = log.mint();
        assert_ne!(
            a, b,
            "two mints handed out the same id {a} after a replayed id at u64::MAX"
        );
        assert_eq!(a, MessageId::FIRST);
        log.raise_to(u64::MAX);
        let c = log.mint();
        let d = log.mint();
        assert!(
            b < c && c < d,
            "{b} {c} {d}: a forged carry cannot park the sequence"
        );
        log.raise_to(ID_CEILING - 1);
        assert_eq!(
            log.mint().raw(),
            ID_CEILING - 1,
            "just below the ceiling is honoured"
        );
    }

    #[test]
    fn a_reloaded_file_merges_what_the_parent_wrote_after_the_boot() {
        let line = |l: LogLine| LogLine::decode(&l.encode()).unwrap();
        let retired = |id: u64| LogLine::Retired {
            id: MessageId::from_raw(id).unwrap(),
            how: Retired::Resolved(crate::Outcome::Ok),
            unix_ms: 1_758_470_100_000,
            title: "done".into(),
            detail: Vec::new(),
            repeats: 1,
            mark: None,
        };
        let boot_lines = vec![
            line(LogLine::Posted(posted(1))),
            line(retired(1)),
            line(LogLine::Posted(posted(2))),
            line(LogLine::Posted(posted(3))),
        ];
        let mut ring = MessageLog::empty();
        ring.replay_all(boot_lines.clone());
        assert_eq!(
            ring.get(MessageId::from_raw(2).unwrap()).unwrap().retired(),
            Some(&Retired::Stale),
            "the boot reads the parent's open row as nobody's"
        );
        let mut carried = posted(3);
        carried.title = "restated".into();
        ring.raise_to(5);
        ring.adopt_carried(carried);
        let mut own = posted(5);
        own.stamp.unix_ms = 1_758_470_900_000;
        own.title = "the successor's own".into();
        ring.record_posted(own);
        let mut disk_lines = boot_lines;
        disk_lines.push(line(retired(2)));
        disk_lines.push(line(LogLine::Posted(posted(4))));
        disk_lines.push(line(retired(4)));
        disk_lines.push(line(LogLine::Posted(posted(5))));
        let mut disk = MessageLog::empty();
        disk.replay_all(disk_lines);

        ring.merge_reloaded(&disk);
        let two = ring.get(MessageId::from_raw(2).unwrap()).unwrap();
        assert!(
            matches!(two.retired(), Some(Retired::Resolved(_))),
            "the parent's ending, not Stale: {:?}",
            two.state
        );
        assert_eq!(two.title, "done");
        let three = ring.get(MessageId::from_raw(3).unwrap()).unwrap();
        assert!(three.is_live(), "a row live here stays live");
        assert_eq!(three.title, "restated");
        assert!(
            ring.get(MessageId::from_raw(4).unwrap())
                .is_some_and(|r| matches!(r.retired(), Some(Retired::Resolved(_)))),
            "a record the parent wrote after the boot is in the history"
        );
        let fives: Vec<&LogRecord> = ring
            .records()
            .filter(|r| r.stamp == posted(5).stamp || r.title == "the successor's own")
            .collect();
        assert_eq!(fives.len(), 2, "both records numbered 5 are kept");
        assert_eq!(
            ring.get(MessageId::from_raw(5).unwrap()).unwrap().title,
            "the successor's own",
            "this process's own number stays its own"
        );
        let ids: Vec<u64> = ring.records().map(|r| r.id.raw()).collect();
        let unique: std::collections::BTreeSet<u64> = ids.iter().copied().collect();
        assert_eq!(unique.len(), ids.len(), "one id, one record: {ids:?}");
        assert!(ring.next_id().raw() > *unique.last().unwrap());
        let before: Vec<LogRecord> = ring.records().cloned().collect();
        ring.merge_reloaded(&disk);
        assert_eq!(ring.records().cloned().collect::<Vec<_>>(), before);
        assert_eq!(
            ring.pending_len(),
            1,
            "a merge queues no line: only the own post"
        );
    }

    #[test]
    fn loaded_records_are_capped_like_posted_ones() {
        let detail: Vec<String> = (0..30)
            .map(|i| format!("{i} {}", "d".repeat(300)))
            .collect();
        let line = format!(
            "m1\tkind=posted\tid=4\tt=1\ttag=system\tsev=info\ttitle={}\\n\\t\u{1b}[2J\tdetail={}\u{1f}\u{1f}\\t\tkey={}\torigin=host\tend=",
            "a".repeat(5000),
            detail.join("\u{1f}"),
            "k".repeat(60)
        );
        let LogLine::Posted(rec) = LogLine::decode(&line).unwrap() else {
            panic!()
        };
        assert!(
            rec.title.chars().count() <= TITLE_CAP,
            "a loaded title is {} chars (cap {TITLE_CAP})",
            rec.title.chars().count()
        );
        assert!(
            rec.title.chars().all(|c| !c.is_control()),
            "a loaded title carries control characters"
        );
        assert_eq!(rec.detail.len(), DETAIL_LINES_CAP);
        assert!(
            rec.detail
                .iter()
                .all(|l| l.chars().count() <= DETAIL_LINE_CAP)
        );
        assert!(
            rec.detail.iter().all(|l| !l.is_empty()),
            "empty and control-only lines go"
        );
        assert_eq!(rec.key.as_ref().map(|k| k.chars().count()), Some(KEY_CAP));
        let retired = format!(
            "m1\tkind=retired\tid=4\thow=answered:{}\tt=2\ttitle={}\tdetail=\trep=1\tend=",
            "l".repeat(500),
            "t".repeat(500)
        );
        match LogLine::decode(&retired).unwrap() {
            LogLine::Retired { how, title, .. } => {
                assert_eq!(title.chars().count(), TITLE_CAP);
                assert!(
                    matches!(how, Retired::Answered { label } if label.chars().count() == TITLE_CAP)
                );
            }
            other => panic!("{other:?}"),
        }
    }

    #[test]
    fn a_lone_empty_detail_line_round_trips() {
        let msg = Message::new(tags::SYSTEM, Severity::Info, "t").line("");
        assert_eq!(msg.detail, Vec::<String>::new(), "dropped at ingress");
        let line = LogLine::Posted(LogRecord::from_posted(
            MessageId::FIRST,
            WallStamp { unix_ms: 1 },
            &msg,
        ));
        assert_eq!(LogLine::decode(&line.encode()).unwrap(), line);
        let mut literal = posted(2);
        literal.detail = vec![String::new()];
        let back = LogLine::decode(&LogLine::Posted(literal.clone()).encode()).unwrap();
        assert_eq!(back, as_decoded(&LogLine::Posted(literal)));
        match back {
            LogLine::Posted(rec) => assert_eq!(rec.detail, Vec::<String>::new()),
            other => panic!("{other:?}"),
        }
    }
}
