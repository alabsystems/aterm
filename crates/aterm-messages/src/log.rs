// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! The durable tagged log: a bounded ring of records, the line codec the
//! host appends to `<log_dir>/messages.log`, and the pending-persist queue
//! (D4: the host appends, the engine encodes). One record per line,
//! `m1<TAB>kind<TAB>k=v<TAB>k=v…`; values escape `\`, TAB, LF, CR and US;
//! unknown keys are ignored and unknown kinds skipped, so an older build
//! reads a newer file and vice versa. Every line ends with an `end=` field
//! the decoder requires, so a line cut short by a crash mid-append —
//! anywhere, inside its last value included — fails to decode instead of
//! reading as a shorter record. A file is an ingress like any other: the
//! decoder clips every word to the model's caps and drops empty detail
//! lines, so a hand-edited or hostile line cannot put in the ring what
//! `post` would never have admitted.

use std::collections::{BTreeMap, BTreeSet, VecDeque};
use std::fmt;

use crate::center::Outcome;
use crate::model::{Glyph, Intent, Message, MessageId, Origin, Severity, Tag, WallStamp};
use crate::text::clip;
use crate::{
    DETAIL_LINE_CAP, DETAIL_LINES_CAP, Instant, KEY_CAP, LOG_CAP, PENDING_PERSIST_CAP, TITLE_CAP,
    WIRE_LOG_SHARE,
};

/// The longest line the decoder accepts. Sized from the model's caps so that
/// EVERY line the encoder writes for a message within them decodes: the caps
/// are in chars, a char is up to four bytes, so [`TITLE_CAP`] +
/// [`DETAIL_LINES_CAP`] × [`DETAIL_LINE_CAP`] + [`KEY_CAP`] chars, the escaped
/// joints, the fixed fields and two encoded intents come to under 32 KiB —
/// half of this. A line past it is foreign or corrupt and the loader skips
/// it (`codec_reads_back_a_message_at_the_crates_own_caps` measures both).
pub const MAX_LINE_BYTES: usize = 64 * 1024;

/// No id at or past this ever raises the sequence: a corrupt line or a
/// forged carry cannot park `next_id` at the ceiling, where every later
/// mint would hand out the same saturated id. No process reaches it.
const ID_CEILING: u64 = 1 << 62;

/// The unit separator that joins detail lines and actions inside one value.
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
    /// The reporter withdrew it with no outcome to claim — a live meter
    /// whose source vanished before any marker said how the pass ended.
    Withdrawn,
    /// A RECORD: posted [`crate::model::Hold::LogOnly`], never on the glass
    /// (design §10.4.7). The line codec spells it `how=folded rec=1`, so an
    /// older build after a rollback reads it as the fold it wrote before.
    Recorded,
    /// ATERM QUIT while the row was still open (design ruling 267): a live
    /// row's work, or a held row's hold, was cut off by the process ending,
    /// not by its reporter. Its record keeps its words and its mark and
    /// says where the work was. An older build does not know the word and
    /// skips the line, so the record reads as the dead process's open row
    /// it did before.
    Quit,
}

