// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! The one judgment of whether a Claude Code launch starts on its account's vault token
//! (`docs/DESIGN-claude-setup-token-2026-11-05.md` §5.3, slice S8): [`judge`].
//!
//! Two callers share it, so a line never changes route under its plan's intent:
//!
//! - **the relaunch plan** (`relaunch::plan`), which types `aterm harness accounts token-run
//!   <label> -- <build> <args…>` for a [`Decision::Token`], and the line it types today otherwise;
//! - **the wrapper** (`accounts_token_run`), which serves the token only where the judgment says
//!   [`Decision::Token`] again at exec, and otherwise runs the program exactly as typed. It only
//!   ever falls back: it never serves past what the plan decided.
//!
//! The steps run in order, each either passing or ending in a fallback whose word names why
//! ([`Fallback::word`]): the platform (`platform`), `ATERM_NO_HARNESS`
//! (`no-harness`), the launch's shape (`shape`), the program (`program`), today's judgment of the
//! launch's login (`outside`), the claims ring for an unlabelled launch (`claimed`, `held`), and
//! the token's standing here (`absent`, `busy`, `unverified`, `awaiting-basis`, `differ`,
//! `ended`, `rejected`, `mismatched`, `unreadable`). A reason names a variable or a document,
//! never a value.
//!
//! The wrapper serves only a launch today's judgment puts on the row's stored login
//! ([`super::accounts::ProcessLogin::of_env`], `with_settings`, `with_profile_store`): it strips
//! no route and overrides no settings, so a gateway, a proxy, a settings `env` block or an
//! `apiKeyHelper` never receives the token, and the launch runs as it would today.

use super::accounts::{self, ProcessLogin, Roster, TokenEntry};
use super::netwatch::Source;
pub use crate::supervise::policy::turn_end::RouteTo;
use std::path::{Path, PathBuf};

/// The marker aterm's wrapper sets beside the descriptor: the token entry it served.
pub const MARKER_ENV: &str = "ATERM_CLAUDE_TOKEN";

/// The variable aterm's wrapper sets beside the marker: the `minted_at` (unix seconds) of the
/// token it served, value-free, so a reader knows which token of the entry a session runs on
/// ([`ServedToken`]): after a `--replace`, a session still on the earlier token names that one.
pub const MINTED_ENV: &str = "ATERM_CLAUDE_TOKEN_MINTED";

/// The variable Claude Code reads a token's descriptor number from (§2.4).
pub const DESCRIPTOR_ENV: &str = "CLAUDE_CODE_OAUTH_TOKEN_FILE_DESCRIPTOR";

/// The token state a launch inherits from no one: a Claude relaunch line that replaces a token
/// session unsets each, and the wrapper makes each afresh (§5.4). [`MINTED_ENV`] is not one: it
/// names a token only beside these two (every reader judges it under the token route they make),
/// so without them it names nothing, and the wrapper unsets and sets it whenever it serves. A
/// line that drops the wrapper therefore spends no bytes on it, and stays shorter than the line
/// that keeps the wrapper.
pub const TOKEN_STATE_ENV: [&str; 2] = [DESCRIPTOR_ENV, MARKER_ENV];

/// Which token of its entry aterm's wrapper served a session on its account's vault token, as the
/// session's exec-time environment ([`MINTED_ENV`]) and its birth say (§5.6): the marker names the
/// entry alone, so after a `--replace` a session still on the earlier token reads as one on the
/// entry.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ServedToken {
    /// The wrapper named the token it served: its `minted_at`.
    Named(u64),
    /// The variable is unset: an earlier aterm's wrapper, which named none, served the session,
    /// which began at the unix second here, where known.
    Unnamed(Option<u64>),
    /// The variable is not a number, or is set twice to different values: it names nothing.
    Garbled,
}

/// What a session's reading or refusal is to the vault's token ([`ServedToken::on`]).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum OnToken {
    /// The session runs on it, its wrapper said so: its reading counts for it and is compared for
    /// its verification, and its refusal is the token's.
    Named,
    /// The session runs on it, though its wrapper named no token: the token was made before the
    /// session began, so no later one can be what it was served. Its reading counts for the token
    /// and its row, and its refusal is the token's, but nothing of it is compared for the token's
    /// verification. A replacement that an earlier build's waiting write landed after the session
    /// began, made before it began, is the one token it can be mistaken for.
    Unnamed,
    /// The session may run on another token (one a `--replace` has since replaced), or names
    /// nothing: nothing of it counts for the vault's token or is known to be the row's.
    Other,
}

impl ServedToken {
    /// What the process whose exec-time environment is `env`, begun at `born` (unix seconds,
    /// where known), was served.
    #[must_use]
    pub fn of<'a>(env: impl IntoIterator<Item = (&'a str, &'a str)>, born: Option<u64>) -> Self {
        let mut named = None;
        for (name, value) in env {
            if name != MINTED_ENV {
                continue;
            }
            let Ok(value) = value.parse::<u64>() else {
                return Self::Garbled;
            };
            if named.is_some_and(|first| first != value) {
                return Self::Garbled;
            }
            named = Some(value);
        }
        named.map_or(Self::Unnamed(born), Self::Named)
    }

    /// What the session is to the vault's token made at `minted_at`.
    #[must_use]
    pub fn on(self, minted_at: u64) -> OnToken {
        match self {
            Self::Named(served) if served == minted_at => OnToken::Named,
            Self::Unnamed(Some(born)) if minted_at < born => OnToken::Unnamed,
            Self::Named(_) | Self::Unnamed(_) | Self::Garbled => OnToken::Other,
        }
    }

    /// Whether the session is known to run on another token than the vault's made at
    /// `minted_at`: its wrapper named another, or named one in words that do not read, or an
    /// earlier wrapper served it no later than that token was made. A session an earlier wrapper
    /// served whose birth is not known ([`Self::Unnamed`]`(None)`) may run on that token or an
    /// earlier one, so it is not, though its readings count for neither ([`Self::on`]).
    #[must_use]
    pub const fn off(self, minted_at: u64) -> bool {
        match self {
            Self::Named(served) => served != minted_at,
            Self::Unnamed(Some(born)) => minted_at >= born,
            Self::Garbled => true,
            Self::Unnamed(None) => false,
        }
    }
}

