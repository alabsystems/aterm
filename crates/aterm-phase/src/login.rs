// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Reader for Claude Code's `/login` dialog states and interrupted-login walls.

use crate::text::{contains, trim, trim_end};

/// One of the picker's methods, by the words its row opens with.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum LoginMethod {
    /// `Claude account with subscription` (Pro, Max, Team, Enterprise).
    Subscription,
    /// `Anthropic Console account` (API usage billing).
    Console,
    /// `3rd-party platform` (Bedrock, Foundry, Vertex).
    Platform,
}

impl LoginMethod {
    /// The words the picker's row opens with (after `N. `).
    #[must_use]
    pub const fn label(self) -> &'static str {
        match self {
            LoginMethod::Subscription => crate::anchor!("login.subscription"),
            LoginMethod::Console => crate::anchor!("login.console"),
            LoginMethod::Platform => crate::anchor!("login.platform"),
        }
    }

    /// The word a journal line carries.
    #[must_use]
    pub const fn word(self) -> &'static str {
        match self {
            LoginMethod::Subscription => "subscription",
            LoginMethod::Console => "console",
            LoginMethod::Platform => "platform",
        }
    }

    /// The method [`Self::word`] names.
    #[must_use]
    pub fn of_word(word: &str) -> Option<LoginMethod> {
        [
            LoginMethod::Subscription,
            LoginMethod::Console,
            LoginMethod::Platform,
        ]
        .into_iter()
        .find(|m| m.word() == word)
    }

    fn of_option(text: &str) -> Option<LoginMethod> {
        [
            LoginMethod::Subscription,
            LoginMethod::Console,
            LoginMethod::Platform,
        ]
        .into_iter()
        .find(|m| text.starts_with(m.label()))
    }
}

/// The picker's rows: each method's index among its options, and which one `❯` stands on.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Picker {
    /// The methods in the order drawn (option 1 first).
    pub methods: Vec<LoginMethod>,
    /// Index into [`Self::methods`] of the focused option.
    pub focused: Option<usize>,
    /// The dialog's title row, for a key's guard.
    pub title_row: String,
}

impl Picker {
    /// Where `method` is among the options.
    #[must_use]
    pub fn index_of(&self, method: LoginMethod) -> Option<usize> {
        self.methods.iter().position(|m| *m == method)
    }
}

/// Which state of the `/login` dialog the screen shows.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum LoginScreen {
    /// The method picker.
    Picker(Picker),
    /// Console's own menu (`How do you want to sign in?`).
    ConsoleMenu,
    /// The browser hand-off: opening, the URL, the paste field.
    Browser,
    /// `Login successful. Press Enter to continue…`, its row for a guard.
    Success { row: String },
    /// Any other state of the dialog: an error, a retry, an account on hold.
    Failed,
}

fn rule(row: &str) -> bool {
    let t = trim(row);
    t.chars().count() >= 20 && t.chars().all(|c| c == '─')
}

/// Parses the `/login` dialog on `rows` if the screen ends with one.
#[must_use]
pub fn login_screen(rows: &[String]) -> Option<LoginScreen> {
    let last_rule = rows.iter().rposition(|r| rule(r))?;
    let below = &rows[last_rule + 1..];
    let title_at = below.iter().position(|r| !trim(r).is_empty())?;
    let title = &below[title_at];
    if trim(title) != "Login" || !title.starts_with("  ") {
        return None;
    }
    let body: Vec<&str> = below[title_at + 1..]
        .iter()
        .map(|r| trim_end(r))
        .filter(|r| !r.is_empty())
        .collect();
    let has = |id: &str| body.iter().any(|r| contains(r, anchor_text_of(id)));
    if has("login.success") {
        let row = body
            .iter()
            .find(|r| contains(r, crate::anchor!("login.success")))
            .map(|r| (*r).to_string())
            .unwrap_or_default();
        return Some(LoginScreen::Success { row });
    }
    if has("login.method") {
        let mut methods = Vec::new();
        let mut focused = None;
        for r in &body {
            let t = trim(r);
            let (cursor, t) = match t.strip_prefix('❯') {
                Some(rest) => (true, trim(rest)),
                None => (false, t),
            };
            let Some((n, label)) = t.split_once(". ") else {
                continue;
            };
            if n.is_empty() || !n.bytes().all(|b| b.is_ascii_digit()) {
                continue;
            }
            if let Some(m) = LoginMethod::of_option(label) {
                if cursor {
                    focused = Some(methods.len());
                }
                methods.push(m);
            }
        }
        if methods.is_empty() {
            return Some(LoginScreen::Failed);
        }
        return Some(LoginScreen::Picker(Picker {
            methods,
            focused,
            title_row: title.clone(),
        }));
    }
    if has("login.console_how") {
        return Some(LoginScreen::ConsoleMenu);
    }
    if has("login.opening") || has("login.no_browser") || has("login.paste") {
        return Some(LoginScreen::Browser);
    }
    Some(LoginScreen::Failed)
}

