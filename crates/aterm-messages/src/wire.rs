// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Wire grammars for `messages`, the `appstatus` compatibility face, and the `notice` verb.

use std::collections::VecDeque;
use std::fmt::Write as _;

use crate::center::{Glass, Live, MessageCenter, Outcome, PostOutcome};
use crate::log::{LogRecord, MessageLog, Retired};
use crate::model::{
    ActionIndex, Amount, Glyph, Hold, Intent, Load, Message, MessageId, Meter, Origin, Restatement,
    Severity, Tag, Unit, WallStamp, tags,
};
use crate::need::{
    DIGEST_CAP, Fingerprint, LaterAsk, Mover, REASON_CAP, Reason, Resurface, STEP_CAP, Then, Why,
};
use crate::text::{clip, glass_title_fault, is_stripped};
use crate::{
    DETAIL_LINE_CAP, DETAIL_LINES_CAP, Duration, Instant, LOG_CAP, MAX_ACTIONS, PROGRESS_GRACE,
    STALE_WIRE, STATS_CAP, TITLE_CAP, WIRE_BYTES_PER_WINDOW, WIRE_KEY_MAX, WIRE_KEY_PREFIX,
    WIRE_LATER_CAP, WIRE_LIVE_CAP, WIRE_MINT_WINDOW, WIRE_MINTS_PER_WINDOW, WIRE_PAINT_GAP,
    WIRE_PRESSES_PER_WINDOW,
};

/// The `messages` verb's usage line.
pub(crate) const READ_USAGE: &str =
    "usage: messages [<n>] [since=<id>] [tag=<tag>] [sev=<sev>] [live]";
/// The `notice post` usage line.
pub(crate) const POST_USAGE: &str =
    "usage: notice post <tag> [sev=<sev>] [key=<key>] [hold=<1..120>] <title>[ -- <detail>]";
/// The longest `hold=` a wire post may ask for, in seconds.
pub(crate) const MAX_WIRE_HOLD_SECS: u32 = 120;
/// The most detail bytes a wire post carries.
pub(crate) const WIRE_DETAIL_BYTES: usize = 2048;
/// The default page of a `messages` read.
pub(crate) const DEFAULT_READ_N: usize = 64;

/// `messages [<n>] [since=<id>] [tag=<tag>] [sev=<sev>] [live]`.
#[derive(Clone, Debug, PartialEq, Eq, Default)]
pub struct ReadQuery {
    /// The page size as given (at most [`LOG_CAP`]); `None` is the default for the query's shape.
    pub n: Option<usize>,
    /// Only ids strictly greater.
    pub since: Option<MessageId>,
    /// Only this tag.
    pub tag: Option<Tag>,
    /// A floor: `sev=warn` is warn + error.
    pub min_severity: Option<Severity>,
    /// Only unretired rows.
    pub live_only: bool,
    /// Only exceptions (`need=later`): the digest's entries, open and filed.
    pub later_only: bool,
}

impl ReadQuery {
    /// How many rows a page holds: `n` when given.
    #[must_use]
    pub(crate) fn page(&self) -> usize {
        self.n.unwrap_or(if self.since.is_some() {
            LOG_CAP
        } else {
            DEFAULT_READ_N
        })
    }

    /// The records the query selects, ascending by id.
    #[must_use]
    pub(crate) fn select<'a>(&self, log: &'a MessageLog) -> Vec<&'a LogRecord> {
        let mut picked: Vec<&LogRecord> = log
            .records()
            .filter(|r| self.since.is_none_or(|s| r.id > s))
            .filter(|r| self.tag.as_ref().is_none_or(|t| r.tag == *t))
            .filter(|r| self.min_severity.is_none_or(|s| r.severity >= s))
            .filter(|r| !self.live_only || (r.is_live() && (!r.later || self.later_only)))
            .filter(|r| !self.later_only || r.later)
            .collect();
        picked.sort_by_key(|r| r.id);
        if self.since.is_some() {
            picked.truncate(self.page());
            return picked;
        }
        let skip = picked.len().saturating_sub(self.page());
        picked.into_iter().skip(skip).collect()
    }
}

/// Parse the words after `messages`; junk ⇒ `Err(READ_USAGE)`.
/// # Errors
/// [`READ_USAGE`] for any token the grammar does not name, a zero count, a bad id, tag or severity.
pub fn parse_read_args(rest: &str) -> Result<ReadQuery, &'static str> {
    let mut q = ReadQuery::default();
    for word in rest.split_whitespace() {
        if word == "live" {
            q.live_only = true;
        } else if let Some(n) = digits::<usize>(word) {
            // Plain digits, as everywhere on the wire: `+5` is a usage error.
            if n == 0 {
                return Err(READ_USAGE);
            }
            q.n = Some(n.min(LOG_CAP));
        } else if let Some(v) = word.strip_prefix("since=") {
            q.since = Some(wire_id(v).ok_or(READ_USAGE)?);
        } else if let Some(v) = word.strip_prefix("tag=") {
            q.tag = Some(Tag::try_new(v).map_err(|_| READ_USAGE)?);
        } else if let Some(v) = word.strip_prefix("sev=") {
            q.min_severity = Some(Severity::parse(v).ok_or(READ_USAGE)?);
        } else if word == "need=later" {
            q.later_only = true;
        } else {
            return Err(READ_USAGE);
        }
    }
    Ok(q)
}

