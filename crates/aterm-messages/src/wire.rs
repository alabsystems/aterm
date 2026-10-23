// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Wire grammars for `messages`, the `appstatus` compatibility face, and the `notice` verb.

use std::collections::VecDeque;

use crate::center::{Live, MessageCenter, Outcome, PostOutcome};
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
    /// The page size as given (at most [`LOG_CAP`]); `None` is the default for the query's shape ([`ReadQuery::page`]).
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
///
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

/// The `appstatus` progress field for a fill: `<n>/100` rounded to the
fn progress_word(fill: Option<u16>) -> String {
    fill.map_or_else(
        || "-".to_string(),
        |p| format!("{}/100", (p.min(1000) + 5) / 10),
    )
}

/// The `state=` word of a record.
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
        (None, None) if rec.later => "review",
        (None, None) => "live",
    }
}

/// One `messages` row: `message <id> at=<unix_ms> ago_ms=<ms> tag=<tag> sev=<sev> origin=<host|wire|carried> state=<…> glass=<row|-> rep=<n> key=<pct|-> title=<pct> detail=<pct> actions=<pct|-> [progress=<n>/100|busy=1] [load=<network|disk|cpu>] [reveal=pending] [since_ms=<ms>] [need=<class>]`.
#[must_use]
pub(crate) fn message_row(
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
    let labels: Vec<&str> = actions
        .iter()
        .filter(|intent| live.is_some() || rec.still_offers(intent))
        .map(Intent::label)
        .collect();
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
                return message_row(&now_words, None, None, now_unix_ms, enc);
            }
            let glass = live.and_then(|_| center.glass_position(rec.id));
            message_row(rec, live, glass, now_unix_ms, enc)
        })
        .collect()
}

/// The `appstatus` body in the retired status bars' byte-identical grammar (field order, keys, percent-encoding, the phase and outcome words; a `phase=live` line carries the row's current words).
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
    "usage: notice <post|progress|done|ask|later|dismiss|act> \u{2026}";
/// The `notice progress` usage line.
pub(crate) const PROGRESS_USAGE: &str = "usage: notice progress <key> [tag=<tag>] [pct=<0..100>|done=<n>/<total>|busy] [unit=<bytes|items|steps>] [load=<network|disk|cpu>] <title>[ -- <stats>]";
/// The `notice done` usage line.
pub(crate) const DONE_USAGE: &str = "usage: notice done <key> [ok|warn|withdraw] [<words>]";
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
/// [`apply`] handed an `act`: a press is the host's to perform ([`press_target`], then its own press path).
pub(crate) const ACT_IS_THE_HOSTS: &str = "ERR notice: act is pressed by the host";

/// Whether `notice` refuses `tag`: every family aterm reports under
fn aterms_own(tag: &Tag) -> bool {
    *tag != tags::SCRIPT && tags::ALL.contains(&tag)
}

/// The refusal for a glass title fault: `notice: <fault>`, one static string
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

/// `Err(refusal)` when `title` may not stand on the glass. `tail`: the verb
fn title_form(title: &str, tail: bool) -> Result<(), &'static str> {
    glass_title_fault(title).map_or(Ok(()), |f| Err(title_refusal(f, tail)))
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

