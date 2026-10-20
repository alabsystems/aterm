// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! THE OWNER'S NOTICES WHEN A SIGN-IN, A RENEWAL OR AN ACCOUNT MOVE CANNOT
//! FINISH ON ITS OWN (the owner, 2026-10-19: *"I want to see the notice of
//! that if the auto login and account rotation is working be FAILED, then I
//! need to see that when clear and simple instructions on what to do about it
//! or what to expect"*).
//!
//! Every notice says the same four things, in this order, as plain sentences:
//! WHAT HAPPENED ([`NoticeText::happened`]), WHAT aterm ALREADY DID
//! ([`NoticeText::did`]), THE ONE THING TO DO NOW ([`NoticeText::todo`]: a
//! command to run or a step to take) and WHAT TO EXPECT AFTER
//! ([`NoticeText::expect`]). The texts are constants, so a test can hold each
//! to that shape without pinning its prose (`notice_tests.rs`): every part
//! one sentence, the happening a reason the message engine admits (no
//! "waiting", no "retrying"), the step one action it admits (no ` or `, no
//! `; `, no snooze), the title short enough for the band.
//!
//! A notice reaches the owner as an exception ([`super::exception::Exception`]
//! with its [`Notice`]): `Now` while only the owner can finish it — the host
//! raises one standing ask on the glass, keyed by the tab and the notice
//! ([`Notice::id`]), withdrawn as soon as the point no longer holds it, and
//! records it in the exceptions digest for later. Nothing is raised while the
//! harness is still working on its own — about to type `/login`, waiting out
//! a person's keys before it does, answering the method picker with the
//! method the session already uses, pressing the success Enter, moving the
//! session to another account or waiting for one: the journal and the ledger
//! carry those. The owner is told at the first point that needs them: the
//! browser sign-in `/login` opened, a method picker aterm does not answer, a
//! sign-in cancelled or stopped, `/login` not run.

/// One notice's constant words. `{label}` (a roster label) and `{why}` (a
/// short clause the policy fills) are the only placeholders, and only where
/// [`NoticeText::fill`] is given a value for them — never in what happened,
/// which the message engine holds to its reason words by substring: a label
/// is the owner's own word, and one such as `retry-1` would fault it, and the
/// owner would be shown nothing.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct NoticeText {
    /// The notice's identity: the journal's word. Lowercase words joined by
    /// `-`.
    pub id: &'static str,
    /// Its short code: the tail of the host's key for its ask, which the
    /// message engine caps (a tab's id and a word would not fit). Two
    /// lowercase letters, one per notice.
    pub code: &'static str,
    /// The band's title, without the tab's place (the host fits that on).
    pub title: &'static str,
    /// What happened, one sentence.
    pub happened: &'static str,
    /// What aterm already did about it, one sentence.
    pub did: &'static str,
    /// The one thing to do now, one imperative sentence.
    pub todo: &'static str,
    /// What to expect once it is done, one sentence.
    pub expect: &'static str,
}

/// A notice with its placeholders filled: what an exception carries to the
/// host ([`super::exception::Exception::notice`]). The happening and the step
/// are the exception's own `why` and `action`; this holds the rest.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Notice {
    /// [`NoticeText::id`].
    pub id: &'static str,
    /// [`NoticeText::code`].
    pub code: &'static str,
    /// [`NoticeText::title`].
    pub title: &'static str,
    /// [`NoticeText::did`], filled.
    pub did: String,
    /// [`NoticeText::expect`], filled.
    pub expect: String,
}

/// What a tab's notice says once its login is back: the host closes the
/// notice's one row in Settings ▸ Messages fixed in these words (the tab's
/// place fitted on), so the record says how the loss ended.
pub const BACK: &str = "Claude's login is back";

/// The placeholder a roster label fills.
pub const LABEL: &str = "{label}";

/// The placeholder a short clause of the policy's fills.
pub const WHY: &str = "{why}";

/// What Claude Code says, and what the owner sees, when its login is gone.
const EXPIRED: &str = "Claude Code's login expired, so it cannot answer prompts.";

/// The one thing that ends a lost login the harness could not carry.
const TYPE_LOGIN: &str = "Type /login in the tab and finish signing in in the browser.";

/// What follows a sign-in made in the tab: Claude Code sends the prompt the
/// login swallowed again by itself, and aterm presses the success Enter.
const CONTINUES: &str = "The session continues by itself once you are signed in.";

/// What follows a renewal: the relaunch onto the renewed login, with the
/// wall's continuation.
const MOVES: &str = "aterm moves the session onto the renewed login and continues its work.";

/// What a `{why}` says where `/login` was not run
/// ([`LOGIN_NOT_RUN`]): a draft stands in the composer (the wall would
/// swallow `/login` with it).
pub const WHY_DRAFT: &str = "a draft is in the composer";

/// …the owner's `retry_api_errors = false` (named without the key, whose
/// `retry` the message engine refuses in a reason).
pub const WHY_RETRY_OFF: &str = "aterm.toml leaves API errors to you";

