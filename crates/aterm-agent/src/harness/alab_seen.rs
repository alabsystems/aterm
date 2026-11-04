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
    /// When the newest reading was made, by which Mac, from which source.
    pub read: Option<(i64, Mac, String)>,
    /// A wall standing at the read, until when.
    pub wall_until: Option<Option<i64>>,
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
    let s = seconds.max(0);
    match s {
        0..60 => format!("{s} s"),
        60..3_600 => format!("{} min", s / 60),
        3_600..172_800 => format!("{} h", s / 3_600),
        _ => format!("{} d", s / 86_400),
    }
}

/// "m17, about 3 h ago (its clock)", or "… ahead (its clock)" for a time past `now`.
#[must_use]
pub fn when(by: &str, t: i64, now: i64) -> String {
    if t > now {
        format!("{by}, {} ahead (its clock)", age(t - now))
    } else {
        format!("{by}, about {} ago (its clock)", age(now - t))
    }
}

fn percent(pct_x10: u16) -> String {
    if pct_x10.is_multiple_of(10) {
        format!("{}%", pct_x10 / 10)
    } else {
        format!("{}.{}%", pct_x10 / 10, pct_x10 % 10)
    }
}

/// One window as a line: "5h 42% used, resets 2026-11-03T19:30:00Z, read 20 min ago on m3
/// (statusline)"; "7d reset 2026-11-02T00:00:00Z, not read since"; "7d at its limit until …".
#[must_use]
pub fn window_line(w: &WindowSeen, now: i64) -> String {
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
        out.push_str(&format!(", read {} ago on {by} ({src})", age(now - *t)));
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

/// What the journal says of `account` as lines, for a report: last used by, signed in on, and
/// each window. Empty where it says nothing.
#[must_use]
pub fn lines(account: &AccountSeen, now: i64) -> Vec<String> {
    let mut out = Vec::new();
    if let Some(last) = &account.last_use {
        out.push(format!("last used by {}", when(&last.by, last.t, now)));
    }
    if !account.signed_in.is_empty() {
        let on: Vec<String> = account
            .signed_in
            .iter()
            .map(|s| format!("{} ({})", s.on, s.state))
            .collect();
        out.push(format!("signed in on {}", on.join(", ")));
    }
    out.extend(account.windows.iter().map(|w| window_line(w, now)));
    out
}

/// The account key of `label`'s row: the key its state learned ([`super::accounts::Account::pk`]),
/// else the lab's account the row is ([`super::alab_roster::Lab::account_of`]).
#[must_use]
pub fn row_key(
    cli: super::accounts::Cli,
    row: &super::accounts::Row,
    state: &super::accounts::State,
    lab: Option<&super::alab_roster::Lab>,
) -> Option<String> {
    state
        .accounts
        .get(&row.label)
        .and_then(|account| account.pk.clone())
        .or_else(|| Some(lab?.account_of(cli, row)?.pk.clone()))
}

/// One roster row the journal says something of: its label, its account key, and what it says.
pub type Reported = (String, String, AccountSeen);

/// Each row of `roster` the journal says something of, in roster order.
#[must_use]
pub fn rows(
    cli: super::accounts::Cli,
    roster: &super::accounts::Roster,
    state: &super::accounts::State,
    lab: Option<&super::alab_roster::Lab>,
    seen: &Seen,
) -> Vec<Reported> {
    roster
        .rows
        .iter()
        .filter_map(|row| {
            let pk = row_key(cli, row, state, lab)?;
            let found = seen.accounts.get(&pk)?;
            Some((row.label.clone(), pk, found.clone()))
        })
        .collect()
}

/// The report's `journal` section: per row label, its account key, last use, sign-ins and
/// windows, each time with its age at `now`.
#[must_use]
pub fn rows_json(rows: &[Reported], now: i64) -> aterm_json::Value {
    use aterm_json::{Map, Value, json};
    let mut out = Map::new();
    for (label, pk, seen) in rows {
        let windows: Vec<Value> = seen
            .windows
            .iter()
            .map(|w| {
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
                    None => ("wall-only", None, None),
                };
                json!({
                    "window": w.window,
                    "sub": w.sub,
                    "state": state,
                    "used_pct_x10": pct,
                    "reset": reset,
                    "read": w.read.as_ref().map(|(t, by, src)| json!({
                        "t": t, "age_s": now - t, "by": by, "src": src,
                    })),
                    "wall_until": w.wall_until.map(|u| json!(u)),
                    "line": window_line(w, now),
                })
            })
            .collect();
        out.insert(
            label.clone(),
            json!({
                "ac": pk,
                "last_use": seen.last_use.as_ref().map(|l| json!({
                    "by": l.by, "t": l.t, "age_s": now - l.t, "clock": "its",
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
    state: &super::accounts::State,
    lab: Option<&super::alab_roster::Lab>,
) -> Option<Asked> {
    if name.contains('@') {
        return None;
    }
    if let Some(row) = roster.rows.iter().find(|row| row.label == name) {
        return Some(Asked {
            label: Some(row.label.clone()),
            pk: row_key(cli, row, state, lab),
        });
    }
    // A row whose state no longer holds it keeps its key in the forgotten observations, so a
    // removed row's label still finds its history.
    let pk = if name.starts_with("ac-") {
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
        .find(|row| row_key(cli, row, state, lab).as_deref() == Some(pk.as_str()))
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

    /// A report says who last used the account, by its clock, where it is signed in, and each
    /// window with its age, Mac and source; a reset nobody read since is never shown as 0.
    /// NEGATIVE CONTROL: with `window_line` printing a `ResetUnread` figure as `0% used`, the
    /// 7d line reads `7d 0% used…`: `left: "7d 0% used, at its limit"`.
    #[test]
    fn a_report_names_the_mac_the_age_and_never_a_guessed_zero() {
        let lines = lines(&seen_account(), 1_000);
        assert_eq!(lines[0], "last used by m17, about 3 h ago (its clock)");
        assert_eq!(lines[1], "signed in on m3 (in), m17 (out)");
        assert_eq!(
            lines[2],
            "5h 42.5% used, reset unknown, read 20 min ago on m3 (statusline)"
        );
        assert_eq!(
            lines[3],
            "7d reset at 1970-01-01T00:00:00Z, not read since, at its limit"
        );
        assert!(lines.iter().all(|l| !l.contains('@')));
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
        use super::super::accounts::{self, Event, Kind, State};
        const KEY: &str = "ac-0123456789abcdef";
        let mut state = State::default();
        state.accounts.insert(
            "now".into(),
            accounts::Account {
                pk: Some(KEY.into()),
                ..Default::default()
            },
        );
        let roster = roster("now", "ann@example.test");
        assert!(
            resolve(
                accounts::Cli::Claude,
                "ann@example.test",
                &roster,
                &state,
                None
            )
            .is_none()
        );
        let asked = resolve(accounts::Cli::Claude, "now", &roster, &state, None).unwrap();
        assert_eq!(asked.pk.as_deref(), Some(KEY));
        let by_key = resolve(accounts::Cli::Claude, KEY, &roster, &state, None).unwrap();
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
        use super::super::accounts::{self, State};
        const KEY: &str = "ac-0123456789abcdef";
        let mut state = State::default();
        state.accounts.insert(
            "one".into(),
            accounts::Account {
                pk: Some(KEY.into()),
                ..Default::default()
            },
        );
        let mut both = roster("one", "ann@example.test");
        both.rows.push(accounts::Row {
            label: "two".into(),
            ..Default::default()
        });
        let mut seen = Seen::default();
        seen.accounts.insert(KEY.into(), seen_account());
        let rows = rows(accounts::Cli::Claude, &both, &state, None, &seen);
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