fn progress_word(fill: Option<u16>) -> String {
    fill.map_or_else(
        || "-".to_string(),
        |p| format!("{}/100", (p.min(1000) + 5) / 10),
    )
}

/// The `state=` word of a record.
#[must_use]
pub const fn state_word(rec: &LogRecord, live: Option<&Live>) -> &'static str {
    match (live, rec.retired()) {
        (Some(l), _) => {
            if l.msg.hold.is_held() {
                "held"
            } else {
                "live"
            }
        }
        (None, Some(how)) => how.as_word(),
        (None, None) if rec.later => "review",
        (None, None) => "live",
    }
}

/// One `messages` row: `message <id> at=<unix_ms> ago_ms=<ms> tag=<tag> sev=<sev> origin=<host|wire|carried> state=<…> [glass=<row|->] rep=<n> key=<pct|-> title=<pct> detail=<pct> actions=<pct|-> [progress=<n>/100|busy=1] [load=<network|disk|cpu>] [reveal=pending] [since_ms=<ms>] [need=<class>]`.
/// `glass=` says a band host's band row ([`Glass::Band`]); a glass-less host's rows omit it.
#[must_use]
pub(crate) fn message_row(
    rec: &LogRecord,
    live: Option<&Live>,
    glass: Glass,
    glass_row: Option<usize>,
    now_unix_ms: u64,
    enc: &dyn Fn(&str) -> String,
) -> String {
    let state = state_word(rec, live);
    let (title, detail, actions, repeats) = match live {
        Some(l) => (&l.msg.title, &l.msg.detail, &l.msg.actions, l.repeats),
        None => (&rec.title, &rec.detail, &rec.actions, rec.repeats),
    };
    let labels: Vec<&str> = actions
        .iter()
        .filter(|intent| live.is_some() || rec.still_offers(intent))
        .map(Intent::label)
        .collect();
    let ago_ms = now_unix_ms.saturating_sub(rec.stamp.unix_ms);
    let mut row = format!(
        "message {} at={} ago_ms={ago_ms} tag={} sev={} origin={} state={state}{} rep={repeats} key={} title={} detail={} actions={}",
        rec.id,
        rec.stamp.unix_ms,
        rec.tag,
        rec.severity.as_str(),
        rec.origin.as_str(),
        match (glass, glass_row) {
            (Glass::Glassless, _) => String::new(),
            (Glass::Band, Some(r)) => format!(" glass={r}"),
            (Glass::Band, None) => " glass=-".to_string(),
        },
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
        if let Some(fill) = l.msg.meter.as_ref().and_then(|m| m.fill_permille) {
            row.push_str(" progress=");
            row.push_str(&(fill.min(1000) / 10).to_string());
            row.push_str("/100");
        } else if l.is_busy() {
            row.push_str(" busy=1");
        }
        if let Some(load) = l.said_load() {
            row.push_str(" load=");
            row.push_str(load.word());
        }
        if !l.revealed {
            row.push_str(" reveal=pending");
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
    let class = match live {
        Some(l) => match l.msg.need() {
            crate::need::Need::Now(ask) if ask.is_adopted() => crate::need::Class::Unclassified,
            need => need.class(),
        },
        None if rec.later => crate::need::Class::Later,
        None => crate::need::Class::Unclassified,
    };
    if class != crate::need::Class::Unclassified {
        row.push_str(" need=");
        row.push_str(class.as_str());
    }
    row
}

/// Every `messages` row the query selects, ascending by id, in [`message_row`]'s grammar.
/// Open digest rows also carry `rev=<confirmation>` for conditional settlement:
/// an opaque entry-local observation, changed by every accepted positive report,
/// even an identical repeat. It is independent of the digest's paint revision.
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
            if live.is_none()
                && rec.later
                && rec.is_live()
                && let Some(e) = center.digest_entry(rec.id)
            {
                let mut now_words = rec.clone();
                now_words.title = e.title;
                now_words.detail = e.detail;
                now_words.actions = e.actions;
                now_words.repeats = e.repeats;
                let mut row = message_row(&now_words, None, center.glass(), None, now_unix_ms, enc);
                if let Some(confirmation) = center.digest_confirmation(rec.id) {
                    let _ = write!(row, " rev={confirmation}");
                }
                return row;
            }
            let row = live.and_then(|_| center.glass_position(rec.id));
            message_row(rec, live, center.glass(), row, now_unix_ms, enc)
        })
        .collect()
}

/// The `appstatus` body in the retired status bars' byte-identical grammar.
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

/// The `notice` verb's usage line: a missing or unknown sub-form.
pub(crate) const NOTICE_USAGE: &str =
    "usage: notice <post|progress|done|settle|ask|later|dismiss|act> \u{2026}";
