// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! THE CREDENTIAL BEHIND A LOGIN, AS FACTS (plan package SF-34, decision SF-D3; the owner,
//! 2026-11-15): what this Mac knows of the one credential a login's sign-in runs on, without its
//! value, for its `ai/logins` row (`docs/DESIGN-shared-files-2026-11-11.md` §10 file 3). Each
//! client's reader states its kind, where it is kept, when it was issued, when it ends, when it
//! was last refreshed and used, and what it is now ([`TokenFacts`]); a time nothing here states
//! is `None`, never a guess.
//!
//! One rule for every client: the end a row states is the credential's own, where nothing renews
//! it. A grant its CLI refreshes (an OAuth login that holds a refresh token) states none, since
//! its access token's end moves at every refresh and is no end of the login; a setup-token, and
//! an access token held with no refresh token, state theirs, `expiring` within the 30 days the
//! vault's `soon` counts ([`aterm_secrets::claude_token::SOON_SECS`]) and `expired` once it
//! passed. `valid` and `expiring` are stated only where the row's login serves by its scoped
//! verdict on record ([`accounts::Account::login_verdict`]), `expired` where it is signed out
//! here ([`accounts::signed_out_here`]), and `unknown` otherwise. Both are judged on the records
//! with this Mac's runtime readings in them ([`Judged`]), as the report and the pick judge rows.
//!
//! - **Claude Code on its vault token** (the upkeep's reading has the row on it here,
//!   [`accounts::on_token_here`], the rule the row's `src` `vault-token` follows): a
//!   `setup-token` in the `secret-store`, issued at its mint and last used as this Mac's table
//!   (`tokens.json` and `tokens-check.json`, value-free) says of the token the reading names (the
//!   one whose end it is), ending at the end the vault's meta states, never refreshed; `refused`
//!   where this Mac recorded Claude Code's refusal of that very token
//!   ([`aterm_secrets::claude_token::refused_here`], the file beside the vault state; no vault
//!   read). A row off its token whose stored login does not serve here states the account's
//!   token this Mac refused, or whose end passed, as `refused` or `expired`, so the fleet sees
//!   this Mac's setup-token is no longer good here ([`SetupTokens::withdrawn`]).
//! - **Claude Code on its stored login**: an `oauth` login, in its `login-file` where its
//!   `.credentials.json` is there (a stat), or else, where Claude Code keeps its logins in the
//!   keychain ([`crate::ai::connector::claude_default_login_in_file`]), the `keychain`; issued at
//!   the renewal that made it, last used at its last served turn. Its file, where it is in one, is
//!   read for whether it still holds a token, whether it holds a refresh token and its access
//!   token's end ([`crate::ai::connector::claude_stored_credential`], bounded and scrubbed, no
//!   value leaving it); a keychain-held login's end is read nowhere here.
//! - **Codex**: its `auth.json` ([`super::codex_app_login::stored_credential_in_home`], where it
//!   is the login of the account the row expects): an `oauth` login or an `api-key`, in its
//!   `login-file`, last refreshed at its `last_refresh`, issued at the renewal that made it;
//!   `refused` where the issuer's refusal of the very refresh token it holds is settled
//!   ([`super::codex_app_login::row_refusal`]). An API key names no account, so nothing ties the
//!   row's renewal or served turn to it: it states no time. It gives no credential context either,
//!   so no word on record is scoped to it, and it is `unknown`; an OAuth login's word is scoped to
//!   the tokens its file held when it was read, so it is `unknown` from its CLI's next refresh
//!   until the harness reads it again (`docs/KNOWN_ISSUES.md`, SF-34).
//! - **Grok Build, Kimi Code and agy** (SF-34b): an `oauth` login in its `login-file`, the one in
//!   the row's own login home (its directory, else its store's default login home), read by its
//!   client's reader (`login_token_stored.rs`: bounded, scrubbed, no value or error text leaving
//!   it), issued at the renewal that made it and last used at its last served turn, `expired`
//!   where the file holds no credential any more. A Grok or Kimi file whose own fields name
//!   another account than the row expects states nothing for the row. No field these readers
//!   read states when its CLI last refreshed it, so none states a refresh time.
//!   - Grok Build's `auth.json`: its reader reads only its records' `key`, `user_id` and `email`
//!     (grok 1.0.77's strings), since no signed-in file was measured, so it reads no end and no
//!     refresh token from it and states no end. It holds no credential where its records state a
//!     `key` and none is non-empty; one that states no `key` is held, as the harness counts a
//!     non-empty file grok has not refused ([`super::grok_signin::login_accepted`]). `refused`
//!     where this Mac marked grok's refusal of that very login, its `auth.json.refused` at least
//!     as new as it ([`super::grok_signin::refused`]).
//!   - Kimi Code's credential file (`credentials/<slot>.json`, the slot of the row's region; of
//!     a row with none, the file its connector names): its access token's end, `expires_at`, is
//!     stated where it holds no `refresh_token`; its `expires_in` is that token's lifetime, not
//!     when the file was written. `refused` where this Mac recorded the issuer's refusal of that
//!     very grant beside it (`shared_provider::kimi_rejected_snapshot`, whose record names the
//!     file's bytes or the empty tokens the refusal left them; Unix only).
//!   - agy's grant (`.gemini/antigravity-cli/antigravity-oauth-token`, Go's `oauth2.Token` in its
//!     `token`): its access token's end, `expiry` (RFC 3339 in the zone it was made in), is
//!     stated where it holds no `refresh_token`. Nothing on this Mac records Google's refusal of
//!     a grant, so it is never `refused`.
//!
//! Nothing here holds, prints or logs a value: [`TokenFacts`] is closed words and Unix seconds,
//! and each reader returns only those. The times are bounded where the journal takes them
//! (`aterm_db::alab::journal::mine::checked`), so no number can carry a value's bytes either.