impl Retired {
    /// The wire's `state=` word: `folded` … `resolved-ok` / `resolved-warn`.
    #[must_use]
    pub fn as_word(&self) -> &'static str {
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

/// One message as the log keeps it: the words at post, updated to the final
/// words when it retires (restatements are never logged, so the Retired
/// line carries them).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct LogRecord {
    /// The id.
    pub id: MessageId,
    /// The wall clock at ingress.
    pub stamp: WallStamp,
    /// The reporter family.
    pub tag: Tag,
    /// The severity.
    pub severity: Severity,
    /// The glyph.
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
    /// When it retired, THIS PROCESS ONLY (a record loaded from disk has
    /// `None` — the `appstatus` ledger stays process-scoped).
    pub retired_at: Option<Instant>,
    /// The last capsule pressed, and when.
    pub last_action: Option<(String, u64)>,
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
        }
    }

    /// `true` while unretired.
    #[must_use]
    pub fn is_live(&self) -> bool {
        self.state == LogState::Posted
    }

    /// The wire's record ([`Origin::wire_owned`]): it counts against the
    /// ring's [`WIRE_LOG_SHARE`].
    #[must_use]
    pub fn wire_owned(&self) -> bool {
        self.origin.wire_owned(self.key.as_deref())
    }

    /// How it retired, if it has.
    #[must_use]
    pub fn retired(&self) -> Option<&Retired> {
        match &self.state {
            LogState::Posted => None,
            LogState::Retired(how) => Some(how),
        }
    }

    /// Whether this RETIRED record still offers `intent` (the page's and the
    /// `messages` verb's one rule): never one that acted on the live row
    /// itself ([`Intent::ends_with_row`], ruling 265), and never the word the
    /// row was answered with (ruling 270: an upgrade row answered `Not today`
    /// drew `Not today` again as its Primary). A live row offers every
    /// intent it carries.
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
        /// The outcome's MARK when the retirement changed it (design ruling
        /// 265): the severity and glyph the record reads under from here —
        /// `✓` Success for delivered work, `⚠` Warn for work that stopped.
        /// Written as `sev=` and `glyph=`, which an older build ignores (it
        /// keeps the posted mark); `None` keeps the posted mark.
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
    /// This many pending lines were dropped before this one — the file is
    /// honest about a gap.
    Dropped {
        /// How many.
        count: u32,
    },
}

/// Which file a line lands in (design ruling 200). The wire's lines —
/// every line about a record [`LogRecord::wire_owned`] — rotate on their
/// own budget in their own file, so a script's flood turns over only its
/// own history on disk, never aterm's (the ring's share, ruling 194, is
/// the same rule in memory).
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
    pub fn id(&self) -> Option<MessageId> {
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
                // A record is spelled as the fold an older build wrote for it,
                // plus a key such a build ignores (log.rs's forward-compatible
                // reader): `how=recorded` would be an unknown word there, and
                // the line would be dropped.
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
        // Written last: a line cut anywhere before here has no terminator.
        field("end", "");
        out
    }

    /// One line back into a record; forward-compatible (unknown keys are
    /// ignored). Every word comes back clipped to the model's caps and
    /// control-free, empty detail lines dropped — what `post` would have
    /// made of it.
    ///
    /// # Errors
    /// [`CodecError`] for a line that is not `m1`, is over the size cap,
    /// names an unknown kind, lacks a required field, or has no `end=`
    /// terminator (it was cut short). The loader skips such a line and
    /// keeps reading.
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
        // The terminator is checked last so the error names a missing
        // required field first; a line cut inside its last VALUE has every
        // field and no terminator.
        if fields.get("end").is_none() {
            return Err(CodecError::Truncated);
        }
        Ok(decoded)
    }
}

/// A Retired line's optional mark: both `sev=` and `glyph=`, or none.
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
        .collect();
    let origin = Origin::parse(&fields.need("origin")?).ok_or(CodecError::BadField("origin"))?;
    Ok(LogLine::Posted(LogRecord {
        id: fields.id()?,
        stamp: WallStamp {
            unix_ms: fields.num("t")?,
        },
        tag,
        severity,
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
    }))
}

/// The `k=v` pairs of one line, unescaped; the first of a repeated key wins.
struct Fields(Vec<(String, String)>);