/// The tag rule: any [`Tag::try_new`] word but aterm's own families.
fn wire_tag(word: &str, usage: &'static str) -> Result<Tag, &'static str> {
    let tag = Tag::try_new(word).map_err(|_| usage)?;
    if aterms_own(&tag) {
        return Err(OWN_TAG_REFUSAL);
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

/// A wire title, detail line, stats or `done`'s words: stripped, then trimmed,
fn clip_words(s: &str, cap: usize) -> String {
    clip(clip(s, usize::MAX).trim(), cap)
}

/// Options come in any order after the fixed words until the first word
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
    /// ≤ [`TITLE_CAP`] chars; a `warn`/`error` title obeys the glass title form ([`glass_title_fault`]).
    pub title: String,
    /// ≤ [`WIRE_DETAIL_BYTES`], split on a literal `\n`, each line clipped to [`DETAIL_LINE_CAP`], ≤ [`DETAIL_LINES_CAP`] lines.
    pub detail: Vec<String>,
}

impl PostRequest {
    /// Parse the words after `notice post`.
    ///
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

    /// The message: `Origin::Wire`, the severity's glyph, no actions ever (a socket client cannot mint an owner gesture).
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
    ///
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

    /// The meter the line declares, normalized: a fill, an amount in the key's own series ([`Amount::series_of`] the full key), or busy.
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

    /// The in-place change a later line makes: the title and the meter, nothing else — tag, severity, hold and actions never.
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
    /// The full key, `wire.<key>`.
    pub key: String,
    /// `ok` by default.
    pub how: DoneHow,
    /// The finishing words (the glass title form), re-titling the row first.
    pub words: Option<String>,
}

impl DoneRequest {
    /// Parse the words after `notice done`.
    ///
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
    ///
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
    ///
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

    /// The ask: the owner only, the step (on the link's button when there is one), the
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

/// The refusal for an ask the door would not stand (`need::Ask::fault`), in the
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

/// `notice <post|progress|done|ask|later|dismiss|act> …`, parsed on the control thread.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum NoticeRequest {
    /// A record, or a failure row.
    Post(PostRequest),
    /// A script's work in flight.
    Progress(ProgressRequest),
    /// Its finish.
    Done(DoneRequest),
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
    ///
    /// # Errors
    /// [`NOTICE_USAGE`] for a missing or unknown sub-form.
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
    /// A paint is armed at [`WireGate::due`]: the last wire paint is younger than [`WIRE_PAINT_GAP`].
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
    #[must_use]
    pub fn due(&self) -> Option<Instant> {
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

    /// Spend one mint carrying `bytes` of title and detail, or say which
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

/// Post a row from the wire (a `post` at warn or error, an `ask`): the
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

/// File a `notice later` entry: the wire's cap on open entries and the digest's room
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
        // The entry retired the live row that told the fact: the band moved.
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

/// How long a script is told to wait while aterm hands itself to a new version ([`switching_reply`]).
pub const SWITCHING_RETRY: Duration = Duration::from_secs(1);

/// `ERR busy notice: aterm is switching to its new version retry_ms=<ms>`.
#[must_use]
pub fn switching_reply() -> String {
    format!(
        "ERR busy notice: aterm is switching to its new version retry_ms={}",
        SWITCHING_RETRY.as_millis()
    )
}

/// `ERR notice: no live message <id>`: `dismiss`, `act` and the host's press that found the row gone all answer with this one line.
#[must_use]
pub fn no_live_reply(id: MessageId) -> String {
    format!("ERR notice: no live message {id}")
}

/// What `notice act` presses.
///
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
    gate.try_press(now).map_err(Busy::reply)?;
    let label = if index.is_details() {
        Intent::Details.label()
    } else {
        row.msg.actions[usize::from(index.0)].label()
    };
    Ok((index, label))
}

/// Spend one band press for a press that did not come through `notice act` (`pointer click` on a capsule).
///
/// # Errors
/// The whole `ERR busy notice: … presses a minute retry_ms=<ms>` line when the window's presses are spent.
pub fn spend_press(gate: &mut WireGate, now: Instant) -> Result<(), String> {
    gate.try_press(now).map_err(Busy::reply)
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
    use aterm_codec::pct::encode as pct_encode;

    fn t0() -> Instant {
        Instant::now()
    }

    fn stamp(ms: u64) -> WallStamp {
        WallStamp { unix_ms: ms }
    }

    /// `appstatus` reads a meter's fill only: a busy row is `progress=-`, like
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

    /// A live toolchain meter at 50 % and a retired Warn update row render the
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

    /// `since_ms` never runs ahead of `ago_ms`: both are read off the reply's
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

    /// A carried row's `since_ms` counts from its retirement in this process,
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
                .hold(Hold::asking(Duration::from_secs(600))),
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
            row.ends_with(" key=- title=Installing detail= actions=- progress=42/100"),
            "{row}"
        );
        assert!(row.contains(" state=live glass=- "), "{row}");
        c.dismiss(p.id, now + Duration::from_millis(250));
        let rec = c.log().get(p.id).unwrap();
        let row = message_row(rec, None, None, 9_000, &pct_encode);
        assert!(row.contains(" state=dismissed glass=- "), "{row}");
        assert!(row.contains(" ago_ms=4000 "), "{row}");
        assert!(row.ends_with(" since_ms=3750"), "{row}");
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

    /// Paging with the last id seen never skips a record. 100 records:
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
            "script sev=warn key=k.1 hold=90 Something happened -- line one\\nline two",
        )
        .unwrap();
        assert_eq!(req.tag, tags::SCRIPT);
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
        let plain = PostRequest::parse("peers  just a title").unwrap();
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
            "script",
            "Bad title",
            "script sev=loud t",
            "script hold=0 t",
            "script hold=121 t",
            "script hold=3600 t",
            "script hold=x t",
        ] {
            assert_eq!(PostRequest::parse(junk), Err(POST_USAGE), "{junk:?}");
        }
        let long = format!("script t -- {}", "é".repeat(WIRE_DETAIL_BYTES));
        let req = PostRequest::parse(&long).unwrap();
        assert!(req.detail[0].len() <= WIRE_DETAIL_BYTES);
        let many = format!(
            "script t -- {}",
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

    /// A wire post under `tag`, which `notice` itself may refuse: what an
    fn wire_row(
        c: &mut MessageCenter,
        g: &mut WireGate,
        tag: Tag,
        rest: &str,
        now: Instant,
    ) -> Applied {
        let mut req = PostRequest::parse(&format!("script {rest}")).expect("a post");
        req.tag = tag;
        apply(c, g, NoticeRequest::Post(req), stamp(1_000), now)
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
    fn onto_glass(c: &mut MessageCenter, now: Instant) {
        c.settle(now, true);
        c.commit_rows(now, 3);
    }

    /// A script posts under `script` or a tag of its own, never under a family
    #[test]
    fn a_script_may_not_post_under_any_of_aterms_families() {
        for own in tags::ALL.iter().filter(|t| ***t != tags::SCRIPT) {
            let own = own.as_str();
            for line in [
                format!("post {own} sev=warn Worker crashed"),
                format!("post {own} a note"),
                format!("progress k tag={own} Building aterm"),
            ] {
                assert_eq!(
                    NoticeRequest::parse(&line).err(),
                    Some(OWN_TAG_REFUSAL),
                    "{line}"
                );
            }
        }
        for line in [
            "post script sev=warn Worker crashed",
            "post crashes a note",
            "progress k tag=script Building aterm",
            "progress k tag=deploy Building aterm",
        ] {
            assert!(NoticeRequest::parse(line).is_ok(), "{line}");
        }
    }

    #[test]
    fn notice_parses_every_form_and_refuses_junk() {
        let Ok(NoticeRequest::Post(p)) = NoticeRequest::parse(
            "post script sev=warn key=deploy hold=90 Deploy failed -- connection refused",
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
            "progress build pct=42.5 load=disk tag=deploy Building aterm -- 3 of 10",
        ) else {
            panic!()
        };
        assert_eq!(
            p,
            ProgressRequest {
                key: "wire.build".into(),
                tag: Tag::try_new("deploy").unwrap(),
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
            (WireIndicator::Busy, tags::SCRIPT, None, "")
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
            ("post script", POST_USAGE),
            ("post script key=Bad t", KEY_REFUSAL),
            ("post script key= t", KEY_REFUSAL),
            ("post script key=wire/x t", KEY_REFUSAL),
            ("post update t", OWN_TAG_REFUSAL),
            ("post harness t", OWN_TAG_REFUSAL),
            ("post toolchain sev=warn t", OWN_TAG_REFUSAL),
            ("post script hold=+5 t", POST_USAGE),
            ("post script -- only a detail", POST_USAGE),
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
            ("progress k tag=update t", OWN_TAG_REFUSAL),
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

    #[test]
    fn an_info_post_is_a_record_and_a_warn_post_is_a_row() {
        let now = t0();
        let (mut c, mut g) = fresh(now);
        let a = notice(
            &mut c,
            &mut g,
            "post script hold=90 hello from a script",
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
        let s = notice(&mut c, &mut g, "post script sev=success Deployed", now);
        assert_eq!(s.reply, "OK recorded message=2");
        let w = notice(
            &mut c,
            &mut g,
            "post script sev=warn Deploy failed -- connection refused",
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
            "post script sev=error hold=90 Disk full",
            now,
        );
        assert_eq!(
            c.live(id_of(&e)).unwrap().msg.hold,
            Hold::For(Duration::from_secs(90))
        );
        let p = notice(&mut c, &mut g, "progress deploy Deploying aterm", now);
        let fixed = notice(
            &mut c,
            &mut g,
            "post script key=deploy Deploy fixed",
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
    #[test]
    fn every_wire_row_earns_its_place_on_the_glass() {
        let now = t0();
        let (mut c, mut g) = fresh(now);
        for line in [
            "post script Just so you know",
            "post peers sev=success All good -- details here",
            "post worker sev=error key=a Worker crashed -- exit 3",
            "progress b Building aterm",
            "progress c pct=10 load=cpu Verifying the payload",
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
                "post script sev=warn one two three four five six seven",
                "notice: title over 6 words; put the rest after --",
            ),
            (
                "post script sev=error Deploy failed: connection refused",
                "notice: title has a clause (\u{2014}, ;, :); put it after --",
            ),
            (
                "progress k Building the whole of the aterm workspace now",
                "notice: title over 6 words; put the rest after --",
            ),
            (
                "progress k Supercalifragilisticexpialidocious-builds everywhere",
                "notice: title over 48 characters; put the rest after --",
            ),
            (
                "progress k Building aterm.",
                "notice: title ends with a period",
            ),
            (
                "done k ok Built; shipped",
                "notice: title has a clause (\u{2014}, ;, :)",
            ),
            (
                "done k warn one two three four five six seven",
                "notice: title over 6 words",
            ),
        ] {
            assert_eq!(NoticeRequest::parse(line), Err(want), "{line}");
        }
        let long = format!("post script {}", "word ".repeat(20));
        let Ok(NoticeRequest::Post(p)) = NoticeRequest::parse(&long) else {
            panic!()
        };
        assert_eq!(p.title.chars().count(), 99);
        let over = format!("post script {}", "x".repeat(200));
        let Ok(NoticeRequest::Post(p)) = NoticeRequest::parse(&over) else {
            panic!()
        };
        assert_eq!(p.title.chars().count(), TITLE_CAP);
        assert!(NoticeRequest::parse("post script Build: done. Next; step").is_ok());
    }

    /// Sanitising: control and invisible format characters never reach a
    #[test]
    fn the_wire_strips_what_could_forge_a_row() {
        let Ok(NoticeRequest::Post(p)) = NoticeRequest::parse(
            "post script sev=warn Deploy\u{1b}[31m \u{202e}failed -- a\u{7}b\\n\u{200b}\\nc",
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
            NoticeRequest::parse("post script \u{200b}\u{202e}"),
            Err(POST_USAGE)
        );
        assert_eq!(
            NoticeRequest::parse("progress k \u{7}\u{7}"),
            Err(PROGRESS_USAGE)
        );
        assert_eq!(
            NoticeRequest::parse("post script sev=warn Build:\u{2060} failed"),
            Err("notice: title has a clause (\u{2014}, ;, :); put it after --")
        );
        assert_eq!(
            NoticeRequest::parse("post script sev=warn Build failed.\u{2060}"),
            Err("notice: title ends with a period")
        );
        let Ok(NoticeRequest::Post(p)) = NoticeRequest::parse(
            "post script sev=warn De\u{061c}ploy\u{00ad} fa\u{e0041}il\u{fdd0}ed\u{ffff}\u{1fffe}\u{206a}\u{fff9}",
        ) else {
            panic!()
        };
        assert_eq!(p.title, "Deploy failed");
    }

    /// A whitespace control is a space before the strip: `Tab\there` is
    #[test]
    fn a_tab_on_the_wire_is_a_space() {
        let Ok(NoticeRequest::Post(p)) = NoticeRequest::parse(
            "post\tscript\tsev=warn\tDeploy\tfailed\t--\tconnection\x0brefused",
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
            NoticeRequest::parse("post script sev=warn one\ttwo\tthree\tfour\tfive\tsix\tseven"),
            Err("notice: title over 6 words; put the rest after --"),
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
        notice(&mut c, &mut g, "post script key=k Fixed", now);
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
        assert_eq!(
            row.msg.tag,
            tags::SCRIPT,
            "a script's row is not aterm's own system"
        );
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
            "progress build pct=42.5 tag=deploy Building aterm -- 3 of 10",
            now + ms(100),
        );
        assert_eq!((b.reply.as_str(), b.note), ("OK message=1", false));
        assert_eq!(c.log().len(), logged, "a restate logs nothing");
        let row = c.live(id).unwrap();
        assert_eq!(row.msg.tag, tags::SCRIPT, "the tag is the first line's");
        let m = row.msg.meter.as_ref().unwrap();
        assert_eq!(
            (m.fill_permille, m.busy, m.stats.as_str()),
            (Some(425), false, "3 of 10")
        );
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
        let h = notice(&mut c, &mut g, "progress fetch load=network Fetching", t);
        assert_eq!(c.live(id_of(&h)).unwrap().shown_load(), Some(Load::Network));
        let f = notice(
            &mut c,
            &mut g,
            "post script sev=warn key=fetch Fetch failed",
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
        let work = id_of(&notice(&mut c, &mut g, "progress same Building aterm", t));
        let fail = notice(
            &mut c,
            &mut g,
            "post script sev=error key=same Building aterm",
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
            assert!(g.due().is_none_or(|d| d <= t + WIRE_PAINT_GAP));
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
    fn an_ask_is_a_classified_standing_row_the_tool_resolves() {
        let now = t0();
        let (mut c, mut g) = fresh(now);
        let line = "ask release.notary tag=release Apple agreement blocks the release -- \
                    Apple's developer agreement for team X lapsed, so notarization answers 403 \
                    -- Accept the current agreements at developer.apple.com/account";
        let Ok(NoticeRequest::Ask(req)) = NoticeRequest::parse(line) else {
            panic!("{line}");
        };
        assert_eq!(req.key, "wire.release.notary");
        assert_eq!(req.tag.as_str(), "release");
        assert_eq!(req.title, "Apple agreement blocks the release");
        let a = notice(&mut c, &mut g, line, now);
        assert_eq!((a.reply.as_str(), a.note), ("OK message=1", true));
        let row = c.live(id_of(&a)).expect("a live row");
        assert!(row.msg.persists(), "standing: {:?}", row.msg.hold);
        assert!(row.msg.actions.is_empty(), "the socket authors no button");
        let crate::need::Need::Now(ask) = row.msg.need() else {
            panic!("classified now: {:?}", row.msg.need());
        };
        assert!(ask.is_standing());
        assert_eq!(
            ask.then().words(),
            "Accept the current agreements at developer.apple.com/account"
        );
        assert_eq!(c.demotions(), 0, "the door keeps it");
        let again = notice(&mut c, &mut g, line, now);
        assert_eq!(again.reply, "OK message=1");
        assert_eq!(c.live_rows().count(), 1);
        let done = notice(&mut c, &mut g, "done release.notary ok", now);
        assert_eq!(done.reply, "OK done=1 how=resolved-ok");
        assert_eq!(c.live_rows().count(), 0);
    }

    #[test]
    fn an_ask_the_door_would_not_stand_is_refused() {
        for (line, want) in [
            ("ask", ASK_USAGE),
            ("ask k", ASK_USAGE),
            ("ask k Title only", ASK_USAGE),
            ("ask k Title -- a reason only", ASK_USAGE),
            ("ask k Title -- -- a step", ASK_USAGE),
            ("ask K Title -- why -- Do it", KEY_REFUSAL),
            ("ask k tag=update Title -- why -- Do it", OWN_TAG_REFUSAL),
            ("ask k tag=Bad Title -- why -- Do it", ASK_USAGE),
            (
                "ask k Title: with a clause -- why -- Do it",
                "notice: title has a clause (\u{2014}, ;, :); put it after --",
            ),
            (
                "ask k Release blocked -- it is waiting for Apple -- Accept it",
                "notice: the reason only says something is pending; say why only the owner can close it",
            ),
            (
                "ask k Release blocked -- Apple refuses it -- Wait for Apple",
                "notice: a snooze is not an action; name the step that closes it",
            ),
            (
                "ask k Release blocked -- Apple refuses it -- Accept it or renew it",
                "notice: two actions in one step; name the one that closes it",
            ),
        ] {
            assert_eq!(NoticeRequest::parse(line).err(), Some(want), "{line}");
        }
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
        let p = id_of(&notice(
            &mut c,
            &mut g,
            "post script sev=warn key=db Database down",
            on,
        ));
        assert_eq!(
            notice(&mut c, &mut g, "done db", on).reply,
            format!("OK done={p} how=resolved-ok")
        );
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
        for line in ["progress k5 Working", "post script sev=warn Deploy failed"] {
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
        assert!(
            notice(&mut c, &mut g, "progress k1 pct=5 Working", now)
                .reply
                .starts_with("OK")
        );
        assert!(
            notice(&mut c, &mut g, "post script sev=warn key=k2 K2 failed", now)
                .reply
                .starts_with("OK message=")
        );
        assert!(
            notice(&mut c, &mut g, "post script a note", now)
                .reply
                .starts_with("OK recorded")
        );
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

        let (mut c, mut g) = fresh(now);
        for i in 0..WIRE_MINTS_PER_WINDOW as u64 {
            let a = notice(
                &mut c,
                &mut g,
                &format!("post script note {i}"),
                now + ms(i),
            );
            assert!(a.reply.starts_with("OK recorded"), "{}", a.reply);
        }
        let logged = c.log().len();
        let busy = notice(&mut c, &mut g, "post script one too many", now + ms(100));
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
        let later = now + Duration::from_secs(60);
        assert!(
            notice(&mut c, &mut g, "post script again", later)
                .reply
                .starts_with("OK")
        );
        assert!(
            notice(&mut c, &mut g, "post script again 2", later)
                .reply
                .starts_with("ERR busy"),
            "the second mint of 1 ms leaves at 60 s + 1 ms"
        );
        assert!(
            notice(&mut c, &mut g, "post script again 2", later + ms(1))
                .reply
                .starts_with("OK")
        );
        assert_eq!(
            notice(&mut c, &mut g, "post script again 3", later + ms(1)).reply,
            "ERR busy notice: 60 new messages a minute retry_ms=1"
        );
        let (mut c, mut g) = fresh(now);
        let w = notice(&mut c, &mut g, "post script sev=warn Deploy failed", now);
        let logged = c.log().len();
        for i in 1..=100 {
            let d = notice(
                &mut c,
                &mut g,
                "post script sev=warn Deploy failed",
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
                &format!("post script note {i}"),
                now + ms(200),
            );
            assert!(a.reply.starts_with("OK recorded"), "{i}: {}", a.reply);
        }
        assert!(
            notice(&mut c, &mut g, "post script the 61st", now + ms(200))
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
                Message::new(tags::RENDER, Severity::Error, "GPU lost").hold(Hold::STANDING),
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
                    .hold(Hold::asking(Duration::from_secs(600))),
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
                    .hold(Hold::asking(Duration::from_secs(600))),
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
        let a = notice(&mut c, &mut g, &format!("act {id} 1"), now);
        assert_eq!((a.reply.as_str(), a.paint), (ACT_IS_THE_HOSTS, Paint::None));
        assert!(c.live(id).is_some());
    }

    /// A loop of `act <id> details` on a row that stays live gets the budget's
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
        let later = now + Duration::from_secs(60);
        assert!(press_target(&c, &mut g, row, &Press::Details, &pct_encode, later).is_ok());
        assert_eq!(no_live_reply(gone), "ERR notice: no live message 999");
    }

    /// A wire post never folds into a host row, and a host post never into a
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
        let w = wire_row(
            &mut c,
            &mut g,
            tags::SESSION,
            "sev=error Keystrokes dropped",
            later,
        );
        assert_ne!(id_of(&w), host, "a new wire row, not the host's");
        assert_eq!(c.live(id_of(&w)).unwrap().msg.origin, Origin::Wire);
        let after = c.live(host).unwrap();
        assert_eq!(after.repeats, before.repeats, "the host row's count");
        assert_eq!(after.fold_at, before.fold_at, "the host row's hold");
        let (mut c, mut g) = fresh(now);
        let planted = id_of(&wire_row(
            &mut c,
            &mut g,
            tags::PRIVACY,
            "sev=warn Allow file access",
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
        let again = wire_row(
            &mut c,
            &mut g,
            tags::PRIVACY,
            "sev=warn Allow file access",
            now,
        );
        assert_eq!(id_of(&again), planted);
    }

    /// The mints' title and detail bytes are budgeted, so full-detail records
    #[test]
    fn the_wire_budgets_the_bytes_it_writes() {
        let now = t0();
        let (mut c, mut g) = fresh(now);
        let detail = vec!["x".repeat(200); 10].join("\\n");
        let line = |i: u64| format!("post script note {i:02} -- {detail}");
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
        assert!(
            notice(&mut c, &mut g, "post script ok", now + ms(100))
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
            "post peers peer said hi -- line",
            now,
        ));
        let on = now + PROGRESS_GRACE;
        onto_glass(&mut c, on);
        let rows = message_rows(&c, &ReadQuery::default(), 3_000, &pct_encode);
        assert_eq!(
            rows,
            vec![
                format!(
                    "message {busy} at=1000 ago_ms=2000 tag=script sev=info origin=wire state=live glass=0 rep=1 key=wire.fetch title=Fetching%20the%20index detail= actions=- busy=1 load=network"
                ),
                format!(
                    "message {fill} at=1000 ago_ms=2000 tag=script sev=info origin=wire state=live glass=1 rep=1 key=wire.build title=Building%20aterm detail= actions=- progress=42/100"
                ),
                format!(
                    "message {rec} at=1000 ago_ms=2000 tag=peers sev=info origin=wire state=recorded glass=- rep=1 key=- title=peer%20said%20hi detail=line actions=- since_ms=2000"
                ),
            ]
        );
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
        notice(&mut c, &mut g, "post script sev=warn Deploy failed", shown);
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
        let peers = message_rows(
            &c,
            &parse_read_args("tag=peers").unwrap(),
            9_000,
            &pct_encode,
        );
        assert_eq!(peers.len(), 1);
        let newest = message_rows(&c, &parse_read_args("1").unwrap(), 9_000, &pct_encode);
        assert!(newest[0].contains("title=Deploy%20failed"), "{newest:?}");
    }

    #[test]
    fn no_capsule_is_ever_authored_from_the_wire() {
        for line in [
            "post script Not now",
            "post script sev=warn key=a Install now",
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