/// The program the route serves.
pub const PROGRAM: &str = "claude";

/// Whether a launch on this host can start on a vault token at all: only on macOS, where spawnfd
/// keeps a named descriptor across an exec ([`Fallback::Platform`], the judgment's first step).
/// Everything that says a row runs on its token here takes this step too
/// ([`accounts::on_token_here`], the upkeep's reading), so no surface counts a token no launch
/// here could start on.
pub const LAUNCHES_ON_TOKEN: bool = cfg!(target_os = "macos");

/// The words the wrapper's verb takes after `aterm` (`harness accounts token-run`).
pub const VERB: [&str; 3] = ["harness", "accounts", "token-run"];

/// The flag of an unlabelled run: the account is the one the launch's own login names.
pub const AUTO: &str = "--auto";

/// Why a launch does not start on the token: one word, and what it names.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Fallback {
    /// Not macOS: spawnfd keeps a named descriptor across an exec only there.
    Platform,
    /// `ATERM_NO_HARNESS` is set (`claude --no-harness`).
    NoHarness,
    /// Not a session-starting shape: what the launch carries.
    Shape(&'static str),
    /// Not a Claude Code build in atpkg's store.
    Program,
    /// Today's judgment puts the launch off the row's stored login: the variable or document.
    Outside(String),
    /// The claims ring: another Mac's claim or this Mac's clock (`WhyNot`'s word).
    Claimed(&'static str),
    /// The row is held by another Mac, named.
    Held(String),
    /// The account has no token entry, or none the lab can key.
    Absent,
    /// The vault state, or this Mac's token table, could not be read in time.
    Busy,
    /// Not verified yet, and not in use here before verification: minted on another Mac at a
    /// person's terminal, or by a meta that names no route.
    Unverified,
    /// Not verified, in use here by its route, and no session on the row's stored login here has
    /// reported its windows since (no basis, §5.6), while that stored login is usable here: the
    /// launch stays on it so that one reports.
    AwaitingBasis,
    /// Not verified, and this Mac's comparison found its windows different from the row's basis
    /// (§5.6).
    Differ,
    /// Its end has passed.
    Ended,
    /// This Mac saw Claude Code refuse it.
    Rejected,
    /// A check of the stored token failed, or its record names another account.
    Mismatched,
    /// This Mac opens no epoch its seal names.
    Unreadable,
}

impl Fallback {
    /// The reason word, the one a ledger and the journal carry.
    #[must_use]
    pub const fn word(&self) -> &'static str {
        match self {
            Self::Platform => "platform",
            Self::NoHarness => "no-harness",
            Self::Shape(_) => "shape",
            Self::Program => "program",
            Self::Outside(_) => "outside",
            Self::Claimed(_) => "claimed",
            Self::Held(_) => "held",
            Self::Absent => "absent",
            Self::Busy => "busy",
            Self::Unverified => "unverified",
            Self::AwaitingBasis => "awaiting-basis",
            Self::Differ => "differ",
            Self::Ended => "ended",
            Self::Rejected => "rejected",
            Self::Mismatched => "mismatched",
            Self::Unreadable => "unreadable",
        }
    }

    /// The word and what it names, value-free (`outside: `ANTHROPIC_BASE_URL` in the
    /// environment`).
    #[must_use]
    pub fn why(&self) -> String {
        let detail = match self {
            Self::Shape(what) => (*what).to_owned(),
            Self::Outside(what) | Self::Held(what) => what.clone(),
            Self::Claimed(what) => (*what).to_owned(),
            _ => return self.word().to_owned(),
        };
        format!("{}: {detail}", self.word())
    }
}

/// The account a launch is judged on: its row's label where a row names it, and its email.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Account {
    /// The roster row's label; `None` for an account no row names (its login record's).
    pub label: Option<String>,
    /// The row's claims email, else the login record's.
    pub email: String,
}

impl Account {
    /// How a labelled line names it: the row's label, else the email.
    #[must_use]
    pub fn named(&self) -> &str {
        self.label.as_deref().unwrap_or(&self.email)
    }
}

/// What the judgment decided.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Decision {
    /// Start on the account's token.
    Token {
        /// The account.
        account: Account,
        /// Its token entry.
        entry: TokenEntry,
    },
    /// The launch is the row's stored login's, and starts on it: why not on the token.
    Stored(Fallback),
    /// The token route has no part in this launch: it runs exactly as typed.
    AsTyped(Fallback),
}

impl Decision {
    /// The fallback, where the judgment made one.
    #[must_use]
    pub const fn fallback(&self) -> Option<&Fallback> {
        match self {
            Self::Token { .. } => None,
            Self::Stored(why) | Self::AsTyped(why) => Some(why),
        }
    }
}

/// What the token's standing here says (§5.1), for [`judge`].
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum TokenFact {
    /// In use here, and its record names the account.
    InUse,
    /// No entry.
    Absent,
    /// The vault state, or this Mac's token table, could not be read in time.
    Busy,
    /// Not verified yet, and not in use here before verification (minted on another Mac at a
    /// person's terminal, or by a meta that names no route).
    Unverified,
    /// Not verified, in use here by its route, with no basis here yet, and the row's stored login
    /// is usable here: sessions stay on it until one reports its windows (§5.6).
    AwaitingBasis,
    /// Not verified, and this Mac's comparison found its windows different from the row's basis:
    /// this Mac starts no session on it.
    Differ,
    /// Ended.
    Ended,
    /// Refused here.
    Rejected,
    /// A check failed, or the record names another account.
    Mismatched,
    /// Unreadable here.
    Unreadable,
}