use super::accounts::{self, AccountRecords, Cli, Row, Store};
use super::cli::shared_provider::kimi_credential_path;
use super::journal::Key;
use crate::ai::connector::Region;
use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

#[path = "login_token_stored.rs"]
mod stored;

/// What kind of credential a sign-in runs on.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Kind {
    /// An OAuth grant its CLI keeps and refreshes.
    OAuth,
    /// A Claude Code setup-token, minted once and never refreshed.
    SetupToken,
    /// An API key.
    ApiKey,
}

impl Kind {
    /// The word an `ai/logins` row states.
    #[must_use]
    pub const fn word(self) -> &'static str {
        match self {
            Self::OAuth => "oauth",
            Self::SetupToken => "setup-token",
            Self::ApiKey => "api-key",
        }
    }
}

/// Where this Mac keeps it.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Kept {
    /// The login keychain.
    Keychain,
    /// Its CLI's own login file.
    LoginFile,
    /// The fleet vault, aterm secrets.
    SecretStore,
}

impl Kept {
    /// The word an `ai/logins` row states.
    #[must_use]
    pub const fn word(self) -> &'static str {
        match self {
            Self::Keychain => "keychain",
            Self::LoginFile => "login-file",
            Self::SecretStore => "secret-store",
        }
    }
}

/// What it is now, as this Mac knows it without its value.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum State {
    /// Its login serves by the word on record, and it states no end (a grant its CLI renews, or
    /// one whose end nothing here reads) or one more than 30 days away.
    Valid,
    /// Its login serves by the word on record, and the end it states is 30 days or fewer away.
    Expiring,
    /// Its stated end passed with nothing to renew it, its login file holds no credential any
    /// more, or its login is signed out here.
    Expired,
    /// Its issuer refused this very credential here.
    Refused,
    /// Nothing on record says.
    Unknown,
}

impl State {
    /// The word an `ai/logins` row states.
    #[must_use]
    pub const fn word(self) -> &'static str {
        match self {
            Self::Valid => "valid",
            Self::Expiring => "expiring",
            Self::Expired => "expired",
            Self::Refused => "refused",
            Self::Unknown => "unknown",
        }
    }
}

/// THE FACTS OF THE CREDENTIAL A LOGIN'S SIGN-IN RUNS ON: closed words and Unix seconds, so it
/// holds no text of a value, digest, address or path; its times are bounded where the journal
/// takes them, so no number carries a value's bytes either.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct TokenFacts {
    pub kind: Kind,
    /// `None` where this Mac cannot tell where its CLI keeps it.
    pub kept: Option<Kept>,
    /// When the sign-in that made it, or a setup-token's mint, issued it.
    pub issued_at: Option<u64>,
    /// The end it states itself, where nothing renews it (a setup-token's, an access token's held
    /// with no refresh token): `None` for a grant its CLI refreshes, and where nothing here reads
    /// an end of it.
    pub expires_at: Option<u64>,
    /// When its CLI last refreshed it.
    pub refreshed_at: Option<u64>,
    /// When this Mac last served a session on it.
    pub last_used: Option<u64>,
    pub state: State,
}

