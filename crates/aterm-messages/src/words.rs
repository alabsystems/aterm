// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Words the Settings page and the band share: relative and absolute time,
//! the `~` abbreviation, and the copy-to-clipboard text of a record. Pure.

use crate::log::LogRecord;

const MS_PER_MIN: u64 = 60_000;
const MS_PER_HOUR: u64 = 3_600_000;
const MS_PER_DAY: u64 = 86_400_000;

/// How long ago `at` was, in words: `just now` (under a minute, or in the
/// future), `3 min ago`, `2 h ago`, `yesterday` (24–48 h), else the date
/// `YYYY-MM-DD`.
#[must_use]
pub fn relative_words(now_unix_ms: u64, at_unix_ms: u64) -> String {
    let ago = now_unix_ms.saturating_sub(at_unix_ms);
    if ago < MS_PER_MIN {
        "just now".to_string()
    } else if ago < MS_PER_HOUR {
        format!("{} min ago", ago / MS_PER_MIN)
    } else if ago < MS_PER_DAY {
        format!("{} h ago", ago / MS_PER_HOUR)
    } else if ago < 2 * MS_PER_DAY {
        "yesterday".to_string()
    } else {
        date_words(at_unix_ms)
    }
}

/// `YYYY-MM-DD HH:MM:SS UTC` — a local twin of
/// `aterm_types::rfc3339::format_rfc3339` (no aterm-types dependency), and
/// it SAYS the zone: the page's meta line and the clipboard sit beside local
/// clocks (the terminal's own, the presence row's `Session started 19:10`),
/// and a bare `02:10:28` seven hours off them reads as a wrong clock, not a
/// different one (Phase 2 review).
#[must_use]
pub fn stamp_words(at_unix_ms: u64) -> String {
    let secs = at_unix_ms / 1000;
    let rem = secs % 86_400;
    let (hh, mm, ss) = (rem / 3600, (rem % 3600) / 60, rem % 60);
    let mut out = date_words(at_unix_ms);
    out.push(' ');
    push_padded(&mut out, hh, 2);
    out.push(':');
    push_padded(&mut out, mm, 2);
    out.push(':');
    push_padded(&mut out, ss, 2);
    out.push_str(" UTC");
    out
}

/// `YYYY-MM-DD` in UTC.
#[must_use]
pub fn date_words(at_unix_ms: u64) -> String {
    let days = at_unix_ms / MS_PER_DAY;
    let (y, m, d) = civil_from_days(days);
    let mut out = String::with_capacity(10);
    push_padded(&mut out, y, 4);
    out.push('-');
    push_padded(&mut out, m, 2);
    out.push('-');
    push_padded(&mut out, d, 2);
    out
}

/// Days since 1970-01-01 → proleptic-Gregorian `(year, month, day)` —
/// Howard Hinnant's `civil_from_days`, on the non-negative half only.
fn civil_from_days(days: u64) -> (u64, u64, u64) {
    let z = days + 719_468;
    let era = z / 146_097;
    let doe = z - era * 146_097;
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = doy - (153 * mp + 2) / 5 + 1;
    let m = if mp < 10 { mp + 3 } else { mp - 9 };
    let y = yoe + era * 400 + u64::from(m <= 2);
    (y, m, d)
}

/// `v` in decimal, zero-padded to at least `width` digits.
fn push_padded(out: &mut String, v: u64, width: usize) {
    let digits = v.to_string();
    for _ in digits.len()..width {
        out.push('0');
    }
    out.push_str(&digits);
}

/// `path` with a leading `home` component abbreviated to `~` — `~` for
/// home itself, `~/sub` below it, sibling and foreign paths verbatim (the
/// pure twin of `app_tabs::home_abbreviated`). `None` or an empty home
/// leaves the path alone; a home given with a trailing slash names the
/// same directory.
#[must_use]
pub fn home_abbreviate(path: &str, home: Option<&str>) -> String {
    let Some(home) = home.filter(|h| !h.is_empty()) else {
        return path.to_string();
    };
    let trimmed = home.trim_end_matches('/');
    let home = if trimmed.is_empty() { "/" } else { trimmed };
    match path.strip_prefix(home) {
        Some(rest) if rest.is_empty() || rest.starts_with('/') => format!("~{rest}"),
        _ => path.to_string(),
    }
}