/// The facts [`judge`] reads at once; the costlier ones it asks ([`Ask`]) only when the steps
/// before them passed.
#[derive(Clone, Copy, Debug)]
pub struct Facts<'a> {
    /// This is macOS.
    pub macos: bool,
    /// `ATERM_NO_HARNESS` is set non-empty.
    pub no_harness: bool,
    /// The launch's argv, `argv[0]` first.
    pub argv: &'a [String],
    /// The program is a Claude Code build in atpkg's store ([`store_build`]).
    pub store_build: bool,
}

/// What [`judge`] asks once the cheap steps passed.
pub trait Ask {
    /// Today's judgment: the account whose stored login the launch is on, and its token entry
    /// (`None`: none the lab can key); or why the launch is off it, naming the variable or the
    /// document.
    ///
    /// # Errors
    /// The launch is off its row's stored login.
    fn account(&self) -> Result<(Account, Option<TokenEntry>), String>;
    /// The claims ring's verdict on `account` (an unlabelled launch only: a labelled line was
    /// admitted under `accounts.lock` before it was typed), where it refuses one.
    fn claims(&self, account: &Account) -> Option<Fallback>;
    /// The token's standing here for `account`'s `entry`.
    fn token(&self, entry: TokenEntry, account: &Account) -> TokenFact;
}

/// THE ONE JUDGMENT (§5.3): the steps in order, each passing or ending in a fallback.
#[must_use]
pub fn judge(facts: &Facts<'_>, ask: &dyn Ask) -> Decision {
    if !facts.macos {
        return Decision::AsTyped(Fallback::Platform);
    }
    if facts.no_harness {
        return Decision::AsTyped(Fallback::NoHarness);
    }
    if let Some(what) = shape_refusal(facts.argv) {
        return Decision::AsTyped(Fallback::Shape(what));
    }
    if !facts.store_build {
        return Decision::Stored(Fallback::Program);
    }
    let (account, entry) = match ask.account() {
        Ok(found) => found,
        Err(why) => return Decision::AsTyped(Fallback::Outside(why)),
    };
    let Some(entry) = entry else {
        return Decision::Stored(Fallback::Absent);
    };
    if let Some(why) = ask.claims(&account) {
        return Decision::Stored(why);
    }
    let why = match ask.token(entry, &account) {
        TokenFact::InUse => return Decision::Token { account, entry },
        TokenFact::Absent => Fallback::Absent,
        TokenFact::Busy => Fallback::Busy,
        TokenFact::Unverified => Fallback::Unverified,
        TokenFact::AwaitingBasis => Fallback::AwaitingBasis,
        TokenFact::Differ => Fallback::Differ,
        TokenFact::Ended => Fallback::Ended,
        TokenFact::Rejected => Fallback::Rejected,
        TokenFact::Mismatched => Fallback::Mismatched,
        TokenFact::Unreadable => Fallback::Unreadable,
    };
    Decision::Stored(why)
}

/// Why `argv` (`argv[0]` first) is not a session-starting shape the token serves, where it is
/// not: [`super::tap::launch_admits`] over it with the tap's own `--settings` value set aside
/// ([`super::tap::without_own_tap`]) and a `-p`/`--print` set aside, so an interactive launch,
/// `-p`, `--resume` and `--continue` are served, and a subcommand (`mcp`, `doctor`, `plugin`,
/// `update`, `auth`, `setup-token`), a one-word prompt, an option no relaunch carries, or a
/// `--settings` or `--bare` of its own is not.
#[must_use]
pub fn shape_refusal(argv: &[String]) -> Option<&'static str> {
    let kept = super::tap::without_own_tap(argv);
    let Some((first, flags)) = kept.split_first() else {
        return Some("no program");
    };
    let mut plain = vec![first.clone()];
    for (name, tokens) in super::upgrade::parse_flag_spans(flags) {
        if !matches!(name, Some("-p" | "--print")) {
            plain.extend(tokens);
        }
    }
    if super::tap::launch_admits(&plain) {
        return None;
    }
    Some(match super::tap::line_refusal(&plain) {
        Some(super::tap::Refusal::Line) => "its own `--settings` or `--bare`",
        Some(_) => "an option no relaunch carries",
        None => "a subcommand or a one-word prompt",
    })
}

/// Why today's judgment puts a launch with environment `env`, settings chain `settings` and the
/// vendor's profile store (`profile_store`) off its stored login, naming the variable or the
/// document, never a value: the first a reader meets ([`ProcessLogin::of_env`],
/// [`ProcessLogin::with_settings`], [`ProcessLogin::with_profile_store`]).
#[must_use]
pub fn outside_why(
    env: &[(&str, &str)],
    settings: &[(String, Source)],
    profile_store: bool,
) -> Option<String> {
    let mut login = None;
    for (name, value) in env {
        if *name == accounts::LOGIN_ENV {
            match login {
                Some(first) if first != *value => {
                    return Some(format!("`{}` given twice", accounts::LOGIN_ENV));
                }
                _ => login = login.or(Some(*value)),
            }
        }
        if value.is_empty() {
            continue;
        }
        if *name == accounts::HOME_ENV || accounts::names_another_login(name, value) {
            return Some(format!("`{name}` in the environment"));
        }
    }
    for (document, source) in settings {
        let text = match source {
            Source::Missing => continue,
            Source::Unreadable => return Some(format!("`{document}` cannot be read")),
            Source::Text(text) => text,
        };
        let Some(read) = super::netwatch::settings_of(text) else {
            return Some(format!("`{document}` cannot be parsed"));
        };
        if read.api_key_helper {
            return Some(format!("`apiKeyHelper` in `{document}`"));
        }
        if let Some((name, _)) = read.env.iter().find(|(name, value)| {
            name == accounts::LOGIN_ENV
                || (!value.is_empty()
                    && (name == accounts::HOME_ENV || accounts::names_another_login(name, value)))
        }) {
            return Some(format!("`{name}` in `{document}`"));
        }
    }
    profile_store.then(|| "the vendor's profile store holds a configuration".to_owned())
}

