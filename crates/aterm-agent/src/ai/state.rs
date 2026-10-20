// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! ONE WORD PER CLI, AND THE HEALTH (design §4): a pure function of [`Facts`], whether the
//! CLI is wanted here, and the clock. Nothing here reads the machine.

use super::connector::Connector;
use super::facts::{Facts, Kept};
use super::store::Ended;

/// A use within this long reads `ok`; older, `idle` (accounts v2's `OK_FRESH_S`).
pub const OK_FRESH_S: i64 = 6 * 3600;

/// One CLI's state.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum State {
    /// A login exists and was used within [`OK_FRESH_S`].
    Ok,
    /// A login exists; nothing used it lately. Its next use is its check.
    Idle,
    /// The usage limit is reached until this time.
    Limited(i64),
    /// Claude: accounts v2's forecast ends by this time.
    Due(i64),
    /// It has run here, and nothing says whether a login exists.
    Unproven,
    /// No login exists, or its login has ended.
    SignedOut,
    /// Wanted and served here, and not installed.
    NotInstalled,
    /// Not to be used here.
    Off,
}

impl State {
    /// Its word.
    #[must_use]
    pub fn word(self) -> &'static str {
        match self {
            State::Ok => "ok",
            State::Idle => "idle",
            State::Limited(_) => "limited",
            State::Due(_) => "due",
            State::Unproven => "unproven",
            State::SignedOut => "signed-out",
            State::NotInstalled => "not-installed",
            State::Off => "off",
        }
    }

    /// Whether a review may be asked of it.
    #[must_use]
    pub fn usable(self) -> bool {
        matches!(
            self,
            State::Ok | State::Idle | State::Due(_) | State::Unproven
        )
    }

    /// Whether a wanted CLI has a definite install or sign-in action.
    /// Unknown login evidence and intentional exclusions need no action.
    #[must_use]
    pub const fn needs_person(self, wanted: bool) -> bool {
        wanted && matches!(self, State::NotInstalled | State::SignedOut | State::Due(_))
    }
}

/// How much a row matters.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub enum Mark {
    /// Working.
    Ok,
    /// Worth knowing; nothing to do.
    Note,
    /// Something to do soon.
    Warn,
    /// A person is needed.
    Fail,
}

impl Mark {
    /// Its word, as `aterm doctor` prints it.
    #[must_use]
    pub fn word(self) -> &'static str {
        match self {
            Mark::Ok => "ok",
            Mark::Note => "note",
            Mark::Warn => "warn",
            Mark::Fail => "FAIL",
        }
    }
}