impl Fields {
    fn parse<'a>(tokens: impl Iterator<Item = &'a str>) -> Self {
        Self(
            tokens
                .filter_map(|t| t.split_once('='))
                .map(|(k, v)| (k.to_string(), unescape(v)))
                .collect(),
        )
    }

    fn get(&self, key: &str) -> Option<&str> {
        self.0
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

    /// The `how=` word, an answered capsule's label clipped like a title; a
    /// fold carrying `rec=1` is a record.
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

/// A loaded `detail=` value under the model's caps: the lines split at US,
/// each clipped to [`DETAIL_LINE_CAP`], empty ones dropped (as the builder
/// drops them), at most [`DETAIL_LINES_CAP`].
fn capped_detail(joined: &str) -> Vec<String> {
    split_us(joined)
        .iter()
        .map(|l| clip(l, DETAIL_LINE_CAP))
        .filter(|l| !l.is_empty())
        .take(DETAIL_LINES_CAP)
        .collect()
}

/// Lines joined by US. A US INSIDE a line would read back as a line break,
/// so it is dropped here — every stored line is control-stripped already,
/// so the codec is total without a second escape level.
fn join_us(lines: &[String]) -> String {
    let mut out = String::new();
    for (i, line) in lines.iter().enumerate() {
        if i > 0 {
            out.push(US);
        }
        out.extend(line.chars().filter(|c| *c != US));
    }
    out
}

fn split_us(s: &str) -> Vec<String> {
    if s.is_empty() {
        return Vec::new();
    }
    s.split(US).map(str::to_string).collect()
}

/// `\`→`\\`, TAB→`\t`, LF→`\n`, CR→`\r`, US→`\u`.
fn escape_into(out: &mut String, v: &str) {
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
fn unescape(v: &str) -> String {
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
    /// The outcome's mark, when the retirement changed it (ruling 265).
    pub(crate) mark: Option<(Severity, Glyph)>,
}

/// The bounded ring of records plus the lines waiting for the host to
/// append. Loaded by replaying the file's tail; written by draining.
#[derive(Clone, Debug, Default)]
pub struct MessageLog {
    ring: VecDeque<LogRecord>,
    next_id: u64,
    pending: VecDeque<(LogLine, Shelf)>,
    dropped: u32,
    /// REPLAY ONLY (ruling 270): a file id another writer posted again,
    /// mapped to the fresh ids its later records were loaded under, oldest
    /// first — so no two records in the ring share an id.
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

    /// Load one line from the file. `Posted` inserts as **retired Stale** —
    /// every line on disk came from a process that is gone, and a Posted
    /// with no Retired is exactly a row nobody folded; `Retired` and `Acted`
    /// merge into their record (a later Retired line overrides the Stale
    /// reading); every id below the ceiling raises `next_id`. The words
    /// arrive already clipped by the decoder.
    ///
    /// THE SAME ID, ANOTHER RECORD (6443feeba, ruling 270): two processes
    /// sharing the file mint ids from their own counters, so one number can
    /// name two records (measured on the owner's Mac: a long-running
    /// window's 15 after a test instance had posted and retired its own).
    /// A Posted that repeats the SAME post (its stamp, tag and title — the
    /// handoff can write one twice) is that record, deduped. Any other
    /// Posted under a number the ring already holds is another writer's and
    /// loads under a FRESH id, whether the first record is closed or still
    /// open, so Settings ▸ Messages never draws two entries under one key.
    /// A later `Retired` or `Acted` for the number goes to the open record
    /// whose title it names, else the newest open one, else the newest.
    pub fn replay(&mut self, line: LogLine) {
        if let Some(id) = line.id() {
            self.raise(id);
        }
        match line {
            LogLine::Posted(mut rec) => {
                let file_id = rec.id;
                let candidates = self.replay_candidates(file_id);
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
                if self.get(file_id).is_some() || !candidates.is_empty() {
                    let fresh = self.next_id();
                    self.raise(fresh);
                    self.minted_at_replay.insert(fresh.raw());
                    self.remapped.entry(file_id.raw()).or_default().push(fresh);
                    rec.id = fresh;
                }
                rec.state = LogState::Retired(Retired::Stale);
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

    /// Load a whole file's lines ([`Self::replay`] each, in the order given —
    /// the loader sorts them by id): first past EVERY id among them, so an id
    /// minted for a collision is above the file's own and never renumbers a
    /// record that comes later in the sort (ruling 270; live, day three's fix
    /// stage: a collision at 3 minted 4 and pushed the wire's 4…9 up by one).
    pub fn replay_all(&mut self, lines: impl IntoIterator<Item = LogLine>) {
        let lines: Vec<LogLine> = lines.into_iter().collect();
        if let Some(top) = lines.iter().filter_map(LogLine::id).max() {
            self.raise(top);
        }
        for line in lines {
            self.replay(line);
        }
    }

    /// The records loaded for file id `file_id`, oldest first: the ring's
    /// own record of that number (unless a remap minted it), then the ones
    /// remapped from it.
    fn replay_candidates(&self, file_id: MessageId) -> Vec<MessageId> {
        let own = (!self.minted_at_replay.contains(&file_id.raw()) && self.get(file_id).is_some())
            .then_some(file_id);
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

    /// Where a replayed line for file id `file_id` merges: the open record
    /// whose posted title `title` names — the title itself, or one of the
    /// final words the center derives from it ([`Self::final_words_of`]: a
    /// delivered row retires `Deployed site`, never `Deploying site`) —
    /// else the newest open one, else the newest.
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

    /// Whether `final_title` (a Retired line's words) can be how a row posted
    /// as `posted` ended: the posted title kept, or its finished, `— done`,
    /// stopped or ended form (`center::outcome_title`'s table forms). A
    /// reporter's own declared words are not derivable and fall through to
    /// the newest open record.
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

    /// Whether a replayed record was closed by a `Retired` line: its state
    /// is a retirement other than the Stale every Posted line loads as.
    fn closed_by_a_line(rec: &LogRecord) -> bool {
        matches!(&rec.state, LogState::Retired(how) if *how != Retired::Stale)
    }

    /// The records, oldest first.
    #[must_use]
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

    /// The lines waiting for the host, oldest first, left in place (the
    /// handoff carry reads them without draining).
    #[cfg(test)]
    pub(crate) fn pending_lines(&self) -> impl Iterator<Item = &LogLine> {
        self.pending.iter().map(|(line, _)| line)
    }

    /// Take every pending line, oldest first. When lines were dropped past
    /// [`PENDING_PERSIST_CAP`] since the last drain, a trailing
    /// [`LogLine::Dropped`] says how many.
    #[cfg(test)]
    pub(crate) fn drain_pending(&mut self) -> Vec<LogLine> {
        self.drain_shelved()
            .into_iter()
            .map(|(line, _)| line)
            .collect()
    }

    /// Take every pending line, oldest first, each with the file it lands in
    /// ([`Shelf`], decided when the line was queued). When lines were dropped
    /// past [`PENDING_PERSIST_CAP`] since the last drain, a trailing
    /// [`LogLine::Dropped`] says how many — the host's line: the gap is in
    /// aterm's record.
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

    // ---- the center's side --------------------------------------------

    /// Mint the next id.
    pub(crate) fn mint(&mut self) -> MessageId {
        let id = self.next_id();
        self.next_id = id.next().raw();
        id
    }

    /// Never mint at or below `id` again — unless `id` sits at the ceiling,
    /// where a corrupt line must not park the sequence.
    fn raise(&mut self, id: MessageId) {
        self.raise_to(id.next().raw());
    }

    /// Never mint below `raw` again (the carry ships the parent's next id);
    /// a `raw` at or past [`ID_CEILING`] is a forgery and is ignored.
    pub(crate) fn raise_to(&mut self, raw: u64) {
        if raw < ID_CEILING {
            self.next_id = self.next_id.max(raw);
        }
    }

    /// Into the ring; past [`LOG_CAP`] the oldest RETIRED record goes — a
    /// live row's record stays while the band paints it (the `messages`
    /// verb and the Settings page read it there; the Retired line merges its
    /// final words into it). [`crate::MAX_LIVE`] ≤ [`LOG_CAP`], so a retired
    /// record is always there to evict; the oldest of all goes only if the
    /// ring were somehow all live. The wire has a SHARE: once
    /// [`WIRE_LOG_SHARE`] records are the wire's
    /// ([`Origin::wire_owned`]), the oldest retired wire record goes first,
    /// so a script's records can never push aterm's own out (design ruling
    /// 194).
    fn push_record(&mut self, rec: LogRecord) {
        if self.ring.len() >= LOG_CAP {
            let wire_full = self.ring.iter().filter(|r| r.wire_owned()).count() >= WIRE_LOG_SHARE;
            let wire_victim = || {
                self.ring
                    .iter()
                    .position(|r| r.wire_owned() && !r.is_live())
            };
            let victim = wire_full
                .then(wire_victim)
                .flatten()
                .or_else(|| self.ring.iter().position(|r| !r.is_live()));
            match victim {
                Some(i) => {
                    self.ring.remove(i);
                }
                None => {
                    self.ring.pop_front();
                }
            }
        }
        self.ring.push_back(rec);
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

    /// A carried row's record, live again in this process: re-opened when
    /// the tail the successor loaded held the parent's own Posted line for
    /// it (its stamp and tag — the title may have been restated since), else
    /// inserted.
    ///
    /// ONE ID, ONE RECORD (ruling 270; round 17 review, V3): the carried id
    /// is the live row's and wins. A record of ANOTHER writer loaded under
    /// that number — a file id, or one [`Self::replay_all`] minted for a
    /// collision above the file's top while the parent's Posted line was
    /// not yet on disk — moves to a fresh id and keeps its own words, never
    /// re-opened under the carried row's. The parent's own record, if a
    /// collision loaded it under a minted id, takes its number back. The
    /// center raises past the carry's `next_id` first, so the fresh id is
    /// above every carried one.
    pub(crate) fn adopt_carried(&mut self, rec: LogRecord) {
        self.raise(rec.id);
        let mine = self.replay_candidates(rec.id).into_iter().find(|id| {
            self.get(*id)
                .is_some_and(|r| r.stamp == rec.stamp && r.tag == rec.tag)
        });
        if mine != Some(rec.id) && self.get(rec.id).is_some() {
            let fresh = self.mint();
            self.minted_at_replay.insert(fresh.raw());
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

    /// What the decoder hands back for `line`: every word clipped to the
    /// model's caps and control-free, empty detail lines dropped — the
    /// identity for anything the engine itself encodes.
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
        }
    }

    /// Invariant 19: encode/decode identity for every field the engine can
    /// hold; tabs, newlines, backslashes and US in a value ride the escapes
    /// without breaking the framing (and come back stripped, as `post`
    /// would have stripped them); a truncated line and an `m2` line are
    /// skipped; ids keep rising after replay; a dead process's unretired
    /// Posted reads back Stale.
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
        // The escapes carry the hostile characters without breaking the
        // framing: the fields AFTER the title decode intact…
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
                // …and the words come back as `post` would have made them:
                // controls stripped, the empty middle line dropped.
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
        // A US inside a detail line cannot read back as a line break: the
        // join drops it (stored lines are control-stripped anyway).
        let mut us_inside = posted(5);
        us_inside.detail = vec!["b\u{1f}c".into()];
        match LogLine::decode(&LogLine::Posted(us_inside).encode()).unwrap() {
            LogLine::Posted(rec) => assert_eq!(rec.detail, vec!["bc".to_string()]),
            other => panic!("{other:?}"),
        }
        // Junk: not m1, an m2 line, a truncated line, an unknown kind, an
        // oversize line.
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
        // Forward-compatible: unknown keys are ignored, a missing optional
        // field defaults, an unknown intent is dropped, an unknown glyph
        // falls back.
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

        // Replay: a dead process's Posted reads Stale; a Retired line merges
        // its final words; ids keep rising past the highest seen.
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

    /// TWO INSTANCES, ONE ID (the owner's `messages.log`, 2026-09-26): a
    /// test instance posted and retired its id 15; the long-running window,
    /// whose counter had not seen it, later posted and retired its own 15.
    /// Both records load, each with its own words. NEGATIVE CONTROL: a Posted
    /// repeated for a record still open (no Retired line yet) is the same
    /// record, deduped as before.
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

        // RULING 270: the first record still OPEN when another writer posts
        // its own 15 — both load, each under its own id, and each Retired
        // line merges into the record whose words it names, whichever
        // retires first.
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
        // A file whose own writer later reaches 16 does not collide with
        // the fresh id: it is remapped in turn.
        both.replay(LogLine::Posted(LogRecord {
            title: "Paste stopped".into(),
            ..posted(16)
        }));
        let ids: Vec<u64> = both.records().map(|r| r.id.raw()).collect();
        assert_eq!(ids, [15, 16, 17]);

        // The loader's whole-file replay mints above every id in the file,
        // so a later record keeps its own number (no cascade).
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

    /// RULING 270 (round 17 review, V2): a DELIVERED row retires under its
    /// finished words (`Deploying site` → `Deployed site`, or `— done`), so a
    /// Retired line is routed by the words the center derives from each open
    /// candidate's posted title, not by the posted title alone. When the
    /// OLDER of two colliding records retires first, its words stay on it and
    /// the other writer's later words land on the other record. NEGATIVE
    /// CONTROL: words no candidate's title can derive still fall back to the
    /// newest open record.
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
        // NEGATIVE CONTROL: words derived from no candidate go to the newest
        // open record, as before.
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
    fn the_pending_queue_is_bounded_and_honest_about_drops() {
        let mut log = MessageLog::empty();
        for _ in 0..(PENDING_PERSIST_CAP + 3) {
            let id = log.mint();
            log.record_posted(LogRecord { id, ..posted(1) });
        }
        assert_eq!(log.pending_len(), PENDING_PERSIST_CAP);
        assert_eq!(log.len(), LOG_CAP);
        let drained = log.drain_pending();
        assert_eq!(drained.len(), PENDING_PERSIST_CAP + 1);
        assert_eq!(drained.last(), Some(&LogLine::Dropped { count: 3 }));
        assert!(log.drain_pending().is_empty());
        assert_eq!(log.pending_len(), 0);
    }

    /// Design ruling 200: every line about a wire-owned record — its Posted,
    /// its Retired, its Acted — is shelved for the wire's file; aterm's own
    /// lines, and the honest `Dropped` gap, for `messages.log`.
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
        // Overflow: the gap line is the host's.
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

    /// Review (2026-09-24, design ruling 194): a script posting once a
    /// second must not push aterm's own history (the crash record, a config
    /// error) out of the ring. The wire's records past [`WIRE_LOG_SHARE`]
    /// evict each other; a host record goes only to another host record.
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
        // One host record, then 600 wire records.
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
        // A ring FULL of host records: the wire takes its share and no more.
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
        // A carried wire row's record (origin carried, a `wire.` key) is the
        // wire's too; a host key is not.
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

    /// REVIEW (2026-09-22): a message at the crate's OWN caps reads back
    /// from its own line. The caps are in CHARS (24 × 240 detail + 120
    /// title + 48 key); [`MAX_LINE_BYTES`] is in BYTES, so it is sized from
    /// the caps at four bytes a char (box-drawing, CJK and backslash-escaped
    /// text at the cap encode to 11–24 KiB) with room to spare — never
    /// `TooLong` for a line this encoder wrote.
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
        // The cap is derived from the model's caps, not chosen: four bytes a
        // char for every capped word, the escaped joints, and 8 KiB for the
        // fixed fields and two encoded intents, all within half the cap.
        let worst = 4 * (TITLE_CAP + DETAIL_LINES_CAP * DETAIL_LINE_CAP + KEY_CAP)
            + 2 * DETAIL_LINES_CAP
            + 8 * 1024;
        assert!(
            worst <= MAX_LINE_BYTES / 2,
            "{worst} > {}",
            MAX_LINE_BYTES / 2
        );
    }

    /// REVIEW (2026-09-22): a line cut short mid-append fails to decode
    /// wherever the cut lands — a cut INSIDE the last field's value used to
    /// read as a different record (`label=Open`, `rep=1` for 12, `n=1` for
    /// 12); the `end=` terminator catches it.
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

    /// REVIEW (2026-09-22): ids keep rising after replay — including past a
    /// replayed id at the ceiling. `raise` used to saturate `next_id` at
    /// `u64::MAX`, after which `mint` handed out `u64::MAX` forever: one
    /// corrupt or hand-edited line made every later post share one id. An
    /// id at or past [`ID_CEILING`] is kept as a record but never raises the
    /// sequence, and a forged carry's `next_id` there is ignored the same.
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

    /// REVIEW (2026-09-22): words the loader takes from the file are not
    /// re-capped or re-sanitized — a hand-edited or hostile line puts a
    /// 5000-char, ESC-bearing, newline-bearing title in the ring, past every
    /// cap `post` enforces; `copy_text` and the page then carry it.
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

    /// REVIEW (2026-09-22): `detail=` on the wire is both `[]` and `[""]`,
    /// so a record with ONE empty detail line used to read back with none
    /// and a compaction was not the identity for it. Now `[""]` is
    /// unrepresentable: the builder drops empty lines, and so does the
    /// decoder, so no message the engine holds is ambiguous on the wire.
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
        // A record literal that bypasses the builder is read as the builder
        // would have built it.
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