/// How a launch names its account: a label (or an email) the line typed, or `--auto`.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Named<'a> {
    /// A row's label, or an account's email.
    Label(&'a str),
    /// The row the launch's login resolves, else the account its login record names.
    Auto,
}

/// TODAY'S JUDGMENT of a launch whose login facts are `who` (after its settings and profile
/// store: [`ProcessLogin::with_settings`], [`ProcessLogin::with_profile_store`]), under `roster`:
/// the account whose stored login it is on, and that account's token entry. The route must be
/// the account's own ([`accounts::AuthRoute::for_account`]: a stored login, or the token whose
/// marker names this account's entry) under the default home. The account is the row
/// `row_for_login` resolves, else, with no row, the one the login record names
/// (`login_email`); a label must name that same row, and an email that same account. `why`
/// names what moved the launch off it, where today's judgment does.
///
/// # Errors
/// The launch is off its row's stored login, or under another row than the one named.
pub fn account_of(
    who: &ProcessLogin,
    roster: Option<&Roster>,
    named: Named<'_>,
    login_email: &dyn Fn(Option<&str>) -> Option<String>,
    why: &dyn Fn() -> Option<String>,
) -> Result<(Account, Option<TokenEntry>), String> {
    let row = roster.and_then(|roster| roster.row_for_login(who.login.as_deref()));
    let entry = row.and_then(TokenEntry::of_row);
    let outside = || why().unwrap_or_else(|| "the launch's own credential".to_owned());
    if !who.default_home {
        return Err(outside());
    }
    let account = match row {
        Some(row) => Account {
            label: Some(row.label.clone()),
            email: row
                .expected_identity_email()
                .map(str::to_owned)
                .unwrap_or_default(),
        },
        None => Account {
            label: None,
            email: login_email(who.login.as_deref()).unwrap_or_default(),
        },
    };
    let entry = match row {
        Some(_) => entry,
        None => (!account.email.is_empty())
            .then(|| TokenEntry::of_email(&account.email))
            .flatten(),
    };
    match who.route.for_account(entry) {
        accounts::AuthRoute::Stored | accounts::AuthRoute::VaultToken { .. } => {}
        accounts::AuthRoute::Outside => return Err(outside()),
    }
    match named {
        Named::Auto => {}
        Named::Label(name) if name.contains('@') => {
            if !accounts::email_matches(&account.email, name) {
                return Err(format!(
                    "`{}` names the login of another account than {name}",
                    accounts::LOGIN_ENV
                ));
            }
        }
        Named::Label(label) => {
            if account.label.as_deref() != Some(label) {
                return Err(format!(
                    "`{}` names another row's login than {label}'s",
                    accounts::LOGIN_ENV
                ));
            }
        }
    }
    let entry = entry.filter(|_| !account.email.is_empty());
    Ok((account, entry))
}

/// The HTTP status Claude Code answers a token it refuses with: `401`. Its lost-login wall also
/// stands over a `403`, Cloudflare's block page, which it writes as `authentication_failed`
/// (`Please run /login`) with that status, though the credential works (measured 2026-11-13,
/// 2.1.323); that is no refusal of the token.
pub const REFUSED_STATUS: u16 = 401;

/// The status Claude Code wrote with the newest API error of a session's transcript tail `jsonl`:
/// `apiErrorStatus` on its newest main-chain `isApiErrorMessage` row, where that row carries one.
/// `None` where the tail holds no such row, or its newest names no status.
#[must_use]
pub fn newest_api_error_status(jsonl: &str) -> Option<u16> {
    jsonl
        .lines()
        .rev()
        .find_map(|line| {
            let v = super::transcript::conversation(line).ok()?;
            let flag = |key: &str| v.get(key).and_then(aterm_json::Value::as_bool) == Some(true);
            (v.get("type").and_then(aterm_json::Value::as_str) == Some("assistant")
                && flag("isApiErrorMessage")
                && !flag("isSidechain"))
            .then(|| {
                v.get("apiErrorStatus")
                    .and_then(aterm_json::Value::as_u64)
                    .and_then(|status| u16::try_from(status).ok())
            })
        })
        .flatten()
}