/// What a reader of one row read ([`read`]): the facts, where its CLI's reader states them, and
/// the first second after the read at which they can read otherwise by the clock alone (a stated
/// end coming within 30 days or passing, a refusal coming to be settled), for the reader of the
/// row to read it again then.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Reading {
    pub facts: Option<TokenFacts>,
    pub turns: Option<i64>,
}

/// What this Mac's table (`tokens.json` and its checks, value-free) says of one vault token: its
/// mint, its end and its last session here.
#[cfg_attr(
    not(unix),
    allow(
        dead_code,
        reason = "no launch keeps a token table off Unix, so nothing makes one there"
    )
)]
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct Minted {
    minted_at: Option<u64>,
    expires_at: Option<u64>,
    last_served_at: Option<u64>,
}

/// This Mac's facts of each account's vault tokens, by account key: its table's row of the
/// account and the kept row of an earlier token the vault may still hold
/// (`accounts_tokens::Table::replaced`), and the mint of the account's token Claude Code refused
/// here, as the file beside the vault state records it
/// ([`aterm_secrets::claude_token::refused_here`]). Empty where neither reads, and off Unix,
/// where no launch keeps them.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct SetupTokens {
    tokens: BTreeMap<String, Vec<Minted>>,
    refused: BTreeMap<String, u64>,
}

impl SetupTokens {
    /// The table in Claude Code's store `store` ([`super::accounts_tokens::read_parts`]: its
    /// `tokens.json` and `tokens-check.json`) and the refusals beside the vault state at `vault`
    /// (none where it is `None`), as these facts, with no vault read; each part empty where it does
    /// not read.
    #[must_use]
    pub fn read(store: &Store, vault: Option<&aterm_secrets::client::Paths>) -> Self {
        #[cfg(unix)]
        {
            let refused = vault
                .map(aterm_secrets::claude_token::refused_here)
                .unwrap_or_default();
            match super::accounts_tokens::read_parts(store) {
                Ok(read) => Self::of(&read.table, &refused),
                Err(_) => Self::of(&super::accounts_tokens::Table::default(), &refused),
            }
        }
        #[cfg(not(unix))]
        {
            let _ = (store, vault);
            Self::default()
        }
    }

    /// The facts `table` and the refusals `refused` hold.
    #[cfg(unix)]
    #[must_use]
    pub fn of(
        table: &super::accounts_tokens::Table,
        refused: &BTreeMap<String, aterm_secrets::claude_token::RefusedHere>,
    ) -> Self {
        let mut tokens: BTreeMap<String, Vec<Minted>> = BTreeMap::new();
        for row in table.rows.values().chain(table.replaced.values()) {
            tokens.entry(row.key.clone()).or_default().push(Minted {
                minted_at: row.minted_at,
                expires_at: row.expires_at,
                last_served_at: row.used_on.last_served_at,
            });
        }
        Self {
            tokens,
            refused: refused
                .iter()
                .map(|(key, refusal)| (key.clone(), refusal.minted_at))
                .collect(),
        }
    }

    /// The files these facts are read from, for a reader that stamps them: the table's two files
    /// in `store`'s directory (off Unix none) and the refusals file beside the vault state at
    /// `vault`.
    #[must_use]
    pub fn files(store: &Store, vault: Option<&aterm_secrets::client::Paths>) -> Vec<PathBuf> {
        #[cfg(unix)]
        {
            let state = store.state_file();
            [
                super::accounts_tokens::TABLE_FILE,
                super::accounts_tokens::CHECK_FILE,
            ]
            .into_iter()
            .map(|name| state.with_file_name(name))
            .chain(vault.map(aterm_secrets::claude_token::refusals_file))
            .collect()
        }
        #[cfg(not(unix))]
        {
            let _ = (store, vault);
            Vec::new()
        }
    }

    /// What the table says of `key`'s token that ends at `end`: the one the upkeep's reading
    /// names, never a row of another token of the account.
    fn of_token(&self, key: &Key, end: Option<u64>) -> Option<Minted> {
        self.tokens
            .get(key.as_str())?
            .iter()
            .copied()
            .find(|row| end.is_some() && row.expires_at == end)
    }

