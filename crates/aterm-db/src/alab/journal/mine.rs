// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! A Mac's own `login` and `usage` rows (docs/DESIGN-shared-files-2026-11-11.md §10, files 3
//! and 4): what it holds of each account and its latest reading of each account's limit
//! windows, published into the journal's week ring beside the `use` and `place` rows, which
//! keep being written, and projected as the shared files `ai/logins` and `ai/usage`
//! (`crate::file`).
//!
//! - **Value-free by type.** A [`Login`] holds a lab key, words of the closed lists below, counts,
//!   Unix seconds and one bounded `[a-z0-9-]` word (the plan): the harness's reading of
//!   `logins.json` and `state.json` becomes one in its own words, and the `aterm` executable
//!   checks every word again on the way (`crates/aterm/src/alab_journal.rs`). Every row the
//!   flusher writes passes [`checked`] first: each cell its column's type and the text guard
//!   (`typed::cell::validate`, which refuses a credential-shaped word), no column the
//!   collection lacks, none it needs missing. A row that fails is not written. No token, its
//!   state or any other secret is a row: those are the fleet vault's alone (the owner,
//!   2026-11-13, `docs/OWNER_RULINGS.md`).
//! - **Local files only.** The harness reports logins through the source the executable installs
//!   ([`install_logins`]), one report per CLI: `None` where its store did not read now, which
//!   leaves that CLI's rows as the book holds them, so a passing failure of one store ends no
//!   row and freezes no other CLI's. The flusher asks for the report where it asks for the
//!   holdings, after its quiet gate and before it takes the journal writer. Nothing is polled and
//!   no other Mac is asked.
//! - **The usage row is the book's** ([`super::book::Facts::usage_row`]): the readings, walls,
//!   plan and sessions the `use` row states, folded from the same notes, so the two never
//!   disagree.
//! - **Pace** ([`super::book::Book::logins_due`], [`super::book::Book::usages_due`]): an urgent
//!   change ([`urgent`]) goes in the next record the pace line admits; any other waits until its
//!   row was last written an hour ago, and only its latest value is written; and each row is
//!   restated in the first record of every week, a login's while the harness reported it this
//!   week or the last. A login the report of its CLI no longer names is restated once as no
//!   longer held here ([`released`]), then forgotten. An urgent change reaches the flusher's
//!   quiet gate either as the sharp event the harness notes beside it (`Event::sharp`), or as
//!   the stamps of the files a login row is read from ([`Source::moved`], a look at each file
//!   and no read): a minute in which neither moved reads no store, and one in which only a file
//!   moved reads the logins and no table unless a login's row changed urgently.

use super::event::{Pk, Word};
use super::place::Why;
use crate::Result;
use crate::typed::{Collection, cell};
use aterm_json::{Map, Value};
use std::collections::BTreeMap;
use std::sync::OnceLock;