/// WHETHER A LOST LOGIN ON THE RUNNING SESSION `who` IS ITS VAULT TOKEN'S REFUSAL (§5.6), and if
/// so, recording it: the session runs on its account's token ([`session_account`],
/// [`accounts::AuthRoute::for_account`]), so the anchored lost-login wall Claude Code drew
/// (`alab_policy::vocab::WallKind::Auth` at its placement, which the loop's wall confirmation never reads
/// from a tool's output under a call, nor from the transcript) is the token's, where the session's
/// newest API error answered [`REFUSED_STATUS`] (`status`, read from its transcript,
/// [`newest_api_error_status`]). Any other status, a `403` from Cloudflare's block page above all,
/// or none, records nothing: a refusal holds the token off this Mac until a new one is made, and a
/// block page says nothing of the token. Its standing is read value-free from the vault at `paths`
/// and the refusal recorded beside it at `now`, this Mac's table row in `store` following the
/// refused token ([`super::accounts_tokens::note_rejected`]: the standing's `refused_at`,
/// `token-rejected`), only where the session runs on the vault's token as `served` says
/// ([`ServedToken::on`]: its wrapper named it, or an earlier wrapper named none and the token was
/// made before the session began): a session still on a token a `--replace` has since replaced,
/// or one whose variable names nothing, met a refusal of its own token, which says nothing of the
/// vault's. `None` for a session on its stored login, or off its account's credentials: that point
/// is the stored login's, handled as before. `Some(false)` for a session on its token whose wall
/// answered no `401`, and where the refusal was recorded before, is of a token the vault no longer
/// holds, or could not be read just now (the next point records it).
#[cfg(unix)]
#[must_use]
pub fn token_refused(
    home: &Path,
    paths: &aterm_secrets::client::Paths,
    store: &accounts::Store,
    roster: Option<&Roster>,
    (who, served): (&ProcessLogin, ServedToken),
    status: Option<u16>,
    now: u64,
) -> Option<bool> {
    use aterm_secrets::claude_token::{self, Key, Lookup};
    let login = who.login.as_deref().filter(|login| !login.is_empty());
    let record = super::profile::login_record(home, login);
    let (_, entry) = session_account(who, roster, record.map(|record| record.email))?;
    match who.route.for_account(Some(entry)) {
        accounts::AuthRoute::VaultToken { .. } => {}
        accounts::AuthRoute::Stored | accounts::AuthRoute::Outside => return None,
    }
    if status != Some(REFUSED_STATUS) {
        return Some(false);
    }
    let Some(key) = Key::parse(entry.key().as_str()) else {
        return Some(false);
    };
    Some(match claude_token::standing(paths, &key) {
        Ok(Lookup::Found(standing))
            if standing
                .meta
                .as_ref()
                .is_some_and(|meta| served.on(meta.minted_at) != OnToken::Other) =>
        {
            super::accounts_tokens::note_rejected(paths, store, &standing, REFUSED_STATUS, now)
                .unwrap_or(false)
        }
        Ok(Lookup::Found(_) | Lookup::Absent | Lookup::Unknown) | Err(_) => false,
    })
}

/// The aterm a line types for the wrapper: this process's lasting one
/// ([`super::tap::stable_exe`]), or a test's ([`with_wrapper`]).
#[must_use]
pub fn wrapper_aterm() -> Option<PathBuf> {
    #[cfg(any(test, debug_assertions))]
    if let Some(aterm) = WRAPPER.with(|wrapper| wrapper.borrow().clone()) {
        return Some(aterm);
    }
    super::tap::stable_exe()
}

#[cfg(any(test, debug_assertions))]
thread_local! {
    /// This thread's aterm for wrapper lines ([`with_wrapper`]).
    static WRAPPER: std::cell::RefCell<Option<PathBuf>> = const { std::cell::RefCell::new(None) };
}

/// Run `f` with [`wrapper_aterm`] answering `aterm` on this thread.
#[cfg(any(test, debug_assertions))]
pub fn with_wrapper<T>(aterm: PathBuf, f: impl FnOnce() -> T) -> T {
    let was = WRAPPER.with(|wrapper| wrapper.replace(Some(aterm)));
    let out = f();
    WRAPPER.with(|wrapper| *wrapper.borrow_mut() = was);
    out
}

/// WHAT A LINE TYPED FOR A SESSION ANOTHER LAUNCHER STARTS RUNS (a worker's `spawn`, the GUI's
/// task launch; §5.4): where it types `exe`, a Claude Code build in atpkg's store past the managed
/// twin ([`store_build`]), with `flags`, a session-starting shape ([`shape_refusal`]), the
/// wrapper ahead of the build, `<aterm> harness accounts token-run --auto -- <exe> <flags…>`: the
/// words the twin's own token block types, so a line that skips the twin (for its yolo) still
/// starts on its account's token. The wrapper judges the rest at exec, on the line's own
/// environment (its login, settings and claims) and the token's standing, and falls back to the
/// build as typed. `None` (the line types `exe`) off macOS, for any
/// other program (the twin's block judges a line that types the twin), or with no lasting aterm.
#[must_use]
pub fn auto_line(exe: &Path, flags: &[String]) -> Option<(PathBuf, Vec<String>)> {
    if !LAUNCHES_ON_TOKEN {
        return None;
    }
    store_build(exe)?;
    let program = exe.to_str()?.to_owned();
    let argv: Vec<String> = std::iter::once(program.clone())
        .chain(flags.iter().cloned())
        .collect();
    if shape_refusal(&argv).is_some() {
        return None;
    }
    let aterm = wrapper_aterm()?;
    let args = VERB
        .iter()
        .map(|word| (*word).to_owned())
        .chain([AUTO.to_owned(), "--".to_owned(), program])
        .chain(flags.iter().cloned())
        .collect();
    Some((aterm, args))
}

/// The build `program` is where it has a Claude Code build's shape in a store: its real path (an
/// exec entry resolved) reads `…/store/claude/<build>/bin/claude` and is a file. The wrapper and
/// the plan hand the token only to such a build in atpkg's own store ([`store_build_in`]).
#[must_use]
pub fn store_build(program: &Path) -> Option<PathBuf> {
    let real = std::fs::canonicalize(program).ok()?;
    let names: Vec<&str> = real
        .components()
        .rev()
        .take(5)
        .filter_map(|c| match c {
            std::path::Component::Normal(s) => s.to_str(),
            _ => None,
        })
        .collect();
    let shaped = matches!(
        names.as_slice(),
        [PROGRAM, "bin", build, PROGRAM, "store"]
            if !build.is_empty() && build.bytes().all(|b| b.is_ascii_digit())
                && !(build.len() > 1 && build.starts_with('0'))
    );
    (shaped && real.is_file()).then_some(real)
}