    /// Whether this Mac recorded Claude Code's refusal of `key`'s token minted at `minted`: that
    /// very token, never another of the account.
    fn refused_of(&self, key: &Key, minted: Option<u64>) -> bool {
        minted.is_some() && self.refused.get(key.as_str()).copied() == minted
    }

    /// The account `key`'s token this Mac's table holds that is no longer good here at `now`, with
    /// what it is: the one Claude Code refused here (`refused`), else one whose end passed
    /// (`expired`), the account's own row before its kept one. `None` where the table holds no
    /// such token of the account.
    fn withdrawn(&self, key: &Key, now: i64) -> Option<(Minted, State)> {
        let rows = self.tokens.get(key.as_str())?;
        let refused = rows
            .iter()
            .find(|row| self.refused_of(key, row.minted_at))
            .map(|row| (*row, State::Refused));
        refused.or_else(|| {
            rows.iter()
                .find(|row| {
                    row.expires_at
                        .is_some_and(|end| i64::try_from(end).is_ok_and(|end| end <= now))
                })
                .map(|row| (*row, State::Expired))
        })
    }
}

/// Unix seconds as a row's, where they are after the epoch.
fn secs(t: i64) -> Option<u64> {
    u64::try_from(t).ok().filter(|t| *t > 0)
}

/// [`aterm_secrets::claude_token::SOON_SECS`] in seconds of a clock.
fn soon() -> i64 {
    i64::try_from(aterm_secrets::claude_token::SOON_SECS).unwrap_or(i64::MAX)
}

/// The earliest of `at` that is after `now`.
fn first_after(now: i64, at: impl IntoIterator<Item = Option<i64>>) -> Option<i64> {
    at.into_iter().flatten().filter(|at| *at > now).min()
}

/// What a credential whose login serves (`serves`) is at `now` by the end it states itself
/// (`end`, `None` for one its CLI renews), with the clock's turns of that end.
fn by_end(end: Option<i64>, serves: bool, now: i64) -> (State, Option<i64>) {
    let turns = end.and_then(|end| first_after(now, [Some(end.saturating_sub(soon())), Some(end)]));
    let state = match end {
        Some(end) if end <= now => State::Expired,
        _ if !serves => State::Unknown,
        Some(end) if end.saturating_sub(now) <= soon() => State::Expiring,
        _ => State::Valid,
    };
    (state, turns)
}

/// THE RECORDS A CREDENTIAL'S STATE IS JUDGED BY: one read of a store's records with this
/// Mac's runtime readings in them (`accounts::Store::hydrate_auth_contexts`), as the report and
/// the pick judge rows: which rows' own credential files show their login lapsed, and, for Codex,
/// each row's word on record scoped to the credentials its `auth.json` holds now. A read with
/// none of that ([`accounts::Store::peek_account_records`]) has no login lapsed and every Codex
/// word out of scope, so no Codex login would serve; [`read`] takes only these.
#[derive(Debug, Clone)]
pub struct Judged {
    records: AccountRecords,
}

impl Judged {
    /// `records`, one read of `store`, with this Mac's runtime readings hydrated: no write, no
    /// observation, and each row's credential file read only for its context and lapse. The
    /// records' own reading of the rows on their vault token ([`accounts::State::tokens_here`])
    /// stays, so a row and its credential are judged by one reading of it.
    #[must_use]
    pub fn of(store: &Store, records: &AccountRecords) -> Self {
        let mut records = records.clone();
        let tokens_here = records.state.tokens_here.take();
        store.hydrate_auth_contexts(&mut records.state, &records.roster);
        records.state.tokens_here = tokens_here;
        Self { records }
    }
}

