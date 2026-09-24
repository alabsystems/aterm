// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! The wire grammars: the `messages` read query and its row, the
//! byte-identical `appstatus` compatibility face, and the `notice post`
//! parser. The percent-encoder is INJECTED (`aterm_control::wire::pct_encode`
//! in the host) — the grammar lives beside the state, the bytes stay the
//! host's, and no second encoder can drift.

use crate::center::{Live, MessageCenter};
use crate::log::{LogRecord, MessageLog};
use crate::model::{Hold, Intent, Message, MessageId, Origin, Severity, Tag};
use crate::text::clip;
use crate::{DETAIL_LINES_CAP, Duration, Instant, KEY_CAP, LOG_CAP, TITLE_CAP};

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
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ReadQuery {
    /// Keep the newest `n` (default 64, max [`LOG_CAP`]), still ascending.
    pub n: usize,
    /// Only ids strictly greater.
    pub since: Option<MessageId>,
    /// Only this tag.
    pub tag: Option<Tag>,
    /// A floor: `sev=warn` is warn + error.
    pub min_severity: Option<Severity>,
    /// Only unretired rows.
    pub live_only: bool,
}

impl Default for ReadQuery {
    fn default() -> Self {
        Self {
            n: DEFAULT_READ_N,
            since: None,
            tag: None,
            min_severity: None,
            live_only: false,
        }
    }
}