macro_rules! closed {
    ($(#[$meta:meta])* $name:ident { $($(#[$vmeta:meta])* $variant:ident = $word:literal),+ $(,)? }) => {
        $(#[$meta])*
        #[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
        pub enum $name { $($(#[$vmeta])* $variant),+ }

        impl $name {
            /// Every word, in declaration order: the column's `Ty::Enum`.
            pub const WORDS: [&'static str; [$($word),+].len()] = [$($word),+];
            const ALL: [Self; [$($word),+].len()] = [$(Self::$variant),+];

            /// The word a row carries.
            #[must_use]
            pub const fn word(self) -> &'static str {
                match self { $(Self::$variant => $word),+ }
            }

            /// The value `word` names.
            #[must_use]
            pub fn parse(word: &str) -> Option<Self> {
                Self::ALL.into_iter().find(|v| v.word() == word)
            }
        }
    };
}

closed!(
    /// The CLI a login is of (`super::CLIS`).
    Cli {
        Claude = "claude",
        Codex = "codex",
        Grok = "grok",
        Kimi = "kimi",
        Agy = "agy",
        Muse = "muse",
        CodexAlab = "codex-alab",
    }
);

closed!(
    /// Where the login lives: the CLI's default home, or a directory aterm manages. Never a
    /// path.
    Place {
        Default = "default",
        Managed = "managed",
    }
);

closed!(
    /// What this Mac's login of the account is now. No build states `key-only` or `routed` yet.
    State {
        SignedIn = "signed-in",
        SignedOut = "signed-out",
        Ended = "ended",
        WrongAccount = "wrong-account",
        KeyOnly = "key-only",
        Routed = "routed",
    }
);

closed!(
    /// What proved the login is the account's.
    Proof {
        Status = "status",
        Page = "page",
        File = "file",
        Unchecked = "unchecked",
    }
);

closed!(
    /// Where this Mac reads the login: Claude Code's profile, a CLI's auth file, a CLI's own
    /// log, or the vault token a session runs on.
    Src {
        Profile = "profile",
        AuthFile = "auth-file",
        CliLog = "cli-log",
        VaultToken = "vault-token",
    }
);

closed!(
    /// Whether a supervised session runs on the login now.
    Use {
        Running = "running",
        Idle = "idle",
    }
);

closed!(
    /// Why this Mac holds the login: a preference of its own names it (`linked`), or the reason
    /// its `place` row gives.
    Held {
        Linked = "linked",
        Fill = "fill",
        Borrow = "borrow",
        Stranded = "stranded",
        Probation = "probation",
    }
);

impl Held {
    /// The word of a `place` row's reason.
    #[must_use]
    pub const fn of(why: Why) -> Self {
        match why {
            Why::Fill => Self::Fill,
            Why::Borrow => Self::Borrow,
            Why::Stranded => Self::Stranded,
            Why::Probation => Self::Probation,
        }
    }
}

closed!(
    /// How this Mac's last sign-in of the account ended.
    Attempt {
        None = "none",
        SignedIn = "signed-in",
        Failed = "failed",
        Blocked = "blocked",
        NoRoute = "no-route",
    }
);

closed!(
    /// The route a sign-in took. Only `vault` is stated, of a sign-in that found the account's
    /// vault token in use here: the routes any other attempt tried are bounded free text in
    /// `logins.json`, which a shared file never carries.
    Route {
        Vault = "vault",
        Device = "device",
        Browser = "browser",
        Window = "window",
        MailCode = "mail-code",
    }
);

closed!(
    /// The window of a usage row's standing wall, `none` with none. A wall of the journal's own
    /// `day` or `month` window has no word here and is not stated; no build states `auth`.
    Wall {
        None = "none",
        FiveHour = "5h",
        Week = "7d",
        WeekModel = "7d-model",
        Credits = "credits",
        Auth = "auth",
    }
);

/// One login this Mac holds, as its harness reads it from `logins.json` and `state.json`. Times
/// are Unix seconds; a row states them as milliseconds.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Login {
    pub account: Pk,
    pub cli: Cli,
    pub place: Place,
    pub state: State,
    pub proof: Proof,
    pub src: Src,
    pub use_: Use,
    /// The supervised sessions on it.
    pub tabs: u64,
    pub held: Held,
    /// Since when the state has held.
    pub since: u64,
    pub served_at: Option<u64>,
    pub ends_by: Option<u64>,
    pub ended_at: Option<u64>,
    pub attempt: Attempt,
    pub attempt_at: Option<u64>,
    pub attempt_route: Option<Route>,
    /// The plan word its renewal's check answered.
    pub plan: Option<Word<16>>,
}

/// Unix seconds as a row's milliseconds.
pub(crate) const fn ms(t: u64) -> u64 {
    t.saturating_mul(1000)
}

fn put(row: &mut Map, key: &str, value: impl Into<Value>) {
    row.insert(key.to_owned(), value.into());
}

fn put_some(row: &mut Map, key: &str, value: Option<impl Into<Value>>) {
    if let Some(value) = value {
        put(row, key, value);
    }
}

impl Login {
    /// The row's content: the row without its counter `n` and the time `at` it is stated.
    pub(crate) fn content(&self) -> Value {
        let mut row = Map::new();
        put(&mut row, "r", super::book::VERSION);
        put(&mut row, "account", self.account.as_str());
        put(&mut row, "cli", self.cli.word());
        put(&mut row, "place", self.place.word());
        put(&mut row, "state", self.state.word());
        put(&mut row, "proof", self.proof.word());
        put(&mut row, "src", self.src.word());
        put(&mut row, "use", self.use_.word());
        put(&mut row, "tabs", self.tabs);
        put(&mut row, "held", self.held.word());
        put(&mut row, "since", ms(self.since));
        put_some(&mut row, "served_at", self.served_at.map(ms));
        put_some(&mut row, "ends_by", self.ends_by.map(ms));
        put_some(&mut row, "ended_at", self.ended_at.map(ms));
        put(&mut row, "attempt", self.attempt.word());
        put_some(&mut row, "attempt_at", self.attempt_at.map(ms));
        put_some(
            &mut row,
            "attempt_route",
            self.attempt_route.map(Route::word),
        );
        put_some(&mut row, "plan", self.plan.as_ref().map(Word::as_str));
        Value::Object(row)
    }
}

/// The row of `content` (a [`Login::content`] or a usage row's,
/// [`super::book::Facts::usage_row`]) at counter `n`, stated at `now` (Unix seconds).
pub(crate) fn row(content: &Value, n: u64, now: u64) -> Value {
    let mut row = content.as_object().cloned().unwrap_or_default();
    put(&mut row, "n", n);
    put(&mut row, "at", ms(now));
    Value::Object(row)
}

/// What `row` says, without its counter and the time it was stated: what two statements of it
/// are compared by.
pub(crate) fn content(row: &Value) -> Value {
    let mut row = row.as_object().cloned().unwrap_or_default();
    for key in ["n", "at"] {
        row.remove(key);
    }
    Value::Object(row)
}

/// THE GUARD every `login` and `usage` row passes before the flusher writes it: each cell is a
/// value of its column's type that the text guard admits (`typed::cell::validate`: a closed
/// word, a key, a count, a time, or text with no address, credential shape or secret's name in
/// it), its one text column (`plan`) holds a plan word the registry seeds ([`plan_word`]), the
/// row names no column `collection` lacks (beside its schema version `r`), and none it needs is
/// missing.
///
/// # Errors
/// The rule the row breaks, naming its column and never its value.
pub(crate) fn checked(collection: &Collection, row: &Value) -> Result<()> {
    let refuse = |why: &str| crate::Error::new(&format!("{}: {why}", collection.name));
    let object = row
        .as_object()
        .ok_or_else(|| refuse("a row is an object"))?;
    for (key, x) in object {
        if key == "r" {
            if x.as_u64() != Some(super::book::VERSION) {
                return Err(refuse("r: the row's schema version"));
            }
            continue;
        }
        let column = collection
            .columns
            .iter()
            .find(|column| column.name == key.as_str() && column.retired.is_none())
            .ok_or_else(|| refuse("a column the collection does not have"))?;
        cell::validate(collection, column, x)?;
        if matches!(column.ty, crate::typed::Ty::Text(_)) && !x.as_str().is_some_and(plan_word) {
            return Err(refuse(&format!("{key}: a plan word the registry seeds")));
        }
    }
    match collection.columns.iter().find(|column| {
        !column.null && column.retired.is_none() && !object.contains_key(column.name)
    }) {
        Some(column) => Err(refuse(&format!(
            "{}: this column needs a value",
            column.name
        ))),
        None => Ok(()),
    }
}

/// Whether `word` names a plan the registry seeds (`alab::seeds::PLANS`, `<provider>-<word>` or
/// `<provider>-<word>-<tier>`): the plan words a CLI states and the harness notes.
pub(crate) fn plan_word(word: &str) -> bool {
    crate::alab::seeds::PLANS.iter().any(|seed| {
        seed.pk.split_once('-').is_some_and(|(_, slug)| {
            slug == word
                || slug
                    .strip_prefix(word)
                    .is_some_and(|tier| tier.starts_with('-'))
        })
    })
}

/// Whether a row's content moved from `old` (`Value::Null` for a row never stated) to `new` in a
/// way the fleet hears at once (docs/DESIGN-shared-files-2026-11-11.md §10, "Publication pace"):
/// a login that turned signed out, ended or onto the wrong account, a sign-in that was blocked,
/// or an end, and a change away from any of them (signed back in, unblocked); a usage row's wall
/// hit or cleared, or its end moved. Any other change is routine.
pub(crate) fn urgent(old: &Value, new: &Value) -> bool {
    urgent_part(old) != urgent_part(new)
}

/// What in a row's content is urgent ([`urgent`]).
fn urgent_part(content: &Value) -> Map {
    let mut out = Map::new();
    let word = |key: &str| content.get(key).and_then(Value::as_str);
    let mut keep = |key: &str| {
        if let Some(value) = content.get(key) {
            out.insert(key.to_owned(), value.clone());
        }
    };
    if word("state").is_some_and(|state| {
        [State::SignedOut, State::Ended, State::WrongAccount]
            .iter()
            .any(|urgent| urgent.word() == state)
    }) {
        keep("state");
    }
    if word("attempt") == Some(Attempt::Blocked.word()) {
        keep("attempt");
    }
    keep("ended_at");
    if word("wall").is_some_and(|wall| wall != Wall::None.word()) {
        keep("wall");
        keep("wall_until");
    }
    out
}

/// What a report says of each CLI's logins: `Some` (empty included) where its store read, every
/// login of the CLI this Mac holds now; `None` where it did not read now, which leaves the CLI's
/// rows as the book holds them. A CLI the report names no entry for is as `None`.
pub type Reports = BTreeMap<Cli, Option<Vec<Login>>>;

/// What reports this Mac's logins: the `aterm` executable's, from each CLI's accounts store.
pub trait Source {
    /// Whether a login row may read otherwise than at the last [`Self::report`]: a file it is
    /// read from moved, by a look at each file's stamp and no read, or the clock reached a word
    /// that turns by time alone (a token reading aging out); `true` before the first report.
    /// What lets a pass with nothing else due learn of a login whose row changed urgently with
    /// no sharp event beside it.
    fn moved(&self) -> bool;

    /// Each CLI's logins now; `None` where none is known (no lab names this Mac).
    fn report(&self) -> Option<Reports>;
}

/// The source a process with none installed has: nothing moves, and nothing is reported.
pub(crate) struct Unreported;

impl Source for Unreported {
    fn moved(&self) -> bool {
        false
    }

    fn report(&self) -> Option<Reports> {
        None
    }
}

static LOGINS: OnceLock<Box<dyn Source + Send + Sync>> = OnceLock::new();

/// Install this process's login source (the `aterm` executable's, from each CLI's accounts
/// store); the first install wins. With none installed the flusher writes no login row.
pub fn install_logins(source: Box<dyn Source + Send + Sync>) -> bool {
    LOGINS.set(source).is_ok()
}

/// The installed source, or [`Unreported`] with none installed.
pub(crate) fn logins() -> &'static dyn Source {
    match LOGINS.get() {
        Some(source) => source.as_ref(),
        None => &Unreported,
    }
}

/// The content a login's row says once the report of its CLI no longer names it (its roster row
/// left this Mac, its store holds no word of it any more, or another account's login took its
/// place): no login of the account is held here, so its last row says `signed-out` since `now`,
/// with no session on it. Every other column says what its last report did. This is what a
/// reader keeps for the retention span (`aterm_file::retain::MINE`) once its weeks are gone,
/// never the working login it last stated.
pub(crate) fn released(content: &Value, now: u64) -> Value {
    let mut row = content.as_object().cloned().unwrap_or_default();
    put(&mut row, "state", State::SignedOut.word());
    put(&mut row, "use", Use::Idle.word());
    put(&mut row, "tabs", 0_u64);
    put(&mut row, "since", ms(now));
    Value::Object(row)
}