/// The `notice progress` usage line.
pub(crate) const PROGRESS_USAGE: &str = "usage: notice progress <key> [tag=<tag>] [pct=<0..100>|done=<n>/<total>|busy] [unit=<bytes|items|steps>] [load=<network|disk|cpu>] <title>[ -- <stats>]";
/// The `notice done` usage line.
pub(crate) const DONE_USAGE: &str = "usage: notice done <key> [ok|warn|withdraw] [<words>]";
/// The `notice settle` usage line.
pub(crate) const SETTLE_USAGE: &str = "usage: notice settle <key> <id> <revision>";
/// The `notice ask` usage line.
pub(crate) const ASK_USAGE: &str =
    "usage: notice ask <key> [tag=<tag>] <title> -- <reason> -- <step>";
/// The `notice later` usage line.
pub(crate) const LATER_USAGE: &str = "usage: notice later <key> [tag=<tag>] [url=<https-url>] <title> -- <reason> -- <step>[ -- <detail>]";
/// A `url=` the wire refuses ([`crate::model::link_host`]).
pub(crate) const URL_REFUSAL: &str = "notice: url is https://<host>[/<path>] with no user and no port, at most 512 printable characters";
/// A `notice later` reason past [`REASON_CAP`]: the digest shows it whole or not at all.
pub(crate) const REASON_LONG: &str = "notice: reason over 90 characters";
/// A `notice later` step past [`STEP_CAP`].
pub(crate) const STEP_LONG: &str = "notice: step over 90 characters";
/// A `notice later` detail past [`crate::exceptions::MORE_LINES`] lines of [`DETAIL_LINE_CAP`] characters.
pub(crate) const DETAIL_LONG: &str = "notice: detail over 3 lines (split by \\n) of 240 characters";
const _: () = assert!(
    REASON_CAP == 90
        && STEP_CAP == 90
        && crate::exceptions::MORE_LINES == 3
        && DETAIL_LINE_CAP == 240
        && crate::model::LINK_URL_CAP == 512
);
/// The `notice dismiss` usage line.
pub(crate) const DISMISS_USAGE: &str = "usage: notice dismiss <id|review>";
/// The `notice act` usage line.
pub(crate) const ACT_USAGE: &str = "usage: notice act <id> <label|index|details>";
/// A key outside the wire's rule.
pub(crate) const KEY_REFUSAL: &str =
    "notice: keys are 1-40 of a-z 0-9 . _ - (they live as wire.<key>)";
/// A tag of aterm's own families.
pub(crate) const OWN_TAG_REFUSAL: &str =
    "notice: that tag is aterm's own; post under script or a tag of your own";
/// [`apply`] handed an `act`: a press is the host's to perform.
pub(crate) const ACT_IS_THE_HOSTS: &str = "ERR notice: act is pressed by the host";

fn aterms_own(tag: &Tag) -> bool {
    *tag != tags::SCRIPT && tags::ALL.contains(&tag)
}

fn title_refusal(fault: &'static str, tail: bool) -> &'static str {
    match (fault, tail) {
        ("a glass title over six words", true) => {
            "notice: title over 6 words; put the rest after --"
        }
        ("a glass title over six words", false) => "notice: title over 6 words",
        ("a glass title over 48 characters", true) => {
            "notice: title over 48 characters; put the rest after --"
        }
        ("a glass title over 48 characters", false) => "notice: title over 48 characters",
        ("a clause in a glass title", true) => {
            "notice: title has a clause (\u{2014}, ;, :); put it after --"
        }
        ("a clause in a glass title", false) => "notice: title has a clause (\u{2014}, ;, :)",
        ("a sentence for a glass title", _) => "notice: title ends with a period",
        _ => "notice: title out of form",
    }
}

fn title_form(title: &str, tail: bool) -> Result<(), &'static str> {
    glass_title_fault(title).map_or(Ok(()), |f| Err(title_refusal(f, tail)))
}

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

fn wire_tag(word: &str, usage: &'static str) -> Result<Tag, &'static str> {
    let tag = Tag::try_new(word).map_err(|_| usage)?;
    if aterms_own(&tag) {
        return Err(OWN_TAG_REFUSAL);
    }
    Ok(tag)
}

fn first_word(s: &str) -> (&str, &str) {
    let s = s.trim_start();
    s.split_once(char::is_whitespace).unwrap_or((s, ""))
}

fn digits<T: std::str::FromStr>(s: &str) -> Option<T> {
    (!s.is_empty() && s.bytes().all(|b| b.is_ascii_digit()))
        .then(|| s.parse().ok())
        .flatten()
}

fn wire_id(s: &str) -> Option<MessageId> {
    digits::<u64>(s).and_then(MessageId::from_raw)
}

fn split_title(rest: &str) -> (&str, &str) {
    let rest = rest.trim_start();
    if rest.trim_end() == "--" {
        return ("", "");
    }
    if let Some(tail) = rest.strip_prefix("-- ") {
        return ("", tail.trim());
    }
    let (title, tail) = rest
        .split_once(" -- ")
        .or_else(|| rest.trim_end().strip_suffix(" --").map(|t| (t, "")))
        .unwrap_or((rest, ""));
    (title.trim(), tail.trim())
}

fn clip_words(s: &str, cap: usize) -> String {
    clip(clip(s, usize::MAX).trim(), cap)
}

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

/// `notice post <tag> [sev=<sev>] [key=<key>] [hold=<1..120>] <title>[ -- <detail>]`.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct PostRequest {
    /// `script` or any tag of the script's own.
    pub tag: Tag,
    /// Defaults to `info`.
    pub severity: Severity,
    /// The full key, `wire.<key>`.
    pub key: Option<String>,
    /// Seconds, 1..=120 (a script cannot say why the owner is needed, so it cannot hold a row longer than a glance).
    pub hold_secs: Option<u32>,
    /// ≤ [`TITLE_CAP`] chars; a `warn`/`error` title obeys the glass title form.
    pub title: String,
    /// ≤ [`WIRE_DETAIL_BYTES`], split on a literal `\n`, each line clipped to [`DETAIL_LINE_CAP`], ≤ [`DETAIL_LINES_CAP`] lines.
    pub detail: Vec<String>,
}

