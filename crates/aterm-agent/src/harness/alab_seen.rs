// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! The journal-backed account surfaces (`docs/DESIGN-alab-lab-model-2026-11-01.md` §7, §8, step
//! 11): who last used an account and on which Mac, where it is signed in, its quota per window
//! with the reading's age, Mac and source, and its history across Macs.
//!
//! `aterm-agent` does not link `aterm-db`, so the journal reaches it as a [`Seen`] the `aterm`
//! executable reads in the background from the journal window and the registry's LabView
//! (`crates/aterm/src/alab_seen.rs`) and hands over through a [`Source`] it installs at start, as
//! the lab's roster source is. Nothing here calls a provider or writes anything. A surface on a
//! typing or painting path asks with `wait: false` and gets what is held, or nothing yet; a verb a
//! person ran (`aterm harness accounts`, `accounts history`) waits for one read. With no source
//! installed (a test, a debug build, `[alab] roster = false`) there is nothing to show and every
//! surface shows what this Mac's own records hold, as before.

use std::collections::BTreeMap;
use std::sync::{Arc, OnceLock};

/// A Mac as the journal names it: its machine label where the registry binds its Commons
/// enrollment to a live machine, else `uuid:<8 hex>` of the enrollment.
pub type Mac = String;

/// One account's last use.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct LastUse {
    pub by: Mac,
    /// When, by that Mac's clock (Unix seconds).
    pub t: i64,
}

/// One Mac's latest sign-in word for an account.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct SignedIn {
    pub on: Mac,
    /// `in`, `out`, `ended`, `wrong` or `failed`.
    pub state: String,
    pub t: i64,
}

/// What one window's readings say.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Figure {
    /// The most used of the current window's readings; `reset` is `None` where the readings
    /// disagree on it beyond their resolution.
    Used { pct_x10: u16, reset: Option<i64> },
    /// Every reset read has passed and nothing was read since.
    ResetUnread { reset: i64 },
}

/// One window of an account (or of one of its workspaces).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct WindowSeen {
    /// `5h`, `7d`, `7d-model`, `day`, `month` or `credits`.
    pub window: String,
    /// The workspace's sub-account key where the window is a workspace's.
    pub sub: Option<String>,
    pub figure: Option<Figure>,
    /// Original time, Mac and source of the quota reading retained with this window.
    pub read: Option<(i64, Mac, String)>,
    /// A wall standing at the read, until when.
    pub wall_until: Option<Option<i64>>,
}

impl WindowSeen {
    /// Project this retained reading at `now`: a passed wall is lifted and a passed reset
    /// becomes unknown until read again. Unknown bounds and the original reading are kept.
    #[must_use]
    pub fn as_of(&self, now: i64) -> Self {
        let mut window = self.clone();
        if matches!(window.wall_until, Some(Some(until)) if until <= now) {
            window.wall_until = None;
        }
        if let Some(Figure::Used {
            reset: Some(reset), ..
        }) = window.figure
            && reset <= now
        {
            window.figure = Some(Figure::ResetUnread { reset });
        }
        window
    }
}

/// One line of an account's history across Macs.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Line {
    pub t: i64,
    pub by: Mac,
    /// The journal's event word (`session-start`, `signed-in`, `quota`, …).
    pub kind: String,
    pub cli: String,
    /// The closed words the event carries (window, figure, reason), never free text.
    pub detail: String,
    /// Read from the reading Mac's heard archive rather than a window table.
    pub heard: bool,
}

/// What the journal says of one account.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct AccountSeen {
    pub last_use: Option<LastUse>,
    pub signed_in: Vec<SignedIn>,
    pub windows: Vec<WindowSeen>,
    /// Its events, oldest first, at most [`HISTORY_MAX`].
    pub history: Vec<Line>,
}

/// The most history lines one account keeps in memory.
pub const HISTORY_MAX: usize = 512;

/// What the journal says, by account key (`ac-…`), as of [`Self::at`].
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Seen {
    pub accounts: BTreeMap<String, AccountSeen>,
    /// When it was read (Unix seconds).
    pub at: i64,
}

/// What gives the harness its [`Seen`]: the `aterm` executable's cache.
pub trait Source: Send + Sync {
    /// The journal as last read. `wait`: read once now when nothing is held yet (a verb a person
    /// ran); otherwise answer at once with what is held, starting a background read if none is.
    fn seen(&self, wait: bool) -> Option<Arc<Seen>>;
}

static INSTALLED: OnceLock<Box<dyn Source>> = OnceLock::new();

/// Install the process's [`Source`], once; a second install is refused.
pub fn install(source: Box<dyn Source>) -> bool {
    INSTALLED.set(source).is_ok()
}

#[cfg(test)]
thread_local! {
    static OVERRIDE: std::cell::RefCell<Option<Arc<Seen>>> = const { std::cell::RefCell::new(None) };
}

/// The journal as this process holds it ([`Source::seen`]); `None` with no source installed.
#[must_use]
pub fn current(wait: bool) -> Option<Arc<Seen>> {
    #[cfg(test)]
    if let Some(seen) = OVERRIDE.with(|cell| cell.borrow().clone()) {
        return Some(seen);
    }
    INSTALLED.get()?.seen(wait)
}