/// Every path atom in `text` (a whitespace-delimited `home/…` token)
/// abbreviated with [`home_abbreviate`]; the band prints `~/…` while the
/// log keeps the absolute path (D4).
#[must_use]
pub fn abbreviate_paths_in(text: &str, home: Option<&str>) -> String {
    let Some(home) = home.filter(|h| !h.is_empty()) else {
        return text.to_string();
    };
    let mut out = String::with_capacity(text.len());
    let mut token = String::new();
    for c in text.chars() {
        if c.is_whitespace() {
            out.push_str(&home_abbreviate(&token, Some(home)));
            token.clear();
            out.push(c);
        } else {
            token.push(c);
        }
    }
    out.push_str(&home_abbreviate(&token, Some(home)));
    out
}

/// The closed table a Complete echo reads a title's leading present
/// participle through: only the verbs a band title uses, or plausibly will.
const FINISHED_VERBS: [(&str, &str); 15] = [
    ("Downloading", "Downloaded"),
    ("Installing", "Installed"),
    ("Updating", "Updated"),
    ("Checking", "Checked"),
    ("Verifying", "Verified"),
    ("Finishing", "Finished"),
    ("Saving", "Saved"),
    ("Restoring", "Restored"),
    ("Loading", "Loaded"),
    ("Preparing", "Prepared"),
    ("Copying", "Copied"),
    ("Moving", "Moved"),
    ("Indexing", "Indexed"),
    ("Building", "Built"),
    ("Fetching", "Fetched"),
];

/// A work title in its FINISHED form (design ruling 154): `Downloading aterm
/// v0.91.0` → `Downloaded aterm v0.91.0`, through [`FINISHED_VERBS`]. `None`
/// when the first word is not in the table — the title keeps its words.
#[must_use]
pub fn finished_form(title: &str) -> Option<String> {
    let first = title.split(' ').next().unwrap_or(title);
    let (_, done) = FINISHED_VERBS.iter().find(|(ing, _)| *ing == first)?;
    Some(format!("{done}{}", &title[first.len()..]))
}

