// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! The wire grammars: the `messages` read query and its row, the
//! byte-identical `appstatus` compatibility face, and the `notice` verb —
//! `post`, `progress`, `done`, `dismiss`, `act` — with its verdicts, caps,
//! budgets, key namespace and paint pacing (design rulings 163–198). The
//! percent-encoder is INJECTED (`aterm_control::wire::pct_encode` in the
//! host) — the grammar lives beside the state, the bytes stay the host's,
//! and no second encoder can drift. The host parses on its control thread
//! ([`NoticeRequest::parse`]), wakes its main thread, calls [`apply`] (or,
//! for `act`, [`press_target`] and its own press) and paints by the returned
//! [`Paint`]: every word of the reply is decided here.

use std::collections::VecDeque;

use crate::center::{Live, MessageCenter, Outcome, PostOutcome};
use crate::log::{LogRecord, MessageLog, Retired};
use crate::model::{
    ActionIndex, Amount, Hold, Intent, Load, Message, MessageId, Meter, Origin, Restatement,
    Severity, Tag, Unit, WallStamp, tags,
};
use crate::text::{clip, glass_title_fault};
use crate::{
    DETAIL_LINE_CAP, DETAIL_LINES_CAP, Duration, Instant, LOG_CAP, MAX_ACTIONS, PROGRESS_GRACE,
    STALE_WIRE, STATS_CAP, TITLE_CAP, WIRE_BYTES_PER_WINDOW, WIRE_KEY_MAX, WIRE_KEY_PREFIX,
    WIRE_LIVE_CAP, WIRE_MINT_WINDOW, WIRE_MINTS_PER_WINDOW, WIRE_PAINT_GAP,
    WIRE_PRESSES_PER_WINDOW,
};

/// The `messages` verb's usage line.
pub const READ_USAGE: &str = "usage: messages [<n>] [since=<id>] [tag=<tag>] [sev=<sev>] [live]";
/// The `notice post` usage line.
pub const POST_USAGE: &str =
    "usage: notice post <tag> [sev=<sev>] [key=<key>] [hold=<1..3600>] <title>[ -- <detail>]";
/// The longest `hold=` a wire post may ask for, in seconds.
pub const MAX_WIRE_HOLD_SECS: u32 = 3600;
/// The most detail bytes a wire post carries.
pub const WIRE_DETAIL_BYTES: usize = 2048;
/// The default page of a `messages` read.
pub const DEFAULT_READ_N: usize = 64;

/// `messages [<n>] [since=<id>] [tag=<tag>] [sev=<sev>] [live]`.
#[derive(Clone, Debug, PartialEq, Eq, Default)]
pub struct ReadQuery {
    /// The page size as given (at most [`LOG_CAP`]); `None` is the default
    /// for the query's shape ([`ReadQuery::page`]).
    pub n: Option<usize>,
    /// Only ids strictly greater.
    pub since: Option<MessageId>,
    /// Only this tag.
    pub tag: Option<Tag>,
    /// A floor: `sev=warn` is warn + error.
    pub min_severity: Option<Severity>,
    /// Only unretired rows.
    pub live_only: bool,
}

impl ReadQuery {
    /// How many rows a page holds: `n` when given; else, with `since=`,
    /// [`LOG_CAP`] (everything above the id), and without it
    /// [`DEFAULT_READ_N`].
    #[must_use]
    pub fn page(&self) -> usize {
        self.n.unwrap_or(if self.since.is_some() {
            LOG_CAP
        } else {
            DEFAULT_READ_N
        })
    }

    /// The records the query selects, ascending by id. Without `since=` the
    /// page is the NEWEST ones (the tail of the log); with it, the OLDEST
    /// ones above the id, so a reader paging with the last id it saw never
    /// skips a record, however many arrived between two reads (design
    /// ruling 198 — the `exits since=` idiom).
    #[must_use]
    pub fn select<'a>(&self, log: &'a MessageLog) -> Vec<&'a LogRecord> {
        let picked = log
            .records()
            .filter(|r| self.since.is_none_or(|s| r.id > s))
            .filter(|r| self.tag.as_ref().is_none_or(|t| r.tag == *t))
            .filter(|r| self.min_severity.is_none_or(|s| r.severity >= s))
            .filter(|r| !self.live_only || r.is_live());
        if self.since.is_some() {
            return picked.take(self.page()).collect();
        }
        let picked: Vec<&LogRecord> = picked.collect();
        let skip = picked.len().saturating_sub(self.page());
        picked.into_iter().skip(skip).collect()
    }
}

/// Parse the words after `messages`; junk ⇒ `Err(READ_USAGE)`.
///
/// # Errors
/// [`READ_USAGE`] for any token the grammar does not name, a zero count,
/// a bad id, tag or severity.
pub fn parse_read_args(rest: &str) -> Result<ReadQuery, &'static str> {
    let mut q = ReadQuery::default();
    for word in rest.split_whitespace() {
        if word == "live" {
            q.live_only = true;
        } else if let Ok(n) = word.parse::<usize>() {
            if n == 0 {
                return Err(READ_USAGE);
            }
            q.n = Some(n.min(LOG_CAP));
        } else if let Some(v) = word.strip_prefix("since=") {
            q.since = Some(
                v.parse()
                    .ok()
                    .and_then(MessageId::from_raw)
                    .ok_or(READ_USAGE)?,
            );
        } else if let Some(v) = word.strip_prefix("tag=") {
            q.tag = Some(Tag::try_new(v).map_err(|_| READ_USAGE)?);
        } else if let Some(v) = word.strip_prefix("sev=") {
            q.min_severity = Some(Severity::parse(v).ok_or(READ_USAGE)?);
        } else {
            return Err(READ_USAGE);
        }
    }
    Ok(q)
}

/// The `appstatus` progress field for a fill: `<n>/100` rounded to the
/// nearest percent (`status_bars.rs:975`), `-` for none.
fn progress_word(fill: Option<u16>) -> String {
    fill.map_or_else(
        || "-".to_string(),
        |p| format!("{}/100", (p.min(1000) + 5) / 10),
    )
}

/// The `state=` word of a record: `held` / `live` while unretired (a held
/// row folds on its hold; a live one stays until its reporter is done), else
/// how it retired ([`crate::log::Retired::as_word`]). ONE table — the `messages` row
/// and Settings ▸ Messages both read it, so the screen and the wire can
/// never disagree about what a row is doing.
#[must_use]
pub fn state_word(rec: &LogRecord, live: Option<&Live>) -> &'static str {
    match (live, rec.retired()) {
        (Some(l), _) => {
            if l.msg.hold.is_held() {
                "held"
            } else {
                "live"
            }
        }
        (None, Some(how)) => how.as_word(),
        (None, None) => "live",
    }
}

/// One `messages` row:
/// `message <id> at=<unix_ms> ago_ms=<ms> tag=<tag> sev=<sev>
/// origin=<host|wire|carried> state=<…> glass=<row|-> rep=<n> key=<pct|->
/// title=<pct> detail=<pct> actions=<pct|-> [progress=<n>/100|level=<n>/100|busy=1]
/// [load=<network|disk|cpu|memory>] [since_ms=<ms>]`. `at=` is the wall
/// clock at ingress; `ago_ms=` and `since_ms=` are both read off the ONE
/// `now_unix_ms` of the reply — the ingress stamp and the retirement's wall
/// time (the wall at posting — the seed's, for a carried row — plus the
/// row's monotonic life, [`Live::wall_at`], ruling 204) — so
/// `since_ms <= ago_ms` always (design ruling 201); `since_ms=` is present
/// only for rows retired in this process; `detail=` is the lines joined by `\n` before
/// encoding; `actions=` the FULL capsule labels joined by `,`. A live row
/// with a fill says `progress=` (a measured level, the strain row, says
/// `level=` instead), a busy one `busy=1`, and `load=` only while
/// the row SHOWS its load words; no ETA, elapsed or frame state ever reaches
/// the line — motion is the glass's (design ruling 175).
#[must_use]
pub fn message_row(
    rec: &LogRecord,
    live: Option<&Live>,
    glass_row: Option<usize>,
    now_unix_ms: u64,
    enc: &dyn Fn(&str) -> String,
) -> String {
    let state = state_word(rec, live);
    let (title, detail, actions, repeats) = match live {
        Some(l) => (&l.msg.title, &l.msg.detail, &l.msg.actions, l.repeats),
        None => (&rec.title, &rec.detail, &rec.actions, rec.repeats),
    };
    let labels: Vec<&str> = actions.iter().map(Intent::label).collect();
    let ago_ms = now_unix_ms.saturating_sub(rec.stamp.unix_ms);
    let mut row = format!(
        "message {} at={} ago_ms={ago_ms} tag={} sev={} origin={} state={state} glass={} rep={repeats} key={} title={} detail={} actions={}",
        rec.id,
        rec.stamp.unix_ms,
        rec.tag,
        rec.severity.as_str(),
        rec.origin.as_str(),
        glass_row.map_or_else(|| "-".to_string(), |r| r.to_string()),
        rec.key.as_deref().map_or_else(|| "-".to_string(), enc),
        enc(title),
        enc(&detail.join("\n")),
        if labels.is_empty() {
            "-".to_string()
        } else {
            enc(&labels.join(","))
        },
    );
    if let Some(l) = live {
        if let Some(fill) = l
            .msg
            .meter
            .as_ref()
            .filter(|m| m.level)
            .and_then(|m| m.fill_permille)
        {
            // A measured level is not progress (design ruling 208).
            row.push_str(" level=");
            row.push_str(&progress_word(Some(fill)));
        } else if let Some(fill) = l.msg.meter.as_ref().and_then(|m| m.fill_permille) {
            row.push_str(" progress=");
            row.push_str(&progress_word(Some(fill)));
        } else if l.is_busy() {
            row.push_str(" busy=1");
        }
        if let Some(load) = l.shown_load() {
            row.push_str(" load=");
            row.push_str(load.word());
        }
    }
    if let (Some(_), Some(retired_ms)) = (rec.retired_at, rec.retired_unix_ms) {
        row.push_str(" since_ms=");
        row.push_str(
            &now_unix_ms
                .saturating_sub(retired_ms)
                .min(ago_ms)
                .to_string(),
        );
    }
    row
}

/// Every `messages` row the query selects, ascending by id, in
/// [`message_row`]'s grammar: `glass=` is the row's visual position while it
/// is on the glass, else `-`. Built only here, so a row is unforgeable: one
/// line per record, no `\n`, the fixed fields in their fixed order, every
/// free field through `enc`.
#[must_use]
pub fn message_rows(
    center: &MessageCenter,
    q: &ReadQuery,
    now_unix_ms: u64,
    enc: &dyn Fn(&str) -> String,
) -> Vec<String> {
    q.select(center.log())
        .into_iter()
        .map(|rec| {
            let live = center.live(rec.id);
            let glass = live.and_then(|_| center.glass_position(rec.id));
            message_row(rec, live, glass, now_unix_ms, enc)
        })
        .collect()
}

/// The `appstatus` body in the bars' byte-identical GRAMMAR
/// (status_bars.rs:971-999 — field order, keys, percent-encoding, the phase
/// and outcome words; a `phase=live` line carries the row's CURRENT words,
/// design ruling 116): one
/// `activity kind=<toolchain|update|harness> phase=live …` per unretired
/// message with those tags — **toolchain-tagged rows first, then
/// update-tagged, then harness-tagged, each `posted_at` ascending, whatever
/// the glass order** (a harness note is a record, so it is only ever
/// `phase=done`; design rulings 61 and 147) — then one
/// `phase=done … outcome=<ok|warn> since_ms=<ms>` per record those tags
/// retired IN THIS PROCESS (a superseded row is not a finished outcome,
/// and neither is a withdrawn one — the bars dropped a meter whose file
/// vanished without a ledger row), oldest first. `outcome=warn` ⇔ severity
/// Warn/Error.
#[must_use]
pub fn activity_rows_compat(
    center: &MessageCenter,
    now: Instant,
    enc: &dyn Fn(&str) -> String,
) -> Vec<String> {
    let lanes = ["toolchain", "update", "harness"];
    let mut rows = Vec::new();
    for lane in lanes {
        let mut live: Vec<&Live> = center
            .live_rows()
            .filter(|l| l.msg.tag.as_str() == lane)
            .collect();
        live.sort_by_key(|l| (l.posted_at, l.id));
        for l in live {
            rows.push(format!(
                "activity kind={lane} phase=live progress={} title={} detail={} stats={} outcome=-",
                progress_word(l.msg.meter.as_ref().and_then(|m| m.fill_permille)),
                enc(&l.msg.title),
                enc(l.msg.detail.first().map_or("", String::as_str)),
                enc(l.msg.meter.as_ref().map_or("", |m| m.stats.as_str())),
            ));
        }
    }
    let mut done: Vec<(&LogRecord, Instant)> = center
        .log()
        .records()
        .filter(|r| lanes.contains(&r.tag.as_str()))
        .filter(|r| {
            !matches!(
                r.retired(),
                None | Some(
                    crate::log::Retired::Superseded { .. } | crate::log::Retired::Withdrawn
                )
            )
        })
        .filter_map(|r| r.retired_at.map(|at| (r, at)))
        .collect();
    done.sort_by_key(|(r, at)| (*at, r.id));
    for (r, finished) in done {
        let outcome = if r.severity >= Severity::Warn {
            "warn"
        } else {
            "ok"
        };
        rows.push(format!(
            "activity kind={} phase=done progress=- title={} detail={} stats= outcome={outcome} since_ms={}",
            r.tag,
            enc(&r.title),
            enc(r.detail.first().map_or("", String::as_str)),
            now.saturating_duration_since(finished).as_millis(),
        ));
    }
    rows
}

// ---------------------------------------------------------------------------
// `notice` — the outside world's voice on the band (design rulings 163–198).
// ---------------------------------------------------------------------------

/// The `notice` verb's usage line: a missing or unknown sub-form.
pub const NOTICE_USAGE: &str = "usage: notice <post|progress|done|dismiss|act> \u{2026}";
/// The `notice progress` usage line.
pub const PROGRESS_USAGE: &str = "usage: notice progress <key> [tag=<tag>] [pct=<0..100>|done=<n>/<total>|busy] [unit=<bytes|items|steps>] [load=<network|disk|cpu>] <title>[ -- <stats>]";
/// The `notice done` usage line.
pub const DONE_USAGE: &str = "usage: notice done <key> [ok|warn|withdraw] [<words>]";
/// The `notice dismiss` usage line.
pub const DISMISS_USAGE: &str = "usage: notice dismiss <id>";
/// The `notice act` usage line.
pub const ACT_USAGE: &str = "usage: notice act <id> <label|index|details>";
/// A key outside the wire's rule (design ruling 167).
pub const KEY_REFUSAL: &str = "notice: keys are 1-40 of a-z 0-9 . _ - (they live as wire.<key>)";
/// A tag of aterm's own lanes (design ruling 171).
pub const LANE_REFUSAL: &str = "notice: the toolchain, update and harness tags are aterm's own";
/// [`apply`] handed an `act`: a press is the HOST's to perform
/// ([`press_target`], then its own press path).
pub const ACT_IS_THE_HOSTS: &str = "ERR notice: act is pressed by the host";