/// The facts of the credential `cli`'s roster row `row` (of the account `key`) signs in with at
/// `now`, from `judged` (one read of its store `store`, [`Judged::of`]), the store's own login
/// files and, for Claude Code, its token facts `setup` ([`SetupTokens::read`]), with the clock's
/// next turn of them ([`Reading`]): no facts for a Codex, Grok Build, Kimi Code or agy row whose
/// login file is not there or holds nothing its reader reads (unreadable, larger than it reads,
/// no JSON object, or no credential Codex signs in with), nor for one that holds another
/// account's login by its own fields.
#[must_use]
pub fn read(
    cli: Cli,
    row: &Row,
    key: &Key,
    judged: &Judged,
    store: &Store,
    setup: &SetupTokens,
    now: i64,
) -> Reading {
    let records = &judged.records;
    match cli {
        Cli::Claude => match accounts::on_token_here(row, &records.state, now) {
            Some(end) => setup_token(setup, key, end, now),
            None => {
                let stored = claude_login(row, records, store, now);
                match setup.withdrawn(key, now) {
                    Some((token, state))
                        if stored
                            .facts
                            .is_none_or(|facts| facts.state == State::Expired) =>
                    {
                        Reading {
                            facts: Some(setup_facts(token, state)),
                            turns: stored.turns,
                        }
                    }
                    _ => stored,
                }
            }
        },
        Cli::Codex => codex_login(row, records, store, now),
        Cli::Grok => grok_login(row, records, store, now),
        Cli::Kimi => kimi_login(row, records, store, now),
        Cli::Agy => agy_login(row, records, store, now),
    }
}

/// The files [`read`] reads `row`'s credential from in its store `store`, for a reader that
/// stamps them to know the facts may read otherwise: under the row's login home, Claude Code's
/// and agy's credential file names; Codex's `auth.json`, the refresh journal beside it and the
/// configuration its credential context reads
/// ([`super::codex_app_login::credential_files_in_home`]); Grok Build's `auth.json` and its
/// refusal marker; Kimi Code's credential file of the row's region, with, on Unix, the
/// configuration and refusal record its refusal is read from, or of a row with none, the file
/// its connector names and the `credentials` folder the others are written into. The store's
/// own files and Claude Code's token facts ([`SetupTokens::files`]) are not among them.
#[must_use]
pub fn files(cli: Cli, row: &Row, store: &Store) -> Vec<PathBuf> {
    let Some(home) = login_home(row, store) else {
        return Vec::new();
    };
    let stores = || cli.connector().stores.iter().map(|name| home.join(name));
    match cli {
        Cli::Claude | Cli::Agy => stores().collect(),
        Cli::Codex => store.user_home().map_or_else(Vec::new, |user_home| {
            super::codex_app_login::credential_files_in_home(&home, user_home)
        }),
        Cli::Grok => stores()
            .flat_map(|file| {
                let marker = super::grok_signin::refused_marker(&file);
                [file, marker]
            })
            .collect(),
        Cli::Kimi => match row.vendor.region {
            Some(region) => std::iter::once(kimi_credential_path(&home, region))
                .chain(kimi_refusal_files(&home, region))
                .collect(),
            None => stores().chain([home.join("credentials")]).collect(),
        },
    }
}

/// A vault token's facts as this Mac's table holds them, in `state`.
fn setup_facts(token: Minted, state: State) -> TokenFacts {
    TokenFacts {
        kind: Kind::SetupToken,
        kept: Some(Kept::SecretStore),
        issued_at: token.minted_at,
        expires_at: token.expires_at,
        refreshed_at: None,
        last_used: token.last_served_at,
        state,
    }
}

/// The vault token of `key` a row runs on here, ending at `end` by the upkeep's reading: `refused`
/// where this Mac recorded Claude Code's refusal of that very token, else by its end.
fn setup_token(setup: &SetupTokens, key: &Key, end: i64, now: i64) -> Reading {
    let ends = secs(end);
    let token = setup.of_token(key, ends).unwrap_or(Minted {
        minted_at: None,
        expires_at: ends,
        last_served_at: None,
    });
    let (state, turns) = by_end(Some(end), true, now);
    Reading {
        facts: Some(setup_facts(
            Minted {
                expires_at: ends,
                ..token
            },
            if setup.refused_of(key, token.minted_at) {
                State::Refused
            } else {
                state
            },
        )),
        turns,
    }
}

/// The directory `row`'s login lives in: its own, else its store's default login home.
fn login_home(row: &Row, store: &Store) -> Option<PathBuf> {
    row.dir
        .as_deref()
        .filter(|dir| !dir.is_empty())
        .map(PathBuf::from)
        .or_else(|| store.default_login_home())
}