/// atpkg's own store of Claude Code builds, `<prefix>/store/claude` under its configured prefix:
/// the one store whose builds the wrapper serves the token to ([`store_build_in`]), and the plan
/// types the wrapper for, so the two judge one build alike.
#[must_use]
pub fn configured_builds() -> Option<PathBuf> {
    atpkg::store::resolve_configured().map(|layout| layout.prefix.join("store").join(PROGRAM))
}

/// The build `program` is ([`store_build`]) where it lies in `builds`, atpkg's own store of
/// Claude Code builds (`<prefix>/store/claude`): its real path is `<builds>/<build>/bin/claude`,
/// with `builds` resolved too. The wrapper execs this real path, never the spelling it was handed,
/// so the file it judged is the file it runs. A same-user process can still change a build in a
/// store it owns: this is a gate against a program of another's choosing, not a boundary between
/// processes of one user (§5.3).
#[must_use]
pub fn store_build_in(builds: &Path, program: &Path) -> Option<PathBuf> {
    let real = store_build(program)?;
    let builds = std::fs::canonicalize(builds).ok()?;
    let root = real.parent()?.parent()?.parent()?;
    (root == builds).then_some(real)
}

/// What the token's state on this Mac says for [`judge`]: in use (and, the caller checks, its
/// record names the account, and that it awaits no basis its row's stored login can give,
/// [`standing_judged`]), or why not.
#[cfg(unix)]
#[must_use]
pub fn fact_of(state: &super::accounts_tokens::TokenState) -> TokenFact {
    use super::accounts_tokens::{TokenState, Unverified};
    match state {
        _ if state.in_use() => TokenFact::InUse,
        TokenState::Unknown => TokenFact::Busy,
        TokenState::None => TokenFact::Absent,
        TokenState::Unreadable => TokenFact::Unreadable,
        TokenState::Mismatched(_) => TokenFact::Mismatched,
        TokenState::Ended => TokenFact::Ended,
        TokenState::Rejected { .. } => TokenFact::Rejected,
        TokenState::Unverified {
            why: Unverified::WindowsDiffer,
            ..
        } => TokenFact::Differ,
        TokenState::Unverified { .. }
        | TokenState::Exposed(_)
        | TokenState::Soon
        | TokenState::Active => TokenFact::Unverified,
    }
}

/// The token's standing for `account`'s `entry` in the vault at `paths`, judged with this Mac's
/// table and accounts state (`store`) and `roster` at `now` ([`standing_judged`]), waiting no
/// longer than `wait` for a vault writer: in use with its record naming the account's email, or
/// why not. With the read, where it was found.
#[cfg(unix)]
#[must_use]
pub fn standing_fact(
    paths: &aterm_secrets::client::Paths,
    store: &accounts::Store,
    roster: Option<&Roster>,
    entry: TokenEntry,
    account: &Account,
    now: u64,
    wait: std::time::Duration,
) -> (TokenFact, Option<aterm_secrets::claude_token::TokenRead>) {
    use aterm_secrets::claude_token::{self, Key, Lookup};
    let Some(key) = Key::parse(entry.key().as_str()) else {
        return (TokenFact::Absent, None);
    };
    let read = match claude_token::read(paths, &key, wait) {
        Ok(read) => read,
        Err(_) => return (TokenFact::Busy, None),
    };
    let (standing, read) = match read {
        Lookup::Unknown => (Lookup::Unknown, None),
        Lookup::Absent => (Lookup::Absent, None),
        Lookup::Found(read) => (Lookup::Found(read.standing.clone()), Some(read)),
    };
    match standing_judged(&standing, paths, store, roster, &key, account, now) {
        TokenFact::InUse => (TokenFact::InUse, read),
        fact => (fact, None),
    }
}

/// [`standing_fact`] of a standing already read, value-free (`claude_token::standing`): in use
/// with its record naming the account's email, or why not. Judged with this Mac's token table in
/// `store` ([`super::accounts_tokens::TokenState::of`]), which must read: one that does not is
/// `busy`, never taken for an empty one. A check file that does not read holds `busy` only a
/// token this Mac compares (minted here, or in use here unverified), whose verdict it holds; any
/// other is judged by `tokens.json` alone, its verification being the vault's word
/// ([`super::accounts_tokens::read_table_for`]). The vault's token is judged by this Mac's facts
/// of that token ([`super::accounts_tokens::Table::row_for`]): while this Mac's write of a
/// replacement is on its way, the token it replaces keeps its own verdict and refusal, so it
/// serves where it served before and nowhere else. Beyond the token's state, THE HOLD (§5.6): a
/// token in use here and not verified, wherever it was minted, whose row here records no basis
/// with a week still running ([`super::accounts_tokens::Unverified::AwaitingBasis`]), is not in
/// use while the row's stored login is usable here ([`stored_usable`]): a launch stays on that
/// stored login, so its session's reading gives the basis a reading of the token is compared
/// with; where this Mac's table holds no row of the held token (one another Mac minted, or a
/// mint whose row this Mac did not write), the row that reading records the basis on is made then
/// ([`super::accounts_tokens::note_held`]). A row whose stored login is not usable here is
/// not held, so the hold never leaves an account with no route: the token serves it unverified,
/// and its readings are compared with nothing until a basis comes.
#[cfg(unix)]
#[must_use]
pub fn standing_judged(
    standing: &aterm_secrets::claude_token::Lookup<aterm_secrets::claude_token::Standing>,
    paths: &aterm_secrets::client::Paths,
    store: &accounts::Store,
    roster: Option<&Roster>,
    key: &aterm_secrets::claude_token::Key,
    account: &Account,
    now: u64,
) -> TokenFact {
    use super::accounts_tokens::{TokenState, Unverified};
    use aterm_secrets::claude_token::Lookup;
    let Ok(table) = super::accounts_tokens::read_table_for(store, standing) else {
        return TokenFact::Busy;
    };
    let state = TokenState::of(standing, table.row_for(key.as_str(), standing), now);
    let fact = fact_of(&state);
    if fact != TokenFact::InUse {
        return fact;
    }
    let named = match standing {
        Lookup::Found(standing) => standing
            .meta
            .as_ref()
            .is_some_and(|meta| accounts::email_matches(&meta.email, &account.email)),
        Lookup::Absent | Lookup::Unknown => false,
    };
    if !named {
        return TokenFact::Mismatched;
    }
    let awaits = matches!(
        state,
        TokenState::Unverified {
            why: Unverified::AwaitingBasis,
            ..
        }
    );
    if !awaits || !stored_usable(&paths.home, store, roster, account, now) {
        return TokenFact::InUse;
    }
    // Held with no row of its own here: the row the stored login's reading records the basis on
    // is made now, so the hold lifts at that reading.
    if let Some(label) = account.label.as_deref()
        && let Lookup::Found(found) = standing
        && table
            .row_for(key.as_str(), standing)
            .is_none_or(|row| row.minted_at != found.meta.as_ref().map(|meta| meta.minted_at))
    {
        let _ = super::accounts_tokens::note_held(store, found, label);
    }
    TokenFact::AwaitingBasis
}