/// The tags `notice` refuses: `appstatus` is what aterm has been doing on its
/// own initiative, so a script can never appear there as aterm's install or
/// update (design ruling 171).
const LANE_TAGS: [&str; 3] = ["toolchain", "update", "harness"];

/// The refusal for a glass title fault: `notice: <fault>`, one static string
/// per fault [`glass_title_fault`] names.
fn title_refusal(fault: &'static str) -> &'static str {
    match fault {
        "a glass title over six words" => "notice: a glass title over six words",
        "a glass title over 48 characters" => "notice: a glass title over 48 characters",
        "a clause in a glass title" => "notice: a clause in a glass title",
        "a sentence for a glass title" => "notice: a sentence for a glass title",
        _ => "notice: a glass title out of form",
    }
}

/// `Err(refusal)` when `title` may not stand on the glass.
fn title_form(title: &str) -> Result<(), &'static str> {
    glass_title_fault(title).map_or(Ok(()), |f| Err(title_refusal(f)))
}

/// The key rule: `[a-z0-9._-]{1,40}`, stored as `wire.<key>`.
fn wire_key(word: &str) -> Result<String, &'static str> {
    let ok = !word.is_empty()
        && word.len() <= WIRE_KEY_MAX
        && word.bytes().all(|b| {
            b.is_ascii_lowercase() || b.is_ascii_digit() || matches!(b, b'.' | b'_' | b'-')
        });
    if ok {
        Ok(format!("{WIRE_KEY_PREFIX}{word}"))
    } else {
        Err(KEY_REFUSAL)
    }
}

/// The tag rule: any [`Tag::try_new`] word but aterm's own lanes.
fn wire_tag(word: &str, usage: &'static str) -> Result<Tag, &'static str> {
    let tag = Tag::try_new(word).map_err(|_| usage)?;
    if LANE_TAGS.contains(&tag.as_str()) {
        return Err(LANE_REFUSAL);
    }
    Ok(tag)
}

/// The first whitespace-separated word of `s` and the rest after it.
fn first_word(s: &str) -> (&str, &str) {
    let s = s.trim_start();
    s.split_once(char::is_whitespace).unwrap_or((s, ""))
}

/// A plain decimal number: digits only (no sign, no space), in range.
fn digits<T: std::str::FromStr>(s: &str) -> Option<T> {
    (!s.is_empty() && s.bytes().all(|b| b.is_ascii_digit()))
        .then(|| s.parse().ok())
        .flatten()
}

/// A message id on the wire: digits, not zero.
fn wire_id(s: &str) -> Option<MessageId> {
    digits::<u64>(s).and_then(MessageId::from_raw)
}

/// `<title>[ -- <tail>]`, both trimmed; a line that opens with `--` has no
/// title.
fn split_title(rest: &str) -> (&str, &str) {
    let rest = rest.trim();
    if rest == "--" {
        return ("", "");
    }
    if let Some(tail) = rest.strip_prefix("-- ") {
        return ("", tail.trim());
    }
    let (title, tail) = rest.split_once(" -- ").unwrap_or((rest, ""));
    (title.trim(), tail.trim())
}

/// Options come in any order after the fixed words until the first word
/// that is none of them: `take` names the options it knows (`Ok(true)`),
/// refuses a bad one (`Err`), or ends the run (`Ok(false)`). Returns what is
/// left: the title and its tail.
fn options(
    mut rest: &str,
    mut take: impl FnMut(&str) -> Result<bool, &'static str>,
) -> Result<&str, &'static str> {
    loop {
        rest = rest.trim_start();
        let word = rest.split_whitespace().next().unwrap_or("");
        if word.is_empty() || !take(word)? {
            return Ok(rest);
        }
        rest = &rest[word.len()..];
    }
}

/// `notice post <tag> [sev=<sev>] [key=<key>] [hold=<1..3600>] <title>[ -- <detail>]`.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct PostRequest {
    /// Any tag but aterm's own lanes (design ruling 171).
    pub tag: Tag,
    /// Defaults to `info`.
    pub severity: Severity,
    /// The FULL key, `wire.<key>` (design ruling 167).
    pub key: Option<String>,
    /// Seconds, 1..=3600; the severity's hold when absent. A record has no
    /// hold, so on `success`/`info` it is accepted and ignored.
    pub hold_secs: Option<u32>,
    /// ≤ [`TITLE_CAP`] chars; a `warn`/`error` title obeys the glass title
    /// form ([`glass_title_fault`]).
    pub title: String,
    /// ≤ [`WIRE_DETAIL_BYTES`], split on a literal `\n`, each line clipped
    /// to [`DETAIL_LINE_CAP`], ≤ [`DETAIL_LINES_CAP`] lines.
    pub detail: Vec<String>,
}

impl PostRequest {
    /// Parse the words after `notice post`.
    ///
    /// # Errors
    /// [`POST_USAGE`] for a missing or invalid tag, an unknown severity, a
    /// hold outside `1..=3600`, or an empty title; [`LANE_REFUSAL`] for
    /// aterm's own tags; [`KEY_REFUSAL`] for a key outside the rule; and
    /// `notice: <fault>` for a `warn`/`error` title the glass refuses.
    pub fn parse(rest: &str) -> Result<Self, &'static str> {
        let (tag, rest) = first_word(rest);
        if tag.is_empty() || rest.trim().is_empty() {
            return Err(POST_USAGE);
        }
        let tag = wire_tag(tag, POST_USAGE)?;
        let mut severity = Severity::Info;
        let mut key = None;
        let mut hold_secs = None;
        let rest = options(rest, |word| {
            if let Some(v) = word.strip_prefix("sev=") {
                severity = Severity::parse(v).ok_or(POST_USAGE)?;
            } else if let Some(v) = word.strip_prefix("key=") {
                key = Some(wire_key(v)?);
            } else if let Some(v) = word.strip_prefix("hold=") {
                let secs: u32 = digits(v).ok_or(POST_USAGE)?;
                if secs == 0 || secs > MAX_WIRE_HOLD_SECS {
                    return Err(POST_USAGE);
                }
                hold_secs = Some(secs);
            } else {
                return Ok(false);
            }
            Ok(true)
        })?;
        let (title, detail) = split_title(rest);
        let title = clip(title, TITLE_CAP);
        if title.is_empty() {
            return Err(POST_USAGE);
        }
        if severity >= Severity::Warn {
            title_form(&title)?;
        }
        let mut end = detail.len().min(WIRE_DETAIL_BYTES);
        while !detail.is_char_boundary(end) {
            end -= 1;
        }
        let detail: Vec<String> = detail[..end]
            .split("\\n")
            .map(|l| clip(l.trim(), DETAIL_LINE_CAP))
            .filter(|l| !l.is_empty())
            .take(DETAIL_LINES_CAP)
            .collect();
        Ok(Self {
            tag,
            severity,
            key,
            hold_secs,
            title,
            detail,
        })
    }

    /// `true` for a RECORD: `success` and `info` go to the log, never the
    /// glass — the owner's *"'FYI CYA' bullshit messages need to go to the
    /// log and not interrupt the user"* (design ruling 163).
    #[must_use]
    pub fn is_record(&self) -> bool {
        self.severity <= Severity::Info
    }

    /// The message: `Origin::Wire`, the severity's glyph, NO actions ever
    /// (a socket client cannot mint an owner gesture, ruling 172). A record
    /// is [`Hold::LogOnly`] whatever `hold=` said; a row holds
    /// [`Hold::Default`] or `For(hold=)`.
    #[must_use]
    pub fn into_message(self) -> Message {
        let hold = if self.is_record() {
            Hold::LogOnly
        } else {
            self.hold_secs.map_or(Hold::Default, |secs| {
                Hold::For(Duration::from_secs(u64::from(secs)))
            })
        };
        let mut msg = Message::new(self.tag, self.severity, self.title)
            .lines(self.detail)
            .origin(Origin::Wire)
            .hold(hold);
        if let Some(key) = &self.key {
            msg = msg.key(key);
        }
        msg
    }
}

/// What `notice progress` shows.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum WireIndicator {
    /// `pct=`: a fill, in permille.
    Fill(u16),
    /// `done=<n>/<total>` (+ `unit=`): a fill with an amount, so an ETA.
    Amount {
        /// How much is done (clamped to `total` by [`Meter::normalized`]).
        done: u64,
        /// How much there is; at least 1.
        total: u64,
        /// What they count (`items` by default).
        unit: Unit,
    },
    /// `busy`, or no indicator word: the comet.
    Busy,
}

/// `pct=<0..100>`: an integer, or one decimal place (`42.5`), in permille.
fn parse_pct(v: &str) -> Option<u16> {
    let (int, frac) = match v.split_once('.') {
        Some((int, frac)) if frac.len() == 1 => (int, frac),
        Some(_) => return None,
        None => (v, "0"),
    };
    if int.len() > 3 {
        return None;
    }
    let p = digits::<u16>(int)? * 10 + digits::<u16>(frac)?;
    (p <= 1000).then_some(p)
}

/// `done=<n>/<total>`, `total ≥ 1`.
fn parse_done(v: &str) -> Option<(u64, u64)> {
    let (n, total) = v.split_once('/')?;
    let (n, total) = (digits::<u64>(n)?, digits::<u64>(total)?);
    (total >= 1).then_some((n, total))
}

/// `notice progress <key> [tag=<tag>] [pct=<0..100>|done=<n>/<total>|busy]
/// [unit=<bytes|items|steps>] [load=<network|disk|cpu>] <title>[ -- <stats>]`.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ProgressRequest {
    /// The FULL key, `wire.<key>`.
    pub key: String,
    /// `system` by default; never aterm's own lanes.
    pub tag: Tag,
    /// The fill, the amount, or busy.
    pub indicator: WireIndicator,
    /// A very heavy phase's resource (design §10.6).
    pub load: Option<Load>,
    /// The glass title form, always.
    pub title: String,
    /// ≤ [`STATS_CAP`] chars.
    pub stats: String,
}

impl ProgressRequest {
    /// Parse the words after `notice progress`.
    ///
    /// # Errors
    /// [`PROGRESS_USAGE`] for a missing key, a bad option, two indicators,
    /// `unit=` without `done=`, or an empty title; [`KEY_REFUSAL`],
    /// [`LANE_REFUSAL`], and `notice: <fault>` for a title out of form.
    pub fn parse(rest: &str) -> Result<Self, &'static str> {
        let (key, rest) = first_word(rest);
        if key.is_empty() {
            return Err(PROGRESS_USAGE);
        }
        let key = wire_key(key)?;
        let mut tag = tags::SYSTEM;
        let mut indicator = None;
        let mut unit = None;
        let mut load = None;
        let rest = options(rest, |word| {
            let mut set = |i: WireIndicator| match indicator.replace(i) {
                None => Ok(true),
                Some(_) => Err(PROGRESS_USAGE),
            };
            if let Some(v) = word.strip_prefix("tag=") {
                tag = wire_tag(v, PROGRESS_USAGE)?;
            } else if let Some(v) = word.strip_prefix("pct=") {
                return set(WireIndicator::Fill(parse_pct(v).ok_or(PROGRESS_USAGE)?));
            } else if let Some(v) = word.strip_prefix("done=") {
                let (done, total) = parse_done(v).ok_or(PROGRESS_USAGE)?;
                return set(WireIndicator::Amount {
                    done,
                    total,
                    unit: Unit::Items,
                });
            } else if word == "busy" {
                return set(WireIndicator::Busy);
            } else if let Some(v) = word.strip_prefix("unit=") {
                unit = Some(Unit::parse(v).ok_or(PROGRESS_USAGE)?);
            } else if let Some(v) = word.strip_prefix("load=") {
                load = Some(Load::parse_wire(v).ok_or(PROGRESS_USAGE)?);
            } else {
                return Ok(false);
            }
            Ok(true)
        })?;
        let indicator = match (indicator.unwrap_or(WireIndicator::Busy), unit) {
            (WireIndicator::Amount { done, total, .. }, unit) => WireIndicator::Amount {
                done,
                total,
                unit: unit.unwrap_or(Unit::Items),
            },
            (other, None) => other,
            (_, Some(_)) => return Err(PROGRESS_USAGE),
        };
        let (title, stats) = split_title(rest);
        let title = clip(title, TITLE_CAP);
        if title.is_empty() {
            return Err(PROGRESS_USAGE);
        }
        title_form(&title)?;
        Ok(Self {
            key,
            tag,
            indicator,
            load,
            title,
            stats: clip(stats, STATS_CAP),
        })
    }

    /// The meter the line declares, normalized: a fill, an amount in the
    /// key's own series ([`Amount::series_of`] the full key), or busy — with
    /// the stats and the load.
    #[must_use]
    pub fn meter(&self) -> Meter {
        let m = match self.indicator {
            WireIndicator::Fill(p) => Meter {
                fill_permille: Some(p),
                ..Meter::default()
            },
            WireIndicator::Amount { done, total, unit } => Meter {
                amount: Some(Amount {
                    series: Amount::series_of(&self.key),
                    done,
                    total,
                    unit,
                }),
                ..Meter::default()
            },
            WireIndicator::Busy => Meter {
                busy: true,
                ..Meter::default()
            },
        };
        Meter {
            stats: self.stats.clone(),
            load: self.load,
            ..m
        }
        .normalized()
    }

    /// A NEW row (design ruling 165): Info, `Origin::Wire`, keyed,
    /// `Hold::Live { STALE_WIRE }`, revealed only after [`PROGRESS_GRACE`]
    /// (a job done inside it never flashes), with its meter; no actions.
    #[must_use]
    pub fn into_message(self) -> Message {
        let meter = self.meter();
        Message::new(self.tag, Severity::Info, self.title)
            .key(&self.key)
            .origin(Origin::Wire)
            .hold(Hold::Live {
                stale_after: STALE_WIRE,
            })
            .reveal_after(PROGRESS_GRACE)
            .meter(meter)
    }

    /// The in-place change a later line makes: the title and the meter,
    /// nothing else — tag, severity, hold and actions never.
    #[must_use]
    pub fn restatement(&self) -> Restatement {
        Restatement {
            title: Some(self.title.clone()),
            meter: Some(Some(self.meter())),
            ..Restatement::default()
        }
    }
}

/// How `notice done` ends a row.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum DoneHow {
    /// Delivered: `resolve(Ok)`, the Complete echo.
    Ok,
    /// Failed: `resolve(Warn)`, the Fault echo.
    Warn,
    /// No outcome to claim: `withdraw`, the Vanish echo.
    Withdraw,
}