impl PostRequest {
    /// Parse the words after `notice post`.
    /// # Errors
    /// [`POST_USAGE`] for a missing or invalid tag, an unknown severity, a hold outside `1..=120`, or an empty title.
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
        let title = clip_words(title, TITLE_CAP);
        if title.is_empty() {
            return Err(POST_USAGE);
        }
        if severity >= Severity::Warn {
            title_form(&title, true)?;
        }
        let mut end = detail.len().min(WIRE_DETAIL_BYTES);
        while !detail.is_char_boundary(end) {
            end -= 1;
        }
        let detail: Vec<String> = detail[..end]
            .split("\\n")
            .map(|l| clip_words(l, DETAIL_LINE_CAP))
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

    /// `true` for a record: `success` and `info` go to the log, never the glass.
    #[must_use]
    pub(crate) fn is_record(&self) -> bool {
        self.severity <= Severity::Info
    }

    /// The message: `Origin::Wire`, the severity's glyph, no actions ever.
    #[must_use]
    pub(crate) fn into_message(self) -> Message {
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

fn parse_done(v: &str) -> Option<(u64, u64)> {
    let (n, total) = v.split_once('/')?;
    let (n, total) = (digits::<u64>(n)?, digits::<u64>(total)?);
    (total >= 1).then_some((n, total))
}

/// `notice progress <key> [tag=<tag>] [pct=<0..100>|done=<n>/<total>|busy] [unit=<bytes|items|steps>] [load=<network|disk|cpu>] <title>[ -- <stats>]`.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ProgressRequest {
    /// The full key, `wire.<key>`.
    pub key: String,
    /// `script` by default; never aterm's own families.
    pub tag: Tag,
    /// The fill, the amount, or busy.
    pub indicator: WireIndicator,
    /// A very heavy phase's resource.
    pub load: Option<Load>,
    /// The glass title form, always.
    pub title: String,
    /// ≤ [`STATS_CAP`] chars.
    pub stats: String,
}

impl ProgressRequest {
    /// Parse the words after `notice progress`.
    /// # Errors
    /// [`PROGRESS_USAGE`] for a missing key, a bad option, two indicators, `unit=` without `done=`, or an empty title.
    pub fn parse(rest: &str) -> Result<Self, &'static str> {
        let (key, rest) = first_word(rest);
        if key.is_empty() {
            return Err(PROGRESS_USAGE);
        }
        let key = wire_key(key)?;
        let mut tag = tags::SCRIPT;
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
                load = Some(Load::parse(v).ok_or(PROGRESS_USAGE)?);
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
        let title = clip_words(title, TITLE_CAP);
        if title.is_empty() {
            return Err(PROGRESS_USAGE);
        }
        title_form(&title, true)?;
        Ok(Self {
            key,
            tag,
            indicator,
            load,
            title,
            stats: clip_words(stats, STATS_CAP),
        })
    }