fn anchor_text_of(id: &str) -> &'static str {
    crate::anchors::anchor(id)
}

/// Whether the last thing said is `⎿  Login interrupted` under `❯ /login`.
#[must_use]
pub fn login_interrupted(rows: &[String]) -> bool {
    let said: Vec<&str> = rows
        .iter()
        .map(|r| trim_end(r))
        .filter(|r| !r.is_empty())
        .collect();
    let end = said
        .iter()
        .rposition(|r| r.starts_with('❯') && !r.starts_with("❯ /login"))
        .and_then(|composer| composer.checked_sub(1))
        .filter(|&top| rule(said[top]))
        .unwrap_or(said.len());
    let said = &said[..end];
    let Some(last) = said.last() else {
        return false;
    };
    let out = trim(last);
    interrupted_row(out) && said.len() >= 2 && trim_end(said[said.len() - 2]) == LOGIN_ROW
}

const LOGIN_ROW: &str = "❯ /login";

fn interrupted_row(row: &str) -> bool {
    trim(row)
        .strip_prefix('⎿')
        .is_some_and(|rest| trim(rest) == crate::anchor!("login.interrupted"))
}

fn said_above(rows: &[String], i: usize) -> Option<usize> {
    rows[..i].iter().rposition(|r| !trim(r).is_empty())
}

/// A `/login` typed at a lost-login wall ([`login_at_wall`]).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LoginAtWall {
    /// The lost login's words, as its row says them.
    pub message: String,
    /// Whether an earlier `/login` at the same wall was interrupted before this one.
    pub left_before: bool,
}

/// Returns the lost-login wall above a standing or interrupted `/login`, if any.
#[must_use]
pub fn login_at_wall(rows: &[String]) -> Option<LoginAtWall> {
    let from = if login_screen(rows).is_some() {
        rows.iter().rposition(|r| rule(r))?
    } else if login_interrupted(rows) {
        rows.iter().rposition(|r| interrupted_row(r))?
    } else {
        return None;
    };
    let mut at = said_above(rows, from).filter(|&i| trim_end(&rows[i]) == LOGIN_ROW)?;
    let mut left_before = false;
    while let Some(out) = said_above(rows, at).filter(|&i| interrupted_row(&rows[i]))
        && let Some(typed) = said_above(rows, out).filter(|&i| trim_end(&rows[i]) == LOGIN_ROW)
    {
        at = typed;
        left_before = true;
    }
    let width = rows
        .iter()
        .map(|r| trim_end(r).chars().count())
        .max()
        .unwrap_or(0)
        .max(20);
    let bar = "─".repeat(width);
    let mut then: Vec<String> = rows[..at].to_vec();
    then.extend([bar.clone(), "❯ ".to_string(), bar]);
    let wall = crate::wall::wall(&then)?;
    (wall.kind == crate::wall::WallKind::Auth).then_some(LoginAtWall {
        message: wall.message,
        left_before,
    })
}

#[cfg(test)]
#[path = "login_tests.rs"]
mod tests;