impl DoneHow {
    /// The retirement it records ([`Retired::as_word`] is the reply's word).
    #[must_use]
    pub fn retired(self) -> Retired {
        match self {
            Self::Ok => Retired::Resolved(Outcome::Ok),
            Self::Warn => Retired::Resolved(Outcome::Warn),
            Self::Withdraw => Retired::Withdrawn,
        }
    }
}

/// `notice done <key> [ok|warn|withdraw] [<words>]`.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct DoneRequest {
    /// The FULL key, `wire.<key>`.
    pub key: String,
    /// `ok` by default.
    pub how: DoneHow,
    /// The finishing words (the glass title form), re-titling the row first.
    pub words: Option<String>,
}

impl DoneRequest {
    /// Parse the words after `notice done`. Words need the outcome before
    /// them: `done build Built aterm` is refused, `done build ok Built aterm`
    /// is not.
    ///
    /// # Errors
    /// [`DONE_USAGE`] for a missing key or an unknown outcome word;
    /// [`KEY_REFUSAL`]; `notice: <fault>` for words out of the title form.
    pub fn parse(rest: &str) -> Result<Self, &'static str> {
        let (key, rest) = first_word(rest);
        if key.is_empty() {
            return Err(DONE_USAGE);
        }
        let key = wire_key(key)?;
        let (how, words) = first_word(rest);
        let how = match how {
            "" | "ok" => DoneHow::Ok,
            "warn" => DoneHow::Warn,
            "withdraw" => DoneHow::Withdraw,
            _ => return Err(DONE_USAGE),
        };
        let words = clip(words.trim(), TITLE_CAP);
        if !words.is_empty() {
            title_form(&words)?;
        }
        Ok(Self {
            key,
            how,
            words: (!words.is_empty()).then_some(words),
        })
    }
}

/// Which capsule `notice act` presses.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Press {
    /// An authored capsule by index, `0..MAX_ACTIONS`.
    Index(u8),
    /// An authored capsule by its FULL label ([`Intent::label`], exact).
    Label(String),
    /// The implicit `Details ›`.
    Details,
}

/// `notice <post|progress|done|dismiss|act> …`, parsed on the control thread.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum NoticeRequest {
    /// A record, or a failure row.
    Post(PostRequest),
    /// A script's work in flight.
    Progress(ProgressRequest),
    /// Its finish.
    Done(DoneRequest),
    /// Take a row down as a person could.
    Dismiss(MessageId),
    /// Press a row's capsule as a person would.
    Act {
        /// The row.
        id: MessageId,
        /// The capsule.
        press: Press,
    },
}

impl NoticeRequest {
    /// The words after `notice`; `Err` is the refusal text after `ERR `.
    /// Every usage, key, tag and title refusal is answered here, before any
    /// wake (design ruling 177).
    ///
    /// # Errors
    /// [`NOTICE_USAGE`] for a missing or unknown sub-form; each sub-form's
    /// own refusals ([`PostRequest::parse`], [`ProgressRequest::parse`],
    /// [`DoneRequest::parse`], [`DISMISS_USAGE`], [`ACT_USAGE`]).
    pub fn parse(rest: &str) -> Result<Self, &'static str> {
        // A tab (or any whitespace control) on the line is a space: the strip
        // below would otherwise glue `Deploy\tfailed` into `Deployfailed`, and
        // a `\t--\t` would never split the detail off (design ruling 189).
        let spaced: String = rest
            .chars()
            .map(|c| {
                if c.is_whitespace() && c.is_control() {
                    ' '
                } else {
                    c
                }
            })
            .collect();
        let (form, rest) = first_word(&spaced);
        match form {
            "post" => PostRequest::parse(rest).map(Self::Post),
            "progress" => ProgressRequest::parse(rest).map(Self::Progress),
            "done" => DoneRequest::parse(rest).map(Self::Done),
            "dismiss" => {
                let mut words = rest.split_whitespace();
                let id = words.next().and_then(wire_id).ok_or(DISMISS_USAGE)?;
                match words.next() {
                    None => Ok(Self::Dismiss(id)),
                    Some(_) => Err(DISMISS_USAGE),
                }
            }
            "act" => {
                let (id, press) = first_word(rest);
                let id = wire_id(id).ok_or(ACT_USAGE)?;
                let press = press.trim();
                let press = if press == "details" {
                    Press::Details
                } else if press.bytes().all(|b| b.is_ascii_digit()) && !press.is_empty() {
                    let i = digits::<u8>(press)
                        .filter(|i| usize::from(*i) < MAX_ACTIONS)
                        .ok_or(ACT_USAGE)?;
                    Press::Index(i)
                } else {
                    let label = clip(press, TITLE_CAP);
                    if label.is_empty() {
                        return Err(ACT_USAGE);
                    }
                    Press::Label(label)
                };
                Ok(Self::Act { id, press })
            }
            _ => Err(NOTICE_USAGE),
        }
    }
}

/// What the host does after [`apply`].
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Paint {
    /// Nothing painted changed (a record, an identical line, a line that
    /// ended nothing).
    None,
    /// Repaint now.
    Now,
    /// A paint is armed at [`WireGate::due`]: the last wire paint is younger
    /// than [`WIRE_PAINT_GAP`].
    Paced,
}

/// What [`apply`] did.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Applied {
    /// The whole reply line, without `\n`.
    pub reply: String,
    /// What the host paints.
    pub paint: Paint,
    /// Log one `aterm.log` line for it: a line that minted or ended a
    /// message. A restate, a duplicate and every refusal log nothing, so a
    /// flood cannot fill the host's log either.
    pub note: bool,
}

impl Applied {
    fn quiet(reply: String) -> Self {
        Self {
            reply,
            paint: Paint::None,
            note: false,
        }
    }
}

/// The wire's budgets and paint pacing: held by the host, owned here.
#[derive(Clone, Debug, Default)]
pub struct WireGate {
    /// The mints inside the trailing window, oldest first: when, and the
    /// title and detail bytes each carried ([`WIRE_BYTES_PER_WINDOW`]).
    mints: VecDeque<(Instant, usize)>,
    /// The `notice act` presses inside the trailing window, oldest first.
    presses: VecDeque<Instant>,
    /// The last wire paint.
    last_paint: Option<Instant>,
    /// The one paced paint armed, if any.
    paint_due: Option<Instant>,
}

impl WireGate {
    /// The armed paced paint — folded into the host's messages deadline.
    /// One instant at most, however many lines arrived.
    #[must_use]
    pub fn due(&self) -> Option<Instant> {
        self.paint_due
    }

    /// `true` ⇒ paint now: the paced paint is due. Records the paint at its
    /// grid instant, so the next paced one lands a whole frame later.
    pub fn take_due(&mut self, now: Instant) -> bool {
        match self.paint_due {
            Some(t) if now >= t => {
                self.painted(t);
                true
            }
            _ => false,
        }
    }

    /// Spend one mint carrying `bytes` of title and detail, or say which
    /// budget is spent and how long until the mint fits.
    fn try_mint(&mut self, now: Instant, bytes: usize) -> Result<(), Busy> {
        while let Some(&(t, _)) = self.mints.front() {
            if later(t, WIRE_MINT_WINDOW) <= now {
                self.mints.pop_front();
            } else {
                break;
            }
        }
        let free_at = |t: Instant| later(t, WIRE_MINT_WINDOW).saturating_duration_since(now);
        if let Some(&(oldest, _)) = self.mints.front()
            && self.mints.len() >= WIRE_MINTS_PER_WINDOW
        {
            return Err(Busy::Mints(free_at(oldest)));
        }
        // The bytes: wait for the oldest mints whose leaving makes room. One
        // line never outweighs the window (a title and a detail at their
        // caps are a few KiB); the clamp only makes the search total.
        let bytes = bytes.min(WIRE_BYTES_PER_WINDOW);
        let mut spent: usize = self.mints.iter().map(|&(_, b)| b).sum();
        if spent + bytes > WIRE_BYTES_PER_WINDOW {
            for &(t, b) in &self.mints {
                spent -= b;
                if spent + bytes <= WIRE_BYTES_PER_WINDOW {
                    return Err(Busy::Bytes(free_at(t)));
                }
            }
        }
        self.mints.push_back((now, bytes));
        Ok(())
    }

    /// Spend one `notice act` press, or say how long until one is free.
    fn try_press(&mut self, now: Instant) -> Result<(), Busy> {
        while let Some(&t) = self.presses.front() {
            if later(t, WIRE_MINT_WINDOW) <= now {
                self.presses.pop_front();
            } else {
                break;
            }
        }
        if let Some(&oldest) = self.presses.front()
            && self.presses.len() >= WIRE_PRESSES_PER_WINDOW
        {
            return Err(Busy::Presses(
                later(oldest, WIRE_MINT_WINDOW).saturating_duration_since(now),
            ));
        }
        self.presses.push_back(now);
        Ok(())
    }

    /// Give back the last mint (the post was a duplicate: nothing minted).
    fn refund(&mut self) {
        self.mints.pop_back();
    }

    /// A glass change: paint now if the last wire paint is a frame old, else
    /// arm the one paced paint at the next frame. Both are recorded on the
    /// center's motion grid ([`MessageCenter::frame_instant`]), and a paced
    /// paint lands ON it: the band's own motion frames fall on the same
    /// instants, so a flood over a moving row shares their present instead
    /// of adding one between them (design ruling 190 — measured live: 56
    /// presents a second before, 31-32 after).
    fn pace(&mut self, center: &MessageCenter, now: Instant) -> Paint {
        match self.last_paint {
            Some(last) if now < later(last, WIRE_PAINT_GAP) => {
                let at = center.grid_after(now, later(last, WIRE_PAINT_GAP));
                self.paint_due.get_or_insert(at);
                Paint::Paced
            }
            _ => {
                self.painted(center.frame_instant(now));
                Paint::Now
            }
        }
    }

    /// A paint happened in the frame starting at `frame`: it covers any
    /// paced one.
    fn painted(&mut self, frame: Instant) {
        self.last_paint = Some(frame);
        self.paint_due = None;
    }
}

/// `t + d`, saturating at `t`.
fn later(t: Instant, d: Duration) -> Instant {
    t.checked_add(d).unwrap_or(t)
}

/// A row the wire owns: minted by it, or keyed in its namespace (a carried
/// wire row's origin is `Carried`, its key is still the wire's).
fn is_wire(row: &Live) -> bool {
    row.msg.origin == Origin::Wire
        || row
            .msg
            .key
            .as_deref()
            .is_some_and(|k| k.starts_with(WIRE_KEY_PREFIX))
}

/// How many wire rows are live.
fn live_wire(center: &MessageCenter) -> usize {
    center.live_rows().filter(|l| is_wire(l)).count()
}

fn cap_refusal() -> Applied {
    Applied::quiet(format!(
        "ERR notice: {WIRE_LIVE_CAP} wire messages are live; end one with notice done"
    ))
}

/// Which wire budget a line found spent, and how long until it fits.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Busy {
    /// [`WIRE_MINTS_PER_WINDOW`].
    Mints(Duration),
    /// [`WIRE_BYTES_PER_WINDOW`].
    Bytes(Duration),
    /// [`WIRE_PRESSES_PER_WINDOW`].
    Presses(Duration),
}

impl Busy {
    /// The whole `ERR busy notice: … retry_ms=<ms>` line (the transient
    /// class drivers back off on; `retry_ms` rounds up, ruling 184).
    fn reply(self) -> String {
        let (what, retry) = match self {
            Self::Mints(d) => (format!("{WIRE_MINTS_PER_WINDOW} new messages a minute"), d),
            Self::Bytes(d) => (
                format!("{} KiB of words a minute", WIRE_BYTES_PER_WINDOW / 1024),
                d,
            ),
            Self::Presses(d) => (format!("{WIRE_PRESSES_PER_WINDOW} presses a minute"), d),
        };
        let ms = retry.as_nanos().div_ceil(1_000_000);
        format!("ERR busy notice: {what} retry_ms={ms}")
    }
}

fn busy_refusal(busy: Busy) -> Applied {
    Applied::quiet(busy.reply())
}

/// The bytes a mint carries against [`WIRE_BYTES_PER_WINDOW`]: its title
/// and its detail lines, what `messages.log` writes twice.
fn words_bytes(msg: &Message) -> usize {
    msg.title.len() + msg.detail.iter().map(String::len).sum::<usize>()
}

/// A reply that repaints now.
fn shown(center: &MessageCenter, gate: &mut WireGate, now: Instant, reply: String) -> Applied {
    gate.painted(center.frame_instant(now));
    Applied {
        reply,
        paint: Paint::Now,
        note: true,
    }
}

/// Apply every form but `act` (the host performs presses: [`press_target`]).
/// The verdict, the caps, the budgets, the keys, the echoes, the pacing and
/// every reply word are decided here; every `ERR` is side-effect free, so a
/// refused line changes nothing.
pub fn apply(
    center: &mut MessageCenter,
    gate: &mut WireGate,
    req: NoticeRequest,
    stamp: WallStamp,
    now: Instant,
) -> Applied {
    match req {
        NoticeRequest::Post(p) => apply_post(center, gate, p, stamp, now),
        NoticeRequest::Progress(p) => apply_progress(center, gate, &p, stamp, now),
        NoticeRequest::Done(d) => apply_done(center, gate, d, now),
        NoticeRequest::Dismiss(id) => apply_dismiss(center, gate, id, now),
        NoticeRequest::Act { .. } => Applied::quiet(ACT_IS_THE_HOSTS.to_string()),
    }
}

fn apply_post(
    center: &mut MessageCenter,
    gate: &mut WireGate,
    p: PostRequest,
    stamp: WallStamp,
    now: Instant,
) -> Applied {
    let record = p.is_record();
    let msg = p.into_message().normalized();
    let under_key = msg
        .key
        .as_deref()
        .and_then(|k| center.live_by_key(k))
        .map(|l| l.id);
    let bytes = words_bytes(&msg);
    if record {
        if let Err(busy) = gate.try_mint(now, bytes) {
            return busy_refusal(busy);
        }
        // The script saying the thing is fixed.
        let resolved = under_key.is_some_and(|id| center.resolve(id, Outcome::Ok, now));
        let posted = center.post(msg, stamp, now);
        let reply = format!("OK recorded message={}", posted.id);
        return if resolved {
            shown(center, gate, now, reply)
        } else {
            Applied {
                reply,
                paint: Paint::None,
                note: true,
            }
        };
    }
    let duplicate = center.live_rows().any(|l| l.duplicates(&msg));
    if !duplicate {
        if under_key.is_none() && live_wire(center) >= WIRE_LIVE_CAP {
            return cap_refusal();
        }
        if let Err(busy) = gate.try_mint(now, bytes) {
            return busy_refusal(busy);
        }
    }
    let posted = center.post(msg, stamp, now);
    let reply = format!("OK message={}", posted.id);
    if posted.outcome == PostOutcome::Duplicate {
        if !duplicate {
            gate.refund();
        }
        // A repeat bumps the count and re-anchors the hold: paced, unlogged.
        let paint = gate.pace(center, now);
        return Applied {
            reply,
            paint,
            note: false,
        };
    }
    shown(center, gate, now, reply)
}