    /// The meter the line declares, normalized.
    #[must_use]
    pub fn meter(&self) -> Meter {
        let m = match self.indicator {
            WireIndicator::Fill(p) => Meter {
                fill_permille: Some(p),
                amount: Some(Amount {
                    series: Amount::series_of(&self.key),
                    done: u64::from(p),
                    total: 1000,
                    unit: Unit::Steps,
                }),
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

    /// A new row: Info, `Origin::Wire`, keyed, `Hold::Live { STALE_WIRE }`, revealed only after [`PROGRESS_GRACE`] (a job done inside it never flashes), with its meter.
    #[must_use]
    pub(crate) fn into_message(self) -> Message {
        let meter = self.meter();
        Message::new(self.tag, Severity::Info, self.title)
            .glyph(Glyph::or_fallback('\u{21bb}'))
            .key(&self.key)
            .origin(Origin::Wire)
            .hold(Hold::Live {
                stale_after: STALE_WIRE,
            })
            .reveal_after(PROGRESS_GRACE)
            .loads(self.load)
            .meter(meter)
    }

    /// The in-place change a later line makes: the title and the meter, nothing else.
    #[must_use]
    pub(crate) fn restatement(&self) -> Restatement {
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
    pub const fn retired(self) -> Retired {
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
    /// The full key, `wire.<key>`.
    pub key: String,
    /// `ok` by default.
    pub how: DoneHow,
    /// The finishing words (the glass title form), re-titling the row first.
    pub words: Option<String>,
}

impl DoneRequest {
    /// Parse the words after `notice done`.
    /// # Errors
    /// [`DONE_USAGE`] for a missing key or an unknown outcome word; [`KEY_REFUSAL`]; `notice: <fault>` for words out of the title form.
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
        let words = clip_words(words, TITLE_CAP);
        if !words.is_empty() {
            title_form(&words, false)?;
        }
        Ok(Self {
            key,
            how,
            words: (!words.is_empty()).then_some(words),
        })
    }
}

/// `notice ask <key> [tag=<tag>] <title> -- <reason> -- <step>`.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct AskRequest {
    /// The full key, `wire.<key>`.
    pub key: String,
    /// `script` by default; never aterm's own families.
    pub tag: Tag,
    /// The glass title form.
    pub title: String,
    /// Why only the owner can close it, ≤ [`crate::need::REASON_CAP`].
    pub reason: String,
    /// The one step that closes it, ≤ [`crate::need::STEP_CAP`].
    pub step: String,
}

impl AskRequest {
    /// Parse the words after `notice ask`.
    /// # Errors
    /// [`ASK_USAGE`] for a missing key, title, reason or step, or a bad option.
    pub fn parse(rest: &str) -> Result<Self, &'static str> {
        let (key, rest) = first_word(rest);
        if key.is_empty() {
            return Err(ASK_USAGE);
        }
        let key = wire_key(key)?;
        let mut tag = tags::SCRIPT;
        let rest = options(rest, |word| match word.strip_prefix("tag=") {
            Some(v) => {
                tag = wire_tag(v, ASK_USAGE)?;
                Ok(true)
            }
            None => Ok(false),
        })?;
        let (title, tail) = split_title(rest);
        let (reason, step) = split_title(tail);
        let title = clip_words(title, TITLE_CAP);
        let reason = clip_words(reason, crate::need::REASON_CAP);
        let step = clip_words(step, crate::need::STEP_CAP);
        if title.is_empty() || reason.is_empty() || step.is_empty() {
            return Err(ASK_USAGE);
        }
        title_form(&title, true)?;
        let ask = Self::ask_of(&reason, &step);
        if let Some(fault) = ask.fault() {
            return Err(ask_refusal(fault, ASK_USAGE));
        }
        Ok(Self {
            key,
            tag,
            title,
            reason,
            step,
        })
    }

    fn ask_of(reason: &str, step: &str) -> crate::need::Ask {
        crate::need::Ask::standing(
            crate::need::StandingWhy::OnlyYou(crate::need::Reason::new(reason)),
            crate::need::Then::step(step),
        )
    }

    /// The row: Warn, `Origin::Wire`, keyed, classified now and standing.
    #[must_use]
    pub(crate) fn into_message(self) -> Message {
        let ask = Self::ask_of(&self.reason, &self.step);
        Message::new(self.tag, Severity::Warn, self.title)
            .key(&self.key)
            .origin(Origin::Wire)
            .now(ask)
    }
}

/// `notice later <key> [tag=<tag>] [url=<https-url>] <title> -- <reason> -- <step>[ -- <detail>]`.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct LaterRequest {
    /// The full key, `wire.<key>`: the fact's identity, kept whole.
    pub key: String,
    /// `script` by default; never aterm's own families.
    pub tag: Tag,
    /// The page the one button opens ([`crate::model::link_host`] admits it).
    pub url: Option<String>,
    /// The glass title form.
    pub title: String,
    /// Why the owner, and not the machine, is needed: at most [`REASON_CAP`].
    pub reason: String,
    /// The one step that settles it: at most [`STEP_CAP`].
    pub step: String,
    /// At most [`crate::exceptions::MORE_LINES`] lines (split on a literal `\n`) of at most [`DETAIL_LINE_CAP`], behind `Details ›`.
    pub detail: Vec<String>,
}

impl LaterRequest {
    /// Parse the words after `notice later`.
    /// # Errors
    /// [`LATER_USAGE`] for a missing key, title, reason or step, a bad option or a second `url=`.
    pub fn parse(rest: &str) -> Result<Self, &'static str> {
        let (key, rest) = first_word(rest);
        if key.is_empty() {
            return Err(LATER_USAGE);
        }
        let key = wire_key(key)?;
        let mut tag = tags::SCRIPT;
        let mut url = None;
        let rest = options(rest, |word| {
            if let Some(v) = word.strip_prefix("tag=") {
                tag = wire_tag(v, LATER_USAGE)?;
            } else if let Some(v) = word.strip_prefix("url=") {
                crate::model::link_host(v).ok_or(URL_REFUSAL)?;
                if url.replace(v.to_string()).is_some() {
                    return Err(LATER_USAGE);
                }
            } else {
                return Ok(false);
            }
            Ok(true)
        })?;
        let (title, tail) = split_title(rest);
        let (reason, tail) = split_title(tail);
        let (step, detail) = split_title(tail);
        let title = clip_words(title, TITLE_CAP);
        let (reason, step) = (clip_words(reason, usize::MAX), clip_words(step, usize::MAX));
        if title.is_empty() || reason.is_empty() || step.is_empty() {
            return Err(LATER_USAGE);
        }
        title_form(&title, true)?;
        if reason.chars().count() > REASON_CAP {
            return Err(REASON_LONG);
        }
        if step.chars().count() > STEP_CAP {
            return Err(STEP_LONG);
        }
        let detail: Vec<String> = detail
            .split("\\n")
            .map(|l| clip_words(l, usize::MAX))
            .filter(|l| !l.is_empty())
            .collect();
        if detail.len() > crate::exceptions::MORE_LINES
            || detail.iter().any(|l| l.chars().count() > DETAIL_LINE_CAP)
        {
            return Err(DETAIL_LONG);
        }
        let req = Self {
            key,
            tag,
            url,
            title,
            reason,
            step,
            detail,
        };
        if let Some(fault) = req.ask().fault() {
            return Err(ask_refusal(fault, LATER_USAGE));
        }
        Ok(req)
    }