/// What the page copies for one record: the title, then `<stamp> · <tag> ·
/// <sev>`, then every detail line.
#[must_use]
pub fn copy_text(rec: &LogRecord) -> String {
    let mut out = rec.title.clone();
    out.push('\n');
    out.push_str(&stamp_words(rec.stamp.unix_ms));
    out.push_str(" \u{00b7} ");
    out.push_str(rec.tag.as_str());
    out.push_str(" \u{00b7} ");
    out.push_str(rec.severity.as_str());
    for line in &rec.detail {
        out.push('\n');
        out.push_str(line);
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::log::{LogRecord, LogState};
    use crate::model::{Glyph, MessageId, Origin, Severity, WallStamp, tags};

    #[test]
    fn relative_words_read_like_a_person_says_them() {
        let now = 1_758_470_000_000;
        assert_eq!(relative_words(now, now), "just now");
        assert_eq!(relative_words(now, now + 5_000), "just now");
        assert_eq!(relative_words(now, now - 59_999), "just now");
        assert_eq!(relative_words(now, now - 60_000), "1 min ago");
        assert_eq!(relative_words(now, now - 3 * MS_PER_MIN), "3 min ago");
        assert_eq!(relative_words(now, now - 2 * MS_PER_HOUR), "2 h ago");
        assert_eq!(relative_words(now, now - 30 * MS_PER_HOUR), "yesterday");
        assert_eq!(relative_words(now, now - 3 * MS_PER_DAY), "2025-09-18");
    }

    /// The goldens `format_rfc3339` is pinned to: the epoch, a plain date, a
    /// leap day, the pre-leap-day boundary — each naming its zone.
    #[test]
    fn stamp_words_match_the_rfc3339_goldens() {
        assert_eq!(stamp_words(0), "1970-01-01 00:00:00 UTC");
        assert_eq!(stamp_words(1_700_000_000_000), "2023-11-14 22:13:20 UTC");
        assert_eq!(stamp_words(1_709_164_800_000), "2024-02-29 00:00:00 UTC");
        assert_eq!(stamp_words(1_709_164_799_000), "2024-02-28 23:59:59 UTC");
        assert_eq!(stamp_words(1_758_470_000_123), "2025-09-21 15:53:20 UTC");
    }

    #[test]
    fn home_abbreviation_is_the_tab_label_rule() {
        let home = Some("/Users//w");
        assert_eq!(home_abbreviate("/Users//w", home), "~");
        assert_eq!(
            home_abbreviate("/Users//w/Library/x.log", home),
            "~/Library/x.log"
        );
        assert_eq!(home_abbreviate("/Users//wx/y", home), "/Users//wx/y");
        assert_eq!(home_abbreviate("/tmp/x", home), "/tmp/x");
        assert_eq!(home_abbreviate("/Users//w/x", None), "/Users//w/x");
        assert_eq!(home_abbreviate("/Users//w/x", Some("")), "/Users//w/x");
        // A trailing slash on the home is the same home.
        assert_eq!(
            home_abbreviate("/Users//w/x.log", Some("/Users//w/")),
            "~/x.log"
        );
        assert_eq!(home_abbreviate("/Users//w", Some("/Users//w/")), "~");
        assert_eq!(
            home_abbreviate("/Users//wx/y", Some("/Users//w/")),
            "/Users//wx/y"
        );
        assert_eq!(
            home_abbreviate("/Users//w", Some("/")),
            "/Users//w",
            "the root is nobody's home"
        );
        assert_eq!(home_abbreviate("/Users//w", Some("//")), "/Users//w");
        assert_eq!(
            abbreviate_paths_in("crash log at /Users//w/Library/x.log under /Users//w", home),
            "crash log at ~/Library/x.log under ~"
        );
        assert_eq!(abbreviate_paths_in("no paths here", home), "no paths here");
        assert_eq!(abbreviate_paths_in("/Users//w/x", None), "/Users//w/x");
    }

    /// The finished forms (ruling 154): the leading participle only, through
    /// the closed table; anything else keeps its words.
    #[test]
    fn a_work_title_reads_finished_through_the_closed_table() {
        for (ing, done) in FINISHED_VERBS {
            assert_eq!(finished_form(ing).as_deref(), Some(done));
            assert_eq!(
                finished_form(&format!("{ing} aterm v0.91.0")),
                Some(format!("{done} aterm v0.91.0"))
            );
        }
        assert_eq!(
            finished_form("Installing ALab tools").as_deref(),
            Some("Installed ALab tools")
        );
        assert_eq!(
            finished_form("Building index").as_deref(),
            Some("Built index")
        );
        for kept in [
            "aterm vX is ready",
            "downloading aterm",
            "Downloadingx aterm",
            "Re-Downloading",
            "",
            "Working",
        ] {
            assert_eq!(finished_form(kept), None, "{kept:?} keeps its words");
        }
    }

    #[test]
    fn copy_text_is_title_stamp_and_lines() {
        let rec = LogRecord {
            id: MessageId::FIRST,
            stamp: WallStamp {
                unix_ms: 1_700_000_000_000,
            },
            tag: tags::CRASH,
            severity: Severity::Error,
            glyph: Glyph::FALLBACK,
            title: "aterm closed unexpectedly last time".into(),
            detail: vec!["crash log at /x".into(), "signal 11".into()],
            actions: vec![],
            key: None,
            origin: Origin::Host,
            repeats: 1,
            state: LogState::Posted,
            retired_unix_ms: None,
            retired_at: None,
            last_action: None,
        };
        assert_eq!(
            copy_text(&rec),
            "aterm closed unexpectedly last time\n2023-11-14 22:13:20 UTC \u{00b7} crash \u{00b7} error\ncrash log at /x\nsignal 11"
        );
    }
}