impl ReadQuery {
    /// The records the query selects, ascending by id.
    #[must_use]
    pub fn select<'a>(&self, log: &'a MessageLog) -> Vec<&'a LogRecord> {
        let picked: Vec<&LogRecord> = log
            .records()
            .filter(|r| self.since.is_none_or(|s| r.id > s))
            .filter(|r| self.tag.as_ref().is_none_or(|t| r.tag == *t))
            .filter(|r| self.min_severity.is_none_or(|s| r.severity >= s))
            .filter(|r| !self.live_only || r.is_live())
            .collect();
        let skip = picked.len().saturating_sub(self.n);
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
            q.n = n.min(LOG_CAP);
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
/// `message <id> at=<unix_ms> ago_ms=<ms> tag=<tag> sev=<sev> state=<…>
/// glass=<row|-> rep=<n> key=<pct|-> title=<pct> detail=<pct> actions=<pct|->
/// [progress=<n>/100] [since_ms=<ms>]`. `at=` is the wall clock at ingress;
/// `since_ms=` is monotonic and present only for rows retired in this
/// process; `detail=` is the lines joined by `\n` before encoding;
/// `actions=` the FULL capsule labels joined by `,`.
#[must_use]
pub fn message_row(
    rec: &LogRecord,
    live: Option<&Live>,
    glass_row: Option<usize>,
    now: Instant,
    now_unix_ms: u64,
    enc: &dyn Fn(&str) -> String,
) -> String {
    let state = state_word(rec, live);
    let (title, detail, actions, repeats) = match live {
        Some(l) => (&l.msg.title, &l.msg.detail, &l.msg.actions, l.repeats),
        None => (&rec.title, &rec.detail, &rec.actions, rec.repeats),
    };
    let labels: Vec<&str> = actions.iter().map(Intent::label).collect();
    let mut row = format!(
        "message {} at={} ago_ms={} tag={} sev={} state={state} glass={} rep={repeats} key={} title={} detail={} actions={}",
        rec.id,
        rec.stamp.unix_ms,
        now_unix_ms.saturating_sub(rec.stamp.unix_ms),
        rec.tag,
        rec.severity.as_str(),
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
    if let Some(fill) = live
        .and_then(|l| l.msg.meter.as_ref())
        .and_then(|m| m.fill_permille)
    {
        row.push_str(" progress=");
        row.push_str(&progress_word(Some(fill)));
    }
    if let Some(retired_at) = rec.retired_at {
        row.push_str(" since_ms=");
        row.push_str(
            &now.saturating_duration_since(retired_at)
                .as_millis()
                .to_string(),
        );
    }
    row
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

/// `notice post <tag> [sev=<sev>] [key=<key>] [hold=<1..3600>] <title>[ -- <detail>]`.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct PostRequest {
    /// Must validate.
    pub tag: Tag,
    /// Defaults to `info`.
    pub severity: Severity,
    /// ≤ [`KEY_CAP`].
    pub key: Option<String>,
    /// Seconds, 1..=3600; the severity's hold when absent.
    pub hold_secs: Option<u32>,
    /// ≤ [`TITLE_CAP`] chars.
    pub title: String,
    /// ≤ [`WIRE_DETAIL_BYTES`], split on a literal `\n`, ≤ [`DETAIL_LINES_CAP`] lines.
    pub detail: Vec<String>,
}

impl PostRequest {
    /// Parse the words after `notice post`.
    ///
    /// # Errors
    /// [`POST_USAGE`] for a missing or invalid tag, an unknown severity, a
    /// hold outside `1..=3600`, or an empty title.
    pub fn parse(rest: &str) -> Result<Self, &'static str> {
        let rest = rest.trim_start();
        let (tag, mut rest) = rest.split_once(char::is_whitespace).ok_or(POST_USAGE)?;
        let tag = Tag::try_new(tag).map_err(|_| POST_USAGE)?;
        let mut severity = Severity::Info;
        let mut key = None;
        let mut hold_secs = None;
        loop {
            rest = rest.trim_start();
            let word = rest.split_whitespace().next().unwrap_or("");
            if let Some(v) = word.strip_prefix("sev=") {
                severity = Severity::parse(v).ok_or(POST_USAGE)?;
            } else if let Some(v) = word.strip_prefix("key=") {
                let k = clip(v, KEY_CAP);
                key = (!k.is_empty()).then_some(k);
            } else if let Some(v) = word.strip_prefix("hold=") {
                let secs: u32 = v.parse().map_err(|_| POST_USAGE)?;
                if secs == 0 || secs > MAX_WIRE_HOLD_SECS {
                    return Err(POST_USAGE);
                }
                hold_secs = Some(secs);
            } else {
                break;
            }
            rest = &rest[word.len()..];
        }
        let (title, detail) = rest.split_once(" -- ").unwrap_or((rest, ""));
        let title = clip(title.trim(), TITLE_CAP);
        if title.is_empty() {
            return Err(POST_USAGE);
        }
        let detail = detail.trim();
        let mut end = detail.len().min(WIRE_DETAIL_BYTES);
        while !detail.is_char_boundary(end) {
            end -= 1;
        }
        let detail: Vec<String> = detail[..end]
            .split("\\n")
            .map(str::trim)
            .filter(|l| !l.is_empty())
            .take(DETAIL_LINES_CAP)
            .map(str::to_string)
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

    /// The message: `Origin::Wire`, the severity's glyph, NO actions ever
    /// (a socket client cannot mint an owner gesture).
    #[must_use]
    pub fn into_message(self) -> Message {
        let mut msg = Message::new(self.tag, self.severity, self.title)
            .lines(self.detail)
            .origin(Origin::Wire);
        if let Some(key) = &self.key {
            msg = msg.key(key);
        }
        if let Some(secs) = self.hold_secs {
            msg = msg.hold(Hold::For(Duration::from_secs(u64::from(secs))));
        }
        msg
    }
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
        let row = message_row(rec, c.live(p.id), Some(0), now, 6_500, &pct_encode);
        assert_eq!(
            row,
            "message 1 at=5000 ago_ms=1500 tag=privacy sev=info state=held glass=0 rep=1 key=privacy.fda title=File%20access%20not%20confirmed detail=Full%20Disk%20Access%20may%20already%20be%20enabled%0Asecond%20line actions=Open%20Settings,Not%20now"
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
        let row = message_row(rec, c.live(m.id), None, now, 5_100, &pct_encode);
        assert!(
            row.ends_with(" key=- title=Installing detail= actions=- progress=43/100"),
            "{row}"
        );
        assert!(row.contains(" state=live glass=- "), "{row}");
        c.dismiss(p.id, now + Duration::from_millis(250));
        let rec = c.log().get(p.id).unwrap();
        let row = message_row(
            rec,
            None,
            None,
            now + Duration::from_millis(1000),
            9_000,
            &pct_encode,
        );
        assert!(row.contains(" state=dismissed glass=- "), "{row}");
        assert!(row.ends_with(" since_ms=750"), "{row}");
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
        assert_eq!(parse_read_args("9999").unwrap().n, LOG_CAP);
        for junk in [
            "0", "since=0", "since=x", "tag=Bad", "sev=loud", "wat", "live=1",
        ] {
            assert_eq!(parse_read_args(junk), Err(READ_USAGE), "{junk}");
        }
    }

    #[test]
    fn notice_post_parses_the_grammar_and_never_authors_an_action() {
        let req = PostRequest::parse(
            "system sev=warn key=k.1 hold=90 Something happened -- line one\\nline two",
        )
        .unwrap();
        assert_eq!(req.tag, tags::SYSTEM);
        assert_eq!(req.severity, Severity::Warn);
        assert_eq!(req.key.as_deref(), Some("k.1"));
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
        assert_eq!(plain.into_message().hold, Hold::Default);
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
}