fn apply_progress(
    center: &mut MessageCenter,
    gate: &mut WireGate,
    p: &ProgressRequest,
    stamp: WallStamp,
    now: Instant,
) -> Applied {
    let under_key = center
        .live_by_key(&p.key)
        .map(|l| (l.id, matches!(l.msg.hold, Hold::Live { .. })));
    if let Some((id, true)) = under_key {
        let before = center.revision();
        center.restate(id, p.restatement(), now);
        let paint = if center.revision() == before {
            Paint::None
        } else {
            gate.pace(center, now)
        };
        return Applied {
            reply: format!("OK message={id}"),
            paint,
            note: false,
        };
    }
    if under_key.is_none() && live_wire(center) >= WIRE_LIVE_CAP {
        return cap_refusal();
    }
    let msg = p.clone().into_message();
    if let Err(busy) = gate.try_mint(now, words_bytes(&msg)) {
        return busy_refusal(busy);
    }
    let posted = center.post(msg, stamp, now);
    let reply = format!("OK message={}", posted.id);
    if posted.outcome == PostOutcome::Duplicate {
        gate.refund();
        let paint = gate.pace(center, now);
        return Applied {
            reply,
            paint,
            note: false,
        };
    }
    shown(center, gate, now, reply)
}

fn apply_done(
    center: &mut MessageCenter,
    gate: &mut WireGate,
    d: DoneRequest,
    now: Instant,
) -> Applied {
    let Some(id) = center.live_by_key(&d.key).map(|l| l.id) else {
        return Applied::quiet("OK done=- how=gone".to_string());
    };
    if let Some(words) = d.words {
        center.restate(
            id,
            Restatement {
                title: Some(words),
                ..Restatement::default()
            },
            now,
        );
    }
    match d.how {
        DoneHow::Ok => center.resolve(id, Outcome::Ok, now),
        DoneHow::Warn => center.resolve(id, Outcome::Warn, now),
        DoneHow::Withdraw => center.withdraw(id, now),
    };
    let reply = format!("OK done={id} how={}", d.how.retired().as_word());
    shown(center, gate, now, reply)
}

fn apply_dismiss(
    center: &mut MessageCenter,
    gate: &mut WireGate,
    id: MessageId,
    now: Instant,
) -> Applied {
    let Some(row) = center.live(id) else {
        return Applied::quiet(no_live_reply(id));
    };
    if !is_wire(row) {
        match row.msg.hold {
            Hold::Live { .. } => {
                return Applied::quiet(format!("ERR notice: message {id} is work in flight"));
            }
            Hold::Ask { .. } => {
                return Applied::quiet(format!(
                    "ERR notice: message {id} asks; answer it with notice act"
                ));
            }
            Hold::Default | Hold::For(_) | Hold::Standing | Hold::LogOnly => {}
        }
    }
    center.dismiss(id, now);
    shown(center, gate, now, format!("OK dismissed={id}"))
}

/// The capsule a press names on a live row: `Index(i)` for `i <
/// actions.len()`, a FULL label equal to [`Intent::label`] (exact,
/// case-sensitive), or `Details` → [`ActionIndex::DETAILS`].
#[must_use]
pub fn resolve_press(row: &Live, press: &Press) -> Option<ActionIndex> {
    match press {
        Press::Index(i) => (usize::from(*i) < row.msg.actions.len()).then_some(ActionIndex(*i)),
        Press::Label(label) => row
            .msg
            .actions
            .iter()
            .position(|a| a.label() == label)
            .and_then(|i| u8::try_from(i).ok())
            .map(ActionIndex),
        Press::Details => Some(ActionIndex::DETAILS),
    }
}

/// The press as the caller spelled it — for `no action <press>`.
#[must_use]
pub fn press_words(press: &Press) -> String {
    match press {
        Press::Index(i) => i.to_string(),
        Press::Label(label) => label.clone(),
        Press::Details => "details".to_string(),
    }
}

/// `ERR notice: no live message <id>` — `dismiss`, `act` and the host's
/// press that found the row gone all answer with this one line.
#[must_use]
pub fn no_live_reply(id: MessageId) -> String {
    format!("ERR notice: no live message {id}")
}

/// What `notice act` presses: the capsule index and its FULL label, or the
/// whole `ERR …` reply (`no live message <id>`, `no action <press> on
/// message <id>`, the press percent-encoded, or `ERR busy notice: 10
/// presses a minute retry_ms=<ms>`). A press that names a capsule spends one
/// of [`WIRE_PRESSES_PER_WINDOW`] (design ruling 195): each repaints, may
/// open a window and logs a line, so a loop of them cannot. The host then
/// performs the page's own press and answers with [`acted_reply`].
///
/// # Errors
/// The reply line, when there is no live row, no such capsule, or no press
/// left in the window.
pub fn press_target(
    center: &MessageCenter,
    gate: &mut WireGate,
    id: MessageId,
    press: &Press,
    enc: &dyn Fn(&str) -> String,
    now: Instant,
) -> Result<(ActionIndex, &'static str), String> {
    let row = center.live(id).ok_or_else(|| no_live_reply(id))?;
    let index = resolve_press(row, press).ok_or_else(|| {
        format!(
            "ERR notice: no action {} on message {id}",
            enc(&press_words(press))
        )
    })?;
    gate.try_press(now).map_err(Busy::reply)?;
    let label = if index.is_details() {
        Intent::Details.label()
    } else {
        row.msg.actions[usize::from(index.0)].label()
    };
    Ok((index, label))
}