/// Whether `row`'s login serves by its verdict on record, scoped to the login the row routes to
/// now ([`accounts::Account::login_verdict`]: a word of a replaced login, or of other
/// credentials, says nothing).
fn serves(row: &Row, records: &AccountRecords) -> bool {
    records
        .state
        .accounts
        .get(&row.label)
        .and_then(|account| account.login_verdict(row))
        .is_some_and(|verdict| verdict.login == accounts::Login::Serves)
}

/// The issue and last use a login's records state: the renewal that made it, and its last
/// served turn.
fn issued_and_used(row: &Row, records: &AccountRecords) -> (Option<u64>, Option<u64>) {
    (
        records
            .logins
            .get(&row.label)
            .and_then(|binding| binding.renewed_at)
            .and_then(secs),
        accounts::login_served_at(row, &records.logins, &records.state).and_then(secs),
    )
}

/// Claude Code's stored login of `row`.
fn claude_login(row: &Row, records: &AccountRecords, store: &Store, now: i64) -> Reading {
    let home = login_home(row, store);
    let in_file = home
        .as_deref()
        .and_then(|home| crate::ai::connector::stored_login_file(Cli::Claude.connector(), home))
        .is_some();
    let file = home
        .as_deref()
        .and_then(crate::ai::connector::claude_stored_credential);
    let end = file
        .filter(|file| !file.refreshable)
        .and_then(|file| file.expires_at);
    let (by_end, turns) = by_end(end, serves(row, records), now);
    let (issued_at, last_used) = issued_and_used(row, records);
    Reading {
        facts: Some(TokenFacts {
            kind: Kind::OAuth,
            kept: if in_file {
                Some(Kept::LoginFile)
            } else if crate::ai::connector::claude_default_login_in_file() {
                None
            } else {
                Some(Kept::Keychain)
            },
            issued_at,
            expires_at: end.and_then(secs),
            refreshed_at: None,
            last_used,
            state: if file.is_some_and(|file| !file.held)
                || accounts::signed_out_here(row, &records.state)
            {
                State::Expired
            } else {
                by_end
            },
        }),
        turns,
    }
}

/// Codex's login of `row`, from its `auth.json`, where it is the login of the account the row
/// expects.
fn codex_login(row: &Row, records: &AccountRecords, store: &Store, now: i64) -> Reading {
    let Some(home) = login_home(row, store) else {
        return Reading::default();
    };
    let Some(stored) =
        super::codex_app_login::stored_credential_in_home(&home, row.expected_identity_email())
    else {
        return Reading::default();
    };
    let standing = super::codex_app_login::row_refusal(
        row,
        store.custody_record(&records.state, row),
        store.default_login_home().as_deref(),
        now,
    );
    let settles = match standing {
        super::codex_app_login::RefusalStanding::Pending { settles_at, .. } => Some(settles_at),
        _ => None,
    };
    let end = stored
        .expires_at
        .filter(|_| !stored.refreshable && !stored.api_key);
    let (by_end, turns) = by_end(end, serves(row, records), now);
    let (issued_at, last_used) = if stored.api_key {
        (None, None)
    } else {
        issued_and_used(row, records)
    };
    Reading {
        facts: Some(TokenFacts {
            kind: if stored.api_key {
                Kind::ApiKey
            } else {
                Kind::OAuth
            },
            kept: Some(Kept::LoginFile),
            issued_at,
            expires_at: end.and_then(secs),
            refreshed_at: stored.refreshed_at.and_then(secs),
            last_used,
            state: if standing.settled().is_some() {
                State::Refused
            } else if accounts::signed_out_here(row, &records.state) {
                State::Expired
            } else {
                by_end
            },
        }),
        turns: first_after(now, [turns, settles]),
    }
}

/// The login `row` signs in with in its CLI's own login file, as that file's reader read it
/// (`stored`; Grok Build's, Kimi Code's and agy's): an OAuth grant in its `login-file`, issued at
/// the renewal that made it and last used at its last served turn, with no refresh time (no
/// field their readers read states one), ending at its access token's end only where it holds a
/// credential and no refresh token; `refused` where this Mac recorded its client's refusal of
/// that very login (`refused`), else `expired` where the file holds no credential any more or the
/// row's login is signed out here, else by its end.
fn file_login(
    row: &Row,
    records: &AccountRecords,
    stored: stored::Stored,
    refused: bool,
    now: i64,
) -> Reading {
    let end = stored
        .expires_at
        .filter(|_| stored.held && !stored.refreshable);
    let (by_end, turns) = by_end(end, serves(row, records), now);
    let (issued_at, last_used) = issued_and_used(row, records);
    Reading {
        facts: Some(TokenFacts {
            kind: Kind::OAuth,
            kept: Some(Kept::LoginFile),
            issued_at,
            expires_at: end.and_then(secs),
            refreshed_at: None,
            last_used,
            state: if refused {
                State::Refused
            } else if !stored.held || accounts::signed_out_here(row, &records.state) {
                State::Expired
            } else {
                by_end
            },
        }),
        turns,
    }
}