/// Whether the roster row `account` names has a stored login sessions on this Mac can run on, as
/// the hold reads it ([`standing_judged`]): the row is in `roster` and held by no other Mac, its
/// login's record under the user's `home` ([`super::profile::login_record`]) names the account's
/// email, and the accounts state in `store`, its lapses read from the row's own credential file
/// ([`accounts::Store::load_judged`]), does not say it is signed out here
/// ([`accounts::signed_out_here`]). A state that does not read says nothing against the record.
/// An account no row names has no stored login a row's reading could give a basis for.
#[cfg(unix)]
fn stored_usable(
    home: &Path,
    store: &accounts::Store,
    roster: Option<&Roster>,
    account: &Account,
    now: u64,
) -> bool {
    let Some((roster, row)) = account
        .label
        .as_deref()
        .zip(roster)
        .and_then(|(label, roster)| Some((roster, roster.row(label)?)))
    else {
        return false;
    };
    if row.held_by.is_some() {
        return false;
    }
    let Some(record) = super::profile::login_record(home, row.dir.as_deref()) else {
        return false;
    };
    if !accounts::email_matches(&record.email, &account.email) {
        return false;
    }
    let now = i64::try_from(now).unwrap_or(i64::MAX);
    store
        .load_judged(now, roster)
        .map_or(true, |(state, _)| !accounts::signed_out_here(row, &state))
}

/// WHETHER A LAUNCH ONTO `row` OF `roster` WOULD START ON A WORKING LOGIN HERE, as the one
/// judgment's row-wide steps decide it ([`judge`]): macOS (`platform`), a token entry the lab keys
/// and an email on the row (`absent`), and the token's standing in the vault at `paths`, read
/// value-free with `claude_token::standing`'s brief wait and judged with this Mac's table in
/// `store` and `roster` at `now` ([`standing_judged`]): in use, its record naming the row's email,
/// or held for its basis ([`TokenFact::AwaitingBasis`]), which the judgment says only where the
/// row's stored login is usable here, so the launch runs on that login. The launch-time guard asks
/// it of a row that only its token puts in rotation ([`accounts::token_guarded`]): where it says
/// no, the launch would fall back to the row's lapsed stored login, so the pick is refused. The
/// launch's own steps (its shape, its program, its environment) are judged at the launch as before.
#[cfg(unix)]
#[must_use]
pub fn row_token_serves(
    paths: &aterm_secrets::client::Paths,
    store: &accounts::Store,
    roster: &Roster,
    row: &accounts::Row,
    now: u64,
) -> bool {
    if !LAUNCHES_ON_TOKEN {
        return false;
    }
    let (Some(entry), Some(email)) = (TokenEntry::of_row(row), row.expected_identity_email())
    else {
        return false;
    };
    let Some(key) = aterm_secrets::claude_token::Key::parse(entry.key().as_str()) else {
        return false;
    };
    let account = Account {
        label: Some(row.label.clone()),
        email: email.to_owned(),
    };
    aterm_secrets::claude_token::standing(paths, &key).is_ok_and(|standing| {
        matches!(
            standing_judged(&standing, paths, store, Some(roster), &key, &account, now),
            TokenFact::InUse | TokenFact::AwaitingBasis
        )
    })
}

/// [`row_token_serves`] for a launch on this Mac under the user's `home`; never off Unix.
#[must_use]
pub fn row_token_serves_at(
    home: &Path,
    store: &accounts::Store,
    roster: &Roster,
    row: &accounts::Row,
    now: i64,
) -> bool {
    #[cfg(unix)]
    {
        row_token_serves(
            &aterm_secrets::client::Paths::for_home(home),
            store,
            roster,
            row,
            u64::try_from(now).unwrap_or(0),
        )
    }
    #[cfg(not(unix))]
    {
        let _ = (home, store, roster, row, now);
        false
    }
}