/// `OK acted=<pct label> performed=<1|0>`.
#[must_use]
pub fn acted_reply(label: &str, performed: bool, enc: &dyn Fn(&str) -> String) -> String {
    format!("OK acted={} performed={}", enc(label), u8::from(performed))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::center::Outcome;
    use crate::model::{Meter, WallStamp, tags};
    use crate::text::char_width;
    use crate::{Duration, HOLD_WARN, Instant};

    /// The host's `pct_encode` (aterm-control/src/wire.rs:28), copied so the
    /// pinned strings are the bytes the socket carries.
    fn pct_encode(s: &str) -> String {
        const HEX: &[u8; 16] = b"0123456789ABCDEF";
        let mut out = String::with_capacity(s.len());
        for b in s.bytes() {
            if b.is_ascii_graphic() && b != b'%' {
                out.push(b as char);
            } else {
                out.push('%');
                out.push(HEX[usize::from(b >> 4)] as char);
                out.push(HEX[usize::from(b & 0x0f)] as char);
            }
        }
        out
    }

    fn t0() -> Instant {
        Instant::now()
    }

    fn stamp(ms: u64) -> WallStamp {
        WallStamp { unix_ms: ms }
    }

    /// `appstatus` reads a meter's FILL only: a busy row is `progress=-`,
    /// like any row with no fraction, and busy changes nothing on the wire.
    /// A harness note is a record: `kind=harness`, `phase=done`, never live
    /// (design ruling 61).
    #[test]
    fn activity_rows_compat_ignores_busy_and_lists_the_harness() {
        let now = t0();
        let mut c = MessageCenter::new(MessageLog::empty(), now);
        c.post(
            Message::new(tags::UPDATE, Severity::Info, "Updating to aterm v1")
                .line("checking the download")
                .meter(Meter::busy("45 MB"))
                .hold(Hold::Live {
                    stale_after: crate::STALE_UPDATE,
                }),
            stamp(1_000),
            now,
        );
        c.post(
            Message::new(tags::HARNESS, Severity::Info, "aterm harness acting").hold(Hold::LogOnly),
            stamp(2_000),
            now,
        );
        let rows = activity_rows_compat(&c, now, &pct_encode);
        assert_eq!(rows.len(), 2, "{rows:?}");
        assert_eq!(
            rows[0],
            "activity kind=update phase=live progress=- title=Updating%20to%20aterm%20v1 \
             detail=checking%20the%20download stats=45%20MB outcome=-"
        );
        assert!(
            rows[1].starts_with(
                "activity kind=harness phase=done progress=- title=aterm%20harness%20acting \
                 detail= stats= outcome=ok since_ms="
            ),
            "{}",
            rows[1]
        );
    }

    /// Invariant 20: a live toolchain meter at 50 % and a retired Warn
    /// update row render exactly the status_bars.rs:4443 strings — and
    /// toolchain-tagged live rows precede update-tagged ones whatever the
    /// glass order (the update row is posted first and sits above).
    #[test]
    fn activity_rows_compat_is_byte_identical() {
        let now = t0();
        let mut c = MessageCenter::new(MessageLog::empty(), now);
        let failed = c.post(
            Message::new(tags::UPDATE, Severity::Warn, "Update failed")
                .line("could not verify the download"),
            stamp(1_000),
            now,
        );
        c.commit_rows(now, 3);
        c.post(
            Message::new(
                tags::TOOLCHAIN,
                Severity::Info,
                "Installing the ALab toolchain",
            )
            .line("trust \u{00b7} extracting")
            .meter(Meter {
                fill_permille: Some(500),
                stats: "512 MB / 1.2 GB".into(),
                ..Meter::default()
            })
            .hold(Hold::Live {
                stale_after: crate::STALE_UPDATE,
            }),
            stamp(2_000),
            now,
        );
        c.commit_rows(now, 3);
        assert_eq!(
            c.on_glass().map(|l| l.id).collect::<Vec<_>>(),
            vec![failed.id, MessageId::from_raw(2).unwrap()],
            "the Warn sits above the Info meter on glass"
        );
        let rows = activity_rows_compat(&c, now, &pct_encode);
        assert_eq!(
            rows,
            vec![
                "activity kind=toolchain phase=live progress=50/100 title=Installing%20the%20ALab%20toolchain detail=trust%20%C2%B7%20extracting stats=512%20MB%20/%201.2%20GB outcome=-",
                "activity kind=update phase=live progress=- title=Update%20failed detail=could%20not%20verify%20the%20download stats= outcome=-",
            ]
        );
        // The Warn row folds into the ledger; 1.5 s later it says so.
        let folds = now + HOLD_WARN;
        assert!(!c.settle(folds, true).retired.is_empty());
        let later = folds + Duration::from_millis(1500);
        let rows = activity_rows_compat(&c, later, &pct_encode);
        assert_eq!(rows.len(), 2);
        assert!(
            rows[0].starts_with("activity kind=toolchain phase=live progress=50/100 "),
            "{}",
            rows[0]
        );
        assert_eq!(
            rows[1],
            "activity kind=update phase=done progress=- title=Update%20failed detail=could%20not%20verify%20the%20download stats= outcome=warn since_ms=1500"
        );
        // A superseded row is not a finished outcome; a resolved Ok one is.
        let a = c.post(
            Message::new(tags::UPDATE, Severity::Info, "Checking").key("update.progress"),
            stamp(3_000),
            later,
        );
        c.post(
            Message::new(tags::UPDATE, Severity::Success, "Updated").key("update.progress"),
            stamp(3_100),
            later,
        );
        assert!(c.live(a.id).is_none());
        let rows = activity_rows_compat(&c, later, &pct_encode);
        assert!(
            rows.iter().all(|r| !r.contains("title=Checking ")),
            "{rows:?}"
        );
        let b = c.live_by_key("update.progress").unwrap().id;
        c.resolve(b, Outcome::Ok, later + Duration::from_millis(10));
        let rows = activity_rows_compat(&c, later + Duration::from_millis(20), &pct_encode);
        assert!(rows.last().unwrap().starts_with("activity kind=update phase=done progress=- title=Updated detail= stats= outcome=ok since_ms=10"), "{rows:?}");
        // A withdrawn meter (its file vanished before any marker) is no
        // finished activity either: the bars dropped it without a ledger
        // row, and the face lists none for it.
        let vanished = c.post(
            Message::new(
                tags::TOOLCHAIN,
                Severity::Info,
                "Installing the ALab toolchain",
            )
            .hold(Hold::Live {
                stale_after: crate::STALE_TAILED,
            })
            .key("toolchain.pass"),
            stamp(4_000),
            later,
        );
        assert!(c.withdraw(vanished.id, later + Duration::from_millis(30)));
        let rows = activity_rows_compat(&c, later + Duration::from_millis(40), &pct_encode);
        assert!(
            rows.iter()
                .all(|r| !(r.contains("phase=done") && r.contains("ALab"))),
            "{rows:?}"
        );
        // The wire is unforgeable: a title full of newlines and `=` is one
        // token in one row.
        let mut hostile = MessageCenter::new(MessageLog::empty(), now);
        hostile.post(
            Message::new(
                tags::TOOLCHAIN,
                Severity::Warn,
                "boom\nactivity kind=update phase=done outcome=ok stats= detail= title=forged",
            ),
            stamp(1),
            now,
        );
        let rows = activity_rows_compat(&hostile, now, &pct_encode);
        assert_eq!(rows.len(), 1);
        assert!(!rows[0].contains('\n'));
        assert_eq!(rows[0].matches("activity kind=").count(), 1, "{}", rows[0]);
        assert!(rows[0].starts_with("activity kind=toolchain phase=live "));
        assert!(rows[0].ends_with(" outcome=-"));
    }

    /// `since_ms` NEVER RUNS AHEAD OF `ago_ms` (design ruling 201): both are
    /// read off the reply's one wall `now` — the ingress stamp and the
    /// retirement's wall time — so a record retired 300 ms into its life
    /// reads `since_ms = ago_ms − 300` at every read, and a wall clock that
    /// moved less than the monotonic one (the live run's 19 ms) clamps to
    /// `since_ms <= ago_ms`, never past it.
    #[test]
    fn since_ms_never_runs_ahead_of_ago_ms() {
        let now = t0();
        let mut c = MessageCenter::new(MessageLog::empty(), now);
        let p = c.post(
            Message::new(tags::CONFIG, Severity::Warn, "Font not found"),
            stamp(10_000),
            now,
        );
        c.commit_rows(now, 3);
        assert!(c.dismiss(p.id, now + Duration::from_millis(300)));
        let rec = c.log().get(p.id).unwrap();
        let field = |row: &str, key: &str| -> u64 {
            row.split(' ')
                .find_map(|f| f.strip_prefix(key))
                .unwrap_or_else(|| panic!("{key} in {row}"))
                .parse()
                .unwrap()
        };
        for now_unix_ms in [10_000, 10_150, 10_300, 10_301, 12_345, 99_999] {
            let row = message_row(rec, None, None, now_unix_ms, &pct_encode);
            let (ago, since) = (field(&row, "ago_ms="), field(&row, "since_ms="));
            assert_eq!(ago, now_unix_ms - 10_000, "{row}");
            assert!(since <= ago, "{row}");
            assert_eq!(since, now_unix_ms.saturating_sub(10_300), "{row}");
        }
    }

    /// A CARRIED row's `since_ms` counts from its retirement in THIS process,
    /// never from the parent's ingress (design ruling 204): a download row
    /// stamped 60 s before the handoff and superseded 1 s after the seed
    /// reads `since_ms` = the read delay, and its logged retirement is the
    /// seed's wall plus 1 s.
    #[test]
    fn a_carried_rows_since_ms_counts_from_its_retirement_here() {
        let now = t0();
        let download = |title: &str| {
            Message::new(tags::UPDATE, Severity::Info, title)
                .key("update.progress")
                .meter(Meter {
                    fill_permille: Some(400),
                    ..Meter::default()
                })
                .hold(Hold::Live {
                    stale_after: Duration::from_secs(30),
                })
        };
        let mut parent = MessageCenter::new(MessageLog::empty(), now);
        let p = parent.post(download("Downloading"), stamp(10_000), now);
        parent.commit_rows(now, 3);
        let carry = parent.carried();
        let seed = now + Duration::from_secs(60);
        let mut child = MessageCenter::new(MessageLog::empty(), seed);
        child.seed_carried(&carry, stamp(70_000), seed);
        let retired = seed + Duration::from_secs(1);
        child.post(download("Installing"), stamp(71_000), retired);
        let rec = child.log().get(p.id).unwrap();
        assert!(rec.retired_at.is_some(), "{rec:?}");
        assert_eq!(rec.retired_unix_ms, Some(71_000));
        let row = message_row(rec, None, None, 71_050, &pct_encode);
        assert!(row.contains(" ago_ms=61050 "), "{row}");
        assert!(row.ends_with(" since_ms=50"), "{row}");
    }

    #[test]
    fn message_rows_carry_state_glass_and_progress() {
        let now = t0();
        let mut c = MessageCenter::new(MessageLog::empty(), now);
        let p = c.post(
            Message::new(tags::PRIVACY, Severity::Info, "File access not confirmed")
                .line("Full Disk Access may already be enabled")
                .line("second line")
                .action(crate::model::Intent::OpenSystemPane {
                    pane: "full-disk-access".into(),
                })
                .action(crate::model::Intent::NotNow {
                    decision: crate::model::Decision::FileAccess,
                })
                .key("privacy.fda")
                .hold(Hold::Ask {
                    for_: Duration::from_secs(600),
                }),
            stamp(5_000),
            now,
        );
        c.commit_rows(now, 3);
        let rec = c.log().get(p.id).unwrap();
        let row = message_row(rec, c.live(p.id), Some(0), 6_500, &pct_encode);
        assert_eq!(
            row,
            "message 1 at=5000 ago_ms=1500 tag=privacy sev=info origin=host state=held glass=0 rep=1 key=privacy.fda title=File%20access%20not%20confirmed detail=Full%20Disk%20Access%20may%20already%20be%20enabled%0Asecond%20line actions=Open%20Settings,Not%20now"
        );
        let m = c.post(
            Message::new(tags::TOOLCHAIN, Severity::Info, "Installing")
                .meter(Meter {
                    fill_permille: Some(425),
                    stats: "x".into(),
                    ..Meter::default()
                })
                .hold(Hold::Live {
                    stale_after: Duration::from_secs(30),
                }),
            stamp(5_100),
            now,
        );
        let rec = c.log().get(m.id).unwrap();
        let row = message_row(rec, c.live(m.id), None, 5_100, &pct_encode);
        assert!(
            row.ends_with(" key=- title=Installing detail= actions=- progress=43/100"),
            "{row}"
        );
        assert!(row.contains(" state=live glass=- "), "{row}");
        c.dismiss(p.id, now + Duration::from_millis(250));
        let rec = c.log().get(p.id).unwrap();
        let row = message_row(rec, None, None, 9_000, &pct_encode);
        assert!(row.contains(" state=dismissed glass=- "), "{row}");
        // Retired at 5 000 + 250 on the wall; read at 9 000.
        assert!(row.contains(" ago_ms=4000 "), "{row}");
        assert!(row.ends_with(" since_ms=3750"), "{row}");
        // The read query: ascending, newest n, filters.
        let q = parse_read_args("1 tag=privacy").unwrap();
        let picked = q.select(c.log());
        assert_eq!(picked.iter().map(|r| r.id).collect::<Vec<_>>(), vec![p.id]);
        let q = parse_read_args("since=1 live sev=info").unwrap();
        assert_eq!(
            q.select(c.log()).iter().map(|r| r.id).collect::<Vec<_>>(),
            vec![m.id]
        );
        assert_eq!(parse_read_args("").unwrap(), ReadQuery::default());
        assert_eq!(parse_read_args("9999").unwrap().n, Some(LOG_CAP));
        for junk in [
            "0", "since=0", "since=x", "tag=Bad", "sev=loud", "wat", "live=1",
        ] {
            assert_eq!(parse_read_args(junk), Err(READ_USAGE), "{junk}");
        }
    }

    /// Review (2026-09-24, design ruling 198): paging with the last id seen
    /// never skips a record. 100 records: `since=10` under `n=64` is 11..=74,
    /// then `since=74` is 75..=100; a bare read is still the newest 64, and
    /// `since=` with no count is everything above the id.
    #[test]
    fn paging_since_the_last_id_loses_nothing() {
        let now = t0();
        let mut c = MessageCenter::new(MessageLog::empty(), now);
        for i in 0..100u64 {
            c.post(
                Message::new(tags::SYSTEM, Severity::Info, format!("record {i}"))
                    .hold(Hold::LogOnly),
                stamp(i),
                now,
            );
        }
        let ids = |line: &str| -> Vec<u64> {
            parse_read_args(line)
                .unwrap()
                .select(c.log())
                .iter()
                .map(|r| r.id.raw())
                .collect()
        };
        assert_eq!(ids("64 since=10"), (11..=74).collect::<Vec<_>>());
        assert_eq!(ids("64 since=74"), (75..=100).collect::<Vec<_>>());
        assert_eq!(ids("since=10"), (11..=100).collect::<Vec<_>>());
        assert_eq!(ids(""), (37..=100).collect::<Vec<_>>());
        assert_eq!(ids("5"), (96..=100).collect::<Vec<_>>());
        assert_eq!(ids("since=100"), Vec::<u64>::new());
    }

    #[test]
    fn notice_post_parses_the_grammar_and_never_authors_an_action() {
        let req = PostRequest::parse(
            "system sev=warn key=k.1 hold=90 Something happened -- line one\\nline two",
        )
        .unwrap();
        assert_eq!(req.tag, tags::SYSTEM);
        assert_eq!(req.severity, Severity::Warn);
        assert_eq!(req.key.as_deref(), Some("wire.k.1"));
        assert_eq!(req.hold_secs, Some(90));
        assert_eq!(req.title, "Something happened");
        assert_eq!(req.detail, vec!["line one", "line two"]);
        let msg = req.into_message();
        assert_eq!(msg.origin, Origin::Wire);
        assert!(msg.actions.is_empty());
        assert_eq!(msg.hold, Hold::For(Duration::from_secs(90)));
        assert_eq!(msg.glyph, Severity::Warn.default_glyph());
        let plain = PostRequest::parse("fabric  just a title").unwrap();
        assert_eq!(plain.severity, Severity::Info);
        assert_eq!(plain.title, "just a title");
        assert!(plain.detail.is_empty());
        assert_eq!(
            plain.into_message().hold,
            Hold::LogOnly,
            "an info post is a record (design ruling 163)"
        );
        for junk in [
            "",
            "system",
            "Bad title",
            "system sev=loud t",
            "system hold=0 t",
            "system hold=3601 t",
            "system hold=x t",
        ] {
            assert_eq!(PostRequest::parse(junk), Err(POST_USAGE), "{junk:?}");
        }
        let long = format!("system t -- {}", "é".repeat(WIRE_DETAIL_BYTES));
        let req = PostRequest::parse(&long).unwrap();
        assert!(req.detail[0].len() <= WIRE_DETAIL_BYTES);
        let many = format!(
            "system t -- {}",
            (0..40)
                .map(|i| i.to_string())
                .collect::<Vec<_>>()
                .join("\\n")
        );
        assert_eq!(
            PostRequest::parse(&many).unwrap().detail.len(),
            DETAIL_LINES_CAP
        );
        let _ = char_width;
    }

    // ---- `notice` (design rulings 163–198) -------------------------------

    use crate::center::EchoKind;
    use crate::model::{Decision, Load, Unit};
    use crate::progress::Eta;
    use crate::text::glass_title_fault;
    use crate::{
        PROGRESS_GRACE, RATE_MIN_SPAN, STALE_WIRE, WIRE_BYTES_PER_WINDOW, WIRE_LIVE_CAP,
        WIRE_MINTS_PER_WINDOW, WIRE_PAINT_GAP, WIRE_PRESSES_PER_WINDOW,
    };

    fn ms(n: u64) -> Duration {
        Duration::from_millis(n)
    }

    fn fresh(now: Instant) -> (MessageCenter, WireGate) {
        (
            MessageCenter::new(MessageLog::empty(), now),
            WireGate::default(),
        )
    }

    /// Parse and apply one `notice` line, as the host's two threads do.
    fn notice(c: &mut MessageCenter, g: &mut WireGate, line: &str, now: Instant) -> Applied {
        let req = NoticeRequest::parse(line).unwrap_or_else(|e| panic!("{line:?}: {e}"));
        apply(c, g, req, stamp(1_000), now)
    }

    /// The id an `OK message=<id>` reply names.
    fn id_of(a: &Applied) -> MessageId {
        let raw = a
            .reply
            .rsplit('=')
            .next()
            .and_then(|n| n.parse().ok())
            .unwrap_or_else(|| panic!("{}", a.reply));
        MessageId::from_raw(raw).unwrap()
    }

    /// Reveal every row past its grace and commit the band: what the host's
    /// settle and grid pass do.
    fn onto_glass(c: &mut MessageCenter, now: Instant) {
        c.settle(now, true);
        c.commit_rows(now, 3);
    }

    #[test]
    fn notice_parses_every_form_and_refuses_junk() {
        let Ok(NoticeRequest::Post(p)) = NoticeRequest::parse(
            "post system sev=warn key=deploy hold=90 Deploy failed -- connection refused",
        ) else {
            panic!()
        };
        assert_eq!(p.key.as_deref(), Some("wire.deploy"));
        assert_eq!((p.severity, p.hold_secs), (Severity::Warn, Some(90)));
        assert_eq!(
            (p.title.as_str(), p.detail.clone()),
            ("Deploy failed", vec!["connection refused".to_string()])
        );
        let Ok(NoticeRequest::Progress(p)) = NoticeRequest::parse(
            "progress build pct=42.5 load=disk tag=packages Building aterm -- 3 of 10",
        ) else {
            panic!()
        };
        assert_eq!(
            p,
            ProgressRequest {
                key: "wire.build".into(),
                tag: tags::PACKAGES,
                indicator: WireIndicator::Fill(425),
                load: Some(Load::Disk),
                title: "Building aterm".into(),
                stats: "3 of 10".into(),
            }
        );
        let Ok(NoticeRequest::Progress(p)) = NoticeRequest::parse("progress build Building aterm")
        else {
            panic!()
        };
        assert_eq!(
            (p.indicator, p.tag, p.load, p.stats.as_str()),
            (WireIndicator::Busy, tags::SYSTEM, None, "")
        );
        for (line, want) in [
            ("progress b busy Working", WireIndicator::Busy),
            ("progress b pct=0 Working", WireIndicator::Fill(0)),
            ("progress b pct=100 Working", WireIndicator::Fill(1000)),
            ("progress b pct=100.0 Working", WireIndicator::Fill(1000)),
            ("progress b pct=007 Working", WireIndicator::Fill(70)),
            (
                "progress b done=5/10 Working",
                WireIndicator::Amount {
                    done: 5,
                    total: 10,
                    unit: Unit::Items,
                },
            ),
            (
                "progress b unit=bytes done=12/10 Working",
                WireIndicator::Amount {
                    done: 12,
                    total: 10,
                    unit: Unit::Bytes,
                },
            ),
        ] {
            let Ok(NoticeRequest::Progress(p)) = NoticeRequest::parse(line) else {
                panic!("{line}")
            };
            assert_eq!(p.indicator, want, "{line}");
        }
        let key40 = "a._-".repeat(10);
        assert!(NoticeRequest::parse(&format!("progress {key40} Working")).is_ok());
        for (line, how, words) in [
            ("done build", DoneHow::Ok, None),
            ("done build ok", DoneHow::Ok, None),
            ("done build warn", DoneHow::Warn, None),
            ("done build withdraw", DoneHow::Withdraw, None),
            (
                "done build ok Built aterm",
                DoneHow::Ok,
                Some("Built aterm"),
            ),
        ] {
            assert_eq!(
                NoticeRequest::parse(line),
                Ok(NoticeRequest::Done(DoneRequest {
                    key: "wire.build".into(),
                    how,
                    words: words.map(str::to_string),
                })),
                "{line}"
            );
        }
        let id7 = MessageId::from_raw(7).unwrap();
        assert_eq!(
            NoticeRequest::parse("dismiss 7"),
            Ok(NoticeRequest::Dismiss(id7))
        );
        for (line, press) in [
            ("act 7 0", Press::Index(0)),
            ("act 7 1", Press::Index(1)),
            ("act 7 Not now", Press::Label("Not now".into())),
            ("act 7 details", Press::Details),
        ] {
            assert_eq!(
                NoticeRequest::parse(line),
                Ok(NoticeRequest::Act { id: id7, press }),
                "{line}"
            );
        }
        let long_key = "k".repeat(41);
        let junk: Vec<(String, &str)> = [
            ("", NOTICE_USAGE),
            ("shout hello", NOTICE_USAGE),
            ("Post system t", NOTICE_USAGE),
            ("post", POST_USAGE),
            ("post system", POST_USAGE),
            ("post system key=Bad t", KEY_REFUSAL),
            ("post system key= t", KEY_REFUSAL),
            ("post system key=wire/x t", KEY_REFUSAL),
            ("post update t", LANE_REFUSAL),
            ("post harness t", LANE_REFUSAL),
            ("post toolchain sev=warn t", LANE_REFUSAL),
            ("post system hold=+5 t", POST_USAGE),
            ("post system -- only a detail", POST_USAGE),
            ("progress", PROGRESS_USAGE),
            ("progress k", PROGRESS_USAGE),
            ("progress k -- stats only", PROGRESS_USAGE),
            ("progress Bad t", KEY_REFUSAL),
            ("progress pct=4 t", KEY_REFUSAL),
            ("progress k pct=101 t", PROGRESS_USAGE),
            ("progress k pct=100.5 t", PROGRESS_USAGE),
            ("progress k pct=4.25 t", PROGRESS_USAGE),
            ("progress k pct=42. t", PROGRESS_USAGE),
            ("progress k pct=.5 t", PROGRESS_USAGE),
            ("progress k pct=-1 t", PROGRESS_USAGE),
            ("progress k pct=+4 t", PROGRESS_USAGE),
            ("progress k pct=1000 t", PROGRESS_USAGE),
            ("progress k pct= t", PROGRESS_USAGE),
            ("progress k done=5/0 t", PROGRESS_USAGE),
            ("progress k done=5 t", PROGRESS_USAGE),
            ("progress k done=/2 t", PROGRESS_USAGE),
            ("progress k done=1/2/3 t", PROGRESS_USAGE),
            ("progress k done=-1/2 t", PROGRESS_USAGE),
            ("progress k unit=bytes t", PROGRESS_USAGE),
            ("progress k pct=4 unit=items t", PROGRESS_USAGE),
            ("progress k done=1/2 unit=miles t", PROGRESS_USAGE),
            ("progress k pct=4 busy t", PROGRESS_USAGE),
            ("progress k pct=4 pct=5 t", PROGRESS_USAGE),
            ("progress k busy busy t", PROGRESS_USAGE),
            ("progress k load=gpu t", PROGRESS_USAGE),
            ("progress k tag=Bad t", PROGRESS_USAGE),
            ("progress k tag=update t", LANE_REFUSAL),
            ("done", DONE_USAGE),
            ("done K", KEY_REFUSAL),
            ("done k maybe", DONE_USAGE),
            ("done k Built aterm", DONE_USAGE),
            ("dismiss", DISMISS_USAGE),
            ("dismiss 0", DISMISS_USAGE),
            ("dismiss x", DISMISS_USAGE),
            ("dismiss +3", DISMISS_USAGE),
            ("dismiss 3 4", DISMISS_USAGE),
            ("act", ACT_USAGE),
            ("act 3", ACT_USAGE),
            ("act 0 details", ACT_USAGE),
            ("act x 0", ACT_USAGE),
            ("act 3 2", ACT_USAGE),
            ("act 3 999", ACT_USAGE),
            ("act 3 \u{1b}", ACT_USAGE),
        ]
        .into_iter()
        .map(|(l, e)| (l.to_string(), e))
        .chain([
            (format!("progress {long_key} t"), KEY_REFUSAL),
            (format!("done {long_key}"), KEY_REFUSAL),
        ])
        .collect();
        for (line, want) in &junk {
            assert_eq!(NoticeRequest::parse(line), Err(*want), "{line:?}");
        }
    }

    /// Every title fault the engine can name has its own `notice:` refusal.
    #[test]
    fn every_title_fault_has_its_refusal() {
        for title in [
            "one two three four five six seven",
            &"x".repeat(49),
            "Build: failed",
            "Build failed.",
        ] {
            let fault = glass_title_fault(title).unwrap();
            assert_eq!(title_refusal(fault), format!("notice: {fault}"));
        }
    }

    #[test]
    fn an_info_post_is_a_record_and_a_warn_post_is_a_row() {
        let now = t0();
        let (mut c, mut g) = fresh(now);
        let a = notice(
            &mut c,
            &mut g,
            "post system hold=90 hello from a script",
            now,
        );
        assert_eq!(
            a,
            Applied {
                reply: "OK recorded message=1".into(),
                paint: Paint::None,
                note: true,
            }
        );
        let rec = c.log().get(id_of(&a)).unwrap();
        assert_eq!(rec.retired(), Some(&Retired::Recorded));
        assert_eq!(rec.origin, Origin::Wire);
        assert_eq!(c.live_rows().count(), 0, "a record is never a row");
        let s = notice(&mut c, &mut g, "post system sev=success Deployed", now);
        assert_eq!(s.reply, "OK recorded message=2");
        let w = notice(
            &mut c,
            &mut g,
            "post system sev=warn Deploy failed -- connection refused",
            now,
        );
        assert_eq!(
            (w.reply.as_str(), w.paint, w.note),
            ("OK message=3", Paint::Now, true)
        );
        let row = c.live(id_of(&w)).unwrap();
        assert_eq!(row.msg.hold, Hold::Default);
        let e = notice(
            &mut c,
            &mut g,
            "post system sev=error hold=90 Disk full",
            now,
        );
        assert_eq!(
            c.live(id_of(&e)).unwrap().msg.hold,
            Hold::For(Duration::from_secs(90))
        );
        // A record under a live wire row's key resolves it Ok first: the
        // script saying the thing is fixed.
        let p = notice(&mut c, &mut g, "progress deploy Deploying aterm", now);
        let fixed = notice(
            &mut c,
            &mut g,
            "post system key=deploy Deploy fixed",
            now + ms(10),
        );
        assert_eq!(fixed.paint, Paint::Now);
        assert!(c.live(id_of(&p)).is_none());
        assert_eq!(
            c.log().get(id_of(&p)).unwrap().retired(),
            Some(&Retired::Resolved(Outcome::Ok))
        );
        assert_eq!(
            c.log().get(id_of(&fixed)).unwrap().key.as_deref(),
            Some("wire.deploy")
        );
    }

    /// The owner's attention rule, for every message the wire can raise: a
    /// record for news, a row only for a failure or for work with its
    /// indicator, a glass title in form, and never a capsule.
    #[test]
    fn every_wire_row_earns_its_place_on_the_glass() {
        let now = t0();
        let (mut c, mut g) = fresh(now);
        for line in [
            "post system Just so you know",
            "post fabric sev=success All good -- details here",
            "post crash sev=error key=a Worker crashed -- exit 3",
            "progress b Building aterm",
            "progress c pct=10 load=cpu Verifying the payload",
            // Past the three-row cap (ruling 197): the error row goes first.
            "done a",
            "progress d done=1/9 unit=steps Migrating the store",
        ] {
            let a = notice(&mut c, &mut g, line, now);
            assert!(a.reply.starts_with("OK "), "{line}: {}", a.reply);
        }
        assert_eq!(c.log().len(), 6);
        for rec in c
            .log()
            .records()
            .filter(|r| r.key.as_deref() != Some("wire.a"))
        {
            assert_eq!(rec.origin, Origin::Wire);
            assert!(rec.actions.is_empty());
            match c.live(rec.id) {
                None => {
                    assert!(rec.severity <= Severity::Info, "{rec:?}");
                    assert_eq!(rec.retired(), Some(&Retired::Recorded));
                }
                Some(l) => {
                    assert_eq!(glass_title_fault(&l.msg.title), None, "{}", l.msg.title);
                    let indicator = l
                        .msg
                        .meter
                        .as_ref()
                        .is_some_and(|m| m.fill_permille.is_some() || m.busy);
                    let progress = matches!(l.msg.hold, Hold::Live { .. }) && indicator;
                    assert!(
                        progress || l.msg.severity >= Severity::Warn,
                        "an FYI on glass: {:?}",
                        l.msg
                    );
                    assert!(l.msg.severity != Severity::Success);
                }
            }
        }
    }

    #[test]
    fn a_glass_title_from_the_wire_obeys_the_title_form() {
        for (line, want) in [
            (
                "post system sev=warn one two three four five six seven",
                "notice: a glass title over six words",
            ),
            (
                "post system sev=error Deploy failed: connection refused",
                "notice: a clause in a glass title",
            ),
            (
                "progress k Building the whole of the aterm workspace now",
                "notice: a glass title over six words",
            ),
            (
                "progress k Supercalifragilisticexpialidocious-builds everywhere",
                "notice: a glass title over 48 characters",
            ),
            (
                "progress k Building aterm.",
                "notice: a sentence for a glass title",
            ),
            (
                "done k ok Built; shipped",
                "notice: a clause in a glass title",
            ),
            (
                "done k warn one two three four five six seven",
                "notice: a glass title over six words",
            ),
        ] {
            assert_eq!(NoticeRequest::parse(line), Err(want), "{line}");
        }
        // A record never reaches the glass: it keeps the 120-character cap.
        let long = format!("post system {}", "word ".repeat(20));
        let Ok(NoticeRequest::Post(p)) = NoticeRequest::parse(&long) else {
            panic!()
        };
        assert_eq!(p.title.chars().count(), 99);
        let over = format!("post system {}", "x".repeat(200));
        let Ok(NoticeRequest::Post(p)) = NoticeRequest::parse(&over) else {
            panic!()
        };
        assert_eq!(p.title.chars().count(), TITLE_CAP);
        assert!(NoticeRequest::parse("post system Build: done. Next; step").is_ok());
    }

    /// Sanitising: control and invisible format characters never reach a
    /// title, a detail line, stats or a label; a title empty after the strip
    /// is a usage error.
    #[test]
    fn the_wire_strips_what_could_forge_a_row() {
        let Ok(NoticeRequest::Post(p)) = NoticeRequest::parse(
            "post system sev=warn Deploy\u{1b}[31m \u{202e}failed -- a\u{7}b\\n\u{200b}\\nc",
        ) else {
            panic!()
        };
        assert_eq!(p.title, "Deploy[31m failed");
        assert_eq!(p.detail, vec!["ab", "c"]);
        let Ok(NoticeRequest::Progress(p)) =
            NoticeRequest::parse(&format!("progress k Working -- {}\u{1b}", "9".repeat(80)))
        else {
            panic!()
        };
        assert_eq!(p.stats.chars().count(), STATS_CAP);
        assert!(p.stats.chars().all(|c| !c.is_control()));
        assert_eq!(
            NoticeRequest::parse("post system \u{200b}\u{202e}"),
            Err(POST_USAGE)
        );
        assert_eq!(
            NoticeRequest::parse("progress k \u{7}\u{7}"),
            Err(PROGRESS_USAGE)
        );
        // Review (2026-09-24, ruling 196): an invisible character cannot
        // hide a seam or a trailing period from the title form, and no bidi
        // mark, tag character or noncharacter survives into a row.
        assert_eq!(
            NoticeRequest::parse("post system sev=warn Build:\u{2060} failed"),
            Err("notice: a clause in a glass title")
        );
        assert_eq!(
            NoticeRequest::parse("post system sev=warn Build failed.\u{2060}"),
            Err("notice: a sentence for a glass title")
        );
        let Ok(NoticeRequest::Post(p)) = NoticeRequest::parse(
            "post system sev=warn De\u{061c}ploy\u{00ad} fa\u{e0041}il\u{fdd0}ed\u{ffff}\u{1fffe}\u{206a}\u{fff9}",
        ) else {
            panic!()
        };
        assert_eq!(p.title, "Deploy failed");
    }

    /// Found on the live socket (2026-09-24): `Tab\there` was stored as
    /// `Tabhere`. A whitespace control is a space before the strip.
    #[test]
    fn a_tab_on_the_wire_is_a_space() {
        let Ok(NoticeRequest::Post(p)) = NoticeRequest::parse(
            "post\tsystem\tsev=warn\tDeploy\tfailed\t--\tconnection\x0brefused",
        ) else {
            panic!()
        };
        assert_eq!(p.severity, Severity::Warn);
        assert_eq!(p.title, "Deploy failed");
        assert_eq!(p.detail, vec!["connection refused"]);
        let Ok(NoticeRequest::Progress(p)) =
            NoticeRequest::parse("progress k\tpct=5\tFetching\tsources")
        else {
            panic!()
        };
        assert_eq!(p.title, "Fetching sources");
        assert_eq!(
            NoticeRequest::parse("post system sev=warn one\ttwo\tthree\tfour\tfive\tsix\tseven"),
            Err("notice: a glass title over six words"),
            "a tab is a word gap to the title form too"
        );
    }

    #[test]
    fn the_wire_names_only_its_own_keys() {
        let now = t0();
        let (mut c, mut g) = fresh(now);
        let update = c.post(
            Message::new(tags::UPDATE, Severity::Info, "Downloading aterm")
                .key("update.progress")
                .meter(Meter::busy(""))
                .hold(Hold::Live {
                    stale_after: crate::STALE_UPDATE,
                }),
            stamp(1),
            now,
        );
        let host_k = c.post(
            Message::new(tags::SYSTEM, Severity::Warn, "Host failure").key("k"),
            stamp(2),
            now,
        );
        let before: Vec<Live> = c.live_rows().cloned().collect();
        let p = notice(&mut c, &mut g, "progress k Working", now);
        let mine = c.live(id_of(&p)).unwrap();
        assert_eq!(mine.msg.key.as_deref(), Some("wire.k"));
        for host in &before {
            assert_eq!(c.live(host.id), Some(host), "a host row moved");
        }
        assert_eq!(
            notice(&mut c, &mut g, "progress update.progress Hijack", now).reply,
            format!("OK message={}", id_of(&p).raw() + 1),
            "wire.update.progress is the wire's own, a new row"
        );
        assert_eq!(c.live(update.id), before.iter().find(|l| l.id == update.id));
        notice(&mut c, &mut g, "post system key=k Fixed", now);
        assert!(c.live(host_k.id).is_some(), "a record resolves wire.k only");
        assert!(c.live(id_of(&p)).is_none());
        let again = notice(&mut c, &mut g, "progress k Working", now);
        let d = notice(&mut c, &mut g, "done k", now);
        assert_eq!(
            d.reply,
            format!("OK done={} how=resolved-ok", id_of(&again))
        );
        assert!(c.live(host_k.id).is_some() && c.live(update.id).is_some());
    }

    #[test]
    fn progress_raises_one_row_per_key_and_restates_it_in_place() {
        let now = t0();
        let (mut c, mut g) = fresh(now);
        let a = notice(&mut c, &mut g, "progress build Building aterm", now);
        assert_eq!((a.paint, a.note), (Paint::Now, true));
        let id = id_of(&a);
        let row = c.live(id).unwrap();
        assert!(row.is_busy(), "no indicator word is busy");
        assert_eq!(row.msg.tag, tags::SYSTEM);
        assert_eq!(row.msg.origin, Origin::Wire);
        assert_eq!(row.msg.severity, Severity::Info);
        assert_eq!(
            row.msg.hold,
            Hold::Live {
                stale_after: STALE_WIRE
            }
        );
        assert_eq!(row.msg.reveal_after, Some(PROGRESS_GRACE));
        assert!(!row.revealed, "a job done inside the grace never flashes");
        let logged = c.log().len();
        let b = notice(
            &mut c,
            &mut g,
            "progress build pct=42.5 tag=packages Building aterm -- 3 of 10",
            now + ms(100),
        );
        assert_eq!((b.reply.as_str(), b.note), ("OK message=1", false));
        assert_eq!(c.log().len(), logged, "a restate logs nothing");
        let row = c.live(id).unwrap();
        assert_eq!(row.msg.tag, tags::SYSTEM, "the tag is the first line's");
        let m = row.msg.meter.as_ref().unwrap();
        assert_eq!(
            (m.fill_permille, m.busy, m.stats.as_str()),
            (Some(425), false, "3 of 10")
        );
        // done= feeds the estimator: an ETA once the rate has its span.
        let mut t = now;
        for step in 1..=8_u64 {
            t = now + ms(200) + Duration::from_secs(step - 1);
            notice(
                &mut c,
                &mut g,
                &format!(
                    "progress build done={}/100 unit=bytes Downloading aterm",
                    step * 5
                ),
                t,
            );
        }
        let row = c.live(id).unwrap();
        let amount = row.msg.meter.as_ref().unwrap().amount.unwrap();
        assert_eq!(amount.series, Amount::series_of("wire.build"));
        assert_eq!((amount.done, amount.unit), (40, Unit::Bytes));
        assert!(t - now > RATE_MIN_SPAN);
        assert!(
            matches!(row.track.eta(t), Eta::Remaining(_)),
            "{:?}",
            row.track.eta(t)
        );
        // A load a row is posted with shows at once.
        let h = notice(&mut c, &mut g, "progress fetch load=network Fetching", t);
        assert_eq!(c.live(id_of(&h)).unwrap().shown_load(), Some(Load::Network));
        // The failure takes the work's place; the work takes a failure's.
        let f = notice(
            &mut c,
            &mut g,
            "post system sev=warn key=fetch Fetch failed",
            t,
        );
        assert!(c.live(id_of(&h)).is_none());
        assert_eq!(
            c.log().get(id_of(&h)).unwrap().retired(),
            Some(&Retired::Superseded { by: id_of(&f) })
        );
        let r = notice(&mut c, &mut g, "progress fetch Retrying the fetch", t);
        assert_ne!(id_of(&r), id_of(&f));
        assert!(c.live(id_of(&f)).is_none());
        assert!(c.live(id_of(&r)).unwrap().is_busy());
        assert_eq!(
            c.live_rows()
                .filter(|l| l.msg.key.as_deref() == Some("wire.fetch"))
                .count(),
            1
        );
        // Review (2026-09-24, ruling 193): the SAME words take each other's
        // place too — a change of severity or hold kind is news, never a
        // repeat of the row it replaces.
        let work = id_of(&notice(&mut c, &mut g, "progress same Building aterm", t));
        let fail = notice(
            &mut c,
            &mut g,
            "post system sev=error key=same Building aterm",
            t,
        );
        assert_ne!(
            id_of(&fail),
            work,
            "the failure is not a repeat of the work"
        );
        assert!(c.live(work).is_none());
        let row = c.live(id_of(&fail)).unwrap();
        assert_eq!((row.msg.severity, row.repeats), (Severity::Error, 1));
        let again = id_of(&notice(&mut c, &mut g, "progress same Building aterm", t));
        assert_ne!(
            again,
            id_of(&fail),
            "the work is not a repeat of the failure"
        );
        assert!(c.live(id_of(&fail)).is_none());
        assert!(c.live(again).unwrap().is_busy());
    }

    #[test]
    fn an_identical_progress_line_bumps_nothing() {
        let now = t0();
        let (mut c, mut g) = fresh(now);
        let line = "progress build pct=40 load=disk Building aterm -- 3 of 10";
        let id = id_of(&notice(&mut c, &mut g, line, now));
        let on = now + PROGRESS_GRACE;
        onto_glass(&mut c, on);
        assert_eq!(c.glass_position(id), Some(0));
        let revision = c.revision();
        let logged = c.log().len();
        let stale_at = c.live(id).unwrap().stale_at.unwrap();
        for n in 1..=50_u64 {
            let a = notice(&mut c, &mut g, line, on + ms(n * 100));
            assert_eq!(
                a,
                Applied {
                    reply: format!("OK message={id}"),
                    paint: Paint::None,
                    note: false,
                }
            );
        }
        assert_eq!(c.revision(), revision, "N identical lines bump no revision");
        assert_eq!(c.log().len(), logged);
        assert_eq!(g.due(), None);
        let rearmed = c.live(id).unwrap().stale_at.unwrap();
        assert_eq!(rearmed, on + ms(5_000) + STALE_WIRE);
        assert!(rearmed > stale_at, "the script is alive: the cap re-arms");
    }

    #[test]
    fn a_progress_flood_paints_at_most_once_a_frame() {
        let now = t0();
        let (mut c, mut g) = fresh(now);
        let first = notice(&mut c, &mut g, "progress build pct=0 Building aterm", now);
        assert_eq!(first.paint, Paint::Now);
        let mut paints = 1_u64;
        let mut t = now;
        for i in 1..=1000_u64 {
            t = now + ms(i);
            // The host's loop: a due paced paint is taken first.
            if g.take_due(t) {
                paints += 1;
            }
            let a = notice(
                &mut c,
                &mut g,
                &format!(
                    "progress build pct={}.{} Building aterm",
                    i / 10 % 101,
                    i % 10
                ),
                t,
            );
            assert!(!a.note);
            if a.paint == Paint::Now {
                paints += 1;
            }
            // Fill changes never multiply deadlines: one paced paint at most,
            // never further than a frame away.
            assert!(g.due().is_none_or(|d| d <= t + WIRE_PAINT_GAP));
            // …and ON the motion grid, where the band's own frames land, so
            // a flood over a moving row shares their present (ruling 190).
            if let Some(d) = g.due() {
                assert_eq!(c.frame_instant(d), d, "{i}: a paced paint off the grid");
                assert!(d > t, "{i}: a paced paint in the past");
            }
        }
        let due = g.due().expect("the last change is paced");
        assert!(g.take_due(due));
        paints += 1;
        assert_eq!(g.due(), None, "taken");
        assert!(!g.take_due(due + ms(1)));
        let bound = 1 + 1000 / WIRE_PAINT_GAP.as_millis() as u64 + 1;
        assert!(
            paints <= bound,
            "{paints} paints for 1000 lines (≤ {bound})"
        );
        assert!(paints >= 20, "{paints}: the fill still moves on the glass");
        assert!(t > now);
        assert_eq!(c.live_rows().count(), 1);
        assert_eq!(c.log().len(), 1, "one mint, one log line");
    }

    #[test]
    fn an_abandoned_progress_row_goes_stale_and_fades() {
        let now = t0();
        let (mut c, mut g) = fresh(now);
        let line = "progress sync busy Syncing the mirror";
        let id = id_of(&notice(&mut c, &mut g, line, now));
        let on = now + PROGRESS_GRACE;
        onto_glass(&mut c, on);
        assert_eq!(c.glass_position(id), Some(0));
        // A long silent step re-sends its line at 119 s: the fade moves.
        notice(&mut c, &mut g, line, on + Duration::from_secs(119));
        assert!(c.settle(on + STALE_WIRE, true).retired.is_empty());
        let fade = on + Duration::from_secs(119) + STALE_WIRE;
        assert_eq!(c.deadline(true), Some(fade));
        let settled = c.settle(fade, true);
        assert_eq!(settled.retired, vec![(id, Retired::Stale)]);
        let echo = c.echoes().iter().find(|e| e.id == id).unwrap();
        assert_eq!(echo.kind, EchoKind::Vanish);
        assert_eq!(
            notice(&mut c, &mut g, "done sync", fade).reply,
            "OK done=- how=gone",
            "a set -e script does not die because its row went stale"
        );
    }

    #[test]
    fn done_ends_a_row_with_its_finish() {
        let now = t0();
        let (mut c, mut g) = fresh(now);
        let on = now + PROGRESS_GRACE;
        let ok = id_of(&notice(
            &mut c,
            &mut g,
            "progress build pct=90 Building aterm",
            now,
        ));
        let warn = id_of(&notice(&mut c, &mut g, "progress test Testing aterm", now));
        let gone = id_of(&notice(&mut c, &mut g, "progress lint Linting aterm", now));
        onto_glass(&mut c, on);
        let a = notice(&mut c, &mut g, "done build ok Built aterm", on);
        assert_eq!(
            a,
            Applied {
                reply: format!("OK done={ok} how=resolved-ok"),
                paint: Paint::Now,
                note: true,
            }
        );
        let rec = c.log().get(ok).unwrap();
        assert_eq!(rec.title, "Built aterm", "the record says the words");
        let echo = c.echoes().iter().find(|e| e.id == ok).unwrap();
        assert_eq!(echo.kind, EchoKind::Complete);
        assert_eq!(echo.msg.finished_title(), "Built aterm");
        let w = notice(&mut c, &mut g, "done test warn", on);
        assert_eq!(w.reply, format!("OK done={warn} how=resolved-warn"));
        assert_eq!(
            c.echoes().iter().find(|e| e.id == warn).unwrap().kind,
            EchoKind::Fault
        );
        let v = notice(&mut c, &mut g, "done lint withdraw", on);
        assert_eq!(v.reply, format!("OK done={gone} how=withdrawn"));
        assert_eq!(
            c.log().get(gone).unwrap().retired(),
            Some(&Retired::Withdrawn)
        );
        assert_eq!(
            c.echoes().iter().find(|e| e.id == gone).unwrap().kind,
            EchoKind::Vanish
        );
        assert!(activity_rows_compat(&c, on, &pct_encode).is_empty());
        let none = notice(&mut c, &mut g, "done nothing", on);
        assert_eq!(
            none,
            Applied {
                reply: "OK done=- how=gone".into(),
                paint: Paint::None,
                note: false,
            }
        );
        // `done` ends ANY live wire row under the key, a warn post too.
        let p = id_of(&notice(
            &mut c,
            &mut g,
            "post system sev=warn key=db Database down",
            on,
        ));
        assert_eq!(
            notice(&mut c, &mut g, "done db", on).reply,
            format!("OK done={p} how=resolved-ok")
        );
        // A job done inside the grace never reached the glass: no echo.
        let quick = id_of(&notice(&mut c, &mut g, "progress quick Checking", on));
        notice(&mut c, &mut g, "done quick", on + ms(500));
        assert!(c.echoes().iter().all(|e| e.id != quick));
    }

    #[test]
    fn the_wire_caps_its_live_rows_and_its_mints() {
        let now = t0();
        let (mut c, mut g) = fresh(now);
        for k in 1..=WIRE_LIVE_CAP {
            notice(&mut c, &mut g, &format!("progress k{k} Working"), now);
        }
        let revision = c.revision();
        let logged = c.log().len();
        let cap =
            format!("ERR notice: {WIRE_LIVE_CAP} wire messages are live; end one with notice done");
        for line in ["progress k5 Working", "post system sev=warn Deploy failed"] {
            let a = notice(&mut c, &mut g, line, now);
            assert_eq!(
                a,
                Applied {
                    reply: cap.clone(),
                    paint: Paint::None,
                    note: false,
                },
                "{line}"
            );
        }
        assert_eq!(
            (c.revision(), c.log().len()),
            (revision, logged),
            "a refusal changes nothing"
        );
        // A restate and a same-key supersede never count; a record is no row.
        assert!(
            notice(&mut c, &mut g, "progress k1 pct=5 Working", now)
                .reply
                .starts_with("OK")
        );
        assert!(
            notice(&mut c, &mut g, "post system sev=warn key=k2 K2 failed", now)
                .reply
                .starts_with("OK message=")
        );
        assert!(
            notice(&mut c, &mut g, "post system a note", now)
                .reply
                .starts_with("OK recorded")
        );
        // A host row never counts against the wire.
        let (mut c, mut g) = fresh(now);
        for i in 0..10 {
            c.post(
                Message::new(tags::SYSTEM, Severity::Warn, format!("host {i}")),
                stamp(1),
                now,
            );
        }
        assert!(
            notice(&mut c, &mut g, "progress k Working", now)
                .reply
                .starts_with("OK")
        );

        // The mint budget: 60 a minute, rows and records alike.
        let (mut c, mut g) = fresh(now);
        for i in 0..WIRE_MINTS_PER_WINDOW as u64 {
            let a = notice(
                &mut c,
                &mut g,
                &format!("post system note {i}"),
                now + ms(i),
            );
            assert!(a.reply.starts_with("OK recorded"), "{}", a.reply);
        }
        let logged = c.log().len();
        let busy = notice(&mut c, &mut g, "post system one too many", now + ms(100));
        assert_eq!(
            busy,
            Applied {
                reply: "ERR busy notice: 60 new messages a minute retry_ms=59900".into(),
                paint: Paint::None,
                note: false,
            }
        );
        let busy = notice(&mut c, &mut g, "progress k Working", now + ms(100));
        assert!(
            busy.reply.starts_with("ERR busy notice: "),
            "{}",
            busy.reply
        );
        assert_eq!(c.log().len(), logged);
        assert_eq!(c.live_rows().count(), 0);
        // The oldest mint leaves the window at 60 s: one more fits.
        let later = now + Duration::from_secs(60);
        assert!(
            notice(&mut c, &mut g, "post system again", later)
                .reply
                .starts_with("OK")
        );
        assert!(
            notice(&mut c, &mut g, "post system again 2", later)
                .reply
                .starts_with("ERR busy"),
            "the second mint of 1 ms leaves at 60 s + 1 ms"
        );
        assert!(
            notice(&mut c, &mut g, "post system again 2", later + ms(1))
                .reply
                .starts_with("OK")
        );
        assert_eq!(
            notice(&mut c, &mut g, "post system again 3", later + ms(1)).reply,
            "ERR busy notice: 60 new messages a minute retry_ms=1"
        );
        // A duplicate mints nothing: a hundred repeats of one failure cost
        // one mint, bump the count, log and note nothing.
        let (mut c, mut g) = fresh(now);
        let w = notice(&mut c, &mut g, "post system sev=warn Deploy failed", now);
        let logged = c.log().len();
        for i in 1..=100 {
            let d = notice(
                &mut c,
                &mut g,
                "post system sev=warn Deploy failed",
                now + ms(i),
            );
            assert_eq!(d.reply, w.reply);
            assert!(!d.note);
            assert_ne!(d.paint, Paint::None);
        }
        assert_eq!(c.live(id_of(&w)).unwrap().repeats, 101);
        assert_eq!(c.log().len(), logged);
        for i in 1..WIRE_MINTS_PER_WINDOW {
            let a = notice(
                &mut c,
                &mut g,
                &format!("post system note {i}"),
                now + ms(200),
            );
            assert!(a.reply.starts_with("OK recorded"), "{i}: {}", a.reply);
        }
        assert!(
            notice(&mut c, &mut g, "post system the 61st", now + ms(200))
                .reply
                .starts_with("ERR busy")
        );
    }

    #[test]
    fn dismiss_takes_down_what_a_person_could() {
        let now = t0();
        let (mut c, mut g) = fresh(now);
        let wire = id_of(&notice(
            &mut c,
            &mut g,
            "progress build Building aterm",
            now,
        ));
        let held = c
            .post(
                Message::new(tags::SYSTEM, Severity::Warn, "Disk nearly full"),
                stamp(1),
                now,
            )
            .id;
        let standing = c
            .post(
                Message::new(tags::RENDER, Severity::Error, "GPU lost").hold(Hold::Standing),
                stamp(1),
                now,
            )
            .id;
        let working = c
            .post(
                Message::new(tags::UPDATE, Severity::Info, "Downloading aterm")
                    .meter(Meter::busy(""))
                    .hold(Hold::Live {
                        stale_after: crate::STALE_UPDATE,
                    }),
                stamp(1),
                now,
            )
            .id;
        let ask = c
            .post(
                Message::new(tags::PRIVACY, Severity::Info, "Allow file access")
                    .action(Intent::NotNow {
                        decision: Decision::FileAccess,
                    })
                    .hold(Hold::Ask {
                        for_: Duration::from_secs(600),
                    }),
                stamp(1),
                now,
            )
            .id;
        for id in [wire, held, standing] {
            let a = notice(&mut c, &mut g, &format!("dismiss {id}"), now);
            assert_eq!(
                a,
                Applied {
                    reply: format!("OK dismissed={id}"),
                    paint: Paint::Now,
                    note: true,
                }
            );
            assert_eq!(
                c.log().get(id).unwrap().retired(),
                Some(&Retired::Dismissed)
            );
        }
        let revision = c.revision();
        for (id, want) in [
            (
                working,
                format!("ERR notice: message {working} is work in flight"),
            ),
            (
                ask,
                format!("ERR notice: message {ask} asks; answer it with notice act"),
            ),
            (wire, format!("ERR notice: no live message {wire}")),
        ] {
            let a = notice(&mut c, &mut g, &format!("dismiss {id}"), now);
            assert_eq!((a.reply, a.paint, a.note), (want, Paint::None, false));
        }
        assert_eq!(
            notice(&mut c, &mut g, "dismiss 999", now).reply,
            "ERR notice: no live message 999"
        );
        assert_eq!(c.revision(), revision);
        assert!(c.live(working).is_some() && c.live(ask).is_some());
    }

    #[test]
    fn resolve_press_names_indices_labels_and_details() {
        let now = t0();
        let (mut c, mut g) = fresh(now);
        let id = c
            .post(
                Message::new(tags::PRIVACY, Severity::Info, "File access not confirmed")
                    .action(Intent::OpenSystemPane {
                        pane: "full-disk-access".into(),
                    })
                    .action(Intent::NotNow {
                        decision: Decision::FileAccess,
                    })
                    .hold(Hold::Ask {
                        for_: Duration::from_secs(600),
                    }),
                stamp(1),
                now,
            )
            .id;
        let row = c.live(id).unwrap();
        for (press, want) in [
            (Press::Index(0), Some(ActionIndex(0))),
            (Press::Index(1), Some(ActionIndex(1))),
            (Press::Label("Open Settings".into()), Some(ActionIndex(0))),
            (Press::Label("Not now".into()), Some(ActionIndex(1))),
            (Press::Details, Some(ActionIndex::DETAILS)),
            (Press::Label("not now".into()), None),
            (Press::Label("Not".into()), None),
        ] {
            assert_eq!(resolve_press(row, &press), want, "{press:?}");
        }
        let one = c
            .post(
                Message::new(tags::SYSTEM, Severity::Warn, "One capsule").action(Intent::NewWindow),
                stamp(1),
                now,
            )
            .id;
        assert_eq!(resolve_press(c.live(one).unwrap(), &Press::Index(1)), None);
        assert_eq!(
            press_target(
                &c,
                &mut g,
                id,
                &Press::Label("Not now".into()),
                &pct_encode,
                now
            ),
            Ok((ActionIndex(1), "Not now"))
        );
        assert_eq!(
            press_target(&c, &mut g, id, &Press::Details, &pct_encode, now),
            Ok((ActionIndex::DETAILS, Intent::Details.label()))
        );
        assert_eq!(
            press_target(
                &c,
                &mut g,
                id,
                &Press::Label("not now".into()),
                &pct_encode,
                now
            ),
            Err(format!("ERR notice: no action not%20now on message {id}"))
        );
        assert_eq!(
            press_target(&c, &mut g, one, &Press::Index(1), &pct_encode, now),
            Err(format!("ERR notice: no action 1 on message {one}"))
        );
        let gone = MessageId::from_raw(99).unwrap();
        assert_eq!(
            press_target(&c, &mut g, gone, &Press::Details, &pct_encode, now),
            Err("ERR notice: no live message 99".to_string())
        );
        assert_eq!(press_words(&Press::Details), "details");
        assert_eq!(
            acted_reply("Not now", true, &pct_encode),
            "OK acted=Not%20now performed=1"
        );
        assert_eq!(
            acted_reply("Install now", false, &pct_encode),
            "OK acted=Install%20now performed=0"
        );
        // `apply` never presses: that is the host's page press.
        let a = notice(&mut c, &mut g, &format!("act {id} 1"), now);
        assert_eq!((a.reply.as_str(), a.paint), (ACT_IS_THE_HOSTS, Paint::None));
        assert!(c.live(id).is_some());
    }

    /// Review (2026-09-24, design ruling 195): a loop of `act <id> details`
    /// on a row that stays live gets the budget's presses and then `ERR
    /// busy`, never a repaint, a window and a log line per line. A refused
    /// press (no row, no capsule) spends nothing.
    #[test]
    fn notice_act_presses_are_budgeted() {
        let now = t0();
        let (mut c, mut g) = fresh(now);
        let row = id_of(&notice(
            &mut c,
            &mut g,
            "progress build Building aterm",
            now,
        ));
        let gone = MessageId::from_raw(999).unwrap();
        for _ in 0..20 {
            assert!(press_target(&c, &mut g, gone, &Press::Details, &pct_encode, now).is_err());
            assert!(press_target(&c, &mut g, row, &Press::Index(0), &pct_encode, now).is_err());
        }
        let mut pressed = 0;
        let mut busy = Vec::new();
        for i in 0..100u64 {
            match press_target(&c, &mut g, row, &Press::Details, &pct_encode, now + ms(i)) {
                Ok(_) => pressed += 1,
                Err(line) => busy.push(line),
            }
        }
        assert_eq!(pressed, WIRE_PRESSES_PER_WINDOW);
        assert_eq!(busy.len(), 100 - WIRE_PRESSES_PER_WINDOW);
        assert_eq!(
            busy[0],
            format!(
                "ERR busy notice: {WIRE_PRESSES_PER_WINDOW} presses a minute retry_ms={}",
                60_000 - WIRE_PRESSES_PER_WINDOW
            )
        );
        // The window slides: the first press leaves at 60 s.
        let later = now + Duration::from_secs(60);
        assert!(press_target(&c, &mut g, row, &Press::Details, &pct_encode, later).is_ok());
        assert_eq!(no_live_reply(gone), "ERR notice: no live message 999");
    }

    /// Review (2026-09-24, design ruling 192): a wire post never folds into
    /// a host row, and a host post never into a wire row, whatever their
    /// words — the wire can never restate a host row (ruling 167).
    #[test]
    fn wire_and_host_rows_never_merge_as_duplicates() {
        let now = t0();
        let (mut c, mut g) = fresh(now);
        let host = c
            .post(
                Message::new(tags::SESSION, Severity::Error, "Keystrokes dropped"),
                stamp(1),
                now,
            )
            .id;
        onto_glass(&mut c, now);
        let before = c.live(host).unwrap().clone();
        let later = now + Duration::from_secs(5);
        let w = notice(
            &mut c,
            &mut g,
            "post session sev=error Keystrokes dropped",
            later,
        );
        assert_ne!(id_of(&w), host, "a new wire row, not the host's");
        assert_eq!(c.live(id_of(&w)).unwrap().msg.origin, Origin::Wire);
        let after = c.live(host).unwrap();
        assert_eq!(after.repeats, before.repeats, "the host row's count");
        assert_eq!(after.fold_at, before.fold_at, "the host row's hold");
        // The reverse: a script plants the words first, then aterm's own
        // report arrives — it is a host row, never counted as the script's.
        let (mut c, mut g) = fresh(now);
        let planted = id_of(&notice(
            &mut c,
            &mut g,
            "post privacy sev=warn Allow file access",
            now,
        ));
        let real = c.post(
            Message::new(tags::PRIVACY, Severity::Warn, "Allow file access"),
            stamp(2),
            now,
        );
        assert_ne!(real.id, planted);
        assert_eq!(c.live(real.id).unwrap().msg.origin, Origin::Host);
        assert_eq!(c.live(planted).unwrap().repeats, 1);
        // Two wire posts of the same words still fold.
        let again = notice(
            &mut c,
            &mut g,
            "post privacy sev=warn Allow file access",
            now,
        );
        assert_eq!(id_of(&again), planted);
    }

    /// Review (2026-09-24, design ruling 194): the mints' title and detail
    /// bytes are budgeted, so full-detail records cannot rotate
    /// `messages.log` over aterm's own lines in minutes.
    #[test]
    fn the_wire_budgets_the_bytes_it_writes() {
        let now = t0();
        let (mut c, mut g) = fresh(now);
        let detail = vec!["x".repeat(200); 10].join("\\n");
        let line = |i: u64| format!("post system note {i:02} -- {detail}");
        let each = words_bytes(
            &PostRequest::parse(&line(0)["post ".len()..])
                .unwrap()
                .into_message(),
        );
        let fits = WIRE_BYTES_PER_WINDOW / each;
        assert!(fits < WIRE_MINTS_PER_WINDOW, "the bytes bind first");
        for i in 0..fits as u64 {
            let a = notice(&mut c, &mut g, &line(i), now + ms(i));
            assert!(a.reply.starts_with("OK recorded"), "{i}: {}", a.reply);
        }
        let logged = c.log().len();
        let busy = notice(&mut c, &mut g, &line(99), now + ms(100));
        assert_eq!(
            busy.reply,
            format!(
                "ERR busy notice: {} KiB of words a minute retry_ms=59900",
                WIRE_BYTES_PER_WINDOW / 1024
            )
        );
        assert_eq!(c.log().len(), logged, "a refusal writes nothing");
        // A short line still fits where the room is.
        assert!(
            notice(&mut c, &mut g, "post system ok", now + ms(100))
                .reply
                .starts_with("OK recorded")
        );
    }

    #[test]
    fn message_rows_carry_origin_busy_and_load() {
        let now = t0();
        let (mut c, mut g) = fresh(now);
        let busy = id_of(&notice(
            &mut c,
            &mut g,
            "progress fetch load=network Fetching the index -- 3 MB",
            now,
        ));
        let fill = id_of(&notice(
            &mut c,
            &mut g,
            "progress build pct=42.5 Building aterm",
            now,
        ));
        let rec = id_of(&notice(
            &mut c,
            &mut g,
            "post fabric peer said hi -- line",
            now,
        ));
        let on = now + PROGRESS_GRACE;
        onto_glass(&mut c, on);
        let rows = message_rows(&c, &ReadQuery::default(), 3_000, &pct_encode);
        assert_eq!(
            rows,
            vec![
                format!(
                    "message {busy} at=1000 ago_ms=2000 tag=system sev=info origin=wire state=live glass=0 rep=1 key=wire.fetch title=Fetching%20the%20index detail= actions=- busy=1 load=network"
                ),
                format!(
                    "message {fill} at=1000 ago_ms=2000 tag=system sev=info origin=wire state=live glass=1 rep=1 key=wire.build title=Building%20aterm detail= actions=- progress=43/100"
                ),
                format!(
                    "message {rec} at=1000 ago_ms=2000 tag=fabric sev=info origin=wire state=recorded glass=- rep=1 key=- title=peer%20said%20hi detail=line actions=- since_ms=2000"
                ),
            ]
        );
        // load= only while the row SHOWS its load words: a load a restate
        // declares after none waits LOAD_AFTER.
        notice(
            &mut c,
            &mut g,
            "progress build pct=50 load=disk Building aterm",
            on,
        );
        let row = |c: &MessageCenter| {
            message_rows(c, &parse_read_args("live").unwrap(), 9_000, &pct_encode)
                .into_iter()
                .find(|r| r.starts_with(&format!("message {fill} ")))
                .unwrap()
        };
        assert!(row(&c).ends_with(" progress=50/100"), "{}", row(&c));
        let shown = on + crate::LOAD_AFTER;
        c.settle(shown, true);
        assert!(
            row(&c).ends_with(" progress=50/100 load=disk"),
            "{}",
            row(&c)
        );
        notice(&mut c, &mut g, "done build withdraw", shown);
        let q = parse_read_args(&format!("since={}", fill.raw() - 1)).unwrap();
        let withdrawn = message_rows(&c, &q, 9_000, &pct_encode);
        assert!(
            withdrawn[0].contains(" origin=wire state=withdrawn glass=- "),
            "{withdrawn:?}"
        );
        // The query: live keeps unretired rows; sev= is a floor; tag= filters.
        notice(&mut c, &mut g, "post system sev=warn Deploy failed", shown);
        let live = message_rows(&c, &parse_read_args("live").unwrap(), 9_000, &pct_encode);
        assert_eq!(live.len(), 2, "{live:?}");
        let warn = message_rows(
            &c,
            &parse_read_args("sev=warn").unwrap(),
            9_000,
            &pct_encode,
        );
        assert_eq!(warn.len(), 1);
        assert!(
            warn[0].contains(" sev=warn origin=wire state=held "),
            "{}",
            warn[0]
        );
        let fabric = message_rows(
            &c,
            &parse_read_args("tag=fabric").unwrap(),
            9_000,
            &pct_encode,
        );
        assert_eq!(fabric.len(), 1);
        let newest = message_rows(&c, &parse_read_args("1").unwrap(), 9_000, &pct_encode);
        assert!(newest[0].contains("title=Deploy%20failed"), "{newest:?}");
    }

    #[test]
    fn no_capsule_is_ever_authored_from_the_wire() {
        for line in [
            "post system Not now",
            "post system sev=warn key=a Install now",
            "progress k Open Settings",
            "progress k pct=5 load=cpu Install now -- Not now",
        ] {
            match NoticeRequest::parse(line).unwrap() {
                NoticeRequest::Post(p) => assert!(p.into_message().actions.is_empty()),
                NoticeRequest::Progress(p) => {
                    let r = p.restatement();
                    assert_eq!(
                        r,
                        Restatement {
                            title: Some(p.title.clone()),
                            meter: Some(Some(p.meter())),
                            ..Restatement::default()
                        },
                        "a wire restatement sets the title and the meter only"
                    );
                    assert!(p.into_message().actions.is_empty());
                }
                other => panic!("{other:?}"),
            }
        }
    }
}