/// One CLI, judged.
#[derive(Debug, Clone)]
pub struct Row {
    /// The row.
    pub connector: &'static Connector,
    /// Whether it is wanted here (`[ai.<id>] use`).
    pub wanted: bool,
    /// Its state.
    pub state: State,
    /// How much it matters.
    pub mark: Mark,
    /// What is known, in one line.
    pub fact: String,
    /// The one act that fixes it.
    pub fix: Option<String>,
    /// The installed version.
    pub version: Option<String>,
    /// Claude's plan.
    pub plan: Option<String>,
    /// The newest use, unix seconds, and how.
    pub used: Option<(i64, &'static str)>,
    /// Claude's forecast end.
    pub renew_by: Option<i64>,
    /// When the usage limit lifts.
    pub limited_until: Option<i64>,
}

/// `n` seconds as a short span: `35 min`, `5 h`, `3 d`.
#[must_use]
pub fn span(n: i64) -> String {
    let n = n.max(0);
    if n < 3600 {
        format!("{} min", (n / 60).max(1))
    } else if n < 2 * 86_400 {
        format!("{} h", n / 3600)
    } else {
        format!("{} d", n / 86_400)
    }
}

/// Unix seconds as a UTC date and time, `2026-11-02 14:05 UTC`.
#[must_use]
pub fn when(t: i64) -> String {
    match u64::try_from(t) {
        Ok(secs) => {
            let s = aterm_types::rfc3339::format_rfc3339(secs).replace('T', " ");
            format!("{} UTC", s.chars().take(16).collect::<String>())
        }
        Err(_) => t.to_string(),
    }
}

/// What shows a login exists, if anything does: `Some(true)` signed in, `Some(false)` no
/// login, `None` nothing aterm can see says either way.
fn login_seen(f: &Facts) -> Option<bool> {
    if let Some(a) = &f.accounts
        && a.login.is_some()
    {
        return Some(a.made);
    }
    if f.connector.is_claude() {
        return Some(f.profile_signed_in || f.login_file.is_some());
    }
    if f.hidden_store || f.connector.stores.is_empty() {
        return None;
    }
    Some(f.login_file.is_some())
}

/// Judges one CLI.
#[must_use]
pub fn assess(f: &Facts, wanted: bool, now: i64) -> Row {
    let c = f.connector;
    let version = f.managed.as_ref().and_then(|m| m.version.clone());
    let unmeasured = if c.measured {
        ""
    } else {
        " (not measured in this build)"
    };
    let mut row = Row {
        connector: c,
        wanted,
        state: State::Off,
        mark: Mark::Note,
        fact: String::new(),
        fix: None,
        version,
        plan: f.plan.clone(),
        used: None,
        renew_by: None,
        limited_until: None,
    };
    let off = |row: &mut Row, why: &str| {
        row.state = State::Off;
        row.mark = Mark::Note;
        row.fact = why.to_string();
    };
    if !wanted {
        off(
            &mut row,
            &format!("not used on this machine ([ai.{}] use = false)", c.id),
        );
        return row;
    }
    match f.kept {
        Some(Kept::Unavailable) => {
            off(&mut row, "no build for this platform");
            return row;
        }
        Some(Kept::Excluded) => {
            off(&mut row, "[packages] exclude names it");
            return row;
        }
        Some(Kept::Removed) => {
            off(
                &mut row,
                &format!(
                    "uninstalled here on purpose (`aterm pkg install {}` brings it back)",
                    c.id
                ),
            );
            return row;
        }
        None => {}
    }
    if f.managed.is_none() {
        row.state = State::NotInstalled;
        row.mark = Mark::Fail;
        row.fact = match &f.foreign {
            Some(p) => format!(
                "not installed by aterm (a copy it does not manage is at {}){unmeasured}",
                p.display()
            ),
            None => format!("not installed{unmeasured}"),
        };
        row.fix = Some(format!("aterm pkg install {}", c.id));
        return row;
    }
    if let Err(value) = &f.home {
        row.state = State::Unproven;
        row.mark = Mark::Note;
        row.fact = if value.is_empty() {
            "no home directory is known (HOME is not set)".to_string()
        } else {
            format!(
                "{} is set to a relative path ({}), which aterm cannot place",
                c.home.var.unwrap_or("its home variable"),
                crate::harness::one_line(value, 60)
            )
        };
        return row;
    }
    let served_at = f.accounts.as_ref().and_then(|a| a.served_at);
    let renewed_at = f.accounts.as_ref().and_then(|a| a.renewed_at);
    let refreshed = f.login_file.as_ref().and_then(|(_, w)| *w);
    let newest_use = [f.record.used_at, served_at, refreshed, renewed_at]
        .into_iter()
        .flatten()
        .max();
    // An end counts when nothing newer than it shows the login working.
    let ended = f
        .record
        .ended_at
        .filter(|end| newest_use.is_none_or(|u| *end >= u))
        .map(|at| {
            let why = f.record.ended_why.map_or("it ended", Ended::says);
            (at, why)
        });
    let expired = f.accounts.as_ref().and_then(|a| a.state) == Some("expired");
    // The newest proof, named by what it was.
    row.used = [
        f.record
            .used_at
            .zip(f.record.used_how.map(crate::ai::store::How::word)),
        served_at.map(|t| (t, "served")),
        renewed_at.map(|t| (t, "renewed")),
        refreshed.map(|t| (t, "refreshed")),
    ]
    .into_iter()
    .flatten()
    .max_by_key(|(t, _)| *t);
    let signed_out = |row: &mut Row, fact: String| {
        row.state = State::SignedOut;
        row.mark = Mark::Fail;
        row.fact = format!("{fact}{unmeasured}");
        row.fix = Some(format!("aterm ai login {}", c.id));
    };
    if expired {
        signed_out(
            &mut row,
            "its login has ended (accounts v2 saw a login wall)".to_string(),
        );
        return row;
    }
    if let Some((at, why)) = ended {
        signed_out(
            &mut row,
            format!(
                "its login has ended: {why} {} ago",
                span(now.saturating_sub(at))
            ),
        );
        return row;
    }
    match login_seen(f) {
        Some(false) if !f.home_exists => {
            signed_out(&mut row, "never signed in here".to_string());
            return row;
        }
        Some(false) => {
            signed_out(&mut row, "no login is stored".to_string());
            return row;
        }
        None if newest_use.is_none() => {
            row.state = State::Unproven;
            row.mark = Mark::Note;
            row.fact = if f.hidden_store {
                "it keeps its login in the OS keyring, which aterm does not read".to_string()
            } else if f.home_exists {
                format!("it has run here; nothing has shown a login yet{unmeasured}")
            } else {
                format!("its home is not there; nothing has shown a login yet{unmeasured}")
            };
            return row;
        }
        _ => {}
    }
    // Signed in.
    let mut words = vec![match &f.plan {
        Some(plan) => format!("signed in ({plan})"),
        None => "signed in".to_string(),
    }];
    if let Some((t, how)) = row.used {
        words.push(format!("{how} {} ago", span(now.saturating_sub(t))));
    }
    let renew_by = f.accounts.as_ref().and_then(|a| a.renew_by);
    let limited = [f.limited_until, f.record.limited_until]
        .into_iter()
        .flatten()
        .filter(|t| *t > now)
        .max();
    row.limited_until = limited;
    row.renew_by = renew_by;
    if let Some(by) = renew_by {
        words.push(format!("renew by {}", when(by)));
        row.state = State::Due(by);
        row.mark = Mark::Warn;
        row.fix = Some(format!("aterm ai login {}", c.id));
    } else if let Some(until) = limited {
        words.push(format!("usage limit until {}", when(until)));
        row.state = State::Limited(until);
        row.mark = Mark::Note;
    } else if f.accounts.as_ref().and_then(|a| a.state) == Some("ok")
        || newest_use.is_some_and(|t| now.saturating_sub(t) <= OK_FRESH_S)
    {
        row.state = State::Ok;
        row.mark = Mark::Ok;
    } else {
        row.state = State::Idle;
        row.mark = Mark::Ok;
    }
    if !f.env_credentials.is_empty() {
        words.push(format!(
            "{} set in this environment would be used instead of the stored login",
            f.env_credentials.join(", ")
        ));
    }
    row.fact = format!("{}{unmeasured}", words.join(" · "));
    row
}

/// The doctor's verdict.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Health {
    /// `OK`, `WARN` or `FAIL`.
    pub word: &'static str,
    /// The ids whose rows are FAIL.
    pub failing: Vec<&'static str>,
    /// The ids whose rows are warn.
    pub warning: Vec<&'static str>,
    /// The ids whose rows are notes.
    pub noted: Vec<&'static str>,
}