/// …the hour's typing budget (`continue_per_hour`) spent.
pub const WHY_BUDGET: &str = "its typing budget for the hour is spent";

/// …a tab with no task: nothing is typed into it.
pub const WHY_NO_TASK: &str = "the tab has no task";

/// …a task done after the done check: nothing more is typed into it.
pub const WHY_TASK_DONE: &str = "its task is finished";

/// Every clause the policy fills a `{why}` with but [`RENEWED_BY_HAND`]'s
/// (the policy's own words for what holds a relaunch), for the tests that
/// measure every notice at its longest.
pub const WHYS: &[&str] = &[
    WHY_DRAFT,
    WHY_RETRY_OFF,
    WHY_BUDGET,
    WHY_NO_TASK,
    WHY_TASK_DONE,
];

/// The `/login` aterm typed opened its browser sign-in (the hand-off read
/// on the screen), the method picker answered with the method the session
/// already uses: the sign-in in the browser is the one step left, and only
/// the owner can take it.
pub const LOGIN_SIGN_IN: NoticeText = NoticeText {
    id: "login-sign-in",
    code: "si",
    title: "Claude's login expired",
    happened: EXPIRED,
    did: "aterm ran /login in the tab and opened the sign-in in your browser.",
    todo: "Finish signing in in the browser window that opened.",
    expect: CONTINUES,
};

/// [`LOGIN_SIGN_IN`] for a session whose login is a roster row
/// (`{label}`: the renewable login it runs on, or the account a move put it
/// on): the browser sign-in must be that row's own account, or two rows of
/// the roster become one account.
pub const LOGIN_SIGN_IN_AS: NoticeText = NoticeText {
    id: "login-sign-in-as",
    code: "sa",
    title: "Claude's login expired",
    happened: EXPIRED,
    did: "aterm ran /login for {label} and opened its sign-in in your browser.",
    todo: "Finish signing in in the browser with that login's own account.",
    expect: CONTINUES,
};

/// The method picker of the `/login` aterm typed, which aterm does not
/// answer: nothing says the session signs in with a subscription (the only
/// method chosen for the owner), or the picker is not drawn as the one
/// measured. The picker is theirs, then the browser.
pub const LOGIN_CHOOSE: NoticeText = NoticeText {
    id: "login-choose",
    code: "ch",
    title: "Claude's login expired",
    happened: EXPIRED,
    did: "aterm ran /login and leaves the choice of sign-in method to you.",
    todo: "Pick your sign-in method in the tab, then finish signing in in the browser.",
    expect: CONTINUES,
};

/// The `/login` aterm typed was left before it signed in (`Login
/// interrupted`, or its dialog gone with no login back): aterm types it once
/// per lost login, never again.
pub const LOGIN_CANCELLED: NoticeText = NoticeText {
    id: "login-cancelled",
    code: "ca",
    title: "Claude sign-in cancelled",
    happened: "Claude Code's sign-in was cancelled before it finished.",
    did: "aterm ran /login once and does not run it again by itself.",
    todo: TYPE_LOGIN,
    expect: CONTINUES,
};

/// A person's own `/login` at a lost login, left before it signed in
/// (`Login interrupted` with no `/login` of aterm's): their answer, which the
/// harness does not override with a `/login` of its own.
pub const LOGIN_LEFT: NoticeText = NoticeText {
    id: "login-left",
    code: "lf",
    title: "Claude sign-in cancelled",
    happened: "Claude Code's sign-in was cancelled before it finished.",
    did: "aterm does not run /login after a sign-in you cancelled.",
    todo: TYPE_LOGIN,
    expect: CONTINUES,
};

/// The dialog stopped short of a login on a state of its own (an OAuth
/// error, an account on hold, Console's own menu): aterm presses none of its
/// keys, whoever ran `/login`.
pub const LOGIN_STOPPED: NoticeText = NoticeText {
    id: "login-stopped",
    code: "st",
    title: "Claude sign-in stopped",
    happened: "Claude Code's sign-in stopped on a message of its own.",
    did: "aterm presses no keys in the sign-in from here.",
    todo: "Follow the login message in the tab to finish signing in.",
    expect: CONTINUES,
};

/// `/login` not typed at all, and not about to be (`{why}`: one of
/// [`WHYS`]).
pub const LOGIN_NOT_RUN: NoticeText = NoticeText {
    id: "login-not-run",
    code: "nr",
    title: "Claude's login expired",
    happened: EXPIRED,
    did: "aterm did not run /login here because {why}.",
    todo: TYPE_LOGIN,
    expect: CONTINUES,
};

/// The login `{label}` was renewed, and only a draft in the composer holds
/// the tab's move onto it.
pub const RENEWED_DRAFT: NoticeText = NoticeText {
    id: "renewed-draft",
    code: "rd",
    title: "Claude login renewed",
    happened: "This tab's login was renewed, and the tab still runs on the old one.",
    did: "aterm holds the move onto {label} because a draft is in the composer.",
    todo: "Send the draft in the tab.",
    expect: MOVES,
};