/// Run `f` with `seen` as this thread's [`current`] journal.
#[cfg(test)]
pub(crate) fn with_seen<T>(seen: Seen, f: impl FnOnce() -> T) -> T {
    OVERRIDE.with(|cell| *cell.borrow_mut() = Some(Arc::new(seen)));
    let out = f();
    OVERRIDE.with(|cell| *cell.borrow_mut() = None);
    out
}

/// `seconds` as a short age: `40 s`, `12 min`, `3 h`, `2 d`.
#[must_use]
pub fn age(seconds: i64) -> String {
    duration_words(u64::try_from(seconds).unwrap_or_default())
}

fn duration_words(seconds: u64) -> String {
    match seconds {
        0..60 => format!("{seconds} s"),
        60..3_600 => format!("{} min", seconds / 60),
        3_600..172_800 => format!("{} h", seconds / 3_600),
        _ => format!("{} d", seconds / 86_400),
    }
}

/// "m17, about 3 h ago (its clock)", or "… ahead (its clock)" for a time past `now`.
#[must_use]
pub fn when(by: &str, t: i64, now: i64) -> String {
    let age = duration_words(now.abs_diff(t));
    if t > now {
        format!("{by}, {age} ahead (its clock)")
    } else {
        format!("{by}, about {age} ago (its clock)")
    }
}

/// The retained quota reading's original Mac, source and source-clock age or lead.
#[must_use]
pub fn read_words(t: i64, by: &str, source: &str, now: i64) -> String {
    format!("read on {} ({source})", when(by, t, now))
}

/// Each Mac's retained sign-in word, with its age or clock lead at the projection time.
/// These records do not establish a current provider login.
#[must_use]
pub fn signed_in_words(records: &[SignedIn], now: i64) -> Option<String> {
    (!records.is_empty()).then(|| {
        records
            .iter()
            .map(|record| when(&format!("{} ({})", record.on, record.state), record.t, now))
            .collect::<Vec<_>>()
            .join("; ")
    })
}

fn percent(pct_x10: u16) -> String {
    if pct_x10.is_multiple_of(10) {
        format!("{}%", pct_x10 / 10)
    } else {
        format!("{}.{}%", pct_x10 / 10, pct_x10 % 10)
    }
}

/// One window as a line: its figure, reset, reading provenance and standing wall. A reading
/// whose source clock is ahead says so; an unread reset is never shown as zero usage.
#[must_use]
pub fn window_line(w: &WindowSeen, now: i64) -> String {
    let w = w.as_of(now);
    let mut out = match &w.sub {
        Some(sub) => format!("{} (workspace {sub})", w.window),
        None => w.window.clone(),
    };
    match &w.figure {
        Some(Figure::Used { pct_x10, reset }) => {
            out.push_str(&format!(" {} used", percent(*pct_x10)));
            match reset {
                Some(reset) => {
                    out.push_str(&format!(", resets {}", super::usage::rfc3339_utc(*reset)));
                }
                None => out.push_str(", reset unknown"),
            }
        }
        Some(Figure::ResetUnread { reset }) => out.push_str(&format!(
            " reset at {}, not read since",
            super::usage::rfc3339_utc(*reset)
        )),
        None => {}
    }
    if let Some((t, by, src)) = &w.read {
        out.push_str(&format!(", {}", read_words(*t, by, src, now)));
    }
    if let Some(until) = w.wall_until {
        match until {
            Some(until) => out.push_str(&format!(
                ", at its limit until {}",
                super::usage::rfc3339_utc(until)
            )),
            None => out.push_str(", at its limit"),
        }
    }
    out
}

/// What the journal says of `account` as lines, for a report: last used by, sign-in records,
/// and each window. Empty where it says nothing.
#[must_use]
pub fn lines(account: &AccountSeen, now: i64) -> Vec<String> {
    let mut out = Vec::new();
    if let Some(last) = &account.last_use {
        out.push(format!("last used by {}", when(&last.by, last.t, now)));
    }
    if let Some(records) = signed_in_words(&account.signed_in, now) {
        out.push(format!("sign-in records: {records}"));
    }
    out.extend(account.windows.iter().map(|w| window_line(w, now)));
    out
}

/// The current row's canonical claims key. Historical label state and registry alias matches
/// do not establish which account the row names now.
#[must_use]
pub fn row_key(cli: super::accounts::Cli, row: &super::accounts::Row) -> Option<String> {
    super::journal::account_key(cli, row).map(|key| key.as_str().to_owned())
}

/// One roster row the journal says something of: its label, its account key, and what it says.
pub type Reported = (String, String, AccountSeen);

/// Each row of `roster` the journal says something of, in roster order.
#[must_use]
pub fn rows(
    cli: super::accounts::Cli,
    roster: &super::accounts::Roster,
    seen: &Seen,
) -> Vec<Reported> {
    roster
        .rows
        .iter()
        .filter_map(|row| {
            let pk = row_key(cli, row)?;
            let found = seen.accounts.get(&pk)?;
            Some((row.label.clone(), pk, found.clone()))
        })
        .collect()
}