    fn ask(&self) -> LaterAsk {
        let then = match self.url.as_deref().and_then(Mover::open_link) {
            Some(mover) => Then::press(mover, &self.step),
            None => Then::step(&self.step),
        };
        LaterAsk::person(
            Why::OnlyYou(Reason::new(&self.reason)),
            then,
            Fingerprint::wire(&self.key),
            Resurface::WAIT,
        )
    }

    /// The entry: Info, `Origin::Wire`, keyed by the ask, its reason and step leading the detail, then the host the button opens (aterm's words, so a link is never only the sender's say-so), then the sender's lines.
    #[must_use]
    pub(crate) fn into_message(self) -> Message {
        let ask = self.ask();
        let host = self
            .url
            .as_deref()
            .and_then(crate::model::link_host)
            .map(|h| format!("Opens a page on {}", h.to_ascii_lowercase()));
        Message::new(self.tag, Severity::Info, self.title)
            .later(ask)
            .origin(Origin::Wire)
            .lines(host)
            .lines(self.detail)
    }
}

fn ask_refusal(fault: &'static str, usage: &'static str) -> &'static str {
    match fault {
        "a reason that only says something is pending" => {
            "notice: the reason only says something is pending; say why only the owner can close it"
        }
        "a snooze is not an action" => {
            "notice: a snooze is not an action; name the step that closes it"
        }
        "two actions in one sentence" => {
            "notice: two actions in one step; name the one that closes it"
        }
        _ => usage,
    }
}

/// Which capsule `notice act` presses.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Press {
    /// An authored capsule by index, `0..MAX_ACTIONS`.
    Index(u8),
    /// An authored capsule by its full label ([`Intent::label`], exact).
    Label(String),
    /// The implicit `Details ›`.
    Details,
}