/// The login `{label}` was renewed, and nothing here can move the tab onto
/// it (`{why}`: `relaunch` or `resume_limits` off, no host that relaunches).
/// The step is the one the owner can always take in the tab itself: the
/// line `accounts renew` printed may be on another machine, or gone.
pub const RENEWED_BY_HAND: NoticeText = NoticeText {
    id: "renewed-by-hand",
    code: "rh",
    title: "Claude login renewed",
    happened: "This tab's login was renewed, and the tab still runs on the old one.",
    did: "aterm cannot move this tab by itself because {why}.",
    todo: TYPE_LOGIN,
    expect: CONTINUES,
};

/// A roster account is signed out: no session is moved onto it.
pub const ACCOUNT_SIGNED_OUT: NoticeText = NoticeText {
    id: "account-signed-out",
    code: "so",
    title: "Claude account signed out",
    happened: "A Claude account in the roster is signed out.",
    did: "aterm moves no session onto a signed-out account.",
    todo: "Run in a terminal: aterm harness accounts login {label}",
    expect: "Sessions can move onto the account again once it is signed in.",
};

/// A renewal signed in an account other than its row's: nothing live
/// changed.
pub const RENEWAL_WRONG_ACCOUNT: NoticeText = NoticeText {
    id: "renewal-wrong-account",
    code: "wa",
    title: "Claude renewal used another account",
    happened: "A renewal signed in a Claude account other than its roster row's.",
    did: "aterm discarded that sign-in of {label} and changed nothing live.",
    todo: "Run in a terminal: aterm harness accounts renew {label}",
    expect: "Sessions keep their login until {label} is renewed as its own account.",
};

/// Every notice, for the tests that hold them to their shape.
pub const ALL: &[NoticeText] = &[
    LOGIN_SIGN_IN,
    LOGIN_SIGN_IN_AS,
    LOGIN_CHOOSE,
    LOGIN_CANCELLED,
    LOGIN_LEFT,
    LOGIN_STOPPED,
    LOGIN_NOT_RUN,
    RENEWED_DRAFT,
    RENEWED_BY_HAND,
    ACCOUNT_SIGNED_OUT,
    RENEWAL_WRONG_ACCOUNT,
];

/// The characters of a roster label a notice's sentences name whole: a
/// longer one is shown clipped there (it is a name, not a command), and a
/// step that would run it is [`RENEW_DUE`] instead, which renews it with
/// every other due login.
pub const LABEL_ROOM: usize = 24;

/// The step a renewal falls back to where the label would not fit it whole:
/// a command is never cut. An ended login is due, so it is renewed too.
pub const RENEW_DUE: &str = "Run in a terminal: aterm harness accounts renew --due";

/// What stands for a label in any other step it would not fit whole: the
/// host's ask names the labels in its details.
pub const LABEL_UNDER_DETAILS: &str = "with the label under Details";

/// `text` with `label` and `why` in their placeholders.
fn fill_one(text: &str, label: Option<&str>, why: Option<&str>) -> String {
    let mut out = text.to_string();
    if let Some(label) = label {
        out = out.replace(LABEL, label);
    }
    if let Some(why) = why {
        out = out.replace(WHY, why);
    }
    out
}

/// The four parts filled.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Filled {
    pub happened: String,
    pub did: String,
    pub todo: String,
    pub expect: String,
}

impl NoticeText {
    /// Every part with `label` and `why` put in their placeholders (a part
    /// with none is its constant). A label past [`LABEL_ROOM`] is clipped in
    /// the sentences, and a step it would push past the engine's cap is
    /// [`RENEW_DUE`] for a renewal, else names [`LABEL_UNDER_DETAILS`].
    #[must_use]
    pub fn fill(&self, label: Option<&str>, why: Option<&str>) -> Filled {
        let named = label.map(|l| {
            if l.chars().count() <= LABEL_ROOM {
                l.to_string()
            } else {
                let mut cut: String = l.chars().take(LABEL_ROOM).collect();
                cut.push('\u{2026}');
                cut
            }
        });
        let named = named.as_deref();
        let mut todo = fill_one(self.todo, label, why);
        if todo.chars().count() > aterm_messages::need::STEP_CAP {
            todo = if self.renews() {
                RENEW_DUE.to_string()
            } else {
                fill_one(self.todo, Some(LABEL_UNDER_DETAILS), why)
            };
        }
        Filled {
            happened: fill_one(self.happened, named, why),
            did: fill_one(self.did, named, why),
            todo,
            expect: fill_one(self.expect, named, why),
        }
    }

    /// Whether the step runs a renewal of the label it names: one past the
    /// cap falls back to [`RENEW_DUE`] ([`Self::fill`]).
    #[must_use]
    pub fn renews(&self) -> bool {
        self.todo.contains("accounts renew {label}")
    }

    /// The parts in the order a reader reads them.
    #[must_use]
    pub fn parts(&self) -> [&'static str; 4] {
        [self.happened, self.did, self.todo, self.expect]
    }
}

#[cfg(test)]
#[path = "notice_tests.rs"]
mod tests;