/// The verdict over every row.
#[must_use]
pub fn health(rows: &[Row]) -> Health {
    let ids = |mark: Mark| {
        rows.iter()
            .filter(|r| r.mark == mark)
            .map(|r| r.connector.id)
            .collect::<Vec<_>>()
    };
    let failing = ids(Mark::Fail);
    let warning = ids(Mark::Warn);
    let noted = ids(Mark::Note);
    let word = if !failing.is_empty() {
        "FAIL"
    } else if !warning.is_empty() {
        "WARN"
    } else {
        "OK"
    };
    Health {
        word,
        failing,
        warning,
        noted,
    }
}

impl Health {
    /// The `health:` line's words after the colon.
    #[must_use]
    pub fn line(&self, next: &str) -> String {
        let plural = |n: usize, one: &str, many: &str| {
            if n == 1 {
                one.to_string()
            } else {
                format!("{n} {many}")
            }
        };
        match self.word {
            "FAIL" => format!(
                "FAIL — {} {} you ({}); next: {next}",
                plural(self.failing.len(), "1 CLI", "CLIs"),
                if self.failing.len() == 1 {
                    "needs"
                } else {
                    "need"
                },
                self.failing.join(", ")
            ),
            "WARN" => format!(
                "WARN — {} ({}); next: {next}",
                plural(self.warning.len(), "1 warning", "warnings"),
                self.warning.join(", ")
            ),
            _ if self.noted.is_empty() => "OK".to_string(),
            _ => format!(
                "OK — {} ({})",
                plural(self.noted.len(), "1 note", "notes"),
                self.noted.join(", ")
            ),
        }
    }
}

#[cfg(test)]
#[path = "state_tests.rs"]
mod tests;