/// THE ROUTE MOVE A RUNNING SESSION IS OFFERED (§5.5), from its route at the gate that resolved
/// its account (`route`, after [`accounts::AuthRoute::for_account`]), its account's token's
/// standing here (`fact`) and whether the row has a stored login on this Mac (`stored_here`):
///
/// - a session on its stored login moves onto its account's token where that is in use here;
/// - a session on its token moves back onto the stored login where the token was refused here,
///   removed, ended or mismatched, or its windows differ here from the row's basis (§5.6: it may
///   be another account's), or it is not in use here unverified (`unverified`: a token minted on
///   another Mac at a person's terminal, or by a meta that names no route, which this Mac serves
///   only once verified), and only where the stored login is here: with none, it stays on the
///   token, and what §5.9 offers for the token's state stands;
/// - nothing else moves. An exposed token is in use (`fact_of`), so it moves nothing (§4.3); a
///   vault that could not be read just now and a token this Mac cannot open move nothing either:
///   the next turn end asks again. A token held for its basis (`awaiting-basis`) moves nothing: a
///   session on the stored login stays there to give the basis, and one already on the token
///   stays on it (its readings are compared once a basis comes).
#[must_use]
pub const fn route_move(
    route: accounts::AuthRoute,
    fact: TokenFact,
    stored_here: bool,
) -> Option<RouteTo> {
    match (route, fact) {
        (accounts::AuthRoute::Stored, TokenFact::InUse) => Some(RouteTo::Token),
        (
            accounts::AuthRoute::VaultToken { .. },
            TokenFact::Absent
            | TokenFact::Unverified
            | TokenFact::Differ
            | TokenFact::Ended
            | TokenFact::Rejected
            | TokenFact::Mismatched,
        ) if stored_here => Some(RouteTo::Stored),
        (
            accounts::AuthRoute::Stored
            | accounts::AuthRoute::VaultToken { .. }
            | accounts::AuthRoute::Outside,
            _,
        ) => None,
    }
}

/// The account a running session `who` is on (§5.2): the row `row_for_login` resolves under
/// `roster`, else the account the login record names (`login_email`); and that account's token
/// entry, where the lab keys one.
#[must_use]
pub fn session_account(
    who: &ProcessLogin,
    roster: Option<&Roster>,
    login_email: Option<String>,
) -> Option<(Account, TokenEntry)> {
    let login = who.login.as_deref().filter(|login| !login.is_empty());
    match roster.and_then(|roster| roster.row_for_login(login)) {
        Some(row) => {
            let email = row.expected_identity_email()?.to_owned();
            Some((
                Account {
                    label: Some(row.label.clone()),
                    email,
                },
                TokenEntry::of_row(row)?,
            ))
        }
        None => {
            let email = login_email.filter(|email| !email.is_empty())?;
            let entry = TokenEntry::of_email(&email)?;
            Some((Account { label: None, email }, entry))
        }
    }
}

/// [`route_move`] for a running Claude Code session `who` on this Mac (the GUI host's
/// `IdleHost::route_move`): its account ([`session_account`]), under the user's `home`, the
/// vault at `paths` (read value-free, never waiting past `claude_token::standing`'s brief wait)
/// and this Mac's token table in `store`, at `now`. A session on its token known to run on
/// another token than the vault's (`served`, [`ServedToken::off`]: a `--replace` replaced the one
/// it was served, what its wrapper named does not read, or it began before the vault's token was
/// made) is judged by its own token, which no longer serves anywhere: it moves onto the vault's
/// token where that is in use here, else onto the row's stored login where that is here
/// ([`stale_move`]). One an earlier wrapper served whose birth is not known may run on the
/// vault's token or an earlier one, so it is judged as one on the vault's token
/// ([`route_move`]), which offers only the moves both readings agree on: back to the stored login
/// where that is here and the vault's token was refused here, removed, ended or mismatched, its
/// windows differ or this Mac does not read it verified; never a relaunch onto the vault's token,
/// which might only serve it again, and no move off one held for its basis. Nothing moves off
/// macOS, or for a session under a custom home.
#[cfg(unix)]
#[must_use]
pub fn offered_move(
    home: &Path,
    paths: &aterm_secrets::client::Paths,
    store: &accounts::Store,
    roster: Option<&Roster>,
    (who, served): (&ProcessLogin, ServedToken),
    now: u64,
) -> Option<RouteTo> {
    use aterm_secrets::claude_token::Lookup;
    if !cfg!(target_os = "macos") || !who.default_home {
        return None;
    }
    let login = who.login.as_deref().filter(|login| !login.is_empty());
    let record = super::profile::login_record(home, login);
    let (account, entry) = session_account(who, roster, record.as_ref().map(|r| r.email.clone()))?;
    let route = who.route.for_account(Some(entry));
    if matches!(route, accounts::AuthRoute::Outside) {
        return None;
    }
    let key = aterm_secrets::claude_token::Key::parse(entry.key().as_str())?;
    let standing = match aterm_secrets::claude_token::standing(paths, &key) {
        Ok(standing) => standing,
        Err(_) => return route_move(route, TokenFact::Busy, record.is_some()),
    };
    let fact = standing_judged(&standing, paths, store, roster, &key, &account, now);
    let stale = matches!(route, accounts::AuthRoute::VaultToken { .. })
        && match &standing {
            Lookup::Found(found) => found
                .meta
                .as_ref()
                .is_some_and(|meta| served.off(meta.minted_at)),
            Lookup::Absent | Lookup::Unknown => false,
        };
    if stale {
        return stale_move(fact, record.is_some());
    }
    route_move(route, fact, record.is_some())
}

/// The move a session on its token known to run on another token than the vault's is offered
/// ([`offered_move`], [`ServedToken::off`]), from the vault's token's standing here (`fact`) and
/// whether the row has a stored login on this Mac (`stored_here`): onto the vault's token where
/// it is in use here (a relaunch serves it), else onto the stored login where it is here; with
/// neither, nothing. A vault that could not be read just now moves nothing: the next turn end
/// asks again.
#[must_use]
pub const fn stale_move(fact: TokenFact, stored_here: bool) -> Option<RouteTo> {
    match fact {
        TokenFact::InUse => Some(RouteTo::Token),
        TokenFact::Busy => None,
        _ if stored_here => Some(RouteTo::Stored),
        _ => None,
    }
}

#[cfg(test)]
#[path = "token_route_tests.rs"]
mod tests;