/// `notice <post|progress|done|settle|ask|later|dismiss|act> …`, parsed on the control thread.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum NoticeRequest {
    /// A record, or a failure row.
    Post(PostRequest),
    /// A script's work in flight.
    Progress(ProgressRequest),
    /// Its finish.
    Done(DoneRequest),
    /// Resolve the exact open digest entry observed at `revision`, without forgetting a filing.
    Settle {
        /// The full key, `wire.<key>`.
        key: String,
        /// The observed open entry.
        id: MessageId,
        /// The opaque confirmation from its `messages` row's `rev` field.
        revision: u64,
    },
    /// The owner is needed now: a standing ask.
    Ask(AskRequest),
    /// The owner is needed, but not now: one entry of the digest.
    Later(LaterRequest),
    /// Take a row down as a person could.
    Dismiss(MessageId),
    /// File every open exception into the log (`notice dismiss review`).
    DismissReview,
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
    /// # Errors
    /// [`NOTICE_USAGE`] for a missing or unknown sub-form; [`SETTLE_USAGE`] for an
    /// incomplete settlement observation or invalid id/revision; [`KEY_REFUSAL`] for
    /// an invalid settlement key. Other forms return their parser's refusal.
    pub fn parse(rest: &str) -> Result<Self, &'static str> {
        let spaced: String = rest
            .chars()
            .map(|c| {
                if c.is_whitespace() && is_stripped(c) {
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
            "settle" => {
                let mut words = rest.split_whitespace();
                let key = wire_key(words.next().ok_or(SETTLE_USAGE)?)?;
                let id = words.next().and_then(wire_id).ok_or(SETTLE_USAGE)?;
                let revision = words.next().and_then(digits::<u64>).ok_or(SETTLE_USAGE)?;
                if words.next().is_some() {
                    return Err(SETTLE_USAGE);
                }
                Ok(Self::Settle { key, id, revision })
            }
            "ask" => AskRequest::parse(rest).map(Self::Ask),
            "later" => LaterRequest::parse(rest).map(Self::Later),
            "dismiss" if rest.trim() == "review" => Ok(Self::DismissReview),
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
    /// Nothing painted changed (a record, an identical line, a line that ended nothing).
    None,
    /// Repaint now.
    Now,
    /// A paint is armed at [`WireGate::due`].
    Paced,
}

/// What [`apply`] did.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Applied {
    /// The whole reply line, without `\n`.
    pub reply: String,
    /// What the host paints.
    pub paint: Paint,
    /// Log one `aterm.log` line for it: a line that minted or ended a message.
    pub note: bool,
}

impl Applied {
    const fn quiet(reply: String) -> Self {
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
    /// The armed paced paint. The web host folds it into its messages deadline
    /// ([`crate::drive::state_deadline`]); the native host only takes it when a settle passes it
    /// ([`crate::drive::drain_and_pace`]).
    #[must_use]
    pub const fn due(&self) -> Option<Instant> {
        self.paint_due
    }

    /// `true` ⇒ paint now: the paced paint is due.
    pub fn take_due(&mut self, now: Instant) -> bool {
        match self.paint_due {
            Some(t) if now >= t => {
                self.painted(t);
                true
            }
            _ => false,
        }
    }

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

    fn refund(&mut self) {
        self.mints.pop_back();
    }

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

    const fn painted(&mut self, frame: Instant) {
        self.last_paint = Some(frame);
        self.paint_due = None;
    }
}

fn later(t: Instant, d: Duration) -> Instant {
    t.checked_add(d).unwrap_or(t)
}

fn is_wire(row: &Live) -> bool {
    row.msg.origin == Origin::Wire
        || row
            .msg
            .key
            .as_deref()
            .is_some_and(|k| k.starts_with(WIRE_KEY_PREFIX))
}

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

fn words_bytes(msg: &Message) -> usize {
    msg.title.len() + msg.detail.iter().map(String::len).sum::<usize>()
}

fn shown(center: &MessageCenter, gate: &mut WireGate, now: Instant, reply: String) -> Applied {
    gate.painted(center.frame_instant(now));
    Applied {
        reply,
        paint: Paint::Now,
        note: true,
    }
}

/// Apply every form but `act` (the host performs presses: [`press_target`]).
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
        NoticeRequest::Done(d) => apply_done(center, gate, d, stamp, now),
        NoticeRequest::Settle { key, id, revision } => {
            apply_settle(center, &key, id, revision, now)
        }
        NoticeRequest::Ask(a) => post_row(center, gate, a.into_message().normalized(), stamp, now),
        NoticeRequest::Later(l) => apply_later(center, gate, l, stamp, now),
        NoticeRequest::Dismiss(id) => apply_dismiss(center, gate, id, stamp, now),
        NoticeRequest::DismissReview => {
            let n = center.file_digest(stamp, now);
            Applied::quiet(format!("OK dismissed=review filed={n}"))
        }
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
    post_row(center, gate, msg, stamp, now)
}

fn post_row(
    center: &mut MessageCenter,
    gate: &mut WireGate,
    msg: Message,
    stamp: WallStamp,
    now: Instant,
) -> Applied {
    let under_key = msg
        .key
        .as_deref()
        .and_then(|k| center.live_by_key(k))
        .map(|l| l.id);
    let duplicate = center.live_rows().any(|l| l.duplicates(&msg));
    if !duplicate {
        if under_key.is_none() && live_wire(center) >= WIRE_LIVE_CAP {
            return cap_refusal();
        }
        if let Err(busy) = gate.try_mint(now, words_bytes(&msg)) {
            return busy_refusal(busy);
        }
    }
    let posted = center.post(msg, stamp, now);
    let reply = format!("OK message={}", posted.id);
    if posted.outcome == PostOutcome::Duplicate {
        if !duplicate {
            gate.refund();
        }
        let paint = gate.pace(center, now);
        return Applied {
            reply,
            paint,
            note: false,
        };
    }
    shown(center, gate, now, reply)
}

fn later_cap_refusal() -> Applied {
    Applied::quiet(format!(
        "ERR notice: {WIRE_LATER_CAP} wire entries are open for later; end one with notice done"
    ))
}

fn digest_full_refusal() -> Applied {
    Applied::quiet(format!(
        "ERR notice: the owner has {DIGEST_CAP} entries to look at; aterm's own keep their place"
    ))
}

fn apply_later(
    center: &mut MessageCenter,
    gate: &mut WireGate,
    l: LaterRequest,
    stamp: WallStamp,
    now: Instant,
) -> Applied {
    let msg = l.into_message().normalized();
    let Some(how) = center.later_outcome(&msg, stamp, now) else {
        return Applied::quiet(format!("ERR {LATER_USAGE}"));
    };
    let under_key = msg
        .key
        .as_deref()
        .is_some_and(|k| center.live_by_key(k).is_some());
    if how == PostOutcome::New {
        if center.digest_keys_under(WIRE_KEY_PREFIX).len() >= WIRE_LATER_CAP {
            return later_cap_refusal();
        }
        if center.digest_len() >= DIGEST_CAP {
            return digest_full_refusal();
        }
    }
    let spends = matches!(how, PostOutcome::New | PostOutcome::Restated);
    if spends && let Err(busy) = gate.try_mint(now, words_bytes(&msg)) {
        return busy_refusal(busy);
    }
    let posted = center.post(msg, stamp, now);
    let word = match posted.outcome {
        PostOutcome::New | PostOutcome::Superseded(_) => "new",
        PostOutcome::Restated => "restated",
        PostOutcome::Duplicate => "duplicate",
        PostOutcome::Suppressed => "quiet",
    };
    if spends && !matches!(posted.outcome, PostOutcome::New | PostOutcome::Restated) {
        gate.refund();
    }
    let reply = format!("OK later={} how={word}", posted.id);
    if posted.outcome == PostOutcome::New && under_key {
        return shown(center, gate, now, reply);
    }
    Applied {
        reply,
        paint: Paint::None,
        note: posted.outcome == PostOutcome::New,
    }
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

fn apply_settle(
    center: &mut MessageCenter,
    key: &str,
    id: MessageId,
    revision: u64,
    now: Instant,
) -> Applied {
    if revision == u64::MAX
        || center.digest_confirmation(id) != Some(revision)
        || center.digest_entry(id).is_none_or(|entry| entry.key != key)
    {
        return Applied::quiet("OK settled=- how=gone".to_string());
    }
    // The check and resolution share this exclusive center borrow. A dismissal,
    // replacement, repeat or restatement cannot intervene, and no filing is forgotten.
    let ended = center.end_digest_key(key, Retired::Resolved(Outcome::Ok), None, now);
    Applied {
        reply: ended.map_or_else(
            || "OK settled=- how=gone".to_string(),
            |ended| format!("OK settled={ended} how=resolved-ok"),
        ),
        paint: Paint::None,
        note: ended.is_some(),
    }
}

fn apply_done(
    center: &mut MessageCenter,
    gate: &mut WireGate,
    d: DoneRequest,
    stamp: WallStamp,
    now: Instant,
) -> Applied {
    let Some(id) = center.live_by_key(&d.key).map(|l| l.id) else {
        let how = d.how.retired();
        let word = how.as_word();
        return match center.end_digest_key(&d.key, how, d.words, now) {
            Some(id) => Applied {
                reply: format!("OK done={id} how={word}"),
                paint: Paint::None,
                note: true,
            },
            None => Applied {
                reply: "OK done=- how=gone".to_string(),
                paint: Paint::None,
                note: center.forget_filing(&d.key, stamp, now),
            },
        };
    };
    if let Some(words) = d.words {
        let restatement = match d.how {
            DoneHow::Ok => Restatement {
                finished: Some(Some(words)),
                ..Restatement::default()
            },
            DoneHow::Warn | DoneHow::Withdraw => Restatement {
                title: Some(words.clone()),
                finished: Some(Some(words)),
                ..Restatement::default()
            },
        };
        center.restate(id, restatement, now);
    }
    match d.how {
        DoneHow::Ok => center.resolve(id, Outcome::Ok, now),
        DoneHow::Warn => center.resolve(id, Outcome::Warn, now),
        DoneHow::Withdraw => center.withdraw(id, now),
    };
    center.forget_filing(&d.key, stamp, now);
    let reply = format!("OK done={id} how={}", d.how.retired().as_word());
    shown(center, gate, now, reply)
}

fn apply_dismiss(
    center: &mut MessageCenter,
    gate: &mut WireGate,
    id: MessageId,
    stamp: WallStamp,
    now: Instant,
) -> Applied {
    // An exception in the digest: a person can file it (the wire is theirs).
    if center.file_digest_one(id, stamp, now) {
        return Applied::quiet(format!("OK dismissed={id}"));
    }
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
            Hold::Default | Hold::For(_) | Hold::Standing(_) | Hold::LogOnly => {}
        }
    }
    center.dismiss(id, now);
    shown(center, gate, now, format!("OK dismissed={id}"))
}