/// The report's `journal` section: per row label, its account key, last use, sign-ins and
/// windows, each time with its signed age at `now`, or null if that age cannot fit in an i64.
#[must_use]
pub fn rows_json(rows: &[Reported], now: i64) -> aterm_json::Value {
    use aterm_json::{Map, Value, json};
    let mut out = Map::new();
    for (label, pk, seen) in rows {
        let windows: Vec<Value> = seen
            .windows
            .iter()
            .map(|w| {
                let w = w.as_of(now);
                let (state, pct, reset) = match &w.figure {
                    Some(Figure::Used { pct_x10, reset }) => (
                        if reset.is_some() {
                            "current"
                        } else {
                            "reset-unknown"
                        },
                        Some(*pct_x10),
                        *reset,
                    ),
                    Some(Figure::ResetUnread { reset }) => ("reset-unread", None, Some(*reset)),
                    None if w.wall_until.is_some() => ("wall-only", None, None),
                    None => ("unknown", None, None),
                };
                json!({
                    "window": w.window,
                    "sub": w.sub,
                    "state": state,
                    "used_pct_x10": pct,
                    "reset": reset,
                    "read": w.read.as_ref().map(|(t, by, src)| json!({
                        "t": t, "age_s": now.checked_sub(*t), "by": by, "src": src,
                    })),
                    "wall_until": w.wall_until.map(|u| json!(u)),
                    "line": window_line(&w, now),
                })
            })
            .collect();
        out.insert(
            label.clone(),
            json!({
                "ac": pk,
                "last_use": seen.last_use.as_ref().map(|l| json!({
                    "by": l.by, "t": l.t, "age_s": now.checked_sub(l.t), "clock": "its",
                })),
                "signed_in": seen.signed_in.iter().map(|s| json!({
                    "on": s.on, "state": s.state, "t": s.t,
                })).collect::<Vec<_>>(),
                "windows": windows,
            }),
        );
    }
    Value::Object(out)
}

/// The report's journal section as text, after the local report.
#[must_use]
pub fn rows_text(rows: &[Reported], now: i64) -> String {
    let mut out = String::from("\nlab journal (what every Mac saw; no provider asked):\n");
    for (label, _, seen) in rows {
        for line in lines(seen, now) {
            out.push_str(&format!("  {label}: {line}\n"));
        }
    }
    out
}

/// What `accounts history <name>` asks about: a row's label on this Mac and the account's key,
/// either or both.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Asked {
    pub label: Option<String>,
    pub pk: Option<String>,
}

/// Which account `name` is: a row label here, an account key (`ac-…`), or a lab label or legacy
/// label. An address is never a name. `None` where nothing goes by it.
#[must_use]
pub fn resolve(
    cli: super::accounts::Cli,
    name: &str,
    roster: &super::accounts::Roster,
    lab: Option<&super::alab_roster::Lab>,
) -> Option<Asked> {
    if name.contains('@') {
        return None;
    }
    let explicit = name.starts_with("ac-") && super::journal::Key::new(name).is_some();
    if !explicit && let Some(row) = roster.rows.iter().find(|row| row.label == name) {
        return Some(Asked {
            label: Some(row.label.clone()),
            pk: row_key(cli, row),
        });
    }
    // Explicit account keys retain their history independently of current roster labels.
    let pk = if explicit {
        Some(name.to_owned())
    } else {
        lab.and_then(|lab| {
            lab.of(cli)
                .find(|a| a.label == name || a.legacy_label.as_deref() == Some(name))
                .map(|a| a.pk.clone())
        })
    }?;
    let label = roster
        .rows
        .iter()
        .find(|row| row_key(cli, row).as_deref() == Some(pk.as_str()))
        .map(|row| row.label.clone());
    Some(Asked {
        label,
        pk: Some(pk),
    })
}

/// The history of `asked`, oldest first: this Mac's ledger rows about it (by key, or by label for
/// a row written before its key was learned), as `here`, and every Mac's journal events about it.
/// A ledger row's free-text reason is never shown, only its kind and the other row's label.
#[must_use]
pub fn history(
    asked: &Asked,
    ledger: &[super::accounts::Event],
    journal: Option<&AccountSeen>,
) -> Vec<String> {
    let mut out: Vec<(i64, u8, String)> = Vec::new();
    for event in ledger {
        let mine = match (&asked.pk, &event.pk) {
            (Some(pk), Some(key)) => pk == key,
            _ => asked.label.as_deref() == Some(event.account.as_str()),
        } || asked.label.is_some() && event.to.as_deref() == asked.label.as_deref();
        if !mine {
            continue;
        }
        let mut line = format!(
            "{} here {} {}",
            super::usage::rfc3339_utc(event.t),
            event.kind.as_str(),
            event.account
        );
        if let Some(to) = &event.to {
            line.push_str(&format!(" to={to}"));
        }
        out.push((event.t, 0, line));
    }
    for line in journal
        .map(|seen| seen.history.as_slice())
        .unwrap_or_default()
    {
        out.push((line.t, 1, history_line(line)));
    }
    out.sort();
    out.into_iter().map(|(_, _, line)| line).collect()
}

