// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! CLAUDE CODE'S `/login` DIALOG, read whole (MEASURED on 2.1.303 in the
//! real-Claude lab, 2026-10-19: `fixtures/claude-2.1.303-login-*.txt`; the
//! success state READ in its bundle, no lab can sign in).
//!
//! `/login` draws one dialog under the conversation: a full-width rule, the
//! title `Login` at column two, its body, and `Esc to cancel`. No composer is
//! drawn while it stands. Its states, in the order a sign-in walks them:
//!
//! * THE METHOD PICKER, `Select login method:` over three numbered methods —
//!   `Claude account with subscription · …`, `Anthropic Console account ·
//!   …`, `3rd-party platform · …` — the `❯` on the focused one (the first,
//!   at the dialog's start). A session whose settings force a method skips
//!   it. Under an environment OAuth token a warning stands above the
//!   question.
//! * CONSOLE'S MENU, `How do you want to sign in?`, whose second option MINTS
//!   AN API KEY in the Console workspace.
//! * THE BROWSER HAND-OFF: `Opening browser to sign in…` for three seconds,
//!   then `Browser didn't open? Use the url below to sign in (c to copy)`,
//!   the URL, and the masked field `Paste code here if prompted >`. The
//!   sign-in is the person's, in the browser; `c` copies, Esc abandons.
//! * SUCCESS, `Logged in as <email>` over `Login successful. Press Enter to
//!   continue…`: the login is saved already, and Enter only closes the
//!   dialog.
//! * Every other state ([`LoginScreen::Failed`]): `OAuth error: …` over
//!   `Press Enter to retry.` (MEASURED: a code exchange the lab's proxy
//!   refused), a login saved only for now, an account on hold.
//!
//! Esc out of any of them before success leaves `❯ /login` over `⎿  Login
//! interrupted` ([`login_interrupted`]) and the wall it was typed at.
//!
//! This reads; it decides nothing. What the harness presses on each is the
//! supervisor's (`aterm-agent`'s `supervise::policy::login`).

use crate::anchors::anchor_text;
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
            LoginMethod::Subscription => anchor_text("login.subscription"),
            LoginMethod::Console => anchor_text("login.console"),
            LoginMethod::Platform => anchor_text("login.platform"),
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

/// The picker's rows: each method's index among its options, and which one
/// the `❯` stands on.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Picker {
    /// The methods in the order drawn (option 1 first).
    pub methods: Vec<LoginMethod>,
    /// The index into [`Self::methods`] of the focused one: `Some(0)` as the
    /// dialog opens, anywhere else moved there by a key.
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

/// A full-width rule row: `─` and nothing else, wider than a dialog's own
/// inner rules could be mistaken for a word.
fn rule(row: &str) -> bool {
    let t = trim(row);
    t.chars().count() >= 20 && t.chars().all(|c| c == '─')
}

/// The `/login` dialog on `rows`, if it is what the screen ends with: the
/// last full-width rule, the title `Login` the first row under it, no rule
/// under that (a composer's frame is two rules), and a body this module
/// knows. `None` for every other screen — the conversation, a composer, any
/// other dialog.
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
            .find(|r| contains(r, anchor_text("login.success")))
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

/// [`anchor_text`] by a runtime id.
fn anchor_text_of(id: &str) -> &'static str {
    crate::anchors::anchor(id)
}

/// Whether the last thing said is `⎿  Login interrupted` under `❯ /login`:
/// the dialog left by Esc (or by its cancel) before its success, the login
/// still gone.
#[must_use]
pub fn login_interrupted(rows: &[String]) -> bool {
    let said: Vec<&str> = rows
        .iter()
        .map(|r| trim_end(r))
        .filter(|r| !r.is_empty())
        .collect();
    // The composer's frame below it: the rows above its top rule.
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
    out.strip_prefix('⎿')
        .is_some_and(|rest| trim(rest) == anchor_text("login.interrupted"))
        && said.len() >= 2
        && trim_end(said[said.len() - 2]) == "❯ /login"
}

#[cfg(test)]
#[path = "login_tests.rs"]
mod tests;