/// The capsule a press names on a live row.
#[must_use]
pub(crate) fn resolve_press(row: &Live, press: &Press) -> Option<ActionIndex> {
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

/// The press as the caller spelled it, for `no action <press>`.
#[must_use]
pub(crate) fn press_words(press: &Press) -> String {
    match press {
        Press::Index(i) => i.to_string(),
        Press::Label(label) => label.clone(),
        Press::Details => "details".to_string(),
    }
}

/// How long a script is told to wait while aterm hands itself to a new version.
pub const SWITCHING_RETRY: Duration = Duration::from_secs(1);

/// `ERR busy notice: aterm is switching to its new version retry_ms=<ms>`.
#[must_use]
pub fn switching_reply() -> String {
    format!(
        "ERR busy notice: aterm is switching to its new version retry_ms={}",
        SWITCHING_RETRY.as_millis()
    )
}

/// `ERR notice: no live message <id>`.
#[must_use]
pub fn no_live_reply(id: MessageId) -> String {
    format!("ERR notice: no live message {id}")
}

/// What `notice act` presses.
/// # Errors
/// The reply line, when there is no live row, no such capsule, or no press left in the window.
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
    if !index.is_details() && row.msg.actions[usize::from(index.0)].person_only() {
        return Err(format!(
            "ERR notice: {} on message {id} is a person's own press on this Mac's screen",
            enc(row.msg.actions[usize::from(index.0)].label())
        ));
    }
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
#[path = "wire_tests.rs"]
mod tests;