/// One history line as text: "2026-11-03T17:00:00Z m17 signed-in claude".
#[must_use]
pub fn history_line(line: &Line) -> String {
    let mut out = format!(
        "{} {} {} {}",
        super::usage::rfc3339_utc(line.t),
        line.by,
        line.kind,
        line.cli
    );
    if !line.detail.is_empty() {
        out.push(' ');
        out.push_str(&line.detail);
    }
    if line.heard {
        out.push_str(" (heard by this Mac)");
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    fn seen_account() -> AccountSeen {
        AccountSeen {
            last_use: Some(LastUse {
                by: "m17".into(),
                t: 1_000 - 3 * 3_600,
            }),
            signed_in: vec![
                SignedIn {
                    on: "m3".into(),
                    state: "in".into(),
                    t: 900,
                },
                SignedIn {
                    on: "m17".into(),
                    state: "out".into(),
                    t: 800,
                },
            ],
            windows: vec![
                WindowSeen {
                    window: "5h".into(),
                    sub: None,
                    figure: Some(Figure::Used {
                        pct_x10: 425,
                        reset: None,
                    }),
                    read: Some((1_000 - 1_200, "m3".into(), "statusline".into())),
                    wall_until: None,
                },
                WindowSeen {
                    window: "7d".into(),
                    sub: None,
                    figure: Some(Figure::ResetUnread { reset: 0 }),
                    read: None,
                    wall_until: Some(None),
                },
            ],
            history: Vec::new(),
        }
    }

    /// A report says who last used the account, by its clock, the retained sign-in records, and
    /// each window with its age, Mac and source; a reset nobody read since is never shown as 0.
    /// NEGATIVE CONTROL: with `window_line` printing a `ResetUnread` figure as `0% used`, the
    /// 7d line reads `7d 0% used…`: `left: "7d 0% used, at its limit"`.
    #[test]
    fn a_report_names_the_mac_the_age_and_never_a_guessed_zero() {
        let held = seen_account();
        let original = held.clone();
        let lines = super::lines(&held, 1_000);
        assert_eq!(lines[0], "last used by m17, about 3 h ago (its clock)");
        for part in [
            "m3 (in), about 1 min ago (its clock)",
            "m17 (out), about 3 min ago (its clock)",
        ] {
            assert!(lines[1].contains(part), "{}", lines[1]);
        }
        assert_eq!(
            lines[2],
            "5h 42.5% used, reset unknown, read on m3, about 20 min ago (its clock) (statusline)"
        );
        assert_eq!(
            lines[3],
            "7d reset at 1970-01-01T00:00:00Z, not read since, at its limit"
        );
        assert!(lines.iter().all(|l| !l.contains('@')));
        let later = super::lines(&held, 1_000 + 3 * 86_400);
        assert!(later[1].contains("m3 (in), about 3 d ago (its clock)"));
        assert!(later[1].contains("m17 (out), about 3 d ago (its clock)"));
        let ahead = super::lines(&held, 700);
        assert!(ahead[1].contains("m3 (in), 3 min ahead (its clock)"));
        assert!(ahead[1].contains("m17 (out), 1 min ahead (its clock)"));
        assert_eq!(held, original);
        assert_eq!(signed_in_words(&[], 1_000), None);
    }

    /// A cached reading crossing its known reset is unknown in both report forms, while the
    /// retained reading and its provenance stay intact.
    /// NEGATIVE CONTROL: normalizing only `window_line` leaves JSON's state current and its
    /// positive quota and wall fields present after the reset.
    #[test]
    fn cached_quota_reports_expire_together_without_changing_the_reading() {
        let rows = vec![(
            "one".into(),
            "ac-0123456789abcdef".into(),
            AccountSeen {
                windows: vec![
                    WindowSeen {
                        window: "5h".into(),
                        sub: None,
                        figure: Some(Figure::Used {
                            pct_x10: 1000,
                            reset: Some(300),
                        }),
                        read: Some((200, "m17".into(), "statusline".into())),
                        wall_until: Some(Some(300)),
                    },
                    WindowSeen {
                        window: "7d".into(),
                        sub: None,
                        figure: None,
                        read: None,
                        wall_until: Some(Some(300)),
                    },
                ],
                ..Default::default()
            },
        )];
        let original = rows.clone();
        let before = rows_json(&rows, 299);
        let window = &before["one"]["windows"][0];
        assert_eq!(window["state"].as_str(), Some("current"));
        assert_eq!(window["used_pct_x10"].as_u64(), Some(1000));
        assert_eq!(window["wall_until"].as_i64(), Some(300));
        assert_eq!(
            before["one"]["windows"][1]["state"].as_str(),
            Some("wall-only")
        );
        for now in [300, 301] {
            let json = rows_json(&rows, now);
            let window = &json["one"]["windows"][0];
            assert_eq!(window["state"].as_str(), Some("reset-unread"));
            assert!(window["used_pct_x10"].is_null());
            assert!(window["wall_until"].is_null());
            assert_eq!(window["reset"].as_i64(), Some(300));
            assert_eq!(window["read"]["t"].as_i64(), Some(200));
            assert_eq!(window["read"]["by"].as_str(), Some("m17"));
            assert_eq!(window["read"]["src"].as_str(), Some("statusline"));
            let line = window["line"].as_str().unwrap();
            assert!(line.contains("not read since"), "{line}");
            assert!(!line.contains("100%") && !line.contains("at its limit"));
            assert!(rows_text(&rows, now).contains(line));
            let wall_only = &json["one"]["windows"][1];
            assert_eq!(wall_only["state"].as_str(), Some("unknown"));
            assert!(wall_only["wall_until"].is_null());
            assert!(!wall_only["line"].as_str().unwrap().contains("at its limit"));
        }
        assert_eq!(rows, original);
    }

    #[test]
    fn quota_read_provenance_tracks_the_source_clock_without_changing_the_window() {
        let reading = 1_120;
        let held = vec![(
            "one".into(),
            "ac-0123456789abcdef".into(),
            AccountSeen {
                last_use: Some(LastUse {
                    by: "m3".into(),
                    t: reading,
                }),
                windows: vec![WindowSeen {
                    window: "5h".into(),
                    sub: None,
                    figure: Some(Figure::Used {
                        pct_x10: 420,
                        reset: Some(1_300),
                    }),
                    read: Some((reading, "m3".into(), "statusline".into())),
                    wall_until: Some(Some(1_300)),
                }],
                ..Default::default()
            },
        )];
        let original = held.clone();
        for (now, words) in [
            (1_000, "2 min ahead"),
            (reading, "0 s ago"),
            (1_240, "2 min ago"),
            (1_300, "3 min ago"),
        ] {
            let json = rows_json(&held, now);
            let shown = &json["one"]["windows"][0];
            let line = shown["line"].as_str().unwrap();
            for part in [words, "m3", "its clock", "statusline"] {
                assert!(line.contains(part), "{line}");
            }
            assert_eq!(shown["read"]["t"].as_i64(), Some(reading));
            assert_eq!(shown["read"]["age_s"].as_i64(), Some(now - reading));
            assert_eq!(
                json["one"]["last_use"]["age_s"].as_i64(),
                Some(now - reading)
            );
            if now < 1_300 {
                assert_eq!(shown["state"].as_str(), Some("current"));
                assert_eq!(shown["used_pct_x10"].as_u64(), Some(420));
                assert_eq!(shown["wall_until"].as_i64(), Some(1_300));
            } else {
                assert_eq!(shown["state"].as_str(), Some("reset-unread"));
                assert!(shown["used_pct_x10"].is_null());
                assert!(shown["wall_until"].is_null());
            }
            assert!(rows_text(&held, now).contains(line));
        }
        let mut no_read = held[0].2.windows[0].clone();
        no_read.read = None;
        assert!(!window_line(&no_read, 1_000).contains("read on"));
        assert_eq!(held, original);
    }

    #[test]
    fn relative_provenance_keeps_raw_times_when_the_signed_age_cannot_fit() {
        for (now, t, direction) in [(i64::MIN, i64::MAX, "ahead"), (i64::MAX, i64::MIN, "ago")] {
            let window = WindowSeen {
                window: "5h".into(),
                sub: None,
                figure: Some(Figure::Used {
                    pct_x10: 420,
                    reset: None,
                }),
                read: Some((t, "m3".into(), "statusline".into())),
                wall_until: None,
            };
            let shown = rows_json(
                &[(
                    "one".into(),
                    "ac-0123456789abcdef".into(),
                    AccountSeen {
                        last_use: Some(LastUse { by: "m3".into(), t }),
                        windows: vec![window.clone()],
                        ..Default::default()
                    },
                )],
                now,
            );
            let read = &shown["one"]["windows"][0]["read"];
            assert_eq!(read["t"].as_i64(), Some(t));
            assert!(read["age_s"].is_null());
            assert_eq!(shown["one"]["last_use"]["t"].as_i64(), Some(t));
            assert!(shown["one"]["last_use"]["age_s"].is_null());
            let line = window_line(&window, now);
            assert!(line.contains(direction), "{line}");
            assert!(line.contains(&format!("{} d", u64::MAX / 86_400)), "{line}");
            assert_eq!(
                shown["one"]["windows"][0]["used_pct_x10"].as_u64(),
                Some(420)
            );
            assert_eq!(window.read.as_ref().unwrap().0, t);
        }
    }

    fn roster(label: &str, email: &str) -> super::super::accounts::Roster {
        super::super::accounts::Roster {
            rows: vec![super::super::accounts::Row {
                label: label.into(),
                email: Some(email.into()),
                ..Default::default()
            }],
        }
    }

    #[test]
    fn readonly_journal_attribution_follows_an_externally_replaced_identity() {
        use super::super::accounts::{self, Cli, Store};
        const ANN: &str = "ac-0123456789abcdef";
        const BOB: &str = "ac-fedcba9876543210";
        let keyer = |_: &str, email: &str| match email {
            "ann@example.test" => Some(ANN.to_owned()),
            "bob@example.test" => Some(BOB.to_owned()),
            _ => None,
        };
        let scratch = aterm_tempfile::tempdir().unwrap();
        let home = scratch.path().join("home");
        std::fs::create_dir(&home).unwrap();
        let store =
            Store::at_for(&scratch.path().join("harness"), Cli::Claude, &home).bounded_by(&[]);
        let path = scratch.path().join("accounts.toml");
        let old = roster("work", "ann@example.test");
        std::fs::write(
            &path,
            accounts::append_rows_for(Cli::Claude, "", &old.rows).unwrap(),
        )
        .unwrap();
        store
            .transact(1_000, &mut None, |state| {
                state.accounts.entry("work".into()).or_default();
                super::super::journal::learn_with(Cli::Claude, &old, state, &keyer);
                Ok(Vec::new())
            })
            .unwrap();
        let replacement = roster("work", "bob@example.test");
        std::fs::write(
            &path,
            accounts::append_rows_for(Cli::Claude, "", &replacement.rows).unwrap(),
        )
        .unwrap();
        let images = || {
            [
                path.clone(),
                store.state_file(),
                store.logins_file(),
                store.ledger_path(),
                store.wake_path(),
                store.state_witness_file(),
                store.logins_witness_file(),
            ]
            .map(|path| std::fs::read(path).ok())
        };
        let before = images();
        let records = store.read_account_records(Some(&path), 1_000).unwrap();
        assert_eq!(records.state.accounts["work"].pk.as_deref(), Some(ANN));
        assert_eq!(
            records
                .roster
                .row("work")
                .unwrap()
                .expected_identity_email(),
            Some("bob@example.test")
        );
        assert_eq!(images(), before, "the read must not rekey persisted state");
        let ann = seen_account();
        let mut bob = ann.clone();
        bob.last_use.as_mut().unwrap().by = "m22".into();
        bob.windows[0].figure = Some(Figure::Used {
            pct_x10: 950,
            reset: Some(2_000),
        });
        let seen = Seen {
            accounts: [(ANN.into(), ann), (BOB.into(), bob.clone())].into(),
            at: 1_000,
        };
        let reported =
            super::super::journal::with_keyer(keyer, || rows(Cli::Claude, &records.roster, &seen));
        assert_eq!(reported.len(), 1);
        assert_eq!(
            reported[0].1, BOB,
            "a reused label is not current identity evidence"
        );
        assert_eq!(reported[0].2, bob);
        let json = rows_json(&reported, 1_000);
        assert_eq!(
            json["work"]["windows"][0]["used_pct_x10"].as_u64(),
            Some(950)
        );
        assert_eq!(json["work"]["last_use"]["by"].as_str(), Some("m22"));
        let old_history = super::super::journal::with_keyer(keyer, || {
            resolve(Cli::Claude, ANN, &records.roster, None).unwrap()
        });
        assert_eq!(old_history.pk.as_deref(), Some(ANN));
        assert_eq!(
            old_history.label, None,
            "the replaced row no longer names Ann"
        );
        assert_eq!(images(), before);
    }

    #[test]
    fn explicit_history_keys_survive_a_colliding_replacement_label() {
        use super::super::{
            accounts::{Cli, Event, Kind},
            journal,
        };
        const ANN: &str = "ac-0123456789abcdef";
        const BOB: &str = "ac-fedcba9876543210";
        let mut current = roster(ANN, "bob@example.test");
        current.rows.push(super::super::accounts::Row {
            label: "sa-0123456789abcdef".into(),
            email: Some("bob@example.test".into()),
            ..Default::default()
        });
        journal::with_keyer(
            |realm, email| {
                (realm == "claude" && email == "bob@example.test").then(|| BOB.to_owned())
            },
            || {
                let old = resolve(Cli::Claude, ANN, &current, None).unwrap();
                assert_eq!(
                    old,
                    Asked {
                        label: None,
                        pk: Some(ANN.into())
                    }
                );
                let replacement = resolve(Cli::Claude, BOB, &current, None).unwrap();
                assert_eq!(replacement.label.as_deref(), Some(ANN));
                assert_eq!(replacement.pk.as_deref(), Some(BOB));
                let sub_like_label =
                    resolve(Cli::Claude, "sa-0123456789abcdef", &current, None).unwrap();
                assert_eq!(sub_like_label.pk.as_deref(), Some(BOB));
                let mut old_event = Event::new(10, Kind::Auth, "before");
                old_event.pk = Some(ANN.into());
                let mut new_event = Event::new(20, Kind::Auth, ANN);
                new_event.pk = Some(BOB.into());
                let lines = history(
                    &old,
                    &[old_event, new_event, Event::new(30, Kind::Rotate, ANN)],
                    None,
                );
                assert_eq!(
                    lines.len(),
                    1,
                    "old key must not borrow the replacement label's events"
                );
                assert!(lines[0].contains(" here auth before"));
            },
        );
    }

    #[test]
    fn unresolved_current_identity_keeps_local_history_without_borrowing_lab_aliases() {
        use super::super::{
            accounts::{Cli, Event, Kind},
            alab_roster::{Lab, LabAccount},
            journal,
        };
        const ANN: &str = "ac-0123456789abcdef";
        let mut current = roster("work", "ann@example.test");
        current.rows[0].identity_email = Some("bob@example.test".into());
        let lab = Lab {
            accounts: vec![LabAccount {
                pk: ANN.into(),
                cli: Cli::Claude,
                vendor: Default::default(),
                label: "ann".into(),
                legacy_label: Some("before".into()),
                email: "ann@example.test".into(),
                identity: None,
                browser: None,
                joinable: true,
                preferred: Vec::new(),
            }],
            ..Lab::default()
        };
        let seen = Seen {
            accounts: [(ANN.into(), seen_account())].into(),
            at: 1_000,
        };
        journal::with_keyer(
            |_, _| None,
            || {
                let asked = resolve(Cli::Claude, "work", &current, Some(&lab)).unwrap();
                assert_eq!(
                    asked,
                    Asked {
                        label: Some("work".into()),
                        pk: None
                    }
                );
                assert!(rows(Cli::Claude, &current, &seen).is_empty());
                assert_eq!(
                    history(&asked, &[Event::new(10, Kind::Auth, "work")], None).len(),
                    1
                );
                for name in [ANN, "ann", "before"] {
                    let old = resolve(Cli::Claude, name, &current, Some(&lab)).unwrap();
                    assert_eq!(
                        old,
                        Asked {
                            label: None,
                            pk: Some(ANN.into())
                        }
                    );
                }
            },
        );
    }

    /// A forgotten row's observations stay under its account key (through `state.json`), and the
    /// row that comes back for that account under another label takes them again; a row of
    /// another account takes nothing.
    /// NEGATIVE CONTROL: with `forget_account` not keeping the removed row (`remember_forgotten`
    /// call removed), the new row starts blind: `assertion failed: back.walls.contains_key("5h")`.
    #[test]
    fn a_forgotten_row_keeps_its_history_under_its_account_key() {
        use super::super::accounts::{self, Account, Logins, State, Wall};
        const KEY: &str = "ac-0123456789abcdef";
        let keyer = |_: &str, email: &str| (email == "ann@example.test").then(|| KEY.to_owned());
        let mut state = State::default();
        let mut walls = BTreeMap::new();
        walls.insert(
            "5h".to_owned(),
            Wall {
                until: Some(2_000),
                at: 1_000,
                by: "aa".into(),
                attempts: 0,
                cleared: false,
            },
        );
        state.accounts.insert(
            "old".into(),
            Account {
                pk: Some(KEY.into()),
                walls,
                ran_at: Some(900),
                auth: accounts::Auth::Ok,
                ..Default::default()
            },
        );
        accounts::forget_account(&mut state, &mut Logins::default(), "old");
        assert!(!state.accounts.contains_key("old"));
        let mut state = State::from_json(&state.to_json(), 1_500).unwrap();
        assert!(state.forgotten.contains_key(KEY), "{:?}", state.forgotten);
        super::super::journal::learn_with(
            accounts::Cli::Claude,
            &roster("other", "bob@example.test"),
            &mut state,
            &keyer,
        );
        assert!(
            state.forgotten.contains_key(KEY),
            "another account takes nothing"
        );
        super::super::journal::learn_with(
            accounts::Cli::Claude,
            &roster("new", "ann@example.test"),
            &mut state,
            &keyer,
        );
        let back = &state.accounts["new"];
        assert!(back.walls.contains_key("5h"));
        assert_eq!(back.ran_at, Some(900));
        assert_eq!(back.pk.as_deref(), Some(KEY));
        assert_eq!(
            back.auth,
            accounts::Auth::Unseen,
            "the old login's word stays behind"
        );
        assert!(state.forgotten.is_empty());
    }

    /// A row whose email is edited to another account is keyed as that account, and what this
    /// Mac observed under it stays with the account it was keyed as: filed under the old key, so a
    /// later forget of the label files nothing under the new one and the new account's row is not
    /// walled by the old login's wall. Its login's word stays with the label.
    /// NEGATIVE CONTROL: with `learn_with` re-keying in place (`account.pk = Some(pk)`, no
    /// `take_observations`), the old wall is carried to the new account:
    /// `assertion failed: !state.accounts["x"].walls.contains_key("5h")`.
    #[test]
    fn a_row_edited_to_another_account_leaves_its_observations_with_the_old_one() {
        use super::super::accounts::{self, Account, Logins, State, Wall};
        const ANN: &str = "ac-0123456789abcdef";
        const BOB: &str = "ac-fedcba9876543210";
        let keyer = |_: &str, email: &str| match email {
            "ann@example.test" => Some(ANN.to_owned()),
            "bob@example.test" => Some(BOB.to_owned()),
            _ => None,
        };
        let mut state = State::default();
        let mut walls = BTreeMap::new();
        walls.insert(
            "5h".to_owned(),
            Wall {
                until: Some(2_000),
                at: 1_000,
                by: "aa".into(),
                attempts: 0,
                cleared: false,
            },
        );
        state.accounts.insert(
            "x".into(),
            Account {
                pk: Some(ANN.into()),
                walls,
                auth: accounts::Auth::Ok,
                ..Default::default()
            },
        );
        super::super::journal::learn_with(
            accounts::Cli::Claude,
            &roster("x", "bob@example.test"),
            &mut state,
            &keyer,
        );
        assert_eq!(state.accounts["x"].pk.as_deref(), Some(BOB));
        assert!(!state.accounts["x"].walls.contains_key("5h"));
        assert_eq!(state.accounts["x"].auth, accounts::Auth::Ok);
        assert!(state.forgotten[ANN].walls.contains_key("5h"));
        accounts::forget_account(&mut state, &mut Logins::default(), "x");
        super::super::journal::learn_with(
            accounts::Cli::Claude,
            &roster("x", "bob@example.test"),
            &mut state,
            &keyer,
        );
        assert!(
            !state.accounts["x"].walls.contains_key("5h"),
            "bob's row is not walled by ann's wall"
        );
        assert!(state.forgotten[ANN].walls.contains_key("5h"));
    }

    /// `accounts history` finds an account by its row label or its key, never by an address, and
    /// lists this Mac's ledger rows of it (by key, or by label before a key was learned) beside
    /// every Mac's journal lines, oldest first, never a ledger row's free-text reason.
    /// NEGATIVE CONTROL: with `history` matching ledger rows by label alone, the row keyed to the
    /// account under its old label is missed: `left: 2 right: 3`.
    #[test]
    fn history_merges_this_macs_ledger_and_the_journal_by_account_key() {
        use super::super::accounts::{self, Event, Kind};
        const KEY: &str = "ac-0123456789abcdef";
        let roster = roster("now", "ann@example.test");
        let (asked, by_key) = super::super::journal::with_keyer(
            |realm, email| {
                (realm == "claude" && email == "ann@example.test").then(|| KEY.to_owned())
            },
            || {
                assert!(
                    resolve(accounts::Cli::Claude, "ann@example.test", &roster, None).is_none()
                );
                (
                    resolve(accounts::Cli::Claude, "now", &roster, None).unwrap(),
                    resolve(accounts::Cli::Claude, KEY, &roster, None).unwrap(),
                )
            },
        );
        assert_eq!(asked.pk.as_deref(), Some(KEY));
        assert_eq!(by_key.label.as_deref(), Some("now"));
        let mut renamed = Event::new(10, Kind::Auth, "before");
        renamed.pk = Some(KEY.into());
        renamed.why = "secret text".into();
        let ledger = vec![
            renamed,
            Event::new(30, Kind::Rotate, "now"),
            Event::new(40, Kind::Rotate, "elsewhere"),
        ];
        let journal = AccountSeen {
            history: vec![Line {
                t: 20,
                by: "m17".into(),
                kind: "signed-in".into(),
                cli: "claude".into(),
                detail: String::new(),
                heard: true,
            }],
            ..Default::default()
        };
        let lines = history(&asked, &ledger, Some(&journal));
        assert_eq!(lines.len(), 3, "{lines:?}");
        assert!(lines[0].contains(" here auth before"));
        assert!(lines[1].contains(" m17 signed-in claude (heard by this Mac)"));
        assert!(lines[2].contains(" here rotate now"));
        assert!(lines.iter().all(|l| !l.contains("secret")));
    }

    /// The report's journal section names each row the journal knows by its label and key, and a
    /// row the journal knows nothing of is left out.
    /// NEGATIVE CONTROL: with `rows` keeping rows the journal has no entry for, `left: 2 right: 1`.
    #[test]
    fn the_report_lists_only_rows_the_journal_knows() {
        use super::super::accounts;
        const KEY: &str = "ac-0123456789abcdef";
        let mut both = roster("one", "ann@example.test");
        both.rows.push(accounts::Row {
            label: "two".into(),
            ..Default::default()
        });
        let mut seen = Seen::default();
        seen.accounts.insert(KEY.into(), seen_account());
        let rows = super::super::journal::with_keyer(
            |realm, email| {
                (realm == "claude" && email == "ann@example.test").then(|| KEY.to_owned())
            },
            || rows(accounts::Cli::Claude, &both, &seen),
        );
        assert_eq!(rows.len(), 1);
        let json = rows_json(&rows, 1_000);
        assert_eq!(json["one"]["ac"].as_str(), Some(KEY));
        assert_eq!(json["one"]["last_use"]["by"].as_str(), Some("m17"));
        assert!(rows_text(&rows, 1_000).contains("  one: last used by m17"));
    }

    /// With no source installed nothing is held; a thread's override is what `current` answers.
    /// NEGATIVE CONTROL: with `current` ignoring the override, the second assertion fails.
    #[test]
    fn current_reads_the_installed_source_only() {
        assert!(current(false).is_none());
        let held = with_seen(Seen::default(), || current(true));
        assert_eq!(held.as_deref(), Some(&Seen::default()));
    }
}