/// Grok Build's login of `row`, from the `auth.json` in its login home, where its records name
/// no other account than the row expects; `refused` where grok's refusal of that very file is
/// marked beside it ([`super::grok_signin::refused`]).
fn grok_login(row: &Row, records: &AccountRecords, store: &Store, now: i64) -> Reading {
    let Some(file) = login_home(row, store)
        .and_then(|home| crate::ai::connector::stored_login_file(Cli::Grok.connector(), &home))
    else {
        return Reading::default();
    };
    match stored::grok(&file, row.expected_identity_email()) {
        Some(stored) => file_login(
            row,
            records,
            stored,
            super::grok_signin::refused(&file),
            now,
        ),
        None => Reading::default(),
    }
}

/// Kimi Code's login of `row`, from the credential file of its region's slot in its login home
/// (of a row with no region, the file its connector names,
/// [`crate::ai::connector::stored_login_file`]), where its token names no other account than
/// the row expects; `refused` where this Mac recorded the issuer's refusal of that very grant
/// ([`kimi_refused`]).
fn kimi_login(row: &Row, records: &AccountRecords, store: &Store, now: i64) -> Reading {
    let Some(home) = login_home(row, store) else {
        return Reading::default();
    };
    let file = match row.vendor.region {
        Some(region) => Some(kimi_credential_path(&home, region)),
        None => crate::ai::connector::stored_login_file(Cli::Kimi.connector(), &home),
    };
    match file.and_then(|file| stored::kimi(&file, row.expected_identity_email())) {
        Some(stored) => file_login(row, records, stored, kimi_refused(row, &home), now),
        None => Reading::default(),
    }
}

/// agy's login of `row`, from the grant in its login home (a whole home of its own, else the
/// person's): never `refused`, since nothing here records Google's refusal of a grant.
fn agy_login(row: &Row, records: &AccountRecords, store: &Store, now: i64) -> Reading {
    match login_home(row, store)
        .and_then(|home| crate::ai::connector::stored_login_file(Cli::Agy.connector(), &home))
        .and_then(|file| stored::agy(&file))
    {
        Some(stored) => file_login(row, records, stored, false, now),
        None => Reading::default(),
    }
}

/// Whether this Mac recorded the issuer's refusal of the very grant `row`'s Kimi Code login in
/// `home` holds (its region's slot): the record the refresh writes beside it names the account
/// the row expects, the configuration its route reads, and the credential file's bytes or the
/// empty tokens the refusal left there (`shared_provider::kimi_rejected_snapshot`). `false`
/// where the row names no region or address, and where the record does not read.
#[cfg(unix)]
fn kimi_refused(row: &Row, home: &Path) -> bool {
    row.vendor
        .region
        .zip(row.expected_identity_email())
        .is_some_and(|(region, expected)| {
            matches!(
                super::cli::shared_provider::kimi_rejected_snapshot(home, region, expected),
                Ok(Some(_))
            )
        })
}

/// Off Unix no refresh records a refusal: never.
#[cfg(not(unix))]
fn kimi_refused(_row: &Row, _home: &Path) -> bool {
    false
}

/// The files [`kimi_refused`] reads beside a Kimi Code login in `home` of `region`: its
/// configuration and the refusal record.
#[cfg(unix)]
fn kimi_refusal_files(home: &Path, region: Region) -> [PathBuf; 2] {
    [
        home.join("config.toml"),
        super::cli::shared_provider::kimi_rejection_file(home, region),
    ]
}

/// Off Unix [`kimi_refused`] reads none.
#[cfg(not(unix))]
fn kimi_refusal_files(_home: &Path, _region: Region) -> [PathBuf; 0] {
    []
}

#[cfg(all(test, unix))]
#[path = "login_token_tests.rs"]
mod tests;
